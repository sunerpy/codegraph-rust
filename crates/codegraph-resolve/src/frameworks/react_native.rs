//! The React Native bridge (upstream `resolution/frameworks/react-native.ts`
//! at `e8ddc4e2`: `4d1a2b3c` #430, `07af3db6` #708's `rn-export` nodes,
//! `873f133c`'s `RCT_EXTERN` modules, aliases and receiver rule, `6557bc83`'s
//! wrong-module guard).
//!
//! A project is the bridge's when its root `package.json` names
//! `react-native`, or one of the first 200 indexed paths is a `.m`/`.mm`
//! file writing `RCT_EXPORT_MODULE` or `RCT_EXTERN_MODULE` or a `.ts`/`.tsx`
//! file registering a TurboModule. It extracts each `.m`/`.mm` file writing
//! `RCT_EXPORT_MODULE`: one `method` node per JavaScript-visible name of its
//! `RCT_EXPORT_METHOD`s and `RCT_REMAP_METHOD`s (`rn-export:` ids), which the
//! Objective-C extractor makes no node of. It reads JavaScript-family
//! references only.
//!
//! The JavaScript-visible methods come from every file's text
//! ([`BridgeFacts`]): an Objective-C export reaches the Objective-C method of
//! its native first keyword in its own file, else the project's sole one;
//! an `RCT_EXTERN` entry the Swift method of its native keyword on the class
//! the shim names; a JVM `@ReactMethod` the JVM method of its name in its
//! own file, else the project's sole one; a TurboModule spec's method every
//! Objective-C and JVM method of its name. A call `NativeModules.Mod.m()`,
//! or `alias.m()` through `const alias = NativeModules.Mod` (an alias two
//! modules share names none), takes module `Mod`'s method at `0.95`, or
//! nothing when the module has none; any other call by the method's name
//! takes one at `0.6`. Both prefer an Objective-C method, then a Swift one,
//! then any.
//!
//! Rust's own (KNOWN_DIFFS "The bridge a name means", KEEP-RUST): where
//! upstream takes the first of the sorted candidates (an export's target
//! outside its own file, an `RCT_EXTERN` class's method, a JVM export's
//! target, the method of the preferred platform), Rust takes only the sole
//! one and nothing when there are several, without falling to the next
//! platform. And the `0.6` answer is not given for the two reference
//! shapes upstream's resolution never lets a framework candidate through
//! (`isUnresolvedJsMemberCall` and `CHAIN_SHAPE`), where Rust's resolver
//! keeps framework candidates.
//!
//! Every pattern runs on the raw text, as upstream's: a commented-out macro
//! counts.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde::{Deserialize, Serialize};

use super::expo_modules::{ExpoBindings, expo_bindings_of};
use super::framework_node;
use super::js_scan::{JS_SPACE, js_trim, multiline_matches};
use super::swift_objc::{node_is_objc_exposed, node_order};
use crate::framework::{FrameworkExtractionContext, FrameworkResolver};
use crate::types::{
    EdgeShape, FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedBy,
    ResolvedRef,
};

/// The resolver's name, which its edges carry in `metadata.framework`.
pub const NAME: &str = "react-native-bridge";

/// The languages it extracts: the Objective-C files of `rn-export` nodes.
const EXTRACT_LANGUAGES: [Language; 1] = [Language::ObjC];

/// The languages whose references it reads (upstream's `resolve` answers
/// JavaScript-family references only).
pub(crate) const JS_FAMILY: [Language; 4] = [
    Language::JavaScript,
    Language::TypeScript,
    Language::Tsx,
    Language::Jsx,
];

/// What a file's text holds when it holds any [`BridgeFacts`]: each fact
/// needs one of them.
pub const FACT_NEEDLES: [&str; 6] = [
    "RCT_EXPORT_METHOD",
    "RCT_REMAP_METHOD",
    "RCT_EXTERN_",
    "NativeModule",
    "@ReactMethod",
    "TurboModuleRegistry",
];

/// The `project_metadata` key prefix of a file's bridge-facts row
/// (`codegraph_watch::rn_bridges`).
pub const ROW_KEY_PREFIX: &str = "rn_bridge_surface:";

/// The key of `path`'s bridge-facts row.
pub fn row_key(path: &str) -> String {
    format!("{ROW_KEY_PREFIX}{path}")
}

/// RCTEventEmitter's own methods, which every emitter has and no JavaScript
/// calls directly (upstream `RN_EMITTER_BUILTINS`).
const RN_EMITTER_BUILTINS: [&str; 6] = [
    "addListener",
    "removeListeners",
    "remove",
    "invalidate",
    "startObserving",
    "stopObserving",
];

/// A pattern ported from upstream: `{S}` is JavaScript's `\s` and `{W}` the
/// body of its ASCII `\w`; its `\b` is written `(?-u:\b)`.
pub(crate) fn js_regex(pattern: &str) -> Regex {
    Regex::new(
        &pattern
            .replace("{S}", JS_SPACE)
            .replace("{W}", "A-Za-z0-9_"),
    )
    .expect("bridge pattern")
}

macro_rules! patterns {
    ($($(#[$doc:meta])* $name:ident = $pattern:expr;)*) => {
        $(
            $(#[$doc])*
            fn $name() -> &'static Regex {
                static PATTERN: OnceLock<Regex> = OnceLock::new();
                PATTERN.get_or_init(|| js_regex($pattern))
            }
        )*
    };
}

patterns! {
    /// `RCT_EXPORT_MODULE(name)`, the name optional.
    export_module = r"RCT_EXPORT_MODULE{S}*\({S}*([A-Za-z_][A-Za-z0-9_]*)?{S}*\)";
    /// What a `.m`/`.mm` file writes when it exports a module.
    export_module_marker = r"RCT_EXPORT_MODULE(?-u:\b)";
    /// The `@implementation` class (upstream `findObjcClassName`).
    objc_implementation = r"@implementation{S}+([A-Za-z_][A-Za-z0-9_]*)";
    /// `RCT_EXPORT_METHOD(first…`.
    export_method = r"RCT_EXPORT_METHOD{S}*\({S}*([A-Za-z_][A-Za-z0-9_]*)";
    /// `RCT_REMAP_METHOD(js, native…`.
    remap_method = r"RCT_REMAP_METHOD{S}*\({S}*([A-Za-z_][A-Za-z0-9_]*){S}*,{S}*([A-Za-z_][A-Za-z0-9_]*)";
    /// `RCT_EXTERN_REMAP_MODULE(js, Class, …`.
    extern_remap_module = r"RCT_EXTERN_REMAP_MODULE{S}*\({S}*([A-Za-z_][A-Za-z0-9_]*){S}*,{S}*([A-Za-z_][A-Za-z0-9_]*)";
    /// `RCT_EXTERN_MODULE(Class, …`.
    extern_module = r"RCT_EXTERN_MODULE{S}*\({S}*([A-Za-z_][A-Za-z0-9_]*)";
    /// `RCT_EXTERN_METHOD(first…`.
    extern_method = r"RCT_EXTERN_METHOD{S}*\({S}*([A-Za-z_][A-Za-z0-9_]*)";
    /// `RCT_EXTERN_REMAP_METHOD(js, native…`.
    extern_remap_method = r"RCT_EXTERN_REMAP_METHOD{S}*\({S}*([A-Za-z_][A-Za-z0-9_]*){S}*,{S}*([A-Za-z_][A-Za-z0-9_]*)";
    /// What a `.m`/`.mm` shim of a Swift module writes.
    extern_marker = r"RCT_EXTERN_(?:REMAP_)?MODULE(?-u:\b)";
    /// What a `.m`/`.mm` file detection reads holds.
    detect_objc = r"RCT_EXPORT_MODULE(?-u:\b)|RCT_EXTERN_(?:REMAP_)?MODULE(?-u:\b)";
    /// What a `.ts`/`.tsx` file detection reads holds.
    detect_turbo = r"TurboModuleRegistry\.(?:get|getEnforcing){S}*<";
    /// A `package.json` dependency on React Native.
    react_native_dependency = r#"["']react-native["']{S}*:"#;
    /// `const alias [: T] = NativeModules.Mod`.
    alias_direct = r"(?-u:\b)(?:const|let|var){S}+([A-Za-z_$][{W}$]*){S}*(?::[^=;]+)?={S}*NativeModules\.([A-Z][{W}$]*)";
    /// `const { A, B: b } = NativeModules`.
    alias_destructured = r"(?-u:\b)(?:const|let|var){S}*\{([^}]+)\}{S}*={S}*NativeModules(?-u:\b)";
    /// One destructured part: `A` or `B: b`.
    alias_part = r"^([A-Za-z_$][{W}$]*)(?:{S}*:{S}*([A-Za-z_$][{W}$]*))?$";
    /// A JVM module's `getName()` literal.
    jvm_get_name = r#"(?-u:\b)getName{S}*\([^)]*\){S}*(?::{S}*String)?{S}*(?:={S}*|\{[^}]*return{S}*)"([^"]+)""#;
    /// A JVM module class.
    jvm_module_class = r"(?-u:\b)class{S}+([A-Za-z_][A-Za-z0-9_]*)(?-u:\b)[^{]*ReactContextBaseJavaModule";
    /// A JVM package class.
    jvm_package_class = r"(?-u:\b)class{S}+([A-Za-z_][A-Za-z0-9_]*)(?-u:\b)[^{]*ReactPackage";
    /// A `@ReactMethod` and the method after it.
    jvm_react_method = r"@ReactMethod(?-u:\b)[^{]*?(?:(?-u:\b)fun{S}+|(?-u:\b)void{S}+|(?-u:\b)public{S}+[{W}][{W}<>\[\]]*{S}+)([A-Za-z_][A-Za-z0-9_]*){S}*\(";
    /// `TurboModuleRegistry.get[Enforcing]<Spec>('Name')`.
    turbo_registration = r#"TurboModuleRegistry\.(?:getEnforcing|get){S}*<[^>]*>{S}*\({S}*['"]([^'"]+)['"]{S}*\)"#;
    /// `export interface Spec … {…\n}`.
    turbo_spec = r"export{S}+interface{S}+Spec(?-u:\b)[^{]*\{((?s:.)*?)\n\}";
    /// A spec method at a line start: `name(`.
    turbo_method_at = r"^{S}*([A-Za-z_][A-Za-z0-9_]*){S}*\(";
    /// A receiver naming its module: `NativeModules.Mod`.
    native_modules_receiver = r"^NativeModules\.([A-Z][{W}$]*)$";
    /// Upstream's `CHAIN_SHAPE` (`/^(.+)\(\)\.(\w+)$/`), `.` matching no line
    /// terminator and `\w` ASCII.
    chain_shape = r"^[^\n\r\x{2028}\x{2029}]+\(\)\.[{W}]+$";
    /// A JavaScript-family path (upstream `/\.(?:[cm]?[jt]sx?)$/`).
    js_family_path = r"\.(?:[cm]?[jt]sx?)$";
}

/// Whether `path` is a JavaScript-family file by upstream's extension test.
pub fn is_js_family_path(path: &str) -> bool {
    js_family_path().is_match(path)
}

/// The 1-based line of a byte offset (upstream counts the `\n`s before it).
fn line_of(source: &str, offset: usize) -> i64 {
    1 + source.as_bytes()[..offset.min(source.len())]
        .iter()
        .filter(|&&byte| byte == b'\n')
        .count() as i64
}

// ---------------------------------------------------------------------------
// Parsers
// ---------------------------------------------------------------------------

/// One `RCT_EXPORT_METHOD` or `RCT_REMAP_METHOD` (upstream
/// `parseObjcRNExports`' entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjcExport {
    pub module_name: String,
    pub js_name: String,
    pub native_kw: String,
    pub line: i64,
}

/// One `RCT_EXTERN_METHOD` or `RCT_EXTERN_REMAP_METHOD` (upstream
/// `parseObjcRNExterns`' entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjcExtern {
    pub module_name: String,
    pub class_name: String,
    pub js_name: String,
    pub native_kw: String,
    pub line: i64,
}

