//! Curriculum module — competency-based school curricula. — Report card handlers.
//!
//! Split from `src/modules/curriculum.rs`; behaviour unchanged.

use super::ai_capabilities::*;
use super::assessment_domains::*;
use super::basic_competencies::*;
use super::constants::*;
use super::core_competencies::*;
use super::frameworks::*;
use super::grade_component_mappings::*;
use super::grade_descriptors::*;
use super::helpers::*;
use super::hooks::*;
use super::inputs::*;
use super::interventions::*;
use super::lesson_plans::*;
use super::levels::*;
use super::module::*;
use super::report_card_entries::*;
use super::settings::*;
use super::state::*;
use super::structures::*;
use super::subjects::*;
use super::types::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub(super) async fn list_report_cards(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let items: Vec<ReportCard> =
            state.report_cards.iter().map(|entry| entry.value().clone()).collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

pub(super) async fn create_report_card(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<ReportCardInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let input = body.into_inner();
        let settings = state.settings();
        let student_ref = require_text(
            input.student_ref.as_deref().unwrap_or(""),
            "student_ref",
            MAX_REF_LEN,
        )?;
        check_student_reference(&ctx, &state, &student_ref)?;
        let framework_id = required_uuid(&input.framework_id, "framework")?;
        let level_id = required_uuid(&input.level_id, "level")?;
        get_record(&state.frameworks, &framework_id, "Framework")?;
        get_record(&state.levels, &level_id, "School level")?;
        let semester = input.semester.unwrap_or(1);
        if !settings.semesters.contains(&semester) {
            return Err(UnifiedError::bad_request(format!(
                "semester \"{semester}\" is not allowed"
            )));
        }
        let academic_year = require_text(
            input.academic_year.as_deref().unwrap_or(""),
            "academic_year",
            MAX_CODE_LEN,
        )?;
        let grade_year = input.grade_year.unwrap_or(1);
        if state.report_cards.iter().any(|entry| {
            let row = entry.value();
            row.student_ref == student_ref
                && row.framework_id == framework_id
                && row.level_id == level_id
                && row.grade_year == grade_year
                && row.semester == semester
                && row.academic_year == academic_year
        }) {
            return Err(UnifiedError::conflict(
                "A report card already exists for this student, framework, level, grade year, semester, and academic year",
            ));
        }
        let status = match &input.status {
            Some(raw) => require_in_list(raw, "status", &settings.report_card_statuses)?,
            None => settings
                .report_card_statuses
                .first()
                .cloned()
                .unwrap_or_else(|| "draft".to_string()),
        };
        let card = ReportCard {
            id: Uuid::new_v4(),
            student_ref,
            framework_id,
            level_id,
            grade_year,
            semester,
            academic_year,
            status,
            issued_at: None,
            issued_by: None,
            created_at: now(),
            updated_at: now(),
        };
        state.report_cards.insert(card.id, card.clone());
        state.emit("curriculum.report_card.created", json!({ "report_card": card }));
        Ok(json!(card))
    })();
    json::respond(result, debug)
}

pub(super) async fn get_report_card(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "report card")?;
        let card = get_record(&state.report_cards, &id, "Report card")?;
        let entries: Vec<ReportCardEntry> = state
            .report_card_entries
            .iter()
            .filter(|entry| entry.value().report_card_id == id)
            .map(|entry| entry.value().clone())
            .collect();
        Ok(json!({ "report_card": card, "entries": entries }))
    })();
    json::respond(result, debug)
}

/// Guard for updates to an issued report card.
///
/// An issued card is immutable except for archiving it: the only
/// allowed update is a status change to `"archived"` with no other
/// field change. Any other request is refused with 409, so a caller
/// can neither edit grade year / semester on an issued card nor
/// un-issue it back to `"draft"` (which would leave `issued_at` /
/// `issued_by` set on a draft card). The old guard only fired when
/// no status was supplied at all, so any payload carrying a status
/// bypassed it entirely.
///
/// Archiving RETAINS `issued_at` and `issued_by`: the card was
/// issued, and the archive is a lifecycle move, not a re-issue, so
/// the issuance record stays as the audit trail.
fn validate_issued_card_update(card: &ReportCard, input: &ReportCardInput) -> UnifiedResult<()> {
    if card.status != "issued" {
        return Ok(());
    }
    let archives = matches!(&input.status, Some(raw) if raw.trim() == "archived");
    if !archives || input.grade_year.is_some() || input.semester.is_some() {
        return Err(UnifiedError::conflict(
            "An issued report card cannot be changed; the only allowed update is archiving it (status \"archived\") with no other field changes",
        ));
    }
    Ok(())
}

pub(super) async fn update_report_card(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReportCardInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let id = parse_uuid(&path.into_inner(), "report card")?;
        let input = body.into_inner();
        let settings = state.settings();
        let mut card = get_record(&state.report_cards, &id, "Report card")?;
        validate_issued_card_update(&card, &input)?;
        if let Some(raw) = &input.status {
            card.status = require_in_list(raw, "status", &settings.report_card_statuses)?;
        }
        // Archiving an issued card keeps `issued_at` / `issued_by`:
        // the card was issued, and the archive is a lifecycle move,
        // not a re-issue, so the issuance record stays as the audit
        // trail (see `validate_issued_card_update`).
        if let Some(year) = input.grade_year {
            card.grade_year = year;
        }
        if let Some(semester) = input.semester {
            if !settings.semesters.contains(&semester) {
                return Err(UnifiedError::bad_request(format!(
                    "semester \"{semester}\" is not allowed"
                )));
            }
            card.semester = semester;
        }
        card.updated_at = now();
        state.report_cards.insert(card.id, card.clone());
        state.emit("curriculum.report_card.updated", json!({ "report_card": card }));
        Ok(json!(card))
    })();
    json::respond(result, debug)
}

