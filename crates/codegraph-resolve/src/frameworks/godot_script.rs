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

/// Sentinel prefix marking an autoload-method-call candidate (F1); the suffix is
/// the literal `Receiver.member` access (`godot:autoload_call:GameFlow.return_to_map`).
/// Emitted as a SECOND candidate beside the plain `Receiver.member`
/// autoload-singleton candidate ([`parse_autoload_candidates`]): the plain one
/// resolves to the singleton `Constant`; this one is roster-gated by
/// [`crate::frameworks::godot::GodotResolver::resolve`] and resolves to the
/// backing script's `func` only when that script holds EXACTLY ONE same-named
/// `func`. The distinct sentinel name — carried by no real symbol — keeps the
/// two candidates as separate `unresolved_refs` rows (two independent edges) and
/// prevents the generic name-matcher from ever binding it on its own.
pub const AUTOLOAD_CALL_PREFIX: &str = "godot:autoload_call:";

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
        scan_autoload_accesses(
            file_path,
            line_no,
            line,
            &from,
            &mut references,
            |out, from, line_no, file_path, access| {
                out.push(reference(from, access, EdgeKind::Calls, line_no, file_path));
            },
        );
    }

    FrameworkResolverExtractionResult {
        nodes: Vec::new(),
        references,
    }
}

/// Parse a `.gd` GDScript file into AUTOLOAD-METHOD-CALL candidate references
/// (F1). Emits one [`AUTOLOAD_CALL_PREFIX`]-prefixed [`EdgeKind::Calls`]
/// reference (`godot:autoload_call:Receiver.member`) per `Uppercase.member`
/// access — the exact same match set as [`parse_autoload_candidates`], scanned
/// via the shared [`scan_autoload_accesses`], but named with the sentinel prefix
/// so the resolver can route it to the backing script's `func`. Over-emitting by
/// design (every uppercase receiver, not just real autoloads); the roster gate in
/// [`crate::frameworks::godot::GodotResolver::resolve`] binds only real
/// `res://`-bound autoloads with a unique same-named `func`, so a non-autoload or
/// ambiguous receiver stays unresolved and produces NO edge. Kept SEPARATE from
/// the dynamic-dispatch and singleton-candidate parsers so each output stays
/// byte-stable; [`crate::frameworks::godot::GodotResolver::extract`] merges all
/// three reference sets for a `.gd` file. Emits NO nodes.
pub(crate) fn parse_autoload_func_candidates(
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
        scan_autoload_accesses(
            file_path,
            line_no,
            line,
            &from,
            &mut references,
            |out, from, line_no, file_path, access| {
                let name = format!("{AUTOLOAD_CALL_PREFIX}{access}");
                out.push(reference(from, &name, EdgeKind::Calls, line_no, file_path));
            },
        );
    }

    FrameworkResolverExtractionResult {
        nodes: Vec::new(),
        references,
    }
}

