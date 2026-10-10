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
