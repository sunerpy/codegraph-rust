//! Dynamic-dispatch boundary detection for `codegraph_explore` (#687).
//!
//! Ports `upstream mcp/dynamic-boundaries.ts` (397 lines) plus the
//! `stripCommentsForRegex` helper it imports
//! (`upstream resolution/strip-comments.ts`). QUERY-TIME ONLY: scans
//! the comment/string-stripped bodies of explored symbols for runtime-dispatch
//! sites and returns a [`BoundaryMatch`] per site. The graph is never mutated.
//!
//! All offsets are BYTE offsets. The upstream works on JS UTF-16 string indices, but
//! every delimiter the strippers and regexes key on (quotes, `\n`, `\\`, the
//! FORMS metacharacters) is ASCII/single-byte, so byte offsets stay aligned and
//! snippets/keys sliced from the original by the same offsets are byte-faithful.

use std::sync::OnceLock;

use regex::Regex;

/// Ports the `BoundaryMatch` interface (dynamic-boundaries.ts:26-46).
#[derive(Debug, Clone)]
pub struct BoundaryMatch {
    /// Stable form id, e.g. `computed-call` — used for per-form dedupe.
    pub form: String,
    /// Human label for the dispatch form, e.g. `computed member call`.
    pub label: String,
    /// One-line source snippet of the site (from the original, untrimmed text).
    pub snippet: String,
    /// 1-based line within the scanned body's FILE (absolute, ready to print).
    pub line: i64,
    /// Statically-visible dispatch key, when one exists.
    pub key: Option<String>,
    /// For typed-bus matches the key is a TYPE name (candidates ~ `${key}Handler`).
    pub key_is_type: bool,
    /// Additional sites of the same form+key in this body beyond the reported one.
    pub more_sites: u32,
}

/// Ports the `CommentLang` union (strip-comments.ts:26-36).
#[derive(Clone, Copy, PartialEq, Eq)]
enum CommentLang {
    Python,
    Javascript,
    Typescript,
    Php,
    Ruby,
    Java,
    CSharp,
    Swift,
    Go,
    Rust,
}

/// Ports `stripCommentsForRegex` (strip-comments.ts:38-59).
fn strip_comments_for_regex(content: &[u8], lang: CommentLang) -> Vec<u8> {
    match lang {
        CommentLang::Python => strip_python(content),
        CommentLang::Ruby => strip_ruby(content),
        CommentLang::Rust => strip_rust(content),
        CommentLang::Php => strip_php(content),
        CommentLang::Go => strip_go(content),
        CommentLang::Javascript
        | CommentLang::Typescript
        | CommentLang::Java
        | CommentLang::CSharp
        | CommentLang::Swift => strip_c_style(
            content,
            // allowSingleQuoteStrings (strip-comments.ts:55)
            matches!(lang, CommentLang::Javascript | CommentLang::Typescript),
        ),
    }
}

/// Ports `blankRange` (strip-comments.ts:65-69): blank every byte in
/// `[start, end)` with a space, but keep newlines for line math.
fn blank_range(buf: &mut [u8], start: usize, end: usize, src: &[u8]) {
    for i in start..end {
        buf[i] = if src[i] == b'\n' { b'\n' } else { b' ' };
    }
}

