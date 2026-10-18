//! What a Swift call reached by name can mean, read from how it is written
//! (upstream #2170 in the shape at `b635dd46`, with #2203's constructed
//! receiver and #2205's typed property).
//!
//! A bare call, or one through `self.` / `super.`, reaches only a member of
//! a type around the call or of what it inherits or conforms to (the
//! declarations' heads, UIKit and AppKit ancestry, the standard collection
//! protocols), the nearest first; `super.` the latter only. A call down a
//! longer chain keeps a match unless its name is one the standard types all
//! carry: then the receiver must name the owner, the owner be an outside
//! type the project only extends, or the call give an argument label only the
//! project's method takes. A member of what a constructor before the call
//! builds, or of a property's declared type, is that type's. A playground or
//! test target's globals are no other file's call target.
//!
//! The extractor records a call down a longer chain with its last receiver
//! segment (`entries.removeAll` for `axis.entries.removeAll()`, KEEP-RUST
//! #1496) where upstream records the bare name; [`swift_nested_chain_link`]
//! gives the resolver upstream's bare name for it, as Scala's does.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use crate::csharp_types::js_regex;
use crate::types::{RefView, ResolutionContext};

/// How a Swift call is written (upstream `SwiftCallShape`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shape {
    /// Implicit `self`, or a free function.
    Bare,
    /// Through `self.` / `Self.`.
    SelfCall,
    /// Through `super.`.
    Super,
    /// On some other receiver.
    Chained,
}

#[derive(Debug, Clone)]
pub(crate) struct SwiftCallShape {
    pub(crate) shape: Shape,
    /// A chained call's receiver, calls and subscripts dropped.
    pub(crate) receiver: String,
    /// The first argument's label, if any.
    pub(crate) label: String,
    /// `name[…]`: a subscript of a value, not a call.
    pub(crate) subscript: bool,
    /// `URLEncodedFormDecoder().decode(…)`: the type the link before the
    /// call constructs.
    pub(crate) constructed: Option<String>,
}

/// JavaScript's `\w`, and `$`.
fn is_word_or_dollar(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

fn is_identifier(name: &str) -> bool {
    crate::csharp_types::is_simple_identifier(name)
}

/// Line `line` (1-based) of `file`, as `split(/\r?\n/)` gives it.
fn line_of(file: &str, line: i64, context: &dyn ResolutionContext) -> Option<String> {
    let facts = context.source_facts(file)?;
    let index = usize::try_from(line).ok()?.checked_sub(1)?;
    facts
        .raw_line(index)
        .map(|text| text.strip_suffix('\r').unwrap_or(text).to_string())
}

/// Where `name` is called or subscripted on `line`: the first `name` not
/// preceded by a word character and followed, past whitespace, by `(`, `{`,
/// `<` or `[` (upstream's `(?<![\w$])name\s*[({<[]`).
fn find_call_of(line: &str, name: &str) -> Option<usize> {
    line.match_indices(name).find_map(|(at, _)| {
        let boundary = !line[..at]
            .chars()
            .next_back()
            .is_some_and(is_word_or_dollar);
        let opens = line[at + name.len()..]
            .trim_start()
            .starts_with(['(', '{', '<', '[']);
        (boundary && opens).then_some(at)
    })
}

/// `text` ends with a `.`, past whitespace.
fn ends_with_dot(text: &str) -> bool {
    text.trim_end().ends_with('.')
}

/// `self.m(` / `super.m(`, not `self[i].m(` or `self.items.m(`.
fn plain_self_or_super() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| js_regex(r"(?:^|[^A-Za-z0-9_$.)\]])(self|Self|super)\s*[?!]?\s*\.\s*$"))
}

/// `TypeName(…).` before the call, flat arguments only.
fn constructor_before() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        js_regex(r"([A-Z][A-Za-z0-9_$]*)\s*(?:<[^<>()]*>)?\s*\([^()]*\)\s*[?!]?\s*\.\s*$")
    })
}

/// `(label:` opening the arguments.
fn first_label() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| js_regex(r"^\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*:"))
}

