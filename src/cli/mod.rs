// Command line interface and daemon control.
//
// `run()` is the binary entry point: it parses arguments with clap,
// merges the subcommands injected by modules, and dispatches to the
// server, daemon, and monitor commands. Implemented in the sibling files
// of this directory.

pub mod commands;
pub mod daemon;
pub mod monitor;

/// Binary entry point. Returns the process exit code.
pub fn run() -> i32 {
    commands::run()
}
