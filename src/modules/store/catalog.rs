// Store module - catalog and update-check handlers.
//
// Split from `src/modules/store.rs`; the module root re-exports the public API of this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::events;
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use semver::Version;
use serde_json::{json, Value};
use std::sync::Arc;

use super::helpers::{
    caller_company, matches_search, paginate, require_caller_company, require_product_in_scope,
    require_tenant, respond_created,
};
use super::pricing::{valid_kind};
use super::signing::{build_signed_url, record_download};
use super::state::{StoreState, now};
use super::types::{CatalogEntry, CreateCatalogEntry, DownloadEntitlement, PackageQuery, PERM_STORE_CATALOG_MANAGE, UpdateCatalogEntry, UpdateCheckRequest};

// ---------------------------------------------------------------------------
// Update API (WordPress-style version checks and package downloads)
// ---------------------------------------------------------------------------

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// the same shape as the digital-products
/// routes in this module.
pub(crate) struct CatalogCaller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn catalog_caller(req: &HttpRequest) -> UnifiedResult<CatalogCaller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(CatalogCaller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
/// The store keeps only the hook bus (no `AppContext`), so the check
/// runs through `state.bus().emit`, the same shape as the
/// digital-products routes.
pub(crate) fn catalog_has_capability(state: &StoreState, role: &str, permission: &str) -> bool {
    let context = state.bus().emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_catalog_permission(
    state: &StoreState,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<CatalogCaller> {
    let caller = catalog_caller(req)?;
    if catalog_has_capability(state, &caller.role, permission) {
        Ok(caller)
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{}\" does not hold permission \"{}\"",
            caller.role, permission
        )))
    }
}

pub(crate) fn require_catalog_manage(
    state: &StoreState,
    req: &HttpRequest,
) -> UnifiedResult<CatalogCaller> {
    require_catalog_permission(state, req, PERM_STORE_CATALOG_MANAGE)
}

pub(crate) async fn list_catalog(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = caller_company(&tenant, &req) {
        return json::error_response(&error, false);
    }
    let params = query.into_inner();
    let mut items: Vec<CatalogEntry> = state
        .catalog
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| {
            matches_search(&format!("{} {} {}", entry.slug, entry.name, entry.changelog), &params)
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|kind| entry.kind == *kind)
                && params.filters.get("slug").is_none_or(|slug| entry.slug == *slug)
                && params.filters.get("product_id").is_none_or(|id| {
                    entry.product_id.as_deref() == Some(id.as_str())
                })
                && params.filters.get("license_required").is_none_or(|flag| {
                    entry.license_required == (*flag == "true" || *flag == "1")
                })
        })
        .collect();
    items.sort_by(|a, b| a.slug.cmp(&b.slug));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_catalog_entry(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: web::Json<CreateCatalogEntry>,
) -> HttpResponse {
    let result: UnifiedResult<CatalogEntry> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        require_catalog_manage(&state, &req)?;
        let input = body.into_inner();
        let slug = input.slug.trim().to_string();
        if slug.is_empty() {
            return Err(UnifiedError::bad_request("Slug must not be empty"));
        }
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Name must not be empty"));
        }
        if !valid_kind(&input.kind) {
            return Err(UnifiedError::bad_request(
                "Kind must be one of: theme, plugin, software",
            ));
        }
        if Version::parse(&input.latest_version).is_err() {
            return Err(UnifiedError::bad_request(
                "Latest version is not a valid semantic version (expected MAJOR.MINOR.PATCH)",
            ));
        }
        if let Some(product_id) = &input.product_id {
            let _product = require_product_in_scope(&state, &scope, product_id)?;
        }
        if state.catalog.contains_key(&slug) {
            return Err(UnifiedError::conflict(
                "A catalog entry with this slug already exists",
            ));
        }
        let entry = CatalogEntry {
            slug: slug.clone(),
            name: input.name.trim().to_string(),
            kind: input.kind,
            product_id: input.product_id,
            product_name: input
                .product_name
                .unwrap_or_else(|| input.name.trim().to_string()),
            latest_version: input.latest_version.trim().to_string(),
            release_id: input.release_id,
            file_id: input.file_id,
            changelog: input.changelog.unwrap_or_default(),
            requires_wp: input.requires_wp,
            tested_wp: input.tested_wp,
            requires_php: input.requires_php,
            license_required: input.license_required.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state.catalog.insert(slug, entry.clone());
        Ok(entry)
    })();
    respond_created(result)
}

