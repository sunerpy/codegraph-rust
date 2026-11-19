//! CFML calls reached by name alone (upstream #2148, in the shape at
//! `b635dd46`): CFML types no receiver, so a bare call written in a component
//! (`.cfc`) reaches only a method of the component or of one it `extends`,
//! read from source; anything else is a built-in (coldbox's `now()` went to a
//! date helper's `now` 228 times).
//!
//! The `extends` chain is read from the components' text, as upstream reads
//! it: a dotted path matched against the indexed `.cfc` files by name, the
//! same directory first, else by the longest path suffix.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::csharp_types::js_regex;
use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// How many component files one chain holds (upstream's 30).
const CHAIN_LIMIT: usize = 30;

/// The per-pass memo of the `extends` chains (upstream `CFML_CHAINS`).
#[derive(Debug, Default)]
pub struct CfmlTables {
    chains: Mutex<HashMap<String, Arc<HashSet<String>>>>,
}

impl CfmlTables {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Whether `path` is a CFML component file (`/\.cfc$/i`).
pub fn is_component_file(path: &str) -> bool {
    path.len() >= 4
        && path
            .get(path.len() - 4..)
            .is_some_and(|tail| tail.eq_ignore_ascii_case(".cfc"))
}

/// `^[A-Za-z_]\w*$`: a call's name with no receiver written into it.
fn is_bare_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether `reference` is a bare CFML call, whose candidates
/// [`is_cfml_method_in_scope`] judges (upstream `cfmlBare`).
pub(crate) fn is_cfml_bare_call(reference: &RefView) -> bool {
    reference.language == Language::Cfml
        && reference.reference_kind == EdgeKind::Calls
        && is_bare_name(&reference.reference_name)
}

/// `/\bfunction\b|<cffunction/i`.
fn first_function_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(?i)(?-u:\b)function(?-u:\b)|<cffunction").expect("CFML function pattern")
    })
}

/// `/\bextends\s*=\s*["']?([\w.\/:-]+)["']?/i`, JavaScript's `\s` and `\w`.
fn extends_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| js_regex(r#"(?i)(?-u:\b)extends\s*=\s*["']?([A-Za-z0-9_./:-]+)["']?"#))
}

/// What a component's text names as the component it extends: the first
/// `extends=` before its first function (upstream reads
/// `text.slice(0, text.search(/\bfunction\b|<cffunction/i) >>> 0 ||
/// text.length)`, so a text with no function, or one opening with it, is
/// read whole).
pub fn extends_of(text: &str) -> Option<String> {
    let cut = first_function_pattern()
        .find(text)
        .map(|found| found.start())
        .filter(|&start| start > 0)
        .unwrap_or(text.len());
    extends_pattern()
        .captures(&text[..cut])
        .and_then(|captures| captures.get(1))
        .map(|name| name.as_str().to_string())
}

/// The directory part of `path`, with its trailing `/` (`''` at the root).
fn directory_of(path: &str) -> &str {
    path.rfind('/').map_or("", |slash| &path[..=slash])
}

/// The indexed `.cfc` files a component path names (upstream
/// `cfmlComponentFiles`): the files declaring a class of its last segment's
/// name, any case; for a bare name the ones in `from`'s directory first, else
/// the ones ending in the longest suffix of the path. The files are in path
/// order (upstream: index order; KEEP-RUST), so the chain's limit cuts the
/// same files on every index.
fn component_files(dotted: &str, from: &str, context: &dyn ResolutionContext) -> Vec<String> {
    let flat = dotted.replace('/', ".");
    let segments: Vec<&str> = flat.split('.').filter(|s| !s.is_empty()).collect();
    let Some(last) = segments.last() else {
        return Vec::new();
    };
    let name = last.to_ascii_lowercase();
    let files: Vec<String> = context
        .get_nodes_by_lower_name_shared(&name)
        .into_iter()
        .filter(|node| node.kind == NodeKind::Class && is_component_file(&node.file_path))
        .map(|node| node.file_path.clone())
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect();
    if files.is_empty() {
        return files;
    }
    if segments.len() == 1 {
        let dir = directory_of(from);
        let local: Vec<String> = files
            .iter()
            .filter(|file| directory_of(file) == dir)
            .cloned()
            .collect();
        return if local.is_empty() { files } else { local };
    }
    for take in (1..=segments.len()).rev() {
        let suffix = format!(
            "/{}.cfc",
            segments[segments.len() - take..]
                .join("/")
                .to_ascii_lowercase()
        );
        let hits: Vec<String> = files
            .iter()
            .filter(|file| format!("/{}", file.to_ascii_lowercase()).ends_with(&suffix))
            .cloned()
            .collect();
        if !hits.is_empty() {
            return hits;
        }
    }
    files
}

