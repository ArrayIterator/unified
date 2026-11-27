//! BI external module.

use super::*;


// HTTP handlers. Every JSON response uses the standard envelope
// (`{"data": ...}` on success, the standard error shape on failure).
// The export endpoint is the single exception: it streams raw CSV /
// JSONL file bytes, like a download, with a content-disposition.

type State = web::Data<Arc<BiState>>;

fn ok<T: Serialize>(data: T) -> HttpResponse {
    success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    success_response(data, 201)
}

fn failed(error: UnifiedError) -> HttpResponse {
    error_response(&error, false)
}

fn sort_by_created_desc<T, F>(mut items: Vec<T>, created_at: F) -> Vec<T>
where
    F: Fn(&T) -> &str,
{
    items.sort_by(|a, b| created_at(b).cmp(created_at(a)));
    items
}

// Datasets

pub(crate) async fn create_dataset(state: State, req: HttpRequest, body: web::Json<CreateDatasetRequest>) -> HttpResponse {
    let body = body.into_inner();
    let tenant = match resolve_tenant(&req, body.tenant_id.as_deref()) {
        Ok(tenant) => tenant,
        Err(error) => return failed(error),
    };
    if body.name.trim().is_empty() || body.name.len() > 128 {
        return failed(bad_request("name must be 1-128 chars"));
    }
    if body.source.trim().is_empty() || body.source.len() > 256 {
        return failed(bad_request("source must be a non-empty string of at most 256 chars"));
    }
    if let Some(description) = &body.description {
        if description.len() > 2000 {
            return failed(bad_request("description must be at most 2000 chars"));
        }
    }
    if let Some(owner) = &body.owner {
        if owner.len() > 128 {
            return failed(bad_request("owner must be at most 128 chars"));
        }
    }
    if let Err(error) = validate_field_defs(&body.fields) {
        return failed(error);
    }
    let dataset = Dataset {
        id: new_id(),
        tenant_id: tenant,
        name: body.name.trim().to_string(),
        source: body.source.trim().to_string(),
        fields: body.fields,
        description: body.description.unwrap_or_default(),
        owner: body.owner.unwrap_or_default(),
        schema_version: 1,
        created_at: now_rfc3339(),
        updated_at: now_rfc3339(),
    };
    match state.datasets.write() {
        Ok(mut datasets) => {
            datasets.insert(dataset.id.clone(), dataset.clone());
        }
        Err(_) => return failed(UnifiedError::internal("bi state lock poisoned")),
    }
    created(dataset)
}

pub(crate) async fn list_datasets(state: State, req: HttpRequest) -> HttpResponse {
    let tenant = match resolve_tenant(&req, None) {
        Ok(tenant) => tenant,
        Err(error) => return failed(error),
    };
    let datasets = match state.datasets.read() {
        Ok(datasets) => datasets
            .values()
            .filter(|dataset| dataset.tenant_id == tenant)
            .cloned()
            .collect::<Vec<_>>(),
        Err(_) => return failed(UnifiedError::internal("bi state lock poisoned")),
    };
    ok(sort_by_created_desc(datasets, |dataset| &dataset.created_at))
}

pub(crate) async fn get_dataset(state: State, req: HttpRequest, path: web::Path<DatasetPath>) -> HttpResponse {
    let tenant = match resolve_tenant(&req, None) {
        Ok(tenant) => tenant,
        Err(error) => return failed(error),
    };
    match state.dataset_for(&tenant, &path.id) {
        Ok(dataset) => ok(dataset),
        Err(error) => failed(error),
    }
}

