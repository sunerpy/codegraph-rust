//! Tauri [`FrameworkResolver`] — the cross-language IPC bridge (Tier 4 item 10,
//! upstream issue #1543).
//!
//! Binds a Rust command function to the TypeScript/JavaScript call sites that
//! reach it over Tauri's IPC boundary, an edge tree-sitter cannot produce because
//! the two halves live in different languages and are joined only by a string
//! literal on the wire.
//!
//! Neither half of that join survives in the store, so both are recovered by
//! reading source (measured: the call literal is nowhere in `unresolved_refs`,
//! whose rows carry `reference_name = "invoke"` and no argument text; and
//! `nodes.decorators` is EMPTY for an attributed Rust fn, so nothing
//! distinguishes a command from any other function in the graph). Re-reading
//! source inside a resolver has direct precedent —
//! [`crate::frameworks::godot::GodotResolver`] recovers its autoload bindings by
//! re-reading every `project.godot` in `resolve()`.
//!
//! # Both text scans are lexically masked, and neither mask is optional
//!
//! [`rust_code_mask`] gates the registration side and [`js_code_mask`] the call
//! side, for DIFFERENT reasons that are worth keeping straight because a future
//! edit could drop either.
//!
//! On the RUST side the mask is the only defence that exists. A commented-out or
//! raw-string copy of the attribute mints no node, while a real UNATTRIBUTED
//! `fn` of the same name does — so a text hit inside a comment has exactly one
//! same-named candidate to bind to, the wrong one, and an `invoke('fake')` then
//! reaches a function the developer never exposed over IPC. Measured over the six
//! raw attribute hits of the adversarial fixture below: an unmasked scan yields
//! six roster keys and six fabricated edges where the correct answer is one.
//!
//! On the JS side the base extractor already discriminates every non-call shape
//! (comments, strings, templates, both regex spellings, division, JSX text and
//! attributes) and keeps the receiver in `client.invoke`. The mask is therefore
//! redundant-but-required: its obligation is to AGREE with the extractor shape by
//! shape, because a resolver at this tier cannot read the extractor's refs
//! (`FrameworkExtractionContext` carries two fields, and `ResolutionContext` has
//! no ref-facing method at all). Unmasked, a legal regex literal spelling the
//! call verbatim — `/invoke('save_config')/` — becomes a fabricated edge to a
//! real command.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};

use crate::framework::FrameworkResolver;
use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// The Rust attribute that marks a function as an IPC command. Held as a `&str`
/// (not spelled inline) so this file's own occurrences of the literal stay inside
/// masked lexical states — see [`command_attr_code_hits`].
const COMMAND_ATTR: &str = "tauri::command";

/// Namespace prefix on the call-side reference name, mirroring
/// `godot_script::AUTOLOAD_CALL_PREFIX`.
///
/// A ref named plainly `get_mcp_port` would be indistinguishable from a genuine
/// reference to a same-named symbol, and the generic pass would then compete for
/// it: `apply_language_gate` filters cross-language candidates only for
/// `References` and `Imports`, so a bare-named TS→Rust `Calls` ref falls through
/// to the name matcher and is bound by its scoring rather than by our uniqueness
/// rule. No real symbol name contains this prefix.
pub const NAMESPACE_PREFIX: &str = "tauri:invoke:";

/// Config-file markers probed by [`TauriResolver::detect`], in order. Both the
/// `src-tauri/` layout `create-tauri-app` generates and the root layout, each in
/// its `.json` and `.json5` spelling.
const TAURI_CONF_MARKERS: &[&str] = &[
    "src-tauri/tauri.conf.json",
    "src-tauri/tauri.conf.json5",
    "tauri.conf.json",
    "tauri.conf.json5",
];

/// The frontend package whose presence in `package.json` marks a Tauri app.
const TAURI_API_PACKAGE: &str = "@tauri-apps/api";

/// The call-side identifier this resolver recognises.
const INVOKE: &str = "invoke";

/// Path prefixes holding OTHER projects' checked-in source. Excluded from the
/// `.rs` attribute probe before the file is even read: a vendored corpus must
/// never decide the host project's framework, and unlike the lexical mask this
/// also stops a genuine (unmasked) attribute inside a fixture — the case a future
/// Tauri golden corpus would create.
const FIXTURE_PREFIXES: &[&str] = &[
    "crates/codegraph-bench/fixtures/",
    "crates/codegraph-resolve/tests/fixtures/",
    "reference/",
];

/// Wire name → its unique command node, or `None` for a POISONED key (two
/// different commands claimed the same spelling, so no edge may be built).
type Roster = BTreeMap<String, Option<Node>>;

/// Tauri IPC resolver.
///
/// The command roster is built once per resolver instance — i.e. once per
/// `ReferenceResolver::initialize`, which is once per resolve pass — because a
/// per-reference rebuild would re-scan every `.rs` file in the project for every
/// `invoke` call site.
#[derive(Default)]
pub struct TauriResolver {
    roster: OnceLock<Roster>,
}

impl FrameworkResolver for TauriResolver {
    fn name(&self) -> &str {
        "tauri"
    }

    /// `None` = applies to ALL languages, for the same reason
    /// [`crate::frameworks::godot::GodotResolver`] does: `resolve()` must read
    /// `.rs` files and `extract()` must see JS/TS files, and
    /// `applies_to_language` gates `extract()` by this list. [`Self::detect`] is
    /// the real activation guard.
    fn languages(&self) -> Option<&[Language]> {
        None
    }

    /// Ordered cheapest-first, returning `true` on the first hit.
    ///
    /// Probes 1-2 use `file_exists` rather than a `get_all_files` basename scan
    /// because `tauri.conf.json` is NOT an indexed file: `get_all_files` is backed
    /// by the `files` table, while `file_exists` reaches the filesystem, so it is
    /// the only probe that can see the marker at all.
    ///
    /// Probe 4 is last because it is the expensive one — it reads every `.rs`
    /// file — and it exists because it is the only probe that survives a monorepo
    /// (`apps/desktop/src-tauri/tauri.conf.json`).
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        for marker in TAURI_CONF_MARKERS {
            if context.file_exists(marker) {
                return true;
            }
        }
        if let Some(pkg) = context.read_file("package.json") {
            if pkg.contains(TAURI_API_PACKAGE) {
                return true;
            }
        }
        context.get_all_files().iter().any(|f| {
            is_rust_source(f)
                && context
                    .read_file(f)
                    .is_some_and(|src| !command_attr_code_hits(&src).is_empty())
        })
    }

    /// Bind a `tauri:invoke:<wire-name>` reference to its command function.
    ///
    /// Confidence is 0.9 so it short-circuits Strategy 1 in `resolve_one_pure`
    /// before the name-matcher: this resolver has already applied its own
    /// uniqueness rule, and nothing downstream may add to or override it. That
    /// matters more here than for any other resolver — `Language::Rust` has no
    /// language family, so the cross-family gates are inert for a Rust target and
    /// `Calls` bypasses them regardless. The roster's exactly-one-match rule is
    /// the ONLY guard that exists.
    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let wire = reference.reference_name.strip_prefix(NAMESPACE_PREFIX)?;
        if reference.reference_kind != EdgeKind::Calls {
            return None;
        }
        let target = self.roster(context).get(wire)?.as_ref()?;
        Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: target.id.clone(),
            confidence: 0.9,
            resolved_by: ResolvedBy::Framework,
        })
    }

    /// Opt the namespaced call-side references through the name-exists
    /// pre-filter.
    ///
    /// REQUIRED, and a deliberate divergence from `GodotResolver` (which returns
    /// `false`). Measured: `has_any_possible_match` tests the whole name, then
    /// splits on the first `.` and on `::`. `tauri:invoke:get_mcp_port` has a
    /// single colon pair and no `.`, so every lookup misses and the ref is dropped
    /// by the pre-filter before Strategy 1 runs. Godot's prefixed refs survive
    /// only because their payload contains a `.` — an accident of the Godot name
    /// shape, not a general rule.
    ///
    /// Prefix-gated, so it claims nothing a real symbol could be named and no
    /// pre-existing reference name's pre-filter verdict changes.
    fn claims_reference(&self, name: &str) -> bool {
        name.starts_with(NAMESPACE_PREFIX)
    }

    /// Emit one namespaced reference per literal-argument `invoke` call site in a
    /// JS-family file.
    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &crate::framework::FrameworkExtractionContext,
    ) -> Option<crate::types::FrameworkResolverExtractionResult> {
        if !is_js_family(file_path) {
            return None;
        }
        Some(crate::types::FrameworkResolverExtractionResult {
            nodes: Vec::new(),
            references: invoke_call_refs(file_path, content),
        })
    }
}

impl TauriResolver {
    fn roster(&self, context: &dyn ResolutionContext) -> &Roster {
        self.roster.get_or_init(|| build_roster(context))
    }
}

// ---------------------------------------------------------------------------
// Registration side (D2): the command roster
// ---------------------------------------------------------------------------

