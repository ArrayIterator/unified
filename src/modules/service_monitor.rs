// Service Monitor module.
//
// Tracks the health of the running process and of the services the
// application depends on (HTTP, TLS, database, Redis, Geo). It also owns
// the configuration reload endpoint
// (`POST /api/v1/service-monitor/reload`), which validates the new
// configuration before applying it — a failing configuration never
// replaces the running one. The public `/health` endpoint is registered
// by the HTTP server itself, not by this module. System data comes from a
// shared snapshot refreshed at most once a second, so the status, info
// and processes endpoints stay cheap under load. The service probes
// run on `GET /api/v1/service-monitor/services`: when a probe finds a
// service `down` that was not `down` on the previous probe, the module
// emits `cluster.health.changed` (the established unhealthy-transition
// event, shared with the clusters module and subscribed to by the
// notifications module) so the outage alerts instead of only showing
// up in the response body. Recovery transitions emit nothing: the
// notifications module only alerts on the unhealthy transition.
//
// This module also owns the external library checks: every module may
// declare the system libraries / binaries it needs
// (`Module::requires_external_libraries`), and this module checks them
// locally — and, on request, on remote servers over SSH — reporting
// each library as ok, missing, or outdated, and emitting
// `library.check.failed` for every failure so notifications and audit
// can pick it up. The latest results are cached and surfaced in the
// status response and through the `/libraries` endpoints below.

use crate::core::config::AppConfig;
use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus};
use crate::core::json::{error_response, success_response};
use crate::core::modules::{
    ExternalLibraryProbe, Module, ModuleRegistry, RouteInfo,
};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use regex::Regex;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::process::Command;
use std::sync::{Arc, LazyLock, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};
use sysinfo::{DiskKind, Disks, ProcessStatus, System};

pub struct ServiceMonitorModule;

