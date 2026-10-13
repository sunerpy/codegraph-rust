//! Objective-C messages reached by name alone (upstream #2150, #2179).
//!
//! A message to `self` or `super`, whose receiver the extractor drops,
//! reaches a member of the sender's hierarchy, read from the `@interface
//! Name : Super` heads of the classes it passes and from UIKit / AppKit
//! ancestry for a category on a system class; a message to `super` goes past
//! the class it is written in. An untyped receiver keeps a unique method
//! name's match when it names a class, or ends in a property whose declared
//! type, has the method's owner in its hierarchy.
//!
//! What these read of other files is in each Objective-C file's head row
//! ([`HeadRow`]), which a sync compares to keep unchanged files current.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

use codegraph_core::types::{Language, Node, NodeKind};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::source_facts::SourceFacts;
use crate::types::{RefView, ResolutionContext};

/// The memo of one resolution pass (upstream `OBJC_SUPERS`).
#[derive(Debug, Default)]
pub struct ObjcTables {
    supers: Mutex<HashMap<String, Arc<Vec<String>>>>,
}

impl ObjcTables {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Kinds a member of an Objective-C class is (upstream `OBJC_MEMBER_KINDS`).
pub(crate) fn is_objc_member_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Method | NodeKind::Property | NodeKind::Field
    )
}

/// The superclass an `@interface Name : Super` line names (upstream
/// `/@interface\s+\w+\s*:\s*(\w+)/`).
pub fn written_superclass(line: &str) -> Option<String> {
    static INTERFACE: OnceLock<Regex> = OnceLock::new();
    INTERFACE
        .get_or_init(|| Regex::new(r"@interface\s+\w+\s*:\s*(\w+)").expect("objc interface"))
        .captures(line)
        .map(|c| c[1].to_string())
}

/// The class an `@property … Type *name` line declares `name` as (upstream
/// `objcPropertyTypes`' pattern). One pattern reads every declaration, the
/// name it captures checked against `name`, so no pattern is compiled per
/// name.
pub fn written_property_type(line: &str, name: &str) -> Option<String> {
    static PROPERTY: OnceLock<Regex> = OnceLock::new();
    PROPERTY
        .get_or_init(|| {
            Regex::new(
                r"@property\s*(?:\([^)]*\)\s*)?([A-Z]\w*)\s*(?:<[^;]*>\s*)?\*\s*(?:_Nullable\s+|_Nonnull\s+)?(\w+)",
            )
            .expect("objc property")
        })
        .captures_iter(line)
        .find(|c| &c[2] == name)
        .map(|c| c[1].to_string())
}

/// A UIKit / AppKit class's superclass (upstream `OBJC_SYSTEM_SUPERS`), the
/// ancestry of a category on a system class.
pub fn system_superclass(name: &str) -> Option<&'static str> {
    crate::swift_scope::objc_system_super(name)
}

/// The line `node` starts on, as [`str::lines`] yields it.
fn first_line<'f>(facts: &'f SourceFacts, node: &Node) -> Option<&'f str> {
    let index = usize::try_from(node.start_line.checked_sub(1)?).ok()?;
    (index < facts.line_count()).then(|| facts.line(index))
}

/// The superclasses an Objective-C class's `@interface` declarations name,
/// or its UIKit / AppKit superclass when none does (upstream
/// `objcSupertypesOf`).
pub(crate) fn objc_supertypes_of(name: &str, context: &dyn ResolutionContext) -> Arc<Vec<String>> {
    let tables = context.objc_tables();
    if let Some(hit) = tables
        .supers
        .lock()
        .ok()
        .and_then(|memo| memo.get(name).cloned())
    {
        return hit;
    }
    let mut supers: Vec<String> = Vec::new();
    for decl in context.get_nodes_by_name_shared(name) {
        if decl.kind != NodeKind::Class || decl.language != Language::ObjC {
            continue;
        }
        let Some(facts) = context.source_facts(&decl.file_path) else {
            continue;
        };
        if let Some(sup) = first_line(&facts, &decl).and_then(written_superclass)
            && !supers.contains(&sup)
        {
            supers.push(sup);
        }
    }
    if supers.is_empty()
        && let Some(system) = system_superclass(name)
    {
        supers.push(system.to_string());
    }
    let supers = Arc::new(supers);
    if let Ok(mut memo) = tables.supers.lock() {
        memo.insert(name.to_string(), Arc::clone(&supers));
    }
    supers
}

