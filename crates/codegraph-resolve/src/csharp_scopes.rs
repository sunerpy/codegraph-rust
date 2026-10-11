//! The namespace blocks and `using` directives of a C# file, read from its
//! source by range (W3-05c).
//!
//! The C# extraction files every symbol of a file under its first root
//! namespace (`Nested.cs`'s `Outer.Inner` and sibling `Second` types are all
//! `Outer::…` until W4-CS-01), so whether an extension method is in scope at a
//! call is read here instead: comments, strings, characters and the inactive
//! branches of `#if` blocks are blanked, then `namespace X { … }` blocks are
//! paired by their braces (a nested block's name joins its parents':
//! `Outer.Inner`) and a file-scoped `namespace X;` covers the rest of the
//! file. Each `using X;` belongs to the namespace block it is written in, or
//! to the whole file outside any; `using static`, an alias `using A = X;` and
//! a `using` statement in a body import no namespace.

/// One file's namespace blocks and `using` directives.
#[derive(Debug, Default)]
pub(crate) struct CsharpScopes {
    /// Namespace blocks: the byte range of the body braces and the full name.
    blocks: Vec<Block>,
    /// A file-scoped namespace: where it is declared and its name.
    file_namespace: Option<(usize, String)>,
    /// `using X;` directives: the namespace named and the block holding it
    /// (`None` for file level, a `global using` included).
    usings: Vec<(String, Option<usize>)>,
}

#[derive(Debug)]
struct Block {
    start: usize,
    end: usize,
    name: String,
}

impl CsharpScopes {
    /// Read `source`'s namespace blocks and `using` directives.
    pub(crate) fn read(source: &str) -> Self {
        let code = blank_non_code(source);
        let tokens = tokenize(&code);
        let mut scopes = Self::default();
        // Open braces: `Some(block)` for a namespace body, `None` otherwise.
        let mut open: Vec<Option<usize>> = Vec::new();
        let mut i = 0;
        while i < tokens.len() {
            let at_namespace_level = open.last().is_none_or(Option::is_some);
            let enclosing = open.last().copied().flatten();
            match tokens[i].text {
                "{" => open.push(None),
                "}" => {
                    if let Some(Some(block)) = open.pop() {
                        scopes.blocks[block].end = tokens[i].at;
                    }
                }
                "namespace" if at_namespace_level => {
                    let (name, next) = qualified_name(&tokens, i + 1);
                    match tokens.get(next).map(|t| t.text) {
                        Some("{") if !name.is_empty() => {
                            let full = match enclosing {
                                Some(parent) => format!("{}.{name}", scopes.blocks[parent].name),
                                None => match &scopes.file_namespace {
                                    Some((_, file)) => format!("{file}.{name}"),
                                    None => name,
                                },
                            };
                            scopes.blocks.push(Block {
                                start: tokens[next].at,
                                end: code.len(),
                                name: full,
                            });
                            open.push(Some(scopes.blocks.len() - 1));
                            i = next + 1;
                            continue;
                        }
                        Some(";") if !name.is_empty() && enclosing.is_none() => {
                            scopes.file_namespace = Some((tokens[i].at, name));
                            i = next + 1;
                            continue;
                        }
                        _ => {}
                    }
                }
                "using" if at_namespace_level => {
                    let global = i > 0 && tokens[i - 1].text == "global";
                    let (name, next) = qualified_name(&tokens, i + 1);
                    if !name.is_empty()
                        && name != "static"
                        && tokens.get(next).map(|t| t.text) == Some(";")
                    {
                        let name = name.strip_prefix("global::").unwrap_or(&name).to_string();
                        scopes
                            .usings
                            .push((name, if global { None } else { enclosing }));
                        i = next + 1;
                        continue;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        scopes
    }

    /// The namespace the code at byte `offset` is in (`""` for the global
    /// namespace).
    pub(crate) fn namespace_at(&self, offset: usize) -> String {
        self.innermost(offset)
            .map(|block| self.blocks[block].name.clone())
            .or_else(|| {
                self.file_namespace
                    .as_ref()
                    .filter(|(at, _)| *at <= offset)
                    .map(|(_, name)| name.clone())
            })
            .unwrap_or_default()
    }

    /// Whether a `using namespace;` is in scope at byte `offset`: one at
    /// file level, or one in a namespace block around `offset`.
    pub(crate) fn imports_at(&self, offset: usize, namespace: &str) -> bool {
        self.usings.iter().any(|(name, block)| {
            name == namespace
                && block.is_none_or(|block| {
                    let block = &self.blocks[block];
                    block.start <= offset && offset <= block.end
                })
        })
    }

    fn innermost(&self, offset: usize) -> Option<usize> {
        self.blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| block.start <= offset && offset <= block.end)
            .min_by_key(|(_, block)| block.end - block.start)
            .map(|(index, _)| index)
    }
}

#[derive(Debug)]
struct Token<'a> {
    at: usize,
    text: &'a str,
}

/// Identifiers (with a verbatim `@`) and single punctuation characters.
fn tokenize(code: &str) -> Vec<Token<'_>> {
    let bytes = code.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
        } else if b.is_ascii_alphanumeric() || b == b'_' || b == b'@' || b >= 0x80 {
            let start = i;
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] >= 0x80)
            {
                i += 1;
            }
            tokens.push(Token {
                at: start,
                text: &code[start..i],
            });
        } else {
            tokens.push(Token {
                at: i,
                text: &code[i..=i],
            });
            i += 1;
        }
    }
    tokens
}

