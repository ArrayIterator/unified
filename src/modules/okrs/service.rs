// OKR / Goals — Business logic shared by handlers and hooks.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::handlers_cycles::*;
use super::handlers_objectives::*;
use super::handlers_key_results::*;
use super::hooks::*;
use super::module::*;

/// Progress of one key result: (current - start) / (target -
/// start), clamped to 0.0–1.0. A zero target range scores 1.0 when
/// the current value reached the target, else 0.0.
pub(crate) fn key_result_progress(key_result: &OkrKeyResult) -> f64 {
    let span = key_result.target_value - key_result.start_value;
    if span == 0.0 {
        if key_result.current_value >= key_result.target_value {
            1.0
        } else {
            0.0
        }
    } else {
        ((key_result.current_value - key_result.start_value) / span).clamp(0.0, 1.0)
    }
}

pub(crate) fn load_cycle(state: &OkrState, raw_id: &str) -> UnifiedResult<OkrCycle> {
    let id = parse_uuid(raw_id, "cycle")?;
    state
        .cycles
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Cycle not found: {id}")))
}

pub(crate) fn load_objective(state: &OkrState, raw_id: &str) -> UnifiedResult<OkrObjective> {
    let id = parse_uuid(raw_id, "objective")?;
    state
        .objectives
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Objective not found: {id}")))
}

pub(crate) fn load_key_result(state: &OkrState, raw_id: &str) -> UnifiedResult<OkrKeyResult> {
    let id = parse_uuid(raw_id, "key result")?;
    state
        .key_results
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Key result not found: {id}")))
}

pub(crate) fn load_link(state: &OkrState, raw_id: &str) -> UnifiedResult<OkrLink> {
    let id = parse_uuid(raw_id, "link")?;
    state
        .links
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Link not found: {id}")))
}

pub(crate) fn key_results_of(state: &OkrState, objective_id: Uuid) -> Vec<OkrKeyResult> {
    let mut out: Vec<OkrKeyResult> = state
        .key_results
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|key_result| key_result.objective_id == objective_id)
        .collect();
    out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    out
}

pub(crate) fn links_of(state: &OkrState, key_result_id: Uuid) -> Vec<OkrLink> {
    let mut out: Vec<OkrLink> = state
        .links
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|link| link.key_result_id == key_result_id)
        .collect();
    out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    out
}

/// Mean key-result progress of an objective, or `None` when it has
/// no key results.
pub(crate) fn objective_progress(state: &OkrState, objective_id: Uuid) -> Option<f64> {
    let results = key_results_of(state, objective_id);
    if results.is_empty() {
        return None;
    }
    let sum: f64 = results.iter().map(key_result_progress).sum();
    Some((sum / results.len() as f64).clamp(0.0, 1.0))
}

/// Share of the cycle elapsed at `now`, 0.0 before it starts and
/// 1.0 after it ends. `None` when the cycle has zero length.
pub(crate) fn cycle_elapsed(cycle: &OkrCycle, now: DateTime<Utc>) -> Option<f64> {
    let total = (cycle.ends_at - cycle.starts_at).num_seconds();
    if total <= 0 {
        return None;
    }
    let elapsed = (now - cycle.starts_at).num_seconds();
    Some((elapsed as f64 / total as f64).clamp(0.0, 1.0))
}

