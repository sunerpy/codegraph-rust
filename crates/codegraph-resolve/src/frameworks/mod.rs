//! Concrete [`FrameworkResolver`] implementations + the detection registry.
//!
//! Ports the subset of `upstream resolution/frameworks/` that v1
//! ships: React/Next.js ([`react`]), Vue/Nuxt ([`vue`]), and NestJS ([`nestjs`]).
//! The other ~19 upstream resolvers remain deferred (see `KNOWN_DIFFS.md`).
//!
//! [`detect_frameworks`] ports `frameworks/index.ts:detectFrameworks` restricted
//! to the three ported resolvers; the orchestrator hands the detected list to
//! [`crate::resolver::ReferenceResolver`] so they participate in
//! [`crate::resolver::ReferenceResolver::resolve_one`] Strategy-1 exactly where
//! the upstream runs them (`index.ts:695-701`).

pub mod nestjs;
pub mod react;
pub mod vue;

use crate::framework::FrameworkResolver;
use crate::types::ResolutionContext;
use codegraph_core::types::{Language, Node, NodeKind};
use std::time::{SystemTime, UNIX_EPOCH};

/// Detect which of the ported frameworks a project uses
/// (`detectFrameworks`, `frameworks/index.ts:90-98`, restricted to react/vue/
/// nestjs). The candidate order mirrors the upstream `FRAMEWORK_RESOLVERS`
/// declaration order for the three ported entries (nestjs, react, vue).
pub fn detect_frameworks(context: &dyn ResolutionContext) -> Vec<Box<dyn FrameworkResolver>> {
    let candidates: Vec<Box<dyn FrameworkResolver>> = vec![
        Box::new(nestjs::NestjsResolver),
        Box::new(react::ReactResolver),
        Box::new(vue::VueResolver),
    ];
    candidates
        .into_iter()
        .filter(|resolver| resolver.detect(context))
        .collect()
}

/// `Language` whose comment syntax / file family a JS/TS-ish path belongs to,
/// used by the route-extracting resolvers when building node `language` fields.
/// Mirrors the upstream `detectLanguage` ternaries that pick `tsx`/`jsx`/
/// `typescript`/`javascript` by extension.
pub(crate) fn js_language_for(file_path: &str) -> Language {
    if file_path.ends_with(".tsx") {
        Language::Tsx
    } else if file_path.ends_with(".jsx") {
        Language::Jsx
    } else if file_path.ends_with(".ts")
        || file_path.ends_with(".mts")
        || file_path.ends_with(".cts")
    {
        Language::TypeScript
    } else {
        Language::JavaScript
    }
}

/// Current epoch millis for a framework node's `updatedAt` (`Date.now()`).
pub(crate) fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

/// Build a framework node carrying the upstream literal (non-hashed) id. The upstream's
/// framework extractors set a fixed id string directly rather than the sha256
/// node-id formula, so the port reproduces that literal id verbatim.
#[allow(clippy::too_many_arguments)]
pub(crate) fn framework_node(
    id: String,
    kind: NodeKind,
    name: String,
    qualified_name: String,
    file_path: String,
    start_line: i64,
    end_line: i64,
    start_column: i64,
    end_column: i64,
    language: Language,
    is_exported: bool,
) -> Node {
    Node {
        id,
        kind,
        name,
        qualified_name,
        file_path,
        language,
        start_line,
        end_line,
        start_column,
        end_column,
        docstring: None,
        signature: None,
        visibility: None,
        is_exported,
        is_async: false,
        is_static: false,
        is_abstract: false,
        decorators: Vec::new(),
        type_parameters: Vec::new(),
        return_type: None,
        updated_at: now_millis(),
    }
}
