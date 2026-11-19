//! Expo modules (upstream `resolution/frameworks/expo-modules.ts` at
//! `e8ddc4e2`: `4d1a2b3c` #430, `07af3db6`'s generic Kotlin functions,
//! `67744468` #2134's JavaScript bindings, `6bd217a2` #2769's escaping).
//!
//! A project is Expo's when its root `package.json` names
//! `expo-modules-core`, or one of the first 200 indexed paths is a `.swift`
//! or `.kt` file declaring a module (`class X : Module` and a
//! `Function("x")`-style declaration). Expo modules
//! ([`ExpoModulesResolver`]) extracts each such file's declarations:
//! one `method` node per `Function`, `AsyncFunction`, `Property` or
//! `Constants` name (`expo-module:` ids), under the module `Name("…")`
//! declares, else the class. Expo modules JS ([`ExpoModulesJsResolver`])
//! reads a JavaScript-family call `X.fn()` whose `X` is bound to
//! `requireNativeModule('Mod')`, in the call's own file or through the
//! import of a module whose default or named export is one: it reaches every
//! declaration of `fn` in module `Mod`, Swift first, at `0.95`; when there
//! is none, the method, property or field `fn` of the type the binding is
//! given (`requireNativeModule<T>`) at `0.9`.
//!
//! Rust's own (KNOWN_DIFFS "The bridge a name means", KEEP-RUST): the
//! typed answer takes the sole such member, nothing of several (upstream
//! takes the first). A name is matched as written, never built into a
//! pattern (#2769).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde::{Deserialize, Serialize};

use super::framework_node;
use super::js_language_for;
use super::react_native::{JS_FAMILY, js_regex, sole_node};
use super::swift_objc::node_order;
use crate::framework::{FrameworkExtractionContext, FrameworkResolver};
use crate::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::types::{
    AlsoTarget, EdgeShape, FrameworkResolverExtractionResult, RefView, ResolutionContext,
    ResolvedBy, ResolvedRef,
};

/// The extraction resolver's name.
pub const NAME: &str = "expo-modules";
/// The JavaScript resolver's name, which its edges carry in
/// `metadata.framework`.
pub const JS_NAME: &str = "expo-modules-js";

/// The languages whose files it extracts.
const EXTRACT_LANGUAGES: [Language; 2] = [Language::Swift, Language::Kotlin];

/// `requireNativeModule('Mod')`, typed or not, optional or not (upstream
/// `REQUIRE_NATIVE`): the type, then the module.
const REQUIRE_NATIVE: &str = r#"(?-u:\b)require(?:Optional)?NativeModule{S}*(?:<{S}*([A-Za-z_$][{W}$]*)[^>]*>)?{S}*\({S}*['"]([A-Za-z_$][{W}$]*)['"]{S}*\)"#;

macro_rules! patterns {
    ($($(#[$doc:meta])* $name:ident = $pattern:expr;)*) => {
        $(
            $(#[$doc])*
            fn $name() -> &'static Regex {
                static PATTERN: OnceLock<Regex> = OnceLock::new();
                PATTERN.get_or_init(|| js_regex(&$pattern))
            }
        )*
    };
}

patterns! {
    /// `Function("x")`, `AsyncFunction<T>("x")`, `Property("x")`,
    /// `Constants("x")` (upstream `EXPO_DECL_RE`).
    expo_decl = r#"(?-u:\b)(Function|AsyncFunction|Property|Constants){S}*(?:<[^(]*>)?{S}*\({S}*["']([A-Za-z_][A-Za-z0-9_]*)["']"#;
    /// `Name("ExpoX")` (upstream `EXPO_MODULE_NAME_RE`).
    expo_module_name = r#"(?-u:\b)Name{S}*\({S}*["']([A-Za-z_][A-Za-z0-9_]*)["']"#;
    /// `class X : Module` (upstream `EXPO_CLASS_RE`).
    expo_class = r"(?-u:\b)class{S}+([A-Za-z_][A-Za-z0-9_]*){S}*:{S}*Module(?-u:\b)";
    /// A `package.json` dependency on Expo's modules core.
    expo_modules_core_dependency = r#"["']expo-modules-core["']{S}*:"#;
    /// `const NAME`, the names a binding can be declared under.
    local_declaration = r"(?-u:\b)(?:const|let|var){S}+([A-Za-z_$][{W}$]*)";
    /// `export const NAME`, the names a named export can be.
    named_declaration = r"(?-u:\b)export{S}+(?:const|let){S}+([A-Za-z_$][{W}$]*)";
    /// What follows a declared name when it is bound to a native module.
    binding_rest = format!(r"^{{S}}*(?::[^=]+)?={{S}}*{REQUIRE_NATIVE}");
    /// `export default requireNativeModule(…)`.
    default_binding = format!(r"(?-u:\b)export{{S}}+default{{S}}+{REQUIRE_NATIVE}");
    /// A call through one name: `X.fn`.
    member_call = r"^([A-Za-z_$][{W}$]*)\.([A-Za-z_$][{W}$]*)$";
}

/// What a name is bound to by `requireNativeModule` (upstream
/// `NativeBinding`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpoBinding {
    /// The module the call names.
    pub module: String,
    /// The type it is given: `requireNativeModule<CameraNativeModule>(…)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,
}

/// A file's `requireNativeModule` bindings: the names it binds, its
/// default export and its named exports, each as upstream's per-name
/// patterns find it first in the comment-stripped text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpoBindings {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locals: Vec<(String, ExpoBinding)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_export: Option<ExpoBinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub named_exports: Vec<(String, ExpoBinding)>,
}