/// A class name without a leading `RCT` (upstream `defaultObjcModuleName`).
fn default_objc_module_name(class_name: &str) -> &str {
    if class_name.starts_with("RCT") && class_name.len() > 3 {
        &class_name[3..]
    } else {
        class_name
    }
}

/// The `@implementation` class an Objective-C file names first (upstream
/// `findObjcClassName`).
pub fn find_objc_class_name(source: &str) -> Option<&str> {
    objc_implementation()
        .captures(source)
        .and_then(|found| found.get(1))
        .map(|name| name.as_str())
}

/// An Objective-C file's exported methods (upstream `parseObjcRNExports`):
/// the module is `RCT_EXPORT_MODULE`'s argument, else the class name
/// without `RCT`; each `RCT_EXPORT_METHOD` names the native first keyword,
/// then each `RCT_REMAP_METHOD` a JavaScript name and its native keyword.
pub fn parse_objc_rn_exports(source: &str, class_name: Option<&str>) -> Vec<ObjcExport> {
    let explicit = export_module()
        .captures(source)
        .and_then(|found| found.get(1))
        .map(|name| name.as_str());
    let Some(module) = explicit.or_else(|| class_name.map(default_objc_module_name)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for found in export_method().captures_iter(source) {
        let keyword = &found[1];
        out.push(ObjcExport {
            module_name: module.to_string(),
            js_name: keyword.to_string(),
            native_kw: keyword.to_string(),
            line: line_of(source, found.get(0).map_or(0, |whole| whole.start())),
        });
    }
    for found in remap_method().captures_iter(source) {
        out.push(ObjcExport {
            module_name: module.to_string(),
            js_name: found[1].to_string(),
            native_kw: found[2].to_string(),
            line: line_of(source, found.get(0).map_or(0, |whole| whole.start())),
        });
    }
    out
}

/// A Swift module's `.m` shim (upstream `parseObjcRNExterns`): the module
/// and class `RCT_EXTERN_REMAP_MODULE(js, Class, …)` or
/// `RCT_EXTERN_MODULE(Class, …)` name, then each `RCT_EXTERN_METHOD`, then
/// each `RCT_EXTERN_REMAP_METHOD`.
pub fn parse_objc_rn_externs(source: &str) -> Vec<ObjcExtern> {
    let remap = extern_remap_module().captures(source);
    let plain = extern_module().captures(source);
    let module = remap
        .as_ref()
        .and_then(|found| found.get(1))
        .or_else(|| plain.as_ref().and_then(|found| found.get(1)));
    let class = remap
        .as_ref()
        .and_then(|found| found.get(2))
        .or_else(|| plain.as_ref().and_then(|found| found.get(1)));
    let (Some(module), Some(class)) = (module, class) else {
        return Vec::new();
    };
    let (module, class) = (module.as_str(), class.as_str());
    let mut out = Vec::new();
    for found in extern_method().captures_iter(source) {
        let keyword = &found[1];
        out.push(ObjcExtern {
            module_name: module.to_string(),
            class_name: class.to_string(),
            js_name: keyword.to_string(),
            native_kw: keyword.to_string(),
            line: line_of(source, found.get(0).map_or(0, |whole| whole.start())),
        });
    }
    for found in extern_remap_method().captures_iter(source) {
        out.push(ObjcExtern {
            module_name: module.to_string(),
            class_name: class.to_string(),
            js_name: found[1].to_string(),
            native_kw: found[2].to_string(),
            line: line_of(source, found.get(0).map_or(0, |whole| whole.start())),
        });
    }
    out
}

/// The names a JavaScript file binds to native modules, in the order
/// upstream binds them: every `const alias = NativeModules.Mod`, then every
/// destructured `const { Mod, Other: alias } = NativeModules`.
pub fn native_module_bindings(source: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for found in alias_direct().captures_iter(source) {
        out.push((found[1].to_string(), found[2].to_string()));
    }
    for found in alias_destructured().captures_iter(source) {
        for part in found[1].split(',') {
            if let Some(entry) = alias_part().captures(js_trim(part)) {
                let module = entry[1].to_string();
                let alias = entry
                    .get(2)
                    .map_or(module.clone(), |alias| alias.as_str().to_string());
                out.push((alias, module));
            }
        }
    }
    out
}

/// The project's native-module aliases (upstream `collectNativeModuleAliases`'
/// two collections): a name bound to two modules is dropped and stays
/// dropped.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct NativeModuleAliases {
    /// The bound names and their modules, in the order they were bound.
    entries: Vec<(String, String)>,
    /// The names bound to more than one module.
    ambiguous: BTreeSet<String>,
}

impl NativeModuleAliases {
    /// Bind `alias` to `module`, as upstream's `bind`.
    pub fn bind(&mut self, alias: &str, module: &str) {
        if self.ambiguous.contains(alias) {
            return;
        }
        if let Some(at) = self.entries.iter().position(|(name, _)| name == alias) {
            if self.entries[at].1 != module {
                self.entries.remove(at);
                self.ambiguous.insert(alias.to_string());
            }
            return;
        }
        self.entries.push((alias.to_string(), module.to_string()));
    }

    /// The module `alias` names, when it names one.
    pub fn get(&self, alias: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(name, _)| name == alias)
            .map(|(_, module)| module.as_str())
    }

    /// Whether `alias` was bound to two modules.
    pub fn is_ambiguous(&self, alias: &str) -> bool {
        self.ambiguous.contains(alias)
    }

    /// The bound names and modules, in binding order.
    pub fn entries(&self) -> &[(String, String)] {
        &self.entries
    }
}

/// Bind the native-module aliases `source` writes into `aliases` (upstream
/// `collectNativeModuleAliases`).
pub fn collect_native_module_aliases(source: &str, aliases: &mut NativeModuleAliases) {
    for (alias, module) in native_module_bindings(source) {
        aliases.bind(&alias, &module);
    }
}

/// A JVM module's `@ReactMethod`s, as `(module, js_name)` (upstream
/// `parseJvmRNExports`): the module is `getName()`'s literal, else the
/// module or package class's name without a trailing `Module`.
pub fn parse_jvm_rn_exports(source: &str) -> Vec<(String, String)> {
    let literal = jvm_get_name()
        .captures(source)
        .and_then(|found| found.get(1))
        .map(|name| name.as_str().to_string());
    let module = literal.or_else(|| {
        jvm_module_class()
            .captures(source)
            .or_else(|| jvm_package_class().captures(source))
            .and_then(|found| found.get(1))
            .map(|class| {
                let class = class.as_str();
                class.strip_suffix("Module").unwrap_or(class).to_string()
            })
    });
    let Some(module) = module.filter(|module| !module.is_empty()) else {
        return Vec::new();
    };
    jvm_react_method()
        .captures_iter(source)
        .map(|found| (module.clone(), found[1].to_string()))
        .collect()
}

/// A TurboModule spec: its registered name and its interface's methods.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurboSpec {
    pub module: String,
    pub methods: Vec<String>,
}

/// A TypeScript TurboModule spec (upstream `parseTurboModuleSpec`): the name
/// its `TurboModuleRegistry.get…<Spec>('Name')` registers and each method
/// line of its `export interface Spec`.
pub fn parse_turbo_module_spec(source: &str) -> Option<TurboSpec> {
    let module = turbo_registration().captures(source)?.get(1)?.as_str();
    let body = turbo_spec().captures(source)?.get(1)?.as_str();
    let methods = multiline_matches(turbo_method_at(), body)
        .into_iter()
        .map(|(_, found)| found[1].to_string())
        .collect();
    Some(TurboSpec {
        module: module.to_string(),
        methods,
    })
}

// ---------------------------------------------------------------------------
// A file's facts
// ---------------------------------------------------------------------------

/// One export or `RCT_EXTERN` entry as a file's facts hold it: no line, so
/// a move is no change.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BridgeEntry {
    pub module: String,
    /// The Swift class an `RCT_EXTERN` entry names; empty for an export.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub class: String,
    pub js_name: String,
    pub native_kw: String,
}

