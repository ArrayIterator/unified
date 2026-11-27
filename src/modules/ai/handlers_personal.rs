//! ai module - HTTP handlers for personal keys, MCP servers and context.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

use super::constants::DEFAULT_RATE_LIMIT_PER_MINUTE;
use super::constants::EVENT_AI_PERSONAL_KEY_CREATED;
use super::constants::EVENT_AI_PERSONAL_KEY_DELETED;
use super::constants::EVENT_AI_PERSONAL_KEY_ROTATED;
use super::constants::MAX_MCP_NAME_LEN;
use super::constants::MAX_PERSONAL_MCP_SERVERS;
use super::constants::MAX_SECRET_LEN;
use super::constants::PERM_AI_READ_USAGE;
use super::constants::PERM_AI_USE;
use super::constants::TEST_CALL_COMPLETION_TOKENS;
use super::constants::TEST_CALL_PROMPT_TOKENS;
use super::context::assemble_shared_context;
use super::crypto::decrypt_api_key;
use super::crypto::encrypt_api_key;
use super::crypto::encryption_secret;
use super::crypto::key_fingerprint;
use super::gating::AiCallRequest;
use super::gating::authorize_ai_call;
use super::gating::release_cap_reservation;
use super::gating::require_allowed_user;
use super::gating::resolve_caller_product;
use super::gating::validate_mcp_endpoint_url;
use super::handlers_usage::load_own_mcp_server;
use super::handlers_usage::usage_matches;
use super::helpers::Caller;
use super::helpers::caller;
use super::helpers::caller_company_uuid;
use super::helpers::caller_user_id;
use super::helpers::check_capability;
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
use super::inputs::CreatePersonalMcpInput;
use super::inputs::UpdatePersonalMcpInput;
use super::inputs::UpsertPersonalKeyInput;
use super::providers::ProviderTestResult;
use super::providers::ai_provider_registry;
use super::providers::resolve_base_url_and_model;
use super::providers::validate_base_url;
use super::state::AiState;
use super::types::AiPersonalKey;
use super::types::AiPersonalKeyView;
use super::types::AiProviderConnection;
use super::types::AiUsageRecord;
use super::types::AiUserMcpServer;
use super::types::AiUserMcpServerView;
use super::types::SharedContext;
use super::types::SiteContextInput;
use super::validation::is_known_model;
use super::validation::validate_cost_cap;
use super::validation::validate_policy_model_name;
use super::validation::validate_token_cap;

/// The caller's own user id, or a 400 when the actor id is not a
/// UUID. Personal endpoints are keyed by user id, so a non-UUID
/// actor cannot own a key or a server.
pub(crate) fn own_user_id(caller: &Caller) -> UnifiedResult<Uuid> {
    caller_user_id(caller).ok_or_else(|| {
        UnifiedError::bad_request(
            "Caller id is not a user id: personal AI settings need a UUID user id",
        )
    })
}

