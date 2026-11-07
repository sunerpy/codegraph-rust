//! The Go prerequisite steps (upstream `goCrossFileMethodContainsEdges` and
//! `goImplementsEdges` at `e8ddc4e2`, W7-15b): the `contains` edge from a Go
//! type to each method it receives that extraction left with its file alone,
//! and the `implements` edge from a Go type to each interface its method set
//! covers. They run before the passes, which read their edges as base edges
//! (the stage's `PREREQUISITES`).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use anyhow::Result;
use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind, UnresolvedRef};
use regex::Regex;
use serde_json::json;

use super::MAX_CALLBACKS_PER_CHANNEL;
use super::dispatch::{methods_of, methods_of_each, place, synthesized};
use super::stage::View;
use crate::frameworks::js_scan::JS_SPACE;
use crate::go_packages::go_written_qualifier_in;
use crate::go_std_methods::{go_signature_arity, go_signature_parts, go_std_methods};
use crate::import_resolver::extract_import_mappings;
use crate::source_facts::SourceFacts;
use crate::types::{ImportMapping, RefView};

/// The steps run only over a project with Go files (upstream's `has('go')`).
pub(super) fn gate(languages: &HashSet<Language>) -> bool {
    languages.contains(&Language::Go)
}

/// The kinds of node a Go method's receiver can be.
const TYPE_KINDS: [NodeKind; 5] = [
    NodeKind::Struct,
    NodeKind::Class,
    NodeKind::Interface,
    NodeKind::Enum,
    NodeKind::TypeAlias,
];

/// An exported Go identifier starts with an upper-case letter, in Unicode's
/// sense (Go's `unicode.IsUpper`, upstream's `/^\p{Lu}/u`).
static EXPORTED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\p{Lu}").expect("the exported-name pattern compiles"));

/// A gRPC client method's last parameter, `opts ...grpc.CallOption`
/// (upstream `goGrpcServerArity`'s pattern, with JavaScript's `\w` and `\s`).
static CALL_OPTIONS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        &r"^(?:[A-Za-z_][A-Za-z0-9_]*\s+)?\.\.\.\s*(?:grpc\.)?CallOption$".replace(r"\s", JS_SPACE),
    )
    .expect("the call-options pattern compiles")
});

/// The directory of a path, the Go package a file belongs to: what comes
/// before its last `/`, or nothing.
fn dir_of(path: &str) -> String {
    let path = path.replace('\\', "/");
    path.rfind('/')
        .map_or_else(String::new, |at| path[..at].to_string())
}

/// What a Go method matches an interface's method by (upstream
/// `goMethodKey`, #2746): its name, and for an unexported name the
/// directory of its file as well, since Go makes the namesake of another
/// package another method.
pub(super) fn method_key(method: &Node) -> String {
    if EXPORTED.is_match(&method.name) {
        return method.name.clone();
    }
    format!("{}\0{}", dir_of(&method.file_path), method.name)
}

/// [`method_key`] for a standard-library type's method (upstream
/// `goStdlibMethodKey`): its name, and for an unexported one its package,
/// spelled so it is no directory of the project's.
fn std_method_key(import_path: Option<&str>, name: &str) -> String {
    if EXPORTED.is_match(name) {
        return name.to_string();
    }
    format!("std:{}\0{name}", import_path.unwrap_or(""))
}

/// The kind of a Go type an embedding can name: a struct, an interface or a
/// defined type; `None` for any other node.
pub(super) fn type_kind(node: &Node) -> Option<NodeKind> {
    (node.language == Language::Go
        && matches!(
            node.kind,
            NodeKind::Struct | NodeKind::Interface | NodeKind::TypeAlias
        ))
    .then_some(node.kind)
}

