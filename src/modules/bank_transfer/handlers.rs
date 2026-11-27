// bank_transfer - HTTP handlers.

use super::service::{
    caller_company, can_manage, require_admin, require_tenant, require_verification_owner,
    verification_visible_to_caller,
};
use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

// Handlers: status, settings.

pub(crate) async fn get_status(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BankTransferState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _ = state;
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let accounts_total = accounts_store().len() as u64;
        let accounts_active = accounts_store()
            .iter()
            .filter(|entry| entry.value().active)
            .count() as u64;
        let mut by_status: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        for entry in verifications_store().iter() {
            *by_status
                .entry(entry.value().status.clone())
                .or_insert(0) += 1;
        }
        let pending = by_status.get(STATUS_AWAITING_PROOF).copied().unwrap_or(0)
            + by_status.get(STATUS_SUBMITTED).copied().unwrap_or(0);
        Ok(json!({
            "module": "bank_transfer",
            "accounts_total": accounts_total,
            "accounts_active": accounts_active,
            "verifications_by_status": by_status,
            "verifications_pending": pending,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BankTransferSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        Ok(settings())
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<UpdateBankTransferSettings>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BankTransferSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        // Operator-only: settings change the amount tolerance and
        // the proof requirements for every transfer.
        require_admin(&ctx, &req)?;
        let input = body.into_inner();
        if let Some(tolerance) = input.amount_tolerance_minor {
            if tolerance < 0 {
                return Err(UnifiedError::bad_request(
                    "amount_tolerance_minor must be zero or greater",
                ));
            }
        }
        if let Some(days) = input.proof_expiry_days {
            if days == 0 || days > 365 {
                return Err(UnifiedError::bad_request(
                    "proof_expiry_days must be between 1 and 365",
                ));
            }
        }
        let mut current = settings_store()
            .write()
            .expect("settings lock poisoned");
        if let Some(tolerance) = input.amount_tolerance_minor {
            current.amount_tolerance_minor = tolerance;
        }
        if let Some(days) = input.proof_expiry_days {
            current.proof_expiry_days = days;
        }
        if let Some(required) = input.require_reference_number {
            current.require_reference_number = required;
        }
        Ok(current.clone())
    })();
    json::respond(result, debug)
}

// Handlers: bank accounts.

pub(crate) async fn list_accounts(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<BankAccountView>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let pagination = params.normalized();
        let mut items: Vec<BankAccountView> = accounts_store()
            .iter()
            .map(|entry| entry.value().view())
            .filter(|account| {
                matches_search(
                    &format!("{} {}", account.bank_name, account.account_holder),
                    &params,
                ) && params.filters.get("currency").is_none_or(|value| {
                    account.currency.eq_ignore_ascii_case(value)
                }) && params.filters.get("active").is_none_or(|value| {
                    account.active == (*value == "true" || *value == "1")
                }) && params.filters.get("bank_name").is_none_or(|value| {
                    account.bank_name == *value
                })
            })
            .collect();
        let sort = params.allowed_sort(&["bank_name", "created_at"], "created_at");
        match sort.as_str() {
            "bank_name" => items.sort_by(|a, b| a.bank_name.cmp(&b.bank_name)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if pagination.order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

/// Active accounts for one currency, masked - the list a shopper
/// picks from at checkout.
pub(crate) async fn list_accounts_for_currency(
    ctx: web::Data<Arc<AppContext>>,
    currency: web::Path<String>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<BankAccountView>> = (|| {
        let currency = validate_currency(&currency)?;
        let mut items: Vec<BankAccountView> = accounts_store()
            .iter()
            .map(|entry| entry.value().view())
            .filter(|account| account.active && account.currency == currency)
            .collect();
        items.sort_by(|a, b| a.bank_name.cmp(&b.bank_name));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_account(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateBankAccountRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BankAccountView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        // Operator-only: bank accounts are the money destination
        // every transfer is checked against.
        require_admin(&ctx, &req)?;
        let input = body.into_inner();
        if input.bank_name.trim().is_empty() {
            return Err(UnifiedError::bad_request("bank_name is required"));
        }
        if input.account_holder.trim().is_empty() {
            return Err(UnifiedError::bad_request("account_holder is required"));
        }
        let account_number = validate_account_number(&input.account_number)?;
        let swift_bic = match input.swift_bic {
            Some(value) if !value.trim().is_empty() => Some(validate_swift_bic(&value)?),
            _ => None,
        };
        let currency = validate_currency(&input.currency)?;
        let instructions = input
            .instructions
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let duplicate = accounts_store().iter().any(|entry| {
            normalize_account_number(&entry.value().account_number) == account_number
                && entry.value().currency == currency
        });
        if duplicate {
            return Err(UnifiedError::conflict(
                "A bank account with this account number already exists for this currency",
            ));
        }
        let now = Utc::now();
        let account = BankAccount {
            id: Uuid::now_v7(),
            bank_name: input.bank_name.trim().to_string(),
            account_holder: input.account_holder.trim().to_string(),
            account_number,
            swift_bic,
            currency,
            instructions,
            active: input.active,
            created_at: now,
            updated_at: now,
        };
        accounts_store().insert(account.id, account.clone());
        Ok(account.view())
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_account(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BankAccountView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        find_account(id.into_inner()).map(|account| account.view())
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_account(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateBankAccountRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BankAccountView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        // Operator-only: bank accounts are the money destination
        // every transfer is checked against.
        require_admin(&ctx, &req)?;
        let id = id.into_inner();
        let input = body.into_inner();
        let mut account = find_account(id)?;
        if let Some(bank_name) = input.bank_name {
            if bank_name.trim().is_empty() {
                return Err(UnifiedError::bad_request("bank_name must not be empty"));
            }
            account.bank_name = bank_name.trim().to_string();
        }
        if let Some(holder) = input.account_holder {
            if holder.trim().is_empty() {
                return Err(UnifiedError::bad_request(
                    "account_holder must not be empty",
                ));
            }
            account.account_holder = holder.trim().to_string();
        }
        if let Some(number) = input.account_number {
            let normalized = validate_account_number(&number)?;
            let duplicate = accounts_store().iter().any(|entry| {
                entry.value().id != id
                    && normalize_account_number(&entry.value().account_number) == normalized
                    && entry.value().currency == account.currency
            });
            if duplicate {
                return Err(UnifiedError::conflict(
                    "A bank account with this account number already exists for this currency",
                ));
            }
            account.account_number = normalized;
        }
        if let Some(swift_bic) = input.swift_bic {
            account.swift_bic = if swift_bic.trim().is_empty() {
                None
            } else {
                Some(validate_swift_bic(&swift_bic)?)
            };
        }
        if let Some(instructions) = input.instructions {
            account.instructions = if instructions.trim().is_empty() {
                None
            } else {
                Some(instructions.trim().to_string())
            };
        }
        if let Some(active) = input.active {
            account.active = active;
        }
        account.updated_at = Utc::now();
        accounts_store().insert(id, account.clone());
        Ok(account.view())
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_account(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        // Operator-only: deleting a destination account changes
        // which transfers can still be verified.
        require_admin(&ctx, &req)?;
        let id = id.into_inner();
        let _account = find_account(id)?;
        let referenced = verifications_store().iter().any(|entry| {
            entry.value().bank_account_id == Some(id)
                && (entry.value().status == STATUS_AWAITING_PROOF
                    || entry.value().status == STATUS_SUBMITTED)
        });
        if referenced {
            return Err(UnifiedError::conflict(
                "Bank account cannot be deleted while open transfer verifications reference it; deactivate it instead",
            ));
        }
        accounts_store().remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// Handlers: transfer verifications.

pub(crate) async fn list_verifications(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<TransferVerification>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let pagination = params.normalized();
        // An administrator holding the manage permission lists every
        // verification; any other caller only ever sees their own
        // orders (an anonymous caller only `guest`-owned ones).
        let manage = can_manage(&ctx, &req);
        let mut items: Vec<TransferVerification> = verifications_store()
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|verification| {
                verification_visible_to_caller(&req, verification, manage)
                    && matches_search(
                    &format!(
                        "{} {} {}",
                        verification.order_id,
                        verification.customer_id,
                        verification
                            .proof
                            .as_ref()
                            .and_then(|proof| proof.reference_number.clone())
                            .unwrap_or_default()
                    ),
                    &params,
                ) && params.filters.get("status").is_none_or(|value| {
                    verification.status == *value
                }) && params.filters.get("order_id").is_none_or(|value| {
                    verification.order_id == *value
                }) && params.filters.get("customer_id").is_none_or(|value| {
                    verification.customer_id == *value
                }) && params.filters.get("bank_account_id").is_none_or(|value| {
                    verification
                        .bank_account_id
                        .map(|id| id.to_string())
                        .as_deref()
                        == Some(value.as_str())
                }) && params.filters.get("split_id").is_none_or(|value| {
                    verification.split_id.map(|id| id.to_string()).as_deref()
                        == Some(value.as_str())
                }) && params.filters.get("currency").is_none_or(|value| {
                    verification.currency.eq_ignore_ascii_case(value)
                })
            })
            .collect();
        let sort = params.allowed_sort(&["created_at", "status"], "created_at");
        match sort.as_str() {
            "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if pagination.order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_verification(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BankTransferState>>,
    body: web::Json<CreateVerificationRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerificationDetailView> = (|| {
        let input = body.into_inner();
        if input.order_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("order_id is required"));
        }
        if input.customer_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("customer_id is required"));
        }
        let currency = validate_currency(&input.currency)?;
        if let Some(expected) = input.expected_amount_minor {
            if expected <= 0 {
                return Err(UnifiedError::bad_request(
                    "expected_amount_minor must be greater than zero",
                ));
            }
        }
        let verification = insert_verification(
            NewVerification {
                order_id: input.order_id.trim().to_string(),
                invoice_id: input
                    .invoice_id
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty()),
                customer_id: input.customer_id.trim().to_string(),
                split_id: input.split_id,
                part_id: input.part_id,
                bank_account_id: input.bank_account_id,
                expected_amount_minor: input.expected_amount_minor,
                currency,
            },
            "system",
        )?;
        state.emit(
            BANK_TRANSFER_VERIFICATION_CREATED,
            json!({
                "verification_id": verification.id,
                "order_id": verification.order_id.clone(),
                "customer_id": verification.customer_id.clone(),
                "status": verification.status.clone(),
            }),
        );
        Ok(verification_detail(&verification))
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_verification(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerificationDetailView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let verification = find_verification(id.into_inner())?;
        // The detail of one verification is visible to its owner
        // and to an administrator holding the manage permission -
        // never to any other caller.
        if !can_manage(&ctx, &req) {
            require_verification_owner(&req, &verification)?;
        }
        Ok(verification_detail(&verification))
    })();
    json::respond(result, debug)
}

pub(crate) async fn submit_proof(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BankTransferState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<SubmitProofRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerificationDetailView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let id = id.into_inner();
        let input = body.into_inner();
        let mut verification = find_verification(id)?;
        if verification.status != STATUS_AWAITING_PROOF {
            return Err(UnifiedError::conflict(format!(
                "Verification is {} and cannot accept proof",
                verification.status
            )));
        }
        // Only the customer who owns this verification may upload
        // its proof: otherwise anyone could attach a proof to
        // another customer's order.
        require_verification_owner(&req, &verification)?;
        let actor = actor_of(&req, input.actor.clone());
        let storage_key = validate_storage_key(&input.storage_key)?;
        let checksum = match input.checksum {
            Some(value) if !value.trim().is_empty() => {
                let checksum = value.trim().to_string();
                if checksum.len() < 32
                    || checksum.len() > 128
                    || !checksum.chars().all(|c| c.is_ascii_hexdigit())
                {
                    return Err(UnifiedError::bad_request(
                        "Proof checksum must be a hex digest of 32 to 128 characters (for example SHA-256)",
                    ));
                }
                Some(checksum)
            }
            _ => None,
        };
        if input.amount_minor <= 0 {
            return Err(UnifiedError::bad_request(
                "Proof amount must be greater than zero (integer minor units)",
            ));
        }
        let currency = validate_currency(&input.currency)?;
        if currency != verification.currency {
            return Err(UnifiedError::unprocessable(format!(
                "Proof currency {currency} does not match the verification currency {}",
                verification.currency
            )));
        }
        let transfer_date = validate_transfer_date(&input.transfer_date)?;
        let settings = settings();
        let reference_number = input
            .reference_number
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if settings.require_reference_number && reference_number.is_none() {
            return Err(UnifiedError::bad_request(
                "A bank reference number is required for the payment proof",
            ));
        }
        if let Some(reference) = &reference_number {
            if reference.len() > 64 {
                return Err(UnifiedError::bad_request(
                    "Reference number must be at most 64 characters",
                ));
            }
        }
        let bank_account_id = input.bank_account_id.or(verification.bank_account_id);
        let bank_account_id = bank_account_id.ok_or_else(|| {
            UnifiedError::bad_request(
                "bank_account_id is required: the account the money was transferred to",
            )
        })?;
        let account = find_account(bank_account_id)?;
        if !account.active {
            return Err(UnifiedError::unprocessable(
                "The selected bank account is not active",
            ));
        }
        // Amount check against the expected total, within the
        // configured tolerance. A mismatch flags the verification
        // instead of blocking it: the administrator can still
        // approve it with a reason.
        let (amount_check, amount_difference) = match verification.expected_amount_minor {
            Some(expected) => {
                let difference = input.amount_minor - expected;
                if difference.abs() <= settings.amount_tolerance_minor {
                    ("matched".to_string(), Some(difference))
                } else {
                    ("mismatch".to_string(), Some(difference))
                }
            }
            None => ("skipped_no_expected_amount".to_string(), None),
        };
        let proof = TransferProof {
            storage_key: storage_key.clone(),
            checksum,
            amount_minor: input.amount_minor,
            currency: currency.clone(),
            transfer_date,
            reference_number,
            note: input
                .note
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            submitted_by: actor.clone(),
            submitted_at: Utc::now(),
        };
        verification.proof = Some(proof);
        verification.bank_account_id = Some(bank_account_id);
        verification.status = STATUS_SUBMITTED.to_string();
        verification.amount_check = amount_check.clone();
        verification.amount_difference_minor = amount_difference;
        verification.updated_at = Utc::now();
        verifications_store().insert(id, verification.clone());
        audit(
            id,
            "proof_submitted",
            &actor,
            "customer",
            None,
        );
        state.emit(
            BANK_TRANSFER_PROOF_SUBMITTED,
            json!({
                "verification_id": id,
                "order_id": verification.order_id,
                "customer_id": verification.customer_id,
                "bank_account_id": bank_account_id,
                "amount_minor": input.amount_minor,
                "currency": currency.clone(),
                "amount_check": amount_check,
                "amount_difference_minor": amount_difference,
                "split_id": verification.split_id,
                "part_id": verification.part_id,
            }),
        );
        notify(
            &state,
            "admin",
            "bank_transfer_proof_submitted",
            "Payment proof submitted",
            &format!(
                "Order {} has a bank transfer proof of {} {} to verify (amount check: {amount_check}).",
                verification.order_id, input.amount_minor, currency
            ),
        );
        Ok(verification_detail(&verification))
    })();
    json::respond(result, debug)
}

pub(crate) async fn verify_transfer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BankTransferState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<VerifyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerificationDetailView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let id = id.into_inner();
        let input = body.into_inner();
        let mut verification = find_verification(id)?;
        if verification.status != STATUS_SUBMITTED {
            return Err(UnifiedError::conflict(format!(
                "Only a submitted verification can be verified (status is \"{}\")",
                verification.status
            )));
        }
        // Operator-only: only an administrator holding the manage
        // permission may approve a transfer. The customer must
        // never approve their own transfer.
        require_admin(&ctx, &req)?;
        forbid_self_approval(&req, &verification)?;
        let actor = actor_of(&req, input.actor.clone());
        let reason = input
            .reason
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let proof = verification.proof.clone().ok_or_else(|| {
            UnifiedError::internal("Submitted verification has no proof attached")
        })?;
        // The administrator approval IS the capture: there is no
        // provider API to call. The transaction id is the record the
        // store, billing, and licensing wiring keys on.
        let transaction_id = input
            .transaction_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("bt-{}", Uuid::now_v7()));
        verification.status = STATUS_APPROVED.to_string();
        verification.transaction_id = Some(transaction_id.clone());
        verification.verified_by = Some(actor.clone());
        verification.verified_at = Some(Utc::now());
        verification.updated_at = Utc::now();
        verifications_store().insert(id, verification.clone());
        audit(
            id,
            "verified",
            &actor,
            "admin",
            reason.clone(),
        );
        state.emit(
            BANK_TRANSFER_VERIFIED,
            json!({
                "verification_id": id,
                "order_id": verification.order_id.clone(),
                "customer_id": verification.customer_id.clone(),
                "transaction_id": transaction_id.clone(),
                "amount_minor": proof.amount_minor,
                "currency": proof.currency.clone(),
                "amount_check": verification.amount_check.clone(),
                "split_id": verification.split_id,
                "part_id": verification.part_id,
            }),
        );
        // Capture event: same shape the gateway emits, so the
        // store / billing / licensing wiring picks the payment up
        // exactly like a gateway payment.
        state.emit(
            "payment.captured",
            json!({
                "transaction_id": transaction_id.clone(),
                "provider_transaction_id": transaction_id,
                "provider": "bank_transfer",
                "customer_id": verification.customer_id.clone(),
                "order_id": verification.order_id.clone(),
                "invoice_id": verification.invoice_id.clone(),
                "amount_minor": proof.amount_minor,
                "currency": proof.currency.clone(),
                "fee_minor": Value::Null,
                "status": "captured",
                "verification_id": id,
                "split_id": verification.split_id,
                "part_id": verification.part_id,
                "amount_mismatch": verification.amount_check == "mismatch",
            }),
        );
        notify(
            &state,
            &verification.customer_id,
            "bank_transfer_verified",
            "Bank transfer verified",
            &format!(
                "Your bank transfer of {} {} for order {} has been verified.",
                proof.amount_minor, proof.currency, verification.order_id
            ),
        );
        Ok(verification_detail(&verification))
    })();
    json::respond(result, debug)
}

pub(crate) async fn reject_transfer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BankTransferState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<RejectRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerificationDetailView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let id = id.into_inner();
        let input = body.into_inner();
        let mut verification = find_verification(id)?;
        if verification.status != STATUS_SUBMITTED {
            return Err(UnifiedError::conflict(format!(
                "Only a submitted verification can be rejected (status is \"{}\")",
                verification.status
            )));
        }
        // Operator-only: only an administrator holding the manage
        // permission may reject a transfer. The customer must never
        // reject their own transfer either: rejection cancels the
        // store order.
        require_admin(&ctx, &req)?;
        forbid_self_approval(&req, &verification)?;
        let actor = actor_of(&req, input.actor.clone());
        let reason = input.reason.trim().to_string();
        if reason.is_empty() {
            return Err(UnifiedError::bad_request(
                "A rejection reason is required",
            ));
        }
        verification.status = STATUS_REJECTED.to_string();
        verification.rejection_reason = Some(reason.clone());
        verification.updated_at = Utc::now();
        verifications_store().insert(id, verification.clone());
        audit(
            id,
            "rejected",
            &actor,
            "admin",
            Some(reason.clone()),
        );
        state.emit(
            BANK_TRANSFER_REJECTED,
            json!({
                "verification_id": id,
                "order_id": verification.order_id.clone(),
                "customer_id": verification.customer_id.clone(),
                "reason": reason.clone(),
                "split_id": verification.split_id,
                "part_id": verification.part_id,
            }),
        );
        // Failure event: the store cancels the order on rejection
        // (the `pending_verification` to `pending` transition stays
        // available in the store's status machine for an administrator
        // who wants to put the order back to awaiting payment).
        state.emit(
            "payment.failed",
            json!({
                "provider": "bank_transfer",
                "customer_id": verification.customer_id.clone(),
                "order_id": verification.order_id.clone(),
                "invoice_id": verification.invoice_id.clone(),
                "reason": reason,
                "verification_id": id,
                "split_id": verification.split_id,
                "part_id": verification.part_id,
            }),
        );
        notify(
            &state,
            &verification.customer_id,
            "bank_transfer_rejected",
            "Bank transfer rejected",
            &format!(
                "Your bank transfer for order {} was rejected: {reason}. Please check the transfer details and try again.",
                verification.order_id
            ),
        );
        Ok(verification_detail(&verification))
    })();
    json::respond(result, debug)
}

