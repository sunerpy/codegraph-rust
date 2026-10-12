//! Java names by scope (upstream #2126, #2195, #2236, in the shape at
//! `b635dd46`): a bare call reaches a method in scope, an import from outside
//! the project owns its name, and a bare type name is a type in reach by
//! package, import or nesting.
//!
//! Supertypes are read from the declarations' heads, as upstream reads them:
//! a first resolution pass has no `extends` edges yet.

use std::cell::OnceCell;
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::source_facts::SourceFacts;
use crate::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::types::{RefView, ResolutionContext};

/// How many type names a bare call's scope walk visits (upstream #2126).
const SCOPE_TYPE_LIMIT: usize = 40;

/// How many levels above a type its ancestors are read (upstream #2236
/// `javaAncestorNames`, `depth < 4`).
const ANCESTOR_DEPTH: usize = 4;

/// The kinds a Java type declaration is indexed as. Upstream's
/// `JAVA_TYPE_KINDS` and `JAVA_TYPE_KINDS_VISIBLE` also list `struct`,
/// `trait`, `record` and `annotation`, which no Java node has.
fn is_java_type_kind(kind: NodeKind) -> bool {
    matches!(kind, NodeKind::Class | NodeKind::Interface | NodeKind::Enum)
}

/// `^[A-Za-z_$][\w$]*$` (ASCII), the bare names these rules judge.
fn is_java_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// A Java call by a bare name: `verify(mock)`, `helper()`, and `this.x()` and
/// `super.x()`, which the extractor records as `x`.
pub(crate) fn is_java_bare_call(reference: &RefView) -> bool {
    reference.language == Language::Java
        && reference.reference_kind == EdgeKind::Calls
        && is_java_identifier(&reference.reference_name)
}

/// What a Java file declares about the names it sees: its package and
/// imports (upstream `javaFileScope`, read without comments and strings) and
/// its static imports (upstream `javaStaticImportsOf`, read from the raw
/// text).
#[derive(Debug, Default)]
pub(crate) struct JavaFileScope {
    /// `package a.b;`, or empty.
    package: String,
    /// Single-type and single static imports as written: `a.b.C`, `a.B.m`.
    single: HashSet<String>,
    /// On-demand imports, static ones included, without the `.*`.
    demand: HashSet<String>,
    /// The owners of `import static a.B.*;`: `B`.
    static_owners: HashSet<String>,
    /// `B.m` for each `import static a.B.m;`.
    static_members: HashSet<String>,
}

impl JavaFileScope {
    pub(crate) fn read(source: &str) -> Self {
        static PACKAGE: OnceLock<Regex> = OnceLock::new();
        static IMPORT: OnceLock<Regex> = OnceLock::new();
        static STATIC_IMPORT: OnceLock<Regex> = OnceLock::new();
        let package = PACKAGE.get_or_init(|| {
            Regex::new(r"(?m)^\s*package\s+([A-Za-z0-9_.]+)\s*;").expect("java package")
        });
        let import = IMPORT.get_or_init(|| {
            Regex::new(r"(?m)^\s*import\s+(?:static\s+)?([A-Za-z0-9_.]+?)(\.\*)?\s*;")
                .expect("java import")
        });
        let static_import = STATIC_IMPORT.get_or_init(|| {
            Regex::new(
                r"(?m)^\s*import\s+static\s+([A-Za-z0-9_.$]+)\s*\.\s*(\*|[A-Za-z0-9_$]+)\s*;",
            )
            .expect("java static import")
        });
        let text = strip_comments_for_regex(source, CommentLang::Java);
        let mut scope = Self {
            package: package
                .captures(&text)
                .map(|caps| caps[1].to_string())
                .unwrap_or_default(),
            ..Self::default()
        };
        for caps in import.captures_iter(&text) {
            let path = caps[1].to_string();
            if caps.get(2).is_some() {
                scope.demand.insert(path);
            } else {
                scope.single.insert(path);
            }
        }
        for caps in static_import.captures_iter(source) {
            let owner = caps[1].rsplit('.').next().unwrap_or_default();
            if &caps[2] == "*" {
                scope.static_owners.insert(owner.to_string());
            } else {
                scope.static_members.insert(format!("{owner}.{}", &caps[2]));
            }
        }
        scope
    }
}

