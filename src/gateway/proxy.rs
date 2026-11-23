// Reverse proxy service on Pingora.
//
// This file is only compiled when the `gateway` Cargo feature is
// enabled (see `gateway/mod.rs`). It implements the documented
// Pingora proxy-service shape:
//
//   - `ProxyHttp` with `request_filter` (routing-rule match),
//     `upstream_peer` (upstream selection), `upstream_request_filter`
//     (forwarded headers), `response_filter` (hop-by-hop stripping
//     and the gateway request-id header), and `logging` (request-log
//     ring buffer).
//   - `http_proxy_service(&server.configuration, proxy)` plus
//     `Server::new(None)` / `bootstrap()` / `add_service()` /
//     `run_forever()` to serve it.
//
// COMPILE-UNVERIFIED POINTS (offline environment, no toolchain and
// no crate registry access; see the coordinator report for the full
// list): every Pingora API call below that is not part of that
// documented shape is marked `UNCERTAIN (pingora-N)`. The rest of
// this file — routing-rule matching, upstream selection through the
// load-balancer module, header handling, health exclusion, request
// logging — uses only verified in-repo APIs.
//
// Upstream selection delegates to the load balancer: balancer pools
// resolve through `eligible_backends` + the pure `select_backend`
// function (round-robin, least-connections, weighted, IP hash); the
// gateway never duplicates strategy code. Direct address pools reuse
// the same function over synthesized backend records (see
// `health::backend_for_direct_address`). Resolution happens on every
// request against the live eligible set, so a resolved address is
// never cached past the health-check TTL.
//
// Stop semantics: Pingora's graceful shutdown is signal-driven at
// the process level (SIGTERM / SIGINT). `GatewayService::stop`
// therefore drains the gateway — new requests are rejected with
// 503 and in-flight requests finish — and the listener thread exits
// with the process. A stopped service cannot rebind its listen
// addresses in the same process; restarting after a stop requires a
// daemon restart. See `GatewayService::stop` and UNCERTAIN
// (pingora-6) for the one-line alternative if `Server::shutdown`
// exists in the resolved Pingora version.

use crate::gateway::health::{
    address_health_key, backend_for_direct_address, connections_snapshot, eligible_for_gateway,
    is_eligible,
};
use crate::gateway::routing::{RoutingRule, UpstreamRef, select_rule};
use crate::gateway::{GATEWAY_STARTED, GATEWAY_STOPPED, GatewayState, RequestLogEntry};
use crate::modules::load_balancer::{
    Backend, note_backend_connection_closed, note_backend_connection_open, select_backend,
};
use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;
use pingora::http::{RequestHeader, ResponseHeader};
use pingora::proxy::{ProxyHttp, Session, http_proxy_service};
use pingora::upstreams::peer::HttpPeer;
use serde_json::json as json_value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json::error_json;
use uuid::Uuid;

/// Hop-by-hop headers, stripped from upstream responses before they
/// reach the client (RFC 9110 section 7.6.1). The `Connection`
/// header itself is stripped first so a listed header cannot survive
/// through it.
const HOP_BY_HOP_HEADERS: [&str; 8] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "proxy-authenticate",
];

/// The upstream chosen for one request.
#[derive(Debug, Clone)]
pub struct UpstreamChoice {
    /// Load-balancer backend id, when the upstream came from a
    /// balancer pool. `None` for direct addresses.
    pub backend_id: Option<Uuid>,
    pub balancer_id: Option<Uuid>,
    /// Dial address (`host:port`).
    pub address: String,
    /// Human-readable upstream name (backend name or address).
    pub name: String,
}

/// Per-request context carried through the Pingora callbacks.
pub struct GatewayCtx {
    pub request_id: String,
    pub started: Instant,
    pub rule_id: Option<Uuid>,
    /// The matched rule itself (cloned). The fallback rule for the
    /// default upstream is never stored in the rule store, so the
    /// context must carry the rule — looking it up by id again in
    /// `upstream_peer` would fail for the fallback path.
    pub rule: Option<RoutingRule>,
    pub upstream: Option<UpstreamChoice>,
    pub client_ip: Option<String>,
    /// True when the request arrived over the TLS listener.
    pub terminated_tls: bool,
}

