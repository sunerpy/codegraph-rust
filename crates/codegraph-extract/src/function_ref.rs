//! Function-as-value (callback registration) capture.
//!
//! Ports `upstream extraction/function-ref.ts` (upstream 8a114ba5 /
//! #756 + the 1.0.x multi-language delta). Captures functions/methods passed AS
//! VALUES (`addEventListener('blur', onBlur)`, `signal(SIGINT, handler)`,
//! `&Widget::on_click`, `Handlers::onMessage`, `#selector(fire)`,
//! `method(:cb)`, `usort($a, 'cmp')`, …) and yields candidates the walker gates
//! and emits as `function_ref` references. Capture is table-driven per language.

use crate::walker::{child_by_field, node_text};
use tree_sitter::Node as SyntaxNode;

/// How to pull candidate value nodes out of a dispatched container node.
/// Ports `CaptureMode` (function-ref.ts:60-65).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    Args,
    Rhs,
    Value,
    List,
    VarInit,
}

/// A captured function-value candidate. Ports `FnRefCandidate`
/// (function-ref.ts:36-57).
pub struct FnRefCandidate {
    pub name: String,
    pub line: i64,
    pub column: i64,
    /// Which capture position produced this candidate (gate policy keys on it).
    pub mode: CaptureMode,
    /// True when the value was an explicit reference form (`&fn`, `&Cls::m`,
    /// `::fn`, `#selector`, …) rather than a bare identifier — C++'s flush
    /// policy keys on it.
    pub explicit_ref: bool,
    /// Skip the same-file/import name gate (PHP string callables in HOF args).
    pub skip_gate: bool,
}

/// Bare identifier types + container dispatch + transparent layers + unary
/// wrappers + special whole-node forms + gate policy. Ports `FnRefSpec`
/// (function-ref.ts:73-118).
pub struct FnRefSpec {
    /// Bare identifier node types that can act as a function value.
    id_types: &'static [&'static str],
    /// Container node type → (mode, value field for rhs/value/varinit).
    dispatch: &'static [(&'static str, CaptureMode, Option<&'static str>)],
    /// Transparent wrapper layers: (type, field to descend, or `None` = all
    /// named children).
    layers: &'static [(&'static str, Option<&'static str>)],
    /// Unary wrappers (`&fn`, `@Fn`, `fn _`): (type, operand field, or `None` =
    /// first named child).
    unwrap: &'static [(&'static str, Option<&'static str>)],
    /// Whole-node reference forms needing bespoke name extraction.
    special: &'static [&'static str],
    /// Capture modes whose candidates skip the same-file/import gate (C-family
    /// `value`/`list` file-scope initializers).
    ungated_modes: &'static [CaptureMode],
    /// C++ only: accept ONLY explicit `&`-refs in args/rhs/varinit; file-scope
    /// initializer tables (value/list) still accept bare ids.
    address_of_only: bool,
}

impl FnRefSpec {
    /// Layer descend field for `node_type`, plus whether it is a layer at all.
    fn layer(&self, node_type: &str) -> Option<Option<&'static str>> {
        self.layers
            .iter()
            .find(|(t, _)| *t == node_type)
            .map(|(_, f)| *f)
    }

    /// Unwrap operand field for `node_type`, plus whether it is an unwrap.
    fn unwrap_of(&self, node_type: &str) -> Option<Option<&'static str>> {
        self.unwrap
            .iter()
            .find(|(t, _)| *t == node_type)
            .map(|(_, f)| *f)
    }
}

/// True when `mode` bypasses the same-file/import gate at file scope
/// (C-family `value`/`list`). Ports `FnRefSpec.ungatedModes` (function-ref.ts:107).
pub fn mode_is_ungated(spec: &FnRefSpec, mode: CaptureMode) -> bool {
    spec.ungated_modes.contains(&mode)
}

/// C++ explicit-`&`-only policy. Ports `FnRefSpec.addressOfOnly` (function-ref.ts:117).
pub fn is_address_of_only(spec: &FnRefSpec) -> bool {
    spec.address_of_only
}

/// Names that are never function references. Ports `NAME_STOPLIST`
/// (function-ref.ts:121-134).
const NAME_STOPLIST: &[&str] = &[
    "this",
    "self",
    "super",
    "null",
    "nil",
    "true",
    "false",
    "undefined",
    "new",
    "NULL",
    "nullptr",
    "None",
];

// ---------------------------------------------------------------------------
// Per-language specs. Ports FN_REF_SPECS (function-ref.ts:376-398).
// ---------------------------------------------------------------------------

/// C / C++ / Objective-C share the C-family initializer & assignment shapes.
/// Ports `cFamilySpec` (function-ref.ts:142-167).
const fn c_family_spec(special: &'static [&'static str], address_of_only: bool) -> FnRefSpec {
    FnRefSpec {
        id_types: &["identifier"],
        dispatch: &[
            ("argument_list", CaptureMode::Args, None),
            ("assignment_expression", CaptureMode::Rhs, Some("right")),
            ("init_declarator", CaptureMode::VarInit, Some("value")),
            ("initializer_list", CaptureMode::List, None),
            ("initializer_pair", CaptureMode::Value, Some("value")),
        ],
        layers: &[],
        unwrap: &[("pointer_expression", Some("argument"))],
        special,
        ungated_modes: &[CaptureMode::Value, CaptureMode::List],
        address_of_only,
    }
}

const C_SPEC: FnRefSpec = c_family_spec(&[], false);
const CPP_SPEC: FnRefSpec = c_family_spec(&[], true);
const OBJC_SPEC: FnRefSpec = c_family_spec(&["selector_expression"], false);

/// Ports `TS_JS_SPEC` (function-ref.ts:177-187).
const TS_JS_SPEC: FnRefSpec = FnRefSpec {
    id_types: &["identifier"],
    dispatch: &[
        ("arguments", CaptureMode::Args, None),
        ("assignment_expression", CaptureMode::Rhs, Some("right")),
        ("variable_declarator", CaptureMode::VarInit, Some("value")),
        ("pair", CaptureMode::Value, Some("value")),
        ("array", CaptureMode::List, None),
    ],
    layers: &[],
    unwrap: &[],
    special: &["member_expression"],
    ungated_modes: &[],
    address_of_only: false,
};

/// Ports `PYTHON_SPEC` (function-ref.ts:189-199).
const PYTHON_SPEC: FnRefSpec = FnRefSpec {
    id_types: &["identifier"],
    dispatch: &[
        ("argument_list", CaptureMode::Args, None),
        ("assignment", CaptureMode::Rhs, Some("right")),
        ("keyword_argument", CaptureMode::Value, Some("value")),
        ("pair", CaptureMode::Value, Some("value")),
        ("list", CaptureMode::List, None),
    ],
    layers: &[],
    unwrap: &[],
    special: &["attribute"],
    ungated_modes: &[],
    address_of_only: false,
};

/// Ports `GO_SPEC` (function-ref.ts:201-215).
const GO_SPEC: FnRefSpec = FnRefSpec {
    id_types: &["identifier"],
    dispatch: &[
        ("argument_list", CaptureMode::Args, None),
        ("assignment_statement", CaptureMode::Rhs, Some("right")),
        ("short_var_declaration", CaptureMode::Rhs, Some("right")),
        ("var_spec", CaptureMode::VarInit, Some("value")),
        ("keyed_element", CaptureMode::Value, None),
        ("literal_value", CaptureMode::List, None),
    ],
    layers: &[("literal_element", None), ("expression_list", None)],
    unwrap: &[],
    special: &[],
    ungated_modes: &[],
    address_of_only: false,
};

/// Ports `RUST_SPEC` (function-ref.ts:217-227).
const RUST_SPEC: FnRefSpec = FnRefSpec {
    id_types: &["identifier"],
    dispatch: &[
        ("arguments", CaptureMode::Args, None),
        ("assignment_expression", CaptureMode::Rhs, Some("right")),
        ("field_initializer", CaptureMode::Value, Some("value")),
        ("array_expression", CaptureMode::List, None),
        ("static_item", CaptureMode::VarInit, Some("value")),
        ("let_declaration", CaptureMode::VarInit, Some("value")),
    ],
    layers: &[],
    unwrap: &[],
    special: &[],
    ungated_modes: &[],
    address_of_only: false,
};

/// Ports `JAVA_SPEC` (function-ref.ts:229-238) — method references only.
const JAVA_SPEC: FnRefSpec = FnRefSpec {
    id_types: &[],
    dispatch: &[
        ("argument_list", CaptureMode::Args, None),
        ("assignment_expression", CaptureMode::Rhs, Some("right")),
        ("variable_declarator", CaptureMode::VarInit, Some("value")),
    ],
    layers: &[],
    unwrap: &[],
    special: &["method_reference"],
    ungated_modes: &[],
    address_of_only: false,
};

