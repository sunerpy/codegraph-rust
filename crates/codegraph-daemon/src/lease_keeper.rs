//! Keep a stdio session attached to its project's shared daemon (upstream #2293).
//!
//! A cold-start `serve --mcp` answers its handshake direct and spawns the shared
//! daemon fire-and-forget. That session holds no writer and runs no watcher: the
//! daemon is what keeps its index live, and a daemon with no client idle-exits
//! after `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS`. The keeper retains a passive lease
//! once the daemon's socket is up, so the daemon lives as long as the session.
//! When the lease stops being live (the daemon crashed, was replaced, or the
//! index was re-created) it starts or re-attaches a daemon with backoff:
//! `CODEGRAPH_DAEMON_RETRY_MS` (default 5000, `0` disables retrying), doubling
//! up to `CODEGRAPH_DAEMON_RETRY_MAX_MS` (default 300000).

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Result;
use codegraph_mcp::ProjectService as _;
use tracing::{debug, info};

use crate::project_service::{
    ProjectDaemonLease, attach_project_daemon_passive, retain_project_daemon_passive,
};

/// Env var name: milliseconds a stdio session waits before starting or
/// re-attaching the shared daemon after its lease was lost (default 5000).
/// `0` turns retrying off. Mirrors upstream.
pub const CODEGRAPH_DAEMON_RETRY_MS: &str = "CODEGRAPH_DAEMON_RETRY_MS";

/// Env var name: the cap the retry delay doubles up to (default 300000).
pub const CODEGRAPH_DAEMON_RETRY_MAX_MS: &str = "CODEGRAPH_DAEMON_RETRY_MAX_MS";

const DEFAULT_DAEMON_RETRY_MS: u64 = 5_000;
const DEFAULT_DAEMON_RETRY_MAX_MS: u64 = 300_000;

/// How often a held lease is checked for liveness. The check reads the daemon's
/// pid record and stats the index database, so it is cheap.
const LEASE_LIVENESS_POLL: Duration = Duration::from_millis(500);

/// Backoff for re-attaching the shared daemon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DaemonRetryPolicy {
    base: Duration,
    max: Duration,
}

impl DaemonRetryPolicy {
    /// Read [`CODEGRAPH_DAEMON_RETRY_MS`] and [`CODEGRAPH_DAEMON_RETRY_MAX_MS`].
    #[must_use]
    pub fn from_env() -> Self {
        let base = parse_delay_ms(
            std::env::var(CODEGRAPH_DAEMON_RETRY_MS).ok().as_deref(),
            DEFAULT_DAEMON_RETRY_MS,
        );
        let max = parse_delay_ms(
            std::env::var(CODEGRAPH_DAEMON_RETRY_MAX_MS).ok().as_deref(),
            DEFAULT_DAEMON_RETRY_MAX_MS,
        );
        Self::from_millis(base, max)
    }

    /// A policy starting at `base_ms` and doubling up to `max_ms`; the cap is
    /// never below the base.
    #[must_use]
    pub fn from_millis(base_ms: u64, max_ms: u64) -> Self {
        Self {
            base: Duration::from_millis(base_ms),
            max: Duration::from_millis(max_ms.max(base_ms)),
        }
    }

    /// Whether a lost or failed lease is retried at all.
    #[must_use]
    pub fn retries(&self) -> bool {
        !self.base.is_zero()
    }

    fn next(&self, delay: Duration) -> Duration {
        delay.saturating_mul(2).min(self.max)
    }
}

/// A non-negative millisecond value; unset, empty, malformed or negative gives
/// `fallback`, and a fraction is truncated (upstream `parseDelayMs`).
fn parse_delay_ms(raw: Option<&str>, fallback: u64) -> u64 {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return fallback;
    };
    if let Ok(millis) = raw.parse::<u64>() {
        return millis;
    }
    match raw.parse::<f64>() {
        // `as` saturates, so an out-of-range value becomes u64::MAX.
        Ok(millis) if millis.is_finite() && millis >= 0.0 => millis.floor() as u64,
        _ => fallback,
    }
}

/// How the keeper asks for a lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Attach {
    /// This process has just spawned the daemon: wait for its socket and never
    /// start a second one.
    JustSpawned,
    /// Start a daemon when no live one owns the project, then attach.
    StartIfAbsent,
}

/// Keeps a passive daemon lease for as long as it is alive; dropping it stops
/// the background thread at its next wait.
#[derive(Debug)]
pub struct SessionLeaseKeeper {
    _stop: mpsc::Sender<()>,
}

