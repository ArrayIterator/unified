//! IoT external module (split from lib.rs; moved verbatim).

use super::*;


pub(crate) fn paginate<T>(items: Vec<T>, page: u32, per_page: u32) -> Vec<T> {
    let page = page.max(1) as usize;
    let per_page = per_page.clamp(1, 100) as usize;
    let start = (page - 1) * per_page;
    items.into_iter().skip(start).take(per_page).collect()
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

fn respond_ok(result: UnifiedResult<Value>) -> HttpResponse {
    json::respond(result, false)
}

fn respond_created(result: UnifiedResult<Value>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

/// Resolve the tenant for an admin request: the `X-Unified-Tenant`
/// header wins, then an explicit `tenant_id` query or body value.
/// There is no default and no fallback to another tenant.
fn tenant_from_request(req: &HttpRequest, fallback: Option<&str>) -> UnifiedResult<String> {
    let from_header = req
        .headers()
        .get("x-unified-tenant")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let tenant = from_header
        .or_else(|| fallback.map(str::to_string))
        .ok_or_else(|| {
            UnifiedError::bad_request(
                "tenant_id is required (X-Unified-Tenant header or tenant_id parameter)",
            )
        })?;
    validate_tenant_id(&tenant)?;
    Ok(tenant)
}

/// Extract the device API key from `X-Device-Key`, or from an
/// `Authorization: Bearer <key>` header. Nothing else is accepted,
/// and the key is never written to any log.
fn device_key_from_request(req: &HttpRequest) -> Option<String> {
    if let Some(value) = req.headers().get("x-device-key") {
        if let Ok(text) = value.to_str() {
            let text = text.trim();
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
    }
    if let Some(value) = req.headers().get("authorization") {
        if let Ok(text) = value.to_str() {
            if let Some(token) = text.strip_prefix("Bearer ") {
                let token = token.trim();
                if !token.is_empty() {
                    return Some(token.to_string());
                }
            }
        }
    }
    None
}

/// Load a device and authenticate the request's device key against
/// it. The tenant is the device's own tenant.
fn authenticate_request(
    state: &IotState,
    req: &HttpRequest,
    device_id: &str,
) -> UnifiedResult<Device> {
    let device = state.load_device_any_tenant(device_id)?;
    let presented = device_key_from_request(req)
        .ok_or_else(|| UnifiedError::unauthorized("device key is required (X-Device-Key header)"))?;
    state.authenticate_device(&device, &presented)?;
    Ok(device)
}

fn parse_device_status(value: &str) -> UnifiedResult<DeviceStatus> {
    match value {
        "provisioned" => Ok(DeviceStatus::Provisioned),
        "active" => Ok(DeviceStatus::Active),
        "suspended" => Ok(DeviceStatus::Suspended),
        "decommissioned" => Ok(DeviceStatus::Decommissioned),
        _ => Err(UnifiedError::bad_request(format!(
            "unknown device status '{value}'"
        ))),
    }
}

fn parse_severity(value: &str) -> UnifiedResult<AlertSeverity> {
    match value {
        "info" => Ok(AlertSeverity::Info),
        "warning" => Ok(AlertSeverity::Warning),
        "critical" => Ok(AlertSeverity::Critical),
        _ => Err(UnifiedError::bad_request(format!(
            "unknown severity '{value}'"
        ))),
    }
}

/// Parse a `from`/`to` query value: epoch seconds, or RFC 3339.
fn parse_time_bound(value: &str) -> UnifiedResult<DateTime<Utc>> {
    if let Ok(secs) = value.parse::<i64>() {
        return DateTime::from_timestamp(secs, 0)
            .ok_or_else(|| UnifiedError::bad_request(format!("invalid timestamp '{value}'")));
    }
    value
        .parse::<DateTime<Utc>>()
        .map_err(|_| UnifiedError::bad_request(format!("invalid timestamp '{value}'")))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_device_types(state: web::Data<Arc<IotState>>) -> HttpResponse {
    respond_ok(Ok(json!({ "device_types": state.device_types() })))
}

pub(crate) async fn create_device_type(
    state: web::Data<Arc<IotState>>,
    req: HttpRequest,
    body: web::Json<CreateDeviceTypeInput>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let input = body.into_inner();
        let _tenant = tenant_from_request(&req, input.tenant_id.as_deref())?;
        validate_text(&input.device_type, "device_type", 64)?;
        if !input
            .device_type
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return Err(UnifiedError::bad_request(
                "device_type is invalid: letters, digits, '-', '_' or '.' only",
            ));
        }
        let mut types = state
            .device_types
            .write()
            .map_err(|_| UnifiedError::internal("device type lock poisoned"))?;
        if !types.iter().any(|t| t == &input.device_type) {
            types.push(input.device_type.clone());
        }
        Ok(json!({ "device_types": types.clone() }))
    })();
    respond_created(result)
}

pub(crate) async fn create_device(
    state: web::Data<Arc<IotState>>,
    req: HttpRequest,
    body: web::Json<CreateDeviceInput>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let input = body.into_inner();
        let tenant_id = tenant_from_request(&req, input.tenant_id.as_deref())?;
        let (device, plain_key) = state.create_device(&tenant_id, input)?;
        Ok(json!({
            "device": device,
            // Shown exactly once. The plaintext key is never stored
            // and this endpoint is the only place it ever appears.
            "device_key_plain": plain_key,
            "device_key_note": "Store this key now; it is shown once and never returned again.",
        }))
    })();
    respond_created(result)
}

