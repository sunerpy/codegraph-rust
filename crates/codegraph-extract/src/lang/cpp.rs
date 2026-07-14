//! C++ `LanguageSpec`, ported from `upstream extraction/languages/c-cpp.ts:144-213`.

use std::sync::OnceLock;

use codegraph_core::types::{Language, NodeKind};
use regex::Regex;
use tree_sitter::{Language as TsLanguage, Node};

use crate::lang::c::{include_import, normalize_c_return_type};
use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

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
        (parts.len() > 1).then(|| parts[..parts.len() - 1].join("::"))
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
    let bytes = blank_cpp_annotation_macro_calls(blank_cpp_inline_annotation_macros(
        blank_cpp_api_prefix_macros(source.as_bytes().to_vec()),
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