/// What one file holds for the React Native and Expo bridges (W7-21): what
/// [`ReactNativeBridgeResolver`] and [`super::expo_modules`] read of its
/// text, and for a Swift file which of its methods and functions are
/// exposed to Objective-C. A sync keeps it as the file's
/// [`ROW_KEY_PREFIX`] row, so the bridges' tables need not read every file
/// again, and compares a changed file's row with its new facts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeFacts {
    /// A `.m`/`.mm` file's exports, in [`parse_objc_rn_exports`]' order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exports: Vec<BridgeEntry>,
    /// A `.m`/`.mm` shim's `RCT_EXTERN` entries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub externs: Vec<BridgeEntry>,
    /// A JavaScript-family file's native-module bindings, `(alias,
    /// module)` in binding order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<(String, String)>,
    /// A `.java`/`.kt` file's `@ReactMethod`s, `(module, js_name)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jvm_exports: Vec<(String, String)>,
    /// A `.ts`/`.tsx` file's TurboModule spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turbo_spec: Option<TurboSpec>,
    /// The `requireNativeModule` bindings, in any file holding
    /// `NativeModule`.
    #[serde(default, skip_serializing_if = "ExpoBindings::is_empty")]
    pub expo: ExpoBindings,
    /// A Swift file's methods and functions, each with its qualified name
    /// and whether its text exposes it to Objective-C, sorted (rows only):
    /// an `RCT_EXTERN` entry reaches a method by its class, which no node
    /// id holds, and an Objective-C message an exposed one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub swift_methods: Vec<SwiftMethodFact>,
}

/// One Swift method or function of a file's facts: its name, its qualified
/// name and whether it is exposed to Objective-C.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SwiftMethodFact(pub String, pub String, pub bool);

impl BridgeFacts {
    /// What `text`, the file at `path`, holds for the bridges' tables.
    pub fn of_text(path: &str, text: &str) -> Self {
        let mut facts = Self::default();
        if !FACT_NEEDLES.iter().any(|needle| text.contains(needle)) {
            return facts;
        }
        if path.ends_with(".m") || path.ends_with(".mm") {
            facts.exports = parse_objc_rn_exports(text, find_objc_class_name(text))
                .into_iter()
                .map(|export| BridgeEntry {
                    module: export.module_name,
                    class: String::new(),
                    js_name: export.js_name,
                    native_kw: export.native_kw,
                })
                .collect();
            if extern_marker().is_match(text) {
                facts.externs = parse_objc_rn_externs(text)
                    .into_iter()
                    .map(|entry| BridgeEntry {
                        module: entry.module_name,
                        class: entry.class_name,
                        js_name: entry.js_name,
                        native_kw: entry.native_kw,
                    })
                    .collect();
            }
        }
        if is_js_family_path(path) && text.contains("NativeModules") {
            facts.aliases = native_module_bindings(text);
        }
        if path.ends_with(".java") || path.ends_with(".kt") {
            facts.jvm_exports = parse_jvm_rn_exports(text);
        }
        if path.ends_with(".ts") || path.ends_with(".tsx") {
            facts.turbo_spec = parse_turbo_module_spec(text);
        }
        if text.contains("NativeModule") {
            facts.expo = expo_bindings_of(text);
        }
        facts
    }

    /// [`Self::of_text`] with a Swift file's methods: `nodes`' Swift methods
    /// and functions, each with its qualified name and whether `text`
    /// exposes it to Objective-C.
    pub fn of_file(path: &str, text: &str, nodes: &[Node]) -> Self {
        let mut facts = Self::of_text(path, text);
        if path.ends_with(".swift") {
            facts.swift_methods = swift_methods(text, nodes);
        }
        facts
    }

    /// Whether the facts hold nothing, so no row is kept.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn decode(value: &str) -> Option<Self> {
        serde_json::from_str(value).ok()
    }
}

/// Whether `node` is one a bridge's extraction made rather than a
/// language's extractor: its row's facts hold no exposure for it.
pub fn is_bridge_node(node: &Node) -> bool {
    node.id.starts_with("rn-export:")
        || node.id.starts_with("expo-module:")
        || node.id.starts_with("fabric-")
}

/// The Swift methods and functions among `nodes` (an extractor's, not a
/// bridge's), each with its qualified name and whether `text` exposes it
/// to Objective-C, sorted.
pub fn swift_methods(text: &str, nodes: &[Node]) -> Vec<SwiftMethodFact> {
    let mut out: Vec<SwiftMethodFact> = nodes
        .iter()
        .filter(|node| {
            node.language == Language::Swift
                && matches!(node.kind, NodeKind::Method | NodeKind::Function)
                && !is_bridge_node(node)
        })
        .map(|node| {
            SwiftMethodFact(
                node.name.clone(),
                node.qualified_name.clone(),
                node_is_objc_exposed(text, node),
            )
        })
        .collect();
    out.sort();
    out
}

/// The node every one of `nodes` is, by id, or `None` when they are none or
/// several (W7-21's "exactly one" counts distinct target nodes).
pub(crate) fn sole_node<'n>(
    nodes: impl IntoIterator<Item = &'n Arc<Node>>,
) -> Option<&'n Arc<Node>> {
    let mut nodes = nodes.into_iter();
    let first = nodes.next()?;
    nodes.all(|node| node.id == first.id).then_some(first)
}

/// Whether the bridges' tables read `path`'s facts: an Objective-C,
/// JavaScript-family, Java or Kotlin file.
fn holds_rn_facts(path: &str) -> bool {
    path.ends_with(".m")
        || path.ends_with(".mm")
        || path.ends_with(".java")
        || path.ends_with(".kt")
        || is_js_family_path(path)
}

