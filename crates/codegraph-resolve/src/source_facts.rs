//! Per-file source facts shared by the resolver's text-scanning gates.
//!
//! Several resolution gates inspect a file's source text for every reference
//! they see: the call site's own line, a comment-stripped view of the whole
//! file, or a JS/TS module-shape decision. Deriving those views afresh per
//! reference made resolution `O(references × file size)` on large files.
//!
//! A [`SourceFacts`] is built once per file and per resolution pass (production
//! contexts memoise it next to their file-content cache), and every derived
//! view is computed lazily, at most once. Each value is a pure function of the
//! source text — the memo slots additionally depend only on the immutable node
//! snapshot of the pass — so a cached answer is identical to recomputing it.

use crate::awaited::AwaitedIndex;
use crate::object_literal::LiteralLookup;
use crate::strip_comments::{CommentLang, blank_string_contents, strip_comments_for_regex};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

/// Newline positions of a text.
///
/// Answers both [`str::lines`]-compatible queries and the raw
/// `split('\n')` view that upstream's column arithmetic uses, without
/// materialising a `Vec<&str>` for every caller.
#[derive(Debug)]
pub struct LineIndex {
    newlines: Vec<usize>,
    len: usize,
}

impl LineIndex {
    /// Index every `\n` in `text`.
    pub fn new(text: &str) -> Self {
        Self {
            newlines: text
                .bytes()
                .enumerate()
                .filter_map(|(offset, byte)| (byte == b'\n').then_some(offset))
                .collect(),
            len: text.len(),
        }
    }

    /// Byte offset where 0-based line `index` starts when every `\n` counts
    /// as a line break, or `None` past the last newline.
    pub fn raw_line_start(&self, index: usize) -> Option<usize> {
        if index == 0 {
            Some(0)
        } else {
            self.newlines.get(index - 1).map(|newline| newline + 1)
        }
    }

    /// How many segments `text.split('\n')` yields: one more than the
    /// newlines (W5-08).
    pub fn raw_line_count(&self) -> usize {
        self.newlines.len() + 1
    }

    /// Segment `index` of `text.split('\n')` (a `\r` before the newline is
    /// kept, and a final empty segment after a trailing newline exists).
    pub fn raw_line<'t>(&self, text: &'t str, index: usize) -> Option<&'t str> {
        let start = self.raw_line_start(index)?;
        let end = self.newlines.get(index).copied().unwrap_or(self.len);
        Some(&text[start..end])
    }

    /// Number of lines [`str::lines`] yields for the indexed text.
    pub fn line_count(&self) -> usize {
        let tail_start = self.newlines.last().map_or(0, |newline| newline + 1);
        self.newlines.len() + usize::from(tail_start < self.len)
    }

    /// Line `index` exactly as [`str::lines`] yields it: a `\n` terminator is
    /// removed together with one `\r` before it; an unterminated final line is
    /// returned verbatim.
    ///
    /// # Panics
    /// When `index >= self.line_count()` or `text` is not the indexed text.
    pub fn line<'t>(&self, text: &'t str, index: usize) -> &'t str {
        let start = self
            .raw_line_start(index)
            .expect("line index within the indexed text");
        match self.newlines.get(index) {
            Some(&newline) => {
                let line = &text[start..newline];
                line.strip_suffix('\r').unwrap_or(line)
            }
            None => &text[start..self.len],
        }
    }

    /// `lines[start..end].join(separator)` over [`str::lines`].
    pub fn join(&self, text: &str, start: usize, end: usize, separator: &str) -> String {
        let mut joined = String::new();
        for index in start..end {
            if index > start {
                joined.push_str(separator);
            }
            joined.push_str(self.line(text, index));
        }
        joined
    }
}

/// Offsets where a JS/TS local binding can start (`localBindingSites`,
/// upstream v1.6.1): every `const`/`let`/`var` keyword, every
/// `function`/`class` keyword, and every `=>`.
#[derive(Debug, Default)]
pub struct LocalBindingSites {
    pub var_decls: Vec<usize>,
    pub fn_decls: Vec<usize>,
    pub arrows: Vec<usize>,
}

/// The declared type of a TS/JS class field, read off the class's own lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsFieldDeclaration {
    /// `field: typeof Value` — the type OF a value (an object-literal namespace).
    pub value_type: bool,
    /// The declared or constructed type as spelled (`ns.Mailer`, `Mailer`).
    pub type_name: String,
}