/// Whether a supertype edge out of a Go type is one of its embeddings
/// (upstream `isGoEmbedding`, #2397): declared, never synthesized, into a
/// Go type ([`type_kind`] of its target), and out of an interface only into
/// an interface.
pub(super) fn is_embedding(
    edge: &Edge,
    source: Option<NodeKind>,
    target: Option<NodeKind>,
) -> bool {
    edge.provenance.as_deref() != Some("heuristic")
        && target.is_some()
        && (source != Some(NodeKind::Interface) || target == Some(NodeKind::Interface))
}

/// Cross-file Go method ownership (upstream `goCrossFileMethodContainsEdges`,
/// #583): extraction gives a type the methods its own file declares, so a
/// method declared elsewhere in its package, and any method of a defined
/// type, has only its file's `contains` edge. Go declares a receiver type in
/// the method's package, a directory, so the first type of its name there
/// owns it, with no provenance, as a parsed `contains` edge has. The
/// methods' `contains` parents are read for all of them at once.
pub(super) fn method_contains_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut named: HashMap<String, Vec<Node>> = HashMap::new();
    let methods = view.nodes_of_kinds_in(&[NodeKind::Method], &[Language::Go])?;
    let ids: Vec<String> = methods.iter().map(|method| method.id.clone()).collect();
    let parents = view.edges_into_each(&ids, EdgeKind::Contains)?;
    let sources: Vec<String> = parents
        .values()
        .flatten()
        .map(|edge| edge.source.clone())
        .collect();
    let sources = view.nodes(&sources)?;
    for method in methods {
        let Some(sep) = method.qualified_name.rfind("::") else {
            continue;
        };
        if sep == 0 {
            continue;
        }
        let receiver = &method.qualified_name[..sep];
        let owned = parents.get(&method.id).is_some_and(|edges| {
            edges.iter().any(|edge| {
                sources
                    .get(&edge.source)
                    .is_some_and(|source| TYPE_KINDS.contains(&source.kind))
            })
        });
        if owned {
            continue;
        }
        if !named.contains_key(receiver) {
            named.insert(receiver.to_string(), view.nodes_named(receiver)?);
        }
        let dir = dir_of(&method.file_path);
        let Some(owner) = named[receiver].iter().find(|node| {
            node.language == Language::Go
                && TYPE_KINDS.contains(&node.kind)
                && dir_of(&node.file_path) == dir
        }) else {
            continue;
        };
        if !seen.insert((owner.id.clone(), method.id.clone())) {
            continue;
        }
        edges.push(Edge {
            id: None,
            source: owner.id.clone(),
            target: method.id.clone(),
            kind: EdgeKind::Contains,
            metadata: Some(json!({ "synthesizedBy": "go-method-contains" })),
            line: Some(method.start_line),
            col: None,
            provenance: None,
        });
    }
    Ok(edges)
}

/// A type's own methods by [`method_key`], each key with the signatures its
/// declarations store.
struct Own {
    keys: Arc<HashSet<String>>,
    signatures: HashMap<String, Vec<Option<String>>>,
}

/// What the types embed from outside the index, read off resolution's
/// failed `extends` rows: a standard-library type's methods (or `error`'s)
/// by key with their arities, or the embedded type itself, named by the
/// package it is written through.
#[derive(Default)]
struct Outside {
    stdlib: Vec<HashMap<String, String>>,
    opaque: Vec<String>,
}

/// What a Go type has, or an interface asks for (upstream
/// `GoMethodNames`).
struct MethodNames {
    /// Its methods by key: its own, the project types' it embeds, and those
    /// the standard-library types it embeds bring.
    methods: Arc<HashSet<String>>,
    /// Embedded types from outside the index whose methods nothing names.
    opaque: HashSet<String>,
    /// Whether `methods` is the type's own set: it embeds nothing (upstream
    /// compares the sets by identity).
    own: bool,
}

/// Each method of a type's method set by key → the arities
/// (`params/results`) its declarations have there; `None` when one of them
/// does not read, so the name rules nothing out.
type Arities = HashMap<String, Option<HashSet<String>>>;

