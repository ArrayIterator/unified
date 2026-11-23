// IoT device manager external module for the Unified server.
//
// One struct (`IotModule`) implementing `unified_core::modules::Module`
// (id `"iot"`), following the exact shape of
// `crates/example-hello-module`: `register(registry)` plus a `module()`
// accessor, routes under `/api/v1/iot/`, and the standard success
// envelope (`{"data": ...}`) on every response.
//
// What this module does
// ---------------------
// - Device registry: devices are provisioned per tenant, each with a
//   unique device key, a lifecycle
//   (provisioned -> active -> suspended -> decommissioned, terminal),
//   a data-driven device type, firmware version, location, and
//   free-form metadata.
// - Per-device API keys: a random key is generated at creation, shown
//   exactly once in the create response, and stored only as its SHA-256
//   hash. It is never returned again and never logged. Device
//   endpoints (telemetry, twin reported state, command polling and
//   acknowledgement) authenticate with it using a constant-time hash
//   comparison.
// - Telemetry: HTTP ingestion with validation, a per-device fixed
//   window rate limit (60/minute), and a bounded ring buffer (last 500
//   points per metric per device). Reads return the points plus
//   min/max/avg/count computed from the stored points only.
// - Device twin: desired vs reported JSON state; the sync status is
//   recomputed on every write.
// - Commands: queued commands with a TTL for devices to poll, plus
//   acknowledgement (acked/failed) and expiry handling.
// - Alert rules: metric threshold rules per device or device type with
//   a cooldown; evaluation runs on every telemetry ingest and fires
//   the `iot.alert.triggered` hook event.
// - Fleet health: per-tenant summary (counts by status, silent
//   devices, out-of-sync twins, open alerts).
//
// What this module honestly does NOT do
// --------------------------------------
// - Ingestion here is HTTP only. No MQTT broker is embedded and no
//   broker connection is opened. The MQTT topic mapping a broker
//   deployment would use is documented in README.md
//   (`unified/iot/{tenant}/{device_key}/telemetry`, and the matching
//   twin/command topics); a broker would translate those topics into
//   calls to these same HTTP endpoints.
// - Storage is in-memory (DashMap). Restarting the server loses
//   devices, telemetry, twins, commands, rules, and alerts. A future
//   persistence step would move the store behind Diesel migrations in
//   this crate's own `init()`; the state type is the single place
//   that would change.
// - Tenancy is enforced by this module itself: every record carries a
//   `tenant_id`, writes require one, and every read filters by it.
//   There is no global view. The tenant comes from the
//   `X-Unified-Tenant` header (the same header name the core
//   middleware uses, `DEFAULT_TENANT_HEADER`) or, where a body or
//   query parameter is the only carrier, from an explicit `tenant_id`
//   field. Admin endpoints never fall back to another tenant's data.
//
// Events emitted (module-owned names; no core event in
// `unified_core::hooks::events` covers device lifecycle or alerts,
// so this module listens to nothing external):
//   - `iot.device.provisioned`   -- a device was created.
//   - `iot.device.decommissioned` -- a device reached the terminal
//     decommissioned status, or was deleted.
//   - `iot.alert.triggered`      -- an alert rule fired on ingest.

use actix_web::{web, HttpRequest, HttpResponse};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};

/// Hook event names emitted by this module.
pub mod events {
    /// Emitted when a device is provisioned (created).
    pub const DEVICE_PROVISIONED: &str = "iot.device.provisioned";
    /// Emitted when a device is decommissioned or deleted.
    pub const DEVICE_DECOMMISSIONED: &str = "iot.device.decommissioned";
    /// Emitted when an alert rule fires on telemetry ingest.
    pub const ALERT_TRIGGERED: &str = "iot.alert.triggered";
}

/// Maximum telemetry points kept per metric per device (ring buffer).
pub const MAX_TELEMETRY_POINTS: usize = 500;
/// Telemetry ingest rate limit: requests per device per minute.
pub const TELEMETRY_RATE_LIMIT_PER_MINUTE: u32 = 60;
/// Telemetry timestamps may not be more than this far in the future.
pub const MAX_FUTURE_SKEW_SECS: i64 = 300;
/// Maximum serialised size of a device metadata object, in bytes.
pub const MAX_METADATA_BYTES: usize = 8 * 1024;
/// Maximum serialised size of a command params object, in bytes.
pub const MAX_COMMAND_PARAMS_BYTES: usize = 4 * 1024;
/// Default silence threshold for fleet health, in seconds (15 minutes).
pub const DEFAULT_SILENT_THRESHOLD_SECS: u64 = 900;
/// Default command time-to-live, in seconds (1 hour).
pub const DEFAULT_COMMAND_TTL_SECS: u64 = 3600;
/// Maximum command time-to-live, in seconds (24 hours).
pub const MAX_COMMAND_TTL_SECS: u64 = 86_400;
/// Maximum alert-rule cooldown, in seconds (24 hours).
pub const MAX_COOLDOWN_SECS: u64 = 86_400;