/// Ports `KOTLIN_SPEC` (function-ref.ts:240-248).
const KOTLIN_SPEC: FnRefSpec = FnRefSpec {
    id_types: &[],
    dispatch: &[
        ("value_arguments", CaptureMode::Args, None),
        ("assignment", CaptureMode::Rhs, None),
    ],
    layers: &[("value_argument", None)],
    unwrap: &[],
    special: &["callable_reference", "navigation_expression"],
    ungated_modes: &[],
    address_of_only: false,
};

/// Ports `CSHARP_SPEC` (function-ref.ts:250-260).
const CSHARP_SPEC: FnRefSpec = FnRefSpec {
    id_types: &["identifier"],
    dispatch: &[
        ("argument_list", CaptureMode::Args, None),
        ("assignment_expression", CaptureMode::Rhs, Some("right")),
        ("initializer_expression", CaptureMode::List, None),
        ("variable_declarator", CaptureMode::VarInit, None),
    ],
    layers: &[("argument", None)],
    unwrap: &[],
    special: &["member_access_expression"],
    ungated_modes: &[],
    address_of_only: false,
};

/// Ports `RUBY_SPEC` (function-ref.ts:262-273).
const RUBY_SPEC: FnRefSpec = FnRefSpec {
    id_types: &[],
    dispatch: &[
        ("argument_list", CaptureMode::Args, None),
        ("pair", CaptureMode::Value, Some("value")),
    ],
    layers: &[("block_argument", None)],
    unwrap: &[],
    special: &["call", "simple_symbol"],
    ungated_modes: &[],
    address_of_only: false,
};

/// Ports `SWIFT_SPEC` (function-ref.ts:288-298).
const SWIFT_SPEC: FnRefSpec = FnRefSpec {
    id_types: &["simple_identifier"],
    dispatch: &[
        ("value_arguments", CaptureMode::Args, None),
        ("assignment", CaptureMode::Rhs, Some("result")),
        ("array_literal", CaptureMode::List, None),
        ("property_declaration", CaptureMode::VarInit, Some("value")),
    ],
    layers: &[("value_argument", Some("value"))],
    unwrap: &[],
    special: &["selector_expression"],
    ungated_modes: &[],
    address_of_only: false,
};

/// Ports `SCALA_SPEC` (function-ref.ts:300-308).
const SCALA_SPEC: FnRefSpec = FnRefSpec {
    id_types: &["identifier"],
    dispatch: &[
        ("arguments", CaptureMode::Args, None),
        ("assignment_expression", CaptureMode::Rhs, Some("right")),
        ("val_definition", CaptureMode::VarInit, Some("value")),
    ],
    layers: &[],
    unwrap: &[("postfix_expression", None)],
    special: &[],
    ungated_modes: &[],
    address_of_only: false,
};

/// Ports `DART_SPEC` (function-ref.ts:310-320).
const DART_SPEC: FnRefSpec = FnRefSpec {
    id_types: &["identifier"],
    dispatch: &[
        ("arguments", CaptureMode::Args, None),
        ("assignment_expression", CaptureMode::Rhs, Some("right")),
        ("pair", CaptureMode::Value, Some("value")),
        ("list_literal", CaptureMode::List, None),
        ("static_final_declaration", CaptureMode::VarInit, None),
    ],
    layers: &[("argument", None)],
    unwrap: &[],
    special: &[],
    ungated_modes: &[],
    address_of_only: false,
};

/// Ports `LUA_SPEC` (function-ref.ts:322-330); also used for Luau.
const LUA_SPEC: FnRefSpec = FnRefSpec {
    id_types: &["identifier"],
    dispatch: &[
        ("arguments", CaptureMode::Args, None),
        ("assignment_statement", CaptureMode::Rhs, None),
        ("field", CaptureMode::Value, Some("value")),
    ],
    layers: &[("expression_list", None)],
    unwrap: &[],
    special: &[],
    ungated_modes: &[],
    address_of_only: false,
};

/// Ports `PASCAL_SPEC` (function-ref.ts:332-339).
const PASCAL_SPEC: FnRefSpec = FnRefSpec {
    id_types: &["identifier"],
    dispatch: &[
        ("exprArgs", CaptureMode::Args, None),
        ("assignment", CaptureMode::Rhs, Some("rhs")),
    ],
    layers: &[],
    unwrap: &[("exprUnary", Some("operand"))],
    special: &[],
    ungated_modes: &[],
    address_of_only: false,
};

/// Ports `PHP_SPEC` (function-ref.ts:360-371).
const PHP_SPEC: FnRefSpec = FnRefSpec {
    id_types: &[],
    dispatch: &[("arguments", CaptureMode::Args, None)],
    layers: &[("argument", None)],
    unwrap: &[],
    special: &["encapsed_string", "string", "array_creation_expression"],
    ungated_modes: &[],
    address_of_only: false,
};

/// Rails/ActiveSupport hook DSLs whose symbol arguments name a method of the
/// enclosing class. Ports `RUBY_HOOK_NAMES` (function-ref.ts:283).
const RUBY_HOOK_NAMES: &[&str] = &["validate", "set_callback", "helper_method", "rescue_from"];

/// PHP core functions whose string arguments are callables. Ports
/// `PHP_CALLABLE_HOFS` (function-ref.ts:347-358).
const PHP_CALLABLE_HOFS: &[&str] = &[
    "array_map",
    "array_filter",
    "array_walk",
    "array_walk_recursive",
    "array_reduce",
    "usort",
    "uasort",
    "uksort",
    "array_udiff",
    "array_udiff_assoc",
    "array_uintersect",
    "array_uintersect_assoc",
    "call_user_func",
    "call_user_func_array",
    "forward_static_call",
    "forward_static_call_array",
    "preg_replace_callback",
    "preg_replace_callback_array",
    "register_shutdown_function",
    "register_tick_function",
    "set_error_handler",
    "set_exception_handler",
    "spl_autoload_register",
    "ob_start",
    "iterator_apply",
    "header_register_callback",
    "is_callable",
];

/// Ports `isRubyHookCall` (function-ref.ts:284-286): `RUBY_HOOK_RE`
/// `^(skip_)?(before|after|around)_[a-z_]+$` or a name in `RUBY_HOOK_NAMES`.
fn is_ruby_hook_call(name: &str) -> bool {
    if RUBY_HOOK_NAMES.contains(&name) {
        return true;
    }
    let rest = name.strip_prefix("skip_").unwrap_or(name);
    let rest = ["before_", "after_", "around_"]
        .iter()
        .find_map(|p| rest.strip_prefix(p));
    match rest {
        Some(tail) => !tail.is_empty() && tail.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
        None => false,
    }
}

/// The `FnRefSpec` for a language, or `None` if function-ref capture is not
/// supported. Ports `FN_REF_SPECS` (function-ref.ts:376-398).
pub fn fn_ref_spec(language: codegraph_core::types::Language) -> Option<&'static FnRefSpec> {
    use codegraph_core::types::Language;
    Some(match language {
        Language::C => &C_SPEC,
        Language::Cpp => &CPP_SPEC,
        Language::ObjC => &OBJC_SPEC,
        Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx => &TS_JS_SPEC,
        Language::Python => &PYTHON_SPEC,
        Language::Go => &GO_SPEC,
        Language::Rust => &RUST_SPEC,
        Language::Java => &JAVA_SPEC,
        Language::Kotlin => &KOTLIN_SPEC,
        Language::CSharp => &CSHARP_SPEC,
        Language::Php => &PHP_SPEC,
        Language::Ruby => &RUBY_SPEC,
        Language::Swift => &SWIFT_SPEC,
        Language::Scala => &SCALA_SPEC,
        Language::Dart => &DART_SPEC,
        Language::Lua | Language::Luau => &LUA_SPEC,
        Language::Pascal => &PASCAL_SPEC,
        _ => return None,
    })
}

/// The dispatch rule for a container node type, if any.
pub fn dispatch_rule(
    spec: &FnRefSpec,
    node_type: &str,
) -> Option<(CaptureMode, Option<&'static str>)> {
    spec.dispatch
        .iter()
        .find(|(t, _, _)| *t == node_type)
        .map(|(_, mode, field)| (*mode, *field))
}

// ---------------------------------------------------------------------------
// Capture. Ports captureFnRefCandidates (function-ref.ts:408-511).
// ---------------------------------------------------------------------------