pub(super) async fn delete_report_card(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let id = parse_uuid(&path.into_inner(), "report card")?;
        let card = get_record(&state.report_cards, &id, "Report card")?;
        if card.status == "issued" {
            return Err(UnifiedError::conflict(
                "An issued report card cannot be deleted",
            ));
        }
        let entry_ids: Vec<Uuid> = state
            .report_card_entries
            .iter()
            .filter(|entry| entry.value().report_card_id == id)
            .map(|entry| *entry.key())
            .collect();
        for entry_id in entry_ids {
            state.report_card_entries.remove(&entry_id);
        }
        state.report_cards.remove(&id);
        state.emit("curriculum.report_card.deleted", json!({ "report_card_id": id }));
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

pub(super) async fn issue_report_card(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let id = parse_uuid(&path.into_inner(), "report card")?;
        let settings = state.settings();
        let mut card = get_record(&state.report_cards, &id, "Report card")?;
        if !settings.report_card_statuses.contains(&"issued".to_string()) {
            return Err(UnifiedError::conflict(
                "The configured report-card statuses do not include an issued status",
            ));
        }
        if card.status == "issued" {
            return Err(UnifiedError::conflict("Report card is already issued"));
        }
        card.status = "issued".to_string();
        card.issued_at = Some(now());
        card.issued_by = Some(caller.actor.clone());
        card.updated_at = now();
        state.report_cards.insert(card.id, card.clone());
        state.emit("curriculum.report_card.issued", json!({ "report_card": card }));
        Ok(json!(card))
    })();
    json::respond(result, debug)
}

pub(super) async fn print_report_card(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let id = parse_uuid(&path.into_inner(), "report card")?;
        let card = get_record(&state.report_cards, &id, "Report card")?;
        let settings = state.settings();
        let payload = json!({
            "report_card_id": card.id,
            "student_ref": card.student_ref,
            "academic_year": card.academic_year,
            "semester": card.semester,
            "paper_size": settings.default_paper_size,
            "document_type": "report_card",
            "requested_by": caller.actor,
        });
        // Event for the printing module (batch 12): it subscribes to
        // `curriculum.report_card.print_requested`. The same payload
        // is also emitted as the generic `print.job.requested` event.
        // This module never calls a printing module directly.
        state.emit("curriculum.report_card.print_requested", payload.clone());
        state.emit("print.job.requested", payload);
        Ok(json!({
            "print_requested": true,
            "report_card_id": card.id,
            "paper_size": settings.default_paper_size,
        }))
    })();
    json::respond(result, debug)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card_with_status(status: &str) -> ReportCard {
        let timestamp = now();
        ReportCard {
            id: Uuid::new_v4(),
            student_ref: "student-1".to_string(),
            framework_id: Uuid::new_v4(),
            level_id: Uuid::new_v4(),
            grade_year: 1,
            semester: 1,
            academic_year: "2026/2027".to_string(),
            status: status.to_string(),
            issued_at: if status == "issued" { Some(timestamp) } else { None },
            issued_by: if status == "issued" {
                Some("teacher-1".to_string())
            } else {
                None
            },
            created_at: timestamp,
            updated_at: timestamp,
        }
    }

    fn input(
        status: Option<&str>,
        grade_year: Option<u32>,
        semester: Option<u32>,
    ) -> ReportCardInput {
        ReportCardInput {
            student_ref: None,
            framework_id: None,
            level_id: None,
            grade_year,
            semester,
            academic_year: None,
            status: status.map(str::to_string),
        }
    }

    #[test]
    fn issued_card_update_without_status_is_rejected() {
        let card = card_with_status("issued");
        let err = validate_issued_card_update(&card, &input(None, None, None)).unwrap_err();
        assert_eq!(err.code, 409);
    }

    #[test]
    fn issued_card_cannot_be_unissued_back_to_draft() {
        let card = card_with_status("issued");
        let err =
            validate_issued_card_update(&card, &input(Some("draft"), None, None)).unwrap_err();
        assert_eq!(err.code, 409);
    }

    #[test]
    fn issued_card_cannot_change_grade_year_or_semester() {
        let card = card_with_status("issued");
        // Grade year / semester changes are refused even alongside
        // the one allowed status change (archive).
        let err = validate_issued_card_update(&card, &input(Some("archived"), Some(2), None))
            .unwrap_err();
        assert_eq!(err.code, 409);
        let err = validate_issued_card_update(&card, &input(Some("archived"), None, Some(2)))
            .unwrap_err();
        assert_eq!(err.code, 409);
        // ... and with no status change at all.
        let err =
            validate_issued_card_update(&card, &input(None, Some(2), None)).unwrap_err();
        assert_eq!(err.code, 409);
    }

    #[test]
    fn issued_card_archive_only_is_allowed() {
        let card = card_with_status("issued");
        assert!(validate_issued_card_update(&card, &input(Some("archived"), None, None)).is_ok());
    }

    #[test]
    fn non_issued_card_update_is_not_guarded() {
        let card = card_with_status("draft");
        assert!(validate_issued_card_update(&card, &input(Some("issued"), Some(2), Some(2)))
            .is_ok());
        assert!(validate_issued_card_update(&card, &input(None, None, None)).is_ok());
    }
}
