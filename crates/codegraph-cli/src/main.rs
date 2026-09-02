//! Single `codegraph` CLI binary.
//!
//! This crate owns process bootstrap: load config fail-fast, initialize tracing,
//! keep the `WorkerGuard` alive, then run the requested command. Library crates
//! only emit tracing events.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{self, BufRead, BufReader, IsTerminal, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate};
use codegraph_core::config::Config;
use codegraph_core::deprioritize::DeprioritizeMatcher;
use codegraph_core::generated_header::detect_generated_file;
use codegraph_core::logger::{LoggerConfig, init_logger};
use codegraph_core::node_id::hash_content;
use codegraph_core::types::{ExtractionResult, FileRecord, Language, Node, NodeKind};
use codegraph_extract::{ExtractOptions, detect_language_with, extract_source_with_observer};
use codegraph_graph::graph::{GodotReach, GraphTraverser};
use codegraph_graph::query::{SearchOptions, search_nodes};
use codegraph_graph::{segment_match, segments};
use codegraph_mcp::{McpServer, RunUntilAdoption};
use codegraph_resolve::ReferenceResolver;
use codegraph_store::queries::SearchResult;
use codegraph_store::{CorruptReason, ExtractionStatus, IndexLease, SlotOutcome, Store};
use diagnostics::{DiagnosticArgs, DiagnosticRun, IndexTracker};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use serde::Serialize;
use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

mod diagnostics;
mod installer;
mod structural_gate;

/// Test-only: the ONE process-wide environment lock for this binary.
///
/// `cargo test` runs every unit test of this binary on threads of a SINGLE
/// process, so `HOME`, `XDG_DATA_HOME`, `USERPROFILE`, … are shared mutable
/// state. Two independent `ENV_LOCK` statics used to live in this file (one per
/// test module) plus a third in `installer::tests`, and because distinct statics
/// do not exclude each other, a test holding "the" lock could still have `HOME`
/// swapped underneath it — which made `install_completions` write into the
/// developer's REAL home directory. Every test in this binary that mutates a
/// process-global env var must therefore go through [`test_env::EnvGuard`].
#[cfg(test)]
pub(crate) mod test_env {
    use std::ffi::{OsStr, OsString};
    use std::sync::{Mutex, MutexGuard};

    /// The single env lock for the whole `codegraph` binary's test suite.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Holds [`ENV_LOCK`] for its whole lifetime, records every variable it
    /// touched, and restores all of them on drop (panic-safe, so a failing
    /// assertion cannot leak a temp `HOME` into the rest of the suite).
    pub(crate) struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Vec<(String, Option<OsString>)>,
        expected: Vec<(String, Option<OsString>)>,
    }

    /// Acquire the process-wide env lock. Poisoning is recovered so one failing
    /// test does not cascade into every other env test.
    pub(crate) fn env_guard() -> EnvGuard {
        EnvGuard {
            _lock: ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner()),
            saved: Vec::new(),
            expected: Vec::new(),
        }
    }

    impl EnvGuard {
        fn remember(&mut self, key: &str) {
            if !self.saved.iter().any(|(k, _)| k == key) {
                self.saved.push((key.to_string(), std::env::var_os(key)));
            }
        }

        fn expect(&mut self, key: &str, value: Option<OsString>) {
            match self.expected.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = value,
                None => self.expected.push((key.to_string(), value)),
            }
        }

        /// Set `key`, then assert the write is observable — so a stolen variable
        /// is a LOUD failure instead of a silent write to the real `$HOME`.
        pub(crate) fn set(&mut self, key: &str, value: impl AsRef<OsStr>) -> &mut Self {
            let value = value.as_ref().to_os_string();
            self.remember(key);
            // SAFETY: ENV_LOCK is held for this guard's whole lifetime, so no
            // other test thread of this binary reads or writes env concurrently.
            unsafe { std::env::set_var(key, &value) };
            self.expect(key, Some(value));
            self.assert_intact();
            self
        }

        /// Unset `key` and assert it stays unset.
        pub(crate) fn remove(&mut self, key: &str) -> &mut Self {
            self.remember(key);
            // SAFETY: as in `set` — serialized by ENV_LOCK.
            unsafe { std::env::remove_var(key) };
            self.expect(key, None);
            self.assert_intact();
            self
        }

        /// Panic if any variable this guard wrote has changed value since.
        ///
        /// This is the escape detector: if some future test mutates `HOME`
        /// without taking [`ENV_LOCK`], the test that owns the guard fails with
        /// a precise message instead of writing into the real home directory.
        pub(crate) fn assert_intact(&self) {
            for (key, want) in &self.expected {
                assert_eq!(
                    &std::env::var_os(key),
                    want,
                    "env var {key} changed underneath this guard: another test \
                     mutated process-global env without holding the shared lock"
                );
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.saved.drain(..).rev() {
                match value {
                    // SAFETY: still holding ENV_LOCK; single-threaded here.
                    Some(v) => unsafe { std::env::set_var(&key, v) },
                    None => unsafe { std::env::remove_var(&key) },
                }
            }
        }
    }
}

const VERSION: &str = env!("CARGO_PKG_VERSION");
const PARSE_REORDER_WINDOW: usize = 512;
// MSVC executables start with a 1 MiB main-thread stack. Keep CLI growth and
// indexing work independent of that platform default.
const CLI_MAIN_STACK_BYTES: usize = 8 * 1024 * 1024;

fn main() {
    let main_thread = match std::thread::Builder::new()
        .name("codegraph-main".to_string())
        .stack_size(CLI_MAIN_STACK_BYTES)
        .spawn(cli_main)
    {
        Ok(handle) => handle,
        Err(err) => {
            eprintln!("CodeGraph startup error: failed to create the CLI thread: {err}");
            std::process::exit(1);
        }
    };

    if let Err(payload) = main_thread.join() {
        std::panic::resume_unwind(payload);
    }
}

fn cli_main() {
    let cli = Cli::parse();
    // Process bootstrap has no addressed project yet, so this config is
    // `APP_CONFIG`-or-defaults ONLY and may configure NOTHING but the logger
    // below. Every project operation (index, sync, watch, an MCP request) loads
    // the addressed project's own immutable config from its resolved index root.
    let config = match Config::load_env_or_default(None) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("CodeGraph config error: {err:#}");
            std::process::exit(1);
        }
    };

    // Logs go to STDERR, never stdout: `serve --mcp` owns stdout for the
    // JSON-RPC stream and a single log byte there corrupts the protocol. The
    // detached daemon / HTTP children re-enter this same path and have their
    // stderr redirected to a log file, so their events land there WITH the
    // subscriber's RFC3339 timestamps. `file` stays off — the child fd
    // redirect, not a second rolling file, is the on-disk sink.
    let logger_cfg = LoggerConfig {
        level: effective_log_level(&config.app.log_level),
        stdout: false,
        stderr: true,
        file: false,
        ..Default::default()
    };
    let _guard = match init_logger(&logger_cfg) {
        Ok(guard) => guard,
        Err(err) => {
            eprintln!("CodeGraph logger error: {err}");
            std::process::exit(1);
        }
    };

    if let Err(err) = run(cli) {
        eprintln!("Error: {err:#}");
        if let Some(guidance) = index_removal_holder_guidance(&err) {
            eprint!("{guidance}");
        }
        std::process::exit(1);
    }
}

#[derive(Debug, Parser)]
#[command(name = "codegraph")]
#[command(version = VERSION)]
#[command(about = "Code intelligence and knowledge graph for any codebase")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    // Upstream flags/output: upstream bin/codegraph.ts:420-424, 431-470.
    Init {
        /// Project root. Lifecycle commands accept this as an optional
        /// positional path.
        path: Option<PathBuf>,
        /// Also write project-level MCP config for these agents (csv ids,
        /// `auto`, `all`, `none`). Defaults to `none` (index only). Editors that
        /// launch the server from a non-project CWD (Kiro, Cursor) need this to
        /// get the project's absolute `--path`.
        #[arg(short, long, default_value = "none")]
        target: String,
        /// Accepted for non-interactive bootstrap compatibility. `init` has no
        /// confirmation prompt today, so this is intentionally behavior-neutral.
        #[arg(short = 'y', long)]
        yes: bool,
        #[command(flatten)]
        diagnostics: DiagnosticArgs,
    },
    // Upstream flags/output: upstream bin/codegraph.ts:482-485, 489-527.
    Uninit {
        /// Project root. Lifecycle commands accept this as an optional
        /// positional path.
        path: Option<PathBuf>,
        #[arg(short, long)]
        force: bool,
    },
    // Upstream flags/output: upstream bin/codegraph.ts:536-540, 545-596.
    Index {
        /// Project root. Lifecycle commands accept this as an optional
        /// positional path.
        path: Option<PathBuf>,
        #[arg(short, long)]
        force: bool,
        #[arg(short, long)]
        quiet: bool,
        #[arg(short, long)]
        verbose: bool,
        #[command(flatten)]
        diagnostics: DiagnosticArgs,
    },
    // Upstream flags/output: upstream bin/codegraph.ts:605-608, 612-657.
    Sync {
        /// Project root. Lifecycle commands accept this as an optional
        /// positional path.
        path: Option<PathBuf>,
        #[arg(short, long)]
        quiet: bool,
        #[command(flatten)]
        diagnostics: DiagnosticArgs,
    },
    // Upstream flags/output shape: upstream bin/codegraph.ts:667-670, 679-738, 743-820.
    Status {
        /// Project root. Lifecycle commands accept this as an optional
        /// positional path.
        path: Option<PathBuf>,
        #[arg(short = 'j', long = "json")]
        json: bool,
    },
    // Upstream `query` flags/output shape: upstream bin/codegraph.ts:831-837,
    // 849-887. `search` is the canonical CLI name; `query` stays compatible.
    /// Search indexed symbols by name.
    #[command(visible_alias = "query")]
    Search {
        search: String,
        /// Project root. Query commands require `-p/--path`; do not append the
        /// project as another positional argument.
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short, long, default_value_t = 10)]
        limit: i64,
        #[arg(short, long)]
        kind: Option<String>,
        #[arg(short = 'j', long = "json")]
        json: bool,
        /// Exit non-zero when no result is found (default exits 0).
        #[arg(long)]
        strict: bool,
    },
    // Upstream flags/output shape: upstream bin/codegraph.ts:903-911, 939-1013.
    Files {
        /// Project root. Query commands require `-p/--path`; do not append the
        /// project as another positional argument.
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// Filter to files under this directory (path prefix).
        #[arg(long, value_name = "DIR")]
        filter: Option<String>,
        /// Filter to files of this language (matches `status` names, e.g. gdscript, godot_scene).
        #[arg(long, value_name = "LANG")]
        language: Option<String>,
        #[arg(long)]
        pattern: Option<String>,
        #[arg(long, value_enum, default_value_t = FilesFormat::Tree)]
        format: FilesFormat,
        #[arg(long)]
        max_depth: Option<usize>,
        #[arg(short = 'j', long = "json")]
        json: bool,
    },
    // Upstream flags/output: upstream bin/codegraph.ts:1110-1115, 1124-1156.
    Serve {
        /// Project root to pin. Use `-p/--path`; this command does not accept a
        /// positional project path.
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(long)]
        mcp: bool,
        #[arg(long = "no-watch")]
        no_watch: bool,
        /// Serve MCP over streamable-HTTP (rmcp) instead of stdio. With `--path`
        /// it pins one already-indexed project; without `--path` it starts a
        /// GLOBAL server where each call carries its own `projectPath`. The
        /// DNS-rebinding host guard is OPEN by default — any `Host` is accepted
        /// (MCP Inspector, Zed, curl connect out of the box). Restrict it by
        /// setting `CODEGRAPH_HTTP_ALLOWED_HOSTS` to a comma list of allowed
        /// hosts (e.g. `localhost,code-server:12025`); a `*` entry (or unset)
        /// means allow all.
        #[arg(long)]
        http: bool,
        /// Address to bind the streamable-HTTP server to (loopback or a real
        /// interface such as `0.0.0.0`). The host guard is OPEN by default; when
        /// restricted via `CODEGRAPH_HTTP_ALLOWED_HOSTS` the loopback defaults
        /// plus this bind authority are always allowed alongside the listed hosts.
        #[arg(long = "http-addr", default_value = "127.0.0.1:8111")]
        http_addr: String,
        /// Run the HTTP MCP server in the BACKGROUND (detached) instead of the
        /// foreground. Only meaningful with `--http`; the parent registers the
        /// server, prints its pid + log path, and exits. Without `--http` this
        /// flag is a hard error.
        #[arg(long)]
        detach: bool,
    },
    // Upstream flags/output: upstream bin/codegraph.ts:1167-1169, 1173-1186.
    Unlock {
        /// Project root. Lifecycle commands accept this as an optional
        /// positional path.
        path: Option<PathBuf>,
    },
    // Upstream flags/output shape: upstream bin/codegraph.ts:1201-1205, 1219-1267.
    Callers {
        symbol: String,
        /// Project root. Query commands require `-p/--path`; do not append the
        /// project as another positional argument.
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        #[arg(short = 'j', long = "json")]
        json: bool,
        /// Exit non-zero when no callers are found (default exits 0).
        #[arg(long)]
        strict: bool,
        /// Disambiguate same-named definitions: keep only the one defined in this
        /// file (exact project-relative path or a trailing path suffix).
        #[arg(long, value_name = "FILE")]
        file: Option<String>,
    },
    // Upstream flags/output shape: upstream bin/codegraph.ts:1280-1284, 1298-1345.
    Callees {
        symbol: String,
        /// Project root. Query commands require `-p/--path`; do not append the
        /// project as another positional argument.
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        #[arg(short = 'j', long = "json")]
        json: bool,
        /// Exit non-zero when no callees are found (default exits 0).
        #[arg(long)]
        strict: bool,
        /// Disambiguate same-named definitions: keep only the one defined in this
        /// file (exact project-relative path or a trailing path suffix).
        #[arg(long, value_name = "FILE")]
        file: Option<String>,
    },
    // Upstream flags/output shape: upstream bin/codegraph.ts:1358-1362, 1374-1439.
    Impact {
        symbol: String,
        /// Project root. Query commands require `-p/--path`; do not append the
        /// project as another positional argument.
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short, long, default_value_t = 2)]
        depth: usize,
        #[arg(short = 'j', long = "json")]
        json: bool,
        /// Exit non-zero when the symbol is not found (default exits 0).
        #[arg(long)]
        strict: bool,
        /// Disambiguate same-named definitions: keep only the one defined in this
        /// file (exact project-relative path or a trailing path suffix).
        #[arg(long, value_name = "FILE")]
        file: Option<String>,
    },
    // Upstream flags/output shape: upstream bin/codegraph.ts:1462-1469, 1479-1582.
    Affected {
        files: Vec<String>,
        /// Project root. Query commands require `-p/--path`; do not append the
        /// project as another positional argument.
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short, long, default_value_t = 5)]
        depth: usize,
        /// glob used to classify affectedTests; does NOT filter affectedFiles. Example: tests/*
        #[arg(short, long, value_name = "GLOB")]
        filter: Option<String>,
    },
    // New analysis surface (not in the v1.0.1 pin): forward file-dependency
    // cycle detection. Ports `findCircularDependencies`
    // (upstream graph/queries.ts:225-263).
    Check {
        /// Project root. Query commands require `-p/--path`; do not append the
        /// project as another positional argument.
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short = 'j', long = "json")]
        json: bool,
    },
    /// Read-only Godot resource audit: orphan resources, dangling references,
    /// and reverse-dependency impact. Computed from the existing graph + disk
    /// checks; adds no extraction and is separate from `check`.
    Audit {
        /// Project root (use `-p/--path`; NOT a result filter — use
        /// `--include`/`--exclude` to narrow).
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// Report `.tres`/`.tscn` resources nothing references.
        #[arg(long)]
        orphans: bool,
        /// Report path references whose target is missing on disk.
        #[arg(long)]
        dangling: bool,
        /// Report what references the given changed resource/script path.
        #[arg(long, value_name = "PATH")]
        impact: Option<String>,
        /// With --impact: emit a derived load/open plan (loadScripts/loadResources/openScenes/reasons).
        #[arg(long = "verify-plan", requires = "impact")]
        verify_plan: bool,
        /// Keep only results whose path is under this prefix (repeatable).
        #[arg(long, value_name = "PREFIX")]
        include: Vec<String>,
        /// Drop results whose path is under this prefix, e.g. addons/ (repeatable).
        #[arg(long, value_name = "PREFIX")]
        exclude: Vec<String>,
        #[arg(short = 'j', long = "json")]
        json: bool,
    },
    Export {
        /// Project root. Query commands require `-p/--path`; do not append the
        /// project as another positional argument.
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short = 'o', long = "out")]
        out: Option<PathBuf>,
        #[arg(long = "no-centrality")]
        no_centrality: bool,
    },
    /// Explore an area of the codebase (the shell equivalent of the MCP
    /// `codegraph_explore` tool). Runs the SAME deterministic engine and prints
    /// the same output — relevant symbols' verbatim source grouped by file plus
    /// the call paths between them. NO LLM.
    Explore {
        /// Symbol names or a natural-language question to explore.
        query: String,
        /// Project root. Query commands require `-p/--path`; do not append the
        /// project as another positional argument.
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// Max number of files to include source from (clamped 1..=20).
        #[arg(long = "max-files")]
        max_files: Option<usize>,
        #[arg(short = 'j', long = "json")]
        json: bool,
    },
    /// Read one symbol or file (the shell equivalent of the MCP `codegraph_node`
    /// tool). Runs the SAME engine: for a symbol it prints its source plus the
    /// caller/callee trail; for a file it prints the line-numbered source plus
    /// which files depend on it. NO LLM.
    Node {
        /// A symbol name, exact node ID (for example from `search --json`), or
        /// a file path/basename to read.
        target: String,
        /// Project root. Query commands require `-p/--path`; do not append the
        /// project as another positional argument.
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// Pin an overloaded SYMBOL to the definition in this file (path or
        /// basename). Its source body is returned, like the unpinned form.
        #[arg(short = 'f', long = "file")]
        file: Option<String>,
        /// Symbol mode: return just the file's symbol map instead of source.
        #[arg(long = "symbols-only")]
        symbols_only: bool,
        #[arg(short = 'j', long = "json")]
        json: bool,
        /// Exit non-zero when the symbol/file is not found (default exits 0).
        #[arg(long)]
        strict: bool,
    },
    // Upstream flags/output: upstream bin/codegraph.ts:1864-1870, 1871-1920.
    // `--global`/`--local` are convenience aliases for `--location` (task spec).
    Install {
        #[arg(short, long)]
        target: Option<String>,
        #[arg(short, long)]
        location: Option<String>,
        #[arg(long, conflicts_with_all = ["local", "location"])]
        global: bool,
        #[arg(long, conflicts_with = "location")]
        local: bool,
        #[arg(short, long)]
        yes: bool,
        /// After a successful agent install, initialize the current project.
        #[arg(short = 'i', long = "init")]
        init: bool,
        #[arg(long = "no-permissions")]
        no_permissions: bool,
        #[arg(long = "prompt-hook")]
        prompt_hook: bool,
        #[arg(long = "print-config")]
        print_config: Option<String>,
    },
    // Upstream flags/output: upstream bin/codegraph.ts:1931-1935, 1936-1956.
    Uninstall {
        #[arg(short, long)]
        target: Option<String>,
        #[arg(short, long)]
        location: Option<String>,
        #[arg(long, conflicts_with_all = ["local", "location"])]
        global: bool,
        #[arg(long, conflicts_with = "location")]
        local: bool,
        #[arg(short, long)]
        yes: bool,
    },
    /// Manage the embedded CodeGraph agent skill (install/update/uninstall/status).
    Skill {
        #[command(subcommand)]
        action: SkillAction,
    },
    /// Manage background HTTP MCP servers started with `serve --http --detach`.
    Http {
        #[command(subcommand)]
        action: HttpAction,
    },
    /// Inspect the foreground stdio MCP processes started by `serve --mcp`.
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
    /// Print the codegraph version.
    Version,
    /// Generate shell completion scripts (bash, zsh, fish, powershell, elvish).
    Completions {
        shell: Shell,
        /// Install the script to the shell's completion location instead of printing it.
        #[arg(long)]
        install: bool,
    },
    /// Update codegraph in place to the latest GitHub release.
    SelfUpdate {
        /// Check for a newer release without installing it.
        #[arg(long)]
        check: bool,
        /// Reinstall even if already on the latest version.
        #[arg(long)]
        force: bool,
        /// Update to a specific version tag (e.g. v0.2.0) instead of latest.
        #[arg(long)]
        tag: Option<String>,
    },
    /// Emit deterministic `codegraph_explore` output for a query (NO LLM). Query
    /// from `--query`/positional or stdin; project is the nearest `.codegraph/`.
    #[command(hide = true)]
    PromptHook {
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short, long)]
        query: Option<String>,
        #[arg(value_name = "QUERY")]
        query_positional: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum FilesFormat {
    Tree,
    Flat,
    Grouped,
}

#[derive(Debug, Subcommand)]
enum SkillAction {
    /// Install the embedded CodeGraph skill into the agent's skill directory.
    Install {
        #[arg(short, long)]
        target: Option<String>,
        #[arg(short, long)]
        location: Option<String>,
        #[arg(long, conflicts_with_all = ["local", "location"])]
        global: bool,
        #[arg(long, conflicts_with = "location")]
        local: bool,
        #[arg(short, long)]
        yes: bool,
    },
    /// Refresh the installed skill and marker-managed agent instructions.
    ///
    /// Use --force to overwrite local edits to SKILL.md; user text outside the
    /// managed instructions markers is always preserved.
    Update {
        #[arg(short, long)]
        target: Option<String>,
        #[arg(short, long)]
        location: Option<String>,
        #[arg(long, conflicts_with_all = ["local", "location"])]
        global: bool,
        #[arg(long, conflicts_with = "location")]
        local: bool,
        #[arg(long)]
        force: bool,
        /// Show a unified installed-versus-embedded diff before writing.
        #[arg(long)]
        diff: bool,
        /// Preview skill and managed-instructions changes without writing files.
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove the installed CodeGraph skill.
    Uninstall {
        #[arg(short, long)]
        target: Option<String>,
        #[arg(short, long)]
        location: Option<String>,
        #[arg(long, conflicts_with_all = ["local", "location"])]
        global: bool,
        #[arg(long, conflicts_with = "location")]
        local: bool,
        #[arg(short, long)]
        yes: bool,
    },
    /// Report installed-skill status per agent.
    Status {
        #[arg(short, long)]
        target: Option<String>,
        #[arg(short, long)]
        location: Option<String>,
        #[arg(long, conflicts_with_all = ["local", "location"])]
        global: bool,
        #[arg(long, conflicts_with = "location")]
        local: bool,
    },
}

#[derive(Debug, Subcommand)]
enum HttpAction {
    /// List all running background HTTP MCP servers.
    List,
    /// Show status for one HTTP MCP server (by addr) or all when omitted.
    Status { addr: Option<String> },
    /// Stop the background HTTP MCP server bound to `addr`.
    Stop { addr: String },
}

/// Deliberately a single-variant enum: by decision A of the rev55 plan `stop` is
/// absent this round (a PID-keyed entry whose pid was reused would let us
/// terminate an innocent process), and the enum shape leaves room to add it once
/// process instance identity can be proven portably.
#[derive(Debug, Subcommand)]
enum McpAction {
    /// List the running foreground stdio MCP processes (`serve --mcp`).
    List {
        /// Emit machine-readable JSON instead of the table.
        #[arg(long)]
        json: bool,
    },
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Init {
            path,
            target,
            yes,
            diagnostics,
        } => cmd_init(path, &target, yes, diagnostics),
        Command::Uninit { path, force } => cmd_uninit(path, force),
        Command::Index {
            path,
            force,
            quiet,
            verbose,
            diagnostics,
        } => cmd_index(path, force, quiet, verbose, diagnostics),
        Command::Sync {
            path,
            quiet,
            diagnostics,
        } => cmd_sync(path, quiet, diagnostics),
        Command::Status { path, json } => cmd_status(path, json),
        Command::Search {
            search,
            path,
            limit,
            kind,
            json,
            strict,
        } => cmd_search(search, path, limit, kind, json, strict),
        Command::Files {
            path,
            filter,
            language,
            pattern,
            format,
            max_depth,
            json,
        } => cmd_files(path, filter, language, pattern, format, max_depth, json),
        Command::Serve {
            path,
            mcp,
            no_watch,
            http,
            http_addr,
            detach,
        } => cmd_serve(path, mcp, no_watch, http, http_addr, detach),
        Command::Unlock { path } => cmd_unlock(path),
        Command::Callers {
            symbol,
            path,
            limit,
            json,
            strict,
            file,
        } => cmd_callers(symbol, path, limit, json, strict, file),
        Command::Callees {
            symbol,
            path,
            limit,
            json,
            strict,
            file,
        } => cmd_callees(symbol, path, limit, json, strict, file),
        Command::Impact {
            symbol,
            path,
            depth,
            json,
            strict,
            file,
        } => cmd_impact(symbol, path, depth, json, strict, file),
        Command::Affected {
            files,
            path,
            depth,
            filter,
        } => cmd_affected(files, path, depth, filter),
        Command::Check { path, json } => cmd_check(path, json),
        Command::Audit {
            path,
            orphans,
            dangling,
            impact,
            verify_plan,
            include,
            exclude,
            json,
        } => cmd_audit(AuditArgs {
            path,
            orphans,
            dangling,
            impact,
            verify_plan,
            include,
            exclude,
            json_output: json,
        }),
        Command::Export {
            path,
            out,
            no_centrality,
        } => cmd_export(path, out, no_centrality),
        Command::Explore {
            query,
            path,
            max_files,
            json,
        } => cmd_explore(query, path, max_files, json),
        Command::Node {
            target,
            path,
            file,
            symbols_only,
            json,
            strict,
        } => cmd_node(target, path, file, symbols_only, json, strict),
        Command::Install {
            target,
            location,
            global,
            local,
            yes,
            init,
            no_permissions,
            prompt_hook,
            print_config,
        } => {
            let print_only = print_config.is_some();
            installer::run_install(installer::InstallArgs {
                target,
                location: location_flag(location, global, local),
                yes,
                permissions: if no_permissions { Some(false) } else { None },
                front_load_hook: prompt_hook,
                print_config,
            })?;
            if init && !print_only {
                cmd_init(None, "none", yes, DiagnosticArgs::default())?;
            }
            Ok(())
        }
        Command::Uninstall {
            target,
            location,
            global,
            local,
            yes,
        } => installer::run_uninstall(installer::UninstallArgs {
            target,
            location: location_flag(location, global, local),
            yes,
        }),
        Command::Skill { action } => match action {
            SkillAction::Install {
                target,
                location,
                global,
                local,
                yes,
            } => installer::run_skill_install(installer::SkillArgs {
                target,
                location: location_flag(location, global, local),
                yes,
                force: false,
                show_diff: false,
                dry_run: false,
            }),
            SkillAction::Update {
                target,
                location,
                global,
                local,
                force,
                diff,
                dry_run,
            } => installer::run_skill_update(installer::SkillArgs {
                target,
                location: location_flag(location, global, local),
                yes: false,
                force,
                show_diff: diff,
                dry_run,
            }),
            SkillAction::Uninstall {
                target,
                location,
                global,
                local,
                yes,
            } => installer::run_skill_uninstall(installer::SkillArgs {
                target,
                location: location_flag(location, global, local),
                yes,
                force: false,
                show_diff: false,
                dry_run: false,
            }),
            SkillAction::Status {
                target,
                location,
                global,
                local,
            } => installer::run_skill_status(installer::SkillArgs {
                target,
                location: location_flag(location, global, local),
                yes: false,
                force: false,
                show_diff: false,
                dry_run: false,
            }),
        },
        Command::Http { action } => match action {
            HttpAction::List => cmd_http_list(),
            HttpAction::Status { addr } => cmd_http_status(addr),
            HttpAction::Stop { addr } => cmd_http_stop(&addr),
        },
        Command::Mcp { action } => match action {
            McpAction::List { json } => cmd_mcp_list(json),
        },
        Command::Version => {
            println!("codegraph {VERSION}");
            Ok(())
        }
        Command::Completions { shell, install } => {
            if install {
                install_completions(shell)
            } else {
                let mut cmd = Cli::command();
                generate(shell, &mut cmd, "codegraph", &mut io::stdout());
                Ok(())
            }
        }
        Command::SelfUpdate { check, force, tag } => cmd_self_update(check, force, tag),
        Command::PromptHook {
            path,
            query,
            query_positional,
        } => cmd_prompt_hook(path, query.or(query_positional)),
    }
}

/// Fold the `--global`/`--local` convenience flags into a `--location` string.
fn location_flag(location: Option<String>, global: bool, local: bool) -> Option<String> {
    if let Some(loc) = location {
        return Some(loc);
    }
    if global {
        return Some("global".to_string());
    }
    if local {
        return Some("local".to_string());
    }
    None
}

fn generate_completion_bytes(shell: Shell) -> Result<Vec<u8>> {
    const COMPLETION_STACK_BYTES: usize = 8 * 1024 * 1024;

    std::thread::Builder::new()
        .name("codegraph-completions".to_string())
        .stack_size(COMPLETION_STACK_BYTES)
        .spawn(move || {
            let mut cmd = Cli::command();
            let mut buf = Vec::new();
            generate(shell, &mut cmd, "codegraph", &mut buf);
            buf
        })
        .context("failed to start completion generator")?
        .join()
        .map_err(|_| anyhow!("completion generator thread panicked"))
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn home_dir() -> Result<PathBuf> {
    env_path("HOME")
        .or_else(|| env_path("USERPROFILE"))
        .ok_or_else(|| anyhow!("cannot resolve home directory (HOME/USERPROFILE unset)"))
}

fn write_completion_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating completion directory {}", parent.display()))?;
    }
    fs::write(path, bytes)
        .with_context(|| format!("writing completion script {}", path.display()))?;
    Ok(())
}

fn completion_target(shell: Shell) -> Result<PathBuf> {
    Ok(match shell {
        Shell::Bash => {
            let base = env_path("XDG_DATA_HOME")
                .unwrap_or_else(|| home_dir().unwrap_or_default().join(".local/share"));
            base.join("bash-completion/completions/codegraph")
        }
        Shell::Zsh => home_dir()?.join(".zfunc/_codegraph"),
        Shell::Fish => home_dir()?.join(".config/fish/completions/codegraph.fish"),
        Shell::PowerShell => {
            let base = env_path("LOCALAPPDATA")
                .unwrap_or_else(|| home_dir().unwrap_or_default().join(".local/share"));
            base.join("codegraph/completion.ps1")
        }
        Shell::Elvish => home_dir()?.join(".config/codegraph/completion.elv"),
        _ => bail!("unsupported shell for --install"),
    })
}

fn powershell_profile_path() -> Result<PathBuf> {
    if let Some(p) = env_path("CODEGRAPH_PS_PROFILE") {
        return Ok(p);
    }
    let user = env_path("USERPROFILE")
        .or_else(|| env_path("HOME"))
        .ok_or_else(|| {
            anyhow!(
                "cannot resolve PowerShell profile (set CODEGRAPH_PS_PROFILE, USERPROFILE, or HOME)"
            )
        })?;
    Ok(user.join("Documents/WindowsPowerShell/Microsoft.PowerShell_profile.ps1"))
}

fn append_dot_source_once(profile: &Path, script: &Path) -> Result<bool> {
    let line = format!(". \"{}\"", script.display());
    let existing = fs::read_to_string(profile).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == line) {
        return Ok(false);
    }
    if let Some(parent) = profile.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating profile directory {}", parent.display()))?;
    }
    let mut prefix = String::new();
    if !existing.is_empty() && !existing.ends_with('\n') {
        prefix.push('\n');
    }
    fs::write(profile, format!("{existing}{prefix}{line}\n"))
        .with_context(|| format!("appending dot-source line to {}", profile.display()))?;
    Ok(true)
}

fn install_completions(shell: Shell) -> Result<()> {
    let target = completion_target(shell)?;
    let bytes = generate_completion_bytes(shell)?;
    write_completion_file(&target, &bytes)?;
    println!("Installed {shell} completions to {}", target.display());

    match shell {
        // PowerShell's `using namespace` header is legal only at file start, so write a separate file and dot-source it (never append inline to $PROFILE).
        Shell::PowerShell => {
            let profile = powershell_profile_path()?;
            let added = append_dot_source_once(&profile, &target)?;
            if added {
                println!("Added dot-source line to {}", profile.display());
            } else {
                println!(
                    "Profile already sources the completion script: {}",
                    profile.display()
                );
            }
            println!("Restart your shell (or run `. $PROFILE`) to load completions.");
            println!(
                "Press Ctrl+Space to trigger menu completion (Set-PSReadLineKeyHandler -Key Tab -Function MenuComplete)."
            );
        }
        Shell::Zsh => {
            println!(
                "Add `fpath+=~/.zfunc` before `compinit` in your ~/.zshrc if it is not already there."
            );
            println!("Restart your shell to load completions.");
        }
        Shell::Elvish => {
            println!(
                "Add `eval (slurp < {})` to your ~/.config/elvish/rc.elv to load completions.",
                target.display()
            );
        }
        _ => {
            println!("Restart your shell to load completions.");
        }
    }
    Ok(())
}

