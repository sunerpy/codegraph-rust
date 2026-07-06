//! C++ `LanguageSpec`, ported from `upstream extraction/languages/c-cpp.ts:144-213`.

use codegraph_core::types::{Language, NodeKind};
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
