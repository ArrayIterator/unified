// bank_transfer - Internal service helpers shared by the handlers.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookContext;
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::HttpRequest;
use chrono::Utc;
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

/// Actor identity: an explicit body actor wins, then the `x-user-id`
/// / `x-session-id` headers, then `unknown`. The value is recorded
/// verbatim in the audit trail.
pub(crate) fn actor_of(req: &HttpRequest, body_actor: Option<String>) -> String {
    if let Some(actor) = body_actor
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return actor;
    }
    let verified = unified_core::caller::caller_from(req);
    if verified.is_authenticated() {
        return verified.user_id;
    }
    "unknown".to_string()
}

/// Caller identities from the server-verified caller. Unlike
/// `actor_of`, the body-supplied actor is never trusted here: the
/// body value is self-asserted, so only the verified user id counts
/// for ownership checks. See the store module: this is an ownership
/// check on verified identity, not authentication.
fn caller_identities(req: &HttpRequest) -> Vec<String> {
    let verified = unified_core::caller::caller_from(req);
    if verified.is_authenticated() {
        vec![verified.user_id]
    } else {
        Vec::new()
    }
}

/// True when the caller may act on a record owned by `owner`: one of
/// the caller identities matches the owner, or there are no identity
/// headers and the owner is `guest`.
pub(crate) fn caller_owns(req: &HttpRequest, owner: &str) -> bool {
    let identities = caller_identities(req);
    if identities.is_empty() {
        return owner == "guest";
    }
    identities.iter().any(|identity| identity == owner)
}

/// Reject a customer action on a verification the caller does not
/// own. Proof upload and cancellation are customer actions, so the
/// caller must be the verification's customer.
pub(crate) fn require_verification_owner(
    req: &HttpRequest,
    verification: &TransferVerification,
) -> UnifiedResult<()> {
    if caller_owns(req, &verification.customer_id) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Verification belongs to a different customer",
        ))
    }
}

/// Reject an administrator action performed by the customer themself.
/// Verification and rejection are administrator actions, so the
/// caller must first be identifiable: a caller with no identity
/// header is refused outright (previously such a caller was allowed
/// through, so anyone without headers could approve or reject any
/// transfer), and a caller who identifies as the verification's
/// customer is refused as self-approval.
pub(crate) fn forbid_self_approval(
    req: &HttpRequest,
    verification: &TransferVerification,
) -> UnifiedResult<()> {
    let identities = caller_identities(req);
    if identities.is_empty() {
        return Err(UnifiedError::unauthorized(
            "An authenticated caller identity is required to verify or reject a transfer",
        ));
    }
    if identities
        .iter()
        .any(|identity| identity == &verification.customer_id)
    {
        return Err(UnifiedError::forbidden(
            "The customer cannot verify or reject their own transfer",
        ));
    }
    Ok(())
}

// Administrator authorization: `X-User-Id` plus `X-Role` headers,
// capability checked through the `role.check` hook, failing closed
// (same pattern as the webhooks, roles and store modules). The
// manage permission gates the operator-only endpoints (settings,
// bank account creation / deletion) and widens the verification
// and audit listings to every order; ordinary callers only ever
// see their own records.

/// Capability required to manage bank transfer settings and bank
/// accounts, and to list verifications and audit trails for every
/// order. The roles module answers `role.check` for it (the admin
/// role's `"*": ["*"]` wildcard covers it once granted); until the
/// capability is granted, the check fails closed.
pub(crate) const PERM_BANK_TRANSFER_MANAGE: &str = "bank_transfer.manage";

/// An identified administrator caller: actor plus the role the
/// capability check ran against.
pub(crate) struct AdminCaller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

