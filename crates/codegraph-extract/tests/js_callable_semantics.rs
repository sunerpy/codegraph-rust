//! JS/TS callable-completeness regressions from the post-v1.6 upstream audit.

use codegraph_core::types::{EdgeKind, ExtractionResult, Language, Node, NodeKind};
use codegraph_extract::extract_source;

fn extract(file: &str, source: &str, language: Language) -> ExtractionResult {
    let result = extract_source(file, source, Some(language));
    assert!(
        result.errors.is_empty(),
        "extraction errors: {:?}",
        result.errors
    );
    result
}

fn node<'a>(result: &'a ExtractionResult, kind: NodeKind, name: &str) -> &'a Node {
    result
        .nodes
        .iter()
        .find(|node| node.kind == kind && node.name == name)
        .unwrap_or_else(|| panic!("missing {kind:?} {name}; nodes={:#?}", result.nodes))
}

fn function_names(result: &ExtractionResult) -> Vec<&str> {
    let mut names: Vec<_> = result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Function)
        .map(|node| node.name.as_str())
        .collect();
    names.sort_unstable();
    names
}

fn calls_from<'a>(result: &'a ExtractionResult, source: &str) -> Vec<&'a str> {
    result
        .unresolved_references
        .iter()
        .filter(|reference| {
            reference.from_node_id == source && reference.reference_kind == EdgeKind::Calls
        })
        .map(|reference| reference.reference_name.as_str())
        .collect()
}

#[test]
fn ts_and_js_generators_are_callable_nodes() {
    let source = r#"
function plain() { return 1; }
function* gen() { yield 2; }
async function* asyncGen() { yield 3; }
const assigned = function* () { yield 4; };
export const exportedGen = async function* () { yield 5; };
"#;

    for (file, language) in [
        ("gens.ts", Language::TypeScript),
        ("gens.js", Language::JavaScript),
    ] {
        let result = extract(file, source, language);
        assert_eq!(
            function_names(&result),
            vec!["assigned", "asyncGen", "exportedGen", "gen", "plain"]
        );
        assert!(node(&result, NodeKind::Function, "exportedGen").is_exported);
        assert!(!node(&result, NodeKind::Function, "assigned").is_exported);
    }
}

#[test]
fn typescript_interface_members_are_owned_declarations() {
    let result = extract(
        "api.d.ts",
        r#"
export interface PlatformApi {
  fetchPage(id: string): Promise<string>;
  version: string;
}
export type Handle = { stop(): void; label: string };
"#,
        Language::TypeScript,
    );

    let interface = node(&result, NodeKind::Interface, "PlatformApi");
    let method = node(&result, NodeKind::Method, "fetchPage");
    let property = node(&result, NodeKind::Property, "version");
    for member in [method, property] {
        assert!(result.edges.iter().any(|edge| {
            edge.kind == EdgeKind::Contains
                && edge.source == interface.id
                && edge.target == member.id
        }));
    }
    assert!(
        result
            .nodes
            .iter()
            .all(|candidate| !(candidate.kind == NodeKind::Function && candidate.name == "stop")),
        "a type-literal method signature must not become a free function"
    );
}

#[test]
fn nested_declarator_handlers_own_their_calls() {
    let result = extract(
        "widget.tsx",
        r#"
function formatLabel(x: unknown) { return x; }
export function Widget(onPick: (x: unknown) => void) {
  const handleClear = () => { onPick(null); };
  const describe = function (item: unknown) { return formatLabel(item); };
  let later = (x: unknown) => formatLabel(x);
  const count = 1;
  const [a, b] = [() => 1, () => 2];
  return later(describe(count));
}
"#,
        Language::Tsx,
    );

    let widget = node(&result, NodeKind::Function, "Widget");
    let handle = node(&result, NodeKind::Function, "handleClear");
    let describe = node(&result, NodeKind::Function, "describe");
    assert_eq!(handle.qualified_name, "Widget::handleClear");
    assert!(result.edges.iter().any(|edge| {
        edge.kind == EdgeKind::Contains && edge.source == widget.id && edge.target == handle.id
    }));
    assert!(calls_from(&result, &handle.id).contains(&"onPick"));
    assert!(!calls_from(&result, &widget.id).contains(&"onPick"));
    assert!(calls_from(&result, &describe.id).contains(&"formatLabel"));
    assert!(!function_names(&result).contains(&"count"));
    assert!(!function_names(&result).contains(&"a"));
}

