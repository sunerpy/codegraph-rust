//! Which files' top-level declarations a Dart file can name without a prefix
//! (upstream `dart-libraries.ts`: #2386, with #2388's URIs and #2391's parts).
//!
//! A Dart library is a file and its `part`s. Its top-level scope holds its own
//! declarations, private `_` names included, and every name exported by a
//! library it imports without a prefix: that library's own public declarations
//! and, through its `export`s, other libraries'. Every import and export is
//! narrowed by its `show` / `hide` combinators. A conditional import (`import
//! 'stub.dart' if (dart.library.io) 'io.dart'`) brings in every alternative.
//! `dart:` libraries and packages outside the repository (flutter, test,
//! mockito, …) export nothing of the project.
//!
//! `package:<name>/<path>` is `<root>/lib/<path>` of the package whose
//! `pubspec.yaml` says `name: <name>`: the nearest `pubspec.yaml` above an
//! indexed `.dart` file, never above the project root. When packages share a
//! name (example apps), the importing file's own package wins, then the one
//! enclosing it, then the one its package depends on by `path:`, else all of
//! them count. An `import`, `export` or `part` edge links the one file its URI
//! names ([`dart_directive_file`]).
//!
//! Directives are read from the head of each file at resolution time. Where
//! the library cannot be established — a `part of` naming no library this can
//! find, a file in no package — every file counts as visible, so the rule only
//! narrows where the library is known.
//!
//! KEEP-RUST: only an indexed file's directives are read, and a URI whose path
//! leaves the project (`package:app/../../x.dart` included) names nothing, so a
//! file the index does not hold never moves resolution and no path outside the
//! project is probed.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use crate::pathutil;
use crate::types::{RefView, ResolutionContext};

/// The names an import's or export's `show` / `hide` combinators let through
/// (upstream `NameFilter`); no `show` lets every name through. Equal filters
/// compare equal, so a namespace keeps each once.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NameFilter {
    pub show: Option<BTreeSet<String>>,
    pub hide: Option<BTreeSet<String>>,
}

impl NameFilter {
    /// The filter that lets every name through.
    pub const EVERY: NameFilter = NameFilter {
        show: None,
        hide: None,
    };

    /// Upstream `makeFilter`: an empty `hide` hides nothing.
    fn new(show: Option<BTreeSet<String>>, hide: Option<BTreeSet<String>>) -> Self {
        Self {
            show,
            hide: hide.filter(|hide| !hide.is_empty()),
        }
    }

    fn is_every(&self) -> bool {
        self.show.is_none() && self.hide.is_none()
    }

    /// `self`, then `next` (upstream `composeFilters`): a name gets through
    /// when both let it.
    fn then(&self, next: &NameFilter) -> NameFilter {
        if self.is_every() {
            return next.clone();
        }
        if next.is_every() {
            return self.clone();
        }
        let show = match (&self.show, &next.show) {
            (Some(a), Some(b)) => Some(a.intersection(b).cloned().collect()),
            (a, b) => a.clone().or_else(|| b.clone()),
        };
        let hide = match (&self.hide, &next.hide) {
            (Some(a), Some(b)) => Some(a.union(b).cloned().collect()),
            (a, b) => a.clone().or_else(|| b.clone()),
        };
        NameFilter::new(show, hide)
    }

    /// Whether `name` gets through (upstream `lets`).
    pub fn lets(&self, name: &str) -> bool {
        self.show.as_ref().is_none_or(|show| show.contains(name))
            && self.hide.as_ref().is_none_or(|hide| !hide.contains(name))
    }
}

/// An `import` or `export` directive (upstream `DartNamespaceDirective`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceDirective {
    /// The URI, then each conditional alternative (`if (dart.library.io)
    /// 'io.dart'`).
    pub uris: Vec<String>,
    /// The prefix of `import '…' as p`.
    pub prefix: Option<String>,
    pub filter: NameFilter,
}

/// What a `part of` directive names: a file by URI, or a library by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartOf {
    Uri(String),
    Name(String),
}

/// The directives at the head of a Dart file (upstream `DartDirectives`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DartDirectives {
    /// `library a.b;` gives `a.b`, `library;` the empty name, no directive
    /// `None`.
    pub library_name: Option<String>,
    pub part_of: Option<PartOf>,
    pub parts: Vec<String>,
    pub imports: Vec<NamespaceDirective>,
    pub exports: Vec<NamespaceDirective>,
}

/// The length of the identifier (`[A-Za-z_$][\w$]*`, ASCII as in
/// JavaScript) at byte `i`, if one starts there.
fn identifier_at(src: &str, i: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    let first = *bytes.get(i)?;
    if !(first.is_ascii_alphabetic() || first == b'_' || first == b'$') {
        return None;
    }
    let len = bytes[i..]
        .iter()
        .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_' || **b == b'$')
        .count();
    Some(len)
}

/// The byte length of the character at `i` (1 past the end).
fn char_len_at(src: &str, i: usize) -> usize {
    src.get(i..)
        .and_then(|rest| rest.chars().next())
        .map_or(1, char::len_utf8)
}

/// The index past the whitespace and comments at `i` (upstream
/// `skipTrivia`; Dart block comments nest).
fn skip_trivia(src: &str, mut i: usize) -> usize {
    let bytes = src.as_bytes();
    let n = bytes.len();
    while i < n {
        let c = bytes[i];
        if c == b' ' || (9..=13).contains(&c) {
            i += 1;
        } else if src[i..].starts_with('\u{feff}') {
            i += '\u{feff}'.len_utf8();
        } else if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
            i = src[i + 2..].find('\n').map_or(n, |at| i + 2 + at + 1);
        } else if c == b'/' && bytes.get(i + 1) == Some(&b'*') {
            let mut depth = 1;
            i += 2;
            while i < n && depth > 0 {
                if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    i += 2;
                } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += char_len_at(src, i);
                }
            }
        } else {
            break;
        }
    }
    i.min(n)
}

/// The string literal at `i` — `'…'`, `"…"`, triple-quoted or raw — as its
/// value and the index past it (upstream `readString`).
fn read_string(src: &str, mut i: usize) -> Option<(String, usize)> {
    let bytes = src.as_bytes();
    let mut raw = false;
    if bytes.get(i) == Some(&b'r') && matches!(bytes.get(i + 1), Some(b'\'' | b'"')) {
        raw = true;
        i += 1;
    }
    let quote = *bytes.get(i)?;
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    let triple = bytes.get(i..i + 3) == Some(&[quote; 3][..]);
    let close_len = if triple { 3 } else { 1 };
    let closes_at = |j: usize| {
        bytes
            .get(j..j + close_len)
            .is_some_and(|w| w.iter().all(|b| *b == quote))
    };
    let mut j = i + close_len;
    let mut value = String::new();
    while j < bytes.len() {
        if closes_at(j) {
            return Some((value, j + close_len));
        }
        let c = src[j..].chars().next()?;
        if c == '\n' && !triple {
            return None;
        }
        if c == '\\' && !raw {
            match src[j + 1..].chars().next() {
                Some(next) => {
                    value.push(next);
                    j += 1 + next.len_utf8();
                }
                None => j += 1,
            }
        } else if c == '$' && !raw && bytes.get(j + 1) == Some(&b'{') {
            let mut depth = 1;
            j += 2;
            while j < bytes.len() && depth > 0 {
                if let Some((_, end)) = read_string(src, j) {
                    j = end;
                    continue;
                }
                if bytes[j] == b'{' {
                    depth += 1;
                } else if bytes[j] == b'}' {
                    depth -= 1;
                }
                j += char_len_at(src, j);
            }
        } else {
            value.push(c);
            j += c.len_utf8();
        }
    }
    None
}

