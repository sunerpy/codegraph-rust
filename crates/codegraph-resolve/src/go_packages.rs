//! Go packages (upstream #2361, for #2323 and #2322; the field chains of
//! #1276/#1316).
//!
//! Go keeps one package per directory, and a Go type name is unique only
//! within its package: each package may have its own `server`, `handler` or
//! `DAO`. A type's methods are declared in its own package, so a receiver
//! typed by its declaration reaches the methods its declaring package has
//! (the caller's own for a bare type, the imported one for `store.Manager`),
//! then those of the types its declaration embeds. A field chain
//! `s.db.CreateItem()` reaches the method on the type the field's declaration
//! names, in the package that declares it. An import path names a package of
//! one of the project's modules: the module declaring the longest prefix of
//! it, the importing file's own module breaking a tie; a module under
//! `testdata/`, or under a directory named with a leading `_` or `.`, serves
//! only its own files. A name written through one of the project's packages
//! is that package's.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};

use crate::types::{GoModule, ImportMapping, RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// The memos of one resolution pass.
#[derive(Debug, Default)]
pub struct GoTables {
    embeds: Mutex<HashMap<String, Arc<Vec<GoTypeRef>>>>,
    package_dirs: Mutex<HashMap<(String, String), Option<String>>>,
}

impl GoTables {
    pub fn new() -> Self {
        Self::default()
    }
}

/// A Go type a declaration names, with the directory of the package that
/// declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoTypeRef {
    pub name: String,
    pub pkg_dir: String,
}

/// Go builtin and predeclared types, which never carry a project method.
pub(crate) const GO_BUILTIN_FIELD_TYPES: [&str; 26] = [
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

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `^[A-Za-z_]\w*$` with ASCII `\w`.
fn is_identifier(text: &str) -> bool {
    text.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && text.bytes().all(is_word_byte)
}

/// The directory of a Go file, which is its package (`""` at the project
/// root; upstream `goPackageDir`).
pub fn package_dir_of(file_path: &str) -> String {
    crate::pathutil::dirname(&file_path.replace('\\', "/"))
}

/// Whether the go tool ignores a module directory: one under `testdata/` or a
/// directory named with a leading `_` or `.`.
fn is_ignored_module_dir(dir: &str) -> bool {
    dir.split('/').any(|segment| {
        segment == "testdata" || segment.starts_with('_') || segment.starts_with('.')
    })
}

/// The module of the nearest `go.mod` at or above `dir`.
fn nearest_module<'m>(dir: &str, modules: &'m [GoModule]) -> Option<&'m GoModule> {
    modules
        .iter()
        .filter(|module| {
            module.dir.is_empty()
                || dir == module.dir
                || dir.starts_with(&format!("{}/", module.dir))
        })
        .max_by_key(|module| module.dir.len())
}

/// The module an import path belongs to (upstream `findGoModuleForImport`):
/// the one whose module path equals it or is a `/`-bounded prefix of it, the
/// longest such path winning, as Go has it; two declaring the same path
/// prefer `own`, the importing file's module. A module the go tool ignores
/// answers only its own files.
fn owning_go_module<'m>(
    import_path: &str,
    modules: &'m [GoModule],
    own: Option<&GoModule>,
) -> Option<&'m GoModule> {
    let is_own = |module: &GoModule| own.is_some_and(|own| own.dir == module.dir);
    let mut best: Option<&GoModule> = None;
    for module in modules
        .iter()
        .filter(|module| !is_ignored_module_dir(&module.dir) || is_own(module))
    {
        if import_path != module.module_path
            && !import_path.starts_with(&format!("{}/", module.module_path))
        {
            continue;
        }
        let longer = best.is_none_or(|best| module.module_path.len() > best.module_path.len());
        let tie_to_own = best
            .is_some_and(|best| module.module_path.len() == best.module_path.len())
            && is_own(module);
        if longer || tie_to_own {
            best = Some(module);
        }
    }
    best
}

/// The project directory of `import_path` inside `module`: the module's own
/// directory followed by the rest of the import path.
fn package_dir_in(import_path: &str, module: &GoModule) -> String {
    let below = import_path
        .strip_prefix(&module.module_path)
        .unwrap_or_default()
        .trim_start_matches('/');
    match (module.dir.as_str(), below) {
        ("", rest) => rest.to_string(),
        (dir, "") => dir.to_string(),
        (dir, rest) => format!("{dir}/{rest}"),
    }
}

/// The project directory of the Go package `import_path` names, when it is a
/// package of one of the project's modules, else `None` (the standard
/// library, a third-party module, a project with no `go.mod`; upstream
/// `getGoPackageDir`). The module declaring the longest prefix of the path
/// takes it; two declaring the same path prefer `from_file`'s own.
pub fn go_import_package_dir(
    import_path: &str,
    from_file: Option<&str>,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let modules = context.get_go_modules();
    let own = from_file.and_then(|file| nearest_module(&package_dir_of(file), &modules));
    let key = (
        import_path.to_string(),
        own.map(|module| module.dir.clone()).unwrap_or_default(),
    );
    let tables = context.go_tables();
    if let Some(hit) = tables
        .package_dirs
        .lock()
        .ok()
        .and_then(|memo| memo.get(&key).cloned())
    {
        return hit;
    }
    let dir = owning_go_module(import_path, &modules, own)
        .map(|module| package_dir_in(import_path, module));
    if let Ok(mut memo) = tables.package_dirs.lock() {
        memo.insert(key, dir.clone());
    }
    dir
}

/// The project directory of the package `file_path` imports as `qualifier`
/// (upstream `goImportPackageDir`): `None` when the file has no such import,
/// `Some(None)` when the import is no package of the project's modules.
pub(crate) fn go_qualifier_package_dir(
    qualifier: &str,
    file_path: &str,
    context: &dyn ResolutionContext,
) -> Option<Option<String>> {
    let imports = context.get_import_mappings(file_path, Language::Go);
    let import = imports
        .iter()
        .find(|import| import.local_name == qualifier)?;
    Some(go_import_package_dir(
        &import.source,
        Some(file_path),
        context,
    ))
}