/// Format the GitHub release tag to target from a release's bare semver.
///
/// `self_update`'s `Release.version` (from `get_latest_release()`) is the bare
/// semver with NO leading `v` (e.g. `0.15.0`), but this repo tags releases as
/// `v{semver}` (e.g. `v0.15.0`), and `target_version_tag` must match the tag
/// exactly. This bridges the two and is idempotent on an already-`v`-prefixed
/// input so it's safe regardless of which form the backend hands us.
fn latest_update_tag(latest_version: &str) -> String {
    let bare = latest_version.strip_prefix('v').unwrap_or(latest_version);
    format!("v{bare}")
}

/// Decide whether `self-update` should skip the download/replace flow because
/// the running binary is already current.
///
/// Returns `true` (skip, print "up to date", do NOT prompt/download) only when:
/// no explicit `--tag` was given, `--force` was not passed, and `latest` is not
/// a greater semver than `current`. An explicit tag or `--force` always proceeds
/// (returns `false`), and a genuinely newer release also proceeds.
fn should_skip_update(current: &str, latest: &str, force: bool, has_explicit_tag: bool) -> bool {
    if force || has_explicit_tag {
        return false;
    }
    !self_update::version::bump_is_greater(current, latest).unwrap_or(false)
}

/// Resolve a GitHub API token via injected env-getter: `GITHUB_TOKEN` then
/// `GH_TOKEN` (the `gh` CLI convention). Empty/whitespace values are rejected so
/// `GITHUB_TOKEN=` sends no broken auth header; `None` preserves anonymous mode.
/// Getter is injected (not `std::env` directly) to stay testable without
/// `set_var`, which is unsafe in edition 2024.
fn resolve_github_token(get: impl Fn(&str) -> Option<String>) -> Option<String> {
    for key in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Some(value) = get(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
    }
    None
}

/// Actionable hint appended on any GitHub-API failure. A 403 is almost always
/// rate-limit exhaustion; we always surface this (rather than fragile-parsing
/// the status out of the error string) so the user learns the exact remedy.
fn self_update_rate_limit_hint() -> &'static str {
    "if this is a GitHub API rate limit, authenticate with:\n  GITHUB_TOKEN=$(gh auth token) codegraph self-update"
}

fn cmd_self_update(check: bool, force: bool, tag: Option<String>) -> Result<()> {
    use self_update::cargo_crate_version;

    let configure = || {
        let mut builder = self_update::backends::github::Update::configure();
        builder
            .repo_owner("sunerpy")
            .repo_name("codegraph-rust")
            .bin_name("codegraph")
            .current_version(cargo_crate_version!())
            .show_download_progress(true)
            .no_confirm(force);
        // Auth all builders uniformly (self_update ignores GITHUB_TOKEN/GH_TOKEN).
        if let Some(token) = resolve_github_token(|k| std::env::var(k).ok()) {
            builder.auth_token(&token);
        }
        builder
    };

    // `--check`: just report whether a newer release exists, never install.
    if check {
        let updater = configure()
            .build()
            .context("configuring the self-update backend")?;
        let latest = updater.get_latest_release().with_context(|| {
            format!(
                "querying the latest GitHub release\n\nhint: {}",
                self_update_rate_limit_hint()
            )
        })?;
        let current = cargo_crate_version!();
        if self_update::version::bump_is_greater(current, &latest.version).unwrap_or(false) {
            println!("codegraph {current} -> {} available", latest.version);
            println!("run `codegraph self-update` to install it");
        } else {
            println!("codegraph {current} is up to date");
        }
        return Ok(());
    }

    // Resolve the tag to install. With an explicit `--tag` we honor it verbatim.
    //
    // Without `--tag` we must resolve the LATEST release ourselves and pin it via
    // `target_version_tag`. Otherwise `self_update`'s no-target path filters
    // releases by semver-compatibility and installs the *first compatible* one,
    // which on a 0.x line advances a single minor per run (e.g. 0.5.2 -> 0.5.3,
    // then 0.5.3 -> 0.14.0) instead of jumping straight to newest. Pinning the
    // latest tag bypasses that stepping so one run lands on the newest release.
    let target_tag = match tag {
        Some(t) => t,
        None => {
            let probe = configure()
                .build()
                .context("configuring the self-update backend")?;
            let latest = probe.get_latest_release().with_context(|| {
                format!(
                    "querying the latest GitHub release\n\nhint: {}",
                    self_update_rate_limit_hint()
                )
            })?;
            let current = cargo_crate_version!();
            if should_skip_update(current, &latest.version, force, false) {
                println!("codegraph {current} is already up to date");
                return Ok(());
            }
            latest_update_tag(&latest.version)
        }
    };

    let mut builder = configure();
    builder.target_version_tag(&target_tag);
    let updater = builder
        .build()
        .context("configuring the self-update backend")?;

    let status = updater.update().with_context(|| {
        format!(
            "performing the self-update\n\nhint: {}",
            self_update_rate_limit_hint()
        )
    })?;
    if status.updated() {
        println!("Updated codegraph to {}", status.version());
    } else {
        println!("codegraph {} is already up to date", status.version());
    }
    Ok(())
}