/// The method sets of the Go types, read once each.
struct Sets<'v, 'a> {
    view: &'v View<'a>,
    /// The types' own methods, read for all of them at once and taken by
    /// [`Self::own`].
    methods: HashMap<String, Vec<Node>>,
    own: HashMap<String, Arc<Own>>,
    embeds: HashMap<String, Vec<String>>,
    outside: HashMap<String, Outside>,
    arities: HashMap<String, Arc<Arities>>,
}

impl Sets<'_, '_> {
    fn own(&mut self, id: &str) -> Result<Arc<Own>> {
        if let Some(own) = self.own.get(id) {
            return Ok(own.clone());
        }
        let methods = match self.methods.remove(id) {
            Some(methods) => methods,
            None => methods_of(self.view, id)?,
        };
        let mut keys = HashSet::new();
        let mut signatures: HashMap<String, Vec<Option<String>>> = HashMap::new();
        for method in methods {
            let key = method_key(&method);
            keys.insert(key.clone());
            signatures.entry(key).or_default().push(method.signature);
        }
        let own = Arc::new(Own {
            keys: Arc::new(keys),
            signatures,
        });
        self.own.insert(id.to_string(), own.clone());
        Ok(own)
    }

    fn embeds(&self, id: &str) -> &[String] {
        self.embeds.get(id).map_or(&[], Vec::as_slice)
    }

    /// The type and every type it embeds, down to the last level; Go allows
    /// a struct to embed a pointer to itself, or to one embedding it back.
    fn closure(&self, id: &str) -> Vec<String> {
        if self.embeds(id).is_empty() {
            return vec![id.to_string()];
        }
        let mut reached: Vec<String> = vec![id.to_string()];
        let mut met: HashSet<&str> = HashSet::from([id]);
        let mut pending = vec![id];
        while let Some(at) = pending.pop() {
            for target in self.embeds(at) {
                if met.insert(target) {
                    reached.push(target.clone());
                    pending.push(target);
                }
            }
        }
        reached
    }

    fn method_names(&mut self, id: &str) -> Result<MethodNames> {
        let types = self.closure(id);
        if types.len() == 1 && !self.outside.contains_key(id) {
            return Ok(MethodNames {
                methods: self.own(id)?.keys.clone(),
                opaque: HashSet::new(),
                own: true,
            });
        }
        let mut methods = HashSet::new();
        let mut opaque = HashSet::new();
        for t in &types {
            methods.extend(self.own(t)?.keys.iter().cloned());
            if let Some(out) = self.outside.get(t) {
                for known in &out.stdlib {
                    methods.extend(known.keys().cloned());
                }
                opaque.extend(out.opaque.iter().cloned());
            }
        }
        Ok(MethodNames {
            methods: Arc::new(methods),
            opaque,
            own: false,
        })
    }

    fn method_arities(&mut self, id: &str) -> Result<Arc<Arities>> {
        if let Some(arities) = self.arities.get(id) {
            return Ok(arities.clone());
        }
        let mut arities = Arities::new();
        for t in self.closure(id) {
            let own = self.own(&t)?;
            for (name, signatures) in &own.signatures {
                let known = arities
                    .entry(name.clone())
                    .or_insert_with(|| Some(HashSet::new()));
                let Some(set) = known else {
                    continue;
                };
                for signature in signatures {
                    match go_signature_arity(signature.as_deref()) {
                        Some(arity) => {
                            set.insert(arity);
                        }
                        None => {
                            *known = None;
                            break;
                        }
                    }
                }
            }
            if let Some(out) = self.outside.get(&t) {
                for methods in &out.stdlib {
                    for (name, arity) in methods {
                        let known = arities
                            .entry(name.clone())
                            .or_insert_with(|| Some(HashSet::new()));
                        if let Some(set) = known {
                            set.insert(arity.clone());
                        }
                    }
                }
            }
        }
        let arities = Arc::new(arities);
        self.arities.insert(id.to_string(), arities.clone());
        Ok(arities)
    }

