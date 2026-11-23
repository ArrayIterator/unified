// Audit Log module.
//
// Append-only log of major events. Entries are created by this module
// itself from hook subscriptions — there is no public create endpoint —
// and are immutable: no update or delete endpoints exist. Interconnection
// happens only through the hook bus.

pub mod ai_capabilities;
pub mod types;
mod constants;
pub mod state;
mod helpers;
mod handlers;
pub mod routes;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::AuditEntry;
pub use self::state::AuditState;
pub use self::routes::AuditModule;
pub use self::routes::register;
