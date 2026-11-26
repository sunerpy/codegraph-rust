//! JS/TS workspace (monorepo) package resolution.
//!
//! Ports `upstream resolution/workspace-packages.ts`. Maps each
//! monorepo member package's declared `name` to its directory so the import
//! resolver can rewrite `@scope/ui/widgets` → `packages/ui/widgets` instead of
//! flagging it as an external npm specifier (`workspace-packages.ts:1-25`).
//! Scope mirrors the upstream v1: reads `workspaces` (array OR `{ packages: [...] }`)
//! from package.json plus a minimal `pnpm-workspace.yaml`, expands one level of
//! `*`/`**` globs, directory-based subpath resolution (`workspace-packages.ts:17-24`).
//!
//! HarmonyOS / OpenHarmony (ArkTS) modules are members too (W7-24a1, upstream
//! #1186, `b635dd46` `workspace-packages.ts:67-198`): a bounded walk finds
//! every `oh-package.json5`, and each `file:` dependency it declares names a
//! member directory; the member's own manifest's `main` is its entry. The
//! walk is breadth-first in name order (KEEP-RUST: upstream walks in
//! `readdir` order, which decides what the directory budget cuts), reads no
//! link, directory or manifest, and keeps every target and entry inside the
//! project lexically.

use serde_json::Value;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::path::{Path, PathBuf};

/// What [`load_workspace_packages`] read: a file, or a directory it listed
/// for a glob's members (W5-06).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WorkspaceInput {
    File(PathBuf),
    MemberListing(PathBuf),
}

thread_local! {
    /// While [`workspace_inputs`] runs, every file the loader reads and every
    /// directory it lists, in order.
    static READS: RefCell<Option<Vec<WorkspaceInput>>> = const { RefCell::new(None) };
}

fn note_read(input: WorkspaceInput) {
    READS.with(|reads| {
        if let Some(list) = reads.borrow_mut().as_mut() {
            list.push(input);
        }
    });
}

/// Every file [`load_workspace_packages`] reads for `project_root` and every
/// directory it lists for a glob's members, once each, in the order it first
/// does: the root `package.json` and `pnpm-workspace.yaml`, each member's
/// `package.json`, a missing one included, and every `oh-package.json5` the
/// ohpm walk reads (W7-24a1), so the set of manifests is part of what a sync
/// compares. What a sync compares to know when the workspace may have moved
/// (W5-06).
pub fn workspace_inputs(project_root: &str) -> Vec<WorkspaceInput> {
    READS.with(|reads| *reads.borrow_mut() = Some(Vec::new()));
    let _ = load_workspace_packages(project_root);
    let read = READS
        .with(|reads| reads.borrow_mut().take())
        .unwrap_or_default();
    let mut seen = HashSet::new();
    read.into_iter()
        .filter(|input| seen.insert(input.clone()))
        .collect()
}

/// The member directory names a glob's base `dir` lists: its directories,
/// but no dot directory and no `node_modules`, sorted.
pub fn member_listing(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| !name.starts_with('.') && name != "node_modules")
        .collect();
    names.sort();
    names
}

/// Member package `name` → directory relative to project root (posix)
/// (`WorkspacePackages`, `workspace-packages.ts:31-34`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePackages {
    /// Ordered map keeps deterministic iteration (the upstream relies only on
    /// first-declaration-wins + longest-match, both order-independent).
    pub by_name: BTreeMap<String, String>,
    /// Member `name` → the entry file its ohpm manifest's `main` names,
    /// relative to the project root (posix): a bare import of the member
    /// resolves straight to it, whatever the importer's language
    /// (`entryByName`, upstream `b635dd46` `workspace-packages.ts:36-43`).
    pub entry_by_name: BTreeMap<String, String>,
}