/// A component file and every component file it extends, at most 30
/// (upstream `cfmlChain`).
fn chain(file: &str, context: &dyn ResolutionContext) -> Arc<HashSet<String>> {
    let tables = context.cfml_tables();
    if let Some(hit) = tables
        .chains
        .lock()
        .ok()
        .and_then(|memo| memo.get(file).cloned())
    {
        return hit;
    }
    let mut chain: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::from([file.to_string()]);
    while chain.len() < CHAIN_LIMIT {
        let Some(current) = queue.pop_front() else {
            break;
        };
        if !chain.insert(current.clone()) {
            continue;
        }
        let extended = context
            .source_facts(&current)
            .and_then(|facts| extends_of(facts.source()));
        if let Some(extended) = extended {
            queue.extend(component_files(&extended, &current, context));
        }
    }
    let chain = Arc::new(chain);
    if let Ok(mut memo) = tables.chains.lock() {
        memo.insert(file.to_string(), Arc::clone(&chain));
    }
    chain
}

/// Whether a bare CFML call can mean `method` (upstream
/// `isCfmlMethodInScope`): in a component, only a method of its own file or
/// of a component it extends. A call in a `.cfm` template is not judged (a
/// ColdBox view runs inside the renderer's scope), nor is one whose name its
/// line does not show without a receiver (a link of a chain the extractor
/// hands over bare).
pub(crate) fn is_cfml_method_in_scope(
    method: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    if !is_component_file(&reference.file_path)
        || !crate::name_matcher::has_no_receiver_on_line(reference, context)
    {
        return true;
    }
    chain(&reference.file_path, context).contains(&method.file_path)
}

/// Whether `reference` names a CFML supertype by its component path
/// (upstream #1152): an `extends` or `implements` written with a `.` or a
/// `/`. A dotted `calls` ref is a member-access chain, never a path.
pub(crate) fn is_cfml_component_path_ref(reference: &RefView) -> bool {
    reference.language == Language::Cfml
        && matches!(
            reference.reference_kind,
            EdgeKind::Extends | EdgeKind::Implements
        )
        && (reference.reference_name.contains('.') || reference.reference_name.contains('/'))
}

/// The CFML supertype a component path names (upstream
/// `resolveCfmlComponentPath`, #1152 / #1154). Two forms exist:
///
/// - Relative, `extends="../base"` / `"./base"` (FW/1): resolved against the
///   referencing file's directory; the class named by the last segment whose
///   file is exactly that path (any case) is it.
/// - Dotted, `extends="coldbox.system.web.Controller"`: dots are directories
///   from the webroot or a mapping, whose leading segments may not exist in
///   the repository (in coldbox itself the path is `system/web/
///   Controller.cfc`). Among the CFML classes and interfaces of the last
///   segment's name, the one whose parent directories match the most
///   segments right to left (any case) is it; one matching none is an
///   out-of-repository library's (mxunit, testbox), and a tie is no edge.
///
/// No other strategy sees the reference: the whole path can only mis-match.
pub(crate) fn resolve_cfml_component_path(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let candidates = |name: &str| -> Vec<Arc<Node>> {
        context
            .get_nodes_by_name_shared(name)
            .into_iter()
            .filter(|node| {
                matches!(node.kind, NodeKind::Class | NodeKind::Interface)
                    && node.language == Language::Cfml
            })
            .collect()
    };
    let norm = |path: &str| path.replace('\\', "/").to_ascii_lowercase();
    let name = &reference.reference_name;
    if name.contains('/') {
        let rel = strip_cfc_suffix(name);
        let from = reference.file_path.replace('\\', "/");
        let mut parts: Vec<&str> = from.split('/').collect();
        parts.pop();
        for segment in rel.split('/') {
            match segment {
                "" | "." => {}
                ".." => {
                    parts.pop()?;
                }
                other => parts.push(other),
            }
        }
        let class_name = parts.last().copied().filter(|n| !n.is_empty())?;
        let want = norm(&format!("{}.cfc", parts.join("/")));
        let mut found: Vec<Arc<Node>> = candidates(class_name)
            .into_iter()
            .filter(|candidate| norm(&candidate.file_path) == want)
            .collect();
        found.sort_by(|a, b| a.id.cmp(&b.id));
        let target = found.into_iter().next()?;
        return Some(ResolvedRef::new(
            reference.clone(),
            target.id.clone(),
            0.95,
            ResolvedBy::FilePath,
        ));
    }
    let segments: Vec<&str> = name
        .split('.')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if segments.len() < 2 {
        return None;
    }
    let class_name = segments[segments.len() - 1];
    let directories = &segments[..segments.len() - 1];
    let mut best: Option<Arc<Node>> = None;
    let mut best_score = 0usize;
    let mut tie = false;
    let mut all = candidates(class_name);
    all.sort_by(|a, b| {
        (&a.file_path, a.start_line, &a.id).cmp(&(&b.file_path, b.start_line, &b.id))
    });
    for candidate in all {
        let score = corroboration(directories, &candidate.file_path);
        if score > best_score {
            best = Some(candidate);
            best_score = score;
            tie = false;
        } else if score == best_score && score > 0 {
            tie = true;
        }
    }
    if best_score == 0 || tie {
        return None;
    }
    let best = best?;
    Some(ResolvedRef::new(
        reference.clone(),
        best.id.clone(),
        0.9,
        ResolvedBy::QualifiedName,
    ))
}

