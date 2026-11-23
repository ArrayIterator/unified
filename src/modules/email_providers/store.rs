//! email_providers - store.

use unified_core::error::{UnifiedError, UnifiedResult};
use dashmap::DashMap;
use serde::{Serialize};
use serde_json::{Value};
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};
use uuid::Uuid;
use super::{events::*, provider_kinds::*, credentials::*, http::*, registry::*, adapters::*, worker::*, hooks::*, handlers::*, module::*};



// ---------------------------------------------------------------------------
// In-memory stores (same pattern as the cloud provider connections).
// ---------------------------------------------------------------------------
pub(crate) static CONNECTIONS: OnceLock<DashMap<Uuid, EmailConnection>> = OnceLock::new();


pub(crate) fn connections_store() -> &'static DashMap<Uuid, EmailConnection> {
    CONNECTIONS.get_or_init(DashMap::new)
}


/// The global default connection id (fallback only; every send takes
/// an explicit connection id first).
pub(crate) static DEFAULT_CONNECTION: OnceLock<RwLock<Option<Uuid>>> = OnceLock::new();


pub(crate) fn default_connection_store() -> &'static RwLock<Option<Uuid>> {
    DEFAULT_CONNECTION.get_or_init(|| RwLock::new(None))
}


pub fn get_connection(id: &Uuid) -> Option<EmailConnection> {
    connections_store().get(id).map(|entry| entry.value().clone())
}


pub fn default_connection() -> Option<EmailConnection> {
    let id = (*default_connection_store()
        .read()
        .expect("default connection lock poisoned"))?;
    get_connection(&id)
}


/// Resolve the connection for a send: an explicit id wins, otherwise
/// the global default. Fails honestly when neither resolves.
pub fn resolve_connection(connection_id: Option<&str>) -> UnifiedResult<EmailConnection> {
    if let Some(raw) = connection_id {
        let id = Uuid::parse_str(raw).map_err(|_| {
            UnifiedError::bad_request(format!("Invalid connection id \"{raw}\""))
        })?;
        return get_connection(&id).ok_or_else(|| {
            UnifiedError::not_found(format!("Email provider connection not found: {raw}"))
        });
    }
    default_connection().ok_or_else(|| {
        UnifiedError::bad_request(
            "No email provider connection_id was supplied and no default connection is set",
        )
    })
}


pub(crate) fn first_enabled_connection(kind: EmailProviderKind) -> Option<EmailConnection> {
    connections_store()
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|connection| connection.kind == kind && connection.enabled)
        .min_by_key(|connection| connection.created_at)
}



#[derive(Debug, Clone)]
// ---------------------------------------------------------------------------
// Messages, results, and the send log.
// ---------------------------------------------------------------------------
/// One outgoing email. `metadata` carries caller context (for example
/// `campaign_id` / `subscriber_id`); adapters map it to the provider's
/// custom-args / tags / metadata field where the provider supports one,
/// so webhooks can be matched back to the campaign.
pub struct EmailMessage {
    pub to: String,
    pub from: Option<String>,
    pub from_name: Option<String>,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub reply_to: Option<String>,
    pub template_id: Option<String>,
    pub variables: HashMap<String, Value>,
    pub metadata: HashMap<String, String>,
    pub test_mode: bool,
}


impl EmailMessage {
    pub(crate) fn sender(&self, connection: &EmailConnection) -> (String, Option<String>) {
        let from = self
            .from
            .clone()
            .or_else(|| connection.sender_email.clone())
            .unwrap_or_default();
        let name = self
            .from_name
            .clone()
            .or_else(|| connection.sender_name.clone());
        (from, name)
    }
}


#[derive(Debug, Clone, Serialize)]
pub struct SendResult {
    pub provider_message_id: Option<String>,
    pub accepted: bool,
    /// True when the delivery was simulated (test mode on a provider
    /// without a native sandbox) or accepted by a provider sandbox.
    pub test_mode: bool,
    pub detail: String,
}


#[derive(Debug, Clone, Serialize)]
pub struct SendLogEntry {
    pub id: String,
    pub connection_id: String,
    pub provider: String,
    pub to: String,
    pub subject: String,
    pub status: String,
    pub provider_message_id: Option<String>,
    pub test_mode: bool,
    pub error: Option<String>,
    pub created_at: String,
}


pub(crate) static SEND_LOG: OnceLock<RwLock<Vec<SendLogEntry>>> = OnceLock::new();


pub(crate) fn send_log_store() -> &'static RwLock<Vec<SendLogEntry>> {
    SEND_LOG.get_or_init(|| RwLock::new(Vec::new()))
}


pub(crate) fn append_log(entry: SendLogEntry) {
    send_log_store()
        .write()
        .expect("send log lock poisoned")
        .push(entry);
}


pub(crate) fn update_log(id: &str, status: &str, provider_message_id: Option<String>, error: Option<String>) {
    let mut log = send_log_store().write().expect("send log lock poisoned");
    if let Some(entry) = log.iter_mut().find(|entry| entry.id == id) {
        entry.status = status.to_string();
        entry.provider_message_id = provider_message_id;
        entry.error = error;
    }
}


/// A queued send job (created by the `email.send_requested` hook).
#[derive(Debug, Clone)]
pub(crate) struct QueuedSend {
    pub(crate) job_id: String,
    pub(crate) connection_id: Uuid,
    pub(crate) message: EmailMessage,
}
