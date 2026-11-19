//! A SvelteKit route's halves, joined by the file system rather than by a
//! call (W7-18b): upstream `callback-synthesizer.ts`'s `svelteKitLoadEdges`
//! and the page half of `sveltekit-synthesizer.ts`
//! (`svelteKitPageComponentEdges`).
//!
//! A route directory's `+page.svelte` receives its `data` from the sibling
//! `+page.server.{ts,js}` or `+page.{ts,js}` `load` and posts its forms to
//! that file's `actions`; the framework wires them by path, with no import
//! between them, so a loader's edit showed no impact on the page it feeds.
//! [`load_edges`] draws a `calls` edge from the page (or `+layout`)
//! component to its loader's hooks. A SvelteKit route is named by its
//! file's path and its component, called `+page` in every page file, by
//! nothing else, so [`page_component_edges`] joins each route to the
//! component of its own file. Both read only paths and nodes, no text.

use std::collections::HashSet;

use anyhow::Result;
use codegraph_core::types::{Edge, EdgeKind, Node, NodeKind};
use serde_json::json;

use super::dispatch::{place, synthesized};
use super::stage::View;
use crate::frameworks::sveltekit_router::sveltekit_pages;

/// A loader's file beside its page, in upstream's order.
const LOADER_SUFFIXES: [&str; 4] = [".server.ts", ".server.js", ".ts", ".js"];

/// The hooks a loader names (upstream `HOOKS`), which count in any of
/// [`HOOK_KINDS`].
const HOOKS: [&str; 2] = ["load", "actions"];

/// The kinds a named hook may be (upstream `HOOK_KINDS`).
const HOOK_KINDS: [NodeKind; 4] = [
    NodeKind::Function,
    NodeKind::Method,
    NodeKind::Constant,
    NodeKind::Variable,
];

/// The directory and the `+page` or `+layout` prefix of a page or layout
/// file (upstream `(.*\/)(\+(?:page|layout))\.svelte$`): its file name is
/// exactly `+page.svelte` or `+layout.svelte`, under some directory, so a
/// file at the project root is neither.
fn page_or_layout(file: &str) -> Option<(&str, &str)> {
    let (dir, name) = file.rsplit_once('/')?;
    let prefix = name.strip_suffix(".svelte")?;
    matches!(prefix, "+page" | "+layout").then(|| (&file[..=dir.len()], prefix))
}

/// The one component of `nodes`, `None` for none or several (upstream
/// `find(kind === 'component')`, the first in node order; the Svelte
/// extractor makes one a file).
fn the_component(nodes: &[Node]) -> Option<&Node> {
    let mut components = nodes.iter().filter(|node| node.kind == NodeKind::Component);
    match (components.next(), components.next()) {
        (Some(component), None) => Some(component),
        _ => None,
    }
}

/// A page or layout component to its loader's hooks (`svelteKitLoadEdges`):
/// for each indexed `+page.svelte` and `+layout.svelte`, the indexed
/// loaders `{dir}{prefix}{.server.ts,.server.js,.ts,.js}` beside it, and in
/// each every function and method, with `load` and `actions` of any hook
/// kind: a form action is an arrow inside `actions` and a node of its own
/// (`default`, `logout`), where the redirect ending the submission is
/// written. The edge is drawn at the component's line and says which hook
/// (`via`) and where it is declared (`registeredAt`).
pub(super) fn load_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let files = view.files()?;
    let indexed: HashSet<&str> = files.iter().map(String::as_str).collect();
    let mut edges = Vec::new();
    for file in &files {
        let Some((dir, prefix)) = page_or_layout(file) else {
            continue;
        };
        let nodes = view.nodes_in_file(file)?;
        let Some(page) = the_component(&nodes) else {
            continue;
        };
        for suffix in LOADER_SUFFIXES {
            let loader = format!("{dir}{prefix}{suffix}");
            if !indexed.contains(loader.as_str()) {
                continue;
            }
            for hook in view.nodes_in_file(&loader)? {
                let named = HOOK_KINDS.contains(&hook.kind) && HOOKS.contains(&hook.name.as_str());
                if !named && !matches!(hook.kind, NodeKind::Function | NodeKind::Method) {
                    continue;
                }
                edges.push(synthesized(
                    page,
                    &hook,
                    EdgeKind::Calls,
                    page.start_line,
                    json!({
                        "synthesizedBy": "sveltekit-load",
                        "via": hook.name,
                        "registeredAt": place(&hook),
                    }),
                ));
            }
        }
    }
    Ok(edges)
}

