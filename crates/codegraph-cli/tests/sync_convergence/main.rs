//! Cross-language sync-versus-fresh convergence matrix.
//!
//! Every case writes a `before` tree, indexes it with the built
//! `codegraph init`, applies one edit, and brings the index up to date through
//! two drivers:
//!
//! - `cli`: `codegraph sync`, the full-scan path a user or a git hook runs;
//! - `watch`: the file watcher's path, `codegraph_watch::sync_changed_paths`
//!   with exactly the changed paths. When an edit touches a watcher control
//!   file (the index root's `config.toml` or `codegraph.json`, or the root
//!   `.gitignore`), the real watcher reloads its scope and runs one full
//!   reconcile instead (`watcher.rs`, the control-file branch of the event
//!   loop), so the driver does the same.
//!
//! The oracle writes the final tree into a separate directory and runs a fresh
//! `init` from an empty index. Each driver's database must equal the oracle's
//! under `canonicalize_db` and `diff_canonical`: every node, file, edge and
//! unresolved reference by its full natural key, plus the schema text. Nothing
//! is compared by count, and no existing database is ever re-indexed.
//!
//! Each case also names the edges its `before` and fresh indexes must hold, so
//! a case cannot pass by exercising nothing. A known gap is an `#[ignore]`d case
//! whose reason names the plan item that fixes it, or says `unscheduled` when no
//! item has taken it yet; run with `-- --ignored`, it fails.
//! `sync_matrix_covers_every_extracted_language` fails when a language with an
//! extractor has no case that runs, and `sync_matrix_covers_every_change_kind`
//! when a [`Change`] has none.

#[path = "../probes/support.rs"]
mod support;

mod cases;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use codegraph_bench::oracle::{canonicalize_db, diff_canonical};
use codegraph_core::types::Language;

use support::{Graph, Indexed, Project};

/// The kind of change a case applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    /// A file appears that satisfies an import or include an unchanged file
    /// already has.
    AddImportTarget,
    /// Only a body changes; every declaration keeps its identity.
    BodyEdit,
    /// A declaration is renamed in its own file, under unchanged references.
    RenameSymbol,
    /// A module an unchanged file imports is deleted.
    DeleteImportedModule,
    /// An `extends`/`implements`-style clause changes.
    EditHeritage,
    /// A second declaration of an already-referenced name appears elsewhere.
    AddCompetitor,
    /// A project control file changes: `package.json`, `tsconfig.json`, the
    /// root `.gitignore`, `.git/info/exclude`, the index root's
    /// `config.toml` or `codegraph.json`, or a Shopify theme marker
    /// (`layout/theme.liquid`, `config/settings_schema.json`).
    ControlFile,
    /// A `.h` header changes from C to C++ content, or an `.inc` include
    /// between PHP and Pascal content.
    HeaderFlip,
    /// A `.js` file gains Flow's `@flow` pragma, so it is read as TSX.
    PragmaFlip,
    /// A module's re-exports change: a name an unchanged file imports
    /// through it now means another definition, though no node of that
    /// name changed.
    ReexportEdit,
}

impl Change {
    const ALL: [Self; 10] = [
        Self::AddImportTarget,
        Self::BodyEdit,
        Self::RenameSymbol,
        Self::DeleteImportedModule,
        Self::EditHeritage,
        Self::AddCompetitor,
        Self::ControlFile,
        Self::HeaderFlip,
        Self::PragmaFlip,
        Self::ReexportEdit,
    ];
}

/// One file operation of a case's edit.
pub enum Op {
    Write(&'static str, &'static str),
    Remove(&'static str),
    Rename(&'static str, &'static str),
}

impl Op {
    /// The paths a watcher event batch for this operation names.
    fn paths(&self) -> Vec<&'static str> {
        match *self {
            Self::Write(path, _) | Self::Remove(path) => vec![path],
            Self::Rename(from, to) => vec![from, to],
        }
    }
}

type EdgeSpec = (&'static str, &'static str, &'static str);

/// A `before` tree, one edit, and the edges that prove the case is live.
pub struct Case {
    change: Change,
    languages: Vec<Language>,
    before: Vec<(&'static str, &'static str)>,
    edit: Vec<Op>,
    edges_before: Vec<EdgeSpec>,
    edges_after: Vec<EdgeSpec>,
    absent_after: Vec<EdgeSpec>,
    languages_after: Vec<(&'static str, &'static str)>,
}

impl Case {
    /// A case of `change` that exercises `languages`; every one of them must
    /// be the language of an indexed file or node.
    pub fn new(change: Change, languages: &[Language]) -> Self {
        Self {
            change,
            languages: languages.to_vec(),
            before: Vec::new(),
            edit: Vec::new(),
            edges_before: Vec::new(),
            edges_after: Vec::new(),
            absent_after: Vec::new(),
            languages_after: Vec::new(),
        }
    }

    /// A file of the `before` tree.
    pub fn file(mut self, path: &'static str, contents: &'static str) -> Self {
        self.before.push((path, contents));
        self
    }

    /// Edit: add or rewrite a file.
    pub fn write(mut self, path: &'static str, contents: &'static str) -> Self {
        self.edit.push(Op::Write(path, contents));
        self
    }

    /// Edit: delete a file.
    pub fn remove(mut self, path: &'static str) -> Self {
        self.edit.push(Op::Remove(path));
        self
    }

    /// Edit: move a file.
    pub fn rename(mut self, from: &'static str, to: &'static str) -> Self {
        self.edit.push(Op::Rename(from, to));
        self
    }

    /// An edge the `before` index must hold, in the probe selector notation.
    pub fn edge_before(
        mut self,
        source: &'static str,
        kind: &'static str,
        target: &'static str,
    ) -> Self {
        self.edges_before.push((source, kind, target));
        self
    }

    /// An edge a fresh index of the final tree must hold.
    pub fn edge_after(
        mut self,
        source: &'static str,
        kind: &'static str,
        target: &'static str,
    ) -> Self {
        self.edges_after.push((source, kind, target));
        self
    }

    /// An edge a fresh index of the final tree must NOT hold, between nodes it
    /// does hold.
    pub fn no_edge_after(
        mut self,
        source: &'static str,
        kind: &'static str,
        target: &'static str,
    ) -> Self {
        self.absent_after.push((source, kind, target));
        self
    }

    /// The language a fresh index of the final tree records for file `path`.
    pub fn file_language_after(mut self, path: &'static str, language: &'static str) -> Self {
        self.languages_after.push((path, language));
        self
    }

    /// An edge both the `before` index and a fresh index of the final tree
    /// must hold.
    pub fn edge_throughout(
        self,
        source: &'static str,
        kind: &'static str,
        target: &'static str,
    ) -> Self {
        self.edge_before(source, kind, target)
            .edge_after(source, kind, target)
    }

    /// The final tree: `before` with the edit applied.
    #[track_caller]
    fn final_tree(&self) -> BTreeMap<String, String> {
        let mut tree = BTreeMap::new();
        for (path, contents) in &self.before {
            assert!(
                tree.insert(path.to_string(), contents.to_string())
                    .is_none(),
                "`before` lists {path} twice"
            );
        }
        assert!(!self.edit.is_empty(), "a case needs an edit");
        for op in &self.edit {
            match *op {
                Op::Write(path, contents) => {
                    tree.insert(path.to_string(), contents.to_string());
                }
                Op::Remove(path) => {
                    assert!(
                        tree.remove(path).is_some(),
                        "removes {path}, which does not exist"
                    );
                }
                Op::Rename(from, to) => {
                    let contents = tree
                        .remove(from)
                        .unwrap_or_else(|| panic!("renames {from}, which does not exist"));
                    assert!(
                        tree.insert(to.to_string(), contents).is_none(),
                        "renames {from} over the existing {to}"
                    );
                }
            }
        }
        tree
    }

    fn index_before(&self, label: &str) -> Indexed {
        self.before
            .iter()
            .fold(Project::named(label), |project, (path, contents)| {
                project.file(path, contents)
            })
            .index()
    }

    /// Applies the edit on disk and returns the paths a watcher would report.
    fn apply(&self, project: &mut Indexed) -> Vec<&'static str> {
        let mut changed = Vec::new();
        for op in &self.edit {
            match *op {
                Op::Write(path, contents) => {
                    project.write(path, contents);
                }
                Op::Remove(path) => {
                    project.remove(path);
                }
                Op::Rename(from, to) => {
                    project.rename(from, to);
                }
            }
            for path in op.paths() {
                if !changed.contains(&path) {
                    changed.push(path);
                }
            }
        }
        changed
    }
}

