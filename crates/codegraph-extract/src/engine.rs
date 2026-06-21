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
    pub parallel: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        let indexing = IndexingConfig::default();
        Self {
            max_file_size: indexing.max_file_size,
            ignore_dirs: indexing.ignore_dirs,
            parallel: true,
        }
    }
}

pub fn detect_language(file_path: impl AsRef<Path>) -> Language {
    let path = file_path.as_ref();
    let normalized = normalize_path(path);
    if let Some(language) = crate::embedded::detect_embedded_language(&normalized) {
        return language;
    }
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return Language::Unknown;
    };
    match ext.to_ascii_lowercase().as_str() {
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
        "luau" => Language::Luau,
        "m" | "mm" => Language::ObjC,
        "r" => Language::R,
        "yml" | "yaml" => Language::Yaml,
        "twig" => Language::Twig,
        "xml" => Language::Xml,
        "properties" => Language::Properties,
        _ => Language::Unknown,
    }
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
    scan_dir(root, root, &ignored_dirs, &gitignore, &mut files)?;
    files.sort();
    Ok(files)
}

fn scan_dir(
    root: &Path,
    dir: &Path,
    ignored_dirs: &HashSet<&str>,
    gitignore: &[String],
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
        if is_ignored_by_patterns(&relative, gitignore) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            scan_dir(root, &path, ignored_dirs, gitignore, files)?;
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
        Language::Yaml | Language::Twig | Language::Properties
    )
}

fn normalize_path(path: impl AsRef<Path>) -> String {
    path.as_ref()
        .components()
        .collect::<PathBuf>()
        .to_string_lossy()
        .replace('\\', "/")
}