impl ServiceMonitorModule {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ServiceMonitorModule {
    fn default() -> Self {
        Self::new()
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(std::sync::Arc::new(ServiceMonitorModule::new()));
}

/// One tracked service and its current state.
#[derive(Debug, Clone, Serialize)]
pub struct ServiceStatus {
    pub id: &'static str,
    pub name: String,
    /// `up`, `down`, `disabled`, or `unavailable`.
    pub status: String,
    pub latency_ms: Option<u64>,
    pub detail: String,
}

/// Event emitted when a probed service transitions to `down`. The
/// established unhealthy-transition event is reused instead of
/// inventing a service-specific one: `cluster.health.changed` is
/// what the clusters module emits on a health change and what the
/// notifications module subscribes to (verified by reading its
/// listener list and its `on_domain_event` gate, which only alerts
/// when `health` is `unhealthy`, `down`, `failed` or `critical`).
/// The payload mirrors the clusters shape (`node_id`, `cluster_id`,
/// `previous_health`, `health`, `consecutive_failures`, `latency_ms`,
/// `checked_at`) plus the service fields (`service_id`,
/// `service_name`, `status`, `detail`) the notification text needs.
/// `cluster_id` is `"local"`: these are the application's own
/// dependencies, not a cluster node.
pub const SERVICE_HEALTH_CHANGED_EVENT: &str = "cluster.health.changed";

/// Last probed status per service id, so only the up-to-down
/// transition emits, not every probe that finds the service down.
static LAST_SERVICE_STATUS: OnceLock<DashMap<&'static str, String>> = OnceLock::new();

fn last_service_status_store() -> &'static DashMap<&'static str, String> {
    LAST_SERVICE_STATUS.get_or_init(DashMap::new)
}

/// Emit `cluster.health.changed` for every service that just
/// transitioned to `down` (its previous probe was not `down`).
/// `disabled` and `unavailable` are not failures and never emit.
/// The remembered status is updated for every service, including
/// recoveries, so a later outage alerts again.
fn emit_service_down_events(
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
#[derive(Debug, Clone, Serialize)]
pub struct MonitorStatus {
    pub status: String,
    pub version: String,
    pub mode: String,
    pub uptime_seconds: u64,
    pub paused: bool,
    pub maintenance: bool,
    pub workers: usize,
    pub total_memory_bytes: u64,
    pub used_memory_bytes: u64,
    pub process_memory_bytes: u64,
    pub cpu_count: usize,
    pub cpu_usage_percent: f32,
    pub total_swap_bytes: u64,
    pub used_swap_bytes: u64,
    pub load_average: LoadAverage,
    pub process_cpu_percent: f32,
    pub process_virtual_memory_bytes: u64,
    pub process_status: String,
    pub process_run_time_seconds: u64,
    /// Summary of the most recent external library check (cached;
    /// `"unknown"` until the first check has run).
    pub libraries: LibraryCheckSummary,
}

/// Server information returned by `GET /api/v1/service-monitor/info`.
#[derive(Debug, Clone, Serialize)]
pub struct ServerInfo {
    pub os: Option<String>,
    pub kernel_version: Option<String>,
    pub hostname: Option<String>,
    pub uptime_seconds: u64,
    pub boot_time_seconds: u64,
    pub cpu_brand: Option<String>,
    pub cpu_frequency_mhz: Option<u64>,
    pub cpu_vendor: Option<String>,
    pub physical_cores: Option<usize>,
    pub logical_cores: usize,
    pub load_average: LoadAverage,
    pub total_memory_bytes: u64,
    pub used_memory_bytes: u64,
    pub free_memory_bytes: u64,
    pub available_memory_bytes: u64,
    pub total_swap_bytes: u64,
    pub used_swap_bytes: u64,
    pub free_swap_bytes: u64,
    pub disks: Vec<DiskInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadAverage {
    pub one_minute: f64,
    pub five_minutes: f64,
    pub fifteen_minutes: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiskInfo {
    pub name: String,
    pub mount_point: String,
    pub file_system: String,
    pub total_space_bytes: u64,
    pub available_space_bytes: u64,
    pub removable: bool,
    pub kind: String,
    pub io_read_bytes: u64,
    pub io_written_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReloadRequest {
    #[serde(default)]
    pub dry_run: bool,
}

/// Short one-letter code for a process state, for tables and API output.
pub fn process_status_label(status: ProcessStatus) -> &'static str {
    match status {
        ProcessStatus::Run => "R",
        ProcessStatus::Sleep => "S",
        ProcessStatus::Idle => "I",
        ProcessStatus::Stop => "T",
        ProcessStatus::Zombie => "Z",
        ProcessStatus::Tracing => "t",
        ProcessStatus::Dead => "X",
        _ => "?",
    }
}

/// Human-readable disk kind.
pub fn disk_kind_label(kind: DiskKind) -> &'static str {
    match kind {
        DiskKind::HDD => "HDD",
        DiskKind::SSD => "SSD",
        DiskKind::Unknown(_) => "Unknown",
    }
}

/// One process row for the processes endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct ApiProcessInfo {
    pub pid: u32,
    pub name: String,
    pub status: String,
    pub user_id: Option<String>,
    pub parent_pid: Option<u32>,
    pub cpu_usage_percent: f32,
    pub memory_bytes: u64,
    pub virtual_memory_bytes: u64,
    pub disk_read_bytes: u64,
    pub disk_written_bytes: u64,
    pub run_time_seconds: u64,
}

#[derive(Debug, Deserialize)]
pub struct ProcessesQuery {
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub sort: Option<String>,
}

/// The daemon's own process, as seen by the snapshot.
#[derive(Debug, Clone)]
struct OwnProcessInfo {
    memory_bytes: u64,
    virtual_memory_bytes: u64,
    cpu_usage_percent: f32,
    status: String,
    run_time_seconds: u64,
}

/// Shared system snapshot used by the status, info and processes handlers.
#[derive(Debug, Clone)]
struct SystemSnapshot {
    total_memory_bytes: u64,
    used_memory_bytes: u64,
    free_memory_bytes: u64,
    available_memory_bytes: u64,
    total_swap_bytes: u64,
    used_swap_bytes: u64,
    free_swap_bytes: u64,
    global_cpu_usage_percent: f32,
    cpu_brand: Option<String>,
    cpu_frequency_mhz: Option<u64>,
    cpu_vendor: Option<String>,
    physical_cores: Option<usize>,
    logical_cores: usize,
    load_average: LoadAverage,
    process: Option<OwnProcessInfo>,
    disks: Vec<DiskInfo>,
    processes: Vec<ApiProcessInfo>,
}

/// How often the shared snapshot is refreshed. CPU usage is a delta between
/// two samples, so the first request after startup reports 0 for CPU
/// percentages until the second refresh.
const SNAPSHOT_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// Shared snapshot cache. Enumerating every process with
/// `System::new_all()` on each request is expensive, so the handlers share
/// one snapshot refreshed at most once a second.
static SYSTEM_CACHE: LazyLock<Mutex<CachedSnapshot>> =
    LazyLock::new(|| Mutex::new(CachedSnapshot::new()));

struct CachedSnapshot {
    system: System,
    disks: Disks,
    snapshot: Option<SystemSnapshot>,
    last_refresh: Option<Instant>,
}

impl CachedSnapshot {
    fn new() -> Self {
        Self {
            system: System::new_all(),
            disks: Disks::new_with_refreshed_list(),
            snapshot: None,
            last_refresh: None,
        }
    }
}

fn api_process_info(process: &sysinfo::Process) -> ApiProcessInfo {
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

fn collect_snapshot(system: &System, disks: &Disks) -> SystemSnapshot {
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
        physical_cores: System::physical_core_count(),
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
            .map(|disk| {
                let usage = disk.usage();
                DiskInfo {
                    name: disk.name().to_string_lossy().to_string(),
                    mount_point: disk.mount_point().to_string_lossy().to_string(),
                    file_system: String::from_utf8_lossy(disk.file_system()).to_string(),
                    total_space_bytes: disk.total_space(),
                    available_space_bytes: disk.available_space(),
                    removable: disk.is_removable(),
                    kind: disk_kind_label(disk.kind()).to_string(),
                    io_read_bytes: usage.total_read_bytes,
                    io_written_bytes: usage.total_written_bytes,
                }
            })
            .collect(),
        processes,
    }
}

fn snapshot_system() -> SystemSnapshot {
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

fn worker_count(ctx: &AppContext) -> usize {
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

async fn status(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
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

/// Top processes by CPU (default) or memory (`?sort=memory`), up to
/// `?limit=` rows (default 10, clamped to 50).
async fn list_processes(query: web::Query<ProcessesQuery>) -> HttpResponse {
    let snapshot = snapshot_system();
    let limit = query.limit.unwrap_or(10).clamp(1, 50);
    let mut processes = snapshot.processes;
    if query.sort.as_deref() == Some("memory") {
        processes.sort_by(|a, b| b.memory_bytes.cmp(&a.memory_bytes));
    }
    processes.truncate(limit);
    success_response(processes, 200)
}

/// Probe one TCP address and return (reachable, latency in milliseconds).
fn probe_tcp(address: &str, timeout: Duration) -> (bool, u64) {
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

fn probe_services(config: &AppConfig, db_present: bool) -> Vec<ServiceStatus> {
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

async fn list_services(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let config = ctx
        .config
        .read()
        .expect("config lock poisoned")
        .clone();
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
            success_response(services, 200)
        }
        Err(error) => error_response(
            &UnifiedError::internal(format!("Service probe failed: {error}")),
            debug,
        ),
    }
}

async fn reload(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<ReloadRequest>,
) -> HttpResponse {
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

async fn info(_ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
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

// ---------------------------------------------------------------------------
// External library checks.
//
// Every module may declare the system libraries / binaries it needs
// through `Module::requires_external_libraries`. This section checks
// those declarations against the host the application runs on and, on
// request, against remote servers over SSH.
//
// Design notes:
//   - The checker never runs an arbitrary command. Local probes only
//     ever run a binary from the fixed allowlist in `probe_commands`
//     with a fixed version flag, and remote probes only ever run those
//     same commands through the system `ssh` client. A declared
//     library with no allowlisted probe is reported as unverifiable
//     instead of being probed some other way.
//   - No OS package management lives here: there is no install,
//     upgrade, or remove action anywhere in this section, and no
//     flow that needs root. The checker reports; the operator acts.
//   - Results are cached in memory. The status endpoint and the
//     Extensions Manager read the same cache, so the library data has
//     exactly one shape everywhere it appears.
// ---------------------------------------------------------------------------

/// Event emitted once per library that a check found missing or
/// outdated. Payload: `{"library", "required_version",
/// "found_version", "status", "target", "required_by", "reason",
/// "checked_at"}`. There was no pre-existing health/library event
/// from this module to reuse (it previously only emitted
/// `app.reload`), so this name follows the existing
/// `<domain>.<action>` naming (`cluster.health.changed`,
/// `module.enabled`) and lives next to the code that emits it, the
/// same way `MODULE_AUTO_ACTIVATED_EVENT` lives in
/// `crate::core::modules`.
pub const LIBRARY_CHECK_FAILED_EVENT: &str = "library.check.failed";

/// One declared external library requirement, aggregated across all
/// modules that declare it: the highest minimum version wins, and
/// every requiring module is listed so notices can name the owners.
#[derive(Debug, Clone)]
pub struct AggregatedRequirement {
    pub name: String,
    pub minimum_version: Option<String>,
    pub reasons: Vec<String>,
    pub required_by: Vec<String>,
    pub probe: Option<ExternalLibraryProbe>,
}

/// Result of checking one library on one target. `status` is exactly
/// one of `"ok"`, `"missing"`, or `"outdated"`: a library whose
/// binary runs but whose version cannot be parsed is reported as
/// `"outdated"` (the requirement cannot be proven met), and a
/// library with no registered probe is reported as `"missing"` with
/// a detail that says so, so neither case is ever silently "ok".
#[derive(Debug, Clone, Serialize)]
pub struct LibraryCheckResult {
    pub library: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub found_version: Option<String>,
    /// `"ok"`, `"missing"`, or `"outdated"`.
    pub status: String,
    /// Ids of the modules that require this library.
    pub required_by: Vec<String>,
    /// Why the library is needed (first declaring module's reason).
    pub reason: String,
    /// What the checker found, in plain language.
    pub detail: String,
    /// `"local"`, a registered SSH server id, or `ssh:user@host`.
    pub target: String,
    pub checked_at: DateTime<Utc>,
}

/// One completed check run against one target.
#[derive(Debug, Clone, Serialize)]
pub struct LibraryCheckReport {
    /// `"local"`, a registered SSH server id, or `ssh:user@host`.
    pub target: String,
    /// `"ok"` (nothing missing/outdated), `"warning"`, or `"error"`
    /// (the target itself could not be checked at all).
    pub status: String,
    pub checked_at: DateTime<Utc>,
    pub ok: usize,
    pub missing: usize,
    pub outdated: usize,
    /// Set only when `status` is `"error"`; then `results` is empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub results: Vec<LibraryCheckResult>,
}

/// Summary of the most recent local check, embedded in the status
/// response so health data and the library endpoints never diverge.
#[derive(Debug, Clone, Serialize)]
pub struct LibraryCheckSummary {
    /// False until the first check has run.
    pub checked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<DateTime<Utc>>,
    /// `"ok"`, `"warning"`, or `"unknown"` (never checked).
    pub status: String,
    pub total: usize,
    pub ok: usize,
    pub missing: usize,
    pub outdated: usize,
}

impl LibraryCheckSummary {
    fn unknown() -> Self {
        Self {
            checked: false,
            checked_at: None,
            status: "unknown".to_string(),
            total: 0,
            ok: 0,
            missing: 0,
            outdated: 0,
        }
    }

    fn from_report(report: &LibraryCheckReport) -> Self {
        Self {
            checked: true,
            checked_at: Some(report.checked_at),
            status: report.status.clone(),
            total: report.results.len(),
            ok: report.ok,
            missing: report.missing,
            outdated: report.outdated,
        }
    }
}

/// Fixed allowlist of version probes: (binary, version arguments).
/// ImageMagick has two candidates because version 7 renamed
/// `convert` to `magick`; the checker tries them in order.
fn probe_commands(probe: ExternalLibraryProbe) -> &'static [(&'static str, &'static [&'static str])]
{
    match probe {
        ExternalLibraryProbe::ImageMagick => {
            &[("magick", &["--version"]), ("convert", &["--version"])]
        }
        ExternalLibraryProbe::Ffmpeg => &[("ffmpeg", &["-version"])],
        ExternalLibraryProbe::Ghostscript => &[("gs", &["--version"])],
        ExternalLibraryProbe::ExifTool => &[("exiftool", &["-ver"])],
        ExternalLibraryProbe::Poppler => &[("pdftotext", &["-v"])],
        ExternalLibraryProbe::LibreOffice => &[("soffice", &["--version"])],
        ExternalLibraryProbe::Python3 => &[("python3", &["--version"])],
        ExternalLibraryProbe::Node => &[("node", &["--version"])],
        ExternalLibraryProbe::Git => &[("git", &["--version"])],
        ExternalLibraryProbe::Curl => &[("curl", &["--version"])],
        ExternalLibraryProbe::Wget => &[("wget", &["--version"])],
        ExternalLibraryProbe::Rsync => &[("rsync", &["--version"])],
        ExternalLibraryProbe::Tar => &[("tar", &["--version"])],
        ExternalLibraryProbe::Unzip => &[("unzip", &["-v"])],
        ExternalLibraryProbe::Zip => &[("zip", &["-v"])],
        ExternalLibraryProbe::Jpegoptim => &[("jpegoptim", &["--version"])],
        ExternalLibraryProbe::Pngquant => &[("pngquant", &["--version"])],
        ExternalLibraryProbe::Optipng => &[("optipng", &["--version"])],
        ExternalLibraryProbe::Cwebp => &[("cwebp", &["-version"])],
        ExternalLibraryProbe::Gifsicle => &[("gifsicle", &["--version"])],
        ExternalLibraryProbe::Sqlite3 => &[("sqlite3", &["--version"])],
        ExternalLibraryProbe::PostgresClient => &[("psql", &["--version"])],
        ExternalLibraryProbe::MysqlClient => &[("mysql", &["--version"])],
        ExternalLibraryProbe::RedisCli => &[("redis-cli", &["--version"])],
        ExternalLibraryProbe::Wkhtmltopdf => &[("wkhtmltopdf", &["--version"])],
        ExternalLibraryProbe::RsvgConvert => &[("rsvg-convert", &["--version"])],
        ExternalLibraryProbe::Vips => &[("vips", &["--version"])],
        ExternalLibraryProbe::Inkscape => &[("inkscape", &["--version"])],
    }
}

/// Name-based probe lookup for requirements declared without an
/// explicit probe. Only names in this table resolve; anything else
/// is unverifiable rather than guessed at.
fn probe_for_name(name: &str) -> Option<ExternalLibraryProbe> {
    match name.to_lowercase().as_str() {
        "imagemagick" | "magick" | "convert" => Some(ExternalLibraryProbe::ImageMagick),
        "ffmpeg" => Some(ExternalLibraryProbe::Ffmpeg),
        "ghostscript" | "gs" => Some(ExternalLibraryProbe::Ghostscript),
        "exiftool" => Some(ExternalLibraryProbe::ExifTool),
        "poppler" | "pdftotext" | "poppler-utils" => Some(ExternalLibraryProbe::Poppler),
        "libreoffice" | "soffice" => Some(ExternalLibraryProbe::LibreOffice),
        "python3" | "python" => Some(ExternalLibraryProbe::Python3),
        "node" | "nodejs" => Some(ExternalLibraryProbe::Node),
        "git" => Some(ExternalLibraryProbe::Git),
        "curl" => Some(ExternalLibraryProbe::Curl),
        "wget" => Some(ExternalLibraryProbe::Wget),
        "rsync" => Some(ExternalLibraryProbe::Rsync),
        "tar" => Some(ExternalLibraryProbe::Tar),
        "unzip" => Some(ExternalLibraryProbe::Unzip),
        "zip" => Some(ExternalLibraryProbe::Zip),
        "jpegoptim" => Some(ExternalLibraryProbe::Jpegoptim),
        "pngquant" => Some(ExternalLibraryProbe::Pngquant),
        "optipng" => Some(ExternalLibraryProbe::Optipng),
        "cwebp" | "libwebp" => Some(ExternalLibraryProbe::Cwebp),
        "gifsicle" => Some(ExternalLibraryProbe::Gifsicle),
        "sqlite3" | "sqlite" => Some(ExternalLibraryProbe::Sqlite3),
        "psql" | "postgresql" | "postgresql-client" => {
            Some(ExternalLibraryProbe::PostgresClient)
        }
        "mysql" | "mysql-client" => Some(ExternalLibraryProbe::MysqlClient),
        "redis-cli" | "redis" => Some(ExternalLibraryProbe::RedisCli),
        "wkhtmltopdf" => Some(ExternalLibraryProbe::Wkhtmltopdf),
        "rsvg-convert" | "librsvg" => Some(ExternalLibraryProbe::RsvgConvert),
        "vips" | "libvips" => Some(ExternalLibraryProbe::Vips),
        "inkscape" => Some(ExternalLibraryProbe::Inkscape),
        _ => None,
    }
}

/// Probe commands for one aggregated requirement: the explicit probe
/// first, else the name lookup, else nothing (unverifiable).
fn probes_for(
    requirement: &AggregatedRequirement,
) -> Vec<(&'static str, &'static [&'static str])> {
    let probe = requirement
        .probe
        .or_else(|| probe_for_name(&requirement.name));
    match probe {
        Some(probe) => probe_commands(probe).to_vec(),
        None => Vec::new(),
    }
}

/// First `major.minor[.patch]` found in probe output, as a semver
/// version plus the matched text (`"7.1.1"`, `"6.9"`). Tool output is
/// free-form (`"Version: ImageMagick 7.1.1-43 ..."`,
/// `"ffmpeg version 6.1.1 ..."`) so only the first version-shaped
/// token is trusted, never the whole line.
static VERSION_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\d+)\.(\d+)(?:\.(\d+))?").expect("version pattern is valid")
});

fn parse_loose_version(text: &str) -> Option<(Version, String)> {
    let captures = VERSION_PATTERN.captures(text)?;
    let major: u64 = captures[1].parse().ok()?;
    let minor: u64 = captures[2].parse().ok()?;
    let patch: u64 = captures
        .get(3)
        .map_or(0, |part| part.as_str().parse().unwrap_or(0));
    let display = if captures.get(3).is_some() {
        format!("{major}.{minor}.{patch}")
    } else {
        format!("{major}.{minor}")
    };
    Some((Version::new(major, minor, patch), display))
}

/// Parse a declared minimum version (`"7"`, `"7.1"`, `"7.1.0"`).
/// A bare major version means `major.0.0`.
fn parse_minimum_version(raw: &str) -> Option<Version> {
    let trimmed = raw.trim();
    let padded = if trimmed.chars().all(|c| c.is_ascii_digit()) {
        format!("{trimmed}.0")
    } else {
        trimmed.to_string()
    };
    parse_loose_version(&padded).map(|(version, _)| version)
}

/// Aggregate every module's external library declarations: one entry
/// per library (case-insensitive), highest minimum version wins,
/// every requiring module and reason collected.
pub fn aggregate_external_requirements(
    registry: &ModuleRegistry,
) -> Vec<AggregatedRequirement> {
    let mut out: Vec<AggregatedRequirement> = Vec::new();
    for module in registry.list() {
        for requirement in module.requires_external_libraries() {
            let key = requirement.name.to_lowercase();
            match out
                .iter()
                .position(|entry: &AggregatedRequirement| entry.name.to_lowercase() == key)
            {
                Some(index) => {
                    let entry = &mut out[index];
                    let module_id = module.id().to_string();
                    if !entry.required_by.contains(&module_id) {
                        entry.required_by.push(module_id);
                    }
                    let reason = requirement.reason.to_string();
                    if !entry.reasons.contains(&reason) {
                        entry.reasons.push(reason);
                    }
                    if entry.probe.is_none() {
                        entry.probe = requirement.probe;
                    }
                    if let Some(candidate) = requirement.minimum_version {
                        entry.minimum_version = Some(match &entry.minimum_version {
                            None => candidate.to_string(),
                            Some(current) => {
                                let current_version = parse_minimum_version(current);
                                let candidate_version = parse_minimum_version(candidate);
                                match (current_version, candidate_version) {
                                    (Some(a), Some(b)) if b > a => candidate.to_string(),
                                    _ => current.clone(),
                                }
                            }
                        });
                    }
                }
                None => out.push(AggregatedRequirement {
                    name: requirement.name.to_string(),
                    minimum_version: requirement.minimum_version.map(str::to_string),
                    reasons: vec![requirement.reason.to_string()],
                    required_by: vec![module.id().to_string()],
                    probe: requirement.probe,
                }),
            }
        }
    }
    for entry in &mut out {
        entry.required_by.sort();
        entry.reasons.sort();
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

/// Cached library check results: the latest local report plus one
/// report per remote target label. In-memory only; a restart clears
/// it and the next read runs a fresh local check.
struct LibraryCheckCache {
    local: Option<LibraryCheckReport>,
    remote: HashMap<String, LibraryCheckReport>,
}

static LIBRARY_CHECK_CACHE: LazyLock<Mutex<LibraryCheckCache>> =
    LazyLock::new(|| {
        Mutex::new(LibraryCheckCache {
            local: None,
            remote: HashMap::new(),
        })
    });

fn store_library_report(report: &LibraryCheckReport) {
    let mut cache = LIBRARY_CHECK_CACHE
        .lock()
        .expect("library check cache lock poisoned");
    if report.target == "local" {
        cache.local = Some(report.clone());
    } else {
        cache.remote.insert(report.target.clone(), report.clone());
    }
}

/// Latest local check results, or an empty list when no check has
/// run yet. Read by the Extensions Manager for per-module notices.
pub fn latest_library_results() -> Vec<LibraryCheckResult> {
    let cache = LIBRARY_CHECK_CACHE
        .lock()
        .expect("library check cache lock poisoned");
    cache
        .local
        .as_ref()
        .map(|report| report.results.clone())
        .unwrap_or_default()
}

/// Latest local check summary, for the status response.
pub fn latest_library_summary() -> LibraryCheckSummary {
    let cache = LIBRARY_CHECK_CACHE
        .lock()
        .expect("library check cache lock poisoned");
    cache
        .local
        .as_ref()
        .map(LibraryCheckSummary::from_report)
        .unwrap_or_else(LibraryCheckSummary::unknown)
}

/// Latest reports for every checked target, local first, then remote
/// targets sorted by label.
pub fn latest_library_reports() -> Vec<LibraryCheckReport> {
    let cache = LIBRARY_CHECK_CACHE
        .lock()
        .expect("library check cache lock poisoned");
    let mut reports: Vec<LibraryCheckReport> = Vec::new();
    if let Some(local) = &cache.local {
        reports.push(local.clone());
    }
    let mut remote: Vec<LibraryCheckReport> = cache.remote.values().cloned().collect();
    remote.sort_by(|a, b| a.target.cmp(&b.target));
    reports.extend(remote);
    reports
}

/// Admin notice text for a failing check result, for example
/// `"ImageMagick 7+ required, 6.9.11 found"`. Returns `None` for an
/// `"ok"` result.
pub fn library_notice_message(result: &LibraryCheckResult) -> Option<String> {
    match result.status.as_str() {
        "missing" => Some(match &result.required_version {
            Some(required) => format!("{} {required}+ required, not installed", result.library),
            None => format!("{} required, not installed", result.library),
        }),
        "outdated" => Some(match (&result.required_version, &result.found_version) {
            (Some(required), Some(found)) => {
                format!("{} {required}+ required, {found} found", result.library)
            }
            (Some(required), None) => format!(
                "{} {required}+ required, installed version could not be determined",
                result.library
            ),
            (None, _) => format!(
                "{} installed version could not be determined",
                result.library
            ),
        }),
        _ => None,
    }
}

/// Combine a finished process's stdout and stderr into one text.
/// Version output lands on either stream depending on the tool
/// (`pdftotext -v` writes to stderr), so both are read.
fn combined_output(output: &std::process::Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&stderr);
    }
    text
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        trimmed.chars().take(max_chars).collect()
    }
}

fn missing_result(
    requirement: &AggregatedRequirement,
    target: &str,
    detail: &str,
    checked_at: DateTime<Utc>,
) -> LibraryCheckResult {
    LibraryCheckResult {
        library: requirement.name.clone(),
        required_version: requirement.minimum_version.clone(),
        found_version: None,
        status: "missing".to_string(),
        required_by: requirement.required_by.clone(),
        reason: requirement.reasons.first().cloned().unwrap_or_default(),
        target: target.to_string(),
        detail: detail.to_string(),
        checked_at,
    }
}

/// Evaluate probe output for one requirement: parse the installed
/// version and compare it with the declared minimum. A binary that
/// runs but reports no parseable version is `"outdated"` — the
/// requirement cannot be proven met.
fn evaluate_found(
    requirement: &AggregatedRequirement,
    output: &str,
    target: &str,
    checked_at: DateTime<Utc>,
) -> LibraryCheckResult {
    let base = LibraryCheckResult {
        library: requirement.name.clone(),
        required_version: requirement.minimum_version.clone(),
        found_version: None,
        status: "ok".to_string(),
        required_by: requirement.required_by.clone(),
        reason: requirement.reasons.first().cloned().unwrap_or_default(),
        target: target.to_string(),
        detail: String::new(),
        checked_at,
    };
    let Some((found_version, found_text)) = parse_loose_version(output) else {
        return LibraryCheckResult {
            status: "outdated".to_string(),
            detail: "The library appears to be installed, but its version could not be parsed from the probe output.".to_string(),
            ..base
        };
    };
    let mut result = LibraryCheckResult {
        found_version: Some(found_text),
        ..base
    };
    match requirement
        .minimum_version
        .as_deref()
        .and_then(parse_minimum_version)
    {
        None => {
            result.detail = match &requirement.minimum_version {
                Some(minimum) => format!(
                    "Minimum version \"{minimum}\" could not be parsed, so only presence was checked."
                ),
                None => "Installed.".to_string(),
            };
        }
        Some(minimum) => {
            if found_version >= minimum {
                result.detail = "Installed version meets the minimum.".to_string();
            } else {
                result.status = "outdated".to_string();
                result.detail = "Installed version is older than the required minimum.".to_string();
            }
        }
    }
    result
}

/// Run one probe binary locally. Returns the combined output when
/// the binary ran (whatever its exit code), or `None` when the
/// binary is not installed or could not be started.
fn run_local_probe(binary: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(binary).args(args).output().ok()?;
    Some(combined_output(&output))
}

fn check_requirement_local(
    requirement: &AggregatedRequirement,
    checked_at: DateTime<Utc>,
) -> LibraryCheckResult {
    let probes = probes_for(requirement);
    if probes.is_empty() {
        return missing_result(
            requirement,
            "local",
            &format!(
                "No version probe is registered for \"{}\"; add one to the probe allowlist before this requirement can be verified.",
                requirement.name
            ),
            checked_at,
        );
    }
    let mut tried: Vec<&str> = Vec::new();
    for (binary, args) in probes {
        tried.push(binary);
        if let Some(output) = run_local_probe(binary, args) {
            return evaluate_found(requirement, &output, "local", checked_at);
        }
    }
    missing_result(
        requirement,
        "local",
        &format!(
            "Probe binaries ({}) were not found on PATH",
            tried.join(", ")
        ),
        checked_at,
    )
}

fn build_report(
    target: &str,
    checked_at: DateTime<Utc>,
    results: Vec<LibraryCheckResult>,
) -> LibraryCheckReport {
    let ok = results.iter().filter(|r| r.status == "ok").count();
    let missing = results.iter().filter(|r| r.status == "missing").count();
    let outdated = results.iter().filter(|r| r.status == "outdated").count();
    let status = if missing == 0 && outdated == 0 {
        "ok"
    } else {
        "warning"
    };
    LibraryCheckReport {
        target: target.to_string(),
        status: status.to_string(),
        checked_at,
        ok,
        missing,
        outdated,
        error: None,
        results,
    }
}

fn error_report(target: &str, detail: &str) -> LibraryCheckReport {
    LibraryCheckReport {
        target: target.to_string(),
        status: "error".to_string(),
        checked_at: Utc::now(),
        ok: 0,
        missing: 0,
        outdated: 0,
        error: Some(detail.to_string()),
        results: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Remote checks over SSH.
//
// There is no SSH client library in the dependency set, and no
// provider or cluster record holds SSH credentials (cluster nodes
// authenticate with HMAC over HTTP; provider connections are API
// credentials), so remote checks run through the system `ssh` binary
// — the same approach the backup module uses for `pg_dump` and the
// mail module uses for `sendmail`. The app never runs as root for
// this: the check runs as the application user, against servers the
// operator configured.
//
// Credentials: a registered server stores only the host, port,
// username, auth kind, and (for keyfile auth) the keyfile *path*.
// A password is never stored anywhere — not in the registry, not in
// a file, not in a log. For password auth the caller supplies a
// one-time password in the re-check request; it lives only in that
// request, is passed to `sshpass` through the `SSHPASS` environment
// variable (never on a command line, where it would show in a
// process listing), and is dropped when the request ends. When the
// `sshpass` helper is not installed, password auth is refused with
// a clear error and keyfile auth is the way in.
// ---------------------------------------------------------------------------

/// One configured remote server. No secret material is stored here:
/// keyfile auth stores the keyfile path only, and password auth
/// stores nothing at all (see the note above).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshServer {
    /// Operator-chosen id, used as the re-check target name.
    pub id: String,
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub username: String,
    /// `"keyfile"` or `"password"`.
    pub auth_kind: String,
    /// Absolute path to the private key file (keyfile auth only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyfile: Option<String>,
}

fn default_ssh_port() -> u16 {
    22
}

fn valid_ssh_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && !host.starts_with('-')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'))
}

fn valid_ssh_username(username: &str) -> bool {
    !username.is_empty()
        && username.len() <= 64
        && !username.starts_with('-')
        && username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn valid_keyfile_path(path: &str) -> bool {
    path.starts_with('/') && !path.contains('\0') && path.len() <= 4096
}

fn valid_server_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// Validate a server record. The same checks guard both the registry
/// endpoints and inline re-check targets.
fn validate_ssh_server(server: &SshServer) -> UnifiedResult<()> {
    if !valid_server_id(&server.id) {
        return Err(UnifiedError::bad_request(
            "SSH server id must be 1-64 characters of letters, digits, '-' or '_'",
        ));
    }
    validate_ssh_connection(
        &server.host,
        server.port,
        &server.username,
        &server.auth_kind,
        server.keyfile.as_deref(),
    )
}

/// Validate the connection fields of a server (host, port, username,
/// auth kind, keyfile) without an id, for inline re-check targets
/// whose label is derived from the host and never registered.
fn validate_ssh_connection(
    host: &str,
    port: u16,
    username: &str,
    auth_kind: &str,
    keyfile: Option<&str>,
) -> UnifiedResult<()> {
    if !valid_ssh_host(host) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid SSH host: \"{host}\""
        )));
    }
    if port == 0 {
        return Err(UnifiedError::bad_request("SSH port must be 1-65535"));
    }
    if !valid_ssh_username(username) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid SSH username: \"{username}\""
        )));
    }
    match auth_kind {
        "keyfile" => {
            let Some(keyfile) = keyfile else {
                return Err(UnifiedError::bad_request(
                    "Keyfile authentication requires a keyfile path",
                ));
            };
            if !valid_keyfile_path(keyfile) {
                return Err(UnifiedError::bad_request(
                    "Keyfile path must be an absolute path",
                ));
            }
        }
        "password" => {
            if keyfile.is_some() {
                return Err(UnifiedError::bad_request(
                    "Password authentication does not take a keyfile path",
                ));
            }
        }
        _ => {
            return Err(UnifiedError::bad_request(
                "SSH auth_kind must be \"keyfile\" or \"password\"",
            ));
        }
    }
    Ok(())
}