pub(crate) async fn update_dataset(
    state: State,
    req: HttpRequest,
    path: web::Path<DatasetPath>,
    body: web::Json<UpdateDatasetRequest>,
) -> HttpResponse {
    let body = body.into_inner();
    let tenant = match resolve_tenant(&req, body.tenant_id.as_deref()) {
        Ok(tenant) => tenant,
        Err(error) => return failed(error),
    };
    let mut dataset = match state.dataset_for(&tenant, &path.id) {
        Ok(dataset) => dataset,
        Err(error) => return failed(error),
    };
    if let Some(name) = body.name {
        if name.trim().is_empty() || name.len() > 128 {
            return failed(bad_request("name must be 1-128 chars"));
        }
        dataset.name = name.trim().to_string();
    }
    if let Some(source) = body.source {
        if source.trim().is_empty() || source.len() > 256 {
            return failed(bad_request("source must be a non-empty string of at most 256 chars"));
        }
        dataset.source = source.trim().to_string();
    }
    if let Some(fields) = body.fields {
        if let Err(error) = validate_field_defs(&fields) {
            return failed(error);
        }
        if fields != dataset.fields {
            dataset.schema_version += 1;
        }
        dataset.fields = fields;
    }
    if let Some(description) = body.description {
        if description.len() > 2000 {
            return failed(bad_request("description must be at most 2000 chars"));
        }
        dataset.description = description;
    }
    if let Some(owner) = body.owner {
        if owner.len() > 128 {
            return failed(bad_request("owner must be at most 128 chars"));
        }
        dataset.owner = owner;
    }
    dataset.updated_at = now_rfc3339();
    match state.datasets.write() {
        Ok(mut datasets) => {
            datasets.insert(dataset.id.clone(), dataset.clone());
        }
        Err(_) => return failed(UnifiedError::internal("bi state lock poisoned")),
    }
    ok(dataset)
}

pub(crate) async fn delete_dataset(state: State, req: HttpRequest, path: web::Path<DatasetPath>) -> HttpResponse {
    let tenant = match resolve_tenant(&req, None) {
        Ok(tenant) => tenant,
        Err(error) => return failed(error),
    };
    let dataset = match state.dataset_for(&tenant, &path.id) {
        Ok(dataset) => dataset,
        Err(error) => return failed(error),
    };
    if let Ok(mut datasets) = state.datasets.write() {
        datasets.remove(&dataset.id);
    }
    // Cascade: runs, snapshots, and schedules of this dataset go
    // with it (same tenant only).
    if let Ok(mut extracts) = state.extracts.write() {
        extracts.retain(|run| !(run.tenant_id == tenant && run.dataset_id == dataset.id));
    }
    if let Ok(mut snapshots) = state.snapshots.write() {
        snapshots.retain(|_, snapshot| !(snapshot.tenant_id == tenant && snapshot.dataset_id == dataset.id));
    }
    if let Ok(mut schedules) = state.schedules.write() {
        schedules.retain(|_, schedule| !(schedule.tenant_id == tenant && schedule.dataset_id == dataset.id));
    }
    ok(json!({ "deleted": dataset.id }))
}

// Extracts and snapshots

pub(crate) async fn run_extract_handler(
    state: State,
    req: HttpRequest,
    path: web::Path<DatasetPath>,
    body: Option<web::Json<ExtractRequest>>,
) -> HttpResponse {
    let body = body.map(|body| body.into_inner()).unwrap_or(ExtractRequest {
        tenant_id: None,
        destination_id: None,
    });
    let tenant = match resolve_tenant(&req, body.tenant_id.as_deref()) {
        Ok(tenant) => tenant,
        Err(error) => return failed(error),
    };
    let state = state.into_inner();
    let run = match state.run_extract(&tenant, &path.id, "manual") {
        Ok(run) => run,
        Err(error) => return failed(error),
    };
    let snapshot_id = state
        .latest_snapshot(&tenant, &path.id)
        .map(|snapshot| snapshot.id);
    let mut payload = json!({ "run": &run, "snapshot_id": &snapshot_id });
    if let Some(destination_id) = body.destination_id {
        match &snapshot_id {
            Some(snapshot_id) => {
                match state
                    .deliver(&tenant, &destination_id, &path.id, snapshot_id)
                    .await
                {
                    Ok(attempt) => {
                        payload["delivery"] = json!(attempt);
                    }
                    Err(error) => return failed(error),
                }
            }
            None => return failed(UnifiedError::internal("extract produced no snapshot")),
        }
    }
    ok(payload)
}

pub(crate) async fn list_extracts(state: State, req: HttpRequest, path: web::Path<DatasetPath>) -> HttpResponse {
    let tenant = match resolve_tenant(&req, None) {
        Ok(tenant) => tenant,
        Err(error) => return failed(error),
    };
    if let Err(error) = state.dataset_for(&tenant, &path.id) {
        return failed(error);
    }
    let runs = match state.extracts.read() {
        Ok(extracts) => extracts
            .iter()
            .filter(|run| run.tenant_id == tenant && run.dataset_id == path.id)
            .cloned()
            .collect::<Vec<_>>(),
        Err(_) => return failed(UnifiedError::internal("bi state lock poisoned")),
    };
    ok(sort_by_created_desc(runs, |run| &run.started_at))
}

