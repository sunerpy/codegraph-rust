//! Erlang (`.erl` / `.hrl`) `LanguageSpec`.
//!
//! Extraction-tier port of `upstream extraction/languages/erlang.ts` (commit
//! `6511722`, #1165 — the extraction slice only). Erlang is form-based: its
//! symbol-bearing shapes (a function's name lives on its `function_clause`, the
//! grammar emits one `fun_decl` per clause, `record_decl` carries fields as
//! direct children, and `-spec`/`-callback`/type bodies parse as `call` nodes)
//! don't fit the generic C-family type-set extractor, so — faithful to
//! upstream's mostly-empty `erlangExtractor` config — [`ErlangSpec`] returns
//! `&[]` for the C-family type-sets and the extraction is driven by the
//! `Language::Erlang`-guarded `visit_erlang_node` walker extension in
//! [`crate::walker`].
//!
//! Two config hooks ARE used through the generic machinery, exactly as
//! upstream: `package_types` (`module_attribute` → a file namespace, so every
//! function's qualified name is `m::f` — the same shape the remote-call branch
//! emits, so `mod:f(...)` resolves through the standard qualified-name matcher
//! with ZERO resolver changes) and `import_types`
//! (`import_attribute`/`pp_include`/`pp_include_lib` → an `Import` node + an
//! `Imports` file/module edge).
//!
//! The non-Godot framework RESOLVER bridges — `-behaviour(x)` callback
//! contracts, `gen_server:call/cast(?MODULE|?SERVER)` → `handle_call`/
//! `handle_cast`, the `spawn`/`apply`/`proc_lib`/`timer`/`rpc` MFA-argument
//! callee lift, var-module dispatch, and `.app`/`.app.src` resource-tuple wiring
//! — are DEFERRED, consistent with ArkTS/Nix/Terraform (the port has exactly one
//! concrete `FrameworkResolver`, `GodotResolver`).
//!
//! The pure AST helper fns below are `pub(crate)` and re-exported through
//! [`crate::lang`] so the walker can reach them as `crate::lang::<fn>`.

use std::collections::HashSet;

use codegraph_core::types::Language;
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

pub struct ErlangSpec;

pub static ERLANG_SPEC: ErlangSpec = ErlangSpec;

impl LanguageSpec for ErlangSpec {
    fn language(&self) -> Language {
        Language::Erlang
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_erlang::LANGUAGE.into()
    }

    fn function_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn class_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn method_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn interface_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn struct_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn enum_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn enum_member_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn type_alias_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn import_types(&self) -> &'static [&'static str] {
        // Handled through the generic `extract_import` path (`extract_import`
        // below). `visit_erlang_node` returns `false` for these kinds so the
        // generic import machinery owns them.
        &["import_attribute", "pp_include", "pp_include_lib"]
    }

    fn call_types(&self) -> &'static [&'static str] {
        // Erlang call/reference edges are emitted by the `Language::Erlang`
        // branch of `visit_erlang_node` (the remote-call shape needs the PARENT
        // `remote` node), so no generic call-type dispatch.
        &[]
    }

    fn variable_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn package_types(&self) -> &'static [&'static str] {
        &["module_attribute"]
    }

    fn extract_package(&self, node: Node<'_>, source: &str) -> Option<String> {
        let name = child_by_field(node, "name")?;
        let text = erlang_atom_text(name, source);
        if text.is_empty() { None } else { Some(text) }
    }

    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        if node.kind() == "import_attribute" {
            let module = child_by_field(node, "module")?;
            let module_name = erlang_atom_text(module, source);
            if module_name.is_empty() {
                return None;
            }
            return Some(ImportInfo {
                module_name,
                signature: erlang_collapse_ws(&node_text(node, source))
                    .chars()
                    .take(200)
                    .collect(),
                handled_refs: false,
            });
        }
        // pp_include / pp_include_lib — a C-include-style file dependency on a
        // `.hrl`; the header path is the `file` child with quotes stripped.
        let file = child_by_field(node, "file")?;
        let raw = node_text(file, source);
        let path = raw.trim().trim_matches('"');
        if path.is_empty() {
            return None;
        }
        Some(ImportInfo {
            module_name: path.to_string(),
            signature: node_text(node, source).trim().to_string(),
            handled_refs: false,
        })
    }

    fn name_field(&self) -> &'static str {
        "name"
    }

    fn body_field(&self) -> &'static str {
        "body"
    }

    fn params_field(&self) -> &'static str {
        "args"
    }

    fn return_field(&self) -> &'static str {
        ""
    }
}

