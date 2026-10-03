//! Lexical facts about Rust source that resolution reads without a parser.
//!
//! [`rust_code_mask`] tells code bytes from comment and literal bytes. It was
//! written for the Tauri resolver, which still uses it to find
//! `#[tauri::command]` attributes, and it backs [`RustUseScopes`], which records
//! where each `use` item is visible so a bare enum variant can be checked
//! against the imports actually in scope (KEEP-RUST, P15).

/// Byte mask over `src`: `true` where the byte is ordinary Rust code.
///
/// One forward scan tracking the states a textual attribute match can hide in,
/// each of which was measured to fabricate an edge (or, for the last one, to lose
/// a real command) when unhandled:
///
/// * line comments, including `///` and `//!`;
/// * block comments, **nestable** — Rust nests, unlike C++, so a C++-shaped mask
///   closes at the first `*/` and reads the trailing bytes as code;
/// * raw strings `r"…"` / `r#"…"#` and their byte forms `br##"…"##`, with the
///   HASH COUNT captured so a custom delimiter still closes correctly;
/// * normal and byte strings, `"…"` / `b"…"`, with `\` escapes;
/// * char literals with `\` escapes (`'\''`) that DECLINE to open on a lifetime
///   `'a` — a mask that treats every `'` as a delimiter masks from the lifetime
///   forward and can swallow a genuine attribute, which is a silent feature death
///   rather than a fabrication.
pub(crate) fn rust_code_mask(src: &str) -> Vec<bool> {
    let bytes = src.as_bytes();
    let mut mask = vec![true; bytes.len()];
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                mask[i] = false;
                i += 1;
            }
            continue;
        }
        if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i = mask_nested_block_comment(bytes, &mut mask, i);
            continue;
        }
        if is_ident_start(b) {
            if let Some((quote, hashes)) = raw_string_prefix(bytes, i) {
                mask[i..quote].fill(false);
                i = mask_raw_string(bytes, &mut mask, quote, hashes);
                continue;
            }
            let (_, after) = read_ident(bytes, i).expect("ident start");
            i = after;
            continue;
        }
        if b == b'"' {
            i = mask_quoted(bytes, &mut mask, i, b'"', false);
            continue;
        }
        if b == b'\'' {
            if char_literal_end(bytes, i).is_some() {
                i = mask_quoted(bytes, &mut mask, i, b'\'', true);
            } else {
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    mask
}

/// Mask a nestable block comment starting at `i` (`/*`). Returns the index just
/// past the outermost `*/`.
fn mask_nested_block_comment(bytes: &[u8], mask: &mut [bool], i: usize) -> usize {
    let mut depth = 0usize;
    let mut k = i;
    while k < bytes.len() {
        if bytes[k] == b'/' && bytes.get(k + 1) == Some(&b'*') {
            depth += 1;
            mask[k] = false;
            mask[k + 1] = false;
            k += 2;
            continue;
        }
        if bytes[k] == b'*' && bytes.get(k + 1) == Some(&b'/') {
            mask[k] = false;
            mask[k + 1] = false;
            k += 2;
            depth -= 1;
            if depth == 0 {
                return k;
            }
            continue;
        }
        mask[k] = false;
        k += 1;
    }
    bytes.len()
}

/// If a raw-string prefix (`r"`, `r##"`, `br"`, `br##"`) starts at `i`, return
/// the index of its opening quote and its hash count.
fn raw_string_prefix(bytes: &[u8], i: usize) -> Option<(usize, usize)> {
    let mut j = i;
    if bytes.get(j) == Some(&b'b') {
        j += 1;
    }
    if bytes.get(j) != Some(&b'r') {
        return None;
    }
    j += 1;
    let hash_start = j;
    while bytes.get(j) == Some(&b'#') {
        j += 1;
    }
    if bytes.get(j) != Some(&b'"') {
        return None;
    }
    Some((j, j - hash_start))
}

/// Mask a raw string whose opening quote is at `quote` and which closes on a `"`
/// followed by exactly `hashes` `#`.
fn mask_raw_string(bytes: &[u8], mask: &mut [bool], quote: usize, hashes: usize) -> usize {
    mask[quote] = false;
    let mut k = quote + 1;
    while k < bytes.len() {
        if bytes[k] == b'"' && closes_raw_string(bytes, k, hashes) {
            let end = (k + hashes).min(bytes.len() - 1);
            mask[k..=end].fill(false);
            return (k + hashes + 1).min(bytes.len());
        }
        mask[k] = false;
        k += 1;
    }
    bytes.len()
}

fn closes_raw_string(bytes: &[u8], quote: usize, hashes: usize) -> bool {
    (1..=hashes).all(|n| bytes.get(quote + n) == Some(&b'#'))
}

/// Mask a `delim`-quoted literal starting at `start`, honouring `\` escapes.
///
/// `reset_at_newline` is `true` only for char literals: a Rust string may legally
/// span lines, while an unclosed `'` means the disambiguation misjudged a
/// lifetime, and failing open there is safer than masking the rest of the file.
fn mask_quoted(
    bytes: &[u8],
    mask: &mut [bool],
    start: usize,
    delim: u8,
    reset_at_newline: bool,
) -> usize {
    mask[start] = false;
    let mut i = start + 1;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' && reset_at_newline {
            return i;
        }
        mask[i] = false;
        if b == b'\\' {
            if i + 1 < bytes.len() {
                mask[i + 1] = false;
            }
            i += 2;
            continue;
        }
        i += 1;
        if b == delim {
            return i;
        }
    }
    bytes.len()
}

