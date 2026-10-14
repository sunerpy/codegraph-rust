//! Which items a bare Rust name can mean (upstream #2124, #2214; W4-RS-01).
//!
//! A bare name reaches what is in scope: an item of its own file, one a
//! `use` binds or a glob over its module brings in, or the prelude's. A name
//! the file imports from outside the project (`use std::task::{Context,
//! Poll}`, `use futures::Stream`) is that outside item, and a path names its
//! module (`io::Result` under `use std::io` is std's; `crate::error::Result`
//! is the `error` module's). The crates that are the project's and the ones
//! its manifests depend on are read from every `Cargo.toml` at or above an
//! indexed `.rs` file.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};

use crate::pathutil;
use crate::types::{RefView, ResolutionContext, ResolvedRef};

/// The project's own crates and the ones its manifests depend on, by the
/// name code writes them (`futures_util`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RustCrates {
    /// `[package] name` of each manifest.
    pub project: BTreeSet<String>,
    /// Every `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`
    /// and per-target dependency, less the project's own crates.
    pub dependencies: BTreeSet<String>,
}

/// No crates, for a context that reads no manifests.
pub fn no_rust_crates() -> Arc<RustCrates> {
    static EMPTY: std::sync::OnceLock<Arc<RustCrates>> = std::sync::OnceLock::new();
    Arc::clone(EMPTY.get_or_init(|| Arc::new(RustCrates::default())))
}

/// The crates of every `Cargo.toml` at or above an indexed `.rs` file, the
/// root's included (upstream `rustProjectCrates` / `rustDependencyCrates`,
/// which read the root manifest and its workspace members). Only a directory
/// of a project-relative path is probed, and each manifest is read as a source
/// file is: a regular file within the size limit, stat first.
pub fn load_rust_crates(project_root: &str, files: &[String]) -> RustCrates {
    let mut dirs: BTreeSet<String> = BTreeSet::from([String::new()]);
    for file in files {
        let file = file.replace('\\', "/");
        if !file.ends_with(".rs")
            || file.starts_with('/')
            || file.split('/').any(|segment| segment == "..")
        {
            continue;
        }
        let mut dir = pathutil::dirname(&file);
        while !dir.is_empty() {
            dirs.insert(dir.clone());
            dir = pathutil::dirname(&dir);
        }
    }
    let mut crates = RustCrates::default();
    for dir in &dirs {
        let relative = if dir.is_empty() {
            "Cargo.toml".to_string()
        } else {
            format!("{dir}/Cargo.toml")
        };
        let Some(text) = crate::context::read_source_file(
            &std::path::Path::new(project_root).join(&relative),
            &relative,
            codegraph_core::config::DEFAULT_MAX_FILE_SIZE,
        ) else {
            continue;
        };
        let (own, dependencies) = parse_cargo_manifest(&text);
        crates.project.extend(own);
        crates.dependencies.extend(dependencies);
    }
    let own = crates.project.clone();
    crates
        .dependencies
        .retain(|dependency| !own.contains(dependency));
    crates
}

