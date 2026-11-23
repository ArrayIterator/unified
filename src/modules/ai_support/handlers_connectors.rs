// Connector handlers.
//
// Authorization (see helpers.rs): every operation requires the
// `ai-support.connectors.manage` capability via the `role.check`
// hook (fail closed) plus the `X-User-Id` / `X-Role` headers.
// Connectors are owned per user, like conversations: only the
// owner — or a platform admin (`admin` / `super_admin` role) —
// may read, update or delete one. Responses never carry the
// stored config verbatim: secret-bearing fields are masked.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Connector handlers
// ---------------------------------------------------------------------------

/// Create one connector record. The name-uniqueness scan and the
/// insert run under `state.connector_write`, so two concurrent
/// creates with the same name cannot both pass the check. The
/// stored config is kept verbatim; callers only ever receive the
/// redacted copy (see `redact_connector`).
pub(crate) fn create_connector_record(
    state: &AiSupportState,
    owner: &str,
    body: CreateConnectorRequest,
) -> UnifiedResult<Connector> {
    let name = require_text(&body.name, "name", MAX_CONNECTOR_NAME_LEN)?;
    let config = match &body.config {
        Some(config) if !config.is_object() => {
            return Err(UnifiedError::bad_request("config must be a JSON object"));
        }
        Some(config) => config.clone(),
        None => json!({}),
    };
    let _guard = state
        .connector_write
        .lock()
        .expect("connector write lock poisoned");
    if state
        .connectors
        .iter()
        .any(|entry| entry.value().name == name)
    {
        return Err(UnifiedError::conflict(format!(
            "A connector named \"{name}\" already exists"
        )));
    }
    let now = Utc::now();
    let connector = Connector {
        id: Uuid::now_v7(),
        name,
        kind: body.kind,
        enabled: body.enabled.unwrap_or(false),
        config,
        owner_id: owner.to_string(),
        created_at: now,
        updated_at: now,
    };
    state.connectors.insert(connector.id, connector.clone());
    Ok(connector)
}

/// Load a connector or 404, then refuse with 403 when `caller`
/// may not see it (not the owner and not a platform admin).
pub(crate) fn load_connector(
    state: &AiSupportState,
    id: Uuid,
    caller: &Caller,
) -> UnifiedResult<Connector> {
    let connector = state
        .connectors
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Connector not found: {id}")))?;
    require_connector_access(&connector, caller)?;
    Ok(connector)
}

/// Apply an update to a connector. The rename check and the insert
/// run under `state.connector_write`, and the stored record is
/// re-read inside the lock so a concurrent update cannot slip a
/// duplicate name past the check.
pub(crate) fn update_connector_record(
    state: &AiSupportState,
    caller: &Caller,
    id: Uuid,
    body: UpdateConnectorRequest,
) -> UnifiedResult<Connector> {
    let _existing = load_connector(state, id, caller)?;
    let _guard = state
        .connector_write
        .lock()
        .expect("connector write lock poisoned");
    let mut connector = state
        .connectors
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Connector not found: {id}")))?;
    if let Some(name) = &body.name {
        let name = require_text(name, "name", MAX_CONNECTOR_NAME_LEN)?;
        if state
            .connectors
            .iter()
            .any(|entry| entry.value().name == name && entry.value().id != id)
        {
            return Err(UnifiedError::conflict(format!(
                "A connector named \"{name}\" already exists"
            )));
        }
        connector.name = name;
    }
    if let Some(kind) = body.kind {
        connector.kind = kind;
    }
    if let Some(enabled) = body.enabled {
        connector.enabled = enabled;
    }
    if let Some(config) = &body.config {
        if !config.is_object() {
            return Err(UnifiedError::bad_request("config must be a JSON object"));
        }
        connector.config = config.clone();
    }
    connector.updated_at = Utc::now();
    state.connectors.insert(connector.id, connector.clone());
    Ok(connector)
}

pub(crate) async fn create_connector(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    body: web::Json<CreateConnectorRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Connector> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = authorize_connectors(&state, &req)?;
        let connector = create_connector_record(&state, &caller.actor, body.into_inner())?;
        Ok(redact_connector(&connector))
    })();
    match result {
        Ok(connector) => created(connector),
        Err(error) => err(error),
    }
}

