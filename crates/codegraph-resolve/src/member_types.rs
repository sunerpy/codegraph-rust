//! The type a C# / Java / Kotlin receiver is declared with as a field or
//! property of the class around a call, or of a class it inherits (upstream
//! #2171 `inferMemberReceiverType`, in #2181's shape): Newtonsoft's
//! `private readonly JsonWriter _innerWriter;` makes `_innerWriter.WriteValue(…)`
//! JsonWriter's, never TraceJsonWriter's own `WriteValue` guessed by name.
//!
//! Only a class's member-level lines are read (brace depth one, or its head
//! for a primary constructor), never a method body or a nested type. The walk
//! into inherited classes carries what their type parameters stand for
//! (`: IntegrationTest<DatabaseInitializer>`), and a member typed by a type
//! parameter of the class declaring it is its bound when the subclass gives
//! no argument. Every read is memoized for the pass in [`MemberTables`].

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

use codegraph_core::generated_header::is_js_whitespace;
use codegraph_core::types::{Language, Node, NodeKind};
use regex::Regex;

use crate::types::{RefView, ResolutionContext};

/// The languages whose fields and properties declare their type where the
/// class declares them (upstream `MEMBER_TYPED_LANGUAGES` as #2181 left it;
/// Swift joins with #2205).
pub fn is_member_typed_language(language: Language) -> bool {
    matches!(
        language,
        Language::CSharp | Language::Java | Language::Kotlin
    )
}

/// Upstream `MEMBER_CLASS_KINDS` (`record` has no kind of its own here).
pub fn is_member_class_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class | NodeKind::Struct | NodeKind::Interface | NodeKind::Enum
    )
}

/// Words that can stand where a declaration's type does without being one
/// (upstream `MEMBER_TYPE_NON_TYPES`).
const MEMBER_TYPE_NON_TYPES: [&str; 46] = [
    "return",
    "new",
    "case",
    "throw",
    "else",
    "in",
    "out",
    "ref",
    "params",
    "await",
    "yield",
    "is",
    "as",
    "using",
    "var",
    "val",
    "goto",
    "nameof",
    "typeof",
    "sizeof",
    "default",
    "when",
    "where",
    "get",
    "set",
    "init",
    "class",
    "interface",
    "enum",
    "struct",
    "record",
    "object",
    "namespace",
    "package",
    "import",
    "extends",
    "implements",
    "fun",
    "static",
    "final",
    "abstract",
    "sealed",
    "override",
    "virtual",
    "delegate",
    "event",
];

/// A Java / C# type's optional type arguments (one level of nesting) and
/// array ranks, as a regex source: `<L, R>`, `<string, List<int>>`, `[]`,
/// `[,]` (upstream `TYPE_ARGS`).
pub(crate) const TYPE_ARGS: &str =
    r"(?:<[^;=(){}<>]*(?:<[^;=(){}<>]*>[^;=(){}<>]*)*>)?\s*(?:\[[\s,]*\]\s*)*";

/// What a type parameter of a declaration around a call is bounded by
/// (upstream `typeParameterBound`'s `string | null | undefined`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TypeParameterBound {
    /// Not a type parameter there at all.
    NotParameter,
    /// A type parameter with no named bound.
    Unbounded,
    /// A type parameter bounded by this type (its last `.` segment).
    Bound(String),
}

/// A class's member-declaration lines, each with its brace depth.
type MemberLines = Arc<Vec<(String, usize)>>;

/// The per-pass memos of the member walk (upstream #2181's `MEMBER_LINES`,
/// `MEMBER_TYPE_MEMO`, `MEMBER_WALKS` and `MEMBER_SHADOWS`).
#[derive(Debug, Default)]
pub struct MemberTables {
    lines: Mutex<HashMap<String, MemberLines>>,
    member_types: Mutex<HashMap<(String, String), Option<String>>>,
    walks: Mutex<HashMap<(String, String), Option<String>>>,
    shadows: Mutex<HashMap<(String, String), bool>>,
}