/// Lazily derived, memoised facts about one file's source text.
#[derive(Debug)]
pub struct SourceFacts {
    source: Arc<str>,
    lines: OnceLock<LineIndex>,
    ts_comment_free: OnceLock<(String, LineIndex)>,
    python_comment_free: OnceLock<(String, LineIndex)>,
    ts_blanked: OnceLock<String>,
    js_sealed_module: OnceLock<bool>,
    js_module_exports: OnceLock<Option<HashSet<String>>>,
    js_binding_sites: OnceLock<LocalBindingSites>,
    js_local_bindings: Mutex<HashMap<String, bool>>,
    node_decisions: Mutex<HashMap<(&'static str, String), bool>>,
    ts_field_declarations: Mutex<HashMap<(String, String), Option<TsFieldDeclaration>>>,
    js_get_state_file: OnceLock<bool>,
    js_selector_names: OnceLock<HashSet<String>>,
    literal_properties: Mutex<HashMap<(String, String), LiteralLookup>>,
    literal_binding_targets: Mutex<HashMap<(String, String), Option<String>>>,
    awaited_raw_names: OnceLock<HashSet<String>>,
    awaited_index: OnceLock<Arc<AwaitedIndex>>,
    rust_use_scopes: OnceLock<crate::rust_lexical::RustUseScopes>,
    /// What the file's `use` items bring in, read against the crates of the
    /// pass that first asked.
    rust_uses: OnceLock<(
        Arc<crate::rust_scope::RustCrates>,
        Arc<crate::rust_scope::RustUses>,
    )>,
    python_from_imports: OnceLock<HashMap<String, String>>,
    python_module_symbols: Mutex<HashMap<(String, usize), Option<String>>>,
    python_file_scan: OnceLock<Arc<crate::python_globals::PythonFileScan>>,
    python_name_scans: Mutex<HashMap<String, Arc<crate::python_globals::NameScan>>>,
    python_globals: Mutex<HashMap<String, Arc<crate::python_globals::GlobalMemo>>>,
    python_plugged_modules: OnceLock<Vec<String>>,
    python_fixture_types: Mutex<HashMap<String, Option<String>>>,
    minified_script: OnceLock<bool>,
    csharp_scopes: OnceLock<Arc<crate::csharp_scopes::CsharpScopes>>,
    sfc_script_blocks: OnceLock<Vec<crate::sfc_scripts::ScriptBlock>>,
    js_destructured_calls: OnceLock<Option<crate::destructured_calls::DestructuredFile>>,
    js_object_scan: OnceLock<Arc<crate::object_owners::JsObjectScan>>,
    cpp_local_alias_lines: Mutex<HashMap<String, Arc<Vec<usize>>>>,
    cpp_template_parameters: Mutex<HashMap<String, Arc<BTreeSet<String>>>>,
    cpp_class_bases: Mutex<HashMap<String, Arc<Vec<String>>>>,
    cpp_usings: OnceLock<Arc<Vec<crate::cpp_supertypes::CppUsing>>>,
    java_file_scope: OnceLock<Arc<crate::java_scope::JavaFileScope>>,
    java_heads: Mutex<HashMap<String, Arc<crate::java_scope::JavaHead>>>,
    kotlin_file_scope: OnceLock<Arc<crate::kotlin_scope::KotlinFileScope>>,
    kotlin_brace_frames: OnceLock<Arc<Vec<crate::kotlin_scope::BraceFrame>>>,
    go_scope_index: OnceLock<Arc<crate::go_scope::GoScopeIndex>>,
    scala_imports: OnceLock<Arc<crate::scala_scope::ScalaImports>>,
    scala_holder_package: OnceLock<Option<String>>,
    scala_package_object_package: OnceLock<Option<String>>,
    csharp_namespace_scope: OnceLock<Arc<crate::csharp_types::CsharpNamespaceScope>>,
    dart_code: OnceLock<Arc<crate::dart_chains::DartCode>>,
    dart_local_scopes: OnceLock<Arc<crate::dart_local_scope::DartLocalScopes>>,
    erlang_imports: OnceLock<Arc<Vec<crate::erlang_imports::ErlangImport>>>,
}

impl SourceFacts {
    /// Facts over `source`; nothing is derived until first asked for.
    pub fn new(source: Arc<str>) -> Self {
        Self {
            source,
            lines: OnceLock::new(),
            ts_comment_free: OnceLock::new(),
            python_comment_free: OnceLock::new(),
            ts_blanked: OnceLock::new(),
            js_sealed_module: OnceLock::new(),
            js_module_exports: OnceLock::new(),
            js_binding_sites: OnceLock::new(),
            js_local_bindings: Mutex::new(HashMap::new()),
            node_decisions: Mutex::new(HashMap::new()),
            ts_field_declarations: Mutex::new(HashMap::new()),
            js_get_state_file: OnceLock::new(),
            js_selector_names: OnceLock::new(),
            literal_properties: Mutex::new(HashMap::new()),
            literal_binding_targets: Mutex::new(HashMap::new()),
            awaited_raw_names: OnceLock::new(),
            awaited_index: OnceLock::new(),
            rust_use_scopes: OnceLock::new(),
            rust_uses: OnceLock::new(),
            python_from_imports: OnceLock::new(),
            python_module_symbols: Mutex::new(HashMap::new()),
            python_file_scan: OnceLock::new(),
            python_name_scans: Mutex::new(HashMap::new()),
            python_globals: Mutex::new(HashMap::new()),
            python_plugged_modules: OnceLock::new(),
            python_fixture_types: Mutex::new(HashMap::new()),
            minified_script: OnceLock::new(),
            csharp_scopes: OnceLock::new(),
            sfc_script_blocks: OnceLock::new(),
            js_destructured_calls: OnceLock::new(),
            js_object_scan: OnceLock::new(),
            cpp_local_alias_lines: Mutex::new(HashMap::new()),
            cpp_template_parameters: Mutex::new(HashMap::new()),
            cpp_class_bases: Mutex::new(HashMap::new()),
            cpp_usings: OnceLock::new(),
            java_file_scope: OnceLock::new(),
            java_heads: Mutex::new(HashMap::new()),
            kotlin_file_scope: OnceLock::new(),
            kotlin_brace_frames: OnceLock::new(),
            go_scope_index: OnceLock::new(),
            scala_imports: OnceLock::new(),
            scala_holder_package: OnceLock::new(),
            scala_package_object_package: OnceLock::new(),
            csharp_namespace_scope: OnceLock::new(),
            dart_code: OnceLock::new(),
            dart_local_scopes: OnceLock::new(),
            erlang_imports: OnceLock::new(),
        }
    }

