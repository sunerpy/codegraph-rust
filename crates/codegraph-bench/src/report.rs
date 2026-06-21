use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::corpus::CorpusStatus;
use crate::pipeline::{CorpusBenchmark, PipelineMeta};
use crate::runner::RunSummary;

/// Task-26 full benchmark report (`results.json`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PipelineReport {
    pub schema_version: u32,
    pub kind: String,
    pub environment: Environment,
    pub meta: PipelineMeta,
    pub corpora: Vec<CorpusBenchmark>,
}

impl PipelineReport {
    pub fn new(workspace_root: &Path, meta: PipelineMeta, corpora: Vec<CorpusBenchmark>) -> Self {
        Self {
            schema_version: 1,
            kind: "task-26-full-benchmark".to_string(),
            environment: Environment::detect(workspace_root),
            meta,
            corpora,
        }
    }
}

pub fn write_pipeline_report(path: &Path, report: &PipelineReport) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(report).context("serializing pipeline report")?;
    fs::write(path, json).with_context(|| format!("writing {}", path.display()))
}

#[derive(Clone, Debug, Serialize)]
pub struct BenchmarkReport {
    pub schema_version: u32,
    pub environment: Environment,
    pub corpora: Vec<CorpusStatus>,
    pub results: BTreeMap<String, CorpusResult>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Environment {
    pub cpu_model: Option<String>,
    pub memory_total_kb: Option<u64>,
    pub os: Option<String>,
    pub kernel: Option<String>,
    pub node_version: Option<String>,
    pub rustc_version: Option<String>,
    pub rust_impl_commit: Option<String>,
    pub typescript_impl_commit: Option<String>,
    pub jake_rust_reference_commit: Option<String>,
    pub rss_collector: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CorpusResult {
    pub implementations: BTreeMap<String, ImplResult>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ImplResult {
    pub metrics: BTreeMap<String, MetricResult>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MetricResult {
    Latency(RunSummary),
    Scalar { value: u64, unit: String },
}

impl BenchmarkReport {
    pub fn smoke(workspace_root: &Path, summary: RunSummary, corpora: Vec<CorpusStatus>) -> Self {
        let mut impl_result = ImplResult::default();
        impl_result.metrics.insert(
            "smoke_latency_ms".to_string(),
            MetricResult::Latency(summary),
        );

        let mut corpus_result = CorpusResult::default();
        corpus_result
            .implementations
            .insert("smoke".to_string(), impl_result);

        let mut results = BTreeMap::new();
        results.insert("smoke".to_string(), corpus_result);

        Self {
            schema_version: 1,
            environment: Environment::detect(workspace_root),
            corpora,
            results,
        }
    }
}

impl Environment {
    pub fn detect(workspace_root: &Path) -> Self {
        Self {
            cpu_model: read_cpu_model(),
            memory_total_kb: read_mem_total_kb(),
            os: read_os_pretty_name(),
            kernel: command_stdout("uname", &["-sr"]),
            node_version: command_stdout("node", &["--version"]),
            rustc_version: command_stdout("rustc", &["--version"]),
            rust_impl_commit: command_stdout_in(workspace_root, "git", &["rev-parse", "HEAD"]),
            typescript_impl_commit: read_reference_pin(
                workspace_root,
                "Upstream TypeScript Implementation",
            ),
            jake_rust_reference_commit: read_reference_pin(
                workspace_root,
                "Jake's Rust Implementation",
            ),
            rss_collector: "/proc/<pid>/status VmHWM polling".to_string(),
        }
    }
}

pub fn write_report(path: &Path, report: &BenchmarkReport) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(report).context("serializing benchmark report")?;
    fs::write(path, json).with_context(|| format!("writing {}", path.display()))
}

fn read_cpu_model() -> Option<String> {
    fs::read_to_string("/proc/cpuinfo")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("model name\t: ").map(str::to_string))
}

fn read_mem_total_kb() -> Option<u64> {
    fs::read_to_string("/proc/meminfo")
        .ok()?
        .lines()
        .find_map(|line| {
            let value = line.strip_prefix("MemTotal:")?.trim();
            value.split_whitespace().next()?.parse().ok()
        })
}

fn read_os_pretty_name() -> Option<String> {
    fs::read_to_string("/etc/os-release")
        .ok()?
        .lines()
        .find_map(|line| {
            let value = line.strip_prefix("PRETTY_NAME=")?;
            Some(value.trim_matches('"').to_string())
        })
}

fn read_reference_pin(workspace_root: &Path, heading: &str) -> Option<String> {
    let pins = fs::read_to_string(workspace_root.join("reference/PINS.md")).ok()?;
    let mut in_section = false;
    for line in pins.lines() {
        if line.starts_with("## ") {
            in_section = line.contains(heading);
        }
        if in_section {
            if let Some(commit) = line.trim().strip_prefix("**Commit:** `") {
                let commit = commit.trim_end_matches('`').trim_end_matches('`');
                return Some(commit.to_string());
            }
        }
    }
    None
}

fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    command_stdout_in(Path::new("."), program, args)
}

fn command_stdout_in(cwd: &Path, program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .current_dir(cwd)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}