static SSH_SERVERS: LazyLock<RwLock<Vec<SshServer>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

/// Registered remote servers (no secret material is ever in these
/// records). Held in memory; a restart clears the registry.
pub fn list_ssh_servers() -> Vec<SshServer> {
    let servers = SSH_SERVERS.read().expect("ssh servers lock poisoned");
    let mut out = servers.clone();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Register (or replace, by id) a remote server for library checks.
pub fn register_ssh_server(server: SshServer) -> UnifiedResult<SshServer> {
    validate_ssh_server(&server)?;
    let mut servers = SSH_SERVERS.write().expect("ssh servers lock poisoned");
    if let Some(existing) = servers.iter_mut().find(|entry| entry.id == server.id) {
        *existing = server.clone();
    } else {
        servers.push(server.clone());
    }
    Ok(server)
}

pub fn remove_ssh_server(id: &str) -> UnifiedResult<()> {
    let mut servers = SSH_SERVERS.write().expect("ssh servers lock poisoned");
    let before = servers.len();
    servers.retain(|entry| entry.id != id);
    if servers.len() == before {
        return Err(UnifiedError::not_found(format!(
            "SSH server \"{id}\" is not registered"
        )));
    }
    Ok(())
}

fn find_ssh_server(id: &str) -> Option<SshServer> {
    SSH_SERVERS
        .read()
        .expect("ssh servers lock poisoned")
        .iter()
        .find(|entry| entry.id == id)
        .cloned()
}

/// Transient authentication for one remote check. A password here
/// lives only for the duration of the re-check request that supplied
/// it and is never stored, logged, or returned.
#[derive(Debug, Clone)]
enum RemoteAuth {
    Keyfile { path: String },
    Password { password: String },
}

/// One resolved remote target for a check run.
#[derive(Debug, Clone)]
struct RemoteTarget {
    /// Report label: registered server id, or `ssh:user@host`.
    label: String,
    host: String,
    port: u16,
    username: String,
    auth: RemoteAuth,
}

/// Inline (request-scoped) remote server: never persisted, used for
/// this re-check only.
#[derive(Debug, Clone, Deserialize)]
pub struct InlineSshServer {
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub username: String,
    pub auth: InlineSshAuth,
}

/// Inline authentication: a keyfile path, or a one-time password
/// that is used for this check only and never stored.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum InlineSshAuth {
    Keyfile { keyfile: String },
    Password { password: String },
}

