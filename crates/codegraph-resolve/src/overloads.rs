//! A call reaches the overload its arguments fit (upstream #2212, #2213,
//! #2238, #2241 `retargetSelfOverload`).
//!
//! A method delegating to another overload of itself (`toInstant(instant)`
//! returning `toInstant(instant, null)`) bound to itself, and a call from
//! elsewhere to the first-indexed overload, since every overload shares the
//! name. The call's argument count is read at the call site and compared with
//! each same-owner overload's parameters, defaults and varargs / `params` /
//! `...` counted; Swift compares argument labels. Nothing is stored: the
//! argument list is read from the source at the reference's position.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use crate::types::{RefView, ResolutionContext, ResolvedRef};

/// Languages whose methods overload by arity (upstream `OVERLOADING_LANGUAGES`;
/// VB.NET joins with its extractor).
fn overloads_by_arity(language: Language) -> bool {
    matches!(
        language,
        Language::CSharp
            | Language::Java
            | Language::Kotlin
            | Language::Swift
            | Language::Cpp
            | Language::Scala
            | Language::Dart
            | Language::Solidity
    )
}

/// How many arguments a parameter list takes; `max: None` for a variadic one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Arity {
    min: usize,
    max: Option<usize>,
}

impl Arity {
    fn takes(self, count: usize) -> bool {
        count >= self.min && self.max.is_none_or(|max| count <= max)
    }
}

/// A method's same-owner overloads in its file and every one's arity.
#[derive(Debug)]
pub(crate) struct OverloadSet {
    name: String,
    own: Option<Arity>,
    siblings: Vec<(String, Option<Arity>)>,
}

/// Overload sets by method id, per resolution pass.
pub(crate) type OverloadSets = Mutex<HashMap<String, Option<Arc<OverloadSet>>>>;

/// `result` moved to the overload the call's arguments fit, or unchanged.
/// C++ overload sets (templates, SFINAE tags) are trusted only for a method's
/// call to itself.
pub(crate) fn retarget_overload(
    result: ResolvedRef,
    reference: &RefView,
    context: &dyn ResolutionContext,
    sets: &OverloadSets,
) -> ResolvedRef {
    if reference.reference_kind != EdgeKind::Calls || !overloads_by_arity(reference.language) {
        return result;
    }
    if reference.language == Language::Cpp && result.target_node_id != reference.from_node_id {
        return result;
    }
    let set = {
        let mut sets = sets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sets.entry(result.target_node_id.clone())
            .or_insert_with(|| overload_set_of(&result.target_node_id, context).map(Arc::new))
            .clone()
    };
    let Some(set) = set else {
        return result;
    };
    let Some(own) = set.own else {
        return result;
    };
    let Some(args) = paren_list_after(
        &reference.file_path,
        reference.line,
        usize::try_from(reference.column).unwrap_or(0),
        &set.name,
        context,
    ) else {
        return result;
    };
    let retarget = |id: &str| ResolvedRef {
        target_node_id: id.to_string(),
        ..result.clone()
    };
    if reference.language == Language::Swift {
        // Swift overloads by argument label as much as by count.
        let labels: Vec<String> = if args.trim().is_empty() {
            Vec::new()
        } else {
            split_top_level(&args)
                .iter()
                .map(|arg| argument_label(arg))
                .collect()
        };
        let fits_labels = |id: &str| -> Option<bool> {
            let decl = context.get_node_by_id_shared(id)?;
            let list = paren_list_after(&decl.file_path, decl.start_line, 0, &set.name, context)?;
            Some(swift_labels_fit(&labels, &list))
        };
        if fits_labels(&result.target_node_id) != Some(false) {
            return result;
        }
        let fit: Vec<&String> = set
            .siblings
            .iter()
            .map(|(id, _)| id)
            .filter(|id| fits_labels(id) == Some(true))
            .collect();
        return match fit.as_slice() {
            [only] => retarget(only),
            _ => result,
        };
    }
    let count = if args.trim().is_empty() {
        0
    } else {
        split_top_level(&args).len()
    };
    if own.takes(count) {
        return result;
    }
    let fits: Vec<&String> = set
        .siblings
        .iter()
        .filter(|(_, arity)| arity.is_some_and(|arity| arity.takes(count)))
        .map(|(id, _)| id)
        .collect();
    match fits.as_slice() {
        [only] => retarget(only),
        _ => result,
    }
}

/// The owner prefix of a qualified name: everything before its last `::`.
fn owner_of(qualified_name: &str) -> &str {
    qualified_name
        .rfind("::")
        .map_or("", |cut| &qualified_name[..cut])
}

/// A method's same-owner overloads, read from its declaration; `None` when it
/// has none.
fn overload_set_of(id: &str, context: &dyn ResolutionContext) -> Option<OverloadSet> {
    let own = context.get_node_by_id_shared(id)?;
    if !matches!(own.kind, NodeKind::Method | NodeKind::Function) {
        return None;
    }
    let owner = owner_of(&own.qualified_name);
    let siblings: Vec<Arc<Node>> = context
        .get_nodes_in_file_shared(&own.file_path)
        .into_iter()
        .filter(|n| {
            n.id != own.id
                && n.name == own.name
                && matches!(n.kind, NodeKind::Method | NodeKind::Function)
                && owner_of(&n.qualified_name) == owner
        })
        .collect();
    if siblings.is_empty() {
        return None;
    }
    let arity = |node: &Node| -> Option<Arity> {
        let list = paren_list_after(&node.file_path, node.start_line, 0, &own.name, context)?;
        let params: Vec<String> = split_top_level(&list)
            .into_iter()
            .filter(|p| !p.is_empty() && p != "void")
            .collect();
        let min = params
            .iter()
            .filter(|p| !p.contains('=') && !is_pack(p))
            .count();
        let max = (!params.iter().any(|p| is_pack(p))).then_some(params.len());
        Some(Arity { min, max })
    };
    Some(OverloadSet {
        name: own.name.clone(),
        own: arity(&own),
        siblings: siblings
            .iter()
            .map(|sibling| (sibling.id.clone(), arity(sibling)))
            .collect(),
    })
}

