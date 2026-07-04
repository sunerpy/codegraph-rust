//! Per-language comment stripper for the [`FrameworkResolver`] extension point's
//! route extractors.
//!
//! Ports `upstream resolution/strip-comments.ts`. Comment characters
//! and string-literal contents are replaced with spaces (NOT removed) so source
//! byte offsets — and therefore line numbers — are preserved (`strip-comments.ts:1-24`).
//!
//! Consumed by the NestJS [`FrameworkResolver`]'s decorator-over-source scanning
//! ([`crate::frameworks::nestjs`]).
//!
//! [`FrameworkResolver`]: crate::framework::FrameworkResolver

/// Languages whose comment syntax the stripper understands
/// (`CommentLang`, `strip-comments.ts:26-36`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentLang {
    Python,
    JavaScript,
    TypeScript,
    Php,
    Ruby,
    Java,
    CSharp,
    Swift,
    Go,
    Rust,
}

/// Replace comments + string literals with spaces, preserving offsets.
///
/// Ports `stripCommentsForRegex` (`strip-comments.ts:38-59`).
pub fn strip_comments_for_regex(content: &str, lang: CommentLang) -> String {
    match lang {
        CommentLang::Python => strip_python(content),
        CommentLang::Ruby => strip_ruby(content),
        CommentLang::Rust => strip_rust(content),
        CommentLang::Php => strip_php(content),
        CommentLang::Go => strip_go(content),
        // allowSingleQuoteStrings only for JS/TS (strip-comments.ts:55).
        CommentLang::JavaScript | CommentLang::TypeScript => strip_c_style(content, true),
        CommentLang::Java | CommentLang::CSharp | CommentLang::Swift => {
            strip_c_style(content, false)
        }
    }
}

/// Replace `[start, end)` with spaces, keeping newlines so line numbers stay
/// valid (`blankRange`, `strip-comments.ts:65-69`).
fn blank_range(buf: &mut [char], src: &[char], start: usize, end: usize) {
    for (i, slot) in buf.iter_mut().enumerate().take(end).skip(start) {
        *slot = if src[i] == '\n' { '\n' } else { ' ' };
    }
}

/// `stripPython` (`strip-comments.ts:73-131`).
fn strip_python(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = chars.clone();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        let c2 = chars.get(i + 1).copied().unwrap_or('\0');
        let c3 = chars.get(i + 2).copied().unwrap_or('\0');

        // Triple-quoted string.
        if (c == '"' || c == '\'') && c2 == c && c3 == c {
            let quote = c;
            let start = i;
            i += 3;
            while i < n {
                if chars[i] == '\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if chars[i] == quote
                    && chars.get(i + 1).copied() == Some(quote)
                    && chars.get(i + 2).copied() == Some(quote)
                {
                    i += 3;
                    break;
                }
                i += 1;
            }
            blank_range(&mut out, &chars, start, i);
            continue;
        }

        // Single-line string.
        if c == '"' || c == '\'' {
            let quote = c;
            i += 1;
            while i < n && chars[i] != quote {
                if chars[i] == '\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if chars[i] == '\n' {
                    break;
                }
                i += 1;
            }
            if i < n && chars[i] == quote {
                i += 1;
            }
            continue;
        }

        // Line comment.
        if c == '#' {
            let start = i;
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            blank_range(&mut out, &chars, start, i);
            continue;
        }

        i += 1;
    }
    out.into_iter().collect()
}

/// `stripRuby` (`strip-comments.ts:135-209`).
fn strip_ruby(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = chars.clone();
    let n = chars.len();
    let mut i = 0;
    let mut at_line_start = true;
    while i < n {
        let c = chars[i];

        // =begin / =end block comments at start of line.
        if at_line_start && c == '=' && starts_with(&chars, i, "=begin") {
            let start = i;
            i += "=begin".len();
            while i < n {
                if chars[i] == '\n' {
                    let mut j = i + 1;
                    while j < n && (chars[j] == ' ' || chars[j] == '\t') {
                        j += 1;
                    }
                    if starts_with(&chars, j, "=end") {
                        i = j + "=end".len();
                        while i < n && chars[i] != '\n' {
                            i += 1;
                        }
                        break;
                    }
                }
                i += 1;
            }
            blank_range(&mut out, &chars, start, i);
            at_line_start = i > 0 && chars[i - 1] == '\n';
            continue;
        }

        // String literals.
        if c == '"' || c == '\'' {
            let quote = c;
            i += 1;
            while i < n && chars[i] != quote {
                if chars[i] == '\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if chars[i] == '\n' {
                    break;
                }
                i += 1;
            }
            if i < n && chars[i] == quote {
                i += 1;
            }
            at_line_start = false;
            continue;
        }

        // Line comment.
        if c == '#' {
            let start = i;
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            blank_range(&mut out, &chars, start, i);
            at_line_start = false;
            continue;
        }

        if c == '\n' {
            at_line_start = true;
            i += 1;
            continue;
        }
        if c == ' ' || c == '\t' {
            i += 1;
            continue;
        }
        at_line_start = false;
        i += 1;
    }
    out.into_iter().collect()
}

