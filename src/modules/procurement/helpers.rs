// procurement -- Caller, validation, totals, budget and view helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use chrono::NaiveDate;
use chrono::Utc;
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::state::*;
use super::types::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the ai module.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the ai module's capability check).
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

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 4, group E).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// Records in this module carry no company field, so the guard is a
/// fail-closed gate only — there is no per-record company check to
/// apply. Real partitioning needs a `tenant_id` on the record, a
/// C11 follow-up, not a C3 gate.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// — a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources.
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

/// Writes require a resolved company: a mutating handler with no
/// resolvable company scope is refused (403). Reads proceed on the
/// tenant gate plus the mismatch check above — records here carry
/// no company field, so there is nothing to partition on read.
pub(crate) fn require_caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<String> {
    caller_company(tenant, req)?
        .ok_or_else(|| UnifiedError::forbidden("A write requires a resolved company scope"))
}

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be empty"
        )));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

pub(crate) fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

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

pub(crate) fn valid_email(email: &str) -> bool {
    if email.is_empty() || email.len() > MAX_EMAIL_LEN || email.contains(' ') {
        return false;
    }
    let mut parts = email.split('@');
    let (Some(local), Some(domain)) = (parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() || local.is_empty() || domain.is_empty() {
        return false;
    }
    domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// Reject a negative minor-unit amount from a request body.
/// Request amounts arrive as integer minor units (the request
/// JSON carries major units, converted by `serde_major`, which
/// already rejects non-finite values); only the non-negative
/// check is left to do here.
pub(crate) fn require_non_negative_minor(amount: i64, field: &str) -> UnifiedResult<i64> {
    if amount < 0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be zero or greater"
        )));
    }
    Ok(amount)
}

/// True for zero-decimal currencies (no minor units), matching the
/// billing and store modules' lists.
pub(crate) fn is_zero_decimal_currency(currency: &str) -> bool {
    matches!(
        currency,
        "JPY" | "KRW" | "VND" | "CLP" | "ISK" | "UGX" | "XAF" | "XOF" | "XPF"
    )
}

/// Convert integer minor units to major units, as JSON and hook
/// payloads carry them. This is the boundary conversion out of the
/// integer representation; business logic keeps minor units.
/// (The inbound conversion lives in `serde_major` on the request
/// structs, so there is no `major_to_minor` here: no f64 major
/// money value reaches this module anymore.)
pub(crate) fn minor_to_major(amount_minor: i64, currency: &str) -> f64 {
    if is_zero_decimal_currency(currency) {
        amount_minor as f64
    } else {
        ((amount_minor as f64 / 100.0) * 100.0).round() / 100.0
    }
}

/// One line's net amount (`quantity * unit_price`) in minor units,
/// rounded half away from zero. `quantity` stays `f64` (it can be
/// fractional); the result is rounded exactly once to minor units,
/// which is the same rounding the old `round_money` float math
/// applied in cents.
pub(crate) fn line_net_minor(quantity: f64, unit_price_minor: i64) -> i64 {
    (quantity * unit_price_minor as f64).round() as i64
}

/// One line's total (`quantity * unit_price * (1 + tax_rate)`) in
/// minor units, rounded half away from zero to minor units.
pub(crate) fn line_total_minor(quantity: f64, unit_price_minor: i64, tax_rate: f64) -> i64 {
    (quantity * unit_price_minor as f64 * (1.0 + tax_rate)).round() as i64
}

/// Round a quantity to 4 decimal places. Quantities stay `f64`;
/// only money moved to minor units.
pub(crate) fn round_quantity(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// Over-receipt check for goods receipts. The received +
/// incoming quantities are rounded with `round_quantity` before
/// comparison: raw f64 addition can turn an exact fill such as
/// 0.1 + 0.2 into 0.30000000000000004, which would falsely
/// exceed an ordered quantity of 0.3.
pub(crate) fn would_over_receive(received: f64, quantity: f64, ordered: f64) -> bool {
    round_quantity(received + quantity) > ordered
}

/// Segregation of duties for approvals: the user who requested
/// a purchase order must never approve it. The module defines
/// no self-approval override permission (only
/// `procurement.view` / `procurement.manage` exist), so
/// self-approval is always rejected.
pub(crate) fn check_not_self_approval(po: &PurchaseOrder, actor: &str) -> UnifiedResult<()> {
    if po.requested_by == actor {
        return Err(UnifiedError::forbidden(
            "The requester of a purchase order cannot approve it; a different approver is required",
        ));
    }
    Ok(())
}

/// Whether any budget references this vendor. Deleting such a
/// vendor would leave budgets with a dangling `vendor_id` that
/// can never match a purchase order again.
pub(crate) fn vendor_has_budgets(state: &ProcurementState, vendor_id: Uuid) -> bool {
    state
        .budgets
        .iter()
        .any(|entry| entry.value().vendor_id == Some(vendor_id))
}

/// Currency: 3-letter ISO code, uppercased.
pub(crate) fn valid_currency(currency: &str) -> UnifiedResult<String> {
    let upper = currency.trim().to_uppercase();
    if upper.len() != 3 || !upper.chars().all(|c| c.is_ascii_uppercase()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code such as \"USD\"",
        ));
    }
    Ok(upper)
}

