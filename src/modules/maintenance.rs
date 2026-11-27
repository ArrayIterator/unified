// Maintenance Windows module.
//
// Scheduled maintenance windows with automatic activation and
// deactivation by time, a global maintenance-mode flag, subscriber
// notifications through the hook bus, and a full history log.
//
// How time-based transitions work (there is no periodic scheduler
// tick event in the codebase to subscribe to):
//
//   - Every handler in this module calls `refresh` first, so a window
//     whose start or end time has passed changes state on the next
//     request that touches the module.
//   - `POST /api/v1/maintenance/tick` runs the same refresh on demand.
//     Point a scheduler job at it (or emit the `maintenance.check`
//     hook, which this module listens to) once a minute for prompt
//     transitions without waiting for traffic.
//   - While a window is active the module sets the shared
//     `AppContext` maintenance flag (`ctx.set_maintenance(true)`).
//     The HTTP server already answers 503 for every route except
//     `/health` and the ACME challenge while that flag is set; the
//     integrator snippet for exempting the public status page lives
//     in the coordinator report.
//
// Events emitted on the hook bus:
//   - `maintenance.scheduled` - a window was created.
//   - `maintenance.started`   - a window became active.
//   - `maintenance.ended`     - a window finished (time or manual).
//   - `maintenance.cancelled` - a window was cancelled.
//   - `maintenance.check`     - listened to; runs a refresh.
//   - `app.maintenance` (core event) - the global flag changed.
//
// Subscriber notification: the notifications module listens to
// `maintenance.scheduled`, `maintenance.started`, and
// `maintenance.ended` when a handler is registered for them; this
// module also forwards each lifecycle event as a
// `notification.requested` event (kind `maintenance`) so subscribers
// are notified through the standard channel.

// Maintenance module root.
//
// The module is split into one file per concern under
// `src/modules/maintenance/`. The public API is unchanged: every item
// previously reachable as `crate::modules::maintenance::Item` is
// re-exported here.

pub mod ai_capabilities;
pub mod types;
mod validation;
mod handlers;
mod hooks;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::*;
pub use self::module::*;
pub(crate) use self::types::*;
pub(crate) use self::validation::*;
pub(crate) use self::handlers::*;
pub(crate) use self::hooks::*;
pub(crate) use self::module::*;
