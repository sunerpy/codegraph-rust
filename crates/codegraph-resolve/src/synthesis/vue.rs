//! The Vue template pass (W7-18c): upstream `callback-synthesizer.ts`'s
//! `vueTemplateEdges` at `e8ddc4e2`, without the manual-mock filter #2790
//! added to its lookups (OD-A1 leaves #2790 to the next round).
//!
//! A single-file component's script is extracted and its template's calls
//! are the component's (#2357), but what the template renders and the
//! handlers it binds reached nothing. For each `.vue` file, the root
//! template's kebab-case (`<el-button>`) and PascalCase (`<MediaCard>`) tags
//! draw `jsx-render` edges from the file's component to the component,
//! function or class they name, a component nested in a Nuxt `components/`
//! directory also by the name Nuxt auto-imports it under; its `@event` and
//! `v-on:event` handlers draw `vue-handler` edges: a bare name to the method
//! or function it names, and a name the script destructures from a
//! composable (`const { close: closeSidebar } = useSidebar()`) to that
//! member of the composable's file. A name the file imports from a package
//! is the package's (#2475): no project namesake, nor a Nuxt component,
//! answers it.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::LazyLock;

use anyhow::Result;
use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde_json::{Value, json};

use super::context::ViewContext;
use super::dispatch::synthesized;
use super::stage::View;
use crate::frameworks::js_scan::{JS_SPACE, is_js_space};
use crate::import_resolver::is_out_of_repo_import;
use crate::name_matcher::crosses_code_boundary;
use crate::package_manifests::is_declared_outside_package;
use crate::types::{ImportMapping, ResolutionContext};

/// The edges one component draws at most (upstream `MAX_JSX_CHILDREN`).
const MAX_CHILDREN: usize = 30;

/// What a tag renders (upstream `COMPONENT_KINDS`).
const COMPONENT_KINDS: [NodeKind; 3] = [NodeKind::Component, NodeKind::Function, NodeKind::Class];

/// What a handler names (upstream `HANDLER_KINDS`).
const HANDLER_KINDS: [NodeKind; 2] = [NodeKind::Method, NodeKind::Function];

/// What a composable's returned member may be (upstream `RETURN_KINDS`): a
/// function, or an arrow a constant or variable holds.
const RETURN_KINDS: [NodeKind; 4] = [
    NodeKind::Method,
    NodeKind::Function,
    NodeKind::Variable,
    NodeKind::Constant,
];

/// `pattern` with JavaScript's `\s`; its other classes are written out
/// ASCII, as JavaScript's are.
fn js(pattern: &str) -> Regex {
    Regex::new(&pattern.replace(r"\s", JS_SPACE)).expect("a Vue template pattern compiles")
}

/// The root template, from the first `<template…>` to the last
/// `</template>`, either in any case (upstream
/// `/<template[^>]*>([\s\S]*)<\/template>/i`); its markup is [`inner`].
static TEMPLATE: LazyLock<Regex> =
    LazyLock::new(|| js(r"(?i-u:<template)[^>]*>(?s:.*)(?i-u:</template>)"));

/// The first script block (upstream `/<script[^>]*>([\s\S]*?)<\/script>/i`);
/// its text is [`inner`].
static SCRIPT: LazyLock<Regex> =
    LazyLock::new(|| js(r"(?i-u:<script)[^>]*>(?s:.*?)(?i-u:</script>)"));

/// A destructure of what a call returns (upstream `VUE_DESTRUCTURE_RE`).
static DESTRUCTURE: LazyLock<Regex> =
    LazyLock::new(|| js(r"(?:const|let|var)\s*\{([^}]+)\}\s*=\s*([A-Za-z0-9_]+)\s*\("));

/// A composable's name, `use` and a capital.
static COMPOSABLE: LazyLock<Regex> = LazyLock::new(|| js(r"^use[A-Z]"));

/// One destructured member, `key` or `key: alias`.
static MEMBER: LazyLock<Regex> =
    LazyLock::new(|| js(r"^([A-Za-z0-9_]+)\s*(?::\s*([A-Za-z0-9_]+))?$"));

/// A kebab-case tag (upstream `VUE_KEBAB_RE`).
static KEBAB: LazyLock<Regex> = LazyLock::new(|| js(r"<([a-z][a-z0-9]*(?:-[a-z0-9]+)+)[\s/>]"));

