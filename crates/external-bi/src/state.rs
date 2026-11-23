//! BI external module (split from lib.rs; moved verbatim).

use super::*;


// ---------------------------------------------------------------------------
// Module state (in-memory, per-tenant)
// ---------------------------------------------------------------------------

/// All module state. Held behind one `Arc` shared by the module
/// struct, its hook handlers, and its HTTP handlers.
pub struct BiState {
    pub(crate) datasets: RwLock<HashMap<String, Dataset>>,
    pub(crate) extracts: RwLock<Vec<ExtractRun>>,
    pub(crate) snapshots: RwLock<HashMap<String, Snapshot>>,
    pub(crate) destinations: RwLock<HashMap<String, Destination>>,
    pub(crate) deliveries: RwLock<Vec<DeliveryAttempt>>,
    pub(crate) schedules: RwLock<HashMap<String, Schedule>>,
    /// Attached in `init` from `AppContext::hooks`; `None` before
    /// startup (and in unit tests), in which case hook emission is a
    /// silent no-op.
    pub(crate) hooks: RwLock<Option<Arc<HookBus>>>,
}

impl BiState {
    pub fn new() -> Self {
        Self {
            datasets: RwLock::new(HashMap::new()),
            extracts: RwLock::new(Vec::new()),
            snapshots: RwLock::new(HashMap::new()),
            destinations: RwLock::new(HashMap::new()),
            deliveries: RwLock::new(Vec::new()),
            schedules: RwLock::new(HashMap::new()),
            hooks: RwLock::new(None),
        }
    }

    pub(crate) fn emit(&self, event: &str, data: Value) -> Option<HookContext> {
        let hooks = self.hooks.read().ok()?;
        let bus = hooks.as_ref()?;
        Some(bus.emit(event, data))
    }

    pub(crate) fn dataset_for(&self, tenant: &str, id: &str) -> Result<Dataset, UnifiedError> {
        let datasets = self.datasets.read().map_err(|_| {
            UnifiedError::internal("bi state lock poisoned")
        })?;
        match datasets.get(id) {
            Some(dataset) if dataset.tenant_id == tenant => Ok(dataset.clone()),
            _ => Err(not_found(format!("dataset {id:?} not found"))),
        }
    }

    pub(crate) fn destination_for(&self, tenant: &str, id: &str) -> Result<Destination, UnifiedError> {
        let destinations = self.destinations.read().map_err(|_| {
            UnifiedError::internal("bi state lock poisoned")
        })?;
        match destinations.get(id) {
            Some(destination) if destination.tenant_id == tenant => Ok(destination.clone()),
            _ => Err(not_found(format!("destination {id:?} not found"))),
        }
    }

    pub(crate) fn schedule_for(&self, tenant: &str, id: &str) -> Result<Schedule, UnifiedError> {
        let schedules = self.schedules.read().map_err(|_| {
            UnifiedError::internal("bi state lock poisoned")
        })?;
        match schedules.get(id) {
            Some(schedule) if schedule.tenant_id == tenant => Ok(schedule.clone()),
            _ => Err(not_found(format!("schedule {id:?} not found"))),
        }
    }

    pub(crate) fn latest_snapshot(&self, tenant: &str, dataset_id: &str) -> Option<Snapshot> {
        let snapshots = self.snapshots.read().ok()?;
        snapshots
            .values()
            .filter(|snapshot| snapshot.tenant_id == tenant && snapshot.dataset_id == dataset_id)
            .max_by_key(|snapshot| snapshot.extracted_at.clone())
            .cloned()
    }

    pub(crate) fn snapshot_for(&self, tenant: &str, id: &str) -> Result<Snapshot, UnifiedError> {
        let snapshots = self.snapshots.read().map_err(|_| {
            UnifiedError::internal("bi state lock poisoned")
        })?;
        match snapshots.get(id) {
            Some(snapshot) if snapshot.tenant_id == tenant => Ok(snapshot.clone()),
            _ => Err(not_found(format!("snapshot {id:?} not found"))),
        }
    }

    pub(crate) fn record_delivery(&self, attempt: DeliveryAttempt) {
        if let Ok(mut deliveries) = self.deliveries.write() {
            deliveries.push(attempt);
        }
    }

