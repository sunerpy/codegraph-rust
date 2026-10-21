pub mod astro;
pub mod dfm;
pub mod liquid;
pub mod mybatis;
pub mod razor;
mod shared;
pub mod shopify;
pub mod svelte;
pub mod vue;
mod vue_options_api;
mod vue_template_calls;

use codegraph_core::types::{ExtractionResult, Language};
use std::path::Path;

pub fn extract_embedded(
    file_path: &str,
    source: &str,
    language: Language,
) -> Option<ExtractionResult> {
    match language {
        Language::Pascal if is_dfm_form_path(file_path) => {
            Some(dfm::DfmExtractor::new(file_path, source).extract())
        }
        Language::Svelte => Some(svelte::SvelteExtractor::new(file_path, source).extract()),
        Language::Vue => Some(vue::VueExtractor::new(file_path, source).extract()),
        Language::Astro => Some(astro::AstroExtractor::new(file_path, source).extract()),
        Language::Liquid => Some(liquid::LiquidExtractor::new(file_path, source).extract()),
        Language::Razor => Some(razor::RazorExtractor::new(file_path, source).extract()),
        Language::Xml => Some(mybatis::MyBatisExtractor::new(file_path, source).extract()),
        _ => None,
    }
}

/// Whether an embedded extractor reads `file_path`; a Shopify theme's JSON
/// template counts only under the `project_root` that tells it apart
/// ([`shopify::is_shopify_liquid_json`]).
pub fn is_embedded_source_path(file_path: &str, project_root: Option<&Path>) -> bool {
    detect_embedded_language(file_path).is_some()
        || is_dfm_form_path(file_path)
        || shopify::is_shopify_liquid_json(file_path, project_root)
}

pub fn is_dfm_form_path(file_path: &str) -> bool {
    let ext = Path::new(file_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);
    matches!(ext.as_deref(), Some("dfm" | "fmx"))
}

/// The embedded language `file_path`'s extension names. A Shopify theme's
/// JSON templates are Liquid too, which takes the project root to tell
/// ([`shopify::is_shopify_liquid_json`]).
pub fn detect_embedded_language(file_path: &str) -> Option<Language> {
    let ext = Path::new(file_path)
        .extension()
        .and_then(|ext| ext.to_str())?
        .to_ascii_lowercase();
    match ext.as_str() {
        "svelte" => Some(Language::Svelte),
        "vue" => Some(Language::Vue),
        "astro" => Some(Language::Astro),
        "liquid" => Some(Language::Liquid),
        "razor" | "cshtml" => Some(Language::Razor),
        "xml" => Some(Language::Xml),
        _ => None,
    }
}
