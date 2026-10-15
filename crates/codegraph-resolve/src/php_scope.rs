//! PHP calls and class names read against the class hierarchy and the file's
//! scope (upstream #2142, #2152, #2103, #2235).
//!
//! Extraction records `$this->m()` under the bare name `m`, so exact and fuzzy
//! matching took any same-named method: a call written through `$this->`,
//! `self::`, `static::` or `parent::` reaches only a method of the enclosing
//! class's ancestry — `extends` and trait `use`, read from source and resolved
//! through the file's `namespace` and `use` imports to fully qualified classes
//! — or a subclass's. A call through a receiver nothing typed keeps the one
//! project method of its name only when the receiver is named after a class
//! whose ancestry has it.
//!
//! A call written without a receiver is a function call (#2103), and a bare
//! class name is its namespace's class or the one a `use` imports (#2235).

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

use codegraph_core::generated_header::is_js_whitespace;
use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::types::{RefView, ResolutionContext};

/// The most types one ancestry walk visits (upstream's 80).
pub const ANCESTRY_LIMIT: usize = 80;

/// Per-pass memos: each file's scope, each declaration's supertypes, each
/// ancestry walk, the type enclosing each call line and whether a name is a
/// trait's.
#[derive(Debug, Default)]
pub struct PhpTables {
    file_scopes: Mutex<HashMap<String, Arc<PhpFileScope>>>,
    supertypes: Mutex<HashMap<String, Arc<Vec<String>>>>,
    ancestries: Mutex<HashMap<Vec<String>, Arc<PhpAncestry>>>,
    enclosing: Mutex<HashMap<CallLine, Option<Arc<Node>>>>,
    traits: Mutex<HashMap<String, bool>>,
}

/// A call's file and 1-based line.
type CallLine = (String, i64);

/// What one ancestry walk reached.
#[derive(Debug, Default)]
pub(crate) struct PhpAncestry {
    /// Every type the walk visited, its start included.
    pub(crate) types: BTreeSet<String>,
    /// Whether a visited name has no PHP type declaration in the repository.
    pub(crate) leaves_repo: bool,
}

impl PhpTables {
    pub fn new() -> Self {
        Self::default()
    }
}

/// A PHP file's `namespace` and its file-level `use A\B\C [as D];` imports,
/// alias → fully qualified name (upstream `phpFileScope`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PhpFileScope {
    pub namespace: String,
    pub uses: HashMap<String, String>,
}

/// How a bare PHP method name is written at its call site (upstream
/// `phpSelfReceiver`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PhpVia {
    /// `$this->m()`, `self::m()`, `static::m()`: the enclosing class's own or
    /// inherited methods, or a subclass's.
    SelfCall,
    /// `parent::m()`: an ancestor's.
    Parent,
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Whether `node` is a PHP type a call's hierarchy walks.
fn is_php_type(node: &Node) -> bool {
    node.language == Language::Php
        && matches!(
            node.kind,
            NodeKind::Class | NodeKind::Trait | NodeKind::Interface | NodeKind::Enum
        )
}