pub(crate) async fn list_snapshots(state: State, req: HttpRequest, path: web::Path<DatasetPath>) -> HttpResponse {
    let tenant = match resolve_tenant(&req, None) {
        Ok(tenant) => tenant,
        Err(error) => return failed(error),
    };
    if let Err(error) = state.dataset_for(&tenant, &path.id) {
        return failed(error);
    }
    let snapshots = match state.snapshots.read() {
        Ok(snapshots) => snapshots
            .values()
            .filter(|snapshot| snapshot.tenant_id == tenant && snapshot.dataset_id == path.id)
            .map(|snapshot| {
                // Listings carry metadata only; fetch rows through
                // the query or export endpoints.
                json!({
                    "id": &snapshot.id,
                    "tenant_id": &snapshot.tenant_id,
                    "dataset_id": &snapshot.dataset_id,
                    "schema_version": snapshot.schema_version,
                    "extracted_at": &snapshot.extracted_at,
                    "row_count": snapshot.row_count,
                })
            })
            .collect::<Vec<_>>(),
        Err(_) => return failed(UnifiedError::internal("bi state lock poisoned")),
    };
    let mut snapshots = snapshots;
    snapshots.sort_by(|a, b| b["extracted_at"].as_str().cmp(&a["extracted_at"].as_str()));
    ok(snapshots)
}

// Query and export

pub(crate) async fn query_dataset(
    state: State,
    req: HttpRequest,
    path: web::Path<DatasetPath>,
    body: web::Json<QueryRequest>,
) -> HttpResponse {
    let body = body.into_inner();
    let tenant = match resolve_tenant(&req, body.tenant_id.as_deref()) {
        Ok(tenant) => tenant,
        Err(error) => return failed(error),
    };
    let dataset = match state.dataset_for(&tenant, &path.id) {
        Ok(dataset) => dataset,
        Err(error) => return failed(error),
    };
    let snapshot = match &body.snapshot_id {
        Some(snapshot_id) => state.snapshot_for(&tenant, snapshot_id),
        None => match state.latest_snapshot(&tenant, &path.id) {
            Some(snapshot) => Ok(snapshot),
            None => Err(not_found("no snapshot exists for this dataset yet; run an extract first")),
        },
    };
    let snapshot = match snapshot {
        Ok(snapshot) => snapshot,
        Err(error) => return failed(error),
    };
    if snapshot.dataset_id != dataset.id {
        return failed(bad_request("snapshot_id belongs to a different dataset"));
    }
    match apply_query(&dataset, &snapshot, &body) {
        Ok(result) => ok(result),
        Err(error) => failed(error),
    }
}

pub(crate) async fn export_dataset(
    state: State,
    req: HttpRequest,
    path: web::Path<DatasetPath>,
    query: web::Query<ExportQuery>,
) -> HttpResponse {
    let tenant = match resolve_tenant(&req, None) {
        Ok(tenant) => tenant,
        Err(error) => return failed(error),
    };
    let format = query.format.as_deref().unwrap_or("csv");
    if format != "csv" && format != "jsonl" {
        return failed(bad_request("format must be \"csv\" or \"jsonl\""));
    }
    let dataset = match state.dataset_for(&tenant, &path.id) {
        Ok(dataset) => dataset,
        Err(error) => return failed(error),
    };
    let snapshot = match &query.snapshot_id {
        Some(snapshot_id) => state.snapshot_for(&tenant, snapshot_id),
        None => match state.latest_snapshot(&tenant, &path.id) {
            Some(snapshot) => Ok(snapshot),
            None => Err(not_found("no snapshot exists for this dataset yet; run an extract first")),
        },
    };
    let snapshot = match snapshot {
        Ok(snapshot) => snapshot,
        Err(error) => return failed(error),
    };
    if snapshot.dataset_id != dataset.id {
        return failed(bad_request("snapshot_id belongs to a different dataset"));
    }
    let filename = format!("bi-{}-{}.{}", dataset.id, &snapshot.id[..8.min(snapshot.id.len())], format);
    // Raw file bytes, not the JSON envelope: this endpoint is a
    // download. Every other JSON response uses the envelope.
    if format == "csv" {
        HttpResponse::Ok()
            .content_type("text/csv; charset=utf-8")
            .insert_header(("Content-Disposition", format!("attachment; filename=\"{filename}\"")))
            .body(rows_to_csv(&dataset.fields, &snapshot.rows))
    } else {
        HttpResponse::Ok()
            .content_type("application/x-ndjson; charset=utf-8")
            .insert_header(("Content-Disposition", format!("attachment; filename=\"{filename}\"")))
            .body(rows_to_jsonl(&snapshot.rows))
    }
}

