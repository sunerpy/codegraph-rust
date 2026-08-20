//! Tier 3 union-reachability regression tests (upstream PR #1516, `e922563`).
//!
//! A `union` node nothing points at would be worse than no union node at all, so
//! each test here drives the FULL extract → resolve pipeline over a committed
//! fixture under `tests/fixtures/tier3/` and asserts an edge REACHES the union —
//! and, where a wrong edge existed before, that the wrong edge is GONE. Both
//! directions, because "the right edge appeared" alone cannot notice that the
//! false one survived beside it.
//!
//! The three cases exercise three INDEPENDENT code paths, each with its own
//! same-shaped struct control:
//!   * `instantiate_agg`  — `resolver.rs`'s `Calls` → `Instantiates` promotion
//!   * `instantiate_rank` — `name_matcher.rs`'s `Instantiates` candidate bonus
//!   * `union_method`     — `name_matcher.rs`'s `matchMethodCall` candidate kinds
//!
//! They are separate fixtures on purpose: one file mixing the mechanisms would
//! make every isolated revert-proof ambiguous.
//!
//! Fixtures are read in place and never written to; the store lives in a temp
//! file, so the worktree stays clean.

use std::path::{Path, PathBuf};

use codegraph_core::types::{Edge, EdgeKind, FileRecord, Node, NodeKind};
use codegraph_extract::{detect_language, extract_file};
use codegraph_resolve::ReferenceResolver;
use codegraph_store::Store;

fn fixture_root(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("tier3")
        .join(name)
}

fn temp_db_path(test_name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    path.push(format!(
        "codegraph-tier3-{test_name}-{}-{nanos}.db",
        std::process::id()
    ));
    path
}

