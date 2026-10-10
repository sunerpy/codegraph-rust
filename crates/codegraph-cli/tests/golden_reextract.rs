//! Re-extraction gate for the committed golden corpora.
//!
//! `crates/codegraph-bench/tests/equivalence.rs` only proves that each committed
//! `colby.db` and its canonical JSON agree with each other; it never runs the
//! extractor. This suite closes that gap: every re-indexable corpus is indexed
//! afresh from `crates/codegraph-bench/fixtures/<corpus>/` with the binary under
//! test, dumped through the same canonicalizer `bench --gen-golden` uses, and
//! compared byte for byte with `reference/golden/<corpus>/`. A change that moves
//! extraction or resolution output therefore fails here until the affected
//! golden is regenerated (`scripts/regen-goldens.sh --write <corpus>`) and the
//! change is recorded in `docs/equivalence.md`.
//!
//! `mini` is not re-indexable (upstream-derived rows); it is covered by
//! `crates/codegraph-resolve/tests/golden_resolution.rs` instead.

use std::path::{Path, PathBuf};
use std::process::Command;

use codegraph_bench::oracle::{diff_canonical, load_golden, write_golden};

/// The canonical artifacts `bench --gen-golden` writes, compared byte for byte.
const ARTIFACTS: &[&str] = &[
    "nodes.json",
    "edges.json",
    "refs.json",
    "files.json",
    "schema.sql",
];

macro_rules! reextract_corpora {
    ($($corpus:ident),+ $(,)?) => {
        /// Every corpus this suite re-extracts, in declaration order.
        const CORPORA: &[&str] = &[$(stringify!($corpus)),+];

        $(
            mod $corpus {
                #[test]
                fn reextracts_byte_identically() {
                    super::reextract(stringify!($corpus));
                }
            }
        )+
    };
}

reextract_corpora!(
    arkts, cfml, cpp, cuda, dart, erlang, go, godot, kotlin, lua, metal, nix, python, ruby, rust,
    scala, solidity, terraform, typescript,
);

#[test]
fn every_reindexable_golden_corpus_is_re_extracted() {
    let root = workspace_root();
    let mut found: Vec<String> = std::fs::read_dir(root.join("reference/golden"))
        .expect("reference/golden is readable")
        .filter_map(|entry| {
            let entry = entry.expect("reference/golden entry");
            let name = entry.file_name().into_string().ok()?;
            let is_corpus = entry.path().join("colby.db").is_file()
                && root
                    .join("crates/codegraph-bench/fixtures")
                    .join(&name)
                    .is_dir();
            (is_corpus && name != "mini").then_some(name)
        })
        .collect();
    found.sort();
    let mut listed: Vec<String> = CORPORA.iter().map(|name| (*name).to_string()).collect();
    listed.sort();
    assert_eq!(
        found, listed,
        "every re-indexable corpus under reference/golden/ must be listed in reextract_corpora!"
    );
}

fn reextract(corpus: &str) {
    let root = workspace_root();
    let fixture = root.join("crates/codegraph-bench/fixtures").join(corpus);
    let golden = root.join("reference/golden").join(corpus);

    let work = TempDir::new(&format!("work-{corpus}"));
    copy_tree(&fixture, work.path());
    let output = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .arg("init")
        .arg(work.path())
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .env_remove("CODEGRAPH_DIR")
        .output()
        .expect("spawn codegraph init");
    assert!(
        output.status.success(),
        "codegraph init failed for corpus {corpus}:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let dumped = TempDir::new(&format!("golden-{corpus}"));
    write_golden(&work.path().join(".codegraph/codegraph.db"), dumped.path())
        .unwrap_or_else(|error| panic!("dump canonical golden for {corpus}: {error:#}"));

    let changed: Vec<&str> = ARTIFACTS
        .iter()
        .copied()
        .filter(|artifact| {
            let fresh = std::fs::read(dumped.path().join(artifact))
                .unwrap_or_else(|error| panic!("read fresh {artifact}: {error}"));
            let committed = std::fs::read(golden.join(artifact))
                .unwrap_or_else(|error| panic!("read committed {corpus}/{artifact}: {error}"));
            fresh != committed
        })
        .collect();
    if changed.is_empty() {
        return;
    }

    let detail = match (load_golden(&golden), load_golden(dumped.path())) {
        (Ok(expected), Ok(actual)) => match diff_canonical(&expected, &actual, None) {
            Ok(()) => {
                "canonical rows are equal; only artifact bytes differ (e.g. schema.sql order)"
                    .to_string()
            }
            Err(report) => report.to_string(),
        },
        (expected, actual) => format!(
            "could not load goldens for a detailed diff: committed={:?} fresh={:?}",
            expected.err(),
            actual.err()
        ),
    };
    panic!(
        "re-extracting corpus `{corpus}` changed {changed:?}.\n\
         If the change is intended, regenerate it with \
         `scripts/regen-goldens.sh --write {corpus}` and record it in docs/equivalence.md.\n\
         {detail}"
    );
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("codegraph-cli is under crates/")
        .to_path_buf()
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap_or_else(|error| {
        panic!("read fixture directory {}: {error}", from.display());
    }) {
        let entry = entry.expect("fixture entry");
        let source = entry.path();
        let target = to.join(entry.file_name());
        let file_type = entry.file_type().expect("fixture entry type");
        if file_type.is_dir() {
            std::fs::create_dir_all(&target).expect("create fixture subdirectory");
            copy_tree(&source, &target);
        } else if file_type.is_file() {
            std::fs::copy(&source, &target).expect("copy fixture file");
        } else {
            panic!(
                "fixture {} contains a non-regular entry; goldens must be built from plain files",
                source.display()
            );
        }
    }
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-golden-reextract-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
