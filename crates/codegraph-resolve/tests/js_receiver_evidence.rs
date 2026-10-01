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

// ---- store actions (#647, #1862) --------------------------------------------

fn store_project() -> Project {
    resolve_project(&files(&[
        (
            "store.ts",
            "import { create } from 'zustand';
interface S { fetchUser(): Promise<void>; reset(): void }
export const useStore = create<S>((set, get, api) => ({
  fetchUser: async () => { get().reset(); },
  reset: () => set({}),
}));
export const anotherStore = create((set, get) => ({
  reset: () => set({}),
}));
",
        ),
        (
            "consumer.ts",
            "import { useStore as current, anotherStore } from './store';
function fetchUser() { return 'local'; }
export async function loginFlow() {
  const { fetchUser } = current.getState();
  await fetchUser();
}
export function hardReset() { current.getState().reset(); }
export function multipleBindings() {
  const { fetchUser, reset } = current.getState();
  fetchUser(); reset();
}
export function otherReset() { anotherStore.getState().reset(); }
export function shadowed() {
  const { fetchUser } = current.getState();
  { const fetchUser = () => 'shadow'; fetchUser(); }
}
export function siblingScope(flag: boolean) {
  if (flag) { const { fetchUser } = current.getState(); }
  return fetchUser();
}
export function unknownStore(unknown: any) { unknown.getState().reset(); }
export function unknownFactory(db: any) { db.prepare().reset(); }
",
        ),
        (
            "not-a-store.ts",
            "export const fake = otherFactory(() => ({ reset() { return 1; } }));\n",
        ),
        (
            "barrel.ts",
            "export { useStore as routedStore } from './store';\n",
        ),
        (
            "barrel-consumer.ts",
            "import { routedStore as current } from './barrel';
export function barrelReset() { current.getState().reset(); }
export function barrelSelected() { const selected = current(s => s.reset); selected(); }
",
        ),
        (
            "selectors.ts",
            "import { useStore as current, anotherStore } from './store';
import { fake } from './not-a-store';
export function rootShadow(current: any) { const selected = current(s => s.reset); selected(); }
export function rootBlockShadow() { const current = fake; const selected = current(s => s.reset); selected(); }
export function fakeSelector() { const selected = fake(s => s.reset); selected(); }
export function Screen() {
  const selected = current((s) => s.reset);
  const otherSelected = anotherStore(s => s.reset);
  function captured() { selected(); }
  function otherCaptured() { otherSelected(); }
  function parameterShadow(selected: () => void) { selected(); }
  const arrowShadow = (selected: () => void) => { selected(); };
  function localShadow() { const selected = () => 1; selected(); }
  return { captured, otherCaptured, parameterShadow, arrowShadow, localShadow };
}
export function sibling() { const selected = current(s => s.reset); }
export function outside() { selected(); }
export function wrongSelector(other: any) {
  const selected = current(s => other.reset);
  selected();
}
export function unknownSelector(unknown: any) {
  const selected = unknown(s => s.reset);
  selected();
}
",
        ),
    ]))
}

const USE_STORE_RESET: &str = "useStore::reset";
const OTHER_STORE_RESET: &str = "anotherStore::reset";

#[test]
fn store_initializers_own_their_inline_actions() {
    let project = store_project();
    let store_actions = |file: &str| {
        let mut names = project
            .store()
            .nodes_by_kind(NodeKind::Function)
            .expect("functions")
            .into_iter()
            .filter(|node| node.file_path == file)
            .map(|node| (node.qualified_name, node.start_line))
            .collect::<Vec<_>>();
        names.sort();
        names
    };
    assert_eq!(
        store_actions("store.ts"),
        vec![
            ("anotherStore::reset".to_string(), 8),
            ("useStore::fetchUser".to_string(), 4),
            ("useStore::reset".to_string(), 5),
        ]
    );
    assert_eq!(
        store_actions("not-a-store.ts"),
        vec![("fake::reset".to_string(), 1)]
    );
}

