// documents — HTTP handlers: settings, domains, and hook handlers.

use super::*;
use super::validation::{may_view_company_quota, resolve_caller_company_id};
use unified_core::config::DocumentsConfig;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Handlers — allowlist (admin)
// ---------------------------------------------------------------------------

pub(crate) async fn list_allowlist(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_ADMIN) {
        return json::error_response(&error, debug);
    }
    let params = query.into_inner();
    let mut items: Vec<AllowlistRule> = state
        .allowlist
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rule| {
            matches_search(&rule.module_id, &params)
                && params
                    .filters
                    .get("module_id")
                    .is_none_or(|value| rule.module_id == *value)
        })
        .collect();
    items.sort_by(|a, b| a.module_id.cmp(&b.module_id));
    json::success_response(paginate(items, &params), 200)
}

pub(crate) async fn create_allowlist(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateAllowlistRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_ADMIN) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<AllowlistRule> = (|| {
        let input = body.into_inner();
        let module_id = valid_module_id(&input.module_id)?;
        if state.allowlist_rule(&module_id).is_some() {
            return Err(UnifiedError::conflict(format!(
                "An allowlist rule already exists for module \"{module_id}\""
            )));
        }
        if input.max_bytes == 0 || input.max_bytes > MAX_UPLOAD_BYTES {
            return Err(UnifiedError::bad_request(format!(
                "max_bytes must be between 1 and {MAX_UPLOAD_BYTES}"
            )));
        }
        let rule = AllowlistRule {
            id: new_id(),
            module_id,
            extensions: validate_extensions(&input.extensions)?,
            mime_types: validate_mime_types(&input.mime_types)?,
            max_bytes: input.max_bytes,
            created_at: now(),
            updated_at: now(),
        };
        state.allowlist.insert(rule.id.clone(), rule.clone());
        Ok(rule)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_allowlist(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_ADMIN) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<AllowlistRule> = state
        .allowlist
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Allowlist rule not found"));
    json::respond(result, debug)
}

pub(crate) async fn update_allowlist(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateAllowlistRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_ADMIN) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<AllowlistRule> = (|| {
        let input = body.into_inner();
        let mut rule = state
            .allowlist
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Allowlist rule not found"))?;
        if let Some(extensions) = input.extensions {
            rule.extensions = validate_extensions(&extensions)?;
        }
        if let Some(mime_types) = input.mime_types {
            rule.mime_types = validate_mime_types(&mime_types)?;
        }
        if let Some(max_bytes) = input.max_bytes {
            if max_bytes == 0 || max_bytes > MAX_UPLOAD_BYTES {
                return Err(UnifiedError::bad_request(format!(
                    "max_bytes must be between 1 and {MAX_UPLOAD_BYTES}"
                )));
            }
            rule.max_bytes = max_bytes;
        }
        rule.updated_at = now();
        state.allowlist.insert(rule.id.clone(), rule.clone());
        Ok(rule)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_allowlist(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_ADMIN) {
        return json::error_response(&error, debug);
    }
    let removed = state.allowlist.remove(path.as_str()).is_some();
    if !removed {
        return json::error_response(
            &UnifiedError::not_found("Allowlist rule not found"),
            debug,
        );
    }
    json::success_response(json!({ "deleted": true, "id": path.as_str() }), 200)
}

// ---------------------------------------------------------------------------
// Handlers — quotas, scanners, settings, downloads
// ---------------------------------------------------------------------------

pub(crate) async fn quota_usage(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<HashMap<String, String>>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let filters = query.into_inner();
    let company_id = filters.get("company_id").cloned().unwrap_or_default();
    let user_id = filters
        .get("user_id")
        .cloned()
        .unwrap_or_else(|| found.actor.clone());
    // A caller may only ask about their own usage unless admin.
    let is_admin = check_capability(&ctx, &found.role, PERM_ADMIN);
    if user_id != found.actor && !is_admin {
        return json::error_response(
            &UnifiedError::forbidden("You may only view your own quota usage"),
            debug,
        );
    }
    // A caller may only ask about a company's usage when they
    // belong to (or administer) that company: the requested
    // `company_id` is never trusted on its own. Non-admins pass
    // only when their resolved company matches, or when they own
    // a document in the requested company (see
    // `may_view_company_quota`).
    if !company_id.is_empty()
        && !may_view_company_quota(
            is_admin,
            resolve_caller_company_id(&ctx, &req, &found).as_deref(),
            &company_id,
            state.owns_document_in_company(&found.actor, &company_id),
        )
    {
        return json::error_response(
            &UnifiedError::forbidden(
                "You may only view quota usage for your own company",
            ),
            debug,
        );
    }
    let mut usage: Vec<Value> = Vec::new();
    for (scope, scope_id) in [("company", company_id.as_str()), ("user", user_id.as_str())] {
        if scope_id.is_empty() {
            continue;
        }
        let key = format!("{scope}:{scope_id}");
        let (used, limit) = match state.quotas.get(&key) {
            Some(quota) => (quota.used_bytes, quota.bytes_limit),
            None => (0, None),
        };
        usage.push(json!({
            "scope": scope,
            "scope_id": scope_id,
            "used_bytes": used,
            "bytes_limit": limit,
            "remaining_bytes": limit.map(|value| value.saturating_sub(used)),
        }));
    }
    json::success_response(json!({ "usage": usage }), 200)
}

pub(crate) async fn list_quotas(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_ADMIN) {
        return json::error_response(&error, debug);
    }
    let params = query.into_inner();
    let mut items: Vec<DocumentQuota> = state
        .quotas
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|quota| {
            // Company-scope quotas belong to one company; user-scope
            // quotas are personal. A scoped caller sees only its own
            // company's quotas.
            if let Some(scope) = &_scope {
                if quota.scope == "company" && quota.scope_id != scope.as_str() {
                    return false;
                }
            }
            params
                .filters
                .get("scope")
                .is_none_or(|value| quota.scope == *value)
                && params
                    .filters
                    .get("scope_id")
                    .is_none_or(|value| quota.scope_id == *value)
        })
        .collect();
    items.sort_by(|a, b| {
        a.scope
            .cmp(&b.scope)
            .then(a.scope_id.cmp(&b.scope_id))
    });
    json::success_response(paginate(items, &params), 200)
}

