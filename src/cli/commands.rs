// Command line interface.
//
// Root command `unified` with global arguments `--config` / `-f`,
// `--monitor`, and `-t` / `--test-config`, plus these subcommands:
//
//   init         write the example configuration to config.yaml
//   start        start the server, optionally as a daemon
//   stop         stop the running daemon
//   reload       validate the configuration, then reload the daemon
//   pause        pause the running daemon
//   resume       resume the paused daemon
//   maintenance  show or switch maintenance mode (on|off)
//   restart      restart the daemon
//   status       show the daemon status
//   monitor      open the live monitor dashboard
//   server       aliases: server start|stop|reload|restart|status
//
// Subcommands injected by modules are merged at runtime: the module
// registry is built, every module registers its CLI commands, and those
// commands are added to the clap command before parsing.
//
// Exit codes: 0 success, 1 usage or configuration error, 2 runtime error.
//
// NOTE (integration contract): `start` calls
// `crate::http::server::run_server(ctx: Arc<AppContext>)`, which is
// implemented by the HTTP server builder. That function does not exist
// yet in this snapshot; the call below assumes this signature.

use crate::cli::daemon::{self, DaemonStatus};
use crate::cli::monitor;
use unified_core::config::AppConfig;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{events, HookBus};
use unified_core::modules::{CliRegistry, ModuleRegistry};
use clap::{Arg, ArgAction, ArgMatches, Command};
use nix::sys::signal::Signal;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Built-in subcommand names. A module command with one of these names
/// is ignored with a warning instead of colliding with a built-in.
const BUILTIN_COMMANDS: &[&str] = &[
    "init",
    "start",
    "stop",
    "reload",
    "pause",
    "resume",
    "maintenance",
    "restart",
    "status",
    "monitor",
    "server",
];

/// Binary entry point. Returns the process exit code.
pub fn run() -> i32 {
    let cli_registry = build_cli_registry();
    let mut command = build_command(&cli_registry);
    let matches = match command.try_get_matches_from_mut(std::env::args_os()) {
        Ok(matches) => matches,
        Err(error) => {
            let _ = error.print();
            // clap uses exit code 2 for errors and 0 for help/version;
            // this project uses 1 for usage errors.
            return if error.use_stderr() { 1 } else { 0 };
        }
    };

    let config_path = resolve_config_path(&matches);
    let test_config = matches.get_flag("test-config");
    let monitor_flag = matches.get_flag("monitor");
    let subcommand = matches
        .subcommand()
        .map(|(name, sub_matches)| (name.to_string(), sub_matches.clone()));

    if test_config {
        return do_test_config(&config_path);
    }
    if monitor_flag && subcommand.is_none() {
        return monitor::run_monitor();
    }

    match subcommand.as_ref().map(|(name, sub)| (name.as_str(), sub)) {
        None => {
            let _ = command.print_help();
            println!();
            0
        }
        Some(("init", _)) => do_init(&config_path),
        Some(("start", sub)) => do_start(&config_path, sub.get_flag("daemonize")),
        Some(("stop", _)) => do_stop(),
        Some(("reload", _)) => do_reload(&config_path),
        Some(("pause", _)) => do_pause(),
        Some(("resume", _)) => do_resume(),
        Some(("maintenance", sub)) => do_maintenance(sub.get_one::<String>("state")),
        Some(("restart", _)) => do_restart(&config_path),
        Some(("status", _)) => do_status(),
        Some(("monitor", _)) => monitor::run_monitor(),
        Some(("server", sub)) => dispatch_server(sub, &config_path),
        Some((name, sub)) => match cli_registry.dispatch(name, sub) {
            Some(Ok(())) => 0,
            Some(Err(error)) => {
                eprintln!("Command `{name}` failed: {error}");
                2
            }
            None => {
                eprintln!("Unknown subcommand: {name}");
                1
            }
        },
    }
}

/// Build the module registry, register every core module, build the
/// application context once, and collect the CLI commands injected by
/// modules. The context is rebuilt with the real configuration when the
/// server starts; building it here proves the module set boots before
/// argument parsing.
fn build_cli_registry() -> CliRegistry {
    let hooks = Arc::new(HookBus::new());
    let modules = Arc::new(ModuleRegistry::new());
    crate::modules::register_all(&modules);
    let config = AppConfig::load_default().unwrap_or_default();
    let _context = AppContext::new(config, hooks, modules.clone());
    let cli_registry = CliRegistry::new();
    modules.register_all_cli(&cli_registry);
    cli_registry
}

