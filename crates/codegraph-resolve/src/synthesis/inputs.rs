//! The files the synthesis stage reads that the index does not hold
//! (W7-17b): the `#include`d files of C and C++ code the scan does not
//! index, such as a `.def` table, read as upstream's `fileExists` and
//! `readFile` read them, within the project's scan scope.
//!
//! Each file a stage run asks for is recorded with its text's hash, or
//! `null` when it is not read, under [`INPUTS_KEY`]; a sync that finds one
//! reading differently recomputes the stage.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use codegraph_core::node_id::hash_content;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_extract::ExtractOptions;
use codegraph_extract::engine::{Membership, ScanScope};
use codegraph_store::Store;

use crate::pathutil::lexical_relative;

/// `project_metadata` key of what the last stage run read beside the index.
pub const INPUTS_KEY: &str = "synthesis_unindexed_inputs";

/// Project-relative path → the hash of the text a stage run read there, or
/// `None` for a file it did not read: absent, outside the scan scope,
/// reached through a symbolic link, or larger than `indexing.max_file_size`.
pub type Inputs = BTreeMap<String, Option<String>>;

/// The stage's reader of the files the index does not hold, recording each.
pub(super) struct UnindexedReader {
    root: PathBuf,
    scope: ScanScope,
    max_file_size: u64,
    texts: RefCell<HashMap<String, Option<Arc<str>>>>,
    recorded: RefCell<Inputs>,
}

impl UnindexedReader {
    pub(super) fn new(root: &Path, options: &ExtractOptions) -> Self {
        Self {
            root: root.to_path_buf(),
            scope: ScanScope::new(root, options),
            max_file_size: options.max_file_size,
            texts: RefCell::new(HashMap::new()),
            recorded: RefCell::new(Inputs::new()),
        }
    }

    /// The project-relative form of `path` the reader reads and records it
    /// under, `None` for a path that would leave the project root (never
    /// probed, never recorded).
    pub(super) fn relative(path: &str) -> Option<String> {
        lexical_relative(path)
    }

    /// `path`'s text, recorded: `None` for a path outside the root and for
    /// a file [`read`] does not read.
    pub(super) fn text(&self, path: &str) -> Option<Arc<str>> {
        let relative = Self::relative(path)?;
        if let Some(cached) = self.texts.borrow().get(&relative) {
            return cached.clone();
        }
        let text = read(&self.root, &self.scope, self.max_file_size, &relative);
        self.recorded
            .borrow_mut()
            .insert(relative.clone(), text.as_deref().map(hash_content));
        self.texts.borrow_mut().insert(relative, text.clone());
        text
    }

    pub(super) fn into_inputs(self) -> Inputs {
        self.recorded.into_inner()
    }
}

/// The text of the project-relative `relative` when the scan's ignore model
/// admits it as a file, whatever its extension, no symbolic link on its
/// way, and it is a regular file no larger than `max_file_size`, read as
/// the index reads source.
fn read(root: &Path, scope: &ScanScope, max_file_size: u64, relative: &str) -> Option<Arc<str>> {
    if scope.admits_file(relative) != Membership::Admitted {
        return None;
    }
    let (_, source) = read_source_file(&root.join(relative), relative, max_file_size).ok()?;
    let SourceText::Text(text) = source else {
        return None;
    };
    Some(Arc::from(text))
}

/// Record what a stage run read beside the index.
pub(super) fn write(store: &Store, inputs: &Inputs) -> Result<()> {
    store.set_project_metadata(INPUTS_KEY, &serde_json::to_string(inputs)?)?;
    Ok(())
}

/// What the last stage run recorded; empty for no row, `None` for a row
/// that does not parse.
pub fn recorded(store: &Store) -> Result<Option<Inputs>> {
    Ok(match store.get_project_metadata(INPUTS_KEY)? {
        None => Some(Inputs::new()),
        Some(row) => serde_json::from_str(&row).ok(),
    })
}