/// Text of an atom with quoted-atom quotes stripped (`'EXIT'` → `EXIT`). Ports
/// `atomText` (erlang.ts:27-29).
pub(crate) fn erlang_atom_text(node: Node<'_>, source: &str) -> String {
    let text = node_text(node, source);
    let bytes = text.as_bytes();
    if text.len() >= 2 && bytes[0] == b'\'' && bytes[text.len() - 1] == b'\'' {
        text[1..text.len() - 1].to_string()
    } else {
        text
    }
}

/// Collapse all runs of whitespace to a single space and trim. Ports
/// `collapseWs` (erlang.ts:31-33).
pub(crate) fn erlang_collapse_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Exported function names for a module, or [`ErlangExports::All`] for
/// `-compile(export_all)`. Ports `moduleExports` (erlang.ts:53-79). The result
/// is used only for the `is_exported` membership check — it never iterates into
/// output — so a `HashSet` is deterministic here.
pub(crate) enum ErlangExports {
    All,
    Names(HashSet<String>),
}

impl ErlangExports {
    pub(crate) fn contains(&self, name: &str) -> bool {
        match self {
            ErlangExports::All => true,
            ErlangExports::Names(set) => set.contains(name),
        }
    }
}

/// Walk the top-level forms collecting `-export([...])` fun names, or `All` when
/// a `-compile(export_all)` attribute is present. Ports `moduleExports`.
pub(crate) fn erlang_module_exports(root: Node<'_>, source: &str) -> ErlangExports {
    let mut names: HashSet<String> = HashSet::new();
    for form in root.named_children(&mut root.walk()) {
        if form.kind() == "compile_options_attribute"
            && node_text(form, source).contains("export_all")
        {
            return ErlangExports::All;
        }
        if form.kind() == "export_attribute" {
            for fa in form.named_children(&mut form.walk()) {
                if fa.kind() != "fa" {
                    continue;
                }
                if let Some(fun) = child_by_field(fa, "fun") {
                    let n = erlang_atom_text(fun, source);
                    if !n.is_empty() {
                        names.insert(n);
                    }
                }
            }
        }
    }
    ErlangExports::Names(names)
}

/// The `-spec` directly above a function (comments may sit between), if it names
/// the function. Ports `precedingSpec` (erlang.ts:82-90).
pub(crate) fn erlang_preceding_spec<'t>(
    fun_decl: Node<'t>,
    name: &str,
    source: &str,
) -> Option<Node<'t>> {
    let mut prev = fun_decl.prev_named_sibling();
    while let Some(p) = prev {
        if p.kind() == "comment" {
            prev = p.prev_named_sibling();
        } else {
            break;
        }
    }
    let p = prev?;
    if p.kind() == "spec" {
        let spec_fun = child_by_field(p, "fun")?;
        if erlang_atom_text(spec_fun, source) == name {
            return Some(p);
        }
    }
    None
}

/// `name(Args) when Guard` — the clause text up to the `->`. Ports
/// `clauseHeader` (erlang.ts:93-97).
pub(crate) fn erlang_clause_header(clause: Node<'_>, source: &str) -> Option<String> {
    let end = child_by_field(clause, "body")
        .map(|b| b.start_byte())
        .unwrap_or_else(|| clause.end_byte());
    let start = clause.start_byte();
    let text = source.get(start..end)?;
    let header = erlang_collapse_ws(text);
    if header.is_empty() {
        None
    } else {
        Some(header)
    }
}

