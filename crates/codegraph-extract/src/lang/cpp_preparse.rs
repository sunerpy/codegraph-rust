//! The C++ pre-parse passes upstream added to `preParseCppSource` that the
//! Rust port did not have (W4-CPP-01, upstream `b635dd46`): macros in class
//! heads (#1070) and in front of return types (#1100), access specifiers
//! spelled as macros and preprocessor conditionals inside a declaration
//! (#2186), attribute macros around pointers, after parameter lists, after
//! declared names and opening declarations (#2449), lone macro lines and the
//! restore of directive lines (#1346).
//!
//! Every pass is offset-preserving: it replaces bytes with spaces (or a
//! keyword padded to the same length) and keeps every line ending, so each
//! node keeps its line and column. The macro scans match on [`masked_code`],
//! the source with comments and directives turned to spaces and literals
//! (raw strings included) to 0x01 bytes, from the same lexical scan the other
//! C++ passes use ([`cpp_code_classes`]); a word in a comment or a literal is
//! never blanked. The `regex` crate has no look-around, so each upstream
//! pattern's look-around and backtracking is written out by hand.

use std::sync::OnceLock;

use regex::Regex;

use super::cpp::{LITERAL, cpp_code_classes};

/// The text the macro scans match on, at the source's byte offsets (upstream
/// `maskCppCode`): comments and preprocessor lines are spaces, a literal keeps
/// its place as 0x01 bytes (a token, not space: in `"… using " GTEST_NAME_`
/// the macro does not start a line), line endings stay.
pub(crate) fn masked_code(source: &str) -> Vec<u8> {
    source
        .bytes()
        .zip(cpp_code_classes(source))
        .map(|(byte, class)| match (byte, class) {
            (b'\n' | b'\r', _) => byte,
            (_, super::cpp::CODE) => byte,
            (_, LITERAL) => 0x01,
            _ => b' ',
        })
        .collect()
}

/// JavaScript's `\s` over ASCII.
fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_macro_char(byte: u8) -> bool {
    byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'
}

fn skip_space(code: &[u8], mut at: usize) -> usize {
    while at < code.len() && is_space(code[at]) {
        at += 1;
    }
    at
}

/// The end of the identifier `[A-Za-z_]\w*` starting at `at`, if one does.
fn ident_end(code: &[u8], at: usize) -> Option<usize> {
    if !code.get(at).copied().is_some_and(is_ident_start) {
        return None;
    }
    let mut end = at + 1;
    while end < code.len() && is_word(code[end]) {
        end += 1;
    }
    Some(end)
}

/// Whether the word at `at` is `word` with a word boundary after it.
fn word_at(code: &[u8], at: usize, word: &str) -> bool {
    code[at..].starts_with(word.as_bytes())
        && !code.get(at + word.len()).copied().is_some_and(is_word)
}

/// The index just past the balanced `(…)` opening at `code[open]`, nesting at
/// most `depth` levels deep (upstream's `\((?:[^()]|\(…\))*\)` patterns spell
/// out two or three levels).
fn balanced_args_end(code: &[u8], open: usize, depth: usize) -> Option<usize> {
    if code.get(open) != Some(&b'(') {
        return None;
    }
    let mut level = 0usize;
    for (at, &byte) in code.iter().enumerate().skip(open) {
        if byte == b'(' {
            level += 1;
            if level > depth {
                return None;
            }
        } else if byte == b')' {
            level -= 1;
            if level == 0 {
                return Some(at + 1);
            }
        }
    }
    None
}

/// The end of a macro token `[A-Z]` + [`is_macro_char`]* starting at `at`.
fn macro_run_end(code: &[u8], at: usize) -> usize {
    let mut end = at + 1;
    while end < code.len() && is_macro_char(code[end]) {
        end += 1;
    }
    end
}

/// Whether `code[at..end]` is `[A-Z][A-Z0-9]*_[A-Z0-9_]*`: a macro with an
/// underscore.
fn is_underscored_macro(code: &[u8], at: usize, end: usize) -> bool {
    let token = &code[at..end];
    token.first().is_some_and(u8::is_ascii_uppercase)
        && token.iter().all(|&byte| is_macro_char(byte))
        && token.contains(&b'_')
}

/// Add a span for each macro of the run `code[start..end]` (upstream
/// `pushCppRunTokens`, `[A-Z][A-Z0-9_]*` with up to three levels of
/// arguments), leaving what lies between them (a comment, in the source).
fn push_run_tokens(spans: &mut Vec<(usize, usize)>, code: &[u8], start: usize, end: usize) {
    let mut at = start;
    while at < end {
        if !code[at].is_ascii_uppercase() {
            at += 1;
            continue;
        }
        let name_end = macro_run_end(code, at).min(end);
        let open = skip_space(code, name_end);
        let token_end = match balanced_args_end(code, open, 3) {
            Some(args_end) if open < end && args_end <= end => args_end,
            _ => name_end,
        };
        spans.push((at, token_end));
        at = token_end;
    }
}

/// Replace each span of `source` with spaces, keeping line endings.
fn blank_spans(source: &str, spans: &[(usize, usize)]) -> String {
    if spans.is_empty() {
        return source.to_string();
    }
    let mut bytes = source.as_bytes().to_vec();
    for &(start, end) in spans {
        for byte in bytes.iter_mut().take(end).skip(start) {
            if *byte != b'\n' && *byte != b'\r' {
                *byte = b' ';
            }
        }
    }
    String::from_utf8(bytes).unwrap_or_else(|_| source.to_string())
}

/// The index just past the template argument list opening at `code[open]`
/// (`<`), or `None` when it does not close before a `;`, `{` or `}` (it was a
/// comparison). Angle brackets inside parentheses do not count (upstream
/// `skipCppAngleArgs`).
fn skip_angle_args(code: &[u8], open: usize) -> Option<usize> {
    let mut angles = 0i32;
    let mut parens = 0i32;
    let limit = code.len().min(open + 4000);
    for at in open..limit {
        match code[at] {
            b'(' => parens += 1,
            b')' => {
                parens -= 1;
                if parens < 0 {
                    return None;
                }
            }
            b';' | b'{' | b'}' => return None,
            b'<' if parens == 0 => angles += 1,
            b'>' if parens == 0 && (at == 0 || code[at - 1] != b'-') => {
                angles -= 1;
                if angles == 0 {
                    return Some(at + 1);
                }
            }
            _ => {}
        }
    }
    None
}

// ---------------------------------------------------------------------------
// #2186: access specifiers spelled as macros, conditionals inside a declaration
// ---------------------------------------------------------------------------

/// Rewrite an access specifier spelled as a macro — nlohmann/json's
/// `JSON_PRIVATE_UNLESS_TESTED:`, Qt's `Q_SIGNALS:` — to the keyword it stands
/// for, padded to the same length (upstream #2186 `rewriteCppAccessMacros`).
/// The line holds the macro (ALL-CAPS with an underscore), its `:` and at
/// most a line comment. tree-sitter-cpp reads `NAME:` inside a class as an
/// error, and json.hpp's recovery lost the members around each one.
pub(crate) fn rewrite_cpp_access_macros(source: &str) -> String {
    static GATE: OnceLock<Regex> = OnceLock::new();
    let gate = GATE.get_or_init(|| Regex::new(r"_[A-Z0-9_]*[ \t]*:").expect("access-macro gate"));
    if !gate
        .find_iter(source)
        .any(|found| source.as_bytes().get(found.end()) != Some(&b':'))
    {
        return source.to_string();
    }
    let classes = cpp_code_classes(source);
    let mut out = source.as_bytes().to_vec();
    let mut changed = false;
    let mut line_start = 0usize;
    while line_start <= source.len() {
        let line_end = source[line_start..]
            .find('\n')
            .map_or(source.len(), |offset| line_start + offset);
        if let Some((name_start, name_end, colon)) =
            access_macro_line(source.as_bytes(), line_start, line_end)
            && classes[name_start] != LITERAL
        {
            let name = &source[name_start..name_end];
            let keyword = if name.contains("PRIVATE") {
                Some("private")
            } else if name.contains("PROTECTED") {
                Some("protected")
            } else if name.contains("PUBLIC") || name == "Q_SIGNALS" || name == "Q_SLOTS" {
                Some("public")
            } else {
                None
            };
            if let Some(keyword) = keyword
                && keyword.len() <= colon - name_start
            {
                let field = &mut out[name_start..colon];
                field[..keyword.len()].copy_from_slice(keyword.as_bytes());
                for byte in &mut field[keyword.len()..] {
                    *byte = b' ';
                }
                changed = true;
            }
        }
        if line_end >= source.len() {
            break;
        }
        line_start = line_end + 1;
    }
    if !changed {
        return source.to_string();
    }
    String::from_utf8(out).unwrap_or_else(|_| source.to_string())
}

/// `^([ \t]*)([A-Z][A-Z0-9]*_[A-Z0-9_]*)([ \t]*):(?!:)(?=[ \t]*(?://[^\n]*)?\r?$)`
/// on one line: the macro's span and its `:`.
fn access_macro_line(bytes: &[u8], start: usize, end: usize) -> Option<(usize, usize, usize)> {
    let mut at = start;
    while at < end && matches!(bytes[at], b' ' | b'\t') {
        at += 1;
    }
    let name_start = at;
    if !bytes.get(at).is_some_and(u8::is_ascii_uppercase) {
        return None;
    }
    let name_end = macro_run_end(bytes, at).min(end);
    if !is_underscored_macro(bytes, name_start, name_end) {
        return None;
    }
    let mut colon = name_end;
    while colon < end && matches!(bytes[colon], b' ' | b'\t') {
        colon += 1;
    }
    if bytes.get(colon) != Some(&b':') || bytes.get(colon + 1) == Some(&b':') {
        return None;
    }
    let mut rest = colon + 1;
    while rest < end && matches!(bytes[rest], b' ' | b'\t') {
        rest += 1;
    }
    let tail = &bytes[rest..end];
    let tail = tail.strip_suffix(b"\r").unwrap_or(tail);
    (tail.is_empty() || tail.starts_with(b"//")).then_some((name_start, name_end, colon))
}

