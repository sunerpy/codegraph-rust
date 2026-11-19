//! Navigation written as markup, and destinations a helper returns (W7-18a):
//! upstream's `expo-router-synthesizer.ts`, `next-router-synthesizer.ts`,
//! `react-router-synthesizer.ts` (at `b635dd46`), `tanstack-router-synthesizer.ts`,
//! `vue-router-synthesizer.ts` and the link half of `sveltekit-synthesizer.ts`.
//!
//! A JSX or template attribute is not a call, so extraction records no
//! reference for `<Link href="/users">`, and the router resolvers, which
//! bind `router.push('/users')`, never see it. Each link pass reads the
//! markup of the files its router's routes serve, attributes a link to the
//! function or component it is written in ([`enclosing_fn`]), matches its
//! destination against the router's table and draws a `navigates` edge from
//! the component to the route, at most [`MAX_LINKS_PER_COMPONENT`] a
//! component. The Expo pass reads `router.push(await helper())`: the screens
//! written in the helper's body are where the navigation goes, an edge from
//! the helper to each.
//!
//! The tables are the resolvers' own, read through [`View::context`]; a
//! computed destination, a relative one, an external one and one no route
//! serves draw nothing, and neither does a path two routes of the link's app
//! serve. Files are read in path order, and a component's links in its own
//! file's order, so the cap keeps the same edges whatever order the files
//! were indexed in.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use anyhow::Result;
use codegraph_core::file_class::is_test_path;
use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde_json::{Value, json};

use super::context::ViewContext;
use super::dispatch::synthesized;
use super::stage::{LineAt, View, enclosing_fn};
use crate::frameworks::expo_router::{expo_screens, normalize_href_path};
use crate::frameworks::js_scan::{JS_SPACE, js_trim, match_bracket};
use crate::frameworks::nextjs::next_link_routes;
use crate::frameworks::react_router::{config_href_expression, react_router_link_routes};
use crate::frameworks::route_href::{
    HOLE, HrefLiteral, LinkRoutes, parse_href_expression, read_string_at, string_end, to_href,
};
use crate::frameworks::sveltekit_router::sveltekit_link_routes;
use crate::frameworks::tanstack_router::{tanstack_destination, tanstack_link_routes};
use crate::frameworks::vue_router::{
    parse_vue_path_object, route_name_in_expression, vue_link_routes,
};
use crate::import_resolver::resolve_import_path;
use crate::strip_comments::strip_js_comments_keeping_offsets;
use crate::types::ResolutionContext;

/// Links a single component may carry before it is a navigation menu, not a
/// decision (upstream `MAX_LINKS_PER_COMPONENT`).
const MAX_LINKS_PER_COMPONENT: usize = 24;

/// A helper yielding more distinct screens than this is a table, not a
/// decision (upstream `MAX_SCREENS_PER_HELPER`).
const MAX_SCREENS_PER_HELPER: usize = 8;

/// `pattern` with JavaScript's `\s`; its `\b` is written `(?-u:\b)`, ASCII as
/// JavaScript's is.
fn js(pattern: &str) -> String {
    pattern.replace(r"\s", JS_SPACE)
}

fn compile(pattern: &str) -> Regex {
    Regex::new(&js(pattern)).expect("a link pattern compiles")
}

/// A file JSX can be written in (upstream `JSX_FILE`).
static JSX_FILE: LazyLock<Regex> = LazyLock::new(|| compile(r"\.(?:[cm]?[jt]sx?|mdx)$"));
/// A JavaScript or TypeScript file, no MDX (TanStack's `JSX_FILE` and
/// Expo's `JS_FILE`).
static JS_FILE: LazyLock<Regex> = LazyLock::new(|| compile(r"\.(?:[cm]?[jt]sx?)$"));
/// A file a Vue template or JSX can be written in (upstream
/// `TEMPLATE_FILE`).
static TEMPLATE_FILE: LazyLock<Regex> = LazyLock::new(|| compile(r"\.(?:vue|[cm]?[jt]sx?)$"));

/// The edges of one link pass, each component's counted (upstream's `seen`
/// and `perComponent`): a pair already drawn is skipped and not counted;
/// every other pair counts, and from the 25th on is dropped.
#[derive(Default)]
struct Links {
    edges: Vec<Edge>,
    seen: HashSet<(String, String)>,
    per_component: HashMap<String, usize>,
}

impl Links {
    fn add(&mut self, component: &Node, route: &Node, line: i64, metadata: Value) {
        let key = (component.id.clone(), route.id.clone());
        if self.seen.contains(&key) {
            return;
        }
        let count = self.per_component.entry(component.id.clone()).or_default();
        *count += 1;
        if *count > MAX_LINKS_PER_COMPONENT {
            return;
        }
        self.seen.insert(key);
        self.edges.push(synthesized(
            component,
            route,
            EdgeKind::Navigates,
            line,
            metadata,
        ));
    }

    /// The link at `site` to every route `href` names there, each edge with
    /// the arm that named its route.
    fn add_destinations(
        &mut self,
        table: &dyn LinkRoutes,
        site: &Site<'_>,
        href: &HrefLiteral,
        synthesized_by: &str,
        nav_method: &str,
    ) {
        // A destination written as a choice names one route per arm, and
        // the user reaches every one of them: each is drawn.
        for (route, arm) in table.destinations(site.file, href) {
            self.add(
                site.component,
                &route,
                site.line,
                json!({
                    "synthesizedBy": synthesized_by,
                    "href": arm.display,
                    "navMethod": nav_method,
                    "registeredAt": format!("{}:{}", site.file, site.line),
                }),
            );
        }
    }
}

/// Where a link is written: its file, its line and the function or
/// component holding it.
struct Site<'s> {
    file: &'s str,
    line: i64,
    component: &'s Node,
}

/// The string literal right after a matched `={` (the expression form of an
/// attribute), `None` when the expression starts with anything else.
fn literal_after(source: &str, at: usize) -> Option<String> {
    matches!(source.as_bytes().get(at), Some(b'"' | b'\'' | b'`'))
        .then(|| read_string_at(source, at))
        .flatten()
}

// =============================================================================
// Next.js
// =============================================================================

/// `<Link … href=…` / `<NextLink … href=…` / `<a … href=…`, the attribute
/// anywhere in the tag (upstream `LINK_TAG`).
static NEXT_LINK_TAG: LazyLock<Regex> = LazyLock::new(|| {
    compile(r#"<(Link|NextLink|a)(?-u:\b)([^>]*?)(?-u:\b)href\s*=\s*(?:"([^"]*)"|'([^']*)'|\{\s*)"#)
});

/// Next.js `<Link href>`, `<NextLink href>` and internal `<a href>`
/// (`nextLinkEdges`). Only a destination starting with `/` is read, for
/// every tag (KEEP-RUST, OD-B10): upstream skips an external `<a href>`
/// only, and its table reads an absolute URL's path, so a `<Link
/// href="https://…">` reached the site's own `/`. Upstream's `href={{
/// pathname }}` reading starts its sticky key at an offset of the whole
/// source in a slice that starts there, so it never matches: an object
/// destination names nothing there, nor here.
pub(super) fn next_link_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let context = view.context();
    let table = next_link_routes(&context);
    if table.is_empty() {
        return Ok(Vec::new());
    }
    let mut links = Links::default();
    for file in view.files()? {
        if !JSX_FILE.is_match(&file) || is_test_path(&file) || !table.serves(&file) {
            continue;
        }
        let Some(source) = view.text(&file) else {
            continue;
        };
        if !source.contains("href") {
            continue;
        }
        let safe = strip_js_comments_keeping_offsets(&source);
        let callables = view.callables_in_file(&file)?;
        let lines = LineAt::new(&safe, 1);
        for found in NEXT_LINK_TAG.captures_iter(&safe) {
            let whole = found.get(0).expect("whole match");
            let tag = &found[1];
            // `href="…"`, or `href={…}` holding a string or a template.
            let literal = found
                .get(3)
                .or_else(|| found.get(4))
                .map(|quoted| quoted.as_str().to_string())
                .or_else(|| literal_after(&safe, whole.end()));
            let Some(literal) = literal.filter(|literal| literal.starts_with('/')) else {
                continue;
            };
            let Some(href) = to_href(Some(literal)) else {
                continue;
            };
            let line = lines.line(whole.start());
            let Some(component) = enclosing_fn(&callables, line) else {
                continue;
            };
            let nav_method = if tag == "a" { "a" } else { "link" };
            links.add_destinations(
                table.as_ref(),
                &Site {
                    file: &file,
                    line,
                    component,
                },
                &href,
                "next-link",
                nav_method,
            );
        }
    }
    Ok(links.edges)
}

