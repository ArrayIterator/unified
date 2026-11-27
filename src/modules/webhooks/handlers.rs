// Webhooks - Handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use rand::{distr::Alphanumeric, Rng};
use reqwest::header;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

use super::*;

// Handlers.

pub(crate) async fn list_events(ctx: web::Data<Arc<AppContext>>, req: HttpRequest) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEBHOOKS_VIEW)?;
        Ok(serde_json::json!({
            "events": KNOWN_EVENTS,
            "count": KNOWN_EVENTS.len(),
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateEndpointRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WebhookEndpointView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEBHOOKS_MANAGE)?;
        let name = body.name.trim().to_string();
        if name.is_empty() || name.chars().count() > 120 {
            return Err(UnifiedError::bad_request(
                "name is required and must be at most 120 characters",
            ));
        }
        let url = validate_webhook_url(&body.url)?;
        validate_event_names(&body.events)?;
        validate_retry_policy(body.max_attempts, body.backoff_base_secs, body.timeout_secs)?;
        let secret = match &body.secret {
            Some(secret) if !secret.trim().is_empty() => {
                if secret.chars().count() < 16 {
                    return Err(UnifiedError::bad_request(
                        "secret must be at least 16 characters",
                    ));
                }
                secret.clone()
            }
            _ => rand::rng()
                .sample_iter(&Alphanumeric)
                .take(48)
                .map(char::from)
                .collect(),
        };
        let now = Utc::now();
        let endpoint = WebhookEndpoint {
            id: Uuid::now_v7(),
            name,
            url: url.to_string(),
            events: body.events.clone(),
            secret,
            active: body.active,
            paused: false,
            max_attempts: body.max_attempts,
            backoff_base_secs: body.backoff_base_secs,
            timeout_secs: body.timeout_secs,
            delivery_count: 0,
            last_delivery_at: None,
            last_status: None,
            created_at: now,
            updated_at: now,
        };
        let view = endpoint.view();
        endpoints_store().insert(endpoint.id, endpoint);
        emit(
            webhook_events::ENDPOINT_CREATED,
            serde_json::json!({
                "endpoint_id": view.id,
                "name": view.name.clone(),
                "url": view.url.clone(),
                "events": view.events.clone(),
            }),
        );
        Ok(view)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_endpoints(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEBHOOKS_VIEW)?;
        let mut items: Vec<WebhookEndpointView> = endpoints_store()
            .iter()
            .map(|entry| entry.value().view())
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|endpoint| {
                endpoint.name.to_lowercase().contains(&needle)
                    || endpoint.url.to_lowercase().contains(&needle)
            });
        }
        if let Some(event) = params.filters.get("event") {
            items.retain(|endpoint| endpoint.events.iter().any(|name| name == event));
        }
        if let Some(active) = params.filters.get("active") {
            let wanted = active == "true";
            items.retain(|endpoint| endpoint.active == wanted);
        }
        if let Some(paused) = params.filters.get("paused") {
            let wanted = paused == "true";
            items.retain(|endpoint| endpoint.paused == wanted);
        }
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_endpoint(ctx: web::Data<Arc<AppContext>>, req: HttpRequest, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WebhookEndpointView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEBHOOKS_VIEW)?;
        find_endpoint(id.into_inner()).map(|endpoint| endpoint.view())
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateEndpointRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<WebhookEndpointView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEBHOOKS_MANAGE)?;
        if let Some(name) = &body.name {
            let name = name.trim();
            if name.is_empty() || name.chars().count() > 120 {
                return Err(UnifiedError::bad_request(
                    "name must be non-empty and at most 120 characters",
                ));
            }
        }
        let url = match &body.url {
            Some(raw) => Some(validate_webhook_url(raw)?),
            None => None,
        };
        if let Some(events) = &body.events {
            validate_event_names(events)?;
        }
        if let Some(secret) = &body.secret {
            if secret.chars().count() < 16 {
                return Err(UnifiedError::bad_request(
                    "secret must be at least 16 characters",
                ));
            }
        }
        let mut endpoint = endpoints_store()
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Webhook endpoint not found"))?;
        let current_policy = (
            body.max_attempts.unwrap_or(endpoint.max_attempts),
            body.backoff_base_secs.unwrap_or(endpoint.backoff_base_secs),
            body.timeout_secs.unwrap_or(endpoint.timeout_secs),
        );
        validate_retry_policy(current_policy.0, current_policy.1, current_policy.2)?;
        if let Some(name) = &body.name {
            endpoint.name = name.trim().to_string();
        }
        if let Some(url) = url {
            endpoint.url = url.to_string();
        }
        if let Some(events) = &body.events {
            endpoint.events = events.clone();
        }
        if let Some(secret) = &body.secret {
            endpoint.secret = secret.clone();
        }
        if let Some(active) = body.active {
            endpoint.active = active;
        }
        if let Some(paused) = body.paused {
            endpoint.paused = paused;
        }
        endpoint.max_attempts = current_policy.0;
        endpoint.backoff_base_secs = current_policy.1;
        endpoint.timeout_secs = current_policy.2;
        endpoint.updated_at = Utc::now();
        let view = endpoint.view();
        drop(endpoint);
        emit(
            webhook_events::ENDPOINT_UPDATED,
            serde_json::json!({
                "endpoint_id": view.id,
                "name": view.name.clone(),
                "url": view.url.clone(),
                "events": view.events.clone(),
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_endpoint(ctx: web::Data<Arc<AppContext>>, req: HttpRequest, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEBHOOKS_MANAGE)?;
        let removed = endpoints_store().remove(&id);
        match removed {
            Some((_, endpoint)) => {
                // The endpoint's delivery records go with it: they
                // reference an endpoint that no longer exists and
                // can never be redelivered.
                deliveries_store().retain(|_, delivery| delivery.endpoint_id != id);
                emit(
                    webhook_events::ENDPOINT_DELETED,
                    serde_json::json!({ "endpoint_id": id, "name": endpoint.name }),
                );
                Ok(serde_json::json!({ "id": id, "deleted": true }))
            }
            None => Err(UnifiedError::not_found("Webhook endpoint not found")),
        }
    })();
    json::respond(result, debug)
}

