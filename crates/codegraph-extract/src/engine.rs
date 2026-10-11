//! File and directory extraction pipeline.
//!
//! Source map: `upstream extraction/index.ts:90-101` maps to content
//! hashing and size skips; `:402-570` maps to directory scanning; and
//! `tree-sitter.ts:4350-4425` maps to source dispatch.

use anyhow::{Context, Result};
use codegraph_core::config::{Config, IndexingConfig};
use codegraph_core::types::{ExtractionResult, Language};
use rayon::prelude::*;
use regex::Regex;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tree_sitter::Parser;

use crate::ext_config::ExtensionOverrides;
use crate::lang::{cpp_code_mask, spec_for_language};
use crate::links::LinkWalk;
use crate::walker::TreeSitterWalker;
use codegraph_core::source_file::{SourceText, read_source_file};

/// Stable diagnostic fragment used when a supported grammar reports parse
/// errors and extraction collapses to only the synthetic file node.
pub const PARSE_COLLAPSE_WARNING: &str = "parse produced no symbols (tree has errors)";

/// Parse-collapse diagnostics are warning-only: the file stays indexed and the
/// index command succeeds, but callers must surface the message honestly.
pub fn is_extraction_warning(message: &str) -> bool {
    message.contains(PARSE_COLLAPSE_WARNING)
}

#[derive(Debug, Clone)]
pub struct ExtractOptions {
    pub max_file_size: u64,
    pub ignore_dirs: Vec<String>,
    pub ignore_paths: Vec<String>,
    pub exclude: Vec<String>,
    pub include: Vec<String>,
    pub parallel: bool,
    /// The addressed project's custom extension→language overrides, loaded from
    /// ITS current-root `codegraph.json` ([`ExtensionOverrides::load_for_paths`]).
    /// Empty by default, so a project that declares none behaves exactly as
    /// before. Passed explicitly instead of discovered, so two projects handled
    /// by one process can never see each other's overrides.
    pub extensions: Arc<ExtensionOverrides>,
}

/// Coarse, read-only extraction stages for diagnostics.
///
/// Observers are notified only when execution enters a stage. They cannot
/// cancel parsing or alter extraction output, so enabling diagnostics remains
/// behavior-neutral and deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractionStage {
    DetectLanguage,
    Prepare,
    Embedded,
    TreeSitterParse,
    Walk,
}

impl ExtractionStage {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DetectLanguage => "detect_language",
            Self::Prepare => "prepare",
            Self::Embedded => "embedded",
            Self::TreeSitterParse => "tree_sitter_parse",
            Self::Walk => "walk",
        }
    }
}

impl Default for ExtractOptions {
    fn default() -> Self {
        let indexing = IndexingConfig::default();
        Self {
            max_file_size: indexing.max_file_size,
            ignore_dirs: indexing.ignore_dirs,
            ignore_paths: indexing.ignore_paths,
            exclude: indexing.exclude,
            include: indexing.include,
            parallel: true,
            extensions: ExtensionOverrides::empty(),
        }
    }
}

impl ExtractOptions {
    /// Build the scan/extract options for ONE project from its own immutable
    /// [`Config`] and extension overrides. This is the only shape production
    /// callers use; nothing here consults a process-global value.
    #[must_use]
    pub fn for_project(config: &Config, extensions: Arc<ExtensionOverrides>) -> Self {
        Self {
            max_file_size: config.indexing.max_file_size,
            ignore_dirs: config.indexing.ignore_dirs.clone(),
            ignore_paths: config.indexing.ignore_paths.clone(),
            exclude: config.indexing.exclude.clone(),
            include: config.indexing.include.clone(),
            parallel: true,
            extensions,
        }
    }
}

/// `ext` is lowercased, no leading dot. `None` is the exact set of extensions a
/// `.codegraph/codegraph.json` override may claim (the golden-safety skip-list).
pub fn builtin_language_for_ext(ext: &str) -> Option<Language> {
    let language = match ext {
        "ts" | "mts" | "cts" => Language::TypeScript,
        "tsx" => Language::Tsx,
        "js" | "mjs" | "cjs" | "xsjs" | "xsjslib" => Language::JavaScript,
        "jsx" => Language::Jsx,
        // Only `.ets` maps to ArkTS; plain `.ts` stays TypeScript (matching upstream).
        "ets" => Language::ArkTs,
        "py" | "pyw" => Language::Python,
        "go" => Language::Go,
        "rs" => Language::Rust,
        "java" => Language::Java,
        "c" | "h" => Language::C,
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => Language::Cpp,
        // Metal Shading Language (≈ C++14) and CUDA (≈ C++ + dialect tokens) both
        // ride the C++ grammar with a dialect-specific pre-parse blank; no new
        // `Language` variant (upstream maps all three to `cpp`).
        "metal" | "cu" | "cuh" => Language::Cpp,
        "cs" => Language::CSharp,
        "php" | "module" | "install" | "theme" | "inc" => Language::Php,
        "rb" | "rake" => Language::Ruby,
        "swift" => Language::Swift,
        "kt" | "kts" => Language::Kotlin,
        "dart" => Language::Dart,
        "vue" => Language::Vue,
        "svelte" => Language::Svelte,
        "liquid" => Language::Liquid,
        "pas" | "dpr" | "dpk" | "lpr" | "dfm" | "fmx" => Language::Pascal,
        "scala" | "sc" => Language::Scala,
        "lua" => Language::Lua,
        "gd" => Language::Gdscript,
        "tscn" => Language::GodotScene,
        "tres" => Language::GodotResource,
        "luau" => Language::Luau,
        "m" | "mm" => Language::ObjC,
        "r" => Language::R,
        "sol" => Language::Solidity,
        "nix" => Language::Nix,
        "tf" | "tfvars" | "tofu" => Language::Terraform,
        "erl" | "hrl" => Language::Erlang,
        "cfc" | "cfm" | "cfs" => Language::Cfml,
        "yml" | "yaml" => Language::Yaml,
        "twig" => Language::Twig,
        "xml" => Language::Xml,
        "properties" => Language::Properties,
        _ => return None,
    };
    Some(language)
}

/// Built-in language detection with NO project extension overrides.
///
/// Use this only where no project is addressed (a bare path classification).
/// Every pipeline path that indexes or syncs a project calls
/// [`detect_language_with`] with that project's own overrides.
pub fn detect_language(file_path: impl AsRef<Path>) -> Language {
    detect_language_with(file_path, &ExtensionOverrides::default())
}

/// Detect `file_path`'s language, consulting `overrides` for extensions the
/// built-in table and the embedded pre-pass leave unclaimed.
///
/// Golden safety: the override is consulted ONLY for extensions unclaimed by
/// both the built-in match and the embedded pre-pass (both checked first), so
/// empty overrides are byte-identical to the pre-override behavior.
pub fn detect_language_with(
    file_path: impl AsRef<Path>,
    overrides: &ExtensionOverrides,
) -> Language {
    let path = file_path.as_ref();
    let normalized = normalize_path(path);
    if let Some(language) = crate::embedded::detect_embedded_language(&normalized) {
        return language;
    }
    // `project.godot` has no extension, so the extension map below cannot catch
    // it. Special-case the bare file name before the extension lookup.
    if path.file_name().and_then(|name| name.to_str()) == Some("project.godot") {
        return Language::GodotProject;
    }
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return Language::Unknown;
    };
    let ext = ext.to_ascii_lowercase();
    if let Some(language) = builtin_language_for_ext(&ext) {
        return language;
    }
    if let Some(language) = overrides.language_for(&ext) {
        return language;
    }
    Language::Unknown
}

/// The language `file_path` is indexed as, given its `source`: the language
/// of its extension, except that a `.h` header holding C++ or Objective-C is
/// that language. Extraction parses a file with it and every path that records
/// a file row stores it, so the row agrees with the file's nodes.
pub fn detect_language_for_source(
    file_path: &str,
    source: &str,
    overrides: &ExtensionOverrides,
) -> Language {
    sniff_header_language(
        file_path,
        detect_language_with(file_path, overrides),
        source,
    )
}

/// [`detect_language_for_source`] for a file as the bounded reader returned
/// it. A file over the size limit was never read, so only its extension
/// decides; it has no nodes to disagree with.
pub fn detect_language_of(
    file_path: &str,
    source: &SourceText,
    overrides: &ExtensionOverrides,
) -> Language {
    match source {
        SourceText::Text(text) => detect_language_for_source(file_path, text, overrides),
        SourceText::Oversize(_) | SourceText::MpegTransportStream => {
            detect_language_with(file_path, overrides)
        }
    }
}

/// `language` refined by `source` when `file_path` is a `.h` header of it:
/// C++ or Objective-C content makes the header that language.
fn sniff_header_language(file_path: &str, language: Language, source: &str) -> Language {
    let header = Path::new(file_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("h"));
    if language != Language::C || !header {
        language
    } else if looks_like_cpp(source) {
        Language::Cpp
    } else if looks_like_objc(source) {
        Language::ObjC
    } else {
        language
    }
}

/// A `.h` file maps to `Language::C` by extension, but may hold C++ or
/// Objective-C. The ordinary unique-C++ probes retain their bounded 8 KiB pass;
/// a second full-source pass recognizes a plain class/struct base clause, whose
/// shape is never valid C. Both passes run over a lexical code-only view so
/// comments, quoted strings, character literals, raw strings, and preprocessor
/// text cannot fabricate a C++ signal.
fn looks_like_cpp(source: &str) -> bool {
    static PREFIX_RE: OnceLock<Regex> = OnceLock::new();
    static BASE_CLAUSE_RE: OnceLock<Regex> = OnceLock::new();
    let prefix_re = PREFIX_RE.get_or_init(|| {
        Regex::new(
            r"\bnamespace\b|\bclass\s+\w+\s*[:{]|\b(?:class|struct)\s+[A-Z][A-Z0-9_]+\s+\w+\s*(?:final\s*)?[:{]|\btemplate\s*<|\b(?:public|private|protected)\s*:|\bvirtual\b|\busing\s+(?:namespace\b|\w+\s*=)",
        )
        .expect("looks-like-cpp regex")
    });
    let base_clause_re = BASE_CLAUSE_RE.get_or_init(|| {
        Regex::new(
            r"\b(?:class|struct)\s+[A-Za-z_]\w*\s*(?:final\s*)?:\s*(?:(?:public|protected|private|virtual)\s+)*(?:[A-Za-z_]\w*::)*[A-Za-z_]\w*(?:\s*<[^{};]*>)?\s*[{,]",
        )
        .expect("C++ base-clause regex")
    });

    let mask = cpp_code_mask(source);
    let mut code = source.as_bytes().to_vec();
    for (index, is_code) in mask.into_iter().enumerate() {
        if !is_code && code[index] != b'\n' && code[index] != b'\r' {
            code[index] = b' ';
        }
    }
    let code = String::from_utf8(code).expect("mask preserves UTF-8");
    prefix_re.is_match(prefix_8k(&code)) || base_clause_re.is_match(&code)
}

