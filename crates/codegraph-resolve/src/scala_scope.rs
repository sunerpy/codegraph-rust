//! Scala names read at their site (W4-SC-01, upstream #2160, #2193, #2199,
//! #2217 and #2230 in their `b635dd46` shape).
//!
//! A bare Scala call reaches a member only from where it is in reach: a
//! later link of a chain only a member of what the receiver is named after
//! (a type or object only through what holds it), a name the enclosing
//! definition binds only that local, and otherwise a member of the types
//! around the call, of their supertypes (class heads read from source by
//! name), of an anonymous class's bases, of an imported object and what it
//! inherits, or of the package object of the file's own package. A member of
//! `package object X` is in scope only in its package and those under it, or
//! through an import naming it. A symbolic name in a type (`F ~> G`, a
//! kind-projector `*`) is a type or nothing.
//!
//! The extractor keeps the last receiver segment of a nested chain (`a.b.c()`
//! is recorded as `b.c`, KEEP-RUST G14) where upstream records the bare `c`;
//! [`scala_nested_chain_link`] tells the two apart, so the matcher can hold
//! such a reference to upstream's chain-link rule.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// Upstream `SCALA_TYPE_KINDS`.
pub fn is_scala_type_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Trait
            | NodeKind::Interface
            | NodeKind::Enum
            | NodeKind::Struct
            | NodeKind::Module
            | NodeKind::Namespace
    )
}

/// Upstream `SCALA_MEMBER_KINDS`.
fn is_scala_member_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Method
            | NodeKind::Field
            | NodeKind::Property
            | NodeKind::Variable
            | NodeKind::Constant
    )
}

/// JavaScript's `\w`.
fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `^[A-Za-z_$][\w$]*$`.
fn is_scala_simple_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| is_word(c) || c == '$')
}

/// A bare Scala call (upstream `scalaBare`).
pub(crate) fn is_scala_bare_call(reference: &RefView) -> bool {
    reference.language == Language::Scala
        && reference.reference_kind == EdgeKind::Calls
        && is_scala_simple_name(&reference.reference_name)
}

/// Segment `line` (1-based) of `file.split('\n')`.
fn raw_line(file: &str, line: i64, context: &dyn ResolutionContext) -> Option<String> {
    let facts = context.source_facts(file)?;
    let index = usize::try_from(line).ok()?.checked_sub(1)?;
    facts.raw_line(index).map(str::to_string)
}

/// `file`'s source text.
fn source_of(
    file: &str,
    context: &dyn ResolutionContext,
) -> Option<Arc<crate::source_facts::SourceFacts>> {
    context.source_facts(file)
}

/// The first occurrence of `name` on `line` that no word character or `$`
/// precedes and a word boundary ends (`(?<![\w$])name\b`).
fn first_name_occurrence(line: &str, name: &str) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    let ends_in_word = name.chars().next_back().is_some_and(is_word);
    line.match_indices(name).map(|(at, _)| at).find(|&at| {
        !line[..at]
            .chars()
            .next_back()
            .is_some_and(|c| is_word(c) || c == '$')
            && line[at + name.len()..].chars().next().is_some_and(is_word) != ends_in_word
    })
}

/// The project-wide Scala facts a resolution pass reads: the supertypes
/// each type name's declarations name, the package objects, and the
/// supertypes a file's imports and packages bring in.
#[derive(Debug, Default)]
pub struct ScalaTables {
    supertypes: Mutex<HashMap<String, Arc<Vec<String>>>>,
    package_objects: OnceLock<Arc<HashMap<String, Vec<String>>>>,
    imported_supertypes: Mutex<HashMap<String, Arc<HashSet<String>>>>,
}

impl ScalaTables {
    pub fn new() -> Self {
        Self::default()
    }
}

/// The simple names the Scala declarations of `type_name` extend or mix in
/// (upstream `scalaSupertypesOf`), once per pass.
pub(crate) fn scala_supertypes(
    type_name: &str,
    context: &dyn ResolutionContext,
) -> Arc<Vec<String>> {
    let tables = context.scala_tables();
    if let Some(hit) = tables
        .supertypes
        .lock()
        .ok()
        .and_then(|memo| memo.get(type_name).cloned())
    {
        return hit;
    }
    let names = Arc::new(crate::class_heads::scala_supertypes_of(type_name, context));
    if let Ok(mut memo) = tables.supertypes.lock() {
        memo.insert(type_name.to_string(), Arc::clone(&names));
    }
    names
}

/// The supertypes each `package object X extends A with B` in `source`
/// names, by object, in order (upstream `scalaPackageObjects`, which reads
/// only a file containing `package object`; the graph holds no node for a
/// package object).
pub fn scala_package_object_heads(source: &str) -> Vec<(String, Vec<String>)> {
    static OBJECT: OnceLock<Regex> = OnceLock::new();
    static BRACKETS: OnceLock<Regex> = OnceLock::new();
    static WITH: OnceLock<Regex> = OnceLock::new();
    if !source.contains("package object") {
        return Vec::new();
    }
    let object = OBJECT.get_or_init(|| {
        Regex::new(r"(?-u:\b)package\s+object\s+([A-Za-z0-9_$]+)\s+extends\s+([^{\n]+)")
            .expect("package object")
    });
    let brackets = BRACKETS.get_or_init(|| Regex::new(r"\[[^\]]*\]").expect("brackets"));
    let with = WITH.get_or_init(|| Regex::new(r"(?-u:\b)with(?-u:\b)").expect("with"));
    object
        .captures_iter(source)
        .map(|caps| {
            let names = with
                .split(&brackets.replace_all(&caps[2], ""))
                .filter_map(|part| {
                    let last = part.trim().rsplit('.').next().unwrap_or_default();
                    let name = last.split('(').next().unwrap_or_default().trim();
                    (!name.is_empty()).then(|| name.to_string())
                })
                .collect();
            (caps[1].to_string(), names)
        })
        .collect()
}