/// Re-check request body. `targets` names `"local"` and/or
/// registered SSH server ids (empty means local only); `servers`
/// adds request-scoped inline servers; `passwords` maps a
/// password-auth server id to its one-time password for this check.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LibraryRecheckRequest {
    #[serde(default)]
    pub targets: Vec<String>,
    #[serde(default)]
    pub servers: Vec<InlineSshServer>,
    #[serde(default)]
    pub passwords: HashMap<String, String>,
}

fn resolve_remote_targets(
    request: &LibraryRecheckRequest,
) -> UnifiedResult<Vec<RemoteTarget>> {
    let mut targets: Vec<RemoteTarget> = Vec::new();
    for target in &request.targets {
        if target == "local" {
            continue;
        }
        let server = find_ssh_server(target).ok_or_else(|| {
            UnifiedError::not_found(format!("SSH server \"{target}\" is not registered"))
        })?;
        let auth = match server.auth_kind.as_str() {
            "keyfile" => RemoteAuth::Keyfile {
                path: server.keyfile.clone().unwrap_or_default(),
            },
            _ => {
                let password = request
                    .passwords
                    .get(target)
                    .cloned()
                    .filter(|password| !password.is_empty())
                    .ok_or_else(|| {
                        UnifiedError::bad_request(format!(
                            "SSH server \"{target}\" uses password authentication; supply a one-time password for it in the re-check request (it is used once and never stored)"
                        ))
                    })?;
                RemoteAuth::Password { password }
            }
        };
        targets.push(RemoteTarget {
            label: server.id.clone(),
            host: server.host.clone(),
            port: server.port,
            username: server.username.clone(),
            auth,
        });
    }
    for inline in &request.servers {
        let (auth_kind, keyfile) = match &inline.auth {
            InlineSshAuth::Keyfile { keyfile } => ("keyfile", Some(keyfile.as_str())),
            InlineSshAuth::Password { .. } => ("password", None),
        };
        validate_ssh_connection(
            &inline.host,
            inline.port,
            &inline.username,
            auth_kind,
            keyfile,
        )?;
        let auth = match &inline.auth {
            InlineSshAuth::Keyfile { keyfile } => RemoteAuth::Keyfile {
                path: keyfile.clone(),
            },
            InlineSshAuth::Password { password } => {
                if password.is_empty() {
                    return Err(UnifiedError::bad_request(
                        "Inline SSH server password must not be empty",
                    ));
                }
                RemoteAuth::Password {
                    password: password.clone(),
                }
            }
        };
        targets.push(RemoteTarget {
            label: format!("ssh:{}@{}", inline.username, inline.host),
            host: inline.host.clone(),
            port: inline.port,
            username: inline.username.clone(),
            auth,
        });
    }
    Ok(targets)
}

