//! Method values on untyped Python module globals (upstream #2291 a, #2332).
//!
//! `thread_pool_exec(settings.conn.fetch, …)` passes a method of a module
//! global that has no static type: `conn = None`, rebound by `global conn;
//! conn = Backend()` in a setup function. Its type is the set of classes its
//! module assigns to it, at module scope or in a function that declares it
//! `global`, joined by what other production modules write to it
//! (`settings.conn = Backend()`). One class resolves to its own method;
//! several bind to the nearest declaration they all inherit, as a base-typed
//! receiver does.
//!
//! A wrong callback edge is worse than none, so the type is unknown when any
//! binding of the global is not a whole constructor call (a factory, a
//! conditional, a tuple target, `for` / `with` / an import, a star import, a
//! namespace-dict write, an annotation the constructor contradicts) or a
//! production write cannot be placed on one module; the caller or a function
//! around it binding the name itself (a parameter, any assignment, a loop, a
//! comprehension, `as`, a `case` pattern, a lambda parameter) is not the
//! global; and a test that installs a double sees no production type.
//! Everything is read from the comment-free code, statement by statement
//! with continuation lines joined; answers are memoised per pass in the
//! files' [`SourceFacts`].

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use crate::import_resolver::resolve_via_import;
use crate::member_value::{python_bases, python_derives_from, python_members, python_ref_class};
use crate::source_facts::{LineIndex, SourceFacts};
use crate::types::{ImportMapping, RefView, ResolutionContext};

/// How one statement binds a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Binding {
    /// `global name`.
    Global,
    /// `name = value`, `name: T = value` or `name: T`.
    Assign {
        annotation: Option<String>,
        value: String,
    },
    /// An import of the name, keyed by where it comes from (`a:x` for `from
    /// a import x`, `a.b:*` for `import a.b as x`).
    Import(String),
    /// Any other binding: a tuple target, a loop, `with … as`, a lambda, …
    Other,
}

/// What one pass over a Python file's code finds for every name.
#[derive(Debug)]
pub(crate) struct PythonFileScan {
    /// Per line: does it start a statement (bracket depth 0, no `\`
    /// continuation)?
    statements: Vec<bool>,
    /// Per line: inside a top-level `if __name__ == "__main__":` block, which
    /// is script code, not module state.
    script: Vec<bool>,
}

/// One file's answers for one name.
#[derive(Debug, Default)]
pub(crate) struct NameScan {
    import_keys: OnceLock<BTreeSet<String>>,
    global_bindings: OnceLock<Vec<Binding>>,
}

/// One module global's answers.
#[derive(Debug, Default)]
pub(crate) struct GlobalMemo {
    own_writes: OnceLock<Option<Vec<GlobalWrite>>>,
    external: OnceLock<ExternalWrites>,
    classes: OnceLock<Option<Vec<Arc<Node>>>>,
}

/// A class a write assigns to a global, as spelled in the file that wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GlobalWrite {
    type_name: String,
    file: String,
}

/// What other files write to a module global.
#[derive(Debug, Default)]
struct ExternalWrites {
    writes: Vec<GlobalWrite>,
    /// A production write that is not a constructor call, or that may land
    /// on another module.
    unknown: bool,
    /// Test files that install a double: a reference in one of them
    /// resolves nothing through the global.
    writers: BTreeSet<String>,
}

/// Python files by the words they spell after `.`, `'` or `"` (upstream
/// #2332 `pythonWriteCandidates`), so that a global's write scan reads only
/// the files that can write it.
#[derive(Debug, Default)]
pub struct PythonWriteIndex {
    files: Vec<String>,
    spelled: HashMap<String, Vec<usize>>,
    /// The files that write through `__dict__`, whose statements may spell
    /// a name anywhere: every word they hold.
    dict: Vec<(usize, Vec<String>)>,
}

