//! MyBatis and iBatis mapper forms upstream v1.6.1 reads (#1182, #1222):
//! close tags with whitespace, commented-out statements, either quote style,
//! namespace-less `<sqlMap>` ids, the iBatis-only verbs, and vendor-split
//! statements written on one line.

use codegraph_core::types::{EdgeKind, ExtractionResult, Language, Node, NodeKind};
use codegraph_extract::extract_source;

fn extract(file: &str, source: &str) -> ExtractionResult {
    let result = extract_source(file, source, Some(Language::Xml));
    assert!(result.errors.is_empty(), "{file}: {:?}", result.errors);
    result
}

fn statements(result: &ExtractionResult) -> Vec<&Node> {
    result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Method)
        .collect()
}

fn qualified_names(result: &ExtractionResult) -> Vec<&str> {
    statements(result)
        .iter()
        .map(|node| node.qualified_name.as_str())
        .collect()
}

fn reference_names(result: &ExtractionResult) -> Vec<&str> {
    result
        .unresolved_references
        .iter()
        .filter(|reference| reference.reference_kind == EdgeKind::References)
        .map(|reference| reference.reference_name.as_str())
        .collect()
}

#[test]
fn a_close_tag_may_carry_whitespace_before_its_bracket() {
    let result = extract(
        "mapper.xml",
        "<mapper namespace=\"com.example.M\">\n\
         \x20 <select id=\"first\">SELECT 1</select >\n\
         \x20 <select id=\"second\">SELECT 2</select>\n\
         </mapper>\n",
    );
    assert_eq!(
        qualified_names(&result),
        vec!["com.example.M::first", "com.example.M::second"]
    );
    assert_eq!(statements(&result)[0].end_line, 2);
}

#[test]
fn commented_out_statements_and_includes_are_ignored_but_cdata_is_not_a_comment() {
    let result = extract(
        "mapper.xml",
        "<mapper namespace=\"com.example.M\">\n\
         \x20 <!-- <select id=\"old\">SELECT 0</select> -->\n\
         \x20 <select id=\"live\">SELECT <!-- <include refid=\"gone\"/> -->\n\
         \x20   <include refid=\"cols\"/></select>\n\
         \x20 <sql id=\"cols\"><![CDATA[ a <!-- ]]></sql>\n\
         \x20 <select id=\"after\">SELECT 3</select>\n\
         </mapper>\n",
    );
    assert_eq!(
        qualified_names(&result),
        vec![
            "com.example.M::live",
            "com.example.M::cols",
            "com.example.M::after"
        ]
    );
    assert_eq!(reference_names(&result), vec!["com.example.M::cols"]);
    let include = &result.unresolved_references[0];
    assert_eq!(include.line, 4, "comment blanking keeps original lines");
}

#[test]
fn ibatis_sqlmaps_without_a_namespace_qualify_by_their_ids() {
    let result = extract(
        "Account.xml",
        "<sqlMap>\n\
         \x20 <select id='Account.getById' resultClass='Account'>SELECT *\n\
         \x20   <include refid='Account.cols'/></select>\n\
         \x20 <statement id=\"Account.raw\">CALL raw()</statement>\n\
         \x20 <procedure id=\"Account.proc\">{call p()}</procedure>\n\
         \x20 <sql id=\"Account.cols\">id, name</sql>\n\
         \x20 <sql id=\"bare\">x</sql>\n\
         \x20 <select id=\"local\"><include refid=\"bare\"/></select>\n\
         </sqlMap>\n",
    );
    assert_eq!(
        qualified_names(&result),
        vec![
            "Account::getById",
            "Account::raw",
            "Account::proc",
            "Account::cols",
            "bare",
            "local"
        ]
    );
    let names = statements(&result)
        .iter()
        .map(|node| node.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec!["getById", "raw", "proc", "cols", "bare", "local"]
    );
    assert_eq!(reference_names(&result), vec!["Account::cols", "bare"]);
}

#[test]
fn either_quote_style_is_read_and_ibatis_verbs_stay_ibatis_only() {
    let ibatis = extract(
        "Legacy.xml",
        "<sqlMap namespace='Legacy'>\n  <statement id='run'>CALL run()</statement>\n</sqlMap>\n",
    );
    assert_eq!(qualified_names(&ibatis), vec!["Legacy::run"]);

    let mybatis = extract(
        "QueryMapper.xml",
        "<mapper namespace='com.example.Q'>\n\
         \x20 <select id='q' parameterType='int' resultType='Row' databaseId='oracle'>SELECT 1</select>\n\
         \x20 <statement id=\"notMyBatis\">CALL x()</statement>\n\
         \x20 <include refid='com.example.Other.cols'/>\n\
         </mapper>\n",
    );
    assert_eq!(qualified_names(&mybatis), vec!["com.example.Q::q"]);
    assert_eq!(
        statements(&mybatis)[0].signature.as_deref(),
        Some("SELECT param=int result=Row databaseId=oracle")
    );
}

#[test]
fn vendor_split_statements_on_one_line_keep_both_nodes() {
    let result = extract(
        "mapper.xml",
        "<mapper namespace=\"com.example.M\"><select id=\"x\" databaseId=\"oracle\">SELECT 1 FROM dual</select><select id=\"x\" databaseId=\"mysql\">SELECT 1</select></mapper>\n",
    );
    let nodes = statements(&result);
    assert_eq!(nodes.len(), 2, "{nodes:#?}");
    assert_ne!(nodes[0].id, nodes[1].id);
    assert_eq!(
        nodes
            .iter()
            .map(|node| node.signature.as_deref())
            .collect::<Vec<_>>(),
        vec![
            Some("SELECT databaseId=oracle"),
            Some("SELECT databaseId=mysql")
        ]
    );
    for node in nodes {
        assert!(
            result
                .edges
                .iter()
                .any(|edge| edge.kind == EdgeKind::Contains && edge.target == node.id)
        );
    }
}
