//! WhatsApp external module (split from lib.rs; moved verbatim).

use super::*;

impl WhatsAppState {
    pub fn new(config: WhatsAppConfig) -> Self {
        Self {
            config: RwLock::new(config),
            messages: RwLock::new(HashMap::new()),
            templates: RwLock::new(HashMap::new()),
            intents: RwLock::new(Vec::new()),
            rate_limits: Mutex::new(HashMap::new()),
            hooks: RwLock::new(None),
            http: reqwest::Client::new(),
        }
    }

    pub fn config_snapshot(&self) -> WhatsAppConfig {
        self.config
            .read()
            .expect("whatsapp config lock poisoned")
            .clone()
    }

    pub fn refresh_config_from_env(&self) {
        let mut config = self.config.write().expect("whatsapp config lock poisoned");
        *config = WhatsAppConfig::from_env();
    }

    pub fn attach_hooks(&self, bus: Arc<HookBus>) {
        let mut hooks = self.hooks.write().expect("whatsapp hooks lock poisoned");
        *hooks = Some(bus);
    }

    /// Emit a hook event when the bus is attached and at least one
    /// handler is registered; a no-op otherwise.
    pub fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self
            .hooks
            .read()
            .expect("whatsapp hooks lock poisoned")
            .as_ref()
        {
            bus.emit_if_registered(event, data);
        }
    }

    pub fn insert_message(&self, record: MessageRecord) {
        self.messages
            .write()
            .expect("whatsapp messages lock poisoned")
            .insert(record.id.clone(), record);
    }

    pub fn get_message(&self, id: &str) -> Option<MessageRecord> {
        self.messages
            .read()
            .expect("whatsapp messages lock poisoned")
            .get(id)
            .cloned()
    }

    /// Fetch a message only when it belongs to `tenant_id`. A
    /// cross-tenant id looks exactly like a missing id (404).
    pub fn get_message_for_tenant(&self, id: &str, tenant_id: &str) -> Option<MessageRecord> {
        self.get_message(id)
            .filter(|record| record.tenant_id == tenant_id)
    }

    /// Find an outbound record by the Meta message id returned on send.
    pub fn find_by_external_id(&self, external_id: &str) -> Option<MessageRecord> {
        self.messages
            .read()
            .expect("whatsapp messages lock poisoned")
            .values()
            .find(|record| record.external_id.as_deref() == Some(external_id))
            .cloned()
    }

    pub fn update_status(&self, id: &str, status: &str, reason: Option<String>) {
        let mut messages = self
            .messages
            .write()
            .expect("whatsapp messages lock poisoned");
        if let Some(record) = messages.get_mut(id) {
            record.status = status.to_string();
            record.status_reason = reason;
            record.updated_at = now_rfc3339();
        }
    }

    pub fn mark_sent(&self, id: &str, external_id: Option<String>) {
        let mut messages = self
            .messages
            .write()
            .expect("whatsapp messages lock poisoned");
        if let Some(record) = messages.get_mut(id) {
            record.status = statuses::SENT.to_string();
            record.status_reason = None;
            record.external_id = external_id;
            record.updated_at = now_rfc3339();
        }
    }

    /// List a tenant's messages, newest first, optionally filtered by
    /// status and/or phone.
    pub fn list_messages(
        &self,
        tenant_id: &str,
        status: Option<&str>,
        phone: Option<&str>,
    ) -> Vec<MessageRecord> {
        let mut out: Vec<MessageRecord> = self
            .messages
            .read()
            .expect("whatsapp messages lock poisoned")
            .values()
            .filter(|record| record.tenant_id == tenant_id)
            .filter(|record| status.is_none_or(|s| record.status == s))
            .filter(|record| phone.is_none_or(|p| record.phone == p))
            .cloned()
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
        out
    }

    pub fn message_count(&self) -> usize {
        self.messages
            .read()
            .expect("whatsapp messages lock poisoned")
            .len()
    }

    pub fn queued_count(&self) -> usize {
        self.messages
            .read()
            .expect("whatsapp messages lock poisoned")
            .values()
            .filter(|record| {
                record.status == statuses::QUEUED || record.status == statuses::PENDING_CONFIG
            })
            .count()
    }

    pub fn template_key(tenant_id: &str, name: &str, language: &str) -> String {
        format!("{tenant_id}|{name}|{language}")
    }

    pub fn get_template(&self, tenant_id: &str, name: &str, language: &str) -> Option<Template> {
        self.templates
            .read()
            .expect("whatsapp templates lock poisoned")
            .get(&Self::template_key(tenant_id, name, language))
            .cloned()
    }

    pub fn insert_template(&self, template: Template) {
        self.templates
            .write()
            .expect("whatsapp templates lock poisoned")
            .insert(
                Self::template_key(&template.tenant_id, &template.name, &template.language),
                template,
            );
    }

    pub fn list_templates(
        &self,
        tenant_id: &str,
        category: Option<&str>,
        language: Option<&str>,
    ) -> Vec<Template> {
        let mut out: Vec<Template> = self
            .templates
            .read()
            .expect("whatsapp templates lock poisoned")
            .values()
            .filter(|template| template.tenant_id == tenant_id)
            .filter(|template| category.is_none_or(|c| template.category == c))
            .filter(|template| language.is_none_or(|l| template.language == l))
            .cloned()
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name).then(a.language.cmp(&b.language)));
        out
    }

    pub fn template_count(&self) -> usize {
        self.templates
            .read()
            .expect("whatsapp templates lock poisoned")
            .len()
    }

    pub fn record_intent(&self, intent: NotificationIntent) {
        self.intents
            .write()
            .expect("whatsapp intents lock poisoned")
            .push(intent);
    }

    /// Attribute an inbound message to a tenant: the tenant of the
    /// newest outbound message to the same phone, or `unassigned`
    /// when the phone has never been messaged. Never returns another
    /// tenant's id.
    pub fn resolve_inbound_tenant(&self, phone: &str) -> String {
        let mut best: Option<MessageRecord> = None;
        for record in self
            .messages
            .read()
            .expect("whatsapp messages lock poisoned")
            .values()
        {
            if record.direction == "outbound" && record.phone == phone {
                let newer = match &best {
                    Some(current) => record.created_at > current.created_at,
                    None => true,
                };
                if newer {
                    best = Some(record.clone());
                }
            }
        }
        best.map(|record| record.tenant_id)
            .unwrap_or_else(|| UNASSIGNED_TENANT.to_string())
    }

    /// Fixed-window send rate limit, per tenant. Counts attempts that
    /// passed validation, before queueing. Returns a 429 error when
    /// the tenant already sent `rate_limit_per_minute` messages in
    /// the current 60-second window.
    pub fn check_rate_limit(&self, tenant_id: &str) -> Result<(), UnifiedError> {
        let limit = self.config_snapshot().rate_limit_per_minute;
        let mut table = self
            .rate_limits
            .lock()
            .expect("whatsapp rate limit lock poisoned");
        let now = Instant::now();
        let entry = table.entry(tenant_id.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) >= Duration::from_secs(60) {
            *entry = (now, 0);
        }
        if entry.1 >= limit {
            return Err(UnifiedError::new(
                format!(
                    "WhatsApp send rate limit exceeded for this tenant ({limit} messages per minute)"
                ),
                429,
            ));
        }
        entry.1 += 1;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Meta Graph API delivery
// ---------------------------------------------------------------------------

/// Build the Graph API request body for one queued outbound message.
/// Pure function (no network), unit-tested.
pub fn build_graph_payload(record: &MessageRecord) -> Value {
    if record.kind == "template" {
        let parameters: Vec<Value> = record
            .template_variables
            .iter()
            .map(|variable| json!({"type": "text", "text": variable}))
            .collect();
        json!({
            "messaging_product": "whatsapp",
            "to": record.phone,
            "type": "template",
            "template": {
                "name": record.template_name,
                "language": {
                    "code": record
                        .template_language
                        .clone()
                        .unwrap_or_else(|| DEFAULT_TEMPLATE_LANGUAGE.to_string()),
                },
                "components": [{ "type": "body", "parameters": parameters }],
            },
        })
    } else {
        json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": record.phone,
            "type": "text",
            "text": {
                "preview_url": false,
                "body": record.body,
            },
        })
    }
}