/// A PascalCase tag (upstream `VUE_PASCAL_RE`); HTML's elements are
/// lowercase, and a built-in (`<Transition>`) names no node.
static PASCAL: LazyLock<Regex> = LazyLock::new(|| js(r"<([A-Z][A-Za-z0-9]*)[\s/>]"));

/// An event binding and its double-quoted value (upstream
/// `VUE_HANDLER_RE`).
static HANDLER: LazyLock<Regex> = LazyLock::new(|| {
    js(r#"(?:@|v-on:)([a-zA-Z][A-Za-z0-9_-]*)(?:\.[A-Za-z0-9_]+)*\s*=\s*"([^"]+)""#)
});

/// A handler's leading name, alone or called.
static HANDLER_NAME: LazyLock<Regex> =
    LazyLock::new(|| js(r"^([A-Za-z_$][A-Za-z0-9_$]*)\s*(?:\(|$)"));

/// A member a script destructures from a composable: the composable called
/// and the member's key in what it returns.
struct Member {
    composable: String,
    key: String,
}

/// One run of the pass.
struct Pass<'v, 'a> {
    view: &'v View<'a>,
    context: ViewContext<'v, 'a>,
    /// Every component, read once.
    components: &'v [Node],
    /// The nodes each name names, read once.
    named: HashMap<String, Rc<Vec<Node>>>,
    /// Each nested component's Nuxt name, built when a tag first needs it.
    nuxt: Option<HashMap<String, Option<&'v Node>>>,
    /// The file's import bindings ([`first_bindings`]), read when one of
    /// its names first needs them.
    bindings: Option<HashMap<String, String>>,
}

/// Vue templates (`vueTemplateEdges`): for each `.vue` file by path, the
/// root template's kebab-case and PascalCase tags (`jsx-render`, `via` the
/// tag as written) and its handlers (`vue-handler`, with the `event`, and
/// for a destructured handler `via` its composable), each an edge from the
/// file's one component at its line, at most [`MAX_CHILDREN`] a component.
pub(super) fn template_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let components = view.nodes_of_kind(NodeKind::Component)?;
    // Each file's one component; none for a file of several (W7-18b's rule).
    let mut file_components: HashMap<&str, Option<&Node>> = HashMap::new();
    for component in &components {
        file_components
            .entry(component.file_path.as_str())
            .and_modify(|held| *held = None)
            .or_insert(Some(component));
    }
    let mut pass = Pass {
        view,
        context: view.context(),
        components: &components,
        named: HashMap::new(),
        nuxt: None,
        bindings: None,
    };
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String, &'static str)> = HashSet::new();
    for file in view.files()? {
        if !file.ends_with(".vue") {
            continue;
        }
        let Some(Some(component)) = file_components.get(file.as_str()).copied() else {
            continue;
        };
        let Some(content) = view.text(&file) else {
            continue;
        };
        let Some(template) = inner(&TEMPLATE, &content, "<template".len(), "</template>".len())
            .filter(|template| !template.is_empty())
        else {
            continue;
        };
        let script = inner(&SCRIPT, &content, "<script".len(), "</script>".len()).unwrap_or("");
        let members = destructured(script);
        pass.bindings = None;
        let mut drawn = Drawn {
            component,
            added: 0,
            seen: &mut seen,
            edges: &mut edges,
        };
        for found in KEBAB.captures_iter(template) {
            if drawn.full() {
                break;
            }
            let written = &found[1];
            let target = pass.resolve(&kebab_to_pascal(written), &COMPONENT_KINDS, &file, true)?;
            drawn.add(
                target,
                "jsx-render",
                || json!({"synthesizedBy": "jsx-render", "via": written}),
            );
        }
        for found in PASCAL.captures_iter(template) {
            if drawn.full() {
                break;
            }
            let tag = &found[1];
            let target = pass.resolve(tag, &COMPONENT_KINDS, &file, true)?;
            drawn.add(
                target,
                "jsx-render",
                || json!({"synthesizedBy": "jsx-render", "via": tag}),
            );
        }
        for found in HANDLER.captures_iter(template) {
            if drawn.full() {
                break;
            }
            let event = &found[1];
            let expression = found[2].trim_matches(is_js_space);
            // An inline arrow, or a `$emit`.
            if expression.contains("=>") || expression.starts_with('$') {
                continue;
            }
            let Some(name) = HANDLER_NAME
                .captures(expression)
                .and_then(|named| named.get(1))
                .map(|name| name.as_str())
            else {
                continue;
            };
            // `@click="save"` names a method Vue calls with the event. A
            // handler that is an expression (`save(item)`, `open = !open`) is
            // the template's own code, whose calls the extractor records
            // (#2357): its leading name is no handler.
            if name == expression
                && let Some(handler) = pass.resolve(name, &HANDLER_KINDS, &file, false)?
            {
                drawn.add(
                    Some(handler),
                    "vue-handler",
                    || json!({"synthesizedBy": "vue-handler", "event": event}),
                );
                continue;
            }
            // A member the script destructures from a composable: the
            // member of that name in the composable's file, never the
            // composable itself, which the component's `useX()` call
            // already reaches.
            let Some(member) = members.get(name) else {
                continue;
            };
            let Some(composable) =
                pass.resolve(&member.composable, &HANDLER_KINDS, &file, false)?
            else {
                continue;
            };
            let returned = pass.named(&member.key)?;
            let mut keyed = returned.iter().filter(|node| {
                RETURN_KINDS.contains(&node.kind) && node.file_path == composable.file_path
            });
            if let (Some(keyed), None) = (keyed.next(), keyed.next()) {
                drawn.add(Some(keyed.clone()), "vue-handler", || {
                    json!({
                        "synthesizedBy": "vue-handler",
                        "event": event,
                        "via": member.composable,
                    })
                });
            }
        }
    }
    Ok(edges)
}