/// How many of a dotted component path's directories `file_path`'s own
/// directories corroborate, right to left and in any case (upstream #1154):
/// `system/web/Controller.cfc` corroborates two of `coldbox.system.web`.
fn corroboration(directories: &[&str], file_path: &str) -> usize {
    let path = file_path.replace('\\', "/");
    let mut dirs: Vec<&str> = path.split('/').collect();
    dirs.pop();
    let mut score = 0usize;
    while score < directories.len()
        && score < dirs.len()
        && directories[directories.len() - 1 - score]
            .eq_ignore_ascii_case(dirs[dirs.len() - 1 - score])
    {
        score += 1;
    }
    score
}

/// What a call on a typed CFML receiver reaches ([`typed_receiver_call`]).
#[derive(Debug)]
pub(crate) enum TypedReceiverCall {
    /// The method of the receiver's type, or of a supertype, the call means.
    Method(Arc<Node>),
    /// Components of the type's name in several files declare the method and
    /// nothing written tells which one the receiver is.
    Ambiguous,
    /// Neither the type nor a supertype declares the method.
    Missing,
}

/// The method a call on a CFML receiver reaches when its declaration writes
/// its type as `written` (upstream #1155): `svc.save()` after `var svc = new
/// a.UserService()`, `createObject("component", "a.UserService")` or a
/// `<cfargument type="a.UserService">`.
///
/// Upstream reduces the written type to its last segment and, when
/// components of that name in several files declare the method, takes the
/// first. Here (KEEP-RUST, fail closed) the component is told apart the way
/// CFML finds it: a dotted path by the directories its file corroborates,
/// right to left and in any case, as an `extends` path is (#1154); a bare
/// name by the call's own directory, which CFML searches first. No
/// corroboration, a tie, or namesakes of a supertype (reached by its name
/// alone) are [`TypedReceiverCall::Ambiguous`]. A method in one file only is
/// the type's whatever the path says, as upstream has it.
pub(crate) fn typed_receiver_call(
    written: &str,
    method_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> TypedReceiverCall {
    let segments: Vec<&str> = written
        .split('.')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let Some((type_name, directories)) = segments.split_last() else {
        return TypedReceiverCall::Missing;
    };
    typed_method_on(type_name, directories, method_name, reference, context, 0)
}

/// [`typed_receiver_call`] on the type `type_name` written under
/// `directories`, walking supertypes as `resolve_method_on_type` does.
fn typed_method_on(
    type_name: &str,
    directories: &[&str],
    method_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
    depth: u32,
) -> TypedReceiverCall {
    let want = format!("{type_name}::{method_name}");
    let nested = format!("::{want}");
    let matches: Vec<Arc<Node>> = context
        .get_nodes_by_name_shared(method_name)
        .into_iter()
        .filter(|m| {
            m.kind == NodeKind::Method
                && crate::name_matcher::same_language_family(m.language, reference.language)
                && (m.qualified_name == want || m.qualified_name.ends_with(&nested))
        })
        .collect();
    if matches.is_empty() {
        if depth < 4 {
            for supertype in context.get_supertypes(type_name, reference.language) {
                match typed_method_on(&supertype, &[], method_name, reference, context, depth + 1) {
                    TypedReceiverCall::Missing => {}
                    reached => return reached,
                }
            }
        }
        return TypedReceiverCall::Missing;
    }
    let files: BTreeSet<&str> = matches.iter().map(|m| m.file_path.as_str()).collect();
    if files.len() == 1 {
        return TypedReceiverCall::Method(Arc::clone(&matches[0]));
    }
    if depth > 0 {
        return TypedReceiverCall::Ambiguous;
    }
    let chosen = if directories.is_empty() {
        let here = directory_of(&reference.file_path);
        let local: Vec<&str> = files
            .iter()
            .copied()
            .filter(|file| directory_of(file) == here)
            .collect();
        match local.as_slice() {
            [only] => Some(*only),
            _ => None,
        }
    } else {
        let mut best: Option<&str> = None;
        let mut best_score = 0usize;
        let mut tie = false;
        for file in &files {
            let score = corroboration(directories, file);
            if score > best_score {
                best = Some(file);
                best_score = score;
                tie = false;
            } else if score == best_score && score > 0 {
                tie = true;
            }
        }
        best.filter(|_| !tie)
    };
    match chosen.and_then(|file| matches.iter().find(|m| m.file_path == file)) {
        Some(method) => TypedReceiverCall::Method(Arc::clone(method)),
        None => TypedReceiverCall::Ambiguous,
    }
}

/// `name` without a trailing `.cfc` (any case).
fn strip_cfc_suffix(name: &str) -> &str {
    if is_component_file(name) {
        &name[..name.len() - 4]
    } else {
        name
    }
}

/// Key prefix of the CFML component rows (W4-CF-01): one `project_metadata`
/// row per `.cfc` file, written by every full index and migration and kept
/// current by sync.
pub const HEAD_ROW_KEY_PREFIX: &str = "cfml_heads:";

/// The `project_metadata` key of `file_path`'s component row.
pub fn head_row_key(file_path: &str) -> String {
    format!("{HEAD_ROW_KEY_PREFIX}{file_path}")
}

/// What the `extends` chain reads of a `.cfc` file: the classes it declares,
/// lowercased (the names a written path resolves to it by), and what its text
/// extends, as [`extends_of`] reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadRow {
    pub components: Vec<String>,
    pub extends: Option<String>,
}