/// Build the root clap command, including module-injected subcommands.
fn build_command(cli_registry: &CliRegistry) -> Command {
    let mut command = Command::new("unified")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Unified Pentagonal Project — high-performance unified management system")
        .arg(
            Arg::new("config")
                .short('f')
                .long("config")
                .value_name("PATH")
                .global(true)
                .action(ArgAction::Set)
                .help("Path to the configuration file (default: config.yaml, or $UNIFIED_CONFIG_FILE)"),
        )
        .arg(
            Arg::new("monitor")
                .long("monitor")
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Open the live monitor dashboard (same as the monitor subcommand)"),
        )
        .arg(
            Arg::new("test-config")
                .short('t')
                .long("test-config")
                .action(ArgAction::SetTrue)
                .help("Test the configuration file and exit"),
        )
        .subcommand_required(false)
        .arg_required_else_help(false)
        .subcommand(
            Command::new("init")
                .about("Write the example configuration to config.yaml (does nothing when the file already exists)"),
        )
        .subcommand(start_command())
        .subcommand(Command::new("stop").about("Stop the running daemon"))
        .subcommand(
            Command::new("reload")
                .about("Validate the configuration, then reload the running daemon (refused when invalid)"),
        )
        .subcommand(Command::new("pause").about("Pause the running daemon"))
        .subcommand(Command::new("resume").about("Resume the paused daemon"))
        .subcommand(
            Command::new("maintenance")
                .about("Show or switch maintenance mode")
                .arg(
                    Arg::new("state")
                        .value_parser(["on", "off"])
                        .required(false)
                        .help("Maintenance mode: on or off (no value shows the current state)"),
                ),
        )
        .subcommand(Command::new("restart").about("Restart the daemon"))
        .subcommand(Command::new("status").about("Show the status of the running daemon"))
        .subcommand(Command::new("monitor").about("Open the live monitor dashboard"))
        .subcommand(
            Command::new("server")
                .about("Manage the Unified server (aliases for the top-level server commands)")
                .subcommand_required(true)
                .arg_required_else_help(true)
                .subcommand(start_command())
                .subcommand(Command::new("stop").about("Stop the running daemon"))
                .subcommand(
                    Command::new("reload")
                        .about("Validate the configuration, then reload the running daemon"),
                )
                .subcommand(Command::new("restart").about("Restart the daemon"))
                .subcommand(Command::new("status").about("Show the status of the running daemon")),
        );

    for module_command in cli_registry.build_commands() {
        if BUILTIN_COMMANDS.contains(&module_command.get_name()) {
            eprintln!(
                "Warning: module command `{}` conflicts with a built-in command and was ignored.",
                module_command.get_name()
            );
            continue;
        }
        command = command.subcommand(module_command);
    }
    command
}

/// The `start` subcommand definition, shared by the top level and by
/// `server start`.
fn start_command() -> Command {
    Command::new("start").about("Start the Unified server").arg(
        Arg::new("daemonize")
            .long("daemonize")
            .action(ArgAction::SetTrue)
            .help("Run the server as a background daemon"),
    )
}

/// Resolve the configuration path: `--config` / `-f` from the deepest
/// matching level, otherwise the default path (`$UNIFIED_CONFIG_FILE` or
/// `config.yaml`).
fn resolve_config_path(matches: &ArgMatches) -> PathBuf {
    let mut found: Option<String> = matches.get_one::<String>("config").cloned();
    if let Some((_, sub)) = matches.subcommand() {
        if let Some(value) = sub.get_one::<String>("config") {
            found = Some(value.clone());
        }
        if let Some((_, nested)) = sub.subcommand() {
            if let Some(value) = nested.get_one::<String>("config") {
                found = Some(value.clone());
            }
        }
    }
    found
        .map(PathBuf::from)
        .unwrap_or_else(AppConfig::default_path)
}