/// Attempt delivery of one queued message via the Meta Graph API.
/// Runs as a background task spawned by the send handler, so the
/// handler can return the queued record immediately. Updates the
/// stored record to `sent` (with Meta's message id) or `failed`.
/// Never logs the access token; failure reasons carry only the HTTP
/// status, never the response body.
pub(crate) async fn attempt_delivery(state: Arc<WhatsAppState>, message_id: String) {
    let Some(record) = state.get_message(&message_id) else {
        return;
    };
    if matches!(
        record.status.as_str(),
        statuses::SENT | statuses::DELIVERED | statuses::READ
    ) {
        return;
    }
    let config = state.config_snapshot();
    let (Some(token), Some(phone_number_id)) = (config.access_token, config.phone_number_id) else {
        state.update_status(
            &message_id,
            statuses::PENDING_CONFIG,
            Some(
                "WhatsApp credentials (WHATSAPP_ACCESS_TOKEN and \
                 WHATSAPP_PHONE_NUMBER_ID) are not configured; the message is \
                 queued and delivery was not attempted."
                    .to_string(),
            ),
        );
        return;
    };
    let url = format!(
        "{}/{}/{}/messages",
        config.graph_api_base.trim_end_matches('/'),
        config.graph_api_version,
        phone_number_id
    );
    let payload = build_graph_payload(&record);
    let response = state
        .http
        .post(&url)
        .bearer_auth(token)
        .json(&payload)
        .timeout(Duration::from_secs(15))
        .send()
        .await;
    match response {
        Ok(resp) if resp.status().is_success() => {
            let external_id = resp.json::<Value>().await.ok().and_then(|body| {
                body.get("messages")
                    .and_then(|messages| messages.get(0))
                    .and_then(|message| message.get("id"))
                    .and_then(|id| id.as_str())
                    .map(|id| id.to_string())
            });
            state.mark_sent(&message_id, external_id);
        }
        Ok(resp) => {
            state.update_status(
                &message_id,
                statuses::FAILED,
                Some(format!(
                    "Meta Graph API rejected the message (HTTP {})",
                    resp.status().as_u16()
                )),
            );
        }
        Err(_) => {
            state.update_status(
                &message_id,
                statuses::FAILED,
                Some("network error while calling the Meta Graph API".to_string()),
            );
        }
    }
}