/// `?.` / `!.` read as `.`.
fn optional_dot() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| js_regex(r"[?!]\s*\."))
}

/// How a bare-named Swift call is written at its site (upstream
/// `swiftCallShape`); `None` for any other reference or when its line does
/// not show it.
pub(crate) fn swift_call_shape(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<SwiftCallShape> {
    let name = reference.reference_name.as_str();
    if reference.language != Language::Swift
        || reference.reference_kind != EdgeKind::Calls
        || !is_identifier(name)
    {
        return None;
    }
    let line = line_of(&reference.file_path, reference.line, context)?;
    let column = usize::try_from(reference.column).unwrap_or(usize::MAX);
    let at_column = line
        .get(column..)
        .is_some_and(|rest| rest.starts_with(name))
        && !line[..column]
            .chars()
            .next_back()
            .is_some_and(is_word_or_dollar);
    let start = if at_column {
        column
    } else {
        find_call_of(&line, name)?
    };
    let before = &line[..start];
    let after = &line[start + name.len()..];
    let subscript = js_regex_starts_with_bracket(after);
    if !ends_with_dot(before) {
        return Some(SwiftCallShape {
            shape: Shape::Bare,
            receiver: String::new(),
            label: String::new(),
            subscript,
            constructed: None,
        });
    }
    if let Some(plain) = plain_self_or_super()
        .captures(before)
        .and_then(|captures| captures.get(1))
    {
        return Some(SwiftCallShape {
            shape: if plain.as_str() == "super" {
                Shape::Super
            } else {
                Shape::SelfCall
            },
            receiver: String::new(),
            label: String::new(),
            subscript,
            constructed: None,
        });
    }
    let label = first_label()
        .captures(after)
        .and_then(|captures| {
            let whole = captures.get(0)?;
            // `(?!:)`: a `::` is no label.
            (!after[whole.end()..].starts_with(':')).then(|| captures.get(1))?
        })
        .map_or_else(String::new, |label| label.as_str().to_string());
    let constructed = constructor_before().captures(before).and_then(|captures| {
        let name = captures.get(1)?;
        // `(?<![\w$.])`: the type's name starts the link.
        (!before[..name.start()]
            .chars()
            .next_back()
            .is_some_and(|c| is_word_or_dollar(c) || c == '.'))
        .then(|| name.as_str().to_string())
    });
    Some(SwiftCallShape {
        shape: Shape::Chained,
        receiver: receiver_expression_name(&optional_dot().replace_all(before, ".")),
        label,
        subscript,
        constructed,
    })
}

/// `^\s*\[`, JavaScript's `\s`.
fn js_regex_starts_with_bracket(text: &str) -> bool {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| js_regex(r"^\s*\[")).is_match(text)
}

/// The receiver a `….name(` is written on, as its path and dotted
/// identifiers with arguments dropped (upstream `rustGoReceiverName`):
/// `Command::new("x").short_flag('f')` → `Command::new.short_flag`, a macro
/// `arg!(…)` → `arg`, a Go composite literal `(JSON{data})` → `JSON`. Read
/// backwards to the expression's start.
pub(crate) fn receiver_expression_name(text: &str) -> String {
    static TRAILING_DOT: OnceLock<Regex> = OnceLock::new();
    static LITERAL: OnceLock<Regex> = OnceLock::new();
    let src = TRAILING_DOT
        .get_or_init(|| js_regex(r"\s*\.\s*$"))
        .replace(text, "")
        .into_owned();
    let chars: Vec<char> = src.chars().collect();
    let mut out: Vec<char> = Vec::new();
    let mut i = chars.len() as isize - 1;
    while i >= 0 {
        let ch = chars[i as usize];
        if matches!(ch, ')' | ']' | '}') {
            let open = match ch {
                ')' => '(',
                ']' => '[',
                _ => '{',
            };
            let mut depth = 0i32;
            let mut j = i;
            while j >= 0 {
                let c = chars[j as usize];
                if c == ch {
                    depth += 1;
                } else if c == open {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                j -= 1;
            }
            if j < 0 {
                return String::new();
            }
            // `(JSON{data})`: a parenthesized composite literal names its
            // type.
            if ch == ')' && out.is_empty() {
                let slice: String = chars[j as usize..=i as usize].iter().collect();
                if let Some(name) = LITERAL
                    .get_or_init(|| js_regex(r"^\(\s*&?\s*([A-Za-z_][A-Za-z0-9_.]*)\s*\{"))
                    .captures(&slice)
                    .and_then(|captures| captures.get(1))
                {
                    return name.as_str().to_string();
                }
            }
            i = j - 1;
        } else if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '$' | '.' | ':' | '!') {
            if ch != '!' {
                out.insert(0, ch);
            }
            i -= 1;
        } else {
            break;
        }
    }
    let out: String = out.into_iter().collect();
    out.trim_matches(|c| c == '.' || c == ':').to_string()
}

