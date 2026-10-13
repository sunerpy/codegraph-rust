//! Ruby calls reached by name alone (upstream #2147, in the shape at
//! `b635dd46`): Ruby types nothing, so a bare `m()` written inside a class is
//! a call on `self` and reaches only the class's ancestry, its superclasses'
//! and the modules any of them `include`s, `extend`s or `prepend`s.
//!
//! The ancestry is read from the declarations' source, as upstream reads it:
//! the superclass from a declaration's first line, the mixins from every line
//! of its body. A first resolution pass has no `extends` edges yet.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::csharp_types::js_regex;
use crate::source_facts::SourceFacts;
use crate::types::{RefView, ResolutionContext};

/// How many classes and modules one ancestry walk visits (upstream's 60).
const ANCESTRY_LIMIT: usize = 60;

/// The per-pass memo of the ancestry walk (upstream `RUBY_ANCESTRY`).
#[derive(Debug, Default)]
pub struct RubyTables {
    ancestry: Mutex<HashMap<String, Arc<HashSet<String>>>>,
}

impl RubyTables {
    pub fn new() -> Self {
        Self::default()
    }
}

fn is_ruby_type(node: &Node) -> bool {
    node.language == Language::Ruby && matches!(node.kind, NodeKind::Class | NodeKind::Module)
}

/// `^[A-Za-z_]\w*[?!]?$`: a call's name with no receiver written into it.
fn is_bare_method_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    let rest = chars.as_str();
    let rest = rest.strip_suffix(['?', '!']).unwrap_or(rest);
    rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether `reference` is a bare Ruby call, whose candidates
/// [`is_ruby_method_in_scope`] judges.
pub(crate) fn is_ruby_bare_call(reference: &RefView) -> bool {
    reference.language == Language::Ruby
        && reference.reference_kind == EdgeKind::Calls
        && is_bare_method_name(&reference.reference_name)
}

/// `^\s*class\s+[\w:]+\s*<\s*(::)?([A-Z][\w:]*)`, JavaScript's `\s` and
/// `\w`.
fn superclass_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| js_regex(r"^\s*class\s+[A-Za-z0-9_:]+\s*<\s*(::)?([A-Z][A-Za-z0-9_:]*)"))
}

/// `^\s*(?:include|extend|prepend)\s+([A-Z:][\w:]*(?:\s*,\s*[A-Z:][\w:]*)*)`.
fn mixin_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        js_regex(
            r"^\s*(?:include|extend|prepend)\s+([A-Z:][A-Za-z0-9_:]*(?:\s*,\s*[A-Z:][A-Za-z0-9_:]*)*)",
        )
    })
}

/// `/\s*,\s*/`.
fn comma_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| js_regex(r"\s*,\s*"))
}

/// What a Ruby class or module declaration's source names as its ancestors:
/// the superclass its first line writes (`::` kept when it roots the name),
/// and every mixin a line of its body writes, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RubyHead {
    pub superclass: Option<String>,
    pub mixins: Vec<String>,
}

impl RubyHead {
    /// The head of the declaration spanning `start_line..=end_line`
    /// (1-based) in `facts` (upstream reads `lines[startLine - 1]` and
    /// `lines.slice(startLine, endLine)`).
    pub(crate) fn read(facts: &SourceFacts, start_line: i64, end_line: i64) -> Self {
        let line_count = facts.line_count();
        let line = |number: i64| -> Option<&str> {
            let index = usize::try_from(number - 1).ok()?;
            (index < line_count).then(|| facts.line(index))
        };
        let superclass = line(start_line).and_then(|first| {
            superclass_pattern().captures(first).map(|captures| {
                let name = captures.get(2).map_or("", |m| m.as_str());
                if captures.get(1).is_some() {
                    format!("::{name}")
                } else {
                    name.to_string()
                }
            })
        });
        let mut mixins = Vec::new();
        for number in (start_line + 1)..=end_line {
            let Some(text) = line(number) else {
                break;
            };
            let Some(captures) = mixin_pattern().captures(text) else {
                continue;
            };
            let names = captures.get(1).map_or("", |m| m.as_str());
            mixins.extend(comma_pattern().split(names).map(str::to_string));
        }
        Self { superclass, mixins }
    }

    /// Every name the head writes.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.superclass
            .iter()
            .chain(&self.mixins)
            .map(String::as_str)
    }
}