#[test]
fn hook_and_curried_wrapper_handlers_are_named_but_computations_are_not() {
    let result = extract(
        "handlers.tsx",
        r#"
function helper() { return 1; }
export function Screen() {
  const approved = useCallback(() => helper(), []);
  const opened = React.useCallback(function () { return helper(); }, []);
  const logged = useEffectEvent(() => helper());
  const wrapped = Effect.fn("Screen.wrapped")(function* () { return helper(); });
  const connected = connect({})(() => helper());
  const total = useMemo(() => helper(), []);
  const mapped = [1].map(() => helper());
  return approved;
}
"#,
        Language::Tsx,
    );

    let names = function_names(&result);
    for expected in ["approved", "opened", "logged", "wrapped", "connected"] {
        assert!(
            names.contains(&expected),
            "missing {expected}; names={names:?}"
        );
        let function = node(&result, NodeKind::Function, expected);
        assert!(calls_from(&result, &function.id).contains(&"helper"));
    }
    assert!(!names.contains(&"total"));
    assert!(!names.contains(&"mapped"));
}

#[test]
fn commonjs_export_assignments_name_only_exported_callable_members() {
    let result = extract(
        "controller.js",
        r#"
function findItems() {}
function removeItem() {}
exports.getItems = async () => { findItems(); };
module.exports.deleteItem = function () { removeItem(); };
exports.plain = 42;
module.exports = { legacy: 1 };
const handlers = {};
handlers.onSave = () => { findItems(); };
"#,
        Language::JavaScript,
    );

    let get = node(&result, NodeKind::Function, "getItems");
    let delete = node(&result, NodeKind::Function, "deleteItem");
    assert!(get.is_exported && delete.is_exported);
    assert!(get.is_async);
    assert!(calls_from(&result, &get.id).contains(&"findItems"));
    assert!(calls_from(&result, &delete.id).contains(&"removeItem"));
    assert!(!function_names(&result).contains(&"onSave"));
}

#[test]
fn tsx_and_jsx_class_fields_are_modelled_like_their_base_language() {
    // TSX and JSX run the TypeScript/JavaScript extractors (upstream
    // `languages/index.ts`); a plain field is a property there, only a
    // function-valued field is a method.
    let source = r#"
class Cart {}
export class Vault {
  items = new Cart();
  #secret = 1;
  handle = () => { run(); };
  run() {}
}
"#;
    let shape = |file: &str, language: Language| {
        let mut members = extract(file, source, language)
            .nodes
            .into_iter()
            .filter(|node| node.qualified_name.starts_with("Vault::"))
            .map(|node| (node.name, node.kind))
            .collect::<Vec<_>>();
        members.sort_by(|a, b| a.0.cmp(&b.0));
        members
    };
    // JS names a field by its `property` child (upstream #808).
    let expected = vec![
        ("#secret".to_string(), NodeKind::Property),
        ("handle".to_string(), NodeKind::Method),
        ("items".to_string(), NodeKind::Property),
        ("run".to_string(), NodeKind::Method),
    ];
    for (file, language) in [
        ("vault.ts", Language::TypeScript),
        ("vault.tsx", Language::Tsx),
        ("vault.js", Language::JavaScript),
        ("vault.jsx", Language::Jsx),
    ] {
        assert_eq!(shape(file, language), expected, "{file}");
    }
}
