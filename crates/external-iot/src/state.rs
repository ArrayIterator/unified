//! IoT external module.

use super::*;


// In-memory store

/// The whole module state. Every map is keyed or filtered by
/// `tenant_id`; there is no global view. The hook bus is attached by
/// `init()` and is `None` (events silently dropped) when the module
/// runs standalone, for example in unit tests.
pub struct IotState {
    pub devices: DashMap<String, Device>,
    /// (tenant_id, device_key) -> device id. Enforces the
    /// per-tenant uniqueness of device keys.
    pub device_ids_by_key: DashMap<(String, String), String>,
    /// device id -> per-metric ring buffers (last 500 points each).
    pub telemetry: DashMap<String, Mutex<HashMap<String, VecDeque<TelemetryPoint>>>>,
    pub twins: DashMap<String, TwinState>,
    pub commands: DashMap<String, DeviceCommand>,
    pub alert_rules: DashMap<String, AlertRule>,
    pub alerts: DashMap<String, Alert>,
    /// device id -> (window start, epoch minute) and count in it.
    pub rate_limits: DashMap<String, Mutex<(u64, u32)>>,
    pub device_types: RwLock<Vec<String>>,
    pub hooks: RwLock<Option<Arc<HookBus>>>,
}

impl Default for IotState {
    fn default() -> Self {
        Self::new()
    }
}

impl IotState {
    pub fn new() -> Self {
        Self {
            devices: DashMap::new(),
            device_ids_by_key: DashMap::new(),
            telemetry: DashMap::new(),
            twins: DashMap::new(),
            commands: DashMap::new(),
            alert_rules: DashMap::new(),
            alerts: DashMap::new(),
            rate_limits: DashMap::new(),
            device_types: RwLock::new(
                DEFAULT_DEVICE_TYPES
                    .iter()
                    .map(|t| t.to_string())
                    .collect(),
            ),
            hooks: RwLock::new(None),
        }
    }

    pub fn set_hooks(&self, bus: Arc<HookBus>) {
        if let Ok(mut guard) = self.hooks.write() {
            *guard = Some(bus);
        }
    }

    pub fn emit(&self, event: &str, data: Value) {
        let bus = self
            .hooks
            .read()
            .ok()
            .and_then(|guard| guard.clone());
        if let Some(bus) = bus {
            bus.emit(event, data);
        }
    }

    pub fn device_types(&self) -> Vec<String> {
        self.device_types
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_else(|_| {
                DEFAULT_DEVICE_TYPES
                    .iter()
                    .map(|t| t.to_string())
                    .collect()
            })
    }

    // -- devices ---------------------------------------------------------

    /// Create a device. Returns the device plus the plaintext API key,
    /// which the caller must show exactly once: it is never stored.
    pub fn create_device(
        &self,
        tenant_id: &str,
        input: CreateDeviceInput,
    ) -> UnifiedResult<(Device, String)> {
        validate_tenant_id(tenant_id)?;
        validate_device_key(&input.device_key)?;
        validate_text(&input.name, "name", 128)?;
        validate_text(&input.device_type, "device_type", 64)?;
        validate_text(&input.firmware_version, "firmware_version", 64)?;
        if let Some(location) = &input.location {
            validate_text(location, "location", 256)?;
        }
        validate_json_object_size(&input.metadata, "metadata", MAX_METADATA_BYTES)?;
        if !self.device_types().iter().any(|t| t == &input.device_type) {
            return Err(UnifiedError::bad_request(format!(
                "device_type '{}' is not in the configured device type list; see GET /api/v1/iot/device-types",
                input.device_type
            )));
        }
        if self
            .device_ids_by_key
            .contains_key(&(tenant_id.to_string(), input.device_key.clone()))
        {
            return Err(UnifiedError::conflict(format!(
                "device_key '{}' already exists in this tenant",
                input.device_key
            )));
        }
        let plain_key = generate_device_api_key();
        let now = now_utc();
        let device = Device {
            id: new_id(),
            tenant_id: tenant_id.to_string(),
            device_key: input.device_key.clone(),
            name: input.name,
            device_type: input.device_type,
            firmware_version: input.firmware_version,
            status: DeviceStatus::Provisioned,
            location: input.location,
            metadata: input.metadata,
            key_hash: hash_device_key(&plain_key),
            created_at: now,
            updated_at: now,
            last_seen_at: None,
        };
        self.device_ids_by_key.insert(
            (tenant_id.to_string(), input.device_key),
            device.id.clone(),
        );
        self.devices.insert(device.id.clone(), device.clone());
        self.twins.insert(device.id.clone(), TwinState::default());
        self.emit(
            events::DEVICE_PROVISIONED,
            json!({
                "device_id": device.id,
                "tenant_id": device.tenant_id,
                "device_key": device.device_key,
                "device_type": device.device_type,
            }),
        );
        Ok((device, plain_key))
    }

