// Api Manager - API version and prefix handlers.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::json as api_json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, web, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;

use super::validation::{
    PERM_API_MANAGER_MANAGE, PERM_API_MANAGER_READ, require_admin,
};

use super::{ApiManagerState, ApiPrefixSettings, ApiVersion, CreateApiVersionInput,
    DeprecateVersionInput, UpdateApiVersionInput, UpdatePrefixInput, VersionChoice,
    parse_version_number, validate_prefix, validate_registry_version_id};

/// Legacy grouped overview of custom endpoints by version string.
/// Paginated over the version groups (the per-group endpoint lists
/// stay complete: this endpoint is superseded by the paginated
/// `/registry`, which is the per-version view to use).
pub(crate) async fn list_versions(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_READ) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let params = query.into_inner();
    let mut grouped: std::collections::BTreeMap<String, Vec<Value>> =
        std::collections::BTreeMap::new();
    for entry in state.endpoints.iter() {
        let endpoint = entry.value();
        grouped
            .entry(endpoint.version.clone())
            .or_default()
            .push(json!({
                "id": endpoint.id,
                "method": endpoint.method,
                "path": endpoint.path,
                "description": endpoint.description,
                "enabled": endpoint.enabled,
                "rate_limit": endpoint.rate_limit,
            }));
    }
    let versions: Vec<Value> = grouped
        .into_iter()
        .map(|(version, endpoints)| {
            let count = endpoints.len();
            json!({
                "version": version,
                "count": count,
                "endpoints": endpoints,
            })
        })
        .collect();
    let pagination = params.normalized();
    let total = versions.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<Value> = versions
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

// ---------------------------------------------------------------------------
// Version registry handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_api_versions(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_READ) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    state.ensure_seeded_versions();
    let params = query.into_inner();
    let search = params.search.clone();
    let enabled_filter = params.filters.get("enabled").cloned();
    let deprecated_filter = params.filters.get("deprecated").cloned();
    let mut items: Vec<ApiVersion> = state
        .versions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|version| {
            if let Some(enabled) = &enabled_filter {
                if version.enabled != (enabled == "true") {
                    return false;
                }
            }
            if let Some(deprecated) = &deprecated_filter {
                if version.deprecated != (deprecated == "true") {
                    return false;
                }
            }
            if let Some(needle) = &search {
                let needle = needle.to_lowercase();
                return version.id.to_lowercase().contains(&needle)
                    || version.notes.to_lowercase().contains(&needle);
            }
            true
        })
        .collect();
    items.sort_by_key(|version| version.number);
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<ApiVersion> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

