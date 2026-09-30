//! Python body docstrings (upstream v1.6.1 #1905).
//!
//! A Python docstring — a bare string literal first in a module, class or
//! function body — used to be invisible: only preceding comments reached the
//! `docstring` column. Ported from upstream `python-body-docstrings.test.ts`;
//! `fixtures/python_docstrings/docstrings.py` is upstream's kernel-parity
//! fixture, unchanged.

use std::collections::HashMap;

use codegraph_core::types::Language;
use codegraph_extract::extract_source;

fn docstrings(source: &str) -> HashMap<String, Option<String>> {
    let result = extract_source("ledger.py", source, Some(Language::Python));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    result
        .nodes
        .into_iter()
        .map(|node| (node.name, node.docstring))
        .collect()
}

fn docstring(by_name: &HashMap<String, Option<String>>, name: &str) -> Option<String> {
    by_name
        .get(name)
        .unwrap_or_else(|| panic!("missing node {name}; {:?}", by_name.keys()))
        .clone()
}

// Git may check the fixture out as CRLF. Start from LF so the CRLF variant
// adds exactly one carriage return per newline on either platform.
fn fixture() -> String {
    include_str!("fixtures/python_docstrings/docstrings.py").replace("\r\n", "\n")
}

#[test]
fn module_decorated_async_and_literal_forms() {
    for (ending, source) in [("LF", fixture()), ("CRLF", fixture().replace('\n', "\r\n"))] {
        let by_name = docstrings(&source);
        let expect = |name: &str, want: &str| {
            assert_eq!(
                docstring(&by_name, name).as_deref(),
                Some(want),
                "{ending}: {name}"
            );
        };
        expect("ledger.py", "Ledger module documentation.");
        expect(
            "reconcile_ledger",
            "Legacy reconciliation path.\n\nSettle the nightly discrepancy with the bank.",
        );
        expect(
            "Ledger",
            "Ledger class comment.\n\nPost entries.\n\nPreserve relative indentation:\n    details here.",
        );
        expect("settle", "Method comment.\n\nSettle the ledger.");
        expect("raw_doc", "Raw \\d+ expression.");
        expect("empty_concat", "Kept prose.");
        expect("blank_comment", "Kept despite blank comment.");
        for name in [
            "empty_doc",
            "bytes_doc",
            "raw_bytes_doc",
            "formatted_doc",
            "interpolated_doc",
            "concatenated_bytes",
            "concatenated_format",
            "late_string",
            "computed_string",
            "tuple_string",
            "bare_tuple",
            "singleton_tuple",
            "nested_string",
        ] {
            assert_eq!(docstring(&by_name, name), None, "{ending}: {name}");
        }
    }
}

#[test]
fn tabs_expand_and_relative_indentation_survives() {
    let by_name =
        docstrings("def tabbed():\n    \"\"\"Summary.\n\n\tDetails.\n\t    Nested.\n    \"\"\"\n");
    assert_eq!(
        docstring(&by_name, "tabbed").as_deref(),
        Some("Summary.\n\nDetails.\n    Nested.")
    );
}

#[test]
fn a_non_docstring_module_first_statement_is_rejected() {
    for source in [
        "b\"bytes\"",
        "f\"formatted\"",
        "pass\n\"late\"",
        "\"one\" + \"two\"",
    ] {
        assert_eq!(
            docstring(&docstrings(source), "ledger.py"),
            None,
            "{source}"
        );
    }
}

#[test]
fn a_docstring_and_a_leading_comment_carry_the_same_sentence() {
    let by_name = docstrings(
        r#"
def reconcile_ledger():
    """Settle the nightly discrepancy with the bank."""
    return LEDGER

# Settle the nightly discrepancy with the bank.
def audit_ledger():
    return LEDGER
"#,
    );
    for name in ["reconcile_ledger", "audit_ledger"] {
        assert_eq!(
            docstring(&by_name, name).as_deref(),
            Some("Settle the nightly discrepancy with the bank."),
            "{name}"
        );
    }
}

#[test]
fn a_multiline_docstring_is_dedented_to_its_own_margin() {
    let by_name = docstrings(
        r#"
class Ledger:
    """Post entries to the general ledger.

    Nightly reconciliation runs against the bank feed.
    """
    pass
"#,
    );
    assert_eq!(
        docstring(&by_name, "Ledger").as_deref(),
        Some(
            "Post entries to the general ledger.\n\nNightly reconciliation runs against the bank feed."
        )
    );
}

#[test]
fn a_comment_and_a_docstring_are_both_kept() {
    let by_name = docstrings(
        r#"
# Legacy path, kept for the 2019 import.
def reconcile_ledger():
    """Settle the nightly discrepancy with the bank."""
    return LEDGER
"#,
    );
    assert_eq!(
        docstring(&by_name, "reconcile_ledger").as_deref(),
        Some(
            "Legacy path, kept for the 2019 import.\n\nSettle the nightly discrepancy with the bank."
        )
    );
}

#[test]
fn single_quoted_prefixed_f_string_and_code_first_statements() {
    let by_name = docstrings(
        r#"
def with_single():
    '''Single-quoted docstring.'''
    return 1

def with_raw_prefix():
    r"""Raw \d+ docstring."""
    return 2

def interpolated(name):
    f"""Hello {name}."""
    return name

def no_docstring():
    total = 0
    return total
"#,
    );
    assert_eq!(
        docstring(&by_name, "with_single").as_deref(),
        Some("Single-quoted docstring.")
    );
    assert_eq!(
        docstring(&by_name, "with_raw_prefix").as_deref(),
        Some("Raw \\d+ docstring.")
    );
    assert_eq!(docstring(&by_name, "interpolated"), None);
    assert_eq!(docstring(&by_name, "no_docstring"), None);
}