    /// Load a device, scoped to a tenant. A missing device and a
    /// device belonging to another tenant both report 404, so tenant
    /// membership is never leaked through the error.
    pub fn load_device(&self, tenant_id: &str, id: &str) -> UnifiedResult<Device> {
        match self.devices.get(id) {
            Some(entry) if entry.tenant_id == tenant_id => Ok(entry.clone()),
            _ => Err(UnifiedError::not_found("device not found")),
        }
    }

    /// Load a device by id for device-key authentication. The caller
    /// must still authenticate the key; the tenant is taken from the
    /// device record itself, never from the request.
    pub fn load_device_any_tenant(&self, id: &str) -> UnifiedResult<Device> {
        match self.devices.get(id) {
            Some(entry) => Ok(entry.clone()),
            None => Err(UnifiedError::not_found("device not found")),
        }
    }

    pub fn list_devices(
        &self,
        tenant_id: &str,
        status: Option<DeviceStatus>,
        device_type: Option<&str>,
        page: u32,
        per_page: u32,
    ) -> Vec<Device> {
        let mut items: Vec<Device> = self
            .devices
            .iter()
            .filter(|entry| entry.tenant_id == tenant_id)
            .filter(|entry| status.is_none_or(|s| entry.status == s))
            .filter(|entry| device_type.is_none_or(|t| entry.device_type == t))
            .map(|entry| entry.clone())
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        paginate(items, page, per_page)
    }

    pub fn update_device(
        &self,
        tenant_id: &str,
        id: &str,
        input: UpdateDeviceInput,
    ) -> UnifiedResult<Device> {
        let mut device = self.load_device(tenant_id, id)?;
        if let Some(name) = input.name {
            validate_text(&name, "name", 128)?;
            device.name = name;
        }
        if let Some(device_type) = input.device_type {
            validate_text(&device_type, "device_type", 64)?;
            if !self.device_types().iter().any(|t| t == &device_type) {
                return Err(UnifiedError::bad_request(format!(
                    "device_type '{device_type}' is not in the configured device type list"
                )));
            }
            device.device_type = device_type;
        }
        if let Some(firmware_version) = input.firmware_version {
            validate_text(&firmware_version, "firmware_version", 64)?;
            device.firmware_version = firmware_version;
        }
        if let Some(location) = input.location {
            validate_text(&location, "location", 256)?;
            device.location = Some(location);
        }
        if let Some(metadata) = input.metadata {
            validate_json_object_size(&metadata, "metadata", MAX_METADATA_BYTES)?;
            device.metadata = metadata;
        }
        let mut decommissioned = false;
        if let Some(status) = input.status {
            if !allowed_transition(device.status, status) {
                return Err(UnifiedError::conflict(format!(
                    "cannot move device from {} to {}",
                    device.status.as_str(),
                    status.as_str()
                )));
            }
            if status == DeviceStatus::Decommissioned
                && device.status != DeviceStatus::Decommissioned
            {
                decommissioned = true;
            }
            device.status = status;
        }
        device.updated_at = now_utc();
        self.devices.insert(device.id.clone(), device.clone());
        if decommissioned {
            self.emit(
                events::DEVICE_DECOMMISSIONED,
                json!({
                    "device_id": device.id,
                    "tenant_id": device.tenant_id,
                    "device_key": device.device_key,
                }),
            );
        }
        Ok(device)
    }

