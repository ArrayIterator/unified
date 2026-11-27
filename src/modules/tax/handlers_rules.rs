// Tax module - tax rule and computation handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

// Handlers: rules

pub(crate) async fn list_rules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let auth: UnifiedResult<Uuid> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        acting_company(&ctx, &_tenant, &req, &caller)
    })();
    let company_id = match auth {
        Ok(company_id) => company_id,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let params = query.into_inner();
    let mut items: Vec<TaxRule> = state
        .rules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rule| {
            rule.company_id == company_id
                && matches_search(
                    &format!(
                        "{} {} {} {}",
                        rule.name, rule.country_code, rule.tax_type, rule.currency
                    ),
                    &params,
                )
                && params
                    .filters
                    .get("country_code")
                    .is_none_or(|country| rule.country_code.eq_ignore_ascii_case(country))
                && params.filters.get("region_code").is_none_or(|region| {
                    rule.region_code
                        .as_deref()
                        .is_none_or(|rule_region| rule_region.eq_ignore_ascii_case(region))
                })
                && params
                    .filters
                    .get("tax_type")
                    .is_none_or(|kind| rule.tax_type == *kind)
                && params
                    .filters
                    .get("enabled")
                    .is_none_or(|flag| rule.enabled == (flag == "true"))
        })
        .collect();
    items.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then(a.country_code.cmp(&b.country_code))
            .then(a.created_at.cmp(&b.created_at))
    });
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    body: web::Json<CreateTaxRule>,
) -> HttpResponse {
    let result: UnifiedResult<TaxRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let company_id = acting_company(&ctx, &_tenant, &req, &caller)?;
        let input = body.into_inner();
        if !valid_country_code(input.country_code.trim()) {
            return Err(UnifiedError::bad_request(
                "country_code must be a 2-letter ISO code",
            ));
        }
        let region_code = input
            .region_code
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(region) = &region_code {
            if !valid_region_code(region) {
                return Err(UnifiedError::bad_request(
                    "region_code must be at most 16 letters, digits, hyphens, or underscores",
                ));
            }
        }
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("name must not be empty"));
        }
        if !TAX_TYPES.contains(&input.tax_type.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "tax_type must be one of: {}",
                TAX_TYPES.join(", ")
            )));
        }
        if !(0.0..=100.0).contains(&input.rate_percent) || !input.rate_percent.is_finite() {
            return Err(UnifiedError::bad_request(
                "rate_percent must be a finite number between 0 and 100",
            ));
        }
        if let Some(threshold) = input.threshold_amount {
            if threshold < 0 {
                return Err(UnifiedError::bad_request(
                    "threshold_amount must be zero or greater",
                ));
            }
        }
        let currency = input
            .currency
            .unwrap_or_else(|| "USD".to_string())
            .trim()
            .to_uppercase();
        if !valid_currency(&currency) {
            return Err(UnifiedError::bad_request(
                "currency must be a 3-letter code",
            ));
        }
        let effective_from = parse_date(&input.effective_from, "effective_from")?;
        let effective_to = input
            .effective_to
            .map(|value| parse_date(&value, "effective_to"))
            .transpose()?;
        if let Some(until) = effective_to {
            if until < effective_from {
                return Err(UnifiedError::bad_request(
                    "effective_to must not be before effective_from",
                ));
            }
        }
        let rule = TaxRule {
            id: new_id(),
            company_id,
            country_code: input.country_code.trim().to_uppercase(),
            region_code,
            name: input.name.trim().to_string(),
            tax_type: input.tax_type,
            rate_percent: round_tax(input.rate_percent),
            threshold_amount: input.threshold_amount,
            currency,
            inclusive: input.inclusive.unwrap_or(false),
            compound: input.compound.unwrap_or(false),
            effective_from,
            effective_to,
            sort_order: input.sort_order.unwrap_or(0),
            enabled: input.enabled.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state.rules.insert(rule.id.clone(), rule.clone());
        state.record_audit(
            company_id,
            &caller.actor,
            "rule_created",
            "tax_rule",
            &rule.id,
            "tax.rule_created",
            json!({
                "country_code": rule.country_code,
                "tax_type": rule.tax_type,
                "rate_percent": rule.rate_percent,
            }),
        );
        Ok(rule)
    })();
    respond_created(result)
}

