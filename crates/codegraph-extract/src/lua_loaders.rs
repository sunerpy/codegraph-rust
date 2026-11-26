//! A project's custom Lua module loaders (upstream issue #1617, OD-11).
//!
//! A Lua code base can load its modules through a function of its own,
//! `Require("Script/Activity/ActivityMgrC.lua")` or `Include "ui.button"`,
//! where other code calls `require`. The project names those functions, and
//! the folders its modules are looked up in first, in the `[lua]` table of
//! its `config.toml`:
//!
//! ```toml
//! [lua]
//! loader_functions = ["Require", "Include"]
//! module_roots = ["Script"]
//! ```
//!
//! or in the `lua` object of its `codegraph.json`
//! (`{"lua": {"loaderFunctions": [...], "moduleRoots": [...]}}`).
//!
//! Both readers keep the block as it was written
//! ([`codegraph_core::config::Config::lua`],
//! [`crate::ExtensionOverrides::lua_block`]) and [`LuaLoaders::merge`]
//! validates it here, so a mistyped block never fails the rest of either
//! file. Each key is taken whole from `config.toml` when that file writes
//! it as an array, an empty one included, and from `codegraph.json`
//! otherwise; the two lists are never joined. Every element that is not a
//! valid name or folder is dropped with a warning on stderr that names it,
//! a block that is no table and a key that is no array are ignored with a
//! warning, and indexing goes on.
//!
//! The result is canonical: the loader names are a set (sorted, each once),
//! the roots a list in the order written (each once, by its normalized
//! form). A project that names neither is [unconfigured](LuaLoaders::is_configured),
//! and its graph is the one a build without this module gives.

use std::sync::Arc;

use serde_json::Value;

/// One project's validated Lua loader settings.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LuaLoaders {
    /// The loader names, sorted, each once: `Require`, `Script.Load`.
    loader_functions: Vec<String>,
    /// The module roots in the order written, each once, normalized: `/`
    /// separated, no `./` before or `/` after, `""` for the project root.
    module_roots: Vec<String>,
}

/// The settings of a project that names none.
static UNCONFIGURED: LuaLoaders = LuaLoaders {
    loader_functions: Vec::new(),
    module_roots: Vec::new(),
};

/// Where a settings block was written, as the warnings name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Toml,
    Json,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Self::Toml => "config.toml [lua]",
            Self::Json => "codegraph.json lua",
        }
    }

    fn loaders_key(self) -> &'static str {
        match self {
            Self::Toml => "loader_functions",
            Self::Json => "loaderFunctions",
        }
    }

    fn roots_key(self) -> &'static str {
        match self {
            Self::Toml => "module_roots",
            Self::Json => "moduleRoots",
        }
    }
}

/// The two lists one block writes, each `None` when the block does not
/// write it as an array.
#[derive(Debug, Default)]
struct Written {
    loader_functions: Option<Vec<String>>,
    module_roots: Option<Vec<String>>,
}

