// Projects module - project validation and lookup helpers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use serde::Serialize;
use serde_json::{Value, json};

// Private helpers (all prefixed `s2_`)

/// Normalize a column name (or supplied key) into a status key:
/// trimmed, lowercased, spaces and dashes become underscores.
pub(crate) fn s2_normalize_status_key(input: &str) -> String {
    input
        .trim()
        .to_lowercase()
        .replace(' ', "_")
        .replace('-', "_")
}

pub(crate) fn s2_valid_project_status(status: &str) -> bool {
    matches!(status, "active" | "on_hold" | "completed" | "cancelled")
}

pub(crate) fn s2_valid_visibility(visibility: &str) -> bool {
    matches!(visibility, "private" | "internal" | "public")
}

pub(crate) fn s2_valid_member_role(role: &str) -> bool {
    matches!(role, "owner" | "manager" | "member" | "viewer")
}

/// A task counts as done when its status is `done` or `completed`.
pub(crate) fn s2_task_is_done(status: &str) -> bool {
    matches!(status, "done" | "completed")
}

/// Clean an optional free-text field: trimmed, empty becomes `None`.
pub(crate) fn s2_clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// Fetch a board and confirm it belongs to the given project.
pub(crate) fn s2_get_board(
    state: &ProjectsState,
    project_id: &str,
    board_id: &str,
) -> UnifiedResult<ProjectBoard> {
    let board = state
        .boards
        .get(board_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Project board not found"))?;
    if board.project_id != project_id {
        return Err(UnifiedError::not_found("Project board not found"));
    }
    Ok(board)
}

/// Fetch a column and confirm it belongs to the given board.
pub(crate) fn s2_get_column(
    state: &ProjectsState,
    board_id: &str,
    column_id: &str,
) -> UnifiedResult<ProjectBoardColumn> {
    let column = state
        .columns
        .get(column_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Board column not found"))?;
    if column.board_id != board_id {
        return Err(UnifiedError::not_found("Board column not found"));
    }
    Ok(column)
}

/// Fetch a milestone and confirm it belongs to the given project.
pub(crate) fn s2_get_milestone(
    state: &ProjectsState,
    project_id: &str,
    milestone_id: &str,
) -> UnifiedResult<ProjectMilestone> {
    let milestone = state
        .milestones
        .get(milestone_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Project milestone not found"))?;
    if milestone.project_id != project_id {
        return Err(UnifiedError::not_found("Project milestone not found"));
    }
    Ok(milestone)
}

/// Build a board column from a template JSON value. Returns `None`
/// when the value carries no usable name.
pub(crate) fn s2_column_from_value(
    board_id: &str,
    value: &Value,
    position: u32,
) -> Option<ProjectBoardColumn> {
    let object = value.as_object()?;
    let name = object
        .get("name")
        .and_then(|entry| entry.as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())?;
    let status_key = match object.get("status_key").and_then(|entry| entry.as_str()) {
        Some(key) if !key.trim().is_empty() => s2_normalize_status_key(key),
        _ => s2_normalize_status_key(name),
    };
    let wip_limit = object
        .get("wip_limit")
        .and_then(|entry| entry.as_u64())
        .map(|limit| limit as u32)
        .filter(|limit| *limit > 0);
    Some(ProjectBoardColumn {
        id: new_id(),
        board_id: board_id.to_string(),
        name: name.to_string(),
        position,
        status_key,
        wip_limit,
    })
}

/// Aggregated numbers for one project, returned by
/// `get_project_overview`.
#[derive(Debug, Clone, Serialize)]
pub struct S2ProjectOverview {
    pub project_id: String,
    pub tasks_total: u64,
    pub tasks_open: u64,
    pub tasks_done: u64,
    pub tasks_overdue: u64,
    pub members: u64,
    pub milestones_total: u64,
    pub milestones_reached: u64,
    pub sprints_active: u64,
    pub total_logged_hours: f64,
    pub billable_unbilled_hours: f64,
    pub progress_percent: f64,
}

/// Resolve the caller's company for a WRITE (C3, strict by
/// default): a write requires a resolved company scope, so an
/// unresolved caller is refused with 403 instead of silently
/// writing a global (unscoped) record. Mutation handlers call
/// this instead of `caller_company`; read handlers keep
/// `caller_company`, and the named carve-outs (self-service
/// writes, settings with no company field, scan/test triggers,
/// platform surfaces) keep it too.
pub(crate) fn require_company_for_write(
    tenant: &unified_core::tenancy::TenantContext,
    req: &actix_web::HttpRequest,
) -> UnifiedResult<Option<String>> {
    match super::state::caller_company(tenant, req)? {
        Some(company) => Ok(Some(company)),
        None => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalize_status_key() {
        assert_eq!(s2_normalize_status_key("In Progress"), "in_progress");
        assert_eq!(s2_normalize_status_key("To-Do"), "to_do");
        assert_eq!(s2_normalize_status_key("  Done "), "done");
        assert_eq!(s2_normalize_status_key("READY"), "ready");
    }

    #[test]
    fn project_status_visibility_and_role_rules() {
        assert!(s2_valid_project_status("active"));
        assert!(s2_valid_project_status("on_hold"));
        assert!(!s2_valid_project_status("archived"));
        assert!(s2_valid_visibility("public"));
        assert!(!s2_valid_visibility("secret"));
        assert!(s2_valid_member_role("owner"));
        assert!(!s2_valid_member_role("superadmin"));
    }

    #[test]
    fn task_is_done_and_clean_optional() {
        assert!(s2_task_is_done("done"));
        assert!(s2_task_is_done("completed"));
        assert!(!s2_task_is_done("in_progress"));
        assert_eq!(s2_clean_optional(None), None);
        assert_eq!(s2_clean_optional(Some("   ".to_string())), None);
        assert_eq!(
            s2_clean_optional(Some(" x ".to_string())),
            Some("x".to_string())
        );
    }

    #[test]
    fn column_from_value_builds_columns() {
        let column = s2_column_from_value(
            "board-1",
            &json!({"name": "In Progress", "wip_limit": 3}),
            2,
        )
        .unwrap();
        assert_eq!(column.board_id, "board-1");
        assert_eq!(column.name, "In Progress");
        assert_eq!(column.status_key, "in_progress");
        assert_eq!(column.position, 2);
        assert_eq!(column.wip_limit, Some(3));
    }

    #[test]
    fn column_from_value_rejects_nameless_and_zero_wip() {
        assert!(s2_column_from_value("board-1", &json!({"name": "  "}), 0).is_none());
        assert!(s2_column_from_value("board-1", &json!({}), 0).is_none());
        let column =
            s2_column_from_value("board-1", &json!({"name": "Backlog", "wip_limit": 0}), 0)
                .unwrap();
        assert_eq!(column.wip_limit, None);
        assert_eq!(column.status_key, "backlog");
    }
}