    /// The raw source text.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The file's Dart code with comments and string contents blanked, read
    /// once (W4-DART-03, upstream #2376 `dartCodeOf`).
    pub(crate) fn dart_code(&self) -> Arc<crate::dart_chains::DartCode> {
        Arc::clone(
            self.dart_code
                .get_or_init(|| Arc::new(crate::dart_chains::DartCode::of(&self.source))),
        )
    }

    /// The parameters, loop variables and locals the Dart file binds, read
    /// once (W4-DART-03c, upstream #2379 `fileScopes`).
    pub(crate) fn dart_local_scopes(&self) -> Arc<crate::dart_local_scope::DartLocalScopes> {
        Arc::clone(self.dart_local_scopes.get_or_init(|| {
            Arc::new(crate::dart_local_scope::DartLocalScopes::of(
                &self.dart_code().text,
            ))
        }))
    }

    /// The `-import` attributes of the Erlang file, read once (W4-ERL-01,
    /// upstream #2178 `erlangImportedModule`).
    pub(crate) fn erlang_imports(&self) -> Arc<Vec<crate::erlang_imports::ErlangImport>> {
        Arc::clone(
            self.erlang_imports
                .get_or_init(|| Arc::new(crate::erlang_imports::erlang_imports_of(&self.source))),
        )
    }

    /// Whether a Rust `use` item visible at byte `offset` names `name` or is a
    /// glob (KEEP-RUST, P15 R4); see [`crate::rust_lexical::RustUseScopes`]. The
    /// file's `use` items are scanned once, on first ask.
    pub(crate) fn rust_use_in_scope(&self, offset: usize, name: &str) -> bool {
        self.rust_use_scopes().covers(offset, name)
    }

    fn rust_use_scopes(&self) -> &crate::rust_lexical::RustUseScopes {
        self.rust_use_scopes
            .get_or_init(|| crate::rust_lexical::RustUseScopes::scan(&self.source))
    }

    /// What the file's `use` items bring in, outside crates told apart by
    /// `crates` (upstream #2214 `rustUsesOf`), read once per crate set.
    pub(crate) fn rust_uses(
        &self,
        crates: &Arc<crate::rust_scope::RustCrates>,
    ) -> Arc<crate::rust_scope::RustUses> {
        let read = || {
            Arc::new(crate::rust_scope::RustUses::read(
                self.rust_use_scopes(),
                crates,
            ))
        };
        let (cached_crates, uses) = self.rust_uses.get_or_init(|| (Arc::clone(crates), read()));
        if Arc::ptr_eq(cached_crates, crates) || cached_crates == crates {
            Arc::clone(uses)
        } else {
            read()
        }
    }

    /// Line index of the raw source.
    pub fn lines(&self) -> &LineIndex {
        self.lines.get_or_init(|| LineIndex::new(&self.source))
    }

    /// Number of lines [`str::lines`] yields for the raw source.
    pub fn line_count(&self) -> usize {
        self.lines().line_count()
    }

    /// Raw source line `index`, as [`str::lines`] yields it.
    pub fn line(&self, index: usize) -> &str {
        self.lines().line(&self.source, index)
    }

    /// Segment `index` of `source.split('\n')`.
    pub fn raw_line(&self, index: usize) -> Option<&str> {
        self.lines().raw_line(&self.source, index)
    }

    /// `source.lines()[start..end].join(separator)`.
    pub fn join_lines(&self, start: usize, end: usize, separator: &str) -> String {
        self.lines().join(&self.source, start, end, separator)
    }