/// How the bare PHP call `reference` is written on its line: through
/// `$this` / `self` / `static`, through `parent`, or neither.
pub(crate) fn php_self_receiver(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<PhpVia> {
    let name = reference.reference_name.as_str();
    if reference.language != Language::Php
        || reference.reference_kind != EdgeKind::Calls
        || name.is_empty()
        || !name.bytes().all(is_word_byte)
    {
        return None;
    }
    let facts = context.source_facts(&reference.file_path)?;
    let index = usize::try_from(reference.line).ok()?.checked_sub(1)?;
    let line = facts.raw_line(index)?;
    let mut parent = false;
    for (at, _) in line.match_indices(name) {
        let after = line[at + name.len()..].trim_start();
        if !after.starts_with('(') {
            continue;
        }
        let before = line[..at].trim_end();
        if let Some(arrow) = before.strip_suffix("->") {
            let arrow = arrow.trim_end();
            let arrow = arrow.strip_suffix('?').map_or(arrow, str::trim_end);
            if arrow.strip_suffix("$this").is_some() {
                return Some(PhpVia::SelfCall);
            }
            continue;
        }
        let Some(path) = before.strip_suffix("::") else {
            continue;
        };
        let path = path.trim_end();
        let word_at = path
            .bytes()
            .rposition(|byte| !is_word_byte(byte))
            .map_or(0, |position| position + 1);
        match &path[word_at..] {
            "self" | "static" => return Some(PhpVia::SelfCall),
            "parent" => parent = true,
            _ => {}
        }
    }
    parent.then_some(PhpVia::Parent)
}

/// `^\s*` with JavaScript's whitespace, U+FEFF included.
const JS_SPACE: &str = r"[\s\x{FEFF}]";

/// A PHP file's scope, read once per pass.
pub(crate) fn php_file_scope(file: &str, context: &dyn ResolutionContext) -> Arc<PhpFileScope> {
    let tables = context.php_tables();
    if let Some(hit) = tables
        .file_scopes
        .lock()
        .ok()
        .and_then(|memo| memo.get(file).cloned())
    {
        return hit;
    }
    let text = context
        .source_facts(file)
        .map(|facts| facts.source().to_string())
        .unwrap_or_default();
    let scope = Arc::new(read_file_scope(&text));
    if let Ok(mut memo) = tables.file_scopes.lock() {
        memo.insert(file.to_string(), Arc::clone(&scope));
    }
    scope
}

fn read_file_scope(text: &str) -> PhpFileScope {
    static NAMESPACE: OnceLock<Regex> = OnceLock::new();
    static FIRST_TYPE: OnceLock<Regex> = OnceLock::new();
    static USE: OnceLock<Regex> = OnceLock::new();
    let namespace = NAMESPACE.get_or_init(|| {
        Regex::new(&format!(
            r"(?m)^{JS_SPACE}*namespace{JS_SPACE}+([\w\\]+){JS_SPACE}*[;{{]"
        ))
        .expect("php namespace")
    });
    let first_type = FIRST_TYPE.get_or_init(|| {
        Regex::new(&format!(
            r"(?m)^{JS_SPACE}*(?:(?:abstract|final|readonly){JS_SPACE}+)*(?:class|trait|interface|enum){JS_SPACE}"
        ))
        .expect("php first type")
    });
    let use_line = USE.get_or_init(|| {
        Regex::new(&format!(
            r"(?m)^{JS_SPACE}*use{JS_SPACE}+(?:function{JS_SPACE}+|const{JS_SPACE}+)?([\w\\]+)(?:{JS_SPACE}+as{JS_SPACE}+(\w+))?{JS_SPACE}*;"
        ))
        .expect("php use")
    });
    let mut scope = PhpFileScope {
        namespace: namespace
            .captures(text)
            .map(|caps| caps[1].to_string())
            .unwrap_or_default(),
        uses: HashMap::new(),
    };
    // File-level imports sit before the first type; a trait `use` inside a
    // class body is not one. A type at the file's very start leaves the
    // whole file, as upstream's `search(…) >>> 0 || length` does.
    let end = match first_type.find(text) {
        Some(found) if found.start() > 0 => found.start(),
        _ => text.len(),
    };
    for caps in use_line.captures_iter(&text[..end]) {
        let fqn = caps[1].trim_start_matches('\\').to_string();
        let alias = caps.get(2).map_or_else(
            || fqn.rsplit('\\').next().unwrap_or(&fqn).to_string(),
            |alias| alias.as_str().to_string(),
        );
        scope.uses.insert(alias, fqn);
    }
    scope
}

/// The qualified name (`A\B::C`) the PHP class name `name` written in `file`
/// refers to (upstream `phpTypeQn`).
pub(crate) fn php_type_qn(name: &str, file: &str, context: &dyn ResolutionContext) -> String {
    if name.starts_with('\\') {
        return qualify(name, &PhpFileScope::default());
    }
    qualify(name, &php_file_scope(file, context))
}

/// The qualified name `name`, written in a file whose scope is `scope`,
/// refers to.
fn qualify(name: &str, scope: &PhpFileScope) -> String {
    let fqn = match name.strip_prefix('\\') {
        Some(rooted) => rooted.to_string(),
        None => {
            let (head, rest) = match name.split_once('\\') {
                Some((head, rest)) => (head, Some(rest)),
                None => (name, None),
            };
            match (scope.uses.get(head), rest) {
                (Some(imported), Some(rest)) => format!("{imported}\\{rest}"),
                (Some(imported), None) => imported.clone(),
                (None, _) if !scope.namespace.is_empty() => format!("{}\\{name}", scope.namespace),
                (None, _) => name.to_string(),
            }
        }
    };
    match fqn.rsplit_once('\\') {
        Some((namespace, last)) => format!("{namespace}::{last}"),
        None => fqn,
    }
}

/// The qualified names a PHP class or trait extends and the traits it uses
/// (upstream `phpSupertypeQns`): the `extends` clause of the declaration's
/// first five lines, and every `use A, B;` in its body before its first
/// `function`.
pub(crate) fn php_supertype_qns(decl: &Node, context: &dyn ResolutionContext) -> Arc<Vec<String>> {
    let tables = context.php_tables();
    if let Some(hit) = tables
        .supertypes
        .lock()
        .ok()
        .and_then(|memo| memo.get(&decl.id).cloned())
    {
        return hit;
    }
    let names = context
        .source_facts(&decl.file_path)
        .map(|facts| supertype_names(&facts, decl.start_line, decl.end_line))
        .unwrap_or_default();
    let qns = Arc::new(
        names
            .iter()
            .map(|name| php_type_qn(name, &decl.file_path, context))
            .collect::<Vec<_>>(),
    );
    if let Ok(mut memo) = tables.supertypes.lock() {
        memo.insert(decl.id.clone(), Arc::clone(&qns));
    }
    qns
}

/// The supertype names a declaration at 1-based `start_line ..= end_line`
/// writes, as written.
pub fn supertype_names(
    facts: &crate::source_facts::SourceFacts,
    start_line: i64,
    end_line: i64,
) -> Vec<String> {
    static EXTENDS: OnceLock<Regex> = OnceLock::new();
    static NAME: OnceLock<Regex> = OnceLock::new();
    static BODY_USE: OnceLock<Regex> = OnceLock::new();
    let extends = EXTENDS.get_or_init(|| {
        Regex::new(r"\bextends[\s\x{FEFF}]+([^{]*?)(?:\bimplements\b|\{)").expect("php extends")
    });
    let name = NAME.get_or_init(|| Regex::new(r"(\\?[A-Za-z_][\w\\]*)").expect("php name"));
    let body_use = BODY_USE.get_or_init(|| {
        Regex::new(&format!(
            r"(?m)^{JS_SPACE}*use{JS_SPACE}+([\w\\,\s\x{{FEFF}}]+?){JS_SPACE}*[;{{]"
        ))
        .expect("php body use")
    });
    let count = facts.line_count();
    let start = usize::try_from(start_line).unwrap_or(1).max(1);
    let end = usize::try_from(end_line).unwrap_or(0);
    let head_end = (start + 4).min(count);
    let head = if start <= count {
        facts.join_lines(start - 1, head_end, " ")
    } else {
        String::new()
    };
    let mut names: Vec<String> = extends
        .captures(&head)
        .map(|caps| {
            name.captures_iter(&caps[1])
                .map(|m| m[1].to_string())
                .collect()
        })
        .unwrap_or_default();
    // `use StringTranslationTrait, MessengerTrait;` at the top of the body,
    // one per line or one list across several.
    let body_end = end.min(start + 80).min(count);
    if start < body_end {
        let first_function =
            (start..body_end).find(|&index| contains_word(facts.line(index), "function"));
        let body = facts.join_lines(start, first_function.unwrap_or(body_end), "\n");
        for caps in body_use.captures_iter(&body) {
            names.extend(
                caps[1]
                    .split(',')
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string),
            );
        }
    }
    names
}