// =============================================================================
// React Router
// =============================================================================

/// The tags that carry a route as a `to` attribute (upstream `LINK_TAGS`).
const LINK_TAGS: [&str; 6] = [
    "Link",
    "NavLink",
    "Navigate",
    "Redirect",
    "LinkContainer",
    "IndexLinkContainer",
];

/// `<Tag … to=…>` for any of `tags`, the attribute anywhere in the tag; an
/// attribute before `to` may hold an arrow, whose `>` is not the tag's end
/// (upstream `linkTagPattern` at `b635dd46`, which escapes a tag's `$`
/// only). Upstream's lookahead for `to={…}` is a group around the brace
/// here: a match starts at `<`, so the next one starts where it would.
fn react_link_tag_pattern(tags: &[String]) -> String {
    let alternation: Vec<String> = tags.iter().map(|tag| tag.replace('$', r"\$")).collect();
    js(&format!(
        r#"<({})(?-u:\b)((?:[^>]|=>)*?)(?-u:\b)to\s*=\s*(?:"([^"]*)"|'([^']*)'|(\{{))"#,
        alternation.join("|")
    ))
}

static REACT_LINK_TAG: LazyLock<Regex> = LazyLock::new(|| {
    let tags: Vec<String> = LINK_TAGS.iter().map(|tag| tag.to_string()).collect();
    Regex::new(&react_link_tag_pattern(&tags)).expect("the link tag pattern compiles")
});
/// The prefilter for a file whose tags are React Router's own.
static REACT_HAS_LINK_TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!("<(?:{})(?-u:\\b)", LINK_TAGS.join("|"))).expect("link tag prefilter")
});
static TO_ATTRIBUTE: LazyLock<Regex> = LazyLock::new(|| compile(r"(?-u:\b)to\s*="));
/// A file that imports React Router's own components (`ROUTER_IMPORT`).
static ROUTER_IMPORT: LazyLock<Regex> =
    LazyLock::new(|| compile(r#"(?-u:\b)from\s*['"]react-router(?:-dom)?['"]"#));
static STYLED_LINK: LazyLock<Regex> =
    LazyLock::new(|| compile(r"(?-u:\b)styled\s*\(\s*(?:Link|NavLink)\s*\)"));
/// `const HeaderLink = styled(Link)` (`STYLED_LINK_CONST`).
static STYLED_LINK_CONST: LazyLock<Regex> = LazyLock::new(|| {
    compile(
        r"(?-u:\b)(?:export\s+)?(?:const|let)\s+([A-Z][A-Za-z0-9_$]*)\s*=\s*styled\s*\(\s*(?:Link|NavLink)\s*\)",
    )
});
/// `export default styled(Link)` (`STYLED_LINK_DEFAULT`).
static STYLED_LINK_DEFAULT: LazyLock<Regex> =
    LazyLock::new(|| compile(r"(?-u:\b)export\s+default\s+styled\s*\(\s*(?:Link|NavLink)\s*\)"));

/// Every styled-components or emotion wrapper of `Link` or `NavLink` in the
/// project (upstream `linkWrappers`): the ones each file declares by name,
/// in the order it declares them, and the files whose default export is
/// one.
#[derive(Default)]
struct LinkWrappers {
    named: HashMap<String, Vec<String>>,
    defaults: HashSet<String>,
}

fn link_wrappers(view: &View<'_>, files: &[String]) -> LinkWrappers {
    let mut wrappers = LinkWrappers::default();
    for file in files {
        if !JSX_FILE.is_match(file) || is_test_path(file) {
            continue;
        }
        let Some(source) = view.text(file) else {
            continue;
        };
        if !source.contains("styled")
            || !STYLED_LINK.is_match(&source)
            || !ROUTER_IMPORT.is_match(&source)
        {
            continue;
        }
        if STYLED_LINK_DEFAULT.is_match(&source) {
            wrappers.defaults.insert(file.clone());
        }
        for found in STYLED_LINK_CONST.captures_iter(&source) {
            let names = wrappers.named.entry(file.clone()).or_default();
            if !names.iter().any(|name| name == &found[1]) {
                names.push(found[1].to_string());
            }
        }
    }
    wrappers
}

/// The language a JSX file's imports are read in (upstream `languageOf`).
fn language_of(file: &str) -> Language {
    static TS: LazyLock<Regex> = LazyLock::new(|| compile(r"\.[cm]?ts$"));
    if file.ends_with(".tsx") {
        Language::Tsx
    } else if TS.is_match(file) {
        Language::TypeScript
    } else if file.ends_with(".jsx") {
        Language::Jsx
    } else {
        Language::JavaScript
    }
}

/// The link tags `file` can write: React Router's own, the wrappers it
/// declares and the wrappers it imports, by the name it imports them under
/// (upstream `linkTagsFor`).
fn link_tags_for(
    file: &str,
    wrappers: &LinkWrappers,
    context: &ViewContext<'_, '_>,
) -> Vec<String> {
    let mut tags: Vec<String> = LINK_TAGS.iter().map(|tag| tag.to_string()).collect();
    tags.extend(wrappers.named.get(file).into_iter().flatten().cloned());
    if !wrappers.named.is_empty() || !wrappers.defaults.is_empty() {
        let language = language_of(file);
        for mapping in context.get_import_mappings(file, language) {
            let Some(target) = resolve_import_path(&mapping.source, file, language, context) else {
                continue;
            };
            if (mapping.is_default && wrappers.defaults.contains(&target))
                || wrappers
                    .named
                    .get(&target)
                    .is_some_and(|names| names.contains(&mapping.exported_name))
            {
                tags.push(mapping.local_name);
            }
        }
    }
    tags
}

/// React Router `<Link to>`, `<NavLink to>`, `<Navigate to>`, v5's
/// `<Redirect to>`, react-router-bootstrap's `<LinkContainer to>` and the
/// styled wrappers of a link (`reactRouterLinkEdges`, at `b635dd46`; #2769's
/// escaping of the prefilter is not ported). A `to={…}` expression is read
/// as a navigation call's argument is, else as a route-config object's
/// `getHref` (#2187); a relative `to` resolves against a nesting this scan
/// does not read, so it names nothing.
pub(super) fn react_router_link_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let context = view.context();
    let table = react_router_link_routes(&context);
    if table.is_empty() {
        return Ok(Vec::new());
    }
    let files = view.files()?;
    let wrappers = link_wrappers(view, &files);
    let mut links = Links::default();
    for file in &files {
        if !JSX_FILE.is_match(file) || is_test_path(file) || !table.serves(file) {
            continue;
        }
        let Some(source) = view.text(file) else {
            continue;
        };
        if !TO_ATTRIBUTE.is_match(&source) {
            continue;
        }
        let tags = link_tags_for(file, &wrappers, &context);
        let own;
        let (prefilter, pattern): (&Regex, &Regex) = if tags.len() == LINK_TAGS.len() {
            (&*REACT_HAS_LINK_TAG, &*REACT_LINK_TAG)
        } else {
            // The prefilter joins the tags as written (`b635dd46`), so a tag
            // holding `$` matches nothing there.
            let (Ok(prefilter), Ok(pattern)) = (
                Regex::new(&format!("<(?:{})(?-u:\\b)", tags.join("|"))),
                Regex::new(&react_link_tag_pattern(&tags)),
            ) else {
                continue;
            };
            own = (prefilter, pattern);
            (&own.0, &own.1)
        };
        if !prefilter.is_match(&source) {
            continue;
        }
        let safe = strip_js_comments_keeping_offsets(&source);
        let callables = view.callables_in_file(file)?;
        let lines = LineAt::new(&safe, 1);
        for found in pattern.captures_iter(&safe) {
            let whole = found.get(0).expect("whole match");
            let tag = &found[1];
            let href = match (found.get(3).or_else(|| found.get(4)), found.get(5)) {
                (Some(quoted), _) => to_href(Some(quoted.as_str().to_string())),
                // `to={…}` holds an expression, read with the reader a
                // navigation call's argument is: a string, a template, a
                // `{ pathname }` object or a conditional; else a route-config
                // object's `getHref`.
                (None, Some(brace)) => {
                    let at = brace.start();
                    let Some(close) = match_bracket(&safe, at) else {
                        continue;
                    };
                    let expression = &safe[at + 1..close];
                    parse_href_expression(expression).or_else(|| {
                        config_href_expression(expression, file, &context)
                            .as_deref()
                            .and_then(parse_href_expression)
                    })
                }
                (None, None) => None,
            };
            let Some(href) = href.filter(|href| href.path.starts_with('/')) else {
                continue;
            };
            let line = lines.line(whole.start());
            let Some(component) = enclosing_fn(&callables, line) else {
                continue;
            };
            let nav_method = match tag {
                "Navigate" => "navigate",
                "Redirect" => "redirect",
                _ => "link",
            };
            links.add_destinations(
                table.as_ref(),
                &Site {
                    file,
                    line,
                    component,
                },
                &href,
                "react-router-link",
                nav_method,
            );
        }
    }
    Ok(links.edges)
}

// =============================================================================
// TanStack Router
// =============================================================================

/// `<Link … to=` / `<Navigate … to=` (upstream `LINK_TAG`).
static TANSTACK_LINK_TAG: LazyLock<Regex> = LazyLock::new(|| {
    compile(r#"<(Link|Navigate)(?-u:\b)([^>]*?)(?-u:\b)to\s*=\s*(?:"([^"]*)"|'([^']*)'|\{\s*)"#)
});
static TANSTACK_HAS_LINK_TAG: LazyLock<Regex> =
    LazyLock::new(|| compile(r"<(?:Link|Navigate)(?-u:\b)"));

/// TanStack Router `<Link to>` and `<Navigate to>` (`tanstackLinkEdges`):
/// `to` is the route PATTERN, read as a route name is, the values riding
/// beside it in `params`. A `<Link from>` with no `to` names no destination.
pub(super) fn tanstack_link_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let context = view.context();
    let table = tanstack_link_routes(&context);
    if table.is_empty() {
        return Ok(Vec::new());
    }
    let mut links = Links::default();
    for file in view.files()? {
        if !JS_FILE.is_match(&file) || is_test_path(&file) || !table.serves(&file) {
            continue;
        }
        let Some(source) = view.text(&file) else {
            continue;
        };
        if !TANSTACK_HAS_LINK_TAG.is_match(&source) {
            continue;
        }
        let safe = strip_js_comments_keeping_offsets(&source);
        let callables = view.callables_in_file(&file)?;
        let lines = LineAt::new(&safe, 1);
        for found in TANSTACK_LINK_TAG.captures_iter(&safe) {
            let whole = found.get(0).expect("whole match");
            // `to="…"`, or `to={…}` holding a string or a template.
            let literal = found
                .get(3)
                .or_else(|| found.get(4))
                .map(|quoted| quoted.as_str().to_string())
                .or_else(|| literal_after(&safe, whole.end()));
            let Some(literal) = literal else {
                continue;
            };
            // Read as the literal written in a call (`JSON.stringify`).
            let Ok(quoted) = serde_json::to_string(&literal) else {
                continue;
            };
            let Some(href) = tanstack_destination(&quoted) else {
                continue;
            };
            let line = lines.line(whole.start());
            let Some(component) = enclosing_fn(&callables, line) else {
                continue;
            };
            let nav_method = if &found[1] == "Navigate" {
                "navigate"
            } else {
                "link"
            };
            links.add_destinations(
                table.as_ref(),
                &Site {
                    file: &file,
                    line,
                    component,
                },
                &href,
                "tanstack-link",
                nav_method,
            );
        }
    }
    Ok(links.edges)
}

// =============================================================================
// Vue Router
// =============================================================================

/// `<router-link … to=` / `<RouterLink … :to=` / `<NuxtLink … to=`
/// (upstream `LINK_TAG`).
static VUE_LINK_TAG: LazyLock<Regex> = LazyLock::new(|| {
    compile(
        r#"<(router-link|RouterLink|NuxtLink|nuxt-link)(?-u:\b)([^>]*?)\s:?to\s*=\s*(?:"([^"]*)"|'([^']*)')"#,
    )
});
static VUE_HAS_LINK_TAG: LazyLock<Regex> =
    LazyLock::new(|| compile(r"<(?:router-link|RouterLink|NuxtLink|nuxt-link)(?-u:\b)"));

/// Vue Router's and Nuxt's `<router-link to>` and bound `:to`, by path or by
/// route name (`vueRouterLinkEdges`). A name two routes of the link's app
/// carry names nothing, as a `router.push({ name })` does (W7-11).
pub(super) fn vue_router_link_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let context = view.context();
    let table = vue_link_routes(&context);
    if table.is_empty() {
        return Ok(Vec::new());
    }
    let mut links = Links::default();
    for file in view.files()? {
        if !TEMPLATE_FILE.is_match(&file) || is_test_path(&file) || !table.serves(&file) {
            continue;
        }
        let Some(source) = view.text(&file) else {
            continue;
        };
        if !VUE_HAS_LINK_TAG.is_match(&source) {
            continue;
        }
        let callables = view.callables_in_file(&file)?;
        let lines = LineAt::new(&source, 1);
        for found in VUE_LINK_TAG.captures_iter(&source) {
            let whole = found.get(0).expect("whole match");
            // A bound `:to` holds an expression; a plain `to` a literal path.
            let value = js_trim(
                found
                    .get(3)
                    .or_else(|| found.get(4))
                    .map_or("", |quoted| quoted.as_str()),
            );
            if value.is_empty() {
                continue;
            }
            let line = lines.line(whole.start());
            let Some(component) = enclosing_fn(&callables, line) else {
                continue;
            };
            let bound = whole.as_str().contains(":to");
            let named = if bound {
                route_name_in_expression(value)
            } else {
                None
            };
            if let Some(name) = &named
                && let Some(route) = table.named(&file, name)
            {
                // A `{ name }` destination names exactly one route.
                links.add(
                    component,
                    &route,
                    line,
                    json!({
                        "synthesizedBy": "vue-router-link",
                        "href": name,
                        "navMethod": "link",
                        "by": "name",
                        "registeredAt": format!("{file}:{line}"),
                    }),
                );
                continue;
            }
            let href = if bound {
                parse_vue_path_object(value).or_else(|| to_href(read_string_at(value, 0)))
            } else {
                to_href(Some(value.to_string()))
            };
            let Some(href) = href.filter(|href| href.path.starts_with('/')) else {
                continue;
            };
            links.add_destinations(
                table.as_ref(),
                &Site {
                    file: &file,
                    line,
                    component,
                },
                &href,
                "vue-router-link",
                "link",
            );
        }
    }
    Ok(links.edges)
}

