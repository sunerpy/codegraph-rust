//! TS/JS member-call receiver shapes (upstream v1.6.1 #1566/#1683/#1862/#1986/#1987).
//!
//! Ported from upstream `kernel-tsjs-parity.test.ts`,
//! `expression-receiver-calls.test.ts` and `call-receiver-no-fabrication.test.ts`:
//! wrappers that keep the receiver are looked through, identifier-rooted
//! chains keep their qualified source text, an untyped expression receiver
//! emits nothing, and an ES private field keeps its `#`.

use codegraph_core::types::{EdgeKind, ExtractionResult, Language};
use codegraph_extract::extract_source;

const JS_FAMILY: [(&str, Language); 4] = [
    ("ts", Language::TypeScript),
    ("tsx", Language::Tsx),
    ("js", Language::JavaScript),
    ("jsx", Language::Jsx),
];

fn extract(file: &str, source: &str, language: Language) -> ExtractionResult {
    let result = extract_source(file, source, Some(language));
    assert!(
        result.errors.is_empty(),
        "{file}: extraction errors: {:?}",
        result.errors
    );
    result
}

fn calls(result: &ExtractionResult) -> Vec<&str> {
    result
        .unresolved_references
        .iter()
        .filter(|reference| reference.reference_kind == EdgeKind::Calls)
        .map(|reference| reference.reference_name.as_str())
        .collect()
}

fn calls_from<'a>(result: &'a ExtractionResult, function: &str) -> Vec<&'a str> {
    let id = &result
        .nodes
        .iter()
        .find(|node| node.name == function)
        .unwrap_or_else(|| panic!("missing {function}; nodes={:#?}", result.nodes))
        .id;
    result
        .unresolved_references
        .iter()
        .filter(|reference| {
            &reference.from_node_id == id && reference.reference_kind == EdgeKind::Calls
        })
        .map(|reference| reference.reference_name.as_str())
        .collect()
}

#[test]
fn transparent_wrappers_peel_and_untyped_expression_receivers_emit_nothing() {
    for (ext, language) in JS_FAMILY {
        let typed = matches!(language, Language::TypeScript | Language::Tsx);
        let typed_calls = if typed {
            "x!.run(); (y as X).run(); (x satisfies X).stop(); getTarget(\"a\")!.install(); \
             if (x && y!.c.has(1)) {} if (x && this.e!.c.has(1)) {}"
        } else {
            ""
        };
        let source = format!(
            "
function list() {{ return []; }}
class Runner {{ go() {{ return 1; }} }}
async function exprReceivers(x, y) {{
  (await list()).map(g);
  (x).run();
  {typed_calls}
  (a ?? b).map(g);
  arr[0].run();
  f().list.map(g);
  (() => 1).call(null);
  this.a.b.run();
  super.stop();
  new Runner().go();
  window.Api.start();
}}
"
        );
        let result = extract(&format!("fixture.{ext}"), &source, language);
        // KEEP-RUST: a chain rooted at `this` through more than one field keeps
        // its last segment (#1496) — `this.a.b.run()` is `b.run`, and
        // `(x && this.e)!.c.has()` is `c.has` — where upstream emits the bare
        // method name.
        let mut expected = vec!["list().map", "list", "x.run"];
        if typed {
            expected.extend([
                "x.run",
                "y.run",
                "x.stop",
                "getTarget().install",
                "getTarget",
                "c.has",
            ]);
        }
        expected.extend(["f", "b.run", "stop", "go", "start"]);
        assert_eq!(calls_from(&result, "exprReceivers"), expected, "{ext}");
    }
}

#[test]
fn typescript_receiver_wrappers_follow_upstream_order() {
    let source = r#"async function f(x: X, y: any) {
  (await list()).map(g);
  x!.run();
  (y as X).run();
  (x satisfies X).stop();
  getTarget("a")!.install();
  (a ?? b).map(g);
  arr[0].run();
  f().list.map(g);
  (() => 1).call(null);
  this.a.b.run();
  new Runner().go();
  window.Api.start();
  (<Runner>x).halt();
}
"#;
    let result = extract("t.ts", source, Language::TypeScript);
    assert_eq!(
        calls(&result),
        vec![
            "list().map",
            "list",
            "x.run",
            "y.run",
            "x.stop",
            "getTarget().install",
            "getTarget",
            "f",
            "b.run",
            "go",
            "start",
            "x.halt",
        ]
    );
}

#[test]
fn identifier_rooted_chains_keep_qualified_sites_and_argument_calls() {
    for (ext, language) in JS_FAMILY {
        let result = extract(
            &format!("fixture.{ext}"),
            "
function readKey() { return 'answer'; }
function local() {
  const values = new Map();
  return values.get(readKey());
}
function nested(holder) {
  holder.values.get(readKey());
  holder.values?.get(readKey());
  holder['values'].get(readKey());
  holder.deep.values.get(readKey());
}
",
            language,
        );
        // Qualified sites are retained for effects; computed keys still make no
        // receiver claim. All four calls inside arguments must also survive.
        assert_eq!(
            calls_from(&result, "nested"),
            vec![
                "holder.values.get",
                "readKey",
                "holder.values.get",
                "readKey",
                "readKey",
                "holder.deep.values.get",
                "readKey",
            ],
            "{ext}"
        );
        assert!(calls(&result).contains(&"values.get"), "{ext}");
    }
}

#[test]
fn optional_chains_keep_the_same_qualified_site() {
    let result = extract(
        "effects.ts",
        "import { client } from './client';
export function create() { return 1; }
export function effects() {
  client.user.create({ data: {} });
  client?.user?.create({ data: {} });
}
",
        Language::TypeScript,
    );
    assert_eq!(
        calls_from(&result, "effects"),
        vec!["client.user.create", "client.user.create"]
    );
}

#[test]
fn private_field_receivers_keep_their_hash() {
    let source = "
class Mailer { send() {} }
class Vault {
  #mailer = new Mailer();
  #items = new Set();
  notify() { this.#mailer?.send(); }
  optional() { this.#mailer.send?.(); }
  put() { this.#items?.add('x'); }
}
";
    for (ext, language) in JS_FAMILY {
        for (ending, text) in [
            ("LF", source.to_string()),
            ("CRLF", source.replace('\n', "\r\n")),
        ] {
            let result = extract(&format!("vault.{ext}"), &text, language);
            assert_eq!(
                calls(&result),
                vec!["this.#mailer.send", "this.#mailer.send", "this.#items.add"],
                "{ext} {ending}"
            );
        }
    }
}

#[test]
fn call_result_receivers_keep_a_plain_inner_callee() {
    let js = extract(
        "src/x.js",
        "function f(d) { d.setdefault(\"k\", []).append(1); make().run(); (0, make)().run(); arr[0]().go(); }",
        Language::JavaScript,
    );
    let mut names = calls(&js);
    names.sort_unstable();
    // `(0, make)` and `arr[0]` are the inner calls' own refs; their chains are dropped.
    assert_eq!(
        names,
        vec![
            "(0, make)",
            "arr[0]",
            "d.setdefault",
            "d.setdefault().append",
            "make",
            "make().run",
        ]
    );

    let py = extract(
        "x.py",
        "def f(d):\n    d.setdefault(\"k\", []).append(1)\n    d.items().get(2)\n",
        Language::Python,
    );
    let mut names = calls(&py);
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "d.items",
            "d.items().get",
            "d.setdefault",
            "d.setdefault().append",
        ]
    );
}
