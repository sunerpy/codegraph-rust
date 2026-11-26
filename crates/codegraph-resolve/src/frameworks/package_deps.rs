//! The dependencies a project declares (upstream `package-deps.ts`): in its
//! root `package.json`, in the ones one or two directories down
//! (`apps/web/package.json`, `frontend/package.json`), and in the ones deeper
//! still above JS/TS code (`src/Web/ClientApp/package.json`, upstream #2395).
//! A framework detector that reads only the root misses every monorepo: a
//! Turborepo keeps `next` in `apps/web/`, an ASP.NET solution its Angular app
//! in `src/Web/ClientApp/`, and that framework's resolver then never runs.
//!
//! The index lists source files, never manifests, so the candidates are the
//! directories of what is indexed, probed through the context: every path is
//! a prefix of an indexed project path, so it stays inside the project.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::types::ResolutionContext;

/// Nested manifests read per project, at most: a monorepo with hundreds of
/// packages is sampled, not scanned.
const MAX_MANIFESTS: usize = 24;

/// Directories probed for a manifest below the first two levels, at most.
const MAX_DEEP_PROBES: usize = MAX_MANIFESTS * 8;

/// A file a JavaScript framework's app is written in; its manifest is in a
/// directory above it.
fn is_script_file(path: &str) -> bool {
    let Some((_, extension)) = path.rsplit_once('.') else {
        return false;
    };
    matches!(
        extension,
        "js" | "jsx"
            | "ts"
            | "tsx"
            | "mjs"
            | "mjsx"
            | "mts"
            | "mtsx"
            | "cjs"
            | "cjsx"
            | "cts"
            | "ctsx"
            | "vue"
            | "svelte"
            | "astro"
    )
}

/// The dependency names a `package.json` declares in `dependencies`,
/// `devDependencies` and `peerDependencies`; `None` when it is not JSON (a
/// template, a broken manifest).
pub(crate) fn manifest_dependency_names(content: &str) -> Option<BTreeSet<String>> {
    let manifest: serde_json::Value = serde_json::from_str(content).ok()?;
    let mut names = BTreeSet::new();
    for field in ["dependencies", "devDependencies", "peerDependencies"] {
        if let Some(group) = manifest.get(field).and_then(serde_json::Value::as_object) {
            names.extend(group.keys().cloned());
        }
    }
    Some(names)
}

/// Every dependency name declared in the manifests read (see the module
/// comment), de-duplicated.
pub fn declared_dependencies<C: ResolutionContext + ?Sized>(context: &C) -> BTreeSet<String> {
    let mut files: Vec<String> = context.get_all_files_shared().as_ref().clone();
    // Path order, as the resolver lists them, so the sample does not depend
    // on the order a caller's list comes in.
    files.sort();
    let mut manifests = vec!["package.json".to_string()];
    for dir in shallow_directories(&files) {
        if manifests.len() > MAX_MANIFESTS {
            break;
        }
        let manifest = format!("{dir}package.json");
        if !dir.contains("node_modules") && context.file_exists(&manifest) {
            manifests.push(manifest);
        }
    }
    // An app can sit deeper: an ASP.NET solution's single-page app in
    // `src/Web/ClientApp/`, or apps under a workspace root at `web/ui/`. The
    // slots left go to the directories above JS/TS code, shallowest first.
    if manifests.len() <= MAX_MANIFESTS {
        for dir in deep_script_directories(&files) {
            if manifests.len() > MAX_MANIFESTS {
                break;
            }
            let manifest = format!("{dir}package.json");
            if context.file_exists(&manifest) {
                manifests.push(manifest);
            }
        }
    }
    let mut names = BTreeSet::new();
    for manifest in &manifests {
        if let Some(content) = context.read_file(manifest)
            && let Some(declared) = manifest_dependency_names(&content)
        {
            names.extend(declared);
        }
    }
    names
}

/// Every manifest [`declared_dependencies`] may read for a project whose
/// indexed files are `files`, whether or not it exists: what a sync digests
/// to know when the dependencies a framework is detected by may have moved.
pub fn manifest_candidates(files: &[String]) -> Vec<String> {
    let mut files = files.to_vec();
    files.sort();
    let mut candidates = vec!["package.json".to_string()];
    candidates.extend(
        shallow_directories(&files)
            .into_iter()
            .filter(|dir| !dir.contains("node_modules"))
            .map(|dir| format!("{dir}package.json")),
    );
    candidates.extend(
        deep_script_directories(&files)
            .into_iter()
            .map(|dir| format!("{dir}package.json")),
    );
    candidates
}

