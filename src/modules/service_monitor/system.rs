// service_monitor module -- System snapshot, service probes, and monitor endpoints.

use unified_core::config::AppConfig;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus};
use unified_core::json::{error_response, success_response};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::Serialize;
use serde_json::json;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use sysinfo::{Disks, System};

use super::*;

/// Last probed status per service id, so only the up-to-down
/// transition emits, not every probe that finds the service down.
pub(crate) static LAST_SERVICE_STATUS: OnceLock<DashMap<&'static str, String>> = OnceLock::new();

pub(crate) fn last_service_status_store() -> &'static DashMap<&'static str, String> {
    LAST_SERVICE_STATUS.get_or_init(DashMap::new)
}

/// Emit `cluster.health.changed` for every service that just
/// transitioned to `down` (its previous probe was not `down`).
/// `disabled` and `unavailable` are not failures and never emit.
/// The remembered status is updated for every service, including
/// recoveries, so a later outage alerts again.
pub(crate) fn emit_service_down_events(
    hooks: &HookBus,
    services: &[ServiceStatus],
    checked_at: DateTime<Utc>,
) {
    let store = last_service_status_store();
    for service in services {
        let previous = store
            .get(service.id)
            .map(|entry| entry.value().clone());
        if service.status == "down" && previous.as_deref() != Some("down") {
            hooks.emit(
                SERVICE_HEALTH_CHANGED_EVENT,
                json!({
                    "node_id": service.id,
                    "cluster_id": "local",
                    "previous_health": previous.clone().unwrap_or_else(|| "unknown".to_string()),
                    "health": "down",
                    "consecutive_failures": 1,
                    "service_id": service.id,
                    "service_name": service.name,
                    "status": service.status,
                    "detail": service.detail,
                    "latency_ms": service.latency_ms,
                    "checked_at": checked_at,
                }),
            );
        }
        store.insert(service.id, service.status.clone());
    }
}

/// Process / server status returned by `GET /api/v1/service-monitor/status`.
pub(crate) fn api_process_info(process: &sysinfo::Process) -> ApiProcessInfo {
    let disk_usage = process.disk_usage();
    ApiProcessInfo {
        pid: process.pid().as_u32(),
        name: process.name().to_string_lossy().into_owned(),
        status: process_status_label(process.status()).to_string(),
        user_id: process.user_id().map(|uid| uid.to_string()),
        parent_pid: process.parent().map(|pid| pid.as_u32()),
        cpu_usage_percent: process.cpu_usage(),
        memory_bytes: process.memory(),
        virtual_memory_bytes: process.virtual_memory(),
        disk_read_bytes: disk_usage.total_read_bytes,
        disk_written_bytes: disk_usage.total_written_bytes,
        run_time_seconds: process.run_time(),
    }
}

pub(crate) fn collect_snapshot(system: &System, disks: &Disks) -> SystemSnapshot {
    let load = System::load_average();
    let first_cpu = system.cpus().first();
    let pid = std::process::id();
    let process = system
        .processes()
        .values()
        .find(|process| process.pid().as_u32() == pid)
        .map(|process| OwnProcessInfo {
            memory_bytes: process.memory(),
            virtual_memory_bytes: process.virtual_memory(),
            cpu_usage_percent: process.cpu_usage(),
            status: process_status_label(process.status()).to_string(),
            run_time_seconds: process.run_time(),
        });
    let mut processes: Vec<ApiProcessInfo> = system
        .processes()
        .values()
        .map(api_process_info)
        .collect();
    processes.sort_by(|a, b| {
        b.cpu_usage_percent
            .partial_cmp(&a.cpu_usage_percent)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.memory_bytes.cmp(&a.memory_bytes))
    });
    SystemSnapshot {
        total_memory_bytes: system.total_memory(),
        used_memory_bytes: system.used_memory(),
        free_memory_bytes: system.free_memory(),
        available_memory_bytes: system.available_memory(),
        total_swap_bytes: system.total_swap(),
        used_swap_bytes: system.used_swap(),
        free_swap_bytes: system.free_swap(),
        global_cpu_usage_percent: system.global_cpu_usage(),
        cpu_brand: first_cpu.map(|cpu| cpu.brand().to_string()),
        cpu_frequency_mhz: first_cpu.map(|cpu| cpu.frequency()),
        cpu_vendor: first_cpu.map(|cpu| cpu.vendor_id().to_string()),
        physical_cores: system.physical_core_count(),
        logical_cores: system.cpus().len(),
        load_average: LoadAverage {
            one_minute: load.one,
            five_minutes: load.five,
            fifteen_minutes: load.fifteen,
        },
        process,
        disks: disks
            .list()
            .iter()
            .map(|disk| DiskInfo {
                name: disk.name().to_string_lossy().to_string(),
                mount_point: disk.mount_point().to_string_lossy().to_string(),
                file_system: disk.file_system().to_string_lossy().to_string(),
                total_space_bytes: disk.total_space(),
                available_space_bytes: disk.available_space(),
                removable: disk.is_removable(),
                kind: disk_kind_label(disk.kind()).to_string(),
                // sysinfo no longer reports per-disk I/O counters.
                io_read_bytes: 0,
                io_written_bytes: 0,
            })
            .collect(),
        processes,
    }
}

