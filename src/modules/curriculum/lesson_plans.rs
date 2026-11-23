//! Curriculum module — competency-based school curricula. — Lesson plan handlers.
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
use super::levels::*;
use super::module::*;
use super::report_card_entries::*;
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

pub(super) async fn list_lesson_plans(
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
        let items: Vec<LessonPlan> =
            state.lesson_plans.iter().map(|entry| entry.value().clone()).collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

pub(super) async fn create_lesson_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<LessonPlanInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let input = body.into_inner();
        let settings = state.settings();
        let framework_id = required_uuid(&input.framework_id, "framework")?;
        let subject_id = required_uuid(&input.subject_id, "subject")?;
        get_record(&state.frameworks, &framework_id, "Framework")?;
        let subject = get_record(&state.subjects, &subject_id, "Subject")?;
        if subject.framework_id != framework_id {
            return Err(UnifiedError::bad_request(
                "Subject does not belong to the given framework",
            ));
        }
        let core_competency_id = optional_uuid(&input.core_competency_id, "core competency")?;
        if let Some(core_id) = core_competency_id {
            let core = get_record(&state.core_competencies, &core_id, "Core competency")?;
            if core.subject_id != subject_id {
                return Err(UnifiedError::bad_request(
                    "Core competency does not belong to the given subject",
                ));
            }
        }
        let basic_competency_id =
            optional_uuid(&input.basic_competency_id, "basic competency")?;
        if let Some(basic_id) = basic_competency_id {
            let basic = get_record(&state.basic_competencies, &basic_id, "Basic competency")?;
            if Some(basic.core_competency_id) != core_competency_id {
                return Err(UnifiedError::bad_request(
                    "Basic competency does not belong to the given core competency",
                ));
            }
        }
        let semester = input.semester.unwrap_or(1);
        if !settings.semesters.contains(&semester) {
            return Err(UnifiedError::bad_request(format!(
                "semester \"{semester}\" is not allowed; allowed values: {}",
                settings
                    .semesters
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let status = match &input.status {
            Some(raw) => require_in_list(raw, "status", &settings.lesson_plan_statuses)?,
            None => settings
                .lesson_plan_statuses
                .first()
                .cloned()
                .unwrap_or_else(|| "draft".to_string()),
        };
        let plan = LessonPlan {
            id: Uuid::new_v4(),
            framework_id,
            subject_id,
            core_competency_id,
            basic_competency_id,
            title: require_text(input.title.as_deref().unwrap_or(""), "title", MAX_TITLE_LEN)?,
            objectives: require_string_list(
                &input.objectives.unwrap_or_default(),
                "objectives",
                MAX_TEXT_LEN,
            )?,
            materials: require_string_list(
                &input.materials.unwrap_or_default(),
                "materials",
                MAX_TEXT_LEN,
            )?,
            activities: require_string_list(
                &input.activities.unwrap_or_default(),
                "activities",
                MAX_TEXT_LEN,
            )?,
            assessment_notes: optional_text(&input.assessment_notes, "assessment_notes", MAX_TEXT_LEN)?
                .unwrap_or_default(),
            time_minutes: input.time_minutes.unwrap_or(0),
            grade_year: input.grade_year.unwrap_or(1),
            semester,
            teacher_id: require_text(
                input.teacher_id.as_deref().unwrap_or(&caller.actor),
                "teacher_id",
                MAX_REF_LEN,
            )?,
            status,
            created_at: now(),
            updated_at: now(),
        };
        state.lesson_plans.insert(plan.id, plan.clone());
        state.emit("curriculum.lesson_plan.created", json!({ "lesson_plan": plan }));
        Ok(json!(plan))
    })();
    json::respond(result, debug)
}

pub(super) async fn get_lesson_plan(
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
        let id = parse_uuid(&path.into_inner(), "lesson plan")?;
        Ok(json!(get_record(&state.lesson_plans, &id, "Lesson plan")?))
    })();
    json::respond(result, debug)
}

