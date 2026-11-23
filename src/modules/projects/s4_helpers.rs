// Projects module — sprint, phase, cycle, and program validation helpers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::error::{UnifiedError, UnifiedResult};
use chrono::{DateTime, Utc};

// ---------------------------------------------------------------------------
// Section-private helpers (s4_ prefix; nothing here is defined in s1)
// ---------------------------------------------------------------------------

/// Trimmed, non-empty text, or `None` when the input is absent or blank.
pub(crate) fn s4_clean_text(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// Require a non-empty value; returns the trimmed text.
pub(crate) fn s4_require_text(value: &str, label: &str) -> UnifiedResult<String> {
    let text = value.trim().to_string();
    if text.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "{label} must not be empty"
        )));
    }
    Ok(text)
}

pub(crate) fn s4_valid_sprint_status(status: &str) -> bool {
    matches!(status, "planned" | "active" | "completed")
}

pub(crate) fn s4_valid_phase_kind(kind: &str) -> bool {
    matches!(kind, "waterfall" | "prince2_stage" | "dmaic" | "hybrid")
}

pub(crate) fn s4_valid_phase_status(status: &str) -> bool {
    matches!(status, "planned" | "active" | "completed")
}

pub(crate) fn s4_valid_gate_status(status: &str) -> bool {
    matches!(status, "pending" | "approved" | "rejected")
}

pub(crate) fn s4_valid_cycle_status(status: &str) -> bool {
    matches!(
        status,
        "shaping" | "betting" | "building" | "cooldown" | "done"
    )
}

pub(crate) fn s4_valid_pi_status(status: &str) -> bool {
    matches!(status, "planned" | "active" | "completed")
}

pub(crate) fn s4_valid_business_case_status(status: &str) -> bool {
    matches!(status, "draft" | "approved" | "rejected")
}

/// Capability check, project lookup, and access check in one step.
/// Returns the resolved project.
pub(crate) fn s4_resolve_project(
    state: &ProjectsState,
    project_id: &str,
    viewer_id: Option<&str>,
    viewer_role: Option<&str>,
    capability: &str,
    scope: &Option<String>,
) -> UnifiedResult<Project> {
    require_capability(state, viewer_role, capability)?;
    let project = state.get_project(project_id)?;
    require_project_access(state, &project, viewer_id, viewer_role, scope)?;
    Ok(project)
}

/// Reject writes to an archived project with 409. Reads stay
/// allowed, so only mutating handlers call this.
pub(crate) fn s4_require_not_archived(project: &Project) -> UnifiedResult<()> {
    if project.archived {
        return Err(UnifiedError::conflict(
            "Project is archived and cannot be modified",
        ));
    }
    Ok(())
}

/// Like `s4_resolve_project`, but for mutating handlers: an archived
/// project is rejected with 409 after the usual capability and
/// access checks.
pub(crate) fn s4_resolve_project_for_write(
    state: &ProjectsState,
    project_id: &str,
    viewer_id: Option<&str>,
    viewer_role: Option<&str>,
    capability: &str,
    scope: &Option<String>,
) -> UnifiedResult<Project> {
    let project =
        s4_resolve_project(state, project_id, viewer_id, viewer_role, capability, scope)?;
    s4_require_not_archived(&project)?;
    Ok(project)
}

/// Reject a direct status write in an update handler. Sprint and
/// phase statuses (and a phase gate) may only change through their
/// dedicated handlers, which enforce the guards and events those
/// transitions require.
pub(crate) fn s4_reject_direct_status_change(
    field: Option<&str>,
    label: &str,
) -> UnifiedResult<()> {
    if field.is_some() {
        return Err(UnifiedError::bad_request(format!(
            "{label} can only change through its dedicated handler"
        )));
    }
    Ok(())
}

/// Actor recorded in the activity log: the viewer, or `system` when
/// no viewer was supplied.
pub(crate) fn s4_actor(viewer_id: &Option<String>) -> &str {
    viewer_id.as_deref().unwrap_or("system")
}

