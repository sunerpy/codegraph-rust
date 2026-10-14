use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;

const CORPORA_DIR: &str = "bench/corpora";

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Corpus {
    pub name: &'static str,
    pub url: &'static str,
    /// Release tag the commit was taken from; `None` for a commit pinned
    /// without one.
    pub tag: Option<&'static str>,
    pub commit: &'static str,
    pub subdir: Option<&'static str>,
    pub expected_loc: u64,
    pub expected_files: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct CorpusStatus {
    pub name: String,
    pub url: String,
    pub tag: Option<String>,
    pub commit: String,
    pub subdir: Option<String>,
    pub expected_loc: u64,
    pub expected_files: u64,
    pub fetched: bool,
    pub actual_commit: Option<String>,
    pub actual_loc: Option<u64>,
    pub actual_files: Option<u64>,
    pub path: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct CorpusCounts {
    pub loc: u64,
    pub files: u64,
}

pub const CORPORA: &[Corpus] = &[
    Corpus {
        name: "fd-small",
        url: "https://github.com/sharkdp/fd.git",
        tag: None,
        commit: "25461e5ce13dc12ff2a75993285a87e99b33db2d",
        subdir: Some("src"),
        expected_loc: 4254,
        expected_files: 21,
    },
    Corpus {
        name: "tokio-medium",
        url: "https://github.com/tokio-rs/tokio.git",
        tag: None,
        commit: "ecb5125a6787b9d8eb818b1b00973bcd55ae77c0",
        subdir: Some("tokio/src"),
        expected_loc: 93339,
        expected_files: 373,
    },
    Corpus {
        name: "typescript-large",
        url: "https://github.com/microsoft/TypeScript.git",
        tag: None,
        commit: "7964e22f2b85f16e520f0e902c7fd7b6f0c15416",
        subdir: Some("src"),
        expected_loc: 424376,
        expected_files: 730,
    },
    // A/B timing corpus (JavaScript).
    Corpus {
        name: "express-medium",
        url: "https://github.com/expressjs/express.git",
        tag: None,
        commit: "dae209ae6559c29cfca2a1f4414c51d89ea643d5",
        subdir: None,
        expected_loc: 21463,
        expected_files: 164,
    },
    // Precision corpora (C++, Java, C#, Swift): resolution changes are sampled
    // on these.
    Corpus {
        name: "leveldb-small",
        url: "https://github.com/google/leveldb.git",
        tag: Some("1.23"),
        commit: "99b3c03b3284f5886f9ef9a4ef703d57373e61be",
        subdir: None,
        expected_loc: 25952,
        expected_files: 142,
    },
    Corpus {
        name: "gson-medium",
        url: "https://github.com/google/gson.git",
        tag: Some("gson-parent-2.14.0"),
        commit: "3ff35d6269894901ab8006258395aafc4b9765cd",
        subdir: Some("gson"),
        expected_loc: 43162,
        expected_files: 211,
    },
    Corpus {
        name: "newtonsoft-large",
        url: "https://github.com/JamesNK/Newtonsoft.Json.git",
        tag: Some("13.0.4"),
        commit: "4e13299d4b0ec96bd4df9954ef646bd2d1b5bf2a",
        subdir: Some("Src"),
        expected_loc: 393357,
        expected_files: 968,
    },
    Corpus {
        name: "alamofire-small",
        url: "https://github.com/Alamofire/Alamofire.git",
        tag: Some("5.12.2"),
        commit: "bda9ed57d72988a3a2ada33d824583541f86eac6",
        subdir: Some("Source"),
        expected_loc: 14833,
        expected_files: 44,
    },
    // Performance corpora: C with dense function-pointer dispatch, and Lua.
    Corpus {
        name: "redis-medium",
        url: "https://github.com/redis/redis.git",
        tag: Some("8.10.2"),
        commit: "498ecd0d6d007db11ddb3aea9428552598a78622",
        subdir: Some("src"),
        expected_loc: 215885,
        expected_files: 679,
    },
    Corpus {
        name: "kong-medium",
        url: "https://github.com/Kong/kong.git",
        tag: Some("3.9.3"),
        commit: "a643428bc4d5397152164a63bcc0f8bc65fce69d",
        subdir: Some("kong"),
        expected_loc: 93929,
        expected_files: 592,
    },
];

/// Find a registry corpus by name.
pub fn find_corpus(name: &str) -> Option<Corpus> {
    CORPORA.iter().copied().find(|corpus| corpus.name == name)
}

pub fn corpora_root(workspace_root: &Path) -> PathBuf {
    workspace_root.join(CORPORA_DIR)
}

/// The checkout of `corpus` under a corpora root.
pub fn checkout_path_in(corpora_root: &Path, corpus: Corpus) -> PathBuf {
    corpora_root.join(corpus.name)
}

/// The subtree of `corpus` that is benchmarked, under a corpora root.
pub fn benchmark_path_in(corpora_root: &Path, corpus: Corpus) -> PathBuf {
    let checkout = checkout_path_in(corpora_root, corpus);
    match corpus.subdir {
        Some(subdir) => checkout.join(subdir),
        None => checkout,
    }
}

pub fn corpus_checkout_path(workspace_root: &Path, corpus: Corpus) -> PathBuf {
    checkout_path_in(&corpora_root(workspace_root), corpus)
}

pub fn corpus_benchmark_path(workspace_root: &Path, corpus: Corpus) -> PathBuf {
    benchmark_path_in(&corpora_root(workspace_root), corpus)
}

pub fn list_statuses(workspace_root: &Path) -> Vec<CorpusStatus> {
    list_statuses_in(&corpora_root(workspace_root))
}

/// Registry rows with the live state of each checkout under `corpora_root`.
pub fn list_statuses_in(corpora_root: &Path) -> Vec<CorpusStatus> {
    CORPORA
        .iter()
        .copied()
        .map(|corpus| status(corpora_root, corpus))
        .collect()
}

pub fn fetch_all(workspace_root: &Path) -> Result<Vec<CorpusStatus>> {
    fetch_all_in(&corpora_root(workspace_root))
}

/// Fetch every registry corpus missing under `corpora_root`; an existing
/// checkout is only verified.
pub fn fetch_all_in(corpora_root: &Path) -> Result<Vec<CorpusStatus>> {
    fs::create_dir_all(corpora_root)
        .with_context(|| format!("creating {}", corpora_root.display()))?;
    for corpus in CORPORA.iter().copied() {
        ensure_corpus_in(corpora_root, corpus)?;
    }
    Ok(list_statuses_in(corpora_root))
}

pub fn ensure_corpus(workspace_root: &Path, corpus: Corpus) -> Result<()> {
    ensure_corpus_in(&corpora_root(workspace_root), corpus)
}

pub fn ensure_corpus_in(corpora_root: &Path, corpus: Corpus) -> Result<()> {
    let path = checkout_path_in(corpora_root, corpus);
    if path.exists() {
        let actual = git_rev_parse(&path).ok();
        if actual.as_deref() == Some(corpus.commit) {
            return Ok(());
        }
        bail!(
            "{} exists at {} but expected {}; remove it or fix the pin",
            path.display(),
            actual.unwrap_or_else(|| "unknown".to_string()),
            corpus.commit
        );
    }

    let tmp_path = path.with_extension("tmp");
    if tmp_path.exists() {
        fs::remove_dir_all(&tmp_path)
            .with_context(|| format!("removing stale {}", tmp_path.display()))?;
    }

    fs::create_dir_all(corpora_root)
        .with_context(|| format!("creating {}", corpora_root.display()))?;
    run_git(
        Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg(&tmp_path),
        corpora_root,
    )?;
    run_git(
        Command::new("git")
            .arg("-C")
            .arg(&tmp_path)
            .args(["remote", "add", "origin", corpus.url]),
        corpora_root,
    )?;
    run_git(
        Command::new("git").arg("-C").arg(&tmp_path).args([
            "fetch",
            "--depth",
            "1",
            "origin",
            corpus.commit,
        ]),
        corpora_root,
    )?;
    run_git(
        Command::new("git")
            .arg("-C")
            .arg(&tmp_path)
            .args(["checkout", "--quiet", "FETCH_HEAD"]),
        corpora_root,
    )?;

    let actual = git_rev_parse(&tmp_path)?;
    if actual != corpus.commit {
        bail!(
            "{} fetched {}, expected {}",
            corpus.name,
            actual,
            corpus.commit
        );
    }

    fs::rename(&tmp_path, &path)
        .with_context(|| format!("moving {} to {}", tmp_path.display(), path.display()))?;
    Ok(())
}

pub fn count_corpus(workspace_root: &Path, corpus: Corpus) -> Result<CorpusCounts> {
    count_tree(&corpus_benchmark_path(workspace_root, corpus))
}

pub fn count_tree(root: &Path) -> Result<CorpusCounts> {
    let mut counts = CorpusCounts::default();
    count_tree_inner(root, &mut counts)?;
    Ok(counts)
}

fn status(corpora_root: &Path, corpus: Corpus) -> CorpusStatus {
    let checkout_path = checkout_path_in(corpora_root, corpus);
    let benchmark_path = benchmark_path_in(corpora_root, corpus);
    let fetched = checkout_path.exists();
    let actual_commit = fetched
        .then(|| git_rev_parse(&checkout_path).ok())
        .flatten();
    let counts = fetched.then(|| count_tree(&benchmark_path).ok()).flatten();

    CorpusStatus {
        name: corpus.name.to_string(),
        url: corpus.url.to_string(),
        tag: corpus.tag.map(str::to_string),
        commit: corpus.commit.to_string(),
        subdir: corpus.subdir.map(str::to_string),
        expected_loc: corpus.expected_loc,
        expected_files: corpus.expected_files,
        fetched,
        actual_commit,
        actual_loc: counts.map(|c| c.loc),
        actual_files: counts.map(|c| c.files),
        path: benchmark_path.display().to_string(),
    }
}

fn count_tree_inner(path: &Path, counts: &mut CorpusCounts) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(path).with_context(|| format!("reading {}", path.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if is_ignored_dir(entry.file_name().as_ref()) {
                continue;
            }
            count_tree_inner(&path, counts)?;
        } else if file_type.is_file() && is_source_like(&path) {
            let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            if bytes.contains(&0) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            counts.files += 1;
            counts.loc += text.lines().filter(|line| !line.trim().is_empty()).count() as u64;
        }
    }
    Ok(())
}

fn is_ignored_dir(name: &OsStr) -> bool {
    matches!(
        name.to_string_lossy().as_ref(),
        ".git" | "target" | "node_modules" | "dist" | "build" | "coverage" | ".next"
    )
}

fn is_source_like(path: &Path) -> bool {
    matches!(
        path.extension().and_then(OsStr::to_str),
        Some(
            "rs" | "ts"
                | "tsx"
                | "js"
                | "jsx"
                | "mjs"
                | "cjs"
                | "json"
                | "md"
                | "toml"
                | "yaml"
                | "yml"
                | "py"
                | "go"
                | "java"
                | "c"
                | "cc"
                | "cpp"
                | "h"
                | "hpp"
                | "cs"
                | "rb"
                | "php"
                | "swift"
                | "kt"
                | "lua"
                | "html"
                | "css"
                | "scss"
        )
    )
}

fn git_rev_parse(path: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "HEAD"])
        .output()
        .with_context(|| format!("running git rev-parse in {}", path.display()))?;
    if !output.status.success() {
        bail!("git rev-parse failed in {}", path.display());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_git(command: &mut Command, cwd: &Path) -> Result<()> {
    let output = command
        .current_dir(cwd)
        .output()
        .with_context(|| "running git command")?;
    if output.status.success() {
        return Ok(());
    }

    Err(anyhow!(
        "git command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn registry_names_are_unique_and_pins_are_full_shas() {
        let names: BTreeSet<_> = CORPORA.iter().map(|corpus| corpus.name).collect();
        assert_eq!(names.len(), CORPORA.len(), "corpus names must be unique");
        for corpus in CORPORA {
            assert!(
                corpus.commit.len() == 40
                    && corpus
                        .commit
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                "{} must pin a full lowercase commit SHA, got {}",
                corpus.name,
                corpus.commit
            );
            assert!(
                corpus.url.starts_with("https://github.com/") && corpus.url.ends_with(".git"),
                "{} has an unexpected URL {}",
                corpus.name,
                corpus.url
            );
        }
    }

    #[test]
    fn ab_and_precision_corpora_are_registered() {
        for name in [
            "express-medium",
            "leveldb-small",
            "gson-medium",
            "newtonsoft-large",
            "alamofire-small",
            "redis-medium",
            "kong-medium",
        ] {
            assert!(
                find_corpus(name).is_some(),
                "{name} is missing from CORPORA"
            );
        }
        assert!(find_corpus("does-not-exist").is_none());
    }

    #[test]
    fn every_registry_pin_is_listed_in_the_benchmark_doc() {
        let doc_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/benchmark.md");
        let doc = fs::read_to_string(&doc_path).expect("docs/benchmark.md is readable");
        for corpus in CORPORA {
            let row = doc
                .lines()
                .find(|line| line.starts_with(&format!("| `{}` ", corpus.name)))
                .unwrap_or_else(|| panic!("docs/benchmark.md has no row for {}", corpus.name));
            assert!(
                row.contains(&format!("`{}`", corpus.commit)),
                "the {} row must carry its pinned commit: {row}",
                corpus.name
            );
            assert!(
                row.contains(&format!("`{}`", corpus.subdir.unwrap_or("."))),
                "the {} row must carry its benchmark directory: {row}",
                corpus.name
            );
            if let Some(tag) = corpus.tag {
                assert!(
                    row.contains(&format!("`{tag}`")),
                    "the {} row must carry its tag: {row}",
                    corpus.name
                );
            }
        }
    }

    #[test]
    fn benchmark_paths_join_the_subdirectory() {
        let root = Path::new("/corpora");
        let fd = find_corpus("fd-small").unwrap();
        assert_eq!(checkout_path_in(root, fd), root.join("fd-small"));
        assert_eq!(
            benchmark_path_in(root, fd),
            root.join("fd-small").join("src")
        );
        let express = find_corpus("express-medium").unwrap();
        assert_eq!(
            benchmark_path_in(root, express),
            root.join("express-medium")
        );
    }
}
