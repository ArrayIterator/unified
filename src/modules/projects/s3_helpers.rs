// Projects module - task validation, sorting, and automation helpers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use chrono::Utc;
use serde_json::{Value, json};
use std::collections::HashSet;

// Private helpers (s3_ prefixed; nothing here may collide with s1)

pub(crate) fn s3_valid_task_type(task_type: &str) -> bool {
    matches!(
        task_type,
        "task" | "story" | "bug" | "spike" | "feature" | "enabler" | "chore"
    )
}

pub(crate) fn s3_valid_priority(priority: &str) -> bool {
    matches!(priority, "low" | "normal" | "high" | "urgent")
}

pub(crate) fn s3_priority_rank(priority: &str) -> u8 {
    match priority {
        "low" => 0,
        "high" => 2,
        "urgent" => 3,
        _ => 1,
    }
}

pub(crate) fn s3_valid_dependency_kind(kind: &str) -> bool {
    matches!(
        kind,
        "finish_start" | "start_start" | "finish_finish" | "start_finish" | "blocks" | "related"
    )
}

pub(crate) fn s3_valid_rule_trigger(trigger: &str) -> bool {
    matches!(
        trigger,
        "task_created" | "status_changed" | "task_completed" | "due_soon" | "label_added"
    )
}

/// A status is usable when it is one of the methodology profile defaults;
/// `done` and `cancelled` are always allowed. An unknown methodology
/// fails open so future methodologies keep working.
pub(crate) fn s3_methodology_allows_status(project: &Project, status: &str) -> bool {
    if status == "done" || status == "cancelled" {
        return true;
    }
    match methodology_profile(&project.methodology) {
        Some(profile) => profile
            .default_statuses
            .iter()
            .any(|candidate| candidate.to_string() == status),
        None => true,
    }
}

/// The methodology's first default status, used for new tasks.
pub(crate) fn s3_default_status(project: &Project) -> String {
    methodology_profile(&project.methodology)
        .and_then(|profile| {
            profile
                .default_statuses
                .first()
                .map(|status| status.to_string())
        })
        .unwrap_or_else(|| "todo".to_string())
}

/// The methodology's first non-terminal status, used when reopening.
pub(crate) fn s3_first_open_status(project: &Project) -> String {
    methodology_profile(&project.methodology)
        .and_then(|profile| {
            profile
                .default_statuses
                .iter()
                .map(|status| status.to_string())
                .find(|status| status != "done" && status != "cancelled")
        })
        .unwrap_or_else(|| "todo".to_string())
}

/// Terminal task statuses: `done` (and its `completed` alias) and
/// `cancelled`. No transition out of a terminal status is allowed
/// through the status handlers; reopening a done task goes through
/// the dedicated reopen handler only.
pub(crate) fn s3_task_is_terminal(status: &str) -> bool {
    matches!(status, "done" | "completed" | "cancelled")
}

/// Allowed task status transitions for a methodology, mirroring the
/// tickets module's explicit transition table: a task may move one
/// step forward or one step back along the methodology's ordered
/// statuses (so rework is possible but `backlog -> done` jumps are
/// not), or to `cancelled` from any open status. `done` and
/// `cancelled` are terminal. An unknown methodology fails open, as
/// `s3_methodology_allows_status` does, so future methodologies keep
/// working; the dedicated complete handler allows any open status
/// to reach done directly.
pub(crate) fn s3_allowed_task_transition(project: &Project, from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    if s3_task_is_terminal(from) {
        return false;
    }
    if to == "cancelled" {
        return true;
    }
    let Some(profile) = methodology_profile(&project.methodology) else {
        return true;
    };
    let ordered = profile.default_statuses;
    // `completed` is the alias of the final `done` status.
    let position = |status: &str| {
        let status = if status == "completed" { "done" } else { status };
        ordered.iter().position(|candidate| *candidate == status)
    };
    match (position(from), position(to)) {
        (Some(from_index), Some(to_index)) => {
            to_index == from_index + 1 || to_index + 1 == from_index
        }
        _ => false,
    }
}

