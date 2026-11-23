//! ai module - HTTP handlers for gate reads and updates.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;

use super::constants::EVENT_AI_ACCESS_POLICY_CHANGED;
use super::constants::PERM_AI_MANAGE_PROVIDERS;
use super::gating::EffectiveGateQuery;
use super::gating::UpdateCompanyGateInput;
use super::gating::UpdateGlobalSettingsInput;
use super::gating::UpdateUserGateInput;
use super::gating::emit_gating_change;
use super::gating::evaluate_ai_gate;
use super::gating::resolve_caller_product;
use super::gating::resolve_user_company;
use super::gating::validate_gate_feature_name;
use super::gating::validate_gate_features;
use super::helpers::caller;
use super::helpers::caller_company_uuid;
use super::helpers::caller_user_id;
use super::helpers::emit_audit;
use super::helpers::err;
use super::helpers::ok;
use super::helpers::parse_uuid;
use super::helpers::require_permission;
use super::helpers::require_tenant;
use super::state::AiState;
use super::types::AiCompanyPolicy;
use super::types::AiGateDecision;
use super::types::AiGlobalSettings;
use super::types::AiUserPolicy;
use super::validation::require_company_scope;

/// The effective gate decision for a user / feature, with the
/// per-layer breakdown that explains which layer decides. Defaults
/// to the caller and the `complete` feature; `feature` accepts the
/// friendly aliases (`support`, `drafting`, `insights`, `chat`).
pub(crate) async fn get_effective_gate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    query: web::Query<EffectiveGateQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiGateDecision> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let params = query.into_inner();
        let target_user_id = match &params.user_id {
            Some(raw) => Some(parse_uuid(raw, "user")?),
            None => caller_user_id(&caller),
        };
        let feature = match &params.feature {
            Some(raw) => validate_gate_feature_name(raw)?,
            None => "complete".to_string(),
        };
        let product_id = match &params.product_id {
            Some(raw) => Some(parse_uuid(raw, "product")?),
            None => resolve_caller_product(&req),
        };
        // Company scope: a company admin inspects only their own
        // company. The target user's company comes from the users
        // module; the caller's own company from the header / hook.
        let caller_company = caller_company_uuid(&_tenant, &req)?;
        let caller_own_user_id = caller_user_id(&caller);
        let target_company = if target_user_id == caller_own_user_id {
            caller_company
        } else {
            match target_user_id {
                Some(target_user_id) => {
                    let resolved = resolve_user_company(&ctx, target_user_id);
                    match resolved {
                        Some(company_id) => {
                            require_company_scope(caller_company, company_id)?;
                            Some(company_id)
                        }
                        None => {
                            if caller_company.is_some() {
                                return Err(UnifiedError::forbidden(
                                    "Cannot verify the target user's company",
                                ));
                            }
                            None
                        }
                    }
                }
                None => caller_company,
            }
        };
        Ok(evaluate_ai_gate(
            &ctx,
            &state,
            target_user_id,
            target_company,
            &feature,
            product_id,
        ))
    })();
    match result {
        Ok(decision) => ok(decision),
        Err(error) => err(error, debug),
    }
}

/// Switch AI on/off for a company, or switch single features off
/// for it. The company must already have an access policy (create
/// one through `POST /access/companies` first): creating a policy
/// shell here would switch the allowlist on for the company, which
/// would deny every user without a user policy.
pub(crate) async fn patch_company_gate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCompanyGateInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<AiCompanyPolicy> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = caller_company_uuid(&_tenant, &req)?;
        let company_id = parse_uuid(&path.into_inner(), "company")?;
        require_company_scope(caller_company, company_id)?;
        let mut policy = state
            .company_policies
            .get(&company_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "No AI access policy exists for company {company_id}: create one through POST /access/companies first"
                ))
            })?;
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
                "ai_enabled": policy.ai_enabled,
            }),
        );
        Ok(policy)
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

/// Switch AI on/off for a user, or switch single features off for
/// them. The user must already have an access policy (create one
/// through `POST /access/users` first).
pub(crate) async fn patch_user_gate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateUserGateInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<AiUserPolicy> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = caller_company_uuid(&_tenant, &req)?;
        let user_id = parse_uuid(&path.into_inner(), "user")?;
        let mut policy = state
            .user_policies
            .get(&user_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "No AI access policy exists for user {user_id}: create one through POST /access/users first"
                ))
            })?;
        require_company_scope(caller_company, policy.company_id)?;
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
                "ai_enabled": policy.ai_enabled,
            }),
        );
        Ok(policy)
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

/// Read the global AI settings (kill-switch + global feature
/// switches).
pub(crate) async fn get_global_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiGlobalSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        Ok(state
            .global_settings
            .read()
            .expect("ai global settings lock poisoned")
            .clone())
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

/// Change the global AI settings. Turning `global_enabled` off is
/// the kill-switch: it denies every AI call immediately,
/// regardless of the other layers.
pub(crate) async fn patch_global_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<UpdateGlobalSettingsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<AiGlobalSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let mut settings = state
            .global_settings
            .read()
            .expect("ai global settings lock poisoned")
            .clone();
        if let Some(global_enabled) = input.global_enabled {
            if settings.global_enabled != global_enabled {
                emit_gating_change(
                    &ctx,
                    &caller.actor,
                    "settings",
                    "global",
                    "ai.global_enabled",
                    json!(settings.global_enabled),
                    json!(global_enabled),
                );
                settings.global_enabled = global_enabled;
            }
        }
        if let Some(disabled) = input.disabled_features {
            let validated = validate_gate_features(&disabled)?.unwrap_or_default();
            if settings.disabled_features != validated {
                emit_gating_change(
                    &ctx,
                    &caller.actor,
                    "settings",
                    "global",
                    "ai.features.disabled",
                    json!(settings.disabled_features),
                    json!(validated),
                );
                settings.disabled_features = validated;
            }
        }
        if let Some(open_mode) = input.open_mode {
            if settings.open_mode != open_mode {
                emit_gating_change(
                    &ctx,
                    &caller.actor,
                    "settings",
                    "global",
                    "ai.open_mode",
                    json!(settings.open_mode),
                    json!(open_mode),
                );
                settings.open_mode = open_mode;
            }
        }
        settings.updated_at = Utc::now();
        *state
            .global_settings
            .write()
            .expect("ai global settings lock poisoned") = settings.clone();
        Ok(settings)
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}
