//! IoT external module (split from lib.rs; moved verbatim).

use super::*;


// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

/// Device lifecycle. `Decommissioned` is terminal: no transition out
/// of it exists, and decommissioned devices reject telemetry,
/// commands, and twin writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceStatus {
    Provisioned,
    Active,
    Suspended,
    Decommissioned,
}

impl DeviceStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeviceStatus::Provisioned => "provisioned",
            DeviceStatus::Active => "active",
            DeviceStatus::Suspended => "suspended",
            DeviceStatus::Decommissioned => "decommissioned",
        }
    }
}

/// True when `to` is a legal status transition from `from`.
/// Same-status updates are allowed (a no-op, not an error).
pub fn allowed_transition(from: DeviceStatus, to: DeviceStatus) -> bool {
    if from == to {
        return true;
    }
    matches!(
        (from, to),
        (DeviceStatus::Provisioned, DeviceStatus::Active)
            | (DeviceStatus::Provisioned, DeviceStatus::Suspended)
            | (DeviceStatus::Provisioned, DeviceStatus::Decommissioned)
            | (DeviceStatus::Active, DeviceStatus::Suspended)
            | (DeviceStatus::Active, DeviceStatus::Decommissioned)
            | (DeviceStatus::Suspended, DeviceStatus::Active)
            | (DeviceStatus::Suspended, DeviceStatus::Decommissioned)
    )
}

/// One registered device. `key_hash` is never serialised: the API
/// never returns it, and the plaintext key is never stored at all.
#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub id: String,
    pub tenant_id: String,
    pub device_key: String,
    pub name: String,
    pub device_type: String,
    pub firmware_version: String,
    pub status: DeviceStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    pub metadata: Value,
    #[serde(skip_serializing)]
    pub key_hash: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<DateTime<Utc>>,
}

/// One stored telemetry point.
#[derive(Debug, Clone, Serialize)]
pub struct TelemetryPoint {
    pub metric: String,
    pub value: f64,
    pub unit: String,
    pub timestamp: DateTime<Utc>,
}

/// Aggregates computed from the stored points only. `min`, `max`,
/// and `avg` are null when no points matched.
#[derive(Debug, Clone, Serialize)]
pub struct TelemetryAggregates {
    pub count: u64,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub avg: Option<f64>,
}

pub fn compute_aggregates(points: &[TelemetryPoint]) -> TelemetryAggregates {
    if points.is_empty() {
        return TelemetryAggregates {
            count: 0,
            min: None,
            max: None,
            avg: None,
        };
    }
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut sum = 0.0f64;
    for point in points {
        if point.value < min {
            min = point.value;
        }
        if point.value > max {
            max = point.value;
        }
        sum += point.value;
    }
    TelemetryAggregates {
        count: points.len() as u64,
        min: Some(min),
        max: Some(max),
        avg: Some(sum / points.len() as f64),
    }
}

/// Desired vs reported device state. `in_sync` is derived, never
/// stored: it is true exactly when desired and reported are deeply
/// equal JSON values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwinState {
    #[serde(default = "empty_object")]
    pub desired: Value,
    #[serde(default = "empty_object")]
    pub reported: Value,
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

impl Default for TwinState {
    fn default() -> Self {
        Self {
            desired: empty_object(),
            reported: empty_object(),
        }
    }
}

impl TwinState {
    pub fn in_sync(&self) -> bool {
        self.desired == self.reported
    }

    pub fn view(&self) -> Value {
        json!({
            "desired": self.desired,
            "reported": self.reported,
            "in_sync": self.in_sync(),
        })
    }
}

/// Command lifecycle. `Expired` is computed, not stored: a queued
/// command whose `expires_at` has passed is reported (and stored,
/// on first observation) as expired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Queued,
    Acked,
    Failed,
    Expired,
}

impl CommandStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            CommandStatus::Queued => "queued",
            CommandStatus::Acked => "acked",
            CommandStatus::Failed => "failed",
            CommandStatus::Expired => "expired",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceCommand {
    pub id: String,
    pub tenant_id: String,
    pub device_id: String,
    pub name: String,
    pub params: Value,
    pub status: CommandStatus,
    pub ttl_secs: u64,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acked_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

impl DeviceCommand {
    /// The status a reader should see right now. A queued command
    /// past its expiry is expired; terminal states never change.
    pub fn effective_status(&self, now: DateTime<Utc>) -> CommandStatus {
        if self.status == CommandStatus::Queued && now > self.expires_at {
            CommandStatus::Expired
        } else {
            self.status
        }
    }
}

/// Threshold comparison operator for alert rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlertOperator {
    #[serde(rename = ">")]
    Greater,
    #[serde(rename = "<")]
    Less,
    #[serde(rename = ">=")]
    GreaterOrEqual,
    #[serde(rename = "<=")]
    LessOrEqual,
    #[serde(rename = "==")]
    Equal,
}

impl AlertOperator {
    pub fn as_str(&self) -> &'static str {
        match self {
            AlertOperator::Greater => ">",
            AlertOperator::Less => "<",
            AlertOperator::GreaterOrEqual => ">=",
            AlertOperator::LessOrEqual => "<=",
            AlertOperator::Equal => "==",
        }
    }
}