/// The Pingora proxy implementation. One instance serves the plain
/// listener (`terminated_tls: false`); a second instance sharing the
/// same state serves the TLS listener (`terminated_tls: true`), so a
/// rule with `require_tls: true` can tell which listener the request
/// arrived on.
pub struct GatewayProxy {
    pub state: Arc<GatewayState>,
    pub terminated_tls: bool,
}

impl GatewayProxy {
    pub fn new(state: Arc<GatewayState>, terminated_tls: bool) -> Self {
        Self { state, terminated_tls }
    }

    /// Read the client IP for selection: the first `X-Forwarded-For`
    /// entry when present, otherwise the peer address of the
    /// downstream connection.
    fn client_ip(session: &Session) -> Option<String> {
        if let Some(forwarded) = session
            .req_header()
            .headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
        {
            if let Some(first) = forwarded.split(',').next() {
                let first = first.trim();
                if !first.is_empty() {
                    return Some(first.to_string());
                }
            }
        }
        // UNCERTAIN (pingora-7): `Session::client_addr` shape.
        // Expected to return the peer socket address of the
        // downstream connection, or `None` when unavailable. If the
        // resolved Pingora version names this differently (for
        // example a `client_addr` field), replace this call and keep
        // the `None` fallback: selection treats a missing IP as
        // `0.0.0.0` for IP-hash purposes.
        session.client_addr().map(|addr| addr.ip().to_string())
    }

    /// Split a `host:port` address into a socket address for
    /// `HttpPeer`. Hostnames are resolved at request time (never
    /// cached), so DNS changes are picked up without a restart.
    async fn resolve_socket_address(address: &str) -> UnifiedResult<std::net::SocketAddr> {
        match address.parse::<std::net::SocketAddr>() {
            Ok(addr) => Ok(addr),
            Err(_) => {
                let mut resolved = tokio::net::lookup_host(address)
                    .await
                    .map_err(|error| {
                        UnifiedError::internal(format!(
                            "Cannot resolve upstream address \"{address}\": {error}"
                        ))
                    })?;
                resolved.next().ok_or_else(|| {
                    UnifiedError::internal(format!(
                        "Upstream address \"{address}\" did not resolve to any socket address"
                    ))
                })
            }
        }
    }
}

/// Choose an upstream for one matched rule. Pure selection logic
/// (no Pingora types) so it can be unit-tested without a listener.
///
/// - Balancer pool: the live eligible set from the load-balancer
///   module, minus backends the gateway health state marks
///   unhealthy, selected with the load balancer's own pure
///   `select_backend`.
/// - Single address: the address itself, unless the gateway health
///   state marks it unhealthy.
/// - Address pool: synthesized backend records for the healthy
///   members, selected with the same `select_backend`.
pub fn choose_upstream(
    state: &GatewayState,
    rule: &RoutingRule,
    client_ip: Option<&str>,
) -> UnifiedResult<UpstreamChoice> {
    let connections: HashMap<Uuid, u64> = connections_snapshot();
    match &rule.upstream {
        UpstreamRef::Balancer { balancer_id } => {
            let eligible = eligible_for_gateway(*balancer_id);
            if eligible.is_empty() {
                return Err(UnifiedError::new(
                    format!(
                        "No healthy upstream is available for load balancer \"{balancer_id}\" (rule \"{}\")",
                        rule.name
                    ),
                    503,
                ));
            }
            let counter = state.next_counter(*balancer_id);
            let picked = select_backend(&eligible, rule.strategy, client_ip, counter, &connections)
                .ok_or_else(|| {
                    UnifiedError::new(
                        format!(
                            "Upstream selection returned no backend for load balancer \"{balancer_id}\""
                        ),
                        503,
                    )
                })?;
            let backend = eligible
                .iter()
                .find(|entry| entry.id == picked)
                .ok_or_else(|| {
                    UnifiedError::internal(format!(
                        "Selected backend \"{picked}\" is not in the eligible set"
                    ))
                })?;
            Ok(UpstreamChoice {
                backend_id: Some(backend.id),
                balancer_id: Some(*balancer_id),
                address: backend.address.clone(),
                name: backend.name.clone(),
            })
        }
        UpstreamRef::Address { address } => {
            if !is_eligible(state, &address_health_key(address)) {
                return Err(UnifiedError::new(
                    format!("Upstream \"{address}\" is unhealthy (rule \"{}\")", rule.name),
                    503,
                ));
            }
            Ok(UpstreamChoice {
                backend_id: None,
                balancer_id: None,
                address: address.clone(),
                name: address.clone(),
            })
        }
        UpstreamRef::AddressPool { addresses } => {
            let backends: Vec<Backend> = addresses
                .iter()
                .filter(|entry| is_eligible(state, &address_health_key(&entry.address)))
                .map(|entry| {
                    backend_for_direct_address(rule.id, &entry.address, entry.weight)
                })
                .collect();
            if backends.is_empty() {
                return Err(UnifiedError::new(
                    format!(
                        "No healthy upstream is available in the address pool (rule \"{}\")",
                        rule.name
                    ),
                    503,
                ));
            }
            let counter = state.next_counter(rule.id);
            let picked = select_backend(&backends, rule.strategy, client_ip, counter, &connections)
                .ok_or_else(|| {
                    UnifiedError::new(
                        format!("Upstream selection returned no address (rule \"{}\")", rule.name),
                        503,
                    )
                })?;
            let backend = backends
                .iter()
                .find(|entry| entry.id == picked)
                .ok_or_else(|| {
                    UnifiedError::internal(format!(
                        "Selected address \"{picked}\" is not in the pool"
                    ))
                })?;
            Ok(UpstreamChoice {
                backend_id: None,
                balancer_id: None,
                address: backend.address.clone(),
                name: backend.address.clone(),
            })
        }
    }
}

