//! A member call binds only with receiver evidence (upstream v1.6.1
//! #1683/#1707/#1862/#1986/#1987).
//!
//! Ported from upstream `js-builtin-method-calls.test.ts`,
//! `ts-this-field-call.test.ts`, `ts-chained-receiver.test.ts`,
//! `expression-receiver-calls.test.ts`, `call-receiver-no-fabrication.test.ts`
//! and `release-main-regressions.test.ts`. Each case drives real extraction and
//! the full resolution pipeline over files written to a temporary project.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use codegraph_core::types::{EdgeKind, FileRecord, Node, NodeKind};
use codegraph_extract::{detect_language, extract_file};
use codegraph_resolve::ReferenceResolver;
use codegraph_store::Store;

static NONCE: AtomicU64 = AtomicU64::new(0);

const JS_EXTENSIONS: [&str; 4] = ["ts", "tsx", "js", "jsx"];

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

impl Project {
    fn store(&self) -> &Store {
        self.store.as_ref().expect("store")
    }

    /// The function or method whose qualified name is `qualified` (in `file`
    /// when given; its path must end with it).
    fn callable(&self, qualified: &str, file: Option<&str>) -> Node {
        let mut found = [NodeKind::Function, NodeKind::Method]
            .into_iter()
            .flat_map(|kind| self.store().nodes_by_kind(kind).expect("nodes by kind"))
            .filter(|node| {
                node.qualified_name == qualified
                    && file.is_none_or(|file| node.file_path.ends_with(file))
            })
            .collect::<Vec<_>>();
        assert_eq!(found.len(), 1, "{qualified} in {file:?}: {found:#?}");
        found.remove(0)
    }

    /// Sorted qualified names of the distinct `calls` targets of `qualified`
    /// (upstream `getCallees` reports nodes, not call sites).
    fn callees(&self, qualified: &str, file: Option<&str>) -> Vec<String> {
        let source = self.callable(qualified, file);
        let mut names = self
            .store()
            .edges_by_source_kind(&source.id, Some(EdgeKind::Calls))
            .expect("calls")
            .into_iter()
            .map(|edge| {
                self.store()
                    .node_by_id(&edge.target)
                    .expect("target query")
                    .expect("target node")
                    .qualified_name
            })
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        names
    }

    /// Sorted qualified names of the distinct callables with a `calls` edge
    /// into `qualified`.
    fn callers(&self, qualified: &str, file: Option<&str>) -> Vec<String> {
        let target = self.callable(qualified, file);
        let mut names = self
            .store()
            .edges_by_target_kind(&target.id, Some(EdgeKind::Calls))
            .expect("callers")
            .into_iter()
            .map(|edge| {
                self.store()
                    .node_by_id(&edge.source)
                    .expect("source query")
                    .expect("source node")
                    .qualified_name
            })
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        names
    }

    /// Reference names of the `calls` refs still unresolved from `qualified`.
    fn unresolved_calls(&self, qualified: &str, file: Option<&str>) -> Vec<String> {
        let source = self.callable(qualified, file);
        self.store()
            .all_unresolved_refs()
            .expect("unresolved refs")
            .into_iter()
            .filter(|reference| {
                reference.from_node_id == source.id && reference.reference_kind == EdgeKind::Calls
            })
            .map(|reference| reference.reference_name)
            .collect()
    }
}

