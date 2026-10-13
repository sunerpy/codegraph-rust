//! C# type names read at their site (W4-CS-01, upstream #2121's C# half,
//! #2216 and #2220 in their `b635dd46` shape).
//!
//! A bare C# name (`TestContext`, `Easing`, `Helper()`) can mean a member
//! only of the types around it, of their bases, or of a type a static using
//! brings in (upstream #2151).
//!
//! A reference in a type position (`Type name`, `List<int>`, `Type?`,
//! `Type[]`, a generic argument, `new X`) names a type, never a member that
//! shares the name. A bare type name of namespace `N` is in reach of a file
//! whose code runs in `N` or a namespace inside it, or that imports `N`: its
//! own `using N;`, the `global using N;` of its project's files (every file's,
//! outside any project), the `<Using Include="N" />` of the project files
//! above it, the SDK's implicit usings, or an alias naming the type. A nested
//! type is reached by bare name only from inside its owner, or from a type
//! deriving from it through any partial part.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::types::{RefView, ResolutionContext};

/// Upstream `CSHARP_TYPE_KINDS` (`record` has no kind of its own here).
pub fn is_csharp_type_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class | NodeKind::Interface | NodeKind::Enum | NodeKind::Struct
    )
}

/// JavaScript's `\s`, `U+FEFF` included: a C# file often opens with a byte
/// order mark, which upstream's `^\s*namespace` / `^\s*using` read past.
const JS_SPACE: &str = r"[\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]";

/// A pattern with every `\s` read as JavaScript's.
fn js_regex(pattern: &str) -> Regex {
    Regex::new(&pattern.replace(r"\s", JS_SPACE)).expect("C# reader pattern")
}

/// JavaScript's `\w`.
fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `^[A-Za-z_]\w*$`.
pub(crate) fn is_simple_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(is_word)
}

/// Segment `line` (1-based) of `file.split('\n')`.
fn raw_line(file: &str, line: i64, context: &dyn ResolutionContext) -> Option<String> {
    let facts = context.source_facts(file)?;
    let index = usize::try_from(line).ok()?.checked_sub(1)?;
    facts.raw_line(index).map(str::to_string)
}

/// Whether a C# reference's site is a type position (upstream #2121
/// `isDotNetTypeRef`, its C# half): `Type sourceType`, `List<int>`,
/// `new TypeMap()`, `Exception? e`, `Dictionary<string, Type>`. Read from the
/// source at the reference's column; a member read (`Builder.Services`), a
/// method group (`MapGet("/x", GetItems)`) or a route's handler is not one.
pub(crate) fn is_csharp_type_ref(reference: &RefView, context: &dyn ResolutionContext) -> bool {
    static NEW: OnceLock<Regex> = OnceLock::new();
    static DECLARES: OnceLock<Regex> = OnceLock::new();
    static OPERATOR: OnceLock<Regex> = OnceLock::new();
    static SHAPE: OnceLock<Regex> = OnceLock::new();
    static ARGUMENT: OnceLock<Regex> = OnceLock::new();
    static OPEN_GENERIC: OnceLock<Regex> = OnceLock::new();
    if reference.language != Language::CSharp
        || !matches!(
            reference.reference_kind,
            EdgeKind::References | EdgeKind::Instantiates | EdgeKind::TypeOf | EdgeKind::Returns
        )
    {
        return false;
    }
    let name = reference.reference_name.as_str();
    if !is_simple_identifier(name) {
        return false;
    }
    // `new TypeMap()` constructs a type; the reference's column is the `new`.
    if reference.reference_kind == EdgeKind::Instantiates {
        return true;
    }
    let Some(line) = raw_line(&reference.file_path, reference.line, context) else {
        return false;
    };
    let column = usize::try_from(reference.column).unwrap_or(usize::MAX);
    if !line
        .get(column..)
        .is_some_and(|rest| rest.starts_with(name))
    {
        return false;
    }
    let before = &line[..column];
    let after = &line[column + name.len()..];
    let new = NEW.get_or_init(|| js_regex(r"(?-u:\b)new\s+$"));
    if new.is_match(before) {
        return true;
    }
    // `Type name`: a declaration names its type first.
    let declares = DECLARES.get_or_init(|| js_regex(r"^\s+@?[A-Za-z_]"));
    if declares.is_match(after) {
        let operator = OPERATOR
            .get_or_init(|| js_regex(r"^\s+(?:is|as|and|or|not|when|in|switch|with)(?-u:\b)"));
        return !operator.is_match(after);
    }
    // `List<int>`, `Type?` (not `?.`, `??`, `?[`), `Type[]`, and the
    // arguments of a generic.
    let shape = SHAPE.get_or_init(|| js_regex(r"^<|^\[\s*[,\]]"));
    if shape.is_match(after)
        || after
            .strip_prefix('?')
            .is_some_and(|rest| !rest.starts_with(['.', '?', '[']))
    {
        return true;
    }
    let argument = ARGUMENT.get_or_init(|| js_regex(r"^\s*[,>]"));
    let open = OPEN_GENERIC.get_or_init(|| Regex::new(r"<[^<>()]*$").expect("open generic"));
    argument.is_match(after) && open.is_match(before)
}