/// The first one or two segments of what IS indexed (`apps/`, `apps/web/`),
/// in the order the sorted `files` reach them; the index lists source files,
/// never manifests, so these are where an app's manifest may sit.
fn shallow_directories(files: &[String]) -> Vec<String> {
    let mut dirs: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for file in files {
        let segments: Vec<&str> = file.split('/').collect();
        if segments.len() > 1 {
            let dir = format!("{}/", segments[0]);
            if seen.insert(dir.clone()) {
                dirs.push(dir);
            }
        }
        if segments.len() > 2 {
            let dir = format!("{}/{}/", segments[0], segments[1]);
            if seen.insert(dir.clone()) {
                dirs.push(dir);
            }
        }
        if dirs.len() > MAX_MANIFESTS * 8 {
            break;
        }
    }
    dirs
}

/// The directories three or more levels down that hold indexed JS/TS code or
/// a directory that does, never inside `node_modules`: shallowest first,
/// sorted within a level, at most [`MAX_DEEP_PROBES`].
fn deep_script_directories(files: &[String]) -> Vec<String> {
    let mut levels: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    for file in files {
        if !is_script_file(file) || file.contains("node_modules") {
            continue;
        }
        let mut depth = 0;
        for (slash, _) in file.match_indices('/') {
            depth += 1;
            if depth >= 3 {
                levels
                    .entry(depth)
                    .or_default()
                    .insert(file[..=slash].to_string());
            }
        }
    }
    levels
        .into_values()
        .flatten()
        .take(MAX_DEEP_PROBES)
        .collect()
}

/// Whether any of `deps` is declared anywhere the project's manifests are
/// read.
pub fn depends_on<C: ResolutionContext + ?Sized>(context: &C, deps: &[&str]) -> bool {
    let names = context.declared_dependencies();
    deps.iter().any(|dep| names.contains(*dep))
}

/// Which detected frameworks' extractors apply to a file (upstream #2129
/// `frameworksForFile`): every one, except a framework with
/// [`crate::framework::FrameworkResolver::app_dependencies`] that some
/// manifest in the project declares, which applies only inside an app whose
/// nearest manifest naming any gated framework names it.
#[derive(Debug, Default)]
pub struct AppGating {
    /// Gated frameworks and the packages that mark one of their apps.
    gated: BTreeMap<String, &'static [&'static str]>,
    /// Directory → the dependency names its `package.json` declares, `None`
    /// without a readable one.
    manifests: BTreeMap<String, Option<BTreeSet<String>>>,
    /// `(directory, framework)` → whether the framework applies there.
    applies: BTreeMap<(String, String), bool>,
    /// Largest manifest read, the project's `indexing.max_file_size`, as the
    /// resolution context reads one.
    max_file_size: u64,
}

