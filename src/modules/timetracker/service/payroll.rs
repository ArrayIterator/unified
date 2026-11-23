// service — payroll.
// Week helpers, payroll computation, and report rows.
//
// Split from `src/modules/timetracker/service.rs`; moved verbatim, no behaviour changes.

use super::super::*;
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

/// The user's hourly rate in force at `at`: the latest
/// `effective_from` at or before `at` whose `effective_to` has
/// not passed. Falls back to the buyer's default rate when the
/// user has no rate row. Buyer default rates carry no currency
/// field (`BuyerSettings` has none), so the fallback is labelled
/// with the global default currency — the amount is the buyer's,
/// the currency label is the product default, and callers that
/// need a per-buyer currency must add one to `BuyerSettings`
/// first.
pub(crate) fn active_rate_for(
    state: &TimeTrackerState,
    user_id: &str,
    at: DateTime<Utc>,
) -> Option<HourlyRate> {
    let mut rates: Vec<HourlyRate> = state
        .rates
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rate| {
            rate.user_id == user_id
                && rate.effective_from <= at
                && rate.effective_to.map(|to| to >= at).unwrap_or(true)
        })
        .collect();
    rates.sort_by(|a, b| b.effective_from.cmp(&a.effective_from));
    if let Some(rate) = rates.into_iter().next() {
        return Some(rate);
    }
    let resolved = state.resolve_settings_for_user(user_id);
    let buyer_rate = resolved
        .company_id
        .and_then(|company_id| state.buyer_settings_for(company_id).default_hourly_rate);
    buyer_rate.map(|rate| HourlyRate {
        id: Uuid::nil(),
        user_id: user_id.to_string(),
        currency: state.global_settings().default_currency,
        rate,
        effective_from: at,
        effective_to: None,
        created_at: at,
        updated_at: at,
    })
}

/// Midnight (00:00 UTC) of the Monday after `at`. When `at` is
/// exactly a Monday midnight that is the following Monday, so a
/// span starting on a week boundary belongs to the new week.
fn next_monday_midnight(at: DateTime<Utc>) -> DateTime<Utc> {
    let date = at.date_naive();
    let days_to_monday = 7 - date.weekday().num_days_from_monday() as i64;
    (date + Duration::days(days_to_monday))
        .and_hms_opt(0, 0, 0)
        .expect("midnight is always a valid time")
        .and_utc()
}

/// Split one clipped entry span into per-ISO-week segments.
/// `total_secs` is the entry's payable seconds for the whole span
/// (already clipped to the payroll window); each segment gets its
/// proportional share of it, and the last segment absorbs rounding
/// drift so the segments always sum to `total_secs`.
pub(crate) fn split_entry_into_weeks(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    total_secs: i64,
) -> Vec<((i32, u32), i64)> {
    let total_wall = (end - start).num_seconds().max(1);
    let mut segments = Vec::new();
    let mut cur = start;
    let mut remaining = total_secs;
    while cur < end {
        let boundary = next_monday_midnight(cur);
        let seg_end = end.min(boundary);
        let seg_wall = (seg_end - cur).num_seconds();
        let is_last = seg_end >= end;
        let secs = if is_last {
            remaining.max(0)
        } else {
            ((total_secs as f64) * (seg_wall as f64 / total_wall as f64)).round() as i64
        };
        if secs > 0 {
            let week = cur.iso_week();
            segments.push(((week.year(), week.week()), secs));
        }
        remaining -= secs;
        cur = seg_end;
    }
    segments
}

