//! C and C++ namespaces the index cannot see (upstream #2176, #2201, #2445).
//!
//! A library that opens its namespace with a macro (fmt's
//! `FMT_BEGIN_NAMESPACE`, `#define FMT_BEGIN_NAMESPACE namespace fmt { inline
//! namespace v12 {`; pybind11's `PYBIND11_NAMESPACE_BEGIN(PYBIND11_NAMESPACE)`)
//! shows the extractor a bare macro line, so every declaration inside is
//! indexed without that namespace and `fmt::format(…)` finds nothing. The
//! project's namespace macros are read from its headers: object-like and
//! single-parameter function-like openers, through identifier macro aliases
//! (`#define RAPIDJSON_NAMESPACE rapidjson`), inline namespaces transparent,
//! and the closers. With them, each file's line ranges a macro opens
//! ([`Frame`]), and the project's namespace aliases (`namespace py =
//! pybind11;`), a qualified name reaches the declaration whose macro prefix
//! plus qualified name is the name written, as the last strategy for a
//! qualified C/C++ name ([`match_cpp_macro_namespaced`]). Among an overload
//! set the call's arguments pick the one they fit (#2201).
//!
//! A name written from the global scope (`::store::Repair(…)`, `::memset`)
//! is the declaration of exactly that qualified name, never a namesake
//! nested in another namespace or class, nor one inside a namespace a macro
//! opens, which only looks global in the index ([`match_cpp_global_name`],
//! #2445).
//!
//! The same facts are what a sync compares to find who must be re-resolved
//! (W4-CPP-07): [`facts_of`] reads one file's aliases and macro definitions
//! as written, [`CppNamespaceTables::usage_lines`] the lines that use a known
//! opener or closer, and [`NamespaceRow`] keeps both per file in
//! `project_metadata`. When the same alias or macro is defined in several
//! files the first by path wins, and within a file the first definition
//! (upstream keeps the last macro alias and closer).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::name_matcher::prefer_call_site_file;
use crate::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// `project_metadata` key prefix of a file's namespace row.
pub const ROW_KEY_PREFIX: &str = "cpp_namespace_surface:";

/// The `project_metadata` key of `file_path`'s namespace row.
pub fn row_key(file_path: &str) -> String {
    format!("{ROW_KEY_PREFIX}{file_path}")
}

fn has_extension(path: &str, extensions: &[&str]) -> bool {
    path.rsplit_once('.').is_some_and(|(stem, extension)| {
        !stem.is_empty()
            && !extension.contains('/')
            && extensions
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
    })
}

/// A file whose namespace aliases the project table reads.
pub fn is_alias_file(path: &str) -> bool {
    has_extension(
        path,
        &["h", "hh", "hpp", "hxx", "inl", "c", "cc", "cpp", "cxx"],
    )
}

/// A header whose `#define`s the namespace macro table reads.
pub fn is_macro_file(path: &str) -> bool {
    has_extension(path, &["h", "hh", "hpp", "hxx", "h++", "inl", "ipp", "tcc"])
}

/// One `#define` that opens, closes or names a namespace.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MacroFact {
    /// `#define FMT_BEGIN_NAMESPACE namespace fmt { inline namespace v12 {`,
    /// the body as written.
    Opener { name: String, body: String },
    /// `#define PYBIND11_NAMESPACE_BEGIN(name) namespace name {`.
    OpenerFn { name: String },
    /// `#define FMT_END_NAMESPACE } }`, with the scopes its body closes.
    Closer { name: String, scopes: usize },
    /// `#define RAPIDJSON_NAMESPACE rapidjson`.
    Alias { name: String, target: String },
}

impl MacroFact {
    /// The macro's name.
    pub fn name(&self) -> &str {
        match self {
            Self::Opener { name, .. }
            | Self::OpenerFn { name }
            | Self::Closer { name, .. }
            | Self::Alias { name, .. } => name,
        }
    }

    /// The text its definition writes besides the name.
    pub fn body(&self) -> &str {
        match self {
            Self::Opener { body, .. } => body,
            Self::Alias { target, .. } => target,
            Self::OpenerFn { .. } | Self::Closer { .. } => "",
        }
    }
}

/// What one C or C++ file writes that the namespace tables read: its
/// namespace aliases and its namespace macro definitions, in file order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CppNamespaceFacts {
    /// `namespace py = pybind11;` as `(py, pybind11)`; a leading `::` of the
    /// target dropped.
    pub aliases: Vec<(String, String)>,
    pub macros: Vec<MacroFact>,
}