pub(crate) async fn list_devices(
    state: web::Data<Arc<IotState>>,
    req: HttpRequest,
    query: web::Query<ListQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = tenant_from_request(&req, query.tenant_id.as_deref())?;
        let status = match &query.status {
            Some(value) => Some(parse_device_status(value)?),
            None => None,
        };
        let items = state.list_devices(
            &tenant_id,
            status,
            query.device_type.as_deref(),
            query.page.unwrap_or(1),
            query.per_page.unwrap_or(20),
        );
        Ok(json!({ "devices": items, "tenant_id": tenant_id }))
    })();
    respond_ok(result)
}

pub(crate) async fn get_device(
    state: web::Data<Arc<IotState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ListQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = tenant_from_request(&req, query.tenant_id.as_deref())?;
        let device = state.load_device(&tenant_id, &path.into_inner())?;
        Ok(json!(device))
    })();
    respond_ok(result)
}

pub(crate) async fn update_device(
    state: web::Data<Arc<IotState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ListQuery>,
    body: web::Json<UpdateDeviceInput>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = tenant_from_request(&req, query.tenant_id.as_deref())?;
        let device = state.update_device(&tenant_id, &path.into_inner(), body.into_inner())?;
        Ok(json!(device))
    })();
    respond_ok(result)
}

pub(crate) async fn delete_device(
    state: web::Data<Arc<IotState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ListQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = tenant_from_request(&req, query.tenant_id.as_deref())?;
        let device = state.delete_device(&tenant_id, &path.into_inner())?;
        Ok(json!({ "deleted": true, "device_id": device.id }))
    })();
    respond_ok(result)
}

pub(crate) async fn ingest_telemetry(
    state: web::Data<Arc<IotState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TelemetryIngestInput>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let device = authenticate_request(&state, &req, &path.into_inner())?;
        let point = state.ingest_telemetry(&device, body.into_inner(), now_utc())?;
        Ok(json!(point))
    })();
    respond_created(result)
}

pub(crate) async fn get_telemetry(
    state: web::Data<Arc<IotState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<TelemetryQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = tenant_from_request(&req, query.tenant_id.as_deref())?;
        let device = state.load_device(&tenant_id, &path.into_inner())?;
        let from = match &query.from {
            Some(value) => Some(parse_time_bound(value)?),
            None => None,
        };
        let to = match &query.to {
            Some(value) => Some(parse_time_bound(value)?),
            None => None,
        };
        let limit = query.limit.unwrap_or(100).clamp(1, 500) as usize;
        let (points, aggregates) =
            state.query_telemetry(&device.id, query.metric.as_deref(), from, to, limit);
        Ok(json!({
            "device_id": device.id,
            "metric": query.metric,
            "points": points,
            "aggregates": aggregates,
        }))
    })();
    respond_ok(result)
}

pub(crate) async fn get_twin(
    state: web::Data<Arc<IotState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ListQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = tenant_from_request(&req, query.tenant_id.as_deref())?;
        let device = state.load_device(&tenant_id, &path.into_inner())?;
        Ok(state.get_twin(&device.id).view())
    })();
    respond_ok(result)
}

pub(crate) async fn update_twin(
    state: web::Data<Arc<IotState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ListQuery>,
    body: web::Json<TwinUpdateInput>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = tenant_from_request(&req, query.tenant_id.as_deref())?;
        let device = state.load_device(&tenant_id, &path.into_inner())?;
        let twin = state.update_twin(&device, body.into_inner(), now_utc(), false)?;
        Ok(twin.view())
    })();
    respond_ok(result)
}

