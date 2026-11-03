//! The structural dispatch passes (upstream `callback-synthesizer.ts`): a
//! framework re-render (`react-render`, `flutter-build`), a C++ virtual
//! override (`cpp-override`), a Kotlin Multiplatform declaration's `actual`
//! (`kotlin-expect-actual`), a Delphi form's code-behind (`pascal-form`)
//! and a MyBatis mapper's statement (`mybatis-java-xml`). Each pairs nodes
//! by their structure or names; none reads a call site.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use anyhow::Result;
use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde_json::{Value, json};

use super::MAX_CALLBACKS_PER_CHANNEL;
use super::stage::{View, node_order, slice_lines};

/// A React class component's state update (`this.setState(…)`).
static SET_STATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"this\.setState\s*\(").expect("setState"));
/// A Flutter `State`'s update (`setState(() {…})`, `this.setState`).
static FLUTTER_SET_STATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b)setState\s*\(").expect("flutter setState"));

/// A synthesized edge of `kind` from `source` to `target` at `line`.
pub(super) fn synthesized(
    source: &Node,
    target: &Node,
    kind: EdgeKind,
    line: i64,
    metadata: Value,
) -> Edge {
    Edge {
        id: None,
        source: source.id.clone(),
        target: target.id.clone(),
        kind,
        metadata: Some(metadata),
        line: Some(line),
        col: None,
        provenance: Some("heuristic".to_string()),
    }
}

/// `node`'s place, `path:line`, as `registeredAt` names it.
pub(super) fn place(node: &Node) -> String {
    format!("{}:{}", node.file_path, node.start_line)
}

/// The methods `parent` contains, in its `contains` edges' order.
pub(super) fn methods_of(view: &View<'_>, parent: &str) -> Result<Vec<Node>> {
    let mut methods = Vec::new();
    for edge in view.edges_from(parent, EdgeKind::Contains)? {
        if let Some(node) = view.node(&edge.target)?
            && node.kind == NodeKind::Method
        {
            methods.push(node);
        }
    }
    Ok(methods)
}

/// [`methods_of`] of each of `parents`, by parent, every one of them
/// present: the same methods in the same order, with one query a chunk for
/// the edges and one for their targets. Edges that tie on target, line and
/// column name the same method, so their order cannot change the list.
pub(super) fn methods_of_each(
    view: &View<'_>,
    parents: &[String],
) -> Result<HashMap<String, Vec<Node>>> {
    let edges = view.edges_from_each(parents, EdgeKind::Contains)?;
    let targets: Vec<String> = edges
        .values()
        .flatten()
        .map(|edge| edge.target.clone())
        .collect();
    let nodes = view.nodes(&targets)?;
    let mut methods = HashMap::new();
    for parent in parents {
        let found: Vec<Node> = edges
            .get(parent)
            .into_iter()
            .flatten()
            .filter_map(|edge| nodes.get(&edge.target))
            .filter(|node| node.kind == NodeKind::Method)
            .cloned()
            .collect();
        methods.insert(parent.clone(), found);
    }
    Ok(methods)
}

/// The classes among `ids`, in upstream's class order ([`node_order`]):
/// those of upstream's walk over every class (`iterateNodesByKind`) that
/// `ids` names, without reading the others.
fn classes_among(view: &View<'_>, ids: &HashSet<String>) -> Result<Vec<Node>> {
    let mut classes = Vec::new();
    for id in ids {
        if let Some(node) = view.node(id)?
            && node.kind == NodeKind::Class
        {
            classes.push(node);
        }
    }
    classes.sort_by(node_order);
    Ok(classes)
}

/// Each class whose method named `name` re-runs on a state update: every
/// sibling method whose source matches `update` links to it, at most
/// [`MAX_CALLBACKS_PER_CHANNEL`] a class. Of every class, only those in
/// `owners`, the parents of a method named `name`, can link, so only they
/// are walked; `accepts` gates the method found.
fn rerender_edges(
    view: &View<'_>,
    name: &str,
    owners: &HashSet<String>,
    accepts: fn(&Node) -> bool,
    update: &Regex,
    synthesized_by: &str,
) -> Result<Vec<Edge>> {
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for class in classes_among(view, owners)? {
        let children = methods_of(view, &class.id)?;
        let Some(target) = children.iter().find(|method| method.name == name) else {
            continue;
        };
        if !accepts(target) {
            continue;
        }
        let mut added = 0;
        for method in &children {
            if added >= MAX_CALLBACKS_PER_CHANNEL {
                break;
            }
            if method.id == target.id {
                continue;
            }
            let Some(content) = view.text(&method.file_path) else {
                continue;
            };
            let Some(source) = slice_lines(&content, method.start_line, method.end_line) else {
                continue;
            };
            if !update.is_match(&source) || !seen.insert((method.id.clone(), target.id.clone())) {
                continue;
            }
            edges.push(synthesized(
                method,
                target,
                EdgeKind::Calls,
                method.start_line,
                json!({
                    "synthesizedBy": synthesized_by,
                    "via": "setState",
                    "registeredAt": place(target),
                }),
            ));
            added += 1;
        }
    }
    Ok(edges)
}

/// The classes that contain a method named `name`.
fn owners_of_method(view: &View<'_>, name: &str) -> Result<HashSet<String>> {
    let mut owners = HashSet::new();
    for method in view.nodes_named(name)? {
        if method.kind != NodeKind::Method {
            continue;
        }
        for edge in view.edges_into(&method.id, EdgeKind::Contains)? {
            owners.insert(edge.source);
        }
    }
    Ok(owners)
}

/// React class components (`reactRenderEdges`): `this.setState(…)` re-runs
/// `render()`, so each method calling it reaches its class's `render`.
pub(super) fn react_render_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let owners = owners_of_method(view, "render")?;
    rerender_edges(
        view,
        "render",
        &owners,
        |_| true,
        &SET_STATE,
        "react-render",
    )
}

