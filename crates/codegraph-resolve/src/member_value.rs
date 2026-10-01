//! Python and Go method values (upstream v1.6.1 #1820).
//!
//! `pool.submit(self.store.fetch)` and `Submit(c.store.Fetch)` pass a method
//! as a value. The extractor keeps the whole receiver path, and this module
//! resolves it through the receiver's own scope — the enclosing class for
//! `self`/`cls`, a field's declared or constructed type, a local's type, an
//! imported class, a Go struct field — before considering a globally unique
//! method name. Ambiguity, a non-callable member (a data attribute, a
//! `@property`) or an unknowable receiver leaves the value unresolved: a wrong
//! callback edge is worse than none.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use crate::import_resolver::resolve_via_import;
use crate::name_matcher::{
    infer_local_receiver_type, prefer_call_site_file, resolve_method_on_type, same_language_family,
};
use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// Primitive and built-in Go field types: a field of one never names a
/// project type.
const GO_BUILTIN_FIELD_TYPES: [&str; 26] = [
    "string",
    "bool",
    "byte",
    "rune",
    "error",
    "any",
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "uintptr",
    "float32",
    "float64",
    "complex64",
    "complex128",
    "chan",
    "map",
    "func",
    "struct",
    "interface",
];

/// Resolve a Python or Go member value `receiver.member` (#1820).
pub(crate) fn match_member_function_ref(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let (receiver, member) = reference.reference_name.rsplit_once('.')?;
    let imports = context.get_import_mappings(&reference.file_path, reference.language);
    let root = receiver.split('.').next().unwrap_or(receiver);
    // An import is authoritative even when it points outside the project.
    if imports.iter().any(|import| import.local_name == root) {
        if reference.language == Language::Python
            && let Some(class) = python_ref_class(receiver, reference, context)
        {
            return unique_callable(
                python_members(&class, member, reference, context, &mut HashSet::new()),
                reference,
                context,
                0.9,
            );
        }
        let imported = resolve_via_import(reference, context)?;
        let node = context.get_node_by_id_shared(&imported.target_node_id)?;
        let same_name = context
            .get_nodes_by_qualified_name_shared(&node.qualified_name)
            .into_iter()
            .filter(|candidate| candidate.file_path == node.file_path)
            .collect();
        return unique_callable(same_name, reference, context, 0.9);
    }
    if reference.language == Language::Go {
        if receiver.contains('.') {
            return match_go_field_chain_value(receiver, member, reference, context);
        }
        if let Some(type_name) = infer_local_receiver_type(receiver, reference, context) {
            return go_method_value(&type_name, member, reference, context);
        }
        let types = context
            .get_nodes_by_name_shared(receiver)
            .into_iter()
            .filter(|node| {
                node.language == Language::Go
                    && matches!(node.kind, NodeKind::Struct | NodeKind::Interface)
            })
            .count();
        if types > 0 {
            // A method expression `Store.Fetch` names its type directly.
            return (types == 1)
                .then(|| go_method_value(receiver, member, reference, context))
                .flatten();
        }
    } else {
        let owner = context
            .get_nodes_in_file_shared(&reference.file_path)
            .into_iter()
            .filter(|node| {
                node.kind == NodeKind::Class
                    && node.start_line <= reference.line
                    && node.end_line >= reference.line
            })
            .max_by_key(|node| node.start_line);
        if receiver == "self" || receiver == "cls" {
            let owner = owner?;
            return unique_callable(
                python_members(&owner, member, reference, context, &mut HashSet::new()),
                reference,
                context,
                0.9,
            );
        }
        let mut type_name = if let Some(field) = receiver
            .strip_prefix("self.")
            .or_else(|| receiver.strip_prefix("cls."))
            .filter(|field| is_word(field))
        {
            python_field_type(field, owner.as_deref()?, reference, context)
        } else {
            python_local_type(receiver, reference, context)
        };
        // A type name used directly (`Store.fetch`) is scoped like an annotation.
        if type_name.is_none()
            && receiver
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_uppercase())
            && is_word(receiver)
        {
            type_name = Some(receiver.to_string());
        }
        if let Some(type_name) = type_name.filter(|name| name != "object" && name != "Any") {
            let class = python_ref_class(&type_name, reference, context)?;
            let members = python_members(&class, member, reference, context, &mut HashSet::new());
            if !members.is_empty() {
                return unique_callable(members, reference, context, 0.9);
            }
            // A base-typed field can hold a subclass-only method: keep only
            // descendants of THAT base, so unrelated namesakes cannot win.
            let descendants = context
                .get_nodes_by_name_shared(member)
                .into_iter()
                .filter(|node| node.kind == NodeKind::Method && node.language == Language::Python)
                .filter(|method| {
                    context
                        .get_nodes_in_file_shared(&method.file_path)
                        .into_iter()
                        .find(|candidate| {
                            candidate.kind == NodeKind::Class
                                && method.qualified_name
                                    == format!("{}::{member}", candidate.qualified_name)
                        })
                        .is_some_and(|parent| {
                            python_derives_from(
                                &parent,
                                &class,
                                reference,
                                context,
                                &mut HashSet::new(),
                            )
                        })
                })
                .collect();
            return unique_callable(descendants, reference, context, 0.8);
        }
    }
    // An unknowable receiver stays unresolved, even when exactly one project
    // method bears the member's name: `self.store.fetch` may be a library
    // object's method. Upstream keeps "the old unique-or-drop discipline"
    // across ALL files here; the port does not (KEEP-RUST).
    None
}