/// Whether a candidate can be what a C# type position names: a property, a
/// method (a constructor is one), an enum case or a field shares the type's
/// name, not its meaning (upstream #2121 `canNameInTypePosition`).
pub(crate) fn can_name_in_type_position(candidate: &Node) -> bool {
    !matches!(
        candidate.kind,
        NodeKind::Property | NodeKind::Method | NodeKind::EnumMember | NodeKind::Field
    )
}

/// The namespaces `<ImplicitUsings>enable</ImplicitUsings>` imports into every
/// file (Microsoft.NET.Sdk; upstream `CSHARP_IMPLICIT_USINGS`).
const CSHARP_IMPLICIT_USINGS: [&str; 7] = [
    "System",
    "System.Collections.Generic",
    "System.IO",
    "System.Linq",
    "System.Net.Http",
    "System.Threading",
    "System.Threading.Tasks",
];

/// The namespaces a C# file's code runs in and the ones it imports
/// (upstream `csharpNamespaceScope`).
#[derive(Debug, Default)]
pub struct CsharpNamespaceScope {
    pub namespaces: Vec<String>,
    pub usings: HashSet<String>,
    pub aliases: HashMap<String, String>,
}

/// What the project files above a directory bring in (upstream
/// `csharpProjectUsings`): their `<Using Include>`s, the SDK's implicit
/// usings, and the `global using`s of the outermost project's own files;
/// `project` says whether a `.csproj` sits there or above.
#[derive(Debug, Clone, Default)]
struct ProjectUsings {
    usings: HashSet<String>,
    project: bool,
}

/// The ancestor names of a C# type, by its qualified name and the walk's
/// depth.
type AncestorMemo = HashMap<(String, usize), Arc<HashSet<String>>>;

/// The project-wide C# facts a resolution pass reads.
#[derive(Debug, Default)]
pub struct CsharpTables {
    project_usings: Mutex<HashMap<String, Arc<ProjectUsings>>>,
    global_usings: Mutex<HashMap<String, Arc<HashSet<String>>>>,
    ancestors: Mutex<AncestorMemo>,
    /// The bases every declaration of a type name lists (#2151), by name.
    supertypes: Mutex<HashMap<String, Arc<Vec<String>>>>,
    /// The types a file's static usings bring in (#2151), by file.
    static_usings: Mutex<HashMap<String, Arc<HashSet<String>>>>,
    /// The types the static usings of the project files from a directory up
    /// and every `global using static` bring in (#2151), by directory.
    project_static_usings: Mutex<HashMap<String, Arc<HashSet<String>>>>,
    /// The types a file's `using Name = …;` aliases name (#2171), by file.
    aliases: Mutex<HashMap<String, Arc<HashMap<String, String>>>>,
}

impl CsharpTables {
    pub fn new() -> Self {
        Self::default()
    }
}

/// `using X;` / `global using X;` / `using A = X;`, not `using static`, as
/// upstream's `^\s*(?:global\s+)?using\s+(?!static\b)(?:([A-Za-z_]\w*)\s*=\s*)?([\w.]+)\s*;`
/// reads them: `(alias, target)`.
pub fn using_directives(text: &str) -> Vec<(Option<String>, String)> {
    static USING: OnceLock<Regex> = OnceLock::new();
    let using = USING.get_or_init(|| {
        js_regex(
            r"(?m)^\s*(?:global\s+)?using\s+(?:([A-Za-z_][A-Za-z0-9_]*)\s*=\s*)?([A-Za-z0-9_.]+)\s*;",
        )
    });
    using
        .captures_iter(text)
        .filter(|caps| !starts_with_static(text, caps))
        .map(|caps| {
            (
                caps.get(1).map(|m| m.as_str().to_string()),
                caps[2].to_string(),
            )
        })
        .collect()
}

/// Whether a `using` match is `using static …` (upstream's `(?!static\b)`
/// right after `using\s+`): the text at the first name after the keyword's
/// blanks is `static` and no word character follows it.
fn starts_with_static(text: &str, caps: &regex::Captures<'_>) -> bool {
    caps.get(1)
        .or_else(|| caps.get(2))
        .is_some_and(|m| is_static_at(text, m.start()))
}

fn is_static_at(text: &str, at: usize) -> bool {
    text[at..]
        .strip_prefix("static")
        .is_some_and(|rest| !rest.starts_with(is_word))
}

/// The `global using X;` (not `static`) of `text`, as upstream's
/// `^\s*global\s+using\s+(?!static\b)([\w.]+)\s*;` reads them.
pub fn global_usings(text: &str) -> Vec<String> {
    static GLOBAL: OnceLock<Regex> = OnceLock::new();
    let global = GLOBAL.get_or_init(|| js_regex(r"(?m)^\s*global\s+using\s+([A-Za-z0-9_.]+)\s*;"));
    global
        .captures_iter(text)
        .filter(|caps| caps.get(1).is_some_and(|m| !is_static_at(text, m.start())))
        .map(|caps| caps[1].to_string())
        .collect()
}

/// The `using static X;` and `global using static X;` of `text`, as
/// upstream's `^\s*(?:global\s+)?using\s+static\s+([\w.]+)\s*;` reads them.
pub fn file_static_usings(text: &str) -> Vec<String> {
    static STATIC: OnceLock<Regex> = OnceLock::new();
    STATIC
        .get_or_init(|| js_regex(r"(?m)^\s*(?:global\s+)?using\s+static\s+([A-Za-z0-9_.]+)\s*;"))
        .captures_iter(text)
        .map(|caps| caps[1].to_string())
        .collect()
}