/// Runs one case through both drivers and compares each with a fresh index.
fn run_case(name: &str, case: &Case) {
    let final_tree = case.final_tree();

    let mut cli = case.index_before("cli");
    expect_edges(name, "before", cli.graph(), &case.edges_before);
    let mut languages = indexed_languages(cli.graph());
    case.apply(&mut cli);
    cli.sync();

    let mut watch = case.index_before("watch");
    let changed = case.apply(&mut watch);
    watcher_sync(&watch, &changed);
    watch.reload();

    let fresh = final_tree
        .iter()
        .fold(Project::named("fresh"), |project, (path, contents)| {
            project.file(path, contents)
        })
        .index();
    expect_edges(name, "fresh", fresh.graph(), &case.edges_after);
    expect_no_edges(name, "fresh", fresh.graph(), &case.absent_after);
    for &(path, language) in &case.languages_after {
        let recorded = fresh
            .graph()
            .files
            .iter()
            .find(|file| file.path == path)
            .map(|file| file.language.as_str());
        assert_eq!(
            recorded,
            Some(language),
            "[{name}] the fresh index records {path} as {recorded:?}"
        );
    }
    languages.extend(indexed_languages(fresh.graph()));
    for language in &case.languages {
        assert!(
            languages.contains(language.as_str()),
            "[{name}] declares {language}, but neither the before nor the fresh index has a \
             {language} file or node; indexed: {languages:?}"
        );
    }
    for project in [&cli, &watch] {
        assert_tree(name, project.root(), &final_tree);
    }

    let expected = canonicalize_db(&fresh.db()).expect("canonicalize the fresh index");
    let mut divergent = Vec::new();
    for (driver, project) in [("cli", &cli), ("watch", &watch)] {
        let actual = canonicalize_db(&project.db()).expect("canonicalize the synced index");
        if let Err(diff) = diff_canonical(&expected, &actual, None) {
            divergent.push(format!(
                "=== {driver} driver: {} canonical difference(s) from a fresh index\n{}{diff}",
                diff.entries().len(),
                readable_difference(fresh.graph(), project.graph()),
            ));
        }
    }
    // The module-surface rows and their version marker (W4a-F3) are no
    // canonical surface, but the next sync reads them: a sync leaves them as a
    // fresh index writes them.
    let surface_rows = support::project_metadata_with_prefix(&fresh.db(), "js_module_surface");
    // So is the C/C++ dependents marker (W4-CPP-04): every sync that
    // succeeded leaves it as a fresh index writes it, and the namespace rows
    // (W4-CPP-07) with it.
    let cpp_marker = support::project_metadata_with_prefix(&fresh.db(), "cpp_type_surface");
    let namespace_rows =
        support::project_metadata_with_prefix(&fresh.db(), "cpp_namespace_surface");
    // The Java head rows and their marker (W4-JVM-01), one prefix for both.
    let head_rows = support::project_metadata_with_prefix(&fresh.db(), "java_type_heads");
    // And the Kotlin surface rows and their marker (W4-JVM-03).
    let kotlin_rows = support::project_metadata_with_prefix(&fresh.db(), "kotlin_surface");
    // And the Scala surface rows and their marker (W4-SC-01).
    let scala_rows = support::project_metadata_with_prefix(&fresh.db(), "scala_surface");
    // And the C# surface rows and their marker (W4-CS-01).
    let csharp_rows = support::project_metadata_with_prefix(&fresh.db(), "csharp_surface");
    // And the member surface rows and their marker (W4-CS-03).
    let member_rows = support::project_metadata_with_prefix(&fresh.db(), "member_surface");
    // And the Ruby head rows and their marker (W4-RB-01).
    let ruby_rows = support::project_metadata_with_prefix(&fresh.db(), "ruby_heads");
    // And the CFML component rows and their marker (W4-CF-01).
    let cfml_rows = support::project_metadata_with_prefix(&fresh.db(), "cfml_heads");
    // And the Solidity head rows and their marker (W4-SOL-01).
    let solidity_rows = support::project_metadata_with_prefix(&fresh.db(), "solidity_heads");
    // And the Swift head rows and their marker (W4-SW-01).
    let swift_rows = support::project_metadata_with_prefix(&fresh.db(), "swift_heads");
    // And the Objective-C head rows and their marker (W4-SW-04).
    let objc_rows = support::project_metadata_with_prefix(&fresh.db(), "objc_heads");
    // And the Go head rows and their marker (W4-GO-01).
    let go_rows = support::project_metadata_with_prefix(&fresh.db(), "go_heads");
    // And the PHP head rows and their marker (W4-PHP-01).
    let php_rows = support::project_metadata_with_prefix(&fresh.db(), "php_heads");
    let lua_rows = support::project_metadata_with_prefix(&fresh.db(), "lua_module");
    let dart_rows = support::project_metadata_with_prefix(&fresh.db(), "dart_heads");
    // And the Dart library rows (W4-DART-02).
    let dart_library_rows = support::project_metadata_with_prefix(&fresh.db(), "dart_library");
    // And the Shopify theme row and its marker (W4-LIQ-01).
    let shopify_rows = support::project_metadata_with_prefix(&fresh.db(), "shopify_themes");
    for (driver, project) in [("cli", &cli), ("watch", &watch)] {
        let rows = support::project_metadata_with_prefix(&project.db(), "js_module_surface");
        if rows != surface_rows {
            divergent.push(format!(
                "=== {driver} driver: module-surface rows differ from a fresh index\n\
                 synced: {rows:#?}\nfresh: {surface_rows:#?}"
            ));
        }
        let marker = support::project_metadata_with_prefix(&project.db(), "cpp_type_surface");
        if marker != cpp_marker {
            divergent.push(format!(
                "=== {driver} driver: the C/C++ dependents marker differs from a fresh index\n\
                 synced: {marker:#?}\nfresh: {cpp_marker:#?}"
            ));
        }
        let rows = support::project_metadata_with_prefix(&project.db(), "cpp_namespace_surface");
        if rows != namespace_rows {
            divergent.push(format!(
                "=== {driver} driver: the C/C++ namespace rows differ from a fresh index\n\
                 synced: {rows:#?}\nfresh: {namespace_rows:#?}"
            ));
        }
        let rows = support::project_metadata_with_prefix(&project.db(), "java_type_heads");
        if rows != head_rows {
            divergent.push(format!(
                "=== {driver} driver: the Java head rows differ from a fresh index\n\
                 synced: {rows:#?}\nfresh: {head_rows:#?}"
            ));
        }
        let rows = support::project_metadata_with_prefix(&project.db(), "kotlin_surface");
        if rows != kotlin_rows {
            divergent.push(format!(
                "=== {driver} driver: the Kotlin surface rows differ from a fresh index\n\
                 synced: {rows:#?}\nfresh: {kotlin_rows:#?}"
            ));
        }
        let rows = support::project_metadata_with_prefix(&project.db(), "scala_surface");
        if rows != scala_rows {
            divergent.push(format!(
                "=== {driver} driver: the Scala surface rows differ from a fresh index\n\
                 synced: {rows:#?}\nfresh: {scala_rows:#?}"
            ));
        }
        let rows = support::project_metadata_with_prefix(&project.db(), "csharp_surface");
        if rows != csharp_rows {
            divergent.push(format!(
                "=== {driver} driver: the C# surface rows differ from a fresh index\n\
                 synced: {rows:#?}\nfresh: {csharp_rows:#?}"
            ));
        }
        let rows = support::project_metadata_with_prefix(&project.db(), "member_surface");
        if rows != member_rows {
            divergent.push(format!(
                "=== {driver} driver: the member surface rows differ from a fresh index\n\
                 synced: {rows:#?}\nfresh: {member_rows:#?}"
            ));
        }
        let rows = support::project_metadata_with_prefix(&project.db(), "ruby_heads");
        if rows != ruby_rows {
            divergent.push(format!(
                "=== {driver} driver: the Ruby head rows differ from a fresh index\n\
                 synced: {rows:#?}\nfresh: {ruby_rows:#?}"
            ));
        }
        let rows = support::project_metadata_with_prefix(&project.db(), "cfml_heads");
        if rows != cfml_rows {
            divergent.push(format!(
                "=== {driver} driver: the CFML component rows differ from a fresh index\n\
                 synced: {rows:#?}\nfresh: {cfml_rows:#?}"
            ));
        }
        let rows = support::project_metadata_with_prefix(&project.db(), "solidity_heads");
        if rows != solidity_rows {
            divergent.push(format!(
                "=== {driver} driver: the Solidity head rows differ from a fresh index\n\
                 synced: {rows:#?}\nfresh: {solidity_rows:#?}"
            ));
        }
        let rows = support::project_metadata_with_prefix(&project.db(), "swift_heads");
        if rows != swift_rows {
            divergent.push(format!(
                "=== {driver} driver: the Swift head rows differ from a fresh index\n\
                 synced: {rows:#?}\nfresh: {swift_rows:#?}"
            ));
        }
        let rows = support::project_metadata_with_prefix(&project.db(), "objc_heads");
        if rows != objc_rows {
            divergent.push(format!(
                "=== {driver} driver: the Objective-C head rows differ from a fresh index\n\
                 synced: {rows:#?}\nfresh: {objc_rows:#?}"
            ));
        }
        let rows = support::project_metadata_with_prefix(&project.db(), "go_heads");
        if rows != go_rows {
            divergent.push(format!(
                "=== {driver} driver: the Go head rows differ from a fresh index\n\
                 synced: {rows:#?}\nfresh: {go_rows:#?}"
            ));
        }
        let rows = support::project_metadata_with_prefix(&project.db(), "php_heads");
        if rows != php_rows {
            divergent.push(format!(
                "=== {driver} driver: the PHP head rows differ from a fresh index\n\
                 synced: {rows:#?}\nfresh: {php_rows:#?}"
            ));
        }
        let rows = support::project_metadata_with_prefix(&project.db(), "lua_module");
        if rows != lua_rows {
            divergent.push(format!(
                "=== {driver} driver: the Lua module rows differ from a fresh index\n\
                 synced: {rows:#?}\nfresh: {lua_rows:#?}"
            ));
        }
        let rows = support::project_metadata_with_prefix(&project.db(), "dart_heads");
        if rows != dart_rows {
            divergent.push(format!(
                "=== {driver} driver: the Dart head rows differ from a fresh index\n\
                 synced: {rows:#?}\nfresh: {dart_rows:#?}"
            ));
        }
        let rows = support::project_metadata_with_prefix(&project.db(), "dart_library");
        if rows != dart_library_rows {
            divergent.push(format!(
                "=== {driver} driver: the Dart library rows differ from a fresh index\n\
                 synced: {rows:#?}\nfresh: {dart_library_rows:#?}"
            ));
        }
        let rows = support::project_metadata_with_prefix(&project.db(), "shopify_themes");
        if rows != shopify_rows {
            divergent.push(format!(
                "=== {driver} driver: the Shopify theme row differs from a fresh index\n\
                 synced: {rows:#?}\nfresh: {shopify_rows:#?}"
            ));
        }
    }
    assert!(
        divergent.is_empty(),
        "[{name}] incremental sync after a {:?} edit diverges from a fresh index\n\
         edit: {}\n{}\n=== fresh index\n{}",
        case.change,
        describe_edit(&case.edit),
        divergent.join("\n"),
        fresh.graph().report(),
    );
}

