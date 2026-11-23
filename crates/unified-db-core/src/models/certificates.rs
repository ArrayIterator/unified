// Diesel models for the certificates domain of the unified-db-core bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Certificates (2026-11-22-000005_certificates).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
// NOTE: the path must stay fully qualified to the TABLE module.
// `crate::schema::certificates` is the schema file-module (schema/certificates.rs),
// not the table: the `pub use certificates::*;` re-export in schema.rs
// cannot surface the inner `certificates` table over the file-module of
// the same name, so the table's columns live at
// `crate::schema::certificates::certificates`. Pointing `table_name` at the
// file-module (or the bare name `certificates`) makes every field fail
// with E0425 "cannot find type ... in module `certificates`".
#[diesel(table_name = crate::schema::certificates::certificates)]
pub struct Certificate {
    pub id: Uuid,
    pub domain: String,
    pub provider: String,
    pub status: String,
    pub issuer: String,
    pub sans: Value,
    pub fingerprint_sha256: String,
    pub not_before: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub days_remaining: Option<i64>,
    pub self_signed: bool,
    pub wildcard: bool,
    pub source: String,
    pub revoked: bool,
    pub last_renewed_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = crate::schema::certificates::certificates)]
pub struct NewCertificate {
    pub id: Uuid,
    pub domain: String,
    pub provider: String,
    pub status: String,
    pub fingerprint_sha256: String,
    pub not_before: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub days_remaining: Option<i64>,
    pub source: String,
    pub last_renewed_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = certificate_renewals)]
pub struct CertificateRenewal {
    pub id: Uuid,
    pub domain: String,
    pub provider: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub new_expires_at: Option<DateTime<Utc>>,
    pub forced: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = certificate_renewals)]
pub struct NewCertificateRenewal {
    pub id: Uuid,
    pub domain: String,
    pub provider: String,
    pub status: String,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub new_expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = certificate_settings)]
pub struct CertificateSettings {
    pub key: String,
    pub enabled: bool,
    pub check_interval_secs: i64,
    pub acme: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = certificate_settings)]
pub struct NewCertificateSettings {
    pub key: String,
}