/// Load workspace member packages for `project_root`
/// (`loadWorkspacePackages`, `workspace-packages.ts:45-61`), and the ohpm
/// members (`b635dd46` 67-78).
///
/// Returns `None` when the project declares no workspaces and no ohpm
/// `file:` dependencies (single-package case).
pub fn load_workspace_packages(project_root: &str) -> Option<WorkspacePackages> {
    let patterns = read_workspace_globs(project_root);

    let mut by_name: BTreeMap<String, String> = BTreeMap::new();
    for pattern in &patterns {
        for dir in expand_workspace_glob(project_root, pattern) {
            if let Some(pkg_name) = read_package_name(&Path::new(project_root).join(&dir)) {
                // First declaration wins (workspace-packages.ts:54).
                by_name.entry(pkg_name).or_insert(dir);
            }
        }
    }

    // HarmonyOS/OpenHarmony (ArkTS) modular projects: every module's
    // oh-package.json5 declares its local siblings as `"data": "file:../../
    // core/data"` dependencies, and code imports the bare name. A workspace
    // member of the same name keeps its directory.
    let root = Path::new(project_root);
    let mut entry_by_name: BTreeMap<String, String> = BTreeMap::new();
    for (name, dir) in collect_ohpm_file_deps(root) {
        if by_name.contains_key(&name) {
            continue;
        }
        if let Some(entry) = read_ohpm_main(root, &dir) {
            entry_by_name.insert(name.clone(), entry);
        }
        by_name.insert(name, dir);
    }

    if by_name.is_empty() {
        return None;
    }
    Some(WorkspacePackages {
        by_name,
        entry_by_name,
    })
}

/// The ohpm manifest's file name.
pub const OHPM_MANIFEST: &str = "oh-package.json5";
/// How deep the walk descends below the project root (upstream
/// `OHPM_WALK_MAX_DEPTH`).
const OHPM_WALK_MAX_DEPTH: usize = 6;
/// How many directories the walk reads at most (upstream
/// `OHPM_WALK_DIR_BUDGET`).
const OHPM_WALK_DIR_BUDGET: usize = 8000;
/// The directories the walk never enters, besides every dot directory
/// (upstream `OHPM_SKIP_DIRS`).
const OHPM_SKIP_DIRS: [&str; 10] = [
    "node_modules",
    "oh_modules",
    ".git",
    ".codegraph",
    ".hvigor",
    ".preview",
    "build",
    "dist",
    "out",
    "oh-package-lock.json5",
];

/// Scan the project for `oh-package.json5` manifests and collect their
/// `file:` dependencies as members: dependency name → target directory
/// (project-relative posix) (`collectOhpmFileDeps`, `b635dd46` 106-177). A
/// name two manifests point at different directories is dropped entirely; a
/// target outside the project is no member.
fn collect_ohpm_file_deps(project_root: &Path) -> BTreeMap<String, String> {
    collect_ohpm_file_deps_within(project_root, OHPM_WALK_DIR_BUDGET)
}

/// [`collect_ohpm_file_deps`] reading at most `budget` directories.
fn collect_ohpm_file_deps_within(project_root: &Path, budget: usize) -> BTreeMap<String, String> {
    let mut by_name: BTreeMap<String, String> = BTreeMap::new();
    let mut ambiguous: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::from([(String::new(), 0)]);
    let mut visited = 0usize;
    while let Some((relative, depth)) = queue.pop_front() {
        visited += 1;
        if visited > budget {
            break;
        }
        let dir = if relative.is_empty() {
            project_root.to_path_buf()
        } else {
            project_root.join(&relative)
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        // `DirEntry::file_type` does not follow links: a linked directory is
        // never entered and a linked manifest never read.
        let mut entries: Vec<(String, std::fs::FileType)> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                Some((name, entry.file_type().ok()?))
            })
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, file_type) in entries {
            if file_type.is_dir() {
                if depth >= OHPM_WALK_MAX_DEPTH
                    || name.starts_with('.')
                    || OHPM_SKIP_DIRS.contains(&name.as_str())
                {
                    continue;
                }
                let child = if relative.is_empty() {
                    name
                } else {
                    format!("{relative}/{name}")
                };
                queue.push_back((child, depth + 1));
                continue;
            }
            if name != OHPM_MANIFEST || !file_type.is_file() {
                continue;
            }
            for (dependency, target) in read_ohpm_file_deps(&dir.join(&name)) {
                let Some(target) = project_relative(&relative, &target) else {
                    continue; // escapes the project
                };
                match by_name.get(&dependency) {
                    None => {
                        if !ambiguous.contains(&dependency) {
                            by_name.insert(dependency, target);
                        }
                    }
                    Some(existing) if *existing != target => {
                        by_name.remove(&dependency);
                        ambiguous.insert(dependency);
                    }
                    Some(_) => {}
                }
            }
        }
    }
    by_name
}

