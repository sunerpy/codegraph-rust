# Design: Cross-platform daemon (Windows support) — Plan B

**Status:** Proposed (design only — not yet implemented)
**Author:** sunerpy
**Scope:** Make `codegraph` build and run on Windows by giving the per-project
daemon a cross-platform IPC transport, matching upstream colby (which supports
Windows via named pipes). No change to extraction / golden output.

---

## 1. Problem

`codegraph` does not build on Windows today. The root cause is the
`codegraph-daemon` crate, which uses Unix-only APIs, and the CLI depends on it
unconditionally — so the whole binary fails to compile for
`x86_64-pc-windows-msvc`.

Upstream colby (TypeScript, v1.0.1) **is** cross-platform: its daemon uses
Node's `net` module, which transparently accepts a Unix domain socket path _or_
a Windows named pipe path (`\\.\pipe\codegraph-<hash>`), selected by
`process.platform === 'win32'` (`src/mcp/daemon-paths.ts`). It also ships
`install.ps1` and handles a Windows+WSL shared-tree edge case
(`CODEGRAPH_DIR=.codegraph-win`, upstream #636) and a named-pipe close-event
hazard (upstream #692). This port dropped that cross-platform layer during the
initial Rust implementation; Plan B restores it.

### Unix-only surfaces (audited)

| File                              | Unix-only usage                                                                                                             | Fix                                                       |
| --------------------------------- | --------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------- |
| `codegraph-daemon/src/lib.rs`     | `std::os::unix::net::{UnixListener, UnixStream}`; accept loop; self-connect shutdown poke                                   | Replace transport with `interprocess` local sockets       |
| `codegraph-daemon/src/session.rs` | `serve_session(stream: UnixStream)`, `stream.try_clone()`                                                                   | Generic stream type; `Arc<Stream>` split                  |
| `codegraph-daemon/src/process.rs` | `current_ppid()` parses `/proc/self/stat` (Linux-only, already broken on macOS); `is_process_alive` shells out to `kill -0` | `#[cfg]`-split `rustix` (unix) / `windows-sys` (windows)  |
| `codegraph-daemon/src/lock.rs`    | `fs::hard_link` atomic pidfile create                                                                                       | `OpenOptions::create_new(true)` (atomic on all platforms) |
| `codegraph-daemon/src/paths.rs`   | `.sock` path only                                                                                                           | Add Windows `\\.\pipe\codegraph-<hash>` branch            |

CLI coupling is **minimal and already portable**: `main.rs` only calls
`daemon_pid_path` + `unlock_project` (both filesystem-only).

`codegraph-watch` already uses the cross-platform `notify` crate — no change.

---

## 2. Decisions (Oracle-reviewed)

1. **IPC: adopt the `interprocess` crate (sync `local_socket` API), pinned `2.4`.**
   Mirrors upstream's unified `net` API: one listener/stream type mapping to a
   Unix domain socket on unix and a named pipe on Windows. Pure-Rust, 0BSD
   license, actively maintained, used by nushell. Keeps the codebase **fully
   sync / blocking** — no tokio (Windows named pipes use overlapped I/O inside
   interprocess, so sync streams behave correctly).

2. **Keep the thread-per-connection blocking model** — no async rewrite of
   `serve_session` / `McpServer::run`.

3. **Shutdown: drop the self-connect poke; use nonblocking-accept + poll.**
   interprocess supports nonblocking accept while keeping accepted streams
   blocking. The accept loop polls an `AtomicBool` + watchdog every ~250-500ms;
   no self-connect session to discard. Identical on both platforms.

4. **Process introspection: `#[cfg]`-split `rustix` (unix) + `windows-sys`
   (windows).** Unix: `rustix::process::getppid()` (fixes today's macOS `/proc`
   breakage) + signal-0 liveness. Windows: `OpenProcess` +
   `GetExitCodeProcess == STILL_ACTIVE`. No `sysinfo` (too heavy for two syscalls).

5. **Windows watchdog: host-pid liveness only.** Windows has no stable `getppid`;
   ppid-orphan detection stays unix-only. Existing `host_pid` liveness poll is the
   portable signal.

6. **Lock: `OpenOptions::create_new(true)`** (atomic `O_EXCL`/`CREATE_NEW` on all
   platforms) replaces the `hard_link` dance. Stale-lock semantics unchanged
   (read pid -> liveness check -> compare-and-delete only if dead).

7. **Defer `CODEGRAPH_DIR=.codegraph-win` (WSL coexistence, #636).** Additive env
   override, orthogonal; reserve the name in docs, implement when a real
   WSL-shared-tree user appears. No regression vs today.

---

## 3. Crate structure & `#[cfg]` boundaries

```
crates/codegraph-daemon/src/
  lib.rs        // unchanged public API; uses transport::{Listener, Stream}
  transport.rs  // NEW: interprocess-backed Listener/Stream + name building
  paths.rs      // + windows pipe-name branch (cfg-split helper)
  process.rs    // cfg-split: unix (rustix) / windows (windows-sys) behind shared fns
  lock.rs       // hard_link -> create_new(true)
  session.rs    // serve_session over the generic stream; Arc<Stream> split
```

Public API of `codegraph-daemon` (`start_or_attach`, `run_foreground`,
`attach_to_daemon`, `DaemonHandle`, `daemon_pid_path`, `unlock_project`, ...) stays
**identical** — the CLI is unaffected. Only the internal stream type changes from
`UnixStream` to the interprocess stream.

### Transport sketch (interprocess sync)

```rust
use interprocess::local_socket::{
    prelude::*, GenericFilePath, GenericNamespaced, ListenerOptions,
    ListenerNonblockingMode,
};

fn listener_name(r: &Rendezvous) -> std::io::Result<Name<'_>> {
    #[cfg(windows)] { r.pipe_name.as_str().to_ns_name::<GenericNamespaced>() }
    #[cfg(unix)]    { r.sock_path.as_os_str().to_fs_name::<GenericFilePath>() }
}

fn bind(r: &Rendezvous) -> std::io::Result<Listener> {
    let l = ListenerOptions::new().name(listener_name(r)?).create_sync()?;
    l.set_nonblocking(ListenerNonblockingMode::Accept)?; // accept nonblocking, streams blocking
    Ok(l)
}

// accept loop (replaces the self-connect poke)
loop {
    if shutdown.load(SeqCst) || supervision_lost_reason(..).is_some() { break; }
    match listener.accept() {
        Ok(stream) => { let s = Arc::new(stream); thread::spawn(move || serve_session(s, ..)); }
        Err(e) if e.kind() == WouldBlock => thread::sleep(poll_interval),
        Err(e) => return Err(e).context("accepting daemon connection"),
    }
}
```

`serve_session` splits via `Arc<Stream>` (since `&Stream: Read + Write`),
replacing `UnixStream::try_clone()`:

```rust
let s = Arc::new(stream);
let reader = BufReader::new(Arc::clone(&s));
let writer = Arc::clone(&s);
// write hello line, then McpServer::new(project).run(reader, writer)
```

### Rendezvous (paths.rs)

```rust
pub struct Rendezvous {
    #[cfg(unix)]    pub sock_path: PathBuf, // <proj>/.codegraph/daemon.sock or $TMPDIR fallback (>100 char sun_path)
    #[cfg(windows)] pub pipe_name: String,  // codegraph-<sha256hash16> (namespaced -> \\.\pipe\...)
}
```

`DaemonLockInfo.socket_path` already stores the rendezvous string in the pidfile —
on Windows it stores the pipe name. `attach_to_daemon` reads it back and connects
via the same `listener_name` path. No schema change.

### process.rs split

```rust
pub fn current_ppid() -> u32;             // unix: rustix getppid(); windows: 0 (unsupported)
pub fn is_process_alive(pid: u32) -> bool; // unix: rustix kill(pid,0); windows: OpenProcess+GetExitCodeProcess
```

`supervision_lost_reason` keeps its logic; on Windows `current_ppid()` returns 0
so the ppid branch is inert and only `host_pid` liveness drives shutdown.

---

## 4. Dependencies (daemon crate only — never enters extraction/store)

| crate                | platform                               | purpose                             | license    | guardrail |
| -------------------- | -------------------------------------- | ----------------------------------- | ---------- | --------- |
| `interprocess` `2.4` | all                                    | local socket / named pipe transport | 0BSD       | OK        |
| `rustix`             | `[target.'cfg(unix)'.dependencies]`    | getppid, signal-0 liveness          | Apache/MIT | OK        |
| `windows-sys`        | `[target.'cfg(windows)'.dependencies]` | OpenProcess, GetExitCodeProcess     | MIT/Apache | OK        |

`scripts/guardrail.sh` denylist is surrealdb/rig/qdrant/lancedb/candle/onnx/ort —
none match. Deps confined to `codegraph-daemon`, so golden byte-stability
(extraction/store) is structurally unaffected.

---

## 5. CI & release

- **Compile gate (Linux, every PR):**
  `cargo check -p codegraph-daemon --target x86_64-pc-windows-msvc` (+ `-p codegraph-rs`)
  to catch `#[cfg]` mistakes without a Windows host.
- **Runtime gate (new `windows-latest` CI job):** daemon integration tests on
  Windows — start, connect, MCP round-trip, stop, lock contention, stale-lock
  recovery, peer-disconnect-unblocks-session (#692 family).
- **Release matrix:** add `x86_64-pc-windows-msvc` (`os: windows-latest`,
  `archive: zip`, native `cargo build` — no zigbuild). Packaging gains a `zip`
  arm producing `codegraph-<version>-x86_64-pc-windows-msvc.zip`.
- README install matrix + `self-update` docs gain the Windows target.

Verifiable only on the Windows runner (not the Linux dev env): named-pipe
accept/connect, blocking session reads unblocking on disconnect, `create_new` lock
under contention, file-deletion-while-handle-open semantics.

---

## 6. Phased rollout

- **Phase 1 — transport refactor, zero behavior change (still Unix-only).**
  Add `transport.rs` + `Listener`/`Stream` over `interprocess`; switch
  `lib.rs`/`session.rs`; switch lock to `create_new(true)`; split `process.rs`.
  Prove **identical** Unix behavior (`make ci` green, daemon tests unchanged).
- **Phase 2 — Windows impl.** Add `#[cfg(windows)]` pipe-name branch +
  `windows-sys` process impl; cross-compile `--target x86_64-pc-windows-msvc`.
- **Phase 3 — CI + release.** Add windows-latest CI job + compile gate; add the
  release-matrix entry + `.zip` packaging; update docs. Cut a release; verify the
  Windows `.zip`.

Each phase is a separate PR. Phase 1 carries the _Unix_ regression risk (it
touches accept-shutdown, lock-create, process introspection) and must prove no
regression before Phase 2.

---

## 7. Risk & blast radius

- **Golden byte-stability: SAFE.** Daemon keeps the store warm; touches no
  extraction/resolution/output. New deps are daemon-crate-only.
- **Unix regression risk** concentrates in three changed primitives
  (accept-shutdown poke -> poll; `hard_link` -> `create_new`; `/proc` -> `rustix`).
  Each needs a Unix regression test proving identical observable behavior.
- **Windows #692:** session reads may not unblock on peer disconnect on named
  pipes. Mitigation: port upstream's idle/liveness session sweep if the
  windows-latest job surfaces it.
- **Shutdown latency:** poll-based shutdown adds up-to-`poll_interval` latency vs
  the instant poke. Acceptable for a keep-warm daemon; tune to ~250ms.

---

## 8. Out of scope (reserved follow-ups)

- `CODEGRAPH_DIR=.codegraph-win` WSL-coexistence override (#636) — name reserved.
- Proactive idle/liveness session sweep (#692) — add only if the Windows runner
  shows the disconnect hazard.
- Windows `install.ps1`-style installer — `cargo install --git` + prebuilt `.zip`
  already cover install; PowerShell installer is optional polish.
