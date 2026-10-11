//! Which bare JS-family import specifiers name a package from outside the
//! repository (upstream #2128 `isDeclaredOutsidePackage` / `manifestScope`).
//!
//! A specifier is outside only when the importing file's package.json, or an
//! enclosing one, declares it (a Deno import map counts), or it is a Node
//! built-in, `@std/…`, a runtime scheme (`node:`, `npm:`, …) or a framework's
//! virtual module. A specifier nothing declares is an alias this resolver
//! does not follow (SvelteKit's `$lib/…`, a nested Nuxt app's `~/…`, a
//! nested app's own `@/…`) and stays the project's. Manifests are read by
//! project-relative path through [`ResolutionContext::read_file_shared`], the
//! bounded, stat-first reader, from directories of indexed files only.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::types::ResolutionContext;

/// The package names a directory's package.json and every enclosing one own
/// (`name`, and a `workspace:` / `file:` / `link:` / `portal:` dependency)
/// and depend on, Deno import-map entries included.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ManifestScope {
    pub own: BTreeSet<String>,
    pub deps: BTreeSet<String>,
}

/// Node's built-in modules without the `node:` scheme
/// (`require('module').builtinModules` across Node 20 to 26: upstream's
/// engines range and the runtime it was measured on). Sorted for binary
/// search.
const NODE_BUILTIN_MODULES: [&str; 68] = [
    "_http_agent",
    "_http_client",
    "_http_common",
    "_http_incoming",
    "_http_outgoing",
    "_http_server",
    "_stream_duplex",
    "_stream_passthrough",
    "_stream_readable",
    "_stream_transform",
    "_stream_wrap",
    "_stream_writable",
    "_tls_common",
    "_tls_wrap",
    "assert",
    "assert/strict",
    "async_hooks",
    "buffer",
    "child_process",
    "cluster",
    "console",
    "constants",
    "crypto",
    "dgram",
    "diagnostics_channel",
    "dns",
    "dns/promises",
    "domain",
    "events",
    "fs",
    "fs/promises",
    "http",
    "http2",
    "https",
    "inspector",
    "inspector/promises",
    "module",
    "net",
    "os",
    "path",
    "path/posix",
    "path/win32",
    "perf_hooks",
    "process",
    "punycode",
    "querystring",
    "readline",
    "readline/promises",
    "repl",
    "stream",
    "stream/consumers",
    "stream/promises",
    "stream/web",
    "string_decoder",
    "sys",
    "timers",
    "timers/promises",
    "tls",
    "trace_events",
    "tty",
    "url",
    "util",
    "util/types",
    "v8",
    "vm",
    "wasi",
    "worker_threads",
    "zlib",
];

/// Is `source`, imported by `from_file`, a package from outside the
/// repository? See the module documentation.
pub fn is_declared_outside_package(
    source: &str,
    from_file: &str,
    context: &dyn ResolutionContext,
) -> bool {
    // Deno's standard library is `@std/…` from JSR.
    if has_runtime_scheme(source)
        || source.starts_with("@std/")
        || NODE_BUILTIN_MODULES.binary_search(&source).is_ok()
    {
        return true;
    }
    let name = package_name(source);
    let file = from_file.replace('\\', "/");
    let scope = importing_dir(&file)
        .map(|dir| context.manifest_scope(dir))
        .unwrap_or_default();
    if scope.own.contains(name) {
        return false;
    }
    if scope.deps.contains(name) {
        return true;
    }
    // SvelteKit's `$app/…` and Astro's `astro:…` belong to the framework,
    // unless this repository is that framework.
    let provider = if source.starts_with("astro:") {
        Some("astro")
    } else if is_sveltekit_virtual(source) {
        Some("@sveltejs/kit")
    } else {
        None
    };
    provider.is_some_and(|provider| {
        !scope.own.contains(provider)
            && !context
                .get_workspace_packages()
                .is_some_and(|ws| ws.by_name.contains_key(provider))
    })
}

