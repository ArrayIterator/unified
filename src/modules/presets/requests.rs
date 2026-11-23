//! Presets + Setup Wizard module. — Request bodies.
//!
//! Split from `src/modules/presets.rs`; behaviour unchanged.

use super::apply::*;
use super::events::*;
use super::helpers::*;
use super::identity::*;
use super::lookups::*;
use super::preset_handlers::*;
use super::records::*;
use super::seeds::*;
use super::stores::*;
use super::wizard_handlers::*;
use super::wizard_steps::*;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub(super) struct PresetModuleEntryInput {
    pub(super) module_id: String,
    #[serde(default = "default_required")]
    pub(super) required: bool,
    #[serde(default)]
    pub(super) sort_order: Option<i32>,
}

pub(super) fn default_required() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub(super) struct CreatePresetRequest {
    #[serde(flatten)]
    pub(super) actor: ActorInput,
    pub(super) name: String,
    #[serde(default)]
    pub(super) slug: Option<String>,
    #[serde(default)]
    pub(super) description: Option<String>,
    pub(super) business_type: String,
    pub(super) modules: Vec<PresetModuleEntryInput>,
    #[serde(default)]
    pub(super) settings: Option<Value>,
    #[serde(default)]
    pub(super) steps: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub(super) struct UpdatePresetRequest {
    #[serde(flatten)]
    pub(super) actor: ActorInput,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) slug: Option<String>,
    #[serde(default)]
    pub(super) description: Option<String>,
    #[serde(default)]
    pub(super) business_type: Option<String>,
    #[serde(default)]
    pub(super) modules: Option<Vec<PresetModuleEntryInput>>,
    #[serde(default)]
    pub(super) settings: Option<Value>,
    #[serde(default)]
    pub(super) steps: Option<Vec<String>>,
    #[serde(default)]
    pub(super) archived: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ClonePresetRequest {
    #[serde(flatten)]
    pub(super) actor: ActorInput,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) slug: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ApplyPresetRequest {
    #[serde(flatten)]
    pub(super) actor: ActorInput,
    pub(super) company_id: Uuid,
    #[serde(default)]
    pub(super) disabled_optional_modules: Option<Vec<String>>,
    #[serde(default)]
    pub(super) settings: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(super) struct StartWizardRequest {
    #[serde(flatten)]
    pub(super) actor: ActorInput,
    pub(super) company_id: Uuid,
    #[serde(default)]
    pub(super) preset_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub(super) struct SaveStepRequest {
    #[serde(flatten)]
    pub(super) actor: ActorInput,
    #[serde(default)]
    pub(super) step: Option<String>,
    #[serde(default)]
    pub(super) preset_id: Option<Uuid>,
    #[serde(default)]
    pub(super) disabled_optional_modules: Option<Vec<String>>,
    #[serde(default)]
    pub(super) settings: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(super) struct WizardActionRequest {
    #[serde(flatten)]
    pub(super) actor: ActorInput,
    #[serde(default)]
    pub(super) preset_id: Option<Uuid>,
    #[serde(default)]
    pub(super) step: Option<String>,
}

/// One module in the preview: its stored entry plus the live
/// availability answer for the tenant.
#[derive(Debug, Clone, Serialize)]
pub struct PresetPreviewModule {
    pub module_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub required: bool,
    /// `"ok"`, `"already_enabled"`, `"not_entitled"`, or `"missing"`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Transitive requirements, in activation order.
    pub will_auto_activate: Vec<String>,
    /// Required module ids that are not registered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_required: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PresetPreview {
    pub preset: Preset,
    pub company_id: Option<Uuid>,
    pub modules: Vec<PresetPreviewModule>,
    /// Activation order for the modules the preview would enable.
    pub apply_order: Vec<String>,
}
