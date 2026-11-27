// Studio - Field-value handlers.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::auth::authorize;
use super::{
    EVENT_VALUES_CHANGED, FieldValue, MAX_RECORD_ID_LEN, PERM_STUDIO_CUSTOMIZE, PERM_STUDIO_VIEW,
    ResolvedCustomField, SetFieldValue, StudioState, ValidateFieldValue, matches_search,
    normalize_company_id, paginate, require_record_type, require_text, validate_field_key,
    validate_value,
};

// Handlers: values.

pub(crate) async fn list_values(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (_, caller_company) = match authorize(&ctx, &req, PERM_STUDIO_VIEW) {
        Ok(scoped) => scoped,
        Err(error) => return json::error_response(&error, debug),
    };
    let params = query.into_inner();
    let record_filter = params.filters.get("record_type").cloned();
    let record_id_filter = params.filters.get("record_id").cloned();
    let company_filter = params.filters.get("company_id").cloned();
    let key_filter = params.filters.get("field_key").cloned();
    let mut items: Vec<FieldValue> = state
        .values
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|value| {
            // Company scoping: a company caller only ever sees
            // their own company's values plus global ones,
            // whatever company_id the query filter asked for.
            caller_company.can_access_record(&value.company_id)
                && matches_search(
                    &format!(
                        "{} {} {}",
                        value.record_type, value.record_id, value.field_key
                    ),
                    &params,
                )
                && record_filter
                    .as_ref()
                    .is_none_or(|record_type| value.record_type == *record_type)
                && record_id_filter
                    .as_ref()
                    .is_none_or(|record_id| value.record_id == *record_id)
                && company_filter
                    .as_ref()
                    .is_none_or(|company_id| value.company_id == *company_id)
                && key_filter
                    .as_ref()
                    .is_none_or(|field_key| value.field_key == *field_key)
        })
        .collect();
    items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    json::success_response(paginate(items, &params), 200)
}