pub(crate) async fn create_api_version(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    input: web::Json<CreateApiVersionInput>,
) -> HttpResponse {
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    state.ensure_seeded_versions();
    let input = input.into_inner();
    let outcome = (|| -> Result<ApiVersion, UnifiedError> {
        let raw_id = input
            .id
            .clone()
            .or(input.version.clone())
            .ok_or_else(|| UnifiedError::bad_request("provide an id such as \"v1\""))?;
        let id = validate_registry_version_id(&raw_id)?;
        if state.versions.contains_key(&id) {
            return Err(UnifiedError::conflict(format!(
                "API version \"{id}\" already exists"
            )));
        }
        let now = Utc::now();
        // Start from the copied version's settings when requested,
        // otherwise from the blank defaults below.
        let mut version = ApiVersion {
            id: id.clone(),
            number: parse_version_number(&id).unwrap_or(0),
            enabled: true,
            deprecated: false,
            sunset_date: None,
            rate_limit_per_minute: 1000,
            auth_required: true,
            notes: String::new(),
            created_at: now,
            updated_at: now,
        };
        if let Some(copy_from) = &input.copy_from {
            let source = state
                .versions
                .get(copy_from.trim())
                .map(|entry| entry.value().clone())
                .ok_or_else(|| {
                    UnifiedError::not_found(format!(
                        "API version \"{copy_from}\" does not exist, so its settings cannot be copied"
                    ))
                })?;
            version.enabled = source.enabled;
            version.deprecated = source.deprecated;
            version.sunset_date = source.sunset_date;
            version.rate_limit_per_minute = source.rate_limit_per_minute;
            version.auth_required = source.auth_required;
            version.notes = format!("Copied from version \"{}\". {}", source.id, source.notes);
        }
        if let Some(enabled) = input.enabled {
            version.enabled = enabled;
        }
        if let Some(deprecated) = input.deprecated {
            version.deprecated = deprecated;
        }
        if let Some(sunset_date) = input.sunset_date {
            if sunset_date <= now {
                return Err(UnifiedError::bad_request(
                    "sunset_date must be in the future",
                ));
            }
            version.sunset_date = Some(sunset_date);
        }
        if version.deprecated && version.sunset_date.is_none() {
            return Err(UnifiedError::bad_request(
                "sunset_date is required when creating a deprecated version",
            ));
        }
        if let Some(rate_limit) = input.rate_limit_per_minute {
            if rate_limit == 0 || rate_limit > 1_000_000 {
                return Err(UnifiedError::bad_request(
                    "rate_limit_per_minute must be between 1 and 1000000",
                ));
            }
            version.rate_limit_per_minute = rate_limit;
        }
        if let Some(auth_required) = input.auth_required {
            version.auth_required = auth_required;
        }
        if let Some(notes) = input.notes {
            if notes.chars().count() > 2000 {
                return Err(UnifiedError::bad_request(
                    "notes must be at most 2000 characters",
                ));
            }
            version.notes = notes;
        }
        state.versions.insert(id.clone(), version.clone());
        ctx.hooks.emit(
            "api.version.created",
            json!({
                "version": id,
                "copy_from": input.copy_from,
                "enabled": version.enabled,
                "rate_limit_per_minute": version.rate_limit_per_minute,
                "auth_required": version.auth_required,
            }),
        );
        Ok(version)
    })();
    match outcome {
        Ok(version) => api_json::success_response(version, 201),
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

pub(crate) async fn get_api_version(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    version: web::Path<String>,
) -> HttpResponse {
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_READ) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    state.ensure_seeded_versions();
    let version = version.into_inner();
    match state.versions.get(&version) {
        Some(entry) => api_json::success_response(entry.value().clone(), 200),
        None => api_json::error_response(
            &UnifiedError::not_found(format!("API version \"{version}\" does not exist")),
            ctx.is_debug(),
        ),
    }
}

pub(crate) async fn update_api_version(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    version: web::Path<String>,
    input: web::Json<UpdateApiVersionInput>,
) -> HttpResponse {
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    state.ensure_seeded_versions();
    let version_id = version.into_inner();
    let input = input.into_inner();
    let outcome = (|| -> Result<ApiVersion, UnifiedError> {
        let mut version = state
            .versions
            .get(&version_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("API version \"{version_id}\" does not exist"))
            })?;
        let was_deprecated = version.deprecated;
        if let Some(enabled) = input.enabled {
            version.enabled = enabled;
        }
        if let Some(deprecated) = input.deprecated {
            version.deprecated = deprecated;
        }
        if let Some(sunset_date) = input.sunset_date {
            if sunset_date <= Utc::now() {
                return Err(UnifiedError::bad_request(
                    "sunset_date must be in the future",
                ));
            }
            version.sunset_date = Some(sunset_date);
        }
        if version.deprecated && version.sunset_date.is_none() {
            return Err(UnifiedError::bad_request(
                "sunset_date is required when deprecating a version",
            ));
        }
        if let Some(rate_limit) = input.rate_limit_per_minute {
            if rate_limit == 0 || rate_limit > 1_000_000 {
                return Err(UnifiedError::bad_request(
                    "rate_limit_per_minute must be between 1 and 1000000",
                ));
            }
            version.rate_limit_per_minute = rate_limit;
        }
        if let Some(auth_required) = input.auth_required {
            version.auth_required = auth_required;
        }
        if let Some(notes) = input.notes {
            if notes.chars().count() > 2000 {
                return Err(UnifiedError::bad_request(
                    "notes must be at most 2000 characters",
                ));
            }
            version.notes = notes;
        }
        version.updated_at = Utc::now();
        state.versions.insert(version_id.clone(), version.clone());
        if version.deprecated && !was_deprecated {
            ctx.hooks.emit(
                "api.version.deprecated",
                json!({
                    "version": version_id,
                    "sunset_date": version.sunset_date.map(|date| date.to_rfc3339()),
                }),
            );
        }
        Ok(version)
    })();
    match outcome {
        Ok(version) => api_json::success_response(version, 200),
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

pub(crate) async fn deprecate_api_version(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    version: web::Path<String>,
    input: web::Json<DeprecateVersionInput>,
) -> HttpResponse {
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    state.ensure_seeded_versions();
    let version_id = version.into_inner();
    let input = input.into_inner();
    let outcome = (|| -> Result<ApiVersion, UnifiedError> {
        let mut version = state
            .versions
            .get(&version_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("API version \"{version_id}\" does not exist"))
            })?;
        if version.deprecated {
            return Err(UnifiedError::conflict(format!(
                "API version \"{version_id}\" is already deprecated"
            )));
        }
        if input.sunset_date <= Utc::now() {
            return Err(UnifiedError::bad_request(
                "sunset_date must be in the future",
            ));
        }
        version.deprecated = true;
        version.sunset_date = Some(input.sunset_date);
        if let Some(notes) = input.notes {
            if notes.chars().count() > 2000 {
                return Err(UnifiedError::bad_request(
                    "notes must be at most 2000 characters",
                ));
            }
            version.notes = notes;
        }
        version.updated_at = Utc::now();
        state.versions.insert(version_id.clone(), version.clone());
        ctx.hooks.emit(
            "api.version.deprecated",
            json!({
                "version": version_id,
                "sunset_date": input.sunset_date.to_rfc3339(),
            }),
        );
        Ok(version)
    })();
    match outcome {
        Ok(version) => api_json::success_response(version, 200),
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

pub(crate) async fn delete_api_version(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    version: web::Path<String>,
) -> HttpResponse {
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    state.ensure_seeded_versions();
    let version_id = version.into_inner();
    let known = state.version_ids();
    // Refuse only when a custom endpoint still covers this version.
    // Built-in endpoints declare "all versions", so they adapt to
    // the remaining versions automatically and never block deletion.
    let referencing: Vec<String> = state
        .endpoints
        .iter()
        .filter(|entry| {
            entry
                .value()
                .version_support
                .supports(&version_id, &known)
        })
        .map(|entry| format!("{} {}", entry.value().method, entry.value().path))
        .collect();
    if !referencing.is_empty() {
        return api_json::error_response(
            &UnifiedError::conflict(format!(
                "API version \"{version_id}\" cannot be deleted: custom endpoints still cover it"
            ))
            .with_metadata("referencing_endpoints", json!(referencing)),
            ctx.is_debug(),
        );
    }
    match state.versions.remove(&version_id) {
        Some((_, version)) => api_json::success_response(
            json!({ "id": version.id, "deleted": true }),
            200,
        ),
        None => api_json::error_response(
            &UnifiedError::not_found(format!("API version \"{version_id}\" does not exist")),
            ctx.is_debug(),
        ),
    }
}

// ---------------------------------------------------------------------------
// Global prefix handlers
// ---------------------------------------------------------------------------

pub(crate) async fn get_prefix(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_READ) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    state.ensure_seeded_versions();
    let settings = state
        .prefix
        .read()
        .map(|settings| settings.clone())
        .unwrap_or_default();
    let migrate_internal = settings.migrate_internal;
    api_json::success_response(
        json!({
            "prefix": settings.prefix,
            "migrate_internal": migrate_internal,
            "updated_at": settings.updated_at,
            "example_public_url": state.public_url("v1", "/example"),
            "internal_routes_unchanged": !migrate_internal,
            "note": if migrate_internal {
                "migrate_internal is enabled: on the next startup the server factory must remount the internal /api/v1/<module> routes under the new prefix (see the remount snippet in the coordinator report). This setting alone does not move routes that Actix already registered."
            } else {
                "The prefix governs the public versioned surface only. Internal module routes stay mounted at /api/v1/<module>."
            },
        }),
        200,
    )
}

