//! Academic - AI handlers and grounding.
//!
//! Part of the `academic` module split; see `src/modules/academic.rs`.
//!
//! Capability declarations live in `ai_capabilities.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::json;
use crate::modules::ai;
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use actix_web::web;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;

use super::constants::PERM_ACADEMIC_READ;
use super::helpers::caller;
use super::helpers::caller_company;
use super::helpers::load_student_for_read;
use super::helpers::require_company;
use super::helpers::require_permission;
use super::helpers::require_tenant;
use super::state::AcademicState;
use super::types::Attendance;
use super::types::Enrollment;

// AI capabilities (AI Capabilities Contract, see `crate::modules::ai`).
//
// Declared here, registered once in `init`, grounded through the
// `academic.ai.summarize_progress_requested` hook, and run through
// the AI module's gated runner. The summary is returned inline and
// is never published, sent, or stored by this module.

/// Anonymised progress grounding for one student: GPA, credits,
/// standing, enrollment and attendance aggregates, and per-subject
/// average component scores. Carries no user id, no student
/// number, and no names - only opaque subject codes and titles.
pub(crate) fn academic_progress_grounding(state: &AcademicState, student_id: &str) -> Value {
    let (gpa, total_credits, earned_credits) = state.gpa_for(student_id);
    let enrollments: Vec<Enrollment> = state
        .enrollments
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|enrollment| enrollment.student_id == student_id)
        .collect();
    let completed = enrollments
        .iter()
        .filter(|enrollment| enrollment.status == "completed")
        .count();
    let attendance: Vec<Attendance> = state
        .attendance
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| record.student_id == student_id)
        .collect();
    let present = attendance
        .iter()
        .filter(|record| record.status == "present" || record.status == "late")
        .count();
    let attendance_rate = if attendance.is_empty() {
        None
    } else {
        Some((present as f64 / attendance.len() as f64 * 100.0).round() / 100.0)
    };
    let mut per_subject: std::collections::HashMap<String, (f64, usize)> =
        std::collections::HashMap::new();
    for grade in state.grades.iter().map(|entry| entry.value().clone()) {
        if !enrollments
            .iter()
            .any(|enrollment| enrollment.id == grade.enrollment_id)
        {
            continue;
        }
        let Some(component) = state
            .grade_components
            .get(&grade.component_id)
            .map(|entry| entry.value().clone())
        else {
            continue;
        };
        let entry = per_subject.entry(component.subject_id).or_insert((0.0, 0));
        entry.0 += grade.score;
        entry.1 += 1;
    }
    let mut subjects: Vec<Value> = per_subject
        .into_iter()
        .filter_map(|(subject_id, (sum, count))| {
            let subject = state
                .subjects
                .get(&subject_id)
                .map(|entry| entry.value().clone())?;
            Some(json!({
                "subject_code": subject.code,
                "subject_title": subject.title,
                "average_score": (sum / count as f64 * 100.0).round() / 100.0,
                "graded_components": count,
            }))
        })
        .collect();
    subjects.sort_by(|a, b| {
        a.get("subject_code")
            .and_then(|value| value.as_str())
            .cmp(&b.get("subject_code").and_then(|value| value.as_str()))
    });
    json!({
        "gpa": (gpa * 100.0).round() / 100.0,
        "total_credits": total_credits,
        "earned_credits": earned_credits,
        "standing": state.standing_for_gpa(gpa),
        "enrollments": enrollments.len(),
        "completed_enrollments": completed,
        "attendance_present": present,
        "attendance_total": attendance.len(),
        "attendance_rate": attendance_rate,
        "subjects": subjects,
    })
}

/// Run one registered academic AI capability:
/// `POST /api/v1/academic/ai/{capability}` with body
/// `{"input": {...}}`. Requires `ai:use` (plus the module's own
/// `academic:read`, and - for `summarize_progress` - the module's
/// read scoping for the named student, so an AI caller cannot
/// summarize a student they may not read). Delegates to the AI
/// module's gated runner.
pub(crate) async fn ai_capability(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ai::CapabilityCallInput>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match require_company(&_tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let debug = ctx.is_debug();
    let capability = path.into_inner();
    let input = body.into_inner().input;
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ) {
        return json::error_response(&error, debug);
    }
    if capability == "summarize_progress" {
        let student_id = match input
            .get("student_id")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(id) => id.to_string(),
            None => {
                return json::error_response(
                    &UnifiedError::unprocessable("input.student_id is required"),
                    debug,
                );
            }
        };
        if let Err(error) = load_student_for_read(&state, &ctx, &caller, &student_id) {
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
    match ai::run_ai_capability(&ctx, &ai_caller, company_id, "academic", &capability, input).await
    {
        Ok(data) => json::success_response(data, 200),
        Err(error) => json::error_response(&error, debug),
    }
}