/// Blank a preprocessor conditional in the middle of a declaration or
/// expression, keeping its first branch (upstream #2186
/// `flattenMidStatementConditionals`): inside a constructor's
/// member-initializer list (`noexcept\n#ifdef X\n : m_okay(false)\n#else\n :
/// m_okay(…)\n#endif`), between a template head and its function, inside an
/// argument list. tree-sitter-cpp reads any of them as an error, and json.hpp's
/// recovery took `basic_json` with it. Where the code before the `#if` has not
/// ended a statement or declaration (`;`, `{`, `}`, a label's `:`), its
/// directives are blanked and so is every branch after the first: one
/// consistent configuration, every newline and offset kept. A conditional
/// between whole declarations stays.
pub(crate) fn flatten_mid_statement_conditionals(source: &str) -> String {
    if !source.contains('#') {
        return source.to_string();
    }
    static DIRECTIVE: OnceLock<Regex> = OnceLock::new();
    static LINE_COMMENT: OnceLock<Regex> = OnceLock::new();
    static BLOCK_COMMENT: OnceLock<Regex> = OnceLock::new();
    static LABEL: OnceLock<Regex> = OnceLock::new();
    static ACCESS: OnceLock<Regex> = OnceLock::new();
    static STATEMENT_MACRO: OnceLock<Regex> = OnceLock::new();
    static CONTINUED: OnceLock<Regex> = OnceLock::new();
    static CONDITIONAL: OnceLock<Regex> = OnceLock::new();
    let directive =
        DIRECTIVE.get_or_init(|| Regex::new(r"^[ \t]*#[ \t]*((?-u:\w)+)").expect("directive"));
    let line_comment = LINE_COMMENT.get_or_init(|| Regex::new(r"//.*$").expect("line comment"));
    let block_comment =
        BLOCK_COMMENT.get_or_init(|| Regex::new(r"/\*.*?\*/").expect("block comment"));
    let label = LABEL.get_or_init(|| Regex::new(r"^(?-u:[\w\s])*:$").expect("label"));
    let access = ACCESS.get_or_init(|| {
        Regex::new(r"^(?:public|private|protected)\s*:$").expect("access specifier")
    });
    let statement_macro = STATEMENT_MACRO
        .get_or_init(|| Regex::new(r"^[A-Z][A-Z0-9_]*(?:\s*\(.*\))?$").expect("statement macro"));
    let continued = CONTINUED.get_or_init(|| Regex::new(r"\\\s*$").expect("continuation"));
    let conditional = CONDITIONAL.get_or_init(|| {
        Regex::new(r"^(?:if|ifdef|ifndef|else|elif|elifdef|elifndef|endif)$")
            .expect("conditional directive")
    });
    let classes = cpp_code_classes(source);
    let mut lines: Vec<String> = source.split('\n').map(str::to_string).collect();
    // The byte offset of each line, to read the lexical class of its `#`.
    let mut offsets = Vec::with_capacity(lines.len());
    let mut offset = 0usize;
    for line in &lines {
        offsets.push(offset);
        offset += line.len() + 1;
    }
    // A raw string's line is no directive, as upstream masks raw strings first.
    let directive_of = |lines: &[String], index: usize| -> Option<String> {
        let line = &lines[index];
        let captures = directive.captures(line)?;
        let hash = offsets[index] + line.find('#')?;
        (classes.get(hash) != Some(&LITERAL)).then(|| captures[1].to_string())
    };
    let code = |line: &str| -> String {
        let without_line = line_comment.replace(line, "");
        block_comment
            .replace_all(&without_line, "")
            .trim()
            .to_string()
    };
    let mut changed = false;
    // Every byte but `\r` becomes a space, so the line keeps its byte length.
    let mut blank = |lines: &mut Vec<String>, index: usize| {
        lines[index] = lines[index]
            .bytes()
            .map(|byte| if byte == b'\r' { '\r' } else { ' ' })
            .collect::<String>();
        changed = true;
    };
    let mut last_code = String::new();
    let mut in_block_comment = false;
    let mut region = 0usize;
    let mut skip_from = 0usize;
    let mut in_continuation = false;
    for index in 0..lines.len() {
        if in_continuation {
            in_continuation = continued.is_match(&lines[index]);
            continue;
        }
        let found = directive_of(&lines, index);
        if let Some(name) = found.as_deref()
            && !in_block_comment
        {
            if continued.is_match(&lines[index]) {
                in_continuation = true;
                if region == 0 {
                    continue;
                }
            }
            let opens = name.starts_with("if");
            if region == 0 {
                // Not after a statement macro written without its `;`
                // (`RAPIDJSON_DIAG_OFF(padded)`), either.
                let mid = opens
                    && !last_code.is_empty()
                    && !(last_code.ends_with([';', '{', '}'])
                        || label.is_match(&last_code)
                        || last_code.ends_with("*/"))
                    && !access.is_match(&last_code)
                    && !statement_macro.is_match(&last_code);
                if !mid {
                    continue;
                }
                region = 1;
                blank(&mut lines, index);
                continue;
            }
            if opens {
                region += 1;
            } else if (name == "else" || name == "elif") && skip_from == 0 {
                skip_from = region;
            } else if name == "endif" {
                if skip_from == region {
                    skip_from = 0;
                }
                region -= 1;
            }
            if conditional.is_match(name) {
                blank(&mut lines, index);
            }
            continue;
        }
        let text = lines[index].clone();
        if region > 0 && skip_from > 0 {
            if !text.trim().is_empty() {
                blank(&mut lines, index);
            }
            continue;
        }
        // Track block comments so a `#` or `;` inside one is not read as code.
        let trimmed = code(&text);
        if in_block_comment {
            if text.contains("*/") {
                in_block_comment = false;
            }
            continue;
        }
        // Some `/*` with no `*/` after it on the line: the last one decides.
        let uncommented = line_comment.replace(&text, "");
        if let Some(open) = uncommented.rfind("/*")
            && !uncommented[open + 2..].contains("*/")
        {
            in_block_comment = true;
            let before = trimmed.split("/*").next().unwrap_or("").trim().to_string();
            if !before.is_empty() {
                last_code = before;
            }
            continue;
        }
        if !trimmed.is_empty() {
            last_code = trimmed;
        }
    }
    if !changed {
        return source.to_string();
    }
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// #1070, #2449: macros in a class head
// ---------------------------------------------------------------------------

/// Blank the export, visibility and attribute macros between `class` or
/// `struct` and the name of a class it *defines* (upstream #1070, #2449
/// `blankCppExportMacros`): Unreal's `*_API`, Qt and Boost's `*_EXPORT`,
/// protobuf's `class PROTOBUF_EXPORT PROTOBUF_FUTURE_ADD_EARLY_WARN_UNUSED
/// Any final : public Message`, rocksdb's `struct ALIGN_AS(64U) HandleImpl`.
/// tree-sitter reads `class MACRO` as an elaborated type and the rest as a
/// function, so the class, its bases and its members drop out. The whole run
/// is blanked; the last name before the base clause or body is the class, and
/// it may carry template arguments (a specialization). Only a base clause or
/// a body after it counts, so `struct FOO var;` and `class FOO obj = …` stay.
/// The walk-time recovery (`detect_export_macro_class`) stays for what this
/// does not see.
pub(crate) fn blank_cpp_export_macros(source: &str) -> String {
    if !source.contains("class") && !source.contains("struct") {
        return source.to_string();
    }
    static GATE: OnceLock<Regex> = OnceLock::new();
    let gate =
        GATE.get_or_init(|| Regex::new(r"\b(?:class|struct)\s+[A-Z][A-Z0-9_]").expect("gate"));
    if !gate.is_match(source) {
        return source.to_string();
    }
    let code = masked_code(source);
    let mut spans = Vec::new();
    let mut at = 0usize;
    while at < code.len() {
        let keyword = if word_at(&code, at, "class") {
            5
        } else if word_at(&code, at, "struct") {
            6
        } else {
            at += 1;
            continue;
        };
        if at > 0 && is_word(code[at - 1]) {
            at += keyword;
            continue;
        }
        let head = at + keyword;
        at = head;
        let Some((run, name_start)) = class_head_run(&code, head) else {
            continue;
        };
        let Some(mut name_end) = ident_end(&code, name_start) else {
            continue;
        };
        // `(?:\s*::\s*[A-Za-z_]\w*)*`
        loop {
            let colons = skip_space(&code, name_end);
            if !code[colons..].starts_with(b"::") {
                break;
            }
            match ident_end(&code, skip_space(&code, colons + 2)) {
                Some(end) => name_end = end,
                None => break,
            }
        }
        at = name_end;
        let mut next = skip_space(&code, name_end);
        if code.get(next) == Some(&b'<') {
            let Some(after) = skip_angle_args(&code, next) else {
                continue;
            };
            next = skip_space(&code, after);
        }
        if word_at(&code, next, "final") {
            next = skip_space(&code, next + 5);
        }
        let opens_definition = code.get(next) == Some(&b'{')
            || (code.get(next) == Some(&b':') && code.get(next + 1) != Some(&b':'));
        if opens_definition {
            for &(start, end) in &run {
                push_run_tokens(&mut spans, &code, start, end);
            }
        }
    }
    blank_spans(source, &spans)
}

/// The macro run of a class head (`\s+((?:[A-Z][A-Z0-9_]+(?:\s*\((?:[^()]|\([^()]*\))*\))?\s+)+)`
/// then `(?!final\b)[A-Za-z_]\w*`) after the keyword ending at `head`: the
/// tokens and where the name starts. The run is greedy; when what follows it
/// is no name, the last macro is the name instead, as the regex backtracks.
fn class_head_run(code: &[u8], head: usize) -> Option<(Vec<(usize, usize)>, usize)> {
    let mut at = head;
    if !code.get(at).copied().is_some_and(is_space) {
        return None;
    }
    at = skip_space(code, at);
    let mut tokens: Vec<(usize, usize)> = Vec::new();
    loop {
        if !code.get(at).is_some_and(u8::is_ascii_uppercase) {
            break;
        }
        let name_end = macro_run_end(code, at);
        if name_end - at < 2 {
            break;
        }
        let open = skip_space(code, name_end);
        let token_end = balanced_args_end(code, open, 2).unwrap_or(name_end);
        if !code.get(token_end).copied().is_some_and(is_space) {
            break;
        }
        tokens.push((at, token_end));
        at = skip_space(code, token_end);
    }
    let names_class = |at: usize| ident_end(code, at).is_some() && !word_at(code, at, "final");
    if tokens.is_empty() {
        return None;
    }
    if names_class(at) {
        return Some((tokens, at));
    }
    if tokens.len() >= 2 {
        let last = tokens.pop().expect("two tokens");
        return Some((tokens, last.0));
    }
    None
}

// ---------------------------------------------------------------------------
// #1100: inline-specifier macros
// ---------------------------------------------------------------------------

/// The inline-specifier macros upstream knows (`CPP_INLINE_MACROS`).
const CPP_INLINE_MACROS: [&str; 60] = [
    "FORCEINLINE_DEBUGGABLE",
    "FORCENOINLINE",
    "FORCEINLINE",
    "PUGI__FN_NO_INLINE",
    "PUGI__FN",
    "PUGIXML_FUNCTION",
    "_ALWAYS_INLINE_",
    "_FORCE_INLINE_",
    "BOOST_FORCEINLINE",
    "BOOST_NOINLINE",
    "Q_INVOKABLE",
    "Q_SCRIPTABLE",
    "Q_ALWAYS_INLINE",
    "Q_SLOT",
    "Q_SIGNAL",
    "FOLLY_ALWAYS_INLINE",
    "FOLLY_NOINLINE",
    "ABSL_ATTRIBUTE_ALWAYS_INLINE",
    "ABSL_ATTRIBUTE_NOINLINE",
    "LLVM_ATTRIBUTE_ALWAYS_INLINE",
    "LLVM_ATTRIBUTE_NOINLINE",
    "V8_INLINE",
    "V8_NOINLINE",
    "EIGEN_STRONG_INLINE",
    "EIGEN_ALWAYS_INLINE",
    "EIGEN_DEVICE_FUNC",
    "RAPIDJSON_FORCEINLINE",
    "MOZ_ALWAYS_INLINE",
    "MOZ_NEVER_INLINE",
    "PROTOBUF_ALWAYS_INLINE",
    "PROTOBUF_NOINLINE",
    "FMT_CONSTEXPR20",
    "FMT_CONSTEXPR",
    "FMT_INLINE",
    "JSON_HEDLEY_ALWAYS_INLINE",
    "JSON_HEDLEY_NEVER_INLINE",
    "HEDLEY_ALWAYS_INLINE",
    "HEDLEY_NEVER_INLINE",
    "GLM_FUNC_QUALIFIER",
    "GLM_FUNC_DECL",
    "GLM_CONSTEXPR",
    "GLM_INLINE",
    "SIMD_FORCE_INLINE",
    "SK_ALWAYS_INLINE",
    "CV_ALWAYS_INLINE",
    "CV_INLINE",
    "EA_FORCE_INLINE",
    "EA_NOINLINE",
    "CC_INLINE",
    "NEVER_INLINE",
    "G_INLINE_FUNC",
    "SQLITE_PRIVATE",
    "SQLITE_API",
    "STDMETHODCALLTYPE",
    "WINAPIV",
    "WINAPI",
    "APIENTRY",
    "ALWAYS_INLINE",
    "FORCE_INLINE",
    "NOINLINE",
];

/// Blank a known inline-specifier macro in specifier position, before the
/// return type or name it qualifies (upstream #1100 `blankCppInlineMacros`):
/// `FORCEINLINE FString GetName(…)` glues the return type onto the name
/// otherwise. Only the curated tokens, as whole words followed by whitespace
/// and an identifier, so `HRESULT DoIt()` and `x = FORCEINLINE ? …` stay.
/// Matched on the source as upstream does, but never inside a literal
/// (KEEP-RUST: upstream only spares raw strings, and a token blanked inside an
/// ordinary string changes its text); the walk-time name recovery stays for
/// macros not on the list.
pub(crate) fn blank_cpp_inline_macros(source: &str) -> String {
    if !CPP_INLINE_MACROS
        .iter()
        .any(|macro_name| source.contains(macro_name))
    {
        return source.to_string();
    }
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        let mut names = CPP_INLINE_MACROS.to_vec();
        names.sort_by_key(|name| std::cmp::Reverse(name.len()));
        Regex::new(&format!(r"\b(?:{})\b", names.join("|"))).expect("inline macros")
    });
    let bytes = source.as_bytes();
    let classes = cpp_code_classes(source);
    let spans: Vec<(usize, usize)> = re
        .find_iter(source)
        .filter(|found| {
            let after = found.end();
            let first = skip_space(bytes, after);
            classes[found.start()] != LITERAL
                && first > after
                && bytes.get(first).copied().is_some_and(is_ident_start)
        })
        .map(|found| (found.start(), found.end()))
        .collect();
    blank_spans(source, &spans)
}