pub(crate) async fn create_quota(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateQuotaRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_ADMIN) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<DocumentQuota> = (|| {
        let input = body.into_inner();
        let scope = match input.scope.trim() {
            "company" | "user" => input.scope.trim().to_string(),
            _ => {
                return Err(UnifiedError::bad_request(
                    "scope must be \"company\" or \"user\"",
                ))
            }
        };
        let scope_id = require_text(&input.scope_id, "scope_id", 128)?;
        let key = format!("{scope}:{scope_id}");
        let mut quota = state
            .quotas
            .get(&key)
            .map(|entry| entry.value().clone())
            .unwrap_or(DocumentQuota {
                id: new_id(),
                scope: scope.clone(),
                scope_id: scope_id.clone(),
                bytes_limit: None,
                used_bytes: 0,
                created_at: now(),
                updated_at: now(),
            });
        quota.bytes_limit = input.bytes_limit;
        quota.updated_at = now();
        state.quotas.insert(key, quota.clone());
        Ok(quota)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_scanners(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return json::error_response(&error, debug);
    }
    json::success_response(
        json!({ "scanners": state.scanner_names(), "count": state.scanners.len() }),
        200,
    )
}

pub(crate) async fn get_settings(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return json::error_response(&error, debug);
    }
    json::success_response(state.settings(), 200)
}

pub(crate) async fn update_settings(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_ADMIN) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<DocumentSettings> = (|| {
        let input = body.into_inner();
        let mut settings = state.settings();
        if let Some(value) = input.allow_unscanned_downloads {
            settings.allow_unscanned_downloads = value;
        }
        if let Some(value) = input.default_chunk_size {
            if !(MIN_CHUNK_SIZE..=MAX_CHUNK_SIZE).contains(&value) {
                return Err(UnifiedError::bad_request(format!(
                    "default_chunk_size must be between {MIN_CHUNK_SIZE} and {MAX_CHUNK_SIZE}"
                )));
            }
            settings.default_chunk_size = value;
        }
        if let Some(value) = input.session_ttl_secs {
            if !(60..=604_800).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "session_ttl_secs must be between 60 and 604800",
                ));
            }
            settings.session_ttl_secs = value;
        }
        if let Some(value) = input.max_upload_bytes {
            if value == 0 || value > MAX_UPLOAD_BYTES {
                return Err(UnifiedError::bad_request(format!(
                    "max_upload_bytes must be between 1 and {MAX_UPLOAD_BYTES}"
                )));
            }
            settings.max_upload_bytes = value;
        }
        if let Some(value) = input.spool_dir {
            let value = value.trim().to_string();
            if value.is_empty() {
                // Empty string clears the override back to the default.
                settings.spool_dir = None;
            } else {
                if value.chars().any(|c| c == '\0' || c.is_control()) {
                    return Err(UnifiedError::bad_request(
                        "spool_dir contains invalid characters",
                    ));
                }
                settings.spool_dir = Some(value);
            }
        }
        if let Some(value) = input.max_chunk_ram_bytes {
            if !(DocumentsConfig::MAX_CHUNK_RAM_MIN..=DocumentsConfig::MAX_CHUNK_RAM_MAX)
                .contains(&value)
            {
                return Err(UnifiedError::bad_request(format!(
                    "max_chunk_ram_bytes must be between {} and {}",
                    DocumentsConfig::MAX_CHUNK_RAM_MIN,
                    DocumentsConfig::MAX_CHUNK_RAM_MAX
                )));
            }
            settings.max_chunk_ram_bytes = value;
        }
        *state.settings.write().expect("settings lock poisoned") = settings.clone();
        Ok(settings)
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_downloads(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let params = query.into_inner();
    let is_admin = check_capability(&ctx, &found.role, PERM_ADMIN);
    let mut items: Vec<DownloadRecord> = state
        .downloads
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| {
            (is_admin || record.user_id == found.actor)
                && params
                    .filters
                    .get("document_id")
                    .is_none_or(|value| record.document_id == *value)
                && params
                    .filters
                    .get("via")
                    .is_none_or(|value| record.via == *value)
                && params
                    .filters
                    .get("scan_status")
                    .is_none_or(|value| record.scan_status == *value)
        })
        .collect();
    items.sort_by(|a, b| b.downloaded_at.cmp(&a.downloaded_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}
