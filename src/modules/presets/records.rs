//! Presets + Setup Wizard module. - Stored records.
//!
//! Split from `src/modules/presets.rs`; behaviour unchanged.

use super::apply::*;
use super::events::*;
use super::helpers::*;
use super::identity::*;
use super::lookups::*;
use super::preset_handlers::*;
use super::requests::*;
use super::seeds::*;
use super::stores::*;
use super::wizard_handlers::*;
use super::wizard_steps::*;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// One module entry inside a preset definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetModuleEntry {
    pub module_id: String,
    pub required: bool,
    pub sort_order: i32,
}

/// A preset definition. `business_type` is a free-form key taken
/// from the data (for example `"freelancer"`); nothing in this
/// module branches on its value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preset {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub business_type: String,
    pub built_in: bool,
    pub archived: bool,
    pub modules: Vec<PresetModuleEntry>,
    /// Settings applied to the tenant on activation (a JSON object).
    pub settings: Value,
    /// Wizard step sequence for this preset, in order. Empty means
    /// "use the default sequence" at wizard start.
    pub steps: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One module outcome inside a preset application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleApplyOutcome {
    pub module_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Dependencies enabled as part of enabling this module.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auto_activated: Vec<String>,
}

/// The recorded result of applying one preset to one tenant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetApplication {
    pub id: Uuid,
    pub preset_id: Uuid,
    pub preset_slug: String,
    pub company_id: Uuid,
    pub actor_id: String,
    /// `"completed"` (nothing failed or was skipped for a dependency
    /// reason), `"partial"` (at least one module was skipped or
    /// failed), or `"failed"` (no module could be enabled).
    pub status: String,
    pub outcomes: Vec<ModuleApplyOutcome>,
    /// The settings object actually applied (preset settings merged
    /// with the caller-supplied overrides).
    pub settings_applied: Value,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

/// Setup wizard state for one tenant. `steps` is the stored step
/// sequence (copied from the chosen preset, or the default);
/// navigation is by position in that list, never by matching a
/// business-type name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WizardState {
    pub id: Uuid,
    pub company_id: Uuid,
    pub actor_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset_id: Option<Uuid>,
    pub steps: Vec<String>,
    pub current_step: String,
    /// `"in_progress"` or `"completed"`.
    pub status: String,
    /// Steps the caller skipped, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped_steps: Vec<String>,
    /// Optional modules the caller toggled off in the review step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_optional_modules: Vec<String>,
    /// Answers from the settings step (company name, branding,
    /// domain), stored as given and handed off via events.
    pub settings: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub application_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The done summary returned by the wizard complete endpoint and
/// stored on the wizard: what was enabled, what was skipped, and
/// what needs attention.
#[derive(Debug, Clone, Serialize)]
pub struct WizardSummary {
    pub wizard_id: Uuid,
    pub company_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub application_id: Option<Uuid>,
    pub enabled: Vec<String>,
    pub already_enabled: Vec<String>,
    pub skipped_not_entitled: Vec<String>,
    pub skipped_dependency_missing: Vec<String>,
    pub skipped_optional_off: Vec<String>,
    pub failed: Vec<String>,
    /// Everything that needs attention: not-entitled, missing
    /// dependency, and failed modules, with reasons.
    pub needs_attention: Vec<ModuleApplyOutcome>,
}
