// service — tests.
// Unit tests for the timetracker service helpers.
//
// Split from `src/modules/timetracker/service.rs`; moved verbatim, no behaviour changes.

use super::super::*;
use super::payroll::split_entry_into_weeks;
use super::*;
use actix_web::HttpRequest;
use chrono::{DateTime, Datelike, Duration, Utc};
use serde_json::{Value, json};
use std::collections::HashMap;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookContext;
use unified_core::json;
use unified_core::pagination::PaginationParams;
use unified_core::storage::StorageRegistry;
use uuid::Uuid;

fn entry(
    user: &str,
    start: DateTime<Utc>,
    duration_secs: i64,
    approval: EntryApproval,
) -> TimeEntry {
    TimeEntry {
        id: Uuid::new_v4(),
        user_id: user.to_string(),
        project_id: None,
        task_id: None,
        timer_id: None,
        description: String::new(),
        started_at: start,
        ended_at: start + chrono::Duration::seconds(duration_secs),
        duration_secs,
        billable: true,
        approval,
        approved_by: None,
        approved_at: None,
        rejection_note: None,
        reason: None,
        created_at: start,
        updated_at: start,
    }
}

#[test]
fn compute_payroll_ignores_unapproved_and_pays_zero_without_rate() {
    let state = TimeTrackerState::default();
    let monday = "2026-11-16T09:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let approved = entry("u1", monday, 7200, EntryApproval::Approved);
    state.entries.insert(approved.id, approved);
    let pending = entry("u1", monday, 7200, EntryApproval::Pending);
    state.entries.insert(pending.id, pending);
    let from = "2026-11-16T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let to = "2026-11-23T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let lines = compute_payroll(&state, from, to, None);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].user_id, "u1");
    assert_eq!(lines[0].total_hours, 2.0);
    assert_eq!(lines[0].billable_hours, 2.0);
    assert_eq!(lines[0].non_billable_hours, 0.0);
    assert_eq!(lines[0].overtime_hours, 0.0);
    assert_eq!(lines[0].hourly_rate, 0.0);
    assert_eq!(lines[0].total_pay, 0.0);
    assert_eq!(lines[0].currency, "USD");
}

#[test]
fn compute_payroll_overtime_per_iso_week() {
    let state = TimeTrackerState::default();
    // 42 hours inside one ISO week (Monday 2026-11-16): 2 hours overtime.
    let monday = "2026-11-16T09:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let tuesday = "2026-11-17T09:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let first = entry("u1", monday, 21 * 3600, EntryApproval::Approved);
    state.entries.insert(first.id, first);
    let second = entry("u1", tuesday, 21 * 3600, EntryApproval::Approved);
    state.entries.insert(second.id, second);
    // An entry outside the window is ignored entirely.
    let outside = entry(
        "u1",
        "2026-12-01T09:00:00Z".parse::<DateTime<Utc>>().unwrap(),
        10 * 3600,
        EntryApproval::Approved,
    );
    state.entries.insert(outside.id, outside);
    let from = "2026-11-16T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let to = "2026-11-23T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let lines = compute_payroll(&state, from, to, None);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].total_hours, 42.0);
    assert_eq!(lines[0].regular_hours, 40.0);
    assert_eq!(lines[0].overtime_hours, 2.0);
    // A user filter for someone else yields nothing.
    assert!(compute_payroll(&state, from, to, Some("u2")).is_empty());
}

fn rate_row(user: &str, rate: f64, effective_from: DateTime<Utc>) -> HourlyRate {
    let now = Utc::now();
    HourlyRate {
        id: Uuid::new_v4(),
        user_id: user.to_string(),
        currency: "USD".to_string(),
        rate,
        effective_from,
        effective_to: None,
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn compute_payroll_clips_entries_to_the_window() {
    let state = TimeTrackerState::default();
    // 09:00-13:00 entry, window 12:00-12:00 next day: only the
    // overlapping hour counts (before the fix the entry was
    // dropped entirely because it started before `from`).
    let entry = entry(
        "u1",
        "2026-11-16T09:00:00Z".parse().unwrap(),
        4 * 3600,
        EntryApproval::Approved,
    );
    state.entries.insert(entry.id, entry);
    let from = "2026-11-16T12:00:00Z".parse().unwrap();
    let to = "2026-11-17T12:00:00Z".parse().unwrap();
    let lines = compute_payroll(&state, from, to, None);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].total_hours, 1.0);
    // An entry ending before the window contributes nothing.
    let state = TimeTrackerState::default();
    let early = entry(
        "u1",
        "2026-11-16T08:00:00Z".parse().unwrap(),
        3600,
        EntryApproval::Approved,
    );
    state.entries.insert(early.id, early);
    assert!(compute_payroll(&state, from, to, None).is_empty());
}