pub(crate) fn snapshot_system() -> SystemSnapshot {
    let mut cache = SYSTEM_CACHE.lock().expect("system snapshot lock poisoned");
    let due = cache
        .last_refresh
        .map(|last| last.elapsed() >= SNAPSHOT_REFRESH_INTERVAL)
        .unwrap_or(true);
    if due {
        cache.system.refresh_all();
        cache.disks.refresh();
        cache.snapshot = Some(collect_snapshot(&cache.system, &cache.disks));
        cache.last_refresh = Some(Instant::now());
    }
    cache
        .snapshot
        .clone()
        .expect("system snapshot collected on first call")
}

pub(crate) fn worker_count(ctx: &AppContext) -> usize {
    let configured = ctx
        .config
        .read()
        .expect("config lock poisoned")
        .app
        .workers;
    if configured > 0 {
        configured
    } else {
        std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1)
    }
}

pub(crate) async fn status(ctx: web::Data<Arc<AppContext>>, req: HttpRequest) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_SERVICE_MONITOR_VIEW) {
        return error_response(&error, ctx.is_debug());
    }
    let snapshot = snapshot_system();
    let (mode, paused, maintenance) = (
        ctx.config
            .read()
            .expect("config lock poisoned")
            .app
            .mode
            .clone(),
        ctx.is_paused(),
        ctx.is_maintenance(),
    );
    let overall = if maintenance {
        "maintenance"
    } else if paused {
        "paused"
    } else {
        "ok"
    };
    let process = snapshot.process.as_ref();
    success_response(
        MonitorStatus {
            status: overall.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            mode,
            uptime_seconds: ctx.uptime_seconds(),
            paused,
            maintenance,
            workers: worker_count(&ctx),
            total_memory_bytes: snapshot.total_memory_bytes,
            used_memory_bytes: snapshot.used_memory_bytes,
            process_memory_bytes: process.map(|own| own.memory_bytes).unwrap_or(0),
            cpu_count: snapshot.logical_cores,
            cpu_usage_percent: snapshot.global_cpu_usage_percent,
            total_swap_bytes: snapshot.total_swap_bytes,
            used_swap_bytes: snapshot.used_swap_bytes,
            load_average: snapshot.load_average,
            process_cpu_percent: process.map(|own| own.cpu_usage_percent).unwrap_or(0.0),
            process_virtual_memory_bytes: process
                .map(|own| own.virtual_memory_bytes)
                .unwrap_or(0),
            process_status: process
                .map(|own| own.status.clone())
                .unwrap_or_else(|| "unknown".to_string()),
            process_run_time_seconds: process.map(|own| own.run_time_seconds).unwrap_or(0),
            libraries: latest_library_summary(),
        },
        200,
    )
}

