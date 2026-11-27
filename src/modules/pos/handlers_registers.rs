// Pos module - register and shift handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};
use uuid::Uuid;
use super::*;

// Register handlers

pub(crate) async fn list_registers(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<PosRegister> = state
            .registers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|register| {
                if let Some(status) = &status_filter {
                    if register.status != *status {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {} {}", register.name, register.code,
                        register.location.clone().unwrap_or_default()).to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["name", "code", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                "code" => a.code.to_lowercase().cmp(&b.code.to_lowercase()),
                _ => a.created_at.cmp(&b.created_at),
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

pub(crate) async fn create_register(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    body: web::Json<CreateRegisterRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosRegister> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let code = match &input.code {
            Some(raw) => require_text(raw, "code", MAX_CODE_LEN)?.to_uppercase(),
            None => format!("REG-{}", &new_id()[..8].to_uppercase()),
        };
        if state
            .registers
            .iter()
            .any(|entry| entry.value().code == code)
        {
            return Err(UnifiedError::conflict(format!(
                "Register code already exists: {code}"
            )));
        }
        let location = optional_text(&input.location, "location", MAX_LOCATION_LEN)?;
        let register = PosRegister {
            id: new_id(),
            name,
            code,
            location,
            status: "active".to_string(),
            cashiers: Vec::new(),
            created_at: now(),
            updated_at: now(),
        };
        state.registers.insert(register.id.clone(), register.clone());
        state.emit(
            "pos.register.created",
            json!({
                "register_id": register.id,
                "code": register.code,
                "name": register.name,
                "created_by": caller.actor,
            }),
        );
        Ok(register)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_register(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosRegister> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        load_register(&state, &path.into_inner())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_register(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRegisterRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosRegister> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_MANAGE)?;
        let id = path.into_inner();
        let input = body.into_inner();
        let mut register = load_register(&state, &id)?;
        if let Some(name) = &input.name {
            register.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(location) = &input.location {
            register.location = match location {
                Some(text) => Some(require_text(text, "location", MAX_LOCATION_LEN)?),
                None => None,
            };
        }
        if let Some(status) = &input.status {
            let status = status.trim().to_lowercase();
            if !matches!(status.as_str(), "active" | "inactive") {
                return Err(UnifiedError::bad_request(
                    "status must be one of: active, inactive",
                ));
            }
            if status == "inactive" && open_shift_for_register(&state, &id).is_some() {
                return Err(UnifiedError::unprocessable(
                    "Cannot deactivate a register with an open shift",
                ));
            }
            register.status = status;
        }
        register.updated_at = now();
        state.registers.insert(register.id.clone(), register.clone());
        state.emit(
            "pos.register.updated",
            json!({
                "register_id": register.id,
                "status": register.status,
                "updated_by": caller.actor,
            }),
        );
        Ok(register)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_register(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_MANAGE)?;
        let id = path.into_inner();
        let register = load_register(&state, &id)?;
        if open_shift_for_register(&state, &id).is_some() {
            return Err(UnifiedError::unprocessable(
                "Cannot delete a register with an open shift",
            ));
        }
        if state
            .sales
            .iter()
            .any(|entry| entry.value().register_id == id)
        {
            return Err(UnifiedError::unprocessable(
                "Cannot delete a register that has sales; deactivate it instead",
            ));
        }
        state.registers.remove(&id);
        state.emit(
            "pos.register.deleted",
            json!({
                "register_id": id,
                "code": register.code,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "register_id": id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn assign_cashier(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AssignCashierRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosRegister> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_MANAGE)?;
        let id = path.into_inner();
        let user_id = require_text(&body.into_inner().user_id, "user_id", MAX_CODE_LEN)?;
        let mut register = load_register(&state, &id)?;
        if !register.cashiers.iter().any(|cashier| cashier == &user_id) {
            register.cashiers.push(user_id.clone());
        }
        register.updated_at = now();
        state.registers.insert(register.id.clone(), register.clone());
        state.emit(
            "pos.register.cashier_assigned",
            json!({
                "register_id": register.id,
                "user_id": user_id,
                "assigned_by": caller.actor,
            }),
        );
        Ok(register)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn unassign_cashier(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AssignCashierRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosRegister> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_MANAGE)?;
        let id = path.into_inner();
        let user_id = require_text(&body.into_inner().user_id, "user_id", MAX_CODE_LEN)?;
        let mut register = load_register(&state, &id)?;
        // A cashier with an open shift on this register cannot be
        // removed until the shift is closed.
        if let Some(shift) = open_shift_for_register(&state, &id) {
            if shift.cashier_id == user_id {
                return Err(UnifiedError::unprocessable(
                    "Cannot remove a cashier with an open shift on this register",
                ));
            }
        }
        register.cashiers.retain(|cashier| cashier != &user_id);
        register.updated_at = now();
        state.registers.insert(register.id.clone(), register.clone());
        state.emit(
            "pos.register.cashier_unassigned",
            json!({
                "register_id": register.id,
                "user_id": user_id,
                "unassigned_by": caller.actor,
            }),
        );
        Ok(register)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Shift handlers

pub(crate) async fn list_shifts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        let params = query.into_inner();
        let register_filter = params.filters.get("register_id").cloned();
        let cashier_filter = params.filters.get("cashier_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<PosShift> = state
            .shifts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|shift| {
                if let Some(register_id) = &register_filter {
                    if shift.register_id != *register_id {
                        return false;
                    }
                }
                if let Some(cashier_id) = &cashier_filter {
                    if shift.cashier_id != *cashier_id {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if shift.status != *status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.opened_at.cmp(&a.opened_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn open_shift(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    body: web::Json<OpenShiftRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosShift> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let input = body.into_inner();
        let register = load_register(&state, &input.register_id)?;
        if register.status != "active" {
            return Err(UnifiedError::unprocessable(
                "Cannot open a shift on an inactive register",
            ));
        }
        caller_may_sell(&ctx, &state, &register, &caller)?;
        if open_shift_for_register(&state, &register.id).is_some() {
            return Err(UnifiedError::conflict(
                "Register already has an open shift",
            ));
        }
        let opening_float = valid_money(input.opening_float.unwrap_or(0), "opening_float")?;
        let currency = match &input.currency {
            Some(raw) => valid_currency(raw)?,
            None => state.settings().default_currency,
        };
        let notes = optional_text(&input.notes, "notes", MAX_NOTES_LEN)?;
        let shift = PosShift {
            id: new_id(),
            register_id: register.id.clone(),
            cashier_id: caller.actor.clone(),
            status: "open".to_string(),
            opening_float,
            currency,
            expected_cash: None,
            counted_cash: None,
            variance: None,
            notes,
            opened_at: now(),
            closed_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state.shifts.insert(shift.id.clone(), shift.clone());
        state.emit(
            "pos.shift.opened",
            json!({
                "shift_id": shift.id,
                "register_id": shift.register_id,
                "cashier_id": shift.cashier_id,
                "opening_float": minor_to_major(shift.opening_float, &shift.currency),
                "currency": shift.currency,
            }),
        );
        Ok(shift)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_shift(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosShift> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        load_shift(&state, &path.into_inner())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn close_shift(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CloseShiftRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let id = path.into_inner();
        let input = body.into_inner();
        let mut shift = load_shift(&state, &id)?;
        if shift.status != "open" {
            return Err(UnifiedError::unprocessable(
                "Shift is already closed",
            ));
        }
        if shift.cashier_id != caller.actor
            && !check_capability(&ctx, &caller.role, PERM_POS_MANAGE)
        {
            return Err(UnifiedError::forbidden(
                "Only the shift cashier or a manager may close this shift",
            ));
        }
        // A shift with open (draft or held) sales cannot close:
        // every cart must be completed or voided first.
        let open_sales = state
            .sales
            .iter()
            .filter(|entry| {
                entry.value().shift_id.as_deref() == Some(shift.id.as_str())
                    && matches!(entry.value().status.as_str(), "draft" | "held")
            })
            .count();
        if open_sales > 0 {
            return Err(UnifiedError::unprocessable(format!(
                "Shift has {open_sales} open sale(s); complete or void them before closing"
            )));
        }
        let counted_cash = valid_money(input.counted_cash, "counted_cash")?;
        let expected_cash = expected_cash_for_shift(&state, &shift);
        let variance = counted_cash - expected_cash;
        shift.expected_cash = Some(expected_cash);
        shift.counted_cash = Some(counted_cash);
        shift.variance = Some(variance);
        shift.status = "closed".to_string();
        shift.closed_at = Some(now());
        if let Some(notes) = optional_text(&input.notes, "notes", MAX_NOTES_LEN)? {
            shift.notes = Some(notes);
        }
        shift.updated_at = now();
        state.shifts.insert(shift.id.clone(), shift.clone());
        let summary = shift_summary(&state, &shift);
        state.emit(
            "pos.shift.closed",
            json!({
                "shift_id": shift.id,
                "register_id": shift.register_id,
                "cashier_id": shift.cashier_id,
                "expected_cash": minor_to_major(expected_cash, &shift.currency),
                "counted_cash": minor_to_major(counted_cash, &shift.currency),
                "variance": minor_to_major(variance, &shift.currency),
                "currency": shift.currency,
                "closed_by": caller.actor,
            }),
        );
        Ok(json!({ "shift": shift, "summary": summary }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_shift_summary(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        let shift = load_shift(&state, &path.into_inner())?;
        Ok(shift_summary(&state, &shift))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
