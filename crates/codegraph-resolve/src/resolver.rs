//! Reference-resolution orchestrator.
//!
//! Ports `upstream resolution/index.ts` — the `ReferenceResolver`
//! class. Coordinates the deterministic strategies (import resolution + name
//! matching) over a [`ResolutionContext`], turns resolved references into edges,
//! and persists them to the [`Store`]. The heuristic add-on layer (the
//! `FrameworkResolver` extension point and callback synthesis) is deferred to v1
//! follow-ups — see `KNOWN_DIFFS.md`. Resolution is synchronous (rusqlite).

use crate::framework::FrameworkResolver;
use crate::import_resolver::{is_php_include_path_ref, resolve_jvm_import, resolve_via_import};
use crate::name_matcher::{
    crosses_known_family, is_php_property_receiver_shape, match_dotted_call_chain,
    match_function_ref, match_method_call, match_reference, match_scoped_call_chain,
    same_language_family,
};
use crate::snapshot_context::{SnapshotResolutionContext, build_edge_adjacency};
use crate::types::{
    RefView, ResolutionContext, ResolutionResult, ResolutionStats, ResolvedBy, ResolvedRef,
};
use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind, UnresolvedRef};
use codegraph_store::Store;
use rayon::prelude::*;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, OnceLock};

/// Read-only deferred-pass intent returned by [`ReferenceResolver::resolve_one_pure`]
/// instead of the deferred-list pushes [`ReferenceResolver::resolve_one`] performs, so
/// the pure path stays callable from the `rayon` parallel map over the `Sync`
/// snapshot. Serial reassembly drains these in index order into the same lists.
pub(crate) enum DeferredIntent {
    /// `deferred_chain_refs` push (#750 conformance pass).
    ChainRef(RefView),
    /// `deferred_this_member_refs` push (#808 supertype pass).
    ThisMemberRef(RefView),
}

/// Languages whose chained static-factory/fluent calls defer to the conformance
/// second pass (`CHAIN_LANGUAGES`, `index.ts:40`).
fn is_chain_language(language: Language) -> bool {
    matches!(
        language,
        Language::Java
            | Language::Kotlin
            | Language::CSharp
            | Language::Swift
            | Language::Rust
            | Language::Go
            | Language::Scala
            | Language::Dart
            | Language::ObjC
            | Language::Pascal
    )
}

/// `::`-receiver chain languages resolve via `match_scoped_call_chain`; the
/// dotted ones via `match_dotted_call_chain` (`SCOPED_CHAIN_LANGUAGES`,
/// `index.ts:41`).
fn is_scoped_chain_language(language: Language) -> bool {
    language == Language::Rust
}

/// The extractor's chained-receiver encoding `<inner>().<method>`
/// (`CHAIN_SHAPE`, `index.ts:44`).
fn has_chain_shape(name: &str) -> bool {
    let Some(suffix_start) = name.rfind("().") else {
        return false;
    };
    let inner = &name[..suffix_start];
    let method = &name[suffix_start + 3..];
    !inner.is_empty()
        && !method.is_empty()
        && method.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// JS/TS built-in identifiers (`JS_BUILT_INS`, `index.ts:67-73`).
fn js_built_ins() -> &'static BTreeSet<&'static str> {
    static SET: OnceLock<BTreeSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        [
            "console",
            "window",
            "document",
            "global",
            "process",
            "Promise",
            "Array",
            "Object",
            "String",
            "Number",
            "Boolean",
            "Date",
            "Math",
            "JSON",
            "RegExp",
            "Error",
            "Map",
            "Set",
            "setTimeout",
            "setInterval",
            "clearTimeout",
            "clearInterval",
            "fetch",
            "require",
            "module",
            "exports",
            "__dirname",
            "__filename",
        ]
        .into_iter()
        .collect()
    })
}

/// React hooks from React itself (`REACT_HOOKS`, `index.ts:75-78`).
fn react_hooks() -> &'static BTreeSet<&'static str> {
    static SET: OnceLock<BTreeSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        [
            "useState",
            "useEffect",
            "useContext",
            "useReducer",
            "useCallback",
            "useMemo",
            "useRef",
            "useLayoutEffect",
            "useImperativeHandle",
            "useDebugValue",
        ]
        .into_iter()
        .collect()
    })
}

/// Python built-ins (`PYTHON_BUILT_INS`, `index.ts:80-84`).
fn python_built_ins() -> &'static BTreeSet<&'static str> {
    static SET: OnceLock<BTreeSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        [
            "print",
            "len",
            "range",
            "str",
            "int",
            "float",
            "list",
            "dict",
            "set",
            "tuple",
            "open",
            "input",
            "type",
            "isinstance",
            "hasattr",
            "getattr",
            "setattr",
            "super",
            "self",
            "cls",
            "None",
            "True",
            "False",
        ]
        .into_iter()
        .collect()
    })
}

/// Python built-in types (`PYTHON_BUILT_IN_TYPES`, `index.ts:86-89`).
fn python_built_in_types() -> &'static BTreeSet<&'static str> {
    static SET: OnceLock<BTreeSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        [
            "list",
            "dict",
            "set",
            "tuple",
            "str",
            "int",
            "float",
            "bool",
            "bytes",
            "bytearray",
            "frozenset",
            "object",
            "super",
        ]
        .into_iter()
        .collect()
    })
}

/// Python built-in methods (`PYTHON_BUILT_IN_METHODS`, `index.ts:91-99`).
fn python_built_in_methods() -> &'static BTreeSet<&'static str> {
    static SET: OnceLock<BTreeSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        [
            "append",
            "extend",
            "insert",
            "remove",
            "pop",
            "clear",
            "sort",
            "reverse",
            "copy",
            "update",
            "keys",
            "values",
            "items",
            "get",
            "add",
            "discard",
            "union",
            "intersection",
            "difference",
            "split",
            "join",
            "strip",
            "lstrip",
            "rstrip",
            "replace",
            "lower",
            "upper",
            "startswith",
            "endswith",
            "find",
            "index",
            "count",
            "encode",
            "decode",
            "format",
            "isdigit",
            "isalpha",
            "isalnum",
            "read",
            "write",
            "readline",
            "readlines",
            "close",
            "flush",
            "seek",
        ]
        .into_iter()
        .collect()
    })
}

/// Go standard-library packages (`GO_STDLIB_PACKAGES`, `index.ts:101-113`).
fn go_stdlib_packages() -> &'static BTreeSet<&'static str> {
    static SET: OnceLock<BTreeSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        [
            "fmt",
            "os",
            "io",
            "net",
            "http",
            "log",
            "math",
            "sort",
            "sync",
            "time",
            "path",
            "bytes",
            "strings",
            "strconv",
            "errors",
            "context",
            "json",
            "xml",
            "csv",
            "html",
            "template",
            "regexp",
            "reflect",
            "runtime",
            "testing",
            "flag",
            "bufio",
            "crypto",
            "encoding",
            "filepath",
            "hash",
            "mime",
            "rand",
            "signal",
            "sql",
            "syscall",
            "unicode",
            "unsafe",
            "atomic",
            "binary",
            "debug",
            "exec",
            "heap",
            "ring",
            "scanner",
            "tar",
            "zip",
            "gzip",
            "zlib",
            "tls",
            "url",
            "user",
            "pprof",
            "trace",
            "ast",
            "build",
            "parser",
            "printer",
            "token",
            "types",
            "cgo",
            "plugin",
            "race",
            "ioutil",
            "utilruntime",
            "utilwait",
            "utilnet",
        ]
        .into_iter()
        .collect()
    })
}

/// Go built-ins (`GO_BUILT_INS`, `index.ts:115-123`).
fn go_built_ins() -> &'static BTreeSet<&'static str> {
    static SET: OnceLock<BTreeSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        [
            "make",
            "new",
            "len",
            "cap",
            "append",
            "copy",
            "delete",
            "close",
            "panic",
            "recover",
            "print",
            "println",
            "complex",
            "real",
            "imag",
            "error",
            "nil",
            "true",
            "false",
            "iota",
            "int",
            "int8",
            "int16",
            "int32",
            "int64",
            "uint",
            "uint8",
            "uint16",
            "uint32",
            "uint64",
            "uintptr",
            "float32",
            "float64",
            "complex64",
            "complex128",
            "string",
            "bool",
            "byte",
            "rune",
            "any",
        ]
        .into_iter()
        .collect()
    })
}

/// Pascal/Delphi standard-unit prefixes (`PASCAL_UNIT_PREFIXES`, `index.ts:125-129`).
const PASCAL_UNIT_PREFIXES: [&str; 14] = [
    "System.",
    "Winapi.",
    "Vcl.",
    "Fmx.",
    "Data.",
    "Datasnap.",
    "Soap.",
    "Xml.",
    "Web.",
    "REST.",
    "FireDAC.",
    "IBX.",
    "IdHTTP",
    "IdTCP",
];

/// Pascal/Delphi built-ins (`PASCAL_BUILT_INS`, `index.ts:131-149`).
fn pascal_built_ins() -> &'static BTreeSet<&'static str> {
    static SET: OnceLock<BTreeSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        [
            "System",
            "SysUtils",
            "Classes",
            "Types",
            "Variants",
            "StrUtils",
            "Math",
            "DateUtils",
            "IOUtils",
            "Generics.Collections",
            "Generics.Defaults",
            "Rtti",
            "TypInfo",
            "SyncObjs",
            "RegularExpressions",
            "SysInit",
            "Windows",
            "Messages",
            "Graphics",
            "Controls",
            "Forms",
            "Dialogs",
            "StdCtrls",
            "ExtCtrls",
            "ComCtrls",
            "Menus",
            "ActnList",
            "WriteLn",
            "Write",
            "ReadLn",
            "Read",
            "Inc",
            "Dec",
            "Ord",
            "Chr",
            "Length",
            "SetLength",
            "High",
            "Low",
            "Assigned",
            "FreeAndNil",
            "Format",
            "IntToStr",
            "StrToInt",
            "FloatToStr",
            "StrToFloat",
            "Trim",
            "UpperCase",
            "LowerCase",
            "Pos",
            "Copy",
            "Delete",
            "Insert",
            "Now",
            "Date",
            "Time",
            "DateToStr",
            "StrToDate",
            "Raise",
            "Exit",
            "Break",
            "Continue",
            "Abort",
            "True",
            "False",
            "nil",
            "Self",
            "Result",
            "Create",
            "Destroy",
            "Free",
            "TObject",
            "TComponent",
            "TPersistent",
            "TInterfacedObject",
            "TList",
            "TStringList",
            "TStrings",
            "TStream",
            "TMemoryStream",
            "TFileStream",
            "Exception",
            "EAbort",
            "EConvertError",
            "EAccessViolation",
            "IInterface",
            "IUnknown",
        ]
        .into_iter()
        .collect()
    })
}

