//! Batch M item 19 — PROJECT-SCOPED v2 configuration, end to end.
//!
//! One process must never let one project's configuration reach another, and must
//! never adopt a legacy `.codegraph/config.toml` / `.codegraph/codegraph.json`.
//! These targets drive the REAL `codegraph` binary:
//!
//! 1. [`global_http_uses_project_scoped_v2_configs`] — the named acceptance
//!    target. ONE global `serve --http` process (no `--path`, `APP_CONFIG`
//!    unset) answers requests for two projects whose current-root `config.toml`
//!    files carry OPPOSING `include`/`exclude`/`max_file_size` settings, plus
//!    hostile LEGACY configs that must be ignored. It proves per-request scoping
//!    in both orders, then proves the same for `sync` and for the live watcher.
//! 2. [`app_config_overrides_both_projects_including_codegraph_dir_collision`] —
//!    the control: `APP_CONFIG` is INTENTIONALLY process-wide, so it supersedes
//!    both projects' own configs; and with both projects pointed at the SAME
//!    absolute `CODEGRAPH_DIR`, their identity-suffixed current roots stay
//!    distinct, so neither project's index or config collides with the other's.
#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use codegraph_core::IndexPaths;
use codegraph_store::Store;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("codegraph-cli is under crates/")
        .to_path_buf()
}

fn mini_fixture() -> PathBuf {
    workspace_root().join("crates/codegraph-bench/fixtures/mini")
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-m19-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            fs::copy(&from, &to).unwrap();
        }
    }
}

/// A `mini`-fixture project plus a `.gitignore`d `Tools/` dir, and HOSTILE legacy
/// configs (`.codegraph/config.toml` + `.codegraph/codegraph.json`) that no
/// production path may read. The legacy TOML asks for the OPPOSITE of every
/// project-scoped expectation, so adopting it fails the assertions below.
fn project_with_hostile_legacy_config(root: &Path, legacy_include: &str) -> PathBuf {
    copy_tree(&mini_fixture(), root);
    fs::write(root.join(".gitignore"), "Tools/\n").unwrap();
    fs::create_dir_all(root.join("Tools")).unwrap();
    fs::write(
        root.join("Tools/helper.ts"),
        "export function toolsHelper() { return 1; }\n",
    )
    .unwrap();
    fs::create_dir_all(root.join(".codegraph")).unwrap();
    fs::write(
        root.join(".codegraph/config.toml"),
        format!(
            "[app]\nname = \"legacy\"\n\n[indexing]\nmax_file_size = 7\ninclude = [{legacy_include}]\n"
        ),
    )
    .unwrap();
    fs::write(
        root.join(".codegraph/codegraph.json"),
        "{\"extensions\":{\".zz\":\"lua\"}}\n",
    )
    .unwrap();
    root.to_path_buf()
}

/// Write `contents` to the project's CURRENT-ROOT `config.toml` — the only
/// project config production reads. Call AFTER `init`, so the current root is the
/// initialized namespace rather than a hand-made directory.
fn write_current_config(project: &Path, contents: &str, codegraph_dir: Option<&str>) {
    let paths = IndexPaths::resolve(project, codegraph_dir).expect("resolve index paths");
    assert!(
        paths.current_root().is_dir(),
        "write the project config after `init` created {}",
        paths.current_root().display()
    );
    fs::write(paths.config_toml(), contents).unwrap();
}