#[test]
fn store_accessors_resolve_inside_the_identified_store() {
    let project = store_project();
    assert!(
        project
            .callees("useStore::fetchUser", None)
            .contains(&USE_STORE_RESET.to_string())
    );
    let hard = project.callees("hardReset", None);
    assert!(hard.contains(&USE_STORE_RESET.to_string()), "{hard:?}");
    assert!(!hard.contains(&OTHER_STORE_RESET.to_string()), "{hard:?}");
    let other = project.callees("otherReset", None);
    assert!(other.contains(&OTHER_STORE_RESET.to_string()), "{other:?}");
    assert!(!other.contains(&USE_STORE_RESET.to_string()), "{other:?}");
    for name in ["unknownStore", "unknownFactory"] {
        let callees = project.callees(name, None);
        assert!(
            !callees
                .iter()
                .any(|callee| callee.starts_with("useStore::")
                    || callee.starts_with("anotherStore::")),
            "{name}: {callees:?}"
        );
    }
}

#[test]
fn destructured_store_actions_resolve_ahead_of_same_named_locals() {
    let project = store_project();
    let login = project.callees("loginFlow", None);
    assert!(
        login.contains(&"useStore::fetchUser".to_string()),
        "{login:?}"
    );
    assert!(!login.contains(&"fetchUser".to_string()), "{login:?}");
    let multiple = project.callees("multipleBindings", None);
    assert!(
        multiple.contains(&"useStore::fetchUser".to_string()),
        "{multiple:?}"
    );
    assert!(
        multiple.contains(&USE_STORE_RESET.to_string()),
        "{multiple:?}"
    );
    for name in ["shadowed", "siblingScope"] {
        let callees = project.callees(name, None);
        assert!(
            !callees.contains(&"useStore::fetchUser".to_string()),
            "{name}: {callees:?}"
        );
    }
}

#[test]
fn selectors_follow_their_own_store_through_closures_and_barrels() {
    let project = store_project();
    assert_eq!(
        project.callees("Screen::captured", None),
        vec![USE_STORE_RESET]
    );
    assert_eq!(
        project.callees("Screen::otherCaptured", None),
        vec![OTHER_STORE_RESET]
    );
    for name in ["barrelReset", "barrelSelected"] {
        let callees = project.callees(name, Some("barrel-consumer.ts"));
        assert!(
            callees.contains(&USE_STORE_RESET.to_string()),
            "{name}: {callees:?}"
        );
        // The imported store itself may also be referenced by the accessor or
        // hook call; the other store's same-named action must not leak in.
        assert!(
            callees
                .iter()
                .all(|callee| callee == USE_STORE_RESET || callee == "useStore"),
            "{name}: {callees:?}"
        );
    }
}

#[test]
fn selectors_are_not_guessed_through_shadows_or_foreign_factories() {
    let project = store_project();
    for name in [
        "Screen::parameterShadow",
        "Screen::arrowShadow",
        "Screen::localShadow",
        "outside",
        "wrongSelector",
        "unknownSelector",
        "rootShadow",
        "rootBlockShadow",
    ] {
        let callees = project.callees(name, Some("selectors.ts"));
        assert!(
            !callees.contains(&USE_STORE_RESET.to_string())
                && !callees.contains(&OTHER_STORE_RESET.to_string()),
            "{name}: {callees:?}"
        );
    }
    let fake = project.callees("fakeSelector", Some("selectors.ts"));
    assert!(!fake.contains(&"fake::reset".to_string()), "{fake:?}");
}

// ---- object-literal members that alias a function (#1932) ------------------

#[test]
fn literal_members_follow_shorthand_and_identifier_bindings() {
    let project = resolve_project(&files(&[
        (
            "a.ts",
            "import { imported } from './d';
const viaArrow = async () => 1;
function viaDecl() { return 2; }
const longForm = async () => 3;
function renamed() { return 4; }

export const api = {
  inline() { return 0; },
  viaArrow,
  viaDecl,
  longForm: longForm,
  alias: renamed,
  imported,
};

export function sameFileCaller() {
  return [api.inline(), api.viaArrow(), api.viaDecl(), api.longForm(), api.alias(), api.imported()];
}
",
        ),
        (
            "b.ts",
            "import { api } from './a';
export function crossFileCaller() {
  return [api.viaArrow(), api.viaDecl(), api.longForm(), api.alias()];
}
",
        ),
        ("d.ts", "export function imported() { return 5; }\n"),
        (
            "e.ts",
            "function frozenFn() { return 6; }\nexport const frozen = Object.freeze({ frozenFn });\n",
        ),
        (
            "f.ts",
            "import { frozen } from './e';\nexport function frozenCaller() { return frozen.frozenFn(); }\n",
        ),
    ]));
    // A literal handed straight to a wrapper still names its members.
    assert_eq!(
        project.callers("frozenFn", Some("e.ts")),
        vec!["frozenCaller"]
    );
    assert_eq!(
        project.callers("api::inline", Some("a.ts")),
        vec!["sameFileCaller"]
    );
    for function in ["viaArrow", "viaDecl", "longForm", "renamed"] {
        assert_eq!(
            project.callers(function, Some("a.ts")),
            vec!["crossFileCaller", "sameFileCaller"],
            "{function}"
        );
    }
    assert_eq!(
        project.callers("imported", Some("d.ts")),
        vec!["sameFileCaller"]
    );
}

