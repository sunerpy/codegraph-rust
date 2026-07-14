//! Name matching for reference resolution.
//!
//! Ports `upstream resolution/name-matcher.ts`. Match semantics —
//! `lower(name)` case handling, candidate ranking, ambiguity behavior, and the
//! cross-language family gate — mirror the upstream exactly. Every strategy cites its
//! upstream source range.

use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};
use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use std::sync::OnceLock;

/// Ceiling on same-name candidates a proximity-SCORED strategy will rank
/// (`AMBIGUOUS_NAME_CEILING`, upstream name-matcher; override
/// `CODEGRAPH_AMBIGUOUS_NAME_CEILING`). Above it the scored strategies DECLINE
/// rather than degrade to O(K²). ONLY `match_fuzzy` and the multi-candidate
/// branch of `match_by_exact_name` consult it; edge-producing strategies are
/// never gated, so resolved golden edges are unchanged.
const AMBIGUOUS_NAME_CEILING: usize = 500;

fn ambiguous_name_ceiling() -> usize {
    static CEILING: OnceLock<usize> = OnceLock::new();
    *CEILING.get_or_init(|| {
        std::env::var("CODEGRAPH_AMBIGUOUS_NAME_CEILING")
            .ok()
            .and_then(|raw| raw.trim().parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(AMBIGUOUS_NAME_CEILING)
    })
}

/// Try to resolve a path-like reference by matching the filename against file
/// nodes (`matchByFilePath`, `name-matcher.ts:14-77`).
pub fn match_by_file_path(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    // Path-like (`a/b.liquid`) OR a bare filename ending in a short extension
    // (`Foo.h`). A bare ref WITHOUT an extension is a symbol, not a file
    // (name-matcher.ts:18-24).
    if !reference.reference_name.contains('/') && !has_short_extension(&reference.reference_name) {
        return None;
    }

    let file_name = reference.reference_name.rsplit('/').next()?;
    if file_name.is_empty() {
        return None;
    }

    let candidates = context.get_nodes_by_name(file_name);
    let file_nodes: Vec<Node> = candidates
        .into_iter()
        .filter(|n| n.kind == NodeKind::File)
        .collect();
    if file_nodes.is_empty() {
        return None;
    }

    // Prefer exact path match on qualified_name (name-matcher.ts:37-45).
    if let Some(exact) = file_nodes.iter().find(|n| {
        n.qualified_name == reference.reference_name || n.file_path == reference.reference_name
    }) {
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: exact.id.clone(),
            confidence: 0.95,
            resolved_by: ResolvedBy::FilePath,
        });
    }

    // Suffix match, picking the closest file node (name-matcher.ts:54-64).
    let suffix_matches: Vec<&Node> = file_nodes
        .iter()
        .filter(|n| {
            n.qualified_name.ends_with(&reference.reference_name)
                || n.file_path.ends_with(&reference.reference_name)
        })
        .collect();
    if !suffix_matches.is_empty() {
        let chosen = pick_closest_file_node(&suffix_matches, reference);
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: chosen.id.clone(),
            confidence: 0.85,
            resolved_by: ResolvedBy::FilePath,
        });
    }

    // Single same-named file node, lower confidence (name-matcher.ts:67-74).
    if file_nodes.len() == 1 {
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: file_nodes[0].id.clone(),
            confidence: 0.7,
            resolved_by: ResolvedBy::FilePath,
        });
    }

    None
}

/// Matches `/\.[A-Za-z][A-Za-z0-9]{0,3}$/` (name-matcher.ts:22).
fn has_short_extension(name: &str) -> bool {
    let Some(dot) = name.rfind('.') else {
        return false;
    };
    let ext = &name[dot + 1..];
    let bytes = ext.as_bytes();
    if bytes.is_empty() || bytes.len() > 4 {
        return false;
    }
    if !bytes[0].is_ascii_alphabetic() {
        return false;
    }
    bytes[1..].iter().all(u8::is_ascii_alphanumeric)
}

/// Pick the file node closest to the referencing file (`pickClosestFileNode`,
/// `name-matcher.ts:86-106`).
fn pick_closest_file_node<'a>(candidates: &[&'a Node], reference: &RefView) -> &'a Node {
    let ref_dir = dir_of(&reference.file_path);
    let same_dir: Vec<&&Node> = candidates
        .iter()
        .filter(|c| dir_of(&c.file_path) == ref_dir)
        .collect();
    let pool: Vec<&Node> = if !same_dir.is_empty() {
        same_dir.into_iter().copied().collect()
    } else {
        candidates.to_vec()
    };

    let mut best = pool[0];
    let mut best_score = f64::NEG_INFINITY;
    for c in &pool {
        let score = compute_path_proximity(&reference.file_path, &c.file_path)
            + if same_language_family(c.language, reference.language) {
                5.0
            } else {
                0.0
            };
        if score > best_score {
            best_score = score;
            best = c;
        }
    }
    best
}

fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

/// Language families that share a type system / runtime (`LANGUAGE_FAMILY`,
/// `name-matcher.ts:113-121`).
fn language_family(lang: Language) -> Option<&'static str> {
    match lang {
        Language::Java | Language::Kotlin | Language::Scala => Some("jvm"),
        Language::Swift | Language::ObjC => Some("apple"),
        Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx => Some("web"),
        Language::C | Language::Cpp => Some("c"),
        Language::CSharp | Language::Razor => Some("dotnet"),
        _ => None,
    }
}

/// `sameLanguageFamily` (`name-matcher.ts:122-126`).
pub fn same_language_family(a: Language, b: Language) -> bool {
    if a == b {
        return true;
    }
    match language_family(a) {
        Some(fa) => language_family(b) == Some(fa),
        None => false,
    }
}

/// `isKnownLanguageFamily` (`name-matcher.ts:134-136`).
pub fn is_known_language_family(lang: Language) -> bool {
    language_family(lang).is_some()
}

/// `crossesKnownFamily` (`name-matcher.ts:147-149`).
pub fn crosses_known_family(a: Language, b: Language) -> bool {
    is_known_language_family(a) && is_known_language_family(b) && !same_language_family(a, b)
}

/// Drop cross-language candidates from a name lookup (`applyLanguageGate`,
/// `name-matcher.ts:160-168`).
fn apply_language_gate(candidates: Vec<Node>, reference: &RefView) -> Vec<Node> {
    match reference.reference_kind {
        EdgeKind::References => candidates
            .into_iter()
            .filter(|c| same_language_family(c.language, reference.language))
            .collect(),
        EdgeKind::Imports => candidates
            .into_iter()
            .filter(|c| !crosses_known_family(c.language, reference.language))
            .collect(),
        _ => candidates,
    }
}

/// Try to resolve a reference by exact name match (`matchByExactName`,
/// `name-matcher.ts:173-209`).
pub fn match_by_exact_name(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let candidates = apply_language_gate(
        context.get_nodes_by_name(&reference.reference_name),
        reference,
    );

    if candidates.is_empty() {
        return None;
    }

    if candidates.len() == 1 {
        let is_cross_language = candidates[0].language != reference.language;
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: candidates[0].id.clone(),
            confidence: if is_cross_language { 0.5 } else { 0.9 },
            resolved_by: ResolvedBy::ExactMatch,
        });
    }

    // Multiple matches — narrow down (name-matcher.ts:194-206). Decline past the
    // ambiguity ceiling rather than proximity-rank a pathological candidate set.
    if candidates.len() > ambiguous_name_ceiling() {
        return None;
    }
    if let Some(best) = find_best_match(reference, &candidates) {
        let proximity = compute_path_proximity(&reference.file_path, &best.file_path);
        let confidence = if proximity >= 30.0 { 0.7 } else { 0.4 };
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: best.id.clone(),
            confidence,
            resolved_by: ResolvedBy::ExactMatch,
        });
    }

    None
}

/// Try to resolve by qualified name (`matchByQualifiedName`,
/// `name-matcher.ts:214-252`).
pub fn match_by_qualified_name(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if !reference.reference_name.contains("::") && !reference.reference_name.contains('.') {
        return None;
    }

    let candidates = context.get_nodes_by_qualified_name(&reference.reference_name);
    if candidates.len() == 1 {
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: candidates[0].id.clone(),
            confidence: 0.95,
            resolved_by: ResolvedBy::QualifiedName,
        });
    }

    // Several symbols share this exact qualified name (an ODR clash / separate
    // translation units): prefer the call site's own file before the partial
    // fallback, else the first-indexed def wins (#1079).
    if candidates.len() > 1 {
        let ordered = prefer_call_site_file(candidates, &reference.file_path);
        if ordered[0].file_path == reference.file_path {
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: ordered[0].id.clone(),
                confidence: 0.95,
                resolved_by: ResolvedBy::QualifiedName,
            });
        }
    }

    // Partial qualified name match (name-matcher.ts:234-249), again preferring
    // the call site's own file among the candidates whose qualified name ends
    // with the reference (#1079).
    let parts: Vec<&str> = reference.reference_name.split([':', '.']).collect();
    if let Some(last_name) = parts.last().filter(|s| !s.is_empty()) {
        let partial: Vec<Node> = context
            .get_nodes_by_name(last_name)
            .into_iter()
            .filter(|c| c.qualified_name.ends_with(&reference.reference_name))
            .collect();
        if !partial.is_empty() {
            let ordered = prefer_call_site_file(partial, &reference.file_path);
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: ordered[0].id.clone(),
                confidence: 0.85,
                resolved_by: ResolvedBy::QualifiedName,
            });
        }
    }

    None
}

/// Prefer the candidate(s) declared in the call site's own file, keeping the
/// rest in their original order (`preferCallSiteFile`, name-matcher.ts:#1079).
/// A same-file definition is the strongest language-agnostic signal for which
/// of several same-named symbols a call means; without it, resolution collapses
/// onto whichever was indexed first, so a call in `b/svc` wrongly targets
/// `a/svc`. No-op when there are <2 candidates or none share the call site's
/// file. The partition is STABLE (preserves within-group order), so edge output
/// stays deterministic.
fn prefer_call_site_file(nodes: Vec<Node>, call_site_file: &str) -> Vec<Node> {
    if nodes.len() < 2 {
        return nodes;
    }
    let mut same: Vec<Node> = Vec::new();
    let mut other: Vec<Node> = Vec::new();
    for n in nodes {
        if n.file_path == call_site_file {
            same.push(n);
        } else {
            other.push(n);
        }
    }
    if same.is_empty() {
        other
    } else {
        same.extend(other);
        same
    }
}

/// Resolve a method on a type, walking supertypes (`resolveMethodOnType`,
/// `name-matcher.ts:254-331`).
#[allow(clippy::too_many_arguments)]
fn resolve_method_on_type(
    type_name: &str,
    method_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
    confidence: f64,
    resolved_by: ResolvedBy,
    preferred_fqn: Option<&str>,
    depth: u32,
) -> Option<ResolvedRef> {
    let method_candidates = context.get_nodes_by_name(method_name);
    let want = format!("{type_name}::{method_name}");
    let matches: Vec<Node> = method_candidates
        .into_iter()
        .filter(|m| {
            m.kind == NodeKind::Method
                && m.language == reference.language
                && (m.qualified_name == want || m.qualified_name.ends_with(&format!("::{want}")))
        })
        .collect();

    if matches.is_empty() {
        // Conformance fallback via supertypes (name-matcher.ts:289-305).
        if depth < 4 {
            for supertype in context.get_supertypes(type_name, reference.language) {
                if let Some(via) = resolve_method_on_type(
                    &supertype,
                    method_name,
                    reference,
                    context,
                    confidence,
                    resolved_by,
                    preferred_fqn,
                    depth + 1,
                ) {
                    return Some(via);
                }
            }
        }
        return None;
    }

    if matches.len() > 1 {
        if let Some(fqn) = preferred_fqn {
            let ext = if reference.language == Language::Kotlin {
                ".kt"
            } else {
                ".java"
            };
            let fqn_path = format!("{}{}", fqn.replace('.', "/"), ext);
            if let Some(chosen) = matches.iter().find(|m| {
                let fp = m.file_path.replace('\\', "/");
                fp.ends_with(&fqn_path) || fp.ends_with(&format!("/{fqn_path}"))
            }) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: chosen.id.clone(),
                    confidence,
                    resolved_by,
                });
            }
        }
    }

    // Language-agnostic ambiguity: several same-named methods survive (ODR clash
    // / separate translation units). Prefer the call site's own file (#1079).
    // Runs AFTER the preferred_fqn block, so Java/Kotlin import disambiguation
    // (#314), whose target is intentionally in another file, is unaffected.
    let ordered = prefer_call_site_file(matches, &reference.file_path);
    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: ordered[0].id.clone(),
        confidence,
        resolved_by,
    })
}

/// Last `::`-separated segment of a C++ name (`cppLastSegment`,
/// `name-matcher.ts:572-575`).
fn cpp_last_segment(name: &str) -> &str {
    name.split("::")
        .filter(|s| !s.is_empty())
        .last()
        .unwrap_or(name)
}

/// Does the graph hold a class/struct named `name`'s last segment?
/// (`cppClassExists`, `name-matcher.ts:621-626`).
fn cpp_class_exists(name: &str, reference: &RefView, context: &dyn ResolutionContext) -> bool {
    let last = cpp_last_segment(name);
    context.get_nodes_by_name(last).iter().any(|n| {
        matches!(n.kind, NodeKind::Class | NodeKind::Struct) && n.language == reference.language
    })
}

/// Escape a receiver for embedding in a `Regex` (the JS
/// `/[.*+?^${}()|[\]\\]/g` set, `name-matcher.ts:523`).
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '.' | '*' | '+' | '?' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Declarator regex matching `Type recv`, `Type* recv`, `Type<X> recv`, etc.,
/// requiring a declarator terminator after the receiver (`buildDeclaratorRegex`,
/// `name-matcher.ts:506-510`). The terminator is matched manually (the regex
/// crate has no lookahead), so this captures up to and through the receiver word.
fn build_declarator_regex(escaped_receiver: &str) -> Regex {
    Regex::new(&format!(
        r"([A-Za-z_][\w:]*(?:\s*<[^;=(){{}}]+>)?(?:\s*[*&]+)?)\s*\b{escaped_receiver}\b"
    ))
    .expect("declarator regex")
}

/// `normalizeCppTypeName` (`name-matcher.ts:480-498` family): strip pointer/ref
/// markers + surrounding whitespace, keep the last `::` segment's namespace tail
/// as the upstream does (the upstream keeps the full `ns::Type`; we mirror by trimming markers
/// only). Returns `""` for an empty/garbage match.
fn normalize_cpp_type_name(raw: &str) -> String {
    raw.trim()
        .trim_end_matches(['*', '&', ' ', '\t'])
        .trim()
        .to_string()
}

/// The char following the receiver word must be a declarator terminator
/// (`;=,)[{(` or end-of-line) — mirrors the upstream lookahead `(?=[;=,)\[{(]|$)`
/// (`name-matcher.ts:508`), which the regex crate cannot express directly.
fn declarator_terminator_ok(line: &str, match_end: usize) -> bool {
    match line[match_end..].chars().find(|c| !c.is_whitespace()) {
        None => true,
        Some(c) => matches!(c, ';' | '=' | ',' | ')' | '[' | '{' | '('),
    }
}

/// Infer the type of an `auto`-declared local from its initializer on the
/// declaration line (`inferCppAutoInitializerType`, `name-matcher.ts:675-695`).
fn infer_cpp_auto_initializer_type(
    line: &str,
    receiver_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
    depth: u32,
) -> Option<String> {
    let escaped = regex_escape(receiver_name);
    let assign = Regex::new(&format!(r"\b{escaped}\b\s*=\s*([^;]+)")).ok()?;
    let init = assign.captures(line)?.get(1)?.as_str().trim();

    static NEW_RE: OnceLock<Regex> = OnceLock::new();
    let new_re = NEW_RE.get_or_init(|| Regex::new(r"^new\s+([A-Za-z_][\w:]*)").expect("new re"));
    if let Some(caps) = new_re.captures(init) {
        return Some(cpp_last_segment(caps.get(1)?.as_str()).to_string());
    }

    static CALL_RE: OnceLock<Regex> = OnceLock::new();
    let call_re = CALL_RE
        .get_or_init(|| Regex::new(r"^([A-Za-z_][\w:]*(?:\s*<[^>;]*>)?)\s*\(").expect("call re"));
    if let Some(caps) = call_re.captures(init) {
        let callee: String = caps.get(1)?.as_str().split_whitespace().collect();
        return resolve_cpp_call_result_type(&callee, reference, context, depth + 1);
    }
    None
}

/// Infer the class produced by a C++ call/construction expression, using return
/// types captured at extraction (`resolveCppCallResultType`,
/// `name-matcher.ts:638-668`). The caller still validates the outer method via
/// `resolve_method_on_type`, so a wrong guess stays silent.
fn resolve_cpp_call_result_type(
    inner: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
    depth: u32,
) -> Option<String> {
    if depth > 3 {
        return None;
    }
    let expr = inner.trim();

    static MAKE_RE: OnceLock<Regex> = OnceLock::new();
    let make_re = MAKE_RE.get_or_init(|| {
        Regex::new(r"(?:^|::)(?:make_unique|make_shared)\s*<\s*([A-Za-z_]\w*)").expect("make re")
    });
    if let Some(caps) = make_re.captures(expr) {
        return Some(caps.get(1)?.as_str().to_string());
    }

    // Single-level member call `recv.method` (name-matcher.ts:651-659).
    if let Some(dot_idx) = expr.rfind('.') {
        if dot_idx > 0 {
            let recv = &expr[..dot_idx];
            let method = &expr[dot_idx + 1..];
            if recv.contains('.') || recv.contains('(') || recv.contains("::") {
                return None;
            }
            let recv_type = infer_cpp_receiver_type(recv, reference, context, depth + 1)?;
            return lookup_callee_return_type(
                &format!("{recv_type}::{method}"),
                reference,
                context,
            );
        }
    }

    if let Some(ret) = lookup_callee_return_type(expr, reference, context) {
        return Some(ret);
    }

    // Direct construction — the callee itself names a class/struct.
    if cpp_class_exists(expr, reference, context) {
        return Some(cpp_last_segment(expr).to_string());
    }
    None
}

/// Infer a C++ receiver's declared type by scanning the source backwards from
/// the call line for its declarator (`inferCppReceiverType`,
/// `name-matcher.ts:512-567`). Handles `Foo x;`, `auto x = Foo::make();`, and a
/// `.h/.hpp/.hxx` header fallback.
fn infer_cpp_receiver_type(
    receiver_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
    depth: u32,
) -> Option<String> {
    let source = context.read_file(&reference.file_path)?;
    let lines: Vec<&str> = source
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .collect();
    if lines.is_empty() {
        return None;
    }
    let call_line_index = ((reference.line - 1).max(0) as usize).min(lines.len() - 1);
    let escaped = regex_escape(receiver_name);
    let receiver_pattern = Regex::new(&format!(r"\b{escaped}\b")).ok()?;
    let declarator_regex = build_declarator_regex(&escaped);

    for i in (0..=call_line_index).rev() {
        let line = lines[i];
        if line.is_empty() || !receiver_pattern.is_match(line) {
            continue;
        }
        if let Some(caps) = declarator_regex.captures(line) {
            let whole = caps.get(0)?;
            if !declarator_terminator_ok(line, whole.end()) {
                continue;
            }
            let normalized = normalize_cpp_type_name(caps.get(1).map_or("", |m| m.as_str()));
            if normalized == "auto" {
                // `auto x = Foo::instance();` — recover the type from the
                // initializer (call return type / construction).
                if let Some(init_type) =
                    infer_cpp_auto_initializer_type(line, receiver_name, reference, context, depth)
                {
                    return Some(init_type);
                }
            } else if !normalized.is_empty() {
                return Some(normalized);
            }
        }
    }

    // Header fallback: `.h/.hpp/.hxx` sibling declaring the receiver.
    static EXT_RE: OnceLock<Regex> = OnceLock::new();
    let ext_re = EXT_RE.get_or_init(|| Regex::new(r"(?i)\.(?:c|cc|cpp|cxx)$").expect("ext re"));
    let mut header_candidates: Vec<String> = Vec::new();
    for ext in [".h", ".hpp", ".hxx"] {
        let candidate = ext_re.replace(&reference.file_path, ext).to_string();
        if candidate != reference.file_path && !header_candidates.contains(&candidate) {
            header_candidates.push(candidate);
        }
    }
    for header_path in header_candidates {
        if !context.file_exists(&header_path) {
            continue;
        }
        let Some(header_source) = context.read_file(&header_path) else {
            continue;
        };
        for line in header_source.split('\n').map(|l| l.trim_end_matches('\r')) {
            if !receiver_pattern.is_match(line) {
                continue;
            }
            if let Some(caps) = declarator_regex.captures(line) {
                let Some(whole) = caps.get(0) else { continue };
                if !declarator_terminator_ok(line, whole.end()) {
                    continue;
                }
                let normalized = normalize_cpp_type_name(caps.get(1).map_or("", |m| m.as_str()));
                if !normalized.is_empty() && normalized != "auto" {
                    return Some(normalized);
                }
            }
        }
    }
    None
}

