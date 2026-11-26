//! Rails (upstream `resolution/frameworks/ruby.ts` at `e8ddc4e2`: #2155's
//! scoped routes, #2269's `only:`/`except:` forms).
//!
//! A project is Rails' when its root `Gemfile` names the `rails` or
//! `railties` gem, when its root holds `config/application.rb`,
//! `app/controllers/application_controller.rb` or `config/routes.rb`
//! ([`ROOT_DETECTION_FILES`], the sync's `ruby` control kind), or when any
//! indexed file is an engine's `config/routes.rb`.
//!
//! A `.rb` file whose comment-stripped text calls `routes.draw`, or that
//! sits in a `config/routes/` directory, is a routes file, read with its
//! blocks ([`scoped_routes`]): `namespace` and `scope` prefix paths and
//! controllers, a `resources … do` block nests its children under the
//! parent's `:x_id`, and `member` and `collection` blocks add actions to the
//! member and collection paths. Any other `.rb` file contributes its
//! explicit `get 'path', to: 'c#a'` routes and its `resources :x` lines
//! ([`flat_routes`]). Every route references its handler as
//! `controller#action`.
//!
//! Resolution, in upstream's order: `controller#action` to the action
//! method or function in `app/controllers/{controller}_controller.rb`, else
//! in the file of the controller class (0.85; when it finds none, no other
//! Rails pattern runs); a CamelCase name to a model class (0.8);
//! `…Controller` to a controller class (0.85); `…Helper` to a helper module
//! and `…Service` or `…Job` to a class (0.8). Where upstream takes the first
//! of several candidates in its store's order, Rust takes the only one and
//! gives no result for several, and it only considers Ruby classes,
//! modules, methods and functions (KNOWN_DIFFS, KEEP-RUST).

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use super::js_scan::{JS_SPACE, is_js_space, js_trim};
use super::offset_text::OffsetText;
use super::{framework_node, sole};
use crate::framework::{FrameworkExtractionContext, FrameworkResolver};
use crate::types::{
    FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedBy, ResolvedRef,
};

/// The languages the resolver reads (upstream `languages`).
const LANGUAGES: [Language; 1] = [Language::Ruby];

/// The root files detection reads by path, indexed or not: inputs of the
/// sync's `ruby` control kind (`codegraph_watch::control_inputs`), since a
/// `Gemfile` is never indexed and a configuration may keep the other three
/// out of the index.
pub const ROOT_DETECTION_FILES: [&str; 4] = [
    "Gemfile",
    "config/application.rb",
    "app/controllers/application_controller.rb",
    "config/routes.rb",
];

/// What a file's source must contain for either reading to find a route:
/// a routes file's `routes.draw`, a `resources` line's `resource`, an
/// explicit route's `to:` or `=>`. The comment stripper only ever blanks
/// text, so the stripped text holds none of them when the source does not.
const ROUTE_NEEDLES: [&str; 4] = ["routes.draw", "resource", "to:", "=>"];

/// A plural resource's actions, in upstream's order (`PLURAL_ACTIONS`).
const PLURAL_ACTIONS: [&str; 7] = [
    "index", "create", "new", "show", "edit", "update", "destroy",
];

/// A singular resource's actions: no `index` (`SINGULAR_ACTIONS`).
const SINGULAR_ACTIONS: [&str; 6] = ["create", "new", "show", "edit", "update", "destroy"];

/// What a route's action can be: a method of a controller class, or a
/// function.
const ACTION_KINDS: [NodeKind; 2] = [NodeKind::Method, NodeKind::Function];

/// A pattern ported from upstream: `{S}` is JavaScript's `\s`, `{W}` the body
/// of its `\w` (ASCII), `{D}` its `.` (no line terminator), and its `\b` is
/// written `(?-u:\b)`. Upstream's `(["'])…\1` is written as two
/// alternatives, one per quote.
fn ruby_regex(pattern: &str) -> Regex {
    Regex::new(
        &pattern
            .replace("{S}", JS_SPACE)
            .replace("{W}", "A-Za-z0-9_")
            .replace("{D}", r"[^\n\r\x{2028}\x{2029}]"),
    )
    .expect("rails pattern")
}

macro_rules! patterns {
    ($($(#[$doc:meta])* $name:ident = $pattern:expr;)*) => {
        $(
            $(#[$doc])*
            fn $name() -> &'static Regex {
                static PATTERN: OnceLock<Regex> = OnceLock::new();
                PATTERN.get_or_init(|| ruby_regex($pattern))
            }
        )*
    };
}

patterns! {
    /// A `Gemfile` line requiring Rails.
    rails_gem = r#"(?-u:\b)gem{S}+["'](?:rails|railties)["']"#;
    /// A routes file's draw call.
    routes_draw = r"(?-u:\b)routes\.draw(?-u:\b)";
    /// A route's `controller#action` reference (upstream's claim and
    /// Pattern 0).
    controller_action_name = r"^([{W}/]+)#([{W}]+)$";
    /// A model's name (upstream's Pattern 1).
    model_name = r"^[A-Z][a-zA-Z]+$";
    /// An explicit route outside a routes file: `get 'p', to: 'c#a'` or
    /// `get 'p' => 'c#a'`.
    explicit_route = r#"(?-u:\b)(get|post|put|patch|delete|match){S}+['"]([^'"]+)['"]{S}*(?:,{S}*to:{S}*|=>{S}*)['"]([^#'"]+)#([^'"]+)['"]"#;
    /// A `resources :x` or `resource :x` outside a routes file, with the
    /// rest of its line.
    flat_resources = r"(?-u:\b)(resources?){S}+:([{W}]+)([^\n]*)";
    /// A resource's `only:` action list, in any of the forms Rails accepts
    /// (upstream `railsActionOption`).
    only_option = r#"(?:(?-u:\b)only:|:only{S}*=>){S}*(?:\[([^\]]*)\]|%[iIwW]{S}*[\[(]([^\])]*)[\])]|:([{W}]+)|["']([{W}]+)["'])"#;
    /// A resource's `except:` action list, the same way.
    except_option = r#"(?:(?-u:\b)except:|:except{S}*=>){S}*(?:\[([^\]]*)\]|%[iIwW]{S}*[\[(]([^\])]*)[\])]|:([{W}]+)|["']([{W}]+)["'])"#;
    /// A statement that opens a block: it ends in `do`, perhaps with block
    /// parameters.
    opens_block = r"(?-u:\b)do{S}*(?:\|[^|]*\|)?{S}*$";
    /// A statement closing a block.
    end_line = r"^end(?-u:\b)";
    /// `namespace :admin` or `namespace "admin/v2"`, and what follows.
    namespace_line = r#"^namespace{S}+(:[{W}]+|["'][{W}/-]+["'])({D}*)$"#;
    /// `scope …`, its arguments up to a closing `)` or `do`.
    scope_line = r"^scope(?-u:\b){S}*\(?{S}*({D}*?)\)?{S}*(?:do(?-u:\b){D}*)?$";
    /// A scope's leading path string.
    scope_lead = r#"^(?:"([^"']*)"|'([^"']*)')"#;
    /// `resources :x` or `resource :x` in a routes file, and what follows.
    resource_line = r"^(resources?){S}+:([{W}]+)({D}*)$";
    /// A `member` block.
    member_line = r"^member(?-u:\b)";
    /// A `collection` block.
    collection_line = r"^collection(?-u:\b)";
    /// `root 'c#a'` or `root to: 'c#a'`.
    root_line = r#"^root{S}*\(?{S}*(?:to:{S}*)?["']([{W}/]+)#([{W}]+)["']"#;
    /// A verb route in a routes file: its verb, its path or action, and
    /// what follows.
    verb_line = r#"^(get|post|put|patch|delete|match){S}*\(?{S}*(:[{W}]+|["'][^"']*["'])({D}*)$"#;
    /// A verb route's `to: 'c#a'` or `=> 'c#a'`.
    verb_target = r#"(?:(?-u:\b)to:{S}*|=>{S}*)["']([{W}/]+)#([{W}]+)["']"#;
    /// A verb route's bare action inside a resource.
    bare_action = r"^[{W}-]+$";
    /// A statement opening a block that names no route.
    plain_block = r"^(?:if|unless|case|begin|while|until)(?-u:\b)";
    /// A `path:` option (upstream `option(tail, 'path')`).
    path_option = r#"(?-u:\b)path:{S}*(:[{W}]+|["'][^"']*["'])"#;
    /// A `module:` option.
    module_option = r#"(?-u:\b)module:{S}*(:[{W}]+|["'][^"']*["'])"#;
    /// A `controller:` option.
    controller_option = r#"(?-u:\b)controller:{S}*(:[{W}]+|["'][^"']*["'])"#;
    /// An `action:` option.
    action_option = r#"(?-u:\b)action:{S}*(:[{W}]+|["'][^"']*["'])"#;
}

pub struct RailsResolver;

impl FrameworkResolver for RailsResolver {
    fn name(&self) -> &str {
        "rails"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&LANGUAGES)
    }

    /// A root `Gemfile` requiring `rails` or `railties`, a root
    /// `config/application.rb`, `app/controllers/application_controller.rb`
    /// or `config/routes.rb`, or an indexed `config/routes.rb` at any depth
    /// (an engine's: solidus keeps each engine's under `backend/config/`).
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        if context
            .read_file_shared("Gemfile")
            .is_some_and(|gemfile| rails_gem().is_match(&gemfile))
        {
            return true;
        }
        if ROOT_DETECTION_FILES[1..]
            .iter()
            .any(|file| context.file_exists(file))
        {
            return true;
        }
        context
            .get_all_files_shared()
            .iter()
            .any(|file| file == "config/routes.rb" || file.ends_with("/config/routes.rb"))
    }

    /// A route's `controller#action`, which names no node, past the name
    /// pre-filter. Only a Ruby reference's: no other language's resolvers
    /// include this one.
    fn claims_reference(&self, name: &str) -> bool {
        controller_action_name().is_match(name)
    }

    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let name = reference.reference_name.as_str();
        // Pattern 0: a route's `controller#action`. Precise, so it answers
        // alone: every controller has an `index`.
        if let Some((controller, action)) = route_target(name) {
            return controller_action(controller, action, context)
                .map(|target| framework_ref(reference, target, 0.85));
        }
        // Pattern 1: an ActiveRecord model.
        if model_name().is_match(name)
            && let Some(target) = model(name, context)
        {
            return Some(framework_ref(reference, target, 0.8));
        }
        // Pattern 2: a controller.
        if name.ends_with("Controller")
            && let Some(target) = controller_class(name, context)
        {
            return Some(framework_ref(reference, target, 0.85));
        }
        // Pattern 3: a helper module.
        if name.ends_with("Helper")
            && let Some(target) = helper(name, context)
        {
            return Some(framework_ref(reference, target, 0.8));
        }
        // Pattern 4: a service or a job.
        if (name.ends_with("Service") || name.ends_with("Job"))
            && let Some(target) = service(name, context)
        {
            return Some(framework_ref(reference, target, 0.8));
        }
        None
    }

    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        if !file_path.ends_with(".rb") {
            return Some(FrameworkResolverExtractionResult::default());
        }
        let routes_directory =
            file_path.starts_with("config/routes/") || file_path.contains("/config/routes/");
        if !routes_directory && !ROUTE_NEEDLES.iter().any(|needle| content.contains(needle)) {
            return Some(FrameworkResolverExtractionResult::default());
        }
        let text = OffsetText::ruby(content);
        if routes_directory || routes_draw().is_match(&text.safe) {
            return Some(scoped_routes(file_path, &text.safe));
        }
        Some(flat_routes(file_path, &text))
    }
}

