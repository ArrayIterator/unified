// Logging and tracing setup.

use tracing_subscriber::{fmt, prelude::*, EnvFilter, Registry};

/// Initialize logging. In development mode the log level is `debug`,
/// otherwise `info`. The `RUST_LOG` environment variable overrides both.
///
/// The subscriber is a `Registry` with two layers: the `fmt` layer
/// writes human-readable lines to stdout, and the Logs module's
/// `LogsLayer` forwards every tracing event into the shared log
/// buffer, so application logs written with `tracing` or `log` also
/// appear in the log viewer. `LogsLayer` never logs anything itself,
/// so it cannot recurse.
pub fn init_logging(debug: bool) {
    let default_level = if debug { "debug" } else { "info" };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("unified={default_level}")));
    let subscriber = Registry::default()
        .with(fmt::layer().with_target(false).with_filter(filter))
        .with(crate::modules::logs::LogsLayer);
    let _ = tracing::subscriber::set_global_default(subscriber);
}
