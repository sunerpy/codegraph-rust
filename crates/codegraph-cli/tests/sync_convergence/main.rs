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
    /// root `.gitignore`, `.git/info/exclude`, or the index root's
    /// `config.toml` or `codegraph.json`.
    ControlFile,
    /// A `.h` header changes from C to C++ content.
    HeaderFlip,
}

impl Change {
    const ALL: [Self; 8] = [
        Self::AddImportTarget,
        Self::BodyEdit,
        Self::RenameSymbol,
        Self::DeleteImportedModule,
        Self::EditHeritage,
        Self::AddCompetitor,
        Self::ControlFile,
        Self::HeaderFlip,
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
    #[ignore = "W5-03 #2450: a reference through an aliased import binding does not link a module that appears later"]
    typescript_aliased_named_import_of_an_added_file,
    tsx_component_import_of_an_added_file,
    #[ignore = "W5-01 #2392: a failed `from pkg.mod import` is not retried when pkg/mod.py appears"]
    python_from_import_of_an_added_module,
    go_call_into_an_added_file_of_the_same_package,
    c_include_of_an_added_header,
    #[ignore = "W5-02 #2403: a Liquid `render` does not link a snippet that appears later"]
    liquid_render_of_an_added_snippet,
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
    vue_component_template_edit,
    liquid_snippet_body_edit,
    luau_module_function_body_edit,
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
    mybatis_rename_an_included_fragment,
    typescript_delete_an_imported_module,
    rust_delete_a_used_module,
    dart_delete_an_imported_library,
    astro_delete_an_imported_module,
    lua_delete_a_required_module,
    nix_delete_an_imported_file,
    python_move_an_imported_module_away,
    typescript_change_a_superclass,
    java_change_a_superclass_and_interface,
    scala_change_a_parent_trait,
    solidity_change_a_base_contract,
    cfml_change_a_base_component,
    python_add_a_same_named_competitor,
    arkts_add_a_same_named_competitor,
    kotlin_add_a_same_named_competitor,
    ruby_add_a_same_named_competitor,
    php_add_a_same_named_competitor,
    php_use_of_a_namespace_gains_a_same_named_import_elsewhere,
    typescript_config_toml_excludes_a_competitor,
    python_root_gitignore_hides_a_competitor,
    python_repository_exclude_hides_a_competitor,
    lua_extension_override_appears_in_codegraph_json,
    #[ignore = "unscheduled, found by W0-04: a tsconfig.json `paths` change does not re-resolve the unchanged importers"]
    typescript_tsconfig_paths_alias_appears,
    #[ignore = "unscheduled, found by W0-04: a package.json framework dependency change does not re-run framework detection"]
    typescript_react_dependency_appears_in_package_json,
    c_header_flips_to_cpp,
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
