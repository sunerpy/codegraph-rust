//! Spring (upstream `resolution/frameworks/java.ts` at `e8ddc4e2`: #2222,
//! #1827, #1461/#1811, #1180, #722).
//!
//! A controller's `@GetMapping`-style and method-level `@RequestMapping`
//! annotations are route nodes, each with a reference to the method it
//! decorates, under the class's `@RequestMapping` prefix. A Spring config
//! file (`application.yml`, `bootstrap-dev.properties`, …) has a `constant`
//! per leaf key, holding the key and never the value (#722); an
//! `@Value("${key}")` or `@ConfigurationProperties("prefix")` is a binding
//! node with a reference the resolver links to that key through Spring's
//! relaxed binding ([`SpringTables`]). The resolver also guesses the
//! classes a Java or Kotlin name conventionally names (`…Service`,
//! `…Repository`, …) through the shared name heuristic
//! ([`super::name_heuristic`]).
//!
//! Offsets are byte offsets. Comments are blanked byte for byte
//! ([`crate::strip_comments::strip_java_comments_keeping_offsets`], Kotlin
//! too, as upstream), so an offset into the stripped text indexes the source
//! too; whatever upstream measures in UTF-16 code units (a column, a
//! window, a captured string) is measured on the source at the same offsets
//! ([`Text`]).

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use super::framework_node;
use super::js_scan::{JS_SPACE, js_trim};
use super::name_heuristic::{NamePick, PickOptions, pick_by_name_outcome};
use super::offset_text::OffsetText as Text;
use super::play::shared_directories;
use crate::framework::{FrameworkExtractionContext, FrameworkResolver};
use crate::types::{
    FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedBy, ResolvedRef,
};

/// The languages the resolver reads (upstream `languages`).
const LANGUAGES: [Language; 4] = [
    Language::Java,
    Language::Kotlin,
    Language::Yaml,
    Language::Properties,
];

/// The root build files detection reads by path, indexed or not: inputs of
/// the sync's `jvm` control kind (`codegraph_watch::control_inputs`), since
/// a `build.gradle` is never indexed and a configuration may keep the others
/// out of the index.
pub const ROOT_DETECTION_FILES: [&str; 3] = ["pom.xml", "build.gradle", "build.gradle.kts"];

/// What a root build file names when the project builds on Spring.
const BUILD_MARKERS: [&str; 2] = ["spring-boot", "springframework"];

/// The annotations a `.java` file shows Spring by.
const ANNOTATIONS: [&str; 4] = [
    "@SpringBootApplication",
    "@RestController",
    "@Service",
    "@Repository",
];

/// The `.java` files detection reads at once: a batch that holds a Spring
/// file ends the scan, as upstream's one-by-one read ends at that file.
const DETECTION_BATCH: usize = 256;

/// How far after a mapping the method it decorates is looked for, in UTF-16
/// code units (upstream's `slice(…, … + 600)`).
const WINDOW: usize = 600;

/// The folders a convention prefers, as path substrings (upstream's
/// `…_DIRS`).
const SERVICE_DIRS: [&str; 2] = ["/service/", "/services/"];
const REPO_DIRS: [&str; 2] = ["/repository/", "/repositories/"];
const CONTROLLER_DIRS: [&str; 2] = ["/controller/", "/controllers/"];
const ENTITY_DIRS: [&str; 5] = ["/entity/", "/entities/", "/model/", "/models/", "/domain/"];
const COMPONENT_DIRS: [&str; 3] = ["/component/", "/components/", "/config/"];

const CLASS_KINDS: [NodeKind; 1] = [NodeKind::Class];
const SERVICE_KINDS: [NodeKind; 2] = [NodeKind::Class, NodeKind::Interface];

/// The suffix an `@ConfigurationProperties` reference's name carries: it
/// binds a subtree, not one key.
const PREFIX_SENTINEL: &str = ":prefix";

/// The id prefixes of the `@Value` and `@ConfigurationProperties` binding
/// nodes (upstream's).
const VALUE_BINDING_ID: &str = "spring-value:";
const PROPERTIES_BINDING_ID: &str = "spring-cp:";

/// A pattern ported from upstream: `{S}` is JavaScript's `\s`, `{W}` the body
/// of its `\w` (ASCII), and its `\b` is written `(?-u:\b)`.
fn jvm_regex(pattern: &str) -> Regex {
    Regex::new(
        &pattern
            .replace("{S}", JS_SPACE)
            .replace("{W}", "A-Za-z0-9_"),
    )
    .expect("spring pattern")
}

macro_rules! patterns {
    ($($(#[$doc:meta])* $name:ident = $pattern:expr;)*) => {
        $(
            $(#[$doc])*
            fn $name() -> &'static Regex {
                static PATTERN: OnceLock<Regex> = OnceLock::new();
                PATTERN.get_or_init(|| jvm_regex($pattern))
            }
        )*
    };
}

patterns! {
    /// `static final String NAME = "value";`, a path a mapping may name.
    constant_declaration = r#"(?-u:\b)static{S}+final{S}+String{S}+([{W}]+){S}*={S}*"([^"]*)"{S}*;"#;
    /// The class's `@RequestMapping(…)`: one whose tail leads to `class`.
    class_mapping = r"@RequestMapping{S}*\(([^)]*)\){S}*(?:@[{W}.]+(?:\([^)]*\))?{S}*)*(?:public{S}+|final{S}+|abstract{S}+|open{S}+|data{S}+|sealed{S}+)*class(?-u:\b)";
    /// A verb's mapping, bare or with its arguments.
    verb_mapping = r"@(GetMapping|PostMapping|PutMapping|PatchMapping|DeleteMapping)(?-u:\b){S}*(\([^)]*\))?";
    /// `@RequestMapping`, bare or with its arguments.
    request_mapping = r"@RequestMapping(?-u:\b){S}*(\([^)]*\))?";
    /// What follows a class's `@RequestMapping`: annotations, modifiers and
    /// `class`.
    class_after = r"^{S}*(?:@[{W}.]+(?:\([^)]*\))?{S}*)*(?:public{S}+|final{S}+|abstract{S}+|open{S}+|data{S}+|sealed{S}+)*class(?-u:\b)";
    /// The method a mapping decorates: Kotlin's `fun name(`, or Java's
    /// return type before the name.
    handler = r"(?-u:\b)fun{S}+([{W}]+){S}*\(|(?-u:\b)(?:public|private|protected){S}+[^;{=]*?{S}+([{W}]+){S}*\(";
    /// A `@RequestMapping`'s `method = RequestMethod.X`.
    request_method = r"method{S}*={S}*[{\[]?{S}*(?:RequestMethod\.)?([{W}]+)";
    /// One argument of a mapping, perhaps named: an array, a string, or
    /// anything up to the next comma.
    mapping_argument = r#"(?:^|,){S}*(?:([{W}]+){S}*={S}*)?(\{(?:[^"'}]|"[^"]*"|'[^']*')*\}|\[(?:[^"'\]]|"[^"]*"|'[^']*')*\]|"[^"]*"|'[^']*'|[^,]+)"#;
    /// One path of an argument: a string, or a constant's name. Upstream's
    /// lookahead `(?=,|$)` is checked after the match ([`mapping_values`]).
    mapping_value = r#"(?:^|,){S}*(?:"([^"]*)"|'([^']*)'|([{W}.]+)){S}*"#;
    /// `@Value("${key}")` or `@Value("${key:default}")`.
    value_binding = r#"@Value{S}*\({S}*["']\$\{([^}:]+)(?::[^}]*)?\}["']{S}*\)"#;
    /// `@ConfigurationProperties("prefix")` or `(prefix = "prefix")`.
    properties_binding = r#"@ConfigurationProperties{S}*\({S}*(?:prefix{S}*={S}*)?["']([^"']+)["']"#;
    /// A Spring config file's name (upstream's `/i`, which folds only ASCII
    /// letters).
    config_file = r"(?i-u)^(application|bootstrap)(-[{W}.-]+)?\.(yml|yaml|properties)$";
    /// A config file without a profile.
    base_config_file = r"(?i-u)^(application|bootstrap)\.(yml|yaml|properties)$";
}

pub struct SpringResolver;

impl FrameworkResolver for SpringResolver {
    fn name(&self) -> &str {
        "spring"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&LANGUAGES)
    }

    /// A root `pom.xml`, `build.gradle` or `build.gradle.kts` naming Spring,
    /// or a `.java` file with one of Spring's annotations.
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        if ROOT_DETECTION_FILES.iter().any(|file| {
            context
                .read_file_shared(file)
                .is_some_and(|content| BUILD_MARKERS.iter().any(|marker| content.contains(marker)))
        }) {
            return true;
        }
        let java: Vec<String> = context
            .get_all_files_shared()
            .iter()
            .filter(|file| file.ends_with(".java"))
            .cloned()
            .collect();
        java.chunks(DETECTION_BATCH)
            .any(|batch| !context.files_containing(batch, &ANNOTATIONS).is_empty())
    }

    /// A config key a binding names (the `:prefix` subtree, or a dotted
    /// key), then the conventions of a Java or Kotlin name.
    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let name = reference.reference_name.as_str();
        if let Some(prefix) = name.strip_suffix(PREFIX_SENTINEL) {
            let tables = context.spring_tables();
            let canonical = canonical_config_key(prefix);
            let best = tables.with_prefix(&canonical, context).min_by(|a, b| {
                a.units
                    .cmp(&b.units)
                    .then_with(|| nearer_first(&a.node, &b.node, &reference.file_path))
                    .then_with(|| node_order(&a.node, &b.node))
            })?;
            return Some(framework_ref(reference, best.node.id.clone(), 0.85));
        }
        // Only a binding's reference is a key: the numerous method calls
        // (`list.add`) are `calls` (#1180).
        if reference.reference_kind == EdgeKind::References
            && is_jvm_source(reference.language)
            && name.contains('.')
            && !name.contains("::")
        {
            let tables = context.spring_tables();
            match tables.exact(&canonical_config_key(name), context) {
                [] => {}
                [only] => return Some(framework_ref(reference, only.node.id.clone(), 0.9)),
                several => {
                    // The base file's key over a profile's, then the shortest
                    // file name; upstream then takes its store's first, Rust
                    // the nearest the binding, then the first by file, line,
                    // column and id (KNOWN_DIFFS).
                    let best = several
                        .iter()
                        .min_by(|a, b| {
                            profile_score(&a.node)
                                .cmp(&profile_score(&b.node))
                                .then_with(|| nearer_first(&a.node, &b.node, &reference.file_path))
                                .then_with(|| node_order(&a.node, &b.node))
                        })
                        .expect("several keys");
                    return Some(framework_ref(reference, best.node.id.clone(), 0.75));
                }
            }
        }
        // The conventions guess only a Java or Kotlin name, and never an
        // inherited type, which imports and name matching resolve (#1827).
        if !is_jvm_source(reference.language)
            || matches!(
                reference.reference_kind,
                EdgeKind::Extends | EdgeKind::Implements
            )
        {
            return None;
        }
        convention(reference, context)
    }

    /// An `@ConfigurationProperties` reference, which names no node, past
    /// the name pre-filter.
    fn claims_reference(&self, name: &str) -> bool {
        name.ends_with(PREFIX_SENTINEL)
    }

    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        if is_spring_config_file(file_path) {
            return Some(extract_config(file_path, content));
        }
        if !file_path.ends_with(".java") && !file_path.ends_with(".kt") {
            return Some(FrameworkResolverExtractionResult::default());
        }
        Some(extract_source(file_path, content))
    }
}

