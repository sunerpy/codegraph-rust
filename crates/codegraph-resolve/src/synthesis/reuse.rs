//! Whether a sync keeps a synthesis step's recorded output (W9-00 row 27,
//! OD-B90). A step keeps it only when its record says it may be reused and
//! nothing it read moved:
//!
//! - no key the sync's changed rows name, by their old image or their new
//!   one, is among its recorded keys, and no changed node matches one of its
//!   pattern lookups, evaluated with SQLite's own operators;
//! - no changed, added or removed file is one its gated scans passed, and
//!   none that is in a scan's scope passes the scan's check now;
//! - each of its recorded lines in a changed file reads the same now;
//! - each file it read beside the index reads as it did;
//! - a prerequisite step that ran again and emitted other edges moves the
//!   keys of those edges for every later step.
//!
//! Anything this cannot tell, a record that does not read, a gate it does
//! not know, a file that cannot be read now, reruns the step.

use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use codegraph_core::node_id::hash_content;
use codegraph_core::types::Edge;
use codegraph_extract::ExtractOptions;
use codegraph_extract::engine::ScanScope;
use codegraph_store::{ChangeSet, NodeImage, Store};

use super::footprint::{GateInput, PassRecordHead, Predicate, encoded_holds, keys};
use super::inputs;
use super::stage::{Read, View, gate_named, read_lines};

/// What a sync's changes move, as a step's record reads it: the keys of
/// every changed row, the changed nodes the pattern lookups are asked of,
/// and the files whose row or text moved.
pub(super) struct Probe {
    keys: HashSet<u64>,
    nodes: Vec<NodeImage>,
    files: BTreeSet<String>,
}

impl Probe {
    pub(super) fn new(changes: &ChangeSet) -> Self {
        let mut probe = Self {
            keys: HashSet::new(),
            nodes: Vec::new(),
            files: BTreeSet::new(),
        };
        for node in changes.nodes.iter() {
            probe.keys.extend([
                keys::node(&node.id),
                keys::file_nodes(&node.file_path),
                keys::file_kind(&node.file_path, &node.kind),
                keys::name(&node.name),
                keys::qualified(&node.qualified_name),
                keys::lower_name(&node.name.to_ascii_lowercase()),
                keys::kind(&node.kind),
                keys::kind_in(&node.kind, &node.language),
            ]);
            probe.nodes.push(node.clone());
        }
        let mut names: HashMap<&str, i64> = HashMap::new();
        for node in &changes.nodes.removed {
            *names.entry(node.name.as_str()).or_default() -= 1;
        }
        for node in &changes.nodes.added {
            *names.entry(node.name.as_str()).or_default() += 1;
        }
        if names.values().any(|&net| net != 0) {
            probe.keys.insert(keys::names());
        }
        let removed_languages: HashMap<&str, &str> = changes
            .nodes
            .removed
            .iter()
            .map(|node| (node.id.as_str(), node.language.as_str()))
            .collect();
        if changes.nodes.added.iter().any(|node| {
            removed_languages
                .get(node.id.as_str())
                .is_some_and(|language| *language != node.language)
        }) {
            probe.keys.insert(keys::language_move());
        }
        for edge in changes.edges.iter() {
            probe.edge(&edge.source, &edge.target, &edge.kind);
        }
        for reference in changes.unresolved_refs.iter() {
            probe.keys.extend([
                keys::refs_of_kind(&reference.reference_kind),
                keys::site(&reference.from_node_id, reference.line, reference.col),
            ]);
        }
        for reference in changes.resolved_refs.iter() {
            probe.keys.insert(keys::site(
                &reference.from_node_id,
                reference.line,
                reference.col,
            ));
        }
        for file in changes.files.iter() {
            probe.keys.extend([
                keys::file_row(&file.path),
                keys::files_of(&file.language),
                keys::text(&file.path),
            ]);
            probe.files.insert(file.path.clone());
        }
        let removed: BTreeSet<&str> = changes
            .files
            .removed
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        let added: BTreeSet<&str> = changes
            .files
            .added
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        if removed != added {
            probe.keys.insert(keys::files());
        }
        for path in &changes.texts {
            probe.keys.insert(keys::text(path));
            probe.files.insert(path.clone());
        }
        probe
    }