/// Build a Pingora error from a message.
///
/// UNCERTAIN (pingora-1): `pingora::Error::new` plus the
/// `ErrorType::InternalError` variant. If the resolved Pingora
/// version renamed the variant (or the constructor), replace this
/// helper in one place; every failure path in this file goes through
/// it. `ErrorType::HTTPStatus(u16)` is the alternative when a
/// specific HTTP status must be rendered instead of a generic
/// upstream error.
fn proxy_error(message: &str) -> pingora::Error {
    log::error!("Gateway proxy error: {message}");
    pingora::Error::new(pingora::ErrorType::InternalError)
}

/// Write a gateway rejection in the standard API envelope
/// (`{"error": {"message", "code", "metadata"}}`, see
/// `crates/unified-core/src/json.rs`) instead of Pingora's bare
/// error page, so gateway rejections look exactly like any other API
/// error. `debug` is always false here: gateway errors carry no
/// trace, so there is nothing a debug render could add.
///
/// UNCERTAIN (pingora-12): `Session::write_response_header` /
/// `write_response_body` on the proxy `Session`, and
/// `ResponseHeader::build` / `insert_header` on the response
/// header. Expected to write the given header, then the body, and
/// finish the stream (`end: true`); the `Content-Length` header is
/// set explicitly so the downstream connection stays reusable. If
/// the resolved Pingora version names these differently, replace
/// this helper in one place. Any failure writing the envelope falls
/// back to Pingora's own `respond_error`, so a client still gets the
/// right status code.
async fn respond_envelope_error(session: &mut Session, status: u16, message: &str) {
    let error = UnifiedError::new(message, status);
    let body = error_json(&error, false).to_string();
    let written = async {
        let mut header = ResponseHeader::build(status, None)?;
        header.insert_header("content-type", "application/json")?;
        header.insert_header("content-length", body.len().to_string())?;
        session.write_response_header(Box::new(header)).await?;
        session.write_response_body(Bytes::from(body), true).await?;
        Ok::<(), pingora::Error>(())
    }
    .await;
    if let Err(error) = written {
        log::error!("Gateway envelope error write failed ({error}); falling back to Pingora error page");
        let _ = session.respond_error(status).await;
    }
}

#[async_trait]
impl ProxyHttp for GatewayProxy {
    type CTX = GatewayCtx;

    fn new_ctx(&self) -> Self::CTX {
        GatewayCtx {
            request_id: Uuid::now_v7().to_string(),
            started: Instant::now(),
            rule_id: None,
            rule: None,
            upstream: None,
            client_ip: None,
            terminated_tls: self.terminated_tls,
        }
    }

