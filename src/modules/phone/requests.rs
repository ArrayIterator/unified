//! Phone request bodies: the JSON shapes the API accepts.
//!
//! Kept separate from `types.rs` so that file stays under the
//! ~1500 line convention. Nothing here is behaviour: every
//! field is validated by the handlers (lengths, codes,
//! time-of-day shapes, target existence) before it is stored.

use serde::Deserialize;
use uuid::Uuid;

// Request bodies

#[derive(Debug, Deserialize)]
pub struct InitiateCallRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callee_id: Option<String>,
    #[serde(default)]
    pub callee_ids: Vec<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default)]
    pub recording_consent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ivr_menu_code: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCallRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_consent: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct InviteParticipantRequest {
    pub user_id: String,
}

#[derive(Debug, Deserialize)]
pub struct EndCallRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SignalRequest {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_id: Option<String>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct CreateQueueRequest {
    pub code: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub ring_strategy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ring_timeout_secs: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wait_secs: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overflow_queue_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overflow_action: Option<String>,
    #[serde(default = "default_true")]
    pub active: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateQueueRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ring_strategy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ring_timeout_secs: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wait_secs: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overflow_queue_id: Option<Option<Uuid>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overflow_action: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct AddQueueMemberRequest {
    pub user_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
    #[serde(default = "default_true")]
    pub active: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateRoutingRuleRequest {
    pub code: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub priority: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_caller_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_time_start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_time_end: Option<String>,
    pub target_type: String,
    pub target_id: String,
    #[serde(default = "default_true")]
    pub active: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRoutingRuleRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_caller_id: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_time_start: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_time_end: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct EvaluateRoutingRequest {
    pub caller_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now_minutes: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct SetPresenceRequest {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateIvrMenuRequest {
    pub code: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub greeting: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub greeting_audio_ref: Option<String>,
    #[serde(default = "default_true")]
    pub active: bool,
    #[serde(default)]
    pub nodes: Vec<CreateIvrNodeRequest>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateIvrMenuRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub greeting: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub greeting_audio_ref: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateIvrNodeRequest {
    pub key: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ResolveIvrRequest {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateScriptRequest {
    pub code: String,
    pub label: String,
    pub trigger: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_capability: Option<String>,
    #[serde(default)]
    pub priority: i64,
    #[serde(default = "default_true")]
    pub active: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateScriptRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_template: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_capability: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct AutoResponseRequest {
    pub caller_id: String,
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub menu_code: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AttachRecordingRequest {
    pub storage_target: String,
    pub storage_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_ring_timeout_secs: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_call_duration_secs: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_requires_consent: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missed_call_timeout_secs: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_group_calls: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_group_participants: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_queue_code: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_ivr_menu_code: Option<Option<String>>,
}

fn default_true() -> bool {
    true
}
