//! Academic - grade-component, room, semester, calendar and schedule handlers.
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

use super::constants::KIND_CALENDAR_EVENT_TYPE;
use super::constants::KIND_COMPONENT_KIND;
use super::constants::MAX_CODE;
use super::constants::MAX_TEXT;
use super::constants::PERM_ACADEMIC_MANAGE;
use super::constants::PERM_ACADEMIC_READ;
use super::constants::PERM_ACADEMIC_READ_ALL;
use super::helpers::caller;
use super::helpers::caller_company;
use super::helpers::check_capability;
use super::helpers::filter_matches;
use super::helpers::matches_search;
use super::helpers::new_id;
use super::helpers::now;
use super::helpers::optional_text;
use super::helpers::paginate;
use super::helpers::parse_date;
use super::helpers::parse_day_of_week;
use super::helpers::parse_time;
use super::helpers::parse_weight;
use super::helpers::require_code;
use super::helpers::require_company;
use super::helpers::require_permission;
use super::helpers::require_tenant;
use super::helpers::require_text;
use super::helpers::time_to_minutes;
use super::state::AcademicState;
use super::types::CalendarEvent;
use super::types::CreateCalendarEvent;
use super::types::CreateGradeComponent;
use super::types::CreateRoom;
use super::types::CreateSchedule;
use super::types::CreateSemester;
use super::types::GradeComponent;
use super::types::Room;
use super::types::Schedule;
use super::types::Semester;
use super::types::UpdateCalendarEvent;
use super::types::UpdateGradeComponent;
use super::types::UpdateRoom;
use super::types::UpdateSchedule;
use super::types::UpdateSemester;

// Handlers: grade components