    /// The whole file through [`strip_comments_for_regex`] with the
    /// TypeScript rules (shared by every JS-family language), plus its line
    /// index. Comment bytes become spaces; string contents are kept.
    pub fn ts_comment_free(&self) -> (&str, &LineIndex) {
        let (code, lines) = self.ts_comment_free.get_or_init(|| {
            let code = strip_comments_for_regex(&self.source, CommentLang::TypeScript);
            let lines = LineIndex::new(&code);
            (code, lines)
        });
        (code, lines)
    }

    /// The whole file through [`strip_comments_for_regex`] with the Python
    /// rules, plus its line index.
    pub fn python_comment_free(&self) -> (&str, &LineIndex) {
        let (code, lines) = self.python_comment_free.get_or_init(|| {
            let code = strip_comments_for_regex(&self.source, CommentLang::Python);
            let lines = LineIndex::new(&code);
            (code, lines)
        });
        (code, lines)
    }

    /// [`Self::ts_comment_free`] with every string and template literal's
    /// contents blanked too (`blankStringContents(stripCommentsForRegex(…))`),
    /// so line-anchored patterns see only code. The comment-free text it is
    /// made from is kept only when [`Self::ts_comment_free`] was asked for.
    pub fn ts_blanked(&self) -> &str {
        self.ts_blanked.get_or_init(|| self.make_ts_blanked())
    }

    /// `read` over [`Self::ts_blanked`]'s text without keeping it: the text
    /// another reader kept, else one made for `read` and dropped after. For
    /// a fact read once per file (whether it is an ES module, what it
    /// exports), so a file that answers it only for other files' references
    /// keeps no copy of itself.
    pub fn with_ts_blanked<T>(&self, read: impl FnOnce(&str) -> T) -> T {
        match self.ts_blanked.get() {
            Some(code) => read(code),
            None => read(&self.make_ts_blanked()),
        }
    }

    fn make_ts_blanked(&self) -> String {
        match self.ts_comment_free.get() {
            Some((code, _)) => blank_string_contents(code),
            None => blank_string_contents(&strip_comments_for_regex(
                &self.source,
                CommentLang::TypeScript,
            )),
        }
    }

    /// Where a JS/TS local binding can start in the raw source.
    pub fn js_binding_sites(&self) -> &LocalBindingSites {
        self.js_binding_sites.get_or_init(|| {
            let source: &str = &self.source;
            LocalBindingSites {
                var_decls: keyword_sites(source, &["const", "let", "var"]),
                fn_decls: keyword_sites(source, &["function", "class"]),
                arrows: source.match_indices("=>").map(|(at, _)| at).collect(),
            }
        })
    }

    /// Memoised sealed-module decision for this file; `compute` runs once.
    pub fn js_sealed_module(&self, compute: impl FnOnce(&Self) -> bool) -> bool {
        *self.js_sealed_module.get_or_init(|| compute(self))
    }

    /// The names this ES module's export lists name, or `None` when it is no
    /// ES module (upstream #2162); `read` runs once.
    pub fn js_module_exports(
        &self,
        read: impl FnOnce(&Self) -> Option<HashSet<String>>,
    ) -> Option<&HashSet<String>> {
        self.js_module_exports.get_or_init(|| read(self)).as_ref()
    }

    /// This file's `const { … } = f(…)` statements, or `None` when it has none
    /// (upstream #2163); `read` runs once.
    pub fn js_destructured_calls(
        &self,
        read: impl FnOnce(&Self) -> Option<crate::destructured_calls::DestructuredFile>,
    ) -> Option<&crate::destructured_calls::DestructuredFile> {
        self.js_destructured_calls
            .get_or_init(|| read(self))
            .as_ref()
    }

    /// What the object-literal lookups read from this JS/TS file (upstream
    /// #2363); `read` runs once.
    pub(crate) fn js_object_scan(
        &self,
        read: impl FnOnce(&Self) -> crate::object_owners::JsObjectScan,
    ) -> Arc<crate::object_owners::JsObjectScan> {
        Arc::clone(self.js_object_scan.get_or_init(|| Arc::new(read(self))))
    }

    /// A Svelte or Vue component's `<script>` blocks (upstream #2227); `read`
    /// runs once.
    pub fn sfc_script_blocks(
        &self,
        read: impl FnOnce(&Self) -> Vec<crate::sfc_scripts::ScriptBlock>,
    ) -> &[crate::sfc_scripts::ScriptBlock] {
        self.sfc_script_blocks.get_or_init(|| read(self))
    }

    /// Memoised "is `name` bound locally in this file" decision; `compute`
    /// runs at most once per name.
    pub fn js_local_binding(&self, name: &str, compute: impl FnOnce(&Self) -> bool) -> bool {
        if let Some(&known) = self
            .js_local_bindings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(name)
        {
            return known;
        }
        let bound = compute(self);
        self.js_local_bindings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(name.to_string(), bound);
        bound
    }