/// Claude Code `UserPromptSubmit` payload piped on stdin: `{"prompt", "cwd"}`
/// (upstream `bin/codegraph.ts:1199-1201`). Permissive: extra fields are
/// ignored and a non-JSON body is handled by the raw-string fallback in
/// [`cmd_prompt_hook`], so this never fails the hook.
#[derive(Debug, Default, serde::Deserialize)]
struct PromptHookPayload {
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

/// Cap on the HIGH-tier explore injection so a large-repo explore can't flood
/// the prompt (upstream `MAX = 16000`).
const PROMPT_HOOK_MAX_INJECT: usize = 16000;

/// `codegraph prompt-hook` — the Claude `UserPromptSubmit` hook entry point,
/// now a confidence-tiered gate (upstream #1126 + #1136, telemetry EXCLUDED):
///
/// - HIGH — a structural keyword (any of ~29 covered languages) OR a code-shaped
///   token verified in the index → full `codegraph_explore` injection.
/// - MEDIUM — no keyword/token, but prose words match indexed symbol-name
///   segments → a short symbol-pointer hint; the agent writes the explore query.
/// - silent — nothing verified → zero-cost no-op.
///
/// Input: an explicit `--query`/positional arg (raw-string path) wins; else
/// stdin is parsed as the `{prompt,cwd}` JSON payload, falling back to treating
/// the raw stdin as the literal query. Project resolution: payload `.cwd` →
/// `--path` → cwd. Honors the `CODEGRAPH_NO_PROMPT_HOOK`/`CODEGRAPH_PROMPT_HOOK`
/// kill-switch. Degradable by contract: every failure path exits 0 silently.
fn cmd_prompt_hook(path: Option<PathBuf>, query: Option<String>) -> Result<()> {
    if matches!(std::env::var("CODEGRAPH_NO_PROMPT_HOOK"), Ok(v) if v == "1")
        || matches!(std::env::var("CODEGRAPH_PROMPT_HOOK"), Ok(v) if v == "0")
    {
        return Ok(());
    }

    // Resolve the query and the payload-supplied cwd.
    let (query, payload_cwd) = match query {
        Some(q) if !q.trim().is_empty() => (q, None),
        _ => {
            let mut buf = String::new();
            io::stdin().read_to_string(&mut buf).ok();
            match serde_json::from_str::<PromptHookPayload>(&buf) {
                Ok(payload) => match payload.prompt {
                    Some(p) if !p.trim().is_empty() => (p, payload.cwd),
                    _ => (buf, None),
                },
                Err(_) => (buf, None),
            }
        }
    };
    let query = query.trim();
    if query.is_empty() {
        return Ok(());
    }

    // Gate BEFORE opening the index: a prompt that clears none of the three
    // tiers is a zero-cost no-op with no filesystem work.
    let keyworded = structural_gate::has_structural_keyword(query);
    let code_tokens = if keyworded {
        Vec::new()
    } else {
        structural_gate::extract_code_tokens(query)
    };
    let prose_words = if keyworded {
        Vec::new()
    } else {
        segments::extract_prose_candidates(query)
    };
    if !keyworded && code_tokens.is_empty() && prose_words.is_empty() {
        return Ok(());
    }

    // Project resolution: payload .cwd → --path → cwd.
    let start = match payload_cwd {
        Some(c) if !c.trim().is_empty() => absolute_path(PathBuf::from(c)),
        _ => absolute_path(path.unwrap_or_else(|| PathBuf::from("."))),
    };
    let project = resolve_project_path_optional(&start);
    if !is_initialized(&project) {
        return Ok(());
    }

    let engine = match codegraph_mcp::CodeGraphEngine::open(&project) {
        Ok(engine) => engine,
        Err(_) => return Ok(()),
    };

    // HIGH: structural keyword, or a code token verified as a real symbol here.
    let token_verified = !keyworded
        && code_tokens
            .iter()
            .any(|t| matches!(engine.store_nodes_by_name(t), Ok(nodes) if !nodes.is_empty()));

    if keyworded || token_verified {
        let result = engine.execute("codegraph_explore", &json!({ "query": query }));
        if result.is_error == Some(true) {
            return Ok(());
        }
        let text = result
            .content
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if text.trim().is_empty() {
            return Ok(());
        }
        let body = if text.len() > PROMPT_HOOK_MAX_INJECT {
            let mut cut = PROMPT_HOOK_MAX_INJECT;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            format!(
                "{}\n\u{2026}(truncated; call codegraph_explore for the rest)",
                &text[..cut]
            )
        } else {
            text
        };
        println!(
            "<codegraph_context note=\"Structural context from CodeGraph for this prompt \u{2014} treat returned source as already read; call codegraph_explore for more.\">\n{body}\n</codegraph_context>"
        );
        return Ok(());
    }

    // MEDIUM: prose words → indexed symbol-name segments. Name the matching
    // symbols and let the agent write the explore query — never run explore, so
    // a fuzzy match can't inject a full explore of the wrong feature.
    let related = segment_match::get_segment_matches(engine.store(), &prose_words, 6);
    if related.is_empty() {
        return Ok(());
    }
    let lines = related
        .iter()
        .map(|m| {
            format!(
                "  - {} ({} \u{2014} {}:{})",
                m.name,
                m.kind.as_str(),
                m.file_path,
                m.start_line
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let example_query = related
        .iter()
        .take(3)
        .map(|m| m.name.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    println!(
        "<codegraph_context note=\"CodeGraph found indexed symbols matching this prompt \u{2014} query the graph before searching files.\">\nThis project's CodeGraph index contains symbols matching this request:\n{lines}\nCall codegraph_explore ONCE with the relevant names in one query (e.g. \"{example_query}\") to get their source, call paths, and blast radius \u{2014} cheaper and more complete than Read/Grep.\n</codegraph_context>"
    );
    Ok(())
}

fn cmd_init(
    path: Option<PathBuf>,
    target: &str,
    _yes: bool,
    diagnostics: DiagnosticArgs,
) -> Result<()> {
    let project = absolute_path(path.unwrap_or_else(|| PathBuf::from(".")));
    if explicit_init_observes_readable_current(&project)? {
        println!("Already initialized in {}", project.display());
        println!("Use \"codegraph index\" to re-index or \"codegraph sync\" to update");
        return installer::run_install_local_targets(project, target);
    }
    guard_indexable_root(&project)?;
    // The rebuild layer creates the current root and its permanent lock under the
    // one outer exclusive lease; pre-creating it here would produce a lockless
    // namespace that acquisition then refuses.
    let result = index_project(
        &project,
        codegraph_store::RebuildKind::ExplicitInit,
        &diagnostics,
        "init",
    )?;
    println!("Initialized in {}", project.display());
    print_index_result(&result);
    installer::run_install_local_targets(project, target)
}

fn cmd_uninit(path: Option<PathBuf>, force: bool) -> Result<()> {
    let project = resolve_required_rebuild_project(path)?;
    if !force {
        bail!("refusing to delete .codegraph without --force");
    }
    let paths = index_paths(&project)?;
    // The drain runs INSIDE uninit's retained exclusive lease, after both durable
    // markers publish and before any runtime child is removed. It sends the
    // versioned, project-identity-bound shutdown control frame — which bypasses
    // data-request lease acquisition, the only way it can be answered while this
    // command holds the namespace exclusively — and waits for the daemon's
    // post-drain ACK. No pid is ever signalled: an unresponsive daemon makes uninit
    // fail closed with the namespace left recoverable `Uninitialized`.
    let identity = paths.project_identity().to_string();
    let outcome = codegraph_store::uninit_index_with_drain(
        &paths,
        std::time::Instant::now() + REBUILD_LEASE_TIMEOUT,
        || false,
        || drain_project_daemon(&project, &identity),
    )?;
    println!("Removed CodeGraph from {}", project.display());
    if outcome.legacy_index_present {
        println!("Legacy CodeGraph index remains untouched");
    }
    Ok(())
}

/// Bounded budget for the ONE exclusive acquisition of the stale-sidecar
/// recovery attempted before the strict startup read gate.
///
/// Deliberately much shorter than [`REBUILD_LEASE_TIMEOUT`]: this acquisition is
/// a best-effort repair on the latency-critical daemon-startup path, and the ONLY
/// legitimate reason it cannot be taken is that another cooperating holder (a
/// live reader or writer) owns the namespace — in which case there is nothing to
/// recover and startup must proceed to its unchanged verdict immediately instead
/// of stalling behind a long-lived MCP reader's shared lease.
const STALE_SIDECAR_RECOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// Whether this project's PREVIOUS rendezvous owner may still be running.
///
/// Read-only by construction: unlike `clear_stale_daemon_lock`, which removes a
/// stale record as a side effect, the startup gate must only OBSERVE liveness —
/// the record is the single-instance exclusion `try_acquire_daemon_lock` claims
/// moments later, so removing it here would open a double-start window.
///
/// Fail-closed: an unreadable or empty pid record is reported as LIVE. An empty
/// record is an in-flight `create_new` placeholder whose rename has not landed,
/// exactly as the daemon lock layer already treats it.
fn previous_daemon_owner_may_be_live(project_root: &Path) -> bool {
    let Ok(pid_path) = codegraph_daemon::daemon_pid_path(project_root) else {
        return true;
    };
    match std::fs::read_to_string(&pid_path) {
        Ok(raw) => match codegraph_daemon::decode_lock_info(&raw) {
            Some(info) => info.pid > 0 && codegraph_daemon::is_process_alive(info.pid),
            None => true,
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

/// Recover a provably dead previous owner's leftover `-wal`/`-shm` before the
/// strict gate runs.
///
/// A daemon killed with an open SQLite connection leaves an un-checkpointed
/// write-ahead log behind, and the sidecar-freedom clause of the `Current` read
/// contract then refuses EVERY later daemon start until `codegraph init` is
/// re-run. The remedy is recovery, not permission: fold that log back into the
/// main database under an exclusive lease and delete the checkpointed sidecars,
/// then let the UNCHANGED gate decide. Nothing is relaxed — the gate below still
/// demands sidecar-freedom, and a namespace this repair cannot fix is still
/// refused.
///
/// Both guards must hold: the rendezvous owner is provably not alive AND the
/// exclusive lease is obtainable within a short bound, so a live daemon's or a
/// live MCP reader's sidecars are never folded underneath them. A failure here is
/// swallowed to a log line: recovery is opportunistic, and the authoritative
/// verdict is always the gate's.
fn recover_dead_owner_sidecars(paths: &codegraph_core::IndexPaths, project_root: &Path) {
    if previous_daemon_owner_may_be_live(project_root) {
        return;
    }
    match Store::recover_stale_current_sidecars(
        paths,
        std::time::Instant::now() + STALE_SIDECAR_RECOVERY_TIMEOUT,
        || false,
    ) {
        Ok(true) => tracing::info!(
            project = %project_root.display(),
            "folded a dead daemon's leftover write-ahead log back into the index"
        ),
        Ok(false) => {}
        Err(error) => tracing::warn!(
            %error,
            project = %project_root.display(),
            "could not recover leftover SQLite sidecars; the following state gate decides"
        ),
    }
}

/// Daemon-startup gate (frozen plan lines 590-592, 603-604).
///
/// ONE bounded shared acquisition validates everything: `Store::open_for_read`
/// acquires the shared `IndexLease` and, under it, corroborates the FULL `Current`
/// contract — both owner-bound state slots (so `Future`, `Corrupt`, `Building`,
/// `Uninitialized`, `Outdated`, and an owner mismatch are all refused), tombstone
/// absence, database presence, sidecar-freedom, and the exact extraction stamp
/// read from the checkpointed main-file bytes. Slot phase alone would let a
/// `Current` slot with a deleted database or a stale stamp publish a rendezvous.
///
/// The returned `Store` OWNS that same lease, so retaining it across pid/socket
/// publication requires no second acquisition: there is no nested lock and no
/// window between validation and publication for another writer to reclassify.
/// A refusal therefore proves only that this daemon published no pid or socket;
/// the detached parent may already have opened the project-local log redirect.
fn authorize_daemon_startup(project_root: &Path) -> Result<codegraph_daemon::StartupAuthorization> {
    let paths = index_paths(project_root)?;
    recover_dead_owner_sidecars(&paths, project_root);
    match Store::open_for_read(
        &paths,
        std::time::Instant::now() + REBUILD_LEASE_TIMEOUT,
        || false,
    ) {
        Ok(store) => Ok(Box::new(store)),
        Err(error) => {
            // Re-read the observed markers for the operator-facing message only.
            // The refusal itself is already decided by the gate above.
            let status = Store::extraction_status(&paths);
            let tombstone = if paths.tombstone().exists() {
                "present"
            } else {
                "absent"
            };
            bail!(
                "refusing to start a daemon for {}: {error}; index state is {status} and its \
                 uninitialized tombstone is {tombstone}. This daemon did not publish a pid or \
                 socket; a detached parent may already have opened {}. Run `codegraph status \
                 \"{}\"` for state-specific recovery guidance.",
                project_root.display(),
                paths.daemon_log().display(),
                project_root.display()
            )
        }
    }
}

/// Ask this project's daemon (if any) to stop accepting, cancel its watcher lease
/// loops, drain, remove its own pid/socket, and ACK. `Err(detail)` is the
/// fail-closed signal; the pid is only ever reported, never signalled.
fn drain_project_daemon(project: &Path, project_identity: &str) -> Result<(), String> {
    match codegraph_daemon::request_daemon_shutdown(project, project_identity) {
        Ok(codegraph_daemon::ShutdownOutcome::NoDaemon) => Ok(()),
        Ok(codegraph_daemon::ShutdownOutcome::Drained { pid }) => {
            tracing::info!(pid, "daemon drained and removed its own rendezvous");
            Ok(())
        }
        Ok(codegraph_daemon::ShutdownOutcome::Unresponsive { pid, detail }) => Err(format!(
            "daemon {pid} did not acknowledge the shutdown control frame ({detail})"
        )),
        Err(error) => Err(format!("could not reach this project's daemon: {error:#}")),
    }
}

fn cmd_index(
    path: Option<PathBuf>,
    force: bool,
    quiet: bool,
    verbose: bool,
    diagnostics: DiagnosticArgs,
) -> Result<()> {
    // Index is the one ordinary CLI surface allowed to retry an authenticated
    // interrupted Building rebuild. Resolve state slots as well as a DB artifact
    // so the crash window after deletion but before final writer creation remains
    // reachable; authorization is still decided later under the exclusive lease.
    let project = resolve_required_rebuild_project(path)?;
    guard_indexable_root(&project)?;
    let paths = index_paths(&project)?;
    recover_dead_owner_sidecars(&paths, &project);
    if !quiet {
        warn_if_stdio_mcp_may_hold_index(&project);
    }
    // `--force` no longer removes the DB up front: the rebuild layer performs the
    // destructive removal itself, AFTER publishing `phase=building` under the one
    // outer exclusive lease, so an interruption can never leave a bare DB with no
    // state marker. Plain `index` takes the same full-rebuild path it always did.
    let _ = force;
    let result = index_project_inner(
        &project,
        codegraph_store::RebuildKind::Reindex,
        verbose,
        quiet,
        &diagnostics,
        "index",
    )?;
    if !quiet {
        print_index_result(&result);
    }
    if result.files_errored > 0 {
        bail!("index completed with {} file errors", result.files_errored);
    }
    Ok(())
}

fn cmd_sync(path: Option<PathBuf>, quiet: bool, diagnostics: DiagnosticArgs) -> Result<()> {
    // Sync must discover authenticated Outdated/Building state so its Store gate
    // can migrate it under one retained exclusive lease. Uninitialized remains
    // discoverable only to reach the typed under-lease rejection; it is never
    // authorized to sync or recreate residue.
    let project = resolve_required_rebuild_project(path)?;
    let paths = index_paths(&project)?;
    if let Some(reason) = owner_mismatch_only_reason(&paths) {
        bail!("{}", owner_mismatch_recovery_error(&project, &reason));
    }
    if Store::extraction_status(&paths) == codegraph_store::ExtractionStatus::Missing
        && paths.current_db().is_file()
    {
        bail!(
            "index database has no state slots; run `codegraph init {}` to replace it",
            project.display()
        );
    }
    recover_dead_owner_sidecars(&paths, &project);
    let mut diagnostic_run = DiagnosticRun::start(
        &project,
        paths.current_root(),
        "sync",
        &diagnostics,
        json!({
            "version": VERSION,
            "platform": std::env::consts::OS,
            "architecture": std::env::consts::ARCH,
            "rayonThreads": rayon::current_num_threads(),
            "command": "sync",
            "fileTotal": serde_json::Value::Null,
            "windowSize": 1,
        }),
    )?;
    if let Some(path) = diagnostic_run.path() {
        eprintln!("Debug log: {}", path.display());
    }
    diagnostic_run.phase_start("sync");
    let sync_started = std::time::Instant::now();
    let diagnostic_sink = diagnostic_run.sink();
    // True single-file incremental sync (P0, docs/optimization-analysis.md §1).
    // sync_project_once self-discovers candidate files via scan_project, so it works
    // for a cold CLI invocation with no daemon. Hash-gated skip + per-file delete/reinsert
    // + full re-resolve makes the result equivalent to `index --force`.
    if !quiet {
        eprintln!("Scanning files…");
    }
    let bar = spinner(
        quiet,
        "{spinner:.green} Syncing {pos}/{len} files ({elapsed})",
    );
    let mut bar_len_set = false;
    let outcome = codegraph_watch::sync_project_once_with_progress(&project, |done, total| {
        if !bar_len_set {
            bar.set_length(total as u64);
            bar_len_set = true;
        }
        bar.set_position(done as u64);
        if let Some(sink) = &diagnostic_sink {
            sink.emit(
                "heartbeat",
                json!({
                    "scheduled": total,
                    "active": usize::from(done < total),
                    "parsed": done,
                    "buffered": 0,
                    "persisted": done,
                    "nextExpected": done,
                }),
            );
        }
    })?;
    finish_phase(&bar, "Synced files");
    diagnostic_run.relocate_to_index_root(paths.current_root());
    if let Some(sink) = &diagnostic_sink
        && let Ok(store) = open_store(&project)
    {
        for relative in &outcome.changed_paths {
            let record = store.file_by_path(relative).ok().flatten();
            sink.emit(
                "file_complete",
                json!({
                    "file": relative,
                    "status": if record.is_some() { "reindexed" } else { "removed" },
                    "language": record.as_ref().map(|file| file.language.to_string()),
                    "sizeBytes": record.as_ref().map(|file| file.size),
                    "nodes": record.as_ref().map(|file| file.node_count),
                    "edges": serde_json::Value::Null,
                    "references": serde_json::Value::Null,
                    "errors": record.as_ref().map(|file| file.errors.len()),
                }),
            );
        }
    }
    diagnostic_run.phase_end(
        "sync",
        sync_started.elapsed(),
        json!({
            "filesChecked": outcome.files_checked,
            "filesReindexed": outcome.files_reindexed,
            "filesSkipped": outcome.files_skipped_unchanged,
            "filesRemoved": outcome.files_removed,
        }),
    );
    if !quiet {
        println!(
            "Synced: {} reindexed, {} skipped (unchanged), {} removed in {}",
            format_number(outcome.files_reindexed as i64),
            format_number(outcome.files_skipped_unchanged as i64),
            format_number(outcome.files_removed as i64),
            format_duration(outcome.duration_ms as i64)
        );
    }
    diagnostic_run.finish_success(json!({
        "durationMs": outcome.duration_ms,
        "filesChecked": outcome.files_checked,
        "filesReindexed": outcome.files_reindexed,
        "filesSkipped": outcome.files_skipped_unchanged,
        "filesRemoved": outcome.files_removed,
    }));
    Ok(())
}

fn cmd_status(path: Option<PathBuf>, json_output: bool) -> Result<()> {
    let start = absolute_path(path.unwrap_or_else(|| PathBuf::from(".")));
    let project = resolve_project_path_optional(&start);
    // Fail closed on an unsafe/aliased/overlapping configured root: status must
    // surface the stable diagnostic, NOT mask an invalid `CODEGRAPH_DIR` as a
    // default `.codegraph` layout (which would report a bogus "not
    // initialized"). A genuinely absent index still resolves fine and reports
    // uninitialized below.
    let resolved = index_paths(&project)?;
    let index_root = resolved.current_root().to_path_buf();
    let db = resolved.current_db();
    let (db_exists, db_size) = match fs::metadata(&db) {
        Ok(metadata) => (metadata.is_file(), metadata.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (false, 0),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", db.display()));
        }
    };
    let wal_size = Store::wal_size_bytes_for_path(&db)?;
    let legacy_index_paths: Vec<PathBuf> = Vec::new();
    let legacy_index_present = false;
    let daemon_running = daemon_already_running(&project);
    let daemon_pid_path = codegraph_daemon::daemon_pid_path(&project)?;
    let daemon_socket_path = codegraph_daemon::recorded_socket_path(&project)?;
    let daemon_log_path = codegraph_daemon::daemon_log_path(&project)?;
    let status_open = match Store::open_for_status(
        &resolved,
        std::time::Instant::now() + STATUS_LEASE_TIMEOUT,
        || false,
    ) {
        Ok(status_open) => status_open,
        Err(codegraph_store::StoreError::MissingStateWithDatabase { .. }) => {
            let detail = missing_state_with_database_detail(&project);
            if json_output {
                let mut status = json!({
                    "initialized": false,
                    "version": VERSION,
                    "projectPath": project,
                    "indexPath": index_root,
                    "lastIndexed": null,
                    "dbPath": db,
                    "dbExists": db_exists,
                    "dbSizeBytes": db_size,
                    "extractionStatus": "missing",
                    "extractionStatusDetail": detail,
                    "legacyIndexPresent": legacy_index_present,
                    "legacyIndexPaths": legacy_index_paths,
                    "daemonRunning": daemon_running,
                    "daemonPidPath": daemon_pid_path,
                    "daemonSocketPath": daemon_socket_path,
                    "daemonLogPath": daemon_log_path,
                });
                if wal_size > 0 {
                    status["walSizeBytes"] = json!(wal_size);
                }
                print_json(&status)?;
            } else {
                println!("\nCodeGraph Status\n");
                println!("Project: {}\n", project.display());
                println!("State:   missing (index database has no state slots)");
                println!("Index Statistics:");
                println!("  DB Size:   {:.2} MB", db_size as f64 / 1024.0 / 1024.0);
                print_wal_status(wal_size, db_size);
                println!("\n  DB Path:   {}", db.display());
                println!(
                    "  Daemon:    {}",
                    if daemon_running { "running" } else { "stopped" }
                );
                println!("\n{detail}");
            }
            return Ok(());
        }
        Err(error @ codegraph_store::StoreError::CurrentWithDatabaseSidecar { .. }) => {
            if json_output {
                let mut status = json!({
                    "initialized": false,
                    "version": VERSION,
                    "projectPath": project,
                    "indexPath": index_root,
                    "lastIndexed": null,
                    "dbPath": db,
                    "dbExists": db_exists,
                    "dbSizeBytes": db_size,
                    "extractionStatus": "current",
                    "extractionStatusDetail": error.to_string(),
                    "legacyIndexPresent": legacy_index_present,
                    "legacyIndexPaths": legacy_index_paths,
                    "daemonRunning": daemon_running,
                    "daemonPidPath": daemon_pid_path,
                    "daemonSocketPath": daemon_socket_path,
                    "daemonLogPath": daemon_log_path,
                });
                if wal_size > 0 {
                    status["walSizeBytes"] = json!(wal_size);
                }
                print_json(&status)?;
            } else {
                println!("\nCodeGraph Status\n");
                println!("Project: {}\n", project.display());
                println!("State:   current (blocked by SQLite sidecar)");
                println!("Index Statistics:");
                println!("  DB Size:   {:.2} MB", db_size as f64 / 1024.0 / 1024.0);
                print_wal_status(wal_size, db_size);
                println!("\n  DB Path:   {}", db.display());
                println!(
                    "  Daemon:    {}",
                    if daemon_running { "running" } else { "stopped" }
                );
            }
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    if status_open.rebuilding {
        if json_output {
            print_json(&json!({
                // The exclusive owner may be between lifecycle publications.
                // DB presence alone cannot corroborate a readable Current index.
                "initialized": false,
                "version": VERSION,
                "projectPath": project,
                "indexPath": index_root,
                "lastIndexed": null,
                "rebuilding": true,
                "dbPath": db,
                "dbExists": db_exists,
                "extractionStatus": null,
                "extractionStatusDetail": "rebuilding",
                "legacyIndexPresent": legacy_index_present,
                "legacyIndexPaths": legacy_index_paths,
                "daemonRunning": daemon_running,
                "daemonPidPath": daemon_pid_path,
                "daemonSocketPath": daemon_socket_path,
                "daemonLogPath": daemon_log_path,
            }))?;
        } else {
            println!("\nCodeGraph Status\n");
            println!("Project: {}", project.display());
            println!("DB Path: {}", db.display());
            println!("State:   rebuilding");
        }
        return Ok(());
    }
    let extraction_status = status_open
        .status
        .clone()
        .expect("a non-busy status probe always classifies the namespace");
    let owner_mismatch_recoverable = owner_mismatch_only_reason(&resolved).is_some();
    let lockless_missing =
        extraction_status == ExtractionStatus::Missing && is_lockless_missing_root(&resolved)?;
    let extraction_status_detail = if lockless_missing {
        lockless_missing_detail(&project, &resolved)
    } else {
        status_extraction_detail(&extraction_status, owner_mismatch_recoverable, &project)
    };
    let store = status_open.into_store();
    if store.is_none() {
        if json_output {
            let mut status = json!({
                "initialized": false,
                "version": VERSION,
                "projectPath": project,
                "indexPath": index_root,
                "lastIndexed": null,
                "dbPath": db,
                "dbExists": db_exists,
                "extractionStatus": extraction_status_name(&extraction_status),
                "extractionStatusDetail": extraction_status_detail,
                "legacyIndexPresent": legacy_index_present,
                "legacyIndexPaths": legacy_index_paths,
                "daemonRunning": daemon_running,
                "daemonPidPath": daemon_pid_path,
                "daemonSocketPath": daemon_socket_path,
                "daemonLogPath": daemon_log_path,
            });
            if matches!(&extraction_status, ExtractionStatus::Building { .. }) {
                status["recoveryCommand"] =
                    json!(format!("codegraph index --force {}", project.display()));
            } else if owner_mismatch_recoverable {
                status["recoveryCommand"] = json!(format!("codegraph init {}", project.display()));
            } else if lockless_missing {
                status["recoveryCommand"] =
                    json!(format!("codegraph init \"{}\"", project.display()));
            }
            print_json(&status)?;
        } else {
            println!("\nCodeGraph Status\n");
            println!("Project: {}", project.display());
            println!("DB Path: {}", db.display());
            println!("State:   {extraction_status}");
            if legacy_index_present {
                println!("Legacy index: present and untouched");
                for path in &legacy_index_paths {
                    println!("  {}", path.display());
                }
            }
            println!(
                "Daemon:  {}",
                if daemon_running { "running" } else { "stopped" }
            );
            match &extraction_status {
                ExtractionStatus::Building { .. } => {
                    println!("Index is not readable while the build is incomplete.");
                    println!(
                        "Recovery: run `codegraph index --force {}` to rebuild the interrupted index; `codegraph init {}` is also supported.",
                        project.display(),
                        project.display()
                    );
                }
                ExtractionStatus::Corrupt { .. } if owner_mismatch_recoverable => {
                    println!(
                        "Index belongs to a different filesystem location because the project was moved or copied."
                    );
                    println!(
                        "Recovery: run `codegraph init {}` to replace it.",
                        project.display()
                    );
                }
                ExtractionStatus::Corrupt { .. } => {
                    println!("Manual recovery is required.");
                }
                ExtractionStatus::Missing if lockless_missing => {
                    println!("{extraction_status_detail}");
                }
                ExtractionStatus::Outdated { .. } => {
                    println!("Index was built by an older extraction version.");
                    println!(
                        "Recovery: run `codegraph sync {}` to migrate it.",
                        project.display()
                    );
                }
                _ => {
                    println!("Not initialized");
                    println!("Run \"codegraph init\" to initialize");
                }
            }
        }
        return Ok(());
    }

    let store = store.expect("Current status retains its corroborated read store");
    let counts = store.counts()?;
    let nodes_by_kind = store.node_counts_by_kind()?;
    let files_by_language = store.file_counts_by_language()?;
    let last_indexed = latest_indexed_at(&store)?;
    let built_with_version = store.get_project_metadata("indexed_with_version")?;
    let built_with_extraction_version = store
        .get_project_metadata(codegraph_store::EXTRACTION_VERSION_KEY)?
        .and_then(|v| v.parse::<u64>().ok());
    let reindex_recommended = last_indexed.is_some()
        && built_with_extraction_version
            .is_none_or(|v| v < codegraph_store::CURRENT_EXTRACTION_VERSION);
    let resolution_incomplete = store.is_resolution_incomplete()?;

    if json_output {
        let mut index_obj = json!({
            "builtWithVersion": built_with_version,
            "builtWithExtractionVersion": built_with_extraction_version,
            "currentExtractionVersion": codegraph_store::CURRENT_EXTRACTION_VERSION,
            "reindexRecommended": reindex_recommended,
        });
        // #1187: surface the interrupted-index state ONLY when the marker is set,
        // so a healthy index's status JSON is byte-identical to a pre-#1187 build.
        if resolution_incomplete {
            index_obj["partial"] = json!(true);
        }
        let mut status = json!({
            "initialized": true,
            "version": VERSION,
            "projectPath": project,
            "indexPath": index_root,
            "lastIndexed": last_indexed.map(iso_like_millis),
            "fileCount": counts.file_count,
            "nodeCount": counts.node_count,
            "edgeCount": counts.edge_count,
            "dbSizeBytes": db_size,
            "backend": "rusqlite",
            "journalMode": journal_mode(&store)?,
            "nodesByKind": map_counts(nodes_by_kind.clone()),
            "languages": files_by_language.iter().filter(|(_, c)| *c > 0).map(|(l, _)| l).collect::<Vec<_>>(),
            "pendingChanges": { "added": 0, "modified": 0, "removed": 0 },
            "worktreeMismatch": null,
            "index": index_obj,
                "dbPath": db,
                "dbExists": db_exists,
                "extractionStatus": extraction_status_name(&extraction_status),
                "extractionStatusDetail": extraction_status.to_string(),
                "legacyIndexPresent": legacy_index_present,
                "legacyIndexPaths": legacy_index_paths,
                "daemonRunning": daemon_running,
            "daemonPidPath": daemon_pid_path,
            "daemonSocketPath": daemon_socket_path,
            "daemonLogPath": daemon_log_path,
        });
        if wal_size > 0 {
            status["walSizeBytes"] = json!(wal_size);
        }
        print_json(&status)?;
        return Ok(());
    }

    println!("\nCodeGraph Status\n");
    println!("Project: {}\n", project.display());
    println!("Index Statistics:");
    println!("  Files:     {}", format_number(counts.file_count));
    println!("  Nodes:     {}", format_number(counts.node_count));
    println!("  Edges:     {}", format_number(counts.edge_count));
    println!("  DB Size:   {:.2} MB", db_size as f64 / 1024.0 / 1024.0);
    print_wal_status(wal_size, db_size);
    println!("  Backend:   rusqlite - bundled SQLite");
    println!("  Journal:   {}\n", journal_mode(&store)?);
    println!("  DB Path:   {}", db.display());
    println!(
        "  Daemon:    {}\n",
        if daemon_running { "running" } else { "stopped" }
    );
    println!("Nodes by Kind:");
    for (kind, count) in nodes_by_kind {
        println!("  {kind:15} {}", format_number(count));
    }
    println!("\nFiles by Language:");
    for (language, count) in files_by_language {
        println!("  {language:15} {}", format_number(count));
    }
    if resolution_incomplete {
        println!(
            "\n⚠ Index is PARTIAL: a resolution pass was interrupted, so some call\n  edges are missing. Run `codegraph sync` to heal it.\n"
        );
    } else {
        println!("\nIndex is up to date\n");
    }
    Ok(())
}

fn print_wal_status(wal_size: u64, db_size: u64) {
    if wal_size > 0 {
        println!("  WAL Size:  {:.2} MB", wal_size as f64 / 1024.0 / 1024.0);
    }
    if wal_size > codegraph_store::wal_valve_threshold_bytes().max(db_size) {
        println!(
            "⚠ WAL is larger than both the configured limit and the database; stop live CodeGraph processes, then run `codegraph sync` to recover it safely."
        );
    }
}

fn extraction_status_name(status: &codegraph_store::ExtractionStatus) -> &'static str {
    match status {
        codegraph_store::ExtractionStatus::Current => "current",
        codegraph_store::ExtractionStatus::Building { .. } => "building",
        codegraph_store::ExtractionStatus::Uninitialized => "uninitialized",
        codegraph_store::ExtractionStatus::Missing => "missing",
        codegraph_store::ExtractionStatus::Outdated { .. } => "outdated",
        codegraph_store::ExtractionStatus::Future { .. } => "future",
        codegraph_store::ExtractionStatus::Corrupt { .. } => "corrupt",
    }
}

fn owner_mismatch_only_reason(
    paths: &codegraph_core::IndexPaths,
) -> Option<codegraph_store::CorruptReason> {
    let classification = codegraph_store::classify(paths);
    let reason = match classification.status() {
        ExtractionStatus::Corrupt {
            reason: reason @ CorruptReason::OwnerMismatch { .. },
        } => reason.clone(),
        _ => return None,
    };
    let mut present = false;
    for index in 0..2 {
        match classification.slot(index) {
            SlotOutcome::Absent => {}
            SlotOutcome::Invalid(CorruptReason::OwnerMismatch { .. }) => present = true,
            SlotOutcome::Valid(_) | SlotOutcome::FutureProtocol(_) | SlotOutcome::Invalid(_) => {
                return None;
            }
        }
    }
    present.then_some(reason)
}

fn owner_mismatch_recovery_error(project: &Path, reason: &CorruptReason) -> String {
    format!(
        "CodeGraph index state in {} is corrupt: {reason}; the index belongs to a different filesystem location because the project was moved or copied; run `codegraph init {}` to replace it",
        project.display(),
        project.display()
    )
}

fn status_extraction_detail(
    status: &ExtractionStatus,
    owner_mismatch_recoverable: bool,
    project: &Path,
) -> String {
    match status {
        ExtractionStatus::Corrupt { reason } if owner_mismatch_recoverable => format!(
            "{reason}; the index belongs to a different filesystem location because the project was moved or copied; run `codegraph init {}` to replace it",
            project.display()
        ),
        ExtractionStatus::Corrupt { reason } => {
            format!("{reason}; manual recovery is required")
        }
        _ => status.to_string(),
    }
}

fn cmd_search(
    search: String,
    path: Option<PathBuf>,
    limit: i64,
    kind: Option<String>,
    json_output: bool,
    strict: bool,
) -> Result<()> {
    let project = resolve_required_project(path)?;
    let store = open_store(&project)?;
    let paths = codegraph_core::IndexPaths::resolve(
        &project,
        std::env::var("CODEGRAPH_DIR").ok().as_deref(),
    )?;
    let config = Config::load_for_paths(None, &paths)?;
    let deprioritize = Arc::new(DeprioritizeMatcher::load_for_paths(&paths, &config));
    let kinds = kind
        .as_deref()
        .map(parse_node_kind)
        .transpose()?
        .into_iter()
        .collect();
    let mut results = search_nodes(
        &store,
        &search,
        &SearchOptions {
            kinds,
            languages: Vec::new(),
            limit: Some(limit),
            offset: Some(0),
            seed_names: Vec::new(),
            deprioritize: Some(deprioritize),
        },
        &project_name_tokens(&project),
    )?;
    if results.iter().all(|r| r.node.name != search)
        && let Some(resolved) = resolve_gdscript_class_member(&store, &search)?
    {
        results = resolved
            .into_iter()
            .map(|node| SearchResult { node, score: 1.0 })
            .collect();
    }
    let is_empty = results.is_empty();
    if json_output {
        let output = results.iter().map(SearchOutput::from).collect::<Vec<_>>();
        print_json_pretty(&output)?;
    } else if is_empty {
        println!("No results found for \"{search}\"");
    } else {
        println!("\nSearch Results for \"{search}\":\n");
        for result in results {
            println!("{}", format_search_result_line(&result));
            println!("  {}:{}", result.node.file_path, result.node.start_line);
            if let Some(signature) = &result.node.signature {
                println!("  {signature}");
            }
            println!();
        }
    }
    if strict && is_empty {
        bail!("codegraph search: no results found for \"{search}\"");
    }
    Ok(())
}

/// Human-readable one-line summary of a search hit. Results are listed
/// best-match-first, so the raw FTS `score` (not a 0..1 fraction) is NOT shown —
/// rendering `score * 100` produced nonsensical values like `12042%` (upstream
/// #1045). The score stays in the `--json` output for sorting/thresholding.
fn format_search_result_line(result: &SearchResult) -> String {
    format!("{:<12}{}", result.node.kind, result.node.name)
}

fn cmd_files(
    path: Option<PathBuf>,
    filter: Option<String>,
    language: Option<String>,
    pattern: Option<String>,
    format: FilesFormat,
    max_depth: Option<usize>,
    json_output: bool,
) -> Result<()> {
    let project = resolve_required_project(path)?;
    let store = open_store(&project)?;
    let mut files = store.all_files()?;
    if let Some(filter) = filter {
        let alt = format!("./{filter}");
        files.retain(|f| f.path.starts_with(&filter) || f.path.starts_with(&alt));
    }
    if let Some(language) = language {
        files.retain(|f| f.language.as_str() == language);
    }
    if let Some(pattern) = pattern {
        files.retain(|f| glob_matches(&pattern, &f.path));
    }
    for file in &mut files {
        file.node_count = store.node_count_by_file_path(&file.path)?;
    }
    if json_output {
        let output = files.iter().map(FileOutput::from).collect::<Vec<_>>();
        print_json_pretty(&output)?;
        return Ok(());
    }
    if files.is_empty() {
        println!("No files found matching the criteria.");
        return Ok(());
    }
    match format {
        FilesFormat::Flat => print_files_flat(&files),
        FilesFormat::Grouped => print_files_grouped(&files),
        FilesFormat::Tree => print_files_tree(&files, max_depth),
    }
    Ok(())
}

/// Whether `CODEGRAPH_DEBUG` is truthy (`"1"`/`"true"`). Retained ONLY as the
/// back-compat translation into a debug log level (see [`effective_log_level`]);
/// RUST_LOG is the primary knob. The old `[codegraph debug]` stderr traces are
/// now `tracing::debug!` events that the EnvFilter gates, so this no longer
/// gates any print directly.
fn debug_enabled() -> bool {
    matches!(
        std::env::var("CODEGRAPH_DEBUG").as_deref(),
        Ok("1") | Ok("true")
    )
}

/// Resolve the effective base log level for the reloadable level filter. This
/// value only sets the reload layer's floor; the EnvFilter (from RUST_LOG)
/// filters on top. Because the two combine with AND, the base must never sit
/// BELOW what RUST_LOG asks for — so when RUST_LOG is set we open the base to
/// `trace` and let the EnvFilter be the sole gate. When RUST_LOG is unset,
/// `CODEGRAPH_DEBUG=1` bumps the base to `debug` for back-compat with the old
/// `[codegraph debug]` traces; otherwise the config level is used unchanged.
fn effective_log_level(config_level: &str) -> String {
    if std::env::var_os("RUST_LOG").is_some() {
        return "trace".to_string();
    }
    if debug_enabled() {
        return "debug".to_string();
    }
    config_level.to_string()
}

/// The project's rendezvous identities for one debug line, or the fail-closed
/// diagnostic when the configured index root is unsafe. Debug output must never
/// reconstruct a path the resolver refused.
fn describe_rendezvous(project_root: &Path) -> String {
    match (
        codegraph_daemon::daemon_pid_path(project_root),
        codegraph_daemon::recorded_socket_path(project_root),
    ) {
        (Ok(pid_path), Ok(socket_path)) => {
            format!(
                "pid={} socket={}",
                pid_path.display(),
                socket_path.display()
            )
        }
        (Err(error), _) | (_, Err(error)) => format!("(unresolved: {error})"),
    }
}

fn emit_serve_startup_debug(
    project_root: &Path,
    explicit_path: bool,
    has_codegraph: bool,
    mode: &ServeMode,
) {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(unknown)".to_string());
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(unknown)".to_string());
    let db = db_path(project_root).ok();
    let db_display = db
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(unresolved)".to_string());
    let db_exists = db.as_ref().map(|p| p.is_file()).unwrap_or(false);
    tracing::debug!(
        %exe,
        %cwd,
        explicit_path,
        default_project = %project_root.display(),
        db = %db_display,
        db_exists,
        has_codegraph_dir = has_codegraph,
        mode = ?mode,
        "serve startup"
    );
}

fn cmd_serve(
    path: Option<PathBuf>,
    mcp: bool,
    no_watch: bool,
    http: bool,
    http_addr: String,
    detach: bool,
) -> Result<()> {
    if http && mcp {
        anyhow::bail!(
            "`--mcp` and `--http` are mutually exclusive: `--mcp` serves MCP over stdio, `--http` serves it over streamable-HTTP. Pick one."
        );
    }
    if detach && !http {
        anyhow::bail!(
            "`--detach` is only meaningful with `--http` (background HTTP MCP server). For stdio `serve --mcp`, the shared daemon already runs in the background automatically."
        );
    }
    if http {
        return cmd_serve_http(path, &http_addr, detach);
    }
    let explicit_path = path.is_some();
    let search_from = absolute_path(path.unwrap_or_else(|| PathBuf::from(".")));
    // Preserve the existing lifecycle-root fallback for interrupted/outdated
    // indexes, but prefer the shared stdio resolver when it finds a usable
    // ancestor or exactly one indexed child workspace project.
    let lifecycle_root = resolve_project_path_optional(&search_from);
    let resolution = codegraph_mcp::resolve_server_root(&search_from, true);
    let project_root = resolution.root.clone().unwrap_or(lifecycle_root);
    if resolution.via_subproject_scan {
        let relative = project_root
            .strip_prefix(&resolution.search_from)
            .unwrap_or(&project_root)
            .display();
        eprintln!(
            "[CodeGraph MCP] No index at or above {}; adopted the single indexed sub-project {relative} as the default project.",
            resolution.search_from.display()
        );
    }
    let project = Some(project_root.clone());
    if mcp {
        // Stop-the-bleed home guard: an IDE (e.g. Kiro) launches `serve --mcp`
        // with CWD=$HOME, which would otherwise spawn a daemon and run a
        // home-wide catch-up sync that pegs a CPU indexing the entire home
        // tree. When the resolved root is too broad ($HOME or filesystem root),
        // serve tools off any existing index but run NO daemon, watcher, or
        // catch-up. A real project nested under $HOME is unaffected.
        if let Some(reason) = codegraph_watch::too_broad_root_reason(&project_root) {
            tracing::info!(
                %reason,
                "no project root; tools still answer off an existing index if present"
            );
            // A FOREGROUND stdio process like `Direct`: it answers tool calls off
            // any existing index, opening the DB per request, so it must be
            // visible to `codegraph mcp list` too — this is exactly the
            // IDE-launched `CWD=$HOME` case that command exists to surface.
            let _registry = register_stdio_mcp_process(project.as_deref(), explicit_path);
            return serve_direct_no_services(project, &project_root, no_watch);
        }
        let has_codegraph = codegraph_dir(&project_root)
            .map(|d| d.is_dir())
            .unwrap_or(false);
        let mode = select_serve_mode(daemon_opt_out(), is_daemon_internal(), has_codegraph);
        emit_serve_startup_debug(&project_root, explicit_path, has_codegraph, &mode);
        match mode {
            ServeMode::Direct => {
                let _registry = register_stdio_mcp_process(project.as_deref(), explicit_path);
                return serve_direct(project, &project_root, no_watch, explicit_path);
            }
            ServeMode::BeDaemon => {
                // The daemon loads THIS project's own watch config itself, from
                // the resolved index root, so nothing is passed down here. The
                // startup gate below validates state/owner/tombstone under a
                // bounded SHARED index lease and RETAINS it across pid/socket
                // publication, so a concurrent `uninit --force` cannot interleave.
                let gate = |root: &Path| authorize_daemon_startup(root);
                return codegraph_daemon::run_foreground_gated(
                    &project_root,
                    codegraph_daemon::DaemonOptions {
                        run_mcp: true,
                        host_pid: codegraph_daemon::host_pid_from_env(),
                        ..Default::default()
                    },
                    Some(&gate),
                )
                .context("running as detached MCP daemon");
            }
            ServeMode::SpawnOrProxy => {
                let _registry = register_stdio_mcp_process(project.as_deref(), explicit_path);
                return serve_spawn_or_proxy(project, &project_root, no_watch, explicit_path);
            }
        }
    }
    eprintln!("\nCodeGraph daemon/watch server\n");
    eprintln!("Daemon and watcher startup is wired here for tasks 24/25.");
    eprintln!("Use `codegraph serve --mcp` to start the committed MCP stdio server.");
    Ok(())
}

/// Register THIS process in the global PID-keyed MCP registry and return the
/// guard that removes the entry when serving ends (stdin EOF is the ordinary
/// shutdown path). Registered from all THREE foreground stdio exits — `Direct`,
/// `SpawnOrProxy`, and the too-broad-root guard's `serve_direct_no_services` —
/// the processes a user sees in their process list, and never from `BeDaemon`,
/// which already holds the per-project `daemon.pid` lock and would otherwise be
/// counted twice.
///
/// `project` is recorded ONLY when the user actually passed `--path`
/// (`explicit_path`). A bare `serve --mcp` defaults `project` to its cwd, but cwd
/// is merely where the process started: with nothing pinned it resolves a project
/// per request, so recording cwd would claim a default it does not have. The field
/// is purely informational either way — nothing filters on it, because a client
/// can ask any server to open any indexed project — so an absent value is the
/// honest one, and `codegraph mcp list` renders it as `<none>`.
///
/// A registry failure NEVER breaks `serve --mcp`: MCP availability outranks
/// observability, so an unwritable state dir is warned about and serving
/// continues unregistered. No signal handlers are installed either — a killed
/// or crashed process leaves a stale entry on purpose, which is exactly what
/// [`codegraph_daemon::mcp_registry::prune_dead`] self-heals on the next read.
fn register_stdio_mcp_process(
    project: Option<&Path>,
    explicit_path: bool,
) -> Option<McpRegistryGuard> {
    use codegraph_daemon::mcp_registry::{self, McpServerInfo};

    let pid = std::process::id();
    let info = McpServerInfo {
        pid,
        project: explicit_path
            .then_some(project)
            .flatten()
            .map(|path| path.display().to_string()),
        transport: "stdio".to_string(),
        started_at: mcp_registry::now_millis(),
        version: VERSION.to_string(),
    };
    match mcp_registry::write_entry(&info) {
        Ok(_) => Some(McpRegistryGuard { pid }),
        Err(error) => {
            tracing::warn!(
                %error,
                "could not register this stdio MCP process; serving anyway (`codegraph mcp list` \
                 will not show it)"
            );
            None
        }
    }
}

/// Best-effort removal of this process's own MCP registry entry on scope exit.
/// A crash is covered by the next read's prune, so this is a courtesy, not a
/// correctness requirement.
struct McpRegistryGuard {
    pid: u32,
}

impl Drop for McpRegistryGuard {
    fn drop(&mut self) {
        let _ = codegraph_daemon::mcp_registry::remove_entry(self.pid);
    }
}

/// `serve --http`: serve MCP over streamable-HTTP (rmcp). Two modes selected by
/// `--path`. With `--path`: PINNED — resolve the project (find-up), REQUIRE an
/// on-disk index (hard-error otherwise — never self-index), and pin it as the
/// default. Without `--path`: GLOBAL — no pinned default, no startup index
/// requirement; each tool call MUST carry its own `projectPath` (the HTTP analog
/// of the Kiro/Qoder bare global entry).
///
/// HTTP servers are keyed by BIND ADDR in a GLOBAL registry (not `.codegraph/`),
/// so this path also does self-healing conflict detection: prune dead entries,
/// error out if a LIVE server already binds the same addr, and (when free) note
/// any other running servers. `--detach` spawns a background child (via the
/// generalized daemon detach primitive) and the parent registers it + exits;
/// foreground (default) registers itself and blocks on `serve_http`.
fn cmd_serve_http(path: Option<PathBuf>, http_addr: &str, detach: bool) -> Result<()> {
    use codegraph_daemon::http_registry::{self, HttpMode, HttpServerInfo};

    let addr = resolve_http_addr(http_addr)?;
    let addr_key = addr.to_string();

    let (project, mode) = match path {
        Some(raw) => {
            let project = resolve_project_path_optional(&absolute_path(raw));
            let db = db_path(&project)?;
            if !db.is_file() {
                anyhow::bail!(
                    "`serve --http --path` requires an indexed project, but no index was found at {}. Run `codegraph init {}` (or `codegraph index`) first.",
                    db.display(),
                    project.display(),
                );
            }
            (Some(project), HttpMode::Pinned)
        }
        None => (None, HttpMode::Global),
    };

    // The detached child re-invokes this same command with the internal marker
    // set. It IS the background server: register itself and run the foreground
    // serve path (never re-detach, never re-run conflict detection — the parent
    // already did that before spawning).
    if is_http_detach_internal() {
        let info = HttpServerInfo {
            pid: std::process::id(),
            addr: addr_key.clone(),
            mode,
            project: project.as_ref().map(|p| p.display().to_string()),
            started_at: http_registry::now_millis(),
            version: VERSION.to_string(),
            log_file: Some(http_log_path(&addr_key).display().to_string()),
        };
        let _ = http_registry::write_entry(&info);
        let _guard = HttpRegistryGuard::new(addr_key);
        return serve_http_impl(project, addr);
    }

    // Parent path: self-heal the registry, then detect conflicts.
    http_registry::prune_dead();
    if let Some(existing) = http_registry::live_entry_for(&addr_key) {
        print_http_conflict(&existing);
        anyhow::bail!(
            "an HTTP MCP server is already running on {addr_key} (pid {}, started {}); stop it with `codegraph http stop {addr_key}` or choose a different --http-addr",
            existing.pid,
            format_started_at(existing.started_at),
        );
    }
    note_other_running_servers(&addr_key);

    if detach {
        let exe = std::env::current_exe().context("resolving current executable for --detach")?;
        let log_file = http_log_path(&addr_key);
        if let Some(parent) = log_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let pid =
            codegraph_daemon::spawn_detached_http(&exe, &addr_key, project.as_deref(), &log_file)
                .context("spawning detached HTTP MCP server")?;
        let info = HttpServerInfo {
            pid,
            addr: addr_key.clone(),
            mode,
            project: project.as_ref().map(|p| p.display().to_string()),
            started_at: http_registry::now_millis(),
            version: VERSION.to_string(),
            log_file: Some(log_file.display().to_string()),
        };
        http_registry::write_entry(&info).context("writing HTTP registry entry")?;
        println!(
            "started HTTP MCP server on {addr_key} (pid {pid}), logs: {}",
            log_file.display()
        );
        return Ok(());
    }

    // Foreground (default): register self, run serve_http (blocking); the Drop
    // guard best-effort removes the entry on graceful exit.
    let info = HttpServerInfo {
        pid: std::process::id(),
        addr: addr_key.clone(),
        mode,
        project: project.as_ref().map(|p| p.display().to_string()),
        started_at: http_registry::now_millis(),
        version: VERSION.to_string(),
        log_file: None,
    };
    let _ = http_registry::write_entry(&info);
    let _guard = HttpRegistryGuard::new(addr_key);
    match &project {
        Some(project) => tracing::debug!(
            %addr,
            project = %project.display(),
            "serve --http (pinned)"
        ),
        None => tracing::debug!(
            %addr,
            "serve --http (global): per-call projectPath, no pinned default"
        ),
    }
    serve_http_impl(project, addr)
}

/// True when this process is the detached HTTP child (re-invoked by
/// [`codegraph_daemon::spawn_detached_http`] with the internal marker set).
fn is_http_detach_internal() -> bool {
    std::env::var(codegraph_daemon::CODEGRAPH_HTTP_DETACH_INTERNAL).as_deref() == Ok("1")
}

/// Log-file path for a detached HTTP server: `<registry_dir>/<addr-sanitized>.log`.
fn http_log_path(addr_key: &str) -> PathBuf {
    codegraph_daemon::http_registry::registry_dir().join(format!(
        "{}.log",
        codegraph_daemon::http_registry::sanitize_addr(addr_key)
    ))
}

/// Best-effort removal of this process's own registry entry on scope exit
/// (graceful foreground shutdown / detached child exit). A crash is covered by
/// the next-start prune, so this is a courtesy, not a correctness requirement.
struct HttpRegistryGuard {
    addr_key: String,
}

impl HttpRegistryGuard {
    fn new(addr_key: String) -> Self {
        Self { addr_key }
    }
}

impl Drop for HttpRegistryGuard {
    fn drop(&mut self) {
        let _ = codegraph_daemon::http_registry::remove_entry(&self.addr_key);
    }
}

/// Print a running instance's details for a same-addr conflict (pid, mode,
/// project, started, log) so the user sees exactly what is holding the addr.
fn print_http_conflict(info: &codegraph_daemon::http_registry::HttpServerInfo) {
    eprintln!(
        "  running: {} (pid {}, mode {}, project {}, started {}{})",
        info.addr,
        info.pid,
        info.mode.as_str(),
        info.project.as_deref().unwrap_or("<global>"),
        format_started_at(info.started_at),
        info.log_file
            .as_deref()
            .map(|l| format!(", log {l}"))
            .unwrap_or_default(),
    );
}

/// After confirming the requested addr is free, note any OTHER live servers so
/// the user knows multiple instances are running.
fn note_other_running_servers(addr_key: &str) {
    let others: Vec<_> = codegraph_daemon::http_registry::live_entries()
        .into_iter()
        .filter(|info| info.addr != addr_key)
        .collect();
    if others.is_empty() {
        return;
    }
    let list = others
        .iter()
        .map(|info| format!("{} (pid {})", info.addr, info.pid))
        .collect::<Vec<_>>()
        .join(", ");
    eprintln!(
        "Note: {} other HTTP MCP server(s) running: {list}",
        others.len()
    );
}

/// Format an epoch-ms timestamp as an RFC3339 local string, falling back to the
/// raw millis when the time crate cannot render it.
fn format_started_at(started_at_ms: u64) -> String {
    let secs = i64::try_from(started_at_ms / 1000).unwrap_or(0);
    OffsetDateTime::from_unix_timestamp(secs)
        .ok()
        .and_then(|dt| {
            dt.to_offset(time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC))
                .format(&Rfc3339)
                .ok()
        })
        .unwrap_or_else(|| format!("{started_at_ms} ms"))
}

/// Resolve a `--http-addr` string to a bind `SocketAddr`, accepting IP literals
/// (`127.0.0.1:8111`, `[::1]:8111`) AND hostnames (`localhost:8111`). Uses
/// `ToSocketAddrs` so `localhost` resolves through the OS resolver; when it
/// yields both IPv6 and IPv4 loopbacks the first IPv4 is preferred so a plain
/// `localhost` binds `127.0.0.1` (keeping curl-to-127.0.0.1 predictable),
/// falling back to the first resolved address otherwise.
fn resolve_http_addr(http_addr: &str) -> Result<std::net::SocketAddr> {
    use std::net::ToSocketAddrs;
    let mut addrs = http_addr.to_socket_addrs().with_context(|| {
        format!(
            "invalid --http-addr {http_addr:?}: expected <host>:<port> (e.g. 127.0.0.1:8111 or localhost:8111)"
        )
    })?;
    let first = addrs
        .next()
        .ok_or_else(|| anyhow!("--http-addr {http_addr:?} resolved to no socket address"))?;
    if first.is_ipv4() {
        return Ok(first);
    }
    Ok(addrs.find(std::net::SocketAddr::is_ipv4).unwrap_or(first))
}

/// Indirection to `codegraph_mcp::serve_http` (streamable-HTTP via rmcp, the
/// sole HTTP transport).
fn serve_http_impl(default_project: Option<PathBuf>, addr: std::net::SocketAddr) -> Result<()> {
    codegraph_mcp::serve_http(default_project, addr).context("serving MCP over streamable-HTTP")
}

/// `codegraph http list`: prune dead entries, then print a table of the live
/// background HTTP MCP servers (ADDR | PID | MODE | PROJECT | STARTED | LOG).
fn cmd_http_list() -> Result<()> {
    let servers = codegraph_daemon::http_registry::live_entries();
    print_http_table(&servers);
    Ok(())
}

/// `codegraph http status [<addr>]`: with an addr, print detail for that one
/// server; without, behave like `list` plus a running-count note.
fn cmd_http_status(addr: Option<String>) -> Result<()> {
    let servers = codegraph_daemon::http_registry::live_entries();
    match addr {
        Some(addr) => match servers.iter().find(|info| info.addr == addr) {
            Some(info) => {
                println!("addr:    {}", info.addr);
                println!("pid:     {}", info.pid);
                println!("mode:    {}", info.mode.as_str());
                println!("project: {}", info.project.as_deref().unwrap_or("<global>"));
                println!("started: {}", format_started_at(info.started_at));
                println!("version: {}", info.version);
                println!("log:     {}", info.log_file.as_deref().unwrap_or("-"));
            }
            None => println!("No HTTP MCP server running on {addr}."),
        },
        None => {
            print_http_table(&servers);
            if !servers.is_empty() {
                println!("({} HTTP MCP server(s) running)", servers.len());
            }
        }
    }
    Ok(())
}

/// `codegraph http stop <addr>`: find the live server on `addr`, send it a
/// graceful terminate (SIGTERM on unix / TerminateProcess on windows), wait
/// briefly, and remove its registry entry.
fn cmd_http_stop(addr: &str) -> Result<()> {
    let addr_key = resolve_http_addr(addr)
        .map(|resolved| resolved.to_string())
        .unwrap_or_else(|_| addr.to_string());
    let info = codegraph_daemon::http_registry::live_entry_for(&addr_key)
        .or_else(|| codegraph_daemon::http_registry::live_entry_for(addr));
    let Some(info) = info else {
        println!("No HTTP MCP server running on {addr}.");
        return Ok(());
    };
    let delivered = codegraph_daemon::terminate_pid(info.pid);
    for _ in 0..50 {
        if !codegraph_daemon::is_process_alive(info.pid) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    codegraph_daemon::http_registry::remove_entry(&info.addr);
    if delivered {
        println!(
            "stopped HTTP MCP server on {} (pid {})",
            info.addr, info.pid
        );
    } else {
        println!(
            "removed stale registry entry for {} (pid {} did not accept the terminate signal)",
            info.addr, info.pid
        );
    }
    Ok(())
}

/// `codegraph mcp list`: prune dead entries, then report the live FOREGROUND
/// stdio MCP processes (`serve --mcp`) — pid, project, start time, version — so
/// a user can tell WHO is holding an index and HOW to stop it.
///
/// We deliberately print stop GUIDANCE instead of offering `mcp stop` (decision
/// A of the rev55 plan): entries are PID-keyed, so a stale entry whose pid was
/// reused by an unrelated process would make us terminate an innocent process,
/// and this workspace has no portable way to prove process instance identity.
///
/// Every branch exits 0, including the unreadable-registry one: this is a
/// diagnostic command, and failing hard while the user is already debugging
/// would be hostile.
fn cmd_mcp_list(json: bool) -> Result<()> {
    match codegraph_daemon::mcp_registry::live_entries() {
        codegraph_daemon::mcp_registry::RegistryRead::Available(servers) => {
            if json {
                print_json_pretty(&json!({ "servers": servers }))
            } else {
                print_mcp_table(&servers);
                Ok(())
            }
        }
        codegraph_daemon::mcp_registry::RegistryRead::Unavailable { path, error } => {
            let path = path.display().to_string();
            if json {
                // `servers` stays an array so a consumer never has to guess the
                // shape; the extra key is what marks the outage.
                print_json_pretty(&json!({
                    "servers": [],
                    "registryUnavailable": { "path": path, "error": error },
                }))
            } else {
                println!("registry unavailable at {path}: {error}");
                println!(
                    "Cannot tell which stdio MCP servers are running. Find them with your OS \
                     process tools (look for `codegraph serve --mcp`), then stop one with {}.",
                    mcp_stop_command()
                );
                Ok(())
            }
        }
    }
}

/// Render the live stdio MCP processes, or a friendly empty state. LAUNCH PROJECT
/// goes LAST and is never truncated: unlike the HTTP table (whose selector, ADDR,
/// is both short and first), here the project path is the field a human reads to
/// recognize a stale row, so clipping it would defeat the purpose.
///
/// The column is LAUNCH PROJECT, not PROJECT, because that is all the entry knows:
/// it is the `--path` the server was started with (`<none>` for a bare launch),
/// and a client can ask any server to open a different indexed project at any
/// time. Nothing in the CLI treats it as a scope.
fn print_mcp_table(servers: &[codegraph_daemon::mcp_registry::McpServerInfo]) {
    if servers.is_empty() {
        println!("No stdio MCP servers registered.");
        println!(
            "Note: older codegraph versions do not register; find those with your OS process \
             tools (look for `codegraph serve --mcp`)."
        );
        return;
    }
    println!(
        "{:>7} {:<27} {:<12} LAUNCH PROJECT",
        "PID", "STARTED", "VERSION"
    );
    for info in servers {
        println!(
            "{:>7} {:<27} {:<12} {}",
            info.pid,
            format_started_at(info.started_at),
            info.version,
            info.project.as_deref().unwrap_or("<none>"),
        );
    }
    println!(
        "({} stdio MCP server(s) registered as running). LAUNCH PROJECT is the `--path` each \
         server started with, not a limit on what it can open: any of them can be asked to open \
         any indexed project. A pid can be reused, so confirm one is still codegraph with `{}` \
         before stopping it with `{}` — closing the client that launched it is cleaner.",
        servers.len(),
        mcp_identity_check_command(),
        mcp_stop_command()
    );
}

/// The platform-appropriate command a HUMAN runs to stop a listed process. See
/// [`cmd_mcp_list`] for why we only ever print this.
fn mcp_stop_command() -> &'static str {
    if cfg!(windows) {
        "taskkill /PID <pid> /F"
    } else {
        "kill <pid>"
    }
}

/// The platform-appropriate command that shows WHAT a pid actually is, printed
/// wherever we offer [`mcp_stop_command`].
///
/// Registry entries are PID-keyed and liveness is checked by pid alone, never by
/// process identity, so a stale entry whose pid was reused names an innocent
/// process. Decision A of the rev55 plan refuses to terminate BECAUSE identity is
/// unprovable here; presenting the pid as proven while handing over a stop
/// command would contradict that, so the user is asked to confirm it first.
fn mcp_identity_check_command() -> &'static str {
    if cfg!(windows) {
        "tasklist /FI \"PID eq <pid>\""
    } else {
        "ps -p <pid> -o command="
    }
}

/// PURE text generator for "a stdio MCP server may be holding this index
/// database" guidance. Inputs in, `String` out — no IO, no environment read, no
/// clock — which is what makes every branch unit-testable even though one of its
/// two call sites (the `RemoveDatabase` failure) cannot have its FAILURE driven
/// from a test (decision B of the rev55 plan: `RebuildFault` exposes no DB
/// removal hook, and a black-box attempt fails EARLIER, at database open).
///
/// `holders` is EVERY live registry entry, never a subset. A server's recorded
/// project is only its launch-time DEFAULT, not a capability boundary:
/// `crate::roots::resolve_project_arg` in `codegraph-mcp` probes an absolute
/// per-call `projectPath` on its own merits and consults the launch default only
/// when no path was passed, so any live server can be asked to open any indexed
/// project's database. Narrowing by that field would therefore hide real holders —
/// exactly the Windows rebuild failure this guidance exists to explain. Every row
/// names its own launch project so a reader can still tell the rows apart.
///
/// `registry_unavailable` carries `(path, error)` when the registry could not be
/// read at all. An unreadable registry deliberately renders the SAME branch as an
/// empty one: informationally both mean "we found no holder and cannot prove there
/// is none", so the actionable advice is identical. The outage only adds a line
/// naming the path, so the user can still tell the two apart.
///
/// Timestamps are NOT rendered here: [`format_started_at`] reads the local UTC
/// offset, which would make this function impure and its output TZ-dependent.
/// The text points at `codegraph mcp list` for start times instead.
fn index_holder_guidance(
    holders: &[codegraph_daemon::mcp_registry::McpServerInfo],
    registry_unavailable: Option<(&str, &str)>,
) -> String {
    let mut out = String::new();
    if holders.is_empty() {
        out.push_str(
            "No codegraph stdio MCP server is registered at all, but that does not prove none is \
             holding this index database:\n",
        );
        if let Some((path, error)) = registry_unavailable {
            out.push_str(&format!(
                "  - the stdio MCP registry at {path} could not be read ({error}), so this check \
                 cannot tell either way;\n  - even a readable registry can be empty because \
                 nothing has registered yet;\n"
            ));
        } else {
            out.push_str("  - nothing has registered yet, so the registry is empty;\n");
        }
        out.push_str(
            "  - codegraph 0.40.x and earlier never register at all, so they never appear here.\n",
        );
    } else {
        out.push_str(
            "Every registered codegraph stdio MCP server is listed below — any of them may be \
             holding this index database open. Each row names the project the server was LAUNCHED \
             for, which is only its default: a client can ask any server to open a different \
             indexed project, so its launch project does not limit which index it may hold.\n",
        );
        for info in holders {
            let launched_for = match info.project.as_deref() {
                Some(project) => format!("launched for {project}"),
                None => "launched without --path, so it has no default project".to_string(),
            };
            out.push_str(&format!(
                "  - pid {} (codegraph {}) — {launched_for}\n",
                info.pid, info.version
            ));
        }
        out.push_str(
            "That list only covers servers that register: codegraph 0.40.x and earlier never did.\n",
        );
    }
    out.push_str(&format!(
        "Look for `codegraph serve --mcp` with your OS process tools (`pgrep -af 'codegraph serve \
         --mcp'` on unix, `tasklist` or Task Manager on Windows). A registered pid is not proof of \
         identity — it may have been reused — so confirm it with `{}` before stopping the holder \
         with `{}`; closing the client that launched it is cleaner. `codegraph mcp list` shows \
         every registered server with its start time.\n",
        mcp_identity_check_command(),
        mcp_stop_command()
    ));
    out
}

/// Every live stdio MCP registry entry, plus `(path, error)` when the registry
/// could not be read at all — the ONE read behind both holder diagnostics.
///
/// There is deliberately no project-narrowed variant. An entry's `project` is the
/// path its `serve --mcp` was LAUNCHED with, not the set of databases it can open:
/// a client passing an absolute `projectPath` reaches any indexed project
/// (`codegraph-mcp/src/roots.rs`, `resolve_project_arg`). Filtering by that field
/// would drop genuine holders, which is precisely how a Windows rebuild came to
/// fail with no holder named.
fn mcp_live_entries() -> (
    Vec<codegraph_daemon::mcp_registry::McpServerInfo>,
    Option<(String, String)>,
) {
    match codegraph_daemon::mcp_registry::live_entries() {
        codegraph_daemon::mcp_registry::RegistryRead::Available(entries) => (entries, None),
        codegraph_daemon::mcp_registry::RegistryRead::Unavailable { path, error } => {
            (Vec::new(), Some((path.display().to_string(), error)))
        }
    }
}

/// Warn BEFORE the destructive rebuild when a registered stdio MCP server may
/// hold this project's index database. Advisory only: it never fails, never
/// changes the exit code, and prints nothing when there is no live server to name,
/// so a successful `index` against an empty registry emits its previous bytes
/// exactly.
///
/// Reports EVERY live server rather than the ones whose recorded project contains
/// `project`. That field is a launch-time default, not a boundary — see
/// [`mcp_live_entries`] — so narrowing by it silently drops the holder in the very
/// scenario this warning exists for: a server launched in one project that a
/// client has since asked to open this one. Over-warning costs a few stderr lines
/// before a destructive rebuild, and only when a server is actually registered;
/// under-warning costs the user the Windows failure with nobody named.
///
/// An unreadable registry stays SILENT here (a debug event only). It reports
/// nothing about a holder, and a line on every single `index` run would be noise
/// the user cannot act on; the same outage IS surfaced on the failure path, where
/// it finally matters.
fn warn_if_stdio_mcp_may_hold_index(project: &Path) {
    let (holders, unavailable) = mcp_live_entries();
    if holders.is_empty() {
        if let Some((path, error)) = unavailable {
            tracing::debug!(
                %path,
                %error,
                "could not read the stdio MCP registry before rebuilding this index"
            );
        }
        return;
    }
    eprintln!(
        "Warning: rebuilding the index for {} deletes its database files, and a process still \
         holding them makes that delete fail (the Windows failure mode).",
        project.display()
    );
    eprint!("{}", index_holder_guidance(&holders, None));
}

/// Append holder guidance to the CLI's presentation of a `RemoveDatabase`
/// failure — the exact Windows failure point, where `std::fs::remove_file` on an
/// open `codegraph.db` cannot succeed. The store-layer message
/// (`cannot remove database artifact {path}: {source}`) is a statement of fact
/// and is left untouched; this only adds "who may be holding it, and how to stop
/// it" AFTER it, and the original error chain is neither wrapped nor swallowed.
///
/// Reports EVERY live registered server, exactly like the PRE-WARNING path — a
/// server's recorded project is only its launch default, so neither path may
/// filter on it (see [`mcp_live_entries`]). Two independent reasons converge here:
/// all this error hands us is a database path rather than the server instance
/// that owns an open handle. An over-inclusive list is strictly better than
/// hiding the actual holder, and each row names its own launch project so a
/// reader can still tell which is which.
///
/// Deliberately NOT integration-tested (decision B of the rev55 plan): the
/// private `RebuildFault` hook cannot inject a DB removal failure, and a
/// black-box attempt fails EARLIER, at database open. The text is covered by
/// [`index_holder_guidance`]'s unit tests; this wiring is confirmed by a
/// hands-on walk-through.
fn index_removal_holder_guidance(err: &anyhow::Error) -> Option<String> {
    err.chain().find_map(
        |cause| match cause.downcast_ref::<codegraph_store::RebuildError>() {
            Some(codegraph_store::RebuildError::RemoveDatabase { .. }) => Some(()),
            _ => None,
        },
    )?;
    let (holders, unavailable) = mcp_live_entries();
    let unavailable = unavailable
        .as_ref()
        .map(|(path, error)| (path.as_str(), error.as_str()));
    Some(index_holder_guidance(&holders, unavailable))
}

#[cfg(test)]
mod index_holder_guidance_tests {
    use super::{
        index_holder_guidance, index_removal_holder_guidance, mcp_identity_check_command,
        mcp_stop_command,
    };
    use codegraph_daemon::mcp_registry::McpServerInfo;
    use std::path::PathBuf;

    fn entry(pid: u32, project: Option<&str>) -> McpServerInfo {
        McpServerInfo {
            pid,
            project: project.map(str::to_string),
            transport: "stdio".to_string(),
            started_at: 1_700_000_000_123,
            version: "0.41.0".to_string(),
        }
    }

    /// Branch A — every holder's PID is named, alongside the platform stop
    /// command a HUMAN runs (decision A: we print it, we never run it).
    #[test]
    fn with_holders_names_every_pid_and_the_stop_command() {
        let text =
            index_holder_guidance(&[entry(4321, Some("/work/alpha")), entry(9876, None)], None);

        assert!(
            text.contains("4321") && text.contains("9876"),
            "every holder pid must be named: {text:?}"
        );
        assert!(
            text.contains(mcp_stop_command()),
            "the platform stop command must be offered as guidance: {text:?}"
        );
        assert!(
            text.contains("/work/alpha"),
            "a pinned holder must name the project it serves: {text:?}"
        );
        assert!(
            text.contains("--path"),
            "a holder with no project must be explained (started without --path): {text:?}"
        );
        assert!(
            text.contains("does not limit which index"),
            "a row's project is the one its server was LAUNCHED for; presenting it as a limit on \
             what that server can open is the defect this text must not repeat: {text:?}"
        );
        assert!(
            text.contains("0.40.x"),
            "the registered set is not exhaustive; legacy builds never register: {text:?}"
        );
        assert!(
            !text.contains("does not prove"),
            "the found-a-holder branch must not read like the nothing-found one: {text:?}"
        );
        assert!(
            text.contains(mcp_identity_check_command()) && text.contains("reused"),
            "a registered pid is not proof of identity, so the user must be told to confirm \
             it before stopping it: {text:?}"
        );
        assert!(
            text.ends_with('\n'),
            "guidance is printed with `eprint!`, so it must end with a newline: {text:?}"
        );
    }

    /// Branch B, readable-but-empty — must name BOTH causes at once: nothing has
    /// registered yet, AND `<=0.40.x` never registers, so absence is not proof.
    #[test]
    fn without_holders_covers_empty_and_legacy_causes() {
        let text = index_holder_guidance(&[], None);

        assert!(
            text.contains("does not prove"),
            "absence of a registered holder must not be presented as proof: {text:?}"
        );
        assert!(
            text.contains("registered yet"),
            "cause 1: the registry may simply be empty: {text:?}"
        );
        assert!(
            text.contains("0.40.x"),
            "cause 2: legacy builds never register at all: {text:?}"
        );
        assert!(
            text.contains("codegraph serve --mcp") && text.contains("OS process tools"),
            "the user must be told what to look for, and with what: {text:?}"
        );
        assert!(
            text.contains(mcp_stop_command()),
            "the platform stop command must be offered here too: {text:?}"
        );
        assert!(
            !text.contains("could not be read"),
            "a readable-but-empty registry must not be reported as an outage: {text:?}"
        );
        assert!(text.ends_with('\n'), "must end with a newline: {text:?}");
    }

    /// Branch B, unreadable — same actionable advice, plus the registry path and
    /// error so the outage is distinguishable from a genuinely empty registry.
    /// All THREE causes are spelled out here (unreadable / possibly-empty /
    /// legacy) so no reader concludes the check was authoritative.
    #[test]
    fn unreadable_registry_renders_the_nothing_found_branch_with_its_path() {
        let text = index_holder_guidance(
            &[],
            Some(("/state/codegraph/mcp", "Not a directory (os error 20)")),
        );

        assert!(
            text.contains("/state/codegraph/mcp") && text.contains("Not a directory (os error 20)"),
            "the outage must name the registry path and the error: {text:?}"
        );
        assert!(
            text.contains("could not be read"),
            "the outage must be stated as an outage: {text:?}"
        );
        assert!(
            text.contains("registered yet"),
            "a readable registry could ALSO have been empty; say so: {text:?}"
        );
        assert!(
            text.contains("0.40.x"),
            "legacy builds never register at all, outage or not: {text:?}"
        );
        assert!(
            text.contains("codegraph serve --mcp") && text.contains(mcp_stop_command()),
            "the advice must stay actionable during an outage: {text:?}"
        );
        assert!(text.ends_with('\n'), "must end with a newline: {text:?}");
    }

    /// The FAILURE path must not narrow by the failing artifact's path because a
    /// server's launch project is informational and does not limit which project
    /// a later MCP request can open. An over-inclusive diagnostic beats hiding the
    /// actual holder.
    #[test]
    fn removal_guidance_reports_a_holder_pinned_outside_the_failing_artifact_path() {
        let dir =
            std::env::temp_dir().join(format!("cg-mcp-removal-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut env = crate::test_env::env_guard();
        env.set(
            codegraph_daemon::mcp_registry::CODEGRAPH_MCP_REGISTRY_DIR,
            &dir,
        );

        let holder = entry(std::process::id(), Some("/unrelated/elsewhere"));
        codegraph_daemon::mcp_registry::write_entry(&holder).unwrap();
        let err = anyhow::Error::new(codegraph_store::RebuildError::RemoveDatabase {
            path: PathBuf::from("/work/project/.codegraph/codegraph.db"),
            source: std::io::Error::other("the process cannot access the file"),
        });

        let text = index_removal_holder_guidance(&err);

        drop(env);
        let _ = std::fs::remove_dir_all(&dir);

        let text = text.expect("a RemoveDatabase failure must carry holder guidance");
        assert!(
            text.contains(&holder.pid.to_string()) && text.contains("/unrelated/elsewhere"),
            "a live registered server must be reported even though the failing artifact does not \
             live under its project: {text:?}"
        );
    }
}

/// Render the running-servers table, or a "none" line when empty.
fn print_http_table(servers: &[codegraph_daemon::http_registry::HttpServerInfo]) {
    if servers.is_empty() {
        println!("No HTTP MCP servers running.");
        return;
    }
    println!(
        "{:<22} {:>7} {:<7} {:<28} {:<25} LOG",
        "ADDR", "PID", "MODE", "PROJECT", "STARTED"
    );
    for info in servers {
        println!(
            "{:<22} {:>7} {:<7} {:<28} {:<25} {}",
            info.addr,
            info.pid,
            info.mode.as_str(),
            truncate_field(info.project.as_deref().unwrap_or("<global>"), 28),
            format_started_at(info.started_at),
            info.log_file.as_deref().unwrap_or("-"),
        );
    }
}

/// Truncate a display field to `max` chars, appending `…` when clipped, so the
/// table columns stay aligned for long project paths.
fn truncate_field(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = value.chars().take(keep).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod resolve_http_addr_tests {
    use super::resolve_http_addr;
    #[test]
    fn localhost_with_port_resolves_to_loopback() {
        let addr = resolve_http_addr("localhost:12025").expect("localhost:PORT must resolve");
        assert_eq!(addr.port(), 12025);
        assert!(
            addr.ip().is_loopback(),
            "localhost must resolve to a loopback address, got {addr}"
        );
    }

    #[test]
    fn ipv4_literal_resolves() {
        let addr = resolve_http_addr("127.0.0.1:8111").expect("127.0.0.1:PORT must resolve");
        assert_eq!(addr.port(), 8111);
        assert!(addr.ip().is_ipv4());
        assert!(addr.ip().is_loopback());
    }

    #[test]
    fn ipv6_bracketed_literal_resolves() {
        let addr = resolve_http_addr("[::1]:8111").expect("[::1]:PORT must resolve");
        assert_eq!(addr.port(), 8111);
        assert!(addr.ip().is_ipv6());
        assert!(addr.ip().is_loopback());
    }

    #[test]
    fn ipv6_unbracketed_literal_resolves() {
        // `::1:8111` is accepted by std's ToSocketAddrs (parsed as [::1]:8111).
        let addr = resolve_http_addr("::1:8111").expect("::1:8111 must resolve");
        assert_eq!(addr.port(), 8111);
        assert!(addr.ip().is_ipv6());
    }

    #[test]
    fn bogus_host_errors_with_actionable_message() {
        let err = resolve_http_addr("not a host").expect_err("bogus host must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("--http-addr") && msg.contains("localhost"),
            "error must be actionable (mention --http-addr + localhost form): {msg}"
        );
    }

    #[test]
    fn missing_port_errors() {
        resolve_http_addr("localhost").expect_err("host with no port must error");
    }
}

#[cfg(test)]
mod normalize_lexical_tests {
    use super::{absolute_path, normalize_lexical};
    use std::path::{Path, PathBuf};

    #[test]
    fn cwd_dot_has_no_trailing_curdir_segment() {
        let normalized = absolute_path(".");
        assert!(
            normalized.is_absolute(),
            "absolute_path(.) must be absolute: {}",
            normalized.display()
        );
        assert!(
            !normalized.to_string_lossy().ends_with("/."),
            "absolute_path(.) must not carry a trailing /. segment: {}",
            normalized.display()
        );
        assert_eq!(
            normalized,
            std::env::current_dir().unwrap(),
            "absolute_path(.) must equal the cwd verbatim"
        );
    }

    #[test]
    fn already_clean_absolute_path_is_unchanged() {
        let clean = PathBuf::from("/tmp/codegraph-project");
        assert_eq!(normalize_lexical(&clean), clean);
    }

    #[test]
    fn strips_curdir_and_folds_parentdir() {
        assert_eq!(
            normalize_lexical(Path::new("/a/./b/../c")),
            PathBuf::from("/a/c")
        );
        assert_eq!(
            normalize_lexical(Path::new("/a/b/.")),
            PathBuf::from("/a/b")
        );
    }
}

/// Whether `serve --mcp` should start background services (live watcher +
/// catch-up sync) for `project_root`. They run when the path was EXPLICIT
/// (`--path X` — the user opted into X) or the cwd is ALREADY indexed. A bare
/// serve from an UNINDEXED cwd (the Zed case) returns false so catch-up never
/// self-indexes the cwd — keeping it unindexed and therefore adoptable when the
/// client reports its real workspace root via `roots/list`.
fn should_run_serve_services(explicit_path: bool, project_root: &Path) -> bool {
    explicit_path
        || codegraph_dir(project_root)
            .map(|d| d.is_dir())
            .unwrap_or(false)
}

fn serve_direct(
    project: Option<PathBuf>,
    project_root: &Path,
    no_watch: bool,
    explicit_path: bool,
) -> Result<()> {
    let run_services = should_run_serve_services(explicit_path, project_root);
    // Watcher startup stays here (pre-handshake). Layer A
    // (`watch_disabled_reason`) already refuses to walk HOME / the filesystem
    // root, so a home-rooted launch never exhausts inotify. Restarting the
    // watcher against a project root adopted later from the `initialize` roots
    // (Layer B) would require McpServer to own the watcher lifecycle across
    // crates; it is deferred — the adopted root still serves tools and is
    // reconciled by the background catch-up sync, just without a live watch.
    // Skipped entirely for a bare serve from an unindexed cwd so the cwd is
    // never self-indexed (keeps it adoptable via roots/list).
    let _watcher = run_services.then(|| start_direct_watcher(project_root, no_watch));
    // Background catch-up of edits made while the server was down (#905). It runs
    // on a detached worker thread; `server.run` proceeds immediately so the FIRST
    // tools/call NEVER waits on the reconcile. Bind the flag to keep it alive (a
    // future status surface can read it); it is intentionally never awaited.
    // Skipped for a too-broad root ($HOME / filesystem root) — `sync_project_once`
    // there walks the entire home tree and pegs a CPU at 99% — and for a bare
    // serve from an unindexed cwd, where `Store::open` would otherwise create
    // `.codegraph/` and race roots adoption (the real project root Zed reports
    // would then be rejected as "already indexed cwd").
    let _catch_up_done = (run_services && should_run_daemon_services(project_root))
        .then(|| spawn_catch_up(project_root));
    serve_direct_stdio(project)
}

/// Serve the direct (pinned) stdio path through the rmcp [`CodeGraphHandler`]
/// (the sole MCP transport). Blocks until stdin EOF. The broad-root/unindexed-cwd
/// adoption handoff keeps the hand-rolled path (`serve_direct_no_services` →
/// [`McpServer::run_until_adoption`]), since rmcp owns its read loop and cannot
/// hand the reader back for the daemon proxy.
fn serve_direct_stdio(project: Option<PathBuf>) -> Result<()> {
    codegraph_mcp::serve_stdio_rmcp(project).context("running rmcp MCP stdio server")
}

/// Serves MCP tools off any existing index WITHOUT starting the watcher,
/// daemon, or catch-up sync. Used when the resolved root is too broad
/// ($HOME / filesystem root), where background services would index the whole
/// home tree.
fn serve_direct_no_services(
    project: Option<PathBuf>,
    _project_root: &Path,
    no_watch: bool,
) -> Result<()> {
    // Owned `Stdin`/`Stdout` (both `Send + 'static`) so the reader handed back
    // on adoption can move into the rmcp session's tokio runtime, which the
    // borrowed `.lock()` guards (`!Send`) cannot.
    let reader = BufReader::new(io::stdin());
    let stdout = io::stdout();
    let mut server = McpServer::new(project);
    match server
        .run_until_adoption(reader, &stdout)
        .context("running MCP stdio server until workspace adoption")?
    {
        RunUntilAdoption::Eof => Ok(()),
        RunUntilAdoption::Adopted {
            project_root,
            reader,
        } => serve_adopted_project(reader, stdout, project_root, no_watch),
    }
}

fn serve_adopted_project<R, W>(
    reader: R,
    writer: W,
    project_root: PathBuf,
    no_watch: bool,
) -> Result<()>
where
    R: BufRead + Send + 'static + Unpin,
    W: Write + Send + 'static + Unpin,
{
    let Some(socket_path) = start_daemon_for_adopted_root(&project_root, no_watch) else {
        return codegraph_mcp::rmcp_session::serve_session_rmcp(reader, writer, project_root)
            .context("running rmcp MCP stdio server for adopted project");
    };

    match codegraph_daemon::attach_to_daemon(&socket_path) {
        Ok(client) if codegraph_daemon::verify_daemon_hello(&client.hello).is_none() => {}
        Ok(_) => {
            tracing::debug!("serve_adopted: daemon version mismatch; serving direct");
            return codegraph_mcp::rmcp_session::serve_session_rmcp(reader, writer, project_root)
                .context("running rmcp MCP stdio server for adopted project");
        }
        Err(err) => {
            tracing::debug!(error = %err, "serve_adopted: daemon preflight failed; serving direct");
            heal_stale_daemon_if_dead(&project_root);
            return codegraph_mcp::rmcp_session::serve_session_rmcp(reader, writer, project_root)
                .context("running rmcp MCP stdio server for adopted project");
        }
    }

    match codegraph_daemon::run_proxy(
        &socket_path,
        Some(codegraph_daemon::current_ppid()),
        reader,
        writer,
    ) {
        Ok(codegraph_daemon::ProxyOutcome::Proxied) => Ok(()),
        Ok(codegraph_daemon::ProxyOutcome::VersionMismatch) => Ok(()),
        Err(err) => {
            tracing::debug!(error = %err, "serve_adopted: proxy attach failed");
            heal_stale_daemon_if_dead(&project_root);
            Ok(())
        }
    }
}

fn start_daemon_for_adopted_root(project_root: &Path, no_watch: bool) -> Option<PathBuf> {
    if daemon_opt_out() || is_daemon_internal() || !should_run_daemon_services(project_root) {
        return None;
    }
    if !codegraph_dir(project_root)
        .map(|d| d.is_dir())
        .unwrap_or(false)
    {
        return None;
    }
    if daemon_already_running(project_root) {
        let socket_path = codegraph_daemon::recorded_socket_path(project_root).ok()?;
        tracing::debug!(
            socket_path = %socket_path.display(),
            "adopted-root: attaching to existing daemon"
        );
        return Some(socket_path);
    }
    let Ok(exe) = std::env::current_exe() else {
        return None;
    };
    match codegraph_daemon::spawn_detached_daemon(&exe, project_root, no_watch) {
        Ok(()) => {
            poll_for_daemon_socket(project_root);
            tracing::info!(
                project = %project_root.display(),
                "started shared daemon for adopted project root"
            );
            let socket_path = codegraph_daemon::recorded_socket_path(project_root).ok()?;
            tracing::debug!(
                socket_path = %socket_path.display(),
                "adopted-root: spawned new daemon"
            );
            socket_path.exists().then_some(socket_path)
        }
        Err(err) => {
            tracing::warn!(error = %err, "adopted project daemon start failed");
            None
        }
    }
}

/// Whether daemon-style background services (detached daemon, file watcher,
/// catch-up sync) may run against `root`. Returns `false` for a too-broad root
/// ($HOME or the filesystem root); shares the decision with the watcher guard
/// via `codegraph_watch::too_broad_root_reason`.
fn should_run_daemon_services(root: &Path) -> bool {
    codegraph_watch::too_broad_root_reason(root).is_none()
}

fn guard_indexable_root(root: &Path) -> Result<()> {
    if let Some(reason) = codegraph_watch::too_broad_root_reason(root) {
        bail!(
            "refusing to index {}: {reason}. Run `codegraph init`/`index` inside a specific project directory instead.",
            root.display()
        );
    }
    Ok(())
}

/// Spawn a ONE-SHOT background catch-up sync that absorbs edits made while the
/// server was down (upstream colby `catchUpSync`, #905). Returns an
/// `Arc<AtomicBool>` flipped to `true` when the background sync finishes, so a
/// status surface could observe completion. The request path MUST NOT block on
/// it: this runs on a detached `std::thread` and is never joined on the
/// handshake / tool-call path.
fn spawn_catch_up(project_root: &Path) -> Arc<AtomicBool> {
    let done = Arc::new(AtomicBool::new(false));
    let thread_done = Arc::clone(&done);
    let root = project_root.to_path_buf();
    std::thread::spawn(move || {
        match codegraph_watch::sync_project_once(&root) {
            Ok(outcome) => {
                let changed = outcome.files_reindexed + outcome.files_removed;
                if changed > 0 {
                    tracing::info!(
                        changed,
                        "caught up {changed} file(s) changed since last run"
                    );
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "catch-up sync failed");
            }
        }
        thread_done.store(true, Ordering::SeqCst);
    });
    done
}

fn start_direct_watcher(
    project_root: &Path,
    no_watch: bool,
) -> Option<codegraph_watch::ProjectWatcher> {
    // Include/exclude, debounce, the enable flag, and extension overrides all come
    // from THIS project's own config (its resolved index root), so a direct serve
    // watches exactly the scope its own `index`/`sync` would.
    let mut opts = match codegraph_watch::watch_options_for_project(project_root) {
        Ok(opts) => opts,
        Err(err) => {
            tracing::warn!(error = %err, "could not load project watch config; watcher disabled");
            return None;
        }
    };
    // An explicit `--no-watch` still wins over the project's `watch.enabled`.
    opts.no_watch = opts.no_watch || no_watch;
    opts.on_sync_complete = Some(std::sync::Arc::new(
        |outcome: codegraph_watch::SyncOutcome| {
            tracing::info!(
                files_reindexed = outcome.files_reindexed,
                duration_ms = outcome.duration_ms,
                "auto-synced {} file(s) in {}ms",
                outcome.files_reindexed,
                outcome.duration_ms
            );
        },
    ));
    opts.on_degraded = Some(std::sync::Arc::new(|reason: String| {
        tracing::warn!(%reason, "file watcher degraded");
    }));
    opts.on_sync_error = Some(std::sync::Arc::new(|reason: String| {
        tracing::warn!(%reason, "file watcher warning");
    }));
    match codegraph_watch::start_serve_watcher(project_root, opts) {
        Ok(Some(watcher)) => {
            tracing::info!("file watcher active — graph will auto-sync on changes");
            Some(watcher)
        }
        Ok(None) => {
            let reason = codegraph_watch::watch_disabled_reason(project_root, no_watch)
                .unwrap_or_else(|| "watching disabled".to_string());
            tracing::info!(%reason, "file watcher disabled");
            None
        }
        Err(err) => {
            tracing::warn!(error = %err, "file watcher failed to start");
            None
        }
    }
}

const DAEMON_SOCKET_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);
const DAEMON_SOCKET_POLL_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(400);

/// What a `SpawnOrProxy` serve should do given whether a shared daemon is
/// already live for this project. Split out as a pure decision so the cold vs
/// warm handshake behavior is unit-testable without touching sockets/processes.
#[derive(Debug, PartialEq, Eq)]
pub enum ColdStartAction {
    /// A daemon is ALREADY running: attach to it via the real proxy (the proxy
    /// answers `initialize`/`tools/list` locally and forwards tool calls). Fast.
    ProxyToRunningDaemon,
    /// COLD start (no live daemon): spawn the shared daemon FIRE-AND-FORGET for
    /// the NEXT session's warm attach, but serve THIS session DIRECT immediately
    /// so the MCP handshake is answered without waiting on daemon readiness.
    SpawnDaemonAndServeDirect,
}

/// Decide the `SpawnOrProxy` handshake strategy. WARM (daemon live) proxies;
/// COLD spawns the shared daemon fire-and-forget and serves this session direct.
pub fn cold_start_action(daemon_running: bool) -> ColdStartAction {
    if daemon_running {
        ColdStartAction::ProxyToRunningDaemon
    } else {
        ColdStartAction::SpawnDaemonAndServeDirect
    }
}

/// `SpawnOrProxy` serve entry point. On a WARM start (a shared daemon is already
/// live) it attaches via [`run_proxy`], falling back to direct serving only if
/// the attach fails. On a COLD start (no live daemon) it spawns the shared
/// daemon FIRE-AND-FORGET — so the NEXT session attaches warm — and immediately
/// serves THIS session DIRECT, answering the MCP `initialize`/`tools/list`
/// handshake without blocking on daemon socket readiness. This is the fix for
/// the cold-start handshake race (opencode marking codegraph `failed` when the
/// spawn→poll→proxy→heal prelude exceeded its MCP init timeout under load).
fn serve_spawn_or_proxy(
    project: Option<PathBuf>,
    project_root: &Path,
    no_watch: bool,
    explicit_path: bool,
) -> Result<()> {
    tracing::debug!(
        rendezvous = %describe_rendezvous(project_root),
        "serve_spawn_or_proxy: begin"
    );
    match cold_start_action(daemon_already_running(project_root)) {
        ColdStartAction::ProxyToRunningDaemon => {
            tracing::debug!("serve_spawn_or_proxy: attaching to existing daemon (warm)");
            if let Some(result) = proxy_to_running_daemon(project_root) {
                return result;
            }
            serve_direct(project, project_root, no_watch, explicit_path)
        }
        ColdStartAction::SpawnDaemonAndServeDirect => {
            // Cold: kick off the shared daemon for FUTURE sessions, then serve
            // this session direct immediately. The spawn is best-effort and
            // idempotent — `start_or_attach` (via the daemon's pid lock) makes N
            // concurrent cold sessions converge on at most one daemon, and a
            // failed/lost race just means this session serves direct (which it
            // does anyway). We deliberately do NOT poll or proxy here: that
            // prelude is exactly what blew past opencode's handshake timeout.
            spawn_shared_daemon_best_effort(project_root, no_watch);
            serve_direct(project, project_root, no_watch, explicit_path)
        }
    }
}

/// Attach to an ALREADY-running shared daemon via the real proxy. Returns
/// `Some(Ok(()))` when the proxy bridged the session (caller must NOT also serve
/// direct), or `None` when it could not attach (socket gone, version mismatch,
/// or connect error) — the caller then falls back to direct serving. Unchanged
/// proxy semantics: `run_proxy` answers `initialize`/`tools/list` locally and
/// forwards tool calls; its fd half-close / ppid-watchdog teardown is untouched.
fn proxy_to_running_daemon(project_root: &Path) -> Option<Result<()>> {
    let socket_path = codegraph_daemon::recorded_socket_path(project_root).ok()?;
    if !socket_path.exists() {
        tracing::debug!("proxy_to_running_daemon: daemon socket missing; falling back to direct");
        heal_stale_daemon_if_dead(project_root);
        return None;
    }

    let host_ppid = Some(codegraph_daemon::current_ppid());
    let stdin = io::stdin();
    match codegraph_daemon::run_proxy(
        &socket_path,
        host_ppid,
        BufReader::new(stdin.lock()),
        io::stdout(),
    ) {
        Ok(codegraph_daemon::ProxyOutcome::Proxied) => Some(Ok(())),
        Ok(codegraph_daemon::ProxyOutcome::VersionMismatch) => {
            tracing::debug!(
                "proxy_to_running_daemon: daemon version mismatch; falling back to direct"
            );
            None
        }
        Err(err) => {
            tracing::debug!(error = %err, "proxy_to_running_daemon: proxy attach failed; falling back to direct");
            heal_stale_daemon_if_dead(project_root);
            None
        }
    }
}

/// Fire-and-forget spawn of the shared daemon on a cold start so subsequent
/// sessions attach warm. Best-effort: errors are logged and swallowed (this
/// session serves direct regardless), and the daemon's own pid lock guarantees
/// N concurrent cold starts do not produce N daemons. Does NOT block on socket
/// readiness — that would reintroduce the handshake stall this fix removes.
fn spawn_shared_daemon_best_effort(project_root: &Path, no_watch: bool) {
    match std::env::current_exe() {
        Ok(exe) => match codegraph_daemon::spawn_detached_daemon(&exe, project_root, no_watch) {
            Ok(()) => {
                tracing::debug!("serve_spawn_or_proxy: spawned shared daemon (fire-and-forget)");
            }
            Err(err) => {
                tracing::debug!(error = %err, "serve_spawn_or_proxy: daemon spawn failed; serving direct only");
            }
        },
        Err(err) => {
            tracing::debug!(error = %err, "serve_spawn_or_proxy: current_exe unavailable; serving direct only");
        }
    }
}

/// Self-heal a project's stale daemon artifacts on a failed proxy attach when
/// the recorded pid is not alive (Fix A): removes the dead daemon's leftover
/// `daemon.sock` + pid lock so the NEXT `serve --mcp` spawns a fresh daemon
/// instead of re-attaching to a socket that never answers. Liveness-gated — a
/// LIVE daemon's artifacts are preserved — so it is safe on any attach failure;
/// the current request still falls back to DIRECT serving regardless.
fn heal_stale_daemon_if_dead(project_root: &Path) {
    if codegraph_daemon::clear_stale_daemon_socket(project_root) {
        tracing::debug!("cleared stale daemon artifacts (dead pid) so the next start spawns fresh");
    }
}

fn daemon_already_running(project_root: &Path) -> bool {
    let Ok(pid_path) = codegraph_daemon::daemon_pid_path(project_root) else {
        return false;
    };
    let Ok(raw) = fs::read_to_string(&pid_path) else {
        return false;
    };
    codegraph_daemon::decode_lock_info(&raw)
        .filter(|info| info.pid > 0)
        .is_some_and(|info| codegraph_daemon::is_process_alive(info.pid))
}

fn poll_for_daemon_socket(project_root: &Path) {
    let deadline = std::time::Instant::now() + DAEMON_SOCKET_POLL_TIMEOUT;
    while std::time::Instant::now() < deadline {
        // Re-read the lock each tick: the daemon rewrites the recorded socket to
        // its bind-fallback choice during startup, so the path can change while
        // we poll (D-Daemon-b).
        if codegraph_daemon::recorded_socket_path(project_root).is_ok_and(|socket| socket.exists())
        {
            return;
        }
        std::thread::sleep(DAEMON_SOCKET_POLL_INTERVAL);
    }
}

fn daemon_opt_out() -> bool {
    std::env::var(codegraph_daemon::CODEGRAPH_NO_DAEMON).as_deref() == Ok("1")
}

fn is_daemon_internal() -> bool {
    std::env::var(codegraph_daemon::CODEGRAPH_DAEMON_INTERNAL).as_deref() == Ok("1")
}

#[derive(Debug, PartialEq, Eq)]
pub enum ServeMode {
    Direct,
    BeDaemon,
    SpawnOrProxy,
}

pub fn select_serve_mode(
    no_daemon: bool,
    is_daemon_internal: bool,
    has_codegraph: bool,
) -> ServeMode {
    if no_daemon {
        ServeMode::Direct
    } else if is_daemon_internal {
        ServeMode::BeDaemon
    } else if !has_codegraph {
        ServeMode::Direct
    } else {
        ServeMode::SpawnOrProxy
    }
}

#[cfg(test)]
mod serve_mode_tests {
    use super::{
        ColdStartAction, ServeMode, cold_start_action, debug_enabled, effective_log_level,
        emit_serve_startup_debug, guard_indexable_root, select_serve_mode,
        should_run_daemon_services, should_run_serve_services,
    };
    use crate::test_env::env_guard;
    use std::path::Path;

    #[test]
    fn debug_enabled_honors_truthy_values_only() {
        let mut env = env_guard();

        env.remove("CODEGRAPH_DEBUG");
        assert!(!debug_enabled(), "unset ⇒ off");
        env.set("CODEGRAPH_DEBUG", "1");
        assert!(debug_enabled(), "\"1\" ⇒ on");
        env.set("CODEGRAPH_DEBUG", "true");
        assert!(debug_enabled(), "\"true\" ⇒ on");
        env.set("CODEGRAPH_DEBUG", "0");
        assert!(!debug_enabled(), "\"0\" ⇒ off");
        env.set("CODEGRAPH_DEBUG", "yes");
        assert!(!debug_enabled(), "any other value ⇒ off");
    }

    #[test]
    fn effective_log_level_translates_codegraph_debug_and_defers_to_rust_log() {
        let mut env = env_guard();

        // Given RUST_LOG unset and CODEGRAPH_DEBUG unset: config level is used verbatim.
        env.remove("RUST_LOG");
        env.remove("CODEGRAPH_DEBUG");
        assert_eq!(
            effective_log_level("info"),
            "info",
            "no knobs ⇒ config level"
        );

        // When CODEGRAPH_DEBUG=1 and RUST_LOG unset: level bumps to debug (back-compat).
        env.set("CODEGRAPH_DEBUG", "1");
        assert_eq!(
            effective_log_level("info"),
            "debug",
            "CODEGRAPH_DEBUG=1 ⇒ debug"
        );

        // When RUST_LOG is set: the base opens to trace so the EnvFilter is the
        // sole gate (the reload floor must not cap RUST_LOG upward).
        env.set("RUST_LOG", "warn");
        assert_eq!(
            effective_log_level("info"),
            "trace",
            "RUST_LOG set ⇒ base opens to trace; EnvFilter owns the gate"
        );
    }

    #[test]
    fn select_serve_mode_decision_order() {
        assert_eq!(select_serve_mode(true, false, true), ServeMode::Direct);
        assert_eq!(select_serve_mode(false, true, true), ServeMode::BeDaemon);
        assert_eq!(select_serve_mode(false, false, false), ServeMode::Direct);
        assert_eq!(
            select_serve_mode(false, false, true),
            ServeMode::SpawnOrProxy
        );
    }

    #[test]
    fn cold_start_action_warm_proxies_cold_spawns_then_serves_direct() {
        assert_eq!(
            cold_start_action(true),
            ColdStartAction::ProxyToRunningDaemon,
            "a live daemon ⇒ attach via proxy (warm path unchanged)"
        );
        assert_eq!(
            cold_start_action(false),
            ColdStartAction::SpawnDaemonAndServeDirect,
            "no live daemon ⇒ fire-and-forget spawn + serve direct immediately (no blocking proxy)"
        );
    }

    #[test]
    fn serve_services_gate_skips_unindexed_bare_cwd_but_runs_when_explicit_or_indexed() {
        let seq = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let unindexed =
            std::env::temp_dir().join(format!("cg-serve-gate-unidx-{}-{seq}", std::process::id()));
        let indexed =
            std::env::temp_dir().join(format!("cg-serve-gate-idx-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&unindexed).unwrap();
        std::fs::create_dir_all(indexed.join(".codegraph")).unwrap();

        assert!(
            should_run_serve_services(true, &unindexed),
            "explicit --path must run services even on an unindexed root"
        );
        assert!(
            !should_run_serve_services(false, &unindexed),
            "bare serve from an unindexed cwd must NOT run services (keeps cwd adoptable)"
        );
        assert!(
            should_run_serve_services(false, &indexed),
            "an already-indexed cwd must keep services"
        );

        let _ = std::fs::remove_dir_all(&unindexed);
        let _ = std::fs::remove_dir_all(&indexed);
    }

    #[test]
    fn daemon_services_disabled_at_home_and_root_enabled_for_nested_project() {
        let mut env = env_guard();
        let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };

        let tmp = std::env::temp_dir().join(format!("cg-serve-home-{}", std::process::id()));
        let nested = tmp.join("workspace/ProdDir/AI/codegraph-rust");
        std::fs::create_dir_all(&nested).unwrap();
        env.set(home_key, &tmp);

        assert!(
            !should_run_daemon_services(&tmp),
            "$HOME must disable daemon services"
        );
        assert!(
            !should_run_daemon_services(Path::new("/")),
            "filesystem root must disable daemon services"
        );
        assert!(
            should_run_daemon_services(&nested),
            "a project nested under $HOME must keep daemon services"
        );

        env.assert_intact();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn guard_indexable_root_rejects_home_and_root_allows_nested_project() {
        let mut env = env_guard();
        let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };

        let tmp = std::env::temp_dir().join(format!("cg-guard-home-{}", std::process::id()));
        let nested = tmp.join("workspace/proj");
        std::fs::create_dir_all(&nested).unwrap();
        env.set(home_key, &tmp);

        assert!(
            guard_indexable_root(&tmp).is_err(),
            "$HOME must be refused as an index root"
        );
        assert!(
            guard_indexable_root(Path::new("/")).is_err(),
            "filesystem root must be refused as an index root"
        );
        assert!(
            guard_indexable_root(&nested).is_ok(),
            "a project nested under $HOME must be indexable"
        );

        env.assert_intact();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn emit_serve_startup_debug_runs_for_every_mode() {
        let root = std::env::temp_dir().join(format!("cg-serve-dbg-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        for mode in [
            ServeMode::Direct,
            ServeMode::BeDaemon,
            ServeMode::SpawnOrProxy,
        ] {
            emit_serve_startup_debug(&root, true, false, &mode);
            emit_serve_startup_debug(&root, false, true, &mode);
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}

fn cmd_unlock(path: Option<PathBuf>) -> Result<()> {
    let start = absolute_path(path.unwrap_or_else(|| PathBuf::from(".")));
    index_paths(&start)?;
    let project = resolve_project_path_optional(&start);
    warn_ancestor_index_retarget(&start, &project);
    let paths = index_paths(&project)?;
    let daemon_lock = codegraph_daemon::daemon_pid_path(&project)?;
    let daemon_removed = daemon_lock.exists() && codegraph_daemon::unlock_project(&project);
    let lock = paths.current_root().join("codegraph.lock");
    let mut lock_removed = false;
    if lock.exists() {
        fs::remove_file(&lock).with_context(|| format!("removing {}", lock.display()))?;
        lock_removed = true;
    }
    if lock_removed || daemon_removed {
        println!("Removed lock file. You can now run indexing again.");
    } else {
        println!("No lock file found - nothing to do");
    }

    if matches!(
        Store::extraction_status(&paths),
        ExtractionStatus::Building { .. }
    ) {
        match IndexLease::acquire_shared_existing(
            &paths,
            std::time::Instant::now() + STATUS_LEASE_TIMEOUT,
            || false,
        ) {
            Ok(_lease) => {
                if matches!(
                    Store::extraction_status(&paths),
                    ExtractionStatus::Building { .. }
                ) {
                    println!(
                        "Index state remains building; no rollback was performed. Run `codegraph index --force {}` to rebuild it (or `codegraph init {}`).",
                        project.display(),
                        project.display()
                    );
                }
            }
            Err(codegraph_store::IndexLeaseError::TimedOut { .. }) => {
                println!("Index build is still running; no recovery command was issued.");
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn cmd_callers(
    symbol: String,
    path: Option<PathBuf>,
    limit: usize,
    json_output: bool,
    strict: bool,
    file: Option<String>,
) -> Result<()> {
    let project = resolve_required_project(path)?;
    let store = open_store(&project)?;
    let nodes = related_nodes_for_symbol(
        &store,
        &project,
        &symbol,
        limit,
        Related::Callers,
        file.as_deref(),
    )?;
    let godot = godot_honesty_for_symbol(&store, &project, &symbol)?;
    if json_output {
        print_json_pretty(&json!({
            "symbol": symbol,
            "file": file,
            "callers": nodes,
            "godotDynamic": godot.as_json(),
        }))?;
    } else {
        print_related(
            "Callers",
            &describe_symbol(&symbol, file.as_deref()),
            &nodes,
        );
        godot.print_cli(nodes.is_empty());
    }
    if strict && nodes.is_empty() {
        bail!("codegraph callers: no callers found for \"{symbol}\"");
    }
    Ok(())
}

fn cmd_callees(
    symbol: String,
    path: Option<PathBuf>,
    limit: usize,
    json_output: bool,
    strict: bool,
    file: Option<String>,
) -> Result<()> {
    let project = resolve_required_project(path)?;
    let store = open_store(&project)?;
    let nodes = related_nodes_for_symbol(
        &store,
        &project,
        &symbol,
        limit,
        Related::Callees,
        file.as_deref(),
    )?;
    if json_output {
        print_json_pretty(&json!({ "symbol": symbol, "file": file, "callees": nodes }))?;
    } else {
        print_related(
            "Callees",
            &describe_symbol(&symbol, file.as_deref()),
            &nodes,
        );
    }
    if strict && nodes.is_empty() {
        bail!("codegraph callees: no callees found for \"{symbol}\"");
    }
    Ok(())
}

/// The shared Godot reverse-referrer fan-out for `impact` and `affected`. A
/// `.tscn`/`.tres`/`project.godot` target owns no `file:` node, so its
/// referrers live only in the path-keyed reverse lanes. Fanning out to all
/// three — the 6-subkind `unresolved_refs` lane, the untagged `main_scene`
/// lane, and the loader `import:`-name lane — behind ONE helper keeps `impact`
/// and `affected` from diverging. Sorted + deduped; deterministic.
fn godot_reverse_referrers(store: &Store, file: &str) -> Result<Vec<String>> {
    let mut referrers = store.dependent_file_paths_unresolved(file)?;
    referrers.extend(store.dependent_file_paths_via_import_name(file)?);
    referrers.extend(store.dependent_file_paths_main_scene(file)?);
    referrers.sort();
    referrers.dedup();
    Ok(referrers)
}

fn cmd_impact(
    symbol: String,
    path: Option<PathBuf>,
    depth: usize,
    json_output: bool,
    strict: bool,
    file: Option<String>,
) -> Result<()> {
    let project = resolve_required_project(path)?;
    let store = open_store(&project)?;
    let depth = depth.clamp(1, 10);
    let matches = symbol_matches(&store, &project, &symbol)?;
    if matches.is_empty() {
        let message = lookup_symbol_not_found_message(&symbol);
        println!("{message}");
        if strict {
            bail!("codegraph impact: symbol \"{symbol}\" not found");
        }
        bail!(message);
    }
    let exact_matches = exact_or_top_matches(&matches, &symbol);
    if exact_matches.is_empty() {
        bail!(lookup_symbol_not_found_message(&symbol));
    }
    let exact_matches = filter_matches_by_file(exact_matches, &symbol, file.as_deref())?;
    let traverser = GraphTraverser::new(&store);
    let mut nodes = HashMap::new();
    let mut edge_keys = HashSet::new();
    let mut godot_files: Vec<String> = Vec::new();
    for node in exact_matches {
        let impact = traverser.get_impact_radius(&node.id, depth)?;
        for (id, node) in impact.nodes {
            nodes.insert(id, node);
        }
        for edge in impact.edges {
            edge_keys.insert((edge.source, edge.target, edge.kind));
        }
        if is_godot_resource_target_node(node) && !godot_files.contains(&node.file_path) {
            godot_files.push(node.file_path.clone());
        }
    }
    let mut godot_referrers: Vec<String> = Vec::new();
    for file in &godot_files {
        godot_referrers.extend(godot_reverse_referrers(&store, file)?);
    }
    godot_referrers.sort();
    godot_referrers.dedup();
    // A loader-side referrer may already be represented by a graph edge (for
    // example, a GDScript `preload`). Keep the path-keyed resource lane
    // structurally disjoint from traversal nodes before counting or rendering.
    let traversal_file_paths = nodes
        .values()
        .map(|node| node.file_path.as_str())
        .collect::<HashSet<_>>();
    godot_referrers.retain(|file| !traversal_file_paths.contains(file.as_str()));
    let resource_edge_count = godot_referrers.len();
    let mut affected = nodes.values().map(NodeSummary::from).collect::<Vec<_>>();
    for from_file in godot_referrers {
        affected.push(NodeSummary {
            name: from_file.clone(),
            kind: NodeKind::File,
            file_path: from_file,
            start_line: 0,
        });
    }
    let godot = godot_honesty_for_symbol(&store, &project, &symbol)?;
    if json_output {
        print_json_pretty(&json!({
            "symbol": symbol,
            "file": file,
            "depth": depth,
            "nodeCount": affected.len(),
            "edgeCount": edge_keys.len() + resource_edge_count,
            "resourceEdgeCount": resource_edge_count,
            "affected": affected,
            "godotDynamic": godot.as_json(),
        }))?;
    } else {
        println!(
            "\nImpact of changing \"{}\" - {} affected symbols:\n",
            describe_symbol(&symbol, file.as_deref()),
            affected.len()
        );
        print_by_file(&affected);
        godot.print_cli(affected.is_empty());
    }
    Ok(())
}

fn cmd_explore(
    query: String,
    path: Option<PathBuf>,
    max_files: Option<usize>,
    json_output: bool,
) -> Result<()> {
    let project = resolve_required_project(path)?;
    let engine = codegraph_mcp::CodeGraphEngine::open(&project)?;
    let mut args = json!({ "query": query });
    if let Some(max_files) = max_files {
        args["maxFiles"] = json!(max_files);
    }
    let result = engine.execute("codegraph_explore", &args);
    print_engine_result("explore", &query, &result, json_output, false)
}

fn cmd_node(
    target: String,
    path: Option<PathBuf>,
    file: Option<String>,
    symbols_only: bool,
    json_output: bool,
    strict: bool,
) -> Result<()> {
    let project = resolve_required_project(path)?;
    let engine = codegraph_mcp::CodeGraphEngine::open(&project)?;
    // #1314: `-f/--file` PINS an overloaded symbol to one file and must still
    // carry `includeCode`, exactly like the bare-symbol branch below — the
    // pinned definition's source body is the whole point of the pin.
    let args = if let Some(file) = &file {
        json!({ "symbol": target, "file": file, "includeCode": true })
    } else if node_target_is_file(&engine, &target) {
        json!({ "file": target, "symbolsOnly": symbols_only })
    } else {
        json!({ "symbol": target, "includeCode": true })
    };
    let result = engine.execute("codegraph_node", &args);
    print_engine_result("node", &target, &result, json_output, strict)
}

/// Decide whether a `codegraph node <target>` argument names an indexed FILE (→
/// file-view mode) or a SYMBOL (→ symbol mode). A path separator or a match
/// against an indexed file path means file mode; a bare identifier is a symbol.
fn node_target_is_file(engine: &codegraph_mcp::CodeGraphEngine, target: &str) -> bool {
    if target.contains(['/', '\\']) {
        return true;
    }
    engine
        .indexed_file_paths()
        .map(|paths| {
            paths
                .iter()
                .any(|p| p == target || p.rsplit('/').next() == Some(target))
        })
        .unwrap_or(false)
}

/// Render an MCP-engine `ToolResult` for a CLI subcommand: the plain rendered
/// text (matching the MCP tool byte-for-byte), or a `{command, query, output,
/// isError}` JSON envelope under `--json`. A tool-level error exits non-zero so
/// scripts can detect a failed lookup.
fn print_engine_result(
    command: &str,
    query: &str,
    result: &codegraph_mcp::protocol::ToolResult,
    json_output: bool,
    strict: bool,
) -> Result<()> {
    let text = result
        .content
        .iter()
        .map(|c| c.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let is_error = result.is_error.unwrap_or(false);
    if json_output {
        print_json_pretty(&json!({
            "command": command,
            "query": query,
            "output": text,
            "isError": is_error,
        }))?;
    } else {
        println!("{text}");
    }
    if is_error {
        bail!("codegraph {command} failed: {text}");
    }
    if strict && result.not_found.unwrap_or(false) {
        bail!("codegraph {command}: \"{query}\" not found");
    }
    Ok(())
}

fn cmd_affected(
    files: Vec<String>,
    path: Option<PathBuf>,
    depth: usize,
    filter: Option<String>,
) -> Result<()> {
    let project = resolve_required_project(path)?;
    let store = open_store(&project)?;
    if files.is_empty() {
        println!("No files provided. Use file arguments.");
        return Ok(());
    }
    let mut affected = HashSet::new();
    let mut traversed = HashSet::new();
    for file in &files {
        if is_test_file(file, filter.as_deref()) {
            affected.insert(file.clone());
            continue;
        }
        let mut queue = VecDeque::from([(file.clone(), 0usize)]);
        let mut visited = HashSet::from([file.clone()]);
        while let Some((current, current_depth)) = queue.pop_front() {
            if current_depth >= depth {
                continue;
            }
            let mut dependents = store.dependent_file_paths(&current)?;
            dependents.extend(godot_reverse_referrers(&store, &current)?);
            dependents.sort();
            dependents.dedup();
            for dependent in dependents {
                if !visited.insert(dependent.clone()) {
                    continue;
                }
                traversed.insert(dependent.clone());
                if is_test_file(&dependent, filter.as_deref()) {
                    affected.insert(dependent);
                } else {
                    queue.push_back((dependent, current_depth + 1));
                }
            }
        }
    }
    let mut sorted = affected.iter().cloned().collect::<Vec<_>>();
    sorted.sort();
    let affected_files = traversed
        .iter()
        .chain(affected.iter())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    print_json_pretty(&json!({
        "changedFiles": files,
        "affectedTests": sorted,
        "affectedFiles": affected_files,
        "totalDependentsTraversed": traversed.len(),
    }))?;
    Ok(())
}

fn cmd_check(path: Option<PathBuf>, json_output: bool) -> Result<()> {
    let project = resolve_required_project(path)?;
    let store = open_store(&project)?;
    let traverser = GraphTraverser::new(&store);
    let cycles = traverser.find_circular_dependencies()?;
    if json_output {
        print_json_pretty(&json!({ "cycles": cycles }))?;
    } else if cycles.is_empty() {
        println!("No circular dependencies found");
    } else {
        println!("\nFound {} circular dependencies:\n", cycles.len());
        for cycle in &cycles {
            let mut chain = cycle.clone();
            if let Some(first) = cycle.first() {
                chain.push(first.clone());
            }
            println!("  {}", chain.join(" \u{2192} "));
        }
    }
    Ok(())
}

fn audit_prefix_keep(path: &str, include: &[String], exclude: &[String]) -> bool {
    let normalized = path.replace('\\', "/");
    let under = |prefix: &String| normalized.starts_with(&prefix.replace('\\', "/"));
    if !include.is_empty() && !include.iter().any(under) {
        return false;
    }
    !exclude.iter().any(under)
}

struct AuditArgs {
    path: Option<PathBuf>,
    orphans: bool,
    dangling: bool,
    impact: Option<String>,
    verify_plan: bool,
    include: Vec<String>,
    exclude: Vec<String>,
    json_output: bool,
}

fn cmd_audit(args: AuditArgs) -> Result<()> {
    let AuditArgs {
        path,
        orphans,
        dangling,
        impact,
        verify_plan,
        include,
        exclude,
        json_output,
    } = args;
    if !orphans && !dangling && impact.is_none() {
        bail!("audit requires at least one of --orphans, --dangling, --impact <path>");
    }
    let project = resolve_required_project(path)?;
    let store = open_store(&project)?;
    let traverser = GraphTraverser::new(&store);

    let mut orphan_list = if orphans {
        traverser.find_orphan_resources()?
    } else {
        Vec::new()
    };
    orphan_list.retain(|o| audit_prefix_keep(&o.file_path, &include, &exclude));
    let mut dangling_list = if dangling {
        traverser.find_dangling_references(&project)?
    } else {
        Vec::new()
    };
    dangling_list.retain(|d| audit_prefix_keep(&d.from_file, &include, &exclude));
    let impact_result = match &impact {
        Some(changed) => {
            let normalized = normalize_impact_input(changed, &project);
            let mut result = traverser.resource_impact(&normalized)?;
            result
                .affected
                .retain(|a| audit_prefix_keep(&a.from_file, &include, &exclude));
            Some(result)
        }
        None => None,
    };

    if json_output {
        let mut out = serde_json::Map::new();
        if orphans {
            out.insert("orphans".to_string(), json!(orphan_list));
        }
        if dangling {
            out.insert("dangling".to_string(), json!(dangling_list));
        }
        if let Some(result) = &impact_result {
            out.insert("impact".to_string(), json!(result));
            if let Some(note) = empty_impact_note(result) {
                out.insert("note".to_string(), json!(note));
            }
            if verify_plan {
                out.insert("verifyPlan".to_string(), json!(verify_plan_view(result)));
            }
        }
        print_json_pretty(&serde_json::Value::Object(out))?;
        return Ok(());
    }

    if orphans {
        print_audit_orphans(&orphan_list);
    }
    if dangling {
        print_audit_dangling(&dangling_list);
    }
    if let Some(result) = &impact_result {
        print_audit_impact(result);
        if verify_plan {
            print_verify_plan(&verify_plan_view(result));
        }
    }
    Ok(())
}

/// Derived load/open plan for one impact result: the `.gd` scripts to reload and
/// `.tscn` scenes to reopen that reference the changed path, plus per-site reasons.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifyPlan {
    changed: String,
    load_scripts: Vec<String>,
    load_resources: Vec<String>,
    open_scenes: Vec<String>,
    reasons: Vec<VerifyReason>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifyReason {
    file: String,
    line: i64,
    edge_kind: String,
    target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    edge_subkind: Option<String>,
}

/// Normalize a raw `audit --impact <changed>` value into the project-relative,
/// `/`-separated form that `resource_impact` expects. Strict order: strip a
/// leading `res://` FIRST (so a `res://…` value is never mistaken for an
/// absolute path), then a leading `./` or `.\`, then convert `\` to `/`. If the
/// result is an OS-absolute path under the project root, make it relative; an
/// absolute path outside the root passes through unchanged (yields an empty
/// impact rather than an error).
fn normalize_impact_input(changed: &str, project: &Path) -> String {
    let mut s = changed;
    if let Some(rest) = s.strip_prefix("res://") {
        s = rest;
    }
    if let Some(rest) = s.strip_prefix("./").or_else(|| s.strip_prefix(".\\")) {
        s = rest;
    }
    let s = s.replace('\\', "/");
    let candidate = Path::new(&s);
    if candidate.is_absolute()
        && let Ok(rel) = candidate.strip_prefix(project)
    {
        return rel.to_string_lossy().replace('\\', "/");
    }
    s
}

fn verify_plan_view(impact: &codegraph_graph::graph::ResourceImpact) -> VerifyPlan {
    let mut load_scripts: Vec<String> = Vec::new();
    let mut load_resources: Vec<String> = Vec::new();
    let mut open_scenes: Vec<String> = Vec::new();
    let mut reasons: Vec<VerifyReason> = Vec::new();
    if impact.changed.ends_with(".gd") {
        load_scripts.push(res_path(&impact.changed));
    } else if impact.changed.ends_with(".tres") || impact.changed.ends_with(".res") {
        load_resources.push(res_path(&impact.changed));
    } else if impact.changed.ends_with(".tscn") {
        open_scenes.push(res_path(&impact.changed));
    }
    for affected in &impact.affected {
        if affected.from_file.ends_with(".gd") {
            load_scripts.push(res_path(&affected.from_file));
        } else if affected.from_file.ends_with(".tres") || affected.from_file.ends_with(".res") {
            load_resources.push(res_path(&affected.from_file));
        } else if affected.from_file.ends_with(".tscn") {
            open_scenes.push(res_path(&affected.from_file));
        }
        reasons.push(VerifyReason {
            file: affected.from_file.clone(),
            line: affected.line,
            edge_kind: affected.edge_kind.clone(),
            target: affected.target.clone(),
            edge_subkind: affected.edge_subkind.clone(),
        });
    }
    load_scripts.sort();
    load_scripts.dedup();
    load_resources.sort();
    load_resources.dedup();
    open_scenes.sort();
    open_scenes.dedup();
    VerifyPlan {
        changed: impact.changed.clone(),
        load_scripts,
        load_resources,
        open_scenes,
        reasons,
    }
}

fn res_path(rel: &str) -> String {
    format!("res://{}", rel.replace('\\', "/"))
}

/// The static boundary note for an EMPTY impact on a Godot resource/script path:
/// "nothing references X" is not proof of zero use (data-driven numeric-id/DSL
/// refs are not followed). `None` when the impact is non-empty or the path is not
/// a Godot resource/script.
fn empty_impact_note(impact: &codegraph_graph::graph::ResourceImpact) -> Option<String> {
    if !impact.affected.is_empty() {
        return None;
    }
    if !is_godot_resource_path(&impact.changed) {
        return None;
    }
    Some(
        "no static references found; godot data-driven numeric-id/DSL references are not included by default"
            .to_string(),
    )
}

fn is_godot_resource_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".tres")
        || lower.ends_with(".tscn")
        || lower.ends_with(".res")
        || lower.ends_with(".gd")
}

/// Whether a matched `impact` node names a Godot RESOURCE whose reverse
/// referrers should fold into the impact set — as opposed to a symbol that
/// merely lives in a Godot file. A `.tscn`/`.tres`/`.res` owns no `file:` node,
/// so its ONLY node is a scene-level `Constant` (the whole-resource target); a
/// `.gd` folds only when the matched node is its `file:` node (a whole-file
/// target), never when it is a symbol (function/class) inside that script — that
/// would wrongly attach the script's scene/autoload referrers to a symbol-level
/// query.
fn is_godot_resource_target_node(node: &codegraph_core::types::Node) -> bool {
    if !is_godot_resource_path(&node.file_path) {
        return false;
    }
    let lower = node.file_path.to_ascii_lowercase();
    if lower.ends_with(".gd") {
        return node.kind == NodeKind::File;
    }
    true
}

fn print_audit_orphans(orphans: &[codegraph_graph::graph::OrphanResource]) {
    if orphans.is_empty() {
        println!("No orphan resources found");
    } else {
        println!("\nFound {} orphan resources:\n", orphans.len());
        for orphan in orphans {
            print!("  {} [{}]", orphan.file_path, orphan.confidence);
            if let Some(note) = &orphan.note {
                print!(" \u{2014} {note}");
            }
            println!();
        }
    }
}

fn print_audit_dangling(dangling: &[codegraph_graph::graph::DanglingRef]) {
    if dangling.is_empty() {
        println!("No dangling references found");
    } else {
        println!("\nFound {} dangling references:\n", dangling.len());
        for reference in dangling {
            println!(
                "  {}:{} \u{2192} {} ({})",
                reference.from_file, reference.line, reference.target_path, reference.kind
            );
        }
    }
}

fn print_audit_impact(impact: &codegraph_graph::graph::ResourceImpact) {
    if impact.affected.is_empty() {
        println!("\nNothing references {}", impact.changed);
        if let Some(note) = empty_impact_note(impact) {
            println!("  note: {note}");
        }
    } else {
        println!(
            "\n{} is referenced by {} site(s):\n",
            impact.changed,
            impact.affected.len()
        );
        for affected in &impact.affected {
            match &affected.edge_subkind {
                Some(subkind) => println!(
                    "  {}:{} ({}/{})",
                    affected.from_file, affected.line, affected.edge_kind, subkind
                ),
                None => println!(
                    "  {}:{} ({})",
                    affected.from_file, affected.line, affected.edge_kind
                ),
            }
        }
    }
}

fn print_verify_plan(plan: &VerifyPlan) {
    println!("\nverify-plan for {}:", plan.changed);
    println!("  loadScripts ({}):", plan.load_scripts.len());
    for script in &plan.load_scripts {
        println!("    {script}");
    }
    println!("  loadResources ({}):", plan.load_resources.len());
    for resource in &plan.load_resources {
        println!("    {resource}");
    }
    println!("  openScenes ({}):", plan.open_scenes.len());
    for scene in &plan.open_scenes {
        println!("    {scene}");
    }
}

fn cmd_export(path: Option<PathBuf>, out: Option<PathBuf>, no_centrality: bool) -> Result<()> {
    let project = resolve_required_project(path)?;
    let store = open_store(&project)?;
    let graph = codegraph_graph::export::node_link_graph_opts(&store, !no_centrality)?;
    let rendered = serde_json::to_string_pretty(&graph)?;
    match out {
        Some(out_path) => {
            fs::write(&out_path, rendered.as_bytes())
                .with_context(|| format!("writing graph export to {}", out_path.display()))?;
            let counts = store.counts()?;
            eprintln!(
                "Exported {} nodes / {} edges to {}",
                counts.node_count,
                counts.edge_count,
                out_path.display()
            );
        }
        None => println!("{rendered}"),
    }
    Ok(())
}

#[derive(Debug)]
struct IndexSummary {
    files_indexed: i64,
    files_skipped: i64,
    files_errored: i64,
    nodes_created: i64,
    edges_created: i64,
    duration_ms: i64,
}

// Progress is a pure side effect: it only counts/displays and never gates,
// reorders, or alters extraction, so golden byte-equivalence is preserved. It
// draws to stderr (stdout carries JSON / golden output) and is hidden when
// stderr is not a TTY or `--quiet`, so CI logs and pipes stay clean.
fn progress_bar(len: u64, quiet: bool, template: &str) -> ProgressBar {
    if quiet || !io::stderr().is_terminal() {
        return ProgressBar::hidden();
    }
    let bar = ProgressBar::with_draw_target(Some(len), ProgressDrawTarget::stderr());
    if let Ok(style) = ProgressStyle::with_template(template) {
        bar.set_style(style.progress_chars("=>-"));
    }
    bar
}

fn spinner(quiet: bool, template: &str) -> ProgressBar {
    if quiet || !io::stderr().is_terminal() {
        return ProgressBar::hidden();
    }
    let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr());
    if let Ok(style) = ProgressStyle::with_template(template) {
        bar.set_style(style);
    }
    bar.enable_steady_tick(std::time::Duration::from_millis(100));
    bar
}

// A labeled phase spinner that ticks while running. `finish_phase` retains a
// "✓ <label> (<elapsed>)" summary line on stderr (vs finish_and_clear which
// wipes it); gated like the other indicators.
fn phase_spinner(label: &str, quiet: bool) -> ProgressBar {
    if quiet || !io::stderr().is_terminal() {
        return ProgressBar::hidden();
    }
    let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr());
    if let Ok(style) = ProgressStyle::with_template("{spinner:.green} {msg}") {
        bar.set_style(style);
    }
    bar.set_message(label.to_string());
    bar.enable_steady_tick(std::time::Duration::from_millis(100));
    bar
}

fn finish_phase(bar: &ProgressBar, label: &str) {
    if bar.is_hidden() {
        return;
    }
    let elapsed = format_duration(bar.elapsed().as_millis() as i64);
    if let Ok(style) = ProgressStyle::with_template("{msg}") {
        bar.set_style(style);
    }
    bar.abandon_with_message(format!("✓ {label} ({elapsed})"));
}

fn index_project(
    project: &Path,
    kind: codegraph_store::RebuildKind,
    diagnostics: &DiagnosticArgs,
    command: &'static str,
) -> Result<IndexSummary> {
    index_project_inner(project, kind, false, false, diagnostics, command)
}

/// Owns one destructive v2 rebuild for the whole full-index body.
///
/// `begin` acquires the single outer exclusive `IndexLease`, classifies under it,
/// publishes `phase=building`, removes the previous database files, and opens
/// the fresh write-capable target. [`Self::finish`] is the EXPLICIT FALLIBLE
/// completion path required by the frozen plan (lines 548-556): under the same
/// retained lease it restores the shared `synchronous=NORMAL` durability, runs the
/// final checkpoint + compaction, stamps extraction version 2, checkpoints that
/// stamp into the main database file, closes the final SQLite connection, and only
/// then publishes `phase=current` (removing a tombstone solely for a successful
/// explicit `init`). Every failure propagates.
///
/// `Drop` is emergency best-effort cleanup only: it can never publish `Current`,
/// so an index that bails out early via `?` leaves the namespace `phase=building`
/// — unreadable and fail-closed — and a rerun rebuilds it from scratch.
/// `Self::finish` consumes the guard, which is what disarms that fallback.
struct BulkIndexPragmaGuard {
    rebuild: Option<codegraph_store::ActiveFullRebuild>,
}

/// Bounded wall-clock budget for acquiring the one outer exclusive lease. Never a
/// blocking wait: `IndexLease` polls `try_lock` against this monotonic deadline.
const REBUILD_LEASE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const READ_LEASE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const STATUS_LEASE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(100);

impl BulkIndexPragmaGuard {
    fn begin(
        paths: &codegraph_core::IndexPaths,
        kind: codegraph_store::RebuildKind,
    ) -> Result<Self> {
        let deadline = std::time::Instant::now() + REBUILD_LEASE_TIMEOUT;
        let rebuild = codegraph_store::begin_full_rebuild(paths, kind, deadline, || false)?;
        let rebuild = rebuild.open_store()?;
        Ok(Self {
            rebuild: Some(rebuild),
        })
    }

    fn finish(mut self) -> Result<()> {
        let rebuild = self
            .rebuild
            .take()
            .expect("a rebuild guard is finished at most once");
        rebuild.finish().map_err(Into::into)
    }
}

impl std::ops::Deref for BulkIndexPragmaGuard {
    type Target = codegraph_store::ActiveFullRebuild;

    fn deref(&self) -> &Self::Target {
        self.rebuild
            .as_ref()
            .expect("the CLI guard owns its active rebuild until finish")
    }
}

impl std::ops::DerefMut for BulkIndexPragmaGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.rebuild
            .as_mut()
            .expect("the CLI guard owns its active rebuild until finish")
    }
}

impl Drop for BulkIndexPragmaGuard {
    fn drop(&mut self) {
        if let Some(rebuild) = self.rebuild.take() {
            // No publication happens here by construction: the namespace stays
            // `phase=building`, so nothing can read a half-built graph.
            tracing::warn!(
                root = %rebuild.paths().current_root().display(),
                "full index did not finalize; index remains phase=building and unreadable",
            );
        }
    }
}

fn index_project_inner(
    project: &Path,
    kind: codegraph_store::RebuildKind,
    verbose: bool,
    quiet: bool,
    diagnostics: &DiagnosticArgs,
    command: &'static str,
) -> Result<IndexSummary> {
    let started = std::time::Instant::now();
    let paths = index_paths(project)?;
    let index_root = paths.current_root().to_path_buf();
    // THIS project's own immutable config + extension overrides, read from its
    // resolved current index root. Nothing consults a process-global value,
    // another project's root, or the process working directory.
    let config = Config::load_for_paths(None, &paths)?;
    let extensions = codegraph_extract::ExtensionOverrides::load_for_paths(&paths);
    let options = ExtractOptions::for_project(&config, extensions);
    let framework_context = codegraph_resolve::framework::FrameworkExtractionContext::new(
        project.to_string_lossy().into_owned(),
        codegraph_resolve::frameworks::godot_dsl_config::GodotDslConfig::load_for_paths(&paths),
    );
    if !quiet {
        eprintln!("Scanning files…");
    }
    let scan_started = std::time::Instant::now();
    let files = codegraph_extract::engine::scan_project(project, &options)?;
    let scan_duration = scan_started.elapsed();
    let mut diagnostic_run = DiagnosticRun::start(
        project,
        &index_root,
        command,
        diagnostics,
        json!({
            "version": VERSION,
            "platform": std::env::consts::OS,
            "architecture": std::env::consts::ARCH,
            "rayonThreads": rayon::current_num_threads(),
            "command": command,
            "fileTotal": files.len(),
            "windowSize": PARSE_REORDER_WINDOW,
            "configLimits": {
                "maxFileSizeBytes": options.max_file_size,
                "includeRuleCount": options.include.len(),
                "excludeRuleCount": options.exclude.len(),
                "ignoreDirCount": options.ignore_dirs.len(),
                "ignorePathCount": options.ignore_paths.len(),
            },
        }),
    )?;
    if let Some(path) = diagnostic_run.path() {
        // This one line is intentionally independent of --quiet so a feedback
        // bundle remains discoverable when all ordinary progress is suppressed.
        eprintln!("Debug log: {}", path.display());
    }
    diagnostic_run.phase_start("scan");
    diagnostic_run.phase_end("scan", scan_duration, json!({ "fileTotal": files.len() }));

    // One destructive rebuild under ONE outer exclusive lease: classify, publish
    // `phase=building`, remove the previous DB files, then open the fresh
    // write-capable target. The index root and DB are created by the rebuild
    // layer, so nothing below reconstructs a path or reopens the namespace.
    //
    // `synchronous=OFF` + a larger cache/mmap window speed up the from-scratch bulk
    // index. Their restore is part of the guard's EXPLICIT FALLIBLE `finish`, not a
    // trailing statement, because every `?` below would skip a trailing restore.
    // If the body bails out early, the guard's Drop only attempts state-gated
    // pragma repair/compaction/close and publishes nothing: the namespace stays
    // `phase=building` and unreadable.
    let mut store = BulkIndexPragmaGuard::begin(&paths, kind)?;
    diagnostic_run.relocate_to_index_root(&index_root);
    store.set_bulk_index_pragmas()?;

    let before = store.counts()?;
    let mut files_indexed = 0;
    let mut files_skipped = 0;
    let mut files_errored = 0;

    // Stream the graph to the store in capped batches instead of holding the whole
    // project in memory. Equivalence with the all-at-once path is byte-for-byte and
    // load-bearing, so the original insert order is reproduced exactly:
    //   1. nodes flush in sorted `scan_project` file order, each file's nodes in
    //      emission order — the resolver's name-matcher tie-break reads candidates
    //      back in node rowid order (`codegraph-resolve` `order_candidates`);
    //   2. ALL nodes are written before ANY edge, because `insert_edges` drops edges
    //      whose endpoints are absent;
    //   3. edges then refs replay in the same file order, so their autoincrement
    //      rowids match the all-at-once path.
    // Edges/refs cannot flush during the node pass (rule 2) and would dominate memory,
    // so they spill to a temp file and stream back in a second batched pass.
    const NODE_FLUSH_ROWS: usize = 10_000;
    const EDGE_FLUSH_ROWS: usize = 20_000;
    const REF_FLUSH_ROWS: usize = 20_000;
    const RESOLVE_BATCH_ROWS: usize = 5_000;

    let mut spill = SpillWriter::new(index_root.clone())?;
    let mut pending_nodes: Vec<Node> = Vec::with_capacity(NODE_FLUSH_ROWS);

    let bar = progress_bar(
        files.len() as u64,
        quiet,
        "{spinner:.green} Indexing [{bar:30}] {pos}/{len} files ({elapsed}) {wide_msg}",
    );
    if verbose {
        bar.set_message(format!(
            "parsing ({} threads)",
            rayon::current_num_threads()
        ));
    }

    // The scheduler, not workers, owns the reorder window. It submits at most
    // 512 files initially and replenishes one slot only after one sorted result
    // has been persisted. Workers only read + parse and therefore never block on
    // window backpressure; the single consumer remains the only Store writer.
    let base_message = if verbose {
        format!("parsing ({} threads)", rayon::current_num_threads())
    } else {
        "parsing".to_string()
    };
    bar.set_message(base_message.clone());
    bar.enable_steady_tick(std::time::Duration::from_millis(250));
    let (tracker, monitor) = IndexTracker::start(bar.clone(), diagnostic_run.sink(), base_message);
    diagnostic_run.phase_start("parse_write");
    let parse_started = std::time::Instant::now();

    type ParsePayload = (String, FileRecord, ExtractionResult);
    let schedule_tracker = tracker.clone();
    let parse_tracker = tracker.clone();
    let buffer_tracker = tracker.clone();
    let persist_tracker = tracker.clone();
    let parse_result = run_ordered_parallel_window(
        files.len(),
        PARSE_REORDER_WINDOW,
        |index| schedule_tracker.scheduled(index, &files[index]),
        |index| -> Result<ParsePayload> {
            let relative = &files[index];
            let full = project.join(relative);

            parse_tracker.stage(index, "metadata");
            let metadata = match fs::metadata(&full)
                .with_context(|| format!("reading metadata for {}", full.display()))
            {
                Ok(metadata) => metadata,
                Err(error) => {
                    parse_tracker.failed(index, &error);
                    return Err(error);
                }
            };

            parse_tracker.stage(index, "read");
            let source = match fs::read_to_string(&full)
                .with_context(|| format!("reading source file {}", full.display()))
            {
                Ok(source) => source,
                Err(error) => {
                    parse_tracker.failed(index, &error);
                    return Err(error);
                }
            };
            parse_tracker.stage(index, "prepare");
            let language = detect_language_with(relative, &options.extensions);
            parse_tracker.file_info(index, metadata.len(), language);

            let result = if metadata.len() > options.max_file_size {
                ExtractionResult {
                    nodes: Vec::new(),
                    edges: Vec::new(),
                    unresolved_references: Vec::new(),
                    errors: vec![format!(
                        "File exceeds max size ({} > {}): {relative}",
                        metadata.len(),
                        options.max_file_size
                    )],
                    duration_ms: 0,
                }
            } else {
                extract_source_with_observer(
                    relative,
                    &source,
                    None,
                    &options.extensions,
                    |stage| parse_tracker.extraction_stage(index, stage),
                )
            };
            let file = FileRecord {
                path: relative.clone(),
                content_hash: hash_content(&source),
                language,
                size: metadata.len() as i64,
                modified_at: modified_millis(&metadata),
                indexed_at: now_millis(),
                node_count: result
                    .nodes
                    .iter()
                    .filter(|node| node.file_path == *relative)
                    .count() as i64,
                errors: result.errors.clone(),
                generated: detect_generated_file(relative, &source),
            };
            parse_tracker.parsed(index, &result);
            Ok((relative.clone(), file, result))
        },
        |buffered| buffer_tracker.buffered(buffered),
        |index, (_relative, file, mut result)| {
            if file.errors.is_empty() {
                files_indexed += 1;
            } else if result.nodes.is_empty() {
                files_skipped += 1;
            } else {
                files_errored += 1;
            }

            let persist = (|| -> Result<()> {
                store.upsert_file(&file)?;
                pending_nodes.append(&mut result.nodes);
                if pending_nodes.len() >= NODE_FLUSH_ROWS {
                    store.upsert_nodes(&pending_nodes)?;
                    pending_nodes.clear();
                }
                spill.write_edges(&result.edges)?;
                spill.write_refs(&result.unresolved_references)?;
                Ok(())
            })();
            if let Err(error) = persist {
                persist_tracker.failed(index, &error);
                return Err(error);
            }
            persist_tracker.persisted(index);
            Ok(())
        },
    );
    monitor.stop();
    parse_result?;
    diagnostic_run.phase_end(
        "parse_write",
        parse_started.elapsed(),
        json!({
            "persisted": bar.position(),
            "filesIndexed": files_indexed,
            "filesSkipped": files_skipped,
            "filesErrored": files_errored,
        }),
    );

    let scan_files = bar.position();
    finish_phase(
        &bar,
        &format!("Indexed {} files", format_number(scan_files as i64)),
    );

    diagnostic_run.phase_start("node_write");
    let node_write_started = std::time::Instant::now();
    let pb = phase_spinner("Persisting nodes", quiet);
    if !pending_nodes.is_empty() {
        store.upsert_nodes(&pending_nodes)?;
    }
    drop(pending_nodes);
    finish_phase(&pb, "Persisted nodes");
    diagnostic_run.phase_end("node_write", node_write_started.elapsed(), json!({}));

    // WAL-valve fold threshold (#1231): with bulk autocheckpoint deferred
    // (set_bulk_index_pragmas), fold the WAL back whenever it grows past this
    // size so it never balloons unbounded across the edge/ref replay passes.
    let wal_valve_bytes = codegraph_store::wal_valve_threshold_bytes();
    let mut spill = spill.into_reader()?;
    diagnostic_run.phase_start("edge_write");
    let edge_write_started = std::time::Instant::now();
    let pb = phase_spinner("Persisting edges", quiet);
    spill.replay_edges(EDGE_FLUSH_ROWS, |batch| {
        store.insert_edges(batch).map_err(anyhow::Error::from)?;
        store
            .checkpoint_wal_if_over(wal_valve_bytes)
            .map_err(anyhow::Error::from)?;
        Ok(())
    })?;
    finish_phase(&pb, "Persisted edges");
    diagnostic_run.phase_end("edge_write", edge_write_started.elapsed(), json!({}));
    diagnostic_run.phase_start("reference_write");
    let reference_write_started = std::time::Instant::now();
    let pb = phase_spinner("Persisting references", quiet);
    spill.replay_refs(REF_FLUSH_ROWS, |batch| {
        store
            .insert_unresolved_refs(batch)
            .map_err(anyhow::Error::from)?;
        store
            .checkpoint_wal_if_over(wal_valve_bytes)
            .map_err(anyhow::Error::from)?;
        Ok(())
    })?;
    finish_phase(&pb, "Persisted references");
    diagnostic_run.phase_end(
        "reference_write",
        reference_write_started.elapsed(),
        json!({}),
    );
    spill.cleanup();

    diagnostic_run.phase_start("framework_extract");
    let framework_started = std::time::Instant::now();
    let pb = phase_spinner("Detecting frameworks", quiet);
    let mut resolver = ReferenceResolver::new(project.to_string_lossy());
    // Detect frameworks then run their per-file extract (route/component/handler
    // nodes + refs) BEFORE resolution, mirroring the upstream tree-sitter.ts:4796-4819
    // framework-extraction pass feeding the resolution pipeline.
    {
        let context =
            codegraph_resolve::StoreResolutionContext::new(&store, project.to_string_lossy());
        resolver.initialize(&context);
    }
    if resolver.has_framework_resolvers() {
        let relative_files = store
            .all_files()?
            .into_iter()
            .map(|f| f.path)
            .collect::<Vec<_>>();
        resolver.extract_and_persist_frameworks_with(
            &mut store,
            &relative_files,
            &framework_context,
            &options.extensions,
        )?;
    }
    finish_phase(&pb, "Detected frameworks");
    diagnostic_run.phase_end(
        "framework_extract",
        framework_started.elapsed(),
        json!({ "enabled": resolver.has_framework_resolvers() }),
    );
    // Finished from INSIDE the callback on the final chunk so the retained line
    // lands before the resolver's deferred passes (which resolve refs this bar
    // does not count). The trailing finish covers the no-chunk case where the
    // callback never fires; `done_in_callback` prevents a double finish.
    let resolve_bar = progress_bar(
        0,
        quiet,
        "{spinner:.green} Resolving references [{bar:30}] {pos}/{len} ({elapsed})",
    );
    let mut bar_sized = false;
    let mut done_in_callback = false;
    diagnostic_run.phase_start("reference_resolution");
    let resolution_started = std::time::Instant::now();
    let resolution_sink = diagnostic_run.sink();
    resolver.resolve_and_persist_batched_with_observer(
        &mut store,
        RESOLVE_BATCH_ROWS,
        |processed, total| {
            if !bar_sized {
                resolve_bar.set_length(total);
                bar_sized = true;
            }
            resolve_bar.set_position(processed);
            if processed >= total && !done_in_callback {
                finish_phase(&resolve_bar, "Resolved references");
                done_in_callback = true;
            }
        },
        |observation| {
            let Some(sink) = &resolution_sink else {
                return;
            };
            match observation {
                codegraph_resolve::ResolutionObservation::Setup {
                    node_count,
                    reference_count,
                    streaming,
                    batch_size,
                } => sink.emit(
                    "resolution_setup",
                    json!({
                        "nodes": node_count,
                        "references": reference_count,
                        "mode": if streaming { "streaming" } else { "snapshot" },
                        "batchSize": batch_size,
                    }),
                ),
                codegraph_resolve::ResolutionObservation::BatchComplete {
                    batch,
                    processed,
                    total,
                } => sink.emit(
                    "resolution_batch",
                    json!({
                        "batch": batch,
                        "processed": processed,
                        "total": total,
                    }),
                ),
            }
        },
    )?;
    if !done_in_callback {
        finish_phase(&resolve_bar, "Resolved references");
    }
    diagnostic_run.phase_end(
        "reference_resolution",
        resolution_started.elapsed(),
        json!({ "processed": resolve_bar.position(), "total": resolve_bar.length() }),
    );
    diagnostic_run.phase_start("framework_finalize");
    let framework_finalize_started = std::time::Instant::now();
    let pb = phase_spinner("Finalizing frameworks", quiet);
    // Cross-file framework finalization (NestJS RouterModule prefixing) after
    // resolution, mirroring the upstream index.ts:358 runPostExtract.
    resolver.run_post_extract(&mut store)?;
    finish_phase(&pb, "Finalized frameworks");
    diagnostic_run.phase_end(
        "framework_finalize",
        framework_finalize_started.elapsed(),
        json!({}),
    );
    store.set_project_metadata("indexed_with_version", VERSION)?;
    let after = store.counts()?;
    // Explicit fallible finalization: pragma restore -> checkpoint + compaction ->
    // extraction stamp -> stamp checkpoint -> close the final connection ->
    // publish `phase=current` (and remove a tombstone only for a successful
    // explicit init). The namespace becomes readable at the LAST step, or not at
    // all. Counts are read BEFORE the connection closes.
    diagnostic_run.phase_start("publish");
    let publish_started = std::time::Instant::now();
    let pb = phase_spinner("Publishing index", quiet);
    store.finish()?;
    finish_phase(&pb, "Published index");
    diagnostic_run.phase_end("publish", publish_started.elapsed(), json!({}));
    let summary = IndexSummary {
        files_indexed,
        files_skipped,
        files_errored,
        nodes_created: after.node_count - before.node_count,
        edges_created: after.edge_count - before.edge_count,
        duration_ms: started.elapsed().as_millis() as i64,
    };
    diagnostic_run.finish_success(json!({
        "durationMs": summary.duration_ms,
        "filesIndexed": summary.files_indexed,
        "filesSkipped": summary.files_skipped,
        "filesErrored": summary.files_errored,
        "nodesCreated": summary.nodes_created,
        "edgesCreated": summary.edges_created,
    }));
    Ok(summary)
}

/// Index-keyed reorder buffer for the streaming index consumer: parsed payloads
/// arrive out of order and are drained strictly by ascending index, reproducing
/// the serial sorted-scan persist order regardless of parse-completion timing.
struct ReorderBuffer<T> {
    pending: BTreeMap<usize, T>,
}

impl<T> ReorderBuffer<T> {
    fn new() -> Self {
        Self {
            pending: BTreeMap::new(),
        }
    }

    fn insert(&mut self, index: usize, payload: T) {
        self.pending.insert(index, payload);
    }

    fn take(&mut self, index: usize) -> Option<T> {
        self.pending.remove(&index)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.pending.len()
    }
}

/// Run `parse` in Rayon while consuming results in strict ascending index order.
///
/// Only the caller schedules work: it submits the first `window` tasks, then
/// submits exactly one replacement after one result has been consumed
/// successfully. Workers never wait on the reorder window, so a slow head task
/// cannot strand every Rayon worker behind a condition variable. At all times,
/// running plus completed-but-not-consumed tasks are bounded by `window`.
fn run_ordered_parallel_window<T>(
    count: usize,
    window: usize,
    mut on_schedule: impl FnMut(usize),
    parse: impl Fn(usize) -> Result<T> + Sync,
    mut on_buffered: impl FnMut(usize),
    mut consume: impl FnMut(usize, T) -> Result<()>,
) -> Result<()>
where
    T: Send,
{
    if window == 0 {
        bail!("parallel parse window must be at least one");
    }
    if count == 0 {
        return Ok(());
    }

    // Keep the scheduler/consumer on the CLI thread. Only spawned parse jobs
    // enter the Rayon pool, so even a two-thread pool retains both workers for
    // parsing while the caller waits for ordered results.
    rayon::in_place_scope(|scope| {
        run_ordered_parallel_window_scoped(
            scope,
            count,
            window,
            &mut on_schedule,
            &parse,
            &mut on_buffered,
            &mut consume,
        )
    })
}

fn run_ordered_parallel_window_scoped<'scope, T>(
    scope: &rayon::Scope<'scope>,
    count: usize,
    window: usize,
    on_schedule: &mut impl FnMut(usize),
    parse: &'scope (impl Fn(usize) -> Result<T> + Sync),
    on_buffered: &mut impl FnMut(usize),
    consume: &mut impl FnMut(usize, T) -> Result<()>,
) -> Result<()>
where
    T: Send + 'scope,
{
    let (tx, rx) = mpsc::channel::<(usize, Result<T>)>();
    let mut next_to_schedule = 0usize;
    let mut next_expected = 0usize;
    let mut awaiting_receive = 0usize;
    let mut buffer = ReorderBuffer::new();
    let mut scheduling_stopped = false;
    let mut terminal_error: Option<anyhow::Error> = None;

    let mut schedule = |index: usize| {
        on_schedule(index);
        let tx = tx.clone();
        scope.spawn(move |_| {
            // The receiver lives until every scheduled task has naturally
            // completed, including the error path, so this send cannot block
            // and should only fail if the scheduler itself panics. Convert an
            // extractor panic into an ordered error payload; otherwise the
            // scheduler would wait forever for this task's missing result.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| parse(index)))
                .unwrap_or_else(|_| {
                    Err(anyhow!(
                        "parallel parse worker panicked at file index {index}"
                    ))
                });
            let _ = tx.send((index, result));
        });
    };

    while next_to_schedule < count && awaiting_receive < window {
        schedule(next_to_schedule);
        next_to_schedule += 1;
        awaiting_receive += 1;
    }

    while awaiting_receive > 0 {
        let (index, result) = rx
            .recv()
            .map_err(|_| anyhow!("parallel parse result channel disconnected"))?;
        awaiting_receive -= 1;
        if result.is_err() {
            // Stop growing the work set as soon as ANY worker reports an error.
            // The error itself is still propagated only when its sorted index
            // reaches `next_expected`.
            scheduling_stopped = true;
        }
        buffer.insert(index, result);
        on_buffered(buffer.pending.len());
        debug_assert!(awaiting_receive + buffer.pending.len() <= window);

        while terminal_error.is_none() {
            let Some(result) = buffer.take(next_expected) else {
                break;
            };
            on_buffered(buffer.pending.len());
            match result {
                Ok(payload) => {
                    if let Err(error) = consume(next_expected, payload) {
                        scheduling_stopped = true;
                        terminal_error = Some(error);
                        break;
                    }
                    next_expected += 1;
                    if !scheduling_stopped && next_to_schedule < count {
                        schedule(next_to_schedule);
                        next_to_schedule += 1;
                        awaiting_receive += 1;
                    }
                }
                Err(error) => {
                    scheduling_stopped = true;
                    terminal_error = Some(error);
                }
            }
            debug_assert!(awaiting_receive + buffer.pending.len() <= window);
        }

        if terminal_error.is_some() {
            // Do not cancel or skip already-started parses. Drain one result
            // from each remaining worker so the scoped tasks can finish
            // naturally, then return the deterministic ordered error.
            while awaiting_receive > 0 {
                let (_, result) = rx
                    .recv()
                    .map_err(|_| anyhow!("parallel parse result channel disconnected"))?;
                drop(result);
                awaiting_receive -= 1;
            }
            break;
        }
    }

    if let Some(error) = terminal_error {
        return Err(error);
    }
    if next_expected != count {
        bail!("parallel parse stopped at file {next_expected} of {count} without an ordered error");
    }
    Ok(())
}

/// On-disk spill for extracted edges and unresolved refs during a full index.
///
/// They cannot be persisted during the node pass (all nodes must precede any edge)
/// and would dominate memory, so they are written as newline-delimited JSON in
/// extraction order and streamed back in capped batches, preserving the exact
/// insert order the all-at-once path produced.
struct SpillWriter {
    edges_path: PathBuf,
    refs_path: PathBuf,
    edges: io::BufWriter<fs::File>,
    refs: io::BufWriter<fs::File>,
}

impl SpillWriter {
    fn new(dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&dir)?;
        let edges_path = dir.join(".index-edges.spill");
        let refs_path = dir.join(".index-refs.spill");
        let edges = io::BufWriter::new(
            fs::File::create(&edges_path)
                .with_context(|| format!("creating spill file {}", edges_path.display()))?,
        );
        let refs = io::BufWriter::new(
            fs::File::create(&refs_path)
                .with_context(|| format!("creating spill file {}", refs_path.display()))?,
        );
        Ok(Self {
            edges_path,
            refs_path,
            edges,
            refs,
        })
    }

    fn write_edges(&mut self, edges: &[codegraph_core::types::Edge]) -> Result<()> {
        for edge in edges {
            serde_json::to_writer(&mut self.edges, edge)?;
            self.edges.write_all(b"\n")?;
        }
        Ok(())
    }

    fn write_refs(&mut self, refs: &[codegraph_core::types::UnresolvedRef]) -> Result<()> {
        for reference in refs {
            serde_json::to_writer(&mut self.refs, reference)?;
            self.refs.write_all(b"\n")?;
        }
        Ok(())
    }

    fn into_reader(mut self) -> Result<SpillReader> {
        self.edges.flush()?;
        self.refs.flush()?;
        Ok(SpillReader {
            edges_path: self.edges_path,
            refs_path: self.refs_path,
        })
    }
}

struct SpillReader {
    edges_path: PathBuf,
    refs_path: PathBuf,
}

impl SpillReader {
    fn replay_edges<F>(&mut self, batch_rows: usize, mut flush: F) -> Result<()>
    where
        F: FnMut(&[codegraph_core::types::Edge]) -> Result<()>,
    {
        let reader = BufReader::new(fs::File::open(&self.edges_path)?);
        let mut batch: Vec<codegraph_core::types::Edge> = Vec::with_capacity(batch_rows);
        for line in reader.lines() {
            batch.push(serde_json::from_str(&line?)?);
            if batch.len() >= batch_rows {
                flush(&batch)?;
                batch.clear();
            }
        }
        if !batch.is_empty() {
            flush(&batch)?;
        }
        Ok(())
    }

    fn replay_refs<F>(&mut self, batch_rows: usize, mut flush: F) -> Result<()>
    where
        F: FnMut(&[codegraph_core::types::UnresolvedRef]) -> Result<()>,
    {
        let reader = BufReader::new(fs::File::open(&self.refs_path)?);
        let mut batch: Vec<codegraph_core::types::UnresolvedRef> = Vec::with_capacity(batch_rows);
        for line in reader.lines() {
            batch.push(serde_json::from_str(&line?)?);
            if batch.len() >= batch_rows {
                flush(&batch)?;
                batch.clear();
            }
        }
        if !batch.is_empty() {
            flush(&batch)?;
        }
        Ok(())
    }

    fn cleanup(self) {
        let _ = fs::remove_file(&self.edges_path);
        let _ = fs::remove_file(&self.refs_path);
    }
}

fn print_index_result(result: &IndexSummary) {
    if result.files_indexed > 0 {
        println!("Indexed {} files", format_number(result.files_indexed));
        println!(
            "{} nodes, {} edges in {}",
            format_number(result.nodes_created),
            format_number(result.edges_created),
            format_duration(result.duration_ms)
        );
    } else if result.files_errored > 0 {
        println!(
            "Indexing failed - all {} files had errors",
            result.files_errored
        );
    } else {
        println!("No files found to index");
    }
    if result.files_skipped > 0 {
        println!("Skipped {} files", format_number(result.files_skipped));
    }
}

fn related_nodes_for_symbol(
    store: &Store,
    project: &Path,
    symbol: &str,
    limit: usize,
    related: Related,
    file: Option<&str>,
) -> Result<Vec<NodeSummary>> {
    let matches = symbol_matches(store, project, symbol)?;
    let exact_matches = exact_or_top_matches(&matches, symbol);
    if exact_matches.is_empty() {
        bail!(lookup_symbol_not_found_message(symbol));
    }
    let exact_matches = filter_matches_by_file(exact_matches, symbol, file)?;
    let traverser = GraphTraverser::new(store);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for node in exact_matches {
        let edges = match related {
            Related::Callers => traverser.get_callers(&node.id, 1)?,
            Related::Callees => traverser.get_callees(&node.id, 1)?,
        };
        for entry in edges {
            if seen.insert(entry.node.id.clone()) {
                out.push(NodeSummary::from(&entry.node));
            }
        }
    }
    out.truncate(limit);
    Ok(out)
}

#[derive(Debug, Clone, Copy)]
enum Related {
    Callers,
    Callees,
}

/// Collected Godot honesty signals for the matched symbols of one query: the
/// runtime-reachability reasons (so "no static callers" is never reported as
/// dead) and the symbols' own `godot:dynamic:` computed call-sites. Empty for
/// non-Godot projects, which keeps the caller/impact output byte-unchanged.
#[derive(Debug, Default)]
struct GodotHonestySummary {
    reached_via_scene: bool,
    reached_via_autoload: bool,
    dynamic_unresolved: Vec<String>,
}

impl GodotHonestySummary {
    fn has_signal(&self) -> bool {
        self.reached_via_scene || self.reached_via_autoload || !self.dynamic_unresolved.is_empty()
    }

    fn is_dynamically_reachable(&self) -> bool {
        self.reached_via_scene || self.reached_via_autoload
    }

    fn reachability_sources(&self) -> String {
        let mut parts = Vec::new();
        if self.reached_via_scene {
            parts.push("signal/get_node/group");
        }
        if self.reached_via_autoload {
            parts.push("autoload");
        }
        parts.join("/")
    }

    fn as_json(&self) -> serde_json::Value {
        if !self.has_signal() {
            return serde_json::Value::Null;
        }
        json!({
            "dynamicallyReachable": self.is_dynamically_reachable(),
            "reachedViaScene": self.reached_via_scene,
            "reachedViaAutoload": self.reached_via_autoload,
            "dynamicUnresolved": self.dynamic_unresolved,
        })
    }

    fn print_cli(&self, callers_were_empty: bool) {
        if self.is_dynamically_reachable() && callers_were_empty {
            println!(
                "no static callers - may be reached dynamically (Godot {})",
                self.reachability_sources()
            );
        }
        if !self.dynamic_unresolved.is_empty() {
            println!("\ndynamic / unresolved references (cannot be statically confirmed):");
            for name in &self.dynamic_unresolved {
                println!("  {name}");
            }
        }
    }
}

/// Aggregate the Godot dynamic-reachability signal across the exact/top matches
/// for `symbol`. Returns an all-empty summary for any project without Godot
/// links to those matches — the gate that keeps non-Godot output unchanged.
fn godot_honesty_for_symbol(
    store: &Store,
    project: &Path,
    symbol: &str,
) -> Result<GodotHonestySummary> {
    let matches = symbol_matches(store, project, symbol)?;
    let mut summary = GodotHonestySummary::default();
    let exact_matches = exact_or_top_matches(&matches, symbol);
    if exact_matches.is_empty() {
        bail!(lookup_symbol_not_found_message(symbol));
    }
    let traverser = GraphTraverser::new(store);
    let mut seen = HashSet::new();
    for node in exact_matches {
        let reach = traverser.godot_dynamic_reachability(node)?;
        for r in &reach.reached_by {
            match r {
                GodotReach::SceneOrResourceLink => summary.reached_via_scene = true,
                GodotReach::Autoload => summary.reached_via_autoload = true,
            }
        }
        for name in reach.dynamic_unresolved {
            if seen.insert(name.clone()) {
                summary.dynamic_unresolved.push(name);
            }
        }
    }
    summary.dynamic_unresolved.sort();
    Ok(summary)
}

fn symbol_matches(store: &Store, project: &Path, symbol: &str) -> Result<Vec<Node>> {
    let results = search_nodes(
        store,
        symbol,
        &SearchOptions {
            limit: Some(50),
            ..Default::default()
        },
        &project_name_tokens(project),
    )?;
    let nodes: Vec<Node> = results.into_iter().map(|r| r.node).collect();
    // GDScript `ClassName.member` qualified-name fallback: when the normal
    // search found no node whose exact `name` equals the queried `symbol` and
    // `symbol` is shaped `<Recv>.<member>`, try to resolve the dotted form to
    // the same `Function` node the short name resolves to. GDScript
    // `class_name X` globals are NOT pushed on the extractor's node stack, so
    // a class method stores `name == qualified_name == <member>` and no dotted
    // node exists — this mirrors the committed T2 resolver
    // (`godot::resolve_class_member`). Returns the resolved nodes directly so
    // callers/impact/query all resolve the dotted form to the exact target.
    if nodes.iter().all(|n| n.name != symbol)
        && let Some(mut resolved) = resolve_gdscript_class_member(store, symbol)?
        && !resolved.is_empty()
    {
        for node in &mut resolved {
            node.qualified_name = symbol.to_string();
        }
        return Ok(resolved);
    }
    Ok(nodes)
}

/// Resolve a GDScript `<Recv>.<member>` symbol to the `Function` node(s) named
/// `<member>` in the file(s) that define the GDScript `class_name` global named
/// `<Recv>`. Returns `Ok(None)` when `symbol` is not a single-dotted form, when
/// `<Recv>` names no GDScript `Class` node, or when no matching member function
/// exists (the caller then falls back to the normal search results — no
/// regression). Deterministic: class files are sorted lexicographically and
/// deduped, mirroring the T2 resolver's byte-stable ordering.
fn resolve_gdscript_class_member(store: &Store, symbol: &str) -> Result<Option<Vec<Node>>> {
    let Some((receiver, member)) = symbol.split_once('.') else {
        return Ok(None);
    };
    // Only a single-level `<Recv>.<member>` receiver.member shape; a further
    // '.' means a chained/nested access this fallback does not handle.
    if receiver.is_empty() || member.is_empty() || member.contains('.') {
        return Ok(None);
    }

    // (a) GDScript `Class` nodes named `<Recv>`; collect their files.
    let mut class_files: Vec<String> = store
        .nodes_by_name(receiver)?
        .into_iter()
        .filter(|n| n.kind == NodeKind::Class && n.language == Language::Gdscript)
        .map(|n| n.file_path)
        .collect();
    if class_files.is_empty() {
        return Ok(None);
    }
    class_files.sort();
    class_files.dedup();

    // (b) For each class file (sorted), the `<member>` `Function` node(s) in it.
    let member_nodes = store.nodes_by_name(member)?;
    let mut out: Vec<Node> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for file in &class_files {
        for node in &member_nodes {
            if node.kind == NodeKind::Function
                && node.language == Language::Gdscript
                && &node.file_path == file
                && seen.insert(node.id.clone())
            {
                out.push(node.clone());
            }
        }
    }

    // (c) Return resolved Function nodes, or `None` to fall through.
    if out.is_empty() {
        Ok(None)
    } else {
        Ok(Some(out))
    }
}

fn exact_or_top_matches<'a>(matches: &'a [Node], symbol: &str) -> Vec<&'a Node> {
    matches
        .iter()
        .filter(|node| {
            node.name == symbol
                || node.qualified_name == symbol
                || (node.file_path == symbol && is_godot_resource_target_node(node))
                || node.name.ends_with(&format!(".{symbol}"))
                || node.name.ends_with(&format!("::{symbol}"))
        })
        .collect()
}

/// Narrow same-named definitions to the one declared in `file` (#1512).
///
/// `None` passes every match through, so the unfiltered output of `callers` /
/// `callees` / `impact` is byte-unchanged. A filter is matched against the whole
/// project-relative path OR any trailing path segment boundary, so both
/// `src/deep/mod.ts` and `deep/mod.ts` select the same definition.
///
/// An unmatched filter is an ERROR listing the files that DO define the symbol.
/// Returning an empty relative-set instead would render as "no callers", which
/// reads as "this symbol is dead" — the exact false negative the flag exists to
/// prevent.
fn filter_matches_by_file<'a>(
    matches: Vec<&'a Node>,
    symbol: &str,
    file: Option<&str>,
) -> Result<Vec<&'a Node>> {
    let Some(want) = file else {
        return Ok(matches);
    };
    let want = want.replace('\\', "/");
    let want = want.trim_start_matches("./");
    let kept: Vec<&'a Node> = matches
        .iter()
        .copied()
        .filter(|node| path_matches_file_filter(&node.file_path, want))
        .collect();
    if kept.is_empty() {
        let mut defined_in: Vec<&str> = matches.iter().map(|n| n.file_path.as_str()).collect();
        defined_in.sort_unstable();
        defined_in.dedup();
        bail!(
            "no definition of \"{symbol}\" in \"{want}\"; it is defined in: {}",
            defined_in.join(", ")
        );
    }
    Ok(kept)
}

/// A file filter matches the whole path or a trailing SEGMENT-aligned suffix, so
/// `other.ts` never selects `my_other.ts`.
fn path_matches_file_filter(file_path: &str, want: &str) -> bool {
    let normalized = file_path.replace('\\', "/");
    normalized == want || normalized.ends_with(&format!("/{want}"))
}

/// Render the queried symbol for human output, naming the applied `--file`
/// filter so a narrowed list is never mistaken for the symbol's full set.
fn describe_symbol(symbol: &str, file: Option<&str>) -> String {
    match file {
        Some(file) => format!("{symbol}\" in \"{file}"),
        None => symbol.to_string(),
    }
}

fn lookup_symbol_not_found_message(symbol: &str) -> String {
    format!(
        "Symbol \"{symbol}\" not found. Run `codegraph search {symbol}` to search for fuzzy matches."
    )
}

fn open_store(project: &Path) -> Result<Store> {
    let paths = index_paths(project)?;
    Store::open_for_read(
        &paths,
        std::time::Instant::now() + READ_LEASE_TIMEOUT,
        || false,
    )
    .map_err(Into::into)
}

/// Whether `project` has a current-namespace index DB. An unsafe/aliased
/// `CODEGRAPH_DIR` (a `resolve` failure) counts as NOT initialized here so
/// project discovery keeps walking; the mutating command paths independently
/// re-resolve fail-closed via [`db_path`]/[`codegraph_dir`] before touching disk.
/// Read-command diagnostics classify the resolved namespace separately rather
/// than interpreting every `false` result as a missing index.
fn is_initialized(project: &Path) -> bool {
    let Ok(paths) = index_paths(project) else {
        return false;
    };
    Store::extraction_status(&paths) == codegraph_store::ExtractionStatus::Current
        && paths.current_db().is_file()
}

/// Explicit init's early-return condition is a fully corroborated readable
/// Current namespace, never raw DB existence. Current+tombstone is an expected
/// retryable finalizer residue for explicit init, while every other Current
/// inconsistency remains a typed error.
fn explicit_init_observes_readable_current(project: &Path) -> Result<bool> {
    let paths = index_paths(project)?;
    if Store::extraction_status(&paths) != codegraph_store::ExtractionStatus::Current {
        return Ok(false);
    }
    let deadline = std::time::Instant::now() + REBUILD_LEASE_TIMEOUT;
    match Store::open_for_read(&paths, deadline, || false) {
        Ok(store) => {
            drop(store);
            Ok(true)
        }
        Err(
            codegraph_store::StoreError::CurrentTombstoned { .. }
            | codegraph_store::StoreError::StateWithoutPermanentLock { .. },
        ) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Discovery predicate for the destructive `index` command. A durable state
/// slot keeps an interrupted Building namespace discoverable even if the DB was
/// already deleted. A raw DB remains discoverable only so the under-lease Store
/// gate can reject Missing+DB as corruption; it is never interpreted as Current.
fn has_rebuild_namespace(project: &Path) -> bool {
    let Ok(paths) = index_paths(project) else {
        return false;
    };
    Store::extraction_status(&paths) != codegraph_store::ExtractionStatus::Missing
        || paths.current_db().exists()
}

/// Authenticated lifecycle state is a discovery marker for default and relative
/// roots even when the namespace is not readable. A permanent lock or raw DB by
/// itself is deliberately not a marker: neither authenticates an owner-bound
/// project state.
fn has_lifecycle_namespace(project: &Path) -> bool {
    let Ok(paths) = index_paths(project) else {
        return false;
    };
    Store::extraction_status(&paths) != codegraph_store::ExtractionStatus::Missing
}

/// Resolve a readable project and report lifecycle-specific recovery without
/// authorizing mutation. Only an all-present-slot OwnerMismatch or a Missing
/// existing root with no permanent lock names explicit init; mixed or other
/// corruption continues to require manual recovery.
fn resolve_required_project(path: Option<PathBuf>) -> Result<PathBuf> {
    let start = absolute_path(path.unwrap_or_else(|| PathBuf::from(".")));
    index_paths(&start)?;
    let project = resolve_project_path_optional(&start);
    let paths = index_paths(&project)?;
    match Store::extraction_status(&paths) {
        ExtractionStatus::Current if paths.current_db().is_file() => Ok(project),
        ExtractionStatus::Current => bail!(
            "CodeGraph index state is current in {}, but {} is missing; no CodeGraph CLI command can recover this externally damaged namespace. After confirming no CodeGraph process is using it, run `rm -rf -- \"{}\" && codegraph init \"{}\"`.",
            project.display(),
            paths.current_db().display(),
            paths.current_root().display(),
            project.display()
        ),
        ExtractionStatus::Building { built: _ } => bail!(
            "CodeGraph index build was interrupted in {}; reads remain blocked to avoid false empty results. Run `codegraph index --force {}` to rebuild it (or `codegraph init {}`).",
            project.display(),
            project.display(),
            project.display()
        ),
        ExtractionStatus::Uninitialized => bail!(
            "CodeGraph index removal was interrupted in {}; run `codegraph init {}` to rebuild it",
            project.display(),
            project.display()
        ),
        ExtractionStatus::Missing => {
            if first_existing_database_artifact(&paths)?.is_some() {
                bail!("{}", missing_state_with_database_detail(&project));
            }
            if is_lockless_missing_root(&paths)? {
                bail!("{}", lockless_missing_detail(&project, &paths));
            }
            bail!(
                "CodeGraph not initialized in {}; run `codegraph init {}` to create or replace the index",
                project.display(),
                project.display()
            )
        }
        ExtractionStatus::Outdated { built } => bail!(
            "CodeGraph index in {} is outdated (built with extraction version {built}); run `codegraph index --force {}` to rebuild it",
            project.display(),
            project.display()
        ),
        ExtractionStatus::Future { built } => bail!(
            "CodeGraph index in {} was built by a newer CodeGraph version (extraction version {built}); upgrade CodeGraph before reading it",
            project.display()
        ),
        ExtractionStatus::Corrupt { reason } => {
            if owner_mismatch_only_reason(&paths).is_some() {
                bail!("{}", owner_mismatch_recovery_error(&project, &reason));
            }
            bail!(
                "CodeGraph index state in {} is corrupt: {reason}; run `codegraph status {}` for details; manual recovery is required",
                project.display(),
                project.display()
            )
        }
    }
}

fn first_existing_database_artifact(paths: &codegraph_core::IndexPaths) -> Result<Option<PathBuf>> {
    let db = paths.current_db();
    let mut artifacts = vec![db.clone()];
    for suffix in ["-wal", "-shm"] {
        let mut native = db.as_os_str().to_os_string();
        native.push(suffix);
        artifacts.push(PathBuf::from(native));
    }
    for path in artifacts {
        match fs::symlink_metadata(&path) {
            Ok(_) => return Ok(Some(path)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
            }
        }
    }
    Ok(None)
}

fn missing_state_with_database_detail(project: &Path) -> String {
    format!(
        "index database has no state slots and may have been created by an older version or another tool; run `codegraph init {}` to replace it",
        project.display()
    )
}

/// Detect the recoverable filesystem shape after the caller has classified the
/// namespace Missing. Both path probes are non-following and inspection errors
/// propagate, so aliases and inaccessible entries cannot masquerade as recovery.
fn is_lockless_missing_root(paths: &codegraph_core::IndexPaths) -> Result<bool> {
    let root = paths.current_root();
    let root_metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", root.display()));
        }
    };
    if !root_metadata.file_type().is_dir() {
        return Ok(false);
    }

    let lock = paths.permanent_lock();
    match fs::symlink_metadata(&lock) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", lock.display())),
    }
}

fn lockless_missing_detail(project: &Path, paths: &codegraph_core::IndexPaths) -> String {
    let lock = paths.permanent_lock();
    let root = lock
        .parent()
        .expect("the permanent lock always belongs to the current index root");
    format!(
        "CodeGraph index directory exists at {}, but its permanent lock is missing at {}; a failed background daemon start can leave this stale namespace. Run `codegraph init \"{}\"` to create the lock and rebuild the index.",
        root.display(),
        lock.display(),
        project.display()
    )
}

fn resolve_required_rebuild_project(path: Option<PathBuf>) -> Result<PathBuf> {
    let start = absolute_path(path.unwrap_or_else(|| PathBuf::from(".")));
    index_paths(&start)?;
    if has_rebuild_namespace(&start) {
        return Ok(start);
    }
    let mut current = start.as_path();
    while let Some(parent) = current.parent() {
        if parent == current {
            break;
        }
        if has_rebuild_namespace(parent) {
            warn_ancestor_index_retarget(&start, parent);
            return Ok(parent.to_path_buf());
        }
        current = parent;
    }
    bail!("CodeGraph not initialized in {}", start.display())
}

/// Announce that a MUTATING command is about to operate on an ANCESTOR's index
/// rather than the directory the user named (#1524).
///
/// `index .` inside an unindexed `parent/child` rebuilt the PARENT and printed
/// only `Indexed N files` — a count including the parent's own files — while
/// creating no child index. The retarget is intended behaviour (it is how one
/// index serves a whole tree), but performing it silently is what made the
/// outcome unreadable, and for `uninit --force` it deletes an index the user
/// never named.
///
/// STDERR only: these commands' stdout is machine-readable and pinned by
/// `stdout_purity.rs`. Callers that already resolved the project themselves —
/// [`cmd_unlock`] — call this directly.
fn warn_ancestor_index_retarget(requested: &Path, resolved: &Path) {
    if requested == resolved {
        return;
    }
    eprintln!(
        "Warning: {} has no CodeGraph index, so this command resolved to an ancestor index at {} \
         and will operate on THAT project, not on {}.",
        requested.display(),
        resolved.display(),
        requested.display()
    );
    eprintln!(
        "         Run `codegraph init {}` first if you meant to give it its own index.",
        requested.display()
    );
}

fn resolve_project_path_optional(start: &Path) -> PathBuf {
    if has_lifecycle_namespace(start) {
        return start.to_path_buf();
    }
    let mut current = start;
    while let Some(parent) = current.parent() {
        if parent == current {
            break;
        }
        if has_lifecycle_namespace(parent) {
            return parent.to_path_buf();
        }
        current = parent;
    }
    start.to_path_buf()
}

fn absolute_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    normalize_lexical(&joined)
}

/// Lexically normalize a path WITHOUT touching the filesystem: drop `.`
/// components and fold each `..` into the preceding component. Unlike
/// [`std::fs::canonicalize`] it never reads the disk, never resolves symlinks,
/// and never fails on a nonexistent path — so `serve --http` (which may point at
/// a not-yet-indexed project) logs `<cwd>` and `<cwd>/.codegraph/codegraph.db`
/// with no dangling `/.` segment from a `cwd.join(".")`.
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push(component);
                }
            }
            other => out.push(other),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(Component::CurDir);
    }
    out
}

/// Fail-closed resolution of the project's index paths through the single
/// `codegraph-core::IndexPaths` authority, honoring `CODEGRAPH_DIR`. Errors on
/// an unsafe/aliased/overlapping configured root or an inaccessible project;
/// callers never fall back to a reconstructed path.
fn index_paths(project: &Path) -> Result<codegraph_core::IndexPaths> {
    codegraph_core::IndexPaths::resolve(project, std::env::var("CODEGRAPH_DIR").ok().as_deref())
        .map_err(Into::into)
}

/// The selected project-local index root for `project`. Fail-closed via
/// [`index_paths`].
fn codegraph_dir(project: &Path) -> Result<PathBuf> {
    Ok(index_paths(project)?.current_root().to_path_buf())
}

/// The selected index DB path for `project`. Fail-closed via [`index_paths`].
fn db_path(project: &Path) -> Result<PathBuf> {
    Ok(index_paths(project)?.current_db())
}

fn parse_node_kind(raw: &str) -> Result<NodeKind> {
    NodeKind::ALL
        .into_iter()
        .find(|kind| kind.as_str() == raw)
        .ok_or_else(|| anyhow!("unknown node kind: {raw}"))
}

fn project_name_tokens(project: &Path) -> HashSet<String> {
    project
        .file_name()
        .and_then(|n| n.to_str())
        .into_iter()
        .flat_map(|name| name.split(['-', '_', '.', ' ']))
        .filter(|part| !part.is_empty())
        .map(|part| part.to_lowercase())
        .collect()
}

fn latest_indexed_at(store: &Store) -> Result<Option<i64>> {
    Ok(store.all_files()?.iter().map(|f| f.indexed_at).max())
}

fn journal_mode(store: &Store) -> Result<String> {
    // A state-gated reader executes queries against a private deserialized
    // in-memory image so it cannot create `-wal`/`-shm` sidecars. Its PRAGMA
    // therefore reports `memory`, not the authoritative main database's mode.
    // While the Store retains its shared lease, inspect SQLite's two format
    // bytes instead: 2/2 is the durable WAL marker. Fall back to PRAGMA for
    // legacy/non-WAL stores where the header cannot distinguish every rollback
    // journal variant.
    let mut header = [0_u8; 20];
    fs::File::open(store.path())?.read_exact(&mut header)?;
    if header.starts_with(b"SQLite format 3\0") && header[18] == 2 && header[19] == 2 {
        return Ok("wal".to_string());
    }
    store
        .connection()
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .map_err(Into::into)
}

fn map_counts(entries: Vec<(String, i64)>) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (key, value) in entries {
        map.insert(key, json!(value));
    }
    serde_json::Value::Object(map)
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn modified_millis(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or_else(now_millis)
}

fn iso_like_millis(ms: i64) -> String {
    match OffsetDateTime::from_unix_timestamp_nanos((ms as i128) * 1_000_000) {
        Ok(dt) => dt.format(&Rfc3339).unwrap_or_else(|_| format!("{ms}")),
        Err(_) => format!("{ms}"),
    }
}

fn format_number(n: i64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn format_duration(ms: i64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m {:.0}s", ms / 60_000, (ms % 60_000) as f64 / 1000.0)
    }
}

fn glob_matches(pattern: &str, value: &str) -> bool {
    glob_match_bytes(pattern.as_bytes(), value.as_bytes())
}

fn glob_match_bytes(pattern: &[u8], value: &[u8]) -> bool {
    if pattern.is_empty() {
        return value.is_empty();
    }
    match pattern[0] {
        b'*' => {
            glob_match_bytes(&pattern[1..], value)
                || (!value.is_empty() && glob_match_bytes(pattern, &value[1..]))
        }
        b'?' => {
            !value.is_empty() && value[0] != b'/' && glob_match_bytes(&pattern[1..], &value[1..])
        }
        ch => !value.is_empty() && ch == value[0] && glob_match_bytes(&pattern[1..], &value[1..]),
    }
}

/// An explicit `--filter` glob fully REPLACES the heuristic — a user who names
/// their test files takes precedence over any pattern table.
fn is_test_file(file: &str, filter: Option<&str>) -> bool {
    if let Some(filter) = filter {
        return glob_matches(filter, file);
    }
    codegraph_core::file_class::is_test_file(file)
}

fn print_json(value: &serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

fn print_json_pretty<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchOutput<'a> {
    node: &'a Node,
    score: f64,
}

impl<'a> From<&'a SearchResult> for SearchOutput<'a> {
    fn from(result: &'a SearchResult) -> Self {
        Self {
            node: &result.node,
            score: result.score,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FileOutput<'a> {
    path: &'a str,
    language: Language,
    node_count: i64,
    size: i64,
}

impl<'a> From<&'a FileRecord> for FileOutput<'a> {
    fn from(file: &'a FileRecord) -> Self {
        Self {
            path: &file.path,
            language: file.language,
            node_count: file.node_count,
            size: file.size,
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeSummary {
    name: String,
    kind: NodeKind,
    file_path: String,
    start_line: i64,
}

impl From<&Node> for NodeSummary {
    fn from(node: &Node) -> Self {
        Self {
            name: node.name.clone(),
            kind: node.kind,
            file_path: node.file_path.clone(),
            start_line: node.start_line,
        }
    }
}

fn print_related(label: &str, symbol: &str, nodes: &[NodeSummary]) {
    if nodes.is_empty() {
        println!("No {} found for \"{}\"", label.to_lowercase(), symbol);
        return;
    }
    println!("\n{label} of \"{symbol}\" ({}):\n", nodes.len());
    for node in nodes {
        println!("{:<12}{}", node.kind, node.name);
        println!("  {}:{}\n", node.file_path, node.start_line);
    }
}

fn print_by_file(nodes: &[NodeSummary]) {
    let mut by_file: HashMap<&str, Vec<&NodeSummary>> = HashMap::new();
    for node in nodes {
        by_file.entry(&node.file_path).or_default().push(node);
    }
    let mut files = by_file.keys().copied().collect::<Vec<_>>();
    files.sort_unstable();
    for file in files {
        println!("{file}");
        for node in &by_file[file] {
            println!("  {:<12}{}:{}", node.kind, node.name, node.start_line);
        }
        println!();
    }
}

fn print_files_flat(files: &[FileRecord]) {
    println!("\nFiles ({}):\n", files.len());
    for file in files {
        println!(
            "  {} ({}, {} symbols)",
            file.path, file.language, file.node_count
        );
    }
}

fn print_files_grouped(files: &[FileRecord]) {
    println!("\nFiles by Language ({} total):\n", files.len());
    let mut by_lang: HashMap<Language, Vec<&FileRecord>> = HashMap::new();
    for file in files {
        by_lang.entry(file.language).or_default().push(file);
    }
    let mut groups = by_lang.into_iter().collect::<Vec<_>>();
    groups.sort_by_key(|b| std::cmp::Reverse(b.1.len()));
    for (language, mut group) in groups {
        group.sort_by(|a, b| a.path.cmp(&b.path));
        println!("{} ({}):", language, group.len());
        for file in group {
            println!("  {} ({} symbols)", file.path, file.node_count);
        }
        println!();
    }
}

fn print_files_tree(files: &[FileRecord], max_depth: Option<usize>) {
    println!("\nProject Structure ({} files):\n", files.len());
    for file in files {
        let depth = file.path.matches('/').count() + 1;
        if max_depth.is_none_or(|max| depth <= max) {
            println!(
                "  {} ({}, {} symbols)",
                file.path, file.language, file.node_count
            );
        }
    }
}

#[cfg(test)]
mod self_update_tests {
    use super::{
        latest_update_tag, resolve_github_token, self_update_rate_limit_hint, should_skip_update,
    };
    use std::collections::HashMap;

    fn getter(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    #[test]
    fn formats_bare_semver_as_v_prefixed_tag() {
        assert_eq!(latest_update_tag("0.15.0"), "v0.15.0");
        assert_eq!(latest_update_tag("1.2.3"), "v1.2.3");
    }

    #[test]
    fn idempotent_on_already_v_prefixed_input() {
        assert_eq!(latest_update_tag("v0.15.0"), "v0.15.0");
    }

    #[test]
    fn skips_when_current_equals_latest_and_not_forced() {
        assert!(should_skip_update("0.23.0", "0.23.0", false, false));
    }

    #[test]
    fn force_never_skips() {
        assert!(!should_skip_update("0.23.0", "0.23.0", true, false));
    }

    #[test]
    fn newer_latest_never_skips() {
        assert!(!should_skip_update("0.23.0", "0.24.0", false, false));
    }

    #[test]
    fn explicit_tag_never_skips() {
        assert!(!should_skip_update("0.23.0", "0.23.0", false, true));
    }

    #[test]
    fn github_token_wins_over_gh_token() {
        let get = getter(&[("GITHUB_TOKEN", "primary"), ("GH_TOKEN", "fallback")]);
        assert_eq!(resolve_github_token(get), Some("primary".to_owned()));
    }

    #[test]
    fn gh_token_used_when_github_token_absent() {
        let get = getter(&[("GH_TOKEN", "fallback")]);
        assert_eq!(resolve_github_token(get), Some("fallback".to_owned()));
    }

    #[test]
    fn empty_or_whitespace_token_treated_as_absent() {
        let get = getter(&[("GITHUB_TOKEN", "   "), ("GH_TOKEN", "real")]);
        assert_eq!(resolve_github_token(get), Some("real".to_owned()));

        let empty = getter(&[("GITHUB_TOKEN", ""), ("GH_TOKEN", "")]);
        assert_eq!(resolve_github_token(empty), None);
    }

    #[test]
    fn no_token_set_resolves_to_none() {
        let get = getter(&[]);
        assert_eq!(resolve_github_token(get), None);
    }

    #[test]
    fn token_value_is_trimmed() {
        let get = getter(&[("GITHUB_TOKEN", "  padded\t")]);
        assert_eq!(resolve_github_token(get), Some("padded".to_owned()));
    }

    #[test]
    fn hint_contains_authenticate_command() {
        assert!(
            self_update_rate_limit_hint()
                .contains("GITHUB_TOKEN=$(gh auth token) codegraph self-update")
        );
    }
}

#[cfg(test)]
mod reorder_tests {
    use super::{ReorderBuffer, run_ordered_parallel_window_scoped};
    use std::sync::mpsc;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    fn drain_ready(
        buffer: &mut ReorderBuffer<usize>,
        next_expected: &mut usize,
        out: &mut Vec<usize>,
    ) {
        while let Some(payload) = buffer.take(*next_expected) {
            out.push(payload);
            *next_expected += 1;
        }
    }

    #[test]
    fn shuffled_arrival_drains_in_order() {
        let mut buffer = ReorderBuffer::new();
        let mut next_expected = 0usize;
        let mut out = Vec::new();
        for i in [3usize, 1, 0, 2, 4] {
            buffer.insert(i, i);
            drain_ready(&mut buffer, &mut next_expected, &mut out);
        }
        assert_eq!(out, vec![0, 1, 2, 3, 4]);
        assert_eq!(buffer.len(), 0);
    }

    #[test]
    fn head_arriving_last_holds_then_releases_all() {
        let mut buffer = ReorderBuffer::new();
        let mut next_expected = 0usize;
        let mut out = Vec::new();
        for i in [1usize, 2, 3, 4] {
            buffer.insert(i, i);
            drain_ready(&mut buffer, &mut next_expected, &mut out);
        }
        assert!(out.is_empty(), "nothing drains until index 0 arrives");
        assert_eq!(buffer.len(), 4);
        buffer.insert(0, 0);
        drain_ready(&mut buffer, &mut next_expected, &mut out);
        assert_eq!(out, vec![0, 1, 2, 3, 4]);
        assert_eq!(buffer.len(), 0);
    }

    #[test]
    fn two_workers_blocked_head_never_schedule_beyond_window() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let runner_release = Arc::clone(&release);
        let scheduled = Arc::new(Mutex::new(Vec::new()));
        let runner_scheduled = Arc::clone(&scheduled);
        let consumed = Arc::new(Mutex::new(Vec::new()));
        let runner_consumed = Arc::clone(&consumed);
        let (started_tx, started_rx) = mpsc::channel();

        let handle = std::thread::spawn(move || {
            let parse_release = Arc::clone(&runner_release);
            let parse = move |index: usize| -> anyhow::Result<usize> {
                started_tx.send(index).unwrap();
                if index == 0 {
                    let (lock, wake) = &*parse_release;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = wake.wait(released).unwrap();
                    }
                }
                Ok(index)
            };
            let mut on_schedule = |index| runner_scheduled.lock().unwrap().push(index);
            let mut on_buffered = |_| {};
            let mut consume = |index, value| {
                assert_eq!(index, value);
                runner_consumed.lock().unwrap().push(index);
                Ok(())
            };
            pool.in_place_scope(|scope| {
                run_ordered_parallel_window_scoped(
                    scope,
                    8,
                    4,
                    &mut on_schedule,
                    &parse,
                    &mut on_buffered,
                    &mut consume,
                )
            })
            .unwrap();
        });

        let mut started = Vec::new();
        for _ in 0..4 {
            started.push(started_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        }
        started.sort_unstable();
        assert_eq!(started, vec![0, 1, 2, 3]);
        assert_eq!(
            *scheduled.lock().unwrap(),
            vec![0, 1, 2, 3],
            "no fifth task is submitted while the ordered head is blocked"
        );

        let (lock, wake) = &*release;
        {
            let mut released = lock.lock().unwrap();
            *released = true;
        }
        wake.notify_all();

        handle.join().unwrap();
        assert_eq!(*consumed.lock().unwrap(), (0usize..8).collect::<Vec<_>>());
    }

    #[test]
    fn consumer_error_stops_replenishment_and_joins_started_tasks() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let scheduled = Arc::new(Mutex::new(Vec::new()));
        let scheduled_for_callback = Arc::clone(&scheduled);
        let parse = |index| Ok(index);
        let mut on_schedule = |index| scheduled_for_callback.lock().unwrap().push(index);
        let mut on_buffered = |_| {};
        let mut consume = |_index, _value| anyhow::bail!("injected database write failure");
        let error = pool
            .in_place_scope(|scope| {
                run_ordered_parallel_window_scoped(
                    scope,
                    8,
                    4,
                    &mut on_schedule,
                    &parse,
                    &mut on_buffered,
                    &mut consume,
                )
            })
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("injected database write failure")
        );
        assert_eq!(
            *scheduled.lock().unwrap(),
            vec![0, 1, 2, 3],
            "a consumer failure stops all replenishment"
        );
    }

    #[test]
    fn parse_error_is_propagated_in_index_order_and_stops_replenishment() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let runner_release = Arc::clone(&release);
        let scheduled = Arc::new(Mutex::new(Vec::new()));
        let runner_scheduled = Arc::clone(&scheduled);
        let consumed = Arc::new(Mutex::new(Vec::new()));
        let runner_consumed = Arc::clone(&consumed);
        let (buffered_tx, buffered_rx) = mpsc::channel();

        let handle = std::thread::spawn(move || {
            let parse_release = Arc::clone(&runner_release);
            let parse = move |index: usize| -> anyhow::Result<usize> {
                if index == 2 {
                    anyhow::bail!("injected read failure at index 2");
                }
                if matches!(index, 0 | 3) {
                    let (lock, wake) = &*parse_release;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = wake.wait(released).unwrap();
                    }
                }
                Ok(index)
            };
            let mut on_schedule = |index| runner_scheduled.lock().unwrap().push(index);
            let mut buffered_results = 0usize;
            let mut on_buffered = |_buffered| {
                buffered_results += 1;
                if buffered_results == 2 {
                    buffered_tx.send(()).unwrap();
                }
            };
            let mut consume = |index, _value| {
                runner_consumed.lock().unwrap().push(index);
                Ok(())
            };
            pool.in_place_scope(|scope| {
                run_ordered_parallel_window_scoped(
                    scope,
                    8,
                    4,
                    &mut on_schedule,
                    &parse,
                    &mut on_buffered,
                    &mut consume,
                )
            })
            .unwrap_err()
        });

        buffered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(*scheduled.lock().unwrap(), vec![0, 1, 2, 3]);
        let (lock, wake) = &*release;
        *lock.lock().unwrap() = true;
        wake.notify_all();

        let error = handle.join().unwrap();
        assert!(
            error
                .to_string()
                .contains("injected read failure at index 2")
        );
        assert_eq!(*consumed.lock().unwrap(), vec![0, 1]);
        assert_eq!(
            *scheduled.lock().unwrap(),
            vec![0, 1, 2, 3],
            "the observed parse failure must prevent replenishment"
        );
    }
}

