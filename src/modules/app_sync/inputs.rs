//! app_sync - inputs.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;



#[derive(Debug, Deserialize)]
// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------
pub(crate) struct RegisterDeviceRequest {
    pub(crate) device_name: String,
    #[serde(default = "default_platform")]
    pub(crate) platform: String,
}


pub(crate) fn default_platform() -> String {
    "unknown".to_string()
}


#[derive(Debug, Deserialize)]
pub(crate) struct SyncPushRequest {
    pub(crate) device_token: String,
    pub(crate) payload: Value,
    /// Optional client-chosen key: replaying a push with the same key
    /// returns the original record instead of appending a duplicate.
    #[serde(default)]
    pub(crate) idempotency_key: Option<String>,
}


#[derive(Debug, Deserialize)]
pub(crate) struct SyncPullRequest {
    pub(crate) device_token: String,
    /// Cursor: a version number, an RFC 3339 timestamp, or omitted for
    /// a full pull.
    #[serde(default)]
    pub(crate) since: Option<Value>,
}


/// Split a pull cursor into a version floor and a timestamp floor.
pub(crate) fn parse_cursor(since: &Option<Value>) -> (u64, Option<DateTime<Utc>>) {
    match since {
        Some(Value::Number(number)) => (
            number.as_u64().unwrap_or(0),
            None,
        ),
        Some(Value::String(text)) => {
            if let Ok(version) = text.parse::<u64>() {
                (version, None)
            } else if let Ok(time) = text.parse::<DateTime<Utc>>() {
                (0, Some(time))
            } else {
                (0, None)
            }
        }
        _ => (0, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cursor_reads_version_numbers() {
        assert_eq!(parse_cursor(&None), (0, None));
        assert_eq!(
            parse_cursor(&Some(serde_json::json!(5))),
            (5, None)
        );
        assert_eq!(
            parse_cursor(&Some(serde_json::json!("7"))),
            (7, None)
        );
    }

    #[test]
    fn parse_cursor_reads_rfc3339_timestamps() {
        let (version, time) = parse_cursor(&Some(serde_json::json!("2026-11-22T10:00:00Z")));
        assert_eq!(version, 0);
        assert_eq!(
            time.map(|moment| moment.format("%Y-%m-%d").to_string()),
            Some("2026-11-22".to_string())
        );
    }

    #[test]
    fn parse_cursor_falls_back_to_full_pull() {
        assert_eq!(parse_cursor(&Some(serde_json::json!("garbage"))), (0, None));
        assert_eq!(parse_cursor(&Some(serde_json::json!(true))), (0, None));
        assert_eq!(parse_cursor(&Some(serde_json::json!(null))), (0, None));
    }
}
