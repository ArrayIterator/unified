//! ai module - HTTP handlers for usage and access policies.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

use super::constants::EVENT_AI_ACCESS_POLICY_CHANGED;
use super::constants::PERM_AI_MANAGE_PROVIDERS;
use super::constants::PERM_AI_READ_USAGE;
use super::gating::emit_gating_change;
use super::gating::validate_gate_features;
use super::helpers::caller;
use super::helpers::caller_company_uuid;
use super::helpers::created;
use super::helpers::emit_audit;
use super::helpers::err;
use super::helpers::in_company_scope_uuid;
use super::helpers::ok;
use super::helpers::paginate;
use super::helpers::parse_uuid;
use super::helpers::require_permission;
use super::helpers::require_record_in_company_scope_uuid;
use super::helpers::require_tenant;
use super::inputs::CreateCompanyPolicyInput;
use super::inputs::CreateUserPolicyInput;
use super::inputs::UpdateCompanyPolicyInput;
use super::inputs::UpdateUserPolicyInput;
use super::state::AiState;
use super::types::AiCompanyPolicy;
use super::types::AiUsageRecord;
use super::types::AiUsageSummary;
use super::types::AiUserMcpServer;
use super::types::AiUserPolicy;
use super::validation::require_company_scope;
use super::validation::validate_cost_cap;
use super::validation::validate_policy_features;
use super::validation::validate_policy_models;
use super::validation::validate_token_cap;

// Usage handlers.

pub(crate) fn usage_matches(record: &AiUsageRecord, params: &PaginationParams) -> bool {
    if let Some(provider_id) = params.filters.get("provider_id") {
        if record.provider_id.to_string() != *provider_id {
            return false;
        }
    }
    if let Some(actor) = params.filters.get("actor") {
        if record.actor != *actor {
            return false;
        }
    }
    if let Some(use_case) = params.filters.get("use_case") {
        if record.use_case != *use_case {
            return false;
        }
    }
    if let Some(status) = params.filters.get("status") {
        if record.status != *status {
            return false;
        }
    }
    if let Some(search) = &params.search {
        if !search.trim().is_empty() {
            let haystack = format!(
                "{} {} {} {}",
                record.actor, record.use_case, record.model, record.provider_kind
            )
            .to_lowercase();
            if !haystack.contains(&search.trim().to_lowercase()) {
                return false;
            }
        }
    }
    true
}

pub(crate) fn summarize_usage(records: &[AiUsageRecord]) -> AiUsageSummary {
    let mut summary = AiUsageSummary {
        calls: records.len() as u64,
        successful_calls: 0,
        failed_calls: 0,
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        estimated_cost_usd: None,
    };
    let mut cost = 0.0f64;
    let mut has_cost = false;
    for record in records {
        match record.status.as_str() {
            "success" => summary.successful_calls += 1,
            _ => summary.failed_calls += 1,
        }
        summary.prompt_tokens += record.prompt_tokens.unwrap_or(0) as u64;
        summary.completion_tokens += record.completion_tokens.unwrap_or(0) as u64;
        summary.total_tokens += record.total_tokens.unwrap_or(0) as u64;
        if let Some(value) = record.estimated_cost_usd {
            cost += value;
            has_cost = true;
        }
    }
    if has_cost {
        summary.estimated_cost_usd = Some((cost * 1_000_000.0).round() / 1_000_000.0);
    }
    summary
}

pub(crate) async fn list_usage(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_READ_USAGE)?;
        let params = query.into_inner();
        let mut items: Vec<AiUsageRecord> = state
            .usage
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                (scope.is_none() || in_company_scope_uuid(&record.company_id, &scope))
                    && usage_matches(record, &params)
            })
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

pub(crate) async fn get_usage(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiUsageRecord> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_READ_USAGE)?;
        let id = parse_uuid(&path.into_inner(), "usage record")?;
        let record = state
            .usage
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("AI usage record not found: {id}")))?;
        if scope.is_some() {
            require_record_in_company_scope_uuid(&record.company_id, &scope)?;
        }
        Ok(record)
    })();
    match result {
        Ok(record) => ok(record),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn usage_summary(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiUsageSummary> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_READ_USAGE)?;
        let params = query.into_inner();
        let records: Vec<AiUsageRecord> = state
            .usage
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                (scope.is_none() || in_company_scope_uuid(&record.company_id, &scope))
                    && usage_matches(record, &params)
            })
            .collect();
        Ok(summarize_usage(&records))
    })();
    match result {
        Ok(summary) => ok(summary),
        Err(error) => err(error, debug),
    }
}

