//! Import resolver.
//!
//! Ports `upstream resolution/import-resolver.ts`. Resolves import
//! paths to files/symbols, handles path aliases ([`crate::path_aliases`]) and
//! workspace packages ([`crate::workspace_packages`]), extracts per-file import
//! mappings + re-exports, and turns import-shaped references into edges. Every
//! ported branch cites its upstream source range.

use crate::path_aliases::apply_aliases;
use crate::pathutil;
use crate::types::{ImportMapping, ReExport, RefView, ResolutionContext, ResolvedBy, ResolvedRef};
use crate::workspace_packages::resolve_workspace_import;
use codegraph_core::types::{Language, Node, NodeKind};
use regex::Regex;
use std::collections::BTreeSet;
use std::sync::OnceLock;

/// Extension resolution order by language (`EXTENSION_RESOLUTION`,
/// `import-resolver.ts:17-37`).
fn extension_resolution(language: Language) -> &'static [&'static str] {
    match language {
        Language::TypeScript => &[
            ".ts",
            ".tsx",
            ".d.ts",
            ".js",
            ".jsx",
            "/index.ts",
            "/index.tsx",
            "/index.js",
        ],
        Language::JavaScript => &[".js", ".jsx", ".mjs", ".cjs", "/index.js", "/index.jsx"],
        Language::Tsx => &[
            ".tsx",
            ".ts",
            ".d.ts",
            ".js",
            ".jsx",
            "/index.tsx",
            "/index.ts",
            "/index.js",
        ],
        Language::Jsx => &[".jsx", ".js", "/index.jsx", "/index.js"],
        Language::Svelte => &[
            ".ts",
            ".js",
            ".svelte",
            ".tsx",
            ".jsx",
            "/index.ts",
            "/index.js",
            "/index.svelte",
        ],
        Language::Vue => &[
            ".ts",
            ".js",
            ".vue",
            ".tsx",
            ".jsx",
            "/index.ts",
            "/index.js",
            "/index.vue",
        ],
        Language::Python => &[".py", "/__init__.py"],
        Language::Go => &[".go"],
        Language::Rust => &[".rs", "/mod.rs"],
        Language::Java => &[".java"],
        Language::C => &[".h", ".c"],
        Language::Cpp => &[".h", ".hpp", ".hxx", ".cpp", ".cc", ".cxx"],
        Language::CSharp => &[".cs"],
        Language::Php => &[".php"],
        Language::Ruby => &[".rb"],
        Language::ObjC => &[".h", ".m", ".mm"],
        _ => &[],
    }
}

/// Resolve an import path to an actual file (`resolveImportPath`,
/// `import-resolver.ts:42-76`).
pub fn resolve_import_path(
    import_path: &str,
    from_file: &str,
    language: Language,
    context: &dyn ResolutionContext,
) -> Option<String> {
    if is_external_import(import_path, language, context) {
        return None;
    }

    let project_root = context.get_project_root().to_string();
    let from_dir = pathutil::dirname(&pathutil::resolve(&project_root, from_file));

    // Relative imports (import-resolver.ts:59-62).
    if import_path.starts_with('.') {
        return resolve_relative_import(import_path, &from_dir, &project_root, language, context);
    }

    // Aliased/absolute imports (import-resolver.ts:64-66).
    if let Some(aliased) = resolve_aliased_import(import_path, &project_root, language, context) {
        return Some(aliased);
    }

    // C/C++ include directory search (import-resolver.ts:71-73).
    if matches!(language, Language::C | Language::Cpp) {
        return resolve_cpp_include_path(import_path, language, context);
    }

    None
}

/// C/C++ standard library header names (`C_CPP_STDLIB_HEADERS`,
/// `import-resolver.ts:82-113`).
fn c_cpp_stdlib_headers() -> &'static BTreeSet<&'static str> {
    static SET: OnceLock<BTreeSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        [
            "assert.h",
            "complex.h",
            "ctype.h",
            "errno.h",
            "fenv.h",
            "float.h",
            "inttypes.h",
            "iso646.h",
            "limits.h",
            "locale.h",
            "math.h",
            "setjmp.h",
            "signal.h",
            "stdalign.h",
            "stdarg.h",
            "stdatomic.h",
            "stdbool.h",
            "stddef.h",
            "stdint.h",
            "stdio.h",
            "stdlib.h",
            "stdnoreturn.h",
            "string.h",
            "tgmath.h",
            "threads.h",
            "time.h",
            "uchar.h",
            "wchar.h",
            "wctype.h",
            "cassert",
            "ccomplex",
            "cctype",
            "cerrno",
            "cfenv",
            "cfloat",
            "cinttypes",
            "ciso646",
            "climits",
            "clocale",
            "cmath",
            "csetjmp",
            "csignal",
            "cstdalign",
            "cstdarg",
            "cstdbool",
            "cstddef",
            "cstdint",
            "cstdio",
            "cstdlib",
            "cstring",
            "ctgmath",
            "ctime",
            "cuchar",
            "cwchar",
            "cwctype",
            "algorithm",
            "any",
            "array",
            "atomic",
            "barrier",
            "bit",
            "bitset",
            "charconv",
            "chrono",
            "codecvt",
            "compare",
            "complex",
            "concepts",
            "condition_variable",
            "coroutine",
            "deque",
            "exception",
            "execution",
            "expected",
            "filesystem",
            "format",
            "forward_list",
            "fstream",
            "functional",
            "future",
            "generator",
            "initializer_list",
            "iomanip",
            "ios",
            "iosfwd",
            "iostream",
            "istream",
            "iterator",
            "latch",
            "limits",
            "list",
            "locale",
            "map",
            "mdspan",
            "memory",
            "memory_resource",
            "mutex",
            "new",
            "numbers",
            "numeric",
            "optional",
            "ostream",
            "print",
            "queue",
            "random",
            "ranges",
            "ratio",
            "regex",
            "scoped_allocator",
            "semaphore",
            "set",
            "shared_mutex",
            "source_location",
            "span",
            "spanstream",
            "sstream",
            "stack",
            "stacktrace",
            "stdexcept",
            "stdfloat",
            "stop_token",
            "streambuf",
            "string",
            "string_view",
            "strstream",
            "syncstream",
            "system_error",
            "thread",
            "tuple",
            "type_traits",
            "typeindex",
            "typeinfo",
            "unordered_map",
            "unordered_set",
            "utility",
            "valarray",
            "variant",
            "vector",
            "version",
        ]
        .into_iter()
        .collect()
    })
}

/// Check if an import is external (`isExternalImport`, `import-resolver.ts:123-202`).
fn is_external_import(
    import_path: &str,
    language: Language,
    context: &dyn ResolutionContext,
) -> bool {
    if import_path.starts_with('.') {
        return false;
    }

    // Workspace-member imports are local (import-resolver.ts:137-140).
    if let Some(ws) = context.get_workspace_packages() {
        if resolve_workspace_import(import_path, &ws).is_some() {
            return false;
        }
    }

    if matches!(
        language,
        Language::TypeScript | Language::JavaScript | Language::Tsx | Language::Jsx
    ) {
        const NODE_BUILTINS: [&str; 12] = [
            "fs",
            "path",
            "os",
            "crypto",
            "http",
            "https",
            "url",
            "util",
            "events",
            "stream",
            "child_process",
            "buffer",
        ];
        if NODE_BUILTINS.contains(&import_path) {
            return true;
        }
        // Project-defined alias prefix → local (import-resolver.ts:149-154).
        if let Some(aliases) = context.get_project_aliases() {
            for pat in &aliases.patterns {
                if import_path.starts_with(&pat.prefix) {
                    return false;
                }
            }
        }
        // Bare specifiers that don't start with known prefixes (import-resolver.ts:155-159).
        if !import_path.starts_with("@/")
            && !import_path.starts_with("~/")
            && !import_path.starts_with("src/")
        {
            return true;
        }
    }

    if language == Language::Python {
        const STD_LIBS: [&str; 10] = [
            "os",
            "sys",
            "json",
            "re",
            "math",
            "datetime",
            "collections",
            "typing",
            "pathlib",
            "logging",
        ];
        let head = import_path.split('.').next().unwrap_or(import_path);
        if STD_LIBS.contains(&head) {
            return true;
        }
    }

    if language == Language::Go {
        if import_path.starts_with('.') {
            return false;
        }
        if let Some(module) = context.get_go_module() {
            if import_path == module.module_path
                || import_path.starts_with(&format!("{}/", module.module_path))
            {
                return false;
            }
        }
        if import_path.contains("/internal/") {
            return false;
        }
        return true;
    }

    if matches!(language, Language::C | Language::Cpp) {
        if c_cpp_stdlib_headers().contains(import_path) {
            return true;
        }
        let without_ext = import_path.strip_suffix(".h").unwrap_or(import_path);
        if c_cpp_stdlib_headers().contains(without_ext) {
            return true;
        }
    }

    false
}

/// Resolve a relative import (`resolveRelativeImport`, `import-resolver.ts:207-252`).
fn resolve_relative_import(
    import_path: &str,
    from_dir: &str,
    project_root: &str,
    language: Language,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let extensions = extension_resolution(language);

    // Python dotted-relative imports (import-resolver.ts:221-232).
    if language == Language::Python && import_path.starts_with('.') {
        let dots = import_path.len() - import_path.trim_start_matches('.').len();
        let up = "../".repeat(dots.saturating_sub(1));
        let rest = import_path[dots..].replace('.', "/");
        let py_base = pathutil::resolve(from_dir, &format!("{up}{rest}"));
        let py_rel = pathutil::relative(project_root, &py_base);
        for ext in extensions {
            let candidate = format!("{py_rel}{ext}");
            if context.file_exists(&candidate) {
                return Some(candidate);
            }
        }
        if !py_rel.is_empty() && context.file_exists(&py_rel) {
            return Some(py_rel);
        }
        return None;
    }

    let base_path = pathutil::resolve(from_dir, import_path);
    let relative_path = pathutil::relative(project_root, &base_path);

    for ext in extensions {
        let candidate = format!("{relative_path}{ext}");
        if context.file_exists(&candidate) {
            return Some(candidate);
        }
    }

    if context.file_exists(&relative_path) {
        return Some(relative_path);
    }

    None
}

/// Resolve an aliased/absolute import (`resolveAliasedImport`,
/// `import-resolver.ts:265-322`).
fn resolve_aliased_import(
    import_path: &str,
    project_root: &str,
    language: Language,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let extensions = extension_resolution(language);
    let try_with_ext = |base_path: &str| -> Option<String> {
        for ext in extensions {
            let candidate = format!("{base_path}{ext}");
            if context.file_exists(&candidate) {
                return Some(candidate);
            }
        }
        if context.file_exists(base_path) {
            return Some(base_path.to_string());
        }
        None
    };

    // 1. Project tsconfig/jsconfig paths (import-resolver.ts:282-289).
    if let Some(alias_map) = context.get_project_aliases() {
        for candidate in apply_aliases(import_path, &alias_map, project_root) {
            if let Some(hit) = try_with_ext(&candidate) {
                return Some(hit);
            }
        }
    }

    // 1.5 Workspace packages (import-resolver.ts:294-301).
    if let Some(ws) = context.get_workspace_packages() {
        if let Some(base) = resolve_workspace_import(import_path, &ws) {
            if let Some(hit) = try_with_ext(&base) {
                return Some(hit);
            }
        }
    }

    // 2. Hard-coded fallback list (import-resolver.ts:305-318).
    const FALLBACK_ALIASES: [(&str, &str); 6] = [
        ("@/", "src/"),
        ("~/", "src/"),
        ("@src/", "src/"),
        ("src/", "src/"),
        ("@app/", "app/"),
        ("app/", "app/"),
    ];
    for (alias, replacement) in FALLBACK_ALIASES {
        if let Some(rest) = import_path.strip_prefix(alias) {
            if let Some(hit) = try_with_ext(&format!("{replacement}{rest}")) {
                return Some(hit);
            }
        }
    }

    // 3. Direct path (import-resolver.ts:321).
    try_with_ext(import_path)
}

/// Resolve a C/C++ include path by searching include directories
/// (`resolveCppIncludePath`, `import-resolver.ts:510-530`).
fn resolve_cpp_include_path(
    import_path: &str,
    language: Language,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let include_dirs = context.get_cpp_include_dirs();
    let extensions = extension_resolution(language);

    for dir in &include_dirs {
        let normalized_dir = dir.replace('\\', "/");
        for ext in extensions {
            let candidate = format!("{normalized_dir}/{import_path}{ext}");
            if context.file_exists(&candidate) {
                return Some(candidate);
            }
        }
        let candidate = format!("{normalized_dir}/{import_path}");
        if context.file_exists(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Is this reference a PHP include/require PATH (vs a namespace `use` symbol)?
/// (`isPhpIncludePathRef`, `import-resolver.ts:541-547`).
pub fn is_php_include_path_ref(reference: &RefView) -> bool {
    reference.language == Language::Php
        && reference.reference_kind == codegraph_core::types::EdgeKind::Imports
        && (reference.reference_name.contains('/') || reference.reference_name.contains('.'))
}

/// Resolve a PHP include/require path to a project-relative file
/// (`resolvePhpIncludePath`, `import-resolver.ts:556-571`).
fn resolve_php_include_path(
    include_path: &str,
    from_file: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let project_root = context.get_project_root().to_string();
    let from_dir = pathutil::dirname(&pathutil::resolve(&project_root, from_file));
    let base_path = pathutil::resolve(&from_dir, include_path);
    let relative_path = pathutil::relative(&project_root, &base_path);
    if context.file_exists(&relative_path) {
        return Some(relative_path);
    }
    for ext in extension_resolution(Language::Php) {
        let candidate = format!("{relative_path}{ext}");
        if context.file_exists(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn js_import_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"import\s+(?:(\w+)\s*,?\s*)?(?:\{([^}]+)\})?\s*(?:(\*)\s+as\s+(\w+))?\s*from\s*['"]([^'"]+)['"]"#,
        )
        .expect("valid JS import regex")
    })
}

fn js_require_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?:const|let|var)\s+(?:(\w+)|\{([^}]+)\})\s*=\s*require\(['"]([^'"]+)['"]\)"#)
            .expect("valid JS require regex")
    })
}