    /// The arities an implementer may declare each of an interface's
    /// methods with (upstream `wantedArities`): the interface's own; a gRPC
    /// client interface, every method of which takes the call options last,
    /// also takes its service's server side.
    fn wanted_arities(&mut self, iface: &str) -> Result<Arc<Arities>> {
        let own = self.method_arities(iface)?;
        let mut wanted = Arities::new();
        for t in self.closure(iface) {
            let declared = self.own(&t)?;
            for (name, signatures) in &declared.signatures {
                let arities = wanted
                    .entry(name.clone())
                    .or_insert_with(|| Some(own.get(name).cloned().flatten().unwrap_or_default()));
                for signature in signatures {
                    let Some(served) = grpc_server_arity(signature.as_deref()) else {
                        return Ok(own);
                    };
                    if let Some(set) = arities {
                        set.insert(served);
                    }
                }
            }
        }
        // What only a standard-library embedding asks for keeps the table's.
        for (name, arities) in own.iter() {
            wanted
                .entry(name.clone())
                .or_insert_with(|| arities.clone());
        }
        Ok(Arc::new(wanted))
    }

    /// Whether `id` has, for each wanted method, a declaration at one of the
    /// wanted arities (upstream `aritiesFit`).
    fn arities_fit(&mut self, id: &str, wanted: &Arities) -> Result<bool> {
        let have = self.method_arities(id)?;
        for (name, arities) in wanted {
            let (Some(arities), Some(Some(got))) = (arities, have.get(name)) else {
                continue;
            };
            if !arities.iter().any(|arity| got.contains(arity)) {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// The arity a gRPC server implements an RPC with, read off the client's
/// signature for it (upstream `goGrpcServerArity`): a unary call is served
/// without the call options and returns a response and an error; a
/// streaming one gets its stream in their place and returns an error.
/// `None` when the signature is no client's.
fn grpc_server_arity(signature: Option<&str>) -> Option<String> {
    let (params, results) = go_signature_parts(signature)?;
    let options = params.last()?;
    if !CALL_OPTIONS.is_match(options) {
        return None;
    }
    let unary = results.len() == 2 && results[0].starts_with('*');
    Some(format!(
        "{}/{}",
        params.len() - 1,
        if unary { 2 } else { 1 }
    ))
}

fn covers(have: &HashSet<String>, want: &HashSet<String>) -> bool {
    have.len() >= want.len() && want.iter().all(|key| have.contains(key))
}

fn covers_opaque(have: &MethodNames, want: &MethodNames) -> bool {
    want.opaque.iter().all(|key| have.opaque.contains(key))
}

/// A file's text as the stage reads it, with its Go imports; `None` when the
/// file is not what the index holds.
type FileFacts = Option<(SourceFacts, Vec<ImportMapping>)>;

/// The package an embedding of a type from outside the index is written
/// through (upstream `goRefQualification`'s `written` and `imported`), read
/// from the stage's fresh text; `None` when that text is unavailable.
fn qualification(
    view: &View<'_>,
    files: &mut HashMap<String, FileFacts>,
    reference: &UnresolvedRef,
) -> Option<(Option<String>, Option<ImportMapping>)> {
    let file = files.entry(reference.file_path.clone()).or_insert_with(|| {
        view.text(&reference.file_path).map(|text| {
            let imports = extract_import_mappings(&text, Language::Go);
            (SourceFacts::new(text), imports)
        })
    });
    let (facts, imports) = file.as_ref()?;
    let view_ref = RefView {
        row_id: reference.id,
        from_node_id: reference.from_node_id.clone(),
        reference_name: reference.reference_name.clone(),
        reference_kind: reference.reference_kind,
        line: reference.line,
        column: reference.col,
        file_path: reference.file_path.clone(),
        language: reference.language,
        is_function_ref: reference.is_function_ref,
        reference_subkind: reference.reference_subkind,
    };
    let Some(written) = go_written_qualifier_in(&view_ref, facts) else {
        return Some((None, None));
    };
    let imported = imports
        .iter()
        .find(|import| import.local_name == written)
        .cloned();
    Some((Some(written), imported))
}

/// Go implicit interface satisfaction (upstream `goImplementsEdges`, #584,
/// #2402, #2417, #2430, #2447, #2746, #2759): a struct or defined type whose
/// method set, embeddings included, covers an interface's method keys at
/// their parameter and result counts implements it. A standard-library or
/// `error` embedding brings the methods the table names; another type from
/// outside the index is opaque, matched only by the same embedding, or else
/// by the one type with all the rest. Empty interfaces match nothing, a type
/// embedding the interface keeps the edge its embedding is, and an interface
/// takes at most [`MAX_CALLBACKS_PER_CHANNEL`] implementers, those declaring
/// all its methods first.
pub(super) fn implements_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let implementers =
        view.nodes_of_kinds_in(&[NodeKind::Struct, NodeKind::TypeAlias], &[Language::Go])?;
    let interfaces = view.nodes_of_kinds_in(&[NodeKind::Interface], &[Language::Go])?;
    let mut kinds: HashMap<String, NodeKind> = HashMap::new();
    for node in &implementers {
        kinds.insert(node.id.clone(), node.kind);
    }
    for node in &interfaces {
        kinds.insert(node.id.clone(), NodeKind::Interface);
    }

    let mut embeds: HashMap<String, Vec<String>> = HashMap::new();
    for kind in [EdgeKind::Extends, EdgeKind::Implements] {
        for edge in view.edges_of_kind(kind)? {
            let Some(&source) = kinds.get(&edge.source) else {
                continue;
            };
            if is_embedding(&edge, Some(source), kinds.get(&edge.target).copied()) {
                embeds.entry(edge.source).or_default().push(edge.target);
            }
        }
    }

    let mut outside: HashMap<String, Outside> = HashMap::new();
    let mut files: HashMap<String, FileFacts> = HashMap::new();
    for reference in view.unresolved_refs_of_kind(EdgeKind::Extends)? {
        if reference.language != Language::Go
            || reference.file_path.is_empty()
            || !kinds.contains_key(&reference.from_node_id)
        {
            continue;
        }
        let name = reference.reference_name.as_str();
        let found = outside.entry(reference.from_node_id.clone()).or_default();
        let Some((written, imported)) = qualification(view, &mut files, &reference) else {
            // Text the index does not hold: an embedding nothing describes,
            // which no other type's matches.
            found.opaque.push(format!(
                "?{}:{}:{}#{name}",
                reference.file_path, reference.line, reference.col
            ));
            continue;
        };
        let import_path = imported.as_ref().map(|import| import.source.as_str());
        let known = if imported.is_some() {
            go_std_methods(import_path, name)
        } else if written.is_some() {
            None
        } else {
            go_std_methods(None, name)
        };
        if let Some(known) = known {
            found.stdlib.push(
                known
                    .into_iter()
                    .map(|(method, arity)| (std_method_key(import_path, method), arity.to_string()))
                    .collect(),
            );
        } else if let Some(import) = &imported {
            found.opaque.push(format!("{}.{name}", import.source));
        } else if let Some(written) = &written {
            found.opaque.push(format!("{written}.{name}"));
        } else {
            found
                .opaque
                .push(format!("{}#{name}", dir_of(&reference.file_path)));
        }
    }

    // An embedding's target is one of these types, so their own methods
    // are every type's a method set reads.
    let ids: Vec<String> = implementers
        .iter()
        .chain(&interfaces)
        .map(|node| node.id.clone())
        .collect();
    let mut sets = Sets {
        view,
        methods: methods_of_each(view, &ids)?,
        own: HashMap::new(),
        embeds,
        outside,
        arities: HashMap::new(),
    };
    let mut candidates: Vec<(&Node, MethodNames)> = Vec::new();
    for node in &implementers {
        let have = sets.method_names(&node.id)?;
        if have.methods.is_empty() && have.opaque.is_empty() {
            continue;
        }
        candidates.push((node, have));
    }

    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for iface in &interfaces {
        let want = sets.method_names(&iface.id)?;
        if want.methods.is_empty() && want.opaque.is_empty() {
            continue;
        }
        let mut added = 0;
        let mut through_embedding: Vec<&Node> = Vec::new();
        let mut wanted: Option<Arc<Arities>> = None;
        let mut covering = 0;
        let mut sole: Option<&Node> = None;
        for (node, have) in &candidates {
            if added >= MAX_CALLBACKS_PER_CHANNEL {
                break;
            }
            if !covers(&have.methods, &want.methods) {
                continue;
            }
            let fits = match &wanted {
                Some(wanted) => sets.arities_fit(&node.id, wanted)?,
                None => {
                    let made = sets.wanted_arities(&iface.id)?;
                    let fits = sets.arities_fit(&node.id, &made)?;
                    wanted = Some(made);
                    fits
                }
            };
            if !fits {
                continue;
            }
            covering += 1;
            if sets.embeds(&node.id).contains(&iface.id) {
                continue;
            }
            if !covers_opaque(have, &want) {
                if covering == 1 {
                    sole = Some(node);
                }
                continue;
            }
            let own = sets.own(&node.id)?;
            if !want.opaque.is_empty() || (!have.own && !covers(&own.keys, &want.methods)) {
                through_embedding.push(node);
            } else if link(&mut edges, &mut seen, node, iface) {
                added += 1;
            }
        }
        if let Some(node) = sole
            && covering == 1
            && !want.methods.is_empty()
        {
            through_embedding.push(node);
        }
        for node in through_embedding {
            if added >= MAX_CALLBACKS_PER_CHANNEL {
                break;
            }
            if link(&mut edges, &mut seen, node, iface) {
                added += 1;
            }
        }
    }
    Ok(edges)
}

/// The `implements` edge from `node` to `iface`, once each pair.
fn link(
    edges: &mut Vec<Edge>,
    seen: &mut HashSet<(String, String)>,
    node: &Node,
    iface: &Node,
) -> bool {
    if !seen.insert((node.id.clone(), iface.id.clone())) {
        return false;
    }
    edges.push(synthesized(
        node,
        iface,
        EdgeKind::Implements,
        node.start_line,
        json!({
            "synthesizedBy": "go-implements",
            "via": iface.name,
            "registeredAt": place(node),
        }),
    ));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::node_id::hash_content;
    use codegraph_core::types::FileRecord;
    use codegraph_store::Store;
    use std::path::{Path, PathBuf};

    fn node(id: &str, kind: NodeKind, name: &str, file: &str, line: i64) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: file.to_string(),
            language: Language::Go,
            start_line: line,
            end_line: line,
            start_column: 0,
            end_column: 1,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: true,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }

    #[test]
    fn a_method_key_holds_an_unexported_name_with_its_package() {
        let method = |name: &str, file: &str| node("m", NodeKind::Method, name, file, 1);
        assert_eq!(method_key(&method("Save", "svc/a.go")), "Save");
        assert_eq!(method_key(&method("save", "svc/a.go")), "svc\0save");
        assert_eq!(method_key(&method("save", "a.go")), "\0save");
        // Go's exported names start with a letter of category Lu: not a
        // title-case letter, nor a Roman numeral Rust calls upper case.
        assert_eq!(method_key(&method("Éclair", "svc/a.go")), "Éclair");
        assert_eq!(method_key(&method("ǅx", "svc/a.go")), "svc\0ǅx");
        assert_eq!(method_key(&method("Ⅰx", "svc/a.go")), "svc\0Ⅰx");
        assert_eq!(
            std_method_key(Some("testing"), "private"),
            "std:testing\0private"
        );
        assert_eq!(std_method_key(None, "Error"), "Error");
    }

    #[test]
    fn a_grpc_client_signature_gives_the_server_arity() {
        let served = |signature: &str| grpc_server_arity(Some(signature));
        assert_eq!(
            served("(ctx context.Context, in *RangeRequest, opts ...grpc.CallOption) (*RangeResponse, error)")
                .as_deref(),
            Some("2/2")
        );
        assert_eq!(
            served("(ctx context.Context, opts ...grpc.CallOption) (KV_WatchClient, error)")
                .as_deref(),
            Some("1/1")
        );
        assert_eq!(
            served("(ctx context.Context, opts ...CallOption) (Watch_WatchClient, error)")
                .as_deref(),
            Some("1/1")
        );
        assert_eq!(served("(id types.ID) Peer"), None);
        assert_eq!(served("(a int"), None);
        assert_eq!(grpc_server_arity(None), None);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cg-go-synthesis-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("svc")).unwrap();
        dir
    }

    const STRUCT_GO: &str = "package svc\n\nimport \"io\"\n\ntype S struct {\n\tio.Closer\n}\n";
    const IFACE_GO: &str = "package svc\n\ntype C interface {\n\tClose() error\n}\n";

    /// A Go project of two files: `S` embeds `io.Closer`, `C` asks for its
    /// `Close`; the index records `svc/s.go` as `recorded`.
    fn closer_project(dir: &Path, recorded: &str) -> Store {
        std::fs::write(dir.join("svc/s.go"), STRUCT_GO).unwrap();
        std::fs::write(dir.join("svc/c.go"), IFACE_GO).unwrap();
        let mut store = Store::open(&dir.join("index.db")).unwrap();
        let close = Node {
            signature: Some("() error".to_string()),
            qualified_name: "C::Close".to_string(),
            ..node("method:close", NodeKind::Method, "Close", "svc/c.go", 4)
        };
        store
            .upsert_nodes(&[
                node("struct:s", NodeKind::Struct, "S", "svc/s.go", 5),
                node("interface:c", NodeKind::Interface, "C", "svc/c.go", 3),
                close,
            ])
            .unwrap();
        store
            .insert_edges(&[Edge {
                id: None,
                source: "interface:c".to_string(),
                target: "method:close".to_string(),
                kind: EdgeKind::Contains,
                metadata: None,
                line: None,
                col: None,
                provenance: None,
            }])
            .unwrap();
        store
            .insert_unresolved_refs(&[UnresolvedRef {
                id: None,
                from_node_id: "struct:s".to_string(),
                reference_name: "Closer".to_string(),
                reference_kind: EdgeKind::Extends,
                line: 6,
                col: 4,
                candidates: None,
                file_path: "svc/s.go".to_string(),
                language: Language::Go,
                is_function_ref: false,
                reference_subkind: None,
            }])
            .unwrap();
        for (path, text) in [("svc/s.go", recorded), ("svc/c.go", IFACE_GO)] {
            store
                .upsert_file(&FileRecord {
                    path: path.to_string(),
                    content_hash: hash_content(text),
                    language: Language::Go,
                    size: 0,
                    modified_at: 0,
                    indexed_at: 0,
                    node_count: 0,
                    errors: Vec::new(),
                    generated: false,
                })
                .unwrap();
        }
        store
    }

    fn implements(store: &Store, dir: &Path) -> Vec<(String, String)> {
        crate::synthesis::synthesize(store, dir, 1 << 20)
            .unwrap()
            .into_iter()
            .filter(|edge| edge.kind == EdgeKind::Implements)
            .map(|edge| (edge.source, edge.target))
            .collect()
    }

    /// The package an embedding is written through is read from the text the
    /// index holds; text that has drifted describes nothing, so the embedded
    /// type brings no methods until a sync reads the file again.
    #[test]
    fn an_embedding_in_drifted_text_brings_no_methods() {
        let dir = temp_dir("fresh");
        let store = closer_project(&dir, STRUCT_GO);
        assert_eq!(
            implements(&store, &dir),
            [("struct:s".to_string(), "interface:c".to_string())]
        );
        drop(store);
        std::fs::remove_dir_all(&dir).unwrap();

        let dir = temp_dir("drifted");
        let store = closer_project(&dir, "package svc\n");
        assert!(implements(&store, &dir).is_empty());
        drop(store);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