pub(crate) fn s4_get_sprint(
    state: &ProjectsState,
    project_id: &str,
    sprint_id: &str,
) -> UnifiedResult<ProjectSprint> {
    let sprint = state
        .sprints
        .get(sprint_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Sprint not found"))?;
    if sprint.project_id != project_id {
        return Err(UnifiedError::not_found("Sprint not found"));
    }
    Ok(sprint)
}

pub(crate) fn s4_get_phase(
    state: &ProjectsState,
    project_id: &str,
    phase_id: &str,
) -> UnifiedResult<ProjectPhase> {
    let phase = state
        .phases
        .get(phase_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Phase not found"))?;
    if phase.project_id != project_id {
        return Err(UnifiedError::not_found("Phase not found"));
    }
    Ok(phase)
}

pub(crate) fn s4_get_cycle(
    state: &ProjectsState,
    project_id: &str,
    cycle_id: &str,
) -> UnifiedResult<ProjectCycle> {
    let cycle = state
        .cycles
        .get(cycle_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Cycle not found"))?;
    if cycle.project_id != project_id {
        return Err(UnifiedError::not_found("Cycle not found"));
    }
    Ok(cycle)
}

pub(crate) fn s4_get_program_increment(
    state: &ProjectsState,
    project_id: &str,
    pi_id: &str,
) -> UnifiedResult<ProjectProgramIncrement> {
    let pi = state
        .program_increments
        .get(pi_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Program increment not found"))?;
    if pi.project_id != project_id {
        return Err(UnifiedError::not_found("Program increment not found"));
    }
    Ok(pi)
}

pub(crate) fn s4_get_business_case(
    state: &ProjectsState,
    project_id: &str,
    case_id: &str,
) -> UnifiedResult<ProjectBusinessCase> {
    let case = state
        .business_cases
        .get(case_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Business case not found"))?;
    if case.project_id != project_id {
        return Err(UnifiedError::not_found("Business case not found"));
    }
    Ok(case)
}

/// Id of the active sprint in a project, ignoring `exclude_id` when
/// supplied (used when the sprint being changed is itself active).
pub(crate) fn s4_active_sprint_id(
    state: &ProjectsState,
    project_id: &str,
    exclude_id: Option<&str>,
) -> Option<String> {
    state
        .sprints
        .iter()
        .find(|entry| {
            let sprint = entry.value();
            sprint.project_id == project_id
                && sprint.status == "active"
                && exclude_id.is_none_or(|id| sprint.id != id)
        })
        .map(|entry| entry.value().id.clone())
}

/// Highest phase position in a project, or 0 when it has no phases.
pub(crate) fn s4_max_phase_position(state: &ProjectsState, project_id: &str) -> u32 {
    state
        .phases
        .iter()
        .filter(|entry| entry.value().project_id == project_id)
        .map(|entry| entry.value().position)
        .max()
        .unwrap_or(0)
}

/// Id of the next phase after `position` in the same project (lowest
/// position greater than the current one), if any.
pub(crate) fn s4_next_phase_id(
    state: &ProjectsState,
    project_id: &str,
    position: u32,
) -> Option<String> {
    state
        .phases
        .iter()
        .filter(|entry| {
            let phase = entry.value();
            phase.project_id == project_id && phase.position > position
        })
        .min_by_key(|entry| entry.value().position)
        .map(|entry| entry.value().id.clone())
}

pub(crate) fn s4_validate_date_range(
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    label: &str,
) -> UnifiedResult<()> {
    if let (Some(start), Some(end)) = (start, end) {
        if end <= start {
            return Err(UnifiedError::bad_request(format!(
                "{label} end date must be after the start date"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_and_require_text() {
        assert_eq!(s4_clean_text(None), None);
        assert_eq!(s4_clean_text(Some("   ".to_string())), None);
        assert_eq!(
            s4_clean_text(Some(" x ".to_string())),
            Some("x".to_string())
        );
        assert_eq!(s4_require_text("  Sprint 1 ", "name").unwrap(), "Sprint 1");
        assert!(s4_require_text("   ", "name").is_err());
    }

    #[test]
    fn status_and_kind_rules() {
        assert!(s4_valid_sprint_status("active"));
        assert!(!s4_valid_sprint_status("archived"));
        assert!(s4_valid_phase_kind("dmaic"));
        assert!(!s4_valid_phase_kind("agile"));
        assert!(s4_valid_phase_status("planned"));
        assert!(s4_valid_gate_status("rejected"));
        assert!(!s4_valid_gate_status("pending_approval"));
        assert!(s4_valid_cycle_status("cooldown"));
        assert!(s4_valid_pi_status("completed"));
        assert!(s4_valid_business_case_status("draft"));
        assert!(!s4_valid_business_case_status("in_review"));
    }

    #[test]
    fn validate_date_range_allows_open_ranges() {
        let now = Utc::now();
        let later = now + chrono::Duration::days(7);
        assert!(s4_validate_date_range(Some(now), Some(later), "sprint").is_ok());
        assert!(s4_validate_date_range(Some(later), Some(now), "sprint").is_err());
        assert!(s4_validate_date_range(Some(now), Some(now), "sprint").is_err());
        assert!(s4_validate_date_range(None, Some(now), "sprint").is_ok());
        assert!(s4_validate_date_range(None, None, "sprint").is_ok());
    }

    #[test]
    fn archived_guard_rejects_archived_projects_only() {
        let mut project = crate::modules::projects::Project {
            id: "p1".to_string(),
            name: "Test".to_string(),
            description: String::new(),
            methodology: "scrum".to_string(),
            status: "active".to_string(),
            visibility: "private".to_string(),
            owner_id: "u1".to_string(),
            company_id: None,
            client_id: None,
            start_date: None,
            due_date: None,
            color: None,
            icon: None,
            labels: Vec::new(),
            custom_field_definitions: serde_json::json!({}),
            archived: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert!(s4_require_not_archived(&project).is_ok());
        project.archived = true;
        let error = s4_require_not_archived(&project).unwrap_err();
        assert_eq!(error.code, 409);
    }

    #[test]
    fn direct_status_change_is_rejected_when_present() {
        assert!(s4_reject_direct_status_change(None, "Sprint status").is_ok());
        assert!(s4_reject_direct_status_change(Some("active"), "Sprint status").is_err());
        assert!(s4_reject_direct_status_change(Some("approved"), "Phase gate_status").is_err());
    }
}