/// The edges one component draws (upstream `addEdge`): a pair already drawn
/// by the same kind of edge is skipped and not counted, the others count to
/// [`MAX_CHILDREN`].
struct Drawn<'d> {
    component: &'d Node,
    added: usize,
    seen: &'d mut HashSet<(String, String, &'static str)>,
    edges: &'d mut Vec<Edge>,
}

impl Drawn<'_> {
    fn full(&self) -> bool {
        self.added >= MAX_CHILDREN
    }

    fn add(&mut self, target: Option<Node>, by: &'static str, metadata: impl FnOnce() -> Value) {
        let Some(target) = target else {
            return;
        };
        if self.full()
            || target.id == self.component.id
            || crosses_code_boundary(self.component.language, target.language)
            || !self
                .seen
                .insert((self.component.id.clone(), target.id.clone(), by))
        {
            return;
        }
        self.edges.push(synthesized(
            self.component,
            &target,
            EdgeKind::Calls,
            self.component.start_line,
            metadata(),
        ));
        self.added += 1;
    }
}

impl Pass<'_, '_> {
    /// The nodes named `name`, read once a pass.
    fn named(&mut self, name: &str) -> Result<Rc<Vec<Node>>> {
        if let Some(nodes) = self.named.get(name) {
            return Ok(Rc::clone(nodes));
        }
        let nodes = Rc::new(self.view.nodes_named(name)?);
        self.named.insert(name.to_string(), Rc::clone(&nodes));
        Ok(nodes)
    }

    /// What `name`, of `kinds`, means in `file` (upstream `resolve`;
    /// KEEP-RUST, failing closed as W7-14's handlers do, where upstream
    /// takes the first in node order): the one of `file`, nothing when the
    /// file holds several; else nothing when the file imports the name from
    /// a package; else the project's one, nothing when it holds several;
    /// else, for a tag (`nuxt`) no node is named, the component Nuxt
    /// auto-imports under it. The package is asked about only for a name
    /// that would otherwise have a target.
    fn resolve(
        &mut self,
        name: &str,
        kinds: &[NodeKind],
        file: &str,
        nuxt: bool,
    ) -> Result<Option<Node>> {
        let named = self.named(name)?;
        let matches: Vec<&Node> = named
            .iter()
            .filter(|node| kinds.contains(&node.kind))
            .collect();
        let mut local = matches.iter().filter(|node| node.file_path == file);
        match (local.next(), local.next()) {
            (Some(node), None) => return Ok(Some((*node).clone())),
            (Some(_), Some(_)) => return Ok(None),
            (None, _) => {}
        }
        let candidate = match matches.as_slice() {
            [] if nuxt => self.nuxt_component(name),
            [node] => Some((*node).clone()),
            _ => None,
        };
        if candidate.is_some() && self.imported_from_package(name, file) {
            return Ok(None);
        }
        Ok(candidate)
    }