#[cfg(test)]
mod pure_helper_tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "cg-cli-ut-{tag}-{}-{}",
            std::process::id(),
            now_millis()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn node(name: &str) -> Node {
        Node {
            id: format!("function:{name}"),
            kind: NodeKind::Function,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: "a.rs".to_string(),
            language: Language::Rust,
            start_line: 1,
            end_line: 2,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }

    fn affected(from_file: &str, subkind: Option<&str>) -> codegraph_graph::graph::AffectedRef {
        codegraph_graph::graph::AffectedRef {
            from_file: from_file.to_string(),
            line: 1,
            edge_kind: "ext_resource".to_string(),
            target: "res://player.gd".to_string(),
            edge_subkind: subkind.map(str::to_string),
        }
    }

    fn impact(
        changed: &str,
        affected: Vec<codegraph_graph::graph::AffectedRef>,
    ) -> codegraph_graph::graph::ResourceImpact {
        codegraph_graph::graph::ResourceImpact {
            changed: changed.to_string(),
            affected,
        }
    }

    #[test]
    fn location_flag_prefers_explicit_location() {
        assert_eq!(
            location_flag(Some("global".to_string()), true, true),
            Some("global".to_string())
        );
    }

    #[test]
    fn location_flag_maps_global_then_local_then_none() {
        assert_eq!(location_flag(None, true, false), Some("global".to_string()));
        assert_eq!(location_flag(None, false, true), Some("local".to_string()));
        assert_eq!(location_flag(None, false, false), None);
    }

    #[test]
    fn truncate_field_leaves_short_values_unchanged() {
        assert_eq!(truncate_field("abc", 5), "abc");
        assert_eq!(truncate_field("abcde", 5), "abcde");
    }

    #[test]
    fn truncate_field_clips_and_appends_ellipsis() {
        assert_eq!(truncate_field("abcdef", 5), "abcd\u{2026}");
    }

    #[test]
    fn truncate_field_counts_chars_not_bytes() {
        let v = "\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}";
        let out = truncate_field(v, 4);
        assert_eq!(out.chars().count(), 4);
        assert!(out.ends_with('\u{2026}'));
    }

    #[test]
    fn format_started_at_renders_epoch_ms_deterministically() {
        let s = format_started_at(0);
        assert!(
            s.contains("1970-01-01") || s.ends_with("ms"),
            "unexpected rendering: {s}"
        );
    }

    #[test]
    fn res_path_prefixes_res_scheme_and_normalizes_backslashes() {
        assert_eq!(res_path("scenes/main.tscn"), "res://scenes/main.tscn");
        assert_eq!(res_path("a\\b\\c.gd"), "res://a/b/c.gd");
    }

    #[test]
    fn is_godot_resource_path_matches_known_extensions_case_insensitive() {
        assert!(is_godot_resource_path("a.tres"));
        assert!(is_godot_resource_path("a.TSCN"));
        assert!(is_godot_resource_path("a.Res"));
        assert!(is_godot_resource_path("a.gd"));
        assert!(!is_godot_resource_path("a.rs"));
        assert!(!is_godot_resource_path("a.txt"));
    }

    #[test]
    fn audit_prefix_keep_no_filters_keeps_everything() {
        assert!(audit_prefix_keep("src/a.gd", &[], &[]));
    }

    #[test]
    fn audit_prefix_keep_include_requires_a_prefix_match() {
        let include = vec!["src/".to_string()];
        assert!(audit_prefix_keep("src/a.gd", &include, &[]));
        assert!(!audit_prefix_keep("assets/a.gd", &include, &[]));
    }

    #[test]
    fn audit_prefix_keep_exclude_drops_matching_prefix() {
        let exclude = vec!["gen/".to_string()];
        assert!(!audit_prefix_keep("gen/a.gd", &[], &exclude));
        assert!(audit_prefix_keep("src/a.gd", &[], &exclude));
    }

    #[test]
    fn audit_prefix_keep_normalizes_backslashes_on_both_sides() {
        let include = vec!["src\\sub".to_string()];
        assert!(audit_prefix_keep("src\\sub\\a.gd", &include, &[]));
        assert!(audit_prefix_keep("src/sub/a.gd", &include, &[]));
    }

    #[test]
    fn normalize_impact_input_strips_res_scheme() {
        assert_eq!(
            normalize_impact_input("res://scenes/main.tscn", Path::new("/proj")),
            "scenes/main.tscn"
        );
    }

    #[test]
    fn normalize_impact_input_strips_leading_curdir_and_folds_backslashes() {
        assert_eq!(
            normalize_impact_input("./a\\b.gd", Path::new("/proj")),
            "a/b.gd"
        );
        assert_eq!(
            normalize_impact_input(".\\a\\b.gd", Path::new("/proj")),
            "a/b.gd"
        );
    }

    #[test]
    #[cfg(unix)]
    fn normalize_impact_input_makes_absolute_under_project_relative() {
        assert_eq!(
            normalize_impact_input("/proj/scenes/x.tscn", Path::new("/proj")),
            "scenes/x.tscn"
        );
    }

    #[test]
    fn normalize_impact_input_absolute_outside_project_passes_through() {
        assert_eq!(
            normalize_impact_input("/other/x.tscn", Path::new("/proj")),
            "/other/x.tscn"
        );
    }

    #[test]
    fn empty_impact_note_none_when_affected_present() {
        let i = impact("x.gd", vec![affected("a.tscn", None)]);
        assert_eq!(empty_impact_note(&i), None);
    }

    #[test]
    fn empty_impact_note_none_for_non_godot_path() {
        let i = impact("src/lib.rs", Vec::new());
        assert_eq!(empty_impact_note(&i), None);
    }

    #[test]
    fn empty_impact_note_some_for_empty_godot_impact() {
        let i = impact("scenes/main.tscn", Vec::new());
        let note = empty_impact_note(&i).expect("godot path with empty impact yields a note");
        assert!(note.contains("no static references"));
    }

    #[test]
    fn verify_plan_view_categorizes_changed_and_affected_by_extension() {
        let i = impact(
            "player.gd",
            vec![affected("scenes/a.tscn", Some("script")), {
                let mut a = affected("data/b.tres", None);
                a.line = 7;
                a
            }],
        );
        let plan = verify_plan_view(&i);
        assert_eq!(plan.changed, "player.gd");
        assert_eq!(plan.load_scripts, vec!["res://player.gd".to_string()]);
        assert_eq!(plan.open_scenes, vec!["res://scenes/a.tscn".to_string()]);
        assert_eq!(plan.load_resources, vec!["res://data/b.tres".to_string()]);
        assert_eq!(plan.reasons.len(), 2);
        assert_eq!(plan.reasons[0].edge_subkind.as_deref(), Some("script"));
    }

    #[test]
    fn verify_plan_view_dedups_and_sorts_categories() {
        let i = impact(
            "scenes/main.tscn",
            vec![
                affected("z.gd", None),
                affected("a.gd", None),
                affected("a.gd", None),
            ],
        );
        let plan = verify_plan_view(&i);
        assert_eq!(
            plan.load_scripts,
            vec!["res://a.gd".to_string(), "res://z.gd".to_string()]
        );
        assert_eq!(plan.open_scenes, vec!["res://scenes/main.tscn".to_string()]);
        assert_eq!(plan.reasons.len(), 3);
    }

    #[test]
    fn exact_or_top_matches_prefers_exact_name() {
        let matches = vec![node("other"), node("target"), node("more")];
        let picked = exact_or_top_matches(&matches, "target");
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].name, "target");
    }

    #[test]
    fn exact_or_top_matches_matches_dotted_and_colon_suffix() {
        let matches = vec![node("Foo.target"), node("Bar::target")];
        let picked = exact_or_top_matches(&matches, "target");
        assert_eq!(picked.len(), 2);
    }

    fn node_in(name: &str, file: &str) -> Node {
        let mut n = node(name);
        n.file_path = file.to_string();
        n
    }

    #[test]
    fn file_filter_matches_whole_path_and_segment_aligned_suffix() {
        assert!(path_matches_file_filter(
            "src/deep/mod.ts",
            "src/deep/mod.ts"
        ));
        assert!(path_matches_file_filter("src/deep/mod.ts", "deep/mod.ts"));
        assert!(path_matches_file_filter("src/deep/mod.ts", "mod.ts"));
    }

    #[test]
    fn file_filter_rejects_a_suffix_that_splits_a_segment() {
        // `other.ts` must not select `my_other.ts` — a plain `ends_with` would.
        assert!(!path_matches_file_filter("src/my_other.ts", "other.ts"));
        assert!(!path_matches_file_filter("src/deep/mod.ts", "eep/mod.ts"));
    }

    #[test]
    fn filter_matches_by_file_none_passes_everything_through() {
        let alpha = node_in("target", "alpha.ts");
        let beta = node_in("target", "beta.ts");
        let matches = vec![&alpha, &beta];
        let kept = filter_matches_by_file(matches, "target", None).expect("no filter");
        assert_eq!(kept.len(), 2, "unfiltered behaviour must be unchanged");
    }

    #[test]
    fn filter_matches_by_file_selects_one_definition() {
        let alpha = node_in("target", "alpha.ts");
        let beta = node_in("target", "beta.ts");
        let kept =
            filter_matches_by_file(vec![&alpha, &beta], "target", Some("beta.ts")).expect("filter");
        assert_eq!(
            kept.iter()
                .map(|n| n.file_path.as_str())
                .collect::<Vec<_>>(),
            vec!["beta.ts"]
        );
    }

    #[test]
    fn filter_matches_by_file_unmatched_errors_and_lists_the_real_files() {
        let alpha = node_in("target", "alpha.ts");
        let beta = node_in("target", "beta.ts");
        let err = filter_matches_by_file(vec![&alpha, &beta], "target", Some("nope.ts"))
            .expect_err("an unmatched filter must error, not return empty");
        let text = err.to_string();
        assert!(text.contains("nope.ts"), "must name the filter: {text}");
        assert!(
            text.contains("alpha.ts") && text.contains("beta.ts"),
            "must list the defining files: {text}"
        );
    }

    #[test]
    fn filter_matches_by_file_normalizes_windows_separators_and_dot_slash() {
        let deep = node_in("target", "src/deep/mod.ts");
        for want in ["src\\deep\\mod.ts", "./src/deep/mod.ts"] {
            let kept =
                filter_matches_by_file(vec![&deep], "target", Some(want)).expect("normalized");
            assert_eq!(kept.len(), 1, "{want} must select the definition");
        }
    }

    #[test]
    fn describe_symbol_names_the_applied_filter() {
        assert_eq!(describe_symbol("target", None), "target");
        assert_eq!(
            describe_symbol("target", Some("alpha.ts")),
            "target\" in \"alpha.ts"
        );
    }

    #[test]
    fn exact_or_top_matches_refuses_fuzzy_only_matches() {
        let matches = vec![node("alpha"), node("beta")];
        let picked = exact_or_top_matches(&matches, "zzz");
        assert!(picked.is_empty());
    }

    #[test]
    fn exact_or_top_matches_accepts_exact_godot_resource_path() {
        let mut scene = node("Main");
        scene.file_path = "main.tscn".to_string();
        scene.language = Language::GodotScene;
        let matches = vec![scene];
        let picked = exact_or_top_matches(&matches, "main.tscn");
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].file_path, "main.tscn");
    }

    #[test]
    fn exact_or_top_matches_empty_input_yields_empty() {
        let matches: Vec<Node> = Vec::new();
        assert!(exact_or_top_matches(&matches, "x").is_empty());
    }

    #[test]
    fn parse_node_kind_accepts_known_and_rejects_unknown() {
        assert_eq!(parse_node_kind("function").unwrap(), NodeKind::Function);
        assert!(parse_node_kind("not-a-kind").is_err());
    }

    #[test]
    fn glob_matches_literal_star_and_question() {
        assert!(glob_matches("abc", "abc"));
        assert!(!glob_matches("abc", "abd"));
        assert!(glob_matches("a*c", "axxxc"));
        assert!(glob_matches("a?c", "abc"));
        assert!(!glob_matches("a?c", "a/c"));
        assert!(glob_matches("*", "anything"));
        assert!(glob_matches("", ""));
        assert!(!glob_matches("", "x"));
    }

    #[test]
    fn is_test_file_honors_explicit_filter_glob() {
        assert!(is_test_file("src/a.rs", Some("src/*")));
        assert!(!is_test_file("lib/a.rs", Some("src/*")));
    }

    #[test]
    fn is_test_file_default_heuristics() {
        assert!(is_test_file("src/a.test.ts", None));
        assert!(is_test_file("pkg/__tests__/a.js", None));
        assert!(is_test_file("app/tests/mod.rs", None));
        assert!(is_test_file("e2e/flow.spec.ts", None));
        assert!(!is_test_file("src/main.rs", None));
        // Go's test convention is a `_test.` stem, not a `.test.` one (#1507).
        assert!(is_test_file("pkg/math_test.go", None));
        assert!(is_test_file("math_test.go", None));
        // Directory patterns are path-SEGMENT anchored, so a repo-root test
        // directory (no leading slash) classifies like a nested one.
        assert!(is_test_file("e2e/flow.go", None));
        assert!(is_test_file("tests/mod.rs", None));
        assert!(!is_test_file("internal/latest.go", None));
        assert!(!is_test_file("src/route2e2e.ts", None));
    }

    #[test]
    fn project_name_tokens_splits_lowercases_and_dedups() {
        let tokens = project_name_tokens(Path::new("/x/My-Cool_Proj.v2"));
        assert!(tokens.contains("my"));
        assert!(tokens.contains("cool"));
        assert!(tokens.contains("proj"));
        assert!(tokens.contains("v2"));
    }

    #[test]
    fn project_name_tokens_empty_for_root() {
        assert!(project_name_tokens(Path::new("/")).is_empty());
    }

    #[test]
    fn map_counts_builds_json_object() {
        let v = map_counts(vec![("a".to_string(), 1), ("b".to_string(), 2)]);
        assert_eq!(v["a"], serde_json::json!(1));
        assert_eq!(v["b"], serde_json::json!(2));
    }

    #[test]
    fn format_number_inserts_thousands_separators() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(999), "999");
        assert_eq!(format_number(1000), "1,000");
        assert_eq!(format_number(1234567), "1,234,567");
    }

    #[test]
    fn format_duration_scales_units() {
        assert_eq!(format_duration(500), "500ms");
        assert_eq!(format_duration(1500), "1.5s");
        assert_eq!(format_duration(65_000), "1m 5s");
    }

    #[test]
    fn iso_like_millis_renders_rfc3339_for_valid_epoch() {
        assert!(iso_like_millis(0).starts_with("1970-01-01T00:00:00"));
    }

    #[test]
    fn now_millis_is_positive() {
        assert!(now_millis() > 0);
    }

    #[test]
    fn modified_millis_reads_file_mtime() {
        let dir = tmp("mtime");
        let file = dir.join("f.txt");
        fs::write(&file, b"x").unwrap();
        let meta = fs::metadata(&file).unwrap();
        assert!(modified_millis(&meta) > 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn db_path_is_under_codegraph_dir() {
        // Reads CODEGRAPH_DIR, which a sibling test unsets and restores, so it
        // takes the same lock even though it never writes.
        let _env = crate::test_env::env_guard();
        if std::env::var("CODEGRAPH_DIR").is_err() {
            let dir = tmp("dbpath");
            let canonical = dir.canonicalize().unwrap();
            assert_eq!(
                db_path(&dir).unwrap(),
                canonical.join(".codegraph/codegraph.db")
            );
            assert_eq!(codegraph_dir(&dir).unwrap(), canonical.join(".codegraph"));
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn is_initialized_false_for_missing_db() {
        let dir = tmp("init");
        assert!(!is_initialized(&dir));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_required_project_errors_when_uninitialized() {
        let dir = tmp("required");
        let err = resolve_required_project(Some(dir.clone())).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "CodeGraph not initialized in {}; run `codegraph init {}` to create or replace the index",
                dir.display(),
                dir.display()
            )
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_project_path_optional_returns_start_when_uninitialized() {
        let dir = tmp("resolve");
        assert_eq!(resolve_project_path_optional(&dir), dir);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ancestor_retarget_warning_is_a_no_op_when_nothing_was_retargeted() {
        // The common case is `requested == resolved`; warning there would print
        // on every ordinary run. Exercised for its no-panic/no-emit contract —
        // the emitting branch is covered end-to-end by
        // `tests/ancestor_retarget_warning.rs`, which can actually capture
        // stderr.
        let same = PathBuf::from("/proj");
        warn_ancestor_index_retarget(&same, &same);
    }

    #[test]
    fn normalize_lexical_leading_parentdir_is_preserved() {
        assert_eq!(
            normalize_lexical(Path::new("../a/b")),
            PathBuf::from("../a/b")
        );
    }

    #[test]
    fn normalize_lexical_empty_becomes_curdir() {
        assert_eq!(normalize_lexical(Path::new("")), PathBuf::from("."));
        assert_eq!(normalize_lexical(Path::new(".")), PathBuf::from("."));
    }

    #[test]
    fn absolute_path_joins_relative_onto_cwd() {
        let out = absolute_path("some/rel");
        assert!(out.is_absolute());
        assert!(out.ends_with("some/rel"));
    }

    #[test]
    fn should_run_serve_services_true_when_explicit_false_when_bare_unindexed() {
        let dir = tmp("serve-svc");
        assert!(should_run_serve_services(true, &dir));
        assert!(!should_run_serve_services(false, &dir));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn godot_honesty_empty_has_no_signal_and_null_json() {
        let s = GodotHonestySummary::default();
        assert!(!s.has_signal());
        assert!(!s.is_dynamically_reachable());
        assert_eq!(s.reachability_sources(), "");
        assert_eq!(s.as_json(), serde_json::Value::Null);
    }

    #[test]
    fn godot_honesty_scene_and_autoload_sources() {
        let s = GodotHonestySummary {
            reached_via_scene: true,
            reached_via_autoload: true,
            dynamic_unresolved: vec!["call_deferred".to_string()],
        };
        assert!(s.has_signal());
        assert!(s.is_dynamically_reachable());
        assert_eq!(s.reachability_sources(), "signal/get_node/group/autoload");
        let j = s.as_json();
        assert_eq!(j["dynamicallyReachable"], serde_json::json!(true));
        assert_eq!(j["reachedViaScene"], serde_json::json!(true));
        assert_eq!(j["reachedViaAutoload"], serde_json::json!(true));
        assert_eq!(j["dynamicUnresolved"], serde_json::json!(["call_deferred"]));
    }

    #[test]
    fn godot_honesty_only_unresolved_has_signal_but_not_reachable() {
        let s = GodotHonestySummary {
            reached_via_scene: false,
            reached_via_autoload: false,
            dynamic_unresolved: vec!["x".to_string()],
        };
        assert!(s.has_signal());
        assert!(!s.is_dynamically_reachable());
        assert_eq!(s.reachability_sources(), "");
        assert_ne!(s.as_json(), serde_json::Value::Null);
    }

    #[test]
    fn node_summary_from_node_copies_key_fields() {
        let s = NodeSummary::from(&node("frobnicate"));
        assert_eq!(s.name, "frobnicate");
        assert_eq!(s.kind, NodeKind::Function);
        assert_eq!(s.file_path, "a.rs");
        assert_eq!(s.start_line, 1);
    }

    #[test]
    fn generate_completion_bytes_are_non_empty_for_bash() {
        let bytes = generate_completion_bytes(clap_complete::Shell::Bash).unwrap();
        assert!(!bytes.is_empty());
        assert!(String::from_utf8_lossy(&bytes).contains("codegraph"));
    }

    #[test]
    fn env_path_none_for_empty_or_unset_some_for_value() {
        let mut env = crate::test_env::env_guard();
        let key = "CODEGRAPH_TEST_ENV_PATH_UNSET_XYZ";
        env.remove(key);
        assert_eq!(env_path(key), None);
        env.set(key, "");
        assert_eq!(env_path(key), None);
        env.set(key, "/some/where");
        assert_eq!(env_path(key), Some(PathBuf::from("/some/where")));
    }
}

#[cfg(test)]
mod formatter_and_env_tests {
    use super::*;
    use crate::test_env::env_guard;
    use codegraph_core::types::{FileRecord, Language, NodeKind};

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "cg-cli-fmt-{tag}-{}-{}",
            std::process::id(),
            now_millis()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn summary(name: &str, kind: NodeKind, file: &str, line: i64) -> NodeSummary {
        NodeSummary {
            name: name.to_string(),
            kind,
            file_path: file.to_string(),
            start_line: line,
        }
    }

    fn file_record(path: &str, language: Language, node_count: i64) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            content_hash: "hash".to_string(),
            language,
            size: 100,
            modified_at: 0,
            indexed_at: 0,
            node_count,
            errors: Vec::new(),
            generated: false,
        }
    }

    #[test]
    fn codegraph_dir_and_db_path_default_layout() {
        let mut env = env_guard();
        env.remove("CODEGRAPH_DIR");
        let proj = tmp("default-layout");
        let canonical = proj.canonicalize().unwrap();
        assert_eq!(codegraph_dir(&proj).unwrap(), canonical.join(".codegraph"));
        assert_eq!(
            db_path(&proj).unwrap(),
            canonical.join(".codegraph/codegraph.db")
        );
        env.assert_intact();
        let _ = fs::remove_dir_all(&proj);
    }

    #[test]
    fn file_output_from_file_record_copies_fields() {
        let fr = file_record("src/x.rs", Language::Rust, 4);
        let out = FileOutput::from(&fr);
        assert_eq!(out.path, "src/x.rs");
        assert_eq!(out.node_count, 4);
        assert_eq!(out.size, 100);
        let json = serde_json::to_string(&out).unwrap();
        assert!(json.contains("\"nodeCount\":4"));
    }

    #[test]
    fn search_output_from_search_result_serializes_score() {
        let n = Node {
            id: "function:q".to_string(),
            kind: NodeKind::Function,
            name: "q".to_string(),
            qualified_name: "q".to_string(),
            file_path: "a.rs".to_string(),
            language: Language::Rust,
            start_line: 1,
            end_line: 2,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        };
        let sr = SearchResult {
            node: n,
            score: 0.75,
        };
        let out = SearchOutput::from(&sr);
        assert_eq!(out.score, 0.75);
        assert_eq!(out.node.name, "q");
        let json = serde_json::to_string(&out).unwrap();
        assert!(json.contains("\"score\":0.75"));
    }

    #[test]
    fn search_human_result_line_has_no_percentage() {
        // #1045: the raw FTS score is not a percentage; multiplying by 100 emits
        // nonsensical values like "12042%". Results are already best-match-first,
        // so the human-readable line must NOT print any percentage at all.
        let n = query_test_node("myFunc");
        let sr = SearchResult {
            node: n,
            score: 120.42,
        };
        let line = format_search_result_line(&sr);
        assert!(
            !line.contains('%'),
            "human query output must not contain a percentage: {line:?}"
        );
        assert!(
            line.contains(&NodeKind::Function.to_string()),
            "kind must be shown: {line:?}"
        );
        assert!(line.contains("myFunc"), "name must be shown: {line:?}");
    }

    #[test]
    fn query_json_output_still_carries_raw_score() {
        // #1045: the percentage is dropped from the HUMAN output only. The
        // machine-readable --json output keeps the raw score for
        // sorting/thresholding, exactly as upstream does.
        let n = query_test_node("myFunc");
        let sr = SearchResult {
            node: n,
            score: 120.42,
        };
        let out = SearchOutput::from(&sr);
        let json = serde_json::to_string(&out).unwrap();
        assert!(
            json.contains("\"score\":120.42"),
            "json must retain the raw score: {json}"
        );
    }

    fn query_test_node(name: &str) -> Node {
        Node {
            id: format!("function:{name}"),
            kind: NodeKind::Function,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: "src/a.rs".to_string(),
            language: Language::Rust,
            start_line: 42,
            end_line: 43,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }

    #[test]
    fn print_index_result_covers_all_three_branches() {
        print_index_result(&IndexSummary {
            files_indexed: 5,
            files_skipped: 2,
            files_errored: 0,
            nodes_created: 10,
            edges_created: 3,
            duration_ms: 1200,
        });
        print_index_result(&IndexSummary {
            files_indexed: 0,
            files_skipped: 0,
            files_errored: 4,
            nodes_created: 0,
            edges_created: 0,
            duration_ms: 5,
        });
        print_index_result(&IndexSummary {
            files_indexed: 0,
            files_skipped: 0,
            files_errored: 0,
            nodes_created: 0,
            edges_created: 0,
            duration_ms: 5,
        });
    }

    #[test]
    fn print_related_empty_and_nonempty_paths() {
        print_related("Callers", "foo", &[]);
        let nodes = vec![summary("foo", NodeKind::Function, "a.rs", 1)];
        print_related("Callers", "foo", &nodes);
    }

    #[test]
    fn print_by_file_groups_and_sorts() {
        let nodes = vec![
            summary("b", NodeKind::Function, "z.rs", 2),
            summary("a", NodeKind::Function, "a.rs", 1),
        ];
        print_by_file(&nodes);
    }

    #[test]
    fn print_files_flat_grouped_tree_smoke() {
        let files = vec![
            file_record("src/a.rs", Language::Rust, 3),
            file_record("src/sub/b.gd", Language::Gdscript, 1),
        ];
        print_files_flat(&files);
        print_files_grouped(&files);
        print_files_tree(&files, None);
        print_files_tree(&files, Some(1));
    }

    #[test]
    fn print_audit_and_verify_plan_smoke() {
        use codegraph_graph::graph::{AffectedRef, DanglingRef, OrphanResource, ResourceImpact};
        print_audit_orphans(&[]);
        print_audit_orphans(&[OrphanResource {
            file_path: "x.tres".to_string(),
            reason: "unused".to_string(),
            confidence: "high".to_string(),
            note: Some("maybe dynamic".to_string()),
        }]);
        print_audit_dangling(&[]);
        print_audit_dangling(&[DanglingRef {
            from_file: "a.tscn".to_string(),
            target_path: "missing.png".to_string(),
            line: 3,
            kind: "ext_resource".to_string(),
        }]);
        let empty = ResourceImpact {
            changed: "a.gd".to_string(),
            affected: Vec::new(),
        };
        print_audit_impact(&empty);
        let impact = ResourceImpact {
            changed: "a.gd".to_string(),
            affected: vec![
                AffectedRef {
                    from_file: "b.tscn".to_string(),
                    line: 2,
                    edge_kind: "instantiates".to_string(),
                    target: "a.gd".to_string(),
                    edge_subkind: Some("scene_instance".to_string()),
                },
                AffectedRef {
                    from_file: "c.gd".to_string(),
                    line: 4,
                    edge_kind: "calls".to_string(),
                    target: "a.gd".to_string(),
                    edge_subkind: None,
                },
            ],
        };
        print_audit_impact(&impact);
        print_verify_plan(&verify_plan_view(&impact));
    }

    #[test]
    fn godot_honesty_print_cli_smoke() {
        let s = GodotHonestySummary {
            reached_via_scene: true,
            dynamic_unresolved: vec!["emit_signal".to_string()],
            ..Default::default()
        };
        s.print_cli(true);
        s.print_cli(false);
    }

    #[test]
    fn print_json_helpers_emit_valid_json() {
        print_json(&json!({ "a": 1 })).unwrap();
        print_json_pretty(&json!({ "b": [1, 2] })).unwrap();
    }

    #[test]
    fn home_dir_resolves_from_home_then_userprofile_then_errors() {
        let mut env = env_guard();
        env.set("HOME", "/home/tester");
        assert_eq!(home_dir().unwrap(), PathBuf::from("/home/tester"));
        env.remove("HOME");
        env.remove("USERPROFILE");
        assert!(home_dir().is_err());
    }

    #[test]
    fn completion_target_paths_per_shell() {
        let mut env = env_guard();
        env.set("HOME", "/h");
        env.remove("XDG_DATA_HOME");
        env.remove("LOCALAPPDATA");

        assert_eq!(
            completion_target(Shell::Bash).unwrap(),
            PathBuf::from("/h/.local/share/bash-completion/completions/codegraph")
        );
        assert_eq!(
            completion_target(Shell::Zsh).unwrap(),
            PathBuf::from("/h/.zfunc/_codegraph")
        );
        assert_eq!(
            completion_target(Shell::Fish).unwrap(),
            PathBuf::from("/h/.config/fish/completions/codegraph.fish")
        );
        assert_eq!(
            completion_target(Shell::PowerShell).unwrap(),
            PathBuf::from("/h/.local/share/codegraph/completion.ps1")
        );
        assert_eq!(
            completion_target(Shell::Elvish).unwrap(),
            PathBuf::from("/h/.config/codegraph/completion.elv")
        );
        env.set("XDG_DATA_HOME", "/xdg");
        assert_eq!(
            completion_target(Shell::Bash).unwrap(),
            PathBuf::from("/xdg/bash-completion/completions/codegraph")
        );
    }

    #[test]
    fn powershell_profile_path_override_then_userprofile_then_error() {
        let mut env = env_guard();

        env.set("CODEGRAPH_PS_PROFILE", "/custom/profile.ps1");
        assert_eq!(
            powershell_profile_path().unwrap(),
            PathBuf::from("/custom/profile.ps1")
        );
        env.remove("CODEGRAPH_PS_PROFILE");
        env.set("USERPROFILE", "/up");
        assert_eq!(
            powershell_profile_path().unwrap(),
            PathBuf::from("/up/Documents/WindowsPowerShell/Microsoft.PowerShell_profile.ps1")
        );
        env.remove("USERPROFILE");
        env.remove("HOME");
        assert!(powershell_profile_path().is_err());
    }

    #[test]
    fn write_completion_file_creates_parent_and_writes() {
        let dir = tmp("wcf");
        let target = dir.join("nested/deep/codegraph");
        write_completion_file(&target, b"# completion").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "# completion");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_dot_source_once_is_idempotent() {
        let dir = tmp("ads");
        let profile = dir.join("profile.ps1");
        let script = dir.join("completion.ps1");
        assert!(append_dot_source_once(&profile, &script).unwrap());
        assert!(!append_dot_source_once(&profile, &script).unwrap());
        let body = fs::read_to_string(&profile).unwrap();
        let line = format!(". \"{}\"", script.display());
        assert_eq!(body.lines().filter(|l| l.trim() == line).count(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn http_log_path_lands_under_registry_dir() {
        let mut env = env_guard();
        let dir = tmp("httplog");
        env.set("CODEGRAPH_HTTP_REGISTRY_DIR", &dir);
        let p = http_log_path("127.0.0.1:8111");
        assert!(p.starts_with(&dir));
        assert!(p.extension().is_some_and(|e| e == "log"));
        env.assert_intact();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn print_http_table_empty_and_nonempty() {
        use codegraph_daemon::http_registry::{HttpMode, HttpServerInfo};
        print_http_table(&[]);
        print_http_table(&[HttpServerInfo {
            pid: 4242,
            addr: "127.0.0.1:8111".to_string(),
            mode: HttpMode::Global,
            project: Some("/a/very/long/project/path/that/exceeds/the/column/width".to_string()),
            started_at: 1_700_000_000_000,
            version: VERSION.to_string(),
            log_file: Some("/tmp/x.log".to_string()),
        }]);
    }

    #[test]
    fn print_http_conflict_and_note_others_isolated() {
        use codegraph_daemon::http_registry::{HttpMode, HttpServerInfo};
        let info = HttpServerInfo {
            pid: 99,
            addr: "127.0.0.1:9999".to_string(),
            mode: HttpMode::Pinned,
            project: Some("/proj".to_string()),
            started_at: 1_700_000_000_000,
            version: VERSION.to_string(),
            log_file: Some("/tmp/y.log".to_string()),
        };
        print_http_conflict(&info);
        let mut env = env_guard();
        let dir = tmp("noteothers");
        env.set("CODEGRAPH_HTTP_REGISTRY_DIR", &dir);
        note_other_running_servers("127.0.0.1:1234");
        env.assert_intact();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_http_detach_internal_reads_env_marker() {
        let mut env = env_guard();
        let key = codegraph_daemon::CODEGRAPH_HTTP_DETACH_INTERNAL;
        env.remove(key);
        assert!(!is_http_detach_internal());
        env.set(key, "1");
        assert!(is_http_detach_internal());
    }

    #[test]
    fn generate_completion_bytes_nonempty_for_each_shell() {
        for shell in [
            Shell::Bash,
            Shell::Zsh,
            Shell::Fish,
            Shell::PowerShell,
            Shell::Elvish,
        ] {
            let bytes = generate_completion_bytes(shell).unwrap();
            assert!(bytes.len() > 100);
            assert!(String::from_utf8_lossy(&bytes).contains("codegraph"));
        }
    }

    #[test]
    fn install_completions_writes_zsh_fish_elvish_into_home() {
        let mut env = env_guard();
        let dir = tmp("install-comp");
        env.set("HOME", &dir);
        env.remove("XDG_DATA_HOME");

        // Every step re-asserts the guarded env BEFORE writing, so a stolen
        // HOME fails here instead of installing into the real home directory.
        env.assert_intact();
        install_completions(Shell::Zsh).unwrap();
        assert!(dir.join(".zfunc/_codegraph").is_file());

        env.assert_intact();
        install_completions(Shell::Fish).unwrap();
        assert!(
            dir.join(".config/fish/completions/codegraph.fish")
                .is_file()
        );

        env.assert_intact();
        install_completions(Shell::Elvish).unwrap();
        let elv = dir.join(".config/codegraph/completion.elv");
        assert!(elv.is_file());
        assert!(fs::read_to_string(&elv).unwrap().contains("codegraph"));

        env.assert_intact();
        install_completions(Shell::Bash).unwrap();
        assert!(
            dir.join(".local/share/bash-completion/completions/codegraph")
                .is_file()
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_completions_powershell_writes_script_and_dot_sources_profile() {
        let mut env = env_guard();
        let dir = tmp("install-ps");
        let profile = dir.join("profile.ps1");
        env.set("LOCALAPPDATA", &dir);
        env.set("CODEGRAPH_PS_PROFILE", &profile);

        env.assert_intact();
        install_completions(Shell::PowerShell).unwrap();
        let script = dir.join("codegraph/completion.ps1");
        assert!(script.is_file());
        env.assert_intact();
        install_completions(Shell::PowerShell).unwrap();
        let line = format!(". \"{}\"", script.display());
        let body = fs::read_to_string(&profile).unwrap();
        assert_eq!(body.lines().filter(|l| l.trim() == line).count(), 1);

        let _ = fs::remove_dir_all(&dir);
    }
}