#[test]
fn literal_members_never_follow_keys_nested_objects_or_shadows() {
    let project = resolve_project(&files(&[(
        "c.ts",
        "function keyOnly() { return 1; }
function nested() { return 2; }
function shadowed() { return 3; }

export const other = { keyOnly: 1, box: { nested } };

export function useOther() {
  return [other.keyOnly(), other.nested()];
}

export function makeApi(shadowed: () => number) {
  const local = { shadowed };
  return local.shadowed();
}
",
    )]));
    for function in ["keyOnly", "nested", "shadowed"] {
        assert_eq!(
            project.callers(function, Some("c.ts")),
            Vec::<String>::new(),
            "{function}"
        );
    }
}

#[test]
fn literal_member_boundaries_pick_the_last_own_property() {
    let cases: [(&str, &str, &str, bool); 14] = [
        (
            "sibling literals",
            "const first = { wrong }; export const api = { run: right };",
            "run",
            true,
        ),
        (
            "unicode before literal",
            "const label = 'é🙂'; export const api = { run: right };",
            "run",
            true,
        ),
        (
            "absent sibling member",
            "const first = { wrong }; export const api = { right };",
            "wrong",
            false,
        ),
        (
            "duplicate key",
            "export const api = { run: wrong, run: right };",
            "run",
            true,
        ),
        (
            "non-callable overwrite",
            "export const api = { wrong, wrong: 0 };",
            "wrong",
            false,
        ),
        (
            "nested member",
            "export const api = { box: { wrong }, method() { return { wrong }; } };",
            "wrong",
            false,
        ),
        (
            "unknown spread",
            "export const api = { wrong, ...unknown };",
            "wrong",
            false,
        ),
        (
            "explicit after spread",
            "export const api = { ...unknown, run: right };",
            "run",
            true,
        ),
        (
            "quoted overwrite",
            "export const api = { wrong, 'wrong': 0 };",
            "wrong",
            false,
        ),
        (
            "computed overwrite",
            "export const api = { wrong, [unknown]: 0 };",
            "wrong",
            false,
        ),
        (
            "frozen literal",
            "export const api = Object.freeze({ run: right });",
            "run",
            true,
        ),
        (
            "parenthesized literal",
            "export const api = ({ run: right });",
            "run",
            true,
        ),
        (
            "inline overwrite",
            "export const api = { run() { return 0; }, run: right };",
            "run",
            true,
        ),
        (
            "nested inline",
            "export const api = { box: { wrong() { return 0; } } };",
            "wrong",
            false,
        ),
    ];
    for ext in ["ts", "js"] {
        for (name, declaration, member, resolves) in cases {
            let project = resolve_project(&files(&[
                (
                    &format!("impl.{ext}"),
                    &format!(
                        "function wrong() {{ return 1; }}
function right() {{ return 2; }}
{declaration}
export function sameCaller() {{ return api.{member}(); }}
"
                    ),
                ),
                (
                    &format!("consumer.{ext}"),
                    &format!(
                        "import {{ api }} from './impl';
export function crossCaller() {{ return api.{member}(); }}
"
                    ),
                ),
            ]));
            let label = format!("{ext}: {name}");
            let impl_file = format!("impl.{ext}");
            let wrong = project.callers("wrong", Some(&impl_file));
            let right = project.callers("right", Some(&impl_file));
            for caller in ["sameCaller", "crossCaller"] {
                assert!(!wrong.contains(&caller.to_string()), "{label}: {wrong:?}");
                assert_eq!(
                    right.contains(&caller.to_string()),
                    resolves,
                    "{label}: {caller} in {right:?}"
                );
            }
            for inline in project
                .store()
                .nodes_by_kind(NodeKind::Function)
                .expect("functions")
                .into_iter()
                .filter(|node| {
                    node.name == member
                        && !matches!(node.qualified_name.as_str(), "right" | "wrong")
                })
            {
                let callers = project.callers(&inline.qualified_name, Some(&inline.file_path));
                assert!(
                    !callers
                        .iter()
                        .any(|caller| caller == "sameCaller" || caller == "crossCaller"),
                    "{label}: {} called by {callers:?}",
                    inline.qualified_name
                );
            }
        }
    }
}