pub(crate) async fn report_twin(
    state: web::Data<Arc<IotState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TwinUpdateInput>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let device = authenticate_request(&state, &req, &path.into_inner())?;
        let input = body.into_inner();
        // The device reports its own state: a reported payload is
        // required here, a desired payload is not accepted from the
        // device (desired state is set by the operator/admin side).
        let reported = input.reported.ok_or_else(|| {
            UnifiedError::bad_request("reported state is required")
        })?;
        let twin = state.update_twin(
            &device,
            TwinUpdateInput {
                desired: None,
                reported: Some(reported),
            },
            now_utc(),
            true,
        )?;
        Ok(twin.view())
    })();
    respond_ok(result)
}

pub(crate) async fn queue_command(
    state: web::Data<Arc<IotState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ListQuery>,
    body: web::Json<CreateCommandInput>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = tenant_from_request(&req, query.tenant_id.as_deref())?;
        let device = state.load_device(&tenant_id, &path.into_inner())?;
        let command = state.queue_command(&device, body.into_inner(), now_utc())?;
        Ok(json!(command))
    })();
    respond_created(result)
}

pub(crate) async fn list_commands(
    state: web::Data<Arc<IotState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<CommandsQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let device = authenticate_request(&state, &req, &path.into_inner())?;
        let queued_only = query.status.as_deref() != Some("all");
        let commands = state.list_commands(&device, queued_only, now_utc());
        Ok(json!({ "device_id": device.id, "commands": commands }))
    })();
    respond_ok(result)
}

pub(crate) async fn ack_command(
    state: web::Data<Arc<IotState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<AckCommandInput>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let (device_id, command_id) = path.into_inner();
        let device = authenticate_request(&state, &req, &device_id)?;
        let command = state.ack_command(&device, &command_id, body.into_inner(), now_utc())?;
        Ok(json!(command))
    })();
    respond_ok(result)
}

pub(crate) async fn create_alert_rule(
    state: web::Data<Arc<IotState>>,
    req: HttpRequest,
    body: web::Json<CreateAlertRuleInput>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let input = body.into_inner();
        let tenant_id = tenant_from_request(&req, input.tenant_id.as_deref())?;
        let rule = state.create_alert_rule(&tenant_id, input)?;
        Ok(json!(rule))
    })();
    respond_created(result)
}

pub(crate) async fn list_alert_rules(
    state: web::Data<Arc<IotState>>,
    req: HttpRequest,
    query: web::Query<ListQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = tenant_from_request(&req, query.tenant_id.as_deref())?;
        let rules = state.list_alert_rules(
            &tenant_id,
            query.device_id.as_deref(),
            query.device_type.as_deref(),
            query.page.unwrap_or(1),
            query.per_page.unwrap_or(20),
        );
        Ok(json!({ "alert_rules": rules, "tenant_id": tenant_id }))
    })();
    respond_ok(result)
}

pub(crate) async fn delete_alert_rule(
    state: web::Data<Arc<IotState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ListQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = tenant_from_request(&req, query.tenant_id.as_deref())?;
        let rule = state.delete_alert_rule(&tenant_id, &path.into_inner())?;
        Ok(json!({ "deleted": true, "rule_id": rule.id }))
    })();
    respond_ok(result)
}

pub(crate) async fn list_alerts(
    state: web::Data<Arc<IotState>>,
    req: HttpRequest,
    query: web::Query<ListQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = tenant_from_request(&req, query.tenant_id.as_deref())?;
        let severity = match &query.severity {
            Some(value) => Some(parse_severity(value)?),
            None => None,
        };
        let alerts = state.list_alerts(
            &tenant_id,
            query.device_id.as_deref(),
            severity,
            query.page.unwrap_or(1),
            query.per_page.unwrap_or(20),
        );
        Ok(json!({ "alerts": alerts, "tenant_id": tenant_id }))
    })();
    respond_ok(result)
}

pub(crate) async fn fleet_health(
    state: web::Data<Arc<IotState>>,
    req: HttpRequest,
    query: web::Query<HealthQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = tenant_from_request(&req, query.tenant_id.as_deref())?;
        let threshold = query
            .silent_threshold_secs
            .unwrap_or(DEFAULT_SILENT_THRESHOLD_SECS);
        if !(60..=86_400).contains(&threshold) {
            return Err(UnifiedError::bad_request(
                "silent_threshold_secs must be 60-86400",
            ));
        }
        Ok(state.fleet_health(&tenant_id, threshold))
    })();
    respond_ok(result)
}
