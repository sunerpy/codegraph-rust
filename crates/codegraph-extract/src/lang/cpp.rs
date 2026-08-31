//! C++ `LanguageSpec`, ported from `upstream extraction/languages/c-cpp.ts:144-213`.

use std::sync::OnceLock;

use codegraph_core::types::{Language, NodeKind};
use regex::Regex;
use tree_sitter::{Language as TsLanguage, Node};

use crate::lang::c::{include_import, normalize_c_return_type};
use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text, strip_cpp_template_args};

pub struct CppSpec;

pub static CPP_SPEC: CppSpec = CppSpec;

impl LanguageSpec for CppSpec {
    fn language(&self) -> Language {
        Language::Cpp
    }
    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_cpp::LANGUAGE.into()
    }
    fn function_types(&self) -> &'static [&'static str] {
        &["function_definition"]
    }
    fn class_types(&self) -> &'static [&'static str] {
        &["class_specifier"]
    }
    fn method_types(&self) -> &'static [&'static str] {
        &["function_definition"]
    }
    fn interface_types(&self) -> &'static [&'static str] {
        &[]
    }
    fn struct_types(&self) -> &'static [&'static str] {
        &["struct_specifier"]
    }
    fn union_types(&self) -> &'static [&'static str] {
        &["union_specifier"]
    }
    fn enum_types(&self) -> &'static [&'static str] {
        &["enum_specifier"]
    }
    fn enum_member_types(&self) -> &'static [&'static str] {
        &["enumerator"]
    }
    fn type_alias_types(&self) -> &'static [&'static str] {
        &["type_definition", "alias_declaration"]
    }
    fn import_types(&self) -> &'static [&'static str] {
        &["preproc_include"]
    }
    fn call_types(&self) -> &'static [&'static str] {
        &["call_expression"]
    }
    fn variable_types(&self) -> &'static [&'static str] {
        &["declaration"]
    }
    fn name_field(&self) -> &'static str {
        "declarator"
    }
    fn body_field(&self) -> &'static str {
        "body"
    }
    fn params_field(&self) -> &'static str {
        "parameters"
    }
    fn return_field(&self) -> &'static str {
        "type"
    }
    fn resolve_name(&self, node: Node<'_>, source: &str) -> Option<String> {
        if let Some(name) = recover_cpp_macro_defined_name(node, source) {
            return Some(name);
        }
        let qid = declarator_qualified_id(child_by_field(node, "declarator")?)?;
        node_text(qid, source)
            .rsplit("::")
            .filter(|part| !part.is_empty())
            .next()
            .map(str::to_string)
    }
    fn get_receiver_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let qid = declarator_qualified_id(child_by_field(node, "declarator")?)?;
        let parts = node_text(qid, source)
            .split("::")
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        if parts.len() <= 1 {
            return None;
        }
        // An out-of-line template method definition spells the class's template
        // parameter list in the qualifier (`template<typename T> T Box<T>::get()`)
        // while the class node is indexed as bare `Box` — strip `<…>` so the
        // receiver matches it, the same normalization #1043 applies to
        // base-class refs. A multi-line parameter list otherwise leaks whole
        // `<…>` blocks (newlines included) into `qualified_name`, which can
        // exceed NAME_MAX (#1309).
        let receiver = strip_cpp_template_args(&parts[..parts.len() - 1].join("::"));
        (!receiver.is_empty()).then_some(receiver)
    }
    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        recover_return_type(node, source)
    }
    fn get_visibility(&self, node: Node<'_>) -> Option<String> {
        let parent = node.parent()?;
        for child in parent.children(&mut parent.walk()) {
            if child.kind() == "access_specifier" {
                return Some(child.child(0)?.kind().trim_end_matches(':').to_string());
            }
        }
        None
    }
    fn resolve_type_alias_kind(&self, node: Node<'_>, _source: &str) -> Option<NodeKind> {
        for child in node.named_children(&mut node.walk()) {
            if child.kind() == "enum_specifier" && child_by_field(child, "body").is_some() {
                return Some(NodeKind::Enum);
            }
            if child.kind() == "struct_specifier" && child_by_field(child, "body").is_some() {
                return Some(NodeKind::Struct);
            }
            if child.kind() == "union_specifier" && child_by_field(child, "body").is_some() {
                return Some(NodeKind::Union);
            }
        }
        None
    }
    fn is_misparsed_function(&self, name: &str, _node: Node<'_>, _source: &str) -> bool {
        name.starts_with("namespace")
            || matches!(
                name,
                "switch" | "if" | "for" | "while" | "do" | "case" | "return"
            )
    }
    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        include_import(node, source)
    }
    fn pre_parse(&self, source: &str, file_path: &str) -> String {
        pre_parse_cpp_source(source, file_path)
    }
}

/// Offset-preserving C++ pre-parse: blank heavily-reflected Unreal-Engine markup
/// (member-level `*_API` prefixes, line-leading no-semicolon annotation macros,
/// mid-line `UMETA`/`UPARAM`/`UE_DEPRECATED`) so the enclosing class parses
/// instead of collapsing into an ERROR node (#1158). Blanking replaces bytes with
/// ASCII spaces — tree-sitter consumes byte offsets, and every blanked span lies
/// on char boundaries, so byte length (and thus line/column) is preserved. Each
/// pass is `contains`-gated, so macro-free C++ is returned byte-identical.
fn pre_parse_cpp_source(source: &str, file_path: &str) -> String {
    let bytes = blank_msvc_com_interface_keyword(blank_cpp_annotation_macro_calls(
        blank_cpp_inline_annotation_macros(blank_cpp_api_prefix_macros(source.as_bytes().to_vec())),
    ));
    let lower = file_path.to_ascii_lowercase();
    let bytes = if lower.ends_with(".metal") {
        blank_metal_attributes(bytes)
    } else if lower.ends_with(".cu") || lower.ends_with(".cuh") || looks_like_cuda_source(source) {
        blank_cuda_constructs(bytes)
    } else {
        bytes
    };
    String::from_utf8(bytes).unwrap_or_else(|_| source.to_string())
}