/// Extract import mappings from a file (`extractImportMappings`,
/// `import-resolver.ts:576-608`).
pub fn extract_import_mappings(content: &str, language: Language) -> Vec<ImportMapping> {
    match language {
        Language::TypeScript
        | Language::JavaScript
        | Language::Tsx
        | Language::Jsx
        | Language::Svelte
        | Language::Vue => extract_js_imports(content),
        Language::Python => extract_python_imports(content),
        Language::Go => extract_go_imports(content),
        Language::Java | Language::Kotlin => extract_java_imports(content),
        Language::Php => extract_php_imports(content),
        Language::C | Language::Cpp => extract_cpp_imports(content),
        _ => Vec::new(),
    }
}

/// `extractJSImports` (`import-resolver.ts:613-712`).
fn extract_js_imports(content: &str) -> Vec<ImportMapping> {
    let mut mappings = Vec::new();

    for caps in js_import_regex().captures_iter(content) {
        let default_import = caps.get(1).map(|m| m.as_str());
        let named_imports = caps.get(2).map(|m| m.as_str());
        let star = caps.get(3).is_some();
        let namespace_alias = caps.get(4).map(|m| m.as_str());
        let source = caps.get(5).map(|m| m.as_str()).unwrap_or_default();

        if let Some(default_import) = default_import {
            mappings.push(ImportMapping {
                local_name: default_import.to_string(),
                exported_name: "default".to_string(),
                source: source.to_string(),
                is_default: true,
                is_namespace: false,
            });
        }

        if let Some(named) = named_imports {
            for raw in named.split(',') {
                let name = raw.trim();
                if let Some((orig, alias)) = parse_as_alias(name) {
                    mappings.push(ImportMapping {
                        local_name: alias,
                        exported_name: orig,
                        source: source.to_string(),
                        is_default: false,
                        is_namespace: false,
                    });
                } else if !name.is_empty() {
                    mappings.push(ImportMapping {
                        local_name: name.to_string(),
                        exported_name: name.to_string(),
                        source: source.to_string(),
                        is_default: false,
                        is_namespace: false,
                    });
                }
            }
        }

        if star {
            if let Some(alias) = namespace_alias {
                mappings.push(ImportMapping {
                    local_name: alias.to_string(),
                    exported_name: "*".to_string(),
                    source: source.to_string(),
                    is_default: false,
                    is_namespace: true,
                });
            }
        }
    }

    for caps in js_require_regex().captures_iter(content) {
        let default_name = caps.get(1).map(|m| m.as_str());
        let destructured = caps.get(2).map(|m| m.as_str());
        let source = caps.get(3).map(|m| m.as_str()).unwrap_or_default();

        if let Some(default_name) = default_name {
            mappings.push(ImportMapping {
                local_name: default_name.to_string(),
                exported_name: "default".to_string(),
                source: source.to_string(),
                is_default: true,
                is_namespace: false,
            });
        }

        if let Some(destructured) = destructured {
            for raw in destructured.split(',') {
                let name = raw.trim();
                if let Some((orig, alias)) = parse_colon_alias(name) {
                    mappings.push(ImportMapping {
                        local_name: alias,
                        exported_name: orig,
                        source: source.to_string(),
                        is_default: false,
                        is_namespace: false,
                    });
                } else if !name.is_empty() {
                    mappings.push(ImportMapping {
                        local_name: name.to_string(),
                        exported_name: name.to_string(),
                        source: source.to_string(),
                        is_default: false,
                        is_namespace: false,
                    });
                }
            }
        }
    }

    mappings
}

/// `extractPythonImports` (`import-resolver.ts:717-765`).
fn extract_python_imports(content: &str) -> Vec<ImportMapping> {
    static FROM_RE: OnceLock<Regex> = OnceLock::new();
    static IMP_RE: OnceLock<Regex> = OnceLock::new();
    let from_re = FROM_RE
        .get_or_init(|| Regex::new(r"from\s+([\w.]+)\s+import\s+([^#\n]+)").expect("py from"));
    let imp_re = IMP_RE.get_or_init(|| {
        Regex::new(r"(?m)^import\s+([\w.]+)(?:\s+as\s+(\w+))?").expect("py import")
    });

    let mut mappings = Vec::new();

    for caps in from_re.captures_iter(content) {
        let source = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
        let imports = caps.get(2).map(|m| m.as_str()).unwrap_or_default();
        for raw in imports.split(',') {
            let name = raw.trim();
            if let Some((orig, alias)) = parse_as_alias(name) {
                mappings.push(ImportMapping {
                    local_name: alias,
                    exported_name: orig,
                    source: source.to_string(),
                    is_default: false,
                    is_namespace: false,
                });
            } else if !name.is_empty() && name != "*" {
                mappings.push(ImportMapping {
                    local_name: name.to_string(),
                    exported_name: name.to_string(),
                    source: source.to_string(),
                    is_default: false,
                    is_namespace: false,
                });
            }
        }
    }

    for caps in imp_re.captures_iter(content) {
        let source = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
        let alias = caps.get(2).map(|m| m.as_str());
        let local_name = alias
            .map(str::to_string)
            .unwrap_or_else(|| source.rsplit('.').next().unwrap_or(source).to_string());
        mappings.push(ImportMapping {
            local_name,
            exported_name: "*".to_string(),
            source: source.to_string(),
            is_default: false,
            is_namespace: true,
        });
    }

    mappings
}