    pub fn delete_device(&self, tenant_id: &str, id: &str) -> UnifiedResult<Device> {
        let device = self.load_device(tenant_id, id)?;
        self.devices.remove(id);
        self.device_ids_by_key
            .remove(&(device.tenant_id.clone(), device.device_key.clone()));
        self.telemetry.remove(id);
        self.twins.remove(id);
        self.rate_limits.remove(id);
        let command_ids: Vec<String> = self
            .commands
            .iter()
            .filter(|entry| entry.device_id == id)
            .map(|entry| entry.id.clone())
            .collect();
        for command_id in command_ids {
            self.commands.remove(&command_id);
        }
        self.emit(
            events::DEVICE_DECOMMISSIONED,
            json!({
                "device_id": device.id,
                "tenant_id": device.tenant_id,
                "device_key": device.device_key,
                "deleted": true,
            }),
        );
        Ok(device)
    }

    // -- device authentication -------------------------------------------

    /// Authenticate a presented device API key against the stored
    /// SHA-256 hash with a constant-time comparison. The plaintext key
    /// is hashed, never compared directly, and never logged.
    pub fn authenticate_device(&self, device: &Device, presented_key: &str) -> UnifiedResult<()> {
        let presented_hash = hash_device_key(presented_key);
        if constant_time_eq(presented_hash.as_bytes(), device.key_hash.as_bytes()) {
            Ok(())
        } else {
            Err(UnifiedError::unauthorized("invalid device key"))
        }
    }

    /// Fixed-window rate limit: at most
    /// `TELEMETRY_RATE_LIMIT_PER_MINUTE` ingests per device per
    /// minute. Returns 429 when the window is exhausted.
    pub fn check_rate_limit(&self, device_id: &str, now_secs: u64) -> UnifiedResult<()> {
        let window = now_secs / 60;
        let entry = self
            .rate_limits
            .entry(device_id.to_string())
            .or_insert_with(|| Mutex::new((window, 0)));
        let mut guard = entry
            .lock()
            .map_err(|_| UnifiedError::internal("rate limit lock poisoned"))?;
        if guard.0 != window {
            *guard = (window, 0);
        }
        if guard.1 >= TELEMETRY_RATE_LIMIT_PER_MINUTE {
            return Err(UnifiedError::new(
                "telemetry rate limit exceeded (60 requests per minute per device)",
                429,
            ));
        }
        guard.1 += 1;
        Ok(())
    }

    // -- telemetry ---------------------------------------------------------