/// Compute payroll lines for approved entries overlapping
/// `[from, to]`. Only approved entries are payable; pending and
/// rejected entries are ignored.
///
/// Window: an entry is clipped to the window — the part before
/// `from` or after `to` is not counted, and an entry starting
/// before `from` still contributes its overlapping part. The
/// clipped seconds are the entry's `duration_secs` scaled by the
/// overlapping share of its wall span (timer entries are net of
/// pauses, so the wall span is longer than the duration).
///
/// Overtime is per user per ISO week: hours beyond
/// `overtime_hours_per_week` pay at `overtime_multiplier`.
/// Entries spanning a week boundary are split at Monday 00:00
/// UTC, so each week's hours land in their own week instead of
/// being bucketed wholly into the week of `started_at`.
///
/// Rates: every entry is priced at the rate in force at its own
/// `started_at` (falling back per entry to the buyer's default
/// rate, then 0), so a mid-period rate change does not re-price
/// earlier hours. The line's `hourly_rate` is the hours-weighted
/// average of its entries' rates, and its `currency` is the
/// currency of the earliest entry's rate (the buyer's default-rate
/// fallback is labelled with the global default currency, because
/// buyer rates carry no currency field). A user without any rate
/// still gets a line at 0 so the gap is visible instead of
/// silently skipped. Money stays `f64` rounded to 4 decimals,
/// per the module / billing NUMERIC(19,4) convention.
pub(crate) fn compute_payroll(
    state: &TimeTrackerState,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    user_filter: Option<&str>,
) -> Vec<PayrollLine> {
    let global = state.global_settings();
    /// One payable piece of one entry inside one ISO week.
    struct Segment {
        week: (i32, u32),
        secs: i64,
        billable: bool,
        rate: f64,
        currency: String,
    }
    let mut approved: Vec<TimeEntry> = state
        .entries
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| {
            record.approval == EntryApproval::Approved
                && record.started_at < to
                && record.ended_at > from
                && user_filter.map(|wanted| record.user_id == wanted).unwrap_or(true)
        })
        .collect();
    approved.sort_by(|a, b| a.started_at.cmp(&b.started_at));
    let mut per_user: HashMap<String, Vec<Segment>> = HashMap::new();
    for entry in approved {
        let wall_total = (entry.ended_at - entry.started_at).num_seconds();
        if wall_total <= 0 {
            continue;
        }
        let clip_start = entry.started_at.max(from);
        let clip_end = entry.ended_at.min(to);
        if clip_end <= clip_start {
            continue;
        }
        let clip_wall = (clip_end - clip_start).num_seconds();
        let clipped_secs =
            ((entry.duration_secs as f64) * (clip_wall as f64 / wall_total as f64)).round() as i64;
        if clipped_secs <= 0 {
            continue;
        }
        let rate = active_rate_for(state, &entry.user_id, entry.started_at);
        let hourly = rate.as_ref().map(|r| r.rate).unwrap_or(0.0);
        let currency = rate
            .as_ref()
            .map(|r| r.currency.clone())
            .unwrap_or_else(|| global.default_currency.clone());
        for (week, secs) in split_entry_into_weeks(clip_start, clip_end, clipped_secs) {
            per_user.entry(entry.user_id.clone()).or_default().push(Segment {
                week,
                secs,
                billable: entry.billable,
                rate: hourly,
                currency: currency.clone(),
            });
        }
    }
    let mut lines: Vec<PayrollLine> = Vec::new();
    for (user_id, mut segments) in per_user {
        // Stable sort by week keeps the chronological order within
        // each week, which the overtime walk below relies on.
        segments.sort_by_key(|segment| segment.week);
        let total_secs: i64 = segments.iter().map(|segment| segment.secs).sum();
        let billable_secs: i64 = segments
            .iter()
            .filter(|segment| segment.billable)
            .map(|segment| segment.secs)
            .sum();
        let threshold_secs = (global.overtime_hours_per_week * 3600.0).round() as i64;
        let mut week_seen: HashMap<(i32, u32), i64> = HashMap::new();
        let mut regular_secs = 0i64;
        let mut overtime_secs = 0i64;
        let mut regular_pay = 0.0f64;
        let mut overtime_pay = 0.0f64;
        let mut rate_weighted = 0.0f64;
        for segment in &segments {
            let seen = week_seen.entry(segment.week).or_default();
            let overtime_part = ((*seen + segment.secs - threshold_secs).max(0)
                - (*seen - threshold_secs).max(0))
            .max(0);
            let regular_part = segment.secs - overtime_part;
            *seen += segment.secs;
            regular_secs += regular_part;
            overtime_secs += overtime_part;
            regular_pay += regular_part as f64 / 3600.0 * segment.rate;
            overtime_pay += overtime_part as f64 / 3600.0 * segment.rate * global.overtime_multiplier;
            rate_weighted += segment.rate * segment.secs as f64;
        }
        let regular_hours = round4(regular_secs as f64 / 3600.0);
        let overtime_hours = round4(overtime_secs as f64 / 3600.0);
        let regular_pay = round4(regular_pay);
        let overtime_pay = round4(overtime_pay);
        lines.push(PayrollLine {
            user_id: user_id.clone(),
            currency: segments
                .first()
                .map(|segment| segment.currency.clone())
                .unwrap_or_else(|| global.default_currency.clone()),
            total_hours: round4(total_secs as f64 / 3600.0),
            billable_hours: round4(billable_secs as f64 / 3600.0),
            non_billable_hours: round4((total_secs - billable_secs) as f64 / 3600.0),
            regular_hours,
            overtime_hours,
            hourly_rate: if total_secs > 0 {
                round4(rate_weighted / total_secs as f64)
            } else {
                0.0
            },
            regular_pay,
            overtime_pay,
            total_pay: round4(regular_pay + overtime_pay),
        });
    }
    lines.sort_by(|a, b| a.user_id.cmp(&b.user_id));
    lines
}