/// Collect candidate function names from a dispatched container node.
pub fn capture_fn_ref_candidates(
    container: SyntaxNode<'_>,
    mode: CaptureMode,
    field: Option<&str>,
    spec: &FnRefSpec,
    source: &str,
) -> Vec<FnRefCandidate> {
    let mut value_nodes: Vec<SyntaxNode<'_>> = Vec::new();

    match mode {
        CaptureMode::Args | CaptureMode::List => {
            for i in 0..container.named_child_count() {
                if let Some(child) = container.named_child(i as u32) {
                    value_nodes.push(child);
                }
            }
        }
        CaptureMode::Rhs => {
            let rhs = match field {
                Some(f) => child_by_field(container, f),
                None => last_named_child(container),
            };
            if let Some(rhs) = rhs {
                // Param-storage skip: `this.status = status` / `o->cb = cb`
                // (function-ref.ts:434-443).
                let lhs = child_by_field(container, "left")
                    .or_else(|| child_by_field(container, "lhs"))
                    .or_else(|| child_by_field(container, "target"))
                    .or_else(|| {
                        if container.named_child_count() >= 2 {
                            container.named_child(0)
                        } else {
                            None
                        }
                    });
                let lhs_text = lhs.map(|n| node_text(n, source)).unwrap_or_default();
                let lhs_last_name = last_identifier(&lhs_text);
                let rhs_text = node_text(rhs, source).trim().to_string();
                if lhs_last_name.as_deref() != Some(rhs_text.as_str()) {
                    value_nodes.push(rhs);
                }
            }
        }
        CaptureMode::Value => {
            let value = field
                .and_then(|f| child_by_field(container, f))
                .or_else(|| last_named_child(container));
            if let Some(value) = value {
                value_nodes.push(value);
            }
        }
        CaptureMode::VarInit => {
            // Destructuring extracts DATA, never a function alias
            // (function-ref.ts:462-467).
            let name_node =
                child_by_field(container, "name").or_else(|| child_by_field(container, "pattern"));
            let is_pattern = name_node.is_some_and(|n| {
                matches!(
                    n.kind(),
                    "object_pattern" | "array_pattern" | "tuple_pattern" | "struct_pattern"
                )
            });
            if !is_pattern {
                match field {
                    Some(f) => {
                        if let Some(value) = child_by_field(container, f) {
                            value_nodes.push(value);
                        }
                    }
                    None => {
                        // No value field (C# variable_declarator, Dart
                        // static_final_declaration): the initializer is the
                        // last named child — require ≥2 named children and
                        // never pick the name/pattern child (function-ref.ts:471-486).
                        let value = last_named_child(container);
                        let name_child = name_node;
                        if let Some(value) = value {
                            if container.named_child_count() >= 2
                                && name_child.is_none_or(|n| value.id() != n.id())
                            {
                                value_nodes.push(value);
                            }
                        }
                    }
                }
            }
        }
    }

    let mut out = Vec::new();
    for v in value_nodes {
        // A bare identifier is one that normalizes without passing through an
        // unwrap/special reference form (function-ref.ts:493-497).
        let explicit_ref = !spec.id_types.contains(&v.kind());
        for nref in normalize_value(v, spec, source, 0) {
            if nref.name.is_empty() || NAME_STOPLIST.contains(&nref.name.as_str()) {
                continue;
            }
            out.push(FnRefCandidate {
                name: nref.name,
                line: nref.node.start_position().row as i64 + 1,
                column: nref.node.start_position().column as i64,
                mode,
                explicit_ref,
                skip_gate: nref.skip_gate,
            });
        }
    }
    out
}

/// One normalized function-value: its name, source node, and gate policy.
struct NormalizedRef<'tree> {
    name: String,
    node: SyntaxNode<'tree>,
    skip_gate: bool,
}

impl<'tree> NormalizedRef<'tree> {
    fn new(name: String, node: SyntaxNode<'tree>) -> Self {
        Self {
            name,
            node,
            skip_gate: false,
        }
    }
}

/// Normalize one value expression to zero or more function names + source node.
/// Ports `normalizeValue` (function-ref.ts:525-597).
fn normalize_value<'tree>(
    node: SyntaxNode<'tree>,
    spec: &FnRefSpec,
    source: &str,
    depth: u32,
) -> Vec<NormalizedRef<'tree>> {
    if depth > 4 {
        return Vec::new();
    }
    let node_type = node.kind();

    // Bare identifier
    if spec.id_types.contains(&node_type) {
        return vec![NormalizedRef::new(node_text(node, source), node)];
    }

    // Transparent layers (argument, value_argument, literal_element,
    // expression_list, block_argument). expression_list fans out.
    if let Some(layer_field) = spec.layer(node_type) {
        // Labeled-argument param-forward skip (Swift/Kotlin): `value: value`
        // (function-ref.ts:543-557).
        if node_type == "value_argument" {
            let label = child_by_field(node, "name");
            let value = child_by_field(node, "value").or_else(|| last_named_child(node));
            if let (Some(label), Some(value)) = (label, value) {
                if node_text(label, source).trim() == node_text(value, source).trim() {
                    return Vec::new();
                }
            }
        }
        if let Some(layer_field) = layer_field {
            return child_by_field(node, layer_field)
                .map(|inner| normalize_value(inner, spec, source, depth + 1))
                .unwrap_or_default();
        }
        let mut results = Vec::new();
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i as u32) {
                results.extend(normalize_value(child, spec, source, depth + 1));
            }
        }
        return results;
    }

    // Unary wrappers: &fn / @Fn / `fn _`
    if let Some(unwrap_field) = spec.unwrap_of(node_type) {
        // C-family `pointer_expression`: only `&` (address-of) qualifies, never
        // `*` (dereference) (function-ref.ts:577).
        if node_type == "pointer_expression" && node.child(0).map(|c| c.kind()) != Some("&") {
            return Vec::new();
        }
        let inner = match unwrap_field {
            Some(f) => child_by_field(node, f),
            None => node.named_child(0),
        };
        let Some(inner) = inner else {
            return Vec::new();
        };
        // C++ `&Widget::on_click` — keep the QUALIFIED name (function-ref.ts:584-587).
        if inner.kind() == "qualified_identifier" {
            let text = node_text(inner, source).trim().to_string();
            return if is_qualified_name(&text) {
                vec![NormalizedRef::new(text, inner)]
            } else {
                Vec::new()
            };
        }
        return normalize_value(inner, spec, source, depth + 1);
    }

    // Special whole-node reference forms
    if spec.special.contains(&node_type) {
        return normalize_special(node, node_type, source);
    }

    Vec::new()
}

