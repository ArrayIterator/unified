// Inbox - Channel, status, priority and ranking validation.

use unified_core::error::{UnifiedError, UnifiedResult};

use super::{CHANNELS, PRIORITIES, STATUSES};

pub(crate) fn valid_channel(channel: &str) -> UnifiedResult<String> {
    let value = channel.trim().to_lowercase();
    if CHANNELS.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(UnifiedError::bad_request(format!(
            "source_channel must be one of: {}",
            CHANNELS.join(", ")
        )))
    }
}

pub(crate) fn valid_status(status: &str) -> UnifiedResult<String> {
    let value = status.trim().to_lowercase();
    if STATUSES.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(UnifiedError::bad_request(format!(
            "status must be one of: {}",
            STATUSES.join(", ")
        )))
    }
}

pub(crate) fn valid_priority(priority: &str) -> UnifiedResult<String> {
    let value = priority.trim().to_lowercase();
    if PRIORITIES.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(UnifiedError::bad_request(format!(
            "priority must be one of: {}",
            PRIORITIES.join(", ")
        )))
    }
}

/// Map a tickets-module status onto an inbox status.
pub(crate) fn ticket_status_to_inbox(status: &str) -> &'static str {
    match status {
        "waiting" => "pending",
        "resolved" => "resolved",
        "closed" => "closed",
        _ => "open",
    }
}

// Queue handlers

pub(crate) fn priority_rank(priority: &str) -> u8 {
    match priority {
        "urgent" => 0,
        "high" => 1,
        "normal" => 2,
        _ => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{require_text, MAX_BODY_LEN};
    use super::*;
    use unified_core::pagination::PaginationParams;
    use serde_json::json;

    #[test]
    fn require_text_trims_and_returns() {
        assert_eq!(require_text("  hello  ", "subject", 10).unwrap(), "hello");
    }

    #[test]
    fn require_text_rejects_empty_and_too_long() {
        assert!(require_text("   ", "subject", 10).is_err());
        assert!(require_text("abcdef", "subject", 5).is_err());
    }

    #[test]
    fn validate_channel_accepts_known_channels() {
        assert_eq!(valid_channel("email").unwrap(), "email");
        assert_eq!(valid_channel("ticket").unwrap(), "ticket");
    }

    #[test]
    fn validate_channel_rejects_unknown() {
        assert!(valid_channel("phone").is_err());
    }

    #[test]
    fn validate_status_accepts_known_statuses() {
        assert_eq!(valid_status("open").unwrap(), "open");
        assert_eq!(valid_status("resolved").unwrap(), "resolved");
    }

    #[test]
    fn validate_status_rejects_unknown() {
        assert!(valid_status("archived").is_err());
    }

    #[test]
    fn validate_priority_accepts_known_priorities() {
        assert_eq!(valid_priority("urgent").unwrap(), "urgent");
        assert_eq!(valid_priority("low").unwrap(), "low");
    }

    #[test]
    fn validate_priority_rejects_unknown() {
        assert!(valid_priority("critical").is_err());
    }

    #[test]
    fn ticket_status_to_inbox_maps_all_cases() {
        assert_eq!(ticket_status_to_inbox("waiting"), "pending");
        assert_eq!(ticket_status_to_inbox("resolved"), "resolved");
        assert_eq!(ticket_status_to_inbox("closed"), "closed");
        assert_eq!(ticket_status_to_inbox("open"), "open");
        assert_eq!(ticket_status_to_inbox("anything-else"), "open");
    }

    #[test]
    fn priority_rank_orders_urgent_first() {
        assert_eq!(priority_rank("urgent"), 0);
        assert_eq!(priority_rank("high"), 1);
        assert_eq!(priority_rank("normal"), 2);
        assert_eq!(priority_rank("low"), 3);
        assert_eq!(priority_rank("unknown"), 3);
    }

    #[test]
    fn validate_reply_body_accepts_text() {
        assert!(require_text("thanks", "body", MAX_BODY_LEN).is_ok());
        assert_eq!(json!("thanks").as_str(), Some("thanks"));
    }

    #[test]
    fn validate_reply_body_rejects_empty() {
        assert!(require_text("   ", "body", MAX_BODY_LEN).is_err());
    }

    #[test]
    fn validate_reply_body_rejects_non_string() {
        assert!(json!(42).as_str().is_none());
    }

    #[test]
    fn validate_page_params_clamps_per_page() {
        let params = PaginationParams {
            page: Some(1),
            per_page: Some(500),
            ..Default::default()
        };
        assert_eq!(params.normalized().per_page, 100);
    }
}