/// The index past the balanced `open` … `close` group at `i`, strings and
/// comments skipped (upstream `skipGroup`); `None` when it never closes.
fn skip_group(src: &str, mut i: usize, open: u8, close: u8) -> Option<usize> {
    let bytes = src.as_bytes();
    let mut depth = 0i64;
    while i < bytes.len() {
        i = skip_trivia(src, i);
        if let Some((_, end)) = read_string(src, i) {
            i = end;
            continue;
        }
        let Some(&c) = bytes.get(i) else {
            break;
        };
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some(i + 1);
            }
        }
        i += char_len_at(src, i);
    }
    None
}

/// The index past the metadata annotation at `i` (`@TestOn('vm')`,
/// `@Tags(['slow'])`, `@JS()`; upstream `skipAnnotation`).
fn skip_annotation(src: &str, mut i: usize) -> usize {
    let bytes = src.as_bytes();
    i += 1;
    loop {
        i = skip_trivia(src, i);
        let Some(len) = identifier_at(src, i) else {
            return i;
        };
        i += len;
        let next = skip_trivia(src, i);
        if bytes.get(next) != Some(&b'.') {
            break;
        }
        i = next + 1;
    }
    i = skip_trivia(src, i);
    if bytes.get(i) == Some(&b'<') {
        let Some(end) = skip_group(src, i, b'<', b'>') else {
            return src.len();
        };
        i = skip_trivia(src, end);
    }
    if bytes.get(i) == Some(&b'(') {
        return skip_group(src, i, b'(', b')').unwrap_or(src.len());
    }
    i
}

/// A token of a directive (upstream `DirectiveToken`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Str(String),
    Word(String),
    Condition,
    Punct(char),
}

/// The tokens of a directive, from just past its keyword to its `;`, and the
/// index past that `;` (upstream `readDirectiveTokens`) — or `None` when a
/// body (`{`, `=>`) comes first, as in a function named `part`, or the file
/// ends.
fn read_directive_tokens(src: &str, mut i: usize) -> Option<(Vec<Token>, usize)> {
    let bytes = src.as_bytes();
    let mut tokens = Vec::new();
    loop {
        i = skip_trivia(src, i);
        if i >= bytes.len() || tokens.len() > 2000 {
            return None;
        }
        let c = bytes[i];
        if c == b';' {
            return Some((tokens, i + 1));
        }
        if c == b'{' || c == b'}' || (c == b'=' && bytes.get(i + 1) == Some(&b'>')) {
            return None;
        }
        if let Some((value, end)) = read_string(src, i) {
            tokens.push(Token::Str(value));
            i = end;
            continue;
        }
        if c == b'(' {
            // A configuration condition: `(dart.library.io)`.
            i = skip_group(src, i, b'(', b')')?;
            tokens.push(Token::Condition);
            continue;
        }
        if let Some(len) = identifier_at(src, i) {
            tokens.push(Token::Word(src[i..i + len].to_string()));
            i += len;
            continue;
        }
        let ch = src[i..].chars().next()?;
        tokens.push(Token::Punct(ch));
        i += ch.len_utf8();
    }
}

/// A dotted library name (`app.named`), as the words and dots it is written
/// with (upstream `dottedName`).
fn dotted_name(tokens: &[Token]) -> Option<String> {
    let mut name = String::new();
    for token in tokens {
        match token {
            Token::Word(word) => name.push_str(word),
            Token::Punct('.') => name.push('.'),
            _ => return None,
        }
    }
    Some(name)
}

/// Record the directive `keyword` with `tokens` in `out`; false when the
/// tokens are no such directive (upstream `addDirective`).
fn add_directive(out: &mut DartDirectives, keyword: &str, tokens: &[Token]) -> bool {
    let first = tokens.first();
    if keyword == "library" {
        let Some(name) = dotted_name(tokens) else {
            return false;
        };
        out.library_name = Some(name);
        return true;
    }
    if keyword == "part" {
        if first == Some(&Token::Word("of".to_string())) {
            match tokens.get(1) {
                Some(Token::Str(uri)) => out.part_of = Some(PartOf::Uri(uri.clone())),
                _ => match dotted_name(&tokens[1..]).filter(|name| !name.is_empty()) {
                    Some(name) => out.part_of = Some(PartOf::Name(name)),
                    None => return false,
                },
            }
            return true;
        }
        let Some(Token::Str(uri)) = first else {
            return false;
        };
        out.parts.push(uri.clone());
        return true;
    }
    let Some(Token::Str(uri)) = first else {
        return false;
    };
    let mut uris = vec![uri.clone()];
    let mut prefix = None;
    let mut filter = NameFilter::EVERY;
    let mut k = 1;
    // Adjacent literals are one string: `'package:a/' 'b.dart'`.
    while let Some(Token::Str(more)) = tokens.get(k) {
        uris[0].push_str(more);
        k += 1;
    }
    while k < tokens.len() {
        match (&tokens[k], tokens.get(k + 1), tokens.get(k + 2)) {
            (Token::Word(word), Some(Token::Condition), Some(Token::Str(alternative)))
                if word == "if" =>
            {
                uris.push(alternative.clone());
                k += 3;
            }
            (Token::Word(word), Some(Token::Word(name)), _) if word == "as" => {
                prefix = Some(name.clone());
                k += 2;
            }
            (Token::Word(word), _, _) if word == "show" || word == "hide" => {
                let shows = word == "show";
                let mut names = BTreeSet::new();
                k += 1;
                while let Some(Token::Word(name)) = tokens.get(k) {
                    names.insert(name.clone());
                    if tokens.get(k + 1) != Some(&Token::Punct(',')) {
                        k += 1;
                        break;
                    }
                    k += 2;
                }
                let combinator = if shows {
                    NameFilter::new(Some(names), None)
                } else {
                    NameFilter::new(None, Some(names))
                };
                filter = filter.then(&combinator);
            }
            // `deferred`, and anything this does not model.
            _ => k += 1,
        }
    }
    let directive = NamespaceDirective {
        uris,
        prefix,
        filter,
    };
    if keyword == "import" {
        out.imports.push(directive);
    } else {
        out.exports.push(directive);
    }
    true
}

/// The `library`, `part`, `part of`, `import` and `export` directives at the
/// head of a Dart source file — past its script tag, comments and metadata,
/// up to its first declaration (upstream `readDartDirectives`).
pub fn read_dart_directives(source: &str) -> DartDirectives {
    let mut out = DartDirectives::default();
    let mut i = if source.starts_with('\u{feff}') {
        '\u{feff}'.len_utf8()
    } else {
        0
    };
    if source[i..].starts_with("#!") {
        i = source[i..].find('\n').map_or(source.len(), |at| i + at + 1);
    }
    loop {
        i = skip_trivia(source, i);
        if i >= source.len() {
            break;
        }
        if source.as_bytes()[i] == b'@' {
            i = skip_annotation(source, i);
            continue;
        }
        let Some(len) = identifier_at(source, i) else {
            break;
        };
        let keyword = &source[i..i + len];
        if !matches!(keyword, "library" | "import" | "export" | "part") {
            break;
        }
        let Some((tokens, end)) = read_directive_tokens(source, i + len) else {
            break;
        };
        if !add_directive(&mut out, keyword, &tokens) {
            break;
        }
        i = end;
    }
    out
}

/// A pub package of the project: its pubspec.yaml's `name`, and the
/// directory holding it (`""` for the project root).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PubPackage {
    pub name: String,
    pub root: String,
}

/// An import prefix (`""` for none) and a declaring file.
type PrefixedFile = (String, String);

/// A package's root and the name of one of its dependencies.
type PackageDependency = (String, String);

/// What a file's imports and its library's own files make visible (upstream
/// `FileScope`).
#[derive(Debug)]
struct FileScope {
    /// The library's files: the library, its parts, and the file itself.
    own: BTreeSet<String>,
    /// The files whose imports apply here: the file, and for a part each file
    /// up to its library.
    importers: Vec<String>,
    /// By import prefix (`""` for none), the libraries those imports name,
    /// each with the import's filter (read on first use).
    imported: OnceLock<BTreeMap<String, Vec<(NameFilter, String)>>>,
    /// By prefix and declaring file, the filters through which the imports
    /// bring in its names (read on first use).
    visible: Mutex<HashMap<PrefixedFile, Arc<Vec<NameFilter>>>>,
}

