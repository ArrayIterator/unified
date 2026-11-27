// Api Manager - Endpoint and registry handlers.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::hardening::client_ip;
use unified_core::json as api_json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use uuid::Uuid;

use super::validation::{
    PERM_API_MANAGER_MANAGE, PERM_API_MANAGER_READ, require_admin,
};
use super::{API_KEY_HEADER, ApiEndpoint, ApiManagerState, CreateEndpointInput, RegistryRow,
    ResolveQuery, UpdateEndpointInput, builtin_collision_error, emit_endpoint_conflict,
    normalize_path_template, parse_version_number, rate_limited_error_response,
    resolve_input_support, strip_public_prefix, validate_method, validate_path, validate_payload,
    validate_rules};

// Endpoint handlers

pub(crate) async fn list_endpoints(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_READ) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let params = query.into_inner();
    let search = params.search.clone();
    let version_filter = params.filters.get("version").cloned();
    let method_filter = params.filters.get("method").cloned();
    let enabled_filter = params.filters.get("enabled").cloned();
    let owner_filter = params.filters.get("owner_module").cloned();
    let source_filter = params.filters.get("source").cloned();
    let auth_filter = params.filters.get("auth_required").cloned();
    let mut items: Vec<ApiEndpoint> = state
        .endpoints
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|endpoint| {
            if let Some(version) = &version_filter {
                // A version matches when the endpoint's version
                // support covers it, or when its legacy single
                // version field equals it (legacy `v1.2` records).
                let known = state.version_ids();
                if !endpoint.version_support.supports(version, &known)
                    && endpoint.version != *version
                {
                    return false;
                }
            }
            if let Some(method) = &method_filter {
                if endpoint.method != method.to_uppercase() {
                    return false;
                }
            }
            if let Some(enabled) = &enabled_filter {
                let wanted = enabled == "true";
                if endpoint.enabled != wanted {
                    return false;
                }
            }
            if let Some(owner) = &owner_filter {
                if endpoint.owner_module != *owner {
                    return false;
                }
            }
            if let Some(source) = &source_filter {
                if endpoint.source != *source {
                    return false;
                }
            }
            if let Some(auth_required) = &auth_filter {
                let wanted = auth_required == "true";
                if endpoint.auth_required != wanted {
                    return false;
                }
            }
            if let Some(needle) = &search {
                let needle = needle.to_lowercase();
                return endpoint.path.to_lowercase().contains(&needle)
                    || endpoint.description.to_lowercase().contains(&needle)
                    || endpoint.version.to_lowercase().contains(&needle);
            }
            true
        })
        .collect();
    let sort = params.allowed_sort(&["created_at", "path", "version"], "created_at");
    let descending = params.normalized().order == "desc";
    items.sort_by(|a, b| {
        let ordering = match sort.as_str() {
            "path" => a.path.cmp(&b.path),
            "version" => a.version.cmp(&b.version),
            _ => a.created_at.cmp(&b.created_at),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<ApiEndpoint> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

pub(crate) async fn create_endpoint(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    input: web::Json<CreateEndpointInput>,
) -> HttpResponse {
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    state.ensure_seeded_versions();
    let input = input.into_inner();
    let outcome = (|| -> Result<ApiEndpoint, UnifiedError> {
        let method = validate_method(&input.method)?;
        let path = validate_path(&input.path)?;
        let support = resolve_input_support(
            &state,
            &input.version,
            &input.versions,
            &input.version_support,
        )?;
        let rules = input.rules.unwrap_or_default();
        validate_rules(&rules)?;
        let rate_limit = input.rate_limit.unwrap_or(60);
        if rate_limit == 0 || rate_limit > 100_000 {
            return Err(UnifiedError::bad_request(
                "rate_limit must be between 1 and 100000 requests per minute",
            ));
        }
        let owner_module = match input.owner_module {
            Some(owner) => {
                let owner = owner.trim().to_string();
                if owner.is_empty() {
                    return Err(UnifiedError::bad_request("owner_module must not be empty"));
                }
                if owner == "builtin" {
                    return Err(UnifiedError::bad_request(
                        "owner_module \"builtin\" is reserved for endpoints imported from the module route catalog",
                    ));
                }
                owner
            }
            None => "custom".to_string(),
        };
        // Collision check 1: a built-in / core / module endpoint
        // already exists with the same method + path in any chosen
        // version. This is rejected with 422 and emits
        // `api.endpoint.conflict`.
        if let Some((conflicting, overlapping)) =
            state.builtin_conflict(&method, &path, &support)
        {
            emit_endpoint_conflict(&ctx, &method, &path, &conflicting, &overlapping);
            return Err(builtin_collision_error(
                &method,
                &path,
                &conflicting,
                &overlapping,
            ));
        }
        // Collision check 2: another custom endpoint already covers
        // the same method + path in an overlapping version. The
        // original 409 behaviour is kept for custom-vs-custom
        // duplicates.
        if let Some((existing, overlapping)) =
            state.custom_conflict(&method, &path, &support, None)
        {
            let _ = existing;
            return Err(UnifiedError::conflict(format!(
                "an endpoint for {method} {path} already exists in version(s) {}",
                overlapping.join(", ")
            )));
        }
        let now = Utc::now();
        let known = state.version_ids();
        let version = support
            .lowest_version(&known)
            .unwrap_or_else(|| "v1".to_string());
        let endpoint = ApiEndpoint {
            id: Uuid::now_v7(),
            method,
            path,
            version,
            description: input.description.unwrap_or_default(),
            rate_limit,
            enabled: input.enabled.unwrap_or(true),
            rules,
            execution_count: 0,
            owner_module: owner_module.clone(),
            auth_required: input.auth_required.unwrap_or(true),
            source: "custom".to_string(),
            version_support: support.clone(),
            created_at: now,
            updated_at: now,
        };
        state.endpoints.insert(endpoint.id, endpoint.clone());
        ctx.hooks.emit(
            "api.endpoint.created",
            json!({
                "endpoint_id": endpoint.id.to_string(),
                "method": endpoint.method,
                "path": endpoint.path,
                "versions": support.available_in(&known),
                "owner_module": owner_module,
            }),
        );
        Ok(endpoint)
    })();
    match outcome {
        Ok(endpoint) => api_json::success_response(endpoint, 201),
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

pub(crate) async fn get_endpoint(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_READ) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    match state.endpoints.get(&id.into_inner()) {
        Some(endpoint) => api_json::success_response(endpoint.value().clone(), 200),
        None => api_json::error_response(
            &UnifiedError::not_found("endpoint not found"),
            ctx.is_debug(),
        ),
    }
}

pub(crate) async fn update_endpoint(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    input: web::Json<UpdateEndpointInput>,
) -> HttpResponse {
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    state.ensure_seeded_versions();
    let id = id.into_inner();
    let input = input.into_inner();
    let outcome = (|| -> Result<ApiEndpoint, UnifiedError> {
        let mut endpoint = state
            .endpoints
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("endpoint not found"))?;
        if let Some(method) = &input.method {
            endpoint.method = validate_method(method)?;
        }
        if let Some(path) = &input.path {
            endpoint.path = validate_path(path)?;
        }
        if input.version.is_some() || input.versions.is_some() || input.version_support.is_some()
        {
            endpoint.version_support = resolve_input_support(
                &state,
                &input.version,
                &input.versions,
                &input.version_support,
            )?;
            let known = state.version_ids();
            endpoint.version = endpoint
                .version_support
                .lowest_version(&known)
                .unwrap_or_else(|| endpoint.version.clone());
        }
        if let Some(rules) = &input.rules {
            validate_rules(rules)?;
            endpoint.rules = rules.clone();
        }
        if let Some(rate_limit) = input.rate_limit {
            if rate_limit == 0 || rate_limit > 100_000 {
                return Err(UnifiedError::bad_request(
                    "rate_limit must be between 1 and 100000 requests per minute",
                ));
            }
            endpoint.rate_limit = rate_limit;
        }
        if let Some(description) = input.description {
            endpoint.description = description;
        }
        if let Some(enabled) = input.enabled {
            endpoint.enabled = enabled;
        }
        if let Some(owner_module) = input.owner_module {
            let owner_module = owner_module.trim().to_string();
            if owner_module.is_empty() {
                return Err(UnifiedError::bad_request("owner_module must not be empty"));
            }
            if owner_module == "builtin" {
                return Err(UnifiedError::bad_request(
                    "owner_module \"builtin\" is reserved for endpoints imported from the module route catalog",
                ));
            }
            endpoint.owner_module = owner_module;
        }
        if let Some(auth_required) = input.auth_required {
            endpoint.auth_required = auth_required;
        }
        // Re-run both collision checks against the updated method +
        // path + version support, excluding this endpoint itself.
        if let Some((conflicting, overlapping)) = state.builtin_conflict(
            &endpoint.method,
            &endpoint.path,
            &endpoint.version_support,
        ) {
            emit_endpoint_conflict(
                &ctx,
                &endpoint.method,
                &endpoint.path,
                &conflicting,
                &overlapping,
            );
            return Err(builtin_collision_error(
                &endpoint.method,
                &endpoint.path,
                &conflicting,
                &overlapping,
            ));
        }
        if let Some((_, overlapping)) = state.custom_conflict(
            &endpoint.method,
            &endpoint.path,
            &endpoint.version_support,
            Some(id),
        ) {
            return Err(UnifiedError::conflict(format!(
                "an endpoint for {} {} already exists in version(s) {}",
                endpoint.method,
                endpoint.path,
                overlapping.join(", ")
            )));
        }
        endpoint.updated_at = Utc::now();
        state.endpoints.insert(id, endpoint.clone());
        Ok(endpoint)
    })();
    match outcome {
        Ok(endpoint) => api_json::success_response(endpoint, 200),
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

pub(crate) async fn delete_endpoint(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let id = id.into_inner();
    match state.endpoints.remove(&id) {
        Some((_, endpoint)) => api_json::success_response(
            json!({ "id": endpoint.id, "deleted": true }),
            200,
        ),
        None => api_json::error_response(
            &UnifiedError::not_found("endpoint not found"),
            ctx.is_debug(),
        ),
    }
}

pub(crate) async fn execute_endpoint(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    payload: web::Json<Value>,
) -> HttpResponse {
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let id = id.into_inner();
    let payload = payload.into_inner();
    let Some(endpoint) = state.endpoints.get(&id).map(|entry| entry.value().clone()) else {
        return api_json::error_response(
            &UnifiedError::not_found("endpoint not found"),
            ctx.is_debug(),
        );
    };
    if !endpoint.enabled {
        return api_json::error_response(
            &UnifiedError::forbidden("endpoint is disabled"),
            ctx.is_debug(),
        );
    }
    // Per-endpoint per-minute rate limit. The 429 carries the same
    // metadata keys and `Retry-After` / `X-RateLimit-*` headers as
    // the access-control rate limit (see `check_access_rate_limit`).
    let now_secs = Utc::now().timestamp().max(0) as u64;
    let minute = now_secs / 60;
    let reset_in_secs = 60 - (now_secs % 60);
    state.prune_rate_counters(minute);
    let counter_key = format!("{id}:{minute}");
    let count = {
        let mut counter = state.rate_counters.entry(counter_key).or_insert(0);
        *counter += 1;
        *counter
    };
    if count > u64::from(endpoint.rate_limit) {
        return rate_limited_error_response(
            &UnifiedError::new(
                format!(
                    "rate limit exceeded: at most {} requests per minute for this endpoint",
                    endpoint.rate_limit
                ),
                429,
            )
            .with_metadata("reason", json!("rate_limited"))
            .with_metadata("endpoint_id", json!(id.to_string()))
            .with_metadata("rate_limit", json!(endpoint.rate_limit))
            .with_metadata("limit", json!(endpoint.rate_limit))
            .with_metadata("remaining", json!(0))
            .with_metadata("retry_after_secs", json!(reset_in_secs))
            .with_metadata("reset_in_secs", json!(reset_in_secs)),
            ctx.is_debug(),
        );
    }

    let errors = validate_payload(&payload, &endpoint.rules);
    let valid = errors.is_empty();
    // Increment the counter atomically on the stored entry: the old
    // clone, `execution_count += 1`, `insert` sequence was a
    // read-modify-write, so concurrent executes overwrote each other
    // and lost counts. `get_mut` holds the shard lock across the
    // increment, so no execute can interleave, and no other field
    // is clobbered by re-inserting a stale clone.
    if let Some(mut stored) = state.endpoints.get_mut(&id) {
        stored.execution_count += 1;
        stored.updated_at = Utc::now();
    }
    ctx.hooks.emit(
        "api-manager.endpoint.executed",
        json!({
            "endpoint_id": id.to_string(),
            "method": endpoint.method,
            "path": endpoint.path,
            "version": endpoint.version,
            "valid": valid,
            "error_count": errors.len(),
        }),
    );
    api_json::success_response(
        json!({
            "endpoint": {
                "id": endpoint.id,
                "method": endpoint.method,
                "path": endpoint.path,
                "version": endpoint.version,
            },
            "valid": valid,
            "errors": errors,
            "echo": payload,
            "dispatch": "validation_only",
            "note": "Payload validation only. Dispatching the request into a registered core route handler is out of scope for this milestone.",
        }),
        200,
    )
}

pub(crate) async fn list_registry(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_READ) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    state.ensure_seeded_versions();
    state.ensure_collected(&ctx.modules.route_catalog(), &ctx.modules);
    let params = query.into_inner();
    let search = params.search.clone();
    let module_filter = params.filters.get("module").cloned();
    let method_filter = params.filters.get("method").cloned();
    let version_filter = params.filters.get("version").cloned();
    let auth_filter = params.filters.get("auth_required").cloned();
    let source_filter = params.filters.get("source").cloned();
    let mut rows: Vec<RegistryRow> = state
        .registry_rows()
        .into_iter()
        .filter(|row| {
            if let Some(module) = &module_filter {
                if row.owner_module != *module {
                    return false;
                }
            }
            if let Some(method) = &method_filter {
                if row.method != method.to_uppercase() {
                    return false;
                }
            }
            if let Some(version) = &version_filter {
                if row.version != *version {
                    return false;
                }
            }
            if let Some(auth_required) = &auth_filter {
                if row.auth_required != (auth_required == "true") {
                    return false;
                }
            }
            if let Some(source) = &source_filter {
                if row.source != *source {
                    return false;
                }
            }
            if let Some(needle) = &search {
                let needle = needle.to_lowercase();
                return row.path.to_lowercase().contains(&needle)
                    || row.description.to_lowercase().contains(&needle)
                    || row.owner_module.to_lowercase().contains(&needle);
            }
            true
        })
        .collect();
    let sort = params.allowed_sort(&["path", "version", "method"], "path");
    let descending = params.normalized().order == "desc";
    rows.sort_by(|a, b| {
        let ordering = match sort.as_str() {
            "version" => parse_version_number(&a.version)
                .unwrap_or(0)
                .cmp(&parse_version_number(&b.version).unwrap_or(0))
                .then(a.path.cmp(&b.path)),
            "method" => a.method.cmp(&b.method).then(a.path.cmp(&b.path)),
            _ => a.path.cmp(&b.path).then(a.method.cmp(&b.method)),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    // Per-version endpoint counts for the grouped overview.
    let mut grouped: BTreeMap<String, u64> = BTreeMap::new();
    for row in state.registry_rows() {
        *grouped.entry(row.version).or_insert(0) += 1;
    }
    let versions: Vec<Value> = grouped
        .into_iter()
        .map(|(version, count)| {
            let record = state.versions.get(&version);
            json!({
                "version": version,
                "count": count,
                "enabled": record.as_ref().map(|entry| entry.enabled).unwrap_or(false),
                "deprecated": record.as_ref().map(|entry| entry.deprecated).unwrap_or(false),
            })
        })
        .collect();
    let pagination = params.normalized();
    let total = rows.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<RegistryRow> = rows
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    let page = PageResult::new(page_items, total, &pagination);
    api_json::success_response(
        json!({
            "items": page.items,
            "pagination": page.pagination,
            "versions": versions,
        }),
        200,
    )
}

pub(crate) async fn get_registry_detail(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<ResolveQuery>,
) -> HttpResponse {
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_READ) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    state.ensure_seeded_versions();
    state.ensure_collected(&ctx.modules.route_catalog(), &ctx.modules);
    let query = query.into_inner();
    let method = match query.method {
        Some(method) => match validate_method(&method) {
            Ok(method) => method,
            Err(error) => return api_json::error_response(&error, ctx.is_debug()),
        },
        None => {
            return api_json::error_response(
                &UnifiedError::bad_request("query parameter \"method\" is required"),
                ctx.is_debug(),
            );
        }
    };
    let path = match query.path {
        Some(path) => match validate_path(&path) {
            Ok(path) => path,
            Err(error) => return api_json::error_response(&error, ctx.is_debug()),
        },
        None => {
            return api_json::error_response(
                &UnifiedError::bad_request("query parameter \"path\" is required"),
                ctx.is_debug(),
            );
        }
    };
    let known = state.version_ids();
    let rows: Vec<RegistryRow> = state
        .registry_rows()
        .into_iter()
        .filter(|row| {
            row.method == method
                && (normalize_path_template(&row.path) == normalize_path_template(&path)
                    || normalize_path_template(&ApiManagerState::relative_path(&row.path))
                        == normalize_path_template(&path)
                    || normalize_path_template(&row.path)
                        == normalize_path_template(&ApiManagerState::relative_path(&path)))
        })
        .collect();
    if rows.is_empty() {
        return api_json::error_response(
            &UnifiedError::not_found(format!("No endpoint matches {method} {path}"))
                .with_metadata("method", json!(method))
                .with_metadata("path", json!(path)),
            ctx.is_debug(),
        );
    }
    let available_in: Vec<String> = rows.iter().map(|row| row.version.clone()).collect();
    let first = rows[0].clone();
    let detail = if let Some(version) = query.version {
        match rows.into_iter().find(|row| row.version == version) {
            Some(row) => row,
            None => {
                return api_json::error_response(
                    &UnifiedError::not_found(format!(
                        "Endpoint {method} {path} is not available in API version \"{version}\""
                    ))
                    .with_metadata("available_in", json!(available_in))
                    .with_metadata("version", json!(version)),
                    ctx.is_debug(),
                );
            }
        }
    } else {
        first
    };
    let _ = known;
    let version_support = detail.version_support.clone();
    let owner_module = detail.owner_module.clone();
    let auth_required = detail.auth_required;
    let rate_limit = detail.rate_limit;
    api_json::success_response(
        json!({
            "endpoint": detail,
            "available_in": available_in,
            "version_support": version_support,
            "owner_module": owner_module,
            "auth_required": auth_required,
            "rate_limit": rate_limit,
        }),
        200,
    )
}

pub(crate) async fn resolve_handler(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<ResolveQuery>,
) -> HttpResponse {
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_READ) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    state.ensure_seeded_versions();
    state.ensure_collected(&ctx.modules.route_catalog(), &ctx.modules);
    let query = query.into_inner();
    let method = match query.method {
        Some(method) => match validate_method(&method) {
            Ok(method) => method,
            Err(error) => return api_json::error_response(&error, ctx.is_debug()),
        },
        None => "GET".to_string(),
    };
    let version = match query.version {
        Some(version) => version.trim().to_string(),
        None => {
            return api_json::error_response(
                &UnifiedError::bad_request("query parameter \"version\" is required"),
                ctx.is_debug(),
            );
        }
    };
    let raw_path = match query.path {
        Some(path) => path,
        None => {
            return api_json::error_response(
                &UnifiedError::bad_request("query parameter \"path\" is required"),
                ctx.is_debug(),
            );
        }
    };
    let (carried_version, relative_path) = strip_public_prefix(&state, &raw_path);
    let version = carried_version.unwrap_or(version);
    match state.resolve_endpoint(&method, &version, &relative_path) {
        Ok(resolved) => api_json::success_response(
            json!({
                "matched": true,
                "method": method,
                "version": version,
                "path": relative_path,
                "public_path": resolved.endpoint.public_path,
                "endpoint": resolved.endpoint,
                "deprecated_warning": resolved.deprecated_warning,
            }),
            200,
        ),
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

/// Public versioned gateway: resolves any request under
/// `/public/{version}/{path...}` against the registry and reports
/// the match. Resolution only - dispatching into the matched core
/// route handler is NOT IMPLEMENTED (this endpoint never executes
/// the handler; there is no in-process dispatcher from a resolved
/// registry row to an actix route, and building one would have to
/// re-run routing, auth, tenant scoping and body forwarding).
/// What DOES work: the registry honours `RouteVersionSupport`
/// (collection expands each module's declaration into per-version
/// rows, `resolve_endpoint` filters by version and reports
/// `not_in_version`), and a deprecated version is signalled both
/// in the body (`deprecated_warning`) and on the wire (`Deprecation`
/// / `Sunset` headers, RFC 8594). No module currently declares
/// anything but `RouteVersionSupport::All`, so every built-in route
/// resolves in every enabled version until a module opts out.
///
/// Deliberately NOT gated by `require_admin`: this is the public
/// gateway, not an admin route. Access control runs inside the
/// handler itself via `check_public_access` (default deny, API-key
/// auth delegated to the apikeys module), so anonymous callers
/// still cannot reach an endpoint without an allow rule and, where
/// required, a valid in-scope key.
pub(crate) async fn resolve_public(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    request: HttpRequest,
) -> HttpResponse {
    state.ensure_seeded_versions();
    state.ensure_collected(&ctx.modules.route_catalog(), &ctx.modules);
    let version = request
        .match_info()
        .get("version")
        .unwrap_or("")
        .to_string();
    let method = request.method().to_string();
    let tail = request
        .match_info()
        .get("path")
        .unwrap_or("")
        .to_string();
    let relative_path = format!("/{tail}");
    match state.resolve_endpoint(&method, &version, &relative_path) {
        Ok(resolved) => {
            // The public gateway enforces access control itself (the
            // middleware exempts this module's prefix): a resolved
            // endpoint with no allow rule, or a missing, invalid, or
            // out-of-scope key, is rejected here, before the
            // resolution is returned. The check runs against the
            // public path (`/{prefix}/{version}/{tail}`), not the
            // gateway's own mount path.
            let presented_key = request
                .headers()
                .get(API_KEY_HEADER)
                .and_then(|value| value.to_str().ok());
            let client_ip = request.peer_addr().map(|addr| addr.ip().to_string());
            let public_path = state.public_url(&version, &relative_path);
            if let Err(error) =
                state.check_public_access(&method, &public_path, presented_key, client_ip)
            {
                return rate_limited_error_response(&error, ctx.is_debug());
            }
            // A deprecated version is signalled on the wire as well
            // as in the body (RFC 8594): `Deprecation: true`, plus
            // `Sunset:` with the version's sunset date in IMF-fixdate
            // form when one is set.
            let mut response = api_json::success_response(
            json!({
                "matched": true,
                "method": method,
                "version": version,
                "path": relative_path,
                "public_path": resolved.endpoint.public_path,
                "endpoint": resolved.endpoint,
                "deprecated_warning": resolved.deprecated_warning,
                "dispatch": "resolution_only",
                "note": "Resolution only. Dispatching the request into the matched core route handler is not implemented.",
            }),
            200,
        );
            if resolved.version.deprecated {
                response.headers_mut().insert(
                    actix_web::http::header::HeaderName::from_static("Deprecation"),
                    actix_web::http::header::HeaderValue::from_static("true"),
                );
                if let Some(sunset) = resolved.version.sunset_date {
                    if let Ok(value) = actix_web::http::header::HeaderValue::from_str(
                        &sunset.format("%a, %d %b %Y %H:%M:%S GMT").to_string(),
                    ) {
                        response.headers_mut().insert(
                            actix_web::http::header::HeaderName::from_static("Sunset"),
                            value,
                        );
                    }
                }
            }
            response
        }
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
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

    fn create_input() -> web::Json<CreateEndpointInput> {
        web::Json(CreateEndpointInput {
            method: "GET".to_string(),
            path: "/things".to_string(),
            version: Some("v1".to_string()),
            versions: None,
            version_support: None,
            description: None,
            rate_limit: None,
            enabled: None,
            rules: None,
            owner_module: None,
            auth_required: None,
        })
    }

    fn seed_endpoint(state: &web::Data<ApiManagerState>) -> ApiEndpoint {
        let now = Utc::now();
        let endpoint = ApiEndpoint {
            id: Uuid::now_v7(),
            method: "POST".to_string(),
            path: "/things".to_string(),
            version: "v1".to_string(),
            description: String::new(),
            rate_limit: 10_000,
            enabled: true,
            rules: Vec::new(),
            execution_count: 0,
            owner_module: "custom".to_string(),
            auth_required: true,
            source: "custom".to_string(),
            version_support: crate::modules::api_manager::VersionSupport::All,
            created_at: now,
            updated_at: now,
        };
        state.endpoints.insert(endpoint.id, endpoint.clone());
        endpoint
    }

    #[tokio::test]
    async fn list_endpoints_rejects_anonymous_callers() {
        let resp = list_endpoints(
            state_data(),
            test_ctx(&[PERM_API_MANAGER_READ]),
            anonymous_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_endpoints_rejects_callers_without_read() {
        let resp = list_endpoints(
            state_data(),
            test_ctx(&[PERM_API_MANAGER_MANAGE]),
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn create_endpoint_rejects_anonymous_callers() {
        let resp = create_endpoint(
            state_data(),
            test_ctx(&[PERM_API_MANAGER_MANAGE]),
            anonymous_request(),
            create_input(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create_endpoint_rejects_callers_without_manage() {
        let resp = create_endpoint(
            state_data(),
            test_ctx(&[PERM_API_MANAGER_READ]),
            authed_request(),
            create_input(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn execute_endpoint_rejects_anonymous_callers() {
        let state = state_data();
        let endpoint = seed_endpoint(&state);
        let resp = execute_endpoint(
            state,
            test_ctx(&[PERM_API_MANAGER_MANAGE]),
            anonymous_request(),
            web::Path::from(endpoint.id),
            web::Json(json!({})),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn execute_endpoint_counts_once_for_authorized_caller() {
        let state = state_data();
        let endpoint = seed_endpoint(&state);
        let resp = execute_endpoint(
            state.clone(),
            test_ctx(&[PERM_API_MANAGER_MANAGE]),
            authed_request(),
            web::Path::from(endpoint.id),
            web::Json(json!({})),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            state.endpoints.get(&endpoint.id).unwrap().execution_count,
            1
        );
    }

    #[test]
    fn concurrent_execute_counts_every_call() {
        // The old clone / increment / insert sequence lost counts
        // under concurrency; the atomic `get_mut` increment must
        // record every one of these parallel executes.
        let state = state_data();
        let ctx = test_ctx(&[PERM_API_MANAGER_MANAGE]);
        let endpoint = seed_endpoint(&state);
        let id = endpoint.id;
        let calls: u64 = 16;
        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..calls {
                let state = state.clone();
                let ctx = ctx.clone();
                handles.push(scope.spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();
                    runtime.block_on(execute_endpoint(
                        state,
                        ctx,
                        authed_request(),
                        web::Path::from(id),
                        web::Json(json!({})),
                    ))
                }));
            }
            for handle in handles {
                assert_eq!(handle.join().unwrap().status(), StatusCode::OK);
            }
        });
        assert_eq!(
            state.endpoints.get(&id).unwrap().execution_count,
            calls
        );
    }
}
