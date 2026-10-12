//! A JavaScript-family module's surface: what other files read from its own
//! text when they resolve against it, which no node records (W4a-F3).
//!
//! Import resolution reads a module's re-exports and export aliases (as
//! `get_re_exports` reads a barrel), its CommonJS default (`module.exports =
//! …`) and whether it has an `export default` statement. Name matching reads
//! the names its export lists give and whether it is a sealed module
//! (upstream #2162), and a component's `<script>` blocks (upstream #2227).
//! The text is gone once the file changes on disk, so an index keeps a digest
//! of the surface per file in `project_metadata`, with the specifiers the
//! module forwards and what they resolved to: a sync that sees the digest
//! move refreshes the files that resolve through the module, and the barrels
//! that forward a changed module are found from the rows alone.

use std::fmt::Write as _;
use std::sync::Arc;

use codegraph_core::node_id::hash_content;
use codegraph_core::types::Language;
use serde::{Deserialize, Serialize};

use crate::import_resolver::{self, CommonJsDefault};
use crate::source_facts::SourceFacts;
use crate::types::{ReExport, ResolutionContext};

/// `project_metadata` key prefix of a file's surface row.
pub const ROW_KEY_PREFIX: &str = "js_module_surface:";
/// `project_metadata` key holding the row format every row of the index was
/// written in. An index without it (built before the rows existed) holds no
/// rows to trust.
pub const VERSION_KEY: &str = "js_module_surface_version";
/// The row format this build writes.
pub const VERSION: &str = "1";

/// The canonical encoding of the default surface: an ES module none of whose
/// text another file's resolution reads. No row records it.
const DEFAULT_CANONICAL: &str = "surface/1\n";

/// The `project_metadata` key of `file_path`'s row.
pub fn row_key(file_path: &str) -> String {
    format!("{ROW_KEY_PREFIX}{file_path}")
}

/// A module's surface as computed from its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleSurface {
    /// The sha256 of the canonical encoding of every fact.
    pub digest: String,
    /// The specifiers it forwards (`export * from`, `export * as … from`,
    /// `export { … } from`, `module.exports = require(…)`), in order of first
    /// appearance.
    pub sources: Vec<String>,
}

/// A surface as its row stores it: with what each source resolved to when
/// the row was written, so a barrel that forwarded a file still names it once
/// the file is gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceRow {
    pub digest: String,
    pub sources: Vec<String>,
    /// `targets[i]` is the project file `sources[i]` resolved to, or `None`.
    pub targets: Vec<Option<String>>,
}

impl SurfaceRow {
    /// `surface` with its sources resolved in the project `context` sees.
    pub fn resolved(
        file_path: &str,
        surface: &ModuleSurface,
        context: &dyn ResolutionContext,
    ) -> Self {
        Self {
            digest: surface.digest.clone(),
            sources: surface.sources.clone(),
            targets: resolve_targets(file_path, &surface.sources, context),
        }
    }

    /// The row's `project_metadata` value.
    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("a surface row is plain JSON")
    }

    /// A row read back; `None` for a value this build does not understand.
    pub fn decode(value: &str) -> Option<Self> {
        serde_json::from_str::<Self>(value)
            .ok()
            .filter(|row| row.sources.len() == row.targets.len())
    }
}

/// Whether a file of this path and language can have a surface: a JS-family
/// path, or a file in a language whose modules another file imports.
pub fn has_module_surface(file_path: &str, language: Language) -> bool {
    crate::context::is_js_family_path_pub(file_path)
        || matches!(
            language,
            Language::TypeScript
                | Language::JavaScript
                | Language::Tsx
                | Language::Jsx
                | Language::ArkTs
                | Language::Vue
                | Language::Svelte
                | Language::Astro
        )
}

/// The surface of `file_path` read from `text`, or `None` when it is the
/// default one. `text` is `None` when the file was not read (over the size
/// limit, or no source), which the resolver reads as no facts at all.
pub fn module_surface(
    file_path: &str,
    language: Language,
    text: Option<&str>,
) -> Option<ModuleSurface> {
    if !has_module_surface(file_path, language) {
        return None;
    }
    let Some(text) = text else {
        return Some(ModuleSurface {
            digest: hash_content("surface/1\nunread\n"),
            sources: Vec::new(),
        });
    };
    let (canonical, sources) = canonical_surface(file_path, text);
    (canonical != DEFAULT_CANONICAL).then(|| ModuleSurface {
        digest: hash_content(&canonical),
        sources,
    })
}