/// Whether `line` writes `word` as a whole word (`\bfunction\b`).
fn contains_word(line: &str, word: &str) -> bool {
    let bytes = line.as_bytes();
    line.match_indices(word).any(|(at, _)| {
        (at == 0 || !is_word_byte(bytes[at - 1]))
            && !bytes
                .get(at + word.len())
                .is_some_and(|byte| is_word_byte(*byte))
    })
}

/// Every type `start` reaches through `extends` and trait `use`, and whether
/// the walk left the repository (upstream `phpAncestry`, at most 80). The
/// declarations of one name are read in path order, so the limit cuts the
/// same names on every index (upstream reads them in index order; KEEP-RUST,
/// as the Ruby walk).
pub(crate) fn php_ancestry(start: &[String], context: &dyn ResolutionContext) -> Arc<PhpAncestry> {
    let tables = context.php_tables();
    if let Some(hit) = tables
        .ancestries
        .lock()
        .ok()
        .and_then(|memo| memo.get(start).cloned())
    {
        return hit;
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<String> = start.iter().cloned().collect();
    let mut leaves_repo = false;
    while seen.len() < ANCESTRY_LIMIT {
        let Some(current) = queue.pop_front() else {
            break;
        };
        if !seen.insert(current.clone()) {
            continue;
        }
        let mut declarations: Vec<Arc<Node>> = context
            .get_nodes_by_qualified_name_shared(&current)
            .into_iter()
            .filter(|node| is_php_type(node))
            .collect();
        if declarations.is_empty() {
            leaves_repo = true;
        }
        declarations.sort_by(|a, b| {
            (&a.file_path, a.start_line, a.start_column, &a.id).cmp(&(
                &b.file_path,
                b.start_line,
                b.start_column,
                &b.id,
            ))
        });
        for declaration in &declarations {
            queue.extend(php_supertype_qns(declaration, context).iter().cloned());
        }
    }
    let walk = Arc::new(PhpAncestry {
        types: seen,
        leaves_repo,
    });
    if let Ok(mut memo) = tables.ancestries.lock() {
        memo.insert(start.to_vec(), Arc::clone(&walk));
    }
    walk
}

/// The innermost PHP type around `reference`'s line, read once per pass.
fn enclosing_php_type(reference: &RefView, context: &dyn ResolutionContext) -> Option<Arc<Node>> {
    let tables = context.php_tables();
    let key = (reference.file_path.clone(), reference.line);
    if let Some(hit) = tables
        .enclosing
        .lock()
        .ok()
        .and_then(|memo| memo.get(&key).cloned())
    {
        return hit;
    }
    let enclosing = context
        .get_nodes_in_file_shared(&reference.file_path)
        .into_iter()
        .filter(|node| {
            is_php_type(node)
                && node.start_line <= reference.line
                && node.end_line >= reference.line
        })
        .max_by(|a, b| {
            a.start_line
                .cmp(&b.start_line)
                .then_with(|| b.id.cmp(&a.id))
        });
    if let Ok(mut memo) = tables.enclosing.lock() {
        memo.insert(key, enclosing.clone());
    }
    enclosing
}

/// What a bare PHP call through `$this`, `self`, `static` or `parent` can
/// reach, read once per reference (upstream `isPhpMethodInScope`'s caller
/// side).
pub(crate) struct PhpSelfScope {
    via: PhpVia,
    /// The innermost type around the call and the ancestry the call walks;
    /// `None` outside any type or inside a trait, where `$this` is whichever
    /// class uses it and nothing is judged.
    caller: Option<(Arc<Node>, Arc<PhpAncestry>)>,
}

/// The scope of `reference` when it is written through `$this`, `self`,
/// `static` or `parent`.
pub(crate) fn php_self_scope(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<PhpSelfScope> {
    let via = php_self_receiver(reference, context)?;
    let caller = enclosing_php_type(reference, context)
        .filter(|node| node.kind != NodeKind::Trait)
        .map(|enclosing| {
            let start = match via {
                PhpVia::Parent => php_supertype_qns(&enclosing, context).as_ref().clone(),
                PhpVia::SelfCall => vec![enclosing.qualified_name.clone()],
            };
            let up = php_ancestry(&start, context);
            (enclosing, up)
        });
    Some(PhpSelfScope { via, caller })
}

impl PhpSelfScope {
    /// Whether `method` can be what the call means: a method of the
    /// enclosing class, one it extends or a trait any of them uses, or —
    /// through `$this` — of a subclass; past an ancestor outside the
    /// repository, a repository trait's.
    pub(crate) fn admits(&self, method: &Node, context: &dyn ResolutionContext) -> bool {
        let Some((owner, _)) = method.qualified_name.rsplit_once("::") else {
            return true;
        };
        let Some((enclosing, up)) = &self.caller else {
            return true;
        };
        if up.types.contains(owner) {
            return true;
        }
        // Past an ancestor outside the repository its members are unseen, the
        // repository's traits it uses among them: a trait's method may still
        // be the one meant, an unrelated class's never is.
        if up.leaves_repo && is_php_trait(owner, context) {
            return true;
        }
        // A base class may call what a subclass defines: the owner descends
        // from the caller.
        self.via == PhpVia::SelfCall
            && php_ancestry(&[owner.to_string()], context)
                .types
                .contains(&enclosing.qualified_name)
    }
}

/// Whether the repository declares a trait named `qualified_name`, read once
/// per pass.
fn is_php_trait(qualified_name: &str, context: &dyn ResolutionContext) -> bool {
    let tables = context.php_tables();
    if let Some(hit) = tables
        .traits
        .lock()
        .ok()
        .and_then(|memo| memo.get(qualified_name).copied())
    {
        return hit;
    }
    let is_trait = context
        .get_nodes_by_qualified_name_shared(qualified_name)
        .iter()
        .any(|node| node.kind == NodeKind::Trait);
    if let Ok(mut memo) = tables.traits.lock() {
        memo.insert(qualified_name.to_string(), is_trait);
    }
    is_trait
}

/// The class names a PHP receiver is read as named after (upstream
/// `phpReceiverReaches`): its last link without the `$`, and the last word of
/// that, each capitalized (`$pageRepo` → `PageRepo`, `Repo`).
pub fn receiver_class_names(receiver: &str) -> Vec<String> {
    let last = receiver
        .rsplit('.')
        .next()
        .unwrap_or(receiver)
        .trim_start_matches('$');
    if last.is_empty() {
        return Vec::new();
    }
    let capitalize = |word: &str| {
        let mut chars = word.chars();
        chars.next().map_or_else(String::new, |first| {
            first.to_uppercase().chain(chars).collect::<String>()
        })
    };
    let mut names: Vec<String> = vec![capitalize(last)];
    if let Some(word) = crate::name_matcher::split_camel_case(last).last() {
        names.push(capitalize(word));
    }
    names.dedup();
    names
}

/// Whether a PHP receiver is named after a class that has `method` in its
/// ancestry (upstream `phpReceiverReaches`, #2152): `$page->save()` → Page,
/// which extends Entity.
pub(crate) fn php_receiver_reaches(
    receiver: &str,
    method: &Node,
    context: &dyn ResolutionContext,
) -> bool {
    let Some((owner, _)) = method.qualified_name.rsplit_once("::") else {
        return false;
    };
    receiver_class_names(receiver).iter().any(|name| {
        context
            .get_nodes_by_name_shared(name)
            .iter()
            .filter(|node| is_php_type(node))
            .any(|decl| {
                php_ancestry(std::slice::from_ref(&decl.qualified_name), context)
                    .types
                    .contains(owner)
            })
    })
}

/// The PHP reference kinds whose name is a class name (upstream
/// `PHP_CLASS_NAME_REFS`): what the extractor records of them is the written
/// name's last segment (`new Field\FirstName()` is `FirstName`), at the
/// name's column, or at `new` for an instantiation.
pub(crate) fn is_php_class_name_ref(kind: EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Instantiates | EdgeKind::Extends | EdgeKind::Implements | EdgeKind::References
    )
}

/// The class name a PHP class reference writes at its position, qualified
/// as in the source (`Field\FirstName`, `\Vendor\Lib\X`, or the bare name):
/// at its column, past `new` and any whitespace or comment after it for an
/// instantiation, and the entry of a trait `use A, B\C;` list (recorded at
/// the `use`, by its written name) that is the recorded name. KEEP-RUST:
/// upstream's extractor records the written name, this one the last segment
/// for every other class name ref, so the rules that read the qualification
/// read it here; `None` when the text there is neither the recorded name nor
/// ends in it.
pub(crate) fn php_written_class_name(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    if reference.language != Language::Php || !is_php_class_name_ref(reference.reference_kind) {
        return None;
    }
    let facts = context.source_facts(&reference.file_path)?;
    let index = usize::try_from(reference.line).ok()?.checked_sub(1)?;
    let at = facts
        .lines()
        .raw_line_start(index)?
        .checked_add(usize::try_from(reference.column).ok()?)?;
    let mut text = facts.source().get(at..)?;
    let name = reference.reference_name.as_str();
    let is_named = |written: &str| written == name || written.rsplit('\\').next() == Some(name);
    if reference.reference_kind == EdgeKind::Instantiates
        && let Some(rest) = strip_php_keyword(text, "new")
    {
        text = skip_php_trivia(rest);
    } else if reference.reference_kind == EdgeKind::Implements
        && let Some(rest) = strip_php_keyword(text, "use")
    {
        let mut rest = skip_php_trivia(rest);
        loop {
            let written = php_name_at(rest);
            if written.is_empty() {
                return None;
            }
            if is_named(written) {
                return Some(written.to_string());
            }
            rest = skip_php_trivia(&rest[written.len()..]);
            rest = skip_php_trivia(rest.strip_prefix(',')?);
        }
    }
    let written = php_name_at(text);
    is_named(written).then(|| written.to_string())
}

/// `text` past the PHP keyword `keyword` it starts with, in any case, when
/// no word character follows it (a `\` may: `new\Foo()`).
fn strip_php_keyword<'t>(text: &'t str, keyword: &str) -> Option<&'t str> {
    let head = text.get(..keyword.len())?;
    let rest = &text[keyword.len()..];
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_' || !c.is_ascii();
    (head.eq_ignore_ascii_case(keyword) && !rest.starts_with(word)).then_some(rest)
}

