//! Department, routing rule and working-hours types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// Types

/// Working hours for one department. `days` uses 0 = Sunday through
/// 6 = Saturday. `timezone` is a display name; `utc_offset_minutes`
/// is what the hours are evaluated against, because no timezone
/// database is available in this crate set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingHours {
    pub days: Vec<u8>,
    pub start: String,
    pub end: String,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    #[serde(default)]
    pub utc_offset_minutes: i32,
}

pub(crate) fn default_timezone() -> String {
    "UTC".to_string()
}

impl Default for WorkingHours {
    fn default() -> Self {
        Self {
            days: vec![1, 2, 3, 4, 5],
            start: "09:00".to_string(),
            end: "17:00".to_string(),
            timezone: "UTC".to_string(),
            utc_offset_minutes: 0,
        }
    }
}

/// Auto-reply sent to the ticket owner when a ticket arrives outside
/// working hours. Sent through `notification.requested`, never by
/// email directly from here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoReply {
    pub enabled: bool,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
}

impl Default for AutoReply {
    fn default() -> Self {
        Self {
            enabled: false,
            subject: String::new(),
            body: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Department {
    pub id: String,
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub email: Option<String>,
    /// `active` or `inactive`. Inactive departments never receive
    /// routed tickets.
    pub status: String,
    /// The fallback department when no routing rule matches.
    pub is_default: bool,
    /// First-response target in minutes.
    pub sla_first_response_minutes: u32,
    /// Resolution target in minutes.
    pub sla_resolution_minutes: u32,
    pub working_hours: WorkingHours,
    pub auto_reply: AutoReply,
    /// Agent user ids. Names live in the Users module.
    #[serde(default)]
    pub agents: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateDepartment {
    pub name: String,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub email: Option<String>,
    pub status: Option<String>,
    pub is_default: Option<bool>,
    pub sla_first_response_minutes: Option<u32>,
    pub sla_resolution_minutes: Option<u32>,
    pub working_hours: Option<WorkingHours>,
    pub auto_reply: Option<AutoReply>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateDepartment {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub email: Option<String>,
    pub status: Option<String>,
    pub is_default: Option<bool>,
    pub sla_first_response_minutes: Option<u32>,
    pub sla_resolution_minutes: Option<u32>,
    pub working_hours: Option<WorkingHours>,
    pub auto_reply: Option<AutoReply>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddAgent {
    pub user_id: String,
    /// Optional role the agent must hold; validated through the
    /// `role.check` hook when supplied.
    pub role_id: Option<String>,
}

/// One routing rule: which department receives a ticket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingRule {
    pub id: String,
    pub department_id: String,
    /// `category`, `product`, or `keyword`.
    pub match_type: String,
    /// Category slug, product name, or keyword substring.
    pub match_value: String,
    /// Evaluation order; lowest first, ties broken by creation time.
    pub sort_order: u32,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateRoutingRule {
    pub match_type: String,
    pub match_value: String,
    pub sort_order: Option<u32>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateRoutingRule {
    pub match_type: Option<String>,
    pub match_value: Option<String>,
    pub sort_order: Option<u32>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoutePreview {
    pub title: Option<String>,
    pub body: Option<String>,
    pub category: Option<String>,
    pub product: Option<String>,
}