/// C standard-library symbols (`C_BUILT_INS`, `index.ts:151-181`).
fn c_built_ins() -> &'static BTreeSet<&'static str> {
    static SET: OnceLock<BTreeSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        [
            "printf",
            "fprintf",
            "sprintf",
            "snprintf",
            "scanf",
            "fscanf",
            "sscanf",
            "malloc",
            "calloc",
            "realloc",
            "free",
            "memcpy",
            "memmove",
            "memset",
            "memcmp",
            "memchr",
            "strlen",
            "strcpy",
            "strncpy",
            "strcat",
            "strncat",
            "strcmp",
            "strncmp",
            "strstr",
            "strchr",
            "strrchr",
            "strtok",
            "strdup",
            "fopen",
            "fclose",
            "fread",
            "fwrite",
            "fgets",
            "fputs",
            "fputc",
            "fgetc",
            "feof",
            "ferror",
            "fflush",
            "fseek",
            "ftell",
            "rewind",
            "exit",
            "abort",
            "atexit",
            "atoi",
            "atol",
            "atof",
            "strtol",
            "strtoul",
            "strtod",
            "qsort",
            "bsearch",
            "abs",
            "labs",
            "rand",
            "srand",
            "sin",
            "cos",
            "tan",
            "sqrt",
            "pow",
            "log",
            "log10",
            "exp",
            "ceil",
            "floor",
            "fabs",
            "time",
            "clock",
            "difftime",
            "mktime",
            "localtime",
            "gmtime",
            "strftime",
            "asctime",
            "assert",
            "errno",
            "perror",
            "remove",
            "rename",
            "tmpfile",
            "tmpnam",
            "getenv",
            "system",
            "signal",
            "raise",
            "setjmp",
            "longjmp",
            "va_start",
            "va_end",
            "va_arg",
            "va_copy",
            "NULL",
            "EOF",
            "BUFSIZ",
            "FILENAME_MAX",
            "RAND_MAX",
            "EXIT_SUCCESS",
            "EXIT_FAILURE",
            "size_t",
            "ptrdiff_t",
            "wchar_t",
            "intptr_t",
            "uintptr_t",
            "int8_t",
            "int16_t",
            "int32_t",
            "int64_t",
            "uint8_t",
            "uint16_t",
            "uint32_t",
            "uint64_t",
            "FILE",
            "stat",
            "lstat",
            "fstat",
            "open",
            "close",
            "read",
            "write",
            "pipe",
            "fork",
            "exec",
            "waitpid",
            "getpid",
            "getppid",
            "kill",
            "sleep",
            "usleep",
            "pthread_create",
            "pthread_join",
            "pthread_mutex_lock",
            "pthread_mutex_unlock",
            "dlopen",
            "dlsym",
            "dlclose",
        ]
        .into_iter()
        .collect()
    })
}

/// C++ built-ins (`CPP_BUILT_INS`, `index.ts:183-192`).
fn cpp_built_ins() -> &'static BTreeSet<&'static str> {
    static SET: OnceLock<BTreeSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        [
            "cout",
            "cin",
            "cerr",
            "clog",
            "endl",
            "flush",
            "ws",
            "std",
            "nullptr",
            "true",
            "false",
            "this",
            "sizeof",
            "alignof",
            "typeid",
            "static_cast",
            "dynamic_cast",
            "reinterpret_cast",
            "const_cast",
            "make_unique",
            "make_shared",
            "make_pair",
            "move",
            "forward",
            "swap",
        ]
        .into_iter()
        .collect()
    })
}

/// Orchestrates reference resolution using multiple strategies
/// (`ReferenceResolver`, `index.ts:199-1189`).
///
/// The `FrameworkResolver` extension-point list is empty until
/// [`Self::initialize`] runs detection; on a non-framework project it stays
/// empty and the strategy loop is a no-op (matching the upstream with zero detected
/// `FrameworkResolver`s).
pub struct ReferenceResolver {
    project_root: String,
    /// Detected `FrameworkResolver` implementations (`frameworks` field,
    /// `index.ts:264`). Empty until [`Self::initialize`] detects react/vue/nestjs.
    framework_resolver_extensions: Vec<Box<dyn FrameworkResolver>>,
    /// Distinct symbol names known to the graph, for the fast pre-filter
    /// (`knownNames`, `index.ts:224`). Populated by `warm_caches`.
    known_names: Option<BTreeSet<String>>,
    /// `this.<member>` fn-refs whose member wasn't on the enclosing class
    /// itself — retried in the supertype pass once implements/extends edges
    /// exist (`deferredThisMemberRefs`, index.ts:214 / #808). A `Mutex` (not
    /// `RefCell`) so the resolver is `Sync` for the parallel chunk resolve; it
    /// is only ever pushed from serial code, never inside the `rayon` map.
    deferred_this_member_refs: std::sync::Mutex<Vec<RefView>>,
    /// Chained static-factory/fluent `calls` refs the first pass couldn't
    /// resolve — drained by [`Self::resolve_chained_calls_via_conformance`]
    /// once implements/extends edges exist (`deferredChainRefs`,
    /// index.ts:209 / #750). A `Mutex` for the same `Sync` reason as
    /// `deferred_this_member_refs`; pushed only from serial code.
    deferred_chain_refs: std::sync::Mutex<Vec<RefView>>,
}

impl ReferenceResolver {
    /// Build a resolver for `project_root`. The `FrameworkResolver` list starts
    /// empty; call [`Self::initialize`] to detect react/vue/nestjs before
    /// resolving (`index.ts:258-261`).
    pub fn new(project_root: impl Into<String>) -> Self {
        Self {
            project_root: project_root.into(),
            framework_resolver_extensions: Vec::new(),
            known_names: None,
            deferred_this_member_refs: std::sync::Mutex::new(Vec::new()),
            deferred_chain_refs: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Detect the project's frameworks and populate the resolver's
    /// `FrameworkResolver` list (`initialize`, `index.ts:263-266`). Must run
    /// before `resolve_all` so Strategy-1 dispatch sees the detected resolvers.
    pub fn initialize(&mut self, context: &dyn ResolutionContext) {
        self.framework_resolver_extensions = crate::frameworks::detect_frameworks(context);
    }

    /// Has at least one `FrameworkResolver` been detected?
    pub fn has_framework_resolvers(&self) -> bool {
        !self.framework_resolver_extensions.is_empty()
    }

    /// Run each detected resolver's `post_extract` finalization and persist the
    /// returned node updates (`runPostExtract`, `index.ts:276-295`). Idempotent —
    /// safe after every index and every incremental sync. Returns the number of
    /// nodes updated.
    pub fn run_post_extract(&self, store: &mut Store) -> anyhow::Result<usize> {
        let mut all_updates: Vec<Node> = Vec::new();
        {
            let context = crate::context::StoreResolutionContext::new(store, &self.project_root);
            for resolver in &self.framework_resolver_extensions {
                if let Some(nodes) = resolver.post_extract(&context) {
                    all_updates.extend(nodes);
                }
            }
        }
        let updated = all_updates.len();
        if !all_updates.is_empty() {
            store.upsert_nodes(&all_updates)?;
        }
        Ok(updated)
    }

    /// Run each detected resolver's per-file `extract` over `relative_files`,
    /// persisting the framework nodes then references (`tree-sitter.ts:4796-4819`,
    /// gated by `getApplicableFrameworks`). Nodes are upserted before refs so
    /// `insert_unresolved_refs` (which drops refs with no source node) keeps them.
    pub fn extract_and_persist_frameworks(
        &self,
        store: &mut Store,
        relative_files: &[String],
    ) -> anyhow::Result<()> {
        if self.framework_resolver_extensions.is_empty() {
            return Ok(());
        }
        let mut nodes: Vec<Node> = Vec::new();
        let mut refs: Vec<UnresolvedRef> = Vec::new();
        for relative in relative_files {
            let language = codegraph_extract::detect_language(relative);
            let Some(content) =
                std::fs::read_to_string(std::path::Path::new(&self.project_root).join(relative))
                    .ok()
            else {
                continue;
            };
            for resolver in &self.framework_resolver_extensions {
                if !applies_to_language(resolver.as_ref(), language) {
                    continue;
                }
                if let Some(result) = resolver.extract(relative, &content, &self.project_root) {
                    nodes.extend(result.nodes);
                    for reference in result.references {
                        refs.push(ref_view_to_unresolved(&reference));
                    }
                }
            }
        }
        if !nodes.is_empty() {
            store.upsert_nodes(&nodes)?;
        }
        if !refs.is_empty() {
            store.insert_unresolved_refs(&refs)?;
        }
        Ok(())
    }

    /// Project root the resolver was built for.
    pub fn project_root(&self) -> &str {
        &self.project_root
    }

    /// Pre-build the known-symbol-name set for the fast pre-filter
    /// (`warmCaches`, `index.ts:298-308`). Reads only the distinct name column
    /// via `known_node_names`, avoiding full-node materialization.
    pub fn warm_caches(&mut self, context: &dyn ResolutionContext) {
        let names: BTreeSet<String> = context.known_node_names().into_iter().collect();
        self.known_names = Some(names);
    }

    /// Resolve all unresolved references (`resolveAll`, `index.ts:511-572`).
    ///
    /// `unresolved_refs` are the persisted rows; each is denormalized into a
    /// [`RefView`] (filePath/language always present — they are NOT NULL in the
    /// store schema, so no node lookup is needed, unlike the upstream `index.ts:529-530`).
    pub fn resolve_all(
        &mut self,
        unresolved_refs: &[UnresolvedRef],
        context: &dyn ResolutionContext,
    ) -> ResolutionResult {
        if self.known_names.is_none() {
            self.warm_caches(context);
        }

        let mut resolved: Vec<ResolvedRef> = Vec::new();
        let mut unresolved: Vec<RefView> = Vec::new();
        let mut by_method: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();

        let refs: Vec<RefView> = unresolved_refs.iter().map(to_ref_view).collect();
        let total = refs.len();

        for reference in &refs {
            match self.resolve_one(reference, context) {
                Some(result) => {
                    *by_method
                        .entry(result.resolved_by.as_str().to_string())
                        .or_insert(0) += 1;
                    resolved.push(result);
                }
                None => unresolved.push(reference.clone()),
            }
        }

        ResolutionResult {
            stats: ResolutionStats {
                total,
                resolved: resolved.len(),
                unresolved: unresolved.len(),
                by_method,
            },
            resolved,
            unresolved,
        }
    }

    /// Resolve a single reference (`resolveOne`, `index.ts:652-746`).
    ///
    /// Serial wrapper over [`Self::resolve_one_pure`]: pushes any returned
    /// deferred intent to the matching deferred list, reproducing the original
    /// in-place push behavior for the non-parallel callers.
    pub fn resolve_one(
        &self,
        reference: &RefView,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        let (resolved, deferred) = self.resolve_one_pure(reference, context);
        match deferred {
            Some(DeferredIntent::ChainRef(reference)) => {
                self.deferred_chain_refs.lock().unwrap().push(reference);
            }
            Some(DeferredIntent::ThisMemberRef(reference)) => {
                self.deferred_this_member_refs
                    .lock()
                    .unwrap()
                    .push(reference);
            }
            None => {}
        }
        resolved
    }

    /// Read-only twin of [`Self::resolve_one`] for the parallel resolve path.
    ///
    /// Identical resolution logic, but instead of pushing to the resolver's
    /// deferred lists it RETURNS a [`DeferredIntent`], so it can run
    /// inside a `rayon` parallel map over a `Sync` [`ResolutionContext`]
    /// (`SnapshotResolutionContext`). `&self` is shared read-only here:
    /// `known_names` is set once in `warm_caches` and only read, and the
    /// `framework_resolver_extensions` are read-only.
    pub(crate) fn resolve_one_pure(
        &self,
        reference: &RefView,
        context: &dyn ResolutionContext,
    ) -> (Option<ResolvedRef>, Option<DeferredIntent>) {
        if self.is_built_in_or_external(reference) {
            return (None, None);
        }

        // Fast pre-filter (index.ts:664-670). The `FrameworkResolver`
        // claims-reference escape is a no-op while the extension-point list is
        // empty.
        if !self.has_any_possible_match(&reference.reference_name)
            && !self.matches_any_import(reference, context)
            && !self
                .framework_resolver_extensions
                .iter()
                .any(|f| f.claims_reference(&reference.reference_name))
        {
            return (None, None);
        }

        // Function-as-value refs (#756) get a dedicated, strictly-gated path,
        // never reaching framework/fuzzy strategies (index.ts:686-699).
        if reference.is_function_ref {
            if reference.reference_name.starts_with("this.") {
                let (resolved, deferred) = self.resolve_this_member_fn_ref_pure(reference, context);
                return (self.gate_language(resolved, reference, context), deferred);
            }
            if let Some(via_import) =
                self.gate_language(resolve_via_import(reference, context), reference, context)
            {
                if let Some(target) = context.get_node_by_id(&via_import.target_node_id) {
                    if matches!(target.kind, NodeKind::Function | NodeKind::Method) {
                        return (Some(via_import), None);
                    }
                }
            }
            return (
                self.gate_language(match_function_ref(reference, context), reference, context),
                None,
            );
        }

        // JVM FQN imports skip everything else (index.ts:675-676).
        if let Some(jvm_import) = resolve_jvm_import(reference, context) {
            return (Some(jvm_import), None);
        }

        let mut candidates: Vec<ResolvedRef> = Vec::new();

        // Strategy 1: `FrameworkResolver` extension point (index.ts:695-701).
        // Empty in v1 → no candidates contributed.
        for resolver in &self.framework_resolver_extensions {
            if let Some(result) = self.gate_framework_resolver_extension_language(
                resolver.resolve(reference, context),
                reference,
                context,
            ) {
                if result.confidence >= 0.9 {
                    return (Some(result), None);
                }
                candidates.push(result);
            }
        }

        // Strategy 2: import-based resolution (index.ts:704-708).
        if let Some(import_result) =
            self.gate_language(resolve_via_import(reference, context), reference, context)
        {
            if import_result.confidence >= 0.9 {
                return (Some(import_result), None);
            }
            candidates.push(import_result);
        }

        // PHP include path: never fall through to name-matcher (index.ts:714-720).
        if is_php_include_path_ref(reference) {
            return (candidates.into_iter().reduce(highest_confidence), None);
        }

        // Strategy 3: name matching (index.ts:723-726).
        if let Some(name_result) =
            self.gate_language(match_reference(reference, context), reference, context)
        {
            candidates.push(name_result);
        }

        if candidates.is_empty() {
            // Defer a chained static-factory/fluent `calls` ref the first pass
            // couldn't resolve — its method may live on a supertype the receiver
            // conforms to, resolvable once implements/extends edges exist
            // (the conformance pass, index.ts:758-768 / #750).
            if reference.reference_kind == EdgeKind::Calls
                && is_chain_language(reference.language)
                && has_chain_shape(&reference.reference_name)
            {
                return (None, Some(DeferredIntent::ChainRef(reference.clone())));
            }
            // PHP `$this->prop->method()` (encoded `this->prop.method`): its
            // method may live on the property's declared supertype, resolvable
            // only once implements/extends edges exist — defer to the same
            // conformance pass (index.ts:925-933 / #1220).
            if reference.reference_kind == EdgeKind::Calls
                && reference.language == Language::Php
                && is_php_property_receiver_shape(&reference.reference_name)
            {
                return (None, Some(DeferredIntent::ChainRef(reference.clone())));
            }
            return (None, None);
        }

        (candidates.into_iter().reduce(highest_confidence), None)
    }

    /// Resolve a `this.<member>` function_ref against the enclosing class's own
    /// members (same file, function/method kind); a member not on the class
    /// RETURNS [`DeferredIntent::ThisMemberRef`] for the #808 supertype pass
    /// instead of pushing it. Ports `resolveThisMemberFnRef` (index.ts:1210-1248).
    fn resolve_this_member_fn_ref_pure(
        &self,
        reference: &RefView,
        context: &dyn ResolutionContext,
    ) -> (Option<ResolvedRef>, Option<DeferredIntent>) {
        let Some(member) = reference.reference_name.strip_prefix("this.") else {
            return (None, None);
        };
        if member.is_empty() {
            return (None, None);
        }
        let Some(from_node) = context.get_node_by_id(&reference.from_node_id) else {
            return (None, None);
        };
        let class_prefix = if matches!(
            from_node.kind,
            NodeKind::Class
                | NodeKind::Struct
                | NodeKind::Interface
                | NodeKind::Trait
                | NodeKind::Protocol
                | NodeKind::Enum
                | NodeKind::Module
        ) {
            from_node.qualified_name.clone()
        } else {
            let Some(sep) = from_node.qualified_name.rfind("::") else {
                return (None, None);
            };
            if sep == 0 {
                return (None, None);
            }
            from_node.qualified_name[..sep].to_string()
        };
        let qualified = format!("{class_prefix}::{member}");
        let target = context
            .get_nodes_by_qualified_name(&qualified)
            .into_iter()
            .filter(|n| {
                matches!(n.kind, NodeKind::Function | NodeKind::Method)
                    && n.file_path == reference.file_path
                    && n.id != reference.from_node_id
            })
            .reduce(|a, b| if a.start_line <= b.start_line { a } else { b });
        match target {
            Some(target) => (
                Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: target.id,
                    confidence: 0.95,
                    resolved_by: ResolvedBy::FunctionRef,
                }),
                None,
            ),
            // Not on the class itself — possibly INHERITED. Retry in the
            // supertype pass once implements/extends edges exist
            // (index.ts:1234-1239).
            None => (None, Some(DeferredIntent::ThisMemberRef(reference.clone()))),
        }
    }