/// The single same-family candidate when it is a callable other than the
/// referencing node itself and not a Python property.
fn unique_callable(
    nodes: Vec<Arc<Node>>,
    reference: &RefView,
    context: &dyn ResolutionContext,
    confidence: f64,
) -> Option<ResolvedRef> {
    let pool = nodes
        .into_iter()
        .filter(|node| same_language_family(node.language, reference.language))
        .collect::<Vec<_>>();
    let [target] = pool.as_slice() else {
        return None;
    };
    (matches!(target.kind, NodeKind::Function | NodeKind::Method)
        && target.id != reference.from_node_id
        && !is_python_property(target, context))
    .then(|| ResolvedRef {
        original: reference.clone(),
        target_node_id: target.id.clone(),
        confidence,
        resolved_by: ResolvedBy::FunctionRef,
    })
}

fn go_method_value(
    type_name: &str,
    member: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    resolve_method_on_type(
        type_name,
        member,
        reference,
        context,
        0.9,
        ResolvedBy::FunctionRef,
        None,
        0,
    )
}

/// `base.field.Method` — the field's declared type read off the base type's
/// own struct declaration (upstream `matchGoFieldChainCall`, #1276/#1316). A
/// package-qualified field type is followed only when the package is in the
/// module; built-in types name no project type.
fn match_go_field_chain_value(
    receiver_chain: &str,
    method: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let (base, field) = receiver_chain.split_once('.')?;
    if base.is_empty() || field.is_empty() || field.contains('.') {
        return None;
    }
    let base_type = infer_local_receiver_type(base, reference, context)?;
    let field_type = Regex::new(&format!(
        r"(?-u:\b){}\s+\*?\[?\]?([A-Za-z_][\w.]*)",
        regex::escape(field)
    ))
    .ok()?;
    let structs = prefer_call_site_file(
        context.get_nodes_by_name_shared(&base_type),
        &reference.file_path,
    )
    .into_iter()
    .filter(|node| {
        matches!(node.kind, NodeKind::Struct | NodeKind::Class) && node.language == Language::Go
    })
    .collect::<Vec<_>>();
    for declaration in structs {
        let Some(facts) = context.source_facts(&declaration.file_path) else {
            continue;
        };
        let start = declaration.start_line.saturating_sub(1).max(0) as usize;
        let end = (declaration.end_line.max(0) as usize).min(facts.line_count());
        for index in start..end {
            let line = strip_go_line_comments(facts.line(index));
            let Some(raw_type) = field_type
                .captures(&line)
                .and_then(|captures| captures.get(1))
                .map(|capture| capture.as_str().to_string())
            else {
                continue;
            };
            if let Some((package, _)) = raw_type.split_once('.') {
                let in_module = context.get_go_modules().iter().any(|module| {
                    context
                        .get_import_mappings(&declaration.file_path, Language::Go)
                        .iter()
                        .any(|import| {
                            import.local_name == package
                                && (import.source == module.module_path
                                    || import
                                        .source
                                        .starts_with(&format!("{}/", module.module_path)))
                        })
                });
                if !in_module {
                    continue;
                }
            }
            let Some(type_name) = raw_type.rsplit('.').next() else {
                continue;
            };
            if !type_name
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
                || GO_BUILTIN_FIELD_TYPES.contains(&type_name)
            {
                continue;
            }
            if let Some(resolved) = resolve_method_on_type(
                type_name,
                method,
                reference,
                context,
                0.85,
                ResolvedBy::FunctionRef,
                None,
                0,
            ) {
                return Some(resolved);
            }
        }
    }
    None
}