/// Computed status from progress versus elapsed cycle time:
/// progress within 0.10 of the expected share is on track, within
/// 0.25 is at risk, further behind is off track. Before the cycle
/// starts the status is `not_started`; a closed cycle reports
/// `closed`; an objective with no key results reports
/// `no_key_results`; a zero-length cycle reports `invalid_cycle`
/// (its elapsed share is undefined, so progress cannot be judged
/// against it even when key results exist).
pub(crate) fn objective_status(
    state: &OkrState,
    objective: &OkrObjective,
    now: DateTime<Utc>,
) -> (Option<f64>, Option<f64>, String) {
    let progress = objective_progress(state, objective.id);
    let Some(cycle) = state.cycles.get(&objective.cycle_id).map(|entry| entry.value().clone())
    else {
        return (progress, None, "no_cycle".to_string());
    };
    if cycle.status == CycleStatus::Closed {
        return (progress, Some(1.0), "closed".to_string());
    }
    if now < cycle.starts_at {
        return (progress, Some(0.0), "not_started".to_string());
    }
    let Some(expected) = cycle_elapsed(&cycle, now) else {
        return (progress, None, "invalid_cycle".to_string());
    };
    let Some(progress) = progress else {
        return (None, Some(expected), "no_key_results".to_string());
    };
    let gap = expected - progress;
    let status = if gap <= 0.10 {
        "on_track"
    } else if gap <= 0.25 {
        "at_risk"
    } else {
        "off_track"
    };
    (Some(progress), Some(expected), status.to_string())
}

pub(crate) fn objective_view(state: &OkrState, objective: &OkrObjective) -> ObjectiveView {
    let now = Utc::now();
    let (progress, expected_progress, status) = objective_status(state, objective, now);
    ObjectiveView {
        objective: objective.clone(),
        progress,
        expected_progress,
        status,
        key_result_count: key_results_of(state, objective.id).len(),
    }
}

pub(crate) fn key_result_view(state: &OkrState, key_result: &OkrKeyResult) -> KeyResultView {
    let links = links_of(state, key_result.id);
    let task_links: Vec<&OkrLink> = links
        .iter()
        .filter(|link| link.link_type == "task")
        .collect();
    let completed = task_links
        .iter()
        .filter(|link| state.completed_tasks.contains_key(&link.target_id))
        .count();
    KeyResultView {
        key_result: key_result.clone(),
        progress: key_result_progress(key_result),
        linked_task_count: task_links.len(),
        completed_task_count: completed,
    }
}

