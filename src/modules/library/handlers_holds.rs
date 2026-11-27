// Library - Hold, fine and policy handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::{COPY_AVAILABLE, CreateHoldRequest, CreatePolicyRequest, FINE_PENDING, FINE_WAIVED,
    HOLD_CANCELLED, HOLD_EXPIRED, HOLD_QUEUED, HOLD_READY, ITEM_ACTIVE, LOAN_ACTIVE, LOAN_OVERDUE,
    LibraryFine, LibraryHold, LibraryState, LoanPolicy, MAX_TITLE_LEN, MEMBER_ACTIVE,
    PERM_LIBRARY_CIRCULATE, PERM_LIBRARY_MANAGE, PERM_LIBRARY_VIEW, UpdatePolicyRequest, caller,
    check_capability, created, err, load_fine, load_hold, load_item, load_member, load_policy,
    next_hold_position, offer_to_next_hold, ok, paginate, require_circulate, require_currency,
    require_in_list, require_non_negative, require_permission, require_positive, require_tenant,
    require_text};

// Hold handlers

pub(crate) async fn create_hold(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    body: web::Json<CreateHoldRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryHold> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let input = body.into_inner();
        let company_id = super::auth::require_company_match_for(&company, &input.company_id)?;
        let member = load_member(&state, &input.member_id)?;
        super::auth::require_record_in_company_scope(&member.company_id, &company)?;
        if member.status != MEMBER_ACTIVE {
            return Err(UnifiedError::conflict(format!(
                "Member \"{}\" is not active (status: {})",
                member.id, member.status
            )));
        }
        let item = load_item(&state, &input.item_id)?;
        super::auth::require_record_in_company_scope(&item.company_id, &company)?;
        if item.status != ITEM_ACTIVE {
            return Err(UnifiedError::conflict("The item is withdrawn and cannot be reserved"));
        }
        let duplicate = state.holds.iter().any(|entry| {
            entry.value().member_id == member.id
                && entry.value().item_id == item.id
                && (entry.value().status == HOLD_QUEUED || entry.value().status == HOLD_READY)
        });
        if duplicate {
            return Err(UnifiedError::conflict(
                "The member already has an active hold for this item",
            ));
        }
        let already_has_item = state.loans.iter().any(|entry| {
            entry.value().member_id == member.id
                && entry.value().item_id == item.id
                && (entry.value().status == LOAN_ACTIVE || entry.value().status == LOAN_OVERDUE)
        });
        if already_has_item {
            return Err(UnifiedError::conflict(
                "The member already has this item on loan; a hold is not needed",
            ));
        }
        let available = state.copies.iter().any(|entry| {
            entry.value().item_id == item.id && entry.value().status == COPY_AVAILABLE
        });
        let is_digital = item.digital_ref.is_some()
            || item.document_ref.is_some()
            || item.storage_ref.is_some();
        if available && !is_digital {
            return Err(UnifiedError::conflict(
                "A copy is available; check the item out instead of reserving it",
            ));
        }
        let now = Utc::now();
        let hold = LibraryHold {
            id: Uuid::new_v4(),
            member_id: member.id,
            user_id: member.user_id.clone(),
            item_id: item.id,
            copy_id: None,
            status: HOLD_QUEUED.to_string(),
            position: next_hold_position(&state, item.id),
            ready_at: None,
            expires_at: None,
            company_id,
            created_at: now,
            updated_at: now,
        };
        state.holds.insert(hold.id, hold.clone());
        state.emit(
            "library.hold.created",
            json!({
                "hold_id": hold.id.to_string(),
                "member_id": member.id.to_string(),
                "user_id": member.user_id,
                "item_id": item.id.to_string(),
                "position": hold.position,
                "actor": caller.actor,
            }),
        );
        Ok(hold)
    })();
    match result {
        Ok(hold) => created(hold),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_holds(
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
        let mut items: Vec<LibraryHold> = state
            .holds
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items.retain(|hold| super::auth::in_company_scope(&hold.company_id, &company));
        if !is_manager {
            items.retain(|hold| hold.user_id == caller.actor);
        }
        if let Some(member_id) = filters.get("member_id") {
            items.retain(|hold| hold.member_id.to_string() == *member_id);
        }
        if let Some(item_id) = filters.get("item_id") {
            items.retain(|hold| hold.item_id.to_string() == *item_id);
        }
        if let Some(status) = filters.get("status") {
            items.retain(|hold| &hold.status == status);
        }
        items.sort_by_key(|hold| (hold.item_id, hold.position));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_hold(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryHold> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let hold = load_hold(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&hold.company_id, &company)?;
        if hold.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        }
        Ok(hold)
    })();
    match result {
        Ok(hold) => ok(hold),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn cancel_hold(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryHold> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let mut hold = load_hold(&state, &path.into_inner())?;
        super::auth::require_record_writable_in_company_scope(&hold.company_id, &company)?;
        if hold.user_id != caller.actor {
            require_circulate(&ctx, &caller.role)?;
        }
        if hold.status != HOLD_QUEUED && hold.status != HOLD_READY {
            return Err(UnifiedError::conflict(format!(
                "Only a queued or ready hold can be cancelled (status: {})",
                hold.status
            )));
        }
        hold.status = HOLD_CANCELLED.to_string();
        hold.updated_at = Utc::now();
        state.holds.insert(hold.id, hold.clone());
        state.emit(
            "library.hold.cancelled",
            json!({
                "hold_id": hold.id.to_string(),
                "member_id": hold.member_id.to_string(),
                "user_id": hold.user_id,
                "item_id": hold.item_id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(hold)
    })();
    match result {
        Ok(hold) => ok(hold),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn fulfill_hold(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryHold> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let mut hold = load_hold(&state, &path.into_inner())?;
        super::auth::require_record_writable_in_company_scope(&hold.company_id, &company)?;
        if hold.status != HOLD_QUEUED {
            return Err(UnifiedError::conflict(format!(
                "Only a queued hold can be marked ready (status: {})",
                hold.status
            )));
        }
        let item = load_item(&state, &hold.item_id.to_string())?;
        let settings = state.settings();
        let now = Utc::now();
        hold.status = HOLD_READY.to_string();
        hold.ready_at = Some(now);
        hold.expires_at = Some(now + chrono::Duration::days(settings.hold_expiry_days));
        hold.updated_at = now;
        state.holds.insert(hold.id, hold.clone());
        state.emit(
            "library.hold.fulfilled",
            json!({
                "hold_id": hold.id.to_string(),
                "member_id": hold.member_id.to_string(),
                "user_id": hold.user_id,
                "item_id": hold.item_id.to_string(),
                "ready_at": now.to_rfc3339(),
                "expires_at": hold.expires_at.map(|value| value.to_rfc3339()),
                "actor": caller.actor,
            }),
        );
        state.notify(
            "library.hold_ready",
            &hold.user_id,
            "Reserved item is ready",
            &format!(
                "The item \"{}\" you reserved is ready for pickup.",
                item.title
            ),
            Some(format!("/library/items/{}", item.id)),
        );
        Ok(hold)
    })();
    match result {
        Ok(hold) => ok(hold),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn expire_hold(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryHold> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let mut hold = load_hold(&state, &path.into_inner())?;
        super::auth::require_record_writable_in_company_scope(&hold.company_id, &company)?;
        if hold.status != HOLD_READY && hold.status != HOLD_QUEUED {
            return Err(UnifiedError::conflict(format!(
                "Only a ready or queued hold can expire (status: {})",
                hold.status
            )));
        }
        let item = load_item(&state, &hold.item_id.to_string())?;
        hold.status = HOLD_EXPIRED.to_string();
        hold.updated_at = Utc::now();
        state.holds.insert(hold.id, hold.clone());
        state.emit(
            "library.hold.expired",
            json!({
                "hold_id": hold.id.to_string(),
                "member_id": hold.member_id.to_string(),
                "user_id": hold.user_id,
                "item_id": hold.item_id.to_string(),
                "actor": caller.actor,
            }),
        );
        // Offer the item to the next hold in the FIFO queue.
        offer_to_next_hold(&state, &item);
        Ok(hold)
    })();
    match result {
        Ok(hold) => ok(hold),
        Err(error) => err(error, debug),
    }
}

// Fine handlers

pub(crate) async fn list_fines(
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
        let mut items: Vec<LibraryFine> = state
            .fines
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items.retain(|fine| super::auth::in_company_scope(&fine.company_id, &company));
        if !is_manager {
            items.retain(|fine| fine.user_id == caller.actor);
        }
        if let Some(member_id) = filters.get("member_id") {
            items.retain(|fine| fine.member_id.to_string() == *member_id);
        }
        if let Some(loan_id) = filters.get("loan_id") {
            items.retain(|fine| fine.loan_id.to_string() == *loan_id);
        }
        if let Some(status) = filters.get("status") {
            items.retain(|fine| &fine.status == status);
        }
        if let Some(reason) = filters.get("reason") {
            items.retain(|fine| &fine.reason == reason);
        }
        items.sort_by_key(|fine| std::cmp::Reverse(fine.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_fine(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryFine> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let fine = load_fine(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&fine.company_id, &company)?;
        if fine.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        }
        Ok(fine)
    })();
    match result {
        Ok(fine) => ok(fine),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn waive_fine(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryFine> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        let mut fine = load_fine(&state, &path.into_inner())?;
        super::auth::require_record_writable_in_company_scope(&fine.company_id, &company)?;
        if fine.status != FINE_PENDING {
            return Err(UnifiedError::conflict(format!(
                "Only a pending fine can be waived (status: {})",
                fine.status
            )));
        }
        fine.status = FINE_WAIVED.to_string();
        fine.waived_by = Some(caller.actor.clone());
        fine.updated_at = Utc::now();
        state.fines.insert(fine.id, fine.clone());
        state.emit(
            "library.fine.waived",
            json!({
                "fine_id": fine.id.to_string(),
                "loan_id": fine.loan_id.to_string(),
                "member_id": fine.member_id.to_string(),
                "user_id": fine.user_id,
                "amount_minor": fine.amount_minor,
                "currency": fine.currency,
                "actor": caller.actor,
            }),
        );
        Ok(fine)
    })();
    match result {
        Ok(fine) => ok(fine),
        Err(error) => err(error, debug),
    }
}

// Loan policy handlers

pub(crate) async fn list_policies(
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
        let mut items: Vec<LoanPolicy> = state
            .policies
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items.retain(|policy| super::auth::in_company_scope(&policy.company_id, &company));
        if let Some(member_type) = filters.get("member_type") {
            items.retain(|policy| &policy.member_type == member_type);
        }
        if let Some(item_type) = filters.get("item_type") {
            items.retain(|policy| &policy.item_type == item_type);
        }
        if let Some(active) = filters.get("active") {
            let wanted = active == "true";
            items.retain(|policy| policy.active == wanted);
        }
        items.sort_by_key(|policy| policy.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    body: web::Json<CreatePolicyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoanPolicy> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        let input = body.into_inner();
        let company_id = super::auth::require_company_match_for(&company, &input.company_id)?;
        let settings = state.settings();
        let name = require_text(&input.name, "name", MAX_TITLE_LEN)?;
        let member_type = if input.member_type == "default" {
            "default".to_string()
        } else {
            require_in_list(&input.member_type, "member_type", &settings.member_types)?
        };
        let item_type = if input.item_type == "all" {
            "all".to_string()
        } else {
            require_in_list(&input.item_type, "item_type", &settings.item_types)?
        };
        let currency = match &input.currency {
            Some(raw) => require_currency(raw)?,
            None => settings.default_currency.clone(),
        };
        let duplicate = state.policies.iter().any(|entry| {
            entry.value().member_type == member_type
                && entry.value().item_type == item_type
                && entry.value().active
        });
        if duplicate {
            return Err(UnifiedError::conflict(format!(
                "An active policy already covers member type \"{member_type}\" and item type \"{item_type}\""
            )));
        }
        let now = Utc::now();
        let policy = LoanPolicy {
            id: Uuid::new_v4(),
            name,
            member_type,
            item_type,
            loan_days: require_positive(input.loan_days, "loan_days")?,
            max_concurrent: require_positive(input.max_concurrent, "max_concurrent")?,
            max_renewals: require_non_negative(input.max_renewals, "max_renewals")?,
            fine_per_day_minor: require_non_negative(
                input.fine_per_day_minor,
                "fine_per_day_minor",
            )?,
            currency,
            grace_days: require_non_negative(input.grace_days, "grace_days")?,
            active: true,
            company_id,
            created_at: now,
            updated_at: now,
        };
        state.policies.insert(policy.id, policy.clone());
        state.emit(
            "library.policy.created",
            json!({
                "policy_id": policy.id.to_string(),
                "member_type": policy.member_type,
                "item_type": policy.item_type,
                "actor": caller.actor,
            }),
        );
        Ok(policy)
    })();
    match result {
        Ok(policy) => created(policy),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoanPolicy> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let policy = load_policy(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&policy.company_id, &company)?;
        Ok(policy)
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePolicyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoanPolicy> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        let input = body.into_inner();
        let mut policy = load_policy(&state, &path.into_inner())?;
        super::auth::require_record_writable_in_company_scope(&policy.company_id, &company)?;
        let settings = state.settings();
        if let Some(name) = &input.name {
            policy.name = require_text(name, "name", MAX_TITLE_LEN)?;
        }
        if let Some(member_type) = &input.member_type {
            policy.member_type = if member_type == "default" {
                "default".to_string()
            } else {
                require_in_list(member_type, "member_type", &settings.member_types)?
            };
        }
        if let Some(item_type) = &input.item_type {
            policy.item_type = if item_type == "all" {
                "all".to_string()
            } else {
                require_in_list(item_type, "item_type", &settings.item_types)?
            };
        }
        if let Some(loan_days) = input.loan_days {
            policy.loan_days = require_positive(loan_days, "loan_days")?;
        }
        if let Some(max_concurrent) = input.max_concurrent {
            policy.max_concurrent = require_positive(max_concurrent, "max_concurrent")?;
        }
        if let Some(max_renewals) = input.max_renewals {
            policy.max_renewals = require_non_negative(max_renewals, "max_renewals")?;
        }
        if let Some(fine) = input.fine_per_day_minor {
            policy.fine_per_day_minor = require_non_negative(fine, "fine_per_day_minor")?;
        }
        if let Some(currency) = &input.currency {
            policy.currency = require_currency(currency)?;
        }
        if let Some(grace_days) = input.grace_days {
            policy.grace_days = require_non_negative(grace_days, "grace_days")?;
        }
        if let Some(active) = input.active {
            policy.active = active;
        }
        if policy.active {
            let duplicate = state.policies.iter().any(|entry| {
                entry.value().id != policy.id
                    && entry.value().member_type == policy.member_type
                    && entry.value().item_type == policy.item_type
                    && entry.value().active
            });
            if duplicate {
                return Err(UnifiedError::conflict(format!(
                    "An active policy already covers member type \"{}\" and item type \"{}\"",
                    policy.member_type, policy.item_type
                )));
            }
        }
        policy.updated_at = Utc::now();
        state.policies.insert(policy.id, policy.clone());
        state.emit(
            "library.policy.updated",
            json!({
                "policy_id": policy.id.to_string(),
                "member_type": policy.member_type,
                "item_type": policy.item_type,
                "actor": caller.actor,
            }),
        );
        Ok(policy)
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_policy(
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
        let policy = load_policy(&state, &path.into_inner())?;
        super::auth::require_record_writable_in_company_scope(&policy.company_id, &company)?;
        state.policies.remove(&policy.id);
        state.emit(
            "library.policy.deleted",
            json!({
                "policy_id": policy.id.to_string(),
                "member_type": policy.member_type,
                "item_type": policy.item_type,
                "actor": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "policy_id": policy.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