pub(crate) async fn get_catalog_entry(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = caller_company(&tenant, &req) {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<CatalogEntry> = state
        .catalog
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Catalog entry not found"));
    json::respond(result, false)
}

pub(crate) async fn update_catalog_entry(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCatalogEntry>,
) -> HttpResponse {
    let result: UnifiedResult<CatalogEntry> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        require_catalog_manage(&state, &req)?;
        let input = body.into_inner();
        let mut entry = state
            .catalog
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Catalog entry not found"))?;
        if let Some(product_id) = entry.product_id.clone() {
            let _product = require_product_in_scope(&state, &scope, &product_id)?;
        }
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Name must not be empty"));
            }
            entry.name = name.trim().to_string();
        }
        if let Some(kind) = input.kind {
            if !valid_kind(&kind) {
                return Err(UnifiedError::bad_request(
                    "Kind must be one of: theme, plugin, software",
                ));
            }
            entry.kind = kind;
        }
        if let Some(product_id) = input.product_id {
            let _product = require_product_in_scope(&state, &scope, &product_id)?;
            entry.product_id = Some(product_id);
        }
        if let Some(product_name) = input.product_name {
            entry.product_name = product_name;
        }
        if let Some(latest_version) = input.latest_version {
            if Version::parse(&latest_version).is_err() {
                return Err(UnifiedError::bad_request(
                    "Latest version is not a valid semantic version (expected MAJOR.MINOR.PATCH)",
                ));
            }
            entry.latest_version = latest_version.trim().to_string();
        }
        if let Some(release_id) = input.release_id {
            entry.release_id = Some(release_id);
        }
        if let Some(file_id) = input.file_id {
            entry.file_id = Some(file_id);
        }
        if let Some(changelog) = input.changelog {
            entry.changelog = changelog;
        }
        if let Some(requires_wp) = input.requires_wp {
            entry.requires_wp = Some(requires_wp);
        }
        if let Some(tested_wp) = input.tested_wp {
            entry.tested_wp = Some(tested_wp);
        }
        if let Some(requires_php) = input.requires_php {
            entry.requires_php = Some(requires_php);
        }
        if let Some(license_required) = input.license_required {
            entry.license_required = license_required;
        }
        entry.updated_at = now();
        Ok(entry.clone())
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_catalog_entry(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        require_catalog_manage(&state, &req)?;
        let entry = state
            .catalog
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Catalog entry not found"))?;
        if let Some(product_id) = &entry.product_id {
            let _product = require_product_in_scope(&state, &scope, product_id)?;
        }
        state
            .catalog
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "slug": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Catalog entry not found"))
    })();
    json::respond(result, false)
}

