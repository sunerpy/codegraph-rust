//! React [`FrameworkResolver`] — ports
//! `upstream resolution/frameworks/react.ts`. React Router's route scanner and
//! navigation are [`super::react_router`]'s; Next.js pages, route handlers and
//! navigation are [`super::nextjs`]'s.

use std::sync::{Arc, OnceLock};

use codegraph_core::types::{Language, Node, NodeKind};
use regex::Regex;

use super::js_language_for;
use super::js_scan::{self, JS_SPACE};
use super::lazy_loader::{self, LoadedPage};
use super::react_router::{self, LAYOUT_PREFIX, LAZY_ROUTE_PREFIX};
use crate::alias_binding::{default_export_binding, extract_local_export_aliases};
use crate::framework::FrameworkResolver;
use crate::import_resolver::{resolve_import_path, resolve_via_import};
use crate::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::types::{
    EdgeShape, FrameworkResolverExtractionResult, ReExport, RefView, ResolutionContext, ResolvedBy,
    ResolvedRef,
};

/// React resolver (`reactResolver`, react.ts:10-269).
pub struct ReactResolver;

impl FrameworkResolver for ReactResolver {
    fn name(&self) -> &str {
        "react"
    }

    /// The languages React components, hooks and contexts are written and
    /// used in. The JSX dialects are here so route extraction runs on the
    /// `.tsx`/`.jsx` files a `<Route element={<X/>}>` lives in (upstream
    /// #841), and `resolve` reads only a script's references, which its
    /// `resolve_languages` default to (#2211): halo's Java `import
    /// org.springframework…SecurityContext` is no React context.
    fn languages(&self) -> Option<&[Language]> {
        const LANGS: [Language; 4] = [
            Language::JavaScript,
            Language::TypeScript,
            Language::Tsx,
            Language::Jsx,
        ];
        Some(&LANGS)
    }

    // Ports reactResolver.detect (react.ts:14-32).
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        // React in a package.json: the root's, or an app's (`frontend/`,
        // `apps/web/`, `src/Web/ClientApp/`).
        if super::package_deps::depends_on(context, &["react", "next", "react-native"]) {
            return true;
        }
        context
            .get_all_files_shared()
            .iter()
            .any(|f| f.ends_with(".jsx") || f.ends_with(".tsx"))
    }

    /// A data-router `lazy: () => import('./routes/x')` route names a module,
    /// not a symbol, and `import:./x#Page` the export its loader picks; a
    /// route's `layout:MainLayout` names the component around it.
    fn claims_reference(&self, name: &str) -> bool {
        name.starts_with(LAZY_ROUTE_PREFIX)
            || name.starts_with(LAYOUT_PREFIX)
            || lazy_loader::picked_export_name(name).is_some()
    }

    // Ports reactResolver.resolve (react.ts:44-131).
    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let name = &reference.reference_name;
        if let Some(spec) = name.strip_prefix(LAZY_ROUTE_PREFIX) {
            return lazy_route_component(spec, &reference.file_path, context)
                .map(|target| framework_resolved(reference, target, 0.9));
        }
        // The export a lazy route's loader picks. Vue Router and Angular name
        // a lazily loaded component the same way, and their routes' are
        // theirs.
        if let Some((spec, export)) = lazy_loader::picked_export_name(name) {
            return is_react_route_ref(reference)
                .then(|| picked_export(spec, export, &reference.file_path, context))
                .flatten()
                .map(|target| framework_resolved(reference, target, 0.9));
        }
        // The layout a route renders inside: what happens in it — its
        // header's links, its logout — happens on the route's screen too.
        if let Some(spec) = name.strip_prefix(LAYOUT_PREFIX) {
            return layout_component(spec, reference, context).map(|target| {
                let mut metadata = serde_json::Map::new();
                metadata.insert("layout".to_string(), serde_json::Value::Bool(true));
                framework_resolved(reference, target, 0.9).with_edge(EdgeShape {
                    edge_kind: None,
                    metadata,
                    also_targets: Vec::new(),
                })
            });
        }
        // Pattern 1 reads a component USED in markup, which only parses in
        // .tsx/.jsx. Without this gate every PascalCase TYPE in a plain .ts
        // file went through component resolution: in a monorepo with
        // same-named classes per package (#764, amplication), a GraphQL-types
        // file's own `Account` type lost to an arbitrary `Account` class in
        // another package.
        let component = matches!(reference.language, Language::Tsx | Language::Jsx)
            && is_pascal_case(name)
            && !is_built_in_type(name);
        let hook = name.starts_with("use") && name.len() > 3;
        let provided = name.ends_with("Context") || name.ends_with("Provider");
        if !(component || hook || provided) {
            return None;
        }

        // A component, hook or context the file IMPORTS is the import's: the
        // package's (`useQuery` from `@tanstack/react-query`, `<Button>` from
        // a UI kit, upstream #2120 and #2475), or the module the import
        // names, which import resolution finds. Framework resolution runs
        // first, and a name lookup here bound trpc's tests' `useQuery` to a
        // hook nested in one of trpc's own factories.
        if context
            .get_import_mappings(&reference.file_path, reference.language)
            .iter()
            .any(|mapping| mapping.local_name == *name)
        {
            return None;
        }

        // Pattern 1: component references (PascalCase).
        if component {
            // A name the file declares itself as a value is that declaration,
            // whatever refers to it (upstream #2420): the function the value
            // wraps, or else the declaration, which name matching binds —
            // never a same-named component in another app of the repository.
            if let Some(own) = declared_component(name, &reference.file_path, context) {
                return own
                    .component
                    .map(|component| framework_resolved(reference, component, 0.9));
            }
            if let Some(target) = resolve_component(name, &reference.file_path, context) {
                return Some(framework_resolved(reference, target, 0.8));
            }
        }

        // Pattern 2: hook references (`use*`).
        if hook {
            if let Some(target) =
                resolve_hook(name, &reference.file_path, context, reference.language)
            {
                return Some(framework_resolved(reference, target, 0.85));
            }
        }

        // Pattern 3: context references (`*Context` / `*Provider`).
        if provided {
            if let Some(target) = resolve_context(name, reference, context) {
                return Some(framework_resolved(reference, target, 0.8));
            }
        }

        None
    }

    // Ports reactResolver.extract (react.ts:133-188).
    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &crate::framework::FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        // Components and custom hooks are NOT extracted here (upstream #841).
        // The tree-sitter extractor emits them across .ts/.tsx/.js/.jsx:
        // function and arrow components as `function` nodes, components a
        // wrapper declares (`forwardRef`/`memo`/`styled`) as `component`
        // nodes, and `useX` hooks as `function` nodes. The regex this pass
        // carried duplicated those nodes (a `useAuth` became two `function`
        // nodes); it contributes only what tree-sitter can't: React Router's
        // routes. Next.js pages and route handlers are `super::nextjs`'s.
        Some(react_router::extract_routes(file_path, content))
    }

    /// Name the routes whose path is built from a constant (React's
    /// `postExtract`).
    fn post_extract(&self, context: &dyn ResolutionContext) -> Option<Vec<Node>> {
        Some(react_router::rename_constant_routes(context))
    }
}

fn framework_resolved(reference: &RefView, target_node_id: String, confidence: f64) -> ResolvedRef {
    ResolvedRef::new(
        reference.clone(),
        target_node_id,
        confidence,
        ResolvedBy::Framework,
    )
}

