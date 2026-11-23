//! Curriculum module — competency-based school curricula. — Hook handlers (academic module events).
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
use super::inputs::*;
use super::interventions::*;
use super::lesson_plans::*;
use super::levels::*;
use super::module::*;
use super::report_card_entries::*;
use super::report_cards::*;
use super::settings::*;
use super::state::*;
use super::structures::*;
use super::subjects::*;
use super::types::*;

use unified_core::hooks::HookContext;
use uuid::Uuid;

/// Handle `academic.student.created` (canonical payload
/// `{student_id, user_id, program_id, application_id}`) and the
/// legacy `{student_ref, level_id, ...}` shape. The canonical identity key is the academic
/// `student_id`, stored here as `student_ref`; `level_id` is kept
/// when the event carries one (the academic module has no level
/// concept, so the canonical event never does). Only the reference
/// is cached — students stay owned by the academic module.
pub(super) fn on_academic_student(state: CurriculumState, ctx: &mut HookContext) {
    let Some(student_ref) = ctx
        .get("student_id")
        .or_else(|| ctx.get("student_ref"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let level_id = ctx
        .get("level_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let program_id = ctx
        .get("program_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    state.known_students.insert(
        student_ref.clone(),
        KnownStudent {
            student_ref,
            level_id,
            program_id,
            source_event: ctx.event.clone(),
            seen_at: now(),
        },
    );
}

/// Handle `academic.enrollment.approved` (`{enrollment_id,
/// student_id, subject_id, semester_id}`). Records the
/// enrollment-to-student link the grade handler resolves through:
/// a grade event carries only an `enrollment_id`, so without this
/// cache the grade's student is unknowable here. The student is
/// also recorded in the lookup cache. A dropped enrollment is not
/// removed: the cache records that the student was seen, and
/// report cards for past semesters must keep validating.
pub(super) fn on_academic_enrollment_approved(state: CurriculumState, ctx: &mut HookContext) {
    let (Some(enrollment_id), Some(student_ref)) = (
        ctx.get("enrollment_id")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .filter(|value| !value.is_empty()),
        ctx.get("student_id")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .filter(|value| !value.is_empty()),
    ) else {
        return;
    };
    let subject_id = ctx
        .get("subject_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let semester_id = ctx
        .get("semester_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    state.known_enrollments.insert(
        enrollment_id.clone(),
        KnownEnrollment {
            enrollment_id,
            student_ref: student_ref.clone(),
            subject_id,
            semester_id,
            seen_at: now(),
        },
    );
    // Record the student only when the lookup cache does not
    // already hold them: `academic.student.created` carries the
    // program id, and this event does not — overwriting would drop
    // it.
    if !state.known_students.contains_key(&student_ref) {
        state.known_students.insert(
            student_ref.clone(),
            KnownStudent {
                student_ref,
                level_id: None,
                program_id: None,
                source_event: ctx.event.clone(),
                seen_at: now(),
            },
        );
    }
}

/// Resolve one academic grade component to a basic competency
/// through the admin-managed mapping table. Returns the competency
/// id when a mapping row exists AND the competency still exists in
/// this module's store; otherwise `None` (a mapping that points at
/// a deleted competency resolves to nothing — never to a guessed
/// replacement).
pub(super) fn competency_for_component(
    state: &CurriculumState,
    component_id: &str,
) -> Option<String> {
    let mapping = state.component_mappings.get(component_id)?;
    let competency_id = mapping.value().basic_competency_id.clone();
    let parsed = Uuid::parse_str(&competency_id).ok()?;
    if state.basic_competencies.contains_key(&parsed) {
        Some(competency_id)
    } else {
        None
    }
}

/// Handle `academic.grade.recorded`.
///
/// Canonical payload `{grade_id, enrollment_id, component_id,
/// score}` — resolved in two steps, both inside this module:
///   1. enrollment → student: `enrollment_id` is looked up in the
///      cache filled by `academic.enrollment.approved`. When the
///      enrollment was never seen (approved before this module
///      registered, or the approval event was missed), the student
///      is unknown and the grade is recorded unmapped with reason
///      `unknown_enrollment` — the student is never guessed.
///   2. component → competency: `component_id` is looked up in the
///      admin-managed grade-component mapping table. When no row
///      exists (or the row points at a deleted competency), the
///      grade is recorded unmapped with reason
///      `unmapped_component` — a mapping is never fabricated.
///
/// Legacy payload `{academic_grade_id, student_ref,
/// basic_competency_id, score}` is still accepted: the student and
/// competency arrive with the event, so the grade records as
/// `mapped` when a competency is given (and exists) and `unmapped`
/// otherwise.
///
/// Every grade that carries an id is recorded, mapped or not: a
/// grade is never dropped silently. The score is kept only as link
/// context; the grade record itself stays in the academic module.
pub(super) fn on_academic_grade_recorded(state: CurriculumState, ctx: &mut HookContext) {
    let Some(grade_id) = ctx
        .get("grade_id")
        .or_else(|| ctx.get("academic_grade_id"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let score = ctx
        .get("score")
        .and_then(|value| value.as_f64())
        .filter(|value| value.is_finite());
    // Legacy shape: student and competency arrive with the event.
    if let Some(student_ref) = ctx
        .get("student_ref")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|value| !value.is_empty())
    {
        let raw_competency = ctx
            .get("basic_competency_id")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .filter(|value| !value.is_empty());
        let competency = raw_competency.clone().filter(|value| {
            Uuid::parse_str(value.as_str())
                .is_ok_and(|id| state.basic_competencies.contains_key(&id))
        });
        let (mapping_status, mapping_reason) = match (&competency, &raw_competency) {
            (Some(_), _) => ("mapped".to_string(), None),
            (None, Some(_)) => (
                "unmapped".to_string(),
                Some("unknown_basic_competency".to_string()),
            ),
            (None, None) => (
                "unmapped".to_string(),
                Some("no_basic_competency_in_event".to_string()),
            ),
        };
        state.known_grades.insert(
            grade_id.clone(),
            KnownGrade {
                academic_grade_id: grade_id,
                student_ref,
                basic_competency_id: competency,
                enrollment_id: ctx
                    .get("enrollment_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                component_id: ctx
                    .get("component_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                score,
                mapping_status,
                mapping_reason,
                recorded_at: now(),
            },
        );
        return;
    }
    // Canonical shape: resolve student via the enrollment cache,
    // competency via the mapping table.
    let enrollment_id = ctx
        .get("enrollment_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|value| !value.is_empty());
    let component_id = ctx
        .get("component_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|value| !value.is_empty());
    let enrollment = enrollment_id
        .as_ref()
        .and_then(|id| state.known_enrollments.get(id))
        .map(|entry| entry.value().clone());
    let (student_ref, student_reason) = match &enrollment {
        Some(link) => (link.student_ref.clone(), None),
        None => (
            String::new(),
            Some("unknown_enrollment".to_string()),
        ),
    };
    let (competency, competency_reason) = match &component_id {
        Some(component) => match competency_for_component(&state, component) {
            Some(id) => (Some(id), None),
            None => (
                None,
                Some("unmapped_component".to_string()),
            ),
        },
        None => (
            None,
            Some("missing_component_id".to_string()),
        ),
    };
    let mapping_reason = student_reason.or(competency_reason);
    let mapping_status = if mapping_reason.is_none() && competency.is_some() {
        "mapped".to_string()
    } else {
        "unmapped".to_string()
    };
    state.known_grades.insert(
        grade_id.clone(),
        KnownGrade {
            academic_grade_id: grade_id,
            student_ref,
            basic_competency_id: competency,
            enrollment_id,
            component_id,
            score,
            mapping_status,
            mapping_reason,
            recorded_at: now(),
        },
    );
}