// =============================================================================
// SvelteKit
// =============================================================================

/// `<a … href=…`, quoted or bound (upstream `LINK_TAG`).
static SVELTE_LINK_TAG: LazyLock<Regex> = LazyLock::new(|| {
    compile(r#"<a(?-u:\b)([^>]*?)(?-u:\b)href\s*=\s*(?:"([^"]*)"|'([^']*)'|\{\s*)"#)
});
/// A Svelte `{expr}` interpolation inside an attribute value.
static SVELTE_INTERPOLATION: LazyLock<Regex> = LazyLock::new(|| compile(r"\{[^}]*\}"));

/// SvelteKit's internal `<a href>`: the router intercepts it, so it is the
/// navigation (`svelteKitLinkEdges`). An external href leaves the site.
pub(super) fn sveltekit_link_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let context = view.context();
    let table = sveltekit_link_routes(&context);
    if table.is_empty() {
        return Ok(Vec::new());
    }
    let mut links = Links::default();
    for file in view.files()? {
        if !file.ends_with(".svelte") || is_test_path(&file) || !table.serves(&file) {
            continue;
        }
        let Some(source) = view.text(&file) else {
            continue;
        };
        if !source.contains("href") {
            continue;
        }
        let callables = view.callables_in_file(&file)?;
        let lines = LineAt::new(&source, 1);
        for found in SVELTE_LINK_TAG.captures_iter(&source) {
            let whole = found.get(0).expect("whole match");
            // `href="…"`, or `href={…}` holding a string or a template.
            let literal = found
                .get(2)
                .or_else(|| found.get(3))
                .map(|quoted| quoted.as_str().to_string())
                .or_else(|| literal_after(&source, whole.end()));
            let Some(literal) = literal.filter(|literal| literal.starts_with('/')) else {
                continue;
            };
            // A `{expr}` inside the attribute is the hole a template's `${…}`
            // is: `/profile/@{user.username}` reaches `/profile/@:user`.
            let holed = SVELTE_INTERPOLATION
                .replace_all(&literal, HOLE.to_string().as_str())
                .into_owned();
            let Some(href) = to_href(Some(holed)) else {
                continue;
            };
            let line = lines.line(whole.start());
            let Some(component) = enclosing_fn(&callables, line) else {
                continue;
            };
            links.add_destinations(
                table.as_ref(),
                &Site {
                    file: &file,
                    line,
                    component,
                },
                &href,
                "sveltekit-link",
                "a",
            );
        }
    }
    Ok(links.edges)
}