/// The package directory that declares a Go type spelled `raw` in
/// `file_path` (upstream `goDeclaredTypePackage`): the file's own for a bare
/// name (`server`, `*Store`), the imported package for a qualified one
/// (`store.Manager`) when it is a project package. `None` when that cannot be
/// told, which leaves resolution as it was.
pub(crate) fn go_declared_type_package(
    raw: Option<&str>,
    file_path: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let name: String = raw?
        .chars()
        .filter(|c| !matches!(c, '*' | '&') && !c.is_whitespace())
        .collect();
    match name.split_once('.') {
        None => Some(package_dir_of(file_path)),
        Some((qualifier, _)) => go_qualifier_package_dir(qualifier, file_path, context).flatten(),
    }
}

/// The identifier right before a trailing `.` of `prefix`, itself not
/// written after another word or `.` (upstream's
/// `/(?:^|[^\w.])([A-Za-z_]\w*)\.$/`).
fn qualifier_before(prefix: &str) -> Option<&str> {
    let head = prefix.strip_suffix('.')?;
    let start = head
        .bytes()
        .rposition(|b| !is_word_byte(b))
        .map_or(0, |at| at + 1);
    let qualifier = &head[start..];
    if !is_identifier(qualifier) {
        return None;
    }
    if head[..start].bytes().next_back() == Some(b'.') {
        return None;
    }
    Some(qualifier)
}

/// Whether `name` is written on `line` with nothing before it but a non-word
/// character other than `.` (upstream's `(?<![\w.])name\b`).
fn written_bare(line: &str, name: &str) -> bool {
    line.match_indices(name).any(|(at, _)| {
        let before_ok = line[..at]
            .bytes()
            .next_back()
            .is_none_or(|b| !is_word_byte(b) && b != b'.');
        let after_ok = line[at + name.len()..]
            .bytes()
            .next()
            .is_none_or(|b| !is_word_byte(b));
        before_ok && after_ok
    })
}

/// The first qualifier `name` is written through on `line` (upstream's
/// `/(?:^|[^\w.])([A-Za-z_]\w*)\.name\b/`).
fn first_qualifier_of<'l>(line: &'l str, name: &str) -> Option<&'l str> {
    let dotted = format!(".{name}");
    line.match_indices(&dotted).find_map(|(at, _)| {
        let after_ok = line[at + dotted.len()..]
            .bytes()
            .next()
            .is_none_or(|b| !is_word_byte(b));
        if !after_ok {
            return None;
        }
        qualifier_before(&line[..=at])
    })
}

/// The package name a Go reference is written through, as spelled — the
/// operand of the selector it is written as, or else the qualifier right
/// before the name at the reference's column, or the line's only spelling of
/// the name — read from its line, since the index keeps only the name
/// (upstream `goRefQualification`'s `written`, #2397, #2448).
fn go_written_qualifier(reference: &RefView, context: &dyn ResolutionContext) -> Option<String> {
    if reference.language != Language::Go || reference.reference_kind == EdgeKind::Imports {
        return None;
    }
    let name = reference.reference_name.rsplit('.').next()?;
    if !is_identifier(name) {
        return None;
    }
    let facts = context.source_facts(&reference.file_path)?;
    let index = usize::try_from(reference.line.checked_sub(1)?)
        .ok()
        .filter(|&index| index < facts.line_count())?;
    let line = facts.line(index);
    let at = usize::try_from(reference.column.max(0)).ok()?;
    if let Some(written) = dotted_qualifier(&reference.reference_name, line, at, name) {
        return written.map(str::to_string);
    }
    if line.get(at..).is_some_and(|rest| rest.starts_with(name)) {
        qualifier_before(&line[..at]).map(str::to_string)
    } else if !written_bare(line, name) {
        first_qualifier_of(line, name).map(str::to_string)
    } else {
        None
    }
}

/// What a dotted Go reference is written through: upstream records a
/// selector of one or two hops (`pkg.F`, `x.f.M`) dotted, and only its first
/// segment can be a package, whatever else the line writes — `perr` in
/// `klog.Error(perr.Error())`, `cmp` in `cmp.Diff(…)` beside a `Diff:`
/// (upstream #2448). The extractor keeps a chain's last receiver segment
/// (#1496), so the operand is read back at the column. A chain of more hops,
/// which upstream records bare, is read as a bare name is (`None`); a chain
/// whose root is no plain name — a call's result (`Chain(…).Handler`,
/// `makeOuter().bar.Run`), a conversion, an index — has no package.
fn dotted_qualifier<'l>(
    reference_name: &str,
    line: &'l str,
    column: usize,
    name: &str,
) -> Option<Option<&'l str>> {
    reference_name.split_once('.')?;
    let at_start = line
        .get(..column)
        .is_some_and(|before| !before.trim_end_matches([' ', '\t']).ends_with('.'));
    if at_start {
        let (links, _) = selector_chain(line, column);
        if links.last() == Some(&name) {
            match links.len() {
                2 | 3 => return Some(Some(links[0])),
                0 | 1 => {}
                _ => return None,
            }
        }
    }
    Some(None)
}

