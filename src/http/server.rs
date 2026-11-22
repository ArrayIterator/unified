// HTTP server: Actix-web application factory, listeners, middleware.
//
// Builds the application from the shared context: module routes are
// registered through the module registry, `/health` is public, and a
// pause / maintenance middleware returns 503 for every other route
// while the daemon is paused or in maintenance mode. Plain TCP listeners
// come from `app.tcp`; TLS listeners come from `app.ssl.listen` and use
// the TLS configuration built from `crate::http::tls::SslStorage`
// through the `crate::http::cert_swap::CertStoreSwap` holder, so
// per-certificate swaps (`ssl.swap`) take effect on new handshakes
// while a whole-store swap only reaches ServerConfigs built after
// it (the running listener keeps its captured store — see the wiring
// comment at the TLS branch).
// The server runs until Ctrl-C or SIGTERM, then shuts down gracefully.

use crate::cli::daemon::update_status_file;
use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hardening::{
    HardeningState, IpFilterMiddleware, RateLimitMiddleware, RequestSizeMiddleware,
    SecurityHeadersMiddleware, SecuritySettings,
};
use crate::core::hooks::{events, HookBus};
use crate::core::json;
use crate::core::middleware::{
    MiddlewareManager, TenantResolutionConfig, TenantResolutionMiddleware,
};
use crate::http::acme::AcmeClient;
use crate::http::tls::SslStorage;
use crate::modules::network::connection_stats;
use crate::themes::assets::{serve_asset, serve_theme_asset, StaticAssetConfig};
use actix_web::dev::Service;
use actix_web::http::{header, KeepAlive};
use actix_web::{web, App, HttpRequest, HttpResponse, HttpServer};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Run the HTTP (and, when configured, HTTPS) server until a shutdown
/// signal arrives. Called by the `start` command.
pub async fn run_server(ctx: Arc<AppContext>) -> UnifiedResult<()> {
    let total_requests = Arc::new(AtomicU64::new(0));
    let (tcp_addrs, tls_addrs, workers, backlog, keep_alive, max_connections,
        max_connection_rate, shutdown_timeout) = {
        let config = ctx.config.read().expect("config lock poisoned");
        (
            config.app.tcp.clone(),
            config.app.ssl.listen.clone(),
            config.app.workers,
            config.app.backlog,
            config.app.keep_alive,
            config.app.max_connections,
            config.app.max_connections_rate,
            config.app.disconnect_timeout,
        )
    };
    if tcp_addrs.is_empty() && tls_addrs.is_empty() {
        return Err(UnifiedError::internal(
            "No listen addresses configured (app.tcp and app.ssl.listen are both empty)",
        ));
    }

    // Security hardening: build the shared enforcement state from the
    // `security` config section (defaults apply when the section is
    // absent) and install it, so the hardening middleware built in the
    // factory below and the Security module handlers share one state:
    // settings updates, lockouts, and IP lists are visible on both
    // sides immediately.
    {
        let settings = {
            let config = ctx.config.read().expect("config lock poisoned");
            SecuritySettings::from_app_config(&config)
        };
        crate::core::hardening::install_shared_state(Arc::new(HardeningState::new(
            settings,
            ctx.hooks.clone(),
        )));
    }

    // Build the TLS configuration once, before binding, so a bad
    // certificate fails startup instead of failing a single listener.
    // The storage itself is kept in an `Arc` alongside the config: the
    // ACME renewal listener (below) swaps renewed certificates into
    // this same storage, and the TLS resolver holds a clone of it, so
    // renewals take effect live without a restart.
    let (tls_config, tls_storage) = if tls_addrs.is_empty() {
        (None, None)
    } else {
        let ssl = ctx
            .config
            .read()
            .expect("config lock poisoned")
            .app
            .ssl
            .clone();
        let storage = crate::http::tls::SslStorage::from_config(&ssl)?;
        // CertStoreSwap (batch 8): the swap holder is the single
        // place holding the current store. The TLS config is built
        // from the holder (`server_config_from_swap`), and the
        // `ssl.swap` request handler is registered on the hook bus,
        // so per-certificate swaps take effect on new handshakes
        // (the storage's DashMaps are read every handshake) and a
        // whole-store swap takes effect for every ServerConfig
        // built after it. Honest limit: the running listener keeps
        // the store Arc its config captured at bind time, so a
        // whole-store swap alone does NOT change live traffic —
        // there is no listener rebuild. Per-certificate swaps
        // (`swap_certificate`) are the live path; a whole-store
        // swap needs a restart (or a future rebuild that calls
        // `server_config_from_swap` again) to reach the listener.
        let swap = Arc::new(crate::http::cert_swap::CertStoreSwap::new(storage.clone()));
        let config = crate::http::cert_swap::server_config_from_swap(&swap)?;
        crate::http::cert_swap::register_cert_swap_hooks(&ctx.hooks, swap.clone());
        (Some(config), Some(storage))
    };

    // Attach the one TLS storage to the application context (and to
    // the gateway's process-wide state when the gateway feature is
    // compiled in) so the HTTPS listener, the load balancer TLS
    // endpoint, and the gateway all resolve certificates through this
    // same storage. Renewals swap certificates into this storage and
    // take effect live. The certificates module still builds its own
    // storage in `seed_from_config` until that function is changed to
    // take this shared handle (its file is owned by another builder);
    // until then the load balancer endpoint must also switch from its
    // config-only lookup to `ctx.tls` (same owner) before the
    // unification is complete.
    if let Some(storage) = &tls_storage {
        ctx.set_tls(storage.clone());
        #[cfg(feature = "gateway")]
        crate::gateway::set_tls_storage(storage.clone());
    }

    let factory_ctx = ctx.clone();
    let factory_counter = total_requests.clone();
    let mut server = HttpServer::new(move || {
        let ctx = factory_ctx.clone();
        let counter = factory_counter.clone();
        let mw_ctx = ctx.clone();
        let mw_counter = counter.clone();
        // Collect the middleware contributed by every module, ordered
        // by priority (highest first) when the dispatcher is built.
        // An empty pipeline passes requests through unchanged.
        let mut middleware_manager = MiddlewareManager::new();
        ctx.modules.register_all_middleware(&mut middleware_manager);
        // Security hardening middleware, ordered by priority (highest
        // first): security headers (1000) -> tenant resolution (950)
        // -> IP blocklist (900) -> rate limiting (800) ->
        // request-size guard (700) -> module middleware. The
        // dispatcher sorts by priority, so this order holds
        // regardless of registration order. Tenant resolution runs
        // before the IP filter and rate limiter so routing is decided
        // before any route dispatch, and after the security headers so
        // its short-circuit responses still carry them.
        // The ACME HTTP-01 challenge route is exempt from the
        // blocklist and rate limiting inside the middleware: a blocked
        // or throttled challenge would break certificate issuance and
        // renewal, which must succeed even while the daemon is under
        // attack or in maintenance; the challenge token itself is
        // unguessable and single-use, so the exemption does not open
        // an abuse vector.
        let hardening = crate::core::hardening::shared_state();
        middleware_manager.register(SecurityHeadersMiddleware::new(hardening.clone()));
        // Tenant resolution (batch 8): resolves the Host header to a
        // tenant through the `tenant.resolve_requested` hook, attaches
        // the tenant context, and blocks control-plane paths on tenant
        // hosts. Its config (main host, blocked paths, tenant header,
        // fallback policies) is built from the environment mapped to
        // the `saas.*` config keys — see `TenantResolutionConfig` —
        // and this is the single construction site: when the typed
        // `[saas]` config section lands, it plugs in here.
        middleware_manager.register(TenantResolutionMiddleware::new(
            ctx.clone(),
            TenantResolutionConfig::from_environment(),
        ));
        middleware_manager.register(IpFilterMiddleware::new(hardening.clone()));
        middleware_manager.register(RateLimitMiddleware::new(hardening.clone()));
        middleware_manager.register(RequestSizeMiddleware::new(hardening.clone()));
        let dispatcher = middleware_manager.create_dispatcher();
        let debug = ctx.is_debug();
        App::new()
            .app_data(web::Data::new(ctx.clone()))
            // The dispatcher wraps the application inside the
            // request-lifecycle middleware below, so pause /
            // maintenance blocking and the request hooks always run
            // first and module middleware never runs for a blocked
            // request.
            .wrap(dispatcher)
            .wrap_fn(move |req, srv| {
                let ctx = mw_ctx.clone();
                let counter = mw_counter.clone();
                let method = req.method().to_string();
                let path = req.path().to_string();
                let debug = ctx.is_debug();
                let blocked = path != "/health"
                    && !path.starts_with("/.well-known/acme-challenge/")
                    && !is_status_exempt(&path)
                    && (ctx.is_paused() || ctx.is_maintenance());
                let maintenance = ctx.is_maintenance();
                let started = Instant::now();
                ctx.hooks.emit(
                    events::REQUEST_BEFORE,
                    serde_json::json!({ "method": method, "path": path }),
                );
                counter.fetch_add(1, Ordering::SeqCst);
                async move {
                    if blocked {
                        let error = UnifiedError::new(
                            if maintenance {
                                "Service is in maintenance mode"
                            } else {
                                "Service is paused"
                            },
                            503,
                        );
                        let response = json::error_response(&error, debug);
                        let elapsed = started.elapsed().as_millis() as u64;
                        ctx.hooks.emit(
                            events::REQUEST_AFTER,
                            serde_json::json!({
                                "method": method, "path": path,
                                "status": 503, "duration_ms": elapsed,
                            }),
                        );
                        log::info!("{method} {path} -> 503 ({elapsed}ms)");
                        return Ok(req.into_response(response));
                    }
                    let res = srv.call(req).await?;
                    let status = res.status().as_u16();
                    let elapsed = started.elapsed().as_millis() as u64;
                    ctx.hooks.emit(
                        events::REQUEST_AFTER,
                        serde_json::json!({
                            "method": method, "path": path,
                            "status": status, "duration_ms": elapsed,
                        }),
                    );
                    log::info!("{method} {path} -> {status} ({elapsed}ms)");
                    Ok(res)
                }
            })
            .configure(move |cfg| {
                cfg.route("/health", web::get().to(health));
                cfg.route(
                    "/.well-known/acme-challenge/{token}",
                    web::get().to(acme_challenge),
                );
                cfg.route("/ws", web::get().to(crate::http::websocket::ws_handler));
                // Static assets: the global `assets/` directory under
                // `/assets/`, and one theme's `assets/` directory under
                // `/theme/{id}/assets/`. Both serve fingerprinted URLs
                // (`?v=<hash>`) with immutable cache headers.
                cfg.app_data(web::Data::new(StaticAssetConfig {
                    debug,
                    ..StaticAssetConfig::default()
                }));
                cfg.route("/assets/{path:.*}", web::get().to(serve_asset));
                cfg.route(
                    "/theme/{id}/assets/{path:.*}",
                    web::get().to(serve_theme_asset),
                );
                // Unmatched routes answer with the standard error
                // envelope instead of Actix's default plain 404.
                cfg.default_service(web::to(not_found));
                ctx.modules.configure_all_routes(cfg);
            })
    });

    if workers > 0 {
        server = server.workers(workers);
    }
    server = server
        .backlog(backlog as i32)
        .keep_alive(if keep_alive == 0 {
            KeepAlive::Disabled
        } else {
            KeepAlive::Timeout(Duration::from_secs(keep_alive))
        })
        .max_connections(max_connections)
        .shutdown_timeout(shutdown_timeout);
    if max_connection_rate > 0 {
        server = server.max_connection_rate(max_connection_rate);
    }
    for addr in &tcp_addrs {
        server = server.bind(addr.as_str()).map_err(|error| {
            UnifiedError::internal(format!("Cannot bind TCP listener {addr}: {error}"))
        })?;
    }
    if let Some(tls) = &tls_config {
        for addr in &tls_addrs {
            server = server.bind_rustls_0_23(addr.as_str(), tls.clone()).map_err(|error| {
                UnifiedError::internal(format!("Cannot bind TLS listener {addr}: {error}"))
            })?;
        }
    }
    log::info!("Listening on {} (HTTP).", tcp_addrs.join(", "));
    if !tls_addrs.is_empty() {
        log::info!("Listening on {} (HTTPS).", tls_addrs.join(", "));
    }

    // ACME renewal wiring: when `acme.enabled` is set, construct one
    // `AcmeClient` and listen for `ssl.renew` so certificates near
    // expiry are renewed and swapped into the live TLS storage. The
    // client is built in a background task (account creation needs
    // network I/O) so a slow or failing ACME directory never blocks
    // startup; a failure only disables automatic renewal.
    if let Some(storage) = tls_storage {
        let acme_enabled = ctx
            .config
            .read()
            .expect("config lock poisoned")
            .acme
            .enabled;
        if acme_enabled {
            let (directory_url, contacts) = {
                let config = ctx.config.read().expect("config lock poisoned");
                (
                    config.acme.directory_url.clone(),
                    config.acme.contacts.clone(),
                )
            };
            let renew_ctx = ctx.clone();
            tokio::spawn(async move {
                match AcmeClient::new(&directory_url, &contacts).await {
                    Ok(client) => {
                        let client = Arc::new(client);
                        let hooks = renew_ctx.hooks.clone();
                        let listener_hooks = hooks.clone();
                        hooks.register(
                            events::SSL_RENEW,
                            "acme",
                            Arc::new(move |context: &mut crate::core::hooks::HookContext| {
                                let Some(domain) = context
                                    .get("domain")
                                    .and_then(|value| value.as_str())
                                    .map(str::to_string)
                                else {
                                    return;
                                };
                                // Ignore completion payloads (a renewal
                                // that already reports an expiry) so one
                                // renewal is not re-run.
                                if context.get("ssl_expires_at").is_some()
                                    || context.get("completed").is_some()
                                {
                                    return;
                                }
                                let client = client.clone();
                                let storage = storage.clone();
                                let hooks = listener_hooks.clone();
                                if tokio::runtime::Handle::try_current().is_ok() {
                                    tokio::spawn(async move {
                                        renew_one(client, storage, hooks, domain).await;
                                    });
                                } else {
                                    std::thread::spawn(move || {
                                        let Ok(runtime) =
                                            tokio::runtime::Builder::new_current_thread()
                                                .enable_all()
                                                .build()
                                        else {
                                            return;
                                        };
                                        runtime.block_on(renew_one(
                                            client, storage, hooks, domain,
                                        ));
                                    });
                                }
                            }),
                        );
                        log::info!(
                            "ACME client ready (directory {directory_url}); listening for ssl.renew."
                        );
                    }
                    Err(error) => {
                        log::error!(
                            "ACME client could not be created ({error}); automatic renewal is disabled."
                        );
                    }
                }
            });
        }
    }

    // Publish live counters to the daemon status file, first now and
    // then every 5 seconds for the lifetime of the server.
    publish_status(&ctx, &total_requests);
    let status_ctx = ctx.clone();
    let status_counter = total_requests.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            publish_status(&status_ctx, &status_counter);
        }
    });

    let running = server.run();
    let handle = running.handle();
    tokio::pin!(running);
    tokio::select! {
        result = &mut running => {
            return result.map_err(|error| {
                UnifiedError::internal(format!("HTTP server failed: {error}"))
            });
        }
        () = shutdown_signal() => {
            log::info!("Shutdown signal received; stopping the HTTP server.");
            handle.stop(true).await;
        }
    }
    running.await.map_err(|error| {
        UnifiedError::internal(format!("HTTP server failed: {error}"))
    })?;
    ctx.hooks.emit(
        events::APP_STOP,
        serde_json::json!({ "status": "stopped" }),
    );
    Ok(())
}

