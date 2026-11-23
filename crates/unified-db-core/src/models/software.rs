// Diesel models for the software domain of the unified-db-core bounded context.
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
// Software taxonomy: groups and categories (cycle-safe tree).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = software_taxonomy)]
pub struct SoftwareTaxonomy {
    pub id: Uuid,
    pub kind: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub parent_id: Option<Uuid>,
    pub sort_order: i64,
    pub depth: i64,
    pub path: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = software_taxonomy)]
pub struct NewSoftwareTaxonomy {
    pub id: Uuid,
    pub kind: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub parent_id: Option<Uuid>,
    pub sort_order: i64,
    pub depth: i64,
    pub path: String,
}

// ---------------------------------------------------------------------------
// Software records: plugins, themes, software, libraries.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
// NOTE: the path must stay fully qualified to the TABLE module.
// `crate::schema::software` is the schema file-module (schema/software.rs),
// not the table: the `pub use software::*;` re-export in schema.rs
// cannot surface the inner `software` table over the file-module of
// the same name, so the table's columns live at
// `crate::schema::software::software`. Pointing `table_name` at the
// file-module (or the bare name `software`) makes every field fail
// with E0425 "cannot find type ... in module `software`".
#[diesel(table_name = crate::schema::software::software)]
pub struct Software {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub kind: String,
    pub description: String,
    pub vendor_company_id: Option<Uuid>,
    pub group_id: Option<Uuid>,
    pub category_id: Option<Uuid>,
    pub status: String,
    pub homepage_url: Option<String>,
    pub repository_url: Option<String>,
    pub documentation_url: Option<String>,
    pub screenshots: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = crate::schema::software::software)]
pub struct NewSoftware {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub kind: String,
    pub description: String,
    pub vendor_company_id: Option<Uuid>,
    pub group_id: Option<Uuid>,
    pub category_id: Option<Uuid>,
    pub status: String,
    pub homepage_url: Option<String>,
    pub repository_url: Option<String>,
    pub documentation_url: Option<String>,
    pub screenshots: Value,
}

// ---------------------------------------------------------------------------
// Software versions (semver) with platform requirements.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = software_versions)]
pub struct SoftwareVersion {
    pub id: Uuid,
    pub product: String,
    pub version: String,
    pub changelog: String,
    pub file_id: Option<Uuid>,
    pub license_required: bool,
    pub software_id: Option<Uuid>,
    pub requirements: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = software_versions)]
pub struct NewSoftwareVersion {
    pub id: Uuid,
    pub product: String,
    pub version: String,
    pub changelog: String,
    pub file_id: Option<Uuid>,
    pub license_required: bool,
    pub software_id: Option<Uuid>,
    pub requirements: Value,
}

// ---------------------------------------------------------------------------
// Software releases: a version on a channel (stable / beta).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = software_releases)]
pub struct SoftwareRelease {
    pub id: Uuid,
    pub version_id: Uuid,
    pub channel: String,
    pub notes: String,
    pub status: String,
    pub download_count: i64,
    pub created_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = software_releases)]
pub struct NewSoftwareRelease {
    pub id: Uuid,
    pub version_id: Uuid,
    pub channel: String,
    pub notes: String,
    pub status: String,
    pub download_count: i64,
    pub published_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Release files with sha256 / sha512 checksums and integrity status.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = release_files)]
pub struct ReleaseFile {
    pub id: Uuid,
    pub release_id: Uuid,
    pub version_id: Uuid,
    pub filename: String,
    pub size_bytes: i64,
    pub checksum_sha256: Option<String>,
    pub checksum_sha512: Option<String>,
    pub download_url: Option<String>,
    pub storage_path: Option<String>,
    pub integrity_status: String,
    pub verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = release_files)]
pub struct NewReleaseFile {
    pub id: Uuid,
    pub release_id: Uuid,
    pub version_id: Uuid,
    pub filename: String,
    pub size_bytes: i64,
    pub checksum_sha256: Option<String>,
    pub checksum_sha512: Option<String>,
    pub download_url: Option<String>,
    pub storage_path: Option<String>,
    pub integrity_status: String,
    pub verified_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Software tags and their many-to-many assignment to software records.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = software_tags)]
pub struct SoftwareTag {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = software_tags)]
pub struct NewSoftwareTag {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = software_tag_assignments)]
pub struct SoftwareTagAssignment {
    pub software_id: Uuid,
    pub tag_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = software_tag_assignments)]
pub struct NewSoftwareTagAssignment {
    pub software_id: Uuid,
    pub tag_id: Uuid,
}
