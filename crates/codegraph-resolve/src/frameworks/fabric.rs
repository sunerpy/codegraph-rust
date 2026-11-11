//! React Native view components (upstream `resolution/frameworks/fabric.ts`
//! at `e8ddc4e2`: `4d1a2b3c` #430, `07af3db6`'s Paper and Android view
//! managers, `6f798dce`'s `requireNativeComponent` modules).
//!
//! A project is React Native's when its root `package.json` names
//! `react-native`, or the `package.json` of a directory one level below
//! `packages/`, `apps/`, `modules/` or `libraries/` does. The resolver
//! extracts what a view component declares and resolves no reference:
//!
//! - a JavaScript-family spec (`codegenNativeComponent<P>('Name')` or
//!   `requireNativeComponent('Name')`): one `component` node per call, and
//!   one `property` node per prop of its `NativeProps` interface;
//! - an Objective-C Paper view manager (`@implementation XManager` writing
//!   `RCT_EXPORT_VIEW_PROPERTY`, `RCT_CUSTOM_VIEW_PROPERTY` or
//!   `RCT_REMAP_VIEW_PROPERTY`): the component the class name names and a
//!   `property` node per prop;
//! - a Java or Kotlin view manager (`class XManager` with `@ReactProp`s):
//!   the same.
//!
//! The ids start `fabric-component:` and `fabric-prop:`; the synthesis
//! stage's `fabricNativeEdges` joins a component to its native view class.

use std::sync::OnceLock;

use codegraph_core::types::{Language, Node, NodeKind};
use regex::Regex;

use super::framework_node;
use super::js_scan::{is_js_space, multiline_matches};
use super::react_native::js_regex;
use crate::framework::{FrameworkExtractionContext, FrameworkResolver};
use crate::types::{FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedRef};

/// The resolver's name.
pub const NAME: &str = "fabric-view";

/// The languages whose files it extracts.
const LANGUAGES: [Language; 7] = [
    Language::TypeScript,
    Language::Tsx,
    Language::JavaScript,
    Language::Jsx,
    Language::ObjC,
    Language::Java,
    Language::Kotlin,
];

/// The workspace roots whose packages detection reads, one level deep
/// (upstream's monorepo escape hatch).
pub const WORKSPACE_ROOTS: [&str; 4] = ["packages", "apps", "modules", "libraries"];

