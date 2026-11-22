// Software / Assets Manager module.
//
// Enterprise software asset manager: software records with a status
// lifecycle (draft -> published -> deprecated / archived), versions
// with semver validation and platform requirements, releases with a
// publish flow, per-release files with sha256/sha512 checksums and
// integrity verification, hierarchical taxonomy (groups and
// categories), tags, full-text discovery, and license-gated downloads:
// a release with `license_required` asks the licensing module through
// the `license.validate_requested` hook, and denies the download
// honestly when no validator answers or the key is invalid.
// Interconnection happens only through the hook bus.
//
// Persistence is in-memory (DashMap) in this module; the Diesel
// tables for durable storage are listed in the module report and are
// applied by a later wiring step.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256, Sha512};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Software kinds accepted by the catalogue.
pub const SOFTWARE_KINDS: [&str; 4] = ["plugin", "theme", "software", "library"];

/// Software lifecycle statuses.
pub const SOFTWARE_STATUSES: [&str; 4] = ["draft", "published", "deprecated", "archived"];

/// Taxonomy node kinds.
pub const TAXONOMY_KINDS: [&str; 2] = ["group", "category"];

/// File integrity statuses.
pub const INTEGRITY_STATUSES: [&str; 4] = ["pending", "verified", "failed", "unverified"];

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Software records
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Release files
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Taxonomy (groups and categories) and tags
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Discovery views
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct SoftwareState {
    hooks: RwLock<Arc<HookBus>>,
    software: DashMap<String, Software>,
    versions: DashMap<String, SoftwareVersion>,
    releases: DashMap<String, Release>,
    release_files: DashMap<String, ReleaseFile>,
    taxonomy: DashMap<String, TaxonomyNode>,
    tags: DashMap<String, Tag>,
    /// software id -> assigned tag ids.
    software_tags: DashMap<String, HashSet<String>>,
}

impl SoftwareState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            software: DashMap::new(),
            versions: DashMap::new(),
            releases: DashMap::new(),
            release_files: DashMap::new(),
            taxonomy: DashMap::new(),
            tags: DashMap::new(),
            software_tags: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    fn software_detail(&self, software: &Software) -> SoftwareDetail {
        let tag_ids = self
            .software_tags
            .get(&software.id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        let mut tags: Vec<Tag> = tag_ids
            .iter()
            .filter_map(|id| self.tags.get(id).map(|entry| entry.value().clone()))
            .collect();
        tags.sort_by(|a, b| a.name.cmp(&b.name));
        let versions: Vec<SoftwareVersion> = self
            .versions
            .iter()
            .filter(|entry| entry.value().software_id.as_deref() == Some(software.id.as_str()))
            .map(|entry| entry.value().clone())
            .collect();
        let version_ids: HashSet<&str> = versions.iter().map(|v| v.id.as_str()).collect();
        let latest_version = versions
            .iter()
            .filter_map(|v| Version::parse(&v.version).ok().map(|parsed| (parsed, v.version.clone())))
            .max()
            .map(|(_, version)| version);
        let mut release_count = 0u64;
        let mut total_downloads = 0u64;
        for entry in self.releases.iter() {
            if version_ids.contains(entry.value().version_id.as_str()) {
                release_count += 1;
                total_downloads += entry.value().download_count;
            }
        }
        SoftwareDetail {
            software: software.clone(),
            tags,
            latest_version,
            release_count,
            total_downloads,
        }
    }

    /// True when the referenced company exists. When no
    /// `company.exists` handler is registered the reference is
    /// accepted unchecked, because this module cannot see the
    /// company store directly.
    fn vendor_exists(&self, company_id: &str) -> bool {
        if !self.bus().has("company.exists") {
            return true;
        }
        let context = self.bus().emit(
            "company.exists",
            json!({ "company_id": company_id }),
        );
        context.results.iter().any(|result| {
            result.get("exists").and_then(|v| v.as_bool()).unwrap_or(false)
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

fn validate_version_string(version: &str) -> UnifiedResult<()> {
    Version::parse(version).map(|_| ()).map_err(|_| {
        UnifiedError::bad_request(format!(
            "Version \"{version}\" is not a valid semantic version (expected MAJOR.MINOR.PATCH)"
        ))
    })
}

/// Validate a partial version such as `6.5` or `8.1` used for
/// WordPress / PHP minimum requirements: one to three dot-separated
/// numeric components.
fn validate_partial_version(value: &str, field: &str) -> UnifiedResult<()> {
    let parts: Vec<&str> = value.trim().split('.').collect();
    let valid = !parts.is_empty()
        && parts.len() <= 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()));
    if valid {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "{field} \"{value}\" is not a valid version (expected e.g. 6.5 or 8.1)"
        )))
    }
}

fn validate_requirements(requirements: &VersionRequirements) -> UnifiedResult<()> {
    if let Some(wordpress) = &requirements.wordpress {
        validate_partial_version(wordpress, "WordPress version")?;
    }
    if let Some(php) = &requirements.php {
        validate_partial_version(php, "PHP version")?;
    }
    if let Some(platform) = &requirements.platform {
        if platform.trim().is_empty() {
            return Err(UnifiedError::bad_request("Platform must not be empty"));
        }
    }
    for dependency in &requirements.dependencies {
        if dependency.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Dependencies must not contain empty entries",
            ));
        }
    }
    Ok(())
}

fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in name.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

fn validate_kind(kind: &str) -> UnifiedResult<()> {
    if SOFTWARE_KINDS.contains(&kind) {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "Kind must be one of: {}",
            SOFTWARE_KINDS.join(", ")
        )))
    }
}

fn validate_status_transition(from: &str, to: &str) -> UnifiedResult<()> {
    if from == to {
        return Err(UnifiedError::conflict(format!(
            "Software is already {from}"
        )));
    }
    let allowed = match from {
        "draft" => ["published", "archived"].as_slice(),
        "published" => ["deprecated", "archived", "draft"].as_slice(),
        "deprecated" => ["published", "archived", "draft"].as_slice(),
        "archived" => ["draft"].as_slice(),
        _ => &[],
    };
    if allowed.contains(&to) {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "Cannot move software from {from} to {to}"
        )))
    }
}

fn validate_url(value: &str, field: &str) -> UnifiedResult<()> {
    if value.starts_with("http://") || value.starts_with("https://") {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "{field} must start with http:// or https://"
        )))
    }
}

fn validate_hex_checksum(value: &str, expected_len: usize, field: &str) -> UnifiedResult<()> {
    let valid = value.len() == expected_len
        && value.chars().all(|ch| ch.is_ascii_hexdigit());
    if valid {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "{field} must be {expected_len} lowercase hex characters"
        )))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn sha512_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha512::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn taxonomy_path(state: &SoftwareState, parent_id: Option<&str>, slug: &str) -> (i64, String) {
    match parent_id.and_then(|id| state.taxonomy.get(id)) {
        Some(parent) => (
            parent.depth + 1,
            format!("{}/{}", parent.path, slug),
        ),
        None => (0, format!("/{slug}")),
    }
}

/// Recompute depth and path for a node and every descendant after a move.
fn recompute_subtree(state: &SoftwareState, root_id: &str) {
    let mut queue: Vec<String> = vec![root_id.to_string()];
    while let Some(id) = queue.pop() {
        let (parent_id, slug) = match state.taxonomy.get(&id) {
            Some(node) => (node.parent_id.clone(), node.slug.clone()),
            None => continue,
        };
        let (depth, path) = taxonomy_path(state, parent_id.as_deref(), &slug);
        if let Some(mut node) = state.taxonomy.get_mut(&id) {
            node.depth = depth;
            node.path = path;
            node.updated_at = now();
        }
        let children: Vec<String> = state
            .taxonomy
            .iter()
            .filter(|entry| entry.value().parent_id.as_deref() == Some(id.as_str()))
            .map(|entry| entry.key().clone())
            .collect();
        queue.extend(children);
    }
}

fn build_tree(
    state: &SoftwareState,
    parent_id: Option<&str>,
    kind: Option<&str>,
) -> Vec<TaxonomyTreeNode> {
    let mut nodes: Vec<TaxonomyNode> = state
        .taxonomy
        .iter()
        .filter(|entry| {
            entry.value().parent_id.as_deref() == parent_id
                && kind.is_none_or(|kind| entry.value().kind == kind)
        })
        .map(|entry| entry.value().clone())
        .collect();
    nodes.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then_with(|| a.name.cmp(&b.name))
    });
    nodes
        .into_iter()
        .map(|node| TaxonomyTreeNode {
            children: build_tree(state, Some(node.id.as_str()), kind),
            node,
        })
        .collect()
}