// ---------------------------------------------------------------------------
// #2449: attribute macros around declarators
// ---------------------------------------------------------------------------

/// The alternative operator spellings, which never name a declaration.
const CPP_OPERATOR_WORDS: [&str; 11] = [
    "and", "and_eq", "bitand", "bitor", "compl", "not", "not_eq", "or", "or_eq", "xor", "xor_eq",
];

/// Blank attribute macros between a pointer or reference and the name it
/// declares (upstream #2449 `blankCppPointerAnnotationMacros`): protobuf's
/// `const Descriptor* PROTOBUF_NONNULL descriptor()`, `Any* PROTOBUF_RESTRICT
/// PROTOBUF_NONNULL other`. tree-sitter takes the macro for the declared name.
/// The macro is ALL-CAPS with an underscore, right after `*` or `&`, and
/// followed by the name (or, after `*`, another `*`): a product (`a * MAX_LEN
/// + 1`) or a bit test (`flags & FOO_BIT)`) is followed by an operator or a
/// closer instead, and a `*` after `)`, `]` or a number multiplies.
pub(crate) fn blank_cpp_pointer_annotation_macros(source: &str) -> String {
    static GATE: OnceLock<Regex> = OnceLock::new();
    let gate = GATE.get_or_init(|| Regex::new(r"[*&]\s*[A-Z][A-Z0-9]*_").expect("gate"));
    if !gate.is_match(source) {
        return source.to_string();
    }
    let code = masked_code(source);
    let mut spans = Vec::new();
    let mut at = 0usize;
    while at < code.len() {
        if !matches!(code[at], b'*' | b'&') {
            at += 1;
            continue;
        }
        let operator = code[at];
        let Some((run_start, run_end)) = pointer_annotation_run(&code, at) else {
            at += 1;
            continue;
        };
        let next_at = run_end;
        // After `&`, only a name: `flags & FOO_BIT\n  && ok` is an expression.
        let reference_then_star = operator == b'&' && code.get(run_end) == Some(&b'*');
        if !reference_then_star && !multiplies(&code, at) {
            push_run_tokens(&mut spans, &code, run_start, run_end);
        }
        at = next_at;
    }
    blank_spans(source, &spans)
}

/// `([*&])\s*((?:[A-Z][A-Z0-9]*_[A-Z0-9_]*(?:\s+|(?=\*)))+)` followed by
/// `(?=\*|::|(?!<operator word>\b)[A-Za-z_~])` at `code[at]`: the run's span.
/// When what follows the greedy run is none of those, the regex gives back
/// the last macro, after which an uppercase letter always is.
fn pointer_annotation_run(code: &[u8], at: usize) -> Option<(usize, usize)> {
    let start = skip_space(code, at + 1);
    let mut ends: Vec<usize> = Vec::new();
    let mut cursor = start;
    while code.get(cursor).is_some_and(u8::is_ascii_uppercase) {
        let end = macro_run_end(code, cursor);
        if !is_underscored_macro(code, cursor, end) {
            break;
        }
        let after = skip_space(code, end);
        if after == end && code.get(end) != Some(&b'*') {
            break;
        }
        ends.push(after);
        cursor = after;
    }
    let follows = |position: usize| -> bool {
        match code.get(position) {
            Some(b'*') => true,
            Some(b':') => code.get(position + 1) == Some(&b':'),
            Some(&byte) if is_ident_start(byte) || byte == b'~' => !CPP_OPERATOR_WORDS
                .iter()
                .any(|word| word_at(code, position, word)),
            _ => false,
        }
    };
    let last = *ends.last()?;
    if follows(last) {
        return Some((start, last));
    }
    (ends.len() >= 2).then(|| (start, ends[ends.len() - 2]))
}

/// Whether the `*` at `code[at]` multiplies: a closing bracket or a number
/// stands before it (`(?:[)\]]|\b\d[\w.]*)\s*$` over the 40 bytes before).
fn multiplies(code: &[u8], at: usize) -> bool {
    let window_start = at.saturating_sub(40);
    let mut end = at;
    while end > window_start && is_space(code[end - 1]) {
        end -= 1;
    }
    if end == window_start {
        return false;
    }
    let last = code[end - 1];
    if matches!(last, b')' | b']') {
        return true;
    }
    // `\b\d[\w.]*` ending at `end`: walk back over `[\w.]`, then find a digit
    // that starts a word.
    let mut start = end;
    while start > window_start && (is_word(code[start - 1]) || code[start - 1] == b'.') {
        start -= 1;
    }
    (start..end).any(|digit| {
        code[digit].is_ascii_digit()
            && (digit == window_start || !is_word(code[digit - 1]))
            && code[digit + 1..end]
                .iter()
                .all(|&byte| is_word(byte) || byte == b'.')
    })
}