/// `file`'s [`JavaFileScope`], read once per resolution pass; empty when the
/// file is unreadable.
fn file_scope(file: &str, context: &dyn ResolutionContext) -> Arc<JavaFileScope> {
    context.source_facts(file).map_or_else(
        || Arc::new(JavaFileScope::default()),
        |facts| facts.java_file_scope(),
    )
}

/// The supertype names a Java type declaration's head writes, by the two
/// readers upstream uses.
#[derive(Debug, Default)]
pub(crate) struct JavaHead {
    /// Upstream `javaSupertypesOf` (#2126): the six lines from the
    /// declaration joined, the `extends` / `implements` clause up to the
    /// first `{` after it, type arguments dropped, each dotted name by its
    /// last segment.
    supertypes: Vec<String>,
    /// The direct names of upstream `javaAncestorNames` (#2236): the lines up
    /// to the one that opens the body (at most seven), the text after the
    /// first `extends` / `implements` before the `{`, type arguments dropped,
    /// each name followed by a `,`, by `implements` or by the end.
    ancestors: Vec<String>,
}

impl JavaHead {
    pub(crate) fn read(facts: &SourceFacts, start_line: i64) -> Self {
        static CLAUSE: OnceLock<Regex> = OnceLock::new();
        static LIST: OnceLock<Regex> = OnceLock::new();
        static NAME: OnceLock<Regex> = OnceLock::new();
        let clause = CLAUSE.get_or_init(|| {
            Regex::new(r"(?-u:\b)(?:extends|implements)(?-u:\b)([^{]*)\{").expect("java clause")
        });
        let list = LIST.get_or_init(|| {
            Regex::new(r"(?-u:\b)(?:extends|implements)(?-u:\b)([^{]*)").expect("java list")
        });
        let name = NAME.get_or_init(|| {
            Regex::new(r"[A-Za-z_$][A-Za-z0-9_$]*(?:\.[A-Za-z_$][A-Za-z0-9_$]*)*")
                .expect("java head name")
        });
        let count = facts.line_count();
        let start = usize::try_from(start_line).unwrap_or(1).saturating_sub(1);

        let end = start.saturating_add(6).min(count);
        let head = if start < end {
            facts.join_lines(start, end, " ")
        } else {
            String::new()
        };
        let written = clause
            .captures(&head)
            .map(|caps| caps[1].to_string())
            .unwrap_or_default();
        let supertypes = name
            .find_iter(&without_type_arguments(&written))
            .filter_map(|m| m.as_str().rsplit('.').next().map(str::to_string))
            .filter(|simple| simple != "extends" && simple != "implements")
            .collect();

        let mut header = String::new();
        let mut at = start;
        while at < start.saturating_add(7).min(count) && !header.contains('{') {
            header.push_str(facts.line(at));
            header.push(' ');
            at += 1;
        }
        let before_body = header.split('{').next().unwrap_or_default();
        let listed = list
            .captures(before_body)
            .map(|caps| without_type_arguments(&caps[1]))
            .unwrap_or_default();
        let ancestors = listed_names(&listed)
            .into_iter()
            .filter(|simple| simple != "extends" && simple != "implements")
            .collect();
        Self {
            supertypes,
            ancestors,
        }
    }
}

/// `text` without its type arguments, two levels deep
/// (`/<[^<>]*(?:<[^<>]*>[^<>]*)*>/g`).
fn without_type_arguments(text: &str) -> String {
    static ARGS: OnceLock<Regex> = OnceLock::new();
    ARGS.get_or_init(|| Regex::new(r"<[^<>]*(?:<[^<>]*>[^<>]*)*>").expect("java type args"))
        .replace_all(text, "")
        .into_owned()
}

/// Each `([A-Za-z_]\w*)\s*(?=,|$|\bimplements\b|\s*$)` of a supertype list:
/// a name followed, after any whitespace, by a `,`, by `implements` or by the
/// end. Upstream's lookahead is checked by hand (the regex crate has none);
/// a word run whose follower fails fails from every start inside it, so a
/// run is judged once, from its first letter or `_`.
fn listed_names(list: &str) -> Vec<String> {
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let bytes = list.as_bytes();
    let mut names = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        if !is_word(bytes[at]) {
            at += 1;
            continue;
        }
        let run = at;
        let mut end = at;
        while end < bytes.len() && is_word(bytes[end]) {
            end += 1;
        }
        let Some(first) = bytes[run..end]
            .iter()
            .position(|b| b.is_ascii_alphabetic() || *b == b'_')
        else {
            at = end;
            continue;
        };
        let rest = &list[end..];
        let after = end + (rest.len() - rest.trim_start().len());
        let follower = &list[after..];
        let followed = follower.is_empty()
            || follower.starts_with(',')
            || follower
                .strip_prefix("implements")
                .is_some_and(|tail| tail.bytes().next().is_none_or(|b| !is_word(b)));
        if followed {
            names.push(list[run + first..end].to_string());
            at = after.max(end);
        } else {
            at = end;
        }
    }
    names
}