/// The files whose public names a library exports, each with the filters on
/// the way (upstream `exportsOf`'s namespace).
pub type Namespace = BTreeMap<String, Vec<NameFilter>>;

/// How many libraries deep an export chain is followed; a deeper chain
/// exports nothing more (KEEP-RUST: upstream recurses without a bound).
const EXPORT_DEPTH_LIMIT: usize = 64;

/// One step of [`DartLibraries::exports_of`]: the namespace, the shallowest
/// library on the walk's stack it leans on, and whether the depth limit cut
/// it short.
struct ExportWalk {
    namespace: Arc<Namespace>,
    low: usize,
    cut: bool,
}

/// One fixed state of the project's Dart files, read in place of a
/// context's: a sync's before or after (W4-DART-02).
#[derive(Debug, Default)]
struct FixedState {
    files: Vec<String>,
    directives: HashMap<String, Arc<DartDirectives>>,
}

/// What a Dart file's resolution reads of the library model, apart from its
/// library's own files and what the libraries it imports export: its scope
/// (`None` when unknown, every file then visible), by import prefix (`""` for
/// none) the libraries its scope's imports bring in with their filters (an
/// unprefixed import naming no library dropped), and the files each of its
/// own directives names (W4-DART-02).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStructure {
    pub scope: Option<(String, Vec<String>)>,
    pub imported: BTreeMap<String, Vec<(NameFilter, String)>>,
    pub targets: Vec<Vec<String>>,
}

/// The Dart library memos of one resolution pass (upstream `Memo`).
#[derive(Debug, Default)]
pub struct DartLibraries {
    state: Option<FixedState>,
    directives: Mutex<HashMap<String, Option<Arc<DartDirectives>>>>,
    package_of_dir: Mutex<HashMap<String, Option<Arc<PubPackage>>>>,
    packages: OnceLock<BTreeMap<String, Vec<Arc<PubPackage>>>>,
    path_dependencies: Mutex<HashMap<PackageDependency, Arc<Vec<String>>>>,
    library_names: OnceLock<BTreeMap<String, Vec<String>>>,
    library_of: Mutex<HashMap<String, Option<String>>>,
    parent_of: Mutex<HashMap<String, String>>,
    members: Mutex<HashMap<String, Arc<BTreeSet<String>>>>,
    exports: Mutex<HashMap<String, Arc<Namespace>>>,
    scopes: Mutex<HashMap<String, Option<Arc<FileScope>>>>,
}

fn memo_get<K, V>(memo: &Mutex<HashMap<K, V>>, key: &K) -> Option<V>
where
    K: std::hash::Hash + Eq,
    V: Clone,
{
    memo.lock().ok().and_then(|memo| memo.get(key).cloned())
}

fn memo_set<K, V>(memo: &Mutex<HashMap<K, V>>, key: K, value: V)
where
    K: std::hash::Hash + Eq,
{
    if let Ok(mut memo) = memo.lock() {
        memo.insert(key, value);
    }
}

/// `name: <package>` at the start of a pubspec line (upstream
/// `PUBSPEC_NAME`).
fn pubspec_name() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?m)^name[ \t]*:[ \t]*['"]?([A-Za-z_][0-9A-Za-z_]*)"#).expect("pubspec name")
    })
}

/// The `name:` a pubspec text gives its package.
pub fn pubspec_package_name(text: &str) -> Option<String> {
    pubspec_name()
        .captures(text)
        .and_then(|captures| captures.get(1))
        .map(|name| name.as_str().to_string())
}

/// The text of the project file `relative`, read as a source file is: a
/// regular file within the size limit, stat first.
fn read_project_file(context: &dyn ResolutionContext, relative: &str) -> Option<String> {
    crate::context::read_source_file(
        &std::path::Path::new(context.get_project_root()).join(relative),
        relative,
        codegraph_core::config::DEFAULT_MAX_FILE_SIZE,
    )
}

/// `dir/file`, or `file` at the project root.
fn join(dir: &str, file: &str) -> String {
    if dir.is_empty() {
        file.to_string()
    } else {
        format!("{dir}/{file}")
    }
}

/// A project-relative path, normalized, unless it leaves the project.
fn contained(path: &str) -> Option<String> {
    let normalized = pathutil::normalize(path);
    (normalized != ".."
        && !normalized.starts_with("../")
        && !normalized.starts_with('/')
        && normalized != ".")
        .then_some(normalized)
}

/// A URI with a scheme (`dart:`, `http:`), which names no project file.
fn has_scheme(uri: &str) -> bool {
    let bytes = uri.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_alphabetic)
        && bytes[1..]
            .iter()
            .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'.' | b'-'))
            .count()
            .checked_add(1)
            .is_some_and(|len| bytes.get(len) == Some(&b':'))
}

/// The `path:` a pubspec gives its dependency `name`, written as a block
/// (`name:`, then `path: x` below it) or a flow mapping (`name: {path: x}`)
/// (upstream `yamlPathDependencies`). A git dependency's `path:` (inside its
/// `git:` mapping) is a path in that repository, so only `path:` directly
/// under the name counts.
pub fn yaml_path_dependencies(text: &str, name: &str) -> Vec<String> {
    static FLOW_PATH: OnceLock<Regex> = OnceLock::new();
    static BLOCK_PATH: OnceLock<Regex> = OnceLock::new();
    let flow_path = FLOW_PATH.get_or_init(|| {
        Regex::new(r#"[{,][ \t]*path[ \t]*:[ \t]*(?:"([^"]*)"|'([^']*)'|([^,}\s]+))"#)
            .expect("flow path")
    });
    let block_path = BLOCK_PATH.get_or_init(|| {
        Regex::new(r#"^[ \t]*path[ \t]*:[ \t]*(?:"([^"]*)"|'([^']*)'|([^\s#]+))"#)
            .expect("block path")
    });
    let first_group = |captures: regex::Captures<'_>| {
        (1..=3)
            .find_map(|group| captures.get(group))
            .map(|found| found.as_str().to_string())
    };
    let lines: Vec<&str> = text
        .split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some((indent, value)) = yaml_key_of(line, name) else {
            continue;
        };
        let value = strip_yaml_comment(value).trim();
        if !value.is_empty() {
            if value.starts_with('{')
                && value.ends_with('}')
                && !value[1..value.len() - 1].contains(['{', '}'])
                && let Some(path) = flow_path.captures(value).and_then(first_group)
            {
                out.push(path);
            }
            continue;
        }
        let mut child_indent = None;
        for next in &lines[i + 1..] {
            let trimmed = next.trim_start_matches([' ', '\t']);
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let depth = next.len() - trimmed.len();
            if depth <= indent {
                break;
            }
            let child = *child_indent.get_or_insert(depth);
            if depth == child
                && let Some(path) = block_path.captures(next).and_then(first_group)
            {
                out.push(path);
            }
        }
    }
    out
}

/// An indented YAML key line for `name` (`  name:` or `  "name": value`):
/// its indent and the text after the colon (upstream's
/// `/^([ \t]+)(['"]?)([A-Za-z_]\w*)\2[ \t]*:(?:[ \t]+(.*))?$/`).
fn yaml_key_of<'a>(line: &'a str, name: &str) -> Option<(usize, &'a str)> {
    let rest = line.trim_start_matches([' ', '\t']);
    let indent = line.len() - rest.len();
    if indent == 0 {
        return None;
    }
    let (quote, rest) = match rest.as_bytes().first() {
        Some(b'\'') => (Some('\''), &rest[1..]),
        Some(b'"') => (Some('"'), &rest[1..]),
        _ => (None, rest),
    };
    let bytes = rest.as_bytes();
    if !bytes
        .first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
    {
        return None;
    }
    let len = bytes
        .iter()
        .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_')
        .count();
    if &rest[..len] != name {
        return None;
    }
    let mut rest = &rest[len..];
    if let Some(quote) = quote {
        rest = rest.strip_prefix(quote)?;
    }
    let rest = rest.trim_start_matches([' ', '\t']).strip_prefix(':')?;
    if rest.is_empty() {
        return Some((indent, ""));
    }
    let value = rest.trim_start_matches([' ', '\t']);
    (value.len() < rest.len()).then_some((indent, value))
}