fn is_descendant_of(state: &SoftwareState, node_id: &str, ancestor_id: &str) -> bool {
    let mut current = state
        .taxonomy
        .get(node_id)
        .and_then(|node| node.parent_id.clone());
    while let Some(id) = current {
        if id == ancestor_id {
            return true;
        }
        current = state
            .taxonomy
            .get(&id)
            .and_then(|node| node.parent_id.clone());
    }
    false
}

fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

// ---------------------------------------------------------------------------
// Version handlers
// ---------------------------------------------------------------------------

async fn list_versions(
    state: web::Data<Arc<SoftwareState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let sort = params.allowed_sort(&["product", "version", "created_at"], "created_at");
    let mut items: Vec<SoftwareVersion> = state
        .versions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|version| {
            matches_search(
                &format!("{} {} {}", version.product, version.version, version.changelog),
                &params,
            ) && params.filters.get("product").is_none_or(|product| {
                version.product == *product
            }) && params.filters.get("software_id").is_none_or(|software_id| {
                version.software_id.as_deref() == Some(software_id.as_str())
            }) && params.filters.get("license_required").is_none_or(|flag| {
                version.license_required == (*flag == "true" || *flag == "1")
            })
        })
        .collect();
    match sort.as_str() {
        "product" => items.sort_by(|a, b| a.product.cmp(&b.product)),
        "version" => items.sort_by(|a, b| {
            let parsed_a = Version::parse(&a.version);
            let parsed_b = Version::parse(&b.version);
            match (parsed_a, parsed_b) {
                (Ok(left), Ok(right)) => left.cmp(&right),
                _ => a.version.cmp(&b.version),
            }
        }),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_version(
    state: web::Data<Arc<SoftwareState>>,
    body: web::Json<CreateVersion>,
) -> HttpResponse {
    let result: UnifiedResult<SoftwareVersion> = (|| {
        let input = body.into_inner();
        if input.product.trim().is_empty() {
            return Err(UnifiedError::bad_request("Product must not be empty"));
        }
        validate_version_string(&input.version)?;
        let requirements = input.requirements.unwrap_or_default();
        validate_requirements(&requirements)?;
        if let Some(software_id) = &input.software_id {
            if !state.software.contains_key(software_id.as_str()) {
                return Err(UnifiedError::not_found("Software not found"));
            }
        }
        let version = SoftwareVersion {
            id: new_id(),
            product: input.product.trim().to_string(),
            version: input.version.trim().to_string(),
            changelog: input.changelog.unwrap_or_default(),
            file_id: input.file_id,
            license_required: input.license_required.unwrap_or(false),
            software_id: input.software_id,
            requirements,
            created_at: now(),
            updated_at: now(),
        };
        state.versions.insert(version.id.clone(), version.clone());
        Ok(version)
    })();
    respond_created(result)
}

async fn get_version(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<SoftwareVersion> = state
        .versions
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Version not found"));
    json::respond(result, false)
}

async fn update_version(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
    body: web::Json<UpdateVersion>,
) -> HttpResponse {
    let result: UnifiedResult<SoftwareVersion> = (|| {
        let input = body.into_inner();
        let mut version = state
            .versions
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Version not found"))?;
        if let Some(product) = input.product {
            if product.trim().is_empty() {
                return Err(UnifiedError::bad_request("Product must not be empty"));
            }
            version.product = product.trim().to_string();
        }
        if let Some(version_string) = input.version {
            validate_version_string(&version_string)?;
            version.version = version_string.trim().to_string();
        }
        if let Some(changelog) = input.changelog {
            version.changelog = changelog;
        }
        if let Some(file_id) = input.file_id {
            version.file_id = Some(file_id);
        }
        if let Some(license_required) = input.license_required {
            version.license_required = license_required;
        }
        if let Some(software_id) = input.software_id {
            if !state.software.contains_key(software_id.as_str()) {
                return Err(UnifiedError::not_found("Software not found"));
            }
            version.software_id = Some(software_id);
        }
        if let Some(requirements) = input.requirements {
            validate_requirements(&requirements)?;
            version.requirements = requirements;
        }
        version.updated_at = now();
        Ok(version.clone())
    })();
    json::respond(result, false)
}

async fn delete_version(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let in_use = state
            .releases
            .iter()
            .any(|entry| entry.value().version_id == path.as_str());
        if in_use {
            return Err(UnifiedError::conflict(
                "Version has releases; delete the releases first",
            ));
        }
        state
            .versions
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Version not found"))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Release handlers
// ---------------------------------------------------------------------------

async fn list_releases(
    state: web::Data<Arc<SoftwareState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<Release> = state
        .releases
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|release| {
            matches_search(&format!("{} {}", release.channel, release.notes), &params)
                && params
                    .filters
                    .get("channel")
                    .is_none_or(|channel| release.channel == *channel)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| release.status == *status)
                && params
                    .filters
                    .get("version_id")
                    .is_none_or(|version_id| release.version_id == *version_id)
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_release(
    state: web::Data<Arc<SoftwareState>>,
    body: web::Json<CreateRelease>,
) -> HttpResponse {
    let result: UnifiedResult<Release> = (|| {
        let input = body.into_inner();
        if !matches!(input.channel.as_str(), "stable" | "beta") {
            return Err(UnifiedError::bad_request(
                "Channel must be stable or beta",
            ));
        }
        if !state.versions.contains_key(&input.version_id) {
            return Err(UnifiedError::not_found("Version not found"));
        }
        let release = Release {
            id: new_id(),
            version_id: input.version_id,
            channel: input.channel,
            notes: input.notes.unwrap_or_default(),
            status: "draft".to_string(),
            download_count: 0,
            created_at: now(),
            published_at: None,
        };
        state.releases.insert(release.id.clone(), release.clone());
        Ok(release)
    })();
    respond_created(result)
}

async fn get_release(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Release> = state
        .releases
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Release not found"));
    json::respond(result, false)
}

async fn delete_release(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .releases
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Release not found"))
    })();
    json::respond(result, false)
}

async fn publish_release(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Release> = (|| {
        let mut release = state
            .releases
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Release not found"))?;
        if release.status == "published" {
            return Err(UnifiedError::conflict("Release is already published"));
        }
        release.status = "published".to_string();
        release.published_at = Some(now());
        let version_id = release.version_id.clone();
        let notes = release.notes.clone();
        let release_value = serde_json::to_value(&*release).unwrap_or(Value::Null);
        drop(release);
        state.emit("software.released", release_value.clone());
        // The published event also carries the version and software
        // context the store needs to refresh its catalog entry, so
        // listeners never have to query this module's state. The
        // plain release shape is kept intact; the extra keys are
        // additive only.
        let mut published_value = release_value;
        if let Some(version) = state
            .versions
            .get(version_id.as_str())
            .map(|entry| entry.value().clone())
        {
            if let Value::Object(map) = &mut published_value {
                map.insert("version".to_string(), json!(version.version));
                let changelog = if notes.trim().is_empty() {
                    version.changelog.clone()
                } else {
                    notes
                };
                map.insert("changelog".to_string(), json!(changelog));
                map.insert(
                    "requirements".to_string(),
                    serde_json::to_value(&version.requirements).unwrap_or(Value::Null),
                );
                if let Some(software_id) = &version.software_id {
                    map.insert("software_id".to_string(), json!(software_id));
                    if let Some(software) = state
                        .software
                        .get(software_id.as_str())
                        .map(|entry| entry.value().clone())
                    {
                        map.insert("software_slug".to_string(), json!(software.slug));
                        map.insert("software_name".to_string(), json!(software.name));
                    }
                }
            }
        }
        state.emit("software.release.published", published_value);
        Ok(state
            .releases
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .expect("release still present"))
    })();
    json::respond(result, false)
}

async fn unpublish_release(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Release> = (|| {
        let mut release = state
            .releases
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Release not found"))?;
        if release.status != "published" {
            return Err(UnifiedError::conflict("Release is not published"));
        }
        release.status = "draft".to_string();
        release.published_at = None;
        let release_value = serde_json::to_value(&*release).unwrap_or(Value::Null);
        drop(release);
        state.emit("software.release.unpublished", release_value);
        Ok(state
            .releases
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .expect("release still present"))
    })();
    json::respond(result, false)
}

async fn release_files(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let release = state
            .releases
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Release not found"))?;
        let version = state
            .versions
            .get(&release.version_id)
            .map(|entry| entry.value().clone());
        let file_ids: Vec<String> = version
            .as_ref()
            .and_then(|version| version.file_id.clone())
            .into_iter()
            .collect();
        let mut release_files: Vec<ReleaseFile> = state
            .release_files
            .iter()
            .filter(|entry| entry.value().release_id == release.id)
            .map(|entry| entry.value().clone())
            .collect();
        release_files.sort_by(|a, b| a.filename.cmp(&b.filename));
        Ok(json!({
            "release_id": release.id,
            "version_id": release.version_id,
            "files": file_ids.iter().map(|file_id| json!({ "file_id": file_id })).collect::<Vec<Value>>(),
            "release_files": release_files,
        }))
    })();
    json::respond(result, false)
}

