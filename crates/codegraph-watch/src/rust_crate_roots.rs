//! The Rust lane of a sync and its crash marker (W7-19h).
//!
//! The Rust framework's module rule (`codegraph_resolve::frameworks::rust`)
//! links a lowercase name to the file node of the root's `src/{name}.rs` or
//! `src/{name}/mod.rs`, else of the `src/lib.rs` or `src/main.rs` of the
//! workspace crate the name gives, the first of them the index holds. That
//! target is a file node, named for its file (`lib.rs`), never for the
//! reference, so a sync that adds or removes one of those files touches no
//! name the reference waits under, and no edge into the file it keeps. A sync
//! re-resolves, for each such file it added or removed, and for each tracked
//! one whose file node came or went (a file over `max_file_size` is recorded
//! without nodes, so crossing the limit adds or removes its file node while
//! the file stays tracked; [`may_be_module_target`]), the references named
//! for it ([`names_reaching`]): the unresolved ones by their whole names, the
//! resolved ones by site.
//!
//! What a sync added and removed lives in its memory, so a sync withdraws
//! [`VERSION_KEY`] before its first change and writes it back once
//! everything resolved, and an index without it re-resolves every Rust file
//! once. The workspace's crates are read from the manifests, which are the
//! Rust control kind's inputs: a change to one re-resolves the Rust files
//! anyway.

use std::collections::BTreeSet;

use anyhow::Result;
use codegraph_resolve::frameworks::cargo_workspace::CrateMap;
use codegraph_store::Store;

/// Metadata key present when no sync left a Rust crate root unretried.
pub const VERSION_KEY: &str = "rust_crate_roots_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// Whether this sync must re-resolve every Rust file.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that no Rust crate root is pending: a full index, a migration, or
/// a sync whose resolution is persisted.
pub fn write_marker(store: &Store) -> Result<()> {
    store.set_project_metadata(VERSION_KEY, VERSION)?;
    Ok(())
}

/// Withdraw the marker before a sync's first change.
pub(crate) fn withdraw_marker(store: &Store) -> Result<()> {
    store.delete_project_metadata(VERSION_KEY)?;
    Ok(())
}

/// The names the module rule may answer with one of `paths`, the files a
/// sync added or removed: `{name}` for the root's `src/{name}.rs` and
/// `src/{name}/mod.rs`, and every name and alias `crates` gives a member for
/// the member's `src/lib.rs` and `src/main.rs`.
pub(crate) fn names_reaching<'p>(
    paths: impl IntoIterator<Item = &'p String>,
    crates: &CrateMap,
) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for path in paths {
        if let Some(name) = root_module_name(path) {
            names.insert(name.to_string());
        }
        if let Some(member) = path
            .strip_suffix("/src/lib.rs")
            .or_else(|| path.strip_suffix("/src/main.rs"))
        {
            names.extend(
                crates
                    .iter()
                    .filter(|(_, dir)| dir.as_str() == member)
                    .map(|(name, _)| name.clone()),
            );
        }
    }
    names
}

/// Whether `path` has a shape [`names_reaching`] reads: the root's
/// `src/{name}.rs` or `src/{name}/mod.rs`, or any directory's
/// `src/lib.rs` or `src/main.rs` (the workspace's crates decide which
/// directories are members). A sync looks up the file node of such a
/// tracked file before and after it re-extracts it.
pub(crate) fn may_be_module_target(path: &str) -> bool {
    root_module_name(path).is_some()
        || path.ends_with("/src/lib.rs")
        || path.ends_with("/src/main.rs")
}

/// The module `path` is when it is the root's `src/{name}.rs` or
/// `src/{name}/mod.rs`.
fn root_module_name(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("src/")?;
    let name = rest
        .strip_suffix("/mod.rs")
        .or_else(|| rest.strip_suffix(".rs"))?;
    (!name.is_empty() && !name.contains('/')).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crates(entries: &[(&str, &str)]) -> CrateMap {
        entries
            .iter()
            .map(|(name, dir)| (name.to_string(), dir.to_string()))
            .collect()
    }

    fn names(paths: &[&str], crates: &CrateMap) -> Vec<String> {
        let paths: Vec<String> = paths.iter().map(|path| path.to_string()).collect();
        names_reaching(&paths, crates).into_iter().collect()
    }

    #[test]
    fn a_root_module_file_names_its_module() {
        let none = CrateMap::new();
        assert_eq!(names(&["src/beta.rs"], &none), ["beta"]);
        assert_eq!(names(&["src/beta/mod.rs"], &none), ["beta"]);
        assert_eq!(
            names(&["src/lib.rs", "src/main.rs"], &none),
            ["lib", "main"]
        );
        // Neither a nested module nor a file outside the root's `src/`.
        assert_eq!(
            names(
                &[
                    "src/a/b.rs",
                    "src/a/b/mod.rs",
                    "lib/beta.rs",
                    "crates/x/src/beta.rs",
                    "src/.rs",
                    "src/beta.txt",
                ],
                &none
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_module_target_is_a_root_module_or_a_directorys_crate_root() {
        for path in [
            "src/beta.rs",
            "src/beta/mod.rs",
            "src/lib.rs",
            "src/main.rs",
            "crates/beta/src/lib.rs",
            "crates/beta/src/main.rs",
            "beta/src/lib.rs",
        ] {
            assert!(may_be_module_target(path), "{path}");
        }
        for path in [
            "src/a/b.rs",
            "src/a/b/mod.rs",
            "lib/beta.rs",
            "crates/beta/src/util.rs",
            "crates/beta/lib.rs",
            "crates/beta/src/lib.rs.bak",
            "src/.rs",
        ] {
            assert!(!may_be_module_target(path), "{path}");
        }
    }

    #[test]
    fn a_member_root_names_every_key_of_its_member() {
        let map = crates(&[
            ("mytool-core", "crates/mytool-core"),
            ("mytool_core", "crates/mytool-core"),
            ("beta", "crates/beta"),
        ]);
        assert_eq!(
            names(&["crates/mytool-core/src/lib.rs"], &map),
            ["mytool-core", "mytool_core"]
        );
        assert_eq!(names(&["crates/beta/src/main.rs"], &map), ["beta"]);
        // A member's other files, and a directory no member is, name none.
        assert_eq!(
            names(
                &[
                    "crates/beta/src/util.rs",
                    "crates/beta/lib.rs",
                    "crates/gamma/src/lib.rs",
                ],
                &map
            ),
            Vec::<String>::new()
        );
    }
}
