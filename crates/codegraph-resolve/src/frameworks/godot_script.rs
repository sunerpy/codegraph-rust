//! `.gd` GDScript dynamic call-site parser — L3 of Godot static analysis (T6).
//!
//! Recognizes Godot's DYNAMIC dispatch call-sites in GDScript source and emits
//! one [`RefView`] reference per call-site, FROM the enclosing function (or the
//! file node when no function encloses it). [`crate::frameworks::godot::GodotResolver::extract`]
//! dispatches here when a file's basename ends in `.gd` (and only for projects
//! where `detect()` saw a `project.godot`). Cross-file resolution of the emitted
//! reference names to their actual handler/method/signal/node DEFINITIONS is
//! T7's `post_extract` job; this layer only recognizes + emits names.
//!
//! # Why a resolver layer (not the GDScript [`LanguageSpec`])
//!
//! The base GDScript symbol extraction (`crates/codegraph-extract/src/lang/
//! gdscript.rs`) is GOLDEN-PROTECTED — changing its output would break existing
//! GDScript goldens. These dynamic references are PURELY ADDITIVE and only ever
//! emitted inside a detected Godot project, so they live ENTIRELY in the
//! `GodotResolver` and never touch the language spec. A plain non-Godot `.gd`
//! repo (no `project.godot`) never activates the resolver, so its output is
//! byte-unchanged.
//!
//! [`LanguageSpec`]: codegraph_extract
//!
//! # Patterns recognized (each emits one reference per call-site)
//!
//! LITERAL target — the argument is a string/identifier literal, so the target
//! NAME is statically known. Emitted as a normal [`RefView`] (T7 resolves it):
//!
//! - `X.connect(handler)` / `signal.timeout.connect(_on_timeout)` — signal
//!   connection → reference to the handler method NAME ([`EdgeKind::Calls`], it
//!   is a deferred call of that handler).
//! - `emit_signal("sig")` / `some_signal.emit()` — signal emission → reference
//!   to the signal NAME ([`EdgeKind::References`]).
//! - `get_node("Path")` / `$NodePath` / `$"Path"` / `%UniqueName` — node access
//!   → reference to the node path / unique name ([`EdgeKind::References`]).
//! - `get_nodes_in_group("g")` / `add_to_group("g")` / `is_in_group("g")` —
//!   group query/membership → reference to the group NAME ([`EdgeKind::References`]).
//! - `has_method("m")` / `call("m", …)` / `call_deferred("m", …)` — dynamic
//!   method dispatch → reference to the method NAME ([`EdgeKind::Calls`]).
//!
//! COMPUTED / NON-LITERAL target — the argument is a variable/expression, so the
//! target is NOT statically known (e.g. `get_node(var_path)`, `call(method_var)`,
//! `emit_signal(sig_var)`). We do NOT fabricate an edge. Instead we record a
//! DYNAMIC-UNRESOLVED reference so T8 (MCP/CLI) can surface it as "dynamic,
//! unconfirmable" — see the representation note below.
//!
//! # Dynamic-unresolved representation (the T6 structural decision)
//!
//! [`RefView`] (types.rs:18-37) has NO field to flag a reference as
//! dynamic/unconfirmable — adding one would ripple into the persisted
//! `UnresolvedRef` row + every resolver. So a computed call-site is encoded by a
//! SENTINEL PREFIX on `reference_name`: [`DYNAMIC_PREFIX`]
//! (`"godot:dynamic:"`) followed by the call kind (e.g.
//! `godot:dynamic:get_node`). The reference_kind stays the honest
//! [`EdgeKind::References`]/[`EdgeKind::Calls`] for the call shape. Because the
//! name is a synthetic sentinel that no real symbol carries, T7's name-match
//! resolution can never accidentally bind it to a definition (it stays
//! unresolved by construction), and T8 categorizes it by matching the
//! [`DYNAMIC_PREFIX`] — "this call dispatches dynamically; the target cannot be
//! statically confirmed". This keeps `RefView` untouched and needs no new
//! EdgeKind/NodeKind/dependency.
//!
//! # Autoload-singleton access — DEFERRED to a follow-up
//!
//! Recognizing `Autoload.method()` / `Autoload.member` requires knowing the
//! autoload NAMES, which come from `project.godot`'s `[autoload]` section (parsed
//! separately by L1) and are NOT available at this per-file `extract()` time. The
//! task allows emitting `IdentifierStartingUppercase.member` as a "candidate
//! autoload" reference for T7 to confirm, BUT a bare uppercase-dot-member scan
//! over GDScript is far too noisy: every `Vector2.ZERO`, `Input.is_action_*`,
//! `Color.RED`, `Engine.get_*`, every class-name constructor, and every built-in
//! type access starts with an uppercase identifier. Emitting all of those would
//! flood the graph with false-positive references that T7 would then have to
//! reject by name. Per the task's explicit "if too noisy, scope this to a
//! follow-up and document" guidance, autoload-candidate recognition is DEFERRED:
//! L3 ships the literal signal/get_node/group/method patterns (the stated
//! priority) cleanly, and T7 can revisit autoload access with the actual
//! `[autoload]` name set in hand (it CAN match `Autoload.x` against the known
//! singleton names there, with zero false positives, which is the right place
//! for it).
//!
//! # Enclosing-function attribution
//!
//! Each reference originates from the nearest preceding `func <name>(...)`
//! header (the GDScript function the call-site sits in), via a deterministic
//! synthesized [`NodeKind::Function`] id for that function name+line — the same
//! id the base GDScript spec would generate for that function, so T7 can line
//! the dynamic refs up with the real function symbols. A call-site before any
//! `func` (top-level / field initializer) is attributed to the `file:{relpath}`
//! node. We emit NO function nodes ourselves (the base spec owns those); we only
//! reference their ids as the `from_node_id`.
//!
//! # Tolerance
//!
//! Every line is scanned defensively: an unterminated string, an unbalanced
//! paren, a malformed `func` header, or an unrecognized line is skipped, never
//! panics. A `.gd` with no dynamic patterns yields an empty result.