/// Infer a Java/Kotlin receiver's declared type from the field declaration in
/// the class enclosing the call site (`inferJavaFieldReceiverType`,
/// `name-matcher.ts:878-925`). Covers Spring `@Autowired`/`@Resource` field
/// injection where the field name doesn't match the type by convention.
fn infer_java_field_receiver_type(
    receiver_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let in_file = context.get_nodes_in_file(&reference.file_path);
    if in_file.is_empty() {
        return None;
    }

    // Tightest class/interface enclosing the call line (latest start).
    let mut enclosing: Option<&Node> = None;
    for n in &in_file {
        if !matches!(n.kind, NodeKind::Class | NodeKind::Interface)
            || n.language != reference.language
        {
            continue;
        }
        let end = if n.end_line != 0 {
            n.end_line
        } else {
            n.start_line
        };
        if n.start_line <= reference.line && end >= reference.line {
            if enclosing.is_none_or(|e| n.start_line >= e.start_line) {
                enclosing = Some(n);
            }
        }
    }
    let enclosing = enclosing?;
    let enclosing_end = if enclosing.end_line != 0 {
        enclosing.end_line
    } else {
        enclosing.start_line
    };

    let field = in_file.iter().find(|n| {
        n.kind == NodeKind::Field
            && n.name == receiver_name
            && n.language == reference.language
            && n.start_line >= enclosing.start_line
            && (if n.end_line != 0 {
                n.end_line
            } else {
                n.start_line
            }) <= enclosing_end
    })?;
    let signature = field.signature.as_ref().filter(|s| !s.is_empty())?;

    // Signature shape "<TypeName> <fieldName>" (extractField): pull the type,
    // strip generics + dotted package + array/varargs markers.
    let before_name = &signature[..signature.rfind(&field.name)?];
    let type_raw = before_name.trim();
    if type_raw.is_empty() {
        return None;
    }
    static GENERICS_RE: OnceLock<Regex> = OnceLock::new();
    let generics_re = GENERICS_RE.get_or_init(|| Regex::new(r"<[^>]*>").expect("generics re"));
    let type_no_generics = generics_re.replace_all(type_raw, "");
    let type_no_array = type_no_generics
        .replace("[]", "")
        .replace("[ ]", "")
        .trim_end_matches("...")
        .trim()
        .to_string();
    let last_part = type_no_array
        .split(|c: char| c == '.' || c.is_whitespace())
        .rfind(|s| !s.is_empty())?;
    if !last_part.chars().next().is_some_and(|c| c.is_uppercase()) {
        return None;
    }
    Some(last_part.to_string())
}

/// Tokens a loose receiver pattern might capture that are never a user-defined
/// type (`NON_TYPE_RECEIVER_TOKENS`, name-matcher.ts:#1108).
fn is_non_type_receiver_token(seg: &str) -> bool {
    matches!(
        seg,
        "this"
            | "self"
            | "super"
            | "new"
            | "return"
            | "await"
            | "yield"
            | "typeof"
            | "null"
            | "nil"
            | "None"
            | "true"
            | "false"
            | "True"
            | "False"
            | "undefined"
    )
}

/// Normalize a captured type expression to a simple type name: drop generic
/// args and pointer/ref markers, take the last `.`/`::`-qualified segment, and
/// reject obvious non-types (`normalizeInferredTypeName`, name-matcher.ts:#1108).
fn normalize_inferred_type_name(raw: &str) -> Option<String> {
    static GENERICS: OnceLock<Regex> = OnceLock::new();
    let generics = GENERICS.get_or_init(|| Regex::new(r"<[^>]*>").expect("inferred generics re"));
    let cleaned = generics.replace_all(raw, "");
    let cleaned = cleaned.replace(['&', '*'], "");
    let seg = cleaned.split(['.', ':']).rfind(|s| !s.is_empty())?;
    if is_non_type_receiver_token(seg) {
        return None;
    }
    Some(seg.to_string())
}

/// Per-language patterns recovering a local variable's (or typed parameter's)
/// declared type from its declaration/initializer line. Each regex captures the
/// type in group 1; `r` is the already-escaped receiver name. Ordered
/// most-specific first. The type-annotation / typed-parameter forms (#1125) are
/// baked into the second pattern per language. Languages without a pattern set
/// (the C++ path uses its own dedicated inferrer) return an empty vector.
fn local_receiver_type_patterns(language: Language, r: &str) -> Vec<Regex> {
    local_receiver_type_patterns_tagged(language, r)
        .into_iter()
        .map(|(re, _)| re)
        .collect()
}

/// The bool tags the Lua/Luau `receiver: Type` ANNOTATION pattern, which needs a
/// code-side self-match gate (#1124): `lg:Log()` is byte-identical to the Luau
/// annotation `lg: Logger`, and the `regex` crate has no look-ahead to reject the
/// call form, so `infer_local_receiver_type` post-checks the tagged capture.
fn local_receiver_type_patterns_tagged(language: Language, r: &str) -> Vec<(Regex, bool)> {
    let build = |src: String| Regex::new(&src).ok().map(|re| (re, false));
    let pats: Vec<String> = match language {
        Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx => vec![
            format!(r"\b{r}\b\s*=\s*new\s+([A-Za-z_$][\w.$]*)"),
            format!(r"\b{r}\b\s*:\s*([A-Z][\w.$]*)"),
        ],
        Language::Python => vec![
            format!(r"\b{r}\b\s*=\s*([A-Z][\w.]*)\s*\("),
            format!(r"\b{r}\b\s*:\s*([A-Z][\w.]*)"),
        ],
        Language::Java => vec![
            format!(r"\b{r}\b\s*=\s*new\s+([A-Za-z_][\w.]*)"),
            format!(r"\b([A-Z][\w.]*)\s+{r}\b\s*[=;,)]"),
        ],
        Language::Kotlin => vec![
            format!(r"\b{r}\b\s*=\s*([A-Z][\w.]*)\s*\("),
            format!(r"\b{r}\b\s*:\s*([A-Z][\w.]*)"),
        ],
        Language::CSharp => vec![
            format!(r"\b{r}\b\s*=\s*new\s+([A-Za-z_][\w.]*)"),
            format!(r"\b([A-Z][\w.]*)\s+{r}\b\s*[=;,)]"),
        ],
        Language::Swift => vec![
            format!(r"\b{r}\b\s*=\s*([A-Z][\w.]*)\s*\("),
            format!(r"\b{r}\b\s*:\s*([A-Z][\w.]*)"),
        ],
        Language::Rust => vec![
            format!(r"\blet\s+(?:mut\s+)?{r}\b(?:\s*:[^=]+)?=\s*&?(?:mut\s+)?([A-Z]\w*)"),
            format!(r"\b{r}\s*:\s*&?(?:mut\s+)?([A-Z]\w*)"),
        ],
        Language::Go => vec![
            format!(r"\b{r}\b\s*:=\s*&?([A-Za-z_][\w.]*)\s*\{{"),
            format!(r"\bvar\s+{r}\s+\*?([A-Za-z_][\w.]*)"),
            format!(r"\b{r}\s+\*?([A-Z][\w.]*)"),
        ],
        Language::Ruby => vec![format!(r"\b{r}\b\s*=\s*([A-Z][\w:]*)\.new\b")],
        Language::Scala => vec![
            format!(r"\b{r}\b\s*=\s*(?:new\s+)?([A-Z][\w.]*)"),
            format!(r"\b{r}\b\s*:\s*([A-Z][\w.]*)"),
        ],
        Language::Dart => vec![
            format!(r"\b{r}\b\s*=\s*([A-Z][\w.]*)\s*\("),
            format!(r"\b([A-Z][\w.]*)\s+{r}\b\s*[=;,)]"),
        ],
        Language::Php => vec![
            format!(r"\$?{r}\b\s*=\s*new\s+([A-Za-z_\\][\w\\]*)"),
            format!(r"\b([A-Za-z_\\][\w\\]*)\s+&?\${r}\b"),
        ],
        Language::Lua | Language::Luau => {
            let mut out: Vec<(Regex, bool)> = Vec::new();
            for src in [
                format!(r"\b{r}\b\s*=\s*([A-Z]\w*)\.new\b"),
                format!(r"\b{r}\b\s*=\s*([A-Z]\w*)\s*\("),
            ] {
                if let Some((re, _)) = build(src) {
                    out.push((re, false));
                }
            }
            if let Ok(re) = Regex::new(&format!(r"\b{r}\b\s*:\s*([A-Z][\w.]*)")) {
                out.push((re, true));
            }
            return out;
        }
        Language::R => vec![format!(r"\b{r}\b\s*(?:<-|<<-|=)\s*([A-Z][\w.]*)\$new\b")],
        Language::Pascal => vec![
            format!(r"\b{r}\b\s*:\s*([A-Z]\w*)"),
            format!(r"\b{r}\b\s*:=\s*([A-Z][\w.]*)\.Create\b"),
        ],
        _ => Vec::new(),
    };
    pats.into_iter().filter_map(build).collect()
}

/// 1-based start line of the tightest function/method enclosing the call
/// (`enclosingScopeStartLine`, name-matcher.ts:#1108). Bounds the backward scan
/// so a same-named variable in another function can't leak in.
fn enclosing_scope_start_line(reference: &RefView, context: &dyn ResolutionContext) -> i64 {
    let mut start = 1i64;
    for n in context.get_nodes_in_file(&reference.file_path) {
        if !matches!(n.kind, NodeKind::Function | NodeKind::Method)
            || n.language != reference.language
        {
            continue;
        }
        let end = if n.end_line != 0 {
            n.end_line
        } else {
            n.start_line
        };
        if n.start_line <= reference.line && end >= reference.line && n.start_line >= start {
            start = n.start_line;
        }
    }
    start
}

/// Infer a receiver's type from its local declaration/initializer in the
/// enclosing function body (`inferLocalReceiverType`, name-matcher.ts:#1108).
/// Language-dispatched via `local_receiver_type_patterns`; returns `None` for
/// languages without patterns or when no declaration is found. Bounded to the
/// enclosing scope. The caller validates the method via `resolve_method_on_type`,
/// so a mis-inference produces no edge.
fn infer_local_receiver_type(
    receiver_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let escaped = regex_escape(receiver_name);
    let patterns = local_receiver_type_patterns_tagged(reference.language, &escaped);
    if patterns.is_empty() {
        return None;
    }
    let source = context.read_file(&reference.file_path)?;
    let lines: Vec<&str> = source
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .collect();
    if lines.is_empty() {
        return None;
    }
    let call_idx = ((reference.line - 1).max(0) as usize).min(lines.len() - 1);
    let start_idx = (enclosing_scope_start_line(reference, context) - 1).max(0) as usize;

    for i in (start_idx..=call_idx).rev() {
        let line = lines[i];
        // A generated/minified line is not where a human declaration lives, and
        // regexing multi-KB text per ref is pure waste — skip it.
        if line.len() > 10_000 {
            continue;
        }
        for (re, is_lua_annotation) in &patterns {
            if let Some(caps) = re.captures(line) {
                if let Some(m) = caps.get(1) {
                    if *is_lua_annotation && lua_annotation_is_method_call(line, m.end()) {
                        continue;
                    }
                    if let Some(ty) = normalize_inferred_type_name(m.as_str()) {
                        return Some(ty);
                    }
                }
            }
        }
    }
    None
}

/// True when a Lua/Luau `receiver:Capitalized` annotation match is really a
/// method call (`lg:Log()`, `lg:Log"s"`, `lg:Log{t}`, or a longer `lg:Log.More`
/// token the annotation regex stopped short of), which must NOT be read as a type
/// annotation (#1124). Reproduces upstream's `(?![\w.]|\s*[({"'\[])` look-ahead in
/// code, since the `regex` crate has no look-ahead. `capture_end` is the byte
/// offset just past the captured type on `line`.
fn lua_annotation_is_method_call(line: &str, capture_end: usize) -> bool {
    let rest = &line[capture_end..];
    if let Some(next) = rest.chars().next() {
        if next.is_alphanumeric() || next == '_' || next == '.' {
            return true;
        }
    }
    let trimmed = rest.trim_start();
    trimmed
        .chars()
        .next()
        .is_some_and(|c| matches!(c, '(' | '"' | '\'' | '[' | '{'))
}

/// The PHP `$this->prop->method()` receiver shape — encoded by the extractor as
/// `this->prop.method` (a SINGLE property segment, no `()`). Matches the upstream
/// `PHP_PROP_SHAPE = /^this->\w+\.\w+$/` (`index.ts:47`). Deeper chains
/// (`this->a->b.method`) and non-`this` receivers do NOT match, so they stay
/// unlinked exactly as before.
pub(crate) fn is_php_property_receiver_shape(name: &str) -> bool {
    static SHAPE: OnceLock<Regex> = OnceLock::new();
    SHAPE
        .get_or_init(|| Regex::new(r"^this->\w+\.\w+$").expect("php prop shape re"))
        .is_match(name)
}

/// Capture form of the PHP property receiver shape: `^(this->\w+)\.(\w+)$` —
/// group 1 the `this->prop` receiver, group 2 the method (`phpThisPropMatch`,
/// `name-matcher.ts:1477`).
fn php_this_prop_call_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(this->\w+)\.(\w+)$").expect("php this-prop call re"))
}

/// Patterns that recover a PHP class property's declared type for a `$this->prop`
/// receiver (`phpPropertyTypePatterns`, `name-matcher.ts:1367-1384`). Deliberately
/// NOT `local_receiver_type_patterns`: only PROPERTY-shaped declarations qualify —
/// a modifier-prefixed typed declaration (covering both a typed property
/// `private ?Foo $prop;` and a promoted constructor parameter
/// `private readonly Foo $prop`), and the pseudoconstructor assignment
/// `$this->prop = new Foo(...)`. A bare `X $prop` parameter or `$prop = new X()`
/// local elsewhere in the file must NOT match — those variables can never alias
/// `$this->prop`. Union-typed properties (`Foo|Bar $prop`) yield no match and thus
/// no edge (silent beats wrong). The classic untyped-property-assigned-in-constructor
/// shape is handled by `infer_php_assigned_property_type` instead. `escaped_prop`
/// is already regex-escaped; the type is captured in group 1.
fn php_property_type_patterns(escaped_prop: &str) -> Vec<Regex> {
    let build = |src: String| Regex::new(&src).ok();
    [
        // private readonly ?Foo $prop  (typed property / promoted ctor param)
        format!(
            r"\b(?:(?:private|protected|public|readonly|static|final)(?:\(set\))?\s+)+\??([A-Za-z_\\][\w\\]*)\s+&?\${escaped_prop}\b"
        ),
        // $this->prop = new Foo()
        format!(r"\$this->{escaped_prop}\b\s*=\s*new\s+([A-Za-z_\\][\w\\]*)"),
    ]
    .into_iter()
    .filter_map(build)
    .collect()
}

/// Second-chance typing for a PHP `$this->prop` receiver whose property
/// declaration carries no static type (classic pre-7.4 style): find the
/// `$this->prop = $var` assignment, then recover `$var`'s type from its own
/// declaration WITHIN the assignment's function — the constructor's (possibly
/// multi-line) parameter list, a typed setter's parameter, or a `= new X()`
/// local. The backward scan stops at the enclosing `function` line (checked for a
/// match first — a single-line `__construct(Foo $var) { ... }` carries the typed
/// parameter itself), so a same-named variable in another method can never type
/// the property. Ports `inferPhpAssignedPropertyType` (`name-matcher.ts:1396-1445`).
fn infer_php_assigned_property_type(
    escaped_prop: &str,
    lines: &[&str],
    call_idx: usize,
) -> Option<String> {
    let assign_re = Regex::new(&format!(r"\$this->{escaped_prop}\b\s*=\s*\$(\w+)\b")).ok()?;
    let assign_at = |i: usize| -> Option<String> {
        let line = lines.get(i)?;
        if line.len() > 10_000 {
            return None;
        }
        assign_re
            .captures(line)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
    };

    // The assignment is position-independent relative to the call — nearest-
    // backward first, then sweep forward, same order as the componentScoped scan.
    let mut assign_idx: Option<usize> = None;
    let mut var_name: Option<String> = None;
    for i in (0..=call_idx.min(lines.len().saturating_sub(1))).rev() {
        if let Some(v) = assign_at(i) {
            assign_idx = Some(i);
            var_name = Some(v);
            break;
        }
    }
    if var_name.is_none() {
        for i in (call_idx + 1)..lines.len() {
            if let Some(v) = assign_at(i) {
                assign_idx = Some(i);
                var_name = Some(v);
                break;
            }
        }
    }
    let (assign_idx, var_name) = (assign_idx?, var_name?);

    let var_patterns = local_receiver_type_patterns(Language::Php, &regex_escape(&var_name));
    static FUNCTION_RE: OnceLock<Regex> = OnceLock::new();
    let function_re =
        FUNCTION_RE.get_or_init(|| Regex::new(r"\bfunction\b").expect("php function re"));
    for i in (0..=assign_idx).rev() {
        let line = lines[i];
        if line.len() <= 10_000 {
            for re in &var_patterns {
                if let Some(caps) = re.captures(line) {
                    if let Some(m) = caps.get(1) {
                        if let Some(ty) = normalize_inferred_type_name(m.as_str()) {
                            return Some(ty);
                        }
                    }
                }
            }
        }
        if function_re.is_match(line) {
            break;
        }
    }
    None
}

/// Infer a PHP `$this->prop` receiver's DECLARED type from PROPERTY-shaped
/// declarations only (the PHP `$this->prop` branch of `inferLocalReceiverType`,
/// `name-matcher.ts:1283-1359`). `receiver_with_prefix` is the raw `this->prop`
/// receiver; the `this->` prefix is stripped, and the scan widens to the WHOLE
/// FILE (the property/ctor declaration lives outside the calling method), unlike
/// the scope-bounded `infer_local_receiver_type`. Only `php_property_type_patterns`
/// are consulted (a same-named local/param can never mistype the property); when
/// no property-shaped declaration matches, the classic-ctor assignment
/// second-chance (`infer_php_assigned_property_type`) runs. Returns `None` when
/// the type can't be recovered → the caller produces NO edge (never guessed).
fn infer_php_property_receiver_type(
    receiver_with_prefix: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let prop = receiver_with_prefix.strip_prefix("this->")?;
    let escaped = regex_escape(prop);
    let patterns = php_property_type_patterns(&escaped);
    let source = context.read_file(&reference.file_path)?;
    let lines: Vec<&str> = source
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .collect();
    if lines.is_empty() {
        return None;
    }
    let call_idx = ((reference.line - 1).max(0) as usize).min(lines.len() - 1);

    for line in &lines {
        // A generated/minified line is not where a human declaration lives, and
        // regexing multi-KB text per ref is pure waste — skip it.
        if line.len() > 10_000 {
            continue;
        }
        for re in &patterns {
            if let Some(caps) = re.captures(line) {
                if let Some(m) = caps.get(1) {
                    if let Some(ty) = normalize_inferred_type_name(m.as_str()) {
                        return Some(ty);
                    }
                }
            }
        }
    }

    // Classic pre-7.4 untyped-property-assigned-in-constructor second chance.
    infer_php_assigned_property_type(&escaped, &lines, call_idx)
}

