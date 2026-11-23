#![recursion_limit = "1024"]
// Unified Pentagonal Project — binary entry point.
//
// The binary is assembled by the CLI module: it parses arguments, loads and
// validates the configuration, and dispatches to the server, daemon, or
// monitor commands. All behaviour is reachable through `crate::cli::run`.

mod auth;
mod cli;
// Feature-gated on purpose (not a plain `mod gateway;`): the gateway
// module references the load balancer module, which is not declared
// in `src/modules/mod.rs` yet, so an unconditional declaration would
// break the default build. Once that declaration lands the gate can
// stay — the gateway's proxy service is itself `#[cfg(feature =
// "gateway")]` inside the module, and the default build must link no
// Pingora code.
#[cfg(feature = "gateway")]
mod gateway;
mod http;
mod logging;
mod modules;
mod themes;

fn main() {
    std::process::exit(crate::cli::run());
}
