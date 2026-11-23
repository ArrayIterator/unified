//! WhatsApp external module (split from lib.rs; moved verbatim).

use super::*;

// ---------------------------------------------------------------------------
// HTTP handlers. Every response uses the standard envelope:
// success -> {"data": ...} via `success_response`, error -> the standard
// error shape via `error_response`. The single exception is the webhook
// verification GET, which must echo Meta's `hub.challenge` as raw text.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct SendRequest {
    pub(crate) tenant_id: Option<String>,
    pub(crate) to: Option<String>,
    #[serde(rename = "type")]
    pub(crate) kind: Option<String>,
    pub(crate) body: Option<String>,
    pub(crate) template_name: Option<String>,
    pub(crate) template_language: Option<String>,
    pub(crate) template_variables: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub(crate) struct CreateTemplateRequest {
    pub(crate) tenant_id: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) language: Option<String>,
    pub(crate) category: Option<String>,
    pub(crate) body: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct TenantQuery {
    pub(crate) tenant_id: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct MessagesQuery {
    pub(crate) tenant_id: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) phone: Option<String>,
    pub(crate) page: Option<u32>,
    pub(crate) per_page: Option<u32>,
}

#[derive(Deserialize)]
pub(crate) struct TemplatesQuery {
    pub(crate) tenant_id: Option<String>,
    pub(crate) category: Option<String>,
    pub(crate) language: Option<String>,
    pub(crate) page: Option<u32>,
    pub(crate) per_page: Option<u32>,
}

#[derive(Deserialize)]
pub(crate) struct ConversationsQuery {
    pub(crate) tenant_id: Option<String>,
    pub(crate) page: Option<u32>,
    pub(crate) per_page: Option<u32>,
}

#[derive(Deserialize)]
pub(crate) struct WebhookVerifyQuery {
    #[serde(rename = "hub.mode")]
    pub(crate) mode: Option<String>,
    #[serde(rename = "hub.verify_token")]
    pub(crate) verify_token: Option<String>,
    #[serde(rename = "hub.challenge")]
    pub(crate) challenge: Option<String>,
}

/// Validate a `tenant_id` query/body value. Missing or malformed ids
/// are rejected; there is no fallback to a global view.
fn require_tenant_id(value: &Option<String>) -> Result<String, UnifiedError> {
    match value {
        Some(id) if is_valid_tenant_id(id) => Ok(id.clone()),
        Some(_) => Err(UnifiedError::bad_request(
            "tenant_id is invalid: use 1-128 letters, digits, '-', '_' or '.'",
        )),
        None => Err(UnifiedError::bad_request("tenant_id is required")),
    }
}

/// Normalise pagination parameters: page starts at 1, per_page is
/// clamped to 1..=100 and defaults to 20.
fn pagination(page: Option<u32>, per_page: Option<u32>) -> (usize, usize) {
    let page = page.unwrap_or(1).max(1) as usize;
    let per_page = per_page.unwrap_or(20).clamp(1, 100) as usize;
    (page, per_page)
}

fn paginate<T: Clone>(items: Vec<T>, page: usize, per_page: usize) -> (Vec<T>, usize) {
    let total = items.len();
    let start = (page - 1).saturating_mul(per_page);
    let page_items = items.into_iter().skip(start).take(per_page).collect();
    (page_items, total)
}