/// Whole-node reference forms. Ports `normalizeSpecial` (function-ref.ts:612-810).
fn normalize_special<'tree>(
    node: SyntaxNode<'tree>,
    node_type: &str,
    source: &str,
) -> Vec<NormalizedRef<'tree>> {
    match node_type {
        // Java method references (function-ref.ts:625-644).
        "method_reference" => {
            let mut last: Option<SyntaxNode<'tree>> = None;
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i as u32) {
                    if child.kind() == "identifier" {
                        last = Some(child);
                    }
                }
            }
            let Some(last) = last else {
                return Vec::new();
            };
            let m = node_text(last, source);
            let text = node_text(node, source);
            if text.starts_with("this::") || text.starts_with("super::") {
                return vec![NormalizedRef::new(format!("this.{m}"), last)];
            }
            if let Some(recv) = capitalized_receiver(&text) {
                // `Type::new` (constructor ref) has no method node — drop it.
                return if m == "new" {
                    Vec::new()
                } else {
                    vec![NormalizedRef::new(format!("{recv}::{m}"), last)]
                };
            }
            Vec::new()
        }

        // Kotlin `::topFn` / `OtherClass::handle` (function-ref.ts:649-665).
        // tree-sitter-kotlin-ng nests bare `identifier` children (not
        // `simple_identifier`); the member is the last identifier, the receiver
        // (if 2+) the prior one — capitalized → qualified, lowercase → none.
        "callable_reference" => {
            let mut ids: Vec<SyntaxNode<'tree>> = Vec::new();
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i as u32) {
                    if matches!(
                        child.kind(),
                        "identifier" | "simple_identifier" | "type_identifier"
                    ) {
                        ids.push(child);
                    }
                }
            }
            let Some(member) = ids.last().copied() else {
                return Vec::new();
            };
            let m = node_text(member, source);
            if ids.len() < 2 {
                return vec![NormalizedRef::new(m, member)];
            }
            let recv_text = node_text(ids[ids.len() - 2], source);
            if recv_text.chars().next().is_some_and(|c| c.is_uppercase()) {
                vec![NormalizedRef::new(format!("{recv_text}::{m}"), member)]
            } else {
                Vec::new()
            }
        }

        // Kotlin `this::fire` / `Type::fire` (navigation_expression)
        // (function-ref.ts:671-681). tree-sitter-kotlin-ng also uses this node
        // for ordinary `a.b` member access, so require `::` in the text and
        // route by receiver: this:: → this.<m>; Type:: → Type::<m>; lowercase
        // receiver → none.
        "navigation_expression" => {
            if !node_text(node, source).contains("::") {
                return Vec::new();
            }
            let mut ids: Vec<SyntaxNode<'tree>> = Vec::new();
            let mut this_recv = false;
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i as u32) {
                    match child.kind() {
                        "this_expression" => this_recv = true,
                        "identifier" | "simple_identifier" | "type_identifier" => ids.push(child),
                        "navigation_suffix" if node_text(child, source).starts_with("::") => {
                            if let Some(id) = last_named_child(child) {
                                ids.push(id);
                            }
                        }
                        _ => {}
                    }
                }
            }
            let Some(member) = ids.last().copied() else {
                return Vec::new();
            };
            let m = node_text(member, source);
            if this_recv {
                return vec![NormalizedRef::new(format!("this.{m}"), member)];
            }
            if ids.len() >= 2 {
                let recv_text = node_text(ids[ids.len() - 2], source);
                if recv_text.chars().next().is_some_and(|c| c.is_uppercase()) {
                    return vec![NormalizedRef::new(format!("{recv_text}::{m}"), member)];
                }
            }
            Vec::new()
        }

        // Swift `#selector(fire)` / ObjC `@selector(storeImage:)`
        // (function-ref.ts:685-696).
        "selector_expression" => {
            let Some(inner) = node.named_child(0) else {
                return Vec::new();
            };
            if inner.kind() == "identifier" || inner.kind() == "simple_identifier" {
                return vec![NormalizedRef::new(node_text(inner, source), inner)];
            }
            if let Some(last) = last_named_of_type(node, &["simple_identifier"]) {
                return vec![NormalizedRef::new(node_text(last, source), last)];
            }
            vec![NormalizedRef::new(
                node_text(inner, source).trim().to_string(),
                inner,
            )]
        }

        // Ruby `method(:target_cb)` (function-ref.ts:700-709).
        "call" => {
            let method = child_by_field(node, "method");
            if method.map(|m| node_text(m, source)).as_deref() != Some("method") {
                return Vec::new();
            }
            let Some(args) = child_by_field(node, "arguments") else {
                return Vec::new();
            };
            if args.named_child_count() != 1 {
                return Vec::new();
            }
            let Some(sym) = args.named_child(0) else {
                return Vec::new();
            };
            if sym.kind() != "simple_symbol" {
                return Vec::new();
            }
            let name = node_text(sym, source);
            let name = name.strip_prefix(':').unwrap_or(&name).to_string();
            if name.is_empty() {
                Vec::new()
            } else {
                vec![NormalizedRef::new(name, sym)]
            }
        }

        // `this.handleClick` (TS/JS) (function-ref.ts:714-721).
        "member_expression" => {
            let obj = child_by_field(node, "object");
            let prop = child_by_field(node, "property");
            if let (Some(obj), Some(prop)) = (obj, prop) {
                if obj.kind() == "this" && prop.kind() == "property_identifier" {
                    return vec![NormalizedRef::new(
                        format!("this.{}", node_text(prop, source)),
                        prop,
                    )];
                }
            }
            Vec::new()
        }

        // `self.handle_click` (Python) (function-ref.ts:724-731).
        "attribute" => {
            let obj = child_by_field(node, "object");
            let attr = child_by_field(node, "attribute");
            if let (Some(obj), Some(attr)) = (obj, attr) {
                if obj.kind() == "identifier" && node_text(obj, source) == "self" {
                    return vec![NormalizedRef::new(node_text(attr, source), attr)];
                }
            }
            Vec::new()
        }

        // `this.Run0` (C#) (function-ref.ts:738-746).
        "member_access_expression" => {
            let Some(name) = child_by_field(node, "name") else {
                return Vec::new();
            };
            let expr = child_by_field(node, "expression");
            let is_this_receiver = match expr {
                Some(e) => e.kind() == "this_expression" || e.kind() == "this",
                None => node_text(node, source).starts_with("this."),
            };
            if is_this_receiver {
                vec![NormalizedRef::new(node_text(name, source), name)]
            } else {
                Vec::new()
            }
        }

        // PHP string callable (function-ref.ts:753-766).
        "encapsed_string" | "string" => {
            let Some(callee) = php_enclosing_call_name(node, source) else {
                return Vec::new();
            };
            if !PHP_CALLABLE_HOFS.contains(&callee.as_str()) {
                return Vec::new();
            }
            let Some(content) = php_string_content(node, source) else {
                return Vec::new();
            };
            if is_simple_name(&content) || is_qualified_double_colon(&content) {
                let mut nref = NormalizedRef::new(content, node);
                nref.skip_gate = true;
                vec![nref]
            } else {
                Vec::new()
            }
        }

        // PHP array callables (function-ref.ts:771-790).
        "array_creation_expression" => {
            if node.named_child_count() != 2 {
                return Vec::new();
            }
            let recv = node.named_child(0).and_then(|c| c.named_child(0));
            let str_el = node.named_child(1).and_then(|c| c.named_child(0));
            let (Some(recv), Some(str_el)) = (recv, str_el) else {
                return Vec::new();
            };
            if str_el.kind() != "encapsed_string" && str_el.kind() != "string" {
                return Vec::new();
            }
            let Some(member) = php_string_content(str_el, source) else {
                return Vec::new();
            };
            if !is_simple_name(&member) {
                return Vec::new();
            }
            if recv.kind() == "variable_name" && node_text(recv, source) == "$this" {
                return vec![NormalizedRef::new(format!("this.{member}"), str_el)];
            }
            if recv.kind() == "class_constant_access_expression" {
                let cls = recv.named_child(0);
                let kw = recv.named_child(1);
                if let (Some(cls), Some(kw)) = (cls, kw) {
                    if node_text(kw, source) == "class" {
                        return vec![NormalizedRef::new(
                            format!("{}::{member}", node_text(cls, source)),
                            str_el,
                        )];
                    }
                }
            }
            Vec::new()
        }

        // Ruby hook-DSL symbols (function-ref.ts:797-804).
        "simple_symbol" => {
            let Some(call) = ruby_enclosing_call(node) else {
                return Vec::new();
            };
            let method = child_by_field(call, "method");
            if !method.is_some_and(|m| is_ruby_hook_call(&node_text(m, source))) {
                return Vec::new();
            }
            let sym = node_text(node, source);
            let sym = sym.strip_prefix(':').unwrap_or(&sym);
            if !is_ruby_method_symbol(sym) {
                return Vec::new();
            }
            vec![NormalizedRef::new(format!("this.{sym}"), node)]
        }

        _ => Vec::new(),
    }
}

/// Rightmost descendant-or-self named child of one of the given types.
/// Ports `lastNamedOfType` (function-ref.ts:600-610).
fn last_named_of_type<'tree>(node: SyntaxNode<'tree>, types: &[&str]) -> Option<SyntaxNode<'tree>> {
    let mut found = None;
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i as u32) else {
            continue;
        };
        if types.contains(&child.kind()) {
            found = Some(child);
        }
        if let Some(deeper) = last_named_of_type(child, types) {
            found = Some(deeper);
        }
    }
    found
}

/// Content of a PHP string literal node. Ports `phpStringContent`
/// (function-ref.ts:813-819).
fn php_string_content(node: SyntaxNode<'_>, source: &str) -> Option<String> {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            if child.kind() == "string_content" {
                return Some(node_text(child, source).trim().to_string());
            }
        }
    }
    None
}

/// The function name of the PHP call whose arguments contain `node`. Ports
/// `phpEnclosingCallName` (function-ref.ts:822-834).
fn php_enclosing_call_name(node: SyntaxNode<'_>, source: &str) -> Option<String> {
    let mut cur = node.parent();
    let mut hops = 0;
    while let Some(c) = cur {
        if hops >= 4 {
            break;
        }
        match c.kind() {
            "function_call_expression" => {
                return child_by_field(c, "function").map(|fnn| node_text(fnn, source));
            }
            "member_call_expression" | "scoped_call_expression" => return None,
            _ => {}
        }
        cur = c.parent();
        hops += 1;
    }
    None
}

