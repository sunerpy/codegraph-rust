//! Session-scoped lifecycle for repositories reached through an explicit
//! `projectPath`.
//!
//! Query routing alone is intentionally read-only and request-scoped. A shipped
//! MCP host may additionally install a [`ProjectServiceBroker`] whose starter
//! attaches the addressed existing index to its shared daemon, waits for one
//! catch-up, and retains a session connection so the daemon's one watcher stays
//! alive. Keeping the starter injectable avoids a dependency cycle:
//! `codegraph-daemon` depends on this crate to serve MCP sessions, while the CLI
//! (which depends on both crates) supplies the concrete daemon starter.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use codegraph_core::IndexPaths;

/// Maximum number of distinct explicit projects for which one MCP session may
/// retain live services. The graph/query surface can still address other
/// projects after the session is restarted; this cap prevents an accidental
/// stream of paths from accumulating daemon connections and watchers forever.
pub const MAX_SESSION_PROJECT_SERVICES: usize = 32;

/// Live services retained for one explicit project. Dropping the final handle
/// must release that session's retained daemon/direct connection.
pub trait ProjectService: Send + Sync {
    /// Whether the retained services still serve the index on disk. A daemon
    /// that exited, or an index removed or re-created since attachment, makes
    /// the handle stale: the broker then drops it and prepares afresh instead
    /// of silently answering without live synchronization.
    fn is_live(&self) -> bool {
        true
    }
}

/// A token with no liveness to lose (tests and no-op starters).
impl ProjectService for () {}

/// Opaque lifetime token returned by the concrete service starter.
pub type ProjectServiceHandle = Arc<dyn ProjectService>;

/// Concrete service startup supplied by the binary/daemon integration layer.
pub type ProjectServiceStarter =
    Arc<dyn Fn(&Path) -> Result<ProjectServiceHandle> + Send + Sync + 'static>;

#[derive(Clone)]
pub struct ProjectServiceBroker {
    inner: Arc<BrokerInner>,
}

struct BrokerInner {
    starter: ProjectServiceStarter,
    handles: Mutex<HashMap<PathBuf, ProjectServiceHandle>>,
    gates: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

impl std::fmt::Debug for ProjectServiceBroker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProjectServiceBroker")
            .field("active_projects", &self.active_project_count())
            .field("limit", &MAX_SESSION_PROJECT_SERVICES)
            .finish_non_exhaustive()
    }
}