    /// Run an extract now: collect rows from every registered
    /// `bi.dataset.provide` provider, validate each row against the
    /// declared fields, record the run, and store an immutable
    /// snapshot of the accepted rows when the run completes.
    pub fn run_extract(
        &self,
        tenant: &str,
        dataset_id: &str,
        trigger: &str,
    ) -> Result<ExtractRun, UnifiedError> {
        let dataset = self.dataset_for(tenant, dataset_id)?;
        let started = Utc::now();
        let started_at = started.to_rfc3339();
        let clock = Instant::now();

        // Collect rows from providers. A provider result is either
        // `{"rows": [...]}` or `{"error": "..."}`; a bare JSON array
        // is also accepted as a row list.
        let mut rows: Vec<Value> = Vec::new();
        let mut provider_error: Option<String> = None;
        if let Some(context) = self.emit(
            events::DATASET_PROVIDE,
            json!({
                "tenant_id": tenant,
                "dataset_id": &dataset.id,
                "source": &dataset.source,
                "fields": &dataset.fields,
            }),
        ) {
            for result in &context.results {
                if let Some(error) = result.get("error").and_then(Value::as_str) {
                    provider_error = Some(error.to_string());
                    continue;
                }
                if let Some(list) = result.get("rows").and_then(Value::as_array) {
                    rows.extend(list.iter().cloned());
                } else if let Some(list) = result.as_array() {
                    rows.extend(list.iter().cloned());
                }
            }
        }
        if rows.len() > MAX_EXTRACT_ROWS {
            provider_error = Some(format!(
                "providers returned {} rows, over the limit of {MAX_EXTRACT_ROWS}",
                rows.len()
            ));
        }

        let finish = |status: &str,
                      accepted: usize,
                      rejected: usize,
                      rejections: Vec<RowRejection>,
                      error: Option<String>|
         -> ExtractRun {
            ExtractRun {
                id: new_id(),
                tenant_id: tenant.to_string(),
                dataset_id: dataset.id.clone(),
                status: status.to_string(),
                trigger: trigger.to_string(),
                started_at: started_at.clone(),
                finished_at: Some(Utc::now().to_rfc3339()),
                duration_ms: Some(clock.elapsed().as_millis() as u64),
                rows_accepted: accepted,
                rows_rejected: rejected,
                rejections,
                error,
            }
        };

        if let Some(error) = provider_error {
            let run = finish("failed", 0, 0, Vec::new(), Some(error));
            self.emit(events::EXTRACT_FAILED, json!(&run));
            if let Ok(mut extracts) = self.extracts.write() {
                extracts.push(run.clone());
            }
            return Err(UnifiedError::internal(
                run.error.clone().unwrap_or_else(|| "extract failed".to_string()),
            ));
        }

        let mut accepted: Vec<Value> = Vec::new();
        let mut rejections: Vec<RowRejection> = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            match validate_row(&dataset.fields, row) {
                Ok(clean) => accepted.push(clean),
                Err(reason) => rejections.push(RowRejection {
                    row_index: index,
                    reason,
                }),
            }
        }
        let run = finish(
            "completed",
            accepted.len(),
            rejections.len(),
            rejections,
            None,
        );
        let snapshot = Snapshot {
            id: new_id(),
            tenant_id: tenant.to_string(),
            dataset_id: dataset.id.clone(),
            schema_version: dataset.schema_version,
            extracted_at: run.finished_at.clone().unwrap_or_else(now_rfc3339),
            row_count: accepted.len(),
            rows: accepted,
        };
        let snapshot_id = snapshot.id.clone();
        if let Ok(mut snapshots) = self.snapshots.write() {
            snapshots.insert(snapshot.id.clone(), snapshot);
        }
        if let Ok(mut extracts) = self.extracts.write() {
            extracts.push(run.clone());
        }
        self.emit(
            events::EXTRACT_COMPLETED,
            json!({ "run": &run, "snapshot_id": &snapshot_id }),
        );
        Ok(run)
    }

    /// Rows of this module's own extract-run history for one tenant,
    /// in the shape the seed provider serves.
    pub(crate) fn seed_history_rows(&self, tenant: &str) -> Vec<Value> {
        let Ok(extracts) = self.extracts.read() else {
            return Vec::new();
        };
        extracts
            .iter()
            .filter(|run| run.tenant_id == tenant)
            .map(|run| {
                json!({
                    "dataset_id": &run.dataset_id,
                    "status": &run.status,
                    "rows_accepted": run.rows_accepted,
                    "rows_rejected": run.rows_rejected,
                    "duration_ms": run.duration_ms.unwrap_or(0),
                    "started_at": &run.started_at,
                })
            })
            .collect()
    }

    /// Deliver one snapshot to a destination (webhook POST or file
    /// write) and record the attempt. Webhook delivery is async and
    /// SSRF-safe; file delivery is a synchronous CSV write below the
    /// fixed export base directory.
    pub(crate) async fn deliver(
        self: &Arc<Self>,
        tenant: &str,
        destination_id: &str,
        dataset_id: &str,
        snapshot_id: &str,
    ) -> Result<DeliveryAttempt, UnifiedError> {
        let destination = self.destination_for(tenant, destination_id)?;
        let dataset = self.dataset_for(tenant, dataset_id)?;
        let snapshot = self.snapshot_for(tenant, snapshot_id)?;
        let attempt = match destination.kind.as_str() {
            "webhook" => {
                self.deliver_webhook(&destination, &dataset, &snapshot)
                    .await
            }
            "file" => self.deliver_file(&destination, &dataset, &snapshot),
            other => {
                return Err(UnifiedError::internal(format!(
                    "unknown destination kind {other:?}"
                )));
            }
        };
        if attempt.status == "failed" {
            self.emit(events::DESTINATION_FAILED, json!(&attempt));
        }
        self.record_delivery(attempt.clone());
        Ok(attempt)
    }

    pub(crate) async fn deliver_webhook(
        &self,
        destination: &Destination,
        dataset: &Dataset,
        snapshot: &Snapshot,
    ) -> DeliveryAttempt {
        let started = Instant::now();
        let base = DeliveryAttempt {
            id: new_id(),
            tenant_id: destination.tenant_id.clone(),
            destination_id: destination.id.clone(),
            dataset_id: Some(dataset.id.clone()),
            snapshot_id: Some(snapshot.id.clone()),
            status: "failed".to_string(),
            status_code: None,
            latency_ms: None,
            error: None,
            attempted_at: now_rfc3339(),
        };
        let url = match &destination.url {
            Some(url) => url.clone(),
            None => {
                return DeliveryAttempt {
                    error: Some("webhook destination has no URL".to_string()),
                    ..base
                };
            }
        };
        // Re-validate at delivery time: a destination created when a
        // host was public may resolve to a private IP later.
        let parsed = match validate_webhook_url(&url).await {
            Ok(parsed) => parsed,
            Err(error) => {
                return DeliveryAttempt {
                    error: Some(error.to_string()),
                    ..base
                };
            }
        };
        let client = match reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(WEBHOOK_TIMEOUT_SECS))
            .build()
        {
            Ok(client) => client,
            Err(error) => {
                return DeliveryAttempt {
                    error: Some(error.to_string()),
                    ..base
                };
            }
        };
        // Body: one NDJSON summary line, then one line per row.
        let mut body = String::new();
        body.push_str(
            &serde_json::to_string(&json!({
                "type": "summary",
                "dataset_id": &dataset.id,
                "snapshot_id": &snapshot.id,
                "schema_version": snapshot.schema_version,
                "extracted_at": &snapshot.extracted_at,
                "row_count": snapshot.row_count,
            }))
            .unwrap_or_default(),
        );
        body.push('\n');
        body.push_str(&rows_to_jsonl(&snapshot.rows));
        match client
            .post(parsed)
            .header("content-type", "application/x-ndjson")
            .body(body)
            .send()
            .await
        {
            Ok(response) => {
                let status = response.status();
                let latency = started.elapsed().as_millis() as u64;
                if status.is_success() {
                    DeliveryAttempt {
                        status: "delivered".to_string(),
                        status_code: Some(status.as_u16()),
                        latency_ms: Some(latency),
                        ..base
                    }
                } else {
                    DeliveryAttempt {
                        status: "failed".to_string(),
                        status_code: Some(status.as_u16()),
                        latency_ms: Some(latency),
                        error: Some(format!("webhook returned HTTP {}", status.as_u16())),
                        ..base
                    }
                }
            }
            Err(error) => DeliveryAttempt {
                latency_ms: Some(started.elapsed().as_millis() as u64),
                error: Some(error.to_string()),
                ..base
            },
        }
    }

    pub(crate) fn deliver_file(
        &self,
        destination: &Destination,
        dataset: &Dataset,
        snapshot: &Snapshot,
    ) -> DeliveryAttempt {
        let started = Instant::now();
        let base = DeliveryAttempt {
            id: new_id(),
            tenant_id: destination.tenant_id.clone(),
            destination_id: destination.id.clone(),
            dataset_id: Some(dataset.id.clone()),
            snapshot_id: Some(snapshot.id.clone()),
            status: "failed".to_string(),
            status_code: None,
            latency_ms: None,
            error: None,
            attempted_at: now_rfc3339(),
        };
        let relative = match &destination.path {
            Some(path) => path.clone(),
            None => {
                return DeliveryAttempt {
                    error: Some("file destination has no path".to_string()),
                    ..base
                };
            }
        };
        let target = match resolve_export_path(&relative) {
            Ok(target) => target,
            Err(error) => {
                return DeliveryAttempt {
                    error: Some(error.to_string()),
                    ..base
                };
            }
        };
        let csv = rows_to_csv(&dataset.fields, &snapshot.rows);
        let result = (|| -> std::io::Result<()> {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, csv)
        })();
        match result {
            Ok(()) => DeliveryAttempt {
                status: "delivered".to_string(),
                latency_ms: Some(started.elapsed().as_millis() as u64),
                ..base
            },
            Err(error) => DeliveryAttempt {
                latency_ms: Some(started.elapsed().as_millis() as u64),
                error: Some(error.to_string()),
                ..base
            },
        }
    }
}