pub(crate) fn valid_quantity(quantity: f64) -> UnifiedResult<f64> {
    if !quantity.is_finite() || quantity <= 0.0 {
        return Err(UnifiedError::bad_request(
            "quantity must be a finite number > 0",
        ));
    }
    Ok((quantity * 10_000.0).round() / 10_000.0)
}

pub(crate) fn valid_tax_rate(tax_rate: f64) -> UnifiedResult<f64> {
    if !tax_rate.is_finite() || tax_rate < 0.0 || tax_rate > 1.0 {
        return Err(UnifiedError::bad_request(
            "tax_rate must be between 0 and 1 (0.10 = 10%)",
        ));
    }
    Ok((tax_rate * 10_000.0).round() / 10_000.0)
}

pub(crate) fn validate_tags(tags: &[String]) -> UnifiedResult<Vec<String>> {
    if tags.len() > MAX_TAGS {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_TAGS} tags are allowed"
        )));
    }
    let mut out: Vec<String> = Vec::new();
    for tag in tags {
        let cleaned = require_text(tag, "tag", MAX_TAG_LEN)?;
        if out.iter().any(|entry| entry == &cleaned) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate tag \"{cleaned}\""
            )));
        }
        out.push(cleaned);
    }
    Ok(out)
}

pub(crate) fn validate_categories(categories: &[String]) -> UnifiedResult<Vec<String>> {
    if categories.len() > MAX_CATEGORIES_PER_VENDOR {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_CATEGORIES_PER_VENDOR} categories are allowed"
        )));
    }
    let mut out: Vec<String> = Vec::new();
    for category in categories {
        let cleaned = require_text(category, "category", MAX_CATEGORY_LEN)?;
        if out.iter().any(|entry| entry == &cleaned) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate category \"{cleaned}\""
            )));
        }
        out.push(cleaned);
    }
    Ok(out)
}

pub(crate) fn validate_rating(rating: i64) -> UnifiedResult<i64> {
    if !(1..=5).contains(&rating) {
        return Err(UnifiedError::bad_request("rating must be between 1 and 5"));
    }
    Ok(rating)
}

pub(crate) fn load_vendor(state: &ProcurementState, raw_id: &str) -> UnifiedResult<Vendor> {
    let id = parse_uuid(raw_id, "vendor")?;
    state
        .vendors
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Vendor not found: {id}")))
}

pub(crate) fn load_po(state: &ProcurementState, raw_id: &str) -> UnifiedResult<PurchaseOrder> {
    let id = parse_uuid(raw_id, "purchase order")?;
    state
        .purchase_orders
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Purchase order not found: {id}")))
}

pub(crate) fn load_budget(
    state: &ProcurementState,
    raw_id: &str,
) -> UnifiedResult<ProcurementBudget> {
    let id = parse_uuid(raw_id, "budget")?;
    state
        .budgets
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Budget not found: {id}")))
}

pub(crate) fn lines_of(state: &ProcurementState, po_id: Uuid) -> Vec<PurchaseOrderLine> {
    let mut lines: Vec<PurchaseOrderLine> = state
        .lines
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|line| line.purchase_order_id == po_id)
        .collect();
    lines.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
    lines
}

pub(crate) fn load_line(
    state: &ProcurementState,
    po_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<PurchaseOrderLine> {
    let id = parse_uuid(raw_id, "purchase order line")?;
    state
        .lines
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|line| line.purchase_order_id == po_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Purchase order line not found: {id}")))
}

pub(crate) fn approvals_of(state: &ProcurementState, po_id: Uuid) -> Vec<PurchaseOrderApproval> {
    let mut approvals: Vec<PurchaseOrderApproval> = state
        .approvals
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|approval| approval.purchase_order_id == po_id)
        .collect();
    approvals.sort_by(|a, b| {
        a.decided_at.cmp(&b.decided_at).then_with(|| a.id.cmp(&b.id))
    });
    approvals
}