/// Keep this session attached to the daemon it has just spawned for
/// `project_root`, and start or re-attach one with backoff whenever the lease
/// is lost. The work runs on a background thread; the returned keeper must stay
/// alive for the session.
#[must_use]
pub fn keep_session_attached(project_root: PathBuf, no_watch: bool) -> SessionLeaseKeeper {
    let (stop, stopped) = mpsc::channel::<()>();
    let policy = DaemonRetryPolicy::from_env();
    let spawned = std::thread::Builder::new()
        .name("codegraph-daemon-lease".to_string())
        .spawn(move || {
            run_keeper(
                policy,
                LEASE_LIVENESS_POLL,
                |attach| -> Result<ProjectDaemonLease> {
                    match attach {
                        Attach::JustSpawned => attach_project_daemon_passive(&project_root),
                        Attach::StartIfAbsent => {
                            retain_project_daemon_passive(&project_root, no_watch)
                        }
                    }
                },
                ProjectDaemonLease::is_live,
                |delay| wait_unless_stopped(&stopped, delay),
            );
        });
    if let Err(error) = spawned {
        debug!(%error, "could not start the daemon lease keeper; the session serves without one");
    }
    SessionLeaseKeeper { _stop: stop }
}

/// Wait `delay`; `false` once the keeper was dropped.
fn wait_unless_stopped(stopped: &mpsc::Receiver<()>, delay: Duration) -> bool {
    matches!(
        stopped.recv_timeout(delay),
        Err(mpsc::RecvTimeoutError::Timeout)
    )
}