/// Every Scala file's package objects (upstream `scalaPackageObjects`), once
/// per pass.
fn scala_package_objects(context: &dyn ResolutionContext) -> Arc<HashMap<String, Vec<String>>> {
    let tables = context.scala_tables();
    Arc::clone(tables.package_objects.get_or_init(|| {
        let mut out: HashMap<String, Vec<String>> = HashMap::new();
        for file in context.get_all_files_shared().iter() {
            if !file.ends_with(".scala") {
                continue;
            }
            let Some(facts) = source_of(file, context) else {
                continue;
            };
            for (object, names) in scala_package_object_heads(facts.source()) {
                out.entry(object).or_default().extend(names);
            }
        }
        Arc::new(out)
    }))
}

/// `^\s*package\s+([\w.]+)\s*$` (multiline): every package clause of a
/// Scala file, package objects aside.
fn package_clause_captures(text: &str) -> impl Iterator<Item = &str> {
    static CLAUSE: OnceLock<Regex> = OnceLock::new();
    CLAUSE
        .get_or_init(|| Regex::new(r"(?m)^\s*package\s+([A-Za-z0-9_.]+)\s*$").expect("package"))
        .captures_iter(text)
        .filter_map(|caps| caps.get(1).map(|m| m.as_str()))
}

/// A Scala file's package clauses, segment by segment, in order (`package
/// cats` / `package laws` reads `cats`, `laws`; upstream
/// `scalaPackageClauses`, which skips `package object`).
pub fn scala_package_clauses(text: &str) -> Vec<String> {
    package_clause_captures(text)
        .filter(|clause| *clause != "object" && !clause.starts_with("object."))
        .flat_map(|clause| clause.split('.').map(str::to_string))
        .collect()
}

/// The last segment of a Scala file's last package clause, a top-level
/// type's holder (upstream #2199, whose pattern a `package object X` line
/// never matches).
pub fn scala_holder_package(text: &str) -> Option<String> {
    package_clause_captures(text)
        .last()
        .map(|clause| clause.rsplit('.').next().unwrap_or_default().to_string())
}

/// The full package a Scala file's `package object X` opens (`algebra`,
/// `cats.syntax`), or `None` without one (upstream
/// `scalaPackageObjectPackage`).
pub fn scala_package_object_package(text: &str) -> Option<String> {
    static OBJECT: OnceLock<Regex> = OnceLock::new();
    let object = OBJECT
        .get_or_init(|| {
            Regex::new(r"(?m)^\s*package\s+object\s+([A-Za-z0-9_$]+)").expect("package object")
        })
        .captures(text)?
        .get(1)?
        .as_str()
        .to_string();
    let mut parts = scala_package_clauses(text);
    parts.push(object);
    Some(parts.join("."))
}

/// What a Scala file's imports bring in (upstream `scalaImportsOf`): the
/// owners imported whole (`import a.b.Obj._`, `.*`, `{_}`), the members
/// imported by name (`Obj.x`), and the owners an import rooted at a value
/// the file declares (`import builder._`) names.
#[derive(Debug, Default)]
pub struct ScalaImports {
    /// In first-import order.
    pub owners: Vec<String>,
    pub members: HashSet<String>,
    pub values: HashSet<String>,
}

impl ScalaImports {
    pub fn read(source: &str) -> Self {
        static IMPORT: OnceLock<Regex> = OnceLock::new();
        let import = IMPORT.get_or_init(|| {
            Regex::new(
                r"(?m)^\s*import\s+([A-Za-z0-9_.]+?)\.(?:(_|\*)|\{([^}]*)\}|([A-Za-z0-9_$]+))\s*$",
            )
            .expect("scala import")
        });
        let mut found = Self::default();
        let add_owner = |owners: &mut Vec<String>, owner: &str| {
            if !owners.iter().any(|seen| seen == owner) {
                owners.push(owner.to_string());
            }
        };
        for caps in import.captures_iter(source) {
            let path = &caps[1];
            let owner = path.rsplit('.').next().unwrap_or_default();
            // `import builder._`: rooted at a value the file declares, whose
            // type it doesn't say.
            let root = path.split('.').next().unwrap_or_default();
            if root.starts_with(|c: char| c.is_ascii_lowercase())
                && (root == path || declares_value(source, root))
            {
                found.values.insert(owner.to_string());
            }
            if caps.get(2).is_some() {
                add_owner(&mut found.owners, owner);
                continue;
            }
            let listed = caps
                .get(3)
                .or_else(|| caps.get(4))
                .map_or("", |m| m.as_str());
            for member in listed.split(',') {
                let member = member.trim();
                let id = member
                    .find("=>")
                    .map_or(member, |at| member[..at].trim_end());
                if id == "_" || id == "*" {
                    add_owner(&mut found.owners, owner);
                } else if !id.is_empty() {
                    found.members.insert(format!("{owner}.{id}"));
                }
            }
        }
        found
    }
}

/// Whether `source` declares `root` as a value: `\b(?:val|var|lazy\s+val)\s+root\b`
/// or a parameter `[(,]\s*root\s*:`.
fn declares_value(source: &str, root: &str) -> bool {
    let root = regex::escape(root);
    Regex::new(&format!(
        r"(?-u:\b)(?:val|var|lazy\s+val)\s+{root}(?-u:\b)|[(,]\s*{root}\s*:"
    ))
    .is_ok_and(|declares| declares.is_match(source))
}