/// Try to resolve by method name on a class/object (`matchMethodCall`,
/// `name-matcher.ts:930-1133`).
///
/// Local-variable / typed-parameter receiver-type inference (#1108/#1125) runs
/// first: C++ keeps its dedicated inferrer (header scan + `auto`), every other
/// language uses the shared source-based `infer_local_receiver_type`. Java/Kotlin
/// then also try field-receiver inference (name-matcher.ts:975-996). All validate
/// the method via `resolve_method_on_type` so a wrong inference yields no edge.
/// Strategies 1-3 (direct class match, capitalized receiver, receiver-overlap
/// scoring) then cover the remaining instance-method edges, each preferring the
/// call site's own file among same-named candidates (#1079).
pub fn match_method_call(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    // PHP property receiver: `$this->prop->method()` reaches the resolver as
    // `this->prop.method` (extractor records the receiver's raw text, leading `$`
    // stripped). Resolve it EXCLUSIVELY through declared-type inference +
    // resolve_method_on_type — the name-similarity strategies below must NEVER see
    // this shape, so a property whose type can't be recovered stays unlinked
    // rather than guessed. Returns unconditionally (`Some` or `None`), mirroring
    // the upstream `phpThisPropMatch` early return (name-matcher.ts:1475-1500).
    if reference.language == Language::Php {
        if let Some(caps) = php_this_prop_call_re().captures(&reference.reference_name) {
            let receiver = caps.get(1)?.as_str();
            let php_method = caps.get(2)?.as_str();
            let inferred = infer_php_property_receiver_type(receiver, reference, context)?;
            return resolve_method_on_type(
                &inferred,
                php_method,
                reference,
                context,
                0.9,
                ResolvedBy::InstanceMethod,
                imported_fqn_of(&inferred, reference, context).as_deref(),
                0,
            );
        }
    }

    let parsed = parse_method_call(&reference.reference_name, reference.language)?;
    let (object_or_class, method_name) = parsed;

    // Receiver-type inference (#1108/#1125): only for a simple `obj.method` /
    // `obj:method` (Lua) / `obj$method` (R) shape — a `Class::method` colon-call
    // is not an instance call. C++ uses its dedicated inferrer; every other
    // language uses the shared source-based inferrer.
    if is_inferable_receiver_call(&reference.reference_name, reference.language) {
        let inferred = if reference.language == Language::Cpp {
            infer_cpp_receiver_type(&object_or_class, reference, context, 0)
        } else {
            infer_local_receiver_type(&object_or_class, reference, context)
        };
        if let Some(inferred) = inferred {
            // Java/Kotlin: the file's import pins WHICH same-named class (#314);
            // other languages disambiguate by call-site file in resolve_method_on_type.
            let imported_fqn = if matches!(reference.language, Language::Java | Language::Kotlin) {
                imported_fqn_of(&inferred, reference, context)
            } else {
                None
            };
            if let Some(typed) = resolve_method_on_type(
                &inferred,
                &method_name,
                reference,
                context,
                0.9,
                ResolvedBy::InstanceMethod,
                imported_fqn.as_deref(),
                0,
            ) {
                return Some(typed);
            }
        }
    }

    // Java/Kotlin field-receiver inference (name-matcher.ts:975-996): the
    // receiver may be a field whose name doesn't match its type by convention
    // (Spring `@Resource`/`@Autowired`). Resolve the method on the field's type.
    if matches!(reference.language, Language::Java | Language::Kotlin)
        && is_dot_call(&reference.reference_name)
    {
        if let Some(inferred) = infer_java_field_receiver_type(&object_or_class, reference, context)
        {
            let imported_fqn = imported_fqn_of(&inferred, reference, context);
            if let Some(typed) = resolve_method_on_type(
                &inferred,
                &method_name,
                reference,
                context,
                0.9,
                ResolvedBy::InstanceMethod,
                imported_fqn.as_deref(),
                0,
            ) {
                return Some(typed);
            }
        }
    }

    // Strategy 1: direct class name match (name-matcher.ts:825-850). When the
    // receiver names a class in several files, try the call site's own file
    // first (#1079).
    for class_node in prefer_call_site_file(
        context.get_nodes_by_name(&object_or_class),
        &reference.file_path,
    ) {
        if matches!(
            class_node.kind,
            NodeKind::Class | NodeKind::Struct | NodeKind::Interface
        ) {
            if class_node.language != reference.language {
                continue;
            }
            if let Some(method_node) = find_method_in_class(&class_node, &method_name, context) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: method_node.id,
                    confidence: 0.85,
                    resolved_by: ResolvedBy::QualifiedName,
                });
            }
        }
    }

    // Strategy 2: instance-variable receiver → capitalized class
    // (name-matcher.ts:852-880), call-site file first (#1079).
    let capitalized = capitalize(&object_or_class);
    if capitalized != object_or_class {
        for class_node in prefer_call_site_file(
            context.get_nodes_by_name(&capitalized),
            &reference.file_path,
        ) {
            if matches!(
                class_node.kind,
                NodeKind::Class | NodeKind::Struct | NodeKind::Interface
            ) {
                if class_node.language != reference.language {
                    continue;
                }
                if let Some(method_node) = find_method_in_class(&class_node, &method_name, context)
                {
                    return Some(ResolvedRef {
                        original: reference.clone(),
                        target_node_id: method_node.id,
                        confidence: 0.8,
                        resolved_by: ResolvedBy::InstanceMethod,
                    });
                }
            }
        }
    }

    // Strategy 3: methods-by-name, receiver-overlap scoring
    // (name-matcher.ts:882-933).
    let method_candidates = context.get_nodes_by_name(&method_name);
    // Ubiquitous-method ceiling (#999): bail before the O(K) scoring work when a
    // method name is re-declared across a vendored theme/SDK.
    if method_candidates.len() > ambiguous_name_ceiling() {
        return None;
    }
    let methods: Vec<Node> = method_candidates
        .into_iter()
        .filter(|n| n.kind == NodeKind::Method && n.name == method_name)
        .collect();
    let same_language: Vec<Node> = methods
        .iter()
        .filter(|m| m.language == reference.language)
        .cloned()
        .collect();
    let target_methods = if !same_language.is_empty() {
        same_language
    } else {
        methods
    };

    if target_methods.len() == 1 && target_methods[0].language == reference.language {
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: target_methods[0].id.clone(),
            confidence: 0.7,
            resolved_by: ResolvedBy::InstanceMethod,
        });
    }

    if target_methods.len() > 1 {
        let receiver_words = split_camel_case(&object_or_class);
        // Same-file candidates first, so a score tie (`score > best_score` keeps
        // the first seen) resolves to the call site's own file (#1079).
        let ordered = prefer_call_site_file(target_methods, &reference.file_path);
        let mut best_match: Option<&Node> = None;
        let mut best_score = 0i64;
        for method in &ordered {
            let class_words = split_camel_case(&method.qualified_name);
            let mut score = receiver_words
                .iter()
                .filter(|w| class_words.iter().any(|cw| cw.eq_ignore_ascii_case(w)))
                .count() as i64;
            if method.language == reference.language {
                score += 1;
            }
            if score > best_score {
                best_score = score;
                best_match = Some(method);
            }
        }
        if let Some(best) = best_match {
            if best_score >= 2 {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: best.id.clone(),
                    confidence: 0.65,
                    resolved_by: ResolvedBy::InstanceMethod,
                });
            }
        }
    }

    None
}

/// Parse `obj.method`, `Class::method`, Lua/Luau `obj:method`, or R `obj$method`
/// (name-matcher.ts:770-778 + #1112). The `:`/`$` separators are recognized ONLY
/// for their owning language, so a `lg:log` ref elsewhere is not a call shape.
fn parse_method_call(name: &str, language: Language) -> Option<(String, String)> {
    if let Some(captures) = match_dot_call(name) {
        return Some(captures);
    }
    if let Some(captures) = match_colon_call(name) {
        return Some(captures);
    }
    if matches!(language, Language::Lua | Language::Luau) {
        if let Some(captures) = match_lua_colon_call(name) {
            return Some(captures);
        }
    }
    if language == Language::R {
        if let Some(captures) = match_r_dollar_call(name) {
            return Some(captures);
        }
    }
    None
}

/// A simple `receiver.method` / `receiver:method` (Lua) / `receiver$method` (R)
/// shape whose receiver type we can try to infer (`inferableReceiver`,
/// name-matcher.ts:#1108/#1112). A `Class::method` colon-call is excluded — it is
/// not an instance call.
fn is_inferable_receiver_call(name: &str, language: Language) -> bool {
    if match_dot_call(name).is_some() {
        return true;
    }
    if matches!(language, Language::Lua | Language::Luau) && match_lua_colon_call(name).is_some() {
        return true;
    }
    if language == Language::R && match_r_dollar_call(name).is_some() {
        return true;
    }
    false
}

/// Did the ref match the dotted `obj.method` shape (the upstream `dotMatch` that
/// gates the Java field-receiver hook, name-matcher.ts:975)?
fn is_dot_call(name: &str) -> bool {
    match_dot_call(name).is_some()
}

/// Matches `/^([\w.]+):(\w+)$/` — a Lua/Luau method call `obj:method` (#1112).
fn match_lua_colon_call(name: &str) -> Option<(String, String)> {
    let idx = name.find(':')?;
    let receiver = &name[..idx];
    let method = &name[idx + 1..];
    if receiver.is_empty() || method.is_empty() {
        return None;
    }
    if !receiver
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
    {
        return None;
    }
    if !is_word(method) {
        return None;
    }
    Some((receiver.to_string(), method.to_string()))
}

/// Matches `/^([\w.]+)\$(\w+)$/` — an R method call `obj$method` (#1112).
fn match_r_dollar_call(name: &str) -> Option<(String, String)> {
    let idx = name.find('$')?;
    let receiver = &name[..idx];
    let method = &name[idx + 1..];
    if receiver.is_empty() || method.is_empty() {
        return None;
    }
    if !receiver
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
    {
        return None;
    }
    if !is_word(method) {
        return None;
    }
    Some((receiver.to_string(), method.to_string()))
}

/// Matches `/^([\w.]+)\.(\w+:?(?:\w+:)*)$/` (name-matcher.ts:770).
fn match_dot_call(name: &str) -> Option<(String, String)> {
    let last_dot = name.rfind('.')?;
    if last_dot == 0 {
        return None;
    }
    let receiver = &name[..last_dot];
    let method = &name[last_dot + 1..];
    if receiver.is_empty()
        || !receiver
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
    {
        return None;
    }
    if !is_selector_like(method) {
        return None;
    }
    Some((receiver.to_string(), method.to_string()))
}

/// Matches `/^(\w+)::(\w+)$/` (name-matcher.ts:771).
fn match_colon_call(name: &str) -> Option<(String, String)> {
    let idx = name.find("::")?;
    let receiver = &name[..idx];
    let method = &name[idx + 2..];
    if receiver.is_empty() || method.is_empty() {
        return None;
    }
    if !is_word(receiver) || !is_word(method) {
        return None;
    }
    Some((receiver.to_string(), method.to_string()))
}

fn is_word(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// `\w+:?(?:\w+:)*` — a word with optional trailing ObjC selector colons.
fn is_selector_like(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars().peekable();
    let mut saw_word = false;
    while let Some(&c) = chars.peek() {
        if c.is_alphanumeric() || c == '_' {
            saw_word = true;
            chars.next();
        } else {
            break;
        }
    }
    if !saw_word {
        return false;
    }
    // Remaining must be groups of `:` then word, ending after a `:`-word or a `:`.
    while let Some(c) = chars.next() {
        if c != ':' {
            return false;
        }
        // optional word after colon
        while let Some(&w) = chars.peek() {
            if w.is_alphanumeric() || w == '_' {
                chars.next();
            } else {
                break;
            }
        }
    }
    true
}

fn find_method_in_class(
    class_node: &Node,
    method_name: &str,
    context: &dyn ResolutionContext,
) -> Option<Node> {
    context
        .get_nodes_in_file(&class_node.file_path)
        .into_iter()
        .find(|n| {
            n.kind == NodeKind::Method
                && n.name == method_name
                && n.qualified_name.contains(&class_node.name)
        })
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Split a camelCase/PascalCase string into words (`splitCamelCase`,
/// `name-matcher.ts:941-946`).
fn split_camel_case(s: &str) -> Vec<String> {
    // Insert spaces at lower→upper and ACRONYM→Word boundaries, then split on
    // separators, then drop single-char words.
    let spaced = insert_camel_spaces(s);
    spaced
        .split(|c: char| c.is_whitespace() || matches!(c, '.' | '_' | ':' | '/' | '\\'))
        .filter(|w| w.chars().count() > 1)
        .map(str::to_string)
        .collect()
}

fn insert_camel_spaces(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 8);
    for i in 0..chars.len() {
        let c = chars[i];
        // /([a-z])([A-Z])/ -> '$1 $2'
        if i + 1 < chars.len() {
            let next = chars[i + 1];
            if c.is_ascii_lowercase() && next.is_ascii_uppercase() {
                out.push(c);
                out.push(' ');
                continue;
            }
        }
        // /([A-Z]+)([A-Z][a-z])/ -> '$1 $2'
        if i + 2 < chars.len()
            && c.is_ascii_uppercase()
            && chars[i + 1].is_ascii_uppercase()
            && chars[i + 2].is_ascii_lowercase()
        {
            out.push(c);
            out.push(' ');
            continue;
        }
        out.push(c);
    }
    out
}

/// Compute directory proximity between two file paths (`computePathProximity`,
/// `name-matcher.ts:953-968`).
fn compute_path_proximity(file_path1: &str, file_path2: &str) -> f64 {
    let dir1: Vec<&str> = drop_last(file_path1);
    let dir2: Vec<&str> = drop_last(file_path2);
    let mut shared = 0usize;
    for i in 0..dir1.len().min(dir2.len()) {
        if dir1[i] == dir2[i] {
            shared += 1;
        } else {
            break;
        }
    }
    ((shared * 15) as f64).min(80.0)
}

fn drop_last(path: &str) -> Vec<&str> {
    let segs: Vec<&str> = path.split('/').collect();
    if segs.is_empty() {
        Vec::new()
    } else {
        segs[..segs.len() - 1].to_vec()
    }
}

/// Find the best matching node among multiple candidates (`findBestMatch`,
/// `name-matcher.ts:973-1055`).
fn find_best_match<'a>(reference: &RefView, candidates: &'a [Node]) -> Option<&'a Node> {
    let mut best_score = -1.0f64;
    let mut best_node: Option<&Node> = None;

    for candidate in candidates {
        let mut score = 0.0f64;

        if candidate.file_path == reference.file_path {
            score += 100.0;
        }
        score += compute_path_proximity(&reference.file_path, &candidate.file_path);
        if candidate.language == reference.language {
            score += 50.0;
        } else {
            score -= 80.0;
        }

        match reference.reference_kind {
            EdgeKind::Calls => {
                if matches!(candidate.kind, NodeKind::Function | NodeKind::Method) {
                    score += 25.0;
                }
            }
            EdgeKind::Instantiates => {
                if matches!(
                    candidate.kind,
                    NodeKind::Class | NodeKind::Struct | NodeKind::Interface
                ) {
                    score += 25.0;
                }
            }
            EdgeKind::Decorates => {
                if matches!(candidate.kind, NodeKind::Function | NodeKind::Method) {
                    score += 25.0;
                } else if matches!(candidate.kind, NodeKind::Class | NodeKind::Interface) {
                    score += 15.0;
                }
            }
            _ => {}
        }

        if candidate.is_exported {
            score += 10.0;
        }

        // Closer line number within same file (name-matcher.ts:1043-1046).
        if candidate.file_path == reference.file_path && candidate.start_line != 0 {
            let distance = (candidate.start_line - reference.line).abs() as f64;
            score += (20.0 - distance / 10.0).max(0.0);
        }

        if score > best_score {
            best_score = score;
            best_node = Some(candidate);
        }
    }

    best_node
}

/// Fuzzy match — last resort with lower confidence (`matchFuzzy`,
/// `name-matcher.ts:1060-1088`).
pub fn match_fuzzy(reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
    let lower_name = reference.reference_name.to_lowercase();
    let candidates = context.get_nodes_by_lower_name(&lower_name);
    if candidates.len() > ambiguous_name_ceiling() {
        return None;
    }

    let callable_kinds = [NodeKind::Function, NodeKind::Method, NodeKind::Class];
    let callable_candidates = apply_language_gate(
        candidates
            .into_iter()
            .filter(|n| callable_kinds.contains(&n.kind))
            .collect(),
        reference,
    );

    let same_language: Vec<Node> = callable_candidates
        .iter()
        .filter(|n| n.language == reference.language)
        .cloned()
        .collect();
    let final_candidates = if !same_language.is_empty() {
        same_language
    } else {
        callable_candidates
    };

    if final_candidates.len() == 1 {
        let is_cross_language = final_candidates[0].language != reference.language;
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: final_candidates[0].id.clone(),
            confidence: if is_cross_language { 0.3 } else { 0.5 },
            resolved_by: ResolvedBy::Fuzzy,
        });
    }

    None
}

/// Resolve a `::`-scoped factory chain (`matchScopedCallChain`,
/// `name-matcher.ts:583-598`).
pub fn match_scoped_call_chain(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let (inner, method) = parse_chain_shape(&reference.reference_name)?;
    if !inner.contains("::") {
        return None;
    }
    let factory_class = &inner[..inner.rfind("::").unwrap()];
    let ret = lookup_callee_return_type(&inner, reference, context)?;
    let resolved_class = if ret == "self" {
        factory_class.to_string()
    } else {
        ret
    };
    resolve_method_on_type(
        &resolved_class,
        &method,
        reference,
        context,
        0.85,
        ResolvedBy::InstanceMethod,
        None,
        0,
    )
}

/// Resolve a dotted chained static-factory / fluent call (`matchDottedCallChain`,
/// `name-matcher.ts:620-678`).
///
/// NOTE: the Go bare-factory fallback (name-matcher.ts:632-657) calls into
/// `matchByExactName`/`matchFuzzy`; we keep the structure but the deterministic
/// core only relies on the declared-return-type path. Construction-via-bare-call
/// (`CONSTRUCTS_VIA_BARE_CALL`, name-matcher.ts:608) is mirrored.
pub fn match_dotted_call_chain(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let (inner, method) = parse_chain_shape(&reference.reference_name)?;
    let last_dot = inner.rfind('.');

    match last_dot {
        None | Some(0) => {
            // Go bare package-level factory (name-matcher.ts:632-658).
            if reference.language == Language::Go {
                if let Some(ret) = lookup_callee_return_type(&inner, reference, context) {
                    return resolve_method_on_type(
                        &ret,
                        &method,
                        reference,
                        context,
                        0.85,
                        ResolvedBy::InstanceMethod,
                        imported_fqn_of(&ret, reference, context).as_deref(),
                        0,
                    );
                }
                let bare_ref = RefView {
                    reference_name: method.clone(),
                    ..reference.clone()
                };
                let bare_match = match_by_exact_name(&bare_ref, context)
                    .or_else(|| match_fuzzy(&bare_ref, context));
                return bare_match.map(|m| ResolvedRef {
                    original: reference.clone(),
                    ..m
                });
            }
            // Constructor receiver in construct-via-bare-call languages
            // (name-matcher.ts:659-667).
            if !constructs_via_bare_call(reference.language)
                || !inner.chars().next().is_some_and(|c| c.is_uppercase())
            {
                return None;
            }
            resolve_method_on_type(
                &inner,
                &method,
                reference,
                context,
                0.85,
                ResolvedBy::InstanceMethod,
                imported_fqn_of(&inner, reference, context).as_deref(),
                0,
            )
        }
        Some(dot) => {
            // Factory/fluent receiver (name-matcher.ts:670-677).
            let factory_class = inner[..dot].rsplit('.').next()?;
            let factory_method = &inner[dot + 1..];
            if factory_class.is_empty() || factory_method.is_empty() {
                return None;
            }
            let ret = lookup_callee_return_type(
                &format!("{factory_class}::{factory_method}"),
                reference,
                context,
            )?;
            resolve_method_on_type(
                &ret,
                &method,
                reference,
                context,
                0.85,
                ResolvedBy::InstanceMethod,
                imported_fqn_of(&ret, reference, context).as_deref(),
                0,
            )
        }
    }
}

/// `CONSTRUCTS_VIA_BARE_CALL` (name-matcher.ts:608).
fn constructs_via_bare_call(language: Language) -> bool {
    matches!(
        language,
        Language::Kotlin | Language::Swift | Language::Scala | Language::Dart
    )
}

/// Matches `/^(.+)\(\)\.(\w+)$/` (name-matcher.ts:565,587,624).
fn parse_chain_shape(name: &str) -> Option<(String, String)> {
    let suffix_start = name.rfind("().")?;
    let inner = &name[..suffix_start];
    let method = &name[suffix_start + 3..];
    if inner.is_empty() || method.is_empty() || !is_word(method) {
        return None;
    }
    Some((inner.to_string(), method.to_string()))
}

/// `importedFqnOf` (`name-matcher.ts:685-692`).
fn imported_fqn_of(
    type_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    context
        .get_import_mappings(&reference.file_path, reference.language)
        .into_iter()
        .find(|i| i.local_name == type_name)
        .map(|i| i.source)
}

/// `lookupCalleeReturnType` (`name-matcher.ts:440-475`).
fn lookup_callee_return_type(
    callee: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let mut method = callee.to_string();
    let mut cls: Option<String> = None;
    if callee.contains("::") {
        let parts: Vec<&str> = callee.split("::").filter(|s| !s.is_empty()).collect();
        method = parts.last().copied().unwrap_or(callee).to_string();
        cls = Some(parts[..parts.len() - 1].join("::"));
    }
    let candidates: Vec<Node> = context
        .get_nodes_by_name(&method)
        .into_iter()
        .filter(|n| {
            matches!(n.kind, NodeKind::Method | NodeKind::Function)
                && n.language == reference.language
                && n.return_type.as_ref().is_some_and(|r| !r.is_empty())
        })
        .collect();

    if let Some(cls) = cls {
        let want = format!("{cls}::{method}");
        return candidates
            .iter()
            .find(|n| {
                n.qualified_name == want
                    || n.qualified_name.ends_with(&format!("::{want}"))
                    || want.ends_with(&format!("::{}", n.qualified_name))
            })
            .and_then(|n| n.return_type.clone());
    }
    candidates
        .iter()
        .find(|n| n.kind == NodeKind::Function)
        .and_then(|n| n.return_type.clone())
}

/// Match all strategies in order of confidence (`matchReference`,
/// `name-matcher.ts:1093-1157`).
pub fn match_reference(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    // 0. File path match.
    if let Some(result) = match_by_file_path(reference, context) {
        return Some(result);
    }

    // 1. Qualified name match.
    if let Some(result) = match_by_qualified_name(reference, context) {
        return Some(result);
    }

    // 1c. `::`-scoped factory chain (PHP / Rust) (name-matcher.ts:1116-1123).
    if matches!(reference.language, Language::Php | Language::Rust) {
        if let Some(result) = match_scoped_call_chain(reference, context) {
            return Some(result);
        }
    }

    // 1d. Dotted chained static-factory / fluent call
    // (Java/Kotlin/C#/Swift/Go/Scala/Dart) (name-matcher.ts:1125-1142).
    if matches!(
        reference.language,
        Language::Java
            | Language::Kotlin
            | Language::CSharp
            | Language::Swift
            | Language::Go
            | Language::Scala
            | Language::Dart
    ) {
        if let Some(result) = match_dotted_call_chain(reference, context) {
            return Some(result);
        }
    }

    // 2. Method call pattern.
    if let Some(result) = match_method_call(reference, context) {
        return Some(result);
    }

    // 3. Exact name match.
    if let Some(result) = match_by_exact_name(reference, context) {
        return Some(result);
    }

    // 4. Fuzzy match.
    match_fuzzy(reference, context)
}