pub(crate) async fn get_my_key(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiPersonalKeyView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let user_id = own_user_id(&caller)?;
        state
            .personal_keys
            .get(&user_id)
            .map(|entry| entry.value().view())
            .ok_or_else(|| {
                UnifiedError::not_found("No personal API key is configured for this user")
            })
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn upsert_my_key(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<UpsertPersonalKeyInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<(AiPersonalKeyView, bool)> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let user_id = own_user_id(&caller)?;
        // In strict mode only an allowed user may configure a key.
        require_allowed_user(&state, user_id)?;
        let key = input.api_key.trim();
        if key.is_empty() || key.len() > MAX_SECRET_LEN {
            return Err(UnifiedError::bad_request(format!(
                "api_key must be between 1 and {MAX_SECRET_LEN} characters"
            )));
        }
        let base_url = match &input.base_url {
            Some(url) => {
                validate_base_url(url, input.provider_kind)?;
                Some(url.trim().to_string())
            }
            None => None,
        };
        let model = match &input.model {
            Some(model) => {
                let model = validate_policy_model_name(model)?;
                if !is_known_model(input.provider_kind, &model) {
                    return Err(UnifiedError::bad_request(format!(
                        "Model \"{model}\" is not a known {} model",
                        input.provider_kind.as_str()
                    )));
                }
                Some(model)
            }
            None => None,
        };
        let monthly_token_cap = validate_token_cap(input.monthly_token_cap)?;
        let monthly_cost_cap_usd = validate_cost_cap(input.monthly_cost_cap_usd)?;
        let secret = encryption_secret(&ctx)?;
        let now = Utc::now();
        let existing = state
            .personal_keys
            .get(&user_id)
            .map(|entry| entry.value().clone());
        let (record, is_new) = match existing {
            Some(mut record) => {
                record.provider_kind = input.provider_kind;
                record.base_url = base_url;
                record.model = model;
                record.api_key_encrypted = encrypt_api_key(&secret, key);
                record.key_fingerprint = key_fingerprint(key);
                record.monthly_token_cap = monthly_token_cap;
                record.monthly_cost_cap_usd = monthly_cost_cap_usd;
                record.enabled = input.enabled.unwrap_or(record.enabled);
                record.updated_at = now;
                (record, false)
            }
            None => (
                AiPersonalKey {
                    id: Uuid::now_v7(),
                    user_id,
                    provider_kind: input.provider_kind,
                    base_url,
                    model,
                    api_key_encrypted: encrypt_api_key(&secret, key),
                    key_fingerprint: key_fingerprint(key),
                    monthly_token_cap,
                    monthly_cost_cap_usd,
                    enabled: input.enabled.unwrap_or(true),
                    created_at: now,
                    updated_at: now,
                },
                true,
            ),
        };
        state.personal_keys.insert(user_id, record.clone());
        emit_audit(
            &ctx,
            if is_new {
                EVENT_AI_PERSONAL_KEY_CREATED
            } else {
                EVENT_AI_PERSONAL_KEY_ROTATED
            },
            &caller.actor,
            &format!("ai-personal-key:{}", record.id),
            json!({
                "id": record.id.to_string(),
                "user_id": user_id.to_string(),
                "provider_kind": record.provider_kind.as_str(),
                "key_configured": true,
            }),
        );
        Ok((record.view(), is_new))
    })();
    match result {
        Ok((view, true)) => created(view),
        Ok((view, false)) => ok(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_my_key(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let user_id = own_user_id(&caller)?;
        let (_, record) = state.personal_keys.remove(&user_id).ok_or_else(|| {
            UnifiedError::not_found("No personal API key is configured for this user")
        })?;
        emit_audit(
            &ctx,
            EVENT_AI_PERSONAL_KEY_DELETED,
            &caller.actor,
            &format!("ai-personal-key:{}", record.id),
            json!({
                "id": record.id.to_string(),
                "user_id": user_id.to_string(),
                "provider_kind": record.provider_kind.as_str(),
            }),
        );
        Ok(json!({ "deleted": true, "key_id": record.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn test_my_key(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let _scope = match caller_company_uuid(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, debug),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_AI_USE) {
        return err(error, debug);
    }
    let user_id = match own_user_id(&caller) {
        Ok(user_id) => user_id,
        Err(error) => return err(error, debug),
    };
    let key = match state.personal_keys.get(&user_id) {
        Some(entry) => entry.value().clone(),
        None => {
            return err(
                UnifiedError::not_found("No personal API key is configured for this user"),
                debug,
            );
        }
    };
    if !key.enabled {
        return err(
            UnifiedError::bad_request("Personal API key is disabled"),
            debug,
        );
    }
    // A synthetic connection carries the key's kind and model so the
    // existing adapters can test it; it is never stored. The test
    // itself runs through the allowlist gate like any other call.
    let (base_url, model) =
        match resolve_base_url_and_model(key.provider_kind, &key.base_url, &key.model) {
            Ok(resolved) => resolved,
            Err(error) => return err(error, debug),
        };
    let connection = AiProviderConnection {
        id: key.id,
        name: format!("personal-key:{}", key.id),
        kind: key.provider_kind,
        base_url,
        model: model.clone(),
        api_key_encrypted: None,
        key_fingerprint: Some(key.key_fingerprint.clone()),
        enabled: true,
        rate_limit_per_minute: DEFAULT_RATE_LIMIT_PER_MINUTE,
        allow_private_host: false,
        allow_http: false,
        created_at: key.created_at,
        updated_at: key.updated_at,
    };
    let company_id = resolve_caller_company(&ctx, &req, &caller);
    // A test call still spends provider traffic, so the cap check
    // sees a small fixed estimate instead of 0.
    let auth = match authorize_ai_call(
        &ctx,
        &state,
        &caller,
        company_id,
        &AiCallRequest {
            use_case: "test",
            model: &model,
            provider_id: key.id,
            provider_kind: key.provider_kind.as_str(),
            estimated_tokens: TEST_CALL_PROMPT_TOKENS + TEST_CALL_COMPLETION_TOKENS,
            estimated_completion_tokens: TEST_CALL_COMPLETION_TOKENS,
            use_personal_key: true,
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
    let api_key = match decrypt_api_key(&secret, &key.api_key_encrypted) {
        Ok(api_key) => Some(api_key),
        Err(error) => {
            release_cap_reservation(&state, auth.reservation_id);
            return err(error, debug);
        }
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
            record_usage(
                &state,
                &connection,
                &caller.actor,
                Some(user_id),
                company_id,
                "test",
                None,
                test.latency_ms,
                None,
                true,
                TEST_CALL_PROMPT_TOKENS,
            );
            release_cap_reservation(&state, auth.reservation_id);
            ok(test)
        }
        Err(error) => {
            let latency_ms = started.elapsed().as_millis() as u64;
            record_usage(
                &state,
                &connection,
                &caller.actor,
                Some(user_id),
                company_id,
                "test",
                None,
                latency_ms,
                Some(error.code),
                true,
                TEST_CALL_PROMPT_TOKENS,
            );
            release_cap_reservation(&state, auth.reservation_id);
            ok(ProviderTestResult {
                provider_id: key.id,
                kind: key.provider_kind.as_str().to_string(),
                reachable: false,
                latency_ms,
                model_count: None,
                detail: error.message.clone(),
            })
        }
    }
}

pub(crate) async fn list_my_mcp(
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
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let user_id = own_user_id(&caller)?;
        let params = query.into_inner();
        let enabled_filter = params.filters.get("enabled").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<AiUserMcpServerView> = state
            .personal_mcp_servers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|server| server.user_id == user_id)
            .filter(|server| {
                if let Some(enabled) = &enabled_filter {
                    let expected = match enabled.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if server.enabled != expected {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack =
                        format!("{} {}", server.name, server.endpoint_url).to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .map(|server| server.view())
            .collect();
        let sort = params.allowed_sort(&["name", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.cmp(&b.name),
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

pub(crate) async fn create_my_mcp(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<CreatePersonalMcpInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<AiUserMcpServerView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let user_id = own_user_id(&caller)?;
        // In strict mode only an allowed user may configure servers.
        require_allowed_user(&state, user_id)?;
        let name = require_text(&input.name, "name", MAX_MCP_NAME_LEN)?;
        let endpoint = validate_mcp_endpoint_url(&input.endpoint_url)?;
        let token = match &input.auth_token {
            Some(token) => {
                let token = token.trim();
                if token.is_empty() || token.len() > MAX_SECRET_LEN {
                    return Err(UnifiedError::bad_request(format!(
                        "auth_token must be between 1 and {MAX_SECRET_LEN} characters"
                    )));
                }
                Some(token.to_string())
            }
            None => None,
        };
        let owned = state
            .personal_mcp_servers
            .iter()
            .filter(|entry| entry.value().user_id == user_id)
            .count();
        if owned >= MAX_PERSONAL_MCP_SERVERS {
            return Err(UnifiedError::bad_request(format!(
                "At most {MAX_PERSONAL_MCP_SERVERS} personal MCP servers per user"
            )));
        }
        let secret = encryption_secret(&ctx)?;
        let (auth_token_encrypted, token_fingerprint) = match &token {
            Some(plain) => (
                Some(encrypt_api_key(&secret, plain)),
                Some(key_fingerprint(plain)),
            ),
            None => (None, None),
        };
        let now = Utc::now();
        let server = AiUserMcpServer {
            id: Uuid::now_v7(),
            user_id,
            name,
            endpoint_url: endpoint.to_string(),
            auth_token_encrypted,
            token_fingerprint,
            enabled: input.enabled.unwrap_or(true),
            created_at: now,
            updated_at: now,
        };
        state.personal_mcp_servers.insert(server.id, server.clone());
        Ok(server.view())
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_my_mcp(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiUserMcpServerView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let user_id = own_user_id(&caller)?;
        let id = parse_uuid(&path.into_inner(), "personal MCP server")?;
        Ok(load_own_mcp_server(&state, user_id, id)?.view())
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_my_mcp(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePersonalMcpInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<AiUserMcpServerView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let user_id = own_user_id(&caller)?;
        let id = parse_uuid(&path.into_inner(), "personal MCP server")?;
        let mut server = load_own_mcp_server(&state, user_id, id)?;
        if let Some(name) = &input.name {
            server.name = require_text(name, "name", MAX_MCP_NAME_LEN)?;
        }
        if let Some(endpoint_url) = &input.endpoint_url {
            server.endpoint_url = validate_mcp_endpoint_url(endpoint_url)?.to_string();
        }
        if let Some(token_change) = &input.auth_token {
            let secret = encryption_secret(&ctx)?;
            match token_change {
                None => {
                    server.auth_token_encrypted = None;
                    server.token_fingerprint = None;
                }
                Some(plain) if plain.trim().is_empty() => {
                    server.auth_token_encrypted = None;
                    server.token_fingerprint = None;
                }
                Some(plain) => {
                    let plain = plain.trim();
                    if plain.len() > MAX_SECRET_LEN {
                        return Err(UnifiedError::bad_request(format!(
                            "auth_token must be at most {MAX_SECRET_LEN} characters"
                        )));
                    }
                    server.auth_token_encrypted = Some(encrypt_api_key(&secret, plain));
                    server.token_fingerprint = Some(key_fingerprint(plain));
                }
            }
        }
        if let Some(enabled) = input.enabled {
            server.enabled = enabled;
        }
        server.updated_at = Utc::now();
        state.personal_mcp_servers.insert(server.id, server.clone());
        Ok(server.view())
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_my_mcp(
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
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let user_id = own_user_id(&caller)?;
        let id = parse_uuid(&path.into_inner(), "personal MCP server")?;
        let _server = load_own_mcp_server(&state, user_id, id)?;
        state.personal_mcp_servers.remove(&id);
        Ok(json!({ "deleted": true, "server_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn usage_by_user(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        // Usage readers may query any user; everyone else sees only
        // their own records.
        let can_read_usage = check_capability(&ctx, &caller.role, PERM_AI_READ_USAGE);
        if !can_read_usage {
            require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        }
        let params = query.into_inner();
        let own_id = caller_user_id(&caller);
        let target = match params.filters.get("user_id") {
            Some(raw) => {
                let target = parse_uuid(raw, "user")?;
                if Some(target) != own_id {
                    if !can_read_usage {
                        return Err(UnifiedError::forbidden(
                            "Reading another user's AI usage requires ai:read_usage",
                        ));
                    }
                    if let Some(company) = caller_company_uuid(&_tenant, &req)? {
                        match state.user_policies.get(&target) {
                            Some(policy) if policy.company_id == company => {}
                            _ => {
                                return Err(UnifiedError::forbidden(
                                    "AI usage of a user outside your company is not visible",
                                ));
                            }
                        }
                    }
                }
                target
            }
            None => {
                own_id.ok_or_else(|| UnifiedError::bad_request("Caller id is not a user id"))?
            }
        };
        let mut items: Vec<AiUsageRecord> = state
            .usage
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| record.user_id == Some(target) && usage_matches(record, &params))
            .collect();
        let sort = params.allowed_sort(&["created_at", "model", "use_case"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "model" => a.model.cmp(&b.model),
                "use_case" => a.use_case.cmp(&b.use_case),
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

pub(crate) async fn usage_by_company(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_READ_USAGE)?;
        let params = query.into_inner();
        // A company-scoped caller is pinned to their own company; a
        // platform usage reader names the company in the query.
        let target = match caller_company_uuid(&_tenant, &req)? {
            Some(company) => company,
            None => {
                let raw = params.filters.get("company_id").ok_or_else(|| {
                    UnifiedError::bad_request(
                        "company_id query parameter is required for platform usage readers",
                    )
                })?;
                parse_uuid(raw, "company")?
            }
        };
        let mut items: Vec<AiUsageRecord> = state
            .usage
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| record.company_id == Some(target) && usage_matches(record, &params))
            .collect();
        let sort = params.allowed_sort(&["created_at", "model", "use_case"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "model" => a.model.cmp(&b.model),
                "use_case" => a.use_case.cmp(&b.use_case),
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

// Shared-context handler.

pub(crate) async fn get_context(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SharedContext> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let _caller = caller(&req)?;
        let params = query.into_inner();
        let query_text = params.search.clone().unwrap_or_default();
        let site = SiteContextInput {
            site_name: params.filters.get("site_name").cloned(),
            site_url: params.filters.get("site_url").cloned(),
            locale: params.filters.get("locale").cloned(),
        };
        Ok(assemble_shared_context(
            Some(&ctx.hooks),
            &query_text,
            &site,
            true,
        ))
    })();
    match result {
        Ok(context) => ok(context),
        Err(error) => err(error, debug),
    }
}
