//! HTTP handlers for versions and releases.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events};
use unified_core::json;
use unified_core::pagination::{PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use semver::Version;
use serde_json::{json, Value};
use std::sync::{Arc};
use super::helpers::{
    caller, caller_company, in_company_scope, release_company, require_company_for_write,
    require_permission, require_record_in_company_scope, require_record_writable_in_company_scope,
    require_release_in_scope, require_software_in_scope, require_tenant, require_version_in_scope,
    version_company,
};
use super::types::{CreateRelease, CreateVersion, PERM_SOFTWARE_CREATE, PERM_SOFTWARE_MANAGE, PERM_SOFTWARE_READ, Release, ReleaseFile, SoftwareVersion, UpdateVersion};
use super::state::SoftwareState;
use super::validation::{matches_search, new_id, now, paginate, respond_created, validate_requirements, validate_version_string};

// ---------------------------------------------------------------------------
// Version handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_versions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_SOFTWARE_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let params = query.into_inner();
    let sort = params.allowed_sort(&["product", "version", "created_at"], "created_at");
    let mut items: Vec<SoftwareVersion> = state
        .versions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|version| {
            in_company_scope(&version_company(&state, version), &scope) && matches_search(
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

pub(crate) async fn create_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    body: web::Json<CreateVersion>,
) -> HttpResponse {
    let result: UnifiedResult<SoftwareVersion> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_CREATE)?;
        let input = body.into_inner();
        if input.product.trim().is_empty() {
            return Err(UnifiedError::bad_request("Product must not be empty"));
        }
        validate_version_string(&input.version)?;
        let requirements = input.requirements.unwrap_or_default();
        validate_requirements(&requirements)?;
        if let Some(software_id) = &input.software_id {
            require_software_in_scope(&state, software_id, &scope)?;
        }
        let product_name = input.product.trim().to_string();
        let version_string = input.version.trim().to_string();
        if state.has_version(input.software_id.as_deref(), &product_name, &version_string) {
            return Err(UnifiedError::conflict(format!(
                "Version \"{version_string}\" already exists for product \"{product_name}\""
            )));
        }
        let version = SoftwareVersion {
            id: new_id(),
            product: product_name,
            version: version_string,
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
    respond_created(result, ctx.is_debug())
}

pub(crate) async fn get_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_SOFTWARE_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let result: UnifiedResult<SoftwareVersion> =
        require_version_in_scope(&state, path.as_str(), &scope);
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateVersion>,
) -> HttpResponse {
    let result: UnifiedResult<SoftwareVersion> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_MANAGE)?;
        let input = body.into_inner();
        let mut version = state
            .versions
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Version not found"))?;
        require_record_writable_in_company_scope(&version_company(&state, &version), &scope)?;
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
            require_software_in_scope(&state, &software_id, &scope)?;
            version.software_id = Some(software_id);
        }
        if let Some(requirements) = input.requirements {
            validate_requirements(&requirements)?;
            version.requirements = requirements;
        }
        version.updated_at = now();
        Ok(version.clone())
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_MANAGE)?;
        require_version_in_scope(&state, path.as_str(), &scope)?;
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
    json::respond(result, ctx.is_debug())
}

// ---------------------------------------------------------------------------
// Release handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_releases(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_SOFTWARE_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let params = query.into_inner();
    let mut items: Vec<Release> = state
        .releases
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|release| {
            in_company_scope(&release_company(&state, release), &scope)
                && matches_search(&format!("{} {}", release.channel, release.notes), &params)
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

pub(crate) async fn create_release(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    body: web::Json<CreateRelease>,
) -> HttpResponse {
    let result: UnifiedResult<Release> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_CREATE)?;
        let input = body.into_inner();
        if !matches!(input.channel.as_str(), "stable" | "beta") {
            return Err(UnifiedError::bad_request(
                "Channel must be stable or beta",
            ));
        }
        require_version_in_scope(&state, &input.version_id, &scope)?;
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
    respond_created(result, ctx.is_debug())
}