/// Blank Metal Shading Language `[[attribute]]` annotations (`[[position]]`,
/// `[[buffer(0)]]`, comma-lists like `[[buffer(0), raster_order_group(0)]]`) to
/// equal-length spaces. MSL puts attributes AFTER the declarator
/// (`float4 position [[position]];`), which tree-sitter-cpp misparses into a
/// spurious `extends` ref from the struct to the field's own type (#1121).
/// `[[`-gated fast-exit; offset-preserving. The `regex` crate is lookahead-free
/// so the tight shape (after `[[`, an identifier then `]]`) alone excludes a
/// subscripted lambda `arr[[]{…}()]`.
fn blank_metal_attributes(bytes: Vec<u8>) -> Vec<u8> {
    let source = match std::str::from_utf8(&bytes) {
        Ok(source) => source,
        Err(_) => return bytes,
    };
    if !source.contains("[[") {
        return bytes;
    }
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"\[\[\s*[A-Za-z_]\w*(?:\s*\([^()\n]*\))?(?:\s*,\s*[A-Za-z_]\w*(?:\s*\([^()\n]*\))?)*\s*\]\]",
        )
        .expect("metal-attribute regex")
    });
    let spans: Vec<(usize, usize)> = re.find_iter(source).map(|m| (m.start(), m.end())).collect();
    let mut bytes = bytes;
    for (start, end) in spans {
        blank_span(&mut bytes, start, end);
    }
    bytes
}

/// Blank CUDA-specific constructs (`.cu`/`.cuh` or content-detected) so the
/// residual parses as plain C++ (#387, CUDA-lang parts of #1172):
/// - `__launch_bounds__(...)` then execution-space/storage specifiers
///   (`__global__` family) — gated by `__`; blanked to equal-length spaces.
///   `__restrict__` is deliberately excluded (grammar parses it natively).
/// - `<<<grid, block[, smem[, stream]]>>>` launch configs — gated by `<<<`;
///   the chevrons otherwise lex as shift operators and destroy the host→kernel
///   call edge. Blanking the span leaves a plain `kernel(args)` call. Only a
///   BRACE-BALANCED match is blanked (a stray `<<<` from a merge-conflict marker
///   opens braces it never closes and is left untouched), matching upstream's
///   char-scan replacer. Offset-preserving.
fn blank_cuda_constructs(bytes: Vec<u8>) -> Vec<u8> {
    let source = match std::str::from_utf8(&bytes) {
        Ok(source) => source,
        Err(_) => return bytes,
    };
    let mut spans: Vec<(usize, usize)> = Vec::new();
    if source.contains("__") {
        static BOUNDS_RE: OnceLock<Regex> = OnceLock::new();
        let bounds_re = BOUNDS_RE.get_or_init(|| {
            Regex::new(r"\b__launch_bounds__\s*\([^()\n]*\)").expect("bounds regex")
        });
        static SPEC_RE: OnceLock<Regex> = OnceLock::new();
        let spec_re = SPEC_RE.get_or_init(|| {
            Regex::new(
                r"\b__(?:global|device|host|constant|shared|managed|grid_constant|forceinline|noinline|launch_bounds)__\b",
            )
            .expect("specifier regex")
        });
        spans.extend(bounds_re.find_iter(source).map(|m| (m.start(), m.end())));
        spans.extend(spec_re.find_iter(source).map(|m| (m.start(), m.end())));
    }
    if source.contains("<<<") {
        static LAUNCH_RE: OnceLock<Regex> = OnceLock::new();
        let launch_re =
            LAUNCH_RE.get_or_init(|| Regex::new(r"<<<[^;]{0,400}?>>>").expect("launch regex"));
        for m in launch_re.find_iter(source) {
            if is_brace_balanced(m.as_str()) {
                spans.push((m.start(), m.end()));
            }
        }
    }
    let mut bytes = bytes;
    for (start, end) in spans {
        blank_span(&mut bytes, start, end);
    }
    bytes
}

/// True when every `{` in `text` is matched by a later `}` and no `}` precedes
/// its opener (net depth stays non-negative and ends at zero). Used to reject a
/// launch-config match that spills across a merge-conflict marker.
fn is_brace_balanced(text: &str) -> bool {
    let mut depth: i32 = 0;
    for b in text.bytes() {
        if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
            if depth < 0 {
                return false;
            }
        }
    }
    depth == 0
}

/// Strong content markers for CUDA in files without a `.cu`/`.cuh` extension
/// (much CUDA lives in `.h`/`.hpp`). The dunder specifiers are nvcc-only and
/// `cudaStream_t` is the runtime stream handle; none is valid C++ anywhere, so
/// a content-triggered blank on a non-CUDA file is inert. Weak markers (`dim3`,
/// `<<<`) are deliberately excluded.
pub(crate) fn looks_like_cuda_source(source: &str) -> bool {
    source.contains("__global__")
        || source.contains("__device__")
        || source.contains("__constant__")
        || source.contains("cudaStream_t")
}

pub(crate) fn blank_cuda_constructs_str(source: &str) -> String {
    let bytes = blank_cuda_constructs(source.as_bytes().to_vec());
    String::from_utf8(bytes).unwrap_or_else(|_| source.to_string())
}

fn blank_span(bytes: &mut [u8], start: usize, end: usize) {
    for b in bytes.iter_mut().take(end).skip(start) {
        if *b != b'\n' && *b != b'\r' {
            *b = b' ';
        }
    }
}

/// Scan a balanced `(...)` from `open` (the index of the `(`), skipping string
/// and char literals so an embedded `)` cannot mis-close. All delimiters are
/// ASCII and UTF-8 continuation bytes never match them, so a byte scan is safe.
/// Returns the index just past the closing `)`, or `None` if unbalanced.
fn balanced_paren_end(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = open;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'"' || c == b'\'' {
            let quote = c;
            i += 1;
            while i < bytes.len() && bytes[i] != quote {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
        } else if c == b'(' {
            depth += 1;
        } else if c == b')' {
            depth -= 1;
            if depth == 0 {
                return Some(i + 1);
            }
        }
        i += 1;
    }
    None
}