impl CppNamespaceFacts {
    pub fn is_empty(&self) -> bool {
        self.aliases.is_empty() && self.macros.is_empty()
    }
}

fn regex(pattern: &'static str, cell: &'static OnceLock<Regex>) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("namespace regex"))
}

/// A file's namespace aliases: `namespace py = pybind11;` at the start of a
/// line.
fn aliases_in(source: &str) -> Vec<(String, String)> {
    static ALIAS: OnceLock<Regex> = OnceLock::new();
    let alias = regex(
        r"(?m)^[ \t]*namespace[ \t]+([A-Za-z_][0-9A-Za-z_]*)[ \t]*=[ \t]*(?:::)?([A-Za-z_][0-9A-Za-z_:]*)[ \t]*;",
        &ALIAS,
    );
    if !source.contains("namespace") {
        return Vec::new();
    }
    alias
        .captures_iter(source)
        .map(|found| (found[1].to_string(), found[2].to_string()))
        .collect()
}

/// A closing macro's body: `}` / `} }`, maybe beside a pragma macro
/// (`PYBIND11_WARNING_POP }`).
fn is_closer_body(body: &str) -> bool {
    static CLOSER: OnceLock<Regex> = OnceLock::new();
    regex(
        r"^(?:[A-Za-z_][0-9A-Za-z_]*\s+)*\}(?:\s*\})*\s*;?$",
        &CLOSER,
    )
    .is_match(body)
}

/// A header's namespace macro definitions.
fn macros_in(source: &str) -> Vec<MacroFact> {
    static DEFINE: OnceLock<Regex> = OnceLock::new();
    static IDENTIFIER: OnceLock<Regex> = OnceLock::new();
    static OPENER: OnceLock<Regex> = OnceLock::new();
    if !(source.contains('#') && source.contains("define")) {
        return Vec::new();
    }
    let define = regex(
        r"(?m)^[ \t]*#[ \t]*define[ \t]+([A-Za-z_][0-9A-Za-z_]*)(\(\s*([A-Za-z_][0-9A-Za-z_]*)?\s*\))?[ \t]+([^\n]*)$",
        &DEFINE,
    );
    let identifier = regex(r"^[A-Za-z_][0-9A-Za-z_]*$", &IDENTIFIER);
    // An inline namespace (transparent, and often named by a macro call)
    // may sit among the namespaces it opens.
    let opener = regex(
        r"^(?:inline\s+namespace\s+[^{}]*\{\s*|namespace\s+[A-Za-z_][0-9A-Za-z_]*\s*\{\s*)+[0-9A-Za-z_\s]*$",
        &OPENER,
    );
    let spliced = source.replace("\\\r\n", " ").replace("\\\n", " ");
    let text = strip_comments_for_regex(&spliced, CommentLang::Cpp);
    let mut out = Vec::new();
    for found in define.captures_iter(&text) {
        let name = found[1].to_string();
        let body = found[4].trim();
        let closes = || body.matches('}').count();
        if found.get(2).is_some() {
            // (a trailing pragma macro, `PYBIND11_WARNING_PUSH`, rides along)
            let opens_its_parameter = found.get(3).is_some_and(|parameter| {
                Regex::new(&format!(
                    r"^namespace\s+{}\s*\{{[0-9A-Za-z_\s]*$",
                    regex::escape(parameter.as_str())
                ))
                .is_ok_and(|opens| opens.is_match(body))
            });
            if opens_its_parameter {
                out.push(MacroFact::OpenerFn { name });
            } else if is_closer_body(body) {
                out.push(MacroFact::Closer {
                    name,
                    scopes: closes(),
                });
            }
            continue;
        }
        if identifier.is_match(body) {
            out.push(MacroFact::Alias {
                name,
                target: body.to_string(),
            });
        } else if is_closer_body(body) {
            out.push(MacroFact::Closer {
                name,
                scopes: closes(),
            });
        } else if opener.is_match(body) {
            out.push(MacroFact::Opener {
                name,
                body: body.to_string(),
            });
        }
    }
    out
}

/// What `path`, whose text is `source`, contributes to the namespace tables.
pub fn facts_of(path: &str, source: &str) -> CppNamespaceFacts {
    CppNamespaceFacts {
        aliases: if is_alias_file(path) {
            aliases_in(source)
        } else {
            Vec::new()
        },
        macros: if is_macro_file(path) {
            macros_in(source)
        } else {
            Vec::new()
        },
    }
}