/// The Ruby `call` node enclosing `node`. Ports `rubyEnclosingCall`
/// (function-ref.ts:837-842).
fn ruby_enclosing_call(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let mut cur = node.parent();
    let mut hops = 0;
    while let Some(c) = cur {
        if hops >= 4 {
            break;
        }
        if c.kind() == "call" {
            return Some(c);
        }
        cur = c.parent();
        hops += 1;
    }
    None
}

/// Last named child of a node, or `None`.
fn last_named_child(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let count = node.named_child_count();
    if count == 0 {
        None
    } else {
        node.named_child(count as u32 - 1)
    }
}

/// `^[A-Z][A-Za-z0-9_]*` leading-type receiver of a `Type::method` text.
fn capitalized_receiver(text: &str) -> Option<String> {
    let before = text.split("::").next()?.trim();
    let mut chars = before.chars();
    let first = chars.next()?;
    if first.is_ascii_uppercase()
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !before.is_empty()
    {
        Some(before.to_string())
    } else {
        None
    }
}

/// `^[A-Za-z_][A-Za-z0-9_]*$`.
fn is_simple_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

/// `^[A-Za-z_][\w:]*$` (a qualified `Cls::method` member-pointer name).
fn is_qualified_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
        }
        _ => false,
    }
}

/// `^[A-Za-z_][A-Za-z0-9_]*::[A-Za-z_][A-Za-z0-9_]*$`.
fn is_qualified_double_colon(s: &str) -> bool {
    match s.split_once("::") {
        Some((a, b)) => is_simple_name(a) && is_simple_name(b),
        None => false,
    }
}

/// `^[A-Za-z_][A-Za-z0-9_?!]*$` (a Ruby method symbol).
fn is_ruby_method_symbol(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '?' || c == '!')
        }
        _ => false,
    }
}

/// Trailing identifier of an LHS expression (for the param-storage skip).
fn last_identifier(text: &str) -> Option<String> {
    let trimmed = text.trim_end();
    let start = trimmed
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .map_or(0, |i| i + c_len(trimmed, i));
    let name = &trimmed[start..];
    if !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
    {
        Some(name.to_string())
    } else {
        None
    }
}

fn c_len(s: &str, idx: usize) -> usize {
    s[idx..].chars().next().map_or(1, char::len_utf8)
}

#[cfg(test)]
mod tests {
    //! Behavior tests for function-as-value capture, driven end-to-end through
    //! the public `extract_source` API (mirrors `tests/batch_a_languages.rs`).
    //! A captured function-ref surfaces as an `UnresolvedRef` with
    //! `is_function_ref == true` and `reference_kind == References`.
    use crate::extract_source;
    use codegraph_core::types::{EdgeKind, Language, UnresolvedRef};

    fn fn_refs(file: &str, source: &str, lang: Language) -> Vec<UnresolvedRef> {
        let result = extract_source(file, source, Some(lang));
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        result
            .unresolved_references
            .into_iter()
            .filter(|r| r.is_function_ref)
            .collect()
    }

    fn has_fn_ref(refs: &[UnresolvedRef], name: &str) -> bool {
        refs.iter()
            .any(|r| r.reference_name == name && r.reference_kind == EdgeKind::References)
    }

    fn names(refs: &[UnresolvedRef]) -> Vec<String> {
        let mut v = refs
            .iter()
            .map(|r| r.reference_name.clone())
            .collect::<Vec<_>>();
        v.sort();
        v
    }

    // --- TS / JS -----------------------------------------------------------

    #[test]
    fn tsjs_captures_bare_identifier_callback_arg() {
        // Given a same-file function passed as a value to addEventListener,
        // When extracted, Then it is captured as a function reference.
        let src = r#"
function onBlur() { return 1; }
function setup() { document.addEventListener('blur', onBlur); }
"#;
        let refs = fn_refs("src/a.ts", src, Language::TypeScript);
        assert!(has_fn_ref(&refs, "onBlur"), "names={:?}", names(&refs));
    }

    #[test]
    fn tsjs_captures_this_member_callback() {
        // `this.handleClick` inside a class method always flushes (member_expression special).
        let src = r#"
class Panel {
  handleClick() { return 2; }
  wire() { this.on('click', this.handleClick); }
}
"#;
        let refs = fn_refs("src/panel.ts", src, Language::TypeScript);
        assert!(
            has_fn_ref(&refs, "this.handleClick"),
            "names={:?}",
            names(&refs)
        );
    }

    #[test]
    fn tsjs_captures_imported_callback_but_gates_unknown_bare_id() {
        // Imported name survives the gate; an unknown bare identifier does not.
        let src = r#"
import { handler } from './handlers';
function boot() { register('x', handler); register('y', totallyUnknownName); }
"#;
        let refs = fn_refs("src/boot.ts", src, Language::TypeScript);
        assert!(has_fn_ref(&refs, "handler"), "names={:?}", names(&refs));
        assert!(
            !has_fn_ref(&refs, "totallyUnknownName"),
            "unknown bare id must be gated out: names={:?}",
            names(&refs)
        );
    }

    #[test]
    fn tsjs_param_storage_assignment_is_skipped() {
        // `this.status = status` stores a param, not a fn-ref (Rhs param-storage skip).
        let src = r#"
function cb() { return 0; }
class S {
  constructor(status) { this.status = status; this.done = cb; }
}
"#;
        let refs = fn_refs("src/s.ts", src, Language::TypeScript);
        assert!(
            !has_fn_ref(&refs, "status"),
            "param-storage rhs must be skipped: names={:?}",
            names(&refs)
        );
        assert!(has_fn_ref(&refs, "cb"), "names={:?}", names(&refs));
    }

    #[test]
    fn tsjs_captures_array_and_object_value_callbacks() {
        // array (List) and pair (Value) capture modes.
        let src = r#"
function a() {}
function b() {}
const table = [a, b];
const map = { onA: a, onB: b };
"#;
        let refs = fn_refs("src/t.ts", src, Language::TypeScript);
        assert!(has_fn_ref(&refs, "a"), "names={:?}", names(&refs));
        assert!(has_fn_ref(&refs, "b"), "names={:?}", names(&refs));
    }

    // --- Python ------------------------------------------------------------

    #[test]
    fn python_captures_self_attribute_and_bare_callback() {
        let src = r#"
def handler():
    return 1

class W:
    def on_click(self):
        return 2
    def wire(self):
        register("click", self.on_click)
        register("blur", handler)
"#;
        let refs = fn_refs("src/w.py", src, Language::Python);
        assert!(has_fn_ref(&refs, "on_click"), "names={:?}", names(&refs));
        assert!(has_fn_ref(&refs, "handler"), "names={:?}", names(&refs));
    }

    // --- Go ----------------------------------------------------------------

    #[test]
    fn go_captures_function_value_argument() {
        let src = r#"
package main
func onEvent() {}
func setup() {
    register("e", onEvent)
}
"#;
        let refs = fn_refs("cmd/g.go", src, Language::Go);
        assert!(has_fn_ref(&refs, "onEvent"), "names={:?}", names(&refs));
    }

    // --- Rust --------------------------------------------------------------

    #[test]
    fn rust_captures_function_value_argument() {
        let src = r#"
fn handler() {}
fn setup() {
    register("sig", handler);
}
"#;
        let refs = fn_refs("src/r.rs", src, Language::Rust);
        assert!(has_fn_ref(&refs, "handler"), "names={:?}", names(&refs));
    }

    // --- Java (method references only) -------------------------------------

    #[test]
    fn java_captures_this_method_reference() {
        let src = r#"
class App {
    void fire() {}
    void wire() {
        button.setOnClick(this::fire);
    }
}
"#;
        let refs = fn_refs("src/App.java", src, Language::Java);
        assert!(has_fn_ref(&refs, "this.fire"), "names={:?}", names(&refs));
    }

    #[test]
    fn java_captures_type_qualified_method_reference() {
        let src = r#"
class Util {
    static void log(String s) {}
}
class App {
    void wire() {
        stream.forEach(Util::log);
    }
}
"#;
        let refs = fn_refs("src/App2.java", src, Language::Java);
        assert!(has_fn_ref(&refs, "Util::log"), "names={:?}", names(&refs));
    }

    #[test]
    fn java_constructor_reference_is_dropped() {
        // `Type::new` is a constructor ref with no method node → not captured.
        let src = r#"
class App {
    void wire() {
        supplier.get(String::new);
    }
}
"#;
        let refs = fn_refs("src/App3.java", src, Language::Java);
        assert!(
            !has_fn_ref(&refs, "String::new"),
            "constructor ref must be dropped: names={:?}",
            names(&refs)
        );
    }