/// Dispatch the `server` subcommand aliases to the same handlers as the
/// top-level server commands.
fn dispatch_server(matches: &ArgMatches, config_path: &Path) -> i32 {
    match matches.subcommand() {
        Some(("start", sub)) => do_start(config_path, sub.get_flag("daemonize")),
        Some(("stop", _)) => do_stop(),
        Some(("reload", _)) => do_reload(config_path),
        Some(("restart", _)) => do_restart(config_path),
        Some(("status", _)) => do_status(),
        _ => {
            eprintln!("Usage: unified server <start|stop|reload|restart|status>");
            1
        }
    }
}

/// `unified -t [-f <path>]`: load the configuration and report every
/// validation problem. Exit 0 when valid, 1 otherwise.
fn do_test_config(path: &Path) -> i32 {
    match AppConfig::test(path) {
        Ok(problems) if problems.is_empty() => {
            println!("Configuration {} is valid.", path.display());
            0
        }
        Ok(problems) => {
            println!("Configuration {} is invalid:", path.display());
            for problem in problems {
                println!("  - {problem}");
            }
            1
        }
        Err(error) => {
            eprintln!("Cannot test configuration {}: {error}", path.display());
            1
        }
    }
}

/// `unified init`: write the example configuration to the config path
/// when the file does not exist yet.
fn do_init(path: &Path) -> i32 {
    if path.exists() {
        println!(
            "Configuration file {} already exists; leaving it unchanged.",
            path.display()
        );
        return 0;
    }
    let example = include_str!("../../config.example.yaml");
    match std::fs::write(path, example) {
        Ok(()) => {
            println!("Created {} from the example configuration.", path.display());
            println!("Edit it, then run `unified -t` to test it.");
            0
        }
        Err(error) => {
            eprintln!("Cannot write {}: {error}", path.display());
            2
        }
    }
}

/// `unified start [--daemonize]`: load and validate the configuration,
/// refuse to start when another daemon is already running, optionally
/// daemonize, then run the server in the foreground of this process (or
/// of the daemon process after the fork).
fn do_start(config_path: &Path, daemonize_flag: bool) -> i32 {
    if let Some(pid) = daemon::is_running() {
        eprintln!("Unified daemon is already running (PID {pid}).");
        eprintln!("Stop it first with `unified stop`, or check its status with `unified status`.");
        return 2;
    }
    let config = match AppConfig::load(config_path) {
        Ok(config) => config,
        Err(error) => {
            eprintln!(
                "Cannot load configuration from {}: {error}",
                config_path.display()
            );
            if !config_path.exists() {
                eprintln!("Run `unified init` to create a configuration file from the example.");
            }
            return 1;
        }
    };
    let problems = config.validate();
    if !problems.is_empty() {
        eprintln!("Configuration {} is invalid:", config_path.display());
        for problem in problems {
            eprintln!("  - {problem}");
        }
        eprintln!("Refusing to start. Fix the configuration or run `unified -t` to test it.");
        return 1;
    }
    crate::logging::init_logging(config.is_debug());
    println!(
        "Starting Unified server in {} mode on {}.",
        config.app.mode,
        config.app.tcp.join(", ")
    );
    if daemonize_flag {
        if let Err(error) = daemon::start_daemonize() {
            eprintln!("Cannot start daemon: {error}");
            return 2;
        }
        // From here on this is the daemon process; stdout and stderr go
        // to the log file.
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Cannot start the async runtime: {error}");
            return 2;
        }
    };
    match runtime.block_on(start_server(config, config_path.to_path_buf())) {
        Ok(()) => {
            println!("Unified server stopped.");
            0
        }
        Err(error) => {
            eprintln!("Server error: {error}");
            2
        }
    }
}