/// Outcome of one remote probe invocation. `TransportFailed` means
/// the ssh transport itself failed (exit status 255, or the client
/// could not start); `NotFound` means the remote shell reported
/// exit status 127, i.e. the probe binary is not installed there.
enum RemoteProbeOutcome {
    Output(String),
    TransportFailed(String),
    NotFound,
}

fn ssh_available() -> bool {
    Command::new("ssh").arg("-V").output().is_ok()
}

fn sshpass_available() -> bool {
    Command::new("sshpass").arg("-V").output().is_ok()
}

fn run_remote_probe(
    target: &RemoteTarget,
    binary: &str,
    args: &[&str],
) -> RemoteProbeOutcome {
    // No shell is involved anywhere: the binary and its arguments are
    // separate argv entries, and the host/username/port/keyfile were
    // validated on the way in, so no configured value can inject a
    // second command. `BatchMode=yes` forbids interactive prompts;
    // `ConnectTimeout=10` bounds a hung connection;
    // `StrictHostKeyChecking=accept-new` trusts a host key on first
    // use (and pins it afterwards) instead of silently trusting every
    // host. Operators who need strict pinning should pre-seed the
    // application user's known_hosts file.
    let mut command = match &target.auth {
        RemoteAuth::Keyfile { .. } => Command::new("ssh"),
        RemoteAuth::Password { password } => {
            let mut wrapped = Command::new("sshpass");
            wrapped.arg("-e").env("SSHPASS", password).arg("ssh");
            wrapped
        }
    };
    command
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg("-o")
        .arg("StrictHostKeyChecking=accept-new")
        .arg("-o")
        .arg("LogLevel=ERROR")
        .arg("-p")
        .arg(target.port.to_string())
        .arg("-l")
        .arg(&target.username);
    if let RemoteAuth::Keyfile { path } = &target.auth {
        command.arg("-i").arg(path);
    }
    command.arg(&target.host).arg(binary);
    for arg in args {
        command.arg(arg);
    }
    let output = match command.output() {
        Ok(output) => output,
        Err(error) => {
            return RemoteProbeOutcome::TransportFailed(format!(
                "Could not start the ssh client: {error}"
            ));
        }
    };
    let code = output.status.code().unwrap_or(-1);
    if code == 255 {
        return RemoteProbeOutcome::TransportFailed(format!(
            "SSH exited with status 255: {}",
            truncate_text(&String::from_utf8_lossy(&output.stderr), 300)
        ));
    }
    if code == 127 {
        return RemoteProbeOutcome::NotFound;
    }
    RemoteProbeOutcome::Output(combined_output(&output))
}

