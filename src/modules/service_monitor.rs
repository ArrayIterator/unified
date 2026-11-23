// Service Monitor module.
//
// Tracks the health of the running process and of the services the
// application depends on (HTTP, TLS, database, Redis, Geo). It also owns
// the configuration reload endpoint
// (`POST /api/v1/service-monitor/reload`), which validates the new
// configuration before applying it -- a failing configuration never
// replaces the running one. The public `/health` endpoint is registered
// by the HTTP server itself, not by this module. System data comes from a
// shared snapshot refreshed at most once a second, so the status, info
// and processes endpoints stay cheap under load. The service probes
// run on `GET /api/v1/service-monitor/services`: when a probe finds a
// service `down` that was not `down` on the previous probe, the module
// emits `cluster.health.changed` (the established unhealthy-transition
// event, shared with the clusters module and subscribed to by the
// notifications module) so the outage alerts instead of only showing
// up in the response body. Recovery transitions emit nothing: the
// notifications module only alerts on the unhealthy transition.
//
// This module also owns the external library checks: every module may
// declare the system libraries / binaries it needs
// (`Module::requires_external_libraries`), and this module checks them
// locally -- and, on request, on remote servers over SSH -- reporting
// each library as ok, missing, or outdated, and emitting
// `library.check.failed` for every failure so notifications and audit
// can pick it up. The latest results are cached and surfaced in the
// status response and through the `/libraries` endpoints below.

pub mod ai_capabilities;
pub mod types;
pub mod system;
pub mod libraries;
pub mod ssh;
pub mod remote;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::*;
pub use self::system::*;
pub use self::libraries::*;
pub use self::ssh::*;
pub use self::remote::*;
pub use self::module::*;