pub(crate) async fn list_connectors(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<Connector>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = authorize_connectors(&state, &req)?;
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let enabled_filter = params.filters.get("enabled").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Connector> = state
            .connectors
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|connector| connector_visible_to(connector, &caller))
            .filter(|connector| {
                if let Some(kind) = &kind_filter {
                    let expected = match kind.as_str() {
                        "webchat" => ConnectorKind::Webchat,
                        "telegram" => ConnectorKind::Telegram,
                        "whatsapp" => ConnectorKind::Whatsapp,
                        "slack" => ConnectorKind::Slack,
                        "messenger" => ConnectorKind::Messenger,
                        _ => return false,
                    };
                    if connector.kind != expected {
                        return false;
                    }
                }
                if let Some(enabled) = &enabled_filter {
                    let expected = match enabled.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if connector.enabled != expected {
                        return false;
                    }
                }
                if !search.is_empty() && !connector.name.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .map(|connector| redact_connector(&connector))
            .collect();
        let sort = params.allowed_sort(&["name", "created_at", "updated_at"], "name");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "created_at" => a.created_at.cmp(&b.created_at),
                "updated_at" => a.updated_at.cmp(&b.updated_at),
                _ => a.name.cmp(&b.name),
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

pub(crate) async fn get_connector(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Connector> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = authorize_connectors(&state, &req)?;
        let id = parse_uuid(&path.into_inner(), "connector")?;
        let connector = load_connector(&state, id, &caller)?;
        Ok(redact_connector(&connector))
    })();
    match result {
        Ok(connector) => ok(connector),
        Err(error) => err(error),
    }
}

pub(crate) async fn update_connector(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateConnectorRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Connector> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = authorize_connectors(&state, &req)?;
        let id = parse_uuid(&path.into_inner(), "connector")?;
        let connector = update_connector_record(&state, &caller, id, body.into_inner())?;
        Ok(redact_connector(&connector))
    })();
    match result {
        Ok(connector) => ok(connector),
        Err(error) => err(error),
    }
}