fn check_requirement_remote(
    target: &RemoteTarget,
    requirement: &AggregatedRequirement,
    checked_at: DateTime<Utc>,
) -> Result<LibraryCheckResult, String> {
    let probes = probes_for(requirement);
    if probes.is_empty() {
        return Ok(missing_result(
            requirement,
            &target.label,
            &format!(
                "No version probe is registered for \"{}\"; add one to the probe allowlist before this requirement can be verified.",
                requirement.name
            ),
            checked_at,
        ));
    }
    let mut tried: Vec<&str> = Vec::new();
    for (binary, args) in probes {
        tried.push(binary);
        match run_remote_probe(target, binary, args) {
            RemoteProbeOutcome::Output(output) => {
                return Ok(evaluate_found(
                    requirement,
                    &output,
                    &target.label,
                    checked_at,
                ));
            }
            RemoteProbeOutcome::NotFound => continue,
            RemoteProbeOutcome::TransportFailed(detail) => return Err(detail),
        }
    }
    Ok(missing_result(
        requirement,
        &target.label,
        &format!(
            "Probe binaries ({}) were not found on the remote host",
            tried.join(", ")
        ),
        checked_at,
    ))
}

fn check_target_remote(
    target: &RemoteTarget,
    requirements: &[AggregatedRequirement],
) -> LibraryCheckReport {
    let checked_at = Utc::now();
    if !ssh_available() {
        return error_report(
            &target.label,
            "The ssh client is not installed on this host, so remote library checks cannot run.",
        );
    }
    if matches!(target.auth, RemoteAuth::Password { .. }) && !sshpass_available() {
        return error_report(
            &target.label,
            "Password authentication needs the sshpass helper, which is not installed; use keyfile authentication instead.",
        );
    }
    match run_remote_probe(target, "true", &[]) {
        RemoteProbeOutcome::TransportFailed(detail) => {
            return error_report(
                &target.label,
                &format!("SSH connection failed: {detail}"),
            );
        }
        _ => {}
    }
    let mut results: Vec<LibraryCheckResult> = Vec::new();
    for requirement in requirements {
        match check_requirement_remote(target, requirement, checked_at) {
            Ok(result) => results.push(result),
            Err(detail) => {
                return error_report(
                    &target.label,
                    &format!("SSH connection failed during the check: {detail}"),
                );
            }
        }
    }
    build_report(&target.label, checked_at, results)
}

