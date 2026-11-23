// Elearning — Module and lesson handlers.

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
// Module and lesson handlers
// ---------------------------------------------------------------------------

/// Content rules for a lesson type, read from settings: types in
/// `video_lesson_types` need a storage reference, types in
/// `text_lesson_types` need a body, any other type needs neither.
/// The type lists are admin-configured, never hard-coded here, so
/// a renamed or added lesson type follows the settings instead
/// of falling through the old "video"/"file"/"text" match.
pub(crate) fn validate_lesson_content(
    settings: &ElearningSettings,
    lesson_type: &str,
    content_ref: &Option<String>,
    body: &Option<String>,
) -> UnifiedResult<()> {
    if settings
        .video_lesson_types
        .iter()
        .any(|kind| kind == lesson_type)
    {
        if content_ref.is_none() {
            return Err(UnifiedError::bad_request(format!(
                "{lesson_type} lessons require content_ref (a storage reference)"
            )));
        }
    } else if settings
        .text_lesson_types
        .iter()
        .any(|kind| kind == lesson_type)
    {
        if body.as_deref().map(str::is_empty).unwrap_or(true) {
            return Err(UnifiedError::bad_request(format!(
                "{lesson_type} lessons require body"
            )));
        }
    }
    Ok(())
}

pub(crate) async fn list_course_modules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let course = load_course(&state, &path.into_inner())?;
        require_record_in_company_scope(&course.company_id, &_scope)?;
        require_course_visible(&ctx, &caller, &course)?;
        let params = query.into_inner();
        let mut items: Vec<CourseModule> = state
            .modules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.course_id == course.id)
            .collect();
        items.sort_by_key(|row| row.position);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_course_module(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateModuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CourseModule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let course = load_course(&state, &path.into_inner())?;
        require_record_in_company_scope(&course.company_id, &_scope)?;
        require_course_write(&ctx, &caller, &course)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let position = match input.position {
            Some(value) => {
                if value < 0 {
                    return Err(UnifiedError::bad_request("position must be >= 0"));
                }
                value
            }
            None => state
                .modules
                .iter()
                .filter(|entry| entry.value().course_id == course.id)
                .map(|entry| entry.value().position)
                .max()
                .map(|max| max + 1)
                .unwrap_or(0),
        };
        let now = Utc::now();
        let row = CourseModule {
            id: Uuid::now_v7(),
            course_id: course.id,
            title,
            description,
            position,
            created_at: now,
            updated_at: now,
        };
        state.modules.insert(row.id, row.clone());
        state.emit(
            "elearning.module.created",
            json!({ "module_id": row.id.to_string(), "course_id": course.id.to_string(), "title": row.title }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => created(row),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_module(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CourseModule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let row = load_module(&state, &path.into_inner())?;
        let course = load_course(&state, &row.course_id.to_string())?;
        require_record_in_company_scope(&course.company_id, &_scope)?;
        require_course_visible(&ctx, &caller, &course)?;
        Ok(row)
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_module(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateModuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CourseModule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let mut row = load_module(&state, &path.into_inner())?;
        let course = load_course(&state, &row.course_id.to_string())?;
        require_record_in_company_scope(&course.company_id, &_scope)?;
        require_course_write(&ctx, &caller, &course)?;
        let input = body.into_inner();
        if let Some(title) = &input.title {
            row.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &input.description {
            row.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(position) = input.position {
            if position < 0 {
                return Err(UnifiedError::bad_request("position must be >= 0"));
            }
            row.position = position;
        }
        row.updated_at = Utc::now();
        state.modules.insert(row.id, row.clone());
        state.emit(
            "elearning.module.updated",
            json!({ "module_id": row.id.to_string(), "course_id": row.course_id.to_string() }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_module(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let row = load_module(&state, &path.into_inner())?;
        let course = load_course(&state, &row.course_id.to_string())?;
        require_record_in_company_scope(&course.company_id, &_scope)?;
        require_course_write(&ctx, &caller, &course)?;
        let lesson_ids: Vec<Uuid> = state
            .lessons
            .iter()
            .filter(|entry| entry.value().module_id == row.id)
            .map(|entry| *entry.key())
            .collect();
        for lesson_id in lesson_ids {
            delete_lesson_rows(&state, lesson_id);
        }
        state.modules.remove(&row.id);
        state.emit(
            "elearning.module.deleted",
            json!({ "module_id": row.id.to_string(), "course_id": row.course_id.to_string() }),
        );
        Ok(json!({ "deleted": true, "id": row.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_module_lessons(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let row = load_module(&state, &path.into_inner())?;
        let course = load_course(&state, &row.course_id.to_string())?;
        require_record_in_company_scope(&course.company_id, &_scope)?;
        require_course_visible(&ctx, &caller, &course)?;
        let params = query.into_inner();
        let type_filter = params.filters.get("lesson_type").cloned();
        let mut items: Vec<Lesson> = state
            .lessons
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|lesson| {
                if lesson.module_id != row.id {
                    return false;
                }
                if let Some(kind) = &type_filter {
                    if &lesson.lesson_type != kind {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|lesson| lesson.position);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_lesson(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateLessonRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Lesson> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let row = load_module(&state, &path.into_inner())?;
        let course = load_course(&state, &row.course_id.to_string())?;
        require_record_in_company_scope(&course.company_id, &_scope)?;
        require_course_write(&ctx, &caller, &course)?;
        let input = body.into_inner();
        let settings = state.settings();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let lesson_type = require_in_list(&input.lesson_type, "lesson_type", &settings.lesson_types)?;
        // Content rules per type come from settings: video and
        // file lessons need a storage reference, text lessons
        // need a body, quiz lessons need neither (their quiz is
        // defined separately).
        let content_ref = optional_text(&input.content_ref, "content_ref", MAX_REF_LEN)?;
        let body_text = optional_text(&input.body, "body", MAX_TEXT_LEN)?;
        validate_lesson_content(&settings, &lesson_type, &content_ref, &body_text)?;
        if let Some(minutes) = input.duration_minutes {
            if minutes < 0 || minutes > 100_000 {
                return Err(UnifiedError::bad_request(
                    "duration_minutes must be between 0 and 100000",
                ));
            }
        }
        let position = match input.position {
            Some(value) => {
                if value < 0 {
                    return Err(UnifiedError::bad_request("position must be >= 0"));
                }
                value
            }
            None => state
                .lessons
                .iter()
                .filter(|entry| entry.value().module_id == row.id)
                .map(|entry| entry.value().position)
                .max()
                .map(|max| max + 1)
                .unwrap_or(0),
        };
        let now = Utc::now();
        let lesson = Lesson {
            id: Uuid::now_v7(),
            module_id: row.id,
            course_id: course.id,
            title,
            lesson_type,
            content_ref,
            body: body_text,
            position,
            required: input.required.unwrap_or(true),
            available_from: input.available_from,
            requires_previous: input.requires_previous.unwrap_or(false),
            duration_minutes: input.duration_minutes,
            created_at: now,
            updated_at: now,
        };
        state.lessons.insert(lesson.id, lesson.clone());
        state.emit(
            "elearning.lesson.created",
            json!({ "lesson_id": lesson.id.to_string(), "course_id": course.id.to_string(), "lesson_type": lesson.lesson_type }),
        );
        Ok(lesson)
    })();
    match result {
        Ok(lesson) => created(lesson),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_lesson(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Lesson> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let lesson = load_lesson(&state, &path.into_inner())?;
        let course = load_course(&state, &lesson.course_id.to_string())?;
        require_record_in_company_scope(&course.company_id, &_scope)?;
        require_course_visible(&ctx, &caller, &course)?;
        Ok(lesson)
    })();
    match result {
        Ok(lesson) => ok(lesson),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_lesson(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateLessonRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Lesson> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let mut lesson = load_lesson(&state, &path.into_inner())?;
        let course = load_course(&state, &lesson.course_id.to_string())?;
        require_record_in_company_scope(&course.company_id, &_scope)?;
        require_course_write(&ctx, &caller, &course)?;
        let input = body.into_inner();
        let settings = state.settings();
        if let Some(title) = &input.title {
            lesson.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(kind) = &input.lesson_type {
            lesson.lesson_type = require_in_list(kind, "lesson_type", &settings.lesson_types)?;
        }
        if let Some(content_ref) = &input.content_ref {
            lesson.content_ref = match content_ref {
                Some(raw) => Some(require_text(raw, "content_ref", MAX_REF_LEN)?),
                None => None,
            };
        }
        if let Some(body_text) = &input.body {
            lesson.body = match body_text {
                Some(raw) => Some(require_text(raw, "body", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        if let Some(position) = input.position {
            if position < 0 {
                return Err(UnifiedError::bad_request("position must be >= 0"));
            }
            lesson.position = position;
        }
        if let Some(required) = input.required {
            lesson.required = required;
        }
        if let Some(available_from) = &input.available_from {
            lesson.available_from = *available_from;
        }
        if let Some(requires_previous) = input.requires_previous {
            lesson.requires_previous = requires_previous;
        }
        if let Some(duration) = &input.duration_minutes {
            if let Some(minutes) = duration {
                if *minutes < 0 || *minutes > 100_000 {
                    return Err(UnifiedError::bad_request(
                        "duration_minutes must be between 0 and 100000",
                    ));
                }
            }
            lesson.duration_minutes = *duration;
        }
        // Re-check the content rules after the edit.
        validate_lesson_content(&settings, &lesson.lesson_type, &lesson.content_ref, &lesson.body)?;
        lesson.updated_at = Utc::now();
        state.lessons.insert(lesson.id, lesson.clone());
        state.emit(
            "elearning.lesson.updated",
            json!({ "lesson_id": lesson.id.to_string(), "course_id": lesson.course_id.to_string() }),
        );
        Ok(lesson)
    })();
    match result {
        Ok(lesson) => ok(lesson),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_lesson(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let lesson = load_lesson(&state, &path.into_inner())?;
        let course = load_course(&state, &lesson.course_id.to_string())?;
        require_record_in_company_scope(&course.company_id, &_scope)?;
        require_course_write(&ctx, &caller, &course)?;
        delete_lesson_rows(&state, lesson.id);
        state.emit(
            "elearning.lesson.deleted",
            json!({ "lesson_id": lesson.id.to_string(), "course_id": lesson.course_id.to_string() }),
        );
        Ok(json!({ "deleted": true, "id": lesson.id.to_string() }))
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
    fn validate_lesson_content_enforces_default_type_rules() {
        let settings = ElearningSettings::default();
        // Video and file lessons need a storage reference.
        assert!(validate_lesson_content(&settings, "video", &None, &None).is_err());
        assert!(validate_lesson_content(&settings, "file", &None, &None).is_err());
        assert!(validate_lesson_content(
            &settings,
            "video",
            &Some("s3://clip.mp4".to_string()),
            &None
        )
        .is_ok());
        // Text lessons need a body.
        assert!(validate_lesson_content(&settings, "text", &None, &None).is_err());
        assert!(validate_lesson_content(
            &settings,
            "text",
            &None,
            &Some("Lesson body".to_string())
        )
        .is_ok());
        // Quiz lessons need neither.
        assert!(validate_lesson_content(&settings, "quiz", &None, &None).is_ok());
    }

    #[test]
    fn validate_lesson_content_follows_admin_configured_types() {
        let mut settings = ElearningSettings::default();
        settings.video_lesson_types = vec!["webinar".to_string()];
        settings.text_lesson_types = vec!["essay".to_string()];
        // "video" is no longer a video type, so no reference is needed.
        assert!(validate_lesson_content(&settings, "video", &None, &None).is_ok());
        // "text" is no longer a text type, so no body is needed.
        assert!(validate_lesson_content(&settings, "text", &None, &None).is_ok());
        // The admin-configured types enforce their own rules.
        assert!(validate_lesson_content(&settings, "webinar", &None, &None).is_err());
        assert!(validate_lesson_content(&settings, "essay", &None, &None).is_err());
        assert!(validate_lesson_content(
            &settings,
            "essay",
            &None,
            &Some("Essay body".to_string())
        )
        .is_ok());
    }
}