pub(crate) async fn get_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<TaxRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = acting_company(&ctx, &_tenant, &req, &caller)?;
        let rule = state
            .rules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tax rule not found"))?;
        if rule.company_id != company_id {
            return Err(UnifiedError::not_found("Tax rule not found"));
        }
        Ok(rule)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateTaxRule>,
) -> HttpResponse {
    let result: UnifiedResult<TaxRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let company_id = acting_company(&ctx, &_tenant, &req, &caller)?;
        let input = body.into_inner();
        if let Some(country) = &input.country_code {
            if !valid_country_code(country.trim()) {
                return Err(UnifiedError::bad_request(
                    "country_code must be a 2-letter ISO code",
                ));
            }
        }
        if let Some(region) = &input.region_code {
            if !region.trim().is_empty() && !valid_region_code(region.trim()) {
                return Err(UnifiedError::bad_request(
                    "region_code must be at most 16 letters, digits, hyphens, or underscores",
                ));
            }
        }
        if let Some(tax_type) = &input.tax_type {
            if !TAX_TYPES.contains(&tax_type.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "tax_type must be one of: {}",
                    TAX_TYPES.join(", ")
                )));
            }
        }
        if let Some(rate) = input.rate_percent {
            if !(0.0..=100.0).contains(&rate) || !rate.is_finite() {
                return Err(UnifiedError::bad_request(
                    "rate_percent must be a finite number between 0 and 100",
                ));
            }
        }
        if let Some(threshold) = input.threshold_amount {
            if threshold < 0 {
                return Err(UnifiedError::bad_request(
                    "threshold_amount must be zero or greater",
                ));
            }
        }
        if let Some(currency) = &input.currency {
            if !valid_currency(currency.trim().to_uppercase().as_str()) {
                return Err(UnifiedError::bad_request(
                    "currency must be a 3-letter code",
                ));
            }
        }
        let effective_from = input
            .effective_from
            .as_deref()
            .map(|value| parse_date(value, "effective_from"))
            .transpose()?;
        let effective_to = input
            .effective_to
            .as_deref()
            .map(|value| parse_date(value, "effective_to"))
            .transpose()?;
        // Validate the effective range against the stored rule before
        // any mutation is applied: a failed validation must leave the
        // stored rule untouched.
        let current = state
            .rules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tax rule not found"))?;
        if current.company_id != company_id {
            return Err(UnifiedError::not_found("Tax rule not found"));
        }
        let prospective_from = effective_from.unwrap_or(current.effective_from);
        let prospective_to = effective_to.or(current.effective_to);
        if let Some(until) = prospective_to {
            if until < prospective_from {
                return Err(UnifiedError::bad_request(
                    "effective_to must not be before effective_from",
                ));
            }
        }
        let mut rule = state
            .rules
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Tax rule not found"))?;
        if rule.company_id != company_id {
            return Err(UnifiedError::not_found("Tax rule not found"));
        }
        if let Some(country) = input.country_code {
            rule.country_code = country.trim().to_uppercase();
        }
        if let Some(region) = input.region_code {
            rule.region_code = if region.trim().is_empty() {
                None
            } else {
                Some(region.trim().to_string())
            };
        }
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("name must not be empty"));
            }
            rule.name = name.trim().to_string();
        }
        if let Some(tax_type) = input.tax_type {
            rule.tax_type = tax_type;
        }
        if let Some(rate) = input.rate_percent {
            rule.rate_percent = round_tax(rate);
        }
        if let Some(threshold) = input.threshold_amount {
            rule.threshold_amount = Some(threshold);
        }
        if let Some(currency) = input.currency {
            rule.currency = currency.trim().to_uppercase();
        }
        if let Some(inclusive) = input.inclusive {
            rule.inclusive = inclusive;
        }
        if let Some(compound) = input.compound {
            rule.compound = compound;
        }
        if let Some(from) = effective_from {
            rule.effective_from = from;
        }
        if let Some(until) = effective_to {
            rule.effective_to = Some(until);
        }
        if let Some(sort_order) = input.sort_order {
            rule.sort_order = sort_order;
        }
        if let Some(enabled) = input.enabled {
            rule.enabled = enabled;
        }
        rule.updated_at = now();
        let updated = rule.clone();
        drop(rule);
        state.record_audit(
            company_id,
            &caller.actor,
            "rule_updated",
            "tax_rule",
            &updated.id,
            "tax.rule_updated",
            json!({
                "country_code": updated.country_code,
                "tax_type": updated.tax_type,
                "rate_percent": updated.rate_percent,
            }),
        );
        Ok(updated)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let company_id = acting_company(&ctx, &_tenant, &req, &caller)?;
        let rule = state
            .rules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tax rule not found"))?;
        if rule.company_id != company_id {
            return Err(UnifiedError::not_found("Tax rule not found"));
        }
        state.rules.remove(path.as_str());
        state.record_audit(
            company_id,
            &caller.actor,
            "rule_deleted",
            "tax_rule",
            &rule.id,
            "tax.rule_deleted",
            json!({ "country_code": rule.country_code, "name": rule.name }),
        );
        Ok(json!({ "deleted": true, "id": rule.id }))
    })();
    json::respond(result, ctx.is_debug())
}