/// Argument labels the standard library's collection and string methods
/// take (upstream `SWIFT_STD_LABELS`).
const SWIFT_STD_LABELS: &[&str] = &[
    "contentsOf",
    "where",
    "by",
    "at",
    "keepingCapacity",
    "separator",
    "forKey",
    "of",
    "into",
    "in",
    "options",
    "maxSplits",
    "omittingEmptySubsequences",
    "with",
    "after",
    "before",
    "upTo",
    "through",
    "offsetBy",
    "default",
];

/// Whether a Swift method's declaration takes the argument label a call
/// gives first (upstream `swiftDeclaresLabel`): `func contains(jpeg marker:
/// JPEGMarker)` for `data.kf.contains(jpeg: .SOF2)`. Labels are part of a
/// Swift method's name, so one the standard library's namesakes never take
/// identifies the project's. Read without a pattern per method.
fn swift_declares_label(n: &Node, label: &str, context: &dyn ResolutionContext) -> bool {
    if label.is_empty() || SWIFT_STD_LABELS.contains(&label) {
        return false;
    }
    let head: Vec<String> = (n.start_line..n.start_line + 4)
        .filter_map(|line| line_of(&n.file_path, line, context))
        .collect();
    let head = head.join(" ");
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    head.match_indices("func").any(|(at, _)| {
        if head[..at].chars().next_back().is_some_and(is_word) {
            return false;
        }
        let rest = &head[at + 4..];
        if !rest.starts_with(char::is_whitespace) {
            return false;
        }
        let Some(rest) = rest.trim_start().strip_prefix(n.name.as_str()) else {
            return false;
        };
        let mut rest = rest.trim_start();
        if let Some(generic) = rest.strip_prefix('<') {
            let Some(close) = generic.find('>') else {
                return false;
            };
            rest = generic[close + 1..].trim_start();
        }
        let Some(rest) = rest.strip_prefix('(') else {
            return false;
        };
        rest.trim_start()
            .strip_prefix(label)
            .is_some_and(|after| !after.starts_with(is_word))
    })
}

/// What the standard library's collection types and protocols refine, so a
/// project extension of `Collection` is in reach of a type that conforms to
/// `RandomAccessCollection` (upstream `SWIFT_STD_SUPERS`).
pub(crate) fn swift_std_supers(type_name: &str) -> Option<&'static [&'static str]> {
    const RANDOM_MUTABLE_REPLACEABLE: &[&str] = &[
        "RandomAccessCollection",
        "MutableCollection",
        "RangeReplaceableCollection",
    ];
    Some(match type_name {
        "RandomAccessCollection" => &["BidirectionalCollection"],
        "BidirectionalCollection" => &["Collection"],
        "MutableCollection" | "RangeReplaceableCollection" => &["Collection"],
        "Collection" => &["Sequence"],
        "LazySequenceProtocol" => &["Sequence"],
        "LazyCollectionProtocol" => &["Collection", "LazySequenceProtocol"],
        "StringProtocol" => &["BidirectionalCollection"],
        "Array" | "ArraySlice" | "ContiguousArray" => RANDOM_MUTABLE_REPLACEABLE,
        "String" | "Substring" => &["StringProtocol", "RangeReplaceableCollection"],
        "Dictionary" => &["Collection"],
        "Set" => &["Collection", "SetAlgebra"],
        "Range" | "ClosedRange" => &["RandomAccessCollection"],
        _ => return None,
    })
}