/// Keywords that take a parenthesized operand: a `)` closing one of these is
/// no parameter list (upstream `CPP_PAREN_KEYWORDS`).
const CPP_PAREN_KEYWORDS: [&str; 39] = [
    "if",
    "while",
    "for",
    "switch",
    "catch",
    "return",
    "sizeof",
    "alignof",
    "alignas",
    "decltype",
    "typeid",
    "noexcept",
    "throw",
    "static_assert",
    "new",
    "delete",
    "case",
    "requires",
    "co_await",
    "co_yield",
    "co_return",
    "asm",
    "__asm__",
    "__attribute__",
    "__attribute",
    "__declspec",
    "_Pragma",
    "else",
    "do",
    "constexpr",
    "consteval",
    "explicit",
    "typeof",
    "__typeof__",
    "__typeof",
    "_Alignof",
    "__alignof__",
    "volatile",
    "__volatile__",
];

fn is_paren_keyword(word: &str) -> bool {
    CPP_PAREN_KEYWORDS.contains(&word)
}

/// Type keywords that may stand right before a declared name.
const CPP_TYPE_KEYWORDS: [&str; 15] = [
    "void", "bool", "char", "char8_t", "char16_t", "char32_t", "wchar_t", "short", "int", "long",
    "float", "double", "signed", "unsigned", "auto",
];

/// Every keyword a declared name can never be (upstream `CPP_KEYWORDS`).
fn is_cpp_keyword(word: &str) -> bool {
    const OTHERS: [&str; 46] = [
        "const",
        "volatile",
        "static",
        "extern",
        "inline",
        "virtual",
        "explicit",
        "constexpr",
        "consteval",
        "constinit",
        "friend",
        "mutable",
        "thread_local",
        "register",
        "typedef",
        "using",
        "template",
        "typename",
        "class",
        "struct",
        "union",
        "enum",
        "namespace",
        "operator",
        "goto",
        "break",
        "continue",
        "default",
        "public",
        "private",
        "protected",
        "this",
        "true",
        "false",
        "nullptr",
        "and",
        "or",
        "not",
        "xor",
        "bitand",
        "bitor",
        "compl",
        "and_eq",
        "or_eq",
        "xor_eq",
        "not_eq",
    ];
    CPP_TYPE_KEYWORDS.contains(&word) || is_paren_keyword(word) || OTHERS.contains(&word)
}

/// Blank attribute macros after a function's parameter list (upstream #2449
/// `blankCppTrailingAttributeMacros`): protobuf's `unknown_fields() const\n
/// ABSL_ATTRIBUTE_LIFETIME_BOUND {`, `~Any() PROTOBUF_FINAL;`, the
/// thread-safety annotations of leveldb, rocksdb and abseil (`void Wait()
/// LOCKS_EXCLUDED(mu_) {`). tree-sitter reads the macro as the start of a new
/// declaration, so the method loses its `;` or its body. The `)` must close a
/// parameter list (its `(` follows a function name, not a keyword, a cast or
/// a macro call); between it and the macro only `const`, `volatile`, `&`,
/// `&&`, `noexcept`, `override` or `final` may stand, and no blank line; the
/// macro (ALL-CAPS, arguments allowed, several may stack) is followed by what
/// may end a declarator.
pub(crate) fn blank_cpp_trailing_attribute_macros(source: &str) -> String {
    static GATE: OnceLock<Regex> = OnceLock::new();
    let gate = GATE.get_or_init(|| {
        Regex::new(r"\)[\s\w&]*?\b[A-Z][A-Z0-9_]{2,}\b").expect("trailing-attribute gate")
    });
    if !gate.is_match(source) {
        return source.to_string();
    }
    let code = masked_code(source);
    let mut spans = Vec::new();
    let mut at = 0usize;
    while at < code.len() {
        if code[at] != b')' {
            at += 1;
            continue;
        }
        let close = at;
        let macro_start = trailing_qualifiers_end(&code, close + 1);
        at = close + 1;
        if !is_long_macro_word(&code, macro_start) {
            continue;
        }
        if has_blank_line(&code[close..macro_start]) {
            continue;
        }
        let run_end = trailing_macro_run_end(&code, macro_start);
        if !ends_declarator(&code, run_end) || !closes_parameter_list(&code, close) {
            continue;
        }
        push_run_tokens(&mut spans, &code, macro_start, run_end);
        at = run_end;
    }
    blank_spans(source, &spans)
}

/// `((?:\s*(?:const|volatile|noexcept|override|final|&&|&)(?!\w))*)\s*` from
/// `at`: where the macro would start.
fn trailing_qualifiers_end(code: &[u8], mut at: usize) -> usize {
    loop {
        let word_start = skip_space(code, at);
        let qualifier = ["const", "volatile", "noexcept", "override", "final"]
            .iter()
            .find(|word| word_at(code, word_start, word))
            .map(|word| word.len())
            .or_else(|| {
                if code[word_start..].starts_with(b"&&") {
                    Some(2)
                } else if code.get(word_start) == Some(&b'&') {
                    Some(1)
                } else {
                    None
                }
            });
        match qualifier {
            Some(length) if !code.get(word_start + length).copied().is_some_and(is_word) => {
                at = word_start + length;
            }
            _ => return skip_space(code, at),
        }
    }
}

/// `[A-Z][A-Z0-9_]{2,}\b` at `at`.
fn is_long_macro_word(code: &[u8], at: usize) -> bool {
    if !code.get(at).is_some_and(u8::is_ascii_uppercase) {
        return false;
    }
    let end = macro_run_end(code, at);
    end - at >= 3 && !code.get(end).copied().is_some_and(is_word)
}

/// `\n[ \t]*\r?\n` in `text`.
fn has_blank_line(text: &[u8]) -> bool {
    text.iter().enumerate().any(|(at, &byte)| {
        if byte != b'\n' {
            return false;
        }
        let mut next = at + 1;
        while next < text.len() && matches!(text[next], b' ' | b'\t') {
            next += 1;
        }
        if text.get(next) == Some(&b'\r') {
            next += 1;
        }
        text.get(next) == Some(&b'\n')
    })
}

/// `(?:[A-Z][A-Z0-9_]{2,}\b(?:\s*\((?:[^()]|\((?:[^()]|\([^()]*\))*\))*\))?\s*)+`
/// from `at`, which [`is_long_macro_word`]: the run's end, trailing space
/// included.
fn trailing_macro_run_end(code: &[u8], at: usize) -> usize {
    let mut end = at;
    while is_long_macro_word(code, end) {
        let name_end = macro_run_end(code, end);
        let open = skip_space(code, name_end);
        let token_end = balanced_args_end(code, open, 3).unwrap_or(name_end);
        end = skip_space(code, token_end);
    }
    end
}

/// `(?:\{|;|=(?!=)|:(?!:)|->|(?:override|final|const|noexcept)\b)` at `at`.
fn ends_declarator(code: &[u8], at: usize) -> bool {
    match code.get(at) {
        Some(b'{' | b';') => true,
        Some(b'=') => code.get(at + 1) != Some(&b'='),
        Some(b':') => code.get(at + 1) != Some(&b':'),
        Some(b'-') => code.get(at + 1) == Some(&b'>'),
        _ => ["override", "final", "const", "noexcept"]
            .iter()
            .any(|word| word_at(code, at, word)),
    }
}

/// Whether the `)` at `code[close]` closes a function declarator's parameter
/// list (upstream `closesParameterList`): its `(` follows `operator…` or a
/// function name that is no keyword and no ALL-CAPS macro.
fn closes_parameter_list(code: &[u8], close: usize) -> bool {
    let mut depth = 0i32;
    let mut open = None;
    let mut at = close as isize;
    while at >= 0 && close - at as usize <= 4000 {
        match code[at as usize] {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    open = Some(at as usize);
                    break;
                }
            }
            b';' | b'{' | b'}' => return false,
            _ => {}
        }
        at -= 1;
    }
    let Some(open) = open else {
        return false;
    };
    let before = &code[open.saturating_sub(200)..open];
    if operator_before(before) {
        return true;
    }
    let mut end = before.len();
    while end > 0 && is_space(before[end - 1]) {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && is_word(before[start - 1]) {
        start -= 1;
    }
    // `([A-Za-z_]\w*)\s*$`: the identifier must start with a letter or `_`.
    while start < end && before[start].is_ascii_digit() {
        start += 1;
    }
    if start == end {
        return false;
    }
    let name = std::str::from_utf8(&before[start..end]).unwrap_or("");
    // `^[A-Z][A-Z0-9_]*$`: a macro call, not a function.
    let macro_call = name.as_bytes()[0].is_ascii_uppercase() && name.bytes().all(is_macro_char);
    !is_paren_keyword(name) && !macro_call
}

/// `\boperator\s*(?:\(\s*\)|\[\s*\]|[^\s\w()[\]]{1,3}|\s+[A-Za-z_][\w\s:<>*&]*)\s*$`
/// over `before`.
fn operator_before(before: &[u8]) -> bool {
    static RE: OnceLock<regex::bytes::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::bytes::Regex::new(
            r"(?-u)\boperator\s*(?:\(\s*\)|\[\s*\]|[^\s\w()\[\]]{1,3}|\s+[A-Za-z_][\w\s:<>*&]*)\s*$",
        )
        .expect("operator before")
    });
    re.is_match(before)
}

