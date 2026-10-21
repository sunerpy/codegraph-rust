//! The unchanged Rust files whose path receivers a sync's type changes reach
//! (W4-RS-03, upstream #2177).
//!
//! A CamelCase path receiver (`Builder::new()`) is an outside type unless the
//! project declares a type, trait or alias of its name, and no node of the
//! method's name records that: a sync that adds, removes or renames such a
//! Rust declaration re-resolves, in every unchanged Rust file whose text
//! writes the name as a receiver (`Name::`, `Name.`), the references written
//! through it ([`is_written_through`]). Only a marker is stored: the files are
//! read when a sync touches such a name, and a sync that fails after its
//! first change leaves the marker away, so the next one re-resolves every
//! Rust file.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use anyhow::Result;
use codegraph_core::types::{Language, Node, NodeKind};
use codegraph_extract::{SourceText, read_source_file};
use codegraph_store::Store;

/// Metadata key present when no sync left a Rust receiver unrefreshed.
pub const VERSION_KEY: &str = "rust_receivers_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// Whether this index needs every Rust file re-resolved.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that every Rust receiver is current: a full index, a migration, or
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

/// Every unchanged Rust file, for a bootstrap.
pub(crate) fn every_rust_file(
    store: &Store,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    Ok(store
        .all_files()?
        .into_iter()
        .filter(|file| {
            file.language == Language::Rust
                && !reindexed.contains(&file.path)
                && !removed.contains(&file.path)
        })
        .map(|file| file.path)
        .collect())
}

/// Whether `node` is a Rust declaration whose name a path receiver can name
/// as a project type (the kinds `match_method_call`'s outside-type stop
/// counts).
pub(crate) fn is_receiver_type(node: &Node) -> bool {
    node.language == Language::Rust
        && matches!(
            node.kind,
            NodeKind::Class
                | NodeKind::Struct
                | NodeKind::Union
                | NodeKind::Interface
                | NodeKind::Enum
                | NodeKind::Namespace
                | NodeKind::Module
                | NodeKind::Trait
                | NodeKind::TypeAlias
        )
}

/// The unchanged Rust files that write one of `names` as a path receiver,
/// with the names each writes.
pub(crate) fn receiver_dependents(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    names: &BTreeSet<String>,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let mut files = BTreeMap::new();
    if names.is_empty() {
        return Ok(files);
    }
    for file in store.all_files()? {
        if file.language != Language::Rust
            || reindexed.contains(&file.path)
            || removed.contains(&file.path)
        {
            continue;
        }
        let Some(text) = read_text(project_root, &file.path, max_file_size) else {
            continue;
        };
        let written: BTreeSet<String> = names
            .iter()
            .filter(|name| writes_receiver(&text, name))
            .cloned()
            .collect();
        if !written.is_empty() {
            files.insert(file.path, written);
        }
    }
    Ok(files)
}

/// Whether `reference_name` is written through one of `names` as its path
/// receiver: `Name::member` or `Name.member`.
pub(crate) fn is_written_through(reference_name: &str, names: &BTreeSet<String>) -> bool {
    let end = reference_name
        .find([':', '.'])
        .unwrap_or(reference_name.len());
    let rest = &reference_name[end..];
    (rest.starts_with("::") || rest.starts_with('.')) && names.contains(&reference_name[..end])
}

fn read_text(project_root: &Path, path: &str, max_file_size: u64) -> Option<String> {
    match read_source_file(&project_root.join(path), path, max_file_size) {
        Ok((_, SourceText::Text(text))) => Some(text),
        _ => None,
    }
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Whether `text` writes the word `name` followed, past spaces, by `::` or
/// `.`. Over-reads comments and strings, which only re-resolves more.
fn writes_receiver(text: &str, name: &str) -> bool {
    let bytes = text.as_bytes();
    text.match_indices(name).any(|(at, _)| {
        let end = at + name.len();
        (at == 0 || !is_word_byte(bytes[at - 1]))
            && !bytes.get(end).is_some_and(|byte| is_word_byte(*byte))
            && {
                let rest = text[end..].trim_start();
                rest.starts_with("::") || rest.starts_with('.')
            }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reference_is_written_through_its_head() {
        let names = BTreeSet::from(["WidgetRef".to_string()]);
        assert!(is_written_through("WidgetRef::fresh", &names));
        assert!(is_written_through("WidgetRef.fresh", &names));
        assert!(!is_written_through("WidgetRef", &names));
        assert!(!is_written_through("crate::WidgetRef::fresh", &names));
        assert!(!is_written_through("Widget::fresh", &names));
    }

    #[test]
    fn a_receiver_is_the_word_before_a_path_or_a_dot() {
        assert!(writes_receiver("let _w = WidgetRef::fresh();", "WidgetRef"));
        assert!(writes_receiver("WidgetRef :: fresh()", "WidgetRef"));
        assert!(writes_receiver("WidgetRef\n    ::fresh()", "WidgetRef"));
        assert!(writes_receiver("Widget.fresh()", "Widget"));
        assert!(!writes_receiver("let _w: WidgetRef = make();", "WidgetRef"));
        assert!(!writes_receiver("WidgetRefs::fresh()", "WidgetRef"));
        assert!(!writes_receiver("MyWidgetRef::fresh()", "WidgetRef"));
    }
}