use codegraph_core::node_id::generate_node_id;
use codegraph_core::types::{EdgeKind, Language, NodeKind};

use super::godot_common::quoted_strings;
use crate::types::{FrameworkResolverExtractionResult, RefView};

/// Sentinel prefix marking a reference whose target is dynamic / non-literal and
/// therefore statically unconfirmable. T8 categorizes a reference as
/// "dynamic, unconfirmable" by testing `reference_name.starts_with(DYNAMIC_PREFIX)`.
/// The suffix after the prefix is the call kind (`get_node` / `call` /
/// `emit_signal` / `connect`) for human-readable surfacing.
pub const DYNAMIC_PREFIX: &str = "godot:dynamic:";

/// `true` when `file_path`'s basename ends in `.gd` (case-sensitive, matching
/// Godot's own extension). Matches by extension the same defensive way L2/L4
/// match `.tscn`/`.tres`, so nested paths dispatch too.
pub(crate) fn is_gdscript(file_path: &str) -> bool {
    file_path
        .rsplit(['/', '\\'])
        .next()
        .is_some_and(|base| base.ends_with(".gd"))
}

/// Parse a `.gd` GDScript file into dynamic-dispatch references.
///
/// Deterministic: references are emitted in source order; the enclosing-function
/// id follows the upstream `{kind}:{sha256("{filePath}:{kind}:{name}:{line}").hex[:32]}`
/// formula via [`generate_node_id`]. Lines are 1-based. Emits NO nodes (the base
/// GDScript spec owns the function/class/signal symbols); only references.
pub(crate) fn parse_gdscript_dynamics(
    file_path: &str,
    content: &str,
) -> FrameworkResolverExtractionResult {
    let mut references: Vec<RefView> = Vec::new();

    // The id of the function the current line sits in. `None` until the first
    // `func` header — top-level call-sites attribute to the file node.
    let mut current_fn: Option<String> = None;
    let file_node_id = format!("file:{file_path}");

    for (idx, raw_line) in content.lines().enumerate() {
        let line_no = (idx + 1) as i64;
        let line = raw_line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Track the enclosing function. A `func name(...)` header opens a new
        // function scope (GDScript has no block-closing token; the next `func`
        // simply replaces it — good enough for attribution).
        if let Some(fn_name) = parse_func_header(line) {
            current_fn = Some(generate_node_id(
                file_path,
                NodeKind::Function,
                fn_name,
                line_no as u32,
            ));
            // The `func` header line itself carries no dynamic call-site.
            continue;
        }

        let from = current_fn.clone().unwrap_or_else(|| file_node_id.clone());
        scan_line(file_path, line_no, line, &from, &mut references);
    }

    FrameworkResolverExtractionResult {
        nodes: Vec::new(),
        references,
    }
}