/// Default device types. This list is data, not an enum: it lives in
/// the state, is listed by `GET /api/v1/iot/device-types`, and can be
/// extended through `POST /api/v1/iot/device-types`.
pub const DEFAULT_DEVICE_TYPES: &[&str] = &[
    "actuator", "camera", "controller", "gateway", "meter", "sensor", "tracker", "other",
];

// Split from the parent file: the implementation now lives
// in the submodules below; the public paths are unchanged
// (this file re-exports everything that used to live here).
pub mod validate;
pub mod types;
pub mod state;
pub mod handlers;
pub mod module;
pub use module::{IotModule, register, module};
pub use state::{IotState};
pub use types::{DeviceStatus, allowed_transition, Device, TelemetryPoint, TelemetryAggregates, compute_aggregates, TwinState, CommandStatus, DeviceCommand, AlertOperator, eval_operator, AlertSeverity, AlertRule, Alert, CreateDeviceInput, UpdateDeviceInput, TelemetryIngestInput, TwinUpdateInput, CreateCommandInput, AckCommandInput, CreateAlertRuleInput, CreateDeviceTypeInput, ListQuery, TelemetryQuery, HealthQuery, CommandsQuery};
pub(crate) use handlers::{ack_command, create_alert_rule, create_device, create_device_type, delete_alert_rule, delete_device, fleet_health, get_device, get_telemetry, get_twin, ingest_telemetry, list_alert_rules, list_alerts, list_commands, list_device_types, list_devices, paginate, queue_command, report_twin, update_device, update_twin};
pub(crate) use validate::{constant_time_eq, generate_device_api_key, hash_device_key, is_valid_metric_name, new_id, now_utc, validate_device_key, validate_json_object_size, validate_tenant_id, validate_text};