fn resolve_project(files: &[(String, String)]) -> Project {
    let root = std::env::temp_dir().join(format!(
        "codegraph-receiver-evidence-{}-{}",
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
    let relative = files
        .iter()
        .map(|(relative, _)| relative.clone())
        .collect::<Vec<_>>();
    for path in &relative {
        let result = extract_file(&root, path).expect("extract fixture");
        assert!(result.errors.is_empty(), "{path}: {:?}", result.errors);
        store
            .upsert_file(&FileRecord {
                path: path.clone(),
                content_hash: "fixture".to_string(),
                language: detect_language(path),
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

    let root_str = root.to_string_lossy().to_string();
    let mut resolver = ReferenceResolver::new(root_str.clone());
    resolver.initialize(&codegraph_resolve::StoreResolutionContext::new(
        &store, &root_str,
    ));
    resolver
        .extract_and_persist_frameworks(&mut store, &relative)
        .expect("framework extract");
    resolver
        .resolve_and_persist(&mut store)
        .expect("resolve fixture");
    resolver.run_post_extract(&mut store).expect("post extract");
    Project {
        root,
        store: Some(store),
    }
}

fn files(entries: &[(&str, &str)]) -> Vec<(String, String)> {
    entries
        .iter()
        .map(|(path, source)| (path.to_string(), source.to_string()))
        .collect()
}

// ---- built-in method names need receiver evidence (#1987) ------------------

const BUILTIN_FAMILIES: [(&str, &[&str]); 7] = [
    (
        "array",
        &[
            "map",
            "filter",
            "reduce",
            "forEach",
            "find",
            "findIndex",
            "some",
            "every",
            "push",
            "pop",
            "slice",
            "splice",
            "sort",
            "flatMap",
            "includes",
            "join",
        ],
    ),
    (
        "collection",
        &[
            "get", "set", "has", "add", "delete", "clear", "keys", "values", "entries",
        ],
    ),
    (
        "string",
        &[
            "trim",
            "split",
            "replace",
            "replaceAll",
            "match",
            "matchAll",
            "search",
            "startsWith",
            "endsWith",
            "substring",
            "toLowerCase",
            "toUpperCase",
        ],
    ),
    ("promise", &["then", "catch", "finally"]),
    ("function", &["call", "apply", "bind"]),
    (
        "event",
        &[
            "on",
            "once",
            "off",
            "emit",
            "addListener",
            "removeListener",
            "removeAllListeners",
            "addEventListener",
            "removeEventListener",
            "dispatchEvent",
        ],
    ),
    ("iterator", &["next", "return", "throw"]),
];

#[test]
fn builtin_method_names_on_unknown_receivers_do_not_guess_project_methods() {
    for ext in JS_EXTENSIONS {
        let mut declarations = String::new();
        let mut calls = String::new();
        let mut expected_callers = 0;
        for (family, methods) in BUILTIN_FAMILIES {
            // Receiver and class share a word; that is still no evidence of its type.
            declarations.push_str(&format!("export class {family}Wrapper {{\n"));
            for method in methods {
                declarations.push_str(&format!("  {method}() {{}}\n"));
                calls.push_str(&format!(
                    "export function {family}_{method}({family}) {{ {family}.{method}(); }}\n"
                ));
                expected_callers += 1;
            }
            declarations.push_str("}\n");
        }
        let project = resolve_project(&files(&[
            (&format!("decoys.{ext}"), &declarations),
            (&format!("calls.{ext}"), &calls),
        ]));
        let callers = project
            .store()
            .nodes_by_kind(NodeKind::Function)
            .expect("functions")
            .into_iter()
            .filter(|node| node.file_path == format!("calls.{ext}"))
            .collect::<Vec<_>>();
        assert_eq!(callers.len(), expected_callers, "{ext}");
        for caller in callers {
            assert_eq!(
                project.callees(&caller.qualified_name, Some(&format!("calls.{ext}"))),
                Vec::<String>::new(),
                "{ext}: {}",
                caller.name
            );
        }
    }
}

#[test]
fn every_builtin_method_family_resolves_on_a_constructed_or_typed_receiver() {
    let mut methods = BUILTIN_FAMILIES
        .iter()
        .flat_map(|(_, methods)| methods.iter().copied())
        .collect::<Vec<_>>();
    methods.sort_unstable();
    methods.dedup();
    for ext in JS_EXTENSIONS {
        let typed = matches!(ext, "ts" | "tsx");
        let declared = methods
            .iter()
            .map(|method| format!("  {method}() {{}}\n"))
            .collect::<String>();
        let called = methods
            .iter()
            .map(|method| format!("  receiver.{method}();\n"))
            .collect::<String>();
        let mut source = format!(
            "export class Project {{\n{declared}}}\nexport function useConstructed() {{\n  const receiver = new Project();\n{called}}}\n"
        );
        if typed {
            source.push_str(&format!(
                "export function useTyped(receiver: Project) {{\n{called}}}\n"
            ));
        }
        let project = resolve_project(&files(&[(&format!("project.{ext}"), &source)]));
        let expected = methods
            .iter()
            .map(|method| format!("Project::{method}"))
            .collect::<Vec<_>>();
        let mut expected_sorted = expected.clone();
        expected_sorted.sort();
        let callers: &[&str] = if typed {
            &["useConstructed", "useTyped"]
        } else {
            &["useConstructed"]
        };
        for caller in callers {
            assert_eq!(
                project.callees(caller, None),
                expected_sorted,
                "{ext}: {caller}"
            );
        }
    }
}

#[test]
fn builtin_values_stay_unresolved_while_real_receivers_resolve() {
    for ext in JS_EXTENSIONS {
        let typed = matches!(ext, "ts" | "tsx");
        let list_annotation = if typed { ": string[]" } else { "" };
        let typed_call = if typed {
            "export function typed(cart: Cart) { cart.map(); }\n"
        } else {
            ""
        };
        let project = resolve_project(&files(&[
            (
                &format!("cart.{ext}"),
                "
export class Cart {
  map() {}
  add() {}
  get() {}
  static bind() {}
  self() { this.add(); }
}
export const api = { map() {} };
export class LRUCache { get() {} }
",
            ),
            (
                &format!("calls.{ext}"),
                &format!(
                    "
import {{ Cart, api }} from './cart';
export function tidy(list{list_annotation}) {{ list.map(); }}
export function cached() {{
  const cache = new Map();
  cache.get('x');
}}
export function unknownCache(cache) {{ cache.get('x'); }}
export function literalArray() {{ const cart = []; cart.map(); }}
export function literalString() {{ const cart = 'x'; cart.trim(); }}
export function constructed() {{
  const cart = new Cart();
  cart.add();
}}
export function imported() {{ Cart.bind(); }}
export function objectLiteral() {{ api.map(); }}
export const local = {{ map() {{}} }};
export function localLiteral() {{ local.map(); }}
{typed_call}"
                ),
            ),
        ]));
        let calls = format!("calls.{ext}");
        for name in [
            "tidy",
            "cached",
            "unknownCache",
            "literalArray",
            "literalString",
        ] {
            assert_eq!(
                project.callees(name, Some(&calls)),
                Vec::<String>::new(),
                "{ext}: {name}"
            );
        }
        assert!(
            project
                .callees("constructed", Some(&calls))
                .contains(&"Cart::add".to_string()),
            "{ext}"
        );
        assert!(
            project
                .callees("imported", Some(&calls))
                .contains(&"Cart::bind".to_string()),
            "{ext}"
        );
        // Object-literal members are qualified by their holder here
        // (upstream keeps the bare member name).
        assert_eq!(
            project.callees("objectLiteral", Some(&calls)),
            vec!["api::map"],
            "{ext}"
        );
        assert_eq!(
            project.callees("localLiteral", Some(&calls)),
            vec!["local::map"],
            "{ext}"
        );
        if typed {
            assert_eq!(
                project.callees("typed", Some(&calls)),
                vec!["Cart::map"],
                "{ext}"
            );
        }
        assert_eq!(
            project.callees("Cart::self", None),
            vec!["Cart::add"],
            "{ext}"
        );
    }
}

// ---- this.<field> and this.#field (#1496, #1987) ---------------------------

fn this_field_project() -> Project {
    resolve_project(&files(&[
        (
            "src/mailer.ts",
            "export class Mailer {\n  send(msg: string): string { return msg; }\n}\n",
        ),
        (
            "src/notifier.ts",
            "import { Mailer } from './mailer';\n\
             export class Notifier {\n  \
             constructor(private readonly mailer: Mailer, private items: string[]) {}\n  \
             send(msg: string): string { return this.mailer.send(msg); }\n  \
             other(msg: string): string { return this.mailer.send(msg); }\n  \
             push(msg: string): void { this.items.push(msg); }\n\
             }\n",
        ),
        (
            "src/legacy-mailer.js",
            "class LegacyMailer {\n  send(msg) { return msg; }\n}\nmodule.exports = { LegacyMailer };\n",
        ),
        (
            "src/legacy.js",
            "const { LegacyMailer } = require('./legacy-mailer');\n\
             class LegacyNotifier {\n  \
             constructor() { this.mailer = new LegacyMailer(); }\n  \
             send(msg) { return this.mailer.send(msg); }\n\
             }\n\
             module.exports = { LegacyNotifier };\n",
        ),
        (
            "src/storage.ts",
            "export const DraftHubStorage = {\n  \
             async get(key: string): Promise<string> { return key; },\n  \
             async getSettings(): Promise<object> { return {}; },\n\
             };\n",
        ),
        (
            "src/keeper.ts",
            "import { DraftHubStorage } from './storage';\n\
             export class Keeper {\n  \
             constructor(private readonly storage: typeof DraftHubStorage) {}\n  \
             async get(key: string): Promise<string> { return this.storage.get(key); }\n  \
             async settings(): Promise<object> { return this.storage.getSettings(); }\n\
             }\n",
        ),
        // `Outbox::send` and `Cart::add` sit in the same file, so a bare-name
        // guess would pick them over the field's real type.
        (
            "src/vault.ts",
            "import { Mailer } from './mailer';\n\
             export class Outbox {\n  send(msg: string): string { return msg; }\n}\n\
             export class Cart {\n  add(item: string): void {}\n}\n\
             export class Vault {\n  \
             #mailer: Mailer;\n  \
             #backup = new Mailer();\n  \
             #items = new Set<string>();\n  \
             constructor(m: Mailer) { this.#mailer = m; }\n  \
             notify(msg: string): string { return this.#mailer.send(msg); }\n  \
             fallback(msg: string): string { return this.#backup.send(msg); }\n  \
             put(x: string): void { this.#items.add(x); }\n\
             }\n",
        ),
    ]))
}

#[test]
fn this_fields_resolve_on_their_declared_or_constructed_type() {
    let project = this_field_project();
    assert_eq!(
        project.callees("Notifier::send", None),
        vec!["Mailer::send"]
    );
    assert_eq!(
        project.callees("Notifier::other", None),
        vec!["Mailer::send"]
    );
    assert!(
        !project
            .callers("Notifier::send", None)
            .contains(&"Notifier::send".to_string())
    );
    assert_eq!(
        project.callees("LegacyNotifier::send", None),
        vec!["LegacyMailer::send"]
    );
    assert_eq!(
        project.callees("Notifier::push", None),
        Vec::<String>::new()
    );
    assert_eq!(
        project.callees("Keeper::settings", None),
        vec!["DraftHubStorage::getSettings"]
    );
    assert_eq!(
        project.callees("Keeper::get", None),
        vec!["DraftHubStorage::get"]
    );
    assert!(
        !project
            .callers("Keeper::get", None)
            .contains(&"Keeper::get".to_string())
    );
}

#[test]
fn private_fields_resolve_on_their_type_and_builtin_fields_stay_unresolved() {
    let project = this_field_project();
    assert_eq!(project.callees("Vault::notify", None), vec!["Mailer::send"]);
    assert_eq!(
        project.callees("Vault::fallback", None),
        vec!["Mailer::send"]
    );
    // `this.#items.add()` on a Set must not bind to the project's `Cart::add`.
    assert_eq!(project.callees("Vault::put", None), Vec::<String>::new());
}

#[test]
fn private_and_public_fields_stay_distinct_in_every_variant() {
    let source = "
export class Mailer { send() {} }
export class Cart { add() {} }
export class Vault {
  #mailer = new Mailer();
  #items = new Set();
  items = new Cart();
  notify() { this.#mailer?.send(); }
  optional() { this.#mailer.send?.(); }
  put() { this.#items?.add('x'); }
  publicPut() { this.items.add('x'); }
}
";
    for ext in JS_EXTENSIONS {
        for (ending, text) in [
            ("LF", source.to_string()),
            ("CRLF", source.replace('\n', "\r\n")),
        ] {
            let project = resolve_project(&files(&[(&format!("vault.{ext}"), &text)]));
            let label = format!("{ext} {ending}");
            assert_eq!(
                project.callees("Vault::notify", None),
                vec!["Mailer::send"],
                "{label}"
            );
            assert_eq!(
                project.callees("Vault::optional", None),
                vec!["Mailer::send"],
                "{label}"
            );
            assert_eq!(
                project.callees("Vault::put", None),
                Vec::<String>::new(),
                "{label}"
            );
            assert_eq!(
                project.callees("Vault::publicPut", None),
                vec!["Cart::add"],
                "{label}"
            );
        }
    }
}

// ---- host-global and untyped chains (#1707, #1566, #1862) ------------------

#[test]
fn host_chains_stay_unresolved_while_project_rooted_chains_resolve() {
    let project = resolve_project(&files(&[
        (
            "storage.ts",
            "declare const chrome: any;\n\
             export const DraftHubStorage = {\n  \
             async get(key: string): Promise<unknown> {\n    \
             const result = await chrome.storage.local.get([key]);\n    \
             return result[key];\n  \
             },\n\
             };\n",
        ),
        (
            "dom.ts",
            "export function querySelector(sel: string): string { return sel; }\n\
             export function findRow(): unknown {\n  \
             return document.body.querySelector(\"tr\");\n\
             }\n",
        ),
        (
            "service.ts",
            "declare const window: any;\n\
             export function ping(): string { return \"pong\"; }\n\
             export function viaGlobal(): string {\n  \
             return window.MyNs.ping();\n\
             }\n\
             export class PingService { ping(): string { return \"service\"; } }\n\
             export class Runner {\n  \
             constructor(private svc: PingService) {}\n  \
             run(): string { return this.svc.ping(); }\n\
             }\n\
             export class AnonymousRunner {\n  \
             constructor(private svc: { ping(): string }) {}\n  \
             run(): string { return this.svc.ping(); }\n\
             }\n",
        ),
    ]));
    assert!(
        !project
            .callees("DraftHubStorage::get", Some("storage.ts"))
            .contains(&"DraftHubStorage::get".to_string())
    );
    assert!(
        !project
            .callees("findRow", Some("dom.ts"))
            .contains(&"querySelector".to_string())
    );
    assert!(
        project
            .callees("viaGlobal", Some("service.ts"))
            .contains(&"ping".to_string())
    );
    assert_eq!(
        project.callees("Runner::run", None),
        vec!["PingService::ping"]
    );
    assert_eq!(
        project.callees("AnonymousRunner::run", None),
        Vec::<String>::new()
    );
}

#[test]
fn qualified_call_sites_stay_as_unresolved_evidence() {
    let project = resolve_project(&files(&[
        (
            "effects.ts",
            "import { client } from './client';\n\
             export function create() { return 1; }\n\
             export function effects() {\n  \
             client.user.create({ data: {} });\n  \
             client?.user?.create({ data: {} });\n\
             }\n",
        ),
        ("client.ts", "export const client = {};\n"),
    ]));
    assert_eq!(project.callees("effects", None), Vec::<String>::new());
    assert_eq!(
        project.unresolved_calls("effects", None),
        vec!["client.user.create", "client.user.create"]
    );
}

#[test]
fn this_fields_resolve_across_sibling_js_and_ts_extensions() {
    for (name, target_ext, caller_ext) in [
        ("Reverse", "tsx", "ts"),
        ("Legacy", "js", "jsx"),
        ("LegacyReverse", "jsx", "js"),
    ] {
        let project = resolve_project(&files(&[
            (
                &format!("{name}.{target_ext}"),
                &format!("export class {name} {{ send() {{ return 1; }} }}\n"),
            ),
            (
                &format!("{name}Caller.{caller_ext}"),
                &format!(
                    "import {{ {name} }} from './{name}';\n\
                     export class {name}Caller {{\n  \
                     service = new {name}();\n  \
                     send() {{ return this.service.send(); }}\n\
                     }}\n"
                ),
            ),
        ]));
        assert_eq!(
            project.callees(&format!("{name}Caller::send"), None),
            vec![format!("{name}::send")],
            "{name}"
        );
    }
}

// ---- expression receivers (#1986) ------------------------------------------

#[test]
fn expression_receivers_never_bind_by_bare_name() {
    let project = resolve_project(&files(&[
        (
            "src/adapter.ts",
            "export class GraphAdapter { async map(req?: string) { return req ?? ''; } }\n\
             export class Runner { run() { return 1; } }\n\
             export class Base { hello() { return 1; } }\n",
        ),
        (
            "src/use.ts",
            "import { GraphAdapter, Runner, Base } from './adapter';\n\
             import { helper } from './helper.js';\n\
             async function list(): Promise<string[]> { return []; }\n\
             export async function names() {\n  \
             const a = (await list()).map((d) => d.length);\n  \
             const b = [1, 2].map((x) => x + 1);\n  \
             const c = (a ?? []).map((x) => x);\n  \
             return [a, b, c];\n\
             }\n\
             export function typed(g: GraphAdapter | undefined) {\n  \
             return g!.map();\n\
             }\n\
             export function fresh() { return new Runner().run() + helper(); }\n\
             export class Child extends Base {\n  \
             greet() { return this.hello() + super.hello(); }\n\
             }\n",
        ),
        ("src/helper.js", "export function helper() { return 2; }\n"),
        (
            "k/src/lib.rs",
            "pub fn lens(v: Vec<String>) -> Vec<usize> { v.iter().map(|s| s.len()).collect() }\n\
             pub fn opt(o: Option<u8>) -> Option<u16> { o.map(|x| x as u16) }\n",
        ),
        (
            "export_ios.go",
            "package main\n\nimport \"C\"\n\n//export OpenFluxStop\nfunc OpenFluxStop() {}\n",
        ),
        (
            "ios/Tunnel.swift",
            "func stopTunnel() {\n  OpenFluxStop()\n}\n",
        ),
        ("web/min.js", "function i(a) { return a; }\n"),
        (
            "android/Diag.kt",
            "fun report() {\n  android.util.Log.i(\"tag\", \"msg\")\n}\n",
        ),
    ]));
    assert_eq!(project.callees("names", Some("use.ts")), vec!["list"]);
    assert_eq!(
        project.callers("GraphAdapter::map", None),
        vec!["typed".to_string()]
    );
    assert_eq!(
        project.callees("lens", Some("lib.rs")),
        Vec::<String>::new()
    );
    assert_eq!(project.callees("opt", Some("lib.rs")), Vec::<String>::new());
    assert_eq!(
        project.callees("typed", Some("use.ts")),
        vec!["GraphAdapter::map"]
    );
    assert_eq!(
        project.callees("fresh", Some("use.ts")),
        vec!["Runner::run", "helper"]
    );
    assert_eq!(project.callees("Child::greet", None), vec!["Base::hello"]);
    assert_eq!(
        project.callees("report", Some("Diag.kt")),
        Vec::<String>::new()
    );
    assert_eq!(
        project.callees("stopTunnel", Some("Tunnel.swift")),
        vec!["OpenFluxStop"]
    );
}

// ---- call-result receivers (#1683) -----------------------------------------

#[test]
fn call_result_receivers_never_fabricate_an_edge() {
    let project = resolve_project(&files(&[
        ("py/__init__.py", ""),
        (
            "py/collect.py",
            "def append(item):\n    return item\n\n\
             def get(key):\n    return key\n\n\
             def make():\n    return {}\n\n\
             def bucket(d, k, v):\n    d.setdefault(k, []).append(v)\n    return d.items().get(k)\n\n\
             def fresh():\n    return make().get(\"x\")\n",
        ),
        (
            "js/collect.js",
            "function append(item) { return item; }\n\
             function run() { return 1; }\n\
             function make() { return {}; }\n\
             function bucket(d, k, v) { d.setdefault(k, []).append(v); make().run(); (0, make)().run(); }\n\
             module.exports = { append, run, make, bucket };\n",
        ),
    ]));
    let py = Some("collect.py");
    assert_eq!(project.callees("bucket", py), Vec::<String>::new());
    assert_eq!(project.callers("append", py), Vec::<String>::new());
    assert_eq!(project.callers("get", py), Vec::<String>::new());
    // The inner call still resolves on its own; `.get` on its unknown product does not.
    assert_eq!(project.callees("fresh", py), vec!["make"]);

    let js = Some("collect.js");
    assert_eq!(project.callers("append", js), Vec::<String>::new());
    // `make().run()` — what `make` returns is unknown, so `run` is not guessed.
    assert_eq!(project.callers("run", js), Vec::<String>::new());
    assert_eq!(project.callees("bucket", js), vec!["make"]);
}