/// The project's namespace macros.
#[derive(Debug, Default)]
pub struct NamespaceMacros {
    /// Each object-like opener and the namespaces it opens, macro aliases
    /// followed, inline namespaces skipped.
    openers: HashMap<String, Vec<String>>,
    opener_fns: HashSet<String>,
    closers: HashSet<String>,
    aliases: HashMap<String, String>,
}

impl NamespaceMacros {
    /// The macros the facts of every file define, in path order.
    pub fn from_facts<'f>(files: impl IntoIterator<Item = &'f CppNamespaceFacts>) -> Self {
        static NAMESPACE: OnceLock<Regex> = OnceLock::new();
        static INLINE: OnceLock<Regex> = OnceLock::new();
        let namespace = regex(r"namespace\s+([A-Za-z_][0-9A-Za-z_]*)", &NAMESPACE);
        let inline = regex(r"inline\s+namespace\s+[^{}]*\{", &INLINE);
        let mut macros = Self::default();
        let mut bodies: Vec<(&str, &str)> = Vec::new();
        for facts in files {
            for fact in &facts.macros {
                match fact {
                    MacroFact::Opener { name, body } => bodies.push((name, body)),
                    MacroFact::OpenerFn { name } => {
                        macros.opener_fns.insert(name.clone());
                    }
                    MacroFact::Closer { name, .. } => {
                        macros.closers.insert(name.clone());
                    }
                    MacroFact::Alias { name, target } => {
                        macros
                            .aliases
                            .entry(name.clone())
                            .or_insert_with(|| target.clone());
                    }
                }
            }
        }
        for (name, body) in bodies {
            if macros.openers.contains_key(name) {
                continue;
            }
            let path: Vec<String> = namespace
                .captures_iter(&inline.replace_all(body, ""))
                .map(|found| {
                    let written = &found[1];
                    macros
                        .aliases
                        .get(written)
                        .cloned()
                        .unwrap_or_else(|| written.to_string())
                })
                .collect();
            if !path.is_empty() {
                macros.openers.insert(name.to_string(), path);
            }
        }
        macros
    }

    fn knows(&self, token: &str) -> bool {
        self.openers.contains_key(token)
            || self.opener_fns.contains(token)
            || self.closers.contains(token)
    }

    /// The lines of `candidates` ([`candidate_usage_lines`]) that use one of
    /// these openers or closers; none while no macro opens a namespace.
    pub fn known_usage(&self, candidates: &[UsageLine]) -> Vec<UsageLine> {
        if self.openers.is_empty() && self.opener_fns.is_empty() {
            return Vec::new();
        }
        candidates
            .iter()
            .filter(|line| self.knows(&line.token))
            .cloned()
            .collect()
    }

    /// The lines of `lines` that use one of these openers or closers on
    /// their own.
    fn usage_lines<'l>(&self, lines: impl Iterator<Item = &'l str>) -> Vec<UsageLine> {
        if self.openers.is_empty() && self.opener_fns.is_empty() {
            return Vec::new();
        }
        self.known_usage(&candidate_usage_lines(lines))
    }

    /// The namespaces a use of `token` (with `arg`) opens, if it opens any.
    pub fn opened_path(&self, token: &str, arg: Option<&str>) -> Option<Vec<String>> {
        match arg {
            Some(arg) if self.opener_fns.contains(token) => Some(vec![
                self.aliases
                    .get(arg)
                    .cloned()
                    .unwrap_or_else(|| arg.to_string()),
            ]),
            Some(_) => None,
            None => self.openers.get(token).cloned(),
        }
    }

    /// The frames `usage` opens: an opener opens a frame, a closer closes
    /// the innermost open one, and a frame the file never closes runs to its
    /// end.
    pub fn frames_of(&self, usage: &[UsageLine]) -> Vec<Frame> {
        let mut frames = Vec::new();
        let mut open: Vec<(i64, Vec<String>)> = Vec::new();
        for line in usage {
            if let Some(path) = self.opened_path(&line.token, line.arg.as_deref()) {
                open.push((line.line, path));
            } else if self.closers.contains(&line.token)
                && let Some((start, path)) = open.pop()
            {
                frames.push(Frame {
                    start,
                    end: line.line,
                    path,
                });
            }
        }
        frames.extend(open.into_iter().map(|(start, path)| Frame {
            start,
            end: i64::MAX,
            path,
        }));
        frames
    }
}

