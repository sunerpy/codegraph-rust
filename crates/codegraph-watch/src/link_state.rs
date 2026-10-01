//! The symlinks an index was built through, and which files must be re-read.
//!
//! The scan follows symlinks (upstream #935), so retargeting a link can point
//! an indexed logical path at a different file with the same size and mtime.
//! Sync's stat pre-filter would then keep the old graph although
//! `index --force` reads the new target. Every full build therefore records
//! the links it followed under [`FOLLOWED_LINKS_KEY`], and a full sync (and
//! the pending inventory) bypasses the stat pre-filter at or under each
//! current link whose identity is new or changed. A missing or malformed
//! record means the identity is unknown — any index built before this record
//! existed, including one that tracked a regular path later replaced by a
//! link — so every current link is re-read until a full sync records it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use codegraph_extract::engine::{FollowedLink, LinkKind};
use codegraph_store::Store;

/// `project_metadata` key holding the links the index was built through: a
/// JSON array of `[relative, canonical, "file" | "dir"]`, sorted by path.
pub const FOLLOWED_LINKS_KEY: &str = "followed_links";

fn kind_name(kind: LinkKind) -> &'static str {
    match kind {
        LinkKind::File => "file",
        LinkKind::Dir => "dir",
    }
}

/// The canonical record: one row per link, sorted by logical path.
fn encode(links: &[FollowedLink]) -> String {
    let mut links = links.iter().collect::<Vec<_>>();
    links.sort_by(|left, right| left.relative.cmp(&right.relative));
    links.dedup_by(|left, right| left.relative == right.relative);
    let rows = links
        .into_iter()
        .map(|link| {
            serde_json::json!([
                link.relative,
                link.canonical.to_string_lossy(),
                kind_name(link.kind)
            ])
        })
        .collect::<Vec<_>>();
    serde_json::Value::Array(rows).to_string()
}

/// `None` for anything that is not exactly the format [`encode`] writes,
/// including rows out of order or a logical path recorded twice: an ambiguous
/// record proves no link identity, so it counts as missing.
fn decode(value: &str) -> Option<BTreeMap<String, (PathBuf, LinkKind)>> {
    let rows: Vec<(String, String, String)> = serde_json::from_str(value).ok()?;
    let mut links = BTreeMap::new();
    let mut previous: Option<String> = None;
    for (relative, canonical, kind) in rows {
        if previous
            .as_ref()
            .is_some_and(|previous| *previous >= relative)
        {
            return None;
        }
        let kind = match kind.as_str() {
            "file" => LinkKind::File,
            "dir" => LinkKind::Dir,
            _ => return None,
        };
        previous = Some(relative.clone());
        links.insert(relative, (PathBuf::from(canonical), kind));
    }
    Some(links)
}

/// Record the links a full build or full sync just indexed through.
pub fn record_followed_links(store: &Store, links: &[FollowedLink]) -> Result<()> {
    store
        .set_project_metadata(FOLLOWED_LINKS_KEY, &encode(links))
        .context("record followed links")?;
    Ok(())
}

/// The logical paths whose stored stat proves nothing because the link that
/// reaches them is new, retargeted, or of unknown identity.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct RehashUnder {
    links: Vec<(String, LinkKind)>,
}

impl RehashUnder {
    /// Compare the current scan's links with the record in `store`.
    pub(crate) fn for_scan(store: &Store, current: &[FollowedLink]) -> Result<Self> {
        let recorded = store
            .get_project_metadata(FOLLOWED_LINKS_KEY)
            .context("read followed links")?
            .as_deref()
            .and_then(decode);
        Ok(Self::compare(recorded.as_ref(), current))
    }

    fn compare(
        recorded: Option<&BTreeMap<String, (PathBuf, LinkKind)>>,
        current: &[FollowedLink],
    ) -> Self {
        let links = current
            .iter()
            .filter(|link| {
                recorded.is_none_or(|recorded| {
                    recorded.get(&link.relative) != Some(&(link.canonical.clone(), link.kind))
                })
            })
            .map(|link| (link.relative.clone(), link.kind))
            .collect();
        Self { links }
    }

