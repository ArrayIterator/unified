//! Activity Timeline module. — Handlers.
//!
//! Split from `src/modules/timeline.rs`; behaviour unchanged.
//!
//! Every read route requires an authenticated caller holding the
//! `timeline.view` permission (or `audit.view`, which covers the
//! same audit data): anonymous callers get 401, authenticated
//! callers without either permission get 403. Entries are redacted
//! before they are served, so a list response can only ever carry
//! the whitelisted summary, never a raw event payload or an IP.

use super::helpers::*;
use super::module::*;
use super::state::*;
use super::types::*;

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;

pub(super) async fn list_feed(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TimelineState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match authorize_timeline_view(&ctx, &req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, debug),
    };
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<TimelineEntry>> =
        apply_filters(state.all_entries(), &params).map(|items| {
            let items: Vec<TimelineEntry> =
                items.into_iter().map(redact_entry).collect();
            paginate(items, &params)
        });
    match result {
        Ok(page) => {
            // Low-priority analytics signal: the feed was viewed.
            // The actor is the authenticated caller, not the `actor`
            // query filter, so it cannot be spoofed.
            ctx.hooks.emit(
                "timeline.viewed",
                json!({
                    "actor": caller.actor,
                    "total": page.pagination.total,
                    "viewed_at": Utc::now().to_rfc3339(),
                }),
            );
            json::success_response(page, 200)
        }
        Err(error) => json::error_response(&error, debug),
    }
}

pub(super) async fn entity_timeline(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TimelineState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize_timeline_view(&ctx, &req) {
        return json::error_response(&error, debug);
    }
    let (entity_type, entity_id) = path.into_inner();
    let params = query.into_inner();
    let mut items: Vec<TimelineEntry> = state
        .all_entries()
        .into_iter()
        .filter(|entry| entry.entity_type == entity_type && entry.entity_id == entity_id)
        .map(redact_entry)
        .collect();
    items.sort_by(|a, b| b.seq.cmp(&a.seq));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(super) async fn counts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TimelineState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize_timeline_view(&ctx, &req) {
        return json::error_response(&error, debug);
    }
    let entries = state.all_entries();
    json::success_response(
        TimelineCounts {
            total: entries.len() as u64,
            by_action: count_by(&entries, |entry| entry.action.clone()),
            by_entity_type: count_by(&entries, |entry| entry.entity_type.clone()),
            by_day: count_by(&entries, |entry| {
                entry.created_at.format("%Y-%m-%d").to_string()
            }),
        },
        200,
    )
}

pub(super) async fn by_day(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TimelineState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize_timeline_view(&ctx, &req) {
        return json::error_response(&error, debug);
    }
    let params = query.into_inner();
    let entries = state.all_entries();
    let mut groups: Vec<DayGroup> = count_by(&entries, |entry| {
        entry.created_at.format("%Y-%m-%d").to_string()
    })
    .into_iter()
    .map(|count| DayGroup {
        date: count.key,
        count: count.count,
    })
    .collect();
    // Newest day first.
    groups.sort_by(|a, b| b.date.cmp(&a.date));
    let page = paginate(groups, &params);
    json::success_response(page, 200)
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
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    /// A context whose `role.check` handler allows every permission.
    fn allowing_context() -> AppContext {
        let ctx = test_context();
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                context.set("allowed", serde_json::Value::Bool(true));
            }),
        );
        ctx
    }

    fn authed_request() -> HttpRequest {
        verified_request("user-1", "admin")
    }

    fn anonymous_request() -> HttpRequest {
        TestRequest::default().to_http_request()
    }

    fn ctx_data(ctx: AppContext) -> web::Data<Arc<AppContext>> {
        web::Data::new(Arc::new(ctx))
    }

    fn state_data(state: Arc<TimelineState>) -> web::Data<Arc<TimelineState>> {
        web::Data::new(state)
    }

    fn seeded_state() -> Arc<TimelineState> {
        let state = Arc::new(TimelineState::new());
        state.record(
            "user.created",
            &json!({
                "actor_id": "alice",
                "summary": "User alice created",
                "entity_type": "user",
                "entity_id": "u-9",
                "ip": "10.0.0.9",
                "password": "s3cr3t",
            }),
        );
        state
    }

    async fn body_json(response: HttpResponse) -> serde_json::Value {
        let bytes = actix_web::body::to_bytes(response.into_body())
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn list_feed_rejects_anonymous_callers() {
        let response = list_feed(
            ctx_data(test_context()),
            state_data(seeded_state()),
            anonymous_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_feed_rejects_callers_without_view_permission() {
        // Headers present, but no `role.check` handler answers:
        // the guard fails closed.
        let response = list_feed(
            ctx_data(test_context()),
            state_data(seeded_state()),
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn list_feed_accepts_callers_with_view_permission() {
        let response = list_feed(
            ctx_data(allowing_context()),
            state_data(seeded_state()),
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_feed_body_carries_no_raw_payload_or_ip() {
        let response = list_feed(
            ctx_data(allowing_context()),
            state_data(seeded_state()),
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        let rendered = body.to_string();
        assert!(!rendered.contains("s3cr3t"), "raw payload leaked: {rendered}");
        assert!(!rendered.contains("10.0.0.9"), "IP leaked: {rendered}");
        let items = body["data"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        let details = items[0]["details"].as_object().unwrap();
        assert_eq!(details.len(), 4);
        assert_eq!(details["actor"], "alice");
        assert_eq!(details["entity_id"], "u-9");
        assert!(items[0]["ip"].is_null());
    }

    #[tokio::test]
    async fn entity_timeline_rejects_anonymous_callers() {
        let response = entity_timeline(
            ctx_data(test_context()),
            state_data(seeded_state()),
            anonymous_request(),
            web::Path::from(("user".to_string(), "u-9".to_string())),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn entity_timeline_rejects_callers_without_view_permission() {
        let response = entity_timeline(
            ctx_data(test_context()),
            state_data(seeded_state()),
            authed_request(),
            web::Path::from(("user".to_string(), "u-9".to_string())),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn entity_timeline_accepts_callers_with_view_permission() {
        let response = entity_timeline(
            ctx_data(allowing_context()),
            state_data(seeded_state()),
            authed_request(),
            web::Path::from(("user".to_string(), "u-9".to_string())),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["data"]["items"].as_array().unwrap().len(), 1);
        assert!(!body.to_string().contains("s3cr3t"));
    }

    #[tokio::test]
    async fn counts_rejects_anonymous_callers() {
        let response = counts(
            ctx_data(test_context()),
            state_data(seeded_state()),
            anonymous_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn counts_rejects_callers_without_view_permission() {
        let response = counts(
            ctx_data(test_context()),
            state_data(seeded_state()),
            authed_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn counts_accepts_callers_with_view_permission() {
        let response = counts(
            ctx_data(allowing_context()),
            state_data(seeded_state()),
            authed_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["data"]["total"], 1);
    }

    #[tokio::test]
    async fn by_day_rejects_anonymous_callers() {
        let response = by_day(
            ctx_data(test_context()),
            state_data(seeded_state()),
            anonymous_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn by_day_rejects_callers_without_view_permission() {
        let response = by_day(
            ctx_data(test_context()),
            state_data(seeded_state()),
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn by_day_accepts_callers_with_view_permission() {
        let response = by_day(
            ctx_data(allowing_context()),
            state_data(seeded_state()),
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }
}