/// Top processes by CPU (default) or memory (`?sort=memory`),
/// paginated with the standard `PaginationParams` / `PageResult`
/// (`?page=` / `?per_page=`, default 20, max 100). The old
/// `?limit=` parameter is retired; `?per_page=` replaces it.
pub(crate) async fn list_processes(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_SERVICE_MONITOR_VIEW) {
        return error_response(&error, ctx.is_debug());
    }
    let params = query.into_inner();
    let snapshot = snapshot_system();
    let mut processes = snapshot.processes;
    if params.sort.as_deref() == Some("memory") {
        processes.sort_by(|a, b| b.memory_bytes.cmp(&a.memory_bytes));
    }
    let pagination = params.normalized();
    let total = processes.len() as u64;
    let page_items: Vec<ApiProcessInfo> = processes
        .into_iter()
        .skip(params.offset() as usize)
        .take(params.limit() as usize)
        .collect();
    success_response(PageResult::new(page_items, total, &pagination), 200)
}

/// Probe one TCP address and return (reachable, latency in milliseconds).
pub(crate) fn probe_tcp(address: &str, timeout: Duration) -> (bool, u64) {
    let started = Instant::now();
    let reachable = address
        .to_socket_addrs()
        .ok()
        .and_then(|mut addrs| addrs.next())
        .and_then(|socket_addr| {
            TcpStream::connect_timeout(&socket_addr, timeout).ok()
        })
        .is_some();
    (reachable, started.elapsed().as_millis() as u64)
}

pub(crate) fn probe_services(config: &AppConfig, db_present: bool) -> Vec<ServiceStatus> {
    let mut services = Vec::new();
    let timeout = Duration::from_millis(2000);

    // HTTP listener: the first configured TCP address.
    match config.app.tcp.first() {
        Some(address) => {
            let (reachable, latency) = probe_tcp(address, timeout);
            services.push(ServiceStatus {
                id: "http",
                name: "HTTP listener".to_string(),
                status: if reachable { "up" } else { "down" }.to_string(),
                latency_ms: Some(latency),
                detail: format!("Listening on {address}"),
            });
        }
        None => services.push(ServiceStatus {
            id: "http",
            name: "HTTP listener".to_string(),
            status: "disabled".to_string(),
            latency_ms: None,
            detail: "No TCP listen address configured".to_string(),
        }),
    }

    // TLS listener.
    let tls_configured =
        !config.app.ssl.cert.is_empty() && !config.app.ssl.key.is_empty();
    match config.app.ssl.listen.first() {
        Some(address) if tls_configured => {
            let (reachable, latency) = probe_tcp(address, timeout);
            services.push(ServiceStatus {
                id: "tls",
                name: "TLS listener".to_string(),
                status: if reachable { "up" } else { "down" }.to_string(),
                latency_ms: Some(latency),
                detail: format!("Listening on {address}"),
            });
        }
        Some(_) => services.push(ServiceStatus {
            id: "tls",
            name: "TLS listener".to_string(),
            status: "disabled".to_string(),
            latency_ms: None,
            detail: "TLS listen address configured but no certificate/key pair"
                .to_string(),
        }),
        None => services.push(ServiceStatus {
            id: "tls",
            name: "TLS listener".to_string(),
            status: "disabled".to_string(),
            latency_ms: None,
            detail: "No TLS listen address configured".to_string(),
        }),
    }

    // Database: reachable only when a pool is attached. A connection is
    // checked out by the caller through the pool; here we can only report
    // whether the pool exists. The live check runs in `list_services`.
    services.push(ServiceStatus {
        id: "database",
        name: "Database".to_string(),
        status: if db_present {
            "up"
        } else {
            "unavailable"
        }
        .to_string(),
        latency_ms: None,
        detail: if db_present {
            "Connection pool attached".to_string()
        } else {
            "No database pool attached".to_string()
        },
    });

    // Redis.
    if config.redis.unix_socket.is_empty() {
        let address = format!("{}:{}", config.redis.host, config.redis.port);
        let (reachable, latency) = probe_tcp(&address, timeout);
        services.push(ServiceStatus {
            id: "redis",
            name: "Redis".to_string(),
            status: if reachable { "up" } else { "down" }.to_string(),
            latency_ms: Some(latency),
            detail: format!("Redis at {address}"),
        });
    } else {
        let path = std::path::Path::new(&config.redis.unix_socket);
        services.push(ServiceStatus {
            id: "redis",
            name: "Redis".to_string(),
            status: if path.exists() { "up" } else { "down" }.to_string(),
            latency_ms: None,
            detail: format!("Redis unix socket {}", config.redis.unix_socket),
        });
    }

    // Geo database file.
    let geo_path = std::path::Path::new(&config.geo.database_path);
    services.push(ServiceStatus {
        id: "geo",
        name: "Geo database".to_string(),
        status: if geo_path.exists() { "up" } else { "down" }.to_string(),
        latency_ms: None,
        detail: format!("GeoLite database at {}", config.geo.database_path),
    });

    services
}