    /// Whether the raw source can destructure a store's `getState()`;
    /// `scan` runs once.
    pub(crate) fn js_get_state_file(&self, scan: impl FnOnce(&str) -> bool) -> bool {
        *self.js_get_state_file.get_or_init(|| scan(&self.source))
    }

    /// Names the raw source binds to a selector call; `scan` runs once.
    pub(crate) fn js_selector_names(
        &self,
        scan: impl FnOnce(&str) -> HashSet<String>,
    ) -> &HashSet<String> {
        self.js_selector_names.get_or_init(|| scan(&self.source))
    }

    /// Memoised own property of the object literal `container_id` (defined in
    /// this file) for `member`; `compute` runs at most once per key.
    pub(crate) fn literal_property(
        &self,
        container_id: &str,
        member: &str,
        compute: impl FnOnce(&Self) -> LiteralLookup,
    ) -> LiteralLookup {
        let key = (container_id.to_string(), member.to_string());
        if let Some(known) = self
            .literal_properties
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
        {
            return known.clone();
        }
        let lookup = compute(self);
        self.literal_properties
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, lookup.clone());
        lookup
    }

    /// Memoised target node id of an object-literal member's binding;
    /// `compute` runs at most once per `(container, key)`.
    pub(crate) fn literal_binding_target(
        &self,
        container_id: &str,
        key: &str,
        compute: impl FnOnce(&Self) -> Option<String>,
    ) -> Option<String> {
        let key = (container_id.to_string(), key.to_string());
        if let Some(known) = self
            .literal_binding_targets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
        {
            return known.clone();
        }
        let target = compute(self);
        self.literal_binding_targets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, target.clone());
        target
    }

    /// Names the raw source binds as `const x = await f(`; `scan` runs once.
    pub(crate) fn awaited_raw_names(
        &self,
        scan: impl FnOnce(&str) -> HashSet<String>,
    ) -> &HashSet<String> {
        self.awaited_raw_names.get_or_init(|| scan(&self.source))
    }

    /// What the raw source's `from <module> import <name>` lines bind, name
    /// to module; `scan` runs once.
    pub(crate) fn python_from_imports(
        &self,
        scan: impl FnOnce(&str) -> HashMap<String, String>,
    ) -> &HashMap<String, String> {
        self.python_from_imports.get_or_init(|| scan(&self.source))
    }

    /// Memoised id of the definition this Python module offers as `name`
    /// when asked `depth` modules into a re-export walk; `compute` runs at
    /// most once per `(name, depth)`, without the lock held, so a walk may
    /// come back to this module.
    pub(crate) fn python_module_symbol(
        &self,
        name: &str,
        depth: usize,
        compute: impl FnOnce() -> Option<String>,
    ) -> Option<String> {
        let key = (name.to_string(), depth);
        if let Some(known) = self
            .python_module_symbols
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
        {
            return known.clone();
        }
        let found = compute();
        self.python_module_symbols
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, found.clone());
        found
    }

    /// The statement starts and `__main__` blocks of the Python code, read
    /// once.
    pub(crate) fn python_file_scan(
        &self,
        build: impl FnOnce(&Self) -> crate::python_globals::PythonFileScan,
    ) -> Arc<crate::python_globals::PythonFileScan> {
        Arc::clone(self.python_file_scan.get_or_init(|| Arc::new(build(self))))
    }

    /// The memoised answers this Python file gives about `name`.
    pub(crate) fn python_name_scan(&self, name: &str) -> Arc<crate::python_globals::NameScan> {
        Arc::clone(
            self.python_name_scans
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .entry(name.to_string())
                .or_default(),
        )
    }

    /// The memoised answers about the module global `id` this file declares.
    pub(crate) fn python_global_memo(&self, id: &str) -> Arc<crate::python_globals::GlobalMemo> {
        Arc::clone(
            self.python_globals
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .entry(id.to_string())
                .or_default(),
        )
    }

    /// The fixture modules the `conftest.py` files above this Python file
    /// pull in; `scan` runs once.
    pub(crate) fn python_plugged_modules(&self, scan: impl FnOnce() -> Vec<String>) -> &[String] {
        self.python_plugged_modules.get_or_init(scan)
    }

    /// Memoised class a pytest fixture parameter of this file is (`key`
    /// names the test and the parameter); `compute` runs at most once per
    /// key, without the lock held.
    pub(crate) fn python_fixture_type(
        &self,
        key: &str,
        compute: impl FnOnce() -> Option<String>,
    ) -> Option<String> {
        if let Some(known) = self
            .python_fixture_types
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key)
        {
            return known.clone();
        }
        let found = compute();
        self.python_fixture_types
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key.to_string(), found.clone());
        found
    }

    /// Memoised lines of the C/C++ function `caller_id` (defined in this file)
    /// that declare an alias, last first (upstream #2399); `compute` runs at
    /// most once per function.
    pub(crate) fn cpp_local_alias_lines(
        &self,
        caller_id: &str,
        compute: impl FnOnce(&Self) -> Vec<usize>,
    ) -> Arc<Vec<usize>> {
        keyed_memo(&self.cpp_local_alias_lines, caller_id, || {
            Arc::new(compute(self))
        })
    }

    /// Memoised C++ template parameters in scope where the node `node_id`
    /// (defined in this file) is declared (upstream #2399); `compute` runs at
    /// most once per node.
    pub(crate) fn cpp_template_parameters(
        &self,
        node_id: &str,
        compute: impl FnOnce(&Self) -> BTreeSet<String>,
    ) -> Arc<BTreeSet<String>> {
        keyed_memo(&self.cpp_template_parameters, node_id, || {
            Arc::new(compute(self))
        })
    }

    /// Memoised bases the head of the C++ class declaration `class_id`
    /// (defined in this file) names (upstream #2399); `compute` runs at most
    /// once per declaration.
    pub(crate) fn cpp_class_bases(
        &self,
        class_id: &str,
        compute: impl FnOnce(&Self) -> Vec<String>,
    ) -> Arc<Vec<String>> {
        keyed_memo(&self.cpp_class_bases, class_id, || Arc::new(compute(self)))
    }

    /// The C/C++ `using` declarations, directives and namespace aliases of the
    /// raw source (upstream #2407); `read` runs once.
    pub(crate) fn cpp_usings(
        &self,
        read: impl FnOnce(&str) -> Vec<crate::cpp_supertypes::CppUsing>,
    ) -> Arc<Vec<crate::cpp_supertypes::CppUsing>> {
        Arc::clone(self.cpp_usings.get_or_init(|| Arc::new(read(&self.source))))
    }

    /// The package and imports of a Java file (upstream #2126, #2236), read
    /// once.
    pub(crate) fn java_file_scope(&self) -> Arc<crate::java_scope::JavaFileScope> {
        Arc::clone(
            self.java_file_scope
                .get_or_init(|| Arc::new(crate::java_scope::JavaFileScope::read(&self.source))),
        )
    }

    /// Memoised supertype names the head of the Java type declaration
    /// `declaration_id` (defined in this file) writes (upstream #2126,
    /// #2236); `read` runs at most once per declaration.
    pub(crate) fn java_head(
        &self,
        declaration_id: &str,
        read: impl FnOnce(&Self) -> crate::java_scope::JavaHead,
    ) -> Arc<crate::java_scope::JavaHead> {
        keyed_memo(&self.java_heads, declaration_id, || Arc::new(read(self)))
    }

    /// What a Scala file's imports bring in (upstream `scalaImportsOf`),
    /// read once.
    pub(crate) fn scala_imports(&self) -> Arc<crate::scala_scope::ScalaImports> {
        Arc::clone(
            self.scala_imports
                .get_or_init(|| Arc::new(crate::scala_scope::ScalaImports::read(&self.source))),
        )
    }

    /// The last segment of a Scala file's last package clause (upstream
    /// #2199), read once.
    pub(crate) fn scala_holder_package(&self) -> Option<String> {
        self.scala_holder_package
            .get_or_init(|| crate::scala_scope::scala_holder_package(&self.source))
            .clone()
    }

    /// The package a Scala file's `package object` opens (upstream #2230),
    /// read once.
    pub(crate) fn scala_package_object_package(&self) -> Option<String> {
        self.scala_package_object_package
            .get_or_init(|| crate::scala_scope::scala_package_object_package(&self.source))
            .clone()
    }

    /// The namespaces a C# file runs in and imports (upstream #2216
    /// `csharpNamespaceScope`), computed once.
    pub(crate) fn csharp_namespace_scope(
        &self,
        compute: impl FnOnce() -> crate::csharp_types::CsharpNamespaceScope,
    ) -> Arc<crate::csharp_types::CsharpNamespaceScope> {
        Arc::clone(
            self.csharp_namespace_scope
                .get_or_init(|| Arc::new(compute())),
        )
    }

    /// The package and imports of a Kotlin file (upstream #2146), read once.
    pub(crate) fn kotlin_file_scope(&self) -> Arc<crate::kotlin_scope::KotlinFileScope> {
        Arc::clone(
            self.kotlin_file_scope
                .get_or_init(|| Arc::new(crate::kotlin_scope::KotlinFileScope::read(&self.source))),
        )
    }

    /// The type bodies of a Kotlin file read from its braces (upstream
    /// #2172 `kotlinBraceFrames`); `read` runs once. A lambda's frame reads
    /// the receiver its function's declaration names in another file, which
    /// a pass does not change either.
    pub(crate) fn kotlin_brace_frames(
        &self,
        read: impl FnOnce(&str) -> Vec<crate::kotlin_scope::BraceFrame>,
    ) -> Arc<Vec<crate::kotlin_scope::BraceFrame>> {
        Arc::clone(
            self.kotlin_brace_frames
                .get_or_init(|| Arc::new(read(&self.source))),
        )
    }

    /// The blocks and the parameter and local declarations of a Go file
    /// (upstream #2448 `goScopeIndex`), read once.
    pub(crate) fn go_scope_index(&self) -> Arc<crate::go_scope::GoScopeIndex> {
        Arc::clone(
            self.go_scope_index
                .get_or_init(|| Arc::new(crate::go_scope::GoScopeIndex::read(&self.source))),
        )
    }

    /// The C# namespace blocks and `using` directives of the raw source,
    /// read once.
    pub(crate) fn csharp_scopes(&self) -> Arc<crate::csharp_scopes::CsharpScopes> {
        Arc::clone(
            self.csharp_scopes
                .get_or_init(|| Arc::new(crate::csharp_scopes::CsharpScopes::read(&self.source))),
        )
    }

    /// Whether the raw source is a minified or bundled script; `detect` runs
    /// once.
    pub(crate) fn minified_script(&self, detect: impl FnOnce(&str) -> bool) -> bool {
        *self.minified_script.get_or_init(|| detect(&self.source))
    }

    /// The awaited-binding index over [`Self::ts_blanked`]; `build` runs once.
    pub(crate) fn awaited_index(
        &self,
        build: impl FnOnce(&str) -> Arc<AwaitedIndex>,
    ) -> Arc<AwaitedIndex> {
        Arc::clone(self.awaited_index.get_or_init(|| build(self.ts_blanked())))
    }

    /// Memoised per-node decision (`kind` names the gate, `node_id` the
    /// candidate defined in this file); `compute` runs at most once per key.
    pub fn node_decision(
        &self,
        kind: &'static str,
        node_id: &str,
        compute: impl FnOnce(&Self) -> bool,
    ) -> bool {
        let key = (kind, node_id.to_string());
        if let Some(&known) = self
            .node_decisions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
        {
            return known;
        }
        let decision = compute(self);
        self.node_decisions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, decision);
        decision
    }

    /// Memoised declaration of `field` on the class `owner_id` defined in this
    /// file; `compute` runs at most once per `(owner, field)`.
    pub fn ts_field_declaration(
        &self,
        owner_id: &str,
        field: &str,
        compute: impl FnOnce(&Self) -> Option<TsFieldDeclaration>,
    ) -> Option<TsFieldDeclaration> {
        let key = (owner_id.to_string(), field.to_string());
        if let Some(known) = self
            .ts_field_declarations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
        {
            return known.clone();
        }
        let declaration = compute(self);
        self.ts_field_declarations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, declaration.clone());
        declaration
    }
}