/// `extractGoImports` (`import-resolver.ts:770-810`).
fn extract_go_imports(content: &str) -> Vec<ImportMapping> {
    static SINGLE_RE: OnceLock<Regex> = OnceLock::new();
    static BLOCK_RE: OnceLock<Regex> = OnceLock::new();
    static LINE_RE: OnceLock<Regex> = OnceLock::new();
    let single_re = SINGLE_RE.get_or_init(|| {
        Regex::new(r#"import\s+(?:(\w+)\s+)?["']([^"']+)["']"#).expect("go single")
    });
    let block_re =
        BLOCK_RE.get_or_init(|| Regex::new(r"(?s)import\s*\(\s*([^)]+)\s*\)").expect("go block"));
    let line_re =
        LINE_RE.get_or_init(|| Regex::new(r#"(?:(\w+)\s+)?["']([^"']+)["']"#).expect("go line"));

    let mut mappings = Vec::new();

    for caps in single_re.captures_iter(content) {
        let alias = caps.get(1).map(|m| m.as_str());
        let source = caps.get(2).map(|m| m.as_str()).unwrap_or_default();
        let package_name = source.rsplit('/').next().unwrap_or(source);
        mappings.push(ImportMapping {
            local_name: alias.unwrap_or(package_name).to_string(),
            exported_name: "*".to_string(),
            source: source.to_string(),
            is_default: false,
            is_namespace: true,
        });
    }

    for caps in block_re.captures_iter(content) {
        let block = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
        for line_caps in line_re.captures_iter(block) {
            let alias = line_caps.get(1).map(|m| m.as_str());
            let source = line_caps.get(2).map(|m| m.as_str()).unwrap_or_default();
            let package_name = source.rsplit('/').next().unwrap_or(source);
            mappings.push(ImportMapping {
                local_name: alias.unwrap_or(package_name).to_string(),
                exported_name: "*".to_string(),
                source: source.to_string(),
                is_default: false,
                is_namespace: true,
            });
        }
    }

    mappings
}

/// `extractJavaImports` (`import-resolver.ts:826-853`).
fn extract_java_imports(content: &str) -> Vec<ImportMapping> {
    static BLOCK_RE: OnceLock<Regex> = OnceLock::new();
    static LINE_RE: OnceLock<Regex> = OnceLock::new();
    static IMPORT_RE: OnceLock<Regex> = OnceLock::new();
    let block_re = BLOCK_RE.get_or_init(|| Regex::new(r"(?s)/\*.*?\*/").expect("java block"));
    let line_re = LINE_RE.get_or_init(|| Regex::new(r"//[^\n]*").expect("java line"));
    let import_re = IMPORT_RE.get_or_init(|| {
        Regex::new(r"(?m)^\s*import\s+(static\s+)?([\w.]+(?:\.\*)?)\s*;").expect("java import")
    });

    let no_block = block_re.replace_all(content, "");
    let stripped = line_re.replace_all(&no_block, "");
    let mut mappings = Vec::new();
    for caps in import_re.captures_iter(&stripped) {
        let fqn = caps.get(2).map(|m| m.as_str()).unwrap_or_default();
        if fqn.ends_with(".*") {
            continue;
        }
        let local_name = fqn.rsplit('.').next().unwrap_or(fqn);
        if local_name.is_empty() {
            continue;
        }
        mappings.push(ImportMapping {
            local_name: local_name.to_string(),
            exported_name: local_name.to_string(),
            source: fqn.to_string(),
            is_default: false,
            is_namespace: false,
        });
    }
    mappings
}

/// `extractPHPImports` (`import-resolver.ts:858-878`).
fn extract_php_imports(content: &str) -> Vec<ImportMapping> {
    static USE_RE: OnceLock<Regex> = OnceLock::new();
    let use_re =
        USE_RE.get_or_init(|| Regex::new(r"use\s+([\w\\]+)(?:\s+as\s+(\w+))?;").expect("php use"));

    let mut mappings = Vec::new();
    for caps in use_re.captures_iter(content) {
        let full_path = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
        let alias = caps.get(2).map(|m| m.as_str());
        let class_name = full_path.rsplit('\\').next().unwrap_or(full_path);
        mappings.push(ImportMapping {
            local_name: alias.unwrap_or(class_name).to_string(),
            exported_name: class_name.to_string(),
            source: full_path.to_string(),
            is_default: false,
            is_namespace: false,
        });
    }
    mappings
}

/// `extractCppImports` (`import-resolver.ts:889-910`).
fn extract_cpp_imports(content: &str) -> Vec<ImportMapping> {
    static INC_RE: OnceLock<Regex> = OnceLock::new();
    static EXT_RE: OnceLock<Regex> = OnceLock::new();
    let inc_re = INC_RE.get_or_init(|| {
        Regex::new(r#"(?m)^\s*#\s*include\s+[<"]([^>"]+)[>"]"#).expect("cpp include")
    });
    let ext_re = EXT_RE
        .get_or_init(|| Regex::new(r"\.(h|hpp|hxx|hh|inl|ipp|cxx|cc|cpp)$").expect("cpp ext"));

    let mut mappings = Vec::new();
    for caps in inc_re.captures_iter(content) {
        let module_path = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
        let last = module_path.rsplit('/').next().unwrap_or(module_path);
        let basename = ext_re.replace(last, "").to_string();
        let local_name = if basename.is_empty() {
            module_path.to_string()
        } else {
            basename
        };
        mappings.push(ImportMapping {
            local_name,
            exported_name: "*".to_string(),
            source: module_path.to_string(),
            is_default: false,
            is_namespace: true,
        });
    }
    mappings
}

fn parse_as_alias(name: &str) -> Option<(String, String)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(\w+)\s+as\s+(\w+)").expect("as-alias"));
    re.captures(name)
        .map(|c| (c[1].to_string(), c[2].to_string()))
}

fn parse_colon_alias(name: &str) -> Option<(String, String)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(\w+)\s*:\s*(\w+)").expect("colon-alias"));
    re.captures(name)
        .map(|c| (c[1].to_string(), c[2].to_string()))
}

/// Strip JS line + block comments preserving strings (`stripJsComments`,
/// `import-resolver.ts:935-972`).
fn strip_js_comments(content: &str) -> String {
    let chars: Vec<char> = content.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(content.len());
    let mut i = 0;
    let mut str_ch: Option<char> = None;
    while i < n {
        let ch = chars[i];
        if let Some(quote) = str_ch {
            out.push(ch);
            if ch == '\\' && i + 1 < n {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if ch == quote {
                str_ch = None;
            }
            i += 1;
            continue;
        }
        if ch == '"' || ch == '\'' || ch == '`' {
            str_ch = Some(ch);
            out.push(ch);
            i += 1;
            continue;
        }
        if ch == '/' && chars.get(i + 1).copied() == Some('/') {
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if ch == '/' && chars.get(i + 1).copied() == Some('*') {
            i += 2;
            while i < n && !(chars[i] == '*' && chars.get(i + 1).copied() == Some('/')) {
                i += 1;
            }
            i += 2;
            continue;
        }
        out.push(ch);
        i += 1;
    }
    out
}

/// Extract JS/TS re-export declarations (`extractReExports`,
/// `import-resolver.ts:989-1042`).
pub fn extract_re_exports(content: &str, language: Language) -> Vec<ReExport> {
    if !matches!(
        language,
        Language::TypeScript | Language::JavaScript | Language::Tsx | Language::Jsx
    ) {
        return Vec::new();
    }
    let cleaned = strip_js_comments(content);
    let mut out = Vec::new();

    static WILDCARD_RE: OnceLock<Regex> = OnceLock::new();
    static NAMED_RE: OnceLock<Regex> = OnceLock::new();
    let wildcard_re = WILDCARD_RE.get_or_init(|| {
        Regex::new(r#"export\s*\*(?:\s+as\s+\w+)?\s*from\s*['"]([^'"]+)['"]"#)
            .expect("reexport wildcard")
    });
    let named_re = NAMED_RE.get_or_init(|| {
        Regex::new(r#"export\s*\{([^}]+)\}\s*from\s*['"]([^'"]+)['"]"#).expect("reexport named")
    });

    for caps in wildcard_re.captures_iter(&cleaned) {
        out.push(ReExport::Wildcard {
            source: caps[1].to_string(),
        });
    }

    for caps in named_re.captures_iter(&cleaned) {
        let inner = &caps[1];
        let source = caps[2].to_string();
        for raw in inner.split(',') {
            let item = raw.trim();
            if item.is_empty() {
                continue;
            }
            if let Some((orig, alias)) = parse_as_alias(item) {
                out.push(ReExport::Named {
                    exported_name: alias,
                    original_name: orig,
                    source: source.clone(),
                });
            } else if is_word(item) {
                out.push(ReExport::Named {
                    exported_name: item.to_string(),
                    original_name: item.to_string(),
                    source: source.clone(),
                });
            }
        }
    }

    out
}

fn is_word(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Resolve a JVM (Java/Kotlin) FQN import via the qualifiedName index
/// (`resolveJvmImport`, `import-resolver.ts:1056-1088`).
pub fn resolve_jvm_import(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if reference.reference_kind != codegraph_core::types::EdgeKind::Imports {
        return None;
    }
    if !matches!(reference.language, Language::Java | Language::Kotlin) {
        return None;
    }

    let fqn = &reference.reference_name;
    let last_dot = fqn.rfind('.')?;
    if last_dot == 0 {
        return None;
    }
    let pkg = &fqn[..last_dot];
    let sym = &fqn[last_dot + 1..];
    if sym == "*" {
        return None;
    }

    let candidates = context.get_nodes_by_qualified_name(&format!("{pkg}::{sym}"));
    if candidates.is_empty() {
        return None;
    }

    let best = if candidates.len() == 1 {
        candidates[0].clone()
    } else {
        pick_closest_jvm_candidate(&candidates, &reference.file_path)
    };
    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: best.id,
        confidence: 0.95,
        resolved_by: ResolvedBy::Import,
    })
}

/// `pickClosestJvmCandidate` (`import-resolver.ts:1096-1119`).
fn pick_closest_jvm_candidate(candidates: &[Node], from_path: &str) -> Node {
    let from_dirs: Vec<&str> = drop_last_segment(from_path);
    let shared_prefix = |p: &str| -> usize {
        let d = drop_last_segment(p);
        let mut shared = 0;
        for i in 0..from_dirs.len().min(d.len()) {
            if from_dirs[i] == d[i] {
                shared += 1;
            } else {
                break;
            }
        }
        shared
    };
    let is_expect = |n: &Node| -> bool { n.decorators.iter().any(|d| d == "expect") };

    let mut best = candidates[0].clone();
    let mut best_prox = shared_prefix(&best.file_path);
    for c in &candidates[1..] {
        let prox = shared_prefix(&c.file_path);
        if prox > best_prox || (prox == best_prox && is_expect(c) && !is_expect(&best)) {
            best = c.clone();
            best_prox = prox;
        }
    }
    best
}

fn drop_last_segment(path: &str) -> Vec<&str> {
    let segs: Vec<&str> = path.split('/').collect();
    if segs.is_empty() {
        Vec::new()
    } else {
        segs[..segs.len() - 1].to_vec()
    }
}

/// Resolve a reference using import mappings (`resolveViaImport`,
/// `import-resolver.ts:1121-1309`).
///
/// NOTE: Go cross-package, Python module-member/absolute-module, Rust path, and
/// Lua require resolution (import-resolver.ts:1202-1253) are language-specific
/// import refinements layered on the same `findExportedSymbol` core; the v1 port
/// implements the JS/TS/JVM/PHP/C-C++ paths that the golden mini exercises plus
/// the generic import-mapping + re-export chase. The remaining per-language
/// import refinements are documented in `KNOWN_DIFFS.md`.
pub fn resolve_via_import(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    // C/C++ #include → file→file edge (import-resolver.ts:1132-1164).
    if matches!(reference.language, Language::C | Language::Cpp)
        && reference.reference_kind == codegraph_core::types::EdgeKind::Imports
    {
        let from_dir = pathutil::dirname(&reference.file_path);
        let sibling_path = pathutil::normalize(&if from_dir.is_empty() {
            reference.reference_name.clone()
        } else {
            format!("{from_dir}/{}", reference.reference_name)
        });
        let sibling_base = pathutil::basename(&sibling_path);
        if let Some(sibling) = context
            .get_nodes_by_name(sibling_base)
            .into_iter()
            .find(|n| n.kind == NodeKind::File && n.file_path == sibling_path)
        {
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: sibling.id,
                confidence: 0.92,
                resolved_by: ResolvedBy::Import,
            });
        }
        let resolved_path = resolve_import_path(
            &reference.reference_name,
            &reference.file_path,
            reference.language,
            context,
        )?;
        let basename = pathutil::basename(&resolved_path);
        if let Some(file_node) = context
            .get_nodes_by_name(basename)
            .into_iter()
            .find(|n| n.kind == NodeKind::File && n.file_path == resolved_path)
        {
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: file_node.id,
                confidence: 0.9,
                resolved_by: ResolvedBy::Import,
            });
        }
        return None;
    }

    // PHP include/require → file→file edge (import-resolver.ts:1173-1194).
    if is_php_include_path_ref(reference) {
        if let Some(resolved_path) =
            resolve_php_include_path(&reference.reference_name, &reference.file_path, context)
        {
            let basename = pathutil::basename(&resolved_path);
            if let Some(file_node) = context
                .get_nodes_by_name(basename)
                .into_iter()
                .find(|n| n.kind == NodeKind::File && n.file_path == resolved_path)
            {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: file_node.id,
                    confidence: 0.9,
                    resolved_by: ResolvedBy::Import,
                });
            }
        }
        return None;
    }

    let imports = context.get_import_mappings(&reference.file_path, reference.language);
    if imports.is_empty() && context.read_file(&reference.file_path).is_none() {
        return None;
    }

    // Go cross-package calls: `pkga.FuncX(...)` → import `…/pkga` maps to a
    // package DIRECTORY; the generic file-lookup below can't follow that
    // (import-resolver.ts:1205-1211, issue #388).
    if reference.language == Language::Go {
        if let Some(go_result) = resolve_go_cross_package_reference(reference, &imports, context) {
            return Some(go_result);
        }
    }

    // Java / Kotlin imported-reference disambiguation (import-resolver.ts:1217-1220).
    if matches!(reference.language, Language::Java | Language::Kotlin) {
        if let Some(java_result) = resolve_java_imported_reference(reference, &imports, context) {
            return Some(java_result);
        }
    }

    // Python qualified access through an imported MODULE (`certs.where()` after
    // `from . import certs`), and absolute dotted module import
    // (`import pkg.mod`) (import-resolver.ts:1226-1238).
    if reference.language == Language::Python {
        if let Some(py_result) = resolve_python_module_member(reference, &imports, context) {
            return Some(py_result);
        }
        if let Some(py_mod) = resolve_python_absolute_module(reference, context) {
            return Some(py_mod);
        }
    }

    // Rust qualified path: resolve the module prefix of `crate::m::Item` /
    // `self::sub::Item` / `super::m::func` to a file, then find the leaf symbol
    // (import-resolver.ts:1243-1248).
    if reference.language == Language::Rust && reference.reference_name.contains("::") {
        if let Some(rust_result) = resolve_rust_path_reference(reference, context) {
            return Some(rust_result);
        }
    }

    // Lua / Luau `require(...)` dotted module path → module file
    // (import-resolver.ts:1253-1256).
    if matches!(reference.language, Language::Lua | Language::Luau)
        && reference.reference_kind == codegraph_core::types::EdgeKind::Imports
    {
        if let Some(lua_result) = resolve_lua_require(reference, context) {
            return Some(lua_result);
        }
    }

    // Whole-module / namespace imports → file→file edge (import-resolver.ts:1260-1269).
    if matches!(
        reference.language,
        Language::Python
            | Language::TypeScript
            | Language::Tsx
            | Language::JavaScript
            | Language::Jsx
    ) {
        if let Some(module_file) = resolve_module_import_to_file(reference, &imports, context) {
            return Some(module_file);
        }
    }

    // Generic: reference name matches an import (import-resolver.ts:1272-1306).
    for imp in &imports {
        if imp.local_name == reference.reference_name
            || reference
                .reference_name
                .starts_with(&format!("{}.", imp.local_name))
        {
            if let Some(resolved_path) = resolve_import_path(
                &imp.source,
                &reference.file_path,
                reference.language,
                context,
            ) {
                let exported_name = if imp.is_default {
                    "default".to_string()
                } else {
                    imp.exported_name.clone()
                };
                let member_name = if imp.is_namespace {
                    Some(
                        reference
                            .reference_name
                            .replacen(&format!("{}.", imp.local_name), "", 1),
                    )
                } else {
                    None
                };

                if let Some(target_node) = find_exported_symbol(
                    &resolved_path,
                    &Want {
                        is_default: imp.is_default,
                        is_namespace: imp.is_namespace,
                        exported_name,
                        member_name,
                    },
                    reference.language,
                    context,
                    &mut BTreeSet::new(),
                    0,
                ) {
                    // `Foo.bar()` on a NAMED class import resolves the receiver to the
                    // class container; descend to the static member so the edge stays a
                    // `calls`/etc. to `method:bar` rather than mislinking to the class
                    // (which `create_edges` would then mis-promote to `instantiates`).
                    // Ports import-resolver.ts:1298-1316 (upstream f7441f21 / #825).
                    let resolved_target = if !imp.is_namespace {
                        resolve_static_member(&target_node, reference, &imp.local_name, context)
                            .unwrap_or(target_node)
                    } else {
                        target_node
                    };
                    return Some(ResolvedRef {
                        original: reference.clone(),
                        target_node_id: resolved_target.id,
                        confidence: 0.9,
                        resolved_by: ResolvedBy::Import,
                    });
                }
            }
        }
    }

    None
}

/// Resolve a whole-MODULE import to its file (`resolveModuleImportToFile`,
/// `import-resolver.ts:1436-1487`). v1 covers the namespace/default → module-file
/// edge; Python absolute-module file lookup is documented in `KNOWN_DIFFS.md`.
fn resolve_module_import_to_file(
    reference: &RefView,
    imports: &[ImportMapping],
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if reference.reference_kind != codegraph_core::types::EdgeKind::Imports {
        return None;
    }
    if reference.reference_name.contains('.') {
        return None;
    }

    for imp in imports {
        if imp.local_name != reference.reference_name {
            continue;
        }

        let module_path = if imp.is_namespace || imp.is_default {
            imp.source.clone()
        } else if reference.language == Language::Python {
            if imp.source.ends_with('.') {
                format!("{}{}", imp.source, imp.local_name)
            } else {
                format!("{}.{}", imp.source, imp.local_name)
            }
        } else {
            continue;
        };

        if let Some(resolved_path) = resolve_import_path(
            &module_path,
            &reference.file_path,
            reference.language,
            context,
        ) {
            if resolved_path != reference.file_path {
                if let Some(file_node) = context
                    .get_nodes_in_file(&resolved_path)
                    .into_iter()
                    .find(|n| n.kind == NodeKind::File)
                {
                    return Some(ResolvedRef {
                        original: reference.clone(),
                        target_node_id: file_node.id,
                        confidence: 0.9,
                        resolved_by: ResolvedBy::Import,
                    });
                }
            }
        }

        // Python absolute `from a.b import submodule`: resolve_import_path only
        // maps RELATIVE dotted paths, so fall back to the absolute dotted-module
        // file lookup (import-resolver.ts:1498-1505).
        if reference.language == Language::Python {
            if let Some(mod_file) =
                find_python_module_file(&module_path, context, &reference.file_path)
            {
                if let Some(file_node) = context
                    .get_nodes_in_file(&mod_file)
                    .into_iter()
                    .find(|n| n.kind == NodeKind::File)
                {
                    return Some(ResolvedRef {
                        original: reference.clone(),
                        target_node_id: file_node.id,
                        confidence: 0.9,
                        resolved_by: ResolvedBy::Import,
                    });
                }
            }
        }
    }
    None
}