// Destinations

pub(crate) async fn create_destination(
    state: State,
    req: HttpRequest,
    body: web::Json<CreateDestinationRequest>,
) -> HttpResponse {
    let body = body.into_inner();
    let tenant = match resolve_tenant(&req, body.tenant_id.as_deref()) {
        Ok(tenant) => tenant,
        Err(error) => return failed(error),
    };
    if body.name.trim().is_empty() || body.name.len() > 128 {
        return failed(bad_request("name must be 1-128 chars"));
    }
    match body.kind.as_str() {
        "webhook" => {
            let Some(url) = &body.url else {
                return failed(bad_request("webhook destinations require a url"));
            };
            if body.path.is_some() {
                return failed(bad_request("webhook destinations must not set a path"));
            }
            if let Err(error) = validate_webhook_url(url).await {
                return failed(error);
            }
        }
        "file" => {
            let Some(path) = &body.path else {
                return failed(bad_request("file destinations require a path"));
            };
            if body.url.is_some() {
                return failed(bad_request("file destinations must not set a url"));
            }
            if let Err(error) = resolve_export_path(path) {
                return failed(error);
            }
        }
        other => {
            return failed(bad_request(format!(
                "unknown destination kind {other:?}: use \"webhook\" or \"file\""
            )));
        }
    }
    let destination = Destination {
        id: new_id(),
        tenant_id: tenant,
        kind: body.kind,
        name: body.name.trim().to_string(),
        url: body.url,
        path: body.path,
        created_at: now_rfc3339(),
    };
    match state.destinations.write() {
        Ok(mut destinations) => {
            destinations.insert(destination.id.clone(), destination.clone());
        }
        Err(_) => return failed(UnifiedError::internal("bi state lock poisoned")),
    }
    created(destination)
}

pub(crate) async fn list_destinations(state: State, req: HttpRequest) -> HttpResponse {
    let tenant = match resolve_tenant(&req, None) {
        Ok(tenant) => tenant,
        Err(error) => return failed(error),
    };
    let destinations = match state.destinations.read() {
        Ok(destinations) => destinations
            .values()
            .filter(|destination| destination.tenant_id == tenant)
            .cloned()
            .collect::<Vec<_>>(),
        Err(_) => return failed(UnifiedError::internal("bi state lock poisoned")),
    };
    ok(sort_by_created_desc(destinations, |destination| &destination.created_at))
}

pub(crate) async fn get_destination(state: State, req: HttpRequest, path: web::Path<DestinationPath>) -> HttpResponse {
    let tenant = match resolve_tenant(&req, None) {
        Ok(tenant) => tenant,
        Err(error) => return failed(error),
    };
    match state.destination_for(&tenant, &path.id) {
        Ok(destination) => ok(destination),
        Err(error) => failed(error),
    }
}

pub(crate) async fn delete_destination(state: State, req: HttpRequest, path: web::Path<DestinationPath>) -> HttpResponse {
    let tenant = match resolve_tenant(&req, None) {
        Ok(tenant) => tenant,
        Err(error) => return failed(error),
    };
    let destination = match state.destination_for(&tenant, &path.id) {
        Ok(destination) => destination,
        Err(error) => return failed(error),
    };
    if let Ok(mut destinations) = state.destinations.write() {
        destinations.remove(&destination.id);
    }
    ok(json!({ "deleted": destination.id }))
}