/// What each of `sources`, forwarded by `file_path`, resolves to in the
/// project `context` sees, as a barrel's re-exports are followed.
pub fn resolve_targets(
    file_path: &str,
    sources: &[String],
    context: &dyn ResolutionContext,
) -> Vec<Option<String>> {
    sources
        .iter()
        .map(|source| {
            import_resolver::resolve_import_path(source, file_path, Language::TypeScript, context)
        })
        .collect()
}

/// The canonical encoding of every fact and the forwarded specifiers. Each
/// fact is computed by the function resolution itself calls, so whatever it
/// reads differently moves the encoding.
fn canonical_surface(file_path: &str, text: &str) -> (String, Vec<String>) {
    fn forward(source: &str, sources: &mut Vec<String>) {
        if !sources.iter().any(|known| known == source) {
            sources.push(source.to_string());
        }
    }
    let mut out = String::from(DEFAULT_CANONICAL);
    let mut sources: Vec<String> = Vec::new();
    // `get_re_exports` reads a JS-family barrel with the TypeScript rules,
    // and any other file with the importer's: TypeScript is the superset.
    for re_export in import_resolver::extract_re_exports(text, Language::TypeScript) {
        let _ = match &re_export {
            ReExport::Wildcard { source } => {
                forward(source, &mut sources);
                writeln!(out, "wildcard\t{source}")
            }
            ReExport::Namespace {
                exported_name,
                source,
            } => {
                forward(source, &mut sources);
                writeln!(out, "namespace\t{exported_name}\t{source}")
            }
            ReExport::Named {
                exported_name,
                original_name,
                source,
            } => {
                forward(source, &mut sources);
                writeln!(out, "named\t{exported_name}\t{original_name}\t{source}")
            }
            ReExport::LocalAlias {
                exported_name,
                original_name,
            } => writeln!(out, "local\t{exported_name}\t{original_name}"),
            ReExport::CommonJsAlias {
                exported_name,
                original_name,
            } => writeln!(out, "commonjs\t{exported_name}\t{original_name}"),
        };
    }
    if import_resolver::is_js_family_file(file_path) {
        let _ = match import_resolver::commonjs_default_of(text) {
            Some(CommonJsDefault::Forward(source)) => {
                forward(&source, &mut sources);
                writeln!(out, "commonjs-forward\t{source}")
            }
            Some(CommonJsDefault::Binding(name)) => writeln!(out, "commonjs-default\t{name}"),
            None => Ok(()),
        };
        if import_resolver::has_esm_default_statement(text) {
            out.push_str("export-default\n");
        }
    }
    let facts = SourceFacts::new(Arc::from(text));
    match crate::name_matcher::read_module_exports(&facts) {
        None => out.push_str("no-module\n"),
        Some(names) => {
            let mut names: Vec<String> = names.into_iter().collect();
            names.sort_unstable();
            for name in names {
                let _ = writeln!(out, "exports\t{name}");
            }
        }
    }
    if crate::name_matcher::is_sealed_by_text(&facts) {
        out.push_str("sealed\n");
    }
    let svelte = file_path.ends_with(".svelte");
    if svelte || file_path.ends_with(".vue") {
        for block in crate::sfc_scripts::read_script_blocks(&facts, svelte) {
            let _ = writeln!(
                out,
                "script\t{}\t{}\t{}",
                block.start, block.end, block.exported
            );
        }
    }
    (out, sources)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(path: &str, text: &str) -> Option<String> {
        module_surface(path, Language::TypeScript, Some(text)).map(|surface| surface.digest)
    }

    #[test]
    fn a_module_exporting_at_its_declarations_has_the_default_surface() {
        assert_eq!(
            digest("src/a.ts", "export function f() {\n  return 1;\n}\n"),
            None
        );
        assert_eq!(
            digest(
                "src/a.ts",
                "import { x } from './x';\nexport const y = x + 1;\n// export * from './z';\n"
            ),
            None
        );
        assert_eq!(
            module_surface("src/main.py", Language::Python, Some("export * from './x'")),
            None
        );
    }

    #[test]
    fn comments_and_bodies_leave_the_digest_alone() {
        let before = "export * from './a';\nexport function f() {\n  return 1;\n}\n";
        let after = "// the barrel\nexport * from './a';\nexport function f() {\n  return 2;\n}\n";
        assert!(digest("src/i.ts", before).is_some());
        assert_eq!(digest("src/i.ts", before), digest("src/i.ts", after));
    }

    #[test]
    fn every_kind_of_fact_moves_the_digest() {
        let base = "import { q } from './q';\nexport { q };\nexport function f() {}\n";
        let variants = [
            "import { q } from './q';\nexport { q };\nexport function f() {}\nexport * from './a';\n",
            "import { q } from './q';\nexport { q };\nexport function f() {}\nexport * as ns from './a';\n",
            "import { q } from './q';\nexport { q };\nexport function f() {}\nexport { a as b } from './a';\n",
            "import { q } from './q';\nexport { q as r };\nexport function f() {}\n",
            "import { q } from './q';\nexport { q };\nexport function f() {}\nexport default q;\n",
            "import { q } from './q';\nexport function f() {}\n",
            "import { q } from './q';\nfunction f() {}\n",
            "function f() {}\n",
        ];
        let base_digest = digest("src/m.ts", base);
        assert!(base_digest.is_some());
        let mut seen = vec![base_digest.clone()];
        for variant in variants {
            let moved = digest("src/m.ts", variant);
            assert!(
                !seen.contains(&moved),
                "{variant:?} kept a digest: {moved:?}"
            );
            seen.push(moved);
        }
        let cjs = |text: &str| module_surface("src/c.js", Language::JavaScript, Some(text));
        let forward_a = cjs("module.exports = require('./a');\n").expect("a forward");
        assert_eq!(forward_a.sources, ["./a"]);
        let forward_b = cjs("module.exports = require('./b');\n").expect("a forward");
        assert_ne!(forward_a.digest, forward_b.digest);
        assert_ne!(
            cjs("module.exports = parse;\n").map(|s| s.digest),
            cjs("module.exports = format;\n").map(|s| s.digest)
        );
    }

    #[test]
    fn sources_are_the_forwarded_specifiers_in_order() {
        let surface = module_surface(
            "src/index.ts",
            Language::TypeScript,
            Some("export * from './b';\nexport { x } from './a';\nexport * as ns from './b';\n"),
        )
        .expect("a barrel");
        assert_eq!(surface.sources, ["./b", "./a"]);
    }

    #[test]
    fn a_component_script_block_moving_moves_the_digest() {
        let surface = |text: &str| {
            module_surface("src/App.svelte", Language::Svelte, Some(text)).map(|s| s.digest)
        };
        let instance = "<script>\n  export let a = 1;\n</script>\n";
        let module = "<script context=\"module\">\n  export let a = 1;\n</script>\n";
        assert_ne!(surface(instance), surface(module));
    }

    #[test]
    fn an_unread_file_has_a_surface_of_its_own() {
        let unread = module_surface("src/big.ts", Language::TypeScript, None).expect("unread");
        assert!(unread.sources.is_empty());
        assert_ne!(
            Some(unread.digest),
            digest("src/big.ts", "export * from './a';\n")
        );
    }

    #[test]
    fn a_row_round_trips_and_rejects_a_mismatched_value() {
        let row = SurfaceRow {
            digest: "d".into(),
            sources: vec!["./a".into(), "pkg".into()],
            targets: vec![Some("src/a.ts".into()), None],
        };
        assert_eq!(SurfaceRow::decode(&row.encode()), Some(row));
        assert_eq!(
            SurfaceRow::decode(r#"{"digest":"d","sources":["./a"],"targets":[]}"#),
            None
        );
        assert_eq!(SurfaceRow::decode("not json"), None);
    }
}