pub(crate) async fn delete_connector(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = authorize_connectors(&state, &req)?;
        let id = parse_uuid(&path.into_inner(), "connector")?;
        let _connector = load_connector(&state, id, &caller)?;
        state.connectors.remove(&id);
        Ok(json!({ "deleted": true, "connector_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
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
                .with_roles(vec![role.to_string()])
                .with_company("company-1".to_string()),
        );
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-1".to_string(),
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

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_company("company-1".to_string()),
        );
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-1".to_string(),
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
    use unified_core::hooks::{HookBus, HookContext};
    use actix_web::test::{read_body, TestRequest};
    use std::collections::HashMap;
    use std::sync::Arc;

    /// State whose `role.check` hook grants the manage capability
    /// exactly to role `manager` (and, via the roles wildcard in
    /// production, to admins). Any other role is refused.
    fn managing_state() -> AiSupportState {
        let state = AiSupportState::default();
        let bus = Arc::new(HookBus::new());
        bus.register(
            "role.check",
            "test",
            Arc::new(|ctx: &mut HookContext| {
                let role = ctx
                    .get("role")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                let permission = ctx
                    .get("permission")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                if role == "manager" && permission == PERM_CONNECTORS_MANAGE {
                    ctx.set("allowed", serde_json::Value::Bool(true));
                }
            }),
        );
        *state.hooks.write().expect("hooks lock poisoned") = Some(bus);
        state
    }

    fn owner_connector(state: &AiSupportState) -> Connector {
        create_connector_record(
            state,
            &test_user_uuid("user-1").to_string(),
            CreateConnectorRequest {
                name: "support-telegram".to_string(),
                kind: ConnectorKind::Telegram,
                enabled: Some(true),
                config: Some(json!({
                    "bot_token": "tok-secret-123",
                    "webhook_url": "https://example.test/hook",
                })),
            },
        )
        .expect("fixture connector must be created")
    }

    fn params() -> PaginationParams {
        PaginationParams {
            page: Some(1),
            per_page: Some(10),
            sort: None,
            order: None,
            search: None,
            filters: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn anonymous_create_is_unauthorized() {
        let state = managing_state();
        let req = TestRequest::default().to_http_request();
        let resp = create_connector(
            web::Data::new(state),
            req,
            web::Json(CreateConnectorRequest {
                name: "x".to_string(),
                kind: ConnectorKind::Webchat,
                enabled: None,
                config: None,
            }),
        )
        .await;
        assert_eq!(resp.status().as_u16(), 401);
    }

    #[tokio::test]
    async fn anonymous_list_and_get_are_unauthorized() {
        let state = managing_state();
        let connector = owner_connector(&state);
        let list = list_connectors(
            web::Data::new(state.clone()),
            TestRequest::default().to_http_request(),
            web::Query(params()),
        )
        .await;
        assert_eq!(list.status().as_u16(), 401);
        let get = get_connector(
            web::Data::new(state),
            TestRequest::default().to_http_request(),
            web::Path::from(connector.id.to_string()),
        )
        .await;
        assert_eq!(get.status().as_u16(), 401);
    }

    #[tokio::test]
    async fn caller_without_the_manage_capability_is_forbidden() {
        let state = managing_state();
        let req = verified_request("user-1", "viewer");
        let resp = list_connectors(web::Data::new(state), req, web::Query(params())).await;
        assert_eq!(resp.status().as_u16(), 403);
    }

    #[tokio::test]
    async fn non_owner_read_of_another_users_connector_is_forbidden() {
        let state = managing_state();
        let connector = owner_connector(&state);
        // user-2 holds the manage capability (role `manager`) but
        // does not own the connector: ownership, not the capability,
        // must refuse.
        let req = verified_request("user-2", "manager");
        let resp = get_connector(
            web::Data::new(state.clone()),
            req,
            web::Path::from(connector.id.to_string()),
        )
        .await;
        assert_eq!(resp.status().as_u16(), 403);
        let list_req = verified_request("user-2", "manager");
        let list = list_connectors(web::Data::new(state), list_req, web::Query(params())).await;
        assert_eq!(list.status().as_u16(), 200);
        let body: Value = serde_json::from_slice(&read_body(list).await).unwrap();
        assert_eq!(body["data"]["items"], json!([]));
    }

    #[tokio::test]
    async fn owner_read_masks_secrets_in_get_and_list() {
        let state = managing_state();
        let connector = owner_connector(&state);
        let req = verified_request("user-1", "manager");
        let resp = get_connector(
            web::Data::new(state.clone()),
            req,
            web::Path::from(connector.id.to_string()),
        )
        .await;
        assert_eq!(resp.status().as_u16(), 200);
        let body: Value = serde_json::from_slice(&read_body(resp).await).unwrap();
        assert_eq!(body["data"]["config"]["bot_token"], "********");
        assert_eq!(
            body["data"]["config"]["webhook_url"],
            "https://example.test/hook"
        );
        assert_eq!(
            body["data"]["owner_id"],
            test_user_uuid("user-1").to_string()
        );
        // The stored config still holds the real secret.
        let stored = state
            .connectors
            .get(&connector.id)
            .expect("connector stored");
        assert_eq!(stored.value().config["bot_token"], "tok-secret-123");

        let list_req = verified_request("user-1", "manager");
        let list = list_connectors(
            web::Data::new(state),
            list_req,
            web::Query(params()),
        )
        .await;
        assert_eq!(list.status().as_u16(), 200);
        let body: Value = serde_json::from_slice(&read_body(list).await).unwrap();
        assert_eq!(body["data"]["items"][0]["config"]["bot_token"], "********");
    }

    #[tokio::test]
    async fn create_response_masks_secrets_and_records_the_owner() {
        let state = managing_state();
        let req = verified_request("user-1", "manager");
        let resp = create_connector(
            web::Data::new(state),
            req,
            web::Json(CreateConnectorRequest {
                name: "fresh".to_string(),
                kind: ConnectorKind::Slack,
                enabled: Some(true),
                config: Some(json!({ "api_key": "key-secret-9" })),
            }),
        )
        .await;
        assert_eq!(resp.status().as_u16(), 201);
        let body: Value = serde_json::from_slice(&read_body(resp).await).unwrap();
        assert_eq!(body["data"]["config"]["api_key"], "********");
        assert_eq!(
            body["data"]["owner_id"],
            test_user_uuid("user-1").to_string()
        );
    }

    #[test]
    fn concurrent_same_name_creates_allow_exactly_one() {
        // Two (here eight) concurrent creates with one name must
        // yield exactly one success and 409 for the rest: the
        // name check and the insert are serialized by the
        // connector write lock.
        let state = AiSupportState::default();
        let outcomes = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..8 {
                let state = &state;
                handles.push(scope.spawn(move || {
                    create_connector_record(
                        state,
                        "user-1",
                        CreateConnectorRequest {
                            name: "dupe".to_string(),
                            kind: ConnectorKind::Webchat,
                            enabled: None,
                            config: None,
                        },
                    )
                }));
            }
            handles
                .into_iter()
                .map(|handle| handle.join().expect("create thread must not panic"))
                .collect::<Vec<_>>()
        });
        assert_eq!(
            outcomes.iter().filter(|result| result.is_ok()).count(),
            1,
            "exactly one same-name create must succeed"
        );
        assert!(
            outcomes
                .iter()
                .filter(|result| result.is_err())
                .all(|result| result.as_ref().unwrap_err().code == 409),
            "every loser must be a 409 conflict"
        );
        assert_eq!(state.connectors.len(), 1);
    }

    #[test]
    fn duplicate_name_create_is_a_conflict() {
        let state = AiSupportState::default();
        owner_connector(&state);
        let error = create_connector_record(
            &state,
            "user-2",
            CreateConnectorRequest {
                name: "support-telegram".to_string(),
                kind: ConnectorKind::Slack,
                enabled: None,
                config: None,
            },
        )
        .unwrap_err();
        assert_eq!(error.code, 409);
    }
}