    /// Whether `file` imports `name` from a package outside the repository
    /// (upstream `importedFromPackage`): the module its binding of the name
    /// names is one no project file answers, which a package.json on the
    /// file's way to the root declares. The declaration, one conjunct of
    /// [`is_out_of_repo_import`] and the cheaper one, is asked first.
    fn imported_from_package(&mut self, name: &str, file: &str) -> bool {
        let context = &self.context;
        let bindings = self.bindings.get_or_insert_with(|| {
            first_bindings(&context.get_import_mappings(file, Language::Vue))
        });
        bindings.get(name).is_some_and(|source| {
            is_declared_outside_package(source, file, context)
                && is_out_of_repo_import(source, file, Language::Vue, context)
        })
    }

    /// The component Nuxt auto-imports under `name` (upstream
    /// `nuxtComponents`): KEEP-RUST, none for a name two components share,
    /// where upstream keeps the first in node order.
    fn nuxt_component(&mut self, name: &str) -> Option<Node> {
        let components = self.components;
        self.nuxt
            .get_or_insert_with(|| {
                let mut names: HashMap<String, Option<&Node>> = HashMap::new();
                for component in components {
                    if let Some(nuxt_name) = nuxt_component_name(&component.file_path) {
                        names
                            .entry(nuxt_name)
                            .and_modify(|held| *held = None)
                            .or_insert(Some(component));
                    }
                }
                names
            })
            .get(name)
            .copied()
            .flatten()
            .cloned()
    }
}

/// The text inside the block `block` finds in `text`: what follows its
/// opening tag (the `open` bytes `<name`, then up to the first `>`) up to
/// its `close` bytes `</name>`, as the upstream pattern's group captures it,
/// read from the match's bounds without the capture engine.
fn inner<'t>(block: &Regex, text: &'t str, open: usize, close: usize) -> Option<&'t str> {
    let found = block.find(text)?;
    let tag = found.start() + open;
    let start = tag + text[tag..found.end()].find('>')? + 1;
    Some(&text[start..found.end() - close])
}

/// Each name a file imports, to the module of its first binding in the
/// import mappings (upstream's `find(localName === name)`; their order is
/// `extract_import_mappings`', not the nodes').
fn first_bindings(mappings: &[ImportMapping]) -> HashMap<String, String> {
    let mut bindings = HashMap::new();
    for mapping in mappings {
        bindings
            .entry(mapping.local_name.clone())
            .or_insert_with(|| mapping.source.clone());
    }
    bindings
}

/// The members a script destructures from a composable, by the name the
/// script binds (upstream's `destructured`): `{ close }` binds `close`,
/// `{ close: closeSidebar }` binds `closeSidebar`; a later destructure of a
/// name replaces an earlier one, as upstream's `Map` keeps the last.
fn destructured(script: &str) -> HashMap<String, Member> {
    let mut members = HashMap::new();
    for found in DESTRUCTURE.captures_iter(script) {
        let composable = &found[2];
        if !COMPOSABLE.is_match(composable) {
            continue;
        }
        for part in found[1].split(',') {
            let Some(member) = MEMBER.captures(part.trim_matches(is_js_space)) else {
                continue;
            };
            let key = &member[1];
            let bound = member.get(2).map_or(key, |alias| alias.as_str());
            members.insert(
                bound.to_string(),
                Member {
                    composable: composable.to_string(),
                    key: key.to_string(),
                },
            );
        }
    }
    members
}

/// `el-button` as `ElButton` (upstream `kebabToPascal`): each `-`-separated
/// part with its first character uppercased. JavaScript uppercases the
/// first UTF-16 unit, so a part starting past the Basic Multilingual Plane
/// keeps its first character.
fn kebab_to_pascal(name: &str) -> String {
    name.split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) if u32::from(first) <= 0xFFFF => {
                    first.to_uppercase().chain(chars).collect::<String>()
                }
                Some(first) => std::iter::once(first).chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect()
}

/// The name Nuxt auto-imports a nested component under (upstream
/// `nuxtComponentName`): its path after the last `components/`, without one
/// `.vue`, `.ts`, `.tsx`, `.js` or `.jsx` (in any case), each segment
/// PascalCased and the segments joined, a segment that starts with the one
/// before replacing it (`base/BaseButton.vue` is `BaseButton`); `None` for
/// a flat component, which its node's own name already names.
fn nuxt_component_name(file: &str) -> Option<String> {
    let marker = file.rfind("components/")?;
    let relative = without_extension(&file[marker + "components/".len()..]);
    let segments: Vec<String> = relative
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(kebab_to_pascal)
        .collect();
    if segments.len() < 2 {
        return None;
    }
    let mut joined: Vec<String> = Vec::new();
    for segment in segments {
        match joined.last_mut() {
            // An empty segment (`-`) is falsy upstream and never replaced.
            Some(previous) if !previous.is_empty() && segment.starts_with(previous.as_str()) => {
                *previous = segment;
            }
            _ => joined.push(segment),
        }
    }
    Some(joined.concat())
}