/// `map[key]`, computed by `compute` without holding the lock when absent.
fn keyed_memo<V: Clone>(
    map: &Mutex<HashMap<String, V>>,
    key: &str,
    compute: impl FnOnce() -> V,
) -> V {
    if let Some(known) = map
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(key)
    {
        return known.clone();
    }
    let value = compute();
    map.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key.to_string(), value.clone());
    value
}

/// Offsets of `\b(?:kw1|kw2|…)\s` matches: a keyword preceded by a non-word
/// character (or the start) and followed by whitespace. `\b`/`\w` are ASCII,
/// as in upstream's JavaScript regexes.
fn keyword_sites(source: &str, keywords: &[&str]) -> Vec<usize> {
    let bytes = source.as_bytes();
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let mut sites = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if (at == 0 || !is_word(bytes[at - 1]))
            && let Some(keyword) = keywords.iter().find(|keyword| {
                bytes[at..].starts_with(keyword.as_bytes())
                    && source[at + keyword.len()..]
                        .chars()
                        .next()
                        .is_some_and(char::is_whitespace)
            })
        {
            sites.push(at);
            at += keyword.len();
            continue;
        }
        at += 1;
    }
    sites
}

#[cfg(test)]
mod tests {
    use super::*;

    fn std_lines(text: &str) -> Vec<&str> {
        text.lines().collect()
    }