/// UIKit and AppKit ancestry (upstream `OBJC_SYSTEM_SUPERS`).
pub(crate) fn objc_system_super(type_name: &str) -> Option<&'static str> {
    Some(match type_name {
        "UIResponder" => "NSObject",
        "UIView" | "UIViewController" => "UIResponder",
        "UIWindow" => "UIView",
        "UIControl" => "UIView",
        "UIButton" | "UITextField" | "UISwitch" | "UISlider" | "UISegmentedControl"
        | "UIStepper" | "UIPageControl" | "UIDatePicker" | "UIRefreshControl" => "UIControl",
        "UIStackView"
        | "UINavigationBar"
        | "UIToolbar"
        | "UITabBar"
        | "UISearchBar"
        | "UIVisualEffectView"
        | "UIActivityIndicatorView"
        | "UIProgressView"
        | "UIPickerView"
        | "UITableViewHeaderFooterView" => "UIView",
        "UITableViewController"
        | "UICollectionViewController"
        | "UINavigationController"
        | "UITabBarController"
        | "UIPageViewController"
        | "UISplitViewController"
        | "UIAlertController"
        | "UIHostingController" => "UIViewController",
        "UIImageView" | "UILabel" | "UIScrollView" => "UIView",
        "UITableView" | "UICollectionView" | "UITextView" => "UIScrollView",
        "UITableViewCell" | "UICollectionReusableView" => "UIView",
        "UICollectionViewCell" => "UICollectionReusableView",
        "MKAnnotationView" | "MKMapView" => "UIView",
        "NSResponder" => "NSObject",
        "NSView" | "NSViewController" | "NSWindow" => "NSResponder",
        "NSControl" => "NSView",
        "NSImageView" | "NSButton" | "NSTextField" | "NSTableView" => "NSControl",
        _ => return None,
    })
}

/// Swift declarations a member hangs off (upstream `SWIFT_TYPE_KINDS`):
/// types, protocols, and extensions (extracted as classes).
pub(crate) fn is_swift_type_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class | NodeKind::Struct | NodeKind::Enum | NodeKind::Interface
    )
}

/// Kinds a member is (upstream `SWIFT_MEMBER_KINDS`).
fn is_swift_member_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Method | NodeKind::Property | NodeKind::Field | NodeKind::EnumMember
    )
}

/// What the project declares a Swift type as (upstream `swiftDeclOf`): the
/// supertypes and protocols every declaration and extension of it names,
/// the standard and system tables first, and whether any of them is the
/// type itself rather than an extension of an outside one.
#[derive(Debug, Default)]
pub(crate) struct SwiftDecl {
    pub(crate) supers: Vec<String>,
    pub(crate) project_type: bool,
}

pub(crate) fn swift_decl_of(type_name: &str, context: &dyn ResolutionContext) -> Arc<SwiftDecl> {
    let tables = context.swift_tables();
    if let Some(hit) = tables.decl(type_name) {
        return hit;
    }
    let mut decl = SwiftDecl::default();
    match swift_std_supers(type_name) {
        Some(supers) => decl.supers.extend(supers.iter().map(|s| s.to_string())),
        None => decl
            .supers
            .extend(objc_system_super(type_name).map(str::to_string)),
    }
    for node in context.get_nodes_by_name_shared(type_name) {
        if node.language != Language::Swift || !is_swift_type_kind(node.kind) {
            continue;
        }
        let head = crate::class_heads::swift_head_of(&node, context);
        decl.supers.extend(head.supers);
        if !head.extension {
            decl.project_type = true;
        }
    }
    let decl = Arc::new(decl);
    tables.set_decl(type_name, Arc::clone(&decl));
    decl
}

