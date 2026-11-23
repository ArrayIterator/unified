// network module -- Process-wide connection counters.

use std::sync::atomic::Ordering;
use std::sync::OnceLock;

use super::*;

pub(crate) static CONNECTION_STATS: OnceLock<ConnectionStats> = OnceLock::new();

pub fn connection_stats() -> &'static ConnectionStats {
    CONNECTION_STATS.get_or_init(ConnectionStats::new)
}

/// Record a newly accepted connection. `kind` is `"http"` or `"websocket"`.
pub fn note_connection_open(kind: &str) {
    let stats = connection_stats();
    match kind {
        "websocket" => stats.websocket.fetch_add(1, Ordering::SeqCst),
        _ => stats.http.fetch_add(1, Ordering::SeqCst),
    };
    stats.total_accepted.fetch_add(1, Ordering::SeqCst);
}

/// Record a closed connection. `kind` is `"http"` or `"websocket"`.
pub fn note_connection_closed(kind: &str) {
    let stats = connection_stats();
    match kind {
        "websocket" => {
            stats.websocket.fetch_sub(1, Ordering::SeqCst);
        }
        _ => {
            stats.http.fetch_sub(1, Ordering::SeqCst);
        }
    };
}