fn looks_like_objc(source: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"@(?:interface|implementation|protocol|synthesize)\b")
            .expect("looks-like-objc regex")
    });
    re.is_match(prefix_8k(source))
}

fn prefix_8k(source: &str) -> &str {
    match source.char_indices().nth(8192) {
        Some((idx, _)) => &source[..idx],
        None => source,
    }
}

/// Extract `source` with NO project extension overrides. Equivalent to
/// [`extract_source_with`] with empty overrides; used where `language` is already
/// known (embedded delegation) or no project is addressed.
pub fn extract_source(
    file_path: &str,
    source: &str,
    language: Option<Language>,
) -> ExtractionResult {
    extract_source_with(file_path, source, language, &ExtensionOverrides::default())
}

/// Extract `source`, resolving an absent `language` through the addressed
/// project's own extension `overrides`.
pub fn extract_source_with(
    file_path: &str,
    source: &str,
    language: Option<Language>,
    overrides: &ExtensionOverrides,
) -> ExtractionResult {
    extract_source_with_observer(file_path, source, language, overrides, |_| {})
}

/// Like [`extract_source_with`], with a read-only stage observer.
///
/// The observer is deliberately notification-only: there is no cancellation
/// return value and no wall-clock timeout. A successful index therefore still
/// parses every admitted file exactly as the ordinary path does.
pub fn extract_source_with_observer(
    file_path: &str,
    source: &str,
    language: Option<Language>,
    overrides: &ExtensionOverrides,
    mut observer: impl FnMut(ExtractionStage),
) -> ExtractionResult {
    let start = Instant::now();
    observer(ExtractionStage::DetectLanguage);
    let language = language.unwrap_or_else(|| detect_language_with(file_path, overrides));
    observer(ExtractionStage::Prepare);
    let language = sniff_header_language(file_path, language, source);
    observer(ExtractionStage::Embedded);
    if let Some(result) = crate::embedded::extract_embedded(file_path, source, language) {
        return result;
    }
    if is_file_level_only_language(language) {
        // The upstream returns an empty extractor result for yaml/twig/properties at
        // `upstream extraction/tree-sitter.ts:4382-4387`.
        return ExtractionResult {
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_references: Vec::new(),
            errors: Vec::new(),
            duration_ms: start.elapsed().as_millis() as i64,
        };
    }
    let Some(spec) = spec_for_language(language) else {
        return ExtractionResult {
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_references: Vec::new(),
            errors: if language == Language::Unknown {
                Vec::new()
            } else {
                vec![format!("Unsupported language: {language}")]
            },
            duration_ms: 0,
        };
    };

    let mut parser = Parser::new();
    // Run `pre_parse` first so the grammar selection can see the (possibly
    // rewritten) source. Only CFML overrides `tree_sitter_language_for_source`
    // to pick between its cfscript / cfml tag grammars per file dialect; every
    // other spec inherits the default (`tree_sitter_language`), so this is a
    // behavior-neutral reorder (their `pre_parse` is the identity default, and
    // the two that override it — C++/embedded — don't override the grammar
    // hook, so their parse path is byte-identical).
    observer(ExtractionStage::Prepare);
    let parsed_source = spec.pre_parse(source, file_path);
    let ts_language = spec.tree_sitter_language_for_source(&parsed_source);
    if let Err(error) = parser.set_language(&ts_language) {
        return ExtractionResult {
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_references: Vec::new(),
            errors: vec![format!("Failed to set parser language: {error}")],
            duration_ms: start.elapsed().as_millis() as i64,
        };
    }
    observer(ExtractionStage::TreeSitterParse);
    let Some(tree) = parser.parse(&parsed_source, None) else {
        return ExtractionResult {
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_references: Vec::new(),
            errors: vec!["Parser returned null tree".to_string()],
            duration_ms: start.elapsed().as_millis() as i64,
        };
    };
    observer(ExtractionStage::Walk);
    let parse_has_error = tree.root_node().has_error();
    let mut result = TreeSitterWalker::new(file_path, &parsed_source, spec, tree.root_node())
        .extract(start.elapsed().as_millis() as i64);
    if parse_has_error
        && result
            .nodes
            .iter()
            .all(|node| node.kind == codegraph_core::types::NodeKind::File)
    {
        result.errors.push(format!(
            "{file_path}: {PARSE_COLLAPSE_WARNING} - the file is indexed but contributes nothing to the graph"
        ));
    }
    result
}

/// Extract one file under the DEFAULT options (no project config). Kept for
/// callers that address no project; project pipelines use
/// [`extract_file_with_options`] so the addressed project's `max_file_size` and
/// extension overrides apply.
pub fn extract_file(
    root: impl AsRef<Path>,
    relative_path: impl AsRef<Path>,
) -> Result<ExtractionResult> {
    extract_file_with_options(root, relative_path, &ExtractOptions::default())
}

/// Extract one file under ONE project's own `options` (size limit + extension
/// overrides), so a per-project `max_file_size` and custom extension map apply
/// to the incremental path exactly as they do to a full scan.
pub fn extract_file_with_options(
    root: impl AsRef<Path>,
    relative_path: impl AsRef<Path>,
    options: &ExtractOptions,
) -> Result<ExtractionResult> {
    extract_file_with_options_observer(root, relative_path, options, |_| {})
}

/// Like [`extract_file_with_options`], with a read-only extraction-stage
/// observer. File metadata and source reads remain caller-visible errors.
pub fn extract_file_with_options_observer(
    root: impl AsRef<Path>,
    relative_path: impl AsRef<Path>,
    options: &ExtractOptions,
    observer: impl FnMut(ExtractionStage),
) -> Result<ExtractionResult> {
    let root = root.as_ref();
    let relative_path = normalize_path(relative_path.as_ref());
    let full_path = root.join(&relative_path);
    let (_, source) = read_source_file(&full_path, &relative_path, options.max_file_size)
        .with_context(|| format!("read source file {}", full_path.display()))?;
    Ok(extraction_of(&relative_path, &source, options, observer))
}

/// The extraction result for one file as [`read_source_file`] saw it: a parse
/// of its text, the size-skip result over the limit, and nothing at all for an
/// MPEG transport stream, which is not source.
pub fn extraction_of(
    relative_path: &str,
    source: &SourceText,
    options: &ExtractOptions,
    observer: impl FnMut(ExtractionStage),
) -> ExtractionResult {
    match source {
        SourceText::Text(text) => {
            extract_source_with_observer(relative_path, text, None, &options.extensions, observer)
        }
        SourceText::Oversize(size) => size_skip_result(relative_path, *size, options.max_file_size),
        SourceText::MpegTransportStream => ExtractionResult {
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_references: Vec::new(),
            errors: Vec::new(),
            duration_ms: 0,
        },
    }
}

pub fn extract_project(
    root: impl AsRef<Path>,
    options: &ExtractOptions,
) -> Result<ExtractionResult> {
    let root = root.as_ref();
    let files = scan_project(root, options)?;
    let parse = |relative: &String| -> Result<ExtractionResult> {
        let full = root.join(relative);
        let (_, source) = read_source_file(&full, relative, options.max_file_size)
            .with_context(|| format!("read source file {}", full.display()))?;
        Ok(extraction_of(relative, &source, options, |_| {}))
    };

    let mut results = if options.parallel {
        files.par_iter().map(parse).collect::<Result<Vec<_>>>()?
    } else {
        files.iter().map(parse).collect::<Result<Vec<_>>>()?
    };
    merge_results(&mut results)
}

/// Result of one source discovery walk. Unsupported extensions are collected
/// during the same traversal so an unsupported-only project can be reported
/// honestly without a second filesystem scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanProjectResult {
    pub files: Vec<String>,
    pub unsupported_by_extension: BTreeMap<String, usize>,
    /// Every symlink the scan followed, sorted by logical path (#935).
    pub links: Vec<FollowedLink>,
    /// Logical path of every directory scanned through a symlink, the links
    /// themselves included, sorted. The watcher watches only these below a link.
    pub linked_dirs: Vec<String>,
    /// For each file link whose target sits in a scanned directory: the
    /// target's logical path → the link paths that alias it, sorted.
    pub file_aliases: BTreeMap<String, Vec<String>>,
}

/// A symlink the scan followed: its logical path, the canonical path it
/// resolved to, and whether it named a file or a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowedLink {
    pub relative: String,
    pub canonical: PathBuf,
    pub kind: LinkKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LinkKind {
    File,
    Dir,
}

/// How the project scan treats one root-relative path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Membership {
    /// [`scan_project`] yields the file, or descends into the directory.
    Admitted,
    /// The scan never yields it — including a path that does not exist.
    Rejected,
    /// The path runs through a symlink, which only a full scan can resolve,
    /// or a component could not be inspected.
    Unknown,
}

