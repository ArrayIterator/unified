// Load Balancer module: balancers over pools of backends with
// round-robin, least-connections, weighted and IP-hash selection,
// health checks with automatic failover, and TLS termination.
// Split into one file per concern; this file only declares the
// submodules and re-exports the public API.

pub mod constants;
pub mod handlers_backends;
pub mod handlers_balancers;
pub mod health;
pub mod hooks;
pub mod module;
pub mod requests;
pub mod selection;
pub mod store;
#[cfg(test)]
mod tests;
pub mod tls;
pub mod types;
pub mod validation;
pub mod views;

// Re-exported from constants.rs.
pub use self::constants::CLUSTER_NODE_HEALTHY;
pub use self::constants::CLUSTER_NODE_REMOVED;
pub use self::constants::CLUSTER_NODE_UNHEALTHY;
pub use self::constants::LB_BACKEND_ADDED;
pub use self::constants::LB_BACKEND_HEALTHY;
pub use self::constants::LB_BACKEND_REMOVED;
pub use self::constants::LB_BACKEND_UNHEALTHY;
pub use self::constants::LB_CREATED;
pub use self::constants::LB_FAILOVER;
pub use self::constants::LB_STRATEGY_CHANGED;
pub use self::constants::LB_UPDATED;
// Re-exported from types.rs.
pub use self::types::Backend;
pub use self::types::BackendSourceKind;
pub use self::types::BackendStats;
pub use self::types::BalancerStatus;
pub use self::types::CachedTlsStatus;
pub use self::types::HealthCheckConfig;
pub use self::types::HealthCheckKind;
pub use self::types::HealthState;
pub use self::types::LoadBalancer;
pub use self::types::Strategy;
pub use self::types::TlsMode;
// Re-exported from store.rs.
pub use self::store::note_backend_connection_closed;
pub use self::store::note_backend_connection_open;
// Re-exported from validation.rs.
pub use self::validation::parse_host_port;
// Re-exported from selection.rs.
pub use self::selection::eligible_backends;
pub use self::selection::select_backend;
// Re-exported from tls.rs.
pub use self::tls::TlsCertificateInfo;
pub use self::tls::tls_certificate_status;
// Re-exported from health.rs.
pub use self::health::ProbeOutcome;
pub use self::health::probe_backend;
// Re-exported from module.rs.
pub use self::module::LoadBalancerModule;
pub use self::module::register;