/// Resolve a Java/Kotlin reference whose receiver is an imported FQN's simple
/// name (`resolveJavaImportedReference`, `import-resolver.ts:1671-1735`).
fn resolve_java_imported_reference(
    reference: &RefView,
    imports: &[ImportMapping],
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if imports.is_empty() {
        return None;
    }
    let ext = if reference.language == Language::Kotlin {
        ".kt"
    } else {
        ".java"
    };

    for imp in imports {
        let matches_bare = imp.local_name == reference.reference_name;
        let matches_qualified = reference
            .reference_name
            .starts_with(&format!("{}.", imp.local_name));
        if !matches_bare && !matches_qualified {
            continue;
        }

        let fqn_path = format!("{}{}", imp.source.replace('.', "/"), ext);
        let member_name = if matches_bare {
            imp.local_name.clone()
        } else {
            reference.reference_name[imp.local_name.len() + 1..].to_string()
        };

        let candidates = context.get_nodes_by_name(&member_name);
        for node in &candidates {
            if node.language != reference.language {
                continue;
            }
            let fp = node.file_path.replace('\\', "/");
            if fp.ends_with(&fqn_path) || fp.ends_with(&format!("/{fqn_path}")) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: node.id.clone(),
                    confidence: 0.9,
                    resolved_by: ResolvedBy::Import,
                });
            }
        }

        if matches_bare {
            if let Some(dot) = imp.source.rfind('.') {
                if dot > 0 {
                    let owner_fqn = &imp.source[..dot];
                    let owner_path = format!("{}{}", owner_fqn.replace('.', "/"), ext);
                    for node in &candidates {
                        if node.language != reference.language {
                            continue;
                        }
                        let fp = node.file_path.replace('\\', "/");
                        if fp.ends_with(&owner_path) || fp.ends_with(&format!("/{owner_path}")) {
                            return Some(ResolvedRef {
                                original: reference.clone(),
                                target_node_id: node.id.clone(),
                                confidence: 0.9,
                                resolved_by: ResolvedBy::Import,
                            });
                        }
                    }
                }
            }
        }
    }
    None
}

/// Recursive depth cap for re-export chains (`REEXPORT_MAX_DEPTH`,
/// `import-resolver.ts:1798`).
const REEXPORT_MAX_DEPTH: u32 = 8;

struct Want {
    is_default: bool,
    is_namespace: bool,
    exported_name: String,
    member_name: Option<String>,
}

/// Find an exported symbol, following re-export chains (`findExportedSymbol`,
/// `import-resolver.ts:1810-1896`).
fn find_exported_symbol(
    file_path: &str,
    want: &Want,
    language: Language,
    context: &dyn ResolutionContext,
    visited: &mut BTreeSet<String>,
    depth: u32,
) -> Option<Node> {
    if depth > REEXPORT_MAX_DEPTH {
        return None;
    }
    if visited.contains(file_path) {
        return None;
    }
    visited.insert(file_path.to_string());

    let nodes_in_file = context.get_nodes_in_file(file_path);

    // 1. Direct hit (import-resolver.ts:1829-1853).
    if want.is_default {
        if let Some(direct) = nodes_in_file
            .iter()
            .find(|n| n.is_exported && n.kind == NodeKind::Component)
            .or_else(|| {
                nodes_in_file.iter().find(|n| {
                    n.is_exported && matches!(n.kind, NodeKind::Function | NodeKind::Class)
                })
            })
        {
            return Some(direct.clone());
        }
    } else if want.is_namespace {
        if let Some(member) = &want.member_name {
            if let Some(direct) = nodes_in_file
                .iter()
                .find(|n| &n.name == member && n.is_exported)
            {
                return Some(direct.clone());
            }
        }
    } else if let Some(direct) = nodes_in_file
        .iter()
        .find(|n| n.name == want.exported_name && n.is_exported)
    {
        return Some(direct.clone());
    }

    // 2 + 3. Re-export hits (import-resolver.ts:1855-1893).
    let re_exports = context.get_re_exports(file_path, language);
    if re_exports.is_empty() {
        return None;
    }

    let target_name = if want.is_default {
        "default"
    } else {
        &want.exported_name
    };
    for rex in &re_exports {
        if let ReExport::Named {
            exported_name,
            original_name,
            source,
        } = rex
        {
            if exported_name == target_name {
                if let Some(next) = resolve_import_path(source, file_path, language, context) {
                    let chained = find_exported_symbol(
                        &next,
                        &Want {
                            is_default: original_name == "default",
                            is_namespace: false,
                            exported_name: original_name.clone(),
                            member_name: None,
                        },
                        language,
                        context,
                        visited,
                        depth + 1,
                    );
                    if chained.is_some() {
                        return chained;
                    }
                }
            }
        }
    }

    for rex in &re_exports {
        if let ReExport::Wildcard { source } = rex {
            if let Some(next) = resolve_import_path(source, file_path, language, context) {
                let chained =
                    find_exported_symbol(&next, want, language, context, visited, depth + 1);
                if chained.is_some() {
                    return chained;
                }
            }
        }
    }

    None
}

fn is_static_member_container(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Struct
            | NodeKind::Interface
            | NodeKind::Enum
            | NodeKind::Trait
            | NodeKind::Protocol
    )
}

/// Resolve `Container.member` — a static method/property access on a NAMED class
/// import — down to the member node, given the already-resolved container.
///
/// Ports `resolveStaticMember` (import-resolver.ts:1937-1958). Returns `None`
/// when the container is not a static-member container, the reference is not a
/// `Container.member...` access, or no same-file member matches; the caller then
/// keeps the container as the resolution target.
fn resolve_static_member(
    container: &Node,
    reference: &RefView,
    local_name: &str,
    context: &dyn ResolutionContext,
) -> Option<Node> {
    if !is_static_member_container(container.kind) {
        return None;
    }
    let prefix = format!("{local_name}.");
    let remainder = reference.reference_name.strip_prefix(&prefix)?;
    let member = remainder.split('.').next().filter(|s| !s.is_empty())?;

    let qualified = format!("{}::{}", container.qualified_name, member);
    let mut candidates: Vec<Node> = context
        .get_nodes_by_qualified_name(&qualified)
        .into_iter()
        .filter(|n| n.file_path == container.file_path)
        .collect();
    if candidates.is_empty() {
        return None;
    }

    if reference.reference_kind == codegraph_core::types::EdgeKind::Calls {
        if let Some(callable) = candidates
            .iter()
            .find(|n| matches!(n.kind, NodeKind::Method | NodeKind::Function))
        {
            return Some(callable.clone());
        }
    }
    Some(candidates.swap_remove(0))
}

/// Go cross-package call `pkga.FuncX` → the exported member in the package
/// directory the import maps to. Ports `resolveGoCrossPackageReference`
/// (import-resolver.ts, issue #388).
fn resolve_go_cross_package_reference(
    reference: &RefView,
    imports: &[ImportMapping],
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let module = context.get_go_module()?;
    let dot_idx = reference.reference_name.find('.')?;
    if dot_idx == 0 {
        return None;
    }
    let receiver = &reference.reference_name[..dot_idx];
    let member = &reference.reference_name[dot_idx + 1..];
    if member.is_empty() {
        return None;
    }

    for imp in imports {
        if imp.local_name != receiver {
            continue;
        }
        // Only in-module imports map to a known directory.
        if imp.source != module.module_path
            && !imp.source.starts_with(&format!("{}/", module.module_path))
        {
            continue;
        }
        let pkg_dir = if imp.source == module.module_path {
            ""
        } else {
            &imp.source[module.module_path.len() + 1..]
        };

        for node in context.get_nodes_by_name(member) {
            if node.language != Language::Go || !node.is_exported {
                continue;
            }
            let fp = node.file_path.replace('\\', "/");
            let file_dir = match fp.rfind('/') {
                Some(i) => &fp[..i],
                None => "",
            };
            if file_dir == pkg_dir {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: node.id,
                    confidence: 0.9,
                    resolved_by: ResolvedBy::Import,
                });
            }
        }
    }
    None
}

/// Python `mod.func()` where `mod` is an imported MODULE (submodule of a
/// package or a bare `import mod`). Ports `resolvePythonModuleMember`
/// (import-resolver.ts, issue #578).
fn resolve_python_module_member(
    reference: &RefView,
    imports: &[ImportMapping],
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let dot_idx = reference.reference_name.find('.')?;
    if dot_idx == 0 {
        return None;
    }
    let receiver = &reference.reference_name[..dot_idx];
    let member = reference.reference_name[dot_idx + 1..].split('.').next()?;
    if member.is_empty() {
        return None;
    }

    for imp in imports {
        if imp.local_name != receiver {
            continue;
        }
        let module_path = if imp.is_namespace {
            imp.source.clone()
        } else if imp.source.ends_with('.') {
            format!("{}{}", imp.source, imp.local_name)
        } else {
            format!("{}.{}", imp.source, imp.local_name)
        };

        let resolved_path = resolve_import_path(
            &module_path,
            &reference.file_path,
            reference.language,
            context,
        )
        .or_else(|| find_python_module_file(&module_path, context, &reference.file_path));
        let Some(resolved_path) = resolved_path else {
            continue;
        };
        if resolved_path == reference.file_path {
            continue;
        }

        if let Some(target) = context
            .get_nodes_in_file(&resolved_path)
            .into_iter()
            .find(|n| {
                n.name == member
                    && matches!(
                        n.kind,
                        NodeKind::Function
                            | NodeKind::Class
                            | NodeKind::Variable
                            | NodeKind::Constant
                    )
            })
        {
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: target.id,
                confidence: 0.85,
                resolved_by: ResolvedBy::Import,
            });
        }
    }
    None
}

/// Python absolute dotted module import `import a.b.c` → its file (the Django
/// `AppConfig.ready()` side-effect-import pattern). Ports
/// `resolvePythonAbsoluteModule` (import-resolver.ts).
fn resolve_python_absolute_module(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if reference.reference_kind != codegraph_core::types::EdgeKind::Imports {
        return None;
    }
    if !reference.reference_name.contains('.') {
        return None;
    }
    let hit_id = find_python_module_file(&reference.reference_name, context, &reference.file_path)
        .and_then(|path| {
            context
                .get_nodes_in_file(&path)
                .into_iter()
                .find(|n| n.kind == NodeKind::File)
                .map(|n| n.id)
        })?;
    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: hit_id,
        confidence: 0.9,
        resolved_by: ResolvedBy::Import,
    })
}

/// Find a Python module file by its absolute dotted path (`a.b.c` →
/// `a/b/c.py` or `a/b/c/__init__.py`). Ports `findPythonModuleFile`.
fn find_python_module_file(
    module: &str,
    context: &dyn ResolutionContext,
    exclude_file_path: &str,
) -> Option<String> {
    if module.is_empty() || module.starts_with('.') {
        return None;
    }
    let rel = module.replace('.', "/");
    let last_seg = module.split('.').next_back()?;
    let ends_with = |p: &str, want: &str| p == want || p.ends_with(&format!("/{want}"));

    let module_file = context
        .get_nodes_by_name(&format!("{last_seg}.py"))
        .into_iter()
        .find(|n| {
            n.kind == NodeKind::File
                && n.file_path != exclude_file_path
                && ends_with(&n.file_path, &format!("{rel}.py"))
        });
    if let Some(n) = module_file {
        return Some(n.file_path);
    }
    context
        .get_nodes_by_name("__init__.py")
        .into_iter()
        .find(|n| {
            n.kind == NodeKind::File
                && n.file_path != exclude_file_path
                && ends_with(&n.file_path, &format!("{rel}/__init__.py"))
        })
        .map(|n| n.file_path)
}

/// Rust qualified path `A::B::C` → leaf `C` in the file the module prefix
/// `A::B` resolves to. Ports `resolveRustPathReference`.
fn resolve_rust_path_reference(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let segments: Vec<&str> = reference
        .reference_name
        .split("::")
        .filter(|s| !s.is_empty())
        .collect();
    if segments.len() < 2 {
        return None;
    }
    let leaf = *segments.last()?;
    let mod_segs = &segments[..segments.len() - 1];

    let file = resolve_rust_module_file(mod_segs, &reference.file_path, context)?;
    if file == reference.file_path {
        return None;
    }
    let target = context.get_nodes_in_file(&file).into_iter().find(|n| {
        n.name == leaf
            && matches!(
                n.kind,
                NodeKind::Function
                    | NodeKind::Struct
                    | NodeKind::Enum
                    | NodeKind::Trait
                    | NodeKind::TypeAlias
                    | NodeKind::Constant
                    | NodeKind::Method
                    | NodeKind::Class
                    | NodeKind::Interface
            )
    })?;
    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: target.id,
        confidence: 0.9,
        resolved_by: ResolvedBy::Import,
    })
}