/// Blank attribute macros after the name a declaration declares, or after its
/// array brackets (upstream #2449 `blankCppDeclaratorAttributeMacros`):
/// leveldb's `port::CondVar cv GUARDED_BY(mu);`, abseil's `ABSL_GUARDED_BY`,
/// `const absl::Cord* cord ABSL_ATTRIBUTE_LIFETIME_BOUND)`, protobuf's
/// `Any_globals_ PROTOBUF_MESSAGE_GLOBALS_SECTION(.data.rel.ro);` and
/// `offsets[] ABSL_ATTRIBUTE_SECTION_VARIABLE(protodesc_cold) = {`. The macro
/// is ALL-CAPS with an underscore, arguments allowed, followed by `;`, `=`,
/// `,` or `)`; before it stands `]` or a name with a lowercase letter that
/// itself follows a type. `Foo DEFAULT_OPTIONS;`, `typedef Foo BAR_T;` and
/// `return x FOO;` stay.
pub(crate) fn blank_cpp_declarator_attribute_macros(source: &str) -> String {
    static GATE: OnceLock<Regex> = OnceLock::new();
    let gate = GATE.get_or_init(|| {
        Regex::new(r"[\w\]]\s+[A-Z][A-Z0-9]*_[A-Z0-9_]*\s*[(;=,)]").expect("declarator gate")
    });
    if !gate.is_match(source) {
        return source.to_string();
    }
    let code = masked_code(source);
    let mut spans = Vec::new();
    let mut at = 0usize;
    while at < code.len() {
        // `(?<![\w$])[A-Z][A-Z0-9]*_[A-Z0-9_]*\b`
        if !code[at].is_ascii_uppercase()
            || (at > 0 && (is_word(code[at - 1]) || code[at - 1] == b'$'))
        {
            at += 1;
            continue;
        }
        let macro_start = at;
        let token_end = macro_run_end(&code, at);
        at = token_end;
        if !is_underscored_macro(&code, macro_start, token_end)
            || code.get(token_end).copied().is_some_and(is_word)
        {
            continue;
        }
        let mut last = macro_start;
        while last > 0 && is_space(code[last - 1]) {
            last -= 1;
        }
        if last == macro_start || last == 0 {
            continue;
        }
        let last = last - 1;
        if !(is_word(code[last]) || code[last] == b']') {
            continue;
        }
        let run_end = declarator_macro_run_end(&code, macro_start);
        let ends = match code.get(run_end) {
            Some(b';' | b',' | b')') => true,
            Some(b'=') => code.get(run_end + 1) != Some(&b'='),
            _ => false,
        };
        if !ends {
            continue;
        }
        if code[last] != b']' && !follows_declared_name(&code, last) {
            continue;
        }
        push_run_tokens(&mut spans, &code, macro_start, run_end);
        at = run_end;
    }
    blank_spans(source, &spans)
}

/// `(?:[A-Z][A-Z0-9]*_[A-Z0-9_]*\b(?:\s*\((?:[^()]|\((?:[^()]|\([^()]*\))*\))*\))?\s*)+`
/// from `at`: the run's end, trailing space included.
fn declarator_macro_run_end(code: &[u8], at: usize) -> usize {
    let mut end = at;
    loop {
        if !code.get(end).is_some_and(u8::is_ascii_uppercase) {
            return end;
        }
        let name_end = macro_run_end(code, end);
        if !is_underscored_macro(code, end, name_end)
            || code.get(name_end).copied().is_some_and(is_word)
        {
            return end;
        }
        let open = skip_space(code, name_end);
        let token_end = balanced_args_end(code, open, 3).unwrap_or(name_end);
        end = skip_space(code, token_end);
    }
}

/// Whether the identifier ending at `code[last]` is a name a declaration
/// declares (upstream `followsDeclaredName`).
fn follows_declared_name(code: &[u8], last: usize) -> bool {
    let mut start = last;
    while start > 0 && is_word(code[start - 1]) {
        start -= 1;
    }
    let name = &code[start..=last];
    if !name.first().copied().is_some_and(is_ident_start)
        || !name.iter().any(u8::is_ascii_lowercase)
        || is_cpp_keyword(std::str::from_utf8(name).unwrap_or(""))
    {
        return false;
    }
    let mut at = start as isize - 1;
    while at >= 0 && is_space(code[at as usize]) {
        at -= 1;
    }
    if at < 0 {
        return false;
    }
    let at = at as usize;
    match code[at] {
        b'*' | b'&' => return true,
        b'>' => return at == 0 || code[at - 1] != b'-',
        byte if !is_word(byte) => return false,
        _ => {}
    }
    let mut word_start = at;
    while word_start > 0 && is_word(code[word_start - 1]) {
        word_start -= 1;
    }
    let before = std::str::from_utf8(&code[word_start..=at]).unwrap_or("");
    if before == "const" || before == "volatile" {
        let mut k = word_start as isize - 1;
        while k >= 0 && is_space(code[k as usize]) {
            k -= 1;
        }
        return k >= 0 && matches!(code[k as usize], b'*' | b'&');
    }
    if CPP_TYPE_KEYWORDS.contains(&before) {
        return true;
    }
    before
        .as_bytes()
        .first()
        .copied()
        .is_some_and(is_ident_start)
        && !is_cpp_keyword(before)
}

/// Blank attribute macros that open a declaration, at the start of a line or
/// after a `[[…]]` attribute (upstream #2449 `blankCppLeadingAttributeMacros`):
/// protobuf's `PROTOBUF_FUTURE_ADD_EARLY_NODISCARD absl::string_view name()
/// const {`, `ABSL_ATTRIBUTE_REINITIALIZES void Clear()`, `[[nodiscard]]
/// PROTOBUF_NDEBUG_INLINE Ptr<T> Make(…)`, googletest's `GTEST_API_
/// std::string JoinAsTuple(…)`, {fmt}'s `FMT_EXPORT struct …`. tree-sitter
/// takes the macro for the type, which swallows the members after it. The
/// macro (ALL-CAPS with an underscore; several may stack) must be followed by
/// what cannot follow a type name: a declaration keyword, `[[`, an
/// out-of-line constructor or destructor, or a type and then the declared
/// name. A type is followed by the name alone (`DWORD_PTR value = 0;`,
/// `HANDLE_T Open(…)`), so it never matches.
pub(crate) fn blank_cpp_leading_attribute_macros(source: &str) -> String {
    static GATE: OnceLock<Regex> = OnceLock::new();
    let gate = GATE.get_or_init(|| {
        Regex::new(r"(?m)(?:^|\]\])[ \t]*[A-Z][A-Z0-9]*_[A-Z0-9_]*\s").expect("leading gate")
    });
    if !gate.is_match(source) {
        return source.to_string();
    }
    let code = masked_code(source);
    let skip_blanks = |mut at: usize| {
        while at < code.len() && matches!(code[at], b' ' | b'\t') {
            at += 1;
        }
        at
    };
    let mut spans = Vec::new();
    let mut at = 0usize;
    while at < code.len() {
        // `(^[ \t]*|\]\][ \t]*)`: the first alternative, then the second.
        let mut matched = None;
        if at == 0 || matches!(code[at - 1], b'\n' | b'\r') {
            let run_start = skip_blanks(at);
            matched = leading_attribute_run(&code, run_start).map(|end| (run_start, end));
        }
        if matched.is_none() && code[at..].starts_with(b"]]") {
            let run_start = skip_blanks(at + 2);
            matched = leading_attribute_run(&code, run_start).map(|end| (run_start, end));
        }
        match matched {
            Some((run_start, run_end)) => {
                push_run_tokens(&mut spans, &code, run_start, run_end);
                at = run_end;
            }
            None => at += 1,
        }
    }
    blank_spans(source, &spans)
}

/// The macro run `((?:[A-Z][A-Z0-9]*_[A-Z0-9_]*\s+)+)` at `start` and the
/// look-ahead after it: the run's end. The run is greedy and gives back its
/// last macros until the look-ahead holds, as the regex backtracks.
fn leading_attribute_run(code: &[u8], start: usize) -> Option<usize> {
    let mut ends = Vec::new();
    let mut at = start;
    while code.get(at).is_some_and(u8::is_ascii_uppercase) {
        let end = macro_run_end(code, at);
        if !is_underscored_macro(code, at, end) || !code.get(end).copied().is_some_and(is_space) {
            break;
        }
        at = skip_space(code, end);
        ends.push(at);
    }
    ends.into_iter()
        .rev()
        .find(|&end| opens_declaration(code, end))
}

/// The declaration keywords after which a macro run opens a declaration.
const LEADING_KEYWORDS: [&str; 36] = [
    "static",
    "inline",
    "virtual",
    "explicit",
    "constexpr",
    "consteval",
    "constinit",
    "friend",
    "extern",
    "thread_local",
    "mutable",
    "typedef",
    "using",
    "template",
    "typename",
    "class",
    "struct",
    "union",
    "enum",
    "void",
    "bool",
    "char",
    "char8_t",
    "char16_t",
    "char32_t",
    "wchar_t",
    "short",
    "int",
    "long",
    "float",
    "double",
    "signed",
    "unsigned",
    "auto",
    "decltype",
    "return",
];

/// The look-ahead of upstream's `CPP_LEADING_ATTR_RE` at `at`: a declaration
/// keyword, `[[`, an out-of-line constructor or destructor (`Any::Impl_::Impl_(`,
/// `Any::~Any(`), or a type and then the name it declares.
fn opens_declaration(code: &[u8], at: usize) -> bool {
    LEADING_KEYWORDS
        .iter()
        .any(|keyword| word_at(code, at, keyword))
        || code[at..].starts_with(b"[[")
        || out_of_line_constructor(code, at)
        || type_then_declared_name(code, at)
}

/// `(?:[A-Za-z_]\w*\s*::\s*)*([A-Za-z_]\w*)\s*::\s*~?\3\s*\(` at `at`.
fn out_of_line_constructor(code: &[u8], at: usize) -> bool {
    // The `::`-separated segments, each with whether a `~` led it.
    let mut segments: Vec<(&[u8], bool)> = Vec::new();
    let mut cursor = at;
    loop {
        let tilde = !segments.is_empty() && code.get(cursor) == Some(&b'~');
        let name_start = if tilde { cursor + 1 } else { cursor };
        let Some(name_end) = ident_end(code, name_start) else {
            break;
        };
        segments.push((&code[name_start..name_end], tilde));
        let colons = skip_space(code, name_end);
        if !code[colons..].starts_with(b"::") {
            cursor = name_end;
            break;
        }
        cursor = skip_space(code, colons + 2);
    }
    if segments.len() < 2
        || segments[..segments.len() - 1]
            .iter()
            .any(|(_, tilde)| *tilde)
    {
        return false;
    }
    let (last, _) = segments[segments.len() - 1];
    let (class, _) = segments[segments.len() - 2];
    last == class && code.get(skip_space(code, cursor)) == Some(&b'(')
}