/// Whether `language` is one a Spring binding or convention is written in.
fn is_jvm_source(language: Language) -> bool {
    matches!(language, Language::Java | Language::Kotlin)
}

fn framework_ref(reference: &RefView, target: String, confidence: f64) -> ResolvedRef {
    ResolvedRef::new(reference.clone(), target, confidence, ResolvedBy::Framework)
}

/// The conventions in upstream's order: the first with a candidate decides.
/// A pick that declines (a tier of several, a pick in another code family)
/// ends them too, so the next rule never guesses again with other kinds and
/// folders (KNOWN_DIFFS); upstream tries the next only when there is no
/// candidate at all, and takes the first otherwise.
fn convention(reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
    let name = reference.reference_name.as_str();
    let rules: [(bool, &[NodeKind], &[&str], f64); 5] = [
        (
            name.ends_with("Service"),
            &SERVICE_KINDS,
            &SERVICE_DIRS,
            0.85,
        ),
        (
            name.ends_with("Repository"),
            &SERVICE_KINDS,
            &REPO_DIRS,
            0.85,
        ),
        (
            name.ends_with("Controller"),
            &CLASS_KINDS,
            &CONTROLLER_DIRS,
            0.85,
        ),
        (is_entity_name(name), &CLASS_KINDS, &ENTITY_DIRS, 0.7),
        (
            name.ends_with("Component") || name.ends_with("Config"),
            &CLASS_KINDS,
            &COMPONENT_DIRS,
            0.8,
        ),
    ];
    for (applies, kinds, dirs, confidence) in rules {
        if !applies {
            continue;
        }
        match pick_by_name_outcome(
            reference,
            kinds,
            &|file| dirs.iter().any(|dir| file.contains(dir)),
            context,
            &PickOptions::default(),
        ) {
            NamePick::Picked(target) => return Some(framework_ref(reference, target, confidence)),
            NamePick::Declined => return None,
            NamePick::NoCandidate => {}
        }
    }
    None
}

/// `^[A-Z][a-zA-Z]+$`.
fn is_entity_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() >= 2
        && bytes[0].is_ascii_uppercase()
        && bytes[1..].iter().all(u8::is_ascii_alphabetic)
}

/// Spring's relaxed binding (`cache-list`, `cacheList`, `cache_list`,
/// `CACHE_LIST`): lower case without dashes and underscores (upstream
/// `canonicalConfigKey`; JavaScript's `toLowerCase` and Rust's
/// `to_lowercase` are both the locale-free full Unicode mapping).
fn canonical_config_key(key: &str) -> String {
    key.to_lowercase().replace(['-', '_'], "")
}

/// Upstream's `(isBase ? 0 : 1) * 1000 + base.length`, the file name's
/// length in UTF-16 code units.
fn profile_score(node: &Node) -> usize {
    let base = file_name(&node.file_path);
    let rank = if base_config_file().is_match(base) {
        0
    } else {
        1000
    };
    rank + base.encode_utf16().count()
}

/// Of two keys alike in rank, the one whose file shares more directories
/// with the binding's first: a multi-module project's modules each hold an
/// `application.yml`, and a module reads its own (KNOWN_DIFFS; upstream
/// takes its store's first).
fn nearer_first(a: &Node, b: &Node, binding_file: &str) -> Ordering {
    shared_directories(&b.file_path, binding_file)
        .cmp(&shared_directories(&a.file_path, binding_file))
}

/// The file, line, column and id order Rust breaks a tie by.
fn node_order(a: &Node, b: &Node) -> Ordering {
    a.file_path
        .cmp(&b.file_path)
        .then(a.start_line.cmp(&b.start_line))
        .then(a.start_column.cmp(&b.start_column))
        .then_with(|| a.id.cmp(&b.id))
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// A config key of the project: a YAML or `.properties` `constant` and its
/// canonical key.
struct ConfigKey {
    canonical: String,
    /// `canonical`'s length in UTF-16 code units (upstream `.length`).
    units: usize,
    node: Arc<Node>,
}

/// The project's Spring config keys, sorted by canonical key, read once per
/// resolution pass: every binding's reference reads them all (upstream
/// scans `getNodesByKind('constant')` per reference).
#[derive(Default)]
pub struct SpringTables {
    keys: OnceLock<Vec<ConfigKey>>,
}

impl SpringTables {
    pub fn new() -> Self {
        Self::default()
    }

    fn keys(&self, context: &dyn ResolutionContext) -> &[ConfigKey] {
        self.keys.get_or_init(|| {
            let mut keys: Vec<ConfigKey> = context
                .get_nodes_by_kind_shared(NodeKind::Constant)
                .into_iter()
                .filter(|node| matches!(node.language, Language::Yaml | Language::Properties))
                .map(|node| {
                    let canonical = canonical_config_key(&node.qualified_name);
                    ConfigKey {
                        units: canonical.encode_utf16().count(),
                        canonical,
                        node,
                    }
                })
                .collect();
            keys.sort_by(|a, b| {
                a.canonical
                    .cmp(&b.canonical)
                    .then_with(|| node_order(&a.node, &b.node))
            });
            keys
        })
    }

    /// The keys whose canonical form is `canonical`.
    fn exact(&self, canonical: &str, context: &dyn ResolutionContext) -> &[ConfigKey] {
        let keys = self.keys(context);
        let start = keys.partition_point(|key| key.canonical.as_str() < canonical);
        let len = keys[start..].partition_point(|key| key.canonical == canonical);
        &keys[start..start + len]
    }

    /// The keys whose canonical form starts with `prefix`, which sort
    /// together.
    fn with_prefix<'t>(
        &'t self,
        prefix: &'t str,
        context: &dyn ResolutionContext,
    ) -> impl Iterator<Item = &'t ConfigKey> {
        let keys = self.keys(context);
        let start = keys.partition_point(|key| key.canonical.as_str() < prefix);
        keys[start..]
            .iter()
            .take_while(move |key| key.canonical.starts_with(prefix))
    }
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/// Whether the file is a Spring config file by its name, the profile
/// variants included (upstream `isSpringConfigFile`).
pub fn is_spring_config_file(file_path: &str) -> bool {
    config_file().is_match(file_name(file_path))
}