/// Every line of `lines` that uses an all-caps macro on its own, with at
/// most one identifier argument (`FMT_BEGIN_NAMESPACE`,
/// `PYBIND11_NAMESPACE_BEGIN(detail)`, a trailing `;` or comment allowed):
/// the lines a namespace opener or closer can be written on.
pub fn candidate_usage_lines<'l>(lines: impl Iterator<Item = &'l str>) -> Vec<UsageLine> {
    static USE: OnceLock<Regex> = OnceLock::new();
    let use_line = regex(
        r"^[ \t]*([A-Z_][A-Z0-9_]*)(?:\(\s*([A-Za-z_][0-9A-Za-z_]*)?\s*\))?[ \t]*;?[ \t]*(?://.*|/\*.*\*/[ \t]*)?\r?$",
        &USE,
    );
    let mut out = Vec::new();
    for (index, text) in lines.enumerate() {
        if let Some(found) = use_line.captures(text) {
            out.push(UsageLine {
                line: index as i64 + 1,
                token: found[1].to_string(),
                arg: found.get(2).map(|arg| arg.as_str().to_string()),
            });
        }
    }
    out
}

/// A line that uses a namespace opener or closer on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageLine {
    /// 1-based.
    pub line: i64,
    pub token: String,
    /// The argument of a function-like use (`PYBIND11_NAMESPACE_BEGIN(detail)`).
    pub arg: Option<String>,
}

/// The lines of a file a namespace macro opens, and the namespaces it opens
/// there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// 1-based, the opening line.
    pub start: i64,
    /// 1-based, the closing line; `i64::MAX` when the file never closes it.
    pub end: i64,
    pub path: Vec<String>,
}

impl Frame {
    fn holds(&self, line: i64) -> bool {
        self.start <= line && self.end >= line
    }
}

/// The project's namespace aliases and namespace macros, read on first use
/// (or from a sync's rows), with each file's frames memoised.
#[derive(Debug, Default)]
pub struct CppNamespaceTables {
    aliases: OnceLock<HashMap<String, String>>,
    macros: OnceLock<NamespaceMacros>,
    frames: Mutex<HashMap<String, Arc<Vec<Frame>>>>,
}

impl CppNamespaceTables {
    /// Tables read from `context`'s files when first asked.
    pub fn new() -> Self {
        Self::default()
    }

    /// Tables over the facts of every file, in path order.
    pub fn from_facts<'f>(files: impl IntoIterator<Item = &'f CppNamespaceFacts> + Clone) -> Self {
        let mut aliases = HashMap::new();
        for facts in files.clone() {
            for (alias, target) in &facts.aliases {
                aliases
                    .entry(alias.clone())
                    .or_insert_with(|| target.clone());
            }
        }
        let tables = Self::default();
        let _ = tables.aliases.set(aliases);
        let _ = tables.macros.set(NamespaceMacros::from_facts(files));
        tables
    }

    /// Every indexed file of `context` the tables read, in path order.
    fn files(context: &dyn ResolutionContext, wanted: fn(&str) -> bool) -> Vec<String> {
        let mut files: Vec<String> = context
            .get_all_files_shared()
            .iter()
            .filter(|file| wanted(file))
            .cloned()
            .collect();
        files.sort();
        files
    }

    fn aliases(&self, context: &dyn ResolutionContext) -> &HashMap<String, String> {
        self.aliases.get_or_init(|| {
            let mut aliases = HashMap::new();
            for file in Self::files(context, is_alias_file) {
                let Some(source) = context.read_file_shared(&file) else {
                    continue;
                };
                for (alias, target) in aliases_in(&source) {
                    aliases.entry(alias).or_insert(target);
                }
            }
            aliases
        })
    }

    fn macros(&self, context: &dyn ResolutionContext) -> &NamespaceMacros {
        self.macros.get_or_init(|| {
            let facts: Vec<CppNamespaceFacts> = Self::files(context, is_macro_file)
                .iter()
                .filter_map(|file| context.read_file_shared(file))
                .map(|source| CppNamespaceFacts {
                    aliases: Vec::new(),
                    macros: macros_in(&source),
                })
                .collect();
            NamespaceMacros::from_facts(&facts)
        })
    }

    /// The project's namespace alias `name` names (`py` → `pybind11`).
    pub fn namespace_alias(&self, name: &str, context: &dyn ResolutionContext) -> Option<String> {
        self.aliases(context).get(name).cloned()
    }

    /// The lines of `lines` that use a namespace opener or closer on their
    /// own (a trailing `;` or comment allowed).
    pub fn usage_lines<'l>(
        &self,
        lines: impl Iterator<Item = &'l str>,
        context: &dyn ResolutionContext,
    ) -> Vec<UsageLine> {
        self.macros(context).usage_lines(lines)
    }

    /// The frames `usage` opens: see [`NamespaceMacros::frames_of`].
    pub fn frames_of(&self, usage: &[UsageLine], context: &dyn ResolutionContext) -> Vec<Frame> {
        self.macros(context).frames_of(usage)
    }

    /// The frames of `file`.
    pub fn frames(&self, file: &str, context: &dyn ResolutionContext) -> Arc<Vec<Frame>> {
        if let Some(frames) = self.frames.lock().expect("frame memo").get(file).cloned() {
            return frames;
        }
        let frames = Arc::new(match context.source_facts(file) {
            Some(facts) => {
                let usage = self.usage_lines(
                    (0..facts.line_count()).map(|index| facts.line(index)),
                    context,
                );
                self.frames_of(&usage, context)
            }
            None => Vec::new(),
        });
        self.frames
            .lock()
            .expect("frame memo")
            .entry(file.to_string())
            .or_insert(frames)
            .clone()
    }

    /// The namespaces macros open around line `line` of `file`, outermost
    /// first.
    pub fn macro_prefix(
        &self,
        file: &str,
        line: i64,
        context: &dyn ResolutionContext,
    ) -> Vec<String> {
        macro_prefix_in(&self.frames(file, context), line)
    }
}

