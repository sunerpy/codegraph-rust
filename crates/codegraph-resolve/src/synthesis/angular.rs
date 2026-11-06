//! The Angular template pass (W7-18d): upstream
//! `angular-template-synthesizer.ts` at `e8ddc4e2` (#2113, #2114, #2398).
//!
//! An Angular component's markup is a template, a `templateUrl` file beside
//! it or an inline `template:` string, that the index never parses. For each
//! `@Component` class of an indexed TypeScript file, by path, its template
//! draws edges from the class:
//!
//! - `angular-template`: a `calls` edge to each child component an element
//!   tag renders, matched by its element selector (`selector:
//!   'app-article-list'`), in the same app first, nothing for a selector two
//!   components share there;
//! - `angular-event`: a `calls` edge to each of the class's own methods an
//!   event binding's statement calls (`(click)="save()"`), the binding as its
//!   `trigger`;
//! - `angular-binding`: a `calls` edge to each own member a property binding,
//!   interpolation, structural directive, control-flow block or `@let` calls
//!   at render (`[name]="icon()"`, `@if (loading())`), and to a getter it
//!   reads; a `references` edge to another method it only reads, `fnRef` for
//!   a function handed over (`[displayWith]="displayFn"`);
//! - `angular-router-link`: a `navigates` edge to each route a `routerLink`
//!   of the template, or a `routerLink:` field of the class, names in the
//!   component's app.
//!
//! A `templateUrl` file the index holds is read as it holds it; any other is
//! read beside the index and recorded (W7-17b's reader, OD-B18), so a sync
//! that finds it changed recomputes the stage.
//!
//! KEEP-RUST: an inline `template:` is read only when its value is a string
//! or template literal (upstream searches the file for the value's first
//! character as a quote); a link's destination keeps only the arms a
//! navigation call's keeps (`navigable_arms`).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use anyhow::Result;
use codegraph_core::file_class::is_test_path;
use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde_json::{Value, json};

use super::c_fnptr::{posix_dirname, posix_normalize};
use super::dispatch::synthesized;
use super::stage::View;
use crate::frameworks::angular_router::{
    LinkDestinations, angular_link_routes, plain_link_destination, static_string,
};
use crate::frameworks::js_scan::{
    JS_SPACE, is_js_space, js_trim, match_bracket, read_fields, skip_string,
};
use crate::frameworks::package_deps::depends_on;
use crate::frameworks::route_href::app_root_for;
use crate::strip_comments::strip_js_comments_keeping_offsets;

/// Links one component draws before it is a navigation menu rather than a
/// decision (upstream `MAX_LINKS_PER_COMPONENT`).
const MAX_LINKS: usize = 24;

/// Children, and handlers, one template draws at most (upstream
/// `MAX_CHILDREN_PER_COMPONENT`).
const MAX_CHILDREN: usize = 40;

/// `pattern` with JavaScript's `\s`; its other classes are written out
/// ASCII, as JavaScript's are.
fn js(pattern: &str) -> Regex {
    Regex::new(&pattern.replace(r"\s", JS_SPACE)).expect("an Angular template pattern compiles")
}

/// `@Component(`.
static DECORATOR: LazyLock<Regex> = LazyLock::new(|| js(r"@Component\s*\("));

/// The class a decorator decorates, right after it.
static DECORATED_CLASS: LazyLock<Regex> = LazyLock::new(|| {
    js(r"^\s*(?:export\s+)?(?:default\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][A-Za-z0-9_$]*)")
});

/// An element selector: `app-article-list`, not `[appDirective]` or
/// `button[app-x]` (upstream `ELEMENT_SELECTOR`).
static ELEMENT_SELECTOR: LazyLock<Regex> = LazyLock::new(|| js(r"^[a-zA-Z][A-Za-z0-9_-]*$"));

/// An opening tag's name (upstream `TAG`, `/<([a-zA-Z][\w-]*)(?=[\s/>])/`):
/// the lookahead is [`tag_ends`].
static TAG: LazyLock<Regex> = LazyLock::new(|| js(r"<([a-zA-Z][A-Za-z0-9_-]*)"));

/// The element a binding is written on, at the last `<` before it.
static ELEMENT: LazyLock<Regex> = LazyLock::new(|| js(r"^<([a-zA-Z][A-Za-z0-9_-]*)"));