/// Emit `library.check.failed` once per missing/outdated result in
/// a report, so notifications and audit can pick failures up.
/// Error reports (target unreachable) emit nothing: nothing was
/// actually checked there.
fn emit_library_failure_events(ctx: &AppContext, report: &LibraryCheckReport) {
    if report.status == "error" {
        return;
    }
    for result in &report.results {
        if result.status == "missing" || result.status == "outdated" {
            ctx.hooks.emit(
                LIBRARY_CHECK_FAILED_EVENT,
                json!({
                    "library": result.library,
                    "required_version": result.required_version,
                    "found_version": result.found_version,
                    "status": result.status,
                    "target": result.target,
                    "required_by": result.required_by,
                    "reason": result.reason,
                    "checked_at": result.checked_at,
                }),
            );
        }
    }
}

/// Run library checks for the requested targets (local when
/// `targets` is empty or contains `"local"`, plus every resolved
/// remote target), cache every report, emit failure events, and
/// return the reports. Blocking I/O: handlers must run this inside
/// `web::block`. A target that cannot be reached yields an `"error"`
/// report for that target instead of failing the whole run.
pub fn run_library_checks(
    ctx: &AppContext,
    request: &LibraryRecheckRequest,
) -> UnifiedResult<Vec<LibraryCheckReport>> {
    let remote_targets = resolve_remote_targets(request)?;
    let requirements = aggregate_external_requirements(&ctx.modules);
    let want_local = request.targets.is_empty()
        || request.targets.iter().any(|target| target == "local");
    let mut reports: Vec<LibraryCheckReport> = Vec::new();
    if want_local {
        let checked_at = Utc::now();
        let results: Vec<LibraryCheckResult> = requirements
            .iter()
            .map(|requirement| check_requirement_local(requirement, checked_at))
            .collect();
        let report = build_report("local", checked_at, results);
        emit_library_failure_events(ctx, &report);
        store_library_report(&report);
        reports.push(report);
    }
    for target in &remote_targets {
        let report = check_target_remote(target, &requirements);
        emit_library_failure_events(ctx, &report);
        store_library_report(&report);
        reports.push(report);
    }
    Ok(reports)
}