/// Flutter `State` classes (`flutterBuildEdges`): `setState(…)` re-runs
/// `build(context)`, so each method calling it reaches its class's `build`
/// when that one is Dart. Upstream walks every class; only one containing a
/// method named `build` can link, so the walk starts from those.
pub(super) fn flutter_build_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let owners = owners_of_method(view, "build")?;
    rerender_edges(
        view,
        "build",
        &owners,
        |build| build.file_path.ends_with(".dart"),
        &FLUTTER_SET_STATE,
        "flutter-build",
    )
}

/// C++ virtual overrides (`cppOverrideEdges` at `b635dd46`): a call through
/// a base reaches each subclass method of the same name, so a base class's
/// method links to the override in each class that `extends` it, at most
/// [`MAX_CALLBACKS_PER_CHANNEL`] a base. Upstream walks every class; only one
/// with an `extends` edge can link, so the walk is over those.
pub(super) fn cpp_override_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let deriving: HashSet<String> = view
        .edges_of_kind(EdgeKind::Extends)?
        .into_iter()
        .map(|edge| edge.source)
        .collect();
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for class in classes_among(view, &deriving)? {
        let overriding: Vec<Node> = methods_of(view, &class.id)?
            .into_iter()
            .filter(|method| method.language == Language::Cpp)
            .collect();
        if overriding.is_empty() {
            continue;
        }
        for extends in view.edges_from(&class.id, EdgeKind::Extends)? {
            let Some(base) = view.node(&extends.target)? else {
                continue;
            };
            if base.language != Language::Cpp || base.id == class.id {
                continue;
            }
            // A later namesake replaces an earlier one, as upstream's `Map`.
            let mut by_name: HashMap<String, Node> = HashMap::new();
            for method in methods_of(view, &base.id)? {
                by_name.insert(method.name.clone(), method);
            }
            let mut added = 0;
            for method in &overriding {
                if added >= MAX_CALLBACKS_PER_CHANNEL {
                    break;
                }
                let Some(virtual_method) = by_name.get(&method.name) else {
                    continue;
                };
                if virtual_method.id == method.id
                    || !seen.insert((virtual_method.id.clone(), method.id.clone()))
                {
                    continue;
                }
                edges.push(synthesized(
                    virtual_method,
                    method,
                    EdgeKind::Calls,
                    virtual_method.start_line,
                    json!({
                        "synthesizedBy": "cpp-override",
                        "via": method.name,
                        "registeredAt": place(method),
                    }),
                ));
                added += 1;
            }
        }
    }
    Ok(edges)
}

/// The kinds an `expect`/`actual` pair may straddle: the same kind, or two
/// type-like ones (an `expect class` met by an `actual typealias`).
fn kmp_kinds_compatible(a: NodeKind, b: NodeKind) -> bool {
    let type_like = |kind| {
        matches!(
            kind,
            NodeKind::Class
                | NodeKind::Interface
                | NodeKind::Struct
                | NodeKind::Enum
                | NodeKind::TypeAlias
        )
    };
    a == b || (type_like(a) && type_like(b))
}

/// Kotlin Multiplatform (`kotlinExpectActualEdges`): an `expect`
/// declaration is fulfilled per platform by an `actual` one, so the
/// declaration (the same qualified name and a compatible kind in another
/// file, not itself `actual`) links to each, at most
/// [`MAX_CALLBACKS_PER_CHANNEL`] for one `actual`.
pub(super) fn kotlin_expect_actual_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let is_actual = |node: &Node| {
        node.decorators
            .iter()
            .any(|decorator| decorator == "actual")
    };
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for actual in view.nodes_with_decorator(Language::Kotlin, "actual")? {
        if !is_actual(&actual) {
            continue;
        }
        let mut added = 0;
        for declaration in view.nodes_qualified(&actual.qualified_name)? {
            if added >= MAX_CALLBACKS_PER_CHANNEL {
                break;
            }
            if declaration.language != Language::Kotlin
                || declaration.id == actual.id
                || !kmp_kinds_compatible(declaration.kind, actual.kind)
                || declaration.file_path == actual.file_path
                || is_actual(&declaration)
                || !seen.insert((declaration.id.clone(), actual.id.clone()))
            {
                continue;
            }
            edges.push(synthesized(
                &declaration,
                &actual,
                EdgeKind::Calls,
                declaration.start_line,
                json!({
                    "synthesizedBy": "kotlin-expect-actual",
                    "via": actual.name,
                    "registeredAt": place(&actual),
                }),
            ));
            added += 1;
        }
    }
    Ok(edges)
}

