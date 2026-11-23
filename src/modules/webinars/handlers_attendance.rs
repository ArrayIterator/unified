// webinars -- Attendance handlers.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use super::entitlement::*;
use super::helpers::*;
use super::state::*;
use super::types::*;

// ---------------------------------------------------------------------------
// Attendance handlers
// ---------------------------------------------------------------------------

pub(crate) async fn join_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarAttendance> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let webinar = load_webinar(&state, &path.into_inner())?;
        if webinar.status != WebinarStatus::Live {
            return Err(UnifiedError::unprocessable(
                "The webinar is not live; only live webinars can be joined",
            ));
        }
        let staff = is_staff(&webinar, &actor, &roles);
        if !staff {
            let registration = find_registration(&state, webinar.id, &actor).ok_or_else(|| {
                UnifiedError::forbidden("Only registered attendees can join this webinar")
            })?;
            if registration.status != "registered" {
                return Err(UnifiedError::forbidden(format!(
                    "Registration status {} cannot join this webinar",
                    registration.status
                )));
            }
            check_entitlement(&state, &webinar, &actor, &roles)?;
        }
        // Re-joining while an open session exists returns that
        // session instead of opening a second one.
        if let Some(session) = open_session(&state, webinar.id, &actor) {
            return Ok(session);
        }
        let now = Utc::now();
        let session = WebinarAttendance {
            id: Uuid::now_v7(),
            webinar_id: webinar.id,
            user_id: actor.clone(),
            joined_at: now,
            left_at: None,
            created_at: now,
            updated_at: now,
        };
        state.attendance.insert(session.id, session.clone());
        state.emit(
            "webinar.attendee.joined",
            json!({
                "webinar_id": webinar.id.to_string(),
                "session_id": session.id.to_string(),
                "user_id": actor,
                "joined_at": now.to_rfc3339(),
                "meeting_room_id": webinar.meeting_room_id.map(|id| id.to_string()),
            }),
        );
        Ok(session)
    })();
    match result {
        Ok(session) => created(session),
        Err(error) => err(error),
    }
}

pub(crate) async fn leave_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarAttendance> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let webinar = load_webinar(&state, &path.into_inner())?;
        let session = open_session(&state, webinar.id, &actor).ok_or_else(|| {
            UnifiedError::not_found(format!(
                "User {actor} has no open attendance session on this webinar"
            ))
        })?;
        let now = Utc::now();
        let mut closed = session.clone();
        closed.left_at = Some(now);
        closed.updated_at = now;
        state.attendance.insert(closed.id, closed.clone());
        state.emit(
            "webinar.attendee.left",
            json!({
                "webinar_id": webinar.id.to_string(),
                "session_id": closed.id.to_string(),
                "user_id": actor,
                "joined_at": closed.joined_at.to_rfc3339(),
                "left_at": now.to_rfc3339(),
                "duration_secs": (now - closed.joined_at).num_seconds(),
            }),
        );
        Ok(closed)
    })();
    match result {
        Ok(session) => ok(session),
        Err(error) => err(error),
    }
}

pub(crate) async fn list_attendance(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<AttendanceView>> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let params = query.into_inner();
        let webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can list attendance",
            ));
        }
        let user_filter = params.filters.get("user_id").cloned();
        let now = Utc::now();
        let mut items: Vec<AttendanceView> = state
            .attendance
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if row.webinar_id != webinar.id {
                    return false;
                }
                if let Some(user_id) = &user_filter {
                    if &row.user_id != user_id {
                        return false;
                    }
                }
                true
            })
            .map(|row| {
                let end = row.left_at.unwrap_or(now);
                AttendanceView {
                    id: row.id,
                    user_id: row.user_id,
                    joined_at: row.joined_at,
                    left_at: row.left_at,
                    duration_secs: (end - row.joined_at).num_seconds(),
                }
            })
            .collect();
        let sort = params.allowed_sort(&["joined_at", "duration_secs"], "joined_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "duration_secs" => a.duration_secs.cmp(&b.duration_secs),
                _ => a.joined_at.cmp(&b.joined_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}
