// Service Monitor module.
//
// Tracks the health of the running process and of the services the
// application depends on (HTTP, TLS, database, Redis, Geo). It also owns
// the configuration reload endpoint
// (`POST /api/v1/service-monitor/reload`), which validates the new
// configuration before applying it — a failing configuration never
// replaces the running one. The public `/health` endpoint is registered
// by the HTTP server itself, not by this module.

use crate::core::config::AppConfig;
use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::events;
use crate::core::json::{error_response, success_response};
use crate::core::modules::{Module, ModuleRegistry};
use actix_web::{web, HttpResponse};
use serde::{Deserialize, Serialize};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};
use sysinfo::{Disks, System};

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
    pub physical_cores: Option<usize>,
    pub logical_cores: usize,
    pub load_average: LoadAverage,
    pub total_memory_bytes: u64,
    pub used_memory_bytes: u64,
    pub free_memory_bytes: u64,
    pub available_memory_bytes: u64,
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
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReloadRequest {
    #[serde(default)]
    pub dry_run: bool,
}

/// Shared system snapshot used by the status and info handlers.
struct SystemSnapshot {
    system: System,
    process_memory_bytes: u64,
}

fn snapshot_system() -> SystemSnapshot {
    let system = System::new_all();
    let pid = std::process::id();
    let process_memory_bytes = system
        .processes()
        .values()
        .find(|process| process.pid().as_u32() == pid)
        .map(|process| process.memory())
        .unwrap_or(0);
    SystemSnapshot {
        system,
        process_memory_bytes,
    }
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
    let system = &snapshot.system;
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
    success_response(
        MonitorStatus {
            status: overall.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            mode,
            uptime_seconds: ctx.uptime_seconds(),
            paused,
            maintenance,
            workers: worker_count(&ctx),
            total_memory_bytes: system.total_memory(),
            used_memory_bytes: system.used_memory(),
            process_memory_bytes: snapshot.process_memory_bytes,
            cpu_count: system.cpus().len(),
        },
        200,
    )
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
    let system = &snapshot.system;
    let load = System::load_average();
    let disks = Disks::new_with_refreshed_list();
    success_response(
        ServerInfo {
            os: System::long_os_version(),
            kernel_version: System::kernel_version(),
            hostname: System::host_name(),
            uptime_seconds: System::uptime(),
            boot_time_seconds: System::boot_time(),
            cpu_brand: system.cpus().first().map(|cpu| cpu.brand().to_string()),
            physical_cores: System::physical_core_count(),
            logical_cores: system.cpus().len(),
            load_average: LoadAverage {
                one_minute: load.one,
                five_minutes: load.five,
                fifteen_minutes: load.fifteen,
            },
            total_memory_bytes: system.total_memory(),
            used_memory_bytes: system.used_memory(),
            free_memory_bytes: system.free_memory(),
            available_memory_bytes: system.available_memory(),
            disks: disks
                .list()
                .iter()
                .map(|disk| DiskInfo {
                    name: disk.name().to_string_lossy().to_string(),
                    mount_point: disk.mount_point().to_string_lossy().to_string(),
                    file_system: String::from_utf8_lossy(disk.file_system()).to_string(),
                    total_space_bytes: disk.total_space(),
                    available_space_bytes: disk.available_space(),
                    removable: disk.is_removable(),
                })
                .collect(),
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
