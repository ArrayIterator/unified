// load_balancer -- Event names and default limits.

// Load Balancer module.
//
// Balancers front a pool of backends and route traffic to them. A
// backend references either a provider-registry server/container
// (`source_kind: "provider"`, `source_id` is the provider connection
// id plus server id), a cluster node (`source_kind: "cluster"`,
// `source_id` is the cluster id plus node id), or a plain address
// (`source_kind: "address"`). The reference is stored as typed
// fields only: this module never imports the provider registry or
// the cluster module. Liveness is resolved through health checks,
// and cluster health is observed through hook events
// (`cluster.node.unhealthy`, `cluster.node.healthy`,
// `cluster.node.removed`) emitted by the cluster module.
//
// Balancing strategies: round-robin, least-connections, weighted,
// IP hash (consistent hashing). Selection is a pure function
// (`select_backend`) so it can be unit-tested without live traffic;
// `POST /{id}/preview` exposes the same selection over HTTP.
//
// Health checks probe each backend (HTTP, HTTPS, or TCP) on a
// background
// sweeper thread. Unhealthy backends leave the rotation and a
// `lb.backend.unhealthy` event is emitted; recovered backends
// re-enter the rotation (`lb.backend.healthy`). When every backend
// of a balancer is unhealthy the balancer enters the degraded state,
// routing answers with a JSON 503 through the standard error
// envelope, and `lb.failover` is emitted.
//
// TLS termination: a balancer with `tls_mode: "terminate"`
// references a domain whose certificate lives in the shared
// `SslStorage` (`unified_core::tls`). This module does not own TLS:
// `tls_certificate_status` performs the lookup against a storage
// handle using the real `SslStorage` API (`lookup`, `cert_expiry`,
// `certificate_names`), and the HTTP handlers validate the domain
// against the configured certificates in `app.ssl.domains`. The
// storage handle is built once in `http::server::run_server`, attached
// to the context with `AppContext::set_tls`, and read back here with
// `AppContext::tls()`; when no handle is attached the TLS endpoint
// falls back to the config-only status.
//
// State is kept in memory (per-process `DashMap` stores). The
// migration SQL and Diesel schema entries for durable persistence
// are documented at the bottom of this file for the integrator;
// until they are applied, balancers, backends, and counters reset
// on restart.
//
// Routes under `/api/v1/load-balancer`:
//
//   Balancers:
//     GET    /                              list (pagination/filter)
//     POST   /                              create
//     GET    /{id}                          get one
//     PUT    /{id}                          update
//     DELETE /{id}                          delete (with its backends)
//     POST   /{id}/enable                   enable
//     POST   /{id}/disable                  disable
//     GET    /{id}/route?client_ip=...      route one request (simulated;
//                                            counts toward requests_routed
//                                            only; it books no active
//                                            connection because a simulated
//                                            request has no close event)
//     POST   /{id}/preview                  preview selection for a client IP
//     GET    /{id}/tls                      TLS termination status
//     POST   /{id}/check-now                probe every backend now
//     GET    /{id}/stats                    balancer + backend stats
//     POST   /{id}/stats/reset              reset balancer + backend stats
//   Backends:
//     GET    /{id}/backends                 list (pagination/filter)
//     POST   /{id}/backends                 create
//     GET    /{id}/backends/{backend_id}     get one
//     PUT    /{id}/backends/{backend_id}     update
//     DELETE /{id}/backends/{backend_id}     delete
//     POST   /{id}/backends/{backend_id}/enable
//     POST   /{id}/backends/{backend_id}/disable
//     POST   /{id}/backends/{backend_id}/drain
//     POST   /{id}/backends/{backend_id}/undrain
//     POST   /{id}/backends/{backend_id}/check-now
//     GET    /{id}/backends/{backend_id}/stats
//     POST   /{id}/backends/{backend_id}/stats/reset
//     POST   /{id}/backends/{backend_id}/connections/release
//
// Hooks emitted: `lb.created`, `lb.updated`, `lb.backend.added`,
// `lb.backend.removed`, `lb.backend.unhealthy`, `lb.backend.healthy`,
// `lb.failover`, `lb.recovered`, `lb.strategy.changed`.
// Hooks observed: `ssl.swap`, `ssl.renew` (TLS status cache),
// `cluster.node.unhealthy`, `cluster.node.healthy`,
// `cluster.node.removed` (cluster backends only).

// ---------------------------------------------------------------------------
// Event names.
// ---------------------------------------------------------------------------

pub const LB_CREATED: &str = "lb.created";

pub const LB_UPDATED: &str = "lb.updated";

pub const LB_BACKEND_ADDED: &str = "lb.backend.added";

pub const LB_BACKEND_REMOVED: &str = "lb.backend.removed";

pub const LB_BACKEND_UNHEALTHY: &str = "lb.backend.unhealthy";

pub const LB_BACKEND_HEALTHY: &str = "lb.backend.healthy";

pub const LB_FAILOVER: &str = "lb.failover";

/// Emitted when a balancer recovers from `Degraded` to `Active`
/// (at least one backend is eligible again).
pub const LB_RECOVERED: &str = "lb.recovered";

pub const LB_STRATEGY_CHANGED: &str = "lb.strategy.changed";

/// Capabilities enforced on the handlers (same fail-closed
/// `role.check` hook pattern as the web3 and procurement modules).
/// Reads list, fetch, preview, and report state; anything that
/// creates, changes, deletes, probes, resets, routes, or releases
/// state requires `load_balancer.manage`.
pub const PERM_LB_READ: &str = "load_balancer.read";

pub const PERM_LB_MANAGE: &str = "load_balancer.manage";

/// Virtual nodes per backend on the IP-hash consistent-hash ring.
/// One ring point per backend spreads traffic lumpily; 128 points
/// per backend evens the distribution out.
pub(crate) const IP_HASH_VIRTUAL_NODES: usize = 128;

/// Cluster health events observed by this module. These names are the
/// contract with the cluster module: it emits them when a node changes
/// health or is removed. No cluster event exists in
/// `unified_core::hooks::events` yet, so the names live here until the
/// cluster module (or core) adopts them.
pub const CLUSTER_NODE_UNHEALTHY: &str = "cluster.node.unhealthy";

pub const CLUSTER_NODE_HEALTHY: &str = "cluster.node.healthy";

pub const CLUSTER_NODE_REMOVED: &str = "cluster.node.removed";

// ---------------------------------------------------------------------------
// Defaults.
// ---------------------------------------------------------------------------

pub(crate) const DEFAULT_HEALTH_PATH: &str = "/health";

pub(crate) const DEFAULT_HEALTH_INTERVAL_SECS: u64 = 30;

pub(crate) const DEFAULT_HEALTH_TIMEOUT_SECS: u64 = 5;

pub(crate) const DEFAULT_HEALTHY_THRESHOLD: u32 = 2;

pub(crate) const DEFAULT_UNHEALTHY_THRESHOLD: u32 = 3;

pub(crate) const DEFAULT_WEIGHT: u32 = 1;

pub(crate) const MAX_WEIGHT: u32 = 100;

pub(crate) const MIN_HEALTH_INTERVAL_SECS: u64 = 5;

pub(crate) const MAX_HEALTH_INTERVAL_SECS: u64 = 3600;

pub(crate) const MAX_HEALTH_TIMEOUT_SECS: u64 = 60;

pub(crate) const MAX_THRESHOLD: u32 = 10;

pub(crate) const SWEEP_TICK_SECS: u64 = 1;

pub(crate) fn default_true() -> bool {
    true
}

pub(crate) fn default_weight() -> u32 {
    DEFAULT_WEIGHT
}
