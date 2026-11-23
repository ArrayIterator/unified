//! Elearning - AI handlers and grounding.
//!
//! Part of the `elearning` module split; see `src/modules/elearning.rs`.
//!
//! Capability declarations live in `ai_capabilities.rs`.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------
// AI capabilities (AI Capabilities Contract, see `crate::modules::ai`).
//
// Declared here, registered once in `init`, grounded through the
// `elearning.ai.<capability>_requested` hooks, and run through the
// AI module's gated runner. Drafts are returned inline and are
// never stored, published, or attached to a lesson by this module:
// creating quiz questions stays on the existing human endpoints.

/// Grounding for one lesson: its title, type, text body (capped),
/// and its course. Video / file lessons have no body text, so they
/// ground on title and course only. Unknown lessons ground nothing
/// (the endpoint 404s on them before delegating).
pub(crate) fn lesson_grounding(state: &ElearningState, lesson_id: &str) -> Value {
    let Some(lesson) = Uuid::parse_str(lesson_id).ok().and_then(|id| {
        state
            .lessons
            .get(&id)
            .map(|entry| entry.value().clone())
    }) else {
        return json!({});
    };
    let course = state
        .courses
        .get(&lesson.course_id)
        .map(|entry| entry.value().clone());
    let body = lesson.body.as_deref().unwrap_or("");
    let body: String = if body.chars().count() > 8000 {
        body.chars().take(8000).collect()
    } else {
        body.to_string()
    };
    json!({
        "lesson": {
            "title": lesson.title,
            "lesson_type": lesson.lesson_type,
            "body": body,
        },
        "course": course.map(|course| json!({
            "title": course.title,
            "description": course.description,
        })),
    })
}

/// Grounding for a quiz draft: the lesson grounding plus the
/// lesson's existing questions (truncated, at most five), so the
/// model drafts new questions instead of repeating existing ones.
pub(crate) fn quiz_grounding(state: &ElearningState, lesson_id: &str) -> Value {
    let mut grounding = lesson_grounding(state, lesson_id);
    let Some(lesson_uuid) = Uuid::parse_str(lesson_id).ok() else {
        return grounding;
    };
    let quiz_ids: Vec<Uuid> = state
        .quizzes
        .iter()
        .filter(|entry| entry.value().lesson_id == lesson_uuid)
        .map(|entry| *entry.key())
        .collect();
    let mut existing: Vec<Value> = Vec::new();
    for question in state.questions.iter().map(|entry| entry.value().clone()) {
        if quiz_ids.contains(&question.quiz_id) {
            let text: String = question.question.chars().take(300).collect();
            existing.push(json!({ "question": text }));
            if existing.len() >= 5 {
                break;
            }
        }
    }
    if let Value::Object(map) = &mut grounding {
        map.insert(
            "existing_questions".to_string(),
            Value::Array(existing),
        );
    }
    grounding
}

/// Run one registered e-learning AI capability:
/// `POST /api/v1/elearning/ai/{capability}` with body
/// `{"input": {...}}`. Requires `ai:use` plus the module's own
/// `elearning.view`; the named lesson must exist (404 otherwise).
/// Delegates to the AI module's gated runner.
pub(crate) async fn ai_capability(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ai::CapabilityCallInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let capability = path.into_inner();
    let input = body.into_inner().input;
    // Tenant edge guard (C3 pilot 3): fail closed without a
    // resolved tenant, then resolve the caller company scope
    // from the verified caller only — never the X-Company-Id
    // header. The AI runner below resolves its own company; the
    // scope here also gates the lesson grounding that follows.
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW) {
        return json::error_response(&error, debug);
    }
    if capability == "draft_quiz" || capability == "summarize_lesson" {
        let raw = input
            .get("lesson_id")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let lesson_id = match Uuid::parse_str(raw) {
            Ok(id) => id,
            Err(_) => {
                return json::error_response(
                    &UnifiedError::bad_request("input.lesson_id must be a valid id"),
                    debug,
                )
            }
        };
        // Grounding reads the lesson's course material, so the
        // lesson must exist and its course must be in the caller
        // scope before anything is delegated.
        let lesson = match state.lessons.get(&lesson_id).map(|entry| entry.value().clone()) {
            Some(lesson) => lesson,
            None => {
                return json::error_response(
                    &UnifiedError::not_found(format!("Lesson not found: {lesson_id}")),
                    debug,
                )
            }
        };
        if let Err(error) = require_course_by_id_in_scope(&state, lesson.course_id, &_scope) {
            return json::error_response(&error, debug);
        }
    }
    let ai_caller = match ai::caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = ai::require_permission(&ctx, &ai_caller.role, ai::PERM_AI_USE) {
        return json::error_response(&error, debug);
    }
    let company_id = ai::resolve_capability_company(&ctx, &req, &ai_caller);
    match ai::run_ai_capability(
        &ctx,
        &ai_caller,
        company_id,
        "elearning",
        &capability,
        input,
    )
    .await
    {
        Ok(data) => json::success_response(data, 200),
        Err(error) => json::error_response(&error, debug),
    }
}
