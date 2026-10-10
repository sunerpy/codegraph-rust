//! Objective-C message sends are calls named by their full selector (G5,
//! upstream since `61153f96`): `[obj a:1 b:2]` calls `obj.a:b:`; a `self` or
//! `super` receiver leaves the bare selector, which resolves in the
//! enclosing class; a class receiver is also a reference to that class.

use codegraph_core::types::{EdgeKind, Language};
use codegraph_extract::extract_source;

fn refs_of(source: &str, kind: EdgeKind) -> Vec<String> {
    let result = extract_source("Sources/Sub.m", source, Some(Language::ObjC));
    let mut refs: Vec<_> = result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == kind)
        .map(|r| r.reference_name.clone())
        .collect();
    refs.sort();
    refs
}

#[test]
fn message_sends_are_named_by_receiver_and_selector() {
    let source = "@implementation Sub\n- (void)work {\n    [self ping];\n    [super ping];\n    [c reset];\n    [c storeImage:k];\n    [cache storeImage:img forKey:key];\n    [Base new];\n    [[Factory create] doIt];\n    [[obj foo] bar];\n}\n@end\n";
    assert_eq!(
        refs_of(source, EdgeKind::Calls),
        [
            "Base.new",
            "Factory.create",
            "Factory.create().doIt",
            "bar",
            "c.reset",
            "c.storeImage:",
            "cache.storeImage:forKey:",
            "obj.foo",
            "ping",
            "ping",
        ]
    );
    assert_eq!(refs_of(source, EdgeKind::References), ["Base", "Factory"]);
}
