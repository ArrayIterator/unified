//! calendar module - Item aggregation, non-working days, conflicts and ranges.

use unified_core::context::AppContext;
use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};
use serde_json::{Value, json};
use std::collections::HashSet;

use super::constants::EVENT_HOLIDAY_LOOKUP;
use super::constants::EVENT_ITEMS_REQUESTED;
use super::constants::MAX_HOLIDAY_LOOKUP_DAYS;
use super::constants::MAX_ITEMS_PER_VIEW;
use super::helpers::Caller;
use super::state::CalendarState;
use super::types::CalendarItem;
use super::types::CalendarSettings;
use super::types::ConflictHint;
use super::types::NonWorkingDay;

// Aggregation.

/// Effective settings for a caller: company defaults overlaid
/// with the caller's own overrides (a set user value wins).
/// Lookups are tenant-scoped: keys are
/// `"{tenant_id}:{company_id}"` and `"{tenant_id}:{user_id}"`, so
/// settings can never leak across tenants.
pub(crate) fn effective_settings(state: &CalendarState, caller: &Caller) -> CalendarSettings {
    let mut settings = CalendarSettings::default();
    if let Some(company_id) = &caller.company_id {
        let key = format!("{}:{company_id}", caller.tenant_id);
        if let Some(company) = state.company_settings.get(&key) {
            settings = company.clone();
        }
    }
    let user_key = format!("{}:{}", caller.tenant_id, caller.actor);
    if let Some(user) = state.user_settings.get(&user_key) {
        let user = user.clone();
        settings.weekend_days = user.weekend_days;
        settings.working_hours_start = user.working_hours_start;
        settings.working_hours_end = user.working_hours_end;
        settings.default_view = user.default_view;
        settings.timezone = user.timezone;
        settings.holiday_country = user.holiday_country;
        if !user.create_link_templates.is_empty() {
            settings.create_link_templates = user.create_link_templates;
        }
    }
    settings
}

/// Modules the caller has hidden through source toggles.
/// Tenant-scoped: only toggles whose tenant_id matches the
/// caller's tenant apply.
pub(crate) fn hidden_modules(state: &CalendarState, caller: &Caller) -> HashSet<String> {
    let mut hidden = HashSet::new();
    for entry in state.sources.iter() {
        let source = entry.value();
        if source.tenant_id == caller.tenant_id && source.user_id == caller.actor && !source.visible
        {
            hidden.insert(source.module.clone());
        }
    }
    hidden
}