impl AppGating {
    /// The gating for `frameworks` (name, app dependencies) on a project
    /// whose manifests declare `declared`. A framework is gated only when some
    /// manifest names one of its packages: otherwise detection found it by
    /// other evidence and every file is its app's.
    pub fn new<'a>(
        frameworks: impl IntoIterator<Item = (&'a str, Option<&'static [&'static str]>)>,
        declared: &BTreeSet<String>,
        max_file_size: u64,
    ) -> Self {
        let gated = frameworks
            .into_iter()
            .filter_map(|(name, deps)| {
                let deps = deps?;
                deps.iter()
                    .any(|dep| declared.contains(*dep))
                    .then(|| (name.to_string(), deps))
            })
            .collect();
        Self {
            gated,
            max_file_size,
            ..Self::default()
        }
    }

    /// Whether no framework is gated, so every extractor applies everywhere.
    pub fn is_empty(&self) -> bool {
        self.gated.is_empty()
    }

    /// Whether the framework `name` extracts `file_path` (project-relative):
    /// [`Self::applies_in`] its directory.
    pub fn applies(&mut self, name: &str, file_path: &str, project_root: &Path) -> bool {
        let dir = file_path.rsplit_once('/').map_or("", |(dir, _)| dir);
        self.applies_in(name, dir, project_root)
    }

    /// Whether the framework `name` extracts the files of the directory `dir`
    /// (project-relative, `""` for the root), reading the `package.json` of
    /// `dir` and each enclosing one under `project_root` as needed. The
    /// nearest manifest that names ANY gated framework decides: a root
    /// `package.json` that declares `expo-router` for an example app does not
    /// make a `docs/` Next.js app, whose own manifest declares `next`, an Expo
    /// app.
    pub fn applies_in(&mut self, name: &str, dir: &str, project_root: &Path) -> bool {
        let Some(deps) = self.gated.get(name).copied() else {
            return true;
        };
        let key = (dir.to_string(), name.to_string());
        if let Some(&memo) = self.applies.get(&key) {
            return memo;
        }
        let mut applies = false;
        let mut current = Some(dir);
        while let Some(d) = current {
            current = if d.is_empty() {
                None
            } else {
                Some(d.rsplit_once('/').map_or("", |(parent, _)| parent))
            };
            let Some(declared) =
                declared_in(&mut self.manifests, d, project_root, self.max_file_size)
            else {
                continue;
            };
            if deps.iter().any(|dep| declared.contains(*dep)) {
                applies = true;
                break;
            }
            let names_another = self.gated.iter().any(|(other, other_deps)| {
                other != name && other_deps.iter().any(|dep| declared.contains(*dep))
            });
            if names_another {
                break;
            }
        }
        self.applies.insert(key, applies);
        applies
    }

    /// A digest of where each gated framework applies among the directories
    /// of `files` (project-relative), or `None` when no framework is gated. A
    /// sync compares it with the last index's: a manifest edit that moves an
    /// app's frameworks changes which files make framework nodes, so every
    /// file is extracted again, as a fresh index extracts it.
    pub fn digest<'a>(
        &mut self,
        files: impl IntoIterator<Item = &'a str>,
        project_root: &Path,
    ) -> Option<String> {
        if self.gated.is_empty() {
            return None;
        }
        let dirs: BTreeSet<&str> = files
            .into_iter()
            .map(|file| file.rsplit_once('/').map_or("", |(dir, _)| dir))
            .collect();
        let names: Vec<String> = self.gated.keys().cloned().collect();
        let mut decisions = String::new();
        for name in &names {
            for dir in &dirs {
                let applies = self.applies_in(name, dir, project_root);
                decisions.push_str(&format!("{name}\t{dir}\t{}\n", u8::from(applies)));
            }
        }
        Some(codegraph_core::node_id::hash_content(&decisions))
    }
}

/// The dependency names `<dir>/package.json` declares, read once into
/// `manifests`.
fn declared_in<'a>(
    manifests: &'a mut BTreeMap<String, Option<BTreeSet<String>>>,
    dir: &str,
    project_root: &Path,
    max_file_size: u64,
) -> Option<&'a BTreeSet<String>> {
    if !manifests.contains_key(dir) {
        let relative = if dir.is_empty() {
            "package.json".to_string()
        } else {
            format!("{dir}/package.json")
        };
        let declared = read_manifest(project_root, &relative, max_file_size)
            .as_deref()
            .and_then(manifest_dependency_names);
        manifests.insert(dir.to_string(), declared);
    }
    manifests.get(dir).and_then(Option::as_ref)
}