/// A YAML value with its trailing comment (` # …`, or `#…` at its start)
/// removed.
fn strip_yaml_comment(value: &str) -> &str {
    let bytes = value.as_bytes();
    for (at, byte) in bytes.iter().enumerate() {
        if *byte == b'#' && (at == 0 || matches!(bytes[at - 1], b' ' | b'\t')) {
            return &value[..at.saturating_sub(usize::from(at > 0))];
        }
    }
    value
}

impl DartLibraries {
    pub fn new() -> Self {
        Self::default()
    }

    /// The model of one fixed state of the project's Dart files (a sync's
    /// before or after): `files` are its indexed Dart files and `directives`
    /// what each of them holds (a file missing from it holds none). The
    /// context still says where the project and its pubspec.yaml files are.
    pub fn of_state(mut files: Vec<String>, directives: HashMap<String, DartDirectives>) -> Self {
        files.sort();
        files.dedup();
        Self {
            state: Some(FixedState {
                files,
                directives: directives
                    .into_iter()
                    .map(|(file, d)| (file, Arc::new(d)))
                    .collect(),
            }),
            ..Self::default()
        }
    }

    /// The project's indexed Dart files.
    fn dart_files(&self, context: &dyn ResolutionContext) -> Vec<String> {
        match &self.state {
            Some(state) => state.files.clone(),
            None => context
                .get_all_files_shared()
                .iter()
                .filter(|file| file.ends_with(".dart"))
                .cloned()
                .collect(),
        }
    }

    /// The directives of the indexed Dart file `file`, read once per pass.
    fn directives_of(
        &self,
        file: &str,
        context: &dyn ResolutionContext,
    ) -> Option<Arc<DartDirectives>> {
        if let Some(state) = &self.state {
            return state
                .files
                .binary_search_by(|f| f.as_str().cmp(file))
                .is_ok()
                .then(|| {
                    state
                        .directives
                        .get(file)
                        .cloned()
                        .unwrap_or_else(|| Arc::new(DartDirectives::default()))
                });
        }
        if let Some(hit) = memo_get(&self.directives, &file.to_string()) {
            return hit;
        }
        let directives = if context.is_indexed_file(file) {
            context
                .read_file_shared(file)
                .map(|source| Arc::new(read_dart_directives(&source)))
        } else {
            None
        };
        memo_set(&self.directives, file.to_string(), directives.clone());
        directives
    }

    /// The package of the nearest pubspec.yaml at or above project-relative
    /// `dir`, never above the project root (upstream `packageOfDir`).
    fn package_of_dir(
        &self,
        dir: &str,
        context: &dyn ResolutionContext,
    ) -> Option<Arc<PubPackage>> {
        let mut walked = Vec::new();
        let mut d = dir.to_string();
        let found = loop {
            if let Some(hit) = memo_get(&self.package_of_dir, &d) {
                break hit;
            }
            walked.push(d.clone());
            if let Some(name) = read_project_file(context, &join(&d, "pubspec.yaml"))
                .as_deref()
                .and_then(pubspec_package_name)
            {
                break Some(Arc::new(PubPackage {
                    name,
                    root: d.clone(),
                }));
            }
            if d.is_empty() {
                break None;
            }
            d = pathutil::dirname(&d);
        };
        for walked in walked {
            memo_set(&self.package_of_dir, walked, found.clone());
        }
        found
    }

    /// The project's packages named `name`: the packages that hold an indexed
    /// `.dart` file (upstream `packagesNamed`).
    fn packages_named(&self, name: &str, context: &dyn ResolutionContext) -> Vec<Arc<PubPackage>> {
        let packages = self.packages.get_or_init(|| {
            let mut by_name: BTreeMap<String, Vec<Arc<PubPackage>>> = BTreeMap::new();
            let mut seen: HashSet<String> = HashSet::new();
            for file in &self.dart_files(context) {
                let Some(package) = self.package_of_dir(&pathutil::dirname(file), context) else {
                    continue;
                };
                if seen.insert(package.root.clone()) {
                    by_name
                        .entry(package.name.clone())
                        .or_default()
                        .push(package);
                }
            }
            for list in by_name.values_mut() {
                list.sort();
            }
            by_name
        });
        packages.get(name).cloned().unwrap_or_default()
    }

    /// The project directories `package`'s pubspec_overrides.yaml and
    /// pubspec.yaml give as the `path:` of its dependency `name` (upstream
    /// `pathDependencies`).
    fn path_dependencies(
        &self,
        package: &PubPackage,
        name: &str,
        context: &dyn ResolutionContext,
    ) -> Arc<Vec<String>> {
        let key = (package.root.clone(), name.to_string());
        if let Some(hit) = memo_get(&self.path_dependencies, &key) {
            return hit;
        }
        let mut dirs: Vec<String> = Vec::new();
        for file in ["pubspec_overrides.yaml", "pubspec.yaml"] {
            let Some(text) = read_project_file(context, &join(&package.root, file)) else {
                continue;
            };
            for written in yaml_path_dependencies(&text, name) {
                let dep = written.replace('\\', "/");
                let bytes = dep.as_bytes();
                if dep.starts_with('/')
                    || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
                {
                    continue;
                }
                let base = if package.root.is_empty() {
                    "."
                } else {
                    package.root.as_str()
                };
                let dir = pathutil::normalize(&format!("{base}/{dep}"));
                if dir == ".." || dir.starts_with("../") {
                    continue;
                }
                let root = if dir == "." { String::new() } else { dir };
                if !dirs.contains(&root) {
                    dirs.push(root);
                }
            }
        }
        let dirs = Arc::new(dirs);
        memo_set(&self.path_dependencies, key, Arc::clone(&dirs));
        dirs
    }

    /// The project files a URI written in `from` names (upstream
    /// `resolveDartUri`): none for `dart:` and other schemes, a package
    /// outside the project, or a path out of it.
    fn resolve_dart_uri(
        &self,
        from: &str,
        uri: &str,
        context: &dyn ResolutionContext,
    ) -> Vec<String> {
        if let Some(rest) = uri.strip_prefix("package:") {
            let Some((name, path)) = rest.split_once('/') else {
                return Vec::new();
            };
            let mut chosen = self.packages_named(name, context);
            if chosen.len() > 1 {
                let own = self.package_of_dir(&pathutil::dirname(from), context);
                let enclosing: Vec<Arc<PubPackage>> = chosen
                    .iter()
                    .filter(|p| p.root.is_empty() || from.starts_with(&format!("{}/", p.root)))
                    .cloned()
                    .collect();
                if let Some(own) = own.as_ref().filter(|own| own.name == name) {
                    chosen = vec![Arc::clone(own)];
                } else if !enclosing.is_empty() {
                    let nearest = enclosing
                        .iter()
                        .fold(None::<&Arc<PubPackage>>, |best, p| match best {
                            Some(best) if best.root.len() >= p.root.len() => Some(best),
                            _ => Some(p),
                        })
                        .cloned();
                    chosen = nearest.into_iter().collect();
                } else if let Some(own) = own {
                    // Example apps that each keep a same-named package (bloc's
                    // two authentication_repository) name theirs as a path
                    // dependency.
                    let dirs = self.path_dependencies(&own, name, context);
                    let declared: Vec<Arc<PubPackage>> = chosen
                        .iter()
                        .filter(|p| dirs.contains(&p.root))
                        .cloned()
                        .collect();
                    if declared.len() == 1 {
                        chosen = declared;
                    }
                }
            }
            return chosen
                .iter()
                .filter_map(|p| contained(&join(&p.root, &format!("lib/{path}"))))
                .collect();
        }
        if has_scheme(uri) {
            return Vec::new();
        }
        let dir = pathutil::dirname(from);
        let base = if dir.is_empty() { "." } else { dir.as_str() };
        contained(&format!("{base}/{uri}")).into_iter().collect()
    }