impl MemberTables {
    pub fn new() -> Self {
        Self::default()
    }
}

fn memo_get<K: std::hash::Hash + Eq, V: Clone>(memo: &Mutex<HashMap<K, V>>, key: &K) -> Option<V> {
    memo.lock().ok().and_then(|memo| memo.get(key).cloned())
}

fn memo_set<K: std::hash::Hash + Eq, V>(memo: &Mutex<HashMap<K, V>>, key: K, value: V) {
    if let Ok(mut memo) = memo.lock() {
        memo.insert(key, value);
    }
}

/// `^[A-Za-z_]\w*$`.
fn is_simple_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `lines.slice(start - 1, end).join(separator)` of `node`'s file, the lines
/// as `split(/\r?\n/)` yields them, clamped to the file.
fn joined_lines(
    node: &Node,
    end: usize,
    separator: &str,
    context: &dyn ResolutionContext,
) -> String {
    let Some(facts) = context.source_facts(&node.file_path) else {
        return String::new();
    };
    let start = usize::try_from(node.start_line)
        .unwrap_or(1)
        .saturating_sub(1);
    let end = end.min(facts.line_count());
    if start >= end {
        return String::new();
    }
    facts.join_lines(start, end, separator)
}

/// The head of `node`'s declaration: `lines` lines from its first, joined with
/// a space, cut at the first `{`.
fn head_before_brace(node: &Node, lines: usize, context: &dyn ResolutionContext) -> String {
    let start = usize::try_from(node.start_line).unwrap_or(1);
    let head = joined_lines(node, start.saturating_sub(1) + lines, " ", context);
    head.split('{').next().unwrap_or_default().to_string()
}

/// The type a C# / Java / Kotlin receiver has as a field or property of the
/// class around the call, or of one it inherits, or `None` when no class
/// declares such a member or the calling function binds the name itself
/// (upstream `inferMemberReceiverType`).
pub(crate) fn infer_member_receiver_type(
    receiver: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let name = receiver.strip_prefix("this.").unwrap_or(receiver);
    if !is_simple_name(name) {
        return None;
    }
    let mut class: Option<Arc<Node>> = None;
    let mut function: Option<Arc<Node>> = None;
    for node in context.get_nodes_in_file_shared(&reference.file_path) {
        if node.language != reference.language
            || node.start_line > reference.line
            || node.end_line < reference.line
        {
            continue;
        }
        if is_member_class_kind(node.kind) {
            if class
                .as_ref()
                .is_none_or(|class| node.start_line >= class.start_line)
            {
                class = Some(node);
            }
        } else if matches!(node.kind, NodeKind::Method | NodeKind::Function)
            && function
                .as_ref()
                .is_none_or(|function| node.start_line >= function.start_line)
        {
            function = Some(node);
        }
    }
    let class = class?;
    let found = member_type_through_hierarchy(&class, name, context)?;
    // A lambda parameter, `var` local, `out` variable or `foreach` binding in
    // the calling function shadows the member.
    if function
        .as_ref()
        .is_some_and(|function| binds_name_itself(function, name, context))
    {
        return None;
    }
    Some(found)
}

/// Whether a C# / Java / Kotlin function body binds `name` itself: a
/// `var`/`val`, an `out` or loop variable, a lambda parameter (upstream
/// `bindsNameItself`, over the whole function as #2181 reads it).
fn binds_name_itself(function: &Node, name: &str, context: &dyn ResolutionContext) -> bool {
    let tables = context.member_tables();
    let key = (function.id.clone(), name.to_string());
    if let Some(hit) = memo_get(&tables.shadows, &key) {
        return hit;
    }
    let end = usize::try_from(function.end_line).unwrap_or(0);
    let body = joined_lines(function, end, "\n", context);
    let r = regex::escape(name);
    let w = "[A-Za-z0-9_]";
    let binds = Regex::new(&format!(
        r"(?-u:\b)(?:var|val|out\s+[{w}.<>?]+|foreach\s*\(\s*[{w}.<>?,\s]+?)\s+{r}(?-u:\b)|(?-u:\b)for\s*\([^;)]*\s{r}\s*:|(?-u:\b){r}\s*=>|[(,]\s*{r}\s*(?:,[^()]*)?\)\s*=>|(?-u:\b){r}\s*(?:,[^{{}}]*)?->"
    ))
    .is_ok_and(|binds| binds.is_match(&body));
    memo_set(&tables.shadows, key, binds);
    binds
}

