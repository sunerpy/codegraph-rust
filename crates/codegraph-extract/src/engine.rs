//! File and directory extraction pipeline.
//!
//! Source map: `upstream extraction/index.ts:90-101` maps to content
//! hashing and size skips; `:402-570` maps to directory scanning; and
//! `tree-sitter.ts:4350-4425` maps to source dispatch.

use anyhow::{Context, Result};
use codegraph_core::config::IndexingConfig;
use codegraph_core::node_id::hash_content;
use codegraph_core::types::{ExtractionResult, Language};
use rayon::prelude::*;
use regex::Regex;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;
use tree_sitter::Parser;

use crate::lang::spec_for_language;
use crate::walker::TreeSitterWalker;

#[derive(Debug, Clone)]
pub struct ExtractOptions {
    pub max_file_size: u64,
    pub ignore_dirs: Vec<String>,
    pub ignore_paths: Vec<String>,
    pub exclude: Vec<String>,
    pub include: Vec<String>,
    pub parallel: bool,
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
        "yml" | "yaml" => Language::Yaml,
        "twig" => Language::Twig,
        "xml" => Language::Xml,
        "properties" => Language::Properties,
        _ => return None,
    };
    Some(language)
}

pub fn detect_language(file_path: impl AsRef<Path>) -> Language {
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
    // Golden safety: the override is consulted ONLY for extensions unclaimed by
    // both the built-in match and the embedded pre-pass (both already checked
    // above). Absent codegraph.json => no override => exact current behavior.
    if let Some(language) = crate::ext_config::override_language_for(path, &ext) {
        return language;
    }
    Language::Unknown
}

/// A `.h` file maps to `Language::C` by extension, but may hold C++ or
/// Objective-C. Match the upstream 8 KB-prefix content sniff (`grammars.ts`
/// `looksLikeCpp`). The `class MACRO Name : Base` alternative recognizes an
/// export-macro-annotated class whose only C++ signal is the macro — the
/// two-token `<KW> <MACRO> <Name>` shape never occurs in valid C, so genuine C
/// headers stay C. Lookahead-free (the `regex` crate has none).
fn looks_like_cpp(source: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"\bnamespace\b|\bclass\s+\w+\s*[:{]|\b(?:class|struct)\s+[A-Z][A-Z0-9_]+\s+\w+\s*(?:final\s*)?[:{]|\btemplate\s*<|\b(?:public|private|protected)\s*:|\bvirtual\b|\busing\s+(?:namespace\b|\w+\s*=)",
        )
        .expect("looks-like-cpp regex")
    });
    re.is_match(prefix_8k(source))
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

pub fn extract_source(
    file_path: &str,
    source: &str,
    language: Option<Language>,
) -> ExtractionResult {
    let start = Instant::now();
    let mut language = language.unwrap_or_else(|| detect_language(file_path));
    if language == Language::C
        && Path::new(file_path)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("h"))
    {
        if looks_like_cpp(source) {
            language = Language::Cpp;
        } else if looks_like_objc(source) {
            language = Language::ObjC;
        }
    }
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
    let ts_language = spec.tree_sitter_language();
    if let Err(error) = parser.set_language(&ts_language) {
        return ExtractionResult {
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_references: Vec::new(),
            errors: vec![format!("Failed to set parser language: {error}")],
            duration_ms: start.elapsed().as_millis() as i64,
        };
    }
    let parsed_source = spec.pre_parse(source, file_path);
    let Some(tree) = parser.parse(&parsed_source, None) else {
        return ExtractionResult {
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_references: Vec::new(),
            errors: vec!["Parser returned null tree".to_string()],
            duration_ms: start.elapsed().as_millis() as i64,
        };
    };
    TreeSitterWalker::new(file_path, &parsed_source, spec, tree.root_node())
        .extract(start.elapsed().as_millis() as i64)
}

pub fn extract_file(
    root: impl AsRef<Path>,
    relative_path: impl AsRef<Path>,
) -> Result<ExtractionResult> {
    let root = root.as_ref();
    let relative_path = normalize_path(relative_path.as_ref());
    let full_path = root.join(&relative_path);
    let metadata = fs::metadata(&full_path)
        .with_context(|| format!("stat source file {}", full_path.display()))?;
    let options = ExtractOptions::default();
    if metadata.len() > options.max_file_size {
        return Ok(size_skip_result(
            &relative_path,
            metadata.len(),
            options.max_file_size,
        ));
    }
    let source = fs::read_to_string(&full_path)
        .with_context(|| format!("read source file {}", full_path.display()))?;
    let _content_hash = hash_content(&source);
    Ok(extract_source(&relative_path, &source, None))
}