#[test]
fn split_entry_into_weeks_splits_at_monday_midnight() {
    let start = "2026-11-22T22:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let end = "2026-11-23T02:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let segments = split_entry_into_weeks(start, end, 4 * 3600);
    assert_eq!(segments.len(), 2);
    // Sunday 2026-11-22 is ISO week 47, Monday is week 48.
    assert_eq!(segments[0], ((2026, 47), 2 * 3600));
    assert_eq!(segments[1], ((2026, 48), 2 * 3600));
    assert_eq!(segments.iter().map(|(_, secs)| secs).sum::<i64>(), 4 * 3600);
}

#[test]
fn compute_payroll_overtime_uses_the_week_each_hour_falls_in() {
    let state = TimeTrackerState::default();
    // 39 hours in ISO week 47, then a 4-hour entry straddling
    // the week boundary (2 hours week 47, 2 hours week 48).
    let first = entry(
        "u1",
        "2026-11-16T09:00:00Z".parse().unwrap(),
        21 * 3600,
        EntryApproval::Approved,
    );
    state.entries.insert(first.id, first);
    let second = entry(
        "u1",
        "2026-11-17T09:00:00Z".parse().unwrap(),
        18 * 3600,
        EntryApproval::Approved,
    );
    state.entries.insert(second.id, second);
    let straddling = entry(
        "u1",
        "2026-11-22T22:00:00Z".parse().unwrap(),
        4 * 3600,
        EntryApproval::Approved,
    );
    state.entries.insert(straddling.id, straddling);
    let from = "2026-11-16T00:00:00Z".parse().unwrap();
    let to = "2026-11-23T12:00:00Z".parse().unwrap();
    let lines = compute_payroll(&state, from, to, None);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].total_hours, 43.0);
    // Week 47 holds 41 hours (1 overtime), week 48 holds 2.
    // Bucketing the whole straddling entry into week 47 would
    // have reported 3 overtime hours instead.
    assert_eq!(lines[0].overtime_hours, 1.0);
    assert_eq!(lines[0].regular_hours, 42.0);
}

#[test]
fn compute_payroll_prices_each_entry_at_its_own_started_at_rate() {
    let state = TimeTrackerState::default();
    state.rates.insert(
        Uuid::new_v4(),
        rate_row("u1", 10.0, "2026-11-16T00:00:00Z".parse().unwrap()),
    );
    state.rates.insert(
        Uuid::new_v4(),
        rate_row("u1", 20.0, "2026-11-18T00:00:00Z".parse().unwrap()),
    );
    let before = entry(
        "u1",
        "2026-11-17T09:00:00Z".parse().unwrap(),
        2 * 3600,
        EntryApproval::Approved,
    );
    state.entries.insert(before.id, before);
    let after = entry(
        "u1",
        "2026-11-19T09:00:00Z".parse().unwrap(),
        2 * 3600,
        EntryApproval::Approved,
    );
    state.entries.insert(after.id, after);
    let from = "2026-11-16T00:00:00Z".parse().unwrap();
    let to = "2026-11-23T00:00:00Z".parse().unwrap();
    let lines = compute_payroll(&state, from, to, None);
    assert_eq!(lines.len(), 1);
    // 2h at $10 plus 2h at $20. Pricing everything at the
    // period-end rate ($20) would have reported $80.
    assert_eq!(lines[0].total_pay, 60.0);
    assert_eq!(lines[0].regular_pay, 60.0);
    assert_eq!(lines[0].hourly_rate, 15.0);
    assert_eq!(lines[0].currency, "USD");
}

