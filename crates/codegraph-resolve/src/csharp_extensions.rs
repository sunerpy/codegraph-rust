//! A C# call on a type the project does not declare reaches the project's
//! extension method for that type (KEEP-RUST, owner 2026-10-11, W3-05c).
//!
//! Upstream #2169 stops name guessing when a receiver's type is an outside
//! one, which also stopped Newtonsoft.Json's `objectType.IsEnum()` (a
//! `System.Type`) from reaching its own `TypeExtensions.IsEnum(this Type)`.
//! At that stop, a C# method is the target when it is the one candidate that
//! takes the receiver's type as its `this` parameter (written without a
//! namespace qualifier, and not one of the method's own type parameters), is
//! in scope at the call (its namespace is the caller's, an enclosing one, or
//! named by a `using` in scope there; see [`crate::csharp_scopes`]), and
//! takes the call's arguments. No candidate, or several, keeps the stop.

use std::sync::Arc;

use codegraph_core::types::{Language, Node, NodeKind};

use crate::overloads::{is_pack, paren_list_after, split_top_level};
use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// The project extension method a C# call `reference` on a receiver of the
/// outside type `type_name` reaches, if exactly one fits.
pub(crate) fn extension_method_target(
    type_name: &str,
    method_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if reference.language != Language::CSharp {
        return None;
    }
    let receiver_type = simple_type_name(type_name)?;
    let call_facts = context.source_facts(&reference.file_path)?;
    let call_offset = offset_of(&call_facts, reference.line, reference.column)?;
    let call_scopes = call_facts.csharp_scopes();
    let call_namespace = call_scopes.namespace_at(call_offset);
    let arguments = paren_list_after(
        &reference.file_path,
        reference.line,
        usize::try_from(reference.column).unwrap_or(0),
        method_name,
        context,
    )?;
    let argument_count = if arguments.trim().is_empty() {
        0
    } else {
        split_top_level(&arguments).len()
    };
    let fitting: Vec<Arc<Node>> = context
        .get_nodes_by_name_shared(method_name)
        .into_iter()
        .filter(|candidate| {
            candidate.kind == NodeKind::Method
                && candidate.language == Language::CSharp
                && extension_fits(candidate, &receiver_type, argument_count, context)
                && namespace_of(candidate, context).is_some_and(|namespace| {
                    namespace.is_empty()
                        || call_namespace == namespace
                        || call_namespace.starts_with(&format!("{namespace}."))
                        || call_scopes.imports_at(call_offset, &namespace)
                })
        })
        .collect();
    match fitting.as_slice() {
        [only] => Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: only.id.clone(),
            confidence: 0.9,
            resolved_by: ResolvedBy::InstanceMethod,
        }),
        _ => None,
    }
}

/// Whether `method` declares a `this` parameter of `receiver_type` and takes
/// `argument_count` arguments besides it.
fn extension_fits(
    method: &Node,
    receiver_type: &str,
    argument_count: usize,
    context: &dyn ResolutionContext,
) -> bool {
    let Some(list) = paren_list_after(
        &method.file_path,
        method.start_line,
        0,
        &method.name,
        context,
    ) else {
        return false;
    };
    let params = split_top_level(&list);
    let Some(this_type) = params.first().and_then(|first| this_parameter_type(first)) else {
        return false;
    };
    if this_type != receiver_type || is_own_type_parameter(method, &this_type, context) {
        return false;
    }
    let rest = &params[1..];
    let min = rest
        .iter()
        .filter(|p| !p.contains('=') && !is_pack(p))
        .count();
    let variadic = rest.iter().any(|p| is_pack(p));
    argument_count >= min && (variadic || argument_count <= rest.len())
}

/// The type of a `this` parameter (`this Type type`, `[NotNull] this
/// IList<T>? list`, `this in Span s`) as a simple name, or `None` for a
/// parameter without `this` or a type written with a namespace qualifier.
pub(crate) fn this_parameter_type(param: &str) -> Option<String> {
    let mut rest = after_this(param)?;
    for modifier in ["scoped", "ref", "in", "readonly"] {
        if let Some(after) = rest.strip_prefix(modifier)
            && after.starts_with(char::is_whitespace)
        {
            rest = after.trim_start();
        }
    }
    let declared = rest.split('=').next().unwrap_or(rest).trim();
    let bare = strip_type_arguments(declared);
    let (written, _name) = bare.trim().rsplit_once(char::is_whitespace)?;
    let written = written.trim();
    if written.contains('.') || written.contains("::") {
        return None;
    }
    simple_type_name(written)
}

