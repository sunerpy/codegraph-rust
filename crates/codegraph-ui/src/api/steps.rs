//! `GET /api/steps` — what happens from here: a screen, a handler or any
//! symbol as the ANCHOR, and everything it sets in motion drawn as typed
//! steps. Upstream `src/ui-server/api/steps.ts` (`e8ddc4e2`).
//!
//! Walking FORWARD from the anchor over calls, renders, handler bindings and
//! navigations, a node is a step when it is a **screen** (a route reached
//! over `navigates`), a **trigger** (a function wired as a value, or called
//! from under an event binding), a **bridge** call (the language changes
//! under the call, JS → native, or a synthesized channel crosses to the
//! server), a native **event** landing back in JS, a **store** action (a
//! function in a store file), or an **effect** (a call that leaves the index
//! into the network, storage, the database, a response…, drawn as its own
//! box beside the function that makes it). Everything else is `via`: listed
//! on the link, never a box. The conditions along the folded chain join into
//! the link's `when`, read from the source at request time.
//!
//! The picture is finite because it is anchored and capped: a bounded depth
//! in steps, fan-out per node and folded nodes per step; hubs and shared
//! chrome are dead ends. Every cap that fired is reported where it fired.
//!
//! The same walk is also read in the code's order ([`super::program`]): the
//! anchor's body as a rail that forks where the code forks.
//!
//! Where this differs from upstream: upstream reads the name a call was
//! written with off a `calls` edge's metadata (`refName`); this index keeps
//! it on the resolved reference (`resolved_refs`), read per layer of the walk
//! by site, the name whose last segment is the target's when a site holds
//! several, the target's name when none or several do. Every list follows a
//! total order — a layer's edges by line, target, kind and column, names by
//! [`locale_compare`] — where upstream takes SQLite's row order for ties.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::Instant;

use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind, UnresolvedRef};
use codegraph_graph::branch_guards::{BranchGuard, GuardForm, guard_label};
use codegraph_graph::call_sites::{SiteLoop, TriggerKind};
use codegraph_graph::collation::locale_compare;
use codegraph_resolve::frameworks::nextjs::{NextRouteKind, next_route_for_file};
use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::sync::OnceLock;

use super::Ctx;
use super::effects::{
    CallKind, Effect, EffectCategory, EffectInput, ProjectKind, classify_effect,
    implicit_response_status, response_status,
};
use super::program::{
    ProgramInput, ProgramSite, ProgramTrigger, SiteAt, SitePosition, StepFacts, WireProgram,
    build_program,
};
use super::route_roots::{RouteRoot, looks_like_component, route_roots};
use super::routes::split_route_name;
use super::when::SiteReader;
use super::wire::{HUB_THRESHOLD, UNCERTAIN_BELOW, WireNodeRef, is_test_path, to_node_ref};
use crate::respond::{ApiError, ApiResult, Query};

// =============================================================================
// Wire shapes (upstream `WireStepsPayload`)
// =============================================================================

/// One arm of one decision, as the site that runs under it records it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WireStepDecision {
    /// Where the branching construct starts (`line:column`).
    pub branch: String,
    /// The decision as a reader says it, always positive.
    pub on: String,
    /// THIS arm's own condition.
    pub arm: String,
    pub form: &'static str,
    #[serde(skip_serializing_if = "is_false")]
    pub not: bool,
}

/// What fires a step or a link: the event it is written under, and the
/// function that writes it there.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WireStepTrigger {
    /// `prop` / `option` / `callback` are read at the site; `request`,
    /// `decorator` and `load` are set from the registration.
    pub kind: &'static str,
    pub name: String,
    pub of: Option<String>,
    /// What runs before it fires: the middleware / guard chain, in order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<Vec<String>>,
    #[serde(rename = "in")]
    pub in_: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WireStepSite {
    pub file: String,
    pub line: i64,
    /// `push /capture`, `calls x`, `client.post` — what the site does.
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<String>,
    /// The conditions THIS site runs under — the whole chain's, joined.
    pub when: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<WireStepTrigger>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<WireStepDecision>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WireStepRegion {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WireStepScreen {
    pub path: String,
    pub component: Option<WireNodeRef>,
    pub endpoint: bool,
    pub inline: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WireStepEffect {
    pub api: String,
    pub apis: Vec<String>,
    pub category: &'static str,
    pub by: WireNodeRef,
    pub line: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<super::effects::Access>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statuses: Option<Vec<u16>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WireStep {
    /// The node's id, or `effect:<function id>:<category>[:<status>]`.
    pub id: String,
    pub kind: &'static str,
    pub anchor: bool,
    /// `None` only for an effect, which is a call site rather than a symbol.
    pub node: Option<WireNodeRef>,
    pub label: String,
    pub sub: String,
    pub depth: usize,
    /// Why the walk did not go on from here: `depth`, `fan-out`, `folded`,
    /// `steps`, `screen` or `component`.
    pub cut: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<WireStepTrigger>,
    pub order: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<WireStepRegion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen: Option<WireStepScreen>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effect: Option<WireStepEffect>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WireStepLink {
    pub id: String,
    pub from: String,
    pub to: String,
    pub kind: &'static str,
    /// The symbols folded between the two steps, in order.
    pub via: Vec<WireNodeRef>,
    pub when: String,
    /// How the last hop was established when it was not a plain call.
    pub label: String,
    /// The call the first hop is written inside the arguments of.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub within: Option<String>,
    pub synthesized: bool,
    pub uncertain: bool,
    pub sites: Vec<WireStepSite>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<WireStepTrigger>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

// =============================================================================
// Caps
// =============================================================================

pub const DEFAULT_DEPTH: i64 = 8;
pub const MAX_DEPTH: i64 = 14;
pub const DEFAULT_LIMIT: i64 = 120;
pub const MAX_LIMIT: i64 = 400;
/// Nodes folded while exploring from ONE step before the walk stops.
const MAX_FOLDED_PER_STEP: usize = 300;
/// Hops of folded plumbing between two steps.
const MAX_FOLD_DEPTH: usize = 7;
/// Outgoing edges followed from one node.
const MAX_FANOUT: usize = 80;
/// Unresolved-reference scans (for effects) per request.
const MAX_EFFECT_SCANS: usize = 800;
/// Call sites read for conditions and arguments per request.
const MAX_WHEN_SITES: usize = 1600;
/// Call sites read for the callee as written per request.
const MAX_CALL_SITES: usize = 4000;
/// Longest effect-box label before its argument list is cut.
const MAX_EFFECT_LABEL: usize = 56;
/// A component rendered by this many distinct parents is chrome.
const SHARED_CHROME_MIN: usize = 5;

/// Edges walked forward: `contains` only function → function, `references`
/// only function-as-value.
const WALK_KINDS: &[EdgeKind] = &[
    EdgeKind::Calls,
    EdgeKind::Instantiates,
    EdgeKind::Navigates,
    EdgeKind::References,
    EdgeKind::Contains,
];

// =============================================================================
// Classification
// =============================================================================

fn is_js_family(language: Language) -> bool {
    matches!(
        language,
        Language::JavaScript | Language::TypeScript | Language::Tsx | Language::Jsx
    )
}

fn is_native_family(language: Language) -> bool {
    matches!(
        language,
        Language::Swift | Language::ObjC | Language::Java | Language::Kotlin
    )
}

fn meta_str<'m>(meta: &'m Map<String, Value>, key: &str) -> Option<&'m str> {
    meta.get(key).and_then(Value::as_str)
}

fn meta_true(meta: &Map<String, Value>, key: &str) -> bool {
    meta.get(key).and_then(Value::as_bool) == Some(true)
}

/// JS → native is a bridge call; native → JS is an event; unless the edge
/// itself says which way it crosses.
pub fn crossing(from: Language, to: Language, meta: &Map<String, Value>) -> Option<&'static str> {
    match meta_str(meta, "tier") {
        Some("client→server") => return Some("bridge"),
        Some("server→client") => return Some("event"),
        _ => {}
    }
    if matches!(
        meta_str(meta, "channel"),
        Some("queue" | "event" | "socket")
    ) {
        return Some("event");
    }
    if is_js_family(from) && is_native_family(to) {
        return Some("bridge");
    }
    if is_native_family(from) && is_js_family(to) {
        return Some("event");
    }
    None
}

/// A file that holds state: a store, a slice, a reducer (upstream
/// `STORE_FILE`, case-insensitive).
pub fn is_store_file(file: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)(?:^|/)(?:stores?|storage|state|slices?|reducers?)/|\.(?:store|storage|slice|reducer)\.[cm]?[jt]sx?$")
            .expect("valid regex")
    })
    .is_match(&file.replace('\\', "/"))
}

/// What a call is when it leaves the index, by the reference text alone.
pub fn effect_category(reference_name: &str) -> Option<&'static str> {
    classify_effect(&EffectInput {
        text: reference_name,
        kind: CallKind::Calls,
        language: None,
        project: ProjectKind::App,
        receiver_type: None,
        args: None,
        project_type: false,
    })
    .map(|effect| effect.category.as_str())
}

fn repository_container(name: &str) -> bool {
    [
        "Repository",
        "Repositories",
        "Repo",
        "Dao",
        "DAO",
        "Mapper",
        "Store",
        "Datastore",
    ]
    .iter()
    .any(|suffix| name.ends_with(suffix))
}

/// Decorators that gate a handler (upstream `GUARD_DECORATOR`, `/i`).
fn guard_decorator() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^(?:UseGuards|UseInterceptors|UsePipes|UseFilters|Roles|Auth|Public|Permissions|Throttle|SkipThrottle|Authorize|AllowAnonymous|PreAuthorize|PostAuthorize|Secured|RolesAllowed|PermitAll|DenyAll|Transactional|Validated|login_required|permission_required|user_passes_test|staff_member_required|require_http_methods|require_POST|require_GET|csrf_exempt|csrf_protect|ratelimit|throttle_classes|permission_classes|authentication_classes|cache_page|ValidateAntiForgeryToken|RequireAuthorization|RequireRole|RequireHttps|EnableCors|CrossOrigin|Cacheable|CacheEvict|CachePut|RateLimiter|CircuitBreaker|Retry|Timeout|Bulkhead|jwt_required|Security|ApiBearerAuth|ApiKeyAuth|BearerAuth|OAuth|Scopes|Roles|HasRole|HasPermission|Idempotent|Lock|Locked|Retryable|Recover)$|Guard|Interceptor|Pipe$|Filter$|Auth|Role|Permission|Throttle|Valid|Transaction|Csrf|Limit")
            .expect("valid regex")
    })
}

/// Decorators that ARE the route, the DI wiring or documentation.
fn not_a_guard() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(?:Get|Post|Put|Patch|Delete|Head|Options|All|Controller|RestController|Resolver|Query|Mutation|Subscription|Injectable|Module|Api[0-9A-Za-z_]*|Http(?:Get|Post|Put|Patch|Delete|Head|Options)|Route|RequestMapping|[0-9A-Za-z_]+Mapping|Component|Service|Repository|Bean|Autowired|Override|Inject|Param|Body|Res|Req|Headers|Ip|HostParam|Session|UploadedFiles?|HttpCode|Header|Redirect|Render|Version|SerializeOptions|ResponseBody|ResponseStatus|Produces|Consumes|FromBody|FromRoute|FromQuery|FromForm|FromHeader|FromServices|Path|PathVariable|RequestParam|RequestBody|RequestHeader|ModelAttribute|Valid|Args|Context|Parent|Info|Field|ObjectType|InputType|ArgsType|Entity|Column|PrimaryGeneratedColumn|OneToMany|ManyToOne|Prop|Schema|Type|Expose|Exclude|Transform|IsString|IsNumber|IsOptional|Length|Min|Max|Deprecated|SuppressWarnings|FunctionalInterface|Slf4j|Data|Builder|Getter|Setter|NoArgsConstructor|AllArgsConstructor|RequiredArgsConstructor|Value|ConfigurationProperties|Configuration|EnableScheduling|SpringBootApplication|Profile|Order|Primary|Qualifier|Lazy|Scope|JsonProperty|JsonIgnore|Nullable|NonNull|NotNull|Size|Pattern|Email|Positive|router\.[0-9A-Za-z_]+|app\.[0-9A-Za-z_]+|api\.[0-9A-Za-z_]+|bp\.[0-9A-Za-z_]+|blueprint\.[0-9A-Za-z_]+|[0-9A-Za-z_]+\.(?:route|get|post|put|patch|delete))$")
            .expect("valid regex")
    })
}