// =============================================================================
// Expo Router: a destination a helper returns
// =============================================================================

/// `.push(await helper(` / `.navigate(obj.helper(` (`NAV_FED_BY_CALL`).
static NAV_FED_BY_CALL: LazyLock<Regex> = LazyLock::new(|| {
    compile(
        r"\.(push|replace|navigate|dismissTo)\(\s*(?:await\s+)?([A-Za-z_$][A-Za-z0-9_$.]*)\s*\(",
    )
});
static NAV_CALL: LazyLock<Regex> =
    LazyLock::new(|| compile(r"\.(?:push|replace|navigate|dismissTo)\("));

/// A navigation call whose argument is a call (upstream `NavSite`).
struct NavSite {
    file: String,
    line: i64,
    method: String,
    callee: String,
}

/// One screen a helper's body names: the route, the literal as written, and
/// where it is, its column in UTF-16 code units as upstream's.
struct HelperScreen {
    node: Arc<Node>,
    href: String,
    line: i64,
    col: i64,
}

/// `router.push(await resolvePostLoginRoute())`: the screens written in the
/// helper's body are where the navigation goes, one `navigates` edge from
/// the helper to each (`expoRouterReturnEdges`), with the literal's own
/// column, so the two arms of a ternary on one line stay apart. A helper is
/// read only because a navigation consumes it. Its literals are matched
/// against the screens of the navigation's own Expo app, and a navigation
/// under no Expo app reads none, as the resolver matches a call (KEEP-RUST,
/// W7-08): upstream reads one table for the project.
pub(super) fn expo_router_return_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let context = view.context();
    let screens = expo_screens(&context);
    if screens.is_empty() {
        return Ok(Vec::new());
    }
    // 1. Every navigation call whose argument is a call.
    let mut sites = Vec::new();
    for file in view.files()? {
        if !JS_FILE.is_match(&file) {
            continue;
        }
        let Some(source) = view.text(&file) else {
            continue;
        };
        if !NAV_CALL.is_match(&source) {
            continue;
        }
        let stripped = strip_js_comments_keeping_offsets(&source);
        let lines = LineAt::new(&stripped, 1);
        for found in NAV_FED_BY_CALL.captures_iter(&stripped) {
            sites.push(NavSite {
                file: file.clone(),
                line: lines.line(found.get(0).expect("whole match").start()),
                method: found[1].to_string(),
                callee: found[2].to_string(),
            });
        }
    }
    // 2. Each callee, the project function it names, the screens its body
    //    names in the navigation's app.
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut memo: HashMap<(String, String), Option<Vec<HelperScreen>>> = HashMap::new();
    for site in &sites {
        let Some(root) = screens.app_root(&site.file) else {
            continue;
        };
        let Some(helper) = resolve_helper(view, site)? else {
            continue;
        };
        let key = (helper.id.clone(), root.to_string());
        let found = memo.entry(key).or_insert_with(|| {
            screens_in_body(view, &helper, |segs| screens.screen(root, &site.file, segs))
        });
        let Some(found) = found else {
            continue;
        };
        for screen in found.iter() {
            if !seen.insert((helper.id.clone(), screen.node.id.clone())) {
                continue;
            }
            let mut edge = synthesized(
                &helper,
                &screen.node,
                EdgeKind::Navigates,
                screen.line,
                json!({
                    "synthesizedBy": "expo-router-return",
                    "href": screen.href,
                    "navMethod": site.method,
                    "registeredAt": format!("{}:{}", site.file, site.line),
                }),
            );
            edge.col = Some(screen.col);
            edges.push(edge);
        }
    }
    Ok(edges)
}

