//! Solidity calls written without a receiver (upstream #2178 (a), in the
//! shape at `b635dd46`): a bare call reaches a function of the contract,
//! interface or library around it, or of one it inherits through its `is`
//! clause, or a free function; never another contract's override.
//! OpenZeppelin's `_msgSender()` in a Context-derived Governor went to
//! ERC2771Context's override 84 times.
//!
//! The `is` chain is read from the declarations' heads, as upstream reads
//! them: by the names they write, every declaration of a name counted.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

use codegraph_core::generated_header::is_js_whitespace;
use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::types::{RefView, ResolutionContext};

/// How many type names one hierarchy holds (upstream's 60).
pub const HIERARCHY_LIMIT: usize = 60;

/// How many lines after a declaration's first its head is read from
/// (upstream reads `lines.slice(startLine - 1, startLine + 10)`).
const HEAD_LINES: i64 = 10;

/// A hierarchy by the file and line it is read at.
type Hierarchies = HashMap<(String, i64), Arc<HashSet<String>>>;

/// The per-pass memos of the `is` chains (upstream `SOLIDITY_SUPERS` and
/// `SOLIDITY_HIERARCHIES`).
#[derive(Debug, Default)]
pub struct SolidityTables {
    supers: Mutex<HashMap<String, Arc<Vec<String>>>>,
    hierarchies: Mutex<Hierarchies>,
}

impl SolidityTables {
    pub fn new() -> Self {
        Self::default()
    }
}

/// The kinds a Solidity contract, interface, library or struct is
/// (upstream `SOLIDITY_TYPE_KINDS`).
pub fn is_solidity_type_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Interface
            | NodeKind::Struct
            | NodeKind::Module
            | NodeKind::Trait
    )
}

/// `^[A-Za-z_]\w*$`: a call's name with no receiver written into it.
fn is_bare_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// An ASCII word character (JavaScript's `\w`).
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `//[^\n]*|/\*[\s\S]*?\*/`: the comments of a head.
fn comment_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN
        .get_or_init(|| Regex::new(r"//[^\n]*|/\*(?s:.)*?\*/").expect("Solidity comment pattern"))
}

/// The supertypes a Solidity declaration's head names (upstream
/// `soliditySupertypesOf`, one declaration's share): `head` is its first
/// line and the ten after it, joined by newlines. Comments are dropped, then
/// the constructor arguments of a base (`ERC165(…)`), and the head ends at
/// the body's `{`; every name the `is` clause lists follows, a dotted one by
/// its last name (`B.Pausable` is `Pausable`). KEEP-RUST: a `//` comment
/// drops only the rest of its own line; upstream joins the lines with spaces
/// first, so a comment inside a head drops the bases written below it.
pub fn written_supertypes(head: &str) -> Vec<String> {
    let text = comment_pattern().replace_all(head, " ").replace('\n', " ");
    let mut depth = 0usize;
    let mut kept = String::new();
    for c in text.chars() {
        if c == '{' && depth == 0 {
            break;
        }
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => kept.push(c),
            _ => {}
        }
    }
    is_clause(&kept).map(listed_names).unwrap_or_default()
}

/// What follows the first `is` of a head (`/\bis\b([\s\S]*)$/`).
fn is_clause(head: &str) -> Option<&str> {
    let bytes = head.as_bytes();
    head.match_indices("is")
        .find(|(at, _)| {
            (*at == 0 || !is_word_byte(bytes[at - 1]))
                && bytes.get(at + 2).is_none_or(|b| !is_word_byte(*b))
        })
        .map(|(at, _)| &head[at + 2..])
}

/// The names an `is` clause lists (`/([A-Za-z_]\w*)\s*(?=,|$)/g`): each
/// word followed by a comma or the clause's end, from its first letter or
/// underscore.
fn listed_names(clause: &str) -> Vec<String> {
    let bytes = clause.as_bytes();
    let mut names = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if !is_word_byte(bytes[at]) {
            at += 1;
            continue;
        }
        let start = at;
        while at < bytes.len() && is_word_byte(bytes[at]) {
            at += 1;
        }
        let rest = clause[at..].trim_start_matches(is_js_whitespace);
        if (rest.is_empty() || rest.starts_with(','))
            && let Some(first) =
                clause[start..at].find(|c: char| c.is_ascii_alphabetic() || c == '_')
        {
            names.push(clause[start + first..at].to_string());
        }
    }
    names
}