/// Whether [`scan_project`] would yield `relative` as a source file or, with
/// `dir`, descend into it as a directory, judged component by component by the
/// same per-entry rules the walk applies.
pub fn scan_membership(
    root: &Path,
    options: &ExtractOptions,
    relative: &str,
    dir: bool,
) -> Membership {
    let components = relative
        .split('/')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    if components.is_empty() || components.iter().any(|c| *c == "." || *c == "..") {
        return Membership::Rejected;
    }
    let ignored_dirs = options
        .ignore_dirs
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let gitignore = RootGitignore::load(root);
    let pattern_sets: Vec<&[String]> = vec![&options.ignore_paths, &options.exclude];
    let include = IncludeSet::new(&options.include, &options.exclude);
    let reserved_roots = codegraph_core::IndexPaths::reserved_index_roots(
        root,
        std::env::var("CODEGRAPH_DIR").ok().as_deref(),
    );
    let rules = ScanRules {
        root,
        ignored_dirs: &ignored_dirs,
        reserved_roots: &reserved_roots,
        pattern_sets: &pattern_sets,
        gitignore: &gitignore,
        include: &include,
        overrides: &options.extensions,
    };
    let mut parent = root.to_path_buf();
    let mut above = GitignoreAncestry::default();
    for (index, name) in components.iter().enumerate() {
        let path = parent.join(name);
        let partial = components[..=index].join("/");
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Membership::Rejected;
            }
            Err(_) => return Membership::Unknown,
        };
        if metadata.file_type().is_symlink() {
            return Membership::Unknown;
        }
        let is_dir = metadata.is_dir();
        if rules.prunes_by_name(&parent, &path, name, &partial, || is_dir) {
            return Membership::Rejected;
        }
        let last = index + 1 == components.len();
        if last && !dir {
            if !metadata.is_file() {
                return Membership::Rejected;
            }
            return match rules.decide(&partial, false, above) {
                EntryDecision::File { extractable: true } => Membership::Admitted,
                _ => Membership::Rejected,
            };
        }
        if !is_dir {
            return Membership::Rejected;
        }
        match rules.decide(&partial, true, above) {
            EntryDecision::Descend(next) => above = next,
            _ => return Membership::Rejected,
        }
        parent = path;
    }
    Membership::Admitted
}

pub fn scan_project(root: &Path, options: &ExtractOptions) -> Result<Vec<String>> {
    Ok(scan_project_with_stats(root, options)?.files)
}

pub fn scan_project_with_stats(root: &Path, options: &ExtractOptions) -> Result<ScanProjectResult> {
    let ignored_dirs = options
        .ignore_dirs
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let gitignore = RootGitignore::load(root);
    // Evaluated in order (default paths → config exclude → .gitignore), so a
    // later `!pattern` negation re-includes a path an earlier set excluded.
    let pattern_sets: Vec<&[String]> = vec![&options.ignore_paths, &options.exclude];
    let include = IncludeSet::new(&options.include, &options.exclude);
    // The EXACT resolved reserved index-root PATH for THIS project, resolved
    // once. scan_dir prunes a directory iff its FULL path equals this root,
    // never by basename, so a user `.codegraph-sources/` stays scannable and an
    // unrelated same-basename directory is never mis-excluded.
    let reserved_roots = codegraph_core::IndexPaths::reserved_index_roots(
        root,
        std::env::var("CODEGRAPH_DIR").ok().as_deref(),
    );
    // No symlink may lead into the project's `.git` or a reserved index root.
    let blocked = std::iter::once(root.join(".git")).chain(reserved_roots.iter().cloned());
    let mut walk = ScanWalk {
        rules: ScanRules {
            root,
            ignored_dirs: &ignored_dirs,
            reserved_roots: &reserved_roots,
            pattern_sets: &pattern_sets,
            gitignore: &gitignore,
            include: &include,
            overrides: &options.extensions,
        },
        links: LinkWalk::new(root, blocked),
        files: Vec::new(),
        unsupported_by_extension: BTreeMap::new(),
        followed: Vec::new(),
        linked_dirs: Vec::new(),
        file_links: Vec::new(),
    };
    let canonical_root = walk
        .links
        .canonical_root()
        .map_or_else(|| root.to_path_buf(), Path::to_path_buf);
    walk.links.enter(canonical_root.clone(), "");
    walk.scan_dir(root, &canonical_root, GitignoreAncestry::default(), 0)?;
    // Symlinked directories, fewest hops first, then by logical path (#935).
    while let Some(linked) = walk.links.next() {
        walk.followed.push(FollowedLink {
            relative: linked.relative.clone(),
            canonical: linked.canonical.clone(),
            kind: LinkKind::Dir,
        });
        walk.linked_dirs.push(linked.relative);
        walk.scan_dir(&linked.path, &linked.canonical, linked.state, linked.hops)?;
    }
    Ok(walk.finish())
}

/// The scan's per-entry rules, shared by the walk and [`scan_membership`] so
/// that single-path membership cannot drift from what the walk yields.
struct ScanRules<'a> {
    root: &'a Path,
    ignored_dirs: &'a HashSet<&'a str>,
    reserved_roots: &'a std::collections::BTreeSet<PathBuf>,
    pattern_sets: &'a [&'a [String]],
    gitignore: &'a RootGitignore,
    include: &'a IncludeSet<'a>,
    overrides: &'a ExtensionOverrides,
}

/// The scan's verdict on one entry whose kind is known.
enum EntryDecision {
    Skip,
    /// Descend into the directory with this ancestry.
    Descend(GitignoreAncestry),
    /// A file the ignore model admits; it is indexed iff `extractable`, and
    /// otherwise counted as an unsupported extension.
    File {
        extractable: bool,
    },
}

impl ScanRules<'_> {
    /// Whether the entry `name` at `path` inside `dir` is pruned before its
    /// kind is read: `.git` directly under the root, an exact reserved index
    /// root at any depth, or an `ignore_dirs` name — except a `build` that is a
    /// JVM package segment under a conventional source root (#1642).
    fn prunes_by_name(
        &self,
        dir: &Path,
        path: &Path,
        name: &str,
        relative: &str,
        resolves_to_dir: impl FnOnce() -> bool,
    ) -> bool {
        // Skip `.git` (a direct child of the scan root) and any directory whose
        // FULL path is a resolved reserved index root — matched at any depth, so
        // a nested configured root like `<root>/cache/index` is pruned exactly,
        // while a same-basename user directory elsewhere is not.
        let is_reserved_root_here =
            (dir == self.root && name == ".git") || self.reserved_roots.contains(path);
        // `build` is also a legal JVM package segment: keep it under a
        // conventional source root while still pruning build output (#1642).
        let jvm_package = name == "build"
            && codegraph_core::config::is_jvm_source_build_dir(relative)
            && resolves_to_dir();
        is_reserved_root_here || (self.ignored_dirs.contains(name) && !jvm_package)
    }

    fn decide(&self, relative: &str, is_dir: bool, above: GitignoreAncestry) -> EntryDecision {
        let own = self.gitignore.matched(relative, is_dir);
        let ignored = is_path_ignored(relative, self.pattern_sets, above.decide(own));
        if is_dir {
            // A model-ignored dir is normally pruned before descent, so a FILE
            // include under a gitignored ancestor would never be reached.
            // Descend anyway when this dir is an ancestor of (or matches) an
            // include pattern; files inside are still pruned unless force-included.
            if ignored && !self.include.wants_descend(relative) {
                EntryDecision::Skip
            } else {
                EntryDecision::Descend(above.child(own))
            }
        } else if !ignored || self.include.forces(relative) {
            // Post-model include decision: a model-ignored file is force-included
            // iff it matches `include` and is NOT overridden by an explicit
            // `exclude` (checked inside `IncludeSet::forces`). Built-in dir skips
            // are already handled structurally above, so include can never
            // resurface node_modules/dist/.git/etc.
            EntryDecision::File {
                extractable: is_extractable_source_path(relative, self.overrides),
            }
        } else {
            EntryDecision::Skip
        }
    }
}

/// One scan's fixed inputs and accumulated output.
struct ScanWalk<'a> {
    rules: ScanRules<'a>,
    links: LinkWalk<GitignoreAncestry>,
    files: Vec<String>,
    unsupported_by_extension: BTreeMap<String, usize>,
    followed: Vec<FollowedLink>,
    linked_dirs: Vec<String>,
    /// Indexed file links: logical path → canonical target.
    file_links: Vec<(String, PathBuf)>,
}

impl ScanWalk<'_> {
    /// Scan `dir`, whose canonical path is `canonical_dir`, reached through
    /// `hops` symlinks. Real subdirectories are scanned at once; a symlinked
    /// one is queued for the next hop level.
    fn scan_dir(
        &mut self,
        dir: &Path,
        canonical_dir: &Path,
        above: GitignoreAncestry,
        hops: usize,
    ) -> Result<()> {
        let entries = fs::read_dir(dir).with_context(|| format!("read dir {}", dir.display()))?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            let relative = normalize_path(path.strip_prefix(self.rules.root).unwrap_or(&path));
            if self.rules.prunes_by_name(dir, &path, &name, &relative, || {
                resolves_to_dir(&entry, &path)
            }) {
                continue;
            }
            let file_type = entry.file_type()?;
            // A symlink is judged at its logical path by what it resolves to
            // (#935). A broken one, or one to anything but a file or a
            // directory, is skipped.
            let (is_dir, is_link) = if file_type.is_symlink() {
                match fs::metadata(&path) {
                    Ok(target) if target.is_dir() => (true, true),
                    Ok(target) if target.is_file() => (false, true),
                    _ => continue,
                }
            } else if file_type.is_dir() {
                (true, false)
            } else if file_type.is_file() {
                (false, false)
            } else {
                continue;
            };
            match self.rules.decide(&relative, is_dir, above) {
                EntryDecision::Skip => {}
                EntryDecision::Descend(next) => {
                    if is_link {
                        self.links.queue(hops + 1, relative, path, next);
                        continue;
                    }
                    let canonical = canonical_dir.join(&file_name);
                    if !self.links.enter(canonical.clone(), &relative) {
                        continue;
                    }
                    if hops > 0 {
                        self.linked_dirs.push(relative);
                    }
                    self.scan_dir(&path, &canonical, next, hops)?;
                }
                EntryDecision::File { extractable } => {
                    let target = if is_link {
                        match self.links.file_target(&path) {
                            Some(target) => Some(target),
                            None => continue,
                        }
                    } else {
                        None
                    };
                    if extractable {
                        if let Some(target) = target {
                            self.followed.push(FollowedLink {
                                relative: relative.clone(),
                                canonical: target.clone(),
                                kind: LinkKind::File,
                            });
                            self.file_links.push((relative.clone(), target));
                        }
                        self.files.push(relative);
                    } else if let Some(extension) = unsupported_extension(&relative) {
                        *self.unsupported_by_extension.entry(extension).or_default() += 1;
                    }
                }
            }
        }
        Ok(())
    }

    fn finish(mut self) -> ScanProjectResult {
        self.files.sort();
        self.followed
            .sort_by(|left, right| left.relative.cmp(&right.relative));
        self.linked_dirs.sort();
        // A file link aliases its target's logical path when the target sits
        // in a scanned directory, so an edit there can re-index the link too.
        let mut file_aliases = BTreeMap::<String, Vec<String>>::new();
        for (alias, target) in self.file_links {
            let (Some(parent), Some(name)) = (target.parent(), target.file_name()) else {
                continue;
            };
            let Some(dir) = self.links.logical_dir(parent) else {
                continue;
            };
            let name = name.to_string_lossy();
            let primary = if dir.is_empty() {
                name.into_owned()
            } else {
                format!("{dir}/{name}")
            };
            file_aliases.entry(primary).or_default().push(alias);
        }
        for aliases in file_aliases.values_mut() {
            aliases.sort();
        }
        ScanProjectResult {
            files: self.files,
            unsupported_by_extension: self.unsupported_by_extension,
            links: self.followed,
            linked_dirs: self.linked_dirs,
            file_aliases,
        }
    }
}