fn framework_ref(reference: &RefView, target: String, confidence: f64) -> ResolvedRef {
    ResolvedRef::new(reference.clone(), target, confidence, ResolvedBy::Framework)
}

/// A route reference's controller path and action (`admin/zones#show`),
/// or `None` for a name that is not one.
pub fn route_target(name: &str) -> Option<(&str, &str)> {
    let found = controller_action_name().captures(name)?;
    Some((found.get(1)?.as_str(), found.get(2)?.as_str()))
}

/// The class a route's controller path names: its last segment camelized,
/// then `Controller` (`admin/zones` → `ZonesController`).
pub fn controller_class_name(controller: &str) -> String {
    let last = controller.rsplit('/').next().unwrap_or(controller);
    format!("{}Controller", camelize(last))
}

/// The file Rails' convention holds a route's controller in
/// (`app/controllers/admin/zones_controller.rb`).
pub fn controller_file(controller: &str) -> String {
    format!("app/controllers/{controller}_controller.rb")
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// The first Ruby node of one of `kinds` named `name` in `file_path`: the
/// first by line, as upstream's `getNodesInFile` orders them; on one line
/// the first by column, then id.
fn first_in_file(
    file_path: &str,
    kinds: &[NodeKind],
    name: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    context
        .get_nodes_in_file_shared(file_path)
        .iter()
        .filter(|node| {
            node.language == Language::Ruby && kinds.contains(&node.kind) && node.name == name
        })
        .min_by(|a, b| {
            (a.start_line, a.start_column, &a.id).cmp(&(b.start_line, b.start_column, &b.id))
        })
        .map(|node| node.id.clone())
}

/// The node of `kind` named `name` in the first of `paths` that exists and
/// holds one (upstream's `possiblePaths` loops).
fn in_conventional_file(
    paths: impl IntoIterator<Item = String>,
    kind: NodeKind,
    name: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    paths.into_iter().find_map(|path| {
        if context.file_exists(&path) {
            first_in_file(&path, &[kind], name, context)
        } else {
            None
        }
    })
}

/// The Ruby classes named `name` (KEEP-RUST: only Ruby's).
fn ruby_classes(name: &str, context: &dyn ResolutionContext) -> Vec<Arc<Node>> {
    context
        .get_nodes_by_name_shared(name)
        .into_iter()
        .filter(|node| node.kind == NodeKind::Class && node.language == Language::Ruby)
        .collect()
}

/// `controller#action` (upstream `resolveControllerAction`): the action in
/// `app/controllers/{controller}_controller.rb`, else in the file of a class
/// named for the path's last segment — of the ones whose path ends with the
/// written path when it has a module and some do (an engine's
/// `spree/admin/zones_controller.rb` for `admin/zones`, not another
/// module's same-named controller). Upstream takes the first such file
/// holding the action; Rust takes the only one and gives no result for
/// several.
fn controller_action(
    controller: &str,
    action: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let direct = controller_file(controller);
    if context.file_exists(&direct)
        && let Some(found) = first_in_file(&direct, &ACTION_KINDS, action, context)
    {
        return Some(found);
    }
    let classes = ruby_classes(&controller_class_name(controller), context);
    let own: Vec<&Arc<Node>> = if controller.contains('/') {
        let suffix = format!("/{controller}_controller.rb");
        classes
            .iter()
            .filter(|class| format!("/{}", class.file_path).ends_with(&suffix))
            .collect()
    } else {
        Vec::new()
    };
    let candidates: Vec<&Arc<Node>> = if own.is_empty() {
        classes.iter().collect()
    } else {
        own
    };
    let mut found: Vec<String> = Vec::new();
    for class in candidates {
        if let Some(id) = first_in_file(&class.file_path, &ACTION_KINDS, action, context)
            && !found.contains(&id)
        {
            found.push(id);
        }
    }
    sole(found)
}

/// A model (upstream `resolveModel`): the class in `app/models/{snake}.rb`,
/// then in `app/models/concerns/{snake}.rb`, else the only Ruby class of
/// that name in a path holding `app/models/`.
fn model(name: &str, context: &dyn ResolutionContext) -> Option<String> {
    if let Some(snake) = snake(name)
        && let Some(found) = in_conventional_file(
            [
                format!("app/models/{snake}.rb"),
                format!("app/models/concerns/{snake}.rb"),
            ],
            NodeKind::Class,
            name,
            context,
        )
    {
        return Some(found);
    }
    sole(
        ruby_classes(name, context)
            .into_iter()
            .filter(|class| class.file_path.contains("app/models/"))
            .map(|class| class.id.clone()),
    )
}

/// A controller (upstream `resolveController`): the class in
/// `app/controllers/{snake}.rb`, `app/controllers/api/{snake}.rb` or
/// `app/controllers/api/v1/{snake}.rb`, else the only Ruby class of that
/// name in a path holding `controllers/`.
fn controller_class(name: &str, context: &dyn ResolutionContext) -> Option<String> {
    if let Some(snake) = snake(name)
        && let Some(found) = in_conventional_file(
            [
                format!("app/controllers/{snake}.rb"),
                format!("app/controllers/api/{snake}.rb"),
                format!("app/controllers/api/v1/{snake}.rb"),
            ],
            NodeKind::Class,
            name,
            context,
        )
    {
        return Some(found);
    }
    sole(
        ruby_classes(name, context)
            .into_iter()
            .filter(|class| class.file_path.contains("controllers/"))
            .map(|class| class.id.clone()),
    )
}

/// A helper (upstream `resolveHelper`): the module in
/// `app/helpers/{snake}.rb`.
fn helper(name: &str, context: &dyn ResolutionContext) -> Option<String> {
    let snake = snake(name)?;
    in_conventional_file(
        [format!("app/helpers/{snake}.rb")],
        NodeKind::Module,
        name,
        context,
    )
}

/// A service or a job (upstream `resolveService`): the class in
/// `app/services/`, `app/jobs/` or `app/workers/`.
fn service(name: &str, context: &dyn ResolutionContext) -> Option<String> {
    let snake = snake(name)?;
    in_conventional_file(
        ["app/services", "app/jobs", "app/workers"].map(|dir| format!("{dir}/{snake}.rb")),
        NodeKind::Class,
        name,
        context,
    )
}

/// Upstream's `name.replace(/([A-Z])/g, '_$1').toLowerCase().slice(1)`: an
/// underscore before each ASCII capital (`HTMLParser` → `h_t_m_l_parser`),
/// lower case, the first UTF-16 unit dropped. `None` when that unit is half
/// of a character: the path upstream builds from the other half names no
/// file.
fn snake(name: &str) -> Option<String> {
    let mut spaced = String::with_capacity(name.len() + 4);
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            spaced.push('_');
        }
        spaced.push(c);
    }
    let lower = spaced.to_lowercase();
    let mut chars = lower.chars();
    let first = chars.next()?;
    (first.len_utf16() == 1).then(|| chars.as_str().to_string())
}