    /// Second pass for `this.<member>` fn-refs whose member wasn't on the
    /// enclosing class itself (#808): once implements/extends edges exist,
    /// NODE-anchored BFS up the supertype graph resolves the member on the
    /// nearest supertype declaring it. Ports `resolveDeferredThisMemberRefs`
    /// (index.ts:1260-1356). Returns the number of newly-created edges.
    pub fn resolve_deferred_this_member_refs(&self, store: &mut Store) -> anyhow::Result<usize> {
        let deferred = std::mem::take(&mut *self.deferred_this_member_refs.lock().unwrap());
        if deferred.is_empty() {
            return Ok(0);
        }

        let mut resolved: Vec<ResolvedRef> = Vec::new();
        for reference in &deferred {
            let Some(member) = reference.reference_name.strip_prefix("this.") else {
                continue;
            };
            if member.is_empty() {
                continue;
            }
            let Some(from_node) = store.node_by_id(&reference.from_node_id).ok().flatten() else {
                continue;
            };
            // Class-body-level hooks (Ruby) attribute to the CLASS node itself;
            // members strip the member segment (index.ts:1271-1282).
            let class_name = if is_supertype_bearing_or_module(from_node.kind) {
                from_node.name.clone()
            } else {
                let Some(sep) = from_node.qualified_name.rfind("::") else {
                    continue;
                };
                if sep == 0 {
                    continue;
                }
                let class_prefix = &from_node.qualified_name[..sep];
                match class_prefix.rfind("::") {
                    Some(s) => class_prefix[s + 2..].to_string(),
                    None => class_prefix.to_string(),
                }
            };

            if let Some(target) = find_inherited_member(store, &class_name, member, reference) {
                resolved.push(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: target,
                    confidence: 0.85,
                    resolved_by: ResolvedBy::FunctionRef,
                });
            }
        }