/// The unit `UFRMAbout.pas` beside a form `UFRMAbout.dfm` or `.fmx`, the
/// extension matched in any ASCII case.
fn form_unit(form: &str) -> Option<String> {
    let at = form.len().checked_sub(4)?;
    let extension = form.get(at..)?;
    (extension.eq_ignore_ascii_case(".dfm") || extension.eq_ignore_ascii_case(".fmx"))
        .then(|| format!("{}.pas", &form[..at]))
}

/// Delphi forms (`pascalFormEdges`): a form unit owns the `.dfm` (VCL) or
/// `.fmx` (FireMonkey) form of its name in its directory, wired by
/// `{$R *.dfm}`, so the unit's file node references the form's.
pub(super) fn pascal_form_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let files = view.files()?;
    let indexed: HashSet<&str> = files.iter().map(String::as_str).collect();
    let file_node = |path: &str| -> Result<Option<Node>> {
        Ok(view
            .nodes_in_file(path)?
            .into_iter()
            .find(|node| node.kind == NodeKind::File))
    };
    let mut edges = Vec::new();
    for form in &files {
        let Some(unit) = form_unit(form) else {
            continue;
        };
        if !indexed.contains(unit.as_str()) {
            continue;
        }
        let (Some(form_node), Some(unit_node)) = (file_node(form)?, file_node(&unit)?) else {
            continue;
        };
        edges.push(synthesized(
            &unit_node,
            &form_node,
            EdgeKind::References,
            unit_node.start_line,
            json!({
                "synthesizedBy": "pascal-form",
                "registeredAt": unit,
            }),
        ));
    }
    Ok(edges)
}

/// MyBatis mappers (`mybatisJavaXmlEdges`): a statement
/// `<namespace>::<id>` holds the SQL of the Java or Kotlin method
/// `<Class>::<id>`, `<Class>` the namespace's last dotted segment, so the
/// method links to the statement when it is the only one of that pair.
pub(super) fn mybatis_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let methods_in = |languages: &[Language]| -> Result<Vec<Node>> {
        Ok(view
            .callables_in_languages(languages)?
            .into_iter()
            .filter(|node| node.kind == NodeKind::Method)
            .collect())
    };
    let statements = methods_in(&[Language::Xml])?;
    if statements.is_empty() {
        return Ok(Vec::new());
    }
    let mut by_class_member: HashMap<String, Vec<Node>> = HashMap::new();
    for method in methods_in(&[Language::Java, Language::Kotlin])? {
        let parts: Vec<&str> = method.qualified_name.split("::").collect();
        let [.., class, member] = parts.as_slice() else {
            continue;
        };
        if class.is_empty() || member.is_empty() {
            continue;
        }
        by_class_member
            .entry(format!("{class}::{member}"))
            .or_default()
            .push(method);
    }
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for statement in &statements {
        let Some((namespace, id)) = statement.qualified_name.rsplit_once("::") else {
            continue;
        };
        if namespace.is_empty() || id.is_empty() {
            continue;
        }
        let class = namespace
            .rsplit_once('.')
            .map_or(namespace, |(_, class)| class);
        let Some([method]) = by_class_member
            .get(&format!("{class}::{id}"))
            .map(Vec::as_slice)
        else {
            continue;
        };
        if !seen.insert((method.id.clone(), statement.id.clone())) {
            continue;
        }
        edges.push(synthesized(
            method,
            statement,
            EdgeKind::Calls,
            method.start_line,
            json!({
                "synthesizedBy": "mybatis-java-xml",
                "via": format!("{class}.{id}"),
                "registeredAt": place(statement),
            }),
        ));
    }
    Ok(edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_form_names_its_unit_in_any_case() {
        assert_eq!(
            form_unit("src/UFRMAbout.dfm").as_deref(),
            Some("src/UFRMAbout.pas")
        );
        assert_eq!(form_unit("src/Main.FMX").as_deref(), Some("src/Main.pas"));
        assert_eq!(form_unit("src/Main.pas"), None);
        assert_eq!(form_unit(".dfm").as_deref(), Some(".pas"));
        assert_eq!(form_unit("dfm"), None);
    }

    #[test]
    fn expect_and_actual_may_straddle_type_kinds() {
        assert!(kmp_kinds_compatible(NodeKind::Class, NodeKind::TypeAlias));
        assert!(kmp_kinds_compatible(NodeKind::Function, NodeKind::Function));
        assert!(!kmp_kinds_compatible(NodeKind::Function, NodeKind::Class));
        assert!(!kmp_kinds_compatible(
            NodeKind::Property,
            NodeKind::Function
        ));
    }
}
