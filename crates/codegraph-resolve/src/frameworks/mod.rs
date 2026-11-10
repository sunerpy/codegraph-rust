//! Concrete [`FrameworkResolver`] implementations + the detection registry.
//!
//! Ports the subset of `upstream resolution/frameworks/` that this port
//! ships: Laravel ([`laravel`]), Drupal ([`drupal`]), Express
//! ([`express`]), React ([`react`]), React Router
//! ([`react_router`]), TanStack Router ([`tanstack_router`]), Next.js
//! ([`nextjs`]), Svelte and SvelteKit ([`svelte`], [`sveltekit_router`]),
//! Vue Router ([`vue_router`]), Angular Router ([`angular_router`]), Astro
//! ([`astro`]), Django, Flask and FastAPI ([`python`]), Rails ([`rails`]),
//! Spring ([`spring`]), Play ([`play`]), Expo Router ([`expo_router`]), Vue
//! and Nuxt ([`vue`]), NestJS ([`nestjs`]), Godot ([`godot`]), Tauri
//! ([`tauri`]), GoFrame ([`goframe`]), Rust's axum and actix routes and
//! Cargo workspace crates ([`rust`], [`cargo_workspace`]), ASP.NET
//! ([`aspnet`]) and SwiftUI, UIKit and Vapor ([`swift`]). The other
//! upstream resolvers remain deferred (see `KNOWN_DIFFS.md`).
//!
//! [`detect_frameworks`] ports `frameworks/index.ts:detectFrameworks` restricted
//! to the ported resolvers; the orchestrator hands the detected list to
//! [`crate::resolver::ReferenceResolver`] so they participate in
//! [`crate::resolver::ReferenceResolver::resolve_one`] Strategy-1 exactly where
//! the upstream runs them (`index.ts:695-701`).

pub mod angular_router;
pub mod aspnet;
pub mod astro;
pub mod cargo_workspace;
pub mod drupal;
pub mod expo_router;
pub mod express;
pub mod godot;
pub(crate) mod godot_common;
pub mod godot_dsl_config;
pub(crate) mod godot_project;
pub(crate) mod godot_resource;
pub(crate) mod godot_scene;
pub mod godot_script;
pub mod goframe;
pub(crate) mod js_scan;
pub mod laravel;
pub(crate) mod lazy_loader;
pub(crate) mod name_heuristic;
pub mod nestjs;
pub mod nextjs;
pub(crate) mod offset_text;
pub mod package_deps;
pub(crate) mod page_component;
pub mod play;
pub mod python;
pub mod rails;
pub mod react;
pub mod react_router;
pub(crate) mod react_tables;
pub mod route_href;
pub mod rust;
pub mod spring;
pub mod svelte;
pub mod sveltekit_router;
pub mod swift;
pub mod tanstack_router;
pub mod tauri;
pub mod vue;
pub mod vue_router;

use crate::framework::FrameworkResolver;
use crate::types::ResolutionContext;
use codegraph_core::types::{Language, Node, NodeKind};
use std::time::{SystemTime, UNIX_EPOCH};

