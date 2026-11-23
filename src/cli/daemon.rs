// Daemon control: PID file, daemonization, signals, and status file.
//
// The daemon is controlled through Unix signals and two files in the
// working directory (paths overridable through environment variables):
//
//   - PID file    `unified.pid`         (env `UNIFIED_PID_FILE`)
//   - Status file `unified.status.json` (env `UNIFIED_STATUS_FILE`)
//   - Log file    `unified.log`         (env `UNIFIED_LOG_FILE`)
//
// Control mechanism — Unix signals sent to the daemon process:
//
//   - SIGHUP  reload the configuration file. The file is loaded and
//             validated first; a failing configuration never replaces
//             the running one.
//   - SIGUSR1 toggle the paused state.
//   - SIGUSR2 toggle maintenance mode.
//
// The signal watcher runs inside the server process (installed by the
// `start` command before the HTTP server starts), so it works both for
// foreground starts and for daemonized starts. The `pause`, `resume`,
// and `maintenance` CLI commands read the status file first so they stay
// idempotent instead of blindly toggling: they only send SIGUSR1/SIGUSR2
// when the daemon is not already in the requested state.

use unified_core::config::AppConfig;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::events;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Environment variable overriding the PID file path.
pub const PID_FILE_ENV: &str = "UNIFIED_PID_FILE";
/// Default PID file name, resolved relative to the working directory.
pub const DEFAULT_PID_FILE: &str = "unified.pid";
/// Environment variable overriding the status file path.
pub const STATUS_FILE_ENV: &str = "UNIFIED_STATUS_FILE";
/// Default status file name, resolved relative to the working directory.
pub const DEFAULT_STATUS_FILE: &str = "unified.status.json";
/// Environment variable overriding the daemon log file path.
pub const LOG_FILE_ENV: &str = "UNIFIED_LOG_FILE";
/// Default daemon log file name, resolved relative to the working directory.
pub const DEFAULT_LOG_FILE: &str = "unified.log";

/// How long `stop` waits for the daemon to exit after SIGTERM before
/// escalating to SIGKILL.
const STOP_WAIT: Duration = Duration::from_secs(10);
/// Poll interval while waiting for the daemon to exit.
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Per-module request statistics, as reported by the daemon.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModuleStat {
    pub id: String,
    pub requests: u64,
}

/// Snapshot of the running daemon, written to the status file at startup
/// and updated when the daemon state changes (pause / maintenance /
// reload). The HTTP server should call `update_status_file` to keep the
/// live counters (`http_connections`, `total_requests`, `modules`, ...)
/// fresh; the monitor tolerates them staying at zero.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub pid: u32,
    pub version: String,
    pub mode: String,
    /// RFC 3339 timestamp of when the daemon started.
    pub started_at: String,
    /// Uptime in seconds at the time the status file was written.
    pub uptime_seconds: u64,
    pub paused: bool,
    pub maintenance: bool,
    pub listeners: Vec<String>,
    pub workers: usize,
    pub http_connections: u64,
    pub websocket_connections: u64,
    pub total_requests: u64,
    pub requests_per_second: f64,
    pub hook_events: u64,
    pub geo_lookups: u64,
    pub modules: Vec<ModuleStat>,
}

impl DaemonStatus {
    /// Build the initial status snapshot for a freshly started daemon.
    pub fn from_context(pid: u32, ctx: &AppContext) -> Self {
        let config = ctx.config.read().expect("config lock poisoned");
        Self {
            pid,
            version: env!("CARGO_PKG_VERSION").to_string(),
            mode: config.app.mode.clone(),
            started_at: chrono::Utc::now().to_rfc3339(),
            uptime_seconds: ctx.uptime_seconds(),
            paused: ctx.is_paused(),
            maintenance: ctx.is_maintenance(),
            listeners: config.app.tcp.clone(),
            workers: config.app.workers,
            ..Default::default()
        }
    }
}

/// Resolve the PID file path: `$UNIFIED_PID_FILE` when set, otherwise
/// `unified.pid` in the working directory.
pub fn pid_file_path() -> PathBuf {
    env_path(PID_FILE_ENV, DEFAULT_PID_FILE)
}

/// Resolve the status file path: `$UNIFIED_STATUS_FILE` when set,
/// otherwise `unified.status.json` in the working directory.
pub fn status_file_path() -> PathBuf {
    env_path(STATUS_FILE_ENV, DEFAULT_STATUS_FILE)
}