/// Sort a task list by an allowed sort field, then apply the direction.
pub(crate) fn s3_sort_tasks(items: &mut Vec<ProjectTask>, sort: &str, descending: bool) {
    match sort {
        "updated_at" => items.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
        "due_date" => items.sort_by(|a, b| {
            (a.due_date.is_none(), a.due_date).cmp(&(b.due_date.is_none(), b.due_date))
        }),
        "priority" => items.sort_by_key(|task| s3_priority_rank(&task.priority)),
        "estimate_points" => items.sort_by(|a, b| {
            a.estimate_points
                .unwrap_or(0.0)
                .partial_cmp(&b.estimate_points.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if descending {
        items.reverse();
    }
}

// --- Container references: every referenced container must exist in the
// same project, otherwise the reference is rejected as a bad request. ---

pub(crate) fn s3_require_board_in_project(
    state: &ProjectsState,
    project_id: &str,
    board_id: &str,
) -> UnifiedResult<()> {
    match state.boards.get(board_id) {
        Some(board) if board.value().project_id == project_id => Ok(()),
        _ => Err(UnifiedError::bad_request(
            "board_id does not reference a board in this project",
        )),
    }
}

pub(crate) fn s3_require_sprint_in_project(
    state: &ProjectsState,
    project_id: &str,
    sprint_id: &str,
) -> UnifiedResult<()> {
    match state.sprints.get(sprint_id) {
        Some(sprint) if sprint.value().project_id == project_id => Ok(()),
        _ => Err(UnifiedError::bad_request(
            "sprint_id does not reference a sprint in this project",
        )),
    }
}

pub(crate) fn s3_require_phase_in_project(
    state: &ProjectsState,
    project_id: &str,
    phase_id: &str,
) -> UnifiedResult<()> {
    match state.phases.get(phase_id) {
        Some(phase) if phase.value().project_id == project_id => Ok(()),
        _ => Err(UnifiedError::bad_request(
            "phase_id does not reference a phase in this project",
        )),
    }
}

pub(crate) fn s3_require_cycle_in_project(
    state: &ProjectsState,
    project_id: &str,
    cycle_id: &str,
) -> UnifiedResult<()> {
    match state.cycles.get(cycle_id) {
        Some(cycle) if cycle.value().project_id == project_id => Ok(()),
        _ => Err(UnifiedError::bad_request(
            "cycle_id does not reference a cycle in this project",
        )),
    }
}

pub(crate) fn s3_require_pi_in_project(
    state: &ProjectsState,
    project_id: &str,
    pi_id: &str,
) -> UnifiedResult<()> {
    match state.program_increments.get(pi_id) {
        Some(increment) if increment.value().project_id == project_id => Ok(()),
        _ => Err(UnifiedError::bad_request(
            "pi_id does not reference a program increment in this project",
        )),
    }
}

pub(crate) fn s3_require_milestone_in_project(
    state: &ProjectsState,
    project_id: &str,
    milestone_id: &str,
) -> UnifiedResult<()> {
    match state.milestones.get(milestone_id) {
        Some(milestone) if milestone.value().project_id == project_id => Ok(()),
        _ => Err(UnifiedError::bad_request(
            "milestone_id does not reference a milestone in this project",
        )),
    }
}

pub(crate) fn s3_validate_task_refs(
    state: &ProjectsState,
    project_id: &str,
    board_id: Option<&str>,
    sprint_id: Option<&str>,
    phase_id: Option<&str>,
    cycle_id: Option<&str>,
    pi_id: Option<&str>,
    milestone_id: Option<&str>,
) -> UnifiedResult<()> {
    if let Some(board_id) = board_id {
        s3_require_board_in_project(state, project_id, board_id)?;
    }
    if let Some(sprint_id) = sprint_id {
        s3_require_sprint_in_project(state, project_id, sprint_id)?;
    }
    if let Some(phase_id) = phase_id {
        s3_require_phase_in_project(state, project_id, phase_id)?;
    }
    if let Some(cycle_id) = cycle_id {
        s3_require_cycle_in_project(state, project_id, cycle_id)?;
    }
    if let Some(pi_id) = pi_id {
        s3_require_pi_in_project(state, project_id, pi_id)?;
    }
    if let Some(milestone_id) = milestone_id {
        s3_require_milestone_in_project(state, project_id, milestone_id)?;
    }
    Ok(())
}

// Board columns and work-in-progress limits.

/// Find the column for a status key, scoped to the project and, when a
/// board is known, to that board. Columns carry only a `board_id`, so
/// project scope is resolved through the column's board.
pub(crate) fn s3_find_column(
    state: &ProjectsState,
    project_id: &str,
    board_id: Option<&str>,
    status_key: &str,
) -> Option<ProjectBoardColumn> {
    state
        .columns
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|column| {
            column.status_key == status_key
                && board_id.is_none_or(|board_id| column.board_id == board_id)
        })
        .find(|column| {
            state
                .boards
                .get(&column.board_id)
                .is_some_and(|board| board.value().project_id == project_id)
        })
}

/// Reject the move when the target column has a WIP limit and is already
/// full. The moving task itself is not counted when it already sits in
/// that column. Columns without a limit never block a move.
pub(crate) fn s3_check_wip_limit(
    state: &ProjectsState,
    project_id: &str,
    board_id: Option<&str>,
    status: &str,
    moving_task_id: &str,
) -> UnifiedResult<()> {
    let Some(column) = s3_find_column(state, project_id, board_id, status) else {
        return Ok(());
    };
    let Some(limit) = column.wip_limit else {
        return Ok(());
    };
    let occupying = state
        .tasks
        .iter()
        .filter(|entry| {
            let task = entry.value();
            task.project_id == project_id
                && task.board_id.as_deref() == board_id
                && task.status == status
                && task.id != moving_task_id
        })
        .count();
    if occupying as u32 >= limit {
        return Err(UnifiedError::bad_request(format!(
            "Target column \"{status}\" has reached its work-in-progress limit of {limit}"
        )));
    }
    Ok(())
}

// Task tree and cascade delete.

/// Collect a task id plus every descendant subtask id (breadth first).
pub(crate) fn s3_collect_task_tree(state: &ProjectsState, root_id: &str) -> Vec<String> {
    let mut collected = vec![root_id.to_string()];
    let mut index = 0;
    while index < collected.len() {
        let parent_id = collected[index].clone();
        index += 1;
        for entry in state.tasks.iter() {
            let task = entry.value();
            if task.parent_id.as_deref() == Some(parent_id.as_str())
                && !collected.contains(&task.id)
            {
                collected.push(task.id.clone());
            }
        }
    }
    collected
}

/// Delete a task and its whole subtask tree, plus every dependency (in
/// either direction), comment, watcher, attachment and time entry that
/// belongs to any of them. Returns the number of tasks removed.
pub(crate) fn s3_delete_task_cascade(state: &ProjectsState, task_id: &str) -> usize {
    let doomed = s3_collect_task_tree(state, task_id);
    let doomed_set: std::collections::HashSet<&str> = doomed.iter().map(String::as_str).collect();
    state.dependencies.retain(|_, dependency| {
        !doomed_set.contains(dependency.task_id.as_str())
            && !doomed_set.contains(dependency.depends_on_task_id.as_str())
    });
    state
        .comments
        .retain(|_, comment| !doomed_set.contains(comment.task_id.as_str()));
    state
        .watchers
        .retain(|_, watcher| !doomed_set.contains(watcher.task_id.as_str()));
    state
        .attachments
        .retain(|_, attachment| !doomed_set.contains(attachment.task_id.as_str()));
    state
        .time_entries
        .retain(|_, entry| !doomed_set.contains(entry.task_id.as_str()));
    let count = doomed.len();
    for id in doomed {
        state.tasks.remove(id.as_str());
    }
    count
}

// Automation rules.

/// Validate the actions array of a rule: non-empty, every action known,
/// and every action carrying the parameters it needs. `set_status`
/// values are checked against the project methodology.
pub(crate) fn s3_validate_rule_actions(project: &Project, actions: &Value) -> UnifiedResult<()> {
    let Some(list) = actions.as_array() else {
        return Err(UnifiedError::bad_request("actions must be a JSON array"));
    };
    if list.is_empty() {
        return Err(UnifiedError::bad_request(
            "actions must be a non-empty JSON array",
        ));
    }
    for action in list {
        let Some(kind) = action.get("action").and_then(|value| value.as_str()) else {
            return Err(UnifiedError::bad_request(
                "Each action must have an action name",
            ));
        };
        match kind {
            "set_status" => {
                let Some(status) = action.get("status").and_then(|value| value.as_str()) else {
                    return Err(UnifiedError::bad_request(
                        "set_status action requires a status",
                    ));
                };
                if !s3_methodology_allows_status(project, status) {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown status for this project methodology: {status}"
                    )));
                }
            }
            "assign" => {
                let has_target = action
                    .get("assignee_id")
                    .and_then(|value| value.as_str())
                    .is_some()
                    || action
                        .get("user_id")
                        .and_then(|value| value.as_str())
                        .is_some();
                if !has_target {
                    return Err(UnifiedError::bad_request(
                        "assign action requires assignee_id",
                    ));
                }
            }
            "add_label" => match action.get("label").and_then(|value| value.as_str()) {
                Some(label) if !label.trim().is_empty() => {}
                _ => {
                    return Err(UnifiedError::bad_request(
                        "add_label action requires a label",
                    ));
                }
            },
            "notify" => {}
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown automation action: {other}"
                )));
            }
        }
    }
    Ok(())
}

