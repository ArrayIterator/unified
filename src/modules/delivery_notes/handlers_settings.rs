//! HTTP handlers for per-tenant delivery notes settings:
//! the DO number format (prefix, width, reset rule), print
//! defaults, and the delivery gates.

use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::Value;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};

use super::helpers::{
    caller, caller_company, err, ok, require_tenant, validate_output_kind,
    validate_paper_size,
    validate_template_id,
};
use super::state::DeliveryNoteState;
use super::types::{
    valid_prefix, DeliveryNoteSettings, UpdateSettingsRequest, MAX_PREFIX_LEN,
    PERM_DELIVERY_NOTES_MANAGE, PERM_DELIVERY_NOTES_VIEW, RESET_RULES,
};

/// Get the tenant's delivery notes settings (defaults when the
/// tenant has no row yet), plus `next_number_example`: what the
/// next DO number will look like under the current settings
/// (rendered with sequence 1, without consuming a number).
pub(crate) async fn get_settings(
    state: web::Data<DeliveryNoteState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = _tenant.tenant_id.to_string();
        let caller = caller(&req)?;
        super::helpers::require_permission(&ctx, &caller.role, PERM_DELIVERY_NOTES_VIEW)?;
        let settings = state.current_settings(&tenant_id);
        let mut value = serde_json::to_value(&settings).unwrap_or(Value::Null);
        if let Value::Object(map) = &mut value {
            map.insert(
                "next_number_example".to_string(),
                Value::from(example_number(&settings)),
            );
        }
        Ok(value)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update the tenant's delivery notes settings (manager). The
/// number format is validated, never trusted: an invalid prefix
/// or reset rule is a 400, and a changed format only affects
/// numbers issued after the change — issued numbers are never
/// rewritten.
pub(crate) async fn update_settings(
    state: web::Data<DeliveryNoteState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = _tenant.tenant_id.to_string();
        let caller = caller(&req)?;
        super::helpers::require_permission(&ctx, &caller.role, PERM_DELIVERY_NOTES_MANAGE)?;
        let mut settings = state.current_settings(&tenant_id);
        let input = body.into_inner();
        if let Some(prefix) = &input.number_prefix {
            let prefix = prefix.trim().to_uppercase();
            if !valid_prefix(&prefix) {
                return Err(UnifiedError::bad_request(format!(
                    "number_prefix must be 1-{MAX_PREFIX_LEN} uppercase letters, digits, and single hyphens only (no leading/trailing hyphen)"
                )));
            }
            settings.number_prefix = prefix;
        }
        if let Some(width) = input.number_width {
            if !(1..=8).contains(&width) {
                return Err(UnifiedError::bad_request(
                    "number_width must be between 1 and 8",
                ));
            }
            settings.number_width = width;
        }
        if let Some(rule) = &input.number_reset_rule {
            let rule = rule.trim().to_lowercase();
            if !RESET_RULES.contains(&rule.as_str()) {
                return Err(UnifiedError::bad_request(
                    "number_reset_rule must be yearly, monthly, or never",
                ));
            }
            settings.number_reset_rule = rule;
        }
        if let Some(template_id) = input.default_template_id {
            settings.default_template_id = match template_id {
                Some(id) => Some(validate_template_id(&id)?),
                None => None,
            };
        }
        if let Some(paper_size_code) = input.default_paper_size_code {
            settings.default_paper_size_code = match paper_size_code {
                Some(code) => Some(validate_paper_size(&code)?),
                None => None,
            };
        }
        if let Some(output_kind) = input.default_output_kind {
            settings.default_output_kind = match output_kind {
                Some(kind) => Some(validate_output_kind(&kind)?),
                None => None,
            };
        }
        if let Some(require_proof) = input.require_proof_for_delivery {
            settings.require_proof_for_delivery = require_proof;
        }
        if let Some(auto_advance) = input.auto_advance_from_shipment {
            settings.auto_advance_from_shipment = auto_advance;
        }
        state.save_settings(&tenant_id, settings.clone());
        Ok(serde_json::to_value(settings).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// The effective number format for the tenant, rendered as an
/// example with the next sequence value (without consuming it):
/// what the next DO number will look like under the current
/// settings.
pub(crate) fn example_number(settings: &DeliveryNoteSettings) -> String {
    use super::types::{format_number, number_period_key};
    let now = chrono::Utc::now();
    let period_key = number_period_key(&settings.number_reset_rule, now);
    format_number(
        &settings.number_prefix,
        &settings.number_reset_rule,
        settings.number_width,
        &period_key,
        1,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_number_uses_settings() {
        let settings = DeliveryNoteSettings::default();
        let example = example_number(&settings);
        assert!(example.starts_with("SJ-2026-"));
        assert!(example.ends_with("0001"));
        let monthly = DeliveryNoteSettings {
            number_reset_rule: "monthly".to_string(),
            ..DeliveryNoteSettings::default()
        };
        assert!(example_number(&monthly).contains("2026-"));
    }
}