/// Decorators that fire a function from outside a request.
fn consumer_decorator() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(?:Process|Processor|OnEvent|OnQueueEvent|OnWorkerEvent|OnGlobalQueueEvent|Cron|Interval|Timeout|MessagePattern|EventPattern|SubscribeMessage|Scheduled|Schedules?|KafkaListener|RabbitListener|RabbitSubscribe|RabbitRPC|JmsListener|SqsListener|SqsMessageHandler|EventListener|TransactionalEventListener|StreamListener|ServiceActivator|receiver|shared_task|task|periodic_task|app\.task|celery\.task|on|hears|command|event|listen|listener|Consume|Consumer|Subscribe|Subscriber|CapSubscribe|Function|FunctionName|TimerTrigger|QueueTrigger|ServiceBusTrigger|EventGridTrigger|BlobTrigger|CosmosDBTrigger|Job|job|Worker|worker|EventHandler|CommandHandler|QueryHandler|OnMessage|MessageHandler|GrpcMethod|GrpcStreamMethod|WebSocketGateway|dramatiq\.actor|actor|huey\.task|db_task|Signal|signal|hook|Hook|OnModuleInit|OnApplicationBootstrap|PostConstruct|PreDestroy|Bean|Startup|Shutdown)$")
            .expect("valid regex")
    })
}

/// The name of a decorator, before its arguments.
fn decorator_name(text: &str) -> &str {
    text.split('(').next().unwrap_or(text).trim()
}

/// The first string literal in a decorator's arguments — `'email'` of
/// `@Process('email')` (upstream's `\(\s*(['"`])((?:(?!\1).)*)\1`, by hand).
fn decorator_literal(text: &str) -> Option<String> {
    text.match_indices('(').find_map(|(open, _)| {
        let rest = text[open + 1..].trim_start();
        let quote = rest
            .chars()
            .next()
            .filter(|ch| matches!(ch, '\'' | '"' | '`'))?;
        let body = &rest[1..];
        let end = body.find(['\n', quote])?;
        body[end..]
            .starts_with(quote)
            .then(|| format!("'{}'", &body[..end]))
    })
}

fn is_guard_decorator(text: &str) -> bool {
    let name = decorator_name(text);
    !not_a_guard().is_match(name) && guard_decorator().is_match(name)
}

/// FastAPI: `dependencies=[Depends(auth), Depends(rate_limit)]` inside the
/// route decorator, split on the commas outside parentheses.
fn dependencies_in(text: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let Some(found) = RE
        .get_or_init(|| Regex::new(r"dependencies\s*=\s*\[([^\]]*)\]").expect("valid regex"))
        .captures(text)
    else {
        return Vec::new();
    };
    let inner = &found[1];
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    for ch in inner.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        // `,(?![^()]*\))`: a comma not followed by a `)` before the next `(`.
        if ch == ',' && depth == 0 {
            out.push(current.trim().to_string());
            current.clear();
            continue;
        }
        current.push(ch);
    }
    out.push(current.trim().to_string());
    out.into_iter().filter(|part| !part.is_empty()).collect()
}

/// A trigger an edge carries in its metadata (a template binding).
fn site_trigger_of(meta: &Map<String, Value>) -> Option<(String, Option<String>)> {
    let trigger = meta.get("trigger")?.as_object()?;
    if trigger.get("kind").and_then(Value::as_str) != Some("prop") {
        return None;
    }
    let name = trigger.get("name")?.as_str()?.to_string();
    let of = trigger
        .get("of")
        .and_then(Value::as_str)
        .map(str::to_string);
    Some((name, of))
}

/// The string a Java / Kotlin return expression builds, when it starts with
/// a literal: `"owners/ownerDetails"`, `"redirect:/owners/" + owner.getId()`
/// → `redirect:/owners/${…}`.
fn spring_view_string(expression: &str) -> Option<String> {
    static LITERAL: OnceLock<Regex> = OnceLock::new();
    static HOLE: OnceLock<Regex> = OnceLock::new();
    let literal =
        LITERAL.get_or_init(|| Regex::new(r#"^"((?:[^"\\]|\\.)*)""#).expect("valid regex"));
    let hole =
        HOLE.get_or_init(|| Regex::new(r"\$\{[^}]*\}|\$[0-9A-Za-z_]+").expect("valid regex"));
    let mut out = String::new();
    let mut rest = expression.trim();
    let mut first = true;
    while !rest.is_empty() {
        if let Some(found) = literal.captures(rest) {
            out.push_str(&hole.replace_all(&found[1], "${…}"));
            rest = rest[found[0].len()..].trim();
        } else {
            if first {
                return None;
            }
            let operand = rest.find('+').filter(|at| *at > 0).unwrap_or(rest.len());
            out.push_str("${…}");
            rest = rest[operand..].trim();
        }
        first = false;
        match rest.strip_prefix('+') {
            Some(after) => rest = after.trim(),
            None => break,
        }
    }
    (!first).then_some(out)
}

fn posix(path: &str) -> String {
    path.replace('\\', "/")
}

fn basename(path: &str) -> String {
    let posix = posix(path);
    posix.rsplit('/').next().unwrap_or(&posix).to_string()
}

/// The last `.`- or `:`-separated piece of a name, argument lists dropped.
fn last_segment(name: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let bare = RE
        .get_or_init(|| Regex::new(r"\([^()]*\)").expect("valid regex"))
        .replace_all(name, "");
    bare.rsplit(['.', ':']).next().unwrap_or(&bare).to_string()
}

// =============================================================================
// The walk's records
// =============================================================================

/// Where a step is first reached from its parent's root: the hop's position,
/// its call's span, and the call it is written inside.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HopSite {
    file: String,
    line: i64,
    column: i64,
    end: (i64, i64),
    within: Option<String>,
}

#[derive(Debug, Clone)]
struct Fold {
    node: Node,
    /// [first folded node, …, this node]; empty for the step's own root.
    chain: Vec<Node>,
    whens: Vec<String>,
    /// The hop out of the step's root this fold descends from.
    first: Option<HopSite>,
}

#[derive(Debug, Clone)]
struct StepRecord {
    step: WireStep,
    first: Option<HopSite>,
    /// Where exploration from this step begins.
    root: Option<Node>,
}

/// One arrival of a layer: an edge that reaches a step, or plumbing.
struct Arrival {
    edge: Edge,
    target: Node,
    meta: Map<String, Value>,
    site: WireStepSite,
    kind: Option<&'static str>,
    link_kind: &'static str,
    event: Option<String>,
    trigger: Option<WireStepTrigger>,
}

/// A span of a call that became an effect step, for the bindings written
/// inside its arguments.
struct FiredSpan {
    start: (i64, i64),
    end: (i64, i64),
    step: usize,
}

/// Source order of two hops: a hop written inside the other's call runs
/// first; else by position; another file sorts after.
fn hop_compare(a: Option<&HopSite>, b: Option<&HopSite>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (a, b) = match (a, b) {
        (Some(a), Some(b)) => (a, b),
        (Some(_), None) => return Ordering::Less,
        (None, Some(_)) => return Ordering::Greater,
        (None, None) => return Ordering::Equal,
    };
    if a.file != b.file {
        return locale_compare(&a.file, &b.file);
    }
    if hop_inside(a, b) {
        return Ordering::Less;
    }
    if hop_inside(b, a) {
        return Ordering::Greater;
    }
    a.line.cmp(&b.line).then(a.column.cmp(&b.column))
}

/// `x` starts strictly after `y` starts and before `y` ends.
fn hop_inside(x: &HopSite, y: &HopSite) -> bool {
    let after_start = x.line > y.line || (x.line == y.line && x.column > y.column);
    let before_end = x.line < y.end.0 || (x.line == y.end.0 && x.column < y.end.1);
    after_start && before_end
}

/// `push /capture`, `renders <Button>`, `via rn-event-channel`, `calls x`.
fn site_text(edge: &Edge, meta: &Map<String, Value>, target: &Node) -> String {
    if edge.kind == EdgeKind::Navigates {
        let method = if edge.provenance.as_deref() == Some("heuristic") {
            "returns".to_string()
        } else {
            meta_str(meta, "navMethod").unwrap_or("push").to_string()
        };
        let href = meta_str(meta, "href").unwrap_or(&target.name);
        return format!("{method} {href}");
    }
    if meta_str(meta, "synthesizedBy") == Some("jsx-render") {
        return format!("renders <{}>", target.name);
    }
    match edge.kind {
        EdgeKind::References => return format!("passes {}", target.name),
        EdgeKind::Contains => return format!("defines {}", target.name),
        EdgeKind::Instantiates => return format!("new {}", target.name),
        _ => {}
    }
    if meta_str(meta, "bridge") == Some("react-native") {
        let module = meta_str(meta, "module")
            .map(|module| format!("{module}."))
            .unwrap_or_default();
        return format!("bridge {module}{}", target.name);
    }
    if meta_str(meta, "channel") == Some("http") {
        return format!(
            "{} {}",
            meta_str(meta, "method").unwrap_or("GET"),
            meta_str(meta, "href").unwrap_or(&target.name)
        );
    }
    if let Some(by) = meta_str(meta, "synthesizedBy") {
        return format!("via {by}");
    }
    format!("calls {}", target.name)
}

/// The words on a hop that was not a plain call.
fn hop_label(meta: &Map<String, Value>, synthesized: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(by) = meta_str(meta, "synthesizedBy") {
        parts.push(format!("via {by}"));
    } else if synthesized {
        parts.push("inferred".to_string());
    }
    let channel = meta_str(meta, "channel");
    if channel == Some("server-action") {
        parts.push("server action".to_string());
    }
    if channel == Some("http")
        && let Some(method) = meta_str(meta, "method")
    {
        parts.push(
            format!("{method} {}", meta_str(meta, "href").unwrap_or(""))
                .trim()
                .to_string(),
        );
    }
    match meta_str(meta, "tier") {
        Some("client→server") => parts.push("to the server".to_string()),
        Some("server→client") => parts.push("from the server".to_string()),
        _ => {}
    }
    if meta_str(meta, "resolvedBy") == Some("receiver-type") {
        parts.push("by the receiver’s declared type".to_string());
    }
    if let Some(event) = meta_str(meta, "event") {
        let word = match channel {
            Some("queue") => "job",
            Some("socket") => "message",
            _ => "event",
        };
        parts.push(format!("{word} {event}"));
    }
    if let Some(queue) = meta_str(meta, "queue") {
        parts.push(format!("queue {queue}"));
    }
    if meta_str(meta, "bridge") == Some("react-native") {
        let module = meta_str(meta, "module")
            .map(|module| format!(" · {module}"))
            .unwrap_or_default();
        parts.push(format!("React Native bridge{module}"));
    }
    if let Some(at) = meta_str(meta, "registeredAt") {
        parts.push(format!("registered at {at}"));
    }
    parts.join(" · ")
}

/// What kind of project the picture is of, by what its routes are.
pub fn project_kind(routes: &[Node], navigates: i64) -> ProjectKind {
    let mut endpoints = 0usize;
    let mut pages = 0usize;
    for route in routes {
        if split_route_name(&route.name).0.is_some() {
            endpoints += 1;
        } else if route.name.starts_with('/') {
            pages += 1;
            if next_route_for_file(&route.file_path)
                .is_some_and(|file| file.kind == NextRouteKind::Page)
            {
                return ProjectKind::Web;
            }
        }
    }
    if endpoints == 0 {
        return ProjectKind::App;
    }
    if navigates > 0 || pages > 0 {
        ProjectKind::Web
    } else {
        ProjectKind::Api
    }
}

fn effect_sub(effect: &WireStepEffect, by: &Node) -> String {
    let access = effect.access.map(|access| match access {
        super::effects::Access::Read => "read",
        super::effects::Access::Write => "write",
    });
    [
        Some(effect.category),
        effect.model.as_deref(),
        access,
        Some(by.name.as_str()),
    ]
    .into_iter()
    .flatten()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" · ")
}

/// The decision a site's innermost condition belongs to.
fn decision_of(guards: &[BranchGuard]) -> Option<WireStepDecision> {
    let guard = guards.last()?;
    if guard.branch.is_empty() || guard.form == GuardForm::Guard {
        return None;
    }
    let mut plain = guard.clone();
    plain.negated = false;
    Some(WireStepDecision {
        branch: guard.branch.clone(),
        on: guard_label(std::slice::from_ref(&plain)),
        arm: guard_label(std::slice::from_ref(guard)),
        form: match guard.form {
            GuardForm::Case => "switch",
            GuardForm::Ternary => "ternary",
            GuardForm::Catch => "try",
            _ => "if",
        },
        not: guard.negated,
    })
}

fn trigger_kind_word(kind: TriggerKind) -> &'static str {
    kind.as_str()
}