async fn download_release(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
    req: HttpRequest,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let release = state
            .releases
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Release not found"))?;
        if release.status != "published" {
            return Err(UnifiedError::forbidden(
                "Only published releases can be downloaded",
            ));
        }
        let version = state
            .versions
            .get(&release.version_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::internal("Release version is missing"))?;
        let file_id = version.file_id.clone().ok_or_else(|| {
            UnifiedError::unprocessable("This release has no file attached")
        })?;
        if version.license_required {
            // Ask the licensing module through the hook bus. When no
            // handler answers, deny honestly instead of guessing.
            let license_key = req
                .headers()
                .get("x-license-key")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_string();
            if license_key.is_empty() {
                return Err(UnifiedError::forbidden(
                    "This release requires a license key (x-license-key header)",
                ));
            }
            if !state.bus().has("license.validate_requested") {
                return Err(UnifiedError::forbidden(
                    "This release requires a license, but no license validator is registered",
                ));
            }
            let context = state.bus().emit(
                "license.validate_requested",
                json!({
                    "license_key": license_key,
                    "product": version.product,
                    "version_id": version.id,
                }),
            );
            let valid = context.results.iter().any(|result| {
                result.get("valid").and_then(|v| v.as_bool()).unwrap_or(false)
            });
            if !valid {
                return Err(UnifiedError::forbidden(
                    "License key is not valid for this product",
                ));
            }
        }
        if let Some(mut entry) = state.releases.get_mut(path.as_str()) {
            entry.download_count += 1;
        }
        state.emit(
            events::DOWNLOAD_STARTED,
            json!({
                "release_id": release.id,
                "version_id": release.version_id,
                "product": version.product,
                "file_id": file_id,
            }),
        );
        Ok(json!({
            "download": {
                "release_id": release.id,
                "version_id": release.version_id,
                "product": version.product,
                "file_id": file_id,
                "status": "ready",
            }
        }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Software record handlers
// ---------------------------------------------------------------------------

async fn list_software(
    state: web::Data<Arc<SoftwareState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let sort = params.allowed_sort(&["name", "slug", "status", "created_at"], "created_at");
    let tag_filter = params.filters.get("tag").cloned();
    let mut items: Vec<Software> = state
        .software
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|software| {
            matches_search(
                &format!("{} {} {}", software.name, software.slug, software.description),
                &params,
            ) && params.filters.get("kind").is_none_or(|kind| {
                software.kind == *kind
            }) && params.filters.get("status").is_none_or(|status| {
                software.status == *status
            }) && params.filters.get("group_id").is_none_or(|group_id| {
                software.group_id.as_deref() == Some(group_id.as_str())
            }) && params.filters.get("category_id").is_none_or(|category_id| {
                software.category_id.as_deref() == Some(category_id.as_str())
            }) && params.filters.get("vendor_company_id").is_none_or(|vendor| {
                software.vendor_company_id.as_deref() == Some(vendor.as_str())
            }) && tag_filter.as_ref().is_none_or(|tag_id| {
                state
                    .software_tags
                    .get(&software.id)
                    .is_some_and(|tags| tags.contains(tag_id.as_str()))
            })
        })
        .collect();
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        "slug" => items.sort_by(|a, b| a.slug.cmp(&b.slug)),
        "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

fn validate_software_refs(
    state: &SoftwareState,
    vendor_company_id: Option<&str>,
    group_id: Option<&str>,
    category_id: Option<&str>,
) -> UnifiedResult<()> {
    if let Some(vendor) = vendor_company_id {
        if vendor.trim().is_empty() {
            return Err(UnifiedError::bad_request("Vendor company id must not be empty"));
        }
        if vendor.parse::<Uuid>().is_err() {
            return Err(UnifiedError::bad_request(
                "Vendor company id must be a UUID",
            ));
        }
        if !state.vendor_exists(vendor) {
            return Err(UnifiedError::not_found("Vendor company not found"));
        }
    }
    if let Some(group_id) = group_id {
        match state.taxonomy.get(group_id) {
            Some(node) if node.kind == "group" => {}
            _ => return Err(UnifiedError::not_found("Group not found")),
        }
    }
    if let Some(category_id) = category_id {
        match state.taxonomy.get(category_id) {
            Some(node) if node.kind == "category" => {}
            _ => return Err(UnifiedError::not_found("Category not found")),
        }
    }
    Ok(())
}

async fn create_software(
    state: web::Data<Arc<SoftwareState>>,
    body: web::Json<CreateSoftware>,
) -> HttpResponse {
    let result: UnifiedResult<SoftwareDetail> = (|| {
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Name must not be empty"));
        }
        validate_kind(&input.kind)?;
        let slug = match &input.slug {
            Some(slug) if !slug.trim().is_empty() => slugify(slug),
            _ => slugify(&input.name),
        };
        if slug.is_empty() {
            return Err(UnifiedError::bad_request(
                "Could not derive a slug from the software name",
            ));
        }
        if state
            .software
            .iter()
            .any(|entry| entry.value().slug == slug)
        {
            return Err(UnifiedError::conflict(format!(
                "Software with slug \"{slug}\" already exists"
            )));
        }
        validate_software_refs(
            &state,
            input.vendor_company_id.as_deref(),
            input.group_id.as_deref(),
            input.category_id.as_deref(),
        )?;
        for url in [
            ("Homepage URL", &input.homepage_url),
            ("Repository URL", &input.repository_url),
            ("Documentation URL", &input.documentation_url),
        ] {
            if let Some(value) = url.1 {
                validate_url(value, url.0)?;
            }
        }
        for screenshot in &input.screenshots {
            validate_url(screenshot, "Screenshot URL")?;
        }
        for tag_id in &input.tag_ids {
            if !state.tags.contains_key(tag_id.as_str()) {
                return Err(UnifiedError::not_found(format!(
                    "Tag \"{tag_id}\" not found"
                )));
            }
        }
        let software = Software {
            id: new_id(),
            slug,
            name: input.name.trim().to_string(),
            kind: input.kind,
            description: input.description.unwrap_or_default(),
            vendor_company_id: input.vendor_company_id,
            group_id: input.group_id,
            category_id: input.category_id,
            status: "draft".to_string(),
            homepage_url: input.homepage_url,
            repository_url: input.repository_url,
            documentation_url: input.documentation_url,
            screenshots: input.screenshots,
            created_at: now(),
            updated_at: now(),
        };
        state
            .software
            .insert(software.id.clone(), software.clone());
        if !input.tag_ids.is_empty() {
            state.software_tags.insert(
                software.id.clone(),
                input.tag_ids.iter().cloned().collect(),
            );
        }
        let detail = state.software_detail(&software);
        state.emit(
            "software.created",
            serde_json::to_value(&detail).unwrap_or(Value::Null),
        );
        Ok(detail)
    })();
    respond_created(result)
}

async fn get_software(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<SoftwareDetail> = state
        .software
        .get(path.as_str())
        .map(|entry| state.software_detail(entry.value()))
        .ok_or_else(|| UnifiedError::not_found("Software not found"));
    json::respond(result, false)
}

async fn update_software(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
    body: web::Json<UpdateSoftware>,
) -> HttpResponse {
    let result: UnifiedResult<SoftwareDetail> = (|| {
        let input = body.into_inner();
        // Slug uniqueness is checked before taking the write lock:
        // iterating the map while holding a RefMut on one of its
        // entries would deadlock on the shard lock.
        let new_slug = input
            .slug
            .as_ref()
            .map(|slug| slugify(slug));
        if let Some(slug) = &new_slug {
            if slug.is_empty() {
                return Err(UnifiedError::bad_request("Slug must not be empty"));
            }
            if state
                .software
                .iter()
                .any(|entry| entry.value().slug == *slug && entry.key() != path.as_str())
            {
                return Err(UnifiedError::conflict(format!(
                    "Software with slug \"{slug}\" already exists"
                )));
            }
        }
        let mut software = state
            .software
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Software not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Name must not be empty"));
            }
            software.name = name.trim().to_string();
        }
        if let Some(slug) = new_slug {
            software.slug = slug;
        }
        if let Some(kind) = input.kind {
            validate_kind(&kind)?;
            software.kind = kind;
        }
        if let Some(description) = input.description {
            software.description = description;
        }
        if let Some(vendor) = input.vendor_company_id {
            validate_software_refs(&state, Some(&vendor), None, None)?;
            software.vendor_company_id = Some(vendor);
        }
        if let Some(group_id) = input.group_id {
            validate_software_refs(&state, None, Some(&group_id), None)?;
            software.group_id = Some(group_id);
        }
        if let Some(category_id) = input.category_id {
            validate_software_refs(&state, None, None, Some(&category_id))?;
            software.category_id = Some(category_id);
        }
        for (field, value) in [
            ("Homepage URL", &input.homepage_url),
            ("Repository URL", &input.repository_url),
            ("Documentation URL", &input.documentation_url),
        ] {
            if let Some(url) = value {
                validate_url(url, field)?;
            }
        }
        if let Some(url) = input.homepage_url {
            software.homepage_url = Some(url);
        }
        if let Some(url) = input.repository_url {
            software.repository_url = Some(url);
        }
        if let Some(url) = input.documentation_url {
            software.documentation_url = Some(url);
        }
        if let Some(screenshots) = input.screenshots {
            for screenshot in &screenshots {
                validate_url(screenshot, "Screenshot URL")?;
            }
            software.screenshots = screenshots;
        }
        if let Some(status) = input.status {
            if !SOFTWARE_STATUSES.contains(&status.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "Status must be one of: {}",
                    SOFTWARE_STATUSES.join(", ")
                )));
            }
            validate_status_transition(&software.status, &status)?;
            software.status = status;
        }
        software.updated_at = now();
        let detail = state.software_detail(&software.clone());
        drop(software);
        state.emit(
            "software.updated",
            serde_json::to_value(&detail).unwrap_or(Value::Null),
        );
        Ok(detail)
    })();
    json::respond(result, false)
}