/// Ports `stripPython` (strip-comments.ts:73-131).
fn strip_python(src: &[u8]) -> Vec<u8> {
    let mut out = src.to_vec();
    let n = src.len();
    let mut i = 0;
    while i < n {
        let c = src[i];
        let c2 = src.get(i + 1).copied().unwrap_or(0);
        let c3 = src.get(i + 2).copied().unwrap_or(0);

        // Triple-quoted string: """...""" or '''...'''
        if (c == b'"' || c == b'\'') && c2 == c && c3 == c {
            let quote = c;
            let start = i;
            i += 3;
            while i < n {
                if src[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src[i] == quote
                    && src.get(i + 1) == Some(&quote)
                    && src.get(i + 2) == Some(&quote)
                {
                    i += 3;
                    break;
                }
                i += 1;
            }
            blank_range(&mut out, start, i, src);
            continue;
        }

        // Single-line string: '...' or "..."
        if c == b'"' || c == b'\'' {
            let quote = c;
            i += 1;
            while i < n && src[i] != quote {
                if src[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            if i < n && src[i] == quote {
                i += 1;
            }
            continue;
        }

        // Line comment
        if c == b'#' {
            let start = i;
            while i < n && src[i] != b'\n' {
                i += 1;
            }
            blank_range(&mut out, start, i, src);
            continue;
        }

        i += 1;
    }
    out
}

/// Ports `stripRuby` (strip-comments.ts:135-209).
fn strip_ruby(src: &[u8]) -> Vec<u8> {
    let mut out = src.to_vec();
    let n = src.len();
    let mut i = 0;
    let mut at_line_start = true;
    while i < n {
        let c = src[i];

        // =begin / =end block comments at start of line.
        if at_line_start && c == b'=' && src[i..].starts_with(b"=begin") {
            let start = i;
            i += b"=begin".len();
            while i < n {
                if src[i] == b'\n' {
                    let mut j = i + 1;
                    while j < n && (src[j] == b' ' || src[j] == b'\t') {
                        j += 1;
                    }
                    if src[j..].starts_with(b"=end") {
                        i = j + b"=end".len();
                        while i < n && src[i] != b'\n' {
                            i += 1;
                        }
                        break;
                    }
                }
                i += 1;
            }
            blank_range(&mut out, start, i, src);
            at_line_start = i > 0 && src[i - 1] == b'\n';
            continue;
        }

        // String literals.
        if c == b'"' || c == b'\'' {
            let quote = c;
            i += 1;
            while i < n && src[i] != quote {
                if src[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            if i < n && src[i] == quote {
                i += 1;
            }
            at_line_start = false;
            continue;
        }

        // Line comment.
        if c == b'#' {
            let start = i;
            while i < n && src[i] != b'\n' {
                i += 1;
            }
            blank_range(&mut out, start, i, src);
            at_line_start = false;
            continue;
        }

        if c == b'\n' {
            at_line_start = true;
            i += 1;
            continue;
        }
        if c == b' ' || c == b'\t' {
            i += 1;
            continue;
        }
        at_line_start = false;
        i += 1;
    }
    out
}

/// Ports `stripCStyle` (strip-comments.ts:213-261).
fn strip_c_style(src: &[u8], allow_single_quote_strings: bool) -> Vec<u8> {
    let mut out = src.to_vec();
    let n = src.len();
    let mut i = 0;
    while i < n {
        let c = src[i];
        let c2 = src.get(i + 1).copied().unwrap_or(0);

        // Block comment.
        if c == b'/' && c2 == b'*' {
            let start = i;
            i += 2;
            while i < n && !(src[i] == b'*' && src.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            if i < n {
                i += 2;
            }
            blank_range(&mut out, start, i, src);
            continue;
        }

        // Line comment.
        if c == b'/' && c2 == b'/' {
            let start = i;
            while i < n && src[i] != b'\n' {
                i += 1;
            }
            blank_range(&mut out, start, i, src);
            continue;
        }

        // String literals.
        if c == b'"' || (allow_single_quote_strings && c == b'\'') || c == b'`' {
            let quote = c;
            i += 1;
            while i < n && src[i] != quote {
                if src[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                // Template literals span lines; regular strings break on newline.
                if quote != b'`' && src[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            if i < n && src[i] == quote {
                i += 1;
            }
            continue;
        }

        i += 1;
    }
    out
}

/// Ports `stripPhp` (strip-comments.ts:265-321).
fn strip_php(src: &[u8]) -> Vec<u8> {
    let mut out = src.to_vec();
    let n = src.len();
    let mut i = 0;
    while i < n {
        let c = src[i];
        let c2 = src.get(i + 1).copied().unwrap_or(0);

        if c == b'/' && c2 == b'*' {
            let start = i;
            i += 2;
            while i < n && !(src[i] == b'*' && src.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            if i < n {
                i += 2;
            }
            blank_range(&mut out, start, i, src);
            continue;
        }

        if c == b'/' && c2 == b'/' {
            let start = i;
            while i < n && src[i] != b'\n' {
                i += 1;
            }
            blank_range(&mut out, start, i, src);
            continue;
        }

        // # line comment (PHP supports both).
        if c == b'#' {
            let start = i;
            while i < n && src[i] != b'\n' {
                i += 1;
            }
            blank_range(&mut out, start, i, src);
            continue;
        }

        if c == b'"' || c == b'\'' || c == b'`' {
            let quote = c;
            i += 1;
            while i < n && src[i] != quote {
                if src[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            if i < n && src[i] == quote {
                i += 1;
            }
            continue;
        }

        i += 1;
    }
    out
}

/// Ports `stripGo` (strip-comments.ts:325-394).
fn strip_go(src: &[u8]) -> Vec<u8> {
    let mut out = src.to_vec();
    let n = src.len();
    let mut i = 0;
    while i < n {
        let c = src[i];
        let c2 = src.get(i + 1).copied().unwrap_or(0);

        if c == b'/' && c2 == b'*' {
            let start = i;
            i += 2;
            while i < n && !(src[i] == b'*' && src.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            if i < n {
                i += 2;
            }
            blank_range(&mut out, start, i, src);
            continue;
        }

        if c == b'/' && c2 == b'/' {
            let start = i;
            while i < n && src[i] != b'\n' {
                i += 1;
            }
            blank_range(&mut out, start, i, src);
            continue;
        }

        // Raw string with backticks (no escapes, can span lines).
        if c == b'`' {
            i += 1;
            while i < n && src[i] != b'`' {
                i += 1;
            }
            if i < n {
                i += 1;
            }
            continue;
        }

        // Interpreted string with double quotes.
        if c == b'"' {
            i += 1;
            while i < n && src[i] != b'"' {
                if src[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            if i < n && src[i] == b'"' {
                i += 1;
            }
            continue;
        }

        // Rune literal with single quotes (handle as a tiny string).
        if c == b'\'' {
            i += 1;
            while i < n && src[i] != b'\'' {
                if src[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            if i < n && src[i] == b'\'' {
                i += 1;
            }
            continue;
        }

        i += 1;
    }
    out
}

/// Ports `stripRust` (strip-comments.ts:398-469).
fn strip_rust(src: &[u8]) -> Vec<u8> {
    let mut out = src.to_vec();
    let n = src.len();
    let mut i = 0;
    while i < n {
        let c = src[i];
        let c2 = src.get(i + 1).copied().unwrap_or(0);

        // Nested block comment.
        if c == b'/' && c2 == b'*' {
            let start = i;
            i += 2;
            let mut depth = 1;
            while i < n && depth > 0 {
                if src[i] == b'/' && src.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    i += 2;
                } else if src[i] == b'*' && src.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            blank_range(&mut out, start, i, src);
            continue;
        }

        if c == b'/' && c2 == b'/' {
            let start = i;
            while i < n && src[i] != b'\n' {
                i += 1;
            }
            blank_range(&mut out, start, i, src);
            continue;
        }

        if c == b'"' {
            i += 1;
            while i < n && src[i] != b'"' {
                if src[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                i += 1;
            }
            if i < n && src[i] == b'"' {
                i += 1;
            }
            continue;
        }

        // Char literal — keep simple: skip 'x' or '\x'.
        if c == b'\'' {
            i += 1;
            while i < n && src[i] != b'\'' {
                if src[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            if i < n && src[i] == b'\'' {
                i += 1;
            }
            continue;
        }

        i += 1;
    }
    out
}

/// Ports the `JS_FAMILY` / `PY` / `RB` / `PHP` / `JVM_CS_GO` / `SWIFT_OBJC`
/// language sets (dynamic-boundaries.ts:68-73). `langs` filters by the raw
/// Node.language string the upstream uses.
fn in_set(set: &[&str], language: &str) -> bool {
    set.contains(&language)
}

const JS_FAMILY: &[&str] = &[
    "typescript",
    "javascript",
    "tsx",
    "jsx",
    "vue",
    "svelte",
    "astro",
];
const PY: &[&str] = &["python"];
const RB: &[&str] = &["ruby"];
const PHP: &[&str] = &["php"];
const JVM_CS_GO: &[&str] = &["java", "kotlin", "scala", "csharp", "go"];
const SWIFT_OBJC: &[&str] = &["swift", "objc", "objcpp", "objective-c"];

/// Ports `singleStringLiteral` (dynamic-boundaries.ts:75-79). The upstream regex uses
/// a `\1` backreference to match the same quote on both ends; the Rust `regex`
/// crate has no backreferences, so we try each quote type explicitly.
fn single_string_literal(text: &str) -> Option<String> {
    static RES: OnceLock<Vec<Regex>> = OnceLock::new();
    let res = RES.get_or_init(|| {
        // `^[^'"`]* QUOTE ([\w.:-]{2,64}) QUOTE [^'"`]*$` for each of ' " `.
        ['\'', '"', '`']
            .iter()
            .map(|q| {
                Regex::new(&format!(
                    r#"^[^'"`]*{q}([\w.:\-]{{2,64}}){q}[^'"`]*$"#,
                    q = regex::escape(&q.to_string())
                ))
                .expect("single-string-literal regex")
            })
            .collect()
    });
    for re in res {
        if let Some(c) = re.captures(text) {
            return Some(c[1].to_string());
        }
    }
    None
}

/// A derived dispatch key plus whether it is a TYPE name (typed-bus).
struct DerivedKey {
    key: String,
    key_is_type: bool,
}

/// Ports a `FormSpec` (dynamic-boundaries.ts:48-66). `key_from` derives the key
/// from the ORIGINAL-source slice; `key_window` extends that slice past the
/// match end (capped at the first newline).
struct FormSpec {
    form: &'static str,
    label: &'static str,
    langs: Option<&'static [&'static str]>,
    re: Regex,
    key_from: Option<fn(&str) -> Option<DerivedKey>>,
    key_window: Option<usize>,
}

fn key_computed_call(orig: &str) -> Option<DerivedKey> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re =
        RE.get_or_init(|| Regex::new(r"\[([^\[\]\n]{1,80})\]\s*\($").expect("computed-call key"));
    let inner = re.captures(orig)?;
    single_string_literal(&inner[1]).map(|key| DerivedKey {
        key,
        key_is_type: false,
    })
}

fn key_ruby_send(orig: &str) -> Option<DerivedKey> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r":(\w+)").expect("ruby-send key"));
    re.captures(orig).map(|m| DerivedKey {
        key: m[1].to_string(),
        key_is_type: false,
    })
}

fn key_single_literal(orig: &str) -> Option<DerivedKey> {
    single_string_literal(orig).map(|key| DerivedKey {
        key,
        key_is_type: false,
    })
}

fn key_typed_bus(orig: &str) -> Option<DerivedKey> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"new\s+([A-Z]\w*)$").expect("typed-bus key"));
    re.captures(orig).map(|m| DerivedKey {
        key: m[1].to_string(),
        key_is_type: true,
    })
}

fn key_selector(orig: &str) -> Option<DerivedKey> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"#selector\s*\(\s*([\w.]+)").expect("selector key"));
    let m = re.captures(orig)?;
    let last = m[1].split('.').next_back().unwrap_or(&m[1]);
    Some(DerivedKey {
        key: last.to_string(),
        key_is_type: false,
    })
}

/// Ports the `FORMS` table (dynamic-boundaries.ts:81-186).
fn forms() -> &'static [FormSpec] {
    static FORMS: OnceLock<Vec<FormSpec>> = OnceLock::new();
    FORMS.get_or_init(|| {
        vec![
            FormSpec {
                form: "computed-call",
                label: "computed member call",
                langs: None,
                re: Regex::new(r"[\w$)\]]\s*\[([^\[\]\n]{1,80})\]\s*\(").expect("computed-call"),
                key_from: Some(key_computed_call),
                key_window: None,
            },
            FormSpec {
                form: "dynamic-import",
                label: "dynamic import",
                langs: Some(JS_FAMILY),
                re: Regex::new(r#"\b(?:import|require)\s*\(\s*(?:[^\s'"`)])"#)
                    .expect("dynamic-import-js"),
                key_from: None,
                key_window: None,
            },
            FormSpec {
                form: "dynamic-import",
                label: "dynamic import",
                langs: Some(PY),
                re: Regex::new(r"\bimportlib\.import_module\s*\(|\b__import__\s*\(")
                    .expect("dynamic-import-py"),
                key_from: None,
                key_window: None,
            },
            FormSpec {
                form: "ruby-send",
                label: "send dispatch",
                langs: Some(RB),
                re: Regex::new(r"\.(?:public_)?send\s*\(\s*:?\w+|\bmethod\s*\(\s*:\w+\s*\)")
                    .expect("ruby-send"),
                key_from: Some(key_ruby_send),
                key_window: None,
            },
            FormSpec {
                form: "php-dynamic",
                label: "dynamic call",
                langs: Some(PHP),
                re: Regex::new(
                    r"\bcall_user_func(?:_array)?\s*\(|\$this\s*->\s*\$\w+\s*\(|\$\w+\s*\(",
                )
                .expect("php-dynamic"),
                key_from: Some(key_single_literal),
                key_window: Some(80),
            },
            FormSpec {
                form: "reflection",
                label: "reflective dispatch",
                langs: Some(JVM_CS_GO),
                re: Regex::new(
                    r"\.invoke\s*\(|\.get(?:Declared)?Method\s*\(|\.GetMethod\s*\(|MethodByName\s*\(|Activator\.CreateInstance|Class\.forName\s*\(",
                )
                .expect("reflection"),
                key_from: Some(key_single_literal),
                key_window: Some(80),
            },
            FormSpec {
                form: "proxy-reflect",
                label: "Proxy/Reflect dispatch",
                langs: Some(JS_FAMILY),
                re: Regex::new(r"\bnew\s+Proxy\s*\(|\bReflect\.(?:get|apply|construct)\s*\(")
                    .expect("proxy-reflect"),
                key_from: None,
                key_window: None,
            },
            FormSpec {
                form: "typed-bus",
                label: "typed message dispatch",
                langs: None,
                re: Regex::new(
                    r"\.(?:[Ss]end|[Pp]ublish|[Dd]ispatch|[Ee]xecute|[Pp]ost|[Ee]mit)(?:Async)?\s*(?:<[^<>\n]{0,80}>)?\s*\(\s*new\s+([A-Z]\w*)",
                )
                .expect("typed-bus"),
                key_from: Some(key_typed_bus),
                key_window: None,
            },
            FormSpec {
                form: "var-key-dispatch",
                label: "string-keyed dispatch (runtime key)",
                langs: None,
                re: Regex::new(
                    r"\.(?:emit|dispatch|trigger|fire|publish|broadcast)\s*\(\s*[A-Za-z_$][\w$]*(?:\.[\w$]+){0,3}\s*[,)]",
                )
                .expect("var-key-dispatch"),
                key_from: None,
                key_window: None,
            },
            FormSpec {
                form: "selector",
                label: "selector dispatch",
                langs: Some(SWIFT_OBJC),
                re: Regex::new(r"#selector\s*\(\s*([\w.]+)|NSClassFromString\s*\(")
                    .expect("selector"),
                key_from: Some(key_selector),
                key_window: None,
            },
        ]
    })
}

/// Ports `commentLang` (dynamic-boundaries.ts:188-219): map a Node.language to
/// the comment-stripper's language set.
fn comment_lang(language: &str) -> Option<CommentLang> {
    match language {
        "python" => Some(CommentLang::Python),
        "gdscript" => Some(CommentLang::Python),
        "ruby" => Some(CommentLang::Ruby),
        "rust" => Some(CommentLang::Rust),
        "php" => Some(CommentLang::Php),
        "go" => Some(CommentLang::Go),
        "javascript" | "jsx" => Some(CommentLang::Javascript),
        "typescript" | "tsx" | "vue" | "svelte" | "astro" => Some(CommentLang::Typescript),
        "java" | "kotlin" | "scala" | "dart" => Some(CommentLang::Java),
        "csharp" => Some(CommentLang::CSharp),
        "swift" => Some(CommentLang::Swift),
        // C-style comments + double-quoted strings — close enough for blanking.
        "c" | "cpp" | "objc" | "objcpp" => Some(CommentLang::Java),
        _ => None,
    }
}

const MAX_MATCHES_PER_BODY: usize = 3;
const MAX_BODY_CHARS: usize = 60_000;

/// Ports `blankStringContents` (dynamic-boundaries.ts:233-259): blank the
/// CONTENTS of string literals (quotes + offsets preserved) so dispatch-shaped
/// prose can't fire a matcher. Run AFTER comment stripping.
fn blank_string_contents(text: &[u8]) -> Vec<u8> {
    let mut out = text.to_vec();
    let n = text.len();
    let mut i = 0;
    while i < n {
        let c = text[i];
        if c == b'"' || c == b'\'' || c == b'`' {
            let quote = c;
            i += 1;
            while i < n && text[i] != quote {
                if text[i] == b'\\' && i + 1 < n {
                    out[i] = b' ';
                    out[i + 1] = b' ';
                    i += 2;
                    continue;
                }
                if quote != b'`' && text[i] == b'\n' {
                    break; // unterminated — stop blanking
                }
                if text[i] != b'\n' {
                    out[i] = b' '; // keep newlines for line math
                }
                i += 1;
            }
            if i < n && text[i] == quote {
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    out
}

/// Ports `countNewlines` (dynamic-boundaries.ts:384-388).
fn count_newlines(text: &[u8], end: usize) -> i64 {
    text[..end].iter().filter(|&&b| b == b'\n').count() as i64
}

/// Ports `snippetAround` (dynamic-boundaries.ts:390-397): the full source line
/// containing `index`, trimmed and capped at 120 chars for display.
fn snippet_around(text: &[u8], index: usize) -> String {
    let line_start = text[..index]
        .iter()
        .rposition(|&b| b == b'\n')
        .map(|p| p + 1)
        .unwrap_or(0);
    let line_end = text[index..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|p| index + p)
        .unwrap_or(text.len());
    let line = String::from_utf8_lossy(&text[line_start..line_end]);
    let line = line.trim();
    // 120/117 are CHAR caps in the upstream; mirror with char-boundary slicing.
    if line.chars().count() > 120 {
        let truncated: String = line.chars().take(117).collect();
        format!("{truncated}...")
    } else {
        line.to_string()
    }
}

const MAX_GETATTR_ARGS: usize = 300;

/// Ports `matchBalancedParen` (dynamic-boundaries.ts:372-382): index of the `)`
/// balancing `text[open]`, or -1 (cap: MAX_GETATTR_ARGS chars).
fn match_balanced_paren(text: &[u8], open: usize) -> i64 {
    let mut depth: i32 = 0;
    let end = text.len().min(open + MAX_GETATTR_ARGS);
    for (i, &c) in text.iter().enumerate().take(end).skip(open) {
        if c == b'(' {
            depth += 1;
        } else if c == b')' {
            depth -= 1;
            if depth == 0 {
                return i as i64;
            }
        }
    }
    -1
}

/// Ports `scanPythonGetattr` (dynamic-boundaries.ts:326-370). getattr dispatch
/// is handled in code, not the FORMS table, because real getattr calls span
/// lines a regex argument class can't bound.
fn scan_python_getattr(
    stripped: &[u8],
    original: &[u8],
    file_start_line: i64,
    out: &mut Vec<BoundaryMatch>,
    seen: &mut Vec<(String, usize)>,
) {
    static GETATTR_RE: OnceLock<Regex> = OnceLock::new();
    static ASSIGN_RE: OnceLock<Regex> = OnceLock::new();
    let getattr_re = GETATTR_RE.get_or_init(|| Regex::new(r"\bgetattr\s*\(").expect("getattr"));
    let assign_re = ASSIGN_RE.get_or_init(|| Regex::new(r"(\w+)\s*=\s*$").expect("getattr-assign"));
    let stripped_str = String::from_utf8_lossy(stripped);
    for m in getattr_re.find_iter(&stripped_str) {
        if out.len() >= MAX_MATCHES_PER_BODY {
            break;
        }
        let m_index = m.start();
        let open = m.end() - 1;
        let close = match_balanced_paren(stripped, open);
        if close == -1 {
            continue;
        }
        let close = close as usize;

        let mut form: Option<&'static str> = None;
        let mut label = "";
        // Immediate call: getattr(...)(
        let after_end = (close + 8).min(stripped.len());
        let after = &stripped[(close + 1).min(stripped.len())..after_end];
        if after_starts_with_open_paren(after) {
            form = Some("getattr-call");
            label = "getattr dispatch";
        } else {
            // Assigned form: look back for `name =` and forward for `name(`.
            let line_start = stripped[..m_index]
                .iter()
                .rposition(|&b| b == b'\n')
                .map(|p| p + 1)
                .unwrap_or(0);
            let before = String::from_utf8_lossy(&stripped[line_start..m_index]);
            if let Some(assign) = assign_re.captures(&before) {
                let name = &assign[1];
                let rest = &stripped[(close + 1).min(stripped.len())..];
                if called_later(rest, name.as_bytes()) {
                    form = Some("getattr-assign");
                    label = "getattr dispatch (assigned, called later)";
                }
            }
        }
        let Some(form) = form else { continue };

        let inner = String::from_utf8_lossy(&original[(open + 1).min(original.len())..close]);
        let key = single_string_literal(&inner);
        let dedupe_key = format!("{form}|{}", key.clone().unwrap_or_default());
        if let Some(pos) = seen.iter().position(|(k, _)| k == &dedupe_key) {
            let idx = seen[pos].1;
            out[idx].more_sites += 1;
            continue;
        }
        let line = file_start_line + count_newlines(original, m_index);
        out.push(BoundaryMatch {
            form: form.to_string(),
            label: label.to_string(),
            snippet: snippet_around(original, m_index),
            line,
            key,
            key_is_type: false,
            more_sites: 0,
        });
        seen.push((dedupe_key, out.len() - 1));
    }
}

/// `/^\s*\(/` test on the bytes right after a balanced getattr close paren.
fn after_starts_with_open_paren(after: &[u8]) -> bool {
    let mut i = 0;
    while i < after.len() && (after[i] == b' ' || after[i] == b'\t') {
        i += 1;
    }
    after.get(i) == Some(&b'(')
}

/// Faithful manual scan for the upstream `new RegExp(\b${name}\s*\()` assigned-call
/// probe (dynamic-boundaries.ts:346), kept regex-free so it never compiles in
/// the getattr loop: find `name` at a `\b` boundary (preceded by a non-word
/// byte or start), followed by optional whitespace then `(`.
fn called_later(hay: &[u8], name: &[u8]) -> bool {
    if name.is_empty() {
        return false;
    }
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = 0;
    while i + name.len() <= hay.len() {
        if &hay[i..i + name.len()] == name {
            let left_boundary = i == 0 || !is_word(hay[i - 1]);
            let right_boundary = hay
                .get(i + name.len())
                .map(|&b| !is_word(b))
                .unwrap_or(true);
            if left_boundary && right_boundary {
                let mut j = i + name.len();
                while j < hay.len() && (hay[j] == b' ' || hay[j] == b'\t' || hay[j] == b'\n') {
                    j += 1;
                }
                if hay.get(j) == Some(&b'(') {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

/// Ports `scanDynamicDispatch` (dynamic-boundaries.ts:269-313). Scan one
/// symbol's body for dynamic-dispatch sites; returns up to MAX_MATCHES_PER_BODY.
///
/// `body` is the symbol's source text; `language` is its Node.language string;
/// `file_start_line` is the 1-based file line where `body` starts (returned
/// lines are absolute file lines).
pub fn scan_dynamic_dispatch(
    body: &str,
    language: &str,
    file_start_line: i64,
) -> Vec<BoundaryMatch> {
    let body_bytes = body.as_bytes();
    let original: &[u8] = if body_bytes.len() > MAX_BODY_CHARS {
        // Slice on a char boundary so the UTF-8 invariant holds; the upstream slices on
        // a UTF-16 unit but the cap only matters for god-functions.
        let mut cut = MAX_BODY_CHARS;
        while cut > 0 && !body.is_char_boundary(cut) {
            cut -= 1;
        }
        &body_bytes[..cut]
    } else {
        body_bytes
    };
    let lang = comment_lang(language);
    let stripped = match lang {
        Some(cl) => blank_string_contents(&strip_comments_for_regex(original, cl)),
        None => blank_string_contents(original),
    };

    let mut out: Vec<BoundaryMatch> = Vec::new();
    // form+key → index into `out` (counts extras). A Vec keeps insertion order
    // and avoids hashing for the tiny per-body candidate set.
    let mut seen: Vec<(String, usize)> = Vec::new();

    if language == "python" {
        scan_python_getattr(&stripped, original, file_start_line, &mut out, &mut seen);
    }

    let stripped_str = String::from_utf8_lossy(&stripped);
    for spec in forms() {
        if out.len() >= MAX_MATCHES_PER_BODY {
            break;
        }
        if let Some(langs) = spec.langs
            && !in_set(langs, language)
        {
            continue;
        }
        for m in spec.re.find_iter(&stripped_str) {
            let m_index = m.start();
            let mut slice_end = m.end();
            if let Some(window) = spec.key_window {
                let window_end = original.len().min(slice_end + window);
                let nl = original[slice_end..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map(|p| slice_end + p);
                slice_end = match nl {
                    Some(nl) if nl < window_end => nl,
                    _ => window_end,
                };
            }
            let orig_slice =
                String::from_utf8_lossy(&original[m_index..slice_end.min(original.len())]);
            let derived = spec.key_from.and_then(|f| f(&orig_slice));
            let dedupe_key = format!(
                "{}|{}",
                spec.form,
                derived.as_ref().map(|d| d.key.as_str()).unwrap_or("")
            );
            if let Some(pos) = seen.iter().position(|(k, _)| k == &dedupe_key) {
                let idx = seen[pos].1;
                out[idx].more_sites += 1;
                continue;
            }
            let line = file_start_line + count_newlines(original, m_index);
            out.push(BoundaryMatch {
                form: spec.form.to_string(),
                label: spec.label.to_string(),
                snippet: snippet_around(original, m_index),
                line,
                key: derived.as_ref().map(|d| d.key.clone()),
                key_is_type: derived.as_ref().map(|d| d.key_is_type).unwrap_or(false),
                more_sites: 0,
            });
            seen.push((dedupe_key, out.len() - 1));
            if out.len() >= MAX_MATCHES_PER_BODY {
                return out;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(body: &str, lang: &str, start: i64) -> BoundaryMatch {
        let mut m = scan_dynamic_dispatch(body, lang, start);
        assert_eq!(m.len(), 1, "expected exactly one match for {body:?}");
        m.remove(0)
    }

    #[test]
    fn computed_call_literal_key() {
        let m = one("return handlers['save'](payload);", "typescript", 10);
        assert_eq!(m.form, "computed-call");
        assert_eq!(m.label, "computed member call");
        assert_eq!(m.key.as_deref(), Some("save"));
        assert_eq!(m.line, 10);
        assert_eq!(m.snippet, "return handlers['save'](payload);");
    }

    #[test]
    fn computed_call_runtime_key_has_no_key() {
        let m = one("return handlers[action.type](payload);", "typescript", 1);
        assert_eq!(m.form, "computed-call");
        assert!(m.key.is_none());
    }

    #[test]
    fn dynamic_import_js_nonliteral() {
        let m = one(
            "async function load(name){ return await import(name); }",
            "javascript",
            1,
        );
        assert_eq!(m.form, "dynamic-import");
        assert_eq!(m.label, "dynamic import");
    }

    #[test]
    fn dynamic_import_python_importlib() {
        let m = one(
            "def load(name):\n    return importlib.import_module(name)",
            "python",
            5,
        );
        assert_eq!(m.form, "dynamic-import");
        assert_eq!(m.line, 6);
    }

    #[test]
    fn ruby_send_symbol_key() {
        let m = one("def run(m)\n  obj.send(:foo)\nend", "ruby", 1);
        assert_eq!(m.form, "ruby-send");
        assert_eq!(m.label, "send dispatch");
        assert_eq!(m.key.as_deref(), Some("foo"));
        assert_eq!(m.line, 2);
    }

    #[test]
    fn php_dynamic_call() {
        let m = one("<?php\nfunction r($c){ return $callback($c); }", "php", 1);
        assert_eq!(m.form, "php-dynamic");
        assert_eq!(m.label, "dynamic call");
    }

    #[test]
    fn reflection_java_invoke() {
        let m = one("Object r = method.invoke(target, args);", "java", 42);
        assert_eq!(m.form, "reflection");
        assert_eq!(m.label, "reflective dispatch");
        assert_eq!(m.line, 42);
    }

    #[test]
    fn typed_bus_type_key() {
        let m = one("mediator.Send(new CreateTodoCommand(x));", "csharp", 3);
        assert_eq!(m.form, "typed-bus");
        assert_eq!(m.label, "typed message dispatch");
        assert_eq!(m.key.as_deref(), Some("CreateTodoCommand"));
        assert!(m.key_is_type);
    }

    #[test]
    fn proxy_reflect_js() {
        let m = one("const p = new Proxy(target, handler);", "typescript", 1);
        assert_eq!(m.form, "proxy-reflect");
        assert_eq!(m.label, "Proxy/Reflect dispatch");
    }

    #[test]
    fn var_key_dispatch_runtime() {
        let m = one("emitter.emit(eventVar, data);", "javascript", 1);
        assert_eq!(m.form, "var-key-dispatch");
        assert_eq!(m.label, "string-keyed dispatch (runtime key)");
        assert!(m.key.is_none());
    }

    #[test]
    fn selector_swift_key() {
        let m = one(
            "button.addTarget(self, action: #selector(handleTap));",
            "swift",
            1,
        );
        assert_eq!(m.form, "selector");
        assert_eq!(m.label, "selector dispatch");
        assert_eq!(m.key.as_deref(), Some("handleTap"));
    }

    #[test]
    fn python_getattr_call_form() {
        let m = one(
            "def d(self, name):\n    return getattr(self, name)(args)",
            "python",
            1,
        );
        assert_eq!(m.form, "getattr-call");
        assert_eq!(m.label, "getattr dispatch");
    }

    #[test]
    fn python_getattr_assigned_form() {
        let body = "def dispatch(self, req):\n    handler = getattr(self, req.method, self.default)\n    return handler(req)";
        let m = one(body, "python", 1);
        assert_eq!(m.form, "getattr-assign");
        assert_eq!(m.label, "getattr dispatch (assigned, called later)");
    }

    #[test]
    fn comment_and_string_dispatch_does_not_fire() {
        // A dispatch shape inside a comment or a string literal must NOT fire —
        // both strippers blank contents while preserving offsets.
        let body =
            "function f(){\n  // handlers['x']()\n  const s = \"obj.send(:y)\";\n  return 1;\n}";
        assert!(scan_dynamic_dispatch(body, "typescript", 1).is_empty());
    }

    #[test]
    fn per_form_dedupe_counts_more_sites() {
        // Two computed-calls with the SAME literal key dedupe to one match with
        // moreSites=1 (`dynamic-boundaries.ts:293-298`).
        let body = "function f(){\n  reg['save']();\n  reg['save']();\n}";
        let m = one(body, "typescript", 1);
        assert_eq!(m.form, "computed-call");
        assert_eq!(m.key.as_deref(), Some("save"));
        assert_eq!(m.more_sites, 1);
        assert_eq!(m.line, 2);
    }

    #[test]
    fn max_matches_per_body_caps_at_three() {
        let body = "function f(){\n  a['p1']();\n  b['p2']();\n  c['p3']();\n  d['p4']();\n}";
        assert_eq!(scan_dynamic_dispatch(body, "typescript", 1).len(), 3);
    }

    #[test]
    fn snippet_trims_and_caps() {
        let long = "x".repeat(200);
        let body = format!("function f(){{\n  reg[a]({long});\n}}");
        let m = one(&body, "typescript", 1);
        assert!(m.snippet.ends_with("..."));
        assert_eq!(m.snippet.chars().count(), 120);
    }

    #[test]
    fn ext_rust_nested_block_comment_stripped() {
        let body = "fn f() {\n    /* outer /* inner handlers[\"x\"]() */ still */\n    let v = obj[key]();\n}";
        let ms = scan_dynamic_dispatch(body, "rust", 1);
        assert_eq!(
            ms.len(),
            1,
            "only the real call outside the comment fires: {ms:?}"
        );
        assert_eq!(ms[0].form, "computed-call");
    }

    #[test]
    fn ext_go_raw_string_and_line_comment_stripped() {
        let body = "func f() {\n    // reflect m.invoke(x)\n    s := `raw m.MethodByName(\"x\")`\n    r := reflect.ValueOf(x).MethodByName(\"Real\")\n}";
        let ms = scan_dynamic_dispatch(body, "go", 1);
        assert_eq!(
            ms.len(),
            1,
            "comment + raw-string dispatches suppressed: {ms:?}"
        );
        assert_eq!(ms[0].form, "reflection");
        assert_eq!(ms[0].key.as_deref(), Some("Real"));
    }

    #[test]
    fn ext_php_hash_and_block_comment_stripped() {
        let body = "<?php\n# call_user_func($fake)\n/* $this->$m() */\n$r = call_user_func('realHandler');";
        let ms = scan_dynamic_dispatch(body, "php", 1);
        assert_eq!(ms.len(), 1, "hash + block comment sites suppressed: {ms:?}");
        assert_eq!(ms[0].form, "php-dynamic");
        assert_eq!(ms[0].key.as_deref(), Some("realHandler"));
    }

    #[test]
    fn ext_ruby_begin_end_block_comment_stripped() {
        let body = "def run\n=begin\n obj.send(:hidden)\n=end\n  obj.send(:visible)\nend";
        let ms = scan_dynamic_dispatch(body, "ruby", 1);
        assert_eq!(ms.len(), 1, "=begin/=end body suppressed: {ms:?}");
        assert_eq!(ms[0].key.as_deref(), Some("visible"));
    }

    #[test]
    fn ext_php_dynamic_this_method_var() {
        let body = "<?php\nfunction r($this){ return $this->$method(); }";
        let m = one(body, "php", 1);
        assert_eq!(m.form, "php-dynamic");
    }

    #[test]
    fn ext_reflection_csharp_activator() {
        let m = one("var o = Activator.CreateInstance(t);", "csharp", 7);
        assert_eq!(m.form, "reflection");
        assert_eq!(m.line, 7);
    }

    #[test]
    fn ext_reflection_java_forname_with_key_window() {
        let m = one("Class.forName(\"com.example.Impl\");", "java", 1);
        assert_eq!(m.form, "reflection");
        assert_eq!(m.key.as_deref(), Some("com.example.Impl"));
    }

    #[test]
    fn ext_proxy_reflect_reflect_apply() {
        let m = one(
            "const r = Reflect.apply(fn, thisArg, args);",
            "javascript",
            1,
        );
        assert_eq!(m.form, "proxy-reflect");
    }

    #[test]
    fn ext_selector_nsclassfromstring() {
        let m = one("let c = NSClassFromString(name);", "swift", 1);
        assert_eq!(m.form, "selector");
        assert!(m.key.is_none());
    }

    #[test]
    fn ext_var_key_dispatch_dotted_path() {
        let m = one("bus.publish(config.event.name, payload);", "typescript", 1);
        assert_eq!(m.form, "var-key-dispatch");
    }

    #[test]
    fn ext_getattr_not_called_yields_nothing() {
        let body = "def d(self, name):\n    x = getattr(self, name)\n    return x";
        assert!(scan_dynamic_dispatch(body, "python", 1).is_empty());
    }

    #[test]
    fn ext_getattr_unbalanced_paren_skipped() {
        let body = "def d(self, name):\n    return getattr(self, name";
        assert!(scan_dynamic_dispatch(body, "python", 1).is_empty());
    }

    #[test]
    fn ext_getattr_dedupe_counts_more_sites() {
        let body = "def d(self):\n    getattr(self, 'x')()\n    getattr(self, 'x')()";
        let ms = scan_dynamic_dispatch(body, "python", 1);
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0].form, "getattr-call");
        assert_eq!(ms[0].more_sites, 1);
    }

    #[test]
    fn ext_unknown_language_still_scans_generic_forms() {
        let m = one("result = table[key](arg)", "haskell", 1);
        assert_eq!(m.form, "computed-call");
    }

    #[test]
    fn ext_max_body_chars_truncation_char_boundary() {
        // A body larger than MAX_BODY_CHARS is sliced on a char boundary; the
        // dispatch site sits inside the retained prefix and still fires.
        let mut body = String::from("function f(){\n  reg['save']();\n");
        body.push_str(&"  const pad = 1;\n".repeat(5000));
        body.push('}');
        assert!(body.len() > MAX_BODY_CHARS);
        let ms = scan_dynamic_dispatch(&body, "typescript", 1);
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0].key.as_deref(), Some("save"));
    }

    #[test]
    fn ext_single_string_literal_all_quote_styles() {
        assert_eq!(single_string_literal("'foo'").as_deref(), Some("foo"));
        assert_eq!(single_string_literal("\"bar\"").as_deref(), Some("bar"));
        assert_eq!(single_string_literal("`baz`").as_deref(), Some("baz"));
        assert!(single_string_literal("no literal here").is_none());
    }

    #[test]
    fn ext_typed_bus_generic_async_variant() {
        let m = one(
            "await mediator.SendAsync<Result>(new QueryThing(id));",
            "csharp",
            1,
        );
        assert_eq!(m.form, "typed-bus");
        assert_eq!(m.key.as_deref(), Some("QueryThing"));
        assert!(m.key_is_type);
    }

    #[test]
    fn ext_dynamic_import_python_dunder_import() {
        let m = one("def f(n):\n    return __import__(n)", "python", 3);
        assert_eq!(m.form, "dynamic-import");
        assert_eq!(m.line, 4);
    }

    #[test]
    fn ext_gdscript_uses_python_comment_lang() {
        let body = "func f():\n\t# handlers['x']()\n\treturn table[k]()";
        let ms = scan_dynamic_dispatch(body, "gdscript", 1);
        assert_eq!(ms.len(), 1, "comment suppressed, real call fires: {ms:?}");
        assert_eq!(ms[0].form, "computed-call");
    }

    #[test]
    fn ext_python_triple_quote_and_string_stripped() {
        let body = "def f(self, name):\n    doc = \"\"\"getattr(self, 'x')() in a docstring\"\"\"\n    s = 'obj.send(:y)'\n    return getattr(self, name)()";
        let ms = scan_dynamic_dispatch(body, "python", 1);
        assert_eq!(ms.len(), 1, "only the real getattr fires: {ms:?}");
        assert_eq!(ms[0].form, "getattr-call");
    }

    #[test]
    fn ext_python_string_with_escape_stripped() {
        let body = "def f(self, name):\n    s = 'it\\'s table[k]() here'\n    return getattr(self, name)()";
        let ms = scan_dynamic_dispatch(body, "python", 1);
        assert_eq!(ms.len(), 1, "escaped-quote string blanked: {ms:?}");
        assert_eq!(ms[0].form, "getattr-call");
    }

    #[test]
    fn ext_ruby_string_and_line_comment_stripped() {
        let body =
            "def run\n  s = \"handlers['x']()\"\n  # obj.send(:hidden)\n  obj.send(:visible)\nend";
        let ms = scan_dynamic_dispatch(body, "ruby", 1);
        assert_eq!(ms.len(), 1, "string + comment suppressed: {ms:?}");
        assert_eq!(ms[0].key.as_deref(), Some("visible"));
    }

    #[test]
    fn ext_c_style_block_and_line_comment_stripped() {
        let body = "function f(){\n  /* handlers['x']() */\n  // reg[y]()\n  return table[k]();\n}";
        let ms = scan_dynamic_dispatch(body, "java", 1);
        assert_eq!(ms.len(), 1, "block + line comment suppressed: {ms:?}");
        assert_eq!(ms[0].form, "computed-call");
    }

    #[test]
    fn ext_java_string_literal_stripped() {
        let body = "class C {\n  void f() {\n    String s = \"m.invoke(x)\";\n    var r = table[k]();\n  }\n}";
        let ms = scan_dynamic_dispatch(body, "java", 1);
        assert_eq!(ms.len(), 1, "string literal blanked: {ms:?}");
        assert_eq!(ms[0].form, "computed-call");
    }

    #[test]
    fn ext_php_string_literal_stripped() {
        let body =
            "<?php\nfunction r(){ $s = 'call_user_func($x)'; return call_user_func('real'); }";
        let ms = scan_dynamic_dispatch(body, "php", 1);
        assert_eq!(ms.len(), 1, "php string blanked: {ms:?}");
        assert_eq!(ms[0].key.as_deref(), Some("real"));
    }

    #[test]
    fn ext_go_interpreted_string_and_rune_stripped() {
        let body = "func f() {\n    s := \"m.invoke(x)\"\n    c := '('\n    r := reflect.ValueOf(x).MethodByName(\"Real\")\n}";
        let ms = scan_dynamic_dispatch(body, "go", 1);
        assert_eq!(ms.len(), 1, "interpreted string + rune blanked: {ms:?}");
        assert_eq!(ms[0].form, "reflection");
    }

    #[test]
    fn ext_rust_string_and_char_literal_stripped() {
        let body = "fn f() {\n    let s = \"handlers[\\\"x\\\"]()\";\n    let c = '(';\n    let v = table[k]();\n}";
        let ms = scan_dynamic_dispatch(body, "rust", 1);
        assert_eq!(ms.len(), 1, "string + char literal blanked: {ms:?}");
        assert_eq!(ms[0].form, "computed-call");
    }

    #[test]
    fn ext_getattr_assigned_then_immediate_call_form() {
        let body = "def dispatch(self, req):\n    fn = getattr(self, req)\n    return fn(req)";
        let m = one(body, "python", 1);
        assert_eq!(m.form, "getattr-assign");
    }

    #[test]
    fn ext_python_string_escaped_quote_branch() {
        let body =
            "def f(self, name):\n    s = \"a \\\" b table[k]()\"\n    return getattr(self, name)()";
        let ms = scan_dynamic_dispatch(body, "python", 1);
        assert_eq!(ms.len(), 1, "escaped quote keeps string intact: {ms:?}");
        assert_eq!(ms[0].form, "getattr-call");
    }

    #[test]
    fn ext_c_style_string_escaped_quote_branch() {
        let body = "function f(){\n  const s = \"esc \\\" m.invoke(x)\";\n  return table[k]();\n}";
        let ms = scan_dynamic_dispatch(body, "java", 1);
        assert_eq!(ms.len(), 1, "escaped quote blanks whole string: {ms:?}");
        assert_eq!(ms[0].form, "computed-call");
    }

    #[test]
    fn ext_php_string_escaped_quote_branch() {
        let body = "<?php\nfunction r(){ $s = \"esc \\\" call_user_func($x)\"; return call_user_func('real'); }";
        let ms = scan_dynamic_dispatch(body, "php", 1);
        assert_eq!(ms.len(), 1, "escaped quote blanks php string: {ms:?}");
        assert_eq!(ms[0].key.as_deref(), Some("real"));
    }

    #[test]
    fn ext_go_string_and_rune_escaped_quote_branch() {
        let body = "func f() {\n    s := \"esc \\\" m.invoke(x)\"\n    c := '\\''\n    r := reflect.ValueOf(x).MethodByName(\"Real\")\n}";
        let ms = scan_dynamic_dispatch(body, "go", 1);
        assert_eq!(ms.len(), 1, "escaped quotes blank string + rune: {ms:?}");
        assert_eq!(ms[0].form, "reflection");
    }

    #[test]
    fn ext_rust_string_and_char_escaped_quote_branch() {
        let body = "fn f() {\n    let s = \"esc \\\" table[x]()\";\n    let c = '\\'';\n    let v = table[k]();\n}";
        let ms = scan_dynamic_dispatch(body, "rust", 1);
        assert_eq!(ms.len(), 1, "escaped quotes blank string + char: {ms:?}");
        assert_eq!(ms[0].form, "computed-call");
    }

    #[test]
    fn ext_python_single_line_string_hits_newline_break() {
        let body = "def f(self, name):\n    s = 'unterminated\n    return getattr(self, name)()";
        let ms = scan_dynamic_dispatch(body, "python", 1);
        assert!(
            !ms.is_empty(),
            "getattr after broken string still fires: {ms:?}"
        );
    }

    #[test]
    fn ext_ruby_indented_begin_end_block() {
        let body = "def run\n   =begin\n obj.send(:hidden)\n   =end\n  obj.send(:shown)\nend";
        let ms = scan_dynamic_dispatch(body, "ruby", 1);
        assert_eq!(ms.len(), 1, "indented =end terminates block: {ms:?}");
        assert_eq!(ms[0].key.as_deref(), Some("shown"));
    }

    #[test]
    fn ext_blank_string_contents_unterminated_stops() {
        let body = "def f(self, name):\n    x = \"open string\n    return getattr(self, name)()";
        let ms = scan_dynamic_dispatch(body, "python", 1);
        assert!(
            !ms.is_empty(),
            "unterminated blank stops at newline: {ms:?}"
        );
    }

    #[test]
    fn ext_getattr_max_matches_cap() {
        let body = "def d(self):\n    getattr(self, 'aa')()\n    getattr(self, 'bb')()\n    getattr(self, 'cc')()\n    getattr(self, 'dd')()";
        let ms = scan_dynamic_dispatch(body, "python", 1);
        assert_eq!(ms.len(), MAX_MATCHES_PER_BODY);
    }
}