/// Probed service statuses, paginated with the standard
/// `PaginationParams` / `PageResult`. Probing always covers every
/// service (and the down-transition events fire for all of them);
/// only the returned page is sliced.
pub(crate) async fn list_services(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_SERVICE_MONITOR_VIEW) {
        return error_response(&error, ctx.is_debug());
    }
    let params = query.into_inner();
    let config = ctx.config.read().expect("config lock poisoned").clone();
    let db_present = ctx.db.read().expect("db lock poisoned").is_some();
    let debug = ctx.is_debug();
    // TCP probes block, so run them off the async worker threads.
    let probed = web::block(move || probe_services(&config, db_present)).await;
    match probed {
        Ok(mut services) => {
            // When a database pool is attached, verify a connection can
            // actually be checked out and record the real latency.
            if db_present {
                let pool = ctx
                    .db
                    .read()
                    .expect("db lock poisoned")
                    .clone()
                    .expect("database pool present");
                let started = Instant::now();
                let checkout = web::block(move || pool.get().map(|_| ()))
                    .await
                    .map_err(|error| {
                        UnifiedError::internal(format!("Database check failed: {error}"))
                    })
                    .and_then(|result| {
                        result.map_err(|error| {
                            UnifiedError::internal(format!(
                                "Database connection checkout failed: {error}"
                            ))
                        })
                    });
                if let Some(service) = services.iter_mut().find(|s| s.id == "database") {
                    match checkout {
                        Ok(()) => {
                            service.status = "up".to_string();
                            service.latency_ms =
                                Some(started.elapsed().as_millis() as u64);
                            service.detail = "Connection checkout succeeded".to_string();
                        }
                        Err(error) => {
                            service.status = "down".to_string();
                            service.latency_ms =
                                Some(started.elapsed().as_millis() as u64);
                            service.detail = error.message.clone();
                        }
                    }
                }
            }
            emit_service_down_events(&ctx.hooks, &services, Utc::now());
            let pagination = params.normalized();
            let total = services.len() as u64;
            let page_items: Vec<ServiceStatus> = services
                .into_iter()
                .skip(params.offset() as usize)
                .take(params.limit() as usize)
                .collect();
            success_response(PageResult::new(page_items, total, &pagination), 200)
        }
        Err(error) => error_response(
            &UnifiedError::internal(format!("Service probe failed: {error}")),
            debug,
        ),
    }
}

pub(crate) async fn reload(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<ReloadRequest>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_SERVICE_MONITOR_MANAGE) {
        return error_response(&error, ctx.is_debug());
    }
    let debug = ctx.is_debug();
    let loaded = AppConfig::load_default();
    let new_config = match loaded {
        Ok(config) => config,
        Err(error) => return error_response(&error, debug),
    };
    let problems = new_config.validate();

    if body.dry_run {
        return success_response(
            serde_json::json!({
                "dry_run": true,
                "valid": problems.is_empty(),
                "problems": problems,
            }),
            200,
        );
    }

    if !problems.is_empty() {
        return error_response(
            &UnifiedError::bad_request("Configuration validation failed")
                .with_metadata("problems", serde_json::json!(problems)),
            debug,
        );
    }

    match ctx.reload_config(new_config) {
        Ok(()) => {
            ctx.hooks.emit(
                events::APP_RELOAD,
                serde_json::json!({ "dry_run": false, "status": "reloaded" }),
            );
            success_response(
                serde_json::json!({ "reloaded": true, "problems": [] }),
                200,
            )
        }
        Err(error) => error_response(&error, debug),
    }
}