/// Build the application context, initialize modules, connect to the
/// database (best effort), run migrations, install the daemon signal
/// watcher, and run the HTTP server.
async fn start_server(config: AppConfig, config_path: PathBuf) -> UnifiedResult<()> {
    let hooks = Arc::new(HookBus::new());
    let modules = Arc::new(ModuleRegistry::new());
    crate::modules::register_all(&modules);
    modules.register_all_hooks(&hooks);
    let ctx = Arc::new(AppContext::new(config, hooks.clone(), modules.clone()));

    // Database: best effort. When a database is configured but cannot be
    // reached, report the error clearly and continue without one;
    // database-backed features stay unavailable instead of failing the
    // whole start. Migrations, on the other hand, must succeed when a
    // connection exists, because running against the wrong schema is
    // worse than not running at all.
    let database_name = ctx
        .config
        .read()
        .expect("config lock poisoned")
        .database
        .database
        .clone();
    if database_name.is_empty() {
        log::info!("No database configured; continuing without a database.");
    } else {
        let db_config = ctx.config.read().expect("config lock poisoned").clone();
        match unified_core::db::pool_from_config(&db_config) {
            Ok(pool) => {
                unified_core::db::run_migrations(&pool)?;
                ctx.set_db(pool);
                log::info!("Database connected and migrations applied.");
            }
            Err(error) => {
                eprintln!("Warning: cannot connect to the database: {error}");
                eprintln!(
                    "Continuing without a database; database-backed features will be unavailable."
                );
                log::warn!("Database unavailable, continuing without it: {error}");
            }
        }
    }

    modules.init_all(&ctx)?;

    // Record this process as the running daemon so `status`, `stop`,
    // `reload`, `pause`, `resume`, `maintenance`, and the monitor can
    // find it. This also covers foreground starts.
    let pid = std::process::id();
    daemon::write_pid_file(pid)?;
    daemon::write_status(&DaemonStatus::from_context(pid, &ctx))?;

    ctx.hooks.emit(
        events::APP_START,
        json!({ "mode": ctx.config.read().expect("config lock poisoned").app.mode }),
    );

    // Control signals (SIGHUP reload, SIGUSR1 pause, SIGUSR2 maintenance)
    // are handled by a background task for the lifetime of the server.
    tokio::spawn(daemon::watch_signals(ctx.clone(), config_path));

    // See the integration-contract note at the top of this file.
    let result = crate::http::server::run_server(ctx.clone()).await;

    ctx.hooks.emit(events::APP_STOP, json!({ "status": "stopped" }));
    daemon::remove_pid_file();
    daemon::remove_status_file();
    result
}

/// `unified stop`: stop the running daemon.
fn do_stop() -> i32 {
    match daemon::stop() {
        Ok(true) => {
            println!("Unified daemon stopped.");
            0
        }
        Ok(false) => {
            println!("Unified daemon is not running.");
            0
        }
        Err(error) => {
            eprintln!("Cannot stop daemon: {error}");
            2
        }
    }
}

/// `unified reload`: validate the configuration first and refuse to
/// reload on failure; otherwise send SIGHUP to the running daemon.
fn do_reload(config_path: &Path) -> i32 {
    let config = match AppConfig::load(config_path) {
        Ok(config) => config,
        Err(error) => {
            eprintln!(
                "Cannot load configuration from {}: {error}",
                config_path.display()
            );
            return 1;
        }
    };
    let problems = config.validate();
    if !problems.is_empty() {
        eprintln!("Configuration {} is invalid; reload refused:", config_path.display());
        for problem in problems {
            eprintln!("  - {problem}");
        }
        return 1;
    }
    match daemon::send_signal(Signal::SIGHUP) {
        Ok(true) => {
            println!("Reload signal sent to the running daemon.");
            0
        }
        Ok(false) => {
            println!("Configuration is valid, but no daemon is running.");
            0
        }
        Err(error) => {
            eprintln!("Cannot send reload signal: {error}");
            2
        }
    }
}

/// `unified pause`: send SIGUSR1 unless the daemon is already paused.
fn do_pause() -> i32 {
    if daemon::is_running().is_none() {
        eprintln!("Cannot pause: the daemon is not running. Start it with `unified start`.");
        return 2;
    }
    if daemon::read_status().is_some_and(|status| status.paused) {
        println!("Daemon is already paused.");
        return 0;
    }
    match daemon::send_signal(Signal::SIGUSR1) {
        Ok(true) => {
            println!("Pause signal sent to the daemon.");
            0
        }
        Ok(false) => {
            eprintln!("Cannot pause: the daemon is not running.");
            2
        }
        Err(error) => {
            eprintln!("Cannot pause daemon: {error}");
            2
        }
    }
}