/// Paths that stay available while the daemon is paused or in
/// maintenance mode: the public status page (`/status`) and the
/// public statuspage API (`/api/v1/statuspage` and everything under
/// it). The status page is the channel that tells operators and
/// visitors the service is down; blocking it during maintenance
/// would hide exactly the information it exists to publish. The
/// exemption covers pause as well as maintenance, the same as the
/// `/health` probe and the ACME challenge above it.
fn is_status_exempt(path: &str) -> bool {
    path == "/status"
        || path.starts_with("/status/")
        || path == "/api/v1/statuspage"
        || path.starts_with("/api/v1/statuspage/")
}

/// `GET /health`: public liveness probe. Always available, even while
/// the daemon is paused or in maintenance mode.
async fn health(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    json::success_response(
        serde_json::json!({ "status": "ok", "uptime_seconds": ctx.uptime_seconds() }),
        200,
    )
}

/// Default service for unmatched routes: the standard error envelope
/// with a 404, instead of Actix's default plain-text 404. When the
/// path is a recorded old path of a renamed named route, answer with
/// a permanent (301) redirect to the route's current effective path
/// instead: the route registry keeps at most one redirect per route
/// (latest-only), so a redirect target is always current and
/// redirects never chain.
async fn not_found(ctx: web::Data<Arc<AppContext>>, req: HttpRequest) -> HttpResponse {
    if let Some(target) = ctx.routes.resolve_redirect(req.path()) {
        return HttpResponse::MovedPermanently()
            .insert_header((header::LOCATION, target))
            .finish();
    }
    json::error_response(
        &UnifiedError::not_found("Route not found"),
        ctx.is_debug(),
    )
}