/// Whether a directory entry is a directory, or a symlink to one.
fn resolves_to_dir(entry: &fs::DirEntry, path: &Path) -> bool {
    entry.file_type().is_ok_and(|kind| {
        kind.is_dir() || (kind.is_symlink() && fs::metadata(path).is_ok_and(|meta| meta.is_dir()))
    })
}

fn unsupported_extension(relative: &str) -> Option<String> {
    let extension = Path::new(relative).extension()?.to_str()?;
    (!extension.is_empty()).then(|| format!(".{}", extension.to_ascii_lowercase()))
}

fn merge_results(results: &mut [ExtractionResult]) -> Result<ExtractionResult> {
    let mut merged = ExtractionResult {
        nodes: Vec::new(),
        edges: Vec::new(),
        unresolved_references: Vec::new(),
        errors: Vec::new(),
        duration_ms: 0,
    };
    for result in results {
        merged.duration_ms += result.duration_ms;
        merged.nodes.append(&mut result.nodes);
        merged.edges.append(&mut result.edges);
        merged
            .unresolved_references
            .append(&mut result.unresolved_references);
        merged.errors.append(&mut result.errors);
    }
    Ok(merged)
}

/// The result recorded for a file over the size limit: no symbols, and the
/// error every indexing path reports for it.
pub fn size_skip_result(file_path: &str, size: u64, max: u64) -> ExtractionResult {
    ExtractionResult {
        nodes: Vec::new(),
        edges: Vec::new(),
        unresolved_references: Vec::new(),
        errors: vec![format!("{SIZE_SKIP_PREFIX} ({size} > {max}): {file_path}")],
        duration_ms: 0,
    }
}

/// The repository's own exclude file, relative to the project root. git reads
/// it beside every `.gitignore` for files nobody wants committed (upstream
/// #1728).
pub const REPOSITORY_EXCLUDE: &str = ".git/info/exclude";

/// The largest exclude file read; a bigger one is treated as absent.
const REPOSITORY_EXCLUDE_MAX_BYTES: u64 = 1024 * 1024;

/// The directory holding [`REPOSITORY_EXCLUDE`], when both it and `.git` are
/// real directories of the project root. A `.git` file (a linked worktree or a
/// submodule) names a git directory outside the project, and a symlinked
/// `.git` or `info` can lead there too, so none of them is followed.
pub fn repository_exclude_dir(root: &Path) -> Option<PathBuf> {
    let real_dir = |path: &Path| {
        fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir())
    };
    let git = root.join(".git");
    let info = git.join("info");
    (real_dir(&git) && real_dir(&info)).then_some(info)
}

/// The text of [`REPOSITORY_EXCLUDE`]: a regular file (not a link) in
/// [`repository_exclude_dir`], read through the bounded source reader, so it
/// is stat-ed before it is opened and a FIFO there is never opened. `None`
/// when there is no such file, or it is unreadable, over the size limit or
/// NUL-laden.
pub fn read_repository_exclude(root: &Path) -> Option<String> {
    let path = repository_exclude_dir(root)?.join("exclude");
    if !fs::symlink_metadata(&path).ok()?.file_type().is_file() {
        return None;
    }
    match read_source_file(&path, REPOSITORY_EXCLUDE, REPOSITORY_EXCLUDE_MAX_BYTES)
        .ok()?
        .1
    {
        SourceText::Text(text) if !text.contains('\0') => Some(text),
        _ => None,
    }
}

/// The project-root `.gitignore`, together with the repository's own
/// [`REPOSITORY_EXCLUDE`], read with git's own rules through the `ignore`
/// crate, as upstream reads them with the `ignore` package: a leading or inner
/// `/` anchors a rule to the root, a slash-less rule applies at any depth, `*`
/// and `**` glob, a trailing `/` matches directories only, and `!` re-includes.
/// git ranks a `.gitignore` above the exclude file, so the exclude rules are
/// read first and a `.gitignore` line wins a conflict with them. Shared with
/// `codegraph-watch`, so the watcher's verdict is the scan's by construction.
/// An unreadable, non-UTF-8 or NUL-laden `.gitignore` is treated as absent and
/// an unparseable line is skipped, never fatal.
#[derive(Debug, Clone, Default)]
pub struct RootGitignore {
    matcher: Option<ignore::gitignore::Gitignore>,
}

impl RootGitignore {
    pub fn load(root: &Path) -> Self {
        let gitignore = fs::read_to_string(root.join(".gitignore"))
            .ok()
            .filter(|text| !text.contains('\0'));
        // Rooted at `.` so a root-relative candidate is never prefix-stripped.
        let mut builder = ignore::gitignore::GitignoreBuilder::new(".");
        for text in [read_repository_exclude(root), gitignore]
            .into_iter()
            .flatten()
        {
            for line in text.lines() {
                // An invalid glob drops only its own line; the rest still apply.
                let _ = builder.add_line(None, line);
            }
        }
        let matcher = builder.build().ok().filter(|matcher| !matcher.is_empty());
        Self { matcher }
    }

    /// The root `.gitignore` verdict for `relative` (root-relative,
    /// `/`-separated): `Some(true)` ignored, `Some(false)` re-included, `None`
    /// undecided. Walks the directories above it; the scan instead carries
    /// that state down its walk in a [`GitignoreAncestry`].
    pub fn verdict(&self, relative: &str, is_dir: bool) -> Option<bool> {
        self.matcher.as_ref()?;
        let above = relative
            .match_indices('/')
            .fold(GitignoreAncestry::default(), |above, (end, _)| {
                above.child(self.matched(&relative[..end], true))
            });
        above.decide(self.matched(relative, is_dir))
    }

    /// The last rule matching `relative` itself, ignoring its ancestors.
    fn matched(&self, relative: &str, is_dir: bool) -> Option<bool> {
        match self.matcher.as_ref()?.matched(relative, is_dir) {
            ignore::Match::None => None,
            ignore::Match::Ignore(_) => Some(true),
            ignore::Match::Whitelist(_) => Some(false),
        }
    }
}

/// What the root `.gitignore` decided for the directories above a path.
#[derive(Debug, Clone, Copy, Default)]
struct GitignoreAncestry {
    ignored: bool,
    reincluded: bool,
}

impl GitignoreAncestry {
    /// The ancestry of an entry below a directory whose own verdict was `own`.
    fn child(self, own: Option<bool>) -> Self {
        Self {
            ignored: self.ignored || own == Some(true),
            reincluded: self.reincluded || own == Some(false),
        }
    }

    /// An ignored ancestor hides the path, since git never re-includes below an
    /// excluded directory; otherwise the path's own last matching rule decides,
    /// and failing that a re-included ancestor re-includes it, so `!res/values/`
    /// still overrides a default path set that matched the files inside.
    fn decide(self, own: Option<bool>) -> Option<bool> {
        if self.ignored {
            Some(true)
        } else {
            own.or(self.reincluded.then_some(false))
        }
    }
}

/// Evaluate the ordered `.gitignore`-style config pattern sets with last-match
/// wins negation, then let the root `.gitignore` verdict, when it has one,
/// decide last: a `!pattern` line un-ignores a path an earlier pattern
/// excluded. Sets are scanned in order, patterns within a set in order, and the
/// final matching pattern decides — so a later `!res/values/` re-includes what
/// a default `res/values*` excluded.
fn is_path_ignored(relative: &str, pattern_sets: &[&[String]], gitignore: Option<bool>) -> bool {
    let mut ignored = false;
    for set in pattern_sets {
        for pattern in set.iter() {
            if let Some(negated) = pattern.strip_prefix('!') {
                if pattern_matches(relative, negated) {
                    ignored = false;
                }
            } else if pattern_matches(relative, pattern) {
                ignored = true;
            }
        }
    }
    gitignore.unwrap_or(ignored)
}

fn pattern_matches(relative: &str, pattern: &str) -> bool {
    if let Some(dir) = pattern.strip_suffix('/') {
        relative == dir || relative.starts_with(&format!("{dir}/"))
    } else if let Some(prefix) = pattern.strip_suffix('*') {
        relative.starts_with(prefix)
    } else {
        relative == pattern || relative.ends_with(&format!("/{pattern}"))
    }
}

/// Single source of truth for the `include`/`exclude` PATH-MATCH decision,
/// shared with `codegraph-watch` so the live watcher's scope is byte-identical
/// to the scan's (AGENTS.md "sync == index --force"). This is the WHOLE-relative
/// path `.gitignore`-style semantics of [`pattern_matches`] — NOT the watcher's
/// basename-glob `rule_matches`, which the two crates previously diverged on
/// (`gen*` matched `gen/helper.ts` in the scan but not the watcher). Argument
/// order is `(pattern, relative)` to read like "does this pattern match?".
pub fn include_exclude_pattern_matches(pattern: &str, relative: &str) -> bool {
    pattern_matches(relative, pattern)
}