/// The dotted (or `global::`-prefixed) name starting at token `i`, and the
/// index of the token after it.
fn qualified_name(tokens: &[Token<'_>], mut i: usize) -> (String, usize) {
    let mut name = String::new();
    if tokens.get(i).map(|t| t.text) == Some("global")
        && tokens.get(i + 1).map(|t| t.text) == Some(":")
        && tokens.get(i + 2).map(|t| t.text) == Some(":")
    {
        name.push_str("global::");
        i += 3;
    }
    while let Some(token) = tokens.get(i) {
        if !token
            .text
            .starts_with(|c: char| c.is_alphanumeric() || c == '_' || c == '@')
        {
            break;
        }
        name.push_str(token.text.trim_start_matches('@'));
        i += 1;
        if tokens.get(i).map(|t| t.text) == Some(".") {
            name.push('.');
            i += 1;
        } else {
            break;
        }
    }
    (name, i)
}

/// `source` with every comment, string and character literal, preprocessor
/// line and inactive `#elif` / `#else` branch replaced by spaces (newlines
/// kept), so only code is left at the same offsets.
fn blank_non_code(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = bytes.to_vec();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for byte in &mut out[from..to.min(bytes.len())] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    let mut i = 0;
    // `#if` nesting, and the depth whose inactive branch is being skipped.
    let mut if_depth = 0usize;
    let mut skipping: Option<usize> = None;
    let mut line_start = true;
    while i < bytes.len() {
        let b = bytes[i];
        if line_start && (b == b' ' || b == b'\t') {
            i += 1;
            continue;
        }
        if line_start && b == b'#' {
            let end = line_end(bytes, i);
            let directive = source[i + 1..end].trim_start();
            let word: String = directive
                .chars()
                .take_while(char::is_ascii_alphabetic)
                .collect();
            match word.as_str() {
                "if" => if_depth += 1,
                "elif" | "else" => {
                    if skipping.is_none() {
                        skipping = Some(if_depth);
                    }
                }
                "endif" => {
                    if skipping == Some(if_depth) {
                        skipping = None;
                    }
                    if_depth = if_depth.saturating_sub(1);
                }
                _ => {}
            }
            blank(&mut out, i, end);
            i = end;
            continue;
        }
        line_start = false;
        if b == b'\n' {
            line_start = true;
            i += 1;
            continue;
        }
        if skipping.is_some() {
            let end = line_end(bytes, i);
            blank(&mut out, i, end);
            i = end;
            continue;
        }
        let end = match b {
            b'/' if bytes.get(i + 1) == Some(&b'/') => line_end(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'*') => source[i + 2..]
                .find("*/")
                .map_or(bytes.len(), |at| i + 2 + at + 2),
            b'\'' => char_literal_end(bytes, i),
            b'"' | b'@' | b'$' => match string_end(bytes, i) {
                Some(end) => end,
                None => {
                    i += 1;
                    continue;
                }
            },
            _ => {
                i += 1;
                continue;
            }
        };
        blank(&mut out, i, end);
        i = end;
    }
    String::from_utf8(out).unwrap_or_default()
}

fn line_end(bytes: &[u8], from: usize) -> usize {
    bytes[from..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(bytes.len(), |at| from + at)
}

/// The end of the character literal opening at `start`.
fn char_literal_end(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < bytes.len() && bytes[i] != b'\'' && bytes[i] != b'\n' {
        i += if bytes[i] == b'\\' { 2 } else { 1 };
    }
    (i + 1).min(bytes.len())
}

/// The end of the string literal starting at `start` (`"…"`, `@"…"`,
/// `$"…"`, `$@"…"`, `@$"…"`, `"""…"""`), or `None` when no string starts
/// there. Interpolation holes are skipped as code, strings in them included.
fn string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    let mut verbatim = false;
    let mut interpolated = 0usize;
    while i < bytes.len() && (bytes[i] == b'@' || bytes[i] == b'$') {
        if bytes[i] == b'@' {
            verbatim = true;
        } else {
            interpolated += 1;
        }
        i += 1;
    }
    if bytes.get(i) != Some(&b'"') {
        return None;
    }
    let quotes = bytes[i..].iter().take_while(|&&b| b == b'"').count();
    if quotes >= 3 {
        // A raw string: closed by as many quotes as opened it.
        let mut j = i + quotes;
        while j < bytes.len() {
            if bytes[j] == b'"' && bytes[j..].iter().take_while(|&&b| b == b'"').count() >= quotes {
                return Some(j + quotes);
            }
            j += 1;
        }
        return Some(bytes.len());
    }
    let mut j = i + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' if !verbatim => j += 2,
            b'"' if verbatim && bytes.get(j + 1) == Some(&b'"') => j += 2,
            b'"' => return Some(j + 1),
            b'\n' if !verbatim => return Some(j),
            b'{' if interpolated > 0 && bytes.get(j + 1) == Some(&b'{') => j += 2,
            b'{' if interpolated > 0 => j = hole_end(bytes, j + 1),
            _ => j += 1,
        }
    }
    Some(bytes.len())
}