/// Blank an export/visibility macro (`ENGINE_API`, `*_EXPORT`, `*_ABI`) in front
/// of a member/method declaration (`ENGINE_API virtual void Tick()`). The upstream
/// `(?=\s+[A-Za-z_])` look-ahead is reproduced in code (the `regex` crate has no
/// look-ahead): a match is blanked only when followed by whitespace then a
/// declaration token, so a value use (`x = FOO_API;`) survives.
fn blank_cpp_api_prefix_macros(bytes: Vec<u8>) -> Vec<u8> {
    let source = match std::str::from_utf8(&bytes) {
        Ok(source) => source,
        Err(_) => return bytes,
    };
    if !(source.contains("_API") || source.contains("_EXPORT") || source.contains("_ABI")) {
        return bytes;
    }
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"\b[A-Z][A-Z0-9_]*(?:_API|_EXPORT|_ABI)\b").expect("api-prefix regex")
    });
    let spans: Vec<(usize, usize)> = re
        .find_iter(source)
        .filter(|m| {
            let mut saw_space = false;
            for c in source[m.end()..].chars() {
                if c.is_whitespace() {
                    saw_space = true;
                } else {
                    return saw_space && (c.is_ascii_alphabetic() || c == '_');
                }
            }
            false
        })
        .map(|m| (m.start(), m.end()))
        .collect();
    let mut bytes = bytes;
    for (start, end) in spans {
        blank_span(&mut bytes, start, end);
    }
    bytes
}

/// Blank a mid-line UE annotation macro (`UMETA(...)`, `UPARAM(...)`,
/// `UE_DEPRECATED*(...)`) — the forms `blank_cpp_annotation_macro_calls` can't see
/// because they are not line-leading. Keyed on an explicit UE-only name list (zero
/// risk to non-UE sources); the whole `MACRO(...)` becomes spaces.
fn blank_cpp_inline_annotation_macros(bytes: Vec<u8>) -> Vec<u8> {
    let source = match std::str::from_utf8(&bytes) {
        Ok(source) => source,
        Err(_) => return bytes,
    };
    if !(source.contains("UMETA") || source.contains("UPARAM") || source.contains("UE_DEPRECATED"))
    {
        return bytes;
    }
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"\b(?:UMETA|UPARAM|UE_DEPRECATED\w*)\s*\(").expect("inline-annotation regex")
    });
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut search_from = 0usize;
    while let Some(m) = re.find_at(source, search_from) {
        match balanced_paren_end(&bytes, m.end() - 1) {
            Some(end) => {
                spans.push((m.start(), end));
                search_from = end;
            }
            None => break,
        }
    }
    let mut bytes = bytes;
    for (start, end) in spans {
        blank_span(&mut bytes, start, end);
    }
    bytes
}

/// Blank a line-leading no-semicolon annotation macro call (`UPROPERTY(...)`,
/// `UFUNCTION(...)`, `GENERATED_BODY()`, `DECLARE_DELEGATE_*(...)`) that decorates
/// the following declaration. Name-list-FREE / structural: the macro must be the
/// first non-whitespace token on its line, ALL-CAPS (`[A-Z][A-Z0-9_]{2,}`), and
/// the char after the balanced `(...)` must START A DECLARATION (`[A-Za-z_~#]`) —
/// so a statement call (`FOO(x);`) or expression fragment is never blanked.
fn blank_cpp_annotation_macro_calls(bytes: Vec<u8>) -> Vec<u8> {
    let source = match std::str::from_utf8(&bytes) {
        Ok(source) => source,
        Err(_) => return bytes,
    };
    static GATE: OnceLock<Regex> = OnceLock::new();
    let gate = GATE.get_or_init(|| {
        Regex::new(r"(?m)^[ \t]*[A-Z][A-Z0-9_]{2,}\s*\(").expect("annotation-gate regex")
    });
    if !gate.is_match(source) {
        return bytes;
    }
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?m)^([ \t]*)([A-Z][A-Z0-9_]{2,})(\s*)\(").expect("annotation-call regex")
    });
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut search_from = 0usize;
    while let Some(caps) = re.captures_at(source, search_from) {
        let whole = caps.get(0).expect("match 0");
        let indent_len = caps.get(1).map_or(0, |g| g.as_str().len());
        let end = match balanced_paren_end(&bytes, whole.end() - 1) {
            Some(end) => end,
            None => {
                search_from = whole.end();
                continue;
            }
        };
        let mut j = end;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        let starts_decl = bytes
            .get(j)
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_' || *b == b'~' || *b == b'#');
        if starts_decl {
            spans.push((whole.start() + indent_len, end));
        }
        search_from = end;
    }
    let mut bytes = bytes;
    for (start, end) in spans {
        blank_span(&mut bytes, start, end);
    }
    bytes
}

