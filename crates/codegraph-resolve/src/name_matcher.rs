//! Name matching for reference resolution.
//!
//! Ports `upstream resolution/name-matcher.ts`. Match semantics —
//! `lower(name)` case handling, candidate ranking, ambiguity behavior, and the
//! cross-language family gate — mirror the upstream exactly. Every strategy cites its
//! upstream source range.

use crate::member_types::TYPE_ARGS;
use crate::object_literal::{LiteralLookup, object_literal_property, property_contains};
use crate::source_facts::{SourceFacts, TsFieldDeclaration};
use crate::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::types::{
    RefView, ResolutionContext, ResolvedBy, ResolvedRef, declares_type_name, is_esm_language,
    node_is_eligible_target,
};
use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use std::borrow::Borrow;
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

/// Ceiling on same-name candidates a proximity-SCORED strategy will rank
/// (`AMBIGUOUS_NAME_CEILING`, upstream name-matcher; override
/// `CODEGRAPH_AMBIGUOUS_NAME_CEILING`). Above it the scored strategies DECLINE
/// rather than degrade to O(K²). ONLY `match_fuzzy` and the multi-candidate
/// branch of `match_by_exact_name` consult it; edge-producing strategies are
/// never gated, so resolved golden edges are unchanged.
const AMBIGUOUS_NAME_CEILING: usize = 500;

fn ambiguous_name_ceiling() -> usize {
    static CEILING: OnceLock<usize> = OnceLock::new();
    *CEILING.get_or_init(|| {
        std::env::var("CODEGRAPH_AMBIGUOUS_NAME_CEILING")
            .ok()
            .and_then(|raw| raw.trim().parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(AMBIGUOUS_NAME_CEILING)
    })
}

/// Try to resolve a path-like reference by matching the filename against file
/// nodes (`matchByFilePath`, `name-matcher.ts:14-77`).
pub fn match_by_file_path(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    // Path-like (`a/b.liquid`) OR a bare filename ending in a short extension
    // (`Foo.h`). A bare ref WITHOUT an extension is a symbol, not a file
    // (name-matcher.ts:18-24).
    if !reference.reference_name.contains('/') && !has_short_extension(&reference.reference_name) {
        return None;
    }

    let file_name = reference.reference_name.rsplit('/').next()?;
    if file_name.is_empty() {
        return None;
    }

    let candidates = context.get_nodes_by_name_shared(file_name);
    let file_nodes: Vec<Arc<Node>> = candidates
        .into_iter()
        .filter(|n| n.kind == NodeKind::File)
        .collect();
    if file_nodes.is_empty() {
        return None;
    }

    // Prefer exact path match on qualified_name (name-matcher.ts:37-45).
    if let Some(exact) = file_nodes.iter().find(|n| {
        n.qualified_name == reference.reference_name || n.file_path == reference.reference_name
    }) {
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: exact.id.clone(),
            confidence: 0.95,
            resolved_by: ResolvedBy::FilePath,
        });
    }

    // Suffix match, picking the closest file node (name-matcher.ts:54-64).
    let suffix_matches: Vec<&Node> = file_nodes
        .iter()
        .map(Arc::as_ref)
        .filter(|n| {
            n.qualified_name.ends_with(&reference.reference_name)
                || n.file_path.ends_with(&reference.reference_name)
        })
        .collect();
    if !suffix_matches.is_empty() {
        let chosen = pick_closest_file_node(&suffix_matches, reference);
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: chosen.id.clone(),
            confidence: 0.85,
            resolved_by: ResolvedBy::FilePath,
        });
    }

    // Single same-named file node, lower confidence (name-matcher.ts:67-74).
    if file_nodes.len() == 1 {
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: file_nodes[0].id.clone(),
            confidence: 0.7,
            resolved_by: ResolvedBy::FilePath,
        });
    }

    None
}

/// Matches `/\.[A-Za-z][A-Za-z0-9]{0,3}$/` (name-matcher.ts:22).
fn has_short_extension(name: &str) -> bool {
    let Some(dot) = name.rfind('.') else {
        return false;
    };
    let ext = &name[dot + 1..];
    let bytes = ext.as_bytes();
    if bytes.is_empty() || bytes.len() > 4 {
        return false;
    }
    if !bytes[0].is_ascii_alphabetic() {
        return false;
    }
    bytes[1..].iter().all(u8::is_ascii_alphanumeric)
}

/// Pick the file node closest to the referencing file (`pickClosestFileNode`,
/// `name-matcher.ts:86-106`).
fn pick_closest_file_node<'a>(candidates: &[&'a Node], reference: &RefView) -> &'a Node {
    let ref_dir = dir_of(&reference.file_path);
    let same_dir: Vec<&&Node> = candidates
        .iter()
        .filter(|c| dir_of(&c.file_path) == ref_dir)
        .collect();
    let pool: Vec<&Node> = if !same_dir.is_empty() {
        same_dir.into_iter().copied().collect()
    } else {
        candidates.to_vec()
    };

    let mut best = pool[0];
    let mut best_score = f64::NEG_INFINITY;
    for c in &pool {
        let score = compute_path_proximity(&reference.file_path, &c.file_path)
            + if same_language_family(c.language, reference.language) {
                5.0
            } else {
                0.0
            };
        if score > best_score {
            best_score = score;
            best = c;
        }
    }
    best
}

fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

/// Language families that share a type system / runtime (`LANGUAGE_FAMILY`,
/// upstream v1.6.1): C, C++, Objective-C and Swift interoperate natively; ArkTS
/// and the single-file-component languages are web.
fn language_family(lang: Language) -> Option<&'static str> {
    match lang {
        Language::Java | Language::Kotlin | Language::Scala => Some("jvm"),
        Language::Swift | Language::ObjC | Language::C | Language::Cpp => Some("native"),
        Language::TypeScript
        | Language::Tsx
        | Language::JavaScript
        | Language::Jsx
        | Language::ArkTs
        | Language::Svelte
        | Language::Vue
        | Language::Astro => Some("web"),
        Language::CSharp | Language::Razor => Some("dotnet"),
        Language::Cfml => Some("cfml"),
        _ => None,
    }
}

/// `sameLanguageFamily`.
pub fn same_language_family(a: Language, b: Language) -> bool {
    if a == b {
        return true;
    }
    match language_family(a) {
        Some(fa) => language_family(b) == Some(fa),
        None => false,
    }
}

/// The code family of `lang` (`CODE_FAMILY`): a multi-language family, or a
/// singleton for every other programming language. Config and markup formats
/// have none, so their transitions stay open.
fn code_family(lang: Language) -> Option<&'static str> {
    language_family(lang).or(match lang {
        Language::Python => Some("python"),
        Language::Go => Some("go"),
        Language::Rust => Some("rust"),
        Language::Php => Some("php"),
        Language::Ruby => Some("ruby"),
        Language::Dart => Some("dart"),
        Language::Lua | Language::Luau => Some("lua"),
        Language::R => Some("r"),
        Language::Erlang => Some("erlang"),
        Language::Pascal => Some("pascal"),
        Language::Solidity => Some("solidity"),
        Language::Nix => Some("nix"),
        _ => None,
    })
}

/// Whether a reference from `a` to `b` crosses between two code families
/// (`crossesCodeBoundary`).
pub fn crosses_code_boundary(a: Language, b: Language) -> bool {
    matches!((code_family(a), code_family(b)), (Some(fa), Some(fb)) if fa != fb)
}

/// Cross-family name matches need an actual ABI boundary (`hasBridgeEvidence`):
/// a native caller reaching a cgo `//export` or a Rust `pub extern "C" fn`, or
/// a Go `C.name` / Rust `extern "C" { fn name(…) }` caller reaching a C or C++
/// function. The evidence is scoped to the named free function.
fn has_bridge_evidence(
    candidate: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    if reference.reference_kind != EdgeKind::Calls || candidate.kind != NodeKind::Function {
        return false;
    }
    let name = regex::escape(&candidate.name);
    static CGO_IMPORT: OnceLock<Regex> = OnceLock::new();
    let cgo_import = || {
        CGO_IMPORT
            .get_or_init(|| Regex::new(r#"(?-u:\b)import\s+(?:\(\s*)?"C""#).expect("cgo import"))
    };
    if code_family(reference.language) == Some("native") {
        let Some(facts) = context.source_facts(&candidate.file_path) else {
            return false;
        };
        if candidate.language == Language::Go {
            let start = (candidate.start_line.max(2) - 2) as usize;
            let end =
                (candidate.end_line.max(candidate.start_line) as usize).min(facts.line_count());
            let declaration = if start < end {
                facts.join_lines(start, end, "\n")
            } else {
                String::new()
            };
            return cgo_import()
                .is_match(&strip_comments_for_regex(facts.source(), CommentLang::Go))
                && Regex::new(&format!(r"(?m)^//export {name}\r?\nfunc {name}\s*\("))
                    .is_ok_and(|export| export.is_match(&declaration));
        }
        if candidate.language == Language::Rust {
            let start = (candidate.start_line.max(1) - 1) as usize;
            let end =
                (candidate.end_line.max(candidate.start_line) as usize).min(facts.line_count());
            let declaration = if start < end {
                facts.join_lines(start, end, "\n")
            } else {
                String::new()
            };
            return Regex::new(&format!(
                r#"(?-u:\b)pub\s+extern\s+"C"\s+fn\s+{name}(?-u:\b)"#
            ))
            .is_ok_and(|export| {
                export.is_match(&strip_comments_for_regex(&declaration, CommentLang::Rust))
            });
        }
    }
    if matches!(candidate.language, Language::C | Language::Cpp) {
        let Some(facts) = context.source_facts(&reference.file_path) else {
            return false;
        };
        if reference.language == Language::Go {
            return cgo_import()
                .is_match(&strip_comments_for_regex(facts.source(), CommentLang::Go))
                && reference.reference_name == format!("C.{}", candidate.name);
        }
        if reference.language == Language::Rust {
            return Regex::new(&format!(
                r#"extern\s+"C"\s*\{{[^}}]*(?-u:\b)fn\s+{name}\s*\("#
            ))
            .is_ok_and(|block| {
                block.is_match(&strip_comments_for_regex(facts.source(), CommentLang::Rust))
            });
        }
    }
    false
}

/// Reject a chosen result that crosses a code-family boundary without bridge
/// evidence (`gateLanguageMatch`). It never shrinks a candidate pool or tries
/// a replacement, so no lone survivor is manufactured.
pub(crate) fn gate_language_match(
    result: Option<ResolvedRef>,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let result = result?;
    let Some(target) = context.get_node_by_id_shared(&result.target_node_id) else {
        return Some(result);
    };
    if crosses_code_boundary(reference.language, target.language)
        && !has_bridge_evidence(&target, reference, context)
    {
        return None;
    }
    // Rust-only (upstream has no Godot support): a scene, resource, or
    // project file's path references into scripts stay unresolved rows, which
    // impact/affected/audit consume path-keyed (F3). Name matching must not
    // turn them into edges through the open config→code transition.
    if reference.reference_kind == EdgeKind::References
        && matches!(
            reference.language,
            Language::GodotScene | Language::GodotResource | Language::GodotProject
        )
        && target.language != reference.language
    {
        return None;
    }
    Some(result)
}

/// Type/value references keep same-family eligibility in the candidate pool:
/// a native namesake must not hide the actual web type. Calls, imports and
/// inheritance are gated on the chosen result instead.
fn apply_language_gate<T: Borrow<Node>>(candidates: Vec<T>, reference: &RefView) -> Vec<T> {
    match reference.reference_kind {
        EdgeKind::References => candidates
            .into_iter()
            .filter(|c| same_language_family(c.borrow().language, reference.language))
            .collect(),
        _ => candidates,
    }
}

/// A function nested inside another FUNCTION is only callable from within its
/// container — Python, JS/TS and every closure language scope it lexically.
/// Resolving a bare name from elsewhere to a nested local fabricates an edge
/// scope already rules out: `join(...)` in one function must never bind to a
/// `join` defined inside a DIFFERENT function (`isLexicallyReachable`, #1230 /
/// upstream `c472cfb`).
///
/// A candidate that a same-file function/method ENCLOSES, found by walking out
/// through its `qualified_name` ([`lexical_scope_of`]), is kept only when the
/// ref originates inside that function's line range. A method of a top-level
/// class is unaffected, as are top-level symbols and C++ namespace-prefixed
/// names (the prefix has no node); a method of a class declared inside a
/// function is scoped by that function (upstream #2125).
pub(crate) fn is_lexically_reachable(
    candidate: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    // An object literal's member is reached through its object (upstream
    // #2363): a bare `load()` or `setTimeout(load)` never means `App.load`,
    // whatever shares the name.
    if candidate.kind == NodeKind::Function
        && is_simple_js_identifier(&reference.reference_name)
        && !crate::object_owners::is_object_member_reachable_by_name(candidate, reference, context)
    {
        return false;
    }
    if crate::object_owners::is_unreachable_path_holder(candidate, reference) {
        return false;
    }
    // A `val` / `const` declared in a function body is that body's alone, and
    // a Scala `val` filed under its class but written in a block of the
    // class body that block's (upstream #2193).
    if matches!(candidate.kind, NodeKind::Variable | NodeKind::Constant)
        || (candidate.kind == NodeKind::Field && candidate.language == Language::Scala)
    {
        return local_declaration_scope(candidate, context).is_none_or(|(start, end)| {
            reference.file_path == candidate.file_path
                && reference.line >= start
                && reference.line <= end
        });
    }
    // A function, or a type (`case class B()` in a test method), or a
    // method of such a type, declared inside a function is only in scope in
    // there (upstream #2123, #2125).
    if !matches!(
        candidate.kind,
        NodeKind::Function
            | NodeKind::Method
            | NodeKind::Class
            | NodeKind::Struct
            | NodeKind::Enum
            | NodeKind::Interface
            | NodeKind::Trait
            | NodeKind::TypeAlias
    ) {
        return true;
    }
    // C and C++ have no nested named functions: a function the graph shows
    // inside another is an extraction artifact (a macro whose designated
    // initializers derail tree-sitter-c error recovery), not a scope, so it
    // stays reachable (`NO_NESTED_FUNCTIONS`, upstream #1708).
    if matches!(candidate.language, Language::C | Language::Cpp) {
        return true;
    }
    lexical_scope_of(candidate, context).is_none_or(|(start, end)| {
        reference.file_path == candidate.file_path
            && reference.line >= start
            && reference.line <= end
    })
}

/// The lines a variable declaration is in scope for when it is local
/// (upstream #2193 `localDeclarationScope`): the innermost function or
/// method of its file whose lines hold its start, or, for a Scala `val` the
/// graph files under its class but written inside a block of the class body,
/// that block. `None` for a declaration at file, class or object level. C and
/// C++ nesting is not trusted (see [`is_lexically_reachable`]), and a Lua
/// global assigned inside a function is still global.
pub(crate) fn local_declaration_scope(
    candidate: &Node,
    context: &dyn ResolutionContext,
) -> Option<(i64, i64)> {
    if matches!(candidate.language, Language::C | Language::Cpp) {
        return None;
    }
    let compute = || {
        if matches!(candidate.language, Language::Lua | Language::Luau)
            && !is_lua_local(candidate, context)
        {
            return None;
        }
        let mut scope: Option<(i64, i64)> = None;
        for n in context
            .get_nodes_in_file_shared(&candidate.file_path)
            .iter()
        {
            if !matches!(n.kind, NodeKind::Function | NodeKind::Method) || n.id == candidate.id {
                continue;
            }
            if n.start_line > candidate.start_line
                || n.end_line < candidate.start_line
                || n.start_line == n.end_line
            {
                continue;
            }
            if n.start_line == candidate.start_line && n.start_column >= candidate.start_column {
                continue;
            }
            if scope.is_none_or(|(start, end)| n.end_line - n.start_line < end - start) {
                scope = Some((n.start_line, n.end_line));
            }
        }
        if scope.is_none()
            && candidate.kind == NodeKind::Field
            && candidate.language == Language::Scala
        {
            scope = crate::scala_scope::scala_block_of(candidate, context);
        }
        scope
    };
    match context.source_facts(&candidate.file_path) {
        Some(facts) => facts.local_declaration_scope(&candidate.id, compute),
        None => compute(),
    }
}

/// Whether a Lua declaration is a `local` one, read from its line (upstream
/// #2143 `isLuaLocal`, which #2193 reads).
pub(crate) fn is_lua_local(candidate: &Node, context: &dyn ResolutionContext) -> bool {
    if !matches!(
        candidate.kind,
        NodeKind::Variable | NodeKind::Constant | NodeKind::Function
    ) {
        return false;
    }
    let Some(facts) = context.source_facts(&candidate.file_path) else {
        return false;
    };
    let Some(index) = usize::try_from(candidate.start_line)
        .ok()
        .and_then(|line| line.checked_sub(1))
        .filter(|&index| index < facts.line_count())
    else {
        return false;
    };
    facts
        .line(index)
        .trim_start()
        .strip_prefix("local")
        .is_some_and(|rest| !rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_'))
}

/// Whether a same-file function or method encloses `candidate`, found by
/// walking out through its qualified name ([`lexical_scope_of`]).
pub(crate) fn is_in_function_body(candidate: &Node, context: &dyn ResolutionContext) -> bool {
    lexical_scope_of(candidate, context).is_some()
}

/// The innermost function or method BODY that scopes a declaration, walking
/// out through its qualified name (`test_x::Request::User::has_perm` →
/// `test_x`, upstream #2125 `lexicalScopeOf`). A method directly on a class
/// is reachable through its instances, and an object-literal method a
/// function returns through the object: neither is scoped by the function it
/// sits in.
fn lexical_scope_of(candidate: &Node, context: &dyn ResolutionContext) -> Option<(i64, i64)> {
    let own = candidate.qualified_name.as_str();
    let parent = own.rsplit_once("::").map_or("", |(parent, _)| parent);
    let mut qualified = own;
    while let Some((up, _)) = qualified.rsplit_once("::") {
        qualified = up;
        let Some(container) = context
            .get_nodes_by_qualified_name_shared(qualified)
            .into_iter()
            .find(|p| {
                p.file_path == candidate.file_path
                    && matches!(p.kind, NodeKind::Function | NodeKind::Method)
                    && p.start_line <= candidate.start_line
                    && p.end_line >= candidate.end_line
            })
        else {
            continue;
        };
        if candidate.kind == NodeKind::Method && qualified == parent {
            return None;
        }
        return Some((container.start_line, container.end_line));
    }
    None
}

/// Languages whose calls are JS/TS calls (upstream `JS_FAMILY`): Vue, Svelte
/// and Astro components' scripts and template expressions included, so a
/// bare `t('key')` in a `.vue` file resolves lexically exactly as in a `.ts`
/// one (upstream #2128).
pub(crate) fn is_js_family(language: Language) -> bool {
    is_js_ts(language) || matches!(language, Language::Vue | Language::Svelte | Language::Astro)
}

/// The four script languages themselves (upstream `JS_TS`), for the rules
/// upstream keeps to them: `this.<field>` and `this.#field` calls and
/// call-result receivers.
pub(crate) fn is_js_ts(language: Language) -> bool {
    matches!(
        language,
        Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx
    )
}

/// Method names that require receiver evidence before a JS/TS call links to
/// project code (`JS_BUILTIN_METHODS`, upstream v1.6.1 #1987): Array / typed
/// array and collection, String, Promise, Function, EventTarget /
/// EventEmitter and iterator methods, and fetch's Response / Request / Blob
/// bodies (upstream #2177). Sorted for binary search.
const JS_BUILTIN_METHODS: [&str; 108] = [
    "add",
    "addEventListener",
    "addListener",
    "apply",
    "arrayBuffer",
    "at",
    "bind",
    "blob",
    "call",
    "catch",
    "charAt",
    "charCodeAt",
    "clear",
    "codePointAt",
    "concat",
    "copyWithin",
    "delete",
    "dispatchEvent",
    "drop",
    "emit",
    "endsWith",
    "entries",
    "eventNames",
    "every",
    "fill",
    "filter",
    "finally",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "flat",
    "flatMap",
    "forEach",
    "formData",
    "get",
    "getMaxListeners",
    "has",
    "includes",
    "indexOf",
    "join",
    "json",
    "keys",
    "lastIndexOf",
    "listenerCount",
    "listeners",
    "localeCompare",
    "map",
    "match",
    "matchAll",
    "next",
    "normalize",
    "off",
    "on",
    "once",
    "padEnd",
    "padStart",
    "pop",
    "prependListener",
    "prependOnceListener",
    "push",
    "rawListeners",
    "reduce",
    "reduceRight",
    "removeAllListeners",
    "removeEventListener",
    "removeListener",
    "repeat",
    "replace",
    "replaceAll",
    "return",
    "reverse",
    "search",
    "set",
    "setMaxListeners",
    "shift",
    "slice",
    "some",
    "sort",
    "splice",
    "split",
    "startsWith",
    "subarray",
    "substr",
    "substring",
    "take",
    "text",
    "then",
    "throw",
    "toArray",
    "toLocaleLowerCase",
    "toLocaleString",
    "toLocaleUpperCase",
    "toLowerCase",
    "toReversed",
    "toSorted",
    "toSpliced",
    "toString",
    "toUpperCase",
    "trim",
    "trimEnd",
    "trimLeft",
    "trimRight",
    "trimStart",
    "unshift",
    "valueOf",
    "values",
    "with",
];

fn is_js_builtin_method(name: &str) -> bool {
    JS_BUILTIN_METHODS.binary_search(&name).is_ok()
}

/// Languages whose file boundary follows ESM-style import/export semantics.
/// ArkTS participates in module visibility even though the narrower JS call
/// inference gates deliberately exclude it (upstream #1719).
pub(crate) fn is_esm_module_family(language: Language) -> bool {
    is_js_ts(language) || language == Language::ArkTs
}

/// A JS/TS/ArkTS file containing an import statement but no export is a sealed
/// module: none of its bindings can be named from another file. Classic scripts,
/// CommonJS modules, local export clauses, and ambient globals remain visible
/// (`isSealedModule`, upstream v1.6.1).
///
/// Cheapest disqualifiers first, as upstream orders them: an exported node
/// (answered without reading the file), then the raw text's `import` and
/// CommonJS markers, and only then the comment- and string-blanked view — a
/// template literal spelling `import fake from "fake"` is data, not an import.
/// The decision is memoised per file for the resolution pass.
fn is_sealed_js_module(file_path: &str, context: &dyn ResolutionContext) -> bool {
    if context.file_has_exported_node(file_path) {
        return false;
    }
    let Some(facts) = context.source_facts(file_path) else {
        return false;
    };
    facts.js_sealed_module(is_sealed_by_text)
}

/// The text half of [`is_sealed_js_module`]: an `import` statement and no
/// `export`, in a file with no CommonJS export.
pub(crate) fn is_sealed_by_text(facts: &crate::source_facts::SourceFacts) -> bool {
    let source = facts.source();
    if !source.contains("import") {
        return false;
    }
    // CommonJS assignments can execute inside template interpolations,
    // which the blanked view hides, so this exemption reads raw source.
    if cjs_export_marker().is_match(source) {
        return false;
    }
    let code = facts.ts_blanked();
    js_import_marker().is_match(code) && !js_export_marker().is_match(code)
}

/// A line-initial `import` statement (`HAS_IMPORT_STATEMENT`).
pub(crate) fn js_import_marker() -> &'static Regex {
    static HAS_IMPORT: OnceLock<Regex> = OnceLock::new();
    HAS_IMPORT.get_or_init(|| Regex::new(r#"(?m)^[ \t]*import[\s{*'"]"#).expect("js import marker"))
}

/// A line-initial `export` or `declare global` (`HAS_ESM_EXPORT`).
pub(crate) fn js_export_marker() -> &'static Regex {
    static HAS_EXPORT: OnceLock<Regex> = OnceLock::new();
    HAS_EXPORT.get_or_init(|| {
        Regex::new(r"(?m)^[ \t]*export[\s{*]|^[ \t]*declare\s+global(?-u:\b)")
            .expect("js export marker")
    })
}

/// `module.exports` or `exports.x` / `exports[…]` (`HAS_CJS_EXPORT`).
pub(crate) fn cjs_export_marker() -> &'static Regex {
    static HAS_CJS_EXPORT: OnceLock<Regex> = OnceLock::new();
    HAS_CJS_EXPORT.get_or_init(|| {
        Regex::new(r"(?-u:\b)module\.exports(?-u:\b)|(?-u:\b)exports\s*[.\[]")
            .expect("CommonJS export marker")
    })
}

/// Whether `candidate` is a top-level binding of an ES module that the module
/// does not export (upstream #2162 `isUnexportedModuleBinding`): declared
/// without `export` and absent from its `export { … }`, `export default x`
/// and `export default { … }` lists, so no other file can name it. The
/// sealed-module rule covers files that export nothing; this is the same
/// boundary per symbol. Classic scripts, CommonJS, `declare global` and
/// `.d.ts` files, members of a class or namespace (qualified names), and
/// anything not declared by a statement of its own (an object literal's
/// member, `proto.x = function x() {}`) are exempt.
fn is_unexported_module_binding(candidate: &Node, context: &dyn ResolutionContext) -> bool {
    if candidate.is_exported
        || !matches!(
            candidate.kind,
            NodeKind::Function
                | NodeKind::Variable
                | NodeKind::Constant
                | NodeKind::Class
                | NodeKind::Interface
                | NodeKind::TypeAlias
                | NodeKind::Enum
                | NodeKind::Component
        )
        || candidate.qualified_name.contains("::")
        || [".d.ts", ".d.cts", ".d.mts"]
            .iter()
            .any(|suffix| candidate.file_path.ends_with(suffix))
    {
        return false;
    }
    let Some(facts) = context.source_facts(&candidate.file_path) else {
        return false;
    };
    let Some(exported) = facts.js_module_exports(read_module_exports) else {
        return false;
    };
    if exported.contains(&candidate.name) {
        return false;
    }
    let Some(line) = usize::try_from(candidate.start_line)
        .ok()
        .and_then(|line| line.checked_sub(1))
        .and_then(|index| facts.raw_line(index))
    else {
        return false;
    };
    static OWN_EXPORT: OnceLock<Regex> = OnceLock::new();
    static DECLARATION: OnceLock<Regex> = OnceLock::new();
    // Its own line says `export` (a node's flag can miss a form).
    if OWN_EXPORT
        .get_or_init(|| Regex::new(r"^\s*export(?-u:\b)").expect("export line"))
        .is_match(line)
    {
        return false;
    }
    // Only a declaration statement is a module binding: an object literal's
    // member (a zustand store action `setZipUri: (v) => set(…)`) is reached
    // through the object, and so is `proto.x = function x() {}`.
    DECLARATION
        .get_or_init(|| {
            Regex::new(
                r"^\s*(?:declare\s+)?(?:async\s+)?(?:function\*?|const|let|var|(?:abstract\s+)?class|interface|type|enum)\s",
            )
            .expect("declaration line")
        })
        .is_match(line)
}

/// The names an ES module's export lists name, or `None` for a file that is
/// no ES module (no `import` / `export`, CommonJS, or `declare global`).
pub(crate) fn read_module_exports(
    facts: &crate::source_facts::SourceFacts,
) -> Option<HashSet<String>> {
    static EXPORT_LIST: OnceLock<Regex> = OnceLock::new();
    static EXPORT_DEFAULT_NAME: OnceLock<Regex> = OnceLock::new();
    static EXPORT_DEFAULT_OBJECT: OnceLock<Regex> = OnceLock::new();
    static DECLARE_GLOBAL: OnceLock<Regex> = OnceLock::new();
    static AS: OnceLock<Regex> = OnceLock::new();
    static IDENTIFIER: OnceLock<Regex> = OnceLock::new();
    let source = facts.source();
    let code = facts.ts_blanked();
    let module = (js_import_marker().is_match(code) || js_export_marker().is_match(code))
        && !cjs_export_marker().is_match(source)
        && !DECLARE_GLOBAL
            .get_or_init(|| {
                Regex::new(r"(?-u:\b)declare\s+global(?-u:\b)").expect("declare global")
            })
            .is_match(code);
    if !module {
        return None;
    }
    let mut names = HashSet::new();
    let export_list = EXPORT_LIST.get_or_init(|| {
        Regex::new(r"(?m)^[ \t]*export\s+(?:type\s+)?\{([^}]*)\}").expect("export list")
    });
    let alias = AS.get_or_init(|| Regex::new(r"\s+as\s+").expect("export alias"));
    for captures in export_list.captures_iter(source) {
        for item in captures[1].split(',') {
            let item = item.trim();
            let item = item
                .strip_prefix("type")
                .filter(|rest| rest.starts_with(char::is_whitespace))
                .map_or(item, str::trim_start);
            let local = alias.split(item).next().unwrap_or(item).trim();
            if !local.is_empty() {
                names.insert(local.to_string());
            }
        }
    }
    let export_default_name = EXPORT_DEFAULT_NAME.get_or_init(|| {
        Regex::new(r"(?m)^[ \t]*export\s+(?:default|=)\s+([A-Za-z_$][0-9A-Za-z_$]*)\s*;?\s*$")
            .expect("export default name")
    });
    for captures in export_default_name.captures_iter(source) {
        names.insert(captures[1].to_string());
    }
    // `export default { getAdapter, adapters: known }` exposes its shorthand
    // and value names; so does a `return { getDefaultActivityRoute, … }` (a
    // composable hands the function out through its result, upstream #2163).
    let export_default_object = EXPORT_DEFAULT_OBJECT.get_or_init(|| {
        Regex::new(r"(?m)^[ \t]*export\s+default\s+\{([^}]*)\}|(?-u:\b)return\s*\{([^{}]*)\}")
            .expect("export default object")
    });
    let identifier = IDENTIFIER
        .get_or_init(|| Regex::new(r"^\s*([A-Za-z_$][0-9A-Za-z_$]*)\s*$").expect("identifier"));
    for captures in export_default_object.captures_iter(code) {
        let Some(items) = captures.get(1).or_else(|| captures.get(2)) else {
            continue;
        };
        for item in items.as_str().split(',') {
            let value = item.rsplit(':').next().unwrap_or(item);
            if let Some(id) = identifier.captures(value) {
                names.insert(id[1].to_string());
            }
        }
    }
    Some(names)
}

/// Post-pipeline visibility gate for JS/TS name targets (`isCrossFileReachable`,
/// upstream v1.6.1). Applying this after a strategy chooses its target avoids
/// promoting an unrelated runner-up.
pub(crate) fn is_js_name_target_visible(
    candidate: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    if reference.reference_kind == EdgeKind::Calls
        && is_esm_module_family(candidate.language)
        && matches!(candidate.kind, NodeKind::Constant | NodeKind::Variable)
        && is_json_require_signature(candidate.signature.as_deref().unwrap_or(""))
    {
        // `const data = require('./data.json')` names a JSON value; it is
        // never callable.
        return false;
    }
    candidate.file_path == reference.file_path
        || !is_esm_module_family(candidate.language)
        || !is_sealed_js_module(&candidate.file_path, context)
}

/// The name-match half of upstream's `isCrossFileReachable` (#2162): an ES
/// module's top-level binding it does not export is no other file's to
/// name. Import resolution still reaches it through what the module does
/// export (`UploadApi.localHelper()` through a default-exported object).
pub(crate) fn is_name_match_reachable(
    candidate: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    candidate.file_path == reference.file_path
        || !is_esm_module_family(candidate.language)
        || !is_unexported_module_binding(candidate, context)
}

/// `^=\s*require\s*\(\s*(['"])[^'"]+\.json\1\s*\)\s*;?\s*$` (upstream), with
/// the back-reference spelled out per quote.
fn is_json_require_signature(signature: &str) -> bool {
    static JSON_REQUIRE: OnceLock<[Regex; 2]> = OnceLock::new();
    JSON_REQUIRE
        .get_or_init(|| {
            [
                Regex::new(r#"^=\s*require\s*\(\s*'[^'"]+\.json'\s*\)\s*;?\s*$"#)
                    .expect("single-quoted JSON require"),
                Regex::new(r#"^=\s*require\s*\(\s*"[^'"]+\.json"\s*\)\s*;?\s*$"#)
                    .expect("double-quoted JSON require"),
            ]
        })
        .iter()
        .any(|pattern| pattern.is_match(signature))
}

/// Keywords after which a name starts an expression, so the call that follows
/// has no receiver (`BARE_CALL_KEYWORDS`, upstream v1.6.1; `go`/`defer` are the
/// Go statement forms).
const BARE_CALL_KEYWORDS: [&str; 18] = [
    "return",
    "await",
    "yield",
    "typeof",
    "void",
    "new",
    "else",
    "case",
    "throw",
    "in",
    "of",
    "instanceof",
    "go",
    "defer",
    // Go: `if T(x)`, `for range f()` (upstream #2408).
    "if",
    "switch",
    "for",
    "range",
];

fn is_ascii_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

/// Whether a `calls` ref is RECEIVER-LESS — `serialize(x)`, not
/// `this.serialize(x)` / `obj.serialize(x)` (`isReceiverLessCall`, upstream
/// v1.6.1). The extractor emits `this.m()` and `pkg.F().M()` under the bare
/// member name, so the receiver is read back from the call site's own line:
/// the text at the ref's column must start with the name, be followed by a
/// call opener, and be preceded by nothing, an operator/opener, or one of
/// [`BARE_CALL_KEYWORDS`].
///
/// Unlike upstream, the opener may sit on a following line: JavaScript's
/// automatic semicolon insertion never splits `finish\n()`, so that is a real
/// bare call. (Go inserts a semicolon after a line-final identifier, so no Go
/// call takes that shape.)
fn is_receiver_less_call(reference: &RefView, context: &dyn ResolutionContext) -> bool {
    if reference.reference_kind != EdgeKind::Calls || reference.reference_name.contains('.') {
        return false;
    }
    let Some(line_index) = (reference.line as usize).checked_sub(1) else {
        return false;
    };
    let Some(facts) = context.source_facts(&reference.file_path) else {
        return false;
    };
    let Some(line) = facts.raw_line(line_index) else {
        return false;
    };
    let name = reference.reference_name.as_str();
    let column = reference.column.max(0) as usize;
    if column > line.len() || !line.is_char_boundary(column) || !line[column..].starts_with(name) {
        return false;
    }
    let Some(line_start) = facts.lines().raw_line_start(line_index) else {
        return false;
    };
    let after =
        facts.source()[line_start + column + name.len()..].trim_start_matches(char::is_whitespace);
    if !after.starts_with('(') && !after.starts_with('<') {
        return false;
    }
    let before = line[..column].trim_end_matches(char::is_whitespace);
    let Some(tail) = before.chars().next_back() else {
        return true;
    };
    if !(is_ascii_word_char(tail) || matches!(tail, '.' | '$' | ']' | ')')) {
        return true;
    }
    let word_len = before
        .bytes()
        .rev()
        .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        .count();
    BARE_CALL_KEYWORDS.contains(&&before[before.len() - word_len..])
}

/// Whether the line of a bare reference shows no receiver before its name
/// (upstream `hasNoReceiverOnLine`, #2091): the name at the reference's
/// column, or ending there, else its first `name(` on the line, has no `.`
/// before it. A name not on its line at all is a link of a chain written
/// across lines. Read case-insensitively, as upstream reads it.
pub(crate) fn has_no_receiver_on_line(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    let Some(index) = usize::try_from(reference.line)
        .ok()
        .and_then(|line| line.checked_sub(1))
    else {
        return true;
    };
    let Some(facts) = context.source_facts(&reference.file_path) else {
        return true;
    };
    let Some(line) = facts.raw_line(index) else {
        return true;
    };
    let lower = line.to_ascii_lowercase();
    let name = reference.reference_name.to_ascii_lowercase();
    let column = usize::try_from(reference.column).unwrap_or(0);
    let named_at = |start: usize| {
        lower
            .get(start..)
            .is_some_and(|rest| rest.starts_with(&name))
    };
    let start = if named_at(column) {
        Some(column)
    } else if column >= name.len() && named_at(column - name.len()) {
        Some(column - name.len())
    } else {
        first_call_of(&lower, &name)
    };
    let Some(start) = start else {
        return false;
    };
    !line[..start]
        .trim_end_matches(codegraph_core::generated_header::is_js_whitespace)
        .ends_with('.')
}

/// The offset of the first `name(` in `line` that no word character or `$`
/// precedes (upstream's `(?<![\w$])name\s*\(`).
fn first_call_of(line: &str, name: &str) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    line.char_indices().map(|(at, _)| at).find(|&at| {
        if !line[at..].starts_with(name) {
            return false;
        }
        let free = line[..at]
            .chars()
            .next_back()
            .is_none_or(|before| !(is_ascii_word_char(before) || before == '$'));
        free && line[at + name.len()..]
            .trim_start_matches(codegraph_core::generated_header::is_js_whitespace)
            .starts_with('(')
    })
}

/// A receiver-less JS/TS call can never bind to a method (#1714).
pub(crate) fn is_bare_js_call(reference: &RefView, context: &dyn ResolutionContext) -> bool {
    is_js_family(reference.language) && is_receiver_less_call(reference, context)
}

/// A receiver-less Go call — a func parameter, a local func value, a
/// package-level function — is never a method, in its own package or in one
/// the file does not import (#1857, upstream `isBareGoCall`).
fn is_bare_go_call(reference: &RefView, context: &dyn ResolutionContext) -> bool {
    reference.language == Language::Go && is_receiver_less_call(reference, context)
}

/// Whether a Go reference's name is written bare, so that Go reads it from
/// the reference's own package (upstream `isGoBareName`, #2408): a type `Node`
/// (variadic `...Node` too), a composite literal `&Event{}`, a call
/// `Walk(v, n)` or a conversion `(*Block)(pb)`. Not `parser.Node` or
/// `...chunks.Meta`, written through an import, nor a name reached through a
/// value: `err[i].Error()`, a `.String()` chained onto the line above.
pub(crate) fn is_go_bare_name(reference: &RefView, context: &dyn ResolutionContext) -> bool {
    let name = reference.reference_name.as_str();
    if reference.language != Language::Go || !is_go_identifier(name) {
        return false;
    }
    if reference.reference_kind == EdgeKind::Calls && is_receiver_less_call(reference, context) {
        return true;
    }
    let Some(facts) = context.source_facts(&reference.file_path) else {
        return false;
    };
    let Some(line) = usize::try_from(reference.line)
        .ok()
        .and_then(|line| line.checked_sub(1))
        .and_then(|index| facts.raw_line(index))
    else {
        return false;
    };
    let column = usize::try_from(reference.column.max(0)).unwrap_or(0);
    let Some(rest) = line.get(column..) else {
        return false;
    };
    // A call's column is its expression's: only a conversion's parenthesized
    // type is still bare there.
    if reference.reference_kind == EdgeKind::Calls {
        let Some(inner) = rest.strip_prefix('(') else {
            return false;
        };
        let inner = inner.trim_start();
        let inner = inner.strip_prefix('*').unwrap_or(inner).trim_start();
        let Some(after) = inner.strip_prefix(name) else {
            return false;
        };
        return after
            .trim_start()
            .strip_prefix(')')
            .is_some_and(|after| after.trim_start().starts_with('('));
    }
    if rest.starts_with(name) {
        let before = line[..column].trim_end();
        return !before.ends_with('.') || before.ends_with("...");
    }
    // A name recorded away from its spelling (a route's handler, at the start
    // of its line): its spelling there, outside strings.
    let code = blank_go_strings(line);
    code.match_indices(name).any(|(at, _)| {
        let free = code[..at]
            .bytes()
            .next_back()
            .is_none_or(|byte| !(byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.'));
        free && code[at + name.len()..]
            .bytes()
            .next()
            .is_none_or(|byte| !(byte.is_ascii_alphanumeric() || byte == b'_'))
    })
}

fn is_go_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// `line` with the contents of its Go string literals blanked (upstream's
/// `/"(?:[^"\\]|\\.)*"|`[^`]*`/g` replaced by spaces).
fn blank_go_strings(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        match c {
            '"' => {
                let mut literal = String::from('"');
                let mut closed = false;
                while let Some((_, next)) = chars.next() {
                    literal.push(next);
                    if next == '\\' {
                        if let Some((_, escaped)) = chars.next() {
                            literal.push(escaped);
                        }
                    } else if next == '"' {
                        closed = true;
                        break;
                    }
                }
                if closed {
                    out.extend(std::iter::repeat_n(' ', literal.chars().count()));
                } else {
                    out.push_str(&literal);
                }
            }
            '`' => {
                let mut literal = String::from('`');
                let mut closed = false;
                for (_, next) in chars.by_ref() {
                    literal.push(next);
                    if next == '`' {
                        closed = true;
                        break;
                    }
                }
                if closed {
                    out.extend(std::iter::repeat_n(' ', literal.chars().count()));
                } else {
                    out.push_str(&literal);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Whether a receiver written as `Name` is a type in `language`'s conventions
/// (upstream #2166 `namesExternalType`). Not in Go (an exported package
/// variable is `FormPost`), nor C, C++ (CUDA and Metal included) or Rust
/// (their type paths use `::`); in Pascal every identifier is capitalized, so only Delphi's type
/// prefixes (`TFoo`, `EFoo`, `IFoo`) and `Exception` count.
fn names_external_type(receiver: &str, language: Language) -> bool {
    let mut bytes = receiver.bytes();
    if !bytes.next().is_some_and(|first| first.is_ascii_uppercase())
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return false;
    }
    if language == Language::Pascal {
        let prefixed = receiver.len() >= 2
            && matches!(receiver.as_bytes()[0], b'T' | b'E' | b'I')
            && receiver.as_bytes()[1].is_ascii_uppercase();
        return prefixed || receiver == "Exception";
    }
    // Rust: `Vec::new()`, `String::from(…)`, `Default::default()` — but
    // `Self::` is the impl's own type, and a SCREAMING_CASE receiver
    // (`REQ_ID.scope(…)`) a static (upstream #2177).
    if language == Language::Rust {
        return receiver != "Self" && receiver.bytes().any(|byte| byte.is_ascii_lowercase());
    }
    // CUDA and Metal sources are extracted as C++.
    !matches!(language, Language::Go | Language::C | Language::Cpp)
}

/// A class, struct, union or interface, or a Scala `object` (upstream
/// `isMethodOwnerKind`).
fn is_method_owner(node: &Node) -> bool {
    matches!(
        node.kind,
        NodeKind::Class | NodeKind::Struct | NodeKind::Union | NodeKind::Interface
    ) || crate::types::is_scala_singleton(node)
}

/// A type name that reads as a type (capitalized) yet names no class,
/// struct or interface of the reference's language family the project
/// declares: one from outside the project (upstream #2169).
fn is_undeclared_type_name(
    type_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    type_name.starts_with(|c: char| c.is_ascii_uppercase())
        && !context
            .get_nodes_by_name_shared(type_name)
            .iter()
            .any(|n| is_method_owner(n) && same_language_family(n.language, reference.language))
}

/// Words that end a C++ declarator scan without naming a type (upstream
/// `CPP_NON_TYPE_TOKENS`).
const CPP_NON_TYPE_TOKENS: &[&str] = &[
    "return",
    "if",
    "else",
    "for",
    "while",
    "do",
    "switch",
    "case",
    "default",
    "break",
    "continue",
    "goto",
    "throw",
    "new",
    "delete",
    "co_await",
    "co_yield",
    "co_return",
    "static_cast",
    "const_cast",
    "dynamic_cast",
    "reinterpret_cast",
    "sizeof",
    "alignof",
    "typeid",
    "and",
    "or",
    "not",
    "xor",
];

/// The type a C++ declared type names, as upstream's `normalizeCppTypeName`
/// reads it: [`normalize_cpp_type_name`]'s reading, its last `::` segment
/// kept (`const SkipList<Key, Comparator>&` → `SkipList`).
fn cpp_declared_type_name(declared: &str) -> Option<String> {
    normalize_cpp_type_name(declared)
        .map(|normalized| cpp_last_segment(&normalized).trim().to_string())
}

/// The simple name of the type a member belongs to: the last `::` / `.`
/// segment before the member's own (`THorseCore` of `THorseCore::Get`).
fn member_owner_name(qualified_name: &str) -> &str {
    let Some(cut) = qualified_name.rfind("::").filter(|&cut| cut > 0) else {
        return "";
    };
    qualified_name[..cut]
        .rsplit("::")
        .next()
        .and_then(|segment| segment.rsplit('.').next())
        .unwrap_or_default()
}

/// A method named `name` on a supertype of `classes`, nearest first, at most
/// five levels up (upstream #2180 `inheritedClassMethod`).
fn inherited_class_method(
    classes: &[Arc<Node>],
    name: &str,
    context: &dyn ResolutionContext,
) -> Option<Arc<Node>> {
    let mut seen: std::collections::HashSet<String> =
        classes.iter().map(|class| class.id.clone()).collect();
    let mut frontier: Vec<Arc<Node>> = classes.to_vec();
    for _ in 0..5 {
        if frontier.is_empty() {
            break;
        }
        let mut next = Vec::new();
        for class in &frontier {
            for supertype in crate::class_heads::declared_supertypes(class, context) {
                for decl in context.get_nodes_by_name_shared(&supertype) {
                    if decl.language != class.language
                        || !is_method_owner(&decl)
                        || !seen.insert(decl.id.clone())
                    {
                        continue;
                    }
                    if let Some(method) = context
                        .get_nodes_in_file_shared(&decl.file_path)
                        .into_iter()
                        .find(|n| {
                            n.kind == NodeKind::Method
                                && n.name == name
                                && member_owner_name(&n.qualified_name) == decl.name
                        })
                    {
                        return Some(method);
                    }
                    next.push(decl);
                }
            }
        }
        frontier = next;
    }
    None
}

/// A request handler the web framework dispatches to: a Django / DRF /
/// Flask view's `get` / `post` / …, a controller's `index` / `store` /
/// `update` / `destroy` (upstream #2175 `DISPATCHED_OWNER` /
/// `DISPATCHED_ACTIONS`). Nothing calls one through an instance by name.
fn is_dispatched_handler(method: &Node) -> bool {
    const ACTIONS: [&str; 17] = [
        "get",
        "post",
        "put",
        "patch",
        "delete",
        "head",
        "options",
        "index",
        "show",
        "store",
        "update",
        "destroy",
        "create",
        "edit",
        "list",
        "retrieve",
        "partial_update",
    ];
    const OWNERS: [&str; 6] = [
        "View",
        "ViewSet",
        "APIView",
        "Controller",
        "Endpoint",
        "ViewMixin",
    ];
    ACTIONS.contains(&method.name.as_str()) && {
        let owner = member_owner_name(&method.qualified_name);
        OWNERS.iter().any(|suffix| owner.ends_with(suffix))
    }
}

/// A test double's word: Fake…, Mock…, Mocked…, Stub…, Dummy…, Spy… (upstream
/// `TEST_DOUBLE_OWNER`, matched per split word).
fn is_test_double_word(word: &str) -> bool {
    ["fake", "mock", "mocked", "stub", "dummy", "spy"]
        .iter()
        .any(|double| word.eq_ignore_ascii_case(double))
}

/// The link of a dotted receiver its value is named after: the last, or for a
/// constant (`InitializationPhase.CONTROLLERS`) the type it belongs to
/// (upstream `receiverLink`).
fn receiver_link(receiver: &str) -> &str {
    let mut links = receiver.rsplit('.');
    let last = links.next().unwrap_or(receiver);
    let constant = last.len() >= 2
        && last.starts_with(|c: char| c.is_ascii_uppercase())
        && last
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    match links.next() {
        Some(owner) if constant => owner,
        _ => last,
    }
}

/// Methods of .NET's base types, collections, streams, strings, LINQ and
/// reflection (upstream #2171 `CSHARP_STD_METHODS`): names a project type
/// overrides or wraps, which a call through an untyped receiver means only
/// when the receiver is named after the owner.
const CSHARP_STD_METHODS: [&str; 108] = [
    "ToString",
    "Equals",
    "GetHashCode",
    "GetType",
    "CompareTo",
    "Add",
    "AddRange",
    "Remove",
    "RemoveAt",
    "RemoveAll",
    "Contains",
    "ContainsKey",
    "ContainsValue",
    "Clear",
    "Insert",
    "IndexOf",
    "CopyTo",
    "ToArray",
    "ToList",
    "ToDictionary",
    "GetEnumerator",
    "MoveNext",
    "Reset",
    "TryGetValue",
    "GetValueOrDefault",
    "TryAdd",
    "Write",
    "WriteLine",
    "WriteAsync",
    "WriteLineAsync",
    "Read",
    "ReadAsync",
    "ReadLine",
    "ReadToEnd",
    "Flush",
    "FlushAsync",
    "Close",
    "Dispose",
    "DisposeAsync",
    "Parse",
    "TryParse",
    "Format",
    "Join",
    "Split",
    "Replace",
    "Substring",
    "Trim",
    "TrimStart",
    "TrimEnd",
    "StartsWith",
    "EndsWith",
    "ToUpper",
    "ToLower",
    "ToUpperInvariant",
    "ToLowerInvariant",
    "Select",
    "Where",
    "First",
    "FirstOrDefault",
    "Single",
    "SingleOrDefault",
    "Last",
    "LastOrDefault",
    "Any",
    "All",
    "Count",
    "Sum",
    "Max",
    "Min",
    "OrderBy",
    "OrderByDescending",
    "GroupBy",
    "Skip",
    "Take",
    "Distinct",
    "Concat",
    "Cast",
    "OfType",
    "Aggregate",
    "Invoke",
    "DynamicInvoke",
    "GetMethod",
    "GetProperty",
    "GetField",
    "GetConstructor",
    "GetCustomAttributes",
    "GetGenericArguments",
    "MakeGenericType",
    "IsAssignableFrom",
    "ConfigureAwait",
    "Wait",
    "ContinueWith",
    "Append",
    "AppendLine",
    "Peek",
    "Push",
    "Pop",
    "Enqueue",
    "Dequeue",
    "HasFlag",
    "Find",
    "FindAll",
    "ForEach",
    "Sort",
    "Reverse",
    "Clone",
    "Seek",
    "SetLength",
];

/// The standard-library method names of a language whose untyped-receiver
/// guesses need the receiver to name the owner (upstream #2171
/// `stdMethodNames`, without Go's set, which W4-GO-03 brings).
fn is_std_method_name(language: Language, name: &str) -> bool {
    match language {
        Language::CSharp => CSHARP_STD_METHODS.contains(&name),
        Language::Rust => is_rust_std_method(name),
        Language::Kotlin => crate::kotlin_scope::is_kotlin_std_method(name),
        Language::Go => crate::go_calls::is_go_std_method(name),
        Language::Dart => crate::dart_scope::DART_STD_METHODS.contains(&name),
        _ => false,
    }
}

/// Whether a receiver is named after the owner of `method`, for a Dart
/// extension's member the type it is `on` (upstream #2207
/// `receiverNamesOwner`): getx's `ext.endsWith(".avi")` on a String shares a
/// word with `RxStringExt`, none with its `Rx<String>`.
fn receiver_names_owner(receiver: &str, method: &Node, context: &dyn ResolutionContext) -> bool {
    if let Some(qualified_name) = crate::dart_scope::dart_receiver_owner(method, context) {
        let on = Node {
            qualified_name,
            ..method.clone()
        };
        return crate::kotlin_scope::shares_receiver_word(receiver, &on);
    }
    crate::kotlin_scope::shares_receiver_word(receiver, method)
}

/// A method of a test double (`MockedResponse`, `_FakeHTTPResponse`) the
/// receiver and the calling file never name (upstream #2197
/// `isUnnamedTestDouble`).
fn is_unnamed_test_double(
    method: &Node,
    receiver: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    let owner = member_owner_name(&method.qualified_name);
    if owner.is_empty()
        || !split_camel_case(owner)
            .iter()
            .any(|w| is_test_double_word(w))
    {
        return false;
    }
    if split_camel_case(receiver_link(receiver))
        .iter()
        .any(|w| is_test_double_word(w))
    {
        return false;
    }
    !context
        .read_file_shared(&reference.file_path)
        .is_some_and(|text| text.contains(owner))
}

/// Languages whose identifiers resolve regardless of case (upstream
/// `CASE_INSENSITIVE_LANGUAGES`: PHP, Pascal/Delphi, CFML; COBOL is not
/// extracted, and VB.NET joins with its extractor).
pub const CASE_INSENSITIVE_LANGUAGES: [Language; 3] =
    [Language::Php, Language::Pascal, Language::Cfml];

/// Whether `language` is one of [`CASE_INSENSITIVE_LANGUAGES`].
pub(crate) fn is_case_insensitive_language(language: Language) -> bool {
    CASE_INSENSITIVE_LANGUAGES.contains(&language)
}

/// What only exists inside a type, reachable through a receiver alone
/// (upstream #2120 `TYPE_MEMBER_KINDS`).
fn is_type_member_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Method | NodeKind::Property | NodeKind::Field | NodeKind::EnumMember
    )
}

/// The first import binding the file declares under `name`.
fn import_binding_named(
    name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<crate::types::ImportMapping> {
    context
        .get_import_mappings(&reference.file_path, reference.language)
        .into_iter()
        .find(|mapping| mapping.local_name == name)
}

/// Does the file bind `name` by importing it from outside the repository
/// (upstream #2120 `isOutOfRepoBinding`)?
fn is_out_of_repo_binding(
    name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    import_binding_named(name, reference, context).is_some_and(|mapping| {
        crate::import_resolver::is_out_of_repo_import(
            &mapping.source,
            &reference.file_path,
            reference.language,
            context,
        )
    })
}

/// Is the root of a member call's receiver (`z` in `z.string`, `ns` in
/// `ns.util.fn`) an import binding that is a module namespace, or one from
/// outside the repository (upstream #2120 `isImportedModuleReceiver`)?
fn is_imported_module_receiver(
    receiver: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    let root = receiver.split('.').next().unwrap_or(receiver);
    import_binding_named(root, reference, context).is_some_and(|mapping| {
        mapping.is_namespace
            || crate::import_resolver::is_out_of_repo_import(
                &mapping.source,
                &reference.file_path,
                reference.language,
                context,
            )
    })
}

/// Is the root of a member call's receiver (`CameraManager` in
/// `CameraManager.x`) one of the file's imports (upstream #2134
/// `isImportBinding`)?
pub(crate) fn is_import_binding(
    receiver: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    let root = receiver.split('.').next().unwrap_or(receiver);
    import_binding_named(root, reference, context).is_some()
}

/// A method a Vue Options API component declares for itself:
/// `index::handleLogin` in `index.vue` (upstream #2140).
fn is_vue_component_method(node: &Node) -> bool {
    if node.kind != NodeKind::Method || !node.file_path.ends_with(".vue") {
        return false;
    }
    let base = node.file_path.rsplit('/').next().unwrap_or(&node.file_path);
    let stem = base.strip_suffix(".vue").unwrap_or(base);
    node.qualified_name
        .strip_prefix(stem)
        .and_then(|rest| rest.strip_prefix("::"))
        .is_some_and(|name| name == node.name)
}

/// Whether `reference` is written as `this.<name>(` in the file that declares
/// `node`.
fn is_this_call_in_own_file(
    node: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    if node.file_path != reference.file_path {
        return false;
    }
    let Some(line) = context
        .source_facts(&reference.file_path)
        .and_then(|facts| {
            usize::try_from(reference.line)
                .ok()
                .and_then(|line| line.checked_sub(1))
                .and_then(|index| facts.raw_line(index).map(str::to_string))
        })
    else {
        return false;
    };
    Regex::new(&format!(
        r"(?-u:\b)this\s*\??\.\s*{}\s*\(",
        regex::escape(&node.name)
    ))
    .is_ok_and(|pattern| pattern.is_match(&line))
}

/// A plain JS identifier, `/^[A-Za-z_$][\w$]*$/`.
pub(crate) fn is_simple_js_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || matches!(first, b'_' | b'$'))
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
}

/// How a Rust `calls` ref recorded under a bare name was written (KEEP-RUST,
/// P15). The extractor keeps a receiver only for an identifier, `self`, one
/// `self.<field>` hop and a `path::call()` receiver; any other receiver —
/// `root().join(x)`, a deeper field chain, `vec![…]`, `?`, `.await`, indexing, a
/// chain broken across lines — leaves the bare method name, with the ref's
/// column at the start of the whole call expression. The source tells the two
/// apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RustCallShape {
    /// `join(x)`: neither receiver nor path, so a free function, never a method.
    Bare,
    /// `root().join(x)`: a method call whose receiver the extractor dropped.
    Method,
}

/// How far past the ref's column a dropped receiver's `.name(` is looked for.
const RUST_RECEIVER_WINDOW: usize = 4096;

/// The shape of a Rust `calls` ref whose name has neither `.` nor `::`, read from
/// its source: [`RustCallShape::Bare`] when the call expression starts with the
/// name and a `(` or `::<` follows, [`RustCallShape::Method`] when it starts with
/// something else and a `.name(` follows within [`RUST_RECEIVER_WINDOW`].
/// `None` — no source, a column off its line, neither pattern — keeps every
/// gate that reads the shape inert.
pub(crate) fn rust_call_shape(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<RustCallShape> {
    let name = reference.reference_name.as_str();
    if reference.language != Language::Rust
        || reference.reference_kind != EdgeKind::Calls
        || name.is_empty()
        || name.contains('.')
        || name.contains("::")
    {
        return None;
    }
    let facts = context.source_facts(&reference.file_path)?;
    let start = rust_ref_offset(reference, &facts)?;
    let rest = &facts.source()[start..];
    if let Some(after) = rest.strip_prefix(name) {
        if after.bytes().next().is_some_and(is_rust_ident_byte) {
            return None;
        }
        let after = after.trim_start();
        return (after.starts_with('(') || after.starts_with("::<")).then_some(RustCallShape::Bare);
    }
    let mut end = rest.len().min(RUST_RECEIVER_WINDOW);
    while !rest.is_char_boundary(end) {
        end -= 1;
    }
    rust_method_call_follows(&rest[..end], name).then_some(RustCallShape::Method)
}

/// The byte offset of `reference`'s column in its file, when the column lies on
/// its line at a char boundary.
fn rust_ref_offset(reference: &RefView, facts: &SourceFacts) -> Option<usize> {
    let line_index = usize::try_from(reference.line).ok()?.checked_sub(1)?;
    let line = facts.raw_line(line_index)?;
    let column = usize::try_from(reference.column).ok()?;
    if column > line.len() || !line.is_char_boundary(column) {
        return None;
    }
    Some(facts.lines().raw_line_start(line_index)? + column)
}

fn is_rust_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

/// Whether `text` holds a method call of `name`: one `.`, the whole word
/// `name`, an optional `::<…>` turbofish, then `(`, with whitespace allowed
/// between the parts.
fn rust_method_call_follows(text: &str, name: &str) -> bool {
    text.match_indices(name).any(|(at, _)| {
        let bytes = text.as_bytes();
        let end = at + name.len();
        if bytes.get(end).is_some_and(|b| is_rust_ident_byte(*b)) {
            return false;
        }
        let before = text[..at].trim_end();
        if !before.ends_with('.') || before.ends_with("..") {
            return false;
        }
        let mut after = text[end..].trim_start();
        if let Some(generics) = after.strip_prefix("::<") {
            let mut depth = 1usize;
            let Some(close) = generics.bytes().position(|b| {
                match b {
                    b'<' => depth += 1,
                    b'>' => depth -= 1,
                    _ => {}
                }
                depth == 0
            }) else {
                return false;
            };
            after = generics[close + 1..].trim_start();
        }
        after.starts_with('(')
    })
}

/// Method names a Rust call whose receiver the extractor dropped may not bind
/// on the name alone (KEEP-RUST, P15 R3; the Rust analogue of upstream's
/// `JS_BUILTIN_METHODS`, #1987). Every entry is a stable (Rust 1.98) method,
/// callable with method syntax, of `Iterator` / `DoubleEndedIterator`,
/// `Option`, `Result`, slices, `Vec`, `VecDeque`, `str`, `String`, `char` / `u8`
/// ASCII, integers, `Duration` / `Instant`, the std maps and sets and `Entry`,
/// `Path` / `PathBuf` / `OsStr` / `DirEntry` / `OpenOptions`, `Mutex` / `RwLock`
/// / `OnceLock` / `Rc` / `Arc` / `Weak` / `RefCell` / `Cell` / atomics, the
/// `io` / `fmt` I/O traits, `process::Command` / `Child`, `mpsc`, or the
/// conversion and comparison traits — a curated subset, the ones commonly
/// reached through call chains. A unique project method that shares one of
/// these names is no evidence of the receiver's type: `root().join(name)` is
/// `Path::join`. Sorted for binary search; changing it is a resolution change.
const RUST_STD_METHODS: [&str; 309] = [
    "abs",
    "all",
    "ancestors",
    "and_then",
    "any",
    "append",
    "arg",
    "args",
    "as_bytes",
    "as_deref",
    "as_deref_mut",
    "as_micros",
    "as_millis",
    "as_mut",
    "as_nanos",
    "as_os_str",
    "as_ref",
    "as_secs",
    "as_secs_f64",
    "as_slice",
    "as_str",
    "back",
    "binary_search",
    "binary_search_by",
    "binary_search_by_key",
    "borrow",
    "borrow_mut",
    "by_ref",
    "bytes",
    "canonicalize",
    "capacity",
    "chain",
    "char_indices",
    "chars",
    "checked_add",
    "checked_sub",
    "chunks",
    "clamp",
    "clear",
    "clone",
    "clone_from",
    "cloned",
    "cmp",
    "collect",
    "compare_exchange",
    "components",
    "concat",
    "contains",
    "contains_key",
    "copied",
    "count",
    "current_dir",
    "cycle",
    "dedup",
    "dedup_by_key",
    "deref",
    "deref_mut",
    "difference",
    "display",
    "drain",
    "duration_since",
    "elapsed",
    "ends_with",
    "entry",
    "enumerate",
    "env",
    "env_remove",
    "eq",
    "eq_ignore_ascii_case",
    "err",
    "exists",
    "expect",
    "expect_err",
    "extend",
    "extend_from_slice",
    "extension",
    "failure",
    "fetch_add",
    "fetch_sub",
    "file_name",
    "file_stem",
    "file_type",
    "fill",
    "filter",
    "filter_map",
    "find",
    "find_map",
    "first",
    "flat_map",
    "flatten",
    "flush",
    "fmt",
    "fold",
    "for_each",
    "front",
    "fuse",
    "ge",
    "get",
    "get_mut",
    "get_or_init",
    "get_or_insert",
    "get_or_insert_with",
    "gt",
    "hash",
    "insert",
    "insert_str",
    "inspect",
    "inspect_err",
    "intersection",
    "into",
    "into_bytes",
    "into_inner",
    "into_iter",
    "into_keys",
    "into_os_string",
    "into_string",
    "into_values",
    "is_absolute",
    "is_alphabetic",
    "is_alphanumeric",
    "is_ascii",
    "is_ascii_alphabetic",
    "is_ascii_alphanumeric",
    "is_ascii_digit",
    "is_ascii_lowercase",
    "is_ascii_punctuation",
    "is_ascii_uppercase",
    "is_ascii_whitespace",
    "is_char_boundary",
    "is_dir",
    "is_empty",
    "is_err",
    "is_err_and",
    "is_file",
    "is_lowercase",
    "is_none",
    "is_none_or",
    "is_ok",
    "is_ok_and",
    "is_relative",
    "is_some",
    "is_some_and",
    "is_subset",
    "is_symlink",
    "is_uppercase",
    "is_whitespace",
    "iter",
    "iter_mut",
    "join",
    "keys",
    "kill",
    "kind",
    "last",
    "le",
    "len",
    "lines",
    "lock",
    "lt",
    "map",
    "map_err",
    "map_or",
    "map_or_else",
    "map_while",
    "match_indices",
    "matches",
    "max",
    "max_by",
    "max_by_key",
    "metadata",
    "min",
    "min_by",
    "min_by_key",
    "ne",
    "next",
    "nth",
    "ok",
    "ok_or",
    "ok_or_else",
    "open",
    "or_default",
    "or_else",
    "or_insert",
    "or_insert_with",
    "output",
    "parent",
    "parse",
    "partial_cmp",
    "partition",
    "path",
    "peek",
    "peekable",
    "pop",
    "pop_back",
    "pop_front",
    "position",
    "pow",
    "product",
    "push",
    "push_back",
    "push_front",
    "push_str",
    "read",
    "read_dir",
    "read_exact",
    "read_line",
    "read_link",
    "read_to_end",
    "read_to_string",
    "recv",
    "reduce",
    "remove",
    "repeat",
    "replace",
    "replacen",
    "reserve",
    "resize",
    "retain",
    "rev",
    "reverse",
    "rfind",
    "rposition",
    "rsplit",
    "rsplit_once",
    "rsplitn",
    "saturating_add",
    "saturating_sub",
    "scan",
    "seek",
    "send",
    "set",
    "set_extension",
    "skip",
    "skip_while",
    "sort",
    "sort_by",
    "sort_by_key",
    "sort_unstable",
    "sort_unstable_by",
    "sort_unstable_by_key",
    "spawn",
    "split",
    "split_at",
    "split_off",
    "split_once",
    "split_terminator",
    "split_whitespace",
    "splitn",
    "starts_with",
    "status",
    "stderr",
    "stdin",
    "stdout",
    "step_by",
    "strip_prefix",
    "strip_suffix",
    "success",
    "sum",
    "swap",
    "take",
    "take_while",
    "to_ascii_lowercase",
    "to_ascii_uppercase",
    "to_digit",
    "to_lowercase",
    "to_owned",
    "to_path_buf",
    "to_str",
    "to_string",
    "to_string_lossy",
    "to_uppercase",
    "to_vec",
    "transpose",
    "trim",
    "trim_end",
    "trim_end_matches",
    "trim_matches",
    "trim_start",
    "trim_start_matches",
    "truncate",
    "try_borrow",
    "try_borrow_mut",
    "try_fold",
    "try_for_each",
    "try_into",
    "try_lock",
    "try_read",
    "try_write",
    "union",
    "unwrap",
    "unwrap_err",
    "unwrap_or",
    "unwrap_or_default",
    "unwrap_or_else",
    "unzip",
    "upgrade",
    "values",
    "values_mut",
    "wait",
    "windows",
    "with_extension",
    "with_file_name",
    "wrapping_add",
    "wrapping_sub",
    "write",
    "write_all",
    "write_fmt",
    "write_str",
    "xor",
    "zip",
];

/// Whether `receiver` is a Rust module path, every segment lowercase
/// (upstream `/^[a-z_][\w]*(?:::[a-z_]\w*)*$/`): `task`, `io`, `std::io`.
fn is_rust_module_path(receiver: &str) -> bool {
    receiver.split("::").all(|segment| {
        let mut bytes = segment.bytes();
        bytes
            .next()
            .is_some_and(|first| first.is_ascii_lowercase() || first == b'_')
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

fn is_rust_std_method(name: &str) -> bool {
    RUST_STD_METHODS.binary_search(&name).is_ok()
}

/// The prelude's enum variants. A bare one is `Option` / `Result`'s unless a
/// `use` in scope imports a project variant of that name (KEEP-RUST, P15 R4).
fn is_rust_prelude_variant(name: &str) -> bool {
    matches!(name, "Ok" | "Err" | "Some" | "None")
}

/// Whether a `use` visible at `reference`'s call site names its name or is a
/// glob. Unknown source counts as such a `use`, which keeps today's binding.
fn rust_variant_import_in_scope(reference: &RefView, context: &dyn ResolutionContext) -> bool {
    let Some(facts) = context.source_facts(&reference.file_path) else {
        return true;
    };
    let Some(offset) = rust_ref_offset(reference, &facts) else {
        return true;
    };
    facts.rust_use_in_scope(offset, &reference.reference_name)
}

/// A Rust method call whose receiver the extractor dropped (KEEP-RUST, P15):
/// `root().join(x)`, or a chain broken across lines. The resolver sends it here
/// ahead of import resolution, and nowhere else. It never binds through a
/// `use`, takes no fuzzy guess, binds only a same-named method (R2), and never
/// one that shares a std method's name (R3); [`match_by_exact_name`] applies
/// both for this shape.
pub fn match_rust_lost_receiver_call(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    match_by_exact_name(reference, context)
}

/// Sticky binding patterns for one plain identifier
/// (`localBindingPatterns`, upstream v1.6.1), anchored at the offset they are
/// tried from. `\b`/`\w` are ASCII, as in upstream's JavaScript. The
/// `function`/`class` form is matched by hand in [`binds_at_sites`].
struct LocalBindingPatterns {
    decl: Regex,
    param: Regex,
}

impl LocalBindingPatterns {
    fn new(name: &str) -> Option<Self> {
        let n = regex::escape(name);
        // `const { name } = require('./m')` / `= await import('./m')` binds an
        // IMPORT, not a shadow; group 1 carries the initializer to tell them
        // apart.
        let decl = format!(
            r"^(?-u:\b)(?:const|let|var)\s+(?:{n}(?-u:\b)|[{{\[][^;=]*?(?-u:\b){n}(?-u:\b)[^;=]*?[}}\]])\s*(?:=\s*([^;\n]*))?"
        );
        // A parameter: every token before the name in the list is itself a
        // parameter (identifier, optional type, optional default), each with
        // exactly one parse, so a string argument containing the word cannot
        // match and a failing search cannot backtrack combinatorially.
        let param = format!(
            r"^\(\s*(?:(?:\.\.\.)?[0-9A-Za-z_$]+(?:\s*(?:\?\s*)?:[^,()]+|\s*=[^,()]+|\s*),\s*)*{n}(?-u:\b)(?:\s*\??\s*:[^,()]*)?(?:\s*=[^,()]*)?(?:\s*,\s*[^()]*)?\)\s*(?::[^=;{{]*)?(?:=>|\{{)"
        );
        Some(Self {
            decl: Regex::new(&decl).ok()?,
            param: Regex::new(&param).ok()?,
        })
    }
}

/// Binding patterns by name — the same names recur file after file
/// (`LOCAL_BINDING_PATTERNS`). Compiled only for names whose binding a
/// cheap necessary-condition check could not already rule out, and capped
/// lower than upstream's 4096 because a compiled Rust regex is much larger.
fn local_binding_patterns(name: &str) -> Option<Arc<LocalBindingPatterns>> {
    const CAP: usize = 1024;
    type PatternCache = (
        HashMap<String, Option<Arc<LocalBindingPatterns>>>,
        std::collections::VecDeque<String>,
    );
    static CACHE: OnceLock<Mutex<PatternCache>> = OnceLock::new();
    let cache =
        CACHE.get_or_init(|| Mutex::new((HashMap::new(), std::collections::VecDeque::new())));
    if let Some(patterns) = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .0
        .get(name)
    {
        return patterns.clone();
    }
    let patterns = LocalBindingPatterns::new(name).map(Arc::new);
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (map, order) = &mut *guard;
    if !map.contains_key(name) {
        if map.len() >= CAP
            && let Some(oldest) = order.pop_front()
        {
            map.remove(&oldest);
        }
        map.insert(name.to_string(), patterns.clone());
        order.push_back(name.to_string());
    }
    patterns
}

fn is_import_binding_value(value: &str) -> bool {
    static IMPORT_BINDING_VALUE: OnceLock<Regex> = OnceLock::new();
    IMPORT_BINDING_VALUE
        .get_or_init(|| {
            Regex::new(r"^\s*(?:await\s+)?(?:require|import)\s*\(").expect("import binding value")
        })
        .is_match(value)
}

/// Whether a JS/TS file binds `name` itself — as a `const`/`let`/`var`/
/// `function`/`class` declaration (destructuring included) or as a parameter
/// of a function or arrow (`isLocallyBoundJsName`, upstream v1.6.1). Such a
/// binding shadows every same-named symbol in other files, so a bare call to
/// it has no cross-file candidate. Read from raw source, memoised per
/// file + name.
fn is_locally_bound_js_name(name: &str, file_path: &str, context: &dyn ResolutionContext) -> bool {
    let Some(facts) = context.source_facts(file_path) else {
        return false;
    };
    facts.js_local_binding(name, |facts| {
        let plain = !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$');
        if plain {
            binds_at_sites(facts, name)
        } else {
            binds_anywhere(facts.source(), name)
        }
    })
}

/// [`is_locally_bound_js_name`]'s patterns searched through the whole source
/// (`bindsAnywhere`), for names that are not plain identifiers.
fn binds_anywhere(source: &str, name: &str) -> bool {
    let n = regex::escape(name);
    let unanchored = |pattern: String| Regex::new(&pattern).ok();
    let decl = unanchored(format!(
        r"(?-u:\b)(?:const|let|var)\s+(?:{n}(?-u:\b)|[{{\[][^;=]*?(?-u:\b){n}(?-u:\b)[^;=]*?[}}\]])\s*(?:=\s*([^;\n]*))?"
    ));
    if let Some(decl) = decl
        && decl.captures_iter(source).any(|captures| {
            !is_import_binding_value(captures.get(1).map_or("", |value| value.as_str()))
        })
    {
        return true;
    }
    let function = unanchored(format!(r"(?-u:\b)(?:function|class)\s+{n}(?-u:\b)"));
    let param = unanchored(format!(
        r"\(\s*(?:(?:\.\.\.)?[0-9A-Za-z_$]+(?:\s*(?:\?\s*)?:[^,()]+|\s*=[^,()]+|\s*),\s*)*{n}(?-u:\b)(?:\s*\??\s*:[^,()]*)?(?:\s*=[^,()]*)?(?:\s*,\s*[^()]*)?\)\s*(?::[^=;{{]*)?(?:=>|\{{)"
    ));
    let arrow = unanchored(format!(r"(?:^|[^0-9A-Za-z_$.]){n}\s*=>"));
    [function, param, arrow]
        .into_iter()
        .flatten()
        .any(|pattern| pattern.is_match(source))
}

/// [`binds_anywhere`] for a plain identifier, tried only where a match can
/// start (`bindsAtSites`): a declaration at its keyword (in order, resuming
/// past each match as a global search does), a parameter at the `(` its list
/// opens with, and `name =>` at each arrow.
fn binds_at_sites(facts: &SourceFacts, name: &str) -> bool {
    let source = facts.source();
    let sites = facts.js_binding_sites();
    // Compile the name's patterns only once a site passes its cheap
    // necessary condition; most names never need them.
    let mut patterns: Option<Option<Arc<LocalBindingPatterns>>> = None;
    let mut patterns = || {
        patterns
            .get_or_insert_with(|| local_binding_patterns(name))
            .clone()
    };
    let mut from = 0;
    for &at in &sites.var_decls {
        if at < from {
            continue;
        }
        // Every decl match spells the name before the first `;` or `=`
        // after its keyword.
        let segment_end = source[at..]
            .find([';', '='])
            .map_or(source.len(), |offset| at + offset);
        if !source[at..segment_end].contains(name) {
            continue;
        }
        let Some(patterns) = patterns() else {
            return false;
        };
        let Some(captures) = patterns.decl.captures(&source[at..]) else {
            continue;
        };
        from = at + captures.get(0).map_or(0, |whole| whole.end());
        if !is_import_binding_value(captures.get(1).map_or("", |value| value.as_str())) {
            return true;
        }
    }
    // `\b(?:function|class)\s+name\b` at each keyword site.
    let names_declaration = |at: usize| {
        let rest = &source[at..];
        let Some(keyword) = ["function", "class"]
            .into_iter()
            .find(|keyword| rest.starts_with(keyword))
        else {
            return false;
        };
        let after = &rest[keyword.len()..];
        let spelled = after.trim_start_matches(char::is_whitespace);
        // `\b` after the name: an ASCII word/non-word transition.
        spelled.len() < after.len()
            && spelled.starts_with(name)
            && name.chars().next_back().is_some_and(is_ascii_word_char)
                != spelled[name.len()..]
                    .chars()
                    .next()
                    .is_some_and(is_ascii_word_char)
    };
    if sites.fn_decls.iter().any(|&at| names_declaration(at)) {
        return true;
    }
    let mut tried = None;
    let mut search_from = 0;
    while let Some(found) = source[search_from..].find(name) {
        let at = search_from + found;
        search_from = at + 1;
        let Some(open) = source[..at].rfind('(') else {
            continue;
        };
        if tried == Some(open) || source[..at].rfind(')').is_some_and(|close| close > open) {
            continue;
        }
        tried = Some(open);
        let Some(patterns) = patterns() else {
            return false;
        };
        if patterns.param.is_match(&source[open..]) {
            return true;
        }
    }
    // `name =>`: the name ends where the whitespace before the arrow starts.
    sites.arrows.iter().any(|&arrow| {
        let end = source[..arrow].trim_end_matches(char::is_whitespace).len();
        let Some(start) = end.checked_sub(name.len()) else {
            return false;
        };
        source.is_char_boundary(start)
            && source[start..].starts_with(name)
            && !source[..start]
                .chars()
                .next_back()
                .is_some_and(|ch| is_ascii_word_char(ch) || matches!(ch, '$' | '.'))
    })
}

/// Try to resolve a reference by exact name match (`matchByExactName`,
/// `name-matcher.ts:173-209`).
pub fn match_by_exact_name(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let bare_js = is_bare_js_call(reference, context);
    let bare_php = crate::php_scope::is_bare_php_call(reference, context);
    let lua_bare = crate::lua_scope::is_lua_bare_call(reference);
    // A type, a value or an import the file binds from a package outside the
    // repository names nothing in it, whatever kind of reference it is
    // (upstream #2134).
    if !bare_js
        && is_js_family(reference.language)
        && reference.reference_kind != EdgeKind::Calls
        && is_simple_js_identifier(&reference.reference_name)
        && is_out_of_repo_binding(&reference.reference_name, reference, context)
    {
        return None;
    }
    // A bare call bound to a store action (#1862) resolves inside that store,
    // ahead of any same-named function.
    if bare_js
        && let Some(action) = crate::js_store::match_js_store_binding_call(reference, context)
    {
        return Some(action);
    }
    // A name destructured from a call's result is what the callee returns
    // under that key (upstream #2163).
    if bare_js
        && let Some(returned) =
            crate::destructured_calls::match_destructured_call_result(reference, context)
    {
        return Some(returned);
    }
    // `import { useQuery } from '@tanstack/react-query'`, a CommonJS
    // `require('supertest')`: the bare call means the package's, and no
    // same-named project symbol (upstream #2120).
    if bare_js && is_out_of_repo_binding(&reference.reference_name, reference, context) {
        return None;
    }
    // KEEP-RUST (P15): a bare-named Rust call's shape, read from its source. A
    // method call whose receiver the extractor dropped never binds a std
    // method's name (R3).
    let rust_shape = rust_call_shape(reference, context);
    if rust_shape == Some(RustCallShape::Method) && is_rust_std_method(&reference.reference_name) {
        return None;
    }
    let rust_bare = rust_shape == Some(RustCallShape::Bare);
    let rust_method = rust_shape == Some(RustCallShape::Method);
    // A bare Rust name reaches what is in scope (upstream #2124, #2214).
    let rust_scoped = crate::rust_scope::is_rust_bare_identifier(reference);
    let same_name = context.get_nodes_by_name_shared(&reference.reference_name);
    // `NAME(...)` where NAME is a function-like macro somewhere in the project
    // is an expansion or a call to a same-named function — never the macro
    // itself (#1839), and never a type that happens to share the name (#2070:
    // expat's `PREFIX(scanRef)(…)` bound an unrelated `struct PREFIX`). Neither
    // is a candidate at all, so neither makes the reference look ambiguous or
    // counts toward the same-name ceiling.
    let macro_call = reference.reference_kind == EdgeKind::Calls
        && matches!(reference.language, Language::C | Language::Cpp)
        && same_name
            .iter()
            .any(|n| crate::c_macro_visibility::is_define_constant(n));
    let reachable: Vec<Arc<Node>> = apply_language_gate(same_name, reference)
        .into_iter()
        // An `import` node is a statement, not a definition: a reference never
        // binds one by name, its own or another file's, and an import named
        // like a definition never makes that definition look ambiguous
        // (upstream #965, `0a91d0f5`). Linking an import to what it names is
        // the import resolver's job.
        .filter(|n| n.kind != NodeKind::Import)
        .filter(|n| !macro_call || matches!(n.kind, NodeKind::Function | NodeKind::Method))
        // Nested locals are only reachable from inside their container (#1230).
        .filter(|n| is_lexically_reachable(n, reference, context))
        .collect();

    // A same-named non-type is not a supertype, and a type member is not
    // importable (#1537/#1536). Filtering BEFORE ranking — not just refusing the
    // winner afterwards — is what lets a legitimate supertype OUTRANK a
    // same-named enum member instead of the whole reference being dropped.
    let bare_go = is_bare_go_call(reference, context);
    // A plain R call is a function call, never an object's method (upstream
    // #2177).
    let bare_r = crate::r_scope::is_bare_r_call(reference, context);
    // A bare Dart call stays in the class hierarchy it is written in
    // (upstream #2145).
    let dart_bare = crate::dart_scope::is_dart_bare_call(reference, context);
    // A Dart type name — a declared type, a supertype, the `Color` of
    // `Color.red` — is in scope as a call is, or through its import prefix
    // (upstream #2386).
    let dart_type_ref = crate::dart_libraries::is_dart_type_ref(reference);
    let dart_prefix = if dart_type_ref {
        crate::dart_libraries::dart_import_prefix_of(reference, context)
    } else {
        None
    };
    // A Python call's shape at its column decides what it can mean (upstream
    // #2125): a bare call no method, a chained one a member of its last link.
    let python_shape = crate::python_values::python_call_shape(reference, context);
    // An Objective-C name in C call syntax calls a function, a block or a
    // function pointer, never a method or property; a message to `self` or
    // `super` reaches the sender's hierarchy, past the sender for `super`
    // (upstream #2150, #2179).
    let objc_shape = objc_call_shape(reference, context);
    let objc_c_call = objc_shape == Some(ObjcCallShape::CCall);
    let objc_send = objc_send_target(objc_shape);
    let objc_hierarchy = OnceCell::new();
    // A bare Java call reaches a method in scope (upstream #2126), and a bare
    // Java type name a type in reach (upstream #2236), both read once.
    let java_bare = crate::java_scope::is_java_bare_call(reference);
    let java_call_scope = OnceCell::new();
    let java_type_site = crate::java_scope::JavaTypeSite::new(reference);
    // A bare Ruby call inside a class reaches its ancestry (upstream #2147),
    // a bare CFML call in a component its `extends` chain (#2148).
    let ruby_bare = crate::ruby_scope::is_ruby_bare_call(reference);
    let cfml_bare = crate::cfml_scope::is_cfml_bare_call(reference);
    // A Kotlin call reaches a top-level declaration only where it is visible
    // (upstream #2146), a bare one only a member in reach (#2172, #2206,
    // #2232), and a standard-library name on an untyped chain link only a
    // member its receiver names (#2168).
    let kotlin_call = crate::kotlin_scope::is_kotlin_call(reference);
    let kotlin_bare =
        kotlin_call && crate::kotlin_scope::is_receiver_less_kotlin_call(reference, context);
    let kotlin_site = crate::kotlin_scope::KotlinSite::new(reference);
    let kotlin_std_chain = crate::kotlin_scope::std_chain_receiver(reference, context);
    let scala_bare = crate::scala_scope::is_scala_bare_call(reference);
    let csharp_type_ref = crate::csharp_types::is_csharp_type_ref(reference, context);
    // A bare C# name whose line shows no receiver (upstream #2151).
    let csharp_bare = crate::csharp_types::is_csharp_bare_name(reference)
        && has_no_receiver_on_line(reference, context);
    // How a bare-named Swift call is written decides what it can mean
    // (upstream #2170).
    let swift_shape = crate::swift_scope::swift_call_shape(reference, context);
    // How a bare-named Go call is written decides what it can mean (upstream
    // #2167, #2177).
    let go_shape = crate::go_calls::go_call_shape(reference, context);
    // A PHP call through `$this`, `self`, `static` or `parent` stays in the
    // class hierarchy (upstream #2142).
    let php_self = crate::php_scope::php_self_scope(reference, context);
    let candidates: Vec<Arc<Node>> = reachable
        .iter()
        .filter(|n| {
            python_shape.as_ref().is_none_or(|shape| {
                crate::python_values::fits_python_call_shape(n, shape, reference, context)
            })
        })
        .filter(|n| !(objc_c_call && crate::objc_scope::is_objc_member_kind(n.kind)))
        .filter(|n| {
            objc_send.is_none_or(|to_super| {
                crate::objc_scope::is_objc_self_send_target(
                    n,
                    reference,
                    objc_hierarchy
                        .get_or_init(|| crate::objc_scope::objc_hierarchy_at(reference, context))
                        .as_deref(),
                    to_super,
                )
            })
        })
        .filter(|n| node_is_eligible_target(reference.reference_kind, n))
        // A Scala type position (`Arbitrary[B]`) never names a method: an
        // `implicit def A: Order[A]` shares its name with half of cats' type
        // parameters (upstream #2123). Nor does a kind-projector placeholder
        // (`F[*]`, `G[?]`, upstream #2217).
        .filter(|n| {
            !(reference.language == Language::Scala
                && reference.reference_kind == EdgeKind::References
                && (reference
                    .reference_name
                    .starts_with(|c: char| c.is_ascii_uppercase())
                    || crate::scala_scope::is_symbolic_name(&reference.reference_name))
                && matches!(n.kind, NodeKind::Method | NodeKind::Function))
        })
        // A receiver-less JS/TS or Go call cannot reach a member of a type: a
        // method (#1714, #1857), nor a property, field or enum case (upstream
        // #2120: mocha's global `it(…)` bound an interface's `it` property). A
        // bare Rust call cannot reach a method (P15 R1).
        .filter(|n| !((bare_js || bare_go || bare_r) && is_type_member_kind(n.kind)))
        // A bare PHP call is a function call: nothing else is callable without
        // a receiver (upstream #2103).
        .filter(|n| !(bare_php && n.kind != NodeKind::Function))
        // Nor is a table's method (`function M.x`, `function M:x`) in Lua,
        // without its table (upstream #2202).
        .filter(|n| !(lua_bare && n.kind == NodeKind::Method))
        .filter(|n| {
            go_shape
                .as_ref()
                .is_none_or(|shape| crate::go_calls::is_go_call_target(n, shape))
        })
        // `$this->assertEquals()` in a test of a vendor base class is no
        // comparator's method (upstream #2142).
        .filter(|n| {
            php_self
                .as_ref()
                .is_none_or(|scope| n.kind == NodeKind::Method && scope.admits(n, context))
        })
        // A Vue component's own method is `this.m()` inside that component,
        // not `this.$refs['input'].click()` on an element another component
        // renders (upstream #2140).
        .filter(|n| {
            !(reference.reference_kind == EdgeKind::Calls
                && is_js_family(reference.language)
                && is_vue_component_method(n)
                && !is_this_call_in_own_file(n, reference, context))
        })
        .filter(|n| !(rust_bare && n.kind == NodeKind::Method))
        // A Rust method call whose receiver was dropped reaches only a method:
        // never a function, type, enum member or import (P15 R2).
        .filter(|n| !rust_method || n.kind == NodeKind::Method)
        // A bare `Ok` / `Err` / `Some` / `None` is the prelude's unless a `use`
        // in its scope imports a project variant of that name (P15 R4).
        .filter(|n| {
            !(rust_bare
                && n.kind == NodeKind::EnumMember
                && is_rust_prelude_variant(&reference.reference_name)
                && !rust_variant_import_in_scope(reference, context))
        })
        // Another file's item only through a `use` that binds it or a glob
        // over its module, a prelude name only through a project `use`, an
        // enum variant only through a `use` of it and its enum, a path's
        // name only in that path's module (upstream #2124, #2214).
        .filter(|n| !rust_scoped || crate::rust_scope::is_rust_name_in_scope(n, reference, context))
        // A name the file binds itself shadows every other file's symbol of
        // that name, so a bare call has no cross-file candidate.
        .filter(|n| {
            !(bare_js
                && n.file_path != reference.file_path
                && is_locally_bound_js_name(
                    &reference.reference_name,
                    &reference.file_path,
                    context,
                ))
        })
        // halo's tests' Mockito `verify(…)` is no `EmailVerificationService`
        // method (upstream #2126).
        .filter(|n| {
            !(java_bare
                && n.kind == NodeKind::Method
                && !java_call_scope
                    .get_or_init(|| crate::java_scope::JavaCallScope::of(reference, context))
                    .reaches(n, reference))
        })
        // rubocop's `format(…)`, Kernel's, is no LSP runtime's `format`: a
        // bare call in a class reaches only the class's ancestry (upstream
        // #2147).
        .filter(|n| {
            !(ruby_bare
                && n.kind == NodeKind::Method
                && !crate::ruby_scope::is_ruby_method_in_scope(n, reference, context))
        })
        // coldbox's `now()`, the built-in, is no date helper's `now`: a bare
        // call in a component reaches only its `extends` chain (upstream
        // #2148).
        .filter(|n| {
            !(cfml_bare
                && n.kind == NodeKind::Method
                && !crate::cfml_scope::is_cfml_method_in_scope(n, reference, context))
        })
        // riverpod's tests' `test(…)`, package:test's function, is no
        // `ProviderContainer.test`: a bare Dart call reaches only a member of
        // the class it is written in, of what that class extends, mixes in or
        // implements, or of an extension on one of those (upstream #2145).
        .filter(|n| {
            !(dart_bare
                && crate::dart_scope::is_dart_type_member(n, context)
                && !crate::dart_scope::is_dart_method_in_scope(n, reference, context))
        })
        // A Dart constructor is reached through its class (`Foo.named(…)`,
        // `@Foo.named(…)`), never by its name alone: not a bare call, not the
        // last link of a chain, not an annotation (riverpod's 210 `@internal`
        // annotations went to its providers' `internal` constructors,
        // upstream #2380).
        .filter(|n| {
            !(reference.language == Language::Dart
                && crate::dart_members::is_dart_constructor(n, context))
        })
        // bloc's brick hook calls `info(…)` on mason's `Logger`, no enum
        // constant of bloc_lint's: a bare Dart call reaches a type's value
        // only from inside the type, as its methods (upstream #2386).
        .filter(|n| {
            !(dart_bare
                && crate::dart_libraries::is_dart_type_value(n)
                && !crate::dart_scope::is_dart_method_in_scope(n, reference, context))
        })
        // A receiver-less Dart name reaches only what its library declares or
        // imports, ahead of the ranking, so a visible namesake wins (upstream
        // #2386).
        .filter(|n| {
            !((dart_bare || dart_type_ref)
                && !crate::dart_libraries::is_dart_top_level_visible(
                    n,
                    reference,
                    context,
                    dart_prefix.as_deref(),
                ))
        })
        // Ahead of the ranking, so a visible namesake wins: retrofit's tests'
        // `new Builder()` is not a wire converter test's nested
        // `CrashingPhone.Builder` (upstream #2236).
        .filter(|n| java_type_site.visible(n, context))
        // A bare PHP class name, only its namespace's or the imported one, ahead
        // of the ranking: koel's `extends Request` under `use
        // App\Http\Requests\API\Request;` is that class, not the first `Request`
        // indexed (upstream #2235).
        .filter(|n| crate::php_scope::is_php_class_visible(n, reference, context))
        .filter(|n| {
            kotlin_std_chain
                .as_deref()
                .is_none_or(|receiver| crate::kotlin_scope::is_std_chain_target(n, receiver))
        })
        .filter(|n| {
            !kotlin_call || crate::kotlin_scope::is_kotlin_top_level_visible(n, reference, context)
        })
        .filter(|n| !kotlin_bare || kotlin_site.reaches(n, context))
        // A project's bitwise extension on a number type is never the
        // standard library's `x and 0xff` (upstream #2174).
        .filter(|n| !crate::kotlin_scope::is_kotlin_number_bitwise(n, reference))
        // A bare Scala name reaches only a member in reach (upstream #2160,
        // #2193, #2199).
        .filter(|n| {
            !scala_bare || crate::scala_scope::is_scala_member_in_scope(n, reference, context)
        })
        // A C# type position names a type, never a member sharing its name
        // (upstream #2121).
        .filter(|n| !csharp_type_ref || crate::csharp_types::can_name_in_type_position(n))
        // A bare C# name is a member of the types around it, of their bases
        // or of a static using's type (upstream #2151).
        .filter(|n| {
            !csharp_bare || crate::csharp_types::is_csharp_member_in_scope(n, reference, context)
        })
        // A C# type name is a type its namespaces can see, and a nested one
        // only from inside its owner, ahead of the ranking, so a visible
        // namesake wins (upstream #2216, #2220).
        .filter(|n| {
            !crate::csharp_types::is_csharp_bare_type_candidate(n, reference)
                || (crate::csharp_types::is_csharp_nested_type_in_scope(n, reference, context)
                    && (n.file_path == reference.file_path
                        || crate::csharp_types::is_csharp_type_visible(n, reference, context)))
        })
        // A Scala package object's member, only where it is in scope, ahead
        // of the ranking, so cats.laws' `Eq` can be the `cats` package
        // object's (upstream #2230).
        .filter(|n| {
            !(reference.language == Language::Scala
                && n.language == Language::Scala
                && n.file_path != reference.file_path
                && !crate::scala_scope::is_scala_package_object_member_visible(
                    n, reference, context,
                ))
        })
        // Charts' `min(a, b)` is no range type's `min` field, `super.init(…)`
        // no Objective-C demo's `init` (upstream #2170).
        .filter(|n| {
            swift_shape.as_ref().is_none_or(|shape| {
                crate::swift_scope::is_swift_call_target(n, shape, reference, context)
            })
        })
        .cloned()
        .collect();
    let candidates = if kotlin_bare {
        kotlin_site.lexical(candidates, context)
    } else if dart_bare {
        // Of those, the library's own (upstream #2386), then the nearest:
        // bloc's `emit(…)` in a `Cubit` is `BlocBase.emit`, not the
        // `Emittable` interface's (upstream #2145).
        crate::dart_scope::nearest_dart_members(
            crate::dart_libraries::prefer_own_dart_library(candidates, reference, context),
            reference,
            context,
        )
    } else if dart_type_ref && dart_prefix.is_none() {
        crate::dart_libraries::prefer_own_dart_library(candidates, reference, context)
    } else if swift_shape
        .as_ref()
        .is_some_and(|shape| shape.shape != crate::swift_scope::Shape::Chained)
    {
        crate::swift_scope::nearest_swift_members(candidates, reference, context)
    } else {
        candidates
    };

    if candidates.is_empty() {
        return None;
    }

    // Branch on the PRE-FILTER count. The single-candidate shortcut skips
    // `find_best_match`, and with it the cross-language penalty that makes a
    // genuinely ambiguous reference decline — so letting the kind filter collapse
    // a multi-candidate set into that shortcut would CREATE an edge the scored
    // path refused (a GDScript `extends Node` binding a Rust `struct Node` once
    // its same-named enum-member rival was filtered out). These gates only ever
    // remove an edge, so the ranking branch is chosen by how ambiguous the
    // reference actually was, and only the CANDIDATES are narrowed.
    if reachable.len() == 1 {
        let is_cross_language = candidates[0].language != reference.language;
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: candidates[0].id.clone(),
            confidence: if is_cross_language { 0.5 } else { 0.9 },
            resolved_by: ResolvedBy::ExactMatch,
        });
    }

    // Multiple matches — narrow down (name-matcher.ts:194-206). Decline past the
    // ambiguity ceiling rather than proximity-rank a pathological candidate set.
    if candidates.len() > ambiguous_name_ceiling() {
        return None;
    }
    if let Some(best) = find_best_match(reference, &candidates) {
        let proximity = compute_path_proximity(&reference.file_path, &best.file_path);
        let confidence = if proximity >= 30.0 { 0.7 } else { 0.4 };
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: best.id.clone(),
            confidence,
            resolved_by: ResolvedBy::ExactMatch,
        });
    }

    None
}

/// Try to resolve by qualified name (`matchByQualifiedName`,
/// `name-matcher.ts:214-252`).
pub fn match_by_qualified_name(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if !reference.reference_name.contains("::") && !reference.reference_name.contains('.') {
        return None;
    }

    // A C or C++ name written from the global scope is the declaration of
    // exactly that name (upstream #2445); the partial match below would take
    // a namesake nested in another namespace or class.
    if matches!(reference.language, Language::C | Language::Cpp)
        && reference.reference_name.starts_with("::")
    {
        return crate::cpp_namespaces::match_cpp_global_name(reference, context);
    }

    let mut candidates = context.get_nodes_by_qualified_name_shared(&reference.reference_name);
    // A C# `using X.Y;` names a namespace: one the project declares, else it
    // is the file's own (external) using, never another file's using of that
    // name (upstream #2220).
    if reference.language == Language::CSharp && reference.reference_kind == EdgeKind::Imports {
        let namespaces: Vec<Arc<Node>> = candidates
            .iter()
            .filter(|n| n.kind == NodeKind::Namespace)
            .cloned()
            .collect();
        candidates = if namespaces.is_empty() {
            candidates
                .into_iter()
                .filter(|n| n.kind != NodeKind::Import || n.file_path == reference.file_path)
                .collect()
        } else {
            prefer_call_site_file(namespaces, &reference.file_path)
                .into_iter()
                .take(1)
                .collect()
        };
    }
    if candidates.len() == 1 {
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: candidates[0].id.clone(),
            confidence: 0.95,
            resolved_by: ResolvedBy::QualifiedName,
        });
    }

    // Several symbols share this exact qualified name (an ODR clash / separate
    // translation units): prefer the call site's own file before the partial
    // fallback, else the first-indexed def wins (#1079).
    if candidates.len() > 1 {
        let ordered = prefer_call_site_file(candidates, &reference.file_path);
        if ordered[0].file_path == reference.file_path {
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: ordered[0].id.clone(),
                confidence: 0.95,
                resolved_by: ResolvedBy::QualifiedName,
            });
        }
    }

    // Erlang qualified refs carry arity when statically known
    // (`mod::function/2`). A missed exact lookup with an arity must never fall
    // through to a sibling arity. An arity-less dynamic MFA ref resolves only
    // when that module defines exactly one arity of the function.
    if reference.language == Language::Erlang && reference.reference_name.contains("::") {
        if split_erlang_arity(&reference.reference_name).is_some() {
            return None;
        }
        let base = reference.reference_name.rsplit("::").next()?;
        let prefix = format!("{}/", reference.reference_name);
        let arity_candidates: Vec<Arc<Node>> = context
            .get_nodes_by_name_shared(base)
            .into_iter()
            .filter(|candidate| {
                candidate.language == Language::Erlang
                    && candidate.kind == NodeKind::Function
                    && candidate
                        .qualified_name
                        .strip_prefix(&prefix)
                        .is_some_and(is_erlang_arity_digits)
            })
            .collect();
        if arity_candidates.len() == 1 {
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: arity_candidates[0].id.clone(),
                confidence: 0.85,
                resolved_by: ResolvedBy::QualifiedName,
            });
        }
        return None;
    }

    // Partial qualified name match (name-matcher.ts:234-249), again preferring
    // the call site's own file among the candidates whose qualified name ends
    // with the reference (#1079).
    let parts: Vec<&str> = reference.reference_name.split([':', '.']).collect();
    if let Some(last_name) = parts.last().filter(|s| !s.is_empty()) {
        let partial: Vec<Arc<Node>> = context
            .get_nodes_by_name_shared(last_name)
            .into_iter()
            .filter(|c| c.qualified_name.ends_with(&reference.reference_name))
            .collect();
        if !partial.is_empty() {
            let ordered = prefer_call_site_file(partial, &reference.file_path);
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: ordered[0].id.clone(),
                confidence: 0.85,
                resolved_by: ResolvedBy::QualifiedName,
            });
        }
    }

    None
}

/// Prefer the candidate(s) declared in the call site's own file, keeping the
/// rest in their original order (`preferCallSiteFile`, name-matcher.ts:#1079).
/// A same-file definition is the strongest language-agnostic signal for which
/// of several same-named symbols a call means; without it, resolution collapses
/// onto whichever was indexed first, so a call in `b/svc` wrongly targets
/// `a/svc`. No-op when there are <2 candidates or none share the call site's
/// file. The partition is STABLE (preserves within-group order), so edge output
/// stays deterministic.
pub(crate) fn prefer_call_site_file<T: Borrow<Node>>(
    nodes: Vec<T>,
    call_site_file: &str,
) -> Vec<T> {
    if nodes.len() < 2 {
        return nodes;
    }
    let mut same: Vec<T> = Vec::new();
    let mut other: Vec<T> = Vec::new();
    for n in nodes {
        if n.borrow().file_path == call_site_file {
            same.push(n);
        } else {
            other.push(n);
        }
    }
    if same.is_empty() {
        other
    } else {
        same.extend(other);
        same
    }
}

/// Languages whose object literals declare callable members (upstream
/// `OBJECT_LITERAL_LANGUAGES`): `export const api = { call() {…} }` used as a
/// namespace (#1573), including the script blocks of single-file components
/// (#2363).
pub(crate) fn is_object_literal_language(language: Language) -> bool {
    matches!(
        language,
        Language::TypeScript
            | Language::Tsx
            | Language::JavaScript
            | Language::Jsx
            | Language::ArkTs
            | Language::Vue
            | Language::Svelte
            | Language::Astro
    )
}

pub(crate) fn range_within(inner: &Node, outer: &Node) -> bool {
    if inner.start_line < outer.start_line || inner.end_line > outer.end_line {
        return false;
    }
    if inner.start_line == outer.start_line && inner.start_column < outer.start_column {
        return false;
    }
    if inner.end_line == outer.end_line && inner.end_column > outer.end_column {
        return false;
    }
    true
}

pub(crate) fn same_range(left: &Node, right: &Node) -> bool {
    left.start_line == right.start_line
        && left.start_column == right.start_column
        && left.end_line == right.end_line
        && left.end_column == right.end_column
}

/// Resolve a direct callable/value member contained by a JS-family
/// object-literal constant or variable. Members nested inside another callable
/// body, outside the holder range, or ambiguous across distinct direct ranges
/// are deliberately rejected.
pub(crate) fn resolve_object_literal_member(
    container: &Node,
    member: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
    confidence: f64,
    resolved_by: ResolvedBy,
) -> Option<ResolvedRef> {
    if !matches!(container.kind, NodeKind::Constant | NodeKind::Variable)
        || !is_object_literal_language(container.language)
        || !same_language_family(container.language, reference.language)
        || member.is_empty()
    {
        return None;
    }

    let callable = |node: &Node| matches!(node.kind, NodeKind::Function | NodeKind::Method);
    let accepts = |node: &Node| {
        callable(node)
            || (reference.reference_kind != EdgeKind::Calls
                && matches!(
                    node.kind,
                    NodeKind::Property | NodeKind::Variable | NodeKind::Constant
                ))
    };
    // Only the literal's own property that defines the member counts — the
    // last one wins — and an identifier-valued member is followed by
    // `resolve_object_literal_binding` instead (#1932).
    let property = match object_literal_property(container, member, context) {
        LiteralLookup::Absent => return None,
        LiteralLookup::Property(property) if property.binding.is_some() => return None,
        LiteralLookup::Property(property) => Some(property),
        LiteralLookup::Unknown => None,
    };
    let inside = context
        .get_nodes_in_file_shared(&container.file_path)
        .into_iter()
        .filter(|node| node.id != container.id && range_within(node, container))
        .collect::<Vec<_>>();
    let callable_bodies = inside
        .iter()
        .filter(|node| callable(node))
        .cloned()
        .collect::<Vec<_>>();
    let mut candidates = inside
        .into_iter()
        .filter(|node| node.name == member && accepts(node))
        .filter(|node| {
            property
                .as_ref()
                .is_none_or(|property| property_contains(property, container, node, context))
        })
        .filter(|candidate| {
            !callable_bodies.iter().any(|body| {
                body.id != candidate.id
                    && !same_range(body, candidate)
                    && range_within(candidate, body)
            })
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }

    candidates.sort_by(|left, right| {
        let left_callable = !callable(left);
        let right_callable = !callable(right);
        left_callable
            .cmp(&right_callable)
            .then(left.start_line.cmp(&right.start_line))
            .then(left.start_column.cmp(&right.start_column))
            .then(left.id.cmp(&right.id))
    });
    let first = candidates.first()?;
    if candidates
        .iter()
        .skip(1)
        .any(|candidate| !same_range(candidate, first))
    {
        return None;
    }

    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: first.id.clone(),
        confidence,
        resolved_by,
    })
}

/// Resolve a method on a type, walking supertypes (`resolveMethodOnType`,
/// `name-matcher.ts:254-331`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_method_on_type(
    type_name: &str,
    method_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
    confidence: f64,
    resolved_by: ResolvedBy,
    preferred_fqn: Option<&str>,
    depth: u32,
) -> Option<ResolvedRef> {
    let method_candidates = context.get_nodes_by_name_shared(method_name);
    let want = format!("{type_name}::{method_name}");
    let matches: Vec<Arc<Node>> = method_candidates
        .into_iter()
        .filter(|m| {
            // One language family shares one type system: a `.tsx` caller
            // reaches a `.ts` class's method (upstream #1862).
            m.kind == NodeKind::Method
                && same_language_family(m.language, reference.language)
                && (m.qualified_name == want || m.qualified_name.ends_with(&format!("::{want}")))
        })
        .collect();

    // Go: only the declaring package's methods count, then what its
    // declaration embeds; `preferred_fqn` is that package's directory
    // (upstream #2323).
    if reference.language == Language::Go
        && let Some(pkg_dir) = preferred_fqn
        && let Some(scoped) = crate::go_packages::resolve_go_method_in_package(
            type_name,
            method_name,
            pkg_dir,
            &matches,
            reference,
            context,
            confidence,
            resolved_by,
            depth,
        )
    {
        return scoped;
    }

    if matches.is_empty() {
        // Conformance fallback via supertypes (name-matcher.ts:289-305).
        if depth < 4 {
            for supertype in context.get_supertypes(type_name, reference.language) {
                if let Some(via) = resolve_method_on_type(
                    &supertype,
                    method_name,
                    reference,
                    context,
                    confidence,
                    resolved_by,
                    preferred_fqn,
                    depth + 1,
                ) {
                    return Some(via);
                }
            }
        }
        // A Dart extension's member: no declaration of the type has it, so an
        // extension `on` the type (or a supertype) supplies it — `s.shout()`
        // on an enum through `extension ShapeInfo on Shape` (upstream #2338).
        if reference.language == Language::Dart
            && depth == 0
            && let Some(member) = crate::dart_members::dart_extension_member_of(
                type_name,
                method_name,
                reference,
                context,
            )
        {
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: member.id.clone(),
                confidence,
                resolved_by,
            });
        }
        return None;
    }

    if matches.len() > 1 && reference.language != Language::Go {
        if let Some(fqn) = preferred_fqn {
            let ext = if reference.language == Language::Kotlin {
                ".kt"
            } else {
                ".java"
            };
            let fqn_path = format!("{}{}", fqn.replace('.', "/"), ext);
            if let Some(chosen) = matches.iter().find(|m| {
                let fp = m.file_path.replace('\\', "/");
                fp.ends_with(&fqn_path) || fp.ends_with(&format!("/{fqn_path}"))
            }) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: chosen.id.clone(),
                    confidence,
                    resolved_by,
                });
            }
        }
    }

    // Language-agnostic ambiguity: several same-named methods survive (ODR clash
    // / separate translation units). Prefer the call site's own file (#1079).
    // Runs AFTER the preferred_fqn block, so Java/Kotlin import disambiguation
    // (#314), whose target is intentionally in another file, is unaffected.
    let ordered = prefer_call_site_file(matches, &reference.file_path);
    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: ordered[0].id.clone(),
        confidence,
        resolved_by,
    })
}

/// Last `::`-separated segment of a C++ name (`cppLastSegment`,
/// `name-matcher.ts:572-575`).
fn cpp_last_segment(name: &str) -> &str {
    name.split("::")
        .filter(|s| !s.is_empty())
        .last()
        .unwrap_or(name)
}

/// Does the graph hold a class/struct/union named `name`'s last segment?
/// (`cppClassExists`, `name-matcher.ts:621-626`).
fn cpp_class_exists(name: &str, reference: &RefView, context: &dyn ResolutionContext) -> bool {
    let last = cpp_last_segment(name);
    context.get_nodes_by_name_shared(last).iter().any(|n| {
        matches!(n.kind, NodeKind::Class | NodeKind::Struct | NodeKind::Union)
            && n.language == reference.language
    })
}

/// Escape a receiver for embedding in a `Regex` (the JS
/// `/[.*+?^${}()|[\]\\]/g` set, `name-matcher.ts:523`).
pub(crate) fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '.' | '*' | '+' | '?' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Declarator regex matching `Type recv`, `Type* recv`, `Type<X> recv`, etc.,
/// requiring a declarator terminator after the receiver (`buildDeclaratorRegex`,
/// `name-matcher.ts:506-510`). The terminator is matched manually (the regex
/// crate has no lookahead), so this captures up to and through the receiver word.
fn build_declarator_regex(escaped_receiver: &str) -> Regex {
    Regex::new(&format!(
        r"([A-Za-z_][\w:]*(?:\s*<[^;=(){{}}]+>)?(?:\s*[*&]+)?)\s*\b{escaped_receiver}\b"
    ))
    .expect("declarator regex")
}

/// Does a `>` in a declared C++ type close no `<` (upstream #2443
/// `cppTypeBeganAbove`)? Then the type began on an earlier line (rocksdb's
/// `std::unique_ptr<BlobContents>>>& blob_reqs` under
/// `autovector<std::pair<BlobRangeReadRequest*,`), or the text is an
/// expression (`i < a->b->c`).
fn cpp_type_began_above(type_name: &str) -> bool {
    let mut depth = 0usize;
    for c in type_name.chars() {
        match c {
            '<' => depth += 1,
            '>' => match depth.checked_sub(1) {
                Some(open) => depth = open,
                None => return true,
            },
            _ => {}
        }
    }
    false
}

/// A C++ declarator scan's captured type (`std::vector<Slice>*`,
/// `autovector<std::pair<int, FileMetaData*>>&`) as receiver inference reads
/// it (upstream `normalizeCppTypeName`, the shape after #2443): `&` / `*`,
/// cv-qualifiers, `typename` / `class` / `struct` and the template
/// arguments, nested ones with them, dropped (`Table<int, Box<int>>` →
/// `Table`, never `Table >`, which names no class). `None` when the text
/// names no type: what is left of a type begun on an earlier line names a
/// template argument's type, not the declared one, and `return x` /
/// `delete x` declare nothing. Upstream keeps only the last `::` segment; the
/// namespaces a declaration writes stay here (`port::Mutex`), as they always
/// have, and [`resolve_method_on_type`] matches the qualified tail. A capture
/// ending in `::` is the receiver's own qualifier, not a type: leveldb's
/// `leveldb::g_env = leveldb::Env::Default();` declares nothing, and the scan
/// goes on to `leveldb::Env* g_env` (upstream reads the type `leveldb` there;
/// this scan never has).
fn normalize_cpp_type_name(raw: &str) -> Option<String> {
    if cpp_type_began_above(raw) {
        return None;
    }
    let bare = crate::cpp_type_aliases::strip_template_arguments(&raw.replace(['&', '*'], " "));
    let normalized = bare
        .split_whitespace()
        .filter(|word| {
            !matches!(
                *word,
                "const" | "volatile" | "mutable" | "typename" | "class" | "struct"
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    let last = normalized.rsplit("::").next()?.trim();
    (!last.is_empty() && !CPP_NON_TYPE_TOKENS.contains(&last)).then_some(normalized)
}

/// A C++ source line as code, for reading declarations (upstream #2413
/// `cppCodeOf`): `None` for a line of a comment (`// …`, ` * …`), else the
/// line with its comments blanked, columns kept. Receiver inference read
/// leveldb's `// … non-null imm_` as a declaration of `imm_` with the type
/// `null`.
fn cpp_code_of(line: &str) -> Option<String> {
    static COMMENT_LINE: OnceLock<Regex> = OnceLock::new();
    static BLOCK: OnceLock<Regex> = OnceLock::new();
    let comment_line =
        COMMENT_LINE.get_or_init(|| Regex::new(r"^\s*(?://|\*)").expect("comment line"));
    if comment_line.is_match(line) {
        return None;
    }
    let block = BLOCK.get_or_init(|| Regex::new(r"/\*.*?\*/").expect("block comment"));
    let mut code = block
        .replace_all(line, |found: &regex::Captures<'_>| {
            " ".repeat(found[0].len())
        })
        .into_owned();
    // A block comment opening here runs past the line.
    if let Some(open) = code.find("/*") {
        code.truncate(open);
    }
    if let Some(comment) = code.find("//") {
        code.truncate(comment);
    }
    Some(code)
}

/// Patterns for a line that declares a C++ receiver in a form the
/// declarator pattern doesn't read (upstream #2413 `cppRebindsReceiver`): a
/// range-`for` variable (`for (const Foo& x : xs)`) or a structured binding
/// (`auto [x, y] = …`).
struct CppRebinding {
    range_variable: Regex,
    binding: Regex,
}

impl CppRebinding {
    fn new(escaped_receiver: &str) -> Option<Self> {
        Some(Self {
            range_variable: Regex::new(&format!(r"\b{escaped_receiver}\s*:")).ok()?,
            binding: Regex::new(&format!(
                r"\bauto\s*&{{0,2}}\s*\[[^\]]*\b{escaped_receiver}\b[^\]]*\]"
            ))
            .ok()?,
        })
    }

    fn rebinds(&self, line: &str) -> bool {
        static FOR: OnceLock<Regex> = OnceLock::new();
        let range_for = FOR.get_or_init(|| Regex::new(r"\bfor\s*\(").expect("range for"));
        // `x :` after a `for (`, not `x::`.
        let in_range_for = range_for.find(line).is_some_and(|open| {
            let rest = &line[open.end()..];
            self.range_variable
                .find_iter(rest)
                .any(|variable| !rest[variable.end()..].starts_with(':'))
        });
        in_range_for || self.binding.is_match(line)
    }
}

/// The char following the receiver word must be a declarator terminator
/// (`;=,)[{(` or end-of-line) — mirrors the upstream lookahead `(?=[;=,)\[{(]|$)`
/// (`name-matcher.ts:508`), which the regex crate cannot express directly.
fn declarator_terminator_ok(line: &str, match_end: usize) -> bool {
    match line[match_end..].chars().find(|c| !c.is_whitespace()) {
        None => true,
        Some(c) => matches!(c, ';' | '=' | ',' | ')' | '[' | '{' | '('),
    }
}

/// Infer the type of an `auto`-declared local from its initializer on the
/// declaration line (`inferCppAutoInitializerType`, `name-matcher.ts:675-695`).
fn infer_cpp_auto_initializer_type(
    line: &str,
    receiver_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
    depth: u32,
) -> Option<String> {
    let escaped = regex_escape(receiver_name);
    let assign = Regex::new(&format!(r"\b{escaped}\b\s*=\s*([^;]+)")).ok()?;
    let init = assign.captures(line)?.get(1)?.as_str().trim();

    static NEW_RE: OnceLock<Regex> = OnceLock::new();
    let new_re = NEW_RE.get_or_init(|| Regex::new(r"^new\s+([A-Za-z_][\w:]*)").expect("new re"));
    if let Some(caps) = new_re.captures(init) {
        return Some(cpp_last_segment(caps.get(1)?.as_str()).to_string());
    }

    static CALL_RE: OnceLock<Regex> = OnceLock::new();
    let call_re = CALL_RE
        .get_or_init(|| Regex::new(r"^([A-Za-z_][\w:]*(?:\s*<[^>;]*>)?)\s*\(").expect("call re"));
    if let Some(caps) = call_re.captures(init) {
        let callee: String = caps.get(1)?.as_str().split_whitespace().collect();
        return resolve_cpp_call_result_type(&callee, reference, context, depth + 1);
    }
    None
}

/// Infer the class produced by a C++ call/construction expression, using return
/// types captured at extraction (`resolveCppCallResultType`,
/// `name-matcher.ts:638-668`). The caller still validates the outer method via
/// `resolve_method_on_type`, so a wrong guess stays silent.
fn resolve_cpp_call_result_type(
    inner: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
    depth: u32,
) -> Option<String> {
    if depth > 3 {
        return None;
    }
    let expr = inner.trim();

    static MAKE_RE: OnceLock<Regex> = OnceLock::new();
    let make_re = MAKE_RE.get_or_init(|| {
        Regex::new(r"(?:^|::)(?:make_unique|make_shared)\s*<\s*([A-Za-z_]\w*)").expect("make re")
    });
    if let Some(caps) = make_re.captures(expr) {
        return Some(caps.get(1)?.as_str().to_string());
    }

    // Single-level member call `recv.method` (name-matcher.ts:651-659).
    if let Some(dot_idx) = expr.rfind('.') {
        if dot_idx > 0 {
            let recv = &expr[..dot_idx];
            let method = &expr[dot_idx + 1..];
            if recv.contains('.') || recv.contains('(') || recv.contains("::") {
                return None;
            }
            let recv_type = infer_cpp_receiver_type(recv, reference, context, depth + 1, None)?;
            return lookup_callee_return_type(
                &format!("{recv_type}::{method}"),
                reference,
                context,
            );
        }
    }

    if let Some(ret) = lookup_callee_return_type(expr, reference, context) {
        return Some(ret);
    }

    // Direct construction — the callee itself names a class/struct.
    if cpp_class_exists(expr, reference, context) {
        return Some(cpp_last_segment(expr).to_string());
    }
    None
}

/// What C++ receiver inference made of a receiver's declared type (upstream
/// #2399 `CppReceiverDeclaration`).
#[derive(Debug, Default)]
struct CppReceiverDeclaration {
    /// The declared type is an alias, followed to the type it names or naming
    /// no particular class (a template parameter's type, `typename
    /// Traits::Field`, a `decltype(…)`).
    aliased: bool,
    /// The type as written in the declaration, read the way upstream's
    /// `normalizeCppTypeName` reads it, without following an alias (`Table`).
    written: String,
    /// The type is reached through a pointer (`Table* t`, `using Field =
    /// const FieldDescriptor*;`).
    pointer: bool,
    /// The type is one of the project's class templates, whose
    /// specializations may add members.
    class_template: bool,
    /// The declared type as written (`std::vector<Slice>*`).
    raw: String,
    /// The declaration is the calling function's own, or a member of its
    /// class ([`is_cpp_callers_declaration`]).
    callers: bool,
    /// On its way back to the declaration the scan passed one of the
    /// receiver it could not read a type from (`auto x = Make();`, `for
    /// (Foo& x : xs)`, `auto [x, y] = …`, the end of a declaration begun on an
    /// earlier line), so the declaration it found may be an earlier
    /// variable's.
    shadowed: bool,
}

/// A declared C++ type as receiver inference reads it: as the declaration
/// wrote it, or, when it is an alias the caller's own declaration uses, the
/// type the alias names (`cpp_type_aliases`), `None` when that type can't be
/// known (upstream #2399 `cppDeclaredType`). An alias is never a class, so its
/// own name is never looked up as one: that is how protobuf's `Field f;
/// f->number()` (`using Field = const FieldDescriptor*;`) reached the
/// generated `Field` message's `number`.
fn cpp_declared_type(
    raw: &str,
    normalized: String,
    in_caller_scope: bool,
    reference: &RefView,
    context: &dyn ResolutionContext,
    found: Option<&mut CppReceiverDeclaration>,
) -> Option<String> {
    // A declaration read from elsewhere (an earlier function, another class)
    // may not be this receiver's, so it is taken as it was before aliases
    // were followed.
    if !in_caller_scope {
        return Some(normalized);
    }
    let aliased = crate::cpp_type_aliases::resolve_cpp_aliased_type(raw, reference, context);
    let followed = match &aliased {
        crate::cpp_type_aliases::CppAliased::NotAnAlias => return Some(normalized),
        crate::cpp_type_aliases::CppAliased::Unreadable => None,
        crate::cpp_type_aliases::CppAliased::Followed(followed) => Some(followed),
    };
    if let Some(found) = found {
        found.aliased = true;
        found.written = cpp_declared_type_name(raw).unwrap_or_default();
        found.pointer = followed.map_or_else(
            || crate::cpp_type_aliases::is_cpp_pointer_type(raw),
            |followed| followed.pointer,
        );
        found.class_template = followed.is_some_and(|followed| followed.class_template);
    }
    followed.map(crate::cpp_type_aliases::cpp_aliased_type_name)
}

/// Is line `line` of `file`, which declares a C++ receiver, the calling
/// function's own code, or a member declaration of the caller's class
/// (upstream #2399 `isCppCallersDeclaration`, #2413's shape)? Receiver
/// inference reads back to the top of the file and through the header, so a
/// declaration it finds elsewhere (an earlier function, another class in the
/// header, a class nested in the caller's: rocksdb's
/// `MultiScan::MultiScanIterator` has a `scan_opts_` of its own) may be
/// another variable of the same name. The scan has blanked comments.
fn is_cpp_callers_declaration(
    file: &str,
    line: i64,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    let Some(caller) = context.get_node_by_id_shared(&reference.from_node_id) else {
        return false;
    };
    if !matches!(caller.kind, NodeKind::Method | NodeKind::Function) {
        return false;
    }
    if file == reference.file_path && line >= caller.start_line && line <= reference.line {
        return true;
    }
    let Some((owner, _)) = caller.qualified_name.rsplit_once("::") else {
        return false;
    };
    let mut innermost: Option<Arc<Node>> = None;
    for class in context.get_nodes_in_file_shared(file) {
        if !matches!(
            class.kind,
            NodeKind::Class | NodeKind::Struct | NodeKind::Union
        ) {
            continue;
        }
        let end = if class.end_line != 0 {
            class.end_line
        } else {
            class.start_line
        };
        if line < class.start_line || line > end {
            continue;
        }
        if innermost
            .as_ref()
            .is_none_or(|inner| class.start_line >= inner.start_line)
        {
            innermost = Some(class);
        }
    }
    innermost.is_some_and(|class| class.qualified_name == owner)
}

/// The operator the C++ member call `reference` is written with (upstream
/// #2413 `cppMemberOperator`): `.` for `receiver.method(…)`, `->` through a
/// pointer, iterator or smart pointer, `None` when the source doesn't show
/// it.
fn cpp_member_operator(
    receiver: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<&'static str> {
    let facts = context.source_facts(&reference.file_path)?;
    let index = usize::try_from(reference.line - 1)
        .ok()
        .filter(|&index| index < facts.line_count())?;
    let mut rest = facts
        .line(index)
        .get(usize::try_from(reference.column).unwrap_or(0)..)
        .and_then(|at| at.strip_prefix(receiver))?;
    // `symbols_by_parent_` ending its line, `.insert(…)` starting the next.
    let mut next = index + 1;
    while rest.trim().is_empty() && next < facts.line_count().min(index + 1 + 3) {
        rest = facts.line(next);
        next += 1;
    }
    let rest = rest.trim_start();
    if rest.starts_with('.') {
        Some(".")
    } else if rest.starts_with("->") {
        Some("->")
    } else {
        None
    }
}

/// Is the C++ member call `reference` written `receiver.method(…)`, not
/// through `->` (upstream #2399 `isCppDotCall`)?
fn is_cpp_dot_call(receiver: &str, reference: &RefView, context: &dyn ResolutionContext) -> bool {
    cpp_member_operator(receiver, reference, context) == Some(".")
}

/// The C and C++ types the project declares under `name` (upstream #2413
/// `cppTypesNamed`).
fn cpp_types_named(name: &str, context: &dyn ResolutionContext) -> Vec<Arc<Node>> {
    context
        .get_nodes_by_name_shared(name)
        .into_iter()
        .filter(|node| {
            matches!(node.language, Language::C | Language::Cpp)
                && matches!(
                    node.kind,
                    NodeKind::Class
                        | NodeKind::Struct
                        | NodeKind::Union
                        | NodeKind::Enum
                        | NodeKind::Interface
                        | NodeKind::TypeAlias
                )
        })
        .collect()
}

/// Is a C++ declared type, as written (`std::vector<Slice>*`, `const
/// absl::flat_hash_set<int>&`), one from outside the project with a
/// lowercase name, as the standard library's and abseil's are (upstream
/// #2413 `isCppLibraryType`)? [`is_undeclared_type_name`] takes the
/// capitalized ones. The name is qualified, by namespaces that are no
/// project type's (`Foo::kMask & key` reads like a declaration of `key`),
/// and the project declares no type of that name: in `std` itself for a
/// `std::` name (googletest's `testing::internal::string` is no
/// `std::string`), anywhere for any other, since a namespace a macro opens
/// (fmt's `FMT_BEGIN_NAMESPACE`) is in no qualified name. An unqualified
/// name is not known to be std's: fmt's `using namespace std;` is inside
/// `namespace adl { … }`, and its `basic_string_view` is fmt's own. Nor is a
/// type whose `>`s outnumber its `<`s, the end of a declaration begun on an
/// earlier line. A `_t` name has no members of its own: a scalar
/// (`std::size_t`, so the declaration is another variable's), a tag, or a
/// trait naming another type (rocksdb's `std::conditional_t<kIsDigested,
/// void*, Slice> dict`).
fn is_cpp_library_type(raw: &str, context: &dyn ResolutionContext) -> bool {
    if raw.matches('<').count() != raw.matches('>').count() {
        return false;
    }
    let Some(segments) = crate::cpp_type_aliases::cpp_type_segments(raw) else {
        return false;
    };
    let Some((name, qualifiers)) = segments.split_last().filter(|(_, rest)| !rest.is_empty())
    else {
        return false;
    };
    if !name.starts_with(|c: char| c.is_ascii_lowercase()) || name.ends_with("_t") {
        return false;
    }
    if qualifiers
        .iter()
        .any(|qualifier| !cpp_types_named(qualifier, context).is_empty())
    {
        return false;
    }
    let declared = cpp_types_named(name, context);
    if qualifiers[0] != "std" {
        return declared.is_empty();
    }
    let spelled = segments.join("::");
    let tail = format!("::{spelled}");
    !declared
        .iter()
        .any(|node| node.qualified_name == spelled || node.qualified_name.ends_with(&tail))
}

/// Record the C++ declaration receiver inference settles on (upstream #2413
/// `noteCppDeclaration`).
fn note_cpp_declaration(found: Option<&mut CppReceiverDeclaration>, raw: &str, callers: bool) {
    if let Some(found) = found {
        found.raw = raw.to_string();
        found.callers = callers;
        found.pointer = crate::cpp_type_aliases::is_cpp_pointer_type(raw);
    }
}

/// Infer a C++ receiver's declared type by scanning the source backwards from
/// the call line for its declarator (`inferCppReceiverType`,
/// `name-matcher.ts:512-567`). Handles `Foo x;`, `auto x = Foo::make();`, and a
/// `.h/.hpp/.hxx` header fallback. Comments are no declarations (upstream
/// #2413). A declared type the caller's own code spells through an alias is
/// the type the alias names (upstream #2399), and `found` records what the
/// declaration said and whether a re-declaration the scan could not read
/// passed on the way.
fn infer_cpp_receiver_type(
    receiver_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
    depth: u32,
    mut found: Option<&mut CppReceiverDeclaration>,
) -> Option<String> {
    let source = context.read_file(&reference.file_path)?;
    let lines: Vec<&str> = source
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .collect();
    if lines.is_empty() {
        return None;
    }
    let call_line_index = ((reference.line - 1).max(0) as usize).min(lines.len() - 1);
    let escaped = regex_escape(receiver_name);
    let receiver_pattern = Regex::new(&format!(r"\b{escaped}\b")).ok()?;
    let declarator_regex = build_declarator_regex(&escaped);
    let mut rebinding: Option<Option<CppRebinding>> = None;

    for i in (0..=call_line_index).rev() {
        let text = lines[i];
        if text.is_empty() || !receiver_pattern.is_match(text) {
            continue;
        }
        let Some(line) = cpp_code_of(text) else {
            continue;
        };
        if !receiver_pattern.is_match(&line) {
            continue;
        }
        let declarator = declarator_regex.captures(&line).filter(|caps| {
            caps.get(0)
                .is_some_and(|whole| declarator_terminator_ok(&line, whole.end()))
        });
        if let Some(caps) = declarator {
            let raw = caps.get(1).map_or("", |m| m.as_str());
            match normalize_cpp_type_name(raw) {
                Some(normalized) if normalized == "auto" => {
                    // `auto x = Foo::instance();` — recover the type from the
                    // initializer (call return type / construction).
                    if let Some(init_type) = infer_cpp_auto_initializer_type(
                        &line,
                        receiver_name,
                        reference,
                        context,
                        depth,
                    ) {
                        return Some(init_type);
                    }
                    // No usable initializer on this line: keep scanning
                    // earlier ones, where what is declared is an earlier
                    // variable.
                    if let Some(found) = found.as_deref_mut() {
                        found.shadowed = true;
                    }
                }
                Some(normalized) => {
                    let in_caller_scope = is_cpp_callers_declaration(
                        &reference.file_path,
                        i as i64 + 1,
                        reference,
                        context,
                    );
                    note_cpp_declaration(found.as_deref_mut(), raw, in_caller_scope);
                    return cpp_declared_type(
                        raw,
                        normalized,
                        in_caller_scope,
                        reference,
                        context,
                        found,
                    );
                }
                // The end of a declaration begun on an earlier line, maybe the
                // receiver's: one found further up may be another variable
                // (upstream #2443).
                None if cpp_type_began_above(raw) => {
                    if let Some(found) = found.as_deref_mut() {
                        found.shadowed = true;
                    }
                }
                None => {}
            }
        } else if let Some(found) = found.as_deref_mut()
            && rebinding
                .get_or_insert_with(|| CppRebinding::new(&escaped))
                .as_ref()
                .is_some_and(|rebinding| rebinding.rebinds(&line))
        {
            found.shadowed = true;
        }
    }

    // Header fallback: `.h/.hpp/.hxx` sibling declaring the receiver.
    static EXT_RE: OnceLock<Regex> = OnceLock::new();
    let ext_re = EXT_RE.get_or_init(|| Regex::new(r"(?i)\.(?:c|cc|cpp|cxx)$").expect("ext re"));
    let mut header_candidates: Vec<String> = Vec::new();
    for ext in [".h", ".hpp", ".hxx"] {
        let candidate = ext_re.replace(&reference.file_path, ext).to_string();
        if candidate != reference.file_path && !header_candidates.contains(&candidate) {
            header_candidates.push(candidate);
        }
    }
    for header_path in header_candidates {
        if !context.file_exists(&header_path) {
            continue;
        }
        let Some(header_source) = context.read_file(&header_path) else {
            continue;
        };
        for (i, text) in header_source
            .split('\n')
            .map(|l| l.trim_end_matches('\r'))
            .enumerate()
        {
            if !receiver_pattern.is_match(text) {
                continue;
            }
            let Some(line) = cpp_code_of(text) else {
                continue;
            };
            let Some(caps) = declarator_regex.captures(&line) else {
                continue;
            };
            let Some(whole) = caps.get(0) else { continue };
            if !declarator_terminator_ok(&line, whole.end()) {
                continue;
            }
            let raw = caps.get(1).map_or("", |m| m.as_str());
            if let Some(normalized) = normalize_cpp_type_name(raw).filter(|n| n != "auto") {
                let in_caller_scope =
                    is_cpp_callers_declaration(&header_path, i as i64 + 1, reference, context);
                note_cpp_declaration(found.as_deref_mut(), raw, in_caller_scope);
                return cpp_declared_type(
                    raw,
                    normalized,
                    in_caller_scope,
                    reference,
                    context,
                    found,
                );
            }
        }
    }
    None
}

/// Infer a Java/Kotlin receiver's declared type from the field declaration in
/// the class enclosing the call site (`inferJavaFieldReceiverType`,
/// `name-matcher.ts:878-925`). Covers Spring `@Autowired`/`@Resource` field
/// injection where the field name doesn't match the type by convention.
fn infer_java_field_receiver_type(
    receiver_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let in_file = context.get_nodes_in_file_shared(&reference.file_path);
    if in_file.is_empty() {
        return None;
    }

    // Tightest class/interface enclosing the call line (latest start).
    let mut enclosing: Option<&Node> = None;
    for n in &in_file {
        if !matches!(n.kind, NodeKind::Class | NodeKind::Interface)
            || n.language != reference.language
        {
            continue;
        }
        let end = if n.end_line != 0 {
            n.end_line
        } else {
            n.start_line
        };
        if n.start_line <= reference.line && end >= reference.line {
            if enclosing.is_none_or(|e| n.start_line >= e.start_line) {
                enclosing = Some(n);
            }
        }
    }
    let enclosing = enclosing?;
    let enclosing_end = if enclosing.end_line != 0 {
        enclosing.end_line
    } else {
        enclosing.start_line
    };

    let field = in_file.iter().find(|n| {
        n.kind == NodeKind::Field
            && n.name == receiver_name
            && n.language == reference.language
            && n.start_line >= enclosing.start_line
            && (if n.end_line != 0 {
                n.end_line
            } else {
                n.start_line
            }) <= enclosing_end
    })?;
    let signature = field.signature.as_ref().filter(|s| !s.is_empty())?;

    // Signature shape "<TypeName> <fieldName>" (extractField): pull the type,
    // strip generics + dotted package + array/varargs markers.
    let before_name = &signature[..signature.rfind(&field.name)?];
    let type_raw = before_name.trim();
    if type_raw.is_empty() {
        return None;
    }
    static GENERICS_RE: OnceLock<Regex> = OnceLock::new();
    let generics_re = GENERICS_RE.get_or_init(|| Regex::new(r"<[^>]*>").expect("generics re"));
    let type_no_generics = generics_re.replace_all(type_raw, "");
    let type_no_array = type_no_generics
        .replace("[]", "")
        .replace("[ ]", "")
        .trim_end_matches("...")
        .trim()
        .to_string();
    let last_part = type_no_array
        .split(|c: char| c == '.' || c.is_whitespace())
        .rfind(|s| !s.is_empty())?;
    if !last_part.chars().next().is_some_and(|c| c.is_uppercase()) {
        return None;
    }
    Some(last_part.to_string())
}

/// Tokens a loose receiver pattern might capture that are never a user-defined
/// type (`NON_TYPE_RECEIVER_TOKENS`, name-matcher.ts:#1108).
fn is_non_type_receiver_token(seg: &str) -> bool {
    matches!(
        seg,
        "this"
            | "self"
            | "super"
            | "new"
            | "return"
            | "await"
            | "yield"
            | "typeof"
            | "null"
            | "nil"
            | "None"
            | "true"
            | "false"
            | "True"
            | "False"
            | "undefined"
    )
}

/// Normalize a captured type expression to a simple type name: drop generic
/// args and pointer/ref markers, take the last `.`/`::`-qualified segment, and
/// reject obvious non-types (`normalizeInferredTypeName`, name-matcher.ts:#1108).
pub(crate) fn normalize_inferred_type_name(raw: &str) -> Option<String> {
    static GENERICS: OnceLock<Regex> = OnceLock::new();
    let generics = GENERICS.get_or_init(|| Regex::new(r"<[^>]*>").expect("inferred generics re"));
    let cleaned = generics.replace_all(raw, "");
    let cleaned = cleaned.replace(['&', '*'], "");
    let seg = cleaned.split(['.', ':']).rfind(|s| !s.is_empty())?;
    if is_non_type_receiver_token(seg) {
        return None;
    }
    Some(seg.to_string())
}

/// Per-language patterns recovering a local variable's (or typed parameter's)
/// declared type from its declaration/initializer line. Each regex captures the
/// type in group 1; `r` is the already-escaped receiver name. Ordered
/// most-specific first. The type-annotation / typed-parameter forms (#1125) are
/// baked into the second pattern per language. Languages without a pattern set
/// (the C++ path uses its own dedicated inferrer) return an empty vector.
pub(crate) fn local_receiver_type_patterns(
    language: Language,
    r: &str,
) -> Vec<(Regex, PatternCheck)> {
    local_receiver_type_patterns_tagged(language, r)
}

/// What a receiver-type pattern needs checked beyond its regex, which the
/// `regex` crate cannot say: it has no look-around.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternCheck {
    /// Nothing beyond the match.
    None,
    /// The Lua/Luau `receiver: Type` ANNOTATION: `lg:Log()` is byte-identical
    /// to the Luau annotation `lg: Logger`, so the call form is rejected here
    /// (#1124).
    LuaAnnotation,
    /// A Python `name = Cls(…)` is a statement of its own: one whose line goes
    /// on to end in a comma is a keyword argument inside a call
    /// (`prefix=IPNetwork(…),`, upstream #2169 `(?![^\n]*,\s*$)`).
    PythonStatement,
}

impl PatternCheck {
    /// Whether a match of the pattern in `text` (`whole`, with its type
    /// `capture`) passes.
    pub fn passes(self, text: &str, whole: regex::Match<'_>, capture: regex::Match<'_>) -> bool {
        match self {
            Self::None => true,
            Self::LuaAnnotation => !lua_annotation_is_method_call(text, capture.end()),
            Self::PythonStatement => {
                let rest = &text[whole.end()..];
                let line_end = rest.find('\n').unwrap_or(rest.len());
                !rest[..line_end]
                    .rfind(',')
                    .is_some_and(|comma| rest[comma + 1..].trim().is_empty())
            }
        }
    }
}

/// The receiver-type patterns of `language`, each with the check its match
/// needs.
fn local_receiver_type_patterns_tagged(language: Language, r: &str) -> Vec<(Regex, PatternCheck)> {
    let build = |src: String| Regex::new(&src).ok().map(|re| (re, PatternCheck::None));
    let pats: Vec<String> = match language {
        Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx => vec![
            format!(r"\b{r}\b\s*=\s*new\s+([A-Za-z_$][\w.$]*)"),
            format!(r"\b{r}\b\s*:\s*([A-Z][\w.$]*)"),
        ],
        Language::Python => {
            let mut out = Vec::new();
            for (src, check) in [
                // group = VLANGroup.objects.create(…): a Django manager call
                // that returns one instance of the model, not `filter` / `all`
                // (a QuerySet; upstream #2175).
                (
                    format!(
                        r"(?:^|;)\s*{r}\s*=\s*([A-Z]\w*)\.objects\.(?:create|get|first|last|latest|earliest|get_by_natural_key)\s*\("
                    ),
                    PatternCheck::None,
                ),
                // lg = Logger(...), a statement of its own (upstream #2169).
                (
                    format!(r"(?:^|;)\s*{r}\s*=\s*([A-Z][\w.]*)\s*\("),
                    PatternCheck::PythonStatement,
                ),
                // Quoted forward references are ordinary Python annotations
                // (and the form produced by `from __future__ import
                // annotations`). Keep this stricter shape before the unquoted
                // annotation pattern so `obj: "pkg.Type"` recovers the same
                // receiver type as `obj: pkg.Type` (#1684).
                (
                    format!(r#"\b{r}\b\s*:\s*["']([A-Z][\w.]*)["']"#),
                    PatternCheck::None,
                ),
                (format!(r"\b{r}\b\s*:\s*([A-Z][\w.]*)"), PatternCheck::None),
            ] {
                if let Ok(re) = Regex::new(&src) {
                    out.push((re, check));
                }
            }
            return out;
        }
        // `FMResultSet *rs = …`, `NSString * _Nullable name;`, a block's
        // `^(FMResultSet *rs)`, never a function `Foo *rs(void)` (upstream's
        // `(?!\s*\()`, read here as the character after the name), and a
        // method parameter, `- (void)read:(nullable FMResultSet *)rs`
        // (upstream #2150).
        Language::ObjC => vec![
            format!(
                r"\b([A-Z]\w*)\s*(?:<[^<>;]*>\s*)?\*\s*(?:(?:_Nullable|_Nonnull|__strong|__weak|__unsafe_unretained|const)\s+)*{r}\b(?:\s*$|\s*[^\s(])"
            ),
            format!(
                r"\(\s*(?:(?:nullable|nonnull|__kindof)\s+)*([A-Z]\w*)\s*(?:<[^<>)]*>\s*)?\*[^)]*\)\s*{r}\b"
            ),
        ],
        // A generic or array type (`Pair<L, R> pair`, `String[] args`) and an
        // enhanced-for variable (upstream #2171).
        Language::Java => vec![
            format!(r"\b{r}\b\s*=\s*new\s+([A-Za-z_][\w.]*)"),
            format!(r"\b([A-Z][\w.]*)\s*{TYPE_ARGS}\s+{r}\b\s*[=;,)]"),
            format!(r"\bfor\s*\(\s*(?:final\s+)?([A-Z][\w.]*)\s*{TYPE_ARGS}\s+{r}\s*:"),
        ],
        Language::Kotlin => vec![
            format!(r"\b{r}\b\s*=\s*([A-Z][\w.]*)\s*\("),
            format!(r"\b{r}\b\s*:\s*([A-Z][\w.]*)"),
        ],
        // A generic, nullable or array type (`List<string> names`,
        // `JProperty? p`) and a `foreach` variable (upstream #2171).
        Language::CSharp => vec![
            format!(r"\b{r}\b\s*=\s*new\s+([A-Za-z_][\w.]*)"),
            format!(r"\b([A-Z][\w.]*)\s*{TYPE_ARGS}\??\s+{r}\b\s*[=;,)]"),
            format!(r"\bforeach\s*\(\s*([A-Z][\w.]*)\s*{TYPE_ARGS}\??\s+{r}\s+in\b"),
        ],
        Language::Swift => vec![
            format!(r"\b{r}\b\s*=\s*([A-Z][\w.]*)\s*\("),
            format!(r"\b{r}\b\s*:\s*([A-Z][\w.]*)"),
        ],
        // CFML (upstream #1155): `svc = new UserService()` / `new
        // path.to.UserService()` (the type is the last segment; the path
        // tells same-named components apart, `cfml_scope::typed_receiver_call`),
        // `createObject("component", "path.to.X")` and the one-argument
        // `createObject("path.to.X")`, a typed cfscript parameter
        // (`required UserService svc`; CFML's built-in types are lowercase by
        // convention), a `<cfargument>` and a component `property` (WireBox's
        // `inject` included), either attribute order. The scan reads raw
        // lines, so tag markup (`<cfset svc = new X()>`) matches too.
        Language::Cfml => vec![
            format!(r"\b{r}\b\s*=\s*new\s+([A-Za-z_][\w.]*)"),
            format!(
                r#"\b{r}\b\s*=\s*[Cc]reate[Oo]bject\s*\(\s*["']component["']\s*,\s*["']([\w.]+)["']"#
            ),
            format!(r#"\b{r}\b\s*=\s*[Cc]reate[Oo]bject\s*\(\s*["']([\w.]+)["']\s*\)"#),
            format!(r"\b([A-Z][\w.]*)\s+{r}\b\s*[=;,)]"),
            format!(
                r#"(?i)\bcfargument[^>\n]*\bname\s*=\s*["']{r}["'][^>\n]*\btype\s*=\s*["']([\w.]+)["']"#
            ),
            format!(
                r#"(?i)\bcfargument[^>\n]*\btype\s*=\s*["']([\w.]+)["'][^>\n]*\bname\s*=\s*["']{r}["']"#
            ),
            format!(
                r#"(?i)\b(?:cf)?property\b[^;\n]*\bname\s*=\s*["']{r}["'][^;\n]*\b(?:type|inject)\s*=\s*["']([\w.]+)["']"#
            ),
            format!(
                r#"(?i)\b(?:cf)?property\b[^;\n]*\b(?:type|inject)\s*=\s*["']([\w.]+)["'][^;\n]*\bname\s*=\s*["']{r}["']"#
            ),
        ],
        Language::Rust => vec![
            format!(r"\blet\s+(?:mut\s+)?{r}\b(?:\s*:[^=]+)?\s*=\s*&?(?:mut\s+)?([A-Z]\w*)"),
            format!(r"\b{r}\s*:\s*&?(?:mut\s+)?([A-Z]\w*)"),
        ],
        Language::Go => vec![
            format!(r"\b{r}\b\s*:=\s*&?([A-Za-z_][\w.]*)\s*\{{"),
            format!(r"\bvar\s+{r}\s+\*?([A-Za-z_][\w.]*)"),
            // A method receiver or parameter of an unexported type — `func (s
            // *server)`, `func (c *cache[T])`, `func handle(h *handler)`, a
            // line of a multi-line parameter list or `var (…)` block: a
            // lowercase type only where a parameter list puts it, after `(`,
            // `,` or nothing and before `,`, `)` or the line's end (upstream
            // #2323).
            format!(r"(?:^|[(,])\s*{r}\s+\*?([a-z_]\w*)\s*(?:\[[^\]]*\])?\s*(?:[,)]|$)"),
            format!(r"\b{r}\s+\*?([A-Z][\w.]*)"),
        ],
        Language::Ruby => vec![format!(r"\b{r}\b\s*=\s*([A-Z][\w:]*)\.new\b")],
        Language::Scala => vec![
            format!(r"\b{r}\b\s*=\s*(?:new\s+)?([A-Z][\w.]*)"),
            format!(r"\b{r}\b\s*:\s*([A-Z][\w.]*)"),
        ],
        Language::Dart => vec![
            format!(r"\b{r}\b\s*=\s*([A-Z][\w.]*)\s*\("),
            format!(r"\b([A-Z][\w.]*)\s+{r}\b\s*[=;,)]"),
        ],
        Language::Php => vec![
            format!(r"\$?{r}\b\s*=\s*new\s+([A-Za-z_\\][\w\\]*)"),
            format!(r"\b([A-Za-z_\\][\w\\]*)\s+&?\${r}\b"),
        ],
        Language::Lua | Language::Luau => {
            let mut out: Vec<(Regex, PatternCheck)> = Vec::new();
            for src in [
                format!(r"\b{r}\b\s*=\s*([A-Z]\w*)\.new\b"),
                format!(r"\b{r}\b\s*=\s*([A-Z]\w*)\s*\("),
            ] {
                if let Some(pattern) = build(src) {
                    out.push(pattern);
                }
            }
            if let Ok(re) = Regex::new(&format!(r"\b{r}\b\s*:\s*([A-Z][\w.]*)")) {
                out.push((re, PatternCheck::LuaAnnotation));
            }
            return out;
        }
        Language::R => vec![format!(r"\b{r}\b\s*(?:<-|<<-|=)\s*([A-Z][\w.]*)\$new\b")],
        Language::Pascal => vec![
            format!(r"\b{r}\b\s*:\s*([A-Z]\w*)"),
            format!(r"\b{r}\b\s*:=\s*([A-Z][\w.]*)\.Create\b"),
        ],
        _ => Vec::new(),
    };
    pats.into_iter().filter_map(build).collect()
}

/// 1-based start line of the tightest function/method enclosing the call
/// (`enclosingScopeStartLine`, name-matcher.ts:#1108). Bounds the backward scan
/// so a same-named variable in another function can't leak in.
pub(crate) fn enclosing_scope_start_line(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> i64 {
    scope_start_line_in(
        context
            .get_nodes_in_file_shared(&reference.file_path)
            .iter()
            .map(AsRef::as_ref),
        reference.language,
        reference.line,
    )
}

/// [`enclosing_scope_start_line`] of 1-based `line` among a file's `nodes`.
pub(crate) fn scope_start_line_in<'n>(
    nodes: impl IntoIterator<Item = &'n Node>,
    language: Language,
    line: i64,
) -> i64 {
    let mut start = 1i64;
    for n in nodes {
        if !matches!(n.kind, NodeKind::Function | NodeKind::Method) || n.language != language {
            continue;
        }
        let end = if n.end_line != 0 {
            n.end_line
        } else {
            n.start_line
        };
        if n.start_line <= line && end >= line && n.start_line >= start {
            start = n.start_line;
        }
    }
    start
}

/// Infer a receiver's type from its local declaration/initializer in the
/// enclosing function body (`inferLocalReceiverType`, name-matcher.ts:#1108).
/// Language-dispatched via `local_receiver_type_patterns`; returns `None` for
/// languages without patterns or when no declaration is found. Bounded to the
/// enclosing scope. The caller validates the method via `resolve_method_on_type`,
/// so a mis-inference produces no edge.
#[cfg(test)]
pub(crate) fn infer_local_receiver_type(
    receiver_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    infer_local_receiver_declaration(receiver_name, reference, context).map(|(name, _)| name)
}

/// [`infer_local_receiver_type`] with the type as the declaration writes it
/// beside its name (`a.UserService` beside `UserService`): a CFML component
/// path says which of several same-named components it is (W4-CF-02;
/// upstream keeps it as `decl.raw`, for Go).
pub(crate) fn infer_local_receiver_declaration(
    receiver_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<(String, String)> {
    // CFML scopes (upstream #1155): `variables.svc` / `this.svc` name a
    // component field whose assignment or `property` usually lives outside
    // the calling function (the init pseudo-constructor, WireBox injection),
    // and `local.svc` / `arguments.svc` a function local or argument. The
    // scope is stripped so the declarations match the bare name, and a
    // component field is looked for in the whole file: the nearest
    // declaration above the call still wins, then the rest of the file below.
    let (scan_receiver, component_scoped) = match cfml_scoped_receiver(receiver_name, reference) {
        Some((scope, member)) => (
            member,
            matches!(scope, CfmlScope::Variables | CfmlScope::This),
        ),
        None => (receiver_name, false),
    };
    let escaped = regex_escape(scan_receiver);
    // A CFML, Objective-C, PHP or Dart scan compiles its patterns only once a
    // line could match one ([`line_may_declare`]): compiled for every member
    // call, CFML's eight made re-resolving a file of a few hundred calls take
    // seconds, and a PHP pair costs a millisecond and a megabyte.
    let lazy = matches!(
        reference.language,
        Language::Cfml | Language::ObjC | Language::Php | Language::Dart
    );
    let compiled: OnceCell<Arc<Vec<(Regex, PatternCheck)>>> = OnceCell::new();
    let patterns = || {
        compiled.get_or_init(|| {
            context.compiled_patterns(
                &format!("receiver:{}:{escaped}", reference.language.as_str()),
                &|| local_receiver_type_patterns_tagged(reference.language, &escaped),
            )
        })
    };
    if !lazy && patterns().is_empty() {
        return None;
    }
    let source = context.read_file(&reference.file_path)?;
    let lines: Vec<&str> = source
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .collect();
    if lines.is_empty() {
        return None;
    }
    let call_idx = ((reference.line - 1).max(0) as usize).min(lines.len() - 1);
    let start_idx = if component_scoped {
        0
    } else {
        (enclosing_scope_start_line(reference, context) - 1).max(0) as usize
    };
    let match_line = |line: &str| -> Option<(String, String)> {
        // A generated/minified line is not where a human declaration lives,
        // and regexing multi-KB text per ref is pure waste — skip it.
        if line.len() > 10_000 {
            return None;
        }
        if lazy && !line_may_declare(reference.language, line, scan_receiver) {
            return None;
        }
        for (re, check) in patterns().iter() {
            if let Some(caps) = re.captures(line) {
                if let (Some(whole), Some(m)) = (caps.get(0), caps.get(1)) {
                    if !check.passes(line, whole, m) {
                        continue;
                    }
                    if let Some(ty) = normalize_inferred_type_name(m.as_str()) {
                        return Some((ty, m.as_str().to_string()));
                    }
                }
            }
        }
        None
    };
    for i in (start_idx..=call_idx).rev() {
        if let Some(declared) = match_line(lines[i]) {
            return Some(declared);
        }
    }
    if component_scoped {
        for line in &lines[call_idx + 1..] {
            if let Some(declared) = match_line(line) {
                return Some(declared);
            }
        }
    }
    None
}

/// Whether `line` could match a lazily compiled receiver pattern of
/// `language` for `receiver`; never false where one does.
fn line_may_declare(language: Language, line: &str, receiver: &str) -> bool {
    match language {
        Language::Cfml => cfml_line_may_declare(line, receiver),
        // Both Objective-C forms write a `*` and the receiver.
        Language::ObjC => line.contains('*') && line.contains(receiver),
        // `$x = new T` writes the receiver, then past whitespace `=` and
        // `new`; `T $x` writes `$` and the receiver after whitespace (an `&`
        // between).
        Language::Php => {
            writes_assignment_of(line, receiver, "new") || writes_typed_variable(line, receiver)
        }
        // `x = Foo(` and `Foo x;` both write the receiver followed, past
        // whitespace, by `=`, `;`, `,` or `)`.
        Language::Dart => writes_before_terminator(line, receiver),
        _ => true,
    }
}

/// Whether `line` writes `name` followed, past whitespace, by `=`, `;`, `,`
/// or `)`: what `{name}\b\s*[=;,)]` needs (never false where that matches).
fn writes_before_terminator(line: &str, name: &str) -> bool {
    occurrences(line, name).any(|at| {
        line[at + name.len()..]
            .trim_start()
            .starts_with(['=', ';', ',', ')'])
    })
}

/// The byte offsets where `needle` starts in `text`, overlapping ones
/// included (`aa` twice in `aaa`).
fn occurrences<'t>(text: &'t str, needle: &'t str) -> impl Iterator<Item = usize> + 't {
    let step = needle.chars().next().map_or(1, char::len_utf8);
    let mut from = 0;
    std::iter::from_fn(move || {
        if needle.is_empty() || from > text.len() {
            return None;
        }
        let at = from + text[from..].find(needle)?;
        from = at + step;
        Some(at)
    })
}

/// Whether `text` writes `name` followed, past whitespace, by `=` and then,
/// past whitespace, by `next`: what `{name}\b\s*=\s*{next}` needs (never
/// false where that matches).
fn writes_assignment_of(text: &str, name: &str, next: &str) -> bool {
    occurrences(text, name).any(|at| {
        text[at + name.len()..]
            .trim_start()
            .strip_prefix('=')
            .is_some_and(|rest| rest.trim_start().starts_with(next))
    })
}

/// Whether `text` writes `$name` after a word and whitespace, an optional
/// `&` between: what `[\w\\]\s+&?\$name` needs (never false where that
/// matches; any non-ASCII character counts as a word's).
fn writes_typed_variable(text: &str, name: &str) -> bool {
    let variable = format!("${name}");
    occurrences(text, &variable).any(|at| {
        let before = &text[..at];
        let before = before.strip_suffix('&').unwrap_or(before);
        before.ends_with(char::is_whitespace)
            && before
                .trim_end()
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '\\' || !c.is_ascii())
    })
}

/// Whether `line` could match a CFML receiver pattern for `receiver`
/// ([`local_receiver_type_patterns_tagged`]); never false where one does.
/// The `new`, `createObject` and typed-parameter forms all write the
/// receiver followed, past whitespace, by `=`, `;`, `,` or `)`; the
/// `<cfargument>` and `property` forms, which read the receiver in any case,
/// an `=` on a line naming `cfargument` or `property` (no letter of either
/// folds to a non-ASCII one, so ASCII lowercasing finds every case).
fn cfml_line_may_declare(line: &str, receiver: &str) -> bool {
    if line.contains('=') {
        let lower = line.to_ascii_lowercase();
        if lower.contains("cfargument") || lower.contains("property") {
            return true;
        }
    }
    receiver.is_empty() || writes_before_terminator(line, receiver)
}

/// The CFML scope a receiver is written through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CfmlScope {
    Variables,
    This,
    Local,
    Arguments,
}

/// A CFML receiver written through a scope (`variables.svc`,
/// `arguments.svc`; upstream's `/^(variables|this|local|arguments)\.(.+)$/i`):
/// the scope and the rest.
fn cfml_scoped_receiver<'r>(
    receiver: &'r str,
    reference: &RefView,
) -> Option<(CfmlScope, &'r str)> {
    if reference.language != Language::Cfml {
        return None;
    }
    let (scope, rest) = receiver.split_once('.')?;
    if rest.is_empty() {
        return None;
    }
    let scope = match scope.to_ascii_lowercase().as_str() {
        "variables" => CfmlScope::Variables,
        "this" => CfmlScope::This,
        "local" => CfmlScope::Local,
        "arguments" => CfmlScope::Arguments,
        _ => return None,
    };
    Some((scope, rest))
}

/// The C# keyword type (`string`, `int`, …) a local or parameter named
/// `receiver_name` is declared with between the enclosing scope's start and
/// the call, as [`infer_local_receiver_type`] scans (KEEP-RUST, W3-05c).
fn csharp_keyword_local_type(
    receiver_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let pattern = Regex::new(&format!(
        r"(?-u:\b)(string|object|int|long|short|byte|bool|char|double|float|decimal|uint|ulong|ushort|sbyte)\??\s+{}(?-u:\b)\s*[=;,)]",
        regex_escape(receiver_name)
    ))
    .ok()?;
    let source = context.read_file(&reference.file_path)?;
    let lines: Vec<&str> = source
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .collect();
    if lines.is_empty() {
        return None;
    }
    let call_idx = ((reference.line - 1).max(0) as usize).min(lines.len() - 1);
    let start_idx = (enclosing_scope_start_line(reference, context) - 1).max(0) as usize;
    (start_idx..=call_idx)
        .rev()
        .filter(|&i| lines[i].len() <= 10_000)
        .find_map(|i| pattern.captures(lines[i]).map(|caps| caps[1].to_string()))
}

/// Span end of `node`, falling back to its start when `end_line` was not
/// recorded — the same convention [`enclosing_scope_start_line`] applies.
fn span_end_line(node: &Node) -> i64 {
    if node.end_line != 0 {
        node.end_line
    } else {
        node.start_line
    }
}

/// Second chance for a receiver that is a CLASS FIELD rather than a local
/// (#1566, ESM only).
///
/// [`enclosing_scope_start_line`] bounds the backward scan to the enclosing
/// FUNCTION or METHOD, so a field declared above a method is structurally
/// unreachable from a call inside it. This reads the field's OWN declaration
/// line instead: the tightest enclosing class-like node spanning the call, then
/// its `Property`/`Field` member of the receiver's name, then the existing
/// [`local_receiver_type_patterns`] applied to that single line.
///
/// Deliberately a bounded NODE lookup, not a widened scan window. The window
/// scan takes the nearest match ABOVE the call, so a class-wide window would
/// prefer a sibling method's `const conn = new Wrong()` over the field —
/// precisely the cross-method leak [`enclosing_scope_start_line`] exists to
/// prevent. Reading one line that lies outside every method body cannot leak.
///
/// Only ever turns inference `None` into `Some`: the caller reaches this after
/// the function-scope scan already declined, so no currently-succeeding
/// inference can change. The recovered type is still subject to
/// [`is_receiver_type_bound_at_call_site`], so an unbound one (a built-in
/// `Map`) is refused rather than resolved.
fn infer_class_field_receiver_type(
    receiver_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    if !is_esm_language(reference.language) {
        return None;
    }
    let escaped = regex_escape(receiver_name);
    let patterns = local_receiver_type_patterns_tagged(reference.language, &escaped);
    if patterns.is_empty() {
        return None;
    }

    let in_file = context.get_nodes_in_file_shared(&reference.file_path);
    let enclosing = in_file
        .iter()
        .filter(|n| {
            matches!(
                n.kind,
                NodeKind::Class | NodeKind::Struct | NodeKind::Interface
            ) && n.language == reference.language
                && n.start_line <= reference.line
                && span_end_line(n) >= reference.line
        })
        .max_by_key(|n| n.start_line)?;

    let want = format!("{}::{}", enclosing.qualified_name, receiver_name);
    let field = in_file.iter().find(|n| {
        matches!(n.kind, NodeKind::Property | NodeKind::Field)
            && n.name == receiver_name
            && n.qualified_name == want
            && n.language == reference.language
    })?;

    let source = context.read_file(&reference.file_path)?;
    let line = source
        .split('\n')
        .nth((field.start_line - 1).max(0) as usize)?
        .trim_end_matches('\r');
    if line.len() > 10_000 {
        return None;
    }
    for (re, check) in &patterns {
        if let Some(caps) = re.captures(line) {
            if let (Some(whole), Some(m)) = (caps.get(0), caps.get(1)) {
                if !check.passes(line, whole, m) {
                    continue;
                }
                if let Some(ty) = normalize_inferred_type_name(m.as_str()) {
                    return Some(ty);
                }
            }
        }
    }
    None
}

/// Resolve a JS/TS `this.<field>.<method>()` exclusively through the field's
/// declaration on the enclosing class. Unproven/builtin/ambiguous types remain
/// unresolved rather than falling through to receiver-name heuristics. An ES
/// private field (`#items`) is a name of its own: it never reads a public
/// `items` declaration, nor the reverse (#1987).
fn match_ts_this_field_call(
    field: &str,
    method_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if !is_ts_field_name(field) {
        return None;
    }
    let written = context.get_node_by_id_shared(&reference.from_node_id)?;
    // An arrow member of an object literal written in a method has that
    // method's `this` (upstream #2363).
    let caller = crate::object_owners::this_scope_caller(written, context);
    let (owner_qualified, _) = caller.qualified_name.rsplit_once("::")?;
    let owner_name = owner_qualified.rsplit("::").next()?;
    let owners = context
        .get_nodes_by_name_shared(owner_name)
        .into_iter()
        .filter(|node| {
            node.file_path == reference.file_path
                && same_language_family(node.language, reference.language)
                && matches!(node.kind, NodeKind::Class | NodeKind::Component)
                && node.qualified_name == owner_qualified
        })
        .collect::<Vec<_>>();
    if owners.len() != 1 {
        return None;
    }
    let owner = &owners[0];
    let facts = context.source_facts(&owner.file_path)?;
    let declaration = facts.ts_field_declaration(&owner.id, field, |facts| {
        scan_ts_field_declaration(facts, owner, field, context)
    })?;

    if declaration.value_type {
        let value_name = declaration.type_name.rsplit('.').next().unwrap_or_default();
        let holders = prefer_call_site_file(
            context.get_nodes_by_name_shared(value_name),
            &reference.file_path,
        );
        for holder in holders.into_iter().filter(|node| {
            matches!(node.kind, NodeKind::Constant | NodeKind::Variable)
                && same_language_family(node.language, reference.language)
        }) {
            if let Some(resolved) = resolve_object_literal_member(
                &holder,
                method_name,
                reference,
                context,
                0.85,
                ResolvedBy::InstanceMethod,
            ) {
                return Some(resolved);
            }
        }
        return None;
    }

    let type_name = declaration.type_name.rsplit('.').next().unwrap_or_default();
    if !type_name
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_uppercase())
    {
        return None;
    }
    resolve_method_on_type(
        type_name,
        method_name,
        reference,
        context,
        0.85,
        ResolvedBy::InstanceMethod,
        None,
        0,
    )
}

/// A public (`mailer`) or ES private (`#mailer`) class field name.
fn is_ts_field_name(field: &str) -> bool {
    match field.strip_prefix('#') {
        Some(private) => {
            !private.is_empty()
                && private
                    .chars()
                    .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '$'))
        }
        None => is_word(field),
    }
}

/// `this.#field.method` → (`#field`, `method`): upstream's
/// `/^this\.(#[\w$]+)\.(\w+)$/` (#1987).
fn split_private_this_field_call(name: &str) -> Option<(&str, &str)> {
    let (field, method) = name.strip_prefix("this.")?.split_once('.')?;
    (field.starts_with('#') && is_ts_field_name(field) && is_word(method))
        .then_some((field, method))
}

/// The first declaration of `field` on the enclosing class's own lines of the
/// comment-free source. Inside a method body only a `this.<field>` assignment
/// counts (`this.mailer = new Mailer()` in a constructor) — a method-local
/// parameter or variable of the same name is not the field — while a
/// constructor's parameter properties (`constructor(private mailer: Mailer)`)
/// read like class-level declarations.
fn scan_ts_field_declaration(
    facts: &SourceFacts,
    owner: &Node,
    field: &str,
    context: &dyn ResolutionContext,
) -> Option<TsFieldDeclaration> {
    let member_prefix = format!("{}::", owner.qualified_name);
    let owner_members = context
        .get_nodes_in_file_shared(&owner.file_path)
        .into_iter()
        .filter(|node| {
            matches!(node.kind, NodeKind::Method | NodeKind::Function)
                && node.qualified_name.starts_with(&member_prefix)
        })
        .collect::<Vec<_>>();
    let (code, lines) = facts.ts_comment_free();
    let start = owner.start_line.saturating_sub(1) as usize;
    let end = (owner.end_line.max(owner.start_line) as usize).min(lines.line_count());
    for index in start..end {
        let line = lines.line(code, index);
        // Every declaration pattern spells the field literally.
        if line.len() > 10_000 || !line.contains(field) {
            continue;
        }
        let source_line = (index + 1) as i64;
        let this_only = owner_members
            .iter()
            .find(|member| {
                source_line >= member.start_line
                    && source_line <= member.end_line.max(member.start_line)
            })
            .is_some_and(|member| {
                !(member.name == "constructor" && declares_parameter_property(line, field))
            });
        if let Some(declaration) = ts_field_on_line(line, field, this_only) {
            return Some(declaration);
        }
    }
    None
}

/// The declaration patterns, tried in order after a whole-token occurrence of
/// the field: `field: typeof Value` (the type OF a value — tried first, or the
/// declared-type pattern would capture the word `typeof`), a declared type
/// `field?: Mailer` (the capture stops at `<`, `[` or `|`, so a generic or
/// union yields its head), and an initializer `field = new Mailer()`.
fn ts_field_patterns() -> &'static [(Regex, bool); 3] {
    static PATTERNS: OnceLock<[(Regex, bool); 3]> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            (
                Regex::new(r"^\s*[?!]?\s*:\s*(?:readonly\s+)?typeof\s+([A-Za-z_$][\w.$]*)")
                    .expect("typeof field pattern"),
                true,
            ),
            (
                Regex::new(r"^\s*[?!]?\s*:\s*(?:readonly\s+)?([A-Za-z_$][\w.$]*)")
                    .expect("declared field pattern"),
                false,
            ),
            (
                Regex::new(r"^\s*=\s*new\s+([A-Za-z_$][\w.$]*)")
                    .expect("initialized field pattern"),
                false,
            ),
        ]
    })
}

/// The first declaration of `field` on one line, pattern by pattern as the
/// upstream field regexes would find it. The field must be a whole `[\w$#]`
/// token — the `(?<![\w$#])` lookbehind of upstream v1.6.1 — so a public
/// `items` never matches `#items` (#1987); with `this_only`, it must also be
/// written `this.<field>`. KEEP-RUST: a field ending in `$` (`users$`) is a
/// whole token too, where upstream's trailing `\b` rejects it.
fn ts_field_on_line(line: &str, field: &str, this_only: bool) -> Option<TsFieldDeclaration> {
    let is_token = |ch: char| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '$' | '#');
    let ends = line
        .match_indices(field)
        .filter(|(at, _)| {
            let before = &line[..*at];
            !before.chars().next_back().is_some_and(is_token)
                && !line[at + field.len()..]
                    .chars()
                    .next()
                    .is_some_and(is_token)
                && (!this_only
                    || before
                        .strip_suffix("this.")
                        .is_some_and(|rest| !rest.chars().next_back().is_some_and(is_token)))
        })
        .map(|(at, _)| at + field.len())
        .collect::<Vec<_>>();
    ts_field_patterns()
        .iter()
        .find_map(|(pattern, value_type)| {
            ends.iter().find_map(|&end| {
                pattern
                    .captures(&line[end..])
                    .and_then(|captures| captures.get(1))
                    .map(|capture| TsFieldDeclaration {
                        value_type: *value_type,
                        type_name: capture.as_str().to_string(),
                    })
            })
        })
}

/// Whether a constructor line declares `field` as a parameter property
/// (`private readonly mailer: Mailer`). A private name is never one.
fn declares_parameter_property(line: &str, field: &str) -> bool {
    static PARAMETER_PROPERTY: OnceLock<Regex> = OnceLock::new();
    PARAMETER_PROPERTY
        .get_or_init(|| {
            Regex::new(
                r"\b(?:public|protected|private|readonly)(?:\s+(?:public|protected|private|readonly))*\s+([\w$]+)",
            )
            .expect("parameter property pattern")
        })
        .captures_iter(line)
        .any(|captures| captures.get(1).is_some_and(|name| name.as_str() == field))
}

/// Is the inferred receiver type `type_name` BOUND at the call site (#1566)?
///
/// A built-in receiver (`new Map<string, string>()`) normalizes to a bare type
/// name that some unrelated project file may also declare. Resolving a method on
/// it then fabricates an edge — and does so through a SUCCESSFUL typed path when
/// that file happens to declare a same-named method, so refusing only after
/// typed resolution FAILS would miss the worst case.
///
/// Two indexed reads answer the question for the [`ESM_LANGUAGES`] family, where
/// cross-file visibility requires an explicit import:
/// 1. a same-file declaration that binds the name ([`declares_type_name`]), or
/// 2. an import mapping whose `local_name` is that name.
///
/// Reading the IMPORTING file's `local_name` means a barrel `export *` chain
/// needs no specifier resolution. Existence is deliberately NOT the test: a
/// project `class Map` in an unimported file exists and must still not hijack.
///
/// Known limit: a type made global by an ambient `.d.ts` declaration and used
/// without an import reads as unbound and is refused. That yields a MISSING
/// edge, not a false one.
///
/// [`ESM_LANGUAGES`]: crate::types::ESM_LANGUAGES
fn is_receiver_type_bound_at_call_site(
    type_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    if context
        .get_nodes_in_file_shared(&reference.file_path)
        .iter()
        .any(|n| n.name == type_name && declares_type_name(n.kind))
    {
        return true;
    }
    context
        .get_import_mappings(&reference.file_path, reference.language)
        .iter()
        .any(|m| m.local_name == type_name)
}

/// True when a Lua/Luau `receiver:Capitalized` annotation match is really a
/// method call (`lg:Log()`, `lg:Log"s"`, `lg:Log{t}`, or a longer `lg:Log.More`
/// token the annotation regex stopped short of), which must NOT be read as a type
/// annotation (#1124). Reproduces upstream's `(?![\w.]|\s*[({"'\[])` look-ahead in
/// code, since the `regex` crate has no look-ahead. `capture_end` is the byte
/// offset just past the captured type on `line`.
fn lua_annotation_is_method_call(line: &str, capture_end: usize) -> bool {
    let rest = &line[capture_end..];
    if let Some(next) = rest.chars().next() {
        if next.is_alphanumeric() || next == '_' || next == '.' {
            return true;
        }
    }
    let trimmed = rest.trim_start();
    trimmed
        .chars()
        .next()
        .is_some_and(|c| matches!(c, '(' | '"' | '\'' | '[' | '{'))
}

/// The PHP `$this->prop->method()` receiver shape — encoded by the extractor as
/// `this->prop.method` (a SINGLE property segment, no `()`). Matches the upstream
/// `PHP_PROP_SHAPE = /^this->\w+\.\w+$/` (`index.ts:47`). Deeper chains
/// (`this->a->b.method`) and non-`this` receivers do NOT match, so they stay
/// unlinked exactly as before.
pub(crate) fn is_php_property_receiver_shape(name: &str) -> bool {
    static SHAPE: OnceLock<Regex> = OnceLock::new();
    SHAPE
        .get_or_init(|| Regex::new(r"^this->\w+\.\w+$").expect("php prop shape re"))
        .is_match(name)
}

/// Capture form of the PHP property receiver shape: `^(this->\w+)\.(\w+)$` —
/// group 1 the `this->prop` receiver, group 2 the method (`phpThisPropMatch`,
/// `name-matcher.ts:1477`).
fn php_this_prop_call_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(this->\w+)\.(\w+)$").expect("php this-prop call re"))
}

/// Patterns that recover a PHP class property's declared type for a `$this->prop`
/// receiver (`phpPropertyTypePatterns`, `name-matcher.ts:1367-1384`). Deliberately
/// NOT `local_receiver_type_patterns`: only PROPERTY-shaped declarations qualify —
/// a modifier-prefixed typed declaration (covering both a typed property
/// `private ?Foo $prop;` and a promoted constructor parameter
/// `private readonly Foo $prop`), and the pseudoconstructor assignment
/// `$this->prop = new Foo(...)`. A bare `X $prop` parameter or `$prop = new X()`
/// local elsewhere in the file must NOT match — those variables can never alias
/// `$this->prop`. Union-typed properties (`Foo|Bar $prop`) yield no match and thus
/// no edge (silent beats wrong). The classic untyped-property-assigned-in-constructor
/// shape is handled by `infer_php_assigned_property_type` instead. `escaped_prop`
/// is already regex-escaped; the type is captured in group 1.
fn php_property_type_patterns(escaped_prop: &str) -> Vec<Regex> {
    let build = |src: String| Regex::new(&src).ok();
    [
        // private readonly ?Foo $prop  (typed property / promoted ctor param)
        format!(
            r"\b(?:(?:private|protected|public|readonly|static|final)(?:\(set\))?\s+)+\??([A-Za-z_\\][\w\\]*)\s+&?\${escaped_prop}\b"
        ),
        // $this->prop = new Foo()
        format!(r"\$this->{escaped_prop}\b\s*=\s*new\s+([A-Za-z_\\][\w\\]*)"),
    ]
    .into_iter()
    .filter_map(build)
    .collect()
}

/// Second-chance typing for a PHP `$this->prop` receiver whose property
/// declaration carries no static type (classic pre-7.4 style): find the
/// `$this->prop = $var` assignment, then recover `$var`'s type from its own
/// declaration WITHIN the assignment's function — the constructor's (possibly
/// multi-line) parameter list, a typed setter's parameter, or a `= new X()`
/// local. The backward scan stops at the enclosing `function` line (checked for a
/// match first — a single-line `__construct(Foo $var) { ... }` carries the typed
/// parameter itself), so a same-named variable in another method can never type
/// the property. Ports `inferPhpAssignedPropertyType` (`name-matcher.ts:1396-1445`).
fn infer_php_assigned_property_type(
    prop: &str,
    escaped_prop: &str,
    lines: &[&str],
    call_idx: usize,
    context: &dyn ResolutionContext,
) -> Option<String> {
    // Compiled only once a line writes `$this->prop =` and then a `$`.
    let compiled: OnceCell<Arc<Vec<(Regex, PatternCheck)>>> = OnceCell::new();
    let property = format!("this->{prop}");
    let assign_at = |i: usize| -> Option<String> {
        let line = lines.get(i)?;
        if line.len() > 10_000 || !writes_assignment_of(line, &property, "$") {
            return None;
        }
        compiled
            .get_or_init(|| {
                context.compiled_patterns(&format!("php-assignment:{escaped_prop}"), &|| {
                    Regex::new(&format!(r"\$this->{escaped_prop}\b\s*=\s*\$(\w+)\b"))
                        .ok()
                        .map(|re| (re, PatternCheck::None))
                        .into_iter()
                        .collect()
                })
            })
            .first()?
            .0
            .captures(line)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
    };

    // The assignment is position-independent relative to the call — nearest-
    // backward first, then sweep forward, same order as the componentScoped scan.
    let mut assign_idx: Option<usize> = None;
    let mut var_name: Option<String> = None;
    for i in (0..=call_idx.min(lines.len().saturating_sub(1))).rev() {
        if let Some(v) = assign_at(i) {
            assign_idx = Some(i);
            var_name = Some(v);
            break;
        }
    }
    if var_name.is_none() {
        for i in (call_idx + 1)..lines.len() {
            if let Some(v) = assign_at(i) {
                assign_idx = Some(i);
                var_name = Some(v);
                break;
            }
        }
    }
    let (assign_idx, var_name) = (assign_idx?, var_name?);

    let var_patterns = local_receiver_type_patterns(Language::Php, &regex_escape(&var_name));
    static FUNCTION_RE: OnceLock<Regex> = OnceLock::new();
    let function_re =
        FUNCTION_RE.get_or_init(|| Regex::new(r"\bfunction\b").expect("php function re"));
    for i in (0..=assign_idx).rev() {
        let line = lines[i];
        if line.len() <= 10_000 {
            for (re, _) in &var_patterns {
                if let Some(caps) = re.captures(line) {
                    if let Some(m) = caps.get(1) {
                        if let Some(ty) = normalize_inferred_type_name(m.as_str()) {
                            return Some(ty);
                        }
                    }
                }
            }
        }
        if function_re.is_match(line) {
            break;
        }
    }
    None
}

/// Infer a PHP `$this->prop` receiver's DECLARED type from PROPERTY-shaped
/// declarations only (the PHP `$this->prop` branch of `inferLocalReceiverType`,
/// `name-matcher.ts:1283-1359`). `receiver_with_prefix` is the raw `this->prop`
/// receiver; the `this->` prefix is stripped, and the scan widens to the WHOLE
/// FILE (the property/ctor declaration lives outside the calling method), unlike
/// the scope-bounded `infer_local_receiver_type`. Only `php_property_type_patterns`
/// are consulted (a same-named local/param can never mistype the property); when
/// no property-shaped declaration matches, the classic-ctor assignment
/// second-chance (`infer_php_assigned_property_type`) runs. Returns `None` when
/// the type can't be recovered → the caller produces NO edge (never guessed).
fn infer_php_property_receiver_type(
    receiver_with_prefix: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let prop = receiver_with_prefix.strip_prefix("this->")?;
    let escaped = regex_escape(prop);
    // Compiled only once a line could match one: `… ?Foo $prop` writes the
    // property after whitespace, `$this->prop = new Foo` its assignment.
    let compiled: OnceCell<Arc<Vec<(Regex, PatternCheck)>>> = OnceCell::new();
    let property = format!("this->{prop}");
    let may_declare = |line: &str| {
        writes_typed_variable(line, prop) || writes_assignment_of(line, &property, "new")
    };
    let source = context.read_file(&reference.file_path)?;
    let lines: Vec<&str> = source
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .collect();
    if lines.is_empty() {
        return None;
    }
    let call_idx = ((reference.line - 1).max(0) as usize).min(lines.len() - 1);

    for line in &lines {
        // A generated/minified line is not where a human declaration lives, and
        // regexing multi-KB text per ref is pure waste — skip it.
        if line.len() > 10_000 || !may_declare(line) {
            continue;
        }
        let patterns = compiled.get_or_init(|| {
            context.compiled_patterns(&format!("php-property:{escaped}"), &|| {
                php_property_type_patterns(&escaped)
                    .into_iter()
                    .map(|re| (re, PatternCheck::None))
                    .collect()
            })
        });
        for (re, _) in patterns.iter() {
            if let Some(caps) = re.captures(line) {
                if let Some(m) = caps.get(1) {
                    if let Some(ty) = normalize_inferred_type_name(m.as_str()) {
                        return Some(ty);
                    }
                }
            }
        }
    }

    // Classic pre-7.4 untyped-property-assigned-in-constructor second chance.
    infer_php_assigned_property_type(prop, &escaped, &lines, call_idx, context)
}

/// Try to resolve by method name on a class/object (`matchMethodCall`,
/// `name-matcher.ts:930-1133`).
///
/// Local-variable / typed-parameter receiver-type inference (#1108/#1125) runs
/// first: C++ keeps its dedicated inferrer (header scan + `auto`), every other
/// language uses the shared source-based `infer_local_receiver_type`. Java/Kotlin
/// then also try field-receiver inference (name-matcher.ts:975-996). All validate
/// the method via `resolve_method_on_type` so a wrong inference yields no edge.
/// Strategies 1-3 (direct class match, capitalized receiver, receiver-overlap
/// scoring) then cover the remaining instance-method edges, each preferring the
/// call site's own file among same-named candidates (#1079).
pub fn match_method_call(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    // PHP property receiver: `$this->prop->method()` reaches the resolver as
    // `this->prop.method` (extractor records the receiver's raw text, leading `$`
    // stripped). Resolve it EXCLUSIVELY through declared-type inference +
    // resolve_method_on_type — the name-similarity strategies below must NEVER see
    // this shape, so a property whose type can't be recovered stays unlinked
    // rather than guessed. Returns unconditionally (`Some` or `None`), mirroring
    // the upstream `phpThisPropMatch` early return (name-matcher.ts:1475-1500).
    if reference.language == Language::Php {
        if let Some(caps) = php_this_prop_call_re().captures(&reference.reference_name) {
            let receiver = caps.get(1)?.as_str();
            let php_method = caps.get(2)?.as_str();
            let inferred = infer_php_property_receiver_type(receiver, reference, context)?;
            return resolve_method_on_type(
                &inferred,
                php_method,
                reference,
                context,
                0.9,
                ResolvedBy::InstanceMethod,
                imported_fqn_of(&inferred, reference, context).as_deref(),
                0,
            );
        }
    }

    // A TS/JS call through an ES private field of the enclosing class —
    // `this.#items.add()`, emitted as `this.#items.add` (#1987) — resolves
    // exactly like `this.<field>` below (#1496). `#` is outside the dotted
    // receiver grammar, so the shape is matched here.
    if is_js_ts(reference.language)
        && let Some((field, method)) = split_private_this_field_call(&reference.reference_name)
    {
        return match_ts_this_field_call(field, method, reference, context);
    }

    let parsed = parse_method_call(&reference.reference_name, reference.language)?;
    let (object_or_class, method_name) = parsed;

    // A Go call through a two-hop field chain, `s.db.CreateItem()`, recorded
    // by its last receiver segment (`db.CreateItem`, KEEP-RUST #1496): it
    // resolves through the field's declared type, or not at all (upstream
    // #1276, #2361).
    if let Some((base, field)) =
        crate::go_packages::go_call_chain(reference, &object_or_class, &method_name, context)
    {
        return crate::go_packages::go_field_chain_call(
            &base,
            &field,
            &method_name,
            reference,
            context,
            0.85,
            ResolvedBy::InstanceMethod,
        );
    }

    // Rust call through one field of the enclosing type — emitted as
    // `self.<field>.<method>`. This branch is exclusive: a missing, external,
    // generic, container, or ambiguous field type must stay unresolved rather
    // than reaching the receiver-name heuristics below.
    if reference.language == Language::Rust {
        if let Some(field) = object_or_class.strip_prefix("self.") {
            return match_rust_self_field_call(field, &method_name, reference, context);
        }
        if object_or_class == "self" {
            return match_rust_self_call(&method_name, reference, context);
        }
    }

    if is_js_ts(reference.language) {
        if let Some(field) = object_or_class.strip_prefix("this.") {
            return match_ts_this_field_call(field, &method_name, reference, context);
        }
    }

    // Lua table functions carry the stable qualified form `Table::method`,
    // while calls may use either `Table.method()` or `Table:method()`. Resolve
    // that exact table member before receiver-type heuristics; an ambiguous
    // exact qualified name stays unresolved instead of falling through to a
    // same-named global guess.
    if matches!(reference.language, Language::Lua | Language::Luau) {
        let want = format!("{object_or_class}::{method_name}");
        let exact = context
            .get_nodes_by_name_shared(&method_name)
            .into_iter()
            .filter(|candidate| {
                candidate.language == reference.language
                    && matches!(candidate.kind, NodeKind::Function | NodeKind::Method)
                    && candidate.qualified_name == want
            })
            .collect::<Vec<_>>();
        if !exact.is_empty() {
            let same_file = exact
                .iter()
                .filter(|candidate| candidate.file_path == reference.file_path)
                .collect::<Vec<_>>();
            let target = if same_file.len() == 1 {
                Some(same_file[0])
            } else if same_file.is_empty() && exact.len() == 1 {
                exact.first()
            } else {
                None
            };
            return target.map(|target| ResolvedRef {
                original: reference.clone(),
                target_node_id: target.id.clone(),
                confidence: 0.95,
                resolved_by: ResolvedBy::InstanceMethod,
            });
        }
    }

    // A Dart call through a type's name is to that type's own member, or to
    // nothing in the project (upstream #2380): the static constant a call
    // through the type invokes (#2367) among them.
    if reference.language == Language::Dart
        && is_dot_call(&reference.reference_name)
        && let Some(own) = crate::dart_members::match_dart_type_member_call(
            &object_or_class,
            &method_name,
            reference,
            context,
        )
    {
        return own;
    }

    // Receiver-type inference (#1108/#1125): only for a simple `obj.method` /
    // `obj:method` (Lua) / `obj$method` (R) shape — a `Class::method` colon-call
    // is not an instance call. C++ uses its dedicated inferrer; every other
    // language uses the shared source-based inferrer.
    if is_inferable_receiver_call(&reference.reference_name, reference.language) {
        let mut cpp_declaration = CppReceiverDeclaration::default();
        // A CFML type as its declaration writes it: the component path tells
        // same-named components apart (W4-CF-02); a Go one, the package
        // declaring it (W4-GO-01).
        let mut cfml_written: Option<String> = None;
        let mut go_written: Option<String> = None;
        let mut inferred = if reference.language == Language::Cpp {
            infer_cpp_receiver_type(
                &object_or_class,
                reference,
                context,
                0,
                Some(&mut cpp_declaration),
            )
        } else {
            // The field lookup runs ONLY where the function-scope scan declined,
            // so it can add an inference but never change one (#1566, ESM only).
            infer_local_receiver_declaration(&object_or_class, reference, context)
                .map(|(name, written)| {
                    if reference.language == Language::Cfml {
                        cfml_written = Some(written);
                    } else if reference.language == Language::Go {
                        go_written = Some(written);
                    }
                    name
                })
                .or_else(|| infer_class_field_receiver_type(&object_or_class, reference, context))
                // A pytest test's parameter is what its fixture returns:
                // flaskbb's `cli_runner.invoke(…)` is click's `CliRunner`, not
                // the project's one `invoke` (upstream #2228).
                .or_else(|| {
                    (reference.language == Language::Python)
                        .then(|| {
                            crate::python_values::python_fixture_return_type(
                                &object_or_class,
                                reference,
                                context,
                            )
                        })
                        .flatten()
                })
        };
        // A Go parameter named like an import is declared through a package
        // as often as not (`logger logger.Interface`), which the patterns
        // above don't read; the scope reader kept its type. Not a field's: in
        // `metadata.Type.String()` that is the type of `metadata` (upstream
        // #2448).
        if inferred.is_none()
            && reference.language == Language::Go
            && is_dot_call(&reference.reference_name)
            && !object_or_class.contains('.')
            && let Some(declared) = crate::go_packages::go_ref_declared_type(reference, context)
        {
            inferred = normalize_inferred_type_name(&declared);
            go_written = Some(declared);
        }
        // A field or property of the class around the call, or of one it
        // inherits, is its declared type (upstream #2171): Newtonsoft's
        // `_innerWriter.WriteValue(…)` is JsonWriter's.
        if inferred.is_none()
            && crate::member_types::is_member_typed_language(reference.language)
            && is_dot_call(&reference.reference_name)
        {
            inferred = crate::member_types::infer_member_receiver_type(
                &object_or_class,
                reference,
                context,
            );
            // A member of a built-in type (`string _name`, `int count`) has
            // no project method; a C# one keeps the project's extension
            // method for it (KEEP-RUST, W3-05c), unless the name is one the
            // keyword type declares itself.
            if let Some(keyword) = inferred
                .as_deref()
                .filter(|t| t.starts_with(|c: char| c.is_ascii_lowercase()))
            {
                if reference.language != Language::CSharp
                    || is_std_method_name(Language::CSharp, &method_name)
                {
                    return None;
                }
                return crate::csharp_extensions::extension_method_target(
                    keyword,
                    &method_name,
                    reference,
                    context,
                );
            }
        }
        // A C# local or parameter of a keyword type (`this string format`) is
        // an outside type as well: only the project's extension method for it
        // (KEEP-RUST, W3-05c; the scorer below no longer takes the method's
        // own words, upstream #2171).
        if inferred.is_none()
            && reference.language == Language::CSharp
            && is_dot_call(&reference.reference_name)
            && let Some(keyword) = csharp_keyword_local_type(&object_or_class, reference, context)
        {
            // `s.StartsWith(".")` is `string.StartsWith`, never a project
            // extension of that name (upstream #2171's standard names).
            if is_std_method_name(Language::CSharp, &method_name) {
                return None;
            }
            return crate::csharp_extensions::extension_method_target(
                &keyword,
                &method_name,
                reference,
                context,
            );
        }
        // A type parameter is its bound; with none, only `Object`'s methods.
        if let Some(type_name) = inferred.clone()
            && crate::member_types::is_member_typed_language(reference.language)
            && !context
                .get_nodes_by_name_shared(&type_name)
                .iter()
                .any(|n| crate::kotlin_scope::is_method_owner_kind(n))
        {
            match crate::member_types::type_parameter_bound(&type_name, reference, context) {
                crate::member_types::TypeParameterBound::Unbounded => return None,
                crate::member_types::TypeParameterBound::Bound(bound) => inferred = Some(bound),
                crate::member_types::TypeParameterBound::NotParameter => {}
            }
        }
        if let Some(inferred) = inferred {
            // #1566 — an inferred type the call site cannot SEE is not this
            // project's type: a built-in `Map` must never resolve onto a
            // same-named project class. Refuse before typed resolution runs, and
            // before Strategies 1/2/3 below, because the hijack arrives via a
            // typed-resolution SUCCESS as readily as via its failure.
            if is_esm_language(reference.language)
                && !is_receiver_type_bound_at_call_site(&inferred, reference, context)
            {
                return None;
            }
            // Java/Kotlin: the file's import pins WHICH same-named class (#314);
            // Go: the package declaring the type (#2323); other languages
            // disambiguate by call-site file in resolve_method_on_type.
            let imported_fqn = if matches!(reference.language, Language::Java | Language::Kotlin) {
                imported_fqn_of(&inferred, reference, context)
            } else if reference.language == Language::Go {
                crate::go_packages::go_declared_type_package(
                    go_written.as_deref(),
                    &reference.file_path,
                    context,
                )
            } else {
                None
            };
            let typed = if reference.language == Language::Cfml {
                match crate::cfml_scope::typed_receiver_call(
                    cfml_written.as_deref().unwrap_or(&inferred),
                    &method_name,
                    reference,
                    context,
                ) {
                    crate::cfml_scope::TypedReceiverCall::Method(method) => Some(ResolvedRef {
                        original: reference.clone(),
                        target_node_id: method.id.clone(),
                        confidence: 0.9,
                        resolved_by: ResolvedBy::InstanceMethod,
                    }),
                    // The receiver is one of the namesakes and nothing says
                    // which: a method picked by name below is a guess.
                    crate::cfml_scope::TypedReceiverCall::Ambiguous => return None,
                    crate::cfml_scope::TypedReceiverCall::Missing => None,
                }
            } else {
                resolve_method_on_type(
                    &inferred,
                    &method_name,
                    reference,
                    context,
                    0.9,
                    ResolvedBy::InstanceMethod,
                    imported_fqn.as_deref(),
                    0,
                )
            };
            if let Some(typed) = typed {
                // `medium and 0xff` on an Int: the standard library's
                // member, not a project `Int.and(Long)` (upstream #2174).
                if reference.language == Language::Kotlin
                    && context
                        .get_node_by_id_shared(&typed.target_node_id)
                        .is_some_and(|target| {
                            crate::kotlin_scope::is_kotlin_number_bitwise(&target, reference)
                        })
                {
                    return None;
                }
                return Some(typed);
            }
            // The receiver's declared type is one the project doesn't declare
            // (`List<String> list`, `String s`, `path = Path("x")`), so the
            // method is that outside type's, never a project method guessed
            // by name (upstream #2169). Only a type name: the last segment of
            // `java.util.List`, not of a call chain. A C++ declared type is
            // read the way upstream's `normalizeCppTypeName` reads it, which
            // the C++ inferrer here does not: `SkipList<Key, Comparator>` is
            // the project's `SkipList`.
            let type_name = if reference.language == Language::Cpp {
                cpp_declared_type_name(&inferred)
            } else {
                Some(inferred.rsplit('.').next().unwrap_or(&inferred).to_string())
            };
            if let Some(type_name) = type_name
                && is_undeclared_type_name(&type_name, reference, context)
            {
                // KEEP-RUST (owner 2026-10-11, W3-05c): a C# call on an
                // outside type still reaches the one project extension method
                // in scope that takes it as `this` (`objectType.IsEnum()` on a
                // `System.Type`); anything else stops here.
                return crate::csharp_extensions::extension_method_target(
                    &type_name,
                    &method_name,
                    reference,
                    context,
                );
            }
        } else {
            match crate::awaited::infer_awaited_receiver(&object_or_class, reference, context) {
                crate::awaited::AwaitedInference::NotApplicable => {}
                crate::awaited::AwaitedInference::Unresolved => return None,
                crate::awaited::AwaitedInference::Inferred {
                    type_name,
                    file_path,
                } => {
                    // Resolve on the awaited type from its own file, and accept
                    // only a method that file's type declares (upstream v1.6.1).
                    let mut at_type = reference.clone();
                    at_type.file_path = file_path.clone();
                    let typed = resolve_method_on_type(
                        &type_name,
                        &method_name,
                        &at_type,
                        context,
                        0.9,
                        ResolvedBy::InstanceMethod,
                        None,
                        0,
                    )?;
                    let target = context.get_node_by_id_shared(&typed.target_node_id)?;
                    if target.qualified_name.starts_with(&format!("{type_name}::"))
                        && target.file_path != file_path
                    {
                        return None;
                    }
                    return Some(ResolvedRef {
                        original: reference.clone(),
                        ..typed
                    });
                }
            }
        }
        if cpp_declaration.aliased {
            // A C++ receiver the calling function or its class declares
            // through an alias has the type the alias names. When that type
            // and its supertypes lack the method (`files_.clear()` through
            // `using Files = std::vector<int>;`), or the alias names a template
            // parameter's type, a method picked by the receiver's name below
            // would be some other type's (upstream #2399).
            if !cpp_declaration.class_template
                && (cpp_declaration.pointer
                    || is_cpp_dot_call(&object_or_class, reference, context))
            {
                return None;
            }
            // Not so for `it->m()` on an iterator or smart pointer, whose `->`
            // reaches an element type the alias doesn't name, nor for one of
            // the project's class templates, whose specializations may declare
            // members the template itself doesn't (rocksdb's
            // `omt_node_templated<T, true>` adds `get_marked`): that call goes
            // on as it did before aliases were followed.
            if is_undeclared_type_name(&cpp_declaration.written, reference, context) {
                return None;
            }
        }
        // A C++ receiver the calling function or its class declares as a
        // type from outside the project with a lowercase name calls that
        // type's own member, through `.` or a raw pointer's `->` (upstream
        // #2413): leveldb's `std::string data_; data_.data()` went to
        // `Slice::data`, protobuf's `std::string proto; proto.append(…)` to
        // `LeftoverBuffer::append`. Not so for `->` on such a value (an
        // iterator, smart pointer or optional hands the call to its element
        // type), nor for a call that doesn't fit the declaration (`.` on a
        // pointer), which then is another variable's.
        if reference.language == Language::Cpp
            && cpp_declaration.callers
            && !cpp_declaration.aliased
            && !cpp_declaration.shadowed
            && cpp_member_operator(&object_or_class, reference, context)
                == Some(if cpp_declaration.pointer { "->" } else { "." })
            && is_cpp_library_type(&cpp_declaration.raw, context)
        {
            return None;
        }
    }

    // Java/Kotlin field-receiver inference (name-matcher.ts:975-996): the
    // receiver may be a field whose name doesn't match its type by convention
    // (Spring `@Resource`/`@Autowired`). Resolve the method on the field's type.
    if matches!(reference.language, Language::Java | Language::Kotlin)
        && is_dot_call(&reference.reference_name)
    {
        if let Some(inferred) = infer_java_field_receiver_type(&object_or_class, reference, context)
        {
            let imported_fqn = imported_fqn_of(&inferred, reference, context);
            if let Some(typed) = resolve_method_on_type(
                &inferred,
                &method_name,
                reference,
                context,
                0.9,
                ResolvedBy::InstanceMethod,
                imported_fqn.as_deref(),
                0,
            ) {
                return Some(typed);
            }
        }
    }

    // Object-literal namespace receiver (#1573): `api.call()` where `api` is a
    // `const api = { call() {…}, get: () => {…} }`. Its members are functions
    // inside the constant's extent, found by containment before the
    // class-shaped strategies. The holder is the one the call can see: in its
    // own file, the nearest enclosing declaration (an IIFE's `const App`
    // before the file's); otherwise a global, `window.App = {…}` or a classic
    // script's top-level `App`, when the file neither imports nor binds the
    // name (upstream #2363). An imported holder reaches the same member
    // through the import path.
    if is_object_literal_language(reference.language)
        && !object_or_class.contains('.')
        && let Some(Some(member)) = crate::object_owners::resolve_object_path_member(
            &object_or_class,
            &method_name,
            reference,
            context,
            None,
        )
    {
        return Some(member);
    }

    // Strategy 1: direct class name match (name-matcher.ts:825-850). When the
    // receiver names a class in several files, try the call site's own file
    // first (#1079), and a C# class the call's namespaces can see before one
    // they can't: serilog's `Some.InformationEvent()` in Serilog.Tests is its
    // own Support namespace's `Some` (upstream #2220).
    let mut class_candidates = prefer_call_site_file(
        context.get_nodes_by_name_shared(&object_or_class),
        &reference.file_path,
    );
    if reference.language == Language::CSharp && class_candidates.len() > 1 {
        let type_ref = RefView {
            reference_name: object_or_class.clone(),
            ..reference.clone()
        };
        let (visible, hidden): (Vec<Arc<Node>>, Vec<Arc<Node>>) =
            class_candidates.into_iter().partition(|c| {
                c.language != Language::CSharp
                    || crate::csharp_types::is_csharp_type_visible(c, &type_ref, context)
            });
        class_candidates = visible.into_iter().chain(hidden).collect();
    }
    for class_node in class_candidates {
        // A Scala `object` owns methods callable as `Obj.method()` (#1824).
        if matches!(
            class_node.kind,
            NodeKind::Class | NodeKind::Struct | NodeKind::Union | NodeKind::Interface
        ) || crate::types::is_scala_singleton(&class_node)
        {
            if class_node.language != reference.language {
                continue;
            }
            if let Some(method_node) = find_method_in_class(&class_node, &method_name, context) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: method_node.id,
                    confidence: 0.85,
                    resolved_by: ResolvedBy::QualifiedName,
                });
            }
        }
    }
    // A class method the named class inherits (Horse's `THorse.Get(…)` is
    // THorseCore's, three `class(…)` heads up) before any guess by name
    // (upstream #2180).
    let named_classes: Vec<Arc<Node>> = context
        .get_nodes_by_name_shared(&object_or_class)
        .into_iter()
        .filter(|class_node| {
            class_node.language == reference.language
                && (matches!(
                    class_node.kind,
                    NodeKind::Class | NodeKind::Struct | NodeKind::Union | NodeKind::Interface
                ) || crate::types::is_scala_singleton(class_node))
        })
        .collect();
    if let Some(inherited) = inherited_class_method(&named_classes, &method_name, context) {
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: inherited.id.clone(),
            confidence: 0.8,
            resolved_by: ResolvedBy::QualifiedName,
        });
    }

    // Built-in method names need a validated receiver (#1987). Typed, imported,
    // object-literal and direct class receivers have had their chance above;
    // capitalization, word overlap or a unique method name are not evidence
    // that `list.map()` / `cache.get()` calls a project class.
    if reference.reference_kind == EdgeKind::Calls
        && is_js_family(reference.language)
        && !matches!(object_or_class.as_str(), "this" | "super")
        && is_js_builtin_method(&method_name)
    {
        return None;
    }

    // A receiver the file imports is past guessing. A namespace import is the
    // module object, whose members are its exports, never some class's method;
    // a binding from a package outside the repository names nothing in it. The
    // import resolver placed what it could, and a method picked by name alone
    // is wrong (upstream #2120: zod's `z.string()` bound a test helper's
    // `string` getter).
    if reference.reference_kind == EdgeKind::Calls
        && is_js_family(reference.language)
        && is_imported_module_receiver(&object_or_class, reference, context)
    {
        return None;
    }

    // A receiver written as a type name the project doesn't declare
    // (`Exception.Create(…)` in Delphi, `Collections.sort(…)`,
    // `Object.assign(…)`) is a type from outside it: a same-named method of
    // some project type is a guess (upstream #2166).
    // A C# using alias names its type: `using Assert =
    // Newtonsoft.Json.Tests.XUnitAssert;` (upstream #2171).
    if reference.language == Language::CSharp
        && let Some(aliased) =
            crate::csharp_types::csharp_using_alias(&object_or_class, reference, context)
    {
        return resolve_method_on_type(
            &aliased,
            &method_name,
            reference,
            context,
            0.9,
            ResolvedBy::InstanceMethod,
            None,
            0,
        );
    }
    // In C#, Java and Rust a capitalized receiver is a type: a project
    // property of that name elsewhere (a test object's `DateTime`) does not
    // make `DateTime.Parse(…)` the project's (upstream #2171, #2177).
    let types_only = matches!(
        reference.language,
        Language::CSharp | Language::Java | Language::Rust
    );
    if names_external_type(&object_or_class, reference.language)
        && !context
            .get_nodes_by_name_shared(&object_or_class)
            .iter()
            .any(|n| {
                same_language_family(n.language, reference.language)
                    && (!types_only
                        || crate::kotlin_scope::is_method_owner_kind(n)
                        || matches!(
                            n.kind,
                            NodeKind::Enum
                                | NodeKind::Namespace
                                | NodeKind::Module
                                | NodeKind::Trait
                                | NodeKind::TypeAlias
                        ))
            })
    {
        return None;
    }
    // Rust `task::spawn(…)` / `io::stdout()`: a function through a module
    // path — never a method some type of that name owns (upstream #2177).
    if reference.language == Language::Rust
        && match_dot_call(&reference.reference_name).is_none()
        && match_colon_call(&reference.reference_name).is_some()
        && is_rust_module_path(&object_or_class)
    {
        return None;
    }
    // `string.Equals(…)`, `object.ReferenceEquals(…)`: a C# keyword type.
    if reference.language == Language::CSharp
        && matches!(
            object_or_class.as_str(),
            "string"
                | "object"
                | "int"
                | "long"
                | "short"
                | "byte"
                | "bool"
                | "char"
                | "double"
                | "float"
                | "decimal"
                | "uint"
                | "ulong"
                | "ushort"
                | "sbyte"
        )
    {
        return None;
    }

    // Strategy 2: instance-variable receiver → capitalized class
    // (name-matcher.ts:852-880), call-site file first (#1079).
    let capitalized = capitalize(&object_or_class);
    if capitalized != object_or_class {
        for class_node in prefer_call_site_file(
            context.get_nodes_by_name_shared(&capitalized),
            &reference.file_path,
        ) {
            if matches!(
                class_node.kind,
                NodeKind::Class | NodeKind::Struct | NodeKind::Union | NodeKind::Interface
            ) || crate::types::is_scala_singleton(&class_node)
            {
                if class_node.language != reference.language {
                    continue;
                }
                if let Some(method_node) = find_method_in_class(&class_node, &method_name, context)
                {
                    return Some(ResolvedRef {
                        original: reference.clone(),
                        target_node_id: method_node.id,
                        confidence: 0.8,
                        resolved_by: ResolvedBy::InstanceMethod,
                    });
                }
            }
        }
    }

    // Strategy 3: methods-by-name, receiver-overlap scoring
    // (name-matcher.ts:882-933).
    let method_candidates = context.get_nodes_by_name_shared(&method_name);
    // Ubiquitous-method ceiling (#999): bail before the O(K) scoring work when a
    // method name is re-declared across a vendored theme/SDK.
    if method_candidates.len() > ambiguous_name_ceiling() {
        return None;
    }
    let methods: Vec<Arc<Node>> = method_candidates
        .into_iter()
        .filter(|n| n.kind == NodeKind::Method && n.name == method_name)
        .collect();
    let same_language: Vec<Arc<Node>> = methods
        .iter()
        .filter(|m| m.language == reference.language)
        .cloned()
        .collect();
    let mut target_methods = if !same_language.is_empty() {
        same_language
    } else {
        methods
    };
    // A receiver the file imports is another module's value: never a method
    // declared in the calling file (upstream #2134: expo-camera's
    // `CameraManager.isAvailableAsync()` reached `CameraView`'s own static
    // method, the one making the call). Ruling the caller's file out may
    // reject a guess; it must never manufacture one, so the one method left
    // is then no likelier than before.
    let mut narrowed = false;
    if is_js_family(reference.language) && is_import_binding(&object_or_class, reference, context) {
        let before = target_methods.len();
        target_methods.retain(|method| method.file_path != reference.file_path);
        narrowed = target_methods.len() != before;
    }
    // A handler the web framework dispatches to (a view's `post`, a
    // controller's `update`) is never called through an instance by name, nor
    // is a project's bitwise extension on a number type (`x and 0xff` is the
    // standard library's), and ruling those out must not leave a lone other
    // one to guess at (upstream #2175, #2174).
    {
        let before = target_methods.len();
        target_methods.retain(|method| {
            !is_dispatched_handler(method)
                && !crate::kotlin_scope::is_kotlin_number_bitwise(method, reference)
        });
        narrowed |= target_methods.len() != before;
    }
    // A Vue component's own method is reached as `this.m()` inside it, never
    // as `e.preventDefault()` on an event nor `this.editor.setValue()` on
    // something the component holds (upstream #2140). A template ref
    // (`this.$refs.form.validate()`) names a child this cannot tell apart.
    // Production code never calls into a test suite (upstream #2171): its
    // methods were never in the running, so ruling them out may leave one.
    if !codegraph_core::file_class::is_test_path(&reference.file_path) {
        target_methods
            .retain(|method| !codegraph_core::file_class::is_test_path(&method.file_path));
    }
    if is_js_family(reference.language) {
        let before = target_methods.len();
        target_methods.retain(|method| {
            !is_vue_component_method(method)
                || (object_or_class == "this" && method.file_path == reference.file_path)
        });
        narrowed |= target_methods.len() != before;
    }
    // What the receiver is named after is its last link (a constant's type):
    // `builder.tokeniser` is a Tokeniser, `table.Columns` no kind of table. A
    // standard-library method name on it (`reader.Value.ToString()`,
    // `sym.map(…)`) is the library type's unless the receiver names the
    // owner (upstream #2171).
    let link = receiver_link(&object_or_class);
    let std = is_std_method_name(reference.language, &method_name)
        && !matches!(object_or_class.as_str(), "self" | "Self" | "this" | "base");

    // A SINGLE same-named candidate is not on its own evidence that the receiver
    // is that candidate's type. Without the overlap check this branch bound every
    // built-in call — `Array.push`, `Vec::len`, `HashMap::get`, tree-sitter
    // `Node::kind` — onto whichever lone project method happened to share the
    // name, which is what manufactured a false reverse `Calls` edge (and with it
    // a phantom import cycle) for a project whose class had a `push` method.
    // The threshold is not a new policy: the multi-candidate block below scores
    // `overlap + 1` for a same-language candidate and gates at `>= 2`, i.e.
    // `overlap >= 1`. Same-language is already guaranteed here, so requiring one
    // shared word makes the two branches say the same thing. Do NOT remove it as
    // redundant, and do NOT drop the language guard — the block below only
    // *bonuses* same language, it does not require it, and `Calls` edges get no
    // cross-family protection from `gate_language`.
    // Where nothing types a receiver, the receiver-word rule (#226) also
    // takes what upstream's owner-word rule does: the receiver's last link
    // spelling the owner's name (`cbsecurity` -> CBSecurity), or a word of
    // it with a trailing number dropped (`executor1` -> Executor; upstream
    // #2148).
    let untyped = crate::ruby_scope::is_untyped_receiver_language(reference.language);
    // An Objective-C receiver also names the owner when it names a class, or
    // ends in a property declared with a type, whose hierarchy has it
    // (upstream #2150): `[managed.anyDataObj containsObject:]` is RLMSet's.
    let objc_reaches = OnceCell::new();
    // A PHP one, when it names a class whose ancestry has it (upstream
    // #2152): `$page->save()` is Entity's, Page extending it.
    let php_reaches = OnceCell::new();
    let names_owner = |method: &Node| {
        crate::kotlin_scope::shares_receiver_word(&object_or_class, method)
            || (reference.language == Language::ObjC
                && *objc_reaches.get_or_init(|| {
                    crate::objc_scope::objc_receiver_reaches(&object_or_class, method, context)
                }))
            || (reference.language == Language::Php
                && *php_reaches.get_or_init(|| {
                    crate::php_scope::php_receiver_reaches(&object_or_class, method, context)
                }))
    };
    if target_methods.len() == 1
        && !narrowed
        && target_methods[0].language == reference.language
        && (receiver_shares_word(&object_or_class, &target_methods[0].qualified_name)
            || (untyped && names_owner(&target_methods[0])))
        // A test double is only what a test names (upstream #2197).
        && !is_unnamed_test_double(&target_methods[0], &object_or_class, reference, context)
        && !(std && !receiver_names_owner(link, &target_methods[0], context))
        // Where nothing types a receiver, the one method of that name must
        // also belong to something the receiver is named after
        // (`web_push_request.legacy_encrypt` -> WebPushRequest; rubocop's
        // `node.loc` went to the project's one `loc` 1,201 times; upstream
        // #2147).
        && !(untyped
            && !crate::ruby_scope::is_self_receiver(&object_or_class)
            && !names_owner(&target_methods[0]))
    {
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: target_methods[0].id.clone(),
            confidence: 0.7,
            resolved_by: ResolvedBy::InstanceMethod,
        });
    }

    if target_methods.len() > 1 {
        // Same-file candidates first, so a score tie (`score > best_score` keeps
        // the first seen) resolves to the call site's own file (#1079).
        let ordered = prefer_call_site_file(target_methods, &reference.file_path);
        let receiver_words = split_camel_case(link);
        let head = receiver_words.last().map(|w| w.to_ascii_lowercase());
        let mut best_match: Option<&Node> = None;
        let mut best_score = 0i64;
        for method in &ordered {
            if std && !receiver_names_owner(link, method, context) {
                continue;
            }
            // A test double is only what a test constructs or names, never a
            // guess from `response.json()` in a file that never mentions it
            // (upstream #2175).
            if is_unnamed_test_double(method, &object_or_class, reference, context) {
                continue;
            }
            // The owner type's own name, not its namespace (`eShop.ClientApp…`
            // shares `Client` with every `httpClient`) nor the method's
            // (upstream #2171).
            let class_words = if method.qualified_name.contains("::") {
                split_camel_case(member_owner_name(&method.qualified_name))
            } else {
                Vec::new()
            };
            let mut score = receiver_words
                .iter()
                .filter(|w| class_words.iter().any(|cw| cw.eq_ignore_ascii_case(w)))
                .count() as i64;
            // The receiver's head noun naming the owner's: `bookPage` is a
            // Page before it is anything of a Book's.
            if head.as_ref().is_some_and(|head| {
                class_words
                    .last()
                    .is_some_and(|cw| cw.eq_ignore_ascii_case(head))
            }) {
                score += 1;
            }
            if method.language == reference.language {
                score += 1;
            }
            if score > best_score {
                best_score = score;
                best_match = Some(method);
            }
        }
        // A wrapper handing its call on (`FileStorage::delete` doing
        // `$storage->delete($path)`) names the caller's own class only by a
        // shared word: a guess that is the caller itself is no guess
        // (upstream #2221).
        if let Some(best) = best_match {
            if best_score >= 2 && best.id != reference.from_node_id {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: best.id.clone(),
                    confidence: 0.65,
                    resolved_by: ResolvedBy::InstanceMethod,
                });
            }
        }
    }

    None
}

const RUST_NON_PROJECT_FIELD_TYPES: &[&str] = &[
    "bool", "char", "str", "String", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16",
    "u32", "u64", "u128", "usize", "f32", "f64", "Self", "self",
];

const RUST_NON_DEREF_CONTAINERS: &[&str] = &[
    "Option",
    "Vec",
    "VecDeque",
    "LinkedList",
    "HashMap",
    "BTreeMap",
    "HashSet",
    "BTreeSet",
    "Mutex",
    "RwLock",
    "RefCell",
    "Cell",
    "OnceLock",
    "Result",
];

/// Reduce a Rust field type to the project type reached by method-call
/// auto-deref. Only references and `Box`/`Rc`/`Arc` are unwrapped.
fn rust_field_type_name(raw: &str) -> Option<String> {
    static REF_PREFIX: OnceLock<Regex> = OnceLock::new();
    let ref_prefix = REF_PREFIX.get_or_init(|| {
        Regex::new(r"^&\s*(?:'\w+\s+)?(?:mut\s+)?").expect("rust reference prefix")
    });

    let mut value = raw.trim();
    loop {
        let before = value;
        if let Some(prefix) = ref_prefix.find(value) {
            value = value[prefix.end()..].trim_start();
        }
        if let Some(open) = value.find('<') {
            let wrapper = value[..open].trim().rsplit("::").next();
            if wrapper.is_some_and(|wrapper| matches!(wrapper, "Box" | "Rc" | "Arc")) {
                value = value[open + 1..].trim_start();
            }
        }
        if let Some(rest) = value.strip_prefix("dyn ") {
            value = rest.trim_start();
        } else if let Some(rest) = value.strip_prefix("impl ") {
            value = rest.trim_start();
        }
        if value == before {
            break;
        }
    }

    let end = value
        .char_indices()
        .find_map(|(index, ch)| matches!(ch, '<' | '>' | '+').then_some(index))
        .unwrap_or(value.len());
    let type_path = value[..end].trim();
    if type_path.contains("::") {
        let root = type_path.split("::").find(|part| !part.is_empty())?;
        if !matches!(root, "crate" | "self" | "super") {
            return None;
        }
    }
    let segment = type_path.rsplit("::").find(|part| !part.is_empty())?;
    if !is_word(segment)
        || RUST_NON_PROJECT_FIELD_TYPES.contains(&segment)
        || RUST_NON_DEREF_CONTAINERS.contains(&segment)
        || (segment.len() == 1 && segment.as_bytes()[0].is_ascii_uppercase())
    {
        return None;
    }
    Some(segment.to_string())
}

fn strip_rust_field_line_comments(line: &str) -> String {
    let line = line.split_once("//").map_or(line, |(head, _)| head);
    let mut output = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find("/*") {
        output.push_str(&rest[..start]);
        let after_start = &rest[start + 2..];
        let Some(end) = after_start.find("*/") else {
            return output;
        };
        rest = &after_start[end + 2..];
    }
    output.push_str(rest);
    output
}

/// Resolve `self.method()` on the Rust type that owns the calling method.
///
/// The extractor preserves this shape as `self.<method>` so the resolver can
/// read the owner from the caller's qualified name. This is deliberately an
/// exclusive, exact match: if the caller is not a method, the owner is absent,
/// or more than one method claims the same owner-qualified name, the reference
/// stays unresolved instead of falling through to proximity/name heuristics.
fn match_rust_self_call(
    method_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if method_name.is_empty() || !is_word(method_name) {
        return None;
    }
    let caller = context.get_node_by_id_shared(&reference.from_node_id)?;
    if caller.language != Language::Rust || caller.kind != NodeKind::Method {
        return None;
    }
    let (owner, _) = caller.qualified_name.rsplit_once("::")?;
    if owner.is_empty() {
        return None;
    }
    let wanted = format!("{owner}::{method_name}");
    let mut owned: Vec<Arc<Node>> = context
        .get_nodes_by_name_shared(method_name)
        .into_iter()
        .filter(|candidate| {
            candidate.language == Language::Rust
                && candidate.kind == NodeKind::Method
                && candidate.qualified_name == wanted
        })
        .collect();
    // Rust qualified names omit module paths, so two modules can each declare
    // `Target`; matching `Target::reset` alone then proves nothing. With more
    // than one owner declaration, require exactly one in the caller's file and
    // a method in that file (upstream #1882). A unique owner still permits
    // impl blocks split across files.
    let owners = context
        .get_nodes_by_qualified_name_shared(owner)
        .into_iter()
        .filter(|node| {
            node.language == Language::Rust
                && matches!(
                    node.kind,
                    NodeKind::Struct
                        | NodeKind::Enum
                        | NodeKind::Union
                        | NodeKind::Trait
                        | NodeKind::Class
                )
        })
        .collect::<Vec<_>>();
    if owners.len() > 1 {
        if owners
            .iter()
            .filter(|node| node.file_path == caller.file_path)
            .count()
            != 1
        {
            return None;
        }
        owned.retain(|node| node.file_path == caller.file_path);
    }
    let [target] = owned.as_slice() else {
        return None;
    };
    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: target.id.clone(),
        confidence: 0.9,
        resolved_by: ResolvedBy::QualifiedName,
    })
}

/// Resolve `self.field.method()` through the field type declared on the
/// caller's own Rust struct/union. Any uncertainty is a deliberate miss.
fn match_rust_self_field_call(
    field: &str,
    method_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if field.is_empty() || field.contains('.') || !is_word(field) {
        return None;
    }
    let caller = context.get_node_by_id_shared(&reference.from_node_id)?;
    let (owner_prefix, _) = caller.qualified_name.rsplit_once("::")?;
    let owner_name = owner_prefix.rsplit("::").next()?;
    if owner_name.is_empty() {
        return None;
    }

    let owners = context
        .get_nodes_by_name_shared(owner_name)
        .into_iter()
        .filter(|node| {
            node.language == Language::Rust
                && matches!(
                    node.kind,
                    NodeKind::Struct | NodeKind::Union | NodeKind::Class
                )
        })
        .collect::<Vec<_>>();
    let same_file = owners
        .iter()
        .filter(|node| node.file_path == reference.file_path)
        .collect::<Vec<_>>();
    let owner = if same_file.len() == 1 {
        same_file[0]
    } else if same_file.is_empty() && owners.len() == 1 {
        &owners[0]
    } else {
        return None;
    };

    let source = context.read_file(&owner.file_path)?;
    let lines = source.lines().collect::<Vec<_>>();
    let start = owner.start_line.saturating_sub(1) as usize;
    let end = (owner.end_line.max(owner.start_line) as usize).min(lines.len());
    if start >= end {
        return None;
    }
    let field_re = Regex::new(&format!(r"\b{}\s*:\s*([^,{{}}]+)", regex::escape(field))).ok()?;

    for raw_line in &lines[start..end] {
        let line = strip_rust_field_line_comments(raw_line);
        let Some(captures) = field_re.captures(&line) else {
            continue;
        };
        let raw_type = captures.get(1)?.as_str();
        let field_type = rust_field_type_name(raw_type)?;

        // A same-named type in multiple project locations is not enough
        // evidence to choose one. External types have zero candidates.
        let type_candidates = context
            .get_nodes_by_name_shared(&field_type)
            .into_iter()
            .filter(|node| {
                node.language == Language::Rust
                    && matches!(
                        node.kind,
                        NodeKind::Struct
                            | NodeKind::Union
                            | NodeKind::Enum
                            | NodeKind::Trait
                            | NodeKind::Class
                    )
            })
            .collect::<Vec<_>>();
        if type_candidates.len() != 1 {
            return None;
        }

        return resolve_method_on_type(
            &field_type,
            method_name,
            reference,
            context,
            0.85,
            ResolvedBy::InstanceMethod,
            None,
            0,
        );
    }
    None
}

/// Parse `obj.method`, `Class::method`, Lua/Luau `obj:method`, or R `obj$method`
/// (name-matcher.ts:770-778 + #1112). The `:`/`$` separators are recognized ONLY
/// for their owning language, so a `lg:log` ref elsewhere is not a call shape.
fn parse_method_call(name: &str, language: Language) -> Option<(String, String)> {
    if let Some(captures) = match_dot_call(name) {
        return Some(captures);
    }
    if matches!(language, Language::C | Language::Cpp) {
        if let Some(captures) = match_cpp_operator_dot_call(name) {
            return Some(captures);
        }
    }
    if let Some(captures) = match_colon_call(name) {
        return Some(captures);
    }
    if matches!(language, Language::Lua | Language::Luau) {
        if let Some(captures) = match_lua_colon_call(name) {
            return Some(captures);
        }
    }
    if language == Language::R {
        if let Some(captures) = match_r_dollar_call(name) {
            return Some(captures);
        }
    }
    None
}

/// A simple `receiver.method` / `receiver:method` (Lua) / `receiver$method` (R)
/// shape whose receiver type we can try to infer (`inferableReceiver`,
/// name-matcher.ts:#1108/#1112). A `Class::method` colon-call is excluded — it is
/// not an instance call.
fn is_inferable_receiver_call(name: &str, language: Language) -> bool {
    if match_dot_call(name).is_some() {
        return true;
    }
    if matches!(language, Language::C | Language::Cpp)
        && match_cpp_operator_dot_call(name).is_some()
    {
        return true;
    }
    if matches!(language, Language::Lua | Language::Luau) && match_lua_colon_call(name).is_some() {
        return true;
    }
    if language == Language::R && match_r_dollar_call(name).is_some() {
        return true;
    }
    false
}

/// Did the ref match the dotted `obj.method` shape (the upstream `dotMatch` that
/// gates the Java field-receiver hook, name-matcher.ts:975)?
fn is_dot_call(name: &str) -> bool {
    match_dot_call(name).is_some()
}

/// Matches `/^([\w.]+):(\w+)$/` — a Lua/Luau method call `obj:method` (#1112).
fn match_lua_colon_call(name: &str) -> Option<(String, String)> {
    let idx = name.find(':')?;
    let receiver = &name[..idx];
    let method = &name[idx + 1..];
    if receiver.is_empty() || method.is_empty() {
        return None;
    }
    if !receiver
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
    {
        return None;
    }
    if !is_word(method) {
        return None;
    }
    Some((receiver.to_string(), method.to_string()))
}

/// Matches `/^([\w.]+)\$(\w+)$/` — an R method call `obj$method` (#1112).
fn match_r_dollar_call(name: &str) -> Option<(String, String)> {
    let idx = name.find('$')?;
    let receiver = &name[..idx];
    let method = &name[idx + 1..];
    if receiver.is_empty() || method.is_empty() {
        return None;
    }
    if !receiver
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
    {
        return None;
    }
    if !is_word(method) {
        return None;
    }
    Some((receiver.to_string(), method.to_string()))
}

/// Matches `/^([\w.]+)\.(\w+:?(?:\w+:)*)$/` (name-matcher.ts:770).
fn match_dot_call(name: &str) -> Option<(String, String)> {
    let last_dot = name.rfind('.')?;
    if last_dot == 0 {
        return None;
    }
    let receiver = &name[..last_dot];
    let method = &name[last_dot + 1..];
    if receiver.is_empty()
        || !receiver
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
    {
        return None;
    }
    if !is_selector_like(method) {
        return None;
    }
    Some((receiver.to_string(), method.to_string()))
}

/// Matches `/^([\w.]+)\.(operator[^\w\s.]+)$/` — a C++ explicit operator call
/// (`a.operator+`, `it.operator[]`) reaching the resolver from the extraction
/// recovery (#1268). The operator's symbol chars fail `match_dot_call`'s
/// selector-like method part, so they are admitted here instead. At least one
/// non-word char after `operator` is required, so `a.operatorTable` stays on the
/// plain dotted pattern (tried first); every downstream strategy compares the
/// method part by exact string equality, so a stray match cannot invent an edge.
fn match_cpp_operator_dot_call(name: &str) -> Option<(String, String)> {
    let idx = name.rfind(".operator")?;
    if idx == 0 {
        return None;
    }
    let receiver = &name[..idx];
    let method = &name[idx + 1..];
    if !receiver
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
    {
        return None;
    }
    let symbol = method.strip_prefix("operator")?;
    if symbol.is_empty()
        || !symbol
            .chars()
            .all(|c| !c.is_alphanumeric() && c != '_' && c != '.' && !c.is_whitespace())
    {
        return None;
    }
    Some((receiver.to_string(), method.to_string()))
}

/// Matches `/^(\w+)::(\w+)$/` (name-matcher.ts:771).
fn match_colon_call(name: &str) -> Option<(String, String)> {
    let idx = name.find("::")?;
    let receiver = &name[..idx];
    let method = &name[idx + 2..];
    if receiver.is_empty() || method.is_empty() {
        return None;
    }
    if !is_word(receiver) || !is_word(method) {
        return None;
    }
    Some((receiver.to_string(), method.to_string()))
}

fn is_word(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// `\w+:?(?:\w+:)*` — a word with optional trailing ObjC selector colons.
fn is_selector_like(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars().peekable();
    let mut saw_word = false;
    while let Some(&c) = chars.peek() {
        if c.is_alphanumeric() || c == '_' {
            saw_word = true;
            chars.next();
        } else {
            break;
        }
    }
    if !saw_word {
        return false;
    }
    // Remaining must be groups of `:` then word, ending after a `:`-word or a `:`.
    while let Some(c) = chars.next() {
        if c != ':' {
            return false;
        }
        // optional word after colon
        while let Some(&w) = chars.peek() {
            if w.is_alphanumeric() || w == '_' {
                chars.next();
            } else {
                break;
            }
        }
    }
    true
}

fn find_method_in_class(
    class_node: &Node,
    method_name: &str,
    context: &dyn ResolutionContext,
) -> Option<Node> {
    context
        .get_nodes_in_file_shared(&class_node.file_path)
        .into_iter()
        .find(|n| {
            n.kind == NodeKind::Method
                && n.name == method_name
                && n.qualified_name.contains(&class_node.name)
        })
        .map(|node| node.as_ref().clone())
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// How many [`split_camel_case`] words a receiver shares, case-insensitively,
/// with a candidate's qualified name. The SINGLE implementation of that
/// comparison: Strategy 3's one-candidate branch and its multi-candidate scorer
/// both route through here, so the two can no longer drift apart.
fn receiver_word_overlap(receiver: &str, qualified_name: &str) -> usize {
    let receiver_words = split_camel_case(receiver);
    let class_words = split_camel_case(qualified_name);
    receiver_words
        .iter()
        .filter(|w| class_words.iter().any(|cw| cw.eq_ignore_ascii_case(w)))
        .count()
}

/// The bool face of [`receiver_word_overlap`] — is there ANY shared word?
///
/// Note that [`split_camel_case`] drops single-character words, so a receiver
/// named `r` shares nothing with anything.
fn receiver_shares_word(receiver: &str, qualified_name: &str) -> bool {
    receiver_word_overlap(receiver, qualified_name) >= 1
}

/// Split a camelCase/PascalCase string into words (`splitCamelCase`,
/// `name-matcher.ts:941-946`).
pub(crate) fn split_camel_case(s: &str) -> Vec<String> {
    // Insert spaces at lower→upper and ACRONYM→Word boundaries, then split on
    // separators, then drop single-char words.
    let spaced = insert_camel_spaces(s);
    spaced
        .split(|c: char| c.is_whitespace() || matches!(c, '.' | '_' | ':' | '/' | '\\'))
        .filter(|w| w.chars().count() > 1)
        .map(str::to_string)
        .collect()
}

fn insert_camel_spaces(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 8);
    for i in 0..chars.len() {
        let c = chars[i];
        // /([a-z])([A-Z])/ -> '$1 $2'
        if i + 1 < chars.len() {
            let next = chars[i + 1];
            if c.is_ascii_lowercase() && next.is_ascii_uppercase() {
                out.push(c);
                out.push(' ');
                continue;
            }
        }
        // /([A-Z]+)([A-Z][a-z])/ -> '$1 $2'
        if i + 2 < chars.len()
            && c.is_ascii_uppercase()
            && chars[i + 1].is_ascii_uppercase()
            && chars[i + 2].is_ascii_lowercase()
        {
            out.push(c);
            out.push(' ');
            continue;
        }
        out.push(c);
    }
    out
}

/// Compute directory proximity between two file paths (`computePathProximity`,
/// `name-matcher.ts:953-968`).
pub(crate) fn compute_path_proximity(file_path1: &str, file_path2: &str) -> f64 {
    let dir1: Vec<&str> = drop_last(file_path1);
    let dir2: Vec<&str> = drop_last(file_path2);
    let mut shared = 0usize;
    for i in 0..dir1.len().min(dir2.len()) {
        if dir1[i] == dir2[i] {
            shared += 1;
        } else {
            break;
        }
    }
    ((shared * 15) as f64).min(80.0)
}

fn drop_last(path: &str) -> Vec<&str> {
    let segs: Vec<&str> = path.split('/').collect();
    if segs.is_empty() {
        Vec::new()
    } else {
        segs[..segs.len() - 1].to_vec()
    }
}

/// Find the best matching node among multiple candidates (`findBestMatch`,
/// `name-matcher.ts:973-1055`).
fn find_best_match<'a, T: Borrow<Node>>(
    reference: &RefView,
    candidates: &'a [T],
) -> Option<&'a Node> {
    let mut best_score = -1.0f64;
    let mut best_node: Option<&Node> = None;

    for candidate in candidates {
        let candidate = candidate.borrow();
        let mut score = 0.0f64;

        if candidate.file_path == reference.file_path {
            score += 100.0;
        }
        score += compute_path_proximity(&reference.file_path, &candidate.file_path);
        if candidate.language == reference.language {
            score += 50.0;
        } else {
            score -= 80.0;
        }

        match reference.reference_kind {
            EdgeKind::Calls => {
                if matches!(candidate.kind, NodeKind::Function | NodeKind::Method) {
                    score += 25.0;
                }
            }
            EdgeKind::Instantiates => {
                if matches!(
                    candidate.kind,
                    NodeKind::Class | NodeKind::Struct | NodeKind::Union | NodeKind::Interface
                ) {
                    score += 25.0;
                }
            }
            EdgeKind::Decorates => {
                if matches!(candidate.kind, NodeKind::Function | NodeKind::Method) {
                    score += 25.0;
                } else if matches!(candidate.kind, NodeKind::Class | NodeKind::Interface) {
                    score += 15.0;
                }
            }
            _ => {}
        }

        // A Scala companion `object` is a module and may share both name and
        // file with its trait/class. It remains type-eligible for languages that
        // do inherit/include modules, but an actual type must win an inheritance
        // tie deterministically (#1824).
        if crate::types::is_inheritance_ref(reference.reference_kind)
            && candidate.kind == NodeKind::Module
        {
            score -= 50.0;
        }

        if candidate.is_exported {
            score += 10.0;
        }

        // Closer line number within same file (name-matcher.ts:1043-1046).
        if candidate.file_path == reference.file_path && candidate.start_line != 0 {
            let distance = (candidate.start_line - reference.line).abs() as f64;
            score += (20.0 - distance / 10.0).max(0.0);
        }

        if score > best_score {
            best_score = score;
            best_node = Some(candidate);
        }
    }

    best_node
}

/// Fuzzy match — last resort with lower confidence (`matchFuzzy`,
/// `name-matcher.ts:1060-1088`).
pub fn match_fuzzy(reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
    // A Rust method call whose receiver was dropped takes no fuzzy guess
    // (KEEP-RUST, P15 R2); a bare Rust call still never reaches a method (R1).
    let rust_shape = rust_call_shape(reference, context);
    if rust_shape == Some(RustCallShape::Method) {
        return None;
    }
    let lower_name = reference.reference_name.to_lowercase();
    let candidates = context.get_nodes_by_lower_name_shared(&lower_name);
    if candidates.len() > ambiguous_name_ceiling() {
        return None;
    }

    let callable_kinds = [NodeKind::Function, NodeKind::Method, NodeKind::Class];
    let case_insensitive = is_case_insensitive_language(reference.language);
    let python_shape = crate::python_values::python_call_shape(reference, context);
    let objc_shape = objc_call_shape(reference, context);
    let objc_c_call = objc_shape == Some(ObjcCallShape::CCall);
    let objc_send = objc_send_target(objc_shape);
    let objc_hierarchy = OnceCell::new();
    let java_bare = crate::java_scope::is_java_bare_call(reference);
    let java_call_scope = OnceCell::new();
    let ruby_bare = crate::ruby_scope::is_ruby_bare_call(reference);
    let cfml_bare = crate::cfml_scope::is_cfml_bare_call(reference);
    let kotlin_call = crate::kotlin_scope::is_kotlin_call(reference);
    let kotlin_bare =
        kotlin_call && crate::kotlin_scope::is_receiver_less_kotlin_call(reference, context);
    let kotlin_site = crate::kotlin_scope::KotlinSite::new(reference);
    let kotlin_std_chain = crate::kotlin_scope::std_chain_receiver(reference, context);
    let scala_bare = crate::scala_scope::is_scala_bare_call(reference);
    let csharp_type_ref = crate::csharp_types::is_csharp_type_ref(reference, context);
    // A bare C# name whose line shows no receiver (upstream #2151).
    let csharp_bare = crate::csharp_types::is_csharp_bare_name(reference)
        && has_no_receiver_on_line(reference, context);
    let swift_shape = crate::swift_scope::swift_call_shape(reference, context);
    let go_shape = crate::go_calls::go_call_shape(reference, context);
    let rust_scoped = crate::rust_scope::is_rust_bare_identifier(reference);
    let php_self = crate::php_scope::php_self_scope(reference, context);
    let bare_r = crate::r_scope::is_bare_r_call(reference, context);
    let dart_bare = crate::dart_scope::is_dart_bare_call(reference, context);
    let dart_type_ref = crate::dart_libraries::is_dart_type_ref(reference);
    let dart_prefix = if dart_type_ref {
        crate::dart_libraries::dart_import_prefix_of(reference, context)
    } else {
        None
    };
    let callable_candidates = apply_language_gate(
        candidates
            .into_iter()
            .filter(|n| callable_kinds.contains(&n.kind))
            // A plain R call is no object's method (upstream #2177).
            .filter(|n| !(bare_r && n.kind == NodeKind::Method))
            // A bare Dart call stays in its class hierarchy (upstream #2145).
            .filter(|n| {
                !(dart_bare
                    && crate::dart_scope::is_dart_type_member(n, context)
                    && !crate::dart_scope::is_dart_method_in_scope(n, reference, context))
            })
            // A Dart constructor, only through its class (upstream #2380).
            .filter(|n| {
                !(reference.language == Language::Dart
                    && crate::dart_members::is_dart_constructor(n, context))
            })
            // A receiver-less Dart name, only what its library sees (upstream
            // #2386).
            .filter(|n| {
                !((dart_bare || dart_type_ref)
                    && !crate::dart_libraries::is_dart_top_level_visible(
                        n,
                        reference,
                        context,
                        dart_prefix.as_deref(),
                    ))
            })
            // C call syntax in Objective-C is no message, and a message to
            // `self` or `super` stays in the sender's hierarchy (upstream
            // #2150, #2179).
            .filter(|n| !(objc_c_call && crate::objc_scope::is_objc_member_kind(n.kind)))
            .filter(|n| {
                objc_send.is_none_or(|to_super| {
                    crate::objc_scope::is_objc_self_send_target(
                        n,
                        reference,
                        objc_hierarchy
                            .get_or_init(|| {
                                crate::objc_scope::objc_hierarchy_at(reference, context)
                            })
                            .as_deref(),
                        to_super,
                    )
                })
            })
            // A bare Java call reaches a method in scope (upstream #2126).
            .filter(|n| {
                !(java_bare
                    && n.kind == NodeKind::Method
                    && !java_call_scope
                        .get_or_init(|| crate::java_scope::JavaCallScope::of(reference, context))
                        .reaches(n, reference))
            })
            // A bare Ruby call in a class, only its ancestry (upstream #2147).
            .filter(|n| {
                !(ruby_bare
                    && n.kind == NodeKind::Method
                    && !crate::ruby_scope::is_ruby_method_in_scope(n, reference, context))
            })
            // A bare CFML call in a component, only its chain (upstream #2148).
            .filter(|n| {
                !(cfml_bare
                    && n.kind == NodeKind::Method
                    && !crate::cfml_scope::is_cfml_method_in_scope(n, reference, context))
            })
            // Kotlin's visibility, member reach, number bitwise extensions
            // and standard chain names, as in exact matching (upstream
            // #2146, #2172, #2174, #2168).
            .filter(|n| {
                !kotlin_call
                    || crate::kotlin_scope::is_kotlin_top_level_visible(n, reference, context)
            })
            .filter(|n| !kotlin_bare || kotlin_site.reaches(n, context))
            .filter(|n| !crate::kotlin_scope::is_kotlin_number_bitwise(n, reference))
            .filter(|n| {
                kotlin_std_chain
                    .as_deref()
                    .is_none_or(|receiver| crate::kotlin_scope::is_std_chain_target(n, receiver))
            })
            // A bare Scala name, only a member in reach (upstream #2160).
            .filter(|n| {
                !scala_bare || crate::scala_scope::is_scala_member_in_scope(n, reference, context)
            })
            // A C# type position, only a type (upstream #2121).
            .filter(|n| !csharp_type_ref || crate::csharp_types::can_name_in_type_position(n))
            // A bare C# name, only a member in scope (upstream #2151).
            .filter(|n| {
                !csharp_bare
                    || crate::csharp_types::is_csharp_member_in_scope(n, reference, context)
            })
            .filter(|n| {
                python_shape.as_ref().is_none_or(|shape| {
                    crate::python_values::fits_python_call_shape(n, shape, reference, context)
                })
            })
            // A Swift call's shape, as in exact matching (upstream #2170).
            .filter(|n| {
                swift_shape.as_ref().is_none_or(|shape| {
                    crate::swift_scope::is_swift_call_target(n, shape, reference, context)
                })
            })
            // A Go call's shape, as in exact matching (upstream #2167).
            .filter(|n| {
                go_shape
                    .as_ref()
                    .is_none_or(|shape| crate::go_calls::is_go_call_target(n, shape))
            })
            // A bare Rust name, only what is in scope (upstream #2124, #2214).
            .filter(|n| {
                !rust_scoped || crate::rust_scope::is_rust_name_in_scope(n, reference, context)
            })
            // A PHP call through `$this` / `self` / `parent`, only the class
            // hierarchy's method (upstream #2142).
            .filter(|n| {
                php_self
                    .as_ref()
                    .is_none_or(|scope| n.kind == NodeKind::Method && scope.admits(n, context))
            })
            // `new …MockData()` makes an instance of a type; a method is never
            // what it names (upstream #2141).
            .filter(|n| {
                !(reference.reference_kind == EdgeKind::Instantiates && n.kind == NodeKind::Method)
            })
            // Names are case-sensitive in every language but a handful: a
            // Java `@Test` is not a `test()` method, TS `Mock()` not `mock`,
            // Python `Path(…)` not `path`. Only those whose identifiers
            // resolve regardless of case use this fallback's lowercase index
            // as such (upstream #2139).
            .filter(|n| case_insensitive || n.name == reference.reference_name)
            .collect(),
        reference,
    );

    let same_language: Vec<Arc<Node>> = callable_candidates
        .iter()
        .filter(|n| n.language == reference.language)
        .cloned()
        .collect();
    // Upstream #2386 also prefers a Dart library's own declaration here. The
    // port leaves it out, as it can never apply: exact matching keeps every
    // own declaration this strategy keeps, so it has resolved the name
    // already, and where exact matching declines past the ambiguity ceiling,
    // this strategy declines first, on the unfiltered set.
    let final_candidates = if !same_language.is_empty() {
        same_language
    } else {
        callable_candidates
    };

    // Every gate below judges the ONE candidate this strategy would commit to,
    // never the candidate set: filtering the unreachable ones out of a crowd
    // would leave a single survivor and hand it every call of that name.
    // These gates may reject a unique guess; they must never manufacture one
    // (upstream v1.6.1 `matchFuzzy`). Cross-file visibility and sealed modules
    // are enforced post-pipeline for every strategy.
    let [candidate] = final_candidates.as_slice() else {
        return None;
    };
    let bare_js_rejects = is_bare_js_call(reference, context)
        && (is_type_member_kind(candidate.kind)
            || (candidate.file_path != reference.file_path
                && is_locally_bound_js_name(
                    &reference.reference_name,
                    &reference.file_path,
                    context,
                )));
    // A name the file binds from outside the repository names nothing in it,
    // for every reference kind (upstream #2120, #2134).
    let out_of_repo = is_js_family(reference.language)
        && is_out_of_repo_binding(&reference.reference_name, reference, context);
    if bare_js_rejects
        || out_of_repo
        || (candidate.kind == NodeKind::Method
            && (is_bare_go_call(reference, context) || rust_shape == Some(RustCallShape::Bare)))
        // A bare PHP call is a function call (case-insensitive, so fuzzy may
        // find one): never the class `View` for `view(…)`, never a method
        // (upstream #2103).
        || (candidate.kind != NodeKind::Function
            && crate::php_scope::is_bare_php_call(reference, context))
        // A builtin method call (`res.text()`) whose only same-named project
        // symbol is some function's closure must decline (#1708).
        || !is_lexically_reachable(candidate, reference, context)
    {
        return None;
    }
    let is_cross_language = candidate.language != reference.language;
    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: candidate.id.clone(),
        confidence: if is_cross_language { 0.3 } else { 0.5 },
        resolved_by: ResolvedBy::Fuzzy,
    })
}

/// Resolve a `::`-scoped factory chain (`matchScopedCallChain`,
/// `name-matcher.ts:583-598`).
pub fn match_scoped_call_chain(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let (inner, method) = parse_chain_shape(&reference.reference_name)?;
    if !inner.contains("::") {
        return None;
    }
    let factory_class = &inner[..inner.rfind("::").unwrap()];
    let ret = lookup_callee_return_type(&inner, reference, context)?;
    let resolved_class = if ret == "self" {
        factory_class.to_string()
    } else {
        ret
    };
    resolve_method_on_type(
        &resolved_class,
        &method,
        reference,
        context,
        0.85,
        ResolvedBy::InstanceMethod,
        None,
        0,
    )
}

/// Resolve a dotted chained static-factory / fluent call (`matchDottedCallChain`,
/// `name-matcher.ts:620-678`).
///
/// NOTE: the Go bare-factory fallback (name-matcher.ts:632-657) calls into
/// `matchByExactName`/`matchFuzzy`; we keep the structure but the deterministic
/// core only relies on the declared-return-type path. Construction-via-bare-call
/// (`CONSTRUCTS_VIA_BARE_CALL`, name-matcher.ts:608) is mirrored.
pub fn match_dotted_call_chain(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let (inner, method) = parse_chain_shape(&reference.reference_name)?;
    let last_dot = inner.rfind('.');

    match last_dot {
        None | Some(0) => {
            // Go bare package-level factory (name-matcher.ts:632-658).
            if reference.language == Language::Go {
                if let Some(ret) = lookup_callee_return_type(&inner, reference, context) {
                    return resolve_method_on_type(
                        &ret,
                        &method,
                        reference,
                        context,
                        0.85,
                        ResolvedBy::InstanceMethod,
                        imported_fqn_of(&ret, reference, context).as_deref(),
                        0,
                    );
                }
                let bare_ref = RefView {
                    reference_name: method.clone(),
                    ..reference.clone()
                };
                let bare_match = match_by_exact_name(&bare_ref, context)
                    .or_else(|| match_fuzzy(&bare_ref, context));
                return bare_match.map(|m| ResolvedRef {
                    original: reference.clone(),
                    ..m
                });
            }
            // Constructor receiver in construct-via-bare-call languages
            // (name-matcher.ts:659-667).
            if !constructs_via_bare_call(reference.language)
                || !inner.chars().next().is_some_and(|c| c.is_uppercase())
            {
                return None;
            }
            resolve_method_on_type(
                &inner,
                &method,
                reference,
                context,
                0.85,
                ResolvedBy::InstanceMethod,
                imported_fqn_of(&inner, reference, context).as_deref(),
                0,
            )
        }
        Some(dot) => {
            // Factory/fluent receiver (name-matcher.ts:670-677).
            let factory_class = inner[..dot].rsplit('.').next()?;
            let factory_method = &inner[dot + 1..];
            if factory_class.is_empty() || factory_method.is_empty() {
                return None;
            }
            let ret = lookup_callee_return_type(
                &format!("{factory_class}::{factory_method}"),
                reference,
                context,
            )?;
            resolve_method_on_type(
                &ret,
                &method,
                reference,
                context,
                0.85,
                ResolvedBy::InstanceMethod,
                imported_fqn_of(&ret, reference, context).as_deref(),
                0,
            )
        }
    }
}

/// `CONSTRUCTS_VIA_BARE_CALL` (name-matcher.ts:608).
fn constructs_via_bare_call(language: Language) -> bool {
    matches!(
        language,
        Language::Kotlin | Language::Swift | Language::Scala | Language::Dart
    )
}

/// Matches `/^(.+)\(\)\.(\w+)$/` (name-matcher.ts:565,587,624).
fn parse_chain_shape(name: &str) -> Option<(String, String)> {
    let suffix_start = name.rfind("().")?;
    let inner = &name[..suffix_start];
    let method = &name[suffix_start + 3..];
    if inner.is_empty() || method.is_empty() || !is_word(method) {
        return None;
    }
    Some((inner.to_string(), method.to_string()))
}

/// `importedFqnOf` (`name-matcher.ts:685-692`).
fn imported_fqn_of(
    type_name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    context
        .get_import_mappings(&reference.file_path, reference.language)
        .into_iter()
        .find(|i| i.local_name == type_name)
        .map(|i| i.source)
}

/// `lookupCalleeReturnType` (`name-matcher.ts:440-475`).
fn lookup_callee_return_type(
    callee: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let mut method = callee.to_string();
    let mut cls: Option<String> = None;
    if callee.contains("::") {
        let parts: Vec<&str> = callee.split("::").filter(|s| !s.is_empty()).collect();
        method = parts.last().copied().unwrap_or(callee).to_string();
        cls = Some(parts[..parts.len() - 1].join("::"));
    }
    let candidates: Vec<Arc<Node>> = context
        .get_nodes_by_name_shared(&method)
        .into_iter()
        .filter(|n| {
            matches!(n.kind, NodeKind::Method | NodeKind::Function)
                && n.language == reference.language
                && n.return_type.as_ref().is_some_and(|r| !r.is_empty())
        })
        .collect();

    if let Some(cls) = cls {
        let want = format!("{cls}::{method}");
        return candidates
            .iter()
            .find(|n| {
                n.qualified_name == want
                    || n.qualified_name.ends_with(&format!("::{want}"))
                    || want.ends_with(&format!("::{}", n.qualified_name))
            })
            .and_then(|n| n.return_type.clone());
    }
    candidates
        .iter()
        .find(|n| n.kind == NodeKind::Function)
        .and_then(|n| n.return_type.clone())
}

/// How a call the extractor recorded by its bare name is written in the
/// source (upstream #2223 `bareCallReceiver`).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BareCallReceiver {
    /// `this.m()`, `self.m()`, `super.m()`, `super().m()`.
    OwnObject,
    /// Any other receiver: `window.$events.m()`, `make().m()`, `self.0.m()`.
    /// `receiver` is the end of the receiver's text, its last 40 characters;
    /// `links` are the member names between `this` and the method, for a
    /// `this.a.b.m()` chain.
    Other {
        receiver: String,
        links: Vec<String>,
    },
}

/// An ASCII word character or `$` (JS `[\w$]`).
fn is_js_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

/// Where the receiver of a bare-recorded call is written, read from up to
/// eight source lines from the reference's column: the first `name` not
/// preceded by a word character and followed (past `<…>` or Scala `[…]` type
/// arguments) by `(` or `{`, whose preceding text ends in `.` or `?.`.
/// `None` for a call written bare, or not found.
pub(crate) fn bare_call_receiver(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<BareCallReceiver> {
    let name = reference.reference_name.as_str();
    if reference.reference_kind != EdgeKind::Calls || !is_simple_js_identifier(name) {
        return None;
    }
    let facts = context.source_facts(&reference.file_path)?;
    let start = usize::try_from(reference.line).ok()?.checked_sub(1)?;
    let end = (start + 8).min(facts.line_count());
    if start >= end {
        return None;
    }
    let window = facts.join_lines(start, end, "\n");
    let column = usize::try_from(reference.column).unwrap_or(0);
    let text = window.get(column..)?;
    let tail = Regex::new(&format!(
        r"^{}\s*(?:<[^<>()]*>|\[(?:[^\[\]]|\[[^\[\]]*\])*\])?\s*[({{]",
        regex::escape(name)
    ))
    .ok()?;
    let at = text.match_indices(name).map(|(at, _)| at).find(|&at| {
        !text[..at].bytes().next_back().is_some_and(is_js_word_byte) && tail.is_match(&text[at..])
    })?;
    let before = text[..at].trim_end();
    let head = before.strip_suffix('.')?;
    let head = head.strip_suffix('?').unwrap_or(head).trim_end();
    static OWN_OBJECT: OnceLock<Regex> = OnceLock::new();
    static SUPER_CALL: OnceLock<Regex> = OnceLock::new();
    static THIS_CHAIN: OnceLock<Regex> = OnceLock::new();
    let own_object = OWN_OBJECT.get_or_init(|| {
        Regex::new(r"(?:^|[^A-Za-z0-9_$.])(?:this|self|super|Self)$").expect("own object")
    });
    let super_call = SUPER_CALL
        .get_or_init(|| Regex::new(r"(?:^|[^A-Za-z0-9_$.])super\s*\([^()]*\)$").expect("super()"));
    if own_object.is_match(head) || super_call.is_match(head) {
        return Some(BareCallReceiver::OwnObject);
    }
    let this_chain = THIS_CHAIN.get_or_init(|| {
        Regex::new(r"(?:^|[^A-Za-z0-9_$.#])((?:this|super)(?:\s*\??\.\s*#?[A-Za-z0-9_$]+)+)$")
            .expect("this chain")
    });
    let links = this_chain
        .captures(head)
        .map(|chain| {
            chain[1]
                .split('.')
                .skip(1)
                .map(|link| {
                    link.chars()
                        .filter(|c| !c.is_whitespace() && *c != '?')
                        .collect()
                })
                .collect()
        })
        .unwrap_or_default();
    let skip = head.chars().count().saturating_sub(40);
    Some(BareCallReceiver::Other {
        receiver: head.chars().skip(skip).collect(),
        links,
    })
}

/// Whether a call recorded by its bare name is written on something other
/// than the caller's own object (upstream #2221, #2223
/// `isCollapsedNonRecursion`).
fn is_collapsed_non_recursion(reference: &RefView, context: &dyn ResolutionContext) -> bool {
    match bare_call_receiver(reference, context) {
        None | Some(BareCallReceiver::OwnObject) => false,
        Some(BareCallReceiver::Other { links, .. }) => {
            !(is_js_family(reference.language)
                && is_collapsed_self_recursion(&links, reference, context))
        }
    }
}

/// Whether a collapsed `this.<field>.m()` inside `m` can be `m` itself: only
/// when the class declares the field as its own type, a tree node's
/// `this.left.insert(v)` (upstream #2221 `isCollapsedSelfRecursion`).
fn is_collapsed_self_recursion(
    links: &[String],
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    let [link] = links else {
        return false;
    };
    if link.contains('(') {
        return false;
    }
    let field = link.strip_prefix('#').unwrap_or(link);
    let Some(caller) = context.get_node_by_id_shared(&reference.from_node_id) else {
        return false;
    };
    let Some(cut) = caller.qualified_name.rfind("::").filter(|&cut| cut > 0) else {
        return false;
    };
    let owner = caller.qualified_name[..cut]
        .rsplit("::")
        .next()
        .unwrap_or_default();
    let Some(class) = context
        .get_nodes_in_file_shared(&reference.file_path)
        .into_iter()
        .find(|node| {
            node.kind == NodeKind::Class
                && node.name == owner
                && node.start_line <= reference.line
                && node.end_line >= reference.line
        })
    else {
        return false;
    };
    let Some(facts) = context.source_facts(&reference.file_path) else {
        return false;
    };
    let first = usize::try_from(class.start_line)
        .unwrap_or(1)
        .saturating_sub(1);
    let last = usize::try_from(class.end_line)
        .unwrap_or(0)
        .min(facts.line_count());
    if first >= last {
        return false;
    }
    let body = facts.join_lines(first, last, "\n");
    let field = regex::escape(field);
    let annotated = Regex::new(&format!(
        r"(?m)(?:^|[\s(,])#?{field}\s*[?!]?\s*:\s*([A-Za-z_$][A-Za-z0-9_$]*)"
    ));
    let constructed = Regex::new(&format!(
        r"(?-u:\b)this\.{field}\s*=\s*new\s+([A-Za-z_$][A-Za-z0-9_$]*)"
    ));
    let declared = annotated
        .ok()
        .and_then(|re| re.captures(&body).map(|c| c[1].to_string()))
        .or_else(|| {
            constructed
                .ok()
                .and_then(|re| re.captures(&body).map(|c| c[1].to_string()))
        });
    declared.as_deref() == Some(owner)
}

/// A declaration's dotted owner path: its file's `package` clauses, then its
/// enclosing types (`cats.kernel.instances`, `cats.Functor`; upstream #2229
/// `ownerPathOf`).
fn owner_path_of(node: &Node, context: &dyn ResolutionContext) -> String {
    static PACKAGE: OnceLock<Regex> = OnceLock::new();
    let package = PACKAGE.get_or_init(|| {
        Regex::new(r"(?m)^\s*package\s+([A-Za-z0-9_.]+)\s*;?\s*$").expect("package clause")
    });
    let text = context.read_file(&node.file_path).unwrap_or_default();
    let pkg = package
        .captures_iter(&text)
        .map(|c| c[1].to_string())
        .collect::<Vec<_>>()
        .join(".");
    let owners = node
        .qualified_name
        .rfind("::")
        .filter(|&cut| cut > 0)
        .map(|cut| node.qualified_name[..cut].replace("::", "."))
        .unwrap_or_default();
    [pkg.as_str(), owners.as_str()]
        .into_iter()
        .filter(|part| {
            !part.is_empty() && !(!pkg.is_empty() && *part == owners && owners.starts_with(&pkg))
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// The supertype an inheritance ref names when the name is the declaring
/// type's own: the other type of that name the written qualifier
/// (`cats.kernel.instances.`, `Functor.`) leads to by its owner path, or
/// `None` unless exactly one fits (upstream #2229 `otherSupertypeNamed`).
fn other_supertype_named(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let name = reference
        .reference_name
        .rsplit("::")
        .next()
        .and_then(|tail| tail.rsplit('.').next())
        .unwrap_or_default();
    if name.is_empty() {
        return None;
    }
    let line = usize::try_from(reference.line)
        .ok()
        .and_then(|line| line.checked_sub(1))
        .and_then(|index| {
            context
                .source_facts(&reference.file_path)
                .and_then(|facts| facts.raw_line(index).map(str::to_string))
        })
        .unwrap_or_default();
    let column = usize::try_from(reference.column).unwrap_or(0);
    let rest = line.get(column..).unwrap_or_default();
    let qualifier = Regex::new(&format!(
        r"^\s*((?:[A-Za-z0-9_$]+\.)+){}(?-u:\b)",
        regex::escape(name)
    ))
    .ok()
    .and_then(|re| {
        re.captures(rest)
            .map(|c| c[1].trim_end_matches('.').to_string())
    })
    .unwrap_or_default();
    let candidates: Vec<Arc<Node>> = context
        .get_nodes_by_name_shared(name)
        .into_iter()
        .filter(|node| {
            node.id != reference.from_node_id
                && node_is_eligible_target(reference.reference_kind, node)
                && same_language_family(node.language, reference.language)
                && (qualifier.is_empty() || {
                    let path = owner_path_of(node, context);
                    path == qualifier || path.ends_with(&format!(".{qualifier}"))
                })
        })
        .collect();
    // Among several, the declaring file's own, then for `implements` a
    // protocol, interface or trait (upstream #2242): SDWebImage's
    // `@interface SDWebImageCacheKeyFilter : NSObject <SDWebImageCacheKeyFilter>`.
    let mut pool = candidates;
    if pool.len() > 1 && pool.iter().any(|n| n.file_path == reference.file_path) {
        pool.retain(|n| n.file_path == reference.file_path);
    }
    let conformable = |n: &Arc<Node>| {
        matches!(
            n.kind,
            NodeKind::Protocol | NodeKind::Interface | NodeKind::Trait
        )
    };
    if pool.len() > 1
        && reference.reference_kind == EdgeKind::Implements
        && pool.iter().any(conformable)
    {
        pool.retain(conformable);
    }
    let [candidate] = pool.as_slice() else {
        return None;
    };
    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: candidate.id.clone(),
        confidence: 0.8,
        resolved_by: ResolvedBy::QualifiedName,
    })
}

/// Reference kinds a bare JS/TS local can be: a call, a value (function refs
/// included), a construction (upstream `JS_LOCAL_REF_KINDS`).
fn is_js_local_ref_kind(kind: EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Calls | EdgeKind::References | EdgeKind::Instantiates
    )
}

/// A name the calling JS/TS function binds itself shadows the file's own
/// (upstream #2226, in `matchReference`): lodash's `mixin(object, …)` calling
/// `object(this.__wrapped__)` is its parameter, whichever strategy (fuzzy
/// included) found a `function object`. A target of another name is what the
/// local was followed to (`const selected = useStore(s => s.reset);
/// selected()` is the store's `reset`) and stays.
pub fn veto_js_local_shadow(
    result: Option<ResolvedRef>,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let result = result?;
    if !is_js_local_ref_kind(reference.reference_kind) {
        return Some(result);
    }
    let Some(target) = context.get_node_by_id_shared(&result.target_node_id) else {
        return Some(result);
    };
    if target.name == reference.reference_name && is_outside_js_local(&target, reference, context) {
        return None;
    }
    Some(result)
}

/// Whether `node` lies outside the function that binds the reference's name
/// itself ([`js_function_local_scope`]; upstream `isOutsideJsLocal`).
fn is_outside_js_local(node: &Node, reference: &RefView, context: &dyn ResolutionContext) -> bool {
    // `const indexName = this.dataSource.namingStrategy.indexName(…)`: a
    // member, whatever the local's name.
    if reference.reference_kind == EdgeKind::Calls
        && bare_call_receiver(reference, context).is_some()
    {
        return false;
    }
    js_function_local_scope(&reference.reference_name, reference, context).is_some_and(
        |(start, end)| {
            !(node.file_path == reference.file_path
                && start <= node.start_line
                && node.start_line <= end)
        },
    )
}

/// The lines of the JS/TS function a reference sits in when that function
/// binds the name itself, a parameter or a `var` / `let` / `const` above the
/// reference (upstream #2226 `jsFunctionLocalScope`). Such a name is the
/// local, never a same-named function declared elsewhere: every lodash
/// helper lives inside `runInContext`, so `baseHas(object, key)`'s `object`
/// reached a `function object() {}` an IIFE declares there. `None` when the
/// function does not bind it.
pub(crate) fn js_function_local_scope(
    name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<(i64, i64)> {
    if !is_js_family(reference.language) || !is_simple_js_identifier(name) {
        return None;
    }
    let function = context.get_node_by_id_shared(&reference.from_node_id)?;
    if !matches!(function.kind, NodeKind::Function | NodeKind::Method)
        || function.start_line > reference.line
        || function.end_line < reference.line
    {
        return None;
    }
    let facts = context.source_facts(&reference.file_path)?;
    // Every function that starts on the same line has the same code through
    // the reference's line, so they share the answer (upstream #2334).
    let key = format!("{}\0{}\0{name}", function.start_line, reference.line);
    facts
        .node_decision("js_code_binds", &key, |facts| {
            js_code_binds_name(name, &function, reference.line, facts)
        })
        .then_some((function.start_line, function.end_line))
}

/// Whether `function`'s code from its first line through `line` declares
/// `name` with `var` / `let` / `const` or names it in a parameter list
/// (upstream `jsCodeBindsName`), comments and strings blanked.
fn js_code_binds_name(name: &str, function: &Node, line: i64, facts: &SourceFacts) -> bool {
    let Some(start) = usize::try_from(function.start_line)
        .ok()
        .and_then(|line| line.checked_sub(1))
    else {
        return false;
    };
    let through = usize::try_from(line.min(function.end_line))
        .unwrap_or(0)
        .min(facts.line_count());
    if start >= through {
        return false;
    }
    let code = crate::strip_comments::strip_comments_for_regex(
        &facts.join_lines(start, through, "\n"),
        crate::strip_comments::CommentLang::JavaScript,
    );
    let Some(patterns) = js_local_patterns(name) else {
        return false;
    };
    // A plain declaration. Destructuring re-binds what a call returns under
    // the same name (`const { t } = useI18n()`), which is the same-named
    // function more often than not, so `const a, …` / `[a]` / `{ a }` forms
    // (`(?!\s*[,\]}])` upstream) do not count.
    let declared = patterns.declared.find_iter(&code).any(|found| {
        !code[found.end()..]
            .trim_start()
            .starts_with([',', ']', '}'])
    });
    // A parameter list, never a control-flow head (`if (openMarkerClose) {`):
    // tried at each `(` not preceded by `if` / `while` / `for` / `switch` /
    // `with` (upstream's `(?=\()(?<!…)` lookbehind).
    declared
        || code.match_indices('(').any(|(at, _)| {
            !follows_control_keyword(&code[..at]) && patterns.parameter.is_match(&code[at..])
        })
}

/// Whether `before` ends, past trailing whitespace, with `if`, `while`,
/// `for`, `switch` or `with` as a whole word.
fn follows_control_keyword(before: &str) -> bool {
    let before = before.trim_end();
    ["if", "while", "for", "switch", "with"]
        .iter()
        .any(|keyword| {
            before.strip_suffix(keyword).is_some_and(|head| {
                !head
                    .bytes()
                    .next_back()
                    .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            })
        })
}

/// [`js_code_binds_name`]'s patterns for one plain identifier.
struct JsLocalPatterns {
    declared: Regex,
    /// [`LocalBindingPatterns`]' parameter form, its return type kept to one
    /// line (`(?::[^=;{}()\n]*)?`), so a ternary's `: data.slice()` below
    /// `filter(canRowExpand)` is no return type.
    parameter: Regex,
}

/// Patterns by name, cached like [`local_binding_patterns`].
fn js_local_patterns(name: &str) -> Option<Arc<JsLocalPatterns>> {
    const CAP: usize = 1024;
    type PatternCache = (
        HashMap<String, Option<Arc<JsLocalPatterns>>>,
        std::collections::VecDeque<String>,
    );
    static CACHE: OnceLock<Mutex<PatternCache>> = OnceLock::new();
    let cache =
        CACHE.get_or_init(|| Mutex::new((HashMap::new(), std::collections::VecDeque::new())));
    if let Some(patterns) = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .0
        .get(name)
    {
        return patterns.clone();
    }
    let n = regex::escape(name);
    let declared = Regex::new(&format!(r"(?-u:\b)(?:const|let|var)\s+{n}(?-u:\b)")).ok();
    let parameter = Regex::new(&format!(
        r"^\(\s*(?:(?:\.\.\.)?[0-9A-Za-z_$]+(?:\s*(?:\?\s*)?:[^,()]+|\s*=[^,()]+|\s*),\s*)*{n}(?-u:\b)(?:\s*\??\s*:[^,()]*)?(?:\s*=[^,()]*)?(?:\s*,\s*[^()]*)?\)\s*(?::[^=;{{}}()\n]*)?(?:=>|\{{)"
    ))
    .ok();
    let patterns = declared.zip(parameter).map(|(declared, parameter)| {
        Arc::new(JsLocalPatterns {
            declared,
            parameter,
        })
    });
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (map, order) = &mut *guard;
    if !map.contains_key(name) {
        if map.len() >= CAP
            && let Some(oldest) = order.pop_front()
        {
            map.remove(&oldest);
        }
        map.insert(name.to_string(), patterns.clone());
        order.push_back(name.to_string());
    }
    patterns
}

/// C has no methods, and C code cannot call a C++ one (upstream #2234, in
/// `matchReference`): hiredis' function pointer `c->funcs->read(c, buf, …)`
/// and redis' POSIX `read(fd, buf, n)` are no Qt adapter's `read`.
pub fn veto_c_method_target(
    result: Option<ResolvedRef>,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let result = result?;
    if reference.language == Language::C
        && reference.reference_kind == EdgeKind::Calls
        && context
            .get_node_by_id_shared(&result.target_node_id)
            .is_some_and(|target| target.kind == NodeKind::Method)
    {
        return None;
    }
    Some(result)
}

/// How a bare Objective-C name is written at its site (upstream #2150
/// `objcCallShape`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObjcCallShape {
    /// C call syntax (`completionBlock()`): a function, a block or a function
    /// pointer, never a method or property.
    CCall,
    /// A message to `self` or `[self class]`, whose receiver the extractor
    /// drops.
    SelfSend,
    /// A message to `super`.
    SuperSend,
}

/// Whether a shape is a message to `self` (`Some(false)`) or to `super`
/// (`Some(true)`), which reach only the sender's hierarchy.
fn objc_send_target(shape: Option<ObjcCallShape>) -> Option<bool> {
    match shape {
        Some(ObjcCallShape::SelfSend) => Some(false),
        Some(ObjcCallShape::SuperSend) => Some(true),
        _ => None,
    }
}

/// The shape of the Objective-C call `reference` (a bare name or selector) on
/// its line, `None` when the line shows neither.
fn objc_call_shape(reference: &RefView, context: &dyn ResolutionContext) -> Option<ObjcCallShape> {
    static SELECTOR: OnceLock<Regex> = OnceLock::new();
    static MESSAGE: OnceLock<Regex> = OnceLock::new();
    static SUPER: OnceLock<Regex> = OnceLock::new();
    static SELF: OnceLock<Regex> = OnceLock::new();
    if reference.language != Language::ObjC
        || reference.reference_kind != EdgeKind::Calls
        || !SELECTOR
            .get_or_init(|| {
                Regex::new(r"^[A-Za-z_][0-9A-Za-z_]*:*(?:[0-9A-Za-z_]+:)*$").expect("selector")
            })
            .is_match(&reference.reference_name)
    {
        return None;
    }
    let facts = context.source_facts(&reference.file_path)?;
    let index = usize::try_from(reference.line - 1)
        .ok()
        .filter(|&index| index < facts.line_count())?;
    let line = facts.line(index);
    let name = reference.reference_name.split(':').next()?;
    if name.is_empty() {
        return None;
    }
    let word = Regex::new(&format!(r"\b{}\b", regex_escape(name))).ok()?;
    let message = MESSAGE.get_or_init(|| Regex::new(r"\[\s*[\w.]+\s+$").expect("message"));
    let to_super = SUPER.get_or_init(|| Regex::new(r"\[\s*super\s+$").expect("super"));
    let to_self =
        SELF.get_or_init(|| Regex::new(r"\[\s*(?:self|\[\s*self\s+class\s*\])\s+$").expect("self"));
    let mut shape = None;
    for found in word.find_iter(line) {
        // Not a member written after `.` (`object.name`), nor the tail of a
        // longer word.
        if line[..found.start()]
            .chars()
            .next_back()
            .is_some_and(|c| c == '.' || c == '_' || c.is_alphanumeric())
        {
            continue;
        }
        let before = &line[..found.start()];
        let after = &line[found.end()..];
        if after.trim_start().starts_with('(') && !message.is_match(before) {
            shape.get_or_insert(ObjcCallShape::CCall);
        } else if to_super.is_match(before) {
            return Some(ObjcCallShape::SuperSend);
        } else if to_self.is_match(before) {
            return Some(ObjcCallShape::SelfSend);
        }
    }
    shape
}

/// The name matcher's self vetoes (upstream `matchReference`): a result that
/// lands on the reference's own node is the caller only when nothing rules it
/// out. A call written on another object is not (#2221, #2223); a type never
/// inherits from itself, so the other type of its name the qualifier leads to
/// is chosen instead (#2229); and a value's initializer never calls the value
/// (#2223).
pub fn veto_self_target(
    result: Option<ResolvedRef>,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let result = result?;
    if result.target_node_id != reference.from_node_id {
        return Some(result);
    }
    if is_collapsed_non_recursion(reference, context) {
        return None;
    }
    if crate::types::is_inheritance_ref(reference.reference_kind) {
        return other_supertype_named(reference, context);
    }
    if reference.reference_kind == EdgeKind::Calls
        && context
            .get_node_by_id_shared(&reference.from_node_id)
            .is_some_and(|from| {
                matches!(
                    from.kind,
                    NodeKind::Variable | NodeKind::Constant | NodeKind::Field | NodeKind::Property
                )
            })
    {
        return None;
    }
    Some(result)
}

/// Match all strategies in order of confidence (`matchReference`,
/// `name-matcher.ts:1093-1157`).
pub fn match_reference(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    // `import java.lang.reflect.Field;`: the file's `Field` is the JDK's,
    // never a project class of that name, by any strategy (upstream #2195:
    // gson's production code bound it to a test's nested
    // `ParameterizedTypesTest.Field`).
    if crate::java_scope::binds_an_outside_import(reference, context) {
        return None;
    }

    // A Swift call through a type path (`API.PackageController.GetRoute.query`)
    // resolves on the type the path names, or not at all: the strategies below
    // would bind it by the member's name alone (upstream #2118).
    if crate::swift_type_visibility::is_swift_type_path_call(reference) {
        return crate::swift_type_visibility::resolve_swift_type_path_call(reference, context);
    }

    // A symbolic name in a Scala type is a type (`F ~> G`) or a
    // kind-projector placeholder (`Either[A, *]`), never an operator method,
    // by any strategy (upstream #2217).
    if let Some(result) = crate::scala_scope::match_scala_symbolic_type(reference, context) {
        return result;
    }

    // A bare Lua call through a `local` alias reaches what the alias names
    // (upstream #2188); `ipairs(t)` is Lua's, unless the file defines its own
    // (upstream #2202: telescope's 122 `for _, v in ipairs(…)` went to a
    // linked list's `ipairs` method).
    if crate::lua_scope::is_lua_bare_call(reference) {
        if let Some(aliased) = crate::lua_scope::lua_alias_target(reference, context) {
            return aliased;
        }
        if crate::lua_scope::LUA_GLOBAL_FUNCTIONS.contains(&reference.reference_name.as_str())
            && !context
                .get_nodes_in_file_shared(&reference.file_path)
                .iter()
                .any(|node| {
                    node.name == reference.reference_name && node.kind == NodeKind::Function
                })
        {
            return None;
        }
    }

    // A Scala chain link the extractor records with its last receiver
    // segment, `b.c` for `a.b.c()` (KEEP-RUST G14), is the bare `c` upstream
    // records: what the receiver strategies below choose must pass
    // upstream's chain-link rule for it, and when they choose nothing the
    // bare name matches as upstream matches it (upstream #2160, #2199).
    let scala_link =
        crate::scala_scope::scala_nested_chain_link(reference, context).map(|bare| RefView {
            reference_name: bare,
            ..reference.clone()
        });
    // So is a Swift one (`entries.removeAll` for `axis.entries.removeAll()`,
    // KEEP-RUST #1496): upstream's chained-call rule for the bare name
    // (#2170).
    let swift_link =
        crate::swift_scope::swift_nested_chain_link(reference, context).map(|bare| RefView {
            reference_name: bare,
            ..reference.clone()
        });
    let in_link_scope = |result: ResolvedRef| -> Option<ResolvedRef> {
        if let Some(bare) = scala_link.as_ref() {
            let target = context.get_node_by_id_shared(&result.target_node_id)?;
            return crate::scala_scope::is_scala_member_in_scope(&target, bare, context)
                .then_some(result);
        }
        if let Some(bare) = swift_link.as_ref() {
            let target = context.get_node_by_id_shared(&result.target_node_id)?;
            let shape = crate::swift_scope::swift_call_shape(bare, context);
            return shape
                .as_ref()
                .is_none_or(|shape| {
                    crate::swift_scope::is_swift_call_target(&target, shape, bare, context)
                })
                .then_some(result);
        }
        Some(result)
    };

    // `window.App.init()` / a sibling's `this.init()`, recorded by the bare
    // name: the object they are written on comes first (upstream #2363).
    if let Some(result) = crate::object_owners::match_collapsed_object_call(reference, context) {
        return Some(result);
    }

    // 0. File path match.
    if let Some(result) = match_by_file_path(reference, context) {
        return Some(result);
    }

    // 1. Qualified name match.
    if let Some(result) = match_by_qualified_name(reference, context) {
        return Some(result);
    }

    // Erlang local call/fun refs (`f/1`) resolve only to the definition of the
    // written arity. Language semantics make the call site's own module the
    // strongest signal; a missing arity stays unresolved rather than linking a
    // same-name sibling.
    if reference.language == Language::Erlang
        && !reference.reference_name.contains("::")
        && matches!(
            reference.reference_kind,
            EdgeKind::Calls | EdgeKind::References
        )
        && let Some((name, arity)) = split_erlang_arity(&reference.reference_name)
    {
        let arity_tail = format!("/{arity}");
        let candidates: Vec<Arc<Node>> = context
            .get_nodes_by_name_shared(name)
            .into_iter()
            .filter(|candidate| {
                candidate.language == Language::Erlang
                    && candidate.kind == NodeKind::Function
                    && candidate.qualified_name.ends_with(&arity_tail)
            })
            .collect();
        if let Some(same_file) = candidates
            .iter()
            .find(|candidate| candidate.file_path == reference.file_path)
        {
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: same_file.id.clone(),
                confidence: 0.95,
                resolved_by: ResolvedBy::ExactMatch,
            });
        }
        if candidates.len() == 1 {
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: candidates[0].id.clone(),
                confidence: 0.8,
                resolved_by: ResolvedBy::ExactMatch,
            });
        }
        if let Some(best) = find_best_match(reference, &candidates) {
            let proximity = compute_path_proximity(&reference.file_path, &best.file_path);
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: best.id.clone(),
                confidence: if proximity >= 30.0 { 0.7 } else { 0.4 },
                resolved_by: ResolvedBy::ExactMatch,
            });
        }
        return None;
    }

    // 1c. `::`-scoped factory chain (PHP / Rust) (name-matcher.ts:1116-1123).
    if matches!(reference.language, Language::Php | Language::Rust) {
        if let Some(result) = match_scoped_call_chain(reference, context) {
            return Some(result);
        }
    }

    // 1d. Dotted chained static-factory / fluent call
    // (Java/Kotlin/C#/Swift/Go/Scala/Dart) (name-matcher.ts:1125-1142).
    if matches!(
        reference.language,
        Language::Java
            | Language::Kotlin
            | Language::CSharp
            | Language::Swift
            | Language::Go
            | Language::Scala
            | Language::Dart
    ) {
        if let Some(result) = match_dotted_call_chain(reference, context).and_then(&in_link_scope) {
            return Some(result);
        }
    }

    // A JS/TS/Python call-result receiver carries no proven result type. It
    // stays unresolved instead of degrading to a global same-named callable:
    // the fuzzy strategy would split `make().run` on `.` and hand it to any
    // `run` (#1683). The one exception is a store accessor, whose store is
    // identified (`useStore.getState().reset`, #1862).
    if reference.reference_kind == EdgeKind::Calls
        && (is_js_ts(reference.language) || reference.language == Language::Python)
        && reference.reference_name.contains("().")
    {
        return crate::js_store::match_store_accessor_chain(reference, context);
    }

    // 2. Method call pattern.
    if let Some(result) = match_method_call(reference, context).and_then(&in_link_scope) {
        return Some(result);
    }
    if let Some(bare) = scala_link.as_ref().or(swift_link.as_ref()) {
        return match_by_exact_name(bare, context)
            .or_else(|| match_fuzzy(bare, context))
            .map(|result| ResolvedRef {
                original: reference.clone(),
                ..result
            });
    }

    // 3. Exact name match.
    if let Some(result) = match_by_exact_name(reference, context) {
        return Some(result);
    }

    // 4. Fuzzy match.
    if let Some(result) = match_fuzzy(reference, context) {
        return Some(result);
    }

    // 5. A C / C++ name qualified by a namespace the project opens with a
    // macro (`fmt::format`, `FMT_BEGIN_NAMESPACE`), which the index cannot
    // see (upstream #2176).
    if matches!(reference.language, Language::C | Language::Cpp)
        && reference.reference_name.contains("::")
    {
        return crate::cpp_namespaces::match_cpp_macro_namespaced(reference, context);
    }
    None
}

fn split_erlang_arity(name: &str) -> Option<(&str, &str)> {
    let (base, arity) = name.rsplit_once('/')?;
    if base.is_empty() || !is_erlang_arity_digits(arity) {
        return None;
    }
    Some((base, arity))
}

fn is_erlang_arity_digits(value: &str) -> bool {
    (1..=3).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_digit())
}

pub(crate) fn is_python_class_function_ref_target(language: Language, kind: NodeKind) -> bool {
    language == Language::Python && kind == NodeKind::Class
}

/// Resolve a `function_ref` (callback-as-value) reference. Receiver-qualified
/// method values reuse the ordinary method-call target selection (#1820); bare
/// names use exact function/method targets plus Python class targets (Python
/// bare methods remain excluded), same language family, same-file first, and
/// cross-file only when unique. No fuzzy fallback. `this.<member>` refs are
/// resolved elsewhere (`resolve_this_member_fn_ref`). Ports `matchFunctionRef`
/// (name-matcher.ts:179-310) plus the issue #1820 extension.
pub fn match_function_ref(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if reference
        .reference_name
        .strip_prefix("this.")
        .is_some_and(|member| !member.contains('.'))
    {
        return None;
    }

    // Python and Go member values retain their receiver path and resolve
    // through the receiver's scope before a unique name (upstream #1820).
    if matches!(reference.language, Language::Python | Language::Go)
        && reference.reference_name.contains('.')
    {
        return crate::member_value::match_member_function_ref(reference, context);
    }

    // Receiver-qualified first-class method value (#1820): reuse the exact
    // same conservative target selection as `obj.method(...)`, but preserve
    // the original `references` kind and label the result as function-ref.
    // This gives callback wiring and direct calls identical receiver inference
    // without fabricating an immediate `calls` edge.
    if match_dot_call(&reference.reference_name).is_some() {
        let mut resolved = match_method_call(reference, context)?;
        resolved.resolved_by = ResolvedBy::FunctionRef;
        return Some(resolved);
    }

    // A bare identifier cannot be a method value in JS/TS/C++/Python/PHP
    // (methods need a receiver). Those match FUNCTIONS; Python additionally
    // accepts CLASS targets through the shared predicate, but still not METHODS.
    let bare_fn_only = matches!(
        reference.language,
        Language::TypeScript
            | Language::Tsx
            | Language::JavaScript
            | Language::Jsx
            | Language::Cpp
            | Language::Python
            | Language::Php
    );

    // Qualified member-pointer (`&Widget::on_click`): resolve the member on
    // that scope, unique-or-drop. Exempt from bare_fn_only.
    if let Some(sep) = reference.reference_name.rfind("::") {
        let member_name = &reference.reference_name[sep + 2..];
        let scoped: Vec<Arc<Node>> = context
            .get_nodes_by_name_shared(member_name)
            .into_iter()
            .filter(|n| {
                matches!(n.kind, NodeKind::Function | NodeKind::Method)
                    && same_language_family(n.language, reference.language)
                    && n.id != reference.from_node_id
                    && (n.qualified_name == reference.reference_name
                        || n.qualified_name
                            .ends_with(&format!("::{}", reference.reference_name)))
            })
            .collect();
        if scoped.is_empty() {
            return None;
        }
        let same_file: Vec<&Node> = scoped
            .iter()
            .map(Arc::as_ref)
            .filter(|n| n.file_path == reference.file_path)
            .collect();
        if same_file.is_empty() && scoped.len() > 1 {
            return None;
        }
        let pool: Vec<&Node> = if same_file.is_empty() {
            scoped.iter().map(Arc::as_ref).collect()
        } else {
            same_file
        };
        let target = pool
            .iter()
            .copied()
            .reduce(|a, b| if a.start_line <= b.start_line { a } else { b })?;
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: target.id.clone(),
            confidence: 0.9,
            resolved_by: ResolvedBy::FunctionRef,
        });
    }

    let named: Vec<Arc<Node>> = context
        .get_nodes_by_name_shared(&reference.reference_name)
        .into_iter()
        .filter(|n| {
            (n.kind == NodeKind::Function
                || (!bare_fn_only && n.kind == NodeKind::Method)
                || is_python_class_function_ref_target(reference.language, n.kind))
                && same_language_family(n.language, reference.language)
                && n.id != reference.from_node_id
        })
        .collect();
    // A function declared inside another is in scope only in there: httpx's
    // `self._build_auth(auth)` passes its own parameter, not the `auth` a test
    // defines inside `test_custom_auth`. Those still count against a lone
    // cross-file guess below: a name several functions use for themselves is
    // as likely a local's (upstream #2219).
    let mut candidates: Vec<Arc<Node>> = named
        .iter()
        .filter(|n| is_lexically_reachable(n, reference, context))
        // A Dart constructor is torn off through its class, never by a bare
        // name (upstream #2380).
        .filter(|n| !crate::dart_members::is_dart_constructor(n, context))
        .cloned()
        .collect();
    if candidates.is_empty() {
        return None;
    }
    // A Python name the function around it binds (a parameter, an
    // assignment) is that local's value: httpx's `auth_flow(self, request)`
    // handing `request` on is not the package's `request()` function. A
    // pytest fixture is what a test's parameter of its name receives.
    if reference.language == Language::Python
        && !candidates
            .iter()
            .any(|n| crate::python_values::is_fixture_in_reach(n, &reference.file_path, context))
        && crate::python_values::is_python_locally_bound(
            &reference.reference_name,
            reference,
            context,
        )
    {
        return None;
    }
    // Likewise a JS/TS parameter or local: lodash's `baseHas(object, key)`
    // passes its own `object` (upstream #2226).
    if let Some((start, end)) =
        js_function_local_scope(&reference.reference_name, reference, context)
    {
        candidates.retain(|n| {
            n.file_path == reference.file_path && start <= n.start_line && n.start_line <= end
        });
        if candidates.is_empty() {
            return None;
        }
    }

    // Swift implicit-self: a bare identifier names a METHOD only of the
    // ENCLOSING type; a same-named method on another class is a parameter
    // collision (name-matcher.ts:250-270).
    if reference.language == Language::Swift
        && candidates.iter().any(|n| n.kind == NodeKind::Method)
    {
        let class_prefix = context
            .get_node_by_id_shared(&reference.from_node_id)
            .and_then(|f| {
                let sep = f.qualified_name.rfind("::")?;
                if sep > 0 {
                    Some(f.qualified_name[..sep].to_string())
                } else {
                    None
                }
            });
        candidates.retain(|n| {
            if n.kind != NodeKind::Method {
                return true;
            }
            let Some(class_prefix) = class_prefix.as_deref() else {
                return false;
            };
            let Some(m_sep) = n.qualified_name.rfind("::") else {
                return false;
            };
            if m_sep == 0 {
                return false;
            }
            let method_prefix = &n.qualified_name[..m_sep];
            method_prefix == class_prefix
                || method_prefix.ends_with(&format!("::{class_prefix}"))
                || class_prefix.ends_with(&format!("::{method_prefix}"))
        });
        if candidates.is_empty() {
            return None;
        }
    }

    let same_file: Vec<&Node> = candidates
        .iter()
        .map(Arc::as_ref)
        .filter(|n| n.file_path == reference.file_path)
        .collect();
    if !same_file.is_empty() {
        // Swift: several same-named methods in one file is an overload family
        // and a bare id hitting it is almost always a parameter collision —
        // refuse (name-matcher.ts:282-288).
        if reference.language == Language::Swift
            && same_file.len() > 1
            && same_file.iter().all(|n| n.kind == NodeKind::Method)
        {
            return None;
        }
        let target = same_file
            .iter()
            .copied()
            .reduce(|a, b| if a.start_line <= b.start_line { a } else { b })?;
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: target.id.clone(),
            confidence: if same_file.len() == 1 { 0.95 } else { 0.9 },
            resolved_by: ResolvedBy::FunctionRef,
        });
    }

    // Cross-file: only an unambiguous match resolves, or, in Python, the one
    // in reach of a name the file imports (netbox's `sender=CustomField`
    // beside a test's own nested `CustomField`).
    if candidates.len() == 1
        && (named.len() == 1
            || (reference.language == Language::Python
                && crate::python_values::python_imports_name(
                    &reference.file_path,
                    &reference.reference_name,
                    context,
                )))
    {
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: candidates[0].id.clone(),
            confidence: 0.8,
            resolved_by: ResolvedBy::FunctionRef,
        });
    }
    None
}

#[cfg(test)]
mod js_module_tests {
    use std::sync::Arc;

    use super::{follows_control_keyword, read_module_exports};
    use crate::source_facts::SourceFacts;

    fn exports(source: &str) -> Option<Vec<String>> {
        read_module_exports(&SourceFacts::new(Arc::from(source))).map(|names| {
            let mut names: Vec<String> = names.into_iter().collect();
            names.sort();
            names
        })
    }

    #[test]
    fn an_es_modules_export_lists_name_its_exported_bindings() {
        let source = "import { a } from './a.js';\nfunction one() {}\nfunction two() {}\nexport { one, two as second, type Shape };\nexport default three;\nexport default { four, five: six, 'seven': eight };\n";
        assert_eq!(
            exports(source).unwrap(),
            ["Shape", "eight", "four", "one", "six", "three", "two"]
        );
    }

    #[test]
    fn classic_scripts_commonjs_and_ambient_globals_are_no_es_modules() {
        assert_eq!(exports("function legacy() {}\n"), None);
        assert_eq!(
            exports("import x from 'x';\nmodule.exports = { x };\n"),
            None
        );
        assert_eq!(
            exports("export {};\ndeclare global { interface Window { a: 1 } }\n"),
            None
        );
        assert_eq!(exports("export const a = 1;\n"), Some(Vec::new()));
    }

    #[test]
    fn control_flow_heads_are_no_parameter_lists() {
        assert!(follows_control_keyword("    if "));
        assert!(follows_control_keyword("x = 1; while"));
        assert!(follows_control_keyword("for"));
        assert!(!follows_control_keyword("elif"));
        assert!(!follows_control_keyword("function handler"));
        assert!(!follows_control_keyword(""));
    }
}

#[cfg(test)]
mod cpp_declared_type_tests {
    use super::{cpp_declared_type_name, normalize_cpp_type_name};

    #[test]
    fn a_capture_naming_no_type_lets_the_declarator_scan_go_on() {
        let is_cpp_non_type_capture = |capture: &str| normalize_cpp_type_name(capture).is_none();
        for capture in [
            "return",
            "return *",
            "throw",
            "delete",
            "sizeof",
            "",
            "&",
            "leveldb::",
        ] {
            assert!(is_cpp_non_type_capture(capture), "{capture:?}");
        }
        for capture in [
            "Table",
            "const Table&",
            "SymbolsByParentSet",
            "leveldb::Table*",
            "SkipList<Key, Comparator>",
            "auto",
        ] {
            assert!(!is_cpp_non_type_capture(capture), "{capture:?}");
        }
    }

    #[test]
    fn a_declared_cpp_type_is_read_as_upstream_reads_it() {
        let read = |declared: &str| cpp_declared_type_name(declared);
        assert_eq!(
            read("SkipList<Key, Comparator>").as_deref(),
            Some("SkipList")
        );
        assert_eq!(read("const ScopedHandle").as_deref(), Some("ScopedHandle"));
        assert_eq!(read("Table<int, Box<int>>&").as_deref(), Some("Table"));
        assert_eq!(
            read("leveldb::Table::Iterator*").as_deref(),
            Some("Iterator")
        );
        assert_eq!(read("std::vector<std::string>").as_deref(), Some("vector"));
        assert_eq!(
            read("typename T::value_type").as_deref(),
            Some("value_type")
        );
        // A type begun on an earlier line, an expression, or no type at all.
        assert_eq!(read("Foo<Bar>>>&"), None);
        assert_eq!(read("BlobContents>>>&"), None);
        assert_eq!(read("return"), None);
        assert_eq!(read("sizeof"), None);
        assert_eq!(read(""), None);
        assert_eq!(read("&"), None);
    }

    /// Upstream #2443: a receiver's declared type keeps no debris of nested
    /// template arguments, and the end of a declaration begun on an earlier
    /// line names no type; the namespaces it writes stay.
    #[test]
    fn a_receiver_type_drops_nested_template_arguments() {
        let read = |raw: &str| super::normalize_cpp_type_name(raw);
        assert_eq!(
            read("autovector<std::pair<int, FileMetaData*>>").as_deref(),
            Some("autovector")
        );
        assert_eq!(
            read("Striped<CacheAlignedWrapper<port::Mutex>>").as_deref(),
            Some("Striped")
        );
        assert_eq!(read("std::vector<Slice>*").as_deref(), Some("std::vector"));
        assert_eq!(read("port::Mutex&").as_deref(), Some("port::Mutex"));
        assert_eq!(read("auto&").as_deref(), Some("auto"));
        assert_eq!(read("std::unique_ptr<BlobContents>>>&"), None);
        assert_eq!(read("CachableEntry<BlobContents>>>&"), None);
        assert!(super::cpp_type_began_above("Blob>>>&"));
        assert!(!super::cpp_type_began_above("a<b<c>>"));
    }

    /// Upstream #2413: the declaration scan reads code, not comments.
    #[test]
    fn a_line_as_code_blanks_its_comments_and_keeps_columns() {
        let code = |line: &str| super::cpp_code_of(line);
        assert_eq!(code("  // a non-null imm_"), None);
        assert_eq!(code("   * imm_ is never null"), None);
        assert_eq!(
            code("/* static */ bool Status::Check(const std::string& msg) {").as_deref(),
            Some("             bool Status::Check(const std::string& msg) {")
        );
        assert_eq!(
            code("  MemTable* imm_;  // the one being compacted").as_deref(),
            Some("  MemTable* imm_;  ")
        );
        assert_eq!(code("  int x; /* runs on").as_deref(), Some("  int x; "));
    }

    /// Upstream #2413: a range-`for` variable or a structured binding
    /// re-declares the receiver in a form the declarator pattern doesn't read.
    #[test]
    fn a_range_for_variable_or_a_structured_binding_rebinds_the_receiver() {
        let rebinding = super::CppRebinding::new("key").expect("patterns");
        assert!(rebinding.rebinds("  for (const Slice& key : slices) {"));
        assert!(rebinding.rebinds("  for (auto& [key, value] : map) {"));
        assert!(rebinding.rebinds("  auto [key, value] = Split(line);"));
        assert!(rebinding.rebinds("  const auto& [first, key] = pair;"));
        assert!(!rebinding.rebinds("  for (int i = 0; i < key::kMax; ++i) {"));
        assert!(!rebinding.rebinds("  std::string key;"));
        assert!(!rebinding.rebinds("  key.clear();"));
    }
}

#[cfg(test)]
mod self_veto_tests {
    use super::*;
    use crate::types::ImportMapping;

    /// A context whose only node is the reference's own.
    struct OneNode(Node);

    impl ResolutionContext for OneNode {
        fn get_nodes_in_file(&self, _file_path: &str) -> Vec<Node> {
            vec![self.0.clone()]
        }
        fn get_nodes_by_name(&self, _name: &str) -> Vec<Node> {
            vec![self.0.clone()]
        }
        fn get_nodes_by_qualified_name(&self, _qualified_name: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_nodes_by_kind(&self, _kind: NodeKind) -> Vec<Node> {
            Vec::new()
        }
        fn file_exists(&self, _file_path: &str) -> bool {
            false
        }
        fn read_file(&self, _file_path: &str) -> Option<String> {
            None
        }
        fn get_project_root(&self) -> &str {
            ""
        }
        fn get_all_files(&self) -> Vec<String> {
            Vec::new()
        }
        fn get_nodes_by_lower_name(&self, _lower_name: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_node_by_id(&self, id: &str) -> Option<Node> {
            (id == self.0.id).then(|| self.0.clone())
        }
        fn get_import_mappings(&self, _file_path: &str, _language: Language) -> Vec<ImportMapping> {
            Vec::new()
        }
    }

    fn own_node(kind: NodeKind) -> Node {
        Node {
            id: format!("{}:00000000000000000000000000000001", kind.as_str()),
            kind,
            name: "response".to_string(),
            qualified_name: "CurlTest::response".to_string(),
            file_path: "src/Curl.scala".to_string(),
            language: Language::Scala,
            start_line: 2,
            end_line: 4,
            start_column: 0,
            end_column: 0,
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

    fn self_result(node: &Node) -> (RefView, Option<ResolvedRef>) {
        let reference = RefView {
            row_id: None,
            from_node_id: node.id.clone(),
            reference_name: "response".to_string(),
            reference_kind: EdgeKind::Calls,
            line: 4,
            column: 4,
            file_path: node.file_path.clone(),
            language: Language::Scala,
            is_function_ref: false,
            reference_subkind: None,
        };
        let result = ResolvedRef {
            original: reference.clone(),
            target_node_id: node.id.clone(),
            confidence: 0.9,
            resolved_by: ResolvedBy::ExactMatch,
        };
        (reference, Some(result))
    }

    /// Upstream #2223: sttp's `val response = basicRequest.get(…).response(…)`
    /// is a request's `response`, never the value calling itself; a method's
    /// call of its own name (recursion) keeps its edge.
    #[test]
    fn a_value_initializer_never_calls_the_value() {
        for kind in [
            NodeKind::Variable,
            NodeKind::Constant,
            NodeKind::Field,
            NodeKind::Property,
        ] {
            let node = own_node(kind);
            let (reference, result) = self_result(&node);
            assert_eq!(
                veto_self_target(result, &reference, &OneNode(node.clone())),
                None,
                "{kind:?}"
            );
        }
        let method = own_node(NodeKind::Method);
        let (reference, result) = self_result(&method);
        assert!(veto_self_target(result, &reference, &OneNode(method.clone())).is_some());
    }
}

#[cfg(test)]
mod ceiling_tests {
    use super::*;
    use crate::types::ImportMapping;
    use codegraph_core::types::ReferenceSubkind;

    struct FakeContext {
        by_name: Vec<Node>,
        by_lower: Vec<Node>,
    }

    impl ResolutionContext for FakeContext {
        fn get_nodes_in_file(&self, _file_path: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_nodes_by_name(&self, _name: &str) -> Vec<Node> {
            self.by_name.clone()
        }
        fn get_nodes_by_qualified_name(&self, _qualified_name: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_nodes_by_kind(&self, _kind: NodeKind) -> Vec<Node> {
            Vec::new()
        }
        fn file_exists(&self, _file_path: &str) -> bool {
            false
        }
        fn read_file(&self, _file_path: &str) -> Option<String> {
            None
        }
        fn get_project_root(&self) -> &str {
            ""
        }
        fn get_all_files(&self) -> Vec<String> {
            Vec::new()
        }
        fn get_nodes_by_lower_name(&self, _lower_name: &str) -> Vec<Node> {
            self.by_lower.clone()
        }
        fn get_node_by_id(&self, _id: &str) -> Option<Node> {
            None
        }
        fn get_import_mappings(&self, _file_path: &str, _language: Language) -> Vec<ImportMapping> {
            Vec::new()
        }
    }

    fn node(idx: usize) -> Node {
        Node {
            id: format!("function:{idx:032x}"),
            kind: NodeKind::Function,
            name: "doThing".to_string(),
            qualified_name: "doThing".to_string(),
            file_path: format!("src/f{idx}.ts"),
            language: Language::TypeScript,
            start_line: 1,
            end_line: 1,
            start_column: 0,
            end_column: 0,
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

    fn reference() -> RefView {
        RefView {
            row_id: None,
            from_node_id: "function:caller".to_string(),
            reference_name: "doThing".to_string(),
            reference_kind: EdgeKind::Calls,
            line: 10,
            column: 0,
            file_path: "src/caller.ts".to_string(),
            language: Language::TypeScript,
            is_function_ref: false,
            reference_subkind: None::<ReferenceSubkind>,
        }
    }

    fn nodes(count: usize) -> Vec<Node> {
        (0..count).map(node).collect()
    }

    /// Upstream #2139: the fuzzy fallback's lowercase index matches across
    /// case only in a case-insensitive language (PHP `formatprice` reaches
    /// `FormatPrice`); a TypeScript `Mock()` never reaches `mock`.
    #[test]
    fn fuzzy_ignores_case_only_in_a_case_insensitive_language() {
        let mut php = node(1);
        php.name = "FormatPrice".to_string();
        php.qualified_name = "FormatPrice".to_string();
        php.language = Language::Php;
        php.file_path = "src/helpers.php".to_string();
        let ctx = FakeContext {
            by_name: Vec::new(),
            by_lower: vec![php.clone()],
        };
        let mut call = reference();
        call.reference_name = "formatprice".to_string();
        call.language = Language::Php;
        call.file_path = "src/view.php".to_string();
        assert_eq!(
            match_fuzzy(&call, &ctx).map(|m| m.target_node_id),
            Some(php.id.clone())
        );

        let mut ts = node(2);
        ts.name = "mock".to_string();
        ts.qualified_name = "mock".to_string();
        let ctx = FakeContext {
            by_name: Vec::new(),
            by_lower: vec![ts],
        };
        let mut call = reference();
        call.reference_name = "Mock".to_string();
        assert_eq!(match_fuzzy(&call, &ctx), None);
    }

    /// Upstream #2141 (c): `new MockData()` never instantiates a method.
    #[test]
    fn a_fuzzy_instantiation_never_names_a_method() {
        let mut method = node(3);
        method.kind = NodeKind::Method;
        method.name = "MockData".to_string();
        method.qualified_name = "Store::MockData".to_string();
        let ctx = FakeContext {
            by_name: Vec::new(),
            by_lower: vec![method],
        };
        let mut new_call = reference();
        new_call.reference_name = "MockData".to_string();
        new_call.reference_kind = EdgeKind::Instantiates;
        assert_eq!(match_fuzzy(&new_call, &ctx), None);
    }

    #[test]
    fn exact_match_declines_above_ceiling_but_resolves_at_it() {
        // Given a candidate set just over the default ceiling, the multi-candidate
        // exact-match branch declines rather than proximity-rank it.
        let over = FakeContext {
            by_name: nodes(AMBIGUOUS_NAME_CEILING + 1),
            by_lower: Vec::new(),
        };
        assert!(match_by_exact_name(&reference(), &over).is_none());

        // Given exactly the ceiling, it still ranks and resolves a winner.
        let at = FakeContext {
            by_name: nodes(AMBIGUOUS_NAME_CEILING),
            by_lower: Vec::new(),
        };
        assert!(match_by_exact_name(&reference(), &at).is_some());
    }

    #[test]
    fn fuzzy_declines_above_ceiling() {
        // Given a fuzzy candidate set over the ceiling, match_fuzzy declines.
        let over = FakeContext {
            by_name: Vec::new(),
            by_lower: nodes(AMBIGUOUS_NAME_CEILING + 1),
        };
        assert!(match_fuzzy(&reference(), &over).is_none());

        // Given a single fuzzy candidate (well under), it resolves.
        let one = FakeContext {
            by_name: Vec::new(),
            by_lower: nodes(1),
        };
        assert!(match_fuzzy(&reference(), &one).is_some());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ImportMapping;
    use codegraph_core::types::ReferenceSubkind;
    use std::collections::HashMap;

    // ---- Configurable in-memory ResolutionContext ---------------------------

    /// A fully configurable fake context. Every lookup is backed by a map keyed
    /// by the exact query string, so a test can wire precise candidate sets for
    /// each named lookup the matcher performs.
    #[derive(Default)]
    struct Ctx {
        by_name: HashMap<String, Vec<Node>>,
        by_qualified: HashMap<String, Vec<Node>>,
        by_lower: HashMap<String, Vec<Node>>,
        in_file: HashMap<String, Vec<Node>>,
        by_id: HashMap<String, Node>,
        files: std::collections::HashSet<String>,
        file_content: HashMap<String, String>,
        supertypes: HashMap<(String, Language), Vec<String>>,
        imports: HashMap<String, Vec<ImportMapping>>,
    }

    impl Ctx {
        fn name(mut self, key: &str, ns: Vec<Node>) -> Self {
            self.by_name.insert(key.to_string(), ns);
            self
        }
        fn qualified(mut self, key: &str, ns: Vec<Node>) -> Self {
            self.by_qualified.insert(key.to_string(), ns);
            self
        }
        fn lower(mut self, key: &str, ns: Vec<Node>) -> Self {
            self.by_lower.insert(key.to_string(), ns);
            self
        }
        fn nodes_in_file(mut self, path: &str, ns: Vec<Node>) -> Self {
            self.in_file.insert(path.to_string(), ns);
            self
        }
        fn node_by_id(mut self, n: Node) -> Self {
            self.by_id.insert(n.id.clone(), n);
            self
        }
        fn file(mut self, path: &str, content: &str) -> Self {
            self.files.insert(path.to_string());
            self.file_content
                .insert(path.to_string(), content.to_string());
            self
        }
        fn supertype(mut self, ty: &str, lang: Language, sups: Vec<&str>) -> Self {
            self.supertypes.insert(
                (ty.to_string(), lang),
                sups.into_iter().map(str::to_string).collect(),
            );
            self
        }
        fn import(mut self, path: &str, mappings: Vec<ImportMapping>) -> Self {
            self.imports.insert(path.to_string(), mappings);
            self
        }
        /// Declare `type_name` imported into `path`, ADDING to any mappings the
        /// file already has. A real ESM file that names a type from another file
        /// carries this import statement; the mock reports none unless a test
        /// wires one, so a receiver-type fixture needs it to be BOUND at its call
        /// site (#1566).
        fn imports_type(mut self, path: &str, type_name: &str) -> Self {
            self.imports
                .entry(path.to_string())
                .or_default()
                .push(ImportMapping {
                    local_name: type_name.to_string(),
                    exported_name: type_name.to_string(),
                    source: "./dep".to_string(),
                    is_default: false,
                    is_namespace: false,
                });
            self
        }
    }

    impl ResolutionContext for Ctx {
        fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
            self.in_file.get(file_path).cloned().unwrap_or_default()
        }
        fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
            self.by_name.get(name).cloned().unwrap_or_default()
        }
        fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node> {
            self.by_qualified
                .get(qualified_name)
                .cloned()
                .unwrap_or_default()
        }
        fn get_nodes_by_kind(&self, _kind: NodeKind) -> Vec<Node> {
            Vec::new()
        }
        fn file_exists(&self, file_path: &str) -> bool {
            self.files.contains(file_path)
        }
        fn read_file(&self, file_path: &str) -> Option<String> {
            self.file_content.get(file_path).cloned()
        }
        fn get_project_root(&self) -> &str {
            ""
        }
        fn get_all_files(&self) -> Vec<String> {
            self.files.iter().cloned().collect()
        }
        fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node> {
            self.by_lower.get(lower_name).cloned().unwrap_or_default()
        }
        fn get_node_by_id(&self, id: &str) -> Option<Node> {
            self.by_id.get(id).cloned()
        }
        fn get_supertypes(&self, type_name: &str, language: Language) -> Vec<String> {
            self.supertypes
                .get(&(type_name.to_string(), language))
                .cloned()
                .unwrap_or_default()
        }
        fn get_import_mappings(&self, file_path: &str, _language: Language) -> Vec<ImportMapping> {
            self.imports.get(file_path).cloned().unwrap_or_default()
        }
    }

    // ---- Node / RefView builders --------------------------------------------

    fn mk(id: &str, kind: NodeKind, name: &str, qname: &str, path: &str, lang: Language) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: qname.to_string(),
            file_path: path.to_string(),
            language: lang,
            start_line: 1,
            end_line: 1,
            start_column: 0,
            end_column: 0,
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

    fn refv(name: &str, kind: EdgeKind, path: &str, lang: Language, line: i64) -> RefView {
        RefView {
            row_id: None,
            from_node_id: "function:caller".to_string(),
            reference_name: name.to_string(),
            reference_kind: kind,
            line,
            column: 0,
            file_path: path.to_string(),
            language: lang,
            is_function_ref: false,
            reference_subkind: None::<ReferenceSubkind>,
        }
    }

    // ================= language family helpers ================================

    #[test]
    fn language_families_group_correctly() {
        assert!(same_language_family(Language::Java, Language::Kotlin));
        assert!(same_language_family(Language::Kotlin, Language::Scala));
        assert!(same_language_family(Language::Swift, Language::ObjC));
        assert!(same_language_family(Language::TypeScript, Language::Jsx));
        assert!(same_language_family(Language::JavaScript, Language::Tsx));
        assert!(same_language_family(Language::C, Language::Cpp));
        // C, C++, Objective-C and Swift interoperate natively (v1.6.1).
        assert!(same_language_family(Language::Swift, Language::Cpp));
        assert!(same_language_family(Language::ArkTs, Language::TypeScript));
        assert!(same_language_family(Language::Vue, Language::TypeScript));
        assert!(same_language_family(Language::CSharp, Language::Razor));
        // Identity always matches even for a language with no family.
        assert!(same_language_family(Language::Rust, Language::Rust));
        // Different families do not match.
        assert!(!same_language_family(Language::Java, Language::Swift));
        // A language with no family (Rust) does not match a different one.
        assert!(!same_language_family(Language::Rust, Language::Go));
    }

    #[test]
    fn code_boundary_covers_singleton_families_and_leaves_config_open() {
        // Two different code families cross.
        assert!(crosses_code_boundary(Language::Java, Language::Swift));
        assert!(crosses_code_boundary(Language::Rust, Language::TypeScript));
        assert!(crosses_code_boundary(Language::Python, Language::Go));
        // One family (or one language) does not.
        assert!(!crosses_code_boundary(Language::Java, Language::Kotlin));
        assert!(!crosses_code_boundary(Language::Lua, Language::Luau));
        assert!(!crosses_code_boundary(Language::Rust, Language::Rust));
        // Config and markup transitions stay open.
        assert!(!crosses_code_boundary(Language::Yaml, Language::Php));
        assert!(!crosses_code_boundary(
            Language::TypeScript,
            Language::Liquid
        ));
    }

    #[test]
    fn cross_family_results_need_bridge_evidence() {
        let ts_method = mk(
            "method:Adapter::map",
            NodeKind::Method,
            "map",
            "Adapter::map",
            "ui/adapter.ts",
            Language::TypeScript,
        );
        let rust_call = refv("map", EdgeKind::Calls, "src/lib.rs", Language::Rust, 1);
        let ctx = Ctx::default().node_by_id(ts_method.clone());
        let chosen = || {
            Some(ResolvedRef {
                original: rust_call.clone(),
                target_node_id: ts_method.id.clone(),
                confidence: 0.5,
                resolved_by: ResolvedBy::ExactMatch,
            })
        };
        assert!(
            gate_language_match(chosen(), &rust_call, &ctx).is_none(),
            "`v.iter().map()` never reaches TS"
        );

        let exported = {
            let mut node = mk(
                "function:add",
                NodeKind::Function,
                "add",
                "add",
                "go/lib.go",
                Language::Go,
            );
            node.start_line = 4;
            node.end_line = 4;
            node
        };
        let c_call = refv("add", EdgeKind::Calls, "native/main.c", Language::C, 1);
        let cgo = Ctx::default()
            .node_by_id(exported.clone())
            .file("go/lib.go", "package main\nimport \"C\"\n//export add\nfunc add(a, b C.int) C.int { return a + b }\n");
        let via_cgo = Some(ResolvedRef {
            original: c_call.clone(),
            target_node_id: exported.id.clone(),
            confidence: 0.5,
            resolved_by: ResolvedBy::ExactMatch,
        });
        assert!(
            gate_language_match(via_cgo, &c_call, &cgo).is_some(),
            "a cgo //export is a C ABI bridge"
        );
    }

    #[test]
    fn language_gate_references_keeps_only_same_family() {
        // References edge: cross-family candidates are dropped.
        let same = mk(
            "function:a",
            NodeKind::Function,
            "f",
            "f",
            "src/a.ts",
            Language::TypeScript,
        );
        let cross = mk(
            "function:b",
            NodeKind::Function,
            "f",
            "f",
            "src/b.java",
            Language::Java,
        );
        let r = refv(
            "f",
            EdgeKind::References,
            "src/c.ts",
            Language::TypeScript,
            1,
        );
        let out = apply_language_gate(vec![same.clone(), cross], &r);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "function:a");
    }

    #[test]
    fn language_gate_other_kind_passes_through() {
        let a = mk(
            "function:a",
            NodeKind::Function,
            "f",
            "f",
            "src/a.java",
            Language::Java,
        );
        let b = mk(
            "function:b",
            NodeKind::Function,
            "f",
            "f",
            "src/b.ts",
            Language::TypeScript,
        );
        let r = refv("f", EdgeKind::Calls, "src/x.ts", Language::TypeScript, 1);
        let out = apply_language_gate(vec![a, b], &r);
        assert_eq!(out.len(), 2);
    }

    // ================= match_by_file_path =====================================

    #[test]
    fn file_path_bare_symbol_without_extension_declines() {
        // A bare ref without `/` and without a short extension is not a file ref.
        let ctx = Ctx::default();
        let r = refv(
            "Widget",
            EdgeKind::References,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_by_file_path(&r, &ctx).is_none());
    }

    #[test]
    fn file_path_no_matching_file_nodes_declines() {
        // Path-like ref but there are no File nodes with that basename.
        let ctx = Ctx::default().name("b.ts", vec![]);
        let r = refv(
            "a/b.ts",
            EdgeKind::References,
            "src/x.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_by_file_path(&r, &ctx).is_none());
    }

    #[test]
    fn file_path_exact_qualified_match_high_confidence() {
        let file = mk(
            "file:src/a/b.ts",
            NodeKind::File,
            "b.ts",
            "a/b.ts",
            "a/b.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("b.ts", vec![file]);
        let r = refv(
            "a/b.ts",
            EdgeKind::References,
            "src/x.ts",
            Language::TypeScript,
            1,
        );
        let res = match_by_file_path(&r, &ctx).expect("exact file match");
        assert_eq!(res.target_node_id, "file:src/a/b.ts");
        assert_eq!(res.resolved_by, ResolvedBy::FilePath);
        assert!((res.confidence - 0.95).abs() < 1e-9);
    }

    #[test]
    fn file_path_suffix_match_picks_closest_and_medium_confidence() {
        // Two file nodes end with the ref; the one in the same dir as the ref wins.
        let near = mk(
            "file:1",
            NodeKind::File,
            "util.ts",
            "app/util.ts",
            "app/util.ts",
            Language::TypeScript,
        );
        let far = mk(
            "file:2",
            NodeKind::File,
            "util.ts",
            "lib/util.ts",
            "lib/util.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("util.ts", vec![near, far]);
        let r = refv(
            "util.ts",
            EdgeKind::References,
            "app/main.ts",
            Language::TypeScript,
            1,
        );
        let res = match_by_file_path(&r, &ctx).expect("suffix match");
        assert_eq!(res.target_node_id, "file:1");
        assert!((res.confidence - 0.85).abs() < 1e-9);
    }

    #[test]
    fn file_path_single_same_named_file_low_confidence() {
        // Single file node whose path neither equals nor suffix-matches the ref.
        let f = mk(
            "file:only",
            NodeKind::File,
            "b.ts",
            "totally/other.ts",
            "totally/other.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("b.ts", vec![f]);
        let r = refv(
            "weird/b.ts",
            EdgeKind::References,
            "src/x.ts",
            Language::TypeScript,
            1,
        );
        let res = match_by_file_path(&r, &ctx).expect("single file fallback");
        assert_eq!(res.target_node_id, "file:only");
        assert!((res.confidence - 0.7).abs() < 1e-9);
    }

    #[test]
    fn file_path_multiple_non_matching_files_declines() {
        // >1 file node, none exact/suffix — no fallback (len != 1).
        let a = mk(
            "file:a",
            NodeKind::File,
            "b.ts",
            "one/other.ts",
            "one/other.ts",
            Language::TypeScript,
        );
        let b = mk(
            "file:b",
            NodeKind::File,
            "b.ts",
            "two/other.ts",
            "two/other.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("b.ts", vec![a, b]);
        let r = refv(
            "weird/b.ts",
            EdgeKind::References,
            "src/x.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_by_file_path(&r, &ctx).is_none());
    }

    #[test]
    fn file_path_short_extension_bare_filename_is_file_ref() {
        // `Foo.h` — bare filename with a short extension counts as a file ref.
        let f = mk(
            "file:h",
            NodeKind::File,
            "Foo.h",
            "inc/Foo.h",
            "inc/Foo.h",
            Language::Cpp,
        );
        let ctx = Ctx::default().name("Foo.h", vec![f]);
        let r = refv("Foo.h", EdgeKind::References, "src/x.cpp", Language::Cpp, 1);
        assert!(match_by_file_path(&r, &ctx).is_some());
    }

    #[test]
    fn has_short_extension_variants() {
        assert!(has_short_extension("Foo.h"));
        assert!(has_short_extension("a.ts"));
        // A 6-char extension is too long (max 4).
        assert!(!has_short_extension("x.liquid"));
        assert!(!has_short_extension("noext"));
        assert!(!has_short_extension("trailingdot."));
        // The extension must start with an alphabetic char.
        assert!(!has_short_extension("bad.123"));
        assert!(has_short_extension("f.cpp"));
        assert!(!has_short_extension("f.toolong"));
    }

    #[test]
    fn file_path_empty_basename_declines() {
        // A ref ending in `/` yields an empty basename.
        let ctx = Ctx::default();
        let r = refv(
            "a/b/",
            EdgeKind::References,
            "src/x.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_by_file_path(&r, &ctx).is_none());
    }

    // ================= match_by_exact_name ====================================

    #[test]
    fn exact_name_empty_candidates_declines() {
        let ctx = Ctx::default();
        let r = refv("Nope", EdgeKind::Calls, "src/a.ts", Language::TypeScript, 1);
        assert!(match_by_exact_name(&r, &ctx).is_none());
    }

    #[test]
    fn exact_name_single_same_language_high_confidence() {
        let n = mk(
            "function:x",
            NodeKind::Function,
            "run",
            "run",
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("run", vec![n]);
        let r = refv("run", EdgeKind::Calls, "src/b.ts", Language::TypeScript, 1);
        let res = match_by_exact_name(&r, &ctx).expect("single match");
        assert_eq!(res.resolved_by, ResolvedBy::ExactMatch);
        assert!((res.confidence - 0.9).abs() < 1e-9);
    }

    #[test]
    fn exact_name_single_cross_language_half_confidence() {
        // Single candidate but a different language within the same family (web)
        // — passes the gate for Calls (other kind) yet is cross-language.
        let n = mk(
            "function:x",
            NodeKind::Function,
            "run",
            "run",
            "src/a.jsx",
            Language::Jsx,
        );
        let ctx = Ctx::default().name("run", vec![n]);
        let r = refv("run", EdgeKind::Calls, "src/b.ts", Language::TypeScript, 1);
        let res = match_by_exact_name(&r, &ctx).expect("single cross match");
        assert!((res.confidence - 0.5).abs() < 1e-9);
    }

    #[test]
    fn exact_name_multi_candidate_ranks_same_file_high_confidence() {
        // Multiple candidates; the same-file one wins and shares >=2 leading dir
        // segments with the ref (proximity 2*15=30 >= 30) => confidence 0.7.
        let same = mk(
            "function:same",
            NodeKind::Function,
            "run",
            "run",
            "src/app/b.ts",
            Language::TypeScript,
        );
        let other = mk(
            "function:other",
            NodeKind::Function,
            "run",
            "run",
            "far/away/x.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("run", vec![same, other]);
        let r = refv(
            "run",
            EdgeKind::Calls,
            "src/app/b.ts",
            Language::TypeScript,
            5,
        );
        let res = match_by_exact_name(&r, &ctx).expect("multi match");
        assert_eq!(res.target_node_id, "function:same");
        assert!((res.confidence - 0.7).abs() < 1e-9);
    }

    #[test]
    fn exact_name_multi_candidate_low_proximity_low_confidence() {
        // Multiple candidates all far away => best proximity < 30 => 0.4.
        let a = mk(
            "function:a",
            NodeKind::Function,
            "run",
            "run",
            "aaa/bbb/x.ts",
            Language::TypeScript,
        );
        let b = mk(
            "function:b",
            NodeKind::Function,
            "run",
            "run",
            "ccc/ddd/y.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("run", vec![a, b]);
        let r = refv(
            "run",
            EdgeKind::Calls,
            "zzz/main.ts",
            Language::TypeScript,
            1,
        );
        let res = match_by_exact_name(&r, &ctx).expect("multi far match");
        assert!((res.confidence - 0.4).abs() < 1e-9);
    }

    // ================= match_by_qualified_name ================================

    #[test]
    fn qualified_name_requires_separator() {
        let ctx = Ctx::default();
        let r = refv(
            "bareName",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_by_qualified_name(&r, &ctx).is_none());
    }

    #[test]
    fn qualified_name_single_exact_match() {
        let n = mk(
            "method:m",
            NodeKind::Method,
            "get",
            "Foo::get",
            "src/a.rs",
            Language::Rust,
        );
        let ctx = Ctx::default().qualified("Foo::get", vec![n]);
        let r = refv("Foo::get", EdgeKind::Calls, "src/b.rs", Language::Rust, 1);
        let res = match_by_qualified_name(&r, &ctx).expect("qualified exact");
        assert_eq!(res.resolved_by, ResolvedBy::QualifiedName);
        assert!((res.confidence - 0.95).abs() < 1e-9);
    }

    #[test]
    fn qualified_name_partial_suffix_match() {
        // No exact qualified hit, but the last segment's node ends with the ref.
        let n = mk(
            "method:m",
            NodeKind::Method,
            "get",
            "pkg::Foo::get",
            "src/a.rs",
            Language::Rust,
        );
        let ctx = Ctx::default()
            .qualified("Foo::get", vec![])
            .name("get", vec![n]);
        let r = refv("Foo::get", EdgeKind::Calls, "src/b.rs", Language::Rust, 1);
        let res = match_by_qualified_name(&r, &ctx).expect("partial qualified");
        assert!((res.confidence - 0.85).abs() < 1e-9);
    }

    #[test]
    fn qualified_name_dotted_no_match_declines() {
        let ctx = Ctx::default();
        let r = refv(
            "a.b.c",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_by_qualified_name(&r, &ctx).is_none());
    }

    #[test]
    fn erlang_qualified_name_requires_exact_written_arity() {
        let f1 = mk(
            "function:f1",
            NodeKind::Function,
            "f",
            "mod::f/1",
            "src/mod.erl",
            Language::Erlang,
        );
        let f2 = mk(
            "function:f2",
            NodeKind::Function,
            "f",
            "mod::f/2",
            "src/mod.erl",
            Language::Erlang,
        );
        let ctx = Ctx::default()
            .qualified("mod::f/2", vec![f2.clone()])
            .name("f", vec![f1, f2]);
        let hit = refv(
            "mod::f/2",
            EdgeKind::Calls,
            "src/client.erl",
            Language::Erlang,
            1,
        );
        assert_eq!(
            match_by_qualified_name(&hit, &ctx)
                .expect("exact arity")
                .target_node_id,
            "function:f2"
        );
        let miss = refv(
            "mod::f/3",
            EdgeKind::Calls,
            "src/client.erl",
            Language::Erlang,
            1,
        );
        assert!(match_by_qualified_name(&miss, &ctx).is_none());
    }

    #[test]
    fn erlang_arityless_qualified_name_requires_one_arity() {
        let single = mk(
            "function:work",
            NodeKind::Function,
            "work",
            "single::work/1",
            "src/single.erl",
            Language::Erlang,
        );
        let unique = Ctx::default().name("work", vec![single]);
        let reference = refv(
            "single::work",
            EdgeKind::Calls,
            "src/spawner.erl",
            Language::Erlang,
            1,
        );
        assert_eq!(
            match_by_qualified_name(&reference, &unique)
                .expect("unique arity")
                .target_node_id,
            "function:work"
        );

        let job1 = mk(
            "function:job1",
            NodeKind::Function,
            "job",
            "multi::job/1",
            "src/multi.erl",
            Language::Erlang,
        );
        let job2 = mk(
            "function:job2",
            NodeKind::Function,
            "job",
            "multi::job/2",
            "src/multi.erl",
            Language::Erlang,
        );
        let ambiguous = Ctx::default().name("job", vec![job1, job2]);
        let reference = refv(
            "multi::job",
            EdgeKind::Calls,
            "src/spawner.erl",
            Language::Erlang,
            1,
        );
        assert!(match_by_qualified_name(&reference, &ambiguous).is_none());
    }

    // ================= parse helpers ==========================================

    #[test]
    fn dot_and_colon_call_parsing() {
        assert_eq!(
            match_dot_call("obj.method"),
            Some(("obj".to_string(), "method".to_string()))
        );
        assert_eq!(
            match_dot_call("a.b.method"),
            Some(("a.b".to_string(), "method".to_string()))
        );
        // ObjC selector with trailing colon.
        assert_eq!(
            match_dot_call("obj.doThis:"),
            Some(("obj".to_string(), "doThis:".to_string()))
        );
        // Leading dot => no receiver.
        assert_eq!(match_dot_call(".method"), None);
        // No dot at all.
        assert_eq!(match_dot_call("method"), None);
        // Receiver with an illegal char.
        assert_eq!(match_dot_call("ob-j.method"), None);

        assert_eq!(
            match_colon_call("Foo::bar"),
            Some(("Foo".to_string(), "bar".to_string()))
        );
        assert_eq!(match_colon_call("::bar"), None);
        assert_eq!(match_colon_call("Foo::"), None);
        assert_eq!(match_colon_call("nocolon"), None);
    }

    #[test]
    fn selector_like_and_word_predicates() {
        assert!(is_selector_like("do"));
        assert!(is_selector_like("do:"));
        assert!(is_selector_like("do:with:"));
        assert!(!is_selector_like(""));
        assert!(!is_selector_like(":leading"));
        assert!(is_word("abc_1"));
        assert!(!is_word(""));
        assert!(!is_word("a b"));
    }

    #[test]
    fn js_builtin_methods_are_sorted_for_binary_search() {
        assert!(JS_BUILTIN_METHODS.windows(2).all(|pair| pair[0] < pair[1]));
        for name in ["map", "get", "then", "addEventListener", "return", "with"] {
            assert!(is_js_builtin_method(name), "{name}");
        }
        for name in ["send", "run", "Map", "mapAll"] {
            assert!(!is_js_builtin_method(name), "{name}");
        }
    }

    #[test]
    fn private_this_field_calls_split_only_the_exact_shape() {
        assert_eq!(
            split_private_this_field_call("this.#items.add"),
            Some(("#items", "add"))
        );
        assert_eq!(
            split_private_this_field_call("this.#$store.get"),
            Some(("#$store", "get"))
        );
        for name in [
            "this.items.add",
            "this.#.add",
            "this.#items",
            "this.#a.b.c",
            "that.#items.add",
            "this.#items.a-b",
        ] {
            assert_eq!(split_private_this_field_call(name), None, "{name}");
        }
    }

    #[test]
    fn ts_field_declarations_keep_public_and_private_names_apart() {
        let declared = |line: &str, field: &str, this_only: bool| {
            ts_field_on_line(line, field, this_only)
                .map(|found| (found.value_type, found.type_name))
        };
        assert_eq!(
            declared("  #items = new Set<string>();", "#items", false),
            Some((false, "Set".to_string()))
        );
        assert_eq!(declared("  #items = new Set();", "items", false), None);
        assert_eq!(declared("  items = new Cart();", "#items", false), None);
        assert_eq!(
            declared("  private readonly mailer?: ns.Mailer;", "mailer", false),
            Some((false, "ns.Mailer".to_string()))
        );
        assert_eq!(
            declared("  storage: typeof DraftHubStorage;", "storage", false),
            Some((true, "DraftHubStorage".to_string()))
        );
        assert_eq!(declared("  mailerX: Decoy;", "mailer", false), None);
        assert_eq!(
            declared("  users$: Observable<User>;", "users$", false),
            Some((false, "Observable".to_string()))
        );
        // Inside a method body only a `this.<field>` assignment counts.
        assert_eq!(
            declared(
                "  constructor() { this.mailer = new Mailer(); }",
                "mailer",
                true
            ),
            Some((false, "Mailer".to_string()))
        );
        assert_eq!(
            declared("  run() { const mailer = new Decoy(); }", "mailer", true),
            None
        );
        assert_eq!(
            declared("  run() { xthis.mailer = new Decoy(); }", "mailer", true),
            None
        );
    }

    #[test]
    fn parameter_properties_need_a_modifier_before_the_exact_name() {
        let line = "  constructor(private readonly mailer: Mailer, other: X, public items: Y) {}";
        assert!(declares_parameter_property(line, "mailer"));
        assert!(declares_parameter_property(line, "items"));
        assert!(!declares_parameter_property(line, "other"));
        assert!(!declares_parameter_property(line, "mail"));
        assert!(!declares_parameter_property(line, "#mailer"));
    }

    #[test]
    fn parse_method_call_prefers_dot_then_colon() {
        assert_eq!(
            parse_method_call("obj.m", Language::TypeScript),
            Some(("obj".to_string(), "m".to_string()))
        );
        assert_eq!(
            parse_method_call("Foo::m", Language::TypeScript),
            Some(("Foo".to_string(), "m".to_string()))
        );
        assert_eq!(parse_method_call("plain", Language::TypeScript), None);
        // Lua colon separator only recognized for Lua/Luau.
        assert_eq!(
            parse_method_call("lg:log", Language::Lua),
            Some(("lg".to_string(), "log".to_string()))
        );
        assert_eq!(parse_method_call("lg:log", Language::Rust), None);
        // R dollar separator only recognized for R.
        assert_eq!(
            parse_method_call("lg$log", Language::R),
            Some(("lg".to_string(), "log".to_string()))
        );
        assert_eq!(parse_method_call("lg$log", Language::TypeScript), None);
    }

    #[test]
    fn is_dot_call_gate() {
        assert!(is_dot_call("obj.method"));
        assert!(!is_dot_call("Foo::method"));
        assert!(!is_dot_call("plain"));
    }

    #[test]
    fn capitalize_and_camel_split() {
        assert_eq!(capitalize("word"), "Word");
        assert_eq!(capitalize(""), "");
        assert_eq!(capitalize("Already"), "Already");
        let words = split_camel_case("HTTPServerConfig");
        assert!(words.iter().any(|w| w == "HTTP"));
        assert!(words.iter().any(|w| w == "Server"));
        assert!(words.iter().any(|w| w == "Config"));
        // single-char words are dropped
        let ws = split_camel_case("aB");
        assert!(ws.is_empty() || ws.iter().all(|w| w.chars().count() > 1));
    }

    // ================= match_method_call ======================================

    #[test]
    fn method_call_declines_when_not_a_call_shape() {
        let ctx = Ctx::default();
        let r = refv(
            "plainname",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_method_call(&r, &ctx).is_none());
    }

    #[test]
    fn rust_field_type_name_unwraps_only_auto_deref_layers() {
        for (raw, expected) in [
            ("Inner", Some("Inner")),
            ("&Inner", Some("Inner")),
            ("&'a mut Inner", Some("Inner")),
            ("Box<Inner>", Some("Inner")),
            ("Rc<Inner>", Some("Inner")),
            ("std::sync::Arc<Box<crate::inner::Inner>>", Some("Inner")),
            ("Box<dyn Source + Send>", Some("Source")),
            ("crate::inner::Inner", Some("Inner")),
            ("std::vec::IntoIter<u8>", None),
            ("regex::Regex", None),
            ("Option<Inner>", None),
            ("Vec<Inner>", None),
            ("Mutex<Inner>", None),
            ("T", None),
            ("u32", None),
            ("(Inner, Other)", None),
            ("*const Inner", None),
            ("[Inner; 4]", None),
        ] {
            assert_eq!(rust_field_type_name(raw).as_deref(), expected, "{raw}");
        }
    }

    #[test]
    fn rust_self_field_call_resolves_on_the_declared_type() {
        let source = "pub struct Outer {\n    pub(crate) inner: Box<Inner>,\n}\n";
        let mut owner = mk(
            "struct:outer",
            NodeKind::Struct,
            "Outer",
            "Outer",
            "src/outer.rs",
            Language::Rust,
        );
        owner.start_line = 1;
        owner.end_line = 3;
        let caller = mk(
            "method:outer-go",
            NodeKind::Method,
            "go",
            "Outer::go",
            "src/outer.rs",
            Language::Rust,
        );
        let inner = mk(
            "struct:inner",
            NodeKind::Struct,
            "Inner",
            "Inner",
            "src/inner.rs",
            Language::Rust,
        );
        let target = mk(
            "method:inner-run",
            NodeKind::Method,
            "run",
            "Inner::run",
            "src/inner.rs",
            Language::Rust,
        );
        let ctx = Ctx::default()
            .file("src/outer.rs", source)
            .node_by_id(caller)
            .name("Outer", vec![owner])
            .name("Inner", vec![inner])
            .name("run", vec![target]);
        let mut reference = refv(
            "self.inner.run",
            EdgeKind::Calls,
            "src/outer.rs",
            Language::Rust,
            6,
        );
        reference.from_node_id = "method:outer-go".to_string();

        let resolved = match_method_call(&reference, &ctx).expect("typed field call");
        assert_eq!(resolved.target_node_id, "method:inner-run");
        assert_eq!(resolved.resolved_by, ResolvedBy::InstanceMethod);
        assert!((resolved.confidence - 0.85).abs() < 1e-9);
    }

    #[test]
    fn arkts_import_only_module_is_sealed_across_files() {
        let candidate = mk(
            "function:hidden",
            NodeKind::Function,
            "hidden",
            "hidden",
            "src/private.ets",
            Language::ArkTs,
        );
        let reference = refv(
            "hidden",
            EdgeKind::Calls,
            "src/consumer.ets",
            Language::ArkTs,
            1,
        );
        let context = Ctx::default().file(
            "src/private.ets",
            "import { fs } from '@ohos.file.fs';\nfunction hidden() { return fs; }\n",
        );

        assert!(
            !is_js_name_target_visible(&candidate, &reference, &context),
            "ArkTS shares the ESM sealed-module boundary"
        );
    }

    #[test]
    fn rust_self_call_uses_the_callers_impl_owner_and_declines_uncertainty() {
        let caller = mk(
            "method:target-run",
            NodeKind::Method,
            "run",
            "Target::run",
            "src/target.rs",
            Language::Rust,
        );
        let target = mk(
            "method:target-reset",
            NodeKind::Method,
            "reset",
            "Target::reset",
            "src/target.rs",
            Language::Rust,
        );
        let decoy = mk(
            "method:decoy-reset",
            NodeKind::Method,
            "reset",
            "Decoy::reset",
            "src/target.rs",
            Language::Rust,
        );
        let unrelated_missing = mk(
            "method:other-missing",
            NodeKind::Method,
            "missing",
            "Other::missing",
            "src/other.rs",
            Language::Rust,
        );
        let ctx = Ctx::default()
            .node_by_id(caller)
            .name("reset", vec![decoy, target])
            .name("missing", vec![unrelated_missing]);
        let mut reference = refv(
            "self.reset",
            EdgeKind::Calls,
            "src/target.rs",
            Language::Rust,
            40,
        );
        reference.from_node_id = "method:target-run".to_string();

        let resolved = match_method_call(&reference, &ctx).expect("owner-qualified method");
        assert_eq!(resolved.target_node_id, "method:target-reset");
        assert_eq!(resolved.resolved_by, ResolvedBy::QualifiedName);
        assert!((resolved.confidence - 0.9).abs() < f64::EPSILON);

        reference.reference_name = "self.missing".to_string();
        assert!(
            match_method_call(&reference, &ctx).is_none(),
            "a method absent from the enclosing type must not bind to another type"
        );

        let duplicate = mk(
            "method:target-reset-duplicate",
            NodeKind::Method,
            "reset",
            "Target::reset",
            "src/target.rs",
            Language::Rust,
        );
        let ambiguous = Ctx::default()
            .node_by_id(mk(
                "method:target-run",
                NodeKind::Method,
                "run",
                "Target::run",
                "src/target.rs",
                Language::Rust,
            ))
            .name(
                "reset",
                vec![
                    mk(
                        "method:target-reset",
                        NodeKind::Method,
                        "reset",
                        "Target::reset",
                        "src/target.rs",
                        Language::Rust,
                    ),
                    duplicate,
                ],
            );
        reference.reference_name = "self.reset".to_string();
        assert!(
            match_method_call(&reference, &ambiguous).is_none(),
            "two owner-qualified candidates must remain unresolved"
        );
    }

    #[test]
    fn rust_self_field_call_does_not_guess_external_or_container_types() {
        for (field_decl, field_name, method_name, type_name) in [
            ("its: std::vec::IntoIter<u8>,", "its", "next", "IntoIter"),
            ("inner: Option<Inner>,", "inner", "take", "Inner"),
            ("items: T,", "items", "run", "T"),
        ] {
            let source = format!("pub struct Outer<T> {{\n    {field_decl}\n}}\n");
            let mut owner = mk(
                "struct:outer",
                NodeKind::Struct,
                "Outer",
                "Outer",
                "src/outer.rs",
                Language::Rust,
            );
            owner.start_line = 1;
            owner.end_line = 3;
            let caller = mk(
                "method:outer-go",
                NodeKind::Method,
                "go",
                "Outer::go",
                "src/outer.rs",
                Language::Rust,
            );
            let decoy_type = mk(
                "struct:decoy",
                NodeKind::Struct,
                type_name,
                type_name,
                "src/decoy.rs",
                Language::Rust,
            );
            let decoy_method = mk(
                "method:decoy",
                NodeKind::Method,
                method_name,
                &format!("{type_name}::{method_name}"),
                "src/decoy.rs",
                Language::Rust,
            );
            let ctx = Ctx::default()
                .file("src/outer.rs", &source)
                .node_by_id(caller)
                .name("Outer", vec![owner])
                .name(type_name, vec![decoy_type])
                .name(method_name, vec![decoy_method]);
            let mut reference = refv(
                &format!("self.{field_name}.{method_name}"),
                EdgeKind::Calls,
                "src/outer.rs",
                Language::Rust,
                6,
            );
            reference.from_node_id = "method:outer-go".to_string();

            assert!(
                match_method_call(&reference, &ctx).is_none(),
                "{field_decl} must stay unresolved"
            );
        }
    }

    #[test]
    fn rust_self_field_call_rejects_ambiguous_project_type() {
        let source = "pub struct Outer {\n    inner: Inner,\n}\n";
        let mut owner = mk(
            "struct:outer",
            NodeKind::Struct,
            "Outer",
            "Outer",
            "src/outer.rs",
            Language::Rust,
        );
        owner.start_line = 1;
        owner.end_line = 3;
        let caller = mk(
            "method:outer-go",
            NodeKind::Method,
            "go",
            "Outer::go",
            "src/outer.rs",
            Language::Rust,
        );
        let first = mk(
            "struct:first",
            NodeKind::Struct,
            "Inner",
            "Inner",
            "src/a.rs",
            Language::Rust,
        );
        let second = mk(
            "struct:second",
            NodeKind::Struct,
            "Inner",
            "Inner",
            "src/b.rs",
            Language::Rust,
        );
        let target = mk(
            "method:inner-run",
            NodeKind::Method,
            "run",
            "Inner::run",
            "src/a.rs",
            Language::Rust,
        );
        let ctx = Ctx::default()
            .file("src/outer.rs", source)
            .node_by_id(caller)
            .name("Outer", vec![owner])
            .name("Inner", vec![first, second])
            .name("run", vec![target]);
        let mut reference = refv(
            "self.inner.run",
            EdgeKind::Calls,
            "src/outer.rs",
            Language::Rust,
            6,
        );
        reference.from_node_id = "method:outer-go".to_string();

        assert!(match_method_call(&reference, &ctx).is_none());
    }

    #[test]
    fn object_literal_member_resolves_by_direct_containment() {
        let mut api = mk(
            "constant:api",
            NodeKind::Constant,
            "api",
            "api",
            "src/api.ts",
            Language::TypeScript,
        );
        api.start_line = 1;
        api.end_line = 8;
        let mut direct = mk(
            "function:direct-call",
            NodeKind::Function,
            "call",
            "call",
            "src/api.ts",
            Language::TypeScript,
        );
        direct.start_line = 2;
        direct.end_line = 2;
        let mut get = mk(
            "function:get",
            NodeKind::Function,
            "get",
            "get",
            "src/api.ts",
            Language::TypeScript,
        );
        get.start_line = 3;
        get.end_line = 6;
        let mut nested = mk(
            "function:nested-call",
            NodeKind::Function,
            "call",
            "call",
            "src/api.ts",
            Language::TypeScript,
        );
        nested.start_line = 4;
        nested.end_line = 4;
        let mut outside = mk(
            "function:outside-call",
            NodeKind::Function,
            "call",
            "call",
            "src/api.ts",
            Language::TypeScript,
        );
        outside.start_line = 10;
        outside.end_line = 10;
        let ctx = Ctx::default()
            .name("api", vec![api.clone()])
            .nodes_in_file("src/api.ts", vec![api, direct, get, nested, outside]);
        let reference = refv(
            "api.call",
            EdgeKind::Calls,
            "src/api.ts",
            Language::TypeScript,
            12,
        );

        let resolved = match_method_call(&reference, &ctx).expect("literal member");
        assert_eq!(resolved.target_node_id, "function:direct-call");
        assert_eq!(resolved.resolved_by, ResolvedBy::InstanceMethod);
        assert!((resolved.confidence - 0.85).abs() < 1e-9);
    }

    #[test]
    fn object_literal_member_rejects_nested_outside_and_ambiguous_candidates() {
        let mut api = mk(
            "constant:api",
            NodeKind::Constant,
            "api",
            "api",
            "src/api.ts",
            Language::TypeScript,
        );
        api.start_line = 1;
        api.end_line = 8;
        let mut get = mk(
            "function:get",
            NodeKind::Function,
            "get",
            "get",
            "src/api.ts",
            Language::TypeScript,
        );
        get.start_line = 2;
        get.end_line = 5;
        let mut nested = mk(
            "function:nested-call",
            NodeKind::Function,
            "call",
            "call",
            "src/api.ts",
            Language::TypeScript,
        );
        nested.start_line = 3;
        nested.end_line = 3;
        let mut outside = mk(
            "function:outside-call",
            NodeKind::Function,
            "call",
            "call",
            "src/api.ts",
            Language::TypeScript,
        );
        outside.start_line = 10;
        outside.end_line = 10;
        let nested_ctx = Ctx::default()
            .name("api", vec![api.clone()])
            .nodes_in_file("src/api.ts", vec![api.clone(), get, nested, outside]);
        let reference = refv(
            "api.call",
            EdgeKind::Calls,
            "src/api.ts",
            Language::TypeScript,
            12,
        );
        assert!(match_method_call(&reference, &nested_ctx).is_none());

        let mut first = mk(
            "function:first",
            NodeKind::Function,
            "call",
            "call",
            "src/api.ts",
            Language::TypeScript,
        );
        first.start_line = 2;
        first.end_line = 2;
        let mut second = mk(
            "function:second",
            NodeKind::Function,
            "call",
            "call",
            "src/api.ts",
            Language::TypeScript,
        );
        second.start_line = 6;
        second.end_line = 6;
        let ambiguous_ctx = Ctx::default()
            .name("api", vec![api.clone()])
            .nodes_in_file("src/api.ts", vec![api, first, second]);
        assert!(match_method_call(&reference, &ambiguous_ctx).is_none());
    }

    #[test]
    fn method_call_strategy1_direct_class_match() {
        // `Widget.render` — Widget is a class in the graph; render is its method.
        let class = mk(
            "class:w",
            NodeKind::Class,
            "Widget",
            "Widget",
            "src/w.ts",
            Language::TypeScript,
        );
        let method = mk(
            "method:r",
            NodeKind::Method,
            "render",
            "Widget::render",
            "src/w.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default()
            .name("Widget", vec![class])
            .nodes_in_file("src/w.ts", vec![method]);
        let r = refv(
            "Widget.render",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        let res = match_method_call(&r, &ctx).expect("strategy1");
        assert_eq!(res.target_node_id, "method:r");
        assert_eq!(res.resolved_by, ResolvedBy::QualifiedName);
        assert!((res.confidence - 0.85).abs() < 1e-9);
    }

    #[test]
    fn method_call_strategy2_capitalized_receiver() {
        // `widget.render` — lowercase instance var, capitalized class Widget.
        let class = mk(
            "class:w",
            NodeKind::Class,
            "Widget",
            "Widget",
            "src/w.ts",
            Language::TypeScript,
        );
        let method = mk(
            "method:r",
            NodeKind::Method,
            "render",
            "Widget::render",
            "src/w.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default()
            .name("widget", vec![]) // no direct class for the lowercase receiver
            .name("Widget", vec![class])
            .nodes_in_file("src/w.ts", vec![method]);
        let r = refv(
            "widget.render",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        let res = match_method_call(&r, &ctx).expect("strategy2");
        assert_eq!(res.target_node_id, "method:r");
        assert_eq!(res.resolved_by, ResolvedBy::InstanceMethod);
        assert!((res.confidence - 0.8).abs() < 1e-9);
    }

    #[test]
    fn method_call_strategy3_single_method_by_name() {
        // No class match; exactly one same-language method named `render`. The
        // receiver is `thing`, which shares a word with `Thing::render` — a
        // receiver naming nothing about the candidate is refused instead, which
        // `method_call_strategy3_single_candidate_refuses_unrelated_receiver`
        // covers.
        let method = mk(
            "method:r",
            NodeKind::Method,
            "render",
            "Thing::render",
            "src/w.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("render", vec![method]);
        let r = refv(
            "thing.render",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        let res = match_method_call(&r, &ctx).expect("strategy3 single");
        assert_eq!(res.target_node_id, "method:r");
        assert!((res.confidence - 0.7).abs() < 1e-9);
    }

    #[test]
    fn method_call_strategy3_single_candidate_refuses_unrelated_receiver() {
        // The unit-level twin of the field report: `out.push(...)` is
        // `Array.push`, and a lone project `DialectGate::push` is not evidence
        // that `out` is a `DialectGate`. Refuse rather than guess.
        let method = mk(
            "method:push",
            NodeKind::Method,
            "push",
            "DialectGate::push",
            "src/streaming/dialect-gate.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("push", vec![method]);
        let r = refv(
            "out.push",
            EdgeKind::Calls,
            "src/tool-call-parser.ts",
            Language::TypeScript,
            6,
        );
        assert!(
            match_method_call(&r, &ctx).is_none(),
            "a receiver sharing no word with the candidate's owner is not evidence"
        );
    }

    #[test]
    fn receiver_shares_word_drops_single_char_words() {
        // `split_camel_case` drops single-character words, so a one-letter
        // receiver can never overlap anything. Pinned because two fixture
        // retargets (`real`, `logger`) depend on it, and a future reader
        // "simplifying" a receiver back to one letter would silently delete
        // their coverage.
        assert!(!receiver_shares_word("r", "R::x"));
        assert!(!receiver_shares_word("r", "Real::poke"));
        assert!(receiver_shares_word("real", "Real::poke"));
        assert_eq!(receiver_word_overlap("r", "R::x"), 0);
    }

    #[test]
    fn method_call_strategy3_receiver_overlap_scoring() {
        // Multiple `save` methods; receiver `UserRepository` overlaps
        // `UserRepository::save` on >=2 words => picked at 0.65.
        let good = mk(
            "method:good",
            NodeKind::Method,
            "save",
            "UserRepository::save",
            "src/u.ts",
            Language::TypeScript,
        );
        let bad = mk(
            "method:bad",
            NodeKind::Method,
            "save",
            "OrderService::save",
            "src/o.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("save", vec![good, bad]);
        let r = refv(
            "userRepository.save",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        let res = match_method_call(&r, &ctx).expect("overlap scoring");
        assert_eq!(res.target_node_id, "method:good");
        assert!((res.confidence - 0.65).abs() < 1e-9);
    }

    #[test]
    fn method_call_strategy3_no_overlap_declines() {
        // Multiple methods, receiver overlaps <2 words => declines.
        let a = mk(
            "method:a",
            NodeKind::Method,
            "go",
            "Alpha::go",
            "src/a.ts",
            Language::TypeScript,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "go",
            "Beta::go",
            "src/b.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("go", vec![a, b]);
        let r = refv(
            "zeta.go",
            EdgeKind::Calls,
            "src/x.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_method_call(&r, &ctx).is_none());
    }

    // ================= match_fuzzy ============================================

    #[test]
    fn fuzzy_single_same_language() {
        let n = mk(
            "function:x",
            NodeKind::Function,
            "run",
            "run",
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().lower("run", vec![n]);
        let r = refv("run", EdgeKind::Calls, "src/b.ts", Language::TypeScript, 1);
        let res = match_fuzzy(&r, &ctx).expect("fuzzy single");
        assert_eq!(res.resolved_by, ResolvedBy::Fuzzy);
        assert!((res.confidence - 0.5).abs() < 1e-9);
    }

    #[test]
    fn fuzzy_single_cross_language_lower_confidence() {
        // Only a cross-language (same family) callable => confidence 0.3.
        let n = mk(
            "function:x",
            NodeKind::Function,
            "run",
            "run",
            "src/a.jsx",
            Language::Jsx,
        );
        let ctx = Ctx::default().lower("run", vec![n]);
        let r = refv("run", EdgeKind::Calls, "src/b.ts", Language::TypeScript, 1);
        let res = match_fuzzy(&r, &ctx).expect("fuzzy cross");
        assert!((res.confidence - 0.3).abs() < 1e-9);
    }

    #[test]
    fn fuzzy_filters_non_callable_kinds() {
        // A single Variable candidate is not callable => declines.
        let n = mk(
            "variable:x",
            NodeKind::Variable,
            "Run",
            "Run",
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().lower("run", vec![n]);
        let r = refv("run", EdgeKind::Calls, "src/b.ts", Language::TypeScript, 1);
        assert!(match_fuzzy(&r, &ctx).is_none());
    }

    #[test]
    fn fuzzy_multiple_candidates_declines() {
        let a = mk(
            "function:a",
            NodeKind::Function,
            "Run",
            "Run",
            "src/a.ts",
            Language::TypeScript,
        );
        let b = mk(
            "function:b",
            NodeKind::Function,
            "Run",
            "Run",
            "src/b.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().lower("run", vec![a, b]);
        let r = refv("run", EdgeKind::Calls, "src/c.ts", Language::TypeScript, 1);
        assert!(match_fuzzy(&r, &ctx).is_none());
    }

    // ================= find_best_match scoring ================================

    #[test]
    fn best_match_scoring_favors_calls_kind_and_export() {
        // Two same-file candidates: a Function (calls kind +25, exported +10)
        // should outrank a Variable of the same name.
        let func = {
            let mut n = mk(
                "function:f",
                NodeKind::Function,
                "x",
                "x",
                "src/a.ts",
                Language::TypeScript,
            );
            n.is_exported = true;
            n.start_line = 10;
            n
        };
        let var = mk(
            "variable:v",
            NodeKind::Variable,
            "x",
            "x",
            "src/a.ts",
            Language::TypeScript,
        );
        let r = refv("x", EdgeKind::Calls, "src/a.ts", Language::TypeScript, 10);
        let pool = [var, func];
        let best = find_best_match(&r, &pool).expect("best");
        assert_eq!(best.id, "function:f");
    }

    #[test]
    fn best_match_instantiates_prefers_class() {
        let class = mk(
            "class:c",
            NodeKind::Class,
            "T",
            "T",
            "far/a.ts",
            Language::TypeScript,
        );
        let func = mk(
            "function:f",
            NodeKind::Function,
            "T",
            "T",
            "far/b.ts",
            Language::TypeScript,
        );
        let r = refv(
            "T",
            EdgeKind::Instantiates,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        let pool = [func, class];
        let best = find_best_match(&r, &pool).expect("best");
        assert_eq!(best.id, "class:c");
    }

    #[test]
    fn best_match_decorates_prefers_function_over_class() {
        let func = mk(
            "function:f",
            NodeKind::Function,
            "D",
            "D",
            "far/a.ts",
            Language::TypeScript,
        );
        let class = mk(
            "class:c",
            NodeKind::Class,
            "D",
            "D",
            "far/b.ts",
            Language::TypeScript,
        );
        let r = refv(
            "D",
            EdgeKind::Decorates,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        let pool = [class, func];
        let best = find_best_match(&r, &pool).expect("best");
        assert_eq!(best.id, "function:f");
    }

    #[test]
    fn best_match_penalizes_cross_language() {
        // Same name; the same-language candidate must win over a cross-language one.
        let same = mk(
            "function:s",
            NodeKind::Function,
            "x",
            "x",
            "far/a.ts",
            Language::TypeScript,
        );
        let cross = mk(
            "function:c",
            NodeKind::Function,
            "x",
            "x",
            "far/b.java",
            Language::Java,
        );
        let r = refv("x", EdgeKind::Calls, "src/a.ts", Language::TypeScript, 1);
        let pool = [cross, same];
        let best = find_best_match(&r, &pool).expect("best");
        assert_eq!(best.id, "function:s");
    }

    // ================= match_scoped_call_chain ================================

    #[test]
    fn scoped_call_chain_resolves_via_return_type() {
        // Rust `Foo::bar().baz` — bar() returns Foo; baz resolves on Foo.
        let factory = {
            let mut n = mk(
                "method:bar",
                NodeKind::Method,
                "bar",
                "Foo::bar",
                "src/a.rs",
                Language::Rust,
            );
            n.return_type = Some("Foo".to_string());
            n
        };
        let baz = mk(
            "method:baz",
            NodeKind::Method,
            "baz",
            "Foo::baz",
            "src/a.rs",
            Language::Rust,
        );
        let ctx = Ctx::default()
            .name("bar", vec![factory])
            .name("baz", vec![baz]);
        let r = refv(
            "Foo::bar().baz",
            EdgeKind::Calls,
            "src/b.rs",
            Language::Rust,
            1,
        );
        let res = match_scoped_call_chain(&r, &ctx).expect("scoped chain");
        assert_eq!(res.target_node_id, "method:baz");
        assert_eq!(res.resolved_by, ResolvedBy::InstanceMethod);
    }

    #[test]
    fn scoped_call_chain_self_return_uses_factory_class() {
        // Return type "self" resolves the method on the factory class itself.
        let factory = {
            let mut n = mk(
                "method:bar",
                NodeKind::Method,
                "bar",
                "Foo::bar",
                "src/a.rs",
                Language::Rust,
            );
            n.return_type = Some("self".to_string());
            n
        };
        let baz = mk(
            "method:baz",
            NodeKind::Method,
            "baz",
            "Foo::baz",
            "src/a.rs",
            Language::Rust,
        );
        let ctx = Ctx::default()
            .name("bar", vec![factory])
            .name("baz", vec![baz]);
        let r = refv(
            "Foo::bar().baz",
            EdgeKind::Calls,
            "src/b.rs",
            Language::Rust,
            1,
        );
        let res = match_scoped_call_chain(&r, &ctx).expect("self chain");
        assert_eq!(res.target_node_id, "method:baz");
    }

    #[test]
    fn scoped_call_chain_requires_colon_scope() {
        // Inner without `::` declines.
        let ctx = Ctx::default();
        let r = refv("foo().baz", EdgeKind::Calls, "src/b.rs", Language::Rust, 1);
        assert!(match_scoped_call_chain(&r, &ctx).is_none());
    }

    #[test]
    fn scoped_call_chain_bad_shape_declines() {
        let ctx = Ctx::default();
        let r = refv("Foo::bar", EdgeKind::Calls, "src/b.rs", Language::Rust, 1);
        assert!(match_scoped_call_chain(&r, &ctx).is_none());
    }

    // ================= match_dotted_call_chain ================================

    #[test]
    fn dotted_chain_go_bare_factory_via_return_type() {
        // Go `NewFoo().Bar` — NewFoo returns Foo; Bar resolves on Foo.
        let newfoo = {
            let mut n = mk(
                "function:nf",
                NodeKind::Function,
                "NewFoo",
                "NewFoo",
                "a.go",
                Language::Go,
            );
            n.return_type = Some("Foo".to_string());
            n
        };
        let bar = mk(
            "method:bar",
            NodeKind::Method,
            "Bar",
            "Foo::Bar",
            "a.go",
            Language::Go,
        );
        let ctx = Ctx::default()
            .name("NewFoo", vec![newfoo])
            .name("Bar", vec![bar]);
        let r = refv("NewFoo().Bar", EdgeKind::Calls, "b.go", Language::Go, 1);
        let res = match_dotted_call_chain(&r, &ctx).expect("go bare chain");
        assert_eq!(res.target_node_id, "method:bar");
    }

    #[test]
    fn dotted_chain_go_bare_fallback_to_exact_name() {
        // Go bare factory with no return type falls back to exact-name on method.
        let target = mk(
            "function:worker",
            NodeKind::Function,
            "Bar",
            "Bar",
            "a.go",
            Language::Go,
        );
        let ctx = Ctx::default()
            .name("NewFoo", vec![]) // no return-type node
            .name("Bar", vec![target]);
        let r = refv("NewFoo().Bar", EdgeKind::Calls, "b.go", Language::Go, 1);
        let res = match_dotted_call_chain(&r, &ctx).expect("go fallback");
        assert_eq!(res.target_node_id, "function:worker");
    }

    #[test]
    fn dotted_chain_construct_via_bare_call_kotlin() {
        // Kotlin `Foo().bar` — capitalized bare constructor receiver.
        let bar = mk(
            "method:bar",
            NodeKind::Method,
            "bar",
            "Foo::bar",
            "a.kt",
            Language::Kotlin,
        );
        let ctx = Ctx::default().name("bar", vec![bar]);
        let r = refv("Foo().bar", EdgeKind::Calls, "b.kt", Language::Kotlin, 1);
        let res = match_dotted_call_chain(&r, &ctx).expect("kotlin construct");
        assert_eq!(res.target_node_id, "method:bar");
    }

    #[test]
    fn dotted_chain_construct_via_bare_lowercase_declines() {
        // Kotlin lowercase bare receiver is not a constructor => declines.
        let ctx = Ctx::default();
        let r = refv("foo().bar", EdgeKind::Calls, "b.kt", Language::Kotlin, 1);
        assert!(match_dotted_call_chain(&r, &ctx).is_none());
    }

    #[test]
    fn dotted_chain_non_go_bare_non_construct_declines() {
        // Java bare `foo().bar` — not Go, not a construct-via-bare language.
        let ctx = Ctx::default();
        let r = refv("foo().bar", EdgeKind::Calls, "b.java", Language::Java, 1);
        assert!(match_dotted_call_chain(&r, &ctx).is_none());
    }

    #[test]
    fn dotted_chain_factory_fluent_receiver() {
        // `Builder.make().build` — make returns Widget; build resolves on Widget.
        let make = {
            let mut n = mk(
                "method:make",
                NodeKind::Method,
                "make",
                "Builder::make",
                "a.java",
                Language::Java,
            );
            n.return_type = Some("Widget".to_string());
            n
        };
        let build = mk(
            "method:build",
            NodeKind::Method,
            "build",
            "Widget::build",
            "a.java",
            Language::Java,
        );
        let ctx = Ctx::default()
            .name("make", vec![make])
            .name("build", vec![build]);
        let r = refv(
            "Builder.make().build",
            EdgeKind::Calls,
            "b.java",
            Language::Java,
            1,
        );
        let res = match_dotted_call_chain(&r, &ctx).expect("fluent chain");
        assert_eq!(res.target_node_id, "method:build");
    }

    #[test]
    fn dotted_chain_bad_shape_declines() {
        let ctx = Ctx::default();
        let r = refv("noshape", EdgeKind::Calls, "b.java", Language::Java, 1);
        assert!(match_dotted_call_chain(&r, &ctx).is_none());
    }

    #[test]
    fn constructs_via_bare_call_languages() {
        assert!(constructs_via_bare_call(Language::Kotlin));
        assert!(constructs_via_bare_call(Language::Swift));
        assert!(constructs_via_bare_call(Language::Scala));
        assert!(constructs_via_bare_call(Language::Dart));
        assert!(!constructs_via_bare_call(Language::Java));
        assert!(!constructs_via_bare_call(Language::Go));
    }

    // ================= resolve_method_on_type =================================

    #[test]
    fn method_on_type_walks_supertype_for_conformance() {
        // ping is on Base, not Sub; resolve_method_on_type walks Sub -> Base.
        let ping = mk(
            "method:ping",
            NodeKind::Method,
            "ping",
            "Base::ping",
            "Base.java",
            Language::Java,
        );
        let ctx =
            Ctx::default()
                .name("ping", vec![ping])
                .supertype("Sub", Language::Java, vec!["Base"]);
        let r = refv("x", EdgeKind::Calls, "Sub.java", Language::Java, 1);
        let res = resolve_method_on_type(
            "Sub",
            "ping",
            &r,
            &ctx,
            0.85,
            ResolvedBy::InstanceMethod,
            None,
            0,
        )
        .expect("supertype walk");
        assert_eq!(res.target_node_id, "method:ping");
    }

    #[test]
    fn method_on_type_no_method_no_supertype_declines() {
        let ctx = Ctx::default().name("ping", vec![]);
        let r = refv("x", EdgeKind::Calls, "Sub.java", Language::Java, 1);
        assert!(
            resolve_method_on_type(
                "Sub",
                "ping",
                &r,
                &ctx,
                0.85,
                ResolvedBy::InstanceMethod,
                None,
                0
            )
            .is_none()
        );
    }

    #[test]
    fn method_on_type_preferred_fqn_disambiguates() {
        // Two Foo::bar methods; preferred_fqn "pkg.Foo" picks the one in that path.
        let a = mk(
            "method:a",
            NodeKind::Method,
            "bar",
            "Foo::bar",
            "src/pkg/Foo.java",
            Language::Java,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "bar",
            "Foo::bar",
            "src/other/Foo.java",
            Language::Java,
        );
        let ctx = Ctx::default().name("bar", vec![a, b]);
        let r = refv("x", EdgeKind::Calls, "src/x.java", Language::Java, 1);
        let res = resolve_method_on_type(
            "Foo",
            "bar",
            &r,
            &ctx,
            0.9,
            ResolvedBy::InstanceMethod,
            Some("pkg.Foo"),
            0,
        )
        .expect("fqn disambiguation");
        assert_eq!(res.target_node_id, "method:a");
    }

    // ================= match_function_ref =====================================

    #[test]
    fn function_ref_this_member_declines() {
        let ctx = Ctx::default();
        let r = refv(
            "this.handler",
            EdgeKind::References,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_function_ref(&r, &ctx).is_none());
    }

    #[test]
    fn function_ref_bare_function_same_file() {
        let n = mk(
            "function:h",
            NodeKind::Function,
            "onBlur",
            "onBlur",
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("onBlur", vec![n]);
        let r = refv(
            "onBlur",
            EdgeKind::References,
            "src/a.ts",
            Language::TypeScript,
            5,
        );
        let res = match_function_ref(&r, &ctx).expect("bare fn ref");
        assert_eq!(res.target_node_id, "function:h");
        assert_eq!(res.resolved_by, ResolvedBy::FunctionRef);
        assert!((res.confidence - 0.95).abs() < 1e-9);
    }

    #[test]
    fn function_ref_cross_file_unique() {
        // Not in the same file but the only candidate => confidence 0.8.
        let n = mk(
            "function:h",
            NodeKind::Function,
            "onBlur",
            "onBlur",
            "src/other.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("onBlur", vec![n]);
        let r = refv(
            "onBlur",
            EdgeKind::References,
            "src/a.ts",
            Language::TypeScript,
            5,
        );
        let res = match_function_ref(&r, &ctx).expect("cross-file unique");
        assert!((res.confidence - 0.8).abs() < 1e-9);
    }

    #[test]
    fn function_ref_bare_fn_only_excludes_methods_in_ts() {
        // In TS a bare id can only be a Function value, so a Method candidate is
        // filtered out => declines.
        let m = mk(
            "method:m",
            NodeKind::Method,
            "handler",
            "Foo::handler",
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("handler", vec![m]);
        let r = refv(
            "handler",
            EdgeKind::References,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_function_ref(&r, &ctx).is_none());
    }

    #[test]
    fn function_ref_bare_class_target_matrix() {
        let cases = [
            (Language::TypeScript, NodeKind::Class, false),
            (Language::Tsx, NodeKind::Class, false),
            (Language::JavaScript, NodeKind::Class, false),
            (Language::Jsx, NodeKind::Class, false),
            (Language::Cpp, NodeKind::Class, false),
            (Language::Php, NodeKind::Class, false),
            (Language::Python, NodeKind::Method, false),
            (Language::Python, NodeKind::Class, true),
        ];

        for (language, kind, should_resolve) in cases {
            let target = mk(
                "target:id",
                kind,
                "Target",
                "Target",
                "src/target",
                language,
            );
            let ctx = Ctx::default().name("Target", vec![target]);
            let r = refv("Target", EdgeKind::References, "src/consumer", language, 1);
            let resolved = match_function_ref(&r, &ctx);

            if should_resolve {
                let resolved = resolved.unwrap_or_else(|| {
                    panic!("{language:?} {kind:?} must be a function-ref target")
                });
                assert_eq!(resolved.target_node_id, "target:id");
                assert_eq!(resolved.resolved_by, ResolvedBy::FunctionRef);
            } else {
                assert!(
                    resolved.is_none(),
                    "{language:?} {kind:?} must not be a function-ref target"
                );
            }
        }
    }

    #[test]
    fn function_ref_qualified_member_pointer_unique() {
        // C++ `&Widget::on_click` — resolve the member on that scope.
        let m = mk(
            "method:oc",
            NodeKind::Method,
            "on_click",
            "Widget::on_click",
            "src/a.cpp",
            Language::Cpp,
        );
        let ctx = Ctx::default().name("on_click", vec![m]);
        let r = refv(
            "Widget::on_click",
            EdgeKind::References,
            "src/a.cpp",
            Language::Cpp,
            1,
        );
        let res = match_function_ref(&r, &ctx).expect("member pointer");
        assert_eq!(res.target_node_id, "method:oc");
        assert!((res.confidence - 0.9).abs() < 1e-9);
    }

    #[test]
    fn function_ref_qualified_member_pointer_ambiguous_cross_file_declines() {
        // Two matches, neither in the ref's file => ambiguous, declines.
        let a = mk(
            "method:a",
            NodeKind::Method,
            "on_click",
            "Widget::on_click",
            "src/one.cpp",
            Language::Cpp,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "on_click",
            "Widget::on_click",
            "src/two.cpp",
            Language::Cpp,
        );
        let ctx = Ctx::default().name("on_click", vec![a, b]);
        let r = refv(
            "Widget::on_click",
            EdgeKind::References,
            "src/three.cpp",
            Language::Cpp,
            1,
        );
        assert!(match_function_ref(&r, &ctx).is_none());
    }

    #[test]
    fn function_ref_qualified_member_pointer_no_match_declines() {
        let ctx = Ctx::default().name("on_click", vec![]);
        let r = refv(
            "Widget::on_click",
            EdgeKind::References,
            "src/a.cpp",
            Language::Cpp,
            1,
        );
        assert!(match_function_ref(&r, &ctx).is_none());
    }

    #[test]
    fn function_ref_no_candidates_declines() {
        let ctx = Ctx::default();
        let r = refv(
            "nowhere",
            EdgeKind::References,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_function_ref(&r, &ctx).is_none());
    }

    #[test]
    fn function_ref_swift_implicit_self_keeps_enclosing_method() {
        // Swift: bare id names a method only on the enclosing type. The caller's
        // qualified_name gives the class prefix; a method on the same class stays.
        let caller = mk(
            "method:caller",
            NodeKind::Method,
            "run",
            "MyView::run",
            "src/v.swift",
            Language::Swift,
        );
        let same_class = mk(
            "method:h",
            NodeKind::Method,
            "helper",
            "MyView::helper",
            "src/v.swift",
            Language::Swift,
        );
        let ctx = Ctx::default()
            .name("helper", vec![same_class])
            .node_by_id(caller);
        let mut r = refv(
            "helper",
            EdgeKind::References,
            "src/v.swift",
            Language::Swift,
            5,
        );
        r.from_node_id = "method:caller".to_string();
        let res = match_function_ref(&r, &ctx).expect("swift self method");
        assert_eq!(res.target_node_id, "method:h");
    }

    #[test]
    fn function_ref_swift_implicit_self_drops_foreign_class_method() {
        // A same-named method on a DIFFERENT class is a parameter collision => drop.
        let caller = mk(
            "method:caller",
            NodeKind::Method,
            "run",
            "MyView::run",
            "src/v.swift",
            Language::Swift,
        );
        let foreign = mk(
            "method:f",
            NodeKind::Method,
            "helper",
            "OtherView::helper",
            "src/o.swift",
            Language::Swift,
        );
        let ctx = Ctx::default()
            .name("helper", vec![foreign])
            .node_by_id(caller);
        let mut r = refv(
            "helper",
            EdgeKind::References,
            "src/v.swift",
            Language::Swift,
            5,
        );
        r.from_node_id = "method:caller".to_string();
        assert!(match_function_ref(&r, &ctx).is_none());
    }

    // ================= match_reference orchestrator ===========================

    #[test]
    fn match_reference_dispatches_file_path_first() {
        let f = mk(
            "file:1",
            NodeKind::File,
            "b.ts",
            "a/b.ts",
            "a/b.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("b.ts", vec![f]);
        let r = refv(
            "a/b.ts",
            EdgeKind::References,
            "src/x.ts",
            Language::TypeScript,
            1,
        );
        let res = match_reference(&r, &ctx).expect("file path via orchestrator");
        assert_eq!(res.resolved_by, ResolvedBy::FilePath);
    }

    #[test]
    fn match_reference_falls_through_to_exact_name() {
        let n = mk(
            "function:x",
            NodeKind::Function,
            "run",
            "run",
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("run", vec![n]);
        let r = refv("run", EdgeKind::Calls, "src/b.ts", Language::TypeScript, 1);
        let res = match_reference(&r, &ctx).expect("exact via orchestrator");
        assert_eq!(res.resolved_by, ResolvedBy::ExactMatch);
    }

    #[test]
    fn match_reference_falls_through_to_fuzzy() {
        // Nothing exact; a single fuzzy (lower-name) callable resolves last.
        let n = mk(
            "function:x",
            NodeKind::Function,
            "run",
            "run",
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().lower("run", vec![n]);
        let r = refv("run", EdgeKind::Calls, "src/b.ts", Language::TypeScript, 1);
        let res = match_reference(&r, &ctx).expect("fuzzy via orchestrator");
        assert_eq!(res.resolved_by, ResolvedBy::Fuzzy);
    }

    #[test]
    fn match_reference_returns_none_when_nothing_matches() {
        let ctx = Ctx::default();
        let r = refv(
            "ghost",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            1,
        );
        assert!(match_reference(&r, &ctx).is_none());
    }

    #[test]
    fn erlang_local_reference_prefers_same_file_and_never_sibling_arity() {
        let local = mk(
            "function:local",
            NodeKind::Function,
            "header",
            "deleg::header/3",
            "src/deleg.erl",
            Language::Erlang,
        );
        let other = mk(
            "function:other",
            NodeKind::Function,
            "header",
            "other::header/3",
            "src/other.erl",
            Language::Erlang,
        );
        let sibling = mk(
            "function:sibling",
            NodeKind::Function,
            "header",
            "deleg::header/2",
            "src/deleg.erl",
            Language::Erlang,
        );
        let ctx = Ctx::default().name("header", vec![other, sibling, local]);
        let hit = refv(
            "header/3",
            EdgeKind::Calls,
            "src/deleg.erl",
            Language::Erlang,
            10,
        );
        assert_eq!(
            match_reference(&hit, &ctx)
                .expect("same-file exact arity")
                .target_node_id,
            "function:local"
        );
        let miss = refv(
            "header/4",
            EdgeKind::Calls,
            "src/deleg.erl",
            Language::Erlang,
            10,
        );
        assert!(match_reference(&miss, &ctx).is_none());
    }

    // ================= C++ receiver inference =================================

    #[test]
    fn cpp_receiver_inference_from_declarator() {
        // `Foo f;\n f.v()` — infer f's type Foo from the declarator, resolve v on Foo.
        let v = mk(
            "method:v",
            NodeKind::Method,
            "v",
            "Foo::v",
            "src/main.cpp",
            Language::Cpp,
        );
        let ctx = Ctx::default()
            .file(
                "src/main.cpp",
                "struct Foo { int v(); };\nint run() { Foo f; return f.v(); }\n",
            )
            .name("v", vec![v]);
        let r = refv("f.v", EdgeKind::Calls, "src/main.cpp", Language::Cpp, 2);
        let res = match_method_call(&r, &ctx).expect("cpp receiver");
        assert_eq!(res.target_node_id, "method:v");
        assert_eq!(res.resolved_by, ResolvedBy::InstanceMethod);
    }

    #[test]
    fn cpp_explicit_operator_call_resolves_via_receiver_type() {
        // `const V& a; a.operator+(b)` reaches the resolver as `a.operator+`
        // (#1268). `Aaa` declares the SAME operator and sorts first, so only
        // receiver-type inference can pick `V`.
        let v_op = mk(
            "method:v_plus",
            NodeKind::Method,
            "operator+",
            "V::operator+",
            "src/optest.cpp",
            Language::Cpp,
        );
        let decoy = mk(
            "method:aaa_plus",
            NodeKind::Method,
            "operator+",
            "Aaa::operator+",
            "src/optest.cpp",
            Language::Cpp,
        );
        let ctx = Ctx::default()
            .file(
                "src/optest.cpp",
                "struct Aaa { Aaa operator+(const Aaa& o) const; };\nstruct V { V operator+(const V& o) const; };\nV add(const V& a, const V& b) { return a.operator+(b); }\n",
            )
            .name("operator+", vec![decoy, v_op]);
        let r = refv(
            "a.operator+",
            EdgeKind::Calls,
            "src/optest.cpp",
            Language::Cpp,
            3,
        );
        let res = match_method_call(&r, &ctx).expect("explicit operator call resolves");
        assert_eq!(
            res.target_node_id, "method:v_plus",
            "receiver type V must win over the same-named Aaa::operator+"
        );
        assert_eq!(res.resolved_by, ResolvedBy::InstanceMethod);
    }

    #[test]
    fn cpp_operator_dot_shape_is_a_method_call_shape() {
        assert_eq!(
            parse_method_call("a.operator+", Language::Cpp),
            Some(("a".to_string(), "operator+".to_string()))
        );
        assert_eq!(
            parse_method_call("it.operator[]", Language::Cpp),
            Some(("it".to_string(), "operator[]".to_string()))
        );
        // Word-shaped names keep the plain dotted pattern.
        assert_eq!(
            parse_method_call("a.operatorTable", Language::Cpp),
            Some(("a".to_string(), "operatorTable".to_string()))
        );
        // The symbolic form is C/C++-only.
        assert_eq!(parse_method_call("a.operator+", Language::TypeScript), None);
        // `operator` with no trailing symbol is NOT the operator shape; it stays
        // an ordinary dotted call (handled by the plain pattern, tried first).
        assert_eq!(match_cpp_operator_dot_call("a.operator"), None);
        assert_eq!(
            parse_method_call("a.operator", Language::Cpp),
            Some(("a".to_string(), "operator".to_string()))
        );
        // A dotted receiver chain is accepted; a bare `.operator+` is not.
        assert_eq!(
            match_cpp_operator_dot_call("w.inner.operator=="),
            Some(("w.inner".to_string(), "operator==".to_string()))
        );
        assert_eq!(match_cpp_operator_dot_call(".operator+"), None);
    }

    #[test]
    fn cpp_receiver_auto_new_initializer() {
        // `auto f = new Foo();\n f.v()` — infer type from the `new Foo` initializer.
        let v = mk(
            "method:v",
            NodeKind::Method,
            "v",
            "Foo::v",
            "src/main.cpp",
            Language::Cpp,
        );
        let ctx = Ctx::default()
            .file(
                "src/main.cpp",
                "int run() { auto f = new Foo(); return f.v(); }\n",
            )
            .name("v", vec![v]);
        let r = refv("f.v", EdgeKind::Calls, "src/main.cpp", Language::Cpp, 1);
        let res = match_method_call(&r, &ctx).expect("cpp auto new");
        assert_eq!(res.target_node_id, "method:v");
    }

    // ================= Java field receiver ====================================

    #[test]
    fn java_field_receiver_from_signature() {
        // `repo.save()` where `repo` is a field of declared type `Repo`.
        let class = mk(
            "class:svc",
            NodeKind::Class,
            "Service",
            "Service",
            "src/S.java",
            Language::Java,
        );
        let mut class = class;
        class.start_line = 1;
        class.end_line = 5;
        let mut field = mk(
            "field:repo",
            NodeKind::Field,
            "repo",
            "Service::repo",
            "src/S.java",
            Language::Java,
        );
        field.start_line = 2;
        field.end_line = 2;
        field.signature = Some("Repo repo".to_string());
        let save = mk(
            "method:save",
            NodeKind::Method,
            "save",
            "Repo::save",
            "src/R.java",
            Language::Java,
        );
        let ctx = Ctx::default()
            .nodes_in_file("src/S.java", vec![class, field])
            .name("save", vec![save]);
        let r = refv(
            "repo.save",
            EdgeKind::Calls,
            "src/S.java",
            Language::Java,
            3,
        );
        let res = match_method_call(&r, &ctx).expect("java field receiver");
        assert_eq!(res.target_node_id, "method:save");
        assert_eq!(res.resolved_by, ResolvedBy::InstanceMethod);
    }

    // ================= ambiguous_name_ceiling env override ====================

    #[test]
    fn ambiguous_name_ceiling_default() {
        // The default ceiling is returned when the env var is unset/invalid.
        // (The OnceLock caches on first read; asserting the constant is stable.)
        assert_eq!(AMBIGUOUS_NAME_CEILING, 500);
        // ambiguous_name_ceiling() returns a positive number.
        assert!(ambiguous_name_ceiling() > 0);
    }

    // ================= C++ inference deep paths ===============================

    #[test]
    fn cpp_receiver_auto_make_unique_initializer() {
        // `auto f = std::make_unique<Foo>();\n f->v()` style — the call-shape
        // initializer routes through resolve_cpp_call_result_type's make_unique arm.
        let v = mk(
            "method:v",
            NodeKind::Method,
            "v",
            "Foo::v",
            "src/m.cpp",
            Language::Cpp,
        );
        let ctx = Ctx::default()
            .file(
                "src/m.cpp",
                "int run() { auto f = std::make_unique<Foo>(); return f.v(); }\n",
            )
            .name("v", vec![v]);
        let r = refv("f.v", EdgeKind::Calls, "src/m.cpp", Language::Cpp, 1);
        let res = match_method_call(&r, &ctx).expect("make_unique inference");
        assert_eq!(res.target_node_id, "method:v");
    }

    #[test]
    fn cpp_receiver_auto_call_return_type_initializer() {
        // `auto f = make();\n f.v()` — make() returns Foo (declared return type),
        // so f is a Foo and v resolves on Foo.
        let make = {
            let mut n = mk(
                "function:make",
                NodeKind::Function,
                "make",
                "make",
                "src/m.cpp",
                Language::Cpp,
            );
            n.return_type = Some("Foo".to_string());
            n
        };
        let v = mk(
            "method:v",
            NodeKind::Method,
            "v",
            "Foo::v",
            "src/m.cpp",
            Language::Cpp,
        );
        let ctx = Ctx::default()
            .file(
                "src/m.cpp",
                "int run() { auto f = make(); return f.v(); }\n",
            )
            .name("make", vec![make])
            .name("v", vec![v]);
        let r = refv("f.v", EdgeKind::Calls, "src/m.cpp", Language::Cpp, 1);
        let res = match_method_call(&r, &ctx).expect("call-return inference");
        assert_eq!(res.target_node_id, "method:v");
    }

    #[test]
    fn cpp_receiver_auto_direct_construction_initializer() {
        // `auto f = Foo();\n f.v()` — the initializer names a class that exists,
        // so resolve_cpp_call_result_type's cpp_class_exists arm resolves Foo.
        let class = mk(
            "class:foo",
            NodeKind::Class,
            "Foo",
            "Foo",
            "src/m.cpp",
            Language::Cpp,
        );
        let v = mk(
            "method:v",
            NodeKind::Method,
            "v",
            "Foo::v",
            "src/m.cpp",
            Language::Cpp,
        );
        let ctx = Ctx::default()
            .file("src/m.cpp", "int run() { auto f = Foo(); return f.v(); }\n")
            .name("Foo", vec![class])
            .name("v", vec![v]);
        let r = refv("f.v", EdgeKind::Calls, "src/m.cpp", Language::Cpp, 1);
        let res = match_method_call(&r, &ctx).expect("direct construction");
        assert_eq!(res.target_node_id, "method:v");
    }

    #[test]
    fn cpp_receiver_header_fallback_declarator() {
        // No declarator in the .cpp; the sibling .h declares `Foo f;` — the header
        // fallback recovers the type.
        let v = mk(
            "method:v",
            NodeKind::Method,
            "v",
            "Foo::v",
            "src/m.cpp",
            Language::Cpp,
        );
        let ctx = Ctx::default()
            .file("src/m.cpp", "int run() { return f.v(); }\n")
            .file("src/m.h", "struct Bag { Foo f; };\n")
            .name("v", vec![v]);
        let r = refv("f.v", EdgeKind::Calls, "src/m.cpp", Language::Cpp, 1);
        let res = match_method_call(&r, &ctx).expect("header fallback");
        assert_eq!(res.target_node_id, "method:v");
    }

    #[test]
    fn cpp_receiver_no_declarator_no_header_declines() {
        // No declarator anywhere and no method fallback => no C++ typed edge.
        let ctx = Ctx::default().file("src/m.cpp", "int run() { return f.v(); }\n");
        let r = refv("f.v", EdgeKind::Calls, "src/m.cpp", Language::Cpp, 1);
        assert!(match_method_call(&r, &ctx).is_none());
    }

    // ================= match_function_ref extra branches =====================

    #[test]
    fn function_ref_member_pointer_single_cross_file_resolves() {
        // Exactly one scoped match, in a different file => still resolves (unique).
        let m = mk(
            "method:oc",
            NodeKind::Method,
            "on_click",
            "Widget::on_click",
            "src/w.cpp",
            Language::Cpp,
        );
        let ctx = Ctx::default().name("on_click", vec![m]);
        let r = refv(
            "Widget::on_click",
            EdgeKind::References,
            "src/other.cpp",
            Language::Cpp,
            1,
        );
        let res = match_function_ref(&r, &ctx).expect("unique cross-file member ptr");
        assert_eq!(res.target_node_id, "method:oc");
    }

    #[test]
    fn function_ref_swift_overload_family_same_file_declines() {
        // Swift: >1 same-named method in the ref's file is a parameter collision.
        let caller = mk(
            "method:caller",
            NodeKind::Method,
            "run",
            "MyView::run",
            "src/v.swift",
            Language::Swift,
        );
        let a = mk(
            "method:a",
            NodeKind::Method,
            "helper",
            "MyView::helper",
            "src/v.swift",
            Language::Swift,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "helper",
            "MyView::helper",
            "src/v.swift",
            Language::Swift,
        );
        let ctx = Ctx::default().name("helper", vec![a, b]).node_by_id(caller);
        let mut r = refv(
            "helper",
            EdgeKind::References,
            "src/v.swift",
            Language::Swift,
            5,
        );
        r.from_node_id = "method:caller".to_string();
        assert!(match_function_ref(&r, &ctx).is_none());
    }

    #[test]
    fn function_ref_method_allowed_in_non_bare_language() {
        // Rust is not a bare-fn-only language, so a Method candidate is eligible.
        let m = mk(
            "method:m",
            NodeKind::Method,
            "handler",
            "Foo::handler",
            "src/a.rs",
            Language::Rust,
        );
        let ctx = Ctx::default().name("handler", vec![m]);
        let r = refv(
            "handler",
            EdgeKind::References,
            "src/a.rs",
            Language::Rust,
            1,
        );
        let res = match_function_ref(&r, &ctx).expect("rust method value");
        assert_eq!(res.target_node_id, "method:m");
    }

    // ================= lookup_callee_return_type / imported_fqn ==============

    #[test]
    fn dotted_chain_uses_imported_fqn_for_preferred_disambiguation() {
        // Go bare factory: NewFoo returns Foo, and the file imports Foo from a
        // module, so imported_fqn_of feeds resolve_method_on_type's preferred fqn.
        let newfoo = {
            let mut n = mk(
                "function:nf",
                NodeKind::Function,
                "NewFoo",
                "NewFoo",
                "a.go",
                Language::Go,
            );
            n.return_type = Some("Foo".to_string());
            n
        };
        // Two Foo::Bar methods; the imported source path picks the one in pkg/Foo.
        let a = mk(
            "method:a",
            NodeKind::Method,
            "Bar",
            "Foo::Bar",
            "pkg/Foo.go",
            Language::Go,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "Bar",
            "Foo::Bar",
            "other/Foo.go",
            Language::Go,
        );
        let import = ImportMapping {
            local_name: "Foo".to_string(),
            exported_name: "Foo".to_string(),
            source: "pkg.Foo".to_string(),
            is_default: false,
            is_namespace: false,
        };
        let ctx = Ctx::default()
            .name("NewFoo", vec![newfoo])
            .name("Bar", vec![a, b])
            .import("b.go", vec![import]);
        let r = refv("NewFoo().Bar", EdgeKind::Calls, "b.go", Language::Go, 1);
        // Kotlin ext is `.kt`; Go/Java use `.java` in resolve_method_on_type's
        // preferred-fqn path, so pkg/Foo.java is sought — no match on `.go` paths
        // means it falls back to matches[0], which is still a valid resolution.
        let res = match_dotted_call_chain(&r, &ctx).expect("go imported fqn");
        assert!(res.target_node_id.starts_with("method:"));
    }

    #[test]
    fn lookup_callee_return_type_bare_function_path() {
        // A bare (no `::`) callee resolves via the first Function candidate's
        // declared return type — exercised through the Go bare-factory chain.
        let make = {
            let mut n = mk(
                "function:mk",
                NodeKind::Function,
                "Build",
                "Build",
                "a.go",
                Language::Go,
            );
            n.return_type = Some("Result".to_string());
            n
        };
        let done = mk(
            "method:done",
            NodeKind::Method,
            "Done",
            "Result::Done",
            "a.go",
            Language::Go,
        );
        let ctx = Ctx::default()
            .name("Build", vec![make])
            .name("Done", vec![done]);
        let r = refv("Build().Done", EdgeKind::Calls, "b.go", Language::Go, 1);
        let res = match_dotted_call_chain(&r, &ctx).expect("bare fn return type");
        assert_eq!(res.target_node_id, "method:done");
    }

    // ================= resolve_method_on_type depth guard ====================

    #[test]
    fn method_on_type_supertype_recursion_bounded() {
        // A supertype cycle deeper than the depth cap (4) still terminates and
        // declines rather than looping forever.
        let ctx = Ctx::default()
            .name("ghost", vec![])
            .supertype("A", Language::Java, vec!["B"])
            .supertype("B", Language::Java, vec!["C"])
            .supertype("C", Language::Java, vec!["D"])
            .supertype("D", Language::Java, vec!["E"])
            .supertype("E", Language::Java, vec!["A"]);
        let r = refv("x", EdgeKind::Calls, "src/a.java", Language::Java, 1);
        assert!(
            resolve_method_on_type(
                "A",
                "ghost",
                &r,
                &ctx,
                0.85,
                ResolvedBy::InstanceMethod,
                None,
                0
            )
            .is_none()
        );
    }

    #[test]
    fn method_on_type_qualified_suffix_match() {
        // The method's qualified_name ends with `::Type::method`, exercising the
        // suffix arm of resolve_method_on_type's match filter.
        let m = mk(
            "method:m",
            NodeKind::Method,
            "run",
            "pkg::Foo::run",
            "src/a.rs",
            Language::Rust,
        );
        let ctx = Ctx::default().name("run", vec![m]);
        let r = refv("x", EdgeKind::Calls, "src/a.rs", Language::Rust, 1);
        let res = resolve_method_on_type(
            "Foo",
            "run",
            &r,
            &ctx,
            0.9,
            ResolvedBy::InstanceMethod,
            None,
            0,
        )
        .expect("suffix match");
        assert_eq!(res.target_node_id, "method:m");
    }

    // ================= Round 5: prefer_call_site_file (#1079) =================

    #[test]
    fn prefer_call_site_file_partitions_same_file_first() {
        // Two same-named nodes: the one in the call site's file is moved first,
        // the rest keep their original order.
        let a = mk(
            "method:a",
            NodeKind::Method,
            "log",
            "Logger::log",
            "a/svc.cpp",
            Language::Cpp,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "log",
            "Logger::log",
            "b/svc.cpp",
            Language::Cpp,
        );
        let c = mk(
            "method:c",
            NodeKind::Method,
            "log",
            "Logger::log",
            "c/svc.cpp",
            Language::Cpp,
        );
        let ordered = prefer_call_site_file(vec![a, b, c], "b/svc.cpp");
        assert_eq!(ordered[0].id, "method:b");
        assert_eq!(ordered[1].id, "method:a");
        assert_eq!(ordered[2].id, "method:c");
    }

    #[test]
    fn prefer_call_site_file_noop_when_lt_two() {
        let a = mk(
            "method:a",
            NodeKind::Method,
            "log",
            "Logger::log",
            "a/svc.cpp",
            Language::Cpp,
        );
        let ordered = prefer_call_site_file(vec![a], "z/other.cpp");
        assert_eq!(ordered.len(), 1);
        assert_eq!(ordered[0].id, "method:a");
    }

    #[test]
    fn prefer_call_site_file_noop_when_none_in_file() {
        // No candidate shares the call site's file — original order preserved.
        let a = mk(
            "method:a",
            NodeKind::Method,
            "log",
            "Logger::log",
            "a/svc.cpp",
            Language::Cpp,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "log",
            "Logger::log",
            "b/svc.cpp",
            Language::Cpp,
        );
        let ordered = prefer_call_site_file(vec![a, b], "z/other.cpp");
        assert_eq!(ordered[0].id, "method:a");
        assert_eq!(ordered[1].id, "method:b");
    }

    #[test]
    fn method_on_type_prefers_call_site_file_when_ambiguous() {
        // Two `Logger::log` methods (ODR clash). Call from b/svc.cpp resolves to
        // the b/ one, NOT the first-indexed a/ one (#1079).
        let a = mk(
            "method:a",
            NodeKind::Method,
            "log",
            "Logger::log",
            "a/svc.cpp",
            Language::Cpp,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "log",
            "Logger::log",
            "b/svc.cpp",
            Language::Cpp,
        );
        let ctx = Ctx::default().name("log", vec![a, b]);
        let r = refv("x", EdgeKind::Calls, "b/svc.cpp", Language::Cpp, 1);
        let res = resolve_method_on_type(
            "Logger",
            "log",
            &r,
            &ctx,
            0.9,
            ResolvedBy::InstanceMethod,
            None,
            0,
        )
        .expect("ambiguous method resolves to own file");
        assert_eq!(res.target_node_id, "method:b");
    }

    #[test]
    fn method_on_type_preferred_fqn_beats_call_site_file() {
        // #314 must still win over #1079: the preferredFqn picks a DIFFERENT file
        // than the call site (Java import disambiguation targets another file).
        let a = mk(
            "method:a",
            NodeKind::Method,
            "bar",
            "Foo::bar",
            "src/pkg/Foo.java",
            Language::Java,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "bar",
            "Foo::bar",
            "src/callsite/Foo.java",
            Language::Java,
        );
        let ctx = Ctx::default().name("bar", vec![a, b]);
        // Call site is src/callsite/... but the imported FQN pins pkg.Foo.
        let r = refv(
            "x",
            EdgeKind::Calls,
            "src/callsite/Caller.java",
            Language::Java,
            1,
        );
        let res = resolve_method_on_type(
            "Foo",
            "bar",
            &r,
            &ctx,
            0.9,
            ResolvedBy::InstanceMethod,
            Some("pkg.Foo"),
            0,
        )
        .expect("preferred fqn wins");
        assert_eq!(res.target_node_id, "method:a");
    }

    #[test]
    fn qualified_name_exact_ambiguous_prefers_call_site_file() {
        // Two exact `Foo::get` qualified matches; the call site's own file wins (#1079).
        let a = mk(
            "method:a",
            NodeKind::Method,
            "get",
            "Foo::get",
            "a/foo.rs",
            Language::Rust,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "get",
            "Foo::get",
            "b/foo.rs",
            Language::Rust,
        );
        let ctx = Ctx::default().qualified("Foo::get", vec![a, b]);
        let r = refv("Foo::get", EdgeKind::Calls, "b/foo.rs", Language::Rust, 1);
        let res = match_by_qualified_name(&r, &ctx).expect("ambiguous qualified own file");
        assert_eq!(res.target_node_id, "method:b");
        assert!((res.confidence - 0.95).abs() < 1e-9);
    }

    #[test]
    fn qualified_name_exact_ambiguous_no_own_file_declines_to_partial() {
        // Two exact matches, neither in the call site's file: the exact-ambiguous
        // branch does NOT resolve (own file not first); falls through to partial.
        let a = mk(
            "method:a",
            NodeKind::Method,
            "get",
            "Foo::get",
            "a/foo.rs",
            Language::Rust,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "get",
            "Foo::get",
            "b/foo.rs",
            Language::Rust,
        );
        // partial fallback: get-by-name candidates whose qualified_name ends with ref.
        let ctx = Ctx::default()
            .qualified("Foo::get", vec![a, b.clone()])
            .name("get", vec![b]);
        let r = refv("Foo::get", EdgeKind::Calls, "z/other.rs", Language::Rust, 1);
        let res = match_by_qualified_name(&r, &ctx).expect("partial fallback");
        // Partial path returns first candidate whose qname ends with the ref.
        assert_eq!(res.target_node_id, "method:b");
        assert!((res.confidence - 0.85).abs() < 1e-9);
    }

    #[test]
    fn qualified_name_partial_prefers_call_site_file() {
        // Partial qualified match with two candidates ending in the ref; own file wins.
        let a = mk(
            "method:a",
            NodeKind::Method,
            "get",
            "pkg::Foo::get",
            "a/foo.rs",
            Language::Rust,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "get",
            "pkg::Foo::get",
            "b/foo.rs",
            Language::Rust,
        );
        let ctx = Ctx::default()
            .qualified("Foo::get", vec![])
            .name("get", vec![a, b]);
        let r = refv("Foo::get", EdgeKind::Calls, "b/foo.rs", Language::Rust, 1);
        let res = match_by_qualified_name(&r, &ctx).expect("partial own file");
        assert_eq!(res.target_node_id, "method:b");
    }

    #[test]
    fn method_call_strategy1_prefers_call_site_file() {
        // `Logger.log` — two Logger classes in different files; the call site's own
        // file class is tried first (#1079).
        let class_a = mk(
            "class:a",
            NodeKind::Class,
            "Logger",
            "Logger",
            "a/logger.ts",
            Language::TypeScript,
        );
        let class_b = mk(
            "class:b",
            NodeKind::Class,
            "Logger",
            "Logger",
            "b/logger.ts",
            Language::TypeScript,
        );
        let method_a = mk(
            "method:a",
            NodeKind::Method,
            "log",
            "Logger::log",
            "a/logger.ts",
            Language::TypeScript,
        );
        let method_b = mk(
            "method:b",
            NodeKind::Method,
            "log",
            "Logger::log",
            "b/logger.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default()
            .name("Logger", vec![class_a, class_b])
            .nodes_in_file("a/logger.ts", vec![method_a])
            .nodes_in_file("b/logger.ts", vec![method_b]);
        let r = refv(
            "Logger.log",
            EdgeKind::Calls,
            "b/logger.ts",
            Language::TypeScript,
            5,
        );
        let res = match_method_call(&r, &ctx).expect("strategy1 own file");
        assert_eq!(res.target_node_id, "method:b");
    }

    #[test]
    fn method_call_strategy3_prefers_call_site_file_on_tie() {
        // Two `save` methods with equal receiver-word overlap; the call site's own
        // file wins the tie because prefer_call_site_file reorders it first (#1079).
        let good_a = mk(
            "method:a",
            NodeKind::Method,
            "save",
            "UserRepository::save",
            "a/repo.ts",
            Language::TypeScript,
        );
        let good_b = mk(
            "method:b",
            NodeKind::Method,
            "save",
            "UserRepository::save",
            "b/repo.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("save", vec![good_a, good_b]);
        let r = refv(
            "userRepository.save",
            EdgeKind::Calls,
            "b/repo.ts",
            Language::TypeScript,
            1,
        );
        let res = match_method_call(&r, &ctx).expect("strategy3 tie own file");
        assert_eq!(res.target_node_id, "method:b");
        assert!((res.confidence - 0.65).abs() < 1e-9);
    }

    // ================= Round 5: #1108 local-var receiver inference ============

    /// Build a ctx whose file `path` has `source`, plus an enclosing function
    /// spanning the whole file (so the scope bound includes the call line).
    ///
    /// `method`'s qualified name names the type the receiver is expected to infer
    /// to (`Logger::log` → `Logger`), so that type is also declared imported into
    /// `path` — the binding a real ESM call site carries and the mock otherwise
    /// lacks (#1566).
    fn ctx_with_scope(
        path: &str,
        source: &str,
        lang: Language,
        method: Node,
        method_name: &str,
    ) -> Ctx {
        let mut fn_node = mk(
            "function:scope",
            NodeKind::Function,
            "run",
            "run",
            path,
            lang,
        );
        fn_node.start_line = 1;
        fn_node.end_line = source.split('\n').count() as i64 + 1;
        let owner = method
            .qualified_name
            .rsplit_once("::")
            .map(|(owner, _)| owner.to_string());
        let ctx = Ctx::default()
            .file(path, source)
            .nodes_in_file(path, vec![fn_node])
            .name(method_name, vec![method]);
        match owner {
            Some(owner) => ctx.imports_type(path, &owner),
            None => ctx,
        }
    }

    #[test]
    fn local_var_ts_new_constructor() {
        // TS: `const lg = new Logger(); lg.log()` — infer lg: Logger, resolve log.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = ctx_with_scope(
            "src/a.ts",
            "function run() {\n  const lg = new Logger();\n  lg.log();\n}\n",
            Language::TypeScript,
            m,
            "log",
        );
        let r = refv(
            "lg.log",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            3,
        );
        let res = match_method_call(&r, &ctx).expect("ts new inference");
        assert_eq!(res.target_node_id, "method:log");
        assert_eq!(res.resolved_by, ResolvedBy::InstanceMethod);
        assert!((res.confidence - 0.9).abs() < 1e-9);
    }

    #[test]
    fn local_var_ts_annotation() {
        // TS: `const lg: Logger = get(); lg.log()` — infer from the annotation.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = ctx_with_scope(
            "src/a.ts",
            "function run() {\n  const lg: Logger = get();\n  lg.log();\n}\n",
            Language::TypeScript,
            m,
            "log",
        );
        let r = refv(
            "lg.log",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            3,
        );
        let res = match_method_call(&r, &ctx).expect("ts annotation inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_python_construction() {
        // Python: `lg = Logger()\n lg.log()`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.py",
            Language::Python,
        );
        let ctx = ctx_with_scope(
            "src/a.py",
            "def run():\n    lg = Logger()\n    lg.log()\n",
            Language::Python,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/a.py", Language::Python, 3);
        let res = match_method_call(&r, &ctx).expect("python construction inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_python_annotation() {
        // Python PEP 526: `lg: Logger` param/annotation.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.py",
            Language::Python,
        );
        let ctx = ctx_with_scope(
            "src/a.py",
            "def run(lg: Logger):\n    lg.log()\n",
            Language::Python,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/a.py", Language::Python, 2);
        let res = match_method_call(&r, &ctx).expect("python annotation inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_go_composite_literal() {
        // Go: `lg := Logger{}\n lg.Log()`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "Log",
            "Logger::Log",
            "a.go",
            Language::Go,
        );
        let ctx = ctx_with_scope(
            "a.go",
            "func run() {\n\tlg := Logger{}\n\tlg.Log()\n}\n",
            Language::Go,
            m,
            "Log",
        );
        let r = refv("lg.Log", EdgeKind::Calls, "a.go", Language::Go, 3);
        let res = match_method_call(&r, &ctx).expect("go composite literal inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_go_var_decl() {
        // Go: `var lg *Logger\n lg.Log()`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "Log",
            "Logger::Log",
            "a.go",
            Language::Go,
        );
        let ctx = ctx_with_scope(
            "a.go",
            "func run() {\n\tvar lg *Logger\n\tlg.Log()\n}\n",
            Language::Go,
            m,
            "Log",
        );
        let r = refv("lg.Log", EdgeKind::Calls, "a.go", Language::Go, 3);
        let res = match_method_call(&r, &ctx).expect("go var decl inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_rust_let_new() {
        // Rust: `let logger = Logger::new(); logger.log()`.
        //
        // The confidence assertion is load-bearing (#228). Until the Rust `let`
        // pattern gained its `\s*` before `=`, inference DECLINED on the spaced
        // form and this test passed anyway through Strategy 3's single-candidate
        // branch at 0.7 — green for years while the code path it is named for
        // never ran. Asserting 0.9 + `InstanceMethod` pins the TYPED path, so a
        // regression that pushes resolution back onto any fallback fails here
        // instead of hiding.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.rs",
            Language::Rust,
        );
        let ctx = ctx_with_scope(
            "src/a.rs",
            "fn run() {\n    let logger = Logger::new();\n    logger.log();\n}\n",
            Language::Rust,
            m,
            "log",
        );
        let r = refv("logger.log", EdgeKind::Calls, "src/a.rs", Language::Rust, 3);
        let res = match_method_call(&r, &ctx).expect("rust let new inference");
        assert_eq!(res.target_node_id, "method:log");
        assert_eq!(res.resolved_by, ResolvedBy::InstanceMethod);
        assert!(
            (res.confidence - 0.9).abs() < 1e-9,
            "must resolve through receiver-type inference (0.9), not a fallback; got {}",
            res.confidence
        );
    }

    #[test]
    fn local_var_rust_let_struct_literal() {
        // Rust: `let logger = Logger { .. }; logger.log()`. The 0.9 assertion is
        // load-bearing for the same reason as `local_var_rust_let_new` (#228).
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.rs",
            Language::Rust,
        );
        let ctx = ctx_with_scope(
            "src/a.rs",
            "fn run() {\n    let logger = Logger { level: 0 };\n    logger.log();\n}\n",
            Language::Rust,
            m,
            "log",
        );
        let r = refv("logger.log", EdgeKind::Calls, "src/a.rs", Language::Rust, 3);
        let res = match_method_call(&r, &ctx).expect("rust struct literal inference");
        assert_eq!(res.target_node_id, "method:log");
        assert_eq!(res.resolved_by, ResolvedBy::InstanceMethod);
        assert!(
            (res.confidence - 0.9).abs() < 1e-9,
            "must resolve through receiver-type inference (0.9), not a fallback; got {}",
            res.confidence
        );
    }

    #[test]
    fn local_var_rust_let_spaced_resolves_without_word_overlap() {
        // #228 end-to-end: `let lg = Logger::new(); lg.log()`.
        //
        // The receiver is spelled `lg` deliberately — it shares no
        // `split_camel_case` word with `Logger::log`, so Strategy 3's
        // single-candidate branch refuses it and NOTHING but receiver-type
        // inference can produce this edge. Before the `\s*` fix the whole call
        // resolved to `None`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.rs",
            Language::Rust,
        );
        let ctx = ctx_with_scope(
            "src/a.rs",
            "fn run() {\n    let lg = Logger::new();\n    lg.log();\n}\n",
            Language::Rust,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/a.rs", Language::Rust, 3);
        let res = match_method_call(&r, &ctx).expect("rust spaced let inference");
        assert_eq!(res.target_node_id, "method:log");
        assert_eq!(res.resolved_by, ResolvedBy::InstanceMethod);
        assert!(
            (res.confidence - 0.9).abs() < 1e-9,
            "must be the typed inference path (0.9); got {}",
            res.confidence
        );
    }

    #[test]
    fn infer_local_receiver_type_rust_let_spacing_matrix() {
        // #228 — the Rust `let` pattern's optional annotation group
        // `(?:\s*:[^=]+)?` absorbed the whitespace before `=` ONLY when an
        // annotation was present, so the idiomatic spaced form never matched.
        // 3,318 spaced occurrences in this repository against 0 unspaced ones:
        // Rust local-variable inference was effectively dead.
        //
        // Receiver `lg` shares no word with `Logger`, and this calls the
        // inferrer DIRECTLY, so no downstream fallback can mask a decline. The
        // negatives are bundled into the same test on purpose: they must not
        // become independently green, because a guard that passes with the fix
        // reverted proves nothing about the fix.
        let build = |decl: &str, receiver: &str| -> (String, Ctx) {
            let source = format!("fn run() {{\n    {decl}\n    {receiver}.log();\n}}\n");
            let mut fn_node = mk(
                "function:scope",
                NodeKind::Function,
                "run",
                "run",
                "src/a.rs",
                Language::Rust,
            );
            fn_node.start_line = 1;
            fn_node.end_line = 4;
            let ctx = Ctx::default()
                .file("src/a.rs", &source)
                .nodes_in_file("src/a.rs", vec![fn_node]);
            (source, ctx)
        };

        for (label, decl) in [
            ("spaced", "let lg = Logger::new();"),
            ("unspaced", "let lg=Logger::new();"),
            ("annotated", "let lg: Logger = Logger::new();"),
            ("mut spaced", "let mut lg = Logger::new();"),
            ("mut annotated", "let mut lg: Logger = Logger::new();"),
            ("struct literal", "let lg = Logger { level: 0 };"),
            ("borrowed", "let lg = &Logger::new();"),
            ("borrowed mut", "let lg = &mut Logger::new();"),
            ("wide gap", "let lg\t=   Logger::new();"),
            ("generic type", "let lg = Logger::<u8>::new();"),
            ("annotated generic", "let lg: Logger<u8> = Logger::new();"),
        ] {
            let (_, ctx) = build(decl, "lg");
            let r = refv("lg.log", EdgeKind::Calls, "src/a.rs", Language::Rust, 3);
            assert_eq!(
                infer_local_receiver_type("lg", &r, &ctx).as_deref(),
                Some("Logger"),
                "{label}: `{decl}` must infer Logger"
            );
        }

        // The added `\s*` must not widen the pattern. A destructuring binding
        // does not declare `lg` as a whole value, a bare re-assignment is not a
        // declaration at all, and a lowercase initializer is not a type.
        for (label, decl, receiver) in [
            (
                "tuple destructure",
                "let (lg, other) = Logger::pair();",
                "lg",
            ),
            ("struct destructure", "let Wrapper { lg } = make();", "lg"),
            ("bare reassignment", "lg = Logger::new();", "lg"),
            ("lowercase initializer", "let lg = logger_new();", "lg"),
            ("other variable", "let other = Logger::new();", "lg"),
        ] {
            let (_, ctx) = build(decl, receiver);
            let r = refv("lg.log", EdgeKind::Calls, "src/a.rs", Language::Rust, 3);
            assert_eq!(
                infer_local_receiver_type(receiver, &r, &ctx),
                None,
                "{label}: `{decl}` must NOT infer a type"
            );
        }
    }

    #[test]
    fn infer_local_receiver_type_rust_let_shadow_takes_nearest_decl() {
        // #228 — with both bindings now matchable, the backward scan must report
        // the one NEAREST the call. A shadowed earlier binding leaking its type
        // downstream would be a false inference, which is strictly worse than
        // the missing edge this fix removes.
        let source = "fn run() {\n    let cfg = Logger::new();\n    let cfg = Writer::new();\n    cfg.log();\n}\n";
        let mut fn_node = mk(
            "function:scope",
            NodeKind::Function,
            "run",
            "run",
            "src/a.rs",
            Language::Rust,
        );
        fn_node.start_line = 1;
        fn_node.end_line = 5;
        let ctx = Ctx::default()
            .file("src/a.rs", source)
            .nodes_in_file("src/a.rs", vec![fn_node]);
        let r = refv("cfg.log", EdgeKind::Calls, "src/a.rs", Language::Rust, 4);
        assert_eq!(
            infer_local_receiver_type("cfg", &r, &ctx).as_deref(),
            Some("Writer")
        );
    }

    #[test]
    fn local_var_java_new() {
        // Java: `Logger lg = new Logger(); lg.log()`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/A.java",
            Language::Java,
        );
        let ctx = ctx_with_scope(
            "src/A.java",
            "void run() {\n    Logger lg = new Logger();\n    lg.log();\n}\n",
            Language::Java,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/A.java", Language::Java, 3);
        let res = match_method_call(&r, &ctx).expect("java new inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_csharp_new() {
        // C#: `var lg = new Logger(); lg.Log()`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "Log",
            "Logger::Log",
            "src/A.cs",
            Language::CSharp,
        );
        let ctx = ctx_with_scope(
            "src/A.cs",
            "void Run() {\n    var lg = new Logger();\n    lg.Log();\n}\n",
            Language::CSharp,
            m,
            "Log",
        );
        let r = refv("lg.Log", EdgeKind::Calls, "src/A.cs", Language::CSharp, 3);
        let res = match_method_call(&r, &ctx).expect("csharp new inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_kotlin_construction() {
        // Kotlin: `val lg = Logger()\n lg.log()`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/A.kt",
            Language::Kotlin,
        );
        let ctx = ctx_with_scope(
            "src/A.kt",
            "fun run() {\n    val lg = Logger()\n    lg.log()\n}\n",
            Language::Kotlin,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/A.kt", Language::Kotlin, 3);
        let res = match_method_call(&r, &ctx).expect("kotlin construction inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_swift_construction() {
        // Swift: `let lg = Logger()\n lg.log()`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/A.swift",
            Language::Swift,
        );
        let ctx = ctx_with_scope(
            "src/A.swift",
            "func run() {\n    let lg = Logger()\n    lg.log()\n}\n",
            Language::Swift,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/A.swift", Language::Swift, 3);
        let res = match_method_call(&r, &ctx).expect("swift construction inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_scala_new() {
        // Scala: `val lg = new Logger\n lg.log()`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/A.scala",
            Language::Scala,
        );
        let ctx = ctx_with_scope(
            "src/A.scala",
            "def run() = {\n  val lg = new Logger\n  lg.log()\n}\n",
            Language::Scala,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/A.scala", Language::Scala, 3);
        let res = match_method_call(&r, &ctx).expect("scala new inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_dart_construction() {
        // Dart: `var lg = Logger();\n lg.log()`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.dart",
            Language::Dart,
        );
        let ctx = ctx_with_scope(
            "src/a.dart",
            "void run() {\n  var lg = Logger();\n  lg.log();\n}\n",
            Language::Dart,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/a.dart", Language::Dart, 3);
        let res = match_method_call(&r, &ctx).expect("dart construction inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn local_var_php_new() {
        // PHP: `$lg = new Logger(); $lg->log()` — ref name is `lg.log`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.php",
            Language::Php,
        );
        let ctx = ctx_with_scope(
            "src/a.php",
            "function run() {\n    $lg = new Logger();\n    $lg->log();\n}\n",
            Language::Php,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/a.php", Language::Php, 3);
        let res = match_method_call(&r, &ctx).expect("php new inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    // ============ #1220 PHP `$this->prop->method()` property resolution ========

    fn php_prop_method(qname: &str) -> Node {
        mk(
            "method:handle",
            NodeKind::Method,
            "handle",
            qname,
            "src/svc.php",
            Language::Php,
        )
    }

    fn php_prop_ctx(source: &str, method: Node) -> Ctx {
        Ctx::default()
            .file("src/svc.php", source)
            .name("handle", vec![method])
    }

    #[test]
    fn php_property_receiver_shape_matches() {
        assert!(is_php_property_receiver_shape("this->dep.method"));
        assert!(!is_php_property_receiver_shape("this->a->b.method"));
        assert!(!is_php_property_receiver_shape("dep.method"));
        assert!(!is_php_property_receiver_shape("this->dep"));
        assert!(!is_php_property_receiver_shape("Foo::bar"));
    }

    #[test]
    fn infer_php_property_typed() {
        let src = "<?php\nclass C {\n    private Foo $dep;\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let ctx = Ctx::default().file("src/svc.php", src);
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            5,
        );
        assert_eq!(
            infer_php_property_receiver_type("this->dep", &r, &ctx),
            Some("Foo".to_string())
        );
    }

    #[test]
    fn infer_php_property_promoted_ctor() {
        let src = "<?php\nclass C {\n    function __construct(private Foo $dep) {}\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let ctx = Ctx::default().file("src/svc.php", src);
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            5,
        );
        assert_eq!(
            infer_php_property_receiver_type("this->dep", &r, &ctx),
            Some("Foo".to_string())
        );
    }

    #[test]
    fn infer_php_property_pseudoctor_new() {
        let src = "<?php\nclass C {\n    function __construct() {\n        $this->dep = new Foo();\n    }\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let ctx = Ctx::default().file("src/svc.php", src);
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            7,
        );
        assert_eq!(
            infer_php_property_receiver_type("this->dep", &r, &ctx),
            Some("Foo".to_string())
        );
    }

    #[test]
    fn infer_php_property_union_untypable() {
        let src = "<?php\nclass C {\n    private Foo|Bar $dep;\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let ctx = Ctx::default().file("src/svc.php", src);
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            5,
        );
        assert_eq!(
            infer_php_property_receiver_type("this->dep", &r, &ctx),
            None
        );
    }

    #[test]
    fn infer_php_assigned_property_classic_ctor() {
        let src = "<?php\nclass C {\n    private $dep;\n    function __construct(Foo $dep) {\n        $this->dep = $dep;\n    }\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let ctx = Ctx::default().file("src/svc.php", src);
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            8,
        );
        assert_eq!(
            infer_php_property_receiver_type("this->dep", &r, &ctx),
            Some("Foo".to_string())
        );
    }

    #[test]
    fn php_this_prop_typed_property_resolves() {
        let src = "<?php\nclass Svc {\n    private Foo $dep;\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let ctx = php_prop_ctx(src, php_prop_method("Foo::handle"));
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            5,
        );
        let res = match_method_call(&r, &ctx).expect("php typed property resolves");
        assert_eq!(res.target_node_id, "method:handle");
        assert_eq!(res.resolved_by, ResolvedBy::InstanceMethod);
        assert!((res.confidence - 0.9).abs() < 1e-9);
    }

    #[test]
    fn php_this_prop_promoted_ctor_resolves() {
        let src = "<?php\nclass Svc {\n    function __construct(private Foo $dep) {}\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let ctx = php_prop_ctx(src, php_prop_method("Foo::handle"));
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            5,
        );
        let res = match_method_call(&r, &ctx).expect("php promoted ctor resolves");
        assert_eq!(res.target_node_id, "method:handle");
    }

    #[test]
    fn php_this_prop_classic_ctor_assignment_resolves() {
        let src = "<?php\nclass Svc {\n    private $dep;\n    function __construct(\n        Foo $dep\n    ) {\n        $this->dep = $dep;\n    }\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let ctx = php_prop_ctx(src, php_prop_method("Foo::handle"));
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            10,
        );
        let res = match_method_call(&r, &ctx).expect("php classic ctor assignment resolves");
        assert_eq!(res.target_node_id, "method:handle");
    }

    #[test]
    fn php_this_prop_typed_setter_injection_resolves() {
        let src = "<?php\nclass Svc {\n    private $dep;\n    function setDep(Foo $dep) {\n        $this->dep = $dep;\n    }\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let ctx = php_prop_ctx(src, php_prop_method("Foo::handle"));
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            8,
        );
        let res = match_method_call(&r, &ctx).expect("php typed setter injection resolves");
        assert_eq!(res.target_node_id, "method:handle");
    }

    #[test]
    fn php_this_prop_interface_typed_resolves_via_supertype() {
        let src = "<?php\nclass Svc {\n    private LoggerInterface $dep;\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let method = mk(
            "method:handle",
            NodeKind::Method,
            "handle",
            "LoggerInterface::handle",
            "src/iface.php",
            Language::Php,
        );
        let ctx = Ctx::default()
            .file("src/svc.php", src)
            .name("handle", vec![method])
            .supertype("LoggerInterface", Language::Php, vec!["LoggerInterface"]);
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            5,
        );
        let res = match_method_call(&r, &ctx).expect("php interface-typed resolves");
        assert_eq!(res.target_node_id, "method:handle");
    }

    #[test]
    fn php_this_prop_inherited_method_resolves() {
        let src = "<?php\nclass Svc {\n    private Child $dep;\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let method = mk(
            "method:handle",
            NodeKind::Method,
            "handle",
            "Parent::handle",
            "src/parent.php",
            Language::Php,
        );
        let ctx = Ctx::default()
            .file("src/svc.php", src)
            .name("handle", vec![method])
            .supertype("Child", Language::Php, vec!["Parent"]);
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            5,
        );
        let res = match_method_call(&r, &ctx).expect("php inherited method resolves");
        assert_eq!(res.target_node_id, "method:handle");
    }

    #[test]
    fn php_this_prop_same_named_local_does_not_mistype() {
        let src = "<?php\nclass Svc {\n    private Foo $dep;\n    function other() {\n        $dep = new Wrong();\n        $dep->handle();\n    }\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let foo = mk(
            "method:foo_handle",
            NodeKind::Method,
            "handle",
            "Foo::handle",
            "src/svc.php",
            Language::Php,
        );
        let wrong = mk(
            "method:wrong_handle",
            NodeKind::Method,
            "handle",
            "Wrong::handle",
            "src/svc.php",
            Language::Php,
        );
        let ctx = Ctx::default()
            .file("src/svc.php", src)
            .name("handle", vec![foo, wrong]);
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            9,
        );
        let res = match_method_call(&r, &ctx).expect("php property typed, not the local");
        assert_eq!(res.target_node_id, "method:foo_handle");
    }

    #[test]
    fn php_this_prop_untypable_stays_unlinked() {
        let src =
            "<?php\nclass Svc {\n    function run() {\n        $this->dep->handle();\n    }\n}\n";
        let ctx = php_prop_ctx(src, php_prop_method("Foo::handle"));
        let r = refv(
            "this->dep.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            4,
        );
        assert!(match_method_call(&r, &ctx).is_none());
    }

    #[test]
    fn php_this_prop_deep_chain_stays_unlinked() {
        let src = "<?php\nclass Svc {\n    private Foo $a;\n    function run() {\n        $this->a->b->handle();\n    }\n}\n";
        let ctx = php_prop_ctx(src, php_prop_method("Foo::handle"));
        let r = refv(
            "this->a->b.handle",
            EdgeKind::Calls,
            "src/svc.php",
            Language::Php,
            5,
        );
        assert!(match_method_call(&r, &ctx).is_none());
    }

    #[test]
    fn local_var_ruby_new() {
        // Ruby: `lg = Logger.new\n lg.log` (uses R4 receiver.method extraction).
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.rb",
            Language::Ruby,
        );
        let ctx = ctx_with_scope(
            "src/a.rb",
            "def run\n  lg = Logger.new\n  lg.log\nend\n",
            Language::Ruby,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/a.rb", Language::Ruby, 3);
        let res = match_method_call(&r, &ctx).expect("ruby new inference");
        assert_eq!(res.target_node_id, "method:log");
    }

    // ================= Round 5: #1125 typed-parameter inference ===============

    #[test]
    fn typed_param_ts_generic_outer_type() {
        // TS generic param: `function use(repo: Repository<User>) { repo.find() }`
        // — infer outer type Repository.
        let m = mk(
            "method:find",
            NodeKind::Method,
            "find",
            "Repository::find",
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = ctx_with_scope(
            "src/a.ts",
            "function use(repo: Repository<User>) {\n  repo.find();\n}\n",
            Language::TypeScript,
            m,
            "find",
        );
        let r = refv(
            "repo.find",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            2,
        );
        let res = match_method_call(&r, &ctx).expect("ts typed param generic");
        assert_eq!(res.target_node_id, "method:find");
    }

    #[test]
    fn typed_param_rust_ref() {
        // Rust: `fn use(lg: &Logger) { lg.log() }` — strip `&`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.rs",
            Language::Rust,
        );
        let ctx = ctx_with_scope(
            "src/a.rs",
            "fn r(lg: &Logger) {\n    lg.log();\n}\n",
            Language::Rust,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/a.rs", Language::Rust, 2);
        let res = match_method_call(&r, &ctx).expect("rust typed ref param");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn typed_param_go_receiver() {
        // Go: `func use(lg Logger) { lg.Log() }`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "Log",
            "Logger::Log",
            "a.go",
            Language::Go,
        );
        let ctx = ctx_with_scope(
            "a.go",
            "func use(lg Logger) {\n\tlg.Log()\n}\n",
            Language::Go,
            m,
            "Log",
        );
        let r = refv("lg.Log", EdgeKind::Calls, "a.go", Language::Go, 2);
        let res = match_method_call(&r, &ctx).expect("go typed param");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn typed_param_dart() {
        // Dart: `void use(Logger lg) { lg.log() }`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.dart",
            Language::Dart,
        );
        let ctx = ctx_with_scope(
            "src/a.dart",
            "void use(Logger lg) {\n  lg.log();\n}\n",
            Language::Dart,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/a.dart", Language::Dart, 2);
        let res = match_method_call(&r, &ctx).expect("dart typed param");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn typed_param_php() {
        // PHP: `function use(Logger $lg) { $lg->log() }`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.php",
            Language::Php,
        );
        let ctx = ctx_with_scope(
            "src/a.php",
            "function use(Logger $lg) {\n    $lg->log();\n}\n",
            Language::Php,
            m,
            "log",
        );
        let r = refv("lg.log", EdgeKind::Calls, "src/a.php", Language::Php, 2);
        let res = match_method_call(&r, &ctx).expect("php typed param");
        assert_eq!(res.target_node_id, "method:log");
    }

    // ================= Round 5: #1112 separators (Lua : / R $ / Pascal .) =====

    #[test]
    fn separator_lua_colon() {
        // Lua: `local lg = Logger.new()\n lg:log()` — ref name is `lg:log`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "a.lua",
            Language::Lua,
        );
        let ctx = ctx_with_scope(
            "a.lua",
            "function run()\n  local lg = Logger.new()\n  lg:log()\nend\n",
            Language::Lua,
            m,
            "log",
        );
        let r = refv("lg:log", EdgeKind::Calls, "a.lua", Language::Lua, 3);
        let res = match_method_call(&r, &ctx).expect("lua colon separator");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn lua_static_table_member_resolves_for_dot_and_colon_calls() {
        let member = mk(
            "method:assigned",
            NodeKind::Method,
            "assignedFn",
            "M::assignedFn",
            "a.lua",
            Language::Lua,
        );
        let ctx = Ctx::default().name("assignedFn", vec![member]);
        for reference_name in ["M.assignedFn", "M:assignedFn"] {
            let reference = refv(reference_name, EdgeKind::Calls, "a.lua", Language::Lua, 3);
            let resolved = match_method_call(&reference, &ctx).expect("static table method");
            assert_eq!(resolved.target_node_id, "method:assigned");
        }
    }

    #[test]
    fn lua_capitalized_method_call_not_self_matched() {
        // #1124: `lg:Log()` is byte-identical to a Luau annotation `lg: Log`; the
        // call line must NOT self-match `type=Log`. The scan continues to the real
        // `local lg = Logger.new()` declaration and resolves to Logger::Log.
        let m = mk(
            "method:Log",
            NodeKind::Method,
            "Log",
            "Logger::Log",
            "a.lua",
            Language::Lua,
        );
        let ctx = ctx_with_scope(
            "a.lua",
            "function run()\n  local lg = Logger.new()\n  lg:Log()\nend\n",
            Language::Lua,
            m,
            "Log",
        );
        let r = refv("lg:Log", EdgeKind::Calls, "a.lua", Language::Lua, 3);
        let res = match_method_call(&r, &ctx).expect("lua PascalCase call resolves");
        assert_eq!(res.target_node_id, "method:Log");
    }

    #[test]
    fn lua_annotation_still_resolves() {
        let m = mk(
            "method:Log",
            NodeKind::Method,
            "Log",
            "Logger::Log",
            "a.luau",
            Language::Luau,
        );
        let ctx = ctx_with_scope(
            "a.luau",
            "function run()\n  local lg: Logger\n  lg:Log()\nend\n",
            Language::Luau,
            m,
            "Log",
        );
        let r = refv("lg:Log", EdgeKind::Calls, "a.luau", Language::Luau, 3);
        let res = match_method_call(&r, &ctx).expect("lua annotation resolves");
        assert_eq!(res.target_node_id, "method:Log");
    }

    #[test]
    fn luau_typed_param_annotation_preserved() {
        let m = mk(
            "method:Log",
            NodeKind::Method,
            "Log",
            "Logger::Log",
            "a.luau",
            Language::Luau,
        );
        let ctx = ctx_with_scope(
            "a.luau",
            "function f(lg: Logger)\n  lg:Log()\nend\n",
            Language::Luau,
            m,
            "Log",
        );
        let r = refv("lg:Log", EdgeKind::Calls, "a.luau", Language::Luau, 2);
        let res = match_method_call(&r, &ctx).expect("luau typed param resolves");
        assert_eq!(res.target_node_id, "method:Log");
    }

    #[test]
    fn separator_r_dollar() {
        // R: `lg <- Logger$new()\n lg$log()` — ref name is `lg$log`.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "a.R",
            Language::R,
        );
        let ctx = ctx_with_scope(
            "a.R",
            "run <- function() {\n  lg <- Logger$new()\n  lg$log()\n}\n",
            Language::R,
            m,
            "log",
        );
        let r = refv("lg$log", EdgeKind::Calls, "a.R", Language::R, 3);
        let res = match_method_call(&r, &ctx).expect("r dollar separator");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn separator_pascal_dot() {
        // Pascal: `var lg: TLogger; lg.Log()` — uses the `.` dot separator.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "Log",
            "TLogger::Log",
            "a.pas",
            Language::Pascal,
        );
        let ctx = ctx_with_scope(
            "a.pas",
            "procedure run;\nvar lg: TLogger;\nbegin\n  lg.Log();\nend;\n",
            Language::Pascal,
            m,
            "Log",
        );
        let r = refv("lg.Log", EdgeKind::Calls, "a.pas", Language::Pascal, 4);
        let res = match_method_call(&r, &ctx).expect("pascal dot separator");
        assert_eq!(res.target_node_id, "method:log");
    }

    #[test]
    fn separator_lua_colon_only_for_lua() {
        // A `lg:log` ref in a non-Lua language is not a method-call shape → declines.
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "a.rs",
            Language::Rust,
        );
        let ctx = Ctx::default().name("log", vec![m]);
        let r = refv("lg:log", EdgeKind::Calls, "a.rs", Language::Rust, 1);
        assert!(match_method_call(&r, &ctx).is_none());
    }

    #[test]
    fn separator_r_dollar_only_for_r() {
        // A `lg$log` ref outside R is not a call shape → declines.
        let ctx = Ctx::default();
        let r = refv("lg$log", EdgeKind::Calls, "a.ts", Language::TypeScript, 1);
        assert!(match_method_call(&r, &ctx).is_none());
    }

    // ================= Round 5: negative cases (no wrong edge) ================

    #[test]
    fn local_var_wrong_type_stays_silent() {
        // `lg = new Widget()` but the method `log` exists only on Logger — the
        // inferred type Widget has no `log`, and there is no Logger receiver word
        // overlap, so no edge is produced (validated-inference invariant).
        let m = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = ctx_with_scope(
            "src/a.ts",
            "function run() {\n  const lg = new Widget();\n  lg.log();\n}\n",
            Language::TypeScript,
            m,
            "log",
        );
        // Only ONE method named log exists → strategy3 single-candidate would
        // resolve it. To make this a true negative, add a second same-lang method
        // so strategy3 needs >=2 overlap which the receiver `lg` can't provide.
        let m2 = mk(
            "method:other",
            NodeKind::Method,
            "log",
            "Other::log",
            "src/b.ts",
            Language::TypeScript,
        );
        let ctx = ctx.name(
            "log",
            vec![
                mk(
                    "method:log",
                    NodeKind::Method,
                    "log",
                    "Logger::log",
                    "src/a.ts",
                    Language::TypeScript,
                ),
                m2,
            ],
        );
        let r = refv(
            "lg.log",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            3,
        );
        // Inference yields Widget → resolve_method_on_type(Widget, log) finds no
        // Widget::log → falls to strategies; two `log` methods, `lg` overlaps
        // neither by >=2 words → declines.
        assert!(match_method_call(&r, &ctx).is_none());
    }

    #[test]
    fn local_var_no_declaration_no_inference() {
        // No local declaration for `lg`; with two ambiguous same-named methods and
        // no overlap, inference finds nothing and strategy 3 declines.
        let a = mk(
            "method:a",
            NodeKind::Method,
            "log",
            "Alpha::log",
            "src/a.ts",
            Language::TypeScript,
        );
        let b = mk(
            "method:b",
            NodeKind::Method,
            "log",
            "Beta::log",
            "src/b.ts",
            Language::TypeScript,
        );
        let ctx = ctx_with_scope(
            "src/a.ts",
            "function run() {\n  lg.log();\n}\n",
            Language::TypeScript,
            a,
            "log",
        )
        .name(
            "log",
            vec![
                mk(
                    "method:a",
                    NodeKind::Method,
                    "log",
                    "Alpha::log",
                    "src/a.ts",
                    Language::TypeScript,
                ),
                b,
            ],
        );
        let r = refv(
            "lg.log",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            2,
        );
        assert!(match_method_call(&r, &ctx).is_none());
    }

    #[test]
    fn local_var_inference_bounded_to_enclosing_scope() {
        // A `Logger` decl in function A must NOT leak into function B's call.
        // Function B (lines 5-7) contains the call; the decl is in A (line 2).
        let logger_log = mk(
            "method:log",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.ts",
            Language::TypeScript,
        );
        let widget_log = mk(
            "method:wlog",
            NodeKind::Method,
            "log",
            "Widget::log",
            "src/w.ts",
            Language::TypeScript,
        );
        let mut fn_a = mk(
            "function:a",
            NodeKind::Function,
            "a",
            "a",
            "src/a.ts",
            Language::TypeScript,
        );
        fn_a.start_line = 1;
        fn_a.end_line = 3;
        let mut fn_b = mk(
            "function:b",
            NodeKind::Function,
            "b",
            "b",
            "src/a.ts",
            Language::TypeScript,
        );
        fn_b.start_line = 5;
        fn_b.end_line = 7;
        let source =
            "function a() {\n  const lg = new Logger();\n}\n\nfunction b() {\n  lg.log();\n}\n";
        let ctx = Ctx::default()
            .file("src/a.ts", source)
            .nodes_in_file("src/a.ts", vec![fn_a, fn_b])
            .name("log", vec![logger_log, widget_log]);
        let r = refv(
            "lg.log",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            6,
        );
        // In B's scope there is no `lg` decl → inference finds nothing; two `log`
        // methods → strategy3 needs overlap `lg` can't give → declines.
        assert!(match_method_call(&r, &ctx).is_none());
    }

    // ================= Round 5: branch coverage for helpers ===================

    #[test]
    fn non_type_receiver_token_rejects_keywords() {
        assert!(is_non_type_receiver_token("this"));
        assert!(is_non_type_receiver_token("self"));
        assert!(is_non_type_receiver_token("undefined"));
        assert!(!is_non_type_receiver_token("Logger"));
    }

    #[test]
    fn normalize_inferred_type_name_strips_and_rejects() {
        // Generics + pointer markers stripped, last segment kept.
        assert_eq!(
            normalize_inferred_type_name("Repository<User>").as_deref(),
            Some("Repository")
        );
        assert_eq!(
            normalize_inferred_type_name("pkg.mod.Logger").as_deref(),
            Some("Logger")
        );
        assert_eq!(
            normalize_inferred_type_name("&Logger").as_deref(),
            Some("Logger")
        );
        // A non-type keyword is rejected.
        assert_eq!(normalize_inferred_type_name("self"), None);
        // An empty capture yields None.
        assert_eq!(normalize_inferred_type_name(""), None);
    }

    #[test]
    fn local_receiver_type_patterns_empty_for_unpatterned_language() {
        // C has no shared patterns (C++ uses its own inferrer) → empty;
        // Objective-C has its declaration and parameter forms (upstream
        // #2150).
        assert!(local_receiver_type_patterns(Language::C, "lg").is_empty());
        assert_eq!(local_receiver_type_patterns(Language::ObjC, "lg").len(), 2);
        assert!(!local_receiver_type_patterns(Language::TypeScript, "lg").is_empty());
    }

    #[test]
    fn infer_local_receiver_type_none_when_no_patterns() {
        // C has no patterns → infer returns None regardless of source.
        let ctx = Ctx::default().file("a.c", "int f() { Logger lg; lg.log(); }\n");
        let r = refv("lg.log", EdgeKind::Calls, "a.c", Language::C, 1);
        assert!(infer_local_receiver_type("lg", &r, &ctx).is_none());
    }

    #[test]
    fn infer_local_receiver_type_none_when_file_unreadable() {
        // TS has patterns but the file can't be read → None.
        let ctx = Ctx::default();
        let r = refv(
            "lg.log",
            EdgeKind::Calls,
            "missing.ts",
            Language::TypeScript,
            1,
        );
        assert!(infer_local_receiver_type("lg", &r, &ctx).is_none());
    }

    #[test]
    fn infer_local_receiver_type_none_when_no_declaration() {
        // Readable source but no matching declaration for `lg` → None.
        let ctx = Ctx::default().file("a.ts", "function f() {\n  lg.log();\n}\n");
        let r = refv("lg.log", EdgeKind::Calls, "a.ts", Language::TypeScript, 2);
        assert!(infer_local_receiver_type("lg", &r, &ctx).is_none());
    }

    #[test]
    fn cfml_line_prefilter_admits_every_line_a_receiver_pattern_matches() {
        let lines = [
            "    var svc = new UserService();",
            "<cfset svc = new app.models.UserService()>",
            "    svc = createObject(\"component\", \"svc.UserService\");",
            "    svc=CreateObject('svc.OrderService')",
            "  function process(required UserService svc) {",
            "  function process(UserService svc, any other) {",
            "    UserService svc;",
            "  <cfargument name=\"svc\" type=\"svc.UserService\">",
            "  <CFARGUMENT type='UserService' NAME='SVC'>",
            "  property name=\"svc\" inject=\"UserService\";",
            "  <cfproperty type=\"UserService\" name=\"svc\">",
            "    return svc.save(1);",
            "    svc.save(1);",
            "    n50.save(svc);",
            "    var other = svc;",
        ];
        let patterns = local_receiver_type_patterns_tagged(Language::Cfml, "svc");
        let mut declaring = 0;
        for line in lines {
            let matched = patterns.iter().any(|(re, check)| {
                re.captures(line).is_some_and(|caps| {
                    matches!((caps.get(0), caps.get(1)), (Some(whole), Some(m)) if check.passes(line, whole, m))
                })
            });
            if matched {
                declaring += 1;
                assert!(cfml_line_may_declare(line, "svc"), "{line}");
            }
        }
        assert_eq!(declaring, 11);
        // A plain member call declares nothing, so no pattern is compiled
        // for it.
        assert!(!cfml_line_may_declare("    svc.save(1);", "svc"));
        assert!(!cfml_line_may_declare("    n50.save(50);", "n5"));
        assert!(cfml_line_may_declare("    aaa = new X();", "aa"));
    }

    #[test]
    fn php_line_prefilters_admit_every_line_a_pattern_matches() {
        let lines = [
            "        $repo = new UserRepository();",
            "        $repo=new \\App\\Repo;",
            "        repo  =  new  Repo();",
            "    public function handle(UserRepository $repo) {",
            "    public function handle(Repo &$repo, $other) {",
            "    private readonly ?Repo $repo;",
            "    public function __construct(protected Repo $repo) {}",
            "        $this->repo = new Repo();",
            "        $this->repo = $repo;",
            "        $this->repo->save();",
            "        $repo->save();",
            "        if ($repo == new Repo()) {}",
            "        $repos = new Repo();",
        ];
        let receiver = local_receiver_type_patterns_tagged(Language::Php, "repo");
        let property = php_property_type_patterns("repo");
        let assignment = Regex::new(r"\$this->repo\b\s*=\s*\$(\w+)\b").unwrap();
        let (mut by_receiver, mut by_property, mut by_assignment) = (0, 0, 0);
        for line in lines {
            if receiver.iter().any(|(re, _)| re.is_match(line)) {
                by_receiver += 1;
                assert!(line_may_declare(Language::Php, line, "repo"), "{line}");
            }
            if property.iter().any(|re| re.is_match(line)) {
                by_property += 1;
                assert!(
                    writes_typed_variable(line, "repo")
                        || writes_assignment_of(line, "this->repo", "new"),
                    "{line}"
                );
            }
            if assignment.is_match(line) {
                by_assignment += 1;
                assert!(writes_assignment_of(line, "this->repo", "$"), "{line}");
            }
        }
        assert_eq!((by_receiver, by_property, by_assignment), (8, 3, 1));
        // A plain member call declares nothing, so no pattern is compiled
        // for it.
        assert!(!line_may_declare(
            Language::Php,
            "        $repo->save();",
            "repo"
        ));
        assert!(!writes_assignment_of(
            "        $this->repo->save();",
            "this->repo",
            "new"
        ));
        assert!(!writes_typed_variable(
            "        $this->repo->save();",
            "repo"
        ));
        assert!(writes_assignment_of("    aaa = new X();", "aa", "new"));
    }

    #[test]
    fn dart_line_prefilter_admits_every_line_a_pattern_matches() {
        let lines = [
            "    final repo = Repo();",
            "    var repo=Repo.named();",
            "    Repo repo;",
            "    Repo repo = makeRepo();",
            "  void go(Repo repo, int n) {",
            "  void go(Repo repo) {",
            "    repo\u{a0}= Repo();",
            "    repo.save();",
            "    if (repo == Repo()) {}",
            "    final repos = Repo();",
            "    expect(repo, isNotNull);",
        ];
        let patterns = local_receiver_type_patterns_tagged(Language::Dart, "repo");
        assert_eq!(patterns.len(), 2);
        let mut matched = 0;
        for line in lines {
            if patterns.iter().any(|(re, _)| re.is_match(line)) {
                matched += 1;
                assert!(line_may_declare(Language::Dart, line, "repo"), "{line}");
            }
        }
        assert_eq!(matched, 7);
        // A plain member call declares nothing, so no pattern is compiled
        // for it.
        assert!(!line_may_declare(
            Language::Dart,
            "    repo.save();",
            "repo"
        ));
        assert!(writes_before_terminator("    aaa = X();", "aa"));
    }

    #[test]
    fn infer_local_receiver_type_skips_minified_line() {
        // A >10k-char line is skipped rather than scanned; the real decl on the
        // next line up is still found.
        let long = "x".repeat(10_050);
        let source =
            format!("function f() {{\n  const lg = new Logger();\n  {long}\n  lg.log();\n}}\n");
        let mut fn_node = mk(
            "function:s",
            NodeKind::Function,
            "f",
            "f",
            "a.ts",
            Language::TypeScript,
        );
        fn_node.start_line = 1;
        fn_node.end_line = 6;
        let ctx = Ctx::default()
            .file("a.ts", &source)
            .nodes_in_file("a.ts", vec![fn_node]);
        let r = refv("lg.log", EdgeKind::Calls, "a.ts", Language::TypeScript, 4);
        assert_eq!(
            infer_local_receiver_type("lg", &r, &ctx).as_deref(),
            Some("Logger")
        );
    }

    #[test]
    fn enclosing_scope_start_line_defaults_and_tightens() {
        // No enclosing function → default 1. With one enclosing method spanning
        // the call, its start line is returned.
        let ctx = Ctx::default();
        let r = refv("lg.log", EdgeKind::Calls, "a.ts", Language::TypeScript, 5);
        assert_eq!(enclosing_scope_start_line(&r, &ctx), 1);

        let mut m = mk(
            "method:x",
            NodeKind::Method,
            "run",
            "C::run",
            "a.ts",
            Language::TypeScript,
        );
        m.start_line = 3;
        m.end_line = 9;
        // A function in another language is ignored.
        let mut other = mk(
            "function:o",
            NodeKind::Function,
            "o",
            "o",
            "a.ts",
            Language::Python,
        );
        other.start_line = 1;
        other.end_line = 20;
        let ctx = Ctx::default().nodes_in_file("a.ts", vec![m, other]);
        assert_eq!(enclosing_scope_start_line(&r, &ctx), 3);
    }

    #[test]
    fn enclosing_scope_start_line_uses_start_when_end_line_zero() {
        // A node with end_line == 0 falls back to start_line for its span; a
        // single-line function whose start == the call line still encloses it.
        let mut m = mk(
            "method:x",
            NodeKind::Method,
            "run",
            "C::run",
            "a.ts",
            Language::TypeScript,
        );
        m.start_line = 5;
        m.end_line = 0;
        let ctx = Ctx::default().nodes_in_file("a.ts", vec![m]);
        let r = refv("lg.log", EdgeKind::Calls, "a.ts", Language::TypeScript, 5);
        assert_eq!(enclosing_scope_start_line(&r, &ctx), 5);
    }

    #[test]
    fn lua_and_r_separator_reject_bad_shapes() {
        // Empty receiver / method reject.
        assert_eq!(match_lua_colon_call(":log"), None);
        assert_eq!(match_lua_colon_call("lg:"), None);
        // Illegal receiver char.
        assert_eq!(match_lua_colon_call("l-g:log"), None);
        // Non-word method.
        assert_eq!(match_lua_colon_call("lg:lo g"), None);
        // No colon.
        assert_eq!(match_lua_colon_call("lglog"), None);

        assert_eq!(match_r_dollar_call("$log"), None);
        assert_eq!(match_r_dollar_call("lg$"), None);
        assert_eq!(match_r_dollar_call("l-g$log"), None);
        assert_eq!(match_r_dollar_call("lg$lo g"), None);
        assert_eq!(match_r_dollar_call("lglog"), None);
    }

    #[test]
    fn is_inferable_receiver_call_shapes() {
        assert!(is_inferable_receiver_call("lg.log", Language::TypeScript));
        assert!(is_inferable_receiver_call("lg:log", Language::Lua));
        assert!(is_inferable_receiver_call("lg$log", Language::R));
        // Colon-call `Class::method` is not inferable.
        assert!(!is_inferable_receiver_call("Foo::log", Language::Rust));
        // Lua separator not inferable outside Lua/Luau.
        assert!(!is_inferable_receiver_call("lg:log", Language::Rust));
        // R separator not inferable outside R.
        assert!(!is_inferable_receiver_call("lg$log", Language::TypeScript));
    }

    // ---- Nested-local lexical reachability (#1230 / upstream c472cfb) -------

    /// `report_missing` (lines 5-8) calls `join`, but the ONLY project `join`
    /// is nested inside `format_fields` (lines 1-3) — lexically unreachable.
    fn nested_local_ctx() -> Ctx {
        let outer = {
            let mut n = mk(
                "function:format_fields",
                NodeKind::Function,
                "format_fields",
                "format_fields",
                "src/repro.py",
                Language::Python,
            );
            n.start_line = 1;
            n.end_line = 3;
            n
        };
        let nested = {
            let mut n = mk(
                "function:join",
                NodeKind::Function,
                "join",
                "format_fields::join",
                "src/repro.py",
                Language::Python,
            );
            n.start_line = 2;
            n.end_line = 3;
            n
        };
        let caller = {
            let mut n = mk(
                "function:report_missing",
                NodeKind::Function,
                "report_missing",
                "report_missing",
                "src/repro.py",
                Language::Python,
            );
            n.start_line = 5;
            n.end_line = 8;
            n
        };
        Ctx::default()
            .name("join", vec![nested.clone()])
            .qualified("format_fields", vec![outer.clone()])
            .nodes_in_file("src/repro.py", vec![outer, nested, caller])
    }

    #[test]
    fn nested_local_is_unreachable_from_another_function() {
        let ctx = nested_local_ctx();
        let reference = refv("join", EdgeKind::Calls, "src/repro.py", Language::Python, 6);
        assert_eq!(
            match_by_exact_name(&reference, &ctx),
            None,
            "a `join` nested in format_fields must NOT resolve from report_missing"
        );
    }

    #[test]
    fn nested_local_resolves_from_inside_its_container() {
        let ctx = nested_local_ctx();
        let reference = refv("join", EdgeKind::Calls, "src/repro.py", Language::Python, 3);
        let resolved = match_by_exact_name(&reference, &ctx).expect("reachable from inside");
        assert_eq!(resolved.target_node_id, "function:join");
    }

    #[test]
    fn nested_local_is_unreachable_from_another_file() {
        let ctx = nested_local_ctx();
        let reference = refv("join", EdgeKind::Calls, "src/other.py", Language::Python, 2);
        assert_eq!(
            match_by_exact_name(&reference, &ctx),
            None,
            "a nested local is never reachable from a different file"
        );
    }

    #[test]
    fn class_member_candidate_is_not_scope_filtered() {
        let cls = {
            let mut n = mk(
                "class:Fmt",
                NodeKind::Class,
                "Fmt",
                "Fmt",
                "src/fmt.py",
                Language::Python,
            );
            n.start_line = 1;
            n.end_line = 6;
            n
        };
        let member = {
            let mut n = mk(
                "function:join",
                NodeKind::Function,
                "join",
                "Fmt::join",
                "src/fmt.py",
                Language::Python,
            );
            n.start_line = 2;
            n.end_line = 3;
            n
        };
        let ctx = Ctx::default()
            .name("join", vec![member.clone()])
            .qualified("Fmt", vec![cls.clone()])
            .nodes_in_file("src/fmt.py", vec![cls, member]);
        let reference = refv("join", EdgeKind::Calls, "src/other.py", Language::Python, 9);
        let resolved = match_by_exact_name(&reference, &ctx).expect("class members stay reachable");
        assert_eq!(resolved.target_node_id, "function:join");
    }

    #[test]
    fn top_level_and_unparented_candidates_are_not_scope_filtered() {
        let top = mk(
            "function:helper",
            NodeKind::Function,
            "helper",
            "helper",
            "src/top.py",
            Language::Python,
        );
        let ctx = Ctx::default()
            .name("helper", vec![top.clone()])
            .nodes_in_file("src/top.py", vec![top]);
        let reference = refv(
            "helper",
            EdgeKind::Calls,
            "src/other.py",
            Language::Python,
            3,
        );
        assert_eq!(
            match_by_exact_name(&reference, &ctx)
                .expect("top-level symbol stays reachable")
                .target_node_id,
            "function:helper"
        );

        // A C++ namespace prefix has no node, so the qualified name still holds
        // `::` but no container resolves — the candidate must survive.
        let ns_fn = mk(
            "function:emit",
            NodeKind::Function,
            "emit",
            "sim::emit",
            "src/sim.cpp",
            Language::Cpp,
        );
        let ns_ctx = Ctx::default()
            .name("emit", vec![ns_fn.clone()])
            .nodes_in_file("src/sim.cpp", vec![ns_fn]);
        let ns_ref = refv("emit", EdgeKind::Calls, "src/use.cpp", Language::Cpp, 4);
        assert_eq!(
            match_by_exact_name(&ns_ref, &ns_ctx)
                .expect("namespace-prefixed symbol stays reachable")
                .target_node_id,
            "function:emit"
        );
    }

    // ---- Tier 2 item 4: dotted receiver refs vs the file-path strategy ------

    #[test]
    fn dotted_receiver_ref_is_not_treated_as_a_file_path() {
        // `has_short_extension("mailer.send")` is TRUE (a 4-char alphanumeric
        // tail), so item 4's rewritten refs now reach `match_by_file_path` first.
        // It must still decline: the lookup key is the WHOLE dotted name, which
        // no file node carries — a decoy `send` file node must not be adopted.
        let decoy = mk(
            "file:send",
            NodeKind::File,
            "send",
            "src/send.ts",
            "src/send.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default()
            .name("send", vec![decoy.clone()])
            .name("mailer.send", vec![]);
        let r = refv(
            "mailer.send",
            EdgeKind::Calls,
            "src/mail.ts",
            Language::TypeScript,
            4,
        );
        assert!(
            match_by_file_path(&r, &ctx).is_none(),
            "a dotted receiver ref must not resolve as a file path"
        );

        // End to end through `match_reference`: the same ref resolves by the
        // method strategies, never with `ResolvedBy::FilePath`.
        let mailer = mk(
            "class:mailer",
            NodeKind::Class,
            "Mailer",
            "Mailer",
            "src/mail.ts",
            Language::TypeScript,
        );
        let send = mk(
            "method:send",
            NodeKind::Method,
            "send",
            "Mailer::send",
            "src/mail.ts",
            Language::TypeScript,
        );
        let full = Ctx::default()
            .name("send", vec![decoy, send.clone()])
            .name("Mailer", vec![mailer])
            .nodes_in_file("src/mail.ts", vec![send]);
        let resolved = match_reference(&r, &full).expect("dotted receiver still resolves");
        assert_ne!(
            resolved.resolved_by,
            ResolvedBy::FilePath,
            "a dotted receiver ref must not be resolved by the file-path strategy"
        );
        assert_eq!(resolved.target_node_id, "method:send");
    }

    // ---- Tier 2 item 5: inferred-type visibility at the call site (#1566) ---

    /// A file declaring `class Map { <method> }` plus a lone project method of
    /// that name, and a local `const m = new Map(); m.<method>()` in
    /// `main.ts`. `bind_in_main` controls whether `Map` is visible at the call
    /// site (same-file declaration), which is exactly what the gate reads.
    fn builtin_hijack_ctx(method: &str, owner: &str, bind_in_main: bool) -> Ctx {
        let map_class = mk(
            "class:map",
            NodeKind::Class,
            "Map",
            "Map",
            "src/other.ts",
            Language::TypeScript,
        );
        let target = mk(
            "method:target",
            NodeKind::Method,
            method,
            &format!("{owner}::{method}"),
            "src/other.ts",
            Language::TypeScript,
        );
        let mut caller = mk(
            "function:useBuiltin",
            NodeKind::Function,
            "useBuiltin",
            "useBuiltin",
            "src/main.ts",
            Language::TypeScript,
        );
        caller.start_line = 1;
        caller.end_line = 4;
        let mut in_main = vec![caller];
        if bind_in_main {
            let mut shadow = map_class.clone();
            shadow.id = "class:map-main".to_string();
            shadow.file_path = "src/main.ts".to_string();
            in_main.push(shadow);
        }
        Ctx::default()
            .file(
                "src/main.ts",
                "function useBuiltin() {\n  const m = new Map<string, string>();\n  m.MEMBER();\n}\n",
            )
            .nodes_in_file("src/main.ts", in_main)
            .name("Map", vec![map_class])
            .name(method, vec![target])
    }

    fn builtin_hijack_ref(method: &str) -> RefView {
        refv(
            &format!("m.{method}"),
            EdgeKind::Calls,
            "src/main.ts",
            Language::TypeScript,
            3,
        )
    }

    #[test]
    fn unimported_project_class_does_not_hijack_builtin() {
        // Given a built-in `Map` in main.ts and an UNIMPORTED project
        // `class Map { get() }` in other.ts,
        // When the call `m.get("a")` resolves,
        // Then nothing binds — today the TYPED path SUCCEEDS at 0.9 on a flatly
        // wrong edge, which is why the gate must sit before typed resolution.
        let ctx = builtin_hijack_ctx("get", "Map", false);
        let landed = match_method_call(&builtin_hijack_ref("get"), &ctx);
        assert!(
            landed.is_none(),
            "an unbound inferred type must not resolve its method, got: {:?}",
            landed.map(|r| (r.target_node_id, r.resolved_by, r.confidence))
        );
    }

    #[test]
    fn unimported_project_class_without_the_method_does_not_fall_through() {
        // The mirror: the unimported project `Map` has no `get`, so typed
        // resolution FAILS and Strategy 3 would adopt a unique `Decoy::get`
        // at 0.7. The gate refuses before either runs.
        let ctx = builtin_hijack_ctx("get", "Decoy", false);
        let landed = match_method_call(&builtin_hijack_ref("get"), &ctx);
        assert!(
            landed.is_none(),
            "an unbound inferred type must not fall through to Strategy 3, got: {:?}",
            landed.map(|r| (r.target_node_id, r.resolved_by, r.confidence))
        );
    }

    #[test]
    fn same_file_project_map_still_resolves() {
        // V5 — a project `class Map` declared in the CALLING file shadows the
        // built-in, so the receiver type IS bound and the edge must be kept.
        let ctx = builtin_hijack_ctx("get", "Map", true);
        let res =
            match_method_call(&builtin_hijack_ref("get"), &ctx).expect("same-file Map is bound");
        assert_eq!(res.target_node_id, "method:target");
        assert_eq!(res.resolved_by, ResolvedBy::InstanceMethod);
        assert!((res.confidence - 0.9).abs() < 1e-9);
    }

    #[test]
    fn imported_project_map_still_resolves() {
        // V1 — a named import binds `Map` at the call site.
        let ctx = builtin_hijack_ctx("get", "Map", false).import(
            "src/main.ts",
            vec![ImportMapping {
                local_name: "Map".to_string(),
                exported_name: "Map".to_string(),
                source: "./other".to_string(),
                is_default: false,
                is_namespace: false,
            }],
        );
        let res =
            match_method_call(&builtin_hijack_ref("get"), &ctx).expect("imported Map is bound");
        assert_eq!(res.target_node_id, "method:target");
        assert!((res.confidence - 0.9).abs() < 1e-9);
    }

    #[test]
    fn barrel_imported_project_map_still_resolves() {
        // V4 — a barrel re-export. The predicate reads the IMPORTING file's
        // `local_name`, so it never has to resolve the specifier.
        let ctx = builtin_hijack_ctx("get", "Map", false).import(
            "src/main.ts",
            vec![ImportMapping {
                local_name: "Map".to_string(),
                exported_name: "Map".to_string(),
                source: "./barrel".to_string(),
                is_default: false,
                is_namespace: false,
            }],
        );
        let res = match_method_call(&builtin_hijack_ref("get"), &ctx).expect("barrel Map is bound");
        assert_eq!(res.target_node_id, "method:target");
        assert!((res.confidence - 0.9).abs() < 1e-9);
    }

    /// C1/C2 — a receiver typed to a same-file PROJECT class `Real`.
    /// `Real::poke` exists; `absentOnReal` does not, but a unique `Decoy` has it.
    fn project_typed_receiver_ctx() -> Ctx {
        let real = mk(
            "class:real",
            NodeKind::Class,
            "Real",
            "Real",
            "src/main.ts",
            Language::TypeScript,
        );
        let poke = mk(
            "method:poke",
            NodeKind::Method,
            "poke",
            "Real::poke",
            "src/main.ts",
            Language::TypeScript,
        );
        let absent = mk(
            "method:absent",
            NodeKind::Method,
            "absentOnReal",
            "Decoy::absentOnReal",
            "src/decoy.ts",
            Language::TypeScript,
        );
        let mut caller = mk(
            "function:use",
            NodeKind::Function,
            "use",
            "use",
            "src/main.ts",
            Language::TypeScript,
        );
        caller.start_line = 1;
        caller.end_line = 4;
        Ctx::default()
            .file(
                "src/main.ts",
                "function use() {\n  const real = new Real();\n  real.poke();\n}\n",
            )
            .nodes_in_file("src/main.ts", vec![real.clone(), poke.clone(), caller])
            .name("Real", vec![real])
            .name("poke", vec![poke])
            .name("absentOnReal", vec![absent])
    }

    #[test]
    fn project_typed_receiver_still_resolves() {
        // C1 — the inferred `Real` is declared in this file, so it is bound and
        // typed resolution proceeds exactly as today.
        let ctx = project_typed_receiver_ctx();
        let r = refv(
            "real.poke",
            EdgeKind::Calls,
            "src/main.ts",
            Language::TypeScript,
            3,
        );
        let res = match_method_call(&r, &ctx).expect("project-typed receiver resolves");
        assert_eq!(res.target_node_id, "method:poke");
        assert!((res.confidence - 0.9).abs() < 1e-9);
    }

    #[test]
    fn absent_method_on_project_type_still_falls_through() {
        // C2 — the type is BOUND but lacks the method, so Strategy 3's guess is
        // reached unchanged. The gate keys on binding, not on method existence.
        // The receiver MUST stay `real`, not `r`: `split_camel_case` drops
        // single-character words, so `r` overlaps nothing and Strategy 3's
        // single-candidate branch would refuse — deleting this test's coverage
        // rather than exercising it. `real` overlaps `Real` in
        // `Decoy::absentOnReal`. The shared fixture declares the same spelling,
        // so all three sites move together or inference declines for the other
        // consumer.
        let ctx = project_typed_receiver_ctx();
        let r = refv(
            "real.absentOnReal",
            EdgeKind::Calls,
            "src/main.ts",
            Language::TypeScript,
            3,
        );
        let res = match_method_call(&r, &ctx).expect("bound type still falls through");
        assert_eq!(res.target_node_id, "method:absent");
        assert!((res.confidence - 0.7).abs() < 1e-9);
    }

    #[test]
    fn go_same_package_receiver_still_resolves() {
        // V3 — Go makes a same-package type in ANOTHER file visible with no
        // import statement at all, so "bound = same-file or imported"
        // under-approximates there. The ESM-only scope is what keeps this edge.
        let thing = mk(
            "struct:thing",
            NodeKind::Struct,
            "Thing",
            "Thing",
            "b.go",
            Language::Go,
        );
        let ping = mk(
            "method:ping",
            NodeKind::Method,
            "Ping",
            "Thing::Ping",
            "b.go",
            Language::Go,
        );
        let mut caller = mk(
            "function:run",
            NodeKind::Function,
            "run",
            "run",
            "a.go",
            Language::Go,
        );
        caller.start_line = 1;
        caller.end_line = 4;
        let ctx = Ctx::default()
            .file("a.go", "func run() {\n\tvar t Thing\n\tt.Ping()\n}\n")
            .nodes_in_file("a.go", vec![caller])
            .name("Thing", vec![thing])
            .name("Ping", vec![ping]);
        let r = refv("t.Ping", EdgeKind::Calls, "a.go", Language::Go, 3);
        let res = match_method_call(&r, &ctx).expect("go same-package receiver resolves");
        assert_eq!(res.target_node_id, "method:ping");
        assert!((res.confidence - 0.9).abs() < 1e-9);
    }

    // ---- Tier 2 item 5: class-field receiver inference (§2.2b) --------------

    /// A class whose FIELD is initialized to a project class, with the call in a
    /// later method — the shape item 4's rewrite produces and that the
    /// function-scope backward scan structurally cannot reach. `with_wrong` adds a
    /// competing `Wrong::doThing`; without it the name is unique project-wide and
    /// Strategy 3 takes its single-candidate shortcut instead of overlap scoring.
    fn class_field_receiver_ctx(
        source: &str,
        field_line: i64,
        sibling: Option<Node>,
        with_wrong: bool,
    ) -> Ctx {
        let mut holder = mk(
            "class:c",
            NodeKind::Class,
            "C",
            "C",
            "src/main.ts",
            Language::TypeScript,
        );
        holder.start_line = 1;
        holder.end_line = source.split('\n').count() as i64;
        let mut field = mk(
            "property:conn",
            NodeKind::Property,
            "conn",
            "C::conn",
            "src/main.ts",
            Language::TypeScript,
        );
        field.start_line = field_line;
        field.end_line = field_line;
        let right = mk(
            "class:right",
            NodeKind::Class,
            "Right",
            "Right",
            "src/dep.ts",
            Language::TypeScript,
        );
        let do_thing = mk(
            "method:right-doThing",
            NodeKind::Method,
            "doThing",
            "Right::doThing",
            "src/dep.ts",
            Language::TypeScript,
        );
        let mut members = vec![holder, field];
        if let Some(sibling) = sibling {
            members.push(sibling);
        }
        let wrong = mk(
            "class:wrong",
            NodeKind::Class,
            "Wrong",
            "Wrong",
            "src/dep.ts",
            Language::TypeScript,
        );
        let mut do_thing_targets = vec![do_thing.clone()];
        if with_wrong {
            do_thing_targets.push(mk(
                "method:wrong-doThing",
                NodeKind::Method,
                "doThing",
                "Wrong::doThing",
                "src/dep.ts",
                Language::TypeScript,
            ));
        }
        // BOTH types are bound at the call site. Binding `Wrong` too is what makes
        // the no-leak control meaningful: were inference to reach a sibling
        // method's `new Wrong()`, the visibility gate would PERMIT it, so the
        // assertion — not the gate — is what catches the leak.
        Ctx::default()
            .file("src/main.ts", source)
            .nodes_in_file("src/main.ts", members)
            .nodes_in_file("src/dep.ts", vec![do_thing, right.clone()])
            .name("Right", vec![right])
            .name("Wrong", vec![wrong])
            .name("doThing", do_thing_targets)
            .imports_type("src/main.ts", "Right")
            .imports_type("src/main.ts", "Wrong")
    }

    #[test]
    fn class_field_project_receiver_resolves_by_type() {
        // Given `private conn = new Right()` as a class FIELD and
        // `conn.doThing()` inside a method,
        // When resolution runs,
        // Then the edge comes from TYPED resolution at 0.9, not a Strategy 3
        // guess at 0.7 — asserting the confidence is what proves inference ran.
        let source = concat!(
            "export class C {\n",
            "  private conn = new Right();\n",
            "\n",
            "  second(): void {\n",
            "    this.conn.doThing();\n",
            "  }\n",
            "}\n",
        );
        let mut second = mk(
            "method:second",
            NodeKind::Method,
            "second",
            "C::second",
            "src/main.ts",
            Language::TypeScript,
        );
        second.start_line = 4;
        second.end_line = 6;
        let ctx = class_field_receiver_ctx(source, 2, Some(second), false);
        let r = refv(
            "conn.doThing",
            EdgeKind::Calls,
            "src/main.ts",
            Language::TypeScript,
            5,
        );
        let res = match_method_call(&r, &ctx).expect("class-field receiver resolves");
        assert_eq!(res.target_node_id, "method:right-doThing");
        assert_eq!(res.resolved_by, ResolvedBy::InstanceMethod);
        assert!(
            (res.confidence - 0.9).abs() < 1e-9,
            "must be typed resolution (0.9), not a Strategy 3 guess: {}",
            res.confidence
        );
    }

    #[test]
    fn class_field_inference_does_not_leak_sibling_method_local() {
        // The falsifier for the rejected widened-window design: the backward
        // scan takes the NEAREST match ABOVE the call, so a class-wide window
        // scanned back from line 12 would hit the sibling method's
        // `const conn = new Wrong()` at line 7 and infer `Wrong`. Reading only
        // the field node's OWN line cannot leak, because it never looks at a
        // line inside any method body.
        let source = concat!(
            "export class C {\n",
            "  private conn = new Right();\n",
            "\n",
            "  first(): void {\n",
            "    const conn = new Wrong();\n",
            "    conn.doThing();\n",
            "  }\n",
            "\n",
            "  second(): void {\n",
            "    this.conn.doThing();\n",
            "  }\n",
            "}\n",
        );
        let mut second = mk(
            "method:second",
            NodeKind::Method,
            "second",
            "C::second",
            "src/main.ts",
            Language::TypeScript,
        );
        second.start_line = 9;
        second.end_line = 11;
        let ctx = class_field_receiver_ctx(source, 2, Some(second), true);
        let r = refv(
            "conn.doThing",
            EdgeKind::Calls,
            "src/main.ts",
            Language::TypeScript,
            10,
        );
        // Asserted as a pure NEGATIVE so it holds both before the fix (where the
        // ref resolves to nothing) and after (where it reaches `Right::doThing`).
        let landed = match_method_call(&r, &ctx).map(|res| res.target_node_id);
        assert_ne!(
            landed.as_deref(),
            Some("method:wrong-doThing"),
            "field inference must not read a sibling method's local declaration"
        );
    }

    // ---- Tier 2 item 6: kind eligibility (#1537/#1536) ---------------------

    #[test]
    fn import_does_not_bind_a_type_member() {
        // `import * as path from 'node:path'` must not bind to some class's
        // `path` PROPERTY — a type member is not importable on its own.
        let prop = mk(
            "property:path",
            NodeKind::Property,
            "path",
            "Request::path",
            "src/types.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("path", vec![prop]);
        let r = refv(
            "path",
            EdgeKind::Imports,
            "src/run.ts",
            Language::TypeScript,
            1,
        );
        let landed = match_by_exact_name(&r, &ctx);
        assert!(
            landed.is_none(),
            "an imports ref must not bind a property, got: {:?}",
            landed.map(|res| res.target_node_id)
        );
    }

    #[test]
    fn import_does_not_bind_a_same_named_function() {
        // 6f — a `function` IS an importable kind, so the kind gate permits it
        // and only import LOCALITY can refuse: `node:path` lives outside the
        // repository, so no in-repo `path` is its referent. Both halves are
        // asserted here so neither gate alone satisfies the test.
        let helper = mk(
            "function:path",
            NodeKind::Function,
            "path",
            "path",
            "src/helpers.ts",
            Language::TypeScript,
        );
        assert!(
            crate::types::is_importable_kind(NodeKind::Function),
            "a function is importable — this case needs the locality gate, not the kind gate"
        );
        let ctx = Ctx::default().name("path", vec![helper]);
        let r = refv(
            "path",
            EdgeKind::Imports,
            "src/run.ts",
            Language::TypeScript,
            1,
        );
        // Locality is decided in the resolver over the file's import mappings, so
        // the end-to-end refusal is asserted by the `esm_import` fixture test.
        // Here the kind gate must at least NOT be what refuses it.
        assert!(
            match_by_exact_name(&r, &ctx).is_some(),
            "the kind gate must permit an importable kind; locality owns this refusal"
        );
    }

    #[test]
    fn out_of_repo_supertype_does_not_bind_enum_member() {
        // 6b — `impl Error for MapperError` where `std::error::Error` is
        // out-of-repo must not bind the enum's own `Error` VARIANT.
        let member = mk(
            "enum_member:Error",
            NodeKind::EnumMember,
            "Error",
            "MapperError::Error",
            "src/lib.rs",
            Language::Rust,
        );
        let ctx = Ctx::default().name("Error", vec![member]);
        let r = refv(
            "Error",
            EdgeKind::Implements,
            "src/lib.rs",
            Language::Rust,
            8,
        );
        let landed = match_by_exact_name(&r, &ctx);
        assert!(
            landed.is_none(),
            "an implements ref must not bind an enum member, got: {:?}",
            landed.map(|res| res.target_node_id)
        );
    }

    #[test]
    fn in_repo_trait_still_implements() {
        // 6d control — a real in-repo trait target must keep its edge, and the
        // pre-ranking filter is what lets it OUTRANK a same-named enum member
        // rather than merely surviving.
        let member = mk(
            "enum_member:Error",
            NodeKind::EnumMember,
            "Error",
            "MapperError::Error",
            "src/lib.rs",
            Language::Rust,
        );
        let trait_node = mk(
            "trait:Error",
            NodeKind::Trait,
            "Error",
            "Error",
            "src/ports.rs",
            Language::Rust,
        );
        let ctx = Ctx::default().name("Error", vec![member, trait_node]);
        let r = refv(
            "Error",
            EdgeKind::Implements,
            "src/lib.rs",
            Language::Rust,
            8,
        );
        let res = match_by_exact_name(&r, &ctx).expect("in-repo trait still resolves");
        assert_eq!(
            res.target_node_id, "trait:Error",
            "the legal supertype must outrank the same-named enum member"
        );
    }

    #[test]
    fn kind_filter_does_not_reach_the_single_candidate_shortcut() {
        // Measured on this repo: a GDScript `extends Node` had two same-named
        // rivals — an `enum_member Node` and a cross-language Rust `struct Node` —
        // and `find_best_match` correctly declined both (the −80 cross-language
        // penalty sinks every score below the −1.0 floor). Filtering the enum
        // member out must NOT hand the survivor to the single-candidate shortcut,
        // which skips that penalty and would fabricate the edge the scored path
        // refused. These gates only ever remove an edge.
        let member = mk(
            "enum_member:Node",
            NodeKind::EnumMember,
            "Node",
            "Command::Node",
            "src/main.rs",
            Language::Rust,
        );
        let strukt = mk(
            "struct:Node",
            NodeKind::Struct,
            "Node",
            "Node",
            "src/types.rs",
            Language::Rust,
        );
        let ctx = Ctx::default().name("Node", vec![member, strukt]);
        let r = refv(
            "Node",
            EdgeKind::Extends,
            "fixtures/godot/effect_manager.gd",
            Language::Gdscript,
            1,
        );
        let landed = match_by_exact_name(&r, &ctx);
        assert!(
            landed.is_none(),
            "a cross-language ambiguous supertype must stay unresolved, got: {:?}",
            landed.map(|res| (res.target_node_id, res.confidence))
        );
    }

    #[test]
    fn class_implements_object_type_alias() {
        // Gate 1's `TypeAlias` allowance is pinned as KEPT, not merely permitted:
        // a TS class implementing an object type alias is a real edge.
        let alias = mk(
            "type_alias:Shape",
            NodeKind::TypeAlias,
            "Shape",
            "Shape",
            "src/shape.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().name("Shape", vec![alias]);
        let r = refv(
            "Shape",
            EdgeKind::Implements,
            "src/box.ts",
            Language::TypeScript,
            2,
        );
        let res = match_by_exact_name(&r, &ctx).expect("type alias is a legal supertype");
        assert_eq!(res.target_node_id, "type_alias:Shape");
    }

    // ================= v1.6.1 gate ports ======================================

    fn at_column(
        name: &str,
        kind: EdgeKind,
        path: &str,
        lang: Language,
        line: i64,
        column: i64,
    ) -> RefView {
        let mut reference = refv(name, kind, path, lang, line);
        reference.column = column;
        reference
    }

    #[test]
    fn no_receiver_on_line_reads_the_name_or_its_first_call() {
        let source = "        AssertValid();\n        this.Check();\n        Easing.Linear\n    .Select(x => x)\n        var t = TestContext.Current;\n        Foo.\tCall(token);\n        x = $call(1); Call(2);\n";
        let ctx = Ctx::default().file("A.cs", source);
        let none = |name: &str, line: i64, column: i64| {
            has_no_receiver_on_line(
                &at_column(
                    name,
                    EdgeKind::Calls,
                    "A.cs",
                    Language::CSharp,
                    line,
                    column,
                ),
                &ctx,
            )
        };
        assert!(none("AssertValid", 1, 8));
        assert!(!none("Check", 2, 13), "`this.` is a receiver");
        assert!(none("Easing", 3, 8));
        assert!(none("easing", 3, 8), "read case-insensitively");
        assert!(!none("Select", 4, 0), "the name is on the line after a `.`");
        assert!(none("TestContext", 5, 27), "a column at the name's end");
        assert!(
            !none("Call", 6, 0),
            "the first `Call(` follows a `.` and blanks"
        );
        assert!(none("Call", 7, 0), "`$call(` is no call of `Call`");
        assert!(
            !none("Missing", 1, 0),
            "a name off its line is a chain link"
        );
    }

    #[test]
    fn receiver_less_call_reads_the_call_site_line() {
        let source = "serialize(x);\nthis.serialize(x);\n  return helper();\nobj.helper();\nx = helper ();\nfinish\n  ();\nnew Foo();\nxreturn helper();\n";
        let ctx = Ctx::default().file("a.ts", source);
        let bare = |name: &str, line: i64, column: i64| {
            is_bare_js_call(
                &at_column(
                    name,
                    EdgeKind::Calls,
                    "a.ts",
                    Language::TypeScript,
                    line,
                    column,
                ),
                &ctx,
            )
        };
        assert!(bare("serialize", 1, 0));
        assert!(!bare("serialize", 2, 5), "this.serialize has a receiver");
        assert!(bare("helper", 3, 9), "a keyword opens an expression");
        assert!(!bare("helper", 4, 4), "obj.helper has a receiver");
        assert!(bare("helper", 5, 4), "an operator precedes a bare call");
        assert!(bare("finish", 6, 0), "ASI never splits `finish\\n()`");
        assert!(bare("Foo", 8, 4), "`new` is a bare-call keyword");
        assert!(
            !bare("helper", 9, 8),
            "a word that merely ends in a keyword is not one"
        );
        assert!(
            !bare("serialize", 1, 1),
            "the name must start at the column"
        );
        // Go shares the check, with its own statement keywords.
        let go = Ctx::default().file("m.go", "\tgo work()\n\tp.work()\n");
        assert!(is_bare_go_call(
            &at_column("work", EdgeKind::Calls, "m.go", Language::Go, 1, 4),
            &go
        ));
        assert!(!is_bare_go_call(
            &at_column("work", EdgeKind::Calls, "m.go", Language::Go, 2, 3),
            &go
        ));
        // Only `calls` refs, and only the owning language family.
        assert!(!is_bare_js_call(
            &at_column(
                "serialize",
                EdgeKind::References,
                "a.ts",
                Language::TypeScript,
                1,
                0
            ),
            &ctx
        ));
        assert!(!is_bare_go_call(
            &at_column(
                "serialize",
                EdgeKind::Calls,
                "a.ts",
                Language::TypeScript,
                1,
                0
            ),
            &ctx
        ));
    }

    // ====== KEEP-RUST P15 — Rust calls that lost their receiver ======

    #[test]
    fn rust_call_shape_reads_bare_and_dropped_receiver_calls() {
        let source = "    join(x);\n    parse::<u8>(s);\n    root().join(x);\n    root()\n        .join(x);\n    joined(x);\n    root().joined(x);\n    0..len();\n    root().collect::<Vec<_>>(x);\n";
        let ctx = Ctx::default().file("src/a.rs", source);
        let shape = |name: &str, line: i64, column: i64| {
            rust_call_shape(
                &at_column(
                    name,
                    EdgeKind::Calls,
                    "src/a.rs",
                    Language::Rust,
                    line,
                    column,
                ),
                &ctx,
            )
        };
        assert_eq!(shape("join", 1, 4), Some(RustCallShape::Bare));
        assert_eq!(
            shape("parse", 2, 4),
            Some(RustCallShape::Bare),
            "a turbofish still opens a bare call"
        );
        assert_eq!(
            shape("join", 3, 4),
            Some(RustCallShape::Method),
            "root().join dropped its receiver"
        );
        assert_eq!(
            shape("join", 4, 4),
            Some(RustCallShape::Method),
            "the `.join(` may sit on a later line"
        );
        assert_eq!(shape("join", 6, 4), None, "`joined(` is another name");
        assert_eq!(shape("join", 7, 4), None, "`.joined(` is not `.join(`");
        assert_eq!(
            shape("len", 8, 7),
            Some(RustCallShape::Bare),
            "a range end is a bare call"
        );
        assert_eq!(
            shape("collect", 9, 4),
            Some(RustCallShape::Method),
            "a turbofish after a dropped receiver"
        );
        assert_eq!(shape("join", 1, 400), None, "a column past the line end");
        assert_eq!(shape("join", 99, 0), None, "a line past the file end");
        // Only Rust `calls` refs whose name has no `.` / `::`, and only with source.
        let unknown = |name: &str, kind: EdgeKind, path: &str, lang: Language| {
            rust_call_shape(&at_column(name, kind, path, lang, 1, 4), &ctx)
        };
        assert_eq!(
            unknown("join", EdgeKind::References, "src/a.rs", Language::Rust),
            None
        );
        assert_eq!(
            unknown("p.join", EdgeKind::Calls, "src/a.rs", Language::Rust),
            None
        );
        assert_eq!(
            unknown("a::join", EdgeKind::Calls, "src/a.rs", Language::Rust),
            None
        );
        assert_eq!(
            unknown("join", EdgeKind::Calls, "src/missing.rs", Language::Rust),
            None
        );
        assert_eq!(
            unknown("join", EdgeKind::Calls, "src/a.rs", Language::TypeScript),
            None
        );
    }

    #[test]
    fn rust_receiver_gates_narrow_exact_name_candidates() {
        // The trailing `use` brings `util.rs`'s `helper` into scope: another
        // file's function is reached only through one (upstream #2214).
        let source = "fn f() {\n    root().join(x);\n    store().nodes_by_ids(x);\n    helper();\n    root().build();\n    Err(1);\n}\nuse crate::util::helper;\n";
        let method = |name: &str, qualified: &str| {
            mk(
                &format!("method:{name}"),
                NodeKind::Method,
                name,
                qualified,
                "src/lib.rs",
                Language::Rust,
            )
        };
        let function = |name: &str| {
            mk(
                &format!("function:{name}"),
                NodeKind::Function,
                name,
                name,
                "src/util.rs",
                Language::Rust,
            )
        };
        let at = |name: &str, line: i64| {
            at_column(name, EdgeKind::Calls, "src/a.rs", Language::Rust, line, 4)
        };
        let target = |reference: &RefView, ctx: &Ctx| {
            match_by_exact_name(reference, ctx).map(|r| r.target_node_id)
        };

        // R3: a lone project method sharing a std method's name binds nothing.
        let ctx = Ctx::default()
            .file("src/a.rs", source)
            .name("join", vec![method("join", "LineIndex::join")]);
        assert_eq!(target(&at("join", 2), &ctx), None);
        // R2: a project-specific name binds its lone method, never a free function.
        let ctx = Ctx::default().file("src/a.rs", source).name(
            "nodes_by_ids",
            vec![method("nodes_by_ids", "Store::nodes_by_ids")],
        );
        assert_eq!(
            target(&at("nodes_by_ids", 3), &ctx).as_deref(),
            Some("method:nodes_by_ids")
        );
        let ctx = Ctx::default()
            .file("src/a.rs", source)
            .name("build", vec![function("build")]);
        assert_eq!(target(&at("build", 5), &ctx), None);
        // R1: a bare call binds the free fn, and never a method.
        let ctx = Ctx::default().file("src/a.rs", source).name(
            "helper",
            vec![method("helper", "Store::helper"), function("helper")],
        );
        assert_eq!(
            target(&at("helper", 4), &ctx).as_deref(),
            Some("function:helper")
        );
        let ctx = Ctx::default()
            .file("src/a.rs", source)
            .name("helper", vec![method("helper", "Store::helper")]);
        assert_eq!(target(&at("helper", 4), &ctx), None);
        // Without that `use`, `util.rs`'s `helper` is out of scope (#2214).
        let unimported = source.replace("use crate::util::helper;\n", "");
        let ctx = Ctx::default()
            .file("src/a.rs", &unimported)
            .name("helper", vec![function("helper")]);
        assert_eq!(target(&at("helper", 4), &ctx), None);
        // R4: a bare `Err` is the prelude's unless a `use` in scope imports one.
        let variant = mk(
            "enum_member:Err",
            NodeKind::EnumMember,
            "Err",
            "Outcome::Err",
            "src/outcome.rs",
            Language::Rust,
        );
        let ctx = Ctx::default()
            .file("src/a.rs", source)
            .name("Err", vec![variant.clone()]);
        assert_eq!(target(&at("Err", 6), &ctx), None);
        let imported = format!("use crate::outcome::Outcome::Err;\n{source}");
        let ctx = Ctx::default()
            .file("src/a.rs", &imported)
            .name("Err", vec![variant]);
        assert_eq!(
            target(&at("Err", 7), &ctx).as_deref(),
            Some("enum_member:Err")
        );
        // No source: the shape is unknown and today's binding stands.
        let ctx = Ctx::default().name("join", vec![method("join", "LineIndex::join")]);
        assert_eq!(target(&at("join", 2), &ctx).as_deref(), Some("method:join"));
    }

    #[test]
    fn rust_receiver_gates_close_the_fuzzy_fallback() {
        // Only the lowercase index holds `join` / `helper`, so the fuzzy
        // fallback is the one strategy that finds them; the shapes still gate
        // them. (Fuzzy matching is exact-case in Rust, upstream #2139.) The
        // trailing `use` brings `x.rs`'s `helper` into scope (upstream #2214).
        let source = "fn f() {\n    root().join(x);\n    helper(x);\n}\nuse crate::x::helper;\n";
        let lone = |name: &str, kind: NodeKind| {
            vec![mk(
                &format!("{kind:?}:{name}"),
                kind,
                name,
                name,
                "src/x.rs",
                Language::Rust,
            )]
        };
        let fuzzy = |name: &str, line: i64, ctx: &Ctx| {
            match_fuzzy(
                &at_column(name, EdgeKind::Calls, "src/a.rs", Language::Rust, line, 4),
                ctx,
            )
        };
        let ctx = Ctx::default()
            .file("src/a.rs", source)
            .lower("join", lone("join", NodeKind::Method));
        assert!(
            fuzzy("join", 2, &ctx).is_none(),
            "a dropped receiver takes no fuzzy guess"
        );
        let ctx = Ctx::default()
            .file("src/a.rs", source)
            .lower("helper", lone("helper", NodeKind::Method));
        assert!(
            fuzzy("helper", 3, &ctx).is_none(),
            "a bare call never reaches a method"
        );
        let ctx = Ctx::default()
            .file("src/a.rs", source)
            .lower("helper", lone("helper", NodeKind::Function));
        assert!(
            fuzzy("helper", 3, &ctx).is_some(),
            "a bare call may still reach a function"
        );
        let ctx = Ctx::default()
            .file("src/a.rs", &source.replace("use crate::x::helper;\n", ""))
            .lower("helper", lone("helper", NodeKind::Function));
        assert!(
            fuzzy("helper", 3, &ctx).is_none(),
            "another file's function only through a `use`"
        );
    }

    #[test]
    fn rust_scope_gate_declines_any_strategys_out_of_scope_target() {
        // Upstream #2214 `gateRustScope`: whichever strategy chose another
        // file's item for a bare Rust name, it stands only when in scope.
        let helper = mk(
            "function:helper",
            NodeKind::Function,
            "helper",
            "helper",
            "src/util.rs",
            Language::Rust,
        );
        let chosen = |reference: &RefView| ResolvedRef {
            original: reference.clone(),
            target_node_id: "function:helper".to_string(),
            confidence: 0.9,
            resolved_by: ResolvedBy::QualifiedName,
        };
        let gate = |source: &str, reference: &RefView| {
            let ctx = Ctx::default()
                .file(&reference.file_path, source)
                .node_by_id(helper.clone());
            crate::rust_scope::gate_rust_scope(Some(chosen(reference)), reference, &ctx)
                .map(|r| r.target_node_id)
        };
        let source = "fn f() {\n    helper();\n}\n";
        let bare = at_column("helper", EdgeKind::Calls, "src/a.rs", Language::Rust, 2, 4);
        assert_eq!(gate(source, &bare), None, "no `use` brings it in");
        let imported = format!("{source}use crate::util::helper;\n");
        assert_eq!(
            gate(&imported, &bare).as_deref(),
            Some("function:helper"),
            "a `use` binds it"
        );
        let globbed = format!("{source}use crate::util::*;\n");
        assert_eq!(
            gate(&globbed, &bare).as_deref(),
            Some("function:helper"),
            "a glob over its module brings it in"
        );
        // Only bare Rust names are judged.
        let dotted = at_column(
            "x.helper",
            EdgeKind::Calls,
            "src/a.rs",
            Language::Rust,
            2,
            4,
        );
        assert_eq!(gate(source, &dotted).as_deref(), Some("function:helper"));
        let other = at_column(
            "helper",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
            2,
            4,
        );
        assert_eq!(gate(source, &other).as_deref(), Some("function:helper"));
    }

    #[test]
    fn a_rust_outside_type_or_module_path_receiver_is_read_by_its_case() {
        // Upstream #2177: a CamelCase receiver is a type, `Self` the impl's
        // own and a SCREAMING_CASE one a static; a lowercase path a module.
        assert!(names_external_type("Vec", Language::Rust));
        assert!(names_external_type("String", Language::Rust));
        assert!(!names_external_type("Self", Language::Rust));
        assert!(!names_external_type("REQ_ID", Language::Rust));
        assert!(!names_external_type("task", Language::Rust));
        assert!(is_rust_module_path("task"));
        assert!(is_rust_module_path("std::io"));
        assert!(is_rust_module_path("_private::fooBar"));
        assert!(!is_rust_module_path("io::Stdout"));
        assert!(!is_rust_module_path("Vec"));
    }

    #[test]
    fn rust_std_methods_are_sorted_and_unique() {
        assert!(RUST_STD_METHODS.windows(2).all(|pair| pair[0] < pair[1]));
        for name in [
            "join",
            "iter",
            "as_str",
            "is_empty",
            "take",
            "get_or_init",
            "open",
            "path",
        ] {
            assert!(is_rust_std_method(name), "{name}");
        }
        for name in ["nodes_by_ids", "current_db", "downgrade", "json"] {
            assert!(!is_rust_std_method(name), "{name}");
        }
    }

    #[test]
    fn binding_sites_agree_with_the_full_search_for_plain_names() {
        let sources = [
            "const helper = 1;\nhelper();",
            "let { a, helper } = obj;\n",
            "var [x, helper] = arr;",
            "const { helper } = require('./m');\nhelper();",
            "const helper = await import('./m');",
            "const t = await helper();",
            "function helper() {}\n",
            "class helper {}\n",
            "function f(a, helper = 1, ...rest) { return helper; }",
            "const g = (x, helper) => helper(x);",
            "items.map(helper => helper.id);",
            "const h = (name: string = \"helper\") => name;",
            "call(\"helper\");\nhelper();",
            "obj.helper => 1;",
            "function helperish() {}\nconst helperX = 1;",
            "export const $helper = 1;",
            "const a = 1; const helper = require('./x'); const helper = 2;",
            "function f(\n  a: number,\n  helper?: string,\n) {}",
        ];
        for source in sources {
            let facts = SourceFacts::new(Arc::from(source));
            for name in ["helper", "$helper", "a", "helperish", "name"] {
                assert_eq!(
                    binds_at_sites(&facts, name),
                    binds_anywhere(source, name),
                    "{name:?} in {source:?}"
                );
            }
        }
    }

    #[test]
    fn local_binding_exempts_import_initialisers() {
        let ctx = Ctx::default()
            .file("a.js", "const { run } = require('./m');\nrun();\n")
            .file("b.js", "const run = makeRunner();\nrun();\n")
            .file("c.js", "function go(run) { run(); }\n");
        assert!(!is_locally_bound_js_name("run", "a.js", &ctx));
        assert!(is_locally_bound_js_name("run", "b.js", &ctx));
        assert!(is_locally_bound_js_name("run", "c.js", &ctx));
        assert!(!is_locally_bound_js_name("run", "missing.js", &ctx));
    }

    #[test]
    fn sealed_module_reads_code_not_literals() {
        let exported = {
            let mut node = mk(
                "function:x",
                NodeKind::Function,
                "x",
                "x",
                "exported.js",
                Language::JavaScript,
            );
            node.is_exported = true;
            node
        };
        let ctx = Ctx::default()
            .file(
                "template.js",
                "const t = `\nimport fake from \"fake\";\n`;\nfunction globalFn() {}\n",
            )
            .file(
                "sealed.js",
                "import fs from 'node:fs';\nfunction hidden() { return fs; }\n",
            )
            .file(
                "common.js",
                "import os from 'node:os';\nmodule.exports = { helper };\n",
            )
            .file(
                "comment.js",
                "// import x from 'y';\nfunction classic() {}\n",
            )
            .file("exported.js", "import a from './a';\nfunction x() {}\n")
            .file(
                "ambient.ts",
                "import a from './a';\ndeclare global { interface W {} }\n",
            )
            .nodes_in_file("exported.js", vec![exported]);
        assert!(
            !is_sealed_js_module("template.js", &ctx),
            "a template spelling import is data"
        );
        assert!(is_sealed_js_module("sealed.js", &ctx));
        assert!(
            !is_sealed_js_module("common.js", &ctx),
            "CommonJS exports stay visible"
        );
        assert!(
            !is_sealed_js_module("comment.js", &ctx),
            "a commented import is not a module marker"
        );
        assert!(
            !is_sealed_js_module("exported.js", &ctx),
            "an exported node needs no text scan"
        );
        assert!(
            !is_sealed_js_module("ambient.ts", &ctx),
            "declare global is an export"
        );
        assert!(!is_sealed_js_module("missing.js", &ctx));
    }

    #[test]
    fn json_require_constant_is_not_a_call_target() {
        let mut data = mk(
            "constant:data",
            NodeKind::Constant,
            "data",
            "data",
            "cfg.js",
            Language::JavaScript,
        );
        data.signature = Some("= require('./data.json');".to_string());
        let mut plain = data.clone();
        plain.signature = Some("= require('./data');".to_string());
        let ctx = Ctx::default().file(
            "cfg.js",
            "const data = require('./data.json');\nmodule.exports = { data };\n",
        );
        let call = refv("data", EdgeKind::Calls, "use.js", Language::JavaScript, 1);
        assert!(!is_js_name_target_visible(&data, &call, &ctx));
        assert!(is_js_name_target_visible(&plain, &call, &ctx));
        let value = refv(
            "data",
            EdgeKind::References,
            "use.js",
            Language::JavaScript,
            1,
        );
        assert!(
            is_js_name_target_visible(&data, &value, &ctx),
            "only calls are rejected"
        );
    }

    #[test]
    fn fuzzy_judges_the_survivor_instead_of_filtering_the_crowd() {
        let method = mk(
            "method:Other::save",
            NodeKind::Method,
            "save",
            "Other::save",
            "other.ts",
            Language::TypeScript,
        );
        let function = mk(
            "function:save",
            NodeKind::Function,
            "save",
            "save",
            "lib.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default()
            .file("a.ts", "save();\n")
            .lower("save", vec![method.clone(), function.clone()]);
        let reference = at_column("save", EdgeKind::Calls, "a.ts", Language::TypeScript, 1, 0);
        assert!(
            match_fuzzy(&reference, &ctx).is_none(),
            "two candidates stay ambiguous even though one is a method"
        );
        let lone_method = Ctx::default()
            .file("a.ts", "save();\n")
            .lower("save", vec![method]);
        assert!(
            match_fuzzy(&reference, &lone_method).is_none(),
            "a bare call never reaches a method"
        );
        let lone_function = Ctx::default()
            .file("a.ts", "save();\n")
            .lower("save", vec![function]);
        assert_eq!(
            match_fuzzy(&reference, &lone_function).map(|r| r.target_node_id),
            Some("function:save".to_string())
        );
    }

    #[test]
    fn fuzzy_declines_a_closure_but_c_nesting_stays_reachable() {
        let mut inner = mk(
            "function:outer::inner",
            NodeKind::Function,
            "inner",
            "outer::inner",
            "lib.js",
            Language::JavaScript,
        );
        inner.start_line = 2;
        inner.end_line = 3;
        let mut outer = mk(
            "function:outer",
            NodeKind::Function,
            "outer",
            "outer",
            "lib.js",
            Language::JavaScript,
        );
        outer.start_line = 1;
        outer.end_line = 4;
        let ctx = Ctx::default()
            .file("use.js", "x.inner();\n")
            .lower("inner", vec![inner.clone()])
            .qualified("outer", vec![outer.clone()]);
        let reference = at_column(
            "inner",
            EdgeKind::Calls,
            "use.js",
            Language::JavaScript,
            1,
            2,
        );
        assert!(
            match_fuzzy(&reference, &ctx).is_none(),
            "#1708: a closure is not reachable"
        );

        let mut c_inner = inner;
        c_inner.language = Language::C;
        c_inner.file_path = "pid.c".to_string();
        let mut c_outer = outer;
        c_outer.language = Language::C;
        c_outer.file_path = "pid.c".to_string();
        let c_ctx = Ctx::default()
            .file("main.c", "inner();\n")
            .lower("inner", vec![c_inner])
            .qualified("outer", vec![c_outer]);
        let c_reference = at_column("inner", EdgeKind::Calls, "main.c", Language::C, 1, 0);
        assert!(
            match_fuzzy(&c_reference, &c_ctx).is_some(),
            "C has no nested functions"
        );
    }

    #[test]
    fn exact_match_keeps_bare_go_calls_off_methods() {
        let method = mk(
            "method:T::run",
            NodeKind::Method,
            "run",
            "T::run",
            "t.go",
            Language::Go,
        );
        let function = mk(
            "function:run",
            NodeKind::Function,
            "run",
            "run",
            "run.go",
            Language::Go,
        );
        let ctx = Ctx::default()
            .file("main.go", "\trun()\n\tt.run()\n")
            .name("run", vec![method.clone(), function]);
        let bare = at_column("run", EdgeKind::Calls, "main.go", Language::Go, 1, 1);
        assert_eq!(
            match_by_exact_name(&bare, &ctx).map(|r| r.target_node_id),
            Some("function:run".to_string())
        );
        let only_method = Ctx::default()
            .file("main.go", "\trun()\n")
            .name("run", vec![method]);
        assert!(match_by_exact_name(&bare, &only_method).is_none());
    }

    // ================= import statements are never name-match targets ========

    /// Upstream #965 (`0a91d0f5`): an `import` node is a statement, not a
    /// definition. Neither another file's import of the same module nor the
    /// file's own is a candidate, so the reference stays unresolved.
    #[test]
    fn exact_match_never_offers_an_import_statement() {
        let theirs = mk(
            "import:widget-vue",
            NodeKind::Import,
            "vue",
            "vue",
            "src/Widget.svelte",
            Language::Svelte,
        );
        let ours = mk(
            "import:app-vue",
            NodeKind::Import,
            "vue",
            "vue",
            "src/App.vue",
            Language::Vue,
        );
        let ctx = Ctx::default().name("vue", vec![theirs, ours]);
        let reference = refv("vue", EdgeKind::Imports, "src/App.vue", Language::Vue, 6);
        assert_eq!(match_by_exact_name(&reference, &ctx), None);
    }

    /// An import statement named like a definition does not make the
    /// definition look ambiguous: the lone definition takes the single-match
    /// confidence, not a proximity-ranked one.
    #[test]
    fn an_import_statement_never_makes_a_definition_ambiguous() {
        let definition = mk(
            "method:json",
            NodeKind::Method,
            "json",
            "Response::json",
            "lib/response.rb",
            Language::Ruby,
        );
        let statement = mk(
            "import:json",
            NodeKind::Import,
            "json",
            "json",
            "app/main.rb",
            Language::Ruby,
        );
        let ctx = Ctx::default().name("json", vec![statement, definition]);
        let reference = refv("json", EdgeKind::Calls, "app/main.rb", Language::Ruby, 4);
        let resolved = match_by_exact_name(&reference, &ctx).expect("the definition resolves");
        assert_eq!(resolved.target_node_id, "method:json");
        assert_eq!(resolved.confidence, 0.9);
    }

    /// The fuzzy fallback considers callables only, so an import statement is
    /// never its guess either.
    #[test]
    fn fuzzy_match_never_offers_an_import_statement() {
        let statement = mk(
            "import:vue",
            NodeKind::Import,
            "Vue",
            "Vue",
            "src/Widget.svelte",
            Language::Svelte,
        );
        let ctx = Ctx::default().lower("vue", vec![statement]);
        let reference = refv("vue", EdgeKind::Calls, "src/App.vue", Language::Vue, 6);
        assert_eq!(match_fuzzy(&reference, &ctx), None);
    }
}