pub fn extract_project(
    root: impl AsRef<Path>,
    options: &ExtractOptions,
) -> Result<ExtractionResult> {
    let root = root.as_ref();
    let files = scan_project(root, options)?;
    let parse = |relative: &String| -> Result<ExtractionResult> {
        let full = root.join(relative);
        let metadata =
            fs::metadata(&full).with_context(|| format!("stat source file {}", full.display()))?;
        if metadata.len() > options.max_file_size {
            return Ok(size_skip_result(
                relative,
                metadata.len(),
                options.max_file_size,
            ));
        }
        let source = fs::read_to_string(&full)
            .with_context(|| format!("read source file {}", full.display()))?;
        let _content_hash = hash_content(&source);
        Ok(extract_source(relative, &source, None))
    };

    let mut results = if options.parallel {
        files.par_iter().map(parse).collect::<Result<Vec<_>>>()?
    } else {
        files.iter().map(parse).collect::<Result<Vec<_>>>()?
    };
    merge_results(&mut results)
}

pub fn scan_project(root: &Path, options: &ExtractOptions) -> Result<Vec<String>> {
    let mut files = Vec::new();
    let ignored_dirs = options
        .ignore_dirs
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let gitignore = read_root_gitignore(root);
    // Evaluated in order (default paths → config exclude → .gitignore), so a
    // later `!pattern` negation re-includes a path an earlier set excluded.
    let pattern_sets: Vec<&[String]> = vec![&options.ignore_paths, &options.exclude, &gitignore];
    let include = IncludeSet::new(&options.include, &options.exclude);
    scan_dir(
        root,
        root,
        &ignored_dirs,
        &pattern_sets,
        &include,
        &mut files,
    )?;
    files.sort();
    Ok(files)
}

fn scan_dir(
    root: &Path,
    dir: &Path,
    ignored_dirs: &HashSet<&str>,
    pattern_sets: &[&[String]],
    include: &IncludeSet,
    files: &mut Vec<String>,
) -> Result<()> {
    let entries = fs::read_dir(dir).with_context(|| format!("read dir {}", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == ".git" || name == ".codegraph" || ignored_dirs.contains(name.as_ref()) {
            continue;
        }
        let relative = normalize_path(path.strip_prefix(root).unwrap_or(&path));
        let ignored = is_path_ignored(&relative, pattern_sets);
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            // A model-ignored dir is normally pruned before descent, so a FILE
            // include under a gitignored ancestor would never be reached.
            // Descend anyway when this dir is an ancestor of (or matches) an
            // include pattern; files inside are still pruned unless force-included.
            if ignored && !include.wants_descend(&relative) {
                continue;
            }
            scan_dir(root, &path, ignored_dirs, pattern_sets, include, files)?;
        } else if file_type.is_file() && is_extractable_source_path(&relative) {
            // Post-model include decision: a model-ignored file is force-included
            // iff it matches `include` and is NOT overridden by an explicit
            // `exclude` (checked inside `IncludeSet::forces`). Built-in dir skips
            // are already handled structurally above, so include can never
            // resurface node_modules/dist/.git/etc.
            if !ignored || include.forces(&relative) {
                files.push(relative);
            }
        }
    }
    Ok(())
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

fn size_skip_result(file_path: &str, size: u64, max: u64) -> ExtractionResult {
    ExtractionResult {
        nodes: Vec::new(),
        edges: Vec::new(),
        unresolved_references: Vec::new(),
        errors: vec![format!(
            "File exceeds max size ({size} > {max}): {file_path}"
        )],
        duration_ms: 0,
    }
}

fn read_root_gitignore(root: &Path) -> Vec<String> {
    fs::read_to_string(root.join(".gitignore"))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(ToOwned::to_owned)
        .collect()
}

/// Evaluate ordered `.gitignore`-style pattern sets with last-match-wins
/// negation: a `!pattern` line un-ignores a path an earlier pattern excluded.
/// Sets are scanned in order, patterns within a set in order, and the final
/// matching pattern decides — so a later `!res/values/` re-includes what a
/// default `res/values*` excluded.
fn is_path_ignored(relative: &str, pattern_sets: &[&[String]]) -> bool {
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
    ignored
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

fn is_extractable_source_path(relative: &str) -> bool {
    let language = detect_language(relative);
    language != Language::Unknown
        && (crate::lang::spec_for_language(language).is_some()
            || crate::embedded::is_embedded_source_path(relative)
            || is_file_level_only_language(language))
}

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
    fn metal_cu_cuh_map_to_cpp() {
        assert_eq!(detect_language("s.metal"), Language::Cpp);
        assert_eq!(detect_language("k.cu"), Language::Cpp);
        assert_eq!(detect_language("k.cuh"), Language::Cpp);
        assert_eq!(Language::ALL.len(), 39);
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
        assert!(is_path_ignored("res/values/strings.xml", &[&defaults]));
        assert!(!is_path_ignored(
            "res/values/strings.xml",
            &[&defaults, &user]
        ));
        assert!(is_path_ignored(
            "res/values-es/strings.xml",
            &[&defaults, &user]
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
    fn dot_h_plain_c_stays_c() {
        let result = extract_source("plain.h", "int add(int a, int b);\n", None);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    }
}
