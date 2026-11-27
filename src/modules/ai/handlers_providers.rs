//! ai module - HTTP handlers for provider connections.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

use super::capabilities::resolve_connection;
use super::constants::DEFAULT_RATE_LIMIT_PER_MINUTE;
use super::constants::EVENT_AI_CALL_COMPLETED;
use super::constants::EVENT_AI_CALL_FAILED;
use super::constants::EVENT_AI_PROVIDER_CREATED;
use super::constants::EVENT_AI_PROVIDER_DELETED;
use super::constants::MAX_NAME_LEN;
use super::constants::MAX_RATE_LIMIT_PER_MINUTE;
use super::constants::PERM_AI_MANAGE_PROVIDERS;
use super::constants::TEST_CALL_COMPLETION_TOKENS;
use super::constants::TEST_CALL_PROMPT_TOKENS;
use super::crypto::decrypt_api_key;
use super::crypto::encrypt_api_key;
use super::crypto::encryption_secret;
use super::crypto::key_fingerprint;
use super::gating::AiAuthorization;
use super::gating::AiCallRequest;
use super::gating::authorize_ai_call;
use super::gating::release_cap_reservation;
use super::gating::resolve_caller_product;
use super::helpers::Caller;
use super::helpers::caller;
use super::helpers::caller_company_uuid;
use super::helpers::caller_user_id;
use super::helpers::created;
use super::helpers::emit_audit;
use super::helpers::err;
use super::helpers::ok;
use super::helpers::paginate;
use super::helpers::parse_uuid;
use super::helpers::record_usage;
use super::helpers::require_permission;
use super::helpers::require_tenant;
use super::helpers::require_text;
use super::helpers::resolve_caller_company;
use super::inputs::CreateProviderConnectionInput;
use super::inputs::UpdateProviderConnectionInput;
use super::providers::ProviderEndpointOptions;
use super::providers::ProviderTestResult;
use super::providers::ai_provider_registry;
use super::providers::resolve_base_url_and_model_with_options;
use super::providers::validate_base_url_with_options;
use super::state::AiState;
use super::types::AiProviderCatalogEntry;
use super::types::AiProviderConnection;
use super::types::AiProviderConnectionView;
use super::types::AiProviderKind;
use super::validation::estimate_prompt_tokens;

// Provider connection handlers.

pub(crate) async fn list_providers(req: HttpRequest) -> HttpResponse {
    let result: UnifiedResult<Vec<AiProviderCatalogEntry>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let _caller = caller(&req)?;
        Ok(AiProviderKind::all()
            .iter()
            .map(|kind| AiProviderCatalogEntry {
                kind: kind.as_str().to_string(),
                display_name: kind.display_name().to_string(),
                default_base_url: kind.default_base_url().map(str::to_string),
                default_model: kind.default_model().map(str::to_string),
                key_required: kind.key_required(),
            })
            .collect())
    })();
    match result {
        Ok(entries) => ok(entries),
        Err(error) => err(error, false),
    }
}

