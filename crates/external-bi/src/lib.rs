// BI / data-warehouse connector module for the Unified server.
//
// What this module is: a connector that lets other modules (or the
// operator) declare *dataset shapes*, extract rows for those shapes
// from hook providers, keep every extract as an immutable snapshot,
// run small honest queries over a snapshot, and ship snapshots to
// webhook or file destinations on a schedule.
//
// What this module is NOT:
// - It is not a SQL engine. Datasets declare a shape only; they never
//   execute SQL. The query endpoint filters / groups / aggregates
//   in-memory rows from one snapshot.
// - It is not a live data warehouse and it has no built-in cron
//   daemon. Schedules are evaluated only when a scheduler emits the
//   `bi.schedule.tick` hook (or when `run-now` is called).
// - It is not a store for other modules' data. All records are
//   per-tenant and in-memory; `tenant_id` is required on writes and
//   enforced as a filter on every read, never a global view.
//
// Inter-module communication goes through the hook bus only:
// providers register for `bi.dataset.provide` and return rows in the
// dataset's declared shape. A seed provider for this module's own
// extract-run history (`source = "bi.extract_runs"`) is built in, so
// an extract works end to end with zero external modules.

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json::{error_response, success_response};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use uuid::Uuid;

/// Event names owned by this module.
pub mod events {
    /// Providers return rows for a dataset. Payload: `tenant_id`,
    /// `dataset_id`, `source`, `fields`. Each provider contributes a
    /// result of `{"rows": [...]}` or `{"error": "..."}`.
    pub const DATASET_PROVIDE: &str = "bi.dataset.provide";
    /// Emitted after a successful extract. Payload: the extract run.
    pub const EXTRACT_COMPLETED: &str = "bi.extract.completed";
    /// Emitted after a failed extract. Payload: the extract run.
    pub const EXTRACT_FAILED: &str = "bi.extract.failed";
    /// Emitted when a destination delivery fails. Payload: the
    /// delivery attempt.
    pub const DESTINATION_FAILED: &str = "bi.destination.failed";
    /// A scheduler emits this (about once a minute) to evaluate due
    /// schedules. This module owns no timer of its own.
    pub const SCHEDULE_TICK: &str = "bi.schedule.tick";
}

/// Source name of the built-in seed provider: rows are this module's
/// own extract-run history for the requesting tenant. A dataset that
/// wants the seed must declare exactly these fields:
///
/// `dataset_id` (string), `status` (string), `rows_accepted`
/// (number), `rows_rejected` (number), `duration_ms` (number),
/// `started_at` (datetime).
pub const SEED_SOURCE: &str = "bi.extract_runs";

/// Default base directory for `file` destinations, relative to the
/// server working directory. Overridable with the
/// `BI_EXPORT_BASE_DIR` environment variable. File destinations may
/// only write below this directory.
pub const DEFAULT_EXPORT_BASE_DIR: &str = "var/bi-exports";

/// Maximum rows a query returns. Requests above this are rejected.
pub const MAX_QUERY_LIMIT: usize = 1000;
/// Default query limit when none is given.
pub const DEFAULT_QUERY_LIMIT: usize = 100;
/// Maximum rows a single extract may accept, as a safety bound.
pub const MAX_EXTRACT_ROWS: usize = 100_000;
/// Timeout for webhook deliveries.
pub const WEBHOOK_TIMEOUT_SECS: u64 = 10;

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

fn new_id() -> String {
    Uuid::now_v7().to_string()
}

fn bad_request(message: impl Into<String>) -> UnifiedError {
    UnifiedError::bad_request(message)
}

fn not_found(message: impl Into<String>) -> UnifiedError {
    UnifiedError::not_found(message)
}

// Split from the parent file: the implementation now lives
// in the submodules below; the public paths are unchanged
// (this file re-exports everything that used to live here).
pub mod types;
pub mod validate;
pub mod export;
pub mod query;
pub mod state;
pub mod handlers;
pub mod module;
pub use export::{csv_escape, rows_to_csv, rows_to_jsonl, is_public_ip, validate_webhook_url, export_base_dir, resolve_export_path};
pub use module::{BiModule, register, module};
pub use query::{apply_query};
pub use state::{BiState};
pub use types::{FieldType, FieldDef, Dataset, RowRejection, ExtractRun, Snapshot, Destination, DeliveryAttempt, Schedule};
pub use validate::{is_valid_field_name, validate_row, validate_cron, cron_matches};
pub(crate) use handlers::{create_dataset, create_destination, create_schedule, delete_dataset, delete_destination, delete_schedule, export_dataset, get_dataset, get_destination, get_schedule, list_datasets, list_deliveries, list_destinations, list_extracts, list_schedules, list_snapshots, query_dataset, run_extract_handler, run_schedule_now, test_destination, update_dataset};
#[allow(unused_imports)] // Re-exported for the unit tests in this file.
pub(crate) use types::{AggSpec, CreateDatasetRequest, CreateDestinationRequest, CreateScheduleRequest, DatasetPath, DeliveryListQuery, DestinationPath, ExportQuery, ExtractRequest, QueryFilter, QueryRequest, SchedulePath, SortSpec, UpdateDatasetRequest};
#[allow(unused_imports)] // Re-exported for the unit tests in this file.
pub(crate) use validate::{resolve_tenant, validate_field_defs, validate_tenant_id};

