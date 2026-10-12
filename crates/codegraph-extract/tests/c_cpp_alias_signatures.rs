//! A C or C++ `type_alias` node records its declaration as its signature
//! (KEEP-RUST, owner 2026-10-12; upstream #2399 reads an alias's type from the
//! declaration, and records nothing on the node): the original source of the
//! declaration, with the `template <…>` head of an alias template,
//! whitespace collapsed and the trailing `;` dropped. A sync compares it to
//! tell an alias that names another type.

use codegraph_core::types::{ExtractionResult, Language, NodeKind};
use codegraph_extract::extract_source;

/// `(kind, qualified name, signature)` of every symbol.
fn signatures(result: &ExtractionResult) -> Vec<(NodeKind, String, Option<String>)> {
    result
        .nodes
        .iter()
        .filter(|node| !matches!(node.kind, NodeKind::File | NodeKind::Import))
        .map(|node| {
            (
                node.kind,
                node.qualified_name.clone(),
                node.signature.clone(),
            )
        })
        .collect()
}

fn alias(qualified: &str, signature: &str) -> (NodeKind, String, Option<String>) {
    (
        NodeKind::TypeAlias,
        qualified.to_string(),
        Some(signature.to_string()),
    )
}

#[test]
fn a_c_typedef_records_its_declaration() {
    let result = extract_source(
        "attr_macro.c",
        "typedef unsigned int UINT32;\n",
        Some(Language::C),
    );
    assert_eq!(
        signatures(&result),
        [alias("UINT32", "typedef unsigned int UINT32")]
    );
}

#[test]
fn a_cpp_using_and_an_alias_template_record_their_declarations() {
    let result = extract_source(
        "db/table.h",
        concat!(
            "namespace leveldb {\n",
            "using Lookup = Table;\n",
            "template <typename K>\n",
            "using Ordered = SkipList<K>;\n",
            "}  // namespace leveldb\n",
        ),
        Some(Language::Cpp),
    );
    assert_eq!(
        signatures(&result),
        [
            alias("leveldb::Lookup", "using Lookup = Table"),
            alias(
                "leveldb::Ordered",
                "template <typename K> using Ordered = SkipList<K>"
            ),
        ]
    );
}

#[test]
fn whitespace_and_line_breaks_collapse_to_one_space() {
    for source in [
        "typedef SkipList<const char*,\n    KeyComparator>   Table ;\n",
        "typedef SkipList<const char*,\r\n    KeyComparator>   Table ;\r\n",
    ] {
        let result = extract_source("db/memtable.h", source, Some(Language::Cpp));
        assert_eq!(
            signatures(&result),
            [alias(
                "Table",
                "typedef SkipList<const char*, KeyComparator> Table"
            )],
            "{source:?}"
        );
    }
}

#[test]
fn the_signature_is_the_source_as_written_not_the_pre_parsed_text() {
    // The C++ pre-parse blanks `GUARDED_BY(mu_)` after a declared name so the
    // grammar reads the declaration; the signature keeps it, as resolution
    // reads it from the file.
    let result = extract_source(
        "db/counter.h",
        "class Counter {\n  typedef int Count GUARDED_BY(mu_);\n};\n",
        Some(Language::Cpp),
    );
    assert_eq!(
        signatures(&result),
        [
            (NodeKind::Class, "Counter".to_string(), None),
            alias("Counter::Count", "typedef int Count GUARDED_BY(mu_)"),
        ]
    );
}

#[test]
fn a_typedef_that_names_an_aggregate_has_no_signature() {
    let result = extract_source(
        "src/types.c",
        concat!(
            "typedef struct { int a; } Pair;\n",
            "typedef union Word { int i; float f; } Word;\n",
            "typedef enum { MODE_IDLE, MODE_BUSY } Mode;\n",
        ),
        Some(Language::C),
    );
    assert_eq!(
        signatures(&result)
            .into_iter()
            .filter(|(kind, _, _)| *kind != NodeKind::EnumMember)
            .collect::<Vec<_>>(),
        [
            (NodeKind::Struct, "Pair".to_string(), None),
            (NodeKind::Union, "Word".to_string(), None),
            (NodeKind::Enum, "Mode".to_string(), None),
        ]
    );
}