/// The crate-root directory (holds `lib.rs`/`main.rs`), walking up from a file.
/// Paths are project-relative POSIX. Ports `rustCrateRootDir`.
fn rust_crate_root_dir(from_file: &str, context: &dyn ResolutionContext) -> Option<String> {
    let mut dir = pathutil::dirname(from_file);
    for _ in 0..64 {
        let lib = if dir.is_empty() {
            "lib.rs".to_string()
        } else {
            format!("{dir}/lib.rs")
        };
        let main = if dir.is_empty() {
            "main.rs".to_string()
        } else {
            format!("{dir}/main.rs")
        };
        if context.file_exists(&lib) || context.file_exists(&main) {
            return Some(dir);
        }
        if dir.is_empty() {
            return None;
        }
        dir = pathutil::dirname(&dir);
    }
    None
}

/// Directory under which the current file's module declares its SUBMODULES.
/// Ports `rustSelfModuleDir`.
fn rust_self_module_dir(from_file: &str) -> String {
    let base = pathutil::basename(from_file);
    let dir = pathutil::dirname(from_file);
    if matches!(base, "mod.rs" | "lib.rs" | "main.rs") {
        return dir;
    }
    let stem = base.strip_suffix(".rs").unwrap_or(base);
    if dir.is_empty() {
        stem.to_string()
    } else {
        format!("{dir}/{stem}")
    }
}

/// Walk module segments to the leaf module's file. Ports `resolveRustModuleFile`.
fn resolve_rust_module_file(
    segments: &[&str],
    from_file: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    if segments.is_empty() {
        return None;
    }
    let resolve_under = |start_dir: Option<String>, rest: &[&str]| -> Option<String> {
        let mut dir = start_dir?;
        let mut target_file: Option<String> = None;
        for seg in rest {
            if matches!(*seg, "self" | "crate" | "super") {
                continue;
            }
            let as_file = if dir.is_empty() {
                format!("{seg}.rs")
            } else {
                format!("{dir}/{seg}.rs")
            };
            let as_mod = if dir.is_empty() {
                format!("{seg}/mod.rs")
            } else {
                format!("{dir}/{seg}/mod.rs")
            };
            if context.file_exists(&as_file) {
                target_file = Some(as_file);
            } else if context.file_exists(&as_mod) {
                target_file = Some(as_mod);
            } else {
                return None;
            }
            dir = if dir.is_empty() {
                (*seg).to_string()
            } else {
                format!("{dir}/{seg}")
            };
        }
        target_file
    };

    let first = segments[0];
    if first == "crate" {
        return resolve_under(rust_crate_root_dir(from_file, context), &segments[1..]);
    }
    if first == "self" {
        return resolve_under(Some(rust_self_module_dir(from_file)), &segments[1..]);
    }
    if first == "super" {
        let supers = segments.iter().take_while(|s| **s == "super").count();
        let mut dir = Some(rust_self_module_dir(from_file));
        for _ in 0..supers {
            dir = dir.filter(|d| !d.is_empty()).map(|d| pathutil::dirname(&d));
        }
        return resolve_under(dir, &segments[supers..]);
    }
    // Bare path: try self-relative (2018 edition submodule) first, then crate.
    resolve_under(Some(rust_self_module_dir(from_file)), segments)
        .or_else(|| resolve_under(rust_crate_root_dir(from_file, context), segments))
}