        if resolved.is_empty() {
            return Ok(0);
        }
        let edges = self.create_edges(&resolved, store);
        let count = edges.len();
        if !edges.is_empty() {
            store.insert_edges(&edges)?;
        }
        Ok(count)
    }

    /// Second resolution pass for chained static-factory/fluent calls whose
    /// chained method is defined on a SUPERTYPE the receiver's type conforms to
    /// (#750). The first pass can't resolve these because implements/extends
    /// edges aren't built yet; this runs AFTER they are persisted, so
    /// `context.get_supertypes` (and the conformance fallback in
    /// `resolve_method_on_type`) can walk them. Ports
    /// `resolveChainedCallsViaConformance` (index.ts:877-904); drains the
    /// in-memory deferred list ONCE (the resolved rows were already deleted in
    /// pass 1, so re-resolution can't re-insert them). Returns the number of
    /// newly-created edges.
    pub fn resolve_chained_calls_via_conformance(
        &self,
        store: &mut Store,
    ) -> anyhow::Result<usize> {
        let deferred = std::mem::take(&mut *self.deferred_chain_refs.lock().unwrap());
        if deferred.is_empty() {
            return Ok(0);
        }

        let resolved: Vec<ResolvedRef> = {
            let context = crate::context::StoreResolutionContext::new(store, &self.project_root);
            deferred
                .iter()
                .filter_map(|reference| {
                    // PHP `this->prop.method` resolves via match_method_call
                    // (declared-type inference + resolve_method_on_type supertype
                    // walk); `::`-receiver languages (Rust) split on `::`; other
                    // dotted-receiver languages on `.` (index.ts:1129-1138 / #1220).
                    let chain_match = if reference.language == Language::Php
                        && is_php_property_receiver_shape(&reference.reference_name)
                    {
                        match_method_call(reference, &context)
                    } else if is_scoped_chain_language(reference.language) {
                        match_scoped_call_chain(reference, &context)
                    } else {
                        match_dotted_call_chain(reference, &context)
                    };
                    self.gate_language(chain_match, reference, &context)
                })
                .collect()
        };

        if resolved.is_empty() {
            return Ok(0);
        }
        let edges = self.create_edges(&resolved, store);
        let count = edges.len();
        if !edges.is_empty() {
            store.insert_edges(&edges)?;
        }
        Ok(count)
    }

    /// Create edges from resolved references (`createEdges`, `index.ts:751-790`).
    pub fn create_edges(&self, resolved: &[ResolvedRef], store: &Store) -> Vec<Edge> {
        resolved
            .iter()
            .map(|reference| {
                let mut kind = reference.original.reference_kind;

                // Promote extends → implements (index.ts:756-764).
                if kind == EdgeKind::Extends {
                    if let Ok(Some(target_node)) = store.node_by_id(&reference.target_node_id) {
                        if matches!(target_node.kind, NodeKind::Interface | NodeKind::Protocol) {
                            if let Ok(Some(source_node)) =
                                store.node_by_id(&reference.original.from_node_id)
                            {
                                if !matches!(
                                    source_node.kind,
                                    NodeKind::Interface | NodeKind::Protocol
                                ) {
                                    kind = EdgeKind::Implements;
                                }
                            }
                        }
                    }
                }

                // Promote calls → instantiates (index.ts:771-776).
                if kind == EdgeKind::Calls {
                    if let Ok(Some(target_node)) = store.node_by_id(&reference.target_node_id) {
                        if matches!(target_node.kind, NodeKind::Class | NodeKind::Struct) {
                            kind = EdgeKind::Instantiates;
                        }
                    }
                }

                Edge {
                    id: None,
                    source: reference.original.from_node_id.clone(),
                    target: reference.target_node_id.clone(),
                    kind,
                    metadata: Some(build_edge_metadata(reference)),
                    line: Some(reference.original.line),
                    col: Some(reference.original.column),
                    provenance: None,
                }
            })
            .collect()
    }

    /// Resolve all refs in the store and persist the resulting edges
    /// (`resolveAndPersist`, `index.ts:795-821`).
    ///
    /// Reads `unresolved_refs`, resolves them, inserts edges, then deletes the
    /// resolved rows so metrics stay accurate.
    pub fn resolve_and_persist(&mut self, store: &mut Store) -> anyhow::Result<ResolutionResult> {
        let unresolved_refs = store.all_unresolved_refs()?;
        let result = {
            let context = crate::context::StoreResolutionContext::new(store, &self.project_root);
            self.resolve_all(&unresolved_refs, &context)
        };

        let edges = self.create_edges(&result.resolved, store);
        if !edges.is_empty() {
            store.insert_edges(&edges)?;
        }

        if !result.resolved.is_empty() {
            delete_resolved_rows(store, &result.resolved)?;
        }

        // #750 conformance pass for chained calls whose method lives on a
        // supertype, then #808 supertype pass for inherited this.<member> refs —
        // both after the main pass built implements/extends edges (index.ts:384-387).
        self.resolve_chained_calls_via_conformance(store)?;
        self.resolve_deferred_this_member_refs(store)?;

        Ok(result)
    }

    /// Memory-bounded form of [`Self::resolve_and_persist`] (`resolveAndPersistBatched`,
    /// `index.ts:870-953`): reads refs through a rowid cursor in `batch_size` chunks
    /// instead of materializing the whole graph. Byte-equivalence rests on four
    /// invariants: `warm_caches` runs ONCE over the full node set; the cursor reads
    /// refs in ascending rowid order so edges insert in the same global order with
    /// identical autoinc ids; resolved-row deletion is DEFERRED to after the loop so
    /// the tuple-keyed delete never removes a not-yet-read duplicate row from a later
    /// batch (which would lose its edge — `unresolved_refs` rows have no UNIQUE
    /// constraint, so the same `(from,name,kind)` tuple recurs across batches); and
    /// only resolved rows are deleted, leaving the same final `unresolved_refs` table.
    ///
    /// Each chunk's refs are resolved IN PARALLEL via an order-preserving
    /// `par_iter().map().collect()` over a `Sync` [`SnapshotResolutionContext`],
    /// then the results are reassembled SERIALLY in index order — identical to the
    /// serial path. The WHOLE-RUN node snapshot is built LAZILY on first-chunk
    /// entry (AFTER framework extraction has injected its nodes/refs at the call
    /// site); the per-chunk `implements`/`extends` edge map is rebuilt from the
    /// live store before each chunk so `get_supertypes` sees per-chunk edge growth.
    pub fn resolve_and_persist_batched(
        &mut self,
        store: &mut Store,
        batch_size: usize,
    ) -> anyhow::Result<ResolutionResult> {
        self.resolve_and_persist_batched_with_progress(store, batch_size, |_, _| {})
    }

    /// Like [`Self::resolve_and_persist_batched`] but reports progress via
    /// `on_progress(processed, total)` after each chunk, letting a caller drive a
    /// `pos/len` bar without `codegraph-resolve` depending on the bar library.
    /// `total` is the post-framework `unresolved_refs` count; `processed`
    /// accumulates `batch.len()` per chunk (refs PROCESSED — resolved rows are
    /// deleted). `on_progress` never gates or reorders work, so resolution output
    /// stays byte-equivalent to the no-callback path.
    pub fn resolve_and_persist_batched_with_progress(
        &mut self,
        store: &mut Store,
        batch_size: usize,
        mut on_progress: impl FnMut(u64, u64),
    ) -> anyhow::Result<ResolutionResult> {
        // Incomplete-resolution marker (#1187): armed BEFORE the first batch and
        // cleared only after the deferred passes complete below. An interrupted
        // run (crash / Ctrl-C / #1122 watchdog kill) leaves it set, so the next
        // `sync` knows to sweep the refs this run never reached into edges.
        store.set_resolution_incomplete()?;

        {
            let context = crate::context::StoreResolutionContext::new(store, &self.project_root);
            self.warm_caches(&context);
        }

        let total_refs = store.unresolved_refs_count()? as u64;
        let mut processed: u64 = 0;

        // Built lazily on first-chunk entry, AFTER framework extraction injected
        // its nodes — never in `new`/`initialize` (would miss framework nodes).
        let mut node_snapshot: Option<SnapshotResolutionContext> = None;
        // Implements/Extends adjacency for `get_supertypes`, seeded once from the
        // store and folded forward per chunk. `create_edges` is the sole producer
        // of inheritance edges between chunks, so a chunk seeing edges from chunks
        // 0..N-1 here is byte-identical to a fresh per-chunk store rebuild — but
        // without that rebuild's per-chunk query storm. Holds only Implements/
        // Extends, so it stays small (inheritance edges, not Calls/References).
        let mut base_adjacency: HashMap<String, Vec<(String, EdgeKind)>> = HashMap::new();

        let mut aggregate = ResolutionResult {
            stats: ResolutionStats {
                total: 0,
                resolved: 0,
                unresolved: 0,
                by_method: std::collections::BTreeMap::new(),
            },
            resolved: Vec::new(),
            unresolved: Vec::new(),
        };

        let mut cursor: i64 = 0;
        loop {
            let batch = store.unresolved_refs_batch(cursor, batch_size)?;
            if batch.is_empty() {
                break;
            }
            cursor = batch
                .iter()
                .filter_map(|reference| reference.id)
                .max()
                .unwrap_or(cursor);

            let base = match &node_snapshot {
                Some(snapshot) => snapshot,
                None => {
                    base_adjacency = (*build_edge_adjacency(store)?).clone();
                    node_snapshot.insert(SnapshotResolutionContext::from_store(
                        store,
                        &self.project_root,
                    )?)
                }
            };
            // Install the adjacency from chunks 0..N-1 BEFORE resolving chunk N
            // (matches the old per-chunk full rebuild's observable state).
            let chunk_ctx = base.with_edge_adjacency(Arc::new(base_adjacency.clone()));

            let result = self.resolve_chunk_parallel(&batch, &chunk_ctx);

            let edges = self.create_edges(&result.resolved, store);
            if !edges.is_empty() {
                store.insert_edges(&edges)?;
            }
            // Fold this chunk's NEW Implements/Extends edges into the adjacency
            // AFTER inserting them, so chunk N+1 sees chunks 0..N — the same
            // forward visibility the per-chunk rebuild produced.
            for edge in &edges {
                if matches!(edge.kind, EdgeKind::Implements | EdgeKind::Extends) {
                    base_adjacency
                        .entry(edge.source.clone())
                        .or_default()
                        .push((edge.target.clone(), edge.kind));
                }
            }

            // Delete THIS batch's resolved rows immediately, bounded by the
            // batch's max id, instead of accumulating every resolved key for a
            // single end-of-loop delete (peak-memory bound on large graphs). The
            // `id <= cursor` guard preserves any duplicate tuple in a later batch
            // (ascending-id read order ⇒ its id > cursor) — same final table.
            let batch_keys: Vec<(String, String, EdgeKind)> = result
                .resolved
                .iter()
                .map(|reference| {
                    (
                        reference.original.from_node_id.clone(),
                        reference.original.reference_name.clone(),
                        reference.original.reference_kind,
                    )
                })
                .collect();
            if !batch_keys.is_empty() {
                store.delete_resolved_unresolved_refs_up_to(&batch_keys, cursor)?;
            }

            aggregate.stats.total += result.stats.total;
            aggregate.stats.resolved += result.stats.resolved;
            aggregate.stats.unresolved += result.stats.unresolved;
            for (method, count) in result.stats.by_method {
                *aggregate.stats.by_method.entry(method).or_insert(0) += count;
            }

            processed += batch.len() as u64;
            on_progress(processed, total_refs);
        }

        // #750 conformance pass then #808 supertype pass, after implements/extends
        // edges exist (index.ts:508-511).
        self.resolve_chained_calls_via_conformance(store)?;
        self.resolve_deferred_this_member_refs(store)?;

        // Full pass completed: clear the #1187 marker set at the top.
        store.clear_resolution_incomplete()?;

        Ok(aggregate)
    }

    /// Resolve one chunk's refs in parallel over the `Sync` snapshot context,
    /// then reassemble SERIALLY in index order — byte-identical to the serial
    /// [`Self::resolve_all`].
    ///
    /// The parallel map is order-preserving (`par_iter().map().collect()`); the
    /// reassembly iterates the collected `Vec` in index order, so resolved-ref
    /// order, `by_method` stats, and the deferred push order all match
    /// the serial batch-slice traversal exactly. `resolve_one_pure` reads `&self`
    /// and the snapshot read-only — no shared mutation inside the map.
    fn resolve_chunk_parallel(
        &self,
        batch: &[UnresolvedRef],
        context: &SnapshotResolutionContext,
    ) -> ResolutionResult {
        let refs: Vec<RefView> = batch.iter().map(to_ref_view).collect();
        let total = refs.len();

        let results: Vec<(Option<ResolvedRef>, Option<DeferredIntent>)> = refs
            .par_iter()
            .map(|reference| self.resolve_one_pure(reference, context))
            .collect();

        let mut resolved: Vec<ResolvedRef> = Vec::new();
        let mut unresolved: Vec<RefView> = Vec::new();
        let mut by_method: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();

        for (reference, (maybe_resolved, deferred)) in refs.iter().zip(results) {
            match deferred {
                Some(DeferredIntent::ChainRef(reference)) => {
                    self.deferred_chain_refs.lock().unwrap().push(reference);
                }
                Some(DeferredIntent::ThisMemberRef(reference)) => {
                    self.deferred_this_member_refs
                        .lock()
                        .unwrap()
                        .push(reference);
                }
                None => {}
            }
            match maybe_resolved {
                Some(result) => {
                    *by_method
                        .entry(result.resolved_by.as_str().to_string())
                        .or_insert(0) += 1;
                    resolved.push(result);
                }
                None => unresolved.push(reference.clone()),
            }
        }

        ResolutionResult {
            stats: ResolutionStats {
                total,
                resolved: resolved.len(),
                unresolved: unresolved.len(),
                by_method,
            },
            resolved,
            unresolved,
        }
    }

    /// Incremental form of [`Self::resolve_and_persist`] for `sync`.
    ///
    /// Resolves only the unresolved references that could change state after a
    /// scoped re-extraction, instead of the whole `unresolved_refs` table:
    ///   * `scope_files` — files whose outgoing resolved (non-`contains`) edges
    ///     were all dropped before this call: the changed/removed files (their
    ///     nodes were deleted, cascading every edge) plus every affected file
    ///     refreshed by the watch layer (one-hop dependents and files whose refs
    ///     resolve to a changed name). Because those files hold NO resolved edges
    ///     now, re-resolving their refs and persisting all resulting edges rebuilds
    ///     exactly that source set with no duplication.
    ///   * `changed_names` — names of symbols added or removed by the change.
    ///     Covers the danger case where a ref in a file NOT in `scope_files` was
    ///     previously unresolved but should now resolve to a newly-added symbol.
    ///     Such a ref is still in `unresolved_refs` with no surviving edge, so
    ///     persisting its edge cannot duplicate one.
    ///
    /// `known_names`/name caches are warmed against the CURRENT full node set, so
    /// resolution sees the post-change symbol table. Refs whose source file is in
    /// `scope_files` are resolved from the file query; name-set refs are added
    /// only when their source file is NOT in `scope_files` (otherwise the file
    /// query already covers them). Resolved rows are deleted exactly as the full
    /// pass does, so the resulting DB is structurally equal to `index --force`.
    pub fn resolve_incremental_and_persist(
        &mut self,
        store: &mut Store,
        scope_files: &std::collections::HashSet<String>,
        changed_names: &std::collections::HashSet<String>,
    ) -> anyhow::Result<ResolutionResult> {
        let files: Vec<String> = scope_files.iter().cloned().collect();
        let names: Vec<String> = changed_names.iter().cloned().collect();

        let by_files = store.unresolved_refs_by_files(&files)?;
        let by_names = store.unresolved_refs_by_names(&names)?;

        // The file query already returns every row whose source file is in
        // `scope_files`, including genuine duplicate rows (the same source
        // resolving to the same target more than once), which must all be kept to
        // reproduce the full pass's edge multiplicity. The name query only adds
        // rows whose source file is OUTSIDE `scope_files`, so the two sets are
        // disjoint and need no cross-query de-duplication.
        let mut scoped: Vec<UnresolvedRef> = by_files;
        for reference in by_names {
            if !scope_files.contains(&reference.file_path) {
                scoped.push(reference);
            }
        }

        let result = {
            let context = crate::context::StoreResolutionContext::new(store, &self.project_root);
            self.resolve_all(&scoped, &context)
        };

        let edges = self.create_edges(&result.resolved, store);
        if !edges.is_empty() {
            store.insert_edges(&edges)?;
        }

        if !result.resolved.is_empty() {
            delete_resolved_rows(store, &result.resolved)?;
        }

        // #750 conformance pass then #808 supertype pass, after implements/extends
        // edges exist (index.ts:508-511).
        self.resolve_chained_calls_via_conformance(store)?;
        self.resolve_deferred_this_member_refs(store)?;

        Ok(result)
    }

    /// Check if a reference is to a built-in / external symbol
    /// (`isBuiltInOrExternal`, `index.ts:982-1081`).
    fn is_built_in_or_external(&self, reference: &RefView) -> bool {
        let name = &reference.reference_name;
        let is_js_ts = matches!(
            reference.language,
            Language::TypeScript | Language::JavaScript | Language::Tsx | Language::Jsx
        );

        if is_js_ts && js_built_ins().contains(name.as_str()) {
            return true;
        }
        if is_js_ts
            && (name.starts_with("console.")
                || name.starts_with("Math.")
                || name.starts_with("JSON."))
        {
            return true;
        }
        if is_js_ts && react_hooks().contains(name.as_str()) {
            return true;
        }

        if reference.language == Language::Python && python_built_ins().contains(name.as_str()) {
            return true;
        }

        if reference.language == Language::Python {
            if let Some(dot) = name.find('.') {
                if dot > 0 {
                    let receiver = &name[..dot];
                    let method = &name[dot + 1..];
                    if python_built_in_types().contains(receiver) {
                        return true;
                    }
                    if python_built_in_methods().contains(method) {
                        let capitalized = capitalize(receiver);
                        if !self
                            .known_names
                            .as_ref()
                            .is_some_and(|k| k.contains(&capitalized))
                        {
                            return true;
                        }
                    }
                }
            }
            if python_built_in_methods().contains(name.as_str())
                && !self.known_names.as_ref().is_some_and(|k| k.contains(name))
            {
                return true;
            }
        }

        if reference.language == Language::Go {
            if let Some(dot) = name.find('.') {
                if dot > 0 {
                    let pkg = &name[..dot];
                    if go_stdlib_packages().contains(pkg) {
                        return true;
                    }
                }
            }
            if go_built_ins().contains(name.as_str()) {
                return true;
            }
        }

        if reference.language == Language::Pascal {
            if PASCAL_UNIT_PREFIXES.iter().any(|p| name.starts_with(p)) {
                return true;
            }
            if pascal_built_ins().contains(name.as_str()) {
                return true;
            }
        }

        if matches!(reference.language, Language::C | Language::Cpp) {
            if name.starts_with("std::") {
                return true;
            }
            if c_built_ins().contains(name.as_str()) || cpp_built_ins().contains(name.as_str()) {
                return !self.has_any_possible_match(name);
            }
        }

        false
    }

    /// Fast pre-filter (`hasAnyPossibleMatch`, `index.ts:579-628`).
    fn has_any_possible_match(&self, name: &str) -> bool {
        let Some(known) = &self.known_names else {
            return true;
        };
        if known.contains(name) {
            return true;
        }

        if let Some(dot) = name.find('.') {
            if dot > 0 {
                let receiver = &name[..dot];
                let member = &name[dot + 1..];
                if known.contains(receiver) || known.contains(member) {
                    return true;
                }
                let capitalized = capitalize(receiver);
                if known.contains(&capitalized) {
                    return true;
                }
                if let Some(last_dot) = name.rfind('.') {
                    if last_dot > dot {
                        let tail = &name[last_dot + 1..];
                        if !tail.is_empty() && known.contains(tail) {
                            return true;
                        }
                    }
                }
            }
        }

        if let Some(colon) = name.find("::") {
            if colon > 0 {
                let receiver = &name[..colon];
                let member = &name[colon + 2..];
                if known.contains(receiver) || known.contains(member) {
                    return true;
                }
                if let Some(last_colon) = name.rfind("::") {
                    if last_colon > colon {
                        let tail = &name[last_colon + 2..];
                        if !tail.is_empty() && known.contains(tail) {
                            return true;
                        }
                    }
                }
            }
        }

        if let Some(slash) = name.rfind('/') {
            if slash > 0 {
                let file_name = &name[slash + 1..];
                if known.contains(file_name) {
                    return true;
                }
            }
        }

        false
    }

    /// Does `reference.reference_name` match an import in its file?
    /// (`matchesAnyImport`, `index.ts:635-647`).
    fn matches_any_import(&self, reference: &RefView, context: &dyn ResolutionContext) -> bool {
        let imports = context.get_import_mappings(&reference.file_path, reference.language);
        imports.iter().any(|imp| {
            imp.local_name == reference.reference_name
                || reference
                    .reference_name
                    .starts_with(&format!("{}.", imp.local_name))
        })
    }

    /// Drop an import/name-strategy result that crosses a language family
    /// (`gateLanguage`, `index.ts:1160-1167`).
    fn gate_language(
        &self,
        result: Option<ResolvedRef>,
        reference: &RefView,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        let result = result?;
        let Some(target_language) = self.language_of_target(&result.target_node_id, context) else {
            return Some(result);
        };
        if reference.reference_kind == EdgeKind::References
            && !same_language_family(target_language, reference.language)
        {
            return None;
        }
        if reference.reference_kind == EdgeKind::Imports
            && crosses_known_family(target_language, reference.language)
        {
            return None;
        }
        Some(result)
    }

    /// Drop a `FrameworkResolver`-strategy result that crosses two known families
    /// (`gateFrameworkResolverLanguage`, `index.ts:1182-1188`). Never fires in v1 (the
    /// `FrameworkResolver` extension-point list is empty).
    fn gate_framework_resolver_extension_language(
        &self,
        result: Option<ResolvedRef>,
        reference: &RefView,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        let result = result?;
        if !matches!(
            reference.reference_kind,
            EdgeKind::References | EdgeKind::Imports
        ) {
            return Some(result);
        }
        if let Some(target_language) = self.language_of_target(&result.target_node_id, context) {
            if crosses_known_family(target_language, reference.language) {
                return None;
            }
        }
        Some(result)
    }

    /// Resolve a target node id to its language (`getLanguageFromNodeId`,
    /// `index.ts:1094-1097`); `None` when the node is absent (gate passes through).
    fn language_of_target(
        &self,
        target_node_id: &str,
        context: &dyn ResolutionContext,
    ) -> Option<Language> {
        context.get_node_by_id(target_node_id).map(|n| n.language)
    }
}