/// Parse a `.gd` GDScript file into AUTOLOAD-CANDIDATE references (the T7
/// deferred-from-L3 work).
///
/// Emits a `Receiver.member` reference ([`EdgeKind::Calls`]) for every
/// `Uppercase.member` access whose receiver is a bare uppercase-initial
/// identifier (`BuffManager.apply`, `GameState.score`). This is the
/// over-emitting half: it deliberately does NOT know which receivers are real
/// autoloads (that roster lives in `project.godot`, unavailable per-file). The
/// roster gate is [`crate::frameworks::godot::GodotResolver::resolve`], which
/// binds ONLY receivers matching a known autoload singleton and returns `None`
/// for everything else (`Vector2.ZERO`, `Input.is_action_pressed`, class
/// constructors). A non-autoload candidate therefore stays unresolved and
/// produces NO edge — zero false positives by construction.
///
/// Kept SEPARATE from [`parse_gdscript_dynamics`] so the L3 dynamic-dispatch
/// output is byte-unchanged; [`crate::frameworks::godot::GodotResolver::extract`]
/// merges both reference sets for a `.gd` file. Emits NO nodes. Attributes each
/// reference to the enclosing `func` (or the file node), exactly like L3.
pub(crate) fn parse_autoload_candidates(
    file_path: &str,
    content: &str,
) -> FrameworkResolverExtractionResult {
    let mut references: Vec<RefView> = Vec::new();
    let mut current_fn: Option<String> = None;
    let file_node_id = format!("file:{file_path}");

    for (idx, raw_line) in content.lines().enumerate() {
        let line_no = (idx + 1) as i64;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(fn_name) = parse_func_header(line) {
            current_fn = Some(generate_node_id(
                file_path,
                NodeKind::Function,
                fn_name,
                line_no as u32,
            ));
            continue;
        }
        let from = current_fn.clone().unwrap_or_else(|| file_node_id.clone());
        scan_autoload_candidates(file_path, line_no, line, &from, &mut references);
    }

    FrameworkResolverExtractionResult {
        nodes: Vec::new(),
        references,
    }
}

/// Scan one line for `Uppercase.member` accesses and push a `Receiver.member`
/// candidate reference per access. Only an uppercase-initial receiver at an
/// identifier boundary qualifies (a lowercase `local.method` is an instance
/// call, never an autoload). A receiver preceded by `.` (a chained
/// `a.B.c`) is skipped — only the leftmost receiver in a chain can be an
/// autoload singleton.
fn scan_autoload_candidates(
    file_path: &str,
    line_no: i64,
    line: &str,
    from: &str,
    out: &mut Vec<RefView>,
) {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if !(b.is_ascii_uppercase()) {
            i += 1;
            continue;
        }
        if i > 0 && (is_ident_byte(bytes[i - 1]) || bytes[i - 1] == b'.') {
            i += 1;
            continue;
        }
        let start = i;
        let mut j = i + 1;
        while j < bytes.len() && is_ident_byte(bytes[j]) {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'.' {
            i = j.max(start + 1);
            continue;
        }
        let member_start = j + 1;
        let mut k = member_start;
        while k < bytes.len() && is_ident_byte(bytes[k]) {
            k += 1;
        }
        if k > member_start {
            let name = &line[start..k];
            out.push(reference(from, name, EdgeKind::Calls, line_no, file_path));
        }
        i = k.max(start + 1);
    }
}

/// Scan a single (already function-attributed) line for every dynamic call-site
/// pattern and push the resulting references. A line may carry more than one
/// pattern (e.g. `$Sprite` and `.connect(...)`), so each scanner runs
/// independently and emits in a fixed order.
fn scan_line(file_path: &str, line_no: i64, line: &str, from: &str, out: &mut Vec<RefView>) {
    scan_call_with_string_arg(file_path, line_no, line, from, out);
    scan_connect(file_path, line_no, line, from, out);
    scan_emit_member(file_path, line_no, line, from, out);
    scan_dollar_node(file_path, line_no, line, from, out);
    scan_unique_node(file_path, line_no, line, from, out);
}