/// The index after the `}` closing an interpolation hole whose code starts at
/// `from`.
fn hole_end(bytes: &[u8], from: usize) -> usize {
    let mut depth = 1usize;
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            b'\'' => {
                i = char_literal_end(bytes, i);
                continue;
            }
            b'"' | b'@' | b'$' => {
                if let Some(end) = string_end(bytes, i) {
                    i = end;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::CsharpScopes;

    fn at(source: &str, needle: &str) -> usize {
        source.find(needle).expect("needle in source")
    }

    #[test]
    fn nested_and_sibling_blocks_and_their_usings() {
        let source = "using System;\nnamespace Outer\n{\n    using Outer.Extras;\n    namespace Inner\n    {\n        class Deep { void M() { } }\n    }\n    class Mid { }\n}\nnamespace Second\n{\n    using Local.Only;\n    class Two { }\n}\nnamespace Third\n{\n    class Three { }\n}\n";
        let scopes = CsharpScopes::read(source);
        assert_eq!(scopes.namespace_at(at(source, "Deep")), "Outer.Inner");
        assert_eq!(scopes.namespace_at(at(source, "Mid")), "Outer");
        assert_eq!(scopes.namespace_at(at(source, "Two")), "Second");
        assert_eq!(scopes.namespace_at(at(source, "Three")), "Third");
        assert_eq!(scopes.namespace_at(0), "");
        assert!(scopes.imports_at(at(source, "Three"), "System"));
        assert!(scopes.imports_at(at(source, "Deep"), "Outer.Extras"));
        assert!(scopes.imports_at(at(source, "Two"), "Local.Only"));
        assert!(!scopes.imports_at(at(source, "Three"), "Local.Only"));
        assert!(!scopes.imports_at(at(source, "Mid"), "Local.Only"));
    }

    #[test]
    fn a_file_scoped_namespace_covers_the_rest_of_the_file() {
        let source = "using System.Reflection;\nnamespace App.Utilities;\nusing App.Helpers;\npublic static class TypeExtensions { }\n";
        let scopes = CsharpScopes::read(source);
        assert_eq!(scopes.namespace_at(at(source, "public")), "App.Utilities");
        assert!(scopes.imports_at(at(source, "public"), "System.Reflection"));
        assert!(scopes.imports_at(at(source, "public"), "App.Helpers"));
    }

    #[test]
    fn static_alias_and_statement_usings_import_no_namespace() {
        let source = "using static System.Math;\nusing Alias = System.Text;\nglobal using System.Linq;\nnamespace App\n{\n    class C\n    {\n        void M() { using (var s = Open()) { } using var t = Open(); }\n    }\n}\n";
        let scopes = CsharpScopes::read(source);
        let inside = at(source, "void M");
        assert!(!scopes.imports_at(inside, "System.Math"));
        assert!(!scopes.imports_at(inside, "System.Text"));
        assert!(!scopes.imports_at(inside, "static"));
        assert!(scopes.imports_at(inside, "System.Linq"));
        assert_eq!(scopes.namespace_at(inside), "App");
    }

    #[test]
    fn braces_in_strings_comments_characters_and_inactive_branches_do_not_count() {
        let source = "namespace A\n{\n    class C\n    {\n        string a = \"{\";\n        string b = @\"C:\\path\\\"\"{\";\n        string c = $\"{(x ? \"}\" : \"{\")}{{\";\n        string d = \"\"\"\n        { raw }}}\n        \"\"\";\n        char e = '{';\n        // }\n        /* } */\n#if NET\n        void F() {\n#else\n        void F(int x) {\n#endif\n        }\n    }\n}\nnamespace B\n{\n    class D { }\n}\n";
        let scopes = CsharpScopes::read(source);
        assert_eq!(scopes.namespace_at(at(source, "class C")), "A");
        assert_eq!(scopes.namespace_at(at(source, "class D")), "B");
    }
}