/// `relative` (a manifest's project-relative directory) joined with `path`,
/// normalized, or `None` when the result leaves the project lexically. The
/// project root itself is `""`, as upstream's `path.relative` gives it.
fn project_relative(relative: &str, path: &str) -> Option<String> {
    let joined = if relative.is_empty() {
        path.to_string()
    } else {
        format!("{relative}/{path}")
    };
    let normalized = crate::pathutil::lexical_relative(&joined)?;
    Some(if normalized == "." {
        String::new()
    } else {
        normalized
    })
}

/// The JSON of the ohpm manifest at `path`, read only when it is a regular
/// file, no link, within the default file-size limit (stat first), with its
/// JSON5 comments and trailing commas stripped; `None` otherwise or when it
/// does not parse (KEEP-RUST: upstream's jsonc-parser recovers a partial
/// value from a malformed file, this reads nothing).
fn read_ohpm_manifest(path: &Path) -> Option<Value> {
    note_read(WorkspaceInput::File(path.to_path_buf()));
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() || meta.len() > codegraph_core::config::DEFAULT_MAX_FILE_SIZE {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&crate::path_aliases::strip_jsonc(&text)).ok()
}

/// One manifest's `file:` dependencies as `(name, target)` pairs
/// (`readOhpmFileDeps`, `b635dd46` 179-198).
fn read_ohpm_file_deps(manifest: &Path) -> Vec<(String, String)> {
    let Some(Value::Object(dependencies)) =
        read_ohpm_manifest(manifest).and_then(|json| json.get("dependencies").cloned())
    else {
        return Vec::new();
    };
    dependencies
        .into_iter()
        .filter_map(|(name, value)| {
            let target = value.as_str()?.strip_prefix("file:")?.trim();
            (!target.is_empty()).then(|| (name, target.to_string()))
        })
        .collect()
}

/// The entry file member directory `dir`'s manifest names in `main`,
/// project-relative, or `None` when the manifest, the field or a target
/// inside the project is missing (`readOhpmMain`, `b635dd46` 86-104).
fn read_ohpm_main(project_root: &Path, dir: &str) -> Option<String> {
    let manifest = if dir.is_empty() {
        project_root.join(OHPM_MANIFEST)
    } else {
        project_root.join(dir).join(OHPM_MANIFEST)
    };
    let json = read_ohpm_manifest(&manifest)?;
    let main = json.get("main")?.as_str()?.trim();
    if main.is_empty() {
        return None;
    }
    project_relative(dir, main).filter(|entry| !entry.is_empty())
}

/// Rewrite a bare workspace import to a project-relative path WITHOUT extension
/// (`resolveWorkspaceImport`, `workspace-packages.ts:70-86`).
///
/// Longest matching package name wins. Returns `None` when no member matches.
pub fn resolve_workspace_import(import_path: &str, ws: &WorkspacePackages) -> Option<String> {
    let mut best_name: Option<&str> = None;
    for name in ws.by_name.keys() {
        if import_path == name || import_path.starts_with(&format!("{name}/")) {
            if best_name.is_none_or(|b| name.len() > b.len()) {
                best_name = Some(name);
            }
        }
    }
    let best_name = best_name?;
    let dir = ws.by_name.get(best_name)?;
    let subpath = &import_path[best_name.len()..]; // '' or '/widgets'
    // A bare member import resolves straight to the entry its manifest
    // names (ohpm `main`, upstream `b635dd46` 220-227): the caller's
    // as-written check hits it without extension or index guessing.
    if subpath.is_empty()
        && let Some(entry) = ws.entry_by_name.get(best_name)
    {
        return Some(entry.clone());
    }
    Some(collapse_slashes(&format!("{dir}{subpath}")))
}

fn collapse_slashes(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut prev_slash = false;
    for ch in path.chars() {
        if ch == '/' {
            if !prev_slash {
                out.push(ch);
            }
            prev_slash = true;
        } else {
            out.push(ch);
            prev_slash = false;
        }
    }
    out
}

