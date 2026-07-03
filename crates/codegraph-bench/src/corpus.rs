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
    pub commit: &'static str,
    pub subdir: Option<&'static str>,
    pub expected_loc: u64,
    pub expected_files: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct CorpusStatus {
    pub name: String,
    pub url: String,
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
        commit: "25461e5ce13dc12ff2a75993285a87e99b33db2d",
        subdir: Some("src"),
        expected_loc: 4254,
        expected_files: 21,
    },
    Corpus {
        name: "tokio-medium",
        url: "https://github.com/tokio-rs/tokio.git",
        commit: "ecb5125a6787b9d8eb818b1b00973bcd55ae77c0",
        subdir: Some("tokio/src"),
        expected_loc: 93339,
        expected_files: 373,
    },
    Corpus {
        name: "typescript-large",
        url: "https://github.com/microsoft/TypeScript.git",
        commit: "7964e22f2b85f16e520f0e902c7fd7b6f0c15416",
        subdir: Some("src"),
        expected_loc: 424376,
        expected_files: 730,
    },
];

pub fn corpora_root(workspace_root: &Path) -> PathBuf {
    workspace_root.join(CORPORA_DIR)
}

pub fn corpus_checkout_path(workspace_root: &Path, corpus: Corpus) -> PathBuf {
    corpora_root(workspace_root).join(corpus.name)
}

pub fn corpus_benchmark_path(workspace_root: &Path, corpus: Corpus) -> PathBuf {
    match corpus.subdir {
        Some(subdir) => corpus_checkout_path(workspace_root, corpus).join(subdir),
        None => corpus_checkout_path(workspace_root, corpus),
    }
}

pub fn list_statuses(workspace_root: &Path) -> Vec<CorpusStatus> {
    CORPORA
        .iter()
        .copied()
        .map(|corpus| status(workspace_root, corpus))
        .collect()
}

pub fn fetch_all(workspace_root: &Path) -> Result<Vec<CorpusStatus>> {
    fs::create_dir_all(corpora_root(workspace_root)).with_context(|| "creating bench/corpora")?;
    for corpus in CORPORA.iter().copied() {
        ensure_corpus(workspace_root, corpus)?;
    }
    Ok(list_statuses(workspace_root))
}

pub fn ensure_corpus(workspace_root: &Path, corpus: Corpus) -> Result<()> {
    let path = corpus_checkout_path(workspace_root, corpus);
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

    fs::create_dir_all(corpora_root(workspace_root)).with_context(|| "creating corpora root")?;
    run_git(
        Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg(&tmp_path),
        workspace_root,
    )?;
    run_git(
        Command::new("git")
            .arg("-C")
            .arg(&tmp_path)
            .args(["remote", "add", "origin", corpus.url]),
        workspace_root,
    )?;
    run_git(
        Command::new("git").arg("-C").arg(&tmp_path).args([
            "fetch",
            "--depth",
            "1",
            "origin",
            corpus.commit,
        ]),
        workspace_root,
    )?;
    run_git(
        Command::new("git")
            .arg("-C")
            .arg(&tmp_path)
            .args(["checkout", "--quiet", "FETCH_HEAD"]),
        workspace_root,
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

fn status(workspace_root: &Path, corpus: Corpus) -> CorpusStatus {
    let checkout_path = corpus_checkout_path(workspace_root, corpus);
    let fetched = checkout_path.exists();
    let actual_commit = fetched
        .then(|| git_rev_parse(&checkout_path).ok())
        .flatten();
    let counts = fetched
        .then(|| count_corpus(workspace_root, corpus).ok())
        .flatten();

    CorpusStatus {
        name: corpus.name.to_string(),
        url: corpus.url.to_string(),
        commit: corpus.commit.to_string(),
        subdir: corpus.subdir.map(str::to_string),
        expected_loc: corpus.expected_loc,
        expected_files: corpus.expected_files,
        fetched,
        actual_commit,
        actual_loc: counts.map(|c| c.loc),
        actual_files: counts.map(|c| c.files),
        path: corpus_benchmark_path(workspace_root, corpus)
            .display()
            .to_string(),
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