/// Scan one line for `Uppercase.member` accesses and push, per access, the
/// candidate references named by `emit`. Only an uppercase-initial receiver at an
/// identifier boundary qualifies (a lowercase `local.method` is an instance
/// call, never an autoload). A receiver preceded by `.` (a chained
/// `a.B.c`) is skipped — only the leftmost receiver in a chain can be an
/// autoload singleton. `emit(out, from, line_no, file_path, access)` receives the
/// matched `Receiver.member` text so the two callers can name their candidate
/// distinctly (plain singleton candidate vs. [`AUTOLOAD_CALL_PREFIX`] func
/// candidate) off the identical match set.
fn scan_autoload_accesses(
    file_path: &str,
    line_no: i64,
    line: &str,
    from: &str,
    out: &mut Vec<RefView>,
    emit: impl Fn(&mut Vec<RefView>, &str, i64, &str, &str),
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
            emit(out, from, line_no, file_path, &line[start..k]);
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
///
/// Recognized statically-resolvable forms (all → [`ArgKind::Literal`] of the
/// handler method name):
/// - bare identifier: `_on_timeout` (also `self._on_timeout` → `_on_timeout`).
/// - bound callable: `_on_timeout.bind(x)` — the head segment before `.bind(`
///   must be a plain identifier (`self._on_x.bind(..)` → `_on_x`).
/// - explicit callable with a `self`/`this` receiver and a string-literal
///   method name: `Callable(self, "handler")` / `Callable(this, "handler")`.
///
/// Everything else — a variable handler, a `Callable(other, "m")` with a
/// non-self/this receiver, a non-literal method name, a `.bind` chain whose head
/// is not a plain identifier, or any other call/expression — is [`ArgKind::Dynamic`].
fn connect_handler(arg: &str) -> ArgKind<'_> {
    let arg = arg.trim();
    if arg.is_empty() {
        return ArgKind::Dynamic;
    }
    // `Callable(self, "handler")` / `Callable(this, "handler")` — extract the
    // string-literal method name, but ONLY when the receiver is `self`/`this`.
    if let Some(inner) = arg
        .strip_prefix("Callable(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return callable_handler(inner);
    }
    // `_handler.bind(x)` — the handler is the segment BEFORE `.bind(`, which must
    // itself reduce to a plain identifier (its trailing `.`-segment).
    if let Some(head) = split_bind_head(arg) {
        let last = head.rsplit('.').next().unwrap_or(head).trim();
        return if is_plain_ident(last) {
            ArgKind::Literal(last)
        } else {
            ArgKind::Dynamic
        };
    }
    // A plain identifier (optionally `self.method` → take the trailing method)
    // is a literal handler name. Anything else containing `(` is a call/expression.
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

/// Classify the inside of a `Callable(...)` connect handler. `inner` is the text
/// between the outer parens of `Callable(<receiver>, <method>)`. Returns the
/// string-literal method name as a literal ONLY when the receiver is exactly
/// `self` or `this`; any other receiver, a non-literal method, or a malformed
/// arg list is dynamic (cross-object callables are not statically resolvable).
fn callable_handler(inner: &str) -> ArgKind<'_> {
    let comma = match top_level_comma(inner) {
        Some(i) => i,
        None => return ArgKind::Dynamic,
    };
    let receiver = inner[..comma].trim();
    if receiver != "self" && receiver != "this" {
        return ArgKind::Dynamic;
    }
    match literal_or_dynamic(inner[comma + 1..].trim()) {
        ArgKind::Literal(name) if !name.is_empty() && is_plain_ident(name) => {
            ArgKind::Literal(name)
        }
        _ => ArgKind::Dynamic,
    }
}

/// If `arg` is a `.bind(...)` chain, return the head expression before the FIRST
/// top-level `.bind(` — the callable being bound. `None` when there is no
/// `.bind(` at top level (paren/quote depth 0). A leading `.bind(` (empty head)
/// yields `Some("")`, which the caller rejects as a non-ident head.
fn split_bind_head(arg: &str) -> Option<&str> {
    let bytes = arg.as_bytes();
    let needle = b".bind(";
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
            b')' | b']' | b'}' => depth -= 1,
            _ => {
                if depth == 0 && bytes[i..].starts_with(needle) {
                    return Some(arg[..i].trim());
                }
            }
        }
        i += 1;
    }
    None
}