/// The namespaces `frames` open around `line`, outermost first.
pub fn macro_prefix_in(frames: &[Frame], line: i64) -> Vec<String> {
    let mut holding: Vec<&Frame> = frames.iter().filter(|frame| frame.holds(line)).collect();
    holding.sort_by_key(|frame| frame.start);
    holding
        .into_iter()
        .flat_map(|frame| frame.path.iter().cloned())
        .collect()
}

/// `node`'s qualified name with the namespaces macros open around it
/// (`fmt::detail::buffer` for `detail::buffer`).
pub(crate) fn macro_qualified_name(node: &Node, context: &dyn ResolutionContext) -> String {
    let prefix =
        context
            .cpp_namespace_tables()
            .macro_prefix(&node.file_path, node.start_line, context);
    if prefix.is_empty() {
        node.qualified_name.clone()
    } else {
        format!("{}::{}", prefix.join("::"), node.qualified_name)
    }
}

/// Kinds a macro-namespaced name may name.
pub const MACRO_NAMESPACED_KINDS: [NodeKind; 9] = [
    NodeKind::Function,
    NodeKind::Method,
    NodeKind::Class,
    NodeKind::Struct,
    NodeKind::Enum,
    NodeKind::TypeAlias,
    NodeKind::Union,
    NodeKind::Variable,
    NodeKind::Constant,
];

fn is_c_family(node: &Node) -> bool {
    matches!(node.language, Language::C | Language::Cpp)
}

/// Resolve `fmt::format` / `fmt::detail::to_unsigned` to the declaration a
/// namespace macro puts there (upstream #2176 `matchCppMacroNamespaced`): a
/// node named `format` (qualified `format` or `detail::to_unsigned` in the
/// index) inside a `FMT_BEGIN_NAMESPACE` … `FMT_END_NAMESPACE` range, or,
/// through a namespace alias (`py::print` under `namespace py =
/// pybind11;`), a declaration whose qualified name is the aliased one.
pub(crate) fn match_cpp_macro_namespaced(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    static NAME: OnceLock<Regex> = OnceLock::new();
    let written = reference.reference_name.as_str();
    let mut target = written.strip_prefix("::").unwrap_or(written).to_string();
    // `::memset` names no namespace for a macro or an alias to open.
    let (head, _) = target.split_once("::")?;
    let head = head.to_string();
    let tables = context.cpp_namespace_tables();
    let alias = tables.namespace_alias(&head, context);
    if let Some(alias) = &alias {
        target = format!("{alias}{}", &target[head.len()..]);
    }
    let name = target.rsplit_once("::").map(|(_, name)| name.to_string())?;
    if !regex(r"^[A-Za-z_~][0-9A-Za-z_]*$", &NAME).is_match(&name) {
        return None;
    }
    let mut matches: Vec<Arc<Node>> = Vec::new();
    for node in context.get_nodes_by_name_shared(&name) {
        if !is_c_family(&node) || !MACRO_NAMESPACED_KINDS.contains(&node.kind) {
            continue;
        }
        let prefix = tables.macro_prefix(&node.file_path, node.start_line, context);
        let effective = if !prefix.is_empty() {
            format!("{}::{}", prefix.join("::"), node.qualified_name)
        } else if alias.is_some() {
            node.qualified_name.clone()
        } else {
            String::new()
        };
        if effective == target {
            matches.push(node);
        }
    }
    // A declaration outside the tests over one in them; among an overload
    // set, the one the call's arguments fit: `fmt::format("{}", v)` is
    // format.h's `format(format_string, T&&...)`, not color.h's
    // `format(const text_style&, …)`.
    let arguments = (matches.len() > 1 && reference.reference_kind == EdgeKind::Calls)
        .then(|| cpp_call_arguments(reference, &name, context))
        .flatten();
    let mut best: Option<(&Arc<Node>, i64)> = None;
    for node in &matches {
        let score = if codegraph_core::file_class::is_test_path(&node.file_path) {
            -10
        } else {
            0
        } + arguments.as_ref().map_or(0, |arguments| {
            cpp_overload_fit(node, &name, arguments, context)
        });
        if best.is_none_or(|(_, best_score)| score > best_score) {
            best = Some((node, score));
        }
    }
    best.map(|(node, _)| ResolvedRef {
        original: reference.clone(),
        target_node_id: node.id.clone(),
        confidence: 0.8,
        resolved_by: ResolvedBy::QualifiedName,
    })
}