/// The `function_clause` children of a `fun_decl` (the grammar emits one clause
/// per `fun_decl`, but a form may nest several).
pub(crate) fn erlang_function_clauses<'t>(fun_decl: Node<'t>) -> Vec<Node<'t>> {
    fun_decl
        .named_children(&mut fun_decl.walk())
        .filter(|c| c.kind() == "function_clause")
        .collect()
}

/// A clause's static function name (its `name` field atom), or `None` for a
/// macro-templated clause (`?M(...) -> ...`) with no static name.
pub(crate) fn erlang_clause_name(clause: Node<'_>, source: &str) -> Option<String> {
    let name_node = child_by_field(clause, "name")?;
    if name_node.kind() != "atom" {
        return None;
    }
    let name = erlang_atom_text(name_node, source);
    if name.is_empty() { None } else { Some(name) }
}

/// A `record_field` child's name atom. Ports the field-name read in
/// `handleRecordDecl` (erlang.ts:154-158).
pub(crate) fn erlang_record_field_name(field: Node<'_>, source: &str) -> Option<String> {
    let name = child_by_field(field, "name")?;
    let text = erlang_atom_text(name, source);
    if text.is_empty() { None } else { Some(text) }
}

/// The name atom of a `type_alias` / `opaque` (its `type_name` wrapper's `name`
/// child). Ports `handleTypeAlias` (erlang.ts:164-173).
pub(crate) fn erlang_type_alias_name(node: Node<'_>, source: &str) -> Option<String> {
    let type_name = child_by_field(node, "name")?;
    let name_node = child_by_field(type_name, "name")?;
    let text = erlang_atom_text(name_node, source);
    if text.is_empty() { None } else { Some(text) }
}

/// The macro name of a `pp_define` (its `lhs` `macro_lhs`'s `name` child). Ports
/// `handlePpDefine` (erlang.ts:175-178).
pub(crate) fn erlang_macro_name(node: Node<'_>, source: &str) -> Option<String> {
    let lhs = child_by_field(node, "lhs")?;
    let name_node = child_by_field(lhs, "name")?;
    let text = node_text(name_node, source);
    if text.is_empty() { None } else { Some(text) }
}

/// The qualified callee name of a `call` node, or `None` for a dynamic
/// (var/macro-module) target that has no static callee. A remote call
/// (`mod:f(...)`) yields `mod::f`; a local call (`f(...)`) yields `f`;
/// `?MODULE:f(...)` yields the bare `f` (same-file preference resolves it).
/// Ports the `node.type === 'call'` arm of the erlang branch in
/// `extractCall` (tree-sitter.ts:3684-3722), MINUS the DEFERRED gen_server and
/// MFA-argument lifts.
pub(crate) fn erlang_call_ref_name(call: Node<'_>, source: &str) -> Option<String> {
    let mut callee = child_by_field(call, "expr")?;
    let mut module_node: Option<Node<'_>> = None;
    // remote(module, fun: call) — the shape the grammar produces; the
    // node-types also permit call(expr: remote), so handle both nestings.
    if let Some(parent) = call.parent() {
        if parent.kind() == "remote" {
            module_node = child_by_field(parent, "module");
        }
    }
    if module_node.is_none() && callee.kind() == "remote" {
        module_node = child_by_field(callee, "module");
        callee = child_by_field(callee, "fun")?;
    }
    if callee.kind() != "atom" {
        return None;
    }
    let fn_bare = erlang_atom_text(callee, source);
    let Some(module_node) = module_node else {
        return Some(fn_bare);
    };
    // `remote_module` wraps the module expression in a `module` field.
    let module_expr = child_by_field(module_node, "module");
    match module_expr {
        Some(m) if m.kind() == "atom" => {
            Some(format!("{}::{}", erlang_atom_text(m, source), fn_bare))
        }
        Some(m) => {
            // Non-atom module. `?MODULE:f(X)` targets THIS module — keep the
            // bare name so same-file preference resolves it. Anything else
            // (`Mod:f(X)`) is dynamic dispatch with no static target: stay
            // silent rather than link an arbitrary same-named function.
            if m.kind() == "macro_call_expr" {
                if let Some(mname) = child_by_field(m, "name") {
                    if node_text(mname, source) == "MODULE" {
                        return Some(fn_bare);
                    }
                }
            }
            None
        }
        None => Some(fn_bare),
    }
}