/// The keeper loop, with every effect injected so the backoff is unit-testable:
/// `attach` acquires a lease, `is_live` checks a held one, and `wait` sleeps for
/// the given delay and returns `false` when the keeper should stop.
fn run_keeper<L>(
    policy: DaemonRetryPolicy,
    liveness_poll: Duration,
    mut attach: impl FnMut(Attach) -> Result<L>,
    is_live: impl Fn(&L) -> bool,
    mut wait: impl FnMut(Duration) -> bool,
) {
    let mut mode = Attach::JustSpawned;
    let mut delay = policy.base;
    loop {
        match attach(mode) {
            Ok(lease) => {
                debug!(?mode, "retained a passive lease on the shared daemon");
                delay = policy.base;
                loop {
                    if !wait(liveness_poll) {
                        return;
                    }
                    if !is_live(&lease) {
                        break;
                    }
                }
                drop(lease);
                if policy.retries() {
                    info!(
                        retry_in_ms = delay.as_millis(),
                        "shared daemon lease lost; starting or re-attaching a daemon"
                    );
                } else {
                    info!("shared daemon lease lost; retrying is disabled");
                }
            }
            Err(error) => {
                debug!(?mode, error = %format!("{error:#}"), "no shared daemon lease");
            }
        }
        if !policy.retries() || !wait(delay) {
            return;
        }
        delay = policy.next(delay);
        mode = Attach::StartIfAbsent;
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use anyhow::anyhow;

    use super::*;

    const POLL: Duration = Duration::from_millis(500);

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    /// One scripted attach result: `Some(n)` is a lease that stays live for
    /// `n` liveness polls, `None` a failed attach.
    struct Script {
        attaches: RefCell<Vec<Option<u32>>>,
        modes: RefCell<Vec<Attach>>,
        waits: RefCell<Vec<Duration>>,
        polls_left: Cell<u32>,
        max_waits: usize,
    }

    impl Script {
        fn new(attaches: Vec<Option<u32>>, max_waits: usize) -> Self {
            Self {
                attaches: RefCell::new(attaches.into_iter().rev().collect()),
                modes: RefCell::new(Vec::new()),
                waits: RefCell::new(Vec::new()),
                polls_left: Cell::new(0),
                max_waits,
            }
        }

        fn run(&self, policy: DaemonRetryPolicy) {
            run_keeper(
                policy,
                POLL,
                |mode| {
                    self.modes.borrow_mut().push(mode);
                    match self.attaches.borrow_mut().pop() {
                        Some(Some(live_polls)) => {
                            self.polls_left.set(live_polls);
                            Ok(())
                        }
                        Some(None) | None => Err(anyhow!("no daemon")),
                    }
                },
                |()| {
                    let left = self.polls_left.get();
                    self.polls_left.set(left.saturating_sub(1));
                    left > 0
                },
                |delay| {
                    let mut waits = self.waits.borrow_mut();
                    waits.push(delay);
                    waits.len() < self.max_waits
                },
            );
        }

        fn retry_waits(&self) -> Vec<Duration> {
            self.waits
                .borrow()
                .iter()
                .copied()
                .filter(|&delay| delay != POLL)
                .collect()
        }
    }

    #[test]
    fn retry_delay_doubles_up_to_the_cap() {
        let script = Script::new(vec![None, None, None, None, None], 5);
        script.run(DaemonRetryPolicy::from_millis(5_000, 15_000));
        assert_eq!(
            script.retry_waits(),
            [ms(5_000), ms(10_000), ms(15_000), ms(15_000), ms(15_000)]
        );
    }

    #[test]
    fn first_attach_never_starts_a_daemon_and_later_ones_may() {
        // Stops on the fourth wait, while the third attach's lease is held.
        let script = Script::new(vec![None, None, Some(9)], 4);
        script.run(DaemonRetryPolicy::from_millis(5_000, 300_000));
        assert_eq!(
            *script.modes.borrow(),
            [
                Attach::JustSpawned,
                Attach::StartIfAbsent,
                Attach::StartIfAbsent
            ]
        );
    }

    #[test]
    fn a_lost_lease_is_retried_from_the_base_delay() {
        // Fails twice (5 s, 10 s), holds a lease for two polls, loses it, then
        // the next retry waits the base delay again rather than 20 s.
        let script = Script::new(vec![None, None, Some(2), None], 8);
        script.run(DaemonRetryPolicy::from_millis(5_000, 300_000));
        assert_eq!(
            *script.waits.borrow(),
            [
                ms(5_000),
                ms(10_000),
                POLL,
                POLL,
                POLL,
                ms(5_000),
                ms(10_000),
                ms(20_000)
            ]
        );
    }

    #[test]
    fn zero_retry_delay_attaches_once_and_never_retries() {
        let failed = Script::new(vec![None, Some(9)], 10);
        failed.run(DaemonRetryPolicy::from_millis(0, 300_000));
        assert_eq!(*failed.modes.borrow(), [Attach::JustSpawned]);
        assert!(failed.waits.borrow().is_empty());

        let lost = Script::new(vec![Some(1), Some(9)], 10);
        lost.run(DaemonRetryPolicy::from_millis(0, 300_000));
        assert_eq!(*lost.modes.borrow(), [Attach::JustSpawned]);
        assert_eq!(*lost.waits.borrow(), [POLL, POLL]);
    }

    #[test]
    fn a_stop_request_ends_the_keeper_while_a_lease_is_held() {
        let script = Script::new(vec![Some(u32::MAX)], 3);
        script.run(DaemonRetryPolicy::from_millis(5_000, 300_000));
        assert_eq!(*script.modes.borrow(), [Attach::JustSpawned]);
        assert_eq!(*script.waits.borrow(), [POLL, POLL, POLL]);
    }

    #[test]
    fn dropping_the_keeper_stops_its_waits() {
        let (stop, stopped) = mpsc::channel::<()>();
        assert!(wait_unless_stopped(&stopped, ms(1)));
        drop(stop);
        assert!(!wait_unless_stopped(&stopped, ms(60_000)));
    }

    #[test]
    fn delay_values_parse_like_upstream() {
        assert_eq!(parse_delay_ms(None, 5_000), 5_000);
        assert_eq!(parse_delay_ms(Some(""), 5_000), 5_000);
        assert_eq!(parse_delay_ms(Some("  "), 5_000), 5_000);
        assert_eq!(parse_delay_ms(Some("0"), 5_000), 0);
        assert_eq!(parse_delay_ms(Some("250"), 5_000), 250);
        assert_eq!(parse_delay_ms(Some("1500.9"), 5_000), 1_500);
        assert_eq!(parse_delay_ms(Some("-1"), 5_000), 5_000);
        assert_eq!(parse_delay_ms(Some("soon"), 5_000), 5_000);
        assert_eq!(parse_delay_ms(Some("NaN"), 5_000), 5_000);
    }

    #[test]
    fn the_cap_is_never_below_the_base() {
        let policy = DaemonRetryPolicy::from_millis(10_000, 1_000);
        assert_eq!(policy.next(ms(10_000)), ms(10_000));
        assert!(policy.retries());
        assert!(!DaemonRetryPolicy::from_millis(0, 1_000).retries());
    }
}