impl ExpoBindings {
    pub fn is_empty(&self) -> bool {
        self.locals.is_empty() && self.default_export.is_none() && self.named_exports.is_empty()
    }

    /// The binding of the name `local` in the file.
    pub fn local(&self, local: &str) -> Option<&ExpoBinding> {
        self.locals
            .iter()
            .find(|(name, _)| name == local)
            .map(|(_, binding)| binding)
    }

    /// The named export `exported`'s binding.
    pub fn named(&self, exported: &str) -> Option<&ExpoBinding> {
        self.named_exports
            .iter()
            .find(|(name, _)| name == exported)
            .map(|(_, binding)| binding)
    }
}

fn binding_of(found: &regex::Captures<'_>) -> ExpoBinding {
    ExpoBinding {
        module: found[2].to_string(),
        type_name: found.get(1).map(|name| name.as_str().to_string()),
    }
}

/// The first binding of each name `declarations` finds in `safe`: at each
/// declaration of a name, in text order, the rest of upstream's per-name
/// pattern (`… NAME [: T] = requireNativeModule(…)`) tried right after the
/// name; the first that holds is the name's. A declaration is looked for at
/// every keyword, so one starting inside another (`const const x`) is read
/// as upstream's per-name search reads it.
fn first_bindings(declarations: &Regex, safe: &str) -> Vec<(String, ExpoBinding)> {
    let mut out: Vec<(String, ExpoBinding)> = Vec::new();
    let mut at = 0;
    while let Some(found) = declarations.captures_at(safe, at) {
        let start = found.get(0).expect("a declaration").start();
        at = start + safe[start..].chars().next().map_or(1, char::len_utf8);
        let name = found.get(1).expect("a declared name");
        if out.iter().any(|(bound, _)| bound == name.as_str()) {
            continue;
        }
        if let Some(rest) = binding_rest().captures(&safe[name.end()..]) {
            out.push((name.as_str().to_string(), binding_of(&rest)));
        }
    }
    out
}

/// What a file's text binds by `requireNativeModule` (upstream
/// `nativeBinding`'s three patterns, run on `stripCommentsForRegex(…,
/// 'typescript')`, which keeps strings): empty when the text holds no
/// `NativeModule`.
pub fn expo_bindings_of(text: &str) -> ExpoBindings {
    if !text.contains("NativeModule") {
        return ExpoBindings::default();
    }
    let safe = strip_comments_for_regex(text, CommentLang::TypeScript);
    ExpoBindings {
        locals: first_bindings(local_declaration(), &safe),
        default_export: default_binding()
            .captures(&safe)
            .map(|found| binding_of(&found)),
        named_exports: first_bindings(named_declaration(), &safe),
    }
}

/// Whether a Swift or Kotlin source declares an Expo module (upstream
/// `isExpoModuleSource`): `class X : Module` and a declaration.
pub fn is_expo_module_source(source: &str) -> bool {
    expo_class().is_match(source) && expo_decl().is_match(source)
}

/// The UTF-16 length of `text`.
fn utf16_len(text: &str) -> i64 {
    text.encode_utf16().count() as i64
}