    fn edge(&mut self, source: &str, target: &str, kind: &str) {
        self.keys.extend([
            keys::edges_into(target, kind),
            keys::edges_from(source, kind),
            keys::edges_of_kind(kind),
        ]);
    }

    /// A prerequisite step ran again: the edges it emits now and did not
    /// before, and the other way round, move as base edges do for the
    /// steps after it.
    pub(super) fn moved_overlay(&mut self, before: &[Edge], now: &[Edge]) {
        let identity = |edge: &Edge| {
            (
                edge.source.clone(),
                edge.target.clone(),
                edge.kind.as_str(),
                edge.line,
                edge.col,
            )
        };
        let mut net: HashMap<_, i64> = HashMap::new();
        for edge in before {
            *net.entry(identity(edge)).or_default() -= 1;
        }
        for edge in now {
            *net.entry(identity(edge)).or_default() += 1;
        }
        for ((source, target, kind, _, _), count) in net {
            if count != 0 {
                self.edge(&source, &target, kind);
            }
        }
    }
}

/// What a step's record is checked against now, beside the view of the
/// synced index: the store's operators for the pattern lookups, and the
/// files beside the index, each read once a sync (the scan scope built only
/// when a record names one).
pub(super) struct Now<'a> {
    store: &'a Store,
    root: PathBuf,
    options: &'a ExtractOptions,
    scope: OnceCell<ScanScope>,
    unindexed: RefCell<HashMap<String, Option<String>>>,
}

impl<'a> Now<'a> {
    pub(super) fn new(store: &'a Store, root: &Path, options: &'a ExtractOptions) -> Self {
        Self {
            store,
            root: root.to_path_buf(),
            options,
            scope: OnceCell::new(),
            unindexed: RefCell::new(HashMap::new()),
        }
    }

    fn unindexed_hash(&self, path: &str) -> Option<String> {
        if let Some(hash) = self.unindexed.borrow().get(path) {
            return hash.clone();
        }
        let scope = self
            .scope
            .get_or_init(|| ScanScope::new(&self.root, self.options));
        let hash = inputs::read(&self.root, scope, self.options.max_file_size, path)
            .as_deref()
            .map(hash_content);
        self.unindexed
            .borrow_mut()
            .insert(path.to_string(), hash.clone());
        hash
    }

    /// Whether `node` is one of the nodes `predicate` looks up.
    fn matches(&self, predicate: &Predicate, node: &NodeImage) -> Result<bool> {
        Ok(match predicate {
            Predicate::Callables {
                globs,
                likes,
                names,
            } => {
                if node.kind != "function" && node.kind != "method" {
                    false
                } else if names.contains(&node.name) {
                    true
                } else {
                    let mut found = false;
                    for glob in globs {
                        found |= self.store.sql_glob(&node.name, glob)?;
                    }
                    for like in likes {
                        found |= self.store.sql_like(&node.name, like)?;
                    }
                    found
                }
            }
            Predicate::Signature { node_kind, text } => {
                node.kind == *node_kind
                    && node
                        .signature
                        .as_deref()
                        .is_some_and(|signature| signature.contains(text.as_str()))
            }
            Predicate::Decorator { language, pattern } => {
                node.language == *language
                    && match node.decorators.as_deref() {
                        Some(decorators) => self.store.sql_like(decorators, pattern)?,
                        None => false,
                    }
            }
        })
    }
}

