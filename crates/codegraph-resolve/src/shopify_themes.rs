//! A Shopify theme's section and snippet references reach that theme's own
//! files (upstream #2396, in the shape at `b635dd46`).
//!
//! `{% render 'price' %}` renders the `snippets/price.liquid` of the theme
//! the file belongs to; `{% section 'header' %}` and a JSON template's
//! section `"type"` name a file in that theme's `sections/`. Shopify never
//! looks in another theme. The Liquid extractor records these as
//! `snippets/<name>.liquid` and `sections/<name>.liquid`, and the path
//! matcher took any file whose path ends with that, so in a repository
//! holding several themes (a theme tool's examples, a store's seasonal
//! themes) a theme missing a section linked to another theme's copy of it.
//!
//! From a file inside a theme
//! ([`codegraph_extract::embedded::shopify::shopify_theme_root`]), these
//! references resolve to the theme's own file or to nothing, and never fall
//! through to the generic strategies. A reference from a file in no theme
//! resolves as before.

use codegraph_core::types::{Language, NodeKind};
use codegraph_extract::embedded::shopify::{SHOPIFY_THEME_FOLDERS, shopify_theme_root};

use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// The file a section or snippet reference made inside a Shopify theme names
/// (upstream `matchShopifyThemeFile`): `Some` with the theme's own
/// `sections/<name>.liquid` or `snippets/<name>.liquid`, or nothing when the
/// theme has none, so no other strategy may guess one; `None` when the
/// reference is no such one, or is made in no theme.
pub(crate) fn match_shopify_theme_file(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<Option<ResolvedRef>> {
    if reference.language != Language::Liquid
        || !(reference.reference_name.starts_with("sections/")
            || reference.reference_name.starts_with("snippets/"))
    {
        return None;
    }
    let theme = shopify_theme_root(
        &reference.file_path,
        &|path| context.file_exists(path),
        &SHOPIFY_THEME_FOLDERS,
    )?;
    let target = if theme.is_empty() {
        reference.reference_name.clone()
    } else {
        format!("{theme}/{}", reference.reference_name)
    };
    let file_name = target.rsplit('/').next().unwrap_or(&target);
    let found = context
        .get_nodes_by_name_shared(file_name)
        .into_iter()
        .find(|node| node.kind == NodeKind::File && node.file_path == target);
    // The confidence the path matcher gives the same link: an exact path for
    // a theme at the project root, a path tail for one in a folder.
    Some(found.map(|file| {
        ResolvedRef::new(
            reference.clone(),
            file.id.clone(),
            if theme.is_empty() { 0.95 } else { 0.85 },
            ResolvedBy::FilePath,
        )
    }))
}