/// `stripCStyle` (`strip-comments.ts:213-261`).
fn strip_c_style(src: &str, allow_single_quote_strings: bool) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = chars.clone();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        let c2 = chars.get(i + 1).copied().unwrap_or('\0');

        // Block comment.
        if c == '/' && c2 == '*' {
            let start = i;
            i += 2;
            while i < n && !(chars[i] == '*' && chars.get(i + 1).copied() == Some('/')) {
                i += 1;
            }
            if i < n {
                i += 2;
            }
            blank_range(&mut out, &chars, start, i);
            continue;
        }

        // Line comment.
        if c == '/' && c2 == '/' {
            let start = i;
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            blank_range(&mut out, &chars, start, i);
            continue;
        }

        // String literals.
        if c == '"' || (allow_single_quote_strings && c == '\'') || c == '`' {
            let quote = c;
            i += 1;
            while i < n && chars[i] != quote {
                if chars[i] == '\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if quote != '`' && chars[i] == '\n' {
                    break;
                }
                i += 1;
            }
            if i < n && chars[i] == quote {
                i += 1;
            }
            continue;
        }

        i += 1;
    }
    out.into_iter().collect()
}

/// `stripPhp` (`strip-comments.ts:265-321`).
fn strip_php(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = chars.clone();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        let c2 = chars.get(i + 1).copied().unwrap_or('\0');

        if c == '/' && c2 == '*' {
            let start = i;
            i += 2;
            while i < n && !(chars[i] == '*' && chars.get(i + 1).copied() == Some('/')) {
                i += 1;
            }
            if i < n {
                i += 2;
            }
            blank_range(&mut out, &chars, start, i);
            continue;
        }

        if c == '/' && c2 == '/' {
            let start = i;
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            blank_range(&mut out, &chars, start, i);
            continue;
        }

        if c == '#' {
            let start = i;
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            blank_range(&mut out, &chars, start, i);
            continue;
        }

        if c == '"' || c == '\'' || c == '`' {
            let quote = c;
            i += 1;
            while i < n && chars[i] != quote {
                if chars[i] == '\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if chars[i] == '\n' {
                    break;
                }
                i += 1;
            }
            if i < n && chars[i] == quote {
                i += 1;
            }
            continue;
        }

        i += 1;
    }
    out.into_iter().collect()
}

/// `stripGo` (`strip-comments.ts:325-394`).
fn strip_go(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = chars.clone();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        let c2 = chars.get(i + 1).copied().unwrap_or('\0');

        if c == '/' && c2 == '*' {
            let start = i;
            i += 2;
            while i < n && !(chars[i] == '*' && chars.get(i + 1).copied() == Some('/')) {
                i += 1;
            }
            if i < n {
                i += 2;
            }
            blank_range(&mut out, &chars, start, i);
            continue;
        }

        if c == '/' && c2 == '/' {
            let start = i;
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            blank_range(&mut out, &chars, start, i);
            continue;
        }

        // Raw string with backticks — keep contents (the upstream does not blank them).
        if c == '`' {
            i += 1;
            while i < n && chars[i] != '`' {
                i += 1;
            }
            if i < n {
                i += 1;
            }
            continue;
        }

        if c == '"' {
            i += 1;
            while i < n && chars[i] != '"' {
                if chars[i] == '\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if chars[i] == '\n' {
                    break;
                }
                i += 1;
            }
            if i < n && chars[i] == '"' {
                i += 1;
            }
            continue;
        }

        if c == '\'' {
            i += 1;
            while i < n && chars[i] != '\'' {
                if chars[i] == '\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if chars[i] == '\n' {
                    break;
                }
                i += 1;
            }
            if i < n && chars[i] == '\'' {
                i += 1;
            }
            continue;
        }

        i += 1;
    }
    out.into_iter().collect()
}

