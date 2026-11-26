//! The navigation calls a sync re-resolves when a route a call may name
//! appears, goes or moves (W7-23b; upstream #2425's `getFailedCallsByTail`
//! and `getResolvedNavigations`, `sync-navigation-retry.test.ts`).

use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind, UnresolvedRef};
use codegraph_store::Store;
use codegraph_store::queries::{FileReferenceSite, LaneReference, NavigationSite, ReferenceSite};

fn store(label: &str) -> Store {
    let path = std::env::temp_dir().join(format!(
        "codegraph-store-navigation-calls-{label}-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    Store::open(&path).expect("open store")
}

fn node(id: &str, kind: NodeKind, name: &str, file: &str) -> Node {
    Node {
        id: id.to_string(),
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        file_path: file.to_string(),
        language: Language::Tsx,
        start_line: 1,
        end_line: 20,
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
    }
}

fn reference(from: &str, name: &str, kind: EdgeKind, line: i64, file: &str) -> UnresolvedRef {
    UnresolvedRef {
        id: None,
        from_node_id: from.to_string(),
        reference_name: name.to_string(),
        reference_kind: kind,
        line,
        col: 2,
        candidates: None,
        file_path: file.to_string(),
        language: Language::Tsx,
        is_function_ref: false,
        reference_subkind: None,
    }
}

fn navigates(from: &str, to: &str, line: i64, provenance: Option<&str>) -> Edge {
    Edge {
        id: None,
        source: from.to_string(),
        target: to.to_string(),
        kind: EdgeKind::Navigates,
        metadata: None,
        line: Some(line),
        col: Some(2),
        provenance: provenance.map(str::to_string),
    }
}

/// Move the rows of `unresolved_refs` on `lines` into `resolved_refs`, as a
/// resolution pass moves the ones it resolves.
fn resolve_lines(store: &mut Store, lines: &[i64]) {
    let ids: Vec<i64> = store
        .all_unresolved_refs()
        .unwrap()
        .into_iter()
        .filter(|reference| lines.contains(&reference.line))
        .filter_map(|reference| reference.id)
        .collect();
    store.delete_resolved_unresolved_refs(&ids).unwrap();
}

fn sites(found: &[LaneReference]) -> Vec<(String, i64, String)> {
    found
        .iter()
        .map(|reference| {
            assert!(!reference.resolved, "{reference:?}");
            assert_eq!(reference.reference_kind, EdgeKind::Calls, "{reference:?}");
            let site = reference.site.site.as_ref().expect("a site");
            (
                reference.site.file_path.clone(),
                site.line,
                reference.reference_name.clone(),
            )
        })
        .collect()
}

/// Two files of calls: some name a router's method (`navigate`, the tail of
/// `history.push` and `this.router.navigate`), some do not, one is not a
/// call, and one already resolved.
fn calls_store(label: &str) -> Store {
    let mut store = store(label);
    store
        .upsert_nodes(&[
            node("function:a", NodeKind::Function, "a", "src/a.tsx"),
            node("function:b", NodeKind::Function, "b", "src/b.tsx"),
        ])
        .unwrap();
    store
        .insert_unresolved_refs(&[
            reference("function:a", "navigate", EdgeKind::Calls, 2, "src/a.tsx"),
            reference(
                "function:a",
                "history.push",
                EdgeKind::Calls,
                3,
                "src/a.tsx",
            ),
            reference("function:a", "router.back", EdgeKind::Calls, 4, "src/a.tsx"),
            reference(
                "function:a",
                "navigate",
                EdgeKind::References,
                5,
                "src/a.tsx",
            ),
            reference("function:a", "pushState", EdgeKind::Calls, 6, "src/a.tsx"),
            reference(
                "function:b",
                "this.router.navigate",
                EdgeKind::Calls,
                2,
                "src/b.tsx",
            ),
            reference("function:b", "push", EdgeKind::Calls, 3, "src/b.tsx"),
            reference("function:b", "navigate", EdgeKind::Calls, 4, "src/b.tsx"),
        ])
        .unwrap();
    // The last call resolved: it is no parked call any more.
    let resolved: Vec<i64> = store
        .all_unresolved_refs()
        .unwrap()
        .into_iter()
        .filter(|reference| reference.file_path == "src/b.tsx" && reference.line == 4)
        .filter_map(|reference| reference.id)
        .collect();
    store.delete_resolved_unresolved_refs(&resolved).unwrap();
    store
}

/// Upstream `getFailedCallsByTail`: the parked `calls` references whose name
/// ends in one of the tails, written whole (`navigate`) or through a receiver
/// (`history.push`, `this.router.navigate`), sorted by site.
#[test]
fn a_parked_call_is_found_by_the_tail_of_its_name() {
    let store = calls_store("by-tail");
    let tails = ["navigate".to_string(), "push".to_string()];
    assert_eq!(
        sites(&store.unresolved_calls_by_tails(&tails, None).unwrap()),
        [
            ("src/a.tsx".to_string(), 2, "navigate".to_string()),
            ("src/a.tsx".to_string(), 3, "history.push".to_string()),
            (
                "src/b.tsx".to_string(),
                2,
                "this.router.navigate".to_string()
            ),
            ("src/b.tsx".to_string(), 3, "push".to_string()),
        ]
    );
    assert!(
        store
            .unresolved_calls_by_tails(&[], None)
            .unwrap()
            .is_empty()
    );
}

/// The calls of the files a sync re-reads for the destinations they read
/// elsewhere (W7-23b's N2): the same lookup, narrowed to those files.
#[test]
fn a_files_parked_calls_are_found_by_tail_in_that_file_alone() {
    let store = calls_store("by-tail-in-files");
    let tails = ["navigate".to_string(), "push".to_string()];
    assert_eq!(
        sites(
            &store
                .unresolved_calls_by_tails(&tails, Some(&["src/b.tsx".to_string()]))
                .unwrap()
        ),
        [
            (
                "src/b.tsx".to_string(),
                2,
                "this.router.navigate".to_string()
            ),
            ("src/b.tsx".to_string(), 3, "push".to_string()),
        ]
    );
    assert!(
        store
            .unresolved_calls_by_tails(&tails, Some(&[]))
            .unwrap()
            .is_empty()
    );
}

/// No ceiling (OD-B42): upstream skips a name more than 500 calls share;
/// every one of 501 is read here.
#[test]
fn every_parked_call_of_a_tail_is_read_however_many_share_it() {
    let mut store = store("by-tail-many");
    store
        .upsert_nodes(&[node("function:a", NodeKind::Function, "a", "src/a.tsx")])
        .unwrap();
    let calls: Vec<UnresolvedRef> = (1..=501)
        .map(|line| reference("function:a", "navigate", EdgeKind::Calls, line, "src/a.tsx"))
        .collect();
    store.insert_unresolved_refs(&calls).unwrap();
    let found = store
        .unresolved_calls_by_tails(&["navigate".to_string()], None)
        .unwrap();
    assert_eq!(found.len(), 501);
}

fn site(file: &str, from: &str, line: i64) -> FileReferenceSite {
    FileReferenceSite {
        file_path: file.to_string(),
        site: Some(ReferenceSite {
            from_node_id: from.to_string(),
            line,
            col: 2,
        }),
    }
}

/// Upstream `getResolvedNavigations`: the `navigates` edges a router's
/// resolver drew, each at its site with the name of the reference resolved
/// there. A synthesizer's (`heuristic`) is no router's answer, and another
/// kind of edge is no navigation; a conditional's two arms are one site.
#[test]
fn a_navigation_site_is_a_routers_edge_with_the_reference_resolved_there() {
    let mut store = store("navigation-sites");
    store
        .upsert_nodes(&[
            node("function:a", NodeKind::Function, "a", "src/a.tsx"),
            node("function:b", NodeKind::Function, "b", "src/b.tsx"),
            node("route:/x", NodeKind::Route, "/x", "src/routes.tsx"),
            node("route:/y", NodeKind::Route, "/y", "src/routes.tsx"),
        ])
        .unwrap();
    store
        .insert_unresolved_refs(&[
            reference(
                "function:a",
                "history.push",
                EdgeKind::Calls,
                2,
                "src/a.tsx",
            ),
            reference("function:a", "navigate", EdgeKind::Calls, 3, "src/a.tsx"),
            reference("function:b", "navigate", EdgeKind::Calls, 2, "src/b.tsx"),
        ])
        .unwrap();
    resolve_lines(&mut store, &[2, 3]);
    store
        .insert_edges(&[
            // `history.push(user ? '/x' : '/y')`: two arms, one site.
            navigates("function:a", "route:/x", 2, None),
            navigates("function:a", "route:/y", 2, None),
            // A link a synthesizer drew from markup.
            navigates("function:a", "route:/x", 9, Some("heuristic")),
            navigates("function:b", "route:/y", 2, None),
            Edge {
                kind: EdgeKind::Calls,
                ..navigates("function:a", "route:/x", 3, None)
            },
        ])
        .unwrap();
    let named = |file: &str, from: &str, line: i64, name: &str| NavigationSite {
        site: site(file, from, line),
        reference_name: Some(name.to_string()),
    };
    assert_eq!(
        store.navigation_sites(None).unwrap(),
        [
            named("src/a.tsx", "function:a", 2, "history.push"),
            named("src/b.tsx", "function:b", 2, "navigate"),
        ]
    );
    assert_eq!(
        store
            .navigation_sites(Some(&["src/b.tsx".to_string()]))
            .unwrap(),
        [named("src/b.tsx", "function:b", 2, "navigate")]
    );
    assert!(store.navigation_sites(Some(&[])).unwrap().is_empty());
}

/// A `navigates` edge whose site holds no row of `resolved_refs` (an index
/// whose rows predate the table) is still a navigation site, its name
/// unknown: the caller reads it as any name rather than skip the call.
#[test]
fn a_navigation_site_without_its_resolved_row_has_no_name() {
    let mut store = store("navigation-sites-unnamed");
    store
        .upsert_nodes(&[
            node("function:a", NodeKind::Function, "a", "src/a.tsx"),
            node("route:/x", NodeKind::Route, "/x", "src/routes.tsx"),
        ])
        .unwrap();
    store
        .insert_edges(&[navigates("function:a", "route:/x", 2, None)])
        .unwrap();
    assert_eq!(
        store.navigation_sites(None).unwrap(),
        [NavigationSite {
            site: site("src/a.tsx", "function:a", 2),
            reference_name: None,
        }]
    );
}