/// Scan every `.rs` file for `#[…tauri::command…]`-attributed functions and key
/// them by BOTH their wire name and its camelCase spelling (D4).
///
/// `generate_handler![…]` is deliberately NOT parsed. The two failure directions
/// are not symmetric: missing the macro parse silently kills the feature for that
/// command, and the ways to miss it are numerous and realistic (a wrapped list, a
/// module-qualified entry, the bare `generate_handler!` spelling after a `use`, or
/// `collect_commands!` for tauri-specta users, where it never appears at all).
/// An over-broad roster costs at most one edge to a command that is attributed but
/// not registered — i.e. to the exact function the developer wrote and named.
///
/// `BTreeMap` (not `HashMap`) so iteration is ordered, and the file list is sorted
/// so the outcome cannot depend on scan order.
fn build_roster(context: &dyn ResolutionContext) -> Roster {
    let mut roster: Roster = BTreeMap::new();
    let mut files = context.get_all_files();
    files.sort();
    for file in files {
        if !file.ends_with(".rs") {
            continue;
        }
        let Some(src) = context.read_file(&file) else {
            continue;
        };
        for name in command_fn_names(&src) {
            admit_command(&mut roster, context, &file, &name);
        }
    }
    roster
}

/// Join one attributed name to a real stored node and insert its two keys.
///
/// The join is what turns a text hit into a genuine node id. Its three outcomes
/// are all deliberate: exactly one same-named `Function`/`Method` in that file is
/// the command; ZERO means the file is unindexed or unparsed, which contributes
/// nothing rather than a dangling target; and TWO OR MORE means the file itself
/// cannot say which function the attribute belongs to, so the key is poisoned
/// rather than guessed.
fn admit_command(roster: &mut Roster, context: &dyn ResolutionContext, file: &str, name: &str) {
    let mut candidates: Vec<Node> = context
        .get_nodes_in_file(file)
        .into_iter()
        .filter(|n| n.name == name && matches!(n.kind, NodeKind::Function | NodeKind::Method))
        .collect();
    let camel = snake_to_camel(name);
    match candidates.len() {
        1 => {
            let node = candidates.pop().expect("checked len");
            insert_key(roster, name.to_string(), &node);
            insert_key(roster, camel, &node);
        }
        0 => {}
        _ => {
            roster.insert(name.to_string(), None);
            roster.insert(camel, None);
        }
    }
}

/// Insert `key → node`, POISONING the key when it already names a DIFFERENT
/// target.
///
/// The "different target id" clause is what keeps a single-word command
/// (`fn refresh`, whose camel spelling equals its wire name) from poisoning
/// itself: both inserts carry the same node id, so the second is a no-op.
fn insert_key(roster: &mut Roster, key: String, node: &Node) {
    match roster.get(&key) {
        None => {
            roster.insert(key, Some(node.clone()));
        }
        Some(Some(existing)) if existing.id == node.id => {}
        Some(Some(_)) => {
            roster.insert(key, None);
        }
        Some(None) => {}
    }
}

/// `foo_bar` → `fooBar`, the `tauri-specta` wire rename. Leading underscores are
/// preserved so a `_private` command keeps a distinguishable key.
///
/// Safe here in a way it would not be elsewhere: the lookup key is never an
/// arbitrary identifier from source, only ever the string literal of an `invoke`
/// call site, which by construction names a command or nothing.
fn snake_to_camel(name: &str) -> String {
    let lead = name.len() - name.trim_start_matches('_').len();
    let mut out = String::with_capacity(name.len());
    out.push_str(&name[..lead]);
    let mut upper_next = false;
    for ch in name[lead..].chars() {
        if ch == '_' {
            upper_next = true;
            continue;
        }
        if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Byte offsets of every [`COMMAND_ATTR`] occurrence in `src` that is ordinary
/// Rust code AND ends at a token boundary.
///
/// The boundary is not something the mask can supply: `tauri::commandant` is
/// ordinary code, so every byte of its `tauri::command` prefix masks `true` and a
/// byte-complete gate alone accepts it — measured, `#[tauri::commandant]` is one
/// of the adversarial fixture's six raw hits and it promotes a real unattributed
/// `fn`.
fn command_attr_code_hits(src: &str) -> Vec<usize> {
    let mask = rust_code_mask(src);
    hits_with_mask(src, &mask)
}

fn hits_with_mask(src: &str, mask: &[bool]) -> Vec<usize> {
    let bytes = src.as_bytes();
    src.match_indices(COMMAND_ATTR)
        .filter(|(i, _)| {
            let end = i + COMMAND_ATTR.len();
            (*i..end).all(|k| mask[k]) && ends_attribute_token(bytes.get(end))
        })
        .map(|(i, _)| i)
        .collect()
}

/// The byte after `tauri::command` in a genuine attribute: `]` closes it, `(`
/// opens its arguments (`#[tauri::command(rename_all = "snake_case")]`), `,`
/// separates it inside a list, and whitespace precedes any of those.
fn ends_attribute_token(next: Option<&u8>) -> bool {
    match next {
        None => false,
        Some(b) => matches!(b, b']' | b'(' | b',') || b.is_ascii_whitespace(),
    }
}

/// Every command function name declared in `src`.
fn command_fn_names(src: &str) -> Vec<String> {
    let mask = rust_code_mask(src);
    let bytes = src.as_bytes();
    hits_with_mask(src, &mask)
        .into_iter()
        .filter_map(|hit| command_fn_name_after(bytes, &mask, hit + COMMAND_ATTR.len()))
        .collect()
}

/// From the end of an accepted attribute token, walk to the `fn <name>` it
/// applies to.
///
/// TWO tokens are gated, not one span: the attribute (already checked by the
/// caller) and the `fn <name>` that follows. Between them the walk SKIPS masked
/// bytes, further attribute tokens, `pub`/`pub(…)` and `async` — requiring the
/// whole span to be code would fail closed on legal Rust, because an
/// attribute / doc-comment / attribute / `fn` interleaving compiles. Skipping the
/// masked bytes is also what stops a commented-out `fn fake()` between the
/// attribute and the real declaration from keying the roster on `fake`.
fn command_fn_name_after(bytes: &[u8], mask: &[bool], attr_end: usize) -> Option<String> {
    let mut i = skip_trivia(bytes, mask, attr_end)?;
    if bytes[i] == b'(' {
        i = skip_balanced(bytes, mask, i, b'(', b')')?;
        i = skip_trivia(bytes, mask, i)?;
    }
    if bytes[i] != b']' {
        return None;
    }
    i += 1;
    loop {
        i = skip_trivia(bytes, mask, i)?;
        if bytes[i] == b'#' {
            if bytes.get(i + 1) != Some(&b'[') {
                return None;
            }
            i = skip_balanced(bytes, mask, i + 1, b'[', b']')?;
            continue;
        }
        let (word, after) = read_ident(bytes, i)?;
        match word {
            "pub" => {
                i = after;
                if let Some(j) = skip_trivia(bytes, mask, i) {
                    if bytes[j] == b'(' {
                        i = skip_balanced(bytes, mask, j, b'(', b')')?;
                    }
                }
            }
            "async" => i = after,
            "fn" => {
                if !(i..after).all(|k| mask[k]) {
                    return None;
                }
                let n = skip_trivia(bytes, mask, after)?;
                let (name, name_end) = read_ident(bytes, n)?;
                if !(n..name_end).all(|k| mask[k]) {
                    return None;
                }
                return Some(name.to_string());
            }
            _ => return None,
        }
    }
}

/// Next index at or after `i` holding a non-whitespace CODE byte, skipping the
/// masked runs (comments and literals) in between.
fn skip_trivia(bytes: &[u8], mask: &[bool], mut i: usize) -> Option<usize> {
    while i < bytes.len() && (bytes[i].is_ascii_whitespace() || !mask[i]) {
        i += 1;
    }
    (i < bytes.len()).then_some(i)
}

/// Index just past the `close` matching the `open` at `i`, counting only CODE
/// bytes so a bracket inside a comment or string cannot unbalance the span.
fn skip_balanced(bytes: &[u8], mask: &[bool], i: usize, open: u8, close: u8) -> Option<usize> {
    let mut depth = 0usize;
    let mut k = i;
    while k < bytes.len() {
        if mask[k] {
            if bytes[k] == open {
                depth += 1;
            } else if bytes[k] == close {
                depth -= 1;
                if depth == 0 {
                    return Some(k + 1);
                }
            }
        }
        k += 1;
    }
    None
}

fn read_ident(bytes: &[u8], i: usize) -> Option<(&str, usize)> {
    if i >= bytes.len() || !is_ident_start(bytes[i]) {
        return None;
    }
    let mut j = i;
    while j < bytes.len() && is_ident_byte(bytes[j]) {
        j += 1;
    }
    std::str::from_utf8(&bytes[i..j]).ok().map(|s| (s, j))
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

fn is_rust_source(path: &str) -> bool {
    path.ends_with(".rs") && !FIXTURE_PREFIXES.iter().any(|p| path.starts_with(p))
}

fn is_js_family(path: &str) -> bool {
    [".ts", ".tsx", ".js", ".jsx", ".mts", ".cts", ".mjs", ".cjs"]
        .iter()
        .any(|ext| path.ends_with(ext))
}

// ---------------------------------------------------------------------------
// The Rust lexical mask
// ---------------------------------------------------------------------------

/// Byte mask over `src`: `true` where the byte is ordinary Rust code.
///
/// One forward scan tracking the states a textual attribute match can hide in,
/// each of which was measured to fabricate an edge (or, for the last one, to lose
/// a real command) when unhandled:
///
/// * line comments, including `///` and `//!`;
/// * block comments, **nestable** — Rust nests, unlike C++, so a C++-shaped mask
///   closes at the first `*/` and reads the trailing bytes as code;
/// * raw strings `r"…"` / `r#"…"#` and their byte forms `br##"…"##`, with the
///   HASH COUNT captured so a custom delimiter still closes correctly;
/// * normal and byte strings, `"…"` / `b"…"`, with `\` escapes;
/// * char literals with `\` escapes (`'\''`) that DECLINE to open on a lifetime
///   `'a` — a mask that treats every `'` as a delimiter masks from the lifetime
///   forward and can swallow a genuine attribute, which is a silent feature death
///   rather than a fabrication.
fn rust_code_mask(src: &str) -> Vec<bool> {
    let bytes = src.as_bytes();
    let mut mask = vec![true; bytes.len()];
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                mask[i] = false;
                i += 1;
            }
            continue;
        }
        if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i = mask_nested_block_comment(bytes, &mut mask, i);
            continue;
        }
        if is_ident_start(b) {
            if let Some((quote, hashes)) = raw_string_prefix(bytes, i) {
                mask[i..quote].fill(false);
                i = mask_raw_string(bytes, &mut mask, quote, hashes);
                continue;
            }
            let (_, after) = read_ident(bytes, i).expect("ident start");
            i = after;
            continue;
        }
        if b == b'"' {
            i = mask_quoted(bytes, &mut mask, i, b'"', false);
            continue;
        }
        if b == b'\'' {
            if char_literal_end(bytes, i).is_some() {
                i = mask_quoted(bytes, &mut mask, i, b'\'', true);
            } else {
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    mask
}

/// Mask a nestable block comment starting at `i` (`/*`). Returns the index just
/// past the outermost `*/`.
fn mask_nested_block_comment(bytes: &[u8], mask: &mut [bool], i: usize) -> usize {
    let mut depth = 0usize;
    let mut k = i;
    while k < bytes.len() {
        if bytes[k] == b'/' && bytes.get(k + 1) == Some(&b'*') {
            depth += 1;
            mask[k] = false;
            mask[k + 1] = false;
            k += 2;
            continue;
        }
        if bytes[k] == b'*' && bytes.get(k + 1) == Some(&b'/') {
            mask[k] = false;
            mask[k + 1] = false;
            k += 2;
            depth -= 1;
            if depth == 0 {
                return k;
            }
            continue;
        }
        mask[k] = false;
        k += 1;
    }
    bytes.len()
}

/// If a raw-string prefix (`r"`, `r##"`, `br"`, `br##"`) starts at `i`, return
/// the index of its opening quote and its hash count.
fn raw_string_prefix(bytes: &[u8], i: usize) -> Option<(usize, usize)> {
    let mut j = i;
    if bytes.get(j) == Some(&b'b') {
        j += 1;
    }
    if bytes.get(j) != Some(&b'r') {
        return None;
    }
    j += 1;
    let hash_start = j;
    while bytes.get(j) == Some(&b'#') {
        j += 1;
    }
    if bytes.get(j) != Some(&b'"') {
        return None;
    }
    Some((j, j - hash_start))
}

/// Mask a raw string whose opening quote is at `quote` and which closes on a `"`
/// followed by exactly `hashes` `#`.
fn mask_raw_string(bytes: &[u8], mask: &mut [bool], quote: usize, hashes: usize) -> usize {
    mask[quote] = false;
    let mut k = quote + 1;
    while k < bytes.len() {
        if bytes[k] == b'"' && closes_raw_string(bytes, k, hashes) {
            let end = (k + hashes).min(bytes.len() - 1);
            mask[k..=end].fill(false);
            return (k + hashes + 1).min(bytes.len());
        }
        mask[k] = false;
        k += 1;
    }
    bytes.len()
}

fn closes_raw_string(bytes: &[u8], quote: usize, hashes: usize) -> bool {
    (1..=hashes).all(|n| bytes.get(quote + n) == Some(&b'#'))
}

/// Mask a `delim`-quoted literal starting at `start`, honouring `\` escapes.
///
/// `reset_at_newline` is `true` only for char literals: a Rust string may legally
/// span lines, while an unclosed `'` means the disambiguation misjudged a
/// lifetime, and failing open there is safer than masking the rest of the file.
fn mask_quoted(
    bytes: &[u8],
    mask: &mut [bool],
    start: usize,
    delim: u8,
    reset_at_newline: bool,
) -> usize {
    mask[start] = false;
    let mut i = start + 1;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' && reset_at_newline {
            return i;
        }
        mask[i] = false;
        if b == b'\\' {
            if i + 1 < bytes.len() {
                mask[i + 1] = false;
            }
            i += 2;
            continue;
        }
        i += 1;
        if b == delim {
            return i;
        }
    }
    bytes.len()
}