/// `dir`'s manifest scope: its parent's, plus what `dir/package.json`,
/// `dir/deno.json` and `dir/deno.jsonc` declare. The parent comes from
/// [`ResolutionContext::manifest_scope`], so a caching context reads each
/// directory once per pass.
pub fn read_manifest_scope<C: ResolutionContext + ?Sized>(dir: &str, context: &C) -> ManifestScope {
    let mut scope = if dir.is_empty() {
        ManifestScope::default()
    } else {
        let parent = dir.rsplit_once('/').map_or("", |(parent, _)| parent);
        context.manifest_scope(parent).as_ref().clone()
    };
    let at = |file: &str| {
        if dir.is_empty() {
            file.to_string()
        } else {
            format!("{dir}/{file}")
        }
    };
    if let Some(Value::Object(json)) = context
        .read_file_shared(&at("package.json"))
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    {
        if let Some(name) = json
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
        {
            scope.own.insert(name.to_string());
        }
        for field in [
            "dependencies",
            "devDependencies",
            "peerDependencies",
            "optionalDependencies",
        ] {
            let Some(deps) = json.get(field).and_then(Value::as_object) else {
                continue;
            };
            for (dep, version) in deps {
                // `workspace:*`, `file:../shared`, `link:…`: a package in
                // this repository.
                if version.as_str().is_some_and(is_in_repo_version) {
                    scope.own.insert(dep.clone());
                } else {
                    scope.deps.insert(dep.clone());
                }
            }
        }
    }
    // A Deno import map names packages the same way: an entry that maps to a
    // registry or a URL, not one that maps to a path in the repository.
    for file in ["deno.json", "deno.jsonc"] {
        let Some(raw) = context.read_file_shared(&at(file)) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<Value>(&crate::path_aliases::strip_jsonc(&raw))
        else {
            continue;
        };
        let Some(imports) = json.get("imports").and_then(Value::as_object) else {
            continue;
        };
        for (key, target) in imports {
            if target.as_str().is_some_and(is_registry_target) {
                scope
                    .deps
                    .insert(key.strip_suffix('/').unwrap_or(key).to_string());
            }
        }
    }
    scope
}

/// The directory a project-relative file sits in (`""` at the root), or
/// `None` for a path that could leave the project (absolute, or with a `..`
/// segment), whose manifests are never read.
fn importing_dir(file: &str) -> Option<&str> {
    if file.starts_with('/')
        || file.as_bytes().get(1) == Some(&b':')
        || file.split('/').any(|segment| segment == "..")
    {
        return None;
    }
    Some(file.rsplit_once('/').map_or("", |(dir, _)| dir))
}

/// `@scope/name` of a scoped specifier, else its first segment.
fn package_name(source: &str) -> &str {
    if source.starts_with('@') {
        let mut slashes = source.match_indices('/').map(|(at, _)| at);
        slashes
            .nth(1)
            .map_or(source, |second_slash| &source[..second_slash])
    } else {
        source.split('/').next().unwrap_or(source)
    }
}

/// `node:`, `bun:`, `jsr:`, `npm:`, `http:` or `https:`.
fn has_runtime_scheme(source: &str) -> bool {
    ["node:", "bun:", "jsr:", "npm:", "http:", "https:"]
        .iter()
        .any(|scheme| source.starts_with(scheme))
}

/// `jsr:`, `npm:`, `http:` or `https:`: an import-map target outside the
/// repository.
fn is_registry_target(target: &str) -> bool {
    ["jsr:", "npm:", "http:", "https:"]
        .iter()
        .any(|scheme| target.starts_with(scheme))
}

/// `workspace:`, `file:`, `link:` or `portal:`: a dependency in the
/// repository.
fn is_in_repo_version(version: &str) -> bool {
    ["workspace:", "file:", "link:", "portal:"]
        .iter()
        .any(|protocol| version.starts_with(protocol))
}

/// SvelteKit's `$app`, `$env` and `$service-worker` modules.
fn is_sveltekit_virtual(source: &str) -> bool {
    ["$app", "$env", "$service-worker"].iter().any(|module| {
        source
            .strip_prefix(module)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

#[cfg(test)]
mod tests {
    use super::{NODE_BUILTIN_MODULES, importing_dir, is_sveltekit_virtual, package_name};

    #[test]
    fn node_builtins_are_sorted_for_binary_search() {
        assert!(
            NODE_BUILTIN_MODULES
                .windows(2)
                .all(|pair| pair[0] < pair[1])
        );
        assert!(NODE_BUILTIN_MODULES.binary_search(&"fs/promises").is_ok());
        assert!(NODE_BUILTIN_MODULES.binary_search(&"lodash").is_err());
    }

    #[test]
    fn package_names_keep_their_scope() {
        assert_eq!(package_name("@app/shared/utils"), "@app/shared");
        assert_eq!(package_name("@app/shared"), "@app/shared");
        assert_eq!(package_name("@app"), "@app");
        assert_eq!(package_name("dayjs/plugin/utc"), "dayjs");
        assert_eq!(package_name("$lib/design"), "$lib");
    }

    #[test]
    fn sveltekit_virtual_modules_are_whole_segments() {
        assert!(is_sveltekit_virtual("$app/navigation"));
        assert!(is_sveltekit_virtual("$env/static/private"));
        assert!(is_sveltekit_virtual("$app"));
        assert!(!is_sveltekit_virtual("$application/x"));
        assert!(!is_sveltekit_virtual("$lib/design"));
    }

    #[test]
    fn only_project_relative_files_name_a_manifest_directory() {
        assert_eq!(
            importing_dir("frontend/pages/labels.vue"),
            Some("frontend/pages")
        );
        assert_eq!(importing_dir("app.ts"), Some(""));
        assert_eq!(importing_dir("/etc/app.ts"), None);
        assert_eq!(importing_dir("C:/app.ts"), None);
        assert_eq!(importing_dir("../outside/app.ts"), None);
        assert_eq!(importing_dir("a/../../b.ts"), None);
    }
}