/// `<(?:[^<>;{}]|<(?:[^<>;{}]|<[^<>;{}]*>)*>)*>` at `at`: the end of template
/// arguments nesting at most three levels.
fn template_args_end(code: &[u8], at: usize) -> Option<usize> {
    if code.get(at) != Some(&b'<') {
        return None;
    }
    let mut depth = 0usize;
    for (index, &byte) in code.iter().enumerate().skip(at) {
        match byte {
            b'<' => {
                depth += 1;
                if depth > 3 {
                    return None;
                }
            }
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            b';' | b'{' | b'}' => return None,
            _ => {}
        }
    }
    None
}

/// The ends a `CPP_TYPE_NAME` starting at `at` can have, longest first:
/// `(?:::\s*)?(?!(?:const|volatile|operator|return)\b)[A-Za-z_]\w*(?:\s*<…>)?`
/// then `(?:\s*::\s*[A-Za-z_]\w*(?:\s*<…>)?)*`.
fn type_name_ends(code: &[u8], at: usize) -> Vec<usize> {
    let mut start = at;
    if code[start..].starts_with(b"::") {
        start = skip_space(code, start + 2);
    }
    if ["const", "volatile", "operator", "return"]
        .iter()
        .any(|word| word_at(code, start, word))
    {
        return Vec::new();
    }
    let Some(mut end) = ident_end(code, start) else {
        return Vec::new();
    };
    let with_args = |end: usize| -> usize {
        let open = skip_space(code, end);
        template_args_end(code, open).unwrap_or(end)
    };
    end = with_args(end);
    let mut ends = vec![end];
    loop {
        let colons = skip_space(code, end);
        if !code[colons..].starts_with(b"::") {
            break;
        }
        let Some(segment_end) = ident_end(code, skip_space(code, colons + 2)) else {
            break;
        };
        end = with_args(segment_end);
        ends.push(end);
    }
    ends.reverse();
    ends
}

/// `CPP_DECLARED_NAME` at `at`:
/// `(?:(?:::\s*)?~?[A-Za-z_]\w*(?:\s*::\s*~?[A-Za-z_]\w*)*\s*[(;=[{]|operator\b)`.
fn declared_name(code: &[u8], at: usize) -> bool {
    if word_at(code, at, "operator") {
        return true;
    }
    let mut cursor = at;
    if code[cursor..].starts_with(b"::") {
        cursor = skip_space(code, cursor + 2);
    }
    if code.get(cursor) == Some(&b'~') {
        cursor += 1;
    }
    let Some(mut end) = ident_end(code, cursor) else {
        return false;
    };
    loop {
        let colons = skip_space(code, end);
        if !code[colons..].starts_with(b"::") {
            break;
        }
        let mut segment = skip_space(code, colons + 2);
        if code.get(segment) == Some(&b'~') {
            segment += 1;
        }
        match ident_end(code, segment) {
            Some(segment_end) => end = segment_end,
            None => break,
        }
    }
    matches!(
        code.get(skip_space(code, end)),
        Some(b'(' | b';' | b'=' | b'[' | b'{')
    )
}

/// `const` or `volatile` with a word boundary at `at`: its length.
fn cv_word(code: &[u8], at: usize) -> Option<usize> {
    ["const", "volatile"]
        .iter()
        .find(|word| word_at(code, at, word))
        .map(|word| word.len())
}