/// Conditions JSON supports {status?, assignee_id?, label?, overdue?}.
/// Every present condition must hold for the rule to match.
pub(crate) fn s3_rule_conditions_match(task: &ProjectTask, conditions: &Value) -> bool {
    if let Some(status) = conditions.get("status").and_then(|value| value.as_str()) {
        if task.status != status {
            return false;
        }
    }
    if let Some(assignee_id) = conditions
        .get("assignee_id")
        .and_then(|value| value.as_str())
    {
        if task.assignee_id.as_deref() != Some(assignee_id) {
            return false;
        }
    }
    if let Some(label) = conditions.get("label").and_then(|value| value.as_str()) {
        if !task.labels.iter().any(|task_label| task_label == label) {
            return false;
        }
    }
    if let Some(overdue) = conditions.get("overdue").and_then(|value| value.as_bool()) {
        let is_overdue = match task.due_date {
            Some(due_date) => {
                due_date < now() && task.status != "done" && task.status != "cancelled"
            }
            None => false,
        };
        if is_overdue != overdue {
            return false;
        }
    }
    true
}

/// Apply one rule's actions to a task in memory. Returns the number of
/// actions that actually changed something (or sent a notification).
/// Invalid action parameters are skipped, never fatal: a rule must not
/// break the task write that triggered it.
pub(crate) fn s3_apply_rule_actions(
    state: &ProjectsState,
    project: &Project,
    task: &mut ProjectTask,
    rule: &ProjectAutomationRule,
) -> usize {
    let Some(actions) = rule.actions.as_array() else {
        return 0;
    };
    let mut applied = 0;
    for action in actions {
        let Some(kind) = action.get("action").and_then(|value| value.as_str()) else {
            continue;
        };
        match kind {
            "set_status" => {
                if let Some(status) = action.get("status").and_then(|value| value.as_str()) {
                    if s3_methodology_allows_status(project, status) && task.status != status {
                        if status == "done" {
                            task.completed_at = Some(now());
                        } else if task.status == "done" {
                            task.completed_at = None;
                        }
                        task.status = status.to_string();
                        applied += 1;
                    }
                }
            }
            "assign" => {
                let target = action
                    .get("assignee_id")
                    .and_then(|value| value.as_str())
                    .or_else(|| action.get("user_id").and_then(|value| value.as_str()));
                if let Some(user_id) = target {
                    if state.is_member(&project.id, user_id)
                        && task.assignee_id.as_deref() != Some(user_id)
                    {
                        task.assignee_id = Some(user_id.to_string());
                        applied += 1;
                    }
                }
            }
            "add_label" => {
                if let Some(label) = action.get("label").and_then(|value| value.as_str()) {
                    if !label.trim().is_empty()
                        && !task.labels.iter().any(|task_label| task_label == label)
                    {
                        task.labels.push(label.to_string());
                        applied += 1;
                    }
                }
            }
            "notify" => {
                let target = action
                    .get("user_id")
                    .and_then(|value| value.as_str())
                    .or_else(|| action.get("assignee_id").and_then(|value| value.as_str()))
                    .map(str::to_string)
                    .or_else(|| task.assignee_id.clone())
                    .unwrap_or_else(|| task.reporter_id.clone());
                let title = action
                    .get("title")
                    .and_then(|value| value.as_str())
                    .unwrap_or("Project automation")
                    .to_string();
                let body = action
                    .get("body")
                    .and_then(|value| value.as_str())
                    .unwrap_or("An automation rule ran for this task.")
                    .to_string();
                state.notify(&target, title, body);
                applied += 1;
            }
            _ => {}
        }
    }
    applied
}