pub(crate) async fn info(ctx: web::Data<Arc<AppContext>>, req: HttpRequest) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_SERVICE_MONITOR_VIEW) {
        return error_response(&error, ctx.is_debug());
    }
    let snapshot = snapshot_system();
    success_response(
        ServerInfo {
            os: System::long_os_version(),
            kernel_version: System::kernel_version(),
            hostname: System::host_name(),
            uptime_seconds: System::uptime(),
            boot_time_seconds: System::boot_time(),
            cpu_brand: snapshot.cpu_brand,
            cpu_frequency_mhz: snapshot.cpu_frequency_mhz,
            cpu_vendor: snapshot.cpu_vendor,
            physical_cores: snapshot.physical_cores,
            logical_cores: snapshot.logical_cores,
            load_average: snapshot.load_average,
            total_memory_bytes: snapshot.total_memory_bytes,
            used_memory_bytes: snapshot.used_memory_bytes,
            free_memory_bytes: snapshot.free_memory_bytes,
            available_memory_bytes: snapshot.available_memory_bytes,
            total_swap_bytes: snapshot.total_swap_bytes,
            used_swap_bytes: snapshot.used_swap_bytes,
            free_swap_bytes: snapshot.free_swap_bytes,
            disks: snapshot.disks,
        },
        200,
    )
}

/// Parse a `host:port` string into a socket address. Used by the service
/// probes; kept public for tests of other modules.
#[allow(dead_code)]
pub fn parse_listen_address(address: &str) -> UnifiedResult<SocketAddr> {
    address.parse::<SocketAddr>().map_err(|_| {
        UnifiedError::bad_request(format!("Invalid listen address: {address}"))
    })
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

    fn data(ctx: AppContext) -> web::Data<Arc<AppContext>> {
        web::Data::new(Arc::new(ctx))
    }

    #[test]
    fn parse_listen_address_accepts_socket_addr() {
        let addr = parse_listen_address("127.0.0.1:8080").expect("valid address");
        assert_eq!(addr.port(), 8080);
    }

    #[test]
    fn parse_listen_address_rejects_bad_input() {
        assert!(parse_listen_address("not-an-address").is_err());
        assert!(parse_listen_address("127.0.0.1").is_err());
    }

    #[tokio::test]
    async fn status_rejects_anonymous_callers() {
        let response = status(data(test_context()), anonymous_request()).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn status_rejects_callers_without_view() {
        // Headers present, but no `role.check` handler answers:
        // the guard fails closed.
        let response = status(data(test_context()), authed_request()).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn status_accepts_callers_with_view() {
        let response = status(data(allowing_context()), authed_request()).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_processes_rejects_anonymous_callers() {
        let response = list_processes(
            data(test_context()),
            anonymous_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_processes_accepts_callers_with_view() {
        let response = list_processes(
            data(allowing_context()),
            authed_request(),
            web::Query(PaginationParams {
                per_page: Some(5),
                sort: Some("memory".to_string()),
                ..Default::default()
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_services_rejects_anonymous_callers() {
        let response = list_services(
            data(test_context()),
            anonymous_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_services_rejects_callers_without_view() {
        let response = list_services(
            data(test_context()),
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn info_rejects_anonymous_callers() {
        let response = info(data(test_context()), anonymous_request()).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn info_accepts_callers_with_view() {
        let response = info(data(allowing_context()), authed_request()).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn reload_rejects_anonymous_callers() {
        let response = reload(
            data(test_context()),
            anonymous_request(),
            web::Json(ReloadRequest { dry_run: true }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn reload_rejects_callers_without_manage() {
        let response = reload(
            data(test_context()),
            authed_request(),
            web::Json(ReloadRequest { dry_run: true }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn reload_passes_the_gate_for_callers_with_manage() {
        // The gate passes, so the request reaches config loading; the
        // outcome after that depends on the on-disk configuration, so
        // only the gate itself is asserted here (not 401 or 403).
        let response = reload(
            data(allowing_context()),
            authed_request(),
            web::Json(ReloadRequest { dry_run: true }),
        )
        .await;
        assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
        assert_ne!(response.status(), StatusCode::FORBIDDEN);
    }
}