    /// The files declaring `library <name>;` (upstream `libraryFilesNamed`).
    fn library_files_named(&self, name: &str, context: &dyn ResolutionContext) -> Vec<String> {
        let names = self.library_names.get_or_init(|| {
            let mut by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for file in &self.dart_files(context) {
                if let Some(declared) = self
                    .directives_of(file, context)
                    .and_then(|d| d.library_name.clone())
                    .filter(|declared| !declared.is_empty())
                {
                    by_name.entry(declared).or_default().push(file.clone());
                }
            }
            by_name
        });
        names.get(name).cloned().unwrap_or_default()
    }

    /// The file a part's `part of` names, or `None` when it names none this
    /// can find (upstream `partParent`).
    fn part_parent(
        &self,
        file: &str,
        part_of: &PartOf,
        context: &dyn ResolutionContext,
    ) -> Option<String> {
        let candidates: Vec<String> = match part_of {
            PartOf::Uri(uri) => self
                .resolve_dart_uri(file, uri, context)
                .into_iter()
                .filter(|f| self.directives_of(f, context).is_some())
                .collect(),
            PartOf::Name(name) => self.library_files_named(name, context),
        };
        if candidates.len() <= 1 {
            return candidates.into_iter().next();
        }
        let listing: Vec<String> = candidates
            .into_iter()
            .filter(|candidate| {
                self.directives_of(candidate, context).is_some_and(|d| {
                    d.parts.iter().any(|uri| {
                        self.resolve_dart_uri(candidate, uri, context)
                            .iter()
                            .any(|part| part == file)
                    })
                })
            })
            .collect();
        if listing.len() == 1 {
            listing.into_iter().next()
        } else {
            None
        }
    }

    /// The library file `file` belongs to: itself, or for a part the library
    /// its `part of` chain ends at; `None` when unknown (upstream
    /// `libraryOf`). A part cycle ends at `None` for every file on it.
    fn library_of(&self, file: &str, context: &dyn ResolutionContext) -> Option<String> {
        let mut path: Vec<String> = Vec::new();
        let mut on_path: HashSet<String> = HashSet::new();
        let mut current = file.to_string();
        let library = loop {
            if let Some(hit) = memo_get(&self.library_of, &current) {
                break hit;
            }
            if !on_path.insert(current.clone()) {
                break None;
            }
            path.push(current.clone());
            let Some(directives) = self.directives_of(&current, context) else {
                break None;
            };
            let Some(part_of) = directives.part_of.as_ref() else {
                break Some(current.clone());
            };
            let Some(parent) = self.part_parent(&current, part_of, context) else {
                break None;
            };
            memo_set(&self.parent_of, current.clone(), parent.clone());
            current = parent;
        };
        for walked in path {
            memo_set(&self.library_of, walked, library.clone());
        }
        library
    }

    /// A library's files: the library and its parts, theirs included
    /// (upstream `membersOf`).
    fn members_of(&self, library: &str, context: &dyn ResolutionContext) -> Arc<BTreeSet<String>> {
        if let Some(hit) = memo_get(&self.members, &library.to_string()) {
            return hit;
        }
        let mut files = BTreeSet::from([library.to_string()]);
        let mut queue = VecDeque::from([library.to_string()]);
        while files.len() < 1000 {
            let Some(file) = queue.pop_front() else {
                break;
            };
            let Some(directives) = self.directives_of(&file, context) else {
                continue;
            };
            for uri in &directives.parts {
                for part in self.resolve_dart_uri(&file, uri, context) {
                    if files.insert(part.clone()) {
                        queue.push_back(part);
                    }
                }
            }
        }
        let files = Arc::new(files);
        memo_set(&self.members, library.to_string(), Arc::clone(&files));
        files
    }

    /// The files whose public names `library` exports, each with the filters
    /// on the way: its own files, and what its `export`s reach (upstream
    /// `exportsOf`). `low` is the shallowest library still on `stack` that the
    /// walk came back to; a result that leans on one is incomplete until that
    /// library finishes, so it is kept only when the walk came back to
    /// nothing shallower than this library. A walk cut at the depth limit is
    /// never kept, so a result never depends on where a walk started.
    fn exports_of(
        &self,
        library: &str,
        context: &dyn ResolutionContext,
        stack: &mut Vec<String>,
    ) -> ExportWalk {
        if let Some(hit) = memo_get(&self.exports, &library.to_string()) {
            return ExportWalk {
                namespace: hit,
                low: usize::MAX,
                cut: false,
            };
        }
        let depth = stack.len();
        if depth >= EXPORT_DEPTH_LIMIT {
            return ExportWalk {
                namespace: Arc::new(Namespace::new()),
                low: usize::MAX,
                cut: true,
            };
        }
        stack.push(library.to_string());
        let mut namespace = Namespace::new();
        let mut low = usize::MAX;
        let mut cut = false;
        let members = self.members_of(library, context);
        for file in members.iter() {
            add_filter(&mut namespace, file, NameFilter::EVERY);
        }
        for file in members.iter() {
            let Some(directives) = self.directives_of(file, context) else {
                continue;
            };
            for directive in &directives.exports {
                for uri in &directive.uris {
                    for target in self.resolve_dart_uri(file, uri, context) {
                        let Some(exported) = self.library_of(&target, context) else {
                            continue;
                        };
                        if let Some(on_stack) = stack.iter().position(|l| *l == exported) {
                            low = low.min(on_stack);
                            continue;
                        }
                        let inner = self.exports_of(&exported, context, stack);
                        low = low.min(inner.low);
                        cut |= inner.cut;
                        for (exported_file, filters) in inner.namespace.iter() {
                            for filter in filters {
                                add_filter(
                                    &mut namespace,
                                    exported_file,
                                    directive.filter.then(filter),
                                );
                            }
                        }
                    }
                }
            }
        }
        stack.pop();
        let namespace = Arc::new(namespace);
        if low >= depth && !cut {
            memo_set(&self.exports, library.to_string(), Arc::clone(&namespace));
        }
        ExportWalk {
            namespace,
            low,
            cut,
        }
    }

    /// What `file` can see, or `None` when that is unknown: its library
    /// cannot be found, or no pubspec.yaml above it says which package its
    /// `package:` imports start from (upstream `scopeOf`).
    fn scope_of(&self, file: &str, context: &dyn ResolutionContext) -> Option<Arc<FileScope>> {
        if let Some(hit) = memo_get(&self.scopes, &file.to_string()) {
            return hit;
        }
        let library = if self
            .package_of_dir(&pathutil::dirname(file), context)
            .is_none()
        {
            None
        } else {
            self.library_of(file, context)
        };
        let scope = library.map(|library| {
            let mut own = (*self.members_of(&library, context)).clone();
            own.insert(file.to_string());
            let mut importers = Vec::new();
            let mut current = Some(file.to_string());
            while let Some(f) = current {
                if importers.len() >= 32 {
                    break;
                }
                importers.push(f.clone());
                if f == library {
                    break;
                }
                current = self.parent_of(&f);
            }
            Arc::new(FileScope {
                own,
                importers,
                imported: OnceLock::new(),
                visible: Mutex::new(HashMap::new()),
            })
        });
        memo_set(&self.scopes, file.to_string(), scope.clone());
        scope
    }