fn fixture_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root).expect("read fixture dir") {
        let path = entry.expect("dir entry").path();
        if let Ok(relative) = path.strip_prefix(root) {
            out.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    out.sort();
    out
}

struct Resolved {
    store: Store,
    edges: Vec<Edge>,
    refs: Vec<String>,
}

impl Resolved {
    fn node(&self, id: &str) -> Node {
        self.store
            .node_by_id(id)
            .expect("node lookup")
            .expect("node exists")
    }

    /// The single node of `kind` named `name`. Panics on 0 or >1, so a fixture
    /// that stops producing the node fails loudly instead of silently skipping.
    fn only(&self, kind: NodeKind, name: &str) -> Node {
        let mut found: Vec<Node> = self
            .store
            .nodes_by_name(name)
            .expect("nodes by name")
            .into_iter()
            .filter(|n| n.kind == kind)
            .collect();
        assert_eq!(
            found.len(),
            1,
            "want exactly one {kind:?} named {name}, got {found:?}"
        );
        found.pop().expect("checked len")
    }

    /// `(edge kind, "targetkind:targetname")` for every non-`Contains` edge out
    /// of `source_id`.
    fn out_of(&self, source_id: &str) -> Vec<(EdgeKind, String)> {
        let mut rows: Vec<(EdgeKind, String)> = self
            .edges
            .iter()
            .filter(|e| e.source == source_id)
            .map(|e| {
                let t = self.node(&e.target);
                (e.kind, format!("{:?}:{}", t.kind, t.name))
            })
            .collect();
        rows.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
        rows
    }
}

fn resolve_fixture(name: &str) -> Resolved {
    let root = fixture_root(name);
    assert!(root.is_dir(), "missing committed fixture {name}");
    let relative = fixture_files(&root);
    assert!(!relative.is_empty(), "fixture {name} has no files");

    let mut store = Store::open(&temp_db_path(name)).expect("open store");
    for path in &relative {
        let language = detect_language(path);
        let result = extract_file(&root, path).expect("extract file");
        store
            .upsert_file(&FileRecord {
                path: path.clone(),
                content_hash: "fixture".to_string(),
                language,
                size: 0,
                modified_at: 0,
                indexed_at: 0,
                node_count: result.nodes.len() as i64,
                errors: Vec::new(),
            })
            .expect("upsert file");
        store.upsert_nodes(&result.nodes).expect("upsert nodes");
        store
            .insert_edges(&result.edges)
            .expect("insert contains edges");
        store
            .insert_unresolved_refs(&result.unresolved_references)
            .expect("insert unresolved refs");
    }

    let root_str = root.to_string_lossy().to_string();
    let mut resolver = ReferenceResolver::new(root_str.clone());
    {
        let context = codegraph_resolve::StoreResolutionContext::new(&store, &root_str);
        resolver.initialize(&context);
    }
    resolver
        .resolve_and_persist(&mut store)
        .expect("resolve and persist");

    let mut edges = Vec::new();
    for kind in NodeKind::ALL {
        for node in store.nodes_by_kind(kind).expect("nodes by kind") {
            for edge in store
                .edges_by_source_kind(&node.id, None)
                .expect("edges by source")
            {
                if edge.kind != EdgeKind::Contains {
                    edges.push(edge);
                }
            }
        }
    }
    let refs = store
        .all_unresolved_refs()
        .expect("all unresolved refs")
        .into_iter()
        .map(|r| format!("{:?}|{}", r.reference_kind, r.reference_name))
        .collect();
    Resolved { store, edges, refs }
}

#[test]
fn cpp_union_call_construction_is_promoted_to_instantiates() {
    // Given `Reg r = Reg();` where `Reg` is a union and the ONLY node of that
    // name, and `Ctl c = Ctl();` as the struct control,
    // When the pipeline resolves,
    // Then the union's ref — extracted as `Calls`, measured PRE-FIX as a DANGLING
    // unresolved ref because no union node existed to bind — both resolves and is
    // promoted to `Instantiates`, leaving NO `Calls` edge behind.
    let g = resolve_fixture("instantiate_agg");
    let mk_union = g.only(NodeKind::Function, "mk_union");
    let out = g.out_of(&mk_union.id);

    assert!(
        out.contains(&(EdgeKind::Instantiates, "Union:Reg".to_string())),
        "the union construction must be promoted to instantiates: {out:?}"
    );
    assert!(
        !out.iter().any(|(kind, _)| *kind == EdgeKind::Calls),
        "a Calls edge must NOT survive the promotion: {out:?}"
    );
    assert!(
        !g.refs.iter().any(|r| r == "Calls|Reg"),
        "the `Reg` ref must no longer be dangling: {:?}",
        g.refs
    );

    let mk_struct = g.only(NodeKind::Function, "mk_struct");
    assert!(
        g.out_of(&mk_struct.id)
            .contains(&(EdgeKind::Instantiates, "Struct:Ctl".to_string())),
        "the struct control regressed: {:?}",
        g.out_of(&mk_struct.id)
    );
}

#[test]
fn cpp_union_wins_instantiates_ranking_over_a_same_named_function() {
    // Given `Value v{1};` — a brace construction, so the ref is extracted as
    // `Instantiates` — competing against a SAME-NAMED `void Value()`,
    // When the pipeline ranks the candidates,
    // Then the `+25` aggregate bonus makes the union win. Measured PRE-FIX this
    // bound `function:Value`, the WRONG target, so the assertion is stated in both
    // directions: the union is hit and the function is not touched at all.
    let g = resolve_fixture("instantiate_rank");
    let ctor_union = g.only(NodeKind::Function, "ctor_union");
    let out = g.out_of(&ctor_union.id);

    assert!(
        out.contains(&(EdgeKind::Instantiates, "Union:Value".to_string())),
        "the union must win the Instantiates ranking: {out:?}"
    );
    assert!(
        !out.iter().any(|(_, target)| target == "Function:Value"),
        "no edge may reach the same-named function: {out:?}"
    );

    let ctor_struct = g.only(NodeKind::Function, "ctor_struct");
    assert!(
        g.out_of(&ctor_struct.id)
            .contains(&(EdgeKind::Instantiates, "Struct:Packet".to_string())),
        "the struct control regressed: {:?}",
        g.out_of(&ctor_struct.id)
    );
}

#[test]
fn cpp_union_method_call_binds_the_union_not_the_struct() {
    // Given a union and a struct in one file that each define `read_field`, and
    // one INSTANCE call on each receiver,
    // When the pipeline resolves the method calls,
    // Then each binds its OWN receiver's member. Measured PRE-FIX: the union had
    // no node, so its member was a free function and `w.read_field()` bound
    // `SWithMethod::read_field` — a false edge to the wrong type, not merely a
    // missing one. Receiver type has to decide, since the names are identical.
    //
    // The load-bearing fix here is the EXTRACTION-side `is_inside_class_like_node`
    // widening, which makes the member a `method` owned by the union; reverting it
    // alone reproduces the false edge. Reverting either `match_method_call`
    // candidate filter does NOT: receiver-type inference resolves this shape
    // before that strategy runs. Three other shapes were tried and each is
    // claimed by an earlier path too — a Rust `Bits::raw(b)` by exact
    // qualified-name matching, and a C++ `Cfg::probe()` never even reaches the
    // resolver as a qualified ref because the extractor emits the bare `probe`.
    // So `match_method_call`'s union widening is a symmetric port hunk with no
    // reachable fixture in this tree; it is compile-verified, not behaviour-proven.
    let g = resolve_fixture("union_method");
    for (caller, want_owner) in [
        ("drive_union", "WithMethod::read_field"),
        ("drive_struct", "SWithMethod::read_field"),
    ] {
        let from = g.only(NodeKind::Function, caller);
        let bound: Vec<String> = g
            .edges
            .iter()
            .filter(|e| e.source == from.id && e.kind == EdgeKind::Calls)
            .map(|e| g.node(&e.target).qualified_name)
            .collect();
        assert_eq!(
            bound,
            vec![want_owner.to_string()],
            "{caller} must bind exactly {want_owner}"
        );
    }
}