/// The watcher-path driver: what the real watcher runs for one event batch.
fn watcher_sync(project: &Indexed, changed: &[&str]) {
    let root = project.root();
    let paths = support::index_paths(root);
    let controls = [
        paths.config_toml(),
        paths.extension_config(),
        paths.project().join(".gitignore"),
        paths
            .project()
            .join(codegraph_extract::engine::REPOSITORY_EXCLUDE),
    ]
    .map(|path| relative_slash(paths.project(), &path));
    let outcome = if changed
        .iter()
        .any(|path| controls.iter().any(|control| control == path))
    {
        codegraph_watch::sync_project_once(root)
    } else {
        codegraph_watch::sync_changed_paths(
            root,
            paths.current_db(),
            changed
                .iter()
                .map(|path| root.join(support::checked_relative(path))),
        )
    };
    outcome.unwrap_or_else(|err| panic!("watcher-path sync of {}: {err:#}", root.display()));
}

fn relative_slash(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or_else(|_| panic!("{} is outside {}", path.display(), root.display()))
        .to_string_lossy()
        .replace('\\', "/")
}

#[track_caller]
fn expect_edges(name: &str, label: &str, graph: &Graph, edges: &[EdgeSpec]) {
    for &(source, kind, target) in edges {
        assert!(
            !graph.edges_matching(source, kind, target).is_empty(),
            "[{name}] the {label} index lacks {source} -[{kind}]-> {target}, which this case \
             exists to exercise\n{}",
            graph.report()
        );
    }
}

#[track_caller]
fn expect_no_edges(name: &str, label: &str, graph: &Graph, edges: &[EdgeSpec]) {
    for &(source, kind, target) in edges {
        for selector in [source, target] {
            assert!(
                !graph.nodes_matching(selector).is_empty(),
                "[{name}] the {label} index has no `{selector}`, so the absence of \
                 {source} -[{kind}]-> {target} would hold vacuously\n{}",
                graph.report()
            );
        }
        assert!(
            graph.edges_matching(source, kind, target).is_empty(),
            "[{name}] the {label} index holds {source} -[{kind}]-> {target}, which this case \
             exists to rule out\n{}",
            graph.report()
        );
    }
}

/// The languages of a graph's files and nodes. Both count: a `.h` file keeps
/// the `c` file language while its nodes may be `cpp`.
fn indexed_languages(graph: &Graph) -> BTreeSet<String> {
    graph
        .files
        .iter()
        .map(|file| file.language.clone())
        .chain(graph.nodes.iter().map(|node| node.language.clone()))
        .collect()
}

/// The project tree on disk, the index's own files aside, must be exactly the
/// tree the oracle indexed.
#[track_caller]
fn assert_tree(name: &str, root: &Path, expected: &BTreeMap<String, String>) {
    let paths = support::index_paths(root);
    let index_root = relative_slash(paths.project(), paths.current_root());
    let controls = [paths.config_toml(), paths.extension_config()]
        .map(|path| relative_slash(paths.project(), &path));
    let mut actual = BTreeMap::new();
    let mut pending = vec![paths.project().to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).expect("read a project directory") {
            let path: PathBuf = entry.expect("read a directory entry").path();
            let relative = relative_slash(paths.project(), &path);
            let in_index =
                relative == index_root || relative.starts_with(&format!("{index_root}/"));
            if path.is_dir() {
                if !in_index || relative == index_root {
                    pending.push(path);
                }
            } else if !in_index || controls.contains(&relative) {
                let contents = fs::read_to_string(&path).expect("read a project file");
                actual.insert(relative, contents);
            }
        }
    }
    assert!(
        actual == *expected,
        "[{name}] the tree under {} is not the tree the fresh index was built from\n\
         on disk: {:?}\nexpected: {:?}",
        root.display(),
        actual.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>()
    );
}