/// Run the binary with foreground-only env, returning (stdout, stderr, ok).
fn cli(args: &[&str], envs: &[(&str, &str)]) -> (String, String, bool) {
    let mut cmd = Command::new(bin());
    cmd.args(args);
    cmd.env("CODEGRAPH_NO_DAEMON", "1");
    cmd.env("CODEGRAPH_NO_WATCH", "1");
    cmd.env_remove("APP_CONFIG");
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let output = cmd.output().expect("run codegraph binary");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

/// The indexed root-relative file paths recorded in the project's v2 database.
fn indexed_files(project: &Path, codegraph_dir: Option<&str>) -> Vec<String> {
    let paths = IndexPaths::resolve(project, codegraph_dir).expect("resolve index paths");
    let store = Store::open(&paths.current_db()).expect("open v2 store");
    let mut files = store
        .all_files()
        .expect("read files")
        .into_iter()
        .map(|file| file.path)
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn http_post_mcp(addr: &str, body: &str) -> std::io::Result<String> {
    let sockaddr = addr.to_socket_addrs()?.next().expect("resolve addr");
    let mut stream = TcpStream::connect_timeout(&sockaddr, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let req = format!(
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes())?;
    stream.flush()?;
    let mut buf = String::new();
    let _ = stream.read_to_string(&mut buf);
    Ok(buf)
}

fn wait_reachable(addr: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"m19","version":"0"}}}"#;
    while Instant::now() < deadline {
        if let Ok(resp) = http_post_mcp(addr, init)
            && resp.contains("\"result\"")
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// One `tools/call` against the GLOBAL HTTP server, addressed by `projectPath`.
fn http_tool_call(addr: &str, id: u64, tool: &str, arguments: &str) -> String {
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"{tool}","arguments":{arguments}}}}}"#
    );
    http_post_mcp(addr, &body).expect("HTTP tool call")
}

/// A child process killed + reaped on drop, so no server leaks out of a test.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A live `serve --mcp` child driven over line-delimited JSON-RPC.
struct ServeProcess {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
}

impl ServeProcess {
    fn spawn(project: &Path) -> Self {
        let mut child = Command::new(bin())
            .arg("serve")
            .arg("--mcp")
            .arg("--path")
            .arg(project)
            .env("CODEGRAPH_NO_DAEMON", "1")
            // The documented env escape hatch still wins over config, keeping the
            // watcher reaction fast and deterministic.
            .env("CODEGRAPH_WATCH_DEBOUNCE_MS", "100")
            .env_remove("CODEGRAPH_NO_WATCH")
            .env_remove("APP_CONFIG")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn serve --mcp");
        let stdin = child.stdin.take().expect("serve stdin");
        let stdout = child.stdout.take().expect("serve stdout");
        Self {
            child,
            stdin,
            reader: BufReader::new(stdout),
        }
    }

    fn send(&mut self, line: &str) {
        self.stdin
            .write_all(line.as_bytes())
            .expect("write request");
        self.stdin.write_all(b"\n").expect("write newline");
        self.stdin.flush().expect("flush request");
    }

    fn read_line(&mut self) -> Option<String> {
        let mut buf = String::new();
        match self.reader.read_line(&mut buf) {
            Ok(0) => None,
            Ok(_) => Some(buf),
            Err(_) => None,
        }
    }