/// The `global using static X;` of `text`, as upstream's
/// `^\s*global\s+using\s+static\s+([\w.]+)\s*;` reads them.
pub fn global_static_usings(text: &str) -> Vec<String> {
    static GLOBAL: OnceLock<Regex> = OnceLock::new();
    GLOBAL
        .get_or_init(|| js_regex(r"(?m)^\s*global\s+using\s+static\s+([A-Za-z0-9_.]+)\s*;"))
        .captures_iter(text)
        .map(|caps| caps[1].to_string())
        .collect()
}

/// The `<Using Include="X" … Static="true"/>` of a project file's text, as
/// upstream's `<Using\s+Include\s*=\s*"([\w.]+)"[^>]*\bStatic\s*=\s*"true"`
/// reads them.
pub fn project_file_static_usings(text: &str) -> Vec<String> {
    static STATIC: OnceLock<Regex> = OnceLock::new();
    STATIC
        .get_or_init(|| {
            js_regex(
                r#"(?i)<Using\s+Include\s*=\s*"([A-Za-z0-9_.]+)"[^>]*(?-u:\b)Static\s*=\s*"true""#,
            )
        })
        .captures_iter(text)
        .map(|caps| caps[1].to_string())
        .collect()
}

/// The `using Name = Some.Namespace.Type;` (or `global using`) aliases of
/// `text` and the simple name of the type each names, as upstream #2171's
/// `^\s*(?:global\s+)?using\s+([A-Za-z_]\w*)\s*=\s*(?:global::)?([\w.]+)\s*(?:<[^;>]*>)?\s*;`
/// reads them.
pub fn using_alias_types(text: &str) -> Vec<(String, String)> {
    static ALIAS: OnceLock<Regex> = OnceLock::new();
    ALIAS
        .get_or_init(|| {
            js_regex(
                r"(?m)^\s*(?:global\s+)?using\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(?:global::)?([A-Za-z0-9_.]+)\s*(?:<[^;>]*>)?\s*;",
            )
        })
        .captures_iter(text)
        .map(|caps| (caps[1].to_string(), last_segment(&caps[2]).to_string()))
        .collect()
}

/// The type a C# file's `using` alias names `name` as, by its simple name
/// (upstream #2171 `csharpUsingAlias`), read once per file and pass.
pub(crate) fn csharp_using_alias(
    name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    if !is_simple_identifier(name) {
        return None;
    }
    let tables = context.csharp_tables();
    let cached = tables
        .aliases
        .lock()
        .ok()
        .and_then(|memo| memo.get(&reference.file_path).cloned());
    let aliases = match cached {
        Some(hit) => hit,
        None => {
            let aliases: HashMap<String, String> = context
                .source_facts(&reference.file_path)
                .map(|facts| using_alias_types(facts.source()).into_iter().collect())
                .unwrap_or_default();
            let aliases = Arc::new(aliases);
            if let Ok(mut memo) = tables.aliases.lock() {
                memo.insert(reference.file_path.clone(), Arc::clone(&aliases));
            }
            aliases
        }
    };
    aliases.get(name).cloned()
}

/// The last `.` segment of a dotted name (`split('.').pop()`).
fn last_segment(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// The namespaces a C# file declares with `namespace X` at a line's start
/// (upstream reads every one, block or file-scoped, after comments are
/// blanked).
pub fn namespace_declarations(stripped: &str) -> Vec<String> {
    static NAMESPACE: OnceLock<Regex> = OnceLock::new();
    NAMESPACE
        .get_or_init(|| js_regex(r"(?m)^\s*namespace\s+([A-Za-z0-9_.]+)"))
        .captures_iter(stripped)
        .map(|caps| caps[1].to_string())
        .collect()
}

/// `path.posix.dirname`.
fn dirname(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) => "/",
        Some(cut) => &path[..cut],
        None => ".",
    }
}

fn is_root_dir(dir: &str) -> bool {
    matches!(dir, "." | "" | "/")
}

/// The `.csproj` and `.props` files of `dir` (a directory of the project,
/// `.` for its root), by name: regular files only, a symlink is not followed.
fn project_files(dir: &str, context: &dyn ResolutionContext) -> Vec<String> {
    let root = Path::new(context.get_project_root());
    let full = if is_root_dir(dir) {
        root.to_path_buf()
    } else {
        root.join(dir)
    };
    let Ok(entries) = std::fs::read_dir(full) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            let lower = name.to_ascii_lowercase();
            lower.ends_with(".csproj") || lower.ends_with(".props")
        })
        .collect();
    names.sort();
    names
}

