//! Terraform [`FrameworkResolver`] (upstream
//! `resolution/frameworks/terraform.ts`, #1173, #1174).
//!
//! Terraform's scope is a module directory: `var.X`, `local.X`, `module.M`
//! and resource and data references resolve only inside the directory of
//! the reference, where the generic name matcher binds by qualified name
//! alone, and a `var.project_id` in `modules/net-vpc/main.tf` bound an
//! unrelated module's `variable "project_id"`. This resolver keeps the real
//! rule:
//!
//! 1. a declaration in the reference's directory (0.95);
//! 2. for a `.tfvars` assignment, the nearest ancestor directory declaring
//!    the variable (`-var-file=envs/prod.tfvars` sets the root module's);
//!    and a provider configuration, which the tree inherits, the nearest
//!    ancestor's (0.9);
//! 3. nothing else: Terraform cannot reference across sibling modules.
//!
//! It bridges a module's boundary through the `:`-scoped references the
//! extractor writes for it alone: `module.M:file` (the entry file of the
//! module's local source directory, `main.tf` when there is one, else the
//! first `.tf` by name), `module.M:var.<in>` (the child's variable a module
//! block sets), `module.M:output.<o>` (the child's output a use reads), and
//! `module.M:remote-output.<o>` (the cloudposse remote-state shape: another
//! component's output, when the component is a literal, or a variable with a
//! literal default, and one directory of that name declares the output).
//! A declaration's `source`, `component` and a variable's `default` are read
//! from the declaration's lines in its file. Registry and git sources stay
//! unresolved: an out-of-repo module is a visible boundary, never a guess.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use codegraph_core::types::{Language, Node, NodeKind};
use regex::Regex;

use crate::framework::FrameworkResolver;
use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// The Terraform framework resolver.
pub struct TerraformResolver;

/// A `module.M:<child>` reference the extractor writes (upstream
/// `SCOPED_REF`).
static SCOPED_REF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^module\.[^.:\s]+:(?:file$|var\.|output\.|remote-output\.)").expect("scoped ref")
});

/// `module.M:<child>`, split.
static SCOPED_PARTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^module\.([^.:\s]+):(.+)$").expect("scoped parts"));

/// A declaration's `source = "…"` line.
static SOURCE_ATTR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\s*source\s*=\s*"([^"]+)""#).expect("source"));
/// A declaration's `component = "…"` line.
static COMPONENT_ATTR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\s*component\s*=\s*"([^"]+)""#).expect("component"));
/// A declaration's `component = var.X` line.
static COMPONENT_VAR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*component\s*=\s*var\.([A-Za-z0-9_-]+)\s*$").expect("component var")
});
/// A variable's `default = "…"` line.
static DEFAULT_ATTR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\s*default\s*=\s*"([^"]+)""#).expect("default"));
/// A remote-state module's source (upstream's `/\/remote-state(\/|$)/`).
static REMOTE_STATE_SOURCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/remote-state(?:/|$)").expect("remote-state"));

/// Whether `file` is a Terraform source file the resolver detects by.
fn is_terraform_file(file: &str) -> bool {
    file.ends_with(".tf") || file.ends_with(".tfvars") || file.ends_with(".tofu")
}

/// The directory of a project path, `.` for the root (upstream `dirOf`).
fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) => "/",
        Some(slash) => &path[..slash],
        None => ".",
    }
}

/// The parent of a directory, `None` above the root (upstream `parentOf`).
fn parent_of(dir: &str) -> Option<&str> {
    if dir == "." || dir.is_empty() {
        return None;
    }
    let parent = dir_of(dir);
    (parent != dir).then_some(parent)
}

/// `relative` (a module's `./` or `../` source) from `base`, normalized to
/// the stored shape, `.` for the root (upstream `joinDirs` and
/// `normalizeRel`).
fn join_dirs(base: &str, relative: &str) -> String {
    let joined = if base == "." {
        relative.to_string()
    } else {
        format!("{base}/{relative}")
    };
    crate::pathutil::normalize(&joined)
}