/// Rewrite a line-leading MSVC COM `interface` keyword to `struct` + 3 spaces
/// (#1519). `interface` is 9 bytes and `struct` is 6, so the 3 trailing spaces
/// keep the byte length — and therefore every node's line and column — exactly
/// as it was, the same contract every other pass in this file honours.
///
/// `interface` is not a C++ keyword, so tree-sitter-cpp reads
/// `interface IFoo : IBase { … };` as a `function_definition`: the container
/// becomes a `function`, its members become free `function`s instead of
/// `method`s, and the base clause is lost entirely. A COM `interface` IS
/// `#define`d to `struct`, so rewriting the token recovers all three at once.
///
/// Declined unless EVERY byte of the token is ordinary code per
/// [`cpp_code_mask`], and declined when the next token is `class`, `struct`,
/// `union` or `enum` — C++/CLI `interface class IFoo` is valid input today and
/// the naive rewrite would emit the INVALID `struct    class IFoo`.
/// `contains`-gated, so C++ without the token is returned byte-identical.
fn blank_msvc_com_interface_keyword(bytes: Vec<u8>) -> Vec<u8> {
    const KEYWORD: &[u8] = b"interface";
    let source = match std::str::from_utf8(&bytes) {
        Ok(source) => source,
        Err(_) => return bytes,
    };
    if !source.contains("interface") {
        return bytes;
    }
    let mask = cpp_code_mask(source);
    let src = source.as_bytes();
    let mut starts: Vec<usize> = Vec::new();
    let mut pos = 0usize;
    loop {
        let line_end = src[pos..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(src.len(), |offset| pos + offset);
        let mut i = pos;
        while i < line_end && matches!(src[i], b' ' | b'\t') {
            i += 1;
        }
        if i + KEYWORD.len() <= line_end && &src[i..i + KEYWORD.len()] == KEYWORD {
            let all_code = (i..i + KEYWORD.len()).all(|k| mask[k]);
            let mut j = i + KEYWORD.len();
            let mut saw_space = false;
            while j < line_end && matches!(src[j], b' ' | b'\t') {
                saw_space = true;
                j += 1;
            }
            let ident_start = src
                .get(j)
                .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_');
            let mut k = j;
            while k < line_end && (src[k].is_ascii_alphanumeric() || src[k] == b'_') {
                k += 1;
            }
            let excluded = matches!(&src[j..k], b"class" | b"struct" | b"union" | b"enum");
            if all_code && saw_space && ident_start && !excluded {
                starts.push(i);
            }
        }
        if line_end >= src.len() {
            break;
        }
        pos = line_end + 1;
    }
    let mut bytes = bytes;
    for start in starts {
        bytes[start..start + 6].copy_from_slice(b"struct");
        for b in bytes[start + 6..start + KEYWORD.len()].iter_mut() {
            *b = b' ';
        }
    }
    bytes
}

/// Byte mask over `src`: `true` where the byte is ordinary code. One forward
/// scan tracking the five lexical states a textual match could hide in — line
/// comments, block comments, char and string literals, C++ raw strings
/// (`R"delim( … )delim"`, delimiter captured so a custom delimiter still closes
/// correctly), and whole LOGICAL preprocessor lines including backslash
/// continuations. Lexical state is not optional for #1519: a comment can hold a
/// COMPLETE declaration (`/* interface IFoo; */`) that becomes the FOLLOWING
/// node's docstring, so a purely textual match corrupts extraction output even
/// though it never mints a node.
pub(crate) fn cpp_code_mask(src: &str) -> Vec<bool> {
    let bytes = src.as_bytes();
    let mut mask = vec![true; bytes.len()];
    let mut i = 0usize;
    let mut at_line_start = true;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' {
            at_line_start = true;
            i += 1;
            continue;
        }
        if at_line_start {
            if matches!(b, b' ' | b'\t' | b'\r') {
                i += 1;
                continue;
            }
            at_line_start = false;
            if b == b'#' {
                i = mask_preprocessor_line(bytes, &mut mask, i);
                at_line_start = true;
                continue;
            }
        }
        if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                mask[i] = false;
                i += 1;
            }
            continue;
        }
        if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
            mask[i] = false;
            mask[i + 1] = false;
            i += 2;
            while i < bytes.len() {
                let closing = bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/');
                mask[i] = false;
                i += 1;
                if closing {
                    mask[i] = false;
                    i += 1;
                    break;
                }
            }
            continue;
        }
        // A raw string is recognised by the `R` immediately before the quote,
        // which also covers every encoding prefix (`LR"`, `u8R"`, `uR"`, `UR"`).
        if b == b'"' && i > 0 && bytes[i - 1] == b'R' {
            i = mask_raw_string(bytes, &mut mask, i);
            continue;
        }
        if b == b'"' || b == b'\'' {
            i = mask_quoted_literal(bytes, &mut mask, i, b);
            continue;
        }
        i += 1;
    }
    mask
}

/// Mask a whole LOGICAL preprocessor line starting at `start` (the `#`),
/// following every backslash-newline continuation. Returns the index just past
/// the logical line. The continuation matters: `#define D \` + a following
/// `  interface IFoo` line is one directive, and its second physical line is
/// not code.
fn mask_preprocessor_line(bytes: &[u8], mask: &mut [bool], start: usize) -> usize {
    let mut i = start;
    loop {
        while i < bytes.len() && bytes[i] != b'\n' {
            mask[i] = false;
            i += 1;
        }
        let mut back = i;
        if back > start && bytes[back - 1] == b'\r' {
            back -= 1;
        }
        let continued = back > start && bytes[back - 1] == b'\\';
        if i < bytes.len() {
            mask[i] = false;
            i += 1;
        }
        if !continued || i >= bytes.len() {
            return i;
        }
    }
}

/// Mask `R"delim( … )delim"` from its opening quote. The delimiter is captured
/// so a custom one (`R"xy( … )xy"`) closes on its own terminator instead of the
/// first `)"`. A malformed opener falls back to ordinary string rules.
fn mask_raw_string(bytes: &[u8], mask: &mut [bool], quote: usize) -> usize {
    let mut delim_end = quote + 1;
    while delim_end < bytes.len()
        && bytes[delim_end] != b'('
        && delim_end - (quote + 1) < 16
        && !bytes[delim_end].is_ascii_whitespace()
    {
        delim_end += 1;
    }
    if delim_end >= bytes.len() || bytes[delim_end] != b'(' {
        return mask_quoted_literal(bytes, mask, quote, b'"');
    }
    let mut closer = Vec::with_capacity(delim_end - quote + 1);
    closer.push(b')');
    closer.extend_from_slice(&bytes[quote + 1..delim_end]);
    closer.push(b'"');
    let mut k = delim_end + 1;
    let end = loop {
        if k + closer.len() > bytes.len() {
            break bytes.len();
        }
        if bytes[k..k + closer.len()] == closer[..] {
            break k + closer.len();
        }
        k += 1;
    };
    for m in mask.iter_mut().take(end).skip(quote) {
        *m = false;
    }
    end
}

/// Mask a `"…"` or `'…'` literal from its opening quote, honouring backslash
/// escapes. An unterminated literal stops at the newline rather than swallowing
/// the rest of the file.
fn mask_quoted_literal(bytes: &[u8], mask: &mut [bool], start: usize, quote: u8) -> usize {
    mask[start] = false;
    let mut i = start + 1;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\n' {
            break;
        }
        mask[i] = false;
        i += 1;
        if c == b'\\' {
            if i < bytes.len() && bytes[i] != b'\n' {
                mask[i] = false;
                i += 1;
            }
            continue;
        }
        if c == quote {
            break;
        }
    }
    i
}