/// The function `site.callee` names, seen from `site.file` (upstream
/// `resolveHelper`): the one of its name in the site's file, else the one in
/// the project; two in the file, or none there and two in the project, is
/// none. Upstream's step between them reads an import mapping's
/// `resolvedPath`, which nothing upstream sets (`extractImportMappings`, the
/// context's `getImportMappings`), so it never decides and is not ported.
fn resolve_helper(view: &View<'_>, site: &NavSite) -> Result<Option<Node>> {
    let bare = site.callee.rsplit('.').next().unwrap_or(&site.callee);
    let candidates: Vec<Node> = view
        .nodes_named(bare)?
        .into_iter()
        .filter(|node| {
            matches!(node.kind, NodeKind::Function | NodeKind::Method)
                && matches!(
                    node.language,
                    Language::TypeScript | Language::JavaScript | Language::Tsx | Language::Jsx
                )
        })
        .collect();
    let local: Vec<&Node> = candidates
        .iter()
        .filter(|node| node.file_path == site.file)
        .collect();
    Ok(match (local.as_slice(), candidates.as_slice()) {
        ([only], _) => Some((*only).clone()),
        ([], [only]) => Some(only.clone()),
        _ => None,
    })
}

/// The screens string literals in `helper`'s body name, each literal
/// beginning with `/` read as an href and matched by `screen` (upstream
/// `screensInBody`): `None` when the body cannot be read, names none, or
/// names more than [`MAX_SCREENS_PER_HELPER`]. The body starts at the arrow,
/// or the first brace for a declaration, so a return type of literal routes
/// is not read.
fn screens_in_body(
    view: &View<'_>,
    helper: &Node,
    screen: impl Fn(&[String]) -> Option<Arc<Node>>,
) -> Option<Vec<HelperScreen>> {
    let text = view.text(&helper.file_path)?;
    let lines: Vec<&str> = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    let start = usize::try_from(helper.start_line - 1).ok()?;
    let end = usize::try_from(helper.end_line).ok()?.min(lines.len());
    let raw = lines.get(start..end.max(start))?.join("\n");
    let body = strip_js_comments_keeping_offsets(&raw);
    let arrow = body.find("=>");
    let brace = body.find('{');
    let scan_from = match (arrow, brace) {
        (Some(arrow), None) => arrow + 2,
        (Some(arrow), Some(brace)) if arrow < brace => arrow + 2,
        (_, Some(brace)) => brace + 1,
        (None, None) => 0,
    };
    let bytes = body.as_bytes();
    let mut found: Vec<HelperScreen> = Vec::new();
    let mut i = scan_from;
    while i < bytes.len() {
        if !matches!(bytes[i], b'"' | b'\'' | b'`') {
            i += 1;
            continue;
        }
        let at = i;
        let literal = read_string_at(&body, at);
        i = string_end(bytes, at) + 1;
        let Some(literal) = literal.filter(|literal| literal.starts_with('/')) else {
            continue;
        };
        let Some(href) = to_href(Some(literal)) else {
            continue;
        };
        let Some(segs) = normalize_href_path(&href.path, &helper.file_path) else {
            continue;
        };
        let Some(node) = screen(&segs) else {
            continue;
        };
        if found.iter().any(|seen| seen.node.id == node.id) {
            continue;
        }
        // The body starts at column 0 of the helper's first line and the
        // comment stripper keeps byte offsets, so `at` is the literal's
        // offset in `raw` too. Its column counts `raw`'s UTF-16 code units:
        // the stripper blanks a comment a byte a space, upstream's a code
        // unit a space, so a character a comment holds before the literal
        // counts once, as written.
        let line_start = raw[..at].rfind('\n').map_or(0, |newline| newline + 1);
        found.push(HelperScreen {
            node,
            href: href.display.clone(),
            line: helper.start_line + raw[..at].matches('\n').count() as i64,
            col: raw[line_start..at].encode_utf16().count() as i64,
        });
        if found.len() > MAX_SCREENS_PER_HELPER {
            return None;
        }
    }
    (!found.is_empty()).then_some(found)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::UNIX_EPOCH;

    use codegraph_core::node_id::hash_content;
    use codegraph_core::types::{Edge, FileRecord, Language, Node, NodeKind};
    use codegraph_store::Store;

    /// A project of hand-written files whose index holds exactly the nodes
    /// given, with no parser in between: the route nodes in the shape each
    /// router's extraction gives them, and the functions links sit in.
    struct Fixture {
        dir: PathBuf,
        files: Vec<(String, String, Language)>,
        nodes: Vec<Node>,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "cg-links-{name}-{}-{}",
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

        fn file(mut self, path: &str, language: Language, text: &str) -> Self {
            self.files
                .push((path.to_string(), text.to_string(), language));
            self
        }

        fn node(mut self, node: Node) -> Self {
            self.nodes.push(node);
            self
        }

        /// The edges the pass naming itself `synthesized_by` draws, as
        /// `source -> target href`, sorted.
        fn links(&self, synthesized_by: &str) -> Vec<String> {
            let name = |id: &str| {
                self.nodes
                    .iter()
                    .find(|node| node.id == id)
                    .map_or("?", |node| node.name.as_str())
                    .to_string()
            };
            let mut links: Vec<String> = self
                .edges(synthesized_by)
                .into_iter()
                .map(|edge| {
                    let href = edge.metadata.as_ref().unwrap()["href"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    format!("{} -> {} {href}", name(&edge.source), name(&edge.target))
                })
                .collect();
            links.sort();
            links
        }

        /// The edges the pass naming itself `synthesized_by` draws.
        fn edges(&self, synthesized_by: &str) -> Vec<Edge> {
            let store = Store::open(&self.dir.join("index.db")).unwrap();
            let mut store = store;
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
            crate::synthesis::synthesize(&store, &self.dir, 1 << 20)
                .unwrap()
                .into_iter()
                .filter(|edge| {
                    edge.metadata
                        .as_ref()
                        .is_some_and(|metadata| metadata["synthesizedBy"] == synthesized_by)
                })
                .collect()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn node(
        id: &str,
        kind: NodeKind,
        name: &str,
        file: &str,
        language: Language,
        lines: (i64, i64),
    ) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: file.to_string(),
            language,
            start_line: lines.0,
            end_line: lines.1,
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

    fn function(name: &str, file: &str, language: Language, lines: (i64, i64)) -> Node {
        node(
            &format!("function:{file}:{name}:{}", lines.0),
            NodeKind::Function,
            name,
            file,
            language,
            lines,
        )
    }

    /// A route node as the Next.js, Expo or SvelteKit extraction names it:
    /// by the path its file gives.
    fn file_route(path: &str, file: &str, language: Language) -> Node {
        node(
            &format!("route:{file}:1:{path}"),
            NodeKind::Route,
            path,
            file,
            language,
            (1, 1),
        )
    }

    /// A route node as React Router's extraction names it, at `line` of
    /// `file`.
    fn react_route(path: &str, file: &str, line: i64) -> Node {
        node(
            &format!("route:{file}:{line}:{path}"),
            NodeKind::Route,
            path,
            file,
            Language::Tsx,
            (line, line),
        )
    }

    /// A Next.js page at `path`: `app/{path}/page.tsx`.
    fn next_page(path: &str) -> (String, Node) {
        let file = if path == "/" {
            "app/page.tsx".to_string()
        } else {
            format!("app{path}/page.tsx")
        };
        let node = file_route(path, &file, Language::Tsx);
        (file, node)
    }

    fn with_next_pages(mut fixture: Fixture, paths: &[&str]) -> Fixture {
        for path in paths {
            let (file, page) = next_page(path);
            fixture = fixture
                .file(&file, Language::Tsx, "export default function Page() {}\n")
                .node(page);
        }
        fixture
    }

    /// Only a destination starting with `/` names a page, for a `<Link
    /// href>` as for an `<a href>` (OD-B10): an absolute URL, a `mailto:`, a
    /// computed href and a path no page serves draw nothing.
    #[test]
    fn a_next_link_names_a_page_only_by_a_path_it_serves() {
        let nav = "export function Nav({ href }) {\n  return (\n    <nav>\n      <Link href=\"https://example.com/\">Out</Link>\n      <a href=\"mailto:me@example.com\">Mail</a>\n      <Link href={href}>Computed</Link>\n      <Link href=\"/nowhere\">Nowhere</Link>\n      <Link href=\"/users\">Users</Link>\n    </nav>\n  )\n}\n";
        let fixture = with_next_pages(Fixture::new("next-paths"), &["/", "/users"])
            .file("src/Nav.tsx", Language::Tsx, nav)
            .node(function("Nav", "src/Nav.tsx", Language::Tsx, (1, 11)));
        assert_eq!(fixture.links("next-link"), ["Nav -> /users /users"]);
    }

    /// A component draws at most 24 links: a pair already drawn is skipped
    /// and not counted, and from the 25th route on nothing is drawn.
    #[test]
    fn a_component_links_at_most_24_routes_a_repeated_pair_uncounted() {
        let paths: Vec<String> = (1..=26).map(|i| format!("/p{i:02}")).collect();
        let path_refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let mut menu = String::from("export function Menu() {\n  return (\n    <nav>\n");
        menu.push_str("      <Link href=\"/p01\">again</Link>\n");
        for path in &paths {
            menu.push_str(&format!("      <Link href=\"{path}\">{path}</Link>\n"));
        }
        menu.push_str("    </nav>\n  )\n}\n");
        let last = menu.lines().count() as i64;
        let fixture = with_next_pages(Fixture::new("next-cap"), &path_refs)
            .file("src/Menu.tsx", Language::Tsx, &menu)
            .node(function("Menu", "src/Menu.tsx", Language::Tsx, (1, last)));
        let expected: Vec<String> = paths[..24]
            .iter()
            .map(|path| format!("Menu -> {path} {path}"))
            .collect();
        assert_eq!(fixture.links("next-link"), expected);
    }

    /// A link written in a test file is not read.
    #[test]
    fn a_link_in_a_test_file_is_not_read() {
        let link = "export function Nav() {\n  return <Link href=\"/users\">Users</Link>\n}\n";
        let fixture = with_next_pages(Fixture::new("next-test-path"), &["/users"])
            .file("src/__tests__/nav.test.tsx", Language::Tsx, link)
            .node(function(
                "Nav",
                "src/__tests__/nav.test.tsx",
                Language::Tsx,
                (1, 3),
            ))
            .file("src/Nav.tsx", Language::Tsx, link)
            .node(function("Nav", "src/Nav.tsx", Language::Tsx, (1, 3)));
        assert_eq!(fixture.links("next-link"), ["Nav -> /users /users"]);
    }

    /// A link is read only from a file under a Next app's root: a package
    /// beside the app names none of its pages.
    #[test]
    fn a_next_link_outside_every_app_root_is_not_read() {
        let link = |name: &str| {
            format!(
                "export function {name}() {{\n  return <Link href=\"/users\">Users</Link>\n}}\n"
            )
        };
        let fixture = Fixture::new("next-roots")
            .file(
                "apps/web/app/users/page.tsx",
                Language::Tsx,
                "export default function Page() {}\n",
            )
            .node(file_route(
                "/users",
                "apps/web/app/users/page.tsx",
                Language::Tsx,
            ))
            .file(
                "apps/web/components/Nav.tsx",
                Language::Tsx,
                &link("AppNav"),
            )
            .node(function(
                "AppNav",
                "apps/web/components/Nav.tsx",
                Language::Tsx,
                (1, 3),
            ))
            .file("packages/ui/Nav.tsx", Language::Tsx, &link("PackageNav"))
            .node(function(
                "PackageNav",
                "packages/ui/Nav.tsx",
                Language::Tsx,
                (1, 3),
            ));
        assert_eq!(fixture.links("next-link"), ["AppNav -> /users /users"]);
    }

    /// `href={…}` holding a string or a template names its page, a template's
    /// hole a dynamic segment; a commented-out link names nothing.
    #[test]
    fn a_next_link_reads_an_expression_but_not_a_comment() {
        let nav = "export function Nav({ id }) {\n  return (\n    <nav>\n      {/* <Link href=\"/old\">Old</Link> */}\n      <Link href={'/'}>Home</Link>\n      <Link href={`/users/${id}`}>User</Link>\n    </nav>\n  )\n}\n";
        let fixture = with_next_pages(Fixture::new("next-expressions"), &["/", "/old"])
            .file(
                "app/users/[id]/page.tsx",
                Language::Tsx,
                "export default function Page() {}\n",
            )
            .node(file_route(
                "/users/:id",
                "app/users/[id]/page.tsx",
                Language::Tsx,
            ))
            .file("src/Nav.tsx", Language::Tsx, nav)
            .node(function("Nav", "src/Nav.tsx", Language::Tsx, (1, 9)));
        assert_eq!(
            fixture.links("next-link"),
            ["Nav -> / /", "Nav -> /users/:id /users/${\u{2026}}"]
        );
    }

    /// A relative `to` names nothing, even when another arm of its
    /// conditional is absolute, a path two routes of the app serve reaches
    /// neither, and a commented-out link names nothing.
    #[test]
    fn a_react_router_link_names_nothing_relative_or_ambiguous() {
        let nav = "export function Nav({ cond }) {\n  return (\n    <nav>\n      <Link to=\"x\">Relative</Link>\n      <Link to={cond ? 'edit' : '/y'}>Choice</Link>\n      <Link to=\"/dup\">Twice</Link>\n      {/* <Link to=\"/y\">Old</Link> */}\n      <Link to=\"/x\">X</Link>\n    </nav>\n  )\n}\n";
        let fixture = Fixture::new("react-ambiguous")
            .file(
                "src/App.tsx",
                Language::Tsx,
                "export function App() {\n  return null\n}\n",
            )
            .node(react_route("/x", "src/App.tsx", 1))
            .node(react_route("/y", "src/App.tsx", 1))
            .node(react_route("/dup", "src/App.tsx", 2))
            .node(react_route("/dup", "src/App.tsx", 3))
            .file("src/Nav.tsx", Language::Tsx, nav)
            .node(function("Nav", "src/Nav.tsx", Language::Tsx, (1, 11)));
        assert_eq!(fixture.links("react-router-link"), ["Nav -> /x /x"]);
    }

    /// A `styled(Link)` wraps React Router's link only in a file importing
    /// React Router: another library's `Link` styled the same way is no
    /// wrapper.
    #[test]
    fn a_styled_link_of_another_library_wraps_nothing() {
        let styled = |library: &str, wrapper: &str, component: &str| {
            format!(
                "import {{ Link }} from '{library}'\nimport styled from 'styled-components'\nexport const {wrapper} = styled(Link)`color: red;`\nexport function {component}() {{\n  return <{wrapper} to=\"/x\">X</{wrapper}>\n}}\n"
            )
        };
        let fixture = Fixture::new("react-styled")
            .file(
                "src/App.tsx",
                Language::Tsx,
                "export function App() {\n  return null\n}\n",
            )
            .node(react_route("/x", "src/App.tsx", 1))
            .file(
                "src/Fancy.tsx",
                Language::Tsx,
                &styled("@mui/material", "FancyLink", "Fancy"),
            )
            .node(function("Fancy", "src/Fancy.tsx", Language::Tsx, (4, 6)))
            .file(
                "src/Plain.tsx",
                Language::Tsx,
                &styled("react-router-dom", "PlainLink", "Plain"),
            )
            .node(function("Plain", "src/Plain.tsx", Language::Tsx, (4, 6)));
        assert_eq!(fixture.links("react-router-link"), ["Plain -> /x /x"]);
    }

    /// A link reaches only the routes of its own app.
    #[test]
    fn a_react_router_link_reaches_only_its_apps_routes() {
        let nav = "export function Nav() {\n  return (\n    <nav>\n      <Link to=\"/b-only\">B</Link>\n      <Link to=\"/a-only\">A</Link>\n    </nav>\n  )\n}\n";
        let app = "export function App() {\n  return null\n}\n";
        let fixture = Fixture::new("react-apps")
            .file("apps/a/src/App.tsx", Language::Tsx, app)
            .node(react_route("/a-only", "apps/a/src/App.tsx", 1))
            .file("apps/b/src/App.tsx", Language::Tsx, app)
            .node(react_route("/b-only", "apps/b/src/App.tsx", 1))
            .file("apps/a/src/Nav.tsx", Language::Tsx, nav)
            .node(function("Nav", "apps/a/src/Nav.tsx", Language::Tsx, (1, 8)));
        assert_eq!(
            fixture.links("react-router-link"),
            ["Nav -> /a-only /a-only"]
        );
    }

    /// A TanStack `<Link from>` with no `to` names no destination: the
    /// route it is written from is not where it goes.
    #[test]
    fn a_tanstack_link_from_without_to_names_nothing() {
        let index = "function Index() {\n  return (\n    <div>\n      <Link from=\"/posts\" search={{ page: 2 }}>Page 2</Link>\n      <Link to=\"/about\">About</Link>\n    </div>\n  )\n}\n";
        let route = |path: &str, file: &str| {
            node(
                &format!("route:{file}:1:{path}:tanstack"),
                NodeKind::Route,
                path,
                file,
                Language::Tsx,
                (1, 1),
            )
        };
        let fixture = Fixture::new("tanstack-from")
            .file(
                "src/routes/posts.tsx",
                Language::Tsx,
                "export const Route = createFileRoute('/posts')({})\n",
            )
            .node(route("/posts", "src/routes/posts.tsx"))
            .file(
                "src/routes/about.tsx",
                Language::Tsx,
                "export const Route = createFileRoute('/about')({})\n",
            )
            .node(route("/about", "src/routes/about.tsx"))
            .file("src/routes/index.tsx", Language::Tsx, index)
            .node(function(
                "Index",
                "src/routes/index.tsx",
                Language::Tsx,
                (1, 8),
            ));
        assert_eq!(fixture.links("tanstack-link"), ["Index -> /about /about"]);
    }

    /// A TanStack `to={…}` holding a string names its route, a `<Navigate
    /// to>` navigates with its own method, and a commented-out link names
    /// nothing.
    #[test]
    fn a_tanstack_link_reads_an_expression_but_not_a_comment() {
        let route = |path: &str, file: &str| {
            node(
                &format!("route:{file}:1:{path}:tanstack"),
                NodeKind::Route,
                path,
                file,
                Language::Tsx,
                (1, 1),
            )
        };
        let index = "function Index() {\n  return (\n    <div>\n      {/* <Link to=\"/old\">Old</Link> */}\n      <Link to={'/posts'}>Posts</Link>\n    </div>\n  )\n}\nfunction Guard() {\n  return <Navigate to=\"/login\" />\n}\n";
        let fixture = Fixture::new("tanstack-expressions")
            .file(
                "src/routes/posts.tsx",
                Language::Tsx,
                "export const Route = createFileRoute('/posts')({})\n",
            )
            .node(route("/posts", "src/routes/posts.tsx"))
            .file(
                "src/routes/old.tsx",
                Language::Tsx,
                "export const Route = createFileRoute('/old')({})\n",
            )
            .node(route("/old", "src/routes/old.tsx"))
            .file(
                "src/routes/login.tsx",
                Language::Tsx,
                "export const Route = createFileRoute('/login')({})\n",
            )
            .node(route("/login", "src/routes/login.tsx"))
            .file("src/routes/index.tsx", Language::Tsx, index)
            .node(function(
                "Index",
                "src/routes/index.tsx",
                Language::Tsx,
                (1, 8),
            ))
            .node(function(
                "Guard",
                "src/routes/index.tsx",
                Language::Tsx,
                (9, 11),
            ));
        assert_eq!(
            fixture.links("tanstack-link"),
            ["Guard -> /login /login", "Index -> /posts /posts"]
        );
        let mut methods: Vec<(String, String)> = fixture
            .edges("tanstack-link")
            .into_iter()
            .map(|edge| {
                let metadata = edge.metadata.unwrap();
                (
                    metadata["href"].as_str().unwrap_or("").to_string(),
                    metadata["navMethod"].as_str().unwrap_or("").to_string(),
                )
            })
            .collect();
        methods.sort();
        assert_eq!(
            methods,
            [
                ("/login".to_string(), "navigate".to_string()),
                ("/posts".to_string(), "link".to_string()),
            ]
        );
    }

    /// An Expo screen file and its route node.
    fn with_screens(mut fixture: Fixture, screens: &[(&str, &str)]) -> Fixture {
        for (path, file) in screens {
            fixture = fixture
                .file(file, Language::Tsx, "export default function Screen() {}\n")
                .node(file_route(path, file, Language::Tsx));
        }
        fixture
    }

    /// A helper is the one function of its name in the push site's file,
    /// else the one in the project: two in the file, or none there and two
    /// in the project, is none.
    #[test]
    fn an_expo_helper_two_functions_could_be_is_none() {
        let pick = "function pick() {\n  return '/welcome'\n}\nfunction pick() {\n  return '/home'\n}\nexport async function go() {\n  router.push(await pick())\n}\n";
        let shared = "export function shared() {\n  return '/home'\n}\n";
        let fixture = with_screens(
            Fixture::new("expo-helpers"),
            &[("/welcome", "app/welcome.tsx"), ("/home", "app/home.tsx")],
        )
        .file("src/a.ts", Language::TypeScript, pick)
        .node(function("pick", "src/a.ts", Language::TypeScript, (1, 3)))
        .node(function("pick", "src/a.ts", Language::TypeScript, (4, 6)))
        .node(function("go", "src/a.ts", Language::TypeScript, (7, 9)))
        .file(
            "src/b.ts",
            Language::TypeScript,
            "export async function goB() {\n  router.push(await shared())\n}\n",
        )
        .node(function("goB", "src/b.ts", Language::TypeScript, (1, 3)))
        .file("src/c.ts", Language::TypeScript, shared)
        .node(function("shared", "src/c.ts", Language::TypeScript, (1, 3)))
        .file("src/d.ts", Language::TypeScript, shared)
        .node(function("shared", "src/d.ts", Language::TypeScript, (1, 3)))
        .file(
            "src/e.ts",
            Language::TypeScript,
            "export async function goE() {\n  router.replace(await only())\n}\n",
        )
        .node(function("goE", "src/e.ts", Language::TypeScript, (1, 3)))
        .file(
            "src/f.ts",
            Language::TypeScript,
            "export function only() {\n  return '/home'\n}\n",
        )
        .node(function("only", "src/f.ts", Language::TypeScript, (1, 3)));
        assert_eq!(fixture.links("expo-router-return"), ["only -> /home /home"]);
    }

    /// A screen's column counts the helper's line as written, in UTF-16
    /// code units as upstream's: a commented `é` and a string's `ä` before
    /// the literal each count once.
    #[test]
    fn an_expo_screen_column_counts_the_line_as_written() {
        let fixture = with_screens(
            Fixture::new("expo-column"),
            &[("/welcome", "app/welcome.tsx"), ("/home", "app/home.tsx")],
        )
        .file(
            "src/route.ts",
            Language::TypeScript,
            "export function pick(seen) {\n  return /* \u{e9} */ seen ? '/welcome' : '\u{e4}' && '/home'\n}\nexport async function go(seen) {\n  router.push(await pick(seen))\n}\n",
        )
        .node(function("pick", "src/route.ts", Language::TypeScript, (1, 3)))
        .node(function("go", "src/route.ts", Language::TypeScript, (4, 6)));
        let mut columns: Vec<(String, Option<i64>, Option<i64>)> = fixture
            .edges("expo-router-return")
            .into_iter()
            .map(|edge| {
                (
                    edge.metadata.as_ref().unwrap()["href"]
                        .as_str()
                        .unwrap_or("")
                        .to_string(),
                    edge.line,
                    edge.col,
                )
            })
            .collect();
        columns.sort();
        assert_eq!(
            columns,
            [
                ("/home".to_string(), Some(2), Some(44)),
                ("/welcome".to_string(), Some(2), Some(24)),
            ]
        );
    }

    /// A commented-out navigation reads no helper, and a literal commented
    /// out in a helper's body names no screen.
    #[test]
    fn a_commented_out_push_or_literal_reads_nothing() {
        let fixture = with_screens(
            Fixture::new("expo-comments"),
            &[("/home", "app/home.tsx"), ("/old", "app/old.tsx")],
        )
        .file(
            "src/route.ts",
            Language::TypeScript,
            "export function pick() {\n  // return '/old'\n  return '/home'\n}\nexport function stale() {\n  return '/old'\n}\nexport async function go() {\n  // router.push(await stale())\n  router.push(await pick())\n}\n",
        )
        .node(function("pick", "src/route.ts", Language::TypeScript, (1, 4)))
        .node(function("stale", "src/route.ts", Language::TypeScript, (5, 7)))
        .node(function("go", "src/route.ts", Language::TypeScript, (8, 11)));
        assert_eq!(fixture.links("expo-router-return"), ["pick -> /home /home"]);
    }

    /// A helper two apps' navigations consume names, for each, the screen
    /// of that app: its screens are read once for each pair of helper and
    /// app.
    #[test]
    fn an_expo_helper_consumed_from_two_apps_names_each_apps_screen() {
        let fixture = with_screens(
            Fixture::new("expo-two-apps"),
            &[
                ("/welcome", "apps/m1/app/welcome.tsx"),
                ("/home", "apps/m2/app/home.tsx"),
            ],
        )
        .file(
            "packages/shared/route.ts",
            Language::TypeScript,
            "export function pick(seen) {\n  return seen ? '/welcome' : '/home'\n}\n",
        )
        .node(function(
            "pick",
            "packages/shared/route.ts",
            Language::TypeScript,
            (1, 3),
        ))
        .file(
            "apps/m1/src/a.ts",
            Language::TypeScript,
            "export async function finish(seen) {\n  router.replace(pick(seen))\n}\n",
        )
        .node(function(
            "finish",
            "apps/m1/src/a.ts",
            Language::TypeScript,
            (1, 3),
        ))
        .file(
            "apps/m2/src/b.ts",
            Language::TypeScript,
            "export async function enter(seen) {\n  router.push(pick(seen))\n}\n",
        )
        .node(function(
            "enter",
            "apps/m2/src/b.ts",
            Language::TypeScript,
            (1, 3),
        ));
        assert_eq!(
            fixture.links("expo-router-return"),
            ["pick -> /home /home", "pick -> /welcome /welcome"]
        );
    }

    /// A helper naming more than eight screens is a table, not a decision:
    /// none of them is drawn; eight are.
    #[test]
    fn an_expo_helper_naming_more_than_eight_screens_is_none() {
        let paths: Vec<String> = (1..=9).map(|i| format!("/s{i}")).collect();
        let files: Vec<String> = (1..=9).map(|i| format!("app/s{i}.tsx")).collect();
        let screens: Vec<(&str, &str)> = paths
            .iter()
            .zip(&files)
            .map(|(path, file)| (path.as_str(), file.as_str()))
            .collect();
        let body = |n: usize| {
            let arms: Vec<String> = paths[..n]
                .iter()
                .enumerate()
                .map(|(i, path)| format!("  if (k === {i}) return '{path}'"))
                .collect();
            arms.join("\n")
        };
        let helpers = format!(
            "export function many(k) {{\n{}\n}}\nexport function eight(k) {{\n{}\n}}\n",
            body(9),
            body(8)
        );
        let fixture = with_screens(Fixture::new("expo-cap"), &screens)
            .file("src/helpers.ts", Language::TypeScript, &helpers)
            .node(function("many", "src/helpers.ts", Language::TypeScript, (1, 11)))
            .node(function("eight", "src/helpers.ts", Language::TypeScript, (12, 21)))
            .file(
                "src/go.ts",
                Language::TypeScript,
                "export async function go(k) {\n  router.push(many(k))\n  router.push(eight(k))\n}\n",
            )
            .node(function("go", "src/go.ts", Language::TypeScript, (1, 4)));
        let expected: Vec<String> = paths[..8]
            .iter()
            .map(|path| format!("eight -> {path} {path}"))
            .collect();
        assert_eq!(fixture.links("expo-router-return"), expected);
    }

    /// A helper's literals are matched against the screens of the push
    /// site's own Expo app, and a push site under no Expo app reads none
    /// (KEEP-RUST, W7-08).
    #[test]
    fn an_expo_helper_names_only_its_push_sites_apps_screens() {
        let fixture = with_screens(
            Fixture::new("expo-apps"),
            &[
                ("/welcome", "apps/m1/app/welcome.tsx"),
                ("/home", "apps/m2/app/home.tsx"),
            ],
        )
        .file(
            "packages/shared/route.ts",
            Language::TypeScript,
            "export function pick(seen) {\n  return seen ? '/welcome' : '/home'\n}\nexport function other(seen) {\n  return seen ? '/welcome' : '/home'\n}\n",
        )
        .node(function("pick", "packages/shared/route.ts", Language::TypeScript, (1, 3)))
        .node(function("other", "packages/shared/route.ts", Language::TypeScript, (4, 6)))
        .file(
            "apps/m1/src/login.ts",
            Language::TypeScript,
            "export async function finish(seen) {\n  router.replace(pick(seen))\n}\n",
        )
        .node(function("finish", "apps/m1/src/login.ts", Language::TypeScript, (1, 3)))
        .file(
            "packages/web/nav.ts",
            Language::TypeScript,
            "export async function leave(seen) {\n  router.push(other(seen))\n}\n",
        )
        .node(function("leave", "packages/web/nav.ts", Language::TypeScript, (1, 3)));
        assert_eq!(
            fixture.links("expo-router-return"),
            ["pick -> /welcome /welcome"]
        );
    }
}