/// `text` past its leading whitespace and comments (`/* … */`, `// …`,
/// `# …` but not a `#[` attribute), line breaks included.
fn skip_php_trivia(mut text: &str) -> &str {
    loop {
        text = text.trim_start_matches(is_js_whitespace);
        if let Some(rest) = text.strip_prefix("/*") {
            text = rest.find("*/").map_or("", |end| &rest[end + 2..]);
        } else if text.starts_with("//") || (text.starts_with('#') && !text.starts_with("#[")) {
            text = text.find('\n').map_or("", |end| &text[end + 1..]);
        } else {
            return text;
        }
    }
}

/// Whether `c` can be part of a PHP class name as written: a word
/// character, a namespace separator or any non-ASCII character.
fn is_php_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '\\' || !c.is_ascii()
}

/// The class name written at the start of `text`, possibly empty.
fn php_name_at(text: &str) -> &str {
    &text[..text
        .find(|c: char| !is_php_name_char(c))
        .unwrap_or(text.len())]
}

/// Whether a bare PHP class name at `reference` can mean `candidate`
/// (upstream `isPhpClassVisible`, #2235): an unqualified class name is the
/// current namespace's class or the one a `use` imports; PHP never falls
/// back to another namespace for classes. koel's `extends Request` went to
/// the first `Request` indexed. KEEP-RUST: the extractor records a class
/// name ref by its last segment, so its written name is read back
/// ([`php_written_class_name`]). A name written with a namespace means only
/// what PHP reads it as, whatever the candidate's kind (upstream's ref keeps
/// the written name, which no other node's name matches); a name that cannot
/// be read back reaches no class, since its namespace is unknown.
pub(crate) fn is_php_class_visible(
    candidate: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    if reference.language != Language::Php {
        return true;
    }
    let name = reference.reference_name.as_str();
    let keyword = ["self", "static", "parent"]
        .iter()
        .any(|keyword| name.eq_ignore_ascii_case(keyword));
    if is_php_class_name_ref(reference.reference_kind) && !keyword {
        match php_written_class_name(reference, context) {
            Some(written) if written.contains('\\') => {
                return candidate.language == Language::Php
                    && candidate.qualified_name
                        == crate::import_resolver::php_qualified_class_qn(
                            &written, reference, context,
                        );
            }
            Some(_) => {}
            None => return !is_php_type(candidate),
        }
    }
    if candidate.language != Language::Php || !is_php_type(candidate) {
        return true;
    }
    let simple = name
        .bytes()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && name.bytes().all(is_word_byte);
    if !simple || keyword {
        return true;
    }
    let fqn = candidate.qualified_name.replace("::", "\\").to_lowercase();
    let scope = php_file_scope(&reference.file_path, context);
    if let Some(imported) = scope.uses.get(name) {
        return imported.to_lowercase() == fqn;
    }
    let expected = if scope.namespace.is_empty() {
        name.to_string()
    } else {
        format!("{}\\{name}", scope.namespace)
    };
    expected.to_lowercase() == fqn
}

