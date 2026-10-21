//! A Ruby method call keeps its receiver (G11, upstream #2147's extraction
//! half): `message.upcase` is called as `message.upcase`, so resolution can
//! read what the receiver is; a `self`/`super` receiver leaves the bare name,
//! a constant receiver is also a reference to the constant, and `Foo.new`
//! stays an instantiation of `Foo`.

use codegraph_core::types::{EdgeKind, Language};
use codegraph_extract::extract_source;

fn refs_of(source: &str, kind: EdgeKind) -> Vec<String> {
    let result = extract_source("app/service.rb", source, Some(Language::Ruby));
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
fn method_calls_keep_their_receiver() {
    let source = "class Service\n  def run(message)\n    message.upcase\n    @logger.log(message)\n    Formatter.shout(message)\n    self.helper\n    super.helper\n    Logger.new\n    Admin::User.new\n    plain(message)\n  end\nend\n";
    assert_eq!(
        refs_of(source, EdgeKind::Calls),
        [
            "@logger.log",
            "Formatter.shout",
            "helper",
            "helper",
            "message.upcase",
            "plain",
        ]
    );
    assert_eq!(refs_of(source, EdgeKind::References), ["Formatter"]);
    assert_eq!(refs_of(source, EdgeKind::Instantiates), ["Logger", "User"]);
}
