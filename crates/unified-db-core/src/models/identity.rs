// Diesel models for the identity domain of the unified-db-core bounded context.
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
#[diesel(table_name = users)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    #[serde(skip_serializing)]
    pub totp_secret: Option<String>,
    pub totp_enabled: bool,
    pub is_active: bool,
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = users)]
pub struct NewUser {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub password_hash: String,
    pub company_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = roles)]
pub struct Role {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub is_system: bool,
    pub permissions: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = roles)]
pub struct NewRole {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub is_system: bool,
    pub permissions: Value,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = companies)]
pub struct Company {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = companies)]
pub struct NewCompany {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = company_sites)]
pub struct CompanySite {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub domain: String,
    pub is_primary: bool,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = company_sites)]
pub struct NewCompanySite {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub domain: String,
    pub is_primary: bool,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = domains)]
pub struct Domain {
    pub id: Uuid,
    pub name: String,
    pub company_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    pub is_active: bool,
    pub ssl_status: String,
    pub ssl_expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = domains)]
pub struct NewDomain {
    pub id: Uuid,
    pub name: String,
    pub company_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = geo_overrides)]
pub struct GeoOverride {
    pub id: Uuid,
    pub cidr: String,
    pub country_code: String,
    pub country_name: Option<String>,
    pub city: Option<String>,
    pub asn: Option<i32>,
    pub asn_organization: Option<String>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = geo_overrides)]
pub struct NewGeoOverride {
    pub id: Uuid,
    pub cidr: String,
    pub country_code: String,
    pub country_name: Option<String>,
    pub city: Option<String>,
    pub asn: Option<i32>,
    pub asn_organization: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = licenses)]
pub struct License {
    pub id: Uuid,
    pub license_key: String,
    pub product: String,
    pub user_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub max_activations: i32,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = licenses)]
pub struct NewLicense {
    pub id: Uuid,
    pub license_key: String,
    pub product: String,
    pub user_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub max_activations: i32,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = license_activations)]
pub struct LicenseActivation {
    pub id: Uuid,
    pub license_id: Uuid,
    pub instance_id: String,
    pub hostname: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = license_activations)]
pub struct NewLicenseActivation {
    pub id: Uuid,
    pub license_id: Uuid,
    pub instance_id: String,
    pub hostname: Option<String>,
}

// User / role assignments (many-to-many).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = user_roles)]
pub struct UserRole {
    pub user_id: Uuid,
    pub role_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = user_roles)]
pub struct NewUserRole {
    pub user_id: Uuid,
    pub role_id: Uuid,
}