/// The byte index of the FIRST top-level `,` in `s` (paren/quote depth 0), or
/// `None` when there is no such separator. Used to split a `Callable(recv, m)`
/// argument list into receiver and method.
fn top_level_comma(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
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
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
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
        reference_subkind: None,
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for the private F2 connect-handler parser helpers and the
    //! shared scanning primitives. These target DEFENSIVE / EDGE-CASE branches
    //! that the public-`extract()` integration tests in
    //! `tests/godot_script.rs` do not reach: malformed / non-literal / non-ident
    //! inputs whose CORRECT behavior is a `Dynamic` classification, an emitted
    //! nothing, or a `None`. Every assertion pins observable behavior, not just
    //! line execution.
    use super::*;

    /// Convenience: match `ArgKind::Literal(x)` → `Some(x)`, `Dynamic` → `None`.
    fn lit(k: ArgKind<'_>) -> Option<&str> {
        match k {
            ArgKind::Literal(s) => Some(s),
            ArgKind::Dynamic => None,
        }
    }

    // ---- literal_or_dynamic (line 384/395 feeders + 633 unterminated) ----

    #[test]
    fn literal_or_dynamic_empty_quoted_is_empty_literal() {
        // `""` (empty string literal) → a Literal whose inner text is empty.
        // This is the value scan_call_with_string_arg rejects at line 395.
        assert_eq!(lit(literal_or_dynamic("\"\"")), Some(""));
    }

    #[test]
    fn literal_or_dynamic_identifier_is_dynamic() {
        // A bare identifier (no leading quote) is a computed/dynamic arg.
        assert!(matches!(literal_or_dynamic("var_path"), ArgKind::Dynamic));
    }

    #[test]
    fn literal_or_dynamic_unterminated_quote_is_dynamic() {
        // A leading quote with no closing quote → treated as dynamic (line 633).
        assert!(matches!(
            literal_or_dynamic("\"unterminated"),
            ArgKind::Dynamic
        ));
    }

    // ---- scan_call_with_string_arg: boundary + empty-literal branches ----

    #[test]
    fn scan_call_ident_suffix_not_matched_as_get_node() {
        // `my_get_node(...)` contains the substring `get_node` but the char
        // before it is an identifier char, so is_ident_boundary_before is false
        // and the occurrence is skipped (line 384 `continue`). No reference.
        let mut out = Vec::new();
        scan_call_with_string_arg("a.gd", 1, "\tmy_get_node(\"X\")", "file:a.gd", &mut out);
        assert!(
            out.is_empty(),
            "get_node as an identifier suffix must not match, got {out:?}"
        );
    }

    #[test]
    fn scan_call_empty_string_literal_emits_nothing() {
        // `get_node("")` — an empty string literal is a Literal("") which the
        // `ArgKind::Literal(_) => {}` arm (line 395) drops: nothing to reference.
        let mut out = Vec::new();
        scan_call_with_string_arg("a.gd", 1, "\tget_node(\"\")", "file:a.gd", &mut out);
        assert!(
            out.is_empty(),
            "empty-literal get_node arg references nothing, got {out:?}"
        );
    }

    #[test]
    fn scan_call_literal_arg_emits_reference() {
        // Control: a non-empty literal emits the reference (the Literal-name arm).
        let mut out = Vec::new();
        scan_call_with_string_arg("a.gd", 3, "\tget_node(\"Player\")", "file:a.gd", &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].reference_name, "Player");
        assert_eq!(out[0].reference_kind, EdgeKind::References);
    }

    #[test]
    fn scan_call_non_paren_after_name_is_skipped() {
        // `get_node` NOT followed by `(` (e.g. used as a value) is not a call.
        let mut out = Vec::new();
        scan_call_with_string_arg("a.gd", 1, "\tvar f = get_node", "file:a.gd", &mut out);
        assert!(
            out.is_empty(),
            "get_node without `(` is not a call, got {out:?}"
        );
    }

    // ---- scan_connect: literal / dynamic / empty branches ----

    #[test]
    fn scan_connect_bare_ident_emits_call_reference() {
        // The literal-handler arm (line 424/425): a bare ident handler resolves.
        let mut out = Vec::new();
        scan_connect(
            "a.gd",
            2,
            "\tsig.connect(_on_timeout)",
            "file:a.gd",
            &mut out,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].reference_name, "_on_timeout");
        assert_eq!(out[0].reference_kind, EdgeKind::Calls);
    }

    #[test]
    fn scan_connect_call_expr_handler_emits_dynamic_sentinel() {
        // A call-expression handler → the Dynamic arm emits the sentinel.
        let mut out = Vec::new();
        scan_connect("a.gd", 2, "\tsig.connect(get_cb())", "file:a.gd", &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].reference_name, dynamic_name("connect"));
        assert_eq!(out[0].reference_kind, EdgeKind::Calls);
    }

    // ---- connect_handler: every classification arm ----

    #[test]
    fn connect_handler_empty_arg_is_dynamic() {
        // A `.connect()` with an empty first arg is dynamic (line 657).
        assert!(matches!(connect_handler(""), ArgKind::Dynamic));
        assert!(matches!(connect_handler("   "), ArgKind::Dynamic));
    }

    #[test]
    fn connect_handler_bare_ident_is_literal() {
        assert_eq!(lit(connect_handler("_on_timeout")), Some("_on_timeout"));
    }

    #[test]
    fn connect_handler_self_member_takes_trailing_ident() {
        assert_eq!(lit(connect_handler("self._on_ready")), Some("_on_ready"));
    }

    #[test]
    fn connect_handler_bind_head_plain_ident_is_literal() {
        // `_on_x.bind(a)` — the head before `.bind(` reduces to a plain ident.
        assert_eq!(
            lit(connect_handler("_on_rect_input.bind(i)")),
            Some("_on_rect_input")
        );
    }

    #[test]
    fn connect_handler_bind_head_self_member_takes_trailing() {
        assert_eq!(
            lit(connect_handler("self._on_died.bind(c)")),
            Some("_on_died")
        );
    }

    #[test]
    fn connect_handler_bind_head_non_ident_is_dynamic() {
        // `get_cb().bind(1)` — the head `get_cb()` is a call, NOT a plain ident,
        // so the bind-head branch classifies it Dynamic (line 674/680 region).
        assert!(matches!(
            connect_handler("get_cb().bind(1)"),
            ArgKind::Dynamic
        ));
    }

    #[test]
    fn connect_handler_call_expr_without_bind_is_dynamic() {
        // A handler containing `(` that is not a `.bind(` chain nor a Callable(..)
        // → the `arg.contains('(')` guard (line 679/680) returns Dynamic.
        assert!(matches!(connect_handler("make_cb(1)"), ArgKind::Dynamic));
    }

    #[test]
    fn connect_handler_trailing_non_ident_is_dynamic() {
        // A member access whose trailing segment is NOT a plain identifier (a
        // numeric index / operator artifact) → Dynamic (line 686/687).
        assert!(matches!(connect_handler("obj.0"), ArgKind::Dynamic));
        assert!(matches!(connect_handler("a.+"), ArgKind::Dynamic));
    }

    // ---- callable_handler: Callable(recv, method) classification ----

    #[test]
    fn callable_handler_self_string_method_is_literal() {
        assert_eq!(
            lit(callable_handler("self, \"_on_pressed\"")),
            Some("_on_pressed")
        );
    }

    #[test]
    fn callable_handler_this_string_method_is_literal() {
        assert_eq!(
            lit(callable_handler("this, \"_on_pressed\"")),
            Some("_on_pressed")
        );
    }

    #[test]
    fn callable_handler_no_top_level_comma_is_dynamic() {
        // A malformed `Callable(...)` with NO top-level comma → Dynamic (line 699).
        assert!(matches!(callable_handler("self"), ArgKind::Dynamic));
        assert!(matches!(callable_handler("\"_on_x\""), ArgKind::Dynamic));
    }

    #[test]
    fn callable_handler_non_self_receiver_is_dynamic() {
        // A non-self/this receiver is a cross-object callable → Dynamic (line 702-703).
        assert!(matches!(
            callable_handler("other_obj, \"_on_pressed\""),
            ArgKind::Dynamic
        ));
    }

    #[test]
    fn callable_handler_empty_method_is_dynamic() {
        // `Callable(self, "")` — an empty method name → Dynamic (line 706-709).
        assert!(matches!(callable_handler("self, \"\""), ArgKind::Dynamic));
    }

    #[test]
    fn callable_handler_non_literal_method_is_dynamic() {
        // `Callable(self, method_var)` — a variable method name → Dynamic.
        assert!(matches!(
            callable_handler("self, method_var"),
            ArgKind::Dynamic
        ));
    }

    #[test]
    fn callable_handler_non_ident_string_method_is_dynamic() {
        // A string-literal method that is not a plain identifier (`"1bad"`) →
        // Dynamic (the `is_plain_ident(name)` guard at line 706 fails).
        assert!(matches!(
            callable_handler("self, \"1bad\""),
            ArgKind::Dynamic
        ));
    }

    // ---- split_bind_head: top-level detection through quotes/parens ----

    #[test]
    fn split_bind_head_plain_chain_returns_head() {
        assert_eq!(split_bind_head("cb.bind(1)"), Some("cb"));
        assert_eq!(split_bind_head("self._on_x.bind(a)"), Some("self._on_x"));
    }

    #[test]
    fn split_bind_head_no_bind_returns_none() {
        assert_eq!(split_bind_head("_on_timeout"), None);
    }

    #[test]
    fn split_bind_head_leading_bind_yields_empty_head() {
        // A leading `.bind(` yields Some("") — the caller then rejects the empty
        // head as a non-ident.
        assert_eq!(split_bind_head(".bind(x)"), Some(""));
    }

    #[test]
    fn split_bind_head_ignores_bind_inside_string() {
        // A `.bind(` that appears INSIDE a quoted string must be ignored (the
        // in-string state machine at lines 726-730/733). Here the only `.bind(`
        // is inside the string, so there is no top-level bind → None.
        assert_eq!(split_bind_head("\"x.bind(y)\""), None);
    }

    #[test]
    fn split_bind_head_ignores_bind_inside_nested_parens() {
        // A `.bind(` nested inside parens (depth > 0) is not top-level; the only
        // top-level one is the outer chain.
        assert_eq!(
            split_bind_head("wrap(a.bind(1)).bind(2)"),
            Some("wrap(a.bind(1))")
        );
    }

    #[test]
    fn split_bind_head_string_before_top_level_bind() {
        // A quoted string precedes the real top-level `.bind(`; the quote-skipping
        // branch (733 opens, 726-729 skip, closing quote) must not break head split.
        assert_eq!(split_bind_head("f(\"a,b\").bind(1)"), Some("f(\"a,b\")"));
    }

    // ---- top_level_comma: quote / nested-paren / no-comma branches ----

    #[test]
    fn top_level_comma_plain() {
        // The simple top-level comma (line 768).
        assert_eq!(top_level_comma("self, \"m\""), Some(4));
    }

    #[test]
    fn top_level_comma_none_when_absent() {
        // No top-level comma → None (line 773).
        assert_eq!(top_level_comma("self"), None);
    }

    #[test]
    fn top_level_comma_ignores_comma_inside_string() {
        // A comma inside a quoted string is skipped (in_str branch 757-762 +
        // opening quote 765); the real top-level comma is after the string.
        assert_eq!(top_level_comma("\"a,b\", m"), Some(5));
    }

    #[test]
    fn top_level_comma_ignores_comma_inside_nested_parens() {
        // A comma nested inside `(...)` (depth > 0, lines 766-767) is skipped.
        assert_eq!(top_level_comma("f(a, b), m"), Some(7));
        // And with NO top-level comma outside the parens → None.
        assert_eq!(top_level_comma("f(a, b)"), None);
    }

    #[test]
    fn top_level_comma_ignores_comma_inside_brackets_and_braces() {
        // Brackets/braces also raise depth (line 766).
        assert_eq!(top_level_comma("[a, b], m"), Some(6));
        assert_eq!(top_level_comma("{a: 1, b: 2}, m"), Some(12));
    }

    // ---- parse_func_header: non-ident name → None (line 822) ----

    #[test]
    fn parse_func_header_valid() {
        assert_eq!(parse_func_header("func apply():"), Some("apply"));
        assert_eq!(
            parse_func_header("static func make() -> Node:"),
            Some("make")
        );
    }

    #[test]
    fn parse_func_header_non_func_line_is_none() {
        assert_eq!(parse_func_header("var x = 1"), None);
    }

    #[test]
    fn parse_func_header_non_ident_name_is_none() {
        // `func 1bad(` — a name beginning with a digit is not a plain identifier
        // (line 821/822 returns None).
        assert_eq!(parse_func_header("func 1bad():"), None);
    }

    #[test]
    fn parse_func_header_empty_name_is_none() {
        // `func (` — nothing before the paren → empty name → None (line 821/822).
        assert_eq!(parse_func_header("func ():"), None);
    }

    // ---- ident_before: the three defensive `None` returns ----

    #[test]
    fn ident_before_reads_signal_name() {
        // Control: `health_changed.emit(` → the ident just left of the dot.
        let line = "\thealth_changed.emit()";
        let dot = line.find(".emit(").expect("dot");
        assert_eq!(ident_before(line, dot), Some("health_changed"));
    }

    #[test]
    fn ident_before_no_ident_returns_none() {
        // A `.emit(` with no identifier to its left (start == end) → None (line 838).
        let line = "\t.emit()";
        let dot = line.find(".emit(").expect("dot");
        assert_eq!(ident_before(line, dot), None);
    }

    #[test]
    fn ident_before_empty_window_returns_none() {
        // `get_x().emit()` — no ident chars immediately left of the dot (the
        // char is `)`), so the window is empty (start == end) → None.
        let line = "\tget_x().emit()";
        let dot = line.find(".emit(").expect("dot");
        assert_eq!(ident_before(line, dot), None);
    }

    #[test]
    fn ident_before_ident_after_close_paren_returns_none() {
        // `)x.emit()` — an ident (`x`) exists left of the dot, but the char
        // before it is `)`, an unnameable call result, so the boundary guard
        // rejects it → None.
        let line = ")x.emit()";
        let dot = line.find(".emit(").expect("dot");
        assert_eq!(ident_before(line, dot), None);
    }

    #[test]
    fn ident_before_leading_digit_returns_none() {
        // A candidate ident that starts with a digit is invalid → None (line 847/848).
        let line = "1x.emit()";
        let dot = line.find(".emit(").expect("dot");
        assert_eq!(ident_before(line, dot), None);
    }

    // ---- is_plain_ident: true + both false paths (line 878) ----

    #[test]
    fn is_plain_ident_true_cases() {
        assert!(is_plain_ident("_on_x"));
        assert!(is_plain_ident("Player2D"));
        assert!(is_plain_ident("a"));
    }

    #[test]
    fn is_plain_ident_false_cases() {
        // Empty, leading digit, and a non-ident interior char all fail. The
        // leading-digit / empty cases exercise the `_ => return false` arm (878).
        assert!(!is_plain_ident(""));
        assert!(!is_plain_ident("1bad"));
        assert!(!is_plain_ident("a-b"));
        assert!(!is_plain_ident("+"));
    }

    // ---- scan_dollar_node: the `$"..."` empty / unterminated-quote branches ----

    #[test]
    fn scan_dollar_node_quoted_path_emits_reference() {
        let mut out = Vec::new();
        scan_dollar_node(
            "a.gd",
            1,
            "\tvar n = $\"Player/Sprite\"",
            "file:a.gd",
            &mut out,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].reference_name, "Player/Sprite");
    }

    #[test]
    fn scan_dollar_node_empty_quoted_path_emits_nothing_but_advances() {
        // `$""` — an empty quoted path references nothing (the `!path.is_empty()`
        // guard drops it), and the scanner advances past the closing quote.
        let mut out = Vec::new();
        scan_dollar_node("a.gd", 1, "\tvar n = $\"\"", "file:a.gd", &mut out);
        assert!(
            out.is_empty(),
            "empty $\"\" references nothing, got {out:?}"
        );
    }

    #[test]
    fn scan_dollar_node_unterminated_quote_emits_nothing() {
        // `$"` with no closing quote — `quoted_strings` yields no complete string,
        // so nothing is emitted and the scan does not panic.
        let mut out = Vec::new();
        scan_dollar_node(
            "a.gd",
            1,
            "\tvar n = $\"unterminated",
            "file:a.gd",
            &mut out,
        );
        assert!(
            out.is_empty(),
            "unterminated $\" references nothing, got {out:?}"
        );
    }

    #[test]
    fn scan_dollar_node_trailing_dollar_is_ignored() {
        // A `$` at end-of-line with no path token following → nothing emitted.
        let mut out = Vec::new();
        scan_dollar_node("a.gd", 1, "\tvar n = $", "file:a.gd", &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn lit_helper_maps_dynamic_to_none() {
        // Exercise the test helper's Dynamic arm directly.
        assert_eq!(lit(ArgKind::Dynamic), None);
        assert_eq!(lit(ArgKind::Literal("x")), Some("x"));
    }

    // ---- is_ident_boundary_before ----

    #[test]
    fn is_ident_boundary_before_start_of_line() {
        assert!(is_ident_boundary_before("get_node()", 0));
    }

    #[test]
    fn is_ident_boundary_before_after_ident_char_is_false() {
        // In `my_get_node`, the `g` of `get_node` (index 3) is preceded by `_`.
        let line = "my_get_node";
        let pos = line.find("get_node").expect("get_node");
        assert!(!is_ident_boundary_before(line, pos));
    }

    #[test]
    fn is_ident_boundary_before_after_dot_is_true() {
        let line = "node.get_node";
        let pos = line.rfind("get_node").expect("get_node");
        assert!(is_ident_boundary_before(line, pos));
    }
}