/// The first candidate in the nearest ancestor directory of `dir`, `dir`
/// itself excluded (upstream `nearestAncestorMatch`).
fn nearest_ancestor<'n>(candidates: &[&'n Node], dir: &str) -> Option<&'n Node> {
    let mut at = parent_of(dir);
    while let Some(current) = at {
        if let Some(found) = candidates
            .iter()
            .find(|candidate| dir_of(&candidate.file_path) == current)
        {
            return Some(found);
        }
        at = parent_of(current);
    }
    None
}

/// The first capture of `pattern` across `node`'s lines in its file
/// (upstream `readNodeSpanMatch`).
fn span_match(node: &Node, pattern: &Regex, context: &dyn ResolutionContext) -> Option<String> {
    // Through the file's line index: every scoped reference reads a
    // declaration's lines, and splitting the whole file each time cost more
    // than the match (W7-24 cost, OD-B73).
    let facts = context.source_facts(&node.file_path)?;
    let end = usize::try_from(node.end_line)
        .ok()?
        .min(facts.lines().raw_line_count());
    let start = usize::try_from(node.start_line - 1).unwrap_or(0);
    if start > end {
        return None;
    }
    (start..end).find_map(|index| {
        let line = facts.raw_line(index)?;
        pattern
            .captures(line)
            .map(|captures| captures[1].to_string())
    })
}

fn framework(reference: &RefView, target: &Node, confidence: f64) -> ResolvedRef {
    ResolvedRef::new(
        reference.clone(),
        target.id.clone(),
        confidence,
        ResolvedBy::Framework,
    )
}

impl FrameworkResolver for TerraformResolver {
    fn name(&self) -> &str {
        "terraform"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&[Language::Terraform])
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        context
            .get_all_files_shared()
            .iter()
            .any(|file| is_terraform_file(file))
    }

    /// A scoped reference names no declared symbol: it passes the name
    /// pre-filter by this claim.
    fn claims_reference(&self, name: &str) -> bool {
        SCOPED_REF.is_match(name)
    }

    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        if reference.language != Language::Terraform {
            return None;
        }
        let name = reference.reference_name.as_str();
        let ref_dir = dir_of(&reference.file_path);
        if let Some(parts) = SCOPED_PARTS.captures(name) {
            return resolve_scoped(reference, &parts[1], &parts[2], ref_dir, context);
        }
        let candidates = context.get_nodes_by_qualified_name_shared(name);
        if candidates.is_empty() {
            return None;
        }
        if let Some(same) = candidates
            .iter()
            .find(|candidate| dir_of(&candidate.file_path) == ref_dir)
        {
            return Some(framework(reference, same, 0.95));
        }
        if reference.file_path.ends_with(".tfvars") && name.starts_with("var.") {
            let all: Vec<&Node> = candidates
                .iter()
                .map(|candidate| candidate.as_ref())
                .collect();
            if let Some(up) = nearest_ancestor(&all, ref_dir) {
                return Some(framework(reference, up, 0.9));
            }
        }
        if name.starts_with("provider.") {
            let configurations: Vec<&Node> = candidates
                .iter()
                .map(|candidate| candidate.as_ref())
                .filter(|candidate| candidate.kind == NodeKind::Namespace)
                .collect();
            return nearest_ancestor(&configurations, ref_dir)
                .map(|up| framework(reference, up, 0.9));
        }
        None
    }
}