/// An Expo module file's declarations (upstream `extractExpoMethods`): one
/// `method` node per declaration name and line, under the module
/// `Name("…")` declares, else the class, else `ExpoModule`.
pub fn extract_expo_methods(file_path: &str, source: &str, language: Language) -> Vec<Node> {
    if !is_expo_module_source(source) {
        return Vec::new();
    }
    let module = expo_module_name()
        .captures(source)
        .and_then(|found| found.get(1))
        .or_else(|| expo_class().captures(source).and_then(|found| found.get(1)))
        .map_or("ExpoModule", |name| name.as_str());
    let mut seen: Vec<(String, i64)> = Vec::new();
    let mut nodes = Vec::new();
    for found in expo_decl().captures_iter(source) {
        let kind = &found[1];
        let name = &found[2];
        let start = found.get(0).expect("a declaration").start();
        let before = &source[..start];
        let line = 1 + before.matches('\n').count() as i64;
        if seen
            .iter()
            .any(|(seen_name, seen_line)| seen_name == name && *seen_line == line)
        {
            continue;
        }
        seen.push((name.to_string(), line));
        let column = utf16_len(before.rsplit('\n').next().unwrap_or(before));
        let mut node = framework_node(
            format!("expo-module:{file_path}:{module}:{name}:{line}"),
            NodeKind::Method,
            name.to_string(),
            format!("{file_path}::{module}.{name}"),
            file_path.to_string(),
            line,
            line,
            column,
            column + utf16_len(kind) + 2 + utf16_len(name) + 2,
            language,
            true,
        );
        node.docstring = Some(format!("Expo Modules {kind}(\"{name}\") in {module}"));
        node.signature = Some(format!("{kind}(\"{name}\")"));
        nodes.push(node);
    }
    nodes
}

/// Whether the project is Expo's: the root `package.json` naming
/// `expo-modules-core`, else one of the first 200 indexed paths a `.swift`
/// or `.kt` module source.
fn detect_expo(context: &dyn ResolutionContext) -> bool {
    if context
        .read_file_shared("package.json")
        .is_some_and(|package| expo_modules_core_dependency().is_match(&package))
    {
        return true;
    }
    let files = context.get_all_files_shared();
    let window: Vec<String> = files[..files.len().min(200)]
        .iter()
        .filter(|file| file.ends_with(".swift") || file.ends_with(".kt"))
        .cloned()
        .collect();
    context
        .files_containing(&window, &["Module"])
        .iter()
        .any(|file| {
            context
                .read_file_shared(file)
                .is_some_and(|text| is_expo_module_source(&text))
        })
}

/// Expo modules: the native declarations.
pub struct ExpoModulesResolver;

impl FrameworkResolver for ExpoModulesResolver {
    fn name(&self) -> &str {
        NAME
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&EXTRACT_LANGUAGES)
    }

    /// No reference is read: the JavaScript side is
    /// [`ExpoModulesJsResolver`]'s.
    fn resolve_languages(&self) -> Option<&[Language]> {
        Some(&[])
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        detect_expo(context)
    }

    fn resolve(
        &self,
        _reference: &RefView,
        _context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        None
    }

    /// A Swift or Kotlin module's declarations, by upstream's extension test
    /// (`.kt` Kotlin, any other Swift).
    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        let language = if file_path.ends_with(".kt") {
            Language::Kotlin
        } else {
            Language::Swift
        };
        Some(FrameworkResolverExtractionResult {
            nodes: extract_expo_methods(file_path, content, language),
            references: Vec::new(),
        })
    }
}

/// Each `(file, name)`'s binding, read once a resolution pass (upstream
/// `bindingMemo`).
#[derive(Default)]
struct BindingMemo {
    bindings: Mutex<HashMap<(String, String), Option<ExpoBinding>>>,
}

/// What `name` is bound to in `file` (upstream `nativeBinding`): its own
/// binding there, else through its import of a relative module, that
/// module's default export (a default import) or its named export.
fn native_binding(name: &str, file: &str, context: &dyn ResolutionContext) -> Option<ExpoBinding> {
    let memo = context
        .route_tables()
        .get_or_build(JS_NAME, BindingMemo::default);
    let key = (file.to_string(), name.to_string());
    if let Some(found) = memo
        .bindings
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
    {
        return found.clone();
    }
    let facts_of = |path: &str| {
        context
            .bridge_facts_of_files(&[path.to_string()])
            .into_iter()
            .next()
            .map(|(_, facts)| facts)
    };
    let mut found = facts_of(file).and_then(|facts| facts.expo.local(name).cloned());
    if found.is_none() {
        let language = js_language_for(file);
        let mapping = context
            .get_import_mappings(file, language)
            .into_iter()
            .find(|mapping| mapping.local_name == name);
        if let Some(mapping) = mapping.filter(|mapping| mapping.source.starts_with('.'))
            && let Some(target) = crate::import_resolver::resolve_import_path(
                &mapping.source,
                file,
                language,
                context,
            )
            && let Some(facts) = facts_of(&target)
        {
            found = if mapping.is_default {
                facts.expo.default_export.clone()
            } else {
                facts.expo.named(&mapping.exported_name).cloned()
            };
        }
    }
    memo.bindings
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key, found.clone());
    found
}