/// A C or C++ name written from the global scope (`::hpb::CreateMessage`,
/// `::operator delete`) is the declaration of exactly that name (upstream
/// #2445): never one nested in another namespace or class (fmt's mock
/// `test::open` is not the `::open(…)` it wraps), nor one inside a namespace
/// a macro opens, which only looks global in the index (fmt's `struct
/// pipe`; [`match_cpp_macro_namespaced`] reads those).
pub(crate) fn match_cpp_global_name(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let name = reference.reference_name.strip_prefix("::")?;
    let tables = context.cpp_namespace_tables();
    let global: Vec<Arc<Node>> = context
        .get_nodes_by_qualified_name_shared(name)
        .into_iter()
        .filter(|node| {
            is_c_family(node)
                && !tables
                    .frames(&node.file_path, context)
                    .iter()
                    .any(|frame| frame.holds(node.start_line))
        })
        .collect();
    let count = global.len();
    let chosen = prefer_call_site_file(global, &reference.file_path)
        .into_iter()
        .next()?;
    Some(ResolvedRef {
        original: reference.clone(),
        confidence: if count == 1 || chosen.file_path == reference.file_path {
            0.95
        } else {
            0.85
        },
        target_node_id: chosen.id.clone(),
        resolved_by: ResolvedBy::QualifiedName,
    })
}

/// Split `a, f(b, c), d<e, f>` at its top-level commas.
fn split_cpp_top_level(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' || c == '\'' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != c {
                j += if chars[j] == '\\' { 2 } else { 1 };
            }
            current.extend(&chars[i..chars.len().min(j + 1)]);
            i = j + 1;
            continue;
        }
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            '>' if i == 0 || chars[i - 1] != '-' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if c == ',' && depth == 0 {
            out.push(current.trim().to_string());
            current.clear();
        } else {
            current.push(c);
        }
        i += 1;
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
    out
}

/// The balanced `( … )` after `name` from 1-based line `line`, byte
/// `column`, of `file` (up to a dozen lines).
fn cpp_paren_list_after(
    file: &str,
    line: i64,
    column: usize,
    name: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let facts = context.source_facts(file)?;
    let start = usize::try_from(line - 1).ok()?;
    let end = (start + 12).min(facts.line_count());
    if start >= end {
        return None;
    }
    let text = facts.join_lines(start, end, "\n");
    let rest = text.get(column..)?;
    let at = Regex::new(&format!(
        r"\b{}\s*(?:<[^<>()]*>)?\s*\(",
        regex::escape(name)
    ))
    .ok()?
    .find(rest)?;
    let open = column + at.end() - 1;
    let mut depth = 0usize;
    for (offset, c) in text[open..].char_indices() {
        if c == '(' {
            depth += 1;
        } else if c == ')' {
            depth -= 1;
            if depth == 0 {
                return Some(text[open + 1..open + offset].to_string());
            }
        }
    }
    None
}

fn cpp_call_arguments(
    reference: &RefView,
    name: &str,
    context: &dyn ResolutionContext,
) -> Option<Vec<String>> {
    let column = usize::try_from(reference.column.max(0)).ok()?;
    cpp_paren_list_after(&reference.file_path, reference.line, column, name, context)
        .map(|list| split_cpp_top_level(&list))
}

