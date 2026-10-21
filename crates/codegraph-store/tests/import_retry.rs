//! The keys a sync retries parked imports by (upstream #2392,
//! `sync-import-retry.test.ts`): what an added file can be imported as, the
//! tail a parked reference waits under, and the lookup that joins them.

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind, UnresolvedRef};
use codegraph_store::{Store, import_path_keys, reference_name_tail};

#[test]
fn import_path_keys_name_a_file_and_its_folder_whole_and_up_to_the_first_dot() {
    assert_eq!(import_path_keys("lib/src/d.dart"), ["src", "d.dart", "d"]);
    assert_eq!(
        import_path_keys("src/types.d.ts"),
        ["src", "types.d.ts", "types"]
    );
    assert_eq!(
        import_path_keys("pkg/sub/__init__.py"),
        ["sub", "__init__.py", "__init__"]
    );
    assert_eq!(
        import_path_keys("vendor/gopkg.in/yaml.v3/yaml.go"),
        ["yaml.v3", "yaml", "yaml.go"]
    );
    assert_eq!(import_path_keys("main.c"), ["main.c", "main"]);
    assert_eq!(
        import_path_keys(".github/.eslintrc.js"),
        [".github", ".eslintrc.js", ".eslintrc"]
    );
}

#[test]
fn a_path_import_waits_under_its_last_segments_stem() {
    let imports = |name: &str| reference_name_tail(name, EdgeKind::Imports);
    assert_eq!(imports("package:app/b.dart"), "b");
    assert_eq!(imports("./foo"), "foo");
    assert_eq!(imports("inc/db.php"), "db");
    assert_eq!(imports("./z.js"), "z");
    // `..` names a folder only through the importing file: the dotted rule.
    assert_eq!(imports("../.."), "");
    // A dotted module path and a symbol keep the dotted tail.
    assert_eq!(imports("pkg.mod"), "mod");
    assert_eq!(reference_name_tail("util.greet", EdgeKind::Calls), "greet");
    assert_eq!(reference_name_tail("mod::fn/2", EdgeKind::Calls), "fn");
    assert_eq!(reference_name_tail("greet", EdgeKind::Calls), "greet");
}

/// Upstream #2403's `referenceNameTail` cases, in this port's keys: a
/// reference written as a path waits under its file's stem, which the import
/// path keys of the file it names carry (upstream parks it under the file
/// name and rewrote the stored tails; this port stores none). A call's `/`
/// is in its arguments or a division, never a path.
#[test]
fn a_path_reference_waits_under_a_key_of_the_file_it_names() {
    let references = |name: &str| reference_name_tail(name, EdgeKind::References);
    for (name, file) in [
        ("snippets/price.liquid", "snippets/price.liquid"),
        ("sections/404.liquid", "sections/404.liquid"),
        ("snippets/icon.logo.liquid", "snippets/icon.logo.liquid"),
    ] {
        let tail = references(name);
        assert!(
            import_path_keys(file).contains(&tail),
            "{name} waits under {tail:?}, a key of {file}: {:?}",
            import_path_keys(file)
        );
    }
    assert_eq!(references("snippets/price.liquid"), "price");
    assert_eq!(references("snippets/icon.logo.liquid"), "icon");
    assert_eq!(references("Foo.Bar"), "Bar");
    assert_eq!(
        reference_name_tail(
            "assert.logfile(\"logs/error.log\").has.line",
            EdgeKind::Calls
        ),
        "line"
    );
    assert_eq!(
        reference_name_tail("(rss_kb / 1024.0).to_i", EdgeKind::Calls),
        "to_i"
    );
    assert_eq!(reference_name_tail("lists::map/2", EdgeKind::Calls), "map");
    assert_eq!(
        reference_name_tail("package:app/b.dart", EdgeKind::Imports),
        "b"
    );
}

