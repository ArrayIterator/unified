// Tenant Destroy module - the irreversible end of a tenant lifecycle.
//
// Destroy is NEVER one click. A tenant is destroyed only through the
// full flow below, and every step is reversible until the final schema
// drop. The state machine is:
//
//   requested
//     -> export_pending            (full export requested, proof pending)
//     -> export_ready              (export completed and downloadable)
//     -> awaiting_confirmation     (typed confirmation attempted / pending)
//     -> awaiting_second_approval  (only when the settings rule requires it)
//     -> suspended_grace           (tenant suspended, grace clock running)
//     -> destroyed_pending_external_cleanup
//     -> destroyed                 (terminal)
//   Any non-terminal state except `destroyed_pending_external_cleanup`
//   can move to `cancelled` (terminal). Cancellation during
//   `suspended_grace` resumes the tenant.
//
// The full module documentation is preserved at the top of `lifecycle.rs`.

// Tenant Destroy module root.
//
// The module is split into one file per concern under
// `src/modules/tenant_destroy/`. The public API is unchanged: every item
// previously reachable as `crate::modules::tenant_destroy::Item` is
// re-exported here.

pub mod constants;
pub mod settings;
pub mod types;
pub mod store;
pub mod validation;
pub mod lifecycle;
mod requests;
mod handlers;
pub mod module;

pub use self::constants::*;
pub use self::settings::*;
pub use self::types::*;
pub use self::store::*;
pub use self::validation::*;
pub use self::lifecycle::*;
pub use self::module::*;
pub(crate) use self::constants::*;
pub(crate) use self::settings::*;
pub(crate) use self::types::*;
pub(crate) use self::store::*;
pub(crate) use self::validation::*;
pub(crate) use self::lifecycle::*;
pub(crate) use self::requests::*;
pub(crate) use self::handlers::*;
pub(crate) use self::module::*;