/// The Swift types around a call (depth 0) and what they inherit or conform
/// to, by distance (upstream `swiftHierarchyAt`, at most 40 types).
fn swift_hierarchy_at(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Arc<HashMap<String, usize>> {
    let tables = context.swift_tables();
    let key = format!("{}\0{}", reference.file_path, reference.line);
    if let Some(hit) = tables.hierarchy(&key) {
        return hit;
    }
    let mut depths: HashMap<String, usize> = HashMap::new();
    let mut queue: VecDeque<(String, usize)> = context
        .get_nodes_in_file_shared(&reference.file_path)
        .into_iter()
        .filter(|n| {
            is_swift_type_kind(n.kind)
                && n.start_line <= reference.line
                && n.end_line >= reference.line
        })
        .map(|n| (n.name.rsplit('.').next().unwrap_or(&n.name).to_string(), 0))
        .collect();
    while depths.len() < 40 {
        let Some((name, depth)) = queue.pop_front() else {
            break;
        };
        if depths.contains_key(&name) {
            continue;
        }
        for supertype in &swift_decl_of(&name, context).supers {
            queue.push_back((supertype.clone(), depth + 1));
        }
        depths.insert(name, depth);
    }
    let depths = Arc::new(depths);
    tables.set_hierarchy(key, Arc::clone(&depths));
    depths
}

/// A Swift type and every supertype and protocol the project says it has
/// (upstream `swiftTypeClosure`, at most 40).
pub(crate) fn swift_type_closure(
    type_name: &str,
    context: &dyn ResolutionContext,
) -> HashSet<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::from([type_name.to_string()]);
    while seen.len() < 40 {
        let Some(name) = queue.pop_front() else {
            break;
        };
        if seen.contains(&name) {
            continue;
        }
        queue.extend(swift_decl_of(&name, context).supers.iter().cloned());
        seen.insert(name);
    }
    seen
}

/// The owner a member's qualified name gives it: its last type segment.
fn owner_of(n: &Node) -> Option<&str> {
    let cut = n.qualified_name.rfind("::").filter(|&cut| cut > 0)?;
    n.qualified_name[..cut]
        .rsplit(['.', ':'])
        .find(|s| !s.is_empty())
}