pub(crate) fn receipts_of_po(state: &ProcurementState, po_id: Uuid) -> Vec<GoodsReceipt> {
    let mut receipts: Vec<GoodsReceipt> = state
        .receipts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|receipt| receipt.purchase_order_id == po_id)
        .collect();
    receipts.sort_by(|a, b| {
        a.received_at.cmp(&b.received_at).then_with(|| a.id.cmp(&b.id))
    });
    receipts
}

/// Recompute a PO's money from its lines. The PO total is the
/// sum of the rounded `line_total` values (each line is
/// `line_total_minor(quantity, unit_price, tax_rate)`), so the
/// displayed lines always add up to the PO total. The subtotal is
/// the sum of the rounded per-line net amounts and the tax total
/// is the difference, so subtotal + tax total always equals the
/// total. All sums are exact integer minor units. The result is
/// stored back on the PO row.
pub(crate) fn recompute_po_totals(state: &ProcurementState, po: &mut PurchaseOrder) {
    let mut subtotal: i64 = 0;
    let mut total: i64 = 0;
    for line in lines_of(state, po.id) {
        subtotal += line_net_minor(line.quantity, line.unit_price);
        total += line.line_total;
    }
    po.subtotal = subtotal;
    po.total = total;
    po.tax_total = po.total - po.subtotal;
}

/// Derive the receipt status of a PO from its lines: no receipts
/// yet keeps the current status; some lines partially received
/// (or some fully received while others are not) means
/// `partially_received`; every line fully received means
/// `received` (and stamps `received_at`).
pub(crate) fn derive_receipt_status(state: &ProcurementState, po: &mut PurchaseOrder) {
    let lines = lines_of(state, po.id);
    if lines.is_empty() {
        return;
    }
    let any_received = lines.iter().any(|line| line.received_quantity > 0.0);
    if !any_received {
        return;
    }
    let all_received = lines
        .iter()
        .all(|line| line.received_quantity >= line.quantity);
    let now = Utc::now();
    if all_received {
        po.status = PurchaseOrderStatus::Received;
        if po.received_at.is_none() {
            po.received_at = Some(now);
        }
    } else {
        po.status = PurchaseOrderStatus::PartiallyReceived;
    }
    po.updated_at = now;
}

/// Validate a PO status transition. Returns a 409 conflict
/// naming the illegal move.
pub(crate) fn transition(
    po: &PurchaseOrder,
    target: PurchaseOrderStatus,
    action: &str,
) -> UnifiedResult<PurchaseOrderStatus> {
    let allowed = match po.status {
        PurchaseOrderStatus::Draft => [
            PurchaseOrderStatus::PendingApproval,
            PurchaseOrderStatus::Cancelled,
        ]
        .contains(&target),
        PurchaseOrderStatus::PendingApproval => [
            PurchaseOrderStatus::Approved,
            PurchaseOrderStatus::Draft,
            PurchaseOrderStatus::Cancelled,
        ]
        .contains(&target),
        PurchaseOrderStatus::Approved => {
            [PurchaseOrderStatus::Ordered, PurchaseOrderStatus::Cancelled].contains(&target)
        }
        PurchaseOrderStatus::Ordered => [
            PurchaseOrderStatus::PartiallyReceived,
            PurchaseOrderStatus::Received,
            PurchaseOrderStatus::Cancelled,
        ]
        .contains(&target),
        PurchaseOrderStatus::PartiallyReceived => [
            PurchaseOrderStatus::Received,
            PurchaseOrderStatus::Closed,
            PurchaseOrderStatus::Cancelled,
        ]
        .contains(&target),
        PurchaseOrderStatus::Received => [PurchaseOrderStatus::Closed].contains(&target),
        PurchaseOrderStatus::Closed | PurchaseOrderStatus::Cancelled => false,
    };
    if !allowed {
        return Err(UnifiedError::conflict(format!(
            "Cannot {action} a purchase order in status \"{}\" (target \"{}\")",
            po.status.as_str(),
            target.as_str(),
        )));
    }
    Ok(target)
}

/// A PO counts as committed spend once approved (and later):
/// draft, pending approval, and cancelled POs never count.
pub(crate) fn is_committed(status: PurchaseOrderStatus) -> bool {
    matches!(
        status,
        PurchaseOrderStatus::Approved
            | PurchaseOrderStatus::Ordered
            | PurchaseOrderStatus::PartiallyReceived
            | PurchaseOrderStatus::Received
            | PurchaseOrderStatus::Closed
    )
}

