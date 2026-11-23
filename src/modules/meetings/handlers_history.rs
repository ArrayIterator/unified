//! HTTP handlers: attendance and history.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::storage::StorageProvider;
use crate::http::websocket::global_hub;
use actix_web::{web, HttpRequest, HttpResponse};
use actix_ws::Message;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Attendance and history
// ---------------------------------------------------------------------------

fn attendance_view(participant: &Participant) -> AttendanceView {
    let duration_secs = match (participant.joined_at, participant.left_at) {
        (Some(joined), Some(left)) => Some(left.signed_duration_since(joined).num_seconds()),
        (Some(joined), None) if participant.status == "joined" => {
            Some(Utc::now().signed_duration_since(joined).num_seconds())
        }
        _ => None,
    };
    AttendanceView {
        user_id: participant.user_id.clone(),
        role: participant.role,
        status: participant.status.clone(),
        joined_at: participant.joined_at,
        left_at: participant.left_at,
        duration_secs,
    }
}

pub(crate) async fn list_attendance(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<AttendanceView>> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let room = load_room(&state, &path.into_inner())?;
        let participant = load_participant(&state, room.id, &actor)?;
        if participant.status != "joined" && !is_host(&room, &actor) {
            return Err(UnifiedError::forbidden(
                "Only a joined participant can read this room's attendance",
            ));
        }
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<AttendanceView> = state
            .participants
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.room_id == room.id)
            .filter(|entry| {
                if let Some(user_id) = &user_filter {
                    if &entry.user_id != user_id {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if &entry.status != status {
                        return false;
                    }
                }
                true
            })
            .map(|entry| attendance_view(&entry))
            .collect();
        items.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

pub(crate) async fn list_history(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
    range: web::Query<HistoryQuery>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<MeetingRoom>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _actor = actor_id(&req)?;
        let params = query.into_inner();
        let range = range.into_inner();
        let host_filter = params.filters.get("host_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<MeetingRoom> = state
            .rooms
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|room| {
                room.status == RoomStatus::Ended || room.status == RoomStatus::Cancelled
            })
            .filter(|room| {
                if let Some(status) = &status_filter {
                    let expected = match status.as_str() {
                        "ended" => RoomStatus::Ended,
                        "cancelled" => RoomStatus::Cancelled,
                        _ => return false,
                    };
                    if room.status != expected {
                        return false;
                    }
                }
                if let Some(host_id) = &host_filter {
                    if &room.host_id != host_id {
                        return false;
                    }
                }
                if let Some(from) = range.from {
                    if room.created_at < from {
                        return false;
                    }
                }
                if let Some(to) = range.to {
                    if room.created_at > to {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        room.title,
                        room.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| {
            b.started_at
                .cmp(&a.started_at)
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    fn seed_ended_room(state: &MeetingsState, token: &str) -> MeetingRoom {
        let now = Utc::now();
        let room = MeetingRoom {
            id: Uuid::now_v7(),
            title: "Ended room".to_string(),
            description: None,
            host_id: "host-1".to_string(),
            status: RoomStatus::Ended,
            access_model: AccessModel::Free,
            product_id: None,
            max_participants: 10,
            waiting_room_enabled: false,
            join_token: token.to_string(),
            starts_at: Some(now - chrono::Duration::hours(2)),
            ends_at: None,
            timezone: "UTC".to_string(),
            recurrence_rule: None,
            scheduler_job_id: None,
            scheduler_status: "not_scheduled".to_string(),
            started_at: Some(now - chrono::Duration::hours(2)),
            ended_at: Some(now - chrono::Duration::hours(1)),
            created_at: now,
            updated_at: now,
        };
        state.rooms.insert(room.id, room.clone());
        room
    }

    fn history_query() -> (
        web::Query<PaginationParams>,
        web::Query<HistoryQuery>,
    ) {
        (
            web::Query(PaginationParams::default()),
            web::Query(HistoryQuery { from: None, to: None }),
        )
    }

    #[tokio::test]
    async fn list_history_requires_actor() {
        let state = web::Data::new(MeetingsState::default());
        seed_ended_room(&state, "secret-history-token");
        let (query, range) = history_query();
        let denied = list_history(
            state.clone(),
            TestRequest::default().to_http_request(),
            query,
            range,
        )
        .await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_history_hides_join_tokens() {
        // History returns ended/cancelled rooms, but the join
        // token must never appear in the response body: the
        // token is skipped at serialization (see types.rs), and
        // this checks the whole handler response, not just the
        // struct.
        let state = web::Data::new(MeetingsState::default());
        seed_ended_room(&state, "secret-history-token-123");
        let (query, range) = history_query();
        let resp = list_history(
            state.clone(),
            verified_request_without_roles("someone"),
            query,
            range,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = String::from_utf8(
            actix_web::body::to_bytes(resp.into_body())
                .await
                .expect("response body reads")
                .to_vec(),
        )
        .expect("response body is UTF-8");
        assert!(
            body.contains("Ended room"),
            "history lists the ended room: {body}"
        );
        assert!(
            !body.contains("secret-history-token-123"),
            "history response must not leak the join token: {body}"
        );
        assert!(
            !body.contains("join_token"),
            "history response must not carry a join_token field at all: {body}"
        );
    }
}