    /// Match the request against the routing rules. Returns `Ok(true)`
    /// only when the response has already been written (no rule
    /// matched, TLS required but not terminated, or the gateway is
    /// draining); otherwise `Ok(false)` and `upstream_peer` runs.
    async fn request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<bool> {
        if self.state.stop_requested.load(std::sync::atomic::Ordering::SeqCst) {
            // Draining: reject with the standard error envelope, not
            // Pingora's bare error page. `Session::respond_error`
            // survives only as the fallback inside
            // `respond_envelope_error`.
            respond_envelope_error(session, 503, "Gateway is draining").await;
            return Ok(true);
        }
        let req_header = session.req_header();
        let host = req_header
            .headers
            .get("host")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let path = req_header.uri.path().to_string();
        let headers: Vec<(String, String)> = req_header
            .headers
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|text| (name.as_str().to_string(), text.to_string()))
            })
            .collect();
        ctx.client_ip = Self::client_ip(session);
        let rules = crate::gateway::routing::list_rules();
        let Some(rule) = select_rule(&rules, &host, &path, &headers) else {
            // No rule matched: fall back to the configured default
            // upstream, or reject the request.
            let config = self.state.config_snapshot();
            let Some(default_upstream) = config.default_upstream.clone() else {
                respond_envelope_error(session, 404, "No gateway route matched").await;
                return Ok(true);
            };
            let rule = match fallback_rule_for_default(&default_upstream) {
                Ok(rule) => rule,
                Err(_) => {
                    respond_envelope_error(session, 502, "Default upstream is misconfigured").await;
                    return Ok(true);
                }
            };
            if rule.require_tls && !ctx.terminated_tls {
                respond_envelope_error(session, 426, "TLS is required for this route").await;
                return Ok(true);
            }
            ctx.rule_id = Some(rule.id);
            ctx.rule = Some(rule);
            return Ok(false);
        };
        if rule.require_tls && !ctx.terminated_tls {
            respond_envelope_error(session, 426, "TLS is required for this route").await;
            return Ok(true);
        }
        ctx.rule_id = Some(rule.id);
        ctx.rule = Some(rule.clone());
        Ok(false)
    }

    /// Select the upstream for the matched rule and return its peer.
    /// The chosen backend's connection counter is opened here and
    /// closed in `logging`, so least-connections selection sees live
    /// connections. (Pingora may call `upstream_peer` again on a
    /// retry; a retried selection opens a second counter entry and
    /// the first is closed by the retry path, so the count can
    /// briefly over-count during retries.)
    async fn upstream_peer(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<Box<HttpPeer>> {
        let rule = ctx.rule.clone().ok_or_else(|| {
            proxy_error("No routing rule was matched before upstream selection")
        })?;
        let choice = choose_upstream(&self.state, &rule, ctx.client_ip.as_deref())
            .map_err(|error| proxy_error(&error.message))?;
        if let Some(backend_id) = choice.backend_id {
            note_backend_connection_open(backend_id);
        }
        let socket_addr = Self::resolve_socket_address(&choice.address)
            .await
            .map_err(|error| proxy_error(&error.message))?;
        let _ = session;
        // UNCERTAIN (pingora-2): `HttpPeer::new(socket_addr, tls,
        // sni)`. Expected signature takes a `SocketAddr`, a TLS flag,
        // and an SNI name. Upstreams are plain HTTP today (`tls:
        // false`, empty SNI); upstream TLS is a future extension that
        // passes `true` plus the upstream host as SNI.
        let peer = HttpPeer::new(socket_addr, false, String::new());
        ctx.upstream = Some(choice);
        Ok(Box::new(peer))
    }

    /// Rewrite the upstream request: forward the client address and
    /// protocol, attach the gateway request id, and carry the matched
    /// rule's rate-limit reference for the hardening module.
    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        upstream_request: &mut RequestHeader,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<()> {
        // UNCERTAIN (pingora-4): `RequestHeader::insert_header(name,
        // value)` and `remove_header(name)`. Expected to mutate the
        // header map in place and report a header error. If the
        // resolved version exposes the header map directly instead,
        // use `upstream_request.headers.insert(...)`.
        if let Some(client_ip) = &ctx.client_ip {
            upstream_request.insert_header("x-forwarded-for", client_ip)?;
        }
        let proto = if ctx.terminated_tls { "https" } else { "http" };
        upstream_request.insert_header("x-forwarded-proto", proto)?;
        upstream_request.insert_header("x-gateway-request-id", &ctx.request_id)?;
        if let Some(rule_id) = ctx.rule_id {
            upstream_request.insert_header("x-gateway-rule-id", &rule_id.to_string())?;
        }
        let _ = session;
        Ok(())
    }

    /// Rewrite the upstream response: strip hop-by-hop headers and
    /// add the gateway request-id header so a response can be traced
    /// back to its request-log entry.
    async fn response_filter(
        &self,
        _session: &mut Session,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<()> {
        // UNCERTAIN (pingora-4): `ResponseHeader::insert_header` /
        // `remove_header`, same note as in `upstream_request_filter`.
        for header in HOP_BY_HOP_HEADERS {
            upstream_response.remove_header(header)?;
        }
        upstream_response.insert_header("x-gateway-request-id", &ctx.request_id)?;
        Ok(())
    }

    /// End of a proxied session: close the backend connection
    /// counter and append the request to the ring buffer. Bodies and
    /// authorization headers are never recorded (see
    /// `RequestLogEntry`).
    async fn logging(
        &self,
        session: &mut Session,
        _e: Option<&pingora::Error>,
        ctx: &mut Self::CTX,
    ) {
        if let Some(choice) = &ctx.upstream {
            if let Some(backend_id) = choice.backend_id {
                note_backend_connection_closed(backend_id);
            }
        }
        let req_header = session.req_header();
        let host = req_header
            .headers
            .get("host")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let path = req_header.uri.path().to_string();
        let method = req_header.method.as_str().to_string();
        // UNCERTAIN (pingora-5): reading the final response status.
        // Expected: `Session::resp_header()` returns the response
        // header when one was written. If the resolved version
        // exposes the status differently, use that accessor and keep
        // the `0` fallback for responses that were never written.
        let status = session
            .resp_header()
            .map(|header| header.status.as_u16())
            .unwrap_or(0);
        self.state.record_request(RequestLogEntry::new(
            ctx.request_id.clone(),
            method,
            host,
            path,
            status,
            ctx.started.elapsed().as_millis() as u64,
            ctx.upstream.as_ref().map(|choice| choice.address.clone()),
            ctx.upstream.as_ref().and_then(|choice| choice.backend_id),
        ));
    }
}

/// Build a synthetic rule for the configured default upstream so the
/// fallback path reuses the same selection code as matched rules.
/// The synthetic rule is never stored; it exists only for one request.
fn fallback_rule_for_default(default_upstream: &str) -> UnifiedResult<RoutingRule> {
    let trimmed = default_upstream.trim();
    let upstream = match trimmed.parse::<Uuid>() {
        Ok(balancer_id) => UpstreamRef::Balancer { balancer_id },
        Err(_) => UpstreamRef::Address {
            address: trimmed.to_string(),
        },
    };
    let now = Utc::now();
    Ok(RoutingRule {
        id: Uuid::now_v7(),
        name: "default-upstream".to_string(),
        priority: i32::MIN,
        enabled: true,
        host: None,
        path_prefix: None,
        path_regex: None,
        headers: Vec::new(),
        upstream,
        strategy: crate::modules::load_balancer::Strategy::RoundRobin,
        require_tls: false,
        rate_limit_override: None,
        health_check: crate::modules::load_balancer::HealthCheckConfig::default(),
        created_at: now,
        updated_at: now,
        compiled_path_regex: None,
    })
}

// ---------------------------------------------------------------------------
// Service lifecycle.
// ---------------------------------------------------------------------------

/// Handle to a running Pingora service thread. The thread itself is
/// detached (Pingora's `run_forever` never returns); the handle only
/// records when the service started and which addresses it bound, so
/// the management API can report the run.
#[derive(Debug)]
pub struct GatewayServiceHandle {
    pub started_at: chrono::DateTime<Utc>,
    pub listen_addrs: Vec<String>,
    pub tls_listen_addrs: Vec<String>,
}

/// The Pingora server: builds the `Server`, attaches the plain and
/// TLS proxy services, and runs it on a dedicated thread.
pub struct GatewayService;

impl GatewayService {
    /// Start the gateway service on a dedicated thread. Fails when
    /// the feature is not enabled, the configuration is disabled or
    /// invalid, a service is already running, or a previous service
    /// was stopped in this process (its listener thread still owns
    /// the addresses; restart the daemon to rebind).
    pub fn start(state: Arc<GatewayState>) -> UnifiedResult<GatewayServiceHandle> {
        crate::gateway::require_feature_enabled()?;
        let config = state.config_snapshot();
        config.check()?;
        if state.is_running() {
            return Err(UnifiedError::conflict("Gateway service is already running"));
        }
        if state.ever_started.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(UnifiedError::conflict(
                "Gateway service was already stopped in this process; restart the daemon to bind the listen addresses again",
            ));
        }
        let listen_addrs = config.listen.clone();
        let tls_listen_addrs = config.tls_listen.clone();

        // UNCERTAIN (pingora-8): `Server::new(None)` plus
        // `bootstrap()`. Expected from the documented Pingora server
        // shape: `Server::new` takes an optional `ServerConf` and
        // returns a `Result<Server>`; `bootstrap` prepares signal
        // handling and the work-stealing runtime. If the resolved
        // version requires an explicit configuration, pass
        // `Some(ServerConf::default())` instead of `None`.
        let mut server = pingora::server::Server::new(None).map_err(|error| {
            UnifiedError::internal(format!("Cannot create the Pingora server: {error}"))
        })?;
        server.bootstrap();

        // Plain-HTTP service.
        // UNCERTAIN (pingora-9): `http_proxy_service(&server.configuration, proxy)`
        // and `service.add_tcp(addr)`. Expected from the documented
        // Pingora load-balancer example: the first argument is the
        // server configuration, and `add_tcp` binds one plain
        // listener. If `server.configuration` is not public in the
        // resolved version, use the configuration the server was
        // built with.
        let mut plain_service = http_proxy_service(
            &server.configuration,
            GatewayProxy::new(state.clone(), false),
        );
        for addr in &listen_addrs {
            plain_service.add_tcp(addr.as_str());
        }
        server.add_service(plain_service);

        // TLS service, when TLS listen addresses are configured.
        if !tls_listen_addrs.is_empty() {
            let storage = state.tls_storage().ok_or_else(|| {
                UnifiedError::unprocessable(
                    "TLS storage is not attached: the HTTP server must attach the SslStorage handle before the gateway TLS listener can start",
                )
            })?;
            let server_config = crate::gateway::tls::build_server_config(&storage)?;
            // UNCERTAIN (pingora-10): TLS listener with a dynamic
            // certificate resolver. Expected shape:
            // `TlsSettings::new()` with the rustls `ServerConfig`
            // built from the shared `SslStorage`, attached with
            // `add_tls_with_settings(addr, settings)`. If the
            // resolved version has no `TlsSettings::new`, build it
            // with struct-update syntax from `Default::default()`;
            // if it has no `server_config` field, the static
            // fallback is `service.add_tls(addr, cert_path,
            // key_path)` per configured domain, which cannot do SNI
            // resolution and is therefore not used here.
            let mut tls_service = http_proxy_service(
                &server.configuration,
                GatewayProxy::new(state.clone(), true),
            );
            for addr in &tls_listen_addrs {
                // Built per address so no `Clone` bound on
                // `TlsSettings` is assumed.
                let mut tls_settings = pingora::listeners::tls::TlsSettings::new();
                tls_settings.server_config = Some(Arc::new(server_config.clone()));
                tls_service.add_tls_with_settings(addr.as_str(), tls_settings);
            }
            server.add_service(tls_service);
        }

        let started_at = Utc::now();
        let handle = GatewayServiceHandle {
            started_at,
            listen_addrs: listen_addrs.clone(),
            tls_listen_addrs: tls_listen_addrs.clone(),
        };
        let stored_handle = GatewayServiceHandle {
            started_at,
            listen_addrs: listen_addrs.clone(),
            tls_listen_addrs: tls_listen_addrs.clone(),
        };
        let state_for_thread = state.clone();
        let spawn = std::thread::Builder::new()
            .name("gateway-pingora".to_string())
            .spawn(move || {
                // UNCERTAIN (pingora-11): `Server::run_forever()`.
                // Expected to block the calling thread serving all
                // attached services until the process receives
                // SIGTERM / SIGINT, which Pingora handles as a
                // graceful shutdown.
                server.run_forever();
                log::info!("Gateway Pingora server thread exited.");
            });
        match spawn {
            Ok(_) => {
                state_for_thread
                    .running
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                state_for_thread
                    .ever_started
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                state_for_thread
                    .stop_requested
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                *state_for_thread
                    .started_at
                    .write()
                    .expect("gateway started_at lock poisoned") = Some(started_at);
                *state_for_thread
                    .service
                    .lock()
                    .expect("gateway service lock poisoned") = Some(stored_handle);
                state_for_thread.emit(
                    GATEWAY_STARTED,
                    json_value!({
                        "listen": listen_addrs,
                        "tls_listen": tls_listen_addrs,
                        "started_at": started_at,
                    }),
                );
                Ok(handle)
            }
            Err(error) => Err(UnifiedError::internal(format!(
                "Cannot start the gateway service thread: {error}"
            ))),
        }
    }

    /// Drain the gateway: new requests are rejected with 503 and
    /// in-flight requests finish. The Pingora listener thread keeps
    /// its sockets until the process exits — Pingora's graceful
    /// shutdown is signal-driven (SIGTERM / SIGINT) at the process
    /// level, and `run_forever` owns the `Server` value, so there is
    /// no in-process handle to close it with.
    ///
    /// UNCERTAIN (pingora-6): if the resolved Pingora version
    /// exposes a programmatic `Server::shutdown()` (or a shutdown
    /// handle from `Server::new`), `stop` should call it here
    /// instead of draining only, and `ever_started` can then allow
    /// a later `start` to rebind. Until that is verified, a stopped
    /// service cannot be started again in the same process.
    pub fn stop(state: Arc<GatewayState>) -> UnifiedResult<GatewayServiceHandle> {
        crate::gateway::require_feature_enabled()?;
        if !state.is_running() {
            return Err(UnifiedError::conflict("Gateway service is not running"));
        }
        state
            .stop_requested
            .store(true, std::sync::atomic::Ordering::SeqCst);
        state
            .running
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let handle = {
            let mut service = state
                .service
                .lock()
                .expect("gateway service lock poisoned");
            let handle = service.as_ref().map(|handle| GatewayServiceHandle {
                started_at: handle.started_at,
                listen_addrs: handle.listen_addrs.clone(),
                tls_listen_addrs: handle.tls_listen_addrs.clone(),
            });
            // The listener thread still owns the sockets until the
            // process exits, but the handle is cleared so the
            // management API no longer reports a live service.
            *service = None;
            handle
        };
        let stopped_at = Utc::now();
        state.emit(
            GATEWAY_STOPPED,
            json_value!({
                "stopped_at": stopped_at,
                "draining": true,
                "note": "New requests are rejected with 503; the listener thread exits with the process (Pingora shutdown is signal-driven). Restart the daemon to serve again.",
            }),
        );
        match handle {
            Some(handle) => Ok(handle),
            None => Err(UnifiedError::internal(
                "Gateway service is running but its handle is missing",
            )),
        }
    }

    /// Reload the gateway without restarting the listener: routing
    /// rules are read live from the store on every request, so a
    /// reload re-validates the configuration and the routing rules
    /// and reports the TLS status for the configured names. Returns
    /// the list of validation problems (empty means the reload
    /// applied cleanly).
    pub fn reload(state: Arc<GatewayState>) -> UnifiedResult<Vec<String>> {
        crate::gateway::require_feature_enabled()?;
        let config = state.config_snapshot();
        let mut problems = config.validate();
        for rule in crate::gateway::routing::list_rules() {
            if rule.enabled && rule.path_regex.is_some() && rule.compiled_path_regex.is_none() {
                problems.push(format!(
                    "Routing rule \"{}\" has a path regex that did not compile",
                    rule.name
                ));
            }
        }
        let app_config = state.app_config_snapshot();
        if let Some(app_config) = app_config {
            let storage = state.tls_storage();
            // Check the SNI names the rules actually serve (concrete
            // hosts only; wildcards are checked without the `*.`
            // prefix). TLS listen addresses are sockets, not server
            // names, so they are not checked here.
            let names: Vec<String> = crate::gateway::routing::list_rules()
                .iter()
                .filter(|rule| rule.enabled)
                .filter_map(|rule| rule.host.as_ref())
                .map(|host| host.strip_prefix("*.").unwrap_or(host).to_string())
                .collect::<std::collections::HashSet<String>>()
                .into_iter()
                .collect();
            let tls_problems = crate::gateway::tls::check_tls_listen_names(
                storage.as_deref(),
                &names,
                &app_config,
            );
            problems.extend(tls_problems);
        }
        state.emit(
            crate::gateway::GATEWAY_RELOADED,
            json_value!({
                "problems": problems,
                "rule_count": crate::gateway::routing::rule_count(),
                "reloaded_at": Utc::now(),
            }),
        );
        Ok(problems)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::routing::{
        CreateRuleRequest, PoolAddress, RoutingRule as Rule, UpstreamRef as UpRef,
    };
    use crate::gateway::GatewayState;
    use crate::modules::load_balancer::{HealthCheckConfig, Strategy};

    fn direct_pool_rule() -> Rule {
        let mut rule = crate::gateway::routing::create_rule(CreateRuleRequest {
            name: "pool-test".to_string(),
            priority: None,
            host: None,
            path_prefix: None,
            path_regex: None,
            headers: None,
            upstream: UpRef::AddressPool {
                addresses: vec![
                    PoolAddress {
                        address: "127.0.0.1:8080".to_string(),
                        weight: 3,
                    },
                    PoolAddress {
                        address: "127.0.0.1:8081".to_string(),
                        weight: 1,
                    },
                ],
            },
            strategy: Some(Strategy::Weighted),
            require_tls: None,
            rate_limit_override: None,
            health_check: Some(HealthCheckConfig::default()),
        })
        .expect("test rule is valid");
        rule.id = Uuid::now_v7();
        rule
    }

    #[test]
    fn weighted_pool_selection_follows_weights() {
        let state = GatewayState::new();
        let rule = direct_pool_rule();
        let mut picked: Vec<String> = Vec::new();
        for _ in 0..4 {
            let choice = choose_upstream(&state, &rule, None).expect("an upstream is picked");
            picked.push(choice.address);
        }
        assert_eq!(
            picked,
            vec![
                "127.0.0.1:8080".to_string(),
                "127.0.0.1:8080".to_string(),
                "127.0.0.1:8080".to_string(),
                "127.0.0.1:8081".to_string(),
            ]
        );
    }

    #[test]
    fn unhealthy_direct_address_is_excluded() {
        let state = GatewayState::new();
        let rule = direct_pool_rule();
        crate::gateway::health::mark_unhealthy(
            &state,
            &address_health_key("127.0.0.1:8080"),
            "127.0.0.1:8080",
        );
        for _ in 0..4 {
            let choice = choose_upstream(&state, &rule, None).expect("an upstream is picked");
            assert_eq!(choice.address, "127.0.0.1:8081");
        }
    }

    #[test]
    fn empty_balancer_pool_is_a_503_not_a_panic() {
        let state = GatewayState::new();
        let now = Utc::now();
        let rule = Rule {
            id: Uuid::now_v7(),
            name: "empty".to_string(),
            priority: 100,
            enabled: true,
            host: None,
            path_prefix: None,
            path_regex: None,
            headers: Vec::new(),
            upstream: UpRef::Balancer {
                balancer_id: Uuid::now_v7(),
            },
            strategy: Strategy::RoundRobin,
            require_tls: false,
            rate_limit_override: None,
            health_check: HealthCheckConfig::default(),
            created_at: now,
            updated_at: now,
            compiled_path_regex: None,
        };
        let error = choose_upstream(&state, &rule, None).expect_err("no backend exists");
        assert_eq!(error.code, 503);
    }
}