/// Committed spend for one scope inside a date window. `from` /
/// `to` bound the PO creation date; `category` and `vendor_id`
/// narrow the scope (either may be `None` for "any"). The result
/// is integer minor units; the sum is exact.
pub(crate) fn committed_spend(
    state: &ProcurementState,
    category: Option<&str>,
    vendor_id: Option<Uuid>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> i64 {
    let mut total: i64 = 0;
    for entry in state.purchase_orders.iter() {
        let po = entry.value();
        if !is_committed(po.status) {
            continue;
        }
        if let Some(category) = category {
            if po.category.as_deref() != Some(category) {
                continue;
            }
        }
        if let Some(vendor_id) = vendor_id {
            if po.vendor_id != vendor_id {
                continue;
            }
        }
        let created = po.created_at.date_naive();
        if let Some(from) = from {
            if created < from {
                continue;
            }
        }
        if let Some(to) = to {
            if created > to {
                continue;
            }
        }
        total += po.total;
    }
    total
}

/// Budgets that apply to a PO: a budget applies when its
/// category matches the PO category (or the budget has no
/// category) AND its vendor matches the PO vendor (or the budget
/// has no vendor), and the PO creation date falls inside the
/// budget period.
pub(crate) fn applicable_budgets(
    state: &ProcurementState,
    po: &PurchaseOrder,
) -> Vec<ProcurementBudget> {
    let created = po.created_at.date_naive();
    state
        .budgets
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|budget| {
            if let Some(category) = &budget.category {
                if po.category.as_deref() != Some(category.as_str()) {
                    return false;
                }
            }
            if let Some(vendor_id) = budget.vendor_id {
                if po.vendor_id != vendor_id {
                    return false;
                }
            }
            created >= budget.period_start && created <= budget.period_end
        })
        .collect()
}

/// Over-budget warnings for one PO: every applicable budget whose
/// committed spend already exceeds its amount. Advisory only --
/// the caller never blocks on this. Money in the payload is major
/// units, as before, converted per budget currency at this
/// boundary.
pub(crate) fn budget_warnings(state: &ProcurementState, po: &PurchaseOrder) -> Vec<Value> {
    let mut warnings: Vec<Value> = Vec::new();
    for budget in applicable_budgets(state, po) {
        let spent = committed_spend(
            state,
            budget.category.as_deref(),
            budget.vendor_id,
            Some(budget.period_start),
            Some(budget.period_end),
        );
        if spent > budget.budget_amount {
            warnings.push(json!({
                "budget_id": budget.id.to_string(),
                "category": budget.category,
                "vendor_id": budget.vendor_id.map(|id| id.to_string()),
                "budget_amount": minor_to_major(budget.budget_amount, &budget.currency),
                "spent": minor_to_major(spent, &budget.currency),
                "over_by": minor_to_major(spent - budget.budget_amount, &budget.currency),
                "currency": budget.currency,
            }));
        }
    }
    warnings
}

/// Emit a `procurement.budget.warning` event and a notification
/// for each over-budget warning. Advisory only.
pub(crate) fn warn_over_budget(state: &ProcurementState, po: &PurchaseOrder, warnings: &[Value]) {
    for warning in warnings {
        state.emit(
            "procurement.budget.warning",
            json!({
                "purchase_order_id": po.id.to_string(),
                "po_number": po.po_number,
                "budget_id": warning.get("budget_id"),
                "spent": warning.get("spent"),
                "budget_amount": warning.get("budget_amount"),
                "over_by": warning.get("over_by"),
            }),
        );
        state.notify(
            "procurement.budget_warning",
            &po.requested_by,
            &format!("Over budget: purchase order {}", po.po_number),
            &format!(
                "Purchase order {} ({}) pushes committed spend to {} against a budget of {} {}.",
                po.po_number,
                po.title,
                warning.get("spent").and_then(Value::as_f64).unwrap_or(0.0),
                warning
                    .get("budget_amount")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
                warning
                    .get("currency")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            ),
            Some(format!("/api/v1/procurement/purchase-orders/{}", po.id)),
        );
    }
}

/// Public view of a vendor.
pub(crate) fn vendor_view(vendor: &Vendor) -> Value {
    json!(vendor)
}