/// A manifest's crate names — its `[package] name` and a `[lib] name` that
/// renames the library — and the dependencies its tables declare, with `-`
/// read as `_`. An array table (`[[bench]]`) ends the table before it, and a
/// dotted key (`serde.version = "1"`) names its first part.
fn parse_cargo_manifest(text: &str) -> (Vec<String>, Vec<String>) {
    let mut own = Vec::new();
    let mut dependencies = Vec::new();
    let mut table = String::new();
    let is_key = |key: &str| {
        !key.is_empty()
            && key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    };
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or_default().trim();
        if let Some(array) = line
            .strip_prefix("[[")
            .and_then(|rest| rest.strip_suffix("]]"))
        {
            table = format!("[{}]", array.trim());
            continue;
        }
        if let Some(header) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
            .filter(|header| !header.contains(['[', ']']))
        {
            table = header.trim().to_string();
            // `[dependencies.tokio]` names one dependency in its header.
            if let Some(named) = dependency_table_name(&table) {
                dependencies.push(named.replace('-', "_"));
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let mut parts = key.trim().split('.').map(str::trim);
        let first = parts.next().unwrap_or_default();
        if !is_key(first) || !parts.all(is_key) {
            continue;
        }
        let dotted = key.contains('.');
        if (table == "package" || table == "lib") && first == "name" && !dotted {
            let name = value.trim().trim_matches(['"', '\'']).replace('-', "_");
            if !name.is_empty() {
                own.push(name);
            }
        } else if is_dependency_table(&table) {
            dependencies.push(first.replace('-', "_"));
        }
    }
    (own, dependencies)
}

/// `dependencies`, `dev-dependencies`, `build-dependencies`, alone or as a
/// dotted table's last part (`target.'cfg(unix)'.dependencies`,
/// `workspace.dependencies`).
fn is_dependency_table(table: &str) -> bool {
    let last = table.rsplit('.').next().unwrap_or(table);
    matches!(
        last,
        "dependencies" | "dev-dependencies" | "build-dependencies"
    )
}

/// The dependency a `[dependencies.NAME]` header names.
fn dependency_table_name(table: &str) -> Option<&str> {
    let (head, name) = table.rsplit_once('.')?;
    let kind = head.rsplit('.').next().unwrap_or(head);
    (matches!(
        kind,
        "dependencies" | "dev-dependencies" | "build-dependencies"
    ) && !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
    .then_some(name)
}

/// What a Rust file's `use` items bring in (upstream `RustUses`).
#[derive(Debug, Default)]
pub(crate) struct RustUses {
    /// Every identifier in the file's project `use` trees — not the ones of
    /// `std` / `core` / `alloc` or a dependency.
    names: HashSet<String>,
    /// The module of each project `use …::X::*` (`super` for `use super::*`).
    globs: HashSet<String>,
    /// The names the file imports from outside the project.
    external: HashSet<String>,
    /// The items the file's project `use`s bind: their leaves (an alias for
    /// an `as`), never the paths they walk.
    bound: HashSet<String>,
}

impl RustUses {
    /// Read from the file's `use` items (comments and strings left out).
    pub(crate) fn read(scopes: &crate::rust_lexical::RustUseScopes, crates: &RustCrates) -> Self {
        let mut uses = Self::default();
        for (leaves, tree) in scopes.items() {
            let root = tree.root.as_str();
            let outside = matches!(root, "std" | "core" | "alloc")
                || (!root.is_empty() && crates.dependencies.contains(root));
            if outside {
                uses.external.extend(leaves.iter().cloned());
                continue;
            }
            uses.bound.extend(leaves.iter().cloned());
            uses.names.extend(tree.idents.iter().cloned());
            uses.globs.extend(tree.glob_modules.iter().cloned());
        }
        uses
    }
}

/// Names the Rust prelude puts in every module; a project item of the same
/// name needs a `use` to shadow one (upstream `RUST_PRELUDE`).
const RUST_PRELUDE: [&str; 33] = [
    "Ok",
    "Err",
    "Some",
    "None",
    "Result",
    "Option",
    "Box",
    "Vec",
    "String",
    "Default",
    "Drop",
    "Iterator",
    "IntoIterator",
    "From",
    "Into",
    "Clone",
    "Copy",
    "Send",
    "Sync",
    "Sized",
    "ToString",
    "ToOwned",
    "PartialEq",
    "Eq",
    "PartialOrd",
    "Ord",
    "AsRef",
    "AsMut",
    "Fn",
    "FnMut",
    "FnOnce",
    "Extend",
    "drop",
];

fn is_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Whether `reference` is a Rust reference by a bare identifier — the ones
/// [`is_rust_name_in_scope`] judges.
pub(crate) fn is_rust_bare_identifier(reference: &RefView) -> bool {
    reference.language == Language::Rust && is_identifier(&reference.reference_name)
}

/// The module a Rust file is: `src/glob.rs` → `glob`, `src/walk/mod.rs` →
/// `walk` (upstream `rustModuleName`).
fn rust_module_name(file_path: &str) -> &str {
    let mut parts = file_path.rsplit('/');
    let base = parts.next().unwrap_or(file_path);
    let base = base.strip_suffix(".rs").unwrap_or(base);
    if matches!(base, "mod" | "lib" | "main") {
        parts.next().unwrap_or(base)
    } else {
        base
    }
}

/// Whether one of the file's globs brings in `candidate`'s module (or, for
/// `use super::*`, its parent's; upstream `rustGlobCovers`).
fn rust_glob_covers(uses: &RustUses, candidate: &Node, reference: &RefView) -> bool {
    if uses.globs.contains(rust_module_name(&candidate.file_path)) {
        return true;
    }
    if !uses.globs.contains("super") {
        return false;
    }
    let dir = |path: &str| path.rsplit_once('/').map_or("", |(dir, _)| dir).to_string();
    let candidate_dir = dir(&candidate.file_path);
    let reference_dir = dir(&reference.file_path);
    candidate_dir == reference_dir || candidate_dir == dir(&reference_dir)
}

/// The path a Rust name is written through on its line, as `(root, seg,
/// single)` — `("crate", "error", false)` for `crate::error::Result`,
/// `("io", "io", true)` for `io::Result` — at the reference's column, or else
/// the line's first `seg::name` when the line writes the name nowhere bare
/// (upstream's two `pathed` patterns). `seg` is empty for a `::name` written
/// from the root; `single` says the path names one segment (upstream's empty
/// first group).
fn written_path(line: &str, column: usize, name: &str) -> Option<(String, String, bool)> {
    let bytes = line.as_bytes();
    let at_column = line
        .get(column..)
        .is_some_and(|rest| rest.starts_with(name))
        && line[..column].trim_end().ends_with("::");
    if at_column {
        return Some(path_before(line, column));
    }
    // `(?<![\w$:])name\b` anywhere on the line: written bare there.
    let occurrences: Vec<usize> = line
        .match_indices(name)
        .map(|(at, _)| at)
        .filter(|&at| {
            bytes
                .get(at + name.len())
                .is_none_or(|byte| !is_word_byte(*byte))
        })
        .collect();
    let bare = occurrences.iter().any(|&at| {
        at == 0 || !(is_word_byte(bytes[at - 1]) || matches!(bytes[at - 1], b'$' | b':'))
    });
    if bare {
        return None;
    }
    occurrences.into_iter().find_map(|at| {
        let (root, seg, single) = path_before(line, at);
        (!seg.is_empty()).then_some((root, seg, single))
    })
}

/// The `root::…::seg::` path that ends right before `end` (after optional
/// spaces): its first segment, its last one, and whether it names one
/// segment at most.
fn path_before(line: &str, end: usize) -> (String, String, bool) {
    let bytes = line.as_bytes();
    let mut segments: Vec<&str> = Vec::new();
    let mut at = end;
    loop {
        let mut p = at;
        while p > 0 && bytes[p - 1].is_ascii_whitespace() {
            p -= 1;
        }
        if p < 2 || &line[p - 2..p] != "::" {
            break;
        }
        p -= 2;
        while p > 0 && bytes[p - 1].is_ascii_whitespace() {
            p -= 1;
        }
        let ident_end = p;
        while p > 0 && is_word_byte(bytes[p - 1]) {
            p -= 1;
        }
        if p == ident_end
            || !bytes
                .get(p)
                .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        {
            // `::name` from the root: no segment before it.
            segments.push("");
            break;
        }
        segments.push(&line[p..ident_end]);
        at = p;
    }
    let seg = segments.first().copied().unwrap_or_default().to_string();
    let root = segments
        .iter()
        .rev()
        .find(|segment| !segment.is_empty())
        .copied()
        .unwrap_or_default()
        .to_string();
    let single = segments
        .iter()
        .filter(|segment| !segment.is_empty())
        .count()
        <= 1;
    (root, seg, single)
}

/// Whether the bare Rust name `reference` writes can mean `candidate`
/// (upstream `isRustNameInScope`, #2124 and #2214).
///
/// Through a path on its line, the path's module decides: `std::` / `core::`
/// / `alloc::` are the standard library's; `crate::`, `self::`, `super::`,
/// `Self::` and a project crate's name (`clap::Command`, re-exported) look it
/// up as before; a path through an outside import (`io::` under `use
/// std::io`) reaches nothing; else the candidate's module, directory or
/// qualified name holds the segment. An enum variant is in scope only through
/// a `use` of it and its enum, or a glob over the enum, and never names a
/// type. Another file's type or function is in scope only through a `use`
/// that binds it or a glob over its module; one the file imports from
/// outside the project is that outside item; a prelude name reaches a project
/// item only through a project `use` that spells it or a glob over its
/// module. A member (reached through a value), an import and a file or
/// module target are not judged here.
pub(crate) fn is_rust_name_in_scope(
    candidate: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    let name = reference.reference_name.as_str();
    // Unknown source keeps the binding, as the prelude variants' rule does.
    let Some(facts) = context.source_facts(&reference.file_path) else {
        return true;
    };
    let line = usize::try_from(reference.line)
        .ok()
        .and_then(|line| line.checked_sub(1))
        .and_then(|index| facts.raw_line(index));
    let column = usize::try_from(reference.column.max(0)).unwrap_or(0);
    let crates = context.get_rust_crates();
    let uses = facts.rust_uses(&crates);
    // A variant path — `Mode::A`, `mode::Mode::B`, a `Mode::C(x)` pattern, or
    // `Self::A` in an impl, where the reference names the impl's type — reads
    // the enum that declares that variant, and nothing else (upstream #2354):
    // ripgrep's `Match::None` is `ignore`'s enum, not the matcher's `struct
    // Match`. An associated const or function read the same way
    // (`Limits::MAX`, `Mode::ALL`) is no variant, so it references no type.
    let at = line.and_then(|line| line.get(column..)).unwrap_or_default();
    let type_read =
        reference.reference_kind == EdgeKind::References && name.bytes().all(is_word_byte);
    let via_self = type_read && name != "Self" && self_path_at(at);
    let names_variant = via_self
        || (at.starts_with(name)
            && !at
                .as_bytes()
                .get(name.len())
                .is_some_and(|byte| is_word_byte(*byte)));
    if type_read
        && names_variant
        && let Some(variant) = variant_after(at)
        && !declares_rust_variant(candidate, variant, context)
    {
        return false;
    }
    // `Self::A` names the impl's type, not whatever path the line writes
    // elsewhere.
    let pathed = if via_self {
        None
    } else {
        line.and_then(|line| written_path(line, column, name))
    };
    if let Some((root, seg, single)) = pathed {
        // `Self::Error` in a signature is the enclosing impl's (or trait's)
        // own associated type, and `V::Value` an associated type of a
        // generic's bound — never a struct of that name: serde's 334
        // `Self::Error`s went to `de::value::Error` (upstream #2231).
        if reference.reference_kind == EdgeKind::References && single {
            if seg == "Self" {
                return candidate.kind == NodeKind::TypeAlias
                    && is_in_enclosing_rust_impl(candidate, reference, &facts);
            }
            if is_rust_generic_param(&seg, reference, &facts) {
                return false;
            }
        }
        if matches!(root.as_str(), "std" | "core" | "alloc")
            && candidate.file_path != reference.file_path
        {
            return false;
        }
        if matches!(seg.as_str(), "" | "crate" | "self" | "super" | "Self")
            || candidate.file_path == reference.file_path
            || crates.project.contains(&seg)
        {
            return true;
        }
        // `io::Error` under `use std::io;` is std's, whatever `io/` directory
        // the project has.
        if uses.external.contains(&seg) && !uses.bound.contains(&seg) {
            return false;
        }
        return rust_module_name(&candidate.file_path) == seg
            || candidate.file_path.contains(&format!("/{seg}/"))
            || candidate.qualified_name.split("::").any(|part| part == seg);
    }
    if candidate.kind == NodeKind::EnumMember {
        if reference.reference_kind == EdgeKind::References {
            return false;
        }
        let owner = candidate
            .qualified_name
            .rsplit_once("::")
            .map(|(owner, _)| owner.rsplit("::").next().unwrap_or(owner))
            .unwrap_or_default();
        return (!owner.is_empty() && uses.globs.contains(owner))
            || (uses.names.contains(name) && uses.names.contains(owner));
    }
    if candidate.file_path == reference.file_path {
        return true;
    }
    // `use std::task::{Context, Poll}`: the file's `Context` is std's. A
    // method call `.env(…)` is no imported name, and a name not on its line
    // is a later link of a chain written across lines.
    if uses.external.contains(name)
        && !uses.bound.contains(name)
        && let Some(line) = line
        && let Some(rest) = line.get(column..)
    {
        let bytes = line.as_bytes();
        // Read from the column on, as upstream's sliced line: the column's
        // own byte has nothing before it.
        let spelled = rest
            .match_indices(name)
            .map(|(at, _)| column + at)
            .find(|&at| {
                (at == column || !(is_word_byte(bytes[at - 1]) || bytes[at - 1] == b'$'))
                    && bytes
                        .get(at + name.len())
                        .is_none_or(|byte| !is_word_byte(*byte))
            });
        if spelled.is_some_and(|at| !line[..at].trim_end().ends_with('.')) {
            return false;
        }
    }
    if !RUST_PRELUDE.contains(&name) {
        // A method is reached through a value, never a `use`.
        if matches!(
            candidate.kind,
            NodeKind::Method
                | NodeKind::Property
                | NodeKind::Field
                | NodeKind::EnumMember
                | NodeKind::File
                | NodeKind::Module
                | NodeKind::Namespace
        ) || reference.reference_kind == EdgeKind::Imports
        {
            return true;
        }
        return uses.bound.contains(name) || rust_glob_covers(&uses, candidate, reference);
    }
    uses.names.contains(name) || rust_glob_covers(&uses, candidate, reference)
}

/// The 1-based line of the `impl` / `trait` header at or above `reference`'s
/// line, 0 for none (upstream `rustEnclosingImplLine`).
fn rust_enclosing_impl_line(
    reference: &RefView,
    facts: &crate::source_facts::SourceFacts,
) -> usize {
    let last = usize::try_from(reference.line)
        .unwrap_or(0)
        .min(facts.line_count());
    (0..last)
        .rev()
        .find(|&index| opens_impl_or_trait(facts.line(index)))
        .map_or(0, |index| index + 1)
}

/// Whether `line` opens an `impl` or `trait` (upstream
/// `/^\s*(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?(?:impl|trait)\b/`).
fn opens_impl_or_trait(line: &str) -> bool {
    let mut rest = line.trim_start();
    if let Some(after) = rest.strip_prefix("pub") {
        let after = match after.strip_prefix('(') {
            Some(inner) => match inner.find(')') {
                Some(close) => &inner[close + 1..],
                None => after,
            },
            None => after,
        };
        if after.starts_with(char::is_whitespace) {
            rest = after.trim_start();
        }
    }
    if let Some(after) = rest.strip_prefix("unsafe")
        && after.starts_with(char::is_whitespace)
    {
        rest = after.trim_start();
    }
    ["impl", "trait"].iter().any(|keyword| {
        rest.strip_prefix(keyword)
            .is_some_and(|after| !after.as_bytes().first().is_some_and(|b| is_word_byte(*b)))
    })
}

/// Whether `candidate` is declared in the `impl` / `trait` block around
/// `reference`, above it (upstream `isInEnclosingRustImpl`).
fn is_in_enclosing_rust_impl(
    candidate: &Node,
    reference: &RefView,
    facts: &crate::source_facts::SourceFacts,
) -> bool {
    let header = i64::try_from(rust_enclosing_impl_line(reference, facts)).unwrap_or(0);
    header > 0
        && candidate.file_path == reference.file_path
        && candidate.start_line >= header
        && candidate.start_line <= reference.line
}

/// Whether `name` is a generic type parameter of the function or impl around
/// `reference` (`fn f<V: Visitor>`, `impl<'de, E>`; upstream
/// `isRustGenericParam`): written inside an angle-bracket list, one nested
/// list allowed before it, and followed by `:`, `,`, `>` or `=`, between the
/// enclosing impl's header (the file's start without one) and the
/// reference's line.
fn is_rust_generic_param(
    name: &str,
    reference: &RefView,
    facts: &crate::source_facts::SourceFacts,
) -> bool {
    if !name.starts_with(|c: char| c.is_ascii_uppercase()) || !name.bytes().all(is_word_byte) {
        return false;
    }
    let from = rust_enclosing_impl_line(reference, facts).saturating_sub(1);
    let through = usize::try_from(reference.line).unwrap_or(0);
    let lines = facts.lines();
    let (Some(start), Some(source)) = (lines.raw_line_start(from), Some(facts.source())) else {
        return false;
    };
    let end = lines
        .raw_line_start(through)
        .map_or(source.len(), |next| next.saturating_sub(1));
    let text = source
        .get(start..end.max(start))
        .unwrap_or_default()
        .as_bytes();
    let name = name.as_bytes();
    (0..text.len()).filter(|&at| text[at] == b'<').any(|open| {
        // Walk the list `open` starts: depth 1 is the list itself, depth 2 one
        // nested list; a deeper one, or the list's close before the name,
        // ends the walk.
        let mut depth = 1;
        let mut at = open + 1;
        while at < text.len() {
            match text[at] {
                b'<' if depth == 1 => depth = 2,
                b'<' => return false,
                b'>' if depth == 2 => depth = 1,
                b'>' => return false,
                _ if depth == 1
                    && text[at..].starts_with(name)
                    && (at == 0 || !is_word_byte(text[at - 1]))
                    && !text.get(at + name.len()).is_some_and(|b| is_word_byte(*b)) =>
                {
                    let after = text[at + name.len()..]
                        .iter()
                        .find(|b| !b.is_ascii_whitespace());
                    if after.is_some_and(|b| matches!(b, b':' | b',' | b'>' | b'=')) {
                        return true;
                    }
                }
                _ => {}
            }
            at += 1;
        }
        false
    })
}

/// Whether `at` opens with `Self ::` (upstream `/^Self\s*::/`).
fn self_path_at(at: &str) -> bool {
    at.strip_prefix("Self")
        .is_some_and(|rest| rest.trim_start().starts_with("::"))
}

/// The capitalized member after the word `at` opens with and a `::`
/// (upstream `/^\w+\s*::\s*([A-Z]\w*)/`): `A` of `Mode::A(x)`.
fn variant_after(at: &str) -> Option<&str> {
    let bytes = at.as_bytes();
    let word = bytes.iter().take_while(|byte| is_word_byte(**byte)).count();
    if word == 0 {
        return None;
    }
    let rest = at[word..].trim_start().strip_prefix("::")?.trim_start();
    if !rest.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }
    let end = rest.bytes().take_while(|byte| is_word_byte(*byte)).count();
    Some(&rest[..end])
}

/// Whether `candidate` is a Rust enum that declares `variant` (upstream
/// `declaresRustVariant`, #2354).
fn declares_rust_variant(candidate: &Node, variant: &str, context: &dyn ResolutionContext) -> bool {
    if candidate.kind != NodeKind::Enum {
        return false;
    }
    let qualified = format!("{}::{variant}", candidate.qualified_name);
    context
        .get_nodes_in_file_shared(&candidate.file_path)
        .iter()
        .any(|node| node.kind == NodeKind::EnumMember && node.qualified_name == qualified)
}

/// A bare Rust name reaches only what is in scope, whichever strategy chose
/// the target (upstream #2214 `gateRustScope`).
pub(crate) fn gate_rust_scope(
    resolved: Option<ResolvedRef>,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let result = resolved?;
    if !is_rust_bare_identifier(reference) {
        return Some(result);
    }
    match context.get_node_by_id_shared(&result.target_node_id) {
        Some(target) if !is_rust_name_in_scope(&target, reference, context) => None,
        _ => Some(result),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manifest_names_its_package_and_dependencies() {
        let text = "[package]\nname = \"my-app\"\n\n[dependencies]\nfutures = \"0.3\"\nserde_json = { version = \"1\" } # json\n\n[dependencies.tokio-util]\nversion = \"0.7\"\n\n[target.'cfg(unix)'.dev-dependencies]\nnix = \"0.27\"\n\n[features]\ndefault = []\n";
        let (own, mut dependencies) = parse_cargo_manifest(text);
        dependencies.sort();
        assert_eq!(own, ["my_app"]);
        assert_eq!(dependencies, ["futures", "nix", "serde_json", "tokio_util"]);
    }

    #[test]
    fn a_manifest_renamed_library_dotted_keys_and_array_tables() {
        let text = "[package]\nname = \"my-app\"\n\n[lib]\nname = \"myapp\"\n\n[dev-dependencies]\ncriterion = \"0.5\"\nserde.version = \"1\"\nserde.features = [\"derive\"]\n\n[[bench]]\nname = \"throughput\"\nharness = false\n";
        let (own, mut dependencies) = parse_cargo_manifest(text);
        dependencies.sort();
        dependencies.dedup();
        assert_eq!(own, ["my_app", "myapp"]);
        assert_eq!(dependencies, ["criterion", "serde"]);
    }

    #[test]
    fn crates_come_from_the_manifests_at_or_above_indexed_rust_files() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("cg-rust-crates-{nanos}"));
        std::fs::create_dir_all(root.join("crates/core/src")).unwrap();
        std::fs::create_dir_all(root.join("tools")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"app\"\n\n[dependencies]\ncore-lib = { path = \"crates/core\" }\nanyhow = \"1\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("crates/core/Cargo.toml"),
            "[package]\nname = \"core-lib\"\n\n[dependencies]\nserde = \"1\"\n",
        )
        .unwrap();
        // No indexed `.rs` file under `tools/`: its manifest is not read.
        std::fs::write(
            root.join("tools/Cargo.toml"),
            "[package]\nname = \"tool\"\n\n[dependencies]\nclap = \"4\"\n",
        )
        .unwrap();
        let files = [
            "src/main.rs",
            "crates/core/src/lib.rs",
            "tools/readme.md",
            "/abs/x.rs",
            "../outside/y.rs",
        ]
        .map(String::from);
        let crates = load_rust_crates(root.to_str().unwrap(), &files);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            crates
                .project
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["app", "core_lib"]
        );
        assert_eq!(
            crates
                .dependencies
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["anyhow", "serde"]
        );
    }

    #[test]
    fn a_path_is_read_at_the_column_or_from_the_line() {
        let line = "pub fn load() -> crate::error::Result<u8> {";
        let column = line.find("Result").unwrap();
        assert_eq!(
            written_path(line, column, "Result"),
            Some(("crate".into(), "error".into(), false))
        );
        let line = "pub fn poll_ready(cx: &mut Context<'_>) -> Poll<io::Result<()>> {";
        assert_eq!(
            written_path(line, 0, "Result"),
            Some(("io".into(), "io".into(), true))
        );
        // Written bare somewhere on the line: no path.
        assert_eq!(
            written_path("fn f(r: Result) -> io::Result", 0, "Result"),
            None
        );
        assert_eq!(
            written_path("let x = ::serde::Serialize;", 15, "Serialize"),
            Some(("serde".into(), "serde".into(), true))
        );
    }

    #[test]
    fn an_impl_or_trait_header_and_a_generic_parameter_are_read_around_a_reference() {
        assert!(opens_impl_or_trait("impl<'de, E> Deserializer for X<E> {"));
        assert!(opens_impl_or_trait("    pub(crate) unsafe trait Visit {"));
        assert!(opens_impl_or_trait("unsafe impl Send for X {}"));
        assert!(!opens_impl_or_trait("implement();"));
        assert!(!opens_impl_or_trait("// impl Foo"));
        let source = "pub struct X;\n\nimpl<E> Deserializer for X<E> {\n    type Error = E;\n\n    fn any<V: Visitor, W>(v: V) -> Result<V::Value, Self::Error> {\n        todo!()\n    }\n}\n";
        let facts = crate::source_facts::SourceFacts::new(std::sync::Arc::from(source));
        let reference = RefView {
            row_id: None,
            from_node_id: "function:any".into(),
            reference_name: "Value".into(),
            reference_kind: EdgeKind::References,
            line: 6,
            column: 40,
            file_path: "src/x.rs".into(),
            language: Language::Rust,
            is_function_ref: false,
            reference_subkind: None,
        };
        assert_eq!(rust_enclosing_impl_line(&reference, &facts), 3);
        for parameter in ["E", "V", "W"] {
            assert!(
                is_rust_generic_param(parameter, &reference, &facts),
                "{parameter}"
            );
        }
        // Upstream's pattern reads any name in a list followed by `:`, `,`,
        // `>` or `=`, a bound before a comma included (`V: Visitor, W`); a
        // name outside every list is none.
        assert!(is_rust_generic_param("Visitor", &reference, &facts));
        for name in ["X", "Deserializer", "Unit"] {
            assert!(!is_rust_generic_param(name, &reference, &facts), "{name}");
        }
    }

    #[test]
    fn a_variant_path_names_its_member() {
        assert_eq!(variant_after("Mode::A => 1,"), Some("A"));
        assert_eq!(variant_after("Mode :: C(x) =>"), Some("C"));
        assert_eq!(variant_after("Self::Ready"), Some("Ready"));
        assert_eq!(variant_after("Mode::new()"), None);
        assert_eq!(variant_after("Mode) {}"), None);
        assert_eq!(variant_after("::A"), None);
        assert!(self_path_at("Self ::A"));
        assert!(!self_path_at("SelfType::A"));
    }

    #[test]
    fn a_module_is_named_after_its_file_or_directory() {
        assert_eq!(rust_module_name("src/glob.rs"), "glob");
        assert_eq!(rust_module_name("src/walk/mod.rs"), "walk");
        assert_eq!(rust_module_name("crates/core/src/lib.rs"), "src");
    }
}