/// Lua / Luau `require("a.b.c")` dotted module path → module file. Ports
/// `resolveLuaRequire`.
fn resolve_lua_require(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let name = &reference.reference_name;
    if name.is_empty() {
        return None;
    }
    let base = if name.contains('.') {
        name.replace('.', "/")
    } else {
        name.clone()
    };
    let suffixes = [
        format!("{base}.lua"),
        format!("{base}.luau"),
        format!("{base}/init.lua"),
        format!("{base}/init.luau"),
    ];
    let files = context.get_all_files();
    let shared = |a: &str, b: &str| -> usize {
        a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count()
    };
    for suffix in &suffixes {
        let mut matches: Vec<String> = files
            .iter()
            .filter(|f| *f == suffix || f.ends_with(&format!("/{suffix}")))
            .cloned()
            .collect();
        if matches.is_empty() {
            continue;
        }
        matches.sort_by_key(|m| std::cmp::Reverse(shared(m, &reference.file_path)));
        let best = &matches[0];
        if best == &reference.file_path {
            continue;
        }
        if let Some(file_node) = context
            .get_nodes_in_file(best)
            .into_iter()
            .find(|n| n.kind == NodeKind::File)
        {
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: file_node.id,
                confidence: 0.9,
                resolved_by: ResolvedBy::Import,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path_aliases::{AliasMap, AliasPattern};
    use crate::types::{GoModule, ImportMapping, ReExport, RefView, ResolutionContext};
    use crate::workspace_packages::WorkspacePackages;
    use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
    use std::collections::{BTreeMap, BTreeSet};

    /// A fully configurable in-memory [`ResolutionContext`] for unit-testing the
    /// import resolver in isolation (no Store, no filesystem). Every collection
    /// is a plain field so each test builds exactly the graph it needs.
    #[derive(Default)]
    struct TestContext {
        project_root: String,
        existing_files: BTreeSet<String>,
        file_contents: BTreeMap<String, String>,
        nodes: Vec<Node>,
        import_mappings: BTreeMap<String, Vec<ImportMapping>>,
        re_exports: BTreeMap<String, Vec<ReExport>>,
        project_aliases: Option<AliasMap>,
        workspace_packages: Option<WorkspacePackages>,
        go_module: Option<GoModule>,
        cpp_include_dirs: Vec<String>,
    }

    impl ResolutionContext for TestContext {
        fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.file_path == file_path)
                .cloned()
                .collect()
        }
        fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.name == name)
                .cloned()
                .collect()
        }
        fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.qualified_name == qualified_name)
                .cloned()
                .collect()
        }
        fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.kind == kind)
                .cloned()
                .collect()
        }
        fn file_exists(&self, file_path: &str) -> bool {
            self.existing_files.contains(file_path)
        }
        fn read_file(&self, file_path: &str) -> Option<String> {
            self.file_contents.get(file_path).cloned()
        }
        fn get_project_root(&self) -> &str {
            &self.project_root
        }
        fn get_all_files(&self) -> Vec<String> {
            self.existing_files.iter().cloned().collect()
        }
        fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.name.to_lowercase() == lower_name)
                .cloned()
                .collect()
        }
        fn get_node_by_id(&self, id: &str) -> Option<Node> {
            self.nodes.iter().find(|n| n.id == id).cloned()
        }
        fn get_import_mappings(&self, file_path: &str, _language: Language) -> Vec<ImportMapping> {
            self.import_mappings
                .get(file_path)
                .cloned()
                .unwrap_or_default()
        }
        fn get_project_aliases(&self) -> Option<AliasMap> {
            self.project_aliases.clone()
        }
        fn get_workspace_packages(&self) -> Option<WorkspacePackages> {
            self.workspace_packages.clone()
        }
        fn get_go_module(&self) -> Option<GoModule> {
            self.go_module.clone()
        }
        fn get_re_exports(&self, file_path: &str, _language: Language) -> Vec<ReExport> {
            self.re_exports.get(file_path).cloned().unwrap_or_default()
        }
        fn get_cpp_include_dirs(&self) -> Vec<String> {
            self.cpp_include_dirs.clone()
        }
    }

    fn node(id: &str, name: &str, kind: NodeKind, file_path: &str, language: Language) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: file_path.to_string(),
            language,
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

    fn exported(mut n: Node) -> Node {
        n.is_exported = true;
        n
    }

    fn file_node(file_path: &str, language: Language) -> Node {
        node(
            &format!("file:{file_path}"),
            pathutil::basename(file_path),
            NodeKind::File,
            file_path,
            language,
        )
    }

    fn reference(name: &str, kind: EdgeKind, file_path: &str, language: Language) -> RefView {
        RefView {
            from_node_id: "function:caller".to_string(),
            reference_name: name.to_string(),
            reference_kind: kind,
            line: 1,
            column: 0,
            file_path: file_path.to_string(),
            language,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    fn mapping(local: &str, exported: &str, source: &str) -> ImportMapping {
        ImportMapping {
            local_name: local.to_string(),
            exported_name: exported.to_string(),
            source: source.to_string(),
            is_default: false,
            is_namespace: false,
        }
    }

    #[test]
    fn extension_resolution_covers_all_language_arms() {
        assert!(extension_resolution(Language::TypeScript).contains(&".ts"));
        assert!(extension_resolution(Language::JavaScript).contains(&".js"));
        assert!(extension_resolution(Language::Tsx).contains(&".tsx"));
        assert!(extension_resolution(Language::Jsx).contains(&".jsx"));
        assert!(extension_resolution(Language::Svelte).contains(&".svelte"));
        assert!(extension_resolution(Language::Vue).contains(&".vue"));
        assert_eq!(
            extension_resolution(Language::Python),
            &[".py", "/__init__.py"]
        );
        assert_eq!(extension_resolution(Language::Go), &[".go"]);
        assert_eq!(extension_resolution(Language::Rust), &[".rs", "/mod.rs"]);
        assert_eq!(extension_resolution(Language::Java), &[".java"]);
        assert!(extension_resolution(Language::C).contains(&".h"));
        assert!(extension_resolution(Language::Cpp).contains(&".cpp"));
        assert_eq!(extension_resolution(Language::CSharp), &[".cs"]);
        assert_eq!(extension_resolution(Language::Php), &[".php"]);
        assert_eq!(extension_resolution(Language::Ruby), &[".rb"]);
        assert!(extension_resolution(Language::ObjC).contains(&".m"));
        assert!(extension_resolution(Language::Yaml).is_empty());
    }

    #[test]
    fn is_word_recognizes_identifiers() {
        assert!(is_word("foo_bar123"));
        assert!(!is_word(""));
        assert!(!is_word("foo-bar"));
        assert!(!is_word("a.b"));
    }

    #[test]
    fn parse_as_alias_extracts_orig_and_alias() {
        assert_eq!(
            parse_as_alias("Foo as Bar"),
            Some(("Foo".to_string(), "Bar".to_string()))
        );
        assert_eq!(parse_as_alias("Foo"), None);
    }

    #[test]
    fn parse_colon_alias_extracts_orig_and_alias() {
        assert_eq!(
            parse_colon_alias("foo: bar"),
            Some(("foo".to_string(), "bar".to_string()))
        );
        assert_eq!(parse_colon_alias("foo"), None);
    }

    #[test]
    fn extract_import_mappings_dispatches_unknown_language_to_empty() {
        assert!(extract_import_mappings("anything", Language::Yaml).is_empty());
    }

    #[test]
    fn extract_js_imports_default_named_and_namespace() {
        let content = r#"
import Foo from './foo';
import { a, b as c } from './bar';
import * as ns from './baz';
"#;
        let m = extract_import_mappings(content, Language::TypeScript);
        assert!(m.iter().any(|x| x.local_name == "Foo"
            && x.exported_name == "default"
            && x.is_default
            && x.source == "./foo"));
        assert!(
            m.iter()
                .any(|x| x.local_name == "a" && x.exported_name == "a" && !x.is_default)
        );
        assert!(
            m.iter()
                .any(|x| x.local_name == "c" && x.exported_name == "b")
        );
        assert!(
            m.iter()
                .any(|x| x.local_name == "ns" && x.exported_name == "*" && x.is_namespace)
        );
    }

    #[test]
    fn extract_js_imports_require_forms() {
        let content = r#"
const def = require('./mod');
const { x, y: z } = require('./other');
"#;
        let m = extract_import_mappings(content, Language::JavaScript);
        assert!(
            m.iter()
                .any(|x| x.local_name == "def" && x.is_default && x.source == "./mod")
        );
        assert!(
            m.iter()
                .any(|x| x.local_name == "x" && x.exported_name == "x")
        );
        assert!(
            m.iter()
                .any(|x| x.local_name == "z" && x.exported_name == "y")
        );
    }

    #[test]
    fn extract_python_imports_from_and_bare() {
        let content = "from pkg.mod import a, b as c, *\nimport os.path as osp\nimport sys\n";
        let m = extract_import_mappings(content, Language::Python);
        assert!(
            m.iter()
                .any(|x| x.local_name == "a" && x.source == "pkg.mod")
        );
        assert!(
            m.iter()
                .any(|x| x.local_name == "c" && x.exported_name == "b")
        );
        assert!(!m.iter().any(|x| x.local_name == "*"));
        assert!(
            m.iter()
                .any(|x| x.local_name == "osp" && x.source == "os.path" && x.is_namespace)
        );
        assert!(
            m.iter()
                .any(|x| x.local_name == "sys" && x.source == "sys" && x.is_namespace)
        );
    }

    #[test]
    fn extract_go_imports_single_and_block_with_alias() {
        let single = r#"import "fmt""#;
        let m = extract_import_mappings(single, Language::Go);
        assert!(
            m.iter()
                .any(|x| x.local_name == "fmt" && x.source == "fmt" && x.is_namespace)
        );

        let block = "import (\n\t\"os\"\n\talias \"github.com/x/y\"\n)\n";
        let m2 = extract_import_mappings(block, Language::Go);
        assert!(m2.iter().any(|x| x.local_name == "os"));
        assert!(
            m2.iter()
                .any(|x| x.local_name == "alias" && x.source == "github.com/x/y")
        );
    }

    #[test]
    fn extract_java_imports_strips_comments_and_skips_wildcard() {
        let content = "/* block import java.util.Bad; */\n// line import java.io.AlsoBad;\nimport java.util.List;\nimport static com.x.Util.foo;\nimport java.util.*;\n";
        let m = extract_import_mappings(content, Language::Java);
        assert!(
            m.iter()
                .any(|x| x.local_name == "List" && x.source == "java.util.List")
        );
        assert!(m.iter().any(|x| x.local_name == "foo"));
        assert!(!m.iter().any(|x| x.source.ends_with(".*")));
        assert!(
            !m.iter()
                .any(|x| x.local_name == "Bad" || x.local_name == "AlsoBad")
        );
    }

    #[test]
    fn extract_php_imports_use_and_alias() {
        let content = "use App\\Models\\User;\nuse App\\Service as Svc;\n";
        let m = extract_import_mappings(content, Language::Php);
        assert!(m.iter().any(|x| x.local_name == "User"
            && x.exported_name == "User"
            && x.source == "App\\Models\\User"));
        assert!(
            m.iter()
                .any(|x| x.local_name == "Svc" && x.exported_name == "Service")
        );
    }

    #[test]
    fn extract_cpp_imports_include_forms() {
        let content = "#include <vector>\n#include \"foo/bar.hpp\"\n";
        let m = extract_import_mappings(content, Language::Cpp);
        assert!(
            m.iter()
                .any(|x| x.local_name == "vector" && x.source == "vector")
        );
        assert!(
            m.iter()
                .any(|x| x.local_name == "bar" && x.source == "foo/bar.hpp")
        );
    }

    #[test]
    fn strip_js_comments_removes_comments_preserving_strings() {
        let src = r#"const a = 1; // line comment
/* block
comment */ const b = "http://not-a-comment";
const c = 'has // slashes';
const d = `tpl \` escaped`;"#;
        let out = strip_js_comments(src);
        assert!(!out.contains("line comment"));
        assert!(!out.contains("block"));
        assert!(out.contains("http://not-a-comment"));
        assert!(out.contains("has // slashes"));
        assert!(out.contains("escaped"));
    }

    #[test]
    fn extract_re_exports_non_js_language_is_empty() {
        assert!(extract_re_exports("export * from './x'", Language::Python).is_empty());
    }

    #[test]
    fn extract_re_exports_wildcard_and_named() {
        let content = r#"
export * from './all';
export * as ns from './ns';
export { Foo, Bar as Baz } from './named';
export { } from './empty';
"#;
        let out = extract_re_exports(content, Language::TypeScript);
        assert!(
            out.iter()
                .any(|r| matches!(r, ReExport::Wildcard { source } if source == "./all"))
        );
        assert!(
            out.iter()
                .any(|r| matches!(r, ReExport::Wildcard { source } if source == "./ns"))
        );
        assert!(out.iter().any(|r| matches!(
            r,
            ReExport::Named { exported_name, original_name, source }
                if exported_name == "Foo" && original_name == "Foo" && source == "./named"
        )));
        assert!(out.iter().any(|r| matches!(
            r,
            ReExport::Named { exported_name, original_name, .. }
                if exported_name == "Baz" && original_name == "Bar"
        )));
    }

    #[test]
    fn is_external_import_relative_is_local() {
        let ctx = TestContext::default();
        assert!(!is_external_import("./foo", Language::TypeScript, &ctx));
    }

    #[test]
    fn is_external_import_node_builtin_is_external() {
        let ctx = TestContext::default();
        assert!(is_external_import("fs", Language::TypeScript, &ctx));
        assert!(is_external_import("path", Language::JavaScript, &ctx));
    }

    #[test]
    fn is_external_import_bare_specifier_is_external() {
        let ctx = TestContext::default();
        assert!(is_external_import("lodash", Language::TypeScript, &ctx));
    }

    #[test]
    fn is_external_import_alias_prefixes_are_local() {
        let ctx = TestContext::default();
        assert!(!is_external_import("@/foo", Language::TypeScript, &ctx));
        assert!(!is_external_import("~/foo", Language::TypeScript, &ctx));
        assert!(!is_external_import("src/foo", Language::TypeScript, &ctx));
    }

    #[test]
    fn is_external_import_project_alias_prefix_is_local() {
        let ctx = TestContext {
            project_aliases: Some(AliasMap {
                base_url: String::new(),
                patterns: vec![AliasPattern {
                    prefix: "@app/".to_string(),
                    suffix: String::new(),
                    has_wildcard: true,
                    replacements: vec!["app/".to_string()],
                }],
            }),
            ..Default::default()
        };
        assert!(!is_external_import("@app/x", Language::TypeScript, &ctx));
    }

    #[test]
    fn is_external_import_workspace_member_is_local() {
        let mut by_name = BTreeMap::new();
        by_name.insert("@scope/ui".to_string(), "packages/ui".to_string());
        let ctx = TestContext {
            workspace_packages: Some(WorkspacePackages { by_name }),
            ..Default::default()
        };
        assert!(!is_external_import(
            "@scope/ui/x",
            Language::TypeScript,
            &ctx
        ));
    }

    #[test]
    fn is_external_import_python_stdlib_is_external() {
        let ctx = TestContext::default();
        assert!(is_external_import("os.path", Language::Python, &ctx));
        assert!(is_external_import("json", Language::Python, &ctx));
        assert!(!is_external_import("mypkg.mod", Language::Python, &ctx));
    }

    #[test]
    fn is_external_import_go_module_and_internal() {
        let ctx = TestContext {
            go_module: Some(GoModule {
                module_path: "example.com/proj".to_string(),
            }),
            ..Default::default()
        };
        assert!(!is_external_import("example.com/proj", Language::Go, &ctx));
        assert!(!is_external_import(
            "example.com/proj/sub",
            Language::Go,
            &ctx
        ));
        assert!(!is_external_import("x/internal/y", Language::Go, &ctx));
        assert!(is_external_import(
            "github.com/other/pkg",
            Language::Go,
            &ctx
        ));
    }

    #[test]
    fn is_external_import_go_without_module_is_external() {
        let ctx = TestContext::default();
        assert!(is_external_import("github.com/x/y", Language::Go, &ctx));
    }

    #[test]
    fn is_external_import_c_cpp_stdlib_headers() {
        let ctx = TestContext::default();
        assert!(is_external_import("stdio.h", Language::C, &ctx));
        assert!(is_external_import("vector", Language::Cpp, &ctx));
        assert!(is_external_import("string.h", Language::C, &ctx));
        assert!(!is_external_import("myheader.h", Language::C, &ctx));
    }

    #[test]
    fn resolve_import_path_external_returns_none() {
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            ..Default::default()
        };
        assert!(resolve_import_path("lodash", "src/a.ts", Language::TypeScript, &ctx).is_none());
    }

    #[test]
    fn resolve_import_path_relative_with_extension() {
        let mut existing = BTreeSet::new();
        existing.insert("src/foo.ts".to_string());
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            ..Default::default()
        };
        assert_eq!(
            resolve_import_path("./foo", "src/a.ts", Language::TypeScript, &ctx),
            Some("src/foo.ts".to_string())
        );
    }

    #[test]
    fn resolve_import_path_relative_index_file() {
        let mut existing = BTreeSet::new();
        existing.insert("src/dir/index.ts".to_string());
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            ..Default::default()
        };
        assert_eq!(
            resolve_import_path("./dir", "src/a.ts", Language::TypeScript, &ctx),
            Some("src/dir/index.ts".to_string())
        );
    }

    #[test]
    fn resolve_import_path_relative_exact_no_ext() {
        let mut existing = BTreeSet::new();
        existing.insert("src/data.json".to_string());
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            ..Default::default()
        };
        assert_eq!(
            resolve_import_path("./data.json", "src/a.ts", Language::TypeScript, &ctx),
            Some("src/data.json".to_string())
        );
    }

    #[test]
    fn resolve_import_path_relative_unresolved_is_none() {
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            ..Default::default()
        };
        assert!(resolve_import_path("./missing", "src/a.ts", Language::TypeScript, &ctx).is_none());
    }

    #[test]
    fn resolve_import_path_python_dotted_relative() {
        let mut existing = BTreeSet::new();
        existing.insert("pkg/sub/mod.py".to_string());
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            ..Default::default()
        };
        assert_eq!(
            resolve_import_path(".sub.mod", "pkg/app.py", Language::Python, &ctx),
            Some("pkg/sub/mod.py".to_string())
        );
    }

    #[test]
    fn resolve_import_path_python_dotted_package_dir() {
        let mut existing = BTreeSet::new();
        existing.insert("pkg/sub".to_string());
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            ..Default::default()
        };
        assert_eq!(
            resolve_import_path(".sub", "pkg/app.py", Language::Python, &ctx),
            Some("pkg/sub".to_string())
        );
    }

    #[test]
    fn resolve_import_path_python_parent_relative() {
        let mut existing = BTreeSet::new();
        existing.insert("pkg/other.py".to_string());
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            ..Default::default()
        };
        assert_eq!(
            resolve_import_path("..other", "pkg/sub/app.py", Language::Python, &ctx),
            Some("pkg/other.py".to_string())
        );
    }

    #[test]
    fn resolve_import_path_fallback_alias() {
        let mut existing = BTreeSet::new();
        existing.insert("src/util.ts".to_string());
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            ..Default::default()
        };
        assert_eq!(
            resolve_import_path("@/util", "src/a.ts", Language::TypeScript, &ctx),
            Some("src/util.ts".to_string())
        );
    }

    #[test]
    fn resolve_import_path_project_alias_map() {
        let mut existing = BTreeSet::new();
        existing.insert("lib/thing.ts".to_string());
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            project_aliases: Some(AliasMap {
                base_url: "/proj".to_string(),
                patterns: vec![AliasPattern {
                    prefix: "@lib/".to_string(),
                    suffix: String::new(),
                    has_wildcard: true,
                    replacements: vec!["lib/*".to_string()],
                }],
            }),
            ..Default::default()
        };
        assert_eq!(
            resolve_import_path("@lib/thing", "src/a.ts", Language::TypeScript, &ctx),
            Some("lib/thing.ts".to_string())
        );
    }

    #[test]
    fn resolve_import_path_workspace_package() {
        let mut existing = BTreeSet::new();
        existing.insert("packages/ui/index.ts".to_string());
        let mut by_name = BTreeMap::new();
        by_name.insert("@scope/ui".to_string(), "packages/ui".to_string());
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            workspace_packages: Some(WorkspacePackages { by_name }),
            ..Default::default()
        };
        assert_eq!(
            resolve_import_path("@scope/ui", "src/a.ts", Language::TypeScript, &ctx),
            Some("packages/ui/index.ts".to_string())
        );
    }

    #[test]
    fn resolve_import_path_cpp_include_dir_search() {
        let mut existing = BTreeSet::new();
        existing.insert("include/lib/header.h".to_string());
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            cpp_include_dirs: vec!["include".to_string()],
            ..Default::default()
        };
        assert_eq!(
            resolve_import_path("lib/header.h", "src/a.cpp", Language::Cpp, &ctx),
            Some("include/lib/header.h".to_string())
        );
    }

    #[test]
    fn resolve_import_path_cpp_include_dir_with_extension_inference() {
        let mut existing = BTreeSet::new();
        existing.insert("include/foo.h".to_string());
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            cpp_include_dirs: vec!["include".to_string()],
            ..Default::default()
        };
        assert_eq!(
            resolve_import_path("foo", "src/a.c", Language::C, &ctx),
            Some("include/foo.h".to_string())
        );
    }

    #[test]
    fn resolve_import_path_cpp_include_not_found() {
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            cpp_include_dirs: vec!["include".to_string()],
            ..Default::default()
        };
        assert!(resolve_import_path("missing.h", "src/a.cpp", Language::Cpp, &ctx).is_none());
    }

    #[test]
    fn is_php_include_path_ref_detection() {
        let with_slash = reference("lib/foo", EdgeKind::Imports, "a.php", Language::Php);
        assert!(is_php_include_path_ref(&with_slash));
        let with_dot = reference("foo.php", EdgeKind::Imports, "a.php", Language::Php);
        assert!(is_php_include_path_ref(&with_dot));
        let namespace_use = reference("Foo", EdgeKind::Imports, "a.php", Language::Php);
        assert!(!is_php_include_path_ref(&namespace_use));
        let not_import = reference("lib/foo", EdgeKind::Calls, "a.php", Language::Php);
        assert!(!is_php_include_path_ref(&not_import));
        let not_php = reference("lib/foo", EdgeKind::Imports, "a.ts", Language::TypeScript);
        assert!(!is_php_include_path_ref(&not_php));
    }

    #[test]
    fn resolve_jvm_import_single_candidate() {
        let target = node(
            "class:1",
            "MyClass",
            NodeKind::Class,
            "com/example/MyClass.java",
            Language::Java,
        );
        let ctx = TestContext {
            nodes: vec![{
                let mut n = target.clone();
                n.qualified_name = "com.example::MyClass".to_string();
                n
            }],
            ..Default::default()
        };
        let r = reference(
            "com.example.MyClass",
            EdgeKind::Imports,
            "Other.java",
            Language::Java,
        );
        let resolved = resolve_jvm_import(&r, &ctx).expect("resolves");
        assert_eq!(resolved.target_node_id, "class:1");
        assert_eq!(resolved.confidence, 0.95);
        assert_eq!(resolved.resolved_by, ResolvedBy::Import);
    }

    #[test]
    fn resolve_jvm_import_rejects_non_import_and_non_jvm() {
        let ctx = TestContext::default();
        let not_import = reference("a.B", EdgeKind::Calls, "X.java", Language::Java);
        assert!(resolve_jvm_import(&not_import, &ctx).is_none());
        let not_jvm = reference("a.B", EdgeKind::Imports, "X.ts", Language::TypeScript);
        assert!(resolve_jvm_import(&not_jvm, &ctx).is_none());
    }

    #[test]
    fn resolve_jvm_import_rejects_no_dot_leading_dot_and_wildcard() {
        let ctx = TestContext::default();
        let no_dot = reference("Bare", EdgeKind::Imports, "X.java", Language::Java);
        assert!(resolve_jvm_import(&no_dot, &ctx).is_none());
        let leading = reference(".B", EdgeKind::Imports, "X.java", Language::Java);
        assert!(resolve_jvm_import(&leading, &ctx).is_none());
        let wildcard = reference("com.example.*", EdgeKind::Imports, "X.java", Language::Java);
        assert!(resolve_jvm_import(&wildcard, &ctx).is_none());
    }

    #[test]
    fn resolve_jvm_import_no_candidates_is_none() {
        let ctx = TestContext::default();
        let r = reference(
            "com.example.Missing",
            EdgeKind::Imports,
            "X.java",
            Language::Java,
        );
        assert!(resolve_jvm_import(&r, &ctx).is_none());
    }

    #[test]
    fn resolve_jvm_import_picks_closest_by_proximity() {
        let near = {
            let mut n = node(
                "class:near",
                "Svc",
                NodeKind::Class,
                "com/example/app/Svc.java",
                Language::Java,
            );
            n.qualified_name = "com.example::Svc".to_string();
            n
        };
        let far = {
            let mut n = node(
                "class:far",
                "Svc",
                NodeKind::Class,
                "org/other/Svc.java",
                Language::Java,
            );
            n.qualified_name = "com.example::Svc".to_string();
            n
        };
        let ctx = TestContext {
            nodes: vec![far, near],
            ..Default::default()
        };
        let r = reference(
            "com.example.Svc",
            EdgeKind::Imports,
            "com/example/app/Caller.java",
            Language::Java,
        );
        let resolved = resolve_jvm_import(&r, &ctx).expect("resolves");
        assert_eq!(resolved.target_node_id, "class:near");
    }

    #[test]
    fn resolve_jvm_import_prefers_expect_decorator_on_tie() {
        let plain = {
            let mut n = node(
                "class:plain",
                "Svc",
                NodeKind::Class,
                "com/example/Svc.java",
                Language::Java,
            );
            n.qualified_name = "com.example::Svc".to_string();
            n
        };
        let expected = {
            let mut n = node(
                "class:expect",
                "Svc",
                NodeKind::Class,
                "com/example/Svc.java",
                Language::Java,
            );
            n.qualified_name = "com.example::Svc".to_string();
            n.decorators = vec!["expect".to_string()];
            n
        };
        let ctx = TestContext {
            nodes: vec![plain, expected],
            ..Default::default()
        };
        let r = reference(
            "com.example.Svc",
            EdgeKind::Imports,
            "com/example/Caller.java",
            Language::Java,
        );
        let resolved = resolve_jvm_import(&r, &ctx).expect("resolves");
        assert_eq!(resolved.target_node_id, "class:expect");
    }

    #[test]
    fn resolve_via_import_cpp_sibling_include() {
        let sibling = file_node("src/util.h", Language::Cpp);
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            nodes: vec![sibling],
            ..Default::default()
        };
        let r = reference("util.h", EdgeKind::Imports, "src/main.cpp", Language::Cpp);
        let resolved = resolve_via_import(&r, &ctx).expect("resolves");
        assert_eq!(resolved.target_node_id, "file:src/util.h");
        assert_eq!(resolved.confidence, 0.92);
    }

    #[test]
    fn resolve_via_import_cpp_via_resolve_import_path() {
        let mut existing = BTreeSet::new();
        existing.insert("include/lib.h".to_string());
        let file = file_node("include/lib.h", Language::Cpp);
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            nodes: vec![file],
            cpp_include_dirs: vec!["include".to_string()],
            ..Default::default()
        };
        let r = reference("lib.h", EdgeKind::Imports, "src/main.cpp", Language::Cpp);
        let resolved = resolve_via_import(&r, &ctx).expect("resolves");
        assert_eq!(resolved.target_node_id, "file:include/lib.h");
        assert_eq!(resolved.confidence, 0.9);
    }

    #[test]
    fn resolve_via_import_cpp_unresolved_is_none() {
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            ..Default::default()
        };
        let r = reference(
            "missing.h",
            EdgeKind::Imports,
            "src/main.cpp",
            Language::Cpp,
        );
        assert!(resolve_via_import(&r, &ctx).is_none());
    }

    #[test]
    fn resolve_via_import_php_include_path() {
        let mut existing = BTreeSet::new();
        existing.insert("lib/helper.php".to_string());
        let file = file_node("lib/helper.php", Language::Php);
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            nodes: vec![file],
            ..Default::default()
        };
        let r = reference(
            "lib/helper.php",
            EdgeKind::Imports,
            "app.php",
            Language::Php,
        );
        let resolved = resolve_via_import(&r, &ctx).expect("resolves");
        assert_eq!(resolved.target_node_id, "file:lib/helper.php");
        assert_eq!(resolved.confidence, 0.9);
    }

    #[test]
    fn resolve_via_import_php_include_unresolved_is_none() {
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            ..Default::default()
        };
        let r = reference(
            "lib/missing.php",
            EdgeKind::Imports,
            "app.php",
            Language::Php,
        );
        assert!(resolve_via_import(&r, &ctx).is_none());
    }

    #[test]
    fn resolve_via_import_empty_imports_and_unreadable_file_is_none() {
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            ..Default::default()
        };
        let r = reference("foo", EdgeKind::Calls, "src/a.ts", Language::TypeScript);
        assert!(resolve_via_import(&r, &ctx).is_none());
    }

    #[test]
    fn resolve_via_import_generic_named_symbol() {
        let mut existing = BTreeSet::new();
        existing.insert("src/foo.ts".to_string());
        let mut import_mappings = BTreeMap::new();
        import_mappings.insert("src/a.ts".to_string(), vec![mapping("bar", "bar", "./foo")]);
        let target = exported(node(
            "function:bar",
            "bar",
            NodeKind::Function,
            "src/foo.ts",
            Language::TypeScript,
        ));
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            import_mappings,
            nodes: vec![target],
            ..Default::default()
        };
        let r = reference("bar", EdgeKind::Calls, "src/a.ts", Language::TypeScript);
        let resolved = resolve_via_import(&r, &ctx).expect("resolves");
        assert_eq!(resolved.target_node_id, "function:bar");
        assert_eq!(resolved.confidence, 0.9);
    }

    #[test]
    fn resolve_via_import_generic_default_import() {
        let mut existing = BTreeSet::new();
        existing.insert("src/foo.ts".to_string());
        let mut import_mappings = BTreeMap::new();
        import_mappings.insert(
            "src/a.ts".to_string(),
            vec![ImportMapping {
                local_name: "Foo".to_string(),
                exported_name: "default".to_string(),
                source: "./foo".to_string(),
                is_default: true,
                is_namespace: false,
            }],
        );
        let target = exported(node(
            "function:foo",
            "foo",
            NodeKind::Function,
            "src/foo.ts",
            Language::TypeScript,
        ));
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            import_mappings,
            nodes: vec![target],
            ..Default::default()
        };
        let r = reference("Foo", EdgeKind::Calls, "src/a.ts", Language::TypeScript);
        let resolved = resolve_via_import(&r, &ctx).expect("resolves");
        assert_eq!(resolved.target_node_id, "function:foo");
    }

    #[test]
    fn resolve_via_import_namespace_member() {
        let mut existing = BTreeSet::new();
        existing.insert("src/ns.ts".to_string());
        let mut import_mappings = BTreeMap::new();
        import_mappings.insert(
            "src/a.ts".to_string(),
            vec![ImportMapping {
                local_name: "ns".to_string(),
                exported_name: "*".to_string(),
                source: "./ns".to_string(),
                is_default: false,
                is_namespace: true,
            }],
        );
        let target = exported(node(
            "function:helper",
            "helper",
            NodeKind::Function,
            "src/ns.ts",
            Language::TypeScript,
        ));
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            import_mappings,
            nodes: vec![target],
            ..Default::default()
        };
        let r = reference(
            "ns.helper",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
        );
        let resolved = resolve_via_import(&r, &ctx).expect("resolves");
        assert_eq!(resolved.target_node_id, "function:helper");
    }

    #[test]
    fn resolve_via_import_static_member_descent() {
        let mut existing = BTreeSet::new();
        existing.insert("src/factory.ts".to_string());
        let mut import_mappings = BTreeMap::new();
        import_mappings.insert(
            "src/a.ts".to_string(),
            vec![mapping("Factory", "Factory", "./factory")],
        );
        let class_node = {
            let mut n = exported(node(
                "class:Factory",
                "Factory",
                NodeKind::Class,
                "src/factory.ts",
                Language::TypeScript,
            ));
            n.qualified_name = "Factory".to_string();
            n
        };
        let method = {
            let mut n = node(
                "method:create",
                "create",
                NodeKind::Method,
                "src/factory.ts",
                Language::TypeScript,
            );
            n.qualified_name = "Factory::create".to_string();
            n
        };
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            import_mappings,
            nodes: vec![class_node, method],
            ..Default::default()
        };
        let r = reference(
            "Factory.create",
            EdgeKind::Calls,
            "src/a.ts",
            Language::TypeScript,
        );
        let resolved = resolve_via_import(&r, &ctx).expect("resolves");
        assert_eq!(resolved.target_node_id, "method:create");
    }

    #[test]
    fn resolve_module_import_to_file_namespace() {
        let mut existing = BTreeSet::new();
        existing.insert("src/ns.ts".to_string());
        let mut import_mappings = BTreeMap::new();
        import_mappings.insert(
            "src/a.ts".to_string(),
            vec![ImportMapping {
                local_name: "ns".to_string(),
                exported_name: "*".to_string(),
                source: "./ns".to_string(),
                is_default: false,
                is_namespace: true,
            }],
        );
        let file = file_node("src/ns.ts", Language::TypeScript);
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            import_mappings,
            nodes: vec![file],
            ..Default::default()
        };
        let r = reference("ns", EdgeKind::Imports, "src/a.ts", Language::TypeScript);
        let resolved = resolve_via_import(&r, &ctx).expect("resolves");
        assert_eq!(resolved.target_node_id, "file:src/ns.ts");
    }

    #[test]
    fn resolve_go_cross_package_reference_resolves_exported_member() {
        let mut import_mappings = BTreeMap::new();
        import_mappings.insert(
            "main.go".to_string(),
            vec![ImportMapping {
                local_name: "pkga".to_string(),
                exported_name: "*".to_string(),
                source: "example.com/proj/pkga".to_string(),
                is_default: false,
                is_namespace: true,
            }],
        );
        let target = {
            let mut n = exported(node(
                "function:FuncX",
                "FuncX",
                NodeKind::Function,
                "pkga/x.go",
                Language::Go,
            ));
            n.is_exported = true;
            n
        };
        let mut existing = BTreeSet::new();
        existing.insert("pkga/x.go".to_string());
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            file_contents: {
                let mut m = BTreeMap::new();
                m.insert("main.go".to_string(), "package main".to_string());
                m
            },
            import_mappings,
            nodes: vec![target],
            go_module: Some(GoModule {
                module_path: "example.com/proj".to_string(),
            }),
            ..Default::default()
        };
        let r = reference("pkga.FuncX", EdgeKind::Calls, "main.go", Language::Go);
        let resolved = resolve_via_import(&r, &ctx).expect("resolves");
        assert_eq!(resolved.target_node_id, "function:FuncX");
    }

    #[test]
    fn resolve_python_module_member_resolves() {
        let mut existing = BTreeSet::new();
        existing.insert("pkg/mod.py".to_string());
        let mut import_mappings = BTreeMap::new();
        import_mappings.insert(
            "app.py".to_string(),
            vec![ImportMapping {
                local_name: "mod".to_string(),
                exported_name: "*".to_string(),
                source: "pkg.mod".to_string(),
                is_default: false,
                is_namespace: true,
            }],
        );
        let helper = node(
            "function:helper",
            "helper",
            NodeKind::Function,
            "pkg/mod.py",
            Language::Python,
        );
        let mod_file = {
            let mut n = file_node("pkg/mod.py", Language::Python);
            n.name = "mod.py".to_string();
            n
        };
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            file_contents: {
                let mut m = BTreeMap::new();
                m.insert("app.py".to_string(), "import mod".to_string());
                m
            },
            import_mappings,
            nodes: vec![helper, mod_file],
            ..Default::default()
        };
        let r = reference("mod.helper", EdgeKind::Calls, "app.py", Language::Python);
        let resolved = resolve_via_import(&r, &ctx).expect("resolves");
        assert_eq!(resolved.target_node_id, "function:helper");
        assert_eq!(resolved.confidence, 0.85);
    }

    #[test]
    fn resolve_python_absolute_module_import() {
        let mod_file = file_node("a/b/c.py", Language::Python);
        let name_file = {
            let mut n = mod_file.clone();
            n.name = "c.py".to_string();
            n
        };
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            file_contents: {
                let mut m = BTreeMap::new();
                m.insert("app.py".to_string(), "import a.b.c".to_string());
                m
            },
            nodes: vec![name_file],
            ..Default::default()
        };
        let r = reference("a.b.c", EdgeKind::Imports, "app.py", Language::Python);
        let resolved = resolve_via_import(&r, &ctx).expect("resolves");
        assert_eq!(resolved.target_node_id, "file:a/b/c.py");
    }

    #[test]
    fn resolve_rust_path_reference_via_crate_root() {
        let mut existing = BTreeSet::new();
        existing.insert("src/lib.rs".to_string());
        existing.insert("src/utils.rs".to_string());
        let leaf = node(
            "function:helper",
            "helper",
            NodeKind::Function,
            "src/utils.rs",
            Language::Rust,
        );
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            file_contents: {
                let mut m = BTreeMap::new();
                m.insert(
                    "src/main_mod.rs".to_string(),
                    "use crate::utils::helper;".to_string(),
                );
                m
            },
            nodes: vec![leaf],
            ..Default::default()
        };
        let r = reference(
            "crate::utils::helper",
            EdgeKind::Calls,
            "src/main_mod.rs",
            Language::Rust,
        );
        let resolved = resolve_via_import(&r, &ctx).expect("resolves");
        assert_eq!(resolved.target_node_id, "function:helper");
    }

    #[test]
    fn resolve_lua_require_dotted_module() {
        let mut existing = BTreeSet::new();
        existing.insert("game/lib/mod.lua".to_string());
        let file = file_node("game/lib/mod.lua", Language::Lua);
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            file_contents: {
                let mut m = BTreeMap::new();
                m.insert(
                    "game/main.lua".to_string(),
                    "require('lib.mod')".to_string(),
                );
                m
            },
            nodes: vec![file],
            ..Default::default()
        };
        let r = reference(
            "game.lib.mod",
            EdgeKind::Imports,
            "game/main.lua",
            Language::Lua,
        );
        let resolved = resolve_via_import(&r, &ctx).expect("resolves");
        assert_eq!(resolved.target_node_id, "file:game/lib/mod.lua");
    }

    #[test]
    fn resolve_java_imported_reference_qualified_member() {
        let mut import_mappings = BTreeMap::new();
        import_mappings.insert(
            "Caller.java".to_string(),
            vec![mapping("Utils", "Utils", "com.example.Utils")],
        );
        let member = node(
            "method:doIt",
            "doIt",
            NodeKind::Method,
            "com/example/Utils.java",
            Language::Java,
        );
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            file_contents: {
                let mut m = BTreeMap::new();
                m.insert(
                    "Caller.java".to_string(),
                    "import com.example.Utils;".to_string(),
                );
                m
            },
            import_mappings,
            nodes: vec![member],
            ..Default::default()
        };
        let r = reference("Utils.doIt", EdgeKind::Calls, "Caller.java", Language::Java);
        let resolved = resolve_via_import(&r, &ctx).expect("resolves");
        assert_eq!(resolved.target_node_id, "method:doIt");
    }

    #[test]
    fn find_exported_symbol_follows_named_reexport_chain() {
        let mut existing = BTreeSet::new();
        existing.insert("src/index.ts".to_string());
        existing.insert("src/impl.ts".to_string());
        let mut re_exports = BTreeMap::new();
        re_exports.insert(
            "src/index.ts".to_string(),
            vec![ReExport::Named {
                exported_name: "Widget".to_string(),
                original_name: "Widget".to_string(),
                source: "./impl".to_string(),
            }],
        );
        let target = exported(node(
            "class:Widget",
            "Widget",
            NodeKind::Class,
            "src/impl.ts",
            Language::TypeScript,
        ));
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            re_exports,
            nodes: vec![target],
            ..Default::default()
        };
        let want = Want {
            is_default: false,
            is_namespace: false,
            exported_name: "Widget".to_string(),
            member_name: None,
        };
        let found = find_exported_symbol(
            "src/index.ts",
            &want,
            Language::TypeScript,
            &ctx,
            &mut BTreeSet::new(),
            0,
        )
        .expect("found via re-export");
        assert_eq!(found.id, "class:Widget");
    }

    #[test]
    fn find_exported_symbol_follows_wildcard_reexport() {
        let mut existing = BTreeSet::new();
        existing.insert("src/index.ts".to_string());
        existing.insert("src/impl.ts".to_string());
        let mut re_exports = BTreeMap::new();
        re_exports.insert(
            "src/index.ts".to_string(),
            vec![ReExport::Wildcard {
                source: "./impl".to_string(),
            }],
        );
        let target = exported(node(
            "function:go",
            "go",
            NodeKind::Function,
            "src/impl.ts",
            Language::TypeScript,
        ));
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            existing_files: existing,
            re_exports,
            nodes: vec![target],
            ..Default::default()
        };
        let want = Want {
            is_default: false,
            is_namespace: false,
            exported_name: "go".to_string(),
            member_name: None,
        };
        let found = find_exported_symbol(
            "src/index.ts",
            &want,
            Language::TypeScript,
            &ctx,
            &mut BTreeSet::new(),
            0,
        )
        .expect("found via wildcard");
        assert_eq!(found.id, "function:go");
    }

    #[test]
    fn find_exported_symbol_respects_depth_and_visited_guards() {
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            ..Default::default()
        };
        let want = Want {
            is_default: false,
            is_namespace: false,
            exported_name: "x".to_string(),
            member_name: None,
        };
        assert!(
            find_exported_symbol(
                "src/a.ts",
                &want,
                Language::TypeScript,
                &ctx,
                &mut BTreeSet::new(),
                REEXPORT_MAX_DEPTH + 1,
            )
            .is_none()
        );

        let mut visited = BTreeSet::new();
        visited.insert("src/a.ts".to_string());
        assert!(
            find_exported_symbol(
                "src/a.ts",
                &want,
                Language::TypeScript,
                &ctx,
                &mut visited,
                0,
            )
            .is_none()
        );
    }

    #[test]
    fn find_exported_symbol_default_and_namespace_hits() {
        let comp = exported(node(
            "component:C",
            "C",
            NodeKind::Component,
            "src/c.tsx",
            Language::Tsx,
        ));
        let ctx = TestContext {
            project_root: "/proj".to_string(),
            nodes: vec![comp],
            ..Default::default()
        };
        let want_default = Want {
            is_default: true,
            is_namespace: false,
            exported_name: "default".to_string(),
            member_name: None,
        };
        let found = find_exported_symbol(
            "src/c.tsx",
            &want_default,
            Language::Tsx,
            &ctx,
            &mut BTreeSet::new(),
            0,
        )
        .expect("default hit");
        assert_eq!(found.id, "component:C");

        let member = exported(node(
            "function:m",
            "m",
            NodeKind::Function,
            "src/ns.ts",
            Language::TypeScript,
        ));
        let ctx2 = TestContext {
            project_root: "/proj".to_string(),
            nodes: vec![member],
            ..Default::default()
        };
        let want_ns = Want {
            is_default: false,
            is_namespace: true,
            exported_name: "*".to_string(),
            member_name: Some("m".to_string()),
        };
        let found2 = find_exported_symbol(
            "src/ns.ts",
            &want_ns,
            Language::TypeScript,
            &ctx2,
            &mut BTreeSet::new(),
            0,
        )
        .expect("namespace member hit");
        assert_eq!(found2.id, "function:m");
    }

    #[test]
    fn resolve_static_member_declines_non_container() {
        let container = node(
            "function:f",
            "f",
            NodeKind::Function,
            "src/a.ts",
            Language::TypeScript,
        );
        let ctx = TestContext::default();
        let r = reference("f.x", EdgeKind::Calls, "src/a.ts", Language::TypeScript);
        assert!(resolve_static_member(&container, &r, "f", &ctx).is_none());
    }

    #[test]
    fn resolve_static_member_prefers_callable_for_calls() {
        let container = {
            let mut n = node(
                "class:C",
                "C",
                NodeKind::Class,
                "src/c.ts",
                Language::TypeScript,
            );
            n.qualified_name = "C".to_string();
            n
        };
        let method = {
            let mut n = node(
                "method:m",
                "m",
                NodeKind::Method,
                "src/c.ts",
                Language::TypeScript,
            );
            n.qualified_name = "C::m".to_string();
            n
        };
        let prop = {
            let mut n = node(
                "variable:m",
                "m",
                NodeKind::Variable,
                "src/c.ts",
                Language::TypeScript,
            );
            n.qualified_name = "C::m".to_string();
            n
        };
        let ctx = TestContext {
            nodes: vec![prop, method],
            ..Default::default()
        };
        let r = reference("C.m", EdgeKind::Calls, "src/x.ts", Language::TypeScript);
        let resolved = resolve_static_member(&container, &r, "C", &ctx).expect("resolves");
        assert_eq!(resolved.kind, NodeKind::Method);
    }

    #[test]
    fn resolve_static_member_no_match_is_none() {
        let container = {
            let mut n = node(
                "class:C",
                "C",
                NodeKind::Class,
                "src/c.ts",
                Language::TypeScript,
            );
            n.qualified_name = "C".to_string();
            n
        };
        let ctx = TestContext::default();
        let r = reference(
            "C.missing",
            EdgeKind::Calls,
            "src/x.ts",
            Language::TypeScript,
        );
        assert!(resolve_static_member(&container, &r, "C", &ctx).is_none());
    }

    #[test]
    fn is_static_member_container_classification() {
        assert!(is_static_member_container(NodeKind::Class));
        assert!(is_static_member_container(NodeKind::Struct));
        assert!(is_static_member_container(NodeKind::Interface));
        assert!(is_static_member_container(NodeKind::Enum));
        assert!(is_static_member_container(NodeKind::Trait));
        assert!(is_static_member_container(NodeKind::Protocol));
        assert!(!is_static_member_container(NodeKind::Function));
    }

    #[test]
    fn drop_last_segment_behavior() {
        assert_eq!(drop_last_segment("a/b/c"), vec!["a", "b"]);
        assert_eq!(drop_last_segment("only"), Vec::<&str>::new());
    }

    #[test]
    fn find_python_module_file_via_module_and_init() {
        let mod_file = {
            let mut n = file_node("a/b/c.py", Language::Python);
            n.name = "c.py".to_string();
            n
        };
        let ctx = TestContext {
            nodes: vec![mod_file],
            ..Default::default()
        };
        assert_eq!(
            find_python_module_file("a.b.c", &ctx, "app.py"),
            Some("a/b/c.py".to_string())
        );

        let init_file = {
            let mut n = file_node("a/b/__init__.py", Language::Python);
            n.name = "__init__.py".to_string();
            n
        };
        let ctx2 = TestContext {
            nodes: vec![init_file],
            ..Default::default()
        };
        assert_eq!(
            find_python_module_file("a.b", &ctx2, "app.py"),
            Some("a/b/__init__.py".to_string())
        );
    }

    #[test]
    fn find_python_module_file_rejects_empty_and_relative() {
        let ctx = TestContext::default();
        assert!(find_python_module_file("", &ctx, "app.py").is_none());
        assert!(find_python_module_file(".rel", &ctx, "app.py").is_none());
    }

    #[test]
    fn rust_self_module_dir_variants() {
        assert_eq!(rust_self_module_dir("src/mod.rs"), "src");
        assert_eq!(rust_self_module_dir("src/lib.rs"), "src");
        assert_eq!(rust_self_module_dir("src/foo.rs"), "src/foo");
        assert_eq!(rust_self_module_dir("foo.rs"), "foo");
    }

    #[test]
    fn rust_crate_root_dir_walks_up() {
        let mut existing = BTreeSet::new();
        existing.insert("src/lib.rs".to_string());
        let ctx = TestContext {
            existing_files: existing,
            ..Default::default()
        };
        assert_eq!(
            rust_crate_root_dir("src/deep/mod.rs", &ctx),
            Some("src".to_string())
        );

        let empty_ctx = TestContext::default();
        assert!(rust_crate_root_dir("src/deep/mod.rs", &empty_ctx).is_none());
    }

    #[test]
    fn resolve_rust_module_file_self_super_and_bare() {
        let mut existing = BTreeSet::new();
        existing.insert("src/lib.rs".to_string());
        existing.insert("src/app/sub.rs".to_string());
        existing.insert("src/sibling.rs".to_string());
        let ctx = TestContext {
            existing_files: existing,
            ..Default::default()
        };
        assert_eq!(
            resolve_rust_module_file(&["self", "sub"], "src/app.rs", &ctx),
            Some("src/app/sub.rs".to_string())
        );
        assert_eq!(
            resolve_rust_module_file(&["super", "sibling"], "src/app/mod.rs", &ctx),
            Some("src/sibling.rs".to_string())
        );
        assert!(resolve_rust_module_file(&[], "src/app.rs", &ctx).is_none());
    }
}