/// Public view of a purchase order, with its lines embedded and
/// any current over-budget warnings attached. Money is major
/// units, as before, converted per PO currency at this boundary;
/// lines serialise through `serde_major` to the same shape.
pub(crate) fn po_view(state: &ProcurementState, po: &PurchaseOrder) -> Value {
    let lines: Vec<Value> = lines_of(state, po.id)
        .into_iter()
        .map(|line| json!(line))
        .collect();
    let warnings = if is_committed(po.status) {
        budget_warnings(state, po)
    } else {
        Vec::new()
    };
    json!({
        "id": po.id.to_string(),
        "po_number": po.po_number,
        "vendor_id": po.vendor_id.to_string(),
        "title": po.title,
        "category": po.category,
        "currency": po.currency,
        "status": po.status.as_str(),
        "subtotal": minor_to_major(po.subtotal, &po.currency),
        "tax_total": minor_to_major(po.tax_total, &po.currency),
        "total": minor_to_major(po.total, &po.currency),
        "promised_date": po.promised_date.map(|date| date.to_string()),
        "ordered_at": po.ordered_at.map(|value| value.to_rfc3339()),
        "received_at": po.received_at.map(|value| value.to_rfc3339()),
        "cancel_reason": po.cancel_reason,
        "requested_by": po.requested_by,
        "approved_by": po.approved_by,
        "lines": lines,
        "budget_warnings": warnings,
        "created_at": po.created_at.to_rfc3339(),
        "updated_at": po.updated_at.to_rfc3339(),
    })
}

/// Public view of a budget, with spend vs budget computed on read.
/// Money is major units, as before, converted per budget currency
/// at this boundary.
pub(crate) fn budget_view(state: &ProcurementState, budget: &ProcurementBudget) -> Value {
    let spent = committed_spend(
        state,
        budget.category.as_deref(),
        budget.vendor_id,
        Some(budget.period_start),
        Some(budget.period_end),
    );
    let remaining = budget.budget_amount - spent;
    json!({
        "id": budget.id.to_string(),
        "category": budget.category,
        "vendor_id": budget.vendor_id.map(|id| id.to_string()),
        "period_start": budget.period_start.to_string(),
        "period_end": budget.period_end.to_string(),
        "budget_amount": minor_to_major(budget.budget_amount, &budget.currency),
        "currency": budget.currency,
        "spent": minor_to_major(spent, &budget.currency),
        "remaining": minor_to_major(remaining, &budget.currency),
        "over_budget": spent > budget.budget_amount,
        "created_by": budget.created_by,
        "created_at": budget.created_at.to_rfc3339(),
        "updated_at": budget.updated_at.to_rfc3339(),
    })
}

// Tests for the pure validation and state helpers in this file.
#[cfg(test)]
mod tests {
    use super::*;