impl PythonWriteIndex {
    /// Index every Python file of the project once.
    pub fn build<C: ResolutionContext + ?Sized>(context: &C) -> Self {
        static SPELLED: OnceLock<Regex> = OnceLock::new();
        static WORD: OnceLock<Regex> = OnceLock::new();
        let spelled_pattern =
            SPELLED.get_or_init(|| Regex::new(r#"[.'"]([0-9A-Za-z_]+)"#).expect("spelled word"));
        let word_pattern = WORD.get_or_init(|| Regex::new(r"[0-9A-Za-z_]+").expect("word"));
        let mut files: Vec<String> = context
            .get_all_files_shared()
            .iter()
            .filter(|file| file.ends_with(".py") || file.ends_with(".pyi"))
            .cloned()
            .collect();
        files.sort();
        let mut spelled: HashMap<String, Vec<usize>> = HashMap::new();
        let mut dict = Vec::new();
        for (at, file) in files.iter().enumerate() {
            let source = context
                .read_file_shared(file)
                .unwrap_or_else(|| Arc::from(""));
            let names: BTreeSet<&str> = spelled_pattern
                .captures_iter(&source)
                .filter_map(|captures| captures.get(1))
                .map(|word| word.as_str())
                .collect();
            if names.contains("__dict__") {
                let words: BTreeSet<&str> = word_pattern
                    .find_iter(&source)
                    .map(|word| word.as_str())
                    .collect();
                dict.push((at, words.into_iter().map(str::to_string).collect()));
            }
            for name in names {
                spelled.entry(name.to_string()).or_default().push(at);
            }
        }
        Self {
            files,
            spelled,
            dict,
        }
    }

    /// The files that can write a module global named `name`, in path
    /// order.
    fn candidates(&self, name: &str, context: &dyn ResolutionContext) -> Vec<String> {
        if !is_ascii_word(name) {
            return self
                .files
                .iter()
                .filter(|file| {
                    context
                        .read_file_shared(file)
                        .is_some_and(|text| text.contains(name))
                })
                .cloned()
                .collect();
        }
        let mut hits: BTreeSet<usize> = self
            .spelled
            .get(name)
            .into_iter()
            .flatten()
            .copied()
            .collect();
        // A word-only name is in the text exactly when it is in one of its
        // words.
        for (at, words) in &self.dict {
            if words.iter().any(|word| word.contains(name)) {
                hits.insert(*at);
            }
        }
        hits.into_iter().map(|at| self.files[at].clone()).collect()
    }
}

fn is_ascii_word(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// A file's comment-free code by line (upstream `pythonMemberLines`).
struct Lines<'a> {
    code: &'a str,
    index: &'a LineIndex,
}

impl<'a> Lines<'a> {
    fn of(facts: &'a SourceFacts) -> Self {
        let (code, index) = facts.python_comment_free();
        Self { code, index }
    }

    fn len(&self) -> usize {
        self.index.line_count()
    }

    fn get(&self, index: usize) -> &'a str {
        if index < self.len() {
            self.index.line(self.code, index)
        } else {
            ""
        }
    }
}

fn file_scan(facts: &SourceFacts) -> Arc<PythonFileScan> {
    facts.python_file_scan(|facts| {
        let lines = Lines::of(facts);
        PythonFileScan {
            statements: statement_starts(&lines),
            script: main_block_lines(&lines),
        }
    })
}

/// Per line: does it start a statement? String contents are skipped.
fn statement_starts(lines: &Lines<'_>) -> Vec<bool> {
    let mut starts = Vec::with_capacity(lines.len());
    let mut depth = 0usize;
    let mut continued = false;
    for index in 0..lines.len() {
        let line = lines.get(index).as_bytes();
        starts.push(depth == 0 && !continued);
        let mut quote = 0u8;
        let mut at = 0;
        while at < line.len() {
            let c = line[at];
            if quote != 0 {
                if c == b'\\' {
                    at += 1;
                } else if c == quote {
                    quote = 0;
                }
            } else if c == b'"' || c == b'\'' {
                quote = c;
            } else if matches!(c, b'(' | b'[' | b'{') {
                depth += 1;
            } else if matches!(c, b')' | b']' | b'}') {
                depth = depth.saturating_sub(1);
            }
            at += 1;
        }
        continued = lines.get(index).trim_end().ends_with('\\');
    }
    starts
}

/// Per line: inside a top-level `if __name__ == "__main__":` block (the
/// guard line too when its body follows on it).
fn main_block_lines(lines: &Lines<'_>) -> Vec<bool> {
    static GUARD: OnceLock<Regex> = OnceLock::new();
    let guard = GUARD.get_or_init(|| {
        Regex::new(
            r#"^if\s+(?:__name__\s*==\s*(?:'__main__'|"__main__")|(?:'__main__'|"__main__")\s*==\s*__name__)\s*:"#,
        )
        .expect("main guard")
    });
    let mut inside = vec![false; lines.len()];
    for index in 0..lines.len() {
        let line = lines.get(index);
        let Some(found) = guard.find(line) else {
            continue;
        };
        if !line[found.end()..].trim().is_empty() {
            inside[index] = true;
        }
        let mut next = index + 1;
        while next < lines.len()
            && !lines
                .get(next)
                .chars()
                .next()
                .is_some_and(|c| !c.is_whitespace())
        {
            inside[next] = true;
            next += 1;
        }
    }
    inside
}

/// The statement starting at line `index`, with its continuation lines.
fn statement_at(lines: &Lines<'_>, scan: &PythonFileScan, index: usize) -> String {
    let mut text = lines.get(index).to_string();
    if scan.statements.get(index).copied().unwrap_or(false) {
        let mut next = index + 1;
        while next < lines.len() && !scan.statements.get(next).copied().unwrap_or(true) {
            text.push('\n');
            text.push_str(lines.get(next));
            next += 1;
        }
    }
    text
}

/// Line indexes that belong to `scope` itself (`None`: the module), not to a
/// def or class nested in it.
fn own_lines(
    scope: Option<&Node>,
    file: &str,
    line_count: usize,
    context: &dyn ResolutionContext,
) -> Vec<usize> {
    let from = scope.map_or(1, |scope| scope.start_line.max(1) as usize);
    let to = scope.map_or(line_count, |scope| {
        (scope.end_line.max(0) as usize).min(line_count)
    });
    if to < from {
        return Vec::new();
    }
    let mut nested = vec![false; to - from + 1];
    for node in context.get_nodes_in_file_shared(file) {
        if !matches!(
            node.kind,
            NodeKind::Function | NodeKind::Method | NodeKind::Class
        ) || scope.is_some_and(|scope| scope.id == node.id)
        {
            continue;
        }
        let (start, end) = (
            node.start_line.max(0) as usize,
            node.end_line.max(0) as usize,
        );
        if start < from
            || end > to
            || scope.is_some_and(|scope| node.start_line <= scope.start_line)
        {
            continue;
        }
        for slot in &mut nested[start - from..=end - from] {
            *slot = true;
        }
    }
    (from..=to)
        .filter(|line| !nested[line - from])
        .map(|line| line - 1)
        .collect()
}

/// `a = b = value` split at its top-level assignment operators.
struct Assignment {
    targets: Vec<String>,
    value: String,
    augmented: bool,
}

/// The assignment `line` makes, or `None` when it assigns nothing.
fn assignment(line: &str) -> Option<Assignment> {
    let bytes = line.as_bytes();
    let mut targets = Vec::new();
    let mut depth = 0i64;
    let mut quote = 0u8;
    let mut start = 0usize;
    let mut augmented = false;
    let mut at = 0usize;
    while at < bytes.len() {
        let c = bytes[at];
        if quote != 0 {
            if c == b'\\' {
                at += 1;
            } else if c == quote {
                quote = 0;
            }
            at += 1;
            continue;
        }
        match c {
            b'"' | b'\'' => quote = c,
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'=' if depth == 0 => {
                let prev = at.checked_sub(1).map(|i| bytes[i]);
                if bytes.get(at + 1) == Some(&b'=') {
                    // `==`
                    at += 2;
                    continue;
                }
                // `!=` and the walrus; `<=` and `>=` (but `<<=`, `>>=`
                // assign).
                if matches!(prev, Some(b'!' | b':'))
                    || (matches!(prev, Some(b'<' | b'>'))
                        && at.checked_sub(2).map(|i| bytes[i]) != prev)
                {
                    at += 1;
                    continue;
                }
                let head = &line[start..at];
                let op = ["//", "**", ">>", "<<"]
                    .into_iter()
                    .find(|op| head.ends_with(op))
                    .map(str::len)
                    .or_else(|| {
                        head.ends_with(['-', '+', '*', '/', '%', '&', '|', '^', '@'])
                            .then_some(1)
                    });
                if op.is_some() {
                    augmented = true;
                }
                targets.push(head[..head.len() - op.unwrap_or(0)].to_string());
                start = at + 1;
            }
            _ => {}
        }
        at += 1;
    }
    (!targets.is_empty()).then(|| Assignment {
        targets,
        value: line[start..].to_string(),
        augmented,
    })
}

/// Whether `text` holds `name` as a binding target: not after a word
/// character or `.`, and not followed by `.` or `[` (upstream
/// `(?<![\w.])name\b(?!\s*[.\[])`).
fn has_word(text: &str, name: &str) -> bool {
    let bytes = text.as_bytes();
    text.match_indices(name).any(|(at, _)| {
        let end = at + name.len();
        at.checked_sub(1)
            .is_none_or(|before| !is_word_byte(bytes[before]) && bytes[before] != b'.')
            && bytes.get(end).is_none_or(|&after| !is_word_byte(after))
            && !text[end..].trim_start().starts_with(['.', '['])
    })
}

/// The patterns that read how a statement binds one name.
struct NamePatterns<'n> {
    name: &'n str,
    annotated_target: Regex,
    annotated_line: Regex,
    walrus_or_as: Regex,
}

