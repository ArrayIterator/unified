// Marketing Automation - Sequence and enrollment handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::service_sequences::*;
use super::handlers_campaigns::*;
use super::handlers_segments::*;
use super::handlers_scores::*;
use super::handlers_audience::*;
use super::hooks::*;
use super::module::*;

// Sequence handlers.

#[derive(Debug, Deserialize)]
pub(crate) struct SequenceInput {
    name: String,
    description: Option<String>,
    trigger_kind: String,
    trigger_form_id: Option<String>,
    trigger_tag: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SequenceUpdateInput {
    name: Option<String>,
    description: Option<String>,
    trigger_kind: Option<String>,
    trigger_form_id: Option<String>,
    trigger_tag: Option<String>,
    status: Option<String>,
}

pub(crate) async fn list_sequences(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Sequence>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<Sequence> = state
            .sequences
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|sequence| {
                matches_search(&format!("{} {}", sequence.name, sequence.description), &params)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| sequence.status == *status)
                    && params
                        .filters
                        .get("trigger_kind")
                        .is_none_or(|kind| sequence.trigger_kind == *kind)
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_sequence(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    body: web::Json<SequenceInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Sequence> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", 200)?;
        validate_trigger_kind(&input.trigger_kind)?;
        if input.trigger_kind == "form_submitted"
            && input.trigger_form_id.as_deref().unwrap_or("").trim().is_empty()
        {
            return Err(UnifiedError::bad_request(
                "trigger_form_id is required when trigger_kind is form_submitted",
            ));
        }
        if input.trigger_kind == "tag_added"
            && input.trigger_tag.as_deref().unwrap_or("").trim().is_empty()
        {
            return Err(UnifiedError::bad_request(
                "trigger_tag is required when trigger_kind is tag_added",
            ));
        }
        let now = now_string();
        let sequence = Sequence {
            id: new_id(),
            name,
            description: input.description.unwrap_or_default(),
            trigger_kind: input.trigger_kind,
            trigger_form_id: input.trigger_form_id.filter(|value| !value.trim().is_empty()),
            trigger_tag: input.trigger_tag.filter(|value| !value.trim().is_empty()),
            status: "active".to_string(),
            created_at: now.clone(),
            updated_at: now,
        };
        state.sequences.insert(sequence.id.clone(), sequence.clone());
        Ok(sequence)
    })();
    match result {
        Ok(sequence) => created(sequence),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_sequence(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Sequence> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "sequence")?;
        state
            .sequences
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Sequence not found"))
    })();
    match result {
        Ok(sequence) => ok(sequence),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_sequence(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SequenceUpdateInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Sequence> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "sequence")?;
        let input = body.into_inner();
        if let Some(kind) = &input.trigger_kind {
            validate_trigger_kind(kind)?;
        }
        if let Some(status) = &input.status {
            if !matches!(status.as_str(), "active" | "paused" | "archived") {
                return Err(UnifiedError::bad_request(
                    "status must be one of: active, paused, archived",
                ));
            }
        }
        let mut sequence = state
            .sequences
            .get_mut(&id.to_string())
            .ok_or_else(|| UnifiedError::not_found("Sequence not found"))?;
        if let Some(name) = input.name {
            sequence.name = require_text(&name, "name", 200)?;
        }
        if let Some(description) = input.description {
            sequence.description = description;
        }
        if let Some(kind) = input.trigger_kind {
            sequence.trigger_kind = kind;
        }
        if let Some(form_id) = input.trigger_form_id {
            sequence.trigger_form_id =
                if form_id.trim().is_empty() { None } else { Some(form_id) };
        }
        if let Some(tag) = input.trigger_tag {
            sequence.trigger_tag = if tag.trim().is_empty() { None } else { Some(tag) };
        }
        if let Some(status) = input.status {
            sequence.status = status;
        }
        sequence.updated_at = now_string();
        Ok(sequence.clone())
    })();
    match result {
        Ok(sequence) => ok(sequence),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_sequence(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "sequence")?;
        if state
            .enrollments
            .iter()
            .any(|entry| entry.value().sequence_id == id.to_string() && entry.value().status == "active")
        {
            return Err(UnifiedError::conflict(
                "Sequence has active enrollments; pause it instead of deleting",
            ));
        }
        match state.sequences.remove(&id.to_string()) {
            Some(_) => {
                let step_ids: Vec<String> = state
                    .steps
                    .iter()
                    .filter(|entry| entry.value().sequence_id == id.to_string())
                    .map(|entry| entry.key().clone())
                    .collect();
                for step_id in &step_ids {
                    state.steps.remove(step_id);
                }
                Ok(json!({ "deleted": true, "id": id.to_string(), "removed_steps": step_ids.len() }))
            }
            None => Err(UnifiedError::not_found("Sequence not found")),
        }
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Steps

#[derive(Debug, Deserialize)]
pub(crate) struct StepInput {
    order: Option<i64>,
    delay_seconds: Option<i64>,
    condition: Option<Value>,
    subject: String,
    body_text: Option<String>,
    body_html: Option<String>,
    template_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct StepUpdateInput {
    order: Option<i64>,
    delay_seconds: Option<i64>,
    condition: Option<Value>,
    subject: Option<String>,
    body_text: Option<String>,
    body_html: Option<String>,
    template_id: Option<String>,
}

pub(crate) async fn list_steps(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<SequenceStep>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "sequence")?;
        if !state.sequences.contains_key(&id.to_string()) {
            return Err(UnifiedError::not_found("Sequence not found"));
        }
        Ok(sequence_steps(&state, &id.to_string()))
    })();
    match result {
        Ok(steps) => ok(steps),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_step(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<StepInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SequenceStep> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "sequence")?;
        if !state.sequences.contains_key(&id.to_string()) {
            return Err(UnifiedError::not_found("Sequence not found"));
        }
        let input = body.into_inner();
        let subject = require_text(&input.subject, "subject", 500)?;
        if input.body_text.is_none() && input.body_html.is_none() && input.template_id.is_none() {
            return Err(UnifiedError::bad_request(
                "Provide body_text or body_html (or a template_id)",
            ));
        }
        let condition = input.condition.unwrap_or_else(|| json!({ "type": "always" }));
        validate_step_condition(&condition)?;
        let delay_seconds = input.delay_seconds.unwrap_or(0);
        if delay_seconds < 0 || delay_seconds > 366 * 86_400 {
            return Err(UnifiedError::bad_request(
                "delay_seconds must be between 0 and 31622400 (one year)",
            ));
        }
        let order = match input.order {
            Some(order) => {
                if order < 1 {
                    return Err(UnifiedError::bad_request("order must be at least 1"));
                }
                if sequence_steps(&state, &id.to_string())
                    .iter()
                    .any(|step| step.order == order)
                {
                    return Err(UnifiedError::conflict(format!(
                        "Step order {order} is already used in this sequence"
                    )));
                }
                order
            }
            None => {
                sequence_steps(&state, &id.to_string())
                    .iter()
                    .map(|step| step.order)
                    .max()
                    .unwrap_or(0)
                    + 1
            }
        };
        let now = now_string();
        let step = SequenceStep {
            id: new_id(),
            sequence_id: id.to_string(),
            order,
            delay_seconds,
            condition,
            subject,
            body_text: input.body_text,
            body_html: input.body_html,
            template_id: input.template_id.filter(|value| !value.trim().is_empty()),
            created_at: now.clone(),
            updated_at: now,
        };
        state.steps.insert(step.id.clone(), step.clone());
        Ok(step)
    })();
    match result {
        Ok(step) => created(step),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_step(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<StepUpdateInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SequenceStep> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let (sequence_id, step_id) = path.into_inner();
        let sequence_uuid = parse_uuid(&sequence_id, "sequence")?;
        let step_uuid = parse_uuid(&step_id, "step")?;
        if !state.sequences.contains_key(&sequence_uuid.to_string()) {
            return Err(UnifiedError::not_found("Sequence not found"));
        }
        let input = body.into_inner();
        if let Some(condition) = &input.condition {
            validate_step_condition(condition)?;
        }
        if let Some(delay) = input.delay_seconds {
            if delay < 0 || delay > 366 * 86_400 {
                return Err(UnifiedError::bad_request(
                    "delay_seconds must be between 0 and 31622400 (one year)",
                ));
            }
        }
        if let Some(order) = input.order {
            if order < 1 {
                return Err(UnifiedError::bad_request("order must be at least 1"));
            }
            if sequence_steps(&state, &sequence_uuid.to_string())
                .iter()
                .any(|step| step.order == order && step.id != step_uuid.to_string())
            {
                return Err(UnifiedError::conflict(format!(
                    "Step order {order} is already used in this sequence"
                )));
            }
        }
        let mut step = state
            .steps
            .get_mut(&step_uuid.to_string())
            .ok_or_else(|| UnifiedError::not_found("Step not found"))?;
        if step.sequence_id != sequence_uuid.to_string() {
            return Err(UnifiedError::not_found("Step not found"));
        }
        if let Some(order) = input.order {
            step.order = order;
        }
        if let Some(delay) = input.delay_seconds {
            step.delay_seconds = delay;
        }
        if let Some(condition) = input.condition {
            step.condition = condition;
        }
        if let Some(subject) = input.subject {
            step.subject = require_text(&subject, "subject", 500)?;
        }
        if let Some(body_text) = input.body_text {
            step.body_text = Some(body_text);
        }
        if let Some(body_html) = input.body_html {
            step.body_html = Some(body_html);
        }
        if let Some(template_id) = input.template_id {
            step.template_id =
                if template_id.trim().is_empty() { None } else { Some(template_id) };
        }
        step.updated_at = now_string();
        Ok(step.clone())
    })();
    match result {
        Ok(step) => ok(step),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_step(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let (sequence_id, step_id) = path.into_inner();
        let sequence_uuid = parse_uuid(&sequence_id, "sequence")?;
        let step_uuid = parse_uuid(&step_id, "step")?;
        let step = state
            .steps
            .get(&step_uuid.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Step not found"))?;
        if step.sequence_id != sequence_uuid.to_string() {
            return Err(UnifiedError::not_found("Step not found"));
        }
        if state
            .sends
            .iter()
            .any(|entry| entry.value().step_id == Some(step_uuid.to_string()))
        {
            return Err(UnifiedError::conflict(
                "Step already has sends recorded; it cannot be deleted",
            ));
        }
        state.steps.remove(&step_uuid.to_string());
        Ok(json!({ "deleted": true, "id": step_uuid.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Enrollment

#[derive(Debug, Deserialize)]
pub(crate) struct EnrollInput {
    contact_id: String,
    email: String,
}

pub(crate) async fn enroll_in_sequence(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<EnrollInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "sequence")?;
        let input = body.into_inner();
        let contact_id = require_text(&input.contact_id, "contact_id", 200)?;
        if !valid_email(&input.email) {
            return Err(UnifiedError::bad_request("email is not a valid email address"));
        }
        match enroll_contact(&state, &id.to_string(), &contact_id, input.email.trim())? {
            Some(enrollment) => Ok(json!({ "enrolled": true, "enrollment": enrollment })),
            None => Ok(json!({
                "enrolled": false,
                "already_enrolled": true,
                "sequence_id": id.to_string(),
                "contact_id": contact_id,
            })),
        }
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_enrollments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Enrollment>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "sequence")?;
        if !state.sequences.contains_key(&id.to_string()) {
            return Err(UnifiedError::not_found("Sequence not found"));
        }
        let params = query.into_inner();
        let mut items: Vec<Enrollment> = state
            .enrollments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|enrollment| {
                enrollment.sequence_id == id.to_string()
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| enrollment.status == *status)
                    && matches_search(&enrollment.email, &params)
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn sequence_funnel(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "sequence")?;
        if !state.sequences.contains_key(&id.to_string()) {
            return Err(UnifiedError::not_found("Sequence not found"));
        }
        let steps = sequence_steps(&state, &id.to_string());
        let enrollments: Vec<Enrollment> = state
            .enrollments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|enrollment| enrollment.sequence_id == id.to_string())
            .collect();
        let total = enrollments.len() as u64;
        let mut funnel: Vec<Value> = Vec::new();
        for step in &steps {
            let sent = state
                .sends
                .iter()
                .filter(|entry| {
                    entry.value().sequence_id == Some(id.to_string())
                        && entry.value().step_id.as_deref() == Some(step.id.as_str())
                        && entry.value().status != "skipped"
                })
                .count() as u64;
            let opened = state
                .sends
                .iter()
                .filter(|entry| {
                    entry.value().sequence_id == Some(id.to_string())
                        && entry.value().step_id.as_deref() == Some(step.id.as_str())
                        && (entry.value().status == "opened" || entry.value().status == "clicked")
                })
                .count() as u64;
            let skipped = state
                .sends
                .iter()
                .filter(|entry| {
                    entry.value().sequence_id == Some(id.to_string())
                        && entry.value().step_id.as_deref() == Some(step.id.as_str())
                        && entry.value().status == "skipped"
                })
                .count() as u64;
            funnel.push(json!({
                "step_id": step.id,
                "step_order": step.order,
                "subject": step.subject,
                "sent": sent,
                "opened": opened,
                "skipped": skipped,
                "conversion_from_enrolled": if total == 0 { 0.0 } else { (sent as f64 / total as f64 * 10000.0).round() / 100.0 },
            }));
        }
        let completed = enrollments
            .iter()
            .filter(|enrollment| enrollment.status == "completed")
            .count() as u64;
        let unsubscribed = enrollments
            .iter()
            .filter(|enrollment| enrollment.status == "unsubscribed")
            .count() as u64;
        Ok(json!({
            "sequence_id": id.to_string(),
            "total_enrolled": total,
            "completed": completed,
            "unsubscribed": unsubscribed,
            "active": total - completed - unsubscribed,
            "steps": funnel,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn process_due_sequences(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_SEND)?;
        let (advanced, waiting, errors) = process_due_enrollments(&state);
        Ok(json!({ "advanced": advanced, "waiting": waiting, "errors": errors }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn advance_enrollment_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_SEND)?;
        let id = parse_uuid(path.as_str(), "enrollment")?;
        let advanced = advance_enrollment(&state, &id.to_string())?;
        let enrollment = state
            .enrollments
            .get(&id.to_string())
            .map(|entry| entry.value().clone());
        Ok(json!({ "advanced": advanced, "enrollment": enrollment }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
