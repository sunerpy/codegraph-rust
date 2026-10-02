//! The embedded viewer bundle and how its files are served — upstream
//! `src/ui-server/static.ts` and `assets.ts`.
//!
//! The bundle is compiled into the binary (see `build.rs`), so there is no
//! viewer directory to resolve, no symlink to follow, and nothing on disk an
//! attacker could swap. A path is looked up in the sorted table or it does not
//! exist.

include!(concat!(env!("OUT_DIR"), "/viewer_assets.rs"));

/// The bytes of one bundle file, by its bundle-relative path (`assets/x.js`).
pub fn viewer_file(path: &str) -> Option<&'static [u8]> {
    VIEWER_FILES
        .binary_search_by(|(p, _)| (*p).cmp(path))
        .ok()
        .map(|i| VIEWER_FILES[i].1)
}

/// Whether this build carries a usable viewer (an `index.html` at least).
pub fn has_viewer() -> bool {
    viewer_file("index.html").is_some()
}

/// The content type upstream's MIME table gives a file, by extension.
pub fn content_type_for(path: &str) -> &'static str {
    let ext = path
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// Hashed build output never changes under a name; everything else (the
/// `index.html` shell above all) must be re-fetched every time.
pub fn cache_control_for(path: &str) -> &'static str {
    if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-store"
    }
}

/// An unmatched path with no extension is a client-side route and gets the app
/// shell; one with an extension is a missing file and gets a 404.
pub fn should_fall_back_to_index(pathname: &str) -> bool {
    let last = pathname.rsplit('/').next().unwrap_or("");
    !last.contains('.') || last.starts_with('.') && last[1..].find('.').is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_so_lookups_are_binary_searches() {
        assert!(VIEWER_FILES.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn content_types_follow_the_extension() {
        assert_eq!(content_type_for("index.html"), "text/html; charset=utf-8");
        assert_eq!(
            content_type_for("assets/a.JS"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(content_type_for("assets/f.woff2"), "font/woff2");
        assert_eq!(content_type_for("LICENSE"), "application/octet-stream");
    }

    #[test]
    fn only_hashed_assets_are_cached() {
        assert_eq!(
            cache_control_for("assets/index-abc.js"),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(cache_control_for("index.html"), "no-store");
    }

    #[test]
    fn extensionless_paths_fall_back_to_the_shell() {
        assert!(should_fall_back_to_index("/s/function:abc"));
        assert!(should_fall_back_to_index("/map"));
        assert!(!should_fall_back_to_index("/missing.js"));
        assert!(!should_fall_back_to_index("/assets/x.css"));
    }
}
