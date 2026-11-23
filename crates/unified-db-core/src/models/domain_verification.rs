// Diesel models for the domain_verification domain of the unified-db-core bounded context.
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
#[diesel(table_name = domain_verification_settings)]
pub struct DomainVerificationSetting {
    pub id: Uuid,
    pub version: i64,
    pub record_type: String,
    pub host_template: String,
    pub value_template: String,
    pub token_length: i32,
    pub token_alphabet: String,
    pub verify_timeout_seconds: i32,
    pub max_attempts: i32,
    pub retry_delay_seconds: i32,
    pub token_ttl_seconds: i64,
    pub rate_limit_window_seconds: i64,
    pub updated_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = domain_verification_settings)]
pub struct NewDomainVerificationSetting {
    pub id: Uuid,
    pub version: i64,
    pub record_type: String,
    pub host_template: String,
    pub value_template: String,
    pub token_length: i32,
    pub token_alphabet: String,
    pub verify_timeout_seconds: i32,
    pub max_attempts: i32,
    pub retry_delay_seconds: i32,
    pub token_ttl_seconds: i64,
    pub rate_limit_window_seconds: i64,
    pub updated_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = domain_verification_tokens)]
pub struct DomainVerificationToken {
    pub id: Uuid,
    pub domain: String,
    pub alias_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub token_hash: String,
    pub record_type: String,
    pub host: String,
    pub expected_value: String,
    pub settings_version: i64,
    pub status: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub verified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = domain_verification_tokens)]
pub struct NewDomainVerificationToken {
    pub id: Uuid,
    pub domain: String,
    pub alias_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub token_hash: String,
    pub record_type: String,
    pub host: String,
    pub expected_value: String,
    pub settings_version: i64,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub verified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = domain_verification_attempts)]
pub struct DomainVerificationAttempt {
    pub id: Uuid,
    pub token_id: Uuid,
    pub domain: String,
    pub alias_id: Option<Uuid>,
    pub attempt_number: i32,
    pub result: String,
    pub record_found: Option<String>,
    pub looked_up: Value,
    pub error: Option<String>,
    pub checked_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = domain_verification_attempts)]
pub struct NewDomainVerificationAttempt {
    pub id: Uuid,
    pub token_id: Uuid,
    pub domain: String,
    pub alias_id: Option<Uuid>,
    pub attempt_number: i32,
    pub result: String,
    pub record_found: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = domain_cert_modes)]
pub struct DomainCertMode {
    pub id: Uuid,
    pub domain: String,
    pub mode: String,
    pub provider: String,
    pub fingerprint_sha256: String,
    pub installed_at: Option<DateTime<Utc>>,
    pub previous_fingerprint: Option<String>,
    pub status: String,
    pub last_error: Option<String>,
    pub updated_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = domain_cert_modes)]
pub struct NewDomainCertMode {
    pub id: Uuid,
    pub domain: String,
    pub mode: String,
    pub provider: String,
    pub installed_at: Option<DateTime<Utc>>,
    pub previous_fingerprint: Option<String>,
    pub status: String,
    pub last_error: Option<String>,
}
