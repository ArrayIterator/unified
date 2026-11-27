//! Curriculum module - competency-based school curricula. - Report card entry handlers.
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
use super::report_cards::*;
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

pub(super) async fn list_report_card_entries(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let card_id = parse_uuid(&path.into_inner(), "report card")?;
        get_record(&state.report_cards, &card_id, "Report card")?;
        let items: Vec<ReportCardEntry> = state
            .report_card_entries
            .iter()
            .filter(|entry| entry.value().report_card_id == card_id)
            .map(|entry| entry.value().clone())
            .collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

pub(super) async fn add_report_card_entry(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReportCardEntryInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let card_id = parse_uuid(&path.into_inner(), "report card")?;
        let input = body.into_inner();
        let settings = state.settings();
        let card = get_record(&state.report_cards, &card_id, "Report card")?;
        if card.status == "issued" {
            return Err(UnifiedError::conflict(
                "Entries cannot be added to an issued report card",
            ));
        }
        let subject_id = required_uuid(&input.subject_id, "subject")?;
        let domain_id = required_uuid(&input.domain_id, "assessment domain")?;
        let subject = get_record(&state.subjects, &subject_id, "Subject")?;
        if subject.framework_id != card.framework_id {
            return Err(UnifiedError::bad_request(
                "Subject does not belong to this report card's framework",
            ));
        }
        let domain = get_record(&state.assessment_domains, &domain_id, "Assessment domain")?;
        if !domain.is_active {
            return Err(UnifiedError::conflict("Assessment domain is not active"));
        }
        if state.report_card_entries.iter().any(|entry| {
            let row = entry.value();
            row.report_card_id == card_id && row.subject_id == subject_id && row.domain_id == domain_id
        }) {
            return Err(UnifiedError::conflict(
                "This subject and assessment domain already have an entry on this report card",
            ));
        }
        let score = match input.score {
            Some(value) => Some(require_score(value, "score")?),
            None => None,
        };
        // Grade label and descriptor are derived from data when the
        // caller does not supply them: the label from the configured
        // grade bands, the descriptor from the descriptor rows.
        let grade_label = match &input.grade_label {
            Some(raw) => Some(require_text(raw, "grade_label", MAX_CODE_LEN)?),
            None => score.and_then(|value| grade_label_for(&settings, value)),
        };
        let descriptor = match &input.descriptor {
            Some(raw) => Some(require_text(raw, "descriptor", MAX_TEXT_LEN)?),
            None => score.and_then(|value| {
                descriptor_for(&state, card.framework_id, Some(domain_id), value)
            }),
        };
        // The academic module may not have announced this grade
        // yet; the link is still accepted - the grade record stays
        // owned by the academic module either way.
        let academic_grade_id = optional_text(&input.academic_grade_id, "academic_grade_id", MAX_REF_LEN)?;
        let entry = ReportCardEntry {
            id: Uuid::new_v4(),
            report_card_id: card_id,
            subject_id,
            domain_id,
            score,
            grade_label,
            descriptor,
            notes: optional_text(&input.notes, "notes", MAX_TEXT_LEN)?.unwrap_or_default(),
            academic_grade_id,
            created_at: now(),
            updated_at: now(),
        };
        state.report_card_entries.insert(entry.id, entry.clone());
        state.emit(
            "curriculum.report_card.entry.added",
            json!({ "report_card_entry": entry }),
        );
        Ok(json!(entry))
    })();
    json::respond(result, debug)
}

pub(super) async fn update_report_card_entry(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReportCardEntryInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let entry_id = parse_uuid(&path.into_inner(), "report card entry")?;
        let input = body.into_inner();
        let settings = state.settings();
        let mut entry = get_record(&state.report_card_entries, &entry_id, "Report card entry")?;
        let card = get_record(&state.report_cards, &entry.report_card_id, "Report card")?;
        if card.status == "issued" {
            return Err(UnifiedError::conflict(
                "Entries of an issued report card cannot be changed",
            ));
        }
        if let Some(value) = input.score {
            entry.score = Some(require_score(value, "score")?);
            if input.grade_label.is_none() {
                entry.grade_label = grade_label_for(&settings, value);
            }
            if input.descriptor.is_none() {
                entry.descriptor = descriptor_for(
                    &state,
                    card.framework_id,
                    Some(entry.domain_id),
                    value,
                );
            }
        }
        if let Some(raw) = &input.grade_label {
            entry.grade_label = Some(require_text(raw, "grade_label", MAX_CODE_LEN)?);
        }
        if let Some(raw) = &input.descriptor {
            entry.descriptor = Some(require_text(raw, "descriptor", MAX_TEXT_LEN)?);
        }
        if let Some(raw) = &input.notes {
            entry.notes = require_text(raw, "notes", MAX_TEXT_LEN)?;
        }
        if let Some(raw) = &input.academic_grade_id {
            entry.academic_grade_id =
                Some(require_text(raw, "academic_grade_id", MAX_REF_LEN)?);
        }
        entry.updated_at = now();
        state.report_card_entries.insert(entry.id, entry.clone());
        state.emit(
            "curriculum.report_card.entry.updated",
            json!({ "report_card_entry": entry }),
        );
        Ok(json!(entry))
    })();
    json::respond(result, debug)
}

pub(super) async fn delete_report_card_entry(
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
        let entry_id = parse_uuid(&path.into_inner(), "report card entry")?;
        let entry = get_record(&state.report_card_entries, &entry_id, "Report card entry")?;
        let card = get_record(&state.report_cards, &entry.report_card_id, "Report card")?;
        if card.status == "issued" {
            return Err(UnifiedError::conflict(
                "Entries of an issued report card cannot be removed",
            ));
        }
        state.report_card_entries.remove(&entry_id);
        state.emit(
            "curriculum.report_card.entry.removed",
            json!({ "report_card_entry_id": entry_id, "report_card_id": card.id }),
        );
        Ok(json!({ "deleted": true, "id": entry_id }))
    })();
    json::respond(result, debug)
}