pub(super) async fn update_lesson_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<LessonPlanInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let id = parse_uuid(&path.into_inner(), "lesson plan")?;
        let input = body.into_inner();
        let settings = state.settings();
        let mut plan = get_record(&state.lesson_plans, &id, "Lesson plan")?;
        if check_capability(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)
            || plan.teacher_id == caller.actor
        {
            // Teacher owns the plan, or caller manages the module.
        } else {
            return Err(UnifiedError::forbidden(
                "Only a manager or the plan teacher may change this lesson plan",
            ));
        }
        if let Some(raw) = &input.title {
            plan.title = require_text(raw, "title", MAX_TITLE_LEN)?;
        }
        if let Some(values) = &input.objectives {
            plan.objectives = require_string_list(values, "objectives", MAX_TEXT_LEN)?;
        }
        if let Some(values) = &input.materials {
            plan.materials = require_string_list(values, "materials", MAX_TEXT_LEN)?;
        }
        if let Some(values) = &input.activities {
            plan.activities = require_string_list(values, "activities", MAX_TEXT_LEN)?;
        }
        if let Some(raw) = &input.assessment_notes {
            plan.assessment_notes = require_text(raw, "assessment_notes", MAX_TEXT_LEN)?;
        }
        if let Some(minutes) = input.time_minutes {
            plan.time_minutes = minutes;
        }
        if let Some(year) = input.grade_year {
            plan.grade_year = year;
        }
        if let Some(semester) = input.semester {
            if !settings.semesters.contains(&semester) {
                return Err(UnifiedError::bad_request(format!(
                    "semester \"{semester}\" is not allowed"
                )));
            }
            plan.semester = semester;
        }
        if let Some(raw) = &input.status {
            plan.status = require_in_list(raw, "status", &settings.lesson_plan_statuses)?;
        }
        plan.updated_at = now();
        state.lesson_plans.insert(plan.id, plan.clone());
        state.emit("curriculum.lesson_plan.updated", json!({ "lesson_plan": plan }));
        Ok(json!(plan))
    })();
    json::respond(result, debug)
}

pub(super) async fn delete_lesson_plan(
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
        let id = parse_uuid(&path.into_inner(), "lesson plan")?;
        let plan = get_record(&state.lesson_plans, &id, "Lesson plan")?;
        if !check_capability(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)
            && plan.teacher_id != caller.actor
        {
            return Err(UnifiedError::forbidden(
                "Only a manager or the plan teacher may delete this lesson plan",
            ));
        }
        state.lesson_plans.remove(&id);
        state.emit("curriculum.lesson_plan.deleted", json!({ "lesson_plan_id": id }));
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

pub(super) fn set_lesson_plan_status(
    ctx: &AppContext,
    state: &CurriculumState,
    caller: &Caller,
    id: Uuid,
    status: &str,
    event: &str,
) -> UnifiedResult<LessonPlan> {
    require_teach(ctx, &caller.role)?;
    let settings = state.settings();
    let status = require_in_list(status, "status", &settings.lesson_plan_statuses)?;
    let mut plan = get_record(&state.lesson_plans, &id, "Lesson plan")?;
    if !check_capability(ctx, &caller.role, PERM_CURRICULUM_MANAGE)
        && plan.teacher_id != caller.actor
    {
        return Err(UnifiedError::forbidden(
            "Only a manager or the plan teacher may change this lesson plan",
        ));
    }
    // `archived` is terminal: an archived plan can never leave that
    // status. A no-op transition (publishing an already-published
    // plan, archiving an already-archived one) is likewise refused
    // with 409 instead of re-emitting the event, so subscribers
    // never see a duplicate publish/archive for a plan that did
    // not change.
    if plan.status == "archived" {
        return Err(UnifiedError::conflict(
            "An archived lesson plan cannot change status",
        ));
    }
    if plan.status == status {
        return Err(UnifiedError::conflict(format!(
            "Lesson plan is already {status}"
        )));
    }
    plan.status = status;
    plan.updated_at = now();
    state.lesson_plans.insert(plan.id, plan.clone());
    state.emit(event, json!({ "lesson_plan": plan }));
    Ok(plan)
}

pub(super) async fn publish_lesson_plan(
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
        let id = parse_uuid(&path.into_inner(), "lesson plan")?;
        let plan = set_lesson_plan_status(
            &ctx,
            &state,
            &caller,
            id,
            "published",
            "curriculum.lesson_plan.published",
        )?;
        Ok(json!(plan))
    })();
    json::respond(result, debug)
}