/// `snake_case` → `CamelCase` (upstream `camelize`), on the ASCII names a
/// route's controller path holds.
pub fn camelize(snake: &str) -> String {
    snake
        .split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    let mut word = String::with_capacity(part.len());
                    word.push(first.to_ascii_uppercase());
                    word.push_str(chars.as_str());
                    word
                }
                None => String::new(),
            }
        })
        .collect()
}

/// Upstream's naive `pluralize`: `y` after a consonant becomes `ies`, a
/// sibilant ending takes `es`, anything else `s`.
fn pluralize(word: &str) -> String {
    let bytes = word.as_bytes();
    if bytes.len() >= 2
        && bytes[bytes.len() - 1] == b'y'
        && !b"aeiou".contains(&bytes[bytes.len() - 2])
    {
        return format!("{}ies", &word[..word.len() - 1]);
    }
    if ["s", "x", "z", "ch", "sh"]
        .iter()
        .any(|ending| word.ends_with(ending))
    {
        return format!("{word}es");
    }
    format!("{word}s")
}

/// Upstream's naive `singularize`, for a nested resource's `:x_id`: `ies`
/// becomes `y`, `ss` and `us` stay, `ses` loses its `es` (upstream's
/// `(x|ch|sh|ses)$` together with `es$` holds for `ses` only), and a final
/// `s` goes.
fn singularize(word: &str) -> String {
    if let Some(stem) = word.strip_suffix("ies") {
        return format!("{stem}y");
    }
    if word.ends_with("ss") || word.ends_with("us") {
        return word.to_string();
    }
    if (word.ends_with('x')
        || word.ends_with("ch")
        || word.ends_with("sh")
        || word.ends_with("ses"))
        && word.ends_with("es")
    {
        return word[..word.len() - 2].to_string();
    }
    word.strip_suffix('s').unwrap_or(word).to_string()
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/// A RESTful action's verb (upstream `RESTFUL_ROUTES[action].method`) and,
/// outside a routes file, its path below `/{resource}`.
fn rest_route(action: &str) -> (&'static str, &'static str) {
    match action {
        "index" => ("GET", ""),
        "create" => ("POST", ""),
        "new" => ("GET", "/new"),
        "show" => ("GET", "/:id"),
        "edit" => ("GET", "/:id/edit"),
        "update" => ("PATCH", "/:id"),
        _ => ("DELETE", "/:id"),
    }
}

/// Upstream's `sym`: a leading `:` goes, then a leading and a trailing
/// quote.
fn sym(written: &str) -> String {
    let rest = written.strip_prefix(':').unwrap_or(written);
    let rest = rest.strip_prefix(['"', '\'']).unwrap_or(rest);
    rest.strip_suffix(['"', '\'']).unwrap_or(rest).to_string()
}

/// Upstream's route-path `join`: a leading `/`, the parts joined with `/`,
/// runs of `/` collapsed, and one trailing `/` dropped after any character
/// JavaScript's `.` matches.
fn join(parts: &[&str]) -> String {
    let joined = format!("/{}", parts.join("/"));
    let mut out = String::with_capacity(joined.len());
    for c in joined.chars() {
        if c == '/' && out.ends_with('/') {
            continue;
        }
        out.push(c);
    }
    if out.len() > 1
        && out.ends_with('/')
        && out[..out.len() - 1]
            .chars()
            .next_back()
            .is_some_and(|before| !matches!(before, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
    {
        out.pop();
    }
    out
}

/// An option's value as written (`:x` or a quoted string), when `tail`
/// holds the option.
fn option_value<'t>(tail: &'t str, pattern: &Regex) -> Option<&'t str> {
    Some(pattern.captures(tail)?.get(1)?.as_str())
}

/// A resource's `only:` or `except:` actions (upstream `railsActionOption`):
/// `[:a, :b]`, `%i[a b]` (any of `iIwW`, brackets or parentheses), `:a`,
/// `"a"`, or the older `:only => …`. `None` when the option is absent.
fn action_list(tail: &str, pattern: &Regex) -> Option<HashSet<String>> {
    let found = pattern.captures(tail)?;
    if let Some(list) = found.get(1) {
        return Some(
            list.as_str()
                .split(',')
                .map(|item| sym(js_trim(item)))
                .filter(|item| !item.is_empty())
                .collect(),
        );
    }
    if let Some(words) = found.get(2) {
        return Some(
            js_trim(words.as_str())
                .split(is_js_space)
                .filter(|word| !word.is_empty())
                .map(str::to_string)
                .collect(),
        );
    }
    let one = found.get(3).or_else(|| found.get(4))?;
    Some(HashSet::from([one.as_str().to_string()]))
}

/// A resource's actions, kept by its `only:` option, else by its `except:`.
fn resource_actions(plural: bool, tail: &str) -> Vec<&'static str> {
    let all: &[&'static str] = if plural {
        &PLURAL_ACTIONS
    } else {
        &SINGULAR_ACTIONS
    };
    if let Some(only) = action_list(tail, only_option()) {
        return all
            .iter()
            .copied()
            .filter(|action| only.contains(*action))
            .collect();
    }
    if let Some(except) = action_list(tail, except_option()) {
        return all
            .iter()
            .copied()
            .filter(|action| !except.contains(*action))
            .collect();
    }
    all.to_vec()
}

fn route_reference(from: &str, name: String, line: i64, file_path: &str) -> RefView {
    RefView {
        row_id: None,
        from_node_id: from.to_string(),
        reference_name: name,
        reference_kind: EdgeKind::References,
        line,
        column: 0,
        file_path: file_path.to_string(),
        language: Language::Ruby,
        is_function_ref: false,
        reference_subkind: None,
    }
}

/// A capture of the stripped text as upstream's holds it.
fn capture(text: &OffsetText, found: Option<regex::Match>) -> String {
    found.map_or(String::new(), |found| {
        text.upstream(found.start(), found.end())
    })
}

/// The routes of a file that is not a routes file (upstream's `extract`
/// after its routes-file test): its explicit routes, then its resources'.
fn flat_routes(file_path: &str, text: &OffsetText) -> FrameworkResolverExtractionResult {
    let mut result = FrameworkResolverExtractionResult::default();
    for found in explicit_route().captures_iter(&text.safe) {
        let whole = found.get(0).expect("whole match");
        let line = text.line_at(whole.start());
        let upper = found[1].to_ascii_uppercase();
        let path = capture(text, found.get(2));
        let id = format!("route:{file_path}:{line}:{upper}:{path}");
        let target = format!(
            "{}#{}",
            capture(text, found.get(3)),
            capture(text, found.get(4))
        );
        result
            .references
            .push(route_reference(&id, target, line, file_path));
        result.nodes.push(framework_node(
            id,
            NodeKind::Route,
            format!("{upper} {path}"),
            format!("{file_path}::route:{path}"),
            file_path.to_string(),
            line,
            line,
            0,
            text.units(whole.start(), whole.end()),
            Language::Ruby,
            false,
        ));
    }
    for found in flat_resources().captures_iter(&text.safe) {
        let whole = found.get(0).expect("whole match");
        let plural = &found[1] == "resources";
        // ASCII word characters, which the stripped text holds as the
        // source does.
        let name = &found[2];
        let tail = capture(text, found.get(3));
        let controller = if plural {
            name.to_string()
        } else {
            pluralize(name)
        };
        let line = text.line_at(whole.start());
        let end_column = text.units(whole.start(), whole.end());
        for action in resource_actions(plural, &tail) {
            let (method, below) = rest_route(action);
            let target = format!("{controller}#{action}");
            let id = format!("route:{file_path}:{line}:{method}:{target}");
            result.nodes.push(framework_node(
                id.clone(),
                NodeKind::Route,
                format!("{method} /{name}{below}"),
                format!("{file_path}::route:{target}"),
                file_path.to_string(),
                line,
                line,
                0,
                end_column,
                Language::Ruby,
                false,
            ));
            result
                .references
                .push(route_reference(&id, target, line, file_path));
        }
    }
    result
}

/// What a block of a routes file is (upstream `RailsFrame.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Namespace,
    Scope,
    Resource,
    Member,
    Collection,
    Block,
}

/// A block of a routes file (upstream `RailsFrame`): the path and the
/// controller module its routes get, and for a resource (and the member,
/// collection and plain blocks inside one, which copy it) its controller
/// and its collection and member paths.
#[derive(Debug, Clone)]
struct Frame {
    kind: FrameKind,
    path: String,
    module: String,
    controller: Option<String>,
    collection_path: Option<String>,
    member_path: Option<String>,
}

impl Frame {
    fn new(kind: FrameKind, path: String, module: String) -> Self {
        Self {
            kind,
            path,
            module,
            controller: None,
            collection_path: None,
            member_path: None,
        }
    }

    /// Upstream's `{ ...frame, kind }`.
    fn as_kind(&self, kind: FrameKind) -> Self {
        Self {
            kind,
            ..self.clone()
        }
    }
}

/// The routes a routes file's walk emits, each id once (upstream `emit`).
struct ScopedRoutes<'f> {
    file_path: &'f str,
    seen: HashSet<String>,
    result: FrameworkResolverExtractionResult,
}