async fn delete_software(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let in_use = state
            .versions
            .iter()
            .any(|entry| entry.value().software_id.as_deref() == Some(path.as_str()));
        if in_use {
            return Err(UnifiedError::conflict(
                "Software has versions; delete the versions first",
            ));
        }
        let removed = state
            .software
            .remove(path.as_str())
            .map(|(_, software)| software);
        match removed {
            Some(software) => {
                state.software_tags.remove(path.as_str());
                state.emit(
                    "software.deleted",
                    serde_json::to_value(&software).unwrap_or(Value::Null),
                );
                Ok(json!({ "deleted": true, "id": path.as_str() }))
            }
            None => Err(UnifiedError::not_found("Software not found")),
        }
    })();
    json::respond(result, false)
}

async fn publish_software(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<SoftwareDetail> = (|| {
        let mut software = state
            .software
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Software not found"))?;
        validate_status_transition(&software.status, "published")?;
        software.status = "published".to_string();
        software.updated_at = now();
        let detail = state.software_detail(&software.clone());
        drop(software);
        state.emit(
            "software.published",
            serde_json::to_value(&detail).unwrap_or(Value::Null),
        );
        Ok(detail)
    })();
    json::respond(result, false)
}

async fn unpublish_software(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<SoftwareDetail> = (|| {
        let mut software = state
            .software
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Software not found"))?;
        validate_status_transition(&software.status, "draft")?;
        software.status = "draft".to_string();
        software.updated_at = now();
        let detail = state.software_detail(&software.clone());
        drop(software);
        state.emit(
            "software.updated",
            serde_json::to_value(&detail).unwrap_or(Value::Null),
        );
        Ok(detail)
    })();
    json::respond(result, false)
}