/// Read workspace glob patterns from package.json + pnpm-workspace.yaml
/// (`readWorkspaceGlobs`, `workspace-packages.ts:89-118`).
fn read_workspace_globs(project_root: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    note_read(WorkspaceInput::File(
        Path::new(project_root).join("package.json"),
    ));
    if let Ok(text) = std::fs::read_to_string(Path::new(project_root).join("package.json")) {
        if let Ok(pkg) = serde_json::from_str::<Value>(&text) {
            match pkg.get("workspaces") {
                Some(Value::Array(ws)) => {
                    out.extend(ws.iter().filter_map(Value::as_str).map(str::to_string));
                }
                Some(Value::Object(obj)) => {
                    if let Some(Value::Array(pkgs)) = obj.get("packages") {
                        out.extend(pkgs.iter().filter_map(Value::as_str).map(str::to_string));
                    }
                }
                _ => {}
            }
        }
    }

    note_read(WorkspaceInput::File(
        Path::new(project_root).join("pnpm-workspace.yaml"),
    ));
    if let Ok(yaml) = std::fs::read_to_string(Path::new(project_root).join("pnpm-workspace.yaml")) {
        out.extend(parse_pnpm_packages(&yaml));
    }

    out
}

/// Minimal pnpm-workspace.yaml `packages:` extractor
/// (`parsePnpmPackages`, `workspace-packages.ts:128-148`).
fn parse_pnpm_packages(yaml: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_packages = false;
    for line in yaml.lines() {
        let trimmed_start = line.trim_start();
        if trimmed_start.starts_with("packages") && line.trim_start().contains(':') {
            // Match `^\s*packages\s*:`.
            let after = trimmed_start.trim_start_matches("packages");
            if after.trim_start().starts_with(':') {
                in_packages = true;
                continue;
            }
        }
        if in_packages {
            let t = line.trim_start();
            if let Some(rest) = t.strip_prefix('-') {
                let item = rest.trim();
                let item = item.trim_matches(|c| c == '\'' || c == '"');
                out.push(item.to_string());
                continue;
            }
            // A non-list, non-blank, non-indented line ends the block.
            if !line.trim().is_empty() && !line.starts_with(char::is_whitespace) {
                in_packages = false;
            }
        }
    }
    out
}

/// Expand one level of a `packages/*` glob to member dirs
/// (`expandWorkspaceGlob`, `workspace-packages.ts:151-170`).
fn expand_workspace_glob(project_root: &str, pattern: &str) -> Vec<String> {
    let norm = pattern.replace('\\', "/");
    let norm = norm.trim_end_matches('/');
    let Some(star) = norm.find('*') else {
        return vec![norm.to_string()]; // exact directory
    };

    let base = norm[..star].trim_end_matches('/').to_string();
    let dir = if base.is_empty() {
        Path::new(project_root).to_path_buf()
    } else {
        Path::new(project_root).join(&base)
    };
    note_read(WorkspaceInput::MemberListing(dir.clone()));
    // Deterministic order: read_dir is FS-ordered; `member_listing` sorts so
    // `or_insert` first-wins is stable.
    member_listing(&dir)
        .into_iter()
        .map(|name| {
            if base.is_empty() {
                name
            } else {
                format!("{base}/{name}")
            }
        })
        .collect()
}