/// A Go declaration line without its `//` and single-line `/* */` comments.
fn strip_go_line_comments(line: &str) -> String {
    let line = line.split("//").next().unwrap_or(line);
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find("/*") {
        out.push_str(&rest[..open]);
        match rest[open + 2..].find("*/") {
            Some(close) => rest = &rest[open + 2 + close + 2..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The project class a Python name refers to: an import binding when the
/// name's root is imported, else the one same-file class of that name.
fn python_ref_class(
    name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<Node> {
    let root = name.split('.').next().unwrap_or(name);
    if context
        .get_import_mappings(&reference.file_path, Language::Python)
        .iter()
        .any(|import| import.local_name == root)
    {
        let mut lookup = reference.clone();
        lookup.reference_name = name.to_string();
        lookup.reference_kind = EdgeKind::References;
        let hit = resolve_via_import(&lookup, context)?;
        let node = context.get_node_by_id_shared(&hit.target_node_id)?;
        let unique = context
            .get_nodes_by_qualified_name_shared(&node.qualified_name)
            .into_iter()
            .filter(|candidate| {
                candidate.kind == NodeKind::Class && candidate.file_path == node.file_path
            })
            .count()
            == 1;
        return (node.kind == NodeKind::Class && unique).then(|| node.as_ref().clone());
    }
    let classes = context
        .get_nodes_by_name_shared(name)
        .into_iter()
        .filter(|node| node.kind == NodeKind::Class && node.file_path == reference.file_path)
        .collect::<Vec<_>>();
    match classes.as_slice() {
        [class] => Some(class.as_ref().clone()),
        _ => None,
    }
}

/// The classes named in a Python class's base list, resolved from its file.
fn python_bases(class: &Node, reference: &RefView, context: &dyn ResolutionContext) -> Vec<Node> {
    let Some(facts) = context.source_facts(&class.file_path) else {
        return Vec::new();
    };
    let Some(index) = usize::try_from(class.start_line)
        .ok()
        .and_then(|line| line.checked_sub(1))
    else {
        return Vec::new();
    };
    if index >= facts.line_count() {
        return Vec::new();
    }
    let header = facts.line(index);
    let Some(bases) = Regex::new(r"^\s*class\s+\w+\s*\(([^)]*)\)")
        .ok()
        .and_then(|pattern| pattern.captures(header))
        .and_then(|captures| captures.get(1))
        .map(|bases| bases.as_str().to_string())
    else {
        return Vec::new();
    };
    let mut at_class = reference.clone();
    at_class.file_path = class.file_path.clone();
    bases
        .split(',')
        .filter_map(|base| python_ref_class(base.trim(), &at_class, context))
        .collect()
}

fn python_derives_from(
    class: &Node,
    base: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
    seen: &mut HashSet<String>,
) -> bool {
    if seen.len() >= 16 || !seen.insert(class.id.clone()) {
        return false;
    }
    python_bases(class, reference, context)
        .iter()
        .any(|parent| {
            parent.id == base.id || python_derives_from(parent, base, reference, context, seen)
        })
}

/// What `class.member` names: an instance or class assignment shadows any
/// method (the class itself is returned, which is not callable), else the
/// class's own method, else what its bases provide.
fn python_members(
    class: &Node,
    member: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
    seen: &mut HashSet<String>,
) -> Vec<Arc<Node>> {
    if seen.len() >= 16 || !seen.insert(class.id.clone()) {
        return Vec::new();
    }
    if let Some(facts) = context.source_facts(&class.file_path) {
        let (code, lines) = facts.python_comment_free();
        let start = class.start_line.saturating_sub(1).max(0) as usize;
        let end = (class.end_line.max(0) as usize).min(lines.line_count());
        let body = lines.join(code, start.min(end), end, "\n");
        let shadow = Regex::new(&format!(
            r"(?m)^\s*(?:(?:self|cls)\.)?{}\s*(?:=|:)",
            regex::escape(member)
        ));
        if shadow.is_ok_and(|pattern| pattern.is_match(&body)) {
            return vec![Arc::new(class.clone())];
        }
    }
    let own = context
        .get_nodes_by_qualified_name_shared(&format!("{}::{member}", class.qualified_name))
        .into_iter()
        .filter(|node| node.file_path == class.file_path)
        .collect::<Vec<_>>();
    if !own.is_empty() {
        return own;
    }
    let mut inherited = BTreeMap::new();
    for base in python_bases(class, reference, context) {
        for node in python_members(&base, member, reference, context, seen) {
            inherited.entry(node.id.clone()).or_insert(node);
        }
    }
    inherited.into_values().collect()
}

fn is_python_property(node: &Node, context: &dyn ResolutionContext) -> bool {
    if node.language != Language::Python || node.kind != NodeKind::Method {
        return false;
    }
    let Some(facts) = context.source_facts(&node.file_path) else {
        return false;
    };
    let decorator = Regex::new(r"^\s*@(?:property|(?:functools\.)?cached_property)\s*$")
        .expect("property decorator pattern");
    let mut index = node.start_line - 2;
    while index >= 0 && (index as usize) < facts.line_count() {
        let line = facts.line(index as usize);
        if !line.trim_start().starts_with('@') {
            break;
        }
        if decorator.is_match(line) {
            return true;
        }
        index -= 1;
    }
    false
}

/// A local's type from its nearest declaration in the calling function —
/// an annotation or a capitalized constructor call — or a parameter
/// annotation; `<unknown>` for any other assignment.
fn python_local_type(
    receiver: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    if !is_word(receiver) {
        return None;
    }
    let caller = context.get_node_by_id_shared(&reference.from_node_id);
    let facts = context.source_facts(&reference.file_path)?;
    let (code, lines) = facts.python_comment_free();
    let name = regex::escape(receiver);
    let declaration = Regex::new(&format!(
        r#"^\s*{name}\s*(?::\s*["']?([\w.]+)["']?)?\s*=\s*(.*)$"#
    ))
    .ok()?;
    let annotation = Regex::new(&format!(r#"^\s*{name}\s*:\s*["']?([\w.]+)"#)).ok()?;
    let constructor = Regex::new(r"^([A-Z][\w.]*)\s*\(").expect("constructor pattern");
    let lowest = caller.as_ref().map_or(1, |caller| caller.start_line).max(1) - 1;
    let mut index = reference.line - 1;
    while index >= lowest {
        if (index as usize) < lines.line_count() {
            let line = lines.line(code, index as usize);
            if let Some(assigned) = declaration.captures(line) {
                if let Some(declared) = assigned.get(1) {
                    return Some(declared.as_str().to_string());
                }
                let value = assigned.get(2).map_or("", |value| value.as_str());
                return Some(
                    constructor
                        .captures(value)
                        .and_then(|captures| captures.get(1))
                        .map_or_else(
                            || "<unknown>".to_string(),
                            |class| class.as_str().to_string(),
                        ),
                );
            }
            if let Some(declared) = annotation
                .captures(line)
                .and_then(|captures| captures.get(1))
            {
                return Some(declared.as_str().to_string());
            }
        }
        index -= 1;
    }
    let signature = caller?.signature.clone()?;
    Regex::new(&format!(r#"(?-u:\b){name}\s*:\s*["']?([\w.]+)"#))
        .ok()?
        .captures(&signature)
        .and_then(|captures| captures.get(1))
        .map(|declared| declared.as_str().to_string())
}

/// A field's type from its class-level annotation, its own annotated or
/// constructed assignment, or the annotated constructor parameter assigned to
/// it. Conflicting assignments are known-but-ambiguous (`<ambiguous>`), never
/// a name-only fallback.
fn python_field_type(
    field: &str,
    owner: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let facts = context.source_facts(&reference.file_path)?;
    let (code, lines) = facts.python_comment_free();
    let field = regex::escape(field);
    let assignment = Regex::new(&format!(
        r#"^\s*(?:self|cls)\.{field}\s*(?::\s*["']?([\w.]+)["']?)?\s*=\s*(.*)$"#
    ))
    .ok()?;
    let annotation = Regex::new(&format!(
        r#"^\s*(?:(?:self|cls)\.)?{field}\s*:\s*["']?([\w.]+)"#
    ))
    .ok()?;
    let receiver_prefix = Regex::new(r"^\s*(?:self|cls)\.").expect("receiver prefix pattern");
    let constructor = Regex::new(r"^([A-Z][\w.]*)\s*\(").expect("constructor pattern");
    let member_prefix = format!("{}::", owner.qualified_name);
    let methods = context
        .get_nodes_in_file_shared(&reference.file_path)
        .into_iter()
        .filter(|node| {
            node.kind == NodeKind::Method && node.qualified_name.starts_with(&member_prefix)
        })
        .collect::<Vec<_>>();
    let mut types = std::collections::BTreeSet::new();
    let start = owner.start_line.max(0) as usize;
    let end = (owner.end_line.max(0) as usize).min(lines.line_count());
    for index in start..end {
        let line_number = index as i64 + 1;
        let method = methods
            .iter()
            .find(|method| method.start_line <= line_number && method.end_line >= line_number);
        if let Some(method) = method {
            if method.name != "__init__" && method.id != reference.from_node_id {
                continue;
            }
            if method.id == reference.from_node_id && line_number > reference.line {
                continue;
            }
        }
        let line = lines.line(code, index);
        if (method.is_none() || receiver_prefix.is_match(line))
            && let Some(declared) = annotation
                .captures(line)
                .and_then(|captures| captures.get(1))
        {
            types.insert(declared.as_str().to_string());
        }
        let Some(assigned) = assignment.captures(line) else {
            continue;
        };
        if let Some(declared) = assigned.get(1) {
            types.insert(declared.as_str().to_string());
            continue;
        }
        let value = assigned.get(2).map_or("", |value| value.as_str());
        if let Some(class) = constructor
            .captures(value)
            .and_then(|captures| captures.get(1))
        {
            types.insert(class.as_str().to_string());
            continue;
        }
        let parameter = value.trim();
        if let Some(method) = method
            && is_word(parameter)
        {
            if let Some(declared) = method.signature.as_deref().and_then(|signature| {
                Regex::new(&format!(
                    r#"(?-u:\b){}\s*:\s*["']?([\w.]+)"#,
                    regex::escape(parameter)
                ))
                .ok()?
                .captures(signature)
                .and_then(|captures| captures.get(1))
                .map(|declared| declared.as_str().to_string())
            }) {
                types.insert(declared);
            }
        } else {
            types.insert("<unknown>".to_string());
        }
    }
    match types.len() {
        0 => None,
        1 => types.into_iter().next(),
        _ => Some("<ambiguous>".to_string()),
    }
}

fn is_word(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
}