/// Overlap test for a view window: an item belongs to the window
/// when its interval intersects `[start, end]` - that is, it
/// starts before the window ends and its effective end falls after
/// the window starts. An event that starts before the window but
/// ends inside it (or spans the whole window) is kept; an item
/// that merely touches a boundary (ends exactly at `start`, or
/// starts exactly at `end`) does not overlap and is excluded.
pub(crate) fn item_overlaps_window(
    item: &CalendarItem,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> bool {
    item.start < end && item.effective_end() > start
}

/// Tenant trust for one item: an item scoped to another tenant is
/// excluded, and a tenant-less item is trusted only when it is
/// explicitly marked system/global (`is_system`). The company rule
/// is the same shape - an item scoped to a company the caller has
/// no company for is excluded - so tenant-less items are never
/// treated as more trusted than company-scoped ones.
pub(crate) fn item_passes_tenant_filter(item: &CalendarItem, tenant_id: &str) -> bool {
    match &item.tenant_id {
        Some(item_tenant) => item_tenant == tenant_id,
        None => item.is_system,
    }
}

/// Ask every module for items overlapping [start, end] and merge
/// the answers. Returns the visible items (sorted by start,
/// de-duplicated by (module, id)) and the number of contributed
/// items skipped as malformed.
pub(crate) fn aggregate_items(
    ctx: &AppContext,
    state: &CalendarState,
    caller: &Caller,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    module_filter: Option<&[String]>,
) -> (Vec<CalendarItem>, usize) {
    let mut request = json!({
        "tenant_id": caller.tenant_id,
        "company_id": caller.company_id,
        "user_id": caller.actor,
        "start": start.to_rfc3339(),
        "end": end.to_rfc3339(),
    });
    if let Some(modules) = module_filter {
        request["modules"] = json!(modules);
    }
    let answer = ctx.hooks.emit(EVENT_ITEMS_REQUESTED, request);
    let hidden = hidden_modules(state, caller);
    let mut items: Vec<CalendarItem> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut skipped = 0usize;
    for result in &answer.results {
        let candidates: Vec<Value> = match result {
            Value::Array(values) => values.clone(),
            Value::Object(map) => match map.get("items") {
                Some(Value::Array(values)) => values.clone(),
                _ => vec![result.clone()],
            },
            _ => {
                skipped += 1;
                continue;
            }
        };
        for candidate in candidates {
            let item: CalendarItem = match serde_json::from_value(candidate) {
                Ok(item) => item,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            if item.title.trim().is_empty() || item.module.trim().is_empty() {
                skipped += 1;
                continue;
            }
            if !item_overlaps_window(&item, start, end) {
                continue;
            }
            if let Some(modules) = module_filter {
                if !modules.contains(&item.module) {
                    continue;
                }
            }
            if hidden.contains(&item.module) {
                continue;
            }
            // Visibility, enforced server-side: private items reach
            // only their owner.
            if item.visibility == "private" {
                match &item.owner_user_id {
                    Some(owner) if owner == &caller.actor => {}
                    _ => continue,
                }
            }
            // Tenancy, enforced server-side by exclusion: an item
            // scoped to another company never leaks (no 403/404
            // signal, it is simply absent). Same rule for tenant:
            // an item carrying a tenant_id different from the
            // caller's tenant is excluded silently, and a
            // tenant-less item is trusted only when it is
            // explicitly marked system/global - an unmarked
            // tenant-less item is excluded the same way.
            if !item_passes_tenant_filter(&item, &caller.tenant_id) {
                continue;
            }
            match (&item.company_id, &caller.company_id) {
                (Some(item_company), Some(caller_company)) => {
                    if item_company != caller_company {
                        continue;
                    }
                }
                (Some(_), None) => continue,
                _ => {}
            }
            if item.end.is_some_and(|item_end| item_end < item.start) {
                skipped += 1;
                continue;
            }
            let key = (item.module.clone(), item.id.clone());
            if !seen.insert(key) {
                continue;
            }
            items.push(item);
        }
    }
    items.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| left.module.cmp(&right.module))
            .then_with(|| left.id.cmp(&right.id))
    });
    if items.len() > MAX_ITEMS_PER_VIEW {
        items.truncate(MAX_ITEMS_PER_VIEW);
    }
    (items, skipped)
}

/// Non-working days inside [start_date, end_date]: configured
/// weekend days plus holidays answered by the `holidays.is_holiday`
/// hook. When no handler is registered for the hook (the holidays
/// module may not exist yet) or no holiday country is configured,
/// only weekends apply - never an error.
pub(crate) fn non_working_days(
    ctx: &AppContext,
    tenant_id: &str,
    settings: &CalendarSettings,
    start_date: NaiveDate,
    end_date: NaiveDate,
) -> Vec<NonWorkingDay> {
    let weekend: HashSet<u8> = settings.weekend_days.iter().copied().collect();
    let mut days: Vec<NonWorkingDay> = Vec::new();
    let mut date = start_date;
    let mut walked: i64 = 0;
    while date <= end_date && walked < MAX_HOLIDAY_LOOKUP_DAYS {
        walked += 1;
        let iso_weekday = date.weekday().number_from_monday() as u8;
        if weekend.contains(&iso_weekday) {
            days.push(NonWorkingDay {
                date: date.to_string(),
                kind: "weekend".to_string(),
                name: None,
                source: None,
            });
        } else if !settings.holiday_country.trim().is_empty() {
            let answer = ctx.hooks.emit(
                EVENT_HOLIDAY_LOOKUP,
                json!({
                    "date": date.to_string(),
                    "country": settings.holiday_country.trim().to_uppercase(),
                    // Required by the holidays module: without a tenant id
                    // its handler answers nothing (fail closed).
                    "tenant_id": tenant_id,
                }),
            );
            for result in &answer.results {
                let is_holiday = result
                    .get("is_holiday")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if is_holiday {
                    days.push(NonWorkingDay {
                        date: date.to_string(),
                        kind: "holiday".to_string(),
                        name: result
                            .get("name")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        source: result
                            .get("source")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    });
                    break;
                }
            }
        }
        date = date + chrono::Duration::days(1);
    }
    days
}