/// Pure threshold evaluation, unit-tested. `==` uses exact float
/// equality on values the device reported; rules that need a
/// tolerance should use `>=`/`<=` instead.
pub fn eval_operator(op: AlertOperator, value: f64, threshold: f64) -> bool {
    match op {
        AlertOperator::Greater => value > threshold,
        AlertOperator::Less => value < threshold,
        AlertOperator::GreaterOrEqual => value >= threshold,
        AlertOperator::LessOrEqual => value <= threshold,
        AlertOperator::Equal => value == threshold,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertSeverity {
    Info,
    Warning,
    Critical,
}

impl AlertSeverity {
    pub fn as_str(&self) -> &'static str {
        match self {
            AlertSeverity::Info => "info",
            AlertSeverity::Warning => "warning",
            AlertSeverity::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AlertRule {
    pub id: String,
    pub tenant_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_type: Option<String>,
    pub metric: String,
    pub operator: AlertOperator,
    pub threshold: f64,
    pub cooldown_secs: u64,
    pub severity: AlertSeverity,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_fired_at: Option<DateTime<Utc>>,
}

impl AlertRule {
    /// True when this rule applies to the given device and metric.
    /// A rule with a device id matches that device only; a rule with
    /// only a device type matches every device of that type; a rule
    /// with both must match both.
    pub fn matches(&self, device: &Device, metric: &str) -> bool {
        if self.metric != metric {
            return false;
        }
        if let Some(device_id) = &self.device_id
            && *device_id != device.id
        {
            return false;
        }
        if let Some(device_type) = &self.device_type
            && *device_type != device.device_type
        {
            return false;
        }
        true
    }

    /// True when the cooldown has passed at `now`.
    pub fn cooldown_passed(&self, now: DateTime<Utc>) -> bool {
        match self.last_fired_at {
            None => true,
            Some(last) => {
                now.signed_duration_since(last).num_seconds() >= self.cooldown_secs as i64
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Alert {
    pub id: String,
    pub tenant_id: String,
    pub device_id: String,
    pub rule_id: String,
    pub metric: String,
    pub value: f64,
    pub threshold: f64,
    pub operator: AlertOperator,
    pub severity: AlertSeverity,
    pub message: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Request inputs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateDeviceInput {
    pub tenant_id: Option<String>,
    pub device_key: String,
    pub name: String,
    pub device_type: String,
    #[serde(default = "default_firmware_version")]
    pub firmware_version: String,
    pub location: Option<String>,
    #[serde(default = "empty_object")]
    pub metadata: Value,
}

fn default_firmware_version() -> String {
    "unknown".to_string()
}

#[derive(Debug, Deserialize)]
pub struct UpdateDeviceInput {
    pub name: Option<String>,
    pub device_type: Option<String>,
    pub firmware_version: Option<String>,
    pub status: Option<DeviceStatus>,
    pub location: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct TelemetryIngestInput {
    pub metric: String,
    pub value: f64,
    pub unit: String,
    /// Optional point timestamp (RFC 3339). Defaults to ingest time.
    /// Rejected when more than 5 minutes in the future.
    pub timestamp: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct TwinUpdateInput {
    pub desired: Option<Value>,
    pub reported: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct CreateCommandInput {
    pub name: String,
    #[serde(default = "empty_object")]
    pub params: Value,
    #[serde(default = "default_command_ttl")]
    pub ttl_secs: u64,
}

fn default_command_ttl() -> u64 {
    DEFAULT_COMMAND_TTL_SECS
}

#[derive(Debug, Deserialize)]
pub struct AckCommandInput {
    /// `"acked"` or `"failed"`.
    pub status: String,
    pub result: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct CreateAlertRuleInput {
    pub tenant_id: Option<String>,
    pub device_id: Option<String>,
    pub device_type: Option<String>,
    pub metric: String,
    pub operator: AlertOperator,
    pub threshold: f64,
    #[serde(default)]
    pub cooldown_secs: u64,
    #[serde(default = "default_severity")]
    pub severity: AlertSeverity,
}

fn default_severity() -> AlertSeverity {
    AlertSeverity::Warning
}

#[derive(Debug, Deserialize)]
pub struct CreateDeviceTypeInput {
    pub tenant_id: Option<String>,
    pub device_type: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub tenant_id: Option<String>,
    pub status: Option<String>,
    pub device_type: Option<String>,
    pub device_id: Option<String>,
    pub severity: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
pub struct TelemetryQuery {
    pub tenant_id: Option<String>,
    pub metric: Option<String>,
    /// Inclusive lower bound: epoch seconds or RFC 3339.
    pub from: Option<String>,
    /// Inclusive upper bound: epoch seconds or RFC 3339.
    pub to: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
pub struct HealthQuery {
    pub tenant_id: Option<String>,
    pub silent_threshold_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct CommandsQuery {
    /// `"queued"` (default) or `"all"`.
    pub status: Option<String>,
}