/// `POST /api/v1/whatsapp/send` — queue an outbound text or template
/// message. Returns 202 with the queued record. When credentials are
/// configured, delivery is attempted in the background; when they are
/// not, the message stays `pending_config` and the response says so
/// (it is never reported as sent).
pub(crate) async fn send_handler(
    state: web::Data<Arc<WhatsAppState>>,
    body: web::Json<SendRequest>,
) -> HttpResponse {
    // `Data<Arc<T>>::into_inner()` unwraps to `Arc<Arc<T>>` (the Data
    // holds an `Arc` of its `T`, here already an `Arc`), so take the
    // inner `Arc<WhatsAppState>` directly via `get_ref()` instead.
    let state = Arc::clone(state.get_ref());
    let request = body.into_inner();

    let tenant_id = match require_tenant_id(&request.tenant_id) {
        Ok(id) => id,
        Err(error) => return error_response(&error, false),
    };
    let to = match request.to.as_deref() {
        Some(phone) if is_valid_e164(phone) => phone.to_string(),
        _ => {
            return error_response(
                &UnifiedError::bad_request(
                    "to must be a valid E.164 phone number, for example +6281234567890",
                ),
                false,
            );
        }
    };
    let kind = match request.kind.as_deref() {
        Some("text") => "text".to_string(),
        Some("template") => "template".to_string(),
        _ => {
            return error_response(
                &UnifiedError::bad_request("type must be \"text\" or \"template\""),
                false,
            );
        }
    };

    let mut template_name: Option<String> = None;
    let mut template_language: Option<String> = None;
    let mut template_variables: Vec<String> = Vec::new();
    let mut text_body: Option<String> = None;

    if kind == "text" {
        match request.body.as_deref() {
            Some(body) if !body.trim().is_empty() && body.chars().count() <= MAX_TEXT_LEN => {
                text_body = Some(body.to_string());
            }
            _ => {
                return error_response(
                    &UnifiedError::bad_request(
                        "body is required for text messages (1-4096 characters)",
                    ),
                    false,
                );
            }
        }
    } else {
        let name = match request.template_name.as_deref() {
            Some(name) if is_valid_template_name(name) => name.to_string(),
            _ => {
                return error_response(
                    &UnifiedError::bad_request(
                        "template_name is required and must be lowercase letters, digits or '_'",
                    ),
                    false,
                );
            }
        };
        let language = request
            .template_language
            .clone()
            .unwrap_or_else(|| DEFAULT_TEMPLATE_LANGUAGE.to_string());
        if !is_valid_language_code(&language) {
            return error_response(
                &UnifiedError::bad_request(
                    "template_language must be a language code such as en_US",
                ),
                false,
            );
        }
        let variables = request.template_variables.clone().unwrap_or_default();
        if variables.len() > MAX_TEMPLATE_VARIABLES {
            return error_response(
                &UnifiedError::bad_request("at most 20 template variables are allowed"),
                false,
            );
        }
        if variables.iter().any(|v| v.chars().count() > 1024) {
            return error_response(
                &UnifiedError::bad_request(
                    "each template variable must be at most 1024 characters",
                ),
                false,
            );
        }
        let template = match state.get_template(&tenant_id, &name, &language) {
            Some(template) => template,
            None => {
                return error_response(
                    &UnifiedError::not_found(
                        "template not found for this tenant (create it first via POST /api/v1/whatsapp/templates)",
                    ),
                    false,
                );
            }
        };
        match count_template_variables(&template.body) {
            Ok(expected) if expected == variables.len() => {}
            Ok(expected) => {
                return error_response(
                    &UnifiedError::bad_request(format!(
                        "template \"{name}\" expects {expected} variable(s), got {}",
                        variables.len()
                    )),
                    false,
                );
            }
            Err(reason) => {
                return error_response(&UnifiedError::internal(reason), false);
            }
        }
        template_name = Some(name);
        template_language = Some(language);
        template_variables = variables;
    }

    if let Err(error) = state.check_rate_limit(&tenant_id) {
        return error_response(&error, false);
    }

    let configured = state.config_snapshot().is_delivery_configured();
    let now = now_rfc3339();
    let record = MessageRecord {
        id: new_wamid(),
        tenant_id,
        direction: "outbound".to_string(),
        phone: to,
        kind,
        body: text_body,
        template_name,
        template_language,
        template_variables,
        status: if configured {
            statuses::QUEUED.to_string()
        } else {
            statuses::PENDING_CONFIG.to_string()
        },
        status_reason: if configured {
            None
        } else {
            Some(
                "WhatsApp credentials (WHATSAPP_ACCESS_TOKEN and \
                 WHATSAPP_PHONE_NUMBER_ID) are not configured; the message is \
                 queued and delivery was not attempted."
                    .to_string(),
            )
        },
        external_id: None,
        created_at: now.clone(),
        updated_at: now,
    };
    // Keep the id stable for the background task.
    let message_id = record.id.clone();
    state.insert_message(record.clone());
    if configured {
        let worker_state = state.clone();
        tokio::spawn(async move {
            attempt_delivery(worker_state, message_id).await;
        });
    }
    // The record is returned as queued / pending_config, never as sent.
    success_response(record, 202)
}

