// Library - Member, loan and overdue handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;

use super::{COPY_AVAILABLE, COPY_DAMAGED, COPY_LOST, COPY_ON_LOAN, COPY_RESERVED, CreateLoanRequest,
    CreateMemberRequest, FINE_PENDING, HOLD_READY, ITEM_ACTIVE, LOAN_ACTIVE,
    LOAN_DAMAGED, LOAN_LOST, LOAN_OVERDUE, LOAN_RETURNED, LibraryFine, LibraryLoan, LibraryMember,
    LibraryState, MAX_NOTE_LEN, MAX_REF_LEN, MAX_SLUG_LEN, MEMBER_ACTIVE,
    MEMBER_SUSPENDED, MarkLoanRequest, PERM_LIBRARY_CIRCULATE, PERM_LIBRARY_MANAGE,
    PERM_LIBRARY_VIEW, UpdateMemberRequest, caller, check_capability, create_fine_for_loan, created,
    err, first_queued_hold, load_item, load_loan, load_member, offer_to_next_hold, ok,
    optional_text, overdue_fine_minor, paginate, parse_datetime, parse_uuid, require_circulate,
    require_currency, require_in_list, require_non_negative, require_permission, require_tenant,
    require_text, resolve_policy};

// ---------------------------------------------------------------------------
// Member handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_members(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let params = query.into_inner();
        let pagination = params.normalized();
        let filters = pagination.filters.clone();
        let mut items: Vec<LibraryMember> = state
            .members
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items.retain(|member| super::auth::in_company_scope(&member.company_id, &company));
        if let Some(member_type) = filters.get("member_type") {
            items.retain(|member| &member.member_type == member_type);
        }
        if let Some(status) = filters.get("status") {
            items.retain(|member| &member.status == status);
        }
        if let Some(user_id) = filters.get("user_id") {
            items.retain(|member| &member.user_id == user_id);
        }
        items.sort_by_key(|member| member.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_member(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    body: web::Json<CreateMemberRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryMember> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let input = body.into_inner();
        let company_id = super::auth::require_company_match_for(&company, &input.company_id)?;
        let settings = state.settings();
        let user_id = require_text(&input.user_id, "user_id", MAX_REF_LEN)?;
        let member_type = require_in_list(&input.member_type, "member_type", &settings.member_types)?;
        let expires_at = match &input.expires_at {
            Some(raw) => Some(parse_datetime(raw, "expires_at")?),
            None => None,
        };
        let notes = optional_text(&input.notes, "notes", MAX_NOTE_LEN)?;
        // Atomic duplicate guard: the scan and the insert happen
        // under the user id's registration lock (see
        // `insert_member_once`), so two concurrent registrations
        // for one user cannot both create an active membership.
        let member = super::service::insert_member_once(
            &state, user_id, member_type, expires_at, notes, company_id,
        )?;
        state.emit(
            "library.member.created",
            json!({
                "member_id": member.id.to_string(),
                "user_id": member.user_id,
                "member_type": member.member_type,
                "actor": caller.actor,
            }),
        );
        Ok(member)
    })();
    match result {
        Ok(member) => created(member),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_member(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryMember> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let member = load_member(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&member.company_id, &company)?;
        if member.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        }
        Ok(member)
    })();
    match result {
        Ok(member) => ok(member),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_member(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateMemberRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryMember> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut member = load_member(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&member.company_id, &company)?;
        let settings = state.settings();
        if let Some(member_type) = &input.member_type {
            member.member_type = require_in_list(member_type, "member_type", &settings.member_types)?;
        }
        if let Some(status) = &input.status {
            let checked = require_text(status, "status", MAX_SLUG_LEN)?;
            if !["active", "suspended", "expired"].contains(&checked.as_str()) {
                return Err(UnifiedError::bad_request(
                    "status must be one of: active, suspended, expired",
                ));
            }
            member.status = checked;
        }
        if let Some(expires_at) = &input.expires_at {
            member.expires_at = match expires_at {
                Some(raw) => Some(parse_datetime(raw, "expires_at")?),
                None => None,
            };
        }
        if let Some(notes) = &input.notes {
            member.notes = match notes {
                Some(value) => Some(require_text(value, "notes", MAX_NOTE_LEN)?),
                None => None,
            };
        }
        member.updated_at = Utc::now();
        state.members.insert(member.id, member.clone());
        let event = if member.status == MEMBER_SUSPENDED {
            "library.member.suspended"
        } else {
            "library.member.updated"
        };
        state.emit(
            event,
            json!({
                "member_id": member.id.to_string(),
                "user_id": member.user_id,
                "status": member.status,
                "actor": caller.actor,
            }),
        );
        Ok(member)
    })();
    match result {
        Ok(member) => ok(member),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_member(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        let member = load_member(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&member.company_id, &company)?;
        let active_loan = state.loans.iter().any(|entry| {
            entry.value().member_id == member.id
                && (entry.value().status == LOAN_ACTIVE || entry.value().status == LOAN_OVERDUE)
        });
        if active_loan {
            return Err(UnifiedError::conflict(
                "A member with active loans cannot be deleted; suspend the membership instead",
            ));
        }
        let pending_fine = state.fines.iter().any(|entry| {
            entry.value().member_id == member.id && entry.value().status == FINE_PENDING
        });
        if pending_fine {
            return Err(UnifiedError::conflict(
                "A member with pending fines cannot be deleted; resolve the fines first",
            ));
        }
        state.members.remove(&member.id);
        Ok(json!({ "deleted": true, "member_id": member.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_member_loans(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let member = load_member(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&member.company_id, &company)?;
        if member.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        }
        let params = query.into_inner();
        let pagination = params.normalized();
        let filters = pagination.filters.clone();
        let mut items: Vec<LibraryLoan> = state
            .loans
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|loan| loan.member_id == member.id)
            .collect();
        items.retain(|loan| super::auth::in_company_scope(&loan.company_id, &company));
        if let Some(status) = filters.get("status") {
            items.retain(|loan| &loan.status == status);
        }
        items.sort_by_key(|loan| std::cmp::Reverse(loan.checked_out_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn member_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let member = load_member(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&member.company_id, &company)?;
        if member.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        }
        let params = query.into_inner();
        let mut loans: Vec<LibraryLoan> = state
            .loans
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|loan| loan.member_id == member.id)
            .collect();
        loans.sort_by_key(|loan| std::cmp::Reverse(loan.checked_out_at));
        let page = paginate(loans.clone(), &params);
        let total_loans = loans.len() as i64;
        let returned = loans
            .iter()
            .filter(|loan| loan.status == LOAN_RETURNED)
            .count() as i64;
        let overdue_now = loans
            .iter()
            .filter(|loan| loan.status == LOAN_OVERDUE)
            .count() as i64;
        let fines: Vec<LibraryFine> = state
            .fines
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|fine| fine.member_id == member.id)
            .collect();
        let pending_fine_minor: i64 = fines
            .iter()
            .filter(|fine| fine.status == FINE_PENDING)
            .map(|fine| fine.amount_minor)
            .sum();
        Ok(json!({
            "member": member,
            "loans": page,
            "total_loans": total_loans,
            "returned_loans": returned,
            "overdue_loans": overdue_now,
            "pending_fine_minor": pending_fine_minor,
            "fines": fines,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_my_loans(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let params = query.into_inner();
        let pagination = params.normalized();
        let filters = pagination.filters.clone();
        let mut items: Vec<LibraryLoan> = state
            .loans
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|loan| loan.user_id == caller.actor)
            .collect();
        items.retain(|loan| super::auth::in_company_scope(&loan.company_id, &company));
        if let Some(status) = filters.get("status") {
            items.retain(|loan| &loan.status == status);
        }
        items.sort_by_key(|loan| std::cmp::Reverse(loan.checked_out_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Loan handlers
// ---------------------------------------------------------------------------

pub(crate) async fn create_loan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    body: web::Json<CreateLoanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryLoan> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let input = body.into_inner();
        let member = load_member(&state, &input.member_id)?;
        super::auth::require_record_in_company_scope(&member.company_id, &company)?;
        if member.status != MEMBER_ACTIVE {
            return Err(UnifiedError::conflict(format!(
                "Member \"{}\" is not active (status: {})",
                member.id, member.status
            )));
        }
        if let Some(expires_at) = member.expires_at {
            if expires_at <= Utc::now() {
                return Err(UnifiedError::conflict(
                    "The membership has expired; renew the membership first",
                ));
            }
        }
        let item = load_item(&state, &input.item_id)?;
        super::auth::require_record_in_company_scope(&item.company_id, &company)?;
        if item.status != ITEM_ACTIVE {
            return Err(UnifiedError::conflict("The item is withdrawn and cannot be loaned"));
        }
        let policy = resolve_policy(&state, &member.member_type, &item.item_type)?;
        let settings = state.settings();
        if settings.block_checkout_when_overdue {
            let has_overdue = state.loans.iter().any(|entry| {
                entry.value().member_id == member.id && entry.value().status == LOAN_OVERDUE
            });
            if has_overdue {
                return Err(UnifiedError::conflict(
                    "The member has an overdue loan and cannot check out until it is resolved",
                ));
            }
        }
        let active_count = state
            .loans
            .iter()
            .filter(|entry| {
                entry.value().member_id == member.id
                    && (entry.value().status == LOAN_ACTIVE
                        || entry.value().status == LOAN_OVERDUE)
            })
            .count() as i64;
        if active_count >= policy.max_concurrent {
            return Err(UnifiedError::conflict(format!(
                "The member already has {} active loans, the policy maximum for this member type",
                policy.max_concurrent
            )));
        }
        let already_has_item = state.loans.iter().any(|entry| {
            entry.value().member_id == member.id
                && entry.value().item_id == item.id
                && (entry.value().status == LOAN_ACTIVE || entry.value().status == LOAN_OVERDUE)
        });
        if already_has_item {
            return Err(UnifiedError::conflict(
                "The member already has an active loan for this item",
            ));
        }
        let is_digital = item.digital_ref.is_some()
            || item.document_ref.is_some()
            || item.storage_ref.is_some();
        let copy_id = match &input.copy_id {
            Some(raw) => Some(parse_uuid(raw, "copy")?),
            None => None,
        };
        if let Some(copy_uuid) = copy_id {
            let copy = state
                .copies
                .get(&copy_uuid)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::not_found(format!("Copy \"{copy_uuid}\" not found")))?;
            if copy.item_id != item.id {
                return Err(UnifiedError::bad_request(
                    "The copy does not belong to the requested item",
                ));
            }
            if copy.status == COPY_ON_LOAN {
                return Err(UnifiedError::conflict(
                    "The copy is already on loan",
                ));
            }
            if copy.status == COPY_RESERVED {
                let held_for_member = state.holds.iter().any(|entry| {
                    entry.value().member_id == member.id
                        && entry.value().item_id == item.id
                        && entry.value().status == HOLD_READY
                });
                if !held_for_member {
                    return Err(UnifiedError::conflict(
                        "The copy is reserved for another member",
                    ));
                }
            }
            if copy.status != COPY_AVAILABLE && copy.status != COPY_RESERVED {
                return Err(UnifiedError::conflict(format!(
                    "The copy cannot be loaned (status: {})",
                    copy.status
                )));
            }
        } else if !is_digital {
            return Err(UnifiedError::bad_request(
                "copy_id is required for a physical item; digital items loan without a copy",
            ));
        }
        // A hold queue blocks checkout for anyone who is not next.
        let queued = first_queued_hold(&state, item.id);
        if let Some(hold) = queued {
            if hold.member_id != member.id {
                return Err(UnifiedError::conflict(
                    "The item has a hold queue; the next member in the queue checks out first",
                ));
            }
        }
        // The racy tail — concurrent-loan count, copy status, loan
        // insert, copy mark — runs atomically in `checkout_copy`
        // (member checkout lock + held copy entry); the pre-checks
        // above are a fast path and the checks inside are
        // authoritative.
        let company_id = super::auth::require_company_match_for(&company, &input.company_id)?;
        let loan = super::service::checkout_copy(&state, &member, &item, copy_id, &policy, company_id)?;
        let due_at = loan.due_at;
        state.emit(
            "library.loan.created",
            json!({
                "loan_id": loan.id.to_string(),
                "member_id": member.id.to_string(),
                "user_id": member.user_id,
                "item_id": item.id.to_string(),
                "copy_id": loan.copy_id.map(|id| id.to_string()),
                "due_at": due_at.to_rfc3339(),
                "actor": caller.actor,
            }),
        );
        Ok(loan)
    })();
    match result {
        Ok(loan) => created(loan),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_loans(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let params = query.into_inner();
        let pagination = params.normalized();
        let filters = pagination.filters.clone();
        let is_manager = check_capability(&ctx, &caller.role, PERM_LIBRARY_MANAGE)
            || check_capability(&ctx, &caller.role, PERM_LIBRARY_CIRCULATE);
        let mut items: Vec<LibraryLoan> = state
            .loans
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items.retain(|loan| super::auth::in_company_scope(&loan.company_id, &company));
        if !is_manager {
            items.retain(|loan| loan.user_id == caller.actor);
        }
        if let Some(member_id) = filters.get("member_id") {
            items.retain(|loan| loan.member_id.to_string() == *member_id);
        }
        if let Some(user_id) = filters.get("user_id") {
            items.retain(|loan| &loan.user_id == user_id);
        }
        if let Some(item_id) = filters.get("item_id") {
            items.retain(|loan| loan.item_id.to_string() == *item_id);
        }
        if let Some(copy_id) = filters.get("copy_id") {
            items.retain(|loan| {
                loan.copy_id
                    .map(|id| id.to_string() == *copy_id)
                    .unwrap_or(false)
            });
        }
        if let Some(status) = filters.get("status") {
            items.retain(|loan| &loan.status == status);
        }
        if let Some(due_before) = filters.get("due_before") {
            if let Ok(limit) = parse_datetime(due_before, "due_before") {
                items.retain(|loan| loan.due_at <= limit);
            }
        }
        items.sort_by_key(|loan| std::cmp::Reverse(loan.checked_out_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_loan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryLoan> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let loan = load_loan(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&loan.company_id, &company)?;
        if loan.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        }
        Ok(loan)
    })();
    match result {
        Ok(loan) => ok(loan),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn renew_loan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryLoan> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let mut loan = load_loan(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&loan.company_id, &company)?;
        if loan.status != LOAN_ACTIVE && loan.status != LOAN_OVERDUE {
            return Err(UnifiedError::conflict(format!(
                "Only an active loan can be renewed (status: {})",
                loan.status
            )));
        }
        let settings = state.settings();
        if loan.status == LOAN_OVERDUE && !settings.allow_renew_when_overdue {
            return Err(UnifiedError::conflict(
                "An overdue loan cannot be renewed; return it first",
            ));
        }
        let member = load_member(&state, &loan.member_id.to_string())?;
        let item = load_item(&state, &loan.item_id.to_string())?;
        let policy = resolve_policy(&state, &member.member_type, &item.item_type)?;
        if loan.renewal_count >= policy.max_renewals {
            return Err(UnifiedError::conflict(format!(
                "The loan already reached the renewal limit of {}",
                policy.max_renewals
            )));
        }
        // A hold queue blocks renewal: the item is wanted by others.
        if first_queued_hold(&state, item.id).is_some() {
            return Err(UnifiedError::conflict(
                "The loan cannot be renewed while another member holds the item",
            ));
        }
        let now = Utc::now();
        loan.due_at = loan.due_at + chrono::Duration::days(policy.loan_days);
        if loan.due_at < now {
            loan.due_at = now + chrono::Duration::days(policy.loan_days);
        }
        loan.renewal_count += 1;
        loan.status = LOAN_ACTIVE.to_string();
        loan.updated_at = now;
        state.loans.insert(loan.id, loan.clone());
        state.emit(
            "library.loan.renewed",
            json!({
                "loan_id": loan.id.to_string(),
                "member_id": loan.member_id.to_string(),
                "user_id": loan.user_id,
                "item_id": loan.item_id.to_string(),
                "due_at": loan.due_at.to_rfc3339(),
                "renewal_count": loan.renewal_count,
                "actor": caller.actor,
            }),
        );
        Ok(loan)
    })();
    match result {
        Ok(loan) => ok(loan),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn return_loan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let mut loan = load_loan(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&loan.company_id, &company)?;
        if loan.status != LOAN_ACTIVE && loan.status != LOAN_OVERDUE {
            return Err(UnifiedError::conflict(format!(
                "Only an active loan can be returned (status: {})",
                loan.status
            )));
        }
        let now = Utc::now();
        let was_overdue = loan.status == LOAN_OVERDUE || now > loan.due_at;
        let member = load_member(&state, &loan.member_id.to_string())?;
        let item = load_item(&state, &loan.item_id.to_string())?;
        let policy = resolve_policy(&state, &member.member_type, &item.item_type)?;
        loan.status = LOAN_RETURNED.to_string();
        loan.returned_at = Some(now);
        loan.updated_at = now;
        state.loans.insert(loan.id, loan.clone());
        if let Some(copy_uuid) = loan.copy_id {
            if let Some(mut entry) = state.copies.get_mut(&copy_uuid) {
                entry.value_mut().status = COPY_AVAILABLE.to_string();
                entry.value_mut().updated_at = now;
            }
        }
        // Revoke outstanding digital access for this loan.
        for mut entry in state.digital_access.iter_mut() {
            if entry.value().loan_id == loan.id && !entry.value().revoked {
                entry.value_mut().revoked = true;
                entry.value_mut().updated_at = now;
            }
        }
        let fine = if was_overdue {
            let amount = overdue_fine_minor(&policy, loan.due_at, now);
            create_fine_for_loan(
                &state,
                &loan,
                amount,
                &policy.currency,
                "overdue",
            )
        } else {
            None
        };
        state.emit(
            "library.loan.returned",
            json!({
                "loan_id": loan.id.to_string(),
                "member_id": loan.member_id.to_string(),
                "user_id": loan.user_id,
                "item_id": loan.item_id.to_string(),
                "copy_id": loan.copy_id.map(|id| id.to_string()),
                "was_overdue": was_overdue,
                "fine_id": fine.as_ref().map(|value| value.id.to_string()),
                "actor": caller.actor,
            }),
        );
        // Offer the item to the next hold in the FIFO queue.
        offer_to_next_hold(&state, &item);
        Ok(json!({ "loan": loan, "fine": fine }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn mark_loan_lost(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<MarkLoanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut loan = load_loan(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&loan.company_id, &company)?;
        if loan.status != LOAN_ACTIVE && loan.status != LOAN_OVERDUE {
            return Err(UnifiedError::conflict(format!(
                "Only an active loan can be marked lost (status: {})",
                loan.status
            )));
        }
        let now = Utc::now();
        let member = load_member(&state, &loan.member_id.to_string())?;
        let item = load_item(&state, &loan.item_id.to_string())?;
        let policy = resolve_policy(&state, &member.member_type, &item.item_type)?;
        loan.status = LOAN_LOST.to_string();
        loan.updated_at = now;
        state.loans.insert(loan.id, loan.clone());
        if let Some(copy_uuid) = loan.copy_id {
            if let Some(mut entry) = state.copies.get_mut(&copy_uuid) {
                entry.value_mut().status = COPY_LOST.to_string();
                entry.value_mut().updated_at = now;
            }
        }
        let currency = match &input.currency {
            Some(raw) => require_currency(raw)?,
            None => policy.currency.clone(),
        };
        let fine = match input.replacement_amount_minor {
            Some(amount) => {
                let checked = require_non_negative(amount, "replacement_amount_minor")?;
                create_fine_for_loan(&state, &loan, checked, &currency, "lost_replacement")
            }
            None => None,
        };
        state.emit(
            "library.loan.marked_lost",
            json!({
                "loan_id": loan.id.to_string(),
                "member_id": loan.member_id.to_string(),
                "user_id": loan.user_id,
                "item_id": loan.item_id.to_string(),
                "copy_id": loan.copy_id.map(|id| id.to_string()),
                "reason": input.reason,
                "fine_id": fine.as_ref().map(|value| value.id.to_string()),
                "actor": caller.actor,
            }),
        );
        Ok(json!({ "loan": loan, "fine": fine }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn mark_loan_damaged(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<MarkLoanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut loan = load_loan(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&loan.company_id, &company)?;
        if loan.status != LOAN_ACTIVE && loan.status != LOAN_OVERDUE {
            return Err(UnifiedError::conflict(format!(
                "Only an active loan can be marked damaged (status: {})",
                loan.status
            )));
        }
        let now = Utc::now();
        let member = load_member(&state, &loan.member_id.to_string())?;
        let item = load_item(&state, &loan.item_id.to_string())?;
        let policy = resolve_policy(&state, &member.member_type, &item.item_type)?;
        loan.status = LOAN_DAMAGED.to_string();
        loan.returned_at = Some(now);
        loan.updated_at = now;
        state.loans.insert(loan.id, loan.clone());
        if let Some(copy_uuid) = loan.copy_id {
            if let Some(mut entry) = state.copies.get_mut(&copy_uuid) {
                entry.value_mut().status = COPY_DAMAGED.to_string();
                entry.value_mut().condition = "damaged".to_string();
                entry.value_mut().updated_at = now;
            }
        }
        let currency = match &input.currency {
            Some(raw) => require_currency(raw)?,
            None => policy.currency.clone(),
        };
        let fine = match input.replacement_amount_minor {
            Some(amount) => {
                let checked = require_non_negative(amount, "replacement_amount_minor")?;
                create_fine_for_loan(&state, &loan, checked, &currency, "damaged_replacement")
            }
            None => None,
        };
        state.emit(
            "library.loan.marked_damaged",
            json!({
                "loan_id": loan.id.to_string(),
                "member_id": loan.member_id.to_string(),
                "user_id": loan.user_id,
                "item_id": loan.item_id.to_string(),
                "copy_id": loan.copy_id.map(|id| id.to_string()),
                "reason": input.reason,
                "fine_id": fine.as_ref().map(|value| value.id.to_string()),
                "actor": caller.actor,
            }),
        );
        Ok(json!({ "loan": loan, "fine": fine }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_overdue_loans(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let params = query.into_inner();
        let now = Utc::now();
        let mut items: Vec<LibraryLoan> = state
            .loans
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|loan| {
                loan.status == LOAN_OVERDUE
                    || ((loan.status == LOAN_ACTIVE) && loan.due_at < now)
            })
            .collect();
        items.retain(|loan| super::auth::in_company_scope(&loan.company_id, &company));
        items.sort_by_key(|loan| loan.due_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn scan_overdue_loans(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let now = Utc::now();
        let mut marked = 0i64;
        let mut fines_raised = 0i64;
        let mut notified = 0i64;
        let candidates: Vec<LibraryLoan> = state
            .loans
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|loan| {
                loan.status == LOAN_ACTIVE
                    && loan.due_at < now
                    && super::auth::in_company_scope(&loan.company_id, &company)
            })
            .collect();
        for mut loan in candidates {
            let member = match state.members.get(&loan.member_id) {
                Some(entry) => entry.value().clone(),
                None => continue,
            };
            let item = match state.items.get(&loan.item_id) {
                Some(entry) => entry.value().clone(),
                None => continue,
            };
            let policy = match resolve_policy(&state, &member.member_type, &item.item_type) {
                Ok(value) => value,
                Err(_) => continue,
            };
            loan.status = LOAN_OVERDUE.to_string();
            loan.last_overdue_noticed_at = Some(now);
            loan.updated_at = now;
            state.loans.insert(loan.id, loan.clone());
            marked += 1;
            let amount = overdue_fine_minor(&policy, loan.due_at, now);
            if create_fine_for_loan(&state, &loan, amount, &policy.currency, "overdue").is_some() {
                fines_raised += 1;
            }
            state.emit(
                "library.loan.overdue",
                json!({
                    "loan_id": loan.id.to_string(),
                    "member_id": loan.member_id.to_string(),
                    "user_id": loan.user_id,
                    "item_id": loan.item_id.to_string(),
                    "due_at": loan.due_at.to_rfc3339(),
                    "actor": caller.actor,
                }),
            );
            state.notify(
                "library.overdue",
                &loan.user_id,
                "Overdue library loan",
                &format!(
                    "The item \"{}\" was due on {} and is now overdue.",
                    item.title,
                    loan.due_at.format("%Y-%m-%d")
                ),
                Some(format!("/library/loans/{}", loan.id)),
            );
            notified += 1;
        }
        Ok(json!({
            "marked_overdue": marked,
            "fines_raised": fines_raised,
            "members_notified": notified,
            "scanned_at": now.to_rfc3339(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