/// Whether a PHP `calls` ref is a bare function call, `redirect($url)` or
/// `view('books.show')`, rather than `$this->redirect()`, `$obj->view()` or
/// `Foo::view()` (upstream `isBarePhpCall`, #2103, #2141): PHP has no
/// implicit `$this`, so a call written without a receiver can only be a
/// function. A PHP ref records its call expression's column
/// (`$this->setPageTitle(` sits at `$this`), so a bare call is one whose
/// text at its column is the name itself, a fully qualified function's
/// leading `\` allowed; a `->`, `?->` or `::` just before it is a member
/// call, a `=>` is not (`'size' => filesize($zip)`).
pub(crate) fn is_bare_php_call(reference: &RefView, context: &dyn ResolutionContext) -> bool {
    if reference.language != Language::Php || reference.reference_kind != EdgeKind::Calls {
        return false;
    }
    // KEEP-RUST: the extractor keeps a fully qualified function's `\` in the
    // name (`\strlen`); upstream's records `strlen`.
    let name = reference
        .reference_name
        .strip_prefix('\\')
        .unwrap_or(&reference.reference_name);
    if name.is_empty() || !name.bytes().all(is_word_byte) {
        return false;
    }
    let Some(facts) = context.source_facts(&reference.file_path) else {
        return false;
    };
    let Some(line) = usize::try_from(reference.line)
        .ok()
        .and_then(|line| line.checked_sub(1))
        .and_then(|index| facts.raw_line(index))
    else {
        return false;
    };
    let Ok(column) = usize::try_from(reference.column) else {
        return false;
    };
    let at = if line.as_bytes().get(column) == Some(&b'\\') {
        column + 1
    } else {
        column
    };
    let Some(after) = line.get(at..).and_then(|text| text.strip_prefix(name)) else {
        return false;
    };
    // `\s*(?:\?\.\s*)?[(<]` (upstream `CALL_OPENER`).
    let after = after.trim_start_matches(is_js_whitespace);
    let after = after
        .strip_prefix("?.")
        .map_or(after, |rest| rest.trim_start_matches(is_js_whitespace));
    if !after.starts_with(['(', '<']) {
        return false;
    }
    let before = line[..at].trim_end_matches(is_js_whitespace);
    !(before.ends_with("->") || before.ends_with("::"))
}