/// Key prefix of the Java type head rows (W4-JVM-01): one
/// `project_metadata` row per Java file, so that a sync can tell which
/// heads a change moved once the old text is gone.
pub const HEAD_ROW_KEY_PREFIX: &str = "java_type_heads:";

/// The `project_metadata` key of `file_path`'s head row.
pub fn head_row_key(file_path: &str) -> String {
    format!("{HEAD_ROW_KEY_PREFIX}{file_path}")
}

/// One Java type declaration's head, as resolution reads it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HeadEntry {
    pub name: String,
    pub qualified_name: String,
    /// What the scope walk of a bare call reads (upstream #2126).
    pub supertypes: Vec<String>,
    /// What a type name's ancestor walk reads (upstream #2236).
    pub ancestors: Vec<String>,
}

impl HeadEntry {
    /// Every name the head writes, by either reader.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.supertypes
            .iter()
            .chain(&self.ancestors)
            .map(String::as_str)
    }
}

/// A Java file's head row: each type declaration whose head names
/// anything, in the order of the declarations in the file (the order the
/// scope walk of a bare call reads same-named declarations in, which its
/// limit makes part of what it reaches). A declaration whose head names
/// nothing contributes no name to any walk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadRow {
    pub entries: Vec<HeadEntry>,
}

impl HeadRow {
    /// The row of a file whose text is `source` and whose nodes are
    /// `nodes`, read by the functions resolution reads heads with.
    pub fn of(source: &str, nodes: &[Node]) -> Self {
        let facts = SourceFacts::new(Arc::from(source));
        let mut declarations: Vec<&Node> = nodes
            .iter()
            .filter(|node| node.language == Language::Java && is_java_type_kind(node.kind))
            .collect();
        declarations.sort_by(|a, b| {
            (a.start_line, a.start_column, &a.id).cmp(&(b.start_line, b.start_column, &b.id))
        });
        let entries: Vec<HeadEntry> = declarations
            .into_iter()
            .filter_map(|node| {
                let head = JavaHead::read(&facts, node.start_line);
                (!head.supertypes.is_empty() || !head.ancestors.is_empty()).then(|| HeadEntry {
                    name: node.name.clone(),
                    qualified_name: node.qualified_name.clone(),
                    supertypes: head.supertypes,
                    ancestors: head.ancestors,
                })
            })
            .collect();
        Self { entries }
    }

    /// No entry: no row records it.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("java head row encodes")
    }

    pub fn decode(value: &str) -> Option<Self> {
        serde_json::from_str(value).ok()
    }
}

/// `declaration`'s [`JavaHead`], read once per resolution pass.
fn head_of(declaration: &Node, context: &dyn ResolutionContext) -> Arc<JavaHead> {
    context
        .source_facts(&declaration.file_path)
        .map_or_else(Arc::default, |facts| {
            facts.java_head(&declaration.id, |facts| {
                JavaHead::read(facts, declaration.start_line)
            })
        })
}

/// The names every Java type declaration called `type_name` extends or
/// implements (upstream `javaSupertypesOf`). The declarations are read in
/// path order, so the walk's limit cuts the same names on every index
/// (upstream reads them in index order; KEEP-RUST).
fn supertypes_of(type_name: &str, context: &dyn ResolutionContext) -> Vec<String> {
    let mut declarations: Vec<Arc<Node>> = context
        .get_nodes_by_name_shared(type_name)
        .into_iter()
        .filter(|node| node.language == Language::Java && is_java_type_kind(node.kind))
        .collect();
    declarations.sort_by(|a, b| {
        (&a.file_path, a.start_line, a.start_column, &a.id).cmp(&(
            &b.file_path,
            b.start_line,
            b.start_column,
            &b.id,
        ))
    });
    declarations
        .iter()
        .flat_map(|declaration| head_of(declaration, context).supertypes.clone())
        .collect()
}