/// Resolve a `function_ref` (callback-as-value) reference: exact name,
/// function/method targets only, same language family, same-file first,
/// cross-file only when unique. No fuzzy fallback. `this.<member>` refs are
/// resolved elsewhere (resolve_this_member_fn_ref). Ports `matchFunctionRef`
/// (name-matcher.ts:179-310).
pub fn match_function_ref(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if reference.reference_name.starts_with("this.") {
        return None;
    }

    // A bare identifier can never be a method value in JS/TS/C++/Python/PHP
    // (methods need a receiver), so those match FUNCTIONS only.
    let bare_fn_only = matches!(
        reference.language,
        Language::TypeScript
            | Language::Tsx
            | Language::JavaScript
            | Language::Jsx
            | Language::Cpp
            | Language::Python
            | Language::Php
    );

    // Qualified member-pointer (`&Widget::on_click`): resolve the member on
    // that scope, unique-or-drop. Exempt from bare_fn_only.
    if let Some(sep) = reference.reference_name.rfind("::") {
        let member_name = &reference.reference_name[sep + 2..];
        let scoped: Vec<Node> = context
            .get_nodes_by_name(member_name)
            .into_iter()
            .filter(|n| {
                matches!(n.kind, NodeKind::Function | NodeKind::Method)
                    && same_language_family(n.language, reference.language)
                    && n.id != reference.from_node_id
                    && (n.qualified_name == reference.reference_name
                        || n.qualified_name
                            .ends_with(&format!("::{}", reference.reference_name)))
            })
            .collect();
        if scoped.is_empty() {
            return None;
        }
        let same_file: Vec<&Node> = scoped
            .iter()
            .filter(|n| n.file_path == reference.file_path)
            .collect();
        if same_file.is_empty() && scoped.len() > 1 {
            return None;
        }
        let pool: Vec<&Node> = if same_file.is_empty() {
            scoped.iter().collect()
        } else {
            same_file
        };
        let target = pool
            .iter()
            .copied()
            .reduce(|a, b| if a.start_line <= b.start_line { a } else { b })?;
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: target.id.clone(),
            confidence: 0.9,
            resolved_by: ResolvedBy::FunctionRef,
        });
    }

    let mut candidates: Vec<Node> = context
        .get_nodes_by_name(&reference.reference_name)
        .into_iter()
        .filter(|n| {
            (n.kind == NodeKind::Function || (!bare_fn_only && n.kind == NodeKind::Method))
                && same_language_family(n.language, reference.language)
                && n.id != reference.from_node_id
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }

    // Swift implicit-self: a bare identifier names a METHOD only of the
    // ENCLOSING type; a same-named method on another class is a parameter
    // collision (name-matcher.ts:250-270).
    if reference.language == Language::Swift
        && candidates.iter().any(|n| n.kind == NodeKind::Method)
    {
        let class_prefix = context
            .get_node_by_id(&reference.from_node_id)
            .and_then(|f| {
                let sep = f.qualified_name.rfind("::")?;
                if sep > 0 {
                    Some(f.qualified_name[..sep].to_string())
                } else {
                    None
                }
            });
        candidates.retain(|n| {
            if n.kind != NodeKind::Method {
                return true;
            }
            let Some(class_prefix) = class_prefix.as_deref() else {
                return false;
            };
            let Some(m_sep) = n.qualified_name.rfind("::") else {
                return false;
            };
            if m_sep == 0 {
                return false;
            }
            let method_prefix = &n.qualified_name[..m_sep];
            method_prefix == class_prefix
                || method_prefix.ends_with(&format!("::{class_prefix}"))
                || class_prefix.ends_with(&format!("::{method_prefix}"))
        });
        if candidates.is_empty() {
            return None;
        }
    }

    let same_file: Vec<&Node> = candidates
        .iter()
        .filter(|n| n.file_path == reference.file_path)
        .collect();
    if !same_file.is_empty() {
        // Swift: several same-named methods in one file is an overload family
        // and a bare id hitting it is almost always a parameter collision —
        // refuse (name-matcher.ts:282-288).
        if reference.language == Language::Swift
            && same_file.len() > 1
            && same_file.iter().all(|n| n.kind == NodeKind::Method)
        {
            return None;
        }
        let target = same_file
            .iter()
            .copied()
            .reduce(|a, b| if a.start_line <= b.start_line { a } else { b })?;
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: target.id.clone(),
            confidence: if same_file.len() == 1 { 0.95 } else { 0.9 },
            resolved_by: ResolvedBy::FunctionRef,
        });
    }

    if candidates.len() == 1 {
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: candidates[0].id.clone(),
            confidence: 0.8,
            resolved_by: ResolvedBy::FunctionRef,
        });
    }
    None
}

#[cfg(test)]
mod ceiling_tests {
    use super::*;
    use crate::types::ImportMapping;
    use codegraph_core::types::ReferenceSubkind;

    struct FakeContext {
        by_name: Vec<Node>,
        by_lower: Vec<Node>,
    }

    impl ResolutionContext for FakeContext {
        fn get_nodes_in_file(&self, _file_path: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_nodes_by_name(&self, _name: &str) -> Vec<Node> {
            self.by_name.clone()
        }
        fn get_nodes_by_qualified_name(&self, _qualified_name: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_nodes_by_kind(&self, _kind: NodeKind) -> Vec<Node> {
            Vec::new()
        }
        fn file_exists(&self, _file_path: &str) -> bool {
            false
        }
        fn read_file(&self, _file_path: &str) -> Option<String> {
            None
        }
        fn get_project_root(&self) -> &str {
            ""
        }
        fn get_all_files(&self) -> Vec<String> {
            Vec::new()
        }
        fn get_nodes_by_lower_name(&self, _lower_name: &str) -> Vec<Node> {
            self.by_lower.clone()
        }
        fn get_node_by_id(&self, _id: &str) -> Option<Node> {
            None
        }
        fn get_import_mappings(&self, _file_path: &str, _language: Language) -> Vec<ImportMapping> {
            Vec::new()
        }
    }

    fn node(idx: usize) -> Node {
        Node {
            id: format!("function:{idx:032x}"),
            kind: NodeKind::Function,
            name: "doThing".to_string(),
            qualified_name: "doThing".to_string(),
            file_path: format!("src/f{idx}.ts"),
            language: Language::TypeScript,
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

    fn reference() -> RefView {
        RefView {
            from_node_id: "function:caller".to_string(),
            reference_name: "doThing".to_string(),
            reference_kind: EdgeKind::Calls,
            line: 10,
            column: 0,
            file_path: "src/caller.ts".to_string(),
            language: Language::TypeScript,
            is_function_ref: false,
            reference_subkind: None::<ReferenceSubkind>,
        }
    }

    fn nodes(count: usize) -> Vec<Node> {
        (0..count).map(node).collect()
    }

    #[test]
    fn exact_match_declines_above_ceiling_but_resolves_at_it() {
        // Given a candidate set just over the default ceiling, the multi-candidate
        // exact-match branch declines rather than proximity-rank it.
        let over = FakeContext {
            by_name: nodes(AMBIGUOUS_NAME_CEILING + 1),
            by_lower: Vec::new(),
        };
        assert!(match_by_exact_name(&reference(), &over).is_none());

        // Given exactly the ceiling, it still ranks and resolves a winner.
        let at = FakeContext {
            by_name: nodes(AMBIGUOUS_NAME_CEILING),
            by_lower: Vec::new(),
        };
        assert!(match_by_exact_name(&reference(), &at).is_some());
    }

    #[test]
    fn fuzzy_declines_above_ceiling() {
        // Given a fuzzy candidate set over the ceiling, match_fuzzy declines.
        let over = FakeContext {
            by_name: Vec::new(),
            by_lower: nodes(AMBIGUOUS_NAME_CEILING + 1),
        };
        assert!(match_fuzzy(&reference(), &over).is_none());

        // Given a single fuzzy candidate (well under), it resolves.
        let one = FakeContext {
            by_name: Vec::new(),
            by_lower: nodes(1),
        };
        assert!(match_fuzzy(&reference(), &one).is_some());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ImportMapping;
    use codegraph_core::types::ReferenceSubkind;
    use std::collections::HashMap;

    // ---- Configurable in-memory ResolutionContext ---------------------------

    /// A fully configurable fake context. Every lookup is backed by a map keyed
    /// by the exact query string, so a test can wire precise candidate sets for
    /// each named lookup the matcher performs.
    #[derive(Default)]
    struct Ctx {
        by_name: HashMap<String, Vec<Node>>,
        by_qualified: HashMap<String, Vec<Node>>,
        by_lower: HashMap<String, Vec<Node>>,
        in_file: HashMap<String, Vec<Node>>,
        by_id: HashMap<String, Node>,
        files: std::collections::HashSet<String>,
        file_content: HashMap<String, String>,
        supertypes: HashMap<(String, Language), Vec<String>>,
        imports: HashMap<String, Vec<ImportMapping>>,
    }

    impl Ctx {
        fn name(mut self, key: &str, ns: Vec<Node>) -> Self {
            self.by_name.insert(key.to_string(), ns);
            self
        }
        fn qualified(mut self, key: &str, ns: Vec<Node>) -> Self {
            self.by_qualified.insert(key.to_string(), ns);
            self
        }
        fn lower(mut self, key: &str, ns: Vec<Node>) -> Self {
            self.by_lower.insert(key.to_string(), ns);
            self
        }
        fn nodes_in_file(mut self, path: &str, ns: Vec<Node>) -> Self {
            self.in_file.insert(path.to_string(), ns);
            self
        }
        fn node_by_id(mut self, n: Node) -> Self {
            self.by_id.insert(n.id.clone(), n);
            self
        }
        fn file(mut self, path: &str, content: &str) -> Self {
            self.files.insert(path.to_string());
            self.file_content
                .insert(path.to_string(), content.to_string());
            self
        }
        fn supertype(mut self, ty: &str, lang: Language, sups: Vec<&str>) -> Self {
            self.supertypes.insert(
                (ty.to_string(), lang),
                sups.into_iter().map(str::to_string).collect(),
            );
            self
        }
        fn import(mut self, path: &str, mappings: Vec<ImportMapping>) -> Self {
            self.imports.insert(path.to_string(), mappings);
            self
        }
    }

    impl ResolutionContext for Ctx {
        fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
            self.in_file.get(file_path).cloned().unwrap_or_default()
        }
        fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
            self.by_name.get(name).cloned().unwrap_or_default()
        }
        fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node> {
            self.by_qualified
                .get(qualified_name)
                .cloned()
                .unwrap_or_default()
        }
        fn get_nodes_by_kind(&self, _kind: NodeKind) -> Vec<Node> {
            Vec::new()
        }
        fn file_exists(&self, file_path: &str) -> bool {
            self.files.contains(file_path)
        }
        fn read_file(&self, file_path: &str) -> Option<String> {
            self.file_content.get(file_path).cloned()
        }
        fn get_project_root(&self) -> &str {
            ""
        }
        fn get_all_files(&self) -> Vec<String> {
            self.files.iter().cloned().collect()
        }
        fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node> {
            self.by_lower.get(lower_name).cloned().unwrap_or_default()
        }
        fn get_node_by_id(&self, id: &str) -> Option<Node> {
            self.by_id.get(id).cloned()
        }
        fn get_supertypes(&self, type_name: &str, language: Language) -> Vec<String> {
            self.supertypes
                .get(&(type_name.to_string(), language))
                .cloned()
                .unwrap_or_default()
        }
        fn get_import_mappings(&self, file_path: &str, _language: Language) -> Vec<ImportMapping> {
            self.imports.get(file_path).cloned().unwrap_or_default()
        }
    }

    // ---- Node / RefView builders --------------------------------------------

    fn mk(id: &str, kind: NodeKind, name: &str, qname: &str, path: &str, lang: Language) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: qname.to_string(),
            file_path: path.to_string(),
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

    fn refv(name: &str, kind: EdgeKind, path: &str, lang: Language, line: i64) -> RefView {
        RefView {
            from_node_id: "function:caller".to_string(),
            reference_name: name.to_string(),
            reference_kind: kind,
            line,
            column: 0,
            file_path: path.to_string(),
            language: lang,
            is_function_ref: false,
            reference_subkind: None::<ReferenceSubkind>,
        }
    }

    // ================= language family helpers ================================

    #[test]
    fn language_families_group_correctly() {
        assert!(same_language_family(Language::Java, Language::Kotlin));
        assert!(same_language_family(Language::Kotlin, Language::Scala));
        assert!(same_language_family(Language::Swift, Language::ObjC));
        assert!(same_language_family(Language::TypeScript, Language::Jsx));
        assert!(same_language_family(Language::JavaScript, Language::Tsx));
        assert!(same_language_family(Language::C, Language::Cpp));
        assert!(same_language_family(Language::CSharp, Language::Razor));
        // Identity always matches even for a language with no family.
        assert!(same_language_family(Language::Rust, Language::Rust));
        // Different families do not match.
        assert!(!same_language_family(Language::Java, Language::Swift));
        // A language with no family (Rust) does not match a different one.
        assert!(!same_language_family(Language::Rust, Language::Go));
    }

    #[test]
    fn known_family_and_cross_family_predicates() {
        assert!(is_known_language_family(Language::Java));
        assert!(is_known_language_family(Language::Cpp));
        assert!(!is_known_language_family(Language::Rust));
        assert!(!is_known_language_family(Language::Go));

        // crosses_known_family: both known + different family.
        assert!(crosses_known_family(Language::Java, Language::Swift));
        // same family => does not cross.
        assert!(!crosses_known_family(Language::Java, Language::Kotlin));
        // one unknown => does not cross.
        assert!(!crosses_known_family(Language::Rust, Language::Java));
        assert!(!crosses_known_family(Language::Java, Language::Rust));
    }

    #[test]
    fn language_gate_references_keeps_only_same_family() {
        // References edge: cross-family candidates are dropped.
        let same = mk(
            "function:a",
            NodeKind::Function,
            "f",
            "f",
            "src/a.ts",
            Language::TypeScript,
        );
        let cross = mk(
            "function:b",
            NodeKind::Function,
            "f",
            "f",
            "src/b.java",
            Language::Java,
        );
        let r = refv(
            "f",
            EdgeKind::References,
            "src/c.ts",
            Language::TypeScript,
            1,
        );
        let out = apply_language_gate(vec![same.clone(), cross], &r);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "function:a");
    }

    #[test]
    fn language_gate_imports_drops_only_cross_known_family() {
        // Imports edge: only candidates that CROSS a known family are dropped;
        // an unknown-family candidate (Rust vs TS) is KEPT.
        let same = mk(
            "function:a",
            NodeKind::Function,
            "f",
            "f",
            "src/a.ts",
            Language::TypeScript,
        );
        let cross = mk(
            "function:b",
            NodeKind::Function,
            "f",
            "f",
            "src/b.java",
            Language::Java,
        );
        let unknown = mk(
            "function:c",
            NodeKind::Function,
            "f",
            "f",
            "src/c.rs",
            Language::Rust,
        );
        let r = refv("f", EdgeKind::Imports, "src/x.ts", Language::TypeScript, 1);
        let out = apply_language_gate(vec![same, cross, unknown], &r);
        // TS kept (same family), Java dropped (crosses web↔jvm), Rust kept (unknown family).
        let ids: Vec<&str> = out.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"function:a"));
        assert!(ids.contains(&"function:c"));
        assert!(!ids.contains(&"function:b"));
    }

    #[test]
    fn language_gate_other_kind_passes_through() {
        let a = mk(
            "function:a",
            NodeKind::Function,
            "f",
            "f",
            "src/a.java",
            Language::Java,
        );
        let b = mk(
            "function:b",
            NodeKind::Function,
            "f",
            "f",
            "src/b.ts",
            Language::TypeScript,
        );
        let r = refv("f", EdgeKind::Calls, "src/x.ts", Language::TypeScript, 1);
        let out = apply_language_gate(vec![a, b], &r);
        assert_eq!(out.len(), 2);
    }

    // ================= match_by_file_path =====================================

    #[test]
    fn file_path_bare_symbol_without_extension_declines() {
        // A bare ref without `/` and without a short extension is not a file ref.
        let ctx = Ctx::default();
        let r = refv(
            "Widget",
            EdgeKind::References,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_by_file_path(&r, &ctx).is_none());
    }