/// Key prefix of the PHP head rows (W4-PHP-01): one `project_metadata` row
/// per PHP file declaring a class, trait, interface or enum, written by every
/// full index and migration and kept current by sync.
pub const HEAD_ROW_KEY_PREFIX: &str = "php_heads:";

/// The `project_metadata` key of `file_path`'s head row.
pub fn head_row_key(file_path: &str) -> String {
    format!("{HEAD_ROW_KEY_PREFIX}{file_path}")
}

/// One PHP type as the ancestry walk reads it: its kind (a trait's `$this`
/// is whichever class uses it, and only a trait is reached past an ancestor
/// outside the repository) and the qualified names it extends and uses,
/// resolved through its own file's scope.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HeadEntry {
    pub qualified_name: String,
    pub kind: String,
    pub supers: Vec<String>,
}

/// A PHP file's head row: every type it declares, sorted (positions are not
/// part of it, so an edit above a class changes nothing).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadRow {
    pub entries: Vec<HeadEntry>,
}

impl HeadRow {
    /// The row of a file whose text is `source` and whose nodes are `nodes`:
    /// what [`php_supertype_qns`] reads of each of its types.
    pub fn of(source: &str, nodes: &[Node]) -> Self {
        let facts = crate::source_facts::SourceFacts::new(Arc::from(source));
        let scope = read_file_scope(source);
        let mut entries: Vec<HeadEntry> = nodes
            .iter()
            .filter(|node| is_php_type(node))
            .map(|node| HeadEntry {
                qualified_name: node.qualified_name.clone(),
                kind: node.kind.as_str().to_string(),
                supers: supertype_names(&facts, node.start_line, node.end_line)
                    .iter()
                    .map(|name| qualify(name, &scope))
                    .collect(),
            })
            .collect();
        entries.sort();
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("php head row encodes")
    }