/// Ask the licensing module through the hook bus whether a key is valid
/// for a product. Returns (valid, license_id). No validator registered
/// means the key cannot be trusted, so the answer is not valid.
pub(crate) fn validate_license_via_hook(
    state: &StoreState,
    license_key: &str,
    product_name: &str,
) -> (bool, Option<String>) {
    if !state.bus().has("license.validate_requested") {
        return (false, None);
    }
    let context = state.bus().emit(
        "license.validate_requested",
        json!({
            "license_key": license_key,
            "product": product_name,
        }),
    );
    for result in &context.results {
        if result.get("valid").and_then(|value| value.as_bool()) == Some(true) {
            let license_id = result
                .get("license_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            return (true, license_id);
        }
    }
    (false, None)
}

pub(crate) fn entitlement_for_license(
    state: &StoreState,
    license_id: &str,
    slug: &str,
) -> Option<DownloadEntitlement> {
    state
        .entitlements
        .iter()
        .find(|entry| {
            entry.value().license_id.as_deref() == Some(license_id)
                && entry.value().software_slug.as_deref() == Some(slug)
        })
        .map(|entry| entry.value().clone())
}

/// WordPress-style version check: given a slug and the installed
/// version, return the latest version, whether an update is available,
/// compatibility metadata, and a signed download URL only when the
/// caller is entitled (valid license plus a download entitlement with
/// downloads left, or a free package that needs no license).
pub(crate) async fn check_update(
    state: web::Data<Arc<StoreState>>,
    body: web::Json<UpdateCheckRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let input = body.into_inner();
        let entry = state
            .catalog
            .get(&input.slug)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Software not found"))?;
        let update_available = match (
            Version::parse(&input.current_version),
            Version::parse(&entry.latest_version),
        ) {
            (Ok(current), Ok(latest)) => latest > current,
            _ => entry.latest_version != input.current_version,
        };
        let mut entitled = false;
        let mut download_url: Option<String> = None;
        if !entry.license_required {
            entitled = true;
            download_url = Some(format!("/api/v1/store/updates/{}/package", entry.slug));
        } else if let Some(license_key) = input.license_key.filter(|key| !key.trim().is_empty()) {
            let (valid, license_id) =
                validate_license_via_hook(&state, &license_key, &entry.product_name);
            if valid {
                if let Some(license_id) = license_id {
                    if let Some(entitlement) =
                        entitlement_for_license(&state, &license_id, &entry.slug)
                    {
                        let usable = entitlement.status == "active"
                            && entitlement.downloads_used < entitlement.download_limit
                            && entitlement
                                .expires_at
                                .is_none_or(|expires_at| expires_at > now());
                        if usable {
                            if let Some(release_id) = entitlement.release_id.clone() {
                                entitled = true;
                                let (url, _) = build_signed_url(&license_id, &release_id);
                                download_url = Some(url);
                            }
                        }
                    }
                }
            }
        }
        state.emit(
            "software.update.checked",
            json!({
                "slug": entry.slug,
                "current_version": input.current_version,
                "latest_version": entry.latest_version,
                "update_available": update_available,
                "entitled": entitled,
            }),
        );
        Ok(json!({
            "slug": entry.slug,
            "name": entry.name,
            "kind": entry.kind,
            "current_version": input.current_version,
            "latest_version": entry.latest_version,
            "update_available": update_available,
            "changelog": entry.changelog,
            "requires_wp": entry.requires_wp,
            "tested_wp": entry.tested_wp,
            "requires_php": entry.requires_php,
            "license_required": entry.license_required,
            "entitled": entitled,
            "download_url": download_url,
        }))
    })();
    json::respond(result, false)
}