/// Detect which of the ported frameworks a project uses
/// (`detectFrameworks`, `frameworks/index.ts:90-98`, restricted to the ported
/// resolvers).
///
/// Strategy 1 asks these in order: the first to return `>= 0.9` wins, and
/// among lower answers the most confident, the earlier on a tie. Each port
/// sits where upstream's `FRAMEWORK_RESOLVERS` puts it relative to its
/// ported neighbours (Laravel and Drupal first, Express before NestJS, Astro
/// between Angular Router
/// and Expo Router, the Python frameworks after Astro, Rails, Spring and Play
/// after them, Rust and ASP.NET after GoFrame, SwiftUI, UIKit and Vapor
/// after ASP.NET); the entries already
/// here keep their places. A port
/// placed ahead of others must not take a reference they resolved before:
/// it answers below `0.9`, or only for references they do not claim.
pub fn detect_frameworks(context: &dyn ResolutionContext) -> Vec<Box<dyn FrameworkResolver>> {
    let candidates: Vec<Box<dyn FrameworkResolver>> = vec![
        // Laravel's routes, mounts and controllers (#2099, #2271), then
        // Drupal's routing files and hooks (#271), first as upstream
        // registers them. They read only PHP references (Drupal YAML's too).
        // Of the resolvers after them only the language-agnostic Godot
        // (GDScript's references), Nuxt (its own routes' `calls`) and Astro
        // (`Astro.*`, `astro:*`), and Spring and Play (Java and Kotlin
        // bindings, Play routes files) read such a reference, and none of
        // them answers one of these, so their place changes no other
        // reference's resolution. A PHP reference both answer (a namespaced
        // resource controller, `C::m`) gets the same result in either of
        // their orders under Strategy 1.
        Box::new(laravel::LaravelResolver),
        Box::new(drupal::DrupalResolver),
        // Express routes and mounts (#1957, #2200, #2243, #2244, #2261),
        // next as upstream registers it. Its three name patterns answer
        // `0.8` or `0.85`, never `>= 0.9`, and only with a declaration of the
        // reference's own file: ahead of the others it decides a reference
        // only on a confidence tie, as upstream's order does.
        Box::new(express::ExpressResolver),
        Box::new(nestjs::NestjsResolver),
        Box::new(react::ReactResolver),
        Box::new(vue::VueResolver),
        Box::new(godot::GodotResolver),
        // Tauri is not upstream's. Strategy 1 must not hand it a reference
        // one of the resolvers above resolved before it came, so it stays
        // after them.
        Box::new(tauri::TauriResolver::default()),
        // Nuxt's file routes, in a Nuxt app only (upstream #2130). Upstream
        // runs it right after Vue; it claims only its own routes' `calls`
        // references to their pages' components, so its place here changes
        // no other reference's resolution.
        Box::new(vue::NuxtResolver),
        // React Router's navigation (F4), before Next.js as upstream's
        // `FRAMEWORK_RESOLVERS` orders the routers: a navigation call is
        // bound by the first router whose routes its href names. It claims
        // only calls that name one of its own routes, which no resolver above
        // resolves.
        Box::new(react_router::ReactRouterResolver),
        // TanStack Router's file and code routes and navigation (F5), after
        // React Router and before Next.js, as upstream orders the routers. It
        // claims only navigation calls that name one of its own routes.
        Box::new(tanstack_router::TanstackRouterResolver),
        // Next.js pages, route handlers and navigation (F3), split out of
        // React. It claims only navigation calls that name one of its own
        // pages, which no resolver above resolves.
        Box::new(nextjs::NextjsResolver),
        // Svelte's and SvelteKit's conventions and a SvelteKit page's route,
        // then SvelteKit's navigation, after Next.js and before Vue Router as
        // upstream orders them. Svelte reads `.svelte` files and the scripts
        // of a Svelte app (runes, `$lib`); SvelteKit claims only `goto` and
        // `redirect` calls that name one of its own pages.
        Box::new(svelte::SvelteResolver),
        Box::new(sveltekit_router::SvelteKitRouterResolver),
        // Vue Router's config routes and navigation, by name or by path
        // (F6), after Next.js and before Expo Router, as upstream orders the
        // routers. It claims navigation calls only where they name one of
        // its own routes or a Nuxt page, and its routes' own references.
        Box::new(vue_router::VueRouterResolver),
        // Angular's `Routes` arrays, the components they render and the
        // router's navigation, after Vue Router and before Expo Router, as
        // upstream orders the routers. It claims its own routes' lazy and
        // layout references, and `router.navigate`-style calls only where
        // they name one of its own routes.
        Box::new(angular_router::AngularRouterResolver),
        // Astro's file routes, its `Astro` global and `astro:*` modules, and
        // components named in Astro markup (#2245, #2267), after Angular
        // Router and before Expo Router as upstream orders them. Its `1.0`
        // answers claim only names that are Astro's own (`Astro.props`,
        // `astro:content`), and its page routes' `calls` land on their own
        // file's component.
        Box::new(astro::AstroResolver),
        // Django, Flask and FastAPI routes, and their name heuristics
        // (#2154, #2225), after Astro as upstream registers them. They read
        // only Python references, which no resolver above resolves, so their
        // place changes no other reference's resolution.
        Box::new(python::DjangoResolver),
        Box::new(python::FlaskResolver),
        Box::new(python::FastapiResolver),
        // Rails' routes and conventions (#2155, #2269), after the Python
        // frameworks and before Spring as upstream registers them. It reads
        // only Ruby references, which no resolver above resolves (Godot reads
        // GDScript's, Astro answers only its own names), so its place
        // changes no other reference's resolution.
        Box::new(rails::RailsResolver),
        // Spring's routes, config keys and conventions, then Play's routes
        // (#2222, #2156), after the Python frameworks as upstream registers
        // them. Of the resolvers above only the language-agnostic Godot
        // (GDScript's references) and Astro (`Astro.*`, `astro:*`) see a
        // Java, Kotlin or Scala reference, and neither answers one of
        // those, so their place changes no other reference's resolution.
        Box::new(spring::SpringResolver),
        Box::new(play::PlayResolver),
        // Expo Router screens and navigation (F2), after Next.js as upstream
        // registers it. It claims navigation calls only where their href
        // names one of its own screens.
        Box::new(expo_router::ExpoRouterResolver),
        // GoFrame's `g.Meta` routes (#747), upstream's after its Go
        // resolver. It resolves no reference, so its place here changes
        // no resolution; the synthesis stage joins its routes.
        Box::new(goframe::GoFrameResolver),
        // Rust's axum and actix routes, its conventions and the Cargo
        // workspace's crates (#2359, #2224, #2214, #151), after GoFrame as
        // upstream registers it. It reads only Rust references, and no
        // resolver above answers one (Godot reads GDScript's, Astro only its
        // own names; Tauri reads Rust's `#[tauri::command]`s but resolves
        // JavaScript's `invoke` calls), so its place changes no other
        // reference's resolution.
        Box::new(rust::RustResolver),
        // ASP.NET's routes and conventions (#2158, #2415), after GoFrame and
        // Rust as upstream registers it; this port's Expo Router already
        // stands before GoFrame. It reads only C# and Razor references, and
        // no resolver above answers one (Godot reads GDScript's, Astro only
        // its own names), so its place changes no other reference's
        // resolution.
        Box::new(aspnet::AspnetResolver),
        // SwiftUI's and UIKit's conventions, then Vapor's routes and
        // conventions (#2106, #2157, #2273, #2242), after ASP.NET as upstream
        // registers them. They read only Swift references, and no resolver
        // above answers one (Godot reads GDScript's, Astro only its own
        // names), so their place changes no other reference's resolution;
        // between them a tie (UIKit's and Vapor's `…ViewController` at 0.85)
        // goes to the earlier, as upstream's.
        Box::new(swift::SwiftUiResolver),
        Box::new(swift::UikitResolver),
        Box::new(swift::VaporResolver),
    ];
    candidates
        .into_iter()
        .filter(|resolver| resolver.detect(context))
        .collect()
}