    /// Validate and ingest one telemetry point. Only `active` devices
    /// may send telemetry; provisioned, suspended, and decommissioned
    /// devices are rejected so a device cannot report before it is
    /// activated or after it is retired.
    pub fn ingest_telemetry(
        &self,
        device: &Device,
        input: TelemetryIngestInput,
        now: DateTime<Utc>,
    ) -> UnifiedResult<TelemetryPoint> {
        if device.status != DeviceStatus::Active {
            return Err(UnifiedError::conflict(format!(
                "device is {} and cannot send telemetry; only active devices can",
                device.status.as_str()
            )));
        }
        if !is_valid_metric_name(&input.metric) {
            return Err(UnifiedError::bad_request(
                "metric is invalid: 1-64 characters, letters, digits, '-', '_' '.' or '/'",
            ));
        }
        if !input.value.is_finite() {
            return Err(UnifiedError::bad_request("value must be a finite number"));
        }
        validate_text(&input.unit, "unit", 32)?;
        let timestamp = input.timestamp.unwrap_or(now);
        if timestamp.signed_duration_since(now).num_seconds() > MAX_FUTURE_SKEW_SECS {
            return Err(UnifiedError::unprocessable(
                "timestamp is more than 5 minutes in the future",
            ));
        }
        self.check_rate_limit(&device.id, now.timestamp() as u64)?;
        let point = TelemetryPoint {
            metric: input.metric.clone(),
            value: input.value,
            unit: input.unit.clone(),
            timestamp,
        };
        {
            let entry = self
                .telemetry
                .entry(device.id.clone())
                .or_insert_with(|| Mutex::new(HashMap::new()));
            let mut guard = entry
                .lock()
                .map_err(|_| UnifiedError::internal("telemetry lock poisoned"))?;
            let buffer = guard.entry(input.metric.clone()).or_default();
            buffer.push_back(point.clone());
            while buffer.len() > MAX_TELEMETRY_POINTS {
                buffer.pop_front();
            }
        }
        if let Some(mut stored) = self.devices.get_mut(&device.id) {
            stored.last_seen_at = Some(now);
        }
        self.evaluate_alert_rules(device, &point, now);
        Ok(point)
    }

    /// Return stored points for a device, optionally filtered by
    /// metric and time range, plus aggregates over exactly those
    /// points. Points are returned oldest first.
    pub fn query_telemetry(
        &self,
        device_id: &str,
        metric: Option<&str>,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        limit: usize,
    ) -> (Vec<TelemetryPoint>, TelemetryAggregates) {
        let mut points: Vec<TelemetryPoint> = Vec::new();
        if let Some(entry) = self.telemetry.get(device_id) {
            if let Ok(guard) = entry.lock() {
            let metrics: Vec<&String> = match metric {
                Some(name) => guard.keys().filter(|key| *key == name).collect(),
                None => guard.keys().collect(),
            };
            for name in metrics {
                if let Some(buffer) = guard.get(name) {
                    for point in buffer.iter() {
                        if from.is_some_and(|from| point.timestamp < from) {
                            continue;
                        }
                        if to.is_some_and(|to| point.timestamp > to) {
                            continue;
                        }
                        points.push(point.clone());
                    }
                }
            }
            }
        }
        points.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        let aggregates = compute_aggregates(&points);
        points.truncate(limit);
        (points, aggregates)
    }

    // -- twins -------------------------------------------------------------

    pub fn get_twin(&self, device_id: &str) -> TwinState {
        self.twins
            .get(device_id)
            .map(|entry| entry.clone())
            .unwrap_or_default()
    }

    pub fn update_twin(
        &self,
        device: &Device,
        input: TwinUpdateInput,
        now: DateTime<Utc>,
        from_device: bool,
    ) -> UnifiedResult<TwinState> {
        if device.status == DeviceStatus::Decommissioned {
            return Err(UnifiedError::conflict(
                "decommissioned device twin cannot be updated",
            ));
        }
        if let Some(desired) = &input.desired {
            validate_json_object_size(desired, "desired", MAX_METADATA_BYTES)?;
        }
        if let Some(reported) = &input.reported {
            validate_json_object_size(reported, "reported", MAX_METADATA_BYTES)?;
        }
        let mut twin = self.get_twin(&device.id);
        if let Some(desired) = input.desired {
            twin.desired = desired;
        }
        if let Some(reported) = input.reported {
            twin.reported = reported;
        }
        self.twins.insert(device.id.clone(), twin.clone());
        if from_device {
            if let Some(mut stored) = self.devices.get_mut(&device.id) {
                stored.last_seen_at = Some(now);
            }
        }
        Ok(twin)
    }

    // -- commands ------------------------------------------------------------

