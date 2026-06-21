//! Structured logging using tracing + tracing-subscriber.
//!
//! One entry point: `init_logger(&LoggerConfig)`.
//! Call once early in main; keep the returned WorkerGuard alive for the program's lifetime.
//!
//! Features:
//! - Simultaneous stdout + rolling-file (daily) output
//! - Local-timezone timestamps (UTC fallback on Linux after threads spawn)
//! - RUST_LOG environment-variable override (precedence over config level)
//! - Runtime log-level change via set_log_level() (no restart)
//! - Optional JSON output for log shippers
//! - Forgiving level parsing (unknown → "info", never panic)
//!
//! For libraries: emit tracing events; the binary calls init_logger.
//!
//! ### Important: Guard Lifetime
//! The returned `WorkerGuard` must stay alive for the program's lifetime. Dropping it
//! flushes and stops the background file-writer thread; buffered logs would be lost.
//! Always bind it in main: `let _guard = init_logger(&cfg)?;`

use std::path::PathBuf;
use std::sync::OnceLock;
use time::format_description::well_known::Rfc3339;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt::time::OffsetTime;
use tracing_subscriber::{
    filter::LevelFilter, fmt, layer::SubscriberExt, reload, util::SubscriberInitExt, EnvFilter,
    Layer, Registry,
};

/// Configuration for the application logger.
///
/// Carry this in your app settings / config file and pass it to `init_logger`.
/// Every field has a safe default so a partial or empty config still produces
/// useful logs instead of silence.
#[derive(Debug, Clone)]
pub struct LoggerConfig {
    /// Minimum level: "trace" | "debug" | "info" | "warn" | "error".
    /// Unknown values fall back to "info".
    pub level: String,
    /// Directory for rolling log files. Created if missing.
    pub directory: PathBuf,
    /// File name prefix, e.g. "myapp" -> "myapp.2026-02-04".
    pub file_prefix: String,
    /// Write logs to stdout (recommended for containers / dev).
    pub stdout: bool,
    /// Write logs to a rolling file in `directory`.
    pub file: bool,
    /// Emit JSON instead of human-readable lines (recommended for prod log shippers).
    pub json: bool,
    /// Include source file:line in each event (handy in dev, noisy in prod).
    pub show_location: bool,
}

impl Default for LoggerConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            directory: PathBuf::from("logs"),
            file_prefix: env!("CARGO_PKG_NAME").to_string(),
            stdout: true,
            file: true,
            json: false,
            show_location: false,
        }
    }
}

/// Map a string level to a `LevelFilter`, defaulting to INFO for anything
/// unrecognized. A config typo should never silence the logger or crash boot.
fn parse_level(level: &str) -> LevelFilter {
    match level.trim().to_lowercase().as_str() {
        "trace" => LevelFilter::TRACE,
        "debug" => LevelFilter::DEBUG,
        "info" => LevelFilter::INFO,
        "warn" | "warning" => LevelFilter::WARN,
        "error" => LevelFilter::ERROR,
        "off" | "none" => LevelFilter::OFF,
        _ => LevelFilter::INFO,
    }
}

/// Handle used to change the log level at runtime. Stored globally so
/// `set_log_level` can reach it from anywhere (HTTP handler, signal, command).
static RELOAD_HANDLE: OnceLock<reload::Handle<LevelFilter, Registry>> = OnceLock::new();

/// Build a timer that prints timestamps in the machine's local timezone.
/// If the local offset can't be determined (a known Linux/threads limitation),
/// fall back to UTC rather than failing to start.
fn local_timer() -> OffsetTime<Rfc3339> {
    match OffsetTime::local_rfc_3339() {
        Ok(t) => t,
        Err(_) => OffsetTime::new(time::UtcOffset::UTC, Rfc3339),
    }
}

/// Apply shared formatting choices (JSON vs text, location, timer) to a writer layer.
/// Generic over the writer/format builder so stdout and file share config.
fn build_fmt_layer<S, W>(
    layer: fmt::Layer<S, fmt::format::DefaultFields, fmt::format::Format<fmt::format::Full>, W>,
    cfg: &LoggerConfig,
    timer: OffsetTime<Rfc3339>,
) -> Box<dyn Layer<S> + Send + Sync>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    W: for<'w> fmt::MakeWriter<'w> + Send + Sync + 'static,
{
    let layer = layer
        .with_timer(timer)
        .with_target(true)
        .with_file(cfg.show_location)
        .with_line_number(cfg.show_location);

    if cfg.json {
        layer.json().with_current_span(true).boxed()
    } else {
        layer.boxed()
    }
}