/// `unified resume`: send SIGUSR1 unless the daemon is already running
/// normally.
fn do_resume() -> i32 {
    if daemon::is_running().is_none() {
        eprintln!("Cannot resume: the daemon is not running. Start it with `unified start`.");
        return 2;
    }
    if daemon::read_status().is_some_and(|status| !status.paused) {
        println!("Daemon is already running normally.");
        return 0;
    }
    match daemon::send_signal(Signal::SIGUSR1) {
        Ok(true) => {
            println!("Resume signal sent to the daemon.");
            0
        }
        Ok(false) => {
            eprintln!("Cannot resume: the daemon is not running.");
            2
        }
        Err(error) => {
            eprintln!("Cannot resume daemon: {error}");
            2
        }
    }
}

/// `unified maintenance [on|off]`: with no argument, show the current
/// state; otherwise send SIGUSR2 unless already in the requested state.
fn do_maintenance(state: Option<&String>) -> i32 {
    match state.map(String::as_str) {
        None => match daemon::read_status() {
            Some(status) => {
                println!(
                    "Maintenance mode is {}.",
                    if status.maintenance { "on" } else { "off" }
                );
                0
            }
            None => {
                println!(
                    "Maintenance mode is unknown: the daemon is not running or has not written a status file."
                );
                0
            }
        },
        Some("on") => set_maintenance(true),
        Some("off") => set_maintenance(false),
        Some(other) => {
            eprintln!("Invalid maintenance state: {other} (expected on or off)");
            1
        }
    }
}

fn set_maintenance(desired: bool) -> i32 {
    if daemon::is_running().is_none() {
        eprintln!(
            "Cannot change maintenance mode: the daemon is not running. Start it with `unified start`."
        );
        return 2;
    }
    if let Some(status) = daemon::read_status() {
        if status.maintenance == desired {
            println!(
                "Maintenance mode is already {}.",
                if desired { "on" } else { "off" }
            );
            return 0;
        }
    }
    match daemon::send_signal(Signal::SIGUSR2) {
        Ok(true) => {
            println!(
                "Maintenance mode {} signal sent to the daemon.",
                if desired { "enabled" } else { "disabled" }
            );
            0
        }
        Ok(false) => {
            eprintln!("Cannot change maintenance mode: the daemon is not running.");
            2
        }
        Err(error) => {
            eprintln!("Cannot change maintenance mode: {error}");
            2
        }
    }
}

/// `unified restart`: stop the daemon when running, then start a new
/// daemonized instance.
fn do_restart(config_path: &Path) -> i32 {
    match daemon::stop() {
        Ok(true) => println!("Daemon stopped."),
        Ok(false) => println!("Daemon was not running."),
        Err(error) => {
            eprintln!("Cannot stop daemon: {error}");
            return 2;
        }
    }
    do_start(config_path, true)
}

/// `unified status`: report whether the daemon is running, with PID,
/// uptime, mode, listeners, and pause / maintenance state.
fn do_status() -> i32 {
    match daemon::is_running() {
        Some(pid) => {
            println!("Unified daemon is running (PID {pid}).");
            match daemon::read_status() {
                Some(status) => {
                    println!("Version: {}", status.version);
                    println!("Mode: {}", status.mode);
                    println!(
                        "Uptime: {}",
                        daemon::format_uptime(daemon_uptime_seconds(&status))
                    );
                    println!("Listeners: {}", status.listeners.join(", "));
                    println!("Workers: {}", status.workers);
                    println!("Paused: {}", if status.paused { "yes" } else { "no" });
                    println!(
                        "Maintenance: {}",
                        if status.maintenance { "on" } else { "off" }
                    );
                    println!("HTTP connections: {}", status.http_connections);
                    println!("WebSocket connections: {}", status.websocket_connections);
                    println!("Total requests: {}", status.total_requests);
                }
                None => {
                    println!("Status file not found; the daemon may still be starting.");
                }
            }
            0
        }
        None => {
            println!("Unified daemon is not running.");
            0
        }
    }
}

/// Uptime of the daemon in seconds, computed from the start timestamp in
/// the status file when it parses, otherwise from the stored snapshot.
fn daemon_uptime_seconds(status: &DaemonStatus) -> u64 {
    if let Ok(started) = chrono::DateTime::parse_from_rfc3339(&status.started_at) {
        let elapsed = chrono::Utc::now()
            .signed_duration_since(started)
            .num_seconds();
        if elapsed >= 0 {
            return elapsed as u64;
        }
    }
    status.uptime_seconds
}