/// The rows each graph holds that the other lacks, in selector notation.
fn readable_difference(fresh: &Graph, synced: &Graph) -> String {
    fn rows(graph: &Graph) -> Vec<String> {
        let mut rows = graph
            .nodes
            .iter()
            .map(|node| format!("node  {}", graph.describe(&node.id)))
            .chain(
                graph
                    .edges
                    .iter()
                    .map(|edge| format!("edge  {}", graph.describe_edge(edge))),
            )
            .chain(
                graph
                    .unresolved
                    .iter()
                    .map(|reference| format!("ref   {}", graph.describe_ref(reference))),
            )
            .chain(
                graph
                    .files
                    .iter()
                    .map(|file| format!("file  {} ({})", file.path, file.language)),
            )
            .collect::<Vec<_>>();
        rows.sort();
        rows
    }
    let fresh_rows = rows(fresh);
    let synced_rows = rows(synced);
    let mut text = String::new();
    for (title, from, other) in [
        ("only in the fresh index", &fresh_rows, &synced_rows),
        ("only in the synced index", &synced_rows, &fresh_rows),
    ] {
        let mut remaining = other.clone();
        let mut only = Vec::new();
        for row in from {
            match remaining.iter().position(|candidate| candidate == row) {
                Some(index) => {
                    remaining.remove(index);
                }
                None => only.push(row.as_str()),
            }
        }
        text.push_str(&format!("{title} ({}):\n", only.len()));
        for row in only {
            text.push_str(&format!("    {row}\n"));
        }
    }
    text
}

