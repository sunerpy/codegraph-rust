//! JS/TS resolution correctness regressions from the post-v1.6 upstream audit.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use codegraph_core::types::{EdgeKind, FileRecord, Language, NodeKind};
use codegraph_extract::extract_file;
use codegraph_resolve::ReferenceResolver;
use codegraph_store::Store;

static NONCE: AtomicU64 = AtomicU64::new(0);

struct Project {
    root: PathBuf,
    store: Option<Store>,
}

impl Drop for Project {
    fn drop(&mut self) {
        drop(self.store.take());
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn language(path: &str) -> Language {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("ts") => Language::TypeScript,
        Some("tsx") => Language::Tsx,
        Some("js") => Language::JavaScript,
        Some("jsx") => Language::Jsx,
        other => panic!("unexpected extension: {other:?}"),
    }
}

fn resolve_project(name: &str, files: &[(&str, &str)]) -> Project {
    let root = std::env::temp_dir().join(format!(
        "codegraph-js-resolution-{name}-{}-{}",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).expect("create fixture root");
    for (relative, source) in files {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture parent");
        }
        std::fs::write(path, source).expect("write fixture");
    }

    let mut store = Store::open(&root.join("graph.db")).expect("open store");
    for (relative, _) in files {
        let result = extract_file(&root, relative).expect("extract fixture");
        assert!(result.errors.is_empty(), "{relative}: {:?}", result.errors);
        store
            .upsert_file(&FileRecord {
                path: (*relative).to_string(),
                content_hash: "fixture".to_string(),
                language: language(relative),
                size: 0,
                modified_at: 0,
                indexed_at: 0,
                node_count: result.nodes.len() as i64,
                errors: Vec::new(),
                generated: false,
            })
            .expect("upsert file");
        store.upsert_nodes(&result.nodes).expect("upsert nodes");
        store.insert_edges(&result.edges).expect("insert edges");
        store
            .insert_unresolved_refs(&result.unresolved_references)
            .expect("insert unresolved refs");
    }

    let mut resolver = ReferenceResolver::new(root.to_string_lossy().to_string());
    resolver.initialize(&codegraph_resolve::StoreResolutionContext::new(
        &store,
        root.to_string_lossy().as_ref(),
    ));
    resolver
        .resolve_and_persist(&mut store)
        .expect("resolve fixture");
    Project {
        root,
        store: Some(store),
    }
}

fn node_id(project: &Project, kind: NodeKind, qualified_name: &str) -> String {
    project
        .store
        .as_ref()
        .expect("store")
        .nodes_by_kind(kind)
        .expect("nodes by kind")
        .into_iter()
        .find(|node| node.qualified_name == qualified_name)
        .unwrap_or_else(|| panic!("missing {kind:?} {qualified_name}"))
        .id
}

fn call_targets(project: &Project, kind: NodeKind, qualified_name: &str) -> Vec<String> {
    let store = project.store.as_ref().expect("store");
    let source = node_id(project, kind, qualified_name);
    let mut targets = store
        .edges_by_source_kind(&source, Some(EdgeKind::Calls))
        .expect("calls")
        .into_iter()
        .filter_map(|edge| store.node_by_id(&edge.target).expect("target query"))
        .map(|node| node.qualified_name)
        .collect::<Vec<_>>();
    targets.sort();
    targets
}

#[test]
fn call_result_receiver_does_not_guess_a_global_callable() {
    let project = resolve_project(
        "call-result",
        &[(
            "collect.js",
            r#"
function append(item) { return item; }
function run() { return 1; }
function make() { return {}; }
function get() { return {}; }
function bucket(d, k, v) {
  d.setdefault(k, []).append(v);
  make().run();
  (0, make)().run();
}
function viaStore() { get().run(); }
module.exports = { append, run, make, get, bucket, viaStore };
"#,
        )],
    );
    assert_eq!(
        call_targets(&project, NodeKind::Function, "bucket"),
        vec!["make"]
    );
    assert_eq!(
        call_targets(&project, NodeKind::Function, "viaStore"),
        vec!["get"],
        "a unique callable name is not proof of get()'s result type"
    );
}

#[test]
fn this_field_calls_resolve_only_through_the_declared_type() {
    let project = resolve_project(
        "this-field",
        &[
            (
                "mailer.ts",
                "export class Mailer { send(msg: string): string { return msg; } }\n",
            ),
            (
                "notifier.ts",
                r#"
import { Mailer } from './mailer';
export class Notifier {
  constructor(private readonly mailer: Mailer, private items: string[]) {}
  send(msg: string): string { return this.mailer.send(msg); }
  other(msg: string): string { return this.mailer.send(msg); }
  push(msg: string): void { this.items.push(msg); }
}
export class NoField {
  fake(shadow: Mailer): string { return this.shadow.send("wrong"); }
  local(): string { const shadow = new Mailer(); return this.shadow.send("wrong"); }
}
"#,
            ),
            (
                "storage.ts",
                r#"
export const DraftStorage = {
  get(key: string): string { return key; },
  settings(): object { return {}; },
};
"#,
            ),
            (
                "keeper.ts",
                r#"
import { DraftStorage } from './storage';
export class Keeper {
  constructor(private readonly storage: typeof DraftStorage) {}
  get(key: string): string { return this.storage.get(key); }
  load(): object { return this.storage.settings(); }
}
"#,
            ),
        ],
    );

    assert_eq!(
        call_targets(&project, NodeKind::Method, "Notifier::send"),
        vec!["Mailer::send"]
    );
    assert_eq!(
        call_targets(&project, NodeKind::Method, "Notifier::other"),
        vec!["Mailer::send"]
    );
    assert!(call_targets(&project, NodeKind::Method, "Notifier::push").is_empty());
    assert!(call_targets(&project, NodeKind::Method, "NoField::fake").is_empty());
    assert!(call_targets(&project, NodeKind::Method, "NoField::local").is_empty());
    assert_eq!(
        call_targets(&project, NodeKind::Method, "Keeper::get"),
        vec!["DraftStorage::get"]
    );
    assert_eq!(
        call_targets(&project, NodeKind::Method, "Keeper::load"),
        vec!["DraftStorage::settings"]
    );
}

#[test]
fn awaited_receiver_uses_the_callees_declared_promise_result() {
    let project = resolve_project(
        "awaited",
        &[
            (
                "engine.ts",
                r#"
export class Engine { run(): string { return "ran"; } }
export class Decoy { run(): string { return "decoy"; } }
export class Pane { split(): string { return "pane"; } }
export class Response { run(): string { return "project-response"; } }
"#,
            ),
            (
                "factory.ts",
                r#"
import { Engine } from './engine';
export async function makeEngine(): Promise<Engine> { return new Engine(); }
export async function listPaths(): Promise<string> { return "a"; }
export async function fetchResponse(): Promise<Response> { return fetch("/") as any; }
"#,
            ),
            (
                "drive.ts",
                r#"
import { makeEngine, listPaths, fetchResponse } from './factory';
export async function drive(): Promise<string> {
  const handle = await makeEngine();
  return handle.run();
}
export async function snapshot(): Promise<string[]> {
  const listed = await listPaths();
  return listed.split("\\0");
}
export async function externalResponse(): Promise<string> {
  const response = await fetchResponse();
  return response.run();
}
"#,
            ),
        ],
    );

    assert_eq!(
        call_targets(&project, NodeKind::Function, "drive"),
        vec!["Engine::run", "makeEngine"]
    );
    assert_eq!(
        call_targets(&project, NodeKind::Function, "snapshot"),
        vec!["listPaths"]
    );
    assert_eq!(
        call_targets(&project, NodeKind::Function, "externalResponse"),
        vec!["fetchResponse"],
        "an unbound DOM Response annotation must not hijack a project Response"
    );
}

#[test]
fn bare_js_call_never_targets_a_method_and_respects_local_bindings() {
    let project = resolve_project(
        "bare-call",
        &[
            (
                "record.ts",
                r#"
function serialize(value: string): string { return value; }
export class Record {
  serialize(): string { return serialize("x"); }
  recurse(): string { return this.serialize(); }
}
export function ping(): Promise<void> {
  return new Promise((resolve) => { resolve(); });
}
export function multiline(): void {
  finish
  ();
}
"#,
            ),
            (
                "other.ts",
                r#"
export function resolve(): void {}
export class Other { serialize(): string { return "wrong"; } finish(): void {} }
"#,
            ),
        ],
    );

    assert_eq!(
        call_targets(&project, NodeKind::Method, "Record::serialize"),
        vec!["serialize"]
    );
    assert_eq!(
        call_targets(&project, NodeKind::Method, "Record::recurse"),
        vec!["Record::serialize"]
    );
    assert!(call_targets(&project, NodeKind::Function, "ping").is_empty());
    assert!(call_targets(&project, NodeKind::Function, "multiline").is_empty());
}

#[test]
fn import_only_js_module_is_not_a_cross_file_name_target() {
    let project = resolve_project(
        "sealed-module",
        &[
            (
                "private.js",
                "import fs from 'node:fs';\nfunction hidden() { return fs; }\nhidden();\n",
            ),
            (
                "consumer.js",
                "function run() { hidden(); globalFn(); helper(); bracketed(); later(); }\n",
            ),
            ("classic.js", "function globalFn() { return 1; }\n"),
            (
                "common.js",
                "import os from 'node:os';\nfunction helper() { return os; }\nmodule.exports = { helper };\n",
            ),
            (
                "bracket.js",
                "import url from 'node:url';\nfunction bracketed() { return url; }\nexports[\"bracketed\"] = bracketed;\n",
            ),
            (
                "later.js",
                "import path from 'node:path';\nfunction later() { return path; }\nexport { later };\n",
            ),
        ],
    );

    assert_eq!(
        call_targets(&project, NodeKind::Function, "run"),
        vec!["bracketed", "globalFn", "helper", "later"]
    );
    assert!(call_targets(&project, NodeKind::Function, "hidden").is_empty());
}

#[test]
fn calls_through_alias_bindings_reach_the_underlying_callable() {
    let project = resolve_project(
        "aliases",
        &[
            (
                "impl.ts",
                r#"
export function realImpl(): number { return 1; }
export const aliasName = realImpl;
export const api = { run: realImpl };
function laterImpl(): number { return 2; }
export { laterImpl as laterAlias };
"#,
            ),
            (
                "consumer.ts",
                r#"
import { aliasName, api, laterAlias } from './impl';
export function viaAlias(): number { return aliasName(); }
export function viaMember(): number { return api.run(); }
export function viaClause(): number { return laterAlias(); }
"#,
            ),
            (
                "local.ts",
                r#"
function localImpl(): number { return 1; }
const localAlias = localImpl;
export function viaLocal(): number { return localAlias(); }
"#,
            ),
        ],
    );

    assert_eq!(
        call_targets(&project, NodeKind::Function, "viaAlias"),
        vec!["realImpl"]
    );
    assert_eq!(
        call_targets(&project, NodeKind::Function, "viaMember"),
        vec!["realImpl"]
    );
    assert_eq!(
        call_targets(&project, NodeKind::Function, "viaClause"),
        vec!["laterImpl"]
    );
    assert_eq!(
        call_targets(&project, NodeKind::Function, "viaLocal"),
        vec!["localImpl"]
    );
}

#[test]
fn wrappers_and_ambiguous_cross_file_alias_targets_do_not_hop() {
    let project = resolve_project(
        "alias-guards",
        &[
            ("one.ts", "export function shared(): number { return 1; }\n"),
            ("two.ts", "export function shared(): number { return 2; }\n"),
            (
                "alias.ts",
                "import { shared } from './one';\nexport const aliasName = shared;\n",
            ),
            (
                "consumer.ts",
                "import { aliasName } from './alias';\nexport function consumer(): number { return aliasName(); }\n",
            ),
            (
                "wrapper.ts",
                "export function realImpl(): number { return 1; }\nexport const wrapper = (): number => realImpl();\n",
            ),
            (
                "use-wrapper.ts",
                "import { wrapper } from './wrapper';\nexport function useWrapper(): number { return wrapper(); }\n",
            ),
        ],
    );

    assert_eq!(
        call_targets(&project, NodeKind::Function, "consumer"),
        vec!["aliasName"]
    );
    assert_eq!(
        call_targets(&project, NodeKind::Function, "useWrapper"),
        vec!["wrapper"]
    );
}

#[test]
fn imported_typescript_interface_method_is_a_real_call_target() {
    let project = resolve_project(
        "interface-method",
        &[
            (
                "api.d.ts",
                "export interface PlatformApi { fetchPage(id: string): Promise<string>; }\n",
            ),
            (
                "consumer.ts",
                "import type { PlatformApi } from './api';\nexport function load(api: PlatformApi) { return api.fetchPage('x'); }\n",
            ),
        ],
    );
    assert_eq!(
        call_targets(&project, NodeKind::Function, "load"),
        vec!["PlatformApi::fetchPage"]
    );
}

#[test]
fn receiver_method_values_resolve_as_references_not_calls() {
    let project = resolve_project(
        "method-values",
        &[
            (
                "mailer.ts",
                "export class Mailer { send(msg: string): string { return msg; } }\n",
            ),
            (
                "consumer.ts",
                r#"
import { Mailer } from './mailer';
export function register(callback: (msg: string) => string): void {}
export class Consumer {
  constructor(private readonly mailer: Mailer) {}
  wire(): void { register(this.mailer.send); }
  wireParam(mailer: Mailer): void { register(mailer.send); }
  dynamic(): void { register(this.unknown.send); }
}
"#,
            ),
        ],
    );
    let store = project.store.as_ref().expect("store");
    let wire = node_id(&project, NodeKind::Method, "Consumer::wire");
    let send = node_id(&project, NodeKind::Method, "Mailer::send");
    let edges = store.edges_by_source_kind(&wire, None).expect("wire edges");
    let callback = edges
        .iter()
        .find(|edge| edge.kind == EdgeKind::References && edge.target == send)
        .unwrap_or_else(|| {
            panic!(
                "receiver-qualified callback reference; edges={edges:#?}; unresolved={:#?}",
                store.all_unresolved_refs().expect("unresolved")
            )
        });
    assert_eq!(
        callback
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("fnRef"))
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        callback
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("resolvedBy"))
            .and_then(serde_json::Value::as_str),
        Some("function-ref")
    );
    assert!(
        !edges
            .iter()
            .any(|edge| edge.kind == EdgeKind::Calls && edge.target == send),
        "passing a method value must not claim an immediate call: {edges:#?}"
    );

    let wire_param = node_id(&project, NodeKind::Method, "Consumer::wireParam");
    let param_edges = store
        .edges_by_source_kind(&wire_param, None)
        .expect("wireParam edges");
    assert!(param_edges.iter().any(|edge| {
        edge.kind == EdgeKind::References
            && edge.target == send
            && edge
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("fnRef"))
                .and_then(serde_json::Value::as_bool)
                == Some(true)
    }));
    assert!(
        !param_edges
            .iter()
            .any(|edge| edge.kind == EdgeKind::Calls && edge.target == send)
    );

    let dynamic = node_id(&project, NodeKind::Method, "Consumer::dynamic");
    assert!(
        store
            .edges_by_source_kind(&dynamic, Some(EdgeKind::References))
            .expect("dynamic refs")
            .into_iter()
            .all(|edge| edge.target != send),
        "an undeclared receiver must remain unresolved"
    );
}

