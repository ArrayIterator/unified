// Studio - Record-type, system-field and definition handlers.

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
    CreateFieldDefinition, CreateRecordType, CreateSystemField, EVENT_FIELDS_CHANGED,
    FieldDefinition, MAX_HELP_LEN, MAX_LABEL_LEN, PERM_STUDIO_ADMIN, PERM_STUDIO_CUSTOMIZE,
    PERM_STUDIO_VIEW, RecordType, STATUS_ACTIVE, STATUS_ARCHIVED, STATUS_SUPERSEDED, StudioState,
    SystemField, UpdateFieldDefinition, field_type_infos, guard, matches_search,
    normalize_company_id, optional_text, paginate, require_text, validate_new_definition,
    validate_options, validate_rules, validate_value,
};

// ---------------------------------------------------------------------------
// Handlers: field types and record types.
// ---------------------------------------------------------------------------

pub(crate) async fn list_field_types(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    match guard(&ctx, &req, PERM_STUDIO_VIEW) {
        Ok(_) => json::success_response(field_type_infos(), 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_record_types(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_VIEW) {
        return json::error_response(&error, debug);
    }
    let params = query.into_inner();
    let module_filter = params.filters.get("module_id").cloned();
    let mut items: Vec<RecordType> = state
        .record_types
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| {
            matches_search(
                &format!(
                    "{} {} {}",
                    record.record_type, record.module_id, record.label
                ),
                &params,
            ) && module_filter
                .as_ref()
                .is_none_or(|module_id| record.module_id == *module_id)
        })
        .collect();
    items.sort_by(|a, b| a.record_type.cmp(&b.record_type));
    json::success_response(paginate(items, &params), 200)
}

pub(crate) async fn create_record_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    body: web::Json<CreateRecordType>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_ADMIN) {
        return json::error_response(&error, debug);
    }
    let result = state.register_record_type(body.into_inner());
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_record_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<RecordType> = state
        .record_types
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| {
            UnifiedError::not_found(format!(
                "Record type \"{}\" is not registered",
                path.as_str()
            ))
        });
    json::respond(result, debug)
}

pub(crate) async fn delete_record_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_ADMIN) {
        return json::error_response(&error, debug);
    }
    let record_type = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        if state
            .definitions
            .iter()
            .any(|entry| entry.value().record_type == record_type)
            || state
                .values
                .iter()
                .any(|entry| entry.value().record_type == record_type)
            || state
                .views
                .iter()
                .any(|entry| entry.value().record_type == record_type)
            || state
                .layouts
                .iter()
                .any(|entry| entry.value().record_type == record_type)
        {
            return Err(UnifiedError::conflict(format!(
                "Record type \"{record_type}\" still has definitions, values, views, or layouts; remove those first"
            )));
        }
        state
            .record_types
            .remove(&record_type)
            .map(|_| json!({ "deleted": true, "record_type": record_type }))
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Record type \"{record_type}\" is not registered"))
            })
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_system_fields(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_VIEW) {
        return json::error_response(&error, debug);
    }
    let record_type = path.into_inner();
    if !state.record_types.contains_key(&record_type) {
        return json::error_response(
            &UnifiedError::not_found(format!("Record type \"{record_type}\" is not registered")),
            debug,
        );
    }
    let params = query.into_inner();
    let items = state.system_fields_of(&record_type);
    let items: Vec<SystemField> = items
        .into_iter()
        .filter(|field| matches_search(&format!("{} {}", field.field_key, field.label), &params))
        .collect();
    json::success_response(paginate(items, &params), 200)
}