impl ProjectServiceBroker {
    #[must_use]
    pub fn new(starter: ProjectServiceStarter) -> Self {
        Self {
            inner: Arc::new(BrokerInner {
                starter,
                handles: Mutex::new(HashMap::new()),
                gates: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Ensure live service ownership for one already-resolved indexed project.
    ///
    /// The physical project root is the cache key, so aliases cannot create two
    /// retained services. Same-project concurrent calls serialize through a
    /// per-project gate; different projects may prepare in parallel. Failures
    /// are not cached, allowing a later call to retry after transient daemon or
    /// lease contention. This function never creates an index namespace: the
    /// concrete starter receives a root only after [`IndexPaths::resolve`], and
    /// the tool handler calls it only after `projectPath` resolved as indexed.
    pub fn ensure(&self, project: &Path) -> Result<()> {
        let paths = IndexPaths::resolve(project, std::env::var("CODEGRAPH_DIR").ok().as_deref())?;
        let key = paths.project().to_path_buf();

        {
            let mut handles = self
                .inner
                .handles
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match handles.get(&key) {
                Some(handle) if handle.is_live() => return Ok(()),
                // Stale: release it and re-attach below (a crashed daemon or an
                // uninit→init cycle leaves nothing watching this project).
                Some(_) => {
                    handles.remove(&key);
                }
                None => {}
            }
        }

        let gate = {
            let mut gates = self
                .inner
                .gates
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !gates.contains_key(&key) && gates.len() >= MAX_SESSION_PROJECT_SERVICES {
                bail!(
                    "explicit project live-service limit reached ({MAX_SESSION_PROJECT_SERVICES}); restart the MCP session (or, over HTTP, the server) before accessing more repositories"
                );
            }
            Arc::clone(
                gates
                    .entry(key.clone())
                    .or_insert_with(|| Arc::new(Mutex::new(()))),
            )
        };
        let _preparing = gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let mut handles = self
            .inner
            .handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if handles.contains_key(&key) {
            return Ok(());
        }
        // Do not hold the handle map across potentially slow catch-up work: the
        // per-project gate above provides same-key exclusion, while other keys
        // stay independent.
        drop(handles);
        let handle = match (self.inner.starter)(&key) {
            Ok(handle) => handle,
            Err(error) => {
                // A failed preparation owns no daemon connection or watcher, so
                // it must not consume one of the bounded retained-project slots
                // forever. Remove the reservation only when this caller is the
                // final user of the gate (the map + this local Arc). If another
                // same-project caller already cloned the gate, leave it in the
                // map: that waiter must retry through the SAME mutex rather than
                // racing a newly-created gate and starting a second service.
                let mut gates = self
                    .inner
                    .gates
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if gates
                    .get(&key)
                    .is_some_and(|stored| Arc::ptr_eq(stored, &gate))
                    && Arc::strong_count(&gate) == 2
                {
                    gates.remove(&key);
                }
                return Err(error);
            }
        };
        handles = self
            .inner
            .handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        handles.insert(key, handle);
        Ok(())
    }

    #[must_use]
    pub fn active_project_count(&self) -> usize {
        self.inner
            .handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    fn project(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "codegraph-project-services-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    struct Toggle(Arc<std::sync::atomic::AtomicBool>);

    impl ProjectService for Toggle {
        fn is_live(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }

    #[test]
    fn a_stale_handle_is_released_and_services_are_prepared_again() {
        let calls = Arc::new(AtomicUsize::new(0));
        let live = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let broker = ProjectServiceBroker::new(Arc::new({
            let calls = Arc::clone(&calls);
            let live = Arc::clone(&live);
            move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                live.store(true, Ordering::SeqCst);
                Ok(Arc::new(Toggle(Arc::clone(&live))) as ProjectServiceHandle)
            }
        }));
        let root = project("stale");
        broker.ensure(&root).unwrap();
        broker.ensure(&root).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "a live handle is reused");

        // The daemon exited or the index was re-created underneath the session.
        live.store(false, Ordering::SeqCst);
        broker.ensure(&root).unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a stale handle is re-prepared"
        );
        assert_eq!(broker.active_project_count(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn one_physical_project_starts_once_and_retains_one_handle() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let broker = ProjectServiceBroker::new(Arc::new({
            let calls = Arc::clone(&calls);
            let observed = Arc::clone(&observed);
            move |root| {
                calls.fetch_add(1, Ordering::SeqCst);
                observed
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(root.to_path_buf());
                Ok(Arc::new(()) as ProjectServiceHandle)
            }
        }));
        let root = project("once");

        broker.ensure(&root).unwrap();
        broker.ensure(&root.join(".")).unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(broker.active_project_count(), 1);
        assert_eq!(
            observed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_slice(),
            &[root.canonicalize().unwrap()]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_failed_start_is_retryable() {
        let calls = Arc::new(AtomicUsize::new(0));
        let broker = ProjectServiceBroker::new(Arc::new({
            let calls = Arc::clone(&calls);
            move |_| {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    anyhow::bail!("transient")
                }
                Ok(Arc::new(()) as ProjectServiceHandle)
            }
        }));
        let root = project("retry");

        assert!(broker.ensure(&root).is_err());
        broker.ensure(&root).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(broker.active_project_count(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failed_distinct_projects_do_not_exhaust_retention_capacity() {
        let broker = ProjectServiceBroker::new(Arc::new(|_| anyhow::bail!("transient")));
        let roots = (0..=MAX_SESSION_PROJECT_SERVICES)
            .map(|index| project(&format!("failed-cap-{index}")))
            .collect::<Vec<_>>();

        for root in roots.iter().take(MAX_SESSION_PROJECT_SERVICES) {
            let error = broker.ensure(root).expect_err("starter must fail");
            assert_eq!(error.to_string(), "transient");
        }
        assert_eq!(broker.active_project_count(), 0);

        let error = broker
            .ensure(&roots[MAX_SESSION_PROJECT_SERVICES])
            .expect_err("the next starter still runs and reports its own failure");
        assert_eq!(error.to_string(), "transient");
        assert!(
            !error.to_string().contains("live-service limit"),
            "failed starts must release their capacity reservation"
        );

        for root in roots {
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn failed_same_project_waiters_keep_one_gate_until_the_last_retry() {
        let calls = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let broker = ProjectServiceBroker::new(Arc::new({
            let calls = Arc::clone(&calls);
            let active = Arc::clone(&active);
            let max_active = Arc::clone(&max_active);
            let release_rx = Arc::clone(&release_rx);
            move |_| {
                let call = calls.fetch_add(1, Ordering::SeqCst);
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(now, Ordering::SeqCst);
                entered_tx.send(call).unwrap();
                release_rx
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .recv()
                    .unwrap();
                active.fetch_sub(1, Ordering::SeqCst);
                anyhow::bail!("transient-{call}")
            }
        }));
        let root = project("same-key-failures");

        let first = {
            let broker = broker.clone();
            let root = root.clone();
            std::thread::spawn(move || broker.ensure(&root))
        };
        assert_eq!(entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 0);

        let second = {
            let broker = broker.clone();
            let root = root.clone();
            std::thread::spawn(move || broker.ensure(&root))
        };
        wait_for_gate_users(&broker, &root, 3);
        release_tx.send(()).unwrap();
        assert_eq!(
            first.join().unwrap().unwrap_err().to_string(),
            "transient-0"
        );
        assert_eq!(entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 1);

        let third = {
            let broker = broker.clone();
            let root = root.clone();
            std::thread::spawn(move || broker.ensure(&root))
        };
        wait_for_gate_users(&broker, &root, 3);
        release_tx.send(()).unwrap();
        assert_eq!(
            second.join().unwrap().unwrap_err().to_string(),
            "transient-1"
        );
        assert_eq!(entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 2);
        release_tx.send(()).unwrap();
        assert_eq!(
            third.join().unwrap().unwrap_err().to_string(),
            "transient-2"
        );

        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(max_active.load(Ordering::SeqCst), 1);
        assert_eq!(broker.active_project_count(), 0);
        assert!(
            broker
                .inner
                .gates
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty(),
            "the final failed waiter releases the reservation"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    fn wait_for_gate_users(broker: &ProjectServiceBroker, root: &Path, minimum: usize) {
        let key = IndexPaths::resolve(root, std::env::var("CODEGRAPH_DIR").ok().as_deref())
            .unwrap()
            .project()
            .to_path_buf();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let users = broker
                .inner
                .gates
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&key)
                .map(Arc::strong_count)
                .unwrap_or(0);
            if users >= minimum {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "gate never reached {minimum} strong users (last={users})"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn distinct_project_retention_is_bounded() {
        let broker =
            ProjectServiceBroker::new(Arc::new(|_| Ok(Arc::new(()) as ProjectServiceHandle)));
        let roots = (0..=MAX_SESSION_PROJECT_SERVICES)
            .map(|index| project(&format!("cap-{index}")))
            .collect::<Vec<_>>();
        for root in roots.iter().take(MAX_SESSION_PROJECT_SERVICES) {
            broker.ensure(root).unwrap();
        }
        let error = broker
            .ensure(&roots[MAX_SESSION_PROJECT_SERVICES])
            .expect_err("the next distinct project must hit the session cap");
        assert!(error.to_string().contains("live-service limit"));
        assert_eq!(broker.active_project_count(), MAX_SESSION_PROJECT_SERVICES);
        for root in roots {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}