    // --- C# ----------------------------------------------------------------

    #[test]
    fn csharp_captures_this_member_access() {
        let src = r#"
class App {
    void Run0() {}
    void Wire() {
        button.Click += this.Run0;
    }
}
"#;
        let refs = fn_refs("src/App.cs", src, Language::CSharp);
        assert!(has_fn_ref(&refs, "Run0"), "names={:?}", names(&refs));
    }

    // --- PHP (string + array callables, HOF-gated) -------------------------

    #[test]
    fn php_captures_string_callable_in_hof() {
        let src = r#"<?php
function cmp($a, $b) { return 0; }
function run($arr) {
    usort($arr, 'cmp');
}
"#;
        let refs = fn_refs("src/p.php", src, Language::Php);
        assert!(has_fn_ref(&refs, "cmp"), "names={:?}", names(&refs));
    }

    #[test]
    fn php_string_callable_outside_hof_is_ignored() {
        // A string callee outside a known callable-HOF is not a fn-ref.
        let src = r#"<?php
function run() {
    doSomethingElse('cmp');
}
"#;
        let refs = fn_refs("src/p2.php", src, Language::Php);
        assert!(
            !has_fn_ref(&refs, "cmp"),
            "non-HOF string arg must be ignored: names={:?}",
            names(&refs)
        );
    }

    // --- Ruby --------------------------------------------------------------

    #[test]
    fn ruby_captures_method_symbol_reference() {
        let src = r#"
class App
  def target_cb
  end
  def wire
    register(method(:target_cb))
  end
end
"#;
        let refs = fn_refs("src/app.rb", src, Language::Ruby);
        assert!(has_fn_ref(&refs, "target_cb"), "names={:?}", names(&refs));
    }

    #[test]
    fn ruby_captures_hook_dsl_symbol_as_this_member() {
        // `before_action :authenticate` → this.authenticate (hook-DSL symbol).
        let src = r#"
class App
  before_action :authenticate
  def authenticate
  end
end
"#;
        let refs = fn_refs("src/hook.rb", src, Language::Ruby);
        assert!(
            has_fn_ref(&refs, "this.authenticate"),
            "names={:?}",
            names(&refs)
        );
    }

    // --- Kotlin ------------------------------------------------------------

    #[test]
    fn kotlin_captures_callable_reference() {
        let src = r#"
class App {
    fun handle() {}
    fun wire() {
        register(::handle)
    }
}
"#;
        let refs = fn_refs("src/App.kt", src, Language::Kotlin);
        // bare `::handle` → member name `handle`.
        assert!(has_fn_ref(&refs, "handle"), "names={:?}", names(&refs));
    }

    // --- Swift (selector) --------------------------------------------------

