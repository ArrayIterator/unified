// Contracts - Token-gated signing links (no capability check - the token is the.

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

// Token-gated signing links (no capability check - the token is the
// credential; see the public-access note in the module header)

/// Resolve a raw signing token to its signer. Unknown, expired,
/// and revoked tokens all answer 404 with the same message, so
/// tokens cannot be probed apart.
pub(crate) fn load_signer_by_token(
    state: &ContractsState,
    token: &str,
) -> UnifiedResult<(Contract, ContractSigner)> {
    let token_hash = hash_value(token);
    let now = Utc::now();
    let signer = state
        .signers
        .iter()
        .map(|entry| entry.value().clone())
        .find(|entry| entry.token_hash == token_hash)
        .ok_or_else(|| UnifiedError::not_found("Signing link not found or expired"))?;
    if signer.token_expires_at <= now {
        return Err(UnifiedError::not_found("Signing link not found or expired"));
    }
    let contract = state
        .contracts
        .get(&signer.contract_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Signing link not found or expired"))?;
    Ok((contract, signer))
}

/// Sequential gate: a lower signer blocks the signer at
/// `order_index` only while still `Pending` or `Viewed`. A
/// `Declined` lower signer does not block (the contract stays
/// sealable by the remaining signers); a `Signed` one never
/// blocks.
pub(crate) fn sequential_blocked(signers: &[ContractSigner], order_index: i64) -> bool {
    signers.iter().any(|other| {
        other.order_index < order_index
            && matches!(
                other.status,
                SignerStatus::Pending | SignerStatus::Viewed
            )
    })
}

/// Sealing rule: every non-declined signer signed, with at least
/// one non-declined signer. A declined signer never blocks
/// sealing, so a declined signer cannot leave the contract
/// unsealable forever.
pub(crate) fn all_non_declined_signed(signers: &[ContractSigner]) -> bool {
    let remaining: Vec<&ContractSigner> = signers
        .iter()
        .filter(|signer| signer.status != SignerStatus::Declined)
        .collect();
    !remaining.is_empty()
        && remaining
            .iter()
            .all(|signer| signer.status == SignerStatus::Signed)
}

pub(crate) async fn get_signing_link(
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = false;
    let result: UnifiedResult<Value> = (|| {
        let token = path.into_inner();
        if token.len() != SIGNING_TOKEN_LEN
            || !token.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Err(UnifiedError::not_found("Signing link not found or expired"));
        }
        let (contract, mut signer) = load_signer_by_token(&state, &token)?;
        if !matches!(
            contract.status,
            ContractStatus::Sent | ContractStatus::PartiallySigned
        ) {
            return Err(UnifiedError::not_found("Signing link not found or expired"));
        }
        // A first view is audited and moves the signer to `viewed`.
        if signer.status == SignerStatus::Pending {
            let now = Utc::now();
            signer.status = SignerStatus::Viewed;
            signer.viewed_at = Some(now);
            signer.updated_at = now;
            state.signers.insert(signer.id, signer.clone());
            record_audit(
                &state,
                contract.id,
                Some(signer.id),
                &signer.email,
                "viewed",
                Some(&req),
                None,
            );
        }
        let signers = signers_of(&state, contract.id);
        Ok(json!({
            "contract": contract_view(&state, &contract),
            "signer": signer_view(&signer),
            "signers": signers.iter().map(signer_view).collect::<Vec<Value>>(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn sign_via_link(
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SignRequest>,
) -> HttpResponse {
    let debug = false;
    let result: UnifiedResult<Value> = (|| {
        let token = path.into_inner();
        if token.len() != SIGNING_TOKEN_LEN
            || !token.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Err(UnifiedError::not_found("Signing link not found or expired"));
        }
        let (mut contract, mut signer) = load_signer_by_token(&state, &token)?;
        if !matches!(
            contract.status,
            ContractStatus::Sent | ContractStatus::PartiallySigned
        ) {
            return Err(UnifiedError::conflict(format!(
                "Contract in status \"{}\" cannot be signed",
                contract.status.as_str(),
            )));
        }
        if signer.status == SignerStatus::Signed {
            return Err(UnifiedError::conflict("This signing link was already used"));
        }
        if signer.status == SignerStatus::Declined {
            return Err(UnifiedError::conflict(
                "This signer declined the contract; the link cannot be used to sign",
            ));
        }
        if !body.confirm {
            return Err(UnifiedError::bad_request(
                "Signing requires \"confirm\": true - the signer must confirm they reviewed the terms",
            ));
        }
        // Sequential order: every lower order_index must have
        // signed already. A declined lower signer does not block.
        if contract.signing_order == SigningOrder::Sequential {
            let signers = signers_of(&state, contract.id);
            if sequential_blocked(&signers, signer.order_index) {
                return Err(UnifiedError::conflict(
                    "Sequential signing: an earlier signer has not signed yet",
                ));
            }
        }
        let now = Utc::now();
        signer.status = SignerStatus::Signed;
        signer.signed_at = Some(now);
        signer.updated_at = now;
        state.signers.insert(signer.id, signer.clone());
        record_audit(
            &state,
            contract.id,
            Some(signer.id),
            &signer.email,
            "signed",
            Some(&req),
            Some(format!("Signer {} <{}> signed", signer.name, signer.email)),
        );
        state.emit(
            "contracts.signer.signed",
            json!({
                "contract_id": contract.id.to_string(),
                "signer_id": signer.id.to_string(),
                "email": signer.email,
                "order_index": signer.order_index,
            }),
        );
        // Advance the contract: last signature seals it. A
        // declined signer does not block sealing (see
        // `all_non_declined_signed`).
        let signers = signers_of(&state, contract.id);
        let all_signed = all_non_declined_signed(&signers);
        if all_signed {
            // Route the sealing move through the status machine:
            // `transition` allows Sent -> Signed for the sealing
            // signature (single signer, or every other signer
            // declined) as well as PartiallySigned -> Signed.
            contract.status = transition(&contract, ContractStatus::Signed, "seal")?;
            contract.signed_at = Some(now);
            contract.content_hash = Some(seal_content_hash(&contract));
            contract.updated_at = now;
            state.contracts.insert(contract.id, contract.clone());
            record_audit(
                &state,
                contract.id,
                Some(signer.id),
                &signer.email,
                "sealed",
                Some(&req),
                Some(format!(
                    "All signers signed; content sealed with hash {}",
                    contract.content_hash.clone().unwrap_or_default()
                )),
            );
            state.emit(
                "contracts.signed",
                json!({
                    "contract_id": contract.id.to_string(),
                    "title": contract.title,
                    "content_hash": contract.content_hash,
                    "signed_at": now.to_rfc3339(),
                }),
            );
            state.notify(
                "contract.signed",
                &contract.created_by,
                &format!("Contract fully signed: {}", contract.title),
                &format!(
                    "All signers signed contract \"{}\". It is sealed and ready to activate.",
                    contract.title
                ),
                Some(format!("/api/v1/contracts/{}", contract.id)),
            );
        } else {
            contract.status = ContractStatus::PartiallySigned;
            contract.updated_at = now;
            state.contracts.insert(contract.id, contract.clone());
        }
        Ok(json!({
            "contract": contract_view(&state, &contract),
            "signer": signer_view(&signer),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn decline_via_link(
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DeclineRequest>,
) -> HttpResponse {
    let debug = false;
    let result: UnifiedResult<Value> = (|| {
        let token = path.into_inner();
        if token.len() != SIGNING_TOKEN_LEN
            || !token.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Err(UnifiedError::not_found("Signing link not found or expired"));
        }
        let (contract, mut signer) = load_signer_by_token(&state, &token)?;
        if !matches!(
            contract.status,
            ContractStatus::Sent | ContractStatus::PartiallySigned
        ) {
            return Err(UnifiedError::conflict(format!(
                "Contract in status \"{}\" cannot be declined",
                contract.status.as_str(),
            )));
        }
        if signer.status == SignerStatus::Signed {
            return Err(UnifiedError::conflict("This signer already signed the contract"));
        }
        if signer.status == SignerStatus::Declined {
            return Err(UnifiedError::conflict("This signer already declined the contract"));
        }
        let reason = require_text(&body.reason, "reason", MAX_REASON_LEN)?;
        let now = Utc::now();
        signer.status = SignerStatus::Declined;
        signer.declined_reason = Some(reason.clone());
        signer.updated_at = now;
        state.signers.insert(signer.id, signer.clone());
        record_audit(
            &state,
            contract.id,
            Some(signer.id),
            &signer.email,
            "declined",
            Some(&req),
            Some(reason.clone()),
        );
        state.emit(
            "contracts.signer.declined",
            json!({
                "contract_id": contract.id.to_string(),
                "signer_id": signer.id.to_string(),
                "email": signer.email,
                "reason": reason,
            }),
        );
        state.notify(
            "contract.signer_declined",
            &contract.created_by,
            &format!("Signer declined: {}", contract.title),
            &format!(
                "Signer {} <{}> declined contract \"{}\": {}",
                signer.name, signer.email, contract.title, reason
            ),
            Some(format!("/api/v1/contracts/{}", contract.id)),
        );
        Ok(json!({
            "contract": contract_view(&state, &contract),
            "signer": signer_view(&signer),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer_at(order_index: i64, status: SignerStatus) -> ContractSigner {
        ContractSigner {
            id: Uuid::new_v4(),
            contract_id: Uuid::new_v4(),
            name: "Signer".to_string(),
            email: "signer@example.com".to_string(),
            order_index,
            status,
            token_hash: "hash".to_string(),
            token_expires_at: Utc::now() + Duration::days(30),
            signed_at: None,
            declined_reason: None,
            viewed_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn all_non_declined_signed_requires_every_remaining_signer_signed() {
        assert!(all_non_declined_signed(&[
            signer_at(0, SignerStatus::Signed),
            signer_at(1, SignerStatus::Signed),
        ]));
        assert!(!all_non_declined_signed(&[
            signer_at(0, SignerStatus::Signed),
            signer_at(1, SignerStatus::Pending),
        ]));
    }

    #[test]
    fn all_non_declined_signed_ignores_declined_signers() {
        // One signer declined, the other signed: the contract is
        // sealable instead of stuck unsealable forever.
        assert!(all_non_declined_signed(&[
            signer_at(0, SignerStatus::Declined),
            signer_at(1, SignerStatus::Signed),
        ]));
    }

    #[test]
    fn all_non_declined_signed_needs_at_least_one_signer() {
        assert!(!all_non_declined_signed(&[]));
        assert!(!all_non_declined_signed(&[
            signer_at(0, SignerStatus::Declined),
            signer_at(1, SignerStatus::Declined),
        ]));
    }

    #[test]
    fn sequential_gate_blocks_only_on_pending_or_viewed_lower_signers() {
        let pending_below = vec![signer_at(0, SignerStatus::Pending)];
        assert!(sequential_blocked(&pending_below, 1));
        let viewed_below = vec![signer_at(0, SignerStatus::Viewed)];
        assert!(sequential_blocked(&viewed_below, 1));
        let signed_below = vec![signer_at(0, SignerStatus::Signed)];
        assert!(!sequential_blocked(&signed_below, 1));
    }

    #[test]
    fn sequential_gate_ignores_declined_lower_signers() {
        let declined_below = vec![signer_at(0, SignerStatus::Declined)];
        assert!(!sequential_blocked(&declined_below, 1));
    }

    #[test]
    fn sequential_gate_ignores_signers_at_or_above_the_index() {
        let signers = vec![
            signer_at(1, SignerStatus::Pending),
            signer_at(2, SignerStatus::Pending),
        ];
        assert!(!sequential_blocked(&signers, 1));
    }
}
