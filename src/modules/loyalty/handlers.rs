//! Loyalty - loyalty handlers.
//!
//! Part of the `loyalty` module split; see `src/modules/loyalty.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use unified_core::json;
use unified_core::pagination::PageResult;
use unified_core::pagination::PaginationParams;
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use actix_web::web;
use chrono::Utc;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use super::constants::DEFAULT_DISCOUNT_PER_POINT;
use super::constants::MAX_COMPANY_ID_LEN;
use super::constants::MAX_DESCRIPTION_LEN;
use super::constants::MAX_NAME_LEN;
use super::constants::MAX_ORDER_ID_LEN;
use super::constants::MAX_POINTS_NAME_LEN;
use super::constants::MAX_PRODUCT_ID_LEN;
use super::constants::MAX_REASON_LEN;
use super::constants::MAX_USER_ID_LEN;
use super::constants::PERM_LOYALTY_MANAGE;
use super::constants::PERM_LOYALTY_VIEW;
use super::helpers::append_ledger;
use super::helpers::balance_view;
use super::helpers::caller;
use super::helpers::{caller_company, require_company_for_write};
use super::helpers::created;
use super::helpers::err;
use super::helpers::expire_program;
use super::helpers::in_company_scope;
use super::helpers::initial_redemption_status;
use super::helpers::is_manager;
use super::helpers::lifetime_points;
use super::helpers::lifetime_spend;
use super::helpers::load_earn_rule;
use super::helpers::load_redemption;
use super::helpers::load_tier;
use super::helpers::ok;
use super::helpers::paginate;
use super::helpers::require_company_match;
use super::helpers::require_customer_access;
use super::helpers::require_permission;
use super::helpers::require_program_in_scope;
use super::helpers::require_tenant;
use super::helpers::require_text;
use super::helpers::round_money;
use super::helpers::tier_for;
use super::helpers::valid_currency;
use super::helpers::valid_price;
use super::types::AdjustPointsRequest;
use super::types::CreateEarnRuleRequest;
use super::types::CreateProgramRequest;
use super::types::CreateTierRequest;
use super::types::EarnPointsRequest;
use super::types::ExpiryMode;
use super::types::LedgerKind;
use super::types::LoyaltyEarnRule;
use super::types::LoyaltyLedgerEntry;
use super::types::LoyaltyProgram;
use super::types::LoyaltyRedemption;
use super::types::LoyaltyState;
use super::types::LoyaltyTier;
use super::types::RedeemRequest;
use super::types::UpdateEarnRuleRequest;
use super::types::UpdateProgramRequest;
use super::types::UpdateTierRequest;

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_programs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<LoyaltyProgram>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_VIEW)?;
        let params = query.into_inner();
        let company_filter = params.filters.get("company_id").cloned();
        let active_filter = params.filters.get("active").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<LoyaltyProgram> = state
            .programs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|program| {
                if !in_company_scope(&program.company_id, &_scope) {
                    return false;
                }
                if let Some(company_id) = &company_filter {
                    if program.company_id.as_deref() != Some(company_id.as_str()) {
                        return false;
                    }
                }
                if let Some(raw) = &active_filter {
                    if program.active.to_string() != *raw {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !program.name.to_lowercase().contains(&search)
                    && !program.points_name.to_lowercase().contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_program(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    body: web::Json<CreateProgramRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyProgram> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let points_name = match &input.points_name {
            Some(raw) => require_text(raw, "points_name", MAX_POINTS_NAME_LEN)?,
            None => "points".to_string(),
        };
        let currency = match &input.currency {
            Some(raw) => valid_currency(raw)?,
            None => "USD".to_string(),
        };
        let discount_per_point = match input.discount_per_point {
            Some(raw) => valid_price(raw)?,
            None => DEFAULT_DISCOUNT_PER_POINT,
        };
        let body_company = match &input.company_id {
            Some(raw) => Some(require_text(raw, "company_id", MAX_COMPANY_ID_LEN)?),
            None => None,
        };
        let company_id = require_company_match(&_tenant, &req, &body_company)?;
        let expiry_days = match input.expiry_days {
            Some(days) if days < 1 => {
                return Err(UnifiedError::bad_request(
                    "expiry_days must be at least 1 when set",
                ));
            }
            other => other,
        };
        let active = input.active.unwrap_or(false);
        let now = Utc::now();
        let program = LoyaltyProgram {
            id: Uuid::new_v4(),
            company_id: company_id.clone(),
            name,
            description,
            points_name,
            currency,
            discount_per_point,
            active,
            expiry_days,
            expiry_mode: input.expiry_mode.unwrap_or(ExpiryMode::Fixed),
            created_at: now,
            updated_at: now,
        };
        // One active program at a time per company: activating this
        // one deactivates the others in the same company group.
        if active {
            for mut entry in state.programs.iter_mut() {
                let other = entry.value_mut();
                if other.id != program.id && other.active && other.company_id == company_id {
                    other.active = false;
                    other.updated_at = now;
                }
            }
        }
        state.programs.insert(program.id, program.clone());
        state.emit(
            "loyalty.program.created",
            json!({
                "program_id": program.id,
                "company_id": company_id,
                "active": program.active,
            }),
        );
        Ok(program)
    })();
    match result {
        Ok(program) => created(program),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_program(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyProgram> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_VIEW)?;
        require_program_in_scope(&state, &path, &_scope)
    })();
    match result {
        Ok(program) => ok(program),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_program(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateProgramRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyProgram> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let input = body.into_inner();
        let mut program = require_program_in_scope(&state, &path, &_scope)?;
        if let Some(raw) = &input.name {
            program.name = require_text(raw, "name", MAX_NAME_LEN)?;
        }
        if let Some(raw) = &input.description {
            program.description = Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?);
        }
        if let Some(raw) = &input.points_name {
            program.points_name = require_text(raw, "points_name", MAX_POINTS_NAME_LEN)?;
        }
        if let Some(raw) = &input.currency {
            program.currency = valid_currency(raw)?;
        }
        if let Some(raw) = input.discount_per_point {
            program.discount_per_point = valid_price(raw)?;
        }
        if let Some(days) = input.expiry_days {
            if days < 1 {
                return Err(UnifiedError::bad_request(
                    "expiry_days must be at least 1 when set",
                ));
            }
            program.expiry_days = Some(days);
        }
        if let Some(mode) = input.expiry_mode {
            program.expiry_mode = mode;
        }
        program.updated_at = Utc::now();
        state.programs.insert(program.id, program.clone());
        Ok(program)
    })();
    match result {
        Ok(program) => ok(program),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_program(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let program = require_program_in_scope(&state, &path, &_scope)?;
        if state
            .ledger
            .iter()
            .any(|entry| entry.value().program_id == program.id)
        {
            return Err(UnifiedError::conflict(
                "Program has ledger rows and cannot be deleted; deactivate it instead",
            ));
        }
        state
            .earn_rules
            .retain(|_, rule| rule.program_id != program.id);
        state.tiers.retain(|_, tier| tier.program_id != program.id);
        state
            .redemptions
            .retain(|_, redemption| redemption.program_id != program.id);
        state.programs.remove(&program.id);
        Ok(json!({ "deleted": true, "id": program.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn activate_program(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyProgram> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let mut program = require_program_in_scope(&state, &path, &_scope)?;
        let now = Utc::now();
        for mut entry in state.programs.iter_mut() {
            let other = entry.value_mut();
            if other.id != program.id && other.active && other.company_id == program.company_id {
                other.active = false;
                other.updated_at = now;
            }
        }
        program.active = true;
        program.updated_at = now;
        state.programs.insert(program.id, program.clone());
        state.emit(
            "loyalty.program.activated",
            json!({
                "program_id": program.id,
                "company_id": program.company_id,
            }),
        );
        Ok(program)
    })();
    match result {
        Ok(program) => ok(program),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn deactivate_program(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyProgram> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let mut program = require_program_in_scope(&state, &path, &_scope)?;
        program.active = false;
        program.updated_at = Utc::now();
        state.programs.insert(program.id, program.clone());
        state.emit(
            "loyalty.program.deactivated",
            json!({
                "program_id": program.id,
                "company_id": program.company_id,
            }),
        );
        Ok(program)
    })();
    match result {
        Ok(program) => ok(program),
        Err(error) => err(error, debug),
    }
}

fn validate_rule_values(
    points_per_unit: f64,
    fixed_points: Option<i64>,
    cap_per_day: Option<i64>,
    cap_total: Option<i64>,
    min_order_amount: Option<f64>,
) -> UnifiedResult<()> {
    if !points_per_unit.is_finite() || points_per_unit < 0.0 {
        return Err(UnifiedError::bad_request(
            "points_per_unit must be a finite number >= 0",
        ));
    }
    if let Some(fixed) = fixed_points {
        if fixed < 0 {
            return Err(UnifiedError::bad_request("fixed_points must be >= 0"));
        }
    }
    if let Some(cap) = cap_per_day {
        if cap < 1 {
            return Err(UnifiedError::bad_request("cap_per_day must be at least 1"));
        }
    }
    if let Some(cap) = cap_total {
        if cap < 1 {
            return Err(UnifiedError::bad_request("cap_total must be at least 1"));
        }
    }
    if let Some(minimum) = min_order_amount {
        if !minimum.is_finite() || minimum < 0.0 {
            return Err(UnifiedError::bad_request(
                "min_order_amount must be a finite number >= 0",
            ));
        }
    }
    Ok(())
}

pub(crate) async fn list_earn_rules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<LoyaltyEarnRule>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_VIEW)?;
        let program = require_program_in_scope(&state, &path, &_scope)?;
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let active_filter = params.filters.get("active").cloned();
        let mut items: Vec<LoyaltyEarnRule> = state
            .earn_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| {
                rule.program_id == program.id
                    && kind_filter
                        .as_deref()
                        .is_none_or(|raw| rule.kind.as_str() == raw)
                    && active_filter
                        .as_deref()
                        .is_none_or(|raw| rule.active.to_string() == raw)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_earn_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateEarnRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyEarnRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let program = require_program_in_scope(&state, &path, &_scope)?;
        let input = body.into_inner();
        let points_per_unit = input.points_per_unit.unwrap_or(0.0);
        validate_rule_values(
            points_per_unit,
            input.fixed_points,
            input.cap_per_day,
            input.cap_total,
            input.min_order_amount,
        )?;
        if input.active.unwrap_or(true)
            && state.earn_rules.iter().any(|entry| {
                let rule = entry.value();
                rule.program_id == program.id && rule.kind == input.kind && rule.active
            })
        {
            return Err(UnifiedError::conflict(format!(
                "An active earn rule already exists for kind \"{}\" on this program",
                input.kind.as_str()
            )));
        }
        let now = Utc::now();
        let rule = LoyaltyEarnRule {
            id: Uuid::new_v4(),
            program_id: program.id,
            kind: input.kind,
            points_per_unit,
            fixed_points: input.fixed_points,
            cap_per_day: input.cap_per_day,
            cap_total: input.cap_total,
            min_order_amount: input.min_order_amount,
            active: input.active.unwrap_or(true),
            created_at: now,
            updated_at: now,
        };
        state.earn_rules.insert(rule.id, rule.clone());
        Ok(rule)
    })();
    match result {
        Ok(rule) => created(rule),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_earn_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyEarnRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_VIEW)?;
        let (program_id, rule_id) = path.into_inner();
        let program = require_program_in_scope(&state, &program_id, &_scope)?;
        load_earn_rule(&state, program.id, &rule_id)
    })();
    match result {
        Ok(rule) => ok(rule),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_earn_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateEarnRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyEarnRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let (program_id, rule_id) = path.into_inner();
        let program = require_program_in_scope(&state, &program_id, &_scope)?;
        let input = body.into_inner();
        let mut rule = load_earn_rule(&state, program.id, &rule_id)?;
        let points_per_unit = input.points_per_unit.unwrap_or(rule.points_per_unit);
        let fixed_points = input.fixed_points.or(rule.fixed_points);
        let cap_per_day = input.cap_per_day.or(rule.cap_per_day);
        let cap_total = input.cap_total.or(rule.cap_total);
        let min_order_amount = input.min_order_amount.or(rule.min_order_amount);
        validate_rule_values(
            points_per_unit,
            fixed_points,
            cap_per_day,
            cap_total,
            min_order_amount,
        )?;
        if input.active == Some(true)
            && state.earn_rules.iter().any(|entry| {
                let other = entry.value();
                other.program_id == program.id
                    && other.kind == rule.kind
                    && other.active
                    && other.id != rule.id
            })
        {
            return Err(UnifiedError::conflict(format!(
                "An active earn rule already exists for kind \"{}\" on this program",
                rule.kind.as_str()
            )));
        }
        rule.points_per_unit = points_per_unit;
        rule.fixed_points = fixed_points;
        rule.cap_per_day = cap_per_day;
        rule.cap_total = cap_total;
        rule.min_order_amount = min_order_amount;
        if let Some(active) = input.active {
            rule.active = active;
        }
        rule.updated_at = Utc::now();
        state.earn_rules.insert(rule.id, rule.clone());
        Ok(rule)
    })();
    match result {
        Ok(rule) => ok(rule),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_earn_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let (program_id, rule_id) = path.into_inner();
        let program = require_program_in_scope(&state, &program_id, &_scope)?;
        let rule = load_earn_rule(&state, program.id, &rule_id)?;
        state.earn_rules.remove(&rule.id);
        Ok(json!({ "deleted": true, "id": rule.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_tiers(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<LoyaltyTier>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_VIEW)?;
        let program = require_program_in_scope(&state, &path, &_scope)?;
        let params = query.into_inner();
        let mut items: Vec<LoyaltyTier> = state
            .tiers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|tier| tier.program_id == program.id)
            .collect();
        items.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then(a.min_lifetime_points.cmp(&b.min_lifetime_points))
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_tier(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateTierRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyTier> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let program = require_program_in_scope(&state, &path, &_scope)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let min_lifetime_points = input.min_lifetime_points.unwrap_or(0);
        if min_lifetime_points < 0 {
            return Err(UnifiedError::bad_request(
                "min_lifetime_points must be >= 0",
            ));
        }
        let min_lifetime_spend = match input.min_lifetime_spend {
            Some(raw) => valid_price(raw)?,
            None => 0.0,
        };
        let discount_percent = match input.discount_percent {
            Some(raw) => {
                if !raw.is_finite() || !(0.0..=100.0).contains(&raw) {
                    return Err(UnifiedError::bad_request(
                        "discount_percent must be between 0 and 100",
                    ));
                }
                raw
            }
            None => 0.0,
        };
        let benefits = match input.benefits {
            Some(raw) => {
                let mut cleaned = Vec::new();
                for benefit in raw {
                    cleaned.push(require_text(&benefit, "benefits entry", MAX_NAME_LEN)?);
                }
                if cleaned.len() > 20 {
                    return Err(UnifiedError::bad_request(
                        "benefits must have at most 20 entries",
                    ));
                }
                cleaned
            }
            None => Vec::new(),
        };
        let now = Utc::now();
        let tier = LoyaltyTier {
            id: Uuid::new_v4(),
            program_id: program.id,
            name,
            min_lifetime_points,
            min_lifetime_spend,
            benefits,
            discount_percent,
            sort_order: input.sort_order.unwrap_or(0),
            created_at: now,
            updated_at: now,
        };
        state.tiers.insert(tier.id, tier.clone());
        Ok(tier)
    })();
    match result {
        Ok(tier) => created(tier),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_tier(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyTier> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_VIEW)?;
        let (program_id, tier_id) = path.into_inner();
        let program = require_program_in_scope(&state, &program_id, &_scope)?;
        load_tier(&state, program.id, &tier_id)
    })();
    match result {
        Ok(tier) => ok(tier),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_tier(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateTierRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyTier> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let (program_id, tier_id) = path.into_inner();
        let program = require_program_in_scope(&state, &program_id, &_scope)?;
        let input = body.into_inner();
        let mut tier = load_tier(&state, program.id, &tier_id)?;
        if let Some(raw) = &input.name {
            tier.name = require_text(raw, "name", MAX_NAME_LEN)?;
        }
        if let Some(value) = input.min_lifetime_points {
            if value < 0 {
                return Err(UnifiedError::bad_request(
                    "min_lifetime_points must be >= 0",
                ));
            }
            tier.min_lifetime_points = value;
        }
        if let Some(raw) = input.min_lifetime_spend {
            tier.min_lifetime_spend = valid_price(raw)?;
        }
        if let Some(raw) = input.benefits {
            let mut cleaned = Vec::new();
            for benefit in raw {
                cleaned.push(require_text(&benefit, "benefits entry", MAX_NAME_LEN)?);
            }
            if cleaned.len() > 20 {
                return Err(UnifiedError::bad_request(
                    "benefits must have at most 20 entries",
                ));
            }
            tier.benefits = cleaned;
        }
        if let Some(raw) = input.discount_percent {
            if !raw.is_finite() || !(0.0..=100.0).contains(&raw) {
                return Err(UnifiedError::bad_request(
                    "discount_percent must be between 0 and 100",
                ));
            }
            tier.discount_percent = raw;
        }
        if let Some(value) = input.sort_order {
            tier.sort_order = value;
        }
        tier.updated_at = Utc::now();
        state.tiers.insert(tier.id, tier.clone());
        Ok(tier)
    })();
    match result {
        Ok(tier) => ok(tier),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_tier(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let (program_id, tier_id) = path.into_inner();
        let program = require_program_in_scope(&state, &program_id, &_scope)?;
        let tier = load_tier(&state, program.id, &tier_id)?;
        state.tiers.remove(&tier.id);
        Ok(json!({ "deleted": true, "id": tier.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn earn_points(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<EarnPointsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyLedgerEntry> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let program = require_program_in_scope(&state, &path, &_scope)?;
        let input = body.into_inner();
        let customer_id = require_text(&input.customer_id, "customer_id", MAX_USER_ID_LEN)?;
        if input.points < 1 {
            return Err(UnifiedError::bad_request("points must be at least 1"));
        }
        let reason = require_text(&input.reason, "reason", MAX_REASON_LEN)?;
        let earn_kind = match &input.earn_kind {
            Some(raw) => {
                let kind = raw.trim().to_lowercase();
                if !["purchase", "referral", "review", "signup"].contains(&kind.as_str()) {
                    return Err(UnifiedError::bad_request(
                        "earn_kind must be one of: purchase, referral, review, signup",
                    ));
                }
                Some(kind)
            }
            None => None,
        };
        let reference_id = match &input.reference_id {
            Some(raw) => Some(require_text(raw, "reference_id", MAX_ORDER_ID_LEN)?),
            None => None,
        };
        let tier_before = tier_for(&state, program.id, &customer_id).map(|tier| tier.id);
        let now = Utc::now();
        let entry = append_ledger(
            &state,
            &program,
            &customer_id,
            LedgerKind::Earn,
            input.points,
            &reason,
            &caller.actor,
            earn_kind.clone(),
            reference_id,
            None,
            now,
        )?;
        state.emit(
            "loyalty.points.earned",
            json!({
                "program_id": program.id,
                "customer_id": customer_id,
                "points": input.points,
                "balance_after": entry.balance_after,
                "earn_kind": earn_kind,
                "reason": reason,
            }),
        );
        let tier_after = tier_for(&state, program.id, &customer_id).map(|tier| tier.id);
        if tier_before != tier_after {
            state.emit(
                "loyalty.tier.changed",
                json!({
                    "program_id": program.id,
                    "customer_id": customer_id,
                    "previous_tier_id": tier_before,
                    "tier_id": tier_after,
                }),
            );
        }
        Ok(entry)
    })();
    match result {
        Ok(entry) => created(entry),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn adjust_points(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AdjustPointsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyLedgerEntry> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let program = require_program_in_scope(&state, &path, &_scope)?;
        let input = body.into_inner();
        let customer_id = require_text(&input.customer_id, "customer_id", MAX_USER_ID_LEN)?;
        if input.points == 0 {
            return Err(UnifiedError::bad_request("points must not be zero"));
        }
        let reason = require_text(&input.reason, "reason", MAX_REASON_LEN)?;
        let tier_before = tier_for(&state, program.id, &customer_id).map(|tier| tier.id);
        let now = Utc::now();
        let entry = append_ledger(
            &state,
            &program,
            &customer_id,
            LedgerKind::Adjust,
            input.points,
            &reason,
            &caller.actor,
            None,
            None,
            None,
            now,
        )?;
        state.emit(
            "loyalty.points.adjusted",
            json!({
                "program_id": program.id,
                "customer_id": customer_id,
                "points": input.points,
                "balance_after": entry.balance_after,
                "reason": reason,
            }),
        );
        let tier_after = tier_for(&state, program.id, &customer_id).map(|tier| tier.id);
        if tier_before != tier_after {
            state.emit(
                "loyalty.tier.changed",
                json!({
                    "program_id": program.id,
                    "customer_id": customer_id,
                    "previous_tier_id": tier_before,
                    "tier_id": tier_after,
                }),
            );
        }
        Ok(entry)
    })();
    match result {
        Ok(entry) => created(entry),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn expire_points(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let program = require_program_in_scope(&state, &path, &_scope)?;
        if program.expiry_days.is_none() {
            return Err(UnifiedError::bad_request(
                "Program has no expiry_days set; points in this program never expire",
            ));
        }
        let now = Utc::now();
        let (customers, points) = expire_program(&state, &program, &caller.actor, now);
        Ok(json!({
            "program_id": program.id,
            "customers_expired": customers,
            "points_expired": points,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_balance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_VIEW)?;
        let (program_id, customer_id) = path.into_inner();
        require_customer_access(&ctx, &caller, &customer_id)?;
        let program = require_program_in_scope(&state, &program_id, &_scope)?;
        Ok(balance_view(&state, &program, &customer_id))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn my_balance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_VIEW)?;
        let program = require_program_in_scope(&state, &path, &_scope)?;
        Ok(balance_view(&state, &program, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_tier_for_customer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_VIEW)?;
        let (program_id, customer_id) = path.into_inner();
        require_customer_access(&ctx, &caller, &customer_id)?;
        let program = require_program_in_scope(&state, &program_id, &_scope)?;
        let tier = tier_for(&state, program.id, &customer_id);
        Ok(json!({
            "program_id": program.id,
            "customer_id": customer_id,
            "lifetime_points": lifetime_points(&state, program.id, &customer_id),
            "lifetime_spend": round_money(lifetime_spend(&state, program.id, &customer_id)),
            "tier": tier,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_ledger(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<LoyaltyLedgerEntry>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_VIEW)?;
        let program = require_program_in_scope(&state, &path, &_scope)?;
        let params = query.into_inner();
        let customer_filter = params.filters.get("customer_id").cloned();
        let kind_filter = params.filters.get("kind").cloned();
        let earn_kind_filter = params.filters.get("earn_kind").cloned();
        let manager = is_manager(&ctx, &caller.role);
        // A non-manager only ever sees their own rows, whatever
        // filter they asked for.
        let effective_customer: Option<String> = if manager {
            customer_filter.clone()
        } else {
            Some(caller.actor.clone())
        };
        if let Some(requested) = &customer_filter {
            if !manager && requested != &caller.actor {
                return Err(UnifiedError::forbidden(
                    "A customer may only access their own loyalty records",
                ));
            }
        }
        let mut items: Vec<LoyaltyLedgerEntry> = state
            .ledger
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                row.program_id == program.id
                    && effective_customer
                        .as_deref()
                        .is_none_or(|customer_id| row.customer_id == customer_id)
                    && kind_filter
                        .as_deref()
                        .is_none_or(|raw| row.kind.as_str() == raw)
                    && earn_kind_filter
                        .as_deref()
                        .is_none_or(|raw| row.earn_kind.as_deref() == Some(raw))
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn redeem_points(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RedeemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyRedemption> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_VIEW)?;
        let program = require_program_in_scope(&state, &path, &_scope)?;
        let input = body.into_inner();
        // A customer redeems their own points; a manager may redeem
        // on behalf of any customer.
        let customer_id = match &input.customer_id {
            Some(other) if other.trim() != caller.actor => {
                require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
                require_text(other, "customer_id", MAX_USER_ID_LEN)?
            }
            _ => caller.actor.clone(),
        };
        if input.points < 1 {
            return Err(UnifiedError::bad_request("points must be at least 1"));
        }
        let kind = input.kind.trim().to_lowercase();
        if kind != "discount" && kind != "product" {
            return Err(UnifiedError::bad_request(
                "kind must be \"discount\" or \"product\"",
            ));
        }
        let order_id = match &input.order_id {
            Some(raw) => Some(require_text(raw, "order_id", MAX_ORDER_ID_LEN)?),
            None => None,
        };
        let product_id = match &input.product_id {
            Some(raw) => Some(require_text(raw, "product_id", MAX_PRODUCT_ID_LEN)?),
            None => None,
        };
        if kind == "discount" && order_id.is_none() {
            return Err(UnifiedError::bad_request(
                "order_id is required for a discount redemption",
            ));
        }
        if kind == "product" && product_id.is_none() {
            return Err(UnifiedError::bad_request(
                "product_id is required for a product redemption",
            ));
        }
        let discount_amount = if kind == "discount" {
            round_money(input.points as f64 * program.discount_per_point)
        } else {
            0.0
        };
        let tier_before = tier_for(&state, program.id, &customer_id).map(|tier| tier.id);
        let now = Utc::now();
        let entry = append_ledger(
            &state,
            &program,
            &customer_id,
            LedgerKind::Spend,
            -input.points,
            &format!("redemption: {kind}"),
            &caller.actor,
            None,
            order_id.clone(),
            None,
            now,
        )?;
        // A discount redemption starts `pending`, not `completed`:
        // its points are spent now, but the discount is only
        // confirmed when the store applies it (see
        // `store.discount.applied` in `hooks`). The pending state
        // is visible in this response (`status`) and in the
        // `loyalty.redemption.pending` event below, so a caller
        // can never mistake the spend for an applied discount.
        // A product redemption completes at once.
        let status = initial_redemption_status(&kind);
        let redemption = LoyaltyRedemption {
            id: Uuid::new_v4(),
            program_id: program.id,
            customer_id: customer_id.clone(),
            points_spent: input.points,
            kind: kind.clone(),
            discount_amount,
            currency: program.currency.clone(),
            product_id: product_id.clone(),
            order_id: order_id.clone(),
            status: status.to_string(),
            created_at: now,
            updated_at: now,
        };
        state.redemptions.insert(redemption.id, redemption.clone());
        state.emit(
            "loyalty.points.spent",
            json!({
                "program_id": program.id,
                "customer_id": customer_id,
                "points": input.points,
                "balance_after": entry.balance_after,
                "redemption_id": redemption.id,
            }),
        );
        if kind == "discount" {
            state.emit(
                "loyalty.redemption.pending",
                json!({
                    "redemption_id": redemption.id,
                    "program_id": program.id,
                    "customer_id": customer_id,
                    "points_spent": input.points,
                    "kind": kind,
                    "discount_amount": discount_amount,
                    "currency": program.currency,
                    "product_id": product_id,
                    "order_id": order_id,
                    "status": "pending",
                    "confirmation": "awaiting store.discount.applied",
                }),
            );
            // Request only: the store module owns applying the
            // discount to the order, and no store listener for
            // this event exists yet (follow-up). The redemption
            // stays `pending` until the store confirms it.
            state.emit(
                "loyalty.discount.requested",
                json!({
                    "redemption_id": redemption.id,
                    "program_id": program.id,
                    "customer_id": customer_id,
                    "order_id": order_id,
                    "discount_amount": discount_amount,
                    "currency": program.currency,
                }),
            );
        } else {
            state.emit(
                "loyalty.redemption.completed",
                json!({
                    "redemption_id": redemption.id,
                    "program_id": program.id,
                    "customer_id": customer_id,
                    "points_spent": input.points,
                    "kind": kind,
                    "discount_amount": discount_amount,
                    "currency": program.currency,
                    "product_id": product_id,
                    "order_id": order_id,
                }),
            );
        }
        let tier_after = tier_for(&state, program.id, &customer_id).map(|tier| tier.id);
        if tier_before != tier_after {
            state.emit(
                "loyalty.tier.changed",
                json!({
                    "program_id": program.id,
                    "customer_id": customer_id,
                    "previous_tier_id": tier_before,
                    "tier_id": tier_after,
                }),
            );
        }
        Ok(redemption)
    })();
    match result {
        Ok(redemption) => created(redemption),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn cancel_redemption(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyRedemption> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let mut redemption = load_redemption(&state, &path)?;
        // Both a completed redemption and a pending discount
        // redemption (points spent, discount not yet confirmed by
        // the store) can be cancelled; cancelling refunds the
        // points. Only an already-cancelled one cannot.
        if redemption.status != "completed" && redemption.status != "pending" {
            return Err(UnifiedError::conflict(format!(
                "Redemption is {} and cannot be cancelled",
                redemption.status
            )));
        }
        let reason = body
            .reason
            .as_deref()
            .map(|raw| require_text(raw, "reason", MAX_REASON_LEN))
            .transpose()?
            .unwrap_or_else(|| "redemption cancelled".to_string());
        let program =
            require_program_in_scope(&state, &redemption.program_id.to_string(), &_scope)?;
        let now = Utc::now();
        // Refund through a new ledger row; the original spend row
        // is never rewritten.
        let entry = append_ledger(
            &state,
            &program,
            &redemption.customer_id,
            LedgerKind::Adjust,
            redemption.points_spent,
            &format!("refund: {reason}"),
            &caller.actor,
            None,
            Some(redemption.id.to_string()),
            None,
            now,
        )?;
        redemption.status = "cancelled".to_string();
        redemption.updated_at = now;
        state.redemptions.insert(redemption.id, redemption.clone());
        state.emit(
            "loyalty.redemption.cancelled",
            json!({
                "redemption_id": redemption.id,
                "program_id": program.id,
                "customer_id": redemption.customer_id,
                "points_refunded": redemption.points_spent,
                "balance_after": entry.balance_after,
                "reason": reason,
            }),
        );
        Ok(redemption)
    })();
    match result {
        Ok(redemption) => ok(redemption),
        Err(error) => err(error, debug),
    }
}

/// Reused by the cancel-redemption handler: an optional free-text
/// reason for a manual review action.
#[derive(Debug, Clone, Deserialize)]
struct ReviewRequest {
    #[serde(default)]
    pub reason: Option<String>,
}