fn store(label: &str) -> Store {
    let path = std::env::temp_dir().join(format!(
        "codegraph-store-import-retry-{label}-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut store = Store::open(&path).expect("open store");
    let files = [
        "a.dart", "c.dart", "d.dart", "e.dart", "f.dart", "g.cs", "h.ts",
    ];
    let file_nodes: Vec<Node> = files
        .iter()
        .map(|file| Node {
            id: format!("file:{file}"),
            kind: NodeKind::File,
            name: file.to_string(),
            qualified_name: file.to_string(),
            file_path: file.to_string(),
            language: Language::Dart,
            start_line: 1,
            end_line: 1,
            start_column: 0,
            end_column: 0,
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
        })
        .collect();
    store.upsert_nodes(&file_nodes).expect("file nodes");
    let parked = |file: &str, name: &str, kind: EdgeKind| UnresolvedRef {
        id: None,
        from_node_id: format!("file:{file}"),
        reference_name: name.to_string(),
        reference_kind: kind,
        line: 1,
        col: 0,
        candidates: None,
        file_path: file.to_string(),
        language: Language::Dart,
        is_function_ref: false,
        reference_subkind: None,
    };
    store
        .insert_unresolved_refs(&[
            parked("a.dart", "package:app/b.dart", EdgeKind::Imports),
            parked("c.dart", "../b.dart", EdgeKind::Imports),
            parked("d.dart", "src/b.dart", EdgeKind::Imports),
            parked("e.dart", "b.dart", EdgeKind::Imports),
            parked("f.dart", "x.b", EdgeKind::Calls),
            parked("g.cs", "Foo.Bar", EdgeKind::Imports),
            parked("h.ts", "useState", EdgeKind::Imports),
        ])
        .expect("park refs");
    store
}

fn names(refs: Vec<UnresolvedRef>) -> Vec<String> {
    let mut names: Vec<String> = refs.into_iter().map(|r| r.reference_name).collect();
    names.sort();
    names
}

fn keys(keys: &[&str]) -> Vec<String> {
    keys.iter().map(|key| key.to_string()).collect()
}

#[test]
fn an_import_is_found_by_the_stem_of_its_path_or_by_its_whole_name_and_nothing_else() {
    let store = store("stem");
    assert_eq!(
        names(
            store
                .retryable_failed_imports(&keys(&["b"]), &[], 500)
                .unwrap()
        ),
        ["../b.dart", "package:app/b.dart", "src/b.dart"]
    );
    assert_eq!(
        names(
            store
                .retryable_failed_imports(&keys(&["b.dart"]), &[], 500)
                .unwrap()
        ),
        ["b.dart"]
    );
    assert_eq!(
        store
            .retryable_failed_imports(&keys(&["b", "b.dart", "b"]), &[], 500)
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn an_import_is_found_by_a_namespace_name_only_when_that_name_is_not_its_tail() {
    let store = store("names");
    assert_eq!(
        names(
            store
                .retryable_failed_imports(&[], &keys(&["Foo.Bar", "useState", "b"]), 500)
                .unwrap()
        ),
        ["Foo.Bar"]
    );
    assert_eq!(
        names(
            store
                .retryable_failed_imports(&keys(&["useState"]), &[], 500)
                .unwrap()
        ),
        ["useState"]
    );
}

#[test]
fn a_key_more_parked_imports_share_than_the_ceiling_is_skipped() {
    let store = store("ceiling");
    assert!(
        store
            .retryable_failed_imports(&keys(&["b"]), &[], 2)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        names(
            store
                .retryable_failed_imports(&keys(&["b", "b.dart"]), &[], 2)
                .unwrap()
        ),
        ["b.dart"]
    );
    assert_eq!(
        store
            .retryable_failed_imports(&keys(&["b"]), &[], 3)
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn a_qualified_reference_is_found_by_its_tail_never_by_its_whole_name() {
    let store = store("tails");
    // `x.b` (a call) and the path imports wait under the tail `b`, as
    // upstream's `name_tail` does; `b.dart` waits under `dart`, the bare
    // `useState` under the whole-name lane only.
    assert_eq!(
        names(
            store
                .unresolved_refs_by_name_tails(&keys(&["b"]), 500)
                .unwrap()
        ),
        ["../b.dart", "package:app/b.dart", "src/b.dart", "x.b"]
    );
    assert_eq!(
        names(
            store
                .unresolved_refs_by_name_tails(&keys(&["Bar"]), 500)
                .unwrap()
        ),
        ["Foo.Bar"]
    );
    assert!(
        store
            .unresolved_refs_by_name_tails(&keys(&["useState", "x.b"]), 500)
            .unwrap()
            .is_empty()
    );
    // A tail more references share than the ceiling wakes none of them.
    assert!(
        store
            .unresolved_refs_by_name_tails(&keys(&["b"]), 3)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        names(
            store
                .unresolved_refs_by_name_tails(&keys(&["b", "Bar"]), 3)
                .unwrap()
        ),
        ["Foo.Bar"]
    );
}