    pub fn decode(value: &str) -> Option<Self> {
        serde_json::from_str(value).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One PHP file, `a.php`, and nothing else.
    struct OneFile(&'static str);

    impl ResolutionContext for OneFile {
        fn get_nodes_in_file(&self, _file_path: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_nodes_by_name(&self, _name: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_nodes_by_qualified_name(&self, _qualified_name: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_nodes_by_kind(&self, _kind: NodeKind) -> Vec<Node> {
            Vec::new()
        }
        fn file_exists(&self, file_path: &str) -> bool {
            file_path == "a.php"
        }
        fn read_file(&self, file_path: &str) -> Option<String> {
            (file_path == "a.php").then(|| self.0.to_string())
        }
        fn get_project_root(&self) -> &str {
            "/proj"
        }
        fn get_all_files(&self) -> Vec<String> {
            vec!["a.php".to_string()]
        }
        fn get_nodes_by_lower_name(&self, _lower_name: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_node_by_id(&self, _id: &str) -> Option<Node> {
            None
        }
        fn get_import_mappings(
            &self,
            _file_path: &str,
            _language: Language,
        ) -> Vec<crate::types::ImportMapping> {
            Vec::new()
        }
    }

    /// A PHP ref of `kind` named `name` in `a.php` at 1-based `line` and
    /// zero-based byte `column`.
    fn php_ref(kind: EdgeKind, name: &str, line: i64, column: i64) -> RefView {
        RefView {
            row_id: None,
            from_node_id: "method:x".to_string(),
            reference_name: name.to_string(),
            reference_kind: kind,
            line,
            column,
            file_path: "a.php".to_string(),
            language: Language::Php,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    #[test]
    fn a_bare_php_call_is_written_without_a_receiver() {
        let context = OneFile(
            "<?php\nreturn redirect('/x');\n$this->redirect();\n$a = ['total' => count($items)];\nFoo::view();\n\\strlen('x');\n$x?->user ();\n$y = user ();\n",
        );
        let call = |name: &str, line: i64, column: i64| {
            is_bare_php_call(&php_ref(EdgeKind::Calls, name, line, column), &context)
        };
        assert!(call("redirect", 2, 7));
        assert!(
            !call("redirect", 3, 0),
            "`$this->redirect()` sits at `$this`"
        );
        assert!(!call("redirect", 3, 7), "a column on the name after `->`");
        assert!(call("count", 4, 17), "`=>` is no member arrow");
        assert!(!call("view", 5, 5), "a column on the name after `::`");
        assert!(call("\\strlen", 6, 0), "a fully qualified function");
        assert!(!call("user", 7, 5), "a column on the name after `?->`");
        assert!(call("user", 8, 5), "whitespace before the parenthesis");
        assert!(
            !call("user", 8, 0),
            "the text at the column is not the name"
        );
    }

    /// A PHP node of `kind` named after the last segment of `qualified_name`.
    fn php_node(kind: NodeKind, qualified_name: &str) -> Node {
        Node {
            id: format!("{}:{qualified_name}", kind.as_str()),
            kind,
            name: qualified_name
                .rsplit("::")
                .next()
                .unwrap_or(qualified_name)
                .to_string(),
            qualified_name: qualified_name.to_string(),
            file_path: "b.php".to_string(),
            language: Language::Php,
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

    #[test]
    fn a_php_class_name_that_cannot_be_read_back_reaches_no_class() {
        let context = OneFile("<?php\nnamespace App;\nclass A extends Base {}\n");
        let at_extends = php_ref(EdgeKind::Extends, "Base", 3, 8);
        let at_name = php_ref(EdgeKind::Extends, "Base", 3, 16);
        let class = php_node(NodeKind::Class, "App::Base");
        assert!(
            !is_php_class_visible(&class, &at_extends, &context),
            "a column off the name tells no namespace: fail closed"
        );
        assert!(
            is_php_class_visible(
                &php_node(NodeKind::Function, "App::Base"),
                &at_extends,
                &context
            ),
            "a namesake of another kind is not judged"
        );
        assert!(is_php_class_visible(&class, &at_name, &context));
        assert!(!is_php_class_visible(
            &php_node(NodeKind::Class, "Other::Base"),
            &at_name,
            &context
        ));
    }

    #[test]
    fn a_php_class_name_is_read_back_past_comments_line_breaks_and_trait_lists() {
        let context = OneFile(
            "<?php\n$a = new\n    F\\Thing();\n$b = new /* why */ \\V\\X();\n$c = new // note\n    Y();\n$d = new\\Z();\nclass K\n{\n    use A, F\\B,\n        C;\n}\n",
        );
        let written = |kind: EdgeKind, name: &str, line: i64, column: i64| {
            php_written_class_name(&php_ref(kind, name, line, column), &context)
        };
        assert_eq!(
            written(EdgeKind::Instantiates, "Thing", 2, 5).as_deref(),
            Some("F\\Thing"),
            "past a line break"
        );
        assert_eq!(
            written(EdgeKind::Instantiates, "X", 4, 5).as_deref(),
            Some("\\V\\X"),
            "past a block comment"
        );
        assert_eq!(
            written(EdgeKind::Instantiates, "Y", 5, 5).as_deref(),
            Some("Y"),
            "past a line comment"
        );
        assert_eq!(
            written(EdgeKind::Instantiates, "Z", 7, 5).as_deref(),
            Some("\\Z"),
            "`new` right before a `\\`"
        );
        // A trait list entry is recorded by its written name, a class head's by
        // its last segment.
        for (name, expected) in [("A", "A"), ("F\\B", "F\\B"), ("B", "F\\B"), ("C", "C")] {
            assert_eq!(
                written(EdgeKind::Implements, name, 10, 4).as_deref(),
                Some(expected),
                "a trait list entry"
            );
        }
        assert_eq!(written(EdgeKind::Implements, "D", 10, 4), None);
    }

    #[test]
    fn a_php_class_name_is_read_back_as_written() {
        let context = OneFile(
            "<?php\nclass A extends Field\\Base {}\n$a = new  Field\\FirstName();\n$b = new \\Vendor\\X();\n$c = new Thing();\nfunction f(?Field\\Typed $t) {}\n",
        );
        let written = |kind: EdgeKind, name: &str, line: i64, column: i64| {
            php_written_class_name(&php_ref(kind, name, line, column), &context)
        };
        assert_eq!(
            written(EdgeKind::Extends, "Base", 2, 16).as_deref(),
            Some("Field\\Base")
        );
        assert_eq!(
            written(EdgeKind::Instantiates, "FirstName", 3, 5).as_deref(),
            Some("Field\\FirstName"),
            "past `new` and its whitespace"
        );
        assert_eq!(
            written(EdgeKind::Instantiates, "X", 4, 5).as_deref(),
            Some("\\Vendor\\X")
        );
        assert_eq!(
            written(EdgeKind::Instantiates, "Thing", 5, 5).as_deref(),
            Some("Thing")
        );
        assert_eq!(
            written(EdgeKind::References, "Typed", 6, 11),
            None,
            "a column that does not land on the name reads nothing"
        );
        assert_eq!(
            written(EdgeKind::References, "Typed", 6, 12).as_deref(),
            Some("Field\\Typed")
        );
        assert_eq!(written(EdgeKind::Calls, "Base", 2, 16), None);
    }

    #[test]
    fn a_file_scope_reads_its_namespace_and_file_level_imports() {
        let text = "<?php\nnamespace Drupal\\my\\Form;\nuse Drupal\\Core\\Form\\FormBase;\nuse \\Other\\Thing as Alias;\nuse function Foo\\bar;\nclass MyForm extends FormBase {\n  use Trait1;\n}\n";
        let scope = read_file_scope(text);
        assert_eq!(scope.namespace, "Drupal\\my\\Form");
        assert_eq!(
            scope.uses.get("FormBase").map(String::as_str),
            Some("Drupal\\Core\\Form\\FormBase")
        );
        assert_eq!(
            scope.uses.get("Alias").map(String::as_str),
            Some("Other\\Thing")
        );
        assert_eq!(scope.uses.get("bar").map(String::as_str), Some("Foo\\bar"));
        assert!(
            !scope.uses.contains_key("Trait1"),
            "a class body's trait use is no import"
        );
    }

    #[test]
    fn a_receiver_is_named_after_its_last_link_and_its_last_word() {
        assert_eq!(receiver_class_names("page"), ["Page"]);
        assert_eq!(receiver_class_names("$pageRepo"), ["PageRepo", "Repo"]);
        assert_eq!(
            receiver_class_names("this.userAvatars"),
            ["UserAvatars", "Avatars"]
        );
        assert!(receiver_class_names("").is_empty());
    }

    #[test]
    fn a_declaration_names_its_extends_and_its_body_traits() {
        let text = "<?php\nclass Command extends Base\\Command implements X {\n  use A,\n      B;\n  use C { foo as bar; }\n  public function run() {}\n  use Late;\n}\n";
        let facts = crate::source_facts::SourceFacts::new(Arc::from(text));
        assert_eq!(
            supertype_names(&facts, 2, 8),
            ["Base\\Command", "A", "B", "C"]
        );
    }
}