/// Call-shaped patterns whose FIRST argument is the target name:
/// `get_node(...)`, `emit_signal(...)`, `get_nodes_in_group(...)`,
/// `add_to_group(...)`, `is_in_group(...)`, `has_method(...)`, `call(...)`,
/// `call_deferred(...)`. A literal first arg → reference to that name; a
/// non-literal (variable/expression) first arg → a dynamic-unresolved sentinel.
fn scan_call_with_string_arg(
    file_path: &str,
    line_no: i64,
    line: &str,
    from: &str,
    out: &mut Vec<RefView>,
) {
    for spec in CALL_SPECS {
        let mut search_from = 0usize;
        while let Some(rel) = line[search_from..].find(spec.name) {
            let name_start = search_from + rel;
            let after = name_start + spec.name.len();
            search_from = after; // advance past this occurrence regardless

            // The call name must be a standalone identifier head: the char
            // before it must not be an identifier char (so `get_node` does not
            // match inside `my_get_node`), and the next non-space char must be
            // `(`.
            if !is_ident_boundary_before(line, name_start) {
                continue;
            }
            let rest = line[after..].trim_start();
            let Some(args) = rest.strip_prefix('(') else {
                continue;
            };
            let target = first_arg(args);
            match literal_or_dynamic(target) {
                ArgKind::Literal(name) if !name.is_empty() => {
                    out.push(reference(from, name, spec.kind, line_no, file_path));
                }
                ArgKind::Literal(_) => {} // empty literal — nothing to reference
                ArgKind::Dynamic => {
                    out.push(reference(
                        from,
                        &dynamic_name(spec.short),
                        spec.kind,
                        line_no,
                        file_path,
                    ));
                }
            }
        }
    }
}

/// `X.connect(handler)` / `sig.timeout.connect(_on_timeout)` — the handler is the
/// FIRST argument and is normally a bare identifier (a method reference), not a
/// quoted string. A literal identifier first arg → reference to the handler NAME
/// ([`EdgeKind::Calls`]); a call-expression or non-identifier first arg →
/// dynamic-unresolved sentinel.
fn scan_connect(file_path: &str, line_no: i64, line: &str, from: &str, out: &mut Vec<RefView>) {
    let mut search_from = 0usize;
    while let Some(rel) = line[search_from..].find(".connect(") {
        let dot = search_from + rel;
        let after = dot + ".connect(".len();
        search_from = after;
        let args = &line[after..];
        let target = first_arg(args);
        match connect_handler(target) {
            ArgKind::Literal(name) if !name.is_empty() => {
                out.push(reference(from, name, EdgeKind::Calls, line_no, file_path));
            }
            ArgKind::Literal(_) => {}
            ArgKind::Dynamic => {
                out.push(reference(
                    from,
                    &dynamic_name("connect"),
                    EdgeKind::Calls,
                    line_no,
                    file_path,
                ));
            }
        }
    }
}

/// `some_signal.emit()` — a signal emission via the `.emit()` method (Godot 4
/// signal-as-object syntax). The signal NAME is the member to the LEFT of
/// `.emit(`. Emits a reference to that signal name ([`EdgeKind::References`]).
/// (`emit_signal("name")` is handled by [`scan_call_with_string_arg`].)
fn scan_emit_member(file_path: &str, line_no: i64, line: &str, from: &str, out: &mut Vec<RefView>) {
    let mut search_from = 0usize;
    while let Some(rel) = line[search_from..].find(".emit(") {
        let dot = search_from + rel;
        search_from = dot + ".emit(".len();
        // The signal name is the identifier immediately before `.emit`.
        if let Some(name) = ident_before(line, dot) {
            // Skip `emit_signal` false hit (it has no `.emit(` substring anyway)
            // and skip when the left side is itself `emit` (unlikely).
            out.push(reference(
                from,
                name,
                EdgeKind::References,
                line_no,
                file_path,
            ));
        }
    }
}

/// `$NodePath` / `$"Quoted/Path"` — the Godot get_node shorthand. Emits a
/// reference to the node path ([`EdgeKind::References`]). A bare `$` with no path
/// (or `$(expr)` dynamic form) emits nothing literal.
fn scan_dollar_node(file_path: &str, line_no: i64, line: &str, from: &str, out: &mut Vec<RefView>) {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        // Don't treat `$` inside a string literal head loosely; GDScript `$` is
        // an operator, so a preceding identifier char would be unusual. Accept
        // it regardless of left context (matches Godot's own lexing).
        let after = i + 1;
        if after >= bytes.len() {
            break;
        }
        if bytes[after] == b'"' {
            // `$"Quoted/Path"` — take the quoted string.
            if let Some(path) = quoted_strings(&line[after..]).into_iter().next() {
                if !path.is_empty() {
                    out.push(reference(
                        from,
                        path,
                        EdgeKind::References,
                        line_no,
                        file_path,
                    ));
                }
                // advance past the closing quote
                i = after + 1;
                continue;
            }
            i = after;
            continue;
        }
        // `$NodePath` — a bare path token: identifier chars plus `/` and `%`.
        let start = after;
        let mut j = start;
        while j < bytes.len() && is_node_path_byte(bytes[j]) {
            j += 1;
        }
        if j > start {
            out.push(reference(
                from,
                &line[start..j],
                EdgeKind::References,
                line_no,
                file_path,
            ));
        }
        i = j.max(after);
    }
}