/// `isPascalCase` (react.ts:274-276).
fn is_pascal_case(s: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Z][a-zA-Z0-9]*$").expect("pascal"))
        .is_match(s)
}

/// `isBuiltInType` / `BUILT_IN_TYPES` (react.ts:281-289).
fn is_built_in_type(name: &str) -> bool {
    const BUILT_IN_TYPES: [&str; 22] = [
        "Array",
        "Boolean",
        "Date",
        "Error",
        "Function",
        "JSON",
        "Math",
        "Number",
        "Object",
        "Promise",
        "RegExp",
        "String",
        "Symbol",
        "Map",
        "Set",
        "WeakMap",
        "WeakSet",
        "React",
        "Component",
        "Fragment",
        "Suspense",
        "StrictMode",
    ];
    BUILT_IN_TYPES.contains(&name)
}

/// `COMPONENT_KINDS` (react.ts:291).
fn is_component_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Component | NodeKind::Function | NodeKind::Class
    )
}

/// `resolveComponent` (react.ts:296-323).
fn resolve_component(
    name: &str,
    from_file: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let candidates = context.get_nodes_by_name_shared(name);
    if candidates.is_empty() {
        return None;
    }
    let components: Vec<_> = candidates
        .into_iter()
        .filter(|n| is_component_kind(n.kind))
        .collect();
    if components.is_empty() {
        return None;
    }

    let from_dir = dir_of(from_file);
    if let Some(same_dir) = components
        .iter()
        .find(|n| n.file_path.starts_with(&from_dir))
    {
        return Some(same_dir.id.clone());
    }

    const COMPONENT_DIRS: [&str; 7] = [
        "/components/",
        "/src/components/",
        "/app/components/",
        "/pages/",
        "/src/pages/",
        "/views/",
        "/src/views/",
    ];
    if let Some(preferred) = components
        .iter()
        .find(|n| COMPONENT_DIRS.iter().any(|d| n.file_path.contains(d)))
    {
        return Some(preferred.id.clone());
    }

    // Only an unambiguous single may resolve (react.ts:322).
    if components.len() == 1 {
        Some(components[0].id.clone())
    } else {
        None
    }
}

/// A value a file declares under a component's name, and what it renders
/// (`declaredComponent`'s answer).
struct DeclaredComponent {
    declaration: Arc<Node>,
    /// The id of the component of the module the value loads lazily, or of
    /// the function it wraps; `None` when there is none to find and the
    /// declaration is the binding, by name.
    component: Option<String>,
}

/// A value `file_path` declares itself under a component's name, and what it
/// renders (upstream `declaredComponent`, #2420). JavaScript scoping makes
/// the name, used anywhere in that file — `<RegisterPage />`, a route's
/// `element: <RegisterPage />`, a type — this declaration, never another
/// file's same-named component: mantis's vite app bound its `RegisterPage`,
/// a `Loadable(lazy(() => import('pages/auth/Register')))`, to the page of
/// that name in the repository's Next.js app. Its `component` is the
/// component of the module a lazy value loads ([`value_loader`]), or the
/// function the value wraps (`observer(function Login() {…})`). A loader
/// nested past the readers' bound has none. `None` when the file declares no
/// such value (a function or class component is found the usual way).
fn declared_component(
    name: &str,
    file_path: &str,
    context: &dyn ResolutionContext,
) -> Option<DeclaredComponent> {
    let declaration = declared_value(name, file_path, context)?;
    let component = match value_loader(&declaration, context) {
        Ok(None) => wrapped_component(&declaration, context).map(|node| node.id.clone()),
        Ok(Some(LoadedPage { spec, member })) => {
            if member == "default" {
                lazy_route_component(&spec, file_path, context)
            } else {
                picked_export(&spec, &member, file_path, context)
            }
        }
        Err(lazy_loader::Exhausted) => None,
    };
    Some(DeclaredComponent {
        declaration,
        component,
    })
}

/// The module a value `declaration` declares loads lazily, and the export
/// its loader picks (upstream `valueLoader`): `Ok(None)` for a value that
/// loads none.
fn value_loader(
    declaration: &Node,
    context: &dyn ResolutionContext,
) -> Result<Option<LoadedPage>, lazy_loader::Exhausted> {
    let code = strip_comments_for_regex(
        &declaration_text(declaration, context),
        CommentLang::TypeScript,
    );
    match js_scan::initializer_at(&code, declaration.name.len()) {
        Some(init) => lazy_loader::value_loader(&code[init..]),
        None => Ok(None),
    }
}

/// The module-level `const`, `let` or `var` `file_path` declares as `name`
/// (upstream `declaredValue`): a module-level declarator's qualified name is
/// its bare name.
fn declared_value(
    name: &str,
    file_path: &str,
    context: &dyn ResolutionContext,
) -> Option<Arc<Node>> {
    context
        .get_nodes_by_name_shared(name)
        .into_iter()
        .filter(|node| {
            node.file_path == file_path
                && matches!(node.kind, NodeKind::Constant | NodeKind::Variable)
                && node.qualified_name == name
        })
        .min_by_key(|node| (node.start_line, node.start_column))
}

/// The component a value wraps, which is what it renders (upstream
/// `wrappedComponent`): the function it holds (`const Application =
/// observer(function Application() {…})`), or the component of its own file
/// a wrapper call is handed ([`handed_component`]). An inner function counts
/// when it has the value's name or ends where the value does — never a
/// helper inside an arrow the graph has no node for (`withFallback('X', ()
/// => { const load = () => …; … })`).
fn wrapped_component(value: &Node, context: &dyn ResolutionContext) -> Option<Arc<Node>> {
    let nodes = context.get_nodes_in_file_shared(&value.file_path);
    let inner: Vec<&Arc<Node>> = nodes
        .iter()
        .filter(|node| {
            is_component_kind(node.kind)
                && node
                    .qualified_name
                    .strip_prefix(value.qualified_name.as_str())
                    .and_then(|rest| rest.strip_prefix("::"))
                    == Some(node.name.as_str())
                && node.start_line >= value.start_line
                && node.end_line <= value.end_line
        })
        .collect();
    if !inner.is_empty() {
        let held = inner
            .iter()
            .find(|node| node.name == value.name)
            .or_else(|| {
                (inner.len() == 1 && inner[0].end_line == value.end_line).then(|| &inner[0])
            });
        return held.map(|node| Arc::clone(node));
    }
    handed_component(value, &nodes, context)
}

/// The component of its own file a wrapper call around a declaration hands
/// over (upstream `handedComponent`): `const TableView =
/// observer(TableViewInner)`, `forwardRef(Inner) as {…}`, or mastodon's
/// `const MemoizedBlurhash = memo(Blurhash)`, whose component node has no
/// body of its own to render from. `nodes` are the file's.
fn handed_component(
    value: &Node,
    nodes: &[Arc<Node>],
    context: &dyn ResolutionContext,
) -> Option<Arc<Node>> {
    let code = strip_comments_for_regex(&declaration_text(value, context), CommentLang::TypeScript);
    let init = js_scan::initializer_at(&code, value.name.len())?;
    let handed = js_scan::wrapped_name(&code[init..js_scan::statement_end(&code, init, false)])?;
    if !is_pascal_case(handed) || handed == value.name {
        return None;
    }
    nodes
        .iter()
        .find(|node| {
            node.name == handed && node.qualified_name == handed && is_component_kind(node.kind)
        })
        .cloned()
}