pub(crate) async fn list_grade_components(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<GradeComponent>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let subject_id = path.into_inner();
        if !state.subjects.contains_key(&subject_id) {
            return Err(UnifiedError::not_found("Subject not found"));
        }
        let params = query.into_inner();
        let mut items: Vec<GradeComponent> = state
            .grade_components
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|component| {
                component.subject_id == subject_id
                    && filter_matches(&params, "kind", &component.kind)
            })
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_grade_component(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateGradeComponent>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GradeComponent> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let subject_id = path.into_inner();
        if !state.subjects.contains_key(&subject_id) {
            return Err(UnifiedError::not_found("Subject not found"));
        }
        let body = body.into_inner();
        let name = require_text(&body.name, "name", MAX_TEXT)?;
        let kind = require_text(&body.kind, "kind", MAX_CODE)?;
        if !state.valid_status(KIND_COMPONENT_KIND, &kind) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown grade component kind \"{kind}\""
            )));
        }
        let weight = parse_weight(body.weight_percent, "weight_percent")?;
        let component = GradeComponent {
            id: new_id(),
            subject_id,
            name,
            kind,
            weight_percent: weight,
            created_at: now(),
            updated_at: now(),
        };
        state
            .grade_components
            .insert(component.id.clone(), component.clone());
        Ok(component)
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_grade_component(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateGradeComponent>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GradeComponent> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let mut component = state
            .grade_components
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Grade component not found"))?;
        if let Some(name) = &body.name {
            component.name = require_text(name, "name", MAX_TEXT)?;
        }
        if let Some(kind) = &body.kind {
            let kind = require_text(kind, "kind", MAX_CODE)?;
            if !state.valid_status(KIND_COMPONENT_KIND, &kind) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown grade component kind \"{kind}\""
                )));
            }
            component.kind = kind;
        }
        if let Some(weight) = body.weight_percent {
            component.weight_percent = parse_weight(weight, "weight_percent")?;
        }
        component.updated_at = now();
        state
            .grade_components
            .insert(component.id.clone(), component.clone());
        Ok(component)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_grade_component(
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
        if !state.grade_components.contains_key(&id) {
            return Err(UnifiedError::not_found("Grade component not found"));
        }
        if state
            .grades
            .iter()
            .any(|entry| entry.value().component_id == id)
        {
            return Err(UnifiedError::conflict(
                "Grade component cannot be deleted while grades reference it",
            ));
        }
        state.grade_components.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// Handlers: rooms

pub(crate) async fn list_rooms(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Room>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Room> = state
            .rooms
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|room| {
                matches_search(&format!("{} {}", room.code, room.name), &params)
                    && filter_matches(&params, "status", &room.status)
            })
            .collect();
        items.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_room(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateRoom>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Room> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let code = require_code(&body.code, "code")?;
        let name = require_text(&body.name, "name", MAX_TEXT)?;
        if state
            .rooms
            .iter()
            .any(|entry| entry.value().code.eq_ignore_ascii_case(&code))
        {
            return Err(UnifiedError::conflict(format!(
                "A room with code \"{code}\" already exists"
            )));
        }
        let capacity = match body.capacity {
            None => None,
            Some(value) => {
                if value <= 0 || value > 10_000 {
                    return Err(UnifiedError::bad_request(
                        "capacity must be between 1 and 10000",
                    ));
                }
                Some(value)
            }
        };
        let room = Room {
            id: new_id(),
            code,
            name,
            building: optional_text(&body.building, "building", MAX_TEXT)?,
            capacity,
            status: "active".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state.rooms.insert(room.id.clone(), room.clone());
        Ok(room)
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_room(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Room> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        state
            .rooms
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Room not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_room(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRoom>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Room> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let mut room = state
            .rooms
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Room not found"))?;
        if let Some(name) = &body.name {
            room.name = require_text(name, "name", MAX_TEXT)?;
        }
        if let Some(building) = &body.building {
            room.building = optional_text(building, "building", MAX_TEXT)?;
        }
        if let Some(capacity) = &body.capacity {
            room.capacity = match capacity {
                None => None,
                Some(value) => {
                    if *value <= 0 || *value > 10_000 {
                        return Err(UnifiedError::bad_request(
                            "capacity must be between 1 and 10000",
                        ));
                    }
                    Some(*value)
                }
            };
        }
        if let Some(status) = &body.status {
            let status = require_text(status, "status", MAX_CODE)?;
            if status != "active" && status != "inactive" {
                return Err(UnifiedError::bad_request(
                    "status must be \"active\" or \"inactive\"",
                ));
            }
            room.status = status;
        }
        room.updated_at = now();
        state.rooms.insert(room.id.clone(), room.clone());
        Ok(room)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_room(
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
        if !state.rooms.contains_key(&id) {
            return Err(UnifiedError::not_found("Room not found"));
        }
        if state
            .schedules
            .iter()
            .any(|entry| entry.value().room_id == id && entry.value().status == "active")
        {
            return Err(UnifiedError::conflict(
                "Room cannot be deleted while active schedules use it",
            ));
        }
        state.rooms.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// Handlers: semesters

pub(crate) async fn list_semesters(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Semester>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Semester> = state
            .semesters
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|semester| {
                matches_search(&format!("{} {}", semester.code, semester.name), &params)
                    && filter_matches(&params, "status", &semester.status)
            })
            .collect();
        items.sort_by(|a, b| b.start_date.cmp(&a.start_date));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_semester(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateSemester>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Semester> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let code = require_code(&body.code, "code")?;
        let name = require_text(&body.name, "name", MAX_TEXT)?;
        if state
            .semesters
            .iter()
            .any(|entry| entry.value().code.eq_ignore_ascii_case(&code))
        {
            return Err(UnifiedError::conflict(format!(
                "A semester with code \"{code}\" already exists"
            )));
        }
        let start_date = parse_date(&body.start_date, "start_date")?;
        let end_date = parse_date(&body.end_date, "end_date")?;
        if end_date <= start_date {
            return Err(UnifiedError::bad_request(
                "end_date must be after start_date",
            ));
        }
        let semester = Semester {
            id: new_id(),
            code,
            name,
            start_date,
            end_date,
            status: "active".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state
            .semesters
            .insert(semester.id.clone(), semester.clone());
        Ok(semester)
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_semester(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Semester> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        state
            .semesters
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Semester not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_semester(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateSemester>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Semester> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let mut semester = state
            .semesters
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Semester not found"))?;
        if let Some(name) = &body.name {
            semester.name = require_text(name, "name", MAX_TEXT)?;
        }
        if let Some(start_date) = &body.start_date {
            semester.start_date = parse_date(start_date, "start_date")?;
        }
        if let Some(end_date) = &body.end_date {
            semester.end_date = parse_date(end_date, "end_date")?;
        }
        if semester.end_date <= semester.start_date {
            return Err(UnifiedError::bad_request(
                "end_date must be after start_date",
            ));
        }
        if let Some(status) = &body.status {
            let status = require_text(status, "status", MAX_CODE)?;
            if !matches!(status.as_str(), "active" | "current" | "completed") {
                return Err(UnifiedError::bad_request(
                    "status must be \"active\", \"current\", or \"completed\"",
                ));
            }
            semester.status = status;
        }
        semester.updated_at = now();
        state
            .semesters
            .insert(semester.id.clone(), semester.clone());
        Ok(semester)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_semester(
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
        if !state.semesters.contains_key(&id) {
            return Err(UnifiedError::not_found("Semester not found"));
        }
        if state
            .enrollments
            .iter()
            .any(|entry| entry.value().semester_id == id)
        {
            return Err(UnifiedError::conflict(
                "Semester cannot be deleted while enrollments reference it",
            ));
        }
        if state
            .schedules
            .iter()
            .any(|entry| entry.value().semester_id == id && entry.value().status == "active")
        {
            return Err(UnifiedError::conflict(
                "Semester cannot be deleted while active schedules reference it",
            ));
        }
        state.semesters.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn activate_semester(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Semester> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let id = path.into_inner();
        let mut semester = state
            .semesters
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Semester not found"))?;
        // Only one semester is current at a time; the previous
        // current semester returns to active.
        for entry in state.semesters.iter() {
            if entry.value().status == "current" && entry.value().id != id {
                let mut previous = entry.value().clone();
                previous.status = "active".to_string();
                previous.updated_at = now();
                state.semesters.insert(previous.id.clone(), previous);
            }
        }
        semester.status = "current".to_string();
        semester.updated_at = now();
        state
            .semesters
            .insert(semester.id.clone(), semester.clone());
        Ok(semester)
    })();
    json::respond(result, debug)
}

// Handlers: academic calendar

pub(crate) async fn list_calendar_events(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<CalendarEvent>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let mut items: Vec<CalendarEvent> = state
            .calendar_events
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|event| {
                matches_search(&event.title, &params)
                    && filter_matches(&params, "event_type", &event.event_type)
                    && params.filters.get("semester_id").is_none_or(|semester_id| {
                        event.semester_id.as_deref() == Some(semester_id.as_str())
                    })
            })
            .collect();
        items.sort_by(|a, b| a.start_date.cmp(&b.start_date));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_calendar_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateCalendarEvent>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CalendarEvent> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let event_type = require_text(&body.event_type, "event_type", MAX_CODE)?;
        if !state.valid_status(KIND_CALENDAR_EVENT_TYPE, &event_type) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown calendar event type \"{event_type}\""
            )));
        }
        if let Some(semester_id) = &body.semester_id {
            if !state.semesters.contains_key(semester_id) {
                return Err(UnifiedError::not_found("Semester not found"));
            }
        }
        let start_date = parse_date(&body.start_date, "start_date")?;
        let end_date = parse_date(&body.end_date, "end_date")?;
        if end_date < start_date {
            return Err(UnifiedError::bad_request(
                "end_date must not be before start_date",
            ));
        }
        let event = CalendarEvent {
            id: new_id(),
            semester_id: body.semester_id.clone(),
            event_type,
            title: require_text(&body.title, "title", MAX_TEXT)?,
            start_date,
            end_date,
            description: optional_text(&body.description, "description", MAX_TEXT)?,
            created_at: now(),
            updated_at: now(),
        };
        state
            .calendar_events
            .insert(event.id.clone(), event.clone());
        Ok(event)
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_calendar_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CalendarEvent> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        state
            .calendar_events
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Calendar event not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_calendar_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCalendarEvent>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CalendarEvent> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let mut event = state
            .calendar_events
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Calendar event not found"))?;
        if let Some(event_type) = &body.event_type {
            let event_type = require_text(event_type, "event_type", MAX_CODE)?;
            if !state.valid_status(KIND_CALENDAR_EVENT_TYPE, &event_type) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown calendar event type \"{event_type}\""
                )));
            }
            event.event_type = event_type;
        }
        if let Some(title) = &body.title {
            event.title = require_text(title, "title", MAX_TEXT)?;
        }
        if let Some(start_date) = &body.start_date {
            event.start_date = parse_date(start_date, "start_date")?;
        }
        if let Some(end_date) = &body.end_date {
            event.end_date = parse_date(end_date, "end_date")?;
        }
        if event.end_date < event.start_date {
            return Err(UnifiedError::bad_request(
                "end_date must not be before start_date",
            ));
        }
        if let Some(description) = &body.description {
            event.description = optional_text(description, "description", MAX_TEXT)?;
        }
        event.updated_at = now();
        state
            .calendar_events
            .insert(event.id.clone(), event.clone());
        Ok(event)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_calendar_event(
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
        if state.calendar_events.remove(&id).is_none() {
            return Err(UnifiedError::not_found("Calendar event not found"));
        }
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// Handlers: schedules

pub(crate) async fn list_schedules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Schedule>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL);
        let own_lecturer = state.lecturer_by_user(&caller.actor);
        let mut items: Vec<Schedule> = state
            .schedules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|schedule| {
                let visible = read_all
                    || own_lecturer
                        .as_ref()
                        .is_some_and(|lecturer| lecturer.id == schedule.lecturer_id)
                    || check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE);
                visible
                    && filter_matches(&params, "subject_id", &schedule.subject_id)
                    && filter_matches(&params, "lecturer_id", &schedule.lecturer_id)
                    && filter_matches(&params, "room_id", &schedule.room_id)
                    && filter_matches(&params, "semester_id", &schedule.semester_id)
                    && filter_matches(&params, "status", &schedule.status)
            })
            .collect();
        items.sort_by(|a, b| {
            a.day_of_week
                .cmp(&b.day_of_week)
                .then_with(|| a.start_time.cmp(&b.start_time))
        });
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateSchedule>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Schedule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        if !state.subjects.contains_key(&body.subject_id) {
            return Err(UnifiedError::not_found("Subject not found"));
        }
        if !state.lecturers.contains_key(&body.lecturer_id) {
            return Err(UnifiedError::not_found("Lecturer not found"));
        }
        if !state.rooms.contains_key(&body.room_id) {
            return Err(UnifiedError::not_found("Room not found"));
        }
        if !state.semesters.contains_key(&body.semester_id) {
            return Err(UnifiedError::not_found("Semester not found"));
        }
        let day_of_week = parse_day_of_week(body.day_of_week)?;
        let start_time = parse_time(&body.start_time, "start_time")?;
        let end_time = parse_time(&body.end_time, "end_time")?;
        let start_minutes = time_to_minutes(&start_time).unwrap_or(0);
        let end_minutes = time_to_minutes(&end_time).unwrap_or(0);
        if end_minutes <= start_minutes {
            return Err(UnifiedError::bad_request(
                "end_time must be after start_time",
            ));
        }
        if let Some(conflict) = state.schedule_conflict(
            &body.semester_id,
            &body.room_id,
            &body.lecturer_id,
            day_of_week,
            start_minutes,
            end_minutes,
            None,
        ) {
            return Err(UnifiedError::conflict(format!(
                "Schedule conflicts with existing schedule {} (room or lecturer already booked at that time)",
                conflict.id
            )));
        }
        let schedule = Schedule {
            id: new_id(),
            subject_id: body.subject_id.clone(),
            lecturer_id: body.lecturer_id.clone(),
            room_id: body.room_id.clone(),
            semester_id: body.semester_id.clone(),
            day_of_week,
            start_time,
            end_time,
            status: "active".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state
            .schedules
            .insert(schedule.id.clone(), schedule.clone());
        state.emit(
            "academic.schedule.created",
            json!({
                "schedule_id": schedule.id,
                "subject_id": schedule.subject_id,
                "room_id": schedule.room_id,
                "semester_id": schedule.semester_id,
            }),
        );
        Ok(schedule)
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Schedule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let schedule = state
            .schedules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Schedule not found"))?;
        let allowed = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL)
            || check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)
            || state
                .lecturer_by_user(&caller.actor)
                .is_some_and(|lecturer| lecturer.id == schedule.lecturer_id);
        if !allowed {
            return Err(UnifiedError::not_found("Schedule not found"));
        }
        Ok(schedule)
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateSchedule>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Schedule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let mut schedule = state
            .schedules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Schedule not found"))?;
        if let Some(lecturer_id) = &body.lecturer_id {
            if !state.lecturers.contains_key(lecturer_id) {
                return Err(UnifiedError::not_found("Lecturer not found"));
            }
            schedule.lecturer_id = lecturer_id.clone();
        }
        if let Some(room_id) = &body.room_id {
            if !state.rooms.contains_key(room_id) {
                return Err(UnifiedError::not_found("Room not found"));
            }
            schedule.room_id = room_id.clone();
        }
        if let Some(day_of_week) = body.day_of_week {
            schedule.day_of_week = parse_day_of_week(day_of_week)?;
        }
        if let Some(start_time) = &body.start_time {
            schedule.start_time = parse_time(start_time, "start_time")?;
        }
        if let Some(end_time) = &body.end_time {
            schedule.end_time = parse_time(end_time, "end_time")?;
        }
        if let Some(status) = &body.status {
            let status = require_text(status, "status", MAX_CODE)?;
            if status != "active" && status != "inactive" {
                return Err(UnifiedError::bad_request(
                    "status must be \"active\" or \"inactive\"",
                ));
            }
            schedule.status = status;
        }
        let start_minutes = time_to_minutes(&schedule.start_time).unwrap_or(0);
        let end_minutes = time_to_minutes(&schedule.end_time).unwrap_or(0);
        if end_minutes <= start_minutes {
            return Err(UnifiedError::bad_request(
                "end_time must be after start_time",
            ));
        }
        if schedule.status == "active" {
            if let Some(conflict) = state.schedule_conflict(
                &schedule.semester_id,
                &schedule.room_id,
                &schedule.lecturer_id,
                schedule.day_of_week,
                start_minutes,
                end_minutes,
                Some(&schedule.id),
            ) {
                return Err(UnifiedError::conflict(format!(
                    "Schedule conflicts with existing schedule {} (room or lecturer already booked at that time)",
                    conflict.id
                )));
            }
        }
        schedule.updated_at = now();
        state
            .schedules
            .insert(schedule.id.clone(), schedule.clone());
        Ok(schedule)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_schedule(
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
        if !state.schedules.contains_key(&id) {
            return Err(UnifiedError::not_found("Schedule not found"));
        }
        if state
            .attendance
            .iter()
            .any(|entry| entry.value().schedule_id == id)
        {
            return Err(UnifiedError::conflict(
                "Schedule cannot be deleted while attendance records exist",
            ));
        }
        state.schedules.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}
