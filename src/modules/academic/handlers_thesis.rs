//! Academic - thesis, standing and definition handlers.
//!
//! Part of the `academic` module split; see `src/modules/academic.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use unified_core::json;
use unified_core::pagination::PageResult;
use unified_core::pagination::PaginationParams;
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use actix_web::web;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;

use super::constants::KIND_THESIS_STATUS;
use super::constants::KIND_THESIS_TRANSITION;
use super::constants::MAX_CODE;
use super::constants::MAX_TEXT;
use super::constants::PERM_ACADEMIC_MANAGE;
use super::constants::PERM_ACADEMIC_READ;
use super::constants::PERM_ACADEMIC_READ_ALL;
use super::helpers::caller;
use super::helpers::caller_company;
use super::helpers::can_read_student;
use super::helpers::check_capability;
use super::helpers::filter_matches;
use super::helpers::is_staff;
use super::helpers::load_student_for_read;
use super::helpers::matches_search;
use super::helpers::new_id;
use super::helpers::now;
use super::helpers::optional_text;
use super::helpers::paginate;
use super::helpers::parse_date;
use super::helpers::parse_optional_date;
use super::helpers::require_company;
use super::helpers::require_permission;
use super::helpers::require_tenant;
use super::helpers::require_text;
use super::helpers::today;
use super::state::AcademicState;
use super::types::AcademicDefinition;
use super::types::AcademicSetting;
use super::types::CreateDefinition;
use super::types::CreateThesis;
use super::types::RecomputeStanding;
use super::types::ScheduleDefense;
use super::types::StandingRecord;
use super::types::Thesis;
use super::types::UpdateDefinition;
use super::types::UpdateSetting;
use super::types::UpdateThesis;

// ---------------------------------------------------------------------------
// Handlers: theses
// ---------------------------------------------------------------------------