/// Resolve the daemon log file path: `$UNIFIED_LOG_FILE` when set,
/// otherwise `unified.log` in the working directory.
pub fn log_file_path() -> PathBuf {
    env_path(LOG_FILE_ENV, DEFAULT_LOG_FILE)
}

fn env_path(env_name: &str, default: &str) -> PathBuf {
    if let Ok(path) = std::env::var(env_name) {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    PathBuf::from(default)
}

/// Read the PID from the PID file. Returns `None` when the file is
/// missing or unreadable; a corrupt file is removed and treated as absent.
fn read_pid_file() -> Option<u32> {
    let path = pid_file_path();
    let text = std::fs::read_to_string(&path).ok()?;
    match text.trim().parse::<u32>() {
        Ok(pid) if pid > 0 => Some(pid),
        _ => {
            let _ = std::fs::remove_file(&path);
            None
        }
    }
}

/// Write the PID file.
pub fn write_pid_file(pid: u32) -> UnifiedResult<()> {
    std::fs::write(pid_file_path(), pid.to_string()).map_err(|error| {
        UnifiedError::internal(format!(
            "Cannot write PID file {}: {error}",
            pid_file_path().display()
        ))
    })
}

/// Remove the PID file, ignoring errors.
pub fn remove_pid_file() {
    let _ = std::fs::remove_file(pid_file_path());
}

/// Remove the status file, ignoring errors.
pub fn remove_status_file() {
    let _ = std::fs::remove_file(status_file_path());
}

/// True when a process with this PID exists (or exists but belongs to
/// another user, which still counts as alive).
pub fn process_alive(pid: u32) -> bool {
    match kill(Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        Err(nix::errno::Errno::EPERM) => true,
        Err(_) => false,
    }
}

/// The PID of the running daemon, or `None` when no daemon is running.
/// A stale PID file (process gone) is removed as a side effect.
pub fn is_running() -> Option<u32> {
    let pid = read_pid_file()?;
    if process_alive(pid) {
        Some(pid)
    } else {
        remove_pid_file();
        remove_status_file();
        None
    }
}

/// Double-fork daemonization.
///
/// In the parent process this function never returns: it waits for the
/// daemon to write its PID file, reports the result on the terminal, and
/// exits (0 on success, 1 when the daemon failed to start). In the
/// daemon process it detaches from the controlling terminal with
/// `setsid`, forks a second time so the daemon can never reacquire a
/// controlling terminal, redirects stdin from `/dev/null` and stdout /
/// stderr to the log file, writes the PID file, and returns `Ok(())` so
/// the caller can continue and run the server in the daemon process.
///
/// The working directory is deliberately left unchanged: configuration,
/// PID, status, and log paths are resolved relative to it.
pub fn start_daemonize() -> UnifiedResult<()> {
    use nix::unistd::{fork, setsid, ForkResult};

    // Remove any stale PID file first; `is_running` normally does this,
    // but daemonization must never report a stale PID as its own.
    remove_pid_file();

    // First fork: the parent waits for the PID file and reports the result.
    match unsafe { fork() } {
        Ok(ForkResult::Parent { .. }) => report_daemon_start(),
        Ok(ForkResult::Child) => {}
        Err(error) => {
            return Err(UnifiedError::internal(format!(
                "Cannot fork daemon process: {error}"
            )));
        }
    }

    // Detach from the controlling terminal.
    setsid().map_err(|error| {
        UnifiedError::internal(format!("Cannot detach daemon session: {error}"))
    })?;

    // Second fork: the intermediate parent exits so the daemon can never
    // reacquire a controlling terminal.
    match unsafe { fork() } {
        Ok(ForkResult::Parent { .. }) => std::process::exit(0),
        Ok(ForkResult::Child) => {}
        Err(error) => {
            return Err(UnifiedError::internal(format!(
                "Cannot fork daemon process: {error}"
            )));
        }
    }

    // The daemon process: redirect stdio to the log file and record its PID.
    redirect_stdio()?;
    write_pid_file(std::process::id())?;
    Ok(())
}

/// Parent side of `start_daemonize`: wait up to 5 seconds for the PID
/// file, report the outcome, and exit. Never returns.
fn report_daemon_start() -> ! {
    let log = log_file_path();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(pid) = read_pid_file() {
            // Give the daemon a moment to prove it stays alive.
            std::thread::sleep(Duration::from_millis(300));
            if process_alive(pid) {
                println!("Unified daemon started (PID {pid}).");
                println!("Logs: {}", log.display());
                std::process::exit(0);
            }
            eprintln!("Unified daemon failed to start; see {} for details.", log.display());
            std::process::exit(1);
        }
        if Instant::now() >= deadline {
            eprintln!(
                "Unified daemon did not report a PID within 5 seconds; see {} for details.",
                log.display()
            );
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Redirect stdin from `/dev/null` and stdout / stderr to the log file.
fn redirect_stdio() -> UnifiedResult<()> {
    use std::os::fd::OwnedFd;
    use std::os::unix::io::FromRawFd;

    let dev_null = std::fs::File::open("/dev/null")
        .map_err(|error| UnifiedError::internal(format!("Cannot open /dev/null: {error}")))?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file_path())
        .map_err(|error| {
            UnifiedError::internal(format!(
                "Cannot open log file {}: {error}",
                log_file_path().display()
            ))
        })?;
    // nix 0.30 `dup2` takes the target as `&mut OwnedFd`. Build it from
    // the well-known descriptor number, then forget it so the descriptor
    // stays open for the daemon's lifetime (dropping it would close it).
    // SAFETY: descriptors 0/1/2 are open in every process.
    let mut stdin_fd = unsafe { OwnedFd::from_raw_fd(0) };
    nix::unistd::dup2(&dev_null, &mut stdin_fd)
        .map_err(|error| UnifiedError::internal(format!("Cannot redirect stdin: {error}")))?;
    std::mem::forget(stdin_fd);
    let mut stdout_fd = unsafe { OwnedFd::from_raw_fd(1) };
    nix::unistd::dup2(&log, &mut stdout_fd)
        .map_err(|error| UnifiedError::internal(format!("Cannot redirect stdout: {error}")))?;
    std::mem::forget(stdout_fd);
    let mut stderr_fd = unsafe { OwnedFd::from_raw_fd(2) };
    nix::unistd::dup2(&log, &mut stderr_fd)
        .map_err(|error| UnifiedError::internal(format!("Cannot redirect stderr: {error}")))?;
    std::mem::forget(stderr_fd);
    Ok(())
}

/// Stop the running daemon: send SIGTERM, wait up to 10 seconds for the
/// process to exit, escalate to SIGKILL when needed, then remove the PID
/// and status files. Returns `Ok(true)` when a daemon was stopped and
/// `Ok(false)` when no daemon was running.
pub fn stop() -> UnifiedResult<bool> {
    let Some(pid) = read_pid_file() else {
        return Ok(false);
    };
    if !process_alive(pid) {
        remove_pid_file();
        remove_status_file();
        return Ok(false);
    }
    kill(Pid::from_raw(pid as i32), Some(Signal::SIGTERM)).map_err(|error| {
        UnifiedError::internal(format!("Cannot stop daemon process {pid}: {error}"))
    })?;
    let deadline = Instant::now() + STOP_WAIT;
    while process_alive(pid) && Instant::now() < deadline {
        std::thread::sleep(STOP_POLL_INTERVAL);
    }
    if process_alive(pid) {
        kill(Pid::from_raw(pid as i32), Some(Signal::SIGKILL)).map_err(|error| {
            UnifiedError::internal(format!("Cannot kill daemon process {pid}: {error}"))
        })?;
        std::thread::sleep(Duration::from_secs(1));
    }
    remove_pid_file();
    remove_status_file();
    Ok(true)
}

/// Send a signal to the running daemon. Returns `Ok(true)` when the
/// signal was sent and `Ok(false)` when no daemon is running.
pub fn send_signal(signal: Signal) -> UnifiedResult<bool> {
    let Some(pid) = is_running() else {
        return Ok(false);
    };
    kill(Pid::from_raw(pid as i32), Some(signal)).map_err(|error| {
        UnifiedError::internal(format!("Cannot signal daemon process {pid}: {error}"))
    })?;
    Ok(true)
}

/// Write the daemon status file.
pub fn write_status(status: &DaemonStatus) -> UnifiedResult<()> {
    let text = serde_json::to_string_pretty(status).map_err(|error| {
        UnifiedError::internal(format!("Cannot serialize daemon status: {error}"))
    })?;
    std::fs::write(status_file_path(), text).map_err(|error| {
        UnifiedError::internal(format!(
            "Cannot write status file {}: {error}",
            status_file_path().display()
        ))
    })
}

/// Read the daemon status file. Returns `None` when the daemon is not
/// running or has not written a status file yet.
pub fn read_status() -> Option<DaemonStatus> {
    let text = std::fs::read_to_string(status_file_path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// Read the status file, apply `update` to it, and write it back. Does
/// nothing when no status file exists.
pub fn update_status_file(update: impl FnOnce(&mut DaemonStatus)) -> UnifiedResult<()> {
    if let Some(mut status) = read_status() {
        update(&mut status);
        write_status(&status)?;
    }
    Ok(())
}

/// Format a duration in seconds as a human-readable uptime string, for
/// example `3d 4h 12m 5s`, `2h 14m 5s`, or `45s`.
pub fn format_uptime(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let secs = seconds % 60;
    if days > 0 {
        format!("{days}d {hours}h {minutes}m {secs}s")
    } else if hours > 0 {
        format!("{hours}h {minutes}m {secs}s")
    } else if minutes > 0 {
        format!("{minutes}m {secs}s")
    } else {
        format!("{secs}s")
    }
}

/// Watch for daemon control signals inside the server process. Runs
/// forever; spawn it as a Tokio task before starting the HTTP server.
///
///   - SIGHUP  reload the configuration file (validated first; a failing
///             configuration never replaces the running one).
///   - SIGUSR1 toggle the paused state.
///   - SIGUSR2 toggle maintenance mode.
pub async fn watch_signals(ctx: Arc<AppContext>, config_path: PathBuf) {
    use tokio::signal::unix::{signal, SignalKind};

    let mut reload = match signal(SignalKind::hangup()) {
        Ok(stream) => stream,
        Err(error) => {
            log::error!("Cannot listen for SIGHUP: {error}");
            return;
        }
    };
    let mut pause = match signal(SignalKind::user_defined1()) {
        Ok(stream) => stream,
        Err(error) => {
            log::error!("Cannot listen for SIGUSR1: {error}");
            return;
        }
    };
    let mut maintenance = match signal(SignalKind::user_defined2()) {
        Ok(stream) => stream,
        Err(error) => {
            log::error!("Cannot listen for SIGUSR2: {error}");
            return;
        }
    };
    log::info!(
        "Daemon control signals ready: SIGHUP reload, SIGUSR1 pause toggle, SIGUSR2 maintenance toggle."
    );
    loop {
        tokio::select! {
            Some(()) = reload.recv() => handle_reload(&ctx, &config_path),
            Some(()) = pause.recv() => handle_pause_toggle(&ctx),
            Some(()) = maintenance.recv() => handle_maintenance_toggle(&ctx),
        }
    }
}

/// Handle SIGHUP: reload the configuration file through the context,
/// which validates it first and leaves the running configuration
/// untouched on failure.
fn handle_reload(ctx: &AppContext, config_path: &Path) {
    match AppConfig::load(config_path) {
        Ok(config) => match ctx.reload_config(config) {
            Ok(()) => {
                log::info!("Configuration reloaded from {}", config_path.display());
                let _ = update_status_file(|status| {
                    let config = ctx.config.read().expect("config lock poisoned");
                    status.mode = config.app.mode.clone();
                    status.listeners = config.app.tcp.clone();
                    status.workers = config.app.workers;
                });
            }
            Err(error) => {
                log::error!("Configuration reload refused: {error}");
            }
        },
        Err(error) => {
            log::error!(
                "Cannot reload configuration from {}: {error}",
                config_path.display()
            );
        }
    }
}

/// Handle SIGUSR1: toggle the paused state and emit the matching event.
fn handle_pause_toggle(ctx: &AppContext) {
    let paused = !ctx.is_paused();
    ctx.set_paused(paused);
    let event = if paused {
        events::APP_PAUSE
    } else {
        events::APP_RESUME
    };
    ctx.hooks.emit(
        event,
        serde_json::json!({ "paused": paused }),
    );
    log::info!(
        "Daemon {}",
        if paused { "paused" } else { "resumed" }
    );
    let _ = update_status_file(|status| status.paused = paused);
}

/// Handle SIGUSR2: toggle maintenance mode and emit the matching event.
fn handle_maintenance_toggle(ctx: &AppContext) {
    let maintenance = !ctx.is_maintenance();
    ctx.set_maintenance(maintenance);
    ctx.hooks.emit(
        events::APP_MAINTENANCE,
        serde_json::json!({ "maintenance": maintenance }),
    );
    log::info!(
        "Maintenance mode {}",
        if maintenance { "enabled" } else { "disabled" }
    );
    let _ = update_status_file(|status| status.maintenance = maintenance);
}