/// The binding a value holds and nothing more (upstream `heldBinding`):
/// `AvatarWithHoverCard` of outline's `const Avatar = AvatarWithHoverCard`,
/// past a type assertion (`= Page as FC`); `None` for any other initializer.
fn held_binding(value: &Node, context: &dyn ResolutionContext) -> Option<String> {
    static HELD: OnceLock<Regex> = OnceLock::new();
    let code = strip_comments_for_regex(&declaration_text(value, context), CommentLang::TypeScript);
    let init = js_scan::initializer_at(&code, value.name.len())?;
    let held = HELD
        .get_or_init(|| {
            Regex::new(&format!(
                "^{JS_SPACE}*({IDENT}){JS_SPACE}*(?:$|(?:as|satisfies){JS_SPACE})"
            ))
            .expect("held binding")
        })
        .captures(&code[init..js_scan::statement_end(&code, init, false)])?;
    let held = held.get(1)?.as_str();
    (held != value.name).then(|| held.to_string())
}

/// The identifier pattern upstream writes `[A-Za-z_$][\w$]*`.
const IDENT: &str = "[A-Za-z_$][A-Za-z0-9_$]*";

/// What a module can export as a component: a declaration, or a value
/// holding one (`EXPORTED_COMPONENT_KINDS`).
fn is_exported_component_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function
            | NodeKind::Component
            | NodeKind::Class
            | NodeKind::Constant
            | NodeKind::Variable
    )
}

/// A barrel can forward another barrel; a few hops settle it
/// (`MAX_REEXPORT_HOPS`).
const MAX_REEXPORT_HOPS: usize = 4;

/// The file a lazily loaded module is, from the file that loads it
/// (`lazyModuleFile`): by the extensions that file's own imports resolve with
/// (`./MainLayout` is `MainLayout/index.jsx` from a `.jsx` file), else by
/// TypeScript's.
fn lazy_module_file(
    spec: &str,
    from_file: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let language = js_language_for(from_file);
    resolve_import_path(spec, from_file, language, context).or_else(|| {
        (language != Language::TypeScript)
            .then(|| resolve_import_path(spec, from_file, Language::TypeScript, context))
            .flatten()
    })
}

/// The component a lazy route module renders: its default export, else its
/// `Component` export (`lazyRouteComponent`).
fn lazy_route_component(
    spec: &str,
    from_file: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let file = lazy_module_file(spec, from_file, context)?;
    exported_component(&file, "default", context, &[])
        .or_else(|| exported_component(&file, "Component", context, &[]))
}

/// The component a lazy route's loader picks from the module `spec` loads:
/// the export named `name`, through barrels (`pickedExport`).
fn picked_export(
    spec: &str,
    name: &str,
    from_file: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let file = lazy_module_file(spec, from_file, context)?;
    exported_component(&file, name, context, &[])
}

/// True for a reference a React route makes (`isReactRouteRef`): from a
/// `route:<file>:…` node, in tsx or jsx, the language React gives its routes
/// even in a `.ts` routes file. Vue Router's and Angular's routes are never
/// in either.
fn is_react_route_ref(reference: &RefView) -> bool {
    matches!(reference.language, Language::Tsx | Language::Jsx)
        && reference
            .from_node_id
            .strip_prefix("route:")
            .and_then(|rest| rest.strip_prefix(reference.file_path.as_str()))
            .is_some_and(|rest| rest.starts_with(':'))
}

/// The component `file` exports as `name` (upstream `exportedComponent`):
/// one it declares — `export default Page`, `export function Component`,
/// `export { Page as Component }` — or one it forwards from the module that
/// declares it, as a barrel `index.ts` does: `export { default } from
/// './page'`, `export { Page as Component } from './page'`, `export * from
/// './page'`, which forwards every name but the default, or an import it
/// exports again, `import { Page } from './page'; export { Page }`, or in a
/// value, outline's `const Avatar = AvatarWithHoverCard; export { Avatar }`.
/// As in JavaScript, a name the file exports itself hides one an `export *`
/// would forward, and a name two `export *` modules both forward is exported
/// by neither. `visited` are the files the lookup came through.
fn exported_component(
    file: &str,
    name: &str,
    context: &dyn ResolutionContext,
    visited: &[&str],
) -> Option<String> {
    if visited.contains(&file) || visited.len() > MAX_REEXPORT_HOPS {
        return None;
    }
    let language = js_language_for(file);
    let mut through = visited.to_vec();
    through.push(file);
    let forwarded = |source: &str, exported: &str| -> Option<String> {
        let target = resolve_import_path(source, file, language, context)?;
        exported_component(&target, exported, context, &through)
    };
    let content = context.read_file_shared(file);
    let code = strip_comments_for_regex(content.as_deref().unwrap_or(""), CommentLang::TypeScript);
    if let Some(local) = own_export(&code, name) {
        let nodes = context.get_nodes_in_file_shared(file);
        let declared_as = |binding: &str| -> Option<Arc<Node>> {
            let named: Vec<&Arc<Node>> = nodes
                .iter()
                .filter(|node| node.name == binding && is_exported_component_kind(node.kind))
                .collect();
            named
                .iter()
                .find(|node| node.qualified_name == binding)
                .or_else(|| named.first())
                .map(|node| Arc::clone(node))
        };
        let exported = declared_as(&local);
        // A constant that holds another binding is that binding: outline's
        // `Avatar/index.ts` is `const Avatar = AvatarWithHoverCard; export {
        // Avatar }`. (A `let` may be assigned another component before it is
        // used.)
        let mut binding = local.clone();
        let mut declared = exported.clone();
        for _ in 0..MAX_REEXPORT_HOPS {
            let Some(held) = declared
                .as_deref()
                .filter(|node| node.kind == NodeKind::Constant)
                .and_then(|node| held_binding(node, context))
            else {
                break;
            };
            declared = declared_as(&held);
            binding = held;
        }
        // A value is what it wraps: `export default Page` of `const Page =
        // observer(function Page_() {…})`. So is a component node minted for
        // a wrapper around another component (`const MemoizedBlurhash =
        // memo(Blurhash)`), which has no body to render from.
        if let Some(declared) = declared {
            let wrapped = match declared.kind {
                NodeKind::Constant | NodeKind::Variable => wrapped_component(&declared, context),
                NodeKind::Component => handed_component(&declared, &nodes, context),
                _ => None,
            };
            return Some(wrapped.unwrap_or(declared).id.clone());
        }
        // A name the module imports and exports again — `import { Page }
        // from './page'; export { Page }`, `import Page from './Page'; export
        // default Page` — is the page that import names. `export default
        // withAuth(Page)` hands over no binding of its own.
        if binding == local
            && name == "default"
            && default_export_binding(&code).as_deref() != Some(local.as_str())
        {
            return None;
        }
        let imported = context
            .get_import_mappings(file, language)
            .into_iter()
            .find(|mapping| mapping.local_name == binding);
        let id = imported
            .filter(|mapping| !mapping.is_namespace)
            .and_then(|mapping| {
                let exported = if mapping.is_default {
                    "default"
                } else {
                    mapping.exported_name.as_str()
                };
                forwarded(&mapping.source, exported)
            });
        // A held binding that leads nowhere leaves the value itself the
        // export.
        return id.or_else(|| exported.map(|node| node.id.clone()));
    }
    let re_exports = context.get_re_exports(file, language);
    for re_export in &re_exports {
        if let ReExport::Named {
            exported_name,
            original_name,
            source,
        } = re_export
            && exported_name == name
        {
            return forwarded(source, original_name);
        }
    }
    if name == "default" {
        return None;
    }
    let mut found: Option<String> = None;
    for re_export in &re_exports {
        let ReExport::Wildcard { source } = re_export else {
            continue;
        };
        let id = forwarded(source, name);
        if let (Some(id), Some(found)) = (&id, &found)
            && id != found
        {
            return None;
        }
        if found.is_none() {
            found = id;
        }
    }
    found
}