/// The non-empty facts of `files` read from their text, in `files`' order:
/// the ones [`ResolutionContext::files_containing`] finds holding a
/// [`FACT_NEEDLES`] needle (what [`ResolutionContext::bridge_facts_of_files`]
/// reads by default).
pub fn bridge_facts_from_text<C: ResolutionContext + ?Sized>(
    context: &C,
    files: &[String],
) -> Vec<(String, Arc<BridgeFacts>)> {
    context
        .files_containing(files, &FACT_NEEDLES)
        .into_iter()
        .filter_map(|file| {
            let text = context.read_file_shared(&file)?;
            let facts = BridgeFacts::of_text(&file, &text);
            (!facts.is_empty()).then(|| (file, Arc::new(facts)))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The tables
// ---------------------------------------------------------------------------

/// One native method a JavaScript name reaches (upstream `NativeMethod`).
#[derive(Debug, Clone)]
struct RnEntry {
    module: String,
    node: Arc<Node>,
}

/// The JavaScript-visible native methods by name and the project's
/// native-module aliases (upstream `buildRNMaps`), built once a resolution
/// pass.
#[derive(Default)]
struct RnMaps {
    by_js_name: HashMap<String, Vec<RnEntry>>,
    aliases: NativeModuleAliases,
}

impl RnMaps {
    fn build(context: &dyn ResolutionContext) -> Self {
        let mut methods = context.get_nodes_by_kind_in_languages_shared(
            NodeKind::Method,
            &[
                Language::ObjC,
                Language::Java,
                Language::Kotlin,
                Language::Swift,
            ],
        );
        methods.sort_by(|a, b| node_order(a, b));
        let mut objc_by_kw: HashMap<String, Vec<Arc<Node>>> = HashMap::new();
        let mut jvm_by_name: HashMap<String, Vec<Arc<Node>>> = HashMap::new();
        let mut swift_by_name: HashMap<String, Vec<Arc<Node>>> = HashMap::new();
        for node in methods {
            match node.language {
                Language::ObjC => {
                    let keyword = node.name.split(':').next().unwrap_or_default();
                    if !keyword.is_empty() {
                        objc_by_kw
                            .entry(keyword.to_string())
                            .or_default()
                            .push(Arc::clone(&node));
                    }
                }
                Language::Java | Language::Kotlin => {
                    jvm_by_name.entry(node.name.clone()).or_default().push(node)
                }
                _ => swift_by_name
                    .entry(node.name.clone())
                    .or_default()
                    .push(node),
            }
        }
        let files: Vec<String> = context
            .get_all_files_shared()
            .iter()
            .filter(|file| holds_rn_facts(file))
            .cloned()
            .collect();
        let mut maps = Self::default();
        // A file's own method of the name, else the project's sole one.
        let own_or_sole = |candidates: Option<&Vec<Arc<Node>>>, file: &str| {
            let candidates = candidates?;
            candidates
                .iter()
                .find(|node| node.file_path == file)
                .or_else(|| sole_node(candidates))
                .cloned()
        };
        for (file, facts) in context.bridge_facts_of_files(&files) {
            for export in &facts.exports {
                if RN_EMITTER_BUILTINS.contains(&export.js_name.as_str()) {
                    continue;
                }
                if let Some(node) = own_or_sole(objc_by_kw.get(&export.native_kw), &file) {
                    maps.add(&export.js_name, &export.module, node);
                }
            }
            for entry in &facts.externs {
                if RN_EMITTER_BUILTINS.contains(&entry.js_name.as_str()) {
                    continue;
                }
                let on_class = swift_by_name
                    .get(&entry.native_kw)
                    .into_iter()
                    .flatten()
                    .filter(|node| {
                        node.qualified_name
                            .split("::")
                            .any(|seg| seg == entry.class)
                    });
                if let Some(node) = sole_node(on_class) {
                    maps.add(&entry.js_name, &entry.module, Arc::clone(node));
                }
            }
            for (alias, module) in &facts.aliases {
                maps.aliases.bind(alias, module);
            }
            for (module, js_name) in &facts.jvm_exports {
                if RN_EMITTER_BUILTINS.contains(&js_name.as_str()) {
                    continue;
                }
                if let Some(node) = own_or_sole(jvm_by_name.get(js_name), &file) {
                    maps.add(js_name, module, node);
                }
            }
            if let Some(spec) = &facts.turbo_spec {
                for method in &spec.methods {
                    if RN_EMITTER_BUILTINS.contains(&method.as_str()) {
                        continue;
                    }
                    let nodes = objc_by_kw
                        .get(method)
                        .into_iter()
                        .chain(jvm_by_name.get(method))
                        .flatten();
                    for node in nodes {
                        maps.add(method, &spec.module, Arc::clone(node));
                    }
                }
            }
        }
        maps
    }

    fn add(&mut self, js_name: &str, module: &str, node: Arc<Node>) {
        self.by_js_name
            .entry(js_name.to_string())
            .or_default()
            .push(RnEntry {
                module: module.to_string(),
                node,
            });
    }
}

/// The entry upstream's `pick` prefers: of the Objective-C entries, else
/// the Swift ones, else any, the one that reaches the sole native method
/// there (KEEP-RUST: upstream takes the first; several leave none, and the
/// next platform is not tried).
fn pick<'e>(entries: impl Iterator<Item = &'e RnEntry> + Clone) -> Option<&'e RnEntry> {
    let platforms: [fn(&RnEntry) -> bool; 3] = [
        |entry| entry.node.language == Language::ObjC,
        |entry| entry.node.language == Language::Swift,
        |entry| !matches!(entry.node.language, Language::ObjC | Language::Swift),
    ];
    for in_platform in platforms {
        let chosen: Vec<&RnEntry> = entries.clone().filter(|entry| in_platform(entry)).collect();
        if chosen.is_empty() {
            continue;
        }
        let nodes: HashSet<&str> = chosen.iter().map(|entry| entry.node.id.as_str()).collect();
        return (nodes.len() == 1).then(|| chosen[0]);
    }
    None
}

/// The two reference shapes whose framework candidates upstream's
/// resolution drops (`resolution-index.ts` 1648-1671): an untyped member
/// chain of three or more segments, and a call on a call's result.
fn upstream_drops_candidates(reference: &RefView) -> bool {
    crate::resolver::is_unresolved_js_member_call(reference)
        || (reference.reference_kind == EdgeKind::Calls
            && chain_shape().is_match(&reference.reference_name))
}

// ---------------------------------------------------------------------------
// The resolver
// ---------------------------------------------------------------------------

/// The resolver.
pub struct ReactNativeBridgeResolver;

impl FrameworkResolver for ReactNativeBridgeResolver {
    fn name(&self) -> &str {
        NAME
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&EXTRACT_LANGUAGES)
    }

    fn resolve_languages(&self) -> Option<&[Language]> {
        Some(&JS_FAMILY)
    }

    /// The root `package.json` naming `react-native`, else one of the first
    /// 200 indexed paths: a `.m`/`.mm` file writing `RCT_EXPORT_MODULE` or
    /// `RCT_EXTERN_MODULE`, or a `.ts`/`.tsx` file registering a
    /// TurboModule.
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        if context
            .read_file_shared("package.json")
            .is_some_and(|package| react_native_dependency().is_match(&package))
        {
            return true;
        }
        let files = context.get_all_files_shared();
        let window = &files[..files.len().min(200)];
        let objc: Vec<String> = window
            .iter()
            .filter(|file| file.ends_with(".mm") || file.ends_with(".m"))
            .cloned()
            .collect();
        let typescript: Vec<String> = window
            .iter()
            .filter(|file| file.ends_with(".ts") || file.ends_with(".tsx"))
            .cloned()
            .collect();
        let holds = |files: &[String], needles: &[&str], pattern: &Regex| {
            context.files_containing(files, needles).iter().any(|file| {
                context
                    .read_file_shared(file)
                    .is_some_and(|text| pattern.is_match(&text))
            })
        };
        holds(&objc, &["RCT_EXPORT_MODULE", "RCT_EXTERN_"], detect_objc())
            || holds(&typescript, &["TurboModuleRegistry"], detect_turbo())
    }

    /// An `RCT_EXPORT_MODULE` file's exported methods, one `method` node per
    /// JavaScript name (upstream #708 `extract`).
    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        let mut result = FrameworkResolverExtractionResult::default();
        if !(file_path.ends_with(".m") || file_path.ends_with(".mm"))
            || !export_module_marker().is_match(content)
        {
            return Some(result);
        }
        let mut seen: HashSet<String> = HashSet::new();
        for export in parse_objc_rn_exports(content, find_objc_class_name(content)) {
            if !seen.insert(export.js_name.clone()) {
                continue;
            }
            let mut node = framework_node(
                format!(
                    "rn-export:{file_path}:{}.{}",
                    export.module_name, export.js_name
                ),
                NodeKind::Method,
                export.js_name.clone(),
                format!("{file_path}::{}.{}", export.module_name, export.js_name),
                file_path.to_string(),
                export.line,
                export.line,
                0,
                0,
                Language::ObjC,
                true,
            );
            node.docstring = Some(format!(
                "RCT_EXPORT_METHOD {} (module {})",
                export.native_kw, export.module_name
            ));
            node.signature = Some(format!("RCT_EXPORT_METHOD({}:\u{2026})", export.native_kw));
            result.nodes.push(node);
        }
        Some(result)
    }

    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        if !JS_FAMILY.contains(&reference.language) {
            return None;
        }
        let raw = reference.reference_name.as_str();
        let (receiver, name) = match raw.rsplit_once('.') {
            Some((receiver, name)) => (Some(receiver), name),
            None => (None, raw),
        };
        let maps = context
            .route_tables()
            .get_or_build(NAME, || RnMaps::build(context));
        let entries = maps
            .by_js_name
            .get(name)
            .filter(|entries| !entries.is_empty())?;
        if let Some(receiver) = receiver {
            let module = native_modules_receiver()
                .captures(receiver)
                .and_then(|found| found.get(1))
                .map(|module| module.as_str())
                .or_else(|| maps.aliases.get(receiver));
            if let Some(module) = module {
                let exact = pick(entries.iter().filter(|entry| entry.module == module))?;
                return Some(bridge_ref(reference, exact, module, 0.95));
            }
        }
        if upstream_drops_candidates(reference) {
            return None;
        }
        let target = pick(entries.iter())?;
        Some(bridge_ref(reference, target, &target.module, 0.6))
    }
}