impl ScopedRoutes<'_> {
    fn emit(&mut self, method: &str, path: &str, target: String, line: i64) {
        let file_path = self.file_path;
        let id = format!("route:{file_path}:{line}:{method}:{path}:{target}");
        if !self.seen.insert(id.clone()) {
            return;
        }
        self.result.nodes.push(framework_node(
            id.clone(),
            NodeKind::Route,
            format!("{method} {path}"),
            format!("{file_path}::route:{target}"),
            file_path.to_string(),
            line,
            line,
            0,
            0,
            Language::Ruby,
            false,
        ));
        self.result
            .references
            .push(route_reference(&id, target, line, file_path));
    }
}

/// How many more `[` and `(` a statement opens than it closes.
fn open_brackets(text: &str) -> i64 {
    text.bytes().fold(0, |open, byte| match byte {
        b'[' | b'(' => open + 1,
        b']' | b')' => open - 1,
        _ => open,
    })
}

/// A routes file's routes with their nesting (upstream
/// `extractScopedRailsRoutes`), read line by line from the stripped text: a
/// statement whose `[` or `(` stays open continues on the next lines unless
/// it opens a block; each `do`, and each `if`, `unless`, `case`, `begin`,
/// `while` or `until` line, opens a block an `end` closes. Every comment of
/// a line runs to its end, so the trimmed lines hold no blanked text and
/// read as upstream's do.
fn scoped_routes(file_path: &str, safe: &str) -> FrameworkResolverExtractionResult {
    let mut routes = ScopedRoutes {
        file_path,
        seen: HashSet::new(),
        result: FrameworkResolverExtractionResult::default(),
    };
    let mut stack = vec![Frame::new(FrameKind::Block, String::new(), String::new())];
    let lines: Vec<&str> = safe.split('\n').collect();
    let mut next = 0;
    while next < lines.len() {
        let line = next as i64 + 1;
        let mut text = js_trim(lines[next]).to_string();
        next += 1;
        if text.is_empty() {
            continue;
        }
        while open_brackets(&text) > 0 && next < lines.len() && !opens_block().is_match(&text) {
            text.push(' ');
            text.push_str(js_trim(lines[next]));
            next += 1;
        }
        if end_line().is_match(&text) {
            if stack.len() > 1 {
                stack.pop();
            }
            continue;
        }
        let opens = opens_block().is_match(&text);
        let frame = stack.last().expect("the file's frame").clone();
        match scoped_statement(&text, &frame, &stack, opens, line, &mut routes) {
            Some(pushed) => {
                if opens {
                    stack.push(pushed);
                }
            }
            None => {
                if opens || plain_block().is_match(&text) {
                    stack.push(frame.as_kind(FrameKind::Block));
                }
            }
        }
    }
    routes.result
}