/// Aggregate entries into daily report rows, optionally filtered
/// by user and project. Activity averages come from the samples in
/// the same window.
pub(crate) fn report_rows(
    state: &TimeTrackerState,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    user_filter: Option<&str>,
    project_filter: Option<Uuid>,
) -> Vec<ReportRow> {
    let mut grouped: HashMap<(String, Option<Uuid>, String), (i64, i64, usize)> = HashMap::new();
    for entry in state.entries.iter() {
        let record = entry.value();
        if record.started_at < from || record.started_at > to {
            continue;
        }
        if let Some(wanted) = user_filter {
            if record.user_id != wanted {
                continue;
            }
        }
        if let Some(wanted) = project_filter {
            if record.project_id != Some(wanted) {
                continue;
            }
        }
        let date = record.started_at.format("%Y-%m-%d").to_string();
        let slot = grouped
            .entry((record.user_id.clone(), record.project_id, date))
            .or_default();
        slot.0 += record.duration_secs;
        if record.billable {
            slot.1 += record.duration_secs;
        }
        slot.2 += 1;
    }
    let mut rows: Vec<ReportRow> = Vec::new();
    for ((user_id, project_id, date), (total_secs, billable_secs, count)) in grouped {
        let day_start = DateTime::parse_from_rfc3339(&format!("{date}T00:00:00+00:00"))
            .map(|parsed| parsed.with_timezone(&Utc))
            .unwrap_or(from);
        let day_end = day_start + Duration::days(1);
        let samples: Vec<i64> = state
            .activity
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|sample| {
                sample.user_id == user_id
                    && sample.interval_start >= day_start
                    && sample.interval_start < day_end
                    && project_filter.map(|wanted| sample.project_id == Some(wanted)).unwrap_or(true)
            })
            .map(|sample| sample.activity_score)
            .collect();
        let avg_activity = if samples.is_empty() {
            None
        } else {
            Some(samples.iter().sum::<i64>() as f64 / samples.len() as f64)
        };
        rows.push(ReportRow {
            user_id,
            project_id,
            date,
            total_secs,
            billable_secs,
            non_billable_secs: total_secs - billable_secs,
            entry_count: count,
            avg_activity,
        });
    }
    rows.sort_by(|a, b| (a.date.clone(), a.user_id.clone()).cmp(&(b.date.clone(), b.user_id.clone())));
    rows
}

/// Build one staff member's activity overview.
pub(crate) fn staff_overview(state: &TimeTrackerState, staff: &StaffSettings) -> StaffOverview {
    let now = Utc::now();
    let today_start = now.date_naive().and_hms_opt(0, 0, 0).map(|naive| naive.and_utc()).unwrap_or(now);
    let week_start = today_start - Duration::days(7);
    let today_secs: i64 = state
        .entries
        .iter()
        .filter(|entry| {
            entry.value().user_id == staff.user_id && entry.value().started_at >= today_start
        })
        .map(|entry| entry.value().duration_secs)
        .sum();
    let week_secs: i64 = state
        .entries
        .iter()
        .filter(|entry| {
            entry.value().user_id == staff.user_id && entry.value().started_at >= week_start
        })
        .map(|entry| entry.value().duration_secs)
        .sum();
    let today_samples: Vec<ActivitySample> = state
        .activity
        .iter()
        .filter(|entry| {
            entry.value().user_id == staff.user_id && entry.value().interval_start >= today_start
        })
        .map(|entry| entry.value().clone())
        .collect();
    let avg_activity_today = if today_samples.is_empty() {
        None
    } else {
        Some(
            today_samples.iter().map(|sample| sample.activity_score).sum::<i64>() as f64
                / today_samples.len() as f64,
        )
    };
    let last_activity_at = today_samples
        .iter()
        .map(|sample| sample.interval_end)
        .max();
    let resolved = state.resolve_settings_for_user(&staff.user_id);
    StaffOverview {
        user_id: staff.user_id.clone(),
        company_id: staff.company_id,
        role: staff.role,
        manager_id: staff.manager_id.clone(),
        team_ids: team_ids_for_user(state, &staff.user_id),
        active_timer_id: active_timer_for(state, &staff.user_id).map(|timer| timer.id),
        today_secs,
        week_secs,
        avg_activity_today,
        last_activity_at,
        screenshots_enabled: resolved.screenshots_enabled
            && state.user_settings_for(&staff.user_id).screenshots_enabled,
    }
}