/// The class or module a constant written inside `scope` names: the nearest
/// enclosing namespace that has it, else the name itself (upstream
/// `rubyConstantQn`).
fn constant_qualified_name(name: &str, scope: &str, context: &dyn ResolutionContext) -> String {
    let mut prefix = scope;
    loop {
        let qualified = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}::{name}")
        };
        if context
            .get_nodes_by_qualified_name_shared(&qualified)
            .iter()
            .any(|node| is_ruby_type(node))
        {
            return qualified;
        }
        if prefix.is_empty() {
            return name.to_string();
        }
        prefix = prefix.rsplit_once("::").map_or("", |(outer, _)| outer);
    }
}

/// A Ruby class's qualified name, those of its superclasses, and of every
/// module mixed into any of them (upstream `rubyAncestry`, at most 60). The
/// declarations of one name are read in path order, so the limit cuts the
/// same names on every index (upstream reads them in index order;
/// KEEP-RUST, as the Java walk).
fn ancestry(qualified_name: &str, context: &dyn ResolutionContext) -> Arc<HashSet<String>> {
    let tables = context.ruby_tables();
    if let Some(hit) = tables
        .ancestry
        .lock()
        .ok()
        .and_then(|memo| memo.get(qualified_name).cloned())
    {
        return hit;
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::from([qualified_name.to_string()]);
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
            .filter(|node| is_ruby_type(node))
            .collect();
        declarations.sort_by(|a, b| {
            (&a.file_path, a.start_line, a.start_column, &a.id).cmp(&(
                &b.file_path,
                b.start_line,
                b.start_column,
                &b.id,
            ))
        });
        let outer = current.rsplit_once("::").map_or("", |(outer, _)| outer);
        for declaration in &declarations {
            let Some(facts) = context.source_facts(&declaration.file_path) else {
                continue;
            };
            let head = RubyHead::read(&facts, declaration.start_line, declaration.end_line);
            if let Some(superclass) = &head.superclass {
                queue.push_back(match superclass.strip_prefix("::") {
                    Some(rooted) => constant_qualified_name(rooted, "", context),
                    None => constant_qualified_name(superclass, outer, context),
                });
            }
            for mixin in &head.mixins {
                queue.push_back(match mixin.strip_prefix("::") {
                    Some(rooted) => constant_qualified_name(rooted, "", context),
                    None => constant_qualified_name(mixin, &current, context),
                });
            }
        }
    }
    let seen = Arc::new(seen);
    if let Ok(mut memo) = tables.ancestry.lock() {
        memo.insert(qualified_name.to_string(), Arc::clone(&seen));
    }
    seen
}

/// Whether a bare Ruby call written inside a class body can mean `method`
/// (upstream `isRubyMethodInScope`): a receiver-less call is a call on
/// `self`, so it reaches the class's own methods, its superclasses', and
/// those of the modules any of them mixes in, resolved against the lexical
/// nesting (`class Foo < Base` inside `module RuboCop::Cop` is
/// `RuboCop::Cop::Base`). A call in a module body, in a block at the top of
/// a file (a spec, a DSL) or in a script is not judged: a module's methods
/// run on whatever includes it. rubocop's `format(…)`, Kernel's, went to the
/// LSP runtime's `format` 411 times.
pub(crate) fn is_ruby_method_in_scope(
    method: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    let Some((owner, _)) = method.qualified_name.rsplit_once("::") else {
        return true;
    };
    let line = reference.line;
    let here = context
        .get_nodes_in_file_shared(&reference.file_path)
        .into_iter()
        .filter(|node| {
            matches!(node.kind, NodeKind::Class | NodeKind::Module)
                && node.start_line <= line
                && node.end_line >= line
        })
        .min_by_key(|node| node.end_line - node.start_line);
    let Some(here) = here.filter(|here| here.kind == NodeKind::Class) else {
        return true;
    };
    ancestry(&here.qualified_name, context).contains(owner)
}

/// Languages whose receivers nothing types, where a unique method name alone
/// is no evidence (upstream `UNTYPED_RECEIVER_LANGUAGES`: Ruby since
/// W4-RB-01, CFML since W4-CF-01, the others with their items). CFML's
/// `server.keyExists()` is the struct member function, not the one component
/// method named `keyExists`.
pub(crate) fn is_untyped_receiver_language(language: Language) -> bool {
    matches!(language, Language::Ruby | Language::Cfml)
}

/// The receivers the untyped-receiver gate never judges (upstream
/// `/^(?:self|self\.class|this|super|weak_?self|strong_?self)$/i`).
pub(crate) fn is_self_receiver(receiver: &str) -> bool {
    let lower = receiver.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "self"
            | "self.class"
            | "this"
            | "super"
            | "weakself"
            | "weak_self"
            | "strongself"
            | "strong_self"
    )
}