/// `(target kind, target name)` of every `implements` edge out of class `name`.
fn implements_targets(project: &Project, name: &str) -> Vec<(NodeKind, String)> {
    let store = project.store.as_ref().expect("store");
    let source = node_id(project, NodeKind::Class, name);
    store
        .edges_by_source_kind(&source, Some(EdgeKind::Implements))
        .expect("implements")
        .into_iter()
        .filter_map(|edge| store.node_by_id(&edge.target).expect("target query"))
        .map(|node| (node.kind, node.name))
        .collect()
}

#[test]
fn implements_binds_the_interface_of_a_value_and_interface_pair() {
    // VS Code declares every service twice under one name: the DI identifier
    // value and the interface (upstream #2055).
    let value = "export const IFooService = createDecorator<IFooService>('fooService');\n";
    let interface = "export interface IFooService {\n  run(): void;\n}\n";
    for first_is_value in [true, false] {
        let foo = format!(
            "import {{ createDecorator }} from './instantiation';\n\n{}",
            if first_is_value {
                format!("{value}{interface}")
            } else {
                format!("{interface}{value}")
            }
        );
        let project = resolve_project(
            "value-interface-pair",
            &[
                (
                    "src/instantiation.ts",
                    "export function createDecorator<T>(id: string): { id: string } { return { id }; }\n",
                ),
                ("src/foo.ts", &foo),
                (
                    "src/fooService.ts",
                    "import { IFooService } from './foo';\n\nexport class FooService implements IFooService {\n  run(): void {}\n}\n",
                ),
            ],
        );
        assert_eq!(
            implements_targets(&project, "FooService"),
            vec![(NodeKind::Interface, "IFooService".to_string())],
            "value first: {first_is_value}"
        );
    }
}

#[test]
fn implements_whose_only_same_named_target_is_a_value_is_dropped() {
    let project = resolve_project(
        "value-only-implements",
        &[
            ("src/foo.ts", "export const IBarService = { id: 'bar' };\n"),
            (
                "src/barService.ts",
                "import { IBarService } from './foo';\n\nexport class BarService implements IBarService {\n  id = 'bar';\n}\n",
            ),
        ],
    );
    assert!(implements_targets(&project, "BarService").is_empty());
}