/// Recover the real function name from the macro-definition idiom
/// `MACRO_NAME(real_name, typed args…) { body }` (flash-attention's
/// `DEFINE_FLASH_FORWARD_KERNEL(flash_fwd_kernel, bool Is_dropout, …) { … }`):
/// tree-sitter parses it as a `function_definition` named after the macro, so
/// every such kernel collapses onto one name (#1172). Narrow gate — ALL of:
/// the parsed name is macro-shaped (`^[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+$`); the
/// first "parameter" is a LONE lowercase-bearing `type_identifier` (the name);
/// ≥2 params and no OTHER param is a lone identifier (so gtest `TEST_F(Fixture,
/// Name)` / `PYBIND11_MODULE(ext, m)` / `BENCHMARK_DEFINE_F(Fix, name)` bail).
fn recover_cpp_macro_defined_name(node: Node<'_>, source: &str) -> Option<String> {
    if node.kind() != "function_definition" {
        return None;
    }
    let declarator = child_by_field(node, "declarator")?;
    if declarator.kind() != "function_declarator" {
        return None;
    }
    let inner = child_by_field(declarator, "declarator")?;
    if inner.kind() != "identifier" {
        return None;
    }
    let macro_name = node_text(inner, source);
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE
        .get_or_init(|| Regex::new(r"^[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+$").expect("macro-name regex"));
    if !re.is_match(&macro_name) {
        return None;
    }
    let params = child_by_field(declarator, "parameters")?;
    let named: Vec<Node<'_>> = params.named_children(&mut params.walk()).collect();
    if named.len() < 2 {
        return None;
    }
    let lone_ident_text = |p: Node<'_>| -> Option<String> {
        if p.kind() != "parameter_declaration" || p.named_child_count() != 1 {
            return None;
        }
        let child = p.named_child(0)?;
        (child.kind() == "type_identifier").then(|| node_text(child, source))
    };
    let name = lone_ident_text(named[0])?;
    if !name.chars().any(|c| c.is_ascii_lowercase()) {
        return None;
    }
    for p in &named[1..] {
        if lone_ident_text(*p).is_some() {
            return None;
        }
    }
    Some(name)
}

/// What an explicit-operator `call_expression` should contribute as a `Calls`
/// ref (#1268).
pub(crate) enum ExplicitOperatorCall {
    /// Emit this callee name (`a.operator+`, or the bare `operator+` for
    /// `this->`).
    Callee(String),
    /// A receiver that cannot aid type inference (`w.obj()->operator+`): emit
    /// NOTHING. A bare operator name would fall through to exact-name matching,
    /// which guesses among unrelated same-named operators — a silent miss is
    /// preferable to a wrong edge.
    Drop,
}

/// Recover the callee of a C++ explicit operator call (`a.operator+(b)`,
/// `p->operator+(b)`, `a.operator[](3)`) — upstream #1268.
///
/// tree-sitter-cpp cannot parse an `operator_name` in field position, so the
/// `call_expression` carries `function: <receiver>` plus an ERROR child wrapping
/// the `operator_name` instead of a `field_expression` callee. Reading the
/// `function` field alone yields just the receiver (`a`), an unresolvable ref.
///
/// `None` means "not this shape" — the caller continues down the normal call
/// path (which still owns `V::operator+(a, b)` and the `a.operator bool()` word
/// form, both of which parse without a stranded `operator_name`).
pub(crate) fn recover_explicit_operator_call(
    node: Node<'_>,
    source: &str,
) -> Option<ExplicitOperatorCall> {
    let operator_name = node
        .named_children(&mut node.walk())
        .filter(|child| child.kind() == "ERROR")
        .find_map(|error| {
            error
                .named_children(&mut error.walk())
                .find(|c| c.kind() == "operator_name")
        })
        .map(|op| compact_operator_name(&node_text(op, source)))?;
    let func = child_by_field(node, "function").or_else(|| node.named_child(0))?;
    let receiver: String = node_text(func, source)
        .replace("->", ".")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if receiver == "this" {
        return Some(ExplicitOperatorCall::Callee(operator_name));
    }
    if !is_simple_receiver_chain(&receiver) {
        return Some(ExplicitOperatorCall::Drop);
    }
    Some(ExplicitOperatorCall::Callee(format!(
        "{receiver}.{operator_name}"
    )))
}

/// Call sites may space a symbolic operator name (`it.operator * ()`,
/// `other.operator < (*this)` in nlohmann/json) while the definition indexes
/// compact (`operator*`). Squeeze the whitespace out of the SYMBOLIC forms only
/// — the word forms (`operator new`, `operator bool`) need their space.
fn compact_operator_name(raw: &str) -> String {
    let trimmed = raw.trim();
    let Some(symbol) = trimmed.strip_prefix("operator") else {
        return trimmed.to_string();
    };
    let symbol = symbol.trim();
    let symbolic = symbol
        .chars()
        .next()
        .is_some_and(|c| !c.is_alphanumeric() && c != '_');
    if !symbolic {
        return trimmed.to_string();
    }
    let squeezed: String = symbol.chars().filter(|c| !c.is_whitespace()).collect();
    format!("operator{squeezed}")
}

/// `^[A-Za-z_][\w.]*$` — an identifier or plain member chain, the only receiver
/// shapes downstream receiver-type inference can work with.
fn is_simple_receiver_chain(receiver: &str) -> bool {
    let mut chars = receiver.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c == '.' || c.is_ascii_alphanumeric())
}

fn declarator_qualified_id<'tree>(declarator: Node<'tree>) -> Option<Node<'tree>> {
    let mut queue = vec![declarator];
    while let Some(current) = queue.pop() {
        if current.kind() == "qualified_identifier" {
            return Some(current);
        }
        for child in current.named_children(&mut current.walk()) {
            if !matches!(child.kind(), "parameter_list" | "trailing_return_type") {
                queue.push(child);
            }
        }
    }
    None
}

