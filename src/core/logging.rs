// Logging and tracing setup.

use tracing_subscriber::{fmt, EnvFilter};

/// Initialize logging. In development mode the log level is `debug`,
/// otherwise `info`. The `RUST_LOG` environment variable overrides both.
pub fn init_logging(debug: bool) {
    let default_level = if debug { "debug" } else { "info" };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("unified={default_level}")));
    let _ = fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}
