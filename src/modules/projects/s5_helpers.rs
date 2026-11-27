// Projects module - time, CSV, statistics, and cycle helpers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::error::UnifiedResult;
use unified_core::json;
use actix_web::HttpResponse;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;

// Private helpers (all prefixed `s5_`)

/// Round a float to two decimal places.
pub(crate) fn s5_round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// Hours for a stopped timer: the exact elapsed time rounded to two
/// decimals, with no minimum billing floor. A timer stopped
/// immediately therefore records 0 hours instead of 0.01h.
pub(crate) fn s5_timer_hours(elapsed_ms: i64) -> f64 {
    s5_round2(elapsed_ms as f64 / 3_600_000.0)
}

/// Round a float to four decimal places.
pub(crate) fn s5_round4(value: f64) -> f64 {
    (value * 10000.0).round() / 10000.0
}

/// A task counts as done when it has a completion timestamp or its
/// status is one of the terminal done spellings.
pub(crate) fn s5_task_is_done(task: &ProjectTask) -> bool {
    task.completed_at.is_some() || task.status == "done" || task.status == "completed"
}

/// A task counts as open when it is neither done nor cancelled/closed.
pub(crate) fn s5_task_is_open(task: &ProjectTask) -> bool {
    !s5_task_is_done(task)
        && task.status != "cancelled"
        && task.status != "closed"
        && task.status != "archived"
}

/// Hours for a single time entry must be greater than zero and at most
/// one full day.
pub(crate) fn s5_valid_hours(hours: f64) -> bool {
    hours > 0.0 && hours <= 24.0
}

/// Parse a `"true"` / `"false"` query filter value.
pub(crate) fn s5_parse_flag(value: Option<&String>) -> Option<bool> {
    match value.map(String::as_str) {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    }
}

/// Escape one CSV field: double any double quotes, and wrap the field
/// in double quotes when it contains a comma, a quote, or a newline.
pub(crate) fn s5_csv_escape(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

/// Format an optional float for CSV output: empty when absent.
pub(crate) fn s5_csv_number(value: Option<f64>) -> String {
    match value {
        Some(number) => {
            if number == number.trunc() {
                format!("{}", number as i64)
            } else {
                format!("{number}")
            }
        }
        None => String::new(),
    }
}

/// Format an optional timestamp for CSV output (RFC 3339, empty when
/// absent).
pub(crate) fn s5_csv_datetime(value: Option<DateTime<Utc>>) -> String {
    match value {
        Some(moment) => moment.to_rfc3339(),
        None => String::new(),
    }
}

/// Median of a sorted slice of floats. Returns 0.0 for an empty slice.
pub(crate) fn s5_median(sorted: &[f64]) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[middle]
    } else {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    }
}