/// A bridge resolution with its `{bridge, module}` metadata.
fn bridge_ref(reference: &RefView, entry: &RnEntry, module: &str, confidence: f64) -> ResolvedRef {
    let mut edge = EdgeShape::default();
    edge.metadata
        .insert("bridge".to_string(), serde_json::json!("react-native"));
    edge.metadata
        .insert("module".to_string(), serde_json::json!(module));
    ResolvedRef::new(
        reference.clone(),
        entry.node.id.clone(),
        confidence,
        ResolvedBy::Framework,
    )
    .with_edge(edge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::route_href::test_ctx::RouteCtx;

    fn method(name: &str, language: Language, file: &str, line: i64) -> Node {
        Node {
            id: format!("{}:{file}:{name}:{line}", language.as_str()),
            kind: NodeKind::Method,
            name: name.to_string(),
            qualified_name: format!("{file}::{name}"),
            file_path: file.to_string(),
            language,
            start_line: line,
            end_line: line + 5,
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

    fn swift_method(name: &str, owner: &str, file: &str, line: i64) -> Node {
        let mut node = method(name, Language::Swift, file, line);
        node.qualified_name = format!("{owner}::{name}");
        node
    }

    fn reference(name: &str, language: Language, file: &str) -> RefView {
        RefView {
            row_id: None,
            from_node_id: format!("caller:{file}"),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::Calls,
            line: 1,
            column: 0,
            file_path: file.to_string(),
            language,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    fn ctx(nodes: Vec<Node>, files: &[(&str, &str)]) -> RouteCtx {
        let mut ctx = RouteCtx::default();
        for (path, content) in files {
            ctx = ctx.file(path, content);
        }
        let mut by_file: HashMap<String, Vec<Node>> = HashMap::new();
        for node in nodes {
            by_file
                .entry(node.file_path.clone())
                .or_default()
                .push(node);
        }
        let mut paths: Vec<String> = by_file.keys().cloned().collect();
        paths.sort();
        for path in paths {
            ctx = ctx.routes(&path, by_file.remove(&path).unwrap());
        }
        ctx
    }

    fn resolve(ctx: &RouteCtx, name: &str, language: Language) -> Option<ResolvedRef> {
        ReactNativeBridgeResolver.resolve(&reference(name, language, "src/a.tsx"), ctx)
    }

    fn target(ctx: &RouteCtx, name: &str) -> Option<(String, f64)> {
        resolve(ctx, name, Language::Tsx).map(|result| (result.target_node_id, result.confidence))
    }

    fn module_of(result: &ResolvedRef) -> serde_json::Value {
        result.edge.as_ref().expect("metadata").metadata["module"].clone()
    }

    const PACKAGE: (&str, &str) = (
        "package.json",
        "{\"dependencies\":{\"react-native\":\"^0.73\"}}",
    );

    const SHIM: &str = "\n#import <React/RCTBridgeModule.h>\n#import <React/RCTViewManager.h>\n\n@interface RCT_EXTERN_MODULE(CaptureView, RCTViewManager)\n\nRCT_EXTERN_METHOD(syncSettings:(NSDictionary *)settings)\nRCT_EXTERN_METHOD(finalizeCaptureSession)\nRCT_EXTERN_REMAP_METHOD(pause, pauseInferenceNow)\n\n@end\n";

    /// Upstream `react-native-bridge.test.ts` 399-409.
    #[test]
    fn the_shim_names_its_module_class_keywords_and_remap() {
        let parsed: Vec<[String; 4]> = parse_objc_rn_externs(SHIM)
            .into_iter()
            .map(|entry| {
                [
                    entry.module_name,
                    entry.class_name,
                    entry.js_name,
                    entry.native_kw,
                ]
            })
            .collect();
        let expected: Vec<[String; 4]> = [
            ["CaptureView", "CaptureView", "syncSettings", "syncSettings"],
            [
                "CaptureView",
                "CaptureView",
                "finalizeCaptureSession",
                "finalizeCaptureSession",
            ],
            ["CaptureView", "CaptureView", "pause", "pauseInferenceNow"],
        ]
        .iter()
        .map(|row| row.map(str::to_string))
        .collect();
        assert_eq!(parsed, expected);
        assert_eq!(
            parse_objc_rn_externs(
                "@interface RCT_EXTERN_REMAP_MODULE(Camera, CameraModule, NSObject)\nRCT_EXTERN_METHOD(snap)"
            ),
            [ObjcExtern {
                module_name: "Camera".to_string(),
                class_name: "CameraModule".to_string(),
                js_name: "snap".to_string(),
                native_kw: "snap".to_string(),
                line: 2,
            }]
        );
        assert!(parse_objc_rn_externs("RCT_EXTERN_METHOD(snap)").is_empty());
    }

    /// Upstream `react-native-bridge.test.ts` 411-432: direct bindings
    /// first, then the destructured ones; a name bound to two modules is
    /// dropped.
    #[test]
    fn aliases_bind_direct_then_destructured_and_drop_a_name_two_modules_share() {
        let mut aliases = NativeModuleAliases::default();
        collect_native_module_aliases(
            "const captureView = NativeModules.CaptureView\nexport const { CaptureEvents, Geo: geolocation } = NativeModules\nlet typed: Spec = NativeModules.Typed\n",
            &mut aliases,
        );
        let entries: Vec<(&str, &str)> = aliases
            .entries()
            .iter()
            .map(|(alias, module)| (alias.as_str(), module.as_str()))
            .collect();
        assert_eq!(
            entries,
            [
                ("captureView", "CaptureView"),
                ("typed", "Typed"),
                ("CaptureEvents", "CaptureEvents"),
                ("geolocation", "Geo"),
            ]
        );
        collect_native_module_aliases("const captureView = NativeModules.Other", &mut aliases);
        assert_eq!(aliases.get("captureView"), None);
        assert!(aliases.is_ambiguous("captureView"));
        // The same module again changes nothing, and a dropped name stays
        // dropped.
        collect_native_module_aliases("const typed = NativeModules.Typed", &mut aliases);
        assert_eq!(aliases.get("typed"), Some("Typed"));
        collect_native_module_aliases(
            "const captureView = NativeModules.CaptureView",
            &mut aliases,
        );
        assert_eq!(aliases.get("captureView"), None);
    }

    /// The module, keywords and lines of an Objective-C module's exports;
    /// a commented-out macro counts, as upstream reads the raw text.
    #[test]
    fn exports_take_the_module_and_the_native_keyword() {
        let source = "@implementation RCTGeolocation\nRCT_EXPORT_MODULE()\nRCT_EXPORT_METHOD(getCurrentPosition:(id)cb) {}\n// RCT_EXPORT_METHOD(stale:(id)x)\nRCT_REMAP_METHOD(compute, doCompute:(id)x) {}\n@end";
        let exports = parse_objc_rn_exports(source, find_objc_class_name(source));
        let rows: Vec<(&str, &str, &str, i64)> = exports
            .iter()
            .map(|export| {
                (
                    export.module_name.as_str(),
                    export.js_name.as_str(),
                    export.native_kw.as_str(),
                    export.line,
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("Geolocation", "getCurrentPosition", "getCurrentPosition", 3),
                ("Geolocation", "stale", "stale", 4),
                ("Geolocation", "compute", "doCompute", 5),
            ]
        );
        let named = "@implementation BluetoothImpl\nRCT_EXPORT_MODULE(BluetoothManager)\nRCT_EXPORT_METHOD(startScan:(id)cb) {}";
        assert_eq!(
            parse_objc_rn_exports(named, find_objc_class_name(named))[0].module_name,
            "BluetoothManager"
        );
        // `RCT` alone stays the module's name; no class and no argument
        // name no module.
        assert_eq!(default_objc_module_name("RCT"), "RCT");
        assert_eq!(default_objc_module_name("Mod"), "Mod");
        assert!(parse_objc_rn_exports("RCT_EXPORT_METHOD(x:(id)a)", None).is_empty());
    }

    #[test]
    fn jvm_exports_take_get_names_literal_or_the_class() {
        let java = "class GeolocationModule extends ReactContextBaseJavaModule {\n  @Override public String getName() { return \"Geolocation\"; }\n  @ReactMethod public void getCurrentPosition(Callback cb) {}\n}";
        assert_eq!(
            parse_jvm_rn_exports(java),
            [("Geolocation".to_string(), "getCurrentPosition".to_string())]
        );
        let kotlin = "class BluetoothModule(ctx: ReactApplicationContext) : ReactContextBaseJavaModule(ctx) {\n  override fun getName(): String = \"BluetoothManager\"\n  @ReactMethod fun startScan(cb: Callback) {}\n}";
        assert_eq!(
            parse_jvm_rn_exports(kotlin),
            [("BluetoothManager".to_string(), "startScan".to_string())]
        );
        let unnamed = "class CameraModule extends ReactContextBaseJavaModule {\n  @ReactMethod\n  public void snap(Promise p) {}\n}";
        assert_eq!(
            parse_jvm_rn_exports(unnamed),
            [("Camera".to_string(), "snap".to_string())]
        );
        assert!(parse_jvm_rn_exports("class X { @ReactMethod void a() {} }").is_empty());
    }

    /// Upstream `react-native.ts` 265-289: `getName()`'s literal, else the
    /// first module class's name, else the first package class's, a
    /// trailing `Module` removed from either.
    #[test]
    fn a_jvm_module_falls_back_to_its_module_class_then_its_package_class() {
        let module = |source: &str| {
            parse_jvm_rn_exports(source)
                .into_iter()
                .map(|(module, _)| module)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            module(
                "public class FooModule extends ReactContextBaseJavaModule {\n  @ReactMethod public void go(Callback cb) {}\n}"
            ),
            ["Foo"]
        );
        assert_eq!(
            module(
                "class BarModule(ctx: ReactApplicationContext) : ReactContextBaseJavaModule(ctx) {\n  @ReactMethod fun go() {}\n}"
            ),
            ["Bar"]
        );
        assert_eq!(
            module(
                "public class ThingModule implements ReactPackage {\n  @ReactMethod public void go() {}\n}"
            ),
            ["Thing"]
        );
        assert_eq!(
            module(
                "public class ThingPackage implements ReactPackage {\n  @ReactMethod public void go() {}\n}"
            ),
            ["ThingPackage"]
        );
        // The module class comes before a package class written first, and
        // `getName()`'s literal before both.
        assert_eq!(
            module(
                "class APackage implements ReactPackage {}\nclass BModule extends ReactContextBaseJavaModule {\n  @ReactMethod public void go() {}\n}"
            ),
            ["B"]
        );
        assert_eq!(
            module(
                "class BModule extends ReactContextBaseJavaModule {\n  public String getName() { return \"Named\"; }\n  @ReactMethod public void go() {}\n}"
            ),
            ["Named"]
        );
        // Only a trailing `Module` goes, and a class named `Module` alone
        // names no module.
        assert_eq!(
            module(
                "class ModuleRegistryModule extends ReactContextBaseJavaModule {\n  @ReactMethod public void go() {}\n}"
            ),
            ["ModuleRegistry"]
        );
        assert!(module("class Module extends ReactContextBaseJavaModule {\n  @ReactMethod public void go() {}\n}").is_empty());
    }

    /// W7-21's "exactly one" counts target nodes: a legacy export and a
    /// TurboModule spec entry reaching the one Objective-C method are one
    /// candidate, and the by-name answer's module is the first entry's.
    #[test]
    fn entries_reaching_one_method_are_one_candidate() {
        let add = method("add:", Language::ObjC, "ios/Calc.m", 3);
        let calc = |spec_module: &str| {
            let spec = format!(
                "import {{ TurboModuleRegistry }} from 'react-native';\nexport interface Spec extends TurboModule {{\n  add(a: number, b: number): number;\n}}\nexport default TurboModuleRegistry.getEnforcing<Spec>('{spec_module}');"
            );
            ctx(
                vec![add.clone()],
                &[
                    PACKAGE,
                    (
                        "ios/Calc.m",
                        "@implementation CalcImpl\nRCT_EXPORT_MODULE(Calc)\nRCT_EXPORT_METHOD(add:(double)a b:(double)b) {}\n@end",
                    ),
                    ("src/NativeCalc.ts", &spec),
                ],
            )
        };
        let same = calc("Calc");
        assert_eq!(
            target(&same, "NativeModules.Calc.add"),
            Some((add.id.clone(), 0.95))
        );
        assert_eq!(target(&same, "Calc.add"), Some((add.id.clone(), 0.6)));
        // `ios/Calc.m` sorts before `src/NativeCalc.ts`: its entry is first.
        let renamed = calc("CalcSpec");
        let bare = resolve(&renamed, "Calc.add", Language::Tsx).expect("the one method");
        assert_eq!(
            (bare.target_node_id.as_str(), bare.confidence),
            (add.id.as_str(), 0.6)
        );
        assert_eq!(module_of(&bare), "Calc");
        assert_eq!(
            target(&renamed, "NativeModules.CalcSpec.add"),
            Some((add.id.clone(), 0.95))
        );
        // Two entries of one method in one file are one candidate too.
        let twice = ctx(
            vec![add.clone()],
            &[
                PACKAGE,
                (
                    "ios/Calc.m",
                    "@implementation CalcImpl\nRCT_EXPORT_MODULE(Calc)\nRCT_EXPORT_METHOD(add:(double)a b:(double)b) {}\nRCT_REMAP_METHOD(add, add:(double)a b:(double)b) {}\n@end",
                ),
            ],
        );
        assert_eq!(target(&twice, "add"), Some((add.id, 0.6)));
    }

    #[test]
    fn a_turbo_spec_lists_its_method_lines() {
        let source = "import { TurboModuleRegistry } from 'react-native';\nexport interface Spec extends TurboModule {\n  getTotalLength(tag: number): number;\n  readonly size: number;\n  isPointInFill(tag: number, options?: object): boolean;\n}\nexport default TurboModuleRegistry.getEnforcing<Spec>('RNSVGRenderableModule');";
        assert_eq!(
            parse_turbo_module_spec(source),
            Some(TurboSpec {
                module: "RNSVGRenderableModule".to_string(),
                methods: vec!["getTotalLength".to_string(), "isPointInFill".to_string()],
            })
        );
        assert_eq!(
            parse_turbo_module_spec("export interface Spec {\n  a(): void;\n}\n"),
            None
        );
        // A `\r` starts a line for JavaScript's multiline `^`.
        let cr = "export interface Spec {\n  a(): void; \r  b(): void;\n}\nTurboModuleRegistry.get<Spec>('M')";
        assert_eq!(
            parse_turbo_module_spec(cr).map(|spec| spec.methods),
            Some(vec!["a".to_string(), "b".to_string()])
        );
    }

    /// Upstream `react-native-bridge.test.ts` 72-102 and 434-436.
    #[test]
    fn detection_reads_the_package_the_macros_and_the_registry() {
        let resolver = ReactNativeBridgeResolver;
        assert!(resolver.detect(&ctx(Vec::new(), &[PACKAGE])));
        assert!(resolver.detect(&ctx(
            Vec::new(),
            &[(
                "NativeFoo.mm",
                "@implementation Foo\nRCT_EXPORT_MODULE()\n@end"
            )]
        )));
        assert!(resolver.detect(&ctx(
            Vec::new(),
            &[(
                "NativeFoo.ts",
                "import { TurboModuleRegistry } from 'react-native';\nexport default TurboModuleRegistry.getEnforcing<Spec>('Foo');"
            )]
        )));
        assert!(resolver.detect(&ctx(Vec::new(), &[("ios/CaptureView.m", SHIM)])));
        assert!(!resolver.detect(&ctx(vec![method("hi", Language::ObjC, "X.m", 1)], &[])));
        // `RCT_EXPORT_MODULE_NO_LOAD` is no `RCT_EXPORT_MODULE`.
        assert!(!resolver.detect(&ctx(
            Vec::new(),
            &[("A.m", "RCT_EXPORT_MODULE_NO_LOAD(A, B)")]
        )));
        // Only the first 200 paths are read.
        let mut files: Vec<(String, String)> = (0..200)
            .map(|at| (format!("a/{at:03}.m"), String::new()))
            .collect();
        files.push(("z.m".to_string(), "RCT_EXPORT_MODULE()".to_string()));
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str()))
            .collect();
        assert!(!resolver.detect(&ctx(Vec::new(), &borrowed)));
    }

    /// Upstream `react-native-bridge.test.ts` 104-221.
    #[test]
    fn a_js_call_reaches_the_objc_jvm_or_spec_method_by_its_js_name() {
        let geolocation = method(
            "getCurrentPosition:",
            Language::ObjC,
            "RCTGeolocation.m",
            10,
        );
        let scan = method("startScan:", Language::ObjC, "Bluetooth.m", 10);
        let compute = method("doInternalCompute:", Language::ObjC, "Computer.m", 10);
        let ctx = ctx(
            vec![geolocation.clone(), scan.clone(), compute.clone()],
            &[
                PACKAGE,
                (
                    "RCTGeolocation.m",
                    "@implementation RCTGeolocation\nRCT_EXPORT_MODULE()\nRCT_EXPORT_METHOD(getCurrentPosition:(RCTResponseSenderBlock)cb) {}\n@end",
                ),
                (
                    "Bluetooth.m",
                    "@implementation BluetoothImpl\nRCT_EXPORT_MODULE(BluetoothManager)\nRCT_EXPORT_METHOD(startScan:(RCTResponseSenderBlock)cb) {}\n@end",
                ),
                (
                    "Computer.m",
                    "@implementation Computer\nRCT_EXPORT_MODULE()\nRCT_REMAP_METHOD(compute, doInternalCompute:(NSDictionary *)opts) {}\n@end",
                ),
            ],
        );
        assert_eq!(
            target(&ctx, "getCurrentPosition"),
            Some((geolocation.id, 0.6))
        );
        assert_eq!(target(&ctx, "startScan"), Some((scan.id.clone(), 0.6)));
        assert_eq!(target(&ctx, "compute"), Some((compute.id, 0.6)));
        let result = resolve(
            &ctx,
            "NativeModules.BluetoothManager.startScan",
            Language::Tsx,
        )
        .expect("the module's method");
        assert_eq!(
            (result.target_node_id.as_str(), result.confidence),
            (scan.id.as_str(), 0.95)
        );
        assert_eq!(module_of(&result), "BluetoothManager");
        // A named module without the method is not guessed at.
        assert!(resolve(&ctx, "NativeModules.Missing.startScan", Language::Tsx).is_none());
        // A native caller is not the bridge's.
        assert!(resolve(&ctx, "startScan", Language::ObjC).is_none());
    }

    #[test]
    fn jvm_and_spec_exports_reach_their_methods() {
        let java = method(
            "getCurrentPosition",
            Language::Java,
            "GeolocationModule.java",
            3,
        );
        let kotlin = method("startScan", Language::Kotlin, "BluetoothModule.kt", 3);
        let objc = method(
            "getTotalLength:",
            Language::ObjC,
            "RNSVGRenderableManager.mm",
            10,
        );
        let ctx = ctx(
            vec![java.clone(), kotlin.clone(), objc.clone()],
            &[
                PACKAGE,
                (
                    "GeolocationModule.java",
                    "class GeolocationModule extends ReactContextBaseJavaModule {\n  @Override public String getName() { return \"Geolocation\"; }\n  @ReactMethod public void getCurrentPosition(Callback cb) {}\n}",
                ),
                (
                    "BluetoothModule.kt",
                    "class BluetoothModule(ctx: ReactApplicationContext) : ReactContextBaseJavaModule(ctx) {\n  override fun getName(): String = \"BluetoothManager\"\n  @ReactMethod fun startScan(cb: Callback) {}\n}",
                ),
                (
                    "NativeSvgRenderableModule.ts",
                    "import { TurboModuleRegistry } from 'react-native';\nexport interface Spec extends TurboModule {\n  getTotalLength(tag: number): number;\n  thingThatDoesntExist(): void;\n}\nexport default TurboModuleRegistry.getEnforcing<Spec>('RNSVGRenderableModule');",
                ),
            ],
        );
        assert_eq!(target(&ctx, "getCurrentPosition"), Some((java.id, 0.6)));
        assert_eq!(target(&ctx, "startScan"), Some((kotlin.id, 0.6)));
        assert_eq!(target(&ctx, "getTotalLength"), Some((objc.id, 0.6)));
        assert_eq!(target(&ctx, "thingThatDoesntExist"), None);
    }

    /// Upstream `react-native-bridge.test.ts` 266-297.
    #[test]
    fn emitter_builtins_are_never_bridged() {
        let ctx = ctx(
            vec![
                method("addListener:", Language::ObjC, "EventEmitter.m", 10),
                method("remove:", Language::ObjC, "EventEmitter.m", 11),
            ],
            &[
                PACKAGE,
                (
                    "EventEmitter.m",
                    "@implementation EventEmitter\nRCT_EXPORT_MODULE()\nRCT_EXPORT_METHOD(addListener:(NSString *)eventName) {}\nRCT_EXPORT_METHOD(remove:(double)id) {}\n@end",
                ),
            ],
        );
        assert_eq!(target(&ctx, "addListener"), None);
        assert_eq!(target(&ctx, "remove"), None);
    }

    fn capture_ctx() -> (RouteCtx, [Node; 3]) {
        let finalize = swift_method(
            "finalizeCaptureSession",
            "CaptureView",
            "ios/CaptureView+ReactBridge.swift",
            26,
        );
        let sync = swift_method("syncSettings", "CaptureView", "ios/CaptureView.swift", 40);
        let pause = swift_method(
            "pauseInferenceNow",
            "CaptureView",
            "ios/CaptureView.swift",
            60,
        );
        let decoy = swift_method(
            "syncSettings",
            "CaptureSettings",
            "ios/CaptureSettings.swift",
            12,
        );
        let ctx = ctx(
            vec![finalize.clone(), sync.clone(), pause.clone(), decoy],
            &[
                (
                    "package.json",
                    "{\"name\":\"app\",\"dependencies\":{\"react-native\":\"0.76\"}}",
                ),
                ("ios/CaptureView.m", SHIM),
                (
                    "src/components/capture/capture-view.tsx",
                    "import { NativeModules, NativeEventEmitter } from 'react-native'\nexport const { CaptureEvents } = NativeModules\nexport const captureView = NativeModules.CaptureView\n",
                ),
            ],
        );
        (ctx, [finalize, sync, pause])
    }

    /// Upstream `react-native-bridge.test.ts` 438-471.
    #[test]
    fn an_aliased_or_named_module_reaches_its_swift_class_at_095() {
        let (ctx, [finalize, sync, pause]) = capture_ctx();
        let aliased = resolve(&ctx, "captureView.finalizeCaptureSession", Language::Tsx)
            .expect("the alias's module");
        assert_eq!(aliased.target_node_id, finalize.id);
        assert_eq!(aliased.confidence, 0.95);
        let metadata = &aliased.edge.as_ref().expect("metadata").metadata;
        assert_eq!(metadata.len(), 2);
        assert_eq!(metadata["bridge"], "react-native");
        assert_eq!(metadata["module"], "CaptureView");
        assert_eq!(
            target(&ctx, "NativeModules.CaptureView.syncSettings"),
            Some((sync.id.clone(), 0.95))
        );
        assert_eq!(target(&ctx, "captureView.pause"), Some((pause.id, 0.95)));
        assert_eq!(target(&ctx, "syncSettings"), Some((sync.id.clone(), 0.6)));
        assert_eq!(target(&ctx, "captureView.nothingHere"), None);
        // An alias of another module that lacks the method is not guessed
        // at either (`6557bc83`'s wrong-module guard).
        assert_eq!(target(&ctx, "CaptureEvents.syncSettings"), None);
        assert_eq!(
            target(&ctx, "somethingElse.syncSettings"),
            Some((sync.id, 0.6))
        );
        assert!(resolve(&ctx, "captureView.finalizeCaptureSession", Language::Swift).is_none());
    }

    /// Upstream's two early returns drop a framework candidate for an
    /// untyped member chain and a call on a call's result: the `0.6` answer
    /// is not given for them, while a named module's `0.95` stands.
    #[test]
    fn the_by_name_answer_is_not_given_for_upstreams_chain_shapes() {
        let (ctx, [_, sync, _]) = capture_ctx();
        assert_eq!(target(&ctx, "a.b.syncSettings"), None);
        assert_eq!(target(&ctx, "make().syncSettings"), None);
        assert_eq!(
            target(&ctx, "this.a.syncSettings"),
            Some((sync.id.clone(), 0.6))
        );
        assert_eq!(target(&ctx, "a.syncSettings"), Some((sync.id.clone(), 0.6)));
        assert_eq!(
            target(&ctx, "NativeModules.CaptureView.syncSettings"),
            Some((sync.id, 0.95))
        );
        // `\w` reads ASCII.
        assert!(chain_shape().is_match("make().sync_1"));
        assert!(!chain_shape().is_match("make().syncé"));
        assert!(!chain_shape().is_match("a\n().b"));
        assert!(!chain_shape().is_match("().b"));
    }

    /// KEEP-RUST: the preferred platform's sole method, nothing when it has
    /// several, and the next platform never tried; an export's target
    /// outside its own file is the project's sole one.
    #[test]
    fn several_methods_of_the_preferred_platform_answer_nothing() {
        let first = method("ping:", Language::ObjC, "A.m", 1);
        let second = method("ping:", Language::ObjC, "B.m", 1);
        let jvm = method("ping", Language::Java, "PingModule.java", 3);
        let java = (
            "PingModule.java",
            "class PingModule extends ReactContextBaseJavaModule {\n  @ReactMethod public void ping(Callback cb) {}\n}",
        );
        let two_objc = ctx(
            vec![first.clone(), second.clone(), jvm.clone()],
            &[
                PACKAGE,
                (
                    "A.m",
                    "@implementation A\nRCT_EXPORT_MODULE()\nRCT_EXPORT_METHOD(ping:(id)x) {}\n@end",
                ),
                (
                    "B.m",
                    "@implementation B\nRCT_EXPORT_MODULE()\nRCT_EXPORT_METHOD(ping:(id)x) {}\n@end",
                ),
                java,
            ],
        );
        // Each export reaches its own file's method, so ObjC holds two.
        assert_eq!(target(&two_objc, "ping"), None);
        assert_eq!(
            target(&two_objc, "NativeModules.A.ping"),
            Some((first.id.clone(), 0.95))
        );
        assert_eq!(
            target(&two_objc, "NativeModules.Ping.ping"),
            Some((jvm.id.clone(), 0.95))
        );
        // An export whose file has no method of its keyword reaches the
        // project's sole one, and none of two.
        let shim = (
            "C.m",
            "@implementation C\nRCT_EXPORT_MODULE()\nRCT_EXPORT_METHOD(ping:(id)x) {}\n@end",
        );
        let sole_elsewhere = ctx(vec![first.clone(), jvm.clone()], &[PACKAGE, shim, java]);
        assert_eq!(
            target(&sole_elsewhere, "NativeModules.C.ping"),
            Some((first.id.clone(), 0.95))
        );
        let two_elsewhere = ctx(vec![first, second, jvm], &[PACKAGE, shim, java]);
        assert_eq!(target(&two_elsewhere, "NativeModules.C.ping"), None);
    }

    #[test]
    fn rn_export_nodes_are_one_per_js_name_of_an_export_module_file() {
        let source = "@implementation CameraModule\n\nRCT_EXPORT_MODULE();\n\nRCT_EXPORT_METHOD(takePicture:(RCTPromiseResolveBlock)resolve rejecter:(RCTPromiseRejectBlock)reject)\n{\n}\nRCT_EXPORT_METHOD(takePicture:(id)again)\nRCT_REMAP_METHOD(snap, doSnap:(id)x)\n@end\n";
        let result = ReactNativeBridgeResolver
            .extract(
                "rn/ios/CameraModule.m",
                source,
                &FrameworkExtractionContext::without_config("/p"),
            )
            .expect("extraction");
        let rows: Vec<(&str, &str, &str, i64, i64, i64, i64)> = result
            .nodes
            .iter()
            .map(|node| {
                (
                    node.id.as_str(),
                    node.name.as_str(),
                    node.qualified_name.as_str(),
                    node.start_line,
                    node.end_line,
                    node.start_column,
                    node.end_column,
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                (
                    "rn-export:rn/ios/CameraModule.m:CameraModule.takePicture",
                    "takePicture",
                    "rn/ios/CameraModule.m::CameraModule.takePicture",
                    5,
                    5,
                    0,
                    0
                ),
                (
                    "rn-export:rn/ios/CameraModule.m:CameraModule.snap",
                    "snap",
                    "rn/ios/CameraModule.m::CameraModule.snap",
                    9,
                    9,
                    0,
                    0
                ),
            ]
        );
        let node = &result.nodes[1];
        assert_eq!(node.kind, NodeKind::Method);
        assert_eq!(node.language, Language::ObjC);
        assert!(node.is_exported);
        assert_eq!(
            node.docstring.as_deref(),
            Some("RCT_EXPORT_METHOD doSnap (module CameraModule)")
        );
        assert_eq!(
            node.signature.as_deref(),
            Some("RCT_EXPORT_METHOD(doSnap:\u{2026})")
        );
        // Without `RCT_EXPORT_MODULE`, or outside `.m`/`.mm`, nothing.
        let context = FrameworkExtractionContext::without_config("/p");
        for (path, text) in [
            ("A.m", "@implementation A\nRCT_EXPORT_METHOD(a:(id)x)\n@end"),
            ("A.h", source),
        ] {
            assert!(
                ReactNativeBridgeResolver
                    .extract(path, text, &context)
                    .expect("extraction")
                    .nodes
                    .is_empty(),
                "{path}"
            );
        }
        assert_eq!(
            ReactNativeBridgeResolver.languages(),
            Some(&[Language::ObjC][..])
        );
        assert_eq!(
            ReactNativeBridgeResolver.resolve_languages(),
            Some(&JS_FAMILY[..])
        );
    }

    #[test]
    fn a_files_facts_hold_what_its_extension_reads_and_round_trip() {
        let objc = BridgeFacts::of_text("ios/CaptureView.m", SHIM);
        assert_eq!(objc.externs.len(), 3);
        assert!(objc.exports.is_empty());
        assert!(objc.aliases.is_empty());
        let js = BridgeFacts::of_text(
            "src/a.js",
            "const a = NativeModules.A\nconst M = requireNativeModule('M')\n",
        );
        assert_eq!(js.aliases, [("a".to_string(), "A".to_string())]);
        assert!(!js.expo.is_empty());
        // A `.vue` file's aliases are not read, its bindings are.
        let vue = BridgeFacts::of_text(
            "src/a.vue",
            "const a = NativeModules.A\nconst M = requireNativeModule('M')\n",
        );
        assert!(vue.aliases.is_empty());
        assert!(!vue.expo.is_empty());
        assert!(BridgeFacts::of_text("a.ts", "export const x = 1").is_empty());
        let encoded = objc.encode();
        assert_eq!(BridgeFacts::decode(&encoded), Some(objc));
        assert_eq!(BridgeFacts::default().encode(), "{}");
    }

    /// `var` binds an alias as `const` and `let` do; a module is a name
    /// with a capital, as upstream's `NativeModules\.([A-Z]…)`.
    #[test]
    fn an_alias_takes_any_keyword_and_a_capitalised_module() {
        let bindings = native_module_bindings(
            "var v = NativeModules.V\nconst lower = NativeModules.lower\nlet ok = NativeModules.Ok\n",
        );
        assert_eq!(
            bindings,
            [
                ("v".to_string(), "V".to_string()),
                ("ok".to_string(), "Ok".to_string()),
            ]
        );
    }

    /// The method after `@ReactMethod`: `fun`, `void`, or `public` and a
    /// return type; `getName()`'s literal is the module as written.
    #[test]
    fn a_react_method_is_read_after_fun_void_or_a_public_type() {
        let java = "class PingModule extends ReactContextBaseJavaModule {\n  @Override public String getName() { return \"PingModule\"; }\n  @ReactMethod void plain(Callback cb) {}\n  @ReactMethod public String typed(Promise p) { return \"\"; }\n  @ReactMethod public List<String> generic(Promise p) { return null; }\n  @ReactMethod\n  fun kotlinStyle() {}\n}";
        let module = "PingModule".to_string();
        assert_eq!(
            parse_jvm_rn_exports(java),
            ["plain", "typed", "generic", "kotlinStyle"]
                .map(|name| (module.clone(), name.to_string()))
        );
    }

    /// The spec is the interface named `Spec`, not the first one exported;
    /// a `.tsx` file's facts hold it, and `get` registers as
    /// `getEnforcing` does, for detection too.
    #[test]
    fn the_spec_interface_is_named_spec_in_a_ts_or_tsx_file() {
        let source = "import { TurboModuleRegistry } from 'react-native';\nexport interface Props {\n  x(): void;\n}\nexport interface Spec extends TurboModule {\n  y(): void;\n}\nexport default TurboModuleRegistry.get<Spec>('M');";
        let spec = Some(TurboSpec {
            module: "M".to_string(),
            methods: vec!["y".to_string()],
        });
        assert_eq!(parse_turbo_module_spec(source), spec);
        assert_eq!(
            BridgeFacts::of_text("src/NativeM.tsx", source).turbo_spec,
            spec
        );
        assert_eq!(
            BridgeFacts::of_text("src/NativeM.ts", source).turbo_spec,
            spec
        );
        assert_eq!(
            BridgeFacts::of_text("src/NativeM.js", source).turbo_spec,
            None
        );
        let resolver = ReactNativeBridgeResolver;
        assert!(resolver.detect(&ctx(Vec::new(), &[("src/NativeM.tsx", source)])));
        // An extern remap module is a shim too; a package that only
        // mentions React Native names no dependency.
        assert!(resolver.detect(&ctx(
            Vec::new(),
            &[(
                "ios/Cam.m",
                "@interface RCT_EXTERN_REMAP_MODULE(Cam, CamModule, NSObject)\n@end"
            )]
        )));
        assert!(!resolver.detect(&ctx(
            Vec::new(),
            &[(
                "package.json",
                "{\"name\":\"react-native-thing\",\"description\":\"for react-native apps\"}"
            )]
        )));
    }

    /// Upstream `RN_EMITTER_BUILTINS`, written out: an export, an
    /// `RCT_EXTERN` entry, a JVM `@ReactMethod` and a TurboModule spec
    /// method of any of them bridge nothing, a name outside them does.
    #[test]
    fn every_emitter_builtin_is_skipped_at_every_kind_of_entry() {
        let builtins = [
            "addListener",
            "removeListeners",
            "remove",
            "invalidate",
            "startObserving",
            "stopObserving",
        ];
        assert_eq!(builtins, RN_EMITTER_BUILTINS);
        for name in builtins.into_iter().chain(["go"]) {
            let expected = |node: &Node| (name == "go").then(|| (node.id.clone(), 0.95));
            let objc = method(&format!("{name}:"), Language::ObjC, "E.m", 3);
            let export_text = format!(
                "@implementation E\nRCT_EXPORT_MODULE()\nRCT_EXPORT_METHOD({name}:(id)x) {{}}\n@end"
            );
            let export = ctx(vec![objc.clone()], &[PACKAGE, ("E.m", &export_text)]);
            assert_eq!(
                target(&export, &format!("NativeModules.E.{name}")),
                expected(&objc),
                "export {name}"
            );
            let swift = swift_method(name, "S", "S.swift", 3);
            let shim_text = format!(
                "@interface RCT_EXTERN_MODULE(S, NSObject)\nRCT_EXTERN_METHOD({name})\n@end"
            );
            let shim = ctx(vec![swift.clone()], &[PACKAGE, ("S.m", &shim_text)]);
            assert_eq!(
                target(&shim, &format!("NativeModules.S.{name}")),
                expected(&swift),
                "extern {name}"
            );
            let java = method(name, Language::Java, "JModule.java", 3);
            let jvm_text = format!(
                "class JModule extends ReactContextBaseJavaModule {{\n  @ReactMethod public void {name}() {{}}\n}}"
            );
            let jvm = ctx(vec![java.clone()], &[PACKAGE, ("JModule.java", &jvm_text)]);
            assert_eq!(
                target(&jvm, &format!("NativeModules.J.{name}")),
                expected(&java),
                "jvm {name}"
            );
            let implementation = method(&format!("{name}:"), Language::ObjC, "T.m", 3);
            let spec_text = format!(
                "export interface Spec extends TurboModule {{\n  {name}(): void;\n}}\nexport default TurboModuleRegistry.get<Spec>('T');"
            );
            let spec = ctx(
                vec![implementation.clone()],
                &[PACKAGE, ("NativeT.ts", &spec_text)],
            );
            assert_eq!(
                target(&spec, &format!("NativeModules.T.{name}")),
                expected(&implementation),
                "spec {name}"
            );
        }
    }

    /// KEEP-RUST at each entry: a JVM export reaches its own file's method,
    /// else the project's sole one and none of two; an `RCT_EXTERN` entry
    /// its class's sole method, a class whose name only holds the shim's
    /// another class; a spec method a Kotlin method too.
    #[test]
    fn jvm_extern_and_spec_entries_take_their_sole_method() {
        let module = (
            "PModule.java",
            "class PModule extends ReactContextBaseJavaModule {\n  @ReactMethod public void ping(Callback cb) {}\n}",
        );
        let own = method("ping", Language::Java, "PModule.java", 2);
        let elsewhere = method("ping", Language::Kotlin, "a/Ping.kt", 2);
        let other = method("ping", Language::Kotlin, "b/Ping.kt", 2);
        let with_own = ctx(vec![own.clone(), elsewhere.clone()], &[PACKAGE, module]);
        assert_eq!(
            target(&with_own, "NativeModules.P.ping"),
            Some((own.id.clone(), 0.95))
        );
        let sole = ctx(vec![elsewhere.clone()], &[PACKAGE, module]);
        assert_eq!(
            target(&sole, "NativeModules.P.ping"),
            Some((elsewhere.id.clone(), 0.95))
        );
        let two = ctx(vec![elsewhere, other], &[PACKAGE, module]);
        assert_eq!(target(&two, "NativeModules.P.ping"), None);

        let shim = (
            "ios/Cam.m",
            "@interface RCT_EXTERN_MODULE(Cam, NSObject)\nRCT_EXTERN_METHOD(snap)\n@end",
        );
        let snap = swift_method("snap", "Cam", "ios/Cam.swift", 3);
        let twin = swift_method("snap", "Cam", "ios/Cam+Ext.swift", 3);
        let longer = swift_method("snap", "CamViewModel", "ios/CamViewModel.swift", 3);
        let one = ctx(vec![snap.clone(), longer], &[PACKAGE, shim]);
        assert_eq!(
            target(&one, "NativeModules.Cam.snap"),
            Some((snap.id.clone(), 0.95))
        );
        let both = ctx(vec![snap, twin], &[PACKAGE, shim]);
        assert_eq!(target(&both, "NativeModules.Cam.snap"), None);

        let spec = (
            "src/NativeK.ts",
            "export interface Spec extends TurboModule {\n  measure(): number;\n}\nexport default TurboModuleRegistry.getEnforcing<Spec>('K');",
        );
        let kotlin = method("measure", Language::Kotlin, "android/KModule.kt", 4);
        let turbo = ctx(vec![kotlin.clone()], &[PACKAGE, spec]);
        assert_eq!(
            target(&turbo, "NativeModules.K.measure"),
            Some((kotlin.id, 0.95))
        );
    }

    /// The Objective-C entry comes first, then the Swift one, then any
    /// other.
    #[test]
    fn objective_c_comes_before_swift_before_the_jvm() {
        let objc = method("sync:", Language::ObjC, "ios/Sync.m", 3);
        let swift = swift_method("sync", "SyncShim", "ios/Sync.swift", 3);
        let java = method("sync", Language::Java, "SyncModule.java", 2);
        let files = [
            PACKAGE,
            (
                "ios/Sync.m",
                "@implementation Sync\nRCT_EXPORT_MODULE()\nRCT_EXPORT_METHOD(sync:(id)x) {}\n@end",
            ),
            (
                "ios/SyncShim.m",
                "@interface RCT_EXTERN_MODULE(SyncShim, NSObject)\nRCT_EXTERN_METHOD(sync)\n@end",
            ),
            (
                "SyncModule.java",
                "class SyncModule extends ReactContextBaseJavaModule {\n  @ReactMethod public void sync() {}\n}",
            ),
        ];
        let all = ctx(vec![objc.clone(), swift.clone(), java.clone()], &files);
        assert_eq!(target(&all, "sync"), Some((objc.id, 0.6)));
        let no_objc = ctx(vec![swift.clone(), java.clone()], &files);
        assert_eq!(target(&no_objc, "sync"), Some((swift.id, 0.6)));
        let jvm_only = ctx(vec![java.clone()], &files);
        assert_eq!(target(&jvm_only, "sync"), Some((java.id, 0.6)));
    }

    /// A receiver names its module only with a capital: a lowercase one is
    /// an untyped member chain, whose by-name answer upstream drops; every
    /// JavaScript-family language is read.
    #[test]
    fn a_lowercase_receiver_is_a_member_chain_and_every_js_language_is_read() {
        let ping = method("ping:", Language::ObjC, "ios/lower.m", 3);
        let ctx = ctx(
            vec![ping.clone()],
            &[
                PACKAGE,
                (
                    "ios/lower.m",
                    "@implementation lower\nRCT_EXPORT_MODULE(lower)\nRCT_EXPORT_METHOD(ping:(id)x) {}\n@end",
                ),
            ],
        );
        assert_eq!(target(&ctx, "NativeModules.lower.ping"), None);
        assert_eq!(target(&ctx, "lower.ping"), Some((ping.id.clone(), 0.6)));
        for language in [
            Language::JavaScript,
            Language::TypeScript,
            Language::Tsx,
            Language::Jsx,
        ] {
            assert_eq!(
                resolve(&ctx, "lower.ping", language).map(|result| result.target_node_id),
                Some(ping.id.clone()),
                "{language:?}"
            );
        }
        assert_eq!(
            ReactNativeBridgeResolver.resolve_languages(),
            Some(
                &[
                    Language::JavaScript,
                    Language::TypeScript,
                    Language::Tsx,
                    Language::Jsx,
                ][..]
            )
        );
        // The call-on-a-call shape gates calls only.
        let (capture, [_, sync, _]) = capture_ctx();
        let mut read = reference("make().syncSettings", Language::Tsx, "src/a.tsx");
        read.reference_kind = EdgeKind::References;
        assert_eq!(
            ReactNativeBridgeResolver
                .resolve(&read, &capture)
                .map(|result| (result.target_node_id, result.confidence)),
            Some((sync.id, 0.6))
        );
    }

    /// An Objective-C++ module's exports are a file's facts and its nodes,
    /// as an Objective-C one's.
    #[test]
    fn an_objective_cpp_module_exports_as_an_objective_c_one() {
        let source =
            "@implementation Plus\nRCT_EXPORT_MODULE()\nRCT_EXPORT_METHOD(add:(id)x) {}\n@end";
        let facts = BridgeFacts::of_text("ios/Plus.mm", source);
        assert_eq!(
            facts.exports,
            [BridgeEntry {
                module: "Plus".to_string(),
                class: String::new(),
                js_name: "add".to_string(),
                native_kw: "add".to_string(),
            }]
        );
        let nodes = ReactNativeBridgeResolver
            .extract(
                "ios/Plus.mm",
                source,
                &FrameworkExtractionContext::without_config("/p"),
            )
            .expect("extraction")
            .nodes;
        assert_eq!(
            nodes
                .iter()
                .map(|node| node.id.as_str())
                .collect::<Vec<_>>(),
            ["rn-export:ios/Plus.mm:Plus.add"]
        );
    }
}