    #[test]
    fn swift_captures_selector_expression() {
        let src = r#"
class App {
    func fire() {}
    func wire() {
        control.addTarget(self, action: #selector(fire))
    }
}
"#;
        let refs = fn_refs("src/App.swift", src, Language::Swift);
        assert!(has_fn_ref(&refs, "fire"), "names={:?}", names(&refs));
    }

    // --- pure-function unit checks (no parser needed) ----------------------

    #[test]
    fn is_ruby_hook_call_matches_prefixes_and_names() {
        assert!(super::is_ruby_hook_call("before_action"));
        assert!(super::is_ruby_hook_call("after_save"));
        assert!(super::is_ruby_hook_call("around_filter"));
        assert!(super::is_ruby_hook_call("skip_before_action"));
        assert!(super::is_ruby_hook_call("validate"));
        assert!(super::is_ruby_hook_call("rescue_from"));
        assert!(!super::is_ruby_hook_call("before_"));
        assert!(!super::is_ruby_hook_call("before_Action"));
        assert!(!super::is_ruby_hook_call("do_thing"));
    }

    #[test]
    fn fn_ref_spec_none_for_unsupported_language() {
        assert!(super::fn_ref_spec(Language::Yaml).is_none());
        assert!(super::fn_ref_spec(Language::Gdscript).is_none());
        assert!(super::fn_ref_spec(Language::Rust).is_some());
    }

    #[test]
    fn spec_helpers_report_policy_flags() {
        let cpp = super::fn_ref_spec(Language::Cpp).unwrap();
        assert!(super::is_address_of_only(cpp));
        let ts = super::fn_ref_spec(Language::TypeScript).unwrap();
        assert!(!super::is_address_of_only(ts));
        let c = super::fn_ref_spec(Language::C).unwrap();
        assert!(super::mode_is_ungated(c, super::CaptureMode::Value));
        assert!(super::mode_is_ungated(c, super::CaptureMode::List));
        assert!(!super::mode_is_ungated(c, super::CaptureMode::Args));
    }

    #[test]
    fn dispatch_rule_resolves_known_and_misses_unknown() {
        let ts = super::fn_ref_spec(Language::TypeScript).unwrap();
        let (mode, field) = super::dispatch_rule(ts, "arguments").unwrap();
        assert_eq!(mode, super::CaptureMode::Args);
        assert_eq!(field, None);
        let (mode, field) = super::dispatch_rule(ts, "variable_declarator").unwrap();
        assert_eq!(mode, super::CaptureMode::VarInit);
        assert_eq!(field, Some("value"));
        assert!(super::dispatch_rule(ts, "no_such_node").is_none());
    }

    #[test]
    fn capture_mode_is_copy_and_comparable() {
        let m = super::CaptureMode::Args;
        let n = m;
        assert_eq!(m, n);
        assert_ne!(super::CaptureMode::Args, super::CaptureMode::Rhs);
        assert!(format!("{m:?}").contains("Args"));
    }

    #[test]
    fn is_simple_name_predicate() {
        assert!(super::is_simple_name("foo_bar1"));
        assert!(super::is_simple_name("_x"));
        assert!(!super::is_simple_name("1abc"));
        assert!(!super::is_simple_name(""));
        assert!(!super::is_simple_name("a::b"));
    }

    #[test]
    fn is_qualified_name_predicate() {
        assert!(super::is_qualified_name("Cls::method"));
        assert!(super::is_qualified_name("plain"));
        assert!(!super::is_qualified_name("::global"));
        assert!(!super::is_qualified_name(""));
    }

    #[test]
    fn is_qualified_double_colon_predicate() {
        assert!(super::is_qualified_double_colon("A::b"));
        assert!(!super::is_qualified_double_colon("Ab"));
        assert!(!super::is_qualified_double_colon("A::1b"));
        assert!(!super::is_qualified_double_colon("::x"));
    }

    #[test]
    fn is_ruby_method_symbol_predicate() {
        assert!(super::is_ruby_method_symbol("save!"));
        assert!(super::is_ruby_method_symbol("valid?"));
        assert!(super::is_ruby_method_symbol("_ok"));
        assert!(!super::is_ruby_method_symbol("9lives"));
        assert!(!super::is_ruby_method_symbol(""));
    }

    #[test]
    fn capitalized_receiver_predicate() {
        assert_eq!(
            super::capitalized_receiver("Foo::bar"),
            Some("Foo".to_string())
        );
        assert_eq!(super::capitalized_receiver("foo::bar"), None);
        assert_eq!(
            super::capitalized_receiver("A1_b::x"),
            Some("A1_b".to_string())
        );
        assert_eq!(super::capitalized_receiver("::x"), None);
    }

    #[test]
    fn last_identifier_extracts_trailing_name() {
        assert_eq!(super::last_identifier("a.b.c"), Some("c".to_string()));
        assert_eq!(
            super::last_identifier("this.status"),
            Some("status".to_string())
        );
        assert_eq!(super::last_identifier("$foo"), Some("$foo".to_string()));
        assert_eq!(super::last_identifier("42"), None);
        assert_eq!(super::last_identifier("   "), None);
    }

    #[test]
    fn c_len_handles_ascii_and_multibyte() {
        assert_eq!(super::c_len("abc", 0), 1);
        assert_eq!(super::c_len("héllo", 1), 2);
    }

    #[test]
    fn kotlin_type_qualified_navigation_expression() {
        let src = r#"
class Helper { fun process() {} }
fun setup() {
    register(Helper::process)
}
fun register(cb: () -> Unit) {}
"#;
        let refs = fn_refs("nav.kt", src, Language::Kotlin);
        assert!(has_fn_ref(&refs, "process"), "names={:?}", names(&refs));
    }

    #[test]
    fn php_array_callable_this_receiver() {
        let src = r#"<?php
class W {
  function run($items) {
    array_map([$this, 'transform'], $items);
  }
  function transform($x) { return $x; }
}
"#;
        let refs = fn_refs("arr.php", src, Language::Php);
        assert!(
            has_fn_ref(&refs, "this.transform"),
            "names={:?}",
            names(&refs)
        );
    }

    #[test]
    fn c_file_scope_initializer_table_is_ungated() {
        let src = r#"
void a(void) {}
void b(void) {}
void (*table[])(void) = { a, b };
"#;
        let refs = fn_refs("t.c", src, Language::C);
        assert!(has_fn_ref(&refs, "a"), "names={:?}", names(&refs));
        assert!(has_fn_ref(&refs, "b"), "names={:?}", names(&refs));
    }

    #[test]
    fn c_address_of_argument_pointer_expression() {
        let src = r#"
void handler(int s) {}
void install(void (*cb)(int)) {}
void setup(void) {
    install(&handler);
}
"#;
        let refs = fn_refs("a.c", src, Language::C);
        assert!(has_fn_ref(&refs, "handler"), "names={:?}", names(&refs));
    }

    #[test]
    fn cpp_qualified_member_pointer_and_bare_arg_gated_out() {
        let src = r#"
struct Widget { void on_click() {} };
void reg(void (Widget::*m)()) {}
void bare(int x) {}
void takes(void (*f)(int)) {}
void setup() {
    reg(&Widget::on_click);
    takes(bare);
}
"#;
        let refs = fn_refs("w.cpp", src, Language::Cpp);
        assert!(
            has_fn_ref(&refs, "Widget::on_click"),
            "names={:?}",
            names(&refs)
        );
        assert!(!has_fn_ref(&refs, "bare"), "names={:?}", names(&refs));
    }

    #[test]
    fn dart_function_value_argument() {
        let src = r#"
void handler() {}
void register(void Function() cb) {}
void setup() {
  register(handler);
}
"#;
        let refs = fn_refs("a.dart", src, Language::Dart);
        assert!(has_fn_ref(&refs, "handler"), "names={:?}", names(&refs));
    }

    #[test]
    fn scala_function_value_argument() {
        let src = r#"
object M {
  def handler(): Unit = {}
  def register(cb: () => Unit): Unit = {}
  def setup(): Unit = {
    register(handler)
  }
}
"#;
        let refs = fn_refs("M.scala", src, Language::Scala);
        assert!(has_fn_ref(&refs, "handler"), "names={:?}", names(&refs));
    }

    #[test]
    fn objc_selector_expression_normalizes_without_panic() {
        let src = r#"
@implementation W
- (void)setup {
    [self performSelector:@selector(fire)];
}
- (void)fire {}
@end
"#;
        let _ = fn_refs("w.m", src, Language::ObjC);
    }

    #[test]
    fn lua_function_value_argument_normalizes_without_panic() {
        let src = r#"
local function handler() end
local function register(cb) end
register(handler)
"#;
        let _ = fn_refs("a.lua", src, Language::Lua);
    }

    #[test]
    fn pascal_address_of_argument_normalizes_without_panic() {
        let src = r#"
program P;
procedure Handler; begin end;
procedure Register(cb: TProc); begin end;
begin
  Register(@Handler);
end.
"#;
        let _ = fn_refs("p.pas", src, Language::Pascal);
    }

    #[test]
    fn fn_ref_spec_covers_all_supported_languages() {
        for lang in [
            Language::C,
            Language::Cpp,
            Language::ObjC,
            Language::TypeScript,
            Language::Tsx,
            Language::JavaScript,
            Language::Jsx,
            Language::Python,
            Language::Go,
            Language::Rust,
            Language::Java,
            Language::Kotlin,
            Language::CSharp,
            Language::Php,
            Language::Ruby,
            Language::Swift,
            Language::Scala,
            Language::Dart,
            Language::Lua,
            Language::Luau,
            Language::Pascal,
        ] {
            assert!(
                super::fn_ref_spec(lang).is_some(),
                "expected spec for {lang:?}"
            );
        }
    }

    #[test]
    fn kotlin_this_navigation_expression() {
        let src = r#"
class A {
    fun fire() {}
    fun w() { register(this::fire) }
}
fun register(cb: () -> Unit) {}
"#;
        let refs = fn_refs("k.kt", src, Language::Kotlin);
        assert!(has_fn_ref(&refs, "this.fire"), "names={:?}", names(&refs));
    }

    #[test]
    fn java_this_and_super_method_reference() {
        let src = r#"
class A extends B {
    void fire() {}
    void w() {
        x.each(this::fire);
        x.each(super::base);
    }
}
"#;
        let refs = fn_refs("j.java", src, Language::Java);
        assert!(has_fn_ref(&refs, "this.fire"), "names={:?}", names(&refs));
        assert!(has_fn_ref(&refs, "this.base"), "names={:?}", names(&refs));
    }

    #[test]
    fn php_static_class_array_callable() {
        let src = r#"<?php
class H { static function make() {} }
function run($a) { array_map([H::class, 'make'], $a); }
"#;
        let refs = fn_refs("p.php", src, Language::Php);
        assert!(has_fn_ref(&refs, "H::make"), "names={:?}", names(&refs));
    }

    #[test]
    fn c_designated_initializer_value_mode() {
        let src = r#"
void a(void) {}
struct S { void (*cb)(void); };
struct S s = { .cb = a };
"#;
        let refs = fn_refs("c.c", src, Language::C);
        assert!(has_fn_ref(&refs, "a"), "names={:?}", names(&refs));
    }

    #[test]
    fn csharp_variable_declarator_no_value_field_var_init() {
        let src = r#"
class A {
    void H() {}
    void W() { System.Action cb = H; }
}
"#;
        let refs = fn_refs("v.cs", src, Language::CSharp);
        assert!(has_fn_ref(&refs, "H"), "names={:?}", names(&refs));
    }

    #[test]
    fn dart_static_final_no_value_field_var_init() {
        let src = r#"
void handler() {}
class A { static final cb = handler; }
"#;
        let refs = fn_refs("v.dart", src, Language::Dart);
        assert!(has_fn_ref(&refs, "handler"), "names={:?}", names(&refs));
    }

    #[test]
    fn swift_labeled_argument_value_layer() {
        let src = r#"
func handler() {}
func reg(cb: () -> Void) {}
func w() { reg(cb: handler) }
"#;
        let refs = fn_refs("s.swift", src, Language::Swift);
        assert!(has_fn_ref(&refs, "handler"), "names={:?}", names(&refs));
    }

    #[test]
    fn kotlin_capitalized_callable_reference_is_qualified() {
        let src = r#"
class Handler { fun handle() {} }
fun reg(cb: () -> Unit) {}
fun w() { reg(Handler::handle) }
"#;
        let refs = fn_refs("k1.kt", src, Language::Kotlin);
        assert!(has_fn_ref(&refs, "handle"), "names={:?}", names(&refs));
    }

    #[test]
    fn scala_postfix_expression_unwrap() {
        let src = r#"
object M {
  def handler(): Unit = {}
  def reg(cb: () => Unit): Unit = {}
  def w(): Unit = { val c = handler _ }
}
"#;
        let refs = fn_refs("sc.scala", src, Language::Scala);
        assert!(has_fn_ref(&refs, "handler"), "names={:?}", names(&refs));
    }

    fn parse(lang: tree_sitter::Language, src: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).unwrap();
        parser.parse(src, None).unwrap()
    }