/// Key prefix of the Ruby head rows (W4-RB-01): one `project_metadata` row
/// per Ruby file with a class or module, written by every full index and
/// migration and kept current by sync.
pub const HEAD_ROW_KEY_PREFIX: &str = "ruby_heads:";

/// The `project_metadata` key of `file_path`'s head row.
pub fn head_row_key(file_path: &str) -> String {
    format!("{HEAD_ROW_KEY_PREFIX}{file_path}")
}

/// One Ruby class or module declaration, as the ancestry walk reads it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HeadEntry {
    pub qualified_name: String,
    pub kind: String,
    #[serde(flatten)]
    pub head: RubyHead,
}

/// A Ruby file's head row: every class and module declaration, in the order
/// of the declarations in the file. A declaration that names nothing is
/// still an entry: its name is one a written constant can resolve to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadRow {
    pub entries: Vec<HeadEntry>,
}

impl HeadRow {
    /// The row of a file whose text is `source` and whose nodes are `nodes`,
    /// read by [`RubyHead::read`].
    pub fn of(source: &str, nodes: &[Node]) -> Self {
        let facts = SourceFacts::new(Arc::from(source));
        let mut declarations: Vec<&Node> = nodes.iter().filter(|node| is_ruby_type(node)).collect();
        declarations.sort_by(|a, b| {
            (a.start_line, a.start_column, &a.id).cmp(&(b.start_line, b.start_column, &b.id))
        });
        let entries = declarations
            .into_iter()
            .map(|node| HeadEntry {
                qualified_name: node.qualified_name.clone(),
                kind: node.kind.as_str().to_string(),
                head: RubyHead::read(&facts, node.start_line, node.end_line),
            })
            .collect();
        Self { entries }
    }

    /// No entry: no row records it.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("ruby head row encodes")
    }

    pub fn decode(value: &str) -> Option<Self> {
        serde_json::from_str(value).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(source: &str, start_line: i64, end_line: i64) -> RubyHead {
        RubyHead::read(&SourceFacts::new(Arc::from(source)), start_line, end_line)
    }

    #[test]
    fn a_head_reads_the_superclass_and_every_mixin_line_of_the_body() {
        let source = "class Foo < Base\n  include RangeHelp, ::Kernel\n  extend Forwardable\n  prepend Mod::Inner\n  def x; include_me; end\nend\n";
        assert_eq!(
            head(source, 1, 6),
            RubyHead {
                superclass: Some("Base".to_string()),
                mixins: vec![
                    "RangeHelp".to_string(),
                    "::Kernel".to_string(),
                    "Forwardable".to_string(),
                    "Mod::Inner".to_string(),
                ],
            }
        );
        let rooted = head("class Foo < ::App::Base\nend\n", 1, 2);
        assert_eq!(rooted.superclass.as_deref(), Some("::App::Base"));
        // A lowercase constant, a module head and a mixin on the first line
        // are not read.
        assert_eq!(head("class Foo < base\nend\n", 1, 2), RubyHead::default());
        assert_eq!(
            head("module M; include X\nend\n", 1, 2),
            RubyHead::default()
        );
    }

    #[test]
    fn a_bare_method_name_has_no_receiver_and_may_end_in_a_question_or_bang() {
        assert!(is_bare_method_name("format"));
        assert!(is_bare_method_name("empty?"));
        assert!(is_bare_method_name("save!"));
        assert!(is_bare_method_name("_private"));
        assert!(!is_bare_method_name("node.loc"));
        assert!(!is_bare_method_name("a?b"));
        assert!(!is_bare_method_name("9lives"));
        assert!(!is_bare_method_name(""));
    }

    #[test]
    fn self_receivers_are_never_judged() {
        for receiver in [
            "self",
            "self.class",
            "SELF",
            "this",
            "super",
            "weakSelf",
            "strong_self",
        ] {
            assert!(is_self_receiver(receiver), "{receiver}");
        }
        assert!(!is_self_receiver("node"));
        assert!(!is_self_receiver("self.node"));
    }

    #[test]
    fn head_rows_round_trip_and_keep_declarations_in_file_order() {
        let row = HeadRow {
            entries: vec![HeadEntry {
                qualified_name: "App::Foo".to_string(),
                kind: "class".to_string(),
                head: RubyHead {
                    superclass: Some("Base".to_string()),
                    mixins: vec!["RangeHelp".to_string()],
                },
            }],
        };
        assert_eq!(HeadRow::decode(&row.encode()), Some(row));
        assert_eq!(HeadRow::decode("not json"), None);
    }
}
