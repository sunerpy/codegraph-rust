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