/// Why a step runs again, or `None` when it keeps its recorded output. The
/// record is of this version: one of another runs the stage whole
/// (`stage::record_head`).
pub(super) fn rerun_reason(
    record: &PassRecordHead,
    probe: &Probe,
    now: &Now<'_>,
    view: &View<'_>,
) -> Result<Option<String>> {
    if !record.reusable {
        return Ok(Some(format!("not reusable {:?}", record.untracked)));
    }
    if let Some(key) = probe
        .keys
        .iter()
        .find(|key| encoded_holds(&record.keys, **key))
    {
        return Ok(Some(format!("key {key:016x}")));
    }
    for predicate in &record.predicates {
        for node in &probe.nodes {
            if now.matches(predicate, node)? {
                return Ok(Some(format!("{predicate:?} matches {}", node.id)));
            }
        }
    }
    for gate_use in &record.gates {
        let Some(gate) = gate_named(&gate_use.gate) else {
            return Ok(Some(format!("unknown gate {}", gate_use.gate)));
        };
        for file in &probe.files {
            if gate_use.contributors.binary_search(file).is_ok() {
                return Ok(Some(format!("{} contributor {file}", gate.name)));
            }
            let Some(stored) = view.file_record(file) else {
                continue;
            };
            if !(gate.scope)(file, stored.language) {
                continue;
            }
            match view.read(file) {
                Read::Text(text) => {
                    let input = GateInput {
                        text: &text,
                        flags: gate_use.flags,
                    };
                    if (gate.check)(&input) {
                        return Ok(Some(format!("{} passes {file}", gate.name)));
                    }
                }
                Read::Absent => {}
                Read::Drifted => return Ok(Some(format!("{} cannot read {file}", gate.name))),
            }
        }
    }
    for slice in &record.slices {
        if !probe.files.contains(&slice.path) {
            continue;
        }
        let same = match view.read(&slice.path) {
            Read::Text(text) => {
                read_lines(&text, slice.start, slice.end, slice.line)
                    .as_deref()
                    .map(hash_content)
                    == slice.hash
            }
            Read::Absent | Read::Drifted => false,
        };
        if !same {
            return Ok(Some(format!(
                "lines {}:{}-{}",
                slice.path, slice.start, slice.end
            )));
        }
    }
    for (path, hash) in &record.unindexed {
        if now.unindexed_hash(path) != *hash {
            return Ok(Some(format!("unindexed {path}")));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_store::{EdgeImage, FileImage, RowChanges};

    fn node(id: &str, kind: &str, name: &str, file: &str, language: &str) -> NodeImage {
        NodeImage {
            id: id.to_string(),
            kind: kind.to_string(),
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: file.to_string(),
            language: language.to_string(),
            start_line: 1,
            end_line: 2,
            signature: None,
            decorators: None,
        }
    }

    /// A changed row names its keys by both images; a rewrite that keeps
    /// every name and path moves neither the name list nor the file list,
    /// and a language that moves under one id is named.
    #[test]
    fn a_probe_names_both_images_and_the_lists_only_when_they_move() {
        let mut moved = node("n:a", "function", "a", "src/a.ts", "typescript");
        moved.end_line = 9;
        let changes = ChangeSet {
            nodes: RowChanges {
                removed: vec![node("n:a", "function", "a", "src/a.ts", "typescript")],
                added: vec![moved],
            },
            edges: RowChanges {
                removed: vec![EdgeImage {
                    source: "n:a".to_string(),
                    target: "n:b".to_string(),
                    kind: "calls".to_string(),
                    line: Some(1),
                    col: Some(0),
                }],
                added: Vec::new(),
            },
            files: RowChanges::default(),
            texts: ["src/a.ts".to_string()].into_iter().collect(),
            ..ChangeSet::default()
        };
        let probe = Probe::new(&changes);
        for key in [
            keys::node("n:a"),
            keys::file_nodes("src/a.ts"),
            keys::file_kind("src/a.ts", "function"),
            keys::name("a"),
            keys::lower_name("a"),
            keys::kind_in("function", "typescript"),
            keys::edges_into("n:b", "calls"),
            keys::edges_from("n:a", "calls"),
            keys::edges_of_kind("calls"),
            keys::text("src/a.ts"),
        ] {
            assert!(probe.keys.contains(&key), "{key:016x}");
        }
        for key in [keys::names(), keys::files(), keys::language_move()] {
            assert!(!probe.keys.contains(&key), "{key:016x}");
        }
        assert!(probe.files.contains("src/a.ts"));

        let flipped = ChangeSet {
            nodes: RowChanges {
                removed: vec![node("n:h", "struct", "h", "inc/h.h", "c")],
                added: vec![node("n:h", "struct", "h", "inc/h.h", "cpp")],
            },
            files: RowChanges {
                removed: vec![FileImage {
                    path: "inc/old.h".to_string(),
                    language: "c".to_string(),
                    generated: false,
                }],
                added: Vec::new(),
            },
            ..ChangeSet::default()
        };
        let probe = Probe::new(&flipped);
        assert!(probe.keys.contains(&keys::language_move()));
        assert!(probe.keys.contains(&keys::files()));
        assert!(probe.keys.contains(&keys::files_of("c")));
        assert!(!probe.keys.contains(&keys::names()));
    }

    /// The property the reuse rests on (W9-00 row 27): over a small index, a
    /// change of one row (a node, an edge, a reference or a file inserted,
    /// deleted or rewritten, a file's text rewritten) that moves what a
    /// lookup answers always reruns the step that made it, checked over
    /// pseudo-random changes for the lookups of the view and its context.
    #[test]
    fn a_row_change_that_moves_a_lookup_reruns_it() {
        use codegraph_core::types::{EdgeKind, Language, NodeKind};
        use codegraph_store::queries::ReferenceSite;

        use super::super::footprint::PassRecord;
        use crate::types::ResolutionContext;

        let dir = std::env::temp_dir().join(format!(
            "cg-reuse-property-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("index.db")).unwrap();
        let options = ExtractOptions::default();
        let names = [
            "save", "onUpdate", "emitAll", "watch", "Thing", "SAVE", "run",
        ];
        let kinds = [
            "function", "method", "class", "variable", "constant", "file", "struct",
        ];
        let languages = ["typescript", "kotlin", "tsx"];
        let files = ["src/a.ts", "src/b.ts", "src/c.kt", "src/d.tsx"];
        let edge_kinds = ["calls", "references", "extends", "contains", "implements"];
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = |bound: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % bound as u64) as usize
        };
        let write_text = |path: &str, text: &str, language: &str, generated: bool| {
            let full = dir.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, text).unwrap();
            let metadata = std::fs::metadata(&full).unwrap();
            let modified = metadata
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64;
            store
                .connection()
                .execute(
                    "INSERT INTO files (path, content_hash, language, size, modified_at, indexed_at, generated) \
                     VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6) ON CONFLICT(path) DO UPDATE SET \
                     content_hash = excluded.content_hash, language = excluded.language, \
                     size = excluded.size, modified_at = excluded.modified_at, generated = excluded.generated",
                    (path, hash_content(text), language, metadata.len() as i64, modified, generated),
                )
                .unwrap();
        };
        let insert_node = |id: &str,
                           kind: &str,
                           name: &str,
                           file: &str,
                           language: &str,
                           line: i64| {
            store
                .connection()
                .execute(
                    "INSERT OR IGNORE INTO nodes (id, kind, name, qualified_name, file_path, language, \
                     start_line, end_line, start_column, end_column, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, 1, 0)",
                    (id, kind, name, format!("A.{name}"), file, language, line, line + 1),
                )
                .unwrap();
        };
        for (at, file) in files.iter().enumerate() {
            write_text(
                file,
                &format!("line one {at}\nbus.emit('x')\nline three\n"),
                languages[at % 3],
                false,
            );
        }
        for at in 0..24 {
            let id = format!("n{at}");
            insert_node(
                &id,
                kinds[at % kinds.len()],
                names[at % names.len()],
                files[at % files.len()],
                languages[at % languages.len()],
                (at % 5) as i64 + 1,
            );
            if at % 4 == 0 {
                store
                    .connection()
                    .execute("UPDATE nodes SET signature = 'const fooThunk = createThunk()' WHERE id = ?1", [&id])
                    .unwrap();
            }
            if at % 6 == 0 {
                store
                    .connection()
                    .execute(
                        "UPDATE nodes SET decorators = '[\"Component\"]' WHERE id = ?1",
                        [&id],
                    )
                    .unwrap();
            }
        }
        for at in 0..30_usize {
            store
                .connection()
                .execute(
                    "INSERT OR IGNORE INTO edges (source, target, kind, line, col) VALUES (?1, ?2, ?3, ?4, 0)",
                    (format!("n{}", at % 24), format!("n{}", (at * 7 + 3) % 24), edge_kinds[at % 5], (at % 4) as i64 + 1),
                )
                .unwrap();
        }
        for (table, at, kind) in [
            ("unresolved_refs", 0, "calls"),
            ("unresolved_refs", 1, "extends"),
            ("resolved_refs", 2, "calls"),
        ] {
            store
                .connection()
                .execute(
                    &format!(
                        "INSERT INTO {table} (from_node_id, reference_name, reference_kind, line, col, file_path, language) \
                         VALUES (?1, 'save', ?2, 2, 0, 'src/a.ts', 'typescript')"
                    ),
                    (format!("n{at}"), kind),
                )
                .unwrap();
        }

        type Lookup = fn(&View<'_>) -> String;
        fn sorted<T: std::fmt::Debug + Ord>(mut items: Vec<T>) -> String {
            items.sort();
            format!("{items:?}")
        }
        let lookups: Vec<(&str, Lookup)> = vec![
            ("node", |view| format!("{:?}", view.node("n1").unwrap())),
            ("nodes", |view| {
                let found = view.nodes(&["n2".to_string(), "n9".to_string()]).unwrap();
                sorted(
                    found
                        .into_iter()
                        .map(|(id, node)| format!("{id}={node:?}"))
                        .collect(),
                )
            }),
            ("nodes_named", |view| {
                format!("{:?}", view.nodes_named("save").unwrap())
            }),
            ("nodes_qualified", |view| {
                format!("{:?}", view.nodes_qualified("A.run").unwrap())
            }),
            ("nodes_in_file", |view| {
                format!("{:?}", view.nodes_in_file("src/a.ts").unwrap())
            }),
            ("callables_in_file", |view| {
                format!("{:?}", view.callables_in_file("src/b.ts").unwrap())
            }),
            ("classes_and_members", |view| {
                let files = ["src/a.ts".to_string(), "src/c.kt".to_string()];
                let found = view.classes_and_members_in_files(&files).unwrap();
                sorted(
                    found
                        .into_iter()
                        .map(|(file, nodes)| format!("{file}={nodes:?}"))
                        .collect(),
                )
            }),
            ("layouts", |view| {
                format!(
                    "{:?}",
                    view.layouts_and_callables_in_file("src/d.tsx").unwrap()
                )
            }),
            ("kinds_in", |view| {
                format!(
                    "{:?}",
                    view.nodes_of_kinds_in(
                        &[NodeKind::Function, NodeKind::Class],
                        &[Language::TypeScript]
                    )
                    .unwrap()
                )
            }),
            ("spans", |view| {
                let spans = view
                    .node_spans_of_kinds_in(
                        &[NodeKind::Variable],
                        &[Language::Kotlin, Language::Tsx],
                    )
                    .unwrap();
                format!(
                    "{:?}",
                    spans
                        .iter()
                        .map(|span| (&span.id, &span.name, span.start_line, span.end_line))
                        .collect::<Vec<_>>()
                )
            }),
            ("method_headers", |view| {
                let headers = view
                    .method_headers_in(&[Language::TypeScript, Language::Kotlin])
                    .unwrap();
                format!(
                    "{:?}",
                    headers
                        .iter()
                        .map(|header| (&header.id, &header.name, header.start_line))
                        .collect::<Vec<_>>()
                )
            }),
            ("signature", |view| {
                format!(
                    "{:?}",
                    view.nodes_of_kind_whose_signature_holds(NodeKind::Constant, "Thunk")
                        .unwrap()
                )
            }),
            ("of_kind", |view| {
                format!("{:?}", view.nodes_of_kind(NodeKind::Class).unwrap())
            }),
            ("named_like", |view| {
                format!(
                    "{:?}",
                    view.callables_named_like(&["on[A-Z]*"], &["%emit%"], &["watch"])
                        .unwrap()
                )
            }),
            ("callables_in", |view| {
                format!(
                    "{:?}",
                    view.callables_in_languages(&[Language::Kotlin]).unwrap()
                )
            }),
            ("decorated", |view| {
                format!(
                    "{:?}",
                    view.nodes_with_decorator(Language::TypeScript, "Component")
                        .unwrap()
                )
            }),
            ("files", |view| format!("{:?}", view.files().unwrap())),
            ("files_of", |view| {
                format!(
                    "{:?}",
                    view.files_of_languages(&[Language::Kotlin]).unwrap()
                )
            }),
            ("edges_into", |view| {
                let edges = view.edges_into("n3", EdgeKind::Calls).unwrap();
                format!(
                    "{:?}",
                    edges
                        .iter()
                        .map(|edge| (&edge.source, edge.line))
                        .collect::<Vec<_>>()
                )
            }),
            ("edges_from", |view| {
                let edges = view.edges_from("n1", EdgeKind::References).unwrap();
                format!(
                    "{:?}",
                    edges
                        .iter()
                        .map(|edge| (&edge.target, edge.line))
                        .collect::<Vec<_>>()
                )
            }),
            ("edges_into_each", |view| {
                let found = view
                    .edges_into_each(&["n3".to_string(), "n10".to_string()], EdgeKind::Extends)
                    .unwrap();
                sorted(
                    found
                        .into_iter()
                        .map(|(target, edges)| {
                            format!(
                                "{target}={:?}",
                                edges
                                    .iter()
                                    .map(|edge| (&edge.source, edge.line))
                                    .collect::<Vec<_>>()
                            )
                        })
                        .collect(),
                )
            }),
            ("edges_from_each", |view| {
                let found = view
                    .edges_from_each(&["n4".to_string(), "n5".to_string()], EdgeKind::Contains)
                    .unwrap();
                sorted(
                    found
                        .into_iter()
                        .map(|(source, edges)| {
                            format!(
                                "{source}={:?}",
                                edges
                                    .iter()
                                    .map(|edge| (&edge.target, edge.line))
                                    .collect::<Vec<_>>()
                            )
                        })
                        .collect(),
                )
            }),
            ("edges_of_kind", |view| {
                let edges = view.edges_of_kind(EdgeKind::Implements).unwrap();
                sorted(
                    edges
                        .iter()
                        .map(|edge| (edge.source.clone(), edge.target.clone(), edge.line))
                        .collect(),
                )
            }),
            ("called_from", |view| {
                let called = view
                    .called_from(
                        &["n3".to_string(), "n17".to_string()],
                        &[Language::TypeScript],
                    )
                    .unwrap();
                sorted(called.into_iter().collect())
            }),
            ("refs_of_kind", |view| {
                let refs = view.unresolved_refs_of_kind(EdgeKind::Calls).unwrap();
                sorted(
                    refs.iter()
                        .map(|reference| {
                            (
                                reference.from_node_id.clone(),
                                reference.reference_name.clone(),
                                reference.line,
                            )
                        })
                        .collect(),
                )
            }),
            ("refs_at", |view| {
                let site = ReferenceSite {
                    from_node_id: "n2".to_string(),
                    line: 2,
                    col: 0,
                };
                let refs = view.refs_at(site).unwrap();
                sorted(
                    refs.iter()
                        .map(|reference| {
                            (
                                reference.reference_name.clone(),
                                reference.reference_kind.as_str().to_string(),
                            )
                        })
                        .collect(),
                )
            }),
            ("text", |view| format!("{:?}", view.text("src/a.ts"))),
            ("text_lines", |view| {
                format!("{:?}", view.text_lines("src/b.ts", 1, 2))
            }),
            ("generated", |view| {
                format!("{:?}", view.is_flagged_generated("src/c.kt"))
            }),
            ("generated_kept", |view| {
                format!("{:?}", view.is_flagged_generated("src/a.ts"))
            }),
            ("gated", |view| {
                let passed = view
                    .gated_texts(&super::super::callback::EMITTER_GATE, 0, |_| true)
                    .unwrap();
                format!(
                    "{:?}",
                    passed
                        .iter()
                        .map(|(path, text)| (path.clone(), text.to_string()))
                        .collect::<Vec<_>>()
                )
            }),
            ("context_name", |view| {
                format!("{:?}", view.context().get_nodes_by_name("run"))
            }),
            ("context_lower", |view| {
                format!("{:?}", view.context().get_nodes_by_lower_name("save"))
            }),
            ("context_qualified", |view| {
                format!("{:?}", view.context().get_nodes_by_qualified_name("A.save"))
            }),
            ("context_file", |view| {
                format!("{:?}", view.context().get_nodes_in_file("src/c.kt"))
            }),
            ("context_id", |view| {
                format!("{:?}", view.context().get_node_by_id("n7"))
            }),
            ("context_names", |view| {
                sorted(view.context().known_node_names())
            }),
            ("context_files", |view| {
                format!("{:?}", view.context().get_all_files())
            }),
            ("context_indexed", |view| {
                format!("{:?}", view.context().is_indexed_file("src/e.ts"))
            }),
            ("context_exported", |view| {
                format!("{:?}", view.context().file_has_exported_node("src/d.tsx"))
            }),
        ];

        let mut moved = 0;
        for round in 0..300 {
            let mut before = Vec::new();
            for (name, lookup) in &lookups {
                let view = View::new(&store, &dir, options.max_file_size);
                view.recorder().begin_pass();
                let answer = lookup(&view);
                let record = PassRecord::new(&[], &view.recorder().end_pass())
                    .to_rows()
                    .unwrap()
                    .head;
                before.push((*name, *lookup, answer, record));
            }
            store.begin_change_log(1000).unwrap();
            let node = format!("n{}", next(30));
            let what = next(13);
            match what {
                0 => {
                    let (kind, name, file) = (
                        kinds[next(kinds.len())],
                        names[next(names.len())],
                        files[next(files.len())],
                    );
                    insert_node(
                        &node,
                        kind,
                        name,
                        file,
                        languages[next(3)],
                        next(5) as i64 + 1,
                    );
                }
                1 => {
                    store
                        .connection()
                        .execute("DELETE FROM nodes WHERE id = ?1", [&node])
                        .unwrap();
                }
                2 => {
                    let column =
                        ["name", "kind", "language", "file_path", "qualified_name"][next(5)];
                    let value = match column {
                        "name" => names[next(names.len())].to_string(),
                        "kind" => kinds[next(kinds.len())].to_string(),
                        "language" => languages[next(3)].to_string(),
                        "file_path" => files[next(files.len())].to_string(),
                        _ => format!("A.{}", names[next(names.len())]),
                    };
                    store
                        .connection()
                        .execute(
                            &format!("UPDATE nodes SET {column} = ?1 WHERE id = ?2"),
                            (value, &node),
                        )
                        .unwrap();
                }
                3 => {
                    let column = ["start_line", "end_line", "is_exported"][next(3)];
                    store
                        .connection()
                        .execute(
                            &format!("UPDATE nodes SET {column} = ?1 WHERE id = ?2"),
                            (next(6) as i64, &node),
                        )
                        .unwrap();
                }
                4 => {
                    let value =
                        [None, Some("const barThunk = 1"), Some("function emitAll()")][next(3)];
                    store
                        .connection()
                        .execute(
                            "UPDATE nodes SET signature = ?1 WHERE id = ?2",
                            (value, &node),
                        )
                        .unwrap();
                }
                5 => {
                    let value = [None, Some("[\"Component\"]"), Some("[\"Injectable\"]")][next(3)];
                    store
                        .connection()
                        .execute(
                            "UPDATE nodes SET decorators = ?1 WHERE id = ?2",
                            (value, &node),
                        )
                        .unwrap();
                }
                6 => {
                    let (target, kind, line) = (
                        format!("n{}", next(30)),
                        edge_kinds[next(5)],
                        next(4) as i64 + 1,
                    );
                    store
                        .connection()
                        .execute(
                            "INSERT OR IGNORE INTO edges (source, target, kind, line, col) \
                             SELECT a.id, b.id, ?3, ?4, 0 FROM nodes a, nodes b WHERE a.id = ?1 AND b.id = ?2",
                            (&node, target, kind, line),
                        )
                        .unwrap();
                }
                7 => {
                    store
                        .connection()
                        .execute(
                            "DELETE FROM edges WHERE id IN (SELECT id FROM edges ORDER BY id LIMIT 1 OFFSET ?1)",
                            [next(20) as i64],
                        )
                        .unwrap();
                }
                8 => {
                    let (table, kind) = (
                        ["unresolved_refs", "resolved_refs"][next(2)],
                        ["calls", "extends"][next(2)],
                    );
                    store
                        .connection()
                        .execute(
                            &format!(
                                "INSERT INTO {table} (from_node_id, reference_name, reference_kind, line, col, file_path, language) \
                                 SELECT id, 'save', ?2, 2, 0, 'src/a.ts', 'typescript' FROM nodes WHERE id = ?1"
                            ),
                            (&node, kind),
                        )
                        .unwrap();
                }
                9 => {
                    let table = ["unresolved_refs", "resolved_refs"][next(2)];
                    store
                        .connection()
                        .execute(&format!("DELETE FROM {table} WHERE id IN (SELECT id FROM {table} ORDER BY id LIMIT 1)"), [])
                        .unwrap();
                }
                10 => {
                    let file =
                        ["src/a.ts", "src/b.ts", "src/c.kt", "src/d.tsx", "src/e.ts"][next(5)];
                    let text = [
                        "line one\nbus.emit('x')\nline three\n",
                        "other one\nline two\n",
                        "line one\nbus.on('x', h)\n",
                        "",
                    ][next(4)];
                    let (language, generated) = (languages[next(3)], next(4) == 0);
                    write_text(file, text, language, generated);
                }
                11 => {
                    let file = ["src/b.ts", "src/c.kt", "src/e.ts"][next(3)];
                    store
                        .connection()
                        .execute("DELETE FROM files WHERE path = ?1", [file])
                        .unwrap();
                }
                _ => {
                    // a.ts and d.tsx are never removed, so the flip always lands.
                    let file = ["src/a.ts", "src/d.tsx", "src/c.kt"][next(3)];
                    store
                        .connection()
                        .execute(
                            "UPDATE files SET generated = 1 - generated WHERE path = ?1",
                            [file],
                        )
                        .unwrap();
                }
            }
            let changes = store
                .take_change_log()
                .unwrap()
                .expect("the log holds the change");
            let probe = Probe::new(&changes);
            let now = Now::new(&store, &dir, &options);
            for (name, lookup, answer, record) in &before {
                let view = View::new(&store, &dir, options.max_file_size);
                let after = lookup(&view);
                if after != *answer {
                    moved += 1;
                    let reason = rerun_reason(record, &probe, &now, &view).unwrap();
                    assert!(
                        reason.is_some(),
                        "round {round}, change {what}: `{name}` moved but its step would be kept\n\
                         before: {answer}\nafter: {after}\nchanges: {changes:?}"
                    );
                }
            }
        }
        assert!(moved > 200, "only {moved} lookups moved");
        drop(store);
        std::fs::remove_dir_all(dir).ok();
    }
}