    fn sample_po(status: PurchaseOrderStatus) -> PurchaseOrder {
        PurchaseOrder {
            id: Uuid::from_u128(1),
            po_number: "PO-1".to_string(),
            vendor_id: Uuid::from_u128(2),
            title: "Test order".to_string(),
            category: None,
            currency: "USD".to_string(),
            status,
            subtotal: 0,
            tax_total: 0,
            total: 0,
            promised_date: None,
            ordered_at: None,
            received_at: None,
            cancel_reason: None,
            requested_by: "tester".to_string(),
            approved_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn email_validation_rejects_common_bad_forms() {
        assert!(valid_email("a@b.com"));
        assert!(!valid_email("no-at"));
        assert!(!valid_email("a@b"));
        assert!(!valid_email("a@.b.com"));
        assert!(!valid_email("a b@c.com"));
        assert!(!valid_email("@b.com"));
    }

    #[test]
    fn non_negative_minor_amounts_are_required() {
        assert_eq!(require_non_negative_minor(0, "unit_price").unwrap(), 0);
        assert_eq!(require_non_negative_minor(123, "unit_price").unwrap(), 123);
        assert!(require_non_negative_minor(-1, "unit_price").is_err());
    }

    #[test]
    fn minor_to_major_converts_cents_and_zero_decimal_currencies() {
        assert_eq!(minor_to_major(10050, "USD"), 100.5);
        assert_eq!(minor_to_major(1013, "USD"), 10.13);
        assert_eq!(minor_to_major(-1013, "USD"), -10.13);
        assert_eq!(minor_to_major(500, "JPY"), 500.0);
    }

    #[test]
    fn quantity_rounding_keeps_four_decimals() {
        assert_eq!(round_quantity(1.23456), 1.2346);
        assert_eq!(round_quantity(2.5), 2.5);
        assert_eq!(round_quantity(0.00005), 0.0001);
    }

    #[test]
    fn line_totals_round_half_away_from_zero_to_minor_units() {
        // 3 x 3.33 at 10%: 999 * 1.1 = 1098.9 -> 1099 minor.
        assert_eq!(line_total_minor(3.0, 333, 0.10), 1099);
        assert_eq!(line_net_minor(3.0, 333), 999);
        // 105 minor at 10% is 10.5 minor: the old float math
        // rounded 0.105 major to 0.11, i.e. 11 minor units.
        assert_eq!(line_total_minor(1.0, 105, 0.10), 116);
        assert_eq!(line_net_minor(1.0, 105), 105);
        // Fractional quantity: half a unit at 3.33 nets 167 minor.
        assert_eq!(line_net_minor(0.5, 333), 167);
    }

    #[test]
    fn currency_is_normalized_to_uppercase_iso_code() {
        assert_eq!(valid_currency("usd").unwrap(), "USD");
        assert!(valid_currency("US").is_err());
        assert!(valid_currency("USDD").is_err());
        assert!(valid_currency("u5d").is_err());
    }

    #[test]
    fn quantity_must_be_positive() {
        assert!(valid_quantity(0.0).is_err());
        assert!(valid_quantity(-1.0).is_err());
        assert_eq!(valid_quantity(2.5).unwrap(), 2.5);
    }

    #[test]
    fn tax_rate_is_bounded_between_zero_and_one() {
        assert!(valid_tax_rate(1.5).is_err());
        assert!(valid_tax_rate(-0.1).is_err());
        assert_eq!(valid_tax_rate(0.1).unwrap(), 0.1);
        assert_eq!(valid_tax_rate(1.0).unwrap(), 1.0);
    }

    #[test]
    fn tags_are_trimmed_deduplicated_and_capped() {
        assert_eq!(
            validate_tags(&[" b ".to_string(), "a".to_string()]).unwrap(),
            vec!["b".to_string(), "a".to_string()]
        );
        assert!(validate_tags(&["a".to_string(), "a".to_string()]).is_err());
        assert!(validate_tags(&["".to_string()]).is_err());
        let many: Vec<String> = (0..21).map(|i| format!("tag{i}")).collect();
        assert!(validate_tags(&many).is_err());
    }

    #[test]
    fn categories_reject_empty_and_duplicate_values() {
        assert!(validate_categories(&["a".to_string(), "".to_string()]).is_err());
        assert!(validate_categories(&["a".to_string(), "a".to_string()]).is_err());
        assert_eq!(
            validate_categories(&["x".to_string()]).unwrap(),
            vec!["x".to_string()]
        );
    }

    #[test]
    fn rating_is_bounded() {
        assert!(validate_rating(0).is_err());
        assert!(validate_rating(6).is_err());
        assert_eq!(validate_rating(3).unwrap(), 3);
    }

    #[test]
    fn transition_enforces_the_status_machine() {
        let draft = sample_po(PurchaseOrderStatus::Draft);
        assert_eq!(
            transition(&draft, PurchaseOrderStatus::PendingApproval, "submit").unwrap(),
            PurchaseOrderStatus::PendingApproval
        );
        assert!(transition(&draft, PurchaseOrderStatus::Approved, "approve").is_err());

        let closed = sample_po(PurchaseOrderStatus::Closed);
        assert!(transition(&closed, PurchaseOrderStatus::Approved, "approve").is_err());
    }

    #[test]
    fn committed_spend_excludes_draft_pending_and_cancelled() {
        assert!(!is_committed(PurchaseOrderStatus::Draft));
        assert!(!is_committed(PurchaseOrderStatus::PendingApproval));
        assert!(!is_committed(PurchaseOrderStatus::Cancelled));
        assert!(is_committed(PurchaseOrderStatus::Approved));
        assert!(is_committed(PurchaseOrderStatus::Ordered));
        assert!(is_committed(PurchaseOrderStatus::Closed));
    }

    #[test]
    fn money_fields_round_trip_as_major_units_in_json() {
        // Money is minor units in Rust but major units on the wire.
        let line: PurchaseOrderLine = serde_json::from_value(serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "purchase_order_id": "00000000-0000-0000-0000-000000000001",
            "description": "Widget",
            "quantity": 2.0,
            "unit_price": 10.25,
            "tax_rate": 0.10,
            "line_total": 22.55,
            "received_quantity": 0.0,
            "created_at": "2026-11-22T00:00:00Z",
            "updated_at": "2026-11-22T00:00:00Z",
        }))
        .unwrap();
        assert_eq!(line.unit_price, 1025);
        assert_eq!(line.line_total, 2255);
        let json = serde_json::to_value(&line).unwrap();
        assert_eq!(json["unit_price"], serde_json::json!(10.25));
        assert_eq!(json["line_total"], serde_json::json!(22.55));
        // A request total rounds half away from zero to minor units.
        let request: CreateLineRequest =
            serde_json::from_str(r#"{"description":"Widget","quantity":1.0,"unit_price":10.005}"#)
                .unwrap();
        assert_eq!(request.unit_price, 1001);
    }

    #[test]
    fn exact_fill_receipt_is_not_over_receipt() {
        // Raw f64 addition reproduces the reported bug: an exact
        // fill of 0.1 + 0.2 against an order of 0.3 compares
        // greater. The rounded check must accept it.
        assert!((0.1 + 0.2) > 0.3);
        assert!(!would_over_receive(0.1, 0.2, 0.3));
        assert!(!would_over_receive(0.0, 0.3, 0.3));
        assert!(would_over_receive(0.1, 0.21, 0.3));
        assert!(would_over_receive(0.0, 0.3001, 0.3));
        assert!(would_over_receive(0.2, 0.2, 0.3));
    }

    #[test]
    fn self_approval_is_rejected_but_others_may_approve() {
        let po = sample_po(PurchaseOrderStatus::PendingApproval);
        assert_eq!(po.requested_by, "tester");
        assert!(check_not_self_approval(&po, "tester").is_err());
        assert!(check_not_self_approval(&po, "another-approver").is_ok());
    }

    #[test]
    fn ordered_and_partially_received_pos_can_be_cancelled() {
        let ordered = sample_po(PurchaseOrderStatus::Ordered);
        assert_eq!(
            transition(&ordered, PurchaseOrderStatus::Cancelled, "cancel").unwrap(),
            PurchaseOrderStatus::Cancelled
        );
        let partial = sample_po(PurchaseOrderStatus::PartiallyReceived);
        assert_eq!(
            transition(&partial, PurchaseOrderStatus::Cancelled, "cancel").unwrap(),
            PurchaseOrderStatus::Cancelled
        );
        // A fully received order still has no cancel path: it
        // must be closed instead.
        let received = sample_po(PurchaseOrderStatus::Received);
        assert!(transition(&received, PurchaseOrderStatus::Cancelled, "cancel").is_err());
        let closed = sample_po(PurchaseOrderStatus::Closed);
        assert!(transition(&closed, PurchaseOrderStatus::Cancelled, "cancel").is_err());
    }

    fn sample_line(
        id: u128,
        po_id: Uuid,
        quantity: f64,
        unit_price: i64,
        tax_rate: f64,
        created_at: chrono::DateTime<Utc>,
    ) -> PurchaseOrderLine {
        PurchaseOrderLine {
            id: Uuid::from_u128(id),
            purchase_order_id: po_id,
            description: "Test line".to_string(),
            quantity,
            unit_price,
            tax_rate,
            line_total: line_total_minor(quantity, unit_price, tax_rate),
            received_quantity: 0.0,
            created_at,
            updated_at: created_at,
        }
    }

    #[test]
    fn po_total_equals_sum_of_rounded_line_totals() {
        let state = ProcurementState::default();
        let mut po = sample_po(PurchaseOrderStatus::Draft);
        state.purchase_orders.insert(po.id, po.clone());
        // Two identical half-unit lines whose per-line rounding
        // differs from rounding the unrounded aggregate: each line
        // nets 167 minor (0.5 x 333), so the lines sum to 334,
        // while rounding the aggregate 333 minor would give 333.
        let now = Utc::now();
        for id in [10u128, 11u128] {
            state
                .lines
                .insert(Uuid::from_u128(id), sample_line(id, po.id, 0.5, 333, 0.0, now));
        }
        recompute_po_totals(&state, &mut po);
        let line_sum: i64 = lines_of(&state, po.id)
            .iter()
            .map(|line| line.line_total)
            .sum();
        assert_eq!(line_sum, 334);
        assert_eq!(po.total, line_sum);
        assert_eq!(po.total, 334);
        assert_eq!(po.subtotal + po.tax_total, po.total);
    }

    #[test]
    fn vendor_with_budgets_is_flagged_for_delete_block() {
        let state = ProcurementState::default();
        let vendor_id = Uuid::from_u128(42);
        let other_vendor_id = Uuid::from_u128(43);
        let now = Utc::now();
        let budget = ProcurementBudget {
            id: Uuid::from_u128(7),
            category: None,
            vendor_id: Some(vendor_id),
            period_start: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            period_end: NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            budget_amount: 100000,
            currency: "USD".to_string(),
            created_by: "tester".to_string(),
            created_at: now,
            updated_at: now,
        };
        state.budgets.insert(budget.id, budget);
        assert!(vendor_has_budgets(&state, vendor_id));
        assert!(!vendor_has_budgets(&state, other_vendor_id));
        // A budget with no vendor scope blocks no vendor.
        let unscoped = ProcurementBudget {
            id: Uuid::from_u128(8),
            category: Some("office".to_string()),
            vendor_id: None,
            period_start: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            period_end: NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            budget_amount: 50000,
            currency: "USD".to_string(),
            created_by: "tester".to_string(),
            created_at: now,
            updated_at: now,
        };
        state.budgets.insert(unscoped.id, unscoped);
        assert!(!vendor_has_budgets(&state, other_vendor_id));
    }

    #[test]
    fn equal_timestamp_lines_order_by_id_deterministically() {
        let state = ProcurementState::default();
        let po_id = Uuid::from_u128(1);
        let now = Utc::now();
        // Insert out of id order, all with the same timestamp.
        for id in [30u128, 10u128, 20u128] {
            state
                .lines
                .insert(Uuid::from_u128(id), sample_line(id, po_id, 1.0, 100, 0.0, now));
        }
        let ordered = lines_of(&state, po_id);
        let ids: Vec<u128> = ordered.iter().map(|line| line.id.as_u128()).collect();
        assert_eq!(ids, vec![10u128, 20u128, 30u128]);
        // Repeat: the order must not depend on map iteration.
        let again = lines_of(&state, po_id);
        let ids_again: Vec<u128> = again.iter().map(|line| line.id.as_u128()).collect();
        assert_eq!(ids, ids_again);
    }

    #[test]
    fn equal_timestamp_approvals_and_receipts_order_by_id_deterministically() {
        let state = ProcurementState::default();
        let po_id = Uuid::from_u128(1);
        let now = Utc::now();
        for id in [30u128, 10u128, 20u128] {
            state.approvals.insert(
                Uuid::from_u128(id),
                PurchaseOrderApproval {
                    id: Uuid::from_u128(id),
                    purchase_order_id: po_id,
                    approver_id: "approver".to_string(),
                    approver_role: "manager".to_string(),
                    decision: ApprovalDecision::Approved,
                    reason: None,
                    decided_at: now,
                },
            );
            state.receipts.insert(
                Uuid::from_u128(id),
                GoodsReceipt {
                    id: Uuid::from_u128(id),
                    purchase_order_id: po_id,
                    line_id: Uuid::from_u128(99),
                    quantity_received: 1.0,
                    quality_note: None,
                    received_by: "receiver".to_string(),
                    received_at: now,
                    created_at: now,
                },
            );
        }
        let approval_ids: Vec<u128> = approvals_of(&state, po_id)
            .iter()
            .map(|approval| approval.id.as_u128())
            .collect();
        assert_eq!(approval_ids, vec![10u128, 20u128, 30u128]);
        let receipt_ids: Vec<u128> = receipts_of_po(&state, po_id)
            .iter()
            .map(|receipt| receipt.id.as_u128())
            .collect();
        assert_eq!(receipt_ids, vec![10u128, 20u128, 30u128]);
    }

    /// Build a request carrying a resolved tenant context plus a
    /// verified caller, for the tenant-gate tests below.
    fn tenant_request(company_id: &str, caller_company: Option<&str>) -> HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        let verified = match caller_company {
            Some(company) => unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
                .with_company(company.to_string()),
            None => unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new()),
        };
        req.extensions_mut().insert(verified);
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: company_id.to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn caller_company_prefers_tenant_company() {
        let company = uuid::Uuid::now_v7().to_string();
        let req = tenant_request(&company, Some(&company));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some(company.clone())
        );
        // No tenant company: the verified caller's company is the scope.
        let req = tenant_request("", Some(&company));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(caller_company(&tenant, &req).unwrap(), Some(company));
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = tenant_request(
            &uuid::Uuid::now_v7().to_string(),
            Some(&uuid::Uuid::now_v7().to_string()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn require_caller_company_refuses_writes_without_a_resolved_company() {
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(require_caller_company(&tenant, &req).is_err());
        let company = uuid::Uuid::now_v7().to_string();
        let req = tenant_request(&company, Some(&company));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(require_caller_company(&tenant, &req).unwrap(), company);
    }
}