/// `Language` whose comment syntax / file family a JS/TS-ish path belongs to,
/// used by the route-extracting resolvers when building node `language` fields.
/// Mirrors the upstream `detectLanguage` ternaries that pick `tsx`/`jsx`/
/// `typescript`/`javascript` by extension.
pub(crate) fn js_language_for(file_path: &str) -> Language {
    if file_path.ends_with(".tsx") {
        Language::Tsx
    } else if file_path.ends_with(".jsx") {
        Language::Jsx
    } else if file_path.ends_with(".ts")
        || file_path.ends_with(".mts")
        || file_path.ends_with(".cts")
    {
        Language::TypeScript
    } else {
        Language::JavaScript
    }
}

/// The one item `items` holds, or `None` when it holds none or several: a
/// framework heuristic's tier resolves only through its sole candidate, and
/// an ambiguous tier proposes nothing, leaving the reference to the other
/// strategies.
pub(crate) fn sole<T>(items: impl IntoIterator<Item = T>) -> Option<T> {
    let mut items = items.into_iter();
    let first = items.next()?;
    items.next().is_none().then_some(first)
}

/// Current epoch millis for a framework node's `updatedAt` (`Date.now()`).
pub(crate) fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

/// Build a framework node carrying the upstream literal (non-hashed) id. The upstream's
/// framework extractors set a fixed id string directly rather than the sha256
/// node-id formula, so the port reproduces that literal id verbatim.
#[allow(clippy::too_many_arguments)]
pub(crate) fn framework_node(
    id: String,
    kind: NodeKind,
    name: String,
    qualified_name: String,
    file_path: String,
    start_line: i64,
    end_line: i64,
    start_column: i64,
    end_column: i64,
    language: Language,
    is_exported: bool,
) -> Node {
    Node {
        id,
        kind,
        name,
        qualified_name,
        file_path,
        language,
        start_line,
        end_line,
        start_column,
        end_column,
        docstring: None,
        signature: None,
        visibility: None,
        is_exported,
        is_async: false,
        is_static: false,
        is_abstract: false,
        decorators: Vec::new(),
        type_parameters: Vec::new(),
        return_type: None,
        updated_at: now_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_language_for_all_branches() {
        assert_eq!(js_language_for("a.tsx"), Language::Tsx);
        assert_eq!(js_language_for("a.jsx"), Language::Jsx);
        assert_eq!(js_language_for("a.ts"), Language::TypeScript);
        assert_eq!(js_language_for("a.mts"), Language::TypeScript);
        assert_eq!(js_language_for("a.cts"), Language::TypeScript);
        assert_eq!(js_language_for("a.js"), Language::JavaScript);
        assert_eq!(js_language_for("noext"), Language::JavaScript);
    }

    #[test]
    fn now_millis_is_positive() {
        assert!(now_millis() > 0);
    }

    #[test]
    fn framework_node_populates_fields() {
        let n = framework_node(
            "route:x".to_string(),
            NodeKind::Route,
            "/x".to_string(),
            "f::route:/x".to_string(),
            "f.tsx".to_string(),
            3,
            4,
            0,
            9,
            Language::Tsx,
            true,
        );
        assert_eq!(n.id, "route:x");
        assert_eq!(n.kind, NodeKind::Route);
        assert_eq!(n.name, "/x");
        assert_eq!(n.start_line, 3);
        assert_eq!(n.end_line, 4);
        assert_eq!(n.end_column, 9);
        assert!(n.is_exported);
        assert!(!n.is_async);
    }
}
