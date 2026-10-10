//! Supertype clauses each language writes: the `extends`/`implements`
//! references a declaration's heritage clause yields, by the name the
//! resolver binds.

use codegraph_core::types::{EdgeKind, Language};
use codegraph_extract::extract_source;

/// `(declaration, kind, supertype)` for every supertype reference in
/// `source`, sorted.
fn supertypes(path: &str, source: &str, language: Language) -> Vec<(String, EdgeKind, String)> {
    let result = extract_source(path, source, Some(language));
    let name_of = |id: &str| {
        result
            .nodes
            .iter()
            .find(|node| node.id == id)
            .map_or_else(|| id.to_string(), |node| node.name.clone())
    };
    let mut refs: Vec<_> = result
        .unresolved_references
        .iter()
        .filter(|r| matches!(r.reference_kind, EdgeKind::Extends | EdgeKind::Implements))
        .map(|r| {
            (
                name_of(&r.from_node_id),
                r.reference_kind,
                r.reference_name.clone(),
            )
        })
        .collect();
    refs.sort_by(|a, b| (&a.0, &a.2).cmp(&(&b.0, &b.2)));
    refs
}

fn extends(declaration: &str, supertype: &str) -> (String, EdgeKind, String) {
    (
        declaration.to_string(),
        EdgeKind::Extends,
        supertype.to_string(),
    )
}

/// G1 (upstream `b712e4de`): a C# `base_list` names the base class and the
/// interfaces in one list. Each entry is an `extends` reference; the resolver
/// turns the one that binds an interface into `implements`. Type arguments
/// and a `global::` alias are dropped, a qualified name is kept, a primary
/// constructor's arguments are not part of the name, and an enum's underlying
/// type is no supertype.
#[test]
fn csharp_base_lists_name_every_supertype() {
    let source = r#"namespace App
{
    public class Store : BaseStore, IStore { }
    public class Repo<T> : Base<T>, IRepo<T> where T : IEntity { }
    public struct Point : IComparable<Point>, System.IEquatable<Point> { }
    public interface IStore : IDisposable, global::App.IReadable { }
    public record Person(string Name) : Entity(Name), IHasName;
    public enum Flags : byte { None }
    public class Plain { }
}
"#;
    assert_eq!(
        supertypes("src/Store.cs", source, Language::CSharp),
        vec![
            extends("IStore", "App.IReadable"),
            extends("IStore", "IDisposable"),
            extends("Person", "Entity"),
            extends("Person", "IHasName"),
            extends("Point", "IComparable"),
            extends("Point", "System.IEquatable"),
            extends("Repo", "Base"),
            extends("Repo", "IRepo"),
            extends("Store", "BaseStore"),
            extends("Store", "IStore"),
        ]
    );
}

/// G5 (upstream `61153f96`): `@interface Sub : Base <Doer, Saver>` extends
/// its superclass and implements each protocol it adopts.
#[test]
fn objc_interfaces_name_their_superclass_and_protocols() {
    let source =
        "@interface Sub : Base <Doer, Saver>\n- (void)work;\n@end\n\n@interface Root\n@end\n";
    assert_eq!(
        supertypes("Sources/Sub.h", source, Language::ObjC),
        vec![
            extends("Sub", "Base"),
            ("Sub".to_string(), EdgeKind::Implements, "Doer".to_string()),
            ("Sub".to_string(), EdgeKind::Implements, "Saver".to_string()),
        ]
    );
}

/// G6 (upstream v1.0.1): a Python class's bases are `extends` refs, a dotted
/// base by its full path; a `metaclass=` keyword or a subscripted generic
/// (`Generic[T]`) is no base the resolver can bind.
#[test]
fn python_class_bases_are_supertypes() {
    let source = "class Flask(Scaffold, mixins.Mixin, metaclass=Meta):\n    pass\n\n\nclass Box(Generic[T], Base):\n    pass\n\n\nclass Plain:\n    pass\n";
    assert_eq!(
        supertypes("app.py", source, Language::Python),
        vec![
            extends("Box", "Base"),
            extends("Flask", "Scaffold"),
            extends("Flask", "mixins.Mixin"),
        ]
    );
}

/// G7: a PHP class extends its `base_clause` parent and implements each
/// interface of its `class_interface_clause`; an interface extends every
/// parent it lists. A qualified name binds by its last segment, the name a
/// class is stored under and a `use` import brings into scope.
#[test]
fn php_extends_and_implements_are_supertypes() {
    let source = "<?php\nnamespace App;\nclass Store extends \\App\\Base implements Contracts\\Saver, Countable {}\ninterface Saver extends Thing, Other {}\n";
    assert_eq!(
        supertypes("src/Store.php", source, Language::Php),
        vec![
            extends("Saver", "Other"),
            extends("Saver", "Thing"),
            extends("Store", "Base"),
            (
                "Store".to_string(),
                EdgeKind::Implements,
                "Countable".to_string()
            ),
            (
                "Store".to_string(),
                EdgeKind::Implements,
                "Saver".to_string()
            ),
        ]
    );
}