/// `file`'s imports, read once.
fn scala_imports_of(file: &str, context: &dyn ResolutionContext) -> Arc<ScalaImports> {
    source_of(file, context).map_or_else(
        || Arc::new(ScalaImports::default()),
        |facts| facts.scala_imports(),
    )
}

/// Every supertype of the objects `file` imports whole and of the package
/// objects of its packages (upstream `scalaImportedSupertypes`): code in
/// `package sttp.client4` sees `package object client4 extends SttpApi`'s
/// members unimported. At most 120 names, once per pass.
fn scala_imported_supertypes(
    file: &str,
    imports: &ScalaImports,
    context: &dyn ResolutionContext,
) -> Arc<HashSet<String>> {
    let tables = context.scala_tables();
    if let Some(hit) = tables
        .imported_supertypes
        .lock()
        .ok()
        .and_then(|memo| memo.get(file).cloned())
    {
        return hit;
    }
    let packages: Vec<String> = source_of(file, context)
        .map(|facts| {
            package_clause_captures(facts.source())
                .flat_map(|clause| clause.split('.').map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let package_objects = scala_package_objects(context);
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = imports.owners.iter().cloned().chain(packages).collect();
    while seen.len() < 120 {
        let Some(type_name) = queue.pop_front() else {
            break;
        };
        let supertypes = scala_supertypes(&type_name, context);
        let objects = package_objects.get(&type_name);
        for sup in supertypes.iter().chain(objects.into_iter().flatten()) {
            if seen.insert(sup.clone()) {
                queue.push_back(sup.clone());
            }
        }
    }
    let seen = Arc::new(seen);
    if let Ok(mut memo) = tables.imported_supertypes.lock() {
        memo.insert(file.to_string(), Arc::clone(&seen));
    }
    seen
}

/// The receiver a Scala `….name` is written on, as its dotted identifiers
/// with call and type arguments dropped: `proc("bash").call()` is `proc`,
/// `Alternative[List].unite` is `Alternative`, `checker.value.onWrite` is
/// `checker.value` (upstream `scalaReceiverName`).
pub(crate) fn scala_receiver_name(before: &str) -> String {
    static TRAILING_DOT: OnceLock<Regex> = OnceLock::new();
    let trailing = TRAILING_DOT.get_or_init(|| Regex::new(r"\s*\.\s*$").expect("trailing dot"));
    let text = trailing.replace(before, "");
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<char> = Vec::new();
    let mut i = chars.len() as isize - 1;
    while i >= 0 {
        let ch = chars[i as usize];
        if ch == ')' || ch == ']' {
            let open = if ch == ')' { '(' } else { '[' };
            let mut depth = 0usize;
            while i >= 0 {
                let at = chars[i as usize];
                if at == ch {
                    depth += 1;
                } else if at == open {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                i -= 1;
            }
            if i < 0 {
                return String::new();
            }
            i -= 1;
        } else if is_word(ch) || ch == '$' || ch == '.' {
            out.push(ch);
            i -= 1;
        } else {
            break;
        }
    }
    out.reverse();
    let joined: String = out.into_iter().collect();
    joined.trim_matches('.').to_string()
}

/// A top-level or nested Scala type's holder: its outer type, or for a
/// top-level one the last segment of its file's last package clause (Scala
/// qualified names leave the package out; upstream #2199).
fn type_holder(node: &Node, context: &dyn ResolutionContext) -> String {
    let segments: Vec<&str> = node.qualified_name.split("::").collect();
    if segments.len() >= 2 {
        return segments[segments.len() - 2]
            .rsplit('.')
            .next()
            .unwrap_or_default()
            .to_string();
    }
    source_of(&node.file_path, context)
        .and_then(|facts| facts.scala_holder_package())
        .unwrap_or_default()
}

/// Whether a Scala function is declared in an `extension (…)` block: its
/// line or one of the three above it opens one (upstream
/// `isScalaExtensionMethod`).
pub fn is_scala_extension_method_in(
    source_lines: &crate::source_facts::SourceFacts,
    start_line: i64,
) -> bool {
    let Ok(start) = usize::try_from(start_line) else {
        return false;
    };
    (start.saturating_sub(4)..start).any(|index| {
        source_lines.raw_line(index).is_some_and(|line| {
            line.trim_start()
                .strip_prefix("extension")
                .is_some_and(|rest| !rest.starts_with(is_word))
        })
    })
}

fn is_scala_extension_method(node: &Node, context: &dyn ResolutionContext) -> bool {
    source_of(&node.file_path, context)
        .is_some_and(|facts| is_scala_extension_method_in(&facts, node.start_line))
}

/// The end of a binder of `name` starting at byte `at` of `text`: upstream's
/// `(?:[(,\[]\s*(?:implicit\s+|using\s+)?NAME\s*:)|(?:\b(?:val|var|def|lazy\s+val)\s+NAME\b)|(?:(?<![\w$.])NAME\s*(?:=>|<-))|(?:\(\s*NAME\s*(?:,[^)]*)?\)\s*=>)`,
/// alternatives tried in order.
fn binder_at(text: &str, at: usize, name: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let skip_space = |mut i: usize| {
        while let Some(c) = text[i..].chars().next() {
            if !c.is_whitespace() {
                break;
            }
            i += c.len_utf8();
        }
        i
    };
    let name_ends_in_word = name.chars().next_back().is_some_and(is_word);
    let name_at = |i: usize| text[i..].starts_with(name).then(|| i + name.len());
    let word_boundary_after_name =
        |end: usize| text[end..].chars().next().is_some_and(is_word) != name_ends_in_word;
    let first = *bytes.get(at)?;
    // `[(,\[]\s*(?:implicit\s+|using\s+)?NAME\s*:`
    if matches!(first, b'(' | b',' | b'[') {
        let after = skip_space(at + 1);
        for prefix in ["implicit", "using", ""] {
            let start = if prefix.is_empty() {
                Some(after)
            } else {
                text[after..].strip_prefix(prefix).and_then(|rest| {
                    let spaced = skip_space(after + prefix.len());
                    (rest.starts_with(char::is_whitespace)).then_some(spaced)
                })
            };
            if let Some(end) = start.and_then(name_at) {
                let colon = skip_space(end);
                if bytes.get(colon) == Some(&b':') {
                    return Some(colon + 1);
                }
            }
        }
    }
    // `\b(?:val|var|def|lazy\s+val)\s+NAME\b`
    if !text[..at].chars().next_back().is_some_and(is_word) {
        for keyword in ["val", "var", "def", "lazy"] {
            let Some(rest) = text[at..].strip_prefix(keyword) else {
                continue;
            };
            let mut after = at + keyword.len();
            if keyword == "lazy" {
                if !rest.starts_with(char::is_whitespace) {
                    continue;
                }
                let spaced = skip_space(after);
                if !text[spaced..].starts_with("val") {
                    continue;
                }
                after = spaced + 3;
            }
            if !text[after..].starts_with(char::is_whitespace) {
                continue;
            }
            if let Some(end) = name_at(skip_space(after))
                && word_boundary_after_name(end)
            {
                return Some(end);
            }
        }
    }
    // `(?<![\w$.])NAME\s*(?:=>|<-)`
    if !text[..at]
        .chars()
        .next_back()
        .is_some_and(|c| is_word(c) || c == '$' || c == '.')
        && let Some(end) = name_at(at)
    {
        let arrow = skip_space(end);
        if text[arrow..].starts_with("=>") || text[arrow..].starts_with("<-") {
            return Some(arrow + 2);
        }
    }
    // `\(\s*NAME\s*(?:,[^)]*)?\)\s*=>`
    if first == b'('
        && let Some(end) = name_at(skip_space(at + 1))
    {
        let mut close = skip_space(end);
        if bytes.get(close) == Some(&b',') {
            close = text[close..]
                .find(')')
                .map_or(text.len(), |offset| close + offset);
        }
        if bytes.get(close) == Some(&b')') {
            let arrow = skip_space(close + 1);
            if text[arrow..].starts_with("=>") {
                return Some(arrow + 2);
            }
        }
    }
    None
}

/// Where the binders of `name` in `text` start, as a global regex finds
/// them one after another.
fn binder_starts(text: &str, name: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut at = 0;
    while at < text.len() {
        if !text.is_char_boundary(at) {
            at += 1;
            continue;
        }
        if let Some(end) = binder_at(text, at, name) {
            starts.push(at);
            at = end.max(at + 1);
        } else {
            at += text[at..].chars().next().map_or(1, char::len_utf8);
        }
    }
    starts
}

/// The definition around a Scala site that binds `name` itself (a
/// parameter, a `val` / `var` / `def`, a lambda or `for` parameter), or
/// `None` (upstream `scalaLocalBinder`). With no function around the site,
/// the innermost type stands in, and a binder counts only inside a block
/// still open at the site: not a sibling test's `val f`, not the class's own
/// members at its body's depth.
fn scala_local_binder(
    name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<Arc<Node>> {
    let nodes = context.get_nodes_in_file_shared(&reference.file_path);
    let innermost = |kind: &dyn Fn(NodeKind) -> bool| {
        nodes
            .iter()
            .filter(|f| {
                kind(f.kind) && f.start_line <= reference.line && f.end_line >= reference.line
            })
            .min_by_key(|f| f.end_line - f.start_line)
            .cloned()
    };
    let function = innermost(&|kind| matches!(kind, NodeKind::Method | NodeKind::Function));
    let scope = function
        .clone()
        .or_else(|| innermost(&is_scala_type_kind))?;
    let facts = context.source_facts(&reference.file_path)?;
    let first = usize::try_from(scope.start_line).ok()?.checked_sub(1)?;
    let last = usize::try_from(reference.line).ok()?;
    let text = (first..last)
        .map_while(|index| facts.raw_line(index))
        .collect::<Vec<_>>()
        .join("\n");
    let starts = binder_starts(&text, name);
    if function.is_some() {
        return (!starts.is_empty()).then_some(scope);
    }
    let mut block_at = vec![0usize; text.len()];
    let mut open: Vec<usize> = Vec::new();
    let mut next = 0usize;
    for (i, byte) in text.bytes().enumerate() {
        if byte == b'{' {
            next += 1;
            open.push(next);
        } else if byte == b'}' {
            open.pop();
        }
        block_at[i] = if open.len() > 1 {
            open[open.len() - 1]
        } else {
            0
        };
    }
    let live: HashSet<usize> = open.iter().skip(1).copied().collect();
    starts
        .iter()
        .any(|&at| {
            let block = block_at.get(at).copied().unwrap_or(0);
            block != 0 && live.contains(&block)
        })
        .then_some(scope)
}

/// The types an anonymous class around a Scala site instantiates (upstream
/// `scalaAnonymousBases`): `new scopt.OptionParser[Config]("scopt") {
/// head("scopt") }` puts OptionParser's members in scope. Read backwards
/// over the open braces of up to 400 lines above the site.
fn scala_anonymous_bases(reference: &RefView, context: &dyn ResolutionContext) -> Vec<String> {
    static HEAD: OnceLock<Regex> = OnceLock::new();
    static ARGUMENTS: OnceLock<Regex> = OnceLock::new();
    static WITH: OnceLock<Regex> = OnceLock::new();
    let head = HEAD.get_or_init(|| {
        Regex::new(
            r"(?-u:\b)new\s+([A-Za-z0-9_.]+(?:\s*\[[^\]]*\])?(?:\s*\([^)]*\))?(?:\s+with\s+[A-Za-z0-9_.]+(?:\s*\[[^\]]*\])?)*)\s*$",
        )
        .expect("anonymous head")
    });
    let arguments =
        ARGUMENTS.get_or_init(|| Regex::new(r"\[[^\]]*\]|\([^)]*\)").expect("arguments"));
    let with = WITH.get_or_init(|| Regex::new(r"\s+with\s+").expect("with"));
    let mut bases = Vec::new();
    let Some(facts) = context.source_facts(&reference.file_path) else {
        return bases;
    };
    let Some(last) = usize::try_from(reference.line)
        .ok()
        .and_then(|line| line.checked_sub(1))
    else {
        return bases;
    };
    let lowest = last.saturating_sub(399);
    let mut depth = 0usize;
    for index in (lowest..=last).rev() {
        let Some(text) = facts.raw_line(index) else {
            continue;
        };
        for (at, ch) in text.char_indices().rev() {
            if ch == '}' {
                depth += 1;
            } else if ch == '{' {
                if depth > 0 {
                    depth -= 1;
                    continue;
                }
                if let Some(caps) = head.captures(&text[..at]) {
                    let written = arguments.replace_all(&caps[1], "");
                    for base in with.split(&written) {
                        bases.push(
                            base.trim()
                                .rsplit('.')
                                .next()
                                .unwrap_or_default()
                                .to_string(),
                        );
                    }
                }
            }
        }
    }
    bases
}

/// Whether a bare Scala name can mean the member `n`, read at its site
/// (upstream `isScalaMemberInScope`).
pub(crate) fn is_scala_member_in_scope(
    n: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    let Some(line) = raw_line(&reference.file_path, reference.line, context) else {
        return true;
    };
    let name = reference.reference_name.as_str();
    let Some(at) = first_name_occurrence(&line, name) else {
        return true;
    };
    let before = &line[..at];
    if before.trim_end().ends_with('.') {
        // A later link of a chain: a member of what the receiver is named
        // after, never a package object's function unless it is an
        // `extension` method.
        if n.file_path == reference.file_path {
            return true;
        }
        // A type or object as a chain link (`pkg.Obj(…)`) is named by what
        // holds it: cats' `arbitrary[Int].map { … }` is no call of
        // alleycats' `object map` (#2199).
        if is_scala_type_kind(n.kind) {
            let holder = type_holder(n, context);
            return !holder.is_empty()
                && scala_receiver_name(before).rsplit('.').next() == Some(holder.as_str());
        }
        if !is_scala_member_kind(n.kind) {
            return n.kind != NodeKind::Function || is_scala_extension_method(n, context);
        }
        let receiver = scala_receiver_name(before);
        return !receiver.is_empty() && crate::kotlin_scope::shares_receiver_word(&receiver, n);
    }
    // A name the enclosing definition binds is that local: `f(true)` with
    // `f: A => B` is not a case class's field `f`.
    if let Some(local) = scala_local_binder(name, reference, context) {
        return n.file_path == reference.file_path
            && n.start_line >= local.start_line
            && n.end_line <= local.end_line;
    }
    if !is_scala_member_kind(n.kind) {
        return true;
    }
    let Some(cut) = n.qualified_name.rfind("::") else {
        return true;
    };
    if n.file_path == reference.file_path {
        return true;
    }
    let owner = n.qualified_name[..cut]
        .rsplit("::")
        .next()
        .unwrap_or_default();
    let imports = scala_imports_of(&reference.file_path, context);
    if imports.owners.iter().any(|imported| imported == owner)
        || imports.members.contains(&format!("{owner}.{name}"))
    {
        return true;
    }
    // `import builder._` brings in a value's members, of a type the file
    // doesn't say.
    if !imports.values.is_empty() {
        return true;
    }
    // An imported object's inherited members: `import sttp.client4._` is
    // `package object client4 extends SttpApi`, so `multipart(…)` is
    // SttpApi's (#2193).
    if scala_imported_supertypes(&reference.file_path, &imports, context).contains(owner) {
        return true;
    }
    let around: Vec<String> = context
        .get_nodes_in_file_shared(&reference.file_path)
        .iter()
        .filter(|t| {
            is_scala_type_kind(t.kind)
                && t.start_line <= reference.line
                && t.end_line >= reference.line
        })
        .map(|t| t.name.clone())
        .collect();
    if around.is_empty() {
        return true;
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = around
        .into_iter()
        .chain(scala_anonymous_bases(reference, context))
        .collect();
    while seen.len() < 60 {
        let Some(type_name) = queue.pop_front() else {
            break;
        };
        if !seen.insert(type_name.clone()) {
            continue;
        }
        if type_name == owner {
            return true;
        }
        queue.extend(scala_supertypes(&type_name, context).iter().cloned());
    }
    false
}

/// Whether a member of a Scala package object is in scope at `reference`:
/// from its package and the packages under it, or from a file importing
/// through the package (`import algebra._`, `import algebra.Eq`; upstream
/// #2230 `isScalaPackageObjectMemberVisible`).
pub(crate) fn is_scala_package_object_member_visible(
    candidate: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    let Some(object_package) = source_of(&candidate.file_path, context)
        .and_then(|facts| facts.scala_package_object_package())
    else {
        return true;
    };
    let Some(here) = source_of(&reference.file_path, context) else {
        return false;
    };
    let text = here.source();
    let package = scala_package_clauses(text).join(".");
    if package == object_package || package.starts_with(&format!("{object_package}.")) {
        return true;
    }
    let last = object_package.rsplit('.').next().unwrap_or_default();
    Regex::new(&format!(
        r"(?m)^\s*import\s+[^\n]*(?-u:\b){}(?-u:\b)",
        regex::escape(last)
    ))
    .is_ok_and(|import| import.is_match(text))
}

/// `^[^\w\s]+$`: a symbolic name (`*`, `~>`).
pub(crate) fn is_symbolic_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| !is_word(c) && !c.is_whitespace())
}

/// A symbolic name in a Scala type is a type (`F ~> G`) or a kind-projector
/// placeholder (`Either[A, *]`), never an operator method, by any strategy
/// (upstream #2217): `Some` with the one Scala type or alias of that name,
/// `Some(None)` with none or several, `None` for any other reference.
pub(crate) fn match_scala_symbolic_type(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<Option<ResolvedRef>> {
    if reference.language != Language::Scala
        || reference.reference_kind != EdgeKind::References
        || !is_symbolic_name(&reference.reference_name)
    {
        return None;
    }
    let types: Vec<Arc<Node>> = context
        .get_nodes_by_name_shared(&reference.reference_name)
        .into_iter()
        .filter(|n| {
            n.language == Language::Scala
                && (is_scala_type_kind(n.kind) || n.kind == NodeKind::TypeAlias)
        })
        .collect();
    let chosen = if types.len() > 1 {
        crate::name_matcher::prefer_call_site_file(types, &reference.file_path)
    } else {
        types
    };
    Some(match chosen.as_slice() {
        [only] => Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: only.id.clone(),
            confidence: 0.8,
            resolved_by: ResolvedBy::ExactMatch,
        }),
        _ => None,
    })
}

/// The bare name upstream records for a Scala call the extractor records as
/// `seg.name` (KEEP-RUST G14): `name`, when the receiver chain is longer than
/// `seg` (`a.seg.name(…)`, `f(x).seg.name`), read from the reference's line
/// at or after its column; `None` for a plain `seg.name(…)` or a shape the
/// line does not show.
pub(crate) fn scala_nested_chain_link(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    if reference.language != Language::Scala || reference.reference_kind != EdgeKind::Calls {
        return None;
    }
    let (segment, name) = reference.reference_name.split_once('.')?;
    if !is_scala_simple_name(segment) || !is_scala_simple_name(name) {
        return None;
    }
    let line = raw_line(&reference.file_path, reference.line, context)?;
    let column = usize::try_from(reference.column)
        .unwrap_or(0)
        .min(line.len());
    let from = (0..=column)
        .rev()
        .find(|&at| line.is_char_boundary(at))
        .unwrap_or(0);
    let rest = &line[from..];
    let link = Regex::new(&format!(
        r"(?:^|[^A-Za-z0-9_$]){}\s*\.\s*{}(?:[^A-Za-z0-9_$]|$)",
        regex::escape(segment),
        regex::escape(name)
    ))
    .ok()?;
    let found = link.find(rest)?;
    let mut start = found.start();
    if !rest[start..].starts_with(segment) {
        start += rest[start..].chars().next().map_or(1, char::len_utf8);
    }
    rest[..start]
        .trim_end()
        .ends_with('.')
        .then(|| name.to_string())
}

/// The `{ … }` block, deeper than its class body, that a Scala `val` filed
/// under its class is written in (cats' `test("…") { val f = … }`), or
/// `None` for a member (upstream #2193 `scalaBlockOf`).
pub(crate) fn scala_block_of(
    candidate: &Node,
    context: &dyn ResolutionContext,
) -> Option<(i64, i64)> {
    let nodes = context.get_nodes_in_file_shared(&candidate.file_path);
    let facts = context.source_facts(&candidate.file_path)?;
    scala_block_among(&facts, nodes.iter().map(|n| n.as_ref()), candidate)
}

/// [`scala_block_of`] over a file's nodes (in the store's order) and text.
fn scala_block_among<'n>(
    facts: &crate::source_facts::SourceFacts,
    mut nodes: impl Iterator<Item = &'n Node>,
    candidate: &Node,
) -> Option<(i64, i64)> {
    let owner = candidate
        .qualified_name
        .rfind("::")
        .map_or("", |cut| &candidate.qualified_name[..cut]);
    let class = nodes.find(|n| {
        n.qualified_name == owner
            && matches!(
                n.kind,
                NodeKind::Class | NodeKind::Trait | NodeKind::Struct | NodeKind::Module
            )
    })?;
    if class.start_line >= candidate.start_line {
        return None;
    }
    scala_block_in(
        facts,
        class.start_line,
        class.end_line,
        candidate.start_line,
    )
}

/// [`scala_block_of`] over a file's lines: the block a declaration starting
/// on line `start` of a class spanning `class_start..=class_end` is written
/// in, deeper than the class body.
pub fn scala_block_in(
    facts: &crate::source_facts::SourceFacts,
    class_start: i64,
    class_end: i64,
    start: i64,
) -> Option<(i64, i64)> {
    static STRINGS: OnceLock<Regex> = OnceLock::new();
    let strings = STRINGS.get_or_init(|| {
        Regex::new(r#""(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)'"#).expect("scala strings")
    });
    let clean = |line: i64| -> String {
        let text = usize::try_from(line)
            .ok()
            .and_then(|line| line.checked_sub(1))
            .filter(|&index| index < facts.line_count())
            .map_or("", |index| facts.line(index));
        let blanked = strings.replace_all(text, "\"\"");
        match blanked.find("//") {
            Some(at) => blanked[..at].to_string(),
            None => blanked.into_owned(),
        }
    };
    let mut opens: Vec<i64> = Vec::new();
    let mut depth = 0usize;
    for line in class_start..start {
        for ch in clean(line).chars() {
            if ch == '{' {
                depth += 1;
                opens.push(line);
            } else if ch == '}' {
                depth = depth.saturating_sub(1);
                opens.pop();
            }
        }
    }
    if depth <= 1 {
        return None;
    }
    let block_start = *opens.last()?;
    let mut d = depth as i64;
    for line in start..=class_end {
        for ch in clean(line).chars() {
            if ch == '{' {
                d += 1;
            } else if ch == '}' {
                d -= 1;
                if d < depth as i64 {
                    return Some((block_start, line));
                }
            }
        }
    }
    Some((block_start, class_end))
}

/// The `project_metadata` key prefix of a Scala file's surface row.
pub const SURFACE_ROW_KEY_PREFIX: &str = "scala_surface:";

/// The `project_metadata` key of `file_path`'s surface row.
pub fn surface_row_key(file_path: &str) -> String {
    format!("{SURFACE_ROW_KEY_PREFIX}{file_path}")
}

/// A declaration a text fact holds for: its node id and name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ScalaFact {
    pub id: String,
    pub name: String,
}

/// A Scala type declaration's head, or a package object's, as the
/// hierarchy walks read it: the supertypes it names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ScalaHead {
    pub name: String,
    pub supertypes: Vec<String>,
}

/// What other files' Scala resolution reads of one Scala file, by the
/// readers resolution uses: its type heads (node order) and package objects
/// (text order), the names its own resolution walks start from, the
/// functions an `extension` line marks and the `val`s a block of a class
/// body scopes (sorted, by node id, so same-named declarations stay apart),
/// the package its package object opens and the holder its top-level types
/// have.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScalaSurface {
    pub heads: Vec<ScalaHead>,
    pub package_objects: Vec<ScalaHead>,
    pub starts: Vec<String>,
    pub extension_functions: Vec<ScalaFact>,
    pub block_locals: Vec<ScalaFact>,
    pub object_package: Option<String>,
    pub holder: Option<String>,
}