/// Index just past the closing `'` when `i` opens a genuine char literal, or
/// `None` when it opens a LIFETIME or label.
///
/// `'\''` takes the escape branch; `'a'` is a literal because the byte after the
/// single character is the closing quote; `'a` in `&'a str` and `'static` are
/// lifetimes, so they open no state at all.
fn char_literal_end(bytes: &[u8], i: usize) -> Option<usize> {
    if bytes.get(i + 1) == Some(&b'\\') {
        let mut k = i + 2;
        while k < bytes.len() && bytes[k] != b'\'' && bytes[k] != b'\n' {
            k += 1;
        }
        return (bytes.get(k) == Some(&b'\'')).then_some(k + 1);
    }
    let rest = std::str::from_utf8(&bytes[i + 1..]).ok()?;
    let ch = rest.chars().next()?;
    let after = i + 1 + ch.len_utf8();
    (bytes.get(after) == Some(&b'\'')).then_some(after + 1)
}

// ---------------------------------------------------------------------------
// Call side (D3): the `invoke('name')` call sites
// ---------------------------------------------------------------------------

/// One `invoke(…)` call site the mask RECOGNISED, whether or not a wire name
/// could be recovered from it.
///
/// The two levels are kept apart because "no reference emitted" conflates two
/// different verdicts: the mask saying "not a call site at all" (a comment, a
/// regex, JSX text) versus gate 4 saying "a call site with no literal to
/// recover" (`invoke(c)`, `` invoke(`cmd_${id}`) ``). Only the first is a
/// mask-agreement question against the extractor, so only the first may be
/// compared against its `invoke | calls` rows.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CallSite {
    line: i64,
    column: i64,
    wire_name: Option<String>,
}

/// Every recognised `invoke(…)` call site in `content`, in source order.
fn invoke_call_sites(file_path: &str, content: &str) -> Vec<CallSite> {
    let lex = js_code_mask(content, is_jsx_capable(file_path));
    let bytes = content.as_bytes();
    let line_starts = line_starts(bytes);
    let mut out = Vec::new();
    for (start, _) in content.match_indices(INVOKE) {
        let end = start + INVOKE.len();
        if start > 0 && is_js_ident_byte(bytes[start - 1]) {
            continue;
        }
        if bytes.get(end).is_some_and(|b| is_js_ident_byte(*b)) {
            continue;
        }
        // Gate 1 — every byte of the identifier is ordinary code. Kills the line
        // and block comments, the string, the template, BOTH regex spellings, and
        // the two JSX shapes.
        if !(start..end).all(|k| lex.mask[k]) {
            continue;
        }
        // Gate 2 — no receiver. `client.invoke` is a call to SOME object's method
        // and this resolver has no way to know the object is a Tauri façade.
        if let Some(p) = prev_code_byte(bytes, &lex.mask, start) {
            if bytes[p] == b'.' {
                continue;
            }
        }
        // Gate 3 — after an optional generic argument list, a call must follow.
        // An import specifier never is, which is what keeps `import { invoke }`
        // out.
        let Some(mut j) = next_significant(bytes, &lex, end) else {
            continue;
        };
        if bytes[j] == b'<' {
            let Some(after) = skip_balanced(bytes, &lex.mask, j, b'<', b'>') else {
                continue;
            };
            let Some(k) = next_significant(bytes, &lex, after) else {
                continue;
            };
            j = k;
        }
        if bytes[j] != b'(' {
            continue;
        }
        // Gate 4 — the first argument must be a literal token the lexer CLOSED,
        // so `invoke("a" + b)` and `invoke('a', {x: 'b'})` cannot smuggle a second
        // literal into the wire name.
        let wire_name = next_significant(bytes, &lex, j + 1)
            .and_then(|arg| lex.literals.get(&arg))
            .map(|span| &content[span.0..span.1])
            .filter(|text| !text.contains("${") && is_wire_name(text))
            .map(str::to_string);
        let (line, column) = line_col(&line_starts, start);
        out.push(CallSite {
            line,
            column,
            wire_name,
        });
    }
    out
}

