// Records module.
//
// A queryable system-records store that sits behind the audit trail:
// who did what, when, on which entity. Records are created only by
// listening to events on the hook bus - there is no public create
// endpoint - and they are append-only: no update or delete endpoints
// exist, only a retention purge that requires explicit confirmation.
//
// Event sources (verified by reading `src/modules/audit.rs` and
// crates/unified-core/src/hooks.rs`):
//
//   - Every event the audit module subscribes to is subscribed here
//     too, using the same names: the well-known constants in
//     `unified_core::hooks::events` (`user.created`, `user.updated`,
//     `role.created`, `license.issued`, `license.revoked`,
//     `module.enabled`, `module.disabled`, `ssl.renew`, `auth.failed`,
//     `geo.lookup`) plus the plain string events the audit module
//     lists (`order.created`, `invoice.paid`, `cloudflare.dns.changed`,
//     `cloudflare.cache.purged`, `cloudflare.connected`,
//     `google.sso.completed`, `google.drive.link`,
//     `google.drive.unlink`).
//   - The task events emitted by the tasks module (`task.created`,
//     `task.assigned`, `task.status.changed`, `task.completed`,
//     `task.comment.added`) and `user.deleted` (emitted by the users
//     module as a plain string event).
//
// The audit module itself is NOT edited: this module registers its
// own listeners for the same event names. A record is derived from
// the event payload with the same field conventions the audit module
// uses (`actor_id` / `actor` / `user_id` for the actor, `resource` as
// `type:id` when present, `ip` when present), plus the explicit
// `entity_type` / `entity_id` / `summary` keys the tasks module
// emits. Anything the audit module records therefore also lands here,
// in queryable form.
//
// Persistence: records live in an in-memory store (the same approach
// the audit module uses). The Diesel table for durable persistence
// does not exist in crates/unified-core/src/db/schema.rs` yet; the migration SQL and the
// schema / model entries are supplied to the integrator separately.
// Nothing in this file references a table that does not exist yet, so
// it compiles against the current schema.
//
// Retention: records older than the configured retention are removed
// only by the explicit purge endpoint. The retention comes from the
// `records` config section (`AppConfig.records.retention_days`,
// default 365 days); `DEFAULT_RETENTION_DAYS` below is only the
// fallback when the configured value is absent (zero).

pub mod ai_capabilities;
mod handlers;
pub mod module;
mod service;
pub mod state;
pub mod types;

pub(crate) use self::handlers::counts_by_action;
pub(crate) use self::handlers::counts_by_entity;
pub(crate) use self::handlers::export_records;
pub(crate) use self::handlers::get_record;
pub(crate) use self::handlers::list_records;
pub(crate) use self::handlers::purge_records;
pub(crate) use self::handlers::retention_view;
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::module::RecordsModule;
pub use self::module::register;
pub(crate) use self::service::count_by;
pub(crate) use self::service::derive_entity;
pub(crate) use self::service::retention_days;
pub use self::state::RecordsState;
pub use self::types::CountEntry;
pub use self::types::DEFAULT_RETENTION_DAYS;
pub use self::types::PurgeInput;
pub(crate) use self::types::RECORDED_EVENTS;
pub use self::types::RetentionView;
pub use self::types::SystemRecord;
