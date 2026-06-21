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
}