pub(crate) async fn list_theses(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Thesis>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL);
        let own_student = state.student_by_user(&caller.actor);
        let own_lecturer = state.lecturer_by_user(&caller.actor);
        let mut items: Vec<Thesis> = state
            .theses
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|thesis| {
                let visible = read_all
                    || own_student
                        .as_ref()
                        .is_some_and(|student| student.id == thesis.student_id)
                    || own_lecturer.as_ref().is_some_and(|lecturer| {
                        lecturer.id == thesis.advisor_id
                            || thesis.co_advisor_id.as_deref() == Some(lecturer.id.as_str())
                    })
                    || check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE);
                visible
                    && filter_matches(&params, "student_id", &thesis.student_id)
                    && filter_matches(&params, "advisor_id", &thesis.advisor_id)
                    && filter_matches(&params, "status", &thesis.status)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_thesis(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateThesis>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Thesis> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let body = body.into_inner();
        let student = state
            .students
            .get(&body.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Student not found"))?;
        let self_filed = student.user_id == caller.actor;
        let staff = is_staff(&ctx, &caller.role);
        if !self_filed && !staff {
            return Err(UnifiedError::not_found("Student not found"));
        }
        if !state.lecturers.contains_key(&body.advisor_id) {
            return Err(UnifiedError::not_found("Advisor not found"));
        }
        if let Some(co_advisor_id) = &body.co_advisor_id {
            if !state.lecturers.contains_key(co_advisor_id) {
                return Err(UnifiedError::not_found("Co-advisor not found"));
            }
            if co_advisor_id == &body.advisor_id {
                return Err(UnifiedError::bad_request(
                    "Co-advisor must be different from the advisor",
                ));
            }
        }
        if state.theses.iter().any(|entry| {
            let thesis = entry.value();
            thesis.student_id == student.id
                && !matches!(thesis.status.as_str(), "completed" | "rejected")
        }) {
            return Err(UnifiedError::conflict(
                "Student already has an open thesis record",
            ));
        }
        let thesis = Thesis {
            id: new_id(),
            student_id: student.id.clone(),
            title: require_text(&body.title, "title", MAX_TEXT)?,
            advisor_id: body.advisor_id.clone(),
            co_advisor_id: body.co_advisor_id.clone(),
            status: "proposal".to_string(),
            proposal_date: parse_optional_date(&body.proposal_date, "proposal_date")?,
            defense_date: None,
            defense_result: None,
            notes: optional_text(&body.notes, "notes", MAX_TEXT)?,
            created_at: now(),
            updated_at: now(),
        };
        state.theses.insert(thesis.id.clone(), thesis.clone());
        state.emit(
            "academic.thesis.created",
            json!({
                "thesis_id": thesis.id,
                "student_id": thesis.student_id,
                "advisor_id": thesis.advisor_id,
            }),
        );
        Ok(thesis)
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_thesis(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Thesis> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let thesis = state
            .theses
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Thesis not found"))?;
        let student = state
            .students
            .get(&thesis.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Thesis not found"))?;
        let visible = can_read_student(&state, &ctx, &caller, &student)
            || state
                .lecturer_by_user(&caller.actor)
                .is_some_and(|lecturer| {
                    lecturer.id == thesis.advisor_id
                        || thesis.co_advisor_id.as_deref() == Some(lecturer.id.as_str())
                });
        if !visible {
            return Err(UnifiedError::not_found("Thesis not found"));
        }
        Ok(thesis)
    })();
    json::respond(result, debug)
}

fn transition_thesis(state: &AcademicState, thesis: &mut Thesis, to: &str) -> UnifiedResult<()> {
    let allowed = state.allowed_transitions(KIND_THESIS_TRANSITION);
    if !allowed
        .iter()
        .any(|(from, target)| from == &thesis.status && target == to)
    {
        return Err(UnifiedError::conflict(format!(
            "Thesis cannot move from \"{}\" to \"{to}\"",
            thesis.status
        )));
    }
    thesis.status = to.to_string();
    thesis.updated_at = now();
    Ok(())
}

pub(crate) async fn update_thesis(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateThesis>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Thesis> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let body = body.into_inner();
        let mut thesis = state
            .theses
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Thesis not found"))?;
        let student = state
            .students
            .get(&thesis.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Thesis not found"))?;
        let is_advisor = state
            .lecturer_by_user(&caller.actor)
            .is_some_and(|lecturer| lecturer.id == thesis.advisor_id);
        let manager = check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE);
        let own_student = student.user_id == caller.actor;
        if !manager && !is_advisor && !own_student {
            return Err(UnifiedError::not_found("Thesis not found"));
        }
        if let Some(title) = &body.title {
            thesis.title = require_text(title, "title", MAX_TEXT)?;
        }
        if let Some(advisor_id) = &body.advisor_id {
            if !manager {
                return Err(UnifiedError::forbidden(
                    "Only a manager may change the thesis advisor",
                ));
            }
            if !state.lecturers.contains_key(advisor_id) {
                return Err(UnifiedError::not_found("Advisor not found"));
            }
            thesis.advisor_id = advisor_id.clone();
        }
        if let Some(co_advisor_id) = &body.co_advisor_id {
            match co_advisor_id {
                None => thesis.co_advisor_id = None,
                Some(id) => {
                    if !state.lecturers.contains_key(id) {
                        return Err(UnifiedError::not_found("Co-advisor not found"));
                    }
                    thesis.co_advisor_id = Some(id.clone());
                }
            }
        }
        if let Some(proposal_date) = &body.proposal_date {
            thesis.proposal_date = parse_optional_date(proposal_date, "proposal_date")?;
        }
        if let Some(status) = &body.status {
            let status = require_text(status, "status", MAX_CODE)?;
            if !state.valid_status(KIND_THESIS_STATUS, &status) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown thesis status \"{status}\""
                )));
            }
            if status != thesis.status {
                if !manager && !is_advisor {
                    return Err(UnifiedError::forbidden(
                        "Only the advisor or a manager may change the thesis status",
                    ));
                }
                let from = thesis.status.clone();
                transition_thesis(&state, &mut thesis, &status)?;
                state.emit(
                    "academic.thesis.status_changed",
                    json!({
                        "thesis_id": thesis.id,
                        "student_id": thesis.student_id,
                        "from_status": from,
                        "to_status": status,
                    }),
                );
            }
        }
        if let Some(notes) = &body.notes {
            thesis.notes = optional_text(notes, "notes", MAX_TEXT)?;
        }
        thesis.updated_at = now();
        state.theses.insert(thesis.id.clone(), thesis.clone());
        Ok(thesis)
    })();
    json::respond(result, debug)
}