/// Whether a file the last stage run recorded reads differently now: its
/// text changed, it appeared, it went, or it moved in or out of the scan
/// scope. A row that does not parse counts as changed.
pub fn inputs_changed(
    store: &Store,
    project_root: &Path,
    options: &ExtractOptions,
) -> Result<bool> {
    let Some(recorded) = recorded(store)? else {
        return Ok(true);
    };
    if recorded.is_empty() {
        return Ok(false);
    }
    let scope = ScanScope::new(project_root, options);
    Ok(recorded.iter().any(|(path, hash)| {
        read(project_root, &scope, options.max_file_size, path)
            .as_deref()
            .map(hash_content)
            != *hash
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temporary directory holding a `project` root and, beside it, an
    /// index and a file outside the root.
    fn sandbox(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "cg-synthesis-inputs-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = dir.join("project");
        std::fs::create_dir_all(&root).unwrap();
        (dir, root)
    }

    fn inputs(rows: &[(&str, Option<&str>)]) -> Inputs {
        rows.iter()
            .map(|(path, text)| (path.to_string(), text.map(hash_content)))
            .collect()
    }

    /// The reader reads a file in the scan scope by its project-relative
    /// path and records every file it is asked for once, with its text's
    /// hash or null; a path leaving the root is neither read nor recorded.
    #[test]
    fn the_reader_records_each_file_it_is_asked_for() {
        let (dir, root) = sandbox("reader");
        std::fs::write(root.join("ops.def"), "table\n").unwrap();
        std::fs::write(root.join("big.def"), "x".repeat(64)).unwrap();
        std::fs::write(root.join("excluded.def"), "excluded\n").unwrap();
        std::fs::write(dir.join("outside.def"), "outside\n").unwrap();
        let options = ExtractOptions {
            max_file_size: 32,
            exclude: vec!["excluded.def".to_string()],
            ..ExtractOptions::default()
        };
        let reader = UnindexedReader::new(&root, &options);
        assert_eq!(reader.text("ops.def").as_deref(), Some("table\n"));
        assert_eq!(reader.text("./sub/../ops.def").as_deref(), Some("table\n"));
        assert_eq!(reader.text("missing.def"), None);
        assert_eq!(reader.text("big.def"), None, "over indexing.max_file_size");
        assert_eq!(reader.text("excluded.def"), None, "outside the scan scope");
        assert_eq!(reader.text("../outside.def"), None);
        assert_eq!(reader.text("sub/../../outside.def"), None);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("ops.def", root.join("linked.def")).unwrap();
            assert_eq!(reader.text("linked.def"), None, "through a symbolic link");
        }
        let mut expected = inputs(&[
            ("big.def", None),
            ("excluded.def", None),
            ("missing.def", None),
            ("ops.def", Some("table\n")),
        ]);
        if cfg!(unix) {
            expected.insert("linked.def".to_string(), None);
        }
        assert_eq!(reader.into_inputs(), expected);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A recorded file reads differently when its text changes, it appears,
    /// it goes, or the scan scope takes it in or out; a row that does not
    /// parse counts as changed, and no row as nothing read.
    #[test]
    fn a_recorded_file_reading_differently_is_a_change() {
        let (dir, root) = sandbox("changed");
        let store = Store::open(&dir.join("index.db")).unwrap();
        let options = ExtractOptions::default();
        let changed = |options: &ExtractOptions| inputs_changed(&store, &root, options).unwrap();
        assert_eq!(recorded(&store).unwrap(), Some(Inputs::new()));
        assert!(!changed(&options), "no row: nothing was read");

        std::fs::write(root.join("ops.def"), "one\n").unwrap();
        let row = inputs(&[("gone.def", None), ("ops.def", Some("one\n"))]);
        write(&store, &row).unwrap();
        assert_eq!(recorded(&store).unwrap(), Some(row));
        assert!(!changed(&options), "every file reads as recorded");

        std::fs::write(root.join("ops.def"), "two\n").unwrap();
        assert!(changed(&options), "its text changed");
        std::fs::write(root.join("ops.def"), "one\n").unwrap();
        assert!(!changed(&options));

        std::fs::write(root.join("gone.def"), "new\n").unwrap();
        assert!(changed(&options), "a missing file appeared");
        std::fs::remove_file(root.join("gone.def")).unwrap();
        assert!(!changed(&options));

        std::fs::remove_file(root.join("ops.def")).unwrap();
        assert!(changed(&options), "a read file went");
        std::fs::write(root.join("ops.def"), "one\n").unwrap();
        assert!(!changed(&options));

        std::fs::write(root.join(".gitignore"), "ops.def\n").unwrap();
        assert!(changed(&options), "the root .gitignore ignores it");
        std::fs::remove_file(root.join(".gitignore")).unwrap();
        let excluded = ExtractOptions {
            exclude: vec!["ops.def".to_string()],
            ..ExtractOptions::default()
        };
        assert!(changed(&excluded), "the configuration excludes it");
        assert!(!changed(&options));

        store.set_project_metadata(INPUTS_KEY, "not json").unwrap();
        assert_eq!(recorded(&store).unwrap(), None);
        assert!(changed(&options), "a row that does not parse");

        write(&store, &Inputs::new()).unwrap();
        assert_eq!(
            store.get_project_metadata(INPUTS_KEY).unwrap().as_deref(),
            Some("{}")
        );
        assert!(!changed(&options));
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }
}
