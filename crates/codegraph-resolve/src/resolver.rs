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
    crosses_known_family, match_dotted_call_chain, match_function_ref, match_reference,
    match_scoped_call_chain, same_language_family,
};
use crate::snapshot_context::{build_edge_adjacency, SnapshotResolutionContext};
use crate::types::{
    RefView, ResolutionContext, ResolutionResult, ResolutionStats, ResolvedBy, ResolvedRef,
};
use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind, UnresolvedRef};
use codegraph_store::Store;
use rayon::prelude::*;
use std::collections::BTreeSet;
use std::sync::OnceLock;

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
                if let Some(result) = resolver.extract(relative, &content) {
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
                    // `::`-receiver languages (Rust) split on `::`; dotted-receiver
                    // languages on `.` (index.ts:890-892).
                    let chain_match = if is_scoped_chain_language(reference.language) {
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
                    metadata: Some(if reference.original.is_function_ref {
                        // Uniform marker for function-as-value edges (#756),
                        // regardless of resolution strategy (index.ts:824-827).
                        serde_json::json!({
                            "confidence": reference.confidence,
                            "resolvedBy": reference.resolved_by.as_str(),
                            "fnRef": true,
                        })
                    } else {
                        serde_json::json!({
                            "confidence": reference.confidence,
                            "resolvedBy": reference.resolved_by.as_str(),
                        })
                    }),
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
        {
            let context = crate::context::StoreResolutionContext::new(store, &self.project_root);
            self.warm_caches(&context);
        }

        let total_refs = store.unresolved_refs_count()? as u64;
        let mut processed: u64 = 0;

        // Built lazily on first-chunk entry, AFTER framework extraction injected
        // its nodes — never in `new`/`initialize` (would miss framework nodes).
        let mut node_snapshot: Option<SnapshotResolutionContext> = None;

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
                None => node_snapshot.insert(SnapshotResolutionContext::from_store(
                    store,
                    &self.project_root,
                )?),
            };
            let chunk_ctx = base.with_edge_adjacency(build_edge_adjacency(store)?);

            let result = self.resolve_chunk_parallel(&batch, &chunk_ctx);

            let edges = self.create_edges(&result.resolved, store);
            if !edges.is_empty() {
                store.insert_edges(&edges)?;
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
    }
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
