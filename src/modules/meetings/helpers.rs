//! Small shared helpers.

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
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn actor_id(req: &HttpRequest) -> UnifiedResult<String> {
    Ok(unified_core::caller::require_caller(req)?.user_id)
}

/// The caller's role slug from the `X-Role` header, if supplied.
/// The slug is only an identifier for the roles module to check: it
/// never grants anything on its own. In particular, the
/// `X-User-Roles` header is never read here: a client-supplied role
/// list must not bypass a paid-room entitlement check.
pub(crate) fn caller_role(req: &HttpRequest) -> Option<String> {
    unified_core::caller::caller_from(req).roles.first().cloned()
}

/// Host override capability, verified server-side: the room host
/// always bypasses the entitlement check for paid rooms. Anyone
/// else must hold the `meetings.host` permission according to the
/// roles module, asked through the `role.check` hook with the
/// caller's `X-Role` slug. Fails closed: no hook bus, no role slug,
/// or no grant means no override.
pub(crate) fn has_host_override(
    state: &MeetingsState,
    room: &MeetingRoom,
    user_id: &str,
    role: Option<&str>,
) -> bool {
    if room.host_id == user_id {
        return true;
    }
    let Some(role) = role.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    let Some(bus) = state.bus() else {
        return false;
    };
    let answer = bus.emit(
        "role.check",
        json!({ "role": role, "permission": "meetings.host", "allowed": false }),
    );
    answer
        .get("allowed")
        .and_then(Value::as_bool)
        .or_else(|| {
            answer
                .results
                .first()
                .and_then(|result| result.get("allowed"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(false)
}

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

pub(crate) fn err(error: UnifiedError) -> HttpResponse {
    json::error_response(&error, false)
}

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

pub(crate) fn participant_key(room_id: Uuid, user_id: &str) -> String {
    format!("{room_id}:{user_id}")
}

pub(crate) fn load_room(state: &MeetingsState, raw_id: &str) -> UnifiedResult<MeetingRoom> {
    let id = parse_uuid(raw_id, "room")?;
    state
        .rooms
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Meeting room not found: {id}")))
}

pub(crate) fn load_participant(
    state: &MeetingsState,
    room_id: Uuid,
    user_id: &str,
) -> UnifiedResult<Participant> {
    state
        .participants
        .get(&participant_key(room_id, user_id))
        .map(|entry| entry.value().clone())
        .ok_or_else(|| {
            UnifiedError::not_found(format!("User {user_id} is not a participant of this room"))
        })
}

pub(crate) fn is_host(room: &MeetingRoom, user_id: &str) -> bool {
    room.host_id == user_id
}

pub(crate) fn is_co_host(participant: &Participant) -> bool {
    participant.role == MeetingRole::CoHost && participant.status == "joined"
}

/// The wire name of a role, matching its serde spelling.
pub(crate) fn role_name(role: MeetingRole) -> &'static str {
    match role {
        MeetingRole::Host => "host",
        MeetingRole::CoHost => "co_host",
        MeetingRole::Participant => "participant",
    }
}

pub(crate) fn can_manage_room(room: &MeetingRoom, participant: Option<&Participant>, user_id: &str) -> bool {
    if is_host(room, user_id) {
        return true;
    }
    participant.is_some_and(is_co_host)
}

pub(crate) fn joined_count(state: &MeetingsState, room_id: Uuid) -> usize {
    state
        .participants
        .iter()
        .filter(|entry| entry.value().room_id == room_id && entry.value().status == "joined")
        .count()
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
    use crate::modules::meetings::{
        AccessModel, MeetingRole, MeetingRoom, MeetingsState, Participant, RoomStatus,
    };

    fn test_room(host_id: &str) -> MeetingRoom {
        MeetingRoom {
            id: Uuid::new_v4(),
            title: "Weekly sync".to_string(),
            description: None,
            host_id: host_id.to_string(),
            status: RoomStatus::Scheduled,
            access_model: AccessModel::Free,
            product_id: None,
            max_participants: 10,
            waiting_room_enabled: false,
            join_token: "token".to_string(),
            starts_at: None,
            ends_at: None,
            timezone: "UTC".to_string(),
            recurrence_rule: None,
            scheduler_job_id: None,
            scheduler_status: "not_scheduled".to_string(),
            started_at: None,
            ended_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn test_participant(room_id: Uuid, user_id: &str, role: MeetingRole, status: &str) -> Participant {
        Participant {
            room_id,
            user_id: user_id.to_string(),
            role,
            status: status.to_string(),
            joined_at: None,
            left_at: None,
            last_seen: Utc::now(),
            audio_muted: false,
            video_muted: false,
            sharing_screen: false,
            hand_raised: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn is_host_true_and_false() {
        let room = test_room("host-1");
        assert!(is_host(&room, "host-1"));
        assert!(!is_host(&room, "someone-else"));
    }

    #[test]
    fn is_co_host_requires_joined_co_host() {
        let room_id = Uuid::new_v4();
        assert!(is_co_host(&test_participant(room_id, "u1", MeetingRole::CoHost, "joined")));
        assert!(!is_co_host(&test_participant(room_id, "u1", MeetingRole::CoHost, "waiting")));
        assert!(!is_co_host(&test_participant(room_id, "u1", MeetingRole::Participant, "joined")));
        assert!(!is_co_host(&test_participant(room_id, "u1", MeetingRole::Host, "joined")));
    }

    #[test]
    fn role_name_spellings() {
        assert_eq!(role_name(MeetingRole::Host), "host");
        assert_eq!(role_name(MeetingRole::CoHost), "co_host");
        assert_eq!(role_name(MeetingRole::Participant), "participant");
    }

    #[test]
    fn can_manage_room_host_cohost_and_participant() {
        let room = test_room("host-1");
        assert!(can_manage_room(&room, None, "host-1"));
        let co_host = test_participant(room.id, "u2", MeetingRole::CoHost, "joined");
        assert!(can_manage_room(&room, Some(&co_host), "u2"));
        let waiting_co_host = test_participant(room.id, "u3", MeetingRole::CoHost, "waiting");
        assert!(!can_manage_room(&room, Some(&waiting_co_host), "u3"));
        let plain = test_participant(room.id, "u4", MeetingRole::Participant, "joined");
        assert!(!can_manage_room(&room, Some(&plain), "u4"));
        assert!(!can_manage_room(&room, None, "u5"));
    }

    #[test]
    fn participant_key_format() {
        let room_id = Uuid::new_v4();
        assert_eq!(participant_key(room_id, "user-9"), format!("{room_id}:user-9"));
    }

    #[test]
    fn joined_count_counts_only_joined_in_room() {
        let state = MeetingsState::default();
        let room_a = Uuid::new_v4();
        let room_b = Uuid::new_v4();
        let joined_a = test_participant(room_a, "u1", MeetingRole::Participant, "joined");
        let left_a = test_participant(room_a, "u2", MeetingRole::Participant, "left");
        let joined_b = test_participant(room_b, "u3", MeetingRole::Participant, "joined");
        for p in [joined_a, left_a, joined_b] {
            let key = participant_key(p.room_id, &p.user_id);
            state.participants.insert(key, p);
        }
        assert_eq!(joined_count(&state, room_a), 1);
        assert_eq!(joined_count(&state, room_b), 1);
        assert_eq!(joined_count(&state, Uuid::new_v4()), 0);
    }

    fn state_with_role_check(allowed: bool) -> MeetingsState {
        let state = MeetingsState::default();
        let bus = Arc::new(HookBus::new());
        bus.register(
            "role.check",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                ctx.set("allowed", Value::Bool(allowed));
            }),
        );
        *state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&bus));
        state
    }

    #[test]
    fn caller_role_reads_the_verified_caller_roles() {
        let req = verified_request("user-1", "admin");
        assert_eq!(caller_role(&req).as_deref(), Some("admin"));
        let bare = TestRequest::default().to_http_request();
        assert_eq!(caller_role(&bare), None);
    }

    #[test]
    fn has_host_override_host_bypasses_without_hook() {
        let room = test_room("host-1");
        let state = MeetingsState::default();
        assert!(has_host_override(&state, &room, "host-1", None));
        assert!(!has_host_override(&state, &room, "someone-else", None));
    }

    #[test]
    fn has_host_override_never_trusts_header_role_alone() {
        // A caller claiming role "admin" gets no override when the
        // roles module denies the permission: the header alone is
        // not proof of anything.
        let room = test_room("host-1");
        let denying = state_with_role_check(false);
        assert!(!has_host_override(&denying, &room, "intruder", Some("admin")));
        assert!(!has_host_override(&denying, &room, "intruder", Some("meetings.host")));
        // With no hook bus at all there is no override either.
        let no_bus = MeetingsState::default();
        assert!(!has_host_override(&no_bus, &room, "intruder", Some("admin")));
    }

    #[test]
    fn has_host_override_granted_only_by_role_check_hook() {
        let room = test_room("host-1");
        let granting = state_with_role_check(true);
        assert!(has_host_override(&granting, &room, "helper", Some("admin")));
        assert!(!has_host_override(&granting, &room, "helper", None));
    }
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 4, group C).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// Meeting (rooms, participants, messages, recordings) records carry no company field, so the guard is a
/// fail-closed gate only: real per-company partitioning needs a
/// `tenant_id` on the records (C11 follow-up), and no per-record
/// company check is applied here.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::HttpMessage;
    use actix_web::test::TestRequest;

    /// Request carrying the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach.
    fn tenant_request() -> HttpRequest {
        let req = TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "acme".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request();
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
    }
}
