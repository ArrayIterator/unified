// Contracts - Contract handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Digest;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// Contract handlers

pub(crate) async fn list_contracts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let template_filter = params.filters.get("template_id").cloned();
        let counterparty_filter = params.filters.get("counterparty_email").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let now = Utc::now();
        let mut items: Vec<Value> = state
            .contracts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|contract| {
                if let Some(status) = &status_filter {
                    if effective_status(contract, now).as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(template_id) = &template_filter {
                    if contract.template_id.map(|id| id.to_string()) != Some(template_id.clone()) {
                        return false;
                    }
                }
                if let Some(email) = &counterparty_filter {
                    if contract.counterparty_email.to_lowercase() != email.to_lowercase() {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {} {}",
                        contract.title,
                        contract.company_party,
                        contract.counterparty_name,
                        contract.counterparty_email
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .map(|contract| contract_view(&state, &contract))
            .collect();
        let sort = params.allowed_sort(&["title", "value", "ends_at", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a
                    .get("title")
                    .and_then(Value::as_str)
                    .cmp(&b.get("title").and_then(Value::as_str)),
                "value" => a
                    .get("value")
                    .and_then(Value::as_f64)
                    .partial_cmp(&b.get("value").and_then(Value::as_f64))
                    .unwrap_or(std::cmp::Ordering::Equal),
                "ends_at" => a
                    .get("ends_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("ends_at").and_then(Value::as_str)),
                _ => a
                    .get("created_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("created_at").and_then(Value::as_str)),
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

pub(crate) async fn create_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    body: web::Json<CreateContractRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let input = body.into_inner();
        if let Some(template_id) = input.template_id {
            if !state.templates.contains_key(&template_id) {
                return Err(UnifiedError::not_found(format!(
                    "Template not found: {template_id}"
                )));
            }
        }
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let company_party = require_text(&input.company_party, "company_party", MAX_NAME_LEN)?;
        let counterparty_name =
            require_text(&input.counterparty_name, "counterparty_name", MAX_NAME_LEN)?;
        let counterparty_email = require_text(
            &input.counterparty_email,
            "counterparty_email",
            MAX_EMAIL_LEN,
        )?;
        if !valid_email(&counterparty_email) {
            return Err(UnifiedError::bad_request(
                "counterparty_email must be a valid email address",
            ));
        }
        let terms = require_text(&input.terms, "terms", MAX_TERMS_LEN)?;
        let value = valid_price(input.value)?;
        let currency = valid_currency(input.currency.as_deref().unwrap_or("USD"))?;
        if let (Some(starts_at), Some(ends_at)) = (input.starts_at, input.ends_at) {
            if ends_at <= starts_at {
                return Err(UnifiedError::bad_request(
                    "ends_at must be after starts_at",
                ));
            }
        }
        let alert_days = match input.alert_days {
            Some(days) => validate_alert_days(&days)?,
            None => state.default_alert_days(),
        };
        let now = Utc::now();
        let contract = Contract {
            id: Uuid::now_v7(),
            template_id: input.template_id,
            title,
            company_party,
            counterparty_name,
            counterparty_email,
            terms,
            value,
            currency,
            signing_order: input.signing_order.unwrap_or_default(),
            status: ContractStatus::Draft,
            starts_at: input.starts_at,
            ends_at: input.ends_at,
            signed_at: None,
            content_hash: None,
            void_reason: None,
            terminate_reason: None,
            renewed_from_id: None,
            alert_days,
            expiry_alert_status: "not_scheduled".to_string(),
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.contracts.insert(contract.id, contract.clone());
        record_audit(
            &state,
            contract.id,
            None,
            &caller.actor,
            "created",
            Some(&req),
            None,
        );
        state.emit(
            "contracts.created",
            json!({
                "contract_id": contract.id.to_string(),
                "title": contract.title,
                "status": "draft",
                "value": contract.value,
                "currency": contract.currency,
                "created_by": caller.actor,
            }),
        );
        Ok(contract_view(&state, &contract))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_VIEW)?;
        let contract = load_contract(&state, &path.into_inner())?;
        Ok(contract_view(&state, &contract))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// A sealed contract (signed or later) cannot be edited: the
/// content hash proves the sealed content. The audited escape
/// hatch is voiding the contract and renewing it into a new
/// draft - voided contracts themselves stay immutable so the
/// audit trail can never be rewritten.
pub(crate) async fn update_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateContractRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let mut contract = load_contract(&state, &path.into_inner())?;
        if contract.status != ContractStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Contract in status \"{}\" cannot be edited: it is sealed. Void it and renew it into a new draft to change the terms.",
                contract.status.as_str(),
            )));
        }
        let input = body.into_inner();
        if let Some(title) = &input.title {
            contract.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(company_party) = &input.company_party {
            contract.company_party =
                require_text(company_party, "company_party", MAX_NAME_LEN)?;
        }
        if let Some(counterparty_name) = &input.counterparty_name {
            contract.counterparty_name =
                require_text(counterparty_name, "counterparty_name", MAX_NAME_LEN)?;
        }
        if let Some(counterparty_email) = &input.counterparty_email {
            let email = require_text(counterparty_email, "counterparty_email", MAX_EMAIL_LEN)?;
            if !valid_email(&email) {
                return Err(UnifiedError::bad_request(
                    "counterparty_email must be a valid email address",
                ));
            }
            contract.counterparty_email = email;
        }
        if let Some(terms) = &input.terms {
            contract.terms = require_text(terms, "terms", MAX_TERMS_LEN)?;
        }
        if let Some(value) = input.value {
            contract.value = valid_price(value)?;
        }
        if let Some(currency) = &input.currency {
            contract.currency = valid_currency(currency)?;
        }
        if let Some(signing_order) = input.signing_order {
            contract.signing_order = signing_order;
        }
        if let Some(starts_at) = input.starts_at {
            contract.starts_at = starts_at;
        }
        if let Some(ends_at) = input.ends_at {
            contract.ends_at = ends_at;
        }
        if let (Some(starts_at), Some(ends_at)) = (contract.starts_at, contract.ends_at) {
            if ends_at <= starts_at {
                return Err(UnifiedError::bad_request(
                    "ends_at must be after starts_at",
                ));
            }
        }
        if let Some(alert_days) = &input.alert_days {
            contract.alert_days = validate_alert_days(alert_days)?;
        }
        contract.updated_at = Utc::now();
        state.contracts.insert(contract.id, contract.clone());
        record_audit(
            &state,
            contract.id,
            None,
            &caller.actor,
            "updated",
            Some(&req),
            None,
        );
        Ok(contract_view(&state, &contract))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let contract = load_contract(&state, &path.into_inner())?;
        if contract.status != ContractStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Only a draft contract can be deleted (current status \"{}\"). Void it instead to keep the audit trail.",
                contract.status.as_str(),
            )));
        }
        for signer in signers_of(&state, contract.id) {
            state.signers.remove(&signer.id);
        }
        state
            .audit_trail
            .retain(|_, entry| entry.contract_id != contract.id);
        state.contracts.remove(&contract.id);
        state.emit(
            "contracts.deleted",
            json!({
                "contract_id": contract.id.to_string(),
                "title": contract.title,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "contract_id": contract.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn send_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let mut contract = load_contract(&state, &path.into_inner())?;
        transition(&contract, ContractStatus::Sent, "send")?;
        let signers = signers_of(&state, contract.id);
        if signers.is_empty() {
            return Err(UnifiedError::bad_request(
                "Add at least one signer before sending a contract",
            ));
        }
        if signers
            .iter()
            .any(|signer| signer.status == SignerStatus::Declined)
        {
            return Err(UnifiedError::bad_request(
                "A signer declined this contract: remove or replace them before sending",
            ));
        }
        contract.status = ContractStatus::Sent;
        contract.updated_at = Utc::now();
        state.contracts.insert(contract.id, contract.clone());
        record_audit(
            &state,
            contract.id,
            None,
            &caller.actor,
            "sent",
            Some(&req),
            Some(format!("{} signer(s)", signers.len())),
        );
        state.emit(
            "contracts.sent",
            json!({
                "contract_id": contract.id.to_string(),
                "title": contract.title,
                "signers": signers.len(),
                "sent_by": caller.actor,
            }),
        );
        Ok(contract_view(&state, &contract))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn activate_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let mut contract = load_contract(&state, &path.into_inner())?;
        transition(&contract, ContractStatus::Active, "activate")?;
        // An already-ended contract must not become active: it
        // would read as expired at once. Expiry alerts are
        // scheduled only here, on activation - never on send.
        reject_activation_if_ended(&contract, Utc::now())?;
        contract.status = ContractStatus::Active;
        contract.updated_at = Utc::now();
        schedule_expiry_alerts(&state, &mut contract);
        state.contracts.insert(contract.id, contract.clone());
        record_audit(
            &state,
            contract.id,
            None,
            &caller.actor,
            "activated",
            Some(&req),
            None,
        );
        state.emit(
            "contracts.activated",
            json!({
                "contract_id": contract.id.to_string(),
                "title": contract.title,
                "value": contract.value,
                "currency": contract.currency,
                "activated_by": caller.actor,
            }),
        );
        // Billing linkage: the billing module may invoice against
        // an active contract. No invoice is created here.
        state.emit(
            "billing.contract_activated",
            json!({
                "contract_id": contract.id.to_string(),
                "value": contract.value,
                "currency": contract.currency,
                "counterparty_name": contract.counterparty_name,
                "counterparty_email": contract.counterparty_email,
                "starts_at": contract.starts_at.map(|value| value.to_rfc3339()),
                "ends_at": contract.ends_at.map(|value| value.to_rfc3339()),
            }),
        );
        Ok(contract_view(&state, &contract))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn void_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<VoidRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let mut contract = load_contract(&state, &path.into_inner())?;
        transition(&contract, ContractStatus::Voided, "void")?;
        let reason = require_text(&body.reason, "reason", MAX_REASON_LEN)?;
        contract.status = ContractStatus::Voided;
        contract.void_reason = Some(reason.clone());
        contract.updated_at = Utc::now();
        state.contracts.insert(contract.id, contract.clone());
        record_audit(
            &state,
            contract.id,
            None,
            &caller.actor,
            "voided",
            Some(&req),
            Some(reason),
        );
        state.emit(
            "contracts.voided",
            json!({
                "contract_id": contract.id.to_string(),
                "title": contract.title,
                "voided_by": caller.actor,
            }),
        );
        Ok(contract_view(&state, &contract))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn terminate_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TerminateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let mut contract = load_contract(&state, &path.into_inner())?;
        transition(&contract, ContractStatus::Terminated, "terminate")?;
        let reason = require_text(&body.reason, "reason", MAX_REASON_LEN)?;
        contract.status = ContractStatus::Terminated;
        contract.terminate_reason = Some(reason.clone());
        contract.updated_at = Utc::now();
        state.contracts.insert(contract.id, contract.clone());
        record_audit(
            &state,
            contract.id,
            None,
            &caller.actor,
            "terminated",
            Some(&req),
            Some(reason),
        );
        state.emit(
            "contracts.terminated",
            json!({
                "contract_id": contract.id.to_string(),
                "title": contract.title,
                "terminated_by": caller.actor,
            }),
        );
        Ok(contract_view(&state, &contract))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn check_expiry_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let mut contract = load_contract(&state, &path.into_inner())?;
        let expired = materialise_expiry(&state, &mut contract);
        if expired {
            state.contracts.insert(contract.id, contract.clone());
        }
        Ok(json!({
            "contract_id": contract.id.to_string(),
            "status": effective_status(&contract, Utc::now()).as_str(),
            "expired_by_check": expired,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn renew_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RenewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let source = load_contract(&state, &path.into_inner())?;
        if matches!(
            source.status,
            ContractStatus::Draft | ContractStatus::Sent | ContractStatus::PartiallySigned
        ) {
            return Err(UnifiedError::conflict(
                "Only a signed, active, expired, terminated, or voided contract can be renewed",
            ));
        }
        let input = body.into_inner();
        let starts_at = input.starts_at.or(source.starts_at);
        let ends_at = input.ends_at.or(source.ends_at);
        if let (Some(starts_at), Some(ends_at)) = (starts_at, ends_at) {
            if ends_at <= starts_at {
                return Err(UnifiedError::bad_request(
                    "ends_at must be after starts_at",
                ));
            }
        }
        let now = Utc::now();
        let renewed = Contract {
            id: Uuid::now_v7(),
            template_id: source.template_id,
            title: source.title.clone(),
            company_party: source.company_party.clone(),
            counterparty_name: source.counterparty_name.clone(),
            counterparty_email: source.counterparty_email.clone(),
            terms: source.terms.clone(),
            value: source.value,
            currency: source.currency.clone(),
            signing_order: source.signing_order,
            status: ContractStatus::Draft,
            starts_at,
            ends_at,
            signed_at: None,
            content_hash: None,
            void_reason: None,
            terminate_reason: None,
            renewed_from_id: Some(source.id),
            alert_days: source.alert_days.clone(),
            expiry_alert_status: "not_scheduled".to_string(),
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.contracts.insert(renewed.id, renewed.clone());
        record_audit(
            &state,
            source.id,
            None,
            &caller.actor,
            "renewed",
            Some(&req),
            Some(format!("Renewed into draft contract {}", renewed.id)),
        );
        state.emit(
            "contracts.renewed",
            json!({
                "contract_id": source.id.to_string(),
                "renewed_contract_id": renewed.id.to_string(),
                "renewed_by": caller.actor,
            }),
        );
        state.emit(
            "contracts.created",
            json!({
                "contract_id": renewed.id.to_string(),
                "title": renewed.title,
                "status": "draft",
                "value": renewed.value,
                "currency": renewed.currency,
                "renewed_from_id": source.id.to_string(),
                "created_by": caller.actor,
            }),
        );
        Ok(contract_view(&state, &renewed))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_audit(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_VIEW)?;
        let contract = load_contract(&state, &path.into_inner())?;
        let params = query.into_inner();
        let action_filter = params.filters.get("action").cloned();
        let actor_filter = params.filters.get("actor").cloned();
        let mut items: Vec<Value> = state
            .audit_trail
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.contract_id == contract.id)
            .filter(|entry| {
                if let Some(action) = &action_filter {
                    if entry.action != *action {
                        return false;
                    }
                }
                if let Some(actor) = &actor_filter {
                    if entry.actor != *actor {
                        return false;
                    }
                }
                true
            })
            .map(|entry| json!(entry))
            .collect();
        items.sort_by(|a, b| {
            b.get("created_at")
                .and_then(Value::as_str)
                .cmp(&a.get("created_at").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