/// `path` without a trailing `.vue`, `.ts`, `.tsx`, `.js` or `.jsx`, in any
/// ASCII case (upstream `/\.(vue|ts|tsx|js|jsx)$/i`).
fn without_extension(path: &str) -> &str {
    for extension in [".vue", ".ts", ".tsx", ".js", ".jsx"] {
        let Some(start) = path.len().checked_sub(extension.len()) else {
            continue;
        };
        if path.is_char_boundary(start) && path[start..].eq_ignore_ascii_case(extension) {
            return &path[..start];
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::UNIX_EPOCH;

    use codegraph_core::node_id::hash_content;
    use codegraph_core::types::{Edge, FileRecord, Language, Node, NodeKind};
    use codegraph_store::Store;

    use super::{kebab_to_pascal, nuxt_component_name};

    /// A project of hand-written files whose index holds exactly the nodes
    /// given, in the shape the Vue and TypeScript extraction gives them.
    struct Fixture {
        dir: PathBuf,
        files: Vec<(String, String, Language)>,
        disk: Vec<(String, String)>,
        nodes: Vec<Node>,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "cg-vue-{name}-{}-{}",
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
                disk: Vec::new(),
                nodes: Vec::new(),
            }
        }

        /// The indexed file `path` holding `text`.
        fn file(mut self, path: &str, language: Language, text: &str) -> Self {
            self.files
                .push((path.to_string(), text.to_string(), language));
            self
        }

        /// A file on disk the index does not hold (a `package.json`).
        fn disk(mut self, path: &str, text: &str) -> Self {
            self.disk.push((path.to_string(), text.to_string()));
            self
        }

        fn node(mut self, node: Node) -> Self {
            self.nodes.push(node);
            self
        }

        /// The edges the pass draws as `kind source -> target metadata`,
        /// sorted, each end named `file name`.
        fn edges(&self) -> Vec<String> {
            let mut store = Store::open(&self.dir.join("index.db")).unwrap();
            store.upsert_nodes(&self.nodes).unwrap();
            for (path, text) in &self.disk {
                let full = self.dir.join(path);
                std::fs::create_dir_all(full.parent().unwrap()).unwrap();
                std::fs::write(&full, text).unwrap();
            }
            for (path, text, language) in &self.files {
                let full = self.dir.join(path);
                std::fs::create_dir_all(full.parent().unwrap()).unwrap();
                std::fs::write(&full, text).unwrap();
                store
                    .upsert_file(&FileRecord {
                        path: path.clone(),
                        content_hash: hash_content(text),
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
                .filter_map(|edge: Edge| {
                    let mut metadata = edge.metadata.clone()?;
                    let by = metadata["synthesizedBy"].as_str()?.to_string();
                    if by != "jsx-render" && by != "vue-handler" {
                        return None;
                    }
                    metadata.as_object_mut()?.remove("synthesizedBy");
                    Some(format!(
                        "{by} {} -> {} {metadata} line {}",
                        label(&edge.source),
                        label(&edge.target),
                        edge.line.unwrap()
                    ))
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

    fn node(kind: NodeKind, name: &str, file: &str, line: i64) -> Node {
        let language = if file.ends_with(".vue") {
            Language::Vue
        } else if file.ends_with(".swift") {
            Language::Swift
        } else {
            Language::TypeScript
        };
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

    /// The component a `.vue` file is, named after its file, at line 1.
    fn sfc(file: &str) -> Node {
        let name = file.rsplit('/').next().unwrap().trim_end_matches(".vue");
        node(NodeKind::Component, name, file, 1)
    }

    #[test]
    fn a_tag_renders_what_it_names_kebab_case_pascal_case_or_by_its_nuxt_name() {
        let app = "<template>\n  <el-button>go</el-button>\n  <MediaCard />\n  <Counter :start=\"1\"/>\n  <Transition><div/></Transition>\n  <base-layout-row></base-layout-row>\n</template>\n<script setup>\nimport Counter from './Counter.vue'\n</script>\n";
        let fixture = Fixture::new("tags")
            .file("src/App.vue", Language::Vue, app)
            .node(sfc("src/App.vue"))
            .node(sfc("src/ElButton.vue"))
            .node(node(
                NodeKind::Component,
                "Card",
                "src/components/media/Card.vue",
                1,
            ))
            .node(sfc("src/Counter.vue"))
            .node(node(
                NodeKind::Function,
                "BaseLayoutRow",
                "src/layout.ts",
                3,
            ))
            // A component's own tag (a recursive tree) is no edge.
            .file(
                "src/TreeItem.vue",
                Language::Vue,
                "<template>\n  <li><TreeItem v-for=\"c in children\" /></li>\n</template>\n",
            )
            .node(sfc("src/TreeItem.vue"));
        assert_eq!(
            fixture.edges(),
            [
                "jsx-render src/App.vue App -> src/Counter.vue Counter {\"via\":\"Counter\"} line 1",
                "jsx-render src/App.vue App -> src/ElButton.vue ElButton {\"via\":\"el-button\"} line 1",
                "jsx-render src/App.vue App -> src/components/media/Card.vue Card {\"via\":\"MediaCard\"} line 1",
                "jsx-render src/App.vue App -> src/layout.ts BaseLayoutRow {\"via\":\"base-layout-row\"} line 1",
            ]
        );
    }

    #[test]
    fn a_handler_names_a_function_or_a_member_a_composable_returns() {
        let panel = "<template>\n  <button @click=\"save\">a</button>\n  <form v-on:submit.prevent=\"send\">b</form>\n  <button @click=\" closeSidebar \">c</button>\n  <button @click=\"toggle(1)\">d</button>\n  <button @click=\"submit(form)\">e</button>\n  <button @click=\"() => go()\">f</button>\n  <button @click=\"reset(() => 1)\">g</button>\n  <button @click=\"$emit('x')\">h</button>\n  <button @click=\"$restart\">i</button>\n  <button @click=\"open = !open\">j</button>\n  <button @click=\"pick\">k</button>\n</template>\n<script setup>\nimport { useSidebar } from '../composables/useSidebar'\nconst { close: closeSidebar, toggle, reset } = useSidebar()\nconst { a: pick } = getStuff()\nfunction save() {}\nfunction send() {}\nfunction submit() {}\nfunction go() {}\nfunction $restart() {}\nlet open = false\n</script>\n";
        // Only the first script block is read: the second one's destructure
        // binds nothing.
        let two = "<script lang=\"ts\">\nexport default {}\n</script>\n<script setup lang=\"ts\">\nconst { close: shut } = useSidebar()\n</script>\n<template>\n  <button @click=\"shut\">x</button>\n</template>\n";
        let fixture = Fixture::new("handlers")
            .file("src/Panel.vue", Language::Vue, panel)
            .node(sfc("src/Panel.vue"))
            .node(node(NodeKind::Function, "save", "src/Panel.vue", 18))
            .node(node(NodeKind::Function, "send", "src/Panel.vue", 19))
            .node(node(NodeKind::Function, "submit", "src/Panel.vue", 20))
            .node(node(NodeKind::Function, "go", "src/Panel.vue", 21))
            .node(node(NodeKind::Function, "$restart", "src/Panel.vue", 22))
            .node(node(NodeKind::Variable, "open", "src/Panel.vue", 23))
            .node(node(
                NodeKind::Function,
                "useSidebar",
                "src/composables/useSidebar.ts",
                1,
            ))
            .node(node(
                NodeKind::Function,
                "close",
                "src/composables/useSidebar.ts",
                2,
            ))
            .node(node(
                NodeKind::Constant,
                "toggle",
                "src/composables/useSidebar.ts",
                3,
            ))
            .node(node(
                NodeKind::Function,
                "reset",
                "src/composables/useSidebar.ts",
                4,
            ))
            // Another file's `close` is no member of this composable.
            .node(node(NodeKind::Function, "close", "src/other.ts", 1))
            // `getStuff` is no composable: its name does not start `use`.
            .node(node(NodeKind::Function, "getStuff", "src/stuff.ts", 1))
            .node(node(NodeKind::Function, "a", "src/stuff.ts", 2))
            .file("src/Two.vue", Language::Vue, two)
            .node(sfc("src/Two.vue"));
        assert_eq!(
            fixture.edges(),
            [
                "vue-handler src/Panel.vue Panel -> src/Panel.vue save {\"event\":\"click\"} line 1",
                "vue-handler src/Panel.vue Panel -> src/Panel.vue send {\"event\":\"submit\"} line 1",
                "vue-handler src/Panel.vue Panel -> src/composables/useSidebar.ts close {\"event\":\"click\",\"via\":\"useSidebar\"} line 1",
                "vue-handler src/Panel.vue Panel -> src/composables/useSidebar.ts toggle {\"event\":\"click\",\"via\":\"useSidebar\"} line 1",
            ]
        );
    }

    #[test]
    fn a_name_imported_from_a_package_is_the_packages() {
        let dashboard = "<script setup lang=\"ts\">\nimport { Button, Card, UiCard } from 'ant-design-vue';\nimport { useFullscreen } from '@vueuse/core';\nimport AppHeader from '../components/AppHeader.vue';\n\nconst { toggle: toggleFullscreen } = useFullscreen();\n</script>\n\n<template>\n  <AppHeader />\n  <Card title=\"Usage\">\n    <Button @click=\"toggleFullscreen\">Full screen</Button>\n  </Card>\n  <UiCard />\n</template>\n";
        // The same tags in a file that imports nothing are the project's.
        let settings = "<template>\n  <Card title=\"Account\">\n    <Button>Save</Button>\n  </Card>\n</template>\n";
        let fixture = Fixture::new("package")
            .disk(
                "package.json",
                "{\"dependencies\":{\"vue\":\"^3.4.0\",\"ant-design-vue\":\"^4.0.0\",\"@vueuse/core\":\"^10.0.0\"}}",
            )
            .file("src/views/Dashboard.vue", Language::Vue, dashboard)
            .node(sfc("src/views/Dashboard.vue"))
            .file("src/views/Settings.vue", Language::Vue, settings)
            .node(sfc("src/views/Settings.vue"))
            .node(sfc("src/components/ui/card/Card.vue"))
            .node(sfc("src/components/ui/button/Button.vue"))
            .node(sfc("src/components/AppHeader.vue"))
            .node(node(NodeKind::Function, "useFullscreen", "src/composables/useFullscreen.ts", 1))
            .node(node(NodeKind::Function, "toggle", "src/composables/useFullscreen.ts", 2));
        // `UiCard` is `components/ui/card/Card.vue`'s Nuxt name, and the
        // package's here.
        assert_eq!(
            fixture.edges(),
            [
                "jsx-render src/views/Dashboard.vue Dashboard -> src/components/AppHeader.vue AppHeader {\"via\":\"AppHeader\"} line 1",
                "jsx-render src/views/Settings.vue Settings -> src/components/ui/button/Button.vue Button {\"via\":\"Button\"} line 1",
                "jsx-render src/views/Settings.vue Settings -> src/components/ui/card/Card.vue Card {\"via\":\"Card\"} line 1",
            ]
        );
    }

    #[test]
    fn a_name_several_nodes_answer_draws_nothing() {
        let a = "<template>\n  <Card/><Modal/><TableRow/><Ship/><Counter/>\n  <button @click=\"save\">s</button>\n  <button @click=\"shut\">c</button>\n</template>\n<script setup>\nconst { close: shut } = useDialog()\n</script>\n";
        let fixture = Fixture::new("ambiguous")
            .file("src/A.vue", Language::Vue, a)
            .node(sfc("src/A.vue"))
            // Two in the project, none in the file.
            .node(sfc("src/x/Card.vue"))
            .node(sfc("src/y/Card.vue"))
            // Two in the file, though one in the project too.
            .node(node(NodeKind::Function, "Modal", "src/A.vue", 7))
            .node(node(NodeKind::Class, "Modal", "src/A.vue", 9))
            .node(sfc("src/Modal.vue"))
            // Two components share the Nuxt name `TableRow`.
            .node(sfc("web/components/table/Row.vue"))
            .node(sfc("admin/components/table/Row.vue"))
            // The one `Ship` is Swift's, across a code boundary.
            .node(node(NodeKind::Class, "Ship", "ios/Ship.swift", 1))
            .node(sfc("src/Counter.vue"))
            .node(node(NodeKind::Method, "save", "src/A.vue", 7))
            .node(node(NodeKind::Function, "save", "src/A.vue", 8))
            // The composable's file holds two members named `close`.
            .node(node(NodeKind::Function, "useDialog", "src/useDialog.ts", 1))
            .node(node(NodeKind::Function, "close", "src/useDialog.ts", 2))
            .node(node(NodeKind::Constant, "close", "src/useDialog.ts", 4));
        // Only `Counter` remains: it names one node.
        assert_eq!(
            fixture.edges(),
            ["jsx-render src/A.vue A -> src/Counter.vue Counter {\"via\":\"Counter\"} line 1"]
        );
        // A file with two components is no single-file component.
        let two = Fixture::new("two-components")
            .file("src/B.vue", Language::Vue, a)
            .node(sfc("src/B.vue"))
            .node(node(NodeKind::Component, "Inner", "src/B.vue", 3))
            .node(sfc("src/Counter.vue"));
        assert_eq!(two.edges(), Vec::<String>::new());
    }

    #[test]
    fn a_component_draws_thirty_edges_and_a_repeated_one_is_not_counted() {
        let tags: String = std::iter::once("<C1/>".to_string())
            .chain((1..=31).map(|i| format!("<C{i}/>")))
            .collect();
        let text = format!("<template>{tags}</template>\n");
        let mut fixture = Fixture::new("cap")
            .file("src/Big.vue", Language::Vue, &text)
            .node(sfc("src/Big.vue"));
        for i in 1..=31 {
            fixture = fixture.node(sfc(&format!("src/C{i}.vue")));
        }
        let edges = fixture.edges();
        assert_eq!(edges.len(), 30, "{edges:#?}");
        assert!(edges.iter().all(|edge| !edge.contains("C31")), "{edges:#?}");
    }

    #[test]
    fn the_template_runs_to_its_last_close_and_is_read_in_any_case() {
        let nested =
            "<TEMPLATE>\n  <template v-if=\"x\"><First/></template>\n  <Second/>\n</Template>\n";
        let fixture = Fixture::new("template")
            .file("src/N.vue", Language::Vue, nested)
            .node(sfc("src/N.vue"))
            // A tag outside the template is none of its.
            .file(
                "src/Empty.vue",
                Language::Vue,
                "<template></template>\n<First/>\n",
            )
            .node(sfc("src/Empty.vue"))
            .file(
                "src/None.vue",
                Language::Vue,
                "<script setup>\nconst x = '<First/>'\n</script>\n",
            )
            .node(sfc("src/None.vue"))
            // Only a `.vue` file has a template.
            .file(
                "src/widget.ts",
                Language::TypeScript,
                "export const t = '<template><Second/></template>';\n",
            )
            .node(node(NodeKind::Component, "Widget", "src/widget.ts", 1))
            .node(sfc("src/First.vue"))
            .node(sfc("src/Second.vue"));
        assert_eq!(
            fixture.edges(),
            [
                "jsx-render src/N.vue N -> src/First.vue First {\"via\":\"First\"} line 1",
                "jsx-render src/N.vue N -> src/Second.vue Second {\"via\":\"Second\"} line 1",
            ]
        );
    }

    #[test]
    fn a_nuxt_name_joins_the_directories_under_components() {
        let name = |file: &str| nuxt_component_name(file);
        assert_eq!(
            name("components/media/Card.vue").as_deref(),
            Some("MediaCard")
        );
        assert_eq!(
            name("app/components/base/BaseButton.vue").as_deref(),
            Some("BaseButton")
        );
        assert_eq!(
            name("components/base/foo/Bar.vue").as_deref(),
            Some("BaseFooBar")
        );
        assert_eq!(name("components/NavBar.vue"), None);
        // The last `components/` of the path counts.
        assert_eq!(
            name("components/foo/components/bar/Baz.vue").as_deref(),
            Some("BarBaz")
        );
        assert_eq!(
            name("src/mycomponents/form/text-input.TSX").as_deref(),
            Some("FormTextInput")
        );
        assert_eq!(name("components/a/B.svelte").as_deref(), Some("AB.svelte"));
        assert_eq!(name("components/media/Card").as_deref(), Some("MediaCard"));
        assert_eq!(name("x/components/-/Card.vue").as_deref(), Some("Card"));
        assert_eq!(name("src/views/Card.vue"), None);
        assert_eq!(kebab_to_pascal("el-button"), "ElButton");
        assert_eq!(kebab_to_pascal("a--b"), "AB");
        assert_eq!(kebab_to_pascal("\u{10428}x-y"), "\u{10428}xY");
    }
}
