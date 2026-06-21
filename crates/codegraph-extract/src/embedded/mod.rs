pub mod astro;
pub mod dfm;
pub mod liquid;
pub mod mybatis;
pub mod razor;
mod shared;
pub mod svelte;
pub mod vue;

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

pub fn is_embedded_source_path(file_path: &str) -> bool {
    detect_embedded_language(file_path).is_some() || is_dfm_form_path(file_path)
}

pub fn is_dfm_form_path(file_path: &str) -> bool {
    let ext = Path::new(file_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);
    matches!(ext.as_deref(), Some("dfm" | "fmx"))
}

pub fn detect_embedded_language(file_path: &str) -> Option<Language> {
    if is_shopify_liquid_json(file_path) {
        return Some(Language::Liquid);
    }
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

pub fn is_shopify_liquid_json(file_path: &str) -> bool {
    let normalized = file_path.replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    (lower.contains("/templates/") || lower.starts_with("templates/")) && lower.ends_with(".json")
        || ((lower.contains("/sections/") || lower.starts_with("sections/"))
            && lower.ends_with(".json"))
}