/// The type `class` declares a member `name` with, or one of its supertypes
/// does, its own members first, each inherited class carrying what its type
/// parameters stand for in the class the walk came from (upstream
/// `memberTypeThroughHierarchy`, at most eight types).
fn member_type_through_hierarchy(
    class: &Node,
    name: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let tables = context.member_tables();
    let key = (class.id.clone(), name.to_string());
    if let Some(hit) = memo_get(&tables.walks, &key) {
        return hit;
    }
    let mut result = None;
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(Arc<Node>, HashMap<String, String>)> = VecDeque::new();
    queue.push_back((Arc::new(class.clone()), HashMap::new()));
    while seen.len() < 8 {
        let Some((owner, args)) = queue.pop_front() else {
            break;
        };
        if !seen.insert(owner.id.clone()) {
            continue;
        }
        if let Some(found) = class_member_type(&owner, name, context) {
            if owner.id == class.id {
                result = Some(found);
                break;
            }
            // A member typed by the declaring class's own type parameter
            // (`protected TFixture Fixture { get; }`) is the argument the
            // subclass gave it, else its bound; with neither, only `object`'s
            // members.
            if let Some(given) = args.get(&found) {
                result = Some(given.clone());
                break;
            }
            result = Some(
                match type_parameter_bound_in(&found, std::slice::from_ref(&owner), context) {
                    TypeParameterBound::NotParameter => found,
                    TypeParameterBound::Unbounded => "object".to_string(),
                    TypeParameterBound::Bound(bound) => bound,
                },
            );
            break;
        }
        for sup in crate::class_heads::class_head_supertypes(&owner, context) {
            let given: Vec<String> = head_type_arguments(&owner, &sup, context)
                .into_iter()
                .map(|argument| args.get(&argument).cloned().unwrap_or(argument))
                .collect();
            for decl in context.get_nodes_by_name_shared(&sup) {
                if decl.language != owner.language || !is_member_class_kind(decl.kind) {
                    continue;
                }
                let params = class_type_parameters(&decl, context);
                let bound: HashMap<String, String> = params
                    .into_iter()
                    .zip(
                        given
                            .iter()
                            .cloned()
                            .chain(std::iter::repeat(String::new())),
                    )
                    .filter(|(_, argument)| !argument.is_empty())
                    .collect();
                queue.push_back((decl, bound));
            }
        }
    }
    memo_set(&tables.walks, key, result.clone());
    result
}

