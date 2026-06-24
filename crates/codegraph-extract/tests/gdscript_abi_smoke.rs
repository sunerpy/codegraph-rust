#[test]
fn gdscript_abi_smoke() {
    let mut p = tree_sitter::Parser::new();
    p.set_language(&tree_sitter_gdscript::LANGUAGE.into()).unwrap();
    let tree = p.parse("func f():\n\tpass\n", None).unwrap();
    assert!(!tree.root_node().has_error());
}

#[test]
fn gdscript_abi_smoke_malformed() {
    let mut p = tree_sitter::Parser::new();
    p.set_language(&tree_sitter_gdscript::LANGUAGE.into()).unwrap();
    let tree = p.parse("@@@bad\n%%%\n", None);
    assert!(tree.is_some(), "parser must return Some(tree) even for malformed input");
    assert!(tree.unwrap().root_node().has_error());
}
