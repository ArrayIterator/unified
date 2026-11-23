// PLM module — product lifecycle management: data-driven
// lifecycle stages, product versions with revision math,
// engineering change orders with a status machine and
// data-driven approval steps, versioned BOM snapshots, and a
// full audit trail.
//
// Routes live under `/api/v1/plm`:
//
//   Stages:
//     GET/POST /stages — list stages / create a stage.
//     POST /stages/seed — seed the default stage set
//       (idempotent; existing slugs are skipped).
//     GET/PATCH/DELETE /stages/{stage_id}.
//   Versions:
//     GET/POST /versions — list versions / create a version
//       (explicit numbers, or a major/minor/patch bump off
//       the superseded revision; otherwise 1.0.0).
//     GET/PATCH/DELETE /versions/{version_id}.
//     POST /versions/{version_id}/transition — move through
//       the stage graph (allowed transitions + approval gate).
//     GET  /versions/{version_id}/diff?against= —
//       field-by-field diff metadata.
//     POST /versions/{version_id}/approvals — record a
//       stage-entry approval.
//   ECOs:
//     GET/POST /ecos — list ECOs / create an ECO.
//     GET/PATCH/DELETE /ecos/{eco_id}.
//     POST /ecos/{eco_id}/transition — draft -> review ->
//       approved -> implemented -> closed, plus rejected
//       (review/approved -> rejected, rejected -> draft).
//     GET/POST /ecos/{eco_id}/approvals — list / decide one
//       approval step.
//   BOM versions:
//     GET/POST /bom-versions — list / version a BOM snapshot.
//     GET/DELETE /bom-versions/{bom_version_id}.
//   Settings and audit:
//     GET  /settings — per-tenant PLM settings.
//     PATCH /settings (manager).
//     GET  /audit — the audit trail.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory stage, version, ECO, approval, BOM-version, and
// audit management; lifecycle stages as data (an admin defines
// them, their order, their allowed transitions, and how many
// approvals entering a stage requires) with a default set
// seeded on request through `/stages/seed`; product versions
// with major.minor.patch revisions, explicit numbers or a
// bump off the superseded revision, effective dates, a
// single current revision per product, and field-by-field
// diff metadata; ECOs with a status machine enforced by a
// single pure transition function, data-driven ECO types from
// per-tenant settings, impact level and assessment, affected
// products/versions and linked documents by id only, and
// ordered approval steps by approver role (approval requires
// every required step approved; a step is decided once, by a
// caller holding the step's role); BOM snapshots with
// strictly increasing version numbers per BOM, gated on an
// approved or implemented ECO when linked; an audit trail
// entry for every creation, transition, approval, and
// versioning; tenant scoping on every row with cross-tenant
// access reading as 404, never 403. What is NOT built here:
// durable storage until the migration runs (the DashMap
// stores are authoritative; migration
// `2026-11-22-000113_plm` is the persistence target);
// products themselves (the products module owns them — this
// module links them by id and marks versions
// `product_deleted` from `product.deleted`); BOMs themselves
// (the manufacturing module owns them — this module stores
// snapshots by BOM id and links manufacturing versions from
// `manufacturing.bom.version.created`); documents themselves
// (the documents module owns them — ECOs link them by id
// only); and email / push delivery (the notifications module
// owns delivery — this module only emits
// `notification.requested`).
//
// Actor identity: the caller is the server-verified caller from
// `unified_core::caller::require_caller` (user id plus
// server-resolved role, both required for capability checks),
// exactly like the rental and forums modules. Capability
// checks use the `role.check` hook and fail closed (no answer
// means not allowed). All routes are declared
// `RouteAuth::Authenticated`.
//
// Events emitted on the hook bus:
//   - `plm.stage.created`, `plm.stage.updated`,
//     `plm.stage.deleted`, `plm.stage.seeded`
//   - `plm.version.created`, `plm.stage.changed`
//     (version stage moves), `plm.approval.decided`
//   - `plm.eco.created`, `plm.eco.status_changed`,
//     `plm.eco.approved`, `plm.eco.implemented`
//   - `plm.bom.versioned`
//   - `notification.requested` (ECO implemented, in the
//     notifications module's payload shape).
// Events this module listens to:
//   - `user.deleted` — anonymizes actor fields on versions,
//     ECOs, approvals, BOM versions, and audit entries to
//     `deleted-user`.
//   - `product.deleted` — marks the product's versions
//     `product_deleted` (history is kept, not removed) and
//     writes an audit entry per version.
//   - `manufacturing.bom.version.created` — links the
//     manufacturing version to a matching PLM BOM snapshot
//     with an audit entry.

pub mod ai_capabilities;
mod handlers_admin;
mod handlers_bom;
mod handlers_ecos;
mod handlers_stages;
mod handlers_versions;
mod helpers;
mod hooks;
mod module;
pub mod state;
pub mod types;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::module::register;
pub use self::module::PlmModule;
pub use self::state::PlmState;
pub use self::types::ApprovalDecision;
pub use self::types::ApprovalStep;
pub use self::types::CreateApprovalRequest;
pub use self::types::CreateBomVersionRequest;
pub use self::types::CreateEcoRequest;
pub use self::types::CreateStageRequest;
pub use self::types::CreateVersionRequest;
pub use self::types::EcoStatus;
pub use self::types::EcoTransitionRequest;
pub use self::types::PlmApproval;
pub use self::types::PlmAuditEntry;
pub use self::types::PlmBomVersion;
pub use self::types::PlmEco;
pub use self::types::PlmSettings;
pub use self::types::PlmStage;
pub use self::types::PlmVersion;
pub use self::types::UpdateEcoRequest;
pub use self::types::UpdateSettingsRequest;
pub use self::types::UpdateStageRequest;
pub use self::types::UpdateVersionRequest;
pub use self::types::VersionTransitionRequest;
