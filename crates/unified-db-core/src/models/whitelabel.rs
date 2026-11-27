// Diesel models for the whitelabel domain of the unified-db-core bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Whitelabel + presets (batch 9 notes sec. 5; `New*` structs added per
// the file convention, DB-defaulted columns omitted).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = whitelabel_global_settings)]
pub struct WhitelabelGlobalSetting {
    pub id: String,
    pub allow_whitelabel: bool,
    pub updated_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = whitelabel_global_settings)]
pub struct NewWhitelabelGlobalSetting {
    pub id: String,
    pub updated_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = whitelabel_company_settings)]
pub struct WhitelabelCompanySetting {
    pub company_id: String,
    pub mode: String,
    pub updated_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = whitelabel_company_settings)]
pub struct NewWhitelabelCompanySetting {
    pub company_id: String,
    pub updated_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = whitelabel_global_defaults)]
pub struct WhitelabelGlobalDefaults {
    pub id: String,
    pub display_name: String,
    pub logo_storage_target: Option<String>,
    pub logo_storage_key: Option<String>,
    pub favicon_storage_target: Option<String>,
    pub favicon_storage_key: Option<String>,
    pub primary_color: String,
    pub accent_color: String,
    pub background_color: String,
    pub text_color: String,
    pub sender_name: String,
    pub sender_email: String,
    pub login_tagline: String,
    pub login_background_storage_target: Option<String>,
    pub login_background_storage_key: Option<String>,
    pub powered_by_text: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = whitelabel_global_defaults)]
pub struct NewWhitelabelGlobalDefaults {
    pub id: String,
    pub logo_storage_target: Option<String>,
    pub logo_storage_key: Option<String>,
    pub favicon_storage_target: Option<String>,
    pub favicon_storage_key: Option<String>,
    pub login_background_storage_target: Option<String>,
    pub login_background_storage_key: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = whitelabel_profiles)]
pub struct WhitelabelProfile {
    pub id: Uuid,
    pub company_id: String,
    pub tenant_id: Option<String>,
    pub display_name: Option<String>,
    pub logo_storage_target: Option<String>,
    pub logo_storage_key: Option<String>,
    pub favicon_storage_target: Option<String>,
    pub favicon_storage_key: Option<String>,
    pub primary_color: Option<String>,
    pub accent_color: Option<String>,
    pub background_color: Option<String>,
    pub text_color: Option<String>,
    pub custom_domain: Option<String>,
    pub domain_status: String,
    pub hide_powered_by: bool,
    pub sender_name: Option<String>,
    pub sender_email: Option<String>,
    pub login_logo_storage_target: Option<String>,
    pub login_logo_storage_key: Option<String>,
    pub login_background_storage_target: Option<String>,
    pub login_background_storage_key: Option<String>,
    pub login_tagline: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = whitelabel_profiles)]
pub struct NewWhitelabelProfile {
    pub id: Uuid,
    pub company_id: String,
    pub tenant_id: Option<String>,
    pub display_name: Option<String>,
    pub logo_storage_target: Option<String>,
    pub logo_storage_key: Option<String>,
    pub favicon_storage_target: Option<String>,
    pub favicon_storage_key: Option<String>,
    pub primary_color: Option<String>,
    pub accent_color: Option<String>,
    pub background_color: Option<String>,
    pub text_color: Option<String>,
    pub custom_domain: Option<String>,
    pub sender_name: Option<String>,
    pub sender_email: Option<String>,
    pub login_logo_storage_target: Option<String>,
    pub login_logo_storage_key: Option<String>,
    pub login_background_storage_target: Option<String>,
    pub login_background_storage_key: Option<String>,
    pub login_tagline: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = whitelabel_history)]
pub struct WhitelabelHistory {
    pub id: Uuid,
    pub company_id: String,
    pub tenant_id: Option<String>,
    pub actor: String,
    pub action: String,
    pub field: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub allowed: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = whitelabel_history)]
pub struct NewWhitelabelHistory {
    pub id: Uuid,
    pub company_id: String,
    pub tenant_id: Option<String>,
    pub actor: String,
    pub action: String,
    pub field: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
}