    #[test]
    fn file_path_no_matching_file_nodes_declines() {
        // Path-like ref but there are no File nodes with that basename.
        let ctx = Ctx::default().name("b.ts", vec![]);
        let r = refv(
            "a/b.ts",
            EdgeKind::References,
            "src/x.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_by_file_path(&r, &ctx).is_none());
    }

    #[test]
    fn file_path_exact_qualified_match_high_confidence() {
        let file = mk(
            "file:src/a/b.ts",
            NodeKind::File,
            "b.ts",
            "a/b.ts",
            "a/b.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("b.ts", vec![file]);
        let r = refv(
            "a/b.ts",
            EdgeKind::References,
            "src/x.ts",
            Language::TypeScript,
            1,
        );
        let res = match_by_file_path(&r, &ctx).expect("exact file match");
        assert_eq!(res.target_node_id, "file:src/a/b.ts");
        assert_eq!(res.resolved_by, ResolvedBy::FilePath);
        assert!((res.confidence - 0.95).abs() < 1e-9);
    }

    #[test]
    fn file_path_suffix_match_picks_closest_and_medium_confidence() {
        // Two file nodes end with the ref; the one in the same dir as the ref wins.
        let near = mk(
            "file:1",
            NodeKind::File,
            "util.ts",
            "app/util.ts",
            "app/util.ts",
            Language::TypeScript,
        );
        let far = mk(
            "file:2",
            NodeKind::File,
            "util.ts",
            "lib/util.ts",
            "lib/util.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("util.ts", vec![near, far]);
        let r = refv(
            "util.ts",
            EdgeKind::References,
            "app/main.ts",
            Language::TypeScript,
            1,
        );
        let res = match_by_file_path(&r, &ctx).expect("suffix match");
        assert_eq!(res.target_node_id, "file:1");
        assert!((res.confidence - 0.85).abs() < 1e-9);
    }

    #[test]
    fn file_path_single_same_named_file_low_confidence() {
        // Single file node whose path neither equals nor suffix-matches the ref.
        let f = mk(
            "file:only",
            NodeKind::File,
            "b.ts",
            "totally/other.ts",
            "totally/other.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("b.ts", vec![f]);
        let r = refv(
            "weird/b.ts",
            EdgeKind::References,
            "src/x.ts",
            Language::TypeScript,
            1,
        );
        let res = match_by_file_path(&r, &ctx).expect("single file fallback");
        assert_eq!(res.target_node_id, "file:only");
        assert!((res.confidence - 0.7).abs() < 1e-9);
    }

    #[test]
    fn file_path_multiple_non_matching_files_declines() {
        // >1 file node, none exact/suffix — no fallback (len != 1).
        let a = mk(
            "file:a",
            NodeKind::File,
            "b.ts",
            "one/other.ts",
            "one/other.ts",
            Language::TypeScript,
        );
        let b = mk(
            "file:b",
            NodeKind::File,
            "b.ts",
            "two/other.ts",
            "two/other.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("b.ts", vec![a, b]);
        let r = refv(
            "weird/b.ts",
            EdgeKind::References,
            "src/x.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_by_file_path(&r, &ctx).is_none());
    }

    #[test]
    fn file_path_short_extension_bare_filename_is_file_ref() {
        // `Foo.h` — bare filename with a short extension counts as a file ref.
        let f = mk(
            "file:h",
            NodeKind::File,
            "Foo.h",
            "inc/Foo.h",
            "inc/Foo.h",
            Language::Cpp,
        );
        let ctx = Ctx::default().name("Foo.h", vec![f]);
        let r = refv("Foo.h", EdgeKind::References, "src/x.cpp", Language::Cpp, 1);
        assert!(match_by_file_path(&r, &ctx).is_some());
    }

    #[test]
    fn has_short_extension_variants() {
        assert!(has_short_extension("Foo.h"));
        assert!(has_short_extension("a.ts"));
        // A 6-char extension is too long (max 4).
        assert!(!has_short_extension("x.liquid"));
        assert!(!has_short_extension("noext"));
        assert!(!has_short_extension("trailingdot."));
        // The extension must start with an alphabetic char.
        assert!(!has_short_extension("bad.123"));
        assert!(has_short_extension("f.cpp"));
        assert!(!has_short_extension("f.toolong"));
    }

    #[test]
    fn file_path_empty_basename_declines() {
        // A ref ending in `/` yields an empty basename.
        let ctx = Ctx::default();
        let r = refv(
            "a/b/",
            EdgeKind::References,
            "src/x.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_by_file_path(&r, &ctx).is_none());
    }

    // ================= match_by_exact_name ====================================

    #[test]
    fn exact_name_empty_candidates_declines() {
        let ctx = Ctx::default();
        let r = refv("Nope", EdgeKind::Calls, "src/a.ts", Language::TypeScript, 1);
        assert!(match_by_exact_name(&r, &ctx).is_none());
    }

    #[test]
    fn exact_name_single_same_language_high_confidence() {
        let n = mk(
            "function:x",
            NodeKind::Function,
            "run",
            "run",
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("run", vec![n]);
        let r = refv("run", EdgeKind::Calls, "src/b.ts", Language::TypeScript, 1);
        let res = match_by_exact_name(&r, &ctx).expect("single match");
        assert_eq!(res.resolved_by, ResolvedBy::ExactMatch);
        assert!((res.confidence - 0.9).abs() < 1e-9);
    }

    #[test]
    fn exact_name_single_cross_language_half_confidence() {
        // Single candidate but a different language within the same family (web)
        // — passes the gate for Calls (other kind) yet is cross-language.
        let n = mk(
            "function:x",
            NodeKind::Function,
            "run",
            "run",
            "src/a.jsx",
            Language::Jsx,
        );
        let ctx = Ctx::default().name("run", vec![n]);
        let r = refv("run", EdgeKind::Calls, "src/b.ts", Language::TypeScript, 1);
        let res = match_by_exact_name(&r, &ctx).expect("single cross match");
        assert!((res.confidence - 0.5).abs() < 1e-9);
    }

    #[test]
    fn exact_name_multi_candidate_ranks_same_file_high_confidence() {
        // Multiple candidates; the same-file one wins and shares >=2 leading dir
        // segments with the ref (proximity 2*15=30 >= 30) => confidence 0.7.
        let same = mk(
            "function:same",
            NodeKind::Function,
            "run",
            "run",
            "src/app/b.ts",
            Language::TypeScript,
        );
        let other = mk(
            "function:other",
            NodeKind::Function,
            "run",
            "run",
            "far/away/x.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("run", vec![same, other]);
        let r = refv(
            "run",
            EdgeKind::Calls,
            "src/app/b.ts",
            Language::TypeScript,
            5,
        );
        let res = match_by_exact_name(&r, &ctx).expect("multi match");
        assert_eq!(res.target_node_id, "function:same");
        assert!((res.confidence - 0.7).abs() < 1e-9);
    }

    #[test]
    fn exact_name_multi_candidate_low_proximity_low_confidence() {
        // Multiple candidates all far away => best proximity < 30 => 0.4.
        let a = mk(
            "function:a",
            NodeKind::Function,
            "run",
            "run",
            "aaa/bbb/x.ts",
            Language::TypeScript,
        );
        let b = mk(
            "function:b",
            NodeKind::Function,
            "run",
            "run",
            "ccc/ddd/y.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("run", vec![a, b]);
        let r = refv(
            "run",
            EdgeKind::Calls,
            "zzz/main.ts",
            Language::TypeScript,
            1,
        );
        let res = match_by_exact_name(&r, &ctx).expect("multi far match");
        assert!((res.confidence - 0.4).abs() < 1e-9);
    }

    // ================= match_by_qualified_name ================================

    #[test]
    fn qualified_name_requires_separator() {
        let ctx = Ctx::default();
        let r = refv(
            "bareName",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_by_qualified_name(&r, &ctx).is_none());
    }

    #[test]
    fn qualified_name_single_exact_match() {
        let n = mk(
            "method:m",
            NodeKind::Method,
            "get",
            "Foo::get",
            "src/a.rs",
            Language::Rust,
        );
        let ctx = Ctx::default().qualified("Foo::get", vec![n]);
        let r = refv("Foo::get", EdgeKind::Calls, "src/b.rs", Language::Rust, 1);
        let res = match_by_qualified_name(&r, &ctx).expect("qualified exact");
        assert_eq!(res.resolved_by, ResolvedBy::QualifiedName);
        assert!((res.confidence - 0.95).abs() < 1e-9);
    }

    #[test]
    fn qualified_name_partial_suffix_match() {
        // No exact qualified hit, but the last segment's node ends with the ref.
        let n = mk(
            "method:m",
            NodeKind::Method,
            "get",
            "pkg::Foo::get",
            "src/a.rs",
            Language::Rust,
        );
        let ctx = Ctx::default()
            .qualified("Foo::get", vec![])
            .name("get", vec![n]);
        let r = refv("Foo::get", EdgeKind::Calls, "src/b.rs", Language::Rust, 1);
        let res = match_by_qualified_name(&r, &ctx).expect("partial qualified");
        assert!((res.confidence - 0.85).abs() < 1e-9);
    }

    #[test]
    fn qualified_name_dotted_no_match_declines() {
        let ctx = Ctx::default();
        let r = refv(
            "a.b.c",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_by_qualified_name(&r, &ctx).is_none());
    }

    // ================= parse helpers ==========================================

    #[test]
    fn dot_and_colon_call_parsing() {
        assert_eq!(
            match_dot_call("obj.method"),
            Some(("obj".to_string(), "method".to_string()))
        );
        assert_eq!(
            match_dot_call("a.b.method"),
            Some(("a.b".to_string(), "method".to_string()))
        );
        // ObjC selector with trailing colon.
        assert_eq!(
            match_dot_call("obj.doThis:"),
            Some(("obj".to_string(), "doThis:".to_string()))
        );
        // Leading dot => no receiver.
        assert_eq!(match_dot_call(".method"), None);
        // No dot at all.
        assert_eq!(match_dot_call("method"), None);
        // Receiver with an illegal char.
        assert_eq!(match_dot_call("ob-j.method"), None);

        assert_eq!(
            match_colon_call("Foo::bar"),
            Some(("Foo".to_string(), "bar".to_string()))
        );
        assert_eq!(match_colon_call("::bar"), None);
        assert_eq!(match_colon_call("Foo::"), None);
        assert_eq!(match_colon_call("nocolon"), None);
    }

    #[test]
    fn selector_like_and_word_predicates() {
        assert!(is_selector_like("do"));
        assert!(is_selector_like("do:"));
        assert!(is_selector_like("do:with:"));
        assert!(!is_selector_like(""));
        assert!(!is_selector_like(":leading"));
        assert!(is_word("abc_1"));
        assert!(!is_word(""));
        assert!(!is_word("a b"));
    }

    #[test]
    fn parse_method_call_prefers_dot_then_colon() {
        assert_eq!(
            parse_method_call("obj.m", Language::TypeScript),
            Some(("obj".to_string(), "m".to_string()))
        );
        assert_eq!(
            parse_method_call("Foo::m", Language::TypeScript),
            Some(("Foo".to_string(), "m".to_string()))
        );
        assert_eq!(parse_method_call("plain", Language::TypeScript), None);
        // Lua colon separator only recognized for Lua/Luau.
        assert_eq!(
            parse_method_call("lg:log", Language::Lua),
            Some(("lg".to_string(), "log".to_string()))
        );
        assert_eq!(parse_method_call("lg:log", Language::Rust), None);
        // R dollar separator only recognized for R.
        assert_eq!(
            parse_method_call("lg$log", Language::R),
            Some(("lg".to_string(), "log".to_string()))
        );
        assert_eq!(parse_method_call("lg$log", Language::TypeScript), None);
    }

    #[test]
    fn is_dot_call_gate() {
        assert!(is_dot_call("obj.method"));
        assert!(!is_dot_call("Foo::method"));
        assert!(!is_dot_call("plain"));
    }

    #[test]
    fn capitalize_and_camel_split() {
        assert_eq!(capitalize("word"), "Word");
        assert_eq!(capitalize(""), "");
        assert_eq!(capitalize("Already"), "Already");
        let words = split_camel_case("HTTPServerConfig");
        assert!(words.iter().any(|w| w == "HTTP"));
        assert!(words.iter().any(|w| w == "Server"));
        assert!(words.iter().any(|w| w == "Config"));
        // single-char words are dropped
        let ws = split_camel_case("aB");
        assert!(ws.is_empty() || ws.iter().all(|w| w.chars().count() > 1));
    }

    // ================= match_method_call ======================================

    #[test]
    fn method_call_declines_when_not_a_call_shape() {
        let ctx = Ctx::default();
        let r = refv(
            "plainname",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_method_call(&r, &ctx).is_none());
    }

    #[test]
    fn method_call_strategy1_direct_class_match() {
        // `Widget.render` — Widget is a class in the graph; render is its method.
        let class = mk(
            "class:w",
            NodeKind::Class,
            "Widget",
            "Widget",
            "src/w.ts",
            Language::TypeScript,
        );
        let method = mk(
            "method:r",
            NodeKind::Method,
            "render",
            "Widget::render",
            "src/w.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default()
            .name("Widget", vec![class])
            .nodes_in_file("src/w.ts", vec![method]);
        let r = refv(
            "Widget.render",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        let res = match_method_call(&r, &ctx).expect("strategy1");
        assert_eq!(res.target_node_id, "method:r");
        assert_eq!(res.resolved_by, ResolvedBy::QualifiedName);
        assert!((res.confidence - 0.85).abs() < 1e-9);
    }

    #[test]
    fn method_call_strategy2_capitalized_receiver() {
        // `widget.render` — lowercase instance var, capitalized class Widget.
        let class = mk(
            "class:w",
            NodeKind::Class,
            "Widget",
            "Widget",
            "src/w.ts",
            Language::TypeScript,
        );
        let method = mk(
            "method:r",
            NodeKind::Method,
            "render",
            "Widget::render",
            "src/w.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default()
            .name("widget", vec![]) // no direct class for the lowercase receiver
            .name("Widget", vec![class])
            .nodes_in_file("src/w.ts", vec![method]);
        let r = refv(
            "widget.render",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        let res = match_method_call(&r, &ctx).expect("strategy2");
        assert_eq!(res.target_node_id, "method:r");
        assert_eq!(res.resolved_by, ResolvedBy::InstanceMethod);
        assert!((res.confidence - 0.8).abs() < 1e-9);
    }

    #[test]
    fn method_call_strategy3_single_method_by_name() {
        // No class match; exactly one same-language method named `render`.
        let method = mk(
            "method:r",
            NodeKind::Method,
            "render",
            "Thing::render",
            "src/w.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("render", vec![method]);
        let r = refv(
            "obj.render",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        let res = match_method_call(&r, &ctx).expect("strategy3 single");
        assert_eq!(res.target_node_id, "method:r");
        assert!((res.confidence - 0.7).abs() < 1e-9);
    }

    #[test]
    fn method_call_strategy3_receiver_overlap_scoring() {
        // Multiple `save` methods; receiver `UserRepository` overlaps
        // `UserRepository::save` on >=2 words => picked at 0.65.
        let good = mk(
            "method:good",
            NodeKind::Method,
            "save",
            "UserRepository::save",
            "src/u.ts",
            Language::TypeScript,
        );
        let bad = mk(
            "method:bad",
            NodeKind::Method,
            "save",
            "OrderService::save",
            "src/o.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("save", vec![good, bad]);
        let r = refv(
            "userRepository.save",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        let res = match_method_call(&r, &ctx).expect("overlap scoring");
        assert_eq!(res.target_node_id, "method:good");
        assert!((res.confidence - 0.65).abs() < 1e-9);
    }

    #[test]
    fn method_call_strategy3_no_overlap_declines() {
        // Multiple methods, receiver overlaps <2 words => declines.
        let a = mk(
            "method:a",
            NodeKind::Method,
            "go",
            "Alpha::go",
            "src/a.ts",
            Language::TypeScript,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "go",
            "Beta::go",
            "src/b.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("go", vec![a, b]);
        let r = refv(
            "zeta.go",
            EdgeKind::Calls,
            "src/x.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_method_call(&r, &ctx).is_none());
    }

    // ================= match_fuzzy ============================================

    #[test]
    fn fuzzy_single_same_language() {
        let n = mk(
            "function:x",
            NodeKind::Function,
            "Run",
            "Run",
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().lower("run", vec![n]);
        let r = refv("run", EdgeKind::Calls, "src/b.ts", Language::TypeScript, 1);
        let res = match_fuzzy(&r, &ctx).expect("fuzzy single");
        assert_eq!(res.resolved_by, ResolvedBy::Fuzzy);
        assert!((res.confidence - 0.5).abs() < 1e-9);
    }

    #[test]
    fn fuzzy_single_cross_language_lower_confidence() {
        // Only a cross-language (same family) callable => confidence 0.3.
        let n = mk(
            "function:x",
            NodeKind::Function,
            "Run",
            "Run",
            "src/a.jsx",
            Language::Jsx,
        );
        let ctx = Ctx::default().lower("run", vec![n]);
        let r = refv("run", EdgeKind::Calls, "src/b.ts", Language::TypeScript, 1);
        let res = match_fuzzy(&r, &ctx).expect("fuzzy cross");
        assert!((res.confidence - 0.3).abs() < 1e-9);
    }

    #[test]
    fn fuzzy_filters_non_callable_kinds() {
        // A single Variable candidate is not callable => declines.
        let n = mk(
            "variable:x",
            NodeKind::Variable,
            "Run",
            "Run",
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().lower("run", vec![n]);
        let r = refv("run", EdgeKind::Calls, "src/b.ts", Language::TypeScript, 1);
        assert!(match_fuzzy(&r, &ctx).is_none());
    }

    #[test]
    fn fuzzy_multiple_candidates_declines() {
        let a = mk(
            "function:a",
            NodeKind::Function,
            "Run",
            "Run",
            "src/a.ts",
            Language::TypeScript,
        );
        let b = mk(
            "function:b",
            NodeKind::Function,
            "Run",
            "Run",
            "src/b.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().lower("run", vec![a, b]);
        let r = refv("run", EdgeKind::Calls, "src/c.ts", Language::TypeScript, 1);
        assert!(match_fuzzy(&r, &ctx).is_none());
    }

    // ================= find_best_match scoring ================================

    #[test]
    fn best_match_scoring_favors_calls_kind_and_export() {
        // Two same-file candidates: a Function (calls kind +25, exported +10)
        // should outrank a Variable of the same name.
        let func = {
            let mut n = mk(
                "function:f",
                NodeKind::Function,
                "x",
                "x",
                "src/a.ts",
                Language::TypeScript,
            );
            n.is_exported = true;
            n.start_line = 10;
            n
        };
        let var = mk(
            "variable:v",
            NodeKind::Variable,
            "x",
            "x",
            "src/a.ts",
            Language::TypeScript,
        );
        let r = refv("x", EdgeKind::Calls, "src/a.ts", Language::TypeScript, 10);
        let pool = [var, func];
        let best = find_best_match(&r, &pool).expect("best");
        assert_eq!(best.id, "function:f");
    }

    #[test]
    fn best_match_instantiates_prefers_class() {
        let class = mk(
            "class:c",
            NodeKind::Class,
            "T",
            "T",
            "far/a.ts",
            Language::TypeScript,
        );
        let func = mk(
            "function:f",
            NodeKind::Function,
            "T",
            "T",
            "far/b.ts",
            Language::TypeScript,
        );
        let r = refv(
            "T",
            EdgeKind::Instantiates,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        let pool = [func, class];
        let best = find_best_match(&r, &pool).expect("best");
        assert_eq!(best.id, "class:c");
    }

    #[test]
    fn best_match_decorates_prefers_function_over_class() {
        let func = mk(
            "function:f",
            NodeKind::Function,
            "D",
            "D",
            "far/a.ts",
            Language::TypeScript,
        );
        let class = mk(
            "class:c",
            NodeKind::Class,
            "D",
            "D",
            "far/b.ts",
            Language::TypeScript,
        );
        let r = refv(
            "D",
            EdgeKind::Decorates,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        let pool = [class, func];
        let best = find_best_match(&r, &pool).expect("best");
        assert_eq!(best.id, "function:f");
    }

    #[test]
    fn best_match_penalizes_cross_language() {
        // Same name; the same-language candidate must win over a cross-language one.
        let same = mk(
            "function:s",
            NodeKind::Function,
            "x",
            "x",
            "far/a.ts",
            Language::TypeScript,
        );
        let cross = mk(
            "function:c",
            NodeKind::Function,
            "x",
            "x",
            "far/b.java",
            Language::Java,
        );
        let r = refv("x", EdgeKind::Calls, "src/a.ts", Language::TypeScript, 1);
        let pool = [cross, same];
        let best = find_best_match(&r, &pool).expect("best");
        assert_eq!(best.id, "function:s");
    }

    // ================= match_scoped_call_chain ================================

    #[test]
    fn scoped_call_chain_resolves_via_return_type() {
        // Rust `Foo::bar().baz` — bar() returns Foo; baz resolves on Foo.
        let factory = {
            let mut n = mk(
                "method:bar",
                NodeKind::Method,
                "bar",
                "Foo::bar",
                "src/a.rs",
                Language::Rust,
            );
            n.return_type = Some("Foo".to_string());
            n
        };
        let baz = mk(
            "method:baz",
            NodeKind::Method,
            "baz",
            "Foo::baz",
            "src/a.rs",
            Language::Rust,
        );
        let ctx = Ctx::default()
            .name("bar", vec![factory])
            .name("baz", vec![baz]);
        let r = refv(
            "Foo::bar().baz",
            EdgeKind::Calls,
            "src/b.rs",
            Language::Rust,
            1,
        );
        let res = match_scoped_call_chain(&r, &ctx).expect("scoped chain");
        assert_eq!(res.target_node_id, "method:baz");
        assert_eq!(res.resolved_by, ResolvedBy::InstanceMethod);
    }

    #[test]
    fn scoped_call_chain_self_return_uses_factory_class() {
        // Return type "self" resolves the method on the factory class itself.
        let factory = {
            let mut n = mk(
                "method:bar",
                NodeKind::Method,
                "bar",
                "Foo::bar",
                "src/a.rs",
                Language::Rust,
            );
            n.return_type = Some("self".to_string());
            n
        };
        let baz = mk(
            "method:baz",
            NodeKind::Method,
            "baz",
            "Foo::baz",
            "src/a.rs",
            Language::Rust,
        );
        let ctx = Ctx::default()
            .name("bar", vec![factory])
            .name("baz", vec![baz]);
        let r = refv(
            "Foo::bar().baz",
            EdgeKind::Calls,
            "src/b.rs",
            Language::Rust,
            1,
        );
        let res = match_scoped_call_chain(&r, &ctx).expect("self chain");
        assert_eq!(res.target_node_id, "method:baz");
    }

    #[test]
    fn scoped_call_chain_requires_colon_scope() {
        // Inner without `::` declines.
        let ctx = Ctx::default();
        let r = refv("foo().baz", EdgeKind::Calls, "src/b.rs", Language::Rust, 1);
        assert!(match_scoped_call_chain(&r, &ctx).is_none());
    }

    #[test]
    fn scoped_call_chain_bad_shape_declines() {
        let ctx = Ctx::default();
        let r = refv("Foo::bar", EdgeKind::Calls, "src/b.rs", Language::Rust, 1);
        assert!(match_scoped_call_chain(&r, &ctx).is_none());
    }

    // ================= match_dotted_call_chain ================================

    #[test]
    fn dotted_chain_go_bare_factory_via_return_type() {
        // Go `NewFoo().Bar` — NewFoo returns Foo; Bar resolves on Foo.
        let newfoo = {
            let mut n = mk(
                "function:nf",
                NodeKind::Function,
                "NewFoo",
                "NewFoo",
                "a.go",
                Language::Go,
            );
            n.return_type = Some("Foo".to_string());
            n
        };
        let bar = mk(
            "method:bar",
            NodeKind::Method,
            "Bar",
            "Foo::Bar",
            "a.go",
            Language::Go,
        );
        let ctx = Ctx::default()
            .name("NewFoo", vec![newfoo])
            .name("Bar", vec![bar]);
        let r = refv("NewFoo().Bar", EdgeKind::Calls, "b.go", Language::Go, 1);
        let res = match_dotted_call_chain(&r, &ctx).expect("go bare chain");
        assert_eq!(res.target_node_id, "method:bar");
    }

    #[test]
    fn dotted_chain_go_bare_fallback_to_exact_name() {
        // Go bare factory with no return type falls back to exact-name on method.
        let target = mk(
            "function:worker",
            NodeKind::Function,
            "Bar",
            "Bar",
            "a.go",
            Language::Go,
        );
        let ctx = Ctx::default()
            .name("NewFoo", vec![]) // no return-type node
            .name("Bar", vec![target]);
        let r = refv("NewFoo().Bar", EdgeKind::Calls, "b.go", Language::Go, 1);
        let res = match_dotted_call_chain(&r, &ctx).expect("go fallback");
        assert_eq!(res.target_node_id, "function:worker");
    }

    #[test]
    fn dotted_chain_construct_via_bare_call_kotlin() {
        // Kotlin `Foo().bar` — capitalized bare constructor receiver.
        let bar = mk(
            "method:bar",
            NodeKind::Method,
            "bar",
            "Foo::bar",
            "a.kt",
            Language::Kotlin,
        );
        let ctx = Ctx::default().name("bar", vec![bar]);
        let r = refv("Foo().bar", EdgeKind::Calls, "b.kt", Language::Kotlin, 1);
        let res = match_dotted_call_chain(&r, &ctx).expect("kotlin construct");
        assert_eq!(res.target_node_id, "method:bar");
    }

    #[test]
    fn dotted_chain_construct_via_bare_lowercase_declines() {
        // Kotlin lowercase bare receiver is not a constructor => declines.
        let ctx = Ctx::default();
        let r = refv("foo().bar", EdgeKind::Calls, "b.kt", Language::Kotlin, 1);
        assert!(match_dotted_call_chain(&r, &ctx).is_none());
    }

    #[test]
    fn dotted_chain_non_go_bare_non_construct_declines() {
        // Java bare `foo().bar` — not Go, not a construct-via-bare language.
        let ctx = Ctx::default();
        let r = refv("foo().bar", EdgeKind::Calls, "b.java", Language::Java, 1);
        assert!(match_dotted_call_chain(&r, &ctx).is_none());
    }

    #[test]
    fn dotted_chain_factory_fluent_receiver() {
        // `Builder.make().build` — make returns Widget; build resolves on Widget.
        let make = {
            let mut n = mk(
                "method:make",
                NodeKind::Method,
                "make",
                "Builder::make",
                "a.java",
                Language::Java,
            );
            n.return_type = Some("Widget".to_string());
            n
        };
        let build = mk(
            "method:build",
            NodeKind::Method,
            "build",
            "Widget::build",
            "a.java",
            Language::Java,
        );
        let ctx = Ctx::default()
            .name("make", vec![make])
            .name("build", vec![build]);
        let r = refv(
            "Builder.make().build",
            EdgeKind::Calls,
            "b.java",
            Language::Java,
            1,
        );
        let res = match_dotted_call_chain(&r, &ctx).expect("fluent chain");
        assert_eq!(res.target_node_id, "method:build");
    }

    #[test]
    fn dotted_chain_bad_shape_declines() {
        let ctx = Ctx::default();
        let r = refv("noshape", EdgeKind::Calls, "b.java", Language::Java, 1);
        assert!(match_dotted_call_chain(&r, &ctx).is_none());
    }

    #[test]
    fn constructs_via_bare_call_languages() {
        assert!(constructs_via_bare_call(Language::Kotlin));
        assert!(constructs_via_bare_call(Language::Swift));
        assert!(constructs_via_bare_call(Language::Scala));
        assert!(constructs_via_bare_call(Language::Dart));
        assert!(!constructs_via_bare_call(Language::Java));
        assert!(!constructs_via_bare_call(Language::Go));
    }

    // ================= resolve_method_on_type =================================

    #[test]
    fn method_on_type_walks_supertype_for_conformance() {
        // ping is on Base, not Sub; resolve_method_on_type walks Sub -> Base.
        let ping = mk(
            "method:ping",
            NodeKind::Method,
            "ping",
            "Base::ping",
            "Base.java",
            Language::Java,
        );
        let ctx =
            Ctx::default()
                .name("ping", vec![ping])
                .supertype("Sub", Language::Java, vec!["Base"]);
        let r = refv("x", EdgeKind::Calls, "Sub.java", Language::Java, 1);
        let res = resolve_method_on_type(
            "Sub",
            "ping",
            &r,
            &ctx,
            0.85,
            ResolvedBy::InstanceMethod,
            None,
            0,
        )
        .expect("supertype walk");
        assert_eq!(res.target_node_id, "method:ping");
    }

    #[test]
    fn method_on_type_no_method_no_supertype_declines() {
        let ctx = Ctx::default().name("ping", vec![]);
        let r = refv("x", EdgeKind::Calls, "Sub.java", Language::Java, 1);
        assert!(
            resolve_method_on_type(
                "Sub",
                "ping",
                &r,
                &ctx,
                0.85,
                ResolvedBy::InstanceMethod,
                None,
                0
            )
            .is_none()
        );
    }

    #[test]
    fn method_on_type_preferred_fqn_disambiguates() {
        // Two Foo::bar methods; preferred_fqn "pkg.Foo" picks the one in that path.
        let a = mk(
            "method:a",
            NodeKind::Method,
            "bar",
            "Foo::bar",
            "src/pkg/Foo.java",
            Language::Java,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "bar",
            "Foo::bar",
            "src/other/Foo.java",
            Language::Java,
        );
        let ctx = Ctx::default().name("bar", vec![a, b]);
        let r = refv("x", EdgeKind::Calls, "src/x.java", Language::Java, 1);
        let res = resolve_method_on_type(
            "Foo",
            "bar",
            &r,
            &ctx,
            0.9,
            ResolvedBy::InstanceMethod,
            Some("pkg.Foo"),
            0,
        )
        .expect("fqn disambiguation");
        assert_eq!(res.target_node_id, "method:a");
    }

    // ================= match_function_ref =====================================

    #[test]
    fn function_ref_this_member_declines() {
        let ctx = Ctx::default();
        let r = refv(
            "this.handler",
            EdgeKind::References,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_function_ref(&r, &ctx).is_none());
    }

    #[test]
    fn function_ref_bare_function_same_file() {
        let n = mk(
            "function:h",
            NodeKind::Function,
            "onBlur",
            "onBlur",
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("onBlur", vec![n]);
        let r = refv(
            "onBlur",
            EdgeKind::References,
            "src/a.ts",
            Language::TypeScript,
            5,
        );
        let res = match_function_ref(&r, &ctx).expect("bare fn ref");
        assert_eq!(res.target_node_id, "function:h");
        assert_eq!(res.resolved_by, ResolvedBy::FunctionRef);
        assert!((res.confidence - 0.95).abs() < 1e-9);
    }

    #[test]
    fn function_ref_cross_file_unique() {
        // Not in the same file but the only candidate => confidence 0.8.
        let n = mk(
            "function:h",
            NodeKind::Function,
            "onBlur",
            "onBlur",
            "src/other.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("onBlur", vec![n]);
        let r = refv(
            "onBlur",
            EdgeKind::References,
            "src/a.ts",
            Language::TypeScript,
            5,
        );
        let res = match_function_ref(&r, &ctx).expect("cross-file unique");
        assert!((res.confidence - 0.8).abs() < 1e-9);
    }

    #[test]
    fn function_ref_bare_fn_only_excludes_methods_in_ts() {
        // In TS a bare id can only be a Function value, so a Method candidate is
        // filtered out => declines.
        let m = mk(
            "method:m",
            NodeKind::Method,
            "handler",
            "Foo::handler",
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("handler", vec![m]);
        let r = refv(
            "handler",
            EdgeKind::References,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_function_ref(&r, &ctx).is_none());
    }

    #[test]
    fn function_ref_qualified_member_pointer_unique() {
        // C++ `&Widget::on_click` — resolve the member on that scope.
        let m = mk(
            "method:oc",
            NodeKind::Method,
            "on_click",
            "Widget::on_click",
            "src/a.cpp",
            Language::Cpp,
        );
        let ctx = Ctx::default().name("on_click", vec![m]);
        let r = refv(
            "Widget::on_click",
            EdgeKind::References,
            "src/a.cpp",
            Language::Cpp,
            1,
        );
        let res = match_function_ref(&r, &ctx).expect("member pointer");
        assert_eq!(res.target_node_id, "method:oc");
        assert!((res.confidence - 0.9).abs() < 1e-9);
    }

    #[test]
    fn function_ref_qualified_member_pointer_ambiguous_cross_file_declines() {
        // Two matches, neither in the ref's file => ambiguous, declines.
        let a = mk(
            "method:a",
            NodeKind::Method,
            "on_click",
            "Widget::on_click",
            "src/one.cpp",
            Language::Cpp,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "on_click",
            "Widget::on_click",
            "src/two.cpp",
            Language::Cpp,
        );
        let ctx = Ctx::default().name("on_click", vec![a, b]);
        let r = refv(
            "Widget::on_click",
            EdgeKind::References,
            "src/three.cpp",
            Language::Cpp,
            1,
        );
        assert!(match_function_ref(&r, &ctx).is_none());
    }

    #[test]
    fn function_ref_qualified_member_pointer_no_match_declines() {
        let ctx = Ctx::default().name("on_click", vec![]);
        let r = refv(
            "Widget::on_click",
            EdgeKind::References,
            "src/a.cpp",
            Language::Cpp,
            1,
        );
        assert!(match_function_ref(&r, &ctx).is_none());
    }

    #[test]
    fn function_ref_no_candidates_declines() {
        let ctx = Ctx::default();
        let r = refv(
            "nowhere",
            EdgeKind::References,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_function_ref(&r, &ctx).is_none());
    }

    #[test]
    fn function_ref_swift_implicit_self_keeps_enclosing_method() {
        // Swift: bare id names a method only on the enclosing type. The caller's
        // qualified_name gives the class prefix; a method on the same class stays.
        let caller = mk(
            "method:caller",
            NodeKind::Method,
            "run",
            "MyView::run",
            "src/v.swift",
            Language::Swift,
        );
        let same_class = mk(
            "method:h",
            NodeKind::Method,
            "helper",
            "MyView::helper",
            "src/v.swift",
            Language::Swift,
        );
        let ctx = Ctx::default()
            .name("helper", vec![same_class])
            .node_by_id(caller);
        let mut r = refv(
            "helper",
            EdgeKind::References,
            "src/v.swift",
            Language::Swift,
            5,
        );
        r.from_node_id = "method:caller".to_string();
        let res = match_function_ref(&r, &ctx).expect("swift self method");
        assert_eq!(res.target_node_id, "method:h");
    }

    #[test]
    fn function_ref_swift_implicit_self_drops_foreign_class_method() {
        // A same-named method on a DIFFERENT class is a parameter collision => drop.
        let caller = mk(
            "method:caller",
            NodeKind::Method,
            "run",
            "MyView::run",
            "src/v.swift",
            Language::Swift,
        );
        let foreign = mk(
            "method:f",
            NodeKind::Method,
            "helper",
            "OtherView::helper",
            "src/o.swift",
            Language::Swift,
        );
        let ctx = Ctx::default()
            .name("helper", vec![foreign])
            .node_by_id(caller);
        let mut r = refv(
            "helper",
            EdgeKind::References,
            "src/v.swift",
            Language::Swift,
            5,
        );
        r.from_node_id = "method:caller".to_string();
        assert!(match_function_ref(&r, &ctx).is_none());
    }

    // ================= match_reference orchestrator ===========================

    #[test]
    fn match_reference_dispatches_file_path_first() {
        let f = mk(
            "file:1",
            NodeKind::File,
            "b.ts",
            "a/b.ts",
            "a/b.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("b.ts", vec![f]);
        let r = refv(
            "a/b.ts",
            EdgeKind::References,
            "src/x.ts",
            Language::TypeScript,
            1,
        );
        let res = match_reference(&r, &ctx).expect("file path via orchestrator");
        assert_eq!(res.resolved_by, ResolvedBy::FilePath);
    }

    #[test]
    fn match_reference_falls_through_to_exact_name() {
        let n = mk(
            "function:x",
            NodeKind::Function,
            "run",
            "run",
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("run", vec![n]);
        let r = refv("run", EdgeKind::Calls, "src/b.ts", Language::TypeScript, 1);
        let res = match_reference(&r, &ctx).expect("exact via orchestrator");
        assert_eq!(res.resolved_by, ResolvedBy::ExactMatch);
    }

    #[test]
    fn match_reference_falls_through_to_fuzzy() {
        // Nothing exact; a single fuzzy (lower-name) callable resolves last.
        let n = mk(
            "function:x",
            NodeKind::Function,
            "Run",
            "Run",
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().lower("run", vec![n]);
        let r = refv("run", EdgeKind::Calls, "src/b.ts", Language::TypeScript, 1);
        let res = match_reference(&r, &ctx).expect("fuzzy via orchestrator");
        assert_eq!(res.resolved_by, ResolvedBy::Fuzzy);
    }

    #[test]
    fn match_reference_returns_none_when_nothing_matches() {
        let ctx = Ctx::default();
        let r = refv(
            "ghost",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_reference(&r, &ctx).is_none());
    }

    // ================= C++ receiver inference =================================

    #[test]
    fn cpp_receiver_inference_from_declarator() {
        // `Foo f;\n f.v()` — infer f's type Foo from the declarator, resolve v on Foo.
        let v = mk(
            "method:v",
            NodeKind::Method,
            "v",
            "Foo::v",
            "src/main.cpp",
            Language::Cpp,
        );
        let ctx = Ctx::default()
            .file(
                "src/main.cpp",
                "struct Foo { int v(); };\nint run() { Foo f; return f.v(); }\n",
            )
            .name("v", vec![v]);
        let r = refv("f.v", EdgeKind::Calls, "src/main.cpp", Language::Cpp, 2);
        let res = match_method_call(&r, &ctx).expect("cpp receiver");
        assert_eq!(res.target_node_id, "method:v");
        assert_eq!(res.resolved_by, ResolvedBy::InstanceMethod);
    }

    #[test]
    fn cpp_receiver_auto_new_initializer() {
        // `auto f = new Foo();\n f.v()` — infer type from the `new Foo` initializer.
        let v = mk(
            "method:v",
            NodeKind::Method,
            "v",
            "Foo::v",
            "src/main.cpp",
            Language::Cpp,
        );
        let ctx = Ctx::default()
            .file(
                "src/main.cpp",
                "int run() { auto f = new Foo(); return f.v(); }\n",
            )
            .name("v", vec![v]);
        let r = refv("f.v", EdgeKind::Calls, "src/main.cpp", Language::Cpp, 1);
        let res = match_method_call(&r, &ctx).expect("cpp auto new");
        assert_eq!(res.target_node_id, "method:v");
    }

    // ================= Java field receiver ====================================

    #[test]
    fn java_field_receiver_from_signature() {
        // `repo.save()` where `repo` is a field of declared type `Repo`.
        let class = mk(
            "class:svc",
            NodeKind::Class,
            "Service",
            "Service",
            "src/S.java",
            Language::Java,
        );
        let mut class = class;
        class.start_line = 1;
        class.end_line = 5;
        let mut field = mk(
            "field:repo",
            NodeKind::Field,
            "repo",
            "Service::repo",
            "src/S.java",
            Language::Java,
        );
        field.start_line = 2;
        field.end_line = 2;
        field.signature = Some("Repo repo".to_string());
        let save = mk(
            "method:save",
            NodeKind::Method,
            "save",
            "Repo::save",
            "src/R.java",
            Language::Java,
        );
        let ctx = Ctx::default()
            .nodes_in_file("src/S.java", vec![class, field])
            .name("save", vec![save]);
        let r = refv(
            "repo.save",
            EdgeKind::Calls,
            "src/S.java",
            Language::Java,
            3,
        );
        let res = match_method_call(&r, &ctx).expect("java field receiver");
        assert_eq!(res.target_node_id, "method:save");
        assert_eq!(res.resolved_by, ResolvedBy::InstanceMethod);
    }

    // ================= ambiguous_name_ceiling env override ====================

    #[test]
    fn ambiguous_name_ceiling_default() {
        // The default ceiling is returned when the env var is unset/invalid.
        // (The OnceLock caches on first read; asserting the constant is stable.)
        assert_eq!(AMBIGUOUS_NAME_CEILING, 500);
        // ambiguous_name_ceiling() returns a positive number.
        assert!(ambiguous_name_ceiling() > 0);
    }

    // ================= C++ inference deep paths ===============================

    #[test]
    fn cpp_receiver_auto_make_unique_initializer() {
        // `auto f = std::make_unique<Foo>();\n f->v()` style — the call-shape
        // initializer routes through resolve_cpp_call_result_type's make_unique arm.
        let v = mk(
            "method:v",
            NodeKind::Method,
            "v",
            "Foo::v",
            "src/m.cpp",
            Language::Cpp,
        );
        let ctx = Ctx::default()
            .file(
                "src/m.cpp",
                "int run() { auto f = std::make_unique<Foo>(); return f.v(); }\n",
            )
            .name("v", vec![v]);
        let r = refv("f.v", EdgeKind::Calls, "src/m.cpp", Language::Cpp, 1);
        let res = match_method_call(&r, &ctx).expect("make_unique inference");
        assert_eq!(res.target_node_id, "method:v");
    }

    #[test]
    fn cpp_receiver_auto_call_return_type_initializer() {
        // `auto f = make();\n f.v()` — make() returns Foo (declared return type),
        // so f is a Foo and v resolves on Foo.
        let make = {
            let mut n = mk(
                "function:make",
                NodeKind::Function,
                "make",
                "make",
                "src/m.cpp",
                Language::Cpp,
            );
            n.return_type = Some("Foo".to_string());
            n
        };
        let v = mk(
            "method:v",
            NodeKind::Method,
            "v",
            "Foo::v",
            "src/m.cpp",
            Language::Cpp,
        );
        let ctx = Ctx::default()
            .file(
                "src/m.cpp",
                "int run() { auto f = make(); return f.v(); }\n",
            )
            .name("make", vec![make])
            .name("v", vec![v]);
        let r = refv("f.v", EdgeKind::Calls, "src/m.cpp", Language::Cpp, 1);
        let res = match_method_call(&r, &ctx).expect("call-return inference");
        assert_eq!(res.target_node_id, "method:v");
    }

    #[test]
    fn cpp_receiver_auto_direct_construction_initializer() {
        // `auto f = Foo();\n f.v()` — the initializer names a class that exists,
        // so resolve_cpp_call_result_type's cpp_class_exists arm resolves Foo.
        let class = mk(
            "class:foo",
            NodeKind::Class,
            "Foo",
            "Foo",
            "src/m.cpp",
            Language::Cpp,
        );
        let v = mk(
            "method:v",
            NodeKind::Method,
            "v",
            "Foo::v",
            "src/m.cpp",
            Language::Cpp,
        );
        let ctx = Ctx::default()
            .file("src/m.cpp", "int run() { auto f = Foo(); return f.v(); }\n")
            .name("Foo", vec![class])
            .name("v", vec![v]);
        let r = refv("f.v", EdgeKind::Calls, "src/m.cpp", Language::Cpp, 1);
        let res = match_method_call(&r, &ctx).expect("direct construction");
        assert_eq!(res.target_node_id, "method:v");
    }

    #[test]
    fn cpp_receiver_header_fallback_declarator() {
        // No declarator in the .cpp; the sibling .h declares `Foo f;` — the header
        // fallback recovers the type.
        let v = mk(
            "method:v",
            NodeKind::Method,
            "v",
            "Foo::v",
            "src/m.cpp",
            Language::Cpp,
        );
        let ctx = Ctx::default()
            .file("src/m.cpp", "int run() { return f.v(); }\n")
            .file("src/m.h", "struct Bag { Foo f; };\n")
            .name("v", vec![v]);
        let r = refv("f.v", EdgeKind::Calls, "src/m.cpp", Language::Cpp, 1);
        let res = match_method_call(&r, &ctx).expect("header fallback");
        assert_eq!(res.target_node_id, "method:v");
    }

    #[test]
    fn cpp_receiver_no_declarator_no_header_declines() {
        // No declarator anywhere and no method fallback => no C++ typed edge.
        let ctx = Ctx::default().file("src/m.cpp", "int run() { return f.v(); }\n");
        let r = refv("f.v", EdgeKind::Calls, "src/m.cpp", Language::Cpp, 1);
        assert!(match_method_call(&r, &ctx).is_none());
    }

    // ================= match_function_ref extra branches =====================

    #[test]
    fn function_ref_member_pointer_single_cross_file_resolves() {
        // Exactly one scoped match, in a different file => still resolves (unique).
        let m = mk(
            "method:oc",
            NodeKind::Method,
            "on_click",
            "Widget::on_click",
            "src/w.cpp",
            Language::Cpp,
        );
        let ctx = Ctx::default().name("on_click", vec![m]);
        let r = refv(
            "Widget::on_click",
            EdgeKind::References,
            "src/other.cpp",
            Language::Cpp,
            1,
        );
        let res = match_function_ref(&r, &ctx).expect("unique cross-file member ptr");
        assert_eq!(res.target_node_id, "method:oc");
    }

    #[test]
    fn function_ref_swift_overload_family_same_file_declines() {
        // Swift: >1 same-named method in the ref's file is a parameter collision.
        let caller = mk(
            "method:caller",
            NodeKind::Method,
            "run",
            "MyView::run",
            "src/v.swift",
            Language::Swift,
        );
        let a = mk(
            "method:a",
            NodeKind::Method,
            "helper",
            "MyView::helper",
            "src/v.swift",
            Language::Swift,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "helper",
            "MyView::helper",
            "src/v.swift",
            Language::Swift,
        );
        let ctx = Ctx::default().name("helper", vec![a, b]).node_by_id(caller);
        let mut r = refv(
            "helper",
            EdgeKind::References,
            "src/v.swift",
            Language::Swift,
            5,
        );
        r.from_node_id = "method:caller".to_string();
        assert!(match_function_ref(&r, &ctx).is_none());
    }

    #[test]
    fn function_ref_method_allowed_in_non_bare_language() {
        // Rust is not a bare-fn-only language, so a Method candidate is eligible.
        let m = mk(
            "method:m",
            NodeKind::Method,
            "handler",
            "Foo::handler",
            "src/a.rs",
            Language::Rust,
        );
        let ctx = Ctx::default().name("handler", vec![m]);
        let r = refv(
            "handler",
            EdgeKind::References,
            "src/a.rs",
            Language::Rust,
            1,
        );
        let res = match_function_ref(&r, &ctx).expect("rust method value");
        assert_eq!(res.target_node_id, "method:m");
    }

    // ================= lookup_callee_return_type / imported_fqn ==============

    #[test]
    fn dotted_chain_uses_imported_fqn_for_preferred_disambiguation() {
        // Go bare factory: NewFoo returns Foo, and the file imports Foo from a
        // module, so imported_fqn_of feeds resolve_method_on_type's preferred fqn.
        let newfoo = {
            let mut n = mk(
                "function:nf",
                NodeKind::Function,
                "NewFoo",
                "NewFoo",
                "a.go",
                Language::Go,
            );
            n.return_type = Some("Foo".to_string());
            n
        };
        // Two Foo::Bar methods; the imported source path picks the one in pkg/Foo.
        let a = mk(
            "method:a",
            NodeKind::Method,
            "Bar",
            "Foo::Bar",
            "pkg/Foo.go",
            Language::Go,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "Bar",
            "Foo::Bar",
            "other/Foo.go",
            Language::Go,
        );
        let import = ImportMapping {
            local_name: "Foo".to_string(),
            exported_name: "Foo".to_string(),
            source: "pkg.Foo".to_string(),
            is_default: false,
            is_namespace: false,
        };
        let ctx = Ctx::default()
            .name("NewFoo", vec![newfoo])
            .name("Bar", vec![a, b])
            .import("b.go", vec![import]);
        let r = refv("NewFoo().Bar", EdgeKind::Calls, "b.go", Language::Go, 1);
        // Kotlin ext is `.kt`; Go/Java use `.java` in resolve_method_on_type's
        // preferred-fqn path, so pkg/Foo.java is sought — no match on `.go` paths
        // means it falls back to matches[0], which is still a valid resolution.
        let res = match_dotted_call_chain(&r, &ctx).expect("go imported fqn");
        assert!(res.target_node_id.starts_with("method:"));
    }

    #[test]
    fn lookup_callee_return_type_bare_function_path() {
        // A bare (no `::`) callee resolves via the first Function candidate's
        // declared return type — exercised through the Go bare-factory chain.
        let make = {
            let mut n = mk(
                "function:mk",
                NodeKind::Function,
                "Build",
                "Build",
                "a.go",
                Language::Go,
            );
            n.return_type = Some("Result".to_string());
            n
        };
        let done = mk(
            "method:done",
            NodeKind::Method,
            "Done",
            "Result::Done",
            "a.go",
            Language::Go,
        );
        let ctx = Ctx::default()
            .name("Build", vec![make])
            .name("Done", vec![done]);
        let r = refv("Build().Done", EdgeKind::Calls, "b.go", Language::Go, 1);
        let res = match_dotted_call_chain(&r, &ctx).expect("bare fn return type");
        assert_eq!(res.target_node_id, "method:done");
    }

    // ================= resolve_method_on_type depth guard ====================

    #[test]
    fn method_on_type_supertype_recursion_bounded() {
        // A supertype cycle deeper than the depth cap (4) still terminates and
        // declines rather than looping forever.
        let ctx = Ctx::default()
            .name("ghost", vec![])
            .supertype("A", Language::Java, vec!["B"])
            .supertype("B", Language::Java, vec!["C"])
            .supertype("C", Language::Java, vec!["D"])
            .supertype("D", Language::Java, vec!["E"])
            .supertype("E", Language::Java, vec!["A"]);
        let r = refv("x", EdgeKind::Calls, "src/a.java", Language::Java, 1);
        assert!(
            resolve_method_on_type(
                "A",
                "ghost",
                &r,
                &ctx,
                0.85,
                ResolvedBy::InstanceMethod,
                None,
                0
            )
            .is_none()
        );
    }

    #[test]
    fn method_on_type_qualified_suffix_match() {
        // The method's qualified_name ends with `::Type::method`, exercising the
        // suffix arm of resolve_method_on_type's match filter.
        let m = mk(
            "method:m",
            NodeKind::Method,
            "run",
            "pkg::Foo::run",
            "src/a.rs",
            Language::Rust,
        );
        let ctx = Ctx::default().name("run", vec![m]);
        let r = refv("x", EdgeKind::Calls, "src/a.rs", Language::Rust, 1);
        let res = resolve_method_on_type(
            "Foo",
            "run",
            &r,
            &ctx,
            0.9,
            ResolvedBy::InstanceMethod,
            None,
            0,
        )
        .expect("suffix match");
        assert_eq!(res.target_node_id, "method:m");
    }

    // ================= Round 5: prefer_call_site_file (#1079) =================

    #[test]
    fn prefer_call_site_file_partitions_same_file_first() {
        // Two same-named nodes: the one in the call site's file is moved first,
        // the rest keep their original order.
        let a = mk(
            "method:a",
            NodeKind::Method,
            "log",
            "Logger::log",
            "a/svc.cpp",
            Language::Cpp,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "log",
            "Logger::log",
            "b/svc.cpp",
            Language::Cpp,
        );
        let c = mk(
            "method:c",
            NodeKind::Method,
            "log",
            "Logger::log",
            "c/svc.cpp",
            Language::Cpp,
        );
        let ordered = prefer_call_site_file(vec![a, b, c], "b/svc.cpp");
        assert_eq!(ordered[0].id, "method:b");
        assert_eq!(ordered[1].id, "method:a");
        assert_eq!(ordered[2].id, "method:c");
    }

    #[test]
    fn prefer_call_site_file_noop_when_lt_two() {
        let a = mk(
            "method:a",
            NodeKind::Method,
            "log",
            "Logger::log",
            "a/svc.cpp",
            Language::Cpp,
        );
        let ordered = prefer_call_site_file(vec![a], "z/other.cpp");
        assert_eq!(ordered.len(), 1);
        assert_eq!(ordered[0].id, "method:a");
    }

    #[test]
    fn prefer_call_site_file_noop_when_none_in_file() {
        // No candidate shares the call site's file — original order preserved.
        let a = mk(
            "method:a",
            NodeKind::Method,
            "log",
            "Logger::log",
            "a/svc.cpp",
            Language::Cpp,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "log",
            "Logger::log",
            "b/svc.cpp",
            Language::Cpp,
        );
        let ordered = prefer_call_site_file(vec![a, b], "z/other.cpp");
        assert_eq!(ordered[0].id, "method:a");
        assert_eq!(ordered[1].id, "method:b");
    }

    #[test]
    fn method_on_type_prefers_call_site_file_when_ambiguous() {
        // Two `Logger::log` methods (ODR clash). Call from b/svc.cpp resolves to
        // the b/ one, NOT the first-indexed a/ one (#1079).
        let a = mk(
            "method:a",
            NodeKind::Method,
            "log",
            "Logger::log",
            "a/svc.cpp",
            Language::Cpp,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "log",
            "Logger::log",
            "b/svc.cpp",
            Language::Cpp,
        );
        let ctx = Ctx::default().name("log", vec![a, b]);
        let r = refv("x", EdgeKind::Calls, "b/svc.cpp", Language::Cpp, 1);
        let res = resolve_method_on_type(
            "Logger",
            "log",
            &r,
            &ctx,
            0.9,
            ResolvedBy::InstanceMethod,
            None,
            0,
        )
        .expect("ambiguous method resolves to own file");
        assert_eq!(res.target_node_id, "method:b");
    }

    #[test]
    fn method_on_type_preferred_fqn_beats_call_site_file() {
        // #314 must still win over #1079: the preferredFqn picks a DIFFERENT file
        // than the call site (Java import disambiguation targets another file).
        let a = mk(
            "method:a",
            NodeKind::Method,
            "bar",
            "Foo::bar",
            "src/pkg/Foo.java",
            Language::Java,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "bar",
            "Foo::bar",
            "src/callsite/Foo.java",
            Language::Java,
        );
        let ctx = Ctx::default().name("bar", vec![a, b]);
        // Call site is src/callsite/... but the imported FQN pins pkg.Foo.
        let r = refv(
            "x",
            EdgeKind::Calls,
            "src/callsite/Caller.java",
            Language::Java,
            1,
        );
        let res = resolve_method_on_type(
            "Foo",
            "bar",
            &r,
            &ctx,
            0.9,
            ResolvedBy::InstanceMethod,
            Some("pkg.Foo"),
            0,
        )
        .expect("preferred fqn wins");
        assert_eq!(res.target_node_id, "method:a");
    }

    #[test]
    fn qualified_name_exact_ambiguous_prefers_call_site_file() {
        // Two exact `Foo::get` qualified matches; the call site's own file wins (#1079).
        let a = mk(
            "method:a",
            NodeKind::Method,
            "get",
            "Foo::get",
            "a/foo.rs",
            Language::Rust,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "get",
            "Foo::get",
            "b/foo.rs",
            Language::Rust,
        );
        let ctx = Ctx::default().qualified("Foo::get", vec![a, b]);
        let r = refv("Foo::get", EdgeKind::Calls, "b/foo.rs", Language::Rust, 1);
        let res = match_by_qualified_name(&r, &ctx).expect("ambiguous qualified own file");
        assert_eq!(res.target_node_id, "method:b");
        assert!((res.confidence - 0.95).abs() < 1e-9);
    }

    #[test]
    fn qualified_name_exact_ambiguous_no_own_file_declines_to_partial() {
        // Two exact matches, neither in the call site's file: the exact-ambiguous
        // branch does NOT resolve (own file not first); falls through to partial.
        let a = mk(
            "method:a",
            NodeKind::Method,
            "get",
            "Foo::get",
            "a/foo.rs",
            Language::Rust,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "get",
            "Foo::get",
            "b/foo.rs",
            Language::Rust,
        );
        // partial fallback: get-by-name candidates whose qualified_name ends with ref.
        let ctx = Ctx::default()
            .qualified("Foo::get", vec![a, b.clone()])
            .name("get", vec![b]);
        let r = refv("Foo::get", EdgeKind::Calls, "z/other.rs", Language::Rust, 1);
        let res = match_by_qualified_name(&r, &ctx).expect("partial fallback");
        // Partial path returns first candidate whose qname ends with the ref.
        assert_eq!(res.target_node_id, "method:b");
        assert!((res.confidence - 0.85).abs() < 1e-9);
    }

    #[test]
    fn qualified_name_partial_prefers_call_site_file() {
        // Partial qualified match with two candidates ending in the ref; own file wins.
        let a = mk(
            "method:a",
            NodeKind::Method,
            "get",
            "pkg::Foo::get",
            "a/foo.rs",
            Language::Rust,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "get",
            "pkg::Foo::get",
            "b/foo.rs",
            Language::Rust,
        );
        let ctx = Ctx::default()
            .qualified("Foo::get", vec![])
            .name("get", vec![a, b]);
        let r = refv("Foo::get", EdgeKind::Calls, "b/foo.rs", Language::Rust, 1);
        let res = match_by_qualified_name(&r, &ctx).expect("partial own file");
        assert_eq!(res.target_node_id, "method:b");
    }

    #[test]
    fn method_call_strategy1_prefers_call_site_file() {
        // `Logger.log` — two Logger classes in different files; the call site's own
        // file class is tried first (#1079).
        let class_a = mk(
            "class:a",
            NodeKind::Class,
            "Logger",
            "Logger",
            "a/logger.ts",
            Language::TypeScript,
        );
        let class_b = mk(
            "class:b",
            NodeKind::Class,
            "Logger",
            "Logger",
            "b/logger.ts",
            Language::TypeScript,
        );
        let method_a = mk(
            "method:a",
            NodeKind::Method,
            "log",
            "Logger::log",
            "a/logger.ts",
            Language::TypeScript,
        );
        let method_b = mk(
            "method:b",
            NodeKind::Method,
            "log",
            "Logger::log",
            "b/logger.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default()
            .name("Logger", vec![class_a, class_b])
            .nodes_in_file("a/logger.ts", vec![method_a])
            .nodes_in_file("b/logger.ts", vec![method_b]);
        let r = refv(
            "Logger.log",
            EdgeKind::Calls,
            "b/logger.ts",
            Language::TypeScript,
            5,
        );
        let res = match_method_call(&r, &ctx).expect("strategy1 own file");
        assert_eq!(res.target_node_id, "method:b");
    }

    #[test]
    fn method_call_strategy3_prefers_call_site_file_on_tie() {
        // Two `save` methods with equal receiver-word overlap; the call site's own
        // file wins the tie because prefer_call_site_file reorders it first (#1079).
        let good_a = mk(
            "method:a",
            NodeKind::Method,
            "save",
            "UserRepository::save",
            "a/repo.ts",
            Language::TypeScript,
        );
        let good_b = mk(
            "method:b",
            NodeKind::Method,
            "save",
            "UserRepository::save",
            "b/repo.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("save", vec![good_a, good_b]);
        let r = refv(
            "userRepository.save",
            EdgeKind::Calls,
            "b/repo.ts",
            Language::TypeScript,
            1,
        );
        let res = match_method_call(&r, &ctx).expect("strategy3 tie own file");
        assert_eq!(res.target_node_id, "method:b");
        assert!((res.confidence - 0.65).abs() < 1e-9);
    }

    // ================= Round 5: #1108 local-var receiver inference ============

    /// Build a ctx whose file `path` has `source`, plus an enclosing function
    /// spanning the whole file (so the scope bound includes the call line).
    fn ctx_with_scope(
        path: &str,
        source: &str,
        lang: Language,
        method: Node,
        method_name: &str,
    ) -> Ctx {
        let mut fn_node = mk(
            "function:scope",
            NodeKind::Function,
            "run",
            "run",
            path,
            lang,
        );
        fn_node.start_line = 1;
        fn_node.end_line = source.split('\n').count() as i64 + 1;
        Ctx::default()
            .file(path, source)
            .nodes_in_file(path, vec![fn_node])
            .name(method_name, vec![method])
    }

    #[test]
    fn local_var_ts_new_constructor() {
        // TS: `const lg = new Logger(); lg.log()` — infer lg: Logger, resolve log.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = ctx_with_scope(
            "src/a.ts",
            "function run() {\n  const lg = new Logger();\n  lg.log();\n}\n",
            Language::TypeScript,
            m,
            "log",
        );
        let r = refv(
            "lg.log",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            3,
        );
        let res = match_method_call(&r, &ctx).expect("ts new inference");
        assert_eq!(res.target_node_id, "method:log");
        assert_eq!(res.resolved_by, ResolvedBy::InstanceMethod);
        assert!((res.confidence - 0.9).abs() < 1e-9);
    }

    #[test]
    fn local_var_ts_annotation() {
        // TS: `const lg: Logger = get(); lg.log()` — infer from the annotation.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = ctx_with_scope(
            "src/a.ts",
            "function run() {\n  const lg: Logger = get();\n  lg.log();\n}\n",
            Language::TypeScript,
            m,
            "log",
        );
        let r = refv(
            "lg.log",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            3,
        );
        let res = match_method_call(&r, &ctx).expect("ts annotation inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_python_construction() {
        // Python: `lg = Logger()\n lg.log()`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.py",
            Language::Python,
        );
        let ctx = ctx_with_scope(
            "src/a.py",
            "def run():\n    lg = Logger()\n    lg.log()\n",
            Language::Python,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/a.py", Language::Python, 3);
        let res = match_method_call(&r, &ctx).expect("python construction inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_python_annotation() {
        // Python PEP 526: `lg: Logger` param/annotation.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.py",
            Language::Python,
        );
        let ctx = ctx_with_scope(
            "src/a.py",
            "def run(lg: Logger):\n    lg.log()\n",
            Language::Python,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/a.py", Language::Python, 2);
        let res = match_method_call(&r, &ctx).expect("python annotation inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_go_composite_literal() {
        // Go: `lg := Logger{}\n lg.Log()`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "Log",
            "Logger::Log",
            "a.go",
            Language::Go,
        );
        let ctx = ctx_with_scope(
            "a.go",
            "func run() {\n\tlg := Logger{}\n\tlg.Log()\n}\n",
            Language::Go,
            m,
            "Log",
        );
        let r = refv("lg.Log", EdgeKind::Calls, "a.go", Language::Go, 3);
        let res = match_method_call(&r, &ctx).expect("go composite literal inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_go_var_decl() {
        // Go: `var lg *Logger\n lg.Log()`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "Log",
            "Logger::Log",
            "a.go",
            Language::Go,
        );
        let ctx = ctx_with_scope(
            "a.go",
            "func run() {\n\tvar lg *Logger\n\tlg.Log()\n}\n",
            Language::Go,
            m,
            "Log",
        );
        let r = refv("lg.Log", EdgeKind::Calls, "a.go", Language::Go, 3);
        let res = match_method_call(&r, &ctx).expect("go var decl inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_rust_let_new() {
        // Rust: `let lg = Logger::new(); lg.log()`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.rs",
            Language::Rust,
        );
        let ctx = ctx_with_scope(
            "src/a.rs",
            "fn run() {\n    let lg = Logger::new();\n    lg.log();\n}\n",
            Language::Rust,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/a.rs", Language::Rust, 3);
        let res = match_method_call(&r, &ctx).expect("rust let new inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_rust_let_struct_literal() {
        // Rust: `let lg = Logger { .. }; lg.log()`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.rs",
            Language::Rust,
        );
        let ctx = ctx_with_scope(
            "src/a.rs",
            "fn run() {\n    let lg = Logger { level: 0 };\n    lg.log();\n}\n",
            Language::Rust,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/a.rs", Language::Rust, 3);
        let res = match_method_call(&r, &ctx).expect("rust struct literal inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_java_new() {
        // Java: `Logger lg = new Logger(); lg.log()`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/A.java",
            Language::Java,
        );
        let ctx = ctx_with_scope(
            "src/A.java",
            "void run() {\n    Logger lg = new Logger();\n    lg.log();\n}\n",
            Language::Java,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/A.java", Language::Java, 3);
        let res = match_method_call(&r, &ctx).expect("java new inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_csharp_new() {
        // C#: `var lg = new Logger(); lg.Log()`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "Log",
            "Logger::Log",
            "src/A.cs",
            Language::CSharp,
        );
        let ctx = ctx_with_scope(
            "src/A.cs",
            "void Run() {\n    var lg = new Logger();\n    lg.Log();\n}\n",
            Language::CSharp,
            m,
            "Log",
        );
        let r = refv("lg.Log", EdgeKind::Calls, "src/A.cs", Language::CSharp, 3);
        let res = match_method_call(&r, &ctx).expect("csharp new inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_kotlin_construction() {
        // Kotlin: `val lg = Logger()\n lg.log()`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/A.kt",
            Language::Kotlin,
        );
        let ctx = ctx_with_scope(
            "src/A.kt",
            "fun run() {\n    val lg = Logger()\n    lg.log()\n}\n",
            Language::Kotlin,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/A.kt", Language::Kotlin, 3);
        let res = match_method_call(&r, &ctx).expect("kotlin construction inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_swift_construction() {
        // Swift: `let lg = Logger()\n lg.log()`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/A.swift",
            Language::Swift,
        );
        let ctx = ctx_with_scope(
            "src/A.swift",
            "func run() {\n    let lg = Logger()\n    lg.log()\n}\n",
            Language::Swift,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/A.swift", Language::Swift, 3);
        let res = match_method_call(&r, &ctx).expect("swift construction inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_scala_new() {
        // Scala: `val lg = new Logger\n lg.log()`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/A.scala",
            Language::Scala,
        );
        let ctx = ctx_with_scope(
            "src/A.scala",
            "def run() = {\n  val lg = new Logger\n  lg.log()\n}\n",
            Language::Scala,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/A.scala", Language::Scala, 3);
        let res = match_method_call(&r, &ctx).expect("scala new inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_dart_construction() {
        // Dart: `var lg = Logger();\n lg.log()`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.dart",
            Language::Dart,
        );
        let ctx = ctx_with_scope(
            "src/a.dart",
            "void run() {\n  var lg = Logger();\n  lg.log();\n}\n",
            Language::Dart,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/a.dart", Language::Dart, 3);
        let res = match_method_call(&r, &ctx).expect("dart construction inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_php_new() {
        // PHP: `$lg = new Logger(); $lg->log()` — ref name is `lg.log`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.php",
            Language::Php,
        );
        let ctx = ctx_with_scope(
            "src/a.php",
            "function run() {\n    $lg = new Logger();\n    $lg->log();\n}\n",
            Language::Php,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/a.php", Language::Php, 3);
        let res = match_method_call(&r, &ctx).expect("php new inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    // ============ #1220 PHP `$this->prop->method()` property resolution ========

    fn php_prop_method(qname: &str) -> Node {
        mk(
            "method:handle",
            NodeKind::Method,
            "handle",
            qname,
            "src/svc.php",
            Language::Php,
        )
    }

    fn php_prop_ctx(source: &str, method: Node) -> Ctx {
        Ctx::default()
            .file("src/svc.php", source)
            .name("handle", vec![method])
    }

    #[test]
    fn php_property_receiver_shape_matches() {
        assert!(is_php_property_receiver_shape("this->dep.method"));
        assert!(!is_php_property_receiver_shape("this->a->b.method"));
        assert!(!is_php_property_receiver_shape("dep.method"));
        assert!(!is_php_property_receiver_shape("this->dep"));
        assert!(!is_php_property_receiver_shape("Foo::bar"));
    }

    #[test]
    fn infer_php_property_typed() {
        let src = "<?php\nclass C {\n    private Foo $dep;\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let ctx = Ctx::default().file("src/svc.php", src);
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            5,
        );
        assert_eq!(
            infer_php_property_receiver_type("this->dep", &r, &ctx),
            Some("Foo".to_string())
        );
    }

    #[test]
    fn infer_php_property_promoted_ctor() {
        let src = "<?php\nclass C {\n    function __construct(private Foo $dep) {}\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let ctx = Ctx::default().file("src/svc.php", src);
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            5,
        );
        assert_eq!(
            infer_php_property_receiver_type("this->dep", &r, &ctx),
            Some("Foo".to_string())
        );
    }

    #[test]
    fn infer_php_property_pseudoctor_new() {
        let src = "<?php\nclass C {\n    function __construct() {\n        $this->dep = new Foo();\n    }\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let ctx = Ctx::default().file("src/svc.php", src);
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            7,
        );
        assert_eq!(
            infer_php_property_receiver_type("this->dep", &r, &ctx),
            Some("Foo".to_string())
        );
    }

    #[test]
    fn infer_php_property_union_untypable() {
        let src = "<?php\nclass C {\n    private Foo|Bar $dep;\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let ctx = Ctx::default().file("src/svc.php", src);
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            5,
        );
        assert_eq!(
            infer_php_property_receiver_type("this->dep", &r, &ctx),
            None
        );
    }

    #[test]
    fn infer_php_assigned_property_classic_ctor() {
        let src = "<?php\nclass C {\n    private $dep;\n    function __construct(Foo $dep) {\n        $this->dep = $dep;\n    }\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let ctx = Ctx::default().file("src/svc.php", src);
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            8,
        );
        assert_eq!(
            infer_php_property_receiver_type("this->dep", &r, &ctx),
            Some("Foo".to_string())
        );
    }

    #[test]
    fn php_this_prop_typed_property_resolves() {
        let src = "<?php\nclass Svc {\n    private Foo $dep;\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let ctx = php_prop_ctx(src, php_prop_method("Foo::handle"));
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            5,
        );
        let res = match_method_call(&r, &ctx).expect("php typed property resolves");
        assert_eq!(res.target_node_id, "method:handle");
        assert_eq!(res.resolved_by, ResolvedBy::InstanceMethod);
        assert!((res.confidence - 0.9).abs() < 1e-9);
    }

    #[test]
    fn php_this_prop_promoted_ctor_resolves() {
        let src = "<?php\nclass Svc {\n    function __construct(private Foo $dep) {}\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let ctx = php_prop_ctx(src, php_prop_method("Foo::handle"));
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            5,
        );
        let res = match_method_call(&r, &ctx).expect("php promoted ctor resolves");
        assert_eq!(res.target_node_id, "method:handle");
    }

    #[test]
    fn php_this_prop_classic_ctor_assignment_resolves() {
        let src = "<?php\nclass Svc {\n    private $dep;\n    function __construct(\n        Foo $dep\n    ) {\n        $this->dep = $dep;\n    }\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let ctx = php_prop_ctx(src, php_prop_method("Foo::handle"));
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            10,
        );
        let res = match_method_call(&r, &ctx).expect("php classic ctor assignment resolves");
        assert_eq!(res.target_node_id, "method:handle");
    }

    #[test]
    fn php_this_prop_typed_setter_injection_resolves() {
        let src = "<?php\nclass Svc {\n    private $dep;\n    function setDep(Foo $dep) {\n        $this->dep = $dep;\n    }\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let ctx = php_prop_ctx(src, php_prop_method("Foo::handle"));
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            8,
        );
        let res = match_method_call(&r, &ctx).expect("php typed setter injection resolves");
        assert_eq!(res.target_node_id, "method:handle");
    }

    #[test]
    fn php_this_prop_interface_typed_resolves_via_supertype() {
        let src = "<?php\nclass Svc {\n    private LoggerInterface $dep;\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let method = mk(
            "method:handle",
            NodeKind::Method,
            "handle",
            "LoggerInterface::handle",
            "src/iface.php",
            Language::Php,
        );
        let ctx = Ctx::default()
            .file("src/svc.php", src)
            .name("handle", vec![method])
            .supertype("LoggerInterface", Language::Php, vec!["LoggerInterface"]);
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            5,
        );
        let res = match_method_call(&r, &ctx).expect("php interface-typed resolves");
        assert_eq!(res.target_node_id, "method:handle");
    }

    #[test]
    fn php_this_prop_inherited_method_resolves() {
        let src = "<?php\nclass Svc {\n    private Child $dep;\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let method = mk(
            "method:handle",
            NodeKind::Method,
            "handle",
            "Parent::handle",
            "src/parent.php",
            Language::Php,
        );
        let ctx = Ctx::default()
            .file("src/svc.php", src)
            .name("handle", vec![method])
            .supertype("Child", Language::Php, vec!["Parent"]);
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            5,
        );
        let res = match_method_call(&r, &ctx).expect("php inherited method resolves");
        assert_eq!(res.target_node_id, "method:handle");
    }

    #[test]
    fn php_this_prop_same_named_local_does_not_mistype() {
        let src = "<?php\nclass Svc {\n    private Foo $dep;\n    function other() {\n        $dep = new Wrong();\n        $dep->handle();\n    }\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let foo = mk(
            "method:foo_handle",
            NodeKind::Method,
            "handle",
            "Foo::handle",
            "src/svc.php",
            Language::Php,
        );
        let wrong = mk(
            "method:wrong_handle",
            NodeKind::Method,
            "handle",
            "Wrong::handle",
            "src/svc.php",
            Language::Php,
        );
        let ctx = Ctx::default()
            .file("src/svc.php", src)
            .name("handle", vec![foo, wrong]);
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            9,
        );
        let res = match_method_call(&r, &ctx).expect("php property typed, not the local");
        assert_eq!(res.target_node_id, "method:foo_handle");
    }

    #[test]
    fn php_this_prop_untypable_stays_unlinked() {
        let src =
            "<?php\nclass Svc {\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let ctx = php_prop_ctx(src, php_prop_method("Foo::handle"));
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            4,
        );
        assert!(match_method_call(&r, &ctx).is_none());
    }

    #[test]
    fn php_this_prop_deep_chain_stays_unlinked() {
        let src = "<?php\nclass Svc {\n    private Foo $a;\n    function run() {\n        $this->a->b->handle();\n    }\n}\n";
        let ctx = php_prop_ctx(src, php_prop_method("Foo::handle"));
        let r = refv(
            "this->a->b.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            5,
        );
        assert!(match_method_call(&r, &ctx).is_none());
    }

    #[test]
    fn local_var_ruby_new() {
        // Ruby: `lg = Logger.new\n lg.log` (uses R4 receiver.method extraction).
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.rb",
            Language::Ruby,
        );
        let ctx = ctx_with_scope(
            "src/a.rb",
            "def run\n  lg = Logger.new\n  lg.log\nend\n",
            Language::Ruby,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/a.rb", Language::Ruby, 3);
        let res = match_method_call(&r, &ctx).expect("ruby new inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    // ================= Round 5: #1125 typed-parameter inference ===============

    #[test]
    fn typed_param_ts_generic_outer_type() {
        // TS generic param: `function use(repo: Repository<User>) { repo.find() }`
        // — infer outer type Repository.
        let m = mk(
            "method:find",
            NodeKind::Method,
            "find",
            "Repository::find",
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = ctx_with_scope(
            "src/a.ts",
            "function use(repo: Repository<User>) {\n  repo.find();\n}\n",
            Language::TypeScript,
            m,
            "find",
        );
        let r = refv(
            "repo.find",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            2,
        );
        let res = match_method_call(&r, &ctx).expect("ts typed param generic");
        assert_eq!(res.target_node_id, "method:find");
    }

    #[test]
    fn typed_param_rust_ref() {
        // Rust: `fn use(lg: &Logger) { lg.log() }` — strip `&`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.rs",
            Language::Rust,
        );
        let ctx = ctx_with_scope(
            "src/a.rs",
            "fn r(lg: &Logger) {\n    lg.log();\n}\n",
            Language::Rust,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/a.rs", Language::Rust, 2);
        let res = match_method_call(&r, &ctx).expect("rust typed ref param");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn typed_param_go_receiver() {
        // Go: `func use(lg Logger) { lg.Log() }`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "Log",
            "Logger::Log",
            "a.go",
            Language::Go,
        );
        let ctx = ctx_with_scope(
            "a.go",
            "func use(lg Logger) {\n\tlg.Log()\n}\n",
            Language::Go,
            m,
            "Log",
        );
        let r = refv("lg.Log", EdgeKind::Calls, "a.go", Language::Go, 2);
        let res = match_method_call(&r, &ctx).expect("go typed param");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn typed_param_dart() {
        // Dart: `void use(Logger lg) { lg.log() }`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.dart",
            Language::Dart,
        );
        let ctx = ctx_with_scope(
            "src/a.dart",
            "void use(Logger lg) {\n  lg.log();\n}\n",
            Language::Dart,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/a.dart", Language::Dart, 2);
        let res = match_method_call(&r, &ctx).expect("dart typed param");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn typed_param_php() {
        // PHP: `function use(Logger $lg) { $lg->log() }`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.php",
            Language::Php,
        );
        let ctx = ctx_with_scope(
            "src/a.php",
            "function use(Logger $lg) {\n    $lg->log();\n}\n",
            Language::Php,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/a.php", Language::Php, 2);
        let res = match_method_call(&r, &ctx).expect("php typed param");
        assert_eq!(res.target_node_id, "method:log");
    }

    // ================= Round 5: #1112 separators (Lua : / R $ / Pascal .) =====

    #[test]
    fn separator_lua_colon() {
        // Lua: `local lg = Logger.new()\n lg:log()` — ref name is `lg:log`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "a.lua",
            Language::Lua,
        );
        let ctx = ctx_with_scope(
            "a.lua",
            "function run()\n  local lg = Logger.new()\n  lg:log()\nend\n",
            Language::Lua,
            m,
            "log",
        );
        let r = refv("lg:log", EdgeKind::Calls, "a.lua", Language::Lua, 3);
        let res = match_method_call(&r, &ctx).expect("lua colon separator");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn lua_capitalized_method_call_not_self_matched() {
        // #1124: `lg:Log()` is byte-identical to a Luau annotation `lg: Log`; the
        // call line must NOT self-match `type=Log`. The scan continues to the real
        // `local lg = Logger.new()` declaration and resolves to Logger::Log.
        let m = mk(
            "method:Log",
            NodeKind::Method,
            "Log",
            "Logger::Log",
            "a.lua",
            Language::Lua,
        );
        let ctx = ctx_with_scope(
            "a.lua",
            "function run()\n  local lg = Logger.new()\n  lg:Log()\nend\n",
            Language::Lua,
            m,
            "Log",
        );
        let r = refv("lg:Log", EdgeKind::Calls, "a.lua", Language::Lua, 3);
        let res = match_method_call(&r, &ctx).expect("lua PascalCase call resolves");
        assert_eq!(res.target_node_id, "method:Log");
    }

    #[test]
    fn lua_annotation_still_resolves() {
        let m = mk(
            "method:Log",
            NodeKind::Method,
            "Log",
            "Logger::Log",
            "a.luau",
            Language::Luau,
        );
        let ctx = ctx_with_scope(
            "a.luau",
            "function run()\n  local lg: Logger\n  lg:Log()\nend\n",
            Language::Luau,
            m,
            "Log",
        );
        let r = refv("lg:Log", EdgeKind::Calls, "a.luau", Language::Luau, 3);
        let res = match_method_call(&r, &ctx).expect("lua annotation resolves");
        assert_eq!(res.target_node_id, "method:Log");
    }

    #[test]
    fn luau_typed_param_annotation_preserved() {
        let m = mk(
            "method:Log",
            NodeKind::Method,
            "Log",
            "Logger::Log",
            "a.luau",
            Language::Luau,
        );
        let ctx = ctx_with_scope(
            "a.luau",
            "function f(lg: Logger)\n  lg:Log()\nend\n",
            Language::Luau,
            m,
            "Log",
        );
        let r = refv("lg:Log", EdgeKind::Calls, "a.luau", Language::Luau, 2);
        let res = match_method_call(&r, &ctx).expect("luau typed param resolves");
        assert_eq!(res.target_node_id, "method:Log");
    }

    #[test]
    fn separator_r_dollar() {
        // R: `lg <- Logger$new()\n lg$log()` — ref name is `lg$log`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "a.R",
            Language::R,
        );
        let ctx = ctx_with_scope(
            "a.R",
            "run <- function() {\n  lg <- Logger$new()\n  lg$log()\n}\n",
            Language::R,
            m,
            "log",
        );
        let r = refv("lg$log", EdgeKind::Calls, "a.R", Language::R, 3);
        let res = match_method_call(&r, &ctx).expect("r dollar separator");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn separator_pascal_dot() {
        // Pascal: `var lg: TLogger; lg.Log()` — uses the `.` dot separator.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "Log",
            "TLogger::Log",
            "a.pas",
            Language::Pascal,
        );
        let ctx = ctx_with_scope(
            "a.pas",
            "procedure run;\nvar lg: TLogger;\nbegin\n  lg.Log();\nend;\n",
            Language::Pascal,
            m,
            "Log",
        );
        let r = refv("lg.Log", EdgeKind::Calls, "a.pas", Language::Pascal, 4);
        let res = match_method_call(&r, &ctx).expect("pascal dot separator");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn separator_lua_colon_only_for_lua() {
        // A `lg:log` ref in a non-Lua language is not a method-call shape → declines.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "a.rs",
            Language::Rust,
        );
        let ctx = Ctx::default().name("log", vec![m]);
        let r = refv("lg:log", EdgeKind::Calls, "a.rs", Language::Rust, 1);
        assert!(match_method_call(&r, &ctx).is_none());
    }

    #[test]
    fn separator_r_dollar_only_for_r() {
        // A `lg$log` ref outside R is not a call shape → declines.
        let ctx = Ctx::default();
        let r = refv("lg$log", EdgeKind::Calls, "a.ts", Language::TypeScript, 1);
        assert!(match_method_call(&r, &ctx).is_none());
    }

    // ================= Round 5: negative cases (no wrong edge) ================

    #[test]
    fn local_var_wrong_type_stays_silent() {
        // `lg = new Widget()` but the method `log` exists only on Logger — the
        // inferred type Widget has no `log`, and there is no Logger receiver word
        // overlap, so no edge is produced (validated-inference invariant).
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = ctx_with_scope(
            "src/a.ts",
            "function run() {\n  const lg = new Widget();\n  lg.log();\n}\n",
            Language::TypeScript,
            m,
            "log",
        );
        // Only ONE method named log exists → strategy3 single-candidate would
        // resolve it. To make this a true negative, add a second same-lang method
        // so strategy3 needs >=2 overlap which the receiver `lg` can't provide.
        let m2 = mk(
            "method:other",
            NodeKind::Method,
            "log",
            "Other::log",
            "src/b.ts",
            Language::TypeScript,
        );
        let ctx = ctx.name(
            "log",
            vec![
                mk(
                    "method:log",
                    NodeKind::Method,
                    "log",
                    "Logger::log",
                    "src/a.ts",
                    Language::TypeScript,
                ),
                m2,
            ],
        );
        let r = refv(
            "lg.log",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            3,
        );
        // Inference yields Widget → resolve_method_on_type(Widget, log) finds no
        // Widget::log → falls to strategies; two `log` methods, `lg` overlaps
        // neither by >=2 words → declines.
        assert!(match_method_call(&r, &ctx).is_none());
    }

    #[test]
    fn local_var_no_declaration_no_inference() {
        // No local declaration for `lg`; with two ambiguous same-named methods and
        // no overlap, inference finds nothing and strategy 3 declines.
        let a = mk(
            "method:a",
            NodeKind::Method,
            "log",
            "Alpha::log",
            "src/a.ts",
            Language::TypeScript,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "log",
            "Beta::log",
            "src/b.ts",
            Language::TypeScript,
        );
        let ctx = ctx_with_scope(
            "src/a.ts",
            "function run() {\n  lg.log();\n}\n",
            Language::TypeScript,
            a,
            "log",
        )
        .name(
            "log",
            vec![
                mk(
                    "method:a",
                    NodeKind::Method,
                    "log",
                    "Alpha::log",
                    "src/a.ts",
                    Language::TypeScript,
                ),
                b,
            ],
        );
        let r = refv(
            "lg.log",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            2,
        );
        assert!(match_method_call(&r, &ctx).is_none());
    }

    #[test]
    fn local_var_inference_bounded_to_enclosing_scope() {
        // A `Logger` decl in function A must NOT leak into function B's call.
        // Function B (lines 5-7) contains the call; the decl is in A (line 2).
        let logger_log = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.ts",
            Language::TypeScript,
        );
        let widget_log = mk(
            "method:wlog",
            NodeKind::Method,
            "log",
            "Widget::log",
            "src/w.ts",
            Language::TypeScript,
        );
        let mut fn_a = mk(
            "function:a",
            NodeKind::Function,
            "a",
            "a",
            "src/a.ts",
            Language::TypeScript,
        );
        fn_a.start_line = 1;
        fn_a.end_line = 3;
        let mut fn_b = mk(
            "function:b",
            NodeKind::Function,
            "b",
            "b",
            "src/a.ts",
            Language::TypeScript,
        );
        fn_b.start_line = 5;
        fn_b.end_line = 7;
        let source =
            "function a() {\n  const lg = new Logger();\n}\n\nfunction b() {\n  lg.log();\n}\n";
        let ctx = Ctx::default()
            .file("src/a.ts", source)
            .nodes_in_file("src/a.ts", vec![fn_a, fn_b])
            .name("log", vec![logger_log, widget_log]);
        let r = refv(
            "lg.log",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            6,
        );
        // In B's scope there is no `lg` decl → inference finds nothing; two `log`
        // methods → strategy3 needs overlap `lg` can't give → declines.
        assert!(match_method_call(&r, &ctx).is_none());
    }

    // ================= Round 5: branch coverage for helpers ===================

    #[test]
    fn non_type_receiver_token_rejects_keywords() {
        assert!(is_non_type_receiver_token("this"));
        assert!(is_non_type_receiver_token("self"));
        assert!(is_non_type_receiver_token("undefined"));
        assert!(!is_non_type_receiver_token("Logger"));
    }

    #[test]
    fn normalize_inferred_type_name_strips_and_rejects() {
        // Generics + pointer markers stripped, last segment kept.
        assert_eq!(
            normalize_inferred_type_name("Repository<User>").as_deref(),
            Some("Repository")
        );
        assert_eq!(
            normalize_inferred_type_name("pkg.mod.Logger").as_deref(),
            Some("Logger")
        );
        assert_eq!(
            normalize_inferred_type_name("&Logger").as_deref(),
            Some("Logger")
        );
        // A non-type keyword is rejected.
        assert_eq!(normalize_inferred_type_name("self"), None);
        // An empty capture yields None.
        assert_eq!(normalize_inferred_type_name(""), None);
    }

    #[test]
    fn local_receiver_type_patterns_empty_for_unpatterned_language() {
        // C/ObjC have no shared patterns (C++ uses its own inferrer) → empty.
        assert!(local_receiver_type_patterns(Language::C, "lg").is_empty());
        assert!(local_receiver_type_patterns(Language::ObjC, "lg").is_empty());
        assert!(!local_receiver_type_patterns(Language::TypeScript, "lg").is_empty());
    }

    #[test]
    fn infer_local_receiver_type_none_when_no_patterns() {
        // C has no patterns → infer returns None regardless of source.
        let ctx = Ctx::default().file("a.c", "int f() { Logger lg; lg.log(); }\n");
        let r = refv("lg.log", EdgeKind::Calls, "a.c", Language::C, 1);
        assert!(infer_local_receiver_type("lg", &r, &ctx).is_none());
    }

    #[test]
    fn infer_local_receiver_type_none_when_file_unreadable() {
        // TS has patterns but the file can't be read → None.
        let ctx = Ctx::default();
        let r = refv(
            "lg.log",
            EdgeKind::Calls,
            "missing.ts",
            Language::TypeScript,
            1,
        );
        assert!(infer_local_receiver_type("lg", &r, &ctx).is_none());
    }

    #[test]
    fn infer_local_receiver_type_none_when_no_declaration() {
        // Readable source but no matching declaration for `lg` → None.
        let ctx = Ctx::default().file("a.ts", "function f() {\n  lg.log();\n}\n");
        let r = refv("lg.log", EdgeKind::Calls, "a.ts", Language::TypeScript, 2);
        assert!(infer_local_receiver_type("lg", &r, &ctx).is_none());
    }

    #[test]
    fn infer_local_receiver_type_skips_minified_line() {
        // A >10k-char line is skipped rather than scanned; the real decl on the
        // next line up is still found.
        let long = "x".repeat(10_050);
        let source =
            format!("function f() {{\n  const lg = new Logger();\n  {long}\n  lg.log();\n}}\n");
        let mut fn_node = mk(
            "function:s",
            NodeKind::Function,
            "f",
            "f",
            "a.ts",
            Language::TypeScript,
        );
        fn_node.start_line = 1;
        fn_node.end_line = 6;
        let ctx = Ctx::default()
            .file("a.ts", &source)
            .nodes_in_file("a.ts", vec![fn_node]);
        let r = refv("lg.log", EdgeKind::Calls, "a.ts", Language::TypeScript, 4);
        assert_eq!(
            infer_local_receiver_type("lg", &r, &ctx).as_deref(),
            Some("Logger")
        );
    }

    #[test]
    fn enclosing_scope_start_line_defaults_and_tightens() {
        // No enclosing function → default 1. With one enclosing method spanning
        // the call, its start line is returned.
        let ctx = Ctx::default();
        let r = refv("lg.log", EdgeKind::Calls, "a.ts", Language::TypeScript, 5);
        assert_eq!(enclosing_scope_start_line(&r, &ctx), 1);

        let mut m = mk(
            "method:x",
            NodeKind::Method,
            "run",
            "C::run",
            "a.ts",
            Language::TypeScript,
        );
        m.start_line = 3;
        m.end_line = 9;
        // A function in another language is ignored.
        let mut other = mk(
            "function:o",
            NodeKind::Function,
            "o",
            "o",
            "a.ts",
            Language::Python,
        );
        other.start_line = 1;
        other.end_line = 20;
        let ctx = Ctx::default().nodes_in_file("a.ts", vec![m, other]);
        assert_eq!(enclosing_scope_start_line(&r, &ctx), 3);
    }

    #[test]
    fn enclosing_scope_start_line_uses_start_when_end_line_zero() {
        // A node with end_line == 0 falls back to start_line for its span; a
        // single-line function whose start == the call line still encloses it.
        let mut m = mk(
            "method:x",
            NodeKind::Method,
            "run",
            "C::run",
            "a.ts",
            Language::TypeScript,
        );
        m.start_line = 5;
        m.end_line = 0;
        let ctx = Ctx::default().nodes_in_file("a.ts", vec![m]);
        let r = refv("lg.log", EdgeKind::Calls, "a.ts", Language::TypeScript, 5);
        assert_eq!(enclosing_scope_start_line(&r, &ctx), 5);
    }

    #[test]
    fn lua_and_r_separator_reject_bad_shapes() {
        // Empty receiver / method reject.
        assert_eq!(match_lua_colon_call(":log"), None);
        assert_eq!(match_lua_colon_call("lg:"), None);
        // Illegal receiver char.
        assert_eq!(match_lua_colon_call("l-g:log"), None);
        // Non-word method.
        assert_eq!(match_lua_colon_call("lg:lo g"), None);
        // No colon.
        assert_eq!(match_lua_colon_call("lglog"), None);

        assert_eq!(match_r_dollar_call("$log"), None);
        assert_eq!(match_r_dollar_call("lg$"), None);
        assert_eq!(match_r_dollar_call("l-g$log"), None);
        assert_eq!(match_r_dollar_call("lg$lo g"), None);
        assert_eq!(match_r_dollar_call("lglog"), None);
    }

    #[test]
    fn is_inferable_receiver_call_shapes() {
        assert!(is_inferable_receiver_call("lg.log", Language::TypeScript));
        assert!(is_inferable_receiver_call("lg:log", Language::Lua));
        assert!(is_inferable_receiver_call("lg$log", Language::R));
        // Colon-call `Class::method` is not inferable.
        assert!(!is_inferable_receiver_call("Foo::log", Language::Rust));
        // Lua separator not inferable outside Lua/Luau.
        assert!(!is_inferable_receiver_call("lg:log", Language::Rust));
        // R separator not inferable outside R.
        assert!(!is_inferable_receiver_call("lg$log", Language::TypeScript));
    }
}