/// `GET /api/v1/whatsapp/templates` — list a tenant's templates.
pub(crate) async fn list_templates_handler(
    state: web::Data<Arc<WhatsAppState>>,
    query: web::Query<TemplatesQuery>,
) -> HttpResponse {
    let state = state.into_inner();
    let query = query.into_inner();
    let tenant_id = match require_tenant_id(&query.tenant_id) {
        Ok(id) => id,
        Err(error) => return error_response(&error, false),
    };
    if let Some(category) = &query.category {
        if !TEMPLATE_CATEGORIES.contains(&category.as_str()) {
            return error_response(
                &UnifiedError::bad_request(
                    "category must be utility, marketing, or authentication",
                ),
                false,
            );
        }
    }
    if let Some(language) = &query.language {
        if !is_valid_language_code(language) {
            return error_response(
                &UnifiedError::bad_request("language must be a language code such as en_US"),
                false,
            );
        }
    }
    let (page, per_page) = pagination(query.page, query.per_page);
    let (items, total) = paginate(
        state.list_templates(
            &tenant_id,
            query.category.as_deref(),
            query.language.as_deref(),
        ),
        page,
        per_page,
    );
    success_response(
        json!({"items": items, "page": page, "per_page": per_page, "total": total}),
        200,
    )
}

/// `POST /api/v1/whatsapp/templates` — create a template (admin data,
/// not code). Returns 201 with the created template.
pub(crate) async fn create_template_handler(
    state: web::Data<Arc<WhatsAppState>>,
    body: web::Json<CreateTemplateRequest>,
) -> HttpResponse {
    let state = state.into_inner();
    let request = body.into_inner();
    let tenant_id = match require_tenant_id(&request.tenant_id) {
        Ok(id) => id,
        Err(error) => return error_response(&error, false),
    };
    let name = match request.name.as_deref() {
        Some(name) if is_valid_template_name(name) => name.to_string(),
        _ => {
            return error_response(
                &UnifiedError::bad_request(
                    "name is required and must be lowercase letters, digits or '_'",
                ),
                false,
            );
        }
    };
    let language = match request.language.as_deref() {
        Some(language) if is_valid_language_code(language) => language.to_string(),
        _ => {
            return error_response(
                &UnifiedError::bad_request("language must be a language code such as en_US"),
                false,
            );
        }
    };
    let category = match request.category.as_deref() {
        Some(category) if TEMPLATE_CATEGORIES.contains(&category) => category.to_string(),
        _ => {
            return error_response(
                &UnifiedError::bad_request(
                    "category must be utility, marketing, or authentication",
                ),
                false,
            );
        }
    };
    let template_body = match request.body.as_deref() {
        Some(body) if !body.trim().is_empty() && body.chars().count() <= MAX_TEXT_LEN => {
            body.to_string()
        }
        _ => {
            return error_response(
                &UnifiedError::bad_request("body is required (1-4096 characters)"),
                false,
            );
        }
    };
    match count_template_variables(&template_body) {
        Ok(count) if count <= MAX_TEMPLATE_VARIABLES => {}
        Ok(_) => {
            return error_response(
                &UnifiedError::bad_request("at most 20 template variables are allowed"),
                false,
            );
        }
        Err(reason) => return error_response(&UnifiedError::bad_request(reason), false),
    }
    if state.get_template(&tenant_id, &name, &language).is_some() {
        return error_response(
            &UnifiedError::conflict(format!(
                "template \"{name}\" already exists for this tenant in language {language}"
            )),
            false,
        );
    }
    let template = Template {
        tenant_id,
        name,
        language,
        category,
        body: template_body,
        created_at: now_rfc3339(),
        updated_at: now_rfc3339(),
    };
    state.insert_template(template.clone());
    success_response(template, 201)
}