/// The `<Using Include="X" />` (not `Static="true"`) of a project file's
/// text, and whether it turns the SDK's implicit usings on.
pub fn project_file_usings(text: &str) -> (Vec<String>, bool) {
    static USING: OnceLock<Regex> = OnceLock::new();
    static STATIC: OnceLock<Regex> = OnceLock::new();
    static IMPLICIT: OnceLock<Regex> = OnceLock::new();
    let using =
        USING.get_or_init(|| js_regex(r#"(?i)<Using\s+Include\s*=\s*"([A-Za-z0-9_.]+)"([^>]*)>"#));
    let is_static = STATIC.get_or_init(|| js_regex(r#"(?i)(?-u:\b)Static\s*=\s*"true""#));
    let implicit = IMPLICIT
        .get_or_init(|| js_regex(r"(?i)<ImplicitUsings>\s*(?:enable|true)\s*</ImplicitUsings>"));
    let usings = using
        .captures_iter(text)
        .filter(|caps| !is_static.is_match(&caps[2]))
        .map(|caps| caps[1].to_string())
        .collect();
    (usings, implicit.is_match(text))
}

/// The `global using X;` of the `.cs` files under `dir` (`.` = the whole
/// repository; upstream `csharpGlobalUsings`), once per pass.
fn csharp_global_usings(dir: &str, context: &dyn ResolutionContext) -> Arc<HashSet<String>> {
    let tables = context.csharp_tables();
    if let Some(hit) = tables
        .global_usings
        .lock()
        .ok()
        .and_then(|memo| memo.get(dir).cloned())
    {
        return hit;
    }
    let prefix = if dir == "." {
        String::new()
    } else {
        format!("{dir}/")
    };
    let mut usings = HashSet::new();
    for file in context.get_all_files_shared().iter() {
        if !file.ends_with(".cs") || !file.starts_with(&prefix) {
            continue;
        }
        let Some(facts) = context.source_facts(file) else {
            continue;
        };
        if !facts.source().contains("global using") {
            continue;
        }
        usings.extend(global_usings(facts.source()));
    }
    let usings = Arc::new(usings);
    if let Ok(mut memo) = tables.global_usings.lock() {
        memo.insert(dir.to_string(), Arc::clone(&usings));
    }
    usings
}

/// What the project files from `dir` up bring in (upstream
/// `csharpProjectUsings`), once per pass: the outermost `.csproj` on the way
/// marks the project, whose own files' `global using`s join; each project
/// file adds its `<Using Include>`s and, when on, the implicit usings.
fn csharp_project_usings(dir: &str, context: &dyn ResolutionContext) -> Arc<ProjectUsings> {
    let tables = context.csharp_tables();
    if let Some(hit) = tables
        .project_usings
        .lock()
        .ok()
        .and_then(|memo| memo.get(dir).cloned())
    {
        return hit;
    }
    let root = is_root_dir(dir);
    let mut result = if root {
        ProjectUsings::default()
    } else {
        csharp_project_usings(dirname(dir), context)
            .as_ref()
            .clone()
    };
    for entry in project_files(dir, context) {
        let relative = if root {
            entry.clone()
        } else {
            format!("{dir}/{entry}")
        };
        let Some(text) = context.read_file_shared(&relative) else {
            continue;
        };
        if entry.to_ascii_lowercase().ends_with(".csproj") && !result.project {
            result.project = true;
            let at = if root { "." } else { dir };
            result
                .usings
                .extend(csharp_global_usings(at, context).iter().cloned());
        }
        let (usings, implicit) = project_file_usings(&text);
        result.usings.extend(usings);
        if implicit {
            result
                .usings
                .extend(CSHARP_IMPLICIT_USINGS.iter().map(|ns| ns.to_string()));
        }
    }
    let result = Arc::new(result);
    if let Ok(mut memo) = tables.project_usings.lock() {
        memo.insert(dir.to_string(), Arc::clone(&result));
    }
    result
}

/// `file`'s namespace scope (upstream `csharpNamespaceScope`), read once.
pub(crate) fn csharp_namespace_scope(
    file: &str,
    context: &dyn ResolutionContext,
) -> Arc<CsharpNamespaceScope> {
    let Some(facts) = context.source_facts(file) else {
        return Arc::new(CsharpNamespaceScope::default());
    };
    facts.csharp_namespace_scope(|| {
        let text = strip_comments_for_regex(facts.source(), CommentLang::Java);
        let project = csharp_project_usings(dirname(file), context);
        let mut usings = project.usings.clone();
        if !project.project {
            usings.extend(csharp_global_usings(".", context).iter().cloned());
        }
        let mut aliases = HashMap::new();
        for (alias, target) in using_directives(&text) {
            match alias {
                Some(alias) => {
                    aliases.insert(alias, target);
                }
                None => {
                    usings.insert(target);
                }
            }
        }
        CsharpNamespaceScope {
            namespaces: namespace_declarations(&text),
            usings,
            aliases,
        }
    })
}

/// Whether a bare C# type name can mean `n` (upstream #2216
/// `isCsharpTypeVisible`): a type in namespace `N` is seen from `N` and the
/// namespaces inside it, and through a `using N;`; an alias names one type;
/// a type in the global namespace is seen everywhere.
pub(crate) fn is_csharp_type_visible(
    n: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    let Some(cut) = n.qualified_name.find("::") else {
        return true;
    };
    let namespace = &n.qualified_name[..cut];
    let scope = csharp_namespace_scope(&reference.file_path, context);
    if let Some(aliased) = scope.aliases.get(&reference.reference_name) {
        return *aliased
            == format!(
                "{namespace}.{}",
                n.qualified_name[cut + 2..].replace("::", ".")
            );
    }
    if scope
        .namespaces
        .iter()
        .any(|own| own == namespace || own.starts_with(&format!("{namespace}.")))
    {
        return true;
    }
    scope.usings.contains(namespace)
}

/// The simple names one C# type declaration's base list names, read from
/// its head (up to seven lines, to the first `{`, a `where` clause dropped;
/// upstream `csharpAncestorNames`' reader).
pub fn csharp_declared_bases(facts: &crate::source_facts::SourceFacts, decl: &Node) -> Vec<String> {
    static LIST: OnceLock<Regex> = OnceLock::new();
    static WHERE: OnceLock<Regex> = OnceLock::new();
    static NAME: OnceLock<Regex> = OnceLock::new();
    let list_re = LIST.get_or_init(|| js_regex(r":\s*([^{;]*)"));
    let where_re =
        WHERE.get_or_init(|| Regex::new(r"(?s)(?-u:\b)where(?-u:\b).*$").expect("where"));
    let name_re = NAME.get_or_init(|| js_regex(r"([A-Za-z_][A-Za-z0-9_]*)\s*(?:<.*)?$"));
    let Ok(start) = usize::try_from(decl.start_line) else {
        return Vec::new();
    };
    let mut header = String::new();
    let mut index = start.saturating_sub(1);
    let end = facts.line_count().min(start + 6);
    while index < end && !header.contains('{') {
        header.push_str(facts.line(index));
        header.push(' ');
        index += 1;
    }
    let head = header.split('{').next().unwrap_or_default();
    let head = where_re.replace(head, "");
    let Some(list) = list_re.captures(&head).map(|caps| caps[1].to_string()) else {
        return Vec::new();
    };
    crate::cpp_namespaces::split_cpp_top_level(&list)
        .iter()
        .filter_map(|base| {
            name_re
                .captures(base.trim())
                .map(|caps| caps[1].to_string())
        })
        .collect()
}

/// The simple names of the C# types `qualified` derives from, through every
/// partial part and base, four levels up (upstream `csharpAncestorNames`;
/// memoized by name and depth, so a pass's order cannot change a result).
fn csharp_ancestor_names(
    qualified: &str,
    depth: usize,
    context: &dyn ResolutionContext,
) -> Arc<HashSet<String>> {
    let tables = context.csharp_tables();
    let key = (qualified.to_string(), depth);
    if let Some(hit) = tables
        .ancestors
        .lock()
        .ok()
        .and_then(|memo| memo.get(&key).cloned())
    {
        return hit;
    }
    let mut names: HashSet<String> = HashSet::new();
    for decl in context.get_nodes_by_qualified_name_shared(qualified) {
        if decl.language != Language::CSharp || !is_csharp_type_kind(decl.kind) {
            continue;
        }
        if let Some(facts) = context.source_facts(&decl.file_path) {
            names.extend(csharp_declared_bases(&facts, &decl));
        }
    }
    if depth < 4 {
        let bases: Vec<String> = names.iter().cloned().collect();
        for base in bases {
            for t in context.get_nodes_by_name_shared(&base) {
                if t.language != Language::CSharp
                    || !is_csharp_type_kind(t.kind)
                    || t.qualified_name == qualified
                {
                    continue;
                }
                names.extend(
                    csharp_ancestor_names(&t.qualified_name, depth + 1, context)
                        .iter()
                        .cloned(),
                );
            }
        }
    }
    let names = Arc::new(names);
    if let Ok(mut memo) = tables.ancestors.lock() {
        memo.insert(key, Arc::clone(&names));
    }
    names
}

/// Whether a bare C# name can reach `n` as a nested type (upstream #2220
/// `isCsharpNestedTypeInScope`): only from inside the type declaring it (any
/// partial part, any depth) or a class deriving from it.
pub(crate) fn is_csharp_nested_type_in_scope(
    n: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    let Some(owner) = csharp_owner_name(&n.qualified_name) else {
        return true;
    };
    let Some(owner_type) = context
        .get_nodes_in_file_shared(&n.file_path)
        .into_iter()
        .find(|p| is_csharp_owner_type(p, owner))
    else {
        // Declared in a namespace, not a type.
        return true;
    };
    let enclosing: Vec<Arc<Node>> = context
        .get_nodes_in_file_shared(&reference.file_path)
        .into_iter()
        .filter(|p| {
            is_csharp_type_kind(p.kind)
                && p.start_line <= reference.line
                && p.end_line >= reference.line
        })
        .collect();
    let nested = format!("{owner}::");
    if enclosing
        .iter()
        .any(|p| p.qualified_name == owner || p.qualified_name.starts_with(&nested))
    {
        return true;
    }
    enclosing
        .iter()
        .any(|p| csharp_ancestor_names(&p.qualified_name, 0, context).contains(&owner_type.name))
}

/// The types a C# file sees through static usings (upstream #2151
/// `csharpStaticUsings`): its own `using static`s, every file's `global using
/// static`, and the `<Using … Static="true"/>` of the project files above it,
/// by their last segment.
fn csharp_static_usings(file: &str, context: &dyn ResolutionContext) -> Arc<HashSet<String>> {
    let tables = context.csharp_tables();
    if let Some(hit) = tables
        .static_usings
        .lock()
        .ok()
        .and_then(|memo| memo.get(file).cloned())
    {
        return hit;
    }
    let mut owners = csharp_project_static_usings(dirname(file), context)
        .as_ref()
        .clone();
    if let Some(facts) = context.source_facts(file) {
        owners.extend(
            file_static_usings(facts.source())
                .iter()
                .map(|name| last_segment(name).to_string()),
        );
    }
    let owners = Arc::new(owners);
    if let Ok(mut memo) = tables.static_usings.lock() {
        memo.insert(file.to_string(), Arc::clone(&owners));
    }
    owners
}

/// The static usings that apply to every file under `dir` (upstream
/// `csharpProjectStaticUsings`): every `global using static` of the
/// repository, and the `<Using … Static="true"/>` of the project files on the
/// way down to `dir`.
fn csharp_project_static_usings(
    dir: &str,
    context: &dyn ResolutionContext,
) -> Arc<HashSet<String>> {
    let tables = context.csharp_tables();
    if let Some(hit) = tables
        .project_static_usings
        .lock()
        .ok()
        .and_then(|memo| memo.get(dir).cloned())
    {
        return hit;
    }
    let root = is_root_dir(dir);
    let mut owners: HashSet<String> = if root {
        let mut owners = HashSet::new();
        for file in context.get_all_files_shared().iter() {
            if !file.ends_with(".cs") {
                continue;
            }
            let Some(facts) = context.source_facts(file) else {
                continue;
            };
            if !facts.source().contains("global using static") {
                continue;
            }
            owners.extend(
                global_static_usings(facts.source())
                    .iter()
                    .map(|name| last_segment(name).to_string()),
            );
        }
        owners
    } else {
        csharp_project_static_usings(dirname(dir), context)
            .as_ref()
            .clone()
    };
    for entry in project_files(dir, context) {
        let relative = if root {
            entry
        } else {
            format!("{dir}/{entry}")
        };
        let Some(text) = context.read_file_shared(&relative) else {
            continue;
        };
        owners.extend(
            project_file_static_usings(&text)
                .iter()
                .map(|name| last_segment(name).to_string()),
        );
    }
    let owners = Arc::new(owners);
    if let Ok(mut memo) = tables.project_static_usings.lock() {
        memo.insert(dir.to_string(), Arc::clone(&owners));
    }
    owners
}

/// The simple names one C# type declaration derives from as upstream #2151's
/// `csharpSupertypesOf` reads them: its head (up to nine lines, to the first
/// `{`) with everything inside `<…>` and `(…)` dropped, the list after
/// `Name :` up to a `where`, each dotted name by its last segment.
pub fn csharp_declared_supers(
    facts: &crate::source_facts::SourceFacts,
    decl: &Node,
) -> Vec<String> {
    static NAME: OnceLock<Regex> = OnceLock::new();
    let Ok(start) = usize::try_from(decl.start_line) else {
        return Vec::new();
    };
    let from = start.saturating_sub(1);
    let to = (start + 8).min(facts.line_count());
    let head = if from < to {
        facts.join_lines(from, to, " ")
    } else {
        String::new()
    };
    let head = &head[..head.find('{').unwrap_or(head.len())];
    let mut depth = 0usize;
    let mut flat = String::new();
    for ch in head.chars() {
        match ch {
            '<' | '(' => depth += 1,
            '>' | ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => flat.push(ch),
            _ => {}
        }
    }
    // JavaScript's `.` stops at a line terminator.
    let Ok(list) = Regex::new(&format!(
        r"(?-u:\b){}{}*:{}*([^\n\r\x{{2028}}\x{{2029}}]*?)(?:(?-u:\b)where(?-u:\b)|$)",
        regex::escape(&decl.name),
        JS_SPACE,
        JS_SPACE
    )) else {
        return Vec::new();
    };
    let bases = list
        .captures(&flat)
        .and_then(|caps| caps.get(1).map(|m| m.as_str().to_string()))
        .unwrap_or_default();
    let name = NAME.get_or_init(|| Regex::new(r"[A-Za-z_][A-Za-z0-9_.]*").expect("base name"));
    name.find_iter(&bases)
        .map(|m| last_segment(m.as_str()).to_string())
        .collect()
}

/// The bases every C# declaration named `type_name` lists, by #2151's
/// reader (upstream `csharpSupertypesOf`), once per pass.
fn csharp_supertypes_of(type_name: &str, context: &dyn ResolutionContext) -> Arc<Vec<String>> {
    let tables = context.csharp_tables();
    if let Some(hit) = tables
        .supertypes
        .lock()
        .ok()
        .and_then(|memo| memo.get(type_name).cloned())
    {
        return hit;
    }
    let mut names = Vec::new();
    for decl in context.get_nodes_by_name_shared(type_name) {
        if decl.language != Language::CSharp || !is_csharp_type_kind(decl.kind) {
            continue;
        }
        if let Some(facts) = context.source_facts(&decl.file_path) {
            names.extend(csharp_declared_supers(&facts, &decl));
        }
    }
    let names = Arc::new(names);
    if let Ok(mut memo) = tables.supertypes.lock() {
        memo.insert(type_name.to_string(), Arc::clone(&names));
    }
    names
}

/// A bare C# name: a `calls` or `references` reference named like
/// `^[A-Za-z_]\w*$` (upstream #2151 `csharpBare`).
pub(crate) fn is_csharp_bare_name(reference: &RefView) -> bool {
    reference.language == Language::CSharp
        && matches!(
            reference.reference_kind,
            EdgeKind::Calls | EdgeKind::References
        )
        && is_simple_identifier(&reference.reference_name)
}

/// Whether a bare C# name, whose line shows no receiver, can mean `n`
/// (upstream #2151 `isCsharpMemberInScope`): a member only of the types
/// around the name (outer types included), of the bases their heads list
/// (every partial declaration, by name, at most 40 types), or of a type a
/// static using brings in. A name outside every type is not judged, nor is
/// anything but a member (no `event` kind here).
pub(crate) fn is_csharp_member_in_scope(
    n: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    if !matches!(
        n.kind,
        NodeKind::Method
            | NodeKind::Property
            | NodeKind::Field
            | NodeKind::EnumMember
            | NodeKind::Constant
    ) {
        return true;
    }
    let Some(owner) = csharp_owner_name(&n.qualified_name) else {
        return true;
    };
    let owner = last_segment(owner.rsplit("::").next().unwrap_or(owner));
    if csharp_static_usings(&reference.file_path, context).contains(owner) {
        return true;
    }
    let mut queue: VecDeque<String> = context
        .get_nodes_in_file_shared(&reference.file_path)
        .into_iter()
        .filter(|t| {
            is_csharp_type_kind(t.kind)
                && t.start_line <= reference.line
                && t.end_line >= reference.line
        })
        .map(|t| t.name.clone())
        .collect();
    // No type around the name: its declaration was not recovered.
    if queue.is_empty() {
        return true;
    }
    let mut seen: HashSet<String> = HashSet::new();
    while seen.len() < 40 {
        let Some(name) = queue.pop_front() else {
            break;
        };
        if !seen.insert(name.clone()) {
            continue;
        }
        if name == owner {
            return true;
        }
        queue.extend(csharp_supertypes_of(&name, context).iter().cloned());
    }
    false
}

/// The C# visibility rule of exact matching and of the cross-file veto
/// (upstream #2216, #2220): a C# type, by a bare C# name.
pub(crate) fn is_csharp_bare_type_candidate(n: &Node, reference: &RefView) -> bool {
    reference.language == Language::CSharp
        && n.language == Language::CSharp
        && is_csharp_type_kind(n.kind)
        && is_simple_identifier(&reference.reference_name)
}

/// The `project_metadata` key prefix of a C# file's surface row.
pub const SURFACE_ROW_KEY_PREFIX: &str = "csharp_surface:";

/// The `project_metadata` key of `file_path`'s surface row.
pub fn surface_row_key(file_path: &str) -> String {
    format!("{SURFACE_ROW_KEY_PREFIX}{file_path}")
}

/// A C# type declaration's head as the two hierarchy walks read it: the
/// declaration's name and qualified name, the bases [`csharp_ancestor_names`]
/// reads (#2220) and those [`csharp_supertypes_of`] reads (#2151).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CsharpHead {
    pub name: String,
    pub qualified_name: String,
    pub bases: Vec<String>,
    pub supers: Vec<String>,
}

/// What other files' C# resolution reads of one C# file, by the readers
/// resolution uses: the namespaces its `global using`s import and the types
/// its `global using static`s name (each sorted), and the heads of its type
/// declarations that list a base to either reader (node order).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CsharpSurface {
    pub globals: Vec<String>,
    pub global_statics: Vec<String>,
    pub heads: Vec<CsharpHead>,
}

impl CsharpSurface {
    /// The row of the C# file whose text is `source` and whose nodes are
    /// `nodes`.
    pub fn of(source: &str, nodes: &[Node]) -> Self {
        let facts = crate::source_facts::SourceFacts::new(Arc::from(source));
        let mut ordered: Vec<&Node> = nodes
            .iter()
            .filter(|node| node.language == Language::CSharp && is_csharp_type_kind(node.kind))
            .collect();
        ordered.sort_by(|a, b| {
            (a.start_line, a.start_column, &a.id).cmp(&(b.start_line, b.start_column, &b.id))
        });
        let heads = ordered
            .into_iter()
            .filter_map(|node| {
                let bases = csharp_declared_bases(&facts, node);
                let supers = csharp_declared_supers(&facts, node);
                (!bases.is_empty() || !supers.is_empty()).then(|| CsharpHead {
                    name: node.name.clone(),
                    qualified_name: node.qualified_name.clone(),
                    bases,
                    supers,
                })
            })
            .collect();
        let globals: BTreeSet<String> = if source.contains("global using") {
            global_usings(source).into_iter().collect()
        } else {
            BTreeSet::new()
        };
        let global_statics: BTreeSet<String> = if source.contains("global using static") {
            global_static_usings(source).into_iter().collect()
        } else {
            BTreeSet::new()
        };
        Self {
            globals: globals.into_iter().collect(),
            global_statics: global_statics.into_iter().collect(),
            heads,
        }
    }

    /// No fact: no row records it.
    pub fn is_empty(&self) -> bool {
        self.globals.is_empty() && self.global_statics.is_empty() && self.heads.is_empty()
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("C# surface row encodes")
    }

    pub fn decode(value: &str) -> Option<Self> {
        serde_json::from_str(value).ok()
    }
}

/// The type a C# declaration named `qualified_name` is nested in, by its
/// qualified name: what comes before its last `::` (`None` at the top).
pub fn csharp_owner_name(qualified_name: &str) -> Option<&str> {
    qualified_name.rfind("::").map(|cut| &qualified_name[..cut])
}

/// Whether `candidate` is the C# type a nested declaration's owner name
/// `owner` names (upstream `isCsharpNestedTypeInScope`'s `ownerType`).
pub fn is_csharp_owner_type(candidate: &Node, owner: &str) -> bool {
    is_csharp_type_kind(candidate.kind) && candidate.qualified_name == owner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usings_skip_static_and_read_aliases() {
        let text = "using System;\nusing static System.Math;\nglobal using App.Support;\nusing Task = App.Tests.Schema.Task;\n  using staticky.Lib;\nusing static;\n";
        assert_eq!(
            using_directives(text),
            [
                (None, "System".to_string()),
                (None, "App.Support".to_string()),
                (
                    Some("Task".to_string()),
                    "App.Tests.Schema.Task".to_string()
                ),
                (None, "staticky.Lib".to_string()),
            ]
        );
        assert_eq!(
            global_usings("global using Unit.Support;\nglobal using static X.Y;\n"),
            ["Unit.Support"]
        );
    }

    #[test]
    fn project_files_read_usings_and_implicit_usings() {
        let (usings, implicit) = project_file_usings(
            "<Project>\n<ItemGroup>\n<Using Include=\"App.Shared\" />\n<Using Include=\"System.Math\" Static=\"true\" />\n<using include=\"Lower.Case\"/>\n</ItemGroup>\n<PropertyGroup><ImplicitUsings>enable</ImplicitUsings></PropertyGroup>\n</Project>\n",
        );
        assert_eq!(usings, ["App.Shared", "Lower.Case"]);
        assert!(implicit);
        assert!(!project_file_usings("<ImplicitUsings>disable</ImplicitUsings>").1);
    }

    #[test]
    fn namespaces_are_read_at_line_starts() {
        assert_eq!(
            namespace_declarations(
                "namespace Alpha\n{\n}\n  namespace Beta.Inner;\nvar namespaceX = 1;\n"
            ),
            ["Alpha", "Beta.Inner"]
        );
    }

    #[test]
    fn a_byte_order_mark_is_blank_as_upstream_reads_it() {
        let text = "\u{feff}using System.Threading.Tasks;\n\u{feff}global using App.Support;\n\u{feff}namespace App.Tests\n{\n}\n";
        assert_eq!(
            using_directives(text),
            [
                (None, "System.Threading.Tasks".to_string()),
                (None, "App.Support".to_string()),
            ]
        );
        assert_eq!(global_usings(text), ["App.Support"]);
        assert_eq!(namespace_declarations(text), ["App.Tests"]);
        // U+0085 is Rust's `\s`, not JavaScript's.
        assert!(namespace_declarations("\u{85}namespace Alpha\n").is_empty());
    }

    #[test]
    fn static_usings_read_files_globals_and_project_files() {
        let text = "using static System.Math;\nglobal using static App.Execution.ExpressionBuilder;\nusing System;\n  global using static Lib.Statics ;\n";
        assert_eq!(
            file_static_usings(text),
            [
                "System.Math",
                "App.Execution.ExpressionBuilder",
                "Lib.Statics"
            ]
        );
        assert_eq!(
            global_static_usings(text),
            ["App.Execution.ExpressionBuilder", "Lib.Statics"]
        );
        assert_eq!(
            project_file_static_usings(
                "<ItemGroup>\n<Using Include=\"A.B.Builder\" Static=\"true\"/>\n<Using Include=\"A.Plain\" />\n<using include=\"A.Lower\" static=\"TRUE\" />\n</ItemGroup>\n"
            ),
            ["A.B.Builder", "A.Lower"]
        );
        assert_eq!(last_segment("A.B.Builder"), "Builder");
    }

    fn type_node(name: &str, start_line: i64) -> Node {
        Node {
            id: format!("class:{name}"),
            kind: NodeKind::Class,
            name: name.to_string(),
            qualified_name: format!("App::{name}"),
            file_path: "A.cs".to_string(),
            language: Language::CSharp,
            start_line,
            end_line: start_line + 12,
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
    fn supers_read_nine_lines_with_generics_and_arguments_dropped() {
        let source = "public class Store<T>(int seed) : Base<T>, System.IDisposable where T : class\n{\n}\npublic class Long\n\n\n\n\n\n\n    : Deep.Inner\n{\n}\npublic class Plain\n{\n}\n";
        let facts = crate::source_facts::SourceFacts::new(Arc::from(source));
        assert_eq!(
            csharp_declared_supers(&facts, &type_node("Store", 1)),
            ["Base", "IDisposable"]
        );
        assert_eq!(
            csharp_declared_supers(&facts, &type_node("Long", 4)),
            ["Inner"]
        );
        assert!(csharp_declared_supers(&facts, &type_node("Plain", 14)).is_empty());
        // The #2220 reader stops at seven lines.
        assert!(csharp_declared_bases(&facts, &type_node("Long", 4)).is_empty());
    }

    #[test]
    fn dirname_follows_posix() {
        assert_eq!(dirname("src/Lib/Guard.cs"), "src/Lib");
        assert_eq!(dirname("Program.cs"), ".");
        assert_eq!(dirname("src"), ".");
    }
}
