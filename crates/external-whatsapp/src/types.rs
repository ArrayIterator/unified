//! WhatsApp external module (split from lib.rs; moved verbatim).

use super::*;

/// One WhatsApp message, outbound or inbound. Every record carries
/// `tenant_id`; reads always filter on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRecord {
    /// Module-generated id, `wamid.<uuid>`.
    pub id: String,
    pub tenant_id: String,
    /// `outbound` or `inbound`.
    pub direction: String,
    /// Counterparty phone in E.164 format.
    pub phone: String,
    /// `text`, `template`, or a Meta inbound type (`image`, `audio`, ...).
    pub kind: String,
    /// Text body for `text` messages (and a short description for
    /// non-text inbound messages).
    pub body: Option<String>,
    pub template_name: Option<String>,
    pub template_language: Option<String>,
    pub template_variables: Vec<String>,
    /// One of the `statuses::*` values.
    pub status: String,
    /// Human-readable reason for `pending_config` / `failed`, if any.
    pub status_reason: Option<String>,
    /// Meta message id returned by the Graph API on send (outbound),
    /// or the id Meta assigned to an inbound message.
    pub external_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A message template: admin-created data, never code. The body may
/// contain `{{n}}` variables; `count_template_variables` validates
/// them at creation time and the send handler checks the caller's
/// variable count against the body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Template {
    pub tenant_id: String,
    pub name: String,
    pub language: String,
    /// `utility`, `marketing`, or `authentication`.
    pub category: String,
    pub body: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Template categories accepted by the Meta template API.
pub const TEMPLATE_CATEGORIES: &[&str] = &["utility", "marketing", "authentication"];

/// A recorded intent to notify a buyer via WhatsApp (from the
/// `store.order.paid` hook). An intent is data only: recording one
/// never sends a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationIntent {
    pub id: String,
    pub tenant_id: String,
    pub order_id: String,
    pub phone: String,
    pub template_name: String,
    /// `ready` (template + credentials configured) or `blocked`.
    pub status: String,
    pub reason: Option<String>,
    pub created_at: String,
}

/// One conversation summary: all messages with one phone number.
#[derive(Debug, Clone, Serialize)]
pub struct ConversationView {
    pub phone: String,
    pub last_message: MessageRecord,
    pub message_count: usize,
    pub unread_count: usize,
    pub last_at: String,
}

/// Shared mutable state for the module. Everything is in memory:
/// messages, templates, notification intents, and the per-tenant
/// send rate limiter. The hook bus is attached by `init` (from
/// `AppContext::hooks`) so webhook handlers can emit events.
pub struct WhatsAppState {
    pub(crate) config: RwLock<WhatsAppConfig>,
    pub(crate) messages: RwLock<HashMap<String, MessageRecord>>,
    pub(crate) templates: RwLock<HashMap<String, Template>>,
    pub(crate) intents: RwLock<Vec<NotificationIntent>>,
    pub(crate) rate_limits: Mutex<HashMap<String, (Instant, u32)>>,
    pub(crate) hooks: RwLock<Option<Arc<HookBus>>>,
    pub(crate) http: reqwest::Client,
}