/// Dry-run a destination with a 1-row sample. Webhook: POSTs a
/// summary line plus one row. File: writes a 1-row sample CSV next
/// to the configured path (`<name>-test.<ext>`), never overwriting
/// the real file.
pub(crate) async fn test_destination(state: State, req: HttpRequest, path: web::Path<DestinationPath>) -> HttpResponse {
    let tenant = match resolve_tenant(&req, None) {
        Ok(tenant) => tenant,
        Err(error) => return failed(error),
    };
    let state = state.into_inner();
    let destination = match state.destination_for(&tenant, &path.id) {
        Ok(destination) => destination,
        Err(error) => return failed(error),
    };
    let sample_dataset = Dataset {
        id: "bi-destination-test".to_string(),
        tenant_id: tenant.clone(),
        name: "BI destination test".to_string(),
        source: "bi.test".to_string(),
        fields: vec![
            FieldDef {
                name: "test".to_string(),
                field_type: FieldType::String,
            },
            FieldDef {
                name: "checked_at".to_string(),
                field_type: FieldType::Datetime,
            },
        ],
        description: String::new(),
        owner: String::new(),
        schema_version: 1,
        created_at: now_rfc3339(),
        updated_at: now_rfc3339(),
    };
    let sample_snapshot = Snapshot {
        id: new_id(),
        tenant_id: tenant.clone(),
        dataset_id: sample_dataset.id.clone(),
        schema_version: 1,
        extracted_at: now_rfc3339(),
        row_count: 1,
        rows: vec![json!({ "test": "bi-destination-test", "checked_at": now_rfc3339() })],
    };
    let attempt = match destination.kind.as_str() {
        "webhook" => {
            state
                .deliver_webhook(&destination, &sample_dataset, &sample_snapshot)
                .await
        }
        "file" => {
            let relative = destination.path.clone().unwrap_or_default();
            match resolve_export_path(&relative) {
                Ok(target) => {
                    // Sample goes to a sibling `-test` file.
                    let test_target = {
                        let stem = target
                            .file_stem()
                            .map(|stem| stem.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "export".to_string());
                        let extension = target
                            .extension()
                            .map(|ext| format!(".{}", ext.to_string_lossy()))
                            .unwrap_or_default();
                        target.with_file_name(format!("{stem}-test{extension}"))
                    };
                    let csv = rows_to_csv(&sample_dataset.fields, &sample_snapshot.rows);
                    let started = Instant::now();
                    let mut attempt = DeliveryAttempt {
                        id: new_id(),
                        tenant_id: tenant.clone(),
                        destination_id: destination.id.clone(),
                        dataset_id: None,
                        snapshot_id: None,
                        status: "failed".to_string(),
                        status_code: None,
                        latency_ms: None,
                        error: None,
                        attempted_at: now_rfc3339(),
                    };
                    let result = (|| -> std::io::Result<()> {
                        if let Some(parent) = test_target.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::write(&test_target, csv)
                    })();
                    attempt.latency_ms = Some(started.elapsed().as_millis() as u64);
                    match result {
                        Ok(()) => {
                            attempt.status = "delivered".to_string();
                        }
                        Err(error) => {
                            attempt.error = Some(error.to_string());
                        }
                    }
                    attempt
                }
                Err(error) => DeliveryAttempt {
                    id: new_id(),
                    tenant_id: tenant.clone(),
                    destination_id: destination.id.clone(),
                    dataset_id: None,
                    snapshot_id: None,
                    status: "failed".to_string(),
                    status_code: None,
                    latency_ms: None,
                    error: Some(error.to_string()),
                    attempted_at: now_rfc3339(),
                },
            }
        }
        _ => {
            return failed(UnifiedError::internal("unknown destination kind"));
        }
    };
    if attempt.status == "failed" {
        state.emit(events::DESTINATION_FAILED, json!(&attempt));
    }
    state.record_delivery(attempt.clone());
    ok(attempt)
}

pub(crate) async fn list_deliveries(state: State, req: HttpRequest, query: web::Query<DeliveryListQuery>) -> HttpResponse {
    let tenant = match resolve_tenant(&req, None) {
        Ok(tenant) => tenant,
        Err(error) => return failed(error),
    };
    let attempts = match state.deliveries.read() {
        Ok(deliveries) => deliveries
            .iter()
            .filter(|attempt| attempt.tenant_id == tenant)
            .filter(|attempt| {
                query
                    .destination_id
                    .as_deref()
                    .is_none_or(|id| attempt.destination_id == id)
            })
            .filter(|attempt| {
                query
                    .dataset_id
                    .as_deref()
                    .is_none_or(|id| attempt.dataset_id.as_deref() == Some(id))
            })
            .cloned()
            .collect::<Vec<_>>(),
        Err(_) => return failed(UnifiedError::internal("bi state lock poisoned")),
    };
    ok(sort_by_created_desc(attempts, |attempt| &attempt.attempted_at))
}