// Unit tests

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn test_dataset() -> Dataset {
        Dataset {
            id: "ds-1".to_string(),
            tenant_id: "tenant-a".to_string(),
            name: "Orders".to_string(),
            source: "orders".to_string(),
            fields: vec![
                FieldDef { name: "region".to_string(), field_type: FieldType::String },
                FieldDef { name: "amount".to_string(), field_type: FieldType::Number },
                FieldDef { name: "paid".to_string(), field_type: FieldType::Boolean },
                FieldDef { name: "placed_at".to_string(), field_type: FieldType::Datetime },
            ],
            description: String::new(),
            owner: String::new(),
            schema_version: 1,
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
        }
    }

    fn test_snapshot(dataset: &Dataset) -> Snapshot {
        Snapshot {
            id: "snap-1".to_string(),
            tenant_id: dataset.tenant_id.clone(),
            dataset_id: dataset.id.clone(),
            schema_version: 1,
            extracted_at: now_rfc3339(),
            row_count: 3,
            rows: vec![
                json!({"region": "west", "amount": 10, "paid": true, "placed_at": "2026-11-22T08:00:00Z"}),
                json!({"region": "west", "amount": 30, "paid": false, "placed_at": "2026-11-22T09:00:00Z"}),
                json!({"region": "east", "amount": 20, "paid": true, "placed_at": "2026-11-22T10:00:00Z"}),
            ],
        }
    }

    fn query() -> QueryRequest {
        QueryRequest {
            tenant_id: None,
            snapshot_id: None,
            filters: None,
            group_by: None,
            aggregations: None,
            sort: None,
            limit: None,
        }
    }

    #[test]
    fn row_validation_accepts_a_good_row_and_drops_extra_keys() {
        let dataset = test_dataset();
        let row = json!({"region": "west", "amount": 10, "paid": true, "placed_at": "2026-11-22T08:00:00Z", "extra": "dropped"});
        let clean = validate_row(&dataset.fields, &row).expect("good row");
        assert_eq!(clean.get("region").unwrap(), "west");
        assert!(clean.get("extra").is_none());
    }

    #[test]
    fn row_validation_rejects_wrong_types_missing_fields_and_bad_datetimes() {
        let dataset = test_dataset();
        assert!(validate_row(&dataset.fields, &json!({"region": "west", "amount": "10", "paid": true, "placed_at": "2026-11-22T08:00:00Z"})).is_err());
        assert!(validate_row(&dataset.fields, &json!({"region": "west", "amount": 10, "paid": true})).is_err());
        assert!(validate_row(&dataset.fields, &json!({"region": "west", "amount": 10, "paid": true, "placed_at": "not-a-date"})).is_err());
        assert!(validate_row(&dataset.fields, &json!({"region": "west", "amount": 10, "paid": null, "placed_at": "2026-11-22T08:00:00Z"})).is_err());
        assert!(validate_row(&dataset.fields, &json!(["not", "an", "object"])).is_err());
    }

    #[test]
    fn query_group_aggregates_are_numeric_only_and_correct() {
        let dataset = test_dataset();
        let snapshot = test_snapshot(&dataset);
        let mut request = query();
        request.group_by = Some("region".to_string());
        request.aggregations = Some(vec![
            AggSpec { agg: "count".to_string(), field: None },
            AggSpec { agg: "sum".to_string(), field: Some("amount".to_string()) },
            AggSpec { agg: "avg".to_string(), field: Some("amount".to_string()) },
            AggSpec { agg: "min".to_string(), field: Some("amount".to_string()) },
            AggSpec { agg: "max".to_string(), field: Some("amount".to_string()) },
        ]);
        request.sort = Some(SortSpec { field: "region".to_string(), direction: Some("asc".to_string()) });
        let result = apply_query(&dataset, &snapshot, &request).expect("query");
        assert_eq!(result["row_count"], 2);
        let rows = result["results"].as_array().expect("results");
        assert_eq!(rows[0]["region"], "east");
        assert_eq!(rows[0]["count"], 1);
        assert_eq!(rows[0]["sum_amount"], 20.0);
        assert_eq!(rows[1]["region"], "west");
        assert_eq!(rows[1]["count"], 2);
        assert_eq!(rows[1]["sum_amount"], 40.0);
        assert_eq!(rows[1]["avg_amount"], 20.0);
        assert_eq!(rows[1]["min_amount"], 10.0);
        assert_eq!(rows[1]["max_amount"], 30.0);
    }

    #[test]
    fn query_rejects_string_field_for_sum_and_unknown_fields() {
        let dataset = test_dataset();
        let snapshot = test_snapshot(&dataset);
        let mut request = query();
        request.aggregations = Some(vec![AggSpec {
            agg: "sum".to_string(),
            field: Some("region".to_string()),
        }]);
        assert!(apply_query(&dataset, &snapshot, &request).is_err());
        let mut request = query();
        request.filters = Some(vec![QueryFilter {
            field: "nope".to_string(),
            op: "=".to_string(),
            value: json!("x"),
        }]);
        assert!(apply_query(&dataset, &snapshot, &request).is_err());
    }

    #[test]
    fn query_filters_and_contains_work() {
        let dataset = test_dataset();
        let snapshot = test_snapshot(&dataset);
        let mut request = query();
        request.filters = Some(vec![
            QueryFilter { field: "region".to_string(), op: "contains".to_string(), value: json!("es") },
            QueryFilter { field: "amount".to_string(), op: ">=".to_string(), value: json!(20) },
        ]);
        let result = apply_query(&dataset, &snapshot, &request).expect("query");
        assert_eq!(result["rows_matched"], 1);
        assert_eq!(result["results"][0]["region"], "west");
        let mut request = query();
        request.limit = Some(1001);
        assert!(apply_query(&dataset, &snapshot, &request).is_err());
    }

    #[test]
    fn cron_validation_and_matching() {
        assert!(validate_cron("0 9 * * *").is_ok());
        assert!(validate_cron("*/15 8-18 * * 1-5").is_ok());
        assert!(validate_cron("0 0 29 2 *").is_ok());
        assert!(validate_cron("0 9 * *").is_err());
        assert!(validate_cron("60 9 * * *").is_err());
        assert!(validate_cron("0 24 * * *").is_err());
        assert!(validate_cron("0 9 0 * *").is_err());
        assert!(validate_cron("0 9 * 13 *").is_err());
        assert!(validate_cron("0 9 * * 8").is_err());
        assert!(validate_cron("x 9 * * *").is_err());
        let nine = Utc.with_ymd_and_hms(2026, 11, 22, 9, 0, 0).unwrap();
        assert!(cron_matches("0 9 * * *", nine));
        assert!(!cron_matches("0 9 * * *", Utc.with_ymd_and_hms(2026, 11, 22, 9, 1, 0).unwrap()));
        assert!(cron_matches("*/15 * * * *", Utc.with_ymd_and_hms(2026, 11, 22, 9, 30, 0).unwrap()));
        assert!(!cron_matches("*/15 * * * *", Utc.with_ymd_and_hms(2026, 11, 22, 9, 31, 0).unwrap()));
    }

    #[test]
    fn csv_quoting_handles_commas_quotes_and_newlines() {
        assert_eq!(csv_escape("plain"), "plain");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_escape("line1\nline2"), "\"line1\nline2\"");
        let dataset = test_dataset();
        let csv = rows_to_csv(
            &dataset.fields,
            &[json!({"region": "a,b", "amount": 5, "paid": true, "placed_at": "2026-11-22T08:00:00Z"})],
        );
        assert!(csv.starts_with("region,amount,paid,placed_at\n"));
        assert!(csv.contains("\"a,b\",5,true,2026-11-22T08:00:00Z\n"));
    }

    #[test]
    fn public_ip_check_rejects_private_and_loopback() {
        assert!(!is_public_ip("127.0.0.1".parse().unwrap()));
        assert!(!is_public_ip("10.1.2.3".parse().unwrap()));
        assert!(!is_public_ip("172.16.0.1".parse().unwrap()));
        assert!(!is_public_ip("192.168.1.1".parse().unwrap()));
        assert!(!is_public_ip("169.254.1.1".parse().unwrap()));
        assert!(!is_public_ip("::1".parse().unwrap()));
        assert!(!is_public_ip("fc00::1".parse().unwrap()));
        assert!(is_public_ip("8.8.8.8".parse().unwrap()));
        assert!(is_public_ip("1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn export_path_rejects_traversal_and_absolute_paths() {
        assert!(resolve_export_path("../secret.csv").is_err());
        assert!(resolve_export_path("/etc/passwd").is_err());
        assert!(resolve_export_path("a/../../b.csv").is_err());
        assert!(resolve_export_path("").is_err());
        assert!(resolve_export_path("exports/orders.csv").is_ok());
    }

    #[test]
    fn tenant_validation_rejects_bad_ids() {
        assert!(validate_tenant_id("tenant-1_x").is_ok());
        assert!(validate_tenant_id("").is_err());
        assert!(validate_tenant_id("has space").is_err());
        assert!(validate_tenant_id("has/slash").is_err());
    }

    #[test]
    fn module_metadata_matches_the_contract() {
        let module = BiModule::new();
        assert_eq!(module.id(), "bi");
        assert_eq!(module.version(), "0.1.0");
        assert!(!module.is_core());
        assert!(module.requires().is_empty());
        assert_eq!(module.route_info().len(), 21);
        assert!(module.route_info().iter().all(|route| route.path.starts_with("/api/v1/bi/")));
    }
}