    /// The filters through which the imports written with `prefix` (`""` for
    /// none) bring `decl_file`'s names in — empty when they bring in none of
    /// them — or `None` when no import has that prefix (upstream
    /// `importedFilters`).
    fn imported_filters(
        &self,
        scope: &FileScope,
        prefix: &str,
        decl_file: &str,
        context: &dyn ResolutionContext,
    ) -> Option<Arc<Vec<NameFilter>>> {
        let imported = scope.imported.get_or_init(|| {
            let mut imported: BTreeMap<String, Vec<(NameFilter, String)>> = BTreeMap::new();
            for importer in &scope.importers {
                let Some(directives) = self.directives_of(importer, context) else {
                    continue;
                };
                for directive in &directives.imports {
                    let libraries = imported
                        .entry(directive.prefix.clone().unwrap_or_default())
                        .or_default();
                    for uri in &directive.uris {
                        for target in self.resolve_dart_uri(importer, uri, context) {
                            if let Some(library) = self.library_of(&target, context) {
                                libraries.push((directive.filter.clone(), library));
                            }
                        }
                    }
                }
            }
            imported
        });
        let Some(libraries) = imported.get(prefix) else {
            return prefix.is_empty().then(|| Arc::new(Vec::new()));
        };
        let key = (prefix.to_string(), decl_file.to_string());
        if let Some(hit) = memo_get(&scope.visible, &key) {
            return Some(hit);
        }
        let mut through: Vec<NameFilter> = Vec::new();
        for (filter, library) in libraries {
            let walk = self.exports_of(library, context, &mut Vec::new());
            for exported in walk.namespace.get(decl_file).into_iter().flatten() {
                push_filter(&mut through, filter.then(exported));
            }
        }
        let through = Arc::new(through);
        memo_set(&scope.visible, key, Arc::clone(&through));
        Some(through)
    }

    /// Whether `name`, declared at the top level of `decl`, is what `from`
    /// writes as `name` (no `prefix`) or `prefix.name`: upstream
    /// `dartLibrarySees` and `dartPrefixSees`.
    pub fn sees(
        &self,
        from: &str,
        prefix: Option<&str>,
        decl: &str,
        name: &str,
        context: &dyn ResolutionContext,
    ) -> bool {
        if prefix.is_none() && from == decl {
            return true;
        }
        let Some(scope) = self.scope_of(from, context) else {
            return true;
        };
        let Some(prefix) = prefix else {
            if scope.own.contains(decl) {
                return true;
            }
            if name.starts_with('_') {
                return false;
            }
            return self
                .imported_filters(&scope, "", decl, context)
                .is_some_and(|filters| filters.iter().any(|filter| filter.lets(name)));
        };
        match self.imported_filters(&scope, prefix, decl, context) {
            None => true,
            Some(filters) => {
                !name.starts_with('_') && filters.iter().any(|filter| filter.lets(name))
            }
        }
    }

    /// How `from` can name `name` declared at the top level of `decl`:
    /// whether without a prefix, then through each import prefix of its
    /// scope in order (W4-DART-02's sync compares it before and after).
    pub fn reach_of(
        &self,
        from: &str,
        decl: &str,
        name: &str,
        context: &dyn ResolutionContext,
    ) -> Vec<(String, bool)> {
        let mut out = vec![(String::new(), self.sees(from, None, decl, name, context))];
        if let Some(scope) = self.scope_of(from, context) {
            // `imported_filters` reads the scope's imports on first use.
            let _ = self.imported_filters(&scope, "", decl, context);
            let prefixes: Vec<String> = scope
                .imported
                .get()
                .map(|imported| {
                    imported
                        .keys()
                        .filter(|prefix| !prefix.is_empty())
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            for prefix in prefixes {
                let seen = self.sees(from, Some(&prefix), decl, name, context);
                out.push((prefix, seen));
            }
        }
        out
    }

    /// The parent a part's `part of` named, once [`Self::library_of`] read it.
    fn parent_of(&self, file: &str) -> Option<String> {
        memo_get(&self.parent_of, &file.to_string())
    }

    /// What `file`'s resolution reads of the model beyond its library's own
    /// files and the namespaces of the libraries it imports.
    pub fn structure_of(&self, file: &str, context: &dyn ResolutionContext) -> FileStructure {
        let targets = self
            .directives_of(file, context)
            .map(|d| {
                let part_of = match &d.part_of {
                    Some(PartOf::Uri(uri)) => vec![uri.clone()],
                    _ => Vec::new(),
                };
                d.imports
                    .iter()
                    .chain(&d.exports)
                    .flat_map(|directive| directive.uris.clone())
                    .chain(d.parts.iter().cloned())
                    .chain(part_of)
                    .map(|uri| self.resolve_dart_uri(file, &uri, context))
                    .collect()
            })
            .unwrap_or_default();
        let Some(scope) = self.scope_of(file, context) else {
            return FileStructure {
                scope: None,
                imported: BTreeMap::new(),
                targets,
            };
        };
        let library = self.library_of(file, context).unwrap_or_default();
        // The imports `imported_filters` reads, an unprefixed one naming no
        // library dropped: it brings nothing in.
        let mut imported: BTreeMap<String, Vec<(NameFilter, String)>> = BTreeMap::new();
        for importer in &scope.importers {
            let Some(directives) = self.directives_of(importer, context) else {
                continue;
            };
            for directive in &directives.imports {
                let prefix = directive.prefix.clone().unwrap_or_default();
                let libraries: Vec<String> = directive
                    .uris
                    .iter()
                    .flat_map(|uri| self.resolve_dart_uri(importer, uri, context))
                    .filter_map(|target| self.library_of(&target, context))
                    .collect();
                if prefix.is_empty() && libraries.is_empty() {
                    continue;
                }
                let entry = imported.entry(prefix).or_default();
                entry.extend(libraries.into_iter().map(|l| (directive.filter.clone(), l)));
            }
        }
        for libraries in imported.values_mut() {
            libraries.sort();
            libraries.dedup();
        }
        FileStructure {
            scope: Some((library, scope.importers.clone())),
            imported,
            targets,
        }
    }

    /// The files whose public names `library` exports, with the filters on
    /// the way, each file's filters sorted.
    pub fn namespace_of(&self, library: &str, context: &dyn ResolutionContext) -> Namespace {
        let walk = self.exports_of(library, context, &mut Vec::new());
        walk.namespace
            .iter()
            .map(|(file, filters)| {
                let mut filters = filters.clone();
                filters.sort();
                (file.clone(), filters)
            })
            .collect()
    }

    /// The library `file` belongs to, if known.
    pub fn library_of_file(&self, file: &str, context: &dyn ResolutionContext) -> Option<String> {
        self.library_of(file, context)
    }
}

/// Add `filter` to `file`'s filters (upstream `addFilter`): every name
/// absorbs the rest, and equal filters are kept once.
fn add_filter(namespace: &mut Namespace, file: &str, filter: NameFilter) {
    push_filter(namespace.entry(file.to_string()).or_default(), filter);
}

fn push_filter(filters: &mut Vec<NameFilter>, filter: NameFilter) {
    if filters.first().is_some_and(NameFilter::is_every) {
        return;
    }
    if filter.is_every() {
        filters.clear();
        filters.push(filter);
    } else if !filters.contains(&filter) {
        filters.push(filter);
    }
}

/// The project file a Dart `import`, `export` or `part` in `from` names by
/// `uri`, or `None`: a `dart:` library, a package from outside the project,
/// a path out of it, or a package name the project repeats with nothing to
/// say which (upstream `dartDirectiveFile`).
pub fn dart_directive_file(
    from: &str,
    uri: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let mut files = context
        .dart_libraries()
        .resolve_dart_uri(from, uri, context);
    (files.len() == 1).then(|| files.remove(0))
}

/// Whether the library of `from` can name `name`, declared at the top level
/// of `decl`, without a prefix (upstream `dartLibrarySees`): `decl` is one of
/// the library's own files, or a library it imports unprefixed exports the
/// name. True when the library of `from` cannot be established.
pub fn dart_library_sees(
    from: &str,
    decl: &str,
    name: &str,
    context: &dyn ResolutionContext,
) -> bool {
    context
        .dart_libraries()
        .sees(from, None, decl, name, context)
}

/// Whether `prefix.name` in `from` can mean `name` declared at the top level
/// of `decl` (upstream `dartPrefixSees`): a library imported `as prefix`
/// exports it. True when no import of the file's library has that prefix,
/// or the library is unknown.
pub fn dart_prefix_sees(
    from: &str,
    prefix: &str,
    decl: &str,
    name: &str,
    context: &dyn ResolutionContext,
) -> bool {
    context
        .dart_libraries()
        .sees(from, Some(prefix), decl, name, context)
}

/// Whether `decl` is a file of the library `from` belongs to (upstream
/// `inSameDartLibrary`; false when that library is unknown).
pub fn in_same_dart_library(from: &str, decl: &str, context: &dyn ResolutionContext) -> bool {
    if from == decl {
        return true;
    }
    context
        .dart_libraries()
        .scope_of(from, context)
        .is_some_and(|scope| scope.own.contains(decl))
}

/// The `project_metadata` key prefix of the Dart library rows (W4-DART-02).
pub const LIBRARY_ROW_KEY_PREFIX: &str = "dart_library:";

/// The `project_metadata` key of `file_path`'s Dart library row.
pub fn library_row_key(file_path: &str) -> String {
    format!("{LIBRARY_ROW_KEY_PREFIX}{file_path}")
}

/// An `import` or `export` as a library row records it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectiveRow {
    pub uris: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide: Option<Vec<String>>,
}

impl DirectiveRow {
    fn of(directive: &NamespaceDirective) -> Self {
        let names =
            |set: &Option<BTreeSet<String>>| set.as_ref().map(|set| set.iter().cloned().collect());
        Self {
            uris: directive.uris.clone(),
            prefix: directive.prefix.clone(),
            show: names(&directive.filter.show),
            hide: names(&directive.filter.hide),
        }
    }
}

/// What a sync keeps of a Dart file's directives, read by the resolver's own
/// reader ([`read_dart_directives`]) from the file's text (W4-DART-02): the
/// `library` name, the `part of` target, the `part`s, and the imports and
/// exports with their prefixes and combinators, in source order.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryRow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_of_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_of_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<DirectiveRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exports: Vec<DirectiveRow>,
}