/// What a bare Java call can reach (upstream #2126 `isJavaMethodInScope`):
/// a method of a class around it, of one of that class's supertypes (an
/// anonymous class's being what it instantiates), or one its file imports
/// statically. halo's tests' Mockito `verify(…)` bound 1,038 calls to an
/// `EmailVerificationService.verify`.
pub(crate) struct JavaCallScope {
    file: Arc<JavaFileScope>,
    /// The types around the call and, breadth first, the names their
    /// declarations' heads extend or implement, at most
    /// [`SCOPE_TYPE_LIMIT`].
    types: HashSet<String>,
}

impl JavaCallScope {
    pub(crate) fn of(reference: &RefView, context: &dyn ResolutionContext) -> Self {
        let mut queue: VecDeque<String> = context
            .get_nodes_in_file_shared(&reference.file_path)
            .iter()
            .filter(|node| {
                is_java_type_kind(node.kind)
                    && node.start_line <= reference.line
                    && node.end_line >= reference.line
            })
            .map(|node| node.name.clone())
            .collect();
        let mut types = HashSet::new();
        while types.len() < SCOPE_TYPE_LIMIT {
            let Some(name) = queue.pop_front() else {
                break;
            };
            if types.contains(&name) {
                continue;
            }
            queue.extend(supertypes_of(&name, context));
            // An anonymous class derives from what it instantiates, as the
            // type-name rule reads it: gson's `new LinkedTreeMapIterator<…>()
            // { … nextNode() … }` calls its base's `nextNode`. Upstream reads
            // only the declaration's head, which names nothing (KEEP-RUST).
            if let Some(instantiated) = anonymous_supertype(&name, &name) {
                queue.push_back(instantiated.to_string());
            }
            types.insert(name);
        }
        Self {
            file: file_scope(&reference.file_path, context),
            types,
        }
    }

    /// Whether `method` is a method of a type in reach, or one the file
    /// imports statically. A method without an owner is always in reach.
    pub(crate) fn reaches(&self, method: &Node, reference: &RefView) -> bool {
        let Some((parent, _)) = method.qualified_name.rsplit_once("::") else {
            return true;
        };
        let owner = parent.rsplit("::").next().unwrap_or(parent);
        self.file.static_owners.contains(owner)
            || self
                .file
                .static_members
                .contains(&format!("{owner}.{}", reference.reference_name))
            || self.types.contains(owner)
    }
}

/// Whether `name` is a package a Java, Kotlin or Scala file of the project
/// declares (upstream `JVM_PACKAGES`).
fn is_project_jvm_package(name: &str, context: &dyn ResolutionContext) -> bool {
    context
        .get_nodes_by_qualified_name_shared(name)
        .iter()
        .any(|node| {
            node.kind == NodeKind::Namespace
                && matches!(
                    node.language,
                    Language::Java | Language::Kotlin | Language::Scala
                )
        })
}

/// Whether a Java or Kotlin reference's first segment is bound by a
/// single-type or static import from a package the project does not declare
/// (upstream #2195 `isJavaOutsideImport`, Kotlin since #2196): after `import
/// java.lang.reflect.Field;` the file's `Field` is the JDK's, and after
/// `import static org.junit.Assert.assertEquals;` its `assertEquals` is
/// JUnit's. An import of the project's own class, a nested one included, is
/// under a project package. A declaration the file makes itself comes first,
/// in its own namespace: a callable for a call, a type otherwise (#2196).
/// An `imports` reference is the import resolver's.
pub(crate) fn binds_an_outside_import(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    if !matches!(reference.language, Language::Java | Language::Kotlin)
        || reference.reference_kind == EdgeKind::Imports
    {
        return false;
    }
    let head = reference
        .reference_name
        .split('.')
        .next()
        .unwrap_or_default();
    let mappings = context.get_import_mappings(&reference.file_path, reference.language);
    let Some(binding) = mappings.iter().find(|mapping| mapping.local_name == head) else {
        return false;
    };
    // Exposed's `toLocalDateTime(value)` inside the column type that defines
    // it, beside `import kotlinx.datetime.toLocalDateTime`; but gson's `new
    // URI(…)` is `java.net.URI` beside `TypeAdapters`' field `URI`.
    let call = reference.reference_kind == EdgeKind::Calls && head == reference.reference_name;
    if context
        .get_nodes_in_file_shared(&reference.file_path)
        .iter()
        .any(|node| {
            node.name == head
                && if call {
                    matches!(node.kind, NodeKind::Method | NodeKind::Function)
                } else {
                    matches!(
                        node.kind,
                        NodeKind::Class
                            | NodeKind::Interface
                            | NodeKind::Enum
                            | NodeKind::Struct
                            | NodeKind::Trait
                            | NodeKind::TypeAlias
                    )
                }
        })
    {
        return false;
    }
    let parts: Vec<&str> = binding.source.split('.').collect();
    !(1..parts.len()).any(|end| is_project_jvm_package(&parts[..end].join("."), context))
}

