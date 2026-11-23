// forms — handlers fields.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::store::*;
use super::common::*;
use super::validation::*;
use super::submission::*;
use super::module::*;
use super::handlers_forms::*;
use super::handlers_submissions::*;

// ---------------------------------------------------------------------------
// Field handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_fields(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_VIEW)?;
        let form = load_form(&state, &path.into_inner())?;
        require_record_in_company_scope(&form.company_id, &_scope)?;
        let params = query.into_inner();
        let items = fields_for(&state, form.id);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_field(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateFieldRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<FormField> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_MANAGE)?;
        let form = load_form(&state, &path.into_inner())?;
        require_record_in_company_scope(&form.company_id, &_scope)?;
        let input = body.into_inner();
        if fields_for(&state, form.id).len() >= MAX_FIELDS_PER_FORM {
            return Err(UnifiedError::bad_request(format!(
                "at most {MAX_FIELDS_PER_FORM} fields are allowed per form"
            )));
        }
        let known_keys: Vec<String> = fields_for(&state, form.id)
            .iter()
            .map(|field| field.key.clone())
            .collect();
        validate_field_definition(&input, &known_keys)?;
        if key_in_use(&state, form.id, input.key.trim(), None) {
            return Err(UnifiedError::bad_request(format!(
                "field key is already in use: {}",
                input.key.trim()
            )));
        }
        if input.key.trim() == form.honeypot_field {
            return Err(UnifiedError::bad_request(
                "field key must not collide with the honeypot field",
            ));
        }
        let now = Utc::now();
        let mut field = build_field(&state, form.id, &input, now);
        if input.position.is_some() {
            shift_fields_for_insert(&state, form.id, field.position);
        }
        state.fields.insert(field.id, field.clone());
        state.forms.insert(form.id, {
            let mut updated = form.clone();
            updated.updated_at = now;
            updated
        });
        Ok(field)
    })();
    match result {
        Ok(field) => created(field),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_field(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<FormField> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_VIEW)?;
        let (form_id, field_id) = path.into_inner();
        let form = load_form(&state, &form_id)?;
        require_record_in_company_scope(&form.company_id, &_scope)?;
        load_field(&state, form.id, &field_id)
    })();
    match result {
        Ok(field) => ok(field),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_field(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateFieldRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<FormField> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_MANAGE)?;
        let (form_id, field_id) = path.into_inner();
        let form = load_form(&state, &form_id)?;
        require_record_in_company_scope(&form.company_id, &_scope)?;
        let mut field = load_field(&state, form.id, &field_id)?;
        let input = body.into_inner();
        // Rebuild a create-shaped definition from the merged values
        // and validate it as a whole, so an update can never leave
        // a field in a state a fresh create would reject.
        let mut merged = CreateFieldRequest {
            key: field.key.clone(),
            label: field.label.clone(),
            field_type: field.field_type.clone(),
            position: Some(field.position),
            required: field.required,
            placeholder: field.placeholder.clone(),
            help_text: field.help_text.clone(),
            options: field.options.clone(),
            min_length: field.min_length,
            max_length: field.max_length,
            min_value: field.min_value,
            max_value: field.max_value,
            pattern: field.pattern.clone(),
            show_when: field.show_when.clone(),
        };
        if let Some(key) = &input.key {
            merged.key = key.trim().to_string();
        }
        if let Some(label) = &input.label {
            merged.label = label.clone();
        }
        if let Some(field_type) = &input.field_type {
            merged.field_type = field_type.clone();
        }
        if let Some(position) = input.position {
            merged.position = Some(position);
        }
        if let Some(required) = input.required {
            merged.required = required;
        }
        if let Some(placeholder) = &input.placeholder {
            merged.placeholder = placeholder.clone();
        }
        if let Some(help) = &input.help_text {
            merged.help_text = help.clone();
        }
        if let Some(options) = &input.options {
            merged.options = options.clone();
        }
        if let Some(min_length) = &input.min_length {
            merged.min_length = *min_length;
        }
        if let Some(max_length) = &input.max_length {
            merged.max_length = *max_length;
        }
        if let Some(min_value) = &input.min_value {
            merged.min_value = *min_value;
        }
        if let Some(max_value) = &input.max_value {
            merged.max_value = *max_value;
        }
        if let Some(pattern) = &input.pattern {
            merged.pattern = pattern.clone();
        }
        if let Some(show_when) = &input.show_when {
            merged.show_when = show_when.clone();
        }
        let known_keys: Vec<String> = fields_for(&state, form.id)
            .iter()
            .map(|entry| entry.key.clone())
            .collect();
        validate_field_definition(&merged, &known_keys)?;
        if merged.key != field.key && key_in_use(&state, form.id, &merged.key, Some(field.id)) {
            return Err(UnifiedError::bad_request(format!(
                "field key is already in use: {}",
                merged.key
            )));
        }
        if merged.key == form.honeypot_field {
            return Err(UnifiedError::bad_request(
                "field key must not collide with the honeypot field",
            ));
        }
        let now = Utc::now();
        field.key = merged.key;
        field.label = merged.label.trim().to_string();
        field.field_type = merged.field_type;
        field.required = merged.required;
        field.placeholder = merged.placeholder;
        field.help_text = merged.help_text;
        field.options = merged.options;
        field.min_length = merged.min_length;
        field.max_length = merged.max_length;
        field.min_value = merged.min_value;
        field.max_value = merged.max_value;
        field.pattern = merged.pattern;
        field.show_when = merged.show_when;
        if let Some(position) = merged.position {
            field.position = position;
        }
        field.updated_at = now;
        state.fields.insert(field.id, field.clone());
        state.forms.insert(form.id, {
            let mut updated = form.clone();
            updated.updated_at = now;
            updated
        });
        Ok(field)
    })();
    match result {
        Ok(field) => ok(field),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_field(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_MANAGE)?;
        let (form_id, field_id) = path.into_inner();
        let form = load_form(&state, &form_id)?;
        require_record_in_company_scope(&form.company_id, &_scope)?;
        let field = load_field(&state, form.id, &field_id)?;
        state.fields.remove(&field.id);
        let stale_values: Vec<Uuid> = state
            .values
            .iter()
            .filter(|entry| entry.value().field_id == field.id)
            .map(|entry| *entry.key())
            .collect();
        for key in stale_values {
            state.values.remove(&key);
        }
        Ok(json!({ "deleted": true, "field_id": field.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn reorder_fields(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReorderFieldsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<FormField>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_MANAGE)?;
        let form = load_form(&state, &path.into_inner())?;
        require_record_in_company_scope(&form.company_id, &_scope)?;
        let input = body.into_inner();
        let current = fields_for(&state, form.id);
        if input.field_ids.len() != current.len() {
            return Err(UnifiedError::bad_request(
                "field_ids must list every field of the form exactly once",
            ));
        }
        let mut seen: Vec<Uuid> = Vec::new();
        for id in &input.field_ids {
            if seen.contains(id) {
                return Err(UnifiedError::bad_request(format!(
                    "Duplicate field id in reorder list: {id}"
                )));
            }
            seen.push(*id);
            if !current.iter().any(|field| field.id == *id) {
                return Err(UnifiedError::bad_request(format!(
                    "Field id does not belong to this form: {id}"
                )));
            }
        }
        let now = Utc::now();
        let mut updated: Vec<FormField> = Vec::new();
        for (position, id) in input.field_ids.iter().enumerate() {
            if let Some(mut field) = state.fields.get_mut(id) {
                field.position = position as i64;
                field.updated_at = now;
                updated.push(field.value().clone());
            }
        }
        updated.sort_by_key(|field| field.position);
        Ok(updated)
    })();
    match result {
        Ok(fields) => ok(fields),
        Err(error) => err(error, debug),
    }
}
