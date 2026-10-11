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
use std::fmt::Write as _;
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

    /// The arity once a C# extension method's `this` parameter, which an
    /// instance-style call does not pass, is set aside.
    fn without_this(self) -> Self {
        Self {
            min: self.min.saturating_sub(1),
            max: self.max.map(|max| max.saturating_sub(1)),
        }
    }
}

/// One overload's declared parameters: its arity, whether it is a C#
/// extension method (a `this` first parameter), the parameters' text, and
/// the type parameters a C# extension method declares (`Open<T>`).
#[derive(Debug, Clone)]
struct Overload {
    arity: Arity,
    extension: bool,
    params: Vec<String>,
    type_params: Vec<String>,
}

/// A method's same-owner overloads in its file.
#[derive(Debug)]
pub(crate) struct OverloadSet {
    name: String,
    owner: String,
    own: Option<Overload>,
    siblings: Vec<(String, Option<Overload>)>,
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
    // A C# extension method called on a receiver other than its own class
    // (`propertyInfo.GetGetMethod(true)`, not `TypeExtensions.GetGetMethod(p,
    // true)`) is not passed its `this` parameter (W3-05c).
    let instance_style = reference.language == Language::CSharp
        && reference
            .reference_name
            .rsplit_once('.')
            .is_some_and(|(receiver, _)| {
                receiver.rsplit('.').next().unwrap_or(receiver) != set.owner
            });
    let arity_of = |entry: &Overload| {
        if entry.extension && instance_style {
            entry.arity.without_this()
        } else {
            entry.arity
        }
    };
    let Some(own) = set.own.as_ref().map(arity_of) else {
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
    let arguments = if args.trim().is_empty() {
        Vec::new()
    } else {
        split_top_level(&args)
    };
    let count = arguments.len();
    let extension_call = instance_style
        && set
            .own
            .iter()
            .chain(set.siblings.iter().filter_map(|(_, entry)| entry.as_ref()))
            .any(|entry| entry.extension);
    if extension_call {
        return match csharp_extension_choice(&set, &result.target_node_id, &arguments) {
            Some(id) if id != result.target_node_id => retarget(&id),
            _ => result,
        };
    }
    if own.takes(count) {
        return result;
    }
    let fits: Vec<&String> = set
        .siblings
        .iter()
        .filter(|(_, entry)| {
            entry
                .as_ref()
                .is_some_and(|entry| arity_of(entry).takes(count))
        })
        .map(|(id, _)| id)
        .collect();
    match fits.as_slice() {
        [only] => retarget(only),
        _ => result,
    }
}

/// How a C# overload takes a call's arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    /// As declared: no `params`, or an array passed at the `params` position.
    Normal,
    /// The `params` array built from the trailing arguments.
    Expanded,
}