async fn list_software_versions(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    if !state.software.contains_key(path.as_str()) {
        return json::error_response(&UnifiedError::not_found("Software not found"), false);
    }
    let mut items: Vec<SoftwareVersion> = state
        .versions
        .iter()
        .filter(|entry| entry.value().software_id.as_deref() == Some(path.as_str()))
        .map(|entry| entry.value().clone())
        .filter(|version| {
            matches_search(
                &format!("{} {} {}", version.product, version.version, version.changelog),
                &params,
            )
        })
        .collect();
    items.sort_by(|a, b| {
        let parsed_a = Version::parse(&a.version);
        let parsed_b = Version::parse(&b.version);
        match (parsed_a, parsed_b) {
            (Ok(left), Ok(right)) => left.cmp(&right),
            _ => a.version.cmp(&b.version),
        }
    });
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn software_release_history(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    if !state.software.contains_key(path.as_str()) {
        return json::error_response(&UnifiedError::not_found("Software not found"), false);
    }
    let mut items: Vec<SoftwareReleaseHistory> = state
        .releases
        .iter()
        .filter_map(|entry| {
            let release = entry.value().clone();
            let version = state.versions.get(&release.version_id)?;
            if version.software_id.as_deref() != Some(path.as_str()) {
                return None;
            }
            let version = version.value().clone();
            if !params
                .filters
                .get("channel")
                .is_none_or(|channel| release.channel == *channel)
                || !params
                    .filters
                    .get("status")
                    .is_none_or(|status| release.status == *status)
            {
                return None;
            }
            Some(SoftwareReleaseHistory {
                release,
                version: version.version,
                changelog: version.changelog,
                license_required: version.license_required,
            })
        })
        .collect();
    items.sort_by(|a, b| {
        b.release
            .published_at
            .cmp(&a.release.published_at)
            .then_with(|| b.release.created_at.cmp(&a.release.created_at))
    });
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn latest_releases(
    state: web::Data<Arc<SoftwareState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<LatestRelease> = state
        .releases
        .iter()
        .filter(|entry| entry.value().status == "published")
        .filter_map(|entry| {
            let release = entry.value().clone();
            let version = state.versions.get(&release.version_id)?.value().clone();
            let (software_name, software_slug) = match &version.software_id {
                Some(id) => state.software.get(id).map(|entry| {
                    (Some(entry.name.clone()), Some(entry.slug.clone()))
                }).unwrap_or((None, None)),
                None => (None, None),
            };
            if !params
                .filters
                .get("channel")
                .is_none_or(|channel| release.channel == *channel)
                || !params.filters.get("software_id").is_none_or(|software_id| {
                    version.software_id.as_deref() == Some(software_id.as_str())
                })
            {
                return None;
            }
            Some(LatestRelease {
                release,
                version: version.version,
                product: version.product,
                software_id: version.software_id,
                software_name,
                software_slug,
            })
        })
        .collect();
    items.sort_by(|a, b| {
        b.release
            .published_at
            .cmp(&a.release.published_at)
            .then_with(|| b.release.created_at.cmp(&a.release.created_at))
    });
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

// ---------------------------------------------------------------------------
// Software tag assignment handlers
// ---------------------------------------------------------------------------

async fn list_software_tags(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    if !state.software.contains_key(path.as_str()) {
        return json::error_response(&UnifiedError::not_found("Software not found"), false);
    }
    let tag_ids = state
        .software_tags
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .unwrap_or_default();
    let mut items: Vec<Tag> = tag_ids
        .iter()
        .filter_map(|id| state.tags.get(id).map(|entry| entry.value().clone()))
        .filter(|tag| matches_search(&format!("{} {}", tag.name, tag.slug), &params))
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn attach_software_tag(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
    body: web::Json<AttachTag>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        if !state.software.contains_key(path.as_str()) {
            return Err(UnifiedError::not_found("Software not found"));
        }
        if !state.tags.contains_key(body.tag_id.as_str()) {
            return Err(UnifiedError::not_found("Tag not found"));
        }
        let mut tags = state
            .software_tags
            .entry(path.as_str().to_string())
            .or_default();
        if !tags.insert(body.tag_id.clone()) {
            return Err(UnifiedError::conflict("Tag is already attached to this software"));
        }
        state.emit(
            "software.tag.attached",
            json!({ "software_id": path.as_str(), "tag_id": body.tag_id }),
        );
        Ok(json!({ "software_id": path.as_str(), "tag_id": body.tag_id }))
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn detach_software_tag(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let (software_id, tag_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        if !state.software.contains_key(software_id.as_str()) {
            return Err(UnifiedError::not_found("Software not found"));
        }
        let mut tags = state
            .software_tags
            .get_mut(software_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Tag is not attached to this software"))?;
        if !tags.remove(tag_id.as_str()) {
            return Err(UnifiedError::not_found("Tag is not attached to this software"));
        }
        state.emit(
            "software.tag.detached",
            json!({ "software_id": software_id, "tag_id": tag_id }),
        );
        Ok(json!({ "detached": true, "software_id": software_id, "tag_id": tag_id }))
    })();
    json::respond(result, false)
}

async fn get_stats(state: web::Data<Arc<SoftwareState>>) -> HttpResponse {
    let total_downloads: u64 = state
        .releases
        .iter()
        .map(|entry| entry.value().download_count)
        .sum();
    let downloads_by_release: Vec<Value> = state
        .releases
        .iter()
        .map(|entry| {
            json!({
                "release_id": entry.value().id,
                "version_id": entry.value().version_id,
                "download_count": entry.value().download_count,
            })
        })
        .collect();
    let mut by_status: HashMap<String, u64> = HashMap::new();
    for entry in state.software.iter() {
        *by_status.entry(entry.value().status.clone()).or_default() += 1;
    }
    let mut by_kind: HashMap<String, u64> = HashMap::new();
    for entry in state.software.iter() {
        *by_kind.entry(entry.value().kind.clone()).or_default() += 1;
    }
    json::success_response(
        json!({
            "total_software": state.software.len(),
            "software_by_status": by_status,
            "software_by_kind": by_kind,
            "total_versions": state.versions.len(),
            "total_releases": state.releases.len(),
            "total_release_files": state.release_files.len(),
            "total_groups": state.taxonomy.iter().filter(|entry| entry.value().kind == "group").count(),
            "total_categories": state.taxonomy.iter().filter(|entry| entry.value().kind == "category").count(),
            "total_tags": state.tags.len(),
            "total_downloads": total_downloads,
            "downloads_by_release": downloads_by_release,
        }),
        200,
    )
}

// ---------------------------------------------------------------------------
// Release file handlers
// ---------------------------------------------------------------------------

async fn create_release_file(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
    body: web::Json<CreateReleaseFile>,
) -> HttpResponse {
    let result: UnifiedResult<ReleaseFile> = (|| {
        let input = body.into_inner();
        let release = state
            .releases
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Release not found"))?;
        if input.filename.trim().is_empty() {
            return Err(UnifiedError::bad_request("Filename must not be empty"));
        }
        if let Some(checksum) = &input.checksum_sha256 {
            validate_hex_checksum(&checksum.to_lowercase(), 64, "checksum_sha256")?;
        }
        if let Some(checksum) = &input.checksum_sha512 {
            validate_hex_checksum(&checksum.to_lowercase(), 128, "checksum_sha512")?;
        }
        if let Some(url) = &input.download_url {
            validate_url(url, "Download URL")?;
        }
        if input.download_url.is_none() && input.storage_path.is_none() {
            return Err(UnifiedError::bad_request(
                "Either download_url or storage_path is required",
            ));
        }
        let integrity_status = if input.checksum_sha256.is_some() || input.checksum_sha512.is_some() {
            "pending".to_string()
        } else {
            "unverified".to_string()
        };
        let file = ReleaseFile {
            id: new_id(),
            release_id: release.id,
            version_id: release.version_id,
            filename: input.filename.trim().to_string(),
            size_bytes: input.size_bytes,
            checksum_sha256: input.checksum_sha256.map(|value| value.to_lowercase()),
            checksum_sha512: input.checksum_sha512.map(|value| value.to_lowercase()),
            download_url: input.download_url,
            storage_path: input.storage_path,
            integrity_status,
            verified_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state.release_files.insert(file.id.clone(), file.clone());
        state.emit(
            "software.file.created",
            serde_json::to_value(&file).unwrap_or(Value::Null),
        );
        Ok(file)
    })();
    respond_created(result)
}

async fn get_release_file(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let (release_id, file_id) = path.into_inner();
    let result: UnifiedResult<ReleaseFile> = state
        .release_files
        .get(file_id.as_str())
        .filter(|entry| entry.value().release_id == release_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Release file not found"));
    json::respond(result, false)
}

async fn update_release_file(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateReleaseFile>,
) -> HttpResponse {
    let (release_id, file_id) = path.into_inner();
    let result: UnifiedResult<ReleaseFile> = (|| {
        let input = body.into_inner();
        let mut file = state
            .release_files
            .get_mut(file_id.as_str())
            .filter(|entry| entry.release_id == release_id)
            .ok_or_else(|| UnifiedError::not_found("Release file not found"))?;
        if let Some(filename) = input.filename {
            if filename.trim().is_empty() {
                return Err(UnifiedError::bad_request("Filename must not be empty"));
            }
            file.filename = filename.trim().to_string();
        }
        if let Some(size_bytes) = input.size_bytes {
            file.size_bytes = size_bytes;
        }
        if let Some(checksum) = input.checksum_sha256 {
            validate_hex_checksum(&checksum.to_lowercase(), 64, "checksum_sha256")?;
            file.checksum_sha256 = Some(checksum.to_lowercase());
            file.integrity_status = "pending".to_string();
            file.verified_at = None;
        }
        if let Some(checksum) = input.checksum_sha512 {
            validate_hex_checksum(&checksum.to_lowercase(), 128, "checksum_sha512")?;
            file.checksum_sha512 = Some(checksum.to_lowercase());
            file.integrity_status = "pending".to_string();
            file.verified_at = None;
        }
        if let Some(url) = input.download_url {
            validate_url(&url, "Download URL")?;
            file.download_url = Some(url);
        }
        if let Some(path) = input.storage_path {
            file.storage_path = Some(path);
        }
        file.updated_at = now();
        Ok(file.clone())
    })();
    json::respond(result, false)
}

async fn delete_release_file(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let (release_id, file_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let belongs = state
            .release_files
            .get(file_id.as_str())
            .is_some_and(|entry| entry.value().release_id == release_id);
        if !belongs {
            return Err(UnifiedError::not_found("Release file not found"));
        }
        state.release_files.remove(file_id.as_str());
        Ok(json!({ "deleted": true, "id": file_id }))
    })();
    json::respond(result, false)
}

async fn verify_release_file(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<(String, String)>,
    body: web::Json<VerifyFile>,
) -> HttpResponse {
    let (release_id, file_id) = path.into_inner();
    let result: UnifiedResult<ReleaseFile> = (|| {
        let content = BASE64.decode(body.content_base64.trim()).map_err(|_| {
            UnifiedError::bad_request("content_base64 is not valid base64")
        })?;
        let mut file = state
            .release_files
            .get_mut(file_id.as_str())
            .filter(|entry| entry.release_id == release_id)
            .ok_or_else(|| UnifiedError::not_found("Release file not found"))?;
        if content.len() as u64 != file.size_bytes {
            file.integrity_status = "failed".to_string();
            file.verified_at = Some(now());
            file.updated_at = now();
            return Err(UnifiedError::unprocessable(format!(
                "Size mismatch: content is {} bytes, expected {} bytes",
                content.len(),
                file.size_bytes
            )));
        }
        let computed_sha256 = sha256_hex(&content);
        let computed_sha512 = sha512_hex(&content);
        let sha256_ok = file
            .checksum_sha256
            .as_ref()
            .is_none_or(|expected| *expected == computed_sha256);
        let sha512_ok = file
            .checksum_sha512
            .as_ref()
            .is_none_or(|expected| *expected == computed_sha512);
        if file.checksum_sha256.is_none() {
            file.checksum_sha256 = Some(computed_sha256);
        }
        if file.checksum_sha512.is_none() {
            file.checksum_sha512 = Some(computed_sha512);
        }
        file.verified_at = Some(now());
        file.updated_at = now();
        if sha256_ok && sha512_ok {
            file.integrity_status = "verified".to_string();
            let verified = file.clone();
            drop(file);
            state.emit(
                "software.file.verified",
                serde_json::to_value(&verified).unwrap_or(Value::Null),
            );
            Ok(verified)
        } else {
            file.integrity_status = "failed".to_string();
            let failed = file.clone();
            drop(file);
            state.emit(
                "software.file.verified",
                serde_json::to_value(&failed).unwrap_or(Value::Null),
            );
            Ok(failed)
        }
    })();
    // A failed verification is a real result, not a handler error: the
    // file record (with integrity_status "failed") is the response.
    match result {
        Ok(file) => json::success_response(file, 200),
        Err(error) => json::error_response(&error, false),
    }
}

// ---------------------------------------------------------------------------
// Taxonomy handlers
// ---------------------------------------------------------------------------

async fn list_taxonomy(
    state: web::Data<Arc<SoftwareState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let sort = params.allowed_sort(&["name", "slug", "sort_order", "created_at"], "sort_order");
    let mut items: Vec<TaxonomyNode> = state
        .taxonomy
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|node| {
            matches_search(&format!("{} {} {}", node.name, node.slug, node.description), &params)
                && params.filters.get("kind").is_none_or(|kind| node.kind == *kind)
                && params.filters.get("parent_id").is_none_or(|parent_id| {
                    node.parent_id.as_deref() == Some(parent_id.as_str())
                })
        })
        .collect();
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        "slug" => items.sort_by(|a, b| a.slug.cmp(&b.slug)),
        "created_at" => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        _ => items.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then_with(|| a.name.cmp(&b.name))
        }),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_taxonomy_node(
    state: web::Data<Arc<SoftwareState>>,
    body: web::Json<CreateTaxonomyNode>,
) -> HttpResponse {
    let result: UnifiedResult<TaxonomyNode> = (|| {
        let input = body.into_inner();
        if !TAXONOMY_KINDS.contains(&input.kind.as_str()) {
            return Err(UnifiedError::bad_request(
                "Kind must be group or category",
            ));
        }
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Name must not be empty"));
        }
        let slug = match &input.slug {
            Some(slug) if !slug.trim().is_empty() => slugify(slug),
            _ => slugify(&input.name),
        };
        if slug.is_empty() {
            return Err(UnifiedError::bad_request(
                "Could not derive a slug from the taxonomy name",
            ));
        }
        if let Some(parent_id) = &input.parent_id {
            match state.taxonomy.get(parent_id.as_str()) {
                Some(parent) if parent.kind == input.kind => {}
                _ => {
                    return Err(UnifiedError::not_found(
                        "Parent node not found or has a different kind",
                    ))
                }
            }
        }
        if state.taxonomy.iter().any(|entry| {
            entry.value().kind == input.kind
                && entry.value().slug == slug
                && entry.value().parent_id == input.parent_id
        }) {
            return Err(UnifiedError::conflict(format!(
                "A {} with slug \"{slug}\" already exists under this parent",
                input.kind
            )));
        }
        let (depth, path) = taxonomy_path(&state, input.parent_id.as_deref(), &slug);
        let node = TaxonomyNode {
            id: new_id(),
            kind: input.kind,
            slug,
            name: input.name.trim().to_string(),
            description: input.description.unwrap_or_default(),
            parent_id: input.parent_id,
            sort_order: input.sort_order.unwrap_or(0),
            depth,
            path,
            created_at: now(),
            updated_at: now(),
        };
        state.taxonomy.insert(node.id.clone(), node.clone());
        state.emit(
            "software.taxonomy.created",
            serde_json::to_value(&node).unwrap_or(Value::Null),
        );
        Ok(node)
    })();
    respond_created(result)
}

async fn get_taxonomy_node(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<TaxonomyNode> = state
        .taxonomy
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Taxonomy node not found"));
    json::respond(result, false)
}

async fn update_taxonomy_node(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
    body: web::Json<UpdateTaxonomyNode>,
) -> HttpResponse {
    let result: UnifiedResult<TaxonomyNode> = (|| {
        let input = body.into_inner();
        // Slug uniqueness is checked before taking the write lock.
        let new_slug = input.slug.as_ref().map(|slug| slugify(slug));
        if let Some(slug) = &new_slug {
            if slug.is_empty() {
                return Err(UnifiedError::bad_request("Slug must not be empty"));
            }
            let (kind, parent_id) = state
                .taxonomy
                .get(path.as_str())
                .map(|entry| (entry.value().kind.clone(), entry.value().parent_id.clone()))
                .ok_or_else(|| UnifiedError::not_found("Taxonomy node not found"))?;
            if state.taxonomy.iter().any(|entry| {
                entry.value().kind == kind
                    && entry.value().slug == *slug
                    && entry.value().parent_id == parent_id
                    && entry.key() != path.as_str()
            }) {
                return Err(UnifiedError::conflict(format!(
                    "A {kind} with slug \"{slug}\" already exists under this parent"
                )));
            }
        }
        let mut node = state
            .taxonomy
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Taxonomy node not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Name must not be empty"));
            }
            node.name = name.trim().to_string();
        }
        if let Some(slug) = new_slug {
            node.slug = slug;
        }
        if let Some(description) = input.description {
            node.description = description;
        }
        if let Some(sort_order) = input.sort_order {
            node.sort_order = sort_order;
        }
        node.updated_at = now();
        let updated = node.clone();
        drop(node);
        recompute_subtree(&state, path.as_str());
        let updated = state
            .taxonomy
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .unwrap_or(updated);
        state.emit(
            "software.taxonomy.updated",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_taxonomy_node(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let node = state
            .taxonomy
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Taxonomy node not found"))?;
        if state
            .taxonomy
            .iter()
            .any(|entry| entry.value().parent_id.as_deref() == Some(path.as_str()))
        {
            return Err(UnifiedError::conflict(
                "Taxonomy node has children; move or delete the children first",
            ));
        }
        let referenced = state.software.iter().any(|entry| {
            (node.kind == "group" && entry.value().group_id.as_deref() == Some(path.as_str()))
                || (node.kind == "category"
                    && entry.value().category_id.as_deref() == Some(path.as_str()))
        });
        if referenced {
            return Err(UnifiedError::conflict(
                "Taxonomy node is referenced by software records",
            ));
        }
        state.taxonomy.remove(path.as_str());
        state.emit(
            "software.taxonomy.deleted",
            serde_json::to_value(&node).unwrap_or(Value::Null),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, false)
}

async fn move_taxonomy_node(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
    body: web::Json<MoveTaxonomyNode>,
) -> HttpResponse {
    let result: UnifiedResult<TaxonomyNode> = (|| {
        let input = body.into_inner();
        let kind = state
            .taxonomy
            .get(path.as_str())
            .map(|entry| entry.value().kind.clone())
            .ok_or_else(|| UnifiedError::not_found("Taxonomy node not found"))?;
        if let Some(parent_id) = &input.parent_id {
            if parent_id == path.as_str() {
                return Err(UnifiedError::bad_request(
                    "A node cannot be its own parent",
                ));
            }
            match state.taxonomy.get(parent_id.as_str()) {
                Some(parent) if parent.kind == kind => {}
                _ => {
                    return Err(UnifiedError::not_found(
                        "Parent node not found or has a different kind",
                    ))
                }
            }
            if is_descendant_of(&state, parent_id, path.as_str()) {
                return Err(UnifiedError::bad_request(
                    "A node cannot be moved under one of its own descendants",
                ));
            }
        }
        let mut node = state
            .taxonomy
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Taxonomy node not found"))?;
        node.parent_id = input.parent_id;
        if let Some(sort_order) = input.sort_order {
            node.sort_order = sort_order;
        }
        node.updated_at = now();
        drop(node);
        recompute_subtree(&state, path.as_str());
        let moved = state
            .taxonomy
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .expect("node still present");
        state.emit(
            "software.taxonomy.updated",
            serde_json::to_value(&moved).unwrap_or(Value::Null),
        );
        Ok(moved)
    })();
    json::respond(result, false)
}

async fn list_taxonomy_children(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    if !state.taxonomy.contains_key(path.as_str()) {
        return json::error_response(&UnifiedError::not_found("Taxonomy node not found"), false);
    }
    let mut items: Vec<TaxonomyNode> = state
        .taxonomy
        .iter()
        .filter(|entry| entry.value().parent_id.as_deref() == Some(path.as_str()))
        .map(|entry| entry.value().clone())
        .filter(|node| {
            matches_search(&format!("{} {} {}", node.name, node.slug, node.description), &params)
        })
        .collect();
    items.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then_with(|| a.name.cmp(&b.name))
    });
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn taxonomy_tree(
    state: web::Data<Arc<SoftwareState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let kind = params.filters.get("kind").cloned();
    if let Some(kind) = &kind {
        if !TAXONOMY_KINDS.contains(&kind.as_str()) {
            return json::error_response(
                &UnifiedError::bad_request("Kind must be group or category"),
                false,
            );
        }
    }
    let roots = build_tree(&state, None, kind.as_deref());
    let page = paginate(roots, &params);
    json::success_response(page, 200)
}

// ---------------------------------------------------------------------------
// Tag handlers
// ---------------------------------------------------------------------------

async fn list_tags(
    state: web::Data<Arc<SoftwareState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let sort = params.allowed_sort(&["name", "slug", "created_at"], "created_at");
    let mut items: Vec<Tag> = state
        .tags
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|tag| matches_search(&format!("{} {}", tag.name, tag.slug), &params))
        .collect();
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        "slug" => items.sort_by(|a, b| a.slug.cmp(&b.slug)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_tag(
    state: web::Data<Arc<SoftwareState>>,
    body: web::Json<CreateTag>,
) -> HttpResponse {
    let result: UnifiedResult<Tag> = (|| {
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Name must not be empty"));
        }
        let slug = match &input.slug {
            Some(slug) if !slug.trim().is_empty() => slugify(slug),
            _ => slugify(&input.name),
        };
        if slug.is_empty() {
            return Err(UnifiedError::bad_request(
                "Could not derive a slug from the tag name",
            ));
        }
        if state.tags.iter().any(|entry| entry.value().slug == slug) {
            return Err(UnifiedError::conflict(format!(
                "Tag with slug \"{slug}\" already exists"
            )));
        }
        let tag = Tag {
            id: new_id(),
            slug,
            name: input.name.trim().to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state.tags.insert(tag.id.clone(), tag.clone());
        state.emit(
            "software.taxonomy.created",
            serde_json::to_value(&tag).unwrap_or(Value::Null),
        );
        Ok(tag)
    })();
    respond_created(result)
}

async fn get_tag(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Tag> = state
        .tags
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Tag not found"));
    json::respond(result, false)
}

async fn update_tag(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
    body: web::Json<UpdateTag>,
) -> HttpResponse {
    let result: UnifiedResult<Tag> = (|| {
        let input = body.into_inner();
        // Slug uniqueness is checked before taking the write lock.
        let new_slug = input.slug.as_ref().map(|slug| slugify(slug));
        if let Some(slug) = &new_slug {
            if slug.is_empty() {
                return Err(UnifiedError::bad_request("Slug must not be empty"));
            }
            if state
                .tags
                .iter()
                .any(|entry| entry.value().slug == *slug && entry.key() != path.as_str())
            {
                return Err(UnifiedError::conflict(format!(
                    "Tag with slug \"{slug}\" already exists"
                )));
            }
        }
        let mut tag = state
            .tags
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Tag not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Name must not be empty"));
            }
            tag.name = name.trim().to_string();
        }
        if let Some(slug) = new_slug {
            tag.slug = slug;
        }
        tag.updated_at = now();
        let updated = tag.clone();
        drop(tag);
        state.emit(
            "software.taxonomy.updated",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_tag(
    state: web::Data<Arc<SoftwareState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tag = state
            .tags
            .remove(path.as_str())
            .map(|(_, tag)| tag)
            .ok_or_else(|| UnifiedError::not_found("Tag not found"))?;
        for mut entry in state.software_tags.iter_mut() {
            entry.value_mut().remove(path.as_str());
        }
        state.emit(
            "software.taxonomy.deleted",
            serde_json::to_value(&tag).unwrap_or(Value::Null),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, false)
}

async fn tag_cloud(
    state: web::Data<Arc<SoftwareState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let sort = params.allowed_sort(&["name", "software_count", "created_at"], "software_count");
    let mut counts: HashMap<String, u64> = HashMap::new();
    for entry in state.software_tags.iter() {
        for tag_id in entry.value().iter() {
            *counts.entry(tag_id.clone()).or_default() += 1;
        }
    }
    let mut items: Vec<TagCount> = state
        .tags
        .iter()
        .map(|entry| TagCount {
            tag: entry.value().clone(),
            software_count: counts.get(entry.key()).copied().unwrap_or(0),
        })
        .filter(|item| matches_search(&format!("{} {}", item.tag.name, item.tag.slug), &params))
        .collect();
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.tag.name.cmp(&b.tag.name)),
        "created_at" => items.sort_by(|a, b| a.tag.created_at.cmp(&b.tag.created_at)),
        _ => items.sort_by(|a, b| {
            b.software_count
                .cmp(&a.software_count)
                .then_with(|| a.tag.name.cmp(&b.tag.name))
        }),
    }
    if params.normalized().order == "asc" && sort == "software_count" {
        items.reverse();
    } else if params.normalized().order == "desc" && sort != "software_count" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

// ---------------------------------------------------------------------------
// Hook handlers: VCS import and validation.
// ---------------------------------------------------------------------------

/// Answer `vcs.software.validate` (from the VCS module) with
/// `{ "valid": bool }`, so a repository connection can only point at a
/// software record that really exists. A handler that answers
/// `valid: false` rejects the connection with 422.
fn on_vcs_software_validate(state: Arc<SoftwareState>, ctx: &mut HookContext) {
    let software_id = ctx
        .get("software_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let valid = !software_id.trim().is_empty() && state.software.contains_key(software_id);
    ctx.add_result(json!({ "valid": valid }));
}

/// Create a draft software version for a release imported from a VCS
/// provider (`vcs.release.imported`, payload is the imported version
/// draft). The version is always a draft: nothing here publishes it,
/// publishing stays a manual step through
/// `POST /api/v1/software/releases/{id}/publish` after a release is
/// created for the version. Importing the same tag twice is a no-op:
/// when a draft version already exists for the same software record
/// and version string it is reported and skipped, so a re-sync never
/// creates duplicates. A draft whose software record does not exist
/// is skipped rather than creating an orphan version.
fn on_vcs_release_imported(state: Arc<SoftwareState>, ctx: &mut HookContext) {
    let software_id = ctx
        .get("software_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_default();
    let version = ctx
        .get("version")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_default();
    let tag = ctx
        .get("tag")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_default();
    if software_id.trim().is_empty() || version.trim().is_empty() {
        ctx.add_result(json!({
            "created": false,
            "reason": "software_id and version are required",
        }));
        return;
    }
    if validate_version_string(&version).is_err() {
        ctx.add_result(json!({
            "created": false,
            "reason": format!("Version \"{version}\" is not a valid semantic version"),
            "tag": tag,
        }));
        return;
    }
    let Some(software) = state
        .software
        .get(software_id.as_str())
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "created": false,
            "reason": "software not found",
            "software_id": software_id,
            "tag": tag,
        }));
        return;
    };
    if let Some(existing) = state.versions.iter().find(|entry| {
        entry.value().software_id.as_deref() == Some(software_id.as_str())
            && entry.value().version == version
    }) {
        ctx.add_result(json!({
            "created": false,
            "duplicate": true,
            "version_id": existing.value().id,
            "software_id": software_id,
            "version": version,
            "tag": tag,
        }));
        return;
    }
    let changelog = ctx
        .get("changelog")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|changelog| !changelog.trim().is_empty())
        .or_else(|| {
            ctx.get("title")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
    let draft = SoftwareVersion {
        id: new_id(),
        product: software.slug.clone(),
        version: version.clone(),
        changelog,
        file_id: None,
        license_required: false,
        software_id: Some(software_id.clone()),
        requirements: VersionRequirements::default(),
        created_at: now(),
        updated_at: now(),
    };
    state.versions.insert(draft.id.clone(), draft.clone());
    ctx.add_result(json!({
        "created": true,
        "version_id": draft.id,
        "software_id": software_id,
        "software_slug": software.slug,
        "version": version,
        "tag": tag,
        "status": "draft",
    }));
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct SoftwareModule {
    state: Arc<SoftwareState>,
}

impl SoftwareModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(SoftwareState::new()),
        }
    }
}

impl Default for SoftwareModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for SoftwareModule {
    fn id(&self) -> &'static str {
        "software"
    }

    fn name(&self) -> &'static str {
        "Software"
    }

    fn description(&self) -> &'static str {
        "Enterprise software and assets manager: software records with a status lifecycle, versions with semver and platform requirements, releases with publish flow, per-release files with checksum integrity verification, hierarchical groups and categories, tags, full-text discovery, and license-gated delivery."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["licensing", "files"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "vcs.release.imported",
            "software",
            Arc::new(move |ctx: &mut HookContext| {
                on_vcs_release_imported(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "vcs.software.validate",
            "software",
            Arc::new(move |ctx: &mut HookContext| {
                on_vcs_software_validate(state.clone(), ctx)
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/software/versions", owner: "software", description: "List versions" },
            RouteInfo { method: "POST", path: "/api/v1/software/versions", owner: "software", description: "Create version" },
            RouteInfo { method: "GET", path: "/api/v1/software/versions/{id}", owner: "software", description: "Get version" },
            RouteInfo { method: "PUT", path: "/api/v1/software/versions/{id}", owner: "software", description: "Update version" },
            RouteInfo { method: "DELETE", path: "/api/v1/software/versions/{id}", owner: "software", description: "Delete version" },
            RouteInfo { method: "GET", path: "/api/v1/software/releases", owner: "software", description: "List releases" },
            RouteInfo { method: "POST", path: "/api/v1/software/releases", owner: "software", description: "Create release" },
            RouteInfo { method: "GET", path: "/api/v1/software/releases/{id}", owner: "software", description: "Get release" },
            RouteInfo { method: "DELETE", path: "/api/v1/software/releases/{id}", owner: "software", description: "Delete release" },
            RouteInfo { method: "POST", path: "/api/v1/software/releases/{id}/publish", owner: "software", description: "Publish release" },
            RouteInfo { method: "POST", path: "/api/v1/software/releases/{id}/unpublish", owner: "software", description: "Unpublish release" },
            RouteInfo { method: "GET", path: "/api/v1/software/releases/{id}/files", owner: "software", description: "Release files" },
            RouteInfo { method: "POST", path: "/api/v1/software/releases/{id}/files", owner: "software", description: "Create release file" },
            RouteInfo { method: "GET", path: "/api/v1/software/releases/{id}/files/{file_id}", owner: "software", description: "Get release file" },
            RouteInfo { method: "PUT", path: "/api/v1/software/releases/{id}/files/{file_id}", owner: "software", description: "Update release file" },
            RouteInfo { method: "DELETE", path: "/api/v1/software/releases/{id}/files/{file_id}", owner: "software", description: "Delete release file" },
            RouteInfo { method: "POST", path: "/api/v1/software/releases/{id}/files/{file_id}/verify", owner: "software", description: "Verify release file" },
            RouteInfo { method: "GET", path: "/api/v1/software/releases/{id}/download", owner: "software", description: "Download release" },
            RouteInfo { method: "GET", path: "/api/v1/software/stats", owner: "software", description: "Get stats" },
            RouteInfo { method: "GET", path: "/api/v1/software/latest", owner: "software", description: "Latest releases" },
            RouteInfo { method: "GET", path: "/api/v1/software/taxonomy", owner: "software", description: "List taxonomy" },
            RouteInfo { method: "POST", path: "/api/v1/software/taxonomy", owner: "software", description: "Create taxonomy node" },
            RouteInfo { method: "GET", path: "/api/v1/software/taxonomy/tree", owner: "software", description: "Taxonomy tree" },
            RouteInfo { method: "GET", path: "/api/v1/software/taxonomy/{id}", owner: "software", description: "Get taxonomy node" },
            RouteInfo { method: "PUT", path: "/api/v1/software/taxonomy/{id}", owner: "software", description: "Update taxonomy node" },
            RouteInfo { method: "DELETE", path: "/api/v1/software/taxonomy/{id}", owner: "software", description: "Delete taxonomy node" },
            RouteInfo { method: "POST", path: "/api/v1/software/taxonomy/{id}/move", owner: "software", description: "Move taxonomy node" },
            RouteInfo { method: "GET", path: "/api/v1/software/taxonomy/{id}/children", owner: "software", description: "List taxonomy children" },
            RouteInfo { method: "GET", path: "/api/v1/software/tags", owner: "software", description: "List tags" },
            RouteInfo { method: "POST", path: "/api/v1/software/tags", owner: "software", description: "Create tag" },
            RouteInfo { method: "GET", path: "/api/v1/software/tags/cloud", owner: "software", description: "Tag cloud" },
            RouteInfo { method: "GET", path: "/api/v1/software/tags/{id}", owner: "software", description: "Get tag" },
            RouteInfo { method: "PUT", path: "/api/v1/software/tags/{id}", owner: "software", description: "Update tag" },
            RouteInfo { method: "DELETE", path: "/api/v1/software/tags/{id}", owner: "software", description: "Delete tag" },
            RouteInfo { method: "GET", path: "/api/v1/software", owner: "software", description: "List software" },
            RouteInfo { method: "POST", path: "/api/v1/software", owner: "software", description: "Create software" },
            RouteInfo { method: "GET", path: "/api/v1/software/{id}", owner: "software", description: "Get software" },
            RouteInfo { method: "PUT", path: "/api/v1/software/{id}", owner: "software", description: "Update software" },
            RouteInfo { method: "DELETE", path: "/api/v1/software/{id}", owner: "software", description: "Delete software" },
            RouteInfo { method: "POST", path: "/api/v1/software/{id}/publish", owner: "software", description: "Publish software" },
            RouteInfo { method: "POST", path: "/api/v1/software/{id}/unpublish", owner: "software", description: "Unpublish software" },
            RouteInfo { method: "GET", path: "/api/v1/software/{id}/versions", owner: "software", description: "List software versions" },
            RouteInfo { method: "GET", path: "/api/v1/software/{id}/releases", owner: "software", description: "Software release history" },
            RouteInfo { method: "GET", path: "/api/v1/software/{id}/tags", owner: "software", description: "List software tags" },
            RouteInfo { method: "POST", path: "/api/v1/software/{id}/tags", owner: "software", description: "Attach software tag" },
            RouteInfo { method: "DELETE", path: "/api/v1/software/{id}/tags/{tag_id}", owner: "software", description: "Detach software tag" },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        // Specific single-segment paths are registered before the
        // generic `/{id}` routes so they are matched first.
        cfg.service(
            web::scope("/api/v1/software")
                .route("/versions", web::get().to(list_versions))
                .route("/versions", web::post().to(create_version))
                .route("/versions/{id}", web::get().to(get_version))
                .route("/versions/{id}", web::put().to(update_version))
                .route("/versions/{id}", web::delete().to(delete_version))
                .route("/releases", web::get().to(list_releases))
                .route("/releases", web::post().to(create_release))
                .route("/releases/{id}", web::get().to(get_release))
                .route("/releases/{id}", web::delete().to(delete_release))
                .route("/releases/{id}/publish", web::post().to(publish_release))
                .route("/releases/{id}/unpublish", web::post().to(unpublish_release))
                .route("/releases/{id}/files", web::get().to(release_files))
                .route("/releases/{id}/files", web::post().to(create_release_file))
                .route("/releases/{id}/files/{file_id}", web::get().to(get_release_file))
                .route("/releases/{id}/files/{file_id}", web::put().to(update_release_file))
                .route(
                    "/releases/{id}/files/{file_id}",
                    web::delete().to(delete_release_file),
                )
                .route(
                    "/releases/{id}/files/{file_id}/verify",
                    web::post().to(verify_release_file),
                )
                .route("/releases/{id}/download", web::get().to(download_release))
                .route("/stats", web::get().to(get_stats))
                .route("/latest", web::get().to(latest_releases))
                .route("/taxonomy", web::get().to(list_taxonomy))
                .route("/taxonomy", web::post().to(create_taxonomy_node))
                .route("/taxonomy/tree", web::get().to(taxonomy_tree))
                .route("/taxonomy/{id}", web::get().to(get_taxonomy_node))
                .route("/taxonomy/{id}", web::put().to(update_taxonomy_node))
                .route("/taxonomy/{id}", web::delete().to(delete_taxonomy_node))
                .route("/taxonomy/{id}/move", web::post().to(move_taxonomy_node))
                .route("/taxonomy/{id}/children", web::get().to(list_taxonomy_children))
                .route("/tags", web::get().to(list_tags))
                .route("/tags", web::post().to(create_tag))
                .route("/tags/cloud", web::get().to(tag_cloud))
                .route("/tags/{id}", web::get().to(get_tag))
                .route("/tags/{id}", web::put().to(update_tag))
                .route("/tags/{id}", web::delete().to(delete_tag))
                .route("", web::get().to(list_software))
                .route("", web::post().to(create_software))
                .route("/{id}", web::get().to(get_software))
                .route("/{id}", web::put().to(update_software))
                .route("/{id}", web::delete().to(delete_software))
                .route("/{id}/publish", web::post().to(publish_software))
                .route("/{id}/unpublish", web::post().to(unpublish_software))
                .route("/{id}/versions", web::get().to(list_software_versions))
                .route("/{id}/releases", web::get().to(software_release_history))
                .route("/{id}/tags", web::get().to(list_software_tags))
                .route("/{id}/tags", web::post().to(attach_software_tag))
                .route("/{id}/tags/{tag_id}", web::delete().to(detach_software_tag)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SoftwareModule::new()));
}