/// A parameter pack (`T&&... args`), not the `...` inside
/// `format_string<T...>`.
fn is_pack(parameter: &str) -> bool {
    static ARGS: OnceLock<Regex> = OnceLock::new();
    let args = regex(r"<[^<>]*>", &ARGS);
    let mut flat = parameter.to_string();
    loop {
        let next = args.replace_all(&flat, "").into_owned();
        if next == flat {
            break;
        }
        flat = next;
    }
    flat.contains("...")
}

/// How well a call's arguments fit an overload's parameters (upstream
/// #2201 `cppOverloadFit`): the count, and each string literal against its
/// parameter, a string type by name over a template parameter that might be
/// one (`const S&`), a narrow literal never a wide parameter nor a wide one
/// a narrow one.
fn cpp_overload_fit(
    node: &Node,
    name: &str,
    arguments: &[String],
    context: &dyn ResolutionContext,
) -> i64 {
    static LITERAL: OnceLock<Regex> = OnceLock::new();
    static WIDE: OnceLock<Regex> = OnceLock::new();
    static STRINGISH: OnceLock<Regex> = OnceLock::new();
    static STRING: OnceLock<Regex> = OnceLock::new();
    static TEMPLATE_PARAMETER: OnceLock<Regex> = OnceLock::new();
    if !matches!(node.kind, NodeKind::Function | NodeKind::Method) {
        return -1;
    }
    let Some(list) = cpp_paren_list_after(&node.file_path, node.start_line, 0, name, context)
    else {
        return 0;
    };
    let parameters: Vec<String> = split_cpp_top_level(&list)
        .into_iter()
        .filter(|parameter| parameter != "void")
        .collect();
    let variadic = parameters.iter().any(|parameter| is_pack(parameter));
    let required = parameters
        .iter()
        .filter(|parameter| !is_pack(parameter) && !parameter.contains('='))
        .count();
    let mut score = 0i64;
    if arguments.len() < required || (!variadic && arguments.len() > parameters.len()) {
        score -= 3;
    }
    let literal = regex(r#"^(?:u8|u|U|L)?"|^FMT_STRING\s*\("#, &LITERAL);
    let wide = regex(r"\bw(?:string|char_t|format|string_view)|wchar_t", &WIDE);
    let stringish = regex(r"string|char|Char|format", &STRINGISH);
    let string = regex(r"string|char|Char|\bstr\b", &STRING);
    let template_parameter = regex(
        r"^(?:const\s+)?[A-Z][0-9A-Za-z_]{0,2}\s*[&*]{0,2}\s*[0-9A-Za-z_]*$",
        &TEMPLATE_PARAMETER,
    );
    for (argument, parameter) in arguments.iter().zip(&parameters) {
        if is_pack(parameter) {
            break;
        }
        if !literal.is_match(argument) {
            continue;
        }
        let wide_argument = argument.starts_with("L\"");
        let wide_parameter = wide.is_match(parameter);
        if wide_argument != wide_parameter && stringish.is_match(parameter) {
            score -= 2;
        } else if string.is_match(parameter) {
            score += 3;
        } else if template_parameter.is_match(parameter) {
            score += 1;
        } else {
            score -= 2;
        }
    }
    score
}

/// A file's namespace row: its facts as written, and the lines that use a
/// namespace opener or closer the tables know of.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamespaceRow {
    pub facts: CppNamespaceFacts,
    pub usage: Vec<UsageLine>,
}

impl NamespaceRow {
    /// `path`'s row from its text, with the opener and closer names of
    /// `tables`.
    pub fn of(
        path: &str,
        source: &str,
        tables: &CppNamespaceTables,
        context: &dyn ResolutionContext,
    ) -> Self {
        Self {
            facts: facts_of(path, source),
            usage: tables.usage_lines(source.lines(), context),
        }
    }

    /// No fact: no row records it.
    pub fn is_empty(&self) -> bool {
        self.facts.is_empty() && self.usage.is_empty()
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("namespace row encodes")
    }