/// Denormalize a stored [`UnresolvedRef`] into a [`RefView`] (`index.ts:522-531`).
fn to_ref_view(reference: &UnresolvedRef) -> RefView {
    RefView {
        from_node_id: reference.from_node_id.clone(),
        reference_name: reference.reference_name.clone(),
        reference_kind: reference.reference_kind,
        line: reference.line,
        column: reference.col,
        file_path: reference.file_path.clone(),
        language: reference.language,
        is_function_ref: reference.is_function_ref,
        reference_subkind: reference.reference_subkind,
    }
}

/// Convert a framework-extracted [`RefView`] back into a stored [`UnresolvedRef`]
/// for persistence (inverse of [`to_ref_view`]).
fn ref_view_to_unresolved(reference: &RefView) -> UnresolvedRef {
    UnresolvedRef {
        id: None,
        from_node_id: reference.from_node_id.clone(),
        reference_name: reference.reference_name.clone(),
        reference_kind: reference.reference_kind,
        line: reference.line,
        col: reference.column,
        candidates: None,
        file_path: reference.file_path.clone(),
        language: reference.language,
        is_function_ref: reference.is_function_ref,
        reference_subkind: reference.reference_subkind,
    }
}

/// Build a resolved edge's `metadata` JSON: `confidence`+`resolvedBy`, plus
/// `fnRef: true` for function-as-value edges (#756, index.ts:824-827) and
/// `subkind` for Godot refs carrying a `reference_subkind`. The base shape (no
/// `fnRef`, no `subkind`) stays byte-identical to the prior output, keeping
/// existing resolved-edge goldens stable.
fn build_edge_metadata(reference: &ResolvedRef) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "confidence".to_string(),
        serde_json::json!(reference.confidence),
    );
    map.insert(
        "resolvedBy".to_string(),
        serde_json::json!(reference.resolved_by.as_str()),
    );
    if reference.original.is_function_ref {
        map.insert("fnRef".to_string(), serde_json::json!(true));
    }
    if let Some(subkind) = reference.original.reference_subkind {
        map.insert("subkind".to_string(), serde_json::json!(subkind.as_str()));
    }
    serde_json::Value::Object(map)
}

/// Does a `FrameworkResolver` apply to `language`? (`getApplicableFrameworks`,
/// `frameworks/index.ts:104-111`): no `languages` list = universal.
fn applies_to_language(resolver: &dyn FrameworkResolver, language: Language) -> bool {
    match resolver.languages() {
        None => true,
        Some(langs) => langs.contains(&language),
    }
}

/// SUPERTYPE_BEARING kinds plus `module` (index.ts:1219 / 1273).
fn is_supertype_bearing_or_module(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Struct
            | NodeKind::Interface
            | NodeKind::Trait
            | NodeKind::Protocol
            | NodeKind::Enum
            | NodeKind::Module
    )
}

/// NODE-anchored BFS up the supertype graph for an inherited `member`
/// (index.ts:1290-1346). The frontier starts at the class node in the ref's
/// own file (a same-named class elsewhere is wrong); supertypes are followed
/// via implements/extends edges, members looked up via `contains` edges. No
/// name-based unions. Returns the target node id, if any.
fn find_inherited_member(
    store: &Store,
    class_name: &str,
    member: &str,
    reference: &RefView,
) -> Option<String> {
    let by_name = store.nodes_by_name(class_name).unwrap_or_default();
    let mut frontier: Vec<Node> = by_name
        .iter()
        .filter(|n| is_supertype_bearing_or_module(n.kind) && n.file_path == reference.file_path)
        .cloned()
        .collect();
    if frontier.is_empty() {
        frontier = by_name
            .into_iter()
            .filter(|n| {
                is_supertype_bearing_or_module(n.kind)
                    && same_language_family(n.language, reference.language)
            })
            .collect();
    }

    let mut seen: std::collections::HashSet<String> =
        frontier.iter().map(|n| n.id.clone()).collect();

    for _ in 0..5 {
        if frontier.is_empty() {
            return None;
        }
        let mut next: Vec<Node> = Vec::new();
        for type_node in &frontier {
            for kind in [EdgeKind::Implements, EdgeKind::Extends] {
                let edges = store
                    .edges_by_source_kind(&type_node.id, Some(kind))
                    .unwrap_or_default();
                for edge in edges {
                    let Some(super_node) = store.node_by_id(&edge.target).ok().flatten() else {
                        continue;
                    };
                    if !seen.insert(super_node.id.clone()) {
                        continue;
                    }
                    if !is_supertype_bearing_or_module(super_node.kind) {
                        continue;
                    }
                    let contains = store
                        .edges_by_source_kind(&super_node.id, Some(EdgeKind::Contains))
                        .unwrap_or_default();
                    for c in contains {
                        if let Some(m) = store.node_by_id(&c.target).ok().flatten() {
                            if m.name == member
                                && matches!(m.kind, NodeKind::Function | NodeKind::Method)
                                && same_language_family(m.language, reference.language)
                            {
                                return Some(m.id);
                            }
                        }
                    }
                    next.push(super_node);
                }
            }
        }
        frontier = next;
    }
    None
}

/// Return the higher-confidence of two candidates (`reduce` in `index.ts:743-745`).
fn highest_confidence(best: ResolvedRef, curr: ResolvedRef) -> ResolvedRef {
    if curr.confidence > best.confidence {
        curr
    } else {
        best
    }
}