/// Overlap hints for one user's items: pairs whose intervals
/// intersect. Hints only - never a hard block.
pub(crate) fn conflict_hints(items: &[CalendarItem]) -> Vec<ConflictHint> {
    let mut hints: Vec<ConflictHint> = Vec::new();
    for left_index in 0..items.len() {
        let left = &items[left_index];
        let Some(left_owner) = &left.owner_user_id else {
            continue;
        };
        for right in items.iter().skip(left_index + 1) {
            if right.owner_user_id.as_deref() != Some(left_owner.as_str()) {
                continue;
            }
            let left_end = left.effective_end();
            let right_end = right.effective_end();
            if left.start < right_end && right.start < left_end {
                let overlap_start = left.start.max(right.start);
                let overlap_end = left_end.min(right_end);
                hints.push(ConflictHint {
                    first_module: left.module.clone(),
                    first_id: left.id.clone(),
                    second_module: right.module.clone(),
                    second_id: right.id.clone(),
                    overlap_start: overlap_start.to_rfc3339(),
                    overlap_end: overlap_end.to_rfc3339(),
                });
            }
        }
    }
    hints
}

/// Default view range for a view kind, in UTC, from "now".
pub(crate) fn default_range(view: &str, now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let day_start = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight is valid");
    let day_start = Utc.from_utc_datetime(&day_start);
    match view {
        "day" => (day_start, day_start + chrono::Duration::days(1)),
        "week" => {
            let monday_offset = now.weekday().num_days_from_monday() as i64;
            let monday = day_start - chrono::Duration::days(monday_offset);
            (monday, monday + chrono::Duration::days(7))
        }
        "agenda" => (day_start, day_start + chrono::Duration::days(14)),
        _ => {
            let first = now
                .date_naive()
                .with_day(1)
                .expect("first of month is valid");
            let first =
                Utc.from_utc_datetime(&first.and_hms_opt(0, 0, 0).expect("midnight is valid"));
            let next_month = if now.month() == 12 {
                NaiveDate::from_ymd_opt(now.year() + 1, 1, 1).expect("valid date")
            } else {
                NaiveDate::from_ymd_opt(now.year(), now.month() + 1, 1).expect("valid date")
            };
            let next_month =
                Utc.from_utc_datetime(&next_month.and_hms_opt(0, 0, 0).expect("midnight is valid"));
            (first, next_month)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::conflict_hints;
    use super::default_range;
    use super::item_overlaps_window;
    use super::item_passes_tenant_filter;
    use crate::modules::calendar::CalendarItem;
    use chrono::{DateTime, Duration, TimeZone, Utc};

    fn item(id: &str, owner: Option<&str>, start_hour: u32, end_hour: u32) -> CalendarItem {
        let day = Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();
        CalendarItem {
            id: id.to_string(),
            module: "appointments".to_string(),
            title: id.to_string(),
            start: day + Duration::hours(i64::from(start_hour)),
            end: Some(day + Duration::hours(i64::from(end_hour))),
            all_day: false,
            color: None,
            label: None,
            deep_link: None,
            visibility: "shared".to_string(),
            owner_user_id: owner.map(str::to_string),
            company_id: None,
            tenant_id: None,
            is_system: false,
            start_local: None,
            end_local: None,
        }
    }

    fn window() -> (DateTime<Utc>, DateTime<Utc>) {
        let day = Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();
        (
            day + Duration::hours(10),
            day + Duration::hours(12),
        )
    }

    #[test]
    fn spanning_item_starting_before_window_is_kept() {
        let (from, to) = window();
        assert!(item_overlaps_window(&item("a", None, 9, 11), from, to));
    }

    #[test]
    fn spanning_item_covering_whole_window_is_kept() {
        let (from, to) = window();
        assert!(item_overlaps_window(&item("a", None, 9, 13), from, to));
    }

    #[test]
    fn item_inside_window_is_kept() {
        let (from, to) = window();
        assert!(item_overlaps_window(&item("a", None, 10, 11), from, to));
    }

    #[test]
    fn item_ending_before_window_is_dropped() {
        let (from, to) = window();
        assert!(!item_overlaps_window(&item("a", None, 8, 9), from, to));
    }

    #[test]
    fn item_starting_after_window_is_dropped() {
        let (from, to) = window();
        assert!(!item_overlaps_window(&item("a", None, 13, 14), from, to));
    }

    #[test]
    fn item_touching_a_boundary_does_not_overlap() {
        let (from, to) = window();
        // Ends exactly when the window starts.
        assert!(!item_overlaps_window(&item("a", None, 9, 10), from, to));
        // Starts exactly when the window ends.
        assert!(!item_overlaps_window(&item("a", None, 12, 13), from, to));
    }

    #[test]
    fn item_without_end_uses_effective_end_for_overlap() {
        let (from, to) = window();
        let day = Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();
        let mut open = item("a", None, 9, 10);
        open.end = None;
        // No end means start + 1h, so 09:00-10:00 touches the
        // window start only and does not overlap.
        assert!(!item_overlaps_window(&open, from, to));
        open.start = day + Duration::hours(11);
        // 11:00-12:00 overlaps the 10:00-12:00 window.
        assert!(item_overlaps_window(&open, from, to));
    }

    #[test]
    fn tenant_filter_keeps_matching_tenant() {
        let mut owned = item("a", None, 10, 11);
        owned.tenant_id = Some("tenant-1".to_string());
        assert!(item_passes_tenant_filter(&owned, "tenant-1"));
    }

    #[test]
    fn tenant_filter_drops_foreign_tenant() {
        let mut foreign = item("a", None, 10, 11);
        foreign.tenant_id = Some("tenant-2".to_string());
        assert!(!item_passes_tenant_filter(&foreign, "tenant-1"));
    }

    #[test]
    fn tenant_filter_drops_unmarked_tenant_less_item() {
        let plain = item("a", None, 10, 11);
        assert_eq!(plain.tenant_id, None);
        assert!(!item_passes_tenant_filter(&plain, "tenant-1"));
    }

    #[test]
    fn tenant_filter_keeps_marked_system_item_without_tenant() {
        let mut system = item("a", None, 10, 11);
        system.is_system = true;
        assert!(item_passes_tenant_filter(&system, "tenant-1"));
    }

    #[test]
    fn day_range_is_one_day_from_midnight() {
        let now = Utc.with_ymd_and_hms(2026, 1, 2, 15, 30, 0).unwrap();
        let (start, end) = default_range("day", now);
        assert_eq!(end - start, Duration::days(1));
        assert_eq!(start.format("%H:%M").to_string(), "00:00");
    }

    #[test]
    fn week_range_is_seven_days() {
        let now = Utc.with_ymd_and_hms(2026, 1, 2, 15, 30, 0).unwrap();
        let (start, end) = default_range("week", now);
        assert_eq!(end - start, Duration::days(7));
        assert!(start <= now && now < end);
    }

    #[test]
    fn agenda_range_is_fourteen_days() {
        let now = Utc.with_ymd_and_hms(2026, 1, 2, 15, 30, 0).unwrap();
        let (start, end) = default_range("agenda", now);
        assert_eq!(end - start, Duration::days(14));
    }

    #[test]
    fn month_range_starts_on_the_first() {
        let now = Utc.with_ymd_and_hms(2026, 1, 15, 15, 30, 0).unwrap();
        let (start, end) = default_range("month", now);
        assert_eq!(start.format("%d").to_string(), "01");
        assert!(end > start);
    }

    #[test]
    fn overlapping_items_of_same_owner_hint() {
        let hints = conflict_hints(&[
            item("a", Some("user-1"), 10, 11),
            item("b", Some("user-1"), 10, 11),
        ]);
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].first_id, "a");
        assert_eq!(hints[0].second_id, "b");
    }

    #[test]
    fn non_overlapping_items_have_no_hint() {
        let hints = conflict_hints(&[
            item("a", Some("user-1"), 10, 11),
            item("b", Some("user-1"), 12, 13),
        ]);
        assert!(hints.is_empty());
    }

    #[test]
    fn different_owners_have_no_hint() {
        let hints = conflict_hints(&[
            item("a", Some("user-1"), 10, 11),
            item("b", Some("user-2"), 10, 11),
        ]);
        assert!(hints.is_empty());
    }

    #[test]
    fn ownerless_items_have_no_hint() {
        let hints = conflict_hints(&[item("a", None, 10, 11), item("b", None, 10, 11)]);
        assert!(hints.is_empty());
    }
}