/// The first node of `file` (by line) whose qualified name is
/// `qualified_name` and that `keep` accepts.
fn first_in_file(
    qualified_name: &str,
    file: &str,
    keep: impl Fn(&Node) -> bool,
    context: &dyn ResolutionContext,
) -> Option<Arc<Node>> {
    context
        .get_nodes_by_qualified_name_shared(qualified_name)
        .into_iter()
        .filter(|node| node.file_path == file && keep(node))
        .min_by_key(|node| node.start_line)
}

/// The type an anonymous class instantiates: `NodeFilter` of
/// `<NodeFilter$anon@12>`, read from its name or the last segment of its
/// qualified name.
fn anonymous_supertype<'n>(name: &'n str, qualified_name: &'n str) -> Option<&'n str> {
    static AT_END: OnceLock<Regex> = OnceLock::new();
    static ANYWHERE: OnceLock<Regex> = OnceLock::new();
    let at_end = AT_END.get_or_init(|| {
        Regex::new(r"<([A-Za-z_$][A-Za-z0-9_$]*)\$anon@[0-9]+>$").expect("java anon name")
    });
    let anywhere = ANYWHERE.get_or_init(|| {
        Regex::new(r"<([A-Za-z_$][A-Za-z0-9_$]*)\$anon@[0-9]+>").expect("java anon segment")
    });
    at_end
        .captures(name)
        .and_then(|caps| caps.get(1))
        .or_else(|| {
            let last = qualified_name.rsplit("::").next().unwrap_or_default();
            anywhere.captures(last).and_then(|caps| caps.get(1))
        })
        .map(|found| found.as_str())
}

/// The names the Java types of qualified name `qualified_name` extend or
/// implement, and theirs up to [`ANCESTOR_DEPTH`] levels above, through
/// every Java type of each name (upstream `javaAncestorNames`). Each type is
/// read once, at the nearest level it is reached; upstream memoises each
/// type at the depth it was first asked for, which makes a name's reach
/// depend on resolution order (KEEP-RUST).
fn ancestor_names(qualified_name: &str, context: &dyn ResolutionContext) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut read = HashSet::new();
    let mut level = vec![qualified_name.to_string()];
    for depth in 0..=ANCESTOR_DEPTH {
        let mut next = Vec::new();
        for qualified in level {
            if !read.insert(qualified.clone()) {
                continue;
            }
            let direct: Vec<String> = context
                .get_nodes_by_qualified_name_shared(&qualified)
                .iter()
                .filter(|node| node.language == Language::Java && is_java_type_kind(node.kind))
                .flat_map(|node| head_of(node, context).ancestors.clone())
                .collect();
            if depth < ANCESTOR_DEPTH {
                for base in &direct {
                    next.extend(
                        context
                            .get_nodes_by_name_shared(base)
                            .into_iter()
                            .filter(|node| {
                                node.language == Language::Java
                                    && is_java_type_kind(node.kind)
                                    && node.qualified_name != qualified
                            })
                            .map(|node| node.qualified_name.clone()),
                    );
                }
            }
            names.extend(direct);
        }
        if next.is_empty() {
            break;
        }
        level = next;
    }
    names
}

/// How the reference's own line writes its name.
#[derive(Debug, Default)]
struct Written {
    /// `outer.new Inner()`.
    through_new: bool,
    /// Each qualifier written before the name, `RequestFactory` of
    /// `RequestFactory.Builder`, a type annotation allowed between them
    /// (jsoup's `Range.@Nullable Spans`).
    qualifiers: Vec<String>,
}

