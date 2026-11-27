// service_monitor module -- Shared types, records, request bodies, and constants.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::HttpRequest;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use sysinfo::{DiskKind, Disks, ProcessStatus, System};

use super::*;

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

/// The daemon's own process, as seen by the snapshot.
#[derive(Debug, Clone)]
pub(crate) struct OwnProcessInfo {
    pub(crate) memory_bytes: u64,
    pub(crate) virtual_memory_bytes: u64,
    pub(crate) cpu_usage_percent: f32,
    pub(crate) status: String,
    pub(crate) run_time_seconds: u64,
}

/// Shared system snapshot used by the status, info and processes handlers.
#[derive(Debug, Clone)]
pub(crate) struct SystemSnapshot {
    pub(crate) total_memory_bytes: u64,
    pub(crate) used_memory_bytes: u64,
    pub(crate) free_memory_bytes: u64,
    pub(crate) available_memory_bytes: u64,
    pub(crate) total_swap_bytes: u64,
    pub(crate) used_swap_bytes: u64,
    pub(crate) free_swap_bytes: u64,
    pub(crate) global_cpu_usage_percent: f32,
    pub(crate) cpu_brand: Option<String>,
    pub(crate) cpu_frequency_mhz: Option<u64>,
    pub(crate) cpu_vendor: Option<String>,
    pub(crate) physical_cores: Option<usize>,
    pub(crate) logical_cores: usize,
    pub(crate) load_average: LoadAverage,
    pub(crate) process: Option<OwnProcessInfo>,
    pub(crate) disks: Vec<DiskInfo>,
    pub(crate) processes: Vec<ApiProcessInfo>,
}

/// How often the shared snapshot is refreshed. CPU usage is a delta between
/// two samples, so the first request after startup reports 0 for CPU
/// percentages until the second refresh.
pub(crate) const SNAPSHOT_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

// Access control.
//
// Every route in this module requires a caller identity and a
// capability, exactly like the supervision and bi modules: the
// caller is identified by the `X-User-Id` and `X-Role` headers, and
// the capability is checked through the `role.check` hook, failing
// closed when no handler answers (a role with no grant, or no roles
// module at all, is denied). Reads need
// `service-monitor.view`; anything that changes state or triggers
// work on the operator's behalf -- config reload, library re-check
// (which opens SSH connections), and SSH server registration /
// removal -- needs `service-monitor.manage`. Operators grant these
// capabilities to roles through the roles module; until they do,
// every endpoint answers 401 (no identity headers) or 403 (the
// role does not hold the capability).

/// Read capability: status, services, processes, info, library
/// results / reports, and the registered SSH server list.
pub(crate) const PERM_SERVICE_MONITOR_VIEW: &str = "service-monitor.view";

/// Admin capability: config reload, library re-check, and SSH
/// server registration / removal.
pub(crate) const PERM_SERVICE_MONITOR_MANAGE: &str = "service-monitor.manage";

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the supervision and bi modules.
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

/// Identify the caller from the request headers and require
/// `permission` for them. Handlers call this first and render the
/// returned error (401 for a missing identity, 403 for a role
/// without the capability) before doing any work.
pub(crate) fn authorize(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<()> {
    let caller = caller(req)?;
    require_permission(ctx, &caller.role, permission)
}

/// Shared snapshot cache. Enumerating every process with
/// `System::new_all()` on each request is expensive, so the handlers share
/// one snapshot refreshed at most once a second.
pub(crate) static SYSTEM_CACHE: LazyLock<Mutex<CachedSnapshot>> =
    LazyLock::new(|| Mutex::new(CachedSnapshot::new()));


pub(crate) struct CachedSnapshot {
    pub(crate) system: System,
    pub(crate) disks: Disks,
    pub(crate) snapshot: Option<SystemSnapshot>,
    pub(crate) last_refresh: Option<Instant>,
}

impl CachedSnapshot {
    pub(crate) fn new() -> Self {
        Self {
            system: System::new_all(),
            disks: Disks::new_with_refreshed_list(),
            snapshot: None,
            last_refresh: None,
        }
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
    use actix_web::test::TestRequest;
    use std::sync::Arc;

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

    #[test]
    fn caller_rejects_requests_without_a_verified_caller() {
        let anonymous = TestRequest::default().to_http_request();
        assert_eq!(caller(&anonymous).unwrap_err().code, 401);
        let no_role = TestRequest::default().to_http_request();
        assert_eq!(caller(&no_role).unwrap_err().code, 401);
        let no_user = TestRequest::default().to_http_request();
        assert_eq!(caller(&no_user).unwrap_err().code, 401);
        let blank = TestRequest::default().to_http_request();
        assert_eq!(caller(&blank).unwrap_err().code, 401);
    }

    #[test]
    fn caller_accepts_a_verified_caller() {
        let caller = caller(&authed_request()).expect("authed request");
        assert_eq!(caller.actor, test_user_uuid("user-1").to_string());
        assert_eq!(caller.role, "admin");
    }

    #[test]
    fn check_capability_fails_closed_without_handler() {
        let ctx = test_context();
        assert!(!check_capability(&ctx, "admin", PERM_SERVICE_MONITOR_VIEW));
        assert!(!check_capability(&ctx, "admin", PERM_SERVICE_MONITOR_MANAGE));
    }

    #[test]
    fn check_capability_honours_role_check_answer() {
        let ctx = allowing_context();
        assert!(check_capability(&ctx, "admin", PERM_SERVICE_MONITOR_VIEW));
        assert!(check_capability(&ctx, "admin", PERM_SERVICE_MONITOR_MANAGE));
    }

    #[test]
    fn require_permission_denies_without_grant() {
        let ctx = test_context();
        let error = require_permission(&ctx, "admin", PERM_SERVICE_MONITOR_VIEW)
            .unwrap_err();
        assert_eq!(error.code, 403);
    }

    #[test]
    fn authorize_rejects_anonymous_and_unprivileged() {
        let ctx = allowing_context();
        let anonymous = TestRequest::default().to_http_request();
        assert_eq!(
            authorize(&ctx, &anonymous, PERM_SERVICE_MONITOR_VIEW)
                .unwrap_err()
                .code,
            401
        );
        let denied_ctx = test_context();
        assert_eq!(
            authorize(&denied_ctx, &authed_request(), PERM_SERVICE_MONITOR_VIEW)
                .unwrap_err()
                .code,
            403
        );
        assert!(authorize(&ctx, &authed_request(), PERM_SERVICE_MONITOR_MANAGE).is_ok());
    }

    #[test]
    fn process_status_labels_match_unix_letters() {
        assert_eq!(process_status_label(ProcessStatus::Run), "R");
        assert_eq!(process_status_label(ProcessStatus::Sleep), "S");
        assert_eq!(process_status_label(ProcessStatus::Idle), "I");
        assert_eq!(process_status_label(ProcessStatus::Stop), "T");
        assert_eq!(process_status_label(ProcessStatus::Zombie), "Z");
        assert_eq!(process_status_label(ProcessStatus::Tracing), "t");
        assert_eq!(process_status_label(ProcessStatus::Dead), "X");
    }

    #[test]
    fn disk_kind_labels() {
        assert_eq!(disk_kind_label(DiskKind::HDD), "HDD");
        assert_eq!(disk_kind_label(DiskKind::SSD), "SSD");
    }
}