/// `GET /api/v1/whatsapp/webhook` — Meta webhook verification. Echoes
/// `hub.challenge` as raw text only when `hub.mode=subscribe` and the
/// verify token matches (constant-time compare). This is the one
/// handler that does not return the JSON envelope: Meta requires the
/// raw challenge string.
pub(crate) async fn webhook_verify_handler(
    state: web::Data<Arc<WhatsAppState>>,
    query: web::Query<WebhookVerifyQuery>,
) -> HttpResponse {
    let state = state.into_inner();
    let query = query.into_inner();
    let config = state.config_snapshot();
    let mode_ok = query.mode.as_deref() == Some("subscribe");
    let challenge_ok = query
        .challenge
        .as_ref()
        .is_some_and(|challenge| !challenge.is_empty() && challenge.len() <= 256);
    let token_ok = match (&config.verify_token, &query.verify_token) {
        (Some(expected), Some(provided)) => {
            constant_time_eq(expected.as_bytes(), provided.as_bytes())
        }
        _ => false,
    };
    if mode_ok && token_ok && challenge_ok {
        HttpResponse::Ok()
            .content_type("text/plain")
            .body(query.challenge.unwrap())
    } else {
        error_response(
            &UnifiedError::forbidden("webhook verification failed"),
            false,
        )
    }
}