/// Whether `reference` is a binding node's, whose target is a config key
/// only the Spring resolver links. The resolver therefore takes no other
/// strategy's answer for it (KNOWN_DIFFS): upstream lets import and name
/// matching outrank Spring's 0.75 profile pick, and its exact match takes
/// the binding node itself, which is named as its key.
pub(crate) fn is_binding_reference(reference: &RefView) -> bool {
    reference.from_node_id.starts_with(VALUE_BINDING_ID)
        || reference.from_node_id.starts_with(PROPERTIES_BINDING_ID)
}

/// Whether `text` ends with `suffix`, ASCII letters in any case.
fn ends_with_ignoring_ascii_case(text: &str, suffix: &str) -> bool {
    text.len() >= suffix.len()
        && text.as_bytes()[text.len() - suffix.len()..].eq_ignore_ascii_case(suffix.as_bytes())
}

/// `content.split(/\r?\n/)`: its lines without the `\n` and the `\r`
/// before it; a `\r` ending the last line stays.
fn lines_of(content: &str) -> impl Iterator<Item = &str> {
    let mut pieces = content.split('\n').peekable();
    std::iter::from_fn(move || {
        let piece = pieces.next()?;
        Some(if pieces.peek().is_some() {
            piece.strip_suffix('\r').unwrap_or(piece)
        } else {
            piece
        })
    })
}

/// One `constant` per leaf key of a Spring config file (upstream
/// `extractSpringConfig`): its id `spring-config:{file}:{line}:{key}`, its
/// name the key's last segment, its qualified name and signature the dotted
/// key, its end column the value's UTF-16 length. The value itself is never
/// stored: config files hold secrets (#722).
fn extract_config(file_path: &str, content: &str) -> FrameworkResolverExtractionResult {
    let language = if ends_with_ignoring_ascii_case(file_path, ".properties") {
        Language::Properties
    } else {
        Language::Yaml
    };
    let mut result = FrameworkResolverExtractionResult::default();
    let entries = if language == Language::Properties {
        properties_entries(content)
    } else {
        yaml_entries(content)
    };
    for (line, key, units) in entries {
        if key.is_empty() {
            continue;
        }
        let name = key.rsplit('.').next().unwrap_or(&key).to_string();
        let mut node = framework_node(
            format!("spring-config:{file_path}:{line}:{key}"),
            NodeKind::Constant,
            name,
            key.clone(),
            file_path.to_string(),
            line,
            line,
            0,
            units,
            language,
            false,
        );
        node.signature = Some(key);
        result.nodes.push(node);
    }
    result
}

/// A `.properties` file's keys: its line, the key, and the value's UTF-16
/// length. A line is a key and a value on either side of the first `=` or
/// `:` no `\` escapes; blank lines and `#` and `!` comments are none.
fn properties_entries(content: &str) -> Vec<(i64, String, i64)> {
    let mut entries = Vec::new();
    for (index, raw) in lines_of(content).enumerate() {
        let trimmed = js_trim(raw);
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
            continue;
        }
        let bytes = raw.as_bytes();
        let mut separator = None;
        let mut at = 0;
        while at < bytes.len() {
            match bytes[at] {
                b'=' | b':' => {
                    separator = Some(at);
                    break;
                }
                b'\\' if at + 1 < bytes.len() => at += 2,
                _ => at += 1,
            }
        }
        let Some(separator) = separator else {
            continue;
        };
        let value = js_trim(&raw[separator + 1..]);
        entries.push((
            index as i64 + 1,
            js_trim(&raw[..separator]).to_string(),
            value.encode_utf16().count() as i64,
        ));
    }
    entries
}

/// A YAML file's leaf keys, dotted through their parents by indentation:
/// its line, the key, and the value's UTF-16 length. A key with nothing (or
/// a comment) after its colon is a parent; list items, `---` and comments
/// are none.
fn yaml_entries(content: &str) -> Vec<(i64, String, i64)> {
    let mut entries = Vec::new();
    let mut parents: Vec<(usize, String)> = Vec::new();
    for (index, raw) in lines_of(content).enumerate() {
        let trimmed = js_trim(raw);
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed == "---"
            || trimmed.starts_with("- ")
        {
            continue;
        }
        let indent = raw
            .bytes()
            .take_while(|&byte| byte == b' ' || byte == b'\t')
            .count();
        let Some(colon) = unquoted_colon(raw) else {
            continue;
        };
        let key = js_trim(&raw[indent..colon]);
        if key.is_empty() {
            continue;
        }
        let after = js_trim(&raw[colon + 1..]);
        while parents.last().is_some_and(|(depth, _)| *depth >= indent) {
            parents.pop();
        }
        let dotted = parents
            .iter()
            .map(|(_, parent)| parent.as_str())
            .chain([key])
            .collect::<Vec<_>>()
            .join(".");
        if after.is_empty() || after.starts_with('#') {
            parents.push((indent, key.to_string()));
        } else {
            entries.push((
                index as i64 + 1,
                dotted,
                strip_one_quote(after).encode_utf16().count() as i64,
            ));
        }
    }
    entries
}

/// The offset of the first `:` outside quotes; a quote after a `\` closes
/// none.
fn unquoted_colon(raw: &str) -> Option<usize> {
    let bytes = raw.as_bytes();
    let mut quote: Option<u8> = None;
    for (at, &byte) in bytes.iter().enumerate() {
        if let Some(open) = quote {
            if byte == open && (at == 0 || bytes[at - 1] != b'\\') {
                quote = None;
            }
            continue;
        }
        match byte {
            b'"' | b'\'' => quote = Some(byte),
            b':' => return Some(at),
            _ => {}
        }
    }
    None
}

/// `text.replace(/^["']|["']$/g, '')`: one quote off each end.
fn strip_one_quote(text: &str) -> &str {
    strip_one_of_each_end(text, &['"', '\''], &['"', '\''])
}

/// Upstream's `replace(/^[…]|[…]$/g, '')`: one of `open` off the start and
/// one of `close` off the end, the end's never the start's.
fn strip_one_of_each_end<'s>(text: &'s str, open: &[char], close: &[char]) -> &'s str {
    let start = usize::from(text.starts_with(open));
    let end = if text.len() > start && text.ends_with(close) {
        text.len() - 1
    } else {
        text.len()
    };
    &text[start..end]
}