/// The `include` force-inclusion decision (#1063), kept separate from the
/// ordered `.gitignore`-style model so it can flip a model-ignored path back in
/// AFTER that model returns its verdict. An explicit config `exclude` is checked
/// here so `exclude` always wins over `include`. A built-in `ignore_dirs` skip is
/// NOT re-checked — those are pruned structurally in `scan_dir` and can never
/// reach an include decision. Empty `include` makes every method a cheap `false`,
/// so the scan stays byte-identical to today.
struct IncludeSet<'a> {
    include: &'a [String],
    exclude: &'a [String],
}

impl<'a> IncludeSet<'a> {
    fn new(include: &'a [String], exclude: &'a [String]) -> Self {
        Self { include, exclude }
    }

    fn is_empty(&self) -> bool {
        self.include.is_empty()
    }

    /// A model-ignored FILE is force-included iff it matches an `include`
    /// pattern and is not knocked out by an explicit `exclude` (exclude wins).
    fn forces(&self, relative: &str) -> bool {
        if self.is_empty() {
            return false;
        }
        self.include
            .iter()
            .any(|p| include_file_matches(relative, p))
            && !self.exclude.iter().any(|p| pattern_matches(relative, p))
    }

    /// Whether a model-ignored DIRECTORY must still be descended: it either
    /// matches an include pattern itself, or is an ANCESTOR of one (a nested
    /// `Tools/gen/x.ts` include needs `Tools/` and `Tools/gen/` walked). An
    /// explicit `exclude` on the directory prunes the whole subtree (exclude
    /// wins), mirroring `forces`.
    fn wants_descend(&self, relative: &str) -> bool {
        if self.is_empty() {
            return false;
        }
        if self.exclude.iter().any(|p| pattern_matches(relative, p)) {
            return false;
        }
        self.include
            .iter()
            .any(|p| include_touches_dir(relative, p))
    }
}

/// True when include `pattern` matches, or could match, something at or below
/// the directory `dir` (root-relative, no trailing slash) — i.e. whether the
/// ancestor dir of an included path must be descended. A bare name with no `/`
/// (e.g. `local`) names a file that `pattern_matches` accepts at ANY depth, so
/// like upstream's whole-tree walk it touches every dir. Otherwise the pattern
/// has a static path stem (dropping a trailing `/` or `*`): the dir is touched
/// when it is at/under the stem (`Tools` under `Tools/**`) or the stem is
/// at/under the dir (`Tools/gen` under the ancestor `Tools`).
fn include_touches_dir(dir: &str, pattern: &str) -> bool {
    if !pattern.contains('/') && !pattern.ends_with('*') {
        return true;
    }
    let stem = include_static_stem(pattern);
    if stem.is_empty() {
        return true;
    }
    dir == stem || dir.starts_with(&format!("{stem}/")) || stem.starts_with(&format!("{dir}/"))
}

/// The literal leading directory of an include `pattern`, trailing slash / `*` /
/// `**` dropped: `Tools/` → `Tools`, `Tools/**` → `Tools`, `Local/ts/x.ts` →
/// `Local/ts/x.ts` (no glob, so the whole thing is literal). Used only for the
/// directory-descent ancestor test.
fn include_static_stem(pattern: &str) -> &str {
    let p = pattern.trim_end_matches('/');
    match p.split_once('*') {
        Some((prefix, _)) => prefix.trim_end_matches('/'),
        None => p,
    }
}

/// Whether an include `pattern` force-includes the FILE at root-relative
/// `relative`. Extends `pattern_matches` with `**` support (`Tools/**` matches
/// every file under `Tools/`), which the ordered-model matcher deliberately does
/// not handle. A `dir/`-suffixed pattern matches any file under that dir; a
/// trailing `/**` (or bare `**`) matches everything under its prefix; the
/// remaining forms defer to `pattern_matches`.
fn include_file_matches(relative: &str, pattern: &str) -> bool {
    if let Some(prefix) = pattern
        .strip_suffix("/**")
        .or_else(|| if pattern == "**" { Some("") } else { None })
    {
        return prefix.is_empty()
            || relative == prefix
            || relative.starts_with(&format!("{prefix}/"));
    }
    pattern_matches(relative, pattern)
}

fn is_extractable_source_path(relative: &str, overrides: &ExtensionOverrides) -> bool {
    let language = detect_language_with(relative, overrides);
    language != Language::Unknown
        && (crate::lang::spec_for_language(language).is_some()
            || crate::embedded::is_embedded_source_path(relative)
            || is_file_level_only_language(language))
}

/// Why an indexed file whose content is current has none of the symbols it
/// should have (upstream #2336). A content-hash comparison calls such a file
/// up to date, so only its stored row can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingSymbols {
    /// Stored with no nodes and no recorded reason; every parse stores at least
    /// the file node, so the row was wiped and a rebuild restores it.
    NeedsReindex,
    /// A recorded parse failure left it without symbols; it stays this way
    /// until the file or the parser changes.
    ParseError,
}

/// Classify one stored file row; `None` when it has its symbols or is empty on
/// purpose (a file over the size limit, a file-level-only language).
#[must_use]
pub fn missing_symbols(file: &codegraph_core::types::FileRecord) -> Option<MissingSymbols> {
    let collapsed = file
        .errors
        .iter()
        .any(|error| error.contains(PARSE_COLLAPSE_WARNING));
    if file.node_count > 0 {
        return collapsed.then_some(MissingSymbols::ParseError);
    }
    if is_file_level_only_language(file.language) {
        return None;
    }
    if file.errors.is_empty() {
        return Some(MissingSymbols::NeedsReindex);
    }
    let skipped_for_size = file
        .errors
        .iter()
        .all(|error| error.starts_with(SIZE_SKIP_PREFIX));
    (!skipped_for_size).then_some(MissingSymbols::ParseError)
}

/// The start of the error [`size_skip_result`] records.
const SIZE_SKIP_PREFIX: &str = "File exceeds max size";

fn is_file_level_only_language(language: Language) -> bool {
    matches!(
        language,
        Language::Yaml
            | Language::Twig
            | Language::Properties
            | Language::GodotScene
            | Language::GodotResource
            | Language::GodotProject
    )
}