pub(crate) async fn set_value(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    body: web::Json<SetFieldValue>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (call, caller_company) = match authorize(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        Ok(scoped) => scoped,
        Err(error) => return json::error_response(&error, debug),
    };
    let input = body.into_inner();
    let result: UnifiedResult<FieldValue> = (|| {
        let record_type = require_record_type(&state, &input.record_type)?;
        let company_id = normalize_company_id(&input.company_id);
        // The body company_id is client-asserted: it must match
        // the caller's server-side company scope.
        caller_company.require_company_scope(&company_id)?;
        let record_id = require_text(&input.record_id, "record_id", MAX_RECORD_ID_LEN)?;
        let field_key = validate_field_key(&input.field_key)?;
        let effective = state.effective_definitions(&record_type, &company_id);
        let definition = effective
            .iter()
            .find(|definition| definition.field_key == field_key)
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "No active field definition \"{field_key}\" for record type \"{record_type}\""
                ))
            })?;
        validate_value(definition, Some(&input.value))?;
        let existing_id = state
            .values
            .iter()
            .find(|entry| {
                let value = entry.value();
                value.record_type == record_type
                    && value.record_id == record_id
                    && value.field_key == field_key
                    && value.company_id == company_id
            })
            .map(|entry| entry.key().clone());
        let now = Utc::now();
        let stored = match existing_id {
            Some(id) => {
                // The row was found by the scan above, but it may
                // have been deleted concurrently since: a missing
                // row is a 404 to the caller, never a panic of the
                // handler task.
                let mut row = state.values.get_mut(&id).ok_or_else(|| {
                    UnifiedError::not_found(
                        "Field value no longer exists; it may have been deleted",
                    )
                })?;
                row.value = input.value.clone();
                row.value_type = definition.field_type.clone();
                row.definition_id = Some(definition.id.clone());
                row.updated_at = now;
                (*row).clone()
            }
            None => {
                let row = FieldValue {
                    id: Uuid::new_v4().to_string(),
                    company_id: company_id.clone(),
                    record_type: record_type.clone(),
                    record_id: record_id.clone(),
                    field_key: field_key.clone(),
                    definition_id: Some(definition.id.clone()),
                    value_type: definition.field_type.clone(),
                    value: input.value.clone(),
                    created_by: call.actor.clone(),
                    created_at: now,
                    updated_at: now,
                };
                state.values.insert(row.id.clone(), row.clone());
                row
            }
        };
        state.emit(
            EVENT_VALUES_CHANGED,
            json!({
                "record_type": record_type,
                "record_id": record_id,
                "field_key": field_key,
                "company_id": company_id,
                "action": "set",
            }),
        );
        Ok(stored)
    })();
    match result {
        Ok(value) => json::success_response(value, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_value(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (_, caller_company) = match authorize(&ctx, &req, PERM_STUDIO_VIEW) {
        Ok(scoped) => scoped,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<FieldValue> = state
        .values
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .filter(|value| caller_company.can_access_record(&value.company_id))
        .ok_or_else(|| UnifiedError::not_found("Field value not found"));
    json::respond(result, debug)
}

pub(crate) async fn delete_value(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (_, caller_company) = match authorize(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        Ok(scoped) => scoped,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<Value> = (|| {
        // Load first: a value of another company does not exist
        // for this caller and is never deleted.
        let existing = state
            .values
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Field value not found"))?;
        if !caller_company.can_access_record(&existing.company_id) {
            return Err(UnifiedError::not_found("Field value not found"));
        }
        let removed = state
            .values
            .remove(path.as_str())
            .map(|(_, value)| value)
            .ok_or_else(|| UnifiedError::not_found("Field value not found"))?;
        state.emit(
            EVENT_VALUES_CHANGED,
            json!({
                "record_type": removed.record_type,
                "record_id": removed.record_id,
                "field_key": removed.field_key,
                "company_id": removed.company_id,
                "action": "deleted",
            }),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn validate_value_handler(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    body: web::Json<ValidateFieldValue>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (_, caller_company) = match authorize(&ctx, &req, PERM_STUDIO_VIEW) {
        Ok(scoped) => scoped,
        Err(error) => return json::error_response(&error, debug),
    };
    let input = body.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let record_type = require_record_type(&state, &input.record_type)?;
        let company_id = normalize_company_id(&input.company_id);
        // Validating against another company's definition would
        // leak whether that definition (and its rules) exists.
        caller_company.require_company_scope(&company_id)?;
        let field_key = validate_field_key(&input.field_key)?;
        let effective = state.effective_definitions(&record_type, &company_id);
        let definition = effective
            .iter()
            .find(|definition| definition.field_key == field_key)
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "No active field definition \"{field_key}\" for record type \"{record_type}\""
                ))
            })?;
        validate_value(definition, Some(&input.value))?;
        Ok(json!({
            "valid": true,
            "field_key": field_key,
            "record_type": record_type,
            "field_type": definition.field_type,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn resolved_custom_fields(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (_, caller_company) = match authorize(&ctx, &req, PERM_STUDIO_VIEW) {
        Ok(scoped) => scoped,
        Err(error) => return json::error_response(&error, debug),
    };
    let (record_type, record_id) = path.into_inner();
    if !state.record_types.contains_key(&record_type) {
        return json::error_response(
            &UnifiedError::not_found(format!("Record type \"{record_type}\" is not registered")),
            debug,
        );
    }
    let params = query.into_inner();
    let requested_company = params
        .filters
        .get("company_id")
        .cloned()
        .unwrap_or_default();
    // An explicitly requested other company fails closed; an
    // empty request defaults to the caller's own company.
    let company_id = match caller_company.scoped_read_company(&requested_company) {
        Ok(company_id) => company_id,
        Err(error) => return json::error_response(&error, debug),
    };
    let definitions = state.effective_definitions(&record_type, &company_id);
    let mut items: Vec<ResolvedCustomField> = definitions
        .into_iter()
        .map(|definition| {
            let stored = state.values.iter().find(|entry| {
                let value = entry.value();
                value.record_type == record_type
                    && value.record_id == record_id
                    && value.field_key == definition.field_key
                    && value.company_id == company_id
            });
            match stored {
                Some(entry) => ResolvedCustomField {
                    definition,
                    value: Some(entry.value().value.clone()),
                    value_id: Some(entry.value().id.clone()),
                },
                None => ResolvedCustomField {
                    definition,
                    value: None,
                    value_id: None,
                },
            }
        })
        .filter(|resolved| {
            matches_search(
                &format!(
                    "{} {}",
                    resolved.definition.field_key, resolved.definition.label
                ),
                &params,
            )
        })
        .collect();
    items.sort_by(|a, b| a.definition.field_key.cmp(&b.definition.field_key));
    json::success_response(paginate(items, &params), 200)
}