    pub fn queue_command(
        &self,
        device: &Device,
        input: CreateCommandInput,
        now: DateTime<Utc>,
    ) -> UnifiedResult<DeviceCommand> {
        if device.status == DeviceStatus::Decommissioned {
            return Err(UnifiedError::conflict(
                "cannot queue a command for a decommissioned device",
            ));
        }
        validate_text(&input.name, "name", 64)?;
        if !input
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return Err(UnifiedError::bad_request(
                "name is invalid: letters, digits, '-', '_' or '.' only",
            ));
        }
        validate_json_object_size(&input.params, "params", MAX_COMMAND_PARAMS_BYTES)?;
        if input.ttl_secs == 0 || input.ttl_secs > MAX_COMMAND_TTL_SECS {
            return Err(UnifiedError::bad_request(format!(
                "ttl_secs must be 1-{MAX_COMMAND_TTL_SECS}"
            )));
        }
        let command = DeviceCommand {
            id: new_id(),
            tenant_id: device.tenant_id.clone(),
            device_id: device.id.clone(),
            name: input.name,
            params: input.params,
            status: CommandStatus::Queued,
            ttl_secs: input.ttl_secs,
            expires_at: now + chrono::Duration::seconds(input.ttl_secs as i64),
            created_at: now,
            acked_at: None,
            result: None,
        };
        self.commands.insert(command.id.clone(), command.clone());
        Ok(command)
    }

    /// List a device's commands with their effective status. Expired
    /// queued commands are persisted as expired on first observation,
    /// so a later read reports the same terminal state.
    pub fn list_commands(
        &self,
        device: &Device,
        queued_only: bool,
        now: DateTime<Utc>,
    ) -> Vec<DeviceCommand> {
        // Snapshot first, then persist expiry: mutating the map while
        // iterating it would deadlock on the DashMap shard lock, so
        // the expiry write happens after the iteration is done.
        let mut items: Vec<DeviceCommand> = self
            .commands
            .iter()
            .filter(|entry| entry.device_id == device.id)
            .map(|entry| entry.clone())
            .collect();
        for command in &mut items {
            let effective = command.effective_status(now);
            if effective == CommandStatus::Expired && command.status == CommandStatus::Queued {
                if let Some(mut stored) = self.commands.get_mut(&command.id) {
                    stored.status = CommandStatus::Expired;
                }
                command.status = CommandStatus::Expired;
            }
        }
        if queued_only {
            items.retain(|command| command.status == CommandStatus::Queued);
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    pub fn ack_command(
        &self,
        device: &Device,
        command_id: &str,
        input: AckCommandInput,
        now: DateTime<Utc>,
    ) -> UnifiedResult<DeviceCommand> {
        let status = match input.status.as_str() {
            "acked" => CommandStatus::Acked,
            "failed" => CommandStatus::Failed,
            _ => {
                return Err(UnifiedError::bad_request(
                    "status must be \"acked\" or \"failed\"",
                ));
            }
        };
        if let Some(result) = &input.result {
            validate_json_object_size(result, "result", MAX_COMMAND_PARAMS_BYTES)?;
        }
        let mut stored = self
            .commands
            .get_mut(command_id)
            .filter(|entry| entry.device_id == device.id)
            .ok_or_else(|| UnifiedError::not_found("command not found"))?;
        if stored.effective_status(now) == CommandStatus::Expired {
            stored.status = CommandStatus::Expired;
            return Err(UnifiedError::conflict("command has expired"));
        }
        if stored.status != CommandStatus::Queued {
            return Err(UnifiedError::conflict(format!(
                "command is already {} and cannot be acknowledged again",
                stored.status.as_str()
            )));
        }
        stored.status = status;
        stored.acked_at = Some(now);
        stored.result = input.result;
        Ok(stored.clone())
    }

    // -- alert rules and alerts ----------------------------------------------

    pub fn create_alert_rule(
        &self,
        tenant_id: &str,
        input: CreateAlertRuleInput,
    ) -> UnifiedResult<AlertRule> {
        validate_tenant_id(tenant_id)?;
        if input.device_id.is_none() && input.device_type.is_none() {
            return Err(UnifiedError::bad_request(
                "an alert rule needs a device_id, a device_type, or both",
            ));
        }
        if let Some(device_id) = &input.device_id {
            // The device must exist in this tenant.
            self.load_device(tenant_id, device_id)?;
        }
        if let Some(device_type) = &input.device_type {
            validate_text(device_type, "device_type", 64)?;
        }
        if !is_valid_metric_name(&input.metric) {
            return Err(UnifiedError::bad_request(
                "metric is invalid: 1-64 characters, letters, digits, '-', '_' '.' or '/'",
            ));
        }
        if !input.threshold.is_finite() {
            return Err(UnifiedError::bad_request("threshold must be a finite number"));
        }
        if input.cooldown_secs > MAX_COOLDOWN_SECS {
            return Err(UnifiedError::bad_request(format!(
                "cooldown_secs must be 0-{MAX_COOLDOWN_SECS}"
            )));
        }
        let rule = AlertRule {
            id: new_id(),
            tenant_id: tenant_id.to_string(),
            device_id: input.device_id,
            device_type: input.device_type,
            metric: input.metric,
            operator: input.operator,
            threshold: input.threshold,
            cooldown_secs: input.cooldown_secs,
            severity: input.severity,
            created_at: now_utc(),
            last_fired_at: None,
        };
        self.alert_rules.insert(rule.id.clone(), rule.clone());
        Ok(rule)
    }

    pub fn list_alert_rules(
        &self,
        tenant_id: &str,
        device_id: Option<&str>,
        device_type: Option<&str>,
        page: u32,
        per_page: u32,
    ) -> Vec<AlertRule> {
        let mut items: Vec<AlertRule> = self
            .alert_rules
            .iter()
            .filter(|entry| entry.tenant_id == tenant_id)
            .filter(|entry| device_id.is_none_or(|id| entry.device_id.as_deref() == Some(id)))
            .filter(|entry| {
                device_type.is_none_or(|t| entry.device_type.as_deref() == Some(t))
            })
            .map(|entry| entry.clone())
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        paginate(items, page, per_page)
    }

    pub fn delete_alert_rule(&self, tenant_id: &str, id: &str) -> UnifiedResult<AlertRule> {
        match self.alert_rules.get(id) {
            Some(entry) if entry.tenant_id == tenant_id => {
                let rule = entry.clone();
                drop(entry);
                self.alert_rules.remove(id);
                Ok(rule)
            }
            _ => Err(UnifiedError::not_found("alert rule not found")),
        }
    }

    /// Evaluate every rule that matches this point. A rule fires only
    /// when the threshold condition holds and its cooldown has passed;
    /// firing stores an alert, stamps `last_fired_at`, and emits
    /// `iot.alert.triggered`. Rule ids are collected before any
    /// mutation so iteration never races with the cooldown update.
    fn evaluate_alert_rules(&self, device: &Device, point: &TelemetryPoint, now: DateTime<Utc>) {
        let candidates: Vec<(String, AlertOperator, f64, AlertSeverity, u64, Option<DateTime<Utc>>)> =
            self.alert_rules
                .iter()
                .filter(|entry| entry.tenant_id == device.tenant_id)
                .filter(|entry| entry.matches(device, &point.metric))
                .map(|entry| {
                    (
                        entry.id.clone(),
                        entry.operator,
                        entry.threshold,
                        entry.severity,
                        entry.cooldown_secs,
                        entry.last_fired_at,
                    )
                })
                .collect();
        for (rule_id, operator, threshold, severity, cooldown_secs, last_fired_at) in candidates {
            if !eval_operator(operator, point.value, threshold) {
                continue;
            }
            let cooldown_ok = match last_fired_at {
                None => true,
                Some(last) => {
                    now.signed_duration_since(last).num_seconds() >= cooldown_secs as i64
                }
            };
            if !cooldown_ok {
                continue;
            }
            if let Some(mut rule) = self.alert_rules.get_mut(&rule_id) {
                rule.last_fired_at = Some(now);
            }
            let alert = Alert {
                id: new_id(),
                tenant_id: device.tenant_id.clone(),
                device_id: device.id.clone(),
                rule_id: rule_id.clone(),
                metric: point.metric.clone(),
                value: point.value,
                threshold,
                operator,
                severity,
                message: format!(
                    "device {} metric {} value {} {} threshold {}",
                    device.device_key,
                    point.metric,
                    point.value,
                    operator.as_str(),
                    threshold
                ),
                status: "open".to_string(),
                created_at: now,
            };
            self.alerts.insert(alert.id.clone(), alert.clone());
            self.emit(
                events::ALERT_TRIGGERED,
                json!({
                    "alert_id": alert.id,
                    "rule_id": rule_id,
                    "tenant_id": alert.tenant_id,
                    "device_id": alert.device_id,
                    "metric": alert.metric,
                    "value": alert.value,
                    "threshold": alert.threshold,
                    "operator": alert.operator.as_str(),
                    "severity": alert.severity.as_str(),
                }),
            );
        }
    }

    pub fn list_alerts(
        &self,
        tenant_id: &str,
        device_id: Option<&str>,
        severity: Option<AlertSeverity>,
        page: u32,
        per_page: u32,
    ) -> Vec<Alert> {
        let mut items: Vec<Alert> = self
            .alerts
            .iter()
            .filter(|entry| entry.tenant_id == tenant_id)
            .filter(|entry| device_id.is_none_or(|id| entry.device_id == id))
            .filter(|entry| severity.is_none_or(|s| entry.severity == s))
            .map(|entry| entry.clone())
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        paginate(items, page, per_page)
    }

    // -- fleet health ----------------------------------------------------------

    /// Per-tenant fleet summary: device counts by status, devices
    /// silent longer than `silent_threshold_secs`, out-of-sync twin
    /// count, and open alert count.
    pub fn fleet_health(&self, tenant_id: &str, silent_threshold_secs: u64) -> Value {
        let now = now_utc();
        let mut by_status = HashMap::from([
            ("provisioned", 0u64),
            ("active", 0u64),
            ("suspended", 0u64),
            ("decommissioned", 0u64),
        ]);
        let mut silent: Vec<Value> = Vec::new();
        let mut device_ids: Vec<String> = Vec::new();
        for entry in self.devices.iter().filter(|e| e.tenant_id == tenant_id) {
            *by_status.entry(entry.status.as_str()).or_insert(0) += 1;
            device_ids.push(entry.id.clone());
            let silent_secs = match entry.last_seen_at {
                Some(last_seen) => now.signed_duration_since(last_seen).num_seconds(),
                None => now.signed_duration_since(entry.created_at).num_seconds(),
            };
            if entry.status != DeviceStatus::Decommissioned
                && silent_secs > silent_threshold_secs as i64
            {
                silent.push(json!({
                    "device_id": entry.id,
                    "device_key": entry.device_key,
                    "name": entry.name,
                    "status": entry.status.as_str(),
                    "last_seen_at": entry.last_seen_at,
                    "silent_secs": silent_secs,
                }));
            }
        }
        let out_of_sync = self
            .twins
            .iter()
            .filter(|entry| device_ids.contains(entry.key()))
            .filter(|entry| !entry.in_sync())
            .count() as u64;
        let open_alerts = self
            .alerts
            .iter()
            .filter(|entry| entry.tenant_id == tenant_id && entry.status == "open")
            .count() as u64;
        json!({
            "tenant_id": tenant_id,
            "devices_by_status": by_status,
            "total_devices": device_ids.len(),
            "silent_devices": silent,
            "silent_count": silent.len(),
            "silent_threshold_secs": silent_threshold_secs,
            "twins_out_of_sync": out_of_sync,
            "open_alerts": open_alerts,
        })
    }
}
