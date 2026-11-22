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
// and processes endpoints stay cheap under load.

use crate::core::config::AppConfig;
use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::events;
use crate::core::json::{error_response, success_response};
use crate::core::modules::{Module, ModuleRegistry};
use actix_web::{web, HttpResponse};
use serde::{Deserialize, Serialize};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, LazyLock, Mutex};
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

impl Module for ServiceMonitorModule {
    fn id(&self) -> &'static str {
        "service-monitor"
    }

    fn name(&self) -> &'static str {
        "Service Monitor"
    }

    fn description(&self) -> &'static str {
        "Monitors process health, tracked services and server information, and reloads the configuration."
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/service-monitor")
                .route("/status", web::get().to(status))
                .route("/services", web::get().to(list_services))
                .route("/processes", web::get().to(list_processes))
                .route("/reload", web::post().to(reload))
                .route("/info", web::get().to(info)),
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