/// The namespaced references for the call sites that yielded a wire name.
///
/// `from_node_id` is the FILE node (D6). Reconstructing the enclosing function's
/// id — the shape `GodotResolver` uses — loses here because GDScript has one
/// function form and TypeScript has many: measured, an object-literal method and
/// an object-literal arrow property both produce NO extractor reference at all and
/// their enclosing node is a `constant`, so a reconstructing scanner would have to
/// special-case a call site the extractor never modelled. And the failure mode is
/// silent — `insert_unresolved_refs` drops a ref whose source node does not exist.
fn invoke_call_refs(file_path: &str, content: &str) -> Vec<RefView> {
    let from_node_id = format!("file:{file_path}");
    let language = super::js_language_for(file_path);
    invoke_call_sites(file_path, content)
        .into_iter()
        .filter_map(|site| {
            let wire = site.wire_name?;
            Some(RefView {
                row_id: None,
                from_node_id: from_node_id.clone(),
                reference_name: format!("{NAMESPACE_PREFIX}{wire}"),
                reference_kind: EdgeKind::Calls,
                line: site.line,
                column: site.column,
                file_path: file_path.to_string(),
                language,
                is_function_ref: false,
                reference_subkind: None,
            })
        })
        .collect()
}

/// A Tauri wire name: an identifier head then identifier bytes plus the
/// separators a command name may carry.
fn is_wire_name(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(head) = chars.next() else {
        return false;
    };
    if !(head.is_ascii_alphabetic() || head == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'))
}

fn line_starts(bytes: &[u8]) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// 1-based line and 0-based column of `offset`, matching the extractor's stored
/// convention (measured: `line` is the reference expression's line, `col` the
/// 0-based offset of its FIRST byte — which is why `client.invoke` is recorded at
/// the `c` of `client`, not at `invoke`).
fn line_col(line_starts: &[usize], offset: usize) -> (i64, i64) {
    let idx = match line_starts.binary_search(&offset) {
        Ok(i) => i,
        Err(i) => i - 1,
    };
    ((idx + 1) as i64, (offset - line_starts[idx]) as i64)
}

/// Nearest index before `i` holding a non-whitespace CODE byte.
fn prev_code_byte(bytes: &[u8], mask: &[bool], i: usize) -> Option<usize> {
    let mut k = i;
    while k > 0 {
        k -= 1;
        if !bytes[k].is_ascii_whitespace() && mask[k] {
            return Some(k);
        }
    }
    None
}

/// Next index at or after `i` that is either a recorded literal start or a
/// non-whitespace CODE byte.
///
/// Skipping masked bytes is a requirement, not a convenience: without it
/// `invoke /* x */ ('save_config')` is rejected — a MISSED edge where the
/// extractor records a call, which is an S2 violation. Stopping AT a literal
/// start is what lets gate 4 see the argument at all, since a literal's own bytes
/// are masked.
fn next_significant(bytes: &[u8], lex: &JsLex, mut i: usize) -> Option<usize> {
    while i < bytes.len() {
        if lex.literals.contains_key(&i) {
            return Some(i);
        }
        if !bytes[i].is_ascii_whitespace() && lex.mask[i] {
            return Some(i);
        }
        i += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// The JS/TS lexical mask
// ---------------------------------------------------------------------------

/// A JS/TS byte mask plus the span of every complete string-literal token.
struct JsLex {
    mask: Vec<bool>,
    /// Token start offset → (content start, content end) of a CLOSED literal.
    literals: BTreeMap<usize, (usize, usize)>,
}

/// Lex `src` into a code mask and its closed literal tokens.
///
/// FIVE states, each required by a shape the base extractor was measured to
/// record nothing for — and this mask's obligation is to agree with it:
///
/// 1. `//` to end of line, and `/* … */` **not** nestable (unlike Rust);
/// 2. `'…'` / `"…"` with `\` escapes, RESET at a newline so an unterminated quote
///    cannot mask the rest of the file;
/// 3. `` `…` `` spanning lines, with `${…}` interpolations scanned as CODE;
/// 4. a REGEX-literal state — `/invoke('save_config')/` is a legal regex whose
///    bytes spell the call verbatim, and without this state it becomes a
///    fabricated edge to a real command;
/// 5. JSX text runs, which are neither a string nor a comment to a naive mask.
fn js_code_mask(src: &str, jsx: bool) -> JsLex {
    let bytes = src.as_bytes();
    let mut lex = JsLex {
        mask: vec![true; bytes.len()],
        literals: BTreeMap::new(),
    };
    scan_js(bytes, &mut lex, 0, false, jsx);
    lex
}

/// Scan from `from` in code state. With `stop_at_brace`, return at the `}` that
/// closes the enclosing template interpolation or JSX expression container.
fn scan_js(bytes: &[u8], lex: &mut JsLex, from: usize, stop_at_brace: bool, jsx: bool) -> usize {
    let mut i = from;
    let mut brace_depth = 0usize;
    let mut jsx_depth = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                lex.mask[i] = false;
                i += 1;
            }
            continue;
        }
        if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i = mask_js_block_comment(bytes, lex, i);
            continue;
        }
        if b == b'/' {
            if regex_opens(bytes, &lex.mask, i) {
                i = mask_js_regex(bytes, lex, i);
            } else {
                i += 1;
            }
            continue;
        }
        if b == b'"' || b == b'\'' {
            i = mask_js_quoted(bytes, lex, i, b);
            continue;
        }
        if b == b'`' {
            i = mask_js_template(bytes, lex, i, jsx);
            continue;
        }
        if jsx && b == b'<' && jsx_tag_opens(bytes, &lex.mask, i) {
            i = scan_jsx_element(bytes, lex, i, &mut jsx_depth);
            continue;
        }
        if b == b'{' {
            brace_depth += 1;
            i += 1;
            continue;
        }
        if b == b'}' {
            if stop_at_brace && brace_depth == 0 {
                return i;
            }
            brace_depth = brace_depth.saturating_sub(1);
            i += 1;
            continue;
        }
        i += 1;
    }
    bytes.len()
}

fn mask_js_block_comment(bytes: &[u8], lex: &mut JsLex, i: usize) -> usize {
    let mut k = i;
    lex.mask[k] = false;
    lex.mask[k + 1] = false;
    k += 2;
    while k < bytes.len() {
        let closing = bytes[k] == b'*' && bytes.get(k + 1) == Some(&b'/');
        lex.mask[k] = false;
        k += 1;
        if closing {
            lex.mask[k] = false;
            return k + 1;
        }
    }
    bytes.len()
}

/// Mask a `'`/`"` literal, recording it as a closed token only when it really
/// closes on its own line.
fn mask_js_quoted(bytes: &[u8], lex: &mut JsLex, start: usize, delim: u8) -> usize {
    lex.mask[start] = false;
    let mut i = start + 1;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' {
            return i;
        }
        lex.mask[i] = false;
        if b == b'\\' {
            if i + 1 < bytes.len() {
                lex.mask[i + 1] = false;
            }
            i += 2;
            continue;
        }
        i += 1;
        if b == delim {
            lex.literals.insert(start, (start + 1, i - 1));
            return i;
        }
    }
    bytes.len()
}

/// Mask a template literal, scanning each `${…}` interpolation as CODE.
///
/// The recorded content span covers the whole raw text between the backticks, so
/// gate 4's `${` check sees an interpolation and declines the call site rather
/// than inventing a name from a fragment.
fn mask_js_template(bytes: &[u8], lex: &mut JsLex, start: usize, jsx: bool) -> usize {
    lex.mask[start] = false;
    let mut i = start + 1;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' {
            lex.mask[i] = false;
            if i + 1 < bytes.len() {
                lex.mask[i + 1] = false;
            }
            i += 2;
            continue;
        }
        if b == b'$' && bytes.get(i + 1) == Some(&b'{') {
            lex.mask[i] = false;
            lex.mask[i + 1] = false;
            let close = scan_js(bytes, lex, i + 2, true, jsx);
            if close >= bytes.len() {
                return bytes.len();
            }
            lex.mask[close] = false;
            i = close + 1;
            continue;
        }
        lex.mask[i] = false;
        i += 1;
        if b == b'`' {
            lex.literals.insert(start, (start + 1, i - 1));
            return i;
        }
    }
    bytes.len()
}

/// Does the `/` at `i` open a regex literal rather than a division?
///
/// Division after `)`, `]`, `}`, an identifier or number byte, and after a `++` /
/// `--`; a regex otherwise. The KEYWORD exception matters: `return /re/` is a
/// regex even though `return` ends in an identifier byte, and treating it as
/// division would leave the regex body unmasked — the fabrication this state
/// exists to prevent. Where the shape stays undecidable the `/` is treated as
/// opening a regex, which masks real code and loses a call site (visible as an
/// S2 failure) instead of inventing one (invisible).
fn regex_opens(bytes: &[u8], mask: &[bool], i: usize) -> bool {
    let Some(p) = prev_code_byte(bytes, mask, i) else {
        return true;
    };
    let b = bytes[p];
    if is_js_ident_byte(b) {
        let mut s = p;
        while s > 0 && is_js_ident_byte(bytes[s - 1]) {
            s -= 1;
        }
        return matches!(
            std::str::from_utf8(&bytes[s..=p]).unwrap_or_default(),
            "return"
                | "typeof"
                | "case"
                | "in"
                | "of"
                | "do"
                | "else"
                | "yield"
                | "await"
                | "delete"
                | "void"
                | "instanceof"
                | "new"
                | "throw"
        );
    }
    if matches!(b, b')' | b']' | b'}') {
        return false;
    }
    if matches!(b, b'+' | b'-') && p > 0 && bytes[p - 1] == b {
        return false;
    }
    true
}