/// Read `relative` (a project path the index produced) under `project_root`
/// the way the resolution context reads a manifest, after proving it
/// lexically inside the root; `None` when it is absent, no regular file,
/// over `max_file_size` or unreadable.
fn read_manifest(project_root: &Path, relative: &str, max_file_size: u64) -> Option<String> {
    let full =
        crate::pathutil::lexical_path_within_root(&project_root.to_string_lossy(), relative)?;
    if !std::fs::metadata(&full).ok()?.is_file() {
        return None;
    }
    crate::context::read_source_file(&full, relative, max_file_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ImportMapping, ResolutionContext};
    use codegraph_core::types::{Language, Node, NodeKind};
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// A context over an indexed file list and the manifests "on disk",
    /// recording every probe (upstream `package-deps.test.ts`'s `tree`).
    struct Tree {
        files: Vec<String>,
        manifests: HashMap<String, String>,
        probes: RefCell<Vec<String>>,
    }

    impl Tree {
        fn new(files: &[&str], manifests: &[(&str, &str)]) -> Self {
            Self {
                files: files.iter().map(|f| f.to_string()).collect(),
                manifests: manifests
                    .iter()
                    .map(|(path, content)| (path.to_string(), content.to_string()))
                    .collect(),
                probes: RefCell::new(Vec::new()),
            }
        }
    }

    impl ResolutionContext for Tree {
        fn get_nodes_in_file(&self, _file_path: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_nodes_by_name(&self, _name: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_nodes_by_qualified_name(&self, _qualified_name: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_nodes_by_kind(&self, _kind: NodeKind) -> Vec<Node> {
            Vec::new()
        }
        fn file_exists(&self, file_path: &str) -> bool {
            self.probes.borrow_mut().push(file_path.to_string());
            self.manifests.contains_key(file_path)
        }
        fn read_file(&self, file_path: &str) -> Option<String> {
            self.manifests.get(file_path).cloned()
        }
        fn get_project_root(&self) -> &str {
            "/project"
        }
        fn get_all_files(&self) -> Vec<String> {
            self.files.clone()
        }
        fn get_nodes_by_lower_name(&self, _lower_name: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_node_by_id(&self, _id: &str) -> Option<Node> {
            None
        }
        fn get_import_mappings(&self, _file_path: &str, _language: Language) -> Vec<ImportMapping> {
            Vec::new()
        }
    }

    #[test]
    fn reads_a_single_page_app_three_or_four_levels_down() {
        for app in ["src/Web/ClientApp/", "src/Presentation/Web/ClientApp/"] {
            let files = [
                "src/Web/Program.cs".to_string(),
                format!("{app}karma.conf.js"),
                format!("{app}src/main.ts"),
                format!("{app}src/app/app.module.ts"),
            ];
            let files: Vec<&str> = files.iter().map(String::as_str).collect();
            let manifest = format!("{app}package.json");
            let tree = Tree::new(
                &files,
                &[(
                    manifest.as_str(),
                    r#"{"dependencies":{"@angular/core":"^21","@angular/router":"^21"}}"#,
                )],
            );
            assert!(depends_on(&tree, &["@angular/router"]), "{app}");
        }
    }

    #[test]
    fn reads_a_workspace_root_two_levels_down_and_its_apps() {
        let tree = Tree::new(
            &[
                "cmd/prometheus/main.go",
                "web/ui/mantine-ui/src/main.tsx",
                "web/ui/react-app/src/App.tsx",
                "web/ui/module/lezer-promql/src/index.ts",
            ],
            &[
                (
                    "web/ui/package.json",
                    r#"{"devDependencies":{"typescript":"*","vite":"*"}}"#,
                ),
                (
                    "web/ui/mantine-ui/package.json",
                    r#"{"dependencies":{"react":"*","react-router-dom":"*"}}"#,
                ),
                (
                    "web/ui/react-app/package.json",
                    r#"{"dependencies":{"react":"*","react-router":"*"}}"#,
                ),
            ],
        );
        let names: Vec<String> = declared_dependencies(&tree).into_iter().collect();
        assert_eq!(
            names,
            [
                "react",
                "react-router",
                "react-router-dom",
                "typescript",
                "vite"
            ]
        );
    }

    #[test]
    fn never_reads_a_manifest_inside_node_modules() {
        let tree = Tree::new(
            &[
                "tools/site/node_modules/framework/dist/index.js",
                "tools/site/src/index.ts",
            ],
            &[(
                "tools/site/node_modules/framework/package.json",
                r#"{"dependencies":{"next":"*"}}"#,
            )],
        );
        assert!(!depends_on(&tree, &["next"]));
        assert!(
            tree.probes
                .borrow()
                .iter()
                .all(|probe| !probe.contains("node_modules"))
        );
        // Nor one a level or two down that is a dependency's own.
        let tree = Tree::new(
            &["node_modules/next/dist/index.js", "src/index.ts"],
            &[(
                "node_modules/next/package.json",
                r#"{"dependencies":{"react":"*"}}"#,
            )],
        );
        assert!(!depends_on(&tree, &["react"]));
        assert!(
            tree.probes
                .borrow()
                .iter()
                .all(|probe| !probe.contains("node_modules"))
        );
    }

    #[test]
    fn probes_only_above_script_code_and_a_bounded_number_of_directories() {
        let mut files = vec!["src/Api/Tools/Generator.cs".to_string()];
        for i in 0..3000 {
            files.push(format!("src/app/feature{i}/parts/widget.component.ts"));
        }
        let files: Vec<&str> = files.iter().map(String::as_str).collect();
        let tree = Tree::new(
            &files,
            &[(
                "src/Api/Tools/package.json",
                r#"{"dependencies":{"express":"*"}}"#,
            )],
        );
        assert!(!depends_on(&tree, &["express"]));
        // `src/`, `src/Api/`, `src/app/` from the first two levels; the rest
        // below them.
        assert!(tree.probes.borrow().len() <= 3 + MAX_DEEP_PROBES);
    }

    #[test]
    fn reads_the_same_manifests_whatever_order_the_files_come_in() {
        let mut files = Vec::new();
        let mut manifests = Vec::new();
        for i in 0..40 {
            files.push(format!("clients/web/app{i}/src/index.ts"));
            manifests.push((
                format!("clients/web/app{i}/package.json"),
                format!(r#"{{"dependencies":{{"dep-{i}":"*"}}}}"#),
            ));
        }
        let manifests: Vec<(&str, &str)> = manifests
            .iter()
            .map(|(path, content)| (path.as_str(), content.as_str()))
            .collect();
        let forward: Vec<&str> = files.iter().map(String::as_str).collect();
        let mut backward = forward.clone();
        backward.reverse();
        let scanned = declared_dependencies(&Tree::new(&forward, &manifests));
        let reversed = declared_dependencies(&Tree::new(&backward, &manifests));
        assert_eq!(scanned, reversed);
        // A sample: the root and 24 more.
        assert_eq!(scanned.len(), MAX_MANIFESTS);
    }

    /// What a sync digests covers every manifest detection probes or reads,
    /// so an edit to one of them reaches the next sync.
    #[test]
    fn the_candidates_cover_every_manifest_detection_reads() {
        let mut files = vec![
            "src/Web/Program.cs".to_string(),
            "src/Web/ClientApp/src/main.ts".to_string(),
            "web/ui/mantine-ui/src/main.tsx".to_string(),
            "tools/site/node_modules/framework/dist/index.js".to_string(),
            "server/app.py".to_string(),
        ];
        // More directories above script code than are probed deep down,
        // fewer than the first two levels sample.
        for i in 0..120 {
            files.push(format!("clients/app{i}/src/deep/index.ts"));
        }
        let paths: Vec<&str> = files.iter().map(String::as_str).collect();
        let tree = Tree::new(&paths, &[]);
        declared_dependencies(&tree);
        let candidates: BTreeSet<String> = manifest_candidates(&files).into_iter().collect();
        assert!(candidates.contains("package.json"));
        assert!(candidates.contains("server/package.json"));
        for probe in tree.probes.borrow().iter() {
            assert!(
                candidates.contains(probe),
                "{probe} is probed but not a candidate"
            );
        }
        assert!(candidates.iter().all(|path| !path.contains("node_modules")));
        assert!(tree.probes.borrow().len() > MAX_DEEP_PROBES);
    }

    /// The digest moves only when a manifest moves an app onto or off a
    /// gated framework, and there is none when nothing is gated.
    #[test]
    fn the_digest_moves_with_an_app_and_not_with_a_version() {
        let files = [
            "apps/mobile/app/index.tsx",
            "apps/web/app/page.tsx",
            "README.md",
        ];
        let digest = |web: &str| {
            let disk = Disk::new(
                "digest",
                &[
                    (
                        "apps/mobile/package.json",
                        r#"{"dependencies":{"expo-router":"*"}}"#,
                    ),
                    ("apps/web/package.json", web),
                ],
            );
            gating(&["expo-router", "next"]).digest(files, &disk.0)
        };
        let next = digest(r#"{"dependencies":{"next":"14"}}"#);
        assert!(next.is_some());
        assert_eq!(next, digest(r#"{"dependencies":{"next":"15"}}"#));
        assert_ne!(next, digest(r#"{"dependencies":{"expo-router":"*"}}"#));
        let disk = Disk::new("digest-none", &[("package.json", "{}")]);
        assert_eq!(gating(&["react"]).digest(files, &disk.0), None);
    }

    #[test]
    fn peer_dependencies_count_and_a_broken_manifest_declares_nothing() {
        assert_eq!(
            manifest_dependency_names(r#"{"peerDependencies":{"react":"*"}}"#),
            Some(BTreeSet::from(["react".to_string()]))
        );
        assert_eq!(manifest_dependency_names("{{ template }}"), None);
    }

    /// A temporary project tree on disk for [`AppGating`].
    struct Disk(std::path::PathBuf);

    impl Disk {
        fn new(label: &str, files: &[(&str, &str)]) -> Self {
            let root = std::env::temp_dir().join(format!(
                "codegraph-app-gating-{label}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            for (path, content) in files {
                let full = root.join(path);
                std::fs::create_dir_all(full.parent().unwrap()).unwrap();
                std::fs::write(full, content).unwrap();
            }
            Self(root)
        }
    }

    impl Drop for Disk {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const EXPO: &[&str] = &["expo-router"];
    const NEXT: &[&str] = &["next"];

    fn gating(declared: &[&str]) -> AppGating {
        let declared = declared.iter().map(|d| d.to_string()).collect();
        AppGating::new(
            [
                ("expo-router", Some(EXPO)),
                ("nextjs", Some(NEXT)),
                ("react", None),
            ],
            &declared,
            codegraph_core::config::DEFAULT_MAX_FILE_SIZE,
        )
    }

    /// Upstream #2129: an Expo app beside a Next.js app; each router reads
    /// only its own app, and an ungated framework reads every file.
    #[test]
    fn a_file_based_router_reads_only_its_own_app() {
        let disk = Disk::new(
            "monorepo",
            &[
                ("package.json", r#"{"name":"mono","workspaces":["apps/*"]}"#),
                (
                    "apps/mobile/package.json",
                    r#"{"dependencies":{"expo":"*","expo-router":"*","react":"*"}}"#,
                ),
                (
                    "apps/web/package.json",
                    r#"{"dependencies":{"next":"*","react":"*"}}"#,
                ),
            ],
        );
        let mut gating = gating(&["expo-router", "next", "react"]);
        assert!(gating.applies("expo-router", "apps/mobile/app/index.tsx", &disk.0));
        assert!(!gating.applies("expo-router", "apps/web/app/page.tsx", &disk.0));
        assert!(gating.applies("nextjs", "apps/web/app/user/[id]/page.tsx", &disk.0));
        assert!(!gating.applies("nextjs", "apps/mobile/app/_layout.tsx", &disk.0));
        assert!(gating.applies("react", "apps/web/app/page.tsx", &disk.0));
        assert!(gating.applies("react", "apps/mobile/app/index.tsx", &disk.0));
    }

    /// The nearest manifest that names a gated framework decides: a Next.js
    /// docs app under a root that declares Expo Router for its example app.
    #[test]
    fn the_nearest_manifest_naming_a_router_decides() {
        let disk = Disk::new(
            "nearest",
            &[
                (
                    "package.json",
                    r#"{"devDependencies":{"expo":"*","expo-router":"*","react":"*"}}"#,
                ),
                (
                    "docs/package.json",
                    r#"{"dependencies":{"next":"*","react":"*"}}"#,
                ),
            ],
        );
        let mut gating = gating(&["expo-router", "next", "react"]);
        assert!(gating.applies("nextjs", "docs/app/layout.tsx", &disk.0));
        assert!(!gating.applies("expo-router", "docs/app/layout.tsx", &disk.0));
        assert!(gating.applies("expo-router", "example/app/sheet.tsx", &disk.0));
        assert!(!gating.applies("nextjs", "example/app/sheet.tsx", &disk.0));
    }

    /// A framework no manifest names was detected by other evidence: it is
    /// not gated, so it reads every file.
    #[test]
    fn a_framework_no_manifest_names_reads_every_file() {
        let disk = Disk::new("undeclared", &[("package.json", r#"{"name":"demo"}"#)]);
        let mut gating = gating(&["react"]);
        assert!(gating.is_empty());
        assert!(gating.applies("expo-router", "app/settings.tsx", &disk.0));
    }
}