// Access allowlist handlers.
//
// Admin endpoints (`/access/...`) require `ai:manage_providers`.
// A caller who belongs to a company may only manage policies of
// that company (`require_company_scope`); a caller with no
// resolvable company is a platform admin. Personal endpoints
// (`/my-key`, `/my-mcp`) derive the owner from the `X-User-Id`
// header only: a caller can never see or touch another user's key
// or servers, and keys / tokens are never returned, only metadata.

/// Find a company policy by its policy id (the store is keyed by
/// `company_id`, so this scans).
pub(crate) fn find_company_policy(state: &AiState, id: Uuid) -> Option<AiCompanyPolicy> {
    state
        .company_policies
        .iter()
        .map(|entry| entry.value().clone())
        .find(|policy| policy.id == id)
}

/// Find a user policy by its policy id (the store is keyed by
/// `user_id`, so this scans).
pub(crate) fn find_user_policy(state: &AiState, id: Uuid) -> Option<AiUserPolicy> {
    state
        .user_policies
        .iter()
        .map(|entry| entry.value().clone())
        .find(|policy| policy.id == id)
}

/// Load a personal MCP server the caller owns. A foreign or missing
/// id is the same 404: existence is never leaked across users.
pub(crate) fn load_own_mcp_server(
    state: &AiState,
    user_id: Uuid,
    id: Uuid,
) -> UnifiedResult<AiUserMcpServer> {
    match state.personal_mcp_servers.get(&id) {
        Some(entry) if entry.user_id == user_id => Ok(entry.value().clone()),
        _ => Err(UnifiedError::not_found(format!(
            "Personal MCP server not found: {id}"
        ))),
    }
}