    fn handshake(&mut self) {
        self.send(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"m19","version":"0"}}}"#,
        );
        self.read_line().expect("initialize response");
    }

    /// Poll `codegraph_search` for `symbol`, returning whether it was FOUND
    /// within the budget. The not-found response echoes the query, so the
    /// explicit sentinel decides presence.
    fn search_finds_within(&mut self, symbol: &str, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        let mut id = 100;
        loop {
            id += 1;
            self.send(&format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"codegraph_search","arguments":{{"query":"{symbol}"}}}}}}"#
            ));
            let found = match self.read_line() {
                Some(line) => line.contains(symbol) && !line.contains("No results found"),
                None => false,
            };
            if found {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

impl Drop for ServeProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// ALPHA force-indexes its gitignored `Tools/`, drops the `tools/` python dir,
/// and caps `max_file_size` far below the fixture's TypeScript sources.
const ALPHA_CONFIG: &str = "[app]\nname = \"alpha\"\n\n[indexing]\nmax_file_size = 120\ninclude = [\"Tools/\"]\nexclude = [\"tools/\"]\n";
/// BETA is the mirror image: no `include` (so `Tools/` stays gitignored), keeps
/// `tools/`, drops `src/math.ts`, and leaves `max_file_size` at the default.
const BETA_CONFIG: &str = "[app]\nname = \"beta\"\n\n[indexing]\nexclude = [\"src/math.ts\"]\n";

/// Item 19. ONE global HTTP process, `APP_CONFIG` unset, two projects with
/// opposing current-root configs — plus `sync` and the live watcher.
#[test]
fn global_http_uses_project_scoped_v2_configs() {
    let home = TestDir::new("global-http");
    let alpha = project_with_hostile_legacy_config(&home.path().join("alpha"), "\"tools/\"");
    let beta = project_with_hostile_legacy_config(&home.path().join("beta"), "\"Tools/\"");

    // -------------------------------------------------- index and sync ---
    // `init` publishes each project's v2 namespace; the per-project config then
    // lands in that namespace. `index --force` must scope each project's SCAN by
    // its own config, and a subsequent `sync` must scope the same way. The hostile
    // legacy `.codegraph/config.toml` in each project asks for the other's
    // `include` and a 7-byte size cap, so adopting it would flip the assertions.
    let (out, err, ok) = cli(&["init", alpha.to_str().unwrap()], &[]);
    assert!(ok, "alpha init failed: stdout={out} stderr={err}");
    let (out, err, ok) = cli(&["init", beta.to_str().unwrap()], &[]);
    assert!(ok, "beta init failed: stdout={out} stderr={err}");
    write_current_config(&alpha, ALPHA_CONFIG, None);
    write_current_config(&beta, BETA_CONFIG, None);
    let (out, err, ok) = cli(&["index", "--force", alpha.to_str().unwrap()], &[]);
    assert!(ok, "alpha index failed: stdout={out} stderr={err}");
    let (out, err, ok) = cli(&["index", "--force", beta.to_str().unwrap()], &[]);
    assert!(ok, "beta index failed: stdout={out} stderr={err}");

    let alpha_files = indexed_files(&alpha, None);
    let beta_files = indexed_files(&beta, None);
    assert!(
        alpha_files.contains(&"Tools/helper.ts".to_string()),
        "alpha's own include must force its gitignored Tools/ in: {alpha_files:?}"
    );
    assert!(
        !alpha_files.iter().any(|path| path.starts_with("tools/")),
        "alpha's own exclude must drop tools/: {alpha_files:?}"
    );
    assert!(
        !beta_files.contains(&"Tools/helper.ts".to_string()),
        "beta must not inherit alpha's include: {beta_files:?}"
    );
    assert!(
        beta_files.contains(&"tools/greeter.py".to_string()),
        "beta must not inherit alpha's exclude: {beta_files:?}"
    );
    assert!(
        !beta_files.contains(&"src/math.ts".to_string()),
        "beta's own exclude must drop src/math.ts: {beta_files:?}"
    );
    assert!(
        alpha_files.contains(&"src/math.ts".to_string()),
        "alpha must not inherit beta's exclude: {alpha_files:?}"
    );

    // The SYNC path is scoped the same way: an identical new file under the
    // gitignored `Tools/` is picked up by alpha (whose config includes it) and
    // ignored by beta (whose config does not, its legacy config notwithstanding).
    for project in [&alpha, &beta] {
        fs::write(
            project.join("Tools/synced.ts"),
            "export function syncedMarker() { return 1; }\n",
        )
        .unwrap();
    }
    let (out, err, ok) = cli(&["sync", alpha.to_str().unwrap()], &[]);
    assert!(ok, "alpha sync failed: stdout={out} stderr={err}");
    let (out, err, ok) = cli(&["sync", beta.to_str().unwrap()], &[]);
    assert!(ok, "beta sync failed: stdout={out} stderr={err}");
    assert!(
        indexed_files(&alpha, None).contains(&"Tools/synced.ts".to_string()),
        "alpha's sync must honor alpha's include"
    );
    assert!(
        !indexed_files(&beta, None).contains(&"Tools/synced.ts".to_string()),
        "beta's sync must not adopt alpha's (or its legacy config's) include"
    );

    // --------------------------------------------------- one HTTP process ---
    // GLOBAL mode: no `--path`, so each tool call carries its own projectPath and
    // ONE process answers for both projects.
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let registry = home.path().join("http-registry");
    let child = Command::new(bin())
        .args(["serve", "--http", "--http-addr", &addr])
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .env("CODEGRAPH_HTTP_REGISTRY_DIR", &registry)
        .env_remove("APP_CONFIG")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn global serve --http");
    let _guard = ChildGuard(child);
    assert!(
        wait_reachable(&addr),
        "the global HTTP MCP server never became reachable on {addr}"
    );

    // Per-project `max_file_size`: alpha's 120-byte cap refuses to serve
    // `src/app.ts` (185 bytes) as source, while beta's default limit renders it.
    // Requested in BOTH orders so no ordering or reuse can hide a bleed.
    let alpha_node = http_tool_call(
        &addr,
        11,
        "codegraph_node",
        &format!(
            r#"{{"projectPath":"{}","file":"src/app.ts"}}"#,
            alpha.display()
        ),
    );
    let beta_node = http_tool_call(
        &addr,
        12,
        "codegraph_node",
        &format!(
            r#"{{"projectPath":"{}","file":"src/app.ts"}}"#,
            beta.display()
        ),
    );
    let alpha_again = http_tool_call(
        &addr,
        13,
        "codegraph_node",
        &format!(
            r#"{{"projectPath":"{}","file":"src/app.ts"}}"#,
            alpha.display()
        ),
    );
    assert!(
        alpha_node.contains("could not read from disk"),
        "alpha's own 120-byte max_file_size must refuse src/app.ts: {alpha_node}"
    );
    assert!(
        !beta_node.contains("could not read from disk"),
        "beta must not inherit alpha's max_file_size: {beta_node}"
    );
    assert!(
        beta_node.contains("export"),
        "beta must serve src/app.ts source: {beta_node}"
    );
    assert!(
        alpha_again.contains("could not read from disk"),
        "alpha must keep its own limit after beta was served: {alpha_again}"
    );

    // The two projects' file sets stay their own inside the SAME process.
    let alpha_listing = http_tool_call(
        &addr,
        21,
        "codegraph_files",
        &format!(r#"{{"projectPath":"{}"}}"#, alpha.display()),
    );
    let beta_listing = http_tool_call(
        &addr,
        22,
        "codegraph_files",
        &format!(r#"{{"projectPath":"{}"}}"#, beta.display()),
    );
    assert!(
        alpha_listing.contains("helper.ts") && !alpha_listing.contains("greeter.py"),
        "alpha's listing must reflect alpha's config: {alpha_listing}"
    );
    assert!(
        beta_listing.contains("greeter.py") && !beta_listing.contains("helper.ts"),
        "beta's listing must reflect beta's config: {beta_listing}"
    );

    // ------------------------------------------------------------- watcher ---
    // The live watcher scopes by the ADDRESSED project's config: a new file under
    // the gitignored `Tools/` is auto-synced for alpha (which includes it) and
    // never for beta (which does not) — even though beta's hostile LEGACY config
    // names `Tools/`.
    let mut alpha_serve = ServeProcess::spawn(&alpha);
    alpha_serve.handshake();
    fs::write(
        alpha.join("Tools/watched.ts"),
        "export function alphaWatchedMarker() { return 1; }\n",
    )
    .unwrap();
    assert!(
        alpha_serve.search_finds_within("alphaWatchedMarker", Duration::from_secs(20)),
        "alpha's watcher must auto-sync a file its own include covers"
    );
    drop(alpha_serve);

    let mut beta_serve = ServeProcess::spawn(&beta);
    beta_serve.handshake();
    fs::write(
        beta.join("Tools/watched.ts"),
        "export function betaWatchedMarker() { return 1; }\n",
    )
    .unwrap();
    assert!(
        !beta_serve.search_finds_within("betaWatchedMarker", Duration::from_secs(6)),
        "beta's watcher must not adopt alpha's (or its legacy config's) include"
    );
    drop(beta_serve);
}

/// The control: `APP_CONFIG` is INTENTIONALLY a process-wide override, so it
/// supersedes both projects' own current-root configs. The same run also points
/// both projects at ONE absolute `CODEGRAPH_DIR`: their identity-suffixed current
/// roots must stay distinct, so the collision attempt cannot merge two projects'
/// storage or configuration.
#[test]
fn app_config_overrides_both_projects_including_codegraph_dir_collision() {
    let home = TestDir::new("app-config");
    let shared_dir = home.path().join("shared-index");
    let shared = shared_dir.to_string_lossy().into_owned();
    let alpha = project_with_hostile_legacy_config(&home.path().join("alpha"), "\"tools/\"");
    let beta = project_with_hostile_legacy_config(&home.path().join("beta"), "\"Tools/\"");

    let override_path = home.path().join("process-wide.toml");
    fs::write(
        &override_path,
        "[app]\nname = \"process-wide\"\n\n[indexing]\ninclude = [\"Tools/\"]\n",
    )
    .unwrap();
    let override_arg = override_path.to_string_lossy().into_owned();

    // The identity-suffixed sibling roots must differ even though CODEGRAPH_DIR is
    // literally the same absolute path for both projects.
    let alpha_paths = IndexPaths::resolve(&alpha, Some(&shared)).expect("alpha paths");
    let beta_paths = IndexPaths::resolve(&beta, Some(&shared)).expect("beta paths");
    assert_ne!(
        alpha_paths.current_root(),
        beta_paths.current_root(),
        "a shared absolute CODEGRAPH_DIR must still yield per-project current roots"
    );
    assert_ne!(alpha_paths.current_db(), beta_paths.current_db());
    assert_ne!(alpha_paths.config_toml(), beta_paths.config_toml());

    // Initialize both namespaces first (so each has a published current root),
    // write each project's OWN config — which forbids the other's scope and, for
    // alpha, `Tools/` too — then reindex under the process-wide APP_CONFIG.
    for project in [&alpha, &beta] {
        let (out, err, ok) = cli(
            &["init", project.to_str().unwrap()],
            &[("CODEGRAPH_DIR", shared.as_str())],
        );
        assert!(
            ok,
            "init failed for {}: stdout={out} stderr={err}",
            project.display()
        );
    }
    write_current_config(&alpha, ALPHA_CONFIG, Some(&shared));
    write_current_config(&beta, BETA_CONFIG, Some(&shared));
    for project in [&alpha, &beta] {
        let (out, err, ok) = cli(
            &["index", "--force", project.to_str().unwrap()],
            &[
                ("CODEGRAPH_DIR", shared.as_str()),
                ("APP_CONFIG", override_arg.as_str()),
            ],
        );
        assert!(
            ok,
            "index under APP_CONFIG failed for {}: stdout={out} stderr={err}",
            project.display()
        );
    }

    // APP_CONFIG won for BOTH: each index carries `Tools/helper.ts`, which
    // alpha's own config allows but beta's forbids.
    for (label, project) in [("alpha", &alpha), ("beta", &beta)] {
        let files = indexed_files(project, Some(&shared));
        assert!(
            files.contains(&"Tools/helper.ts".to_string()),
            "APP_CONFIG must override {label}'s own config: {files:?}"
        );
        // Storage stayed separate: each project's DB holds only its own tree.
        assert!(
            files.contains(&"src/app.ts".to_string()),
            "{label}'s index must hold its own sources: {files:?}"
        );
    }
    assert!(
        alpha_paths.current_root().is_dir() && beta_paths.current_root().is_dir(),
        "both identity-suffixed roots must exist side by side under the shared CODEGRAPH_DIR"
    );

    // And with APP_CONFIG UNSET the same two projects fall back to their own
    // configs again — the override is process-wide, not sticky state on disk.
    let (out, err, ok) = cli(
        &["index", "--force", beta.to_str().unwrap()],
        &[("CODEGRAPH_DIR", shared.as_str())],
    );
    assert!(ok, "beta reindex failed: stdout={out} stderr={err}");
    let beta_files = indexed_files(&beta, Some(&shared));
    assert!(
        !beta_files.contains(&"Tools/helper.ts".to_string()),
        "without APP_CONFIG beta must use its own config again: {beta_files:?}"
    );
}