#[test]
fn parse_window_rejects_windows_over_366_days_of_real_duration() {
    use unified_core::pagination::PaginationParams;
    use std::collections::HashMap;
    let params_with = |from: &str, to: &str| PaginationParams {
        page: None,
        per_page: None,
        sort: None,
        order: None,
        search: None,
        filters: HashMap::from([
            ("from".to_string(), from.to_string()),
            ("to".to_string(), to.to_string()),
        ]),
    };
    // 366 days plus 23 hours truncates to 366 whole days but
    // exceeds the cap.
    assert!(parse_window(&params_with(
        "2026-01-01T00:00:00Z",
        "2027-01-02T23:00:00Z"
    ))
    .is_err());
    // Exactly 366 days passes.
    assert!(parse_window(&params_with(
        "2026-01-01T00:00:00Z",
        "2027-01-02T00:00:00Z"
    ))
    .is_ok());
    assert!(parse_window(&params_with(
        "2026-01-02T00:00:00Z",
        "2026-01-01T00:00:00Z"
    ))
    .is_err());
}

fn invite_for(email: &str, team_id: Option<Uuid>) -> StaffInvite {
    let now = Utc::now();
    StaffInvite {
        id: Uuid::new_v4(),
        company_id: Uuid::new_v4(),
        email: email.to_string(),
        role: TrackerRole::Member,
        team_id,
        invited_by: "admin".to_string(),
        token_hash: "hash".to_string(),
        status: "pending".to_string(),
        expires_at: now + chrono::Duration::days(7),
        accepted_at: None,
        accepted_user_id: None,
        created_at: now,
        updated_at: now,
    }
}

fn staff_record(user_id: &str, company_id: Uuid) -> StaffSettings {
    let now = Utc::now();
    StaffSettings {
        user_id: user_id.to_string(),
        company_id,
        role: TrackerRole::Member,
        manager_id: None,
        screenshots_enabled: None,
        blur_screenshots: None,
        idle_threshold_secs: None,
        activity_level: None,
        timezone: None,
        working_hours_start: None,
        working_hours_end: None,
        created_at: now,
        updated_at: now,
    }
}