/// Whether a declared parameter is a C# extension method's `this` one,
/// attributes before it allowed (`[NotNull] this PropertyInfo p`).
/// A `this` parameter's declaration without its name, whitespace runs
/// collapsed (`IList<T>`, `ref Span<byte>`), or `None` when `param` is no
/// `this` parameter. Two overloads in one class whose texts are equal extend
/// the same type.
pub(crate) fn this_parameter_text(param: &str) -> Option<String> {
    let (written, _name) = after_this(param)?.rsplit_once(char::is_whitespace)?;
    Some(written.split_whitespace().collect::<Vec<_>>().join(" "))
}

pub(crate) fn is_this_parameter(param: &str) -> bool {
    after_this(param).is_some()
}

/// What follows the `this` modifier of a parameter, past its attributes.
fn after_this(param: &str) -> Option<&str> {
    let mut rest = param.trim();
    while let Some(after) = rest.strip_prefix('[') {
        rest = after.split_once(']')?.1.trim_start();
    }
    let rest = rest.strip_prefix("this")?;
    rest.starts_with(char::is_whitespace)
        .then(|| rest.trim_start())
}

/// A type's simple name: type arguments, `?` and a trailing `[]` kept out
/// (`IList<Type>?` → `IList`); `None` for a qualified or empty name.
fn simple_type_name(written: &str) -> Option<String> {
    let bare = strip_type_arguments(written);
    let name = bare.trim().trim_end_matches('?').trim();
    let name = name.rsplit('.').next().unwrap_or(name);
    (!name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '[' || c == ']'))
    .then(|| name.to_string())
}

/// `text` without its `<…>` groups, nested ones with them.
fn strip_type_arguments(text: &str) -> String {
    let mut depth = 0usize;
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// Whether `name` is one of `method`'s own type parameters
/// (`static T Twice<T>(this T value)`).
fn is_own_type_parameter(method: &Node, name: &str, context: &dyn ResolutionContext) -> bool {
    let Some(facts) = context.source_facts(&method.file_path) else {
        return false;
    };
    let Some(start) = usize::try_from(method.start_line)
        .ok()
        .and_then(|line| line.checked_sub(1))
    else {
        return false;
    };
    let end = (start + 12).min(facts.line_count());
    if start >= end {
        return false;
    }
    let head = facts.join_lines(start, end, " ");
    let Some(at) = head.find(&format!("{}<", method.name)) else {
        return false;
    };
    let list = &head[at + method.name.len() + 1..];
    let list = list.split('>').next().unwrap_or_default();
    list.split(',').any(|param| param.trim() == name)
}

/// The namespace `method` is declared in, read from its file's source.
fn namespace_of(method: &Node, context: &dyn ResolutionContext) -> Option<String> {
    let facts = context.source_facts(&method.file_path)?;
    let offset = offset_of(&facts, method.start_line, method.start_column)?;
    Some(facts.csharp_scopes().namespace_at(offset))
}

/// The byte offset of 1-based `line`, `column` in a file's source.
fn offset_of(facts: &crate::source_facts::SourceFacts, line: i64, column: i64) -> Option<usize> {
    let index = usize::try_from(line).ok()?.checked_sub(1)?;
    let start = facts.lines().raw_line_start(index)?;
    Some(start + usize::try_from(column).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::{simple_type_name, this_parameter_type};

    #[test]
    fn a_this_parameter_names_its_type() {
        assert_eq!(
            this_parameter_type("this Type type").as_deref(),
            Some("Type")
        );
        assert_eq!(
            this_parameter_type("this ILGenerator generator").as_deref(),
            Some("ILGenerator")
        );
        assert_eq!(
            this_parameter_type("[NotNull] this IList<Type>? types").as_deref(),
            Some("IList")
        );
        assert_eq!(
            this_parameter_type("this in Span<byte> span").as_deref(),
            Some("Span")
        );
        assert_eq!(
            this_parameter_type("this Type type = null").as_deref(),
            Some("Type")
        );
        assert_eq!(this_parameter_type("Type type"), None);
        assert_eq!(this_parameter_type("thisType type"), None);
        assert_eq!(this_parameter_type("this System.Timers.Timer timer"), None);
        assert_eq!(this_parameter_type("this global::System.Type type"), None);
    }

    #[test]
    fn a_receiver_type_is_read_as_a_simple_name() {
        assert_eq!(simple_type_name("Type").as_deref(), Some("Type"));
        assert_eq!(simple_type_name("List<int>?").as_deref(), Some("List"));
        assert_eq!(simple_type_name("System.Type").as_deref(), Some("Type"));
        assert_eq!(simple_type_name(""), None);
    }
}