/// The first `<…>` of a declaration's head, split at its top-level commas
/// (upstream `angleArguments`).
pub(crate) fn angle_arguments(text: &str) -> Vec<String> {
    let Some(open) = text.find('<') else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    for ch in text[open + 1..].chars() {
        if ch == '<' {
            depth += 1;
        } else if ch == '>' {
            if depth == 0 {
                break;
            }
            depth -= 1;
        }
        if ch == ',' && depth == 0 {
            out.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    out.push(current);
    out.into_iter()
        .map(|argument| argument.trim_matches(is_js_whitespace).to_string())
        .collect()
}

/// The type parameters a class declares: `TFixture` for `class
/// IntegrationTest<TFixture>` (upstream `declaredTypeParameters`, four lines
/// of its head).
fn class_type_parameters(decl: &Node, context: &dyn ResolutionContext) -> Vec<String> {
    context
        .source_facts(&decl.file_path)
        .map(|facts| class_type_parameters_in(&facts, decl))
        .unwrap_or_default()
}

/// [`class_type_parameters`] of `decl` in a file whose facts are `facts`.
fn class_type_parameters_in(facts: &crate::source_facts::SourceFacts, decl: &Node) -> Vec<String> {
    static PARAMETER: OnceLock<Regex> = OnceLock::new();
    static VARIANCE: OnceLock<Regex> = OnceLock::new();
    let start = usize::try_from(decl.start_line)
        .unwrap_or(1)
        .saturating_sub(1);
    let end = (start + 4).min(facts.line_count());
    let head = if start < end {
        facts.join_lines(start, end, " ")
    } else {
        String::new()
    };
    let Ok(at) = Regex::new(&format!(r"(?-u:\b){}\s*<", regex::escape(&decl.name))) else {
        return Vec::new();
    };
    let Some(found) = at.find(&head) else {
        return Vec::new();
    };
    let parameter = PARAMETER.get_or_init(|| {
        Regex::new(r"([A-Za-z_][A-Za-z0-9_]*)\s*(?:extends(?-u:\b).*|:.*)?$").expect("parameter")
    });
    let variance =
        VARIANCE.get_or_init(|| Regex::new(r"^(?:in|out|reified)\s+").expect("variance"));
    angle_arguments(&head[found.start()..])
        .into_iter()
        .map(|p| {
            let p = variance.replace(&p, "");
            parameter
                .captures(&p)
                .map(|caps| caps[1].to_string())
                .unwrap_or_default()
        })
        .collect()
}

/// The type arguments a class's head gives a supertype, as simple names:
/// `DatabaseInitializer` for `: IntegrationTest<QueryTests.DatabaseInitializer>`
/// (upstream `headTypeArguments`, nine lines of its head).
fn head_type_arguments(class: &Node, sup: &str, context: &dyn ResolutionContext) -> Vec<String> {
    let head = head_before_brace(class, 9, context);
    let Ok(at) = Regex::new(&format!(
        r"(?:[:,]|(?-u:\b)extends|(?-u:\b)implements)\s*(?:[A-Za-z0-9_.]+\.)?{}\s*<",
        regex::escape(sup)
    )) else {
        return Vec::new();
    };
    let Some(found) = at.find(&head) else {
        return Vec::new();
    };
    angle_arguments(&head[found.start()..])
        .into_iter()
        .map(|argument| {
            let argument = argument.split('<').next().unwrap_or_default();
            argument
                .rsplit('.')
                .next()
                .unwrap_or_default()
                .trim_matches(is_js_whitespace)
                .to_string()
        })
        .collect()
}

/// The type `class` declares a member `name` with, read from its
/// member-level lines (upstream `classMemberType`); inside parentheses at
/// member depth is a method's parameter list, only the head's (a primary
/// constructor's) declares members.
fn class_member_type(class: &Node, name: &str, context: &dyn ResolutionContext) -> Option<String> {
    let tables = context.member_tables();
    let key = (class.id.clone(), name.to_string());
    if let Some(hit) = memo_get(&tables.member_types, &key) {
        return hit;
    }
    let r = regex::escape(name);
    let pattern = if class.language == Language::Kotlin {
        Regex::new(&format!(
            r"(?-u:\b)(?:val|var)\s+{r}\s*:\s*([A-Z][A-Za-z0-9_.]*)"
        ))
    } else {
        Regex::new(&format!(
            r"(?:^|[\s(,])([A-Za-z_][A-Za-z0-9_.]*)\s*{TYPE_ARGS}\??\s+{r}\s*(?:[=;,)]|\{{)"
        ))
    };
    let mut found = None;
    if let Ok(pattern) = pattern {
        for (text, depth) in class_member_lines(class, context).iter() {
            if !text.contains(name) {
                continue;
            }
            let Some(caps) = pattern.captures(text) else {
                continue;
            };
            let at = caps.get(0).map_or(0, |m| m.start());
            let before = &text[..at];
            let in_parens =
                *depth == 1 && before.matches('(').count() > before.matches(')').count();
            let written = caps.get(1).map_or("", |m| m.as_str());
            if !in_parens && !MEMBER_TYPE_NON_TYPES.contains(&written) {
                found = crate::name_matcher::normalize_inferred_type_name(written);
                break;
            }
        }
    }
    memo_set(&tables.member_types, key, found.clone());
    found
}

/// A class's own member-declaration lines with their brace depth: those at
/// its body's depth, or its head, with comments and string contents dropped
/// (upstream `classMemberLines`, read once per class).
pub(crate) fn class_member_lines(class: &Node, context: &dyn ResolutionContext) -> MemberLines {
    let tables = context.member_tables();
    if let Some(hit) = memo_get(&tables.lines, &class.id) {
        return hit;
    }
    let lines = context
        .source_facts(&class.file_path)
        .map(|facts| member_lines_of(&facts, class))
        .unwrap_or_default();
    let lines = Arc::new(lines);
    memo_set(&tables.lines, class.id.clone(), Arc::clone(&lines));
    lines
}

/// [`class_member_lines`] of `class` in a file whose facts are `facts`.
pub(crate) fn member_lines_of(
    facts: &crate::source_facts::SourceFacts,
    class: &Node,
) -> Vec<(String, usize)> {
    static STRINGS: OnceLock<Regex> = OnceLock::new();
    static BLOCK: OnceLock<Regex> = OnceLock::new();
    static LINE: OnceLock<Regex> = OnceLock::new();
    let strings = STRINGS.get_or_init(|| {
        Regex::new(r#""(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'"#).expect("member strings")
    });
    let block = BLOCK.get_or_init(|| Regex::new(r"/\*.*?\*/").expect("member block"));
    let line_comment = LINE.get_or_init(|| Regex::new(r"//.*$").expect("member line"));
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut in_comment = false;
    let start = usize::try_from(class.start_line).unwrap_or(1).max(1);
    let end = usize::try_from(class.end_line).unwrap_or(0);
    for line in start..=end {
        let raw = if line <= facts.line_count() {
            facts.line(line - 1)
        } else {
            ""
        };
        let mut raw = raw;
        if in_comment {
            let Some(close) = raw.find("*/") else {
                continue;
            };
            raw = &raw[close + 2..];
            in_comment = false;
        }
        let text = strings.replace_all(raw, "\"\"");
        let text = block.replace_all(&text, " ");
        let mut text = line_comment.replace(&text, "").into_owned();
        if let Some(open) = text.find("/*") {
            text.truncate(open);
            in_comment = true;
        }
        if depth <= 1 && !text.trim_matches(is_js_whitespace).is_empty() {
            out.push((text.clone(), depth));
        }
        for ch in text.chars() {
            if ch == '{' {
                depth += 1;
            } else if ch == '}' {
                depth = depth.saturating_sub(1);
            }
        }
    }
    out
}

/// What a Java / C# / Kotlin type parameter of the class or method around a
/// call is bounded by (upstream `typeParameterBound`).
pub(crate) fn type_parameter_bound(
    type_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> TypeParameterBound {
    let around: Vec<Arc<Node>> = context
        .get_nodes_in_file_shared(&reference.file_path)
        .into_iter()
        .filter(|n| {
            n.start_line <= reference.line
                && n.end_line >= reference.line
                && (is_member_class_kind(n.kind)
                    || matches!(n.kind, NodeKind::Method | NodeKind::Function))
        })
        .collect();
    type_parameter_bound_in(type_name, &around, context)
}

/// [`type_parameter_bound`] over the heads (six lines, to the first `{`) of
/// the given declarations (upstream `typeParameterBoundIn`).
fn type_parameter_bound_in(
    type_name: &str,
    decls: &[Arc<Node>],
    context: &dyn ResolutionContext,
) -> TypeParameterBound {
    let uppercase = type_name.starts_with(|c: char| c.is_ascii_uppercase())
        && type_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !uppercase {
        return TypeParameterBound::NotParameter;
    }
    let t = regex::escape(type_name);
    let variance = r"(?:in\s+|out\s+|reified\s+)?";
    let (Ok(bound), Ok(where_bound), Ok(declared)) = (
        Regex::new(&format!(
            r"[<,]\s*{variance}{t}\s*(?:extends|:)\s*([A-Z][A-Za-z0-9_.]*)"
        )),
        Regex::new(&format!(r"(?-u:\b)where\s+{t}\s*:\s*([A-Z][A-Za-z0-9_.]*)")),
        Regex::new(&format!(r"[<,]\s*{variance}{t}\s*[,>]")),
    ) else {
        return TypeParameterBound::NotParameter;
    };
    let mut is_declared = false;
    for decl in decls {
        let head = head_before_brace(decl, 6, context);
        let found = bound
            .captures(&head)
            .or_else(|| where_bound.captures(&head))
            .map(|caps| caps[1].to_string());
        if let Some(found) = found {
            let last = found.rsplit('.').next().unwrap_or(&found).to_string();
            return TypeParameterBound::Bound(last);
        }
        if declared.is_match(&head) {
            is_declared = true;
        }
    }
    if is_declared {
        TypeParameterBound::Unbounded
    } else {
        TypeParameterBound::NotParameter
    }
}

/// The `project_metadata` key prefix of a file's member surface row.
pub const SURFACE_ROW_KEY_PREFIX: &str = "member_surface:";

/// The `project_metadata` key of `file_path`'s member surface row.
pub fn surface_row_key(file_path: &str) -> String {
    format!("{SURFACE_ROW_KEY_PREFIX}{file_path}")
}

/// What the member walk reads of one class declaration of another file: its
/// name and qualified name, the supertypes its head names
/// ([`crate::class_heads::class_head_supertypes`]), and a digest of its
/// member-level lines, of its head (nine lines, cut at the first `{`), which
/// the type arguments it gives and the bounds are read from, and of the type
/// parameters it declares.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct MemberClass {
    pub name: String,
    pub qualified_name: String,
    pub supers: Vec<String>,
    pub digest: String,
}

/// What other files' member walks (W4-CS-03, upstream #2171) read of one C#,
/// Java or Kotlin file: each class, struct, interface and enum declaration,
/// in node order.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MemberSurface {
    pub classes: Vec<MemberClass>,
}

impl MemberSurface {
    /// The row of the file whose text is `source` and whose nodes are
    /// `nodes`.
    pub fn of(source: &str, nodes: &[Node]) -> Self {
        let facts = crate::source_facts::SourceFacts::new(Arc::from(source));
        let mut ordered: Vec<&Node> = nodes
            .iter()
            .filter(|node| {
                is_member_typed_language(node.language) && is_member_class_kind(node.kind)
            })
            .collect();
        ordered.sort_by(|a, b| {
            (a.start_line, a.start_column, &a.id).cmp(&(b.start_line, b.start_column, &b.id))
        });
        let classes = ordered
            .into_iter()
            .map(|node| {
                let mut read = String::new();
                for (text, depth) in member_lines_of(&facts, node) {
                    read.push_str(&format!("{depth}\t{text}\n"));
                }
                read.push_str("\n--head--\n");
                let start = usize::try_from(node.start_line)
                    .unwrap_or(1)
                    .saturating_sub(1);
                let end = (start + 9).min(facts.line_count());
                if start < end {
                    let head = facts.join_lines(start, end, " ");
                    read.push_str(head.split('{').next().unwrap_or_default());
                }
                read.push_str("\n--parameters--\n");
                read.push_str(&class_type_parameters_in(&facts, node).join(","));
                let digest = codegraph_core::node_id::hash_content(&read);
                MemberClass {
                    name: node.name.clone(),
                    qualified_name: node.qualified_name.clone(),
                    supers: crate::class_heads::class_head_supertypes_in(&facts, node),
                    digest: digest[..16].to_string(),
                }
            })
            .collect();
        Self { classes }
    }

    /// No class: no row records it.
    pub fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("member surface row encodes")
    }

    pub fn decode(value: &str) -> Option<Self> {
        serde_json::from_str(value).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_lines_keep_member_depth_and_drop_comments_and_strings() {
        let source = "public class Writer : Base\n{\n    private readonly JsonWriter _inner; // the inner \"one\"\n    /* a block comment\n       naming TextWriter _text; */\n    private string Name = \"x { y\";\n    public void Write()\n    {\n        var local = 1;\n    }\n}\n";
        let facts = crate::source_facts::SourceFacts::new(Arc::from(source));
        let mut class = test_node("Writer");
        class.start_line = 1;
        class.end_line = 11;
        let lines = member_lines_of(&facts, &class);
        let texts: Vec<(&str, usize)> = lines.iter().map(|(t, d)| (t.trim(), *d)).collect();
        assert_eq!(
            texts,
            [
                ("public class Writer : Base", 0),
                ("{", 0),
                ("private readonly JsonWriter _inner;", 1),
                ("private string Name = \"\";", 1),
                ("public void Write()", 1),
                ("{", 1),
                ("}", 2),
                ("}", 1),
            ]
            .into_iter()
            .filter(|(_, depth)| *depth <= 1)
            .collect::<Vec<_>>()
        );
    }

    fn test_node(name: &str) -> Node {
        Node {
            id: format!("class:{name}"),
            kind: NodeKind::Class,
            name: name.to_string(),
            qualified_name: format!("App::{name}"),
            file_path: "A.cs".to_string(),
            language: Language::CSharp,
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
    fn a_member_surface_digest_ignores_method_bodies() {
        let before = "public class Base\n{\n    protected JsonWriter Writer { get; set; }\n    public int Depth()\n    {\n        return 1;\n    }\n}\n";
        let after_body = before.replace("return 1;", "return 2;");
        let after_member = before.replace("JsonWriter Writer", "TextWriter Writer");
        let mut class = test_node("Base");
        class.end_line = 8;
        let digest = |source: &str| {
            MemberSurface::of(source, std::slice::from_ref(&class)).classes[0]
                .digest
                .clone()
        };
        assert_eq!(digest(before), digest(&after_body));
        assert_ne!(digest(before), digest(&after_member));
    }

    #[test]
    fn a_member_surface_digest_reads_the_head_to_its_body() {
        let before = "public abstract class Middle : IntegrationTest<FirstInit> where TFixture : FirstInit\n{\n}\n";
        let mut class = test_node("Middle");
        class.end_line = 3;
        let digest = |source: &str| {
            MemberSurface::of(source, std::slice::from_ref(&class)).classes[0]
                .digest
                .clone()
        };
        assert_ne!(
            digest(before),
            digest(&before.replace("<FirstInit>", "<SecondInit>"))
        );
        assert_ne!(
            digest(before),
            digest(&before.replace(": FirstInit", ": SecondInit"))
        );
        let generic = "public class Box<T>\n{\n}\n";
        let mut boxed = test_node("Box");
        boxed.end_line = 3;
        let parameters = |source: &str| {
            MemberSurface::of(source, std::slice::from_ref(&boxed)).classes[0]
                .digest
                .clone()
        };
        assert_ne!(
            parameters(generic),
            parameters(&generic.replace("<T>", "<U>"))
        );
    }

    #[test]
    fn angle_arguments_split_at_top_level_commas() {
        assert_eq!(angle_arguments(": Pair<L, R> {"), ["L", "R"]);
        assert_eq!(
            angle_arguments("Map<string, List<int>> x"),
            ["string", "List<int>"]
        );
        assert!(angle_arguments("class Plain").is_empty());
    }
}