/// `%UniqueName` — the Godot scene-unique-node shorthand. Emits a reference to
/// the unique name ([`EdgeKind::References`]). Only matched when `%` is followed
/// by an identifier head (so the `%` modulo operator, which is followed by a
/// space/number, is not mistaken for a unique-node access).
fn scan_unique_node(file_path: &str, line_no: i64, line: &str, from: &str, out: &mut Vec<RefView>) {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            i += 1;
            continue;
        }
        let after = i + 1;
        // A unique-node access requires an identifier head right after `%`
        // (`%Health`); `a % b` (modulo) has a space/operand and is skipped.
        if after < bytes.len() && (bytes[after].is_ascii_alphabetic() || bytes[after] == b'_') {
            let start = after;
            let mut j = start;
            while j < bytes.len() && is_node_path_byte(bytes[j]) {
                j += 1;
            }
            out.push(reference(
                from,
                &line[start..j],
                EdgeKind::References,
                line_no,
                file_path,
            ));
            i = j;
        } else {
            i = after;
        }
    }
}

/// A recognized call name and the [`EdgeKind`] + short label its reference uses.
struct CallSpec {
    /// The call identifier as it appears in source (without the `(`).
    name: &'static str,
    /// The edge kind for a resolved reference of this call.
    kind: EdgeKind,
    /// The short label used in the dynamic-unresolved sentinel name.
    short: &'static str,
}

/// All `call(name, …)`-shaped patterns whose first argument is the target.
/// Order is fixed for deterministic emission when a line carries several.
/// NOTE: `get_nodes_in_group` and `get_node` both contain `get_node` as a
/// substring; the boundary check in [`scan_call_with_string_arg`] keys on the
/// char AFTER the name being `(` (after trimming), so `get_node(` only matches
/// the real `get_node(` call and not `get_nodes_in_group(` (whose char after
/// `get_node` is `s`). Listing the longer names first is therefore not required,
/// but keeps the intent clear.
const CALL_SPECS: &[CallSpec] = &[
    CallSpec {
        name: "get_nodes_in_group",
        kind: EdgeKind::References,
        short: "get_nodes_in_group",
    },
    CallSpec {
        name: "add_to_group",
        kind: EdgeKind::References,
        short: "add_to_group",
    },
    CallSpec {
        name: "is_in_group",
        kind: EdgeKind::References,
        short: "is_in_group",
    },
    CallSpec {
        name: "get_node",
        kind: EdgeKind::References,
        short: "get_node",
    },
    CallSpec {
        name: "emit_signal",
        kind: EdgeKind::References,
        short: "emit_signal",
    },
    CallSpec {
        name: "has_method",
        kind: EdgeKind::Calls,
        short: "has_method",
    },
    CallSpec {
        name: "call_deferred",
        kind: EdgeKind::Calls,
        short: "call_deferred",
    },
    CallSpec {
        name: "call",
        kind: EdgeKind::Calls,
        short: "call",
    },
];

/// Classification of a call's first argument.
enum ArgKind<'a> {
    /// A string-literal argument (the inner text), statically known.
    Literal(&'a str),
    /// A variable / expression argument — statically unknown.
    Dynamic,
}

/// Classify a call's first-argument text as a string literal or a dynamic
/// expression. A leading `"` (after trim) makes it a literal (its inner text);
/// anything else (an identifier, a member access, a call) is dynamic.
fn literal_or_dynamic(arg: &str) -> ArgKind<'_> {
    let arg = arg.trim();
    if arg.starts_with('"') {
        match quoted_strings(arg).into_iter().next() {
            Some(inner) => ArgKind::Literal(inner),
            None => ArgKind::Dynamic, // unterminated quote — treat as dynamic
        }
    } else {
        ArgKind::Dynamic
    }
}