/// `POST /api/v1/whatsapp/webhook` — Meta event webhook. The
/// X-Hub-Signature-256 HMAC-SHA256 signature is verified against the
/// app secret BEFORE the body is parsed; a missing app secret, a
/// missing signature, or a bad signature is rejected with 401.
/// Inbound messages are stored (status `received`) and delivery
/// statuses update the matching outbound record; both emit hook events.
pub(crate) async fn webhook_post_handler(
    state: web::Data<Arc<WhatsAppState>>,
    req: HttpRequest,
    body: web::Bytes,
) -> HttpResponse {
    let state = state.into_inner();
    if body.len() > MAX_WEBHOOK_BODY_BYTES {
        return error_response(
            &UnifiedError::new("webhook payload exceeds 1 MiB", 413),
            false,
        );
    }
    let config = state.config_snapshot();
    let Some(app_secret) = config.app_secret else {
        return error_response(
            &UnifiedError::unauthorized(
                "webhook signature verification is not configured (WHATSAPP_APP_SECRET)",
            ),
            false,
        );
    };
    let signature = req
        .headers()
        .get("X-Hub-Signature-256")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let Some(signature_hex) = signature.strip_prefix("sha256=") else {
        return error_response(
            &UnifiedError::unauthorized("missing X-Hub-Signature-256 header"),
            false,
        );
    };
    if !verify_hmac_sha256_hex(app_secret.as_bytes(), &body, signature_hex) {
        return error_response(
            &UnifiedError::unauthorized("invalid webhook signature"),
            false,
        );
    }
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return error_response(
                &UnifiedError::bad_request("webhook payload is not valid JSON"),
                false,
            );
        }
    };

    let mut processed_messages = 0usize;
    let mut processed_statuses = 0usize;
    if let Some(entries) = payload.get("entry").and_then(|value| value.as_array()) {
        for entry in entries {
            let Some(changes) = entry.get("changes").and_then(|value| value.as_array()) else {
                continue;
            };
            for change in changes {
                let Some(value) = change.get("value") else {
                    continue;
                };
                if let Some(messages) = value.get("messages").and_then(|v| v.as_array()) {
                    for message in messages {
                        let Some(from) = message.get("from").and_then(|v| v.as_str()) else {
                            continue;
                        };
                        if !is_valid_e164(from) {
                            continue;
                        }
                        let meta_type = message
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("text");
                        let body_text = if meta_type == "text" {
                            message
                                .get("text")
                                .and_then(|v| v.get("body"))
                                .and_then(|v| v.as_str())
                                .map(|text| text.to_string())
                        } else {
                            Some(format!("[{meta_type} message]"))
                        };
                        let Some(body_text) = body_text else {
                            continue;
                        };
                        let body_text: String = body_text.chars().take(MAX_TEXT_LEN).collect();
                        let tenant_id = state.resolve_inbound_tenant(from);
                        let record = MessageRecord {
                            id: new_wamid(),
                            tenant_id: tenant_id.clone(),
                            direction: "inbound".to_string(),
                            phone: from.to_string(),
                            kind: meta_type.to_string(),
                            body: Some(body_text),
                            template_name: None,
                            template_language: None,
                            template_variables: Vec::new(),
                            status: statuses::RECEIVED.to_string(),
                            status_reason: None,
                            external_id: message
                                .get("id")
                                .and_then(|v| v.as_str())
                                .map(|id| id.to_string()),
                            created_at: now_rfc3339(),
                            updated_at: now_rfc3339(),
                        };
                        state.insert_message(record.clone());
                        state.emit(
                            MESSAGE_RECEIVED_EVENT,
                            json!({
                                "message_id": record.id,
                                "tenant_id": record.tenant_id,
                                "phone": record.phone,
                                "kind": record.kind,
                                "body": record.body,
                            }),
                        );
                        processed_messages += 1;
                    }
                }
                if let Some(statuses) = value.get("statuses").and_then(|v| v.as_array()) {
                    for status in statuses {
                        let Some(meta_id) = status.get("id").and_then(|v| v.as_str()) else {
                            continue;
                        };
                        let Some(meta_status) = status.get("status").and_then(|v| v.as_str())
                        else {
                            continue;
                        };
                        let mapped = match meta_status {
                            "sent" => statuses::SENT,
                            "delivered" => statuses::DELIVERED,
                            "read" => statuses::READ,
                            "failed" => statuses::FAILED,
                            _ => continue,
                        };
                        let Some(existing) = state.find_by_external_id(meta_id) else {
                            continue;
                        };
                        let reason = if mapped == statuses::FAILED {
                            status
                                .get("errors")
                                .and_then(|v| v.as_array())
                                .and_then(|errors| errors.first())
                                .and_then(|error| error.get("title"))
                                .and_then(|title| title.as_str())
                                .map(|title| title.to_string())
                        } else {
                            None
                        };
                        state.update_status(&existing.id, mapped, reason);
                        state.emit(
                            MESSAGE_STATUS_EVENT,
                            json!({
                                "message_id": existing.id,
                                "external_id": meta_id,
                                "tenant_id": existing.tenant_id,
                                "status": mapped,
                            }),
                        );
                        processed_statuses += 1;
                    }
                }
            }
        }
    }
    success_response(
        json!({
            "received": true,
            "processed_messages": processed_messages,
            "processed_statuses": processed_statuses,
        }),
        200,
    )
}

/// `GET /api/v1/whatsapp/messages` — paginated message log for one
/// tenant, optionally filtered by status and/or phone.
pub(crate) async fn list_messages_handler(
    state: web::Data<Arc<WhatsAppState>>,
    query: web::Query<MessagesQuery>,
) -> HttpResponse {
    let state = state.into_inner();
    let query = query.into_inner();
    let tenant_id = match require_tenant_id(&query.tenant_id) {
        Ok(id) => id,
        Err(error) => return error_response(&error, false),
    };
    if let Some(status) = &query.status {
        if !statuses::ALL.contains(&status.as_str()) {
            return error_response(
                &UnifiedError::bad_request(
                    "status must be queued, pending_config, sent, failed, delivered, read, or received",
                ),
                false,
            );
        }
    }
    if let Some(phone) = &query.phone {
        if !is_valid_e164(phone) {
            return error_response(
                &UnifiedError::bad_request("phone must be a valid E.164 number"),
                false,
            );
        }
    }
    let (page, per_page) = pagination(query.page, query.per_page);
    let (items, total) = paginate(
        state.list_messages(&tenant_id, query.status.as_deref(), query.phone.as_deref()),
        page,
        per_page,
    );
    success_response(
        json!({"items": items, "page": page, "per_page": per_page, "total": total}),
        200,
    )
}