pub(crate) async fn update_prefix(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    input: web::Json<UpdatePrefixInput>,
) -> HttpResponse {
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    state.ensure_seeded_versions();
    let input = input.into_inner();
    let outcome = (|| -> Result<ApiPrefixSettings, UnifiedError> {
        let prefix = validate_prefix(&input.prefix)?;
        let mut settings = state
            .prefix
            .read()
            .map(|settings| settings.clone())
            .unwrap_or_default();
        settings.prefix = prefix;
        if let Some(migrate_internal) = input.migrate_internal {
            settings.migrate_internal = migrate_internal;
        }
        settings.updated_at = Utc::now();
        *state
            .prefix
            .write()
            .map_err(|_| UnifiedError::internal("prefix setting lock is poisoned"))? =
            settings.clone();
        Ok(settings)
    })();
    match outcome {
        Ok(settings) => api_json::success_response(
            json!({
                "prefix": settings.prefix,
                "migrate_internal": settings.migrate_internal,
                "updated_at": settings.updated_at,
                "example_public_url": state.public_url("v1", "/example"),
                "internal_routes_unchanged": !settings.migrate_internal,
                "note": if settings.migrate_internal {
                    "migrate_internal is enabled: on the next startup the server factory must remount the internal /api/v1/<module> routes under the new prefix. This setting alone does not move routes that Actix already registered."
                } else {
                    "The prefix governs the public versioned surface only. Internal module routes stay mounted at /api/v1/<module>."
                },
            }),
            200,
        ),
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

// ---------------------------------------------------------------------------
// Version choices, endpoint registry, resolver
// ---------------------------------------------------------------------------

/// `GET /version-choices`: the shape the admin UI creation form
/// consumes when an admin picks which versions a custom endpoint
/// covers: one existing version, all versions, or a multi-select.
pub(crate) async fn get_version_choices(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_READ) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    state.ensure_seeded_versions();
    let versions: Vec<VersionChoice> = state
        .versions
        .iter()
        .map(|entry| {
            let version = entry.value();
            VersionChoice {
                id: version.id.clone(),
                label: version.id.clone(),
                enabled: version.enabled,
                deprecated: version.deprecated,
                sunset_date: version.sunset_date,
            }
        })
        .collect();
    let mut versions = versions;
    versions.sort_by_key(|choice| parse_version_number(&choice.id).unwrap_or(0));
    let default_version = versions
        .iter()
        .find(|choice| choice.enabled && !choice.deprecated)
        .map(|choice| choice.id.clone());
    api_json::success_response(
        json!({
            "modes": [
                { "id": "single", "label": "One version", "description": "The endpoint is available in exactly one version." },
                { "id": "all", "label": "All versions", "description": "The endpoint is available in every registered version." },
                { "id": "multi", "label": "Multiple versions", "description": "Pick two or more versions from the list." },
            ],
            "versions": versions,
            "default_mode": "single",
            "default_version": default_version,
        }),
        200,
    )
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
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
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
    use unified_core::config::AppConfig;
    use unified_core::hooks::{HookBus, HookContext};
    use unified_core::modules::ModuleRegistry;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    /// App context whose `role.check` hook allows exactly the given
    /// permissions (all other permissions are denied).
    fn test_ctx(allowed: &[&str]) -> web::Data<Arc<AppContext>> {
        let hooks = Arc::new(HookBus::new());
        let allowed: Vec<String> = allowed
            .iter()
            .map(|permission| permission.to_string())
            .collect();
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
        let ctx = AppContext::new(AppConfig::default(), hooks, Arc::new(ModuleRegistry::new()));
        web::Data::new(Arc::new(ctx))
    }

    fn state_data() -> web::Data<ApiManagerState> {
        let state = ApiManagerState::default();
        state.ensure_seeded_versions();
        web::Data::new(state)
    }

    fn authed_request() -> HttpRequest {
        verified_request("tester", "admin")
    }

    fn anonymous_request() -> HttpRequest {
        TestRequest::default().to_http_request()
    }

    fn create_version_input() -> web::Json<CreateApiVersionInput> {
        web::Json(CreateApiVersionInput {
            id: Some("v9".to_string()),
            version: None,
            copy_from: None,
            enabled: None,
            deprecated: None,
            sunset_date: None,
            rate_limit_per_minute: None,
            auth_required: None,
            notes: None,
        })
    }

    #[tokio::test]
    async fn list_api_versions_rejects_anonymous_callers() {
        let resp = list_api_versions(
            state_data(),
            test_ctx(&[PERM_API_MANAGER_READ]),
            anonymous_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create_api_version_rejects_anonymous_callers() {
        let resp = create_api_version(
            state_data(),
            test_ctx(&[PERM_API_MANAGER_MANAGE]),
            anonymous_request(),
            create_version_input(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create_api_version_rejects_callers_without_manage() {
        let resp = create_api_version(
            state_data(),
            test_ctx(&[PERM_API_MANAGER_READ]),
            authed_request(),
            create_version_input(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn create_api_version_accepts_authorized_caller() {
        let state = state_data();
        let resp = create_api_version(
            state.clone(),
            test_ctx(&[PERM_API_MANAGER_MANAGE]),
            authed_request(),
            create_version_input(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert!(state.versions.contains_key("v9"));
    }

    #[tokio::test]
    async fn update_prefix_rejects_anonymous_callers() {
        let resp = update_prefix(
            state_data(),
            test_ctx(&[PERM_API_MANAGER_MANAGE]),
            anonymous_request(),
            web::Json(UpdatePrefixInput {
                prefix: "api".to_string(),
                migrate_internal: None,
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