/// Mask a regex literal, honouring `\` escapes and character classes (a `/`
/// inside `[…]` does not close it). A regex cannot span lines, so a newline ends
/// the state and leaves the remaining bytes as code.
fn mask_js_regex(bytes: &[u8], lex: &mut JsLex, start: usize) -> usize {
    lex.mask[start] = false;
    let mut i = start + 1;
    let mut in_class = false;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' {
            return i;
        }
        lex.mask[i] = false;
        if b == b'\\' {
            if i + 1 < bytes.len() {
                lex.mask[i + 1] = false;
            }
            i += 2;
            continue;
        }
        i += 1;
        match b {
            b'[' => in_class = true,
            b']' => in_class = false,
            b'/' if !in_class => return i,
            _ => {}
        }
    }
    bytes.len()
}

/// Does the `<` at `i` open a JSX element rather than a comparison or a generic
/// argument list?
///
/// The distinction is load-bearing in both directions: reading `Promise<number>`
/// as JSX would mask the real code that follows it (a lost edge), while reading
/// `<p>` as a comparison leaves JSX text unmasked (a fabricated edge, which the
/// measured fixture contains). An identifier, `)` or `]` before the `<` means a
/// comparison or a type argument list — except after the keywords that can only
/// be followed by an expression.
fn jsx_tag_opens(bytes: &[u8], mask: &[bool], i: usize) -> bool {
    let after = match bytes.get(i + 1) {
        Some(b) => *b,
        None => return false,
    };
    if !(after.is_ascii_alphabetic() || after == b'_' || after == b'/' || after == b'>') {
        return false;
    }
    let Some(p) = prev_code_byte(bytes, mask, i) else {
        return true;
    };
    let b = bytes[p];
    if is_js_ident_byte(b) {
        let mut s = p;
        while s > 0 && is_js_ident_byte(bytes[s - 1]) {
            s -= 1;
        }
        return matches!(
            std::str::from_utf8(&bytes[s..=p]).unwrap_or_default(),
            "return" | "case" | "default" | "yield" | "await" | "do" | "else" | "in" | "of"
        );
    }
    matches!(
        b,
        b'(' | b'{' | b'}' | b'[' | b',' | b';' | b'=' | b':' | b'?' | b'&' | b'|' | b'!' | b'>'
    )
}

/// Scan one JSX tag from its `<`, then mask the text run that follows it when the
/// scan is still inside an element.
///
/// `jsx_depth` is what makes nesting correct: text after a nested `</b>` is still
/// its parent's text and must stay masked, while text after the OUTERMOST closing
/// tag is ordinary code and must not be — the fixture's `.tsx` has a real
/// `invoke` call on the line after a closed element, so masking past it would be
/// an S2 failure.
fn scan_jsx_element(bytes: &[u8], lex: &mut JsLex, start: usize, jsx_depth: &mut usize) -> usize {
    let closing = bytes.get(start + 1) == Some(&b'/');
    let mut i = start + 1;
    let mut self_closing = false;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' || b == b'\'' {
            i = mask_js_quoted(bytes, lex, i, b);
            continue;
        }
        if b == b'{' {
            let close = scan_js(bytes, lex, i + 1, true, true);
            if close >= bytes.len() {
                return bytes.len();
            }
            i = close + 1;
            continue;
        }
        if b == b'/' && bytes.get(i + 1) == Some(&b'>') {
            self_closing = true;
            i += 2;
            break;
        }
        if b == b'>' {
            i += 1;
            break;
        }
        i += 1;
    }
    if closing {
        *jsx_depth = jsx_depth.saturating_sub(1);
    } else if !self_closing {
        *jsx_depth += 1;
    }
    if *jsx_depth == 0 {
        return i;
    }
    while i < bytes.len() && bytes[i] != b'<' {
        lex.mask[i] = false;
        i += 1;
    }
    i
}

fn is_js_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