/// The edge's metadata as an object (an edge without one has none).
fn meta_of(edge: &Edge) -> Map<String, Value> {
    edge.metadata
        .as_ref()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

// =============================================================================
// The endpoint
// =============================================================================

/// The anchor: `anchor=<id>`, or `symbol=<name>` resolved to the most
/// screen-like symbol of that name, the rest reported as ambiguous.
fn resolve_anchor(ctx: &Ctx<'_>, query: &Query) -> ApiResult<(Node, Vec<WireNodeRef>)> {
    if let Some(id) = query.get("anchor").filter(|id| !id.trim().is_empty()) {
        let Some(node) = ctx.store.node_by_id(id)? else {
            return Err(ApiError::not_found(
                format!("No symbol with id \"{id}\" in this index."),
                Some("It may have moved in a re-index; open it from search or the Screens view."),
            ));
        };
        return Ok((node, Vec::new()));
    }
    let name = query.get("symbol").unwrap_or("").trim();
    if name.is_empty() {
        return Err(ApiError::bad_request(
            "Give the picture an anchor: ?anchor=<node id> or ?symbol=<name>.",
        ));
    }
    let rank = |kind: NodeKind| match kind {
        NodeKind::Route => 0,
        NodeKind::Component => 1,
        NodeKind::Function => 2,
        NodeKind::Method => 3,
        NodeKind::Class => 4,
        NodeKind::Constant => 5,
        NodeKind::Variable => 6,
        _ => 9,
    };
    let mut matches: Vec<Node> = ctx
        .store
        .nodes_by_name(name)?
        .into_iter()
        .filter(|node| {
            !matches!(
                node.kind,
                NodeKind::File | NodeKind::Import | NodeKind::Export
            )
        })
        .collect();
    matches.sort_by(|a, b| {
        rank(a.kind)
            .cmp(&rank(b.kind))
            .then_with(|| locale_compare(&a.file_path, &b.file_path))
            .then(a.start_line.cmp(&b.start_line))
            .then_with(|| a.id.cmp(&b.id))
    });
    let Some(anchor) = matches.first().cloned() else {
        return Err(ApiError::not_found(
            format!("Nothing in this index is named \"{name}\"."),
            Some("Try the search box; names are matched exactly."),
        ));
    };
    let ambiguous = matches.iter().skip(1).take(8).map(to_node_ref).collect();
    Ok((anchor, ambiguous))
}

pub fn build(ctx: &Ctx<'_>, query: &Query) -> ApiResult<Value> {
    let started = Instant::now();
    let depth_cap = query.int("depth", 1, MAX_DEPTH, Some(DEFAULT_DEPTH))? as usize;
    let limit = query.int("limit", 20, MAX_LIMIT, Some(DEFAULT_LIMIT))? as usize;
    let through = query.flag("through");
    let counts = ctx.store.counts()?;
    let index = json!({
        "lastIndexedAt": ctx.store.last_indexed_at()?,
        "edges": counts.edge_count,
        "files": counts.file_count,
    });
    let (anchor, ambiguous) = resolve_anchor(ctx, query)?;
    let routes = ctx.store.nodes_of_kind_ordered(NodeKind::Route)?;
    let roots = route_roots(ctx, &routes)?;
    let navigates = ctx
        .store
        .edge_counts_by_kind()?
        .into_iter()
        .find(|(kind, _)| kind == "navigates")
        .map_or(0, |(_, count)| count);
    let project = project_kind(&routes, navigates);
    let mut walk = Walk {
        ctx,
        limit,
        depth_cap,
        through,
        project,
        roots,
        reader: SiteReader::new(ctx, MAX_WHEN_SITES),
        calls: SiteReader::new(ctx, MAX_CALL_SITES),
        steps: Vec::new(),
        step_index: HashMap::new(),
        links: Vec::new(),
        link_index: HashMap::new(),
        programs: Vec::new(),
        program_index: HashMap::new(),
        truncated_steps: 0,
        truncated_hubs: 0,
        truncated_chrome: 0,
        effect_scans: 0,
        fan_in: HashMap::new(),
        render_parents: HashMap::new(),
        file_scope_edges: HashMap::new(),
        file_scope_refs: HashMap::new(),
        fired_spans: HashMap::new(),
        file_types: HashMap::new(),
        class_types: HashMap::new(),
        class_by_name: HashMap::new(),
        project_types: HashMap::new(),
        regions: false,
    };
    let payload = walk.run(&anchor)?;
    let mut payload = payload;
    payload["anchor"] = serde_json::to_value(to_node_ref(&anchor)).unwrap_or(Value::Null);
    payload["ambiguous"] = serde_json::to_value(ambiguous).unwrap_or(Value::Null);
    payload["index"] = index;
    payload["timing"] = json!({ "elapsedMs": started.elapsed().as_millis() as u64 });
    Ok(payload)
}

/// Request-scoped state of one Steps walk.
struct Walk<'c, 'a> {
    ctx: &'c Ctx<'a>,
    limit: usize,
    depth_cap: usize,
    through: bool,
    project: ProjectKind,
    roots: HashMap<String, RouteRoot>,
    /// Conditions, loops, arguments and triggers.
    reader: SiteReader<'c, 'a>,
    /// The call as written, decorators, member types, returns, directives.
    calls: SiteReader<'c, 'a>,
    /// The steps in the order they were made; `step_index` finds one.
    steps: Vec<StepRecord>,
    step_index: HashMap<String, usize>,
    links: Vec<WireStepLink>,
    link_index: HashMap<String, usize>,
    /// The rail's material per function: its sites in the order recorded,
    /// and the keys already recorded.
    programs: Vec<(String, Vec<ProgramSite>, HashSet<String>)>,
    program_index: HashMap<String, usize>,
    truncated_steps: usize,
    truncated_hubs: usize,
    truncated_chrome: usize,
    effect_scans: usize,
    fan_in: HashMap<String, i64>,
    render_parents: HashMap<String, usize>,
    file_scope_edges: HashMap<String, Vec<Edge>>,
    file_scope_refs: HashMap<String, Vec<UnresolvedRef>>,
    fired_spans: HashMap<String, Vec<FiredSpan>>,
    file_types: HashMap<String, HashMap<String, String>>,
    class_types: HashMap<String, BTreeMap<String, String>>,
    class_by_name: HashMap<String, Option<Node>>,
    project_types: HashMap<String, bool>,
    /// A screen anchor's picture is laid out by region.
    regions: bool,
}

/// What `effect_link` is handed: the reference and where it is written.
struct EffectRef {
    name: String,
    kind: CallKind,
    line: i64,
    column: Option<i64>,
}

impl<'c, 'a> Walk<'c, 'a> {
    fn step(&self, index: usize) -> &StepRecord {
        &self.steps[index]
    }

    fn step_mut(&mut self, index: usize) -> &mut StepRecord {
        &mut self.steps[index]
    }

    // ------------------------------------------------------------ readers --

    /// A hop's position with its call's span and enclosing call, read from
    /// the tree; the bare position when unreadable or not THIS call.
    fn hop_at(
        &mut self,
        caller: &Node,
        line: Option<i64>,
        column: Option<i64>,
        callee: Option<&str>,
    ) -> HopSite {
        let at_line = line.unwrap_or(caller.start_line);
        let at_column = column.unwrap_or(0);
        let read = if line.is_some_and(|line| line > 0) {
            self.calls
                .call_site(&caller.file_path, caller.language, line, column, callee)
        } else {
            None
        };
        let usable = read.as_ref().is_some_and(|read| {
            callee.is_none_or(|callee| last_segment(&read.callee) == last_segment(callee))
        });
        let bare = HopSite {
            file: caller.file_path.clone(),
            line: at_line,
            column: at_column,
            end: (at_line, at_column),
            within: None,
        };
        if !usable {
            return bare;
        }
        let read = read.expect("a usable read");
        let span = read.span;
        HopSite {
            file: caller.file_path.clone(),
            line: span.map_or(at_line, |span| span.start.line as i64),
            column: span.map_or(at_column, |span| span.start.column as i64),
            end: span.map_or((at_line, at_column), |span| {
                (span.end.line as i64, span.end.column as i64)
            }),
            within: read.within,
        }
    }

    fn point_hop(caller: &Node, line: Option<i64>, column: Option<i64>) -> HopSite {
        let at_line = line.unwrap_or(caller.start_line);
        let at_column = column.unwrap_or(0);
        HopSite {
            file: caller.file_path.clone(),
            line: at_line,
            column: at_column,
            end: (at_line, at_column),
            within: None,
        }
    }

    fn with_args(
        &mut self,
        mut site: WireStepSite,
        caller: &Node,
        line: Option<i64>,
        column: Option<i64>,
    ) -> WireStepSite {
        if let Some(args) = self
            .reader
            .args(&caller.file_path, caller.language, line, column)
        {
            site.args = Some(args);
        }
        site
    }

    fn trigger_at(
        &mut self,
        caller: &Node,
        line: Option<i64>,
        column: Option<i64>,
    ) -> Option<WireStepTrigger> {
        let found = self
            .reader
            .trigger(&caller.file_path, caller.language, line, column)?;
        Some(WireStepTrigger {
            kind: trigger_kind_word(found.kind),
            name: found.name,
            of: found.of,
            after: None,
            in_: caller.name.clone(),
        })
    }

    fn member_types_of(&mut self, node: &Node) -> BTreeMap<String, String> {
        let key = format!("{}:{}", node.file_path, node.start_line);
        if let Some(types) = self.class_types.get(&key) {
            return types.clone();
        }
        let types = self
            .calls
            .member_types(&node.file_path, node.language, node.start_line);
        self.class_types.insert(key, types.clone());
        types
    }

    /// The declared type of the receiver a call is made on.
    fn receiver_type_for(&mut self, caller: &Node, callee: &str) -> ApiResult<Option<String>> {
        let stripped = callee
            .strip_prefix("this.")
            .or_else(|| callee.strip_prefix("self."))
            .unwrap_or(callee);
        let first = stripped.split(['.', ':', '(']).next().unwrap_or("");
        if first.is_empty() || first.starts_with(|ch: char| ch.is_ascii_uppercase()) {
            return Ok(None);
        }
        let declared = self.member_types_of(caller);
        if let Some(own) = declared
            .get(first)
            .or_else(|| declared.get(first.strip_prefix('_').unwrap_or(first)))
        {
            return Ok(Some(own.clone()));
        }
        if !self.file_types.contains_key(&caller.file_path) {
            let mut types: HashMap<String, String> = HashMap::new();
            for node in self
                .ctx
                .store
                .nodes_in_files(std::slice::from_ref(&caller.file_path))?
            {
                if !matches!(
                    node.kind,
                    NodeKind::Field | NodeKind::Property | NodeKind::Variable | NodeKind::Parameter
                ) {
                    continue;
                }
                let Some(signature) = node.signature.as_deref() else {
                    continue;
                };
                let signature = signature.split_whitespace().collect::<Vec<_>>().join(" ");
                if let Some(found) = typed_signature(&signature, &node.name) {
                    types.entry(node.name.clone()).or_insert(found);
                }
            }
            self.file_types.insert(caller.file_path.clone(), types);
        }
        Ok(self.file_types[&caller.file_path].get(first).cloned())
    }

    /// The class / interface / struct a declared type names in the index,
    /// or `None` for a library's.
    fn class_of_type(&mut self, type_name: &str) -> ApiResult<Option<Node>> {
        let base = type_name.split('<').next().unwrap_or(type_name);
        let base = base.trim_start_matches(['*', '&']);
        let base = base.strip_suffix(['?', '!']).unwrap_or(base);
        let name = base.rsplit(['.', ':']).next().unwrap_or(base).trim();
        if name.is_empty()
            || matches!(
                name,
                "string"
                    | "number"
                    | "boolean"
                    | "any"
                    | "unknown"
                    | "object"
                    | "void"
                    | "String"
                    | "Integer"
                    | "Long"
                    | "Boolean"
                    | "int"
                    | "long"
                    | "bool"
                    | "var"
                    | "dynamic"
                    | "Object"
                    | "List"
                    | "Map"
                    | "Set"
                    | "Array"
                    | "Promise"
                    | "Optional"
                    | "Task"
                    | "IEnumerable"
                    | "Iterable"
            )
        {
            return Ok(None);
        }
        if let Some(found) = self.class_by_name.get(name) {
            return Ok(found.clone());
        }
        let mut found: Vec<Node> = self
            .ctx
            .store
            .nodes_by_name(name)?
            .into_iter()
            .filter(|node| {
                matches!(
                    node.kind,
                    NodeKind::Class | NodeKind::Interface | NodeKind::Struct
                )
            })
            .collect();
        found.sort_by(|a, b| {
            a.file_path
                .cmp(&b.file_path)
                .then(a.start_line.cmp(&b.start_line))
                .then_with(|| a.id.cmp(&b.id))
        });
        let class = found
            .iter()
            .find(|node| !is_test_path(&node.file_path))
            .or(found.first())
            .cloned();
        self.class_by_name.insert(name.to_string(), class.clone());
        Ok(class)
    }

    /// Whether a Swift callee's root names a type the project declares.
    fn declares_swift_type(&mut self, callee: &str) -> ApiResult<bool> {
        let stripped = callee
            .strip_prefix("self.")
            .or_else(|| callee.strip_prefix("Self."))
            .unwrap_or(callee);
        let root = stripped.split(['.', '(', ':', '<']).next().unwrap_or("");
        let shaped = root.starts_with(|ch: char| ch.is_ascii_uppercase())
            && root
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
        if !shaped {
            return Ok(false);
        }
        if let Some(known) = self.project_types.get(root) {
            return Ok(*known);
        }
        let mut known = false;
        for node in self.ctx.store.nodes_by_name(root)? {
            if node.language != Language::Swift
                || !matches!(
                    node.kind,
                    NodeKind::Class
                        | NodeKind::Struct
                        | NodeKind::Enum
                        | NodeKind::Interface
                        | NodeKind::Protocol
                )
            {
                continue;
            }
            if !self.reader.swift_extension(&node) {
                known = true;
                break;
            }
        }
        self.project_types.insert(root.to_string(), known);
        Ok(known)
    }

    /// Where a member call really goes, by the receiver's declared type.
    fn resolve_by_receiver(&mut self, caller: &Node, callee: &str) -> ApiResult<Option<Node>> {
        static ARGS: OnceLock<Regex> = OnceLock::new();
        let bare = ARGS
            .get_or_init(|| Regex::new(r"\([^()]*\)").expect("valid regex"))
            .replace_all(callee, "");
        let segments: Vec<&str> = bare.split(['.', ':']).filter(|s| !s.is_empty()).collect();
        if segments.len() < 2 {
            return Ok(None);
        }
        let Some(type_name) = self.receiver_type_for(caller, callee)? else {
            return Ok(None);
        };
        let Some(class) = self.class_of_type(&type_name)? else {
            return Ok(None);
        };
        let method = segments[segments.len() - 1];
        Ok(self
            .ctx
            .store
            .nodes_in_files(std::slice::from_ref(&class.file_path))?
            .into_iter()
            .find(|node| {
                matches!(node.kind, NodeKind::Method | NodeKind::Function)
                    && node.name == method
                    && node.start_line >= class.start_line
                    && node.end_line <= class.end_line
            }))
    }

    /// A name-match the call as written disproves: a method of the CALLER's
    /// own class, called on a receiver that is not `this` (JS family only).
    fn own_method_without_receiver(caller: &Node, target: &Node, written: &str) -> bool {
        if !is_js_family(caller.language) || target.kind != NodeKind::Method {
            return false;
        }
        if written.starts_with("this.")
            || written.starts_with("this?")
            || written.starts_with("self.")
            || written.starts_with("self?")
        {
            return false;
        }
        let container = |qualified: &str| {
            qualified
                .rfind(['.', ':'])
                .map_or(qualified.to_string(), |at| {
                    qualified[..at].trim_end_matches(['.', ':']).to_string()
                })
        };
        caller.file_path == target.file_path
            && container(&caller.qualified_name) == container(&target.qualified_name)
    }

    /// A method the walk cannot enter on a repository-shaped container.
    fn repository_method(&self, target: &Node) -> ApiResult<bool> {
        if !matches!(target.kind, NodeKind::Method | NodeKind::Function)
            || is_test_path(&target.file_path)
        {
            return Ok(false);
        }
        let container = container_name(&target.qualified_name);
        static DIR: OnceLock<Regex> = OnceLock::new();
        let in_dir = DIR
            .get_or_init(|| {
                Regex::new(r"(?i)(?:^|/)(?:repositories|repository|dao|daos|mappers)/")
                    .expect("valid regex")
            })
            .is_match(&posix(&target.file_path));
        if !repository_container(&container) && !in_dir {
            return Ok(false);
        }
        Ok(self
            .ctx
            .store
            .outgoing_edges_from(std::slice::from_ref(&target.id), WALK_KINDS)?
            .is_empty())
    }

    /// What runs before a route's handler: the middleware arguments at the
    /// registration, or the guard decorators on it.
    fn chain_for(&mut self, route: &Node, root: Option<&Node>) -> Vec<String> {
        let mut after: Vec<String> = Vec::new();
        if is_js_family(route.language)
            && let Some(site) = self.calls.call_site(
                &route.file_path,
                route.language,
                Some(route.start_line),
                Some(0),
                None,
            )
        {
            static VERB: OnceLock<Regex> = OnceLock::new();
            if VERB
                .get_or_init(|| {
                    Regex::new(r"(?i)\.(?:get|post|put|patch|delete|all|use|head|options|route)$")
                        .expect("valid regex")
                })
                .is_match(&site.callee)
            {
                let mut args: Vec<String> = site.arg_list.iter().skip(1).cloned().collect();
                if args.last().is_some_and(|last| !last.starts_with('{')) {
                    args.pop();
                }
                static SHAPE: OnceLock<Regex> = OnceLock::new();
                let shape = SHAPE.get_or_init(|| Regex::new(r"^\{ ?…? ?\}$").expect("valid regex"));
                for arg in args {
                    if !arg.is_empty() && !shape.is_match(&arg) {
                        after.push(arg);
                    }
                }
            }
        }
        if let Some(root) = root.filter(|root| root.id != route.id)
            && let Some(decorators) =
                self.calls
                    .decorators(&root.file_path, root.language, root.start_line)
        {
            for decorator in decorators.class.iter().chain(decorators.own.iter()) {
                if !is_js_family(root.language) && root.language != Language::Python {
                    if is_guard_decorator(decorator) {
                        after.push(decorator.clone());
                    }
                    continue;
                }
                after.extend(dependencies_in(decorator));
                if is_guard_decorator(decorator) {
                    after.push(decorator.clone());
                }
            }
        }
        let mut seen = HashSet::new();
        after.retain(|item| seen.insert(item.clone()));
        after
    }

    /// The request a route's handler serves, as its trigger.
    fn request_trigger(&mut self, route: &Node, root: Option<&Node>) -> Option<WireStepTrigger> {
        let (method, path) = split_route_name(&route.name);
        let Some(method) = method else {
            if next_route_for_file(&route.file_path)
                .is_some_and(|file| file.kind == NextRouteKind::Page)
            {
                return Some(WireStepTrigger {
                    kind: "load",
                    name: "GET".to_string(),
                    of: Some(path),
                    after: None,
                    in_: basename(&route.file_path),
                });
            }
            return None;
        };
        let after = self.chain_for(route, root);
        Some(WireStepTrigger {
            kind: "request",
            name: method,
            of: Some(path),
            after: (!after.is_empty()).then_some(after),
            in_: basename(&route.file_path),
        })
    }

    /// A job, an event, a message or a schedule that fires a function.
    fn consumer_trigger(&mut self, node: &Node) -> Option<WireStepTrigger> {
        if !matches!(node.kind, NodeKind::Function | NodeKind::Method) {
            return None;
        }
        let decorators = self
            .calls
            .decorators(&node.file_path, node.language, node.start_line)?;
        for decorator in &decorators.own {
            let name = decorator_name(decorator);
            let last = name.rsplit('.').next().unwrap_or(name);
            if !consumer_decorator().is_match(name) && !consumer_decorator().is_match(last) {
                continue;
            }
            let guards: Vec<String> = decorators
                .class
                .iter()
                .chain(decorators.own.iter())
                .filter(|other| *other != decorator && is_guard_decorator(other))
                .cloned()
                .collect();
            return Some(WireStepTrigger {
                kind: "decorator",
                name: name.to_string(),
                of: decorator_literal(decorator),
                after: (!guards.is_empty()).then_some(guards),
                in_: basename(&node.file_path),
            });
        }
        None
    }

    // -------------------------------------------------------------- steps --

    fn step_for(
        &mut self,
        node: &Node,
        kind: &'static str,
        depth: usize,
        event: Option<String>,
        trigger: Option<WireStepTrigger>,
    ) -> Option<usize> {
        if let Some(&index) = self.step_index.get(&node.id) {
            let existing = &mut self.steps[index].step;
            // A listener met as a handler is the native event's landing when
            // the walk arrives from the other side.
            if existing.kind == "trigger" && kind == "event" {
                existing.kind = "event";
                if event.is_some() {
                    existing.event = event.clone();
                }
            }
            if kind == "event"
                && let Some(event) = event
            {
                let seed: Vec<String> = existing.event.iter().cloned().collect();
                let events = existing.events.get_or_insert(seed);
                if !events.contains(&event) {
                    events.push(event);
                }
            }
            return Some(index);
        }
        if self.steps.len() >= self.limit {
            self.truncated_steps += 1;
            return None;
        }
        let is_route = node.kind == NodeKind::Route;
        let route_root = if is_route {
            self.roots.get(&node.id).cloned()
        } else {
            None
        };
        let sub = if is_route {
            match &route_root {
                None => basename(&node.file_path),
                Some(root) if root.inline => {
                    format!("inline handler · {}", basename(&node.file_path))
                }
                Some(root) => root.node.name.clone(),
            }
        } else {
            posix(&node.file_path)
        };
        let screen = is_route.then(|| WireStepScreen {
            path: node.name.clone(),
            component: route_root
                .as_ref()
                .filter(|root| !root.inline)
                .map(|root| to_node_ref(&root.node)),
            endpoint: split_route_name(&node.name).0.is_some(),
            inline: route_root.as_ref().is_some_and(|root| root.inline),
        });
        let events = (kind == "event")
            .then(|| event.clone())
            .flatten()
            .map(|event| vec![event]);
        let record = StepRecord {
            step: WireStep {
                id: node.id.clone(),
                // A route is a screen or an endpoint — except one reached
                // across a tier, which is the crossing.
                kind: if is_route && kind != "bridge" {
                    "screen"
                } else {
                    kind
                },
                anchor: false,
                node: Some(to_node_ref(node)),
                label: node.name.clone(),
                sub,
                depth,
                cut: None,
                event,
                events,
                trigger,
                order: 0,
                region: None,
                screen,
                effect: None,
            },
            first: None,
            root: if is_route {
                route_root.map(|root| root.node)
            } else {
                Some(node.clone())
            },
        };
        self.step_index.insert(node.id.clone(), self.steps.len());
        self.steps.push(record);
        Some(self.steps.len() - 1)
    }

    /// One box per (function, category); a reply's box per outcome.
    fn effect_step(
        &mut self,
        by: &Node,
        name: &str,
        line: i64,
        effect: &Effect,
        depth: usize,
        status: Option<u16>,
    ) -> Option<usize> {
        let category = effect.category.as_str();
        let id = match status {
            Some(status) => format!("effect:{}:{category}:{status}", by.id),
            None => format!("effect:{}:{category}", by.id),
        };
        if let Some(&index) = self.step_index.get(&id) {
            let record = &mut self.steps[index].step;
            let boxed = record.effect.as_mut().expect("an effect box");
            if !boxed.apis.iter().any(|api| api == name) {
                boxed.apis.push(name.to_string());
                record.label = format!("{} +{}", boxed.apis[0], boxed.apis.len() - 1);
            }
            if let Some(model) = &effect.model
                && boxed.model.as_deref() != Some(model.as_str())
            {
                let mut models: Vec<String> = boxed
                    .model
                    .as_deref()
                    .unwrap_or("")
                    .split(", ")
                    .filter(|model| !model.is_empty())
                    .map(str::to_string)
                    .collect();
                if !models.contains(model) {
                    models.push(model.clone());
                }
                let mut joined = models
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                if models.len() > 3 {
                    joined.push_str(", …");
                }
                boxed.model = Some(joined);
            }
            if let (Some(access), Some(known)) = (effect.access, boxed.access)
                && access != known
            {
                boxed.access = None;
            }
            let sub = effect_sub(boxed, by);
            record.sub = sub;
            return Some(index);
        }
        if self.steps.len() >= self.limit {
            self.truncated_steps += 1;
            return None;
        }
        let boxed = WireStepEffect {
            api: name.to_string(),
            apis: vec![name.to_string()],
            category,
            by: to_node_ref(by),
            line,
            model: effect.model.clone(),
            access: effect.access,
            statuses: None,
        };
        let sub = effect_sub(&boxed, by);
        let record = StepRecord {
            step: WireStep {
                id: id.clone(),
                kind: "effect",
                anchor: false,
                node: None,
                label: name.to_string(),
                sub,
                depth,
                cut: None,
                event: None,
                events: None,
                trigger: None,
                order: 0,
                region: None,
                screen: None,
                effect: Some(boxed),
            },
            first: None,
            root: None,
        };
        self.step_index.insert(id, self.steps.len());
        self.steps.push(record);
        Some(self.steps.len() - 1)
    }

    /// The region a step reached from `from` through `chain` belongs to.
    fn region_of(&self, from: usize, chain: &[Node]) -> Option<WireStepRegion> {
        if !self.regions {
            return None;
        }
        let from = self.step(from);
        if !from.step.anchor {
            return from.step.region.clone();
        }
        if let Some(head) = chain.first() {
            return Some(WireStepRegion {
                id: head.id.clone(),
                label: head.name.clone(),
            });
        }
        Some(WireStepRegion {
            id: from
                .root
                .as_ref()
                .map_or_else(|| from.step.id.clone(), |root| root.id.clone()),
            label: from
                .root
                .as_ref()
                .map_or_else(|| from.step.label.clone(), |root| root.name.clone()),
        })
    }

    /// The step a binding written inside an effect call's arguments arrives
    /// from: the innermost such span whose effect names the call.
    fn fired_by_effect(
        &self,
        function: &str,
        line: Option<i64>,
        column: Option<i64>,
        of: &str,
    ) -> Option<usize> {
        let line = line?;
        let column = column.unwrap_or(0);
        let spans = self.fired_spans.get(function)?;
        let want = last_segment(of);
        let mut best: Option<&FiredSpan> = None;
        for span in spans {
            if line < span.start.0 || line > span.end.0 {
                continue;
            }
            if line == span.start.0 && column < span.start.1 {
                continue;
            }
            if line == span.end.0 && column > span.end.1 {
                continue;
            }
            let names_it = self.steps[span.step]
                .step
                .effect
                .as_ref()
                .is_some_and(|effect| effect.apis.iter().any(|api| last_segment(api) == want));
            if !names_it {
                continue;
            }
            if best.is_none_or(|best| span.start > best.start) {
                best = Some(span);
            }
        }
        best.map(|span| span.step)
    }

    /// Record one site in the function it is written in, once per position
    /// and destination.
    #[allow(clippy::too_many_arguments)]
    fn record(
        &mut self,
        function: &Node,
        hop: &HopSite,
        guards: &[BranchGuard],
        step: Option<String>,
        link: Option<String>,
        into: Option<String>,
        trigger: Option<&WireStepTrigger>,
        loops: Vec<SiteLoop>,
    ) {
        let index = match self.program_index.get(&function.id) {
            Some(&index) => index,
            None => {
                self.programs
                    .push((function.id.clone(), Vec::new(), HashSet::new()));
                self.program_index
                    .insert(function.id.clone(), self.programs.len() - 1);
                self.programs.len() - 1
            }
        };
        let key = format!(
            "{}:{}:{}",
            hop.line,
            hop.column,
            step.as_deref().or(into.as_deref()).unwrap_or("")
        );
        let (_, sites, keys) = &mut self.programs[index];
        if !keys.insert(key) {
            return;
        }
        sites.push(ProgramSite {
            step,
            link,
            into,
            at: SiteAt {
                line: hop.line,
                column: hop.column,
                end: SitePosition {
                    line: hop.end.0,
                    column: hop.end.1,
                },
            },
            within: hop.within.clone(),
            guards: guards.to_vec(),
            loops,
            trigger: trigger.map(|trigger| ProgramTrigger {
                kind: trigger.kind.to_string(),
                name: trigger.name.clone(),
            }),
        });
    }

    /// Add or merge a link; its id.
    #[allow(clippy::too_many_arguments)]
    fn link(
        &mut self,
        from: usize,
        to: usize,
        kind: &'static str,
        chain: &[Node],
        whens: &[String],
        site: WireStepSite,
        edge: Option<&Edge>,
        trigger: Option<WireStepTrigger>,
        within: Option<String>,
    ) -> String {
        let meta = edge.map(meta_of).unwrap_or_default();
        let synthesized = edge.is_some_and(|edge| edge.provenance.as_deref() == Some("heuristic"));
        let confidence = meta.get("confidence").and_then(Value::as_f64);
        let via: Vec<WireNodeRef> = chain.iter().map(to_node_ref).collect();
        let via_key = via
            .iter()
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>()
            .join(">");
        let from_id = self.steps[from].step.id.clone();
        let to_id = self.steps[to].step.id.clone();
        let id = format!("{from_id} {to_id} {via_key}");
        let mut seen: Vec<&str> = Vec::new();
        for when in whens {
            if !when.is_empty() && !seen.contains(&when.as_str()) {
                seen.push(when);
            }
        }
        let when = seen.join(" && ");
        let mut stamped = site;
        stamped.when = when.clone();
        if trigger.is_some() {
            stamped.trigger = trigger.clone();
        }
        // A `contains` edge is how a nested handler is FOUND, not a place it
        // is called from: its row stays only while no call site was seen.
        let structural = |site: &WireStepSite| site.text.starts_with("defines ");
        if let Some(&index) = self.link_index.get(&id) {
            let existing = &mut self.links[index];
            if structural(&stamped) && existing.sites.iter().any(|site| !structural(site)) {
                return id;
            }
            if !structural(&stamped) && existing.sites.iter().all(structural) {
                existing.sites.clear();
            }
            // One statement, two references (`res.status(201)` and its
            // `.json(…)`): the outer call is the site.
            match existing
                .sites
                .iter()
                .position(|site| site.file == stamped.file && site.line == stamped.line)
            {
                None => existing.sites.push(stamped),
                Some(same) => {
                    let known = &existing.sites[same].text;
                    if stamped.text.starts_with(known.as_str()) && stamped.text.len() > known.len()
                    {
                        existing.sites[same] = stamped;
                    }
                }
            }
            if existing.trigger.is_none() && trigger.is_some() {
                existing.trigger = trigger;
            }
            if existing.within.is_none() && within.is_some() {
                existing.within = within;
            }
            if when != existing.when {
                if when.is_empty() || existing.when.is_empty() {
                    existing.when = String::new();
                } else if !existing.when.split(" || ").any(|known| known == when) {
                    existing.when = format!("{} || {when}", existing.when);
                }
            }
            return id;
        }
        self.link_index.insert(id.clone(), self.links.len());
        self.links.push(WireStepLink {
            id: id.clone(),
            from: from_id,
            to: to_id,
            kind,
            via,
            when,
            label: hop_label(&meta, synthesized),
            within,
            synthesized,
            uncertain: confidence.is_some_and(|confidence| confidence < UNCERTAIN_BELOW),
            sites: vec![stamped],
            trigger: trigger.clone(),
        });
        let target = &mut self.steps[to].step;
        if let Some(trigger) = trigger
            && target.kind == "trigger"
            && target.trigger.is_none()
        {
            target.trigger = Some(trigger);
        }
        id
    }

    /// How a Spring MVC handler answers: each `return` of a view name.
    fn spring_mvc_replies(&mut self, node: &Node) -> Vec<(String, Option<u16>, i64, i64)> {
        if !matches!(node.language, Language::Java | Language::Kotlin)
            || node.kind != NodeKind::Method
        {
            return Vec::new();
        }
        let Some(decorators) =
            self.calls
                .decorators(&node.file_path, node.language, node.start_line)
        else {
            return Vec::new();
        };
        static MAPPING: OnceLock<Regex> = OnceLock::new();
        let mapping = MAPPING.get_or_init(|| {
            Regex::new(r"^(?:Get|Post|Put|Patch|Delete|Request)Mapping(?-u:\b)")
                .expect("valid regex")
        });
        if !decorators
            .own
            .iter()
            .any(|decorator| mapping.is_match(decorator))
        {
            return Vec::new();
        }
        let word = |text: &str, prefix: &str| {
            text.strip_prefix(prefix).is_some_and(|rest| {
                !rest
                    .chars()
                    .next()
                    .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            })
        };
        if decorators
            .class
            .iter()
            .any(|decorator| word(decorator, "RestController"))
            || decorators
                .own
                .iter()
                .any(|decorator| word(decorator, "ResponseBody"))
        {
            return Vec::new();
        }
        let mut out = Vec::new();
        for site in self
            .calls
            .returns(&node.file_path, node.language, node.start_line)
        {
            let Some(view) = site
                .value
                .clone()
                .or_else(|| spring_view_string(&site.expression))
            else {
                continue;
            };
            let (text, status) = if let Some(rest) = view.strip_prefix("redirect:") {
                (format!("redirect:{rest}"), Some(302))
            } else if let Some(rest) = view.strip_prefix("forward:") {
                (format!("forward:{rest}"), None)
            } else {
                (format!("view {view}"), Some(200))
            };
            out.push((text, status, site.line as i64, site.column as i64));
        }
        out
    }

    /// One effect site: the call as written, what it passes, when, what fires
    /// it, and — for a response — the status. Whether it was drawn.
    #[allow(clippy::too_many_arguments)]
    fn effect_link(
        &mut self,
        step: usize,
        fold: &Fold,
        reference: &EffectRef,
        trigger: Option<WireStepTrigger>,
        fallback_args: Option<String>,
        require_receiver: bool,
        preset: Option<(Effect, Option<u16>)>,
    ) -> ApiResult<bool> {
        let node = &fold.node;
        let site = if preset.is_some() {
            None
        } else {
            self.calls.call_site(
                &node.file_path,
                node.language,
                Some(reference.line),
                reference.column,
                Some(&reference.name),
            )
        };
        // The site read must be THIS call: its last segment is the
        // reference's.
        let usable = site.as_ref().is_some_and(|site| {
            !site.callee.is_empty() && last_segment(&site.callee) == last_segment(&reference.name)
        });
        let text = if usable {
            site.as_ref().expect("a usable site").callee.clone()
        } else {
            reference.name.clone()
        };
        if require_receiver && !text.contains(['.', ':', '>']) {
            return Ok(false);
        }
        let args = if usable {
            site.as_ref().map(|site| site.args.clone())
        } else {
            fallback_args
        };
        let (effect, preset_status) = match preset {
            Some((effect, status)) => (effect, Some(status)),
            None => {
                let declared = self.receiver_type_for(node, &text)?;
                let receiver_type = match declared {
                    Some(declared) if self.resolve_by_receiver(node, &text)?.is_none() => {
                        Some(declared)
                    }
                    _ => None,
                };
                let project_type =
                    node.language == Language::Swift && self.declares_swift_type(&text)?;
                let Some(effect) = classify_effect(&EffectInput {
                    text: &text,
                    kind: reference.kind,
                    language: Some(node.language),
                    project: self.project,
                    receiver_type: receiver_type.as_deref(),
                    args: args.as_deref(),
                    project_type,
                }) else {
                    return Ok(false);
                };
                (effect, None)
            }
        };
        let status = match preset_status {
            Some(status) => status,
            None if effect.category == EffectCategory::Response => {
                response_status(&text, args.as_deref())
                    .or_else(|| {
                        site.as_ref()
                            .filter(|_| usable)
                            .and_then(|site| site.status)
                    })
                    .or_else(|| implicit_response_status(&text))
            }
            None => None,
        };
        // A call bound inside ANOTHER effect's arguments hangs off that box.
        let fired = match trigger {
            Some(trigger) => Some(trigger),
            None => self.trigger_at(node, Some(reference.line), reference.column),
        };
        let from = fired
            .as_ref()
            .and_then(|fired| fired.of.as_deref())
            .and_then(|of| {
                self.fired_by_effect(&node.id, Some(reference.line), reference.column, of)
            })
            .unwrap_or(step);
        let depth = self.steps[from].step.depth + 1;
        let Some(target) = self.effect_step(node, &text, reference.line, &effect, depth, status)
        else {
            return Ok(true);
        };
        let guards = self.reader.guards(
            &node.file_path,
            node.language,
            Some(reference.line),
            reference.column,
        );
        let when = guard_label(&guards);
        let wire_site = WireStepSite {
            file: posix(&node.file_path),
            line: reference.line,
            text: text.clone(),
            args: args.clone(),
            when: String::new(),
            trigger: None,
            status,
            decision: decision_of(&guards),
        };
        let span = site.as_ref().and_then(|site| site.span);
        let local = HopSite {
            file: node.file_path.clone(),
            line: span.map_or(reference.line, |span| span.start.line as i64),
            column: span.map_or(reference.column.unwrap_or(0), |span| {
                span.start.column as i64
            }),
            end: span.map_or((reference.line, reference.column.unwrap_or(0)), |span| {
                (span.end.line as i64, span.end.column as i64)
            }),
            within: site.as_ref().and_then(|site| site.within.clone()),
        };
        if usable && span.is_some() {
            self.fired_spans
                .entry(node.id.clone())
                .or_default()
                .push(FiredSpan {
                    start: (local.line, local.column),
                    end: local.end,
                    step: target,
                });
        }
        let hop = fold.first.clone().unwrap_or_else(|| local.clone());
        if self.steps[target].first.is_none() {
            self.steps[target].first = Some(hop.clone());
        }
        if self.steps[target].step.region.is_none() {
            let region = self.region_of(from, &fold.chain);
            self.steps[target].step.region = region;
        }
        let mut whens = fold.whens.clone();
        whens.push(when);
        let id = self.link(
            from,
            target,
            "effect",
            &fold.chain,
            &whens,
            wire_site,
            None,
            fired.clone(),
            hop.within.clone(),
        );
        let loops = self.reader.loops(
            &node.file_path,
            node.language,
            Some(reference.line),
            reference.column,
        );
        let target_id = self.steps[target].step.id.clone();
        self.record(
            node,
            &local,
            &guards,
            Some(target_id),
            Some(id),
            None,
            fired.as_ref(),
            loops,
        );
        Ok(true)
    }

    // --------------------------------------------------------------- walk --

    fn run(&mut self, anchor: &Node) -> ApiResult<Value> {
        let first = self
            .step_for(anchor, "anchor", 0, None, None)
            .expect("the anchor fits");
        self.step_mut(first).step.anchor = true;
        let first_root = self.step(first).root.clone();
        let trigger = if anchor.kind == NodeKind::Route {
            self.request_trigger(anchor, first_root.as_ref())
        } else {
            self.consumer_trigger(anchor)
        };
        if trigger.is_some() {
            self.step_mut(first).step.trigger = trigger;
        }
        self.regions = {
            let step = &self.step(first).step;
            step.kind == "screen" && !step.screen.as_ref().is_some_and(|screen| screen.endpoint)
        };
        let mut queue: VecDeque<usize> = VecDeque::from([first]);
        let mut explored: HashSet<String> = HashSet::from([self.step(first).step.id.clone()]);

        while let Some(step) = queue.pop_front() {
            let Some(root) = self.step(step).root.clone() else {
                continue;
            };
            let (kind, is_anchor, node_kind) = {
                let record = &self.step(step).step;
                (
                    record.kind,
                    record.anchor,
                    record.node.as_ref().map(|node| node.kind),
                )
            };
            // Another screen is a chapter of its own; so is an endpoint
            // reached across a tier.
            if (kind == "screen" || (kind == "bridge" && node_kind == Some("route")))
                && !is_anchor
                && !self.through
            {
                self.step_mut(step).step.cut = Some("screen");
                continue;
            }
            // A native event landing in a COMPONENT lands on another screen's
            // body.
            if kind == "event" && !is_anchor && !self.through && looks_like_component(&root) {
                self.step_mut(step).step.cut = Some("component");
                continue;
            }
            if self.step(step).step.depth >= self.depth_cap {
                if !self
                    .ctx
                    .store
                    .outgoing_edges_from(std::slice::from_ref(&root.id), WALK_KINDS)?
                    .is_empty()
                {
                    self.step_mut(step).step.cut = Some("depth");
                }
                continue;
            }
            self.explore(step, &root, &mut queue, &mut explored)?;
        }
        self.finish(first)
    }

    /// Breadth-first through the plumbing from one step until the next steps.
    fn explore(
        &mut self,
        step: usize,
        root: &Node,
        queue: &mut VecDeque<usize>,
        explored: &mut HashSet<String>,
    ) -> ApiResult<()> {
        let mut visited: HashSet<String> = HashSet::from([root.id.clone()]);
        let mut frontier: Vec<Fold> = vec![Fold {
            node: root.clone(),
            chain: Vec::new(),
            whens: Vec::new(),
            first: None,
        }];
        for _ in 0..=MAX_FOLD_DEPTH {
            if frontier.is_empty() {
                break;
            }
            let ids: Vec<String> = frontier.iter().map(|fold| fold.node.id.clone()).collect();
            let outgoing = self.ctx.store.outgoing_edges_from(&ids, WALK_KINDS)?;
            // Lookups only; each fold's edges are sorted before use.
            let mut by_source: HashMap<String, Vec<Edge>> = HashMap::new();
            for edge in outgoing {
                by_source.entry(edge.source.clone()).or_default().push(edge);
            }
            // A wrapper component, or a value a registration is written
            // inside, is lent the file scope's edges within its lines.
            for fold in &frontier {
                let value = matches!(fold.node.kind, NodeKind::Constant | NodeKind::Variable);
                let behaviour = by_source.get(&fold.node.id).map_or(0, |edges| {
                    edges
                        .iter()
                        .filter(|edge| {
                            edge.kind != EdgeKind::References || meta_true(&meta_of(edge), "fnRef")
                        })
                        .count()
                });
                if (fold.node.kind != NodeKind::Component && !value) || behaviour > 0 {
                    continue;
                }
                for edge in self.file_scope_edges_within(&fold.node, value)? {
                    let mut lent = edge.clone();
                    lent.source = fold.node.id.clone();
                    by_source
                        .entry(fold.node.id.clone())
                        .or_default()
                        .push(lent);
                }
            }
            let mut target_ids: Vec<String> = by_source
                .values()
                .flat_map(|edges| edges.iter().map(|edge| edge.target.clone()))
                .collect();
            target_ids.sort();
            target_ids.dedup();
            let targets = if target_ids.is_empty() {
                HashMap::new()
            } else {
                self.ctx.store.nodes_by_ids(&target_ids)?
            };
            let unknown: Vec<String> = target_ids
                .iter()
                .filter(|id| !self.fan_in.contains_key(*id))
                .cloned()
                .collect();
            if !unknown.is_empty() {
                let counted = self.ctx.store.count_incoming_edges(&unknown)?;
                for id in unknown {
                    let count = counted.get(&id).copied().unwrap_or(0);
                    self.fan_in.insert(id, count);
                }
            }
            // The names the calls of this layer were written with.
            let call_sites: Vec<(String, i64, i64)> = by_source
                .values()
                .flat_map(|edges| edges.iter())
                .filter(|edge| edge.kind == EdgeKind::Calls)
                .filter_map(|edge| Some((edge.source.clone(), edge.line?, edge.col?)))
                .collect();
            let written_names = self.ctx.store.resolved_reference_names_at(&call_sites)?;

            let mut next: Vec<Fold> = Vec::new();
            for fold in std::mem::take(&mut frontier) {
                self.explore_fold(
                    step,
                    &fold,
                    &by_source,
                    &targets,
                    &written_names,
                    &mut visited,
                    &mut next,
                    queue,
                    explored,
                )?;
            }
            frontier = next;
        }
        Ok(())
    }

    /// The name a call edge was written with: the one resolved reference
    /// at its site; of several, the one whose last segment is the target's
    /// name; `None` (the target's name stands in) when none or several do.
    fn ref_name(
        edge: &Edge,
        target: &Node,
        written: &HashMap<(String, i64, i64), Vec<String>>,
    ) -> Option<String> {
        let (line, col) = (edge.line?, edge.col?);
        let mut names: Vec<&String> = written
            .get(&(edge.source.clone(), line, col))?
            .iter()
            .collect();
        names.sort();
        names.dedup();
        if let [only] = names.as_slice() {
            return Some((*only).clone());
        }
        let matching: Vec<&&String> = names
            .iter()
            .filter(|name| codegraph_store::viewer::reference_name_tail(name) == target.name)
            .collect();
        match matching.as_slice() {
            [only] => Some((**only).clone()),
            _ => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn explore_fold(
        &mut self,
        step: usize,
        fold: &Fold,
        by_source: &HashMap<String, Vec<Edge>>,
        targets: &HashMap<String, Node>,
        written: &HashMap<(String, i64, i64), Vec<String>>,
        visited: &mut HashSet<String>,
        next: &mut Vec<Fold>,
        queue: &mut VecDeque<usize>,
        explored: &mut HashSet<String>,
    ) -> ApiResult<()> {
        let node = &fold.node;
        let own_edges: Vec<Edge> = by_source.get(&node.id).cloned().unwrap_or_default();
        // A call a synthesized channel already follows is the crossing, not
        // also a call outside the index.
        let mut channel_lines: HashSet<i64> = HashSet::new();
        let mut channel_calls: HashMap<i64, HashSet<String>> = HashMap::new();
        for edge in &own_edges {
            let meta = meta_of(edge);
            let (Some(_), Some(line)) = (meta_str(&meta, "channel"), edge.line) else {
                continue;
            };
            channel_lines.insert(line);
            if let Some(callee) = meta_str(&meta, "callee") {
                channel_calls.entry(line).or_default().insert(
                    callee
                        .rsplit(['.', ':'])
                        .next()
                        .unwrap_or(callee)
                        .to_string(),
                );
            }
        }
        // Effects made by this node, folded or not.
        if self.effect_scans < MAX_EFFECT_SCANS {
            self.effect_scans += 1;
            let mut refs = self
                .ctx
                .store
                .unresolved_refs_from(&node.id)
                .unwrap_or_default();
            if refs.is_empty() && matches!(node.kind, NodeKind::Constant | NodeKind::Variable) {
                refs = self.file_scope_refs_within(node)?;
            }
            refs.sort_by(|a, b| a.line.cmp(&b.line).then(a.col.cmp(&b.col)));
            for reference in refs {
                let kind = match reference.reference_kind {
                    EdgeKind::Calls => CallKind::Calls,
                    EdgeKind::Instantiates => CallKind::Instantiates,
                    _ => continue,
                };
                if channel_lines.contains(&reference.line) {
                    continue;
                }
                self.effect_link(
                    step,
                    fold,
                    &EffectRef {
                        name: reference.reference_name.clone(),
                        kind,
                        line: reference.line,
                        column: Some(reference.col),
                    },
                    None,
                    None,
                    false,
                    None,
                )?;
            }
            // A Spring MVC handler answers by what it returns, which is no
            // call.
            for (text, status, line, column) in self.spring_mvc_replies(node) {
                self.effect_link(
                    step,
                    fold,
                    &EffectRef {
                        name: text,
                        kind: CallKind::Calls,
                        line,
                        column: Some(column),
                    },
                    None,
                    None,
                    false,
                    Some((
                        Effect {
                            category: EffectCategory::Response,
                            model: None,
                            access: None,
                        },
                        status,
                    )),
                )?;
            }
        }

        let mut edges: Vec<Edge> = own_edges
            .into_iter()
            .filter(|edge| {
                let meta = meta_of(edge);
                match edge.kind {
                    EdgeKind::References => meta_true(&meta, "fnRef"),
                    EdgeKind::Contains => {
                        let target = targets.get(&edge.target);
                        let from_function =
                            matches!(node.kind, NodeKind::Function | NodeKind::Method);
                        let from_root_class = node.kind == NodeKind::Class
                            && fold.chain.is_empty()
                            && self.steps[step]
                                .root
                                .as_ref()
                                .is_some_and(|root| root.id == node.id);
                        (from_function || from_root_class)
                            && target.is_some_and(|target| {
                                matches!(target.kind, NodeKind::Function | NodeKind::Method)
                            })
                    }
                    _ => true,
                }
            })
            .collect();
        edges.sort_by(|a, b| {
            a.line
                .unwrap_or(0)
                .cmp(&b.line.unwrap_or(0))
                .then_with(|| locale_compare(&a.target, &b.target))
                .then_with(|| a.kind.as_str().cmp(b.kind.as_str()))
                .then(a.col.cmp(&b.col))
        });
        if edges.len() > MAX_FANOUT {
            self.steps[step].step.cut = Some("fan-out");
            edges.truncate(MAX_FANOUT);
        }

        // Two passes: every edge that arrives at a step, then the rest.
        let mut arrivals: Vec<Arrival> = Vec::new();
        let from_test = is_test_path(&node.file_path);
        for edge in edges {
            let Some(found) = targets.get(&edge.target) else {
                continue;
            };
            if found.kind == NodeKind::File {
                continue;
            }
            let mut meta = meta_of(&edge);
            let mut target = found.clone();
            let mut retargeted = false;
            let ref_name = (edge.kind == EdgeKind::Calls)
                .then(|| Self::ref_name(&edge, &target, written))
                .flatten();
            // A channel follows the call on this line: the channel is the
            // story.
            if meta_str(&meta, "channel").is_none()
                && edge.kind == EdgeKind::Calls
                && channel_lines.contains(&edge.line.unwrap_or(-1))
            {
                let written = ref_name.clone().unwrap_or_else(|| target.name.clone());
                let last = written
                    .rsplit(['.', ':'])
                    .next()
                    .unwrap_or(&written)
                    .to_string();
                let channel_named = edge
                    .line
                    .and_then(|line| channel_calls.get(&line))
                    .is_some_and(|calls| calls.contains(&last));
                if matches!(target.kind, NodeKind::Constant | NodeKind::Variable) || channel_named {
                    continue;
                }
            }
            if edge.kind == EdgeKind::Calls
                && meta_str(&meta, "synthesizedBy").is_none()
                && edge.provenance.as_deref() != Some("heuristic")
            {
                let ref_name = ref_name.clone().unwrap_or_else(|| target.name.clone());
                let bare = !ref_name.contains('.');
                let value = matches!(target.kind, NodeKind::Constant | NodeKind::Variable);
                let mut external = false;
                if !bare && !value {
                    let declared = self.receiver_type_for(node, &ref_name)?;
                    external = match declared {
                        Some(declared) => self.class_of_type(&declared)?.is_none(),
                        None => false,
                    };
                }
                if bare || external || value {
                    let drawn = self.effect_link(
                        step,
                        fold,
                        &EffectRef {
                            name: ref_name.clone(),
                            kind: CallKind::Calls,
                            line: edge.line.unwrap_or(node.start_line),
                            column: edge.col,
                        },
                        None,
                        None,
                        true,
                        None,
                    )?;
                    if drawn {
                        continue;
                    }
                    if bare
                        && let Some(written) = self.calls.call_site(
                            &node.file_path,
                            node.language,
                            edge.line,
                            edge.col,
                            Some(&ref_name),
                        )
                        && written.callee.contains(['.', ':'])
                        && written.callee.rsplit(['.', ':']).next() == Some(ref_name.as_str())
                    {
                        let real = self.resolve_by_receiver(node, &written.callee)?;
                        match real {
                            Some(real) if real.id != target.id => {
                                target = real;
                                retargeted = true;
                            }
                            None if Self::own_method_without_receiver(
                                node,
                                &target,
                                &written.callee,
                            ) =>
                            {
                                continue;
                            }
                            _ => {}
                        }
                    }
                }
            }
            if target.id == node.id {
                continue;
            }
            // A production walk never enters a test double.
            if !from_test && is_test_path(&target.file_path) {
                continue;
            }
            let site = WireStepSite {
                file: posix(&node.file_path),
                line: edge.line.unwrap_or(node.start_line),
                text: site_text(&edge, &meta, &target),
                args: None,
                when: String::new(),
                trigger: None,
                status: None,
                decision: None,
            };
            let is_call = matches!(edge.kind, EdgeKind::Calls | EdgeKind::Instantiates)
                || (edge.kind == EdgeKind::References && meta_true(&meta, "fnRef"));
            let trigger = match site_trigger_of(&meta) {
                Some((name, of)) => Some(WireStepTrigger {
                    kind: "prop",
                    name,
                    of,
                    after: None,
                    in_: node.name.clone(),
                }),
                None if is_call => self.trigger_at(node, edge.line, edge.col),
                None => None,
            };
            // A server action, by its directive.
            if edge.provenance.as_deref() != Some("heuristic")
                && (edge.kind == EdgeKind::Calls
                    || (edge.kind == EdgeKind::References && meta_true(&meta, "fnRef")))
                && matches!(target.kind, NodeKind::Function | NodeKind::Method)
                && is_js_family(target.language)
                && is_js_family(node.language)
            {
                let callee =
                    self.calls
                        .directive(&target.file_path, target.language, target.start_line);
                if (callee.file == Some("server") || callee.own)
                    && self
                        .calls
                        .directive(&node.file_path, node.language, node.start_line)
                        .file
                        != Some("server")
                {
                    meta.insert("tier".to_string(), json!("client→server"));
                    meta.insert("channel".to_string(), json!("server-action"));
                }
            }
            let mut kind: Option<&'static str> = None;
            let mut link_kind: &'static str = "calls";
            let mut event: Option<String> = None;
            if target.kind == NodeKind::Route && meta_str(&meta, "tier") != Some("client→server")
            {
                kind = Some("screen");
                link_kind = "navigates";
            } else {
                let cross = crossing(node.language, target.language, &meta);
                let evidenced = edge.provenance.as_deref() == Some("heuristic")
                    || meta_str(&meta, "bridge") == Some("react-native")
                    || meta_str(&meta, "resolvedBy") == Some("framework")
                    || meta_str(&meta, "channel") == Some("server-action");
                if cross.is_some() && !evidenced {
                    continue;
                }
                match cross {
                    Some("event") => {
                        kind = Some("event");
                        link_kind = "event";
                        event = meta_str(&meta, "event").map(str::to_string);
                    }
                    Some(_) => {
                        kind = Some("bridge");
                        link_kind = "bridge";
                    }
                    None => {
                        let callable = matches!(target.kind, NodeKind::Function | NodeKind::Method);
                        if callable
                            && is_store_file(&target.file_path)
                            && !is_store_file(&node.file_path)
                        {
                            kind = Some("store");
                            link_kind = "store";
                        } else if callable
                            && !looks_like_component(&target)
                            && ((edge.kind == EdgeKind::References && meta_true(&meta, "fnRef"))
                                || trigger.is_some())
                        {
                            kind = Some("trigger");
                            link_kind = "handler";
                        }
                    }
                }
            }
            if retargeted {
                meta.insert("resolvedBy".to_string(), json!("receiver-type"));
            }
            arrivals.push(Arrival {
                edge,
                target,
                meta,
                site,
                kind,
                link_kind,
                event,
                trigger,
            });
        }

        for arrival in arrivals.iter().filter(|arrival| arrival.kind.is_some()) {
            let kind = arrival.kind.expect("a step kind");
            let (line, column) = (arrival.edge.line, arrival.edge.col);
            // A binding written inside an effect call's arguments arrives
            // from that box.
            let fired_by = arrival
                .trigger
                .as_ref()
                .and_then(|trigger| trigger.of.as_deref())
                .and_then(|of| self.fired_by_effect(&node.id, line, column, of));
            let from = fired_by.unwrap_or(step);
            let fresh = !self.step_index.contains_key(&arrival.target.id);
            let step_trigger = (kind == "trigger")
                .then(|| arrival.trigger.clone())
                .flatten();
            let depth = self.steps[from].step.depth + 1;
            let Some(to) = self.step_for(
                &arrival.target,
                kind,
                depth,
                arrival.event.clone(),
                step_trigger,
            ) else {
                continue;
            };
            if fresh && self.steps[to].step.trigger.is_none() {
                let root = self.steps[to].root.clone();
                let trigger = if arrival.target.kind == NodeKind::Route {
                    self.request_trigger(&arrival.target, root.as_ref())
                } else {
                    self.consumer_trigger(&arrival.target)
                };
                if trigger.is_some() {
                    self.steps[to].step.trigger = trigger;
                }
            }
            let guards = self
                .reader
                .guards(&node.file_path, node.language, line, column);
            let when = guard_label(&guards);
            let mut site = arrival.site.clone();
            let channel = meta_str(&arrival.meta, "channel");
            if channel.is_some() && channel != Some("server-action") {
                let written =
                    self.calls
                        .call_site(&node.file_path, node.language, line, column, None);
                site = match written {
                    Some(written) if !written.callee.is_empty() => WireStepSite {
                        text: written.callee,
                        args: Some(written.args),
                        ..site
                    },
                    _ => self.with_args(site, node, line, column),
                };
            } else if matches!(arrival.link_kind, "bridge" | "store" | "calls")
                || (arrival.link_kind == "handler"
                    && matches!(arrival.edge.kind, EdgeKind::Calls | EdgeKind::Instantiates))
            {
                site = self.with_args(site, node, line, column);
            }
            let is_call_hop = matches!(
                arrival.edge.kind,
                EdgeKind::Calls | EdgeKind::Instantiates | EdgeKind::Navigates
            );
            let local = if is_call_hop {
                self.hop_at(node, line, column, Some(&arrival.target.name))
            } else {
                Self::point_hop(node, line, column)
            };
            let hop = fold.first.clone().unwrap_or_else(|| local.clone());
            if self.steps[to].first.is_none() {
                self.steps[to].first = Some(hop.clone());
            }
            if self.steps[to].step.region.is_none() {
                let region = self.region_of(from, &fold.chain);
                self.steps[to].step.region = region;
            }
            site.decision = decision_of(&guards);
            let mut whens = fold.whens.clone();
            whens.push(when);
            let edge_with_meta = Edge {
                metadata: Some(Value::Object(arrival.meta.clone())),
                ..arrival.edge.clone()
            };
            let id = self.link(
                from,
                to,
                arrival.link_kind,
                &fold.chain,
                &whens,
                site,
                Some(&edge_with_meta),
                arrival.trigger.clone(),
                hop.within.clone(),
            );
            let loops = self
                .reader
                .loops(&node.file_path, node.language, line, column);
            let to_id = self.steps[to].step.id.clone();
            self.record(
                node,
                &local,
                &guards,
                Some(to_id.clone()),
                Some(id),
                None,
                arrival.trigger.as_ref(),
                loops,
            );
            if self.steps[to].root.is_some() && !explored.contains(&to_id) {
                explored.insert(to_id);
                queue.push_back(to);
            }
        }

        for arrival in arrivals.iter().filter(|arrival| arrival.kind.is_none()) {
            let edge = &arrival.edge;
            let target = &arrival.target;
            let (line, column) = (edge.line, edge.col);
            // A thrown exception the framework answers with is a response; a
            // repository method the walk cannot enter is the database.
            if edge.kind == EdgeKind::Instantiates
                && target.kind == NodeKind::Class
                && self.effect_link(
                    step,
                    fold,
                    &EffectRef {
                        name: target.name.clone(),
                        kind: CallKind::Instantiates,
                        line: line.unwrap_or(node.start_line),
                        column,
                    },
                    arrival.trigger.clone(),
                    None,
                    false,
                    None,
                )?
            {
                continue;
            }
            if edge.kind == EdgeKind::Calls && self.repository_method(target)? {
                let container = container_name(&target.qualified_name);
                let ref_name = Self::ref_name(edge, target, written);
                let api = match ref_name {
                    Some(name) if name.contains('.') => name,
                    _ => format!("{container}.{}", target.name),
                };
                if self.effect_link(
                    step,
                    fold,
                    &EffectRef {
                        name: api,
                        kind: CallKind::Calls,
                        line: line.unwrap_or(node.start_line),
                        column,
                    },
                    arrival.trigger.clone(),
                    None,
                    false,
                    None,
                )? {
                    continue;
                }
            }
            // Already a step, reached here by a plain call: a link, not a fold.
            if let Some(&known) = self.step_index.get(&target.id) {
                if known != step {
                    let fired_by = arrival
                        .trigger
                        .as_ref()
                        .and_then(|trigger| trigger.of.as_deref())
                        .and_then(|of| self.fired_by_effect(&node.id, line, column, of));
                    let guards = self
                        .reader
                        .guards(&node.file_path, node.language, line, column);
                    let local = self.hop_at(node, line, column, Some(&target.name));
                    let hop = fold.first.clone().unwrap_or_else(|| local.clone());
                    let mut site = self.with_args(arrival.site.clone(), node, line, column);
                    site.decision = decision_of(&guards);
                    let mut whens = fold.whens.clone();
                    whens.push(guard_label(&guards));
                    let id = self.link(
                        fired_by.unwrap_or(step),
                        known,
                        "calls",
                        &fold.chain,
                        &whens,
                        site,
                        Some(edge),
                        arrival.trigger.clone(),
                        hop.within.clone(),
                    );
                    let loops = self
                        .reader
                        .loops(&node.file_path, node.language, line, column);
                    let known_id = self.steps[known].step.id.clone();
                    self.record(
                        node,
                        &local,
                        &guards,
                        Some(known_id),
                        Some(id),
                        None,
                        arrival.trigger.as_ref(),
                        loops,
                    );
                }
                continue;
            }
            // Plumbing: fold it and keep walking, unless it is a dead end.
            if visited.contains(&target.id) {
                continue;
            }
            if self.fan_in.get(&target.id).copied().unwrap_or(0) >= HUB_THRESHOLD {
                self.truncated_hubs += 1;
                continue;
            }
            if meta_str(&arrival.meta, "synthesizedBy") == Some("jsx-render")
                && self.render_parents_of(target)? >= SHARED_CHROME_MIN
            {
                self.truncated_chrome += 1;
                continue;
            }
            if visited.len() >= MAX_FOLDED_PER_STEP {
                let record = &mut self.steps[step].step;
                record.cut = record.cut.or(Some("folded"));
                continue;
            }
            visited.insert(target.id.clone());
            let guards = self
                .reader
                .guards(&node.file_path, node.language, line, column);
            let local = if matches!(edge.kind, EdgeKind::Calls | EdgeKind::Instantiates) {
                self.hop_at(node, line, column, Some(&target.name))
            } else {
                Self::point_hop(node, line, column)
            };
            let first = fold.first.clone().unwrap_or_else(|| local.clone());
            let loops = self
                .reader
                .loops(&node.file_path, node.language, line, column);
            self.record(
                node,
                &local,
                &guards,
                None,
                None,
                Some(target.id.clone()),
                arrival.trigger.as_ref(),
                loops,
            );
            let mut chain = fold.chain.clone();
            chain.push(target.clone());
            let mut whens = fold.whens.clone();
            whens.push(guard_label(&guards));
            next.push(Fold {
                node: target.clone(),
                chain,
                whens,
                first: Some(first),
            });
        }
        Ok(())
    }

    /// How many distinct parents render this node as a JSX child — a file
    /// renders at module scope and is no parent (#2611).
    fn render_parents_of(&mut self, node: &Node) -> ApiResult<usize> {
        if let Some(count) = self.render_parents.get(&node.id) {
            return Ok(*count);
        }
        let incoming = self
            .ctx
            .store
            .incoming_edges_to(std::slice::from_ref(&node.id), &[EdgeKind::Calls])?;
        let rendered: Vec<String> = incoming
            .iter()
            .filter(|edge| meta_str(&meta_of(edge), "synthesizedBy") == Some("jsx-render"))
            .map(|edge| edge.source.clone())
            .collect();
        let sources = self.ctx.store.nodes_by_ids(&rendered)?;
        let parents: HashSet<&String> = rendered
            .iter()
            .filter(|source| {
                sources
                    .get(*source)
                    .is_some_and(|node| node.kind != NodeKind::File)
            })
            .collect();
        let count = parents.len();
        self.render_parents.insert(node.id.clone(), count);
        Ok(count)
    }

    /// Function-as-value references — and, for a value, calls and
    /// navigations — made at a file's top level within a node's lines.
    fn file_scope_edges_within(&mut self, node: &Node, calls: bool) -> ApiResult<Vec<Edge>> {
        if !self.file_scope_edges.contains_key(&node.file_path) {
            let file_id = format!("file:{}", node.file_path);
            let edges: Vec<Edge> = self
                .ctx
                .store
                .outgoing_edges_from(
                    std::slice::from_ref(&file_id),
                    &[EdgeKind::References, EdgeKind::Calls, EdgeKind::Navigates],
                )?
                .into_iter()
                .filter(|edge| {
                    edge.kind != EdgeKind::References || meta_true(&meta_of(edge), "fnRef")
                })
                .collect();
            self.file_scope_edges.insert(node.file_path.clone(), edges);
        }
        Ok(self.file_scope_edges[&node.file_path]
            .iter()
            .filter(|edge| {
                (calls || edge.kind == EdgeKind::References)
                    && edge
                        .line
                        .is_some_and(|line| line >= node.start_line && line <= node.end_line)
            })
            .cloned()
            .collect())
    }

    /// The file scope's unresolved calls within a value's lines.
    fn file_scope_refs_within(&mut self, node: &Node) -> ApiResult<Vec<UnresolvedRef>> {
        if !self.file_scope_refs.contains_key(&node.file_path) {
            let file_id = format!("file:{}", node.file_path);
            let refs = self
                .ctx
                .store
                .unresolved_refs_from(&file_id)
                .unwrap_or_default();
            self.file_scope_refs.insert(node.file_path.clone(), refs);
        }
        Ok(self.file_scope_refs[&node.file_path]
            .iter()
            .filter(|reference| {
                reference.line >= node.start_line && reference.line <= node.end_line
            })
            .cloned()
            .collect())
    }

    // ------------------------------------------------------------- finish --

    fn finish(&mut self, first: usize) -> ApiResult<Value> {
        // An effect box with ONE call behind it says what that call passes;
        // a response box is labelled by its statuses.
        let mut sites_by_step: HashMap<String, Vec<WireStepSite>> = HashMap::new();
        for link in &self.links {
            sites_by_step
                .entry(link.to.clone())
                .or_default()
                .extend(link.sites.iter().cloned());
        }
        for record in &mut self.steps {
            let step = &mut record.step;
            if step.kind != "effect" {
                continue;
            }
            let Some(effect) = step.effect.as_mut() else {
                continue;
            };
            let sites = sites_by_step.get(&step.id).cloned().unwrap_or_default();
            if effect.category == "response" {
                let mut statuses: Vec<u16> = sites.iter().filter_map(|site| site.status).collect();
                statuses.sort_unstable();
                statuses.dedup();
                if !statuses.is_empty() {
                    step.label = statuses
                        .iter()
                        .map(u16::to_string)
                        .collect::<Vec<_>>()
                        .join(" · ");
                    effect.statuses = Some(statuses);
                    continue;
                }
            }
            if effect.apis.len() != 1 {
                continue;
            }
            let [only] = sites.as_slice() else {
                continue;
            };
            let Some(args) = &only.args else {
                continue;
            };
            let label = format!("{}({args})", effect.api);
            step.label = if label.encode_utf16().count() > MAX_EFFECT_LABEL {
                let mut cut: String = String::new();
                let mut units = 0;
                for ch in label.chars() {
                    units += ch.len_utf16();
                    if units > MAX_EFFECT_LABEL - 2 {
                        break;
                    }
                    cut.push(ch);
                }
                format!("{cut}…)")
            } else {
                label
            };
        }
        // A row reads in the code's order.
        let mut by_depth: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (index, record) in self.steps.iter().enumerate() {
            by_depth.entry(record.step.depth).or_default().push(index);
        }
        for row in by_depth.values_mut() {
            row.sort_by(|a, b| {
                let (a, b) = (&self.steps[*a], &self.steps[*b]);
                hop_compare(a.first.as_ref(), b.first.as_ref())
                    .then_with(|| locale_compare(&a.step.label, &b.step.label))
                    .then_with(|| locale_compare(&a.step.id, &b.step.id))
            });
            for (order, index) in row.iter().enumerate() {
                self.steps[*index].step.order = order;
            }
        }
        let mut ordered: Vec<&StepRecord> = self.steps.iter().collect();
        ordered.sort_by(|a, b| {
            a.step
                .depth
                .cmp(&b.step.depth)
                .then(a.step.order.cmp(&b.step.order))
                .then_with(|| locale_compare(&a.step.id, &b.step.id))
        });

        // The second reading: the same steps in the code's order.
        let mut nodes_by_id: HashMap<String, Node> = HashMap::new();
        for record in &self.steps {
            if let Some(root) = &record.root {
                nodes_by_id.insert(root.id.clone(), root.clone());
            }
        }
        let sites: HashMap<String, Vec<ProgramSite>> = self
            .programs
            .iter()
            .map(|(function, sites, _)| (function.clone(), sites.clone()))
            .collect();
        let store = self.ctx.store;
        let node = |id: &str| {
            nodes_by_id
                .get(id)
                .cloned()
                .or_else(|| store.node_by_id(id).ok().flatten())
                .map(|node| to_node_ref(&node))
        };
        let steps_ref = &self.steps;
        let index_ref = &self.step_index;
        let facts = |id: &str| {
            let record = &steps_ref[*index_ref.get(id)?];
            Some(StepFacts {
                reply: record
                    .step
                    .effect
                    .as_ref()
                    .is_some_and(|effect| effect.category == "response"),
                into: match (&record.step.cut, &record.root) {
                    (None, Some(root)) => Some(root.id.clone()),
                    _ => None,
                },
            })
        };
        let first_record = &self.steps[first];
        let program: Option<WireProgram> = build_program(&ProgramInput {
            sites: &sites,
            root: first_record.root.as_ref().map(|root| root.id.clone()),
            node: &node,
            step: &facts,
        });
        let screen_anchor = first_record.step.kind == "screen"
            && !first_record
                .step
                .screen
                .as_ref()
                .is_some_and(|screen| screen.endpoint);
        let default_view = if program.is_some() && !screen_anchor {
            "order"
        } else {
            "tree"
        };
        let mut links: Vec<&WireStepLink> = self.links.iter().collect();
        links.sort_by(|a, b| locale_compare(&a.id, &b.id));
        let steps: Vec<&WireStep> = ordered.iter().map(|record| &record.step).collect();
        Ok(json!({
            "anchor": Value::Null,
            "ambiguous": [],
            "project": self.project,
            "steps": steps,
            "links": links,
            "program": program,
            "defaultView": default_view,
            "depth": self.depth_cap,
            "limit": self.limit,
            "through": self.through,
            "truncated": {
                "steps": self.truncated_steps,
                "hubs": self.truncated_hubs,
                "chrome": self.truncated_chrome,
            },
            "index": Value::Null,
            "timing": Value::Null,
        }))
    }
}

/// The last segment of a qualified name's container: `UsersRepository` of
/// `src.users.UsersRepository.find`.
fn container_name(qualified: &str) -> String {
    let container = match qualified.rfind(['.', ':']) {
        Some(at) => qualified[..at].trim_end_matches(['.', ':']),
        None => qualified,
    };
    container
        .rsplit(['.', ':'])
        .find(|segment| !segment.is_empty())
        .unwrap_or("")
        .to_string()
}

/// A declared type in a field's signature: `OwnerRepository owners`,
/// `private final OwnerRepository owners`, `owners: OwnerRepository`
/// (upstream's two patterns, the name spliced in with [`regex::escape`] as
/// upstream escapes it).
fn typed_signature(signature: &str, name: &str) -> Option<String> {
    if name.is_empty() || !signature.contains(name) {
        return None;
    }
    let name = regex::escape(name);
    let before = Regex::new(&format!(
        r"(?:^|\s)([A-Z][0-9A-Za-z_<>,?. ]*?)\s+{name}(?-u:\b)"
    ))
    .ok()?;
    if let Some(found) = before.captures(signature) {
        return Some(found[1].trim().to_string());
    }
    let after = Regex::new(&format!(r"(?-u:\b){name}\s*:\s*([A-Z][0-9A-Za-z_<>,?. ]*)")).ok()?;
    after
        .captures(signature)
        .map(|found| found[1].trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str) -> Node {
        Node {
            id: format!("function:{name}"),
            kind: NodeKind::Function,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: "src/users.ts".to_string(),
            language: Language::TypeScript,
            start_line: 1,
            end_line: 9,
            start_column: 0,
            end_column: 1,
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

    fn call_at(line: Option<i64>, col: Option<i64>) -> Edge {
        Edge {
            id: None,
            source: "function:caller".to_string(),
            target: "function:create".to_string(),
            kind: EdgeKind::Calls,
            metadata: None,
            line,
            col,
            provenance: None,
        }
    }

    fn written(names: &[&str]) -> HashMap<(String, i64, i64), Vec<String>> {
        HashMap::from([(
            ("function:caller".to_string(), 3, 4),
            names.iter().map(|name| name.to_string()).collect(),
        )])
    }

    /// The name a call was written with comes from the resolved reference
    /// at its site: the only one, else the one whose last segment is the
    /// target's name; with no such choice the target's name stands in.
    #[test]
    fn a_call_is_named_as_written_at_its_site() {
        let create = node("create");
        let at = call_at(Some(3), Some(4));
        let name = |names: &[&str]| Walk::ref_name(&at, &create, &written(names));
        assert_eq!(
            name(&["prisma.user.create"]).as_deref(),
            Some("prisma.user.create")
        );
        // One name written twice is one name.
        assert_eq!(
            name(&["db.create", "db.create"]).as_deref(),
            Some("db.create")
        );
        assert_eq!(
            name(&["log", "prisma.user.create"]).as_deref(),
            Some("prisma.user.create")
        );
        // Two that end in the target's name, or none that does: no choice.
        assert_eq!(name(&["a.create", "b.create"]), None);
        assert_eq!(name(&["log", "warn"]), None);
        // Nothing resolved at the site, or an edge without a position.
        let elsewhere = call_at(Some(5), Some(4));
        assert_eq!(
            Walk::ref_name(&elsewhere, &create, &written(&["prisma.user.create"])),
            None
        );
        let nowhere = call_at(None, None);
        assert_eq!(
            Walk::ref_name(&nowhere, &create, &written(&["prisma.user.create"])),
            None
        );
    }

    /// A field's declared type, with the field's name spliced into the
    /// pattern escaped (A 109): a name with a metacharacter matches itself
    /// and nothing else.
    #[test]
    fn reads_a_fields_declared_type_with_its_name_escaped() {
        assert_eq!(
            typed_signature("private final OwnerRepository owners", "owners").as_deref(),
            Some("OwnerRepository")
        );
        assert_eq!(
            typed_signature("owners: OwnerRepository", "owners").as_deref(),
            Some("OwnerRepository")
        );
        assert_eq!(
            typed_signature("Repo $repo", "$repo").as_deref(),
            Some("Repo")
        );
        assert_eq!(
            typed_signature("Repo rexo; Other re.o", "re.o").as_deref(),
            Some("Other")
        );
        assert_eq!(typed_signature("OwnerRepository owners", "other"), None);
    }
}