    pub fn decode(value: &str) -> Option<Self> {
        serde_json::from_str(value).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macros_read_openers_closers_and_aliases_as_written() {
        let facts = facts_of(
            "include/fmt/base.h",
            "#pragma once\n#define FMT_BEGIN_NAMESPACE \\\n  namespace fmt {           \\\n  inline namespace v12 {\n#define FMT_END_NAMESPACE \\\n  }                       \\\n  }\n#define PYBIND11_NAMESPACE pybind11 // the namespace\n#define PYBIND11_NAMESPACE_BEGIN(name) namespace name { PYBIND11_WARNING_PUSH\n#define PYBIND11_NAMESPACE_END(name) PYBIND11_WARNING_POP }\n#define NOT_A_NAMESPACE (1 + 2)\nnamespace py = ::pybind11;\n",
        );
        assert_eq!(facts.aliases, [("py".to_string(), "pybind11".to_string())]);
        assert!(
            matches!(&facts.macros[0], MacroFact::Opener { name, body }
                if name == "FMT_BEGIN_NAMESPACE"
                    && body.split_whitespace().collect::<Vec<_>>()
                        == ["namespace", "fmt", "{", "inline", "namespace", "v12", "{"]),
            "{:?}",
            facts.macros
        );
        assert_eq!(
            facts.macros[1..],
            [
                MacroFact::Closer {
                    name: "FMT_END_NAMESPACE".to_string(),
                    scopes: 2,
                },
                MacroFact::Alias {
                    name: "PYBIND11_NAMESPACE".to_string(),
                    target: "pybind11".to_string(),
                },
                MacroFact::OpenerFn {
                    name: "PYBIND11_NAMESPACE_BEGIN".to_string(),
                },
                MacroFact::Closer {
                    name: "PYBIND11_NAMESPACE_END".to_string(),
                    scopes: 1,
                },
            ]
        );
        // A source holds no macro the table reads; its aliases count.
        let source = facts_of("src/use.cc", "#define X namespace x {\nnamespace a = b;\n");
        assert!(source.macros.is_empty());
        assert_eq!(source.aliases.len(), 1);
    }

    #[test]
    fn frames_follow_openers_and_closers_with_inline_namespaces_transparent() {
        let header = facts_of(
            "base.h",
            "#define NS_ALIAS realns\n#define BEGIN namespace NS_ALIAS { inline namespace v1 {\n#define END } }\n#define BEGIN_FN(name) namespace name {\n#define END_FN(name) }\n",
        );
        let macros = NamespaceMacros::from_facts([&header]);
        let usage = macros.usage_lines(
            [
                "BEGIN",
                "int a;",
                "BEGIN_FN(NS_ALIAS)  // detail",
                "int b;",
                "END_FN(NS_ALIAS);",
                "END",
                "OTHER",
                "BEGIN",
                "int c;",
            ]
            .into_iter(),
        );
        assert_eq!(
            usage.iter().map(|line| line.line).collect::<Vec<_>>(),
            [1, 3, 5, 6, 8]
        );
        let frames = macros.frames_of(&usage);
        assert_eq!(macro_prefix_in(&frames, 2), ["realns"]);
        assert_eq!(macro_prefix_in(&frames, 4), ["realns", "realns"]);
        assert_eq!(macro_prefix_in(&frames, 7), Vec::<String>::new());
        assert_eq!(macro_prefix_in(&frames, 9), ["realns"]);
    }

    #[test]
    fn the_first_definition_by_path_wins() {
        let first = facts_of(
            "a.h",
            "#define NS one\n#define BEGIN namespace first {\nnamespace al = x;\n",
        );
        let second = facts_of(
            "b.h",
            "#define NS two\n#define BEGIN namespace second {\nnamespace al = y;\n",
        );
        let tables = CppNamespaceTables::from_facts([&first, &second]);
        assert_eq!(
            tables
                .aliases
                .get()
                .and_then(|aliases| aliases.get("al"))
                .map(String::as_str),
            Some("x")
        );
        let macros = tables.macros.get().expect("macros");
        assert_eq!(macros.aliases.get("NS").map(String::as_str), Some("one"));
        assert_eq!(
            macros.openers.get("BEGIN"),
            Some(&vec!["first".to_string()])
        );
    }

    #[test]
    fn top_level_commas_skip_nesting_strings_and_arrows() {
        assert_eq!(
            split_cpp_top_level(r#"a, f(b, c), d<e, f>, "x,y", p->q, '\''"#),
            ["a", "f(b, c)", "d<e, f>", "\"x,y\"", "p->q", "'\\''"]
        );
        assert!(is_pack("T&&... args"));
        assert!(!is_pack("format_string<T...> fmt"));
    }

    #[test]
    fn a_row_round_trips() {
        let row = NamespaceRow {
            facts: facts_of("a.h", "#define BEGIN namespace a {\nnamespace b = c;\n"),
            usage: vec![UsageLine {
                line: 3,
                token: "BEGIN".to_string(),
                arg: None,
            }],
        };
        assert_eq!(NamespaceRow::decode(&row.encode()), Some(row));
        assert_eq!(row_key("a.h"), "cpp_namespace_surface:a.h");
    }
}