pub(crate) async fn schedule_defense(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ScheduleDefense>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Thesis> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let body = body.into_inner();
        let mut thesis = state
            .theses
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Thesis not found"))?;
        let is_advisor = state
            .lecturer_by_user(&caller.actor)
            .is_some_and(|lecturer| lecturer.id == thesis.advisor_id);
        let manager = check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE);
        if !is_advisor && !manager {
            return Err(UnifiedError::forbidden(
                "Only the advisor or a manager may schedule a defense",
            ));
        }
        let defense_date = parse_date(&body.defense_date, "defense_date")?;
        if defense_date < today() {
            return Err(UnifiedError::bad_request(
                "defense_date must not be in the past",
            ));
        }
        if thesis.status != "defense_scheduled" {
            let from = thesis.status.clone();
            transition_thesis(&state, &mut thesis, "defense_scheduled")?;
            state.emit(
                "academic.thesis.status_changed",
                json!({
                    "thesis_id": thesis.id,
                    "student_id": thesis.student_id,
                    "from_status": from,
                    "to_status": "defense_scheduled",
                }),
            );
        }
        thesis.defense_date = Some(defense_date);
        if let Some(result) = &body.result {
            let result = require_text(result, "result", MAX_TEXT)?;
            thesis.defense_result = Some(result.clone());
            let to = if result.eq_ignore_ascii_case("passed") {
                "defended"
            } else if result.eq_ignore_ascii_case("failed") {
                "in_progress"
            } else {
                return Err(UnifiedError::bad_request(
                    "result must be \"passed\" or \"failed\"",
                ));
            };
            let from = thesis.status.clone();
            transition_thesis(&state, &mut thesis, to)?;
            state.emit(
                "academic.thesis.status_changed",
                json!({
                    "thesis_id": thesis.id,
                    "student_id": thesis.student_id,
                    "from_status": from,
                    "to_status": to,
                }),
            );
        }
        thesis.updated_at = now();
        state.theses.insert(thesis.id.clone(), thesis.clone());
        state.emit(
            "academic.thesis.defense_scheduled",
            json!({
                "thesis_id": thesis.id,
                "student_id": thesis.student_id,
                "defense_date": thesis.defense_date.map(|date| date.format("%Y-%m-%d").to_string()),
            }),
        );
        if let Some(student) = state.students.get(&thesis.student_id) {
            state.notify(
                &student.user_id,
                "Thesis defense scheduled",
                &format!(
                    "Your thesis defense is scheduled for {}.",
                    defense_date.format("%Y-%m-%d")
                ),
            );
        }
        Ok(thesis)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_thesis(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let id = path.into_inner();
        if state.theses.remove(&id).is_none() {
            return Err(UnifiedError::not_found("Thesis not found"));
        }
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: academic standing
// ---------------------------------------------------------------------------

pub(crate) async fn list_standing(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<StandingRecord>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL);
        let own_student = state.student_by_user(&caller.actor);
        let mut items: Vec<StandingRecord> = state
            .standing_records
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                let visible = read_all
                    || own_student
                        .as_ref()
                        .is_some_and(|student| student.id == record.student_id)
                    || check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE);
                visible
                    && filter_matches(&params, "student_id", &record.student_id)
                    && filter_matches(&params, "semester_id", &record.semester_id)
                    && filter_matches(&params, "standing", &record.standing)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn recompute_standing(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<RecomputeStanding>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<StandingRecord>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        if !state.semesters.contains_key(&body.semester_id) {
            return Err(UnifiedError::not_found("Semester not found"));
        }
        let student_ids: Vec<String> = match &body.student_id {
            Some(student_id) => {
                if !state.students.contains_key(student_id) {
                    return Err(UnifiedError::not_found("Student not found"));
                }
                vec![student_id.clone()]
            }
            None => state
                .students
                .iter()
                .map(|entry| entry.value().id.clone())
                .collect(),
        };
        let mut records: Vec<StandingRecord> = Vec::new();
        for student_id in student_ids {
            let (gpa, total_credits, _) = state.gpa_for(&student_id);
            let standing = state.standing_for_gpa(gpa);
            let previous = state
                .standing_records
                .iter()
                .map(|entry| entry.value().clone())
                .find(|record| {
                    record.student_id == student_id && record.semester_id == body.semester_id
                });
            let mut record = match previous {
                Some(mut existing) => {
                    existing.gpa = gpa;
                    existing.total_credits = total_credits;
                    existing.standing = standing.clone();
                    existing.updated_at = now();
                    existing
                }
                None => StandingRecord {
                    id: new_id(),
                    student_id: student_id.clone(),
                    semester_id: body.semester_id.clone(),
                    gpa,
                    total_credits,
                    standing: standing.clone(),
                    reason: None,
                    created_at: now(),
                    updated_at: now(),
                },
            };
            if standing != "good" {
                record.reason = Some(format!(
                    "GPA {gpa} falls under the \"{standing}\" standing rule for this semester"
                ));
            }
            state
                .standing_records
                .insert(record.id.clone(), record.clone());
            state.emit(
                "academic.standing.changed",
                json!({
                    "student_id": record.student_id,
                    "semester_id": record.semester_id,
                    "standing": record.standing,
                    "gpa": record.gpa,
                }),
            );
            if standing != "good" {
                if let Some(student) = state.students.get(&student_id) {
                    state.notify(
                        &student.user_id,
                        "Academic standing update",
                        &format!(
                            "Your academic standing for this semester is \"{standing}\" (GPA {gpa})."
                        ),
                    );
                }
            }
            records.push(record);
        }
        Ok(records)
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_student_standing(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<StandingRecord>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let student = load_student_for_read(&state, &ctx, &caller, path.as_str())?;
        let params = query.into_inner();
        let mut items: Vec<StandingRecord> = state
            .standing_records
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                record.student_id == student.id
                    && filter_matches(&params, "semester_id", &record.semester_id)
                    && filter_matches(&params, "standing", &record.standing)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: definitions and settings
// ---------------------------------------------------------------------------

pub(crate) async fn list_definitions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AcademicDefinition>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let mut items: Vec<AcademicDefinition> = state
            .definitions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|definition| {
                matches_search(&format!("{} {}", definition.kind, definition.key), &params)
                    && filter_matches(&params, "kind", &definition.kind)
            })
            .collect();
        items.sort_by(|a, b| {
            a.kind
                .cmp(&b.kind)
                .then_with(|| a.sort_order.cmp(&b.sort_order))
                .then_with(|| a.key.cmp(&b.key))
        });
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateDefinition>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AcademicDefinition> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let kind = require_text(&body.kind, "kind", MAX_CODE)?;
        let key = require_text(&body.key, "key", MAX_CODE)?;
        let label = require_text(&body.label, "label", MAX_TEXT)?;
        if state.definitions.iter().any(|entry| {
            let definition = entry.value();
            definition.kind == kind && definition.key == key
        }) {
            return Err(UnifiedError::conflict(format!(
                "A definition for kind \"{kind}\" with key \"{key}\" already exists"
            )));
        }
        let definition = AcademicDefinition {
            id: new_id(),
            kind,
            key,
            label,
            config: body.config.unwrap_or(Value::Object(Default::default())),
            active: true,
            sort_order: body.sort_order.unwrap_or(0),
            created_at: now(),
            updated_at: now(),
        };
        state
            .definitions
            .insert(definition.id.clone(), definition.clone());
        Ok(definition)
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AcademicDefinition> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        state
            .definitions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Definition not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateDefinition>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AcademicDefinition> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let mut definition = state
            .definitions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Definition not found"))?;
        if let Some(label) = &body.label {
            definition.label = require_text(label, "label", MAX_TEXT)?;
        }
        if let Some(config) = &body.config {
            definition.config = config.clone();
        }
        if let Some(active) = body.active {
            definition.active = active;
        }
        if let Some(sort_order) = body.sort_order {
            definition.sort_order = sort_order;
        }
        definition.updated_at = now();
        state
            .definitions
            .insert(definition.id.clone(), definition.clone());
        Ok(definition)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let id = path.into_inner();
        if state.definitions.remove(&id).is_none() {
            return Err(UnifiedError::not_found("Definition not found"));
        }
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AcademicSetting>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let mut items: Vec<AcademicSetting> = state
            .settings
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|setting| matches_search(&setting.key, &params))
            .collect();
        items.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_setting(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateSetting>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AcademicSetting> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let key = require_text(path.as_str(), "key", MAX_CODE)?;
        let body = body.into_inner();
        let setting = AcademicSetting {
            key: key.clone(),
            value: body.value,
            description: optional_text(&body.description, "description", MAX_TEXT)?,
            updated_at: now(),
        };
        state.settings.insert(key, setting.clone());
        Ok(setting)
    })();
    json::respond(result, debug)
}