#[test]
fn literal_bindings_do_not_cross_parameter_or_value_shadows() {
    let project = resolve_project(&files(&[(
        "impl.ts",
        "function target() { return 0; }
export function parameter(target: () => number) {
  const api = { target };
  return api.target();
}
export function value() {
  const target = 0;
  const api = { target };
  return api.target();
}
export function later() {
  const api = { target };
  const target = 0;
  return api.target();
}
",
    )]));
    let callers = project.callers("target", Some("impl.ts"));
    for name in ["parameter", "value", "later"] {
        assert!(!callers.contains(&name.to_string()), "{name}: {callers:?}");
    }
}

#[test]
fn literal_bindings_follow_renamed_imports_and_ignore_nested_namesakes() {
    let project = resolve_project(&files(&[
        ("target.ts", "export function actual() { return 1; }\n"),
        (
            "impl.ts",
            "import { actual as renamed } from './target';
function unrelated() { function renamed() { return 0; } return renamed(); }
export const api = { run: renamed };
export function sameCaller() { return api.run(); }
",
        ),
        (
            "consumer.ts",
            "import { api as facade } from './impl';
export function crossCaller() { return facade.run(); }
",
        ),
    ]));
    let actual = project.callers("actual", Some("target.ts"));
    assert!(actual.contains(&"sameCaller".to_string()), "{actual:?}");
    assert!(actual.contains(&"crossCaller".to_string()), "{actual:?}");
    let nested = project.callers("unrelated::renamed", Some("impl.ts"));
    assert!(
        !nested
            .iter()
            .any(|caller| caller == "sameCaller" || caller == "crossCaller"),
        "{nested:?}"
    );
}

// ---- default-export namespace objects (F13) --------------------------------

#[test]
fn a_default_exported_namespace_object_resolves_its_members() {
    let project = resolve_project(&files(&[
        ("package.json", "{\"name\":\"app\"}"),
        (
            "src/api/frames.ts",
            "export async function uploadARCapture(uri: string) {\n  return uri\n}\n",
        ),
        (
            "src/api/folders.ts",
            "export function createFolder(name: string) {\n  return name\n}\n",
        ),
        (
            "src/api/index.ts",
            "import { uploadARCapture } from './frames'\n\
             import { createFolder } from './folders'\n\
             function localHelper() {\n  return 1\n}\n\
             const UploadApi = {\n  uploadARCapture,\n  makeFolder: createFolder,\n  localHelper,\n}\n\
             export default UploadApi\n",
        ),
        (
            "src/hooks.ts",
            "import UploadApi from './api'\n\
             export function handleZipComplete(uri: string) {\n\
             \x20 UploadApi.makeFolder(uri)\n\
             \x20 UploadApi.localHelper()\n\
             \x20 return UploadApi.uploadARCapture(uri)\n\
             }\n",
        ),
    ]));
    assert_eq!(
        project.callees("handleZipComplete", None),
        vec!["createFolder", "localHelper", "uploadARCapture"]
    );
}

#[test]
fn a_default_import_of_a_later_exported_const_finds_that_const() {
    let project = resolve_project(&files(&[
        ("package.json", "{\"name\":\"app\"}"),
        (
            "src/store.ts",
            "const useStore = {\n  read() {\n    return 1\n  },\n}\n\
             export function unrelated() {\n  return 2\n}\n\
             export default useStore\n",
        ),
        (
            "src/use.ts",
            "import store from './store'\nexport function consume() {\n  return store.read()\n}\n",
        ),
    ]));
    // Without the `export default NAME` binding the default import guessed the
    // first exported function (`unrelated`).
    assert_eq!(project.callees("consume", None), vec!["useStore::read"]);
}
