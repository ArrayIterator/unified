// Diesel models for the presets domain of the unified-db-platform bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = presets)]
pub struct Preset {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub business_type: String,
    pub built_in: bool,
    pub archived: bool,
    pub settings: Value,
    pub steps: Value,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = presets)]
pub struct NewPreset {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub business_type: String,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = preset_modules)]
pub struct PresetModule {
    pub id: Uuid,
    pub preset_id: Uuid,
    pub module_id: String,
    pub required: bool,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = preset_modules)]
pub struct NewPresetModule {
    pub id: Uuid,
    pub preset_id: Uuid,
    pub module_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = preset_wizard_states)]
pub struct PresetWizardState {
    pub id: Uuid,
    pub company_id: Uuid,
    pub actor_id: String,
    pub preset_id: Option<Uuid>,
    pub steps: Value,
    pub current_step: String,
    pub status: String,
    pub skipped_steps: Value,
    pub disabled_optional_modules: Value,
    pub settings: Value,
    pub application_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = preset_wizard_states)]
pub struct NewPresetWizardState {
    pub id: Uuid,
    pub company_id: Uuid,
    pub preset_id: Option<Uuid>,
    pub application_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = preset_applications)]
pub struct PresetApplication {
    pub id: Uuid,
    pub preset_id: Option<Uuid>,
    pub preset_slug: String,
    pub company_id: Uuid,
    pub actor_id: String,
    pub status: String,
    pub outcomes: Value,
    pub settings_applied: Value,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = preset_applications)]
pub struct NewPresetApplication {
    pub id: Uuid,
    pub preset_id: Option<Uuid>,
    pub preset_slug: String,
    pub company_id: Uuid,
    pub completed_at: Option<DateTime<Utc>>,
}