impl LibraryRow {
    /// The row of a Dart file's text.
    pub fn of(source: &str) -> Self {
        let directives = read_dart_directives(source);
        let (part_of_uri, part_of_name) = match directives.part_of {
            Some(PartOf::Uri(uri)) => (Some(uri), None),
            Some(PartOf::Name(name)) => (None, Some(name)),
            None => (None, None),
        };
        Self {
            library: directives.library_name,
            part_of_uri,
            part_of_name,
            parts: directives.parts,
            imports: directives.imports.iter().map(DirectiveRow::of).collect(),
            exports: directives.exports.iter().map(DirectiveRow::of).collect(),
        }
    }

    /// The directives this row records, as the reader returned them.
    pub fn directives(&self) -> DartDirectives {
        let directive = |row: &DirectiveRow| NamespaceDirective {
            uris: row.uris.clone(),
            prefix: row.prefix.clone(),
            filter: NameFilter::new(
                row.show
                    .as_ref()
                    .map(|names| names.iter().cloned().collect()),
                row.hide
                    .as_ref()
                    .map(|names| names.iter().cloned().collect()),
            ),
        };
        DartDirectives {
            library_name: self.library.clone(),
            part_of: match (&self.part_of_uri, &self.part_of_name) {
                (Some(uri), _) => Some(PartOf::Uri(uri.clone())),
                (None, Some(name)) => Some(PartOf::Name(name.clone())),
                (None, None) => None,
            },
            parts: self.parts.clone(),
            imports: self.imports.iter().map(directive).collect(),
            exports: self.exports.iter().map(directive).collect(),
        }
    }

    /// Whether the file has no directive at all.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("dart library row encodes")
    }

    pub fn decode(value: &str) -> Option<Self> {
        serde_json::from_str(value).ok()
    }
}

/// What a Dart library declares at its top level (upstream
/// `isDartLibraryDecl`): a function, a type, a typedef, a top-level `final` /
/// `const`. The extractor files mixins and extensions as classes.
pub fn is_dart_library_decl(node: &Node) -> bool {
    node.language == Language::Dart
        && matches!(
            node.kind,
            NodeKind::Function
                | NodeKind::Class
                | NodeKind::Enum
                | NodeKind::TypeAlias
                | NodeKind::Constant
                | NodeKind::Variable
                | NodeKind::Interface
                | NodeKind::Struct
                | NodeKind::Trait
        )
        && !node.qualified_name.contains("::")
}

/// A value a Dart type holds — an enum constant, a `static const` / `final`
/// (upstream `isDartTypeValue`) — which a bare name reaches only from inside
/// the type, as its methods: bloc's brick hook calls `info(…)` on mason's
/// `Logger`, and the call went to bloc_lint's enum constant
/// `LinterRuleState.info`.
pub(crate) fn is_dart_type_value(node: &Node) -> bool {
    node.language == Language::Dart
        && matches!(
            node.kind,
            NodeKind::EnumMember
                | NodeKind::Variable
                | NodeKind::Constant
                | NodeKind::Field
                | NodeKind::Property
        )
        && node.qualified_name.contains("::")
}

/// A Dart type name — a declared type, a supertype, the `Color` of
/// `Color.red` — which is in scope as a call is, or through its import
/// prefix (upstream #2386's `dartTypeRef`).
pub(crate) fn is_dart_type_ref(reference: &RefView) -> bool {
    reference.language == Language::Dart
        && (reference.reference_kind == EdgeKind::References
            || crate::types::is_inheritance_ref(reference.reference_kind))
        && crate::dart_scope::is_dart_identifier(&reference.reference_name)
}

/// The import prefix a Dart type name is written through — the `p` of
/// `p.Report`, `p.Color.red` — or `None` (upstream `dartImportPrefixOf`). A
/// type reference's column is the name's start; on a line where that misses,
/// the nearest whole-word occurrence stands in.
pub(crate) fn dart_import_prefix_of(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let line = crate::dart_scope::source_line(&reference.file_path, reference.line, context)?;
    let name = reference.reference_name.as_str();
    let is_word = |at: usize| {
        line.get(at..).is_some_and(|rest| rest.starts_with(name))
            && !line[..at]
                .chars()
                .next_back()
                .is_some_and(crate::dart_scope::is_dart_name_char)
            && !line[at + name.len()..]
                .chars()
                .next()
                .is_some_and(crate::dart_scope::is_dart_name_char)
    };
    let column = usize::try_from(reference.column).ok();
    let start = column.filter(|at| is_word(*at)).or_else(|| {
        line.match_indices(name)
            .map(|(at, _)| at)
            .find(|at| is_word(*at))
    })?;
    let before = line[..start].trim_end_matches(crate::dart_scope::is_js_space);
    let before = before
        .strip_suffix('.')?
        .trim_end_matches(crate::dart_scope::is_js_space);
    let run = before.len()
        - before
            .trim_end_matches(crate::dart_scope::is_dart_name_char)
            .len();
    let word = &before[before.len() - run..];
    let word = word.trim_start_matches(|c: char| c.is_ascii_digit());
    (!word.is_empty()).then(|| word.to_string())
}

/// Whether a Dart name written without a receiver — a call, a type — can mean
/// `candidate` when it is declared at the top level of a library (upstream
/// `isDartTopLevelVisible`): the caller's own library declares it, or a
/// library the caller imports without a prefix exports it, `show` / `hide`
/// and `export` chains followed. A name written through an import prefix
/// (`p.Report`) means only what the imports with that prefix export.
/// riverpod's generated `async.g.dart` (`part of 'async.dart'`) calls the
/// `family(…)` async.dart declares, and the call went to annotated.dart's
/// `family` beside it.
pub(crate) fn is_dart_top_level_visible(
    candidate: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
    prefix: Option<&str>,
) -> bool {
    if !is_dart_library_decl(candidate) {
        return true;
    }
    match prefix {
        None => dart_library_sees(
            &reference.file_path,
            &candidate.file_path,
            &candidate.name,
            context,
        ),
        Some(prefix) => dart_prefix_sees(
            &reference.file_path,
            prefix,
            &candidate.file_path,
            &candidate.name,
            context,
        ),
    }
}