/// The local name a module's own `export` gives `name` (upstream
/// `ownExport`): its default export ([`default_exported_name`]), `export
/// function Component`, `export const Component`, `export { Page as
/// Component }`. `None` when it has none.
fn own_export(source: &str, name: &str) -> Option<String> {
    if name == "default" {
        if let Some(declared) = default_exported_name(source) {
            return Some(declared);
        }
    } else if let Ok(declaration) = Regex::new(&format!(
        r"(?-u:\b)export{JS_SPACE}+(?:async{JS_SPACE}+)?(?:const|let|var|function|class){JS_SPACE}+{}",
        regex::escape(name)
    )) && declaration.find_iter(source).any(|m| {
        // `(?![\w$])`
        !source
            .as_bytes()
            .get(m.end())
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$'))
    }) {
        return Some(name.to_string());
    }
    extract_local_export_aliases(source)
        .into_iter()
        .find(|alias| alias.exported_name == name)
        .map(|alias| alias.local_name)
}

/// The name a module's default export carries (upstream
/// `defaultExportedName`): `export default Login`, `export default function
/// Login`, `export default class Login`, `export default Login as FC`, or the
/// component a wrapper call hands over — `observer(Login)`,
/// `React.memo(Login)`, `connect(mapState)(Login)`,
/// `withTranslation()(withRouter(Login))`.
fn default_exported_name(source: &str) -> Option<String> {
    static AT: OnceLock<Regex> = OnceLock::new();
    static DECLARED: OnceLock<Regex> = OnceLock::new();
    static BINDING: OnceLock<Regex> = OnceLock::new();
    let at = AT
        .get_or_init(|| {
            Regex::new(&format!(r"(?-u:\b)export{JS_SPACE}+default{JS_SPACE}+"))
                .expect("export default")
        })
        .find(source)?;
    let rest = &source[at.end()..];
    let declared = DECLARED.get_or_init(|| {
        Regex::new(&format!(
            r"^(?:async{JS_SPACE}+)?(?:function{JS_SPACE}*\*?{JS_SPACE}*|class{JS_SPACE}+)({IDENT})"
        ))
        .expect("default declaration")
    });
    let binding = BINDING.get_or_init(|| {
        Regex::new(&format!(
            r"^({IDENT})(?:[ \t]*(?:;|\r?\n|$)|{JS_SPACE}+(?:as|satisfies){JS_SPACE})"
        ))
        .expect("default binding")
    });
    if let Some(named) = declared
        .captures(rest)
        .or_else(|| binding.captures(rest))
        .and_then(|captures| captures.get(1))
    {
        return Some(named.as_str().to_string());
    }
    js_scan::wrapped_name(&rest[..js_scan::statement_end(rest, 0, false)]).map(str::to_string)
}

/// The component a route's `layout:` reference names, found the way the
/// route's own component is (upstream `layoutComponent`): through the route
/// file's import of it, else declared in reach — or a lazily loaded layout
/// module's component, or the export its loader picks.
fn layout_component(
    spec: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    if let Some(module) = spec.strip_prefix(LAZY_ROUTE_PREFIX) {
        return lazy_route_component(module, &reference.file_path, context);
    }
    if is_react_route_ref(reference)
        && let Some((module, export)) = lazy_loader::picked_export_name(spec)
    {
        return picked_export(module, export, &reference.file_path, context);
    }
    let Some(mapping) = context
        .get_import_mappings(&reference.file_path, reference.language)
        .into_iter()
        .find(|mapping| mapping.local_name == spec)
    else {
        // A layout the route file declares itself: no name match follows a
        // `layout:` reference, so the declaration binds here when there is
        // no component to find.
        if let Some(own) = declared_component(spec, &reference.file_path, context) {
            return Some(own.component.unwrap_or_else(|| own.declaration.id.clone()));
        }
        return resolve_component(spec, &reference.file_path, context);
    };
    let via_import = resolve_via_import(
        &RefView {
            reference_name: spec.to_string(),
            ..reference.clone()
        },
        context,
    );
    if let Some(resolved) = via_import {
        return Some(resolved.target_node_id);
    }
    // `import { default as AppRoot } from './routes/app/root'`: read the
    // module the import names — never a same-named component somewhere else.
    if mapping.is_default || mapping.exported_name == "default" {
        return lazy_route_component(&mapping.source, &reference.file_path, context);
    }
    let file = resolve_import_path(
        &mapping.source,
        &reference.file_path,
        reference.language,
        context,
    )?;
    context
        .get_nodes_in_file_shared(&file)
        .iter()
        .find(|node| node.name == mapping.exported_name && is_component_kind(node.kind))
        .map(|node| node.id.clone())
}

/// A declarator's own source text, `B = lazy(…)` in `const A = lazy(…), B =
/// lazy(…)` (upstream `declarationText`): its lines, the last cut at its end
/// column and the first from its start column. Columns are the byte columns
/// the extractor stored; a file that no longer reads that way gives nothing.
fn declaration_text(node: &Node, context: &dyn ResolutionContext) -> String {
    let Some(facts) = context.source_facts(&node.file_path) else {
        return String::new();
    };
    let (Ok(first), Ok(end)) = (
        usize::try_from(node.start_line - 1),
        usize::try_from(node.end_line),
    ) else {
        return String::new();
    };
    let end = end.min(facts.line_count());
    if first >= end {
        return String::new();
    }
    let mut lines: Vec<&str> = (first..end).map(|index| facts.line(index)).collect();
    let last = lines.len() - 1;
    if let Ok(cut @ 1..) = usize::try_from(node.end_column) {
        let line = lines[last];
        let Some(kept) = line.get(..cut.min(line.len())) else {
            return String::new();
        };
        lines[last] = kept;
    }
    let from = usize::try_from(node.start_column).unwrap_or(0);
    let Some(kept) = lines[0].get(from.min(lines[0].len())..) else {
        return String::new();
    };
    lines[0] = kept;
    lines.join("\n")
}

/// `isEsmLanguage` (react.ts:1312-1314): JS/TS and their JSX dialects,
/// modules where a cross-file name needs an import.
fn is_esm_script(language: Language) -> bool {
    matches!(
        language,
        Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx
    )
}