/// Run every enabled rule of the project whose trigger matches, against
/// the current stored copy of the task. One pass only: actions never
/// chain into further rule runs, so rules cannot loop. Rules that apply
/// at least one action bump their own run count and last run time.
pub(crate) fn apply_automation_rules(
    state: &ProjectsState,
    project: &Project,
    task: &ProjectTask,
    trigger: &str,
) {
    let mut rules: Vec<ProjectAutomationRule> = state
        .automation_rules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rule| rule.project_id == project.id && rule.enabled && rule.trigger == trigger)
        .collect();
    rules.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    for rule in rules {
        let Some(mut current) = state
            .tasks
            .get(task.id.as_str())
            .map(|entry| entry.value().clone())
        else {
            continue;
        };
        if !s3_rule_conditions_match(&current, &rule.conditions) {
            continue;
        }
        let applied = s3_apply_rule_actions(state, project, &mut current, &rule);
        if applied > 0 {
            current.updated_at = now();
            state.tasks.insert(current.id.clone(), current);
            if let Some(mut stored) = state.automation_rules.get_mut(rule.id.as_str()) {
                stored.run_count += 1;
                stored.last_run_at = Some(now());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::projects::Project;
    use serde_json::json;

    fn project_with_methodology(methodology: &str) -> Project {
        Project {
            id: "p1".to_string(),
            name: "Test".to_string(),
            description: String::new(),
            methodology: methodology.to_string(),
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
            custom_field_definitions: json!({}),
            archived: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn task_type_priority_dependency_and_trigger_rules() {
        assert!(s3_valid_task_type("bug"));
        assert!(s3_valid_task_type("enabler"));
        assert!(!s3_valid_task_type("epic"));
        assert!(s3_valid_priority("urgent"));
        assert!(!s3_valid_priority("critical"));
        assert!(s3_valid_dependency_kind("blocks"));
        assert!(!s3_valid_dependency_kind("parent"));
        assert!(s3_valid_rule_trigger("due_soon"));
        assert!(!s3_valid_rule_trigger("on_comment"));
    }

    #[test]
    fn priority_rank_orders_urgency() {
        assert_eq!(s3_priority_rank("low"), 0);
        assert_eq!(s3_priority_rank("normal"), 1);
        assert_eq!(s3_priority_rank("high"), 2);
        assert_eq!(s3_priority_rank("urgent"), 3);
        assert_eq!(s3_priority_rank("unknown"), 1);
    }

    #[test]
    fn default_and_first_open_status_follow_the_methodology() {
        let scrum = project_with_methodology("scrum");
        assert_eq!(s3_default_status(&scrum), "backlog");
        assert_eq!(s3_first_open_status(&scrum), "backlog");
        let unknown = project_with_methodology("not-a-methodology");
        assert_eq!(s3_default_status(&unknown), "todo");
        assert_eq!(s3_first_open_status(&unknown), "todo");
    }

    #[test]
    fn methodology_allows_status_rules() {
        let scrum = project_with_methodology("scrum");
        assert!(s3_methodology_allows_status(&scrum, "review"));
        assert!(!s3_methodology_allows_status(&scrum, "shipped"));
        assert!(s3_methodology_allows_status(&scrum, "done"));
        assert!(s3_methodology_allows_status(&scrum, "cancelled"));
        let unknown = project_with_methodology("not-a-methodology");
        assert!(s3_methodology_allows_status(&unknown, "anything"));
    }

    #[test]
    fn task_transition_table_blocks_jumps_and_terminal_exits() {
        // Scrum order: backlog, sprint_backlog, in_progress, review, done.
        let scrum = project_with_methodology("scrum");
        assert!(s3_allowed_task_transition(&scrum, "backlog", "sprint_backlog"));
        assert!(s3_allowed_task_transition(&scrum, "review", "in_progress"));
        assert!(s3_allowed_task_transition(&scrum, "review", "done"));
        assert!(s3_allowed_task_transition(&scrum, "in_progress", "cancelled"));
        assert!(s3_allowed_task_transition(&scrum, "backlog", "backlog"));
        assert!(!s3_allowed_task_transition(&scrum, "backlog", "done"));
        assert!(!s3_allowed_task_transition(&scrum, "backlog", "in_progress"));
        assert!(!s3_allowed_task_transition(&scrum, "in_progress", "done"));
        assert!(!s3_allowed_task_transition(&scrum, "done", "in_progress"));
        assert!(!s3_allowed_task_transition(&scrum, "done", "cancelled"));
        assert!(!s3_allowed_task_transition(&scrum, "cancelled", "done"));
        assert!(!s3_allowed_task_transition(&scrum, "cancelled", "backlog"));
        assert!(s3_task_is_terminal("done"));
        assert!(s3_task_is_terminal("completed"));
        assert!(s3_task_is_terminal("cancelled"));
        assert!(!s3_task_is_terminal("review"));
        // Unknown methodology fails open for open statuses only.
        let unknown = project_with_methodology("not-a-methodology");
        assert!(s3_allowed_task_transition(&unknown, "todo", "done"));
        assert!(!s3_allowed_task_transition(&unknown, "done", "todo"));
    }
}