pub(super) async fn archive_lesson_plan(
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
        let id = parse_uuid(&path.into_inner(), "lesson plan")?;
        let plan = set_lesson_plan_status(
            &ctx,
            &state,
            &caller,
            id,
            "archived",
            "curriculum.lesson_plan.archived",
        )?;
        Ok(json!(plan))
    })();
    json::respond(result, debug)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;

    /// A context whose `role.check` handler allows every permission.
    fn allowing_context() -> AppContext {
        let ctx = AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        );
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                context.set("allowed", serde_json::Value::Bool(true));
            }),
        );
        ctx
    }

    fn teacher() -> Caller {
        Caller {
            actor: "teacher-1".to_string(),
            role: "teacher".to_string(),
        }
    }

    fn plan_with_status(state: &CurriculumState, status: &str) -> Uuid {
        let id = Uuid::new_v4();
        let timestamp = now();
        state.lesson_plans.insert(
            id,
            LessonPlan {
                id,
                framework_id: Uuid::new_v4(),
                subject_id: Uuid::new_v4(),
                core_competency_id: None,
                basic_competency_id: None,
                title: "Plan".to_string(),
                objectives: vec![],
                materials: vec![],
                activities: vec![],
                assessment_notes: String::new(),
                time_minutes: 45,
                grade_year: 1,
                semester: 1,
                teacher_id: "teacher-1".to_string(),
                status: status.to_string(),
                created_at: timestamp,
                updated_at: timestamp,
            },
        );
        id
    }

    #[test]
    fn publish_draft_plan_succeeds() {
        let ctx = allowing_context();
        let state = CurriculumState::default();
        let id = plan_with_status(&state, "draft");
        let plan = set_lesson_plan_status(
            &ctx,
            &state,
            &teacher(),
            id,
            "published",
            "curriculum.lesson_plan.published",
        )
        .unwrap();
        assert_eq!(plan.status, "published");
    }

    #[test]
    fn republish_published_plan_is_rejected_without_reemit() {
        let ctx = allowing_context();
        let state = CurriculumState::default();
        let id = plan_with_status(&state, "published");
        let err = set_lesson_plan_status(
            &ctx,
            &state,
            &teacher(),
            id,
            "published",
            "curriculum.lesson_plan.published",
        )
        .unwrap_err();
        assert_eq!(err.code, 409);
        assert_eq!(
            state.lesson_plans.get(&id).unwrap().status,
            "published"
        );
    }

    #[test]
    fn archive_published_plan_succeeds() {
        let ctx = allowing_context();
        let state = CurriculumState::default();
        let id = plan_with_status(&state, "published");
        let plan = set_lesson_plan_status(
            &ctx,
            &state,
            &teacher(),
            id,
            "archived",
            "curriculum.lesson_plan.archived",
        )
        .unwrap();
        assert_eq!(plan.status, "archived");
    }

    #[test]
    fn archived_plan_cannot_leave_terminal_status() {
        let ctx = allowing_context();
        let state = CurriculumState::default();
        let id = plan_with_status(&state, "archived");
        let err = set_lesson_plan_status(
            &ctx,
            &state,
            &teacher(),
            id,
            "published",
            "curriculum.lesson_plan.published",
        )
        .unwrap_err();
        assert_eq!(err.code, 409);
        let err = set_lesson_plan_status(
            &ctx,
            &state,
            &teacher(),
            id,
            "archived",
            "curriculum.lesson_plan.archived",
        )
        .unwrap_err();
        assert_eq!(err.code, 409);
        assert_eq!(state.lesson_plans.get(&id).unwrap().status, "archived");
    }
}
