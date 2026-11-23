// records — HTTP handlers.

use super::service::{
    caller, parse_record_filters, record_matches, require_permission, retention_configured,
};
use super::types::{PERM_RECORDS_MANAGE, PERM_RECORDS_READ};
use super::*;
use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::json::{error_response, success_response};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::Value;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Handlers (read-only except the retention purge).
// ---------------------------------------------------------------------------

pub(crate) async fn list_records(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<RecordsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_RECORDS_READ) {
        return error_response(&error, debug);
    }
    let params = query.into_inner();
    let pagination = params.normalized();
    let descending = pagination.order == "desc";
    let filters = match parse_record_filters(&params) {
        Ok(filters) => filters,
        Err(error) => return error_response(&error, debug),
    };

    let mut items: Vec<SystemRecord> = state
        .records
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| record_matches(record, &filters))
        .collect();
    items.sort_by(|a, b| b.seq.cmp(&a.seq));
    if !descending {
        items.reverse();
    }
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<SystemRecord> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    success_response(
        PageResult::new(page_items, total, &pagination).to_json(),
        200,
    )
}

pub(crate) async fn get_record(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<RecordsState>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_RECORDS_READ) {
        return error_response(&error, debug);
    }
    let record_id = id.into_inner();
    match state.records.get(record_id.as_str()) {
        Some(record) => success_response(record.value().clone(), 200),
        None => error_response(
            &UnifiedError::not_found(format!("Record {record_id} not found")),
            debug,
        ),
    }
}

pub(crate) async fn counts_by_entity(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<RecordsState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_RECORDS_READ) {
        return error_response(&error, debug);
    }
    let records: Vec<SystemRecord> = state
        .records
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    success_response(count_by(&records, |record| record.entity_type.as_str()), 200)
}

pub(crate) async fn counts_by_action(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<RecordsState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_RECORDS_READ) {
        return error_response(&error, debug);
    }
    let records: Vec<SystemRecord> = state
        .records
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    success_response(count_by(&records, |record| record.action.as_str()), 200)
}

pub(crate) async fn export_records(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<RecordsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_RECORDS_READ) {
        return error_response(&error, debug);
    }
    let params = query.into_inner();
    // Reuse the list filters (parsed by the same helper the list
    // handler uses, so from/to/search apply here too); export is
    // capped at 10 000 records so a single download cannot exhaust
    // memory.
    let filters = match parse_record_filters(&params) {
        Ok(filters) => filters,
        Err(error) => return error_response(&error, debug),
    };
    let mut items: Vec<SystemRecord> = state
        .records
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| record_matches(record, &filters))
        .collect();
    items.sort_by(|a, b| b.seq.cmp(&a.seq));
    items.truncate(10_000);
    let filename = format!("records-{}.json", Utc::now().format("%Y%m%dT%H%M%SZ"));
    let body = unified_core::json::success(serde_json::json!({
        "exported_at": Utc::now(),
        "count": items.len(),
        "records": items,
    }));
    match actix_web::http::StatusCode::from_u16(200) {
        Ok(status) => HttpResponse::build(status)
            .insert_header((
                "Content-Disposition",
                format!("attachment; filename=\"{filename}\""),
            ))
            .insert_header(("Content-Type", "application/json"))
            .json(body),
        Err(_) => error_response(&UnifiedError::internal("Export failed"), debug),
    }
}

pub(crate) async fn retention_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<RecordsState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_RECORDS_READ) {
        return error_response(&error, debug);
    }
    let oldest = state
        .records
        .iter()
        .map(|entry| entry.value().created_at)
        .min();
    success_response(
        RetentionView {
            retention_days: retention_days(&ctx),
            configured: retention_configured(&ctx),
            total_records: state.records.len() as u64,
            oldest_record_at: oldest,
        },
        200,
    )
}

