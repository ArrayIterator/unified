// forms - handlers forms.

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
use super::handlers_fields::*;
use super::handlers_submissions::*;

// Form handlers

pub(crate) fn form_view(state: &FormsState, form: &Form) -> Value {
    let fields = fields_for(state, form.id);
    let mut view = serde_json::to_value(form).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut view {
        map.insert(
            "fields".to_string(),
            serde_json::to_value(fields).unwrap_or(Value::Null),
        );
    }
    view
}

pub(crate) async fn list_forms(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Form> = state
            .forms
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|form| {
                if !in_company_scope(&form.company_id, &_scope) {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if form.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(company) = &company_filter {
                    if form.company_id.as_deref() != Some(company.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {}",
                        form.title,
                        form.description.clone().unwrap_or_default(),
                        form.slug
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["title", "slug", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a.title.cmp(&b.title),
                "slug" => a.slug.cmp(&b.slug),
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

pub(crate) async fn create_form(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    body: web::Json<CreateFormRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_MANAGE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let slug = match &input.slug {
            Some(raw) => {
                let slug = raw.trim().to_lowercase();
                if !valid_slug(&slug) {
                    return Err(UnifiedError::bad_request(
                        "slug must be lowercase letters, digits, and single hyphens only (no leading/trailing hyphen, max 128 characters)",
                    ));
                }
                slug
            }
            None => slugify(&title),
        };
        if slug_in_use(&state, &slug, None) {
            return Err(UnifiedError::conflict(format!("slug is already in use: {slug}")));
        }
        if input.fields.len() > MAX_FIELDS_PER_FORM {
            return Err(UnifiedError::bad_request(format!(
                "at most {MAX_FIELDS_PER_FORM} fields are allowed per form"
            )));
        }
        let company_id = match &require_company_match(&_tenant, &req, &input.company_id)? {
            Some(raw) => Some(require_text(raw, "company_id", 128)?),
            None => None,
        };
        let submit_button_label = match &input.submit_button_label {
            Some(raw) => require_text(raw, "submit_button_label", MAX_BUTTON_LABEL_LEN)?,
            None => default_submit_label(),
        };
        let success_message = match &input.success_message {
            Some(raw) => require_text(raw, "success_message", MAX_SUCCESS_MESSAGE_LEN)?,
            None => default_success_message(),
        };
        let honeypot_field = match &input.honeypot_field {
            Some(raw) => {
                let field = raw.trim().to_string();
                if !valid_key(&field) {
                    return Err(UnifiedError::bad_request(
                        "honeypot_field must be a field key ([a-z0-9_]+, max 64 characters)",
                    ));
                }
                field
            }
            None => default_honeypot_field(),
        };
        let mapping = match &input.mapping {
            Some(raw) => Some(validate_mapping(raw)?),
            None => None,
        };
        // Validate every field definition before storing anything:
        // a condition may reference any key of the same batch.
        let batch_keys: Vec<String> = input
            .fields
            .iter()
            .map(|field| field.key.trim().to_string())
            .collect();
        let mut seen_keys: Vec<String> = Vec::new();
        for field in &input.fields {
            if seen_keys.contains(&field.key) {
                return Err(UnifiedError::bad_request(format!(
                    "Duplicate field key \"{}\"",
                    field.key
                )));
            }
            seen_keys.push(field.key.clone());
            validate_field_definition(field, &batch_keys)?;
        }
        if batch_keys.contains(&honeypot_field) {
            return Err(UnifiedError::bad_request(
                "honeypot_field must not collide with a real field key",
            ));
        }
        let now = Utc::now();
        let form = Form {
            id: Uuid::now_v7(),
            title,
            description,
            slug: slug.clone(),
            status: input.status.unwrap_or_default(),
            company_id,
            owner_id: caller.actor.clone(),
            submit_button_label,
            success_message,
            honeypot_field,
            mapping,
            created_at: now,
            updated_at: now,
        };
        state.forms.insert(form.id, form.clone());
        for (index, field_input) in input.fields.iter().enumerate() {
            let mut field = build_field(&state, form.id, field_input, now);
            if field_input.position.is_none() {
                field.position = index as i64;
            }
            state.fields.insert(field.id, field);
        }
        state.emit(
            "forms.created",
            json!({
                "form_id": form.id.to_string(),
                "slug": form.slug,
                "title": form.title,
                "status": form.status.as_str(),
            }),
        );
        Ok(form_view(&state, &form))
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_form(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_VIEW)?;
        let form = load_form(&state, &path.into_inner())?;
        require_record_in_company_scope(&form.company_id, &_scope)?;
        Ok(form_view(&state, &form))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_form(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateFormRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_MANAGE)?;
        let mut form = load_form(&state, &path.into_inner())?;
        require_record_writable_in_company_scope(&form.company_id, &_scope)?;
        let input = body.into_inner();
        if let Some(title) = &input.title {
            form.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &input.description {
            form.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(slug) = &input.slug {
            let slug = slug.trim().to_lowercase();
            if !valid_slug(&slug) {
                return Err(UnifiedError::bad_request(
                    "slug must be lowercase letters, digits, and single hyphens only",
                ));
            }
            if slug_in_use(&state, &slug, Some(form.id)) {
                return Err(UnifiedError::conflict(format!("slug is already in use: {slug}")));
            }
            form.slug = slug;
        }
        if let Some(status) = input.status {
            if !FormStatus::transition_allowed(form.status, status) {
                return Err(UnifiedError::unprocessable(format!(
                    "Form status cannot move from \"{}\" to \"{}\" (allowed: draft -> published, draft -> archived, published -> archived, archived -> draft)",
                    form.status.as_str(),
                    status.as_str()
                )));
            }
            form.status = status;
        }
        if let Some(company_id) = &input.company_id {
            form.company_id = match company_id {
                Some(raw) => {
                    require_company_match_for(&_scope, &Some(raw.clone()))?;
                    Some(require_text(raw, "company_id", 128)?)
                }
                None => None,
            };
        }
        if let Some(label) = &input.submit_button_label {
            form.submit_button_label =
                require_text(label, "submit_button_label", MAX_BUTTON_LABEL_LEN)?;
        }
        if let Some(message) = &input.success_message {
            form.success_message =
                require_text(message, "success_message", MAX_SUCCESS_MESSAGE_LEN)?;
        }
        if let Some(honeypot) = &input.honeypot_field {
            let field = honeypot.trim().to_string();
            if !valid_key(&field) {
                return Err(UnifiedError::bad_request(
                    "honeypot_field must be a field key ([a-z0-9_]+, max 64 characters)",
                ));
            }
            if fields_for(&state, form.id)
                .iter()
                .any(|entry| entry.key == field)
            {
                return Err(UnifiedError::bad_request(
                    "honeypot_field must not collide with a real field key",
                ));
            }
            form.honeypot_field = field;
        }
        if let Some(mapping) = &input.mapping {
            form.mapping = match mapping {
                Some(raw) => Some(validate_mapping(raw)?),
                None => None,
            };
        }
        form.updated_at = Utc::now();
        state.forms.insert(form.id, form.clone());
        state.emit(
            "forms.updated",
            json!({
                "form_id": form.id.to_string(),
                "slug": form.slug,
                "status": form.status.as_str(),
            }),
        );
        Ok(form_view(&state, &form))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_form(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_MANAGE)?;
        let form = load_form(&state, &path.into_inner())?;
        require_record_writable_in_company_scope(&form.company_id, &_scope)?;
        let id = form.id;
        state.forms.remove(&id);
        let stale_fields: Vec<Uuid> = state
            .fields
            .iter()
            .filter(|entry| entry.value().form_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in stale_fields {
            state.fields.remove(&key);
        }
        let stale_submissions: Vec<Uuid> = state
            .submissions
            .iter()
            .filter(|entry| entry.value().form_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in &stale_submissions {
            state.submissions.remove(key);
        }
        let stale_values: Vec<Uuid> = state
            .values
            .iter()
            .filter(|entry| stale_submissions.contains(&entry.value().submission_id))
            .map(|entry| *entry.key())
            .collect();
        for key in stale_values {
            state.values.remove(&key);
        }
        state.emit(
            "forms.deleted",
            json!({ "form_id": id.to_string(), "slug": form.slug }),
        );
        Ok(json!({ "deleted": true, "form_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_embed_code(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_VIEW)?;
        let form = load_form(&state, &path.into_inner())?;
        require_record_in_company_scope(&form.company_id, &_scope)?;
        let render_url = format!("/api/v1/forms/public/{}", form.slug);
        let submit_url = format!("/api/v1/forms/public/{}/submit", form.slug);
        Ok(json!({
            "form_id": form.id.to_string(),
            "slug": form.slug,
            "render_url": render_url,
            "submit_url": submit_url,
            "script_tag": format!(
                "<script src=\"{render_url}\" data-form-submit=\"{submit_url}\"></script>",
                render_url = render_url,
                submit_url = submit_url,
            ),
            "iframe_tag": format!(
                "<iframe src=\"{render_url}\" title=\"{title}\" width=\"100%\" height=\"600\" frameborder=\"0\"></iframe>",
                render_url = render_url,
                title = form.title,
            ),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
