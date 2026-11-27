// Contracts - Signer handlers.

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

// Signer handlers

pub(crate) async fn list_signers(
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
        let status_filter = params.filters.get("status").cloned();
        let items: Vec<Value> = signers_of(&state, contract.id)
            .into_iter()
            .filter(|signer| {
                if let Some(status) = &status_filter {
                    if signer.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                true
            })
            .map(|signer| signer_view(&signer))
            .collect();
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Shift every signer at or after insertion index `at` one step
/// down the order, so inserting at `at` never creates a duplicate
/// `order_index`. The caller persists the shifted signers.
pub(crate) fn shift_order_indexes(signers: &mut [ContractSigner], at: i64) {
    for signer in signers.iter_mut() {
        if signer.order_index >= at {
            signer.order_index += 1;
        }
    }
}

pub(crate) async fn add_signer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddSignerRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    // The raw token is returned exactly once, inside this response
    // under `token`; it is never stored, only its SHA-256 hash.
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let contract = load_contract(&state, &path.into_inner())?;
        if !matches!(
            contract.status,
            ContractStatus::Draft | ContractStatus::Sent
        ) {
            return Err(UnifiedError::conflict(format!(
                "Signers can only be added while a contract is draft or sent (current status \"{}\")",
                contract.status.as_str(),
            )));
        }
        let existing = signers_of(&state, contract.id);
        if existing.len() >= MAX_SIGNERS_PER_CONTRACT {
            return Err(UnifiedError::bad_request(format!(
                "At most {MAX_SIGNERS_PER_CONTRACT} signers per contract"
            )));
        }
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let email = require_text(&input.email, "email", MAX_EMAIL_LEN)?;
        if !valid_email(&email) {
            return Err(UnifiedError::bad_request(
                "email must be a valid email address",
            ));
        }
        if existing
            .iter()
            .any(|signer| signer.email.to_lowercase() == email.to_lowercase())
        {
            return Err(UnifiedError::conflict(format!(
                "Signer with email \"{email}\" already exists on this contract"
            )));
        }
        let order_index = match input.order_index {
            Some(index) => {
                if index < 0 || index > existing.len() as i64 {
                    return Err(UnifiedError::bad_request(format!(
                        "order_index must be between 0 and {}",
                        existing.len()
                    )));
                }
                index
            }
            None => existing.len() as i64,
        };
        let ttl_days = input.expires_in_days.unwrap_or(DEFAULT_TOKEN_TTL_DAYS);
        if !(1..=90).contains(&ttl_days) {
            return Err(UnifiedError::bad_request(
                "expires_in_days must be between 1 and 90",
            ));
        }
        let token = generate_token();
        let now = Utc::now();
        // Inserting at an occupied index shifts every signer at or
        // after it one step down, so `order_index` values stay
        // unique (an explicit index equal to the signer count, or
        // no index at all, shifts nobody).
        let mut shifted: Vec<ContractSigner> = existing
            .iter()
            .filter(|signer| signer.order_index >= order_index)
            .cloned()
            .collect();
        shift_order_indexes(&mut shifted, order_index);
        for mut signer in shifted {
            signer.updated_at = now;
            state.signers.insert(signer.id, signer);
        }
        let signer = ContractSigner {
            id: Uuid::now_v7(),
            contract_id: contract.id,
            name,
            email,
            order_index,
            status: SignerStatus::Pending,
            token_hash: hash_value(&token),
            token_expires_at: now + Duration::days(ttl_days),
            signed_at: None,
            declined_reason: None,
            viewed_at: None,
            created_at: now,
            updated_at: now,
        };
        state.signers.insert(signer.id, signer.clone());
        record_audit(
            &state,
            contract.id,
            Some(signer.id),
            &caller.actor,
            "signer_added",
            Some(&req),
            Some(format!("Signer {} <{}>", signer.name, signer.email)),
        );
        state.emit(
            "contracts.signer.added",
            json!({
                "contract_id": contract.id.to_string(),
                "signer_id": signer.id.to_string(),
                "email": signer.email,
                "order_index": signer.order_index,
            }),
        );
        let mut view = signer_view(&signer);
        if let Value::Object(map) = &mut view {
            map.insert("token".to_string(), Value::String(token));
        }
        Ok(view)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn remove_signer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let (contract_id, signer_id) = path.into_inner();
        let contract = load_contract(&state, &contract_id)?;
        let signer = load_signer(&state, contract.id, &signer_id)?;
        if contract.status != ContractStatus::Draft && contract.status != ContractStatus::Sent {
            return Err(UnifiedError::conflict(format!(
                "Signers can only be removed while a contract is draft or sent (current status \"{}\")",
                contract.status.as_str(),
            )));
        }
        if signer.status == SignerStatus::Signed {
            return Err(UnifiedError::conflict(
                "A signer who already signed cannot be removed; void the contract instead",
            ));
        }
        state.signers.remove(&signer.id);
        record_audit(
            &state,
            contract.id,
            Some(signer.id),
            &caller.actor,
            "signer_removed",
            Some(&req),
            Some(format!("Signer {} <{}>", signer.name, signer.email)),
        );
        state.emit(
            "contracts.signer.removed",
            json!({
                "contract_id": contract.id.to_string(),
                "signer_id": signer.id.to_string(),
                "email": signer.email,
            }),
        );
        Ok(json!({ "deleted": true, "signer_id": signer.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn resend_signer_token(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    // The fresh raw token is returned exactly once, inside this
    // response under `token`; the old token stops working because
    // only the new hash is stored.
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let (contract_id, signer_id) = path.into_inner();
        let contract = load_contract(&state, &contract_id)?;
        let mut signer = load_signer(&state, contract.id, &signer_id)?;
        if !matches!(
            contract.status,
            ContractStatus::Sent | ContractStatus::PartiallySigned
        ) {
            return Err(UnifiedError::conflict(format!(
                "Signing tokens can only be re-issued while a contract is sent or partially signed (current status \"{}\")",
                contract.status.as_str(),
            )));
        }
        if signer.status == SignerStatus::Signed || signer.status == SignerStatus::Declined {
            return Err(UnifiedError::conflict(format!(
                "Cannot re-issue a token for a signer who already {}",
                signer.status.as_str(),
            )));
        }
        let token = generate_token();
        let now = Utc::now();
        signer.token_hash = hash_value(&token);
        signer.token_expires_at = now + Duration::days(DEFAULT_TOKEN_TTL_DAYS);
        signer.updated_at = now;
        state.signers.insert(signer.id, signer.clone());
        record_audit(
            &state,
            contract.id,
            Some(signer.id),
            &caller.actor,
            "token_resent",
            Some(&req),
            Some(format!("New signing token issued to {}", signer.email)),
        );
        state.emit(
            "contracts.signer.token_resent",
            json!({
                "contract_id": contract.id.to_string(),
                "signer_id": signer.id.to_string(),
                "email": signer.email,
            }),
        );
        let mut view = signer_view(&signer);
        if let Value::Object(map) = &mut view {
            map.insert("token".to_string(), Value::String(token));
        }
        Ok(view)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer_at(order_index: i64) -> ContractSigner {
        ContractSigner {
            id: Uuid::new_v4(),
            contract_id: Uuid::new_v4(),
            name: "Signer".to_string(),
            email: "signer@example.com".to_string(),
            order_index,
            status: SignerStatus::Pending,
            token_hash: "hash".to_string(),
            token_expires_at: Utc::now() + Duration::days(30),
            signed_at: None,
            declined_reason: None,
            viewed_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn indexes(signers: &[ContractSigner]) -> Vec<i64> {
        signers.iter().map(|signer| signer.order_index).collect()
    }

    #[test]
    fn shift_order_indexes_moves_only_signers_at_or_after_insertion() {
        let mut signers = vec![signer_at(0), signer_at(1), signer_at(2)];
        shift_order_indexes(&mut signers, 1);
        assert_eq!(indexes(&signers), vec![0, 2, 3]);
    }

    #[test]
    fn shift_order_indexes_at_zero_moves_every_signer() {
        let mut signers = vec![signer_at(0), signer_at(1)];
        shift_order_indexes(&mut signers, 0);
        assert_eq!(indexes(&signers), vec![1, 2]);
    }

    #[test]
    fn shift_order_indexes_at_end_moves_nobody() {
        let mut signers = vec![signer_at(0), signer_at(1)];
        shift_order_indexes(&mut signers, 2);
        assert_eq!(indexes(&signers), vec![0, 1]);
    }

    #[test]
    fn shift_order_indexes_leaves_empty_list_unchanged() {
        let mut signers: Vec<ContractSigner> = vec![];
        shift_order_indexes(&mut signers, 0);
        assert!(signers.is_empty());
    }
}