/// Recompute one key result's current value from its linked tasks:
/// current = start + (completed / total) * (target - start). A key
/// result with no task links is left untouched. Returns true when
/// the stored value changed.
pub(crate) fn refresh_rollup(state: &OkrState, key_result_id: Uuid) -> bool {
    let Some(key_result) = state
        .key_results
        .get(&key_result_id)
        .map(|entry| entry.value().clone())
    else {
        return false;
    };
    let task_links: Vec<OkrLink> = links_of(state, key_result_id)
        .into_iter()
        .filter(|link| link.link_type == "task")
        .collect();
    if task_links.is_empty() {
        return false;
    }
    let completed = task_links
        .iter()
        .filter(|link| state.completed_tasks.contains_key(&link.target_id))
        .count();
    let fraction = completed as f64 / task_links.len() as f64;
    let next = key_result.start_value
        + fraction * (key_result.target_value - key_result.start_value);
    if (next - key_result.current_value).abs() < f64::EPSILON {
        return false;
    }
    if let Some(mut stored) = state.key_results.get_mut(&key_result_id) {
        stored.current_value = next;
        stored.updated_at = Utc::now();
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn at(timestamp: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(timestamp, 0).unwrap()
    }

    fn key_result(start: f64, target: f64, current: f64) -> OkrKeyResult {
        OkrKeyResult {
            id: Uuid::nil(),
            objective_id: Uuid::nil(),
            title: "KR".to_string(),
            unit: "%".to_string(),
            target_value: target,
            start_value: start,
            current_value: current,
            created_at: at(1_700_000_000),
            updated_at: at(1_700_000_000),
        }
    }

    fn cycle(status: CycleStatus) -> OkrCycle {
        OkrCycle {
            id: Uuid::nil(),
            company_id: "acme".to_string(),
            name: "Q1".to_string(),
            description: None,
            starts_at: at(1_000),
            ends_at: at(2_000),
            status,
            created_at: at(1_000),
            updated_at: at(1_000),
        }
    }

    fn make_objective(cycle_id: Uuid) -> OkrObjective {
        OkrObjective {
            id: Uuid::nil(),
            cycle_id,
            company_id: "acme".to_string(),
            parent_objective_id: None,
            title: "Grow".to_string(),
            description: None,
            level: "company".to_string(),
            owner_id: "user-1".to_string(),
            final_score: None,
            created_at: at(1_000),
            updated_at: at(1_000),
        }
    }

    #[test]
    fn key_result_progress_is_the_share_of_the_range() {
        assert_eq!(key_result_progress(&key_result(0.0, 100.0, 50.0)), 0.5);
        assert_eq!(key_result_progress(&key_result(10.0, 20.0, 15.0)), 0.5);
        assert_eq!(key_result_progress(&key_result(0.0, 100.0, 0.0)), 0.0);
        assert_eq!(key_result_progress(&key_result(0.0, 100.0, 100.0)), 1.0);
    }

    #[test]
    fn key_result_progress_clamps_out_of_range_values() {
        assert_eq!(key_result_progress(&key_result(0.0, 100.0, 150.0)), 1.0);
        assert_eq!(key_result_progress(&key_result(0.0, 100.0, -10.0)), 0.0);
    }

    #[test]
    fn key_result_progress_handles_a_zero_target_range() {
        assert_eq!(key_result_progress(&key_result(10.0, 10.0, 10.0)), 1.0);
        assert_eq!(key_result_progress(&key_result(10.0, 10.0, 5.0)), 0.0);
    }

    #[test]
    fn key_result_progress_works_for_reversed_ranges() {
        assert_eq!(key_result_progress(&key_result(100.0, 0.0, 50.0)), 0.5);
        assert_eq!(key_result_progress(&key_result(100.0, 0.0, 100.0)), 0.0);
        assert_eq!(key_result_progress(&key_result(100.0, 0.0, 0.0)), 1.0);
    }

    #[test]
    fn cycle_elapsed_is_the_share_of_the_cycle() {
        let cycle = cycle(CycleStatus::Active);
        assert_eq!(cycle_elapsed(&cycle, at(1_500)), Some(0.5));
        assert_eq!(cycle_elapsed(&cycle, at(500)), Some(0.0));
        assert_eq!(cycle_elapsed(&cycle, at(2_500)), Some(1.0));
        let zero = OkrCycle { starts_at: at(1_000), ends_at: at(1_000), ..cycle };
        assert_eq!(cycle_elapsed(&zero, at(1_000)), None);
    }

    #[test]
    fn objective_progress_averages_key_results() {
        let state = OkrState::default();
        let objective = make_objective(Uuid::nil());
        state.objectives.insert(objective.id, objective.clone());
        let first = OkrKeyResult { id: Uuid::from_u128(1), ..key_result(0.0, 100.0, 50.0) };
        let second = OkrKeyResult { id: Uuid::from_u128(2), ..key_result(0.0, 100.0, 100.0) };
        state.key_results.insert(first.id, first);
        state.key_results.insert(second.id, second);
        assert_eq!(objective_progress(&state, objective.id), Some(0.75));
    }

    #[test]
    fn objective_progress_is_none_without_key_results() {
        let state = OkrState::default();
        assert_eq!(objective_progress(&state, Uuid::nil()), None);
    }

    #[test]
    fn objective_status_reports_lifecycle_states() {
        let state = OkrState::default();
        let objective = make_objective(Uuid::nil());
        let (progress, expected, status) = objective_status(&state, &objective, at(1_500));
        assert_eq!(progress, None);
        assert_eq!(expected, None);
        assert_eq!(status, "no_cycle");

        let closed = cycle(CycleStatus::Closed);
        state.cycles.insert(closed.id, closed);
        let objective = make_objective(Uuid::nil());
        let (_, expected, status) = objective_status(&state, &objective, at(1_500));
        assert_eq!(expected, Some(1.0));
        assert_eq!(status, "closed");

        let mut upcoming = cycle(CycleStatus::Active);
        upcoming.id = Uuid::from_u128(9);
        upcoming.starts_at = at(2_000);
        upcoming.ends_at = at(3_000);
        state.cycles.insert(upcoming.id, upcoming);
        let objective = make_objective(Uuid::from_u128(9));
        let (_, expected, status) = objective_status(&state, &objective, at(1_500));
        assert_eq!(expected, Some(0.0));
        assert_eq!(status, "not_started");
    }

    #[test]
    fn objective_status_bands_progress_against_elapsed_time() {
        let state = OkrState::default();
        let cycle = cycle(CycleStatus::Active);
        state.cycles.insert(cycle.id, cycle);
        let objective = make_objective(Uuid::nil());
        state.objectives.insert(objective.id, objective.clone());
        let mut result = key_result(0.0, 100.0, 50.0);
        result.objective_id = objective.id;
        state.key_results.insert(result.id, result);
        let (_, _, status) = objective_status(&state, &objective, at(1_500));
        assert_eq!(status, "on_track");

        let mut stored = state.key_results.get_mut(&Uuid::nil()).unwrap();
        stored.current_value = 30.0;
        drop(stored);
        let (_, _, status) = objective_status(&state, &objective, at(1_500));
        assert_eq!(status, "at_risk");

        let mut stored = state.key_results.get_mut(&Uuid::nil()).unwrap();
        stored.current_value = 10.0;
        drop(stored);
        let (_, _, status) = objective_status(&state, &objective, at(1_500));
        assert_eq!(status, "off_track");
    }

    #[test]
    fn refresh_rollup_moves_current_value_with_completed_tasks() {
        let state = OkrState::default();
        let mut result = key_result(0.0, 100.0, 0.0);
        result.id = Uuid::from_u128(5);
        state.key_results.insert(result.id, result);
        let first_task = Uuid::from_u128(11);
        let second_task = Uuid::from_u128(12);
        for (id, task) in [(Uuid::from_u128(21), first_task), (Uuid::from_u128(22), second_task)] {
            state.links.insert(id, OkrLink {
                id,
                key_result_id: Uuid::from_u128(5),
                link_type: "task".to_string(),
                target_id: task,
                created_at: at(1_700_000_000),
            });
        }
        state.completed_tasks.insert(first_task, at(1_700_000_100));
        assert!(refresh_rollup(&state, Uuid::from_u128(5)));
        let stored = state.key_results.get(&Uuid::from_u128(5)).unwrap();
        assert_eq!(stored.current_value, 50.0);
        drop(stored);
        assert!(!refresh_rollup(&state, Uuid::from_u128(5)));
    }

    #[test]
    fn refresh_rollup_leaves_unlinked_and_unknown_results_alone() {
        let state = OkrState::default();
        let result = key_result(0.0, 100.0, 0.0);
        state.key_results.insert(result.id, result);
        assert!(!refresh_rollup(&state, Uuid::nil()));
        assert!(!refresh_rollup(&state, Uuid::from_u128(77)));
    }

    #[test]
    fn zero_length_cycle_reports_invalid_cycle_despite_key_results() {
        let state = OkrState::default();
        let mut cycle = cycle(CycleStatus::Active);
        cycle.starts_at = at(1_000);
        cycle.ends_at = at(1_000);
        state.cycles.insert(cycle.id, cycle);
        let objective = make_objective(Uuid::nil());
        let mut result = key_result(0.0, 100.0, 50.0);
        result.objective_id = objective.id;
        state.key_results.insert(result.id, result);
        let (progress, expected, status) = objective_status(&state, &objective, at(1_500));
        assert_eq!(progress, Some(0.5));
        assert_eq!(expected, None);
        assert_eq!(status, "invalid_cycle");
    }
}