fn run_user_created(state: &TimeTrackerState, user_id: &str, email: &str) -> usize {
    let mut ctx = HookContext::new("user.created", json!({ "id": user_id, "email": email }));
    on_user_created(state.clone(), &mut ctx);
    ctx.results
        .first()
        .and_then(|result| result.get("accepted"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as usize
}

#[test]
fn user_created_accepts_invite_and_creates_staff_and_membership() {
    let state = TimeTrackerState::default();
    let team_id = Uuid::new_v4();
    let invite = invite_for("a@example.com", Some(team_id));
    state.invites.insert(invite.id, invite);
    assert_eq!(run_user_created(&state, "u1", "a@example.com"), 1);
    assert!(state.staff.contains_key("u1"));
    assert!(state
        .team_members
        .contains_key(&team_member_key(team_id, "u1")));
}

#[test]
fn user_created_adds_team_membership_even_when_staff_already_exists() {
    let state = TimeTrackerState::default();
    let team_id = Uuid::new_v4();
    let invite = invite_for("a@example.com", Some(team_id));
    let company_id = invite.company_id;
    state.invites.insert(invite.id, invite);
    state
        .staff
        .insert("u1".to_string(), staff_record("u1", company_id));
    assert_eq!(run_user_created(&state, "u1", "a@example.com"), 1);
    assert!(state
        .team_members
        .contains_key(&team_member_key(team_id, "u1")));
    // The pre-existing staff record was not duplicated.
    assert_eq!(state.staff.len(), 1);
}

#[test]
fn user_created_counts_nothing_when_the_invite_produces_nothing() {
    let state = TimeTrackerState::default();
    let invite = invite_for("a@example.com", None);
    let company_id = invite.company_id;
    state.invites.insert(invite.id, invite);
    state
        .staff
        .insert("u1".to_string(), staff_record("u1", company_id));
    assert_eq!(run_user_created(&state, "u1", "a@example.com"), 0);
    // The invite itself is still marked accepted.
    assert_eq!(
        state
            .invites
            .iter()
            .next()
            .map(|entry| entry.status.clone()),
        Some("accepted".to_string())
    );
}

fn shot_for(user: &str, days_old: i64) -> ScreenshotRecord {
    let now = Utc::now();
    ScreenshotRecord {
        id: Uuid::new_v4(),
        user_id: user.to_string(),
        timer_id: None,
        project_id: None,
        task_id: None,
        storage_target: "timetracker-screenshots".to_string(),
        storage_key: format!("timetracker/{user}/shot.png"),
        content_sha256: "abc".to_string(),
        content_type: "image/png".to_string(),
        size_bytes: 10,
        blurred: false,
        captured_at: now - chrono::Duration::days(days_old),
        created_at: now,
    }
}

struct FailingProvider {
    fail_delete: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl unified_core::storage::StorageProvider for FailingProvider {
    fn kind(&self) -> unified_core::storage::StorageKind {
        unified_core::storage::StorageKind::Local
    }
    async fn upload(&self, _key: &str, _data: Vec<u8>) -> Result<String, UnifiedError> {
        Ok("key".to_string())
    }
    async fn download(&self, _key: &str) -> Result<Vec<u8>, UnifiedError> {
        Ok(Vec::new())
    }
    async fn delete(&self, _key: &str) -> Result<(), UnifiedError> {
        if self.fail_delete.load(std::sync::atomic::Ordering::SeqCst) {
            Err(UnifiedError::internal("storage delete failed"))
        } else {
            Ok(())
        }
    }
    async fn signed_url(&self, _key: &str, _expires_secs: u64) -> Result<String, UnifiedError> {
        Ok("https://example.invalid/x".to_string())
    }
    async fn test_connection(&self) -> Result<(), UnifiedError> {
        Ok(())
    }
}

fn run_purge(
    state: &TimeTrackerState,
    registry: &unified_core::storage::StorageRegistry,
) -> (u64, u64) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("purge test runtime")
        .block_on(purge_expired_screenshots(state, registry))
}

#[test]
fn purge_keeps_metadata_when_the_object_delete_fails() {
    let state = TimeTrackerState::default();
    let shot = shot_for("u1", 100);
    state.screenshots.insert(shot.id, shot.clone());
    let registry = unified_core::storage::StorageRegistry::new();
    let provider = std::sync::Arc::new(FailingProvider {
        fail_delete: std::sync::atomic::AtomicBool::new(true),
    });
    registry.register("timetracker-screenshots".to_string(), provider.clone());
    let (removed, failed) = run_purge(&state, &registry);
    assert_eq!((removed, failed), (0, 1));
    // Metadata is kept so a later purge can retry the delete.
    assert!(state.screenshots.contains_key(&shot.id));
    // The retry succeeds and removes the metadata.
    provider
        .fail_delete
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let (removed, failed) = run_purge(&state, &registry);
    assert_eq!((removed, failed), (1, 0));
    assert!(!state.screenshots.contains_key(&shot.id));
}

#[test]
fn purge_keeps_metadata_when_no_provider_is_registered() {
    let state = TimeTrackerState::default();
    let shot = shot_for("u1", 100);
    state.screenshots.insert(shot.id, shot.clone());
    let registry = unified_core::storage::StorageRegistry::new();
    let (removed, failed) = run_purge(&state, &registry);
    assert_eq!((removed, failed), (0, 1));
    assert!(state.screenshots.contains_key(&shot.id));
}

#[test]
fn emit_time_logged_void_marks_a_compensating_delete() {
    use unified_core::hooks::HookBus;
    let state = TimeTrackerState::default();
    let bus = std::sync::Arc::new(HookBus::new());
    let seen: std::sync::Arc<std::sync::Mutex<Vec<Value>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = seen.clone();
    bus.register(
        events::TIME_LOGGED,
        "test",
        std::sync::Arc::new(move |ctx: &mut HookContext| {
            captured
                .lock()
                .expect("capture lock poisoned")
                .push(ctx.data.clone());
        }),
    );
    *state.hooks.write().expect("hooks lock poisoned") = Some(bus);
    let start = "2026-11-16T09:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let entry = entry("u1", start, 3600, EntryApproval::Approved);
    emit_time_logged(&state, &entry, "manual", false);
    emit_time_logged(&state, &entry, "manual_delete", true);
    let payloads = seen.lock().expect("capture lock poisoned");
    assert_eq!(payloads.len(), 2);
    assert_eq!(
        payloads[0].get("void").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        payloads[0].get("amount").and_then(|v| v.as_f64()),
        Some(0.0)
    );
    assert_eq!(
        payloads[1].get("void").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        payloads[1].get("amount").and_then(|v| v.as_f64()),
        Some(0.0)
    );
    assert_eq!(
        payloads[1].get("source").and_then(|v| v.as_str()),
        Some("manual_delete")
    );
    assert_eq!(
        payloads[1].get("duration_secs").and_then(|v| v.as_i64()),
        Some(3600)
    );
}