impl LuaLoaders {
    /// The settings of a project that names no loader and no root.
    #[must_use]
    pub fn empty() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// A shared reference to the unconfigured settings.
    #[must_use]
    pub fn unconfigured() -> &'static Self {
        &UNCONFIGURED
    }

    /// Validate and merge the `[lua]` table of `config.toml` (`toml`) and
    /// the `lua` object of `codegraph.json` (`json`), as their readers kept
    /// them: per key, `config.toml`'s array replaces `codegraph.json`'s
    /// whole. Every element, key or block that is dropped is named in a
    /// warning on stderr.
    #[must_use]
    pub fn merge(toml: Option<&Value>, json: Option<&Value>) -> Self {
        let toml = toml
            .map(|block| written(block, Source::Toml))
            .unwrap_or_default();
        let json = json
            .map(|block| written(block, Source::Json))
            .unwrap_or_default();
        let mut loader_functions = toml
            .loader_functions
            .or(json.loader_functions)
            .unwrap_or_default();
        loader_functions.sort();
        loader_functions.dedup();
        let module_roots = toml.module_roots.or(json.module_roots).unwrap_or_default();
        Self {
            loader_functions,
            module_roots,
        }
    }

    /// Settings from already valid lists, as [`Self::merge`] would leave
    /// them: for tests and for reading a stored record back.
    fn from_lists(loader_functions: Vec<String>, module_roots: Vec<String>) -> Self {
        let mut loader_functions: Vec<String> = loader_functions
            .into_iter()
            .filter(|name| is_loader_name(name))
            .collect();
        loader_functions.sort();
        loader_functions.dedup();
        let mut roots: Vec<String> = Vec::new();
        for root in module_roots {
            if let Ok(root) = normalize_root(&root)
                && !roots.contains(&root)
            {
                roots.push(root);
            }
        }
        Self {
            loader_functions,
            module_roots: roots,
        }
    }

    /// Whether the project names at least one loader or one root. Only then
    /// does a module literal count as a file path as written (OD-B83).
    #[must_use]
    pub fn is_configured(&self) -> bool {
        !self.loader_functions.is_empty() || !self.module_roots.is_empty()
    }

    /// The loader names, sorted.
    #[must_use]
    pub fn loader_functions(&self) -> &[String] {
        &self.loader_functions
    }

    /// The module roots in the order written, `""` for the project root.
    #[must_use]
    pub fn module_roots(&self) -> &[String] {
        &self.module_roots
    }

    /// Whether `callee`, a call's function as a dotted path of names
    /// (`Require`, `Script.Load`), is one of the loaders: exactly, case
    /// included.
    #[must_use]
    pub fn is_loader(&self, callee: &str) -> bool {
        self.loader_functions
            .binary_search_by(|name| name.as_str().cmp(callee))
            .is_ok()
    }

    /// The canonical text of these settings, which a full index records and
    /// a sync compares: `{"loaderFunctions":[…],"moduleRoots":[…]}`.
    #[must_use]
    pub fn canonical_json(&self) -> String {
        serde_json::json!({
            "loaderFunctions": self.loader_functions,
            "moduleRoots": self.module_roots,
        })
        .to_string()
    }

    /// The settings a [`Self::canonical_json`] text records, `None` for a
    /// text that is not one.
    #[must_use]
    pub fn from_canonical_json(text: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(text).ok()?;
        let object = value.as_object()?;
        if object.len() != 2 {
            return None;
        }
        let strings = |key: &str| -> Option<Vec<String>> {
            object
                .get(key)?
                .as_array()?
                .iter()
                .map(|element| element.as_str().map(str::to_string))
                .collect()
        };
        let settings = Self::from_lists(strings("loaderFunctions")?, strings("moduleRoots")?);
        // Only the text this build writes reads back: anything else is a
        // record to rebuild.
        (settings.canonical_json() == text).then_some(settings)
    }
}

/// What `block`, written in `source`, gives each key, every dropped part
/// named in a warning.
fn written(block: &Value, source: Source) -> Written {
    let Some(table) = block.as_object() else {
        tracing::warn!(
            target: "codegraph_extract::lua_loaders",
            source = source.label(),
            value = %block,
            "ignoring the Lua loader settings: expected a table"
        );
        return Written::default();
    };
    let mut out = Written::default();
    for (key, value) in table {
        if key == source.loaders_key() {
            out.loader_functions = elements(value, source, key).map(|elements| {
                elements
                    .iter()
                    .filter_map(|element| loader_name(element, source, key))
                    .collect()
            });
        } else if key == source.roots_key() {
            out.module_roots = elements(value, source, key).map(|elements| {
                let mut roots: Vec<String> = Vec::new();
                for element in elements {
                    if let Some(root) = module_root(element, source, key)
                        && !roots.contains(&root)
                    {
                        roots.push(root);
                    }
                }
                roots
            });
        } else {
            tracing::warn!(
                target: "codegraph_extract::lua_loaders",
                source = source.label(),
                key = %key,
                expected = %format!("{} or {}", source.loaders_key(), source.roots_key()),
                "ignoring an unknown Lua loader setting"
            );
        }
    }
    out
}

/// The elements of a key's array, `None` (with a warning) when it is no
/// array.
fn elements<'v>(value: &'v Value, source: Source, key: &str) -> Option<&'v Vec<Value>> {
    let array = value.as_array();
    if array.is_none() {
        tracing::warn!(
            target: "codegraph_extract::lua_loaders",
            source = source.label(),
            key = %key,
            value = %value,
            "ignoring a Lua loader setting: expected an array"
        );
    }
    array
}

/// A loader name element, `None` (with a warning) when it is not one.
fn loader_name(element: &Value, source: Source, key: &str) -> Option<String> {
    match element.as_str() {
        Some(name) if is_loader_name(name) => Some(name.to_string()),
        _ => {
            tracing::warn!(
                target: "codegraph_extract::lua_loaders",
                source = source.label(),
                key = %key,
                element = %element,
                "ignoring an invalid Lua loader name: expected a name or a dotted path of names"
            );
            None
        }
    }
}

