//! A Dart call through an import prefix (upstream #2389) — `http.get(…)`
//! after `import 'package:http/http.dart' as http;`, `fmt.describe(…)`,
//! `kit.Widget()` — calls a top-level declaration a library imported with
//! that prefix exports, `export` chains and `show` / `hide` followed, or
//! nothing of the project: never a member found by its name. riverpod's
//! docs' `http.get(…)` calls went to a docs example's `Http::get`, by their
//! receiver's capitalized name. A parameter, local or member named like the
//! prefix hides it, as Dart's scopes nest.
//!
//! KEEP-RUST: a Dart reference is placed where its chain starts, so a
//! prefixed call's column is on the prefix (upstream's is past the name).

use std::sync::Arc;

use codegraph_core::types::{EdgeKind, Language, Node};

use crate::dart_libraries::{dart_import_prefixes, dart_prefix_sees, is_dart_library_decl};
use crate::dart_scope::{in_order, is_dart_name_char, is_dart_type_kind, is_js_space};
use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// What a Dart call written `prefix.name(…)` through an import prefix links:
/// `Some` with the one top-level declaration the prefixed imports bring in
/// under the name, or nothing; `None` when the receiver is no import prefix
/// there, or a parameter, local or member of that name hides it (upstream
/// `matchDartPrefixedCall`).
pub(crate) fn match_dart_prefixed_call(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<Option<ResolvedRef>> {
    if reference.language != Language::Dart || reference.reference_kind != EdgeKind::Calls {
        return None;
    }
    let (prefix, name) = crate::dart_members::member_read_parts(&reference.reference_name)?;
    if !dart_import_prefixes(&reference.file_path, context).contains(prefix) {
        return None;
    }
    let Some(at) = prefixed_call_site(reference, prefix, name, context) else {
        return Some(None);
    };
    if is_dart_name_hidden(prefix, &reference.file_path, reference.line, at, context) {
        return None;
    }
    let found = dart_prefixed_decls(prefix, name, &reference.file_path, context);
    Some((found.len() == 1).then(|| ResolvedRef {
        original: reference.clone(),
        target_node_id: found[0].id.clone(),
        confidence: 0.9,
        resolved_by: ResolvedBy::Import,
    }))
}

/// The top-level Dart declarations named `name` that the imports written
/// with `prefix` bring into `file`'s library (upstream `dartPrefixedDecls`).
pub(crate) fn dart_prefixed_decls(
    prefix: &str,
    name: &str,
    file: &str,
    context: &dyn ResolutionContext,
) -> Vec<Arc<Node>> {
    context
        .get_nodes_by_name_shared(name)
        .into_iter()
        .filter(|node| {
            is_dart_library_decl(node)
                && dart_prefix_sees(file, prefix, &node.file_path, name, context)
        })
        .collect()
}

/// Where `prefix` starts in the code of a Dart call written `prefix.name(…)`
/// at the reference's place, or `None` when the call is written otherwise
/// (upstream `dartPrefixedCallSite`, which reads back from the name).
fn prefixed_call_site(
    reference: &RefView,
    prefix: &str,
    name: &str,
    context: &dyn ResolutionContext,
) -> Option<usize> {
    let code = context.source_facts(&reference.file_path)?.dart_code();
    let text = code.text.as_str();
    let at = code.line_start(reference.line)? + usize::try_from(reference.column).ok()?;
    let rest = text.get(at..)?.strip_prefix(prefix)?;
    let rest = rest.trim_start_matches(is_js_space).strip_prefix('.')?;
    let rest = rest.trim_start_matches(is_js_space).strip_prefix(name)?;
    if rest.starts_with(is_dart_name_char) {
        return None;
    }
    // `x.p.name(…)` is a member chain, whatever `p` names.
    (!text[..at].trim_end_matches(is_js_space).ends_with('.')).then_some(at)
}

/// Whether a parameter or local around offset `at` of `file`, or a member of
/// the type around line `line`, is named `name`: it hides a library-level
/// name there, an import prefix among them, as Dart's scopes nest (upstream
/// `isDartNameHidden`).
pub(crate) fn is_dart_name_hidden(
    name: &str,
    file: &str,
    line: i64,
    at: usize,
    context: &dyn ResolutionContext,
) -> bool {
    if crate::dart_local_scope::is_dart_locally_bound(file, name, at, context) {
        return true;
    }
    let mut own: Option<Arc<Node>> = None;
    for node in in_order(context.get_nodes_in_file_shared(file)) {
        if is_dart_type_kind(node.kind)
            && node.start_line <= line
            && node.end_line >= line
            && own
                .as_ref()
                .is_none_or(|own| node.start_line >= own.start_line)
        {
            own = Some(node);
        }
    }
    let Some(own) = own else {
        return false;
    };
    let qualified_name = format!("{}::{name}", own.qualified_name);
    crate::member_types::class_member_type(&own, name, context).is_some()
        || context
            .get_nodes_by_name_shared(name)
            .iter()
            .any(|node| node.file_path == own.file_path && node.qualified_name == qualified_name)
}