    /// Whether `relative` must be re-read regardless of its stored stat.
    pub(crate) fn covers(&self, relative: &str) -> bool {
        self.links.iter().any(|(link, kind)| match kind {
            LinkKind::File => relative == link,
            LinkKind::Dir => {
                relative == link
                    || relative
                        .strip_prefix(link.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(relative: &str, canonical: &str, kind: LinkKind) -> FollowedLink {
        FollowedLink {
            relative: relative.to_string(),
            canonical: PathBuf::from(canonical),
            kind,
        }
    }

    #[test]
    fn the_record_round_trips_and_rejects_anything_else() {
        let links = vec![
            link("a.ts", "/x/a.ts", LinkKind::File),
            link("lib", "/x/lib", LinkKind::Dir),
        ];
        let decoded = decode(&encode(&links)).expect("round trip");
        assert_eq!(
            decoded.get("lib"),
            Some(&(PathBuf::from("/x/lib"), LinkKind::Dir))
        );
        assert_eq!(decoded.len(), 2);
        for malformed in ["", "{}", "[[\"a\",\"/x\"]]", "[[\"a\",\"/x\",\"socket\"]]"] {
            assert_eq!(decode(malformed), None, "{malformed:?}");
        }
        // A duplicated or reordered path is ambiguous, so it proves nothing.
        let duplicated = r#"[["lib","/old","dir"],["lib","/new","dir"]]"#;
        let reordered = r#"[["z","/z","dir"],["a","/a","dir"]]"#;
        assert_eq!(decode(duplicated), None);
        assert_eq!(decode(reordered), None);
    }

    #[test]
    fn an_ambiguous_record_rehashes_every_current_link() {
        // The last duplicate matches the current target; a decoder that kept it
        // would skip the rehash the earlier, different identity demands.
        let duplicated = r#"[["lib","/old","dir"],["lib","/new","dir"]]"#;
        let current = [link("lib", "/new", LinkKind::Dir)];
        let rehash = RehashUnder::compare(decode(duplicated).as_ref(), &current);
        assert!(rehash.covers("lib/a.ts"));
    }

    #[test]
    fn only_new_retargeted_or_retyped_links_are_rehashed() {
        let recorded = decode(&encode(&[
            link("same", "/t/same", LinkKind::Dir),
            link("moved", "/t/old", LinkKind::Dir),
            link("retyped", "/t/retyped", LinkKind::Dir),
        ]))
        .unwrap();
        let current = [
            link("same", "/t/same", LinkKind::Dir),
            link("moved", "/t/new", LinkKind::Dir),
            link("retyped", "/t/retyped", LinkKind::File),
            link("fresh", "/t/fresh", LinkKind::Dir),
        ];
        let rehash = RehashUnder::compare(Some(&recorded), &current);
        assert!(!rehash.covers("same/a.ts"));
        assert!(rehash.covers("moved/a.ts"));
        assert!(rehash.covers("retyped"));
        assert!(
            !rehash.covers("retyped/a.ts"),
            "a file link covers itself only"
        );
        assert!(rehash.covers("fresh/deep/b.ts"));
        assert!(
            !rehash.covers("freshly.ts"),
            "a prefix match needs a segment boundary"
        );
    }

    #[test]
    fn an_unknown_record_rehashes_under_every_current_link() {
        let current = [
            link("lib", "/t/lib", LinkKind::Dir),
            link("x.ts", "/t/x.ts", LinkKind::File),
        ];
        let rehash = RehashUnder::compare(None, &current);
        assert!(rehash.covers("lib/a.ts"));
        assert!(rehash.covers("x.ts"));
        assert!(!rehash.covers("src/plain.ts"));
    }
}