pub(crate) async fn get_release(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_SOFTWARE_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let result: UnifiedResult<Release> =
        require_release_in_scope(&state, path.as_str(), &scope);
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_release(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_MANAGE)?;
        require_release_in_scope(&state, path.as_str(), &scope)?;
        state
            .releases
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Release not found"))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn publish_release(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Release> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_MANAGE)?;
        let mut release = state
            .releases
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Release not found"))?;
        require_record_writable_in_company_scope(&release_company(&state, &release), &scope)?;
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
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn unpublish_release(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Release> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_MANAGE)?;
        let mut release = state
            .releases
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Release not found"))?;
        require_record_writable_in_company_scope(&release_company(&state, &release), &scope)?;
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
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn release_files(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_READ)?;
        let release = require_release_in_scope(&state, path.as_str(), &scope)?;
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
    json::respond(result, ctx.is_debug())
}

/// Download a release.
///
/// PUBLIC endpoint: no caller/permission gate by design — delivery
/// is gated by the API Manager's `ApiAccessRule` for this path
/// (default deny) plus the license-key check below when the version
/// requires a license. Only published releases can be downloaded.
pub(crate) async fn download_release(
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

#[cfg(test)]
mod tests {

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
    use unified_core::config::AppConfig;
    use unified_core::context::AppContext;
    use unified_core::hooks::{HookBus, HookContext};
    use unified_core::modules::ModuleRegistry;
    use crate::modules::software::types::VersionRequirements;
    use actix_web::test::TestRequest;
    use chrono::Utc;
    use std::sync::Arc;

    /// App context whose `role.check` hook allows exactly the given
    /// permissions (all other permissions are denied).
    fn test_ctx(allowed: &[&str]) -> web::Data<Arc<AppContext>> {
        let hooks = Arc::new(HookBus::new());
        let allowed: Vec<String> = allowed.iter().map(|permission| permission.to_string()).collect();
        hooks.register(
            "role.check",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                let permission = ctx
                    .get("permission")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                ctx.set(
                    "allowed",
                    serde_json::json!(allowed.iter().any(|held| held == permission)),
                );
            }),
        );
        let ctx = AppContext::new(
            AppConfig::default(),
            hooks,
            Arc::new(ModuleRegistry::new()),
        );
        web::Data::new(Arc::new(ctx))
    }

    /// The company the handler tests act as (C3 task 315).
    const TEST_COMPANY: &str = "company-a";

    /// A request scoped to the test company (C3 task 315):
    /// mutation handlers require a resolved company, so the
    /// happy-path tests carry one on both the tenant context and
    /// the verified caller.
    fn authed_request() -> HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid("tester"), Vec::new())
                .with_company("company-a".to_string())
                .with_roles(vec!["admin".to_string()]),
        );
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-a".to_string(),
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

    fn create_input(product: &str, version: &str) -> CreateVersion {
        CreateVersion {
            product: product.to_string(),
            version: version.to_string(),
            changelog: None,
            file_id: None,
            license_required: None,
            software_id: None,
            requirements: None,
        }
    }

    fn seed_version(state: &SoftwareState) {
        state.versions.insert(
            "ver-1".to_string(),
            SoftwareVersion {
                id: "ver-1".to_string(),
                product: "my-plugin".to_string(),
                version: "1.2.3".to_string(),
                changelog: String::new(),
                file_id: None,
                license_required: false,
                software_id: None,
                requirements: VersionRequirements::default(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        );
    }

    #[test]
    fn has_version_matches_the_full_triple() {
        let state = SoftwareState::new();
        seed_version(&state);
        assert!(state.has_version(None, "my-plugin", "1.2.3"));
        assert!(!state.has_version(None, "my-plugin", "1.2.4"));
        assert!(!state.has_version(None, "other-plugin", "1.2.3"));
        assert!(!state.has_version(Some("sw-1"), "my-plugin", "1.2.3"));
    }

    #[tokio::test]
    async fn create_version_rejects_a_duplicate_version() {
        let ctx = test_ctx(&["software:create"]);
        let state = Arc::new(SoftwareState::new());
        seed_version(&state);
        let response = create_version(
            ctx,
            web::Data::new(state),
            authed_request(),
            web::Json(create_input("my-plugin", "1.2.3")),
        )
        .await;
        assert_eq!(response.status().as_u16(), 409);
    }

    #[tokio::test]
    async fn create_version_accepts_a_new_version_for_the_same_product() {
        let ctx = test_ctx(&["software:create"]);
        let state = Arc::new(SoftwareState::new());
        seed_version(&state);
        let response = create_version(
            ctx,
            web::Data::new(state),
            authed_request(),
            web::Json(create_input("my-plugin", "1.2.4")),
        )
        .await;
        assert_eq!(response.status().as_u16(), 201);
    }
}