pub(crate) async fn create_system_field(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateSystemField>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_ADMIN) {
        return json::error_response(&error, debug);
    }
    let record_type = path.into_inner();
    if !state.record_types.contains_key(&record_type) {
        return json::error_response(
            &UnifiedError::not_found(format!("Record type \"{record_type}\" is not registered")),
            debug,
        );
    }
    let result = state.add_system_field(&record_type, body.into_inner());
    match result {
        Ok(field) => json::success_response(field, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_definitions(
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
    let company_filter = params.filters.get("company_id").cloned();
    let status_filter = params.filters.get("status").cloned();
    let type_filter = params.filters.get("field_type").cloned();
    let mut items: Vec<FieldDefinition> = state
        .definitions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|definition| {
            // Company scoping: a company caller only ever sees
            // their own company's definitions plus global ones,
            // whatever company_id the query filter asked for.
            caller_company.can_access_record(&definition.company_id)
                && matches_search(
                    &format!(
                        "{} {} {}",
                        definition.field_key, definition.label, definition.record_type
                    ),
                    &params,
                )
                && record_filter
                    .as_ref()
                    .is_none_or(|record_type| definition.record_type == *record_type)
                && company_filter
                    .as_ref()
                    .is_none_or(|company_id| definition.company_id == *company_id)
                && status_filter
                    .as_ref()
                    .is_none_or(|status| definition.status == *status)
                && type_filter
                    .as_ref()
                    .is_none_or(|field_type| definition.field_type == *field_type)
        })
        .collect();
    items.sort_by(|a, b| {
        a.record_type
            .cmp(&b.record_type)
            .then(a.field_key.cmp(&b.field_key))
            .then(b.version.cmp(&a.version))
    });
    json::success_response(paginate(items, &params), 200)
}

pub(crate) async fn create_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    body: web::Json<CreateFieldDefinition>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (call, caller_company) = match authorize(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        Ok(scoped) => scoped,
        Err(error) => return json::error_response(&error, debug),
    };
    let input = body.into_inner();
    let result: UnifiedResult<FieldDefinition> = (|| {
        // The body company_id is client-asserted: it must match
        // the caller's server-side company scope (a company caller
        // may not create definitions for another company, or
        // global ones).
        let company_id = normalize_company_id(&input.company_id);
        caller_company.require_company_scope(&company_id)?;
        let mut definition = validate_new_definition(&state, &input)?;
        definition.created_by = call.actor.clone();
        state
            .definitions
            .insert(definition.id.clone(), definition.clone());
        state.emit(
            EVENT_FIELDS_CHANGED,
            json!({
                "field_key": definition.field_key,
                "record_type": definition.record_type,
                "company_id": definition.company_id,
                "version": definition.version,
                "definition_id": definition.id,
                "action": "created",
            }),
        );
        Ok(definition)
    })();
    match result {
        Ok(definition) => json::success_response(definition, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_definition(
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
    let result: UnifiedResult<FieldDefinition> = state
        .definitions
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .filter(|definition| caller_company.can_access_record(&definition.company_id))
        .ok_or_else(|| UnifiedError::not_found("Field definition not found"));
    json::respond(result, debug)
}

/// Update creates a NEW version: the old row becomes `superseded`
/// and is never mutated, so values validated against it keep
/// working. The field type and field key are immutable — changing
/// a type is a new field, not a new version.
pub(crate) async fn update_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateFieldDefinition>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (call, caller_company) = match authorize(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        Ok(scoped) => scoped,
        Err(error) => return json::error_response(&error, debug),
    };
    let input = body.into_inner();
    let result: UnifiedResult<FieldDefinition> = (|| {
        let old = state
            .definitions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Field definition not found"))?;
        // A definition of another company does not exist for this
        // caller: checked before any version is created.
        if !caller_company.can_access_record(&old.company_id) {
            return Err(UnifiedError::not_found("Field definition not found"));
        }
        if old.status == STATUS_ARCHIVED {
            return Err(UnifiedError::conflict(
                "An archived field definition cannot be updated; create a new field instead",
            ));
        }
        let label = match &input.label {
            Some(label) => require_text(label, "label", MAX_LABEL_LEN)?,
            None => old.label.clone(),
        };
        let required = input.required.unwrap_or(old.required);
        let help_text = match &input.help_text {
            Some(help) => optional_text(&Some(help.clone()), "help_text", MAX_HELP_LEN)?,
            None => old.help_text.clone(),
        };
        let options = match &input.options {
            Some(options) => validate_options(&old.field_type, options)?,
            None => old.options.clone(),
        };
        let rules = match &input.validation {
            Some(rules) => {
                validate_rules(&old.field_type, rules)?;
                rules.clone()
            }
            None => old.validation.clone(),
        };
        let default_value = match &input.default_value {
            Some(default) => Some(default.clone()),
            None => old.default_value.clone(),
        };
        let mut next = old.clone();
        next.label = label;
        next.required = required;
        next.help_text = help_text;
        next.options = options;
        next.validation = rules;
        next.default_value = default_value.clone();
        if let Some(default) = &next.default_value {
            validate_value(&next, Some(default))?;
        }
        // Re-validate stored values only against the new rules when
        // they still reference this field key: values that no longer
        // validate stay attached to the old version and are reported
        // through the resolved endpoint instead of being deleted.
        next.id = Uuid::new_v4().to_string();
        next.version = old.version + 1;
        next.status = STATUS_ACTIVE.to_string();
        next.created_by = call.actor.clone();
        next.updated_at = Utc::now();
        if let Some(mut old_row) = state.definitions.get_mut(old.id.as_str()) {
            old_row.status = STATUS_SUPERSEDED.to_string();
            old_row.updated_at = Utc::now();
        }
        state.definitions.insert(next.id.clone(), next.clone());
        state.emit(
            EVENT_FIELDS_CHANGED,
            json!({
                "field_key": next.field_key,
                "record_type": next.record_type,
                "company_id": next.company_id,
                "version": next.version,
                "definition_id": next.id,
                "action": "updated",
            }),
        );
        Ok(next)
    })();
    match result {
        Ok(definition) => json::success_response(definition, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn delete_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    // Delete archives the row: definitions are never hard-deleted
    // while values may still reference them.
    archive_definition(ctx, state, req, path).await
}

pub(crate) async fn archive_definition(
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
    let result: UnifiedResult<FieldDefinition> = (|| {
        let mut definition = state
            .definitions
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Field definition not found"))?;
        // Scope is checked on the row itself before it is mutated,
        // so a definition of another company is never archived.
        if !caller_company.can_access_record(&definition.company_id) {
            return Err(UnifiedError::not_found("Field definition not found"));
        }
        definition.status = STATUS_ARCHIVED.to_string();
        definition.updated_at = Utc::now();
        let archived = (*definition).clone();
        drop(definition);
        state.emit(
            EVENT_FIELDS_CHANGED,
            json!({
                "field_key": archived.field_key,
                "record_type": archived.record_type,
                "company_id": archived.company_id,
                "version": archived.version,
                "definition_id": archived.id,
                "action": "archived",
            }),
        );
        Ok(archived)
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_effective_definitions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (_, caller_company) = match authorize(&ctx, &req, PERM_STUDIO_VIEW) {
        Ok(scoped) => scoped,
        Err(error) => return json::error_response(&error, debug),
    };
    let record_type = path.into_inner();
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
    let items = state.effective_definitions(&record_type, &company_id);
    let items: Vec<FieldDefinition> = items
        .into_iter()
        .filter(|definition| {
            matches_search(
                &format!("{} {}", definition.field_key, definition.label),
                &params,
            )
        })
        .collect();
    json::success_response(paginate(items, &params), 200)
}