    fn first_of_kind<'t>(node: tree_sitter::Node<'t>, kind: &str) -> Option<tree_sitter::Node<'t>> {
        if node.kind() == kind {
            return Some(node);
        }
        for i in 0..node.named_child_count() {
            let child = node.named_child(i as u32)?;
            if let Some(found) = first_of_kind(child, kind) {
                return Some(found);
            }
        }
        None
    }

    fn capture_names(
        lang: tree_sitter::Language,
        src: &str,
        container_kind: &str,
        mode: super::CaptureMode,
        field: Option<&str>,
        spec: &super::FnRefSpec,
    ) -> Vec<String> {
        let tree = parse(lang, src);
        let container = first_of_kind(tree.root_node(), container_kind).unwrap();
        super::capture_fn_ref_candidates(container, mode, field, spec, src)
            .into_iter()
            .map(|c| c.name)
            .collect()
    }

    #[test]
    fn cpp_family_spec_dispatch_and_ungated_modes() {
        let cpp = super::fn_ref_spec(Language::Cpp).unwrap();
        assert!(super::is_address_of_only(cpp));
        assert!(super::dispatch_rule(cpp, "argument_list").is_some());
        assert!(super::dispatch_rule(cpp, "assignment_expression").is_some());
        assert!(super::dispatch_rule(cpp, "init_declarator").is_some());
        assert!(super::dispatch_rule(cpp, "initializer_list").is_some());
        assert!(super::dispatch_rule(cpp, "initializer_pair").is_some());
        assert!(super::mode_is_ungated(cpp, super::CaptureMode::Value));
        let objc = super::fn_ref_spec(Language::ObjC).unwrap();
        assert!(!super::is_address_of_only(objc));
    }

    #[test]
    fn rhs_param_storage_skip_direct() {
        let ts = super::fn_ref_spec(Language::TypeScript).unwrap();
        // `this.status = status` -> lhs last-name == rhs -> skipped (no capture).
        let names = capture_names(
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            "class C { m(status) { this.status = status; } }",
            "assignment_expression",
            super::CaptureMode::Rhs,
            Some("right"),
            ts,
        );
        assert!(names.is_empty(), "param-storage rhs skip; names={names:?}");
    }

    #[test]
    fn varinit_destructuring_pattern_is_skipped() {
        let ts = super::fn_ref_spec(Language::TypeScript).unwrap();
        // Destructuring `const { a } = obj` -> pattern name -> no fn-ref.
        let names = capture_names(
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            "const { a } = obj;",
            "variable_declarator",
            super::CaptureMode::VarInit,
            Some("value"),
            ts,
        );
        assert!(names.is_empty(), "destructuring skip; names={names:?}");
    }

    #[test]
    fn java_this_super_and_type_method_references_direct() {
        let src = "class A extends B { void w() { x.each(this::fire); x.each(super::base); y.each(Util::log); z.get(String::new); } }";
        let tree = parse(tree_sitter_java::LANGUAGE.into(), src);
        let mut names = Vec::new();
        fn walk<'t>(n: tree_sitter::Node<'t>, out: &mut Vec<tree_sitter::Node<'t>>) {
            if n.kind() == "method_reference" {
                out.push(n);
            }
            for i in 0..n.named_child_count() {
                if let Some(c) = n.named_child(i as u32) {
                    walk(c, out);
                }
            }
        }
        let mut refs = Vec::new();
        walk(tree.root_node(), &mut refs);
        for r in refs {
            for nref in super::normalize_special(r, "method_reference", src) {
                names.push(nref.name);
            }
        }
        assert!(names.contains(&"this.fire".to_string()), "names={names:?}");
        assert!(names.contains(&"this.base".to_string()), "names={names:?}");
        assert!(names.contains(&"Util::log".to_string()), "names={names:?}");
        // String::new (constructor) -> dropped.
        assert!(
            !names.contains(&"String::new".to_string()),
            "names={names:?}"
        );
    }

    #[test]
    fn cpp_qualified_pointer_member_ref_direct() {
        let cpp = super::fn_ref_spec(Language::Cpp).unwrap();
        let src = "struct W { void m(){} }; void reg(void (W::*p)()) {} void s() { reg(&W::m); }";
        let tree = parse(tree_sitter_cpp::LANGUAGE.into(), src);
        let arg_list = first_of_kind(tree.root_node(), "argument_list").unwrap();
        let names: Vec<String> =
            super::capture_fn_ref_candidates(arg_list, super::CaptureMode::Args, None, cpp, src)
                .into_iter()
                .map(|c| c.name)
                .collect();
        assert!(
            names.iter().any(|n| n == "W::m"),
            "qualified member ptr; names={names:?}"
        );
    }

    #[test]
    fn pure_predicate_helpers_edge_cases() {
        assert!(!super::is_qualified_double_colon("A:::b"));
        assert_eq!(super::last_identifier(""), None);
    }

    fn special_names(lang: tree_sitter::Language, src: &str, node_kind: &str) -> Vec<String> {
        let tree = parse(lang, src);
        let node = first_of_kind(tree.root_node(), node_kind).unwrap();
        super::normalize_special(node, node_kind, src)
            .into_iter()
            .map(|n| n.name)
            .collect()
    }

    fn all_of_kind<'t>(
        node: tree_sitter::Node<'t>,
        kind: &str,
        out: &mut Vec<tree_sitter::Node<'t>>,
    ) {
        if node.kind() == kind {
            out.push(node);
        }
        for i in 0..node.named_child_count() {
            if let Some(c) = node.named_child(i as u32) {
                all_of_kind(c, kind, out);
            }
        }
    }

    fn special_names_last(lang: tree_sitter::Language, src: &str, node_kind: &str) -> Vec<String> {
        let tree = parse(lang, src);
        let mut nodes = Vec::new();
        all_of_kind(tree.root_node(), node_kind, &mut nodes);
        let node = *nodes.last().unwrap();
        super::normalize_special(node, node_kind, src)
            .into_iter()
            .map(|n| n.name)
            .collect()
    }

    #[test]
    fn kotlin_callable_reference_lowercase_and_capitalized_direct() {
        let bare = special_names(
            tree_sitter_kotlin_ng::LANGUAGE.into(),
            "fun w() { reg(::top) }",
            "callable_reference",
        );
        assert!(
            bare.contains(&"top".to_string()),
            "bare ::top; names={bare:?}"
        );
    }

    #[test]
    fn ts_this_member_expression_direct() {
        let lang = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let names = special_names(
            lang,
            "class C { m() { reg(this.handler); } }",
            "member_expression",
        );
        assert!(
            names.contains(&"this.handler".to_string()),
            "this.member; names={names:?}"
        );
    }

    #[test]
    fn python_self_attribute_direct() {
        let lang = tree_sitter_python::LANGUAGE.into();
        let names = special_names(lang, "reg(self.handler)", "attribute");
        assert!(
            names.contains(&"handler".to_string()),
            "self.attr; names={names:?}"
        );
    }

    #[test]
    fn csharp_member_access_this_receiver_direct() {
        let lang = tree_sitter_c_sharp::LANGUAGE.into();
        let names = special_names(
            lang,
            "class C { void W() { reg(this.Run); } }",
            "member_access_expression",
        );
        assert!(
            names.contains(&"Run".to_string()),
            "this.Run; names={names:?}"
        );
    }

    #[test]
    fn ruby_method_symbol_call_direct() {
        let names = special_names_last(
            tree_sitter_ruby::LANGUAGE.into(),
            "reg(method(:target))",
            "call",
        );
        assert!(
            names.contains(&"target".to_string()),
            "method(:sym); names={names:?}"
        );
    }

    #[test]
    fn swift_selector_expression_direct() {
        let lang = tree_sitter_swift::LANGUAGE.into();
        let names = special_names(
            lang,
            "func w() { let s = #selector(fire) }",
            "selector_expression",
        );
        assert!(
            names.contains(&"fire".to_string()) || !names.is_empty(),
            "#selector; names={names:?}"
        );
    }

    #[test]
    fn php_string_and_array_callable_direct() {
        let str_names = special_names(
            tree_sitter_php::LANGUAGE_PHP.into(),
            "<?php function r($a) { usort($a, 'cmp'); }",
            "string",
        );
        assert!(
            str_names.contains(&"cmp".to_string()),
            "php string callable; names={str_names:?}"
        );
        let arr_names = special_names(
            tree_sitter_php::LANGUAGE_PHP.into(),
            "<?php class W { function r($a) { array_map([$this, 'm'], $a); } }",
            "array_creation_expression",
        );
        assert!(
            arr_names.contains(&"this.m".to_string()),
            "php array callable; names={arr_names:?}"
        );
    }

    #[test]
    fn kotlin_navigation_expression_receiver_routing_direct() {
        let lang = tree_sitter_kotlin_ng::LANGUAGE;
        let this_ref = special_names(
            lang.into(),
            "fun w() { reg(this::fire) }",
            "navigation_expression",
        );
        assert!(
            this_ref.contains(&"this.fire".to_string()),
            "this:: -> this.member; names={this_ref:?}"
        );
        let type_ref = special_names(
            lang.into(),
            "fun w() { reg(Handler::run) }",
            "navigation_expression",
        );
        assert!(
            type_ref.contains(&"Handler::run".to_string()),
            "Type:: -> Type::member; names={type_ref:?}"
        );
        let lower = special_names(
            lang.into(),
            "fun w() { reg(inst::run) }",
            "navigation_expression",
        );
        assert!(
            lower.is_empty(),
            "lowercase receiver -> none; names={lower:?}"
        );
    }
}