macro_rules! patterns {
    ($($(#[$doc:meta])* $name:ident = $pattern:expr;)*) => {
        $(
            $(#[$doc])*
            fn $name() -> &'static Regex {
                static PATTERN: OnceLock<Regex> = OnceLock::new();
                PATTERN.get_or_init(|| js_regex($pattern))
            }
        )*
    };
}

patterns! {
    /// `codegenNativeComponent<P>('Name')` or `requireNativeComponent('Name')`
    /// (upstream `CODEGEN_DECL_RE`).
    codegen_decl = r#"(?-u:\b)(codegenNativeComponent|requireNativeComponent){S}*(?:<[^>]+>)?{S}*\({S}*['"]([A-Za-z_][A-Za-z0-9_]*)['"]"#;
    /// A Paper view-property macro and its JavaScript name (upstream
    /// `RCT_VIEW_PROP_RE`).
    rct_view_prop = r"(?-u:\b)RCT_(?:EXPORT|CUSTOM|REMAP)_VIEW_PROPERTY{S}*\({S}*([A-Za-z_][A-Za-z0-9_]*)";
    /// The `@implementation` class (upstream `OBJC_IMPL_RE`).
    objc_impl = r"@implementation{S}+([A-Za-z_][A-Za-z0-9_]*)";
    /// A JVM file's first class.
    jvm_class = r"(?-u:\b)class{S}+([A-Za-z_][A-Za-z0-9_]*)(?-u:\b)";
    /// `@ReactProp("name")` or `@ReactProp(name = "name")`.
    react_prop = r#"@ReactProp{S}*\({S}*(?:name{S}*={S}*)?"([^"]+)""#;
    /// `export interface NativeProps … {…\n}` (upstream
    /// `findNativePropsBody`).
    native_props = r"export{S}+interface{S}+NativeProps(?-u:\b)[^{]*\{((?s:.)*?)\n\}";
    /// A prop at a line start: `name?:` (upstream `extractPropNames`).
    prop_at = r"^{S}*([A-Za-z_][A-Za-z0-9_]*)\??{S}*:";
    /// A JavaScript-family spec path (upstream `/\.(?:[cm]?[jt]s|[jt]sx)$/`).
    spec_path = r"\.(?:[cm]?[jt]s|[jt]sx)$";
    /// A `package.json` dependency on React Native.
    react_native_dependency = r#"["']react-native["']{S}*:"#;
    /// A TypeScript path (upstream `/\.[cm]?ts$/`).
    typescript_path = r"\.[cm]?ts$";
}

/// The UTF-16 length of `text`.
fn utf16_len(text: &str) -> i64 {
    text.encode_utf16().count() as i64
}

/// The 1-based line of a byte offset.
fn line_of(source: &str, offset: usize) -> i64 {
    1 + source[..offset].matches('\n').count() as i64
}

/// The JavaScript component name a native view manager's class gives
/// (upstream `deriveComponentNameFromManager`): no leading `RCT`, then no
/// trailing `ViewManager`, else no trailing `Manager`.
fn component_name_from_manager(class_name: &str) -> &str {
    let name = class_name.strip_prefix("RCT").unwrap_or(class_name);
    name.strip_suffix("ViewManager")
        .or_else(|| name.strip_suffix("Manager"))
        .unwrap_or(name)
}

/// A spec's language by its extension (upstream: a Paper module is often
/// Flow `.js`).
fn spec_language(file_path: &str) -> Language {
    if file_path.ends_with(".tsx") {
        Language::Tsx
    } else if typescript_path().is_match(file_path) {
        Language::TypeScript
    } else if file_path.ends_with(".jsx") {
        Language::Jsx
    } else {
        Language::JavaScript
    }
}

/// Whether the 80 UTF-16 code units after a prop's colon open with
/// optional space and `(`: a method's shape, not a prop's (upstream's
/// `after` test).
fn opens_method_shape(rest: &str) -> bool {
    let mut units = 0;
    for ch in rest.chars() {
        units += ch.len_utf16();
        if units > 80 {
            return false;
        }
        if ch == '(' {
            return true;
        }
        if !is_js_space(ch) {
            return false;
        }
    }
    false
}

/// The prop names of a `NativeProps` body, in order (upstream
/// `extractPropNames`): each line-start `name?:` whose type does not open
/// like a method's.
fn prop_names(body: &str) -> Vec<&str> {
    multiline_matches(prop_at(), body)
        .into_iter()
        .filter_map(|(start, found)| {
            let end = start + found.get(0).expect("a prop").end();
            (!opens_method_shape(&body[end..])).then(|| found.get(1).expect("a name").as_str())
        })
        .collect()
}

/// A JavaScript-family spec's component and prop nodes (upstream
/// `extractFabricNodes`).
fn spec_nodes(file_path: &str, source: &str) -> Vec<Node> {
    if !source.contains("codegenNativeComponent") && !source.contains("requireNativeComponent") {
        return Vec::new();
    }
    let mut nodes = Vec::new();
    for found in codegen_decl().captures_iter(source) {
        let declared_with = &found[1];
        let component = &found[2];
        let start = found.get(0).expect("a declaration").start();
        let before = &source[..start];
        let line = line_of(source, start);
        let column = utf16_len(before.rsplit('\n').next().unwrap_or(before));
        let codegen = declared_with == "codegenNativeComponent";
        let mut node = framework_node(
            format!("fabric-component:{file_path}:{component}:{line}"),
            NodeKind::Component,
            component.to_string(),
            format!("{file_path}::{component}"),
            file_path.to_string(),
            line,
            line,
            column,
            column + utf16_len(declared_with),
            spec_language(file_path),
            true,
        );
        node.docstring = Some(if codegen {
            format!("Fabric/Codegen native component '{component}'")
        } else {
            format!("Native component '{component}' (requireNativeComponent)")
        });
        node.signature = Some(if codegen {
            format!("codegenNativeComponent<NativeProps>('{component}')")
        } else {
            format!("requireNativeComponent('{component}')")
        });
        nodes.push(node);
    }
    if let Some(body) = native_props()
        .captures(source)
        .and_then(|found| found.get(1))
        .map(|body| body.as_str())
    {
        let language = if file_path.ends_with(".tsx") {
            Language::Tsx
        } else {
            Language::TypeScript
        };
        // Upstream finds each prop's line by text: the first occurrence of
        // its name after the first occurrence of the body's text.
        let body_at = source.find(body).unwrap_or(0);
        for prop in prop_names(body) {
            let line = source[body_at..]
                .find(prop)
                .map_or(1, |at| line_of(source, body_at + at));
            let mut node = framework_node(
                format!("fabric-prop:{file_path}:{prop}:{line}"),
                NodeKind::Property,
                prop.to_string(),
                format!("{file_path}::NativeProps.{prop}"),
                file_path.to_string(),
                line,
                line,
                0,
                utf16_len(prop),
                language,
                true,
            );
            node.docstring = Some(format!("Fabric NativeProps prop '{prop}'"));
            nodes.push(node);
        }
    }
    nodes
}

/// A view manager's component node and one `property` node per prop the
/// matches of `props` name, the first of each name (upstream's two
/// view-manager extractors share this shape).
#[allow(clippy::too_many_arguments)]
fn manager_nodes(
    file_path: &str,
    source: &str,
    class_name: &str,
    class_at: usize,
    language: Language,
    component_doc: String,
    component_signature: String,
    props: &Regex,
    prop_doc: &str,
) -> Vec<Node> {
    let component = component_name_from_manager(class_name);
    if component.is_empty() {
        return Vec::new();
    }
    let line = line_of(source, class_at);
    let mut node = framework_node(
        format!("fabric-component:{file_path}:{component}:{line}"),
        NodeKind::Component,
        component.to_string(),
        format!("{file_path}::{component}"),
        file_path.to_string(),
        line,
        line,
        0,
        utf16_len(component),
        language,
        true,
    );
    node.docstring = Some(component_doc.replace("{component}", component));
    node.signature = Some(component_signature);
    let mut nodes = vec![node];
    let mut seen: Vec<&str> = Vec::new();
    for found in props.captures_iter(source) {
        let prop = found.get(1).expect("a prop").as_str();
        if seen.contains(&prop) {
            continue;
        }
        seen.push(prop);
        let line = line_of(source, found.get(0).expect("a prop").start());
        let mut node = framework_node(
            format!("fabric-prop:{file_path}:{prop}:{line}"),
            NodeKind::Property,
            prop.to_string(),
            format!("{file_path}::{component}.{prop}"),
            file_path.to_string(),
            line,
            line,
            0,
            utf16_len(prop),
            language,
            true,
        );
        node.docstring = Some(format!("{prop_doc} '{prop}' on {component}"));
        nodes.push(node);
    }
    nodes
}

/// An Objective-C Paper view manager's nodes (upstream
/// `extractLegacyViewManagerNodes`).
fn paper_manager_nodes(file_path: &str, source: &str) -> Vec<Node> {
    if !source.contains("RCT_EXPORT_VIEW_PROPERTY")
        && !source.contains("RCT_CUSTOM_VIEW_PROPERTY")
        && !source.contains("RCT_REMAP_VIEW_PROPERTY")
    {
        return Vec::new();
    }
    let Some(found) = objc_impl().captures(source) else {
        return Vec::new();
    };
    let class = found.get(1).expect("a class").as_str();
    if !class.ends_with("Manager") {
        return Vec::new();
    }
    manager_nodes(
        file_path,
        source,
        class,
        found.get(0).expect("an implementation").start(),
        Language::ObjC,
        format!(
            "Legacy Paper ViewManager component '{{component}}' (from @implementation {class})"
        ),
        format!("RCT_EXPORT_MODULE() // ViewManager: {class}"),
        rct_view_prop(),
        "Legacy Paper view prop",
    )
}

/// A Java or Kotlin view manager's nodes (upstream
/// `extractJvmViewManagerNodes`).
fn jvm_manager_nodes(file_path: &str, source: &str) -> Vec<Node> {
    if !source.contains("@ReactProp") {
        return Vec::new();
    }
    let Some(found) = jvm_class().captures(source) else {
        return Vec::new();
    };
    let class = found.get(1).expect("a class").as_str();
    if !class.ends_with("Manager") {
        return Vec::new();
    }
    let language = if file_path.ends_with(".kt") {
        Language::Kotlin
    } else {
        Language::Java
    };
    manager_nodes(
        file_path,
        source,
        class,
        found.get(0).expect("a class").start(),
        language,
        format!("Android view-manager component '{{component}}' (from class {class})"),
        format!("class {class} : ViewManager"),
        react_prop(),
        "Android @ReactProp prop",
    )
}

/// The nodes a file's view components declare, by its extension (upstream
/// `extract`'s dispatch).
pub fn extract_view_nodes(file_path: &str, source: &str) -> Vec<Node> {
    if spec_path().is_match(file_path) {
        spec_nodes(file_path, source)
    } else if file_path.ends_with(".m") || file_path.ends_with(".mm") {
        paper_manager_nodes(file_path, source)
    } else if file_path.ends_with(".java") || file_path.ends_with(".kt") {
        jvm_manager_nodes(file_path, source)
    } else {
        Vec::new()
    }
}

/// What detection reads below the workspace roots of the project at
/// `project_root`, which no index holds: each [`WORKSPACE_ROOTS`] root and
/// the directories it lists, sorted by name (the sync's control input,
/// W7-21). A root that is no directory lists none.
pub fn workspace_listings(project_root: &str) -> Vec<(&'static str, Vec<String>)> {
    WORKSPACE_ROOTS
        .iter()
        .map(|root| {
            (
                *root,
                super::cargo_workspace::list_directories_within(project_root, root),
            )
        })
        .collect()
}

/// The resolver.
pub struct FabricViewResolver;

impl FrameworkResolver for FabricViewResolver {
    fn name(&self) -> &str {
        NAME
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&LANGUAGES)
    }

    /// No reference is read.
    fn resolve_languages(&self) -> Option<&[Language]> {
        Some(&[])
    }

    /// The root `package.json` naming `react-native`, else a workspace
    /// package's, one level below a [`WORKSPACE_ROOTS`] directory.
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        let names_react_native = |path: &str| {
            context
                .read_file_shared(path)
                .is_some_and(|package| react_native_dependency().is_match(&package))
        };
        names_react_native("package.json")
            || WORKSPACE_ROOTS.iter().any(|root| {
                context
                    .list_directories(root)
                    .iter()
                    .any(|sub| names_react_native(&format!("{root}/{sub}/package.json")))
            })
    }

    fn resolve(
        &self,
        _reference: &RefView,
        _context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        None
    }

    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        Some(FrameworkResolverExtractionResult {
            nodes: extract_view_nodes(file_path, content),
            references: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::route_href::test_ctx::RouteCtx;

    fn rows(nodes: &[Node]) -> Vec<(String, NodeKind, String, Language, i64, i64, i64)> {
        nodes
            .iter()
            .map(|node| {
                (
                    node.id.clone(),
                    node.kind,
                    node.qualified_name.clone(),
                    node.language,
                    node.start_line,
                    node.start_column,
                    node.end_column,
                )
            })
            .collect()
    }

    const SPEC: &str = "import { codegenNativeComponent } from 'react-native';\nimport type { ViewProps, CodegenTypes as CT } from 'react-native';\n\nexport interface NativeProps extends ViewProps {\n  color?: ColorValue;\n  onTap?: CT.DirectEventHandler<TapEvent>;\n  measure(arg: number): void;\n  onClose?: () => void;\n}\n\nexport default codegenNativeComponent<NativeProps>('MyView');\n";

    /// Upstream `fabric-view.test.ts`: the component and its props.
    #[test]
    fn a_codegen_spec_declares_its_component_and_props() {
        let nodes = extract_view_nodes("src/MyViewNativeComponent.ts", SPEC);
        let column = 15;
        assert_eq!(
            rows(&nodes),
            [
                (
                    "fabric-component:src/MyViewNativeComponent.ts:MyView:11".to_string(),
                    NodeKind::Component,
                    "src/MyViewNativeComponent.ts::MyView".to_string(),
                    Language::TypeScript,
                    11,
                    column,
                    column + 22
                ),
                (
                    "fabric-prop:src/MyViewNativeComponent.ts:color:5".to_string(),
                    NodeKind::Property,
                    "src/MyViewNativeComponent.ts::NativeProps.color".to_string(),
                    Language::TypeScript,
                    5,
                    0,
                    5
                ),
                (
                    "fabric-prop:src/MyViewNativeComponent.ts:onTap:6".to_string(),
                    NodeKind::Property,
                    "src/MyViewNativeComponent.ts::NativeProps.onTap".to_string(),
                    Language::TypeScript,
                    6,
                    0,
                    5
                ),
            ]
        );
        // `onClose?: () => void` opens like a method after its colon, so
        // upstream's test skips it with `measure(…)`.
        assert!(nodes.iter().all(|node| node.name != "onClose"));
        assert_eq!(
            nodes[0].docstring.as_deref(),
            Some("Fabric/Codegen native component 'MyView'")
        );
        assert_eq!(
            nodes[0].signature.as_deref(),
            Some("codegenNativeComponent<NativeProps>('MyView')")
        );
        assert_eq!(
            nodes[1].docstring.as_deref(),
            Some("Fabric NativeProps prop 'color'")
        );
        assert!(nodes.iter().all(|node| node.is_exported));
        // A `.tsx` spec's nodes are TSX; a `.js` module's component is
        // JavaScript and its props TypeScript, as upstream's.
        let tsx = extract_view_nodes("src/V.tsx", SPEC);
        assert_eq!(tsx.len(), 3);
        assert!(tsx.iter().all(|node| node.language == Language::Tsx));
        // A `.jsx` spec's component is JSX, its props TypeScript.
        let jsx = extract_view_nodes("src/V.jsx", SPEC);
        assert_eq!(jsx.len(), 3);
        assert_eq!(jsx[0].language, Language::Jsx);
        assert_eq!(jsx[1].language, Language::TypeScript);
        let js = extract_view_nodes("src/V.js", SPEC);
        assert_eq!(js[0].language, Language::JavaScript);
        assert_eq!(js[1].language, Language::TypeScript);
        // No spec call, no nodes.
        assert!(
            extract_view_nodes(
                "src/Plain.ts",
                "export interface NativeProps {\n  a: 1;\n}\n"
            )
            .is_empty()
        );
    }

    /// Upstream `fabric-view.test.ts` 9-52: the props, a spec without
    /// `NativeProps`, and a file without a spec; a prop declared twice is
    /// two nodes of one id, the later of which the store keeps.
    #[test]
    fn upstream_spec_cases_and_a_repeated_prop() {
        let source = "\n'use client';\nimport { codegenNativeComponent } from 'react-native';\nimport type { ViewProps, CodegenTypes as CT, ColorValue } from 'react-native';\n\ntype TapEvent = Readonly<{ x: number; y: number }>;\n\nexport interface NativeProps extends ViewProps {\n  color?: ColorValue;\n  onTap?: CT.DirectEventHandler<TapEvent>;\n  caption?: string;\n}\n\nexport default codegenNativeComponent<NativeProps>('MyView', {});\n";
        let nodes = extract_view_nodes("src/MyViewNativeComponent.ts", source);
        let components: Vec<&str> = nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Component)
            .map(|node| node.name.as_str())
            .collect();
        assert_eq!(components, ["MyView"]);
        let mut props: Vec<&str> = nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Property)
            .map(|node| node.name.as_str())
            .collect();
        props.sort_unstable();
        assert_eq!(props, ["caption", "color", "onTap"]);
        assert!(extract_view_nodes("plain.ts", "export const x = 1;").is_empty());
        let bare = extract_view_nodes(
            "Bare.ts",
            "\nimport { codegenNativeComponent } from 'react-native';\nexport default codegenNativeComponent('BareComponent');\n",
        );
        assert_eq!(
            bare.iter()
                .map(|node| (node.kind, node.name.as_str()))
                .collect::<Vec<_>>(),
            [(NodeKind::Component, "BareComponent")]
        );
        let twice = extract_view_nodes(
            "V.ts",
            "codegenNativeComponent('V');\nexport interface NativeProps {\n  color?: string;\n  color?: number;\n}\n",
        );
        let ids: Vec<&str> = twice
            .iter()
            .filter(|node| node.kind == NodeKind::Property)
            .map(|node| node.id.as_str())
            .collect();
        assert_eq!(
            ids,
            ["fabric-prop:V.ts:color:3", "fabric-prop:V.ts:color:3"]
        );
    }

    /// Upstream `6f798dce`: segmented-control's Paper module, a Flow `.js`
    /// file whose component is `requireNativeComponent`'s.
    #[test]
    fn a_paper_module_names_its_native_component() {
        let nodes = extract_view_nodes(
            "js/RNCSegmentedControlNativeComponent.js",
            "'use strict';\nconst { requireNativeComponent } = require('react-native');\n\nmodule.exports = requireNativeComponent('RNCSegmentedControl');\n",
        );
        assert_eq!(
            rows(&nodes),
            [(
                "fabric-component:js/RNCSegmentedControlNativeComponent.js:RNCSegmentedControl:4"
                    .to_string(),
                NodeKind::Component,
                "js/RNCSegmentedControlNativeComponent.js::RNCSegmentedControl".to_string(),
                Language::JavaScript,
                4,
                17,
                17 + 22
            )]
        );
        assert_eq!(
            nodes[0].docstring.as_deref(),
            Some("Native component 'RNCSegmentedControl' (requireNativeComponent)")
        );
        assert_eq!(
            nodes[0].signature.as_deref(),
            Some("requireNativeComponent('RNCSegmentedControl')")
        );
    }

    /// A prop's column counts UTF-16 units, a method-shaped line is no
    /// prop, and a `(` past 80 units does not make one.
    #[test]
    fn props_skip_method_shapes_within_eighty_units() {
        let far = " ".repeat(79);
        let source = format!(
            "codegenNativeComponent('V');\nexport interface NativeProps {{\n  near: ({far}x: 1) => void;\n  far:{far}(x);\n  m (x): void;\n}}\n"
        );
        let names: Vec<String> = extract_view_nodes("V.ts", &source)
            .into_iter()
            .filter(|node| node.kind == NodeKind::Property)
            .map(|node| node.name)
            .collect();
        // `near: (` opens within the window; `far:` + 79 spaces + `(` is
        // 80 units, inside it too; `m (x)` is no `name:` at all.
        assert_eq!(names, Vec::<String>::new());
        let past = format!(
            "codegenNativeComponent('V');\nexport interface NativeProps {{\n  past:{far} (x);\n}}\n"
        );
        let names: Vec<String> = extract_view_nodes("V.ts", &past)
            .into_iter()
            .filter(|node| node.kind == NodeKind::Property)
            .map(|node| node.name)
            .collect();
        assert_eq!(names, ["past"]);
        let wide = extract_view_nodes("V.ts", "/* é😀 */ codegenNativeComponent('W');\n");
        assert_eq!((wide[0].start_column, wide[0].end_column), (10, 10 + 22));
    }

    /// Upstream `fabric-view.test.ts`'s Paper and Android view managers.
    #[test]
    fn view_managers_declare_their_component_and_props() {
        let paper = extract_view_nodes(
            "ios/RNCMyViewManager.m",
            "#import <React/RCTViewManager.h>\n\n@implementation RNCMyViewManager\n\nRCT_EXPORT_MODULE()\n\nRCT_EXPORT_VIEW_PROPERTY(values, NSArray)\nRCT_CUSTOM_VIEW_PROPERTY(text, NSString, RNCMyView) {}\nRCT_REMAP_VIEW_PROPERTY(jsName, nativeKeyPath, NSString)\nRCT_EXPORT_VIEW_PROPERTY(values, NSArray)\n\n@end\n",
        );
        assert_eq!(
            rows(&paper),
            [
                (
                    "fabric-component:ios/RNCMyViewManager.m:RNCMy:3".to_string(),
                    NodeKind::Component,
                    "ios/RNCMyViewManager.m::RNCMy".to_string(),
                    Language::ObjC,
                    3,
                    0,
                    5
                ),
                (
                    "fabric-prop:ios/RNCMyViewManager.m:values:7".to_string(),
                    NodeKind::Property,
                    "ios/RNCMyViewManager.m::RNCMy.values".to_string(),
                    Language::ObjC,
                    7,
                    0,
                    6
                ),
                (
                    "fabric-prop:ios/RNCMyViewManager.m:text:8".to_string(),
                    NodeKind::Property,
                    "ios/RNCMyViewManager.m::RNCMy.text".to_string(),
                    Language::ObjC,
                    8,
                    0,
                    4
                ),
                (
                    "fabric-prop:ios/RNCMyViewManager.m:jsName:9".to_string(),
                    NodeKind::Property,
                    "ios/RNCMyViewManager.m::RNCMy.jsName".to_string(),
                    Language::ObjC,
                    9,
                    0,
                    6
                ),
            ]
        );
        assert_eq!(
            paper[0].docstring.as_deref(),
            Some(
                "Legacy Paper ViewManager component 'RNCMy' (from @implementation RNCMyViewManager)"
            )
        );
        assert_eq!(
            paper[0].signature.as_deref(),
            Some("RCT_EXPORT_MODULE() // ViewManager: RNCMyViewManager")
        );
        assert_eq!(
            paper[1].docstring.as_deref(),
            Some("Legacy Paper view prop 'values' on RNCMy")
        );
        assert_eq!(paper[1].signature, None);
        // `ViewManager` goes before `Manager` (`RNCMyViewManager` is `RNCMy`),
        // and a leading `RCT` too: `RCTFooViewManager` is `Foo`.
        let rct = extract_view_nodes(
            "ios/RCTFooViewManager.mm",
            "@implementation RCTFooViewManager\nRCT_EXPORT_VIEW_PROPERTY(a, BOOL)\n@end\n",
        );
        assert_eq!(rct[0].name, "Foo");
        // A class not named a manager, or none, is not one.
        assert!(
            extract_view_nodes(
                "ios/Thing.m",
                "@implementation Thing\nRCT_EXPORT_VIEW_PROPERTY(a, BOOL)\n@end\n"
            )
            .is_empty()
        );
        assert!(
            extract_view_nodes("ios/NoMacro.m", "@implementation NoMacroManager\n@end\n")
                .is_empty()
        );
        // `RCTManager` names no component.
        assert!(
            extract_view_nodes(
                "ios/RCTManager.m",
                "@implementation RCTManager\nRCT_EXPORT_VIEW_PROPERTY(a, BOOL)\n@end\n"
            )
            .is_empty()
        );
        let kotlin = extract_view_nodes(
            "android/MyViewManager.kt",
            "class MyViewManager : SimpleViewManager<MyView>() {\n  @ReactProp(name = \"color\")\n  fun setColor(view: MyView, color: Int) {}\n  @ReactProp(\"size\") fun setSize(view: MyView, size: Float) {}\n  @ReactProp(name = \"color\") fun again() {}\n}\n",
        );
        assert_eq!(
            rows(&kotlin),
            [
                (
                    "fabric-component:android/MyViewManager.kt:My:1".to_string(),
                    NodeKind::Component,
                    "android/MyViewManager.kt::My".to_string(),
                    Language::Kotlin,
                    1,
                    0,
                    2
                ),
                (
                    "fabric-prop:android/MyViewManager.kt:color:2".to_string(),
                    NodeKind::Property,
                    "android/MyViewManager.kt::My.color".to_string(),
                    Language::Kotlin,
                    2,
                    0,
                    5
                ),
                (
                    "fabric-prop:android/MyViewManager.kt:size:4".to_string(),
                    NodeKind::Property,
                    "android/MyViewManager.kt::My.size".to_string(),
                    Language::Kotlin,
                    4,
                    0,
                    4
                ),
            ]
        );
        assert_eq!(
            kotlin[0].docstring.as_deref(),
            Some("Android view-manager component 'My' (from class MyViewManager)")
        );
        assert_eq!(
            kotlin[0].signature.as_deref(),
            Some("class MyViewManager : ViewManager")
        );
        assert_eq!(
            kotlin[1].docstring.as_deref(),
            Some("Android @ReactProp prop 'color' on My")
        );
        let java = extract_view_nodes(
            "android/BarManager.java",
            "public class BarManager extends ViewManager<Bar, Shadow> {\n  @ReactProp(name = \"x\") public void setX(Bar v, int x) {}\n}\n",
        );
        assert_eq!(java[0].language, Language::Java);
        assert_eq!(java[0].name, "Bar");
        // The first class decides: a helper class first is no manager.
        assert!(
            extract_view_nodes(
                "android/Helper.kt",
                "class Helper\nclass BazManager { @ReactProp(\"a\") fun a() {} }\n"
            )
            .is_empty()
        );
    }

    #[test]
    fn detection_reads_the_root_package_or_a_workspace_packages() {
        let root = RouteCtx::default().file(
            "package.json",
            "{\"dependencies\": {\"react-native\": \"0.74.0\"}}",
        );
        assert!(FabricViewResolver.detect(&root));
        let workspace = RouteCtx::default().dirs("apps", &["mobile"]).unlisted(
            "apps/mobile/package.json",
            "{\"dependencies\": {'react-native' : \"0.74.0\"}}",
        );
        assert!(FabricViewResolver.detect(&workspace));
        assert_eq!(
            workspace.listed(),
            ["packages", "apps"],
            "the roots are read in order until one holds a package"
        );
        let none = RouteCtx::default().dirs("packages", &["web"]).unlisted(
            "packages/web/package.json",
            "{\"dependencies\": {\"react\": \"18\"}}",
        );
        assert!(!FabricViewResolver.detect(&none));
        assert_eq!(none.listed(), ["packages", "apps", "modules", "libraries"]);
        for root in ["packages", "modules", "libraries"] {
            let found = RouteCtx::default().dirs(root, &["native"]).unlisted(
                &format!("{root}/native/package.json"),
                "{\"dependencies\": {\"react-native\": \"0.74.0\"}}",
            );
            assert!(FabricViewResolver.detect(&found), "{root}");
        }
        assert_eq!(
            FabricViewResolver.languages(),
            Some(
                &[
                    Language::TypeScript,
                    Language::Tsx,
                    Language::JavaScript,
                    Language::Jsx,
                    Language::ObjC,
                    Language::Java,
                    Language::Kotlin,
                ][..]
            )
        );
        assert_eq!(FabricViewResolver.resolve_languages(), Some(&[][..]));
    }

    /// A prop's line is its name's first occurrence after the body's
    /// start, not an earlier mention.
    #[test]
    fn a_props_line_is_read_from_the_bodys_start() {
        let nodes = extract_view_nodes(
            "V.ts",
            "// color comes from the theme\ncodegenNativeComponent('V');\nexport interface NativeProps {\n  color?: string;\n}\n",
        );
        let props: Vec<(&str, i64)> = nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Property)
            .map(|node| (node.name.as_str(), node.start_line))
            .collect();
        assert_eq!(props, [("color", 4)]);
    }
}