pub(crate) fn set_paused(id: Uuid, paused: bool) -> UnifiedResult<WebhookEndpointView> {
    let mut endpoint = endpoints_store()
        .get_mut(&id)
        .ok_or_else(|| UnifiedError::not_found("Webhook endpoint not found"))?;
    endpoint.paused = paused;
    endpoint.updated_at = Utc::now();
    let view = endpoint.view();
    drop(endpoint);
    if paused {
        emit(
            webhook_events::ENDPOINT_PAUSED,
            serde_json::json!({ "endpoint_id": id }),
        );
    }
    Ok(view)
}

pub(crate) async fn pause_endpoint(ctx: web::Data<Arc<AppContext>>, req: HttpRequest, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WebhookEndpointView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEBHOOKS_MANAGE)?;
        set_paused(id.into_inner(), true)
    })();
    json::respond(result, debug)
}

pub(crate) async fn resume_endpoint(ctx: web::Data<Arc<AppContext>>, req: HttpRequest, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WebhookEndpointView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEBHOOKS_MANAGE)?;
        set_paused(id.into_inner(), false)
    })();
    json::respond(result, debug)
}

pub(crate) async fn test_endpoint(ctx: web::Data<Arc<AppContext>>, req: HttpRequest, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEBHOOKS_MANAGE)?;
        let endpoint = find_endpoint(id)?;
        if !endpoint.active {
            return Err(UnifiedError::conflict(
                "Cannot test a disabled webhook endpoint; enable it first",
            ));
        }
        if endpoint.paused {
            return Err(UnifiedError::conflict(
                "Cannot test a paused webhook endpoint; resume it first",
            ));
        }
        let payload = serde_json::json!({
            "event": webhook_events::PING,
            "endpoint_id": endpoint.id,
            "sent_at": Utc::now().to_rfc3339(),
            "message": "Test delivery from the webhooks manager",
        });
        let delivery_id = enqueue_delivery(&endpoint, webhook_events::PING, &payload);
        Ok(serde_json::json!({
            "delivery_id": delivery_id,
            "endpoint_id": endpoint.id,
            "event": webhook_events::PING,
        }))
    })();
    match result {
        Ok(view) => json::success_response(view, 202),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_deliveries(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEBHOOKS_VIEW)?;
        let mut items: Vec<WebhookDelivery> = deliveries_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(endpoint_id) = params.filters.get("endpoint_id") {
            items.retain(|delivery| delivery.endpoint_id.to_string() == *endpoint_id);
        }
        if let Some(event) = params.filters.get("event") {
            items.retain(|delivery| delivery.event == *event);
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|delivery| delivery.status == *status);
        }
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|delivery| delivery.event.to_lowercase().contains(&needle));
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_delivery(ctx: web::Data<Arc<AppContext>>, req: HttpRequest, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WebhookDelivery> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEBHOOKS_VIEW)?;
        deliveries_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Webhook delivery not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn redeliver(ctx: web::Data<Arc<AppContext>>, req: HttpRequest, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<WebhookDelivery> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEBHOOKS_MANAGE)?;
        let delivery = deliveries_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Webhook delivery not found"))?;
        if delivery.status == "delivering" || delivery.status == "retrying" {
            return Err(UnifiedError::conflict(format!(
                "Cannot redeliver a delivery in status \"{}\"",
                delivery.status
            )));
        }
        // A redelivery sends to the endpoint again, so the endpoint
        // must be live: a deleted endpoint can never receive it, and
        // a disabled or paused endpoint must not be sent into.
        let endpoint = find_endpoint(delivery.endpoint_id)?;
        if !endpoint.active {
            return Err(UnifiedError::conflict(
                "Cannot redeliver to a disabled webhook endpoint; enable it first",
            ));
        }
        if endpoint.paused {
            return Err(UnifiedError::conflict(
                "Cannot redeliver to a paused webhook endpoint; resume it first",
            ));
        }
        if let Some(mut record) = deliveries_store().get_mut(&id) {
            record.attempts = 0;
            record.status_code = None;
            record.error = None;
            record.next_retry_at = None;
            record.delivered_at = None;
            record.set_status(DeliveryStatus::Queued);
        }
        let delivery_id = id;
        // Same shape as enqueue_delivery: run the delivery on the
        // current runtime when there is one; without a runtime the
        // record stays queued until a later redelivery runs it.
        // (tokio::spawn would panic outside a runtime.)
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move { run_delivery(delivery_id).await });
            }
            Err(_) => {}
        }
        deliveries_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Webhook delivery not found"))
    })();
    match result {
        Ok(view) => json::success_response(view, 202),
        Err(error) => json::error_response(&error, debug),
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

    fn data(ctx: AppContext) -> web::Data<Arc<AppContext>> {
        web::Data::new(Arc::new(ctx))
    }

    fn stored_endpoint(active: bool, paused: bool) -> WebhookEndpoint {
        let endpoint = WebhookEndpoint {
            id: Uuid::now_v7(),
            name: "test".to_string(),
            url: "https://example.com/hook".to_string(),
            events: vec!["webhook.ping".to_string()],
            secret: "test-secret-that-is-long".to_string(),
            active,
            paused,
            max_attempts: 3,
            backoff_base_secs: 5,
            timeout_secs: 30,
            delivery_count: 0,
            last_delivery_at: None,
            last_status: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        endpoints_store().insert(endpoint.id, endpoint.clone());
        endpoint
    }

    fn stored_delivery(endpoint_id: Uuid) -> WebhookDelivery {
        let delivery = WebhookDelivery {
            id: Uuid::now_v7(),
            endpoint_id,
            event: "webhook.ping".to_string(),
            payload: serde_json::json!({"event": "webhook.ping"}),
            payload_hash: "hash".to_string(),
            status: DeliveryStatus::Failed.as_str().to_string(),
            attempts: 1,
            max_attempts: 3,
            next_retry_at: None,
            status_code: Some(500),
            duration_ms: Some(12),
            error: Some("boom".to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            delivered_at: None,
        };
        deliveries_store().insert(delivery.id, delivery.clone());
        delivery
    }

    #[tokio::test]
    async fn list_endpoints_rejects_anonymous_callers() {
        let response = list_endpoints(
            data(test_context()),
            TestRequest::default().to_http_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_endpoints_rejects_callers_without_webhooks_view() {
        // Headers present, but no `role.check` handler answers:
        // the guard fails closed.
        let response = list_endpoints(
            data(test_context()),
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn list_endpoints_accepts_callers_with_webhooks_view() {
        let response = list_endpoints(
            data(allowing_context()),
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_endpoint_rejects_a_paused_endpoint() {
        let endpoint = stored_endpoint(true, true);
        let response = test_endpoint(
            data(allowing_context()),
            authed_request(),
            web::Path::from(endpoint.id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn redeliver_rejects_a_paused_endpoint() {
        let endpoint = stored_endpoint(true, true);
        let delivery = stored_delivery(endpoint.id);
        let response = redeliver(
            data(allowing_context()),
            authed_request(),
            web::Path::from(delivery.id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn redeliver_rejects_a_disabled_endpoint() {
        let endpoint = stored_endpoint(false, false);
        let delivery = stored_delivery(endpoint.id);
        let response = redeliver(
            data(allowing_context()),
            authed_request(),
            web::Path::from(delivery.id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn delete_endpoint_removes_its_deliveries() {
        let endpoint = stored_endpoint(true, false);
        let delivery = stored_delivery(endpoint.id);
        let response = delete_endpoint(
            data(allowing_context()),
            authed_request(),
            web::Path::from(endpoint.id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(endpoints_store().get(&endpoint.id).is_none());
        assert!(deliveries_store().get(&delivery.id).is_none());
    }
}