fn normalize_path(path: impl AsRef<Path>) -> String {
    path.as_ref()
        .components()
        .collect::<PathBuf>()
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    #[test]
    fn missing_symbols_classifies_stored_rows_like_upstream() {
        use codegraph_core::types::FileRecord;
        let row = |language: Language, node_count: i64, errors: &[&str]| FileRecord {
            path: "src/a.ts".to_string(),
            content_hash: String::new(),
            language,
            size: 1,
            modified_at: 0,
            indexed_at: 0,
            node_count,
            errors: errors.iter().map(|error| (*error).to_string()).collect(),
            generated: false,
        };
        let collapsed = format!(
            "src/a.ts: {PARSE_COLLAPSE_WARNING} - the file is indexed but contributes nothing to the graph"
        );
        assert_eq!(missing_symbols(&row(Language::TypeScript, 3, &[])), None);
        assert_eq!(
            missing_symbols(&row(Language::TypeScript, 1, &[&collapsed])),
            Some(MissingSymbols::ParseError)
        );
        assert_eq!(
            missing_symbols(&row(Language::TypeScript, 0, &[])),
            Some(MissingSymbols::NeedsReindex)
        );
        assert_eq!(missing_symbols(&row(Language::Yaml, 0, &[])), None);
        assert_eq!(missing_symbols(&row(Language::GodotScene, 0, &[])), None);
        let oversized = size_skip_result("src/a.ts", 9, 4).errors;
        let oversized = oversized.iter().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(
            missing_symbols(&row(Language::TypeScript, 0, &oversized)),
            None
        );
        assert_eq!(
            missing_symbols(&row(Language::TypeScript, 0, &["could not read the file"])),
            Some(MissingSymbols::ParseError)
        );
    }

    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::SystemTime;

    fn unique_project(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("cg_scan_{tag}_{}_{nanos}_{n}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp project");
        dir
    }

    fn touch(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).expect("create parent dirs");
        fs::write(&path, contents).expect("write file");
    }

    /// The scanner excludes ONLY the exact selected index root, never an
    /// arbitrary `.codegraph-*`-prefixed user directory.
    #[test]
    fn scan_keeps_user_codegraph_prefixed_dir_but_excludes_reserved_roots() {
        let project = unique_project("reserved_roots");
        touch(
            &project,
            ".codegraph-sources/kept.ts",
            "export const a = 1;",
        );
        touch(&project, ".codegraph/codegraph.db", "reserved");
        touch(
            &project,
            ".codegraph-v2/retired_name.ts",
            "export const retired = 0;",
        );
        touch(&project, "src/app.ts", "export const b = 2;");

        let files = scan_project(&project, &ExtractOptions::default()).expect("scan project");

        assert!(
            files.contains(&".codegraph-sources/kept.ts".to_string()),
            "a user `.codegraph-sources` dir must stay scannable: {files:?}"
        );
        assert!(
            files.contains(&"src/app.ts".to_string()),
            "ordinary source must be indexed: {files:?}"
        );
        assert!(
            !files.iter().any(|f| f.starts_with(".codegraph/")),
            "the selected `.codegraph` root must be excluded: {files:?}"
        );
        assert!(
            files.contains(&".codegraph-v2/retired_name.ts".to_string()),
            "the retired basename is not reserved and must stay scannable: {files:?}"
        );

        fs::remove_dir_all(&project).ok();
    }

    /// Exclusion is by EXACT resolved root PATH, not basename: a directory that
    /// merely SHARES the reserved basename but lives at a different path (nested
    /// under a source subtree) is NOT a resolved root and stays scannable.
    #[test]
    fn scan_excludes_only_top_level_root_paths_not_same_named_nested_dirs() {
        let project = unique_project("reserved_nested");
        touch(&project, ".codegraph/inner.ts", "export const r = 0;");
        touch(&project, "src/.codegraph/nested.ts", "export const a = 1;");
        touch(&project, "src/app.ts", "export const b = 2;");

        let files = scan_project(&project, &ExtractOptions::default()).expect("scan project");

        assert!(
            files.contains(&"src/.codegraph/nested.ts".to_string()),
            "a same-basename dir nested off the root is NOT the reserved root: {files:?}"
        );
        assert!(files.contains(&"src/app.ts".to_string()), "{files:?}");
        assert!(
            !files.iter().any(|f| f.starts_with(".codegraph/")),
            "the top-level `.codegraph` root is still excluded: {files:?}"
        );

        fs::remove_dir_all(&project).ok();
    }

    /// A real directory scan of a Godot project skips the regenerated `.godot/`
    /// engine cache and the vendored `addons/` plugin tree while still finding
    /// first-party `.gd` business code.
    #[test]
    fn scan_ignores_godot_cache_and_addons_by_default() {
        let project = unique_project("godot");
        touch(&project, "player.gd", "extends Node");
        touch(&project, ".godot/imported/icon.png-abc.ctex", "cache");
        touch(&project, ".godot/global_script_class_cache.cfg", "[]");
        touch(
            &project,
            "addons/some_plugin/plugin.gd",
            "extends EditorPlugin",
        );

        let options = ExtractOptions::default();
        let files = scan_project(&project, &options).expect("scan project");

        assert!(
            files.contains(&"player.gd".to_string()),
            "first-party business code must be indexed: {files:?}"
        );
        assert!(
            !files.iter().any(|f| f.starts_with(".godot/")),
            ".godot/ engine cache must be skipped: {files:?}"
        );
        assert!(
            !files.iter().any(|f| f.starts_with("addons/")),
            "addons/ vendored plugins must be skipped: {files:?}"
        );

        fs::remove_dir_all(&project).ok();
    }

    /// `build` is a legal JVM package segment: a Java/Kotlin/Scala package
    /// directory named `build` under a conventional source root stays indexed,
    /// while module build output and the other defaults inside it stay pruned
    /// (upstream #1642).
    #[test]
    fn scan_keeps_jvm_packages_named_build_but_prunes_build_output() {
        let project = unique_project("jvm_build_package");
        touch(
            &project,
            "src/main/java/com/acme/build/Builder.java",
            "class Builder {}",
        );
        touch(
            &project,
            "app/src/test/kotlin/build/BuildTest.kt",
            "class BuildTest",
        );
        touch(&project, "src/main/scala/build/Tool.scala", "object Tool");
        touch(&project, "build/generated/Gen.java", "class Gen {}");
        touch(&project, "app/build/classes/Out.java", "class Out {}");
        touch(
            &project,
            "src/main/resources/build/Res.java",
            "class Res {}",
        );
        touch(
            &project,
            "src/main/java/build/node_modules/dep.js",
            "export {}",
        );

        let files = scan_project(&project, &ExtractOptions::default()).expect("scan project");
        assert_eq!(
            files,
            vec![
                "app/src/test/kotlin/build/BuildTest.kt".to_string(),
                "src/main/java/com/acme/build/Builder.java".to_string(),
                "src/main/scala/build/Tool.scala".to_string(),
            ]
        );

        fs::remove_dir_all(&project).ok();
    }

    /// A team authoring first-party code under `addons/` can re-include it by
    /// overriding `indexing.ignore_dirs` (the same override surface a custom
    /// `.codegraph/config.toml` populates), proving the default is opt-out.
    #[test]
    fn scan_reincludes_addons_when_override_drops_it() {
        let project = unique_project("godot_override");
        touch(&project, "addons/first_party/tool.gd", "extends Node");
        touch(&project, ".godot/cache.cfg", "[]");

        let mut options = ExtractOptions::default();
        options.ignore_dirs.retain(|dir| dir != "addons");
        let files = scan_project(&project, &options).expect("scan project");
        assert!(
            files.contains(&"addons/first_party/tool.gd".to_string()),
            "addons/ must be re-includable via ignore_dirs override: {files:?}"
        );
        assert!(
            !files.iter().any(|f| f.starts_with(".godot/")),
            ".godot/ stays ignored even when addons is re-included: {files:?}"
        );

        fs::remove_dir_all(&project).ok();
    }

    /// `[indexing] exclude` skips root-relative path patterns the same way
    /// `.gitignore` does, while leaving everything else indexed.
    #[test]
    fn scan_honors_config_exclude_patterns() {
        let project = unique_project("exclude");
        touch(&project, "src/app.ts", "export const a = 1;");
        touch(&project, "static/bundle.ts", "export const b = 2;");
        touch(&project, "gen/out.ts", "export const c = 3;");

        let options = ExtractOptions {
            exclude: vec!["static/".to_string(), "gen".to_string()],
            ..ExtractOptions::default()
        };
        let files = scan_project(&project, &options).expect("scan project");

        assert!(
            files.contains(&"src/app.ts".to_string()),
            "non-excluded source must be indexed: {files:?}"
        );
        assert!(
            !files.iter().any(|f| f.starts_with("static/")),
            "excluded static/ must be skipped: {files:?}"
        );
        assert!(
            !files.iter().any(|f| f.starts_with("gen/")),
            "excluded gen must be skipped: {files:?}"
        );

        fs::remove_dir_all(&project).ok();
    }

    /// #1063 (a): a `.gitignore`d dir named in `include` is force-indexed.
    #[test]
    fn scan_include_forces_gitignored_dir_into_index() {
        let project = unique_project("include_dir");
        touch(&project, ".gitignore", "Tools/\n");
        touch(&project, "src/app.ts", "export const a = 1;");
        touch(&project, "Tools/helper.ts", "export const b = 2;");

        let options = ExtractOptions {
            include: vec!["Tools/".to_string()],
            ..ExtractOptions::default()
        };
        let files = scan_project(&project, &options).expect("scan project");
        assert!(
            files.contains(&"Tools/helper.ts".to_string()),
            "gitignored Tools/ in include must be indexed: {files:?}"
        );
        assert!(files.contains(&"src/app.ts".to_string()));
        fs::remove_dir_all(&project).ok();
    }

    /// #1063 (b): a built-in skip (`node_modules`) named in `include` stays
    /// skipped — include can never resurface a built-in ignored dir.
    #[test]
    fn scan_include_never_reincludes_builtin_skip() {
        let project = unique_project("include_builtin");
        touch(&project, "src/app.ts", "export const a = 1;");
        touch(&project, "node_modules/pkg/index.ts", "export const x = 1;");

        let options = ExtractOptions {
            include: vec!["node_modules/".to_string()],
            ..ExtractOptions::default()
        };
        let files = scan_project(&project, &options).expect("scan project");
        assert!(
            !files.iter().any(|f| f.starts_with("node_modules/")),
            "include must not resurface a built-in skip: {files:?}"
        );
        assert!(files.contains(&"src/app.ts".to_string()));
        fs::remove_dir_all(&project).ok();
    }

    /// #1063 (c): a path in BOTH `include` and `exclude` is skipped — an
    /// explicit `exclude` always wins over `include`.
    #[test]
    fn scan_exclude_wins_over_include() {
        let project = unique_project("include_exclude");
        touch(&project, ".gitignore", "Tools/\n");
        touch(&project, "Tools/helper.ts", "export const b = 2;");

        let options = ExtractOptions {
            include: vec!["Tools/".to_string()],
            exclude: vec!["Tools/".to_string()],
            ..ExtractOptions::default()
        };
        let files = scan_project(&project, &options).expect("scan project");
        assert!(
            !files.iter().any(|f| f.starts_with("Tools/")),
            "exclude must win over include: {files:?}"
        );
        fs::remove_dir_all(&project).ok();
    }

    /// #1063 (d): an `include` naming a SINGLE FILE (or nested `**` glob) under a
    /// gitignored ANCESTOR is indexed — the ancestor dir is descended even though
    /// the ordered model would prune it, while non-included siblings stay pruned.
    #[test]
    fn scan_include_reaches_file_under_gitignored_ancestor() {
        let project = unique_project("include_ancestor");
        touch(&project, ".gitignore", "Local/\n");
        touch(&project, "Local/ts/wanted.ts", "export const w = 1;");
        touch(&project, "Local/ts/other.ts", "export const o = 2;");
        touch(&project, "Local/skip.ts", "export const s = 3;");

        let options = ExtractOptions {
            include: vec!["Local/ts/wanted.ts".to_string()],
            ..ExtractOptions::default()
        };
        let files = scan_project(&project, &options).expect("scan project");
        assert!(
            files.contains(&"Local/ts/wanted.ts".to_string()),
            "a file include under a gitignored ancestor must be indexed: {files:?}"
        );
        assert!(
            !files.contains(&"Local/ts/other.ts".to_string())
                && !files.contains(&"Local/skip.ts".to_string()),
            "non-included siblings under the ancestor stay pruned: {files:?}"
        );

        let glob = ExtractOptions {
            include: vec!["Local/ts/**".to_string()],
            ..ExtractOptions::default()
        };
        let files = scan_project(&project, &glob).expect("scan project");
        assert!(
            files.contains(&"Local/ts/wanted.ts".to_string())
                && files.contains(&"Local/ts/other.ts".to_string()),
            "a nested ** include pulls in the whole subdir: {files:?}"
        );
        assert!(
            !files.contains(&"Local/skip.ts".to_string()),
            "the ** glob does not reach a sibling outside its dir: {files:?}"
        );
        fs::remove_dir_all(&project).ok();
    }

    /// #1063 (e): empty `include` leaves the scanned file set byte-identical.
    #[test]
    fn scan_empty_include_is_byte_identical() {
        let project = unique_project("include_empty");
        touch(&project, ".gitignore", "vendor/\n");
        touch(&project, "src/app.ts", "export const a = 1;");
        touch(&project, "vendor/dep.ts", "export const d = 2;");

        let base = scan_project(&project, &ExtractOptions::default()).expect("scan");
        let with_empty = scan_project(
            &project,
            &ExtractOptions {
                include: Vec::new(),
                ..ExtractOptions::default()
            },
        )
        .expect("scan");
        assert_eq!(
            base, with_empty,
            "empty include must not change the file set"
        );
        assert!(!base.iter().any(|f| f.starts_with("vendor/")));
        fs::remove_dir_all(&project).ok();
    }

    #[test]
    fn extract_file_reads_and_parses_a_real_source_file() {
        let project = unique_project("extract_file");
        touch(&project, "src/lib.rs", "pub fn run() -> i32 { helper() }\n");
        let result = extract_file(&project, "src/lib.rs").expect("extract file");
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert!(
            result.nodes.iter().any(|n| n.name == "run"),
            "expected the run fn node: {:#?}",
            result.nodes
        );
        fs::remove_dir_all(&project).ok();
    }

    #[test]
    fn extract_project_merges_nodes_serially_and_in_parallel() {
        let project = unique_project("extract_project");
        touch(&project, "a.rs", "pub fn a() {}\n");
        touch(&project, "b.rs", "pub fn b() {}\n");

        let serial = ExtractOptions {
            parallel: false,
            ..ExtractOptions::default()
        };
        let merged = extract_project(&project, &serial).expect("serial extract");
        assert!(merged.nodes.iter().any(|n| n.name == "a"));
        assert!(merged.nodes.iter().any(|n| n.name == "b"));

        let parallel = ExtractOptions::default();
        let merged_par = extract_project(&project, &parallel).expect("parallel extract");
        assert!(merged_par.nodes.iter().any(|n| n.name == "a"));
        assert!(merged_par.nodes.iter().any(|n| n.name == "b"));

        fs::remove_dir_all(&project).ok();
    }

    #[test]
    fn extract_project_skips_over_size_limit_file_with_error() {
        let project = unique_project("extract_project_big");
        touch(&project, "small.rs", "pub fn ok() {}\n");
        touch(&project, "big.rs", &"// x\n".repeat(64));

        let options = ExtractOptions {
            max_file_size: 8,
            parallel: false,
            ..ExtractOptions::default()
        };
        let merged = extract_project(&project, &options).expect("extract");
        assert!(
            merged.errors.iter().any(|e| e.contains("exceeds max size")),
            "expected a size-skip error: {:?}",
            merged.errors
        );
        fs::remove_dir_all(&project).ok();
    }

    #[test]
    fn detect_language_unknown_for_extensionless_and_foreign_extensions() {
        assert_eq!(detect_language("README"), Language::Unknown);
        assert_eq!(detect_language("data.bin"), Language::Unknown);
        assert_eq!(detect_language("project.godot"), Language::GodotProject);
        assert_eq!(detect_language("src/lib.rs"), Language::Rust);
    }

    #[test]
    fn gd_uid_sidecar_never_becomes_file_record() {
        assert_eq!(
            detect_language("Scripts/effect_manager.gd.uid"),
            Language::Unknown
        );
        assert!(
            !is_extractable_source_path(
                "Scripts/effect_manager.gd.uid",
                &ExtensionOverrides::default()
            ),
            ".gd.uid sidecar must never be scanned into a file record"
        );
    }

    #[test]
    fn metal_cu_cuh_map_to_cpp() {
        assert_eq!(detect_language("s.metal"), Language::Cpp);
        assert_eq!(detect_language("k.cu"), Language::Cpp);
        assert_eq!(detect_language("k.cuh"), Language::Cpp);
        assert_eq!(Language::ALL.len(), 42);
    }

    #[test]
    fn arkts_extension_maps_to_arkts() {
        assert_eq!(detect_language("view.ets"), Language::ArkTs);
    }

    #[test]
    fn sol_maps_to_solidity() {
        assert_eq!(detect_language("Token.sol"), Language::Solidity);
    }

    #[test]
    fn nix_extension_maps_to_nix() {
        assert_eq!(detect_language("flake.nix"), Language::Nix);
    }

    #[test]
    fn terraform_extensions_map_to_terraform() {
        assert_eq!(detect_language("main.tf"), Language::Terraform);
        assert_eq!(detect_language("prod.tfvars"), Language::Terraform);
        assert_eq!(detect_language("main.tofu"), Language::Terraform);
    }

    #[test]
    fn erlang_extensions_map_to_erlang() {
        assert_eq!(detect_language("m.erl"), Language::Erlang);
        assert_eq!(detect_language("defs.hrl"), Language::Erlang);
    }

    #[test]
    fn cfml_extensions_map_to_cfml() {
        assert_eq!(detect_language("Widget.cfc"), Language::Cfml);
        assert_eq!(detect_language("page.cfm"), Language::Cfml);
        assert_eq!(detect_language("Gadget.cfs"), Language::Cfml);
    }

    #[test]
    fn plain_ts_stays_typescript() {
        assert_eq!(detect_language("m.ts"), Language::TypeScript);
    }

    #[test]
    fn extract_source_unknown_language_yields_empty_no_error() {
        let result = extract_source("mystery.unknownext", "content", None);
        assert!(result.nodes.is_empty());
        assert!(
            result.errors.is_empty(),
            "unknown language must be silent: {:?}",
            result.errors
        );
    }

    #[test]
    fn extract_source_file_level_only_language_is_empty() {
        let result = extract_source("config.yaml", "a: 1\nb: 2\n", Some(Language::Yaml));
        assert!(result.nodes.is_empty());
        assert!(result.edges.is_empty());
        assert!(result.errors.is_empty());
    }

    #[test]
    fn size_skip_result_formats_the_error_and_is_empty() {
        let skip = size_skip_result("huge.rs", 100, 50);
        assert!(skip.nodes.is_empty());
        assert_eq!(skip.errors.len(), 1);
        assert!(skip.errors[0].contains("100 > 50"));
        assert!(skip.errors[0].contains("huge.rs"));
    }

    #[test]
    fn is_ignored_by_patterns_matches_dir_prefix_and_suffix_forms() {
        assert!(pattern_matches("dist/app.js", "dist/"));
        assert!(pattern_matches("gen", "gen"));
        assert!(pattern_matches("src/gen", "gen"));
        assert!(pattern_matches("tmpfile.txt", "tmp*"));
        assert!(!pattern_matches("src/app.js", "dist/"));
        assert!(!pattern_matches("src/app.js", "gen"));
        assert!(!pattern_matches("src/app.js", "tmp*"));
    }

    #[test]
    fn scan_excludes_android_res_variants_by_default() {
        // #1047: standard Android res/ subdirs (and their locale/density
        // variants) are excluded by default; real code stays indexed.
        let project = unique_project("android_res");
        touch(&project, "src/main/java/App.java", "class App {}");
        touch(&project, "res/values/strings.xml", "<resources/>");
        touch(&project, "res/values-es/strings.xml", "<resources/>");
        touch(&project, "res/drawable/ic.xml", "<vector/>");
        touch(&project, "res/drawable-hdpi/ic.xml", "<vector/>");
        touch(&project, "res/layout/main.xml", "<LinearLayout/>");
        touch(&project, "res/menu/m.xml", "<menu/>");

        let files = scan_project(&project, &ExtractOptions::default()).expect("scan");
        assert!(
            files.contains(&"src/main/java/App.java".to_string()),
            "first-party Java must be indexed: {files:?}"
        );
        assert!(
            !files.iter().any(|f| f.starts_with("res/")),
            "Android res/ variants must be excluded by default: {files:?}"
        );
        fs::remove_dir_all(&project).ok();
    }

    #[test]
    fn scan_keeps_res_raw_and_src_main_resources_by_default() {
        // #1047 preservation: res/raw/ holds real assets and MyBatis mapper XML
        // under src/main/resources/ carries code symbols — neither is excluded.
        let project = unique_project("android_keep");
        touch(&project, "res/raw/data.xml", "<data/>");
        touch(
            &project,
            "src/main/resources/mapper/UserMapper.xml",
            "<mapper/>",
        );
        touch(&project, "res/values/strings.xml", "<resources/>");

        let files = scan_project(&project, &ExtractOptions::default()).expect("scan");
        assert!(
            files.contains(&"res/raw/data.xml".to_string()),
            "res/raw/ must be kept: {files:?}"
        );
        assert!(
            files.contains(&"src/main/resources/mapper/UserMapper.xml".to_string()),
            "src/main/resources/ MyBatis mappers must be kept: {files:?}"
        );
        assert!(
            !files.contains(&"res/values/strings.xml".to_string()),
            "res/values/ still excluded alongside the kept dirs: {files:?}"
        );
        fs::remove_dir_all(&project).ok();
    }

    #[test]
    fn gitignore_negation_reincludes_default_excluded_res_dir() {
        // #1047: a user can re-include a default-excluded res/ dir with a
        // .gitignore negation (`!res/values/`).
        let project = unique_project("android_negation");
        touch(&project, ".gitignore", "!res/values/\n");
        touch(&project, "res/values/strings.xml", "<resources/>");
        touch(&project, "res/drawable/ic.xml", "<vector/>");

        let files = scan_project(&project, &ExtractOptions::default()).expect("scan");
        assert!(
            files.contains(&"res/values/strings.xml".to_string()),
            "negation must re-include res/values/: {files:?}"
        );
        assert!(
            !files.iter().any(|f| f.starts_with("res/drawable")),
            "un-negated res/drawable stays excluded: {files:?}"
        );
        fs::remove_dir_all(&project).ok();
    }

    #[test]
    fn is_path_ignored_negation_is_last_match_wins() {
        let defaults = vec!["res/values*".to_string()];
        let user = vec!["!res/values/".to_string()];
        assert!(is_path_ignored(
            "res/values/strings.xml",
            &[&defaults],
            None
        ));
        assert!(!is_path_ignored(
            "res/values/strings.xml",
            &[&defaults, &user],
            None
        ));
        assert!(is_path_ignored(
            "res/values-es/strings.xml",
            &[&defaults, &user],
            None
        ));
    }

    #[test]
    fn scan_project_honors_root_gitignore() {
        let project = unique_project("gitignore");
        touch(&project, ".gitignore", "# comment\nvendor/\n\n*.log\n");
        touch(&project, "src/main.rs", "fn main() {}\n");
        touch(&project, "vendor/dep.rs", "pub fn dep() {}\n");
        let files = scan_project(&project, &ExtractOptions::default()).expect("scan");
        assert!(files.contains(&"src/main.rs".to_string()));
        assert!(
            !files.iter().any(|f| f.starts_with("vendor/")),
            ".gitignore vendor/ must be skipped: {files:?}"
        );
        fs::remove_dir_all(&project).ok();
    }

    /// The root `.gitignore` is read with git's own rules, as upstream's `ignore`
    /// matcher reads it: a leading or inner `/` anchors a rule to the root, a
    /// slash-less rule applies at any depth, `*` and `**` glob, and nothing below
    /// an ignored directory can be re-included.
    #[test]
    fn scan_project_reads_the_root_gitignore_with_git_rules() {
        let project = unique_project("gitignore_git_rules");
        touch(
            &project,
            ".gitignore",
            "/artifacts/\n*.gen.ts\ndocs/**/draft/\nscratch/\nlogs/\n!logs/keep.ts\n",
        );
        for file in [
            "src/app.ts",
            "artifacts/a.ts",
            "src/artifacts/b.ts",
            "src/types.gen.ts",
            "docs/guide/draft/c.ts",
            "docs/guide/final/d.ts",
            "src/scratch/e.ts",
            "logs/keep.ts",
        ] {
            touch(&project, file, "export const x = 1;\n");
        }
        let files = scan_project(&project, &ExtractOptions::default()).expect("scan");
        assert_eq!(
            files,
            vec!["docs/guide/final/d.ts", "src/app.ts", "src/artifacts/b.ts"]
        );
        fs::remove_dir_all(&project).ok();
    }

    /// An explicit root `.gitignore` still decides a JVM package named `build`
    /// (upstream #1642): a bare `build/` hides it as git does, while Android's
    /// anchored `/build` and Spring Initializr's `build/` plus
    /// `!**/src/main/**/build/` keep it. Module build output stays pruned.
    #[test]
    fn root_gitignore_still_decides_jvm_packages_named_build() {
        for (gitignore, kept) in [
            ("build/\n", false),
            ("/build\n", true),
            (
                "build/\n!**/src/main/**/build/\n!**/src/test/**/build/\n",
                true,
            ),
        ] {
            let project = unique_project("jvm_build_gitignore");
            touch(&project, ".gitignore", gitignore);
            touch(&project, "src/main/java/App.java", "class App {}");
            touch(
                &project,
                "src/main/java/com/acme/build/Hidden.java",
                "class Hidden {}",
            );
            touch(&project, "app/build/generated/Out.java", "class Out {}");
            let files = scan_project(&project, &ExtractOptions::default()).expect("scan");
            let mut expected = vec!["src/main/java/App.java".to_string()];
            if kept {
                expected.push("src/main/java/com/acme/build/Hidden.java".to_string());
            }
            assert_eq!(files, expected, "{gitignore:?}");
            fs::remove_dir_all(&project).ok();
        }
    }

    #[test]
    fn scan_project_reports_visible_unsupported_extensions_in_the_same_walk() {
        let project = unique_project("unsupported-extensions");
        touch(&project, ".gitignore", "ignored.move\n");
        touch(&project, "a.move", "module a {}\n");
        touch(&project, "nested/b.MOVE", "module b {}\n");
        touch(&project, "tool.pl", "print 1;\n");
        touch(&project, "ignored.move", "module ignored {}\n");
        touch(&project, "README", "no extension\n");

        let report = scan_project_with_stats(&project, &ExtractOptions::default()).expect("scan");
        assert!(report.files.is_empty());
        assert_eq!(
            report.unsupported_by_extension,
            BTreeMap::from([(".move".to_string(), 2), (".pl".to_string(), 1)])
        );
        fs::remove_dir_all(&project).ok();
    }

    #[test]
    fn extract_project_parallel_path_merges_results() {
        let project = unique_project("extract_project_par");
        touch(&project, "a.rs", "pub fn a() {}\n");
        touch(&project, "b.rs", "pub fn b() {}\n");
        let options = ExtractOptions {
            parallel: true,
            ..ExtractOptions::default()
        };
        let merged = extract_project(&project, &options).expect("extract");
        fs::remove_dir_all(&project).ok();
        assert!(
            merged.nodes.iter().any(|n| n.name == "a")
                && merged.nodes.iter().any(|n| n.name == "b"),
            "parallel extract merges both files: {:?}",
            merged.nodes.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn looks_like_cpp_export_macro_class() {
        assert!(looks_like_cpp(
            "class ENGINE_API UFoo : public UObject { };"
        ));
    }

    #[test]
    fn looks_like_cpp_plain_c_false() {
        assert!(!looks_like_cpp("struct Foo { int x; };\nvoid f(void);\n"));
    }

    #[test]
    fn looks_like_cpp_plain_base_clause_forms() {
        for source in [
            "struct Base {};\nstruct Derived : Base {};\n",
            "struct Derived : public Base {};\n",
            "struct Derived : ns::Base {};\n",
            "struct Derived : Base<int, Foo<T>> {};\n",
            "struct Derived final : Base {};\n",
            "class Derived : public A, private B\n{\n};\n",
            "struct Derived : virtual protected Base {};\n",
        ] {
            assert!(looks_like_cpp(source), "expected C++: {source}");
        }
    }

    #[test]
    fn looks_like_cpp_base_clause_scans_past_prefix() {
        let preamble =
            "#ifndef BIG_H\n#define BIG_H\n".to_string() + &"#define VALUE_0 0\n".repeat(700);
        assert!(preamble.len() > 8192);
        assert!(looks_like_cpp(&format!(
            "{preamble}struct Base {{}};\nstruct Derived : Base {{}};\n#endif\n"
        )));
    }

    #[test]
    fn looks_like_cpp_masks_c_false_positives() {
        for source in [
            "struct S { unsigned int a : 3; unsigned int b : 5; };\n",
            "static inline int sz(int x) { return x ? sizeof(struct foo) : 0; }\n",
            "static void g(void) {\nstruct_end:\n  return;\n}\n",
            "/* struct Fake : Base { }; */\nstruct Real { int x; };\n",
            "const char *s = \"struct Fake : Base {\";\nstruct Real { int x; };\n",
            "const char c = ':';\nstruct Real { int x; };\n",
            "#define FAKE struct Fake : Base {\nstruct Real { int x; };\n",
        ] {
            assert!(!looks_like_cpp(source), "expected C: {source}");
        }
    }

    #[test]
    fn looks_like_objc_interface() {
        assert!(looks_like_objc("@interface Foo\n@end\n"));
    }

    #[test]
    fn dot_h_reclassified_to_cpp_by_content() {
        let result = extract_source(
            "F.h",
            "class ENGINE_API UFoo : public UObject { GENERATED_BODY() };",
            None,
        );
        assert!(
            result.nodes.iter().any(|n| n.name == "UFoo"),
            "a UE .h should be reclassified to C++ and yield a UFoo class node, got: {:?}",
            result
                .nodes
                .iter()
                .map(|n| (n.kind, n.name.as_str()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn dot_h_plain_derived_struct_is_cpp_without_phantom_function() {
        let result = extract_source(
            "min.h",
            "struct Base {};\nstruct Derived : Base {};\n",
            None,
        );
        let derived = result
            .nodes
            .iter()
            .find(|node| node.name == "Derived")
            .expect("Derived node");
        assert_eq!(derived.kind, codegraph_core::types::NodeKind::Struct);
        assert_eq!(derived.language, Language::Cpp);
        assert!(!result.nodes.iter().any(|node| {
            node.name == "Base" && node.kind == codegraph_core::types::NodeKind::Function
        }));
    }

    #[test]
    fn dot_h_plain_c_stays_c() {
        let result = extract_source("plain.h", "int add(int a, int b);\n", None);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    }

    fn cpp_raw_string(delimiter: &str) -> String {
        format!(
            "const char* kTemplate = R\"{delimiter}(\nstruct Ignored {{ int v; }};\n){delimiter}\";\n\nint after_the_raw_string(int x) {{\n  return x + 1;\n}}\n"
        )
    }

    #[test]
    fn parse_collapse_warning_is_honest_and_narrow() {
        let collapsed = extract_source("min.cpp", &cpp_raw_string("FILE_TEMPLATE_V1"), None);
        assert_eq!(
            collapsed
                .nodes
                .iter()
                .filter(|node| node.kind != codegraph_core::types::NodeKind::File)
                .count(),
            0
        );
        assert_eq!(collapsed.errors.len(), 1);
        assert!(is_extraction_warning(&collapsed.errors[0]));
        assert!(collapsed.errors[0].contains(PARSE_COLLAPSE_WARNING));

        let healthy = extract_source("min.cpp", &cpp_raw_string("FILE_TEMPLATE_V"), None);
        assert!(
            healthy
                .nodes
                .iter()
                .any(|node| node.name == "after_the_raw_string")
        );
        assert!(healthy.errors.is_empty(), "errors={:?}", healthy.errors);

        for (path, source) in [
            ("empty.cpp", ""),
            ("includes.cpp", "#include <stdio.h>\n#include <stdlib.h>\n"),
            (
                "survivor.cpp",
                "int before() { return 0; }\nconst char* x = R\"FILE_TEMPLATE_V1(\nstruct Ignored {};\n)FILE_TEMPLATE_V1\";\n",
            ),
        ] {
            let result = extract_source(path, source, None);
            assert!(
                result.errors.is_empty(),
                "a healthy/partially surviving file must not be called a collapse: {path}: {:?}",
                result.errors
            );
        }
    }

    #[test]
    fn observed_extraction_reports_stages_without_changing_output() {
        let source = "export function answer(): number { return 42; }\n";
        let mut ordinary = extract_source_with(
            "src/answer.ts",
            source,
            None,
            &ExtensionOverrides::default(),
        );
        let mut stages = Vec::new();
        let mut observed = extract_source_with_observer(
            "src/answer.ts",
            source,
            None,
            &ExtensionOverrides::default(),
            |stage| stages.push(stage),
        );
        ordinary.duration_ms = 0;
        observed.duration_ms = 0;
        assert_eq!(observed, ordinary);
        assert_eq!(
            stages,
            vec![
                ExtractionStage::DetectLanguage,
                ExtractionStage::Prepare,
                ExtractionStage::Embedded,
                ExtractionStage::Prepare,
                ExtractionStage::TreeSitterParse,
                ExtractionStage::Walk,
            ]
        );
    }
}
