// network module -- HTTP handlers.

use unified_core::context::{AppContext, GeoInfo};
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::events;
use unified_core::json;
use unified_core::modules::Module;
use actix_web::{web, HttpRequest, HttpResponse};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::*;

// Caller authentication, mirroring the bi and supervision modules:
// identity comes from the server-verified caller, and the
// roles module answers `role.check` on the hook bus. Fails closed:
// no answer means not allowed.

/// Caller identity: the server-verified caller.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(
    ctx: &AppContext,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

pub(crate) async fn network_info(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_NETWORK_VIEW)?;
        let networks = sysinfo::Networks::new_with_refreshed_list();
        let mut interfaces = Vec::new();
        for (name, data) in networks.iter() {
            interfaces.push(InterfaceView {
                name: name.clone(),
                mac_address: data.mac_address().to_string(),
                ip_addresses: data
                    .ip_networks()
                    .iter()
                    .map(|network| format!("{}/{}", network.addr, network.prefix))
                    .collect(),
                received_bytes: data.received(),
                transmitted_bytes: data.transmitted(),
            });
        }
        interfaces.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(serde_json::json!({
            "hostname": sysinfo::System::host_name(),
            "uptime_seconds": sysinfo::System::uptime(),
            "interfaces": interfaces,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn geo_lookup_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<GeoLookupRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GeoInfo> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_NETWORK_MANAGE)?;
        let ip = body.ip.trim().to_string();
        // Delegate to the GEO module through the hook event first.
        let emitted = ctx.hooks.emit(
            events::GEO_LOOKUP,
            serde_json::json!({ "ip": ip }),
        );
        for result_value in emitted.results {
            // `GeoInfo` is Serialize-only, so rebuild it from the JSON
            // result instead of deserialising it.
            if result_value.get("error").is_some() {
                continue;
            }
            let info = GeoInfo {
                ip: result_value
                    .get("ip")
                    .and_then(Value::as_str)
                    .unwrap_or(&ip)
                    .to_string(),
                country_code: result_value
                    .get("country_code")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                country_name: result_value
                    .get("country_name")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                city: result_value
                    .get("city")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                region: result_value
                    .get("region")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                asn: result_value.get("asn").and_then(Value::as_u64).map(|v| v as u32),
                asn_organization: result_value
                    .get("asn_organization")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                latitude: result_value.get("latitude").and_then(Value::as_f64),
                longitude: result_value.get("longitude").and_then(Value::as_f64),
                source: result_value
                    .get("source")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
            };
            // The lookup result is returned as-is. It is deliberately
            // NOT written to the app-global Geo variable: that global
            // is later cloned into user, company, and license records,
            // so storing an arbitrary queried IP here would pollute
            // subsequently created records with someone else's lookup.
            if info.source != "unknown" || info.country_code.is_some() {
                return Ok(info);
            }
        }
        // Fallback when no hook handler answered: call the GEO lookup
        // directly and return the result without touching global state.
        let info = crate::modules::geo::lookup_geo(&ctx, &ip)?;
        Ok(info)
    })();
    json::respond(result, debug)
}


pub(crate) async fn connections_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_NETWORK_VIEW)?;
        let stats = connection_stats();
        let http = stats.http.load(Ordering::SeqCst);
        let websocket = stats.websocket.load(Ordering::SeqCst);
        Ok(serde_json::json!({
            "http": http,
            "websocket": websocket,
            "total_active": http + websocket,
            "total_accepted": stats.total_accepted.load(Ordering::SeqCst),
            "uptime_seconds": ctx.uptime_seconds(),
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn dns_check_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<DnsCheckRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DnsReport> = (|| async {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_NETWORK_MANAGE)?;
        let verifier = DnsVerifier::new();
        let report = verifier
            .check_domain_full(body.domain.trim(), body.selector.as_deref())
            .await?;
        // Publish the report on the hook bus so the Domains, Mail, and
        // Security modules can observe every DNS check.
        ctx.hooks.emit(
            "dns.check",
            serde_json::to_value(&report).unwrap_or(Value::Null),
        );
        Ok(report)
    })()
    .await;
    json::respond(result, debug)
}

// Module definition.

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

    fn data(ctx: AppContext) -> web::Data<Arc<AppContext>> {
        web::Data::new(Arc::new(ctx))
    }

    #[test]
    fn caller_accepts_a_verified_caller() {
        let found = caller(&authed_request()).expect("caller parses headers");
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
    }

    #[test]
    fn caller_rejects_a_request_without_a_verified_caller() {
        let req = TestRequest::default().to_http_request();
        let error = caller(&req).expect_err("a request without a verified caller must fail");
        assert_eq!(error.code, 401);
    }

    #[test]
    fn caller_without_a_resolved_role_is_anonymous() {
        // A verified caller with no role resolved holds the anonymous
        // role: the identity resolves, the capability check is what
        // refuses it.
        let req = verified_request_without_roles("user-1");
        let found = caller(&req).expect("a verified caller resolves without a role");
        assert_eq!(found.role, "anonymous");
    }

    #[test]
    fn require_permission_fails_closed_without_a_role_check_handler() {
        // No `role.check` handler is registered on this bus, so the
        // preset `"allowed": false` must stand and the call is refused.
        let ctx = test_context();
        assert!(!check_capability(&ctx, "admin", PERM_NETWORK_VIEW));
        let error = require_permission(&ctx, "admin", PERM_NETWORK_VIEW)
            .expect_err("no role.check handler must deny");
        assert_eq!(error.code, 403);
    }

    #[test]
    fn require_permission_accepts_when_role_check_allows() {
        let ctx = allowing_context();
        assert!(require_permission(&ctx, "admin", PERM_NETWORK_MANAGE).is_ok());
    }

    #[tokio::test]
    async fn network_info_rejects_anonymous_callers() {
        let response = network_info(data(test_context()), anonymous_request()).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn network_info_allows_an_authorised_caller() {
        let response = network_info(data(allowing_context()), authed_request()).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn connections_rejects_anonymous_callers() {
        let response = connections_handler(data(test_context()), anonymous_request()).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn connections_allows_an_authorised_caller() {
        let response = connections_handler(data(allowing_context()), authed_request()).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn geo_lookup_rejects_anonymous_callers_without_looking_up() {
        let body = web::Json(GeoLookupRequest {
            ip: "8.8.8.8".to_string(),
        });
        let response = geo_lookup_handler(data(test_context()), anonymous_request(), body).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn dns_check_rejects_anonymous_callers_without_checking() {
        let body = web::Json(DnsCheckRequest {
            domain: "example.com".to_string(),
            selector: None,
        });
        let response = dns_check_handler(data(test_context()), anonymous_request(), body).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