/// A declaration's head as [`written_supertypes`] reads it: its first line
/// and the ten after it, `\r` dropped, joined by newlines, from `line` (a
/// 0-based line's text).
fn head_of<'a>(start_line: i64, line: impl Fn(usize) -> Option<&'a str>) -> String {
    let mut out = Vec::new();
    for number in start_line.max(1)..=start_line + HEAD_LINES {
        let Some(text) = usize::try_from(number - 1).ok().and_then(&line) else {
            break;
        };
        out.push(text.trim_end_matches('\r'));
    }
    out.join("\n")
}

/// What the Solidity declarations named `name` inherit, every one of them
/// read (upstream `soliditySupertypesOf`).
fn supertypes_of(name: &str, context: &dyn ResolutionContext) -> Arc<Vec<String>> {
    let tables = context.solidity_tables();
    if let Some(hit) = tables
        .supers
        .lock()
        .ok()
        .and_then(|memo| memo.get(name).cloned())
    {
        return hit;
    }
    let mut supers: Vec<String> = Vec::new();
    for decl in context.get_nodes_by_name_shared(name) {
        if decl.language != Language::Solidity || !is_solidity_type_kind(decl.kind) {
            continue;
        }
        let Some(facts) = context.source_facts(&decl.file_path) else {
            continue;
        };
        supers.extend(written_supertypes(&head_of(decl.start_line, |index| {
            facts.raw_line(index)
        })));
    }
    let supers = Arc::new(supers);
    if let Ok(mut memo) = tables.supers.lock() {
        memo.insert(name.to_string(), Arc::clone(&supers));
    }
    supers
}

/// The names of the types around `reference` and of every type they
/// inherit, at most 60 (upstream's per-reference memo, here per line).
fn hierarchy_at(reference: &RefView, context: &dyn ResolutionContext) -> Arc<HashSet<String>> {
    let tables = context.solidity_tables();
    let key = (reference.file_path.clone(), reference.line);
    if let Some(hit) = tables
        .hierarchies
        .lock()
        .ok()
        .and_then(|memo| memo.get(&key).cloned())
    {
        return hit;
    }
    let mut queue: VecDeque<String> = context
        .get_nodes_in_file_shared(&reference.file_path)
        .iter()
        .filter(|node| {
            is_solidity_type_kind(node.kind)
                && node.start_line <= reference.line
                && node.end_line >= reference.line
        })
        .map(|node| node.name.clone())
        .collect();
    let mut names: HashSet<String> = HashSet::new();
    while names.len() < HIERARCHY_LIMIT {
        let Some(name) = queue.pop_front() else {
            break;
        };
        if names.contains(&name) {
            continue;
        }
        queue.extend(supertypes_of(&name, context).iter().cloned());
        names.insert(name);
    }
    let names = Arc::new(names);
    if let Ok(mut memo) = tables.hierarchies.lock() {
        memo.insert(key, Arc::clone(&names));
    }
    names
}

/// Whether a Solidity call is written with no receiver (upstream
/// `isReceiverLessSolidityCall`): `_msgSender()`, not `token._msgSender()`.
/// The line's first `name(` not inside a longer name is read, as upstream
/// reads it, so a modifier written without arguments (`onlyOwner`) is not
/// judged. This extractor writes a call's receiver into its name
/// (`super.f`, `SafeCast.toUint48`), so the `.` before `name(` matters only
/// when the same name is called through a receiver earlier on the line.
pub(crate) fn is_receiver_less_solidity_call(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    if reference.language != Language::Solidity
        || reference.reference_kind != EdgeKind::Calls
        || !is_bare_name(&reference.reference_name)
    {
        return false;
    }
    let Some(facts) = context.source_facts(&reference.file_path) else {
        return false;
    };
    let Some(line) = usize::try_from(reference.line - 1)
        .ok()
        .and_then(|index| facts.raw_line(index))
    else {
        return false;
    };
    let name = reference.reference_name.as_str();
    let bytes = line.as_bytes();
    line.match_indices(name)
        .find(|(at, _)| {
            (*at == 0 || !(is_word_byte(bytes[at - 1]) || bytes[at - 1] == b'$'))
                && line[at + name.len()..]
                    .trim_start_matches(is_js_whitespace)
                    .starts_with('(')
        })
        .is_some_and(|(at, _)| !line[..at].trim_end_matches(is_js_whitespace).ends_with('.'))
}

/// Whether a bare Solidity call can mean `node` (upstream
/// `isSolidityMemberInScope`): a function of a contract around the call or
/// of one it inherits; anything but a Solidity method (a free function) is
/// not judged.
pub(crate) fn is_solidity_member_in_scope(
    node: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    solidity_method_owner(node).is_none_or(|owner| hierarchy_at(reference, context).contains(owner))
}

