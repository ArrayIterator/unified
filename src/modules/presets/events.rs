//! Presets + Setup Wizard module. — Events emitted by this module. Every payload is audit-shaped: `actor_id`, `action`, `target_type`, `target_id`, optional `company_id`, and `details`, plus `recorded_at`, so the audit module can record them once it subscribes to these names.
//!
//! Split from `src/modules/presets.rs`; behaviour unchanged.

use super::apply::*;
use super::helpers::*;
use super::identity::*;
use super::lookups::*;
use super::preset_handlers::*;
use super::records::*;
use super::requests::*;
use super::seeds::*;
use super::stores::*;
use super::wizard_handlers::*;
use super::wizard_steps::*;

/// A preset definition was created. Payload: audit shape, details
/// carry the preset slug and business type.
pub const PRESET_CREATED: &str = "preset.created";
/// A preset definition was edited. Payload: audit shape.
pub const PRESET_UPDATED: &str = "preset.updated";
/// A preset definition was archived. Payload: audit shape.
pub const PRESET_ARCHIVED: &str = "preset.archived";
/// A preset definition was cloned. Payload: audit shape, details
/// carry `source_preset_id`.
pub const PRESET_CLONED: &str = "preset.cloned";
/// A preset was applied to a tenant. Payload: audit shape, details
/// carry the application id and the per-status counts.
pub const PRESET_APPLIED: &str = "preset.applied";
/// One module was enabled by a preset application. Payload:
/// `{"preset_id", "application_id", "company_id", "module_id",
/// "actor_id", "auto_activated"}`. Emitted next to the core
/// `module.enabled` event for the same module.
pub const PRESET_MODULE_ENABLED: &str = "preset.module.enabled";
/// One module was skipped by a preset application (not entitled,
/// dependency missing, optional and toggled off, or failed).
/// Payload: `{"preset_id", "application_id", "company_id",
/// "module_id", "actor_id", "reason"}`.
pub const PRESET_MODULE_SKIPPED: &str = "preset.module.skipped";
/// A preset's settings were applied to a tenant. Payload:
/// `{"preset_id", "application_id", "company_id", "actor_id",
/// "settings"}`. The settings, company, branding, and domains
/// modules can subscribe to receive the handoff.
pub const PRESET_SETTINGS_APPLIED: &str = "preset.settings.applied";
/// The module list of one preset, as a hook query ONLY. Request
/// payload: `{"preset_id"}` (or `{"preset_slug"}`). Answer result:
/// `{"preset_id", "preset_slug", "module_ids": ["<module id>", ...]
/// (stored order), "required_module_ids": [...]}`; an unknown preset
/// (or a request without an id) answers with empty lists plus an
/// `"error"`. Tenants provisioning asks this event to resolve a
/// `preset_id` alone into grantable modules (the same shape as
/// `saas_packages.package.modules_requested`, which carries
/// `{"package_id"}` -> `{"module_ids": [...]}`); until this
/// answerer existed, a preset id alone granted no modules.
///
/// This event is never emitted by a preset application: the
/// apply-time handoff uses `presets.preset.modules_applied`
/// instead, so a query answer (the preset's full stored module
/// list) can never be mistaken for the set an application actually
/// applied.
pub const PRESET_MODULES_REQUESTED: &str = "presets.preset.modules_requested";
/// The module list one preset application actually enabled (or
/// found already enabled), as an apply-time handoff. Payload:
/// `{"preset_id", "preset_slug", "application_id", "company_id",
/// "actor_id", "module_ids": [...]}` where `module_ids` is that
/// set in preset order, including transitive dependencies pulled
/// in by the apply; skipped and failed modules are never listed.
/// The Extensions Manager / entitlements / tenants grant flow can
/// enable these modules without re-reading the preset. Distinct
/// from `presets.preset.modules_requested` on purpose: that name
/// is the query for a preset's stored module list.
pub const PRESET_MODULES_APPLIED: &str = "presets.preset.modules_applied";
/// A setup wizard was started. Payload: audit shape.
pub const WIZARD_STARTED: &str = "presets.wizard.started";
/// A wizard step was saved. Payload: audit shape, details carry the
/// step name and which fields were stored.
pub const WIZARD_STEP_SAVED: &str = "presets.wizard.step.saved";
/// A wizard step was skipped. Payload: audit shape, details carry
/// the skipped step name.
pub const WIZARD_SKIPPED: &str = "presets.wizard.step.skipped";
/// A wizard was resumed. Payload: audit shape.
pub const WIZARD_RESUMED: &str = "presets.wizard.resumed";
/// A wizard was restarted. Payload: audit shape.
pub const WIZARD_RESTARTED: &str = "presets.wizard.restarted";
/// A wizard was completed. Payload: audit shape, details carry the
/// application id and the done summary.
pub const WIZARD_COMPLETED: &str = "presets.wizard.completed";
/// A wizard's settings step was completed. Payload:
/// `{"company_id", "actor_id", "company_name", "branding",
/// "domain"}`. The branding and domains modules own those answers;
/// this module only stores and hands them off.
pub const WIZARD_SETTINGS_COMPLETED: &str = "presets.wizard.settings.completed";
