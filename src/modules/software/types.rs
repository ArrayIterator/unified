//! Software catalogue types: software, versions, releases, files, taxonomy.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Software kinds accepted by the catalogue.
pub const SOFTWARE_KINDS: [&str; 4] = ["plugin", "theme", "software", "library"];

/// Software lifecycle statuses.
pub const SOFTWARE_STATUSES: [&str; 4] = ["draft", "published", "deprecated", "archived"];

/// Taxonomy node kinds.
pub const TAXONOMY_KINDS: [&str; 2] = ["group", "category"];

/// File integrity statuses.
pub const INTEGRITY_STATUSES: [&str; 4] = ["pending", "verified", "failed", "unverified"];

/// May read the software catalogue (records, versions, releases,
/// files, taxonomy, tags, statistics). Checked through the
/// `role.check` hook; fail closed.
pub const PERM_SOFTWARE_READ: &str = "software:read";

/// May create software records, versions, releases, files,
/// taxonomy nodes, and tags. Checked through the `role.check` hook;
/// fail closed.
pub const PERM_SOFTWARE_CREATE: &str = "software:create";

/// May update, delete, publish/unpublish, and verify software
/// records, versions, releases, files, taxonomy nodes, and tags.
/// Checked through the `role.check` hook; fail closed.
pub const PERM_SOFTWARE_MANAGE: &str = "software:manage";

// Types

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VersionRequirements {
    #[serde(default)]
    pub wordpress: Option<String>,
    #[serde(default)]
    pub php: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoftwareVersion {
    pub id: String,
    pub product: String,
    pub version: String,
    pub changelog: String,
    pub file_id: Option<String>,
    pub license_required: bool,
    /// Owning software record id, when the version belongs to a catalogue entry.
    #[serde(default)]
    pub software_id: Option<String>,
    /// Minimum platform requirements for this version.
    #[serde(default)]
    pub requirements: VersionRequirements,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateVersion {
    pub product: String,
    pub version: String,
    pub changelog: Option<String>,
    pub file_id: Option<String>,
    pub license_required: Option<bool>,
    #[serde(default)]
    pub software_id: Option<String>,
    #[serde(default)]
    pub requirements: Option<VersionRequirements>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateVersion {
    pub product: Option<String>,
    pub version: Option<String>,
    pub changelog: Option<String>,
    pub file_id: Option<String>,
    pub license_required: Option<bool>,
    #[serde(default)]
    pub software_id: Option<String>,
    #[serde(default)]
    pub requirements: Option<VersionRequirements>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Release {
    pub id: String,
    pub version_id: String,
    /// `stable` or `beta`.
    pub channel: String,
    pub notes: String,
    /// `draft` or `published`.
    pub status: String,
    pub download_count: u64,
    pub created_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateRelease {
    pub version_id: String,
    pub channel: String,
    pub notes: Option<String>,
}

// Software records

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Software {
    pub id: String,
    pub slug: String,
    pub name: String,
    /// One of `plugin`, `theme`, `software`, `library`.
    pub kind: String,
    pub description: String,
    /// Owning vendor company id (UUID string); validated against the
    /// company module through the `company.exists` hook when a handler
    /// is registered, otherwise accepted as a plain reference.
    pub vendor_company_id: Option<String>,
    /// Owning group taxonomy node id.
    pub group_id: Option<String>,
    /// Owning category taxonomy node id.
    pub category_id: Option<String>,
    /// `draft`, `published`, `deprecated`, or `archived`.
    pub status: String,
    pub homepage_url: Option<String>,
    pub repository_url: Option<String>,
    pub documentation_url: Option<String>,
    /// Screenshot image URLs.
    pub screenshots: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateSoftware {
    pub name: String,
    #[serde(default)]
    pub slug: Option<String>,
    pub kind: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub vendor_company_id: Option<String>,
    #[serde(default)]
    pub group_id: Option<String>,
    #[serde(default)]
    pub category_id: Option<String>,
    #[serde(default)]
    pub homepage_url: Option<String>,
    #[serde(default)]
    pub repository_url: Option<String>,
    #[serde(default)]
    pub documentation_url: Option<String>,
    #[serde(default)]
    pub screenshots: Vec<String>,
    #[serde(default)]
    pub tag_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSoftware {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub vendor_company_id: Option<String>,
    #[serde(default)]
    pub group_id: Option<String>,
    #[serde(default)]
    pub category_id: Option<String>,
    #[serde(default)]
    pub homepage_url: Option<String>,
    #[serde(default)]
    pub repository_url: Option<String>,
    #[serde(default)]
    pub documentation_url: Option<String>,
    #[serde(default)]
    pub screenshots: Option<Vec<String>>,
    /// When present, must be a valid lifecycle transition.
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoftwareDetail {
    #[serde(flatten)]
    pub software: Software,
    pub tags: Vec<Tag>,
    pub latest_version: Option<String>,
    pub release_count: u64,
    pub total_downloads: u64,
}

// Release files

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseFile {
    pub id: String,
    pub release_id: String,
    pub version_id: String,
    pub filename: String,
    pub size_bytes: u64,
    /// Lowercase hex sha256 digest, when supplied.
    pub checksum_sha256: Option<String>,
    /// Lowercase hex sha512 digest, when supplied.
    pub checksum_sha512: Option<String>,
    pub download_url: Option<String>,
    pub storage_path: Option<String>,
    /// `pending`, `verified`, `failed`, or `unverified`.
    pub integrity_status: String,
    pub verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateReleaseFile {
    pub filename: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub checksum_sha256: Option<String>,
    #[serde(default)]
    pub checksum_sha512: Option<String>,
    #[serde(default)]
    pub download_url: Option<String>,
    #[serde(default)]
    pub storage_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateReleaseFile {
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub checksum_sha256: Option<String>,
    #[serde(default)]
    pub checksum_sha512: Option<String>,
    #[serde(default)]
    pub download_url: Option<String>,
    #[serde(default)]
    pub storage_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VerifyFile {
    /// Base64-encoded bytes to hash and compare against the stored checksums.
    pub content_base64: String,
}

// Taxonomy (groups and categories) and tags

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxonomyNode {
    pub id: String,
    /// `group` or `category`.
    pub kind: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub parent_id: Option<String>,
    pub sort_order: i64,
    /// Depth in the tree; roots are 0.
    pub depth: i64,
    /// Slash path of slugs from the root, for example `/tools/wordpress`.
    pub path: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxonomyTreeNode {
    #[serde(flatten)]
    pub node: TaxonomyNode,
    pub children: Vec<TaxonomyTreeNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTaxonomyNode {
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub sort_order: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTaxonomyNode {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub sort_order: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MoveTaxonomyNode {
    /// New parent id; null (absent) moves the node to the root.
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub sort_order: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTag {
    pub name: String,
    #[serde(default)]
    pub slug: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTag {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagCount {
    #[serde(flatten)]
    pub tag: Tag,
    pub software_count: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AttachTag {
    pub tag_id: String,
}

// Discovery views

#[derive(Debug, Clone, Serialize)]
pub struct LatestRelease {
    pub release: Release,
    pub version: String,
    pub product: String,
    pub software_id: Option<String>,
    pub software_name: Option<String>,
    pub software_slug: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SoftwareReleaseHistory {
    pub release: Release,
    pub version: String,
    pub changelog: String,
    pub license_required: bool,
}