impl ScalaSurface {
    /// The row of the Scala file `path` whose text is `source` and whose
    /// nodes are `nodes`.
    pub fn of(path: &str, source: &str, nodes: &[Node]) -> Self {
        let facts = crate::source_facts::SourceFacts::new(Arc::from(source));
        let mut ordered: Vec<&Node> = nodes.iter().collect();
        ordered.sort_by(|a, b| {
            (a.start_line, a.start_column, &a.id).cmp(&(b.start_line, b.start_column, &b.id))
        });
        let mut surface = Self::default();
        let mut starts: BTreeSet<String> = BTreeSet::new();
        let mut extension_functions: BTreeSet<ScalaFact> = BTreeSet::new();
        let mut block_locals: BTreeSet<ScalaFact> = BTreeSet::new();
        for node in &ordered {
            if is_scala_type_kind(node.kind) {
                starts.insert(node.name.clone());
                if node.language == Language::Scala {
                    let supertypes = crate::class_heads::scala_declaration_supertypes(&facts, node);
                    if !supertypes.is_empty() {
                        surface.heads.push(ScalaHead {
                            name: node.name.clone(),
                            supertypes,
                        });
                    }
                }
            } else if node.kind == NodeKind::Function
                && is_scala_extension_method_in(&facts, node.start_line)
            {
                extension_functions.insert(ScalaFact {
                    id: node.id.clone(),
                    name: node.name.clone(),
                });
            } else if node.kind == NodeKind::Field
                && node.language == Language::Scala
                && scala_block_among(&facts, nodes.iter(), node).is_some()
            {
                block_locals.insert(ScalaFact {
                    id: node.id.clone(),
                    name: node.name.clone(),
                });
            }
        }
        if path.ends_with(".scala") {
            surface.package_objects = scala_package_object_heads(source)
                .into_iter()
                .filter(|(_, supertypes)| !supertypes.is_empty())
                .map(|(name, supertypes)| ScalaHead { name, supertypes })
                .collect();
        }
        starts.extend(scala_new_bases(source));
        starts.extend(ScalaImports::read(source).owners);
        starts.extend(
            package_clause_captures(source)
                .flat_map(|clause| clause.split('.').map(str::to_string)),
        );
        surface.starts = starts.into_iter().collect();
        surface.extension_functions = extension_functions.into_iter().collect();
        surface.block_locals = block_locals.into_iter().collect();
        surface.object_package = scala_package_object_package(source);
        surface.holder = scala_holder_package(source);
        surface
    }