/// A module root element, normalized, `None` (with a warning) when it is not
/// one.
fn module_root(element: &Value, source: Source, key: &str) -> Option<String> {
    let checked = match element.as_str() {
        Some(root) => normalize_root(root),
        None => Err("expected a string"),
    };
    match checked {
        Ok(root) => Some(root),
        Err(reason) => {
            tracing::warn!(
                target: "codegraph_extract::lua_loaders",
                source = source.label(),
                key = %key,
                element = %element,
                reason,
                "ignoring an invalid Lua module root"
            );
            None
        }
    }
}

/// Whether `name` is a Lua name or a dotted path of them, as a call's
/// function is written: `[A-Za-z_][A-Za-z0-9_]*`, then any `.` and another,
/// ASCII only, nothing trimmed.
fn is_loader_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('.').all(|part| {
            part.bytes()
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
                && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
}

/// `root` as a project-relative folder: one leading `./` and one trailing
/// `/` dropped, `""` for the project root (`.`, `./`, `""`). Lexical only: the
/// folder is never looked at on disk, and a root that is absolute (`/x`,
/// `C:`), holds a `\`, or has an empty, `.` or `..` segment is refused.
fn normalize_root(root: &str) -> Result<String, &'static str> {
    if root.contains('\\') {
        return Err("a backslash is not a path separator here; write `/`");
    }
    if root.starts_with('/') {
        return Err("an absolute path is outside the project");
    }
    let bytes = root.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err("a drive path is outside the project");
    }
    let rest = root.strip_prefix("./").unwrap_or(root);
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.is_empty() || rest == "." {
        return Ok(String::new());
    }
    if rest
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err("expected a project-relative folder without empty, `.` or `..` segments");
    }
    Ok(rest.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn merged(toml: Option<Value>, json: Option<Value>) -> LuaLoaders {
        LuaLoaders::merge(toml.as_ref(), json.as_ref())
    }

    #[test]
    fn loader_names_follow_the_lua_name_rule() {
        for good in ["Require", "_load", "Include2", "Script.Load", "a.b_c.D9"] {
            assert!(is_loader_name(good), "{good}");
        }
        for bad in [
            "",
            "1Require",
            "Re quire",
            " Require",
            "Require ",
            "Script.",
            ".Load",
            "Script..Load",
            "Script:Load",
            "Script[1]",
            "Requíre",
            "Require()",
        ] {
            assert!(!is_loader_name(bad), "{bad:?}");
        }
        let settings = merged(
            Some(json!({"loader_functions": ["Require", "1bad", 7, null, "Script.Load"]})),
            None,
        );
        assert_eq!(settings.loader_functions(), ["Require", "Script.Load"]);
    }

    #[test]
    fn module_roots_must_be_project_relative_folders() {
        for (root, normalized) in [
            ("Script", "Script"),
            ("Script/", "Script"),
            ("./Script", "Script"),
            ("./Script/", "Script"),
            ("Game/Script", "Game/Script"),
            (".", ""),
            ("./", ""),
            ("./.", ""),
            ("", ""),
            (" Script", " Script"),
        ] {
            assert_eq!(normalize_root(root).as_deref(), Ok(normalized), "{root:?}");
        }
        for bad in [
            "/",
            "/abs",
            "C:",
            "c:/x",
            "Script\\Lua",
            "..",
            "../x",
            "a/../b",
            "a/./b",
            "a//b",
            "Script//",
            "././x",
        ] {
            assert!(normalize_root(bad).is_err(), "{bad:?}");
        }
        let settings = merged(
            Some(json!({"module_roots": ["Script", "/abs", 3, "./Lib/", "../out"]})),
            None,
        );
        assert_eq!(settings.module_roots(), ["Script", "Lib"]);
    }

    #[test]
    fn entries_are_deduplicated_by_first_occurrence() {
        let settings = merged(
            Some(json!({
                "loader_functions": ["Include", "Require", "Include"],
                "module_roots": ["Script", "Lib", "./Script/", "Lib/", "."],
            })),
            None,
        );
        assert_eq!(settings.loader_functions(), ["Include", "Require"]);
        assert_eq!(settings.module_roots(), ["Script", "Lib", ""]);
    }

    #[test]
    fn a_toml_key_replaces_the_json_key_whole() {
        let json_block = json!({
            "loaderFunctions": ["Include"],
            "moduleRoots": ["Lib"],
        });
        // TOML writes only the loaders: they replace JSON's, the roots stay
        // JSON's.
        let settings = merged(
            Some(json!({"loader_functions": ["Require"]})),
            Some(json_block.clone()),
        );
        assert_eq!(settings.loader_functions(), ["Require"]);
        assert_eq!(settings.module_roots(), ["Lib"]);
        // An empty TOML array is a list written too.
        let settings = merged(Some(json!({"module_roots": []})), Some(json_block.clone()));
        assert_eq!(settings.loader_functions(), ["Include"]);
        assert!(settings.module_roots().is_empty());
        // So is one whose every element is dropped.
        let settings = merged(
            Some(json!({"loader_functions": ["1bad"]})),
            Some(json_block.clone()),
        );
        assert!(settings.loader_functions().is_empty());
        // JSON alone, and neither.
        let settings = merged(None, Some(json_block));
        assert_eq!(settings.loader_functions(), ["Include"]);
        assert_eq!(settings.module_roots(), ["Lib"]);
        assert_eq!(merged(None, None), LuaLoaders::default());
    }

    #[test]
    fn a_mistyped_block_or_key_is_ignored() {
        let json_block = json!({"loaderFunctions": ["Include"], "moduleRoots": ["Lib"]});
        // A block that is no table is as if unwritten.
        for block in [json!("Require"), json!(["Require"]), json!(3), json!(true)] {
            assert_eq!(
                merged(Some(block), Some(json_block.clone())),
                merged(None, Some(json_block.clone()))
            );
        }
        // A key that is no array is as if unwritten: the other file's applies.
        let settings = merged(
            Some(json!({"loader_functions": "Require", "module_roots": {"a": 1}})),
            Some(json_block.clone()),
        );
        assert_eq!(settings.loader_functions(), ["Include"]);
        assert_eq!(settings.module_roots(), ["Lib"]);
        // TOML's key names in JSON, and JSON's in TOML, are unknown keys.
        let settings = merged(
            Some(json!({"loaderFunctions": ["Require"]})),
            Some(json!({"loader_functions": ["Include"]})),
        );
        assert_eq!(settings, LuaLoaders::default());
        assert!(!settings.is_configured());
    }

    #[test]
    fn the_canonical_form_sorts_loaders_and_keeps_root_order() {
        let a = merged(
            Some(
                json!({"loader_functions": ["Require", "Include"], "module_roots": ["Script", "Lib"]}),
            ),
            None,
        );
        let b = merged(
            Some(
                json!({"loader_functions": ["Include", "Require", "Require"], "module_roots": ["Script", "Lib"]}),
            ),
            None,
        );
        let c = merged(
            Some(
                json!({"loader_functions": ["Include", "Require"], "module_roots": ["Lib", "Script"]}),
            ),
            None,
        );
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(
            a.canonical_json(),
            r#"{"loaderFunctions":["Include","Require"],"moduleRoots":["Script","Lib"]}"#
        );
        assert_eq!(
            LuaLoaders::from_canonical_json(&a.canonical_json()),
            Some(a.clone())
        );
        assert_eq!(
            LuaLoaders::default().canonical_json(),
            r#"{"loaderFunctions":[],"moduleRoots":[]}"#
        );
        for unreadable in [
            "",
            "{",
            "[]",
            r#"{"loaderFunctions":[]}"#,
            r#"{"loaderFunctions":["Require","Include"],"moduleRoots":[]}"#,
            r#"{"loaderFunctions":["1bad"],"moduleRoots":[]}"#,
            r#"{"loaderFunctions":[],"moduleRoots":[],"extra":1}"#,
            r#"{"moduleRoots":[],"loaderFunctions":[]}"#,
        ] {
            assert_eq!(
                LuaLoaders::from_canonical_json(unreadable),
                None,
                "{unreadable}"
            );
        }
    }

    #[test]
    fn no_settings_is_unconfigured() {
        assert!(!LuaLoaders::default().is_configured());
        assert!(!LuaLoaders::unconfigured().is_configured());
        assert!(!merged(Some(json!({})), Some(json!({}))).is_configured());
        assert!(
            !merged(
                Some(json!({"loader_functions": [], "module_roots": ["/abs"]})),
                None
            )
            .is_configured()
        );
        assert!(merged(Some(json!({"module_roots": ["."]})), None).is_configured());
        let loaders = merged(
            Some(json!({"loader_functions": ["Require", "Game.Load"]})),
            None,
        );
        assert!(loaders.is_configured());
        assert!(loaders.is_loader("Require"));
        assert!(loaders.is_loader("Game.Load"));
        assert!(!loaders.is_loader("require"));
        assert!(!loaders.is_loader("Load"));
        assert!(!loaders.is_loader("Game"));
    }
}