/// Read the `name` field from a member directory's package.json
/// (`readPackageName`, `workspace-packages.ts:173-180`).
fn read_package_name(dir_abs: &Path) -> Option<String> {
    note_read(WorkspaceInput::File(dir_abs.join("package.json")));
    let text = std::fs::read_to_string(dir_abs.join("package.json")).ok()?;
    let pkg: Value = serde_json::from_str(&text).ok()?;
    pkg.get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let mut p = std::env::temp_dir();
        p.push(format!("cg-ws-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p).expect("mkdir temp");
        p
    }

    fn write_pkg(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("package.json"),
            format!("{{ \"name\": \"{name}\" }}"),
        )
        .unwrap();
    }

    #[test]
    fn workspace_inputs_are_every_file_read_and_folder_listed() {
        let root = temp_dir("inputs");
        std::fs::write(
            root.join("package.json"),
            "{ \"workspaces\": [\"packages/*\"] }",
        )
        .unwrap();
        write_pkg(&root.join("packages/ui"), "@acme/ui");
        std::fs::create_dir_all(root.join("packages/bare")).unwrap();
        std::fs::create_dir_all(root.join("packages/node_modules")).unwrap();
        std::fs::write(root.join("packages/README.md"), "members\n").unwrap();
        assert_eq!(
            workspace_inputs(&root.to_string_lossy()),
            [
                WorkspaceInput::File(root.join("package.json")),
                WorkspaceInput::File(root.join("pnpm-workspace.yaml")),
                WorkspaceInput::MemberListing(root.join("packages")),
                WorkspaceInput::File(root.join("packages/bare/package.json")),
                WorkspaceInput::File(root.join("packages/ui/package.json")),
            ]
        );
        // The listing is the member folders: no file, no `node_modules`.
        assert_eq!(member_listing(&root.join("packages")), ["bare", "ui"]);
        // No workspace: only the two root files are read.
        std::fs::write(root.join("package.json"), "{ \"name\": \"solo\" }").unwrap();
        assert_eq!(
            workspace_inputs(&root.to_string_lossy()),
            [
                WorkspaceInput::File(root.join("package.json")),
                WorkspaceInput::File(root.join("pnpm-workspace.yaml")),
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn collapse_slashes_dedupes() {
        assert_eq!(collapse_slashes("a//b///c"), "a/b/c");
        assert_eq!(collapse_slashes("a/b"), "a/b");
        assert_eq!(collapse_slashes("/a//"), "/a/");
    }

    #[test]
    fn parse_pnpm_packages_extracts_list() {
        let yaml = "packages:\n  - 'packages/*'\n  - \"apps/*\"\n  - libs/one\nother: x\n";
        let out = parse_pnpm_packages(yaml);
        assert_eq!(out, vec!["packages/*", "apps/*", "libs/one"]);
    }

    #[test]
    fn parse_pnpm_packages_block_ends_on_dedent() {
        let yaml = "packages:\n  - a/*\nnextkey:\n  - b/*\n";
        let out = parse_pnpm_packages(yaml);
        assert_eq!(out, vec!["a/*"]);
    }

    #[test]
    fn parse_pnpm_packages_no_packages_key() {
        assert!(parse_pnpm_packages("foo:\n  - bar\n").is_empty());
    }

    #[test]
    fn expand_workspace_glob_exact_dir_no_star() {
        let root = temp_dir("exact");
        let out = expand_workspace_glob(root.to_str().unwrap(), "packages/ui");
        assert_eq!(out, vec!["packages/ui"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn expand_workspace_glob_expands_star() {
        let root = temp_dir("star");
        write_pkg(&root.join("packages/a"), "@s/a");
        write_pkg(&root.join("packages/b"), "@s/b");
        std::fs::create_dir_all(root.join("packages/node_modules")).unwrap();
        std::fs::create_dir_all(root.join("packages/.hidden")).unwrap();
        let mut out = expand_workspace_glob(root.to_str().unwrap(), "packages/*");
        out.sort();
        assert_eq!(out, vec!["packages/a", "packages/b"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn expand_workspace_glob_star_at_root() {
        let root = temp_dir("rootstar");
        write_pkg(&root.join("alpha"), "alpha");
        let mut out = expand_workspace_glob(root.to_str().unwrap(), "*");
        out.sort();
        assert!(out.contains(&"alpha".to_string()));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn expand_workspace_glob_missing_dir_empty() {
        let root = temp_dir("missing");
        let out = expand_workspace_glob(root.to_str().unwrap(), "nope/*");
        assert!(out.is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn read_package_name_reads_and_filters_empty() {
        let root = temp_dir("readname");
        write_pkg(&root.join("p"), "mypkg");
        assert_eq!(
            read_package_name(&root.join("p")),
            Some("mypkg".to_string())
        );
        std::fs::write(root.join("p").join("package.json"), r#"{ "name": "" }"#).unwrap();
        assert!(read_package_name(&root.join("p")).is_none());
        std::fs::write(root.join("p").join("package.json"), r#"{ }"#).unwrap();
        assert!(read_package_name(&root.join("p")).is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_workspace_packages_from_package_json_array() {
        let root = temp_dir("wsarr");
        std::fs::write(
            root.join("package.json"),
            r#"{ "workspaces": ["packages/*"] }"#,
        )
        .unwrap();
        write_pkg(&root.join("packages/ui"), "@app/ui");
        write_pkg(&root.join("packages/core"), "@app/core");
        let ws = load_workspace_packages(root.to_str().unwrap()).expect("ws");
        assert_eq!(ws.by_name.get("@app/ui"), Some(&"packages/ui".to_string()));
        assert_eq!(
            ws.by_name.get("@app/core"),
            Some(&"packages/core".to_string())
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_workspace_packages_from_object_form() {
        let root = temp_dir("wsobj");
        std::fs::write(
            root.join("package.json"),
            r#"{ "workspaces": { "packages": ["libs/*"] } }"#,
        )
        .unwrap();
        write_pkg(&root.join("libs/x"), "x");
        let ws = load_workspace_packages(root.to_str().unwrap()).expect("ws");
        assert_eq!(ws.by_name.get("x"), Some(&"libs/x".to_string()));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_workspace_packages_from_pnpm_yaml() {
        let root = temp_dir("wspnpm");
        std::fs::write(
            root.join("pnpm-workspace.yaml"),
            "packages:\n  - 'apps/*'\n",
        )
        .unwrap();
        write_pkg(&root.join("apps/web"), "web");
        let ws = load_workspace_packages(root.to_str().unwrap()).expect("ws");
        assert_eq!(ws.by_name.get("web"), Some(&"apps/web".to_string()));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_workspace_packages_none_for_single_repo() {
        let root = temp_dir("single");
        std::fs::write(root.join("package.json"), r#"{ "name": "solo" }"#).unwrap();
        assert!(load_workspace_packages(root.to_str().unwrap()).is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_workspace_packages_none_when_members_have_no_name() {
        let root = temp_dir("noname");
        std::fs::write(
            root.join("package.json"),
            r#"{ "workspaces": ["packages/*"] }"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("packages/nada")).unwrap();
        assert!(load_workspace_packages(root.to_str().unwrap()).is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_workspace_packages_first_declaration_wins() {
        let root = temp_dir("dup");
        std::fs::write(
            root.join("package.json"),
            r#"{ "workspaces": ["packages/*"] }"#,
        )
        .unwrap();
        write_pkg(&root.join("packages/aaa"), "dup");
        write_pkg(&root.join("packages/bbb"), "dup");
        let ws = load_workspace_packages(root.to_str().unwrap()).expect("ws");
        // BTreeMap iteration + `or_insert` = first key alphabetically wins.
        assert_eq!(ws.by_name.get("dup"), Some(&"packages/aaa".to_string()));
        std::fs::remove_dir_all(&root).ok();
    }

    fn sample_ws() -> WorkspacePackages {
        let mut by_name = BTreeMap::new();
        by_name.insert("@app/ui".to_string(), "packages/ui".to_string());
        by_name.insert("@app/ui-core".to_string(), "packages/ui-core".to_string());
        WorkspacePackages {
            by_name,
            entry_by_name: BTreeMap::new(),
        }
    }

    #[test]
    fn resolve_workspace_import_exact_name() {
        assert_eq!(
            resolve_workspace_import("@app/ui", &sample_ws()),
            Some("packages/ui".to_string())
        );
    }

    #[test]
    fn resolve_workspace_import_subpath() {
        assert_eq!(
            resolve_workspace_import("@app/ui/widgets", &sample_ws()),
            Some("packages/ui/widgets".to_string())
        );
    }

    #[test]
    fn resolve_workspace_import_longest_match_wins() {
        assert_eq!(
            resolve_workspace_import("@app/ui-core/x", &sample_ws()),
            Some("packages/ui-core/x".to_string())
        );
    }

    #[test]
    fn resolve_workspace_import_no_match() {
        assert!(resolve_workspace_import("react", &sample_ws()).is_none());
        // Prefix but not a full segment boundary.
        assert!(resolve_workspace_import("@app/uixyz", &sample_ws()).is_none());
    }

    #[test]
    fn workspace_packages_derive_debug_clone_eq() {
        let ws = sample_ws();
        assert_eq!(ws.clone(), ws);
        assert!(format!("{ws:?}").contains("WorkspacePackages"));
    }

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// The two modules of upstream's ohpm test (`arkts-resolution.test.ts`
    /// 113-153): `core/data` names its entry, `feature/goods` depends on it
    /// by a `file:` path; both manifests are JSON5 with a comment and
    /// trailing commas.
    fn ohpm_project(tag: &str) -> PathBuf {
        let root = temp_dir(tag);
        write(
            &root,
            "core/data/oh-package.json5",
            "{\n  // ohpm module manifest\n  \"name\": \"data\",\n  \"main\": \"Index.ets\",\n}\n",
        );
        write(
            &root,
            "feature/goods/oh-package.json5",
            "{\n  \"name\": \"goods\",\n  \"dependencies\": {\n    \"data\": \"file:../../core/data\", // local module\n    \"@ohos/axios\": \"^2.0.0\",\n  },\n}\n",
        );
        root
    }

    /// W7-24a1, upstream `b635dd46` `workspace-packages.ts:67-78,106-198`: a
    /// `file:` dependency of an ohpm manifest is a workspace member, named
    /// by the dependency, and a bare import of it resolves to the entry its
    /// own manifest's `main` names. A registry dependency stays external.
    #[test]
    fn w724a1_ohpm_file_dependencies_are_members_with_their_main_entry() {
        let root = ohpm_project("ohpm-members");
        let ws = load_workspace_packages(root.to_str().unwrap()).expect("ohpm members");
        assert_eq!(ws.by_name.get("data"), Some(&"core/data".to_string()));
        assert_eq!(ws.by_name.get("@ohos/axios"), None);
        assert_eq!(
            resolve_workspace_import("data", &ws),
            Some("core/data/Index.ets".to_string())
        );
        // A subpath is under the member's directory, not under its entry.
        assert_eq!(
            resolve_workspace_import("data/src/x", &ws),
            Some("core/data/src/x".to_string())
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Upstream's precision rule: a name two manifests point at different
    /// directories is dropped, not guessed; a target or an entry outside the
    /// project is no member and no entry.
    #[test]
    fn w724a1_ohpm_ambiguous_or_escaping_dependencies_are_dropped() {
        let root = temp_dir("ohpm-drop");
        write(
            &root,
            "app1/oh-package.json5",
            "{ \"dependencies\": { \"common\": \"file:../libs/common1\", \"kept\": \"file:../libs/kept\" } }",
        );
        write(
            &root,
            "app2/oh-package.json5",
            "{ \"dependencies\": { \"common\": \"file:../libs/common2\", \"kept\": \"file:../libs/kept\", \"out\": \"file:../../elsewhere\" } }",
        );
        write(
            &root,
            "libs/kept/oh-package.json5",
            "{ \"name\": \"kept\", \"main\": \"../../../escape/Index.ets\" }",
        );
        let ws = load_workspace_packages(root.to_str().unwrap()).expect("one member");
        assert_eq!(ws.by_name.get("common"), None, "{ws:?}");
        assert_eq!(ws.by_name.get("out"), None, "{ws:?}");
        assert_eq!(ws.by_name.get("kept"), Some(&"libs/kept".to_string()));
        // `main` escapes the project: the bare import names the directory.
        assert_eq!(
            resolve_workspace_import("kept", &ws),
            Some("libs/kept".to_string())
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The walk prunes dot, build and dependency directories and stops six
    /// levels down (upstream `OHPM_SKIP_DIRS`, `OHPM_WALK_MAX_DEPTH`).
    #[test]
    fn w724a1_ohpm_walk_prunes_build_and_dependency_dirs_and_stops_at_depth_six() {
        let root = temp_dir("ohpm-prune");
        let manifest =
            |name: &str| format!("{{ \"dependencies\": {{ \"{name}\": \"file:./m\" }} }}");
        for skipped in [
            "node_modules",
            "oh_modules",
            ".hvigor",
            ".preview",
            "build",
            "dist",
            "out",
            ".git",
        ] {
            write(
                &root,
                &format!("{skipped}/oh-package.json5"),
                &manifest(skipped),
            );
        }
        write(&root, "a/b/c/d/e/f/oh-package.json5", &manifest("depth6"));
        write(&root, "a/b/c/d/e/f/g/oh-package.json5", &manifest("depth7"));
        let ws = load_workspace_packages(root.to_str().unwrap()).expect("the depth-6 member");
        assert_eq!(ws.by_name.keys().collect::<Vec<_>>(), ["depth6"], "{ws:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// KEEP-RUST: the walk is breadth-first in name order, so the directory
    /// budget cuts the same place on every file system (upstream walks in
    /// `readdir` order).
    #[test]
    fn w724a1_ohpm_walk_is_in_name_order_under_the_budget() {
        let root = temp_dir("ohpm-order");
        write(
            &root,
            "a0000/oh-package.json5",
            "{ \"dependencies\": { \"first\": \"file:../t1\" } }",
        );
        for index in 1..8000 {
            std::fs::create_dir_all(root.join(format!("a{index:04}"))).unwrap();
        }
        write(
            &root,
            "zz/oh-package.json5",
            "{ \"dependencies\": { \"last\": \"file:../t2\" } }",
        );
        let ws = load_workspace_packages(root.to_str().unwrap()).expect("the first member");
        assert_eq!(ws.by_name.get("first"), Some(&"t1".to_string()));
        assert_eq!(
            ws.by_name.get("last"),
            None,
            "past the 8000-directory budget"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The walk never follows a link out of the project, whether to a
    /// directory or to a manifest file, reads no manifest over the default
    /// file-size limit, and takes no entry from a member's linked manifest.
    #[cfg(unix)]
    #[test]
    fn w724a1_ohpm_walk_reads_no_link_and_no_oversize_manifest() {
        let root = temp_dir("ohpm-links");
        let outside = temp_dir("ohpm-links-outside");
        write(
            &outside,
            "oh-package.json5",
            "{ \"dependencies\": { \"linked\": \"file:./sub\" } }",
        );
        std::os::unix::fs::symlink(&outside, root.join("linkdir")).unwrap();
        std::fs::create_dir_all(root.join("m")).unwrap();
        std::os::unix::fs::symlink(
            outside.join("oh-package.json5"),
            root.join("m/oh-package.json5"),
        )
        .unwrap();
        let padding = " ".repeat(codegraph_core::config::DEFAULT_MAX_FILE_SIZE as usize);
        write(
            &root,
            "big/oh-package.json5",
            &format!("{{ \"dependencies\": {{ \"big\": \"file:./x\" }} }}{padding}"),
        );
        write(
            &root,
            "real/oh-package.json5",
            "{ \"dependencies\": { \"real\": \"file:./x\" } }",
        );
        write(
            &outside,
            "member/oh-package.json5",
            "{ \"main\": \"Index.ets\" }",
        );
        std::fs::create_dir_all(root.join("real/x")).unwrap();
        std::os::unix::fs::symlink(
            outside.join("member/oh-package.json5"),
            root.join("real/x/oh-package.json5"),
        )
        .unwrap();
        let ws = load_workspace_packages(root.to_str().unwrap()).expect("the real member");
        assert_eq!(ws.by_name.keys().collect::<Vec<_>>(), ["real"], "{ws:?}");
        assert!(ws.entry_by_name.is_empty(), "{ws:?}");
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    /// Every manifest the walk reads is a workspace input, so a sync whose
    /// manifests moved re-resolves the JavaScript family.
    #[test]
    fn w724a1_workspace_inputs_record_each_ohpm_manifest() {
        let root = ohpm_project("ohpm-inputs");
        let inputs = workspace_inputs(&root.to_string_lossy());
        for manifest in [
            "core/data/oh-package.json5",
            "feature/goods/oh-package.json5",
        ] {
            assert!(
                inputs.contains(&WorkspaceInput::File(root.join(manifest))),
                "{manifest} missing from {inputs:?}"
            );
        }
        std::fs::remove_dir_all(&root).ok();
    }
}