    /// No fact: no row records it.
    pub fn is_empty(&self) -> bool {
        self.heads.is_empty()
            && self.package_objects.is_empty()
            && self.starts.is_empty()
            && self.extension_functions.is_empty()
            && self.block_locals.is_empty()
            && self.object_package.is_none()
            && self.holder.is_none()
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("scala surface row encodes")
    }

    pub fn decode(value: &str) -> Option<Self> {
        serde_json::from_str(value).ok()
    }
}

/// Every type a `new X(…) with Y` anywhere in `source` instantiates: what
/// [`scala_anonymous_bases`] can read at any site of the file, and more.
fn scala_new_bases(source: &str) -> Vec<String> {
    static NEW: OnceLock<Regex> = OnceLock::new();
    static ARGUMENTS: OnceLock<Regex> = OnceLock::new();
    static WITH: OnceLock<Regex> = OnceLock::new();
    let new = NEW.get_or_init(|| {
        Regex::new(
            r"(?-u:\b)new\s+([A-Za-z0-9_.]+(?:\s*\[[^\]]*\])?(?:\s*\([^)]*\))?(?:\s+with\s+[A-Za-z0-9_.]+(?:\s*\[[^\]]*\])?)*)",
        )
        .expect("new bases")
    });
    let arguments =
        ARGUMENTS.get_or_init(|| Regex::new(r"\[[^\]]*\]|\([^)]*\)").expect("arguments"));
    let with = WITH.get_or_init(|| Regex::new(r"\s+with\s+").expect("with"));
    let mut bases = Vec::new();
    for caps in new.captures_iter(source) {
        let written = arguments.replace_all(&caps[1], "");
        for base in with.split(&written) {
            bases.push(
                base.trim()
                    .rsplit('.')
                    .next()
                    .unwrap_or_default()
                    .to_string(),
            );
        }
    }
    bases
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receiver_names_drop_arguments_and_keep_dotted_links() {
        assert_eq!(scala_receiver_name("x = proc(\"bash\")."), "proc");
        assert_eq!(scala_receiver_name("Alternative[List]."), "Alternative");
        assert_eq!(scala_receiver_name("  checker.value."), "checker.value");
        assert_eq!(scala_receiver_name("gen[Int](1)."), "gen");
        assert_eq!(scala_receiver_name("(a + b)."), "");
        assert_eq!(scala_receiver_name("f(x))."), "");
        assert_eq!(scala_receiver_name("val y = fa.iterator ."), "fa.iterator");
    }

    #[test]
    fn a_name_occurrence_needs_a_boundary_on_both_sides() {
        assert_eq!(first_name_occurrence("xs.map(f).map(g)", "map"), Some(3));
        assert_eq!(first_name_occurrence("remap(f); map(g)", "map"), Some(10));
        assert_eq!(first_name_occurrence("mapped(f)", "map"), None);
        assert_eq!(first_name_occurrence("$map(f) map", "map"), Some(8));
    }

    #[test]
    fn binders_follow_upstreams_alternatives() {
        let binds = |text: &str, name: &str| !binder_starts(text, name).is_empty();
        assert!(binds("def tabulate[A](f: Boolean => A)", "f"));
        assert!(binds("(implicit ev: Eq[A])", "ev"));
        assert!(binds("(using  ctx: Ctx)", "ctx"));
        assert!(binds("val f = (i: Int) => i", "f"));
        assert!(binds("lazy  val f = 1", "f"));
        assert!(binds("xs.foreach { f => f }", "f"));
        assert!(binds("for { f <- fs } yield f", "f"));
        assert!(binds("forAll { (e: Int, f: Int => Int) => f(e) }", "f"));
        assert!(binds("xs.map((f, g) => f)", "f"));
        assert!(!binds("x.f => y", "f"));
        assert!(!binds("valf = 1", "f"));
        assert!(!binds("val ff = 1", "f"));
        assert!(!binds("(ff: Int)", "f"));
        assert!(!binds("f(true)", "f"));
        assert_eq!(binder_starts("val f = 1; val f = 2", "f"), [0, 11]);
    }

    #[test]
    fn package_clauses_skip_package_objects() {
        let text = "package cats\npackage laws\n\npackage object syntax extends Api\n";
        assert_eq!(scala_package_clauses(text), ["cats", "laws"]);
        assert_eq!(scala_holder_package(text).as_deref(), Some("laws"));
        assert_eq!(
            scala_package_object_package(text).as_deref(),
            Some("cats.laws.syntax")
        );
        assert_eq!(
            scala_package_object_package("package a.b\n\nobject C\n"),
            None
        );
        assert_eq!(
            scala_package_object_package("package object algebra {\n}\n").as_deref(),
            Some("algebra")
        );
        assert_eq!(
            scala_package_clauses("package objectives\n"),
            ["objectives"]
        );
        assert!(scala_package_clauses("package object\n").is_empty());
    }

    #[test]
    fn package_object_heads_name_their_supertypes() {
        assert_eq!(
            scala_package_object_heads("package app\n\npackage object api extends Api\n"),
            [("api".to_string(), vec!["Api".to_string()])]
        );
        assert_eq!(
            scala_package_object_heads(
                "package object client4 extends SttpApi[F] with sttp.Other(1) with Third {\n}\n"
            ),
            [(
                "client4".to_string(),
                vec![
                    "SttpApi".to_string(),
                    "Other".to_string(),
                    "Third".to_string()
                ]
            )]
        );
        assert!(scala_package_object_heads("package  object x extends A\n").is_empty());
    }

    #[test]
    fn imports_read_owners_members_and_values() {
        let source = "import app.syntax.all._\nimport a.b.Obj.{x, y => z, _}\nimport c.Tools.make\nimport builder._\nimport cfg.nested.*\nval cfg = load()\n";
        let imports = ScalaImports::read(source);
        assert_eq!(imports.owners, ["all", "Obj", "builder", "nested"]);
        assert_eq!(
            imports.members,
            HashSet::from([
                "Obj.x".to_string(),
                "Obj.y".to_string(),
                "Tools.make".to_string()
            ])
        );
        assert_eq!(
            imports.values,
            HashSet::from(["builder".to_string(), "nested".to_string()])
        );
    }

    #[test]
    fn symbolic_names_are_all_operator_characters() {
        assert!(is_symbolic_name("*"));
        assert!(is_symbolic_name("~>"));
        assert!(!is_symbolic_name("A"));
        assert!(!is_symbolic_name("a_*"));
        assert!(!is_symbolic_name(""));
    }
}