/// Classify a `.connect(...)` first argument. The handler is normally a bare
/// method-reference identifier (`_on_timeout`) in Godot 4 — a literal target.
/// A `Callable(...)`/`func(...)`/member-call or non-identifier is dynamic.
fn connect_handler(arg: &str) -> ArgKind<'_> {
    let arg = arg.trim();
    if arg.is_empty() {
        return ArgKind::Dynamic;
    }
    // A plain identifier (optionally `self.method` → take the trailing method)
    // is a literal handler name. Anything containing `(` is a call/expression.
    if arg.contains('(') {
        return ArgKind::Dynamic;
    }
    // `self._on_timeout` / `obj.method` → the trailing identifier segment.
    let last = arg.rsplit('.').next().unwrap_or(arg).trim();
    if is_plain_ident(last) {
        ArgKind::Literal(last)
    } else {
        ArgKind::Dynamic
    }
}

/// Extract the first argument substring from a call's argument list (the text
/// just after the opening `(`), stopping at the top-level `,` or the matching
/// `)`. Paren depth is tracked so a nested call/array in the first arg stays
/// whole. Quotes are honored so a `,`/`)` inside a string does not terminate.
fn first_arg(args: &str) -> &str {
    let bytes = args.as_bytes();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if b == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                if depth == 0 {
                    return args[..i].trim();
                }
                depth -= 1;
            }
            b',' if depth == 0 => return args[..i].trim(),
            _ => {}
        }
        i += 1;
    }
    args.trim()
}

/// Parse a `func <name>(...)` header → the function name. Returns `None` for any
/// line that is not a function header. Handles `static func`, leading
/// annotations are on their own line so not considered here.
fn parse_func_header(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("func ")
        .or_else(|| line.strip_prefix("static func "))?;
    let rest = rest.trim_start();
    let end = rest.find(['(', ' ', '\t']).unwrap_or(rest.len());
    let name = rest[..end].trim();
    if name.is_empty() || !is_plain_ident(name) {
        return None;
    }
    Some(name)
}

/// The identifier immediately to the LEFT of byte index `dot` (the `.` of
/// `.emit(`), if it is a plain identifier. Used to read the signal name in
/// `signal_name.emit()`.
fn ident_before(line: &str, dot: usize) -> Option<&str> {
    let bytes = line.as_bytes();
    let end = dot;
    let mut start = end;
    while start > 0 && is_ident_byte(bytes[start - 1]) {
        start -= 1;
    }
    if start == end {
        return None;
    }
    // The char before the identifier must not be `.` chained onto a call result
    // we cannot name (e.g. `get_x().emit()`); require a boundary.
    if start > 0 && bytes[start - 1] == b')' {
        return None;
    }
    let name = &line[start..end];
    // A leading digit would not be a valid identifier.
    if name.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    Some(name)
}

/// `true` when the byte preceding `pos` is NOT an identifier byte (so the call
/// name at `pos` is a standalone token, not a suffix of a longer identifier).
fn is_ident_boundary_before(line: &str, pos: usize) -> bool {
    if pos == 0 {
        return true;
    }
    !is_ident_byte(line.as_bytes()[pos - 1])
}

/// `true` for bytes valid inside a node path token (`Player/Sprite2D`,
/// `Health`): identifier chars plus `/` and `%` (unique-node within a path).
fn is_node_path_byte(b: u8) -> bool {
    is_ident_byte(b) || b == b'/' || b == b'%'
}

/// `true` for an identifier byte (`[A-Za-z0-9_]`).
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `true` when `s` is a non-empty plain identifier (`[A-Za-z_][A-Za-z0-9_]*`).
fn is_plain_ident(s: &str) -> bool {
    let mut chars = s.bytes();
    match chars.next() {
        Some(b) if b.is_ascii_alphabetic() || b == b'_' => {}
        _ => return false,
    }
    chars.all(is_ident_byte)
}

/// Build the dynamic-unresolved sentinel name for a call kind, e.g.
/// `godot:dynamic:get_node`.
fn dynamic_name(short: &str) -> String {
    format!("{DYNAMIC_PREFIX}{short}")
}

/// Build a reference edge from the enclosing function/file node to a target NAME
/// (or a dynamic sentinel name).
fn reference(
    from_node_id: &str,
    name: &str,
    kind: EdgeKind,
    line_no: i64,
    file_path: &str,
) -> RefView {
    RefView {
        from_node_id: from_node_id.to_string(),
        reference_name: name.to_string(),
        reference_kind: kind,
        line: line_no,
        column: 0,
        file_path: file_path.to_string(),
        language: Language::Gdscript,
        is_function_ref: false,
    }
}