/// Caller identity for the capability-checked routes, from the
/// The server-verified caller (see `unified_core::caller::Caller`):
/// the user id identifies the actor, the server-resolved role is the
/// role slug or id the capability check runs against.
pub(crate) fn admin_caller(req: &HttpRequest) -> UnifiedResult<AdminCaller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(AdminCaller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

/// Require that `role` holds `permission`; fail closed when the
/// roles module does not grant it.
pub(crate) fn require_permission(
    ctx: &AppContext,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// Require an administrator caller holding the manage permission:
/// identify the caller from the headers, then check the capability.
/// Used by the operator-only endpoints (settings, bank account
/// creation / deletion).
pub(crate) fn require_admin(ctx: &AppContext, req: &HttpRequest) -> UnifiedResult<AdminCaller> {
    let caller = admin_caller(req)?;
    require_permission(ctx, &caller.role, PERM_BANK_TRANSFER_MANAGE)?;
    Ok(caller)
}
// Tenant edge guard (C3 pilot 4, gate-only).

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` / `authorize` or any
/// capability check.
///
/// This module's records carry no `company_id` / `tenant_id` scope
/// field, so the guard is a fail-closed gate only: no per-record
/// company check is applied. Real partitioning of these records
/// needs a `tenant_id` on the record (C11 follow-up).
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// Company scope of the verified caller: the tenant context's
/// company first, the server-resolved caller company second; a
/// mismatch between the two is a 403. The `X-Company-Id` header is
/// never read. Resolved in every gated handler so a tenant/caller
/// mismatch fails closed even though these records carry no company
/// field to filter on (gate-only; see `require_tenant`).
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified.clone()),
    }
}

/// True when the caller holds the manage permission (used to widen
/// a self-service listing to every order's records). Missing
/// identity headers or a denied capability mean "no", never an
/// error: this is a widening check, not the gate itself.
pub(crate) fn can_manage(ctx: &AppContext, req: &HttpRequest) -> bool {
    let Ok(caller) = admin_caller(req) else {
        return false;
    };
    check_capability(ctx, &caller.role, PERM_BANK_TRANSFER_MANAGE)
}

/// True when a list endpoint may show a verification to this
/// caller: a caller holding the manage permission sees every
/// verification, otherwise only their own (an anonymous caller
/// only ever sees `guest`-owned verifications).
pub(crate) fn verification_visible_to_caller(
    req: &HttpRequest,
    verification: &TransferVerification,
    manage: bool,
) -> bool {
    if manage {
        return true;
    }
    caller_owns(req, &verification.customer_id)
}

/// Paginate an already-sorted, already-filtered list.
pub(crate) fn paginate<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

pub(crate) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        None => true,
        Some(needle) => haystack.to_lowercase().contains(&needle.to_lowercase()),
    }
}

/// Request an in-app notification through the hook bus (the
/// notifications module listens to `notification.requested`).
pub(crate) fn notify(
    state: &BankTransferState,
    user_id: &str,
    kind: &str,
    title: &str,
    body: &str,
) {
    state.emit(
        "notification.requested",
        json!({
            "user_id": user_id,
            "kind": kind,
            "title": title,
            "body": body,
        }),
    );
}

/// Append one audit entry for a verification.
pub(crate) fn audit(
    verification_id: Uuid,
    action: &str,
    actor: &str,
    actor_role: &str,
    reason: Option<String>,
) {
    let entry = VerificationAuditEntry {
        id: Uuid::now_v7(),
        verification_id,
        action: action.to_string(),
        actor: actor.to_string(),
        actor_role: actor_role.to_string(),
        reason,
        created_at: Utc::now(),
    };
    audit_store().insert(entry.id, entry);
}

pub(crate) fn find_account(id: Uuid) -> UnifiedResult<BankAccount> {
    accounts_store()
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Bank account \"{id}\" not found")))
}

pub(crate) fn find_verification(id: Uuid) -> UnifiedResult<TransferVerification> {
    verifications_store()
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| {
            UnifiedError::not_found(format!("Transfer verification \"{id}\" not found"))
        })
}

/// Build the detail view of a verification: the record, the masked
/// bank account it names, and the full audit trail in time order.
pub(crate) fn verification_detail(verification: &TransferVerification) -> VerificationDetailView {
    let bank_account = verification
        .bank_account_id
        .and_then(|id| accounts_store().get(&id).map(|entry| entry.value().view()));
    let mut entries: Vec<VerificationAuditEntry> = audit_store()
        .iter()
        .filter(|entry| entry.value().verification_id == verification.id)
        .map(|entry| entry.value().clone())
        .collect();
    entries.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    VerificationDetailView {
        verification: verification.clone(),
        bank_account,
        audit: entries,
    }
}

/// Open a verification in status `awaiting_proof`. A second open
/// verification for the same order (and the same split part, when
/// one is given) is refused with a 409, so a checkout or a split
/// payment can be retried safely without opening duplicates.
pub(crate) fn insert_verification(
    input: NewVerification,
    actor: &str,
) -> UnifiedResult<TransferVerification> {
    let open = verifications_store().iter().any(|entry| {
        let existing = entry.value();
        existing.order_id == input.order_id
            && existing.split_id == input.split_id
            && existing.part_id == input.part_id
            && (existing.status == STATUS_AWAITING_PROOF || existing.status == STATUS_SUBMITTED)
    });
    if open {
        return Err(UnifiedError::conflict(format!(
            "An open transfer verification already exists for order {}",
            input.order_id
        )));
    }
    if let Some(bank_account_id) = input.bank_account_id {
        let account = find_account(bank_account_id)?;
        if !account.active {
            return Err(UnifiedError::unprocessable(
                "The selected bank account is not active",
            ));
        }
    }
    let now = Utc::now();
    let verification = TransferVerification {
        id: Uuid::now_v7(),
        order_id: input.order_id,
        invoice_id: input.invoice_id,
        customer_id: input.customer_id,
        split_id: input.split_id,
        part_id: input.part_id,
        bank_account_id: input.bank_account_id,
        status: STATUS_AWAITING_PROOF.to_string(),
        expected_amount_minor: input.expected_amount_minor,
        currency: input.currency,
        proof: None,
        amount_check: "not_checked".to_string(),
        amount_difference_minor: None,
        transaction_id: None,
        verified_by: None,
        verified_at: None,
        rejection_reason: None,
        created_at: now,
        updated_at: now,
    };
    verifications_store().insert(verification.id, verification.clone());
    audit(
        verification.id,
        "created",
        actor,
        "system",
        None,
    );
    Ok(verification)
}

/// Find the open verification for an order (and the same split part,
/// when one is given): the same open check `insert_verification`
/// refuses a duplicate for.
pub(crate) fn find_open_verification(
    order_id: &str,
    split_id: Option<Uuid>,
    part_id: Option<Uuid>,
) -> Option<TransferVerification> {
    verifications_store()
        .iter()
        .map(|entry| entry.value().clone())
        .find(|existing| {
            existing.order_id == order_id
                && existing.split_id == split_id
                && existing.part_id == part_id
                && (existing.status == STATUS_AWAITING_PROOF
                    || existing.status == STATUS_SUBMITTED)
        })
}

/// Hook result for a checkout or split payment that arrives while
/// an open verification already exists for the same order: a
/// distinct `already_exists` status (with the existing
/// verification's id) instead of `rejected`, so a safe retry is
/// recognisable as a retry and the caller can resume the existing
/// verification instead of treating the payment as refused.
pub(crate) fn already_exists_result(verification: &TransferVerification) -> Value {
    json!({
        "bank_transfer": "already_exists",
        "order_id": verification.order_id,
        "verification_id": verification.id,
        "status": verification.status,
        "note": "An open transfer verification already exists for this order; upload proof to the existing verification instead of opening a new one.",
    })
}

// Hook listeners.

/// The store started a checkout with provider `bank_transfer` and
/// emitted `payment.initiated` with `{ "order", "provider",
/// "amount" }`. Open a verification in status `awaiting_proof` so
/// the shopper can upload proof. The expected amount comes from the
/// order total (major units, converted to minor units); when the
/// total is absent the amount check is skipped on proof upload.
pub(crate) fn on_payment_initiated(state: Arc<BankTransferState>, ctx: &mut HookContext) {
    let provider = ctx
        .get("provider")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if provider != "bank_transfer" && provider != "bank-transfer" {
        return;
    }
    let order = ctx.get("order").cloned().unwrap_or(Value::Null);
    let order_id = order
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(order_id) = order_id else {
        ctx.add_result(json!({
            "bank_transfer": "rejected",
            "error": "payment.initiated carries no order id",
        }));
        return;
    };
    // Store orders always carry `user_id`, and guest orders carry
    // `"guest"` (the store builds the order from the cart owner,
    // which falls back to `"guest"` with no identity headers), so
    // the fallback here is `"guest"` as well: an empty customer id
    // would make proof submission impossible, because the ownership
    // check only ever matches an empty owner against no one.
    let customer_id = order
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("guest")
        .to_string();
    let currency = order
        .get("currency")
        .and_then(|value| value.as_str())
        .unwrap_or("USD")
        .to_uppercase();
    let amount_major = ctx
        .get("amount")
        .and_then(|value| value.as_f64())
        .or_else(|| order.get("total").and_then(|value| value.as_f64()));
    let expected = amount_major.and_then(|amount| major_to_minor(amount, &currency));
    match insert_verification(
        NewVerification {
            order_id: order_id.clone(),
            invoice_id: None,
            customer_id,
            split_id: None,
            part_id: None,
            bank_account_id: None,
            expected_amount_minor: expected,
            currency,
        },
        "system",
    ) {
        Ok(verification) => {
            // The store moves the order to `pending_verification` on
            // this event, so it is emitted here as well as on the
            // manual create endpoint.
            state.emit(
                BANK_TRANSFER_VERIFICATION_CREATED,
                json!({
                    "verification_id": verification.id,
                    "order_id": verification.order_id.clone(),
                    "customer_id": verification.customer_id.clone(),
                    "status": verification.status.clone(),
                }),
            );
            ctx.add_result(json!({
                "bank_transfer": "accepted",
                "order_id": order_id,
                "verification_id": verification.id,
                "status": verification.status,
                "note": "Transfer the amount to a configured bank account, then upload proof with POST /api/v1/bank-transfer/verifications/{id}/proof.",
            }));
        }
        Err(error) => {
            // A duplicate open verification is a safe retry, not a
            // refusal: answer `already_exists` with the existing
            // verification so the caller resumes it instead of
            // treating the payment as rejected.
            if let Some(existing) = find_open_verification(&order_id, None, None) {
                ctx.add_result(already_exists_result(&existing));
            } else {
                ctx.add_result(json!({
                    "bank_transfer": "rejected",
                    "order_id": order_id,
                    "error": error.message,
                }));
            }
        }
    }
}

/// A split part is being paid with bank transfer (emitted by the
/// payments module as `bank_transfer.proof.requested` with the split
/// / part ids, the payer, and the part amount). Open a verification
/// for that part so the payer can upload proof; approval then
/// completes the part through `payment.captured`.
pub(crate) fn on_proof_requested(state: Arc<BankTransferState>, ctx: &mut HookContext) {
    let split_id = ctx
        .get("split_id")
        .and_then(|value| value.as_str())
        .and_then(|value| Uuid::parse_str(value).ok());
    let part_id = ctx
        .get("part_id")
        .and_then(|value| value.as_str())
        .and_then(|value| Uuid::parse_str(value).ok());
    let order_id = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_default();
    if order_id.is_empty() {
        ctx.add_result(json!({
            "bank_transfer": "rejected",
            "error": "bank_transfer.proof.requested carries no order_id",
        }));
        return;
    }
    // Same `"guest"` fallback as `on_payment_initiated`: an empty
    // customer id would make proof submission impossible, because
    // the ownership check only ever matches an empty owner against
    // no one.
    let customer_id = ctx
        .get("customer_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("guest")
        .to_string();
    let currency = ctx
        .get("currency")
        .and_then(|value| value.as_str())
        .unwrap_or("USD")
        .to_uppercase();
    let expected = ctx
        .get("amount_minor")
        .and_then(|value| value.as_i64());
    let invoice_id = ctx
        .get("invoice_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    match insert_verification(
        NewVerification {
            order_id: order_id.clone(),
            invoice_id,
            customer_id,
            split_id,
            part_id,
            bank_account_id: None,
            expected_amount_minor: expected,
            currency,
        },
        "system",
    ) {
        Ok(verification) => {
            audit(
                verification.id,
                "proof_requested",
                "system",
                "system",
                None,
            );
            state.emit(
                BANK_TRANSFER_VERIFICATION_CREATED,
                json!({
                    "verification_id": verification.id,
                    "order_id": verification.order_id.clone(),
                    "customer_id": verification.customer_id.clone(),
                    "status": verification.status.clone(),
                    "split_id": verification.split_id,
                    "part_id": verification.part_id,
                }),
            );
            ctx.add_result(json!({
                "bank_transfer": "accepted",
                "order_id": order_id,
                "verification_id": verification.id,
                "status": verification.status,
            }));
        }
        Err(error) => {
            // A duplicate open verification is a safe retry, not a
            // refusal: answer `already_exists` with the existing
            // verification so the caller resumes it instead of
            // treating the payment as rejected.
            if let Some(existing) = find_open_verification(&order_id, split_id, part_id) {
                ctx.add_result(already_exists_result(&existing));
            } else {
                ctx.add_result(json!({
                    "bank_transfer": "rejected",
                    "order_id": order_id,
                    "error": error.message,
                }));
            }
        }
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
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use actix_web::test::TestRequest;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn test_context_with_role_check(allowed: bool) -> AppContext {
        let bus = Arc::new(HookBus::new());
        bus.register(
            "role.check",
            "test",
            Arc::new(move |context: &mut HookContext| {
                context.set("allowed", Value::Bool(allowed));
            }),
        );
        AppContext::new(AppConfig::default(), bus, Arc::new(ModuleRegistry::new()))
    }

    fn admin_request() -> HttpRequest {
        verified_request("admin-1", "admin")
    }

    fn test_verification(customer_id: &str) -> TransferVerification {
        let now = Utc::now();
        TransferVerification {
            id: Uuid::now_v7(),
            order_id: format!("order-{}", Uuid::now_v7()),
            invoice_id: None,
            customer_id: customer_id.to_string(),
            split_id: None,
            part_id: None,
            bank_account_id: None,
            status: STATUS_AWAITING_PROOF.to_string(),
            expected_amount_minor: None,
            currency: "USD".to_string(),
            proof: None,
            amount_check: "not_checked".to_string(),
            amount_difference_minor: None,
            transaction_id: None,
            verified_by: None,
            verified_at: None,
            rejection_reason: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn forbid_self_approval_rejects_a_caller_without_identity() {
        let verification = test_verification("customer-1");
        let req = TestRequest::default().to_http_request();
        assert!(forbid_self_approval(&req, &verification).is_err());
    }

    #[test]
    fn forbid_self_approval_rejects_the_customer() {
        let verification = test_verification("customer-1");
        let req = verified_request_without_roles("customer-1");
        assert!(forbid_self_approval(&req, &verification).is_err());
    }

    #[test]
    fn forbid_self_approval_allows_another_identified_caller() {
        let verification = test_verification("customer-1");
        let req = verified_request_without_roles("admin-1");
        assert!(forbid_self_approval(&req, &verification).is_ok());
    }

    #[test]
    fn admin_caller_requires_a_verified_caller() {
        assert!(admin_caller(&TestRequest::default().to_http_request()).is_err());
        assert!(admin_caller(
            &TestRequest::default().to_http_request()
        )
        .is_err());
        assert!(admin_caller(
            &TestRequest::default().to_http_request()
        )
        .is_err());
        let caller = admin_caller(&admin_request()).unwrap();
        assert_eq!(caller.actor, test_user_uuid("admin-1").to_string());
        assert_eq!(caller.role, "admin");
    }

    #[test]
    fn require_permission_fails_closed_without_a_role_check_handler() {
        // No `role.check` handler is registered on this bus, so the
        // preset `"allowed": false` must stand and the call is refused.
        let ctx = test_context();
        assert!(!check_capability(&ctx, "admin", PERM_BANK_TRANSFER_MANAGE));
        assert!(require_permission(&ctx, "admin", PERM_BANK_TRANSFER_MANAGE).is_err());
        assert!(require_admin(&ctx, &admin_request()).is_err());
    }

    #[test]
    fn require_admin_allows_when_role_check_grants() {
        let ctx = test_context_with_role_check(true);
        let caller = require_admin(&ctx, &admin_request()).unwrap();
        assert_eq!(caller.actor, test_user_uuid("admin-1").to_string());
        assert_eq!(caller.role, "admin");
    }

    #[test]
    fn require_admin_rejects_when_role_check_denies() {
        let ctx = test_context_with_role_check(false);
        assert!(require_admin(&ctx, &admin_request()).is_err());
    }

    #[test]
    fn can_manage_widens_only_for_granted_admins() {
        let denied = test_context_with_role_check(false);
        let granted = test_context_with_role_check(true);
        let anonymous = TestRequest::default().to_http_request();
        assert!(!can_manage(&denied, &anonymous));
        assert!(!can_manage(&denied, &admin_request()));
        assert!(!can_manage(&granted, &anonymous));
        assert!(can_manage(&granted, &admin_request()));
    }

    #[test]
    fn verification_visible_to_caller_scoping() {
        let owned = test_verification("customer-1");
        let guest_owned = test_verification("guest");
        let owner = verified_request_without_roles("customer-1");
        let stranger = verified_request_without_roles("customer-2");
        let anonymous = TestRequest::default().to_http_request();

        // An admin (manage) sees every verification.
        assert!(verification_visible_to_caller(&admin_request(), &owned, true));
        // The owner sees their own verification, a stranger does not.
        assert!(verification_visible_to_caller(&owner, &owned, false));
        assert!(!verification_visible_to_caller(&stranger, &owned, false));
        // An anonymous caller only ever sees `guest`-owned records.
        assert!(verification_visible_to_caller(&anonymous, &guest_owned, false));
        assert!(!verification_visible_to_caller(&anonymous, &owned, false));
    }

    #[test]
    fn find_open_verification_and_already_exists_result() {
        let mut verification = test_verification("customer-1");
        let order_id = verification.order_id.clone();
        verifications_store().insert(verification.id, verification.clone());

        let found = find_open_verification(&order_id, None, None).unwrap();
        assert_eq!(found.id, verification.id);
        let result = already_exists_result(&found);
        assert_eq!(result["bank_transfer"], "already_exists");
        assert_eq!(result["order_id"], order_id);
        assert_eq!(result["verification_id"], verification.id.to_string());
        assert_eq!(result["status"], STATUS_AWAITING_PROOF);

        // A closed verification is no duplicate: it no longer blocks
        // a new verification for the same order.
        verification.status = STATUS_APPROVED.to_string();
        verifications_store().insert(verification.id, verification.clone());
        assert!(find_open_verification(&order_id, None, None).is_none());

        verifications_store().remove(&verification.id);
    }
}
