// Library - Digital access, audit, settings and AI handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use super::{AUDIT_COMPLETED, AUDIT_OPEN, COPY_AVAILABLE, COPY_DAMAGED, COPY_LOST, COPY_WITHDRAWN,
    CreateAuditRequest, CreateDiscrepancyRequest, DISCREPANCY_DAMAGED, DISCREPANCY_FOUND,
    DISCREPANCY_MISSING, DISCREPANCY_PENDING, DISCREPANCY_RESOLVED, DISCREPANCY_WRITTEN_OFF,
    DigitalAccess, DigitalAccessRequest, InventoryAudit, InventoryDiscrepancy, LOAN_ACTIVE,
    LOAN_OVERDUE, LibraryLoan, LibrarySettings, LibraryState, MAX_NOTE_LEN, MAX_SLUG_LEN,
    MAX_TITLE_LEN, PERM_LIBRARY_MANAGE, PERM_LIBRARY_VIEW, ResolveDiscrepancyRequest,
    UpdateSettingsRequest, caller, created, err, load_audit, load_copy, load_item, load_loan, ok,
    opt_string, optional_text, paginate, parse_uuid, require_circulate, require_currency,
    require_in_list, require_non_negative, require_permission, require_positive, require_tenant,
    require_text};

// ---------------------------------------------------------------------------
// Digital access handlers
// ---------------------------------------------------------------------------