/// Nearest-rank percentile of a sorted slice. Returns 0.0 for an empty
/// slice. `percentile` is 0.0..=100.0.
pub(crate) fn s5_percentile(sorted: &[f64], percentile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (percentile / 100.0 * sorted.len() as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[index]
}

/// End of a calendar day (23:59:59 UTC).
pub(crate) fn s5_end_of_day(day: chrono::NaiveDate) -> DateTime<Utc> {
    day.and_hms_opt(23, 59, 59)
        .map(|naive| naive.and_utc())
        .unwrap_or_else(now)
}

/// Recursive depth-first search used to recover one concrete cycle
/// from a dependency graph that is not a DAG. `state` maps a task id
/// to 1 while it is on the current path and 2 once fully explored.
/// Returns the ids forming the cycle, with the closing id repeated
/// at the end, or `None` when no cycle is reachable from `node`.
pub(crate) fn s5_cycle_dfs(
    node: &str,
    successors: &std::collections::HashMap<String, Vec<String>>,
    state: &mut std::collections::HashMap<String, u8>,
    stack: &mut Vec<String>,
) -> Option<Vec<String>> {
    state.insert(node.to_string(), 1);
    stack.push(node.to_string());
    if let Some(next_nodes) = successors.get(node) {
        for next in next_nodes {
            let next_state = state.get(next).copied().unwrap_or(0);
            if next_state == 1 {
                let position = stack.iter().position(|id| id == next).unwrap_or(0);
                let mut cycle: Vec<String> = stack[position..].to_vec();
                cycle.push(next.clone());
                return Some(cycle);
            }
            if next_state == 0 {
                if let Some(cycle) = s5_cycle_dfs(next, successors, state, stack) {
                    return Some(cycle);
                }
            }
        }
    }
    stack.pop();
    state.insert(node.to_string(), 2);
    None
}

/// Turn a create result into a 201 success response or an error
/// response, using the standard envelope.
pub(crate) fn s5_respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

/// Emit the time-logged events for a time entry: always
/// `project.time.logged`, and additionally `billing.time_logged` when
/// the entry is billable so billing can invoice it. Emission only;
/// this never calls billing directly.
pub(crate) fn s5_emit_time_logged(state: &ProjectsState, entry: &ProjectTimeEntry) {
    state.emit(
        "project.time.logged",
        json!({
            "project_id": entry.project_id,
            "task_id": entry.task_id,
            "entry_id": entry.id,
            "user_id": entry.user_id,
            "hours": entry.hours,
            "billable": entry.billable,
        }),
    );
    if entry.billable {
        state.emit(
            "billing.time_logged",
            json!({
                "project_id": entry.project_id,
                "task_id": entry.task_id,
                "user_id": entry.user_id,
                "hours": entry.hours,
                "entry_id": entry.id,
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::projects::ProjectTask;
    use serde_json::json;

    fn task_with(status: &str, completed_at: Option<DateTime<Utc>>) -> ProjectTask {
        ProjectTask {
            id: "t1".to_string(),
            project_id: "p1".to_string(),
            board_id: None,
            parent_id: None,
            epic_id: None,
            title: "Task".to_string(),
            description: String::new(),
            task_type: "task".to_string(),
            status: status.to_string(),
            priority: "normal".to_string(),
            assignee_id: None,
            reporter_id: "u1".to_string(),
            estimate_points: None,
            estimate_hours: None,
            labels: Vec::new(),
            custom_values: json!({}),
            start_date: None,
            due_date: None,
            completed_at,
            sprint_id: None,
            phase_id: None,
            cycle_id: None,
            pi_id: None,
            milestone_id: None,
            process_group: None,
            knowledge_area: None,
            waste_flag: None,
            pair_partner_id: None,
            test_first: false,
            dmaic_phase: None,
            appetite_weeks: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn rounding_helpers() {
        assert_eq!(s5_round2(1.234), 1.23);
        assert_eq!(s5_round2(1.236), 1.24);
        assert_eq!(s5_round4(1.23456), 1.2346);
        assert_eq!(s5_round4(1.23454), 1.2345);
    }

    #[test]
    fn timer_hours_have_no_minimum_floor() {
        assert_eq!(s5_timer_hours(0), 0.0);
        assert_eq!(s5_timer_hours(1_000), 0.0);
        assert_eq!(s5_timer_hours(3_600_000), 1.0);
        assert_eq!(s5_timer_hours(5_400_000), 1.5);
    }

    #[test]
    fn task_done_and_open_states() {
        assert!(s5_task_is_done(&task_with("done", None)));
        assert!(s5_task_is_done(&task_with("in_progress", Some(Utc::now()))));
        assert!(!s5_task_is_done(&task_with("in_progress", None)));
        assert!(s5_task_is_open(&task_with("in_progress", None)));
        assert!(!s5_task_is_open(&task_with("done", None)));
        assert!(!s5_task_is_open(&task_with("cancelled", None)));
        assert!(!s5_task_is_open(&task_with("closed", None)));
    }

    #[test]
    fn valid_hours_and_flag_parsing() {
        assert!(s5_valid_hours(0.5));
        assert!(s5_valid_hours(24.0));
        assert!(!s5_valid_hours(0.0));
        assert!(!s5_valid_hours(24.5));
        assert_eq!(s5_parse_flag(Some(&"true".to_string())), Some(true));
        assert_eq!(s5_parse_flag(Some(&"false".to_string())), Some(false));
        assert_eq!(s5_parse_flag(Some(&"yes".to_string())), None);
        assert_eq!(s5_parse_flag(None), None);
    }

    #[test]
    fn csv_escape_and_number_formatting() {
        assert_eq!(s5_csv_escape("plain"), "plain");
        assert_eq!(s5_csv_escape("a,b"), "\"a,b\"");
        assert_eq!(s5_csv_escape("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(s5_csv_escape("line\nbreak"), "\"line\nbreak\"");
        assert_eq!(s5_csv_number(None), "");
        assert_eq!(s5_csv_number(Some(3.0)), "3");
        assert_eq!(s5_csv_number(Some(2.5)), "2.5");
        assert_eq!(s5_csv_datetime(None), "");
    }

    #[test]
    fn median_and_percentile() {
        assert_eq!(s5_median(&[]), 0.0);
        assert_eq!(s5_median(&[1.0, 2.0, 3.0]), 2.0);
        assert_eq!(s5_median(&[1.0, 2.0, 3.0, 4.0]), 2.5);
        assert_eq!(s5_percentile(&[], 50.0), 0.0);
        assert_eq!(s5_percentile(&[1.0, 2.0, 3.0, 4.0], 50.0), 2.0);
        assert_eq!(s5_percentile(&[1.0, 2.0, 3.0, 4.0], 100.0), 4.0);
        assert_eq!(s5_percentile(&[1.0, 2.0, 3.0, 4.0], 25.0), 1.0);
    }

    #[test]
    fn cycle_dfs_finds_cycles_only() {
        let mut successors = std::collections::HashMap::new();
        successors.insert("a".to_string(), vec!["b".to_string()]);
        successors.insert("b".to_string(), vec!["a".to_string()]);
        let mut state = std::collections::HashMap::new();
        let mut stack = Vec::new();
        let cycle = s5_cycle_dfs("a", &successors, &mut state, &mut stack);
        assert_eq!(
            cycle,
            Some(vec!["a".to_string(), "b".to_string(), "a".to_string()])
        );
        let mut acyclic = std::collections::HashMap::new();
        acyclic.insert("a".to_string(), vec!["b".to_string()]);
        acyclic.insert("b".to_string(), Vec::new());
        let mut state = std::collections::HashMap::new();
        let mut stack = Vec::new();
        assert_eq!(s5_cycle_dfs("a", &acyclic, &mut state, &mut stack), None);
    }
}
