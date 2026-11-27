// Activity Timeline module.
//
// A unified activity feed that combines the records store, the audit
// trail, and task / comment events into one newest-first feed. This
// module does NOT duplicate the records store and does NOT read it
// directly: it keeps a lightweight in-memory timeline cache fed by
// subscribing to the same event names the records module subscribes
// to (verified by reading `src/modules/records.rs`), plus the task
// events and the verified failure events listed below. Each entry is
// derived from the event payload with the same field conventions the
// audit and records modules use (`actor_id` / `actor` / `user_id` for
// the actor, `entity_type` / `entity_id` / `summary`, `resource` as
// `type:id`, `ip` when present).
//
// Event sources (all names verified in the tree):
//   - The audit / records event set: the well-known constants in
//     `unified_core::hooks::events` (`user.created`, `user.updated`,
//     `role.created`, `license.issued`, `license.revoked`,
//     `module.enabled`, `module.disabled`, `ssl.renew`, `auth.failed`,
//     `geo.lookup`) plus the plain string events `order.created`,
//     `invoice.paid`, `cloudflare.dns.changed`,
//     `cloudflare.cache.purged`, `cloudflare.connected`,
//     `google.sso.completed`, `google.drive.link`,
//     `google.drive.unlink`, `user.deleted`.
//   - The task events emitted by the tasks module: `task.created`,
//     `task.assigned`, `task.status.changed`, `task.completed`,
//     `task.comment.added`.
//   - Verified failure events: `security.auth.failed`
//     (`core::hardening::security_events::AUTH_FAILED`),
//     `security.bruteforce.locked`, `cluster.sync.failed`
//     (`modules::clusters::CLUSTER_SYNC_FAILED`), `payment.failed`
//     (`modules::payments::PAYMENT_FAILED`).
//
// The cache is bounded (see `MAX_ENTRIES`): the oldest entries are
// dropped when the cap is hit, so the feed is a rolling window, not a
// durable log - the audit log and the records store remain the durable
// sources of truth.

pub mod ai_capabilities;
mod handlers;
mod helpers;
pub mod module;
pub mod state;
pub mod types;

// Re-exported from `types`.
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::CountEntry;
pub use self::types::DayGroup;
pub use self::types::TimelineCounts;
pub use self::types::TimelineEntry;
pub use self::types::MAX_ENTRIES;
// Re-exported from `state`.
pub use self::state::TimelineState;
// Re-exported from `module`.
pub use self::module::register;
pub use self::module::TimelineModule;