/// Curated inline-specifier / attribute macros that precede a return type in
/// real-world C++ (`#1100-1103`). When one of these sits before the return
/// type, tree-sitter misparses it AS the return type; recognizing it lets the
/// real return type be recovered from the trailing ERROR node.
const INLINE_SPECIFIER_MACROS: &[&str] = &[
    // Unreal Engine
    "FORCEINLINE",
    "FORCENOINLINE",
    "FORCEINLINE_DEBUGGABLE",
    // pugixml
    "PUGI__FN",
    "PUGIXML_FUNCTION",
    // Godot
    "_FORCE_INLINE_",
    "_ALWAYS_INLINE_",
    // Boost
    "BOOST_FORCEINLINE",
    "BOOST_NOINLINE",
    // generic / cross-project
    "ALWAYS_INLINE",
    "FORCE_INLINE",
    "NOINLINE",
    "INLINE",
    // Qt
    "Q_ALWAYS_INLINE",
    "Q_NEVER_INLINE",
    "Q_DECL_CONSTEXPR",
    "Q_INVOKABLE",
    // Folly
    "FOLLY_ALWAYS_INLINE",
    "FOLLY_NOINLINE",
    // Abseil
    "ABSL_ATTRIBUTE_ALWAYS_INLINE",
    "ABSL_ATTRIBUTE_NOINLINE",
    "ABSL_MUST_USE_RESULT",
    // LLVM
    "LLVM_ATTRIBUTE_ALWAYS_INLINE",
    "LLVM_ATTRIBUTE_NOINLINE",
    "LLVM_NODISCARD",
    // V8
    "V8_INLINE",
    "V8_NOINLINE",
    "V8_WARN_UNUSED_RESULT",
    // Eigen
    "EIGEN_STRONG_INLINE",
    "EIGEN_ALWAYS_INLINE",
    "EIGEN_DEVICE_FUNC",
    // rapidjson
    "RAPIDJSON_FORCEINLINE",
    // Mozilla
    "MOZ_ALWAYS_INLINE",
    "MOZ_NEVER_INLINE",
    "MOZ_MUST_USE",
    // Protobuf
    "PROTOBUF_ALWAYS_INLINE",
    "PROTOBUF_NOINLINE",
    // fmt
    "FMT_INLINE",
    "FMT_CONSTEXPR",
    // nlohmann json
    "JSON_HEDLEY_ALWAYS_INLINE",
    // GLM
    "GLM_FUNC_QUALIFIER",
    "GLM_INLINE",
    // Bullet
    "SIMD_FORCE_INLINE",
    // Skia
    "SK_ALWAYS_INLINE",
    // OpenCV
    "CV_ALWAYS_INLINE",
    "CV_INLINE",
    // EASTL
    "EA_FORCE_INLINE",
    // Cocos2d-x
    "CC_FORCE_INLINE",
    // GLib
    "G_INLINE_FUNC",
    "G_GNUC_INTERNAL",
    // SQLite
    "SQLITE_PRIVATE",
    "SQLITE_API",
    // Windows calling conventions / attributes
    "WINAPI",
    "CALLBACK",
    "APIENTRY",
    "WINAPIV",
    "STDMETHODCALLTYPE",
    "__stdcall",
    "__cdecl",
    "__fastcall",
    "__declspec",
];

fn is_inline_specifier_macro(text: &str) -> bool {
    INLINE_SPECIFIER_MACROS.contains(&text)
}

/// The first `identifier` inside the ERROR node that tree-sitter emits when a
/// leading macro is misparsed as the return type. In that misparse the real
/// return type ends up here (`FORCEINLINE FString f()` → `type=FORCEINLINE`,
/// `ERROR{identifier=FString}`).
fn error_recovered_return_identifier<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    let error = node
        .children(&mut node.walk())
        .find(|c| c.kind() == "ERROR")?;
    error
        .named_children(&mut error.walk())
        .find(|c| c.kind() == "identifier")
}

/// C++ return-type resolution with inline-specifier-macro recovery (`#1100-1103`).
/// - No misparse (no ERROR sibling): normal `type`-field resolution.
/// - Listed macro before the type: recover the real return type from the ERROR
///   node (`FORCEINLINE FString f()` → `FString`).
/// - Unknown leading macro (generic #1102): the name is already correct via the
///   declarator; do NOT record the macro as the return type — return None.
fn recover_return_type(node: Node<'_>, source: &str) -> Option<String> {
    let type_node = child_by_field(node, "type")?;
    let type_text = node_text(type_node, source);
    let type_text = type_text.trim();
    if type_node.kind() == "type_identifier" {
        if is_inline_specifier_macro(type_text) {
            if let Some(real) = error_recovered_return_identifier(node) {
                return normalize_c_return_type(&node_text(real, source));
            }
            return None;
        }
        if error_recovered_return_identifier(node).is_some() {
            return None;
        }
    }
    normalize_c_return_type(&node_text(type_node, source))
}

/// True for an export/visibility macro (`*_API`, `*_EXPORT`, `*_ABI`) that,
/// placed between `class`/`struct` and the type name, makes tree-sitter misread
/// the whole declaration as a function and drop the class (`#1061`).
fn is_export_visibility_macro(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty()
        && text == text.to_ascii_uppercase()
        && (text.ends_with("_API") || text.ends_with("_EXPORT") || text.ends_with("_ABI"))
}

/// The components recovered from an export-macro-misparsed class (`#1061`):
/// the real class-name node, an optional single base-class node, the class body,
/// and whether the outer node was a `struct` (vs `class`).
pub(crate) struct ExportMacroClass<'tree> {
    pub name: Node<'tree>,
    pub base: Option<Node<'tree>>,
    pub body: Node<'tree>,
    pub is_struct: bool,
}