/// The fourth look-ahead alternative:
/// `(?:(?:const|volatile)\s+)*TYPE(?:\s*(?:const|volatile)\b)?(?:\s*[*&]+\s*|\s+)(?:(?:const|volatile)\s+)?NAME`.
fn type_then_declared_name(code: &[u8], at: usize) -> bool {
    // Leading cv-qualifiers, each followed by space. A type never starts with
    // one, so taking fewer of them cannot help.
    let mut start = at;
    while let Some(length) = cv_word(code, start) {
        let after = start + length;
        if !code.get(after).copied().is_some_and(is_space) {
            break;
        }
        start = skip_space(code, after);
    }
    for type_end in type_name_ends(code, start) {
        // `(?:\s*(?:const|volatile)\b)?`, with and without.
        let mut after_types = vec![];
        let qualifier = skip_space(code, type_end);
        if let Some(length) = cv_word(code, qualifier) {
            after_types.push(qualifier + length);
        }
        after_types.push(type_end);
        for after_type in after_types {
            // `(?:\s*[*&]+\s*|\s+)`
            let mut separators = vec![];
            let stars = skip_space(code, after_type);
            if matches!(code.get(stars), Some(b'*' | b'&')) {
                let mut end = stars;
                while matches!(code.get(end), Some(b'*' | b'&')) {
                    end += 1;
                }
                separators.push(skip_space(code, end));
            }
            if code.get(after_type).copied().is_some_and(is_space) {
                separators.push(skip_space(code, after_type));
            }
            for name_at in separators {
                // `(?:(?:const|volatile)\s+)?`, with and without.
                if let Some(length) = cv_word(code, name_at)
                    && code.get(name_at + length).copied().is_some_and(is_space)
                    && declared_name(code, skip_space(code, name_at + length))
                {
                    return true;
                }
                if declared_name(code, name_at) {
                    return true;
                }
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// #1346: lone macro lines and directive lines
// ---------------------------------------------------------------------------

/// Blank a macro that is the only token on its line, no parentheses, no
/// semicolon (upstream #1346 `blankLoneMacroLines`): {fmt}'s
/// `FMT_BEGIN_NAMESPACE`, Qt's `Q_OBJECT`. A bare identifier is no statement
/// or declaration, so tree-sitter drops into error recovery at each one. The
/// line holds one ALL-CAPS token (four characters or more, with an
/// underscore) and at most a comment; the previous line of code does not end
/// in a continuation (`=`, an operator, `,`, `(`, `?`, `:`, a `\`), a template
/// head aside; the next line of code starts like a declaration or scope
/// (letter, `_`, `#`, `{`, `}`, `~`) or the file ends. C++ only here: the C
/// pre-parse stays as it was (KEEP-RUST).
pub(crate) fn blank_lone_macro_lines(source: &str) -> String {
    static GATE: OnceLock<Regex> = OnceLock::new();
    static LINE: OnceLock<Regex> = OnceLock::new();
    static CONTINUES: OnceLock<Regex> = OnceLock::new();
    static TEMPLATE: OnceLock<Regex> = OnceLock::new();
    static PREVIOUS_COMMENTS: OnceLock<Regex> = OnceLock::new();
    let gate = GATE.get_or_init(|| {
        Regex::new(r"(?m)^[ \t]*[A-Z][A-Z0-9_]{3,}[ \t]*\r?$").expect("lone macro gate")
    });
    if !gate.is_match(source) {
        return source.to_string();
    }
    let line_re = LINE.get_or_init(|| {
        Regex::new(r"^[ \t]*([A-Z][A-Z0-9_]{3,})[ \t]*(?://[^\n\r]*|/\*[^\n\r]*\*/[ \t]*)?\r?$")
            .expect("lone macro line")
    });
    let continues =
        CONTINUES.get_or_init(|| Regex::new(r"[=+\-*/%&|^<>?:,(\\]$").expect("continuation end"));
    let template = TEMPLATE.get_or_init(|| Regex::new(r"^template\s*<").expect("template head"));
    let previous_comments = PREVIOUS_COMMENTS
        .get_or_init(|| Regex::new(r"/\*.*?\*/|//.*$").expect("previous comments"));
    let classes = cpp_code_classes(source);
    let mut lines: Vec<String> = source.split('\n').map(str::to_string).collect();
    let mut offsets = Vec::with_capacity(lines.len());
    let mut offset = 0usize;
    for line in &lines {
        offsets.push(offset);
        offset += line.len() + 1;
    }
    let content = |line: &str| line.strip_suffix('\r').unwrap_or(line).trim().to_string();
    let mut changed = false;
    for index in 0..lines.len() {
        let Some(captures) = line_re.captures(&lines[index]) else {
            continue;
        };
        let token = captures.get(1).expect("token group");
        // A raw string's line is no code, as upstream masks raw strings.
        if !token.as_str().contains('_')
            || classes.get(offsets[index] + token.start()) == Some(&LITERAL)
        {
            continue;
        }
        let mut previous = index as isize - 1;
        while previous >= 0 && content(&lines[previous as usize]).is_empty() {
            previous -= 1;
        }
        let previous_text = if previous >= 0 {
            content(&lines[previous as usize])
        } else {
            String::new()
        };
        let previous_code = if previous_text.starts_with("//")
            || previous_text.starts_with("/*")
            || previous_text.starts_with('*')
        {
            String::new()
        } else {
            previous_comments
                .replace_all(&previous_text, "")
                .trim()
                .to_string()
        };
        if !previous_code.is_empty()
            && !template.is_match(&previous_code)
            && continues.is_match(&previous_code)
        {
            continue;
        }
        if let Some(first) = first_code_char_after(&lines, index + 1)
            && !(first.is_ascii_alphabetic() || matches!(first, '_' | '#' | '{' | '}' | '~'))
        {
            continue;
        }
        let (start, end) = (token.start(), token.end());
        let line = &mut lines[index];
        line.replace_range(start..end, &" ".repeat(end - start));
        changed = true;
    }
    if !changed {
        return source.to_string();
    }
    lines.join("\n")
}

/// The first character of code at or after line `from`, past blank lines
/// and comments (upstream `firstCodeCharAfter`).
fn first_code_char_after(lines: &[String], from: usize) -> Option<char> {
    let mut in_block = false;
    for line in lines.iter().skip(from) {
        let mut text = line.trim().to_string();
        loop {
            if in_block {
                match text.find("*/") {
                    None => {
                        text.clear();
                        break;
                    }
                    Some(end) => {
                        text = text[end + 2..].trim().to_string();
                        in_block = false;
                    }
                }
            }
            if text.starts_with("//") {
                text.clear();
            } else if let Some(rest) = text.strip_prefix("/*") {
                in_block = true;
                text = rest.to_string();
                continue;
            }
            break;
        }
        if let Some(first) = text.chars().next() {
            return Some(first);
        }
    }
    None
}

/// Copy the directive lines of `original` (and their `\` continuations) back
/// over `blanked` (upstream #1346 `restoreDirectiveLines`): tree-sitter keeps
/// a `#define` body as raw text it does not parse, so blanking there only
/// breaks the directive. A raw string's line is no directive.
pub(crate) fn restore_directive_lines(original: &str, blanked: &str) -> String {
    if blanked == original || !original.contains('#') {
        return blanked.to_string();
    }
    static DIRECTIVE: OnceLock<Regex> = OnceLock::new();
    static CONTINUED: OnceLock<Regex> = OnceLock::new();
    let directive = DIRECTIVE.get_or_init(|| Regex::new(r"^[ \t]*#").expect("directive line"));
    let continued = CONTINUED.get_or_init(|| Regex::new(r"\\\s*$").expect("continuation"));
    let classes = cpp_code_classes(original);
    let original_lines: Vec<&str> = original.split('\n').collect();
    let mut lines: Vec<String> = blanked.split('\n').map(str::to_string).collect();
    let mut changed = false;
    let mut continuation = false;
    let mut offset = 0usize;
    for (index, line) in original_lines.iter().enumerate() {
        if index >= lines.len() {
            break;
        }
        let hash = line.find('#').map(|at| offset + at);
        let is_directive = continuation
            || (directive.is_match(line) && hash.and_then(|at| classes.get(at)) != Some(&LITERAL));
        if is_directive && lines[index] != *line {
            lines[index] = (*line).to_string();
            changed = true;
        }
        continuation = is_directive && continued.is_match(line.strip_suffix('\r').unwrap_or(line));
        offset += line.len() + 1;
    }
    if !changed {
        return blanked.to_string();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `blank` removes exactly `macros` from `input` and nothing else, every
    /// offset kept (upstream `expectBlanked`).
    #[track_caller]
    fn expect_blanked(blank: fn(&str) -> String, input: &str, macros: &[&str]) {
        let out = blank(input);
        assert_eq!(out.len(), input.len(), "{input:?}");
        let mut expected = input.to_string();
        for macro_text in macros {
            let at = expected
                .find(macro_text)
                .unwrap_or_else(|| panic!("{macro_text:?} in {input:?}"));
            let blanked: String = macro_text
                .chars()
                .map(|c| if c == '\n' || c == '\r' { c } else { ' ' })
                .collect();
            expected.replace_range(at..at + macro_text.len(), &blanked);
        }
        assert_eq!(out, expected, "{input:?}");
    }

    #[track_caller]
    fn expect_untouched(blank: fn(&str) -> String, inputs: &[&str]) {
        for input in inputs {
            assert_eq!(blank(input), *input, "{input:?}");
        }
    }

    #[test]
    fn blanks_every_macro_between_class_and_the_name_and_before_a_specialization() {
        expect_blanked(
            blank_cpp_export_macros,
            "class PROTOBUF_EXPORT  PROTOBUF_FUTURE_ADD_EARLY_WARN_UNUSED Any final : public ::google::protobuf::Message {};",
            &["PROTOBUF_EXPORT  PROTOBUF_FUTURE_ADD_EARLY_WARN_UNUSED"],
        );
        expect_blanked(
            blank_cpp_export_macros,
            "template <typename Element>\nclass ABSL_ATTRIBUTE_WARN_UNUSED PROTOBUF_DECLSPEC_EMPTY_BASES\n    RepeatedField final\n    : private internal::RepeatedFieldBase {};",
            &[
                "ABSL_ATTRIBUTE_WARN_UNUSED",
                "PROTOBUF_DECLSPEC_EMPTY_BASES",
            ],
        );
        expect_blanked(
            blank_cpp_export_macros,
            "template <typename T, bool kOrProxy>\nclass PROTOBUF_DECLSPEC_EMPTY_BASES RepeatedFieldProxyWithSet<\n    T, kOrProxy,\n    std::enable_if_t<IsMessage<T>>> {};",
            &["PROTOBUF_DECLSPEC_EMPTY_BASES"],
        );
        expect_blanked(
            blank_cpp_export_macros,
            "struct ALIGN_AS(64U) HandleImpl : public ClockHandle {};",
            &["ALIGN_AS(64U)"],
        );
        // An all-caps class name after a macro is the name, `final` included.
        expect_blanked(
            blank_cpp_export_macros,
            "class FOO_API BAR_IMPL final : public X {};",
            &["FOO_API"],
        );
    }

    #[test]
    fn leaves_class_heads_alone_when_nothing_is_defined_there() {
        expect_untouched(
            blank_cpp_export_macros,
            &[
                "class FOO_API Name;",
                "class FOO_API Name<int> var;",
                "struct STAT_T st = {0};",
                "class GTEST_1_TUPLE_(T) {",
                "// class PROTOBUF_EXPORT Any final : public Message {",
                "const char* s = \"class FOO Bar {\";",
            ],
        );
    }

    #[test]
    fn blanks_annotations_between_a_pointer_or_reference_and_the_declared_name() {
        expect_blanked(
            blank_cpp_pointer_annotation_macros,
            "static const Descriptor* PROTOBUF_NONNULL descriptor() { return d; }",
            &["PROTOBUF_NONNULL"],
        );
        expect_blanked(
            blank_cpp_pointer_annotation_macros,
            "void Any::InternalSwap(Any* PROTOBUF_RESTRICT PROTOBUF_NONNULL other) {}",
            &["PROTOBUF_RESTRICT PROTOBUF_NONNULL"],
        );
        expect_blanked(
            blank_cpp_pointer_annotation_macros,
            "void f(const FieldDescriptor * PROTOBUF_NULLABLE *\n    PROTOBUF_NONNULL field, const T& ABSL_ATTRIBUTE_LIFETIME_BOUND v);",
            &[
                "PROTOBUF_NULLABLE",
                "PROTOBUF_NONNULL",
                "ABSL_ATTRIBUTE_LIFETIME_BOUND",
            ],
        );
        expect_blanked(
            blank_cpp_pointer_annotation_macros,
            "static constexpr const EnumDescriptor* PROTOBUF_NONNULL* PROTOBUF_NULLABLE\n    file_level_enum_descriptors = nullptr;",
            &["PROTOBUF_NONNULL", "PROTOBUF_NULLABLE"],
        );
    }

    #[test]
    fn leaves_products_bit_tests_unnamed_parameters_and_comments_alone() {
        expect_untouched(
            blank_cpp_pointer_annotation_macros,
            &[
                "int n = count * MAX_LEN + 1;",
                "bool b = (flags & FOO_BIT) != 0;",
                "bool b = flags & FOO_BIT\n    && enabled;",
                "bool b = flags & FOO_BIT and enabled;",
                "void f(Foo* PROTOBUF_NONNULL);",
                "/**\n * NOTE_THIS applies\n * TODO_LATER fix it\n */",
                "const char* s = \"a * PROTOBUF_NONNULL b\";",
                "int digits10 = int(sizeof(int) * CHAR_BIT * 3 / 10);",
                "int n = 2 * PAGE_SIZE * pages;",
                "#define PTR_T int *\nFOO_BAR name;",
            ],
        );
    }

    #[test]
    fn blanks_attribute_macros_after_a_parameter_list() {
        expect_blanked(
            blank_cpp_trailing_attribute_macros,
            "const UnknownFieldSet& unknown_fields() const\n      ABSL_ATTRIBUTE_LIFETIME_BOUND {\n  return f;\n}",
            &["ABSL_ATTRIBUTE_LIFETIME_BOUND"],
        );
        expect_blanked(
            blank_cpp_trailing_attribute_macros,
            "~Any() PROTOBUF_FINAL;",
            &["PROTOBUF_FINAL"],
        );
        expect_blanked(
            blank_cpp_trailing_attribute_macros,
            "void Lock() EXCLUSIVE_LOCK_FUNCTION() { mu_.lock(); }\nvoid Wait() LOCKS_EXCLUDED(mu_);",
            &["EXCLUSIVE_LOCK_FUNCTION()", "LOCKS_EXCLUDED(mu_)"],
        );
        expect_blanked(
            blank_cpp_trailing_attribute_macros,
            "explicit MutexLock(port::Mutex* mu) EXCLUSIVE_LOCK_FUNCTION(mu) : mu_(mu) {}",
            &["EXCLUSIVE_LOCK_FUNCTION(mu)"],
        );
        expect_blanked(
            blank_cpp_trailing_attribute_macros,
            "bool operator==(const A& a) const ABSL_MUST_USE_RESULT;",
            &["ABSL_MUST_USE_RESULT"],
        );
    }

    #[test]
    fn leaves_statements_casts_and_macro_calls_followed_by_macros_alone() {
        expect_untouched(
            blank_cpp_trailing_attribute_macros,
            &[
                "if (x) RETURN_FALSE;",
                "while (busy) SPIN_PAUSE;",
                "int v = (int) MAX_VALUE;",
                "FMT_PRAGMA_CLANG(diagnostic ignored \"-Wbit-int-extension\")\n\nTEST(std_test, bitint) {\n}",
                "DECLARE_FOO(a) DECLARE_BAR(b);",
                "void f()\n\nBAR_BAZ;",
                "bool b = f(x) == FOO_BAR;",
                // A directive's `)` closes no parameter list.
                "#  if FMT_USE_FCNTL && !defined(__MINGW32__)\nTEST(file_test, open_windows_file) {\n}",
                "if constexpr (POS + 1 == str.size())\n  FMT_THROW(format_error(\"unmatched brace\"));",
            ],
        );
    }

    #[test]
    fn blanks_attribute_macros_after_a_declared_name_or_its_brackets() {
        expect_blanked(
            blank_cpp_declarator_attribute_macros,
            "port::CondVar cv GUARDED_BY(mu);",
            &["GUARDED_BY(mu)"],
        );
        expect_blanked(
            blank_cpp_declarator_attribute_macros,
            "int refs_ GUARDED_BY(refs_mutex_);",
            &["GUARDED_BY(refs_mutex_)"],
        );
        expect_blanked(
            blank_cpp_declarator_attribute_macros,
            "Version* const version GUARDED_BY(mu);",
            &["GUARDED_BY(mu)"],
        );
        expect_blanked(
            blank_cpp_declarator_attribute_macros,
            "std::map<int, int> cache_ ABSL_GUARDED_BY(mutex_) = {};",
            &["ABSL_GUARDED_BY(mutex_)"],
        );
        expect_blanked(
            blank_cpp_declarator_attribute_macros,
            "const AnyGlobalsTypeInternal Any_globals_\n    PROTOBUF_MESSAGE_GLOBALS_SECTION(.data.rel.ro);",
            &["PROTOBUF_MESSAGE_GLOBALS_SECTION(.data.rel.ro)"],
        );
        expect_blanked(
            blank_cpp_declarator_attribute_macros,
            "const ::uint32_t Table::offsets[] ABSL_ATTRIBUTE_SECTION_VARIABLE(\n    protodesc_cold) = {1, 2};",
            &["ABSL_ATTRIBUTE_SECTION_VARIABLE(\n    protodesc_cold)"],
        );
        expect_blanked(
            blank_cpp_declarator_attribute_macros,
            "explicit CordInputStream(const absl::Cord* cord ABSL_ATTRIBUTE_LIFETIME_BOUND);",
            &["ABSL_ATTRIBUTE_LIFETIME_BOUND"],
        );
    }

    #[test]
    fn leaves_variables_named_in_capitals_and_names_without_a_type_before_them_alone() {
        expect_untouched(
            blank_cpp_declarator_attribute_macros,
            &[
                "Foo DEFAULT_OPTIONS;",
                "static Foo DEFAULT_OPTIONS;",
                "const Foo DEFAULT_OPTIONS = Make();",
                "ns::Foo DEFAULT_OPTIONS;",
                "typedef Foo BAR_T;",
                "template <typename T> T MAX_VALUE;",
                "return x FOO_SUFFIX;",
                "f(a, b FOO_FLAG);",
                "x = rate FOO_UNITS;",
                "EXPECT_EQ(2 SECS_X, d);",
            ],
        );
    }

    #[test]
    fn blanks_attribute_macros_that_open_a_declaration() {
        expect_blanked(
            blank_cpp_leading_attribute_macros,
            "  PROTOBUF_FUTURE_ADD_EARLY_NODISCARD absl::string_view name() const { return n; }",
            &["PROTOBUF_FUTURE_ADD_EARLY_NODISCARD"],
        );
        expect_blanked(
            blank_cpp_leading_attribute_macros,
            "  PROTOBUF_FUTURE_ADD_EARLY_NODISCARD static constexpr size_t\n  kMax = 3;",
            &["PROTOBUF_FUTURE_ADD_EARLY_NODISCARD"],
        );
        expect_blanked(
            blank_cpp_leading_attribute_macros,
            "  ABSL_ATTRIBUTE_REINITIALIZES void Clear() final;",
            &["ABSL_ATTRIBUTE_REINITIALIZES"],
        );
        expect_blanked(
            blank_cpp_leading_attribute_macros,
            "  [[nodiscard]] PROTOBUF_NDEBUG_INLINE Ptr<T> Make(Args&&... args) {}",
            &["PROTOBUF_NDEBUG_INLINE"],
        );
        expect_blanked(
            blank_cpp_leading_attribute_macros,
            "GTEST_API_ std::string JoinAsTuple(const Strings& fields);",
            &["GTEST_API_"],
        );
        expect_blanked(
            blank_cpp_leading_attribute_macros,
            "FMT_EXPORT struct as_identifiers_t {};",
            &["FMT_EXPORT"],
        );
        expect_blanked(
            blank_cpp_leading_attribute_macros,
            "PROTOBUF_NDEBUG_INLINE Any::Impl_::Impl_(Arena* arena) : x_(arena) {}",
            &["PROTOBUF_NDEBUG_INLINE"],
        );
        expect_blanked(
            blank_cpp_leading_attribute_macros,
            "PROTOBUF_ATTRIBUTE_NO_DESTROY PROTOBUF_CONSTINIT\n    PROTOBUF_ATTRIBUTE_INIT_PRIORITY1 const AnyGlobals Any_globals_;",
            &[
                "PROTOBUF_ATTRIBUTE_NO_DESTROY PROTOBUF_CONSTINIT\n    PROTOBUF_ATTRIBUTE_INIT_PRIORITY1",
            ],
        );
    }

    #[test]
    fn leaves_a_type_in_capitals_alone() {
        expect_untouched(
            blank_cpp_leading_attribute_macros,
            &[
                "DWORD_PTR value = 0;",
                "SIZE_T const n = 3;",
                "UINT_PTR const* p = q;",
                "HANDLE_T Open(int flags);",
                "UINT_PTR Foo::Bar() { return 0; }",
                "RESULT_T operator==(const A& a);",
                "RESULT_T\nGnuStyle(int x) {}",
                "int x = a |\n    SOME_CONST\n    | OTHER;",
                "// PROTOBUF_FUTURE_ADD_EARLY_NODISCARD absl::string_view name() const;",
                // A macro inside a string concatenation, with a declaration after it.
                "static const char kHelp[] =\n    \"using \" GTEST_NAME_\n    \". You can use the\\n\"\n    \"  --\" GTEST_FLAG_PREFIX_\n    \"list_tests\\n\";\nstatic bool g_help = false;",
            ],
        );
    }

    #[test]
    fn blanks_only_the_macros_of_a_run_not_a_comment_between_them() {
        expect_blanked(
            blank_cpp_leading_attribute_macros,
            "PROTOBUF_ATTRIBUTE_NO_DESTROY /* keep */ PROTOBUF_CONSTINIT const Foo kFoo;",
            &["PROTOBUF_ATTRIBUTE_NO_DESTROY", "PROTOBUF_CONSTINIT"],
        );
    }

    #[test]
    fn blanks_a_lone_macro_line_with_comments_under_it_but_not_an_operand() {
        let fmt = "FMT_BEGIN_EXPORT\n\n// A generic formatting context with custom output iterator and character\n/* (code unit) support. */\ntemplate <typename OutputIt, typename Char> class generic_context {};\n";
        expect_blanked(blank_lone_macro_lines, fmt, &["FMT_BEGIN_EXPORT"]);
        // A string literal continues the expression, comment or not.
        let operand = "const char* s =\n  \"x\"\n  FOO_PREFIX\n  // comment\n  \"y\";\n";
        assert_eq!(blank_lone_macro_lines(operand), operand);
        // An enum member after `A, /* comment */` is still an enum member.
        let enumeration = "typedef enum {\n  CURLE_OK,\n  CURLE_TOO_LARGE, /* 100 */\n  CURL_LAST /* never use! */\n} CURLcode;\n";
        assert_eq!(blank_lone_macro_lines(enumeration), enumeration);
    }

    #[test]
    fn keeps_the_first_branch_of_a_mid_declaration_conditional_offsets_intact() {
        let source = "A() noexcept\n#ifdef X\n  : a(1)\n#else\n  : a(2)\n#endif\n{}\n";
        let out = flatten_mid_statement_conditionals(source);
        assert_eq!(out.len(), source.len());
        assert!(out.contains("  : a(1)"), "{out:?}");
        assert!(!out.contains("a(2)"), "{out:?}");
        assert!(!out.contains('#'), "{out:?}");
        // Between whole declarations a conditional stays.
        let top = "int a;\n#ifdef X\nint b;\n#endif\n";
        assert_eq!(flatten_mid_statement_conditionals(top), top);
    }

    #[test]
    fn rewrites_an_access_specifier_spelled_as_a_macro() {
        let source = "class C {\n  JSON_PRIVATE_UNLESS_TESTED:\n    int x;\n  Q_SIGNALS: // signals\n    void f();\n  FOO_BAR:\n};\n";
        let out = rewrite_cpp_access_macros(source);
        assert_eq!(out.len(), source.len());
        assert!(out.contains("  private                   :\n"), "{out:?}");
        assert!(out.contains("  public   : // signals\n"), "{out:?}");
        // A macro naming no access level, and `A::B` scope, stay.
        assert!(out.contains("  FOO_BAR:\n"), "{out:?}");
        assert_eq!(
            rewrite_cpp_access_macros("x = FOO_PUBLIC::y;\n"),
            "x = FOO_PUBLIC::y;\n"
        );
    }

    #[test]
    fn restores_directive_lines_and_their_continuations() {
        let original = "#define FOO_API \\\n  __attribute__((x))\nFOO_API int f();\n";
        let blanked = "#define         \\\n  __attribute__((x))\n        int f();\n";
        assert_eq!(
            restore_directive_lines(original, blanked),
            "#define FOO_API \\\n  __attribute__((x))\n        int f();\n"
        );
        // A raw string's line is no directive.
        let raw = "auto s = R\"(\n#define X\n)\";\n";
        assert_eq!(
            restore_directive_lines(raw, "auto s = R\"(\n         \n)\";\n"),
            "auto s = R\"(\n         \n)\";\n"
        );
    }

    #[test]
    fn blanks_known_inline_macros_in_specifier_position_only() {
        expect_blanked(
            blank_cpp_inline_macros,
            "FORCEINLINE FString GetName() const;",
            &["FORCEINLINE"],
        );
        expect_untouched(
            blank_cpp_inline_macros,
            &[
                "x = FORCEINLINE ? 1 : 0;",
                "FORCEINLINE_SOMETHINGELSE f();",
                "const char* s = \"FORCEINLINE f\";",
            ],
        );
    }

    #[test]
    fn masked_code_turns_comments_to_spaces_and_literals_to_tokens() {
        let source = "a /* b */ \"c\" // d\n#if X\nR\"(e)\" f";
        let code = masked_code(source);
        assert_eq!(code.len(), source.len());
        // The shared lexer starts a raw string at its quote, so the `R`
        // prefix stays: one uppercase letter, which no macro scan takes.
        assert_eq!(
            String::from_utf8_lossy(&code),
            "a         \u{1}\u{1}\u{1}     \n     \nR\u{1}\u{1}\u{1}\u{1}\u{1} f"
        );
    }
}