/// The type a Solidity method belongs to, by the last name of the path its
/// qualified name is under; `None` for anything else, which a bare call's
/// chain does not judge.
pub fn solidity_method_owner(node: &Node) -> Option<&str> {
    if node.kind != NodeKind::Method || node.language != Language::Solidity {
        return None;
    }
    let (path, _) = node.qualified_name.rsplit_once("::")?;
    (!path.is_empty()).then(|| path.rsplit("::").next().unwrap_or(path))
}

/// Key prefix of the Solidity head rows (W4-SOL-01): one `project_metadata`
/// row per Solidity file with a type, written by every full index and
/// migration and kept current by sync.
pub const HEAD_ROW_KEY_PREFIX: &str = "solidity_heads:";

/// The `project_metadata` key of `file_path`'s head row.
pub fn head_row_key(file_path: &str) -> String {
    format!("{HEAD_ROW_KEY_PREFIX}{file_path}")
}

/// One type declaration of a Solidity file: its name and what its head
/// inherits, as [`written_supertypes`] reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HeadEntry {
    pub name: String,
    pub supers: Vec<String>,
}

/// What the `is` chains read of a Solidity file: an entry per type it
/// declares, sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadRow {
    pub types: Vec<HeadEntry>,
}

impl HeadRow {
    /// The row of a Solidity file whose text is `source` and whose nodes are
    /// `nodes`.
    pub fn of(source: &str, nodes: &[Node]) -> Self {
        let lines: Vec<&str> = source.split('\n').collect();
        let mut types: Vec<HeadEntry> = nodes
            .iter()
            .filter(|node| node.language == Language::Solidity && is_solidity_type_kind(node.kind))
            .map(|node| HeadEntry {
                name: node.name.clone(),
                supers: written_supertypes(&head_of(node.start_line, |index| {
                    lines.get(index).copied()
                })),
            })
            .collect();
        types.sort();
        Self { types }
    }

    /// Nothing for the chains to read: no type.
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("Solidity head row encodes")
    }

    pub fn decode(value: &str) -> Option<Self> {
        serde_json::from_str(value).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn supers(head: &str) -> Vec<String> {
        written_supertypes(head)
    }

    #[test]
    fn an_is_clause_is_read_as_written() {
        assert_eq!(
            supers("abstract contract Governor is Context, ERC165, EIP712, Nonces, IGovernor {"),
            ["Context", "ERC165", "EIP712", "Nonces", "IGovernor"]
        );
        assert_eq!(
            supers("contract Token is ERC20(\"Token\", \"TKN\"), Ownable(msg.sender) {"),
            ["ERC20", "Ownable"]
        );
        assert_eq!(supers("contract A is B.Pausable {"), ["Pausable"]);
        assert_eq!(
            supers(
                "contract Vault is\n    Ownable(msg.sender), // the owner\n    B.Pausable,\n    /* nonces */ Middle\n{"
            ),
            ["Ownable", "Pausable", "Middle"]
        );
        assert_eq!(
            supers("library Math {\n    function add() {}\n}"),
            Vec::<String>::new()
        );
        assert_eq!(supers("interface IThis is IERC165 {"), ["IERC165"]);
        // A name merely starting with `is` is no clause.
        assert_eq!(supers("contract isolated {"), Vec::<String>::new());
        assert_eq!(supers("contract A is /* B, */ C {"), ["C"]);
    }

    #[test]
    fn a_head_row_lists_each_type_with_its_bases() {
        let source = "pragma solidity ^0.8.20;\n\ncontract B is A {}\n\ninterface I {}\n";
        let node = |kind: NodeKind, name: &str, line: i64| Node {
            id: format!("{}:{name}", kind.as_str()),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: "x.sol".to_string(),
            language: Language::Solidity,
            start_line: line,
            end_line: line,
            start_column: 0,
            end_column: 1,
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
        };
        let row = HeadRow::of(
            source,
            &[
                node(NodeKind::Interface, "I", 5),
                node(NodeKind::Class, "B", 3),
                node(NodeKind::Method, "m", 3),
            ],
        );
        assert_eq!(
            row.types,
            [
                HeadEntry {
                    name: "B".to_string(),
                    supers: vec!["A".to_string()],
                },
                HeadEntry {
                    name: "I".to_string(),
                    supers: Vec::new(),
                },
            ]
        );
        assert_eq!(HeadRow::decode(&row.encode()), Some(row));
    }
}
