// Software / Assets Manager module.
//
// Product versions with semver validation, a release publish flow
// (draft -> published, emitting `software.released`), files per release,
// download counts, and license-gated downloads: a release with
// `license_required` asks the licensing module through the
// `license.validate_requested` hook, and denies the download honestly
// when no validator answers or the key is invalid. Interconnection
// happens only through the hook bus.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoftwareVersion {
    pub id: String,
    pub product: String,
    pub version: String,
    pub changelog: String,
    pub file_id: Option<String>,
    pub license_required: bool,
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
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateVersion {
    pub product: Option<String>,
    pub version: Option<String>,
    pub changelog: Option<String>,
    pub file_id: Option<String>,
    pub license_required: Option<bool>,
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
// State
// ---------------------------------------------------------------------------

pub struct SoftwareState {
    hooks: RwLock<Arc<HookBus>>,
    versions: DashMap<String, SoftwareVersion>,
    releases: DashMap<String, Release>,
}

impl SoftwareState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            versions: DashMap::new(),
            releases: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
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
        let version = SoftwareVersion {
            id: new_id(),
            product: input.product.trim().to_string(),
            version: input.version.trim().to_string(),
            changelog: input.changelog.unwrap_or_default(),
            file_id: input.file_id,
            license_required: input.license_required.unwrap_or(false),
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
        let release_value = serde_json::to_value(&*release).unwrap_or(Value::Null);
        drop(release);
        state.emit("software.released", release_value);
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
        Ok(json!({
            "release_id": release.id,
            "version_id": release.version_id,
            "files": file_ids.iter().map(|file_id| json!({ "file_id": file_id })).collect::<Vec<Value>>(),
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
    json::success_response(
        json!({
            "total_versions": state.versions.len(),
            "total_releases": state.releases.len(),
            "total_downloads": total_downloads,
            "downloads_by_release": downloads_by_release,
        }),
        200,
    )
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
        "Software and assets manager: versions, releases, downloads and license-gated delivery."
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

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
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
                .route("/releases/{id}/files", web::get().to(release_files))
                .route("/releases/{id}/download", web::get().to(download_release))
                .route("/stats", web::get().to(get_stats)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SoftwareModule::new()));
}