pub(crate) async fn purge_records(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<RecordsState>>,
    req: HttpRequest,
    body: web::Json<PurgeInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_RECORDS_MANAGE) {
        return error_response(&error, debug);
    }
    let input = body.into_inner();
    if !input.confirm {
        return error_response(
            &UnifiedError::unprocessable(
                "Retention purge requires {\"confirm\": true}",
            )
            .with_metadata("confirm", Value::Bool(false)),
            debug,
        );
    }
    let keep_days = input.keep_days.unwrap_or_else(|| retention_days(&ctx));
    if keep_days == 0 {
        return error_response(
            &UnifiedError::bad_request("keep_days must be at least 1"),
            debug,
        );
    }
    let cutoff = Utc::now() - chrono::Duration::days(i64::from(keep_days));
    let mut purged: u64 = 0;
    state
        .records
        .retain(|_, record| {
            if record.created_at < cutoff {
                purged += 1;
                false
            } else {
                true
            }
        });
    success_response(
        serde_json::json!({
            "purged": purged,
            "keep_days": keep_days,
            "cutoff": cutoff,
        }),
        200,
    )
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
    use chrono::{DateTime, Duration};
    use std::collections::HashMap;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn context_with_retention(days: u32) -> AppContext {
        let mut config = AppConfig::default();
        config.records.retention_days = days;
        AppContext::new(
            config,
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    /// A context whose `role.check` handler allows every permission.
    fn allowing(ctx: AppContext) -> AppContext {
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

    fn data(ctx: AppContext) -> web::Data<Arc<AppContext>> {
        web::Data::new(Arc::new(ctx))
    }

    fn seed(
        state: &Arc<RecordsState>,
        id: &str,
        actor: &str,
        summary: &str,
        created_at: DateTime<Utc>,
    ) {
        state.records.insert(
            id.to_string(),
            SystemRecord {
                id: id.to_string(),
                seq: 1,
                actor: actor.to_string(),
                action: "task.created".to_string(),
                entity_type: "task".to_string(),
                entity_id: "e1".to_string(),
                summary: summary.to_string(),
                details: Value::Null,
                ip: None,
                created_at,
            },
        );
    }

    fn params_with(filters: &[(&str, &str)], search: Option<&str>) -> PaginationParams {
        PaginationParams {
            search: search.map(str::to_string),
            filters: filters
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect::<HashMap<String, String>>(),
            ..Default::default()
        }
    }

    async fn body_json(response: HttpResponse) -> Value {
        let bytes = actix_web::body::to_bytes(response.into_body())
            .await
            .expect("response body");
        serde_json::from_slice(&bytes).expect("response body is JSON")
    }

    #[tokio::test]
    async fn list_records_rejects_anonymous_callers() {
        let response = list_records(
            data(test_context()),
            web::Data::new(Arc::new(RecordsState::new())),
            anonymous_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_records_rejects_callers_without_records_read() {
        // Headers present, but no `role.check` handler answers:
        // the guard fails closed.
        let response = list_records(
            data(test_context()),
            web::Data::new(Arc::new(RecordsState::new())),
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn list_records_accepts_callers_with_records_read() {
        let state = Arc::new(RecordsState::new());
        seed(
            &state,
            "r1",
            "alice",
            "alpha",
            "2026-03-15T00:00:00Z".parse().unwrap(),
        );
        let response = list_records(
            data(allowing(test_context())),
            web::Data::new(state),
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_record_rejects_anonymous_callers() {
        let response = get_record(
            data(test_context()),
            web::Data::new(Arc::new(RecordsState::new())),
            anonymous_request(),
            web::Path::from("r1".to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn counts_by_entity_rejects_callers_without_records_read() {
        let response = counts_by_entity(
            data(test_context()),
            web::Data::new(Arc::new(RecordsState::new())),
            authed_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn counts_by_action_rejects_anonymous_callers() {
        let response = counts_by_action(
            data(test_context()),
            web::Data::new(Arc::new(RecordsState::new())),
            anonymous_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn export_records_rejects_anonymous_callers() {
        let response = export_records(
            data(test_context()),
            web::Data::new(Arc::new(RecordsState::new())),
            anonymous_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn retention_view_rejects_callers_without_records_read() {
        let response = retention_view(
            data(test_context()),
            web::Data::new(Arc::new(RecordsState::new())),
            authed_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn retention_view_reports_configured_false_when_retention_unset() {
        let response = retention_view(
            data(allowing(context_with_retention(0))),
            web::Data::new(Arc::new(RecordsState::new())),
            authed_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["data"]["configured"], Value::Bool(false));
        assert_eq!(body["data"]["retention_days"].as_u64(), Some(365));
    }

    #[tokio::test]
    async fn retention_view_reports_configured_true_for_default_config() {
        let response = retention_view(
            data(allowing(test_context())),
            web::Data::new(Arc::new(RecordsState::new())),
            authed_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["data"]["configured"], Value::Bool(true));
        assert_eq!(body["data"]["retention_days"].as_u64(), Some(365));
    }

    #[tokio::test]
    async fn purge_records_rejects_anonymous_callers() {
        let state = Arc::new(RecordsState::new());
        seed(&state, "r1", "alice", "old", Utc::now());
        let response = purge_records(
            data(test_context()),
            web::Data::new(state.clone()),
            anonymous_request(),
            web::Json(PurgeInput {
                confirm: true,
                keep_days: Some(1),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(state.records.len(), 1);
    }

    #[tokio::test]
    async fn purge_records_rejects_callers_without_records_manage() {
        let state = Arc::new(RecordsState::new());
        seed(&state, "r1", "alice", "old", Utc::now());
        let response = purge_records(
            data(test_context()),
            web::Data::new(state.clone()),
            authed_request(),
            web::Json(PurgeInput {
                confirm: true,
                keep_days: Some(1),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(state.records.len(), 1);
    }

    #[tokio::test]
    async fn purge_records_with_manage_purges_only_old_records() {
        let state = Arc::new(RecordsState::new());
        seed(
            &state,
            "old",
            "alice",
            "old record",
            Utc::now() - Duration::days(400),
        );
        seed(&state, "recent", "alice", "recent record", Utc::now());
        let response = purge_records(
            data(allowing(test_context())),
            web::Data::new(state.clone()),
            authed_request(),
            web::Json(PurgeInput {
                confirm: true,
                keep_days: Some(30),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["data"]["purged"].as_u64(), Some(1));
        assert!(state.records.get("old").is_none());
        assert!(state.records.get("recent").is_some());
    }

    #[tokio::test]
    async fn export_records_applies_from_and_search_filters() {
        let state = Arc::new(RecordsState::new());
        seed(
            &state,
            "a",
            "alice",
            "alpha report",
            "2026-01-01T00:00:00Z".parse().unwrap(),
        );
        seed(
            &state,
            "b",
            "alice",
            "beta report",
            "2026-06-01T00:00:00Z".parse().unwrap(),
        );
        seed(
            &state,
            "c",
            "bob",
            "alpha summary",
            "2026-06-01T00:00:00Z".parse().unwrap(),
        );
        let response = export_records(
            data(allowing(test_context())),
            web::Data::new(state),
            authed_request(),
            web::Query(params_with(
                &[("from", "2026-03-01T00:00:00Z")],
                Some("alpha"),
            )),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["data"]["count"].as_u64(), Some(1));
        assert_eq!(body["data"]["records"][0]["id"], Value::from("c"));
    }

    #[tokio::test]
    async fn export_records_rejects_malformed_from() {
        let response = export_records(
            data(allowing(test_context())),
            web::Data::new(Arc::new(RecordsState::new())),
            authed_request(),
            web::Query(params_with(&[("from", "not-a-date")], None)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
