// supervision module -- HTTP handlers for progress reports and the project dashboard.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::*;

/// Apply a fresh project-progress hook answer to a report. When
/// the hook answers, the stored percent is the hook value and
/// the source must say so, whatever the previous source was --
/// otherwise a report whose percent came from the hook would
/// still claim to be manual. When the hook does not answer, the
/// stored percent and source are left untouched.
pub(crate) fn apply_project_progress_snapshot(
    report: &mut ProgressReport,
    hook_percent: Option<f64>,
) {
    if let Some(percent) = hook_percent {
        report.project_percent_complete = Some(percent);
        report.source = ProgressSource::ProjectHook;
    }
}

pub(crate) async fn list_progress_reports(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        let params = query.into_inner();
        let project_filter = params.filters.get("project_id").cloned();
        let milestone_filter = params.filters.get("milestone_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<Value> = state
            .progress_reports
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|report| {
                if let Some(project_id) = &project_filter {
                    if &report.project_id != project_id {
                        return false;
                    }
                }
                if let Some(milestone_id) = &milestone_filter {
                    if report.milestone_id.as_deref() != Some(milestone_id.as_str()) {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if report.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                true
            })
            .map(|report| report_view(&report))
            .collect();
        items.sort_by(|a, b| {
            a.get("period_end")
                .and_then(Value::as_str)
                .cmp(&b.get("period_end").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn create_progress_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    body: web::Json<CreateProgressReportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let input = body.into_inner();
        let project_id = require_id(&input.project_id, "project_id")?;
        query_project(&ctx, &project_id)?;
        let milestone_id = match &input.milestone_id {
            Some(raw) => Some(require_id(raw, "milestone_id")?),
            None => None,
        };
        if input.period_end < input.period_start {
            return Err(UnifiedError::bad_request(
                "period_end must not be before period_start",
            ));
        }
        let reported = valid_percent(
            input.reported_percent_complete,
            "reported_percent_complete",
        )?;
        let planned = match input.planned_percent_complete {
            Some(value) => Some(valid_percent(value, "planned_percent_complete")?),
            None => None,
        };
        // Snapshot the project percent through the hook query.
        // When the projects module does not answer, fall back to
        // an explicit manual estimate when one was supplied, and
        // otherwise record the snapshot as unverified with no
        // project percent at all.
        let (project_percent, source) =
            match query_project_progress(&ctx, &project_id, milestone_id.as_deref()) {
                Some(percent) => (Some(percent), ProgressSource::ProjectHook),
                None => match input.manual_project_percent_complete {
                    Some(manual) => (
                        Some(valid_percent(manual, "manual_project_percent_complete")?),
                        ProgressSource::Manual,
                    ),
                    None => (None, ProgressSource::Unverified),
                },
            };
        let now = Utc::now();
        let report = ProgressReport {
            id: Uuid::now_v7(),
            project_id: project_id.clone(),
            milestone_id,
            title: require_text(&input.title, "title", MAX_TITLE_LEN)?,
            period_start: input.period_start,
            period_end: input.period_end,
            reported_percent_complete: reported,
            planned_percent_complete: planned,
            project_percent_complete: project_percent,
            source,
            status: ReportStatus::Draft,
            notes: opt_text(&input.notes, "notes", MAX_TEXT_LEN)?,
            reported_by: caller.actor.clone(),
            reviewed_by: None,
            reviewed_at: None,
            created_at: now,
            updated_at: now,
        };
        state
            .progress_reports
            .insert(report.id, report.clone());
        state.emit(
            "supervision.progress_report.created",
            json!({
                "report_id": report.id.to_string(),
                "project_id": report.project_id,
                "milestone_id": report.milestone_id,
                "reported_percent_complete": report.reported_percent_complete,
                "project_percent_complete": report.project_percent_complete,
                "source": report.source.as_str(),
            }),
        );
        Ok(report_view(&report))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn get_progress_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        let report = load_report(&state, &path.into_inner())?;
        Ok(report_view(&report))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn update_progress_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateProgressReportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_MANAGE)?;
        let mut report = load_report(&state, &path.into_inner())?;
        if !matches!(report.status, ReportStatus::Draft | ReportStatus::Returned) {
            return Err(UnifiedError::conflict(format!(
                "only a draft or returned report can be updated (status is \"{}\")",
                report.status.as_str()
            )));
        }
        let input = body.into_inner();
        if let Some(title) = input.title {
            report.title = require_text(&title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(period_start) = input.period_start {
            report.period_start = period_start;
        }
        if let Some(period_end) = input.period_end {
            report.period_end = period_end;
        }
        if report.period_end < report.period_start {
            return Err(UnifiedError::bad_request(
                "period_end must not be before period_start",
            ));
        }
        if let Some(reported) = input.reported_percent_complete {
            report.reported_percent_complete =
                valid_percent(reported, "reported_percent_complete")?;
        }
        if let Some(planned) = input.planned_percent_complete {
            report.planned_percent_complete = match planned {
                Some(value) => Some(valid_percent(value, "planned_percent_complete")?),
                None => None,
            };
        }
        if let Some(notes) = input.notes {
            report.notes = match notes {
                Some(raw) => Some(require_text(&raw, "notes", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        // Re-snapshotting after an edit refreshes the project
        // percent from the hook query; a returned report goes back
        // to draft so it must be submitted again.
        let progress =
            query_project_progress(&ctx, &report.project_id, report.milestone_id.as_deref());
        apply_project_progress_snapshot(&mut report, progress);
        report.status = ReportStatus::Draft;
        report.updated_at = Utc::now();
        state.progress_reports.insert(report.id, report.clone());
        state.emit(
            "supervision.progress_report.updated",
            json!({ "report_id": report.id.to_string() }),
        );
        Ok(report_view(&report))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn submit_progress_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let mut report = load_report(&state, &path.into_inner())?;
        if report.status != ReportStatus::Draft {
            return Err(UnifiedError::conflict(
                "only a draft report can be submitted",
            ));
        }
        report.status = ReportStatus::Submitted;
        report.updated_at = Utc::now();
        state.progress_reports.insert(report.id, report.clone());
        state.emit(
            "supervision.progress_report.submitted",
            json!({
                "report_id": report.id.to_string(),
                "project_id": report.project_id,
            }),
        );
        Ok(report_view(&report))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn decide_progress_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecisionRequest>,
    decision: ApprovalDecision,
) -> UnifiedResult<Value> {
    let _tenant = require_tenant(&req)?;
    let _scope = caller_company(&_tenant, &req)?;
    let caller = caller(&req)?;
    require_permission(&ctx, &caller.role, PERM_SUPERVISION_APPROVE)?;
    let mut report = load_report(&state, &path.into_inner())?;
    if report.status != ReportStatus::Submitted {
        return Err(UnifiedError::conflict(
            "only a submitted report can be decided",
        ));
    }
    let input = body.into_inner();
    let comment = opt_text(&input.comment, "comment", MAX_COMMENT_LEN)?;
    if decision == ApprovalDecision::Returned && comment.is_none() {
        return Err(UnifiedError::bad_request(
            "a comment is required when returning a report",
        ));
    }
    report.status = match decision {
        ApprovalDecision::Approved => ReportStatus::Approved,
        ApprovalDecision::Returned => ReportStatus::Returned,
    };
    report.reviewed_by = Some(caller.actor.clone());
    report.reviewed_at = Some(Utc::now());
    report.updated_at = Utc::now();
    state.progress_reports.insert(report.id, report.clone());
    let approval = record_approval(
        &state,
        ApprovalTargetKind::ProgressReport,
        report.id,
        &report.project_id,
        &caller.actor,
        decision,
        comment,
    );
    state.emit(
        "supervision.progress_report.decided",
        json!({
            "report_id": report.id.to_string(),
            "project_id": report.project_id,
            "decision": decision.as_str(),
            "approval_id": approval.id.to_string(),
        }),
    );
    let mut view = report_view(&report);
    if let Value::Object(map) = &mut view {
        map.insert(
            "approval".to_string(),
            serde_json::to_value(approval).unwrap_or(Value::Null),
        );
    }
    Ok(view)
}


pub(crate) async fn approve_progress_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecisionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    match decide_progress_report(ctx, state, req, path, body, ApprovalDecision::Approved).await {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn return_progress_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecisionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    match decide_progress_report(ctx, state, req, path, body, ApprovalDecision::Returned).await {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Defect handlers


/// Per-project supervision summary, computed on read.
///
/// Number provenance (exactly which numbers come from where):
///   - `project` (id, name, status, archived): the projects
///     module, via the `project.get_requested` hook query, read
///     at request time. Not stored here.
///   - `open_defects`, `defects_by_severity`,
///     `defects_by_status`: counted from this module's
///     `supervision_defects` rows for the project.
///   - `overdue_inspections`, `inspections_by_status`,
///     `inspections_due_next_7_days`: counted from this module's
///     `supervision_inspections` rows; an inspection is overdue
///     when `scheduled_for` is in the past and its status is
///     still `scheduled` or `in_progress`.
///   - `latest_report`, `progress_variance_points`: from this
///     module's latest approved or submitted
///     `supervision_progress_reports` row for the project
///     (variance = reported minus planned, computed on read);
///     `project_percent_complete` inside it is the snapshot the
///     hook query returned when the report was created, not a
///     live project value.
///   - `active_assignments`, `unsigned_daily_logs`: counted from
///     this module's `supervision_assignments` /
///     `supervision_daily_logs` rows.
pub(crate) async fn project_dashboard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        let params = query.into_inner();
        let project_id = params
            .filters
            .get("project_id")
            .cloned()
            .ok_or_else(|| {
                UnifiedError::bad_request("project_id query parameter is required")
            })?;
        let project = query_project(&ctx, &project_id)?;
        let now = Utc::now();
        let defects: Vec<Defect> = state
            .defects
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|defect| defect.project_id == project_id)
            .collect();
        let open_defects = defects
            .iter()
            .filter(|defect| {
                matches!(
                    defect.status,
                    DefectStatus::Open | DefectStatus::InProgress
                )
            })
            .count() as u64;
        let mut defects_by_severity = serde_json::Map::new();
        for severity in ["low", "medium", "high", "critical"] {
            defects_by_severity.insert(
                severity.to_string(),
                Value::from(
                    defects
                        .iter()
                        .filter(|defect| defect.severity.as_str() == severity)
                        .count() as u64,
                ),
            );
        }
        let mut defects_by_status = serde_json::Map::new();
        for status in ["open", "in_progress", "resolved", "verified"] {
            defects_by_status.insert(
                status.to_string(),
                Value::from(
                    defects
                        .iter()
                        .filter(|defect| defect.status.as_str() == status)
                        .count() as u64,
                ),
            );
        }
        let inspections: Vec<Inspection> = state
            .inspections
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|inspection| inspection.project_id == project_id)
            .collect();
        let overdue_inspections = inspections
            .iter()
            .filter(|inspection| {
                inspection.scheduled_for < now
                    && matches!(
                        inspection.status,
                        InspectionStatus::Scheduled | InspectionStatus::InProgress
                    )
            })
            .count() as u64;
        let due_next_7_days = inspections
            .iter()
            .filter(|inspection| {
                inspection.scheduled_for >= now
                    && inspection.scheduled_for
                        < now + chrono::Duration::days(7)
                    && matches!(
                        inspection.status,
                        InspectionStatus::Scheduled | InspectionStatus::InProgress
                    )
            })
            .count() as u64;
        let mut inspections_by_status = serde_json::Map::new();
        for status in [
            "scheduled",
            "in_progress",
            "submitted",
            "approved",
            "returned",
            "cancelled",
        ] {
            inspections_by_status.insert(
                status.to_string(),
                Value::from(
                    inspections
                        .iter()
                        .filter(|inspection| inspection.status.as_str() == status)
                        .count() as u64,
                ),
            );
        }
        let mut reports: Vec<ProgressReport> = state
            .progress_reports
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|report| {
                report.project_id == project_id
                    && matches!(
                        report.status,
                        ReportStatus::Submitted | ReportStatus::Approved
                    )
            })
            .collect();
        reports.sort_by_key(|report| report.period_end);
        let latest_report = reports.last().map(|report| report_view(report));
        let progress_variance_points = reports.last().and_then(|report| {
            report
                .planned_percent_complete
                .map(|planned| {
                    ((report.reported_percent_complete - planned) * 100.0).round() / 100.0
                })
        });
        let active_assignments = state
            .assignments
            .iter()
            .filter(|entry| {
                entry.value().project_id == project_id
                    && entry.value().status == AssignmentStatus::Active
            })
            .count() as u64;
        let unsigned_daily_logs = state
            .daily_logs
            .iter()
            .filter(|entry| {
                entry.value().project_id == project_id && entry.value().signed_at.is_none()
            })
            .count() as u64;
        Ok(json!({
            "project": {
                "id": project.id,
                "name": project.name,
                "status": project.status,
                "archived": project.archived,
                "source": "project.get_requested",
            },
            "open_defects": open_defects,
            "defects_by_severity": defects_by_severity,
            "defects_by_status": defects_by_status,
            "overdue_inspections": overdue_inspections,
            "inspections_due_next_7_days": due_next_7_days,
            "inspections_by_status": inspections_by_status,
            "latest_report": latest_report,
            "progress_variance_points": progress_variance_points,
            "active_assignments": active_assignments,
            "unsigned_daily_logs": unsigned_daily_logs,
            "sources": {
                "project": "project.get_requested (live hook query)",
                "defects": "supervision_defects (own table)",
                "inspections": "supervision_inspections (own table)",
                "progress": "supervision_progress_reports latest submitted/approved snapshot (project_percent_complete is the snapshot-time hook value, not live)",
                "assignments": "supervision_assignments (own table)",
                "daily_logs": "supervision_daily_logs (own table)",
            },
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Hook listeners (projects-module events)

// Acknowledge `project.created` / `project.updated`: this module
// keeps no project cache, so there is nothing to update.
// (Plain comment, not a doc comment: no listener lives in this file,
// so a `///` doc comment here would be orphaned.)

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn sample_report(source: ProgressSource, project_percent: Option<f64>) -> ProgressReport {
        let now = Utc::now();
        ProgressReport {
            id: Uuid::now_v7(),
            project_id: "project-1".to_string(),
            milestone_id: None,
            title: "Weekly progress".to_string(),
            period_start: NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date"),
            period_end: NaiveDate::from_ymd_opt(2026, 1, 7).expect("valid date"),
            reported_percent_complete: 40.0,
            planned_percent_complete: Some(35.0),
            project_percent_complete: project_percent,
            source,
            status: ReportStatus::Draft,
            notes: None,
            reported_by: "user-1".to_string(),
            reviewed_by: None,
            reviewed_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn hook_answer_upgrades_manual_source_to_project_hook() {
        // The S3 defect: a manual report re-snapshotted from the
        // hook kept its manual source while storing the hook value.
        let mut report = sample_report(ProgressSource::Manual, Some(30.0));
        apply_project_progress_snapshot(&mut report, Some(45.0));
        assert_eq!(report.project_percent_complete, Some(45.0));
        assert_eq!(report.source, ProgressSource::ProjectHook);
    }

    #[test]
    fn hook_answer_upgrades_unverified_source_to_project_hook() {
        let mut report = sample_report(ProgressSource::Unverified, None);
        apply_project_progress_snapshot(&mut report, Some(20.0));
        assert_eq!(report.project_percent_complete, Some(20.0));
        assert_eq!(report.source, ProgressSource::ProjectHook);
    }

    #[test]
    fn hook_answer_refreshes_percent_for_hook_sourced_report() {
        let mut report = sample_report(ProgressSource::ProjectHook, Some(10.0));
        apply_project_progress_snapshot(&mut report, Some(12.5));
        assert_eq!(report.project_percent_complete, Some(12.5));
        assert_eq!(report.source, ProgressSource::ProjectHook);
    }

    #[test]
    fn unanswered_hook_leaves_percent_and_source_untouched() {
        for source in [
            ProgressSource::Manual,
            ProgressSource::Unverified,
            ProgressSource::ProjectHook,
        ] {
            let mut report = sample_report(source, Some(30.0));
            apply_project_progress_snapshot(&mut report, None);
            assert_eq!(report.project_percent_complete, Some(30.0));
            assert_eq!(report.source, source);
        }
    }

    #[test]
    fn report_status_error_text_uses_snake_case() {
        // S5: the update-report conflict message must read
        // "draft", not the Debug form "Draft".
        assert_eq!(ReportStatus::Draft.as_str(), "draft");
        assert_eq!(ReportStatus::Submitted.as_str(), "submitted");
        assert_ne!(format!("{:?}", ReportStatus::Draft), "draft");
    }
}
