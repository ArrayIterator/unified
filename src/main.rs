// Unified Pentagonal Project — binary entry point.
//
// The binary is assembled by the CLI module: it parses arguments, loads and
// validates the configuration, and dispatches to the server, daemon, or
// monitor commands. All behaviour is reachable through `crate::cli::run`.

mod auth;
mod cli;
mod core;
mod db;
mod http;
mod modules;
mod themes;

fn main() {
    std::process::exit(crate::cli::run());
}