/// Index just past the closing `'` when `i` opens a genuine char literal, or
/// `None` when it opens a LIFETIME or label.
///
/// `'\''` takes the escape branch; `'a'` is a literal because the byte after the
/// single character is the closing quote; `'a` in `&'a str` and `'static` are
/// lifetimes, so they open no state at all.
fn char_literal_end(bytes: &[u8], i: usize) -> Option<usize> {
    if bytes.get(i + 1) == Some(&b'\\') {
        let mut k = i + 2;
        while k < bytes.len() && bytes[k] != b'\'' && bytes[k] != b'\n' {
            k += 1;
        }
        return (bytes.get(k) == Some(&b'\'')).then_some(k + 1);
    }
    let rest = std::str::from_utf8(&bytes[i + 1..]).ok()?;
    let ch = rest.chars().next()?;
    let after = i + 1 + ch.len_utf8();
    (bytes.get(after) == Some(&b'\'')).then_some(after + 1)
}

/// The identifier starting at byte `i`, and the index just past it.
pub(crate) fn read_ident(bytes: &[u8], i: usize) -> Option<(&str, usize)> {
    if i >= bytes.len() || !is_ident_start(bytes[i]) {
        return None;
    }
    let mut j = i;
    while j < bytes.len() && is_ident_byte(bytes[j]) {
        j += 1;
    }
    std::str::from_utf8(&bytes[i..j]).ok().map(|s| (s, j))
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Where each `use` item of one Rust file is visible (KEEP-RUST, P15 R4).
///
/// A `use` is visible throughout the innermost brace block that holds it, or the
/// whole file at the top level, before or after the item and in that block's
/// nested blocks. An inline `mod NAME { … }` body is a name-resolution boundary:
/// a child module sees a parent's names only through its own `use super::…`.
/// Braces are matched over code bytes only ([`rust_code_mask`]); when they do not
/// balance, every `use` counts file-wide, which can only keep a binding the
/// resolver would make without this record.
#[derive(Debug, Default)]
pub(crate) struct RustUseScopes {
    uses: Vec<UseItem>,
    /// Inline `mod NAME { … }` bodies, as `(open, close)` brace offsets.
    modules: Vec<(usize, usize)>,
}

/// A `use` item as the scan meets it, before brace pairs are known.
struct ScannedUse {
    /// The innermost open brace around the item; `None` is the file.
    block_open: Option<usize>,
    /// The innermost inline module's open brace around the item.
    module_open: Option<usize>,
    glob: bool,
    names: Vec<String>,
}

#[derive(Debug)]
struct UseItem {
    /// The block the item is visible in, as `(open, close)` byte offsets.
    block: (usize, usize),
    /// The open brace of the inline module that holds the item; `None` is the file.
    module: Option<usize>,
    glob: bool,
    names: Vec<String>,
}

impl RustUseScopes {
    pub(crate) fn scan(src: &str) -> Self {
        let bytes = src.as_bytes();
        let mask = rust_code_mask(src);
        let mut stack: Vec<usize> = Vec::new();
        let mut module_opens: Vec<usize> = Vec::new();
        let mut pairs: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
        let mut found: Vec<ScannedUse> = Vec::new();
        let mut balanced = true;
        let mut i = 0usize;
        while i < bytes.len() {
            if !mask[i] {
                i += 1;
                continue;
            }
            match bytes[i] {
                b'{' => {
                    if follows_mod_name(bytes, &mask, i) {
                        module_opens.push(i);
                    }
                    stack.push(i);
                    i += 1;
                }
                b'}' => {
                    match stack.pop() {
                        Some(open) => {
                            pairs.insert(open, i);
                        }
                        None => balanced = false,
                    }
                    i += 1;
                }
                // `use<'a>` is precise capturing in a type, not an item.
                _ if is_keyword_at(bytes, &mask, i, b"use")
                    && next_code_byte(bytes, &mask, i + 3) != Some(b'<') =>
                {
                    let end = (i + 3..bytes.len())
                        .find(|&j| mask[j] && bytes[j] == b';')
                        .unwrap_or(bytes.len());
                    let mut glob = false;
                    let mut names = Vec::new();
                    let mut j = i + 3;
                    while j < end {
                        if !mask[j] {
                            j += 1;
                        } else if bytes[j] == b'*' {
                            glob = true;
                            j += 1;
                        } else if let Some((ident, after)) = read_ident(bytes, j) {
                            names.push(ident.to_string());
                            j = after;
                        } else {
                            j += 1;
                        }
                    }
                    found.push(ScannedUse {
                        block_open: stack.last().copied(),
                        module_open: stack
                            .iter()
                            .rev()
                            .find(|open| module_opens.contains(open))
                            .copied(),
                        glob,
                        names,
                    });
                    // A use tree's own braces balance inside the item.
                    i = end;
                }
                _ => i += 1,
            }
        }
        let balanced = balanced && stack.is_empty();
        let whole_file = (0, bytes.len());
        let uses = found
            .into_iter()
            .map(|found| UseItem {
                block: match found
                    .block_open
                    .and_then(|open| pairs.get(&open).map(|close| (open, *close)))
                {
                    Some(span) if balanced => span,
                    _ => whole_file,
                },
                module: found.module_open.filter(|_| balanced),
                glob: found.glob,
                names: found.names,
            })
            .collect();
        let modules = if balanced {
            module_opens
                .iter()
                .filter_map(|open| pairs.get(open).map(|close| (*open, *close)))
                .collect()
        } else {
            Vec::new()
        };
        Self { uses, modules }
    }

    /// Whether a `use` visible at byte `offset` names `name` or is a glob.
    pub(crate) fn covers(&self, offset: usize, name: &str) -> bool {
        let module = self
            .modules
            .iter()
            .filter(|(open, close)| *open < offset && offset < *close)
            .map(|(open, _)| *open)
            .max();
        self.uses.iter().any(|item| {
            item.module == module
                && item.block.0 <= offset
                && offset <= item.block.1
                && (item.glob || item.names.iter().any(|n| n == name))
        })
    }
}

/// Whether the code byte at `i` starts the keyword `word`, on word boundaries.
fn is_keyword_at(bytes: &[u8], mask: &[bool], i: usize, word: &[u8]) -> bool {
    bytes[i..].starts_with(word)
        && mask[i..i + word.len()].iter().all(|code| *code)
        && (i == 0 || !mask[i - 1] || !is_ident_byte(bytes[i - 1]))
        && bytes
            .get(i + word.len())
            .is_none_or(|b| !is_ident_byte(*b) || !mask[i + word.len()])
}

/// The first code byte at or after `i` that is not whitespace.
fn next_code_byte(bytes: &[u8], mask: &[bool], i: usize) -> Option<u8> {
    (i..bytes.len())
        .find(|&j| mask[j] && !bytes[j].is_ascii_whitespace())
        .map(|j| bytes[j])
}

/// Whether the `{` at `brace` opens an inline module: `mod NAME {`, skipping
/// whitespace and comments between the parts.
fn follows_mod_name(bytes: &[u8], mask: &[bool], brace: usize) -> bool {
    let skip_back = |mut j: usize| {
        while j > 0 && (!mask[j - 1] || bytes[j - 1].is_ascii_whitespace()) {
            j -= 1;
        }
        j
    };
    let name_end = skip_back(brace);
    let mut name_start = name_end;
    while name_start > 0 && mask[name_start - 1] && is_ident_byte(bytes[name_start - 1]) {
        name_start -= 1;
    }
    if name_start == name_end {
        return false;
    }
    let mod_end = skip_back(name_start);
    mod_end >= 3 && mod_end < name_start && is_keyword_at(bytes, mask, mod_end - 3, b"mod")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(src: &str, needle: &str) -> usize {
        src.find(needle).expect("needle in source")
    }

    #[test]
    fn use_scopes_follow_blocks_and_inline_modules() {
        let src = "use a::Outcome::Err;\n\
                   fn top() { Err(1) }\n\
                   mod child { fn f() { Err(2) } }\n\
                   mod globbed { use super::*; fn g() { Err(3) } }\n\
                   fn local() { use a::Outcome::Ok; Ok(4) }\n\
                   fn sibling() { Ok(5) }\n\
                   // use a::Outcome::Some;\n\
                   fn commented() { Some(6) }\n\
                   fn capture() -> impl Sized + use<> { None }\n\
                   pub(crate) mod braced /* inner */ { use a::Outcome::{Ok, Some}; fn h() { Some(7) } }\n";
        let scopes = RustUseScopes::scan(src);
        assert!(scopes.covers(at(src, "Err(1)"), "Err"));
        assert!(
            !scopes.covers(at(src, "Err(2)"), "Err"),
            "a parent use does not reach a child module"
        );
        assert!(
            scopes.covers(at(src, "Err(3)"), "Err"),
            "the child's own glob does"
        );
        assert!(scopes.covers(at(src, "Ok(4)"), "Ok"));
        assert!(
            !scopes.covers(at(src, "Ok(5)"), "Ok"),
            "a fn-local use does not reach a sibling fn"
        );
        assert!(
            !scopes.covers(at(src, "Some(6)"), "Some"),
            "a commented-out use is no use"
        );
        assert!(
            !scopes.covers(at(src, "None }"), "None"),
            "`use<>` precise capturing is no use item"
        );
        assert!(
            scopes.covers(at(src, "Some(7)"), "Some"),
            "a braced use tree, past a comment"
        );
    }

    #[test]
    fn unbalanced_braces_make_every_use_file_wide() {
        let src = "fn g() { Err(0) }\nmod child { use a::Err;\nfn f() { Err(1) }\n";
        let scopes = RustUseScopes::scan(src);
        assert!(scopes.covers(at(src, "Err(0)"), "Err"));
        assert!(scopes.covers(at(src, "Err(1)"), "Err"));
    }
}