/// A bare Java name's reference site, read once for every candidate it is
/// judged against (upstream #2236 `isJavaTypeVisible`).
pub(crate) struct JavaTypeSite<'r> {
    reference: &'r RefView,
    judged: bool,
    here: OnceCell<Arc<JavaFileScope>>,
    line: OnceCell<String>,
    written: OnceCell<Written>,
    /// `(qualified name, name)` of each Java type around the reference.
    enclosing: OnceCell<Vec<(String, String)>>,
    /// What the types around the reference extend or implement, and what
    /// an anonymous class among them instantiates.
    around: OnceCell<HashSet<String>>,
}

impl<'r> JavaTypeSite<'r> {
    pub(crate) fn new(reference: &'r RefView) -> Self {
        Self {
            reference,
            judged: reference.language == Language::Java
                && is_java_identifier(&reference.reference_name),
            here: OnceCell::new(),
            line: OnceCell::new(),
            written: OnceCell::new(),
            enclosing: OnceCell::new(),
            around: OnceCell::new(),
        }
    }

    /// Whether the bare name can mean `candidate`. A top-level type is in
    /// reach from its own package and through a single-type or on-demand
    /// import; a nested type from inside its owner (or a type deriving from
    /// it), through an import of it or of its owner's members, or written
    /// through its owner. A constructor is in reach where its type is, and an
    /// enum constant inside its enum, in a `case` label, through a static
    /// import or written through its enum. Anything else, and any name in
    /// another language, is not judged here.
    pub(crate) fn visible(&self, candidate: &Node, context: &dyn ResolutionContext) -> bool {
        if !self.judged || candidate.language != Language::Java {
            return true;
        }
        match candidate.kind {
            NodeKind::Method => self.constructor_visible(candidate, context),
            NodeKind::EnumMember => self.enum_constant_visible(candidate, context),
            kind if is_java_type_kind(kind) => self.type_visible(candidate, context),
            _ => true,
        }
    }

    /// lombok's `@Builder` and okhttp's `new OkHttpClient.Builder()` are no
    /// project `Builder`'s constructor. A call is judged as a method.
    fn constructor_visible(&self, candidate: &Node, context: &dyn ResolutionContext) -> bool {
        let segments: Vec<&str> = candidate.qualified_name.split("::").collect();
        if segments.len() < 2
            || segments[segments.len() - 2] != candidate.name
            || self.reference.reference_kind == EdgeKind::Calls
        {
            return true;
        }
        let owner_qualified = segments[..segments.len() - 1].join("::");
        first_in_file(
            &owner_qualified,
            &candidate.file_path,
            |node| is_java_type_kind(node.kind),
            context,
        )
        .is_none_or(|owner| self.visible(&owner, context))
    }

    /// Never `java.lang.Character`'s `Character.MIN_SUPPLEMENTARY_CODE_POINT`
    /// for jsoup's `TokenType.Character`.
    fn enum_constant_visible(&self, candidate: &Node, context: &dyn ResolutionContext) -> bool {
        if candidate.file_path == self.reference.file_path {
            return true;
        }
        let segments: Vec<&str> = candidate.qualified_name.split("::").collect();
        let enum_name = segments
            .len()
            .checked_sub(2)
            .map_or("", |index| segments[index]);
        let line = self.line(context);
        let name = regex::escape(&self.reference.reference_name);
        let in_case = Regex::new(&format!(
            r"(?-u:\b)case(?-u:\b)[^:;]*(?-u:\b){name}(?-u:\b)"
        ))
        .is_ok_and(|label| label.is_match(line));
        let through_enum = Regex::new(&format!(
            r"(?-u:\b){}\s*\.\s*{name}(?-u:\b)",
            regex::escape(enum_name)
        ))
        .is_ok_and(|written| written.is_match(line));
        if in_case || through_enum {
            return true;
        }
        let package = file_scope(&candidate.file_path, context);
        let enum_path = std::iter::once(package.package.as_str())
            .chain(
                segments
                    .get(1..segments.len().saturating_sub(1))
                    .unwrap_or_default()
                    .iter()
                    .copied(),
            )
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(".");
        let here = self.here(context);
        here.single
            .contains(&format!("{enum_path}.{}", self.reference.reference_name))
            || here.demand.contains(&enum_path)
    }