/// One statement of a routes file: its routes emitted, and the frame it
/// would open (upstream's `pushed`).
fn scoped_statement(
    text: &str,
    frame: &Frame,
    stack: &[Frame],
    opens: bool,
    line: i64,
    routes: &mut ScopedRoutes,
) -> Option<Frame> {
    if let Some(found) = namespace_line().captures(text) {
        let name = sym(&found[1]);
        let segment = option_value(&found[2], path_option()).map_or_else(|| name.clone(), sym);
        return Some(Frame::new(
            FrameKind::Namespace,
            join(&[&frame.path, &segment]),
            format!("{}{name}/", frame.module),
        ));
    }
    if opens && let Some(found) = scope_line().captures(text) {
        let args = found.get(1).map_or("", |args| args.as_str());
        let lead = scope_lead().captures(args).map(|lead| {
            lead.get(1)
                .or_else(|| lead.get(2))
                .map_or("", |m| m.as_str())
        });
        let path = match lead {
            Some(lead) => lead.to_string(),
            None => option_value(args, path_option()).map_or_else(String::new, sym),
        };
        let module = match option_value(args, module_option()) {
            Some(module) => format!("{}{}/", frame.module, sym(module)),
            None => frame.module.clone(),
        };
        return Some(Frame::new(
            FrameKind::Scope,
            join(&[&frame.path, &path]),
            module,
        ));
    }
    if let Some(found) = resource_line().captures(text) {
        let plural = &found[1] == "resources";
        let name = &found[2];
        let tail = &found[3];
        let segment = option_value(tail, path_option()).map_or_else(|| name.to_string(), sym);
        let controller = match option_value(tail, controller_option()) {
            Some(written) => format!("{}{}", frame.module, sym(written)),
            None if plural => format!("{}{name}", frame.module),
            None => format!("{}{}", frame.module, pluralize(name)),
        };
        let collection_path = join(&[&frame.path, &segment]);
        let member_path = if plural {
            join(&[&collection_path, ":id"])
        } else {
            collection_path.clone()
        };
        for action in resource_actions(plural, tail) {
            let path = match action {
                "index" | "create" => collection_path.clone(),
                "new" => join(&[&collection_path, "new"]),
                "edit" => join(&[&member_path, "edit"]),
                _ => member_path.clone(),
            };
            routes.emit(
                rest_route(action).0,
                &path,
                format!("{controller}#{action}"),
                line,
            );
        }
        if !opens {
            return None;
        }
        let nested = if plural {
            join(&[&collection_path, &format!(":{}_id", singularize(name))])
        } else {
            collection_path.clone()
        };
        return Some(Frame {
            kind: FrameKind::Resource,
            path: nested,
            module: frame.module.clone(),
            controller: Some(controller),
            collection_path: Some(collection_path),
            member_path: Some(member_path),
        });
    }
    if opens && member_line().is_match(text) {
        return Some(frame.as_kind(FrameKind::Member));
    }
    if opens && collection_line().is_match(text) {
        return Some(frame.as_kind(FrameKind::Collection));
    }
    if let Some(found) = root_line().captures(text) {
        routes.emit(
            "GET",
            &join(&[&frame.path]),
            format!("{}{}#{}", frame.module, &found[1], &found[2]),
            line,
        );
        return None;
    }
    if let Some(found) = verb_line().captures(text) {
        let upper = found[1].to_ascii_uppercase();
        let method = if upper == "MATCH" { "ANY" } else { &upper };
        let raw = sym(&found[2]);
        let tail = &found[3];
        if let Some(to) = verb_target().captures(tail) {
            let base = match frame.kind {
                FrameKind::Member => frame.member_path.as_deref(),
                FrameKind::Collection => frame.collection_path.as_deref(),
                _ => Some(frame.path.as_str()),
            };
            routes.emit(
                method,
                &join(&[base.unwrap_or(""), &raw]),
                format!("{}{}#{}", frame.module, &to[1], &to[2]),
                line,
            );
            return None;
        }
        // The nearest resource, member or collection block: one without a
        // controller (a `member do` outside any resource) stops the search,
        // as upstream's `find` does.
        let resource = stack.iter().rev().find(|block| {
            matches!(
                block.kind,
                FrameKind::Resource | FrameKind::Member | FrameKind::Collection
            )
        });
        if let Some(resource) = resource
            && let Some(controller) = resource.controller.as_deref()
            && !controller.is_empty()
            && bare_action().is_match(&raw)
        {
            let action = sym(option_value(tail, action_option()).unwrap_or(&raw)).replace('-', "_");
            let base = if frame.kind == FrameKind::Collection {
                resource.collection_path.as_deref()
            } else {
                resource.member_path.as_deref()
            };
            routes.emit(
                method,
                &join(&[base.unwrap_or(""), &raw]),
                format!("{controller}#{action}"),
                line,
            );
        }
        return None;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::route_href::test_ctx::RouteCtx;

    fn rails(file: &str, src: &str) -> FrameworkResolverExtractionResult {
        RailsResolver
            .extract(
                file,
                src,
                &FrameworkExtractionContext::without_config("/project"),
            )
            .unwrap()
    }

    fn names(result: &FrameworkResolverExtractionResult) -> Vec<String> {
        result.nodes.iter().map(|node| node.name.clone()).collect()
    }

    fn targets(result: &FrameworkResolverExtractionResult) -> Vec<String> {
        result
            .references
            .iter()
            .map(|reference| reference.reference_name.clone())
            .collect()
    }

    /// The (name, target) pairs of the routes `src` draws in
    /// `config/routes.rb`, inside `Rails.application.routes.draw do … end`.
    fn drawn(src: &str) -> Vec<(String, String)> {
        let result = rails(
            "config/routes.rb",
            &format!("Rails.application.routes.draw do\n{src}end\n"),
        );
        assert_eq!(result.nodes.len(), result.references.len());
        result
            .nodes
            .iter()
            .zip(&result.references)
            .map(|(node, reference)| {
                assert_eq!(reference.from_node_id, node.id);
                (node.name.clone(), reference.reference_name.clone())
            })
            .collect()
    }

    fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
        expected
            .iter()
            .map(|(name, target)| (name.to_string(), target.to_string()))
            .collect()
    }

    fn node(kind: NodeKind, name: &str, file: &str, line: i64, language: Language) -> Node {
        let mut node = framework_node(
            id(kind, name, file, line),
            kind,
            name.to_string(),
            name.to_string(),
            file.to_string(),
            line,
            line + 1,
            0,
            0,
            language,
            false,
        );
        node.updated_at = 0;
        node
    }

    fn ruby(kind: NodeKind, name: &str, file: &str, line: i64) -> Node {
        node(kind, name, file, line, Language::Ruby)
    }

    /// A Ruby controller file: the class on line 1, a method every three
    /// lines from line 3.
    fn controller(file: &str, class: &str, methods: &[&str]) -> Vec<Node> {
        let mut nodes = vec![ruby(NodeKind::Class, class, file, 1)];
        for (at, method) in methods.iter().enumerate() {
            nodes.push(ruby(NodeKind::Method, method, file, 3 + 3 * at as i64));
        }
        nodes
    }

    fn id(kind: NodeKind, name: &str, file: &str, line: i64) -> String {
        format!("{}:{file}:{name}:{line}", kind.as_str())
    }

    fn reference(name: &str) -> RefView {
        RefView {
            row_id: None,
            from_node_id: "method:app/x.rb:call:1".to_string(),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::References,
            line: 1,
            column: 0,
            file_path: "app/x.rb".to_string(),
            language: Language::Ruby,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    fn target(ctx: &RouteCtx, name: &str) -> Option<(String, f64)> {
        RailsResolver
            .resolve(&reference(name), ctx)
            .map(|resolved| (resolved.target_node_id, resolved.confidence))
    }

    // -- detection, claims ------------------------------------------------

    #[test]
    fn detects_a_gemfile_requiring_rails_or_railties() {
        let gemfile =
            |text: &str| RailsResolver.detect(&RouteCtx::default().unlisted("Gemfile", text));
        assert!(gemfile(
            "source \"https://rubygems.org\"\ngem \"rails\", \"~> 7.1\"\n"
        ));
        assert!(gemfile("gem 'railties', '7.1.0'\n"));
        // JavaScript's `\s` holds a no-break space.
        assert!(gemfile("gem\u{a0}\"rails\"\n"));
        assert!(!gemfile("gem \"sinatra\"\ngem \"rails-html-sanitizer\"\n"));
        assert!(!gemfile("rubygem \"rails\"\n"));
        assert!(!gemfile("gem rails\n"));
        assert!(!RailsResolver.detect(&RouteCtx::default()));
    }

    #[test]
    fn detects_a_root_marker_or_a_routes_file_at_any_depth() {
        for file in [
            "config/application.rb",
            "app/controllers/application_controller.rb",
            "config/routes.rb",
        ] {
            assert!(
                RailsResolver.detect(&RouteCtx::default().unlisted(file, "")),
                "{file}"
            );
        }
        // An engine's routes file, indexed.
        assert!(RailsResolver.detect(&RouteCtx::default().file("backend/config/routes.rb", "")));
        assert!(RailsResolver.detect(&RouteCtx::default().file("config/routes.rb", "")));
        for file in [
            "backend/myconfig/routes.rb",
            "backend/config/routes.rbx",
            "config/routes/admin.rb",
            "app/controllers/users_controller.rb",
            "engine/config/application.rb",
        ] {
            assert!(
                !RailsResolver.detect(&RouteCtx::default().file(file, "")),
                "{file}"
            );
        }
        // The any-depth rule reads the indexed files only.
        assert!(
            !RailsResolver.detect(&RouteCtx::default().unlisted("engine/config/routes.rb", ""))
        );
    }

    #[test]
    fn claims_route_targets_only() {
        for name in ["users#index", "admin/zones#show", "a_b/c1#d_e", "/x#y"] {
            assert!(RailsResolver.claims_reference(name), "{name}");
        }
        for name in [
            "users",
            "#index",
            "users#",
            "users#in-dex",
            "Admin::Zones#show",
            "users#index#x",
            "users.index",
            "us ers#index",
        ] {
            assert!(!RailsResolver.claims_reference(name), "{name}");
        }
    }

    // -- frameworks.test.ts 1002-1016, 2122-2133 --------------------------

    #[test]
    fn extracts_a_route_with_the_controller_action_syntax() {
        let result = rails("config/routes.rb", "get '/users', to: 'users#index'\n");
        assert_eq!(names(&result), ["GET /users"]);
        assert_eq!(targets(&result), ["users#index"]);
    }

    #[test]
    fn extracts_a_route_without_the_to_keyword() {
        let result = rails("config/routes.rb", "post '/items' => 'items#create'\n");
        assert_eq!(targets(&result), ["items#create"]);
    }

    #[test]
    fn skips_begin_end_and_hash_commented_routes() {
        let src = "\n# get '/fake', to: 'fake#index'\n=begin\nget '/also-fake', to: 'fake#show'\n=end\nget '/real', to: 'real#index'\n";
        let result = rails("config/routes.rb", src);
        assert_eq!(names(&result), ["GET /real"]);
        assert_eq!(targets(&result), ["real#index"]);
        // The same in a routes file read with its blocks.
        assert_eq!(drawn(src), pairs(&[("GET /real", "real#index")]));
    }

    // -- the flat reading ---------------------------------------------------

    #[test]
    fn a_flat_route_has_upstreams_id_name_qualified_name_and_columns() {
        let route = "get \"/ping\", to: \"health#ping\"";
        let result = rails("lib/api.rb", &format!("x = 1\n  {route}\n"));
        let node = &result.nodes[0];
        assert_eq!(node.id, "route:lib/api.rb:2:GET:/ping");
        assert_eq!(node.name, "GET /ping");
        assert_eq!(node.qualified_name, "lib/api.rb::route:/ping");
        assert_eq!(node.kind, NodeKind::Route);
        assert_eq!(node.language, Language::Ruby);
        assert_eq!(
            (
                node.start_line,
                node.end_line,
                node.start_column,
                node.end_column
            ),
            (2, 2, 0, route.len() as i64)
        );
        let reference = &result.references[0];
        assert_eq!(reference.from_node_id, node.id);
        assert_eq!(reference.reference_name, "health#ping");
        assert_eq!(reference.reference_kind, EdgeKind::References);
        assert_eq!((reference.line, reference.column), (2, 0));
        assert_eq!(reference.language, Language::Ruby);
        assert_eq!(reference.file_path, "lib/api.rb");
    }

    #[test]
    fn a_flat_match_across_a_comment_measures_utf16_units() {
        // The comment between the path and `to:` is blanked; upstream's match
        // is as long as the source in UTF-16 units, the emoji two of them.
        let route = "get '/a', # \u{1F600} note\n  to: 'x#y'";
        let result = rails("lib/x.rb", &format!("{route}\n"));
        assert_eq!(names(&result), ["GET /a"]);
        assert_eq!(targets(&result), ["x#y"]);
        let units = route.encode_utf16().count() as i64;
        assert_ne!(units, route.len() as i64);
        assert_eq!(result.nodes[0].end_column, units);
        assert_eq!(result.nodes[0].start_line, 1);
    }

    #[test]
    fn a_resources_line_outside_a_routes_file_draws_its_restful_routes() {
        let src =
            "resources :photos\nresource :profile, only: [:show, :edit]\nget \"/x\", to: \"a#b\"\n";
        let result = rails("lib/routing.rb", src);
        // The explicit routes come first, then the resources'.
        assert_eq!(
            names(&result),
            [
                "GET /x",
                "GET /photos",
                "POST /photos",
                "GET /photos/new",
                "GET /photos/:id",
                "GET /photos/:id/edit",
                "PATCH /photos/:id",
                "DELETE /photos/:id",
                // A singular resource's paths are upstream's, written as a
                // plural's.
                "GET /profile/:id",
                "GET /profile/:id/edit",
            ]
        );
        assert_eq!(
            targets(&result),
            [
                "a#b",
                "photos#index",
                "photos#create",
                "photos#new",
                "photos#show",
                "photos#edit",
                "photos#update",
                "photos#destroy",
                "profiles#show",
                "profiles#edit",
            ]
        );
        let photos = &result.nodes[1];
        assert_eq!(photos.id, "route:lib/routing.rb:1:GET:photos#index");
        assert_eq!(photos.qualified_name, "lib/routing.rb::route:photos#index");
        assert_eq!(photos.end_column, "resources :photos".len() as i64);
        assert_eq!(
            result.nodes[8].id,
            "route:lib/routing.rb:2:GET:profiles#show"
        );
        assert_eq!(
            result.nodes[8].end_column,
            "resource :profile, only: [:show, :edit]".len() as i64
        );
    }

    #[test]
    fn reads_only_and_except_in_every_form_rails_accepts() {
        let plural = |tail: &str| resource_actions(true, tail);
        assert_eq!(plural(", only: [:index, :show]"), ["index", "show"]);
        assert_eq!(plural(", only: [\"index\", 'show', ]"), ["index", "show"]);
        assert_eq!(
            plural(", only: %i[new create index]"),
            ["index", "create", "new"]
        );
        assert_eq!(plural(", only: %I( show )"), ["show"]);
        assert_eq!(
            plural(", except: %w(destroy edit update)"),
            ["index", "create", "new", "show"]
        );
        assert_eq!(
            plural(", except: %W[destroy]"),
            ["index", "create", "new", "show", "edit", "update"]
        );
        assert_eq!(plural(", :only => [:index, :show]"), ["index", "show"]);
        assert_eq!(
            plural(", :except=>:destroy"),
            ["index", "create", "new", "show", "edit", "update"]
        );
        assert_eq!(plural(", only: :show"), ["show"]);
        assert_eq!(plural(", only: \"show\""), ["show"]);
        assert!(plural(", only: []").is_empty());
        // `only:` decides when both are written.
        assert_eq!(plural(", except: [:show], only: [:show]"), ["show"]);
        // A form it does not read keeps every action.
        assert_eq!(plural(", only: ACTIONS"), PLURAL_ACTIONS);
        assert_eq!(plural(""), PLURAL_ACTIONS);
        assert_eq!(resource_actions(false, ""), SINGULAR_ACTIONS);
        assert_eq!(resource_actions(false, ", only: [:index, :show]"), ["show"]);
    }

    #[test]
    fn a_file_under_config_routes_or_drawing_routes_is_read_with_its_blocks() {
        // A routes file's ids hold the path and the target.
        let scoped = rails("config/routes/admin.rb", "get \"/x\", to: \"a#b\"\n");
        assert_eq!(
            scoped.nodes[0].id,
            "route:config/routes/admin.rb:1:GET:/x:a#b"
        );
        assert_eq!(
            scoped.nodes[0].qualified_name,
            "config/routes/admin.rb::route:a#b"
        );
        assert_eq!(
            (scoped.nodes[0].start_column, scoped.nodes[0].end_column),
            (0, 0)
        );
        let drawn = rails(
            "engine/lib/routes.rb",
            "Engine.routes.draw do\n  get \"/x\", to: \"a#b\"\nend\n",
        );
        assert_eq!(drawn.nodes[0].id, "route:engine/lib/routes.rb:2:GET:/x:a#b");
        // `routes.draw` in a comment draws nothing.
        let flat = rails("lib/x.rb", "# routes.draw\nget \"/x\", to: \"a#b\"\n");
        assert_eq!(flat.nodes[0].id, "route:lib/x.rb:2:GET:/x");
        // Only `.rb` files.
        assert!(
            rails("lib/tasks/routes.rake", "get '/a', to: 'b#c'\n")
                .nodes
                .is_empty()
        );
        assert!(
            rails("config/routes/a.txt", "get '/a', to: 'b#c'\n")
                .nodes
                .is_empty()
        );
        // A file naming none of the needles is skipped.
        assert!(
            rails("app/models/user.rb", "class User\nend\n")
                .nodes
                .is_empty()
        );
    }

    // -- rails-scoped-routes.test.ts ----------------------------------------

    const ENGINE_ROUTES: &str = r#"Spree::Core::Engine.routes.draw do
  namespace :admin do
    get "/search/users", to: "search#users", as: :search_users
    resources :zones, only: [
      :index,
      :show
    ]
    resources :products do
      resources :images
      member do
        get :preview
      end
      collection do
        post :update_positions
      end
    end
  end
  scope "/v2", module: "api" do
    resource :account, only: [:show]
  end
  root to: "home#index"
end
"#;

    #[test]
    fn prefixes_namespaces_and_scopes_nests_resources_and_adds_member_and_collection_actions() {
        let result = rails("backend/config/routes.rb", ENGINE_ROUTES);
        let found: Vec<(String, String, i64)> = result
            .nodes
            .iter()
            .zip(&result.references)
            .map(|(node, reference)| {
                (
                    node.name.clone(),
                    reference.reference_name.clone(),
                    node.start_line,
                )
            })
            .collect();
        let expected: Vec<(String, String, i64)> = [
            ("GET /admin/search/users", "admin/search#users", 3),
            ("GET /admin/zones", "admin/zones#index", 4),
            ("GET /admin/zones/:id", "admin/zones#show", 4),
            ("GET /admin/products", "admin/products#index", 8),
            ("POST /admin/products", "admin/products#create", 8),
            ("GET /admin/products/new", "admin/products#new", 8),
            ("GET /admin/products/:id", "admin/products#show", 8),
            ("GET /admin/products/:id/edit", "admin/products#edit", 8),
            ("PATCH /admin/products/:id", "admin/products#update", 8),
            ("DELETE /admin/products/:id", "admin/products#destroy", 8),
            (
                "GET /admin/products/:product_id/images",
                "admin/images#index",
                9,
            ),
            (
                "POST /admin/products/:product_id/images",
                "admin/images#create",
                9,
            ),
            (
                "GET /admin/products/:product_id/images/new",
                "admin/images#new",
                9,
            ),
            (
                "GET /admin/products/:product_id/images/:id",
                "admin/images#show",
                9,
            ),
            (
                "GET /admin/products/:product_id/images/:id/edit",
                "admin/images#edit",
                9,
            ),
            (
                "PATCH /admin/products/:product_id/images/:id",
                "admin/images#update",
                9,
            ),
            (
                "DELETE /admin/products/:product_id/images/:id",
                "admin/images#destroy",
                9,
            ),
            (
                "GET /admin/products/:id/preview",
                "admin/products#preview",
                11,
            ),
            (
                "POST /admin/products/update_positions",
                "admin/products#update_positions",
                14,
            ),
            ("GET /v2/account", "api/accounts#show", 19),
            ("GET /", "home#index", 21),
        ]
        .iter()
        .map(|(name, target, line)| (name.to_string(), target.to_string(), *line))
        .collect();
        assert_eq!(found, expected);
        // `only:` written across lines still limits the actions.
        assert!(!names(&result).contains(&"DELETE /admin/zones/:id".to_string()));
    }

    #[test]
    fn namespace_and_scope_options() {
        assert_eq!(
            drawn(
                "  namespace :admin, path: \"manage\" do\n    resources :users, only: :index\n  end\n"
            ),
            pairs(&[("GET /manage/users", "admin/users#index")])
        );
        assert_eq!(
            drawn("  namespace \"api/v2\" do\n    get \"status\", to: \"status#show\"\n  end\n"),
            pairs(&[("GET /api/v2/status", "api/v2/status#show")])
        );
        assert_eq!(
            drawn("  scope path: \"/x\", module: :legacy do\n    get \"a\", to: \"a#b\"\n  end\n"),
            pairs(&[("GET /x/a", "legacy/a#b")])
        );
        assert_eq!(
            drawn("  scope(\"/y\") do\n    get \"a\", to: \"a#b\"\n  end\n"),
            pairs(&[("GET /y/a", "a#b")])
        );
        assert_eq!(
            drawn("  scope module: :admin do\n    get \"x\", to: \"y#z\"\n  end\n"),
            pairs(&[("GET /x", "admin/y#z")])
        );
        // A scope that opens no block scopes nothing.
        assert_eq!(
            drawn("  scope \"/z\"\n  get \"a\", to: \"a#b\"\n"),
            pairs(&[("GET /a", "a#b")])
        );
    }

    #[test]
    fn resource_options_and_nesting() {
        assert_eq!(
            drawn(
                "  resources :photos, path: \"images\", controller: \"pictures\", only: :index\n"
            ),
            pairs(&[("GET /images", "pictures#index")])
        );
        assert_eq!(
            drawn("  resources :categories do\n    resources :posts, only: :index\n  end\n"),
            pairs(&[
                ("GET /categories", "categories#index"),
                ("POST /categories", "categories#create"),
                ("GET /categories/new", "categories#new"),
                ("GET /categories/:id", "categories#show"),
                ("GET /categories/:id/edit", "categories#edit"),
                ("PATCH /categories/:id", "categories#update"),
                ("DELETE /categories/:id", "categories#destroy"),
                ("GET /categories/:category_id/posts", "posts#index"),
            ])
        );
        // A singular resource nests below its own path.
        assert_eq!(
            drawn(
                "  resource :profile, only: :show do\n    resources :photos, only: :index\n  end\n"
            ),
            pairs(&[
                ("GET /profile", "profiles#show"),
                ("GET /profile/photos", "photos#index"),
            ])
        );
    }

    #[test]
    fn member_collection_and_bare_actions() {
        assert_eq!(
            drawn(
                "  resources :posts, only: [] do\n    member do\n      get \"pre-view\", action: :preview_page\n      get \"sum-mary\"\n      get \"x\", to: \"other#y\"\n    end\n    collection do\n      get :search\n      get \"y\", to: \"other#z\"\n    end\n    get :feed\n  end\n"
            ),
            pairs(&[
                ("GET /posts/:id/pre-view", "posts#preview_page"),
                ("GET /posts/:id/sum-mary", "posts#sum_mary"),
                ("GET /posts/:id/x", "other#y"),
                ("GET /posts/search", "posts#search"),
                ("GET /posts/y", "other#z"),
                // Directly in the resource, as upstream reads it: the member
                // path.
                ("GET /posts/:id/feed", "posts#feed"),
            ])
        );
        // No resource around a bare action, or a member block outside one:
        // no route.
        assert!(drawn("  get :lonely\n  member do\n    get :preview\n  end\n").is_empty());
    }

    #[test]
    fn verbs_root_and_plain_blocks() {
        assert_eq!(
            drawn(
                "  match \"legacy\", to: \"old#show\", via: :all\n  get \"a\" => \"b#c\"\n  delete(\"d\", to: \"e#f\")\n"
            ),
            pairs(&[
                ("ANY /legacy", "old#show"),
                ("GET /a", "b#c"),
                ("DELETE /d", "e#f"),
            ])
        );
        assert_eq!(
            drawn(
                "  namespace :admin do\n    root \"dashboard#show\"\n  end\n  root 'home#index'\n"
            ),
            pairs(&[
                ("GET /admin", "admin/dashboard#show"),
                ("GET /", "home#index"),
            ])
        );
        // An `if` block and a `constraints … do` block keep their frame's
        // path; `end` closes them.
        assert_eq!(
            drawn(
                "  namespace :admin do\n    if Rails.env.development?\n      get \"dev\", to: \"dev#index\"\n    end\n    constraints(lambda { true }) do\n      get \"x\", to: \"y#z\"\n    end\n    get \"w\", to: \"y#w\"\n  end\n  get \"after\", to: \"a#b\"\n"
            ),
            pairs(&[
                ("GET /admin/dev", "admin/dev#index"),
                ("GET /admin/x", "admin/y#z"),
                ("GET /admin/w", "admin/y#w"),
                ("GET /after", "a#b"),
            ])
        );
        // A stray `end` leaves the file's frame.
        assert_eq!(
            drawn("  end\n  end\n  get \"a\", to: \"a#b\"\n"),
            pairs(&[("GET /a", "a#b")])
        );
    }

    #[test]
    fn a_statement_opening_a_block_takes_no_following_line() {
        // A `(` a block's opening line leaves open (here in a string) does
        // not join the block's body to the statement, as upstream's
        // continuation stops at a line ending in `do`.
        let found = rails(
            "config/routes.rb",
            "Rails.application.routes.draw do\n  scope \"(:locale\" do\n    get 'users', to: 'users#index'\n  end\nend\n",
        );
        assert_eq!(names(&found), ["GET /(:locale/users"]);
        assert_eq!(targets(&found), ["users#index"]);
    }

    #[test]
    fn a_route_id_is_emitted_once() {
        let mut routes = ScopedRoutes {
            file_path: "config/routes.rb",
            seen: HashSet::new(),
            result: FrameworkResolverExtractionResult::default(),
        };
        routes.emit("GET", "/a", "a#b".to_string(), 3);
        routes.emit("GET", "/a", "a#b".to_string(), 3);
        routes.emit("GET", "/a", "a#c".to_string(), 3);
        assert_eq!(routes.result.nodes.len(), 2);
        assert_eq!(routes.result.references.len(), 2);
        assert_eq!(
            routes.result.nodes[0].id,
            "route:config/routes.rb:3:GET:/a:a#b"
        );
    }

    // -- rails-only-except-forms.test.ts ------------------------------------

    #[test]
    fn reads_the_forms_of_a_routes_file() {
        let result = rails(
            "config/routes.rb",
            "Rails.application.routes.draw do\n  resources :family_exports, only: %i[new create index] do\n    member do\n      get :download\n    end\n  end\n  resources :tags, except: %w(destroy edit update)\n  resources :imports, :only => [:index, :show]\n  resource :session, only: \"show\"\nend\n",
        );
        let actions = |controller: &str| {
            let mut found: Vec<String> = result
                .nodes
                .iter()
                .filter_map(|node| {
                    node.qualified_name
                        .rsplit_once("::route:")
                        .and_then(|(_, target)| target.strip_prefix(&format!("{controller}#")))
                        .map(str::to_string)
                })
                .collect();
            found.sort();
            found
        };
        assert_eq!(
            actions("family_exports"),
            ["create", "download", "index", "new"]
        );
        assert_eq!(actions("tags"), ["create", "index", "new", "show"]);
        assert_eq!(actions("imports"), ["index", "show"]);
        assert_eq!(actions("sessions"), ["show"]);
    }

    // -- resolution ---------------------------------------------------------

    #[test]
    fn a_route_target_resolves_in_its_conventional_controller_file_first() {
        let file = "app/controllers/users_controller.rb";
        let mut nodes = controller(file, "UsersController", &["index", "show"]);
        nodes.push(ruby(NodeKind::Method, "index", file, 20));
        let other = "lib/users_controller.rb";
        let ctx = RouteCtx::default().routes(file, nodes).routes(
            other,
            controller(other, "UsersController", &["index", "edit"]),
        );
        // The first by line in the conventional file, whatever else holds one.
        assert_eq!(
            target(&ctx, "users#index"),
            Some((id(NodeKind::Method, "index", file, 3), 0.85))
        );
        // Not there: the only other controller class holding it.
        assert_eq!(
            target(&ctx, "users#edit"),
            Some((id(NodeKind::Method, "edit", other, 6), 0.85))
        );
        // A function counts as an action.
        let functions = "app/controllers/pages_controller.rb";
        let ctx = RouteCtx::default().routes(
            functions,
            vec![ruby(NodeKind::Function, "home", functions, 2)],
        );
        assert_eq!(
            target(&ctx, "pages#home"),
            Some((id(NodeKind::Function, "home", functions, 2), 0.85))
        );
    }

    #[test]
    fn a_route_target_falls_back_to_the_only_ruby_controller_class_holding_the_action() {
        let file = "rails/app/controllers/users_controller.rb";
        let ts = "nest/users.controller.ts";
        let ctx = RouteCtx::default()
            .routes(
                file,
                controller(file, "UsersController", &["index", "show"]),
            )
            .routes(
                ts,
                vec![
                    node(
                        NodeKind::Class,
                        "UsersController",
                        ts,
                        1,
                        Language::TypeScript,
                    ),
                    node(NodeKind::Method, "index", ts, 3, Language::TypeScript),
                ],
            );
        // The TypeScript class is no candidate (KEEP-RUST).
        assert_eq!(
            target(&ctx, "users#index"),
            Some((id(NodeKind::Method, "index", file, 3), 0.85))
        );
        assert_eq!(
            target(&ctx, "users#show"),
            Some((id(NodeKind::Method, "show", file, 6), 0.85))
        );
        assert_eq!(target(&ctx, "users#destroy"), None);
        assert_eq!(target(&ctx, "health#show"), None);
    }

    #[test]
    fn several_controller_files_holding_the_action_give_no_result() {
        let a = "engine_a/app/controllers/users_controller.rb";
        let b = "engine_b/app/controllers/users_controller.rb";
        let ctx = RouteCtx::default()
            .routes(a, controller(a, "UsersController", &["index"]))
            .routes(b, controller(b, "UsersController", &["index", "show"]));
        assert_eq!(target(&ctx, "users#index"), None);
        assert_eq!(
            target(&ctx, "users#show"),
            Some((id(NodeKind::Method, "show", b, 6), 0.85))
        );
        // Two class nodes in one file find its one action.
        let file = "app/lib/zones_controller.rb";
        let mut nodes = controller(file, "ZonesController", &["index"]);
        nodes.push(ruby(NodeKind::Class, "ZonesController", file, 30));
        let ctx = RouteCtx::default().routes(file, nodes);
        assert_eq!(
            target(&ctx, "zones#index"),
            Some((id(NodeKind::Method, "index", file, 3), 0.85))
        );
    }

    #[test]
    fn a_namespaced_route_reads_its_own_modules_controllers_when_there_are_any() {
        let admin = "backend/app/controllers/spree/admin/zones_controller.rb";
        let api = "api/app/controllers/spree/api/zones_controller.rb";
        let ctx = RouteCtx::default()
            .routes(admin, controller(admin, "ZonesController", &["show"]))
            .routes(api, controller(api, "ZonesController", &["index", "show"]));
        assert_eq!(
            target(&ctx, "admin/zones#show"),
            Some((id(NodeKind::Method, "show", admin, 3), 0.85))
        );
        // The admin controller inherits `index`; the API's is not it.
        assert_eq!(target(&ctx, "admin/zones#index"), None);
        // A module no file's path ends with reads every class of the name.
        assert_eq!(
            target(&ctx, "store/zones#index"),
            Some((id(NodeKind::Method, "index", api, 3), 0.85))
        );
        assert_eq!(target(&ctx, "store/zones#show"), None);
        assert_eq!(
            target(&ctx, "zones#index"),
            Some((id(NodeKind::Method, "index", api, 3), 0.85))
        );
    }

    #[test]
    fn a_model_resolves_in_app_models_then_concerns_then_by_name() {
        let ctx = RouteCtx::default()
            .routes(
                "app/models/user_profile.rb",
                vec![ruby(
                    NodeKind::Class,
                    "UserProfile",
                    "app/models/user_profile.rb",
                    2,
                )],
            )
            .routes(
                "app/models/concerns/taggable.rb",
                vec![ruby(
                    NodeKind::Class,
                    "Taggable",
                    "app/models/concerns/taggable.rb",
                    1,
                )],
            )
            .routes(
                "engine/app/models/user_profile.rb",
                vec![ruby(
                    NodeKind::Class,
                    "UserProfile",
                    "engine/app/models/user_profile.rb",
                    1,
                )],
            )
            .routes(
                "engine/app/models/taggable.rb",
                vec![ruby(
                    NodeKind::Class,
                    "Taggable",
                    "engine/app/models/taggable.rb",
                    1,
                )],
            )
            .routes(
                "engine/app/models/spree/order.rb",
                vec![ruby(
                    NodeKind::Class,
                    "Order",
                    "engine/app/models/spree/order.rb",
                    3,
                )],
            )
            .routes(
                "lib/order.rb",
                vec![ruby(NodeKind::Class, "Order", "lib/order.rb", 1)],
            )
            .routes(
                "a/app/models/line.rb",
                vec![ruby(NodeKind::Class, "Line", "a/app/models/line.rb", 1)],
            )
            .routes(
                "b/app/models/line.rb",
                vec![ruby(NodeKind::Class, "Line", "b/app/models/line.rb", 1)],
            )
            .routes(
                "web/app/models/item.ts",
                vec![node(
                    NodeKind::Class,
                    "Item",
                    "web/app/models/item.ts",
                    1,
                    Language::TypeScript,
                )],
            );
        // The conventional files answer before the classes of the name in
        // a path holding `app/models/`, of which there are two here.
        assert_eq!(
            target(&ctx, "UserProfile"),
            Some((
                id(
                    NodeKind::Class,
                    "UserProfile",
                    "app/models/user_profile.rb",
                    2
                ),
                0.8
            ))
        );
        assert_eq!(
            target(&ctx, "Taggable"),
            Some((
                id(
                    NodeKind::Class,
                    "Taggable",
                    "app/models/concerns/taggable.rb",
                    1
                ),
                0.8
            ))
        );
        assert_eq!(
            target(&ctx, "Order"),
            Some((
                id(
                    NodeKind::Class,
                    "Order",
                    "engine/app/models/spree/order.rb",
                    3
                ),
                0.8
            ))
        );
        // Several by name (KEEP-RUST), or none Ruby's: no result.
        assert_eq!(target(&ctx, "Line"), None);
        assert_eq!(target(&ctx, "Item"), None);
    }

    #[test]
    fn controllers_helpers_services_and_jobs_resolve_by_convention() {
        let ctx = RouteCtx::default()
            .routes(
                "app/controllers/admin_controller.rb",
                vec![ruby(
                    NodeKind::Class,
                    "AdminController",
                    "app/controllers/admin_controller.rb",
                    1,
                )],
            )
            .routes(
                "app/controllers/api/v1/widgets_controller.rb",
                vec![ruby(
                    NodeKind::Class,
                    "WidgetsController",
                    "app/controllers/api/v1/widgets_controller.rb",
                    2,
                )],
            )
            .routes(
                "app/controllers/api/sessions_controller.rb",
                vec![ruby(
                    NodeKind::Class,
                    "SessionsController",
                    "app/controllers/api/sessions_controller.rb",
                    3,
                )],
            )
            .routes(
                "engine/app/controllers/admin_controller.rb",
                vec![ruby(
                    NodeKind::Class,
                    "AdminController",
                    "engine/app/controllers/admin_controller.rb",
                    1,
                )],
            )
            .routes(
                "engine/app/controllers/sessions_controller.rb",
                vec![ruby(
                    NodeKind::Class,
                    "SessionsController",
                    "engine/app/controllers/sessions_controller.rb",
                    1,
                )],
            )
            .routes(
                "engine/app/controllers/widgets_controller.rb",
                vec![ruby(
                    NodeKind::Class,
                    "WidgetsController",
                    "engine/app/controllers/widgets_controller.rb",
                    1,
                )],
            )
            .routes(
                "engine/app/controllers/spree/zones_controller.rb",
                vec![ruby(
                    NodeKind::Class,
                    "ZonesController",
                    "engine/app/controllers/spree/zones_controller.rb",
                    4,
                )],
            )
            .routes(
                "a/controllers/pages_controller.rb",
                vec![ruby(
                    NodeKind::Class,
                    "PagesController",
                    "a/controllers/pages_controller.rb",
                    1,
                )],
            )
            .routes(
                "b/controllers/pages_controller.rb",
                vec![ruby(
                    NodeKind::Class,
                    "PagesController",
                    "b/controllers/pages_controller.rb",
                    1,
                )],
            )
            .routes(
                "app/helpers/users_helper.rb",
                vec![
                    ruby(
                        NodeKind::Class,
                        "UsersHelper",
                        "app/helpers/users_helper.rb",
                        1,
                    ),
                    ruby(
                        NodeKind::Module,
                        "UsersHelper",
                        "app/helpers/users_helper.rb",
                        5,
                    ),
                ],
            )
            .routes(
                "app/services/sync_service.rb",
                vec![ruby(
                    NodeKind::Class,
                    "SyncService",
                    "app/services/sync_service.rb",
                    1,
                )],
            )
            .routes(
                "app/jobs/cleanup_job.rb",
                vec![ruby(
                    NodeKind::Class,
                    "CleanupJob",
                    "app/jobs/cleanup_job.rb",
                    1,
                )],
            )
            .routes(
                "app/workers/mail_job.rb",
                vec![ruby(
                    NodeKind::Class,
                    "MailJob",
                    "app/workers/mail_job.rb",
                    1,
                )],
            );
        // The conventional files answer before the classes of the name in
        // a path holding `controllers/`, of which there are two for each.
        assert_eq!(
            target(&ctx, "AdminController"),
            Some((
                id(
                    NodeKind::Class,
                    "AdminController",
                    "app/controllers/admin_controller.rb",
                    1
                ),
                0.85
            ))
        );
        assert_eq!(
            target(&ctx, "WidgetsController"),
            Some((
                id(
                    NodeKind::Class,
                    "WidgetsController",
                    "app/controllers/api/v1/widgets_controller.rb",
                    2
                ),
                0.85
            ))
        );
        assert_eq!(
            target(&ctx, "SessionsController"),
            Some((
                id(
                    NodeKind::Class,
                    "SessionsController",
                    "app/controllers/api/sessions_controller.rb",
                    3
                ),
                0.85
            ))
        );
        assert_eq!(
            target(&ctx, "ZonesController"),
            Some((
                id(
                    NodeKind::Class,
                    "ZonesController",
                    "engine/app/controllers/spree/zones_controller.rb",
                    4
                ),
                0.85
            ))
        );
        assert_eq!(target(&ctx, "PagesController"), None);
        // A helper is a module.
        assert_eq!(
            target(&ctx, "UsersHelper"),
            Some((
                id(
                    NodeKind::Module,
                    "UsersHelper",
                    "app/helpers/users_helper.rb",
                    5
                ),
                0.8
            ))
        );
        assert_eq!(
            target(&ctx, "SyncService"),
            Some((
                id(
                    NodeKind::Class,
                    "SyncService",
                    "app/services/sync_service.rb",
                    1
                ),
                0.8
            ))
        );
        assert_eq!(
            target(&ctx, "CleanupJob"),
            Some((
                id(NodeKind::Class, "CleanupJob", "app/jobs/cleanup_job.rb", 1),
                0.8
            ))
        );
        assert_eq!(
            target(&ctx, "MailJob"),
            Some((
                id(NodeKind::Class, "MailJob", "app/workers/mail_job.rb", 1),
                0.8
            ))
        );
        assert_eq!(target(&ctx, "MissingService"), None);
        assert_eq!(target(&ctx, "Nothing"), None);
    }

    // -- helpers ------------------------------------------------------------

    #[test]
    fn inflects_names_the_way_upstream_does() {
        assert_eq!(pluralize("category"), "categories");
        assert_eq!(pluralize("day"), "days");
        assert_eq!(pluralize("box"), "boxes");
        assert_eq!(pluralize("bus"), "buses");
        assert_eq!(pluralize("match"), "matches");
        assert_eq!(pluralize("wish"), "wishes");
        assert_eq!(pluralize("quiz"), "quizes");
        assert_eq!(pluralize("user"), "users");
        assert_eq!(pluralize("y"), "ys");
        assert_eq!(singularize("categories"), "category");
        assert_eq!(singularize("addresses"), "address");
        assert_eq!(singularize("status"), "status");
        assert_eq!(singularize("class"), "class");
        // Upstream's `(x|ch|sh|ses)$` with `es$` holds for `ses` only.
        assert_eq!(singularize("boxes"), "boxe");
        assert_eq!(singularize("users"), "user");
        assert_eq!(singularize("sheep"), "sheep");
        assert_eq!(camelize("user_profiles"), "UserProfiles");
        assert_eq!(camelize("zones"), "Zones");
        assert_eq!(camelize("a__b"), "AB");
        assert_eq!(camelize(""), "");
        assert_eq!(controller_class_name("admin/zones"), "ZonesController");
        assert_eq!(
            controller_class_name("user_profiles"),
            "UserProfilesController"
        );
        assert_eq!(
            controller_file("admin/zones"),
            "app/controllers/admin/zones_controller.rb"
        );
        assert_eq!(
            route_target("admin/zones#show"),
            Some(("admin/zones", "show"))
        );
        assert_eq!(route_target("Admin::Zones#show"), None);
        assert_eq!(snake("UserProfile").as_deref(), Some("user_profile"));
        assert_eq!(snake("HTMLParser").as_deref(), Some("h_t_m_l_parser"));
        assert_eq!(
            snake("Admin::UsersController").as_deref(),
            Some("admin::_users_controller")
        );
        assert_eq!(snake("\u{c4}Controller").as_deref(), Some("_controller"));
        assert_eq!(snake("\u{1D49C}Controller"), None);
    }

    #[test]
    fn joins_paths_and_reads_symbols_the_way_upstream_does() {
        assert_eq!(join(&["", "users"]), "/users");
        assert_eq!(join(&["/admin", "/search/users"]), "/admin/search/users");
        assert_eq!(join(&["", ""]), "/");
        assert_eq!(join(&["/a/", ""]), "/a");
        assert_eq!(join(&["/admin"]), "/admin");
        assert_eq!(join(&["a\n", ""]), "/a\n/");
        assert_eq!(sym(":admin"), "admin");
        assert_eq!(sym("\"a\""), "a");
        assert_eq!(sym("'a\""), "a");
        assert_eq!(sym("\""), "");
        assert_eq!(sym("''"), "");
        assert_eq!(sym("::x"), ":x");
        assert_eq!(open_brackets("only: [:a, (b"), 2);
        assert_eq!(open_brackets("x]"), -1);
    }
}