/// Expo modules JS: a JavaScript call into a native module by its binding.
pub struct ExpoModulesJsResolver;

impl FrameworkResolver for ExpoModulesJsResolver {
    fn name(&self) -> &str {
        JS_NAME
    }

    /// No file is extracted.
    fn languages(&self) -> Option<&[Language]> {
        Some(&[])
    }

    fn resolve_languages(&self) -> Option<&[Language]> {
        Some(&JS_FAMILY)
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        detect_expo(context)
    }

    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        if reference.reference_kind != EdgeKind::Calls {
            return None;
        }
        let call = member_call().captures(&reference.reference_name)?;
        let binding = native_binding(&call[1], &reference.file_path, context)?;
        let function = &call[2];
        let suffix = format!("::{}.{function}", binding.module);
        let mut declared: Vec<Arc<Node>> = context
            .get_nodes_by_name_shared(function)
            .into_iter()
            .filter(|node| {
                node.id.starts_with("expo-module:") && node.qualified_name.ends_with(&suffix)
            })
            .collect();
        declared.sort_by(|a, b| node_order(a, b));
        declared.sort_by_key(|node| node.language != Language::Swift);
        if let Some((first, rest)) = declared.split_first() {
            let mut result = ResolvedRef::new(
                reference.clone(),
                first.id.clone(),
                0.95,
                ResolvedBy::Framework,
            );
            if !rest.is_empty() {
                result = result.with_edge(EdgeShape {
                    also_targets: rest
                        .iter()
                        .map(|node| AlsoTarget {
                            target_node_id: node.id.clone(),
                            metadata: serde_json::Map::new(),
                        })
                        .collect(),
                    ..EdgeShape::default()
                });
            }
            return Some(result);
        }
        let type_name = binding.type_name.as_ref()?;
        let suffix = format!("{type_name}::{function}");
        let typed: Vec<Arc<Node>> = context
            .get_nodes_by_name_shared(function)
            .into_iter()
            .filter(|node| {
                matches!(
                    node.kind,
                    NodeKind::Method | NodeKind::Property | NodeKind::Field
                ) && node.qualified_name.ends_with(&suffix)
            })
            .collect();
        sole_node(&typed).map(|node| {
            ResolvedRef::new(
                reference.clone(),
                node.id.clone(),
                0.9,
                ResolvedBy::Framework,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::route_href::test_ctx::RouteCtx;
    use crate::types::ImportMapping;

    fn context() -> FrameworkExtractionContext {
        FrameworkExtractionContext::without_config("/p")
    }

    fn extract(path: &str, source: &str) -> Vec<Node> {
        ExpoModulesResolver
            .extract(path, source, &context())
            .expect("extraction")
            .nodes
    }

    const HAPTICS: &str = "\nimport ExpoModulesCore\n\npublic class HapticsModule: Module {\n  public func definition() -> ModuleDefinition {\n    Name(\"ExpoHaptics\")\n\n    AsyncFunction(\"notificationAsync\") { (notificationType: NotificationType) in\n      // body\n    }\n\n    AsyncFunction(\"impactAsync\") { (style: ImpactStyle) in\n      // body\n    }\n\n    Function(\"synchronousThing\") {\n      return 1\n    }\n\n    Property(\"isAvailable\") {\n      return true\n    }\n  }\n}\n";

    /// Upstream `expo-modules.test.ts` 9-82.
    #[test]
    fn a_modules_declarations_are_its_method_nodes() {
        let nodes = extract("ios/HapticsModule.swift", HAPTICS);
        let rows: Vec<(&str, &str, &str, i64, i64, i64)> = nodes
            .iter()
            .map(|node| {
                (
                    node.id.as_str(),
                    node.name.as_str(),
                    node.qualified_name.as_str(),
                    node.start_line,
                    node.start_column,
                    node.end_column,
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                (
                    "expo-module:ios/HapticsModule.swift:ExpoHaptics:notificationAsync:8",
                    "notificationAsync",
                    "ios/HapticsModule.swift::ExpoHaptics.notificationAsync",
                    8,
                    4,
                    4 + 13 + 2 + 17 + 2
                ),
                (
                    "expo-module:ios/HapticsModule.swift:ExpoHaptics:impactAsync:12",
                    "impactAsync",
                    "ios/HapticsModule.swift::ExpoHaptics.impactAsync",
                    12,
                    4,
                    4 + 13 + 2 + 11 + 2
                ),
                (
                    "expo-module:ios/HapticsModule.swift:ExpoHaptics:synchronousThing:16",
                    "synchronousThing",
                    "ios/HapticsModule.swift::ExpoHaptics.synchronousThing",
                    16,
                    4,
                    4 + 8 + 2 + 16 + 2
                ),
                (
                    "expo-module:ios/HapticsModule.swift:ExpoHaptics:isAvailable:20",
                    "isAvailable",
                    "ios/HapticsModule.swift::ExpoHaptics.isAvailable",
                    20,
                    4,
                    4 + 8 + 2 + 11 + 2
                ),
            ]
        );
        let node = &nodes[0];
        assert_eq!(node.kind, NodeKind::Method);
        assert_eq!(node.language, Language::Swift);
        assert_eq!(node.end_line, node.start_line);
        assert!(node.is_exported);
        assert_eq!(
            node.docstring.as_deref(),
            Some("Expo Modules AsyncFunction(\"notificationAsync\") in ExpoHaptics")
        );
        assert_eq!(
            node.signature.as_deref(),
            Some("AsyncFunction(\"notificationAsync\")")
        );
        // The class names the module without `Name(…)`.
        let bare = extract(
            "ios/BareModule.swift",
            "\npublic class BareModule: Module {\n  public func definition() -> ModuleDefinition {\n    Function(\"doX\") { return 1 }\n  }\n}\n",
        );
        assert_eq!(
            bare[0].qualified_name,
            "ios/BareModule.swift::BareModule.doX"
        );
        // No module, no nodes.
        assert!(extract("Helper.swift", "\nclass Helper {\n  func doX() { }\n}\n").is_empty());
        // Kotlin, a generic `AsyncFunction<T>` included.
        let kotlin = extract(
            "FooModule.kt",
            "\nclass FooModule : Module() {\n    override fun definition() = ModuleDefinition {\n        Name(\"ExpoFoo\")\n        AsyncFunction(\"doAsync\") { name: String -> name.uppercase() }\n        Function(\"doSync\") { 42 }\n        AsyncFunction<Float>(\"getBatteryLevelAsync\") { 1.0f }\n    }\n}\n",
        );
        let names: Vec<&str> = kotlin.iter().map(|node| node.name.as_str()).collect();
        assert_eq!(names, ["doAsync", "doSync", "getBatteryLevelAsync"]);
        assert!(kotlin.iter().all(|node| node.language == Language::Kotlin));
    }

    /// A column counts UTF-16 code units, and one name declared twice on a
    /// line is one node.
    #[test]
    fn columns_count_utf16_and_a_line_holds_one_declaration_of_a_name() {
        let nodes = extract(
            "ios/M.swift",
            "class M: Module {\n  /* é\u{1F600} */ Function(\"a\") { }; Function(\"a\") { }\n  Function(\"a\") { }\n}\n",
        );
        let rows: Vec<(i64, i64, i64)> = nodes
            .iter()
            .map(|node| (node.start_line, node.start_column, node.end_column))
            .collect();
        assert_eq!(
            rows,
            [(2, 12, 12 + 8 + 2 + 1 + 2), (3, 2, 2 + 8 + 2 + 1 + 2)]
        );
        // Only `Name("…")` at a word start names the module.
        assert_eq!(
            extract(
                "ios/N.swift",
                "class N: Module {\n  DisplayName(\"X\")\n  Function(\"b\") { }\n}\n"
            )[0]
            .qualified_name,
            "ios/N.swift::N.b"
        );
    }

    #[test]
    fn bindings_take_upstreams_three_patterns_on_stripped_text() {
        let bindings = expo_bindings_of(
            "import { requireNativeModule } from 'expo-modules-core';\n// const Old = requireNativeModule('Old');\nconst Haptics = requireNativeModule('ExpoHaptics');\nlet Typed: Spec = requireOptionalNativeModule<CameraNativeModule>('ExpoCamera');\nexport const Named = requireNativeModule<T, U>('Mod');\nexport default requireNativeModule<CameraNativeModule>('ExpoCamera');\n",
        );
        assert_eq!(bindings.local("Old"), None);
        assert_eq!(
            bindings.local("Haptics"),
            Some(&ExpoBinding {
                module: "ExpoHaptics".to_string(),
                type_name: None
            })
        );
        assert_eq!(
            bindings.local("Typed"),
            Some(&ExpoBinding {
                module: "ExpoCamera".to_string(),
                type_name: Some("CameraNativeModule".to_string())
            })
        );
        assert_eq!(
            bindings.named("Named"),
            Some(&ExpoBinding {
                module: "Mod".to_string(),
                type_name: Some("T".to_string())
            })
        );
        assert_eq!(
            bindings.default_export,
            Some(ExpoBinding {
                module: "ExpoCamera".to_string(),
                type_name: Some("CameraNativeModule".to_string())
            })
        );
        // A name's first declaration that binds it is its binding; a
        // declaration whose value is something else is none.
        let later = expo_bindings_of(
            "const A = 1;\nconst A = requireNativeModule('Second');\nconst B = make();\n",
        );
        assert_eq!(later.local("A").map(|b| b.module.as_str()), Some("Second"));
        assert_eq!(later.local("B"), None);
        assert!(expo_bindings_of("const A = requireModule('X')").is_empty());
        // A declaration inside another's words is read too.
        assert_eq!(
            expo_bindings_of("const const B = requireNativeModule('Inner');")
                .local("B")
                .map(|b| b.module.as_str()),
            Some("Inner")
        );
    }

    fn ref_in(file: &str, name: &str, language: Language) -> RefView {
        RefView {
            row_id: None,
            from_node_id: format!("caller:{file}"),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::Calls,
            line: 1,
            column: 0,
            file_path: file.to_string(),
            language,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    fn camera_ctx(extra_declaration: bool) -> (RouteCtx, Vec<Node>) {
        let swift = extract(
            "ios/CameraViewModule.swift",
            "import ExpoModulesCore\npublic final class CameraViewModule: Module {\n  public func definition() -> ModuleDefinition {\n    Name(\"ExpoCamera\")\n    AsyncFunction(\"launchScanner\") { (options: ScannerOptions) in }\n  }\n}\n",
        );
        let kotlin = extract(
            "android/CameraViewModule.kt",
            "class CameraViewModule : Module() {\n  override fun definition() = ModuleDefinition {\n    Name(\"ExpoCamera\")\n    AsyncFunction(\"launchScanner\") { options: ScannerOptions -> }\n  }\n}\n",
        );
        let typed = framework_node(
            "property:available".to_string(),
            NodeKind::Property,
            "isAvailableAsync".to_string(),
            "CameraNativeModule::isAvailableAsync".to_string(),
            "src/Camera.types.ts".to_string(),
            3,
            3,
            2,
            30,
            Language::TypeScript,
            false,
        );
        let mut nodes = vec![kotlin[0].clone(), swift[0].clone(), typed];
        if extra_declaration {
            let mut twin = nodes[2].clone();
            twin.id = "property:available-twin".to_string();
            twin.file_path = "src/Other.types.ts".to_string();
            nodes.push(twin);
        }
        // Another module's function of the name, which no binding of
        // `ExpoCamera` reaches.
        nodes.extend(extract(
            "ios/OtherModule.swift",
            "public class OtherModule: Module {\n  public func definition() -> ModuleDefinition {\n    Name(\"ExpoOther\")\n    AsyncFunction(\"launchScanner\") { }\n  }\n}\n",
        ));
        let mut ctx = RouteCtx::default()
            .file(
                "src/ExpoCameraManager.ts",
                "import { requireNativeModule } from 'expo';\nimport type { CameraNativeModule } from './Camera.types';\n\nexport default requireNativeModule<CameraNativeModule>('ExpoCamera');\nexport const Named = requireNativeModule('ExpoCamera');\n",
            )
            .file(
                "src/CameraView.tsx",
                "import CameraManager from './ExpoCameraManager';\nimport { Named as Cam } from './ExpoCameraManager';\n",
            )
            .import(
                "src/CameraView.tsx",
                ImportMapping {
                    local_name: "CameraManager".to_string(),
                    exported_name: "default".to_string(),
                    source: "./ExpoCameraManager".to_string(),
                    is_default: true,
                    is_namespace: false,
                },
            )
            .import(
                "src/CameraView.tsx",
                ImportMapping {
                    local_name: "Cam".to_string(),
                    exported_name: "Named".to_string(),
                    source: "./ExpoCameraManager".to_string(),
                    is_default: false,
                    is_namespace: false,
                },
            );
        let mut by_file: HashMap<String, Vec<Node>> = HashMap::new();
        for node in &nodes {
            by_file
                .entry(node.file_path.clone())
                .or_default()
                .push(node.clone());
        }
        for (file, nodes) in by_file {
            ctx = ctx.routes(&file, nodes);
        }
        (ctx, nodes)
    }

    /// Upstream `expo-modules.test.ts` 218-283: the module's declarations,
    /// Swift first and the Kotlin one beside it; else the type the binding
    /// is given.
    #[test]
    fn a_binding_reaches_its_modules_declarations_swift_first() {
        let (ctx, nodes) = camera_ctx(false);
        let result = ExpoModulesJsResolver
            .resolve(
                &ref_in(
                    "src/CameraView.tsx",
                    "CameraManager.launchScanner",
                    Language::Tsx,
                ),
                &ctx,
            )
            .expect("the module's function");
        assert_eq!(result.target_node_id, nodes[1].id, "Swift first");
        assert_eq!(result.confidence, 0.95);
        let also: Vec<&str> = result
            .edge
            .as_ref()
            .expect("an also-target")
            .also_targets
            .iter()
            .map(|target| target.target_node_id.as_str())
            .collect();
        assert_eq!(also, [nodes[0].id.as_str()]);
        assert!(result.edge.as_ref().unwrap().metadata.is_empty());
        // A named import is looked up by its exported name.
        let named = ExpoModulesJsResolver
            .resolve(
                &ref_in("src/CameraView.tsx", "Cam.launchScanner", Language::Tsx),
                &ctx,
            )
            .expect("the named export's module");
        assert_eq!(named.target_node_id, nodes[1].id);
        assert_eq!(
            named
                .edge
                .as_ref()
                .expect("an also-target")
                .also_targets
                .len(),
            1
        );
        let typed = ExpoModulesJsResolver
            .resolve(
                &ref_in(
                    "src/CameraView.tsx",
                    "CameraManager.isAvailableAsync",
                    Language::Tsx,
                ),
                &ctx,
            )
            .expect("the typed member");
        assert_eq!(
            (typed.target_node_id.as_str(), typed.confidence),
            ("property:available", 0.9)
        );
        // No binding, no answer; a non-call is not read.
        assert!(
            ExpoModulesJsResolver
                .resolve(
                    &ref_in("src/CameraView.tsx", "Other.launchScanner", Language::Tsx),
                    &ctx
                )
                .is_none()
        );
        let mut reference = ref_in(
            "src/CameraView.tsx",
            "CameraManager.launchScanner",
            Language::Tsx,
        );
        reference.reference_kind = EdgeKind::References;
        assert!(ExpoModulesJsResolver.resolve(&reference, &ctx).is_none());
        // KEEP-RUST: two typed members answer nothing.
        let (twins, _) = camera_ctx(true);
        assert!(
            ExpoModulesJsResolver
                .resolve(
                    &ref_in(
                        "src/CameraView.tsx",
                        "CameraManager.isAvailableAsync",
                        Language::Tsx
                    ),
                    &twins
                )
                .is_none()
        );
    }

    #[test]
    fn detection_reads_the_package_or_a_module_source() {
        let package = RouteCtx::default().file(
            "package.json",
            "{\"dependencies\":{\"expo-modules-core\":\"^1.0.0\"}}",
        );
        assert!(ExpoModulesResolver.detect(&package));
        assert!(ExpoModulesJsResolver.detect(&package));
        let source = RouteCtx::default().file("ios/HapticsModule.swift", HAPTICS);
        assert!(ExpoModulesResolver.detect(&source));
        let plain = RouteCtx::default().file("ios/Helper.swift", "class Helper: Module {}\n");
        assert!(!ExpoModulesResolver.detect(&plain));
        assert_eq!(
            ExpoModulesResolver.languages(),
            Some(&[Language::Swift, Language::Kotlin][..])
        );
        assert_eq!(ExpoModulesResolver.resolve_languages(), Some(&[][..]));
        assert_eq!(ExpoModulesJsResolver.languages(), Some(&[][..]));
        assert_eq!(
            ExpoModulesJsResolver.resolve_languages(),
            Some(
                &[
                    Language::JavaScript,
                    Language::TypeScript,
                    Language::Tsx,
                    Language::Jsx,
                ][..]
            )
        );
    }

    /// `Constants("…")` declares as the others do, a single-quoted name
    /// too; a declaration without `class X : Module` is no module's.
    #[test]
    fn constants_and_single_quoted_names_declare_and_a_module_needs_its_class() {
        let nodes = extract(
            "ios/C.swift",
            "class C: Module {\n  Constants(\"PI\")\n  Function('single') { }\n}\n",
        );
        let names: Vec<&str> = nodes.iter().map(|node| node.name.as_str()).collect();
        assert_eq!(names, ["PI", "single"]);
        assert_eq!(
            nodes[0].docstring.as_deref(),
            Some("Expo Modules Constants(\"PI\") in C")
        );
        assert!(
            extract(
                "ios/NoClass.swift",
                "func definition() {\n  Function(\"x\") { }\n}\n"
            )
            .is_empty()
        );
    }

    /// Detection reads a Kotlin module source too, and only the first 200
    /// indexed paths.
    #[test]
    fn detection_reads_a_kotlin_source_within_the_first_two_hundred_paths() {
        let kotlin = "class FooModule : Module() {\n  override fun definition() = ModuleDefinition {\n    Function(\"go\") { }\n  }\n}\n";
        assert!(
            ExpoModulesResolver.detect(&RouteCtx::default().file("android/FooModule.kt", kotlin))
        );
        let mut late = RouteCtx::default();
        for at in 0..200 {
            late = late.file(&format!("a/{at:03}.swift"), "");
        }
        late = late.file("z.kt", kotlin);
        assert!(!ExpoModulesResolver.detect(&late));
    }

    /// A binding in the call's own file, typed: its type's method, field or
    /// property of the name, not another type's.
    #[test]
    fn an_own_files_typed_binding_reaches_its_types_member_of_any_kind() {
        let member = |id: &str, kind: NodeKind, name: &str, owner: &str| {
            framework_node(
                id.to_string(),
                kind,
                name.to_string(),
                format!("{owner}::{name}"),
                "src/Cam.types.ts".to_string(),
                3,
                3,
                2,
                30,
                Language::TypeScript,
                false,
            )
        };
        let nodes = [
            member("method:snap", NodeKind::Method, "snap", "CamType"),
            member("field:zoom", NodeKind::Field, "zoom", "CamType"),
            member("property:flash", NodeKind::Property, "flash", "CamType"),
            member("method:other-snap", NodeKind::Method, "snap", "OtherType"),
            member(
                "method:other-zoom",
                NodeKind::Method,
                "zoom",
                "CamTypeHelper",
            ),
        ];
        let ctx = RouteCtx::default()
            .file(
                "src/a.ts",
                "import { requireNativeModule } from 'expo';\nconst Cam = requireNativeModule<CamType>('ExpoCam');\n",
            )
            .routes("src/Cam.types.ts", nodes.to_vec());
        for (call, expected) in [
            ("Cam.snap", "method:snap"),
            ("Cam.zoom", "field:zoom"),
            ("Cam.flash", "property:flash"),
        ] {
            let result = ExpoModulesJsResolver
                .resolve(&ref_in("src/a.ts", call, Language::TypeScript), &ctx)
                .expect(call);
            assert_eq!(
                (result.target_node_id.as_str(), result.confidence),
                (expected, 0.9),
                "{call}"
            );
        }
    }

    /// The memo is per file: one name bound to two modules in two files
    /// reaches each file's module.
    #[test]
    fn one_name_bound_in_two_files_reaches_each_files_module() {
        let x = extract(
            "ios/XModule.swift",
            "class XModule: Module {\n  Name(\"ExpoX\")\n  Function(\"go\") { }\n}\n",
        );
        let y = extract(
            "ios/YModule.swift",
            "class YModule: Module {\n  Name(\"ExpoY\")\n  Function(\"go\") { }\n}\n",
        );
        let ctx = RouteCtx::default()
            .file("src/x.ts", "const M = requireNativeModule('ExpoX');\n")
            .file("src/y.ts", "const M = requireNativeModule('ExpoY');\n")
            .routes("ios/XModule.swift", x.clone())
            .routes("ios/YModule.swift", y.clone());
        let target = |file: &str| {
            ExpoModulesJsResolver
                .resolve(&ref_in(file, "M.go", Language::TypeScript), &ctx)
                .map(|result| result.target_node_id)
        };
        assert_eq!(target("src/x.ts"), Some(x[0].id.clone()));
        assert_eq!(target("src/y.ts"), Some(y[0].id.clone()));
    }
}