pub(crate) async fn list_company_policies(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = caller_company_uuid(&_tenant, &req)?;
        let params = query.into_inner();
        let company_filter = params.filters.get("company_id").cloned();
        let enabled_filter = params.filters.get("enabled").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<AiCompanyPolicy> = state
            .company_policies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|policy| {
                if let Some(company) = caller_company {
                    if policy.company_id != company {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if policy.company_id.to_string() != *company_id {
                        return false;
                    }
                }
                if let Some(enabled) = &enabled_filter {
                    let expected = match enabled.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if policy.enabled != expected {
                        return false;
                    }
                }
                if !search.is_empty() && !policy.company_id.to_string().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["created_at", "company_id"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "company_id" => a.company_id.cmp(&b.company_id),
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

pub(crate) async fn create_company_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<CreateCompanyPolicyInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<AiCompanyPolicy> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = caller_company_uuid(&_tenant, &req)?;
        require_company_scope(caller_company, input.company_id)?;
        let allowed_models = validate_policy_models(&input.allowed_models)?;
        let allowed_features = validate_policy_features(&input.allowed_features)?;
        let monthly_token_cap = validate_token_cap(input.monthly_token_cap)?;
        let monthly_cost_cap_usd = validate_cost_cap(input.monthly_cost_cap_usd)?;
        let disabled_features = validate_gate_features(&input.disabled_features)?;
        if state.company_policies.contains_key(&input.company_id) {
            return Err(UnifiedError::conflict(format!(
                "A company access policy already exists for company {}",
                input.company_id
            )));
        }
        let now = Utc::now();
        let policy = AiCompanyPolicy {
            id: Uuid::now_v7(),
            company_id: input.company_id,
            allowed_models,
            allowed_features,
            monthly_token_cap,
            monthly_cost_cap_usd,
            enabled: input.enabled.unwrap_or(true),
            ai_enabled: input.ai_enabled.unwrap_or(true),
            disabled_features,
            created_at: now,
            updated_at: now,
        };
        state
            .company_policies
            .insert(policy.company_id, policy.clone());
        emit_audit(
            &ctx,
            EVENT_AI_ACCESS_POLICY_CHANGED,
            &caller.actor,
            &format!("ai-company-policy:{}", policy.id),
            json!({
                "id": policy.id.to_string(),
                "company_id": policy.company_id.to_string(),
                "enabled": policy.enabled,
            }),
        );
        Ok(policy)
    })();
    match result {
        Ok(policy) => created(policy),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_company_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiCompanyPolicy> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = caller_company_uuid(&_tenant, &req)?;
        let id = parse_uuid(&path.into_inner(), "company policy")?;
        let policy = find_company_policy(&state, id).ok_or_else(|| {
            UnifiedError::not_found(format!("Company access policy not found: {id}"))
        })?;
        require_company_scope(caller_company, policy.company_id)?;
        Ok(policy)
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_company_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCompanyPolicyInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<AiCompanyPolicy> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = caller_company_uuid(&_tenant, &req)?;
        let id = parse_uuid(&path.into_inner(), "company policy")?;
        let mut policy = find_company_policy(&state, id).ok_or_else(|| {
            UnifiedError::not_found(format!("Company access policy not found: {id}"))
        })?;
        require_company_scope(caller_company, policy.company_id)?;
        if let Some(models) = &input.allowed_models {
            policy.allowed_models = validate_policy_models(models)?;
        }
        if let Some(features) = &input.allowed_features {
            policy.allowed_features = validate_policy_features(features)?;
        }
        if let Some(cap) = input.monthly_token_cap {
            policy.monthly_token_cap = validate_token_cap(cap)?;
        }
        if let Some(cap) = input.monthly_cost_cap_usd {
            policy.monthly_cost_cap_usd = validate_cost_cap(cap)?;
        }
        if let Some(enabled) = input.enabled {
            policy.enabled = enabled;
        }
        if let Some(ai_enabled) = input.ai_enabled {
            if policy.ai_enabled != ai_enabled {
                emit_gating_change(
                    &ctx,
                    &caller.actor,
                    "company",
                    &policy.company_id.to_string(),
                    "ai_enabled",
                    json!(policy.ai_enabled),
                    json!(ai_enabled),
                );
                policy.ai_enabled = ai_enabled;
            }
        }
        if let Some(disabled) = input.disabled_features {
            let validated = validate_gate_features(&disabled)?;
            if policy.disabled_features != validated {
                emit_gating_change(
                    &ctx,
                    &caller.actor,
                    "company",
                    &policy.company_id.to_string(),
                    "disabled_features",
                    json!(policy.disabled_features),
                    json!(validated),
                );
                policy.disabled_features = validated;
            }
        }
        policy.updated_at = Utc::now();
        state
            .company_policies
            .insert(policy.company_id, policy.clone());
        emit_audit(
            &ctx,
            EVENT_AI_ACCESS_POLICY_CHANGED,
            &caller.actor,
            &format!("ai-company-policy:{}", policy.id),
            json!({
                "id": policy.id.to_string(),
                "company_id": policy.company_id.to_string(),
                "enabled": policy.enabled,
            }),
        );
        Ok(policy)
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_company_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = caller_company_uuid(&_tenant, &req)?;
        let id = parse_uuid(&path.into_inner(), "company policy")?;
        let policy = find_company_policy(&state, id).ok_or_else(|| {
            UnifiedError::not_found(format!("Company access policy not found: {id}"))
        })?;
        require_company_scope(caller_company, policy.company_id)?;
        state.company_policies.remove(&policy.company_id);
        emit_audit(
            &ctx,
            EVENT_AI_ACCESS_POLICY_CHANGED,
            &caller.actor,
            &format!("ai-company-policy:{}", policy.id),
            json!({
                "id": policy.id.to_string(),
                "company_id": policy.company_id.to_string(),
                "enabled": false,
                "deleted": true,
            }),
        );
        Ok(json!({ "deleted": true, "policy_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_user_policies(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = caller_company_uuid(&_tenant, &req)?;
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let enabled_filter = params.filters.get("enabled").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<AiUserPolicy> = state
            .user_policies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|policy| {
                if let Some(company) = caller_company {
                    if policy.company_id != company {
                        return false;
                    }
                }
                if let Some(user_id) = &user_filter {
                    if policy.user_id.to_string() != *user_id {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if policy.company_id.to_string() != *company_id {
                        return false;
                    }
                }
                if let Some(enabled) = &enabled_filter {
                    let expected = match enabled.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if policy.enabled != expected {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !policy.user_id.to_string().contains(&search)
                    && !policy.company_id.to_string().contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["created_at", "user_id", "company_id"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "user_id" => a.user_id.cmp(&b.user_id),
                "company_id" => a.company_id.cmp(&b.company_id),
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

pub(crate) async fn create_user_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<CreateUserPolicyInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<AiUserPolicy> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = caller_company_uuid(&_tenant, &req)?;
        // A company admin limits only their own company's users.
        require_company_scope(caller_company, input.company_id)?;
        let allowed_models = validate_policy_models(&input.allowed_models)?;
        let allowed_features = validate_policy_features(&input.allowed_features)?;
        let monthly_token_cap = validate_token_cap(input.monthly_token_cap)?;
        let monthly_cost_cap_usd = validate_cost_cap(input.monthly_cost_cap_usd)?;
        let disabled_features = validate_gate_features(&input.disabled_features)?;
        if state.user_policies.contains_key(&input.user_id) {
            return Err(UnifiedError::conflict(format!(
                "A user access policy already exists for user {}",
                input.user_id
            )));
        }
        let now = Utc::now();
        let policy = AiUserPolicy {
            id: Uuid::now_v7(),
            user_id: input.user_id,
            company_id: input.company_id,
            allowed_models,
            allowed_features,
            monthly_token_cap,
            monthly_cost_cap_usd,
            enabled: input.enabled.unwrap_or(true),
            ai_enabled: input.ai_enabled.unwrap_or(true),
            disabled_features,
            created_at: now,
            updated_at: now,
        };
        state.user_policies.insert(policy.user_id, policy.clone());
        emit_audit(
            &ctx,
            EVENT_AI_ACCESS_POLICY_CHANGED,
            &caller.actor,
            &format!("ai-user-policy:{}", policy.id),
            json!({
                "id": policy.id.to_string(),
                "user_id": policy.user_id.to_string(),
                "company_id": policy.company_id.to_string(),
                "enabled": policy.enabled,
            }),
        );
        Ok(policy)
    })();
    match result {
        Ok(policy) => created(policy),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_user_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiUserPolicy> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = caller_company_uuid(&_tenant, &req)?;
        let id = parse_uuid(&path.into_inner(), "user policy")?;
        let policy = find_user_policy(&state, id).ok_or_else(|| {
            UnifiedError::not_found(format!("User access policy not found: {id}"))
        })?;
        require_company_scope(caller_company, policy.company_id)?;
        Ok(policy)
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_user_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateUserPolicyInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<AiUserPolicy> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = caller_company_uuid(&_tenant, &req)?;
        let id = parse_uuid(&path.into_inner(), "user policy")?;
        let mut policy = find_user_policy(&state, id).ok_or_else(|| {
            UnifiedError::not_found(format!("User access policy not found: {id}"))
        })?;
        require_company_scope(caller_company, policy.company_id)?;
        if let Some(models) = &input.allowed_models {
            policy.allowed_models = validate_policy_models(models)?;
        }
        if let Some(features) = &input.allowed_features {
            policy.allowed_features = validate_policy_features(features)?;
        }
        if let Some(cap) = input.monthly_token_cap {
            policy.monthly_token_cap = validate_token_cap(cap)?;
        }
        if let Some(cap) = input.monthly_cost_cap_usd {
            policy.monthly_cost_cap_usd = validate_cost_cap(cap)?;
        }
        if let Some(enabled) = input.enabled {
            policy.enabled = enabled;
        }
        if let Some(ai_enabled) = input.ai_enabled {
            if policy.ai_enabled != ai_enabled {
                emit_gating_change(
                    &ctx,
                    &caller.actor,
                    "user",
                    &policy.user_id.to_string(),
                    "ai_enabled",
                    json!(policy.ai_enabled),
                    json!(ai_enabled),
                );
                policy.ai_enabled = ai_enabled;
            }
        }
        if let Some(disabled) = input.disabled_features {
            let validated = validate_gate_features(&disabled)?;
            if policy.disabled_features != validated {
                emit_gating_change(
                    &ctx,
                    &caller.actor,
                    "user",
                    &policy.user_id.to_string(),
                    "disabled_features",
                    json!(policy.disabled_features),
                    json!(validated),
                );
                policy.disabled_features = validated;
            }
        }
        policy.updated_at = Utc::now();
        state.user_policies.insert(policy.user_id, policy.clone());
        emit_audit(
            &ctx,
            EVENT_AI_ACCESS_POLICY_CHANGED,
            &caller.actor,
            &format!("ai-user-policy:{}", policy.id),
            json!({
                "id": policy.id.to_string(),
                "user_id": policy.user_id.to_string(),
                "company_id": policy.company_id.to_string(),
                "enabled": policy.enabled,
            }),
        );
        Ok(policy)
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_user_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = caller_company_uuid(&_tenant, &req)?;
        let id = parse_uuid(&path.into_inner(), "user policy")?;
        let policy = find_user_policy(&state, id).ok_or_else(|| {
            UnifiedError::not_found(format!("User access policy not found: {id}"))
        })?;
        require_company_scope(caller_company, policy.company_id)?;
        state.user_policies.remove(&policy.user_id);
        emit_audit(
            &ctx,
            EVENT_AI_ACCESS_POLICY_CHANGED,
            &caller.actor,
            &format!("ai-user-policy:{}", policy.id),
            json!({
                "id": policy.id.to_string(),
                "user_id": policy.user_id.to_string(),
                "company_id": policy.company_id.to_string(),
                "enabled": false,
                "deleted": true,
            }),
        );
        Ok(json!({ "deleted": true, "policy_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