pub(crate) async fn list_provider_connections(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let enabled_filter = params.filters.get("enabled").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<AiProviderConnectionView> = state
            .providers
            .iter()
            .map(|entry| entry.value().view())
            .filter(|view| {
                if let Some(kind) = &kind_filter {
                    if view.kind != *kind {
                        return false;
                    }
                }
                if let Some(enabled) = &enabled_filter {
                    let expected = match enabled.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if view.enabled != expected {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack =
                        format!("{} {} {}", view.name, view.kind, view.model).to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["name", "kind", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.cmp(&b.name),
                "kind" => a.kind.cmp(&b.kind),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_provider_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<CreateProviderConnectionInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<AiProviderConnectionView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let options = ProviderEndpointOptions {
            allow_private_host: input.allow_private_host.unwrap_or(false),
            allow_http: input.allow_http.unwrap_or(false),
        };
        let (base_url, model) = resolve_base_url_and_model_with_options(
            input.kind,
            &input.base_url,
            &input.model,
            options,
        )?;
        let rate_limit = input
            .rate_limit_per_minute
            .unwrap_or(DEFAULT_RATE_LIMIT_PER_MINUTE);
        if rate_limit > MAX_RATE_LIMIT_PER_MINUTE {
            return Err(UnifiedError::bad_request(format!(
                "rate_limit_per_minute must be at most {MAX_RATE_LIMIT_PER_MINUTE}"
            )));
        }
        let key = input
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty());
        if input.kind.key_required() && key.is_none() {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" requires api_key",
                input.kind.as_str()
            )));
        }
        let secret = encryption_secret(&ctx)?;
        let (api_key_encrypted, key_fingerprint) = match key {
            Some(plain) => (
                Some(encrypt_api_key(&secret, plain)),
                Some(key_fingerprint(plain)),
            ),
            None => (None, None),
        };
        if state
            .providers
            .iter()
            .any(|entry| entry.value().name == name)
        {
            return Err(UnifiedError::conflict(format!(
                "An AI provider connection named \"{name}\" already exists"
            )));
        }
        let now = Utc::now();
        let connection = AiProviderConnection {
            id: Uuid::now_v7(),
            name,
            kind: input.kind,
            base_url,
            model,
            api_key_encrypted,
            key_fingerprint,
            enabled: input.enabled.unwrap_or(true),
            rate_limit_per_minute: rate_limit,
            allow_private_host: options.allow_private_host,
            allow_http: options.allow_http,
            created_at: now,
            updated_at: now,
        };
        state.providers.insert(connection.id, connection.clone());
        emit_audit(
            &ctx,
            EVENT_AI_PROVIDER_CREATED,
            &caller.actor,
            &format!("ai-provider:{}", connection.id),
            json!({
                "id": connection.id.to_string(),
                "kind": connection.kind.as_str(),
                "name": connection.name,
                "key_configured": connection.api_key_encrypted.is_some(),
            }),
        );
        Ok(connection.view())
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_provider_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiProviderConnectionView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let id = parse_uuid(&path.into_inner(), "provider connection")?;
        state
            .providers
            .get(&id)
            .map(|entry| entry.value().view())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("AI provider connection not found: {id}"))
            })
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_provider_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateProviderConnectionInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<AiProviderConnectionView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let id = parse_uuid(&path.into_inner(), "provider connection")?;
        let mut connection = state
            .providers
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("AI provider connection not found: {id}"))
            })?;
        if let Some(name) = &input.name {
            let name = require_text(name, "name", MAX_NAME_LEN)?;
            if state
                .providers
                .iter()
                .any(|entry| entry.value().name == name && entry.value().id != id)
            {
                return Err(UnifiedError::conflict(format!(
                    "An AI provider connection named \"{name}\" already exists"
                )));
            }
            connection.name = name;
        }
        if input.base_url.is_some() || input.model.is_some() {
            let options = ProviderEndpointOptions {
                allow_private_host: input
                    .allow_private_host
                    .unwrap_or(connection.allow_private_host),
                allow_http: input.allow_http.unwrap_or(connection.allow_http),
            };
            let (base_url, model) = resolve_base_url_and_model_with_options(
                connection.kind,
                &input.base_url.or(Some(connection.base_url.clone())),
                &input.model.or(Some(connection.model.clone())),
                options,
            )?;
            connection.base_url = base_url;
            connection.model = model;
        }
        if let Some(allow_private_host) = input.allow_private_host {
            connection.allow_private_host = allow_private_host;
        }
        if let Some(allow_http) = input.allow_http {
            connection.allow_http = allow_http;
        }
        // A flag change alone must re-validate the stored base URL
        // against the new opt-ins: turning an opt-in off must not
        // leave a private / http endpoint in place.
        if input.allow_private_host.is_some() || input.allow_http.is_some() {
            validate_base_url_with_options(
                &connection.base_url,
                connection.kind,
                ProviderEndpointOptions::from_connection(&connection),
            )?;
        }
        if let Some(key_change) = &input.api_key {
            let secret = encryption_secret(&ctx)?;
            match key_change {
                None => {
                    connection.api_key_encrypted = None;
                    connection.key_fingerprint = None;
                }
                Some(plain) if plain.trim().is_empty() => {
                    connection.api_key_encrypted = None;
                    connection.key_fingerprint = None;
                }
                Some(plain) => {
                    connection.api_key_encrypted = Some(encrypt_api_key(&secret, plain.trim()));
                    connection.key_fingerprint = Some(key_fingerprint(plain.trim()));
                }
            }
        }
        if connection.kind.key_required() && connection.api_key_encrypted.is_none() {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" requires api_key",
                connection.kind.as_str()
            )));
        }
        if let Some(enabled) = input.enabled {
            connection.enabled = enabled;
        }
        if let Some(rate_limit) = input.rate_limit_per_minute {
            if rate_limit > MAX_RATE_LIMIT_PER_MINUTE {
                return Err(UnifiedError::bad_request(format!(
                    "rate_limit_per_minute must be at most {MAX_RATE_LIMIT_PER_MINUTE}"
                )));
            }
            connection.rate_limit_per_minute = rate_limit;
        }
        connection.updated_at = Utc::now();
        state.providers.insert(connection.id, connection.clone());
        Ok(connection.view())
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_provider_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let id = parse_uuid(&path.into_inner(), "provider connection")?;
        let connection = state
            .providers
            .remove(&id)
            .map(|(_, connection)| connection)
            .ok_or_else(|| {
                UnifiedError::not_found(format!("AI provider connection not found: {id}"))
            })?;
        state.rate_buckets.retain(|key, _| key.0 != id);
        emit_audit(
            &ctx,
            EVENT_AI_PROVIDER_DELETED,
            &caller.actor,
            &format!("ai-provider:{id}"),
            json!({
                "id": id.to_string(),
                "kind": connection.kind.as_str(),
                "name": connection.name,
            }),
        );
        Ok(json!({ "deleted": true, "provider_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn test_provider_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiProviderConnection> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let id = parse_uuid(&path.into_inner(), "provider connection")?;
        let connection = state
            .providers
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("AI provider connection not found: {id}"))
            })?;
        Ok(connection)
    })();
    let connection = match result {
        Ok(connection) => connection,
        Err(error) => return err(error, debug),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    // The connection test is a provider call, so the allowlist gate
    // runs first: a test on a connection the caller's company or user
    // is not allowed to use is denied before any provider traffic.
    // A failed test itself is still recorded as a usage row below.
    let user_id = caller_user_id(&caller);
    let company_id = resolve_caller_company(&ctx, &req, &caller);
    // A test call still spends provider traffic, so the cap check
    // sees a small fixed estimate instead of 0 (a 0 estimate
    // would make the cap check a no-op and test traffic free).
    let auth = match authorize_ai_call(
        &ctx,
        &state,
        &caller,
        company_id,
        &AiCallRequest {
            use_case: "test",
            model: &connection.model,
            provider_id: connection.id,
            provider_kind: connection.kind.as_str(),
            estimated_tokens: TEST_CALL_PROMPT_TOKENS + TEST_CALL_COMPLETION_TOKENS,
            estimated_completion_tokens: TEST_CALL_COMPLETION_TOKENS,
            use_personal_key: false,
            product_id: resolve_caller_product(&req),
        },
    ) {
        Ok(auth) => auth,
        Err(error) => return err(error, debug),
    };
    let started = Instant::now();
    let secret = match encryption_secret(&ctx) {
        Ok(secret) => secret,
        Err(error) => {
            release_cap_reservation(&state, auth.reservation_id);
            return err(error, debug);
        }
    };
    let api_key = match &connection.api_key_encrypted {
        Some(stored) => match decrypt_api_key(&secret, stored) {
            Ok(key) => Some(key),
            Err(error) => {
                release_cap_reservation(&state, auth.reservation_id);
                return err(error, debug);
            }
        },
        None => None,
    };
    let adapter = match ai_provider_registry().build(&connection, api_key) {
        Ok(adapter) => adapter,
        Err(error) => {
            release_cap_reservation(&state, auth.reservation_id);
            return err(error, debug);
        }
    };
    match adapter.test_connection().await {
        Ok(test) => {
            let usage = record_usage(
                &state,
                &connection,
                &caller.actor,
                user_id,
                company_id,
                "test",
                None,
                test.latency_ms,
                None,
                false,
                TEST_CALL_PROMPT_TOKENS,
            );
            release_cap_reservation(&state, auth.reservation_id);
            emit_audit(
                &ctx,
                EVENT_AI_CALL_COMPLETED,
                &caller.actor,
                &format!("ai-provider:{}", connection.id),
                json!({
                    "id": usage.id.to_string(),
                    "provider_id": connection.id.to_string(),
                    "provider_kind": connection.kind.as_str(),
                    "use_case": "test",
                    "usage_id": usage.id.to_string(),
                }),
            );
            ok(test)
        }
        Err(error) => {
            let latency_ms = started.elapsed().as_millis() as u64;
            let usage = record_usage(
                &state,
                &connection,
                &caller.actor,
                user_id,
                company_id,
                "test",
                None,
                latency_ms,
                Some(error.code),
                false,
                TEST_CALL_PROMPT_TOKENS,
            );
            release_cap_reservation(&state, auth.reservation_id);
            emit_audit(
                &ctx,
                EVENT_AI_CALL_FAILED,
                &caller.actor,
                &format!("ai-provider:{}", connection.id),
                json!({
                    "id": usage.id.to_string(),
                    "provider_id": connection.id.to_string(),
                    "provider_kind": connection.kind.as_str(),
                    "use_case": "test",
                    "error_code": error.code,
                    "usage_id": usage.id.to_string(),
                }),
            );
            // A failed test is a result, not an API error: report it
            // with `reachable: false` and the (sanitized) reason.
            ok(ProviderTestResult {
                provider_id: connection.id,
                kind: connection.kind.as_str().to_string(),
                reachable: false,
                latency_ms,
                model_count: None,
                detail: error.message.clone(),
            })
        }
    }
}

// Completion and draft handlers.

/// Resolve the provider connection and run the allowlist gate for
/// one draft / completion call, after the prompt is built and before
/// any provider traffic. Returns the resolved connection plus the
/// authorization. Every completion / draft entry point calls this.
pub(crate) fn authorize_call(
    ctx: &Arc<AppContext>,
    state: &AiState,
    req: &HttpRequest,
    caller: &Caller,
    provider_id: Option<Uuid>,
    prompt_text: &str,
    max_tokens: u32,
    use_case: &str,
    use_personal_key: bool,
) -> UnifiedResult<(AiProviderConnection, AiAuthorization)> {
    let connection = resolve_connection(state, provider_id)?;
    let company_id = resolve_caller_company(ctx, req, caller);
    let auth = authorize_ai_call(
        ctx,
        state,
        caller,
        company_id,
        &AiCallRequest {
            use_case,
            model: &connection.model,
            provider_id: connection.id,
            provider_kind: connection.kind.as_str(),
            estimated_tokens: estimate_prompt_tokens(prompt_text) + max_tokens,
            estimated_completion_tokens: max_tokens,
            use_personal_key,
            product_id: resolve_caller_product(req),
        },
    )?;
    Ok((connection, auth))
}