/// `GET /api/v1/whatsapp/messages/{id}` — one message. A message from
/// another tenant looks exactly like a missing message (404).
pub(crate) async fn get_message_handler(
    state: web::Data<Arc<WhatsAppState>>,
    path: web::Path<String>,
    query: web::Query<TenantQuery>,
) -> HttpResponse {
    let state = state.into_inner();
    let id = path.into_inner();
    let tenant_id = match require_tenant_id(&query.tenant_id) {
        Ok(id) => id,
        Err(error) => return error_response(&error, false),
    };
    match state.get_message_for_tenant(&id, &tenant_id) {
        Some(record) => success_response(record, 200),
        None => error_response(&UnifiedError::not_found("message not found"), false),
    }
}

/// `GET /api/v1/whatsapp/conversations` — messages grouped by phone
/// for one tenant: last message, message count, and unread count
/// (inbound messages still in `received` status).
pub(crate) async fn list_conversations_handler(
    state: web::Data<Arc<WhatsAppState>>,
    query: web::Query<ConversationsQuery>,
) -> HttpResponse {
    let state = state.into_inner();
    let query = query.into_inner();
    let tenant_id = match require_tenant_id(&query.tenant_id) {
        Ok(id) => id,
        Err(error) => return error_response(&error, false),
    };
    let mut grouped: HashMap<String, ConversationView> = HashMap::new();
    for record in state.list_messages(&tenant_id, None, None) {
        let entry = grouped
            .entry(record.phone.clone())
            .or_insert_with(|| ConversationView {
                phone: record.phone.clone(),
                last_message: record.clone(),
                message_count: 0,
                unread_count: 0,
                last_at: record.created_at.clone(),
            });
        entry.message_count += 1;
        if record.direction == "inbound" && record.status == statuses::RECEIVED {
            entry.unread_count += 1;
        }
        if record.created_at >= entry.last_at {
            entry.last_at = record.created_at.clone();
            entry.last_message = record.clone();
        }
    }
    let mut conversations: Vec<ConversationView> = grouped.into_values().collect();
    conversations.sort_by(|a, b| b.last_at.cmp(&a.last_at).then(a.phone.cmp(&b.phone)));
    let (page, per_page) = pagination(query.page, query.per_page);
    let (items, total) = paginate(conversations, page, per_page);
    success_response(
        json!({"items": items, "page": page, "per_page": per_page, "total": total}),
        200,
    )
}

/// `GET /api/v1/whatsapp/status` — module status. Only booleans and
/// counts are reported; no secret is ever returned.
pub(crate) async fn status_handler(state: web::Data<Arc<WhatsAppState>>) -> HttpResponse {
    let state = state.into_inner();
    let config = state.config_snapshot();
    success_response(
        json!({
            "module": "whatsapp",
            "delivery_configured": config.is_delivery_configured(),
            "has_access_token": config.access_token.is_some(),
            "has_phone_number_id": config.phone_number_id.is_some(),
            "has_verify_token": config.verify_token.is_some(),
            "has_app_secret": config.app_secret.is_some(),
            "rate_limit_per_minute": config.rate_limit_per_minute,
            "graph_api_version": config.graph_api_version,
            "messages": state.message_count(),
            "queued_messages": state.queued_count(),
            "templates": state.template_count(),
        }),
        200,
    )
}