/// `resolveHook` (react.ts:1320-1346).
fn resolve_hook(
    name: &str,
    from_file: &str,
    context: &dyn ResolutionContext,
    language: Language,
) -> Option<String> {
    let candidates = context.get_nodes_by_name_shared(name);
    let is_hook = |node: &Node| node.kind == NodeKind::Function && node.name.starts_with("use");
    // A hook nested inside another function is only callable in there
    // (upstream #2120).
    let nested = |node: &Node| {
        context
            .get_nodes_in_file_shared(&node.file_path)
            .iter()
            .any(|outer| {
                outer.id != node.id
                    && matches!(outer.kind, NodeKind::Function | NodeKind::Method)
                    && outer.start_line <= node.start_line
                    && outer.end_line >= node.end_line
                    && (outer.start_line < node.start_line || outer.end_line > node.end_line)
            })
    };
    if let Some(own) = candidates
        .iter()
        .find(|node| node.file_path == from_file && is_hook(node) && !nested(node))
    {
        return Some(own.id.clone());
    }
    // A JS/TS module reaches another file's hook only by importing it — the
    // import resolver's to follow (an imported name never gets here) —
    // never by name alone (upstream #2240).
    if is_esm_script(language) {
        return None;
    }
    let hooks: Vec<_> = candidates
        .iter()
        .filter(|node| is_hook(node) && !nested(node))
        .collect();
    const HOOK_DIRS: [&str; 4] = ["/hooks/", "/src/hooks/", "/lib/hooks/", "/utils/hooks/"];
    hooks
        .iter()
        .find(|node| HOOK_DIRS.iter().any(|dir| node.file_path.contains(dir)))
        .or_else(|| hooks.first())
        .map(|node| node.id.clone())
}

/// `resolveContext` (react.ts:1351-1377).
fn resolve_context(
    name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    // In a JS/TS module only the file's own context is in reach by name;
    // another file's comes through an import (trpc's adapters'
    // `createContext?.(…)` is an option, not an example app's
    // `createContext`, upstream #2240).
    if is_esm_script(reference.language) {
        return context
            .get_nodes_by_name_shared(name)
            .iter()
            .find(|node| node.file_path == reference.file_path)
            .map(|node| node.id.clone());
    }
    let candidates = context.get_nodes_by_name_shared(name);
    if candidates.is_empty() {
        // Without the Context/Provider suffix.
        let base_name = strip_context_suffix(name);
        if base_name != name {
            let base_candidates = context.get_nodes_by_name_shared(base_name);
            if !base_candidates.is_empty() {
                return Some(base_candidates[0].id.clone());
            }
        }
        return None;
    }
    const CONTEXT_DIRS: [&str; 6] = [
        "/context/",
        "/contexts/",
        "/src/context/",
        "/src/contexts/",
        "/providers/",
        "/src/providers/",
    ];
    if let Some(preferred) = candidates
        .iter()
        .find(|n| CONTEXT_DIRS.iter().any(|d| n.file_path.contains(d)))
    {
        return Some(preferred.id.clone());
    }
    Some(candidates[0].id.clone())
}

/// `name.replace(/Context$|Provider$/, '')` (react.ts:352).
fn strip_context_suffix(name: &str) -> &str {
    if let Some(stripped) = name.strip_suffix("Context") {
        stripped
    } else if let Some(stripped) = name.strip_suffix("Provider") {
        stripped
    } else {
        name
    }
}