fn describe_edit(edit: &[Op]) -> String {
    edit.iter()
        .map(|op| match *op {
            Op::Write(path, _) => format!("write {path}"),
            Op::Remove(path) => format!("remove {path}"),
            Op::Rename(from, to) => format!("rename {from} -> {to}"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Every language a file can be extracted in: a tree-sitter spec or an
/// embedded extractor. YAML, Twig, Properties and the three Godot formats have
/// no extractor (they get a file node; the Godot resolver reads the latter), so
/// the gate does not require them; a GDScript case still indexes a Godot
/// project and scene.
fn extracted_languages() -> Vec<Language> {
    Language::ALL
        .into_iter()
        .filter(|&language| {
            codegraph_extract::lang::spec_for_language(language).is_some()
                || codegraph_extract::embedded::extract_embedded("coverage-probe", "", language)
                    .is_some()
        })
        .collect()
}

macro_rules! gap {
    () => {
        None
    };
    ($gap:literal) => {
        Some($gap)
    };
}

/// Registers each case function of [`cases`] as one test, `#[ignore]`d with
/// its known-gap reason where one is given, and lists them all for the
/// coverage gates.
macro_rules! matrix {
    ($($(#[ignore = $gap:literal])? $name:ident,)*) => {
        $(
            #[test]
            $(#[ignore = $gap])?
            fn $name() {
                run_case(stringify!($name), &cases::$name());
            }
        )*

        fn registered() -> Vec<(&'static str, Option<&'static str>, Case)> {
            vec![$((stringify!($name), gap!($($gap)?), cases::$name()),)*]
        }
    };
}

// Every `#[ignore]` below fails when run. A sync retries an unresolved
// reference only when its `reference_name` equals the bare name of a node the
// changed files added or removed (`unresolved_refs_by_names`), so a reference
// spelled as an alias, a module path or a qualified name stays unresolved after
// the file that satisfies it appears. `package.json` and `tsconfig.json` are not
// source files: when only one of them changes, neither driver re-resolves
// anything.
matrix! {
    typescript_named_import_of_an_added_file,
    typescript_default_import_of_an_added_file,
    typescript_aliased_named_import_of_an_added_file,
    typescript_aliased_named_import_gains_its_export,
    tsx_aliased_component_import_gains_its_export,
    typescript_aliased_named_import_leaves_a_namesake_when_its_module_gains_the_export,
    typescript_aliased_named_import_leaves_an_in_module_namesake_when_its_module_gains_the_export,
    typescript_aliased_named_import_loses_its_export,
    tsx_component_import_of_an_added_file,
    python_from_import_of_an_added_module,
    php_include_of_an_added_file,
    dart_import_of_an_added_library,
    csharp_using_of_an_added_namespace,
    java_import_of_an_added_class,
    lua_require_of_an_added_module,
    typescript_out_of_repo_call_ignores_an_added_namesake,
    svelte_lib_alias_call_reaches_an_added_module,
    vue_bare_call_ignores_an_added_interface_property,
    javascript_unexported_namesake_appears,
    svelte_instance_script_namesake_appears,
    javascript_parameter_shadows_an_added_function,
    typescript_destructured_composable_call_reaches_an_added_module,
    typescript_module_destructuring_call_body_edit,
    vue_own_function_shadows_a_nuxt_auto_import,
    typescript_value_reference_survives_an_added_namesake,
    javascript_global_namespace_member_appears,
    javascript_global_namespace_member_renamed,
    javascript_object_literal_this_call_body_edit,
    tsx_tag_becomes_a_parameter_body_edit,
    typescript_interface_moves_into_a_declare_module_body,
    vue_options_api_this_call_body_edit,
    vue_template_call_reaches_an_added_composable,
    javascript_fetch_body_call_ignores_an_added_method,
    python_bare_call_ignores_an_added_method,
    python_outside_import_ignores_an_added_function,
    python_call_becomes_a_local_binding_body_edit,
    python_star_reexport_of_an_added_module,
    python_package_gains_a_star_reexport,
    python_package_drops_a_star_reexport,
    python_reexporting_package_is_deleted,
    python_named_reexport_moves_to_another_module,
    python_module_member_reexport_appears,
    python_dotted_module_class_of_an_added_module,
    python_self_call_ignores_an_added_import_target,
    python_fixture_returns_another_class,
    python_conftest_plugs_a_fixture_module,
    python_plugged_fixture_returns_another_class,
    python_aliased_plugged_fixture_returns_another_class,
    python_closer_conftest_overrides_a_fixture,
    python_django_manager_result_body_edit,
    python_module_global_retyped_in_its_module,
    python_module_global_gains_an_external_write,
    python_module_global_loses_an_unknown_write,
    python_module_global_writer_is_deleted,
    javascript_require_imports_an_added_module,
    javascript_require_member_reaches_an_added_module,
    typescript_require_default_of_an_added_es_module,
    typescript_namespace_reexport_member_reaches_an_added_module,
    typescript_namespace_reexport_names_another_module,
    typescript_named_reexport_names_another_module,
    typescript_nested_wildcard_reexport_names_another_module,
    typescript_local_export_alias_names_another_declaration,
    javascript_commonjs_forward_names_another_module,
    typescript_barrel_gains_a_named_reexport,
    javascript_export_list_drops_a_binding,
    javascript_export_list_gains_a_binding,
    javascript_sealed_module_gains_a_commonjs_export,
    svelte_instance_script_becomes_a_module_script,
    typescript_alias_forwarded_barrel_is_deleted,
    typescript_workspace_entry_barrel_is_renamed,
    javascript_barrel_chain_follows_the_importer_language,
    cpp_attribute_macro_class_body_edit,
    cpp_broken_tree_class_body_edit,
    cpp_declared_base_class_heritage_edit,
    cpp_alias_retarget_moves_an_unchanged_receiver,
    cpp_template_alias_retarget_moves_an_unchanged_receiver,
    cpp_alias_chain_retarget_moves_an_unchanged_receiver,
    cpp_header_member_type_moves_an_unchanged_receiver,
    cpp_inner_class_added_hides_an_outer_alias,
    cpp_alias_in_an_unincluded_header_makes_it_ambiguous,
    cpp_base_change_moves_an_inherited_alias,
    cpp_base_change_reaches_an_out_of_line_method_of_a_derived_class,
    cpp_batch_rewrites_a_base_before_dropping_a_derivation,
    cpp_source_file_namesake_does_not_take_a_base,
    cpp_using_declaration_names_the_base,
    cpp_header_member_becomes_a_library_type,
    cpp_project_declares_a_lowercase_outside_type,
    cpp_nested_template_member_type_moves_an_unchanged_receiver,
    cpp_cross_file_namespace_alias_retarget,
    cpp_macro_opener_opens_another_namespace,
    cpp_macro_alias_moves_an_opener_it_feeds,
    cpp_macro_closer_moves_a_declaration_out_of_the_namespace,
    cpp_free_operator_moves_out_of_the_macro_namespace,
    cpp_opener_definition_stops_opening_a_namespace,
    c_call_ignores_an_added_cpp_method,
    objc_c_call_ignores_an_added_method,
    cpp_macro_visibility_through_a_deep_include_graph,
    cpp_lowercase_macro_class_body_edit,
    c_auto_local_initializer_call_body_edit,
    java_subclass_gains_a_base_its_caller_reaches,
    typescript_dollar_import_reaches_an_added_module,
    typescript_dollar_default_export_names_another_declaration,
    java_super_call_in_an_override_body_edit,
    java_annotation_ignores_a_case_variant_method_body_edit,
    typescript_outside_type_call_ignores_an_added_method,
    cpp_class_template_receiver_body_edit,
    csharp_extension_method_call_body_edit,
    csharp_method_becomes_an_extension_method,
    csharp_extension_overload_of_a_renamed_type_parameter_appears,
    csharp_extension_overload_under_another_constraint_appears,
    csharp_extension_overload_spelled_with_other_spacing_appears,
    csharp_extension_overload_with_a_raw_string_default_appears,
    python_inherited_classmethod_body_edit,
    python_class_method_of_an_added_class,
    python_inherited_classmethod_of_an_added_base,
    typescript_test_suite_namesake_appears,
    java_overload_delegation_body_edit,
    python_call_ignores_a_react_hook_body_edit,
    typescript_type_parameter_added_body_edit,
    python_function_value_bound_by_an_added_parameter,
    javascript_minified_script_becomes_readable,
    javascript_readable_script_becomes_minified,
    typescript_side_effect_import_of_an_added_file,
    go_import_of_an_added_package,
    solidity_import_of_an_added_file,
    svelte_import_of_an_added_component,
    vue_import_of_an_added_component,
    go_call_into_an_added_file_of_the_same_package,
    typescript_collapsed_parse_is_fixed,
    go_composite_literal_of_an_added_struct,
    go_parameter_type_of_an_added_struct,
    java_parameter_type_of_an_added_class,
    csharp_parameter_type_of_an_added_class,
    kotlin_property_type_of_an_added_class,
    php_property_type_of_an_added_class,
    c_include_of_an_added_header,
    liquid_render_of_an_added_snippet,
    liquid_json_template_section_of_an_added_section,
    razor_reference_to_an_added_class,
    r_source_of_an_added_file,
    #[ignore = "unscheduled, found by W0-04: an Erlang remote call `util:double/1` is not retried when module util appears"]
    erlang_remote_call_into_an_added_module,
    #[ignore = "unscheduled, found by W0-04: a MyBatis include of another mapper's fragment is not retried when that mapper appears"]
    mybatis_include_of_an_added_fragment,
    typescript_pr2288_same_named_methods_keep_typed_callers,
    java_overloads_keep_their_callers,
    jsx_component_body_edit,
    swift_method_body_edit,
    swift_extension_becomes_a_declaration,
    swift_subclass_scope_follows_a_superclass_change_in_another_file,
    swift_conformer_head_moves_an_extension_method_call,
    swift_declaration_of_an_extended_type_appears,
    swift_extension_beside_a_reference_keeps_the_declaration,
    swift_type_path_call_follows_an_extension_path_change,
    swift_type_path_call_reaches_an_extension_that_now_writes_its_path,
    swift_type_path_call_reaches_a_member_its_type_gains,
    swift_type_path_call_reaches_a_member_an_extension_gains_past_the_tail_ceiling,
    swift_type_path_call_follows_a_second_namesake_an_extension_gains,
    swift_type_path_call_drops_a_namesake_an_extension_loses,
    swift_bare_call_follows_a_superclass_change_in_another_file,
    swift_typed_property_follows_an_inherited_declaration,
    go_embedded_method_follows_an_embedding_change_in_another_file,
    go_field_chain_follows_a_field_type_change_in_another_file,
    go_field_chain_reaches_a_retyped_field_past_the_tail_ceiling,
    go_field_chain_follows_an_import_retarget_in_the_declaring_file,
    go_embedded_method_follows_an_import_retarget_in_the_declaring_file,
    go_unexported_receiver_reaches_a_method_its_package_gains,
    go_qualified_type_reference_reaches_a_package_type_that_appears,
    go_alias_method_follows_an_alias_target_change_in_another_file,
    go_alias_chain_follows_a_middle_alias_change_in_another_file,
    go_alias_method_reaches_the_aliased_type_once_the_alias_appears,
    go_defined_type_reaches_a_type_its_package_gains,
    go_external_qualified_reference_never_reaches_a_namesake_that_appears,
    go_asserted_call_follows_an_embedding_change_in_another_file,
    go_asserted_call_reaches_a_method_the_asserted_type_gains,
    go_type_position_reaches_a_type_its_package_gains,
    objc_self_send_follows_a_superclass_change_in_another_file,
    objc_super_send_follows_the_header_of_its_class,
    objc_typed_property_receiver_follows_a_declared_type_change,
    objc_typed_property_receiver_reaches_an_owner_its_type_comes_to_inherit,
    objc_typed_local_reaches_an_inherited_method_past_the_tail_ceiling,
    objc_category_send_reaches_a_system_superclass_category_that_appears,
    objc_namesake_protocol_keeps_its_own_header_when_a_copy_appears,
    php_this_call_follows_an_ancestor_extends_change,
    typescript_line_shift_above_a_function_keeps_namesake_edges_elsewhere,
    php_line_shift_in_a_base_class_keeps_its_callers,
    c_static_function_becomes_visible_with_a_line_added_above,
    php_base_this_call_leaves_a_deeper_subclass_method_once_a_middle_class_is_rebased,
    php_this_call_reaches_a_method_once_a_middle_class_extends_its_class,
    php_this_call_follows_a_trait_use_change_in_an_ancestor,
    php_parent_call_follows_an_import_retarget_in_an_ancestor_file,
    php_this_call_leaves_a_repository_trait_once_the_outside_base_appears,
    php_this_call_leaves_a_method_once_its_trait_becomes_a_class,
    php_this_call_reaches_a_method_once_its_class_becomes_a_trait,
    php_this_call_reaches_a_repository_trait_once_its_imported_base_is_deleted,
    php_untyped_receiver_leaves_a_method_its_class_stops_inheriting,
    php_untyped_receiver_reaches_an_inherited_method_past_the_tail_ceiling,
    php_untyped_receiver_reaches_a_method_its_ancestor_gains_past_the_tail_ceiling,
    php_untyped_receiver_leaves_a_method_a_seed_pushes_past_the_walk_limit,
    php_bare_call_reaches_a_function_that_appears,
    php_bare_class_name_reaches_a_class_added_to_its_namespace,
    php_namespace_alias_instantiation_reaches_a_class_that_appears,
    php_qualified_static_call_leaves_a_class_moved_to_another_namespace,
    php_qualified_static_call_reaches_a_class_that_appears_past_the_tail_ceiling,
    php_qualified_static_call_reaches_a_method_its_class_gains_past_the_tail_ceiling,
    php_function_called_from_the_global_namespace_reaches_it_once_it_appears,
    php_call_in_another_case_reaches_a_function_that_appears,
    pascal_call_in_another_case_reaches_a_function_that_appears,
    lua_function_made_local_leaves_its_callers_in_another_file,
    lua_local_function_made_global_reaches_its_callers_in_another_file,
    lua_alias_reaches_a_member_its_module_starts_exporting,
    lua_alias_reaches_a_member_re_exported_from_a_module_two_away,
    lua_alias_reaches_a_member_of_a_module_that_appears,
    lua_alias_of_a_library_function_reaches_the_patch_that_appears,
    lua_alias_of_a_library_function_leaves_the_patch_once_a_second_appears,
    lua_alias_through_a_module_reaches_the_library_patch_that_appears,
    lua_alias_through_a_changed_module_reaches_the_library_patch_that_appears,
    lua_alias_of_a_host_table_member_reaches_the_method_that_appears,
    lua_alias_through_a_commented_spread_or_multi_target_require_reaches_a_new_export,
    lua_alias_reaches_a_new_export_through_a_middle_module_edited_in_the_same_sync,
    lua_the_first_of_two_local_namesakes_made_global_reaches_its_callers_in_another_file,
    lua_alias_follows_the_first_of_two_same_named_aliases_after_they_swap,
    r_test_file_variable_stays_out_of_another_test_that_calls_its_name,
    r_test_variable_moved_to_a_helper_reaches_the_other_tests,
    r_base_named_binding_whose_line_gains_a_function_reaches_its_callers,
    r_base_named_binding_whose_line_loses_its_function_leaves_its_callers,
    r_bare_call_stays_off_an_objects_method_that_appears,
    dart_bare_call_reaches_a_method_of_a_base_its_superclass_starts_to_extend,
    dart_bare_call_leaves_a_method_of_a_base_its_superclass_stops_extending,
    dart_bare_call_reaches_an_extension_member_once_it_is_on_the_callers_base,
    dart_value_call_named_like_a_constructor_reaches_an_added_extension_member,
    dart_value_call_named_like_a_constructor_leaves_a_removed_extension_member,
    dart_bare_call_reaches_an_extension_member_once_its_superclass_extends_the_on_type,
    dart_bare_call_reaches_a_member_whose_base_its_superclass_brings_nearer,
    dart_bare_call_leaves_a_member_pushed_past_the_walk_limit,
    dart_core_method_call_leaves_an_extension_moved_to_another_type,
    dart_bare_call_leaves_an_extension_member_once_the_extension_is_unnamed,
    dart_core_method_call_reaches_an_extension_moved_onto_its_type_past_the_tail_ceiling,
    dart_export_show_gains_a_name_an_unchanged_importer_calls,
    dart_export_hide_drops_a_name_an_unchanged_importer_calls,
    dart_part_joins_its_library_and_shadows_an_imported_name,
    dart_library_import_reaches_its_unchanged_part,
    dart_library_import_leaves_its_unchanged_part,
    dart_import_prefix_swap_moves_an_unchanged_parts_type,
    dart_library_name_brings_in_an_unchanged_part,
    dart_library_of_an_unchanged_part_appears,
    dart_same_named_package_gains_its_first_dart_file,
    dart_delete_a_part_that_shadowed_an_imported_name,
    dart_unnamed_extension_reaches_a_part_once_its_file_joins_the_library,
    dart_import_of_an_added_library_past_the_import_key_ceiling,
    dart_two_barrels_trade_an_exported_name,
    dart_library_directives_move_down_a_line,
    dart_hidden_base_type_leaves_a_call_on_its_subclass,
    dart_shown_base_type_brings_its_method_to_a_call_on_its_subclass_past_the_tail_ceiling,
    dart_library_import_brings_a_base_method_to_a_call_on_its_parts_subclass,
    dart_getter_read_reaches_a_getter_its_type_gains,
    dart_getter_read_reaches_a_getter_its_type_gains_past_the_tail_ceiling,
    dart_getter_read_leaves_a_getter_its_type_drops,
    dart_getter_read_through_an_inherited_field_follows_its_retyping,
    dart_getter_read_follows_its_receiver_types_new_base,
    dart_call_on_an_enum_reaches_a_member_an_added_extension_brings,
    dart_call_on_an_enum_reaches_a_member_an_added_extension_brings_past_the_tail_ceiling,
    dart_getter_read_reaches_a_getter_an_added_extension_brings_past_the_tail_ceiling,
    dart_call_through_a_type_reaches_the_static_constant_it_gains,
    dart_call_through_a_type_reaches_the_static_constant_it_gains_past_the_tail_ceiling,
    dart_initializer_calls_reach_a_function_added_in_another_file,
    dart_field_and_extension_types_reach_a_type_added_in_another_file,
    dart_getter_read_through_a_deleted_type_links_nothing,
    dart_getter_read_through_a_type_that_appears_reaches_its_base,
    dart_call_through_a_type_that_appears_reaches_its_member_past_the_tail_ceiling,
    dart_getter_read_through_an_inherited_field_follows_its_types_new_base,
    dart_getter_read_leaves_a_base_of_its_own_file_its_receivers_type_stops_extending,
    dart_getter_read_through_a_field_its_class_inherits_from_a_new_base,
    dart_getter_read_reaches_a_getter_an_existing_extension_gains_past_the_tail_ceiling,
    dart_chain_link_follows_a_return_type_retyped_in_another_file,
    dart_chain_link_follows_a_top_level_functions_return_type,
    dart_chain_link_follows_a_static_constants_initializer,
    dart_chain_link_follows_the_end_of_a_long_top_level_initializer,
    dart_chain_link_after_a_universal_extension_it_gains,
    dart_chain_link_leaves_a_method_that_becomes_a_setter,
    dart_chain_link_follows_a_methods_own_type_parameters,
    dart_chain_link_through_fields_of_a_call_result_follows_a_retyping,
    dart_chain_link_through_a_local_follows_its_initializers_return_type,
    dart_chain_link_follows_a_retyped_call_method,
    dart_chain_link_through_a_constant_follows_the_function_its_initializer_calls,
    dart_chain_link_through_a_field_initializer_follows_the_function_it_calls,
    dart_chain_link_after_an_extension_getter_its_receiver_gains,
    dart_chain_link_leaves_a_member_a_base_of_an_inner_type_drops,
    dart_chain_link_after_a_universal_extension_it_gains_past_the_tail_ceiling,
    dart_chain_link_follows_a_class_head_that_stops_declaring_a_type_parameter,
    dart_chain_link_through_a_local_of_a_deleted_type_links_nothing,
    dart_chain_link_reaches_an_unnamed_extension_once_its_file_joins_the_library,
    dart_chain_link_in_a_type_follows_a_base_its_base_gains,
    dart_chain_link_leaves_an_extension_that_moves_to_another_type,
    dart_chain_link_through_a_constant_an_extension_types_leaves_with_it,
    dart_chain_link_through_a_function_typed_by_a_class_whose_head_changes,
    dart_chain_link_through_a_static_constant_follows_a_part_joining_its_library,
    dart_chain_link_headed_by_a_local_follows_a_type_of_its_name,
    dart_chain_link_reads_a_field_its_type_gains_through_a_base,
    dart_chain_link_reaches_an_extension_on_a_base_its_type_gains,
    dart_chain_link_reaches_an_extension_on_any_type_once_its_type_comes,
    dart_parameter_call_links_nothing_when_a_namesake_appears,
    dart_handler_parameter_call_links_nothing_when_its_base_gains_the_method,
    dart_local_function_call_keeps_its_local_when_a_namesake_appears,
    dart_call_through_a_type_reaches_a_const_constructor_it_gains,
    dart_bare_call_leaves_a_method_that_becomes_a_constructor,
    dart_bare_call_reaches_a_constructor_that_becomes_a_method,
    dart_call_through_a_type_reaches_a_factory_added_below_a_getter_of_its_name,
    dart_prefixed_call_reaches_a_name_its_barrel_starts_to_show,
    dart_prefixed_call_leaves_a_name_its_barrel_stops_showing,
    dart_prefixed_call_in_a_part_follows_a_prefix_its_library_gains,
    dart_chain_through_a_prefix_reaches_the_member_its_type_gains,
    dart_annotation_reaches_a_constant_its_imported_library_gains,
    dart_prefixed_call_reaches_a_function_its_library_gains_past_the_tail_ceiling,
    dart_prefixed_call_reaches_a_name_its_barrel_starts_to_show_past_the_tail_ceiling,
    #[ignore = "W5-06: a pubspec.yaml change does not re-resolve the project's Dart files"]
    dart_pubspec_rename_strands_a_package_import,
    #[ignore = "W5-06: a pubspec.yaml change does not re-resolve the project's Dart files"]
    dart_pubspec_path_dependency_picks_a_same_named_package,
    php_call_in_another_case_leaves_a_function_once_a_namesake_in_another_case_appears,
    php_function_called_in_another_case_from_the_global_namespace_reaches_it_once_it_appears,
    #[ignore = "W5-07: a call through a class name reads its supertypes from the class heads, which no sync lane records"]
    php_inherited_static_call_follows_a_class_head_spread_past_its_third_line,
    #[ignore = "W5-07: a call through a class name reads its supertypes from the class heads, which no sync lane records"]
    python_inherited_classmethod_follows_a_base_change_in_another_file,
    #[ignore = "W5-07: a call through a class name reads its supertypes from the class heads, which no sync lane records"]
    python_inherited_classmethod_follows_a_base_change_beside_its_old_target,
    #[ignore = "W5-07: a call through a class name reads its supertypes from the class heads, which no sync lane records"]
    java_inherited_static_call_follows_a_base_change_in_another_file,
    #[ignore = "W5-07: a call through a class name reads its supertypes from the class heads, which no sync lane records"]
    csharp_inherited_static_call_follows_a_base_change_in_another_file,
    #[ignore = "W5-07: a call through a class name reads its supertypes from the class heads, which no sync lane records"]
    swift_inherited_class_method_follows_a_base_change_in_another_file,
    typescript_value_reader_body_edit,
    astro_frontmatter_call_body_edit,
    go_value_initializer_body_edit,
    cpp_scoped_read_body_edit,
    java_static_field_read_body_edit,
    ruby_class_constant_reader_body_edit,
    kotlin_object_constant_reader_body_edit,
    cpp_qualified_call_body_edit,
    vue_component_template_edit,
    liquid_snippet_body_edit,
    luau_module_function_body_edit,
    vue_script_function_body_edit,
    svelte_instance_script_call_edit,
    ruby_receiver_call_body_edit,
    objc_implementation_body_edit,
    gdscript_autoload_method_body_edit,
    typescript_rename_an_imported_function,
    javascript_rename_a_function_used_by_import_and_require,
    cpp_rename_a_member_function,
    csharp_rename_a_called_method,
    svelte_rename_an_imported_function,
    pascal_rename_a_unit_function,
    scala_rename_a_called_method,
    terraform_rename_a_variable,
    erlang_rename_a_remotely_called_function,
    erlang_imported_call_reaches_a_function_its_module_gains,
    erlang_bare_call_reaches_a_function_its_header_gains,
    erlang_imported_call_reaches_a_function_its_module_gains_past_the_tail_ceiling,
    mybatis_rename_an_included_fragment,
    typescript_delete_an_imported_module,
    rust_delete_a_used_module,
    rust_glob_module_gains_an_item,
    rust_out_of_scope_namesake_appears,
    rust_prelude_name_ignores_a_project_namesake_that_appears,
    rust_glob_variant_leaves_when_its_enum_is_renamed,
    rust_variant_path_reaches_the_enum_once_it_declares_the_variant,
    rust_variant_path_leaves_the_enum_when_the_variant_is_renamed,
    rust_outside_type_receiver_reaches_a_type_the_project_gains,
    rust_outside_type_receiver_leaves_when_the_project_type_is_renamed,
    rust_alias_receiver_reaches_a_method_once_the_alias_appears,
    rust_alias_receiver_leaves_the_method_when_the_alias_goes,
    #[ignore = "W5-06: a Cargo.toml change does not re-resolve the project's Rust files"]
    rust_cargo_dependency_appears,
    dart_delete_an_imported_library,
    astro_delete_an_imported_module,
    lua_delete_a_required_module,
    nix_delete_an_imported_file,
    python_move_an_imported_module_away,
    typescript_change_a_superclass,
    java_change_a_superclass_and_interface,
    java_bare_call_follows_a_base_change_in_another_file,
    java_enum_constant_body_edit,
    java_lombok_data_appears_on_a_class,
    java_lombok_hand_written_getter_replaces_the_generated_one,
    java_lombok_field_removal_drops_its_accessor,
    kotlin_enum_entry_body_edit,
    kotlin_bare_call_follows_a_base_change_in_another_file,
    kotlin_receiver_type_appears_in_another_file,
    kotlin_lambda_receiver_changes_in_another_file,
    kotlin_import_package_appears_in_the_project,
    kotlin_gradle_script_owner_becomes_a_project_class,
    kotlin_platform_ancestry_chain_follows_a_project_head_change,
    kotlin_base_body_edit_keeps_a_bare_call,
    kotlin_batch_rewrites_a_base_and_a_middle_type,
    kotlin_lambda_receiver_changes_through_a_typealias,
    kotlin_gradle_script_follows_a_project_head_change,
    kotlin_infix_function_appears_in_another_file,
    kotlin_number_bitwise_extension_appears_in_another_file,
    kotlin_when_guard_keeps_the_class_around_it,
    java_nested_type_follows_a_base_change_in_another_file,
    java_nested_type_ignores_an_added_namesake,
    java_import_package_appears_in_the_project,
    java_import_package_leaves_the_project,
    java_import_package_is_renamed_away,
    java_bare_call_follows_a_same_named_head_in_another_file,
    java_anonymous_class_follows_an_interface_head_change,
    java_head_window_text_moves_a_subclass_bare_call,
    java_batch_rewrites_a_base_and_a_middle_type,
    java_same_named_heads_trade_places_at_the_walk_limit,
    csharp_base_list_gains_an_interface,
    objc_change_a_superclass,
    python_change_a_base_class,
    php_change_a_parent_class,
    go_change_an_embedded_struct,
    dart_change_a_mixin,
    scala_change_a_parent_trait,
    scala_bare_call_follows_a_base_change_in_another_file,
    scala_imported_package_object_changes_its_supertype,
    scala_package_object_is_renamed_under_its_members,
    scala_chain_link_type_changes_its_package,
    scala_member_val_becomes_block_local,
    scala_function_becomes_an_extension_method,
    scala_type_reference_follows_a_namesake_head_in_another_file,
    scala_package_object_takes_an_imported_name,
    scala_chain_link_type_moves_into_the_named_package,
    scala_block_local_val_becomes_a_member,
    scala_base_body_edit_keeps_a_bare_call,
    scala_batch_rewrites_a_base_and_a_middle_type,
    scala_same_named_vals_trade_block_and_member_states,
    scala_holder_change_reaches_past_the_tail_ceiling,
    scala_package_object_supertype_head_changes_in_another_file,
    csharp_global_using_appears_for_its_project,
    csharp_global_using_leaves_its_project,
    #[ignore = "W5-06: a .csproj or .props change does not re-resolve the project's C# files"]
    csharp_csproj_using_appears,
    csharp_partial_part_gains_a_base,
    csharp_middle_type_drops_its_base,
    csharp_nested_owner_becomes_a_namespace,
    csharp_type_moves_into_an_imported_namespace,
    csharp_type_position_ignores_an_added_property,
    csharp_bare_call_follows_a_base_change_in_another_file,
    csharp_bare_call_follows_a_long_head_in_another_file,
    csharp_global_using_static_appears,
    csharp_global_using_static_leaves,
    #[ignore = "W5-06: a .csproj or .props change does not re-resolve the project's C# files"]
    csharp_csproj_static_using_appears,
    csharp_bare_name_ignores_an_added_property,
    csharp_inherited_member_type_changes_in_another_file,
    csharp_inherited_member_body_edit,
    java_inherited_field_type_changes_in_another_file,
    kotlin_inherited_property_type_changes_in_another_file,
    csharp_middle_type_argument_changes_in_another_file,
    java_type_parameter_bound_changes_in_another_file,
    csharp_accessor_body_call_of_an_added_class,
    csharp_field_initializer_body_edit,
    csharp_target_typed_new_of_an_added_class,
    csharp_type_position_ignores_an_added_constant,
    ruby_bare_call_follows_a_mixin_added_in_another_file,
    ruby_bare_call_follows_a_superclass_change_in_another_file,
    ruby_mixin_resolves_to_a_nearer_module_that_appears,
    ruby_bare_call_ignores_an_added_unrelated_method,
    ruby_untyped_receiver_ignores_a_lone_added_method,
    ruby_ancestor_method_body_edit,
    solidity_change_a_base_contract,
    solidity_bare_call_reaches_a_function_its_chain_comes_to_inherit,
    solidity_bare_call_leaves_a_function_its_chain_stops_inheriting,
    solidity_bare_call_follows_a_namesake_base_that_appears,
    cfml_change_a_base_component,
    cfml_dotted_extends_of_an_added_component,
    cfml_dotted_extends_takes_a_better_corroborated_component,
    cfml_relative_extends_of_an_added_component,
    cfml_typed_receiver_reaches_a_method_its_type_gains,
    cfml_typed_receiver_namesake_appears,
    cfml_typed_receiver_namesake_leaves,
    cfml_typed_receiver_namesake_leaves_past_the_tail_ceiling,
    cfml_typed_receiver_takes_a_better_corroborated_namesake,
    cfml_script_tag_method_body_edit,
    cfml_query_body_in_a_wrapped_method_edit,
    cfml_script_tag_body_nested_past_the_depth_limit_drops_the_file,
    cfml_query_body_nested_past_the_depth_limit_drops_the_file,
    cfml_implements_an_added_interface_tag,
    cfml_bare_call_follows_an_extends_change_in_another_file,
    cfml_bare_extends_takes_a_component_that_appears_beside_it,
    cfml_bare_call_ignores_an_added_built_in_namesake,
    cfml_untyped_receiver_ignores_a_lone_added_method,
    cfml_receiver_named_after_an_added_component,
    cfml_inherited_method_body_edit,
    python_add_a_same_named_competitor,
    arkts_add_a_same_named_competitor,
    kotlin_add_a_same_named_competitor,
    ruby_add_a_same_named_competitor,
    php_add_a_same_named_competitor,
    php_use_of_a_namespace_gains_a_same_named_import_elsewhere,
    tsx_catch_all_alias_ignores_a_same_named_project_component,
    typescript_config_toml_excludes_a_competitor,
    python_root_gitignore_hides_a_competitor,
    python_repository_exclude_hides_a_competitor,
    lua_extension_override_appears_in_codegraph_json,
    #[ignore = "unscheduled, found by W0-04: a tsconfig.json `paths` change does not re-resolve the unchanged importers"]
    typescript_tsconfig_paths_alias_appears,
    #[ignore = "unscheduled, found by W0-04: a package.json framework dependency change does not re-run framework detection"]
    typescript_react_dependency_appears_in_package_json,
    liquid_json_template_joins_a_theme_once_its_settings_schema_appears,
    liquid_json_templates_leave_once_the_theme_layout_goes,
    liquid_json_template_in_a_mixed_case_folder_joins_a_marked_theme,
    liquid_json_template_joins_a_theme_through_an_ignored_settings_schema,
    liquid_json_template_leaves_once_an_ignored_settings_schema_goes,
    liquid_render_stays_in_a_folder_once_it_is_marked_a_theme,
    liquid_render_leaves_a_folder_theme_once_its_marker_goes,
    liquid_json_template_loses_its_own_themes_section,
    liquid_json_template_gains_its_own_themes_section,
    c_header_flips_to_cpp,
    pascal_include_flips_from_php_and_its_routine_is_called,
    pascal_include_flips_to_php_and_its_routine_leaves,
    javascript_file_gains_the_flow_pragma,
}

/// An index built before the module-surface rows existed (W4a-F3) can hold
/// an edge an older sync left stale: such a sync re-extracted a barrel that
/// came to forward another module and never refreshed the barrel's importers.
/// The test fakes that index (the barrel's file row says its new text, its
/// importer's edge still goes the old way, no marker and no rows), then an
/// unrelated edit is synced: both drivers must reconcile every
/// JavaScript-family file, match a fresh index, and leave the rows and marker
/// a fresh index writes.
#[test]
fn surface_bootstrap_reconciles_an_index_built_before_the_rows() {
    const A: &str = "export function safeParse(value: unknown) {\n  return value;\n}\n";
    const B: &str = "export function safeParse(value: unknown) {\n  return [value];\n}\n";
    const USE: &str = "import { safeParse } from './index';\n\nexport function check() {\n  return safeParse(1);\n}\n";
    const TO_B: &str = "export * from './b';\n";
    const OTHER: &str = "export const other = 2;\n";
    let before = [
        ("src/a.ts", A),
        ("src/b.ts", B),
        ("src/index.ts", "export * from './a';\n"),
        ("src/use.ts", USE),
        ("src/other.ts", "export const other = 1;\n"),
    ];
    let index_stale = |label: &str| {
        let mut project = before
            .iter()
            .fold(
                support::Project::named(label),
                |project, (path, contents)| project.file(path, contents),
            )
            .index();
        project.write("src/index.ts", TO_B);
        let db = project.db();
        let disk = fs::metadata(project.root().join("src/index.ts")).unwrap();
        let modified = disk
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let conn = rusqlite::Connection::open(&db).unwrap();
        let updated = conn
            .execute(
                "UPDATE files SET content_hash = ?1, size = ?2, modified_at = ?3 WHERE path = 'src/index.ts'",
                rusqlite::params![
                    codegraph_core::node_id::hash_content(TO_B),
                    TO_B.len() as i64,
                    modified
                ],
            )
            .unwrap();
        assert_eq!(updated, 1);
        conn.execute(
            "DELETE FROM project_metadata WHERE substr(key, 1, 17) = 'js_module_surface'",
            [],
        )
        .unwrap();
        conn.close().unwrap();
        project.reload();
        assert!(
            !project
                .graph()
                .edges_matching("function check", "calls", "function safeParse @src/a.ts")
                .is_empty(),
            "the faked index keeps the stale edge\n{}",
            project.graph().report()
        );
        project.write("src/other.ts", OTHER);
        project
    };

    let mut cli = index_stale("cli");
    cli.sync();
    let mut watch = index_stale("watch");
    watcher_sync(&watch, &["src/other.ts"]);
    watch.reload();

    let fresh = [
        ("src/a.ts", A),
        ("src/b.ts", B),
        ("src/index.ts", TO_B),
        ("src/use.ts", USE),
        ("src/other.ts", OTHER),
    ]
    .iter()
    .fold(
        support::Project::named("fresh"),
        |project, (path, contents)| project.file(path, contents),
    )
    .index();
    let expected = canonicalize_db(&fresh.db()).expect("canonicalize the fresh index");
    for (driver, project) in [("cli", &cli), ("watch", &watch)] {
        let actual = canonicalize_db(&project.db()).expect("canonicalize the synced index");
        if let Err(diff) = diff_canonical(&expected, &actual, None) {
            panic!(
                "[{driver}] the reconciled index differs from a fresh one\n{}{diff}",
                readable_difference(fresh.graph(), project.graph()),
            );
        }
    }
    let surface_rows = support::project_metadata_with_prefix(&fresh.db(), "js_module_surface");
    assert!(
        surface_rows
            .iter()
            .any(|(key, _)| key == "js_module_surface_version"),
        "{surface_rows:?}"
    );
    for (driver, project) in [("cli", &cli), ("watch", &watch)] {
        assert_eq!(
            support::project_metadata_with_prefix(&project.db(), "js_module_surface"),
            surface_rows,
            "[{driver}] module-surface rows"
        );
    }
}

#[test]
fn sync_matrix_covers_every_extracted_language() {
    let mut running = BTreeMap::<&str, Vec<&str>>::new();
    for (name, gap, case) in registered() {
        if gap.is_none() {
            for language in case.languages {
                running.entry(language.as_str()).or_default().push(name);
            }
        }
    }
    let missing = extracted_languages()
        .into_iter()
        .map(Language::as_str)
        .filter(|language| !running.contains_key(language))
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "languages with an extractor but no running sync convergence case: {missing:?}\n\
         add a case to tests/sync_convergence/cases.rs; covered so far: {running:#?}"
    );
}

#[test]
fn sync_matrix_covers_every_change_kind() {
    let running = registered()
        .into_iter()
        .filter(|(_, gap, _)| gap.is_none())
        .map(|(_, _, case)| case.change)
        .collect::<BTreeSet<_>>();
    let missing = Change::ALL
        .into_iter()
        .filter(|change| !running.contains(change))
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "change kinds with no running sync convergence case: {missing:?}"
    );
}