/// Detect the `class MYMODULE_API C : public Base { ... }` misparse: the outer
/// node's `type` field is a `class`/`struct` specifier whose name is an
/// export-visibility macro, and the real class name/base/body live in the
/// following ERROR / declarator / body children (`#1061`). Only single, plain
/// base classes are recovered; the templated-base case (#1043) is DEFERRED.
pub(crate) fn detect_export_macro_class<'tree>(
    node: Node<'tree>,
    source: &str,
) -> Option<ExportMacroClass<'tree>> {
    let type_node = child_by_field(node, "type")?;
    let is_struct = match type_node.kind() {
        "class_specifier" => false,
        "struct_specifier" => true,
        _ => return None,
    };
    let macro_name = child_by_field(type_node, "name")?;
    if macro_name.kind() != "type_identifier"
        || !is_export_visibility_macro(&node_text(macro_name, source))
    {
        return None;
    }
    let body = child_by_field(node, "body")?;
    if body.kind() != "compound_statement" && body.kind() != "field_declaration_list" {
        return None;
    }
    let declarator = child_by_field(node, "declarator");
    let error_ident = node
        .children(&mut node.walk())
        .find(|c| c.kind() == "ERROR")
        .and_then(|err| {
            err.named_children(&mut err.walk())
                .find(|c| c.kind() == "identifier")
        });
    let (name, base) = match error_ident {
        Some(error_ident) => (error_ident, declarator.filter(|d| d.kind() == "identifier")),
        None => (declarator.filter(|d| d.kind() == "identifier")?, None),
    };
    Some(ExportMacroClass {
        name,
        base,
        body,
        is_struct,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_cpp_api_prefix_member() {
        let src = "ENGINE_API virtual void Tick();";
        let out = pre_parse_cpp_source(src, "t.cpp");
        assert_eq!(out.len(), src.len());
        assert!(out.starts_with("           virtual void Tick();"));
    }

    #[test]
    fn blank_cpp_api_prefix_bare_value_untouched() {
        let src = "int x = MY_API;";
        let out = pre_parse_cpp_source(src, "t.cpp");
        assert_eq!(out, src);
    }

    #[test]
    fn blank_cpp_annotation_macro_calls_ue() {
        let src = "UPROPERTY(EditAnywhere)\nint X;";
        let out = pre_parse_cpp_source(src, "t.cpp");
        assert_eq!(out.len(), src.len());
        assert!(out.starts_with("                       \nint X;"));
    }

    #[test]
    fn blank_cpp_annotation_statement_call_untouched() {
        let src = "FOO(x);";
        let out = pre_parse_cpp_source(src, "t.cpp");
        assert_eq!(out, src);
    }

    #[test]
    fn msvc_com_interface_rewrite_preserves_byte_offsets() {
        // `interface` (9 bytes) -> `struct` (6) + 3 spaces, so every subsequent
        // byte offset — and therefore every node's line and column — is exactly
        // where it was. A shorter or longer replacement would silently shift the
        // whole file's positions.
        let src = "interface IWidget : IBase {\n    void Run() { }\n};\n";
        let out = pre_parse_cpp_source(src, "com.hpp");
        assert_eq!(
            out.len(),
            src.len(),
            "the rewrite must preserve byte length"
        );
        assert_eq!(out, "struct    IWidget : IBase {\n    void Run() { }\n};\n");
        // The rewritten declaration must land on the same line and column as the
        // equivalent struct written by hand.
        let control = "struct    SControl : IBase {\n    void Run() { }\n};\n";
        let control_out = pre_parse_cpp_source(control, "ctl.hpp");
        assert_eq!(
            control_out, control,
            "the control must be returned unchanged"
        );
        assert_eq!(
            out.find("IWidget").expect("name present"),
            control.find("SControl").expect("control name present"),
            "the rewritten name must start at the control's column"
        );
    }

    /// Every §3.3.3 shape, as a direct unit test of the pass: 11 that must NOT
    /// fire and 4 that must, each also asserting byte length is preserved. This
    /// is the ONLY isolated proof of the `class`/`struct`/`union`/`enum`
    /// exclusion — the end-to-end `neg_interface.hpp` invariance test cannot see
    /// it, because dropping the exclusion emits `struct    class INegCli`, which
    /// tree-sitter's error recovery happens to extract identically for that
    /// file's text (measured). The exclusion is still required: it stops the pass
    /// emitting invalid C++ from input that parses correctly today.
    #[test]
    fn cpp_code_mask_covers_comments_strings_and_preproc() {
        let must_not_fire: &[(&str, &str)] = &[
            (
                "block comment",
                "/*\ninterface IFoo;\n*/\nstruct R { int x; };\n",
            ),
            (
                "block cmt full decl",
                "/*\ninterface IGhost { virtual void B() = 0; };\n*/\nstruct R { int x; };\n",
            ),
            ("line comment", "// interface IFoo;\nstruct R { int x; };\n"),
            (
                "cpp/cli class",
                "interface class IFoo { public: virtual void B()=0; };\n",
            ),
            ("interface struct", "interface struct IBar { int x; };\n"),
            ("__interface", "__interface IFoo { virtual void B()=0; };\n"),
            ("raw string", "const char* s = R\"(\ninterface IFoo\n)\";\n"),
            (
                "raw string full decl",
                "const char* s = R\"(\ninterface IGhost { virtual void B() = 0; };\n)\";\n",
            ),
            (
                "raw delim",
                "const char* s = R\"xy(\ninterface IFoo )\" still inside\n)xy\";\n",
            ),
            (
                "macro continuation",
                "#define DECL \\\n  interface IMacro\nstruct R { int x; };\n",
            ),
            ("interface_ptr", "int interface_ptr = 0;\n"),
        ];
        for (label, src) in must_not_fire {
            let out = pre_parse_cpp_source(src, "n.hpp");
            assert_eq!(out.len(), src.len(), "{label}: byte length moved");
            assert_eq!(out, *src, "{label}: the substitution must NOT fire");
        }

        let must_fire: &[(&str, &str, &str)] = &[
            (
                "plain",
                "interface IWidget : IBase {\n};\n",
                "struct    IWidget : IBase {\n};\n",
            ),
            (
                "newline brace",
                "interface INewline\n{\n};\n",
                "struct    INewline\n{\n};\n",
            ),
            (
                "indented",
                "  interface IIndent {\n};\n",
                "  struct    IIndent {\n};\n",
            ),
            (
                // The discriminating case: the masked `#define` directive is left
                // alone while the real declaration beneath it IS rewritten.
                "define interface",
                "#define interface struct\ninterface IDefined {\n};\n",
                "#define interface struct\nstruct    IDefined {\n};\n",
            ),
        ];
        for (label, src, want) in must_fire {
            let out = pre_parse_cpp_source(src, "p.hpp");
            assert_eq!(out.len(), src.len(), "{label}: byte length moved");
            assert_eq!(out, *want, "{label}: the substitution must fire");
        }
    }

    #[test]
    fn blank_cpp_annotation_in_expression_untouched() {
        let src = "if (CHECK(x)) {}";
        let out = pre_parse_cpp_source(src, "t.cpp");
        assert_eq!(out, src);
    }

    #[test]
    fn blank_cpp_inline_annotation_umeta() {
        let src = "Foo UMETA(DisplayName=\"Foo\"),";
        let out = pre_parse_cpp_source(src, "t.cpp");
        assert_eq!(out.len(), src.len());
        assert!(out.starts_with("Foo "));
        assert!(out.ends_with(","));
        assert!(!out.contains("UMETA"));
    }

    #[test]
    fn blank_cpp_inline_annotation_lowercase_untouched() {
        let src = "auto v = meta(1);";
        let out = pre_parse_cpp_source(src, "t.cpp");
        assert_eq!(out, src);
    }

    #[test]
    fn cpp_pre_parse_is_offset_preserving() {
        let src = r#"class ENGINE_API UFoo : public UObject
{
    GENERATED_BODY()
    UPROPERTY(EditAnywhere)
    ENGINE_API int X;
    UFUNCTION()
    void Bar();
};
"#;
        let out = pre_parse_cpp_source(src, "t.cpp");
        assert_eq!(out.len(), src.len());
        assert_eq!(
            out.bytes().filter(|&b| b == b'\n').count(),
            src.bytes().filter(|&b| b == b'\n').count()
        );
    }

    #[test]
    fn cpp_pre_parse_noop_on_plain_cpp() {
        let src = r#"namespace ns {
class Widget {
public:
    void render();
};
}
"#;
        assert_eq!(pre_parse_cpp_source(src, "t.cpp"), src);
    }

    #[test]
    fn blank_metal_attributes_blanks_field_attribute() {
        let src = "float4 position [[position]];";
        let out = blank_metal_attributes(src.as_bytes().to_vec());
        let out = String::from_utf8(out).unwrap();
        assert_eq!(out.len(), src.len());
        assert_eq!(out, "float4 position             ;");
        assert!(!out.contains("[["));
    }

    #[test]
    fn blank_metal_attributes_ignores_no_double_bracket() {
        let src = "float4 position;";
        let out = blank_metal_attributes(src.as_bytes().to_vec());
        assert_eq!(String::from_utf8(out).unwrap(), src);
    }

    #[test]
    fn blank_metal_attributes_ignores_lambda_subscript() {
        let src = "arr[[]{return 0;}()]";
        let out = blank_metal_attributes(src.as_bytes().to_vec());
        assert_eq!(String::from_utf8(out).unwrap(), src);
    }

    #[test]
    fn metal_attribute_blanked_only_for_dot_metal() {
        let metal = pre_parse_cpp_source("struct S { float4 p [[position]]; };", "s.metal");
        assert!(!metal.contains("[[position]]"));
        assert!(metal.contains("float4 p"));
        let cpp = "[[nodiscard]] int f();";
        assert_eq!(pre_parse_cpp_source(cpp, "s.cpp"), cpp);
    }

    #[test]
    fn blank_cuda_specifier() {
        let src = "__global__ void f() {}";
        let out = blank_cuda_constructs(src.as_bytes().to_vec());
        let out = String::from_utf8(out).unwrap();
        assert_eq!(out.len(), src.len());
        assert!(out.starts_with("           void f()"));
        assert!(!out.contains("__global__"));
    }

    #[test]
    fn blank_cuda_launch_config() {
        let src = "f<<<g, b>>>(x);";
        let out = blank_cuda_constructs(src.as_bytes().to_vec());
        let out = String::from_utf8(out).unwrap();
        assert_eq!(out.len(), src.len());
        assert_eq!(out, "f          (x);");
    }

    #[test]
    fn blank_cuda_launch_unbalanced_braces_untouched() {
        let src = "a <<< b } c >>> d;";
        let out = blank_cuda_constructs(src.as_bytes().to_vec());
        assert_eq!(String::from_utf8(out).unwrap(), src);
    }

    #[test]
    fn looks_like_cuda_source_markers() {
        assert!(looks_like_cuda_source("__global__ void f();"));
        assert!(looks_like_cuda_source("cudaStream_t s;"));
        assert!(!looks_like_cuda_source("int main() { return 0; }"));
    }

    #[test]
    fn cuda_blanked_for_dot_cu() {
        let out = pre_parse_cpp_source("void h() { k<<<g, b>>>(x); }", "k.cu");
        assert!(!out.contains("<<<"));
        assert!(out.contains("k          (x);"));
    }

    #[test]
    fn cuda_blanked_by_content_in_hpp() {
        let out = pre_parse_cpp_source("__global__ void k() {}", "k.hpp");
        assert!(!out.contains("__global__"));
    }

    #[test]
    fn recover_macro_kernel_name() {
        assert_eq!(
            resolve_macro_name("DEFINE_KERNEL(my_kernel, int n) {}"),
            Some("my_kernel".to_string())
        );
    }

    #[test]
    fn recover_macro_ignores_gtest_shape() {
        assert_eq!(resolve_macro_name("TEST_F(Fixture, Name) {}"), None);
    }

    #[test]
    fn recover_macro_ignores_lowercase_macro() {
        assert_eq!(resolve_macro_name("define_x(a, int b) {}"), None);
    }

    fn resolve_macro_name(src: &str) -> Option<String> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(src, None).unwrap();
        let mut cursor = tree.root_node().walk();
        let func = tree
            .root_node()
            .named_children(&mut cursor)
            .find(|c| c.kind() == "function_definition")?;
        recover_cpp_macro_defined_name(func, src)
    }
}