// Handlers: compute and computations

pub(crate) async fn compute_tax(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    body: web::Json<ComputeRequest>,
) -> HttpResponse {
    let result: UnifiedResult<TaxComputation> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = acting_company(&ctx, &_tenant, &req, &caller)?;
        let input = body.into_inner();
        // `amount` arrives as integer minor units (the request
        // JSON carries major units, converted by `serde_major`);
        // it only needs the non-negative check here.
        if input.amount < 0 {
            return Err(UnifiedError::bad_request(
                "amount must be a finite number zero or greater",
            ));
        }
        let currency = input
            .currency
            .unwrap_or_else(|| state.settings_for(company_id).default_currency)
            .trim()
            .to_uppercase();
        if !valid_currency(&currency) {
            return Err(UnifiedError::bad_request(
                "currency must be a 3-letter code",
            ));
        }
        let country_code = input
            .country_code
            .map(|value| value.trim().to_uppercase())
            .filter(|value| !value.is_empty());
        if let Some(country) = &country_code {
            if !valid_country_code(country) {
                return Err(UnifiedError::bad_request(
                    "country_code must be a 2-letter ISO code",
                ));
            }
        }
        let region_code = input
            .region_code
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(region) = &region_code {
            if !valid_region_code(region) {
                return Err(UnifiedError::bad_request(
                    "region_code must be at most 16 letters, digits, hyphens, or underscores",
                ));
            }
        }
        let on = input
            .date
            .as_deref()
            .map(|value| parse_date(value, "date"))
            .transpose()?
            .unwrap_or_else(today);
        let source_type = input.source_type.unwrap_or_else(|| "manual".to_string());
        if !matches!(source_type.as_str(), "manual" | "invoice" | "order") {
            return Err(UnifiedError::bad_request(
                "source_type must be one of: manual, invoice, order",
            ));
        }
        let source_id = input.source_id.unwrap_or_else(new_id);
        Ok(compute_and_store(
            &state,
            company_id,
            &source_type,
            &source_id,
            input.amount,
            &currency,
            country_code.as_deref(),
            region_code.as_deref(),
            on,
        ))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn list_computations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let auth: UnifiedResult<Uuid> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        acting_company(&ctx, &_tenant, &req, &caller)
    })();
    let company_id = match auth {
        Ok(company_id) => company_id,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let params = query.into_inner();
    let mut items: Vec<TaxComputation> = state
        .computations
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|computation| {
            computation.company_id == company_id
                && params
                    .filters
                    .get("source_type")
                    .is_none_or(|kind| computation.source_type == *kind)
                && params
                    .filters
                    .get("source_id")
                    .is_none_or(|id| computation.source_id == *id)
                && params.filters.get("country_code").is_none_or(|country| {
                    computation
                        .country_code
                        .as_deref()
                        .is_none_or(|code| code.eq_ignore_ascii_case(country))
                })
                && params
                    .filters
                    .get("currency")
                    .is_none_or(|currency| computation.currency.eq_ignore_ascii_case(currency))
        })
        .collect();
    items.sort_by(|a, b| b.computed_at.cmp(&a.computed_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn get_computation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<TaxComputation> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = acting_company(&ctx, &_tenant, &req, &caller)?;
        let computation = state
            .computations
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tax computation not found"))?;
        if computation.company_id != company_id {
            return Err(UnifiedError::not_found("Tax computation not found"));
        }
        Ok(computation)
    })();
    json::respond(result, ctx.is_debug())
}