impl HeadRow {
    /// The row of a `.cfc` file whose text is `source` and whose nodes are
    /// `nodes`.
    pub fn of(source: &str, nodes: &[Node]) -> Self {
        let components: BTreeSet<String> = nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Class)
            .map(|node| node.name.to_ascii_lowercase())
            .collect();
        Self {
            components: components.into_iter().collect(),
            extends: extends_of(source),
        }
    }

    /// Nothing for the chain to read: no class and no `extends`.
    pub fn is_empty(&self) -> bool {
        self.components.is_empty() && self.extends.is_none()
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("cfml head row encodes")
    }

    pub fn decode(value: &str) -> Option<Self> {
        serde_json::from_str(value).ok()
    }
}

/// The lowercased last segment of a written component path, the name
/// [`component_files`] looks it up by.
pub fn extended_name(dotted: &str) -> Option<String> {
    dotted
        .replace('/', ".")
        .split('.')
        .rfind(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extends_is_read_before_the_first_function_or_from_the_whole_text() {
        assert_eq!(
            extends_of("component extends=\"app.system.Base\" {\n  function index() {}\n}\n")
                .as_deref(),
            Some("app.system.Base")
        );
        assert_eq!(
            extends_of("<cfcomponent extends='Base'>\n<cffunction name=\"x\"></cffunction>\n")
                .as_deref(),
            Some("Base")
        );
        // After the first function it is not the component's.
        assert_eq!(
            extends_of("component {\n  function x() { var y = { extends = \"Other\" }; }\n}\n"),
            None
        );
        // A text opening with `function`, or with none, is read whole (`0 ||
        // text.length` and `-1 >>> 0`).
        assert_eq!(
            extends_of("function x() {}\ncomponent extends=\"Late\" {}\n").as_deref(),
            Some("Late")
        );
        assert_eq!(
            extends_of("component extends = models/Base {}\n").as_deref(),
            Some("models/Base")
        );
        // `\bfunction\b` is a whole word: `functional` does not cut the head.
        assert_eq!(
            extends_of("// functional\ncomponent extends=\"Base\" {}\n").as_deref(),
            Some("Base")
        );
    }

    #[test]
    fn component_files_and_names_are_read_case_insensitively() {
        assert!(is_component_file("system/Handler.cfc"));
        assert!(is_component_file("views/Main.CFC"));
        assert!(!is_component_file("views/main.cfm"));
        assert_eq!(extended_name("app.system.Base").as_deref(), Some("base"));
        assert_eq!(extended_name("models/User").as_deref(), Some("user"));
        assert_eq!(extended_name(".."), None);
        assert_eq!(directory_of("a/b/C.cfc"), "a/b/");
        assert_eq!(directory_of("C.cfc"), "");
    }

    #[test]
    fn a_bare_cfml_name_has_no_receiver() {
        assert!(is_bare_name("now"));
        assert!(is_bare_name("_init2"));
        assert!(!is_bare_name("server.keyExists"));
        assert!(!is_bare_name("empty?"));
        assert!(!is_bare_name("9x"));
    }

    #[test]
    fn head_rows_round_trip() {
        let row = HeadRow {
            components: vec!["handler".to_string()],
            extends: Some("app.system.Base".to_string()),
        };
        assert_eq!(HeadRow::decode(&row.encode()), Some(row));
        assert!(HeadRow::default().is_empty());
    }
}
