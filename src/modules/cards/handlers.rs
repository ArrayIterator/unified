// cards — HTTP handlers.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Request/response shapes.
// ---------------------------------------------------------------------------

/// Read view of one standard brand, with its effective enabled
/// state for the requesting tenant.
#[derive(Debug, Clone, Serialize)]
pub struct BrandView {
    pub id: String,
    pub name: String,
    pub lengths: Vec<u8>,
    pub cvv_length: u8,
    pub luhn_required: bool,
    pub active: bool,
    pub enabled: bool,
}

fn brand_view(rule: &BrandRule, settings: &CardsSettings) -> BrandView {
    let enabled = (rule.id != "gpn" || settings.gpn_enabled)
        && !settings
            .disabled_standard_brands
            .iter()
            .any(|id| id == rule.id);
    BrandView {
        id: rule.id.to_string(),
        name: rule.name.to_string(),
        lengths: rule.lengths.to_vec(),
        cvv_length: rule.cvv_length,
        luhn_required: rule.luhn_required,
        active: rule.active,
        enabled,
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ValidateRequest {
    pub number: String,
    #[serde(default)]
    pub expiry: Option<String>,
    #[serde(default)]
    pub cvv: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSettingsRequest {
    #[serde(default)]
    pub test_mode: Option<String>,
    #[serde(default)]
    pub max_expiry_years_ahead: Option<u32>,
    #[serde(default)]
    pub gpn_enabled: Option<bool>,
    #[serde(default)]
    pub disabled_standard_brands: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateCustomRuleRequest {
    pub name: String,
    pub brand_name: String,
    #[serde(default)]
    pub iin_prefixes: Vec<String>,
    #[serde(default)]
    pub iin_ranges: Vec<IinRangeInput>,
    pub allowed_lengths: Vec<u8>,
    #[serde(default)]
    pub cvv_length: Option<u8>,
    #[serde(default)]
    pub luhn_required: Option<bool>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub overrides_standard_brand: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateCustomRuleRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub brand_name: Option<String>,
    #[serde(default)]
    pub iin_prefixes: Option<Vec<String>>,
    #[serde(default)]
    pub iin_ranges: Option<Vec<IinRangeInput>>,
    #[serde(default)]
    pub allowed_lengths: Option<Vec<u8>>,
    #[serde(default)]
    pub cvv_length: Option<u8>,
    #[serde(default)]
    pub luhn_required: Option<bool>,
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Double option so an explicit JSON `null` can clear the
    /// override: outer `None` = field absent, leave unchanged;
    /// `Some(None)` = clear; `Some(Some(id))` = set to that
    /// standard brand.
    #[serde(default)]
    pub overrides_standard_brand: Option<Option<String>>,
}

/// Resolve an update of `overrides_standard_brand`: an absent
/// field keeps the existing value, an explicit null clears it,
/// and a supplied value replaces it.
pub(crate) fn resolve_overrides_standard_brand(
    input: Option<Option<String>>,
    existing: Option<String>,
) -> Option<String> {
    input.unwrap_or(existing)
}

/// One daily validation count row (no PANs).
#[derive(Debug, Clone, Serialize)]
pub struct DailyCountView {
    pub date: String,
    pub matched_rule: String,
    pub result: String,
    pub count: u64,
}

// ---------------------------------------------------------------------------
// Custom-rule input validation.
// ---------------------------------------------------------------------------

struct ValidatedRuleFields {
    pub(crate) name: String,
    pub(crate) brand_name: String,
    pub(crate) iin_prefixes: Vec<String>,
    pub(crate) iin_ranges: Vec<IinRangeInput>,
    pub(crate) allowed_lengths: Vec<u8>,
    pub(crate) cvv_length: u8,
    pub(crate) overrides_standard_brand: Option<String>,
}

fn validate_rule_fields(
    name: String,
    brand_name: String,
    mut iin_prefixes: Vec<String>,
    mut iin_ranges: Vec<IinRangeInput>,
    mut allowed_lengths: Vec<u8>,
    cvv_length: u8,
    overrides_standard_brand: Option<String>,
) -> UnifiedResult<ValidatedRuleFields> {
    let name = require_text(&name, "name", 100)?;
    let brand_name = require_text(&brand_name, "brand_name", 100)?;

    let mut prefixes: Vec<String> = Vec::new();
    for prefix in iin_prefixes.drain(..) {
        let prefix = prefix.trim().to_string();
        if prefix.is_empty()
            || prefix.len() > 8
            || !prefix.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(UnifiedError::bad_request(
                "iin_prefixes must be digit strings of 1-8 digits",
            ));
        }
        if !prefixes.contains(&prefix) {
            prefixes.push(prefix);
        }
    }
    prefixes.sort();

    let mut ranges: Vec<IinRangeInput> = Vec::new();
    for range in iin_ranges.drain(..) {
        let start = range.start.trim().to_string();
        let end = range.end.trim().to_string();
        if start.len() != end.len()
            || start.len() < 2
            || start.len() > 8
            || !start.bytes().all(|byte| byte.is_ascii_digit())
            || !end.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(UnifiedError::bad_request(
                "iin_ranges start and end must be digit strings of equal length (2-8 digits)",
            ));
        }
        let start_value = start.parse::<u64>().map_err(|_| {
            UnifiedError::bad_request("iin_ranges start is not a valid number")
        })?;
        let end_value = end
            .parse::<u64>()
            .map_err(|_| UnifiedError::bad_request("iin_ranges end is not a valid number"))?;
        if start_value > end_value {
            return Err(UnifiedError::bad_request(
                "iin_ranges start must not be greater than end",
            ));
        }
        if !ranges
            .iter()
            .any(|kept| kept.start == start && kept.end == end)
        {
            ranges.push(IinRangeInput { start, end });
        }
    }
    ranges.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));

    if prefixes.is_empty() && ranges.is_empty() {
        return Err(UnifiedError::bad_request(
            "a custom rule needs at least one iin_prefix or iin_range",
        ));
    }

    allowed_lengths.sort_unstable();
    allowed_lengths.dedup();
    if allowed_lengths.is_empty()
        || allowed_lengths
            .iter()
            .any(|length| *length < MIN_PAN_LENGTH as u8 || *length > MAX_PAN_LENGTH as u8)
    {
        return Err(UnifiedError::bad_request(
            "allowed_lengths must be non-empty with every length between 12 and 19",
        ));
    }

    if cvv_length != 3 && cvv_length != 4 {
        return Err(UnifiedError::bad_request(
            "cvv_length must be 3 or 4",
        ));
    }

    let overrides_standard_brand = match overrides_standard_brand {
        Some(value) => {
            let value = value.trim().to_string();
            if value.is_empty() {
                None
            } else if !known_standard_brand(&value) {
                return Err(UnifiedError::bad_request(format!(
                    "overrides_standard_brand \"{value}\" is not a known standard brand"
                )));
            } else {
                Some(value)
            }
        }
        None => None,
    };

    Ok(ValidatedRuleFields {
        name,
        brand_name,
        iin_prefixes: prefixes,
        iin_ranges: ranges,
        allowed_lengths,
        cvv_length,
        overrides_standard_brand,
    })
}