/// The referenced fun name of an `internal_fun` (`fun f/1` → `f`) or
/// `external_fun` (`fun mod:f/1` → `mod::f`), or `None` for a var-part
/// (`fun Mod:F/A`) that is dynamic. These are function VALUES —
/// `EdgeKind::References`, not calls. Ports the `internal_fun`/`external_fun`
/// arm (tree-sitter.ts:3785-3803).
pub(crate) fn erlang_fun_value_ref_name(node: Node<'_>, source: &str) -> Option<String> {
    let fun = child_by_field(node, "fun")?;
    if fun.kind() != "atom" {
        return None;
    }
    let mut name = erlang_atom_text(fun, source);
    if node.kind() == "external_fun" {
        let module_wrapper = child_by_field(node, "module")?;
        let module_atom = child_by_field(module_wrapper, "name")?;
        if module_atom.kind() != "atom" {
            return None;
        }
        name = format!("{}::{}", erlang_atom_text(module_atom, source), name);
    }
    Some(name)
}

/// The record name atom of a `record_expr` / `record_update_expr` /
/// `record_index_expr` / `record_field_expr` (their `name` field is a
/// `record_name` wrapping the atom). These are record USAGES —
/// `EdgeKind::References`. Ports the record arm (tree-sitter.ts:3828-3838).
pub(crate) fn erlang_record_ref_name(node: Node<'_>, source: &str) -> Option<String> {
    let record_name = child_by_field(node, "name")?;
    if record_name.kind() != "record_name" {
        return None;
    }
    let atom_node = child_by_field(record_name, "name")?;
    if atom_node.kind() != "atom" {
        return None;
    }
    Some(erlang_atom_text(atom_node, source))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_erlang::LANGUAGE.into())
            .unwrap();
        parser.parse(src, None).unwrap()
    }

    fn first_of_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
        if node.kind() == kind {
            return Some(node);
        }
        for i in 0..node.named_child_count() as u32 {
            let child = node.named_child(i)?;
            if let Some(found) = first_of_kind(child, kind) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn erlang_spec_has_empty_type_sets() {
        assert!(ERLANG_SPEC.function_types().is_empty());
        assert!(ERLANG_SPEC.class_types().is_empty());
        assert!(ERLANG_SPEC.method_types().is_empty());
        assert!(ERLANG_SPEC.struct_types().is_empty());
        assert!(ERLANG_SPEC.call_types().is_empty());
        assert!(ERLANG_SPEC.variable_types().is_empty());
        assert_eq!(ERLANG_SPEC.language(), Language::Erlang);
        // package + import ARE wired (the two upstream config hooks).
        assert_eq!(ERLANG_SPEC.package_types(), &["module_attribute"]);
        assert_eq!(
            ERLANG_SPEC.import_types(),
            &["import_attribute", "pp_include", "pp_include_lib"]
        );
    }

    #[test]
    fn erlang_parses_module() {
        let src = "-module(m).\n-export([f/0]).\nf() -> ok.\n";
        let tree = parse(src);
        assert!(!tree.root_node().has_error());
        let module_attr = first_of_kind(tree.root_node(), "module_attribute").expect("module");
        assert_eq!(
            ERLANG_SPEC.extract_package(module_attr, src).as_deref(),
            Some("m")
        );
    }

    #[test]
    fn atom_text_strips_quotes() {
        let src = "-module('My.Mod').\n";
        let tree = parse(src);
        let module_attr = first_of_kind(tree.root_node(), "module_attribute").expect("module");
        let name = child_by_field(module_attr, "name").expect("name");
        assert_eq!(erlang_atom_text(name, src), "My.Mod");
    }
}
