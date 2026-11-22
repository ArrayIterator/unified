// HTTP server: Actix-web application factory, listeners, middleware.
//
// Builds the application from the shared context: module routes are
// registered through the module registry, `/health` is public, and a
// pause / maintenance middleware returns 503 for every other route
// while the daemon is paused or in maintenance mode. Plain TCP listeners
// come from `app.tcp`; TLS listeners come from `app.ssl.listen` and use
// the TLS configuration built from `crate::http::tls::SslStorage`.
// The server runs until Ctrl-C or SIGTERM, then shuts down gracefully.

use crate::cli::daemon::update_status_file;
use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus};
use crate::core::json;
use crate::http::acme::AcmeClient;
use crate::http::tls::SslStorage;
use crate::modules::network::connection_stats;
use actix_web::dev::Service;
use actix_web::http::KeepAlive;
use actix_web::{web, App, HttpResponse, HttpServer};
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
        let config = crate::http::tls::server_config(&storage)?;
        (Some(config), Some(storage))
    };

    let factory_ctx = ctx.clone();
    let factory_counter = total_requests.clone();
    let mut server = HttpServer::new(move || {
        let ctx = factory_ctx.clone();
        let counter = factory_counter.clone();
        let mw_ctx = ctx.clone();
        let mw_counter = counter.clone();
        App::new()
            .app_data(web::Data::new(ctx.clone()))
            .wrap_fn(move |req, srv| {
                let ctx = mw_ctx.clone();
                let counter = mw_counter.clone();
                let method = req.method().to_string();
                let path = req.path().to_string();
                let debug = ctx.is_debug();
                let blocked = path != "/health"
                    && !path.starts_with("/.well-known/acme-challenge/")
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

/// `GET /health`: public liveness probe. Always available, even while
/// the daemon is paused or in maintenance mode.
async fn health(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    json::success_response(
        serde_json::json!({ "status": "ok", "uptime_seconds": ctx.uptime_seconds() }),
        200,
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