/// The classes a message to `self` or `super` at `reference` can reach: the
/// class it is written in, first, then what that one inherits from, at most
/// 30 (upstream `objcHierarchyAt`). `None` outside any class.
pub(crate) fn objc_hierarchy_at(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<Vec<String>> {
    let in_file = context.get_nodes_in_file_shared(&reference.file_path);
    let covers = |node: &Node| node.start_line <= reference.line && node.end_line >= reference.line;
    let here = in_file
        .iter()
        .filter(|node| node.kind == NodeKind::Class && covers(node))
        .min_by_key(|node| node.end_line - node.start_line)
        .map(|node| node.name.clone())
        .or_else(|| {
            // An `@implementation` whose range the index lost still names its
            // class in its methods: `SDWebImageDownloaderDecryptor::initWithBlock:`.
            in_file
                .iter()
                .find(|node| {
                    node.kind == NodeKind::Method
                        && covers(node)
                        && node.qualified_name.contains("::")
                })
                .and_then(|node| node.qualified_name.rsplit_once("::"))
                .map(|(owner, _)| owner.to_string())
        })?;
    let mut seen: Vec<String> = Vec::new();
    let mut queue: VecDeque<String> = VecDeque::from([here]);
    while seen.len() < 30 {
        let Some(name) = queue.pop_front() else {
            break;
        };
        if seen.contains(&name) {
            continue;
        }
        queue.extend(objc_supertypes_of(&name, context).iter().cloned());
        seen.push(name);
    }
    Some(seen)
}

/// Whether a message to `self` (or, with `to_super`, to `super`) can mean
/// `node` (upstream `isObjcSelfSendTarget`): a member of a class of the
/// sender's `hierarchy`, past the sender's own class for `super`. Where
/// tree-sitter-objc lost an `@implementation` its methods are functions: one
/// in the sender's own file, or in the file named after a class of the
/// hierarchy, still counts, as does any when the sender's class is unknown;
/// a function elsewhere is never what `[super init]` sends to.
pub(crate) fn is_objc_self_send_target(
    node: &Node,
    reference: &RefView,
    hierarchy: Option<&[String]>,
    to_super: bool,
) -> bool {
    let sender = hierarchy.and_then(|classes| classes.first());
    if is_objc_member_kind(node.kind) {
        let (Some((owner, _)), Some(classes)) = (node.qualified_name.rsplit_once("::"), hierarchy)
        else {
            return true;
        };
        // `[super init]` goes past the class it is written in.
        return classes.iter().any(|class| class == owner)
            && !(to_super && sender.is_some_and(|sender| sender == owner));
    }
    if node.file_path == reference.file_path {
        return !to_super;
    }
    let Some(classes) = hierarchy else {
        return true;
    };
    let base = file_base(&node.file_path);
    classes.iter().any(|class| class == base)
        && !(to_super && sender.is_some_and(|sender| sender == base))
}

/// A path's file name without its extension (upstream
/// `.slice(lastIndexOf('/') + 1).replace(/\.\w+$/, '')`).
pub fn file_base(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rfind('.') {
        Some(dot)
            if dot + 1 < name.len()
                && name[dot + 1..]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_') =>
        {
            &name[..dot]
        }
        _ => name,
    }
}

/// `^[A-Z]\w*$` with JavaScript's ASCII `\w`.
fn is_class_word(text: &str) -> bool {
    text.starts_with(|c: char| c.is_ascii_uppercase())
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Whether an Objective-C receiver's type has `method`'s owner in its
/// hierarchy (upstream `objcReceiverReaches`): the receiver names a project
/// class (`[AllTypesObject objectsInRealm:…]`, a class method RLMObject
/// declares), or ends in a property one of whose `@property … Type *name`
/// declarations gives such a type (`managed.anyDataObj` → RLMSet, for
/// `containsObject:`).
pub(crate) fn objc_receiver_reaches(
    receiver: &str,
    method: &Node,
    context: &dyn ResolutionContext,
) -> bool {
    let Some((owner, _)) = method.qualified_name.rsplit_once("::") else {
        return false;
    };
    let types: Vec<String> = if is_class_word(receiver) {
        if context
            .get_nodes_by_name_shared(receiver)
            .iter()
            .any(|node| node.kind == NodeKind::Class && node.language == Language::ObjC)
        {
            vec![receiver.to_string()]
        } else {
            Vec::new()
        }
    } else if let Some((_, last)) = receiver.rsplit_once('.') {
        objc_property_types(last, context)
    } else {
        Vec::new()
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = types.into();
    while seen.len() < 40 {
        let Some(name) = queue.pop_front() else {
            break;
        };
        if !seen.insert(name.clone()) {
            continue;
        }
        if name == owner {
            return true;
        }
        queue.extend(objc_supertypes_of(&name, context).iter().cloned());
    }
    false
}

/// The classes the `@property … Type *name` declarations of `name` give
/// (upstream `objcPropertyTypes`).
fn objc_property_types(name: &str, context: &dyn ResolutionContext) -> Vec<String> {
    let mut types: Vec<String> = Vec::new();
    for node in context.get_nodes_by_name_shared(name) {
        if node.kind != NodeKind::Property || node.language != Language::ObjC {
            continue;
        }
        let Some(facts) = context.source_facts(&node.file_path) else {
            continue;
        };
        if let Some(written) =
            first_line(&facts, &node).and_then(|line| written_property_type(line, name))
            && !types.contains(&written)
        {
            types.push(written);
        }
    }
    types
}

/// Key prefix of the Objective-C head rows (W4-SW-04): one
/// `project_metadata` row per Objective-C file with a class or property,
/// written by every full index and migration and kept current by sync.
pub const HEAD_ROW_KEY_PREFIX: &str = "objc_heads:";

/// The `project_metadata` key of `file_path`'s head row.
pub fn head_row_key(file_path: &str) -> String {
    format!("{HEAD_ROW_KEY_PREFIX}{file_path}")
}

/// One Objective-C class or property, as the readers of this module take it
/// from its first line: the superclass a class's `@interface` names, the
/// type a property is declared with.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HeadEntry {
    /// `class` or `property`.
    pub kind: String,
    pub name: String,
    pub written: Option<String>,
}

/// An Objective-C file's head row: every class and property, sorted
/// (positions are not part of it, so an edit above a class changes nothing).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadRow {
    pub entries: Vec<HeadEntry>,
}

impl HeadRow {
    /// The row of a file whose text is `source` and whose nodes are `nodes`.
    pub fn of(source: &str, nodes: &[Node]) -> Self {
        let facts = SourceFacts::new(Arc::from(source));
        let mut entries: Vec<HeadEntry> = nodes
            .iter()
            .filter(|node| node.language == Language::ObjC)
            .filter_map(|node| {
                let line = first_line(&facts, node);
                let (kind, written) = match node.kind {
                    NodeKind::Class => ("class", line.and_then(written_superclass)),
                    NodeKind::Property => (
                        "property",
                        line.and_then(|line| written_property_type(line, &node.name)),
                    ),
                    _ => return None,
                };
                Some(HeadEntry {
                    kind: kind.to_string(),
                    name: node.name.clone(),
                    written,
                })
            })
            .collect();
        entries.sort();
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("objc head row encodes")
    }

    pub fn decode(value: &str) -> Option<Self> {
        serde_json::from_str(value).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(kind: NodeKind, qualified_name: &str, file_path: &str, lines: (i64, i64)) -> Node {
        Node {
            id: format!("{}:{qualified_name}", kind.as_str()),
            kind,
            name: qualified_name
                .rsplit("::")
                .next()
                .unwrap_or(qualified_name)
                .to_string(),
            qualified_name: qualified_name.to_string(),
            file_path: file_path.to_string(),
            language: Language::ObjC,
            start_line: lines.0,
            end_line: lines.1,
            start_column: 0,
            end_column: 1,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }

    fn send(file_path: &str) -> RefView {
        RefView {
            row_id: None,
            from_node_id: "method:x".to_string(),
            reference_name: "init".to_string(),
            reference_kind: codegraph_core::types::EdgeKind::Calls,
            line: 4,
            column: 4,
            file_path: file_path.to_string(),
            language: Language::ObjC,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    #[test]
    fn heads_read_the_superclass_and_the_declared_property_type() {
        assert_eq!(
            written_superclass("@interface SDCache : SDBase <SDCaching>").as_deref(),
            Some("SDBase")
        );
        assert_eq!(written_superclass("@interface UIView (WebCache)"), None);
        assert_eq!(
            written_property_type(
                "@property (nonatomic, readonly) RLMSet<NSString *> *anyDataObj;",
                "anyDataObj"
            )
            .as_deref(),
            Some("RLMSet")
        );
        assert_eq!(
            written_property_type("@property NSString * _Nullable name;", "name").as_deref(),
            Some("NSString")
        );
        assert_eq!(
            written_property_type("@property (copy) NSString *a; @property NSURL *b;", "b")
                .as_deref(),
            Some("NSURL")
        );
        assert_eq!(
            written_property_type("@property NSString *names;", "name"),
            None
        );
        assert_eq!(
            written_property_type("@property (assign) BOOL done;", "done"),
            None
        );
    }

    #[test]
    fn a_self_send_reaches_the_hierarchy_and_a_super_send_goes_past_the_sender() {
        let hierarchy = [
            "Child".to_string(),
            "Base".to_string(),
            "NSObject".to_string(),
        ];
        let reference = send("Child.m");
        let child_init = node(NodeKind::Method, "Child::init", "Child.m", (3, 6));
        let base_init = node(NodeKind::Method, "Base::init", "Base.m", (3, 6));
        let other_init = node(NodeKind::Method, "Other::init", "Other.m", (3, 6));
        assert!(is_objc_self_send_target(
            &child_init,
            &reference,
            Some(&hierarchy),
            false
        ));
        assert!(!is_objc_self_send_target(
            &child_init,
            &reference,
            Some(&hierarchy),
            true
        ));
        assert!(is_objc_self_send_target(
            &base_init,
            &reference,
            Some(&hierarchy),
            true
        ));
        assert!(!is_objc_self_send_target(
            &other_init,
            &reference,
            Some(&hierarchy),
            false
        ));
        assert!(is_objc_self_send_target(
            &other_init,
            &reference,
            None,
            true
        ));
        // A function: the sender's own file, or a file named after a class
        // of the hierarchy.
        let here = node(NodeKind::Function, "helper", "Child.m", (8, 9));
        let base = node(NodeKind::Function, "initWithBlock:", "Lib/Base.m", (1, 2));
        let elsewhere = node(NodeKind::Function, "init", "Lib/Util.m", (1, 2));
        assert!(is_objc_self_send_target(
            &here,
            &reference,
            Some(&hierarchy),
            false
        ));
        assert!(!is_objc_self_send_target(
            &here,
            &reference,
            Some(&hierarchy),
            true
        ));
        assert!(is_objc_self_send_target(
            &base,
            &reference,
            Some(&hierarchy),
            true
        ));
        assert!(!is_objc_self_send_target(
            &elsewhere,
            &reference,
            Some(&hierarchy),
            false
        ));
    }

    #[test]
    fn a_file_base_drops_only_a_word_extension() {
        assert_eq!(file_base("Core/UIView+WebCache.m"), "UIView+WebCache");
        assert_eq!(file_base("a.b.m"), "a.b");
        assert_eq!(file_base("Makefile"), "Makefile");
        assert_eq!(file_base("x/a.b-c"), "a.b-c");
    }

    #[test]
    fn a_row_holds_every_class_and_property_with_what_its_line_writes() {
        let text = "@interface SDCache : SDBase\n@property (nonatomic) NSURL *url;\n@end\n@implementation SDCache\n- (void)helper {\n}\n@end\n";
        let nodes = [
            node(NodeKind::Class, "SDCache", "SDCache.m", (1, 3)),
            node(NodeKind::Property, "SDCache::url", "SDCache.m", (2, 2)),
            node(NodeKind::Class, "SDCache", "SDCache.m", (4, 7)),
            node(NodeKind::Method, "SDCache::helper", "SDCache.m", (5, 6)),
        ];
        let row = HeadRow::of(text, &nodes);
        let entry = |kind: &str, name: &str, written: Option<&str>| HeadEntry {
            kind: kind.to_string(),
            name: name.to_string(),
            written: written.map(str::to_string),
        };
        assert_eq!(
            row.entries,
            [
                entry("class", "SDCache", None),
                entry("class", "SDCache", Some("SDBase")),
                entry("property", "url", Some("NSURL")),
            ]
        );
        assert_eq!(HeadRow::decode(&row.encode()), Some(row));
    }
}
