// Extensions module -- the Modules / Extensions Manager.
//
// Lists every registered module with its enabled state and dependency
// information, enables and disables modules, validates the module
// dependency graph, and lists the hook events registered on the bus.
// Disabling a core module is refused (409). The enabled state is kept in
// memory; it is advisory -- it gates the state reported by this API and
// the `module.enabled` / `module.disabled` events, but it does not
// unregister already-mounted routes. Full dynamic unload (route rebuild
// without a restart) is a follow-up; see the note in `disable_with_check`.
//
// Dependency behaviour (see `unified_core::modules`):
//   - Enabling a module auto-activates its required modules first,
//     transitively, in dependency order. Each auto-activated module
//     gets a `module.enabled` event plus a `module.auto_activated`
//     event, and the enable response lists them.
//   - Disabling a module that other enabled modules depend on is
//     refused with a 409 that lists the dependents, unless the caller
//     passes `?force=true`, in which case the dependents are disabled
//     too (dependents first) and listed in the response.
//   - `GET /extensions/{id}/requirements` reports the full requirement
//     check for one module before it is enabled, and
//     `GET /extensions/dependencies` reports the whole-graph validation.

pub mod module;
pub mod types;
mod service;
pub mod handlers;

pub use self::module::*;
pub use self::types::*;
pub use self::service::*;
pub use self::handlers::*;