/// The overload of `set` an instance-style call of a C# extension method
/// takes, by C#'s rules with no `this` argument (W3-05c). Only the extension
/// overloads whose `this` parameter is declared as the current target's is
/// ([`receiver_key`]: the method's own type parameters numbered, so `Box<T>`
/// and `Box<U>` agree) take the receiver it was chosen for: a static method
/// that is no extension, or one extending another type, is never a
/// candidate. Of those, an overload that takes the arguments in its normal
/// form beats one that takes them only expanded; an array creation passed at
/// a `params` position goes to the `params` overload over one whose
/// parameter there is not an array. `None` when the current target is no
/// extension method, or no overload fits; the current target when several
/// still do.
fn csharp_extension_choice(
    set: &OverloadSet,
    current: &str,
    arguments: &[String],
) -> Option<String> {
    let receiver = set.own.as_ref().and_then(receiver_key)?;
    let count = arguments.len();
    let array_last = arguments.last().is_some_and(|arg| is_array_creation(arg));
    let form = |entry: &Overload| -> Option<Form> {
        let params = entry.params.get(1..).unwrap_or_default();
        let variadic = params.last().is_some_and(|p| is_pack(p));
        let fixed = if variadic {
            &params[..params.len() - 1]
        } else {
            params
        };
        let required = fixed.iter().filter(|p| !p.contains('=')).count();
        if !variadic {
            return (required <= count && count <= fixed.len()).then_some(Form::Normal);
        }
        if count == params.len() && array_last {
            return Some(Form::Normal);
        }
        (count >= required).then_some(Form::Expanded)
    };
    let entries: Vec<(&str, &Overload)> = set
        .own
        .iter()
        .map(|entry| (current, entry))
        .chain(set.siblings.iter().filter_map(|(id, entry)| {
            entry
                .as_ref()
                .filter(|entry| receiver_key(entry).as_ref() == Some(&receiver))
                .map(|entry| (id.as_str(), entry))
        }))
        .collect();
    let fitting: Vec<(&str, &Overload, Form)> = entries
        .iter()
        .filter_map(|&(id, entry)| form(entry).map(|form| (id, entry, form)))
        .collect();
    let best = if fitting.iter().any(|(_, _, form)| *form == Form::Normal) {
        Form::Normal
    } else {
        Form::Expanded
    };
    let mut chosen: Vec<(&str, &Overload)> = fitting
        .iter()
        .filter(|(_, _, form)| *form == best)
        .map(|&(id, entry, _)| (id, entry))
        .collect();
    if chosen.len() > 1 && array_last {
        // `new object?[] { … }` at a `params object?[]` position is an
        // identity conversion; to an `object?` parameter it is not.
        let at = count - 1;
        let declared = |entry: &Overload| entry.params.get(1 + at).cloned().unwrap_or_default();
        let packs: Vec<(&str, &Overload)> = chosen
            .iter()
            .copied()
            .filter(|(_, entry)| is_pack(&declared(entry)))
            .collect();
        let others_take_no_array = chosen
            .iter()
            .filter(|(_, entry)| !is_pack(&declared(entry)))
            .all(|(_, entry)| !declares_array(&declared(entry)));
        if packs.len() == 1 && others_take_no_array {
            chosen = packs;
        }
    }
    match chosen.as_slice() {
        [] => None,
        [(id, _)] => Some((*id).to_string()),
        _ => chosen
            .iter()
            .any(|(id, _)| *id == current)
            .then(|| current.to_string()),
    }
}

/// A C# extension overload's `this` type as written, with the method's own
/// type parameters numbered by first appearance: `Box<T>` over `Open<T>` and
/// `Box<U>` over `Open<U>` are both `Box<#0>`, while `IList<int>` stays
/// apart from `IList<T>`. `None` for an overload that is no extension method.
fn receiver_key(entry: &Overload) -> Option<String> {
    let text = crate::csharp_extensions::this_parameter_text(entry.params.first()?)?;
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut seen: Vec<&str> = Vec::new();
    let mut key = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(first) = rest.chars().next() {
        let len = if is_word(first) {
            rest.find(|c: char| !is_word(c)).unwrap_or(rest.len())
        } else {
            first.len_utf8()
        };
        let token = &rest[..len];
        if entry.type_params.iter().any(|param| param == token) {
            let ordinal = seen.iter().position(|s| *s == token).unwrap_or_else(|| {
                seen.push(token);
                seen.len() - 1
            });
            let _ = write!(key, "#{ordinal}");
        } else {
            key.push_str(token);
        }
        rest = &rest[len..];
    }
    Some(key)
}

