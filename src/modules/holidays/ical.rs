//! iCalendar (ICS) feed parsing.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, ProductCategory, RouteInfo, RouteVersionSupport};
use unified_core::tenancy::{schema_name_for_str, TenantContext as TenancyContext};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use reqwest::header::RETRY_AFTER;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// iCal parsing (hand-rolled: no ical crate is available).
// ---------------------------------------------------------------------------

/// One parsed VEVENT: the date the holiday falls on and its name.
#[derive(Debug, Clone)]
pub(crate) struct IcsEvent {
    pub(crate) date: NaiveDate,
    pub(crate) summary: String,
}

/// Unfold iCal lines: a line starting with a space or tab continues
/// the previous line (RFC 5545 section 3.1).
fn unfold_ics_lines(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if (line.starts_with(' ') || line.starts_with('\t')) && !out.is_empty() {
            let previous = out.pop().expect("unfold checked non-empty");
            out.push(format!("{previous}{}", line[1..].to_string()));
        } else {
            out.push(line.to_string());
        }
    }
    out
}

/// Unescape an iCal TEXT value: `\\` -> `\`, `\n` / `\N` -> newline,
/// `\,` -> `,`, `\;` -> `;`.
fn ics_unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(char) = chars.next() {
        if char != '\\' {
            out.push(char);
            continue;
        }
        match chars.next() {
            Some('n') | Some('N') => out.push('\n'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Parse an iCal date value: `YYYYMMDD`, `YYYYMMDDTHHMMSSZ`, or
/// `YYYYMMDDTHHMMSS` (a floating local time — the date part is what
/// a holiday needs). Returns None when the first 8 digits are not a
/// valid calendar date.
fn parse_ics_date(value: &str) -> Option<NaiveDate> {
    let digits: String = value.chars().take(8).collect();
    if digits.len() != 8 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    NaiveDate::parse_from_str(&digits, "%Y%m%d").ok()
}

/// Split one unfolded iCal content line into (name, value): the name
/// is the part before the first `;` or `:`, the value is the part
/// after the first `:`.
fn split_ics_line(line: &str) -> Option<(String, String)> {
    let colon = line.find(':')?;
    let name_part = &line[..colon];
    let name = name_part.split(';').next().unwrap_or("").to_ascii_uppercase();
    if name.is_empty() {
        return None;
    }
    Some((name, line[colon + 1..].to_string()))
}

/// Parse an iCal feed into its VEVENTs. Events without a parseable
/// DTSTART or without a SUMMARY are skipped (counted by the caller
/// as found-but-unusable through the returned skip count); a feed
/// with no VEVENT at all parses to an empty list, which the sync
/// records honestly as zero holidays found.
pub(crate) fn parse_ics(text: &str) -> (Vec<IcsEvent>, usize) {
    let mut events: Vec<IcsEvent> = Vec::new();
    let mut skipped: usize = 0;
    let mut in_event = false;
    let mut current_date: Option<NaiveDate> = None;
    let mut current_summary: Option<String> = None;
    for line in unfold_ics_lines(text) {
        let Some((name, value)) = split_ics_line(&line) else {
            continue;
        };
        match name.as_str() {
            "BEGIN" if value.eq_ignore_ascii_case("VEVENT") => {
                in_event = true;
                current_date = None;
                current_summary = None;
            }
            "END" if value.eq_ignore_ascii_case("VEVENT") => {
                if in_event {
                    match (current_date, current_summary.take()) {
                        (Some(date), Some(summary)) if !summary.trim().is_empty() => {
                            events.push(IcsEvent {
                                date,
                                summary: ics_unescape(summary.trim()),
                            });
                        }
                        _ => skipped += 1,
                    }
                }
                in_event = false;
                current_date = None;
                current_summary = None;
            }
            "DTSTART" if in_event => {
                if current_date.is_none() {
                    current_date = parse_ics_date(value.trim());
                }
            }
            "SUMMARY" if in_event => {
                if current_summary.is_none() {
                    current_summary = Some(value);
                }
            }
            _ => {}
        }
    }
    // A VEVENT whose END never arrives (truncated feed) is an
    // incomplete event too: count it as skipped instead of
    // silently dropping it.
    if in_event {
        skipped += 1;
    }
    (events, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfold_joins_folded_lines() {
        let lines = unfold_ics_lines("BEGIN:VEVENT\nSUMMARY:Long\n  folded summary\nDTSTART:20261225");
        assert_eq!(
            lines,
            vec![
                "BEGIN:VEVENT",
                "SUMMARY:Long folded summary",
                "DTSTART:20261225",
            ]
        );
    }

    #[test]
    fn unfold_strips_crlf() {
        assert_eq!(unfold_ics_lines("A\r\nB\r\n"), vec!["A", "B"]);
    }

    #[test]
    fn ics_unescape_handles_escapes() {
        assert_eq!(ics_unescape("a\\nb"), "a\nb");
        assert_eq!(ics_unescape("a\\,b"), "a,b");
        assert_eq!(ics_unescape("a\\\\b"), "a\\b");
        assert_eq!(ics_unescape("abc\\"), "abc\\");
        assert_eq!(ics_unescape("plain"), "plain");
    }

    #[test]
    fn parse_ics_date_accepts_date_and_datetime() {
        assert_eq!(
            parse_ics_date("20261225"),
            Some(NaiveDate::from_ymd_opt(2026, 12, 25).unwrap())
        );
        assert_eq!(
            parse_ics_date("20261225T090000Z"),
            Some(NaiveDate::from_ymd_opt(2026, 12, 25).unwrap())
        );
        assert_eq!(parse_ics_date("20260230"), None);
        assert_eq!(parse_ics_date("hello"), None);
    }

    #[test]
    fn split_ics_line_splits_name_and_value() {
        assert_eq!(
            split_ics_line("DTSTART;VALUE=DATE:20261225"),
            Some(("DTSTART".to_string(), "20261225".to_string()))
        );
        assert_eq!(
            split_ics_line("BEGIN:VEVENT"),
            Some(("BEGIN".to_string(), "VEVENT".to_string()))
        );
    }

    #[test]
    fn split_ics_line_rejects_lines_without_colon_or_name() {
        assert_eq!(split_ics_line("NOPE"), None);
        assert_eq!(split_ics_line(":x"), None);
    }

    #[test]
    fn parse_ics_parses_events() {
        let feed = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nDTSTART;VALUE=DATE:20261225\nSUMMARY:Christmas Day\nEND:VEVENT\nBEGIN:VEVENT\nDTSTART:20270101T000000Z\nSUMMARY:New Year\nEND:VEVENT\nEND:VCALENDAR";
        let (events, skipped) = parse_ics(feed);
        assert_eq!(skipped, 0);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].date, NaiveDate::from_ymd_opt(2026, 12, 25).unwrap());
        assert_eq!(events[0].summary, "Christmas Day");
        assert_eq!(events[1].date, NaiveDate::from_ymd_opt(2027, 1, 1).unwrap());
        assert_eq!(events[1].summary, "New Year");
    }

    #[test]
    fn parse_ics_skips_incomplete_events() {
        let feed = "BEGIN:VEVENT\nDTSTART;VALUE=DATE:20261225\nSUMMARY:Has Summary\nEND:VEVENT\nBEGIN:VEVENT\nDTSTART;VALUE=DATE:20261226\nEND:VEVENT";
        let (events, skipped) = parse_ics(feed);
        assert_eq!(events.len(), 1);
        assert_eq!(skipped, 1);
    }

    #[test]
    fn parse_ics_counts_unterminated_event_as_skipped() {
        let feed = "BEGIN:VEVENT\nDTSTART;VALUE=DATE:20261225\nSUMMARY:Christmas Day\n";
        let (events, skipped) = parse_ics(feed);
        assert_eq!(events.len(), 0);
        assert_eq!(skipped, 1);
    }

    #[test]
    fn parse_ics_unescapes_summary() {
        let feed = "BEGIN:VEVENT\nDTSTART;VALUE=DATE:20261225\nSUMMARY:New Year\\, Day\nEND:VEVENT";
        let (events, skipped) = parse_ics(feed);
        assert_eq!(skipped, 0);
        assert_eq!(events[0].summary, "New Year, Day");
    }
}