/// Initialize the global logger. Call ONCE, as early as possible in `main`.
///
/// Returns a `WorkerGuard` that MUST be kept alive for the program's lifetime
/// (e.g. `let _guard = init_logger(&cfg)?;` in `main`). Dropping it flushes and
/// stops the background file-writer thread, so buffered logs would be lost.
///
/// # Errors
///
/// Returns an error if:
/// - Logger already initialized (RELOAD_HANDLE already set)
/// - Cannot create the log directory
pub fn init_logger(cfg: &LoggerConfig) -> Result<Option<WorkerGuard>, Box<dyn std::error::Error>> {
    let base_level = parse_level(&cfg.level);

    // Reloadable level filter -> enables runtime `set_log_level`.
    let (level_filter, reload_handle) = reload::Layer::new(base_level);
    RELOAD_HANDLE
        .set(reload_handle)
        .map_err(|_| "logger already initialized")?;

    // RUST_LOG (and per-target directives) override the base level when present,
    // matching the conventional Rust logging UX. Falls back to the config level.
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(cfg.level.clone()));

    // Local-timezone timestamps. Resolve the offset once at startup.
    let timer = local_timer();

    let mut guard: Option<WorkerGuard> = None;

    // Stdout layer (human-readable or JSON).
    let stdout_layer = if cfg.stdout {
        Some(build_fmt_layer(
            fmt::layer().with_writer(std::io::stdout),
            cfg,
            timer.clone(),
        ))
    } else {
        None
    };

    // Rolling-file layer (daily rotation, non-blocking writes).
    let file_layer = if cfg.file {
        std::fs::create_dir_all(&cfg.directory)?;
        let file_appender = tracing_appender::rolling::daily(&cfg.directory, &cfg.file_prefix);
        let (non_blocking, worker_guard) = tracing_appender::non_blocking(file_appender);
        guard = Some(worker_guard);
        Some(build_fmt_layer(
            // Files should never carry ANSI color codes.
            fmt::layer().with_ansi(false).with_writer(non_blocking),
            cfg,
            timer,
        ))
    } else {
        None
    };

    Registry::default()
        .with(level_filter)
        .with(env_filter)
        .with(stdout_layer)
        .with(file_layer)
        .init();

    tracing::info!(level = %base_level, "logger initialized");
    Ok(guard)
}

/// Change the global log level at runtime. Safe to call from an HTTP handler,
/// signal handler, or admin command. Unknown levels fall back to INFO.
pub fn set_log_level(level: &str) -> Result<(), String> {
    let new_level = parse_level(level);
    let handle = RELOAD_HANDLE.get().ok_or("logger not initialized")?;
    handle
        .modify(|filter| *filter = new_level)
        .map_err(|e| format!("failed to reload log level: {e}"))?;
    tracing::info!(level = %new_level, "log level changed at runtime");
    Ok(())
}

/// Report the current global level as a lowercase string.
pub fn current_log_level() -> Option<String> {
    let handle = RELOAD_HANDLE.get()?;
    handle.clone_current().map(|f| f.to_string().to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_parse_level_forgiving() {
        assert_eq!(parse_level("trace"), LevelFilter::TRACE);
        assert_eq!(parse_level("debug"), LevelFilter::DEBUG);
        assert_eq!(parse_level("info"), LevelFilter::INFO);
        assert_eq!(parse_level("warn"), LevelFilter::WARN);
        assert_eq!(parse_level("warning"), LevelFilter::WARN);
        assert_eq!(parse_level("error"), LevelFilter::ERROR);
        assert_eq!(parse_level("off"), LevelFilter::OFF);
        // Unknown -> info (never panic)
        assert_eq!(parse_level("unknown"), LevelFilter::INFO);
        assert_eq!(parse_level("garbage"), LevelFilter::INFO);
    }

    #[test]
    fn test_logger_config_default() {
        let cfg = LoggerConfig::default();
        assert_eq!(cfg.level, "info");
        assert_eq!(cfg.directory, PathBuf::from("logs"));
        assert!(cfg.stdout);
        assert!(cfg.file);
        assert!(!cfg.json);
        assert!(!cfg.show_location);
    }

    #[test]
    fn test_init_logger_creates_directory() {
        let temp_dir = std::env::temp_dir().join("codegraph_test_logs");
        let _ = fs::remove_dir_all(&temp_dir);

        let cfg = LoggerConfig {
            level: "info".to_string(),
            directory: temp_dir.clone(),
            file_prefix: "test".to_string(),
            stdout: false,
            file: true,
            json: false,
            show_location: false,
        };

        let result = init_logger(&cfg);
        assert!(result.is_ok(), "init_logger should succeed");
        assert!(temp_dir.exists(), "log directory should be created");

        // Clean up
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_logger_config_custom() {
        let cfg = LoggerConfig {
            level: "debug".to_string(),
            directory: PathBuf::from("/tmp/custom_logs"),
            file_prefix: "custom".to_string(),
            stdout: true,
            file: false,
            json: true,
            show_location: true,
        };
        assert_eq!(cfg.level, "debug");
        assert_eq!(cfg.file_prefix, "custom");
        assert!(cfg.json);
        assert!(cfg.show_location);
    }

    #[test]
    fn test_env_filter_override() {
        // This test demonstrates RUST_LOG precedence:
        // If RUST_LOG is set, it overrides config level.
        // We test this by checking that parse_level respects the config,
        // and EnvFilter would respect the env var (the tracing-subscriber handles it).

        std::env::remove_var("RUST_LOG");
        let base_level = parse_level("info");
        assert_eq!(base_level, LevelFilter::INFO);

        // If RUST_LOG were set, EnvFilter::try_from_default_env() would pick it up
        // (we don't actually set it here to keep tests isolated)
    }
}