/// List the latest local library check results (paginated, with
/// `?search=` and `?status=` / `?target=` filters). When nothing has
/// been checked yet, the first read runs a fresh local check so the
/// response is useful instead of empty; later reads serve the cache.
async fn list_libraries(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let pagination = query.normalized();
    let search = pagination.search.clone().map(|s| s.to_lowercase());
    let status_filter = pagination.filters.get("status").cloned();
    let target_filter = pagination.filters.get("target").cloned();
    let debug = ctx.is_debug();
    let ctx_clone = Arc::clone(&ctx);
    // Probe I/O blocks, so it runs off the async worker threads.
    let loaded = web::block(move || {
        let cached = latest_library_results();
        if cached.is_empty() {
            let request = LibraryRecheckRequest::default();
            run_library_checks(&ctx_clone, &request).map(|_| latest_library_results())
        } else {
            Ok(cached)
        }
    })
    .await;
    let mut results = match loaded {
        Ok(Ok(results)) => results,
        Ok(Err(error)) => return error_response(&error, debug),
        Err(error) => {
            return error_response(
                &UnifiedError::internal(format!("Library check failed: {error}")),
                debug,
            );
        }
    };
    results.retain(|result| {
        if let Some(search) = &search {
            let haystack = format!(
                "{} {} {}",
                result.library,
                result.reason,
                result.required_by.join(" ")
            )
            .to_lowercase();
            if !haystack.contains(search.as_str()) {
                return false;
            }
        }
        if let Some(status) = &status_filter {
            if &result.status != status {
                return false;
            }
        }
        if let Some(target) = &target_filter {
            if &result.target != target {
                return false;
            }
        }
        true
    });
    match pagination.sort.as_deref().unwrap_or("library") {
        "status" => results.sort_by(|a, b| {
            a.status
                .cmp(&b.status)
                .then_with(|| a.library.to_lowercase().cmp(&b.library.to_lowercase()))
        }),
        _ => results.sort_by(|a, b| a.library.to_lowercase().cmp(&b.library.to_lowercase())),
    }
    if pagination.order == "desc" {
        results.reverse();
    }
    let total = results.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(results.len());
    let page_items = if start < results.len() {
        results[start..end].to_vec()
    } else {
        Vec::new()
    };
    success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

/// Re-check action: run the library checks again for the requested
/// targets and return one report per target. Remote targets that
/// cannot be reached yield an `"error"` report instead of failing
/// the whole run; unknown server ids and invalid targets are 400/404
/// errors in the standard envelope.
async fn recheck_libraries(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<LibraryRecheckRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let request = body.into_inner();
    let ctx_clone = Arc::clone(&ctx);
    let checked = web::block(move || run_library_checks(&ctx_clone, &request)).await;
    match checked {
        Ok(Ok(reports)) => success_response(reports, 200),
        Ok(Err(error)) => error_response(&error, debug),
        Err(error) => error_response(
            &UnifiedError::internal(format!("Library check failed: {error}")),
            debug,
        ),
    }
}

/// List the latest check report per target (local first, then
/// remote targets sorted by label), from the cache. This is the
/// read-only view of remote results: the re-check endpoint runs the
/// checks, this one only reports the latest stored outcome.
async fn list_library_reports() -> HttpResponse {
    success_response(latest_library_reports(), 200)
}

async fn list_registered_ssh_servers() -> HttpResponse {
    success_response(list_ssh_servers(), 200)
}

async fn register_ssh_server_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<SshServer>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    match register_ssh_server(body.into_inner()) {
        Ok(server) => success_response(server, 201),
        Err(error) => error_response(&error, debug),
    }
}

async fn remove_ssh_server_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    match remove_ssh_server(&id.into_inner()) {
        Ok(()) => success_response(json!({ "removed": true }), 200),
        Err(error) => error_response(&error, debug),
    }
}

impl Module for ServiceMonitorModule {
    fn id(&self) -> &'static str {
        "service-monitor"
    }

    fn name(&self) -> &'static str {
        "Service Monitor"
    }

    fn description(&self) -> &'static str {
        "Monitors process health, tracked services and server information, reloads the configuration, and checks the external system libraries modules require, locally and over SSH."
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/service-monitor/status", owner: "service-monitor", description: "Status" },
            RouteInfo { method: "GET", path: "/api/v1/service-monitor/services", owner: "service-monitor", description: "List services" },
            RouteInfo { method: "GET", path: "/api/v1/service-monitor/processes", owner: "service-monitor", description: "Top processes by CPU (default) or memory (`?sort=memory`), up to `?limit=` rows (default 10, clamped to 50)." },
            RouteInfo { method: "POST", path: "/api/v1/service-monitor/reload", owner: "service-monitor", description: "Reload" },
            RouteInfo { method: "GET", path: "/api/v1/service-monitor/info", owner: "service-monitor", description: "Info" },
            RouteInfo { method: "GET", path: "/api/v1/service-monitor/libraries", owner: "service-monitor", description: "List the latest external library check results (paginated, filter by status/target). Runs a fresh local check when nothing has been checked yet." },
            RouteInfo { method: "POST", path: "/api/v1/service-monitor/libraries/check", owner: "service-monitor", description: "Re-check external libraries locally and on remote SSH servers; emits library.check.failed per missing/outdated library." },
            RouteInfo { method: "GET", path: "/api/v1/service-monitor/libraries/reports", owner: "service-monitor", description: "List the latest library check report per target (local first, then remote targets)." },
            RouteInfo { method: "GET", path: "/api/v1/service-monitor/ssh-servers", owner: "service-monitor", description: "List registered SSH servers for remote library checks (no secret material is stored)." },
            RouteInfo { method: "POST", path: "/api/v1/service-monitor/ssh-servers", owner: "service-monitor", description: "Register (or replace) an SSH server for remote library checks." },
            RouteInfo { method: "DELETE", path: "/api/v1/service-monitor/ssh-servers/{id}", owner: "service-monitor", description: "Remove a registered SSH server." },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/service-monitor")
                .route("/status", web::get().to(status))
                .route("/services", web::get().to(list_services))
                .route("/processes", web::get().to(list_processes))
                .route("/reload", web::post().to(reload))
                .route("/info", web::get().to(info))
                .route("/libraries", web::get().to(list_libraries))
                .route("/libraries/check", web::post().to(recheck_libraries))
                .route("/libraries/reports", web::get().to(list_library_reports))
                .route("/ssh-servers", web::get().to(list_registered_ssh_servers))
                .route("/ssh-servers", web::post().to(register_ssh_server_endpoint))
                .route("/ssh-servers/{id}", web::delete().to(remove_ssh_server_endpoint)),
        );
    }
}

/// Parse a `host:port` string into a socket address. Used by the service
/// probes; kept public for tests of other modules.
#[allow(dead_code)]
pub fn parse_listen_address(address: &str) -> UnifiedResult<SocketAddr> {
    address.parse::<SocketAddr>().map_err(|_| {
        UnifiedError::bad_request(format!("Invalid listen address: {address}"))
    })
}