/// `routerLink="…"` (plain) and `[routerLink]="…"` (bound), either quote;
/// `routerLinkActive` is neither (upstream `ROUTER_LINK`).
static ROUTER_LINK: LazyLock<Regex> =
    LazyLock::new(|| js(r#"(\[routerLink\]|(?-u:\b)routerLink)\s*=\s*(?:"([^"]*)"|'([^']*)')"#));

/// `(click)="save()"`, `(ngSubmit)="submitForm()"`: an event binding
/// (upstream `EVENT_BINDING`, `(?<!\[)` checked by [`after_ok`]), not the
/// event half of `[(ngModel)]`.
static EVENT_BINDING: LazyLock<Regex> =
    LazyLock::new(|| js(r#"\(([A-Za-z][A-Za-z0-9_.-]*)\)\s*=\s*(?:"([^"]*)"|'([^']*)')"#));

/// A call a template statement makes on its component: `save()`,
/// `this.toggle(item)`, not `form.reset()` (upstream `OWN_CALL`, its
/// `(?<![\w$.])` checked by [`after_ok`]).
static OWN_CALL: LazyLock<Regex> =
    LazyLock::new(|| js(r"(?:this\s*\.\s*)?([A-Za-z_$][A-Za-z0-9_$]*)\s*\("));

/// A `routerLink:` field of an object literal written in a class (upstream
/// `ROUTER_LINK_FIELD`, its `(?<![\w$.])` checked by [`after_ok`]).
static ROUTER_LINK_FIELD: LazyLock<Regex> = LazyLock::new(|| js(r"routerLink\s*:\s*"));

/// `[name]="…"`, `[attr.aria-label]="…"`, `[class.done]="…"`,
/// `[(ngModel)]="…"`: a binding whose value is an expression (upstream
/// `PROPERTY_BINDING`).
static PROPERTY_BINDING: LazyLock<Regex> = LazyLock::new(|| {
    js(
        r#"\[(\([A-Za-z][A-Za-z0-9_.-]*\)|[A-Za-z@][A-Za-z0-9_.@-]*)\]\s*=\s*(?:"([^"]*)"|'([^']*)')"#,
    )
});

/// `*ngIf="…"`, `*ngFor="let item of items; trackBy: trackById"`: a
/// structural directive, in its microsyntax (upstream
/// `STRUCTURAL_DIRECTIVE`, its `(?<![\w$*-])` checked by [`after_ok`]).
static STRUCTURAL_DIRECTIVE: LazyLock<Regex> =
    LazyLock::new(|| js(r#"\*([A-Za-z][A-Za-z0-9_.-]*)\s*=\s*(?:"([^"]*)"|'([^']*)')"#));

/// `{{ … }}`, in text or inside an attribute's value (upstream
/// `INTERPOLATION`).
static INTERPOLATION: LazyLock<Regex> = LazyLock::new(|| js(r"(?s)\{\{(.*?)\}\}"));

/// A control-flow block that takes an expression: `@if (…) {`, `@else if
/// (…)`, `@for (…)`, `@switch (…)`, `@case (…)`, `@defer (…)` (upstream
/// `CONTROL_FLOW`, its `(?<![\w$@])` checked by [`after_ok`]).
static CONTROL_FLOW: LazyLock<Regex> =
    LazyLock::new(|| js(r"@(if|else\s+if|for|switch|case|defer)\s*\("));

/// `@let total = price() * quantity();`: a template's own constant
/// (upstream `LET_DECLARATION`, its `(?<![\w$@])` checked by [`after_ok`]).
static LET_DECLARATION: LazyLock<Regex> =
    LazyLock::new(|| js(r"@let\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*="));

/// A template reference variable, `#nameInput` or `ref-nameInput`, which the
/// whole template sees (upstream `REFERENCE_VARIABLE`, its `(?<=\s)` and
/// `(?=[\s=/>])` checked by [`declared_name_ok`]).
static REFERENCE_VARIABLE: LazyLock<Regex> =
    LazyLock::new(|| js(r"(?:#|ref-)([A-Za-z_$][A-Za-z0-9_$]*)"));

/// `let-row`, `let-i="index"` on an `<ng-template>`: a local of what it
/// renders (upstream `TEMPLATE_INPUT`, checked as
/// [`REFERENCE_VARIABLE`] is).
static TEMPLATE_INPUT: LazyLock<Regex> = LazyLock::new(|| js(r"let-([A-Za-z_$][A-Za-z0-9_$]*)"));

/// An HTML comment (upstream `HTML_COMMENT`).
static HTML_COMMENT: LazyLock<Regex> = LazyLock::new(|| js(r"(?s)<!--.*?-->"));

/// A name an expression writes, `this.` maybe leading it; a member of
/// something else (`form.reset`) is not one (upstream `EXPRESSION_NAME`, its
/// `(?<![\w$.])` checked by [`after_ok`]).
static EXPRESSION_NAME: LazyLock<Regex> =
    LazyLock::new(|| js(r"(?:this\s*\.\s*)?([A-Za-z_$][A-Za-z0-9_$]*)"));

/// A name at the start of a microsyntax's rest (upstream `NAME_AT`).
static NAME_AT: LazyLock<Regex> = LazyLock::new(|| js(r"^[A-Za-z_$][A-Za-z0-9_$]*"));

/// `@defer`'s `when`, maybe after `prefetch` or `hydrate`.
static DEFER_WHEN: LazyLock<Regex> = LazyLock::new(|| js(r"^\s*(?:(?:prefetch|hydrate)\s+)?when"));

/// `@for`'s head, `item of`.
static FOR_HEAD: LazyLock<Regex> = LazyLock::new(|| js(r"^(\s*)([A-Za-z_$][A-Za-z0-9_$]*)\s+of"));

/// `@for`'s `track`.
static FOR_TRACK: LazyLock<Regex> = LazyLock::new(|| js(r"^\s*track"));

/// `@for`'s `let`.
static FOR_LET: LazyLock<Regex> = LazyLock::new(|| js(r"^\s*let"));

/// One `name = $context` of `@for`'s `let`.
static FOR_ALIAS: LazyLock<Regex> =
    LazyLock::new(|| js(r"([A-Za-z_$][A-Za-z0-9_$]*)\s*=\s*\$?[A-Za-z0-9_$]+"));

/// `@if`'s `as alias`.
static IF_ALIAS: LazyLock<Regex> = LazyLock::new(|| js(r"^\s*as\s+([A-Za-z_$][A-Za-z0-9_$]*)"));

/// A field's or a property's `= value` after its name and type (upstream's
/// `/^[?!]?\s*(?::[^=;]*(?:=>[^=;]*)*)?=(?![=>])\s*/`, the lookahead the
/// character after the first group).
static FIELD_ASSIGN: LazyLock<Regex> =
    LazyLock::new(|| js(r"^([?!]?\s*(?::[^=;]*(?:=>[^=;]*)*)?=)(?:[^=>]|$)"));

/// A value that is a function: `function`, or `name =>`.
static FUNCTION_VALUE: LazyLock<Regex> =
    LazyLock::new(|| js(r"^(?:function(?:[^A-Za-z0-9_$]|$)|[A-Za-z_$][A-Za-z0-9_$]*\s*=>)"));

/// An arrow after a parenthesized parameter list, maybe typed.
static ARROW_AFTER_PARAMS: LazyLock<Regex> = LazyLock::new(|| js(r"^\s*(?::[^=]*)?=>"));

/// A JavaScript identifier character, `[\w$]`.
fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

/// Whether the byte before `at` in `text` is none of `excluded`, as a
/// lookbehind `(?<![…])` asks: the start of the text passes, and so does a
/// non-ASCII character, which no excluded byte is.
fn after_ok(text: &str, at: usize, excluded: impl Fn(u8) -> bool) -> bool {
    at == 0 || !excluded(text.as_bytes()[at - 1])
}

/// Each match of `pattern` in `text` whose start passes `start_ok`, found as
/// a JavaScript `g` regex with a lookbehind finds them: a start that fails
/// is retried one byte later, an accepted match continues after its end.
fn matches_where<'t>(
    pattern: &Regex,
    text: &'t str,
    mut start_ok: impl FnMut(&regex::Captures<'t>) -> bool,
) -> Vec<regex::Captures<'t>> {
    let mut out = Vec::new();
    let mut from = 0;
    while from <= text.len() {
        let Some(found) = pattern.captures_at(text, from) else {
            break;
        };
        let whole = found.get(0).expect("the whole match");
        if start_ok(&found) {
            from = if whole.end() > whole.start() {
                whole.end()
            } else {
                whole.end() + 1
            };
            out.push(found);
        } else {
            from = whole.start() + 1;
        }
        while from < text.len() && !text.is_char_boundary(from) {
            from += 1;
        }
    }
    out
}

/// The 1-based line of offset `at` in `text` (upstream `lineOfOffset`).
fn line_of_offset(text: &str, at: usize) -> i64 {
    let end = at.min(text.len());
    1 + text.as_bytes()[..end]
        .iter()
        .filter(|&&b| b == b'\n')
        .count() as i64
}

/// Whether `c`, the character at a lookbehind's place, is JavaScript's `\s`.
fn is_blank_at(text: &str, at: Option<usize>) -> bool {
    at.and_then(|at| text[at..].chars().next())
        .is_some_and(is_js_space)
}

/// A template file's path, from the component file that names it (upstream
/// `templatePathFor`): `path.posix.normalize(path.posix.join(dirname, url))`.
fn template_path_for(component_file: &str, template_url: &str) -> String {
    posix_normalize(&format!(
        "{}/{}",
        posix_dirname(component_file),
        template_url
    ))
}

/// A component's template: the file it is read from, its text, and the
/// line its first character is on (1 for a template file).
struct Template {
    file: String,
    text: String,
    first_line: i64,
    inline: bool,
}

/// One `@Component` class with its element selectors and template
/// (upstream `AngularComponent`), its file's text and its own methods and
/// properties, by line.
struct Component {
    node: Node,
    file: String,
    selectors: Vec<String>,
    template: Option<Template>,
    source: Arc<str>,
    own: Vec<Node>,
}

/// Every `@Component` class `file` declares (upstream `componentsIn`).
fn components_in(
    view: &View<'_>,
    indexed: &HashSet<String>,
    file: &str,
    content: &Arc<str>,
    nodes: &[Node],
) -> Vec<Component> {
    let mut out = Vec::new();
    let safe = strip_js_comments_keeping_offsets(content);
    let mut from = 0;
    while let Some(found) = DECORATOR.find_at(&safe, from) {
        let paren = found.end() - 1;
        let Some(close) = match_bracket(&safe, paren) else {
            from = found.end();
            continue;
        };
        from = close;
        let Some(open) = safe[paren..].find('{').map(|at| paren + at) else {
            continue;
        };
        if open > close {
            continue;
        }
        let Some(obj_end) = match_bracket(&safe, open) else {
            continue;
        };
        let after: String = safe[close + 1..].chars().take(399).collect();
        let Some(class) = DECORATED_CLASS.captures(&after) else {
            continue;
        };
        let name = &class[1];
        let Some(node) = nodes
            .iter()
            .find(|node| node.kind == NodeKind::Class && node.name == name)
        else {
            continue;
        };
        let fields = read_fields(&safe, open, obj_end);
        let field = |key: &str| fields.iter().find(|field| field.key == key);
        let selectors = field("selector")
            .and_then(|field| static_string(field.text))
            .map(|selector| {
                selector
                    .split(',')
                    .map(js_trim)
                    .filter(|selector| ELEMENT_SELECTOR.is_match(selector))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let template_url = field("templateUrl")
            .and_then(|field| static_string(field.text))
            .filter(|url| !url.is_empty());
        let template = if let Some(url) = template_url {
            let path = template_path_for(file, &url);
            template_text(view, indexed, &path)
                .filter(|text| !text.is_empty())
                .map(|text| Template {
                    file: path,
                    text: text.to_string(),
                    first_line: 1,
                    inline: false,
                })
        } else {
            field("template")
                .and_then(|inline| inline_template(content, inline.at, inline.text, file))
        };
        // The class's own methods and properties (upstream filters the
        // file's nodes by qualified name in the pass).
        let prefix = format!("{}::", node.qualified_name);
        let own = nodes
            .iter()
            .filter(|member| {
                matches!(member.kind, NodeKind::Method | NodeKind::Property)
                    && member.qualified_name.starts_with(&prefix)
            })
            .cloned()
            .collect();
        out.push(Component {
            node: node.clone(),
            file: file.to_string(),
            selectors,
            template,
            source: Arc::clone(content),
            own,
        });
    }
    out
}

/// An inline `template:`'s body, from the original source (stripping
/// comments must not touch a template's text), at its own offset. KEEP-RUST:
/// only a string or template literal; upstream takes the value's first
/// character, whatever it is, for the quote to search for and skip to.
fn inline_template(content: &str, key_at: usize, value: &str, file: &str) -> Option<Template> {
    let quote = js_trim(value).chars().next().unwrap_or('`');
    if !matches!(quote, '`' | '\'' | '"') {
        return None;
    }
    let search_from = key_at + "template".len();
    let quote_at = search_from + content.get(search_from..)?.find(quote)?;
    let end = skip_string(content, quote_at)?;
    (end > quote_at).then(|| Template {
        file: file.to_string(),
        text: content[quote_at + 1..end].to_string(),
        first_line: line_of_offset(content, quote_at + 1),
        inline: true,
    })
}

/// A template file's text: an indexed file's as the index holds it, any
/// other's as the stage reads the files the index does not hold, recorded
/// (OD-B18; the order of W7-17b's `resolve_include`).
fn template_text(view: &View<'_>, indexed: &HashSet<String>, path: &str) -> Option<Arc<str>> {
    if indexed.contains(path) {
        return view.text(path);
    }
    let relative = View::unindexed_path(path)?;
    if indexed.contains(&relative) {
        return view.text(&relative);
    }
    view.unindexed_text(&relative)
}

/// Every component of the project's indexed TypeScript files, by path, the
/// test files left out (upstream `angularComponents`).
fn angular_components(view: &View<'_>) -> Result<Vec<Component>> {
    let files = view.files()?;
    let indexed: HashSet<String> = files.iter().cloned().collect();
    let mut components = Vec::new();
    for file in &files {
        if !is_component_source(file) || is_test_path(file) {
            continue;
        }
        let Some(content) = view.text(file) else {
            continue;
        };
        if !content.contains("@Component") {
            continue;
        }
        let nodes = view.nodes_in_file(file)?;
        components.extend(components_in(view, &indexed, file, &content, &nodes));
    }
    Ok(components)
}

/// `/\.[cm]?ts$/`, not a `.d.ts`.
fn is_component_source(file: &str) -> bool {
    (file.ends_with(".ts") || file.ends_with(".cts") || file.ends_with(".mts"))
        && !file.ends_with(".d.ts")
}

/// Whether a project of these file languages can hold a component: its
/// classes are TypeScript.
pub(super) fn gate(languages: &HashSet<Language>) -> bool {
    languages.contains(&Language::TypeScript)
}

/// Angular templates (`angularTemplateEdges`): in a project that depends
/// on `@angular/core`, each component's template, by component, draws its
/// children, its event handlers, the members it reads at render and its
/// links.
pub(super) fn template_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let context = view.context();
    if !depends_on(&context, &["@angular/core"]) {
        return Ok(Vec::new());
    }
    let components = angular_components(view)?;
    if components.is_empty() {
        return Ok(Vec::new());
    }
    // Element selector → the components that answer to it.
    let mut by_selector: HashMap<&str, Vec<&Component>> = HashMap::new();
    for component in &components {
        for selector in &component.selectors {
            by_selector.entry(selector).or_default().push(component);
        }
    }
    let child_for = |tag: &str, parent: &Component| -> Option<&Component> {
        match by_selector.get(tag)?.as_slice() {
            [] => None,
            [only] => Some(*only),
            all => {
                let root = app_root_for(&parent.file);
                let near: Vec<&Component> = all
                    .iter()
                    .copied()
                    .filter(|candidate| candidate.file.starts_with(&root))
                    .collect();
                match near.as_slice() {
                    [only] => Some(*only),
                    _ => None,
                }
            }
        }
    };
    let table = angular_link_routes(&context);
    let destinations = LinkDestinations::new(&context);
    let mut edges = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for component in &components {
        let Some(template) = &component.template else {
            continue;
        };
        let text = template.text.as_str();
        // Where an edge is written: the tag's own line for an inline
        // template; for a template file, the class, the file the edge's
        // source is in.
        let site_line = |at: usize| {
            if template.inline {
                template.first_line + line_of_offset(text, at) - 1
            } else {
                component.node.start_line
            }
        };
        let registered_at = |at: usize| {
            format!(
                "{}:{}",
                template.file,
                template.first_line + line_of_offset(text, at) - 1
            )
        };

        let mut children = 0;
        for found in matches_where(&TAG, text, |found| {
            tag_ends(text, found.get(0).expect("tag").end())
        }) {
            if children >= MAX_CHILDREN {
                break;
            }
            let tag = &found[1];
            let Some(child) = child_for(tag, component) else {
                continue;
            };
            if child.node.id == component.node.id
                || !seen.insert(format!("{}>{}", component.node.id, child.node.id))
            {
                continue;
            }
            children += 1;
            let at = found.get(0).expect("tag").start();
            edges.push(synthesized(
                &component.node,
                &child.node,
                EdgeKind::Calls,
                site_line(at),
                json!({"synthesizedBy": "angular-template", "via": tag, "registeredAt": registered_at(at)}),
            ));
        }

        // Event bindings: `(click)="toggleFavorite()"` runs the component's
        // own method when the user acts. The binding is the trigger the
        // steps draw, known only from the template, so it rides on the edge.
        // The last method declared under a name, as a `Map` built in line
        // order keeps it.
        let members: HashMap<&str, &Node> = component
            .own
            .iter()
            .filter(|node| node.kind == NodeKind::Method)
            .map(|node| (node.name.as_str(), node))
            .collect();
        // Members this template already links: only the first edge between
        // two nodes survives the merge.
        let mut linked: HashSet<String> = HashSet::new();
        let mut handlers = 0;
        for event_binding in matches_where(&EVENT_BINDING, text, |found| {
            after_ok(text, found.get(0).expect("event").start(), |b| b == b'[')
        }) {
            if handlers >= MAX_CHILDREN {
                break;
            }
            let statement = event_binding
                .get(2)
                .or_else(|| event_binding.get(3))
                .map_or("", |value| value.as_str());
            let event = format!("({})", &event_binding[1]);
            let at = event_binding.get(0).expect("event").start();
            let element = element_at(text, at);
            for call in matches_where(&OWN_CALL, statement, |found| {
                after_ok(statement, found.get(0).expect("call").start(), |b| {
                    is_name_byte(b) || b == b'.'
                })
            }) {
                let Some(method) = members.get(&call[1]) else {
                    continue;
                };
                if !seen.insert(format!("{}>{}>{}", component.node.id, method.id, event)) {
                    continue;
                }
                linked.insert(method.id.clone());
                handlers += 1;
                edges.push(synthesized(
                    &component.node,
                    method,
                    EdgeKind::Calls,
                    site_line(at),
                    json!({
                        "synthesizedBy": "angular-event",
                        "via": event,
                        "registeredAt": registered_at(at),
                        "trigger": {"kind": "prop", "name": event, "of": element},
                    }),
                ));
            }
        }

        // Bindings: `[name]="icon()"`, `{{ label() }}`, `@if (loading())`
        // read the component every time it renders, so the hop carries no
        // trigger. At most one edge per member, so the class's own size
        // bounds them.
        let lines: Vec<&str> = component.source.split('\n').collect();
        let mut shapes: HashMap<String, Shape> = HashMap::new();
        let mut shape = |node: &Node| -> Shape {
            *shapes
                .entry(node.id.clone())
                .or_insert_with(|| member_shape(&lines, node))
        };
        let mut named: HashMap<&str, Vec<&Node>> = HashMap::new();
        for node in &component.own {
            named.entry(node.name.as_str()).or_default().push(node);
        }
        for member_use in template_member_uses(text) {
            let Some((target, kind, fn_ref)) = binding_target(
                named.get(member_use.name.as_str()),
                member_use.call,
                &mut shape,
            ) else {
                continue;
            };
            if !linked.insert(target.id.clone()) {
                continue;
            }
            let mut metadata = json!({
                "synthesizedBy": "angular-binding",
                "via": member_use.via,
                "registeredAt": registered_at(member_use.at),
            });
            if fn_ref {
                metadata["fnRef"] = Value::Bool(true);
            }
            edges.push(synthesized(
                &component.node,
                target,
                kind,
                site_line(member_use.at),
                metadata,
            ));
        }

        if !table.serves(&component.file) {
            continue;
        }
        let mut links = 0;
        // A tab bar or menu built in the class, `this.tabs = [{ label,
        // routerLink: internalRoutes.home.subRoutes.summary.routerLink }]`,
        // handed to a tabs component whose template binds
        // `[routerLink]="tab.routerLink"` in a loop: the destination is the
        // field this class wrote.
        {
            let first = usize::try_from(component.node.start_line - 1).unwrap_or(0);
            let last = usize::try_from(component.node.end_line).unwrap_or(0);
            let body = lines
                .iter()
                .skip(first)
                .take(last.saturating_sub(first))
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
            for field in matches_where(&ROUTER_LINK_FIELD, &body, |found| {
                after_ok(&body, found.get(0).expect("field").start(), |b| {
                    is_name_byte(b) || b == b'.'
                })
            }) {
                if links >= MAX_LINKS {
                    break;
                }
                let whole = field.get(0).expect("field");
                let Some(href) = field_value(&body, whole.end())
                    .and_then(|value| destinations.of(value, &component.file, &component.node))
                else {
                    continue;
                };
                let line = component.node.start_line + line_of_offset(&body, whole.start()) - 1;
                for (route, arm) in table.destinations(&component.file, &href) {
                    if !seen.insert(format!("{}>{}", component.node.id, route.id)) {
                        continue;
                    }
                    links += 1;
                    edges.push(synthesized(
                        &component.node,
                        &route,
                        EdgeKind::Navigates,
                        line,
                        json!({
                            "synthesizedBy": "angular-router-link",
                            "href": arm.display,
                            "navMethod": "routerLink",
                            "registeredAt": format!("{}:{line}", component.file),
                        }),
                    ));
                }
            }
        }
        for link in ROUTER_LINK.captures_iter(text) {
            if links >= MAX_LINKS {
                break;
            }
            let value = link
                .get(2)
                .or_else(|| link.get(3))
                .map_or("", |value| value.as_str());
            let href = if &link[1] == "[routerLink]" {
                destinations.of(value, &component.file, &component.node)
            } else {
                plain_link_destination(value)
            };
            let Some(href) = href else {
                continue;
            };
            let at = link.get(0).expect("link").start();
            for (route, arm) in table.destinations(&component.file, &href) {
                if !seen.insert(format!("{}>{}", component.node.id, route.id)) {
                    continue;
                }
                links += 1;
                edges.push(synthesized(
                    &component.node,
                    &route,
                    EdgeKind::Navigates,
                    site_line(at),
                    json!({
                        "synthesizedBy": "angular-router-link",
                        "href": arm.display,
                        "navMethod": "routerLink",
                        "registeredAt": registered_at(at),
                        "template": true,
                    }),
                ));
            }
        }
    }
    Ok(edges)
}

/// Whether an opening tag's name ends at `end`: a space, `/` or `>` follows
/// it (`TAG`'s lookahead).
fn tag_ends(text: &str, end: usize) -> bool {
    text[end..]
        .chars()
        .next()
        .is_some_and(|c| is_js_space(c) || c == '/' || c == '>')
}

/// The element a binding at `at` is written on: the last opening tag before
/// it (upstream `elementAt`, its 64-unit window).
fn element_at(text: &str, at: usize) -> Option<String> {
    let open = text[..=at.min(text.len().saturating_sub(1))].rfind('<')?;
    let found = ELEMENT.captures(&text[open..])?;
    let name = &found[1];
    Some(name[..name.len().min(63)].to_string())
}

/// An object field's value text from `at`, up to its `,` or closing bracket
/// at depth 0 (upstream `fieldValue`).
fn field_value(text: &str, at: usize) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut i = at;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' | b'`' => i = skip_string(text, i)? + 1,
            b'[' | b'{' | b'(' => i = match_bracket(text, i)? + 1,
            b',' | b'}' | b']' | b')' | b';' => break,
            _ => i += 1,
        }
    }
    let value = js_trim(&text[at..i.min(bytes.len())]);
    (!value.is_empty()).then_some(value)
}

// =============================================================================
// What a template reads at render
// =============================================================================

/// A name a template expression calls (`icon()`) or only reads (`canSave`,
/// `displayFn`; upstream `TemplateMemberUse`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MemberUse {
    pub(super) name: String,
    /// The name's offset in the template.
    pub(super) at: usize,
    pub(super) call: bool,
    /// The binding the expression belongs to, as written: `[name]`, `{{ }}`,
    /// `*ngIf`, `@if`, `@let`.
    pub(super) via: String,
}

/// One expression, and the scope it is read in: the directive or block that
/// evaluates it, whose own locals it sees only where Angular says so
/// (`@for`'s `track` sees the loop variable; the `@if` condition does not
/// see its own `as` alias).
struct Expression {
    via: String,
    at: usize,
    text: String,
    scope: i64,
    sees_own_locals: bool,
}

/// A name the template declares, never the component's member, visible from
/// `from` on; its scope is the directive or block that declared it, -1 for a
/// reference variable or a `let-` input.
struct Local {
    name: String,
    from: usize,
    scope: i64,
}

/// `text` with each byte of each match of `pattern` but its newlines blanked
/// to a space, so offsets and lines survive.
fn blank_matches(text: &str, pattern: &Regex) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for found in pattern.find_iter(text) {
        out.push_str(&text[last..found.start()]);
        out.extend(
            found
                .as_str()
                .bytes()
                .map(|b| if b == b'\n' { '\n' } else { ' ' }),
        );
        last = found.end();
    }
    out.push_str(&text[last..]);
    out
}

/// Each string literal blanked to spaces, a byte a space, so a quoted
/// `'save()'` names nothing and offsets survive; a template literal keeps
/// its `${…}` holes, which are expressions (upstream `blankStrings`).
fn blank_strings(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => {
                let close = skip_string(text, i).map_or(bytes.len(), |end| end + 1);
                out.extend(std::iter::repeat_n(' ', close - i));
                i = close;
            }
            b'`' => {
                out.push(' ');
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'`' {
                        out.push(' ');
                        i += 1;
                        break;
                    }
                    let close = if bytes[i] == b'$' && bytes.get(i + 1) == Some(&b'{') {
                        match_bracket(text, i + 1)
                    } else {
                        None
                    };
                    if let Some(close) = close {
                        out.push_str("  ");
                        out.push_str(&blank_strings(&text[i + 2..close]));
                        out.push(' ');
                        i = close + 1;
                        continue;
                    }
                    let step = if bytes[i] == b'\\' {
                        1 + text[i + 1..].chars().next().map_or(0, char::len_utf8)
                    } else {
                        text[i..].chars().next().map_or(1, char::len_utf8)
                    };
                    out.extend(std::iter::repeat_n(' ', step));
                    i += step;
                }
            }
            _ => {
                let c = text[i..].chars().next().expect("a character");
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    out
}

/// The byte offset of the character before `at` that is not blank, or
/// `None` at the start.
fn before_blanks(text: &str, at: usize) -> Option<usize> {
    let (offset, _) = text[..at]
        .char_indices()
        .rev()
        .find(|(_, c)| !is_js_space(*c))?;
    Some(offset)
}

/// The names an expression calls or reads, minus a pipe's name and an
/// object literal's keys (upstream `expressionUses`).
fn expression_uses(expression: &str) -> Vec<(String, usize, bool)> {
    let text = blank_strings(expression);
    let mut out = Vec::new();
    for found in matches_where(&EXPRESSION_NAME, &text, |found| {
        after_ok(&text, found.get(0).expect("name").start(), |b| {
            is_name_byte(b) || b == b'.'
        })
    }) {
        let whole = found.get(0).expect("name");
        let name = &found[1];
        let end = whole.end();
        let before = before_blanks(&text, whole.start());
        let after = text[end..]
            .char_indices()
            .find(|(_, c)| !is_js_space(*c))
            .map(|(offset, _)| end + offset);
        let prev = before.map(|at| text.as_bytes()[at]);
        let next = after.map(|at| text.as_bytes()[at]);
        // `form .reset()`: a member of something else, however it is spaced.
        if prev == Some(b'.') {
            continue;
        }
        // A pipe's name: `total | currency: code()`; `a || b` is no pipe.
        if prev == Some(b'|')
            && before
                .and_then(|at| at.checked_sub(1))
                .map(|at| text.as_bytes()[at])
                != Some(b'|')
        {
            continue;
        }
        // An object literal's key: `[ngClass]="{ active: isActive() }"`.
        if matches!(prev, Some(b'{' | b',')) && next == Some(b':') {
            continue;
        }
        out.push((name.to_string(), end - name.len(), next == Some(b'(')));
    }
    out
}

/// Whether `text` at `i` starts the word `word`, not followed by `[\w$]`.
fn word_at(text: &str, i: usize, word: &str) -> bool {
    text[i..].starts_with(word)
        && !text
            .as_bytes()
            .get(i + word.len())
            .is_some_and(|&b| is_name_byte(b))
}

/// Where the expression starting at `from` ends: a `;` or `,` outside any
/// bracket, or the word `as` or `let` that opens the next microsyntax
/// binding (upstream `expressionEnd`).
fn expression_end(text: &str, from: usize) -> usize {
    let bytes = text.as_bytes();
    let mut depth: i64 = 0;
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' | b'`' => {
                let Some(end) = skip_string(text, i) else {
                    return bytes.len();
                };
                i = end + 1;
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b';' | b',' if depth == 0 => return i,
            _ if depth == 0
                && is_blank_at(text, text[..i].char_indices().next_back().map(|(at, _)| at))
                && (word_at(text, i, "as") || word_at(text, i, "let")) =>
            {
                return i;
            }
            _ => {}
        }
        i += text[i..].chars().next().map_or(1, char::len_utf8);
    }
    bytes.len()
}

/// `text` split at each `;` outside brackets and strings, with each part's
/// offset (upstream `topLevelParts`).
fn top_level_parts(text: &str) -> Vec<(usize, &str)> {
    let bytes = text.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth: i64 = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' | b'`' => {
                let Some(end) = skip_string(text, i) else {
                    break;
                };
                i = end + 1;
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b';' if depth == 0 => {
                parts.push((start, &text[start..i]));
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push((start, &text[start..]));
    parts
}

/// A structural directive's microsyntax, read into the expressions it
/// evaluates, with their offsets, and the locals it declares: `let item of
/// items; let i = index; trackBy: trackById` evaluates `items` and
/// `trackById` and declares `item` and `i`. A key (`of`, `trackBy:`,
/// `else`) and the context name a binding takes (`index`, `index as i`)
/// belong to the directive, never the component (upstream
/// `readMicrosyntax`).
pub(super) fn read_microsyntax(text: &str) -> (Vec<(usize, String)>, Vec<String>) {
    let bytes = text.as_bytes();
    let mut expressions = Vec::new();
    let mut locals = Vec::new();
    let mut i = 0;
    let blank = |i: usize| text[i..].chars().next().is_some_and(is_js_space);
    let skip = |i: &mut usize, separators: bool| {
        while *i < bytes.len() && (blank(*i) || (separators && matches!(bytes[*i], b';' | b','))) {
            *i += text[*i..].chars().next().map_or(1, char::len_utf8);
        }
    };
    let word = |i: &mut usize| -> Option<String> {
        let found = NAME_AT.find(&text[*i..])?;
        *i += found.end();
        Some(found.as_str().to_string())
    };
    let mut first = true;
    while i < bytes.len() {
        skip(&mut i, true);
        if i >= bytes.len() {
            break;
        }
        let start = i;
        let key = word(&mut i);
        if key.as_deref() == Some("let") && i < bytes.len() && blank(i) {
            // `let item`, `let i = index`: a local, and the context name it
            // takes.
            skip(&mut i, false);
            if let Some(local) = word(&mut i) {
                locals.push(local);
            }
            skip(&mut i, false);
            if bytes.get(i) == Some(&b'=') {
                i += 1;
                skip(&mut i, false);
                word(&mut i);
            }
            first = false;
            continue;
        }
        if first || key.is_none() {
            // The directive's own expression (`*ngIf="cond"`) has no key.
            i = start;
        } else {
            skip(&mut i, false);
            if bytes.get(i) == Some(&b':') {
                i += 1;
            }
            skip(&mut i, false);
        }
        let end = expression_end(text, i);
        if end > i {
            expressions.push((i, text[i..end].to_string()));
        }
        i = end;
        skip(&mut i, false);
        if word_at(text, i, "as") {
            i += 2;
            skip(&mut i, false);
            if let Some(alias) = word(&mut i) {
                locals.push(alias);
            }
        }
        // Nothing read (a stray character): step past it.
        if i == start {
            i += text[i..].chars().next().map_or(1, char::len_utf8);
        }
        first = false;
    }
    (expressions, locals)
}

/// The `;` that ends a `@let` declaration's expression (upstream
/// `statementEnd`).
fn let_end(text: &str, from: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth: i64 = 0;
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' | b'`' => {
                i = skip_string(text, i)? + 1;
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            b';' if depth == 0 => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// Whether a declared name's match passes `(?<=\s)` before it and
/// `(?=[\s=/>])` after it.
fn declared_name_ok(text: &str, found: &regex::Captures<'_>) -> bool {
    let whole = found.get(0).expect("declared name");
    let before = text[..whole.start()].chars().next_back();
    before.is_some_and(is_js_space)
        && text[whole.end()..]
            .chars()
            .next()
            .is_some_and(|c| is_js_space(c) || matches!(c, '=' | '/' | '>'))
}

/// Every name a template's bindings call or read at render, property
/// bindings, interpolations, structural directives, control flow and `@let`,
/// in template order, minus what the template names itself: a pipe, an
/// object key, a microsyntax key, and any local it declares (`let item`, `as
/// user`, `#input`, `@let total`). Event bindings are not here: they run when
/// the user acts, and are read on their own. A commented-out binding reads
/// nothing (upstream `templateMemberUses`).
pub(super) fn template_member_uses(template: &str) -> Vec<MemberUse> {
    let text = blank_matches(template, &HTML_COMMENT);
    let text = text.as_str();
    let mut expressions: Vec<Expression> = Vec::new();
    let mut locals: Vec<Local> = Vec::new();
    let mut scope: i64 = 0;
    let value_at = |whole_end: usize, value: &str| whole_end - 1 - value.len();
    let tag_start = |at: usize| text[..at].rfind('<').unwrap_or(0);
    let quoted = |found: &regex::Captures<'_>| {
        found
            .get(2)
            .or_else(|| found.get(3))
            .map_or_else(String::new, |value| value.as_str().to_string())
    };

    for found in PROPERTY_BINDING.captures_iter(text) {
        let value = quoted(&found);
        scope += 1;
        expressions.push(Expression {
            via: format!("[{}]", &found[1]),
            at: value_at(found.get(0).expect("binding").end(), &value),
            text: value,
            scope,
            sees_own_locals: false,
        });
    }
    for found in matches_where(&STRUCTURAL_DIRECTIVE, text, |found| {
        after_ok(text, found.get(0).expect("directive").start(), |b| {
            is_name_byte(b) || b == b'*' || b == b'-'
        })
    }) {
        let whole = found.get(0).expect("directive");
        let value = quoted(&found);
        let at = value_at(whole.end(), &value);
        scope += 1;
        let (read, declared) = read_microsyntax(&value);
        for (offset, expression) in read {
            expressions.push(Expression {
                via: format!("*{}", &found[1]),
                at: at + offset,
                text: expression,
                scope,
                sees_own_locals: false,
            });
        }
        // The host element and everything inside it see the directive's
        // locals.
        for name in declared {
            locals.push(Local {
                name,
                from: tag_start(whole.start()),
                scope,
            });
        }
    }
    for found in INTERPOLATION.captures_iter(text) {
        scope += 1;
        expressions.push(Expression {
            via: "{{ }}".to_string(),
            at: found.get(0).expect("interpolation").start() + 2,
            text: found[1].to_string(),
            scope,
            sees_own_locals: false,
        });
    }
    for found in matches_where(&CONTROL_FLOW, text, |found| {
        after_ok(text, found.get(0).expect("block").start(), |b| {
            is_name_byte(b) || b == b'@'
        })
    }) {
        let whole = found.get(0).expect("block");
        let open = whole.end() - 1;
        let Some(close) = match_bracket(text, open) else {
            continue;
        };
        let kind = found[1]
            .split(is_js_space)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let via = format!("@{kind}");
        scope += 1;
        let parts: Vec<(usize, &str)> = top_level_parts(&text[open + 1..close])
            .into_iter()
            .map(|(offset, part)| (open + 1 + offset, part))
            .collect();
        let expression = |at: usize, text: &str, sees_own_locals: bool| Expression {
            via: via.clone(),
            at,
            text: text.to_string(),
            scope,
            sees_own_locals,
        };
        match kind.as_str() {
            "switch" | "case" => {
                for (at, part) in &parts {
                    expressions.push(expression(*at, part, false));
                }
            }
            // `when cond` is an expression; `on viewport`, `on timer(5s)`
            // are triggers.
            "defer" => {
                for (at, part) in &parts {
                    if let Some(when) = DEFER_WHEN.find(part)
                        && !part
                            .as_bytes()
                            .get(when.end())
                            .is_some_and(|&b| is_name_byte(b))
                    {
                        expressions.push(expression(at + when.end(), &part[when.end()..], false));
                    }
                }
            }
            // `item of items(); track item.id; let i = $index, odd = $odd`
            "for" => {
                let mut parts = parts.iter();
                if let Some((head_at, head)) = parts.next()
                    && let Some(head_loop) = FOR_HEAD.captures(head)
                    && !head
                        .as_bytes()
                        .get(head_loop.get(0).expect("head").end())
                        .is_some_and(|&b| is_name_byte(b))
                {
                    let end = head_loop.get(0).expect("head").end();
                    locals.push(Local {
                        name: head_loop[2].to_string(),
                        from: whole.start(),
                        scope,
                    });
                    expressions.push(expression(head_at + end, &head[end..], false));
                }
                for (at, part) in parts {
                    if let Some(track) = FOR_TRACK.find(part)
                        && !part
                            .as_bytes()
                            .get(track.end())
                            .is_some_and(|&b| is_name_byte(b))
                    {
                        expressions.push(expression(at + track.end(), &part[track.end()..], true));
                        continue;
                    }
                    if let Some(let_word) = FOR_LET.find(part)
                        && !part
                            .as_bytes()
                            .get(let_word.end())
                            .is_some_and(|&b| is_name_byte(b))
                    {
                        for alias in FOR_ALIAS.captures_iter(part) {
                            locals.push(Local {
                                name: alias[1].to_string(),
                                from: whole.start(),
                                scope,
                            });
                        }
                    }
                }
            }
            // `@if (user(); as user)`: the condition, then the alias its
            // body sees.
            _ => {
                let mut parts = parts.iter();
                if let Some((at, head)) = parts.next() {
                    expressions.push(expression(*at, head, false));
                }
                for (_, part) in parts {
                    if let Some(alias) = IF_ALIAS.captures(part) {
                        locals.push(Local {
                            name: alias[1].to_string(),
                            from: whole.start(),
                            scope,
                        });
                    }
                }
            }
        }
    }
    for found in matches_where(&LET_DECLARATION, text, |found| {
        after_ok(text, found.get(0).expect("let").start(), |b| {
            is_name_byte(b) || b == b'@'
        })
    }) {
        let whole = found.get(0).expect("let");
        let at = whole.end();
        let Some(end) = let_end(text, at) else {
            continue;
        };
        scope += 1;
        expressions.push(Expression {
            via: "@let".to_string(),
            at,
            text: text[at..end].to_string(),
            scope,
            sees_own_locals: false,
        });
        locals.push(Local {
            name: found[1].to_string(),
            from: whole.start(),
            scope,
        });
    }
    for found in matches_where(&REFERENCE_VARIABLE, text, |found| {
        declared_name_ok(text, found)
    }) {
        locals.push(Local {
            name: found[1].to_string(),
            from: 0,
            scope: -1,
        });
    }
    for found in matches_where(&TEMPLATE_INPUT, text, |found| declared_name_ok(text, found)) {
        locals.push(Local {
            name: found[1].to_string(),
            from: tag_start(found.get(0).expect("input").start()),
            scope: -1,
        });
    }

    let mut uses = Vec::new();
    for expression in &expressions {
        for (name, offset, call) in expression_uses(&expression.text) {
            let at = expression.at + offset;
            let shadowed = locals.iter().any(|local| {
                local.name == name
                    && local.from <= at
                    && (local.scope != expression.scope || expression.sees_own_locals)
            });
            if !shadowed {
                uses.push(MemberUse {
                    name,
                    at,
                    call,
                    via: expression.via.clone(),
                });
            }
        }
    }
    uses.sort_by_key(|member_use| member_use.at);
    uses
}

/// What a method-kind member's own declaration says it is: an accessor
/// (`get canSave() {`), a function (`displayFn(p) {`, `onPick = (p) => …`),
/// or a field holding what a call returned (`days = Array.from({ length: 31
/// }, (_, i) => i + 1)`, `total = computed(() => …)`): a method only because
/// a callback is written in its initializer, and as likely plain data
/// (upstream `MemberShape`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Get,
    Set,
    Function,
    Value,
}

/// The byte offset of UTF-16 offset `units` into `text`, at most its end.
fn byte_of_utf16(text: &str, units: i64) -> usize {
    let mut count = 0i64;
    for (offset, c) in text.char_indices() {
        if count >= units {
            return offset;
        }
        count += c.len_utf16() as i64;
    }
    text.len()
}

/// A member's shape, read from its declaration's five lines from its column
/// (upstream `memberShape`).
fn member_shape(lines: &[&str], member: &Node) -> Shape {
    let first = usize::try_from(member.start_line - 1).unwrap_or(0);
    let joined = lines
        .iter()
        .skip(first)
        .take(5)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let head = &joined[byte_of_utf16(&joined, member.start_column)..];
    let name = member.name.as_str();
    let is_name_or_quote = |b: u8| is_name_byte(b) || matches!(b, b'\'' | b'"' | b'`');
    let mut from = 0;
    loop {
        let Some(found) = head.get(from..).and_then(|rest| rest.find(name)) else {
            return Shape::Function;
        };
        let at = from + found;
        from = at + name.len();
        if name.is_empty() {
            return Shape::Function;
        }
        // The name itself: not a longer name, and not a decorator's
        // `'value'` argument.
        if (at > 0 && is_name_or_quote(head.as_bytes()[at - 1]))
            || head
                .as_bytes()
                .get(from)
                .is_some_and(|&b| is_name_or_quote(b))
        {
            continue;
        }
        if let Some(accessor) = accessor_before(&head[..at]) {
            return accessor;
        }
        // `name = …`, `name: Type = …` (a function type's `=>` is not the
        // `=`).
        let Some(field) = FIELD_ASSIGN.captures(&head[from..]) else {
            return Shape::Function;
        };
        let assigned = from + field.get(1).expect("assignment").end();
        let value = head[assigned..].trim_start_matches(is_js_space);
        let value = strip_async(value);
        if FUNCTION_VALUE.is_match(value) {
            return Shape::Function;
        }
        let close = if value.starts_with('(') {
            match_bracket(value, 0)
        } else {
            None
        };
        return if close
            .is_some_and(|close| close > 0 && ARROW_AFTER_PARAMS.is_match(&value[close + 1..]))
        {
            Shape::Function
        } else {
            Shape::Value
        };
    }
}

/// `get ` or `set ` just before a name, not ending a longer word
/// (`(?<![\w$])(get|set)\s+$`).
fn accessor_before(before: &str) -> Option<Shape> {
    let trimmed = before.trim_end_matches(is_js_space);
    if trimmed.len() == before.len() {
        return None;
    }
    let (shape, word) = if trimmed.ends_with("get") {
        (Shape::Get, "get")
    } else if trimmed.ends_with("set") {
        (Shape::Set, "set")
    } else {
        return None;
    };
    let start = trimmed.len() - word.len();
    (start == 0 || !is_name_byte(trimmed.as_bytes()[start - 1])).then_some(shape)
}

/// `value` without a leading `async` and the blanks after it
/// (`replace(/^async\s+/, '')`).
fn strip_async(value: &str) -> &str {
    if let Some(rest) = value.strip_prefix("async") {
        let trimmed = rest.trim_start_matches(is_js_space);
        if trimmed.len() < rest.len() {
            return trimmed;
        }
    }
    value
}

/// The member one use links, how, and whether as a function reference
/// (upstream `bindingTarget`). A call links what it calls: a method, or a
/// signal held in a field (`count = signal(0)` is a property). A name read
/// without a call links only a method-kind member: a getter runs when it is
/// read, so it is called; a function handed over uncalled
/// (`[displayWith]="displayFn"`) is a function reference; a field a call
/// filled (`@for (day of days)`) is a plain reference. Same-named methods
/// resolve as the event bindings' do: the last declared (an overload's
/// body).
fn binding_target<'n>(
    candidates: Option<&Vec<&'n Node>>,
    call: bool,
    shape: &mut impl FnMut(&Node) -> Shape,
) -> Option<(&'n Node, EdgeKind, bool)> {
    let candidates = candidates?;
    let methods: Vec<&'n Node> = candidates
        .iter()
        .copied()
        .filter(|node| node.kind == NodeKind::Method)
        .collect();
    if call {
        let target = methods
            .iter()
            .rev()
            .copied()
            .find(|node| matches!(shape(node), Shape::Function | Shape::Value))
            .or_else(|| methods.first().copied())
            .or_else(|| {
                candidates
                    .iter()
                    .copied()
                    .find(|node| node.kind == NodeKind::Property)
            })?;
        return Some((target, EdgeKind::Calls, false));
    }
    if let Some(getter) = methods
        .iter()
        .copied()
        .find(|node| shape(node) == Shape::Get)
    {
        return Some((getter, EdgeKind::Calls, false));
    }
    let read = methods
        .iter()
        .rev()
        .copied()
        .find(|node| shape(node) != Shape::Set)?;
    let fn_ref = shape(read) == Shape::Function;
    Some((read, EdgeKind::References, fn_ref))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::UNIX_EPOCH;

    use codegraph_core::node_id::hash_content;
    use codegraph_core::types::FileRecord;
    use codegraph_extract::ExtractOptions;
    use codegraph_store::Store;

    use super::*;

    /// A project of hand-written TypeScript files whose index holds exactly
    /// the nodes given, a `package.json` declaring `@angular/core` on disk.
    struct Fixture {
        dir: PathBuf,
        files: Vec<(String, String, Language)>,
        disk: Vec<(String, String)>,
        nodes: Vec<Node>,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "cg-angular-{name}-{}-{}",
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
                disk: vec![(
                    "package.json".to_string(),
                    "{\"dependencies\":{\"@angular/core\":\"^19.0.0\"}}".to_string(),
                )],
                nodes: Vec::new(),
            }
        }

        /// The indexed file `path` holding `text`.
        fn file(mut self, path: &str, text: &str) -> Self {
            let language = if path.ends_with(".ts") || path.ends_with(".mts") {
                Language::TypeScript
            } else {
                Language::JavaScript
            };
            self.files
                .push((path.to_string(), text.to_string(), language));
            self
        }

        /// A file on disk, its text different from what the index recorded
        /// when `path` is indexed too.
        fn disk(mut self, path: &str, text: &str) -> Self {
            self.disk.push((path.to_string(), text.to_string()));
            self
        }

        /// The class `name` of `file` at `line`, and its methods at the
        /// lines after it.
        fn class(mut self, file: &str, name: &str, line: i64, methods: &[&str]) -> Self {
            let mut class = node(NodeKind::Class, name, file, line);
            class.qualified_name = format!("{file}::{name}");
            class.end_line = line + methods.len() as i64 + 1;
            self.nodes.push(class);
            for (at, method) in methods.iter().enumerate() {
                let mut node = node(NodeKind::Method, method, file, line + 1 + at as i64);
                node.qualified_name = format!("{file}::{name}::{method}");
                node.start_column = 2;
                self.nodes.push(node);
            }
            self
        }

        fn store(&self) -> Store {
            let mut store = Store::open(&self.dir.join("index.db")).unwrap();
            store.upsert_nodes(&self.nodes).unwrap();
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
            for (path, text) in &self.disk {
                let full = self.dir.join(path);
                std::fs::create_dir_all(full.parent().unwrap()).unwrap();
                std::fs::write(&full, text).unwrap();
            }
            store
        }

        /// `by source -> target via line` for each Angular template edge in
        /// `edges`, sorted.
        fn labels(&self, edges: Vec<Edge>) -> Vec<String> {
            let name = |id: &str| {
                self.nodes
                    .iter()
                    .find(|node| node.id == id)
                    .map_or("?".to_string(), |node| node.name.clone())
            };
            let mut out: Vec<String> = edges
                .into_iter()
                .filter_map(|edge| {
                    let metadata = edge.metadata.clone()?;
                    let by = metadata["synthesizedBy"].as_str()?.to_string();
                    by.starts_with("angular-").then(|| {
                        format!(
                            "{by} {} -> {} {} line {}",
                            name(&edge.source),
                            name(&edge.target),
                            metadata["via"].as_str().unwrap_or("?"),
                            edge.line.unwrap()
                        )
                    })
                })
                .collect();
            out.sort();
            out
        }

        /// The pass's edges over the fixture, as [`Self::labels`].
        fn edges(&self) -> Vec<String> {
            let store = self.store();
            self.labels(crate::synthesis::synthesize(&store, &self.dir, 1 << 20).unwrap())
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn node(kind: NodeKind, name: &str, file: &str, line: i64) -> Node {
        Node {
            id: format!("{}:{file}:{name}:{line}", kind.as_str()),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: file.to_string(),
            language: Language::TypeScript,
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

    /// A one-line component `name` with `selector` and an inline
    /// `template`, its class on line 2 and `methods` after it.
    fn component(selector: &str, template: &str, name: &str, methods: &[&str]) -> String {
        let mut text = format!(
            "@Component({{ selector: '{selector}', template: `{template}` }})\nexport class {name} {{\n"
        );
        for method in methods {
            text.push_str(&format!("  {method}() {{}}\n"));
        }
        text.push_str("}\n");
        text
    }

    /// A tag renders the one component its element selector names, of the
    /// parent's own app when several do, nothing when two of that app do;
    /// a component's own tag, an attribute selector and a selector nothing
    /// answers render nothing.
    #[test]
    fn a_tag_renders_the_component_its_selector_names_in_its_own_app_first() {
        let parent = component(
            "app-parent",
            "<app-card></app-card><app-solo/><app-twice></app-twice><app-parent></app-parent><app-none></app-none><div appCard></div>",
            "ParentComponent",
            &[],
        );
        let fixture = Fixture::new("children")
            .file("apps/a/src/app/parent.component.ts", &parent)
            .class(
                "apps/a/src/app/parent.component.ts",
                "ParentComponent",
                2,
                &[],
            )
            .file(
                "apps/a/src/app/card.component.ts",
                &component("app-card, [appCard]", "", "CardA", &[]),
            )
            .class("apps/a/src/app/card.component.ts", "CardA", 2, &[])
            .file(
                "apps/b/src/app/card.component.ts",
                &component("app-card", "", "CardB", &[]),
            )
            .class("apps/b/src/app/card.component.ts", "CardB", 2, &[])
            .file(
                "libs/ui/solo.component.ts",
                &component("app-solo", "", "Solo", &[]),
            )
            .class("libs/ui/solo.component.ts", "Solo", 2, &[])
            .file(
                "apps/a/src/app/one.component.ts",
                &component("app-twice", "", "TwiceOne", &[]),
            )
            .class("apps/a/src/app/one.component.ts", "TwiceOne", 2, &[])
            .file(
                "apps/a/src/app/two.component.ts",
                &component("app-twice", "", "TwiceTwo", &[]),
            )
            .class("apps/a/src/app/two.component.ts", "TwiceTwo", 2, &[]);
        assert_eq!(
            fixture.edges(),
            [
                "angular-template ParentComponent -> CardA app-card line 1",
                "angular-template ParentComponent -> Solo app-solo line 1",
            ]
        );
    }

    /// A template draws at most forty children and forty event handlers; a
    /// repeated pair is skipped before it counts.
    #[test]
    fn a_template_draws_forty_children_and_forty_handlers() {
        let mut template = String::from("<app-c0></app-c0>");
        let mut methods = Vec::new();
        for i in 0..42 {
            template.push_str(&format!("<app-c{i}></app-c{i}>"));
        }
        template.push_str("<b (click)=\"m0()\"></b>");
        for i in 0..42 {
            template.push_str(&format!("<b (click)=\"m{i}()\"></b>"));
            methods.push(format!("m{i}"));
        }
        let method_names: Vec<&str> = methods.iter().map(String::as_str).collect();
        let mut fixture = Fixture::new("caps")
            .file(
                "src/app/parent.component.ts",
                &component("app-parent", &template, "Parent", &method_names),
            )
            .class("src/app/parent.component.ts", "Parent", 2, &method_names);
        for i in 0..42 {
            let file = format!("src/app/c{i}.component.ts");
            let name = format!("C{i}");
            fixture = fixture
                .file(&file, &component(&format!("app-c{i}"), "", &name, &[]))
                .class(&file, &name, 2, &[]);
        }
        let edges = fixture.edges();
        let count = |by: &str| edges.iter().filter(|edge| edge.starts_with(by)).count();
        assert_eq!(count("angular-template "), 40, "{edges:#?}");
        assert_eq!(count("angular-event "), 40, "{edges:#?}");
        for i in 0..40 {
            assert!(
                edges.contains(&format!("angular-template Parent -> C{i} app-c{i} line 1")),
                "C{i}"
            );
            assert!(
                edges.contains(&format!("angular-event Parent -> m{i} (click) line 1")),
                "m{i}"
            );
        }
    }

    /// A test file's components, a declaration file's and a JavaScript
    /// file's are not read; an inline `template:` that is no literal is no
    /// template (KEEP-RUST).
    #[test]
    fn only_literal_templates_of_typescript_sources_are_read() {
        let bound = "<b (click)=\"go()\"></b>";
        let fixture = Fixture::new("sources")
            .file("src/app/x.component.spec.ts", &component("app-x", bound, "SpecComponent", &["go"]))
            .class("src/app/x.component.spec.ts", "SpecComponent", 2, &["go"])
            .file("src/app/x.d.ts", &component("app-y", bound, "DeclaredComponent", &["go"]))
            .class("src/app/x.d.ts", "DeclaredComponent", 2, &["go"])
            .file("src/app/x.component.js", &component("app-z", bound, "ScriptComponent", &["go"]))
            .class("src/app/x.component.js", "ScriptComponent", 2, &["go"])
            .file(
                "src/app/bound.component.ts",
                "const markup = `<b (click)=\"go()\"></b>`;\n@Component({ selector: 'app-w', template: markup })\nexport class BoundComponent {\n  go() {}\n}\n",
            )
            .class("src/app/bound.component.ts", "BoundComponent", 3, &["go"])
            .file("src/app/ok.component.mts", &component("app-ok", bound, "OkComponent", &["go"]))
            .class("src/app/ok.component.mts", "OkComponent", 2, &["go"])
            // Upstream reads `ag + '<b (click)="go()"></b>' + ` here, from
            // the value's first character to its next one.
            .file(
                "src/app/joined.component.ts",
                "@Component({ selector: 'app-v', template: tag + '<b (click)=\"go()\"></b>' + tag })\nexport class JoinedComponent {\n  go() {}\n}\n",
            )
            .class("src/app/joined.component.ts", "JoinedComponent", 2, &["go"]);
        assert_eq!(
            fixture.edges(),
            ["angular-event OkComponent -> go (click) line 1"]
        );
    }

    /// A `(` right after a `[` opens no event binding (upstream's
    /// `(?<!\[)`): `[(go)="go()"`, a two-way binding without its `]`, runs
    /// nothing, as `[(ngModel)]` does, whose `]` the pattern never crosses.
    #[test]
    fn half_a_two_way_binding_is_no_event() {
        let fixture = Fixture::new("two-way")
            .file(
                "src/app/x.component.ts",
                &component(
                    "app-x",
                    "<input [(go)=\"go()\"><input [(ngModel)]=\"go()\">",
                    "XComponent",
                    &["go"],
                ),
            )
            .class("src/app/x.component.ts", "XComponent", 2, &["go"]);
        assert!(
            fixture
                .edges()
                .iter()
                .all(|edge| !edge.starts_with("angular-event ")),
            "{:?}",
            fixture.edges()
        );
    }

    /// A project that does not depend on `@angular/core` has no component.
    #[test]
    fn a_project_without_angular_draws_nothing() {
        let fixture = Fixture::new("no-angular")
            .disk(
                "package.json",
                "{\"dependencies\":{\"@angular/common\":\"^19.0.0\"}}",
            )
            .file(
                "src/app/x.component.ts",
                &component("app-x", "<b (click)=\"go()\"></b>", "XComponent", &["go"]),
            )
            .class("src/app/x.component.ts", "XComponent", 2, &["go"]);
        assert!(fixture.edges().is_empty());
    }

    /// An event runs the last method its class declares under the name it
    /// calls, its implementation after its overloads (upstream's `Map`
    /// built in line order).
    #[test]
    fn an_event_runs_the_last_method_of_its_name() {
        let source = "@Component({ selector: 'app-x', template: `<b (click)=\"go()\"></b>` })\nexport class XComponent {\n  go(): void;\n  go(n?: number) {}\n}\n";
        let fixture = Fixture::new("overloads")
            .file("src/app/x.component.ts", source)
            .class("src/app/x.component.ts", "XComponent", 2, &["go", "go"]);
        let store = fixture.store();
        let edges = crate::synthesis::synthesize(&store, &fixture.dir, 1 << 20).unwrap();
        let targets: Vec<&str> = edges
            .iter()
            .filter(|edge| {
                edge.metadata
                    .as_ref()
                    .is_some_and(|m| m["synthesizedBy"] == "angular-event")
            })
            .map(|edge| edge.target.as_str())
            .collect();
        assert_eq!(targets, ["method:src/app/x.component.ts:go:4"]);
    }

    /// A template the index holds that changed since it was indexed reads
    /// as absent, and is not read beside the index nor recorded.
    #[test]
    fn a_drifted_indexed_template_is_read_as_absent() {
        let source = "@Component({ selector: 'app-x', templateUrl: './x.component.html' })\nexport class XComponent {\n  go() {}\n}\n";
        let fixture = Fixture::new("drift")
            .file("src/app/x.component.ts", source)
            .class("src/app/x.component.ts", "XComponent", 2, &["go"])
            .file("src/app/x.component.html", "<b (click)=\"go()\"></b>\n")
            .disk(
                "src/app/x.component.html",
                "<b (click)=\"go()\"></b><i></i>\n",
            );
        let mut store = fixture.store();
        crate::synthesis::run_stage(&mut store, &fixture.dir, &ExtractOptions::default()).unwrap();
        let edges = store.edges_by_kind(EdgeKind::Calls).unwrap();
        assert!(fixture.labels(edges).is_empty());
        assert_eq!(
            crate::synthesis::recorded_inputs(&store).unwrap(),
            Some(crate::synthesis::Inputs::new())
        );
        // Unchanged, it is read as the index holds it.
        let fresh = Fixture::new("fresh")
            .file("src/app/x.component.ts", source)
            .class("src/app/x.component.ts", "XComponent", 2, &["go"])
            .file("src/app/x.component.html", "<b (click)=\"go()\"></b>\n");
        let mut store = fresh.store();
        crate::synthesis::run_stage(&mut store, &fresh.dir, &ExtractOptions::default()).unwrap();
        let edges = store.edges_by_kind(EdgeKind::Calls).unwrap();
        assert_eq!(
            fresh.labels(edges),
            ["angular-event XComponent -> go (click) line 2"]
        );
        assert_eq!(
            crate::synthesis::recorded_inputs(&store).unwrap(),
            Some(crate::synthesis::Inputs::new())
        );
    }

    fn uses(template: &str) -> Vec<String> {
        template_member_uses(template)
            .into_iter()
            .map(|u| format!("{}{} {}", u.name, if u.call { "()" } else { "" }, u.via))
            .collect()
    }

    /// Upstream `angular-router.test.ts` "reads property bindings,
    /// interpolations, structural directives, control flow and @let".
    #[test]
    fn a_template_reads_its_bindings_interpolations_directives_blocks_and_lets() {
        assert_eq!(
            uses(
                r#"<button (click)="cycle()" [attr.aria-label]="label()"><i [name]="icon()"></i></button>"#
            ),
            ["label() [attr.aria-label]", "icon() [name]"]
        );
        assert_eq!(
            uses(
                r#"<p *ngIf="isOpen(); else closed">{{ count() }} {{ total }}</p><ng-template #closed></ng-template>"#
            ),
            ["isOpen() *ngIf", "count() {{ }}", "total {{ }}"]
        );
        assert_eq!(
            uses(
                "@if (loading()) { x } @else if (failed()) { y } @switch (mode()) { @case (Mode.A) { a } }"
            ),
            [
                "loading() @if",
                "failed() @else if",
                "mode() @switch",
                "Mode @case"
            ]
        );
        // `on timer(5s)` is a trigger, not a call.
        assert_eq!(
            uses("@defer (on viewport; when ready(); prefetch on timer(5s)) { x }"),
            ["ready() @defer"]
        );
        assert_eq!(
            uses(r#"@let total = price() * quantity; <input [(ngModel)]="title">"#),
            ["price() @let", "quantity @let", "title [(ngModel)]"]
        );
    }

    /// Upstream "leaves out a pipe, an object key, another object's member,
    /// a string, a comment and an event binding".
    #[test]
    fn a_template_leaves_out_pipes_keys_members_strings_comments_and_events() {
        assert_eq!(
            uses(
                r#"{{ total | currency: code() }} <i [ngClass]="{ active: isActive() }" [x]="form.reset() || this.own()"></i>"#
            ),
            [
                "total {{ }}",
                "code() {{ }}",
                "isActive() [ngClass]",
                "form [x]",
                "own() [x]"
            ]
        );
        assert!(
            uses(r#"<!-- {{ legacy() }} <b [x]="old()"></b> --> {{ 'quoted()' }} <b (click)="save()"></b>"#)
                .is_empty()
        );
        // A template literal's text is a string; its holes are expressions.
        assert_eq!(
            uses(
                "<td [class.today]=\"isToday(`${key}-${formatDay(day)}`)\" [title]=\"`quoted()`\"></td>"
            ),
            [
                "isToday() [class.today]",
                "key [class.today]",
                "formatDay() [class.today]",
                "day [class.today]"
            ]
        );
    }

    /// Upstream "leaves out the names the template declares, where they are
    /// visible".
    #[test]
    fn a_template_leaves_out_the_names_it_declares_where_they_are_visible() {
        // A microsyntax's locals, keys and context names belong to the
        // directive.
        assert_eq!(
            uses(
                r#"<li *ngFor="let item of items(); let i = index; trackBy: trackById" [title]="item.name">{{ i }}</li>"#
            ),
            ["items() *ngFor", "trackById *ngFor"]
        );
        // An alias belongs to the body, not to the condition that names it.
        assert_eq!(
            uses("@if (user(); as user) { {{ user.name }} }"),
            ["user() @if"]
        );
        assert_eq!(
            uses(r#"<div *ngIf="user() as user">{{ user.name }}</div>"#),
            ["user() *ngIf"]
        );
        // `track` sees the loop's own names.
        assert_eq!(
            uses(
                "@for (item of items(); track item.id; let odd = $odd) { {{ item.name }} {{ odd }} }"
            ),
            ["items() @for"]
        );
        assert_eq!(uses("@let total = price(); {{ total }}"), ["price() @let"]);
        // A reference variable is seen across the template, even before it
        // is declared.
        assert!(uses(r#"<b [x]="input.value"></b><input #input>"#).is_empty());
        assert!(
            uses(r#"<ng-template let-row let-i="index">{{ row.name }} {{ i }}</ng-template>"#)
                .is_empty()
        );
    }

    /// Upstream "splits a microsyntax into its expressions and its locals".
    #[test]
    fn a_microsyntax_splits_into_its_expressions_and_its_locals() {
        assert_eq!(
            read_microsyntax("let item of items | async; index as i; trackBy: trackFn"),
            (
                vec![
                    (12, "items | async".to_string()),
                    (48, "trackFn".to_string())
                ],
                vec!["item".to_string(), "i".to_string()]
            )
        );
        assert_eq!(
            read_microsyntax("user$ | async as user; else loading"),
            (
                vec![
                    (0, "user$ | async ".to_string()),
                    (28, "loading".to_string())
                ],
                vec!["user".to_string()]
            )
        );
    }
}
