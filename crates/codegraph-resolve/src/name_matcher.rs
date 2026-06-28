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

    // Partial qualified name match (name-matcher.ts:234-249).
    let parts: Vec<&str> = reference.reference_name.split([':', '.']).collect();
    if let Some(last_name) = parts.last().filter(|s| !s.is_empty()) {
        let partial = context.get_nodes_by_name(last_name);
        for candidate in partial {
            if candidate
                .qualified_name
                .ends_with(&reference.reference_name)
            {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: candidate.id,
                    confidence: 0.85,
                    resolved_by: ResolvedBy::QualifiedName,
                });
            }
        }
    }

    None
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

    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: matches[0].id.clone(),
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
            if enclosing.map_or(true, |e| n.start_line >= e.start_line) {
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

/// Try to resolve by method name on a class/object (`matchMethodCall`,
/// `name-matcher.ts:930-1133`).
///
/// C++ receiver-type inference (name-matcher.ts:953-968) and Java/Kotlin
/// field-receiver inference (name-matcher.ts:975-996) run first as typed-receiver
/// hooks; both validate the method via `resolve_method_on_type` so a wrong
/// inference yields no edge. Strategies 1-3 (direct class match, capitalized
/// receiver, receiver-overlap scoring) then cover the golden mini's instance-method
/// edges.
pub fn match_method_call(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let parsed = parse_method_call(&reference.reference_name)?;
    let (object_or_class, method_name) = parsed;

    // C++ receiver inference (name-matcher.ts:953-968): only for the dotted
    // `obj.method` shape (a `Class::method` colon-call is not an instance call).
    if reference.language == Language::Cpp && is_dot_call(&reference.reference_name) {
        if let Some(inferred) = infer_cpp_receiver_type(&object_or_class, reference, context, 0) {
            if let Some(typed) = resolve_method_on_type(
                &inferred,
                &method_name,
                reference,
                context,
                0.9,
                ResolvedBy::InstanceMethod,
                None,
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

    // Strategy 1: direct class name match (name-matcher.ts:825-850).
    for class_node in context.get_nodes_by_name(&object_or_class) {
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
    // (name-matcher.ts:852-880).
    let capitalized = capitalize(&object_or_class);
    if capitalized != object_or_class {
        for class_node in context.get_nodes_by_name(&capitalized) {
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
        let mut best_match: Option<&Node> = None;
        let mut best_score = 0i64;
        for method in &target_methods {
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

/// Parse `obj.method` or `Class::method` (name-matcher.ts:770-778).
fn parse_method_call(name: &str) -> Option<(String, String)> {
    if let Some(captures) = match_dot_call(name) {
        return Some(captures);
    }
    match_colon_call(name)
}

/// Did the ref match the dotted `obj.method` shape (the upstream `dotMatch` that
/// gates the C++/Java typed-receiver hooks, name-matcher.ts:953/975)?
fn is_dot_call(name: &str) -> bool {
    match_dot_call(name).is_some()
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
        let target =
            pool.iter()
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
        let target =
            same_file
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