    #[test]
    fn line_index_matches_str_lines_for_every_terminator_shape() {
        for text in [
            "",
            "a",
            "a\n",
            "a\nb",
            "a\nb\n",
            "\n",
            "\n\n",
            "a\r\nb\r\n",
            "a\r\nb\r",
            "a\rb\n",
            "\r\n",
            "x\n\ny\r\n\r\nz",
            "é\nü\r\n中",
        ] {
            let index = LineIndex::new(text);
            let expected = std_lines(text);
            assert_eq!(index.line_count(), expected.len(), "count for {text:?}");
            for (i, line) in expected.iter().enumerate() {
                assert_eq!(index.line(text, i), *line, "line {i} of {text:?}");
            }
            for start in 0..=expected.len() {
                for end in start..=expected.len() {
                    assert_eq!(
                        index.join(text, start, end, "\n"),
                        expected[start..end].join("\n"),
                        "join {start}..{end} of {text:?}"
                    );
                }
            }
            let split: Vec<&str> = text.split('\n').collect();
            for (i, segment) in split.iter().enumerate() {
                assert_eq!(
                    index.raw_line(text, i),
                    Some(*segment),
                    "raw {i} of {text:?}"
                );
            }
            assert_eq!(index.raw_line(text, split.len()), None);
        }
    }