/// `module.<module>:<child>` (upstream `resolveScopedModuleRef`): the
/// `module "<module>"` declaration of the reference's directory, or the
/// project's only one, and the child named inside the directory its local
/// source names (or, for a remote-state output, the component's).
fn resolve_scoped(
    reference: &RefView,
    module: &str,
    child: &str,
    ref_dir: &str,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let declarations: Vec<_> = context
        .get_nodes_by_qualified_name_shared(&format!("module.{module}"))
        .into_iter()
        .filter(|node| node.kind == NodeKind::Module)
        .collect();
    let declaration = declarations
        .iter()
        .find(|node| dir_of(&node.file_path) == ref_dir)
        .or_else(|| (declarations.len() == 1).then(|| &declarations[0]))?;
    let source = span_match(declaration, &SOURCE_ATTR, context)?;

    if let Some(output) = child.strip_prefix("remote-output.") {
        if !REMOTE_STATE_SOURCE.is_match(&source) {
            return None;
        }
        let component = span_match(declaration, &COMPONENT_ATTR, context).or_else(|| {
            // `component = var.vpc_component_name` with a literal `default`
            // in the declaration's directory: one hop, the same literal gate.
            let variable = span_match(declaration, &COMPONENT_VAR, context)?;
            let declared: Vec<_> = context
                .get_nodes_by_qualified_name_shared(&format!("var.{variable}"))
                .into_iter()
                .filter(|node| dir_of(&node.file_path) == dir_of(&declaration.file_path))
                .collect();
            match declared.as_slice() {
                [only] => span_match(only, &DEFAULT_ATTR, context),
                _ => None,
            }
        })?;
        let suffix = format!("/{component}");
        let outputs: Vec<_> = context
            .get_nodes_by_qualified_name_shared(&format!("output.{output}"))
            .into_iter()
            .filter(|node| {
                let dir = dir_of(&node.file_path);
                dir == component || dir.ends_with(&suffix)
            })
            .collect();
        let first = outputs.first()?;
        let dirs: BTreeSet<&str> = outputs.iter().map(|node| dir_of(&node.file_path)).collect();
        if dirs.len() > 1 {
            // Two directories claim the component's name: never guess.
            return None;
        }
        return Some(framework(reference, first, 0.9));
    }

    if !(source.starts_with("./") || source.starts_with("../")) {
        // A registry, git or absolute source is outside the project.
        return None;
    }
    let target_dir = join_dirs(dir_of(&declaration.file_path), &source);

    if child == "file" {
        let mut files: Vec<String> = context
            .get_all_files_shared()
            .iter()
            .filter(|file| {
                dir_of(file) == target_dir && (file.ends_with(".tf") || file.ends_with(".tofu"))
            })
            .cloned()
            .collect();
        files.sort();
        let entry = files
            .iter()
            .find(|file| file.ends_with("/main.tf") || file.as_str() == "main.tf")
            .or_else(|| files.first())?;
        let file_node = context
            .get_nodes_in_file_shared(entry)
            .into_iter()
            .find(|node| node.kind == NodeKind::File)?;
        return Some(framework(reference, &file_node, 0.95));
    }

    // `var.X` or `output.X`: the child module's own qualified names.
    let target = context
        .get_nodes_by_qualified_name_shared(child)
        .into_iter()
        .find(|node| dir_of(&node.file_path) == target_dir)?;
    Some(framework(reference, &target, 0.95))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directories_are_the_stored_shape() {
        assert_eq!(dir_of("main.tf"), ".");
        assert_eq!(dir_of("modules/vpc/main.tf"), "modules/vpc");
        assert_eq!(parent_of("modules/vpc"), Some("modules"));
        assert_eq!(parent_of("modules"), Some("."));
        assert_eq!(parent_of("."), None);
        assert_eq!(join_dirs(".", "./modules/vpc"), "modules/vpc");
        assert_eq!(join_dirs("envs/prod", "../../modules/vpc/"), "modules/vpc");
        assert_eq!(join_dirs("a", ".."), ".");
        assert_eq!(join_dirs(".", "../outside"), "../outside");
    }

    #[test]
    fn scoped_references_are_the_module_wiring() {
        for name in [
            "module.vpc:file",
            "module.vpc:var.cidr",
            "module.vpc:output.id",
            "module.vpc:remote-output.vpc_id",
        ] {
            assert!(SCOPED_REF.is_match(name), "{name}");
        }
        for name in [
            "module.vpc",
            "module.vpc:files",
            "var.cidr",
            "module.a.b:file",
        ] {
            assert!(!SCOPED_REF.is_match(name), "{name}");
        }
    }
}