/// A variadic parameter: `...` (outside one level of `<…>`), C#'s `params`,
/// Kotlin's `vararg`.
fn is_pack(param: &str) -> bool {
    static ANGLES: OnceLock<Regex> = OnceLock::new();
    static KEYWORD: OnceLock<Regex> = OnceLock::new();
    let flat = ANGLES
        .get_or_init(|| Regex::new(r"<[^<>]*>").expect("angle group"))
        .replace_all(param, "");
    flat.contains("...")
        || KEYWORD
            .get_or_init(|| Regex::new(r"(?-u:\b)(?:params|vararg)\s").expect("variadic keyword"))
            .is_match(&flat)
}

/// A Swift argument's label (`numberOfRowsInSection: section`), or `_`.
fn argument_label(argument: &str) -> String {
    static LABEL: OnceLock<Regex> = OnceLock::new();
    let label = LABEL.get_or_init(|| Regex::new(r"^\s*([A-Za-z_]\w*)\s*:").expect("swift label"));
    match label.captures(argument) {
        Some(c) if !argument[c.get(0).map_or(0, |m| m.end())..].starts_with(':') => {
            c[1].to_string()
        }
        _ => "_".to_string(),
    }
}

/// Whether a Swift call's argument labels (`_` for none) fit a declaration's
/// parameter list: each parameter's external label in order, one with a
/// default value or a variadic one free to be left out (upstream
/// `swiftLabelsFit`).
fn swift_labels_fit(labels: &[String], param_list: &str) -> bool {
    static HEAD: OnceLock<Regex> = OnceLock::new();
    let head = HEAD.get_or_init(|| {
        Regex::new(
            r"^\s*(?:@\w+(?:\([^)]*\))?\s+)*(?:inout\s+)?([A-Za-z_]\w*)(?:\s+([A-Za-z_]\w*))?\s*:",
        )
        .expect("swift parameter head")
    });
    let params: Vec<(String, bool)> = if param_list.trim().is_empty() {
        Vec::new()
    } else {
        split_top_level(param_list)
            .iter()
            .map(|param| {
                let label = head
                    .captures(param)
                    .map_or_else(|| "_".to_string(), |c| c[1].to_string());
                (label, param.contains('=') || param.contains("..."))
            })
            .collect()
    };
    let mut at = 0;
    for (label, optional) in &params {
        if at < labels.len() && labels[at] == *label {
            at += 1;
            continue;
        }
        if !optional {
            return false;
        }
    }
    at == labels.len()
}

/// Split `a, f(b, c), d<e, f>` at its top-level commas (upstream
/// `splitCppTopLevel`): a quoted string is one unit, `(`, `[`, `{` and `<`
/// nest, and `->` closes nothing.
fn split_top_level(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    let mut at = 0;
    while at < chars.len() {
        let ch = chars[at];
        if ch == '"' || ch == '\'' {
            let mut end = at + 1;
            while end < chars.len() && chars[end] != ch {
                end += if chars[end] == '\\' { 2 } else { 1 };
            }
            let stop = (end + 1).min(chars.len());
            current.extend(&chars[at..stop]);
            at = end + 1;
            continue;
        }
        match ch {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            '>' if at == 0 || chars[at - 1] != '-' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if ch == ',' && depth == 0 {
            out.push(current.trim().to_string());
            current.clear();
        } else {
            current.push(ch);
        }
        at += 1;
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
    out
}

/// The balanced `( … )` after `name`, from `line` / `column` of `file`
/// (twelve lines at most), or `None` (upstream `cppParenListAfter`).
fn paren_list_after(
    file: &str,
    line: i64,
    column: usize,
    name: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let facts = context.source_facts(file)?;
    let start = usize::try_from(line).ok()?.checked_sub(1)?;
    let end = (start + 12).min(facts.line_count());
    if start >= end {
        return None;
    }
    let text = facts.join_lines(start, end, "\n");
    let rest = text.get(column..)?;
    let at = Regex::new(&format!(
        r"(?-u:\b){}\s*(?:<[^<>()]*>)?\s*\(",
        regex::escape(name)
    ))
    .ok()?
    .find(rest)?;
    let open = column + at.end() - 1;
    let mut depth = 0usize;
    for (offset, ch) in text[open..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[open + 1..open + offset].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{split_top_level, swift_labels_fit};

    #[test]
    fn top_level_commas_skip_nested_groups_and_strings() {
        assert_eq!(
            split_top_level("a, f(b, c), d<e, f>, \"x, y\""),
            ["a", "f(b, c)", "d<e, f>", "\"x, y\""]
        );
        assert_eq!(split_top_level("p -> q, r"), ["p -> q", "r"]);
        assert!(split_top_level("  ").is_empty());
    }

    #[test]
    fn swift_labels_fit_in_order_with_optional_parameters() {
        let labels = vec!["_".to_string(), "numberOfRowsInSection".to_string()];
        assert!(swift_labels_fit(
            &labels,
            "_ tableView: UITableView, numberOfRowsInSection section: Int"
        ));
        assert!(!swift_labels_fit(
            &labels,
            "_ tableView: UITableView, titleForHeaderInSection section: Int"
        ));
        let labels = vec!["optionallyProducingResumeData".to_string()];
        assert!(swift_labels_fit(
            &labels,
            "optionallyProducingResumeData handler: ((Data?) -> Void)?, retry: Bool = false"
        ));
    }
}
