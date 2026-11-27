//! calendar module - iCalendar (RFC 5545) rendering.

use actix_web::HttpResponse;
use chrono::{DateTime, Utc};

use super::types::CalendarItem;

// iCalendar (RFC 5545, hand-built - no calendar crate in Cargo.toml).
// Follows the manual building pattern from events.rs / appointments.rs.

/// Escape a TEXT value per RFC 5545 section 3.3.11: backslash,
/// semicolon, comma, and newlines. Folding is applied at 75
/// octets per line (section 3.1) with a single leading space on
/// continuation lines.
pub(crate) fn ics_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            other => out.push(other),
        }
    }
    out
}

pub(crate) fn ics_fold(line: &str) -> String {
    if line.len() <= 75 {
        return line.to_string();
    }
    let mut out = String::new();
    let mut current = String::new();
    for ch in line.chars() {
        if current.len() + ch.len_utf8() > 75 {
            out.push_str(&current);
            out.push_str("\r\n ");
            current.clear();
        }
        current.push(ch);
    }
    out.push_str(&current);
    out
}

pub(crate) fn ics_datetime(value: DateTime<Utc>) -> String {
    value.format("%Y%m%dT%H%M%SZ").to_string()
}

pub(crate) fn ics_date(value: DateTime<Utc>) -> String {
    value.format("%Y%m%d").to_string()
}

pub(crate) fn ics_item_block(item: &CalendarItem) -> String {
    let mut out = String::new();
    out.push_str("BEGIN:VEVENT\r\n");
    out.push_str(&ics_fold(&format!(
        "UID:{}-{}@unified",
        ics_escape(&item.module),
        ics_escape(&item.id)
    )));
    out.push_str("\r\n");
    out.push_str(&format!("DTSTAMP:{}\r\n", ics_datetime(Utc::now())));
    if item.all_day {
        out.push_str(&format!("DTSTART;VALUE=DATE:{}\r\n", ics_date(item.start)));
        out.push_str(&format!(
            "DTEND;VALUE=DATE:{}\r\n",
            ics_date(item.effective_end())
        ));
    } else {
        out.push_str(&format!("DTSTART:{}\r\n", ics_datetime(item.start)));
        out.push_str(&format!("DTEND:{}\r\n", ics_datetime(item.effective_end())));
    }
    out.push_str(&ics_fold(&format!("SUMMARY:{}", ics_escape(&item.title))));
    out.push_str("\r\n");
    if let Some(label) = &item.label {
        out.push_str(&ics_fold(&format!("CATEGORIES:{}", ics_escape(label))));
        out.push_str("\r\n");
    }
    if let Some(link) = &item.deep_link {
        out.push_str(&ics_fold(&format!("URL:{}", ics_escape(link))));
        out.push_str("\r\n");
        out.push_str(&ics_fold(&format!("DESCRIPTION:{}", ics_escape(link))));
        out.push_str("\r\n");
    }
    out.push_str(&format!(
        "X-UNIFIED-MODULE:{}\r\n",
        ics_escape(&item.module)
    ));
    out.push_str(&format!(
        "X-UNIFIED-VISIBILITY:{}\r\n",
        ics_escape(&item.visibility)
    ));
    out.push_str("END:VEVENT");
    out
}

pub(crate) fn ics_calendar(items: &[CalendarItem], calendar_name: &str) -> String {
    let mut out = String::new();
    out.push_str("BEGIN:VCALENDAR\r\n");
    out.push_str("VERSION:2.0\r\n");
    out.push_str("PRODID:-//Unified//Calendar//EN\r\n");
    out.push_str("CALSCALE:GREGORIAN\r\n");
    out.push_str("METHOD:PUBLISH\r\n");
    out.push_str(&ics_fold(&format!(
        "X-WR-CALNAME:{}",
        ics_escape(calendar_name)
    )));
    out.push_str("\r\n");
    for item in items {
        out.push_str(&ics_item_block(item));
        out.push_str("\r\n");
    }
    out.push_str("END:VCALENDAR\r\n");
    out
}

pub(crate) fn ics_response(body: String) -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/calendar; charset=utf-8")
        .insert_header((
            "Content-Disposition",
            "attachment; filename=\"calendar.ics\"",
        ))
        .body(body)
}

#[cfg(test)]
mod tests {
    use super::ics_calendar;
    use super::ics_date;
    use super::ics_datetime;
    use super::ics_escape;
    use super::ics_fold;
    use super::ics_item_block;
    use crate::modules::calendar::CalendarItem;
    use chrono::{TimeZone, Utc};

    fn item(title: &str) -> CalendarItem {
        CalendarItem {
            id: "item-1".to_string(),
            module: "appointments".to_string(),
            title: title.to_string(),
            start: Utc.with_ymd_and_hms(2026, 1, 2, 10, 0, 0).unwrap(),
            end: None,
            all_day: false,
            color: None,
            label: None,
            deep_link: None,
            visibility: "shared".to_string(),
            owner_user_id: None,
            company_id: None,
            tenant_id: None,
            is_system: false,
            start_local: None,
            end_local: None,
        }
    }

    #[test]
    fn escape_specials() {
        assert_eq!(ics_escape("a;b,c\\d\ne"), "a\\;b\\,c\\\\d\\ne");
    }

    #[test]
    fn escape_plain_unchanged() {
        assert_eq!(ics_escape("plain title"), "plain title");
        assert_eq!(ics_escape(""), "");
    }

    #[test]
    fn escape_drops_carriage_return() {
        assert_eq!(ics_escape("a\rb"), "ab");
    }

    #[test]
    fn fold_short_unchanged() {
        let line = "a".repeat(75);
        assert_eq!(ics_fold(&line), line);
    }

    #[test]
    fn fold_long_inserts_continuation() {
        let folded = ics_fold(&"a".repeat(80));
        assert!(folded.contains("\r\n "));
        assert!(!folded.contains("\r\n  "));
    }

    #[test]
    fn datetime_and_date_format() {
        let moment = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
        assert_eq!(ics_datetime(moment), "20260102T030405Z");
        assert_eq!(ics_date(moment), "20260102");
    }

    #[test]
    fn item_block_wraps_event() {
        let block = ics_item_block(&item("Team; sync"));
        assert!(block.starts_with("BEGIN:VEVENT\r\n"));
        assert!(block.contains("Team\\; sync"));
        assert!(block.contains("END:VEVENT"));
    }

    #[test]
    fn calendar_wraps_items() {
        let body = ics_calendar(&[item("Demo")], "My Calendar");
        assert!(body.contains("BEGIN:VCALENDAR"));
        assert!(body.contains("END:VCALENDAR"));
        assert!(body.contains("Demo"));
    }
}