// ---------------------------------------------------------------------------
// HTTP handlers.
// ---------------------------------------------------------------------------

pub(crate) async fn validate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CardsState>>,
    req: HttpRequest,
    body: web::Json<ValidateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CardValidationResult> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CARDS_VALIDATE)?;
        let tenant = _tenant.tenant_id.to_string();
        let input = body.into_inner();
        if input.number.trim().is_empty() {
            return Err(UnifiedError::bad_request("number must not be empty"));
        }
        if input.number.len() > MAX_RAW_NUMBER_LENGTH {
            return Err(UnifiedError::bad_request(
                "number is too long (at most 64 characters)",
            ));
        }
        if let Some(expiry) = &input.expiry {
            if expiry.len() > 10 {
                return Err(UnifiedError::bad_request(
                    "expiry is too long (expected MM/YY or MM/YYYY)",
                ));
            }
        }
        if let Some(cvv) = &input.cvv {
            if cvv.len() > 16 {
                return Err(UnifiedError::bad_request("cvv is too long"));
            }
        }
        let outcome = validate_and_record(&state, &tenant, &ValidateInput {
            number: input.number,
            expiry: input.expiry,
            cvv: input.cvv,
        });
        ctx.hooks.emit(
            CARDS_VALIDATED,
            json!({
                "tenant_id": tenant,
                "masked_pan": outcome.masked_pan,
                "brand_id": outcome.brand_id,
                "brand_name": outcome.brand_name,
                "matched_rule": outcome.matched_rule,
                "valid": outcome.valid,
                "reason_codes": outcome.reason_codes,
                "is_test_number": outcome.is_test_number,
                "inactive_brand": outcome.inactive_brand,
            }),
        );
        Ok(outcome)
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_brands(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CardsState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<BrandView>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CARDS_READ)?;
        let tenant = _tenant.tenant_id.to_string();
        let settings = state.settings_for(&tenant);
        Ok(STANDARD_BRANDS
            .iter()
            .map(|rule| brand_view(rule, &settings))
            .collect())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_brand(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CardsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BrandView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CARDS_READ)?;
        let tenant = _tenant.tenant_id.to_string();
        let settings = state.settings_for(&tenant);
        let id = path.into_inner();
        let rule = STANDARD_BRANDS
            .iter()
            .find(|rule| rule.id == id)
            .ok_or_else(|| UnifiedError::not_found(format!("Brand \"{id}\" not found")))?;
        Ok(brand_view(rule, &settings))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CardsState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CardsSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CARDS_READ)?;
        let tenant = _tenant.tenant_id.to_string();
        Ok(state.settings_for(&tenant))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CardsState>>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CardsSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CARDS_MANAGE)?;
        let tenant = _tenant.tenant_id.to_string();
        let input = body.into_inner();
        let mut settings = state.settings_for(&tenant);
        if let Some(test_mode) = input.test_mode {
            settings.test_mode = TestMode::from_str(test_mode.trim()).ok_or_else(|| {
                UnifiedError::bad_request("test_mode must be \"live\" or \"sandbox\"")
            })?;
        }
        if let Some(max_years) = input.max_expiry_years_ahead {
            if max_years < 1 || max_years > MAX_EXPIRY_YEARS_AHEAD_LIMIT {
                return Err(UnifiedError::bad_request(
                    "max_expiry_years_ahead must be between 1 and 50",
                ));
            }
            settings.max_expiry_years_ahead = max_years;
        }
        if let Some(gpn_enabled) = input.gpn_enabled {
            settings.gpn_enabled = gpn_enabled;
        }
        if let Some(disabled) = input.disabled_standard_brands {
            let mut cleaned: Vec<String> = Vec::new();
            for id in disabled {
                let id = id.trim().to_string();
                if id.is_empty() {
                    continue;
                }
                if !known_standard_brand(&id) {
                    return Err(UnifiedError::bad_request(format!(
                        "disabled_standard_brands contains unknown brand \"{id}\""
                    )));
                }
                if !cleaned.contains(&id) {
                    cleaned.push(id);
                }
            }
            cleaned.sort();
            settings.disabled_standard_brands = cleaned;
        }
        state.settings.insert(tenant, settings.clone());
        Ok(settings)
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_custom_rules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CardsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<CardCustomRule>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CARDS_READ)?;
        let tenant = _tenant.tenant_id.to_string();
        let params = query.into_inner();
        let mut items: Vec<CardCustomRule> = state
            .rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.tenant_id == tenant)
            .filter(|rule| {
                matches_search(&format!("{} {}", rule.name, rule.brand_name), &params)
                    && params
                        .filters
                        .get("enabled")
                        .is_none_or(|wanted| {
                            rule.enabled == (*wanted == "true" || *wanted == "1")
                        })
                    && params
                        .filters
                        .get("overrides_standard_brand")
                        .is_none_or(|wanted| {
                            rule.overrides_standard_brand.as_deref() == Some(wanted.as_str())
                        })
            })
            .collect();
        let sort = params.allowed_sort(&["name", "brand_name", "created_at"], "created_at");
        items.sort_by(|a, b| match sort.as_str() {
            "name" => a.name.cmp(&b.name).then(a.id.to_string().cmp(&b.id.to_string())),
            "brand_name" => a
                .brand_name
                .cmp(&b.brand_name)
                .then(a.id.to_string().cmp(&b.id.to_string())),
            _ => a
                .created_at
                .cmp(&b.created_at)
                .then(a.id.to_string().cmp(&b.id.to_string())),
        });
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_custom_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CardsState>>,
    req: HttpRequest,
    body: web::Json<CreateCustomRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CardCustomRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CARDS_MANAGE)?;
        let tenant = _tenant.tenant_id.to_string();
        let input = body.into_inner();
        let fields = validate_rule_fields(
            input.name,
            input.brand_name,
            input.iin_prefixes,
            input.iin_ranges,
            input.allowed_lengths,
            input.cvv_length.unwrap_or(3),
            input.overrides_standard_brand,
        )?;
        let now = Utc::now();
        let rule = CardCustomRule {
            id: Uuid::now_v7(),
            tenant_id: tenant,
            name: fields.name,
            brand_name: fields.brand_name,
            iin_prefixes: fields.iin_prefixes,
            iin_ranges: fields.iin_ranges,
            allowed_lengths: fields.allowed_lengths,
            cvv_length: fields.cvv_length,
            luhn_required: input.luhn_required.unwrap_or(true),
            enabled: input.enabled.unwrap_or(true),
            overrides_standard_brand: fields.overrides_standard_brand,
            created_at: now,
            updated_at: now,
        };
        state.rules.insert(rule.id, rule.clone());
        ctx.hooks.emit(
            CARDS_CUSTOM_RULE_CHANGED,
            json!({
                "tenant_id": rule.tenant_id.clone(),
                "id": rule.id.to_string(),
                "action": "created",
                "actor_id": caller.actor.clone(),
                "name": rule.name.clone(),
                "brand_name": rule.brand_name.clone(),
            }),
        );
        Ok(rule)
    })();
    match result {
        Ok(rule) => json::success_response(rule, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_custom_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CardsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CardCustomRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CARDS_READ)?;
        let tenant = _tenant.tenant_id.to_string();
        let id = Uuid::parse_str(path.into_inner().trim())
            .map_err(|_| UnifiedError::bad_request("Invalid custom rule id: expected a UUID"))?;
        let rule = state
            .rules
            .get(&id)
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.tenant_id == tenant)
            .ok_or_else(|| UnifiedError::not_found("Custom rule not found"))?;
        Ok(rule)
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_custom_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CardsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCustomRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CardCustomRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CARDS_MANAGE)?;
        let tenant = _tenant.tenant_id.to_string();
        let id = Uuid::parse_str(path.into_inner().trim())
            .map_err(|_| UnifiedError::bad_request("Invalid custom rule id: expected a UUID"))?;
        let existing = state
            .rules
            .get(&id)
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.tenant_id == tenant)
            .ok_or_else(|| UnifiedError::not_found("Custom rule not found"))?;
        let input = body.into_inner();
        let fields = validate_rule_fields(
            input.name.unwrap_or(existing.name.clone()),
            input.brand_name.unwrap_or(existing.brand_name.clone()),
            input.iin_prefixes.unwrap_or(existing.iin_prefixes.clone()),
            input.iin_ranges.unwrap_or(existing.iin_ranges.clone()),
            input.allowed_lengths.unwrap_or(existing.allowed_lengths.clone()),
            input.cvv_length.unwrap_or(existing.cvv_length),
            resolve_overrides_standard_brand(
                input.overrides_standard_brand,
                existing.overrides_standard_brand.clone(),
            ),
        )?;
        let updated = CardCustomRule {
            id: existing.id,
            tenant_id: existing.tenant_id,
            name: fields.name,
            brand_name: fields.brand_name,
            iin_prefixes: fields.iin_prefixes,
            iin_ranges: fields.iin_ranges,
            allowed_lengths: fields.allowed_lengths,
            cvv_length: fields.cvv_length,
            luhn_required: input.luhn_required.unwrap_or(existing.luhn_required),
            enabled: input.enabled.unwrap_or(existing.enabled),
            overrides_standard_brand: fields.overrides_standard_brand,
            created_at: existing.created_at,
            updated_at: Utc::now(),
        };
        state.rules.insert(updated.id, updated.clone());
        ctx.hooks.emit(
            CARDS_CUSTOM_RULE_CHANGED,
            json!({
                "tenant_id": updated.tenant_id.clone(),
                "id": updated.id.to_string(),
                "action": "updated",
                "actor_id": caller.actor.clone(),
                "name": updated.name.clone(),
                "brand_name": updated.brand_name.clone(),
            }),
        );
        Ok(updated)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_custom_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CardsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CARDS_MANAGE)?;
        let tenant = _tenant.tenant_id.to_string();
        let id = Uuid::parse_str(path.into_inner().trim())
            .map_err(|_| UnifiedError::bad_request("Invalid custom rule id: expected a UUID"))?;
        let removed = state
            .rules
            .get(&id)
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.tenant_id == tenant)
            .ok_or_else(|| UnifiedError::not_found("Custom rule not found"))?;
        state.rules.remove(&id);
        ctx.hooks.emit(
            CARDS_CUSTOM_RULE_CHANGED,
            json!({
                "tenant_id": removed.tenant_id,
                "id": removed.id.to_string(),
                "action": "deleted",
                "actor_id": caller.actor.clone(),
                "name": removed.name,
                "brand_name": removed.brand_name,
            }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_counts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CardsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<DailyCountView>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CARDS_READ)?;
        let tenant = _tenant.tenant_id.to_string();
        let params = query.into_inner();
        let mut items: Vec<DailyCountView> = state
            .counts
            .iter()
            .filter(|entry| entry.key().0 == tenant)
            .map(|entry| DailyCountView {
                date: entry.key().1.clone(),
                matched_rule: entry.key().2.clone(),
                result: entry.key().3.clone(),
                count: *entry.value(),
            })
            .filter(|item| {
                params
                    .filters
                    .get("date")
                    .is_none_or(|wanted| &item.date == wanted)
                    && params
                        .filters
                        .get("matched_rule")
                        .is_none_or(|wanted| &item.matched_rule == wanted)
                    && params
                        .filters
                        .get("result")
                        .is_none_or(|wanted| &item.result == wanted)
            })
            .collect();
        let sort = params.allowed_sort(&["date", "matched_rule", "result", "count"], "date");
        items.sort_by(|a, b| match sort.as_str() {
            "matched_rule" => a.matched_rule.cmp(&b.matched_rule),
            "result" => a.result.cmp(&b.result),
            "count" => a.count.cmp(&b.count),
            _ => a.date.cmp(&b.date),
        });
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}