/// Each SvelteKit page route to the component of its own file
/// (`svelteKitPageComponentEdges`): the page that renders when a
/// navigation lands there, drawn at the component's line, `registeredAt`
/// the file. Every page the SvelteKit table holds is joined, one whose path
/// another page of its app serves too included (KEEP-RUST: upstream walks
/// its table's paths, each holding the first page in node order).
pub(super) fn page_component_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let context = view.context();
    let mut edges = Vec::new();
    for page in sveltekit_pages(&context) {
        let nodes = view.nodes_in_file(&page.file_path)?;
        let Some(component) = the_component(&nodes) else {
            continue;
        };
        edges.push(synthesized(
            &page,
            component,
            EdgeKind::Calls,
            component.start_line,
            json!({
                "synthesizedBy": "sveltekit-page",
                "registeredAt": page.file_path,
            }),
        ));
    }
    Ok(edges)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::UNIX_EPOCH;

    use codegraph_core::node_id::hash_content;
    use codegraph_core::types::{Edge, FileRecord, Language, Node, NodeKind};
    use codegraph_store::Store;

    use super::page_or_layout;

    /// A project of hand-written files whose index holds exactly the nodes
    /// given: the pages, components and loader nodes in the shape the
    /// Svelte and JavaScript extraction gives them.
    struct Fixture {
        dir: PathBuf,
        files: Vec<(String, Language)>,
        nodes: Vec<Node>,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "cg-sveltekit-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self {
                dir,
                files: Vec::new(),
                nodes: Vec::new(),
            }
        }

        /// The indexed file `path`, with `nodes` in it.
        fn file(mut self, path: &str, language: Language, nodes: Vec<Node>) -> Self {
            self.files.push((path.to_string(), language));
            self.nodes.extend(nodes);
            self
        }

        /// The edges the pass naming itself `synthesized_by` draws, as
        /// `source -> target metadata`, sorted, with each end's file.
        fn edges(&self, synthesized_by: &str) -> Vec<String> {
            let mut store = Store::open(&self.dir.join("index.db")).unwrap();
            store.upsert_nodes(&self.nodes).unwrap();
            for (path, language) in &self.files {
                let full = self.dir.join(path);
                std::fs::create_dir_all(full.parent().unwrap()).unwrap();
                std::fs::write(&full, path).unwrap();
                store
                    .upsert_file(&FileRecord {
                        path: path.clone(),
                        content_hash: hash_content(path),
                        language: *language,
                        size: 0,
                        modified_at: 0,
                        indexed_at: 0,
                        node_count: 0,
                        errors: Vec::new(),
                        generated: false,
                    })
                    .unwrap();
            }
            let label = |id: &str| {
                self.nodes
                    .iter()
                    .find(|node| node.id == id)
                    .map_or("?".to_string(), |node| {
                        format!("{} {}", node.file_path, node.name)
                    })
            };
            let mut edges: Vec<String> = crate::synthesis::synthesize(&store, &self.dir, 1 << 20)
                .unwrap()
                .into_iter()
                .filter(|edge: &Edge| {
                    edge.metadata
                        .as_ref()
                        .is_some_and(|metadata| metadata["synthesizedBy"] == synthesized_by)
                })
                .map(|edge| {
                    let mut metadata = edge.metadata.clone().unwrap();
                    metadata.as_object_mut().unwrap().remove("synthesizedBy");
                    format!(
                        "{} -> {} {} line {}",
                        label(&edge.source),
                        label(&edge.target),
                        metadata,
                        edge.line.unwrap()
                    )
                })
                .collect();
            edges.sort();
            edges
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn node(kind: NodeKind, name: &str, file: &str, language: Language, line: i64) -> Node {
        Node {
            id: format!("{}:{file}:{name}:{line}", kind.as_str()),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: file.to_string(),
            language,
            start_line: line,
            end_line: line + 1,
            start_column: 0,
            end_column: 1,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: true,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }

    /// A `+page.svelte` or `+layout.svelte` as the Svelte extraction gives
    /// it: its component at line 1, and for a page the route its path
    /// names (`route:{file}:{path}:1`).
    fn svelte(file: &str, route: Option<&str>) -> Vec<Node> {
        let name = file.rsplit('/').next().unwrap().trim_end_matches(".svelte");
        let mut nodes = vec![node(NodeKind::Component, name, file, Language::Svelte, 1)];
        if let Some(path) = route {
            let mut route = node(NodeKind::Route, path, file, Language::Svelte, 1);
            route.id = format!("route:{file}:{path}:1");
            nodes.push(route);
        }
        nodes
    }

    fn js(kind: NodeKind, name: &str, file: &str, line: i64) -> Node {
        node(kind, name, file, Language::JavaScript, line)
    }

    #[test]
    fn a_page_or_layout_is_its_file_name_under_a_directory() {
        assert_eq!(
            page_or_layout("src/routes/+page.svelte"),
            Some(("src/routes/", "+page"))
        );
        assert_eq!(page_or_layout("a/+layout.svelte"), Some(("a/", "+layout")));
        for file in [
            "+page.svelte",
            "src/routes/x+page.svelte",
            "src/routes/+error.svelte",
            "src/routes/+page.svelte.ts",
            "src/routes/+page.ts",
        ] {
            assert_eq!(page_or_layout(file), None, "{file}");
        }
    }

    /// A page calls its loader's `load`, its `actions` and every function
    /// and method the loader declares; the server loader, the universal one
    /// and the layout's each count, at the component's line. A variable
    /// that is no hook, a class, a type and an interface named like hooks,
    /// and a loader of another directory do not.
    #[test]
    fn a_page_calls_the_hooks_of_its_loaders() {
        const SERVER: &str = "src/routes/login/+page.server.js";
        const UNIVERSAL: &str = "src/routes/login/+page.js";
        const LAYOUT: &str = "src/routes/+layout.server.ts";
        let ts = |kind, name, line| node(kind, name, LAYOUT, Language::TypeScript, line);
        let fixture = Fixture::new("load")
            .file(
                "src/routes/login/+page.svelte",
                Language::Svelte,
                svelte("src/routes/login/+page.svelte", Some("/login")),
            )
            .file(
                SERVER,
                Language::JavaScript,
                vec![
                    js(NodeKind::Function, "load", SERVER, 2),
                    js(NodeKind::Constant, "actions", SERVER, 5),
                    js(NodeKind::Function, "default", SERVER, 6),
                    js(NodeKind::Variable, "limit", SERVER, 12),
                    js(NodeKind::Class, "Session", SERVER, 13),
                ],
            )
            .file(
                UNIVERSAL,
                Language::JavaScript,
                vec![js(NodeKind::Variable, "load", UNIVERSAL, 1)],
            )
            .file(
                "src/routes/+layout.svelte",
                Language::Svelte,
                svelte("src/routes/+layout.svelte", None),
            )
            .file(
                LAYOUT,
                Language::TypeScript,
                vec![
                    ts(NodeKind::TypeAlias, "load", 1),
                    ts(NodeKind::Interface, "actions", 2),
                    ts(NodeKind::Method, "user", 3),
                ],
            )
            .file(
                "src/routes/other/+page.server.js",
                Language::JavaScript,
                vec![js(
                    NodeKind::Function,
                    "load",
                    "src/routes/other/+page.server.js",
                    1,
                )],
            );
        assert_eq!(
            fixture.edges("sveltekit-load"),
            [
                r#"src/routes/+layout.svelte +layout -> src/routes/+layout.server.ts user {"registeredAt":"src/routes/+layout.server.ts:3","via":"user"} line 1"#,
                r#"src/routes/login/+page.svelte +page -> src/routes/login/+page.js load {"registeredAt":"src/routes/login/+page.js:1","via":"load"} line 1"#,
                r#"src/routes/login/+page.svelte +page -> src/routes/login/+page.server.js actions {"registeredAt":"src/routes/login/+page.server.js:5","via":"actions"} line 1"#,
                r#"src/routes/login/+page.svelte +page -> src/routes/login/+page.server.js default {"registeredAt":"src/routes/login/+page.server.js:6","via":"default"} line 1"#,
                r#"src/routes/login/+page.svelte +page -> src/routes/login/+page.server.js load {"registeredAt":"src/routes/login/+page.server.js:2","via":"load"} line 1"#,
            ]
        );
    }

    /// No loader edge without the page's component, from a page at the
    /// project root, or to a loader the index does not hold.
    #[test]
    fn a_page_without_its_component_or_its_loader_calls_nothing() {
        let mut without = svelte("src/routes/a/+page.svelte", Some("/a"));
        without.retain(|node| node.kind != NodeKind::Component);
        let fixture = Fixture::new("load-none")
            .file("src/routes/a/+page.svelte", Language::Svelte, without)
            .file(
                "src/routes/a/+page.server.js",
                Language::JavaScript,
                vec![js(
                    NodeKind::Function,
                    "load",
                    "src/routes/a/+page.server.js",
                    1,
                )],
            )
            .file(
                "+page.svelte",
                Language::Svelte,
                svelte("+page.svelte", None),
            )
            .file(
                "+page.server.js",
                Language::JavaScript,
                vec![js(NodeKind::Function, "load", "+page.server.js", 1)],
            )
            .file(
                "src/routes/b/+page.svelte",
                Language::Svelte,
                svelte("src/routes/b/+page.svelte", Some("/b")),
            );
        assert!(fixture.edges("sveltekit-load").is_empty());
    }

    /// Two components in one page file make neither the page: the loader's
    /// hooks and the route join nothing there.
    #[test]
    fn a_page_file_of_two_components_joins_nothing() {
        let mut nodes = svelte("src/routes/x/+page.svelte", Some("/x"));
        nodes.push(node(
            NodeKind::Component,
            "Other",
            "src/routes/x/+page.svelte",
            Language::Svelte,
            4,
        ));
        let fixture = Fixture::new("two-components")
            .file("src/routes/x/+page.svelte", Language::Svelte, nodes)
            .file(
                "src/routes/x/+page.ts",
                Language::TypeScript,
                vec![node(
                    NodeKind::Function,
                    "load",
                    "src/routes/x/+page.ts",
                    Language::TypeScript,
                    1,
                )],
            );
        assert!(fixture.edges("sveltekit-load").is_empty());
        assert!(fixture.edges("sveltekit-page").is_empty());
    }

    /// Each page route calls its own file's component; a `[...rest]` page,
    /// which the table does not hold, and a layout, which is no route, do
    /// not. Two pages of one app serving one path each join their own
    /// component.
    #[test]
    fn a_route_calls_its_own_files_component() {
        let fixture = Fixture::new("page")
            .file(
                "src/routes/+page.svelte",
                Language::Svelte,
                svelte("src/routes/+page.svelte", Some("/")),
            )
            .file(
                "src/routes/(a)/x/+page.svelte",
                Language::Svelte,
                svelte("src/routes/(a)/x/+page.svelte", Some("/x")),
            )
            .file(
                "src/routes/(b)/x/+page.svelte",
                Language::Svelte,
                svelte("src/routes/(b)/x/+page.svelte", Some("/x")),
            )
            .file(
                "src/routes/[...rest]/+page.svelte",
                Language::Svelte,
                svelte("src/routes/[...rest]/+page.svelte", Some("/*rest")),
            )
            .file(
                "src/routes/+layout.svelte",
                Language::Svelte,
                svelte("src/routes/+layout.svelte", None),
            );
        assert_eq!(
            fixture.edges("sveltekit-page"),
            [
                r#"src/routes/(a)/x/+page.svelte /x -> src/routes/(a)/x/+page.svelte +page {"registeredAt":"src/routes/(a)/x/+page.svelte"} line 1"#,
                r#"src/routes/(b)/x/+page.svelte /x -> src/routes/(b)/x/+page.svelte +page {"registeredAt":"src/routes/(b)/x/+page.svelte"} line 1"#,
                r#"src/routes/+page.svelte / -> src/routes/+page.svelte +page {"registeredAt":"src/routes/+page.svelte"} line 1"#,
            ]
        );
    }
}
