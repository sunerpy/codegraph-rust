//! Every Go `var` and `const` spec is a node (W7-15d0, upstream #2680,
//! #2682): a `var ( … )` group's members inside its `var_spec_list` too,
//! each with its own doc comment or the declaration's, its initializer walked
//! under it (#693), and a function value in that initializer referenced once,
//! by the spec (#2743). The source of the first test is upstream's own
//! `go-var-groups.test.ts`.

use codegraph_core::types::{EdgeKind, ExtractionResult, Language, NodeKind};
use codegraph_extract::extract_source;

const SHAPES_GO: &str = "// Package shapes is documented.
package shapes

import (
\t\"errors\"
\t\"fmt\"
)

// Errors returned by the shapes package.
var (
\t// ErrNoSides is returned for a shape without sides.
\tErrNoSides = errors.New(\"no sides\")
\tErrTooMany = newError(\"too many sides\")
)

var (
\tregistry = map[string]int{}
\t// DefaultName is documented inside its group.
\tDefaultName string
)

// single is documented above its own declaration.
var single = 3

func newError(msg string) error { return fmt.Errorf(\"shapes: %s\", msg) }

func check(n int) error {
\tif n == 0 {
\t\treturn ErrNoSides
\t}
\treturn ErrTooMany
}
";

/// The id of the one node of `kind` named `name`.
#[track_caller]
fn id_of(result: &ExtractionResult, kind: NodeKind, name: &str) -> String {
    let found: Vec<_> = result
        .nodes
        .iter()
        .filter(|node| node.kind == kind && node.name == name)
        .collect();
    assert_eq!(found.len(), 1, "{kind:?} {name}: {found:#?}");
    found[0].id.clone()
}

/// Upstream `go-var-groups.test.ts`, LF and CRLF: every member of a var
/// group is a variable the file contains, with its own doc comment or the
/// group's; a call in a member's initializer is that variable's; a read of
/// a member in the same file references it.
#[test]
fn every_member_of_a_go_var_group_is_a_variable() {
    for crlf in [false, true] {
        let source = if crlf {
            SHAPES_GO.replace('\n', "\r\n")
        } else {
            SHAPES_GO.to_string()
        };
        let result = extract_source("shapes/shapes.go", &source, Some(Language::Go));
        let docs: Vec<(&str, Option<String>)> = [
            "ErrNoSides",
            "ErrTooMany",
            "registry",
            "DefaultName",
            "single",
        ]
        .into_iter()
        .map(|name| {
            let id = id_of(&result, NodeKind::Variable, name);
            let node = result.nodes.iter().find(|node| node.id == id).unwrap();
            (name, node.docstring.clone())
        })
        .collect();
        let owned = |text: &str| Some(text.to_string());
        assert_eq!(
            docs,
            [
                (
                    "ErrNoSides",
                    owned("ErrNoSides is returned for a shape without sides.")
                ),
                (
                    "ErrTooMany",
                    owned("Errors returned by the shapes package.")
                ),
                ("registry", None),
                (
                    "DefaultName",
                    owned("DefaultName is documented inside its group.")
                ),
                (
                    "single",
                    owned("single is documented above its own declaration.")
                ),
            ],
            "crlf {crlf}"
        );
        let file = result
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::File)
            .unwrap()
            .id
            .clone();
        for name in ["ErrNoSides", "ErrTooMany", "registry", "DefaultName"] {
            let id = id_of(&result, NodeKind::Variable, name);
            assert!(
                result
                    .edges
                    .iter()
                    .any(|edge| edge.kind == EdgeKind::Contains
                        && edge.source == file
                        && edge.target == id),
                "file contains {name}, crlf {crlf}"
            );
        }
        let too_many = id_of(&result, NodeKind::Variable, "ErrTooMany");
        assert!(
            result.unresolved_references.iter().any(|reference| {
                reference.from_node_id == too_many
                    && reference.reference_kind == EdgeKind::Calls
                    && reference.reference_name == "newError"
            }),
            "ErrTooMany calls newError, crlf {crlf}: {:#?}",
            result.unresolved_references
        );
        let check = id_of(&result, NodeKind::Function, "check");
        for name in ["ErrNoSides", "ErrTooMany"] {
            let id = id_of(&result, NodeKind::Variable, name);
            assert!(
                result
                    .edges
                    .iter()
                    .any(|edge| edge.kind == EdgeKind::References
                        && edge.source == check
                        && edge.target == id),
                "check references {name}, crlf {crlf}"
            );
        }
    }
}

/// A spec's doc is the comment above it in its group, else the comment above
/// its declaration; a comment after code on the line above is that line's,
/// and a `const ( … )` group's members read theirs the same way.
#[test]
fn a_go_spec_doc_is_its_own_or_its_declarations() {
    let source = "package limits

// Limits of a request.
const (
\tBaseLimit = 10 // the floor
\tWideLimit = 50
\t// TallLimit is the ceiling.
\tTallLimit = 90
)

var (
\tfirst = 1 // after the first
\tsecond = 2
)
";
    let result = extract_source("limits.go", source, Some(Language::Go));
    let doc = |kind: NodeKind, name: &str| {
        let id = id_of(&result, kind, name);
        result
            .nodes
            .iter()
            .find(|node| node.id == id)
            .unwrap()
            .docstring
            .clone()
    };
    let group = Some("Limits of a request.".to_string());
    assert_eq!(doc(NodeKind::Constant, "BaseLimit"), group);
    assert_eq!(doc(NodeKind::Constant, "WideLimit"), group);
    assert_eq!(
        doc(NodeKind::Constant, "TallLimit"),
        Some("TallLimit is the ceiling.".to_string())
    );
    assert_eq!(doc(NodeKind::Variable, "first"), None);
    assert_eq!(doc(NodeKind::Variable, "second"), None);
}

/// The calls, composite literals and function values of a package-level
/// initializer are its variable's, and a function value in it is referenced
/// once, from the variable: upstream `function-ref.test.ts`'s Go case.
#[test]
fn a_go_initializer_is_walked_under_its_variable() {
    let source = "package main

func goHome() {}

var handlers = map[string]func(){\"home\": goHome}
";
    let result = extract_source("handlers.go", source, Some(Language::Go));
    let handlers = id_of(&result, NodeKind::Variable, "handlers");
    let sources: Vec<&str> = result
        .unresolved_references
        .iter()
        .filter(|reference| reference.is_function_ref && reference.reference_name == "goHome")
        .map(|reference| reference.from_node_id.as_str())
        .collect();
    assert_eq!(sources, [handlers.as_str()]);
}

/// A `:=` outside a function, which the grammar reads only through error
/// recovery, declares a variable per name over the whole declaration, with
/// its doc and its right side as the signature; that side is not walked, as
/// upstream does not walk it.
#[test]
fn a_top_level_short_declaration_declares_each_name_unwalked() {
    let source = "package main

// doc of the pair
a, b := one(), two()

func one() int { return 1 }
func two() int { return 2 }
";
    let result = extract_source("pair.go", source, Some(Language::Go));
    for name in ["a", "b"] {
        let id = id_of(&result, NodeKind::Variable, name);
        let node = result.nodes.iter().find(|node| node.id == id).unwrap();
        assert_eq!(
            (
                node.start_line,
                node.docstring.as_deref(),
                node.signature.as_deref()
            ),
            (4, Some("doc of the pair"), Some("= one(), two()")),
            "{name}"
        );
        assert!(
            !result
                .unresolved_references
                .iter()
                .any(|reference| reference.from_node_id == id),
            "{name} walks nothing: {:#?}",
            result.unresolved_references
        );
    }
}

/// The signature of `name`'s one node of `kind`.
#[track_caller]
fn signature_of(result: &ExtractionResult, kind: NodeKind, name: &str) -> Option<String> {
    let id = id_of(result, kind, name);
    result
        .nodes
        .iter()
        .find(|node| node.id == id)
        .and_then(|node| node.signature.clone())
}

/// A signature is cut as upstream cuts it, in UTF-16 code units: the
/// value's first 100, `...` when it has 100 or more (exactly 100 included),
/// a pair the cut splits written as U+FFFD (upstream keeps its high half,
/// which its SQLite binding stores as U+FFFD), and none for an empty value.
/// The `:=` form reads its right side the same way.
#[test]
fn a_go_signature_counts_utf16_units_as_upstream() {
    let cjk = "中".repeat(40);
    let source = format!(
        "package main\n\nvar short = \"{cjk}\"\nvar split = \"{}\u{1F600}\"\nvar fits = \"{}\u{1F600}\"\nvar exact = \"{}\"\nconst wide = \"{}\"\n",
        "a".repeat(98),
        "a".repeat(97),
        "x".repeat(98),
        "\u{1F600}".repeat(60),
    );
    let result = extract_source("signatures.go", &source, Some(Language::Go));
    for (kind, name, expected) in [
        (NodeKind::Variable, "short", format!("= \"{cjk}\"")),
        (
            NodeKind::Variable,
            "split",
            format!("= \"{}\u{FFFD}...", "a".repeat(98)),
        ),
        (
            NodeKind::Variable,
            "fits",
            format!("= \"{}\u{1F600}...", "a".repeat(97)),
        ),
        (
            NodeKind::Variable,
            "exact",
            format!("= \"{}\"...", "x".repeat(98)),
        ),
        (
            NodeKind::Constant,
            "wide",
            format!("= \"{}\u{FFFD}...", "\u{1F600}".repeat(49)),
        ),
    ] {
        assert_eq!(
            signature_of(&result, kind, name).as_deref(),
            Some(expected.as_str()),
            "{name}"
        );
    }

    let short_decl = format!("package main\n\n// doc\nc := \"{cjk}\"\n\nfunc f() {{}}\n");
    let result = extract_source("short.go", &short_decl, Some(Language::Go));
    assert_eq!(
        signature_of(&result, NodeKind::Variable, "c").as_deref(),
        Some(format!("= \"{cjk}\"").as_str())
    );

    // Error recovery reads `var broken = ;` with a zero-width value.
    let result = extract_source(
        "missing.go",
        "package main\n\nvar broken = ;\n",
        Some(Language::Go),
    );
    assert_eq!(signature_of(&result, NodeKind::Variable, "broken"), None);
}