/// Delete resolved rows from `unresolved_refs`
/// (`deleteSpecificResolvedReferences`, `index.ts:811-817`).
fn delete_resolved_rows(store: &mut Store, resolved: &[ResolvedRef]) -> anyhow::Result<()> {
    let keys: Vec<(String, String, EdgeKind)> = resolved
        .iter()
        .map(|r| {
            (
                r.original.from_node_id.clone(),
                r.original.reference_name.clone(),
                r.original.reference_kind,
            )
        })
        .collect();
    store.delete_resolved_unresolved_refs(&keys)?;
    Ok(())
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ImportMapping;
    use codegraph_core::types::FileRecord;
    use std::collections::HashMap;

    // ---------------------------------------------------------------------
    // Free-function units (no store needed)
    // ---------------------------------------------------------------------

    #[test]
    fn capitalize_units() {
        assert_eq!(capitalize("foo"), "Foo");
        assert_eq!(capitalize(""), "");
        assert_eq!(capitalize("A"), "A");
    }

    #[test]
    fn is_chain_language_covers_members_and_negatives() {
        for lang in [
            Language::Java,
            Language::Kotlin,
            Language::CSharp,
            Language::Swift,
            Language::Rust,
            Language::Go,
            Language::Scala,
            Language::Dart,
            Language::ObjC,
            Language::Pascal,
        ] {
            assert!(is_chain_language(lang), "{lang:?} should chain");
        }
        assert!(!is_chain_language(Language::Python));
        assert!(!is_chain_language(Language::TypeScript));
    }

    #[test]
    fn is_scoped_chain_language_only_rust() {
        assert!(is_scoped_chain_language(Language::Rust));
        assert!(!is_scoped_chain_language(Language::Java));
    }

    #[test]
    fn has_chain_shape_units() {
        assert!(has_chain_shape("foo().bar"));
        assert!(has_chain_shape("a.b().method_1"));
        assert!(!has_chain_shape("plain"));
        // empty inner before "()." fails.
        assert!(!has_chain_shape("().bar"));
        // empty method after "()." fails.
        assert!(!has_chain_shape("foo()."));
        // non-ident method char fails.
        assert!(!has_chain_shape("foo().bar-baz"));
    }

    #[test]
    fn builtin_sets_contain_expected_members() {
        assert!(js_built_ins().contains("console"));
        assert!(react_hooks().contains("useState"));
        assert!(python_built_ins().contains("print"));
        assert!(python_built_in_types().contains("dict"));
        assert!(python_built_in_methods().contains("append"));
        assert!(go_stdlib_packages().contains("fmt"));
        assert!(go_built_ins().contains("make"));
        assert!(pascal_built_ins().contains("WriteLn"));
        assert!(c_built_ins().contains("printf"));
        assert!(cpp_built_ins().contains("cout"));
    }

    #[test]
    fn highest_confidence_picks_greater() {
        let a = resolved_ref("a", 0.5);
        let b = resolved_ref("b", 0.9);
        assert_eq!(highest_confidence(a.clone(), b.clone()).target_node_id, "b");
        // Ties keep the first argument (`best`).
        let c = resolved_ref("c", 0.9);
        assert_eq!(highest_confidence(b, c).target_node_id, "b");
    }

    #[test]
    fn is_supertype_bearing_or_module_units() {
        for k in [
            NodeKind::Class,
            NodeKind::Struct,
            NodeKind::Interface,
            NodeKind::Trait,
            NodeKind::Protocol,
            NodeKind::Enum,
            NodeKind::Module,
        ] {
            assert!(is_supertype_bearing_or_module(k), "{k:?}");
        }
        assert!(!is_supertype_bearing_or_module(NodeKind::Function));
    }

    #[test]
    fn to_ref_view_and_back_roundtrip() {
        let stored = UnresolvedRef {
            id: Some(7),
            from_node_id: "from".to_string(),
            reference_name: "X".to_string(),
            reference_kind: EdgeKind::Calls,
            line: 3,
            col: 4,
            candidates: None,
            file_path: "a.ts".to_string(),
            language: Language::TypeScript,
            is_function_ref: true,
            reference_subkind: None,
        };
        let view = to_ref_view(&stored);
        assert_eq!(view.from_node_id, "from");
        assert_eq!(view.line, 3);
        assert_eq!(view.column, 4);
        assert!(view.is_function_ref);
        let back = ref_view_to_unresolved(&view);
        assert_eq!(back.reference_name, "X");
        assert_eq!(back.col, 4);
        assert!(back.is_function_ref);
        assert_eq!(back.id, None);
    }

    #[test]
    fn build_edge_metadata_base_and_fn_ref() {
        let mut r = resolved_ref("t", 0.75);
        let base = build_edge_metadata(&r);
        assert_eq!(base["confidence"].as_f64(), Some(0.75));
        assert_eq!(base["resolvedBy"].as_str(), Some("import"));
        assert!(base.get("fnRef").is_none());
        r.original.is_function_ref = true;
        let with_fn = build_edge_metadata(&r);
        assert_eq!(with_fn["fnRef"].as_bool(), Some(true));
    }

    #[test]
    fn build_edge_metadata_includes_subkind() {
        let mut r = resolved_ref("t", 0.9);
        r.original.reference_subkind = Some(codegraph_core::types::ReferenceSubkind::ScriptAttach);
        let meta = build_edge_metadata(&r);
        assert_eq!(meta["subkind"].as_str(), Some("script_attach"));
    }

    #[test]
    fn applies_to_language_universal_and_scoped() {
        assert!(applies_to_language(&ReactLike, Language::TypeScript));
        assert!(!applies_to_language(&ReactLike, Language::Python));
        assert!(applies_to_language(&Universal, Language::Python));
    }

    // ---------------------------------------------------------------------
    // is_built_in_or_external / has_any_possible_match / gates via a resolver
    // ---------------------------------------------------------------------

    #[test]
    fn is_built_in_or_external_js_family() {
        let r = ReferenceResolver::new("/root");
        assert!(r.is_built_in_or_external(&mk_ref(
            "console",
            EdgeKind::Calls,
            Language::TypeScript
        )));
        assert!(r.is_built_in_or_external(&mk_ref(
            "console.log",
            EdgeKind::Calls,
            Language::TypeScript
        )));
        assert!(r.is_built_in_or_external(&mk_ref(
            "Math.max",
            EdgeKind::Calls,
            Language::JavaScript
        )));
        assert!(r.is_built_in_or_external(&mk_ref("useState", EdgeKind::Calls, Language::Tsx)));
        assert!(!r.is_built_in_or_external(&mk_ref(
            "myFunc",
            EdgeKind::Calls,
            Language::TypeScript
        )));
    }

    #[test]
    fn is_built_in_or_external_python_receiver_and_method() {
        let r = ReferenceResolver::new("/root");
        // built-in type receiver dict.get.
        assert!(r.is_built_in_or_external(&mk_ref("dict.get", EdgeKind::Calls, Language::Python)));
        // built-in method with unknown capitalized receiver -> external.
        assert!(r.is_built_in_or_external(&mk_ref("x.append", EdgeKind::Calls, Language::Python)));
        // bare built-in.
        assert!(r.is_built_in_or_external(&mk_ref("print", EdgeKind::Calls, Language::Python)));
        // bare built-in method with no known name -> external.
        assert!(r.is_built_in_or_external(&mk_ref("append", EdgeKind::Calls, Language::Python)));
    }

    #[test]
    fn is_built_in_or_external_python_method_known_type_not_external() {
        let mut r = ReferenceResolver::new("/root");
        // If the capitalized receiver IS a known name, x.append is NOT external.
        let mut known = BTreeSet::new();
        known.insert("X".to_string());
        r.known_names = Some(known);
        assert!(!r.is_built_in_or_external(&mk_ref("x.append", EdgeKind::Calls, Language::Python)));
    }

    #[test]
    fn is_built_in_or_external_go_pascal_c_cpp() {
        let mut r = ReferenceResolver::new("/root");
        assert!(r.is_built_in_or_external(&mk_ref("fmt.Println", EdgeKind::Calls, Language::Go)));
        assert!(r.is_built_in_or_external(&mk_ref("make", EdgeKind::Calls, Language::Go)));
        assert!(r.is_built_in_or_external(&mk_ref(
            "System.SysUtils",
            EdgeKind::Imports,
            Language::Pascal
        )));
        assert!(r.is_built_in_or_external(&mk_ref("WriteLn", EdgeKind::Calls, Language::Pascal)));
        assert!(r.is_built_in_or_external(&mk_ref("std::vector", EdgeKind::Calls, Language::Cpp)));
        // A C built-in is external only when it has NO possible match; a warmed
        // (empty) known-set makes has_any_possible_match false, so printf is external.
        r.known_names = Some(BTreeSet::new());
        assert!(r.is_built_in_or_external(&mk_ref("printf", EdgeKind::Calls, Language::C)));
    }

    #[test]
    fn has_any_possible_match_none_known_returns_true() {
        // Without warmed caches, everything is a possible match.
        let r = ReferenceResolver::new("/root");
        assert!(r.has_any_possible_match("anything"));
    }

    #[test]
    fn has_any_possible_match_dotted_scoped_and_slash() {
        let mut r = ReferenceResolver::new("/root");
        let mut known = BTreeSet::new();
        known.insert("Foo".to_string());
        known.insert("bar".to_string());
        known.insert("Widget".to_string());
        known.insert("helper".to_string());
        r.known_names = Some(known);
        // direct.
        assert!(r.has_any_possible_match("Foo"));
        // dotted receiver known.
        assert!(r.has_any_possible_match("Foo.method"));
        // dotted member known.
        assert!(r.has_any_possible_match("obj.bar"));
        // capitalized receiver known (foo -> Foo).
        assert!(r.has_any_possible_match("foo.thing"));
        // scoped receiver/member.
        assert!(r.has_any_possible_match("Widget::render"));
        assert!(r.has_any_possible_match("thing::helper"));
        // slash tail known.
        assert!(r.has_any_possible_match("path/to/Foo"));
        // nothing known.
        assert!(!r.has_any_possible_match("unrelated.symbol"));
    }

    // ---------------------------------------------------------------------
    // Store-driven: create_edges promotions + resolve_all/persist paths
    // ---------------------------------------------------------------------

    fn temp_db(slug: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        p.push(format!(
            "codegraph-resolver-ut-{slug}-{}-{nanos}.db",
            std::process::id()
        ));
        p
    }

    #[test]
    fn create_edges_promotes_extends_to_implements_and_calls_to_instantiates() {
        let mut store = Store::open(&temp_db("promote")).expect("open");
        // Interface + non-interface source -> extends promotes to implements.
        let iface = mk_node("interface:I", NodeKind::Interface, "I", "a.ts");
        let cls = mk_node("class:C", NodeKind::Class, "C", "a.ts");
        // A class target for calls->instantiates.
        let target_cls = mk_node("class:D", NodeKind::Class, "D", "a.ts");
        let caller = mk_node("function:f", NodeKind::Function, "f", "a.ts");
        store
            .upsert_nodes(&[
                iface.clone(),
                cls.clone(),
                target_cls.clone(),
                caller.clone(),
            ])
            .expect("upsert");

        let resolver = ReferenceResolver::new("/root");

        let extends = ResolvedRef {
            original: RefView {
                from_node_id: cls.id.clone(),
                reference_name: "I".to_string(),
                reference_kind: EdgeKind::Extends,
                line: 1,
                column: 0,
                file_path: "a.ts".to_string(),
                language: Language::TypeScript,
                is_function_ref: false,
                reference_subkind: None,
            },
            target_node_id: iface.id.clone(),
            confidence: 0.9,
            resolved_by: ResolvedBy::Import,
        };
        let calls = ResolvedRef {
            original: RefView {
                from_node_id: caller.id.clone(),
                reference_name: "D".to_string(),
                reference_kind: EdgeKind::Calls,
                line: 2,
                column: 0,
                file_path: "a.ts".to_string(),
                language: Language::TypeScript,
                is_function_ref: false,
                reference_subkind: None,
            },
            target_node_id: target_cls.id.clone(),
            confidence: 0.9,
            resolved_by: ResolvedBy::Import,
        };
        let edges = resolver.create_edges(&[extends, calls], &store);
        assert_eq!(edges.len(), 2);
        assert!(edges.iter().any(|e| e.kind == EdgeKind::Implements));
        assert!(edges.iter().any(|e| e.kind == EdgeKind::Instantiates));
    }

    #[test]
    fn resolve_all_reports_stats_and_skips_built_ins() {
        let mut store = Store::open(&temp_db("resolveall")).expect("open");
        let add = mk_node("function:add", NodeKind::Function, "add", "math.ts");
        store
            .upsert_nodes(std::slice::from_ref(&add))
            .expect("upsert");
        let mut resolver = ReferenceResolver::new("/root");

        let refs = vec![
            // resolvable by name match.
            UnresolvedRef {
                id: Some(1),
                from_node_id: "function:caller".to_string(),
                reference_name: "add".to_string(),
                reference_kind: EdgeKind::Calls,
                line: 1,
                col: 0,
                candidates: None,
                file_path: "app.ts".to_string(),
                language: Language::TypeScript,
                is_function_ref: false,
                reference_subkind: None,
            },
            // built-in, skipped (unresolved).
            UnresolvedRef {
                id: Some(2),
                from_node_id: "function:caller".to_string(),
                reference_name: "console".to_string(),
                reference_kind: EdgeKind::Calls,
                line: 2,
                col: 0,
                candidates: None,
                file_path: "app.ts".to_string(),
                language: Language::TypeScript,
                is_function_ref: false,
                reference_subkind: None,
            },
        ];
        let ctx = crate::context::StoreResolutionContext::new(&store, "/root");
        let result = resolver.resolve_all(&refs, &ctx);
        assert_eq!(result.stats.total, 2);
        assert_eq!(result.stats.resolved, 1);
        assert_eq!(result.stats.unresolved, 1);
    }

    #[test]
    fn resolve_and_persist_batched_matches_serial_edge_count() {
        // Build the same tiny graph twice; the serial and batched persist paths
        // must produce the same number of resolved edges.
        let serial = run_persist(false);
        let batched = run_persist(true);
        assert_eq!(serial, batched);
        assert!(serial > 0, "expected at least one resolved edge");
    }

    fn run_persist(batched: bool) -> usize {
        let slug = if batched { "batched" } else { "serial" };
        let mut store = Store::open(&temp_db(slug)).expect("open");
        let add = mk_node("function:add", NodeKind::Function, "add", "math.ts");
        let caller = mk_node("function:run", NodeKind::Function, "run", "app.ts");
        store.upsert_file(&file_rec("math.ts")).expect("file");
        store.upsert_file(&file_rec("app.ts")).expect("file");
        store
            .upsert_nodes(&[add.clone(), caller.clone()])
            .expect("nodes");
        store
            .insert_unresolved_refs(&[UnresolvedRef {
                id: None,
                from_node_id: caller.id.clone(),
                reference_name: "add".to_string(),
                reference_kind: EdgeKind::Calls,
                line: 1,
                col: 0,
                candidates: None,
                file_path: "app.ts".to_string(),
                language: Language::TypeScript,
                is_function_ref: false,
                reference_subkind: None,
            }])
            .expect("refs");
        let mut resolver = ReferenceResolver::new("/root");
        if batched {
            resolver
                .resolve_and_persist_batched(&mut store, 10)
                .expect("batched");
        } else {
            resolver.resolve_and_persist(&mut store).expect("serial");
        }
        store
            .edges_by_source_kind(&caller.id, Some(EdgeKind::Calls))
            .expect("edges")
            .len()
    }

    #[test]
    fn batched_pass_clears_incomplete_marker_on_success() {
        let mut store = Store::open(&temp_db("marker-clear")).expect("open");
        let add = mk_node("function:add", NodeKind::Function, "add", "math.ts");
        let caller = mk_node("function:run", NodeKind::Function, "run", "app.ts");
        store.upsert_file(&file_rec("math.ts")).expect("file");
        store.upsert_file(&file_rec("app.ts")).expect("file");
        store.upsert_nodes(&[add, caller.clone()]).expect("nodes");
        store
            .insert_unresolved_refs(&[UnresolvedRef {
                id: None,
                from_node_id: caller.id,
                reference_name: "add".to_string(),
                reference_kind: EdgeKind::Calls,
                line: 1,
                col: 0,
                candidates: None,
                file_path: "app.ts".to_string(),
                language: Language::TypeScript,
                is_function_ref: false,
                reference_subkind: None,
            }])
            .expect("refs");
        let mut resolver = ReferenceResolver::new("/root");
        resolver
            .resolve_and_persist_batched(&mut store, 10)
            .expect("batched");
        assert!(
            !store.is_resolution_incomplete().expect("marker read"),
            "a completed batched pass must clear the #1187 incomplete marker"
        );
    }

    #[test]
    fn resolve_and_persist_batched_reports_progress() {
        let mut store = Store::open(&temp_db("progress")).expect("open");
        let add = mk_node("function:add", NodeKind::Function, "add", "math.ts");
        let caller = mk_node("function:run", NodeKind::Function, "run", "app.ts");
        store.upsert_file(&file_rec("math.ts")).expect("file");
        store.upsert_file(&file_rec("app.ts")).expect("file");
        store.upsert_nodes(&[add, caller.clone()]).expect("nodes");
        store
            .insert_unresolved_refs(&[UnresolvedRef {
                id: None,
                from_node_id: caller.id,
                reference_name: "add".to_string(),
                reference_kind: EdgeKind::Calls,
                line: 1,
                col: 0,
                candidates: None,
                file_path: "app.ts".to_string(),
                language: Language::TypeScript,
                is_function_ref: false,
                reference_subkind: None,
            }])
            .expect("refs");
        let mut resolver = ReferenceResolver::new("/root");
        let mut last_total = 0u64;
        resolver
            .resolve_and_persist_batched_with_progress(&mut store, 10, |_processed, total| {
                last_total = total;
            })
            .expect("batched");
        assert_eq!(last_total, 1);
    }

    #[test]
    fn resolve_incremental_and_persist_resolves_scope_file() {
        let mut store = Store::open(&temp_db("incremental")).expect("open");
        let add = mk_node("function:add", NodeKind::Function, "add", "math.ts");
        let caller = mk_node("function:run", NodeKind::Function, "run", "app.ts");
        store.upsert_file(&file_rec("math.ts")).expect("file");
        store.upsert_file(&file_rec("app.ts")).expect("file");
        store.upsert_nodes(&[add, caller.clone()]).expect("nodes");
        store
            .insert_unresolved_refs(&[UnresolvedRef {
                id: None,
                from_node_id: caller.id.clone(),
                reference_name: "add".to_string(),
                reference_kind: EdgeKind::Calls,
                line: 1,
                col: 0,
                candidates: None,
                file_path: "app.ts".to_string(),
                language: Language::TypeScript,
                is_function_ref: false,
                reference_subkind: None,
            }])
            .expect("refs");
        let mut resolver = ReferenceResolver::new("/root");
        let mut scope = std::collections::HashSet::new();
        scope.insert("app.ts".to_string());
        let names = std::collections::HashSet::new();
        let result = resolver
            .resolve_incremental_and_persist(&mut store, &scope, &names)
            .expect("incremental");
        assert_eq!(result.stats.resolved, 1);
    }

    #[test]
    fn has_framework_resolvers_and_initialize() {
        let mut resolver = ReferenceResolver::new("/root");
        assert!(!resolver.has_framework_resolvers());
        assert_eq!(resolver.project_root(), "/root");
        let ctx = MinimalCtx {
            files: HashMap::from([(
                "package.json".to_string(),
                r#"{"dependencies":{"react":"18"}}"#.to_string(),
            )]),
        };
        resolver.initialize(&ctx);
        assert!(resolver.has_framework_resolvers());
    }

    #[test]
    fn run_post_extract_and_extract_frameworks_noop_without_detection() {
        let mut store = Store::open(&temp_db("noframework")).expect("open");
        let resolver = ReferenceResolver::new("/root");
        assert_eq!(resolver.run_post_extract(&mut store).expect("post"), 0);
        assert!(
            resolver
                .extract_and_persist_frameworks(&mut store, &["a.ts".to_string()])
                .is_ok()
        );
    }

    #[test]
    fn deferred_passes_empty_are_noops() {
        let mut store = Store::open(&temp_db("deferred")).expect("open");
        let resolver = ReferenceResolver::new("/root");
        assert_eq!(
            resolver
                .resolve_deferred_this_member_refs(&mut store)
                .expect("deferred"),
            0
        );
        assert_eq!(
            resolver
                .resolve_chained_calls_via_conformance(&mut store)
                .expect("chain"),
            0
        );
    }

    #[test]
    fn gate_language_none_passthrough_and_family_drop() {
        let mut store = Store::open(&temp_db("gate")).expect("open");
        let py_node = mk_node2(
            "function:py",
            NodeKind::Function,
            "py",
            "a.py",
            Language::Python,
        );
        store
            .upsert_nodes(std::slice::from_ref(&py_node))
            .expect("nodes");
        let resolver = ReferenceResolver::new("/root");
        let ctx = crate::context::StoreResolutionContext::new(&store, "/root");
        // None input -> None.
        assert!(
            resolver
                .gate_language(
                    None,
                    &mk_ref("x", EdgeKind::References, Language::TypeScript),
                    &ctx
                )
                .is_none()
        );
        // References ref to a Python target from a TS source -> dropped (different family).
        let candidate = ResolvedRef {
            original: mk_ref("py", EdgeKind::References, Language::TypeScript),
            target_node_id: py_node.id.clone(),
            confidence: 0.8,
            resolved_by: ResolvedBy::Import,
        };
        assert!(
            resolver
                .gate_language(
                    Some(candidate),
                    &mk_ref("py", EdgeKind::References, Language::TypeScript),
                    &ctx
                )
                .is_none()
        );
    }

    #[test]
    fn resolve_one_this_member_resolves_to_own_class_method() {
        // A `this.helper()` function_ref inside a class method resolves to the
        // class's own `helper` method (resolve_this_member_fn_ref_pure hit).
        let mut store = Store::open(&temp_db("thismember")).expect("open");
        let cls = mk_node2(
            "class:C",
            NodeKind::Class,
            "C",
            "a.ts",
            Language::TypeScript,
        );
        let mut helper = mk_node2(
            "method:helper",
            NodeKind::Method,
            "helper",
            "a.ts",
            Language::TypeScript,
        );
        helper.qualified_name = "a.ts::C::helper".to_string();
        helper.start_line = 5;
        let mut caller = mk_node2(
            "method:run",
            NodeKind::Method,
            "run",
            "a.ts",
            Language::TypeScript,
        );
        caller.qualified_name = "a.ts::C::run".to_string();
        caller.start_line = 2;
        store
            .upsert_nodes(&[cls, helper.clone(), caller.clone()])
            .expect("nodes");
        let mut resolver = ReferenceResolver::new("/root");
        resolver.warm_caches(&crate::context::StoreResolutionContext::new(
            &store, "/root",
        ));
        let ctx = crate::context::StoreResolutionContext::new(&store, "/root");
        let reference = RefView {
            from_node_id: caller.id.clone(),
            reference_name: "this.helper".to_string(),
            reference_kind: EdgeKind::References,
            line: 3,
            column: 0,
            file_path: "a.ts".to_string(),
            language: Language::TypeScript,
            is_function_ref: true,
            reference_subkind: None,
        };
        let resolved = resolver.resolve_one(&reference, &ctx).expect("resolves");
        assert_eq!(resolved.target_node_id, helper.id);
        assert_eq!(resolved.resolved_by, ResolvedBy::FunctionRef);
    }

    #[test]
    fn resolve_one_this_member_missing_defers_then_noop_without_supertypes() {
        // `this.absent` is not on the class; resolve_one defers it. Running the
        // #808 pass with no implements/extends edges resolves nothing.
        let mut store = Store::open(&temp_db("thisdefer")).expect("open");
        let cls = mk_node2(
            "class:C",
            NodeKind::Class,
            "C",
            "a.ts",
            Language::TypeScript,
        );
        let mut caller = mk_node2(
            "method:run",
            NodeKind::Method,
            "run",
            "a.ts",
            Language::TypeScript,
        );
        caller.qualified_name = "a.ts::C::run".to_string();
        store.upsert_nodes(&[cls, caller.clone()]).expect("nodes");
        let mut resolver = ReferenceResolver::new("/root");
        resolver.warm_caches(&crate::context::StoreResolutionContext::new(
            &store, "/root",
        ));
        {
            let ctx = crate::context::StoreResolutionContext::new(&store, "/root");
            let reference = RefView {
                from_node_id: caller.id.clone(),
                reference_name: "this.absent".to_string(),
                reference_kind: EdgeKind::References,
                line: 3,
                column: 0,
                file_path: "a.ts".to_string(),
                language: Language::TypeScript,
                is_function_ref: true,
                reference_subkind: None,
            };
            assert!(resolver.resolve_one(&reference, &ctx).is_none());
        }
        assert_eq!(
            resolver
                .resolve_deferred_this_member_refs(&mut store)
                .expect("pass"),
            0
        );
    }

    #[test]
    fn resolve_one_function_ref_via_import_target_kind() {
        // A bare function_ref (not this.*) resolves through match_function_ref to
        // the function node in the same file.
        let mut store = Store::open(&temp_db("fnref")).expect("open");
        let onblur = mk_node2(
            "function:onBlur",
            NodeKind::Function,
            "onBlur",
            "a.ts",
            Language::TypeScript,
        );
        let caller = mk_node2(
            "function:setup",
            NodeKind::Function,
            "setup",
            "a.ts",
            Language::TypeScript,
        );
        store
            .upsert_nodes(&[onblur.clone(), caller.clone()])
            .expect("nodes");
        let mut resolver = ReferenceResolver::new("/root");
        resolver.warm_caches(&crate::context::StoreResolutionContext::new(
            &store, "/root",
        ));
        let ctx = crate::context::StoreResolutionContext::new(&store, "/root");
        let reference = RefView {
            from_node_id: caller.id.clone(),
            reference_name: "onBlur".to_string(),
            reference_kind: EdgeKind::References,
            line: 2,
            column: 0,
            file_path: "a.ts".to_string(),
            language: Language::TypeScript,
            is_function_ref: true,
            reference_subkind: None,
        };
        let resolved = resolver.resolve_one(&reference, &ctx).expect("resolves");
        assert_eq!(resolved.target_node_id, onblur.id);
    }

    #[test]
    fn helper_stubs_are_exercised() {
        // Drives the test-only ReactLike / Universal / MinimalCtx stub methods so
        // they are not counted as uncovered noise.
        let ctx = MinimalCtx {
            files: HashMap::from([("f.ts".to_string(), "x".to_string())]),
        };
        for f in [ReactLike.name().to_string(), Universal.name().to_string()] {
            assert!(!f.is_empty());
        }
        assert!(ReactLike.languages().is_some());
        assert!(Universal.languages().is_none());
        assert!(ReactLike.detect(&ctx));
        assert!(Universal.detect(&ctx));
        assert!(
            ReactLike
                .resolve(&mk_ref("x", EdgeKind::Calls, Language::TypeScript), &ctx)
                .is_none()
        );
        assert!(
            Universal
                .resolve(&mk_ref("x", EdgeKind::Calls, Language::TypeScript), &ctx)
                .is_none()
        );
        assert!(ctx.get_nodes_in_file("f.ts").is_empty());
        assert!(ctx.get_nodes_by_name("x").is_empty());
        assert!(ctx.get_nodes_by_qualified_name("x").is_empty());
        assert!(ctx.get_nodes_by_kind(NodeKind::Function).is_empty());
        assert!(ctx.file_exists("f.ts"));
        assert_eq!(ctx.read_file("f.ts").as_deref(), Some("x"));
        assert_eq!(ctx.get_project_root(), "/root");
        assert_eq!(ctx.get_all_files().len(), 1);
        assert!(ctx.get_nodes_by_lower_name("x").is_empty());
        assert!(ctx.get_node_by_id("x").is_none());
        assert!(
            ctx.get_import_mappings("f.ts", Language::TypeScript)
                .is_empty()
        );
    }

    #[test]
    fn has_any_possible_match_deep_tail_branches() {
        let mut r = ReferenceResolver::new("/root");
        let mut known = BTreeSet::new();
        known.insert("leaf".to_string());
        known.insert("Scoped".to_string());
        r.known_names = Some(known);
        // Dotted chain a.b.leaf: none of head/cap match, but the last-dot tail does.
        assert!(r.has_any_possible_match("a.b.leaf"));
        // Scoped chain a::b::Scoped: last-colon tail matches.
        assert!(r.has_any_possible_match("a::b::Scoped"));
        // Neither head nor any tail known.
        assert!(!r.has_any_possible_match("a.b.c"));
        assert!(!r.has_any_possible_match("a::b::c"));
    }

    #[test]
    fn extract_and_persist_frameworks_runs_when_detected() {
        // With a react project detected, extract_and_persist_frameworks reads the
        // relative file and persists any framework nodes/refs it emits.
        let dir = std::env::temp_dir().join(format!(
            "codegraph-fw-extract-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("pages")).expect("mkdir");
        std::fs::write(
            dir.join("package.json"),
            r#"{"dependencies":{"react":"18"}}"#,
        )
        .expect("pkg");
        // The react resolver's languages() is [JavaScript, TypeScript]; a `.tsx`
        // maps to Language::Tsx (not applicable), so use a `.ts` page whose Next.js
        // route branch still fires on `export default`.
        std::fs::write(
            dir.join("pages/about.ts"),
            "export default function About() { return 1; }",
        )
        .expect("page");
        let mut store = Store::open(&temp_db("fwextract")).expect("open");
        let mut resolver = ReferenceResolver::new(dir.to_string_lossy().to_string());
        {
            let ctx = crate::context::StoreResolutionContext::new(
                &store,
                resolver.project_root().to_string(),
            );
            resolver.initialize(&ctx);
        }
        assert!(resolver.has_framework_resolvers());
        resolver
            .extract_and_persist_frameworks(&mut store, &["pages/about.ts".to_string()])
            .expect("extract");
        let routes = store.nodes_by_kind(NodeKind::Route).expect("routes");
        assert!(routes.iter().any(|n| n.name == "/about"), "got {routes:#?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn conformance_pass_resolves_inherited_this_member() {
        // Sub has no `greet`; its supertype Base does. A deferred `this.greet`
        // resolves via find_inherited_member's BFS once extends edges exist.
        let mut store = Store::open(&temp_db("inherit")).expect("open");
        let base = mk_node2(
            "class:Base",
            NodeKind::Class,
            "Base",
            "base.ts",
            Language::TypeScript,
        );
        let mut greet = mk_node2(
            "method:greet",
            NodeKind::Method,
            "greet",
            "base.ts",
            Language::TypeScript,
        );
        greet.qualified_name = "base.ts::Base::greet".to_string();
        let sub = mk_node2(
            "class:Sub",
            NodeKind::Class,
            "Sub",
            "sub.ts",
            Language::TypeScript,
        );
        let mut run = mk_node2(
            "method:run",
            NodeKind::Method,
            "run",
            "sub.ts",
            Language::TypeScript,
        );
        run.qualified_name = "sub.ts::Sub::run".to_string();
        store
            .upsert_nodes(&[base.clone(), greet.clone(), sub.clone(), run.clone()])
            .expect("nodes");
        // contains edges: Base -> greet ; extends edge: Sub -> Base.
        store
            .insert_edges(&[
                Edge {
                    id: None,
                    source: base.id.clone(),
                    target: greet.id.clone(),
                    kind: EdgeKind::Contains,
                    metadata: None,
                    line: Some(1),
                    col: Some(0),
                    provenance: None,
                },
                Edge {
                    id: None,
                    source: sub.id.clone(),
                    target: base.id.clone(),
                    kind: EdgeKind::Extends,
                    metadata: None,
                    line: Some(1),
                    col: Some(0),
                    provenance: None,
                },
            ])
            .expect("edges");
        let target = find_inherited_member(
            &store,
            "Sub",
            "greet",
            &RefView {
                from_node_id: run.id.clone(),
                reference_name: "this.greet".to_string(),
                reference_kind: EdgeKind::References,
                line: 2,
                column: 0,
                file_path: "sub.ts".to_string(),
                language: Language::TypeScript,
                is_function_ref: true,
                reference_subkind: None,
            },
        );
        assert_eq!(target.as_deref(), Some(greet.id.as_str()));
    }

    #[test]
    fn find_inherited_member_none_when_no_supertype_has_member() {
        // Class exists but neither it nor its (absent) supertypes declare `gone`.
        let mut store = Store::open(&temp_db("inherit-none")).expect("open");
        let cls = mk_node2(
            "class:C",
            NodeKind::Class,
            "C",
            "a.ts",
            Language::TypeScript,
        );
        store
            .upsert_nodes(std::slice::from_ref(&cls))
            .expect("nodes");
        let target = find_inherited_member(
            &store,
            "C",
            "gone",
            &RefView {
                from_node_id: "x".to_string(),
                reference_name: "this.gone".to_string(),
                reference_kind: EdgeKind::References,
                line: 1,
                column: 0,
                file_path: "a.ts".to_string(),
                language: Language::TypeScript,
                is_function_ref: true,
                reference_subkind: None,
            },
        );
        assert!(target.is_none());
    }

    // ---------------------------------------------------------------------
    // helpers
    // ---------------------------------------------------------------------

    struct ReactLike;
    impl FrameworkResolver for ReactLike {
        fn name(&self) -> &str {
            "react-like"
        }
        fn languages(&self) -> Option<&[Language]> {
            const L: [Language; 1] = [Language::TypeScript];
            Some(&L)
        }
        fn detect(&self, _c: &dyn ResolutionContext) -> bool {
            true
        }
        fn resolve(&self, _r: &RefView, _c: &dyn ResolutionContext) -> Option<ResolvedRef> {
            None
        }
    }

    struct Universal;
    impl FrameworkResolver for Universal {
        fn name(&self) -> &str {
            "universal"
        }
        fn detect(&self, _c: &dyn ResolutionContext) -> bool {
            true
        }
        fn resolve(&self, _r: &RefView, _c: &dyn ResolutionContext) -> Option<ResolvedRef> {
            None
        }
    }

    struct MinimalCtx {
        files: HashMap<String, String>,
    }
    impl ResolutionContext for MinimalCtx {
        fn get_nodes_in_file(&self, _f: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_nodes_by_name(&self, _n: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_nodes_by_qualified_name(&self, _q: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_nodes_by_kind(&self, _k: NodeKind) -> Vec<Node> {
            Vec::new()
        }
        fn file_exists(&self, f: &str) -> bool {
            self.files.contains_key(f)
        }
        fn read_file(&self, f: &str) -> Option<String> {
            self.files.get(f).cloned()
        }
        fn get_project_root(&self) -> &str {
            "/root"
        }
        fn get_all_files(&self) -> Vec<String> {
            self.files.keys().cloned().collect()
        }
        fn get_nodes_by_lower_name(&self, _n: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_node_by_id(&self, _id: &str) -> Option<Node> {
            None
        }
        fn get_import_mappings(&self, _f: &str, _l: Language) -> Vec<ImportMapping> {
            Vec::new()
        }
    }

    fn mk_ref(name: &str, kind: EdgeKind, lang: Language) -> RefView {
        RefView {
            from_node_id: "from".to_string(),
            reference_name: name.to_string(),
            reference_kind: kind,
            line: 1,
            column: 0,
            file_path: "a.ts".to_string(),
            language: lang,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    fn resolved_ref(target: &str, confidence: f64) -> ResolvedRef {
        ResolvedRef {
            original: mk_ref("x", EdgeKind::Calls, Language::TypeScript),
            target_node_id: target.to_string(),
            confidence,
            resolved_by: ResolvedBy::Import,
        }
    }

    fn mk_node(id: &str, kind: NodeKind, name: &str, file: &str) -> Node {
        mk_node2(id, kind, name, file, Language::TypeScript)
    }

    fn mk_node2(id: &str, kind: NodeKind, name: &str, file: &str, lang: Language) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: format!("{file}::{name}"),
            file_path: file.to_string(),
            language: lang,
            start_line: 1,
            end_line: 1,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }

    fn file_rec(path: &str) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            content_hash: "h".to_string(),
            language: Language::TypeScript,
            size: 0,
            modified_at: 0,
            indexed_at: 0,
            node_count: 0,
            errors: Vec::new(),
        }
    }
}