/// A Java or Kotlin file's routes and bindings (upstream
/// `springResolver.extract` on a `.java` or `.kt` file).
fn extract_source(file_path: &str, content: &str) -> FrameworkResolverExtractionResult {
    let language = if file_path.ends_with(".kt") {
        Language::Kotlin
    } else {
        Language::Java
    };
    let text = Text::java(content);
    let mut constants: HashMap<String, String> = HashMap::new();
    for captures in constant_declaration().captures_iter(&text.safe) {
        let name = captures.get(1).expect("constant name");
        let value = captures.get(2).expect("constant value");
        constants.insert(
            text.upstream(name.start(), name.end()),
            text.upstream(value.start(), value.end()),
        );
    }
    let prefixes = class_mapping().captures(&text.safe).map_or_else(
        || vec![String::new()],
        |captures| {
            let args = captures.get(1).expect("class mapping arguments");
            parse_mapping_paths(&text.upstream(args.start(), args.end()), &constants)
        },
    );
    let source = Source {
        text: &text,
        file_path,
        language,
    };
    let mut result = FrameworkResolverExtractionResult::default();
    // A verb's mapping, always a method's, bare or with a path.
    for captures in verb_mapping().captures_iter(&text.safe) {
        let whole = captures.get(0).expect("whole match");
        let method = match &captures[1] {
            "GetMapping" => "GET",
            "PostMapping" => "POST",
            "PutMapping" => "PUT",
            "PatchMapping" => "PATCH",
            _ => "DELETE",
        };
        let args = source.arguments(captures.get(2));
        let paths = parse_mapping_paths(&args, &constants);
        let handler = handler_name(&source.window(whole.end()));
        source.routes(
            &whole,
            method,
            &prefixes,
            &paths,
            handler.as_deref(),
            &mut result,
        );
    }
    // A method's `@RequestMapping`, the older style; the class's is its
    // prefix, read above.
    for captures in request_mapping().captures_iter(&text.safe) {
        let whole = captures.get(0).expect("whole match");
        let after = source.window(whole.end());
        if class_after().is_match(&after) {
            continue;
        }
        let Some(handler) = handler_name(&after) else {
            continue;
        };
        let args = source.arguments(captures.get(1));
        let method = request_method()
            .captures(&args)
            .map_or_else(|| "ANY".to_string(), |verb| verb[1].to_ascii_uppercase());
        let paths = parse_mapping_paths(&args, &constants);
        source.routes(
            &whole,
            &method,
            &prefixes,
            &paths,
            Some(&handler),
            &mut result,
        );
    }
    source.bindings(&mut result);
    result
}

/// The file a Java or Kotlin extraction reads.
struct Source<'s> {
    text: &'s Text<'s>,
    file_path: &'s str,
    language: Language,
}