    #[test]
    fn raw_line_start_counts_every_newline() {
        let text = "ab\r\ncd\n\nef";
        let index = LineIndex::new(text);
        assert_eq!(index.raw_line_start(0), Some(0));
        assert_eq!(index.raw_line_start(1), Some(4));
        assert_eq!(index.raw_line_start(2), Some(7));
        assert_eq!(index.raw_line_start(3), Some(8));
        assert_eq!(index.raw_line_start(4), None);
    }

    #[test]
    fn keyword_sites_require_ascii_boundary_and_trailing_space() {
        let source = "const a = 1; xconst b; let\tc; var(d); function f() {} class C {}";
        let facts = SourceFacts::new(Arc::from(source));
        let sites = facts.js_binding_sites();
        let at = |needle: &str| source.find(needle).unwrap();
        assert_eq!(sites.var_decls, vec![at("const a"), at("let\t")]);
        assert_eq!(sites.fn_decls, vec![at("function"), at("class")]);
        assert_eq!(sites.arrows, Vec::<usize>::new());
    }

    #[test]
    fn local_binding_decision_is_computed_once_per_name() {
        let facts = SourceFacts::new(Arc::from("const a = 1; // b\n"));
        let mut runs = 0;
        assert!(facts.js_local_binding("a", |facts| {
            runs += 1;
            facts.source().contains("const a")
        }));
        assert!(facts.js_local_binding("a", |_| unreachable!("memoised")));
        assert!(!facts.js_local_binding("b", |facts| {
            runs += 1;
            facts.source().contains("const b")
        }));
        assert_eq!(runs, 2);
    }

    #[test]
    fn sealed_and_node_decisions_are_computed_once() {
        let facts = SourceFacts::new(Arc::from("import x from 'y';\n"));
        assert!(facts.js_sealed_module(|_| true));
        assert!(facts.js_sealed_module(|_| unreachable!("memoised")));
        assert!(facts.node_decision("c_static", "function:1", |_| true));
        assert!(facts.node_decision("c_static", "function:1", |_| unreachable!("memoised")));
        assert!(!facts.node_decision("rust_trait", "function:1", |_| false));
    }

    #[test]
    fn ts_field_declarations_are_computed_once_per_owner_and_field() {
        let facts = SourceFacts::new(Arc::from("class A { #m = new M(); }\n"));
        let declared = TsFieldDeclaration {
            value_type: false,
            type_name: "M".to_string(),
        };
        let mut runs = 0;
        let mut compute = |_: &SourceFacts| {
            runs += 1;
            Some(declared.clone())
        };
        assert_eq!(
            facts.ts_field_declaration("class:1", "#m", &mut compute),
            Some(declared.clone())
        );
        assert_eq!(
            facts.ts_field_declaration("class:1", "#m", |_| unreachable!("memoised")),
            Some(declared)
        );
        assert_eq!(facts.ts_field_declaration("class:1", "m", |_| None), None);
        assert_eq!(
            facts.ts_field_declaration("class:1", "m", |_| unreachable!("memoised")),
            None
        );
        assert_eq!(runs, 1);
    }

    /// A module fact read from a transient blanked copy reads the text
    /// [`SourceFacts::ts_blanked`] keeps, whichever comes first.
    #[test]
    fn with_ts_blanked_reads_the_text_ts_blanked_keeps() {
        let source = "import a from 'x'; // export b\n# c\nconst s = `t ${'u'}`; /* export d */\n";
        let transient = SourceFacts::new(Arc::from(source));
        let read = transient.with_ts_blanked(str::to_string);
        let kept = SourceFacts::new(Arc::from(source));
        assert_eq!(read, kept.ts_blanked());
        assert_eq!(kept.with_ts_blanked(str::to_string), kept.ts_blanked());
        let comment_free_first = SourceFacts::new(Arc::from(source));
        let _ = comment_free_first.ts_comment_free();
        assert_eq!(comment_free_first.with_ts_blanked(str::to_string), read);
    }

    #[test]
    fn ts_blanked_hides_template_imports() {
        let facts = SourceFacts::new(Arc::from(
            "const t = `\nimport fake from \"fake\";\n`;\n// import x\n",
        ));
        assert!(!facts.ts_blanked().contains("import"));
        assert_eq!(facts.ts_blanked().matches('\n').count(), 4);
    }
}
