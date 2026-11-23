// forms — handlers submissions.

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
use super::handlers_fields::*;

// ---------------------------------------------------------------------------
// Submission handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_submissions(
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
        let status_filter = params.filters.get("status").cloned();
        let mapping_filter = params.filters.get("mapping_status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Value> = state
            .submissions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|submission| submission.form_id == form.id)
            .filter(|submission| {
                if let Some(status) = &status_filter {
                    if &submission.status != status {
                        return false;
                    }
                }
                if let Some(mapping) = &mapping_filter {
                    if &submission.mapping_status != mapping {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let view = submission_view(&state, submission);
                    if !view.to_string().to_lowercase().contains(&search) {
                        return false;
                    }
                }
                true
            })
            .map(|submission| submission_view(&state, &submission))
            .collect();
        items.sort_by(|a, b| {
            let a_created = a.get("created_at").and_then(Value::as_str).unwrap_or("");
            let b_created = b.get("created_at").and_then(Value::as_str).unwrap_or("");
            b_created.cmp(a_created)
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn export_submissions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ExportQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let format = query.into_inner().format.to_lowercase();
    if format != "csv" && format != "json" {
        return err(
            UnifiedError::bad_request("format must be \"csv\" or \"json\""),
            debug,
        );
    }
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_VIEW)?;
        let form = load_form(&state, &path.into_inner())?;
        require_record_in_company_scope(&form.company_id, &_scope)?;
        let fields = fields_for(&state, form.id);
        let mut submissions: Vec<FormSubmission> = state
            .submissions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|submission| submission.form_id == form.id)
            .collect();
        submissions.sort_by_key(|submission| submission.created_at);
        if format == "json" {
            let items: Vec<Value> = submissions
                .iter()
                .map(|submission| submission_view(&state, submission))
                .collect();
            return Ok(json!({ "form_id": form.id.to_string(), "submissions": items }));
        }
        let mut out = String::from("id,status,mapping_status,created_at");
        for field in &fields {
            out.push(',');
            out.push_str(&field.key);
        }
        out.push_str("\r\n");
        for submission in &submissions {
            let view = submission_view(&state, submission);
            let values = view
                .get("values")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut cells: Vec<String> = vec![
                submission.id.to_string(),
                submission.status.clone(),
                submission.mapping_status.clone(),
                submission.created_at.to_rfc3339(),
            ];
            for field in &fields {
                let cell = values
                    .iter()
                    .find(|value| {
                        value.get("field_key").and_then(Value::as_str) == Some(field.key.as_str())
                    })
                    .and_then(|value| value.get("value"))
                    .map(value_to_string)
                    .unwrap_or_default();
                cells.push(escape_csv_field(&cell));
            }
            out.push_str(&cells.join(","));
            out.push_str("\r\n");
        }
        Ok(json!({ "form_id": form.id.to_string(), "format": "csv", "csv": out }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) fn load_submission(state: &FormsState, raw_id: &str) -> UnifiedResult<FormSubmission> {
    let id = parse_uuid(raw_id, "submission")?;
    state
        .submissions
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Submission not found: {id}")))
}

pub(crate) async fn get_submission(
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
        let submission = load_submission(&state, &path.into_inner())?;
        let parent_form = load_form(&state, &submission.form_id.to_string())?;
        require_record_in_company_scope(&parent_form.company_id, &_scope)?;
        Ok(submission_view(&state, &submission))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) fn transition_submission_status(
    state: &FormsState,
    submission: &FormSubmission,
    to: &str,
) -> FormSubmission {
    let mut updated = submission.clone();
    updated.status = to.to_string();
    updated.updated_at = Utc::now();
    state.submissions.insert(updated.id, updated.clone());
    updated
}

pub(crate) async fn mark_submission_read(
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
        let submission = load_submission(&state, &path.into_inner())?;
        let parent_form = load_form(&state, &submission.form_id.to_string())?;
        require_record_in_company_scope(&parent_form.company_id, &_scope)?;
        if submission.status == "spam" {
            return Err(UnifiedError::conflict(
                "A spam submission cannot be marked read; it stays spam",
            ));
        }
        let updated = transition_submission_status(&state, &submission, "read");
        Ok(submission_view(&state, &updated))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn mark_submission_spam(
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
        let submission = load_submission(&state, &path.into_inner())?;
        let parent_form = load_form(&state, &submission.form_id.to_string())?;
        require_record_in_company_scope(&parent_form.company_id, &_scope)?;
        let updated = transition_submission_status(&state, &submission, "spam");
        Ok(submission_view(&state, &updated))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn remap_submission(
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
        let mut submission = load_submission(&state, &path.into_inner())?;
        let parent_form = load_form(&state, &submission.form_id.to_string())?;
        require_record_in_company_scope(&parent_form.company_id, &_scope)?;
        let form = state
            .forms
            .get(&submission.form_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Form not found: {}", submission.form_id)))?;
        if form.mapping.is_none() {
            return Err(UnifiedError::conflict(
                "This form has no mapping configured; add one before retrying",
            ));
        }
        if submission.mapping_status == "mapped" {
            return Err(UnifiedError::conflict(
                "This submission is already mapped",
            ));
        }
        let mut stored: Vec<(FormField, Value)> = Vec::new();
        for value in state.values.iter() {
            if value.value().submission_id != submission.id {
                continue;
            }
            if let Some(field) = state.fields.get(&value.value().field_id) {
                stored.push((field.value().clone(), value.value().value.clone()));
            }
        }
        stored.sort_by_key(|(field, _)| (field.position, field.created_at));
        let outcome = try_mapping(&state, &form, &submission, &stored);
        apply_mapping_outcome(&state, &form, &mut submission, outcome);
        Ok(submission_view(&state, &submission))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Public render + submit
// ---------------------------------------------------------------------------

pub(crate) async fn get_public_form(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    path: web::Path<String>,
) -> HttpResponse {
    // No capability check here on purpose: this is the public form
    // render for embedding. The API Manager gate (ApiAccessRule
    // with public_access for this path, default deny) is what makes
    // it public — see the module header.
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let form = load_form_by_slug(&state, &path.into_inner())?;
        if form.status != FormStatus::Published {
            return Err(UnifiedError::not_found(format!(
                "Form not found: {}",
                form.slug
            )));
        }
        Ok(form_view(&state, &form))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// True when the honeypot caught a bot: either the honeypot field
/// inside `values` or the top-level honeypot value is non-empty.
/// Checked before the rate limit so bots never consume quota.
pub(crate) fn is_honeypot_filled(
    values: &Map<String, Value>,
    honeypot_field: &str,
    honeypot: Option<&str>,
) -> bool {
    values
        .get(honeypot_field)
        .map(|value| !is_empty_value(value))
        .unwrap_or(false)
        || honeypot
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
}

pub(crate) async fn submit_public_form(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SubmitFormRequest>,
) -> HttpResponse {
    // No capability check here on purpose: this is the public
    // submit endpoint. It is protected by the API Manager gate
    // (default deny), the honeypot, and the rate limits instead —
    // see the module header.
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let slug = path.into_inner();
        let form = load_form_by_slug(&state, &slug)?;
        if form.status != FormStatus::Published {
            return Err(UnifiedError::not_found(format!(
                "Form not found: {}",
                form.slug
            )));
        }
        let input = body.into_inner();
        let ip = client_ip(&req).unwrap_or_else(|| "unknown".to_string());
        let user_agent = req
            .headers()
            .get("user-agent")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let verified = unified_core::caller::caller_from(&req);
        let submitted_by = if verified.is_authenticated() {
            Some(verified.user_id.clone())
        } else {
            None
        };
        // Honeypot first, before the rate limit: a bot that fills
        // the honeypot field must not consume the per-IP / per-form
        // quota, or bots could exhaust it for real submitters.
        let honeypot_filled = is_honeypot_filled(
            &input.values,
            &form.honeypot_field,
            input.honeypot.as_deref(),
        );
        let now = Utc::now();
        if honeypot_filled {
            let submission = FormSubmission {
                id: Uuid::now_v7(),
                form_id: form.id,
                status: "spam".to_string(),
                mapping_status: "none".to_string(),
                mapping_error: Some("honeypot field was filled".to_string()),
                mapped_target_id: None,
                ip_address: Some(ip),
                user_agent,
                submitted_by,
                created_at: now,
                updated_at: now,
            };
            state.submissions.insert(submission.id, submission);
            return Ok(json!({
                "received": true,
                "message": form.success_message,
            }));
        }
        // Only genuine (non-bot) submissions consume quota.
        check_rate_limit(&state, form.id, &ip)?;
        let fields = fields_for(&state, form.id);
        let stored = validate_submission_values(&fields, &input.values, &form.honeypot_field)?;
        let mut submission = FormSubmission {
            id: Uuid::now_v7(),
            form_id: form.id,
            status: "new".to_string(),
            mapping_status: if form.mapping.is_some() {
                "failed".to_string()
            } else {
                "none".to_string()
            },
            mapping_error: None,
            mapped_target_id: None,
            ip_address: Some(ip),
            user_agent,
            submitted_by,
            created_at: now,
            updated_at: now,
        };
        state.submissions.insert(submission.id, submission.clone());
        for (field, value) in &stored {
            let row = FormSubmissionValue {
                id: Uuid::now_v7(),
                submission_id: submission.id,
                field_id: field.id,
                field_key: field.key.clone(),
                value: value.clone(),
                file_storage_key: if field.field_type == "file" {
                    value.as_str().map(str::to_string)
                } else {
                    None
                },
                created_at: now,
            };
            state.values.insert(row.id, row);
        }
        state.emit(
            "forms.submission.received",
            json!({
                "form_id": form.id.to_string(),
                "submission_id": submission.id.to_string(),
                "slug": form.slug,
            }),
        );
        if form.mapping.is_some() {
            let outcome = try_mapping(&state, &form, &submission, &stored);
            apply_mapping_outcome(&state, &form, &mut submission, outcome);
        }
        Ok(json!({
            "received": true,
            "submission_id": submission.id.to_string(),
            "mapping_status": submission.mapping_status,
            "message": form.success_message,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn honeypot_detection_covers_both_honeypot_spots() {
        let mut values = Map::new();
        assert!(!is_honeypot_filled(&values, "website", None));
        assert!(!is_honeypot_filled(&values, "website", Some("  ")));
        values.insert("website".to_string(), json!(""));
        assert!(!is_honeypot_filled(&values, "website", None));
        values.insert("website".to_string(), json!("http://spam.example"));
        assert!(is_honeypot_filled(&values, "website", None));
        values.insert("website".to_string(), json!(""));
        assert!(is_honeypot_filled(&values, "website", Some("bot")));
        // Another field being filled is not the honeypot.
        let mut other = Map::new();
        other.insert("name".to_string(), json!("Ada"));
        assert!(!is_honeypot_filled(&other, "website", None));
    }
}