/// The import a Go reference is written through — `context` in
/// `context.Context`, `store` in `store.Manager` (upstream `goRefQualifier`).
/// `None` for a name written bare, or through anything that is not one of
/// the file's imports.
pub(crate) fn go_ref_qualifier(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ImportMapping> {
    let written = go_written_qualifier(reference, context)?;
    context
        .get_import_mappings(&reference.file_path, Language::Go)
        .into_iter()
        .find(|import| import.local_name == written)
}

/// Whether a Go reference is written through a package that is none of its
/// file's imports as the index knows them (upstream `isGoUnknownQualified`,
/// #2397): `yaml` in `yaml.Node` under an unaliased `import
/// "go.yaml.in/yaml/v3"`, recorded under its last path element. Which
/// package that is cannot be told from here.
pub(crate) fn is_go_unknown_qualified(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    go_written_qualifier(reference, context).is_some_and(|written| {
        !context
            .get_import_mappings(&reference.file_path, Language::Go)
            .iter()
            .any(|import| import.local_name == written)
    })
}

/// Whether a Go reference is written through an imported package from
/// outside the project's modules — `context.Context`, `fmt.Errorf`, a
/// third-party `gin.H` (upstream `isGoExternalQualified`, #2233, with
/// #2361's package directories).
pub(crate) fn is_go_external_qualified(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    let Some(import) = go_ref_qualifier(reference, context) else {
        return false;
    };
    !(import.source.starts_with('.')
        || import.source.contains("/internal/")
        || go_import_package_dir(&import.source, Some(&reference.file_path), context).is_some())
}

/// Whether `candidate` lives in the project package a Go reference is written
/// through, when it is one (upstream `isInGoQualifierPackage`):
/// `store.Manager` names the `Manager` of the directory the `store` import
/// maps to, never a same-named symbol of another package (#2322).
pub(crate) fn is_in_go_qualifier_package(
    candidate: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    // A package never holds a method: a call or method value landing on one
    // is made through a variable that shadows the import — prometheus's
    // `labels := &maxHeap{}`, then `labels.get()`.
    if candidate.kind == NodeKind::Method
        && (reference.reference_kind == EdgeKind::Calls || reference.is_function_ref)
    {
        return true;
    }
    let Some(import) = go_ref_qualifier(reference, context) else {
        return true;
    };
    go_import_package_dir(&import.source, Some(&reference.file_path), context)
        .is_none_or(|dir| package_dir_of(&candidate.file_path) == dir)
}

/// The node kinds a Go `type` declaration produces (upstream `GO_TYPE_KINDS`).
fn is_go_type_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Struct | NodeKind::Interface | NodeKind::TypeAlias
    )
}

/// The declarations of the Go type `type_name` in the package at `pkg_dir`.
fn package_types(
    type_name: &str,
    pkg_dir: &str,
    context: &dyn ResolutionContext,
) -> Vec<Arc<Node>> {
    context
        .get_nodes_by_name_shared(type_name)
        .into_iter()
        .filter(|node| {
            node.language == Language::Go
                && is_go_type_kind(node.kind)
                && package_dir_of(&node.file_path) == pkg_dir
        })
        .collect()
}