impl Source<'_> {
    /// A mapping's arguments without their parentheses, as upstream's
    /// stripped text holds them.
    fn arguments(&self, group: Option<regex::Match<'_>>) -> String {
        group.map_or_else(String::new, |group| {
            self.text.upstream(group.start() + 1, group.end() - 1)
        })
    }

    /// The `WINDOW` code units after `at`, where a mapping's method is
    /// looked for. A match ends at a character the stripper kept or after a
    /// whole run of blanks, so `at` starts a source character.
    fn window(&self, at: usize) -> String {
        if self.text.content.is_char_boundary(at) {
            self.text.units_after(at, WINDOW)
        } else {
            String::new()
        }
    }

    /// One route per class prefix and path, each referencing `handler`.
    fn routes(
        &self,
        whole: &regex::Match<'_>,
        method: &str,
        prefixes: &[String],
        paths: &[String],
        handler: Option<&str>,
        result: &mut FrameworkResolverExtractionResult,
    ) {
        let line = self.text.line_at(whole.start());
        let width = self.text.units(whole.start(), whole.end());
        for prefix in prefixes {
            for path in paths {
                let route_path = join_path(prefix, path);
                let node = framework_node(
                    format!("route:{}:{line}:{method}:{route_path}", self.file_path),
                    NodeKind::Route,
                    format!("{method} {route_path}"),
                    format!("{}::route:{route_path}", self.file_path),
                    self.file_path.to_string(),
                    line,
                    line,
                    0,
                    width,
                    self.language,
                    false,
                );
                if let Some(handler) = handler {
                    result
                        .references
                        .push(self.reference(&node.id, handler.to_string(), line));
                }
                result.nodes.push(node);
            }
        }
    }

    /// `@Value("${key}")` and `@ConfigurationProperties("prefix")`: a
    /// `constant` binding node each, with a reference to the key or (marked
    /// `:prefix`) the subtree (upstream `extractSpringValueBindings`).
    fn bindings(&self, result: &mut FrameworkResolverExtractionResult) {
        let text = self.text;
        for captures in value_binding().captures_iter(&text.safe) {
            let whole = captures.get(0).expect("whole match");
            let found = captures.get(1).expect("key");
            let key = js_trim(&text.upstream(found.start(), found.end())).to_string();
            if key.is_empty() {
                continue;
            }
            let line = text.line_at(whole.start());
            self.binding(
                result,
                format!("{VALUE_BINDING_ID}{}:{line}:{key}", self.file_path),
                format!("{}::@Value:{key}", self.file_path),
                format!("@Value(\"{key}\")"),
                key.clone(),
                key,
                line,
                text.units(whole.start(), whole.end()),
            );
        }
        for captures in properties_binding().captures_iter(&text.safe) {
            let whole = captures.get(0).expect("whole match");
            let found = captures.get(1).expect("prefix");
            let prefix = js_trim(&text.upstream(found.start(), found.end())).to_string();
            if prefix.is_empty() {
                continue;
            }
            let line = text.line_at(whole.start());
            self.binding(
                result,
                format!("{PROPERTIES_BINDING_ID}{}:{line}:{prefix}", self.file_path),
                format!("{}::@ConfigurationProperties:{prefix}", self.file_path),
                format!("@ConfigurationProperties(\"{prefix}\")"),
                format!("{prefix}{PREFIX_SENTINEL}"),
                prefix,
                line,
                text.units(whole.start(), whole.end()),
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn binding(
        &self,
        result: &mut FrameworkResolverExtractionResult,
        id: String,
        qualified_name: String,
        signature: String,
        reference: String,
        name: String,
        line: i64,
        width: i64,
    ) {
        let mut node = framework_node(
            id,
            NodeKind::Constant,
            name,
            qualified_name,
            self.file_path.to_string(),
            line,
            line,
            0,
            width,
            self.language,
            false,
        );
        node.signature = Some(signature);
        result
            .references
            .push(self.reference(&node.id, reference, line));
        result.nodes.push(node);
    }

    /// A `references` reference from `from`, at its line, column 0.
    fn reference(&self, from: &str, name: String, line: i64) -> RefView {
        RefView {
            row_id: None,
            from_node_id: from.to_string(),
            reference_name: name,
            reference_kind: EdgeKind::References,
            line,
            column: 0,
            file_path: self.file_path.to_string(),
            language: self.language,
            is_function_ref: false,
            reference_subkind: None,
        }
    }
}

/// The name of the first method declared in `window`.
fn handler_name(window: &str) -> Option<String> {
    handler().captures(window).map(|captures| {
        captures
            .get(1)
            .or_else(|| captures.get(2))
            .expect("handler name")
            .as_str()
            .to_string()
    })
}

/// A mapping's paths (upstream `parseMappingPaths`): its `value`, `path`
/// and unnamed arguments' strings, and the same-file constants it names by
/// their last segment, in order, once each. An argument without a path, or
/// an empty array, is the class's path (`""`); a constant the file does not
/// declare is no path. With no such argument at all, `[""]`.
fn parse_mapping_paths(args: &str, constants: &HashMap<String, String>) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    let mut has_paths = false;
    for captures in mapping_argument().captures_iter(args) {
        if captures
            .get(1)
            .is_some_and(|name| name.as_str() != "value" && name.as_str() != "path")
        {
            continue;
        }
        has_paths = true;
        let scope = js_trim(strip_one_of_each_end(
            js_trim(&captures[2]),
            &['{', '['],
            &['}', ']'],
        ));
        if scope.is_empty() {
            paths.push(String::new());
        }
        paths.extend(mapping_values(scope, constants));
    }
    if !has_paths {
        return vec![String::new()];
    }
    let mut seen = HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
    paths
}

/// The paths one argument lists, as upstream's
/// `(?:^|,)\s*(?:"…"|'…'|[\w.]+)\s*(?=,|$)` finds them: Rust's `regex` has
/// no lookahead, so a match is kept only when a comma or the end follows,
/// and the search otherwise resumes one character past where the match
/// began, where JavaScript's resumes. A match at a start either satisfies
/// the lookahead with its greedy extent or none at that start does.
fn mapping_values(scope: &str, constants: &HashMap<String, String>) -> Vec<String> {
    let mut paths = Vec::new();
    let mut at = 0;
    while at <= scope.len() {
        let Some(captures) = mapping_value().captures_at(scope, at) else {
            break;
        };
        let whole = captures.get(0).expect("whole match");
        let rest = &scope[whole.end()..];
        if !(rest.is_empty() || rest.starts_with(',')) {
            at = whole.start()
                + scope[whole.start()..]
                    .chars()
                    .next()
                    .map_or(1, char::len_utf8);
            continue;
        }
        let path = captures
            .get(1)
            .or_else(|| captures.get(2))
            .map(|quoted| quoted.as_str().to_string())
            .or_else(|| {
                captures.get(3).and_then(|word| {
                    constants
                        .get(word.as_str().rsplit('.').next().unwrap_or_default())
                        .cloned()
                })
            });
        if let Some(path) = path {
            paths.push(path);
        }
        at = whole.end();
    }
    paths
}

/// A class prefix and a method's path as one `/path` (upstream `joinPath`).
fn join_path(prefix: &str, path: &str) -> String {
    let parts: Vec<&str> = [prefix, path]
        .into_iter()
        .map(|part| part.trim_matches('/'))
        .filter(|part| !part.is_empty())
        .collect();
    format!("/{}", parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::route_href::test_ctx::RouteCtx;

    fn context() -> FrameworkExtractionContext {
        FrameworkExtractionContext::without_config("/project")
    }

    fn spring(file: &str, src: &str) -> FrameworkResolverExtractionResult {
        SpringResolver.extract(file, src, &context()).unwrap()
    }

    fn names(result: &FrameworkResolverExtractionResult) -> Vec<String> {
        result.nodes.iter().map(|node| node.name.clone()).collect()
    }

    fn targets(result: &FrameworkResolverExtractionResult) -> Vec<String> {
        result
            .references
            .iter()
            .map(|reference| reference.reference_name.clone())
            .collect()
    }

    fn node(
        kind: NodeKind,
        name: &str,
        qualified: &str,
        file: &str,
        line: i64,
        language: Language,
    ) -> Node {
        let mut node = framework_node(
            format!("{}:{file}:{name}:{line}", kind.as_str()),
            kind,
            name.to_string(),
            qualified.to_string(),
            file.to_string(),
            line,
            line + 2,
            0,
            0,
            language,
            false,
        );
        node.updated_at = 0;
        node
    }

    fn key(file: &str, line: i64, dotted: &str) -> Node {
        let language = if file.ends_with(".properties") {
            Language::Properties
        } else {
            Language::Yaml
        };
        let mut node = node(
            NodeKind::Constant,
            dotted.rsplit('.').next().unwrap(),
            dotted,
            file,
            line,
            language,
        );
        node.id = format!("spring-config:{file}:{line}:{dotted}");
        node
    }

    fn reference(name: &str, kind: EdgeKind, file: &str, language: Language) -> RefView {
        RefView {
            row_id: None,
            from_node_id: format!("file:{file}"),
            reference_name: name.to_string(),
            reference_kind: kind,
            line: 7,
            column: 4,
            file_path: file.to_string(),
            language,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    fn resolve(ctx: &RouteCtx, reference: &RefView) -> Option<(String, f64)> {
        SpringResolver
            .resolve(reference, ctx)
            .map(|resolved| (resolved.target_node_id, resolved.confidence))
    }

    // --- Extraction (upstream frameworks.test.ts 1017-1245) ------------------

    #[test]
    fn a_get_mapping_under_the_class_prefix_references_its_method() {
        let src = "@RestController\n@RequestMapping(\"/api/users\")\npublic class UserController {\n    @GetMapping(\"/{id}\")\n    public User getUser(@PathVariable Long id) {\n        return null;\n    }\n}\n";
        let result = spring("src/UserController.java", src);
        assert_eq!(names(&result), ["GET /api/users/{id}"]);
        let route = &result.nodes[0];
        assert_eq!(
            route.id,
            "route:src/UserController.java:4:GET:/api/users/{id}"
        );
        assert_eq!(
            route.qualified_name,
            "src/UserController.java::route:/api/users/{id}"
        );
        assert_eq!(route.kind, NodeKind::Route);
        assert_eq!(route.language, Language::Java);
        assert_eq!(
            (route.start_line, route.start_column, route.end_column),
            (4, 0, 20)
        );
        assert_eq!(targets(&result), ["getUser"]);
        assert_eq!(result.references[0].reference_kind, EdgeKind::References);
        assert_eq!(result.references[0].from_node_id, route.id);
        assert_eq!(
            (result.references[0].line, result.references[0].column),
            (4, 0)
        );
    }

    #[test]
    fn several_paths_and_same_file_constant_prefixes() {
        // #1461/#1811: an array of paths, constants for paths and prefixes.
        let src = "public class A {\n  static final String BASE = \"/base\";\n  public static final String ITEMS = \"/items\";\n}\n@RequestMapping({BASE, \"/alt\"})\npublic class C {\n  @GetMapping(value = {ITEMS, \"/list\"}, produces = \"application/json\")\n  public List<Item> items() { return null; }\n  @PostMapping(path = Paths.ITEMS)\n  public void add() {}\n}\n";
        let result = spring("C.java", src);
        assert_eq!(
            names(&result),
            [
                "GET /base/items",
                "GET /base/list",
                "GET /alt/items",
                "GET /alt/list",
                "POST /base/items",
                "POST /alt/items",
            ]
        );
        assert_eq!(
            targets(&result),
            ["items", "items", "items", "items", "add", "add"]
        );
    }

    #[test]
    fn a_bare_mapping_inherits_the_prefix_and_the_class_mapping_is_no_route() {
        let src = "@RestController\n@RequestMapping(\"/orders\")\npublic class OrderController {\n  @PostMapping\n  public Order create(@RequestBody Order order) { return order; }\n  @DeleteMapping()\n  public void clear() {}\n}\n";
        let result = spring("OrderController.java", src);
        assert_eq!(names(&result), ["POST /orders", "DELETE /orders"]);
        assert_eq!(targets(&result), ["create", "clear"]);
    }

    #[test]
    fn a_path_it_cannot_resolve_is_omitted_and_an_empty_array_is_the_prefix() {
        let src = "@RequestMapping(\"/p\")\nclass C {\n  @GetMapping(Missing.PATH)\n  public void a() {}\n  @GetMapping(value = {})\n  public void b() {}\n  @GetMapping(produces = \"x\")\n  public void c() {}\n}\n";
        let result = spring("C.java", src);
        // `Missing.PATH` names no constant here: the mapping has a path
        // argument and no path, so it makes no route.
        assert_eq!(names(&result), ["GET /p", "GET /p"]);
        assert_eq!(targets(&result), ["b", "c"]);
    }

    #[test]
    fn a_mapping_in_a_javadoc_is_no_route_and_lines_count_the_comment() {
        let src = "/**\n * Example: @GetMapping(\"/doc\")\n */\n@RestController\npublic class C {\n  // @PostMapping(\"/no\")\n  @GetMapping(\"/yes\")\n  public String yes() { return \"\"; }\n}\n";
        let result = spring("C.java", src);
        assert_eq!(names(&result), ["GET /yes"]);
        assert_eq!(result.nodes[0].start_line, 7);
    }

    #[test]
    fn kotlin_functions_and_stacked_annotations() {
        let src = "@RestController\n@RequestMapping(\"/k\")\nclass KController {\n    @GetMapping(\"/a\")\n    @ResponseBody\n    @Transactional(readOnly = true)\n    fun listAll(): List<String> = emptyList()\n}\n";
        let result = spring("src/K.kt", src);
        assert_eq!(names(&result), ["GET /k/a"]);
        assert_eq!(targets(&result), ["listAll"]);
        assert_eq!(result.nodes[0].language, Language::Kotlin);
        assert_eq!(result.references[0].language, Language::Kotlin);
        // A Gradle script is no Kotlin source here.
        assert!(spring("build.gradle.kts", src).nodes.is_empty());
    }

    #[test]
    fn a_method_request_mapping_takes_its_verb_or_any() {
        let src = "@Controller\n@RequestMapping(\"/legacy\")\npublic class L {\n  @RequestMapping(value = \"/a\", method = RequestMethod.post)\n  public String a() { return \"\"; }\n  @RequestMapping(\"/b\")\n  public String b() { return \"\"; }\n  @RequestMapping(value = \"/c\", method = {RequestMethod.GET, RequestMethod.HEAD})\n  public String c() { return \"\"; }\n  @RequestMapping(\"/none\")\n  private int field;\n}\n";
        let result = spring("L.java", src);
        assert_eq!(
            names(&result),
            ["POST /legacy/a", "ANY /legacy/b", "GET /legacy/c"]
        );
        assert_eq!(targets(&result), ["a", "b", "c"]);
    }

    #[test]
    fn a_mapping_finds_no_method_beyond_the_window() {
        let filler = "x".repeat(WINDOW);
        let src = format!(
            "class C {{\n  @GetMapping(\"/far\")\n  /*{filler}*/ public void far() {{}}\n}}\n"
        );
        let result = spring("C.java", &src);
        assert_eq!(names(&result), ["GET /far"]);
        assert!(result.references.is_empty());
    }

    #[test]
    fn columns_and_the_window_count_utf16_units_of_the_source() {
        // A blanked comment of non-ASCII text counts its code units, not
        // its bytes: after 580 `é` the method's `(` is the window's last
        // unit (counted in bytes it would be far past it); one more and it
        // falls outside.
        let source = |count: usize| {
            let comment = "é".repeat(count);
            format!("class C {{\n  @GetMapping(\"/é\") /*{comment}*/ public void m() {{}}\n}}\n")
        };
        let result = spring("C.java", &source(580));
        // The match's own width, the space after it not included.
        assert_eq!(
            result.nodes[0].end_column,
            "@GetMapping(\"/é\")".encode_utf16().count() as i64
        );
        assert_eq!(targets(&result), ["m"]);
        let result = spring("C.java", &source(581));
        assert_eq!(names(&result), ["GET /é"]);
        assert!(result.references.is_empty());
        // An astral character is two units.
        let src = "class C {\n  @GetMapping(\"/😀\")\n  public void m() {}\n}\n";
        let result = spring("C.java", src);
        assert_eq!(result.nodes[0].end_column, 18);
        assert_eq!(names(&result), ["GET /😀"]);
    }

    #[test]
    fn value_and_configuration_properties_bindings() {
        let src = "class J {\n  @Value(\"${jwt.tokenHead}\")\n  private String head;\n  @Value(\"${ jwt.expiration : 3600 }\")\n  private long exp;\n  @Value(\"${}\")\n  private String none;\n}\n@ConfigurationProperties(prefix = \"app.cache\")\nclass CacheProps {}\n";
        let result = spring("src/J.java", src);
        assert_eq!(
            names(&result),
            ["jwt.tokenHead", "jwt.expiration", "app.cache"]
        );
        let head = &result.nodes[0];
        assert_eq!(head.id, "spring-value:src/J.java:2:jwt.tokenHead");
        assert_eq!(head.kind, NodeKind::Constant);
        assert_eq!(head.qualified_name, "src/J.java::@Value:jwt.tokenHead");
        assert_eq!(head.signature.as_deref(), Some("@Value(\"jwt.tokenHead\")"));
        assert_eq!(head.end_column, "@Value(\"${jwt.tokenHead}\")".len() as i64);
        let cache = &result.nodes[2];
        assert_eq!(cache.id, "spring-cp:src/J.java:9:app.cache");
        assert_eq!(
            cache.qualified_name,
            "src/J.java::@ConfigurationProperties:app.cache"
        );
        assert_eq!(
            cache.signature.as_deref(),
            Some("@ConfigurationProperties(\"app.cache\")")
        );
        assert_eq!(
            targets(&result),
            ["jwt.tokenHead", "jwt.expiration", "app.cache:prefix"]
        );
        assert!(
            result
                .references
                .iter()
                .all(|r| r.reference_kind == EdgeKind::References)
        );
        assert_eq!(result.references[2].from_node_id, cache.id);
    }

    #[test]
    fn parse_mapping_paths_keeps_arrays_and_quoted_commas_together() {
        let constants: HashMap<String, String> = [
            ("A".to_string(), "/a".to_string()),
            ("y".to_string(), "/y".to_string()),
        ]
        .into_iter()
        .collect();
        let paths = |args: &str| parse_mapping_paths(args, &constants);
        assert_eq!(paths(""), [""]);
        assert_eq!(paths("\"/x\""), ["/x"]);
        assert_eq!(paths("value = \"/x\""), ["/x"]);
        assert_eq!(paths("path = '/y'"), ["/y"]);
        assert_eq!(paths("produces = \"/z\""), [""]);
        assert_eq!(paths("{\"/a,b\", 'c'}"), ["/a,b", "c"]);
        assert_eq!(paths("{\"a\", Paths.A}"), ["a", "/a"]);
        assert_eq!(paths("value = {}"), [""]);
        assert_eq!(paths("[]"), [""]);
        assert_eq!(paths("{\"/d\", \"/d\"}"), ["/d"]);
        // `"a" x` is no path: the lookahead fails, and so does a search
        // resumed past the quote.
        assert_eq!(paths("{\"a\" x}"), Vec::<String>::new());
        assert_eq!(paths("{\"a\" x, \"b\"}"), ["b"]);
        // The search resumes one character past where the failed match
        // began, as JavaScript's does, so it finds `,y` inside the quotes
        // whose match failed: the constant `y`.
        assert_eq!(paths("{\"x,y,\"z}"), ["/y"]);
        assert_eq!(paths("Missing.B"), Vec::<String>::new());
    }

    #[test]
    fn join_path_normalizes_slashes() {
        assert_eq!(join_path("", ""), "/");
        assert_eq!(join_path("/a/", "/b/"), "/a/b");
        assert_eq!(join_path("a", ""), "/a");
        assert_eq!(join_path("//", "x"), "/x");
    }

    // --- Config files ---------------------------------------------------------

    #[test]
    fn spring_config_files_by_name() {
        for file in [
            "application.yml",
            "src/main/resources/application.yaml",
            "application-dev.properties",
            "bootstrap.yml",
            "conf/Application-PROD.1.YML",
        ] {
            assert!(is_spring_config_file(file), "{file}");
        }
        for file in [
            "app.yml",
            "application.json",
            "application-.yml",
            "my-application.yml",
            "applicationdev.yml",
            "application-dév.yml",
        ] {
            assert!(!is_spring_config_file(file), "{file}");
        }
    }

    #[test]
    fn yaml_leaf_keys_by_indentation() {
        let src = "server:\n  port: 8080\n  ssl: # a parent\n    enabled: true\n---\nspring:\n  datasource:\n    url: \"jdbc:mysql://h/db?a=b\"\n    'user:name': root\n  list:\n    - a: 1\n\tkey: 'x'\n# comment: 1\nplain: ''\n";
        let result = spring("src/main/resources/application.yml", src);
        let keys: Vec<(&str, &str, i64, i64)> = result
            .nodes
            .iter()
            .map(|n| {
                (
                    n.qualified_name.as_str(),
                    n.name.as_str(),
                    n.start_line,
                    n.end_column,
                )
            })
            .collect();
        assert_eq!(
            keys,
            [
                ("server.port", "port", 2, 4),
                ("server.ssl.enabled", "enabled", 4, 4),
                ("spring.datasource.url", "url", 8, 21),
                ("spring.datasource.'user:name'", "'user:name'", 9, 4),
                // A tab is one column of indentation, less than `list:`'s
                // two spaces: `key` is `list`'s sibling, not its child.
                ("spring.key", "key", 12, 1),
                ("plain", "plain", 14, 0),
            ]
        );
        let first = &result.nodes[0];
        assert_eq!(
            first.id,
            "spring-config:src/main/resources/application.yml:2:server.port"
        );
        assert_eq!(first.kind, NodeKind::Constant);
        assert_eq!(first.language, Language::Yaml);
        assert_eq!(first.signature.as_deref(), Some("server.port"));
        assert_eq!(first.start_column, 0);
        assert!(result.references.is_empty());
    }

    #[test]
    fn a_yaml_value_s_length_counts_utf16_code_units() {
        // `é` is one unit and two bytes, `😀` two units and four bytes; the
        // quotes around the value do not count.
        let result = spring("application.yml", "a:\n  b: 'é😀'\n  c: é\n");
        let ends: Vec<(&str, i64)> = result
            .nodes
            .iter()
            .map(|n| (n.qualified_name.as_str(), n.end_column))
            .collect();
        assert_eq!(ends, [("a.b", 3), ("a.c", 1)]);
    }

    #[test]
    fn config_values_are_never_stored() {
        let src = "db.password=s3cr3t-value\nspring:\n";
        let props = spring("application.properties", src);
        let yaml = spring("application.yml", "db:\n  password: s3cr3t-value\n");
        for node in props.nodes.iter().chain(&yaml.nodes) {
            let fields = format!(
                "{} {} {} {:?} {:?}",
                node.id, node.name, node.qualified_name, node.signature, node.docstring
            );
            assert!(!fields.contains("s3cr3t"), "{fields}");
        }
        assert_eq!(props.nodes[0].end_column, 12);
        assert_eq!(yaml.nodes[0].end_column, 12);
    }

    #[test]
    fn properties_keys_split_at_the_first_unescaped_separator() {
        let src = "# c\n! c\n\n a.b = 1 \nc.d:2\ne\\=f=3\ng\\:h:4\nlone\n=nokey\nurl=jdbc:x\r\nlast=\u{FEFF}é😀\u{FEFF}\r";
        let result = spring("config/application-dev.properties", src);
        let keys: Vec<(&str, i64, i64)> = result
            .nodes
            .iter()
            .map(|n| (n.qualified_name.as_str(), n.start_line, n.end_column))
            .collect();
        assert_eq!(
            keys,
            [
                ("a.b", 4, 1),
                ("c.d", 5, 1),
                ("e\\=f", 6, 1),
                ("g\\:h", 7, 1),
                ("url", 10, 6),
                ("last", 11, 3),
            ]
        );
        assert!(
            result
                .nodes
                .iter()
                .all(|n| n.language == Language::Properties)
        );
        // U+0085 is no space to JavaScript's trim.
        let result = spring("application.properties", "k=\u{85}v\u{85}\n");
        assert_eq!(result.nodes[0].end_column, 3);
    }

    #[test]
    fn crlf_lines_split_like_upstream() {
        let lines: Vec<&str> = lines_of("a\r\nb\rc\n\r\nd\r").collect();
        assert_eq!(lines, ["a", "b\rc", "", "d\r"]);
        assert_eq!(strip_one_quote("\""), "");
        assert_eq!(strip_one_quote("\"\""), "");
        assert_eq!(strip_one_quote("'a\""), "a");
        assert_eq!(strip_one_quote("a"), "a");
    }

    // --- Resolution -----------------------------------------------------------

    #[test]
    fn a_dotted_key_resolves_through_relaxed_binding() {
        let ctx = RouteCtx::default().routes(
            "src/main/resources/application.yml",
            vec![
                key("src/main/resources/application.yml", 3, "jwt.token-head"),
                key("src/main/resources/application.yml", 4, "jwt.expiration"),
            ],
        );
        let java = reference(
            "jwt.tokenHead",
            EdgeKind::References,
            "src/J.java",
            Language::Java,
        );
        assert_eq!(
            resolve(&ctx, &java),
            Some((
                "spring-config:src/main/resources/application.yml:3:jwt.token-head".to_string(),
                0.9
            ))
        );
        let kotlin = reference(
            "JWT_EXPIRATION",
            EdgeKind::References,
            "src/K.kt",
            Language::Kotlin,
        );
        assert_eq!(resolve(&ctx, &kotlin), None, "no dot, no key");
        // The gate: a `calls`, a Scala reference and a `::` name are no keys.
        for gated in [
            reference(
                "jwt.tokenHead",
                EdgeKind::Calls,
                "src/J.java",
                Language::Java,
            ),
            reference(
                "jwt.tokenHead",
                EdgeKind::References,
                "src/S.scala",
                Language::Scala,
            ),
            reference(
                "jwt::tokenHead",
                EdgeKind::References,
                "src/J.java",
                Language::Java,
            ),
        ] {
            assert_eq!(resolve(&ctx, &gated), None, "{gated:?}");
        }
    }

    #[test]
    fn several_profiles_prefer_the_base_file_then_the_shortest_name() {
        let ctx = RouteCtx::default()
            .routes(
                "cfg/application-prod.yml",
                vec![key("cfg/application-prod.yml", 1, "a.b")],
            )
            .routes(
                "cfg/application-dev.yml",
                vec![key("cfg/application-dev.yml", 1, "a.b")],
            )
            .routes(
                "cfg/application.yml",
                vec![key("cfg/application.yml", 9, "a.b")],
            );
        let java = reference("a.b", EdgeKind::References, "src/J.java", Language::Java);
        assert_eq!(
            resolve(&ctx, &java),
            Some(("spring-config:cfg/application.yml:9:a.b".to_string(), 0.75))
        );
        // Without the base file, the shorter profile name; ties by path.
        let ctx = RouteCtx::default()
            .routes(
                "z/application-dev.yml",
                vec![key("z/application-dev.yml", 1, "a.b")],
            )
            .routes(
                "a/application-dev.yml",
                vec![key("a/application-dev.yml", 5, "a.b")],
            )
            .routes(
                "a/application-local.yml",
                vec![key("a/application-local.yml", 1, "a.b")],
            );
        assert_eq!(
            resolve(&ctx, &java),
            Some((
                "spring-config:a/application-dev.yml:5:a.b".to_string(),
                0.75
            ))
        );
    }

    #[test]
    fn a_prefix_binds_the_shortest_key_under_it() {
        let ctx = RouteCtx::default().routes(
            "application.yml",
            vec![
                key("application.yml", 1, "app.cache.name.user-token"),
                key("application.yml", 2, "app.cache-list"),
                key("application.yml", 3, "app.cache.ttl"),
                key("application.yml", 4, "other.x"),
            ],
        );
        let cp = reference(
            "app.cache:prefix",
            EdgeKind::References,
            "src/P.java",
            Language::Java,
        );
        // Canonical forms keep their dots: `app.cache-list` is
        // `app.cachelist`, which starts with `app.cache` as upstream compares
        // strings, not segments. It ties `app.cache.ttl` at 13 units, and the
        // earlier line wins the tie.
        assert_eq!(
            resolve(&ctx, &cp),
            Some((
                "spring-config:application.yml:2:app.cache-list".to_string(),
                0.85
            ))
        );
        // Relaxed binding folds case; the deeper prefix leaves one key.
        let deep = reference(
            "App.Cache.Name:prefix",
            EdgeKind::References,
            "src/P.java",
            Language::Java,
        );
        assert_eq!(
            resolve(&ctx, &deep),
            Some((
                "spring-config:application.yml:1:app.cache.name.user-token".to_string(),
                0.85
            ))
        );
        let none = reference(
            "nothing:prefix",
            EdgeKind::References,
            "src/P.java",
            Language::Java,
        );
        assert_eq!(resolve(&ctx, &none), None);
        assert!(SpringResolver.claims_reference("app.cache:prefix"));
        assert!(!SpringResolver.claims_reference("app.cache"));
    }

    #[test]
    fn a_prefix_tie_on_length_takes_the_first_by_path() {
        let ctx = RouteCtx::default()
            .routes(
                "b/application.yml",
                vec![key("b/application.yml", 1, "p.ab")],
            )
            .routes(
                "a/application.yml",
                vec![key("a/application.yml", 2, "p.aa")],
            );
        let cp = reference("p:prefix", EdgeKind::References, "J.java", Language::Java);
        assert_eq!(
            resolve(&ctx, &cp),
            Some(("spring-config:a/application.yml:2:p.aa".to_string(), 0.85))
        );
    }

    #[test]
    fn keys_alike_in_rank_prefer_the_binding_s_own_module() {
        // Two modules each hold an `application.yml` defining the key: a
        // binding takes its own module's, one in neither the first by path.
        let ctx = RouteCtx::default()
            .routes(
                "admin/src/main/resources/application.yml",
                vec![
                    key("admin/src/main/resources/application.yml", 1, "jwt.secret"),
                    key(
                        "admin/src/main/resources/application.yml",
                        2,
                        "app.cache.ttl",
                    ),
                ],
            )
            .routes(
                "portal/src/main/resources/application.yml",
                vec![
                    key("portal/src/main/resources/application.yml", 1, "jwt.secret"),
                    key(
                        "portal/src/main/resources/application.yml",
                        2,
                        "app.cache.ttl",
                    ),
                ],
            );
        let bound = |file: &str, name: &str| {
            resolve(
                &ctx,
                &reference(name, EdgeKind::References, file, Language::Java),
            )
        };
        assert_eq!(
            bound("portal/src/main/java/P.java", "jwt.secret"),
            Some((
                "spring-config:portal/src/main/resources/application.yml:1:jwt.secret".to_string(),
                0.75
            ))
        );
        assert_eq!(
            bound("admin/src/main/java/A.java", "jwt.secret"),
            Some((
                "spring-config:admin/src/main/resources/application.yml:1:jwt.secret".to_string(),
                0.75
            ))
        );
        assert_eq!(
            bound("security/src/main/java/S.java", "jwt.secret"),
            Some((
                "spring-config:admin/src/main/resources/application.yml:1:jwt.secret".to_string(),
                0.75
            ))
        );
        // A prefix's shortest keys tie the same way.
        assert_eq!(
            bound("portal/src/main/java/P.java", "app.cache:prefix"),
            Some((
                "spring-config:portal/src/main/resources/application.yml:2:app.cache.ttl"
                    .to_string(),
                0.85
            ))
        );
        // Proximity only breaks a tie: another module's base file still
        // outranks the binding's own profile file.
        let ctx = RouteCtx::default()
            .routes(
                "admin/src/main/resources/application.yml",
                vec![key("admin/src/main/resources/application.yml", 3, "x.y")],
            )
            .routes(
                "portal/src/main/resources/application-dev.yml",
                vec![key(
                    "portal/src/main/resources/application-dev.yml",
                    3,
                    "x.y",
                )],
            );
        assert_eq!(
            resolve(
                &ctx,
                &reference(
                    "x.y",
                    EdgeKind::References,
                    "portal/src/main/java/P.java",
                    Language::Java
                )
            ),
            Some((
                "spring-config:admin/src/main/resources/application.yml:3:x.y".to_string(),
                0.75
            ))
        );
    }

    #[test]
    fn the_conventions_and_their_gates() {
        let service = node(
            NodeKind::Interface,
            "UserService",
            "UserService",
            "src/service/UserService.java",
            3,
            Language::Java,
        );
        let entity = node(
            NodeKind::Class,
            "Order",
            "Order",
            "src/model/Order.java",
            3,
            Language::Java,
        );
        let config = node(
            NodeKind::Class,
            "WebConfig",
            "WebConfig",
            "src/config/WebConfig.java",
            3,
            Language::Java,
        );
        let ctx = RouteCtx::default()
            .routes("src/service/UserService.java", vec![service.clone()])
            .routes("src/model/Order.java", vec![entity.clone()])
            .routes("src/config/WebConfig.java", vec![config.clone()]);
        let at = |name: &str, kind: EdgeKind, language: Language| {
            resolve(&ctx, &reference(name, kind, "src/web/C.java", language))
        };
        assert_eq!(
            at("UserService", EdgeKind::References, Language::Java),
            Some((service.id.clone(), 0.85))
        );
        assert_eq!(
            at("Order", EdgeKind::References, Language::Kotlin),
            Some((entity.id.clone(), 0.7))
        );
        // `WebConfig` is an entity-shaped name first: no class of it under
        // the entity folders, but one at all, so 0.7 (as upstream).
        assert_eq!(
            at("WebConfig", EdgeKind::Calls, Language::Java),
            Some((config.id.clone(), 0.7))
        );
        // #1827: never inheritance, never another language.
        assert_eq!(at("Order", EdgeKind::Extends, Language::Java), None);
        assert_eq!(
            at("UserService", EdgeKind::Implements, Language::Java),
            None
        );
        assert_eq!(at("Order", EdgeKind::References, Language::Scala), None);
    }

    #[test]
    fn a_convention_that_declines_ends_the_chain() {
        // Two `AccountService` classes in no preferred folder: the service
        // rule declines, and the entity rule does not guess again.
        let a = node(
            NodeKind::Class,
            "AccountService",
            "AccountService",
            "a/AccountService.java",
            1,
            Language::Java,
        );
        let b = node(
            NodeKind::Class,
            "AccountService",
            "AccountService",
            "b/AccountService.java",
            1,
            Language::Java,
        );
        let ctx = RouteCtx::default()
            .routes("a/AccountService.java", vec![a])
            .routes("b/AccountService.java", vec![b.clone()]);
        let java = reference(
            "AccountService",
            EdgeKind::References,
            "web/C.java",
            Language::Java,
        );
        assert_eq!(resolve(&ctx, &java), None);
        // An interface and a class of the name: the service rule, which
        // takes both kinds, declines, and the entity rule, which would find
        // the one class, does not run.
        let iface = node(
            NodeKind::Interface,
            "AccountService",
            "AccountService",
            "a/AccountService.java",
            1,
            Language::Java,
        );
        let ctx = RouteCtx::default()
            .routes("a/AccountService.java", vec![iface])
            .routes("b/AccountService.java", vec![b]);
        assert_eq!(resolve(&ctx, &java), None);
        // With no candidate a rule passes to the next: an `…Config` that is
        // an interface has no class candidate anywhere, so nothing.
        let iface = node(
            NodeKind::Interface,
            "MailConfig",
            "MailConfig",
            "x/MailConfig.java",
            1,
            Language::Java,
        );
        let ctx = RouteCtx::default().routes("x/MailConfig.java", vec![iface]);
        let java = reference(
            "MailConfig",
            EdgeKind::References,
            "web/C.java",
            Language::Java,
        );
        assert_eq!(resolve(&ctx, &java), None);
        // A repository found as a class under its folder.
        let repo = node(
            NodeKind::Class,
            "OrderRepository",
            "OrderRepository",
            "src/repository/OrderRepository.java",
            1,
            Language::Java,
        );
        let ctx =
            RouteCtx::default().routes("src/repository/OrderRepository.java", vec![repo.clone()]);
        let java = reference(
            "OrderRepository",
            EdgeKind::References,
            "web/C.java",
            Language::Java,
        );
        assert_eq!(resolve(&ctx, &java), Some((repo.id, 0.85)));
    }

    #[test]
    fn detection_by_build_file_or_annotation() {
        let ctx = RouteCtx::default()
            .unlisted("build.gradle", "plugins { id 'org.springframework.boot' }");
        assert!(SpringResolver.detect(&ctx));
        let ctx =
            RouteCtx::default().unlisted("pom.xml", "<artifactId>spring-boot-starter</artifactId>");
        assert!(SpringResolver.detect(&ctx));
        let ctx = RouteCtx::default().file(
            "build.gradle.kts",
            "implementation(\"org.springframework:spring-web\")",
        );
        assert!(SpringResolver.detect(&ctx));
        let ctx = RouteCtx::default()
            .unlisted("pom.xml", "<artifactId>guava</artifactId>")
            .file("src/A.java", "class A {}")
            .file("src/B.kt", "@Service class B")
            .file("src/C.java", "@Repository\ninterface C {}");
        assert!(SpringResolver.detect(&ctx));
        let ctx = RouteCtx::default()
            .file("src/A.java", "class A {}")
            .file("src/B.kt", "@Service class B");
        assert!(
            !SpringResolver.detect(&ctx),
            "only `.java` files are scanned"
        );
        // Each annotation alone shows Spring.
        for annotation in [
            "@SpringBootApplication",
            "@RestController",
            "@Service",
            "@Repository",
        ] {
            let ctx =
                RouteCtx::default().file("src/A.java", &format!("{annotation}\nclass A {{}}"));
            assert!(SpringResolver.detect(&ctx), "{annotation}");
        }
    }

    #[test]
    fn the_java_scan_reads_batches_until_one_holds_spring() {
        // A Spring file past the first batch is still found.
        let mut ctx = RouteCtx::default();
        for index in 0..(DETECTION_BATCH + 3) {
            ctx = ctx.file(&format!("src/F{index:04}.java"), "class F {}");
        }
        assert!(!SpringResolver.detect(&ctx));
        let ctx = ctx.file("src/Z.java", "@SpringBootApplication class Z {}");
        assert!(SpringResolver.detect(&ctx));
    }

    #[test]
    fn the_java_scan_stops_at_the_first_batch_holding_spring() {
        // A Spring file in the first batch ends the scan: no file of the
        // next batch is read.
        let mut ctx = RouteCtx::default().file("src/A.java", "@RestController class A {}");
        for index in 1..(DETECTION_BATCH + 3) {
            ctx = ctx.file(&format!("src/F{index:04}.java"), "class F {}");
        }
        assert!(SpringResolver.detect(&ctx));
        let reads = ctx.reads();
        let last_of_first = format!("src/F{:04}.java", DETECTION_BATCH - 1);
        let first_of_next = format!("src/F{DETECTION_BATCH:04}.java");
        assert!(reads.contains(&last_of_first), "{reads:?}");
        assert!(!reads.contains(&first_of_next), "{reads:?}");
    }

    #[test]
    fn canonical_keys_relax_case_dashes_and_underscores() {
        assert_eq!(
            canonical_config_key("App.Cache-List_Max"),
            "app.cachelistmax"
        );
        assert_eq!(canonical_config_key("ÄÖ-Ü"), "äöü");
    }
}