/// Download the latest package for a slug. Licensed packages require a
/// valid license key and a download entitlement with downloads left;
/// each successful download counts against the entitlement limit.
pub(crate) async fn download_package(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
    query: web::Query<PackageQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let slug = path.into_inner();
        let entry = state
            .catalog
            .get(&slug)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Software not found"))?;
        let release_id = entry.release_id.clone().ok_or_else(|| {
            UnifiedError::unprocessable("No release is linked to this software")
        })?;
        if !entry.license_required {
            state.emit(
                events::DOWNLOAD_STARTED,
                json!({
                    "slug": slug,
                    "release_id": release_id,
                    "file_id": entry.file_id,
                }),
            );
            return Ok(json!({
                "package": {
                    "slug": entry.slug,
                    "name": entry.name,
                    "version": entry.latest_version,
                    "release_id": release_id,
                    "file_id": entry.file_id,
                    "status": "ready",
                }
            }));
        }
        let license_key = query
            .into_inner()
            .license_key
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| {
                UnifiedError::forbidden(
                    "This package requires a valid license key (license_key query parameter)",
                )
            })?;
        let (valid, license_id) =
            validate_license_via_hook(&state, &license_key, &entry.product_name);
        if !valid {
            return Err(UnifiedError::forbidden(
                "License key is not valid for this product",
            ));
        }
        let license_id = license_id.ok_or_else(|| {
            UnifiedError::forbidden("License key is not valid for this product")
        })?;
        let entitlement = entitlement_for_license(&state, &license_id, &slug).ok_or_else(|| {
            UnifiedError::forbidden(
                "No download entitlement exists for this license and product",
            )
        })?;
        let updated = record_download(&state, &entitlement.id, &release_id)?;
        let (download_url, expires_at) = build_signed_url(&license_id, &release_id);
        Ok(json!({
            "package": {
                "slug": entry.slug,
                "name": entry.name,
                "version": entry.latest_version,
                "release_id": release_id,
                "file_id": entry.file_id,
                "status": "ready",
                "download_url": download_url,
                "download_expires_at": expires_at.to_rfc3339(),
                "downloads_used": updated.downloads_used,
                "download_limit": updated.download_limit,
            }
        }))
    })();
    json::respond(result, false)
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()])
                .with_company("company-1".to_string()),
        );
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-1".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_company("company-1".to_string()),
        );
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-1".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    fn state_data() -> web::Data<Arc<StoreState>> {
        web::Data::new(Arc::new(StoreState::new()))
    }

    /// A state whose `role.check` handler allows every permission.
    fn allowing_state() -> web::Data<Arc<StoreState>> {
        let state = state_data();
        state.bus().register(
            "role.check",
            "test",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                context.set("allowed", serde_json::Value::Bool(true));
            }),
        );
        state
    }

    fn authed_request() -> HttpRequest {
        verified_request("user-1", "admin")
    }

    fn create_body() -> web::Json<CreateCatalogEntry> {
        web::Json(CreateCatalogEntry {
            slug: "test-plugin".to_string(),
            name: "Test Plugin".to_string(),
            kind: "plugin".to_string(),
            product_id: None,
            product_name: None,
            latest_version: "1.0.0".to_string(),
            release_id: None,
            file_id: None,
            changelog: None,
            requires_wp: None,
            tested_wp: None,
            requires_php: None,
            license_required: None,
        })
    }

    fn update_body() -> web::Json<UpdateCatalogEntry> {
        web::Json(UpdateCatalogEntry {
            name: Some("Renamed Plugin".to_string()),
            kind: None,
            product_id: None,
            product_name: None,
            latest_version: None,
            release_id: None,
            file_id: None,
            changelog: None,
            requires_wp: None,
            tested_wp: None,
            requires_php: None,
            license_required: None,
        })
    }

    #[test]
    fn catalog_caller_accepts_a_verified_caller() {
        let found = catalog_caller(&authed_request()).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
    }

    #[test]
    fn catalog_caller_rejects_a_request_without_a_verified_caller() {
        let req = TestRequest::default().to_http_request();
        let error = catalog_caller(&req).unwrap_err();
        assert_eq!(error.code, 401);
    }

    #[test]
    fn catalog_caller_without_a_resolved_role_is_anonymous() {
        // A verified caller with no role resolved holds the anonymous
        // role: the identity resolves, the capability check is what
        // refuses it.
        let req = verified_request_without_roles("user-1");
        let found = catalog_caller(&req).expect("a verified caller resolves without a role");
        assert_eq!(found.role, "anonymous");
    }

    #[test]
    fn catalog_has_capability_fails_closed_without_a_role_check_handler() {
        let state = StoreState::new();
        assert!(!catalog_has_capability(&state, "admin", PERM_STORE_CATALOG_MANAGE));
    }

    #[tokio::test]
    async fn create_catalog_entry_rejects_anonymous_callers() {
        let response = create_catalog_entry(
            state_data(),
            TestRequest::default().to_http_request(),
            create_body(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create_catalog_entry_rejects_callers_without_catalog_manage() {
        // Headers present, but no `role.check` handler answers:
        // the guard fails closed.
        let response =
            create_catalog_entry(state_data(), authed_request(), create_body()).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn create_catalog_entry_accepts_callers_with_catalog_manage() {
        let response =
            create_catalog_entry(allowing_state(), authed_request(), create_body()).await;
        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn update_catalog_entry_rejects_anonymous_callers() {
        let response = update_catalog_entry(
            state_data(),
            TestRequest::default().to_http_request(),
            web::Path::from("test-plugin".to_string()),
            update_body(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn update_catalog_entry_rejects_callers_without_catalog_manage() {
        let response = update_catalog_entry(
            state_data(),
            authed_request(),
            web::Path::from("test-plugin".to_string()),
            update_body(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn delete_catalog_entry_rejects_anonymous_callers() {
        let response = delete_catalog_entry(
            state_data(),
            TestRequest::default().to_http_request(),
            web::Path::from("test-plugin".to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn delete_catalog_entry_rejects_callers_without_catalog_manage() {
        let response = delete_catalog_entry(
            state_data(),
            authed_request(),
            web::Path::from("test-plugin".to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