impl<'n> NamePatterns<'n> {
    fn new(name: &'n str) -> Option<Self> {
        let n = regex::escape(name);
        Some(Self {
            name,
            annotated_target: Regex::new(&format!(r#"^{n}\s*:\s*["']?([0-9A-Za-z_.]+)["']?$"#))
                .ok()?,
            annotated_line: Regex::new(&format!(r#"^\s*{n}\s*:\s*["']?([0-9A-Za-z_.]+)["']?\s*$"#))
                .ok()?,
            walrus_or_as: Regex::new(&format!(r"(?-u:\b){n}[ \t]*:=|(?-u:\b)as[ \t]+{n}(?-u:\b)"))
                .ok()?,
        })
    }
}

/// Every way the statement starting at line `index` binds the name, read
/// across its continuation lines. Only statement lines can assign or
/// import; any line can bind through `as`, a loop, a lambda or the walrus
/// (upstream `pythonLineBindings`).
fn line_bindings(
    lines: &Lines<'_>,
    scan: &PythonFileScan,
    index: usize,
    patterns: &NamePatterns<'_>,
) -> Vec<Binding> {
    static IMPORT: OnceLock<Regex> = OnceLock::new();
    static PART: OnceLock<Regex> = OnceLock::new();
    static DECLARED: OnceLock<Regex> = OnceLock::new();
    static CASE: OnceLock<Regex> = OnceLock::new();
    static DEL: OnceLock<Regex> = OnceLock::new();
    static LOOP: OnceLock<Regex> = OnceLock::new();
    static LAMBDA: OnceLock<Regex> = OnceLock::new();
    let name = patterns.name;
    let statement = scan.statements.get(index).copied().unwrap_or(false);
    let line = statement_at(lines, scan, index);
    let mut out = Vec::new();
    // An import statement can name the name on a continuation line: `from x
    // import (\n    name,\n)`.
    let import = IMPORT.get_or_init(|| {
        Regex::new(r"(?s)^\s*(?:from\s+([0-9A-Za-z_.]+)\s+)?import\s+(.*)$")
            .expect("python import statement")
    });
    if statement && let Some(imported) = import.captures(&line) {
        let names = imported.get(2).map_or("", |names| names.as_str());
        let from = imported.get(1).map(|from| from.as_str());
        // `from x import *` can bind any name.
        if from.is_some() && names.trim() == "*" {
            return vec![Binding::Other];
        }
        if !names.contains(name) {
            return out;
        }
        let part = PART.get_or_init(|| {
            Regex::new(r"^([0-9A-Za-z_.]+)(?:\s+as\s+([0-9A-Za-z_]+))?$").expect("import part")
        });
        let cleaned = names.replace(['(', ')', '\\'], " ");
        for piece in cleaned.split(',') {
            let Some(found) = part.captures(piece.trim()) else {
                continue;
            };
            let module = found.get(1).map_or("", |m| m.as_str());
            let alias = found.get(2).map(|alias| alias.as_str());
            let local = alias.unwrap_or_else(|| {
                if from.is_some() {
                    module
                } else {
                    module.split('.').next().unwrap_or(module)
                }
            });
            if local != name {
                continue;
            }
            out.push(Binding::Import(match from {
                Some(from) => format!("{from}:{module}"),
                None if alias.is_some() => format!("{module}:*"),
                None => format!("{local}:*"),
            }));
        }
        return out;
    }
    if !line.contains(name) || !has_word(&line, name) {
        return out;
    }
    let declared = DECLARED.get_or_init(|| {
        Regex::new(r"^\s*(global|nonlocal)\s+([0-9A-Za-z_\s,]+)$").expect("global declaration")
    });
    if let Some(found) = declared.captures(&line) {
        let names = found.get(2).map_or("", |names| names.as_str());
        if !names.split(',').any(|declared| declared.trim() == name) {
            return Vec::new();
        }
        return vec![if &found[1] == "global" {
            Binding::Global
        } else {
            Binding::Other
        }];
    }
    if statement {
        // A `case` pattern binds its capture names (`case [name]:`, `case
        // Cls(k=name):`, `case name:`).
        if CASE
            .get_or_init(|| Regex::new(r"^\s*case(?-u:\b)").expect("case"))
            .is_match(&line)
        {
            return vec![Binding::Other];
        }
        if let Some(assigned) = assignment(&line) {
            let target = if assigned.targets.len() == 1 && !assigned.augmented {
                assigned.targets[0].trim()
            } else {
                ""
            };
            let annotated = patterns.annotated_target.captures(target);
            if target == name || annotated.is_some() {
                out.push(Binding::Assign {
                    annotation: annotated
                        .and_then(|found| found.get(1))
                        .map(|found| found.as_str().to_string()),
                    value: assigned.value.trim().to_string(),
                });
            } else if assigned.targets.iter().any(|target| has_word(target, name)) {
                out.push(Binding::Other);
            }
        } else if let Some(annotation) = patterns
            .annotated_line
            .captures(&line)
            .and_then(|found| found.get(1))
        {
            out.push(Binding::Assign {
                annotation: Some(annotation.as_str().to_string()),
                value: String::new(),
            });
        } else if DEL
            .get_or_init(|| Regex::new(r"^\s*del(?-u:\b)").expect("del"))
            .is_match(&line)
        {
            out.push(Binding::Other);
        }
    }
    if patterns.walrus_or_as.is_match(&line) {
        out.push(Binding::Other);
    }
    let for_loop = LOOP
        .get_or_init(|| Regex::new(r"(?-u:\b)for\s+([^:]+?)\s+in(?-u:\b)").expect("for target"));
    for found in for_loop.captures_iter(&line) {
        if has_word(&found[1], name) {
            out.push(Binding::Other);
        }
    }
    let lambda =
        LAMBDA.get_or_init(|| Regex::new(r"(?-u:\b)lambda(?-u:\b)([^:]*):").expect("lambda"));
    for found in lambda.captures_iter(&line) {
        if has_word(&found[1], name) {
            out.push(Binding::Other);
        }
    }
    out
}

/// Whether `name`, read at the reference, is bound by the calling function
/// or one around it (a parameter, an assignment, a loop, `as`, a lambda, an
/// import) rather than being the module global. With `imports_bind` false,
/// an import of the name is no shadow: it binds the module the file imports
/// (upstream `pythonBindsLocally`).
pub(crate) fn binds_locally(
    name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
    imports_bind: bool,
) -> bool {
    static DEF: OnceLock<Regex> = OnceLock::new();
    let Some(facts) = context.source_facts(&reference.file_path) else {
        return false;
    };
    let (Some(patterns), Ok(param)) = (
        NamePatterns::new(name),
        Regex::new(&format!(
            r"[(,]\s*\*{{0,2}}{}\s*[:=,)]",
            regex::escape(name)
        )),
    ) else {
        return false;
    };
    let def = DEF.get_or_init(|| Regex::new(r"^\s*(?:async\s+)?def(?-u:\b)").expect("def"));
    let lines = Lines::of(&facts);
    let scan = file_scan(&facts);
    let mut scopes: Vec<Arc<Node>> = context
        .get_nodes_in_file_shared(&reference.file_path)
        .into_iter()
        .filter(|node| {
            matches!(node.kind, NodeKind::Function | NodeKind::Method)
                && node.start_line <= reference.line
                && node.end_line >= reference.line
        })
        .collect();
    scopes.sort_by(|a, b| {
        b.start_line
            .cmp(&a.start_line)
            .then_with(|| a.id.cmp(&b.id))
    });
    for scope in scopes {
        let own = own_lines(
            Some(scope.as_ref()),
            &reference.file_path,
            lines.len(),
            context,
        );
        let bindings: Vec<Binding> = own
            .iter()
            .flat_map(|&index| line_bindings(&lines, &scan, index, &patterns))
            .collect();
        if bindings.contains(&Binding::Global) {
            return false;
        }
        let def_line = own
            .iter()
            .copied()
            .find(|&index| def.is_match(lines.get(index)));
        let mut header = scope.signature.clone().unwrap_or_default();
        if let Some(def_line) = def_line {
            let mut index = def_line;
            while index < lines.len()
                && (index == def_line || !scan.statements.get(index).copied().unwrap_or(true))
            {
                header.push_str(lines.get(index));
                index += 1;
            }
        }
        if param.is_match(&header) {
            return true;
        }
        if bindings.iter().any(|binding| {
            !matches!(binding, Binding::Global)
                && (!matches!(binding, Binding::Import(_)) || imports_bind)
        }) {
            return true;
        }
    }
    false
}

/// The distinct sources `file` imports `name` from, at any scope (`from a
/// import x` is `a:x`).
fn import_keys(name: &str, file: &str, context: &dyn ResolutionContext) -> BTreeSet<String> {
    static IMPORT_WORD: OnceLock<Regex> = OnceLock::new();
    let Some(facts) = context.source_facts(file) else {
        return BTreeSet::new();
    };
    facts
        .python_name_scan(name)
        .import_keys
        .get_or_init(|| {
            let Some(patterns) = NamePatterns::new(name) else {
                return BTreeSet::new();
            };
            let import_word = IMPORT_WORD
                .get_or_init(|| Regex::new(r"(?-u:\b)import(?-u:\b)").expect("import word"));
            let lines = Lines::of(&facts);
            let scan = file_scan(&facts);
            let mut keys = BTreeSet::new();
            for index in 0..lines.len() {
                if !scan.statements[index] || !import_word.is_match(lines.get(index)) {
                    continue;
                }
                for binding in line_bindings(&lines, &scan, index, &patterns) {
                    if let Binding::Import(key) = binding {
                        keys.insert(key);
                    }
                }
            }
            keys
        })
        .clone()
}

/// Every binding of module global `name` in `file`: at module scope, and in
/// each function that declares it `global`, outside a `__main__` block.
fn global_bindings(name: &str, file: &str, context: &dyn ResolutionContext) -> Vec<Binding> {
    static GLOBAL_LINE: OnceLock<Regex> = OnceLock::new();
    let Some(facts) = context.source_facts(file) else {
        return Vec::new();
    };
    facts
        .python_name_scan(name)
        .global_bindings
        .get_or_init(|| {
            let Some(patterns) = NamePatterns::new(name) else {
                return Vec::new();
            };
            let global_line =
                GLOBAL_LINE.get_or_init(|| Regex::new(r"^\s*global(?-u:\b)").expect("global line"));
            let lines = Lines::of(&facts);
            let scan = file_scan(&facts);
            let scopes: Vec<Arc<Node>> = context
                .get_nodes_in_file_shared(file)
                .into_iter()
                .filter(|node| {
                    matches!(
                        node.kind,
                        NodeKind::Class | NodeKind::Function | NodeKind::Method
                    )
                })
                .collect();
            let mut regions = vec![own_lines(None, file, lines.len(), context)];
            for index in 0..lines.len() {
                if !global_line.is_match(lines.get(index))
                    || line_bindings(&lines, &scan, index, &patterns).is_empty()
                {
                    continue;
                }
                let line = index as i64 + 1;
                let scope = scopes
                    .iter()
                    .filter(|scope| scope.start_line <= line && scope.end_line >= line)
                    .max_by(|a, b| {
                        a.start_line
                            .cmp(&b.start_line)
                            .then_with(|| b.id.cmp(&a.id))
                    });
                if let Some(scope) = scope
                    && scope.kind != NodeKind::Class
                {
                    regions.push(own_lines(Some(scope.as_ref()), file, lines.len(), context));
                }
            }
            regions
                .iter()
                .flatten()
                .filter(|&&index| !scan.script.get(index).copied().unwrap_or(false))
                .flat_map(|&index| line_bindings(&lines, &scan, index, &patterns))
                .filter(|binding| *binding != Binding::Global)
                .collect()
        })
        .clone()
}

/// A module-scope Python variable: not a class attribute or a function
/// local.
pub(crate) fn is_module_global(node: &Node, context: &dyn ResolutionContext) -> bool {
    node.language == Language::Python
        && matches!(node.kind, NodeKind::Variable | NodeKind::Constant)
        && !context
            .get_nodes_in_file_shared(&node.file_path)
            .iter()
            .any(|scope| {
                matches!(
                    scope.kind,
                    NodeKind::Class | NodeKind::Function | NodeKind::Method
                ) && scope.start_line <= node.start_line
                    && scope.end_line >= node.start_line
            })
}

/// `Cls(...)` / `pkg.mod.Cls(...)` as the WHOLE (possibly multi-line) value,
/// its callee; `None` for anything else (`Cls() if x else y`, `make()`).
fn constructor_call(text: &str) -> Option<String> {
    static CALLEE: OnceLock<Regex> = OnceLock::new();
    let mut text = text.trim();
    // `(Cls())` is the same value: peel parentheses that wrap the whole
    // expression.
    while text.starts_with('(') {
        let mut depth = 0i64;
        let mut close = None;
        for (at, c) in text.char_indices() {
            if c == '(' {
                depth += 1;
            } else if c == ')' {
                depth -= 1;
                if depth == 0 {
                    close = Some(at);
                    break;
                }
            }
        }
        if close != Some(text.len() - 1) {
            break;
        }
        text = text[1..text.len() - 1].trim();
    }
    let callee = CALLEE.get_or_init(|| {
        Regex::new(r"^((?:[A-Za-z_][0-9A-Za-z_]*\.)*[A-Z][0-9A-Za-z_]*)\s*\(").expect("callee")
    });
    let found = callee.captures(text)?;
    let open = found.get(0)?.end() - 1;
    let bytes = text.as_bytes();
    let mut depth = 0i64;
    let mut quote = 0u8;
    let mut at = open;
    while at < bytes.len() {
        let c = bytes[at];
        if quote != 0 {
            if c == b'\\' {
                at += 1;
            } else if c == quote {
                quote = 0;
            }
        } else if c == b'"' || c == b'\'' {
            quote = c;
        } else if matches!(c, b'(' | b'[' | b'{') {
            depth += 1;
        } else if matches!(c, b')' | b']' | b'}') {
            depth -= 1;
            if depth == 0 {
                let rest = &text[at + 1..];
                return rest
                    .chars()
                    .all(|c| c.is_whitespace() || c == '\\')
                    .then(|| found[1].to_string());
            }
        }
        at += 1;
    }
    None
}

/// Single-line string contents blanked (triple-quoted strings are blanked
/// by the comment stripper already).
fn blank_strings(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        if c == '"' || c == '\'' {
            let mut end = at + 1;
            let mut closed = None;
            while end < chars.len() && chars[end] != '\n' {
                if chars[end] == '\\' {
                    // `\.` takes any character but a newline.
                    if chars.get(end + 1).is_none_or(|&next| next == '\n') {
                        break;
                    }
                    end += 2;
                    continue;
                }
                if chars[end] == c {
                    closed = Some(end);
                    break;
                }
                end += 1;
            }
            if let Some(close) = closed {
                out.push(c);
                out.extend(std::iter::repeat_n(' ', close - at - 1));
                out.push(c);
                at = close + 1;
                continue;
            }
        }
        out.push(c);
        at += 1;
    }
    out
}

/// Test code installs doubles: a test suite's file, or pytest's
/// `conftest.py` wherever it sits.
fn is_test_file(file: &str) -> bool {
    codegraph_core::file_class::is_test_path(file)
        || file == "conftest.py"
        || file.ends_with("/conftest.py")
}

/// The project files a Python module path can name from `from_file`:
/// relative paths (`..settings`) exactly, absolute ones by path suffix, so a
/// source root still resolves, and two files sharing the tail
/// (`x/settings.py`, `y/settings.py`) both come back.
fn module_files(dotted: &str, from_file: &str, context: &dyn ResolutionContext) -> Vec<String> {
    let dots = dotted.bytes().take_while(|&b| b == b'.').count();
    let parts: Vec<&str> = dotted[dots..]
        .split('.')
        .filter(|part| !part.is_empty())
        .collect();
    let Some(last) = parts.last() else {
        return Vec::new();
    };
    let mut dir = String::new();
    if dots > 0 {
        let mut at = from_file.rsplit_once('/').map_or("", |(dir, _)| dir);
        for _ in 1..dots {
            at = at.rsplit_once('/').map_or("", |(dir, _)| dir);
        }
        dir = at.to_string();
    }
    let rel = std::iter::once(dir.as_str())
        .chain(parts.iter().copied())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    let wants = [
        format!("{rel}.py"),
        format!("{rel}.pyi"),
        format!("{rel}/__init__.py"),
    ];
    let matches = |file: &str| {
        wants.iter().any(|want| {
            if dots > 0 {
                file == want
            } else {
                file == want || file.ends_with(&format!("/{want}"))
            }
        })
    };
    let mut out = Vec::new();
    for basename in [
        format!("{last}.py"),
        format!("{last}.pyi"),
        "__init__.py".to_string(),
    ] {
        for node in context.get_nodes_by_name_shared(&basename) {
            if node.kind == NodeKind::File
                && matches(&node.file_path)
                && !out.contains(&node.file_path)
            {
                out.push(node.file_path.clone());
            }
        }
    }
    out
}

/// The project files an import of `file` can name.
fn imported_files(
    mapping: &ImportMapping,
    file: &str,
    context: &dyn ResolutionContext,
) -> Vec<String> {
    let dotted = if mapping.is_namespace {
        mapping.source.clone()
    } else if !mapping.source.is_empty() && mapping.source.bytes().all(|b| b == b'.') {
        format!("{}{}", mapping.source, mapping.exported_name)
    } else {
        format!("{}.{}", mapping.source, mapping.exported_name)
    };
    module_files(&dotted, file, context)
}

/// How `file` spells the module `module_file`: `aliases` import exactly that
/// file, `ambiguous` could also name another file sharing its dotted tail.
/// `import a.b` binds `a`, so that module is spelled `a.b`; `import a.b as
/// c` binds `c`.
fn module_aliases(
    file: &str,
    module_file: &str,
    context: &dyn ResolutionContext,
) -> (Vec<String>, Vec<String>) {
    let mut aliases = BTreeSet::new();
    let mut ambiguous = BTreeSet::new();
    let facts = context.source_facts(file);
    for mapping in context.get_import_mappings(file, Language::Python) {
        let files = imported_files(&mapping, file, context);
        if !files.iter().any(|found| found == module_file) {
            continue;
        }
        // The mapping cannot tell `import a.b` from `import a.b as b`; the
        // source line can, for exactly this module, outside strings.
        let plain_dotted = mapping.is_namespace
            && mapping.source.contains('.')
            && mapping.source.rsplit('.').next() == Some(mapping.local_name.as_str())
            && !facts.as_ref().is_some_and(|facts| {
                let lines = Lines::of(facts);
                (0..lines.len()).any(|index| {
                    imports_explicitly_as(
                        &blank_strings(lines.get(index)),
                        &mapping.source,
                        &mapping.local_name,
                    )
                })
            });
        let spelling = if plain_dotted {
            mapping.source.clone()
        } else {
            mapping.local_name.clone()
        };
        if files.len() == 1 {
            aliases.insert(spelling);
        } else {
            ambiguous.insert(spelling);
        }
    }
    (
        aliases.into_iter().collect(),
        ambiguous.into_iter().collect(),
    )
}

/// Whether `line` imports exactly `source` as `alias` (upstream
/// `\bimport\s[^\n]*(?<![\w.])source\s+as\s+alias\b`).
fn imports_explicitly_as(line: &str, source: &str, alias: &str) -> bool {
    static IMPORT: OnceLock<Regex> = OnceLock::new();
    let import = IMPORT.get_or_init(|| Regex::new(r"(?-u:\b)import\s").expect("import"));
    let Some(found) = import.find(line) else {
        return false;
    };
    let rest = &line[found.end()..];
    let bytes = rest.as_bytes();
    rest.match_indices(source).any(|(at, _)| {
        if at
            .checked_sub(1)
            .is_some_and(|before| is_word_byte(bytes[before]) || bytes[before] == b'.')
        {
            return false;
        }
        let after = &rest[at + source.len()..];
        let spaced = after.trim_start();
        if spaced.len() == after.len() {
            return false;
        }
        let Some(tail) = spaced.strip_prefix("as") else {
            return false;
        };
        let named = tail.trim_start();
        named.len() < tail.len()
            && named
                .strip_prefix(alias)
                .is_some_and(|end| !end.bytes().next().is_some_and(is_word_byte))
    })
}

/// The global's own module's bindings, or `None` when one leaves its type
/// unknown, whatever other modules write.
fn own_writes(global: &Node, context: &dyn ResolutionContext) -> Option<Vec<GlobalWrite>> {
    let facts = context.source_facts(&global.file_path)?;
    facts
        .python_global_memo(&global.id)
        .own_writes
        .get_or_init(|| {
            let file = &global.file_path;
            if dynamic_global_write(&global.name, file, context) {
                return None;
            }
            let mut writes = Vec::new();
            for binding in global_bindings(&global.name, file, context) {
                let Binding::Assign { annotation, value } = binding else {
                    return None;
                };
                let constructor = (!value.is_empty() && value != "None")
                    .then(|| constructor_call(&value))
                    .flatten();
                if let Some(annotation) = annotation {
                    // `conn: Base = make()` trusts the annotation; `conn: A
                    // = B()` contradicts it.
                    if constructor.as_ref().is_some_and(|constructor| {
                        constructor.rsplit('.').next() != annotation.rsplit('.').next()
                    }) {
                        return None;
                    }
                    writes.push(GlobalWrite {
                        type_name: annotation,
                        file: file.clone(),
                    });
                    continue;
                }
                if value == "None" {
                    continue;
                }
                writes.push(GlobalWrite {
                    type_name: constructor?,
                    file: file.clone(),
                });
            }
            Some(writes)
        })
        .clone()
}

/// Whether the global's own module can write it through its namespace dict:
/// `globals()` / `vars()` / `sys.modules[__name__]` used as anything but a
/// literal-key read or `.get`, or a literal-key write of this name.
fn dynamic_global_write(name: &str, file: &str, context: &dyn ResolutionContext) -> bool {
    static MODULE_SCOPE: OnceLock<Regex> = OnceLock::new();
    static FUNCTION_SCOPE: OnceLock<Regex> = OnceLock::new();
    static SAFE: OnceLock<Regex> = OnceLock::new();
    let Some(facts) = context.source_facts(file) else {
        return false;
    };
    let lines = Lines::of(&facts);
    let scan = file_scan(&facts);
    // `vars()` is the module dict only at module scope; inside a function it
    // is the locals.
    let module_lines: HashSet<usize> = own_lines(None, file, lines.len(), context)
        .into_iter()
        .collect();
    let module_scope = MODULE_SCOPE.get_or_init(|| {
        Regex::new(
            r"(?-u:\b)globals\(\s*\)|(?-u:\b)vars\(\s*\)|(?-u:\b)sys\.modules\s*\[\s*__name__\s*\]",
        )
        .expect("module namespace")
    });
    let function_scope = FUNCTION_SCOPE.get_or_init(|| {
        Regex::new(r"(?-u:\b)globals\(\s*\)|(?-u:\b)sys\.modules\s*\[\s*__name__\s*\]")
            .expect("function namespace")
    });
    let safe = SAFE.get_or_init(|| {
        Regex::new(
            r#"(?-u:\b)globals\(\s*\)\s*(?:\[\s*(?:'([0-9A-Za-z_]+)'|"([0-9A-Za-z_]+)")\s*\]|\.\s*get\s*\()"#,
        )
        .expect("namespace read")
    });
    for index in 0..lines.len() {
        if !scan.statements[index] || scan.script.get(index).copied().unwrap_or(false) {
            continue;
        }
        let statement = statement_at(&lines, &scan, index);
        let code = blank_strings(&statement);
        let namespace = if module_lines.contains(&index) {
            module_scope
        } else {
            function_scope
        };
        let uses = namespace.find_iter(&code).count();
        if uses == 0 {
            continue;
        }
        let targets = assignment(&statement)
            .map(|assigned| assigned.targets)
            .unwrap_or_default();
        let mut safe_uses = 0;
        for found in safe.captures_iter(&statement) {
            let whole = found.get(0).map_or("", |whole| whole.as_str());
            let key = found
                .get(1)
                .or_else(|| found.get(2))
                .map(|key| key.as_str());
            let written = key.is_some() && targets.iter().any(|target| target.contains(whole));
            if written && key == Some(name) {
                return true;
            }
            safe_uses += 1;
        }
        if uses > safe_uses {
            return true;
        }
    }
    false
}

/// Writes to module global `global` from OTHER files. A production write
/// `<module>.<name> = Cls(...)` adds a type; any other production write
/// (another value, a tuple target, `setattr`) makes the type unknown. Test
/// files install doubles that do not define the production type; they are
/// recorded as writers, and a reference inside one resolves nothing through
/// the global.
fn external_writes<'m>(
    global: &Node,
    memo: &'m GlobalMemo,
    context: &dyn ResolutionContext,
) -> &'m ExternalWrites {
    memo.external.get_or_init(|| {
        let mut out = ExternalWrites::default();
        let name = &global.name;
        let n = regex::escape(name);
        let index = context.python_write_index();
        for file in index.candidates(name, context) {
            if file == global.file_path {
                continue;
            }
            let test = is_test_file(&file);
            let (aliases, ambiguous) = module_aliases(&file, &global.file_path, context);
            if !test && aliases.is_empty() && ambiguous.is_empty() {
                continue;
            }
            let spell = |names: &[String]| {
                format!(
                    "(?:{})",
                    names
                        .iter()
                        .map(|name| regex::escape(name))
                        .collect::<Vec<_>>()
                        .join("|")
                )
            };
            // Test files: any receiver (the double may sit on any spelling
            // of the module).
            let receiver = if test {
                r"[0-9A-Za-z_.]+".to_string()
            } else {
                spell(&[aliases.as_slice(), ambiguous.as_slice()].concat())
            };
            let exact_receiver = if test {
                receiver.clone()
            } else if aliases.is_empty() {
                // Matches nothing: a write through an ambiguous spelling may
                // land on another module.
                "\\x00".to_string()
            } else {
                spell(&aliases)
            };
            let (Ok(target_pattern), Ok(exact), Ok(dynamic)) = (
                Regex::new(&format!(r"(?:^|[^0-9A-Za-z_.]){receiver}\.{n}(?-u:\b)")),
                Regex::new(&format!(r"^{exact_receiver}\.{n}(?:\s*:[^=]*)?$")),
                Regex::new(&format!(
                    r#"(?-u:\b)(?:setattr|patch\.object)\s*\(\s*{receiver}\s*,\s*['"]{n}['"]|(?:^|[^0-9A-Za-z_.]){receiver}\.__dict__\s*(?:\[|\.\s*update\s*\()"#
                )),
            ) else {
                out.unknown = true;
                return out;
            };
            let Some(facts) = context.source_facts(&file) else {
                continue;
            };
            let lines = Lines::of(&facts);
            let scan = file_scan(&facts);
            for line in 0..lines.len() {
                if !scan.statements[line] || scan.script.get(line).copied().unwrap_or(false) {
                    continue;
                }
                let statement = statement_at(&lines, &scan, line);
                if !statement.contains(name.as_str()) {
                    continue;
                }
                let assigned = assignment(&statement);
                let written = assigned.as_ref().is_some_and(|assigned| {
                    assigned
                        .targets
                        .iter()
                        .any(|target| writes_global(target.trim(), &target_pattern))
                });
                if !written && !dynamic.is_match(&statement) {
                    continue;
                }
                if test {
                    out.writers.insert(file.clone());
                    continue;
                }
                let single = assigned.as_ref().filter(|assigned| {
                    assigned.targets.len() == 1
                        && !assigned.augmented
                        && exact.is_match(assigned.targets[0].trim())
                });
                let value = single.map_or(String::new(), |assigned| {
                    assigned.value.trim().to_string()
                });
                if value == "None" {
                    continue;
                }
                let Some(constructor) = (!value.is_empty())
                    .then(|| constructor_call(&value))
                    .flatten()
                else {
                    out.unknown = true;
                    return out;
                };
                out.writes.push(GlobalWrite {
                    type_name: constructor,
                    file: file.clone(),
                });
            }
        }
        out
    })
}

/// Whether an assignment target writes the global: the pattern matches and
/// what follows the global's name is not `.`, `[` or `(` (a deeper chain, a
/// subscript, a call).
fn writes_global(text: &str, target: &Regex) -> bool {
    target.find_iter(text).any(|found| {
        !text[found.end()..]
            .trim_start()
            .starts_with(['.', '[', '('])
    })
}

/// The classes a module global can hold: what its module assigns and what
/// production writes from other modules join, each resolved in the file that
/// wrote it; `None` when any is unknown.
fn global_classes(
    global: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<Vec<Arc<Node>>> {
    let facts = context.source_facts(&global.file_path)?;
    let memo = facts.python_global_memo(&global.id);
    memo.classes
        .get_or_init(|| {
            let own = own_writes(global, context)?;
            let external = external_writes(global, &memo, context);
            if external.unknown {
                return None;
            }
            let mut classes: Vec<Arc<Node>> = Vec::new();
            for write in external.writes.iter().chain(&own) {
                let mut at_writer = reference.clone();
                at_writer.file_path = write.file.clone();
                let class = python_ref_class(&write.type_name, &at_writer, context)?;
                if !classes.iter().any(|seen| seen.id == class.id) {
                    classes.push(Arc::new(class));
                }
            }
            Some(classes)
        })
        .clone()
}

/// The method a module global's value can dispatch to for `member`: one
/// class's own method; for several, the nearest declaration they all
/// inherit, as a base-typed receiver does; otherwise every candidate, which
/// the caller declines as ambiguous.
pub(crate) fn global_members(
    global: &Node,
    member: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Vec<Arc<Node>> {
    // Unknown from its own module alone: no edge, and no other module to read.
    if own_writes(global, context).is_none() {
        return Vec::new();
    }
    let Some(facts) = context.source_facts(&global.file_path) else {
        return Vec::new();
    };
    let memo = facts.python_global_memo(&global.id);
    // A test that installs its own double sees the double, not the
    // production type.
    if external_writes(global, &memo, context)
        .writers
        .contains(&reference.file_path)
    {
        return Vec::new();
    }
    let Some(classes) = global_classes(global, reference, context) else {
        return Vec::new();
    };
    let mut targets: BTreeMap<String, Arc<Node>> = BTreeMap::new();
    for class in &classes {
        for node in python_members(class, member, reference, context, &mut HashSet::new()) {
            targets.entry(node.id.clone()).or_insert(node);
        }
    }
    let targets: Vec<Arc<Node>> = targets.into_values().collect();
    if targets.len() <= 1 {
        return targets;
    }
    // Several targets: the nearest declaration every candidate class
    // inherits, whether or not a candidate overrides it.
    let owner = |node: &Node| {
        context
            .get_nodes_in_file_shared(&node.file_path)
            .into_iter()
            .find(|class| {
                class.kind == NodeKind::Class
                    && node.qualified_name == format!("{}::{member}", class.qualified_name)
            })
    };
    let mut declarations: Vec<(Arc<Node>, Arc<Node>)> = Vec::new();
    let mut queue: std::collections::VecDeque<Arc<Node>> = targets.iter().cloned().collect();
    while let Some(declaration) = queue.pop_front() {
        if declarations.len() >= 32 {
            break;
        }
        let Some(class) = owner(&declaration) else {
            continue;
        };
        if declarations
            .iter()
            .any(|(seen, _)| seen.id == declaration.id)
        {
            continue;
        }
        declarations.push((Arc::clone(&declaration), Arc::clone(&class)));
        for base in python_bases(&class, reference, context) {
            queue.extend(python_members(
                &base,
                member,
                reference,
                context,
                &mut HashSet::new(),
            ));
        }
    }
    let inherits = |class: &Node, base: &Node| {
        class.id == base.id
            || python_derives_from(class, base, reference, context, &mut HashSet::new())
    };
    let shared: Vec<&(Arc<Node>, Arc<Node>)> = declarations
        .iter()
        .filter(|(_, owner)| classes.iter().all(|class| inherits(class, owner)))
        .collect();
    let nearest: Vec<&&(Arc<Node>, Arc<Node>)> = shared
        .iter()
        .filter(|(_, owner)| shared.iter().all(|(_, other)| inherits(owner, other)))
        .collect();
    match nearest.as_slice() {
        [(declaration, _)] => vec![Arc::clone(declaration)],
        _ => targets,
    }
}

/// The module global a member value's receiver names through an import:
/// `settings.conn` after `import settings`, `conn` after `from settings
/// import conn`, never a deeper chain. A local import of the root binds what
/// the file imports, unless the file imports it from two places or binds
/// the root otherwise.
pub(crate) fn imported_global(
    receiver: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<Arc<Node>> {
    let segments: Vec<&str> = receiver.split('.').collect();
    if segments.len() > 2 {
        return None;
    }
    let mut lookup = reference.clone();
    lookup.reference_name = receiver.to_string();
    lookup.reference_kind = EdgeKind::References;
    let hit = resolve_via_import(&lookup, context)?;
    let global = context.get_node_by_id_shared(&hit.target_node_id)?;
    let root = segments[0];
    (Some(global.name.as_str()) == segments.last().copied()
        && is_module_global(&global, context)
        && import_keys(root, &reference.file_path, context).len() == 1
        && global_bindings(root, &reference.file_path, context)
            .iter()
            .all(|binding| matches!(binding, Binding::Import(_)))
        && !binds_locally(root, reference, context, false))
    .then_some(global)
}

/// The module global of the reference's own file that `receiver` names,
/// unless the calling function or one around it binds the name itself.
pub(crate) fn own_file_global(
    receiver: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<Arc<Node>> {
    let mut globals: Vec<Arc<Node>> = context
        .get_nodes_in_file_shared(&reference.file_path)
        .into_iter()
        .filter(|node| node.name == receiver && is_module_global(node, context))
        .collect();
    globals.sort_by(|a, b| {
        (a.start_line, a.start_column, &a.id).cmp(&(b.start_line, b.start_column, &b.id))
    });
    let global = globals.into_iter().next()?;
    (!binds_locally(receiver, reference, context, true)).then_some(global)
}

/// The names of the module globals of `file` that a member value can type:
/// those whose own module's bindings are all constructor calls, `None` or
/// annotations. Sync re-resolves the member values through them when a
/// module that can write them changes.
pub fn typable_module_globals(file: &str, context: &dyn ResolutionContext) -> BTreeSet<String> {
    context
        .get_nodes_in_file_shared(file)
        .into_iter()
        .filter(|node| is_module_global(node, context) && own_writes(node, context).is_some())
        .map(|node| node.name.clone())
        .collect()
}

/// The names of every module global of `file`.
pub fn module_globals(file: &str, context: &dyn ResolutionContext) -> BTreeSet<String> {
    context
        .get_nodes_in_file_shared(file)
        .into_iter()
        .filter(|node| is_module_global(node, context))
        .map(|node| node.name.clone())
        .collect()
}

/// The project files the Python import statements of `file` name, the
/// modules it can write a global of.
pub fn imported_module_files<'s>(
    file: &str,
    statements: impl IntoIterator<Item = &'s str>,
    context: &dyn ResolutionContext,
) -> BTreeSet<String> {
    statements
        .into_iter()
        .flat_map(|statement| {
            crate::import_resolver::extract_import_mappings(statement, Language::Python)
        })
        .flat_map(|mapping| imported_files(&mapping, file, context))
        .collect()
}

/// Whether a function-value reference's text before its member, `line[..
/// column]`, names one of `globals` as its receiver: `conn.` or
/// `settings.conn.`.
pub fn reads_through_global(before_member: &str, globals: &BTreeSet<String>) -> bool {
    let Some(chain) = before_member.strip_suffix('.') else {
        return false;
    };
    let start = chain
        .bytes()
        .rposition(|b| !(is_word_byte(b) || b == b'.'))
        .map_or(0, |at| at + 1);
    let segments: Vec<&str> = chain[start..].split('.').collect();
    segments.len() <= 2
        && segments.last().is_some_and(|last| globals.contains(*last))
        && segments.iter().all(|segment| is_ascii_word(segment))
}

/// Whether a function-value reference name reads through one of `globals`:
/// `conn.fetch` or `settings.conn.fetch`.
pub fn names_through_global(reference_name: &str, globals: &BTreeSet<String>) -> bool {
    let segments: Vec<&str> = reference_name.split('.').collect();
    matches!(segments.len(), 2 | 3)
        && globals.contains(segments[segments.len() - 2])
        && segments.iter().all(|segment| is_ascii_word(segment))
}

/// The module files a `conftest.py`'s text pulls pytest fixtures in from
/// (`from tests.fixtures.cli import *`, `pytest_plugins = [...]`).
pub fn conftest_plugged_modules(text: &str) -> Vec<String> {
    crate::python_values::plugged_modules_in(text)
}

/// The module files the quoted dotted names in `text` spell, as a
/// `pytest_plugins` value lists them.
pub fn quoted_module_files(text: &str) -> Vec<String> {
    crate::python_values::quoted_module_files(text)
}

/// The parameter names of a Python signature (`(self, request, *args,
/// user: User = None)`).
pub fn parameter_names(signature: &str) -> Vec<String> {
    let inner = signature
        .find('(')
        .map_or(signature, |open| &signature[open + 1..]);
    let mut names = Vec::new();
    let mut depth = 0i64;
    let mut part = String::new();
    let mut flush = |part: &mut String| {
        let name: String = part
            .trim()
            .trim_start_matches('*')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            names.push(name);
        }
        part.clear();
    };
    for c in inner.chars() {
        match c {
            '(' | '[' | '{' => {
                depth += 1;
                part.push(c);
            }
            ')' | ']' | '}' if depth == 0 => break,
            ')' | ']' | '}' => {
                depth -= 1;
                part.push(c);
            }
            ',' if depth == 0 => flush(&mut part),
            _ => part.push(c),
        }
    }
    flush(&mut part);
    names
}

#[cfg(test)]
mod tests {
    use super::{
        assignment, blank_strings, constructor_call, has_word, imports_explicitly_as,
        parameter_names,
    };

    #[test]
    fn parameter_names_read_a_signature() {
        assert_eq!(
            parameter_names("(self, request, *args, user: Dict[str, int] = None, **kwargs)"),
            ["self", "request", "args", "user", "kwargs"]
        );
        assert_eq!(parameter_names("(account)"), ["account"]);
        assert!(parameter_names("()").is_empty());
    }

    #[test]
    fn assignment_splits_targets_and_marks_augmented_operators() {
        let plain = assignment("a = b = Store()").expect("assigns");
        assert_eq!(plain.targets, ["a ", " b "]);
        assert_eq!(plain.value, " Store()");
        assert!(!plain.augmented);
        assert!(assignment("x += 1").expect("assigns").augmented);
        assert!(assignment("x //= 2").expect("assigns").augmented);
        assert!(assignment("x <<= 2").expect("assigns").augmented);
        for none in ["x == y", "x != y", "f(a=1)", "(n := 3)", "x <= y", "x >= y"] {
            assert!(assignment(none).is_none(), "{none}");
        }
        let annotated = assignment("conn: Store = Store()").expect("assigns");
        assert_eq!(annotated.targets, ["conn: Store "]);
    }

    #[test]
    fn has_word_reads_a_binding_target() {
        assert!(has_word("conn, _ = x", "conn"));
        assert!(has_word("(a,\n conn)", "conn"));
        assert!(!has_word("self.conn = x", "conn"));
        assert!(!has_word("conn.pool = x", "conn"));
        assert!(!has_word("conn[0] = x", "conn"));
        assert!(!has_word("connection = x", "conn"));
    }

    #[test]
    fn constructor_call_takes_the_whole_value_only() {
        assert_eq!(constructor_call("Store()").as_deref(), Some("Store"));
        assert_eq!(
            constructor_call("backends.es.ES(\n    x=1,\n)").as_deref(),
            Some("backends.es.ES")
        );
        assert_eq!(constructor_call("(Store())").as_deref(), Some("Store"));
        assert_eq!(constructor_call("Store() if flag else Decoy()"), None);
        assert_eq!(constructor_call("make_store()"), None);
        assert_eq!(constructor_call("Store().fetch"), None);
    }

    #[test]
    fn strings_are_blanked_and_explicit_aliases_read_exactly() {
        assert_eq!(blank_strings(r#"x = "a\"b" + 'c'"#), r#"x = "    " + ' '"#);
        assert!(imports_explicitly_as(
            "import pkg.settings as settings",
            "pkg.settings",
            "settings"
        ));
        assert!(!imports_explicitly_as(
            "import other.pkg.settings as settings",
            "pkg.settings",
            "settings"
        ));
        assert!(!imports_explicitly_as(
            "import pkg.settings",
            "pkg.settings",
            "settings"
        ));
    }
}