/// `GET /.well-known/acme-challenge/{token}`: answer an ACME HTTP-01
/// challenge from the shared challenge store populated by
/// `AcmeClient::order_certificate`. The key authorization is returned
/// as plain text; an unknown token is a 404 in the standard error
/// envelope. This route stays available during pause / maintenance so
/// a renewal is never blocked by daemon state.
async fn acme_challenge(token: web::Path<String>) -> HttpResponse {
    let token = token.into_inner();
    let found = crate::http::acme::challenge_store()
        .read()
        .ok()
        .and_then(|store| store.get(&token).cloned());
    match found {
        Some(authorization) => HttpResponse::Ok()
            .content_type("text/plain")
            .body(authorization),
        None => json::error_response(
            &UnifiedError::not_found(format!("Unknown ACME challenge token \"{token}\"")),
            false,
        ),
    }
}

/// Renew one domain's certificate when it expires within 30 days, and
/// swap the renewed certificate into the live TLS storage. On success
/// an `ssl.swap` event is emitted (domain + new expiry) so the Domains
/// module can update its SSL status; failures are only logged.
async fn renew_one(
    client: Arc<AcmeClient>,
    storage: Arc<SslStorage>,
    hooks: Arc<HookBus>,
    domain: String,
) {
    match client.renew_if_expiring(&storage, &domain, 30).await {
        Ok(true) => {
            let expires_at = storage
                .cert_expiry(&domain)
                .map(|expiry| expiry.to_rfc3339());
            log::info!("ACME certificate renewed for {domain}");
            hooks.emit(
                events::SSL_SWAP,
                serde_json::json!({
                    "domain": domain,
                    "ssl_expires_at": expires_at,
                    "renewed": true,
                }),
            );
        }
        Ok(false) => {
            log::debug!("ACME certificate for {domain} is not near expiry; no renewal.");
        }
        Err(error) => {
            log::error!("ACME renewal failed for {domain}: {error}");
        }
    }
}

/// Write the live counters (uptime, pause / maintenance state, active
/// HTTP and WebSocket connections, total requests) to the daemon status
/// file. Does nothing when no status file exists.
fn publish_status(ctx: &AppContext, total_requests: &AtomicU64) {
    let stats = connection_stats();
    let total = total_requests.load(Ordering::SeqCst);
    let _ = update_status_file(|status| {
        status.uptime_seconds = ctx.uptime_seconds();
        status.paused = ctx.is_paused();
        status.maintenance = ctx.is_maintenance();
        status.http_connections = stats.http.load(Ordering::SeqCst);
        status.websocket_connections = stats.websocket.load(Ordering::SeqCst);
        status.total_requests = total;
    });
}

/// Resolve when Ctrl-C or SIGTERM arrives.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                let _ = signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}
