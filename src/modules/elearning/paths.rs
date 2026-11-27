// Elearning - Learning path handlers.

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

// Learning path handlers

pub(crate) async fn list_paths(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let params = query.into_inner();
        let search = params.search.clone().map(|value| value.to_lowercase());
        let mut items: Vec<LearningPath> = state
            .paths
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if !path_visible(&state, row.id, &_scope) {
                    return false;
                }
                if let Some(needle) = &search {
                    let haystack = format!(
                        "{} {}",
                        row.title,
                        row.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(needle) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|row| row.title.clone());
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_path(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    body: web::Json<CreatePathRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LearningPath> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let now = Utc::now();
        let row = LearningPath {
            id: Uuid::now_v7(),
            title,
            description,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.paths.insert(row.id, row.clone());
        state.emit(
            "elearning.path.created",
            json!({ "path_id": row.id.to_string(), "title": row.title }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => created(row),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_path(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let row = load_path(&state, &path.into_inner())?;
        let mut courses: Vec<LearningPathCourse> = state
            .path_courses
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| item.path_id == row.id)
            .collect();
        courses.sort_by_key(|item| item.position);
        courses.retain(|item| course_in_scope(&state, item.course_id, &_scope));
        Ok(json!({ "path": row, "courses": courses }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_path(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePathRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LearningPath> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let mut row = load_path(&state, &path.into_inner())?;
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
        row.updated_at = Utc::now();
        state.paths.insert(row.id, row.clone());
        state.emit(
            "elearning.path.updated",
            json!({ "path_id": row.id.to_string(), "title": row.title }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_path(
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
        require_teach(&ctx, &caller.role)?;
        let row = load_path(&state, &path.into_inner())?;
        state.path_courses.retain(|_, item| item.path_id != row.id);
        state.paths.remove(&row.id);
        state.emit(
            "elearning.path.deleted",
            json!({ "path_id": row.id.to_string(), "title": row.title }),
        );
        Ok(json!({ "deleted": true, "id": row.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn add_path_course(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddPathCourseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LearningPathCourse> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let row = load_path(&state, &path.into_inner())?;
        let input = body.into_inner();
        let course = load_course(&state, &input.course_id)?;
        require_record_in_company_scope(&course.company_id, &_scope)?;

        if state.path_courses.iter().any(|entry| {
            entry.value().path_id == row.id && entry.value().course_id == course.id
        }) {
            return Err(UnifiedError::conflict(
                "Course is already in this learning path",
            ));
        }
        let position = match input.position {
            Some(value) => {
                if value < 0 {
                    return Err(UnifiedError::bad_request("position must be >= 0"));
                }
                value
            }
            None => state
                .path_courses
                .iter()
                .filter(|entry| entry.value().path_id == row.id)
                .map(|entry| entry.value().position)
                .max()
                .map(|max| max + 1)
                .unwrap_or(0),
        };
        let item = LearningPathCourse {
            id: Uuid::now_v7(),
            path_id: row.id,
            course_id: course.id,
            position,
            created_at: Utc::now(),
        };
        state.path_courses.insert(item.id, item.clone());
        state.emit(
            "elearning.path.course.added",
            json!({
                "path_id": row.id.to_string(),
                "course_id": course.id.to_string(),
                "position": position,
            }),
        );
        Ok(item)
    })();
    match result {
        Ok(item) => created(item),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn remove_path_course(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let (path_id, course_id) = path.into_inner();
        let row = load_path(&state, &path_id)?;
        let course = load_course(&state, &course_id)?;
        require_record_in_company_scope(&course.company_id, &_scope)?;

        let Some(item) = state
            .path_courses
            .iter()
            .find(|entry| entry.value().path_id == row.id && entry.value().course_id == course.id)
            .map(|entry| entry.value().clone())
        else {
            return Err(UnifiedError::not_found(
                "Course is not in this learning path",
            ));
        };
        state.path_courses.remove(&item.id);
        state.emit(
            "elearning.path.course.removed",
            json!({
                "path_id": row.id.to_string(),
                "course_id": course.id.to_string(),
            }),
        );
        Ok(json!({ "deleted": true, "id": item.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) fn compute_path_progress(state: &ElearningState, path: &LearningPath, user_id: &str) -> PathProgress {
    let mut items: Vec<LearningPathCourse> = state
        .path_courses
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| item.path_id == path.id)
        .collect();
    items.sort_by_key(|item| item.position);
    let total = items.len();
    let mut completed_count = 0usize;
    let mut courses: Vec<PathCourseProgress> = Vec::with_capacity(total);
    for item in items {
        let enrollment = find_enrollment(state, item.course_id, user_id);
        let (enrolled, completed, percent) = match enrollment {
            Some(row) => {
                let progress = course_progress(state, &row);
                (true, progress.completed, progress.progress_percent)
            }
            None => (false, false, 0),
        };
        if completed {
            completed_count += 1;
        }
        courses.push(PathCourseProgress {
            course_id: item.course_id,
            position: item.position,
            enrolled,
            completed,
            progress_percent: percent,
        });
    }
    // An empty path has no progress: 0 percent, not complete.
    // (A course with no required lessons reports 100 percent on
    // enrollment, but a path with no courses has nothing to
    // complete, so it must not report itself finished.)
    let percent = if total == 0 {
        0
    } else {
        ((completed_count as i64 * 100) / total as i64).clamp(0, 100)
    };
    PathProgress {
        path_id: path.id,
        user_id: user_id.to_string(),
        total_courses: total,
        completed_courses: completed_count,
        progress_percent: percent,
        completed: total > 0 && completed_count == total,
        courses,
    }
}

pub(crate) async fn get_path_progress(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let row = load_path(&state, &path.into_inner())?;
        let progress = compute_path_progress(&state, &row, &caller.actor);
        // Reading progress never emits completion: completion is
        // announced by the course-completion flow, not by a read.
        Ok(json!(progress))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_my_paths(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let params = query.into_inner();
        let enrolled_course_ids: Vec<Uuid> = state
            .enrollments
            .iter()
            .filter(|entry| {
                entry.value().user_id == caller.actor
                    && entry.value().status != ENROLL_CANCELLED
                    && course_in_scope(&state, entry.value().course_id, &_scope)
            })
            .map(|entry| entry.value().course_id)
            .collect();
        let mut items: Vec<PathProgress> = state
            .paths
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|path| {
                state.path_courses.iter().any(|item| {
                    item.value().path_id == path.id
                        && enrolled_course_ids.contains(&item.value().course_id)
                })
            })
            .map(|path| compute_path_progress(&state, &path, &caller.actor))
            .collect();
        items.sort_by_key(|row| row.path_id);
        Ok(paginate(items, &params).to_json())
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
    fn empty_path_progress_is_zero_and_incomplete() {
        let state = ElearningState::default();
        let now = Utc::now();
        let path = LearningPath {
            id: Uuid::now_v7(),
            title: "Empty path".to_string(),
            description: None,
            created_by: "admin".to_string(),
            created_at: now,
            updated_at: now,
        };
        let progress = compute_path_progress(&state, &path, "learner-1");
        assert_eq!(progress.total_courses, 0);
        assert_eq!(progress.completed_courses, 0);
        assert_eq!(progress.progress_percent, 0);
        assert!(!progress.completed);
        assert!(progress.courses.is_empty());
    }
}
