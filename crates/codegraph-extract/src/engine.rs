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
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tree_sitter::Parser;

use crate::lang::spec_for_language;
use crate::walker::TreeSitterWalker;

#[derive(Debug, Clone)]
pub struct ExtractOptions {
    pub max_file_size: u64,
    pub ignore_dirs: Vec<String>,
    pub exclude: Vec<String>,
    pub parallel: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        let indexing = IndexingConfig::default();
        Self {
            max_file_size: indexing.max_file_size,
            ignore_dirs: indexing.ignore_dirs,
            exclude: indexing.exclude,
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
        "py" | "pyw" => Language::Python,
        "go" => Language::Go,
        "rs" => Language::Rust,
        "java" => Language::Java,
        "c" | "h" => Language::C,
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => Language::Cpp,
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

pub fn extract_source(
    file_path: &str,
    source: &str,
    language: Option<Language>,
) -> ExtractionResult {
    let start = Instant::now();
    let language = language.unwrap_or_else(|| detect_language(file_path));
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
    let parsed_source = spec.pre_parse(source);
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
    scan_dir(
        root,
        root,
        &ignored_dirs,
        &gitignore,
        &options.exclude,
        &mut files,
    )?;
    files.sort();
    Ok(files)
}

fn scan_dir(
    root: &Path,
    dir: &Path,
    ignored_dirs: &HashSet<&str>,
    gitignore: &[String],
    exclude: &[String],
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
        if is_ignored_by_patterns(&relative, gitignore)
            || is_ignored_by_patterns(&relative, exclude)
        {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            scan_dir(root, &path, ignored_dirs, gitignore, exclude, files)?;
        } else if file_type.is_file() && is_extractable_source_path(&relative) {
            files.push(relative);
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

fn is_ignored_by_patterns(relative: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| {
        if let Some(dir) = pattern.strip_suffix('/') {
            relative == dir || relative.starts_with(&format!("{dir}/"))
        } else if let Some(prefix) = pattern.strip_suffix('*') {
            relative.starts_with(prefix)
        } else {
            relative == pattern || relative.ends_with(&format!("/{pattern}"))
        }
    })
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
        let patterns = vec!["dist/".to_string(), "gen".to_string(), "tmp*".to_string()];
        assert!(is_ignored_by_patterns("dist/app.js", &patterns));
        assert!(is_ignored_by_patterns("gen", &patterns));
        assert!(is_ignored_by_patterns("src/gen", &patterns));
        assert!(is_ignored_by_patterns("tmpfile.txt", &patterns));
        assert!(!is_ignored_by_patterns("src/app.js", &patterns));
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
}