/// The type parameters a method declares (`Open<T, U>(…)`: `T`, `U`), read
/// from its declaration; empty when it declares none.
fn method_type_parameters(
    file: &str,
    line: i64,
    name: &str,
    context: &dyn ResolutionContext,
) -> Vec<String> {
    let Some(facts) = context.source_facts(file) else {
        return Vec::new();
    };
    let Some(start) = usize::try_from(line).ok().and_then(|l| l.checked_sub(1)) else {
        return Vec::new();
    };
    let end = (start + 12).min(facts.line_count());
    if start >= end {
        return Vec::new();
    }
    let text = facts.join_lines(start, end, "\n");
    // The first `name(` or `name<…>(` is the declaration's own head, as in
    // [`paren_list_after`]; a later overload's type parameters are not its.
    let Ok(pattern) = Regex::new(&format!(
        r"(?-u:\b){}\s*(?:<([^<>()]*)>)?\s*\(",
        regex::escape(name)
    )) else {
        return Vec::new();
    };
    pattern
        .captures(&text)
        .and_then(|captures| captures.get(1))
        .map(|list| {
            list.as_str()
                .split(',')
                .filter_map(|param| {
                    // `[Attr] T`: the name is the last word.
                    param
                        .trim()
                        .rsplit(|c: char| !(c.is_alphanumeric() || c == '_'))
                        .next()
                        .filter(|name| !name.is_empty())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `new T[] { … }`, `new T[n]` or `new[] { … }`.
fn is_array_creation(argument: &str) -> bool {
    let Some(rest) = argument.trim().strip_prefix("new") else {
        return false;
    };
    if !rest.starts_with(|c: char| c.is_whitespace() || c == '[') {
        return false;
    }
    let head = rest.split(['{', '(']).next().unwrap_or_default();
    head.contains('[')
}

/// Whether a declared parameter's type is an array (`object?[] args`).
fn declares_array(param: &str) -> bool {
    let without_default = param.split('=').next().unwrap_or(param);
    without_default
        .trim()
        .rsplit_once(char::is_whitespace)
        .is_some_and(|(declared, _)| declared.trim_end().ends_with(']'))
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
    let arity = |node: &Node| -> Option<Overload> {
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
        let extension = node.language == Language::CSharp
            && params
                .first()
                .is_some_and(|first| crate::csharp_extensions::is_this_parameter(first));
        let type_params = if extension {
            method_type_parameters(&node.file_path, node.start_line, &own.name, context)
        } else {
            Vec::new()
        };
        Some(Overload {
            arity: Arity { min, max },
            extension,
            params,
            type_params,
        })
    };
    Some(OverloadSet {
        name: own.name.clone(),
        owner: owner.rsplit("::").next().unwrap_or(owner).to_string(),
        own: arity(&own),
        siblings: siblings
            .iter()
            .map(|sibling| (sibling.id.clone(), arity(sibling)))
            .collect(),
    })
}

/// A variadic parameter: `...` (outside one level of `<…>`), C#'s `params`,
/// Kotlin's `vararg`.
pub(crate) fn is_pack(param: &str) -> bool {
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
pub(crate) fn split_top_level(text: &str) -> Vec<String> {
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
pub(crate) fn paren_list_after(
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
    use super::{
        Arity, Overload, OverloadSet, csharp_extension_choice, declares_array, is_array_creation,
        split_top_level, swift_labels_fit,
    };

    fn overload(params: &[&str]) -> Overload {
        generic(&[], params)
    }

    /// An overload declaring the type parameters `type_params`.
    fn generic(type_params: &[&str], params: &[&str]) -> Overload {
        let params: Vec<String> = params.iter().map(|p| (*p).to_string()).collect();
        let min = params
            .iter()
            .filter(|p| !p.contains('=') && !super::is_pack(p))
            .count();
        let max = (!params.iter().any(|p| super::is_pack(p))).then_some(params.len());
        Overload {
            arity: Arity { min, max },
            extension: params
                .first()
                .is_some_and(|p| crate::csharp_extensions::is_this_parameter(p)),
            params,
            type_params: type_params.iter().map(|p| (*p).to_string()).collect(),
        }
    }

    fn set(own: &[&str], siblings: &[(&str, &[&str])]) -> OverloadSet {
        OverloadSet {
            name: "M".to_string(),
            owner: "Ext".to_string(),
            own: Some(overload(own)),
            siblings: siblings
                .iter()
                .map(|(id, params)| ((*id).to_string(), Some(overload(params))))
                .collect(),
        }
    }

    fn args(text: &str) -> Vec<String> {
        split_top_level(text)
    }

    #[test]
    fn an_array_at_a_params_position_takes_the_params_overload() {
        let formats = set(
            &[
                "this string format",
                "IFormatProvider provider",
                "object? arg0",
            ],
            &[(
                "params",
                &[
                    "this string format",
                    "IFormatProvider provider",
                    "params object?[] args",
                ],
            )],
        );
        assert_eq!(
            csharp_extension_choice(&formats, "own", &args("provider, new object?[] { a, b }"))
                .as_deref(),
            Some("params")
        );
        // A plain argument: the normal form of the fixed overload beats the
        // expanded `params` one.
        assert_eq!(
            csharp_extension_choice(&formats, "own", &args("provider, a")).as_deref(),
            Some("own")
        );
        assert_eq!(
            csharp_extension_choice(&formats, "params", &args("provider, a, b")).as_deref(),
            Some("params")
        );
    }

    #[test]
    fn an_attributed_this_parameter_is_not_an_argument() {
        let getters = set(
            &["[NotNull] this PropertyInfo propertyInfo", "bool nonPublic"],
            &[("one", &["[NotNull] this PropertyInfo propertyInfo"])],
        );
        assert_eq!(
            csharp_extension_choice(&getters, "own", &args("true")).as_deref(),
            Some("own")
        );
        assert_eq!(
            csharp_extension_choice(&getters, "own", &args("")).as_deref(),
            Some("one")
        );
    }

    #[test]
    fn same_arity_overloads_keep_the_current_target() {
        let methods = set(
            &["this Type type", "string name", "BindingFlags flags"],
            &[(
                "other",
                &["this Type type", "string name", "IList<Type> types"],
            )],
        );
        assert_eq!(
            csharp_extension_choice(&methods, "own", &args("null, parameterTypes")).as_deref(),
            Some("own")
        );
    }

    #[test]
    fn only_overloads_extending_the_receivers_type_compete() {
        // `type.Pick(arg)`: the plain static `Pick(object?)` and
        // `Pick(this Exception, object?)` fit one argument in their normal
        // form, but neither takes a `Type` receiver.
        let picks = set(
            &["this Type type", "params object?[] args"],
            &[
                ("plain", &["object? arg"]),
                ("other", &["this Exception error", "object? arg"]),
            ],
        );
        assert_eq!(
            csharp_extension_choice(&picks, "own", &args("arg")).as_deref(),
            Some("own")
        );
        // The `this` types are compared as written, spacing aside:
        // `IList<int>` is not the `IList<T>` the current target extends.
        let lists = OverloadSet {
            name: "M".to_string(),
            owner: "Ext".to_string(),
            own: Some(generic(&["T"], &["this IList<T> list", "params T[] items"])),
            siblings: vec![
                (
                    "ints".to_string(),
                    Some(overload(&["this IList<int> list", "int item"])),
                ),
                (
                    "same".to_string(),
                    Some(generic(&["T"], &["this  IList<T>  list", "T item"])),
                ),
            ],
        };
        assert_eq!(
            csharp_extension_choice(&lists, "own", &args("1")).as_deref(),
            Some("same")
        );
        let timers = set(
            &["this System.Timers.Timer timer", "params object?[] args"],
            &[(
                "threading",
                &["this System.Threading.Timer timer", "object? arg"],
            )],
        );
        assert_eq!(
            csharp_extension_choice(&timers, "own", &args("arg")).as_deref(),
            Some("own")
        );
        // A current target that is no extension method names no receiver
        // type to compare with.
        let plain = set(
            &["object? arg"],
            &[("ext", &["this Type type", "object? arg"])],
        );
        assert_eq!(csharp_extension_choice(&plain, "own", &args("arg")), None);
    }

    #[test]
    fn renamed_type_parameters_extend_the_same_type() {
        let pair = |own: Overload, sibling: Overload| OverloadSet {
            name: "Open".to_string(),
            owner: "BoxExtensions".to_string(),
            own: Some(own),
            siblings: vec![("sibling".to_string(), Some(sibling))],
        };
        // `box.Open(1)`: `Open<U>(this Box<U>, U)` takes it in its normal
        // form, though its type parameter is not named `T`.
        let boxes = pair(
            generic(&["T"], &["this Box<T> box", "params T[] values"]),
            generic(&["U"], &["this Box<U> box", "U value"]),
        );
        assert_eq!(
            csharp_extension_choice(&boxes, "own", &args("1")).as_deref(),
            Some("sibling")
        );
        // Numbered by first appearance in the `this` type: `Map<B, A>` over
        // `<A, B>` is `Map<T, U>` over `<T, U>`.
        let maps = pair(
            generic(&["T", "U"], &["this Map<T, U> map", "params T[] keys"]),
            generic(&["A", "B"], &["this Map<B, A> map", "B key"]),
        );
        assert_eq!(
            csharp_extension_choice(&maps, "own", &args("k")).as_deref(),
            Some("sibling")
        );
        // A constructed `Box<int>` is not the open `Box<T>`.
        let constructed = pair(
            generic(&["T"], &["this Box<T> box", "params T[] values"]),
            overload(&["this Box<int> box", "int value"]),
        );
        assert_eq!(
            csharp_extension_choice(&constructed, "own", &args("1")).as_deref(),
            Some("own")
        );
    }

    #[test]
    fn array_creations_and_array_parameters_are_recognized() {
        assert!(is_array_creation("new object?[] { a, b }"));
        assert!(is_array_creation("new[] { 1, 2 }"));
        assert!(is_array_creation("new int[3]"));
        assert!(!is_array_creation("new Widget(1)"));
        assert!(!is_array_creation("newer"));
        assert!(declares_array("object?[] args"));
        assert!(declares_array("params object?[] args"));
        assert!(!declares_array("object? arg0"));
    }

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