    /// halo's `Context`, retrofit's `Builder`, jsoup's `Attribute` (meant:
    /// `Evaluator.Attribute`) reached a same-named type nothing imported.
    fn type_visible(&self, candidate: &Node, context: &dyn ResolutionContext) -> bool {
        let segments: Vec<&str> = candidate.qualified_name.split("::").collect();
        let candidate_scope = file_scope(&candidate.file_path, context);
        let package = candidate_scope.package.as_str();
        // The qualified name leads with the package when there is one.
        let type_path = if !package.is_empty() && segments.first() == Some(&package) {
            &segments[1..]
        } else {
            &segments[..]
        };
        let path = std::iter::once(package)
            .chain(type_path.iter().copied())
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(".");
        let written = self.written(context);
        if written.through_new {
            return true;
        }
        // Nested only inside a type the file declares; a class local to a
        // method is the lexical rule's to judge.
        let owner_qualified = segments[..segments.len().saturating_sub(1)].join("::");
        let owner = (segments.len() > 1)
            .then(|| {
                first_in_file(
                    &owner_qualified,
                    &candidate.file_path,
                    |node| !matches!(node.kind, NodeKind::Namespace | NodeKind::File),
                    context,
                )
            })
            .flatten();
        if owner
            .as_ref()
            .is_some_and(|owner| !is_java_type_kind(owner.kind))
        {
            return true;
        }
        let owner_name = owner.as_ref().map_or("", |owner| owner.name.as_str());
        let nested = owner.is_some();
        let owner_suffix = format!(".{owner_name}");
        if written.qualifiers.iter().any(|qualifier| {
            if nested {
                qualifier == owner_name || qualifier.ends_with(&owner_suffix)
            } else {
                qualifier == package
            }
        }) {
            return true;
        }
        let here = self.here(context);
        if !nested {
            if candidate.file_path == self.reference.file_path || package == here.package {
                return true;
            }
            return here.single.contains(&path) || here.demand.contains(package);
        }
        // Nested: inside its owner, a subtype of it, or imported.
        let owner_prefix = format!("{owner_qualified}::");
        if self.enclosing(context).iter().any(|(qualified, _)| {
            *qualified == owner_qualified || qualified.starts_with(&owner_prefix)
        }) {
            return true;
        }
        if self.around(context).contains(owner_name) {
            return true;
        }
        let owner_path = path.rsplit_once('.').map_or("", |(owner, _)| owner);
        here.single.contains(&path) || here.demand.contains(owner_path)
    }

    fn here(&self, context: &dyn ResolutionContext) -> &JavaFileScope {
        self.here
            .get_or_init(|| file_scope(&self.reference.file_path, context))
    }

    /// The reference's own line, or empty.
    fn line(&self, context: &dyn ResolutionContext) -> &str {
        self.line.get_or_init(|| {
            context
                .source_facts(&self.reference.file_path)
                .and_then(|facts| {
                    usize::try_from(self.reference.line)
                        .ok()
                        .and_then(|line| line.checked_sub(1))
                        .filter(|index| *index < facts.line_count())
                        .map(|index| facts.line(index).to_string())
                })
                .unwrap_or_default()
        })
    }

    fn written(&self, context: &dyn ResolutionContext) -> &Written {
        self.written.get_or_init(|| {
            let line = self.line(context);
            // Both shapes write a `.` before the name.
            if !line.contains('.') {
                return Written::default();
            }
            let name = regex::escape(&self.reference.reference_name);
            let through_new = Regex::new(&format!(r"\.\s*new\s+{name}(?-u:\b)"))
                .is_ok_and(|shape| shape.is_match(line));
            let qualifiers = Regex::new(&format!(
                r"([A-Za-z_$][A-Za-z0-9_$.]*)\s*\.\s*(?:@[A-Za-z0-9_.]+(?:\([^)]*\))?\s+)*{name}(?-u:\b)"
            ))
            .map(|shape| {
                shape
                    .captures_iter(line)
                    .map(|caps| caps[1].to_string())
                    .collect()
            })
            .unwrap_or_default();
            Written {
                through_new,
                qualifiers,
            }
        })
    }

    fn enclosing(&self, context: &dyn ResolutionContext) -> &[(String, String)] {
        self.enclosing.get_or_init(|| {
            context
                .get_nodes_in_file_shared(&self.reference.file_path)
                .iter()
                .filter(|node| {
                    is_java_type_kind(node.kind)
                        && node.start_line <= self.reference.line
                        && node.end_line >= self.reference.line
                })
                .map(|node| (node.qualified_name.clone(), node.name.clone()))
                .collect()
        })
    }