fn is_jsx_capable(path: &str) -> bool {
    path.ends_with(".tsx") || path.ends_with(".jsx")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ImportMapping;

    /// This file's own bytes, and the integration test's, read at compile time so
    /// the guard below cannot drift away from the files it guards.
    const SELF_SRC: &str = include_str!("tauri.rs");
    const IPC_TEST_SRC: &str = include_str!("../../tests/tauri_ipc.rs");

    /// The adversarial registration-side fixture — measured legal Rust
    /// (`rustc --edition 2021 --emit=metadata`, exit 0) carrying SIX raw
    /// `tauri::command` hits of which exactly ONE is a genuine attribute on a
    /// genuine command. `tests/tauri_ipc.rs` holds the same bytes for the
    /// end-to-end edge count, and both sides assert the raw hit count so the two
    /// copies cannot drift apart unnoticed.
    const ADVERSARIAL: &str = r####"/* outer /* inner */ #[tauri::command]
fn nested_fake() -> u16 { 0 } */
const D1: &str = r##"
#[tauri::command]
fn hash_fake() -> u16 { 1 }
"##;
const D2: &[u8] = br##"
#[tauri::command]
fn byteraw_fake() -> u16 { 2 }
"##;
const D3: &[u8] = b"#[tauri::command] fn bytestr_fake() -> u16 { 3 }";
fn life<'a>(s: &'a str) -> &'a str { s }
const Q: char = '\'';
#[tauri::commandant]
fn attr_boundary_fake() -> u16 { 4 }
fn nested_fake() -> u16 { 40 }
fn hash_fake() -> u16 { 41 }
fn byteraw_fake() -> u16 { 42 }
fn bytestr_fake() -> u16 { 43 }
#[tauri::command]
fn real_cmd() -> u16 { 8111 }
"####;

    #[derive(Default)]
    struct TestContext {
        files: BTreeMap<String, String>,
        nodes: Vec<Node>,
    }

    impl TestContext {
        fn with_file(mut self, path: &str, content: &str) -> Self {
            self.files.insert(path.to_string(), content.to_string());
            self
        }

        fn with_fn(mut self, file: &str, name: &str, line: i64) -> Self {
            self.nodes.push(node(file, name, line));
            self
        }
    }

    fn node(file: &str, name: &str, line: i64) -> Node {
        Node {
            id: format!("function:{file}:{name}:{line}"),
            kind: NodeKind::Function,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: file.to_string(),
            language: Language::Rust,
            start_line: line,
            end_line: line,
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

    impl ResolutionContext for TestContext {
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
        fn get_nodes_by_qualified_name(&self, _qualified_name: &str) -> Vec<Node> {
            Vec::new()
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
        fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.name.to_lowercase() == lower_name)
                .cloned()
                .collect()
        }
        fn get_node_by_id(&self, id: &str) -> Option<Node> {
            self.nodes.iter().find(|n| n.id == id).cloned()
        }
        fn get_import_mappings(&self, _file_path: &str, _language: Language) -> Vec<ImportMapping> {
            Vec::new()
        }
    }

    /// The attribute spelled at run time, so these tests add no code-position
    /// occurrence of the literal they are testing.
    fn attr() -> String {
        format!("#[{COMMAND_ATTR}]")
    }

    fn roster_keys(ctx: &TestContext) -> Vec<String> {
        build_roster(ctx).into_keys().collect()
    }

    fn live_keys(ctx: &TestContext) -> Vec<String> {
        build_roster(ctx)
            .into_iter()
            .filter_map(|(k, v)| v.map(|_| k))
            .collect()
    }

    // -----------------------------------------------------------------------
    // T0b / T3 — probe 4 must not make this repository self-detect
    // -----------------------------------------------------------------------

    /// Both files carry the raw literal (in a doc comment, a `const`, and test
    /// fixtures), so a bare `contains` scan over `.rs` files turns codegraph-rust
    /// into a self-detecting Tauri project — which would destroy the premise that
    /// `detect()` is false for all sixteen golden corpora.
    #[test]
    fn tauri_probe4_masked_scan_finds_no_code_hit_in_own_sources() {
        assert!(
            SELF_SRC.matches(COMMAND_ATTR).count() >= 1,
            "the guard is pointless unless the literal really occurs here"
        );
        assert!(
            IPC_TEST_SRC.matches(COMMAND_ATTR).count() >= 1,
            "the guard is pointless unless the literal really occurs in the test"
        );

        assert_eq!(
            command_attr_code_hits(SELF_SRC),
            Vec::<usize>::new(),
            "frameworks/tauri.rs must have NO code-position occurrence"
        );
        assert_eq!(
            command_attr_code_hits(IPC_TEST_SRC),
            Vec::<usize>::new(),
            "tests/tauri_ipc.rs must have NO code-position occurrence"
        );
    }

    /// The counter-test: a mask that failed closed on genuine Tauri source would
    /// be a worse bug than the one it fixes.
    #[test]
    fn tauri_probe4_masked_scan_still_sees_genuine_attributes() {
        let src = format!("{}\nfn get_mcp_port() -> u16 {{ 8111 }}\n", attr());
        assert_eq!(command_attr_code_hits(&src).len(), 1);
    }

    /// A vendored corpus must never decide the host project's framework, and the
    /// mask cannot help here: the planted attribute is genuine, unmasked code.
    #[test]
    fn tauri_probe4_excludes_fixture_corpora() {
        let planted = format!("{}\nfn vendored() -> u16 {{ 0 }}\n", attr());
        for prefix in FIXTURE_PREFIXES {
            let ctx =
                TestContext::default().with_file(&format!("{prefix}vendor/main.rs"), &planted);
            assert!(
                !TauriResolver::default().detect(&ctx),
                "a `{prefix}` file must not flip detect()"
            );
        }
    }

    // -----------------------------------------------------------------------
    // T3 — detection: one positive per probe, plus two negatives
    // -----------------------------------------------------------------------

    #[test]
    fn tauri_detect_probe1_src_tauri_conf() {
        for marker in ["src-tauri/tauri.conf.json", "src-tauri/tauri.conf.json5"] {
            let ctx = TestContext::default().with_file(marker, "{}");
            assert!(TauriResolver::default().detect(&ctx), "{marker}");
        }
    }

    #[test]
    fn tauri_detect_probe2_root_conf() {
        for marker in ["tauri.conf.json", "tauri.conf.json5"] {
            let ctx = TestContext::default().with_file(marker, "{}");
            assert!(TauriResolver::default().detect(&ctx), "{marker}");
        }
    }

    #[test]
    fn tauri_detect_probe3_package_json_api_dependency() {
        let ctx = TestContext::default().with_file(
            "package.json",
            r#"{"dependencies":{"@tauri-apps/api":"^2.0.0"}}"#,
        );
        assert!(TauriResolver::default().detect(&ctx));
    }

    #[test]
    fn tauri_detect_probe4_rust_attribute() {
        let ctx = TestContext::default().with_file(
            "src-tauri/src/main.rs",
            &format!("{}\nfn get_mcp_port() -> u16 {{ 8111 }}\n", attr()),
        );
        assert!(TauriResolver::default().detect(&ctx));
    }

    /// The token boundary's own guard, and the place it is genuinely load-bearing:
    /// probe 4 has no following-token structure to lean on, so without the boundary
    /// check a file whose only occurrence is `tauri::commandant` — entirely ordinary
    /// code the mask cannot reject — flips `detect()` for the whole project.
    #[test]
    fn tauri_detect_negative_token_boundary_collision() {
        let ctx = TestContext::default().with_file(
            "src/lib.rs",
            &format!(
                "mod tauri_mod {{ pub fn commandant() -> u16 {{ 7 }} }}\n\
                 use tauri_mod as {COMMAND_ATTR}ant_alias;\n"
            ),
        );
        assert_eq!(
            ctx.read_file("src/lib.rs")
                .expect("fixture")
                .matches(COMMAND_ATTR)
                .count(),
            1,
            "the guard is pointless unless the raw literal occurs"
        );
        assert!(!TauriResolver::default().detect(&ctx));
    }

    #[test]
    fn tauri_detect_negative_empty_project() {
        assert!(!TauriResolver::default().detect(&TestContext::default()));
    }

    /// Mirrors THIS repository: plain Rust, an `invoke`-named function, no marker.
    #[test]
    fn tauri_detect_negative_plain_rust_with_invoke_fn() {
        let ctx = TestContext::default()
            .with_file("src/lib.rs", "fn invoke(name: &str) -> u16 { 0 }\n")
            .with_file("package.json", r#"{"dependencies":{"react":"18"}}"#);
        assert!(!TauriResolver::default().detect(&ctx));
    }

    // -----------------------------------------------------------------------
    // T4 — the roster accept-shape (D2)
    // -----------------------------------------------------------------------

    #[test]
    fn tauri_roster_accepts_all_four_attribute_arrangements() {
        let src = format!(
            "{a}\nfn a_cmd() -> u16 {{ 0 }}\n\
             {a}\npub async fn b_cmd(x: String) -> bool {{ x.is_empty() }}\n\
             #[specta::specta]\n{a}\nfn c_cmd() -> u16 {{ 0 }}\n\
             {a}\n#[specta::specta]\nfn d_cmd() -> u16 {{ 0 }}\n",
            a = attr()
        );
        assert_eq!(
            command_fn_names(&src),
            vec!["a_cmd", "b_cmd", "c_cmd", "d_cmd"]
        );
    }

    #[test]
    fn tauri_roster_accepts_attribute_arguments() {
        let src = format!(
            "#[{COMMAND_ATTR}(rename_all = \"snake_case\")]\nfn with_args() -> u16 {{ 0 }}\n"
        );
        assert_eq!(command_fn_names(&src), vec!["with_args"]);
    }

    #[test]
    fn tauri_roster_accepts_pub_crate_visibility() {
        let src = format!("{}\npub(crate) fn scoped() -> u16 {{ 0 }}\n", attr());
        assert_eq!(command_fn_names(&src), vec!["scoped"]);
    }

    #[test]
    fn tauri_roster_negative_unattributed_fn() {
        assert!(command_fn_names("fn plain() -> u16 { 0 }\n").is_empty());
    }

    /// An attribute whose name has no matching node in that file contributes
    /// nothing rather than a dangling target.
    #[test]
    fn tauri_roster_negative_no_matching_node_in_file() {
        let ctx = TestContext::default().with_file(
            "src/main.rs",
            &format!("{}\nfn ghost() -> u16 {{ 0 }}\n", attr()),
        );
        assert!(roster_keys(&ctx).is_empty());
    }

    /// A `use` of the attribute path is not an attribute: the following byte is a
    /// boundary, but no `fn` declaration follows the token.
    #[test]
    fn tauri_roster_negative_use_statement() {
        let src = format!("use {COMMAND_ATTR};\nfn unrelated() -> u16 {{ 0 }}\n");
        assert!(command_fn_names(&src).is_empty());
    }

    // -----------------------------------------------------------------------
    // T4b — the roster's lexical isolation. The ONLY defence on the Rust side.
    // -----------------------------------------------------------------------

    /// Every masked copy of the attribute mints no node, while the real
    /// UNATTRIBUTED function does — so a naive scan's same-file/same-name join has
    /// exactly one candidate per fake name and it is the wrong one. Measured: a
    /// raw scan yields six keys here; the masked, boundary-checked scan yields one.
    #[test]
    fn tauri_roster_masked_shapes_yield_only_the_real_command() {
        assert_eq!(
            ADVERSARIAL.matches(COMMAND_ATTR).count(),
            6,
            "the fixture must still carry six raw hits"
        );
        assert_eq!(command_fn_names(ADVERSARIAL), vec!["real_cmd"]);
    }

    #[test]
    fn tauri_roster_negative_a_nested_block_comment() {
        let src = format!(
            "/* outer /* inner */ {a}\nfn nested_fake() -> u16 {{ 0 }} */\n\
             fn nested_fake() -> u16 {{ 40 }}\n",
            a = attr()
        );
        assert!(command_fn_names(&src).is_empty());
    }

    #[test]
    fn tauri_roster_negative_b_hash_raw_string() {
        let src = format!(
            "const D1: &str = r##\"\n{a}\nfn hash_fake() -> u16 {{ 1 }}\n\"##;\n\
             fn hash_fake() -> u16 {{ 41 }}\n",
            a = attr()
        );
        assert!(command_fn_names(&src).is_empty());
    }

    #[test]
    fn tauri_roster_negative_c_byte_raw_string() {
        let src = format!(
            "const D2: &[u8] = br##\"\n{a}\nfn byteraw_fake() -> u16 {{ 2 }}\n\"##;\n\
             fn byteraw_fake() -> u16 {{ 42 }}\n",
            a = attr()
        );
        assert!(command_fn_names(&src).is_empty());
    }

    #[test]
    fn tauri_roster_negative_d_byte_string() {
        let src = format!(
            "const D3: &[u8] = b\"{a} fn bytestr_fake() -> u16 {{ 3 }}\";\n\
             fn bytestr_fake() -> u16 {{ 43 }}\n",
            a = attr()
        );
        assert!(command_fn_names(&src).is_empty());
    }

    /// The one negative the mask CANNOT catch: every byte masks `true`, so gate 1
    /// alone accepts it and only the token boundary rejects it.
    #[test]
    fn tauri_roster_negative_e_token_boundary_commandant() {
        let src = format!("#[{COMMAND_ATTR}ant]\nfn attr_boundary_fake() -> u16 {{ 4 }}\n");
        assert!(command_fn_names(&src).is_empty());
    }

    /// What gate 2 buys: the attribute is genuine code, so gate 1 admits it, and a
    /// scan that took the first textual `fn` would key the roster on `fake`.
    #[test]
    fn tauri_roster_negative_f_comment_hidden_fn() {
        let src = format!(
            "{a}\n// fn fake() -> u16 {{ 0 }}\npub fn real_one() -> u16 {{ 1 }}\n\
             fn fake() -> u16 {{ 42 }}\n",
            a = attr()
        );
        assert_eq!(command_fn_names(&src), vec!["real_one"]);
    }

    /// A mask that opens a char state on every `'` masks from the lifetime forward
    /// and swallows the genuine attribute — a silent feature death.
    #[test]
    fn tauri_roster_positive_g_lifetime_does_not_open_char_state() {
        let src = format!(
            "fn life<'a>(s: &'a str) -> &'a str {{ s }}\n{a}\nfn real_cmd() -> u16 {{ 8111 }}\n",
            a = attr()
        );
        assert_eq!(command_fn_names(&src), vec!["real_cmd"]);
    }

    #[test]
    fn tauri_roster_positive_h_escaped_quote_char_literal() {
        let src = format!(
            "const Q: char = '\\'';\n{a}\nfn real_cmd() -> u16 {{ 8111 }}\n",
            a = attr()
        );
        assert_eq!(command_fn_names(&src), vec!["real_cmd"]);
    }

    /// Legal Rust, so a whole-span mask gate fails closed on real code.
    #[test]
    fn tauri_roster_positive_i_doc_comment_interleave() {
        let src = format!(
            "{a}\n/// Returns the MCP port.\n#[allow(dead_code)]\n\
             pub async fn get_mcp_port() -> u16 {{ 8111 }}\n",
            a = attr()
        );
        assert_eq!(command_fn_names(&src), vec!["get_mcp_port"]);
    }

    // -----------------------------------------------------------------------
    // T5 — the poisoning rule (D4)
    // -----------------------------------------------------------------------

    #[test]
    fn tauri_roster_poisons_same_name_commands_in_two_files() {
        let src = format!("{}\nfn save() -> u16 {{ 0 }}\n", attr());
        let ctx = TestContext::default()
            .with_file("a.rs", &src)
            .with_fn("a.rs", "save", 2)
            .with_file("b.rs", &src)
            .with_fn("b.rs", "save", 2);
        assert_eq!(roster_keys(&ctx), vec!["save".to_string()]);
        assert!(live_keys(&ctx).is_empty(), "the key must be POISONED");
    }

    #[test]
    fn tauri_roster_poisons_only_the_ambiguous_camel_spelling() {
        let ctx = TestContext::default()
            .with_file(
                "a.rs",
                &format!(
                    "{a}\nfn get_mcp_port() -> u16 {{ 0 }}\n{a}\nfn getMcpPort() -> u16 {{ 1 }}\n",
                    a = attr()
                ),
            )
            .with_fn("a.rs", "get_mcp_port", 2)
            .with_fn("a.rs", "getMcpPort", 4);
        assert_eq!(live_keys(&ctx), vec!["get_mcp_port".to_string()]);
        assert_eq!(
            build_roster(&ctx).get("getMcpPort"),
            Some(&None),
            "the camel spelling is ambiguous and must be poisoned"
        );
    }

    /// The plan's most plausible lazy-implementation bug: without the "different
    /// target id" clause every single-word command poisons itself.
    #[test]
    fn tauri_roster_single_word_command_is_not_self_poisoned() {
        let ctx = TestContext::default()
            .with_file(
                "a.rs",
                &format!("{}\nfn refresh() -> u16 {{ 0 }}\n", attr()),
            )
            .with_fn("a.rs", "refresh", 2);
        assert_eq!(live_keys(&ctx), vec!["refresh".to_string()]);
    }

    #[test]
    fn tauri_roster_camel_key_reaches_the_same_node() {
        let ctx = TestContext::default()
            .with_file(
                "a.rs",
                &format!("{}\nfn get_mcp_port() -> u16 {{ 0 }}\n", attr()),
            )
            .with_fn("a.rs", "get_mcp_port", 2);
        let roster = build_roster(&ctx);
        let snake = roster.get("get_mcp_port").cloned().flatten();
        let camel = roster.get("getMcpPort").cloned().flatten();
        assert!(snake.is_some());
        assert_eq!(snake.map(|n| n.id), camel.map(|n| n.id));
    }

    /// Two same-named functions in ONE file leave the attribute unable to say
    /// which it belongs to, so the key is poisoned rather than guessed.
    #[test]
    fn tauri_roster_poisons_ambiguous_same_file_nodes() {
        let ctx = TestContext::default()
            .with_file("a.rs", &format!("{}\nfn dup() -> u16 {{ 0 }}\n", attr()))
            .with_fn("a.rs", "dup", 2)
            .with_fn("a.rs", "dup", 9);
        assert!(live_keys(&ctx).is_empty());
    }

    #[test]
    fn tauri_snake_to_camel_shapes() {
        assert_eq!(snake_to_camel("get_mcp_port"), "getMcpPort");
        assert_eq!(snake_to_camel("refresh"), "refresh");
        assert_eq!(snake_to_camel("getMcpPort"), "getMcpPort");
        assert_eq!(snake_to_camel("_private_cmd"), "_privateCmd");
    }

    // -----------------------------------------------------------------------
    // T6 — `claims_reference` is prefix-gated
    // -----------------------------------------------------------------------

    #[test]
    fn tauri_claims_only_the_namespaced_prefix() {
        let r = TauriResolver::default();
        assert!(r.claims_reference("tauri:invoke:get_mcp_port"));
        assert!(!r.claims_reference(INVOKE));
        assert!(!r.claims_reference("get_mcp_port"));
        assert!(!r.claims_reference(""));
    }

    #[test]
    fn tauri_resolver_identity() {
        let r = TauriResolver::default();
        assert_eq!(r.name(), "tauri");
        assert!(r.languages().is_none());
    }

    // -----------------------------------------------------------------------
    // The Rust mask, state by state
    // -----------------------------------------------------------------------

    /// `src` with every masked byte replaced by a space, so a test can assert the
    /// exact code/non-code split rather than a boolean.
    fn masked_text(src: &str) -> String {
        let mask = rust_code_mask(src);
        src.bytes()
            .zip(mask)
            .map(|(b, code)| if code { b as char } else { ' ' })
            .collect()
    }

    #[test]
    fn rust_mask_line_and_nested_block_comments() {
        assert_eq!(masked_text("a // b\nc"), "a     \nc");
        assert_eq!(
            masked_text("a /* x /* y */ z */ b"),
            "a                   b"
        );
    }

    #[test]
    fn rust_mask_raw_strings_capture_hash_count() {
        assert_eq!(masked_text("r##\"a\"#b\"##c"), "           c");
        assert_eq!(masked_text("br#\"x\"#y"), "       y");
        assert_eq!(masked_text("r\"x\"y"), "    y");
    }

    /// The `b` of a byte string stays marked as code — it is consumed as an
    /// identifier before the string state opens — and that is irrelevant to every
    /// gate, which tests the CONTENT bytes. What matters is that the content is
    /// masked, including an escaped quote that must not close the literal early.
    #[test]
    fn rust_mask_byte_and_normal_strings() {
        assert_eq!(masked_text("b\"x\"y"), "b   y");
        assert_eq!(masked_text("\"a\\\"b\"c"), "      c");
    }

    #[test]
    fn rust_mask_lifetime_is_not_a_char_literal() {
        assert_eq!(masked_text("&'a str; x"), "&'a str; x");
        assert_eq!(masked_text("'static X"), "'static X");
    }

    #[test]
    fn rust_mask_char_literals_including_escapes() {
        assert_eq!(masked_text("'\\'' x"), "     x");
        assert_eq!(masked_text("'a' x"), "    x");
    }

    #[test]
    fn rust_mask_multiline_string_is_not_reset_at_newline() {
        assert_eq!(masked_text("\"a\nb\" c"), "      c");
    }

    #[test]
    fn ends_attribute_token_accepts_only_boundaries() {
        assert!(ends_attribute_token(Some(&b']')));
        assert!(ends_attribute_token(Some(&b'(')));
        assert!(ends_attribute_token(Some(&b',')));
        assert!(ends_attribute_token(Some(&b' ')));
        assert!(ends_attribute_token(Some(&b'\n')));
        assert!(!ends_attribute_token(Some(&b'a')));
        assert!(!ends_attribute_token(None));
    }

    // -----------------------------------------------------------------------
    // T7 — the JS/TS mask, one test per lexer state
    // -----------------------------------------------------------------------

    fn js_masked_text(src: &str, jsx: bool) -> String {
        let lex = js_code_mask(src, jsx);
        src.bytes()
            .zip(lex.mask)
            .map(|(b, code)| if code { b as char } else { ' ' })
            .collect()
    }

    #[test]
    fn js_mask_state1_comments_are_not_nestable() {
        assert_eq!(js_masked_text("a // b\nc", false), "a     \nc");
        assert_eq!(
            js_masked_text("a /* x /* y */ z", false),
            "a              z"
        );
    }

    #[test]
    fn js_mask_state2_quotes_reset_at_newline() {
        assert_eq!(js_masked_text("a 'b' c", false), "a     c");
        assert_eq!(js_masked_text("a 'b\nc", false), "a   \nc");
    }

    #[test]
    fn js_mask_state3_template_interpolation_stays_code() {
        assert_eq!(js_masked_text("`a${b}c`", false), "    b   ");
        assert_eq!(js_masked_text("`a\nb` c", false), "      c");
    }

    #[test]
    fn js_mask_state4_regex_versus_division() {
        assert_eq!(js_masked_text("x = /a+/; y", false), "x =     ; y");
        assert_eq!(
            js_masked_text("q = a / b, r = a /2/ b;", false),
            "q = a / b, r = a /2/ b;"
        );
        assert_eq!(js_masked_text("return /a/;", false), "return    ;");
        assert_eq!(js_masked_text("x = /a[/]b/; y", false), "x =        ; y");
    }

    #[test]
    fn js_mask_state5_jsx_text_and_attributes() {
        let src = "const a = <p>invoke('x')</p>;";
        assert_eq!(js_masked_text(src, true), "const a = <p>           </p>;");
        // A generic argument list is NOT a JSX element: reading it as one would
        // mask the real code that follows.
        assert_eq!(
            js_masked_text("const a: Promise<number> = f(); invoke('x');", true),
            "const a: Promise<number> = f(); invoke(   );"
        );
    }

    #[test]
    fn js_mask_state5_jsx_nesting_ends_at_the_outermost_close() {
        let src = "const a = <p>t<b>u</b>v</p>; invoke('x');";
        assert_eq!(
            js_masked_text(src, true),
            "const a = <p> <b> </b> </p>; invoke(   );"
        );
    }

    #[test]
    fn js_mask_literal_spans_are_recorded_only_when_closed() {
        let lex = js_code_mask("a 'b' 'c\n", false);
        assert_eq!(lex.literals.get(&2), Some(&(3, 4)));
        assert_eq!(
            lex.literals.get(&6),
            None,
            "an unterminated literal is not a token"
        );
    }

    // -----------------------------------------------------------------------
    // T8 — `extract()` asserted shape by shape against the extractor's output
    // -----------------------------------------------------------------------

    /// `/tmp/m-matrix`'s measured bytes: 15 shape lines, of which 12 must emit
    /// nothing and 3 must emit.
    const MATRIX_TS: &str = r#"import { invoke } from '@tauri-apps/api/core';
// invoke('save_config')
/* invoke('save_config') */
const doc = "invoke('save_config')";
const tpl = `invoke('save_config')`;
const reEsc = /invoke\('save_config'\)/;
const reRaw = /invoke('save_config')/;
const a = 6, b = 3;
const q = a / b, r = a /2/ b;
export function viaMember() { return client.invoke('save_config'); }
export function dyn(c: string) { return invoke(c); }
export function interp(id: string) { return invoke(`cmd_${id}`); }
export async function multi() {
  return await invoke(
    'save_config'
  );
}
export async function cmt() { return await invoke /* x */ ('save_config'); }
export async function real() { return await invoke('save_config'); }
"#;

    const MATRIX_TSX: &str = r#"import { invoke } from '@tauri-apps/api/core';
export function A() { return <div title="invoke('save_config')">y</div>; }
export function B() { return <p>invoke('save_config')</p>; }
export async function C() { return await invoke('save_config'); }
"#;

    fn positions(sites: &[CallSite]) -> Vec<(i64, i64)> {
        sites.iter().map(|s| (s.line, s.column)).collect()
    }

    /// Assertion 2 of T8: the RECOGNISED call sites must equal the extractor's
    /// bare `invoke | calls` positions exactly — `line` 1-based, `col` 0-based at
    /// the start of the reference expression, both measured on the baseline binary.
    #[test]
    fn tauri_extract_recognised_sites_match_the_extractor_positions() {
        assert_eq!(
            positions(&invoke_call_sites("src/app.ts", MATRIX_TS)),
            vec![(11, 40), (12, 44), (14, 15), (18, 43), (19, 44)]
        );
        assert_eq!(
            positions(&invoke_call_sites("src/comp.tsx", MATRIX_TSX)),
            vec![(4, 41)]
        );
    }

    /// Assertion 1 of T8: three refs from `app.ts`, one from `comp.tsx`. Measured
    /// on these bytes, a raw scan yields 7 in `app.ts`, a mask without the regex
    /// state 5, and mask-plus-receiver-guard-without-regex 4 — so the count moves
    /// for every defect and no `!contains` check separates any of them.
    #[test]
    fn tauri_extract_emits_one_ref_per_literal_call_site() {
        let ts = invoke_call_refs("src/app.ts", MATRIX_TS);
        assert_eq!(ts.len(), 3, "{ts:#?}");
        assert_eq!(
            ts.iter()
                .map(|r| (r.line, r.column, r.reference_name.clone()))
                .collect::<Vec<_>>(),
            vec![
                (14, 15, "tauri:invoke:save_config".to_string()),
                (18, 43, "tauri:invoke:save_config".to_string()),
                (19, 44, "tauri:invoke:save_config".to_string()),
            ]
        );
        let tsx = invoke_call_refs("src/comp.tsx", MATRIX_TSX);
        assert_eq!(tsx.len(), 1, "{tsx:#?}");
        assert_eq!(tsx[0].line, 4);
        assert_eq!(tsx[0].column, 41);
    }

    /// The two rows where the mask AGREES a call site exists and gate 4 still
    /// declines: a non-literal argument has no name to recover, and no dynamic
    /// sentinel is emitted either, because nothing would ever read it.
    #[test]
    fn tauri_extract_dynamic_arguments_are_recognised_but_emit_nothing() {
        let sites = invoke_call_sites("src/app.ts", MATRIX_TS);
        let dynamic: Vec<&CallSite> = sites
            .iter()
            .filter(|s| matches!((s.line, s.column), (11, 40) | (12, 44)))
            .collect();
        assert_eq!(dynamic.len(), 2);
        assert!(dynamic.iter().all(|s| s.wire_name.is_none()));
    }

    #[test]
    fn tauri_extract_ref_shape_is_file_granular_calls() {
        let refs = invoke_call_refs("src/app.ts", MATRIX_TS);
        let first = refs.first().expect("a ref");
        assert_eq!(first.from_node_id, "file:src/app.ts");
        assert_eq!(first.reference_kind, EdgeKind::Calls);
        assert_eq!(first.language, Language::TypeScript);
        assert!(first.row_id.is_none());
        assert!(first.reference_subkind.is_none());
    }

    /// The one enumerated S1 exception (D6): an object-literal call site is a REAL
    /// call the extractor could not attribute to any function, so we emit a
    /// superset there — six refs for six call sites where the extractor records
    /// four rows, the two extra being the object-literal shapes, both attributed
    /// to the file node.
    #[test]
    fn tauri_extract_object_literal_sites_are_a_deliberate_superset() {
        let src = r#"import { invoke } from '@tauri-apps/api/core';
export function plainFn() { return invoke('save_config'); }
export const arrowConst = () => invoke('save_config');
class C { method() { return invoke('save_config'); } }
const obj = { prop() { return invoke('save_config'); } };
const obj2 = { arrow: () => invoke('save_config') };
export default function anon() { return invoke('save_config'); }
"#;
        let refs = invoke_call_refs("src/shapes.ts", src);
        assert_eq!(refs.len(), 6);
        assert_eq!(
            refs.iter().map(|r| r.line).collect::<Vec<_>>(),
            vec![2, 3, 4, 5, 6, 7]
        );
        assert!(refs.iter().all(|r| r.from_node_id == "file:src/shapes.ts"));
    }

    #[test]
    fn tauri_extract_only_runs_on_js_family_files() {
        let ctx = crate::framework::FrameworkExtractionContext::without_config("/root");
        let r = TauriResolver::default();
        assert!(
            r.extract("src-tauri/src/main.rs", "invoke('x')", &ctx)
                .is_none()
        );
        assert!(r.extract("src-tauri/tauri.conf.json", "{}", &ctx).is_none());
        let ts = r
            .extract("src/app.ts", "invoke('x');", &ctx)
            .expect("js file");
        assert_eq!(ts.references.len(), 1);
        assert!(ts.nodes.is_empty(), "this resolver emits NO nodes");
    }

    #[test]
    fn tauri_extract_rejects_non_wire_name_literals() {
        for arg in ["'9lives'", "''", "'a b'", "'a/b'"] {
            let src = format!("invoke({arg});");
            assert!(
                invoke_call_refs("a.ts", &src).is_empty(),
                "{arg} is not a wire name"
            );
        }
        assert_eq!(invoke_call_refs("a.ts", "invoke('a-b.c:d');").len(), 1);
    }

    #[test]
    fn tauri_extract_generic_argument_list_still_reaches_the_call() {
        assert_eq!(
            invoke_call_refs("a.ts", "invoke<number>('get_port');").len(),
            1
        );
    }

    #[test]
    fn tauri_extract_second_literal_cannot_smuggle_a_name() {
        let refs = invoke_call_refs("a.ts", "invoke('save_config', { body: 'other' });");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].reference_name, "tauri:invoke:save_config");
    }

    #[test]
    fn tauri_extract_identifier_boundaries() {
        assert!(invoke_call_refs("a.ts", "invokeAll('x');").is_empty());
        assert!(invoke_call_refs("a.ts", "myinvoke('x');").is_empty());
        assert!(invoke_call_refs("a.ts", "$invoke('x');").is_empty());
    }

    #[test]
    fn is_rust_source_and_js_family_extensions() {
        assert!(is_rust_source("crates/a/src/lib.rs"));
        assert!(!is_rust_source("reference/golden/x/main.rs"));
        assert!(!is_rust_source("src/app.ts"));
        assert!(is_js_family("src/app.ts"));
        assert!(is_js_family("src/comp.tsx"));
        assert!(is_js_family("src/a.mjs"));
        assert!(!is_js_family("src-tauri/src/main.rs"));
    }
}