/// Of the in-scope members a bare / `self.` / `super.` Swift call could
/// mean, the nearest (upstream `nearestSwiftMembers`): the type's own, else
/// its superclass's. Alamofire's `self.cancel()` in a DownloadRequest
/// extension is DownloadRequest's override, not Request's.
pub(crate) fn nearest_swift_members(
    candidates: Vec<Arc<Node>>,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Vec<Arc<Node>> {
    let hierarchy = swift_hierarchy_at(reference, context);
    let depth_of = |n: &Node| -> Option<usize> {
        if !is_swift_member_kind(n.kind)
            && n.kind != NodeKind::Constant
            && n.kind != NodeKind::Variable
        {
            return None;
        }
        hierarchy.get(owner_of(n)?).copied()
    };
    let depths: Vec<usize> = candidates.iter().filter_map(|n| depth_of(n)).collect();
    if depths.len() < 2 {
        return candidates;
    }
    let nearest = depths.into_iter().min().unwrap_or(0);
    candidates
        .into_iter()
        .filter(|n| depth_of(n).is_none_or(|depth| depth == nearest))
        .collect()
}

/// Methods of Swift's standard collections, strings and optionals and of
/// Foundation / UIKit views, names a project type rarely carries itself
/// (upstream `SWIFT_STD_METHODS`). Ones it often does (`cancel`, `resume`,
/// `store`, `validate`, `load`) are left out.
const SWIFT_STD_METHODS: &[&str] = &[
    "append",
    "insert",
    "remove",
    "removeAll",
    "removeFirst",
    "removeLast",
    "removeValue",
    "contains",
    "map",
    "compactMap",
    "flatMap",
    "filter",
    "reduce",
    "forEach",
    "sorted",
    "sort",
    "first",
    "last",
    "min",
    "max",
    "firstIndex",
    "lastIndex",
    "index",
    "enumerated",
    "reversed",
    "joined",
    "split",
    "prefix",
    "suffix",
    "dropFirst",
    "dropLast",
    "allSatisfy",
    "randomElement",
    "shuffled",
    "popLast",
    "replaceSubrange",
    "replacingOccurrences",
    "components",
    "trimmingCharacters",
    "hasPrefix",
    "hasSuffix",
    "lowercased",
    "uppercased",
    "appending",
    "updateValue",
    "merge",
    "merging",
    "union",
    "intersection",
    "subtracting",
    "formUnion",
    "isEqual",
    "addSubview",
    "removeFromSuperview",
    "setNeedsDisplay",
    "setNeedsLayout",
    "layoutIfNeeded",
    "addGestureRecognizer",
    "addTarget",
    "addObserver",
    "removeObserver",
    "eraseToAnyPublisher",
];

/// Whether a Swift file's declarations are visible from another file as far
/// as separate modules go (upstream `isSwiftTargetVisible`): one under a
/// `…Tests` directory or in a playground only from inside it.
fn is_swift_target_visible(declaration_file: &str, from_file: &str) -> bool {
    let segments: Vec<&str> = declaration_file.split('/').collect();
    let dirs = &segments[..segments.len().saturating_sub(1)];
    let Some(target) = dirs
        .iter()
        .position(|segment| segment.ends_with("Tests") || segment.ends_with(".playground"))
    else {
        return true;
    };
    let prefix = format!("{}/", dirs[..=target].join("/"));
    from_file.starts_with(&prefix)
}

/// Whether a Swift call of that shape can mean `n` (upstream
/// `isSwiftCallTarget`). A member reached with no receiver or through
/// `self.` belongs to a type around the call or to what it inherits or
/// conforms to, through `super.` to the latter only; top-level code has no
/// implicit self. A member reached down a longer chain keeps its match unless
/// its name is one the standard types all carry (unless the receiver names the
/// owner, the owner is an outside type the project only extends, or the call
/// gives a label only the project's method takes); a member of what the link
/// before constructs, or of a typed property, is that type's.
pub(crate) fn is_swift_call_target(
    n: &Node,
    shape: &SwiftCallShape,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    let value = matches!(n.kind, NodeKind::Constant | NodeKind::Variable);
    // A global `let` / `var` from another file is subscripted, not called:
    // Charts' `min(a, b)` went to a playground page's `let min = 20.0`.
    if value && n.file_path != reference.file_path && !shape.subscript {
        return false;
    }
    // A test target imports the module it tests, never the reverse.
    if !is_swift_target_visible(&n.file_path, &reference.file_path) {
        return false;
    }
    let owner = owner_of(n);
    if !is_swift_member_kind(n.kind) && !(owner.is_some() && value) {
        if n.kind != NodeKind::Function {
            return true;
        }
        return shape.shape == Shape::Bare
            || owner.is_some()
            || (shape.shape == Shape::Chained
                && shape.receiver.starts_with(|c: char| c.is_ascii_uppercase()));
    }
    let Some(owner) = owner else {
        return true;
    };
    if shape.shape == Shape::Chained {
        // A member of what the link before constructs, or of what that
        // inherits: vapor's `URLEncodedFormDecoder().decode(…)` is no
        // request's private `_URLQueryContainer.decode`. (A capitalized C
        // function, realm's `RLMObjectBaseObjectSchema(obj)!`, constructs
        // nothing.)
        if let Some(constructed) = &shape.constructed
            && !context
                .get_nodes_by_name_shared(constructed)
                .iter()
                .any(|f| f.kind == NodeKind::Function)
        {
            return swift_type_closure(constructed, context).contains(owner);
        }
        // A property the type around the call declares with a type
        // (Kingfisher's `var cache: ImageCache!`) is that type.
        let simple = shape
            .receiver
            .strip_prefix("self.")
            .unwrap_or(&shape.receiver);
        if is_identifier(simple)
            && let Some(typed) =
                crate::member_types::infer_member_receiver_type(&shape.receiver, reference, context)
            && typed.starts_with(|c: char| c.is_ascii_uppercase())
        {
            return swift_type_closure(&typed, context).contains(owner);
        }
        if shape.receiver.is_empty() || !SWIFT_STD_METHODS.contains(&n.name.as_str()) {
            return true;
        }
        let link = shape.receiver.rsplit('.').next().unwrap_or(&shape.receiver);
        return crate::kotlin_scope::shares_receiver_word(link, n)
            || !swift_decl_of(owner, context).project_type
            || swift_declares_label(n, &shape.label, context);
    }
    swift_hierarchy_at(reference, context)
        .get(owner)
        .is_some_and(|&depth| shape.shape != Shape::Super || depth > 0)
}

/// A Swift call recorded with its last receiver segment (`b.c` for
/// `a.b.c()`, KEEP-RUST #1496), as the bare `c` upstream records it: what the
/// receiver strategies choose must pass upstream's rule for the chained call,
/// and when they choose nothing the bare name matches as upstream matches it.
/// `None` for a call written on a plain receiver (`b.c()`), which upstream
/// records as `b.c` too.
pub(crate) fn swift_nested_chain_link(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    if reference.language != Language::Swift || reference.reference_kind != EdgeKind::Calls {
        return None;
    }
    let (segment, name) = reference.reference_name.split_once('.')?;
    if !is_identifier(segment) || !is_identifier(name) {
        return None;
    }
    let line = line_of(&reference.file_path, reference.line, context)?;
    let column = usize::try_from(reference.column)
        .unwrap_or(0)
        .min(line.len());
    let from = (0..=column)
        .rev()
        .find(|&at| line.is_char_boundary(at))
        .unwrap_or(0);
    let rest = &line[from..];
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    rest.match_indices(segment).find_map(|(at, _)| {
        if rest[..at].chars().next_back().is_some_and(is_word) {
            return None;
        }
        let after = rest[at + segment.len()..].trim_start();
        let after = after.strip_prefix(['?', '!']).unwrap_or(after).trim_start();
        let after = after.strip_prefix('.')?.trim_start();
        let tail = after.strip_prefix(name)?;
        if tail.starts_with(is_word) {
            return None;
        }
        ends_with_dot(&rest[..at]).then(|| name.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_receiver_expression_drops_its_calls_and_subscripts() {
        assert_eq!(
            receiver_expression_name("        return data.kf."),
            "data.kf"
        );
        assert_eq!(
            receiver_expression_name("        axis.entries."),
            "axis.entries"
        );
        assert_eq!(
            receiver_expression_name("x = items[0].first()."),
            "items.first"
        );
        assert_eq!(
            receiver_expression_name("Command::new(\"x\")."),
            "Command::new"
        );
        assert_eq!(receiver_expression_name("(JSON{data})."), "JSON");
        assert_eq!(receiver_expression_name("f(unbalanced]."), "");
    }

    #[test]
    fn test_targets_and_playgrounds_are_visible_from_inside_only() {
        assert!(is_swift_target_visible(
            "Sources/App/A.swift",
            "Tests/AppTests/B.swift"
        ));
        assert!(!is_swift_target_visible(
            "Tests/AppTests/A.swift",
            "Sources/App/B.swift"
        ));
        assert!(is_swift_target_visible(
            "Tests/AppTests/A.swift",
            "Tests/AppTests/Sub/B.swift"
        ));
        assert!(!is_swift_target_visible(
            "Demo/Chart.playground/Contents.swift",
            "Sources/A.swift"
        ));
    }

    #[test]
    fn the_standard_and_system_tables_refine_their_types() {
        assert_eq!(swift_std_supers("Array").map(|s| s.len()), Some(3));
        assert_eq!(swift_std_supers("Collection"), Some(&["Sequence"][..]));
        assert_eq!(
            objc_system_super("UICollectionViewController"),
            Some("UIViewController")
        );
        assert_eq!(objc_system_super("NSButton"), Some("NSControl"));
        assert_eq!(objc_system_super("View"), None);
    }
}