pub(crate) async fn cancel_verification(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BankTransferState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: Option<web::Json<CancelVerificationRequest>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerificationDetailView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let id = id.into_inner();
        let input = body.map(|body| body.into_inner());
        let mut verification = find_verification(id)?;
        if verification.status != STATUS_AWAITING_PROOF
            && verification.status != STATUS_SUBMITTED
        {
            return Err(UnifiedError::conflict(format!(
                "Verification is {} and cannot be cancelled",
                verification.status
            )));
        }
        // Only the owning customer may cancel their verification.
        require_verification_owner(&req, &verification)?;
        let actor = actor_of(
            &req,
            input.as_ref().and_then(|input| input.actor.clone()),
        );
        let reason = input
            .and_then(|input| input.reason)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        verification.status = STATUS_CANCELLED.to_string();
        verification.updated_at = Utc::now();
        verifications_store().insert(id, verification.clone());
        audit(id, "cancelled", &actor, "customer", reason.clone());
        state.emit(
            BANK_TRANSFER_CANCELLED,
            json!({
                "verification_id": id,
                "order_id": verification.order_id.clone(),
                "customer_id": verification.customer_id.clone(),
                "reason": reason,
            }),
        );
        notify(
            &state,
            &verification.customer_id,
            "bank_transfer_cancelled",
            "Bank transfer cancelled",
            &format!(
                "The bank transfer verification for order {} was cancelled.",
                verification.order_id
            ),
        );
        Ok(verification_detail(&verification))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_audit(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<VerificationAuditEntry>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let id = id.into_inner();
        let verification = find_verification(id)?;
        // The audit trail of one verification is visible to its
        // owner and to an administrator holding the manage
        // permission - never to any other caller.
        if !can_manage(&ctx, &req) {
            require_verification_owner(&req, &verification)?;
        }
        let pagination = params.normalized();
        let mut items: Vec<VerificationAuditEntry> = audit_store()
            .iter()
            .filter(|entry| entry.value().verification_id == id)
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                matches_search(&format!("{} {}", entry.action, entry.actor), &params)
                    && params
                        .filters
                        .get("action")
                        .is_none_or(|value| entry.action == *value)
                    && params.filters.get("actor_role").is_none_or(|value| {
                        entry.actor_role == *value
                    })
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if pagination.order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
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
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    fn test_context_with_role_check(allowed: bool) -> AppContext {
        let bus = Arc::new(HookBus::new());
        bus.register(
            "role.check",
            "test",
            Arc::new(move |context: &mut unified_core::hooks::HookContext| {
                context.set("allowed", Value::Bool(allowed));
            }),
        );
        AppContext::new(AppConfig::default(), bus, Arc::new(ModuleRegistry::new()))
    }

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn data(ctx: AppContext) -> web::Data<Arc<AppContext>> {
        web::Data::new(Arc::new(ctx))
    }

    fn state_data() -> web::Data<Arc<BankTransferState>> {
        web::Data::new(Arc::new(BankTransferState::new()))
    }

    fn admin_request() -> HttpRequest {
        verified_request("admin-1", "admin")
    }

    fn customer_request(customer_id: &str) -> HttpRequest {
        verified_request(customer_id, "customer")
    }

    fn stored_account() -> BankAccount {
        let now = Utc::now();
        let account = BankAccount {
            id: Uuid::now_v7(),
            bank_name: "Test Bank".to_string(),
            account_holder: "Test Holder".to_string(),
            account_number: "1234567890".to_string(),
            swift_bic: None,
            currency: "USD".to_string(),
            instructions: None,
            active: true,
            created_at: now,
            updated_at: now,
        };
        accounts_store().insert(account.id, account.clone());
        account
    }

    fn stored_verification(customer_id: &str, status: &str) -> TransferVerification {
        let now = Utc::now();
        let verification = TransferVerification {
            id: Uuid::now_v7(),
            order_id: format!("order-{}", Uuid::now_v7()),
            invoice_id: None,
            customer_id: customer_id.to_string(),
            split_id: None,
            part_id: None,
            bank_account_id: None,
            status: status.to_string(),
            expected_amount_minor: Some(1000),
            currency: "USD".to_string(),
            proof: Some(TransferProof {
                storage_key: "proofs/order-1/receipt.png".to_string(),
                checksum: None,
                amount_minor: 1000,
                currency: "USD".to_string(),
                transfer_date: "2026-11-22".to_string(),
                reference_number: None,
                note: None,
                submitted_by: customer_id.to_string(),
                submitted_at: now,
            }),
            amount_check: "matched".to_string(),
            amount_difference_minor: Some(0),
            transaction_id: None,
            verified_by: None,
            verified_at: None,
            rejection_reason: None,
            created_at: now,
            updated_at: now,
        };
        verifications_store().insert(verification.id, verification.clone());
        verification
    }

    fn update_body() -> web::Json<UpdateBankAccountRequest> {
        web::Json(UpdateBankAccountRequest {
            bank_name: Some("Renamed Bank".to_string()),
            account_holder: None,
            account_number: None,
            swift_bic: None,
            instructions: None,
            active: None,
        })
    }

    #[tokio::test]
    async fn update_account_rejects_anonymous_callers() {
        let account = stored_account();
        let response = update_account(
            data(test_context()),
            TestRequest::default().to_http_request(),
            web::Path::from(account.id),
            update_body(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn update_account_rejects_callers_without_manage_permission() {
        let account = stored_account();
        let response = update_account(
            data(test_context_with_role_check(false)),
            admin_request(),
            web::Path::from(account.id),
            update_body(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn update_account_accepts_granted_admin() {
        let account = stored_account();
        let response = update_account(
            data(test_context_with_role_check(true)),
            admin_request(),
            web::Path::from(account.id),
            update_body(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn verify_transfer_rejects_identified_non_admin() {
        // The caller is identified and is not the submitter, so the
        // old self-approval guard alone would have let them through.
        let verification = stored_verification("customer-1", STATUS_SUBMITTED);
        let response = verify_transfer(
            data(test_context_with_role_check(false)),
            state_data(),
            customer_request("customer-2"),
            web::Path::from(verification.id),
            web::Json(VerifyRequest {
                actor: None,
                reason: None,
                transaction_id: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn verify_transfer_accepts_granted_admin() {
        let verification = stored_verification("customer-1", STATUS_SUBMITTED);
        let response = verify_transfer(
            data(test_context_with_role_check(true)),
            state_data(),
            admin_request(),
            web::Path::from(verification.id),
            web::Json(VerifyRequest {
                actor: None,
                reason: None,
                transaction_id: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn verify_transfer_still_rejects_admin_who_is_the_customer() {
        // The manage permission is granted, but the admin identity
        // is the verification's customer: self-approval stays
        // forbidden.
        let verification = stored_verification("admin-1", STATUS_SUBMITTED);
        let response = verify_transfer(
            data(test_context_with_role_check(true)),
            state_data(),
            admin_request(),
            web::Path::from(verification.id),
            web::Json(VerifyRequest {
                actor: None,
                reason: None,
                transaction_id: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn reject_transfer_rejects_identified_non_admin() {
        let verification = stored_verification("customer-1", STATUS_SUBMITTED);
        let response = reject_transfer(
            data(test_context_with_role_check(false)),
            state_data(),
            customer_request("customer-2"),
            web::Path::from(verification.id),
            web::Json(RejectRequest {
                actor: None,
                reason: "Proof does not match".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn reject_transfer_accepts_granted_admin() {
        let verification = stored_verification("customer-1", STATUS_SUBMITTED);
        let response = reject_transfer(
            data(test_context_with_role_check(true)),
            state_data(),
            admin_request(),
            web::Path::from(verification.id),
            web::Json(RejectRequest {
                actor: None,
                reason: "Proof does not match".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_verification_allows_admin_for_other_customers_record() {
        let verification = stored_verification("customer-1", STATUS_AWAITING_PROOF);
        let response = get_verification(
            data(test_context_with_role_check(true)),
            admin_request(),
            web::Path::from(verification.id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_verification_rejects_non_admin_stranger() {
        let verification = stored_verification("customer-1", STATUS_AWAITING_PROOF);
        let response = get_verification(
            data(test_context_with_role_check(false)),
            customer_request("customer-2"),
            web::Path::from(verification.id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn get_verification_allows_owner() {
        let verification = stored_verification("customer-1", STATUS_AWAITING_PROOF);
        let response = get_verification(
            data(test_context_with_role_check(false)),
            customer_request("customer-1"),
            web::Path::from(verification.id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }
}