/// A library's own top-level declaration shadows every import of its name
/// (upstream `preferOwnDartLibrary`), so when the caller's library declares
/// one, the imported namesakes are no candidates. (A function nested in
/// another body is not the library's.)
pub(crate) fn prefer_own_dart_library(
    candidates: Vec<Arc<Node>>,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Vec<Arc<Node>> {
    if candidates
        .iter()
        .filter(|node| is_dart_library_decl(node))
        .count()
        < 2
    {
        return candidates;
    }
    let own: HashSet<String> = candidates
        .iter()
        .filter(|node| {
            is_dart_library_decl(node)
                && in_same_dart_library(&reference.file_path, &node.file_path, context)
                && crate::name_matcher::is_lexically_reachable(node, reference, context)
        })
        .map(|node| node.id.clone())
        .collect();
    if own.is_empty() {
        return candidates;
    }
    candidates
        .into_iter()
        .filter(|node| !is_dart_library_decl(node) || own.contains(&node.id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uris(directives: &[NamespaceDirective]) -> Vec<Vec<String>> {
        directives.iter().map(|d| d.uris.clone()).collect()
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    /// Upstream's `readDartDirectives` cases.
    #[test]
    fn reads_every_directive_up_to_the_first_declaration() {
        let d = read_dart_directives(
            "// Copyright\n@TestOn('vm')\nlibrary app.main;\n\nimport 'dart:async';\nimport 'package:kit/kit.dart' show A, B hide B;\nimport 'stub.dart' if (dart.library.io) 'io.dart' if (dart.library.js_interop == 'true') 'web.dart';\nimport 'lazy.dart' deferred as lazy;\nexport 'src/a.dart'\n    show\n        // one\n        One,\n        Two;\npart 'main.g.dart';\n\nvoid main() {}\nimport 'late.dart';\n",
        );
        assert_eq!(d.library_name.as_deref(), Some("app.main"));
        assert_eq!(d.part_of, None);
        assert_eq!(d.parts, vec!["main.g.dart".to_string()]);
        assert_eq!(
            uris(&d.imports),
            vec![
                vec!["dart:async".to_string()],
                vec!["package:kit/kit.dart".to_string()],
                vec![
                    "stub.dart".to_string(),
                    "io.dart".to_string(),
                    "web.dart".to_string()
                ],
                vec!["lazy.dart".to_string()],
            ]
        );
        assert_eq!(
            d.imports
                .iter()
                .map(|i| i.prefix.clone())
                .collect::<Vec<_>>(),
            vec![None, None, None, Some("lazy".to_string())]
        );
        assert_eq!(d.imports[1].filter.show, Some(set(&["A", "B"])));
        assert_eq!(d.imports[1].filter.hide, Some(set(&["B"])));
        assert_eq!(uris(&d.exports), vec![vec!["src/a.dart".to_string()]]);
        assert_eq!(d.exports[0].filter.show, Some(set(&["One", "Two"])));
    }

    #[test]
    fn reads_part_of_by_uri_and_by_library_name_and_an_unnamed_library() {
        assert_eq!(
            read_dart_directives("part of 'async.dart';\n").part_of,
            Some(PartOf::Uri("async.dart".to_string()))
        );
        assert_eq!(
            read_dart_directives("part of app.named;\n").part_of,
            Some(PartOf::Name("app.named".to_string()))
        );
        let unnamed = read_dart_directives("/// Docs\nlibrary;\n\nimport 'a.dart';\n");
        assert_eq!(unnamed.library_name.as_deref(), Some(""));
        assert_eq!(uris(&unnamed.imports), vec![vec!["a.dart".to_string()]]);
    }

    #[test]
    fn stops_at_a_declaration_that_looks_like_a_directive() {
        let d = read_dart_directives("import 'a.dart';\npart() => 1;\nimport 'b.dart';\n");
        assert_eq!(uris(&d.imports), vec![vec!["a.dart".to_string()]]);
    }

    /// The readers past upstream's cases: a BOM and a script tag, nested
    /// block comments, raw, triple-quoted and adjacent strings, and a
    /// `${…}` in an annotation's argument.
    #[test]
    fn reads_past_the_head_shapes_dart_allows() {
        let d = read_dart_directives(
            "\u{feff}#!/usr/bin/env dart\n/* a /* nested */ comment */\n@Annotation('${\"x\"}')\nimport r'raw.dart';\nimport '''triple.dart''';\nimport 'package:a/' 'b.dart' as ab;\n",
        );
        assert_eq!(
            uris(&d.imports),
            vec![
                vec!["raw.dart".to_string()],
                vec!["triple.dart".to_string()],
                vec!["package:a/b.dart".to_string()],
            ]
        );
        assert_eq!(d.imports[2].prefix.as_deref(), Some("ab"));
    }

    #[test]
    fn filters_compose_as_dart_combinators_do() {
        let show_ab = NameFilter::new(Some(set(&["A", "B"])), None);
        let hide_b = NameFilter::new(None, Some(set(&["B"])));
        let both = show_ab.then(&hide_b);
        assert!(both.lets("A"));
        assert!(!both.lets("B"));
        assert!(!both.lets("C"));
        assert_eq!(NameFilter::EVERY.then(&hide_b), hide_b);
        assert_eq!(
            NameFilter::new(None, Some(BTreeSet::new())),
            NameFilter::EVERY
        );
        let show_b = NameFilter::new(Some(set(&["B", "C"])), None);
        assert_eq!(show_ab.then(&show_b).show, Some(set(&["B"])));
    }

    /// Upstream #2388's pubspec shapes: a block dependency with a comment, a
    /// flow mapping, and a git dependency's own `path:` that is no project
    /// directory.
    #[test]
    fn reads_a_dependencys_path_from_block_and_flow_yaml() {
        let block = "name: login_a\ndependencies:\n  auth_repo:\n    # the app's own copy\n    path: packages/auth_repo\n  bloc: ^9.0.0\n";
        assert_eq!(
            yaml_path_dependencies(block, "auth_repo"),
            vec!["packages/auth_repo".to_string()]
        );
        let flow = "name: login_b\ndependencies:\n  auth_repo: { path: \"packages/auth_repo\" }\n";
        assert_eq!(
            yaml_path_dependencies(flow, "auth_repo"),
            vec!["packages/auth_repo".to_string()]
        );
        let git = "dependencies:\n  auth_repo:\n    git:\n      url: https://example.com/x.git\n      path: auth_repo\n";
        assert_eq!(
            yaml_path_dependencies(git, "auth_repo"),
            Vec::<String>::new()
        );
        let quoted = "dependencies:\n  'auth_repo':\n    path: '../shared/auth_repo' # local\n";
        assert_eq!(
            yaml_path_dependencies(quoted, "auth_repo"),
            vec!["../shared/auth_repo".to_string()]
        );
        assert_eq!(yaml_path_dependencies(block, "bloc"), Vec::<String>::new());
    }

    #[test]
    fn a_scheme_names_no_project_file() {
        assert!(has_scheme("dart:async"));
        assert!(has_scheme("http://example.com/a.dart"));
        assert!(has_scheme("x+y.z-w:path"));
        assert!(!has_scheme("src/a.dart"));
        assert!(!has_scheme("../a.dart"));
        assert!(!has_scheme(":a"));
        assert_eq!(contained("lib/../../x.dart"), None);
        assert_eq!(
            contained("lib/./a/../b.dart").as_deref(),
            Some("lib/b.dart")
        );
    }

    #[test]
    fn a_pubspec_names_its_package() {
        assert_eq!(pubspec_package_name("name: app\n").as_deref(), Some("app"));
        assert_eq!(
            pubspec_package_name("# x\nname:   \"kit_2\"\nversion: 1.0.0\n").as_deref(),
            Some("kit_2")
        );
        assert_eq!(pubspec_package_name("  name: nested\n"), None);
    }
}