// ---------------------------------------------------------------------------
// Unit tests (run with `cargo check -p external-iot --tests` / `cargo test`)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn test_device_input(device_key: &str) -> CreateDeviceInput {
        CreateDeviceInput {
            tenant_id: Some("tenant-a".to_string()),
            device_key: device_key.to_string(),
            name: "Greenhouse Sensor 1".to_string(),
            device_type: "sensor".to_string(),
            firmware_version: "1.2.3".to_string(),
            location: Some("Greenhouse A".to_string()),
            metadata: json!({ "room": "A1" }),
        }
    }

    fn active_device(state: &IotState) -> (Device, String) {
        let (device, plain_key) = state
            .create_device("tenant-a", test_device_input("sensor-001"))
            .expect("create device");
        let device = state
            .update_device(
                "tenant-a",
                &device.id,
                UpdateDeviceInput {
                    name: None,
                    device_type: None,
                    firmware_version: None,
                    status: Some(DeviceStatus::Active),
                    location: None,
                    metadata: None,
                },
            )
            .expect("activate device");
        (device, plain_key)
    }

    fn ingest(
        state: &IotState,
        device: &Device,
        metric: &str,
        value: f64,
        now: DateTime<Utc>,
    ) -> TelemetryPoint {
        state
            .ingest_telemetry(
                device,
                TelemetryIngestInput {
                    metric: metric.to_string(),
                    value,
                    unit: "C".to_string(),
                    timestamp: Some(now),
                },
                now,
            )
            .expect("ingest")
    }

    #[test]
    fn device_key_is_hashed_and_verifies_with_constant_time_compare() {
        let state = IotState::new();
        let (device, plain_key) = state
            .create_device("tenant-a", test_device_input("sensor-001"))
            .expect("create device");
        // The stored hash is not the plaintext key.
        assert_ne!(device.key_hash, plain_key);
        assert_eq!(device.key_hash, hash_device_key(&plain_key));
        // The right key authenticates; a wrong key does not.
        assert!(state.authenticate_device(&device, &plain_key).is_ok());
        assert!(state.authenticate_device(&device, "wrong-key").is_err());
        // The key hash is never serialised into the API view.
        let view = serde_json::to_string(&device).expect("serialise device");
        assert!(!view.contains("key_hash"));
        assert!(!view.contains(&plain_key));
    }

    #[test]
    fn device_key_is_unique_per_tenant_only() {
        let state = IotState::new();
        state
            .create_device("tenant-a", test_device_input("sensor-001"))
            .expect("create in tenant-a");
        // Same key in the same tenant conflicts.
        assert!(state
            .create_device("tenant-a", test_device_input("sensor-001"))
            .is_err());
        // Same key in another tenant is fine.
        assert!(state
            .create_device("tenant-b", test_device_input("sensor-001"))
            .is_ok());
    }

    #[test]
    fn tenant_isolation_is_enforced_on_reads() {
        let state = IotState::new();
        let (device, _) = state
            .create_device("tenant-a", test_device_input("sensor-001"))
            .expect("create device");
        // Another tenant cannot load, update, or list it.
        assert!(state.load_device("tenant-b", &device.id).is_err());
        assert!(state
            .update_device(
                "tenant-b",
                &device.id,
                UpdateDeviceInput {
                    name: Some("Hijacked".to_string()),
                    device_type: None,
                    firmware_version: None,
                    status: None,
                    location: None,
                    metadata: None,
                },
            )
            .is_err());
        assert!(state
            .list_devices("tenant-b", None, None, 1, 20)
            .is_empty());
        assert_eq!(state.list_devices("tenant-a", None, None, 1, 20).len(), 1);
    }

    #[test]
    fn status_lifecycle_rejects_illegal_transitions_and_terminal_state() {
        assert!(allowed_transition(
            DeviceStatus::Provisioned,
            DeviceStatus::Active
        ));
        assert!(allowed_transition(
            DeviceStatus::Active,
            DeviceStatus::Suspended
        ));
        assert!(allowed_transition(
            DeviceStatus::Suspended,
            DeviceStatus::Active
        ));
        assert!(allowed_transition(
            DeviceStatus::Active,
            DeviceStatus::Decommissioned
        ));
        // No way back from decommissioned, and no skipping backwards.
        assert!(!allowed_transition(
            DeviceStatus::Decommissioned,
            DeviceStatus::Active
        ));
        assert!(!allowed_transition(
            DeviceStatus::Suspended,
            DeviceStatus::Provisioned
        ));
        // A same-status update is allowed (a no-op, not an error).
        assert!(allowed_transition(
            DeviceStatus::Provisioned,
            DeviceStatus::Provisioned
        ));
        let state = IotState::new();
        let (device, _) = state
            .create_device("tenant-a", test_device_input("sensor-001"))
            .expect("create device");
        let decommissioned = state
            .update_device(
                "tenant-a",
                &device.id,
                UpdateDeviceInput {
                    name: None,
                    device_type: None,
                    firmware_version: None,
                    status: Some(DeviceStatus::Decommissioned),
                    location: None,
                    metadata: None,
                },
            )
            .expect("decommission");
        assert_eq!(decommissioned.status, DeviceStatus::Decommissioned);
        assert!(state
            .update_device(
                "tenant-a",
                &device.id,
                UpdateDeviceInput {
                    name: None,
                    device_type: None,
                    firmware_version: None,
                    status: Some(DeviceStatus::Active),
                    location: None,
                    metadata: None,
                },
            )
            .is_err());
    }

    #[test]
    fn telemetry_is_rejected_for_non_active_devices_and_future_timestamps() {
        let state = IotState::new();
        let now = now_utc();
        let (device, _) = state
            .create_device("tenant-a", test_device_input("sensor-001"))
            .expect("create device");
        // Still provisioned: no telemetry.
        assert!(state
            .ingest_telemetry(
                &device,
                TelemetryIngestInput {
                    metric: "temperature".to_string(),
                    value: 21.5,
                    unit: "C".to_string(),
                    timestamp: Some(now),
                },
                now,
            )
            .is_err());
        let state2 = IotState::new();
        let (device, _) = active_device(&state2);
        // More than 5 minutes in the future is rejected.
        assert!(state2
            .ingest_telemetry(
                &device,
                TelemetryIngestInput {
                    metric: "temperature".to_string(),
                    value: 21.5,
                    unit: "C".to_string(),
                    timestamp: Some(now + Duration::seconds(301)),
                },
                now,
            )
            .is_err());
        // Exactly at the boundary and non-finite values are rejected too.
        assert!(state2
            .ingest_telemetry(
                &device,
                TelemetryIngestInput {
                    metric: "temperature".to_string(),
                    value: f64::NAN,
                    unit: "C".to_string(),
                    timestamp: Some(now),
                },
                now,
            )
            .is_err());
    }

    #[test]
    fn telemetry_aggregates_are_computed_from_stored_points() {
        let state = IotState::new();
        let now = now_utc();
        let (device, _) = active_device(&state);
        for (offset, value) in [(0, 10.0), (60, 20.0), (120, 30.0)] {
            ingest(
                &state,
                &device,
                "temperature",
                value,
                now + Duration::seconds(offset),
            );
        }
        let (points, aggregates) =
            state.query_telemetry(&device.id, Some("temperature"), None, None, 100);
        assert_eq!(points.len(), 3);
        assert_eq!(aggregates.count, 3);
        assert_eq!(aggregates.min, Some(10.0));
        assert_eq!(aggregates.max, Some(30.0));
        assert_eq!(aggregates.avg, Some(20.0));
        // A metric with no points reports empty aggregates, not zeroes.
        let (points, aggregates) =
            state.query_telemetry(&device.id, Some("humidity"), None, None, 100);
        assert!(points.is_empty());
        assert_eq!(aggregates.count, 0);
        assert_eq!(aggregates.min, None);
        assert_eq!(aggregates.avg, None);
        // A time range filters honestly.
        let (points, aggregates) = state.query_telemetry(
            &device.id,
            Some("temperature"),
            Some(now + Duration::seconds(60)),
            Some(now + Duration::seconds(120)),
            100,
        );
        assert_eq!(points.len(), 2);
        assert_eq!(aggregates.count, 2);
        assert_eq!(aggregates.avg, Some(25.0));
    }

    #[test]
    fn rate_limit_allows_sixty_per_minute_then_blocks() {
        let state = IotState::new();
        let device_id = "device-1";
        let window = 1_700_000_000u64 / 60;
        for _ in 0..TELEMETRY_RATE_LIMIT_PER_MINUTE {
            assert!(state.check_rate_limit(device_id, window * 60).is_ok());
        }
        assert!(state.check_rate_limit(device_id, window * 60).is_err());
        // The next minute window resets the budget.
        assert!(state
            .check_rate_limit(device_id, (window + 1) * 60)
            .is_ok());
    }

    #[test]
    fn twin_sync_is_recomputed_on_every_write() {
        let state = IotState::new();
        let (device, _) = active_device(&state);
        let now = now_utc();
        // Fresh twin is in sync (both empty).
        assert!(state.get_twin(&device.id).in_sync());
        // Setting desired state marks it out of sync.
        let twin = state
            .update_twin(
                &device,
                TwinUpdateInput {
                    desired: Some(json!({ "target_temp": 22.0 })),
                    reported: None,
                },
                now,
                false,
            )
            .expect("set desired");
        assert!(!twin.in_sync());
        // Reporting the same state brings it back in sync.
        let twin = state
            .update_twin(
                &device,
                TwinUpdateInput {
                    desired: None,
                    reported: Some(json!({ "target_temp": 22.0 })),
                },
                now,
                true,
            )
            .expect("set reported");
        assert!(twin.in_sync());
        // Reporting a different state goes out of sync again.
        let twin = state
            .update_twin(
                &device,
                TwinUpdateInput {
                    desired: None,
                    reported: Some(json!({ "target_temp": 19.5 })),
                },
                now,
                true,
            )
            .expect("set reported again");
        assert!(!twin.in_sync());
    }

    #[test]
    fn alert_rule_fires_once_per_cooldown_then_fires_again() {
        let state = IotState::new();
        let now = now_utc();
        let (device, _) = active_device(&state);
        let rule = state
            .create_alert_rule(
                "tenant-a",
                CreateAlertRuleInput {
                    tenant_id: Some("tenant-a".to_string()),
                    device_id: Some(device.id.clone()),
                    device_type: None,
                    metric: "temperature".to_string(),
                    operator: AlertOperator::Greater,
                    threshold: 30.0,
                    cooldown_secs: 600,
                    severity: AlertSeverity::Critical,
                },
            )
            .expect("create rule");
        // Below threshold: no alert.
        ingest(&state, &device, "temperature", 25.0, now);
        assert!(state.list_alerts("tenant-a", None, None, 1, 20).is_empty());
        // Above threshold: one alert.
        ingest(&state, &device, "temperature", 31.0, now);
        assert_eq!(state.list_alerts("tenant-a", None, None, 1, 20).len(), 1);
        // Still above threshold but inside the cooldown: no second alert.
        ingest(
            &state,
            &device,
            "temperature",
            32.0,
            now + Duration::seconds(60),
        );
        assert_eq!(state.list_alerts("tenant-a", None, None, 1, 20).len(), 1);
        // After the cooldown: fires again.
        ingest(
            &state,
            &device,
            "temperature",
            33.0,
            now + Duration::seconds(601),
        );
        assert_eq!(state.list_alerts("tenant-a", None, None, 1, 20).len(), 2);
        // The stored alert carries the rule and the triggering value.
        let alerts = state.list_alerts("tenant-a", None, None, 1, 20);
        assert_eq!(alerts[0].rule_id, rule.id);
        assert_eq!(alerts[0].value, 33.0);
        assert_eq!(alerts[0].severity, AlertSeverity::Critical);
    }

    #[test]
    fn alert_rule_by_device_type_matches_every_device_of_that_type() {
        let state = IotState::new();
        let now = now_utc();
        let (device, _) = active_device(&state);
        state
            .create_alert_rule(
                "tenant-a",
                CreateAlertRuleInput {
                    tenant_id: Some("tenant-a".to_string()),
                    device_id: None,
                    device_type: Some("sensor".to_string()),
                    metric: "temperature".to_string(),
                    operator: AlertOperator::GreaterOrEqual,
                    threshold: 30.0,
                    cooldown_secs: 0,
                    severity: AlertSeverity::Warning,
                },
            )
            .expect("create rule");
        ingest(&state, &device, "temperature", 30.0, now);
        assert_eq!(state.list_alerts("tenant-a", None, None, 1, 20).len(), 1);
        // A rule with neither device nor type is rejected.
        assert!(state
            .create_alert_rule(
                "tenant-a",
                CreateAlertRuleInput {
                    tenant_id: Some("tenant-a".to_string()),
                    device_id: None,
                    device_type: None,
                    metric: "temperature".to_string(),
                    operator: AlertOperator::Greater,
                    threshold: 30.0,
                    cooldown_secs: 0,
                    severity: AlertSeverity::Warning,
                },
            )
            .is_err());
    }

    #[test]
    fn commands_expire_and_ack_moves_to_terminal_state() {
        let state = IotState::new();
        let now = now_utc();
        let (device, _) = active_device(&state);
        let command = state
            .queue_command(
                &device,
                CreateCommandInput {
                    name: "reboot".to_string(),
                    params: json!({ "delay_secs": 5 }),
                    ttl_secs: 60,
                },
                now,
            )
            .expect("queue command");
        assert_eq!(command.status, CommandStatus::Queued);
        // Polling sees the queued command.
        assert_eq!(state.list_commands(&device, true, now).len(), 1);
        // Acknowledging moves it to a terminal state; a second ack fails.
        let acked = state
            .ack_command(
                &device,
                &command.id,
                AckCommandInput {
                    status: "acked".to_string(),
                    result: Some(json!({ "ok": true })),
                },
                now,
            )
            .expect("ack");
        assert_eq!(acked.status, CommandStatus::Acked);
        assert!(state
            .ack_command(
                &device,
                &command.id,
                AckCommandInput {
                    status: "acked".to_string(),
                    result: None,
                },
                now,
            )
            .is_err());
        // A queued command past its TTL is reported as expired.
        let short = state
            .queue_command(
                &device,
                CreateCommandInput {
                    name: "ping".to_string(),
                    params: json!({}),
                    ttl_secs: 1,
                },
                now,
            )
            .expect("queue short command");
        let later = now + Duration::seconds(120);
        let listed = state.list_commands(&device, false, later);
        let expired = listed
            .iter()
            .find(|c| c.id == short.id)
            .expect("expired command listed");
        assert_eq!(expired.status, CommandStatus::Expired);
        assert!(state
            .ack_command(
                &device,
                &short.id,
                AckCommandInput {
                    status: "acked".to_string(),
                    result: None,
                },
                later,
            )
            .is_err());
    }

    #[test]
    fn module_contract_matches_the_example_shape() {
        let module = IotModule::new();
        assert_eq!(module.id(), "iot");
        assert_eq!(module.version(), "0.1.0");
        assert!(!module.is_core());
        assert!(module.requires().is_empty());
        let routes = module.route_info();
        assert!(routes.iter().any(|r| r.path == "/api/v1/iot/health"));
        assert!(routes
            .iter()
            .any(|r| r.path == "/api/v1/iot/devices/{id}/telemetry" && r.method == "POST"));
        assert!(routes.iter().all(|r| r.owner == "iot"));
        assert!(routes.iter().all(|r| r.path.starts_with("/api/v1/iot/")));
        // The `module()` accessor (shadowed above by the local binding,
        // so it is called through the crate root) returns a working
        // trait object with the same id.
        let via_accessor = crate::module();
        assert_eq!(via_accessor.id(), "iot");
    }
}