    /// An anonymous class (`new NodeFilter() { … }`, named
    /// `<NodeFilter$anon@N>`) derives from what it instantiates.
    fn around(&self, context: &dyn ResolutionContext) -> &HashSet<String> {
        self.around.get_or_init(|| {
            let mut names = HashSet::new();
            for (qualified, name) in self.enclosing(context) {
                let Some(instantiated) = anonymous_supertype(name, qualified) else {
                    names.extend(ancestor_names(qualified, context));
                    continue;
                };
                names.insert(instantiated.to_string());
                for declaration in context.get_nodes_by_name_shared(instantiated) {
                    if declaration.language == Language::Java && is_java_type_kind(declaration.kind)
                    {
                        names.extend(ancestor_names(&declaration.qualified_name, context));
                    }
                }
            }
            names
        })
    }
}

/// Whether a bare Java name at `reference` can mean `candidate` (upstream
/// #2236 `isJavaTypeVisible`); [`JavaTypeSite::visible`] for one candidate.
pub(crate) fn is_java_type_visible(
    candidate: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    JavaTypeSite::new(reference).visible(candidate, context)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listed_names_follow_upstream_lookahead() {
        assert_eq!(
            listed_names(" Base implements Foo, Bar "),
            ["Base", "Foo", "Bar"]
        );
        assert_eq!(listed_names(" a.b.Base "), ["Base"]);
        // A name followed by anything else is no supertype.
        assert_eq!(listed_names(" B permits C, D "), ["C", "D"]);
        assert_eq!(listed_names(" Foo$Bar "), ["Bar"]);
        assert_eq!(listed_names(" 9ab "), ["ab"]);
        assert!(listed_names(" Comparable> ").is_empty());
        assert_eq!(listed_names(" X implementsY"), ["implementsY"]);
    }

    #[test]
    fn type_arguments_go_two_levels_deep() {
        assert_eq!(without_type_arguments("Map<K, List<V>> x"), "Map x");
        assert_eq!(without_type_arguments("A<B<C<D>>>"), "A<B>");
    }

    #[test]
    fn file_scope_reads_package_imports_and_static_imports() {
        let scope = JavaFileScope::read(
            "// package nope;\npackage app.web;\n\nimport java.util.List;\nimport static org.junit.Assert.*;\nimport static org.mockito.Mockito.verify;\nimport app.model.*;\n/* import gone.Thing; */\n",
        );
        assert_eq!(scope.package, "app.web");
        assert!(scope.single.contains("java.util.List"));
        assert!(scope.single.contains("org.mockito.Mockito.verify"));
        assert!(scope.demand.contains("org.junit.Assert"));
        assert!(scope.demand.contains("app.model"));
        assert!(!scope.single.contains("gone.Thing"));
        assert!(scope.static_owners.contains("Assert"));
        assert!(scope.static_members.contains("Mockito.verify"));
    }

    #[test]
    fn heads_are_read_by_both_readers() {
        let facts = SourceFacts::new(Arc::from(
            "public class Child<T extends Number>\n    extends Base<Map<String, T>>\n    implements a.b.Runner, Closeable {\n}\n",
        ));
        let head = JavaHead::read(&facts, 1);
        // `javaSupertypesOf` reads the first clause: `extends Number>` here.
        assert_eq!(head.supertypes, ["Number", "Base", "Runner", "Closeable"]);
        assert_eq!(head.ancestors, ["Base", "Runner", "Closeable"]);
        let plain = SourceFacts::new(Arc::from("class Alone {\n}\n"));
        let head = JavaHead::read(&plain, 1);
        assert!(head.supertypes.is_empty() && head.ancestors.is_empty());
    }

    #[test]
    fn anonymous_classes_name_what_they_instantiate() {
        assert_eq!(
            anonymous_supertype("<NodeFilter$anon@5>", "a::B::f::<NodeFilter$anon@5>"),
            Some("NodeFilter")
        );
        assert_eq!(anonymous_supertype("Plain", "a::Plain"), None);
    }

    #[test]
    fn identifiers_are_ascii_with_dollar() {
        assert!(is_java_identifier("$x_1"));
        assert!(!is_java_identifier("1x"));
        assert!(!is_java_identifier("a.b"));
        assert!(!is_java_identifier(""));
    }
}