/// A Go declaration line without its `//` and single-line `/* */` comments.
pub(crate) fn strip_go_line_comments(line: &str) -> String {
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

/// What a Go struct or interface declaration's text embeds, as written: each
/// member that is nothing but a type (`BaseAPI`, `*cached.BaseManager`,
/// `ReadView`, optionally tagged), one per line or `;`-separated, as
/// `(qualifier, name)`.
pub fn written_embeddings(declaration: &str) -> Vec<(Option<String>, String)> {
    let (Some(open), Some(close)) = (declaration.find('{'), declaration.rfind('}')) else {
        return Vec::new();
    };
    if close <= open {
        return Vec::new();
    }
    let mut body = declaration[open + 1..close].to_string();
    // Members of a nested anonymous struct are not this type's.
    while let Some(inner) = innermost_braces(&body) {
        body.replace_range(inner, "");
    }
    body.split(['\n', ';'])
        .filter_map(embedded_member)
        .collect()
}

/// The range of the first `{…}` with no brace inside (upstream's
/// `/\{[^{}]*\}/`).
fn innermost_braces(text: &str) -> Option<std::ops::Range<usize>> {
    let mut open = None;
    for (at, byte) in text.bytes().enumerate() {
        match byte {
            b'{' => open = Some(at),
            b'}' => {
                if let Some(start) = open {
                    return Some(start..at + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// A struct or interface member that is nothing but a type (upstream's
/// `/^\s*\*?\s*([A-Za-z_]\w*)(?:\.([A-Za-z_]\w*))?\s*(?:\[[^\]]*\])?\s*(?:`[^`]*`|"[^"]*")?\s*$/`).
fn embedded_member(member: &str) -> Option<(Option<String>, String)> {
    let mut rest = member.trim_start();
    rest = rest.strip_prefix('*').unwrap_or(rest).trim_start();
    let first_end = rest
        .bytes()
        .position(|b| !is_word_byte(b))
        .unwrap_or(rest.len());
    let first = &rest[..first_end];
    if !is_identifier(first) {
        return None;
    }
    rest = &rest[first_end..];
    let mut qualifier = None;
    let mut name = first.to_string();
    if let Some(after) = rest.strip_prefix('.') {
        let end = after
            .bytes()
            .position(|b| !is_word_byte(b))
            .unwrap_or(after.len());
        let second = &after[..end];
        if !is_identifier(second) {
            return None;
        }
        qualifier = Some(first.to_string());
        name = second.to_string();
        rest = &after[end..];
    }
    rest = rest.trim_start();
    if let Some(after) = rest.strip_prefix('[') {
        let close = after.find(']')?;
        rest = after[close + 1..].trim_start();
    }
    for quote in ['`', '"'] {
        if let Some(after) = rest.strip_prefix(quote) {
            let close = after.find(quote)?;
            rest = after[close + 1..].trim_start();
            break;
        }
    }
    rest.trim().is_empty().then_some((qualifier, name))
}

/// The project types a Go struct or interface embeds, with the directory of
/// the package that declares each (upstream `goEmbeddedTypes`). An embedded
/// type from outside the project's packages (`suite.Suite`, `sync.Mutex`)
/// has no project methods and is left out, as is one whose package cannot be
/// told.
pub(crate) fn embedded_types(
    type_node: &Node,
    context: &dyn ResolutionContext,
) -> Arc<Vec<GoTypeRef>> {
    let tables = context.go_tables();
    if let Some(hit) = tables
        .embeds
        .lock()
        .ok()
        .and_then(|memo| memo.get(&type_node.id).cloned())
    {
        return hit;
    }
    let mut out = Vec::new();
    if let Some(facts) = context.source_facts(&type_node.file_path) {
        let start = usize::try_from(type_node.start_line.saturating_sub(1)).unwrap_or(0);
        let end = usize::try_from(type_node.end_line.max(type_node.start_line))
            .unwrap_or(0)
            .min(facts.line_count());
        let declaration: Vec<String> = (start..end)
            .map(|index| strip_go_line_comments(facts.line(index)))
            .collect();
        for (qualifier, name) in written_embeddings(&declaration.join("\n")) {
            let pkg_dir = match qualifier {
                Some(qualifier) => {
                    go_qualifier_package_dir(&qualifier, &type_node.file_path, context).flatten()
                }
                None => Some(package_dir_of(&type_node.file_path)),
            };
            if let Some(pkg_dir) = pkg_dir {
                out.push(GoTypeRef { name, pkg_dir });
            }
        }
    }
    let out = Arc::new(out);
    if let Ok(mut memo) = tables.embeds.lock() {
        memo.insert(type_node.id.clone(), Arc::clone(&out));
    }
    out
}

/// `method_name` on the Go type `type_name` the package at `pkg_dir`
/// declares (upstream `resolveGoMethodInPackage`, #2323): only that
/// package's `<type_name>::<method_name>` counts, then the types its
/// declaration embeds. `None` when the package declares no such type (a dot
/// import, a file that is not indexed): the caller then resolves by name as
/// before. `Some(None)` is no edge.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_go_method_in_package(
    type_name: &str,
    method_name: &str,
    pkg_dir: &str,
    matches: &[Arc<Node>],
    reference: &RefView,
    context: &dyn ResolutionContext,
    confidence: f64,
    resolved_by: ResolvedBy,
    depth: u32,
) -> Option<Option<ResolvedRef>> {
    let own: Vec<Arc<Node>> = matches
        .iter()
        .filter(|method| package_dir_of(&method.file_path) == pkg_dir)
        .cloned()
        .collect();
    if !own.is_empty() {
        if reference.is_function_ref && own.len() != 1 {
            return Some(None);
        }
        let ordered = crate::name_matcher::prefer_call_site_file(own, &reference.file_path);
        return Some(Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: ordered[0].id.clone(),
            confidence,
            resolved_by,
        }));
    }
    let types = package_types(type_name, pkg_dir, context);
    if types.is_empty() {
        return None;
    }
    if depth >= 4 {
        return Some(None);
    }
    let mut unplaced = 0;
    for declaration in &types {
        // An alias is the type it names, and has that type's methods
        // (upstream #2417).
        if let Some(aliased) = alias_target(declaration, context) {
            match aliased {
                Some(target) => {
                    if let Some(via) = crate::name_matcher::resolve_method_on_type(
                        &target.name,
                        method_name,
                        reference,
                        context,
                        confidence,
                        resolved_by,
                        Some(&target.pkg_dir),
                        depth + 1,
                    ) {
                        return Some(Some(via));
                    }
                }
                None => unplaced += 1,
            }
            continue;
        }
        for embedded in embedded_types(declaration, context).iter() {
            if let Some(via) = crate::name_matcher::resolve_method_on_type(
                &embedded.name,
                method_name,
                reference,
                context,
                confidence,
                resolved_by,
                Some(&embedded.pkg_dir),
                depth + 1,
            ) {
                return Some(Some(via));
            }
        }
    }
    // An alias of a type from outside the project (`type Ctx =
    // context.Context`) or of no named type declares nothing here: the method
    // is looked up by name, as it was before aliases had nodes.
    if unplaced == types.len() {
        return None;
    }
    Some(None)
}

/// The type a Go alias names, with the directory of the package that
/// declares it (upstream `goAliasTarget`, #2417): `mvccpb.Event` for `type
/// Event = mvccpb.Event`, `Local` for `type Ptr = *Local`, `List` for `type
/// Items[T any] = List[T]`, read from the declaration as an embedding is.
/// `Some(None)` for an alias of anything else: a type from outside the
/// project's packages, a predeclared one, a `func(…)` or `map[…]…`. `None`
/// when the node is no alias: a struct, an interface, or a defined type
/// (`type Dur int`), which declares a type of its own.
fn alias_target(type_node: &Node, context: &dyn ResolutionContext) -> Option<Option<GoTypeRef>> {
    if type_node.kind != NodeKind::TypeAlias {
        return None;
    }
    let facts = context.source_facts(&type_node.file_path)?;
    let start = usize::try_from(type_node.start_line.saturating_sub(1)).unwrap_or(0);
    let end = usize::try_from(type_node.end_line.max(type_node.start_line))
        .unwrap_or(0)
        .min(facts.line_count());
    let column = usize::try_from(type_node.start_column.max(0)).unwrap_or(0);
    // From the alias's name, where the node starts, to the end of its type.
    let declaration = (start..end)
        .map(|index| {
            let line = facts.line(index);
            let line = if index == start {
                line.get(column..).unwrap_or_default()
            } else {
                line
            };
            strip_go_line_comments(line)
        })
        .collect::<Vec<_>>()
        .join(" ");
    let rest = &declaration[alias_head_end(&declaration)?..];
    Some(alias_named_type(rest).and_then(|(qualifier, name)| {
        let pkg_dir = match &qualifier {
            Some(qualifier) => {
                go_qualifier_package_dir(qualifier, &type_node.file_path, context).flatten()
            }
            None => Some(package_dir_of(&type_node.file_path)),
        }?;
        (qualifier.is_some() || !GO_BUILTIN_FIELD_TYPES.contains(&name.as_str()))
            .then_some(GoTypeRef { name, pkg_dir })
    }))
}

/// Where the type of an alias declaration starts, past `Name =` or `Name[T
/// any] =` (upstream's `/^\s*[A-Za-z_]\w*\s*(?:\[[^\]]*\])?\s*=/`); `None` for
/// a defined type, which has no `=` there.
fn alias_head_end(declaration: &str) -> Option<usize> {
    let mut at = declaration.len() - declaration.trim_start().len();
    let bytes = declaration.as_bytes();
    if at >= bytes.len() || !(bytes[at].is_ascii_alphabetic() || bytes[at] == b'_') {
        return None;
    }
    while at < bytes.len() && is_word_byte(bytes[at]) {
        at += 1;
    }
    while at < bytes.len() && bytes[at].is_ascii_whitespace() {
        at += 1;
    }
    if at < bytes.len() && bytes[at] == b'[' {
        let close = declaration[at..].find(']')?;
        at += close + 1;
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
    }
    (at < bytes.len() && bytes[at] == b'=').then_some(at + 1)
}

/// The named type an alias's right-hand side is: `T`, `*T` or `pkg.T`, each
/// perhaps with type arguments (upstream's
/// `/^\s*(?:\*\s*)?([A-Za-z_]\w*)(?:\s*\.\s*([A-Za-z_]\w*))?\s*(?:\[.*\])?\s*;?\s*$/`),
/// as `(qualifier, name)`.
fn alias_named_type(text: &str) -> Option<(Option<String>, String)> {
    let mut rest = text.trim_start();
    if let Some(after) = rest.strip_prefix('*') {
        rest = after.trim_start();
    }
    let word = |text: &str| -> usize {
        text.bytes()
            .position(|b| !is_word_byte(b))
            .unwrap_or(text.len())
    };
    let first_end = word(rest);
    let first = &rest[..first_end];
    if !is_identifier(first) {
        return None;
    }
    rest = &rest[first_end..];
    let mut result = (None, first.to_string());
    let trimmed = rest.trim_start();
    if let Some(after) = trimmed.strip_prefix('.') {
        let after = after.trim_start();
        let end = word(after);
        let second = &after[..end];
        if !is_identifier(second) {
            return None;
        }
        result = (Some(first.to_string()), second.to_string());
        rest = &after[end..];
    }
    let mut tail = rest.trim();
    if tail.starts_with('[') {
        // `(?:\[.*\])?`: a bracketed run to the last `]`.
        let close = tail.rfind(']')?;
        tail = tail[close + 1..].trim();
    }
    let tail = tail.strip_suffix(';').unwrap_or(tail).trim();
    tail.is_empty().then_some(result)
}

/// The type a field of `declaration` (a Go struct) is declared with, as
/// written (upstream's `\bfield\s+\*?\[?\]?([A-Za-z_][\w.]*)`, read line by
/// line with comments stripped).
pub fn written_field_type(declaration_lines: &[String], field: &str) -> Option<String> {
    for line in declaration_lines {
        let mut from = 0;
        while let Some(found) = line[from..].find(field) {
            let at = from + found;
            from = at + field.len();
            if line[..at].bytes().next_back().is_some_and(is_word_byte) {
                continue;
            }
            let mut rest = &line[at + field.len()..];
            if rest.bytes().next().is_some_and(is_word_byte) {
                continue;
            }
            let trimmed = rest.trim_start();
            if trimmed.len() == rest.len() {
                continue;
            }
            rest = trimmed;
            rest = rest.strip_prefix('*').unwrap_or(rest);
            rest = rest.strip_prefix('[').unwrap_or(rest);
            rest = rest.strip_prefix(']').unwrap_or(rest);
            if !rest.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
                continue;
            }
            let end = rest
                .bytes()
                .position(|b| !is_word_byte(b) && b != b'.')
                .unwrap_or(rest.len());
            return Some(rest[..end].to_string());
        }
    }
    None
}

/// A Go call through a two-hop field chain, `base.field.method()`, which the
/// extractor records by its last receiver segment (`field.method`, KEEP-RUST
/// #1496) at the column where the chain starts: the chain read back from the
/// line, `(base, field)`, when it is that and nothing longer or shorter
/// (upstream emits `base.field.method` for exactly this receiver shape,
/// #1276).
pub(crate) fn go_call_chain(
    reference: &RefView,
    receiver: &str,
    method: &str,
    context: &dyn ResolutionContext,
) -> Option<(String, String)> {
    if reference.language != Language::Go || !is_identifier(receiver) {
        return None;
    }
    let facts = context.source_facts(&reference.file_path)?;
    let index = usize::try_from(reference.line.checked_sub(1)?)
        .ok()
        .filter(|&index| index < facts.line_count())?;
    let line = facts.line(index);
    let column = usize::try_from(reference.column.max(0)).ok()?;
    let (links, end) = selector_chain(line, column);
    let [base, field, called] = links.as_slice() else {
        return None;
    };
    (*field == receiver && *called == method && line[end..].trim_start().starts_with('('))
        .then(|| (base.to_string(), field.to_string()))
}

/// Whether a Go reference `receiver.member` is a link inside a longer
/// selector chain, `s.db.member`, whose receiver is a field rather than the
/// chain's operand: no package qualifies it.
pub(crate) fn is_go_chain_link(reference: &RefView, context: &dyn ResolutionContext) -> bool {
    let Some((receiver, _)) = reference.reference_name.split_once('.') else {
        return false;
    };
    let Some(facts) = context.source_facts(&reference.file_path) else {
        return false;
    };
    let Some(index) = reference
        .line
        .checked_sub(1)
        .and_then(|line| usize::try_from(line).ok())
        .filter(|&index| index < facts.line_count())
    else {
        return false;
    };
    let line = facts.line(index);
    let Ok(column) = usize::try_from(reference.column.max(0)) else {
        return false;
    };
    let (links, _) = selector_chain(line, column);
    links.len() >= 3 && links.first().is_some_and(|operand| *operand != receiver)
}

/// The names of the selector chain a Go expression at `column` of `line`
/// starts with — its operand, then each `.name` — and where it ends.
fn selector_chain(line: &str, column: usize) -> (Vec<&str>, usize) {
    let bytes = line.as_bytes();
    let word_end = |start: usize| {
        let mut end = start;
        while end < bytes.len() && is_word_byte(bytes[end]) {
            end += 1;
        }
        end
    };
    let starts_word =
        |at: usize| at < bytes.len() && (bytes[at].is_ascii_alphabetic() || bytes[at] == b'_');
    if !starts_word(column) || (column > 0 && is_word_byte(bytes[column - 1])) {
        return (Vec::new(), column);
    }
    let mut end = word_end(column);
    let mut links = vec![&line[column..end]];
    loop {
        let mut at = end;
        while at < bytes.len() && matches!(bytes[at], b' ' | b'\t') {
            at += 1;
        }
        if at >= bytes.len() || bytes[at] != b'.' {
            break;
        }
        at += 1;
        while at < bytes.len() && matches!(bytes[at], b' ' | b'\t') {
            at += 1;
        }
        if !starts_word(at) {
            break;
        }
        let next = word_end(at);
        links.push(&line[at..next]);
        end = next;
    }
    (links, end)
}

/// Resolve a Go two-hop field chain `base.field.method` (upstream
/// `matchGoFieldChainCall`, #1276, scoped by package since #2361): `base`'s
/// type from the enclosing scope, `field`'s declared type from that struct's
/// own declaration lines (the declaring package's struct first), and the
/// method validated on the field's type in the package declaring it. A field
/// whose type has no project node (`sql.DB`) links nothing.
pub(crate) fn go_field_chain_call(
    base: &str,
    field: &str,
    method: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
    confidence: f64,
    resolved_by: ResolvedBy,
) -> Option<ResolvedRef> {
    let (base_type, written) =
        crate::name_matcher::infer_local_receiver_declaration(base, reference, context)?;
    let mut structs: Vec<Arc<Node>> = context
        .get_nodes_by_name_shared(&base_type)
        .into_iter()
        .filter(|node| {
            matches!(node.kind, NodeKind::Struct | NodeKind::Class) && node.language == Language::Go
        })
        .collect();
    // The struct the declaring package has is the one the base's type means:
    // Go type names repeat across packages, and another package's
    // same-named struct with a same-named field is not this one (#2323).
    if let Some(base_pkg) = go_declared_type_package(Some(&written), &reference.file_path, context)
    {
        let declared: Vec<Arc<Node>> = structs
            .iter()
            .filter(|node| package_dir_of(&node.file_path) == base_pkg)
            .cloned()
            .collect();
        if !declared.is_empty() {
            structs = declared;
        }
    }
    for declaration in crate::name_matcher::prefer_call_site_file(structs, &reference.file_path) {
        let Some(facts) = context.source_facts(&declaration.file_path) else {
            continue;
        };
        let start = usize::try_from(declaration.start_line.saturating_sub(1)).unwrap_or(0);
        let end = usize::try_from(declaration.end_line.max(0))
            .unwrap_or(0)
            .min(facts.line_count());
        let lines: Vec<String> = (start..end)
            .map(|index| strip_go_line_comments(facts.line(index)))
            .collect();
        for line in &lines {
            let Some(raw_type) = written_field_type(std::slice::from_ref(line), field) else {
                continue;
            };
            // The package declaring the field's type: the struct's own for a
            // bare name, the imported one for a package-qualified name, which
            // is followed only when it is one of the project's packages.
            let pkg_dir = match raw_type.split_once('.') {
                Some((qualifier, _)) => {
                    match go_qualifier_package_dir(qualifier, &declaration.file_path, context)
                        .flatten()
                    {
                        Some(dir) => dir,
                        None => continue,
                    }
                }
                None => package_dir_of(&declaration.file_path),
            };
            let Some(type_name) = raw_type.rsplit('.').next() else {
                continue;
            };
            if !type_name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                || GO_BUILTIN_FIELD_TYPES.contains(&type_name)
            {
                continue;
            }
            if let Some(resolved) = crate::name_matcher::resolve_method_on_type(
                type_name,
                method,
                reference,
                context,
                confidence,
                resolved_by,
                Some(&pkg_dir),
                0,
            ) {
                return Some(resolved);
            }
        }
    }
    None
}

/// Key prefix of the Go head rows (W4-GO-01): one `project_metadata` row per
/// Go file with a type declaration, written by every full index and
/// migration and kept current by sync.
pub const HEAD_ROW_KEY_PREFIX: &str = "go_heads:";

/// The `project_metadata` key of `file_path`'s head row.
pub fn head_row_key(file_path: &str) -> String {
    format!("{HEAD_ROW_KEY_PREFIX}{file_path}")
}

/// One Go type declaration as resolution reads its text: what it embeds (by
/// name), every name it mentions, a digest of the text itself, so a sync
/// tells a changed field type from an unchanged one, and the import path
/// each package qualifier it writes (`dep` in `dep.Manager`) binds in its
/// file, so a sync tells a retargeted import too.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct HeadEntry {
    pub name: String,
    pub embeds: Vec<String>,
    pub mentions: Vec<String>,
    pub digest: String,
    /// `qualifier=import path`, sorted.
    pub imports: Vec<String>,
}

/// A Go file's head row: every struct, interface and type alias, sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HeadRow {
    pub entries: Vec<HeadEntry>,
}

impl HeadRow {
    /// The row of a file whose text is `source` and whose nodes are `nodes`.
    pub fn of(source: &str, nodes: &[Node]) -> Self {
        let lines: Vec<&str> = source.lines().collect();
        let file_imports = crate::import_resolver::extract_import_mappings(source, Language::Go);
        let mut entries: Vec<HeadEntry> = nodes
            .iter()
            .filter(|node| node.language == Language::Go && is_go_type_kind(node.kind))
            .map(|node| {
                let start = usize::try_from(node.start_line.saturating_sub(1)).unwrap_or(0);
                let end = usize::try_from(node.end_line.max(node.start_line))
                    .unwrap_or(0)
                    .min(lines.len());
                let declaration = lines
                    .get(start..end.max(start))
                    .unwrap_or_default()
                    .iter()
                    .map(|line| strip_go_line_comments(line))
                    .collect::<Vec<_>>()
                    .join("\n");
                let mut embeds: Vec<String> = written_embeddings(&declaration)
                    .into_iter()
                    .map(|(_, name)| name)
                    .collect();
                embeds.sort();
                embeds.dedup();
                let mut imports: Vec<String> = written_qualifiers(&declaration)
                    .into_iter()
                    .filter_map(|qualifier| {
                        file_imports
                            .iter()
                            .find(|import| import.local_name == qualifier)
                            .map(|import| format!("{qualifier}={}", import.source))
                    })
                    .collect();
                imports.sort();
                imports.dedup();
                HeadEntry {
                    name: node.name.clone(),
                    embeds,
                    mentions: mentioned_names(&declaration),
                    digest: format!("{:016x}", fnv1a(declaration.as_bytes())),
                    imports,
                }
            })
            .collect();
        entries.sort();
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("go head row encodes")
    }

    pub fn decode(value: &str) -> Option<Self> {
        serde_json::from_str(value).ok()
    }
}

/// The identifiers a declaration's text writes right before a `.` and
/// another identifier: the package qualifiers of its `pkg.Type` names.
fn written_qualifiers(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if !(bytes[at].is_ascii_alphabetic() || bytes[at] == b'_')
            || (at > 0 && (is_word_byte(bytes[at - 1]) || bytes[at - 1] == b'.'))
        {
            at += 1;
            continue;
        }
        let mut end = at;
        while end < bytes.len() && is_word_byte(bytes[end]) {
            end += 1;
        }
        if end + 1 < bytes.len()
            && bytes[end] == b'.'
            && (bytes[end + 1].is_ascii_alphabetic() || bytes[end + 1] == b'_')
        {
            out.push(&text[at..end]);
        }
        at = end;
    }
    out
}