fn dir_of(file: &str) -> String {
    match file.rfind('/') {
        Some(idx) => file[..idx].to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    /// Extraction context for a resolver that needs no project config.
    fn test_extraction_context() -> crate::framework::FrameworkExtractionContext {
        crate::framework::FrameworkExtractionContext::without_config("")
    }

    use super::*;
    use crate::types::ImportMapping;
    use codegraph_core::types::{EdgeKind, Node};
    use std::collections::HashMap;

    #[derive(Default)]
    struct Ctx {
        files: HashMap<String, String>,
        nodes: Vec<Node>,
        imports: HashMap<String, Vec<ImportMapping>>,
    }

    impl Ctx {
        fn file(mut self, path: &str, content: &str) -> Self {
            self.files.insert(path.to_string(), content.to_string());
            self
        }
        fn node(mut self, n: Node) -> Self {
            self.nodes.push(n);
            self
        }
        /// `file` imports `local` from `source`.
        fn import(mut self, file: &str, local: &str, source: &str) -> Self {
            self.imports
                .entry(file.to_string())
                .or_default()
                .push(ImportMapping {
                    local_name: local.to_string(),
                    exported_name: local.to_string(),
                    source: source.to_string(),
                    is_default: false,
                    is_namespace: false,
                });
            self
        }
    }

    impl ResolutionContext for Ctx {
        fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.file_path == file_path)
                .cloned()
                .collect()
        }
        fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.name == name)
                .cloned()
                .collect()
        }
        fn get_nodes_by_qualified_name(&self, q: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.qualified_name == q)
                .cloned()
                .collect()
        }
        fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.kind == kind)
                .cloned()
                .collect()
        }
        fn file_exists(&self, file_path: &str) -> bool {
            self.files.contains_key(file_path)
        }
        fn read_file(&self, file_path: &str) -> Option<String> {
            self.files.get(file_path).cloned()
        }
        fn get_project_root(&self) -> &str {
            "/project"
        }
        fn get_all_files(&self) -> Vec<String> {
            self.files.keys().cloned().collect()
        }
        fn get_nodes_by_lower_name(&self, lower: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.name.to_lowercase() == lower)
                .cloned()
                .collect()
        }
        fn get_node_by_id(&self, id: &str) -> Option<Node> {
            self.nodes.iter().find(|n| n.id == id).cloned()
        }
        fn get_import_mappings(&self, file: &str, _l: Language) -> Vec<ImportMapping> {
            self.imports.get(file).cloned().unwrap_or_default()
        }
    }

    /// A node of `file` at `lines` (1-based, inclusive) and byte `columns`,
    /// with its own qualified name.
    fn placed(
        kind: NodeKind,
        qualified_name: &str,
        file: &str,
        lines: (i64, i64),
        columns: (i64, i64),
    ) -> Node {
        let name = qualified_name.rsplit("::").next().unwrap_or(qualified_name);
        Node {
            qualified_name: qualified_name.to_string(),
            start_line: lines.0,
            end_line: lines.1,
            start_column: columns.0,
            end_column: columns.1,
            ..mk_node(
                &format!("{}:{file}:{qualified_name}:{}", kind.as_str(), lines.0),
                kind,
                name,
                file,
                Language::Tsx,
            )
        }
    }

    fn mk_node(id: &str, kind: NodeKind, name: &str, file: &str, lang: Language) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: format!("{file}::{name}"),
            file_path: file.to_string(),
            language: lang,
            start_line: 1,
            end_line: 1,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }

    fn a_ref(name: &str, kind: EdgeKind, file: &str, lang: Language) -> RefView {
        RefView {
            row_id: None,
            from_node_id: format!("from:{file}"),
            reference_name: name.to_string(),
            reference_kind: kind,
            line: 1,
            column: 0,
            file_path: file.to_string(),
            language: lang,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    /// The export a lazy route's loader picks, `import:./x#Page`, is React's
    /// only from a React route: Vue Router and Angular name a lazily loaded
    /// component the same way (upstream `isReactRouteRef`).
    #[test]
    fn a_picked_export_resolves_only_from_a_react_route() {
        let login = placed(
            NodeKind::Function,
            "Login",
            "src/views/Login.tsx",
            (1, 3),
            (0, 1),
        );
        let ctx = Ctx::default()
            .file("src/router.tsx", "")
            .file("src/router.ts", "")
            .file(
                "src/views/Login.tsx",
                "export function Login() {\n  return null;\n}\n",
            )
            .node(login.clone());
        let from = |file: &str, language: Language, node: &str| RefView {
            from_node_id: node.to_string(),
            ..a_ref(
                "import:./views/Login#Login",
                EdgeKind::References,
                file,
                language,
            )
        };
        let route = from(
            "src/router.tsx",
            Language::Tsx,
            "route:src/router.tsx:3:/login",
        );
        assert!(ReactResolver.claims_reference(&route.reference_name));
        assert_eq!(
            ReactResolver
                .resolve(&route, &ctx)
                .map(|r| r.target_node_id),
            Some(login.id.clone())
        );
        for reference in [
            from(
                "src/router.ts",
                Language::TypeScript,
                "route:src/router.ts:3:/login",
            ),
            from(
                "src/router.tsx",
                Language::Tsx,
                "function:src/router.tsx:Routes:1",
            ),
            from(
                "src/router.tsx",
                Language::Tsx,
                "route:src/other.tsx:3:/login",
            ),
        ] {
            assert!(
                ReactResolver.resolve(&reference, &ctx).is_none(),
                "{}",
                reference.from_node_id
            );
        }
        // A layout the loader picks, the same way.
        let layout = RefView {
            reference_name: "layout:import:./views/Login#Login".to_string(),
            ..route.clone()
        };
        assert_eq!(
            ReactResolver
                .resolve(&layout, &ctx)
                .map(|r| r.target_node_id),
            Some(login.id)
        );
    }

    // -- pure-helper units --------------------------------------------------

    #[test]
    fn is_pascal_case_matches_and_rejects() {
        assert!(is_pascal_case("Button"));
        assert!(is_pascal_case("A"));
        assert!(!is_pascal_case("button"));
        assert!(!is_pascal_case("_X"));
        assert!(!is_pascal_case(""));
    }

    #[test]
    fn is_built_in_type_flags_known_and_passes_others() {
        assert!(is_built_in_type("React"));
        assert!(is_built_in_type("Fragment"));
        assert!(!is_built_in_type("Button"));
    }

    #[test]
    fn is_component_kind_covers_variants() {
        assert!(is_component_kind(NodeKind::Component));
        assert!(is_component_kind(NodeKind::Function));
        assert!(is_component_kind(NodeKind::Class));
        assert!(!is_component_kind(NodeKind::Variable));
    }

    #[test]
    fn dir_of_cases() {
        assert_eq!(dir_of("src/a/b.tsx"), "src/a");
        assert_eq!(dir_of("top.tsx"), "");
    }

    #[test]
    fn strip_context_suffix_cases() {
        assert_eq!(strip_context_suffix("AuthContext"), "Auth");
        assert_eq!(strip_context_suffix("ThemeProvider"), "Theme");
        assert_eq!(strip_context_suffix("Plain"), "Plain");
    }

    // -- dep_present via detect --------------------------------------------

    #[test]
    fn detect_via_dev_dependency() {
        let ctx = Ctx::default().file("package.json", r#"{"devDependencies":{"next":"14"}}"#);
        assert!(ReactResolver.detect(&ctx));
    }

    #[test]
    fn detect_via_react_native_dependency() {
        let ctx = Ctx::default().file("package.json", r#"{"dependencies":{"react-native":"0.7"}}"#);
        assert!(ReactResolver.detect(&ctx));
    }

    #[test]
    fn detect_malformed_package_json_falls_through_to_file_scan() {
        // Unparseable package.json is ignored; a .jsx file still triggers detection.
        let ctx = Ctx::default()
            .file("package.json", "not json {{{")
            .file("src/App.jsx", "export default function App(){}");
        assert!(ReactResolver.detect(&ctx));
    }

    #[test]
    fn detect_false_when_no_signal() {
        let ctx = Ctx::default().file("src/index.ts", "export const x = 1;");
        assert!(!ReactResolver.detect(&ctx));
    }

    // -- resolve: hook directory preference --------------------------------

    #[test]
    fn resolve_hook_in_a_script_is_the_files_own() {
        // In a JS/TS module another file's hook comes through an import (the
        // import resolver's), never by name (upstream #2240).
        let hook = mk_node(
            "fn:src/util.ts:useThing:1",
            NodeKind::Function,
            "useThing",
            "src/util.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().node(hook.clone());
        let elsewhere = a_ref("useThing", EdgeKind::Calls, "src/Page.tsx", Language::Tsx);
        assert!(ReactResolver.resolve(&elsewhere, &ctx).is_none());
        let own = a_ref(
            "useThing",
            EdgeKind::Calls,
            "src/util.ts",
            Language::TypeScript,
        );
        let r = ReactResolver.resolve(&own, &ctx).expect("resolves");
        assert_eq!(r.target_node_id, hook.id);
        assert_eq!(r.confidence, 0.85);
    }

    #[test]
    fn resolve_hook_never_targets_a_nested_hook() {
        // A hook declared inside another function is only callable in there
        // (upstream #2120): trpc's `useQuery` inside one of its factories.
        let factory = placed(
            NodeKind::Function,
            "createHooks",
            "src/hooks.ts",
            (1, 10),
            (0, 1),
        );
        let nested = placed(
            NodeKind::Function,
            "createHooks::useQuery",
            "src/hooks.ts",
            (2, 4),
            (2, 3),
        );
        let ctx = Ctx::default().node(factory).node(nested);
        let reference = a_ref(
            "useQuery",
            EdgeKind::Calls,
            "src/hooks.ts",
            Language::TypeScript,
        );
        assert!(ReactResolver.resolve(&reference, &ctx).is_none());
    }

    #[test]
    fn resolve_hook_outside_a_script_prefers_a_hooks_directory() {
        // Only a non-script language reaches another file's hook by name: a
        // hooks/ directory first, else the first candidate.
        let far = mk_node(
            "fn:src/util.ts:useThing:1",
            NodeKind::Function,
            "useThing",
            "src/util.ts",
            Language::TypeScript,
        );
        let near = mk_node(
            "fn:src/hooks/useThing.ts:useThing:1",
            NodeKind::Function,
            "useThing",
            "src/hooks/useThing.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().node(far.clone()).node(near.clone());
        assert_eq!(
            resolve_hook("useThing", "src/a.py", &ctx, Language::Python),
            Some(near.id)
        );
        let ctx = Ctx::default().node(far.clone());
        assert_eq!(
            resolve_hook("useThing", "src/a.py", &ctx, Language::Python),
            Some(far.id)
        );
    }

    #[test]
    fn resolve_hook_none_when_not_a_use_function() {
        // Candidate exists but is not a `use*` function -> no hook resolution.
        let notf = mk_node(
            "class:src/Page.tsx:useThing:1",
            NodeKind::Class,
            "useThing",
            "src/Page.tsx",
            Language::Tsx,
        );
        let ctx = Ctx::default().node(notf);
        let reference = a_ref("useThing", EdgeKind::Calls, "src/Page.tsx", Language::Tsx);
        assert!(ReactResolver.resolve(&reference, &ctx).is_none());
    }

    #[test]
    fn resolve_hook_short_use_name_skipped() {
        // "use" alone (len == 3) is not a hook name.
        let ctx = Ctx::default();
        let reference = a_ref("use", EdgeKind::Calls, "src/Page.tsx", Language::Tsx);
        assert!(ReactResolver.resolve(&reference, &ctx).is_none());
    }

    #[test]
    fn resolve_context_in_a_script_is_the_files_own() {
        // trpc's adapters' `createContext?.(…)` is an option, not an example
        // app's `createContext` (upstream #2240).
        let other = mk_node(
            "var:src/context/AuthContext.tsx:AuthContext:1",
            NodeKind::Variable,
            "AuthContext",
            "src/context/AuthContext.tsx",
            Language::Tsx,
        );
        let own = mk_node(
            "var:src/App.tsx:AuthContext:1",
            NodeKind::Variable,
            "AuthContext",
            "src/App.tsx",
            Language::Tsx,
        );
        let ctx = Ctx::default().node(other).node(own.clone());
        let reference = a_ref(
            "AuthContext",
            EdgeKind::References,
            "src/App.tsx",
            Language::Tsx,
        );
        let r = ReactResolver.resolve(&reference, &ctx).expect("resolves");
        assert_eq!(r.target_node_id, own.id);
        let elsewhere = a_ref(
            "AuthContext",
            EdgeKind::References,
            "src/Page.tsx",
            Language::Tsx,
        );
        assert!(ReactResolver.resolve(&elsewhere, &ctx).is_none());
    }

    /// A reference outside JS/TS, which only [`resolve_context`] itself sees.
    fn script_less_ref(name: &str) -> RefView {
        a_ref(name, EdgeKind::References, "src/app.py", Language::Python)
    }

    #[test]
    fn resolve_context_strips_suffix_when_no_direct_match() {
        // No `AuthProvider` node, but a base `Auth` node exists -> suffix strip
        // fallback resolves to it.
        let base = mk_node(
            "var:src/Auth.tsx:Auth:1",
            NodeKind::Variable,
            "Auth",
            "src/Auth.tsx",
            Language::Tsx,
        );
        let ctx = Ctx::default().node(base.clone());
        assert_eq!(
            resolve_context("AuthProvider", &script_less_ref("AuthProvider"), &ctx),
            Some(base.id)
        );
    }

    #[test]
    fn resolve_context_prefers_context_dir() {
        let far = mk_node(
            "var:src/other/AuthContext.tsx:AuthContext:1",
            NodeKind::Variable,
            "AuthContext",
            "src/other/AuthContext.tsx",
            Language::Tsx,
        );
        let near = mk_node(
            "var:src/context/AuthContext.tsx:AuthContext:1",
            NodeKind::Variable,
            "AuthContext",
            "src/context/AuthContext.tsx",
            Language::Tsx,
        );
        let ctx = Ctx::default().node(far).node(near.clone());
        assert_eq!(
            resolve_context("AuthContext", &script_less_ref("AuthContext"), &ctx),
            Some(near.id)
        );
    }

    #[test]
    fn resolve_context_none_when_no_candidate_and_no_base() {
        let ctx = Ctx::default();
        let reference = a_ref(
            "MissingProvider",
            EdgeKind::References,
            "src/App.tsx",
            Language::Tsx,
        );
        assert!(ReactResolver.resolve(&reference, &ctx).is_none());
        assert_eq!(
            resolve_context("MissingProvider", &script_less_ref("MissingProvider"), &ctx),
            None
        );
    }

    // -- resolve: the file's imports and its own values ---------------------

    #[test]
    fn an_imported_name_is_left_to_import_resolution() {
        // antd's `<Button>` renders the package's, not the project's
        // namesake (upstream #2120, #2475); the same for a hook or context.
        let button = mk_node(
            "component:src/components/Button.tsx:Button:1",
            NodeKind::Component,
            "Button",
            "src/components/Button.tsx",
            Language::Tsx,
        );
        let hook = mk_node(
            "fn:src/Page.tsx:useQuery:1",
            NodeKind::Function,
            "useQuery",
            "src/Page.tsx",
            Language::Tsx,
        );
        let ctx = Ctx::default().node(button.clone()).node(hook.clone());
        let tag = a_ref(
            "Button",
            EdgeKind::References,
            "src/Page.tsx",
            Language::Tsx,
        );
        let call = a_ref("useQuery", EdgeKind::Calls, "src/Page.tsx", Language::Tsx);
        assert_eq!(
            ReactResolver.resolve(&tag, &ctx).map(|r| r.target_node_id),
            Some(button.id)
        );
        assert_eq!(
            ReactResolver.resolve(&call, &ctx).map(|r| r.target_node_id),
            Some(hook.id)
        );
        let ctx = ctx.import("src/Page.tsx", "Button", "antd").import(
            "src/Page.tsx",
            "useQuery",
            "@tanstack/react-query",
        );
        assert!(ReactResolver.resolve(&tag, &ctx).is_none());
        assert!(ReactResolver.resolve(&call, &ctx).is_none());
    }

    /// `src/App.tsx` for the declared-value cases, with each value's node.
    fn app_with_values() -> Ctx {
        let file = "src/App.tsx";
        let source = "import { observer } from 'mobx-react';\n\
                      const Panel = observer(function PanelInner() {\n\
                      \x20 return <div />;\n\
                      });\n\
                      function Inner() { return <span />; }\n\
                      const Item = forwardRef(Inner) as any; // a cast\n\
                      const Home = lazy(() => import('./pages/Home'));\n";
        let mut ctx = Ctx::default().file(file, source);
        for (kind, name, lines, columns) in [
            (NodeKind::Constant, "Panel", (2, 4), (6, 2)),
            (NodeKind::Function, "Panel::PanelInner", (2, 4), (23, 1)),
            (NodeKind::Function, "Inner", (5, 5), (0, 37)),
            (NodeKind::Constant, "Item", (6, 6), (6, 37)),
            (NodeKind::Constant, "Home", (7, 7), (6, 47)),
        ] {
            ctx = ctx.node(placed(kind, name, file, lines, columns));
        }
        // Another app's namesakes, in a component directory.
        for name in ["Panel", "Item", "Home"] {
            ctx = ctx.node(mk_node(
                &format!("function:src/components/{name}.tsx:{name}:1"),
                NodeKind::Function,
                name,
                &format!("src/components/{name}.tsx"),
                Language::Tsx,
            ));
        }
        ctx
    }

    #[test]
    fn a_value_the_file_declares_renders_what_it_wraps() {
        // Upstream #2420: the function a value holds, or the component of
        // its own file a wrapper call is handed, past a type assertion.
        let ctx = app_with_values();
        for (name, target) in [
            ("Panel", "function:src/App.tsx:Panel::PanelInner:2"),
            ("Item", "function:src/App.tsx:Inner:5"),
        ] {
            let reference = a_ref(name, EdgeKind::References, "src/App.tsx", Language::Tsx);
            let r = ReactResolver.resolve(&reference, &ctx).expect(name);
            assert_eq!(r.target_node_id, target, "{name}");
            assert_eq!(r.confidence, 0.9, "{name}");
        }
    }

    #[test]
    fn a_value_wrapping_no_component_is_its_own_declaration() {
        // Nothing to render is found, so the declaration binds by name, never
        // another app's same-named component; elsewhere that one still wins.
        let ctx = app_with_values();
        let own = a_ref("Home", EdgeKind::References, "src/App.tsx", Language::Tsx);
        assert!(ReactResolver.resolve(&own, &ctx).is_none());
        let elsewhere = a_ref("Home", EdgeKind::References, "src/Page.tsx", Language::Tsx);
        assert_eq!(
            ReactResolver
                .resolve(&elsewhere, &ctx)
                .map(|r| r.target_node_id),
            Some("function:src/components/Home.tsx:Home:1".to_string())
        );
    }

    #[test]
    fn an_inner_function_counts_by_its_name_or_its_end() {
        let file = "src/Page.tsx";
        let value = placed(NodeKind::Constant, "Page", file, (1, 6), (6, 2));
        // A helper inside an arrow the graph has no node for renders nothing.
        let ctx = Ctx::default().node(value.clone()).node(placed(
            NodeKind::Function,
            "Page::load",
            file,
            (2, 2),
            (8, 20),
        ));
        assert_eq!(wrapped_component(&value, &ctx), None);
        // The value's namesake is what it holds, wherever it ends.
        let ctx = ctx.node(placed(
            NodeKind::Function,
            "Page::Page",
            file,
            (3, 5),
            (2, 3),
        ));
        assert_eq!(
            wrapped_component(&value, &ctx).map(|node| node.id.clone()),
            Some("function:src/Page.tsx:Page::Page:3".to_string())
        );
        // A lone inner function ending where the value does.
        let ctx = Ctx::default().node(value.clone()).node(placed(
            NodeKind::Function,
            "Page::Render",
            file,
            (2, 6),
            (8, 1),
        ));
        assert_eq!(
            wrapped_component(&value, &ctx).map(|node| node.id.clone()),
            Some("function:src/Page.tsx:Page::Render:2".to_string())
        );
    }

    #[test]
    fn a_declaration_reads_its_own_declarator() {
        let ctx = app_with_values();
        let item = placed(NodeKind::Constant, "Item", "src/App.tsx", (6, 6), (6, 37));
        assert_eq!(
            declaration_text(&item, &ctx),
            "Item = forwardRef(Inner) as any"
        );
        let panel = placed(NodeKind::Constant, "Panel", "src/App.tsx", (2, 4), (6, 2));
        assert_eq!(
            declaration_text(&panel, &ctx),
            "Panel = observer(function PanelInner() {\n  return <div />;\n})"
        );
        // A file that no longer reads that way gives nothing.
        let stale = placed(NodeKind::Constant, "Item", "src/App.tsx", (60, 61), (6, 37));
        assert_eq!(declaration_text(&stale, &ctx), "");
        let ctx = Ctx::default().file("src/App.tsx", "const \u{e9}t\u{e9} = 1;\n");
        let split = placed(NodeKind::Constant, "x", "src/App.tsx", (1, 1), (7, 20));
        assert_eq!(declaration_text(&split, &ctx), "");
    }

    #[test]
    fn resolve_component_prefers_component_dir_over_ambiguous() {
        // Two candidates in unrelated dirs, one under /components/ -> that wins.
        let a = mk_node(
            "component:src/a/Button.tsx:Button:1",
            NodeKind::Component,
            "Button",
            "src/a/Button.tsx",
            Language::Tsx,
        );
        let b = mk_node(
            "component:src/components/Button.tsx:Button:1",
            NodeKind::Component,
            "Button",
            "src/components/Button.tsx",
            Language::Tsx,
        );
        let ctx = Ctx::default().node(a).node(b.clone());
        let reference = a_ref(
            "Button",
            EdgeKind::References,
            "src/z/Page.tsx",
            Language::Tsx,
        );
        let r = ReactResolver.resolve(&reference, &ctx).expect("resolves");
        assert_eq!(r.target_node_id, b.id);
    }

    #[test]
    fn resolve_component_ambiguous_multiple_returns_none() {
        // Two non-preferred candidates in different dirs -> ambiguous, None.
        let a = mk_node(
            "component:src/a/Button.tsx:Button:1",
            NodeKind::Component,
            "Button",
            "src/a/Button.tsx",
            Language::Tsx,
        );
        let b = mk_node(
            "component:src/b/Button.tsx:Button:1",
            NodeKind::Component,
            "Button",
            "src/b/Button.tsx",
            Language::Tsx,
        );
        let ctx = Ctx::default().node(a).node(b);
        let reference = a_ref(
            "Button",
            EdgeKind::References,
            "src/z/Page.tsx",
            Language::Tsx,
        );
        assert!(ReactResolver.resolve(&reference, &ctx).is_none());
    }

    #[test]
    fn resolve_component_none_when_no_component_kind() {
        let v = mk_node(
            "var:src/Button.tsx:Button:1",
            NodeKind::Variable,
            "Button",
            "src/Button.tsx",
            Language::Tsx,
        );
        let ctx = Ctx::default().node(v);
        let reference = a_ref(
            "Button",
            EdgeKind::References,
            "src/Page.tsx",
            Language::Tsx,
        );
        assert!(ReactResolver.resolve(&reference, &ctx).is_none());
    }

    #[test]
    fn resolve_built_in_type_component_skipped() {
        // PascalCase but a built-in type -> pattern 1 does not fire.
        let ctx = Ctx::default();
        let reference = a_ref("React", EdgeKind::References, "src/Page.tsx", Language::Tsx);
        assert!(ReactResolver.resolve(&reference, &ctx).is_none());
    }

    // -- extract: routes only -------------------------------------------------

    #[test]
    fn extract_emits_no_component_or_hook_nodes() {
        // The tree-sitter extractor owns components and hooks (upstream
        // #841); a regex node here duplicated them.
        let content = "export const Card = () => { return <div/>; };\n\
                       export function Panel() { return <div/>; }\n\
                       const Fancy = React.forwardRef(() => <div/>);\n\
                       export function useCounter() { return 0; }\n";
        for file in [
            "src/Card.tsx",
            "src/Card.jsx",
            "src/useCounter.ts",
            "src/useX.js",
        ] {
            let result = ReactResolver
                .extract(file, content, &test_extraction_context())
                .expect("extract");
            assert!(result.nodes.is_empty(), "{file}: {:?}", result.nodes);
        }
    }

    #[test]
    fn extract_reads_react_router_routes() {
        // The routes are React Router's scanner's (`super::react_router`);
        // React's extraction hands them over, each with what it renders.
        let content = "<Route path=\"/x\" element={<Home/>}/>";
        let result = ReactResolver
            .extract("src/App.tsx", content, &test_extraction_context())
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");
        assert_eq!(route.name, "/x");
        assert_eq!(route.language, Language::Tsx);
        assert!(result.references.iter().any(|r| r.reference_name == "Home"));
        // A route with no address of its own, and a data route rendering
        // nothing, are no routes.
        for (file, content) in [
            ("src/App.tsx", "<Route element={<Home/>}/>"),
            (
                "src/routes.tsx",
                "createBrowserRouter([{ path: '/only' }]);",
            ),
        ] {
            let result = ReactResolver
                .extract(file, content, &test_extraction_context())
                .expect("extract");
            assert!(result.nodes.is_empty(), "{content}: {:?}", result.nodes);
        }
    }
}
