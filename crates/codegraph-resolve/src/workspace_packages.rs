//! JS/TS workspace (monorepo) package resolution.
//!
//! Ports `upstream resolution/workspace-packages.ts`. Maps each
//! monorepo member package's declared `name` to its directory so the import
//! resolver can rewrite `@scope/ui/widgets` → `packages/ui/widgets` instead of
//! flagging it as an external npm specifier (`workspace-packages.ts:1-25`).
//! Scope mirrors the upstream v1: reads `workspaces` (array OR `{ packages: [...] }`)
//! from package.json plus a minimal `pnpm-workspace.yaml`, expands one level of
//! `*`/`**` globs, directory-based subpath resolution (`workspace-packages.ts:17-24`).

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// Member package `name` → directory relative to project root (posix)
/// (`WorkspacePackages`, `workspace-packages.ts:31-34`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePackages {
    /// Ordered map keeps deterministic iteration (the upstream relies only on
    /// first-declaration-wins + longest-match, both order-independent).
    pub by_name: BTreeMap<String, String>,
}

/// Load workspace member packages for `project_root`
/// (`loadWorkspacePackages`, `workspace-packages.ts:45-61`).
///
/// Returns `None` when the project declares no workspaces (single-package case).
pub fn load_workspace_packages(project_root: &str) -> Option<WorkspacePackages> {
    let patterns = read_workspace_globs(project_root);
    if patterns.is_empty() {
        return None;
    }

    let mut by_name: BTreeMap<String, String> = BTreeMap::new();
    for pattern in &patterns {
        for dir in expand_workspace_glob(project_root, pattern) {
            if let Some(pkg_name) = read_package_name(&Path::new(project_root).join(&dir)) {
                // First declaration wins (workspace-packages.ts:54).
                by_name.entry(pkg_name).or_insert(dir);
            }
        }
    }
    if by_name.is_empty() {
        return None;
    }
    Some(WorkspacePackages { by_name })
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
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        out.push(if base.is_empty() {
            name
        } else {
            format!("{base}/{name}")
        });
    }
    out
}

/// Read the `name` field from a member directory's package.json
/// (`readPackageName`, `workspace-packages.ts:173-180`).
fn read_package_name(dir_abs: &Path) -> Option<String> {
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
        WorkspacePackages { by_name }
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
}