/// Every identifier a declaration's text holds, sorted, the predeclared
/// types aside.
fn mentioned_names(text: &str) -> Vec<String> {
    let mut names: Vec<String> = text
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|word| is_identifier(word) && !GO_BUILTIN_FIELD_TYPES.contains(word))
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The 64-bit FNV-1a hash: a digest stable across builds.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(path: &str, dir: &str) -> GoModule {
        GoModule {
            module_path: path.to_string(),
            dir: dir.to_string(),
        }
    }

    #[test]
    fn owning_go_module_prefers_the_longest_matching_module_path() {
        let modules = vec![module("ex/a", "a"), module("ex/a/sub", "a/sub")];
        assert_eq!(
            owning_go_module("ex/a/sub/pkg", &modules, None).map(|m| m.dir.as_str()),
            Some("a/sub"),
            "the nested module must win over its prefix"
        );
        assert_eq!(
            owning_go_module("ex/a/other", &modules, None).map(|m| m.dir.as_str()),
            Some("a")
        );
        assert!(owning_go_module("github.com/x/y", &modules, None).is_none());
    }

    #[test]
    fn owning_go_module_rejects_a_shared_path_prefix_that_is_not_a_segment() {
        let modules = vec![module("ex/a", "a")];
        assert!(
            owning_go_module("ex/abc", &modules, None).is_none(),
            "`ex/abc` is a different module, not a package inside `ex/a`"
        );
    }

    #[test]
    fn owning_go_module_breaks_a_tie_to_the_own_module_and_skips_ignored_ones() {
        let modules = vec![
            module("ex/svc", "internal/testdata/stub"),
            module("ex/svc", "v1"),
            module("ex/svc", "v2"),
        ];
        assert_eq!(
            owning_go_module("ex/svc/pkg", &modules, None).map(|m| m.dir.as_str()),
            Some("v1")
        );
        assert_eq!(
            owning_go_module("ex/svc/pkg", &modules, Some(&modules[2])).map(|m| m.dir.as_str()),
            Some("v2")
        );
        // The testdata module answers only its own files.
        assert_eq!(
            owning_go_module("ex/svc/pkg", &modules[..1], None).map(|m| m.dir.as_str()),
            None
        );
        assert_eq!(
            owning_go_module("ex/svc/pkg", &modules[..1], Some(&modules[0]))
                .map(|m| m.dir.as_str()),
            Some("internal/testdata/stub")
        );
    }

    #[test]
    fn go_package_dir_joins_module_dir_with_the_path_below_it() {
        assert_eq!(package_dir_in("ex/a", &module("ex/a", "a")), "a");
        assert_eq!(package_dir_in("ex/a/pkg", &module("ex/a", "a")), "a/pkg");
        assert_eq!(package_dir_in("ex/m/pkg", &module("ex/m", "")), "pkg");
        assert_eq!(
            package_dir_in("ex/m", &module("ex/m", "")),
            "",
            "a root module's own package lives in the project root"
        );
    }

    #[test]
    fn a_row_holds_each_type_with_its_embeddings_mentions_and_digest() {
        let text = "package h\n\nimport dep \"example.com/app/store1\"\n\ntype server struct {\n\tBaseAPI // the base\n\tstore *store\n\tdb *dep.Manager\n}\n\ntype store struct{}\n";
        let node = |name: &str, kind: NodeKind, lines: (i64, i64)| Node {
            id: format!("{}:{name}", kind.as_str()),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: "h/server.go".to_string(),
            language: Language::Go,
            start_line: lines.0,
            end_line: lines.1,
            start_column: 5,
            end_column: 1,
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
        };
        let nodes = [
            node("server", NodeKind::Struct, (5, 9)),
            node("store", NodeKind::Struct, (11, 11)),
        ];
        let row = HeadRow::of(text, &nodes);
        assert_eq!(row.entries.len(), 2);
        assert_eq!(row.entries[0].name, "server");
        assert_eq!(row.entries[0].embeds, ["BaseAPI"]);
        assert_eq!(
            row.entries[0].mentions,
            ["BaseAPI", "Manager", "db", "dep", "server", "store", "type"]
        );
        assert_eq!(row.entries[0].imports, ["dep=example.com/app/store1"]);
        let retyped = HeadRow::of(&text.replace("store *store", "store *cache"), &nodes);
        assert_ne!(retyped.entries[0].digest, row.entries[0].digest);
        // The same declaration through a retargeted import is another entry.
        let retargeted = HeadRow::of(&text.replace("store1", "store2"), &nodes);
        assert_eq!(retargeted.entries[0].digest, row.entries[0].digest);
        assert_ne!(retargeted.entries[0], row.entries[0]);
        assert_eq!(HeadRow::decode(&row.encode()), Some(row));
    }

    #[test]
    fn a_dotted_reference_is_written_through_its_operand() {
        let line = "\tklog.Error(perr.Error())";
        assert_eq!(
            dotted_qualifier("klog.Error", line, 1, "Error"),
            Some(Some("klog"))
        );
        assert_eq!(
            dotted_qualifier("perr.Error", line, 12, "Error"),
            Some(Some("perr"))
        );
        // Two hops, kept as their last receiver segment.
        let line = "\t\tzap.String(\"id\", t.ID.String()),";
        assert_eq!(
            dotted_qualifier("ID.String", line, 19, "String"),
            Some(Some("t"))
        );
        // Three hops are recorded bare upstream: read as a bare name.
        let line = "\te.cfg.logger.Error(\"x\", zap.Error(err))";
        assert_eq!(dotted_qualifier("logger.Error", line, 1, "Error"), None);
        // A factory call's result has no package; a bare name is read from
        // its line.
        // The root of the chain is a call's result: no package, whatever the
        // stored name's head is.
        assert_eq!(
            dotted_qualifier("bar.Run", "\tmakeOuter().bar.Run()", 1, "Run"),
            Some(None)
        );
        let line = "\tr.Mount(\"/w\", Chain(func(next http.Handler) http.Handler {";
        assert_eq!(
            dotted_qualifier("Chain().Handler", line, 15, "Handler"),
            Some(None)
        );
        assert_eq!(
            dotted_qualifier("Message", "func f(src proto.Message)", 17, "Message"),
            None
        );
    }

    #[test]
    fn an_alias_names_one_type_or_none() {
        assert_eq!(alias_head_end("Event = mvccpb.Event"), Some(7));
        assert_eq!(alias_head_end("Items[T any] = List[T]"), Some(14));
        assert_eq!(alias_head_end("Dur int"), None);
        assert_eq!(
            alias_named_type(" mvccpb.Event"),
            Some((Some("mvccpb".to_string()), "Event".to_string()))
        );
        assert_eq!(
            alias_named_type(" *Local"),
            Some((None, "Local".to_string()))
        );
        assert_eq!(
            alias_named_type(" List[T]"),
            Some((None, "List".to_string()))
        );
        assert_eq!(alias_named_type(" map[string]*KeyValue"), None);
        assert_eq!(alias_named_type(" func(ctx context.Context) error"), None);
        assert_eq!(alias_named_type(" []WatchResponse"), None);
    }

    #[test]
    fn a_module_dir_the_go_tool_ignores_is_told() {
        assert!(is_ignored_module_dir("internal/testdata/stub"));
        assert!(is_ignored_module_dir("_examples/x"));
        assert!(is_ignored_module_dir(".hidden"));
        assert!(!is_ignored_module_dir(""));
        assert!(!is_ignored_module_dir("tools/lint"));
    }

    #[test]
    fn the_nearest_module_is_the_deepest_one_above() {
        let modules = [
            module("ex/root", ""),
            module("ex/svc", "svc"),
            module("ex/svc2", "svc2"),
        ];
        assert_eq!(
            nearest_module("svc/internal", &modules).map(|m| m.dir.as_str()),
            Some("svc")
        );
        assert_eq!(
            nearest_module("svc2", &modules).map(|m| m.dir.as_str()),
            Some("svc2")
        );
        assert_eq!(
            nearest_module("web", &modules).map(|m| m.dir.as_str()),
            Some("")
        );
        assert_eq!(
            package_dir_in("ex/svc/internal/store", &modules[1]),
            "svc/internal/store"
        );
        assert_eq!(package_dir_in("ex/root", &modules[0]), "");
    }

    #[test]
    fn a_qualifier_is_read_before_the_name_or_from_the_line() {
        assert_eq!(qualifier_before("\tdb *store."), Some("store"));
        assert_eq!(qualifier_before("x.store."), None);
        assert_eq!(qualifier_before("("), None);
        assert!(written_bare("func f(c Context) {", "Context"));
        assert!(!written_bare("func f(c context.Context) {", "Context"));
        assert_eq!(
            first_qualifier_of("func f(c context.Context) {", "Context"),
            Some("context")
        );
        assert_eq!(first_qualifier_of("a.b.Context", "Context"), None);
    }

    #[test]
    fn embeddings_are_the_members_that_are_only_a_type() {
        let declaration = "type server struct {\n\tBaseAPI\n\t*cached.BaseManager `json:\"-\"`\n\tservice *domain.Service\n\tinner struct {\n\t\tHidden\n\t}\n\tList[int]\n}";
        assert_eq!(
            written_embeddings(declaration),
            [
                (None, "BaseAPI".to_string()),
                (Some("cached".to_string()), "BaseManager".to_string()),
                (None, "List".to_string()),
            ]
        );
        assert_eq!(
            written_embeddings("type EphemeralKV struct{ RemoteKV }"),
            [(None, "RemoteKV".to_string())]
        );
    }

    #[test]
    fn a_field_type_is_read_from_its_declaration_line() {
        let lines = [
            "\tservice *domain.Service".to_string(),
            "\tstore   *store".to_string(),
        ];
        assert_eq!(
            written_field_type(&lines, "service").as_deref(),
            Some("domain.Service")
        );
        assert_eq!(
            written_field_type(&lines, "store").as_deref(),
            Some("store")
        );
        assert_eq!(written_field_type(&lines, "serv"), None);
        assert_eq!(
            written_field_type(&["\titems []Item".to_string()], "items").as_deref(),
            Some("Item")
        );
    }

    #[test]
    fn a_selector_chain_is_read_from_its_start() {
        assert_eq!(
            selector_chain("\treturn s.db.CreateItem(name)", 8).0,
            ["s", "db", "CreateItem"]
        );
        assert_eq!(selector_chain("x := a . b(c)", 5).0, ["a", "b"]);
        assert!(selector_chain("ab.c()", 1).0.is_empty());
    }
}