pub(crate) async fn issue_digital_access(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DigitalAccessRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let item = load_item(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&item.company_id, &company)?;
        let input = body.into_inner();
        let loan = load_loan(&state, &input.loan_id)?;
        super::auth::require_record_in_company_scope(&loan.company_id, &company)?;
        if loan.item_id != item.id {
            return Err(UnifiedError::bad_request(
                "The loan does not belong to the requested item",
            ));
        }
        if loan.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        }
        if loan.status != LOAN_ACTIVE && loan.status != LOAN_OVERDUE {
            return Err(UnifiedError::conflict(format!(
                "Digital access needs an active loan (status: {})",
                loan.status
            )));
        }
        if item.digital_ref.is_none()
            && item.document_ref.is_none()
            && item.storage_ref.is_none()
        {
            return Err(UnifiedError::conflict(
                "The item has no digital reference; digital access is not available",
            ));
        }
        let settings = state.settings();
        let ttl = match input.expires_secs {
            Some(value) => {
                if value == 0 || value > settings.digital_link_ttl_secs * 24 {
                    return Err(UnifiedError::bad_request(
                        "expires_secs is outside the allowed range for digital access",
                    ));
                }
                value
            }
            None => settings.digital_link_ttl_secs,
        };
        let (token, expires_at) = state.issue_signed_token(&loan.id.to_string(), ttl);
        let now = Utc::now();
        let grant = DigitalAccess {
            id: Uuid::new_v4(),
            loan_id: loan.id,
            item_id: item.id,
            member_id: loan.member_id,
            user_id: loan.user_id.clone(),
            expires_at,
            revoked: false,
            redeemed_at: None,
            company_id: loan.company_id.clone(),
            created_at: now,
            updated_at: now,
        };
        state.digital_access.insert(grant.id, grant.clone());
        state.emit(
            "library.digital_access.issued",
            json!({
                "access_id": grant.id.to_string(),
                "loan_id": loan.id.to_string(),
                "item_id": item.id.to_string(),
                "user_id": loan.user_id,
                "expires_at": expires_at.to_rfc3339(),
                "actor": caller.actor,
            }),
        );
        Ok(json!({
            "access_id": grant.id.to_string(),
            "token": token,
            "expires_at": expires_at.to_rfc3339(),
            "item_id": item.id.to_string(),
            "loan_id": loan.id.to_string(),
        }))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn redeem_digital_access(
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
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let token = path.into_inner();
        let loan_id = state.verify_signed_token(&token)?;
        let loan_uuid = parse_uuid(&loan_id, "loan")?;
        let loan = state
            .loans
            .get(&loan_uuid)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Loan for this access token not found"))?;
        super::auth::require_record_in_company_scope(&loan.company_id, &company)?;
        if loan.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        }
        if loan.status != LOAN_ACTIVE && loan.status != LOAN_OVERDUE {
            return Err(UnifiedError::forbidden(
                "Digital access is no longer valid for this loan",
            ));
        }
        let grant = state
            .digital_access
            .iter()
            .map(|entry| entry.value().clone())
            .find(|entry| entry.loan_id == loan_uuid && !entry.revoked)
            .ok_or_else(|| UnifiedError::forbidden("Digital access has been revoked"))?;
        if grant.expires_at <= Utc::now() {
            return Err(UnifiedError::forbidden("Digital access has expired"));
        }
        let item = load_item(&state, &loan.item_id.to_string())?;
        super::auth::require_record_in_company_scope(&item.company_id, &company)?;
        let now = Utc::now();
        if let Some(mut entry) = state.digital_access.get_mut(&grant.id) {
            entry.value_mut().redeemed_at = Some(now);
            entry.value_mut().updated_at = now;
        }
        state.emit(
            "library.digital_access.redeemed",
            json!({
                "access_id": grant.id.to_string(),
                "loan_id": loan.id.to_string(),
                "item_id": item.id.to_string(),
                "user_id": loan.user_id,
                "actor": caller.actor,
            }),
        );
        // References only: the caller hands these to the documents /
        // storage modules; no bytes are ever returned here.
        Ok(json!({
            "item_id": item.id.to_string(),
            "title": item.title,
            "digital_ref": item.digital_ref,
            "document_ref": item.document_ref,
            "storage_ref": item.storage_ref,
            "expires_at": grant.expires_at.to_rfc3339(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_item_digital(
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
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let item = load_item(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&item.company_id, &company)?;
        Ok(json!({
            "item_id": item.id.to_string(),
            "title": item.title,
            "digital_ref": item.digital_ref,
            "document_ref": item.document_ref,
            "storage_ref": item.storage_ref,
            "has_digital": item.digital_ref.is_some() || item.document_ref.is_some() || item.storage_ref.is_some(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Inventory audit handlers
// ---------------------------------------------------------------------------

pub(crate) async fn create_audit(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    body: web::Json<CreateAuditRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InventoryAudit> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        let input = body.into_inner();
        let company_id = super::auth::require_company_match_for(&company, &input.company_id)?;
        let name = require_text(&input.name, "name", MAX_TITLE_LEN)?;
        let now = Utc::now();
        let audit = InventoryAudit {
            id: Uuid::new_v4(),
            name,
            scope_location: optional_text(&input.scope_location, "scope_location", MAX_TITLE_LEN)?,
            scope_shelf: optional_text(&input.scope_shelf, "scope_shelf", MAX_SLUG_LEN)?,
            status: AUDIT_OPEN.to_string(),
            started_by: caller.actor.clone(),
            completed_at: None,
            items_checked: 0,
            discrepancies_found: 0,
            company_id,
            created_at: now,
            updated_at: now,
        };
        state.audits.insert(audit.id, audit.clone());
        Ok(audit)
    })();
    match result {
        Ok(audit) => created(audit),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_audits(
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
        let mut items: Vec<InventoryAudit> = state
            .audits
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items.retain(|audit| super::auth::in_company_scope(&audit.company_id, &company));
        if let Some(status) = filters.get("status") {
            items.retain(|audit| &audit.status == status);
        }
        items.sort_by_key(|audit| std::cmp::Reverse(audit.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_audit(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InventoryAudit> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let audit = load_audit(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&audit.company_id, &company)?;
        Ok(audit)
    })();
    match result {
        Ok(audit) => ok(audit),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn complete_audit(
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
        let mut audit = load_audit(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&audit.company_id, &company)?;
        if audit.status != AUDIT_OPEN {
            return Err(UnifiedError::conflict(format!(
                "Only an open audit can be completed (status: {})",
                audit.status
            )));
        }
        let discrepancies: Vec<InventoryDiscrepancy> = state
            .discrepancies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.audit_id == audit.id)
            .collect();
        let unresolved = discrepancies
            .iter()
            .filter(|entry| entry.status == DISCREPANCY_PENDING)
            .count() as i64;
        if unresolved > 0 {
            return Err(UnifiedError::conflict(format!(
                "The audit has {unresolved} unresolved discrepancies; resolve them first"
            )));
        }
        let now = Utc::now();
        audit.status = AUDIT_COMPLETED.to_string();
        audit.completed_at = Some(now);
        audit.discrepancies_found = discrepancies.len() as i64;
        audit.updated_at = now;
        state.audits.insert(audit.id, audit.clone());
        state.emit(
            "library.inventory.completed",
            json!({
                "audit_id": audit.id.to_string(),
                "name": audit.name,
                "scope_location": audit.scope_location,
                "scope_shelf": audit.scope_shelf,
                "items_checked": audit.items_checked,
                "discrepancies_found": audit.discrepancies_found,
                "discrepancies": discrepancies.iter().map(|entry| json!({
                    "discrepancy_id": entry.id.to_string(),
                    "copy_id": entry.copy_id.to_string(),
                    "kind": entry.kind,
                    "status": entry.status,
                })).collect::<Vec<Value>>(),
                "actor": caller.actor,
            }),
        );
        Ok(json!({ "audit": audit, "discrepancies": discrepancies }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn add_discrepancy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateDiscrepancyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InventoryDiscrepancy> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let audit = load_audit(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&audit.company_id, &company)?;
        if audit.status != AUDIT_OPEN {
            return Err(UnifiedError::conflict(
                "Discrepancies can only be recorded on an open audit",
            ));
        }
        let input = body.into_inner();
        let settings = state.settings();
        let kind = require_in_list(&input.kind, "kind", &settings.discrepancy_kinds)?;
        let copy = load_copy(&state, &input.copy_id)?;
        super::auth::require_record_in_company_scope(&copy.company_id, &company)?;
        if let Some(scope) = &audit.scope_location {
            if copy.location.as_deref() != Some(scope.as_str()) {
                return Err(UnifiedError::bad_request(
                    "The copy is outside the audit scope location",
                ));
            }
        }
        if let Some(shelf) = &audit.scope_shelf {
            if copy.shelf.as_deref() != Some(shelf.as_str()) {
                return Err(UnifiedError::bad_request(
                    "The copy is outside the audit scope shelf",
                ));
            }
        }
        let now = Utc::now();
        let discrepancy = InventoryDiscrepancy {
            id: Uuid::new_v4(),
            audit_id: audit.id,
            copy_id: copy.id,
            item_id: copy.item_id,
            kind: kind.clone(),
            note: optional_text(&input.note, "note", MAX_NOTE_LEN)?,
            status: DISCREPANCY_PENDING.to_string(),
            resolved_by: None,
            company_id: audit.company_id.clone(),
            created_at: now,
            updated_at: now,
        };
        state.discrepancies.insert(discrepancy.id, discrepancy.clone());
        let mut updated_audit = audit.clone();
        updated_audit.items_checked += 1;
        updated_audit.discrepancies_found += 1;
        updated_audit.updated_at = now;
        state.audits.insert(updated_audit.id, updated_audit);
        if kind == DISCREPANCY_MISSING {
            if let Some(mut entry) = state.copies.get_mut(&copy.id) {
                entry.value_mut().status = COPY_LOST.to_string();
                entry.value_mut().updated_at = now;
            }
        }
        if kind == DISCREPANCY_DAMAGED {
            if let Some(mut entry) = state.copies.get_mut(&copy.id) {
                entry.value_mut().status = COPY_DAMAGED.to_string();
                entry.value_mut().condition = "damaged".to_string();
                entry.value_mut().updated_at = now;
            }
        }
        state.emit(
            "library.inventory.discrepancy.recorded",
            json!({
                "discrepancy_id": discrepancy.id.to_string(),
                "audit_id": audit.id.to_string(),
                "copy_id": copy.id.to_string(),
                "item_id": copy.item_id.to_string(),
                "kind": kind,
                "actor": caller.actor,
            }),
        );
        Ok(discrepancy)
    })();
    match result {
        Ok(discrepancy) => created(discrepancy),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn resolve_discrepancy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<ResolveDiscrepancyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InventoryDiscrepancy> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        let (audit_id, discrepancy_id) = path.into_inner();
        let audit = load_audit(&state, &audit_id)?;
        super::auth::require_record_in_company_scope(&audit.company_id, &company)?;
        let input = body.into_inner();
        let resolution = require_text(&input.resolution, "resolution", MAX_SLUG_LEN)?;
        if ![DISCREPANCY_RESOLVED, DISCREPANCY_WRITTEN_OFF].contains(&resolution.as_str()) {
            return Err(UnifiedError::bad_request(
                "resolution must be one of: resolved, written_off",
            ));
        }
        if resolution == DISCREPANCY_WRITTEN_OFF && input.approved != Some(true) {
            return Err(UnifiedError::bad_request(
                "A write-off needs explicit approval (approved: true)",
            ));
        }
        let uuid = parse_uuid(&discrepancy_id, "discrepancy")?;
        let mut discrepancy = state
            .discrepancies
            .get(&uuid)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Discrepancy \"{discrepancy_id}\" not found"))
            })?;
        if discrepancy.audit_id != audit.id {
            return Err(UnifiedError::not_found(format!(
                "Discrepancy \"{discrepancy_id}\" does not belong to this audit"
            )));
        }
        super::auth::require_record_in_company_scope(&discrepancy.company_id, &company)?;
        if discrepancy.status != DISCREPANCY_PENDING {
            return Err(UnifiedError::conflict(format!(
                "Only a pending discrepancy can be resolved (status: {})",
                discrepancy.status
            )));
        }
        let now = Utc::now();
        discrepancy.status = resolution.clone();
        discrepancy.resolved_by = Some(caller.actor.clone());
        if let Some(note) = &input.note {
            discrepancy.note = Some(require_text(note, "note", MAX_NOTE_LEN)?);
        }
        discrepancy.updated_at = now;
        state.discrepancies.insert(discrepancy.id, discrepancy.clone());
        if resolution == DISCREPANCY_WRITTEN_OFF {
            if let Some(mut entry) = state.copies.get_mut(&discrepancy.copy_id) {
                entry.value_mut().status = COPY_WITHDRAWN.to_string();
                entry.value_mut().updated_at = now;
            }
        }
        if resolution == DISCREPANCY_RESOLVED && discrepancy.kind == DISCREPANCY_FOUND {
            if let Some(mut entry) = state.copies.get_mut(&discrepancy.copy_id) {
                if entry.value().status == COPY_LOST {
                    entry.value_mut().status = COPY_AVAILABLE.to_string();
                    entry.value_mut().updated_at = now;
                }
            }
        }
        state.emit(
            "library.inventory.discrepancy.resolved",
            json!({
                "discrepancy_id": discrepancy.id.to_string(),
                "audit_id": audit.id.to_string(),
                "copy_id": discrepancy.copy_id.to_string(),
                "resolution": resolution,
                "actor": caller.actor,
            }),
        );
        Ok(discrepancy)
    })();
    match result {
        Ok(discrepancy) => ok(discrepancy),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn audit_report(
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
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let audit = load_audit(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&audit.company_id, &company)?;
        let mut discrepancies: Vec<InventoryDiscrepancy> = state
            .discrepancies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.audit_id == audit.id)
            .collect();
        discrepancies.sort_by_key(|entry| entry.created_at);
        let by_kind: HashMap<String, i64> =
            discrepancies
                .iter()
                .fold(HashMap::new(), |mut acc, entry| {
                    *acc.entry(entry.kind.clone()).or_insert(0) += 1;
                    acc
                });
        let by_status: HashMap<String, i64> =
            discrepancies
                .iter()
                .fold(HashMap::new(), |mut acc, entry| {
                    *acc.entry(entry.status.clone()).or_insert(0) += 1;
                    acc
                });
        Ok(json!({
            "audit": audit,
            "discrepancies": discrepancies,
            "total_discrepancies": discrepancies.len(),
            "by_kind": by_kind,
            "by_status": by_status,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Reminder handlers
// ---------------------------------------------------------------------------

pub(crate) async fn send_due_soon_reminders(
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
        let settings = state.settings();
        let now = Utc::now();
        let window_end = now + chrono::Duration::days(settings.reminder_days_before_due);
        let mut notified = 0i64;
        let candidates: Vec<LibraryLoan> = state
            .loans
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|loan| {
                loan.status == LOAN_ACTIVE
                    && super::auth::in_company_scope(&loan.company_id, &company)
                    && loan.due_at > now
                    && loan.due_at <= window_end
                    && loan
                        .last_reminded_at
                        .map(|reminded| reminded < now - chrono::Duration::days(1))
                        .unwrap_or(true)
            })
            .collect();
        for mut loan in candidates {
            let item = match state.items.get(&loan.item_id) {
                Some(entry) => entry.value().clone(),
                None => continue,
            };
            state.notify(
                "library.due_soon",
                &loan.user_id,
                "Library loan due soon",
                &format!(
                    "The item \"{}\" is due on {}.",
                    item.title,
                    loan.due_at.format("%Y-%m-%d")
                ),
                Some(format!("/library/loans/{}", loan.id)),
            );
            loan.last_reminded_at = Some(now);
            loan.updated_at = now;
            state.loans.insert(loan.id, loan);
            notified += 1;
        }
        Ok(json!({
            "members_notified": notified,
            "window_days": settings.reminder_days_before_due,
            "sent_at": now.to_rfc3339(),
            "actor": caller.actor,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn send_overdue_reminders(
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
        let mut notified = 0i64;
        let candidates: Vec<LibraryLoan> = state
            .loans
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|loan| {
                super::auth::in_company_scope(&loan.company_id, &company)
                    && (loan.status == LOAN_OVERDUE
                    || (loan.status == LOAN_ACTIVE && loan.due_at < now))
                    && loan
                        .last_overdue_noticed_at
                        .map(|noticed| noticed < now - chrono::Duration::days(1))
                        .unwrap_or(true)
            })
            .collect();
        for mut loan in candidates {
            let item = match state.items.get(&loan.item_id) {
                Some(entry) => entry.value().clone(),
                None => continue,
            };
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
            loan.last_overdue_noticed_at = Some(now);
            loan.updated_at = now;
            state.loans.insert(loan.id, loan);
            notified += 1;
        }
        Ok(json!({
            "members_notified": notified,
            "sent_at": now.to_rfc3339(),
            "actor": caller.actor,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Settings handlers
// ---------------------------------------------------------------------------

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibrarySettings> = (|| {
        let tenant = require_tenant(&req)?;
        super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        Ok(state.settings())
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibrarySettings> = (|| {
        let tenant = require_tenant(&req)?;
        super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.settings();
        let mut validate_list =
            |values: &Option<Vec<String>>, field: &str| -> UnifiedResult<Option<Vec<String>>> {
                match values {
                    Some(list) => {
                        if list.is_empty() {
                            return Err(UnifiedError::bad_request(format!(
                                "{field} must not be empty"
                            )));
                        }
                        let mut checked = Vec::with_capacity(list.len());
                        for value in list {
                            checked.push(require_text(value, field, MAX_SLUG_LEN)?);
                        }
                        Ok(Some(checked))
                    }
                    None => Ok(None),
                }
            };
        if let Some(categories) = validate_list(&input.categories, "categories")? {
            settings.categories = categories;
        }
        if let Some(languages) = validate_list(&input.languages, "languages")? {
            settings.languages = languages;
        }
        if let Some(member_types) = validate_list(&input.member_types, "member_types")? {
            settings.member_types = member_types;
        }
        if let Some(item_types) = validate_list(&input.item_types, "item_types")? {
            settings.item_types = item_types;
        }
        if let Some(item_statuses) = validate_list(&input.item_statuses, "item_statuses")? {
            settings.item_statuses = item_statuses;
        }
        if let Some(copy_statuses) = validate_list(&input.copy_statuses, "copy_statuses")? {
            settings.copy_statuses = copy_statuses;
        }
        if let Some(copy_conditions) = validate_list(&input.copy_conditions, "copy_conditions")? {
            settings.copy_conditions = copy_conditions;
        }
        if let Some(loan_statuses) = validate_list(&input.loan_statuses, "loan_statuses")? {
            settings.loan_statuses = loan_statuses;
        }
        if let Some(hold_statuses) = validate_list(&input.hold_statuses, "hold_statuses")? {
            settings.hold_statuses = hold_statuses;
        }
        if let Some(fine_statuses) = validate_list(&input.fine_statuses, "fine_statuses")? {
            settings.fine_statuses = fine_statuses;
        }
        if let Some(kinds) = validate_list(&input.discrepancy_kinds, "discrepancy_kinds")? {
            settings.discrepancy_kinds = kinds;
        }
        if let Some(currency) = &input.default_currency {
            settings.default_currency = require_currency(currency)?;
        }
        if let Some(days) = input.hold_expiry_days {
            settings.hold_expiry_days = require_positive(days, "hold_expiry_days")?;
        }
        if let Some(ttl) = input.digital_link_ttl_secs {
            if ttl == 0 || ttl > 604_800 {
                return Err(UnifiedError::bad_request(
                    "digital_link_ttl_secs must be between 1 and 604800",
                ));
            }
            settings.digital_link_ttl_secs = ttl;
        }
        if let Some(limit) = input.public_search_rate_limit_per_minute {
            if limit > 10_000 {
                return Err(UnifiedError::bad_request(
                    "public_search_rate_limit_per_minute must be at most 10000",
                ));
            }
            settings.public_search_rate_limit_per_minute = limit;
        }
        if let Some(days) = input.reminder_days_before_due {
            settings.reminder_days_before_due = require_non_negative(days, "reminder_days_before_due")?;
        }
        if let Some(block) = input.block_checkout_when_overdue {
            settings.block_checkout_when_overdue = block;
        }
        if let Some(allow) = input.allow_renew_when_overdue {
            settings.allow_renew_when_overdue = allow;
        }
        if let Some(max) = input.max_grounding_matches {
            if max == 0 || max > 50 {
                return Err(UnifiedError::bad_request(
                    "max_grounding_matches must be between 1 and 50",
                ));
            }
            settings.max_grounding_matches = max;
        }
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        state.emit(
            "library.settings.updated",
            json!({
                "member_types": settings.member_types,
                "item_types": settings.item_types,
                "default_currency": settings.default_currency,
                "actor": caller.actor,
            }),
        );
        Ok(settings)
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// AI handlers
// ---------------------------------------------------------------------------

pub(crate) async fn ai_search_assist(
    ctx: web::Data<Arc<AppContext>>,
    _state: web::Data<LibraryState>,
    req: HttpRequest,
    body: web::Json<Value>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return err(error, debug),
    };
    let scope = match super::auth::caller_company(&tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, debug),
    };
    let caller = match caller(&req) {
        Ok(value) => value,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW) {
        return err(error, debug);
    }
    // The company id the AI gate enforces is bound to the scope
    // resolved above, never to a client-asserted header: a scope
    // company that does not parse is a data error and fails closed
    // instead of silently degrading to an unscoped call.
    let company_id = match scope.company() {
        Some(raw) => match Uuid::parse_str(raw) {
            Ok(id) => Some(id),
            Err(_) => {
                return err(
                    UnifiedError::forbidden(
                        "Caller company scope is not a valid company id",
                    ),
                    debug,
                );
            }
        },
        None => None,
    };
    // The AI gate runs inside the helper; this handler never calls
    // a provider directly and never bypasses the gate.
    // The AI module's Caller is the type `run_ai_capability`
    // takes; it carries the same actor and role as this module's
    // local caller.
    let ai_caller = crate::modules::ai::Caller {
        actor: caller.actor.clone(),
        role: caller.role.clone(),
    };
    let delegated = crate::modules::ai::run_ai_capability(
        &ctx,
        &ai_caller,
        company_id,
        "library",
        "search_assist",
        input,
    )
    .await;
    match delegated {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn ai_recommendations(
    ctx: web::Data<Arc<AppContext>>,
    _state: web::Data<LibraryState>,
    req: HttpRequest,
    body: web::Json<Value>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return err(error, debug),
    };
    let scope = match super::auth::caller_company(&tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, debug),
    };
    let caller = match caller(&req) {
        Ok(value) => value,
        Err(error) => return err(error, debug),
    };
    let gate: UnifiedResult<()> = (|| {
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        if let Some(user_id) = opt_string(&input, "user_id") {
            if user_id != caller.actor {
                require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
            }
        }
        Ok(())
    })();
    if let Err(error) = gate {
        return err(error, debug);
    }
    // The company id the AI gate enforces is bound to the scope
    // resolved above, never to a client-asserted header: a scope
    // company that does not parse is a data error and fails closed
    // instead of silently degrading to an unscoped call.
    let company_id = match scope.company() {
        Some(raw) => match Uuid::parse_str(raw) {
            Ok(id) => Some(id),
            Err(_) => {
                return err(
                    UnifiedError::forbidden(
                        "Caller company scope is not a valid company id",
                    ),
                    debug,
                );
            }
        },
        None => None,
    };
    // The AI module's Caller is the type `run_ai_capability`
    // takes; it carries the same actor and role as this module's
    // local caller.
    let ai_caller = crate::modules::ai::Caller {
        actor: caller.actor.clone(),
        role: caller.role.clone(),
    };
    let delegated = crate::modules::ai::run_ai_capability(
        &ctx,
        &ai_caller,
        company_id,
        "library",
        "recommendations",
        input,
    )
    .await;
    match delegated {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