// Schedules

pub(crate) async fn create_schedule(state: State, req: HttpRequest, body: web::Json<CreateScheduleRequest>) -> HttpResponse {
    let body = body.into_inner();
    let tenant = match resolve_tenant(&req, body.tenant_id.as_deref()) {
        Ok(tenant) => tenant,
        Err(error) => return failed(error),
    };
    if let Err(reason) = validate_cron(&body.cron) {
        return failed(bad_request(format!("invalid cron expression: {reason}")));
    }
    if let Err(error) = state.dataset_for(&tenant, &body.dataset_id) {
        return failed(error);
    }
    if let Some(destination_id) = &body.destination_id {
        if let Err(error) = state.destination_for(&tenant, destination_id) {
            return failed(error);
        }
    }
    let schedule = Schedule {
        id: new_id(),
        tenant_id: tenant,
        dataset_id: body.dataset_id,
        destination_id: body.destination_id,
        cron: body.cron,
        enabled: body.enabled.unwrap_or(true),
        last_run_at: None,
        created_at: now_rfc3339(),
    };
    match state.schedules.write() {
        Ok(mut schedules) => {
            schedules.insert(schedule.id.clone(), schedule.clone());
        }
        Err(_) => return failed(UnifiedError::internal("bi state lock poisoned")),
    }
    created(schedule)
}

pub(crate) async fn list_schedules(state: State, req: HttpRequest) -> HttpResponse {
    let tenant = match resolve_tenant(&req, None) {
        Ok(tenant) => tenant,
        Err(error) => return failed(error),
    };
    let schedules = match state.schedules.read() {
        Ok(schedules) => schedules
            .values()
            .filter(|schedule| schedule.tenant_id == tenant)
            .cloned()
            .collect::<Vec<_>>(),
        Err(_) => return failed(UnifiedError::internal("bi state lock poisoned")),
    };
    ok(sort_by_created_desc(schedules, |schedule| &schedule.created_at))
}

pub(crate) async fn get_schedule(state: State, req: HttpRequest, path: web::Path<SchedulePath>) -> HttpResponse {
    let tenant = match resolve_tenant(&req, None) {
        Ok(tenant) => tenant,
        Err(error) => return failed(error),
    };
    match state.schedule_for(&tenant, &path.id) {
        Ok(schedule) => ok(schedule),
        Err(error) => failed(error),
    }
}

pub(crate) async fn delete_schedule(state: State, req: HttpRequest, path: web::Path<SchedulePath>) -> HttpResponse {
    let tenant = match resolve_tenant(&req, None) {
        Ok(tenant) => tenant,
        Err(error) => return failed(error),
    };
    let schedule = match state.schedule_for(&tenant, &path.id) {
        Ok(schedule) => schedule,
        Err(error) => return failed(error),
    };
    if let Ok(mut schedules) = state.schedules.write() {
        schedules.remove(&schedule.id);
    }
    ok(json!({ "deleted": schedule.id }))
}

pub(crate) async fn run_schedule_now(state: State, req: HttpRequest, path: web::Path<SchedulePath>) -> HttpResponse {
    let tenant = match resolve_tenant(&req, None) {
        Ok(tenant) => tenant,
        Err(error) => return failed(error),
    };
    let state = state.into_inner();
    let schedule = match state.schedule_for(&tenant, &path.id) {
        Ok(schedule) => schedule,
        Err(error) => return failed(error),
    };
    let run = match state.run_extract(&tenant, &schedule.dataset_id, "schedule") {
        Ok(run) => run,
        Err(error) => return failed(error),
    };
    if let Ok(mut schedules) = state.schedules.write() {
        if let Some(entry) = schedules.get_mut(&schedule.id) {
            entry.last_run_at = Some(now_rfc3339());
        }
    }
    let snapshot_id = state
        .latest_snapshot(&tenant, &schedule.dataset_id)
        .map(|snapshot| snapshot.id);
    let mut payload = json!({ "run": &run, "snapshot_id": &snapshot_id });
    if let (Some(destination_id), Some(snapshot_id)) = (&schedule.destination_id, &snapshot_id) {
        match state
            .deliver(&tenant, destination_id, &schedule.dataset_id, snapshot_id)
            .await
        {
            Ok(attempt) => {
                payload["delivery"] = json!(attempt);
            }
            Err(error) => return failed(error),
        }
    }
    ok(payload)
}
