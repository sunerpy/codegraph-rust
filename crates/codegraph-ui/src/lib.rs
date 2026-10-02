//! `codegraph ui` — the local browser viewer: a loopback HTTP server that answers
//! the viewer's JSON API from the project's index and serves the embedded web app.
//!
//! A port of upstream colbymchenry/codegraph `src/ui-server/**` (`v1.6.1`). The
//! viewer is a pure reader of an index that already exists: it never creates,
//! migrates or syncs one. Its one write is a saved trail under the index root
//! (`<IndexPaths::current_root()>/ui/trails/`), refused under `--read-only`.

pub mod api;
pub mod assets;
pub mod browser;
pub mod caches;
pub mod events;
pub mod highlight;
pub mod respond;
pub mod security;
pub mod server;
pub mod session;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};

pub use server::{DEFAULT_PORT_ATTEMPTS, DEFAULT_UI_PORT};

/// How the viewer is started.
#[derive(Debug, Clone)]
pub struct UiOptions {
    /// The indexed project to read.
    pub project_root: PathBuf,
    /// An exact port, or `None` for the default with fallback.
    pub port: Option<u16>,
    /// Refuse every write, with the reason shown in place of Save.
    pub read_only: bool,
}

/// A running viewer: where it answers, and how to stop it.
pub struct UiServer {
    pub port: u16,
    pub url: String,
    state: Arc<server::AppState>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl UiServer {
    /// The live channel's state, for tests and diagnostics.
    pub fn state(&self) -> &server::AppState {
        &self.state
    }

    /// Stop accepting, end every live stream, and wait for the server to drain.
    pub async fn close(mut self) {
        self.state.events.close();
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

/// The reason shown in place of Save when the viewer was started read-only.
pub const READ_ONLY_REASON: &str =
    "This viewer was started with --read-only, so trails cannot be saved.";

/// Bind and start the viewer. Resolves once the socket is bound, so the caller
/// can print a URL that already answers.
pub async fn start(options: UiOptions) -> anyhow::Result<UiServer> {
    let listener = server::bind(options.port).await?;
    let port = listener.local_addr()?.port();
    let state = app_state(&options, port)?;
    let app = server::router(state.clone());
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await;
    });
    Ok(UiServer {
        port,
        url: format!("http://127.0.0.1:{port}"),
        state,
        shutdown: Some(tx),
        task: Some(task),
    })
}

/// Run the viewer until SIGINT or SIGTERM — for the CLI, which owns no async
/// runtime. `on_ready` runs once the socket is bound, with the URL that
/// already answers.
pub fn serve_until_shutdown(options: UiOptions, on_ready: impl FnOnce(&str)) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let server = start(options).await?;
        on_ready(&server.url);
        shutdown_signal().await;
        // Streams first: a client still attached would hold its socket open
        // against the server's own shutdown.
        server.close().await;
        Ok(())
    })
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).ok();
        let term = async {
            match terminate.as_mut() {
                Some(stream) => {
                    stream.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            () = term => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// The server state for `options`, answering as if bound to `port` (the
/// `Host` and `Origin` checks compare against it). [`start`] builds one; tests
/// drive [`server::router`] over it without a socket.
pub fn app_state(options: &UiOptions, port: u16) -> anyhow::Result<Arc<server::AppState>> {
    let requested = options.project_root.clone();
    let project_root = std::fs::canonicalize(&requested).unwrap_or(requested);
    let paths = session::resolve_paths(&project_root)?;
    let state = Arc::new(server::AppState {
        project_root: paths.project().to_path_buf(),
        paths,
        read_only: options.read_only,
        read_only_reason: options.read_only.then(|| READ_ONLY_REASON.to_string()),
        port: AtomicU16::new(port),
        events: events::EventHub::new(),
        caches: caches::Caches::default(),
        trail_author: std::sync::OnceLock::new(),
    });
    state.port.store(port, Ordering::Relaxed);
    Ok(state)
}

/// Whether this binary carries the viewer bundle.
pub fn has_viewer() -> bool {
    assets::has_viewer()
}

/// The project root a `codegraph ui [path]` invocation reads: the nearest
/// directory at or above `start` holding an index, as upstream's walk-up does.
pub fn find_indexed_root(start: &Path) -> Option<PathBuf> {
    let mut current = std::fs::canonicalize(start).ok()?;
    loop {
        if let Ok(paths) = session::resolve_paths(&current)
            && paths.current_db().is_file()
        {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}