/// `stripRust` (`strip-comments.ts:398-469`) — nested block comments.
fn strip_rust(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = chars.clone();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        let c2 = chars.get(i + 1).copied().unwrap_or('\0');

        // Nested block comment.
        if c == '/' && c2 == '*' {
            let start = i;
            i += 2;
            let mut depth = 1;
            while i < n && depth > 0 {
                if chars[i] == '/' && chars.get(i + 1).copied() == Some('*') {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1).copied() == Some('/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            blank_range(&mut out, &chars, start, i);
            continue;
        }

        if c == '/' && c2 == '/' {
            let start = i;
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            blank_range(&mut out, &chars, start, i);
            continue;
        }

        if c == '"' {
            i += 1;
            while i < n && chars[i] != '"' {
                if chars[i] == '\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                i += 1;
            }
            if i < n && chars[i] == '"' {
                i += 1;
            }
            continue;
        }

        if c == '\'' {
            i += 1;
            while i < n && chars[i] != '\'' {
                if chars[i] == '\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if chars[i] == '\n' {
                    break;
                }
                i += 1;
            }
            if i < n && chars[i] == '\'' {
                i += 1;
            }
            continue;
        }

        i += 1;
    }
    out.into_iter().collect()
}

fn starts_with(chars: &[char], at: usize, needle: &str) -> bool {
    let nch: Vec<char> = needle.chars().collect();
    if at + nch.len() > chars.len() {
        return false;
    }
    chars[at..at + nch.len()] == nch[..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_blanks_comment_preserves_offsets() {
        // strip-comments.ts:9-11 worked example.
        let input = "x = 1  # path('/fake/', V)\n real = 2";
        let out = strip_comments_for_regex(input, CommentLang::Python);
        assert_eq!(out, "x = 1                     \n real = 2");
        // Newlines preserved → same number of lines.
        assert_eq!(out.lines().count(), input.lines().count());
    }

    #[test]
    fn rust_strips_nested_block_comments() {
        let input = "let a = 1; /* outer /* inner */ still */ let b = 2;";
        let out = strip_comments_for_regex(input, CommentLang::Rust);
        assert!(out.starts_with("let a = 1; "));
        assert!(out.trim_end().ends_with("let b = 2;"));
        assert!(!out.contains("inner"));
    }

    /// The blanking always keeps the same length and preserves `\n` positions,
    /// so line counts and byte offsets are stable across every language.
    fn assert_offsets_preserved(input: &str, out: &str) {
        assert_eq!(out.chars().count(), input.chars().count());
        let in_nl: Vec<usize> = input
            .char_indices()
            .filter(|(_, c)| *c == '\n')
            .map(|(i, _)| i)
            .collect();
        let out_nl: Vec<usize> = out
            .char_indices()
            .filter(|(_, c)| *c == '\n')
            .map(|(i, _)| i)
            .collect();
        assert_eq!(in_nl, out_nl);
    }

    #[test]
    fn python_triple_quoted_string_blanked() {
        let input = "a = 1\nx = \"\"\"multi\nline # not comment\n\"\"\"\nb = 2";
        let out = strip_comments_for_regex(input, CommentLang::Python);
        assert_offsets_preserved(input, &out);
        assert!(out.starts_with("a = 1\n"));
        assert!(out.trim_end().ends_with("b = 2"));
        assert!(!out.contains("multi"));
        assert!(!out.contains("not comment"));
        assert_eq!(out.lines().count(), input.lines().count());
    }

    #[test]
    fn python_triple_quoted_single_quotes_and_escapes() {
        let input = "x = '''a\\'''b'''c";
        let out = strip_comments_for_regex(input, CommentLang::Python);
        assert_offsets_preserved(input, &out);
        assert!(out.ends_with('c'));
    }

    #[test]
    fn python_single_line_string_with_hash_inside() {
        let input = "url = \"http://x#frag\"  # real comment";
        let out = strip_comments_for_regex(input, CommentLang::Python);
        assert_offsets_preserved(input, &out);
        // The `#frag` inside the string is not treated as a comment; the string
        // itself is kept intact (single-line strings are NOT blanked in python).
        assert!(out.contains("http://x#frag"));
        // The trailing real comment IS blanked.
        assert!(!out.contains("real comment"));
    }

    #[test]
    fn python_string_escape_and_unterminated() {
        let input = "s = \"esc\\\"still\ncode = 1";
        let out = strip_comments_for_regex(input, CommentLang::Python);
        assert_offsets_preserved(input, &out);
        assert!(out.contains("code = 1"));
    }

    #[test]
    fn python_single_quote_string_kept() {
        let input = "a = 'literal' # c";
        let out = strip_comments_for_regex(input, CommentLang::Python);
        assert!(out.contains("'literal'"));
        assert!(!out.contains("# c") || out.contains("   "));
    }

    #[test]
    fn ruby_begin_end_block_comment_blanked() {
        let input = "x = 1\n=begin\nthis is\na comment\n=end\ny = 2";
        let out = strip_comments_for_regex(input, CommentLang::Ruby);
        assert_offsets_preserved(input, &out);
        assert!(out.starts_with("x = 1\n"));
        assert!(out.trim_end().ends_with("y = 2"));
        assert!(!out.contains("this is"));
        assert!(!out.contains("comment"));
    }

    #[test]
    fn ruby_begin_end_indented_end() {
        let input = "=begin\ndoc\n  =end\nreal = 3";
        let out = strip_comments_for_regex(input, CommentLang::Ruby);
        assert_offsets_preserved(input, &out);
        assert!(out.contains("real = 3"));
        assert!(!out.contains("doc"));
    }

    #[test]
    fn ruby_line_comment_and_string() {
        let input = "name = \"foo#bar\" # trailing";
        let out = strip_comments_for_regex(input, CommentLang::Ruby);
        assert_offsets_preserved(input, &out);
        assert!(out.contains("\"foo#bar\""));
        assert!(!out.contains("trailing"));
    }

    #[test]
    fn ruby_string_escape_and_newline_break() {
        let input = "a = 'x\\'y'\nb = \"unterminated\nc = 1";
        let out = strip_comments_for_regex(input, CommentLang::Ruby);
        assert_offsets_preserved(input, &out);
        assert!(out.contains("c = 1"));
    }

    #[test]
    fn ruby_whitespace_before_hash_not_line_start() {
        // A `#` not at line start after code is still a line comment.
        let input = "  code # note";
        let out = strip_comments_for_regex(input, CommentLang::Ruby);
        assert!(out.contains("code"));
        assert!(!out.contains("note"));
    }

    #[test]
    fn js_block_and_line_comments() {
        let input = "let a = 1; /* block */ let b = 2; // line\nlet c = 3;";
        let out = strip_comments_for_regex(input, CommentLang::JavaScript);
        assert_offsets_preserved(input, &out);
        assert!(!out.contains("block"));
        assert!(!out.contains("line"));
        assert!(out.contains("let c = 3;"));
    }

    #[test]
    fn js_template_literal_multiline_skipped_not_treated_as_comment() {
        let input = "const t = `line1\nline2 // not comment`;\nx = 1";
        let out = strip_comments_for_regex(input, CommentLang::TypeScript);
        assert_offsets_preserved(input, &out);
        // String literals are SKIPPED (advanced past) but not blanked, so the
        // template body survives; the point is the inner `//` is not a comment.
        assert!(out.contains("not comment"));
        assert!(out.contains("x = 1"));
    }

    #[test]
    fn js_single_quote_allowed() {
        let input = "let s = 'a//b'; // c";
        let out = strip_comments_for_regex(input, CommentLang::JavaScript);
        assert_offsets_preserved(input, &out);
        assert!(!out.contains("c"));
    }

    #[test]
    fn java_single_quote_not_a_string() {
        // For Java, single quotes are NOT string delimiters (char literal path
        // is not enabled), so a `'` is a plain char in the scan.
        let input = "char c = 'x'; // note";
        let out = strip_comments_for_regex(input, CommentLang::Java);
        assert_offsets_preserved(input, &out);
        assert!(!out.contains("note"));
    }

    #[test]
    fn c_style_unterminated_block_comment() {
        let input = "code /* never closed";
        let out = strip_comments_for_regex(input, CommentLang::CSharp);
        assert_offsets_preserved(input, &out);
        assert!(out.starts_with("code "));
        assert!(!out.contains("never"));
    }

    #[test]
    fn c_style_string_escape_and_double_quote() {
        let input = "let s = \"esc\\\"quote\"; // trailing";
        let out = strip_comments_for_regex(input, CommentLang::Swift);
        assert_offsets_preserved(input, &out);
        assert!(!out.contains("trailing"));
    }

    #[test]
    fn php_all_comment_styles() {
        let input = "<?php\n$a = 1; // slash\n$b = 2; # hash\n/* block */ $c = 3;";
        let out = strip_comments_for_regex(input, CommentLang::Php);
        assert_offsets_preserved(input, &out);
        assert!(!out.contains("slash"));
        assert!(!out.contains("hash"));
        assert!(!out.contains("block"));
        assert!(out.contains("$c = 3;"));
    }

    #[test]
    fn php_string_and_backtick() {
        let input = "$s = \"a#b\"; $t = `cmd`; # c";
        let out = strip_comments_for_regex(input, CommentLang::Php);
        assert_offsets_preserved(input, &out);
        assert!(out.contains("\"a#b\""));
        assert!(!out.contains("# c") || out.trim_end().ends_with("`;"));
    }

    #[test]
    fn php_unterminated_block_and_escape() {
        let input = "$x = \"esc\\\"y\";\n/* open";
        let out = strip_comments_for_regex(input, CommentLang::Php);
        assert_offsets_preserved(input, &out);
        assert!(!out.contains("open"));
    }

    #[test]
    fn php_string_newline_break() {
        let input = "$s = 'unterminated\n$y = 1";
        let out = strip_comments_for_regex(input, CommentLang::Php);
        assert_offsets_preserved(input, &out);
        assert!(out.contains("$y = 1"));
    }

    #[test]
    fn go_raw_string_backtick_kept() {
        // Go raw strings (backticks) are NOT blanked — contents preserved.
        let input = "s := `raw // not comment`\nx := 1";
        let out = strip_comments_for_regex(input, CommentLang::Go);
        assert_offsets_preserved(input, &out);
        assert!(out.contains("raw // not comment"));
        assert!(out.contains("x := 1"));
    }

    #[test]
    fn go_block_line_and_double_string() {
        let input = "/* b */ s := \"a//b\" // trailing\nx := 1";
        let out = strip_comments_for_regex(input, CommentLang::Go);
        assert_offsets_preserved(input, &out);
        assert!(!out.contains(" b "));
        assert!(!out.contains("trailing"));
        assert!(out.contains("x := 1"));
    }

    #[test]
    fn go_rune_single_quote_literal() {
        let input = "r := 'x' // note";
        let out = strip_comments_for_regex(input, CommentLang::Go);
        assert_offsets_preserved(input, &out);
        assert!(!out.contains("note"));
    }

    #[test]
    fn go_string_escapes_and_unterminated() {
        let input = "s := \"esc\\\"y\"\nt := 'a\\'b'\nu := \"open\nv := 1";
        let out = strip_comments_for_regex(input, CommentLang::Go);
        assert_offsets_preserved(input, &out);
        assert!(out.contains("v := 1"));
    }

    #[test]
    fn go_unterminated_backtick_and_rune() {
        let input = "s := `open\nr := 'z";
        let out = strip_comments_for_regex(input, CommentLang::Go);
        assert_offsets_preserved(input, &out);
    }

    #[test]
    fn rust_line_comment_and_string() {
        let input = "let s = \"a//b\"; // trailing\nlet x = 1;";
        let out = strip_comments_for_regex(input, CommentLang::Rust);
        assert_offsets_preserved(input, &out);
        assert!(out.contains("\"a//b\""));
        assert!(!out.contains("trailing"));
        assert!(out.contains("let x = 1;"));
    }

    #[test]
    fn rust_char_literal_and_escape() {
        let input = "let c = 'x'; let s = \"esc\\\"y\"; // note";
        let out = strip_comments_for_regex(input, CommentLang::Rust);
        assert_offsets_preserved(input, &out);
        assert!(!out.contains("note"));
    }

    #[test]
    fn rust_unterminated_block_and_string() {
        let input = "code /* open /* nested\nlet s = \"open";
        let out = strip_comments_for_regex(input, CommentLang::Rust);
        assert_offsets_preserved(input, &out);
        assert!(out.starts_with("code "));
    }

    #[test]
    fn rust_char_newline_break() {
        let input = "let c = 'a\nlet x = 1;";
        let out = strip_comments_for_regex(input, CommentLang::Rust);
        assert_offsets_preserved(input, &out);
        assert!(out.contains("let x = 1;"));
    }

    #[test]
    fn empty_input_all_langs() {
        for lang in [
            CommentLang::Python,
            CommentLang::Ruby,
            CommentLang::JavaScript,
            CommentLang::TypeScript,
            CommentLang::Php,
            CommentLang::Java,
            CommentLang::CSharp,
            CommentLang::Swift,
            CommentLang::Go,
            CommentLang::Rust,
        ] {
            assert_eq!(strip_comments_for_regex("", lang), "");
        }
    }

    #[test]
    fn comment_lang_derives() {
        let a = CommentLang::Rust;
        let b = a;
        assert_eq!(a, b);
        assert_ne!(CommentLang::Go, CommentLang::Rust);
        assert!(format!("{a:?}").contains("Rust"));
    }
}
