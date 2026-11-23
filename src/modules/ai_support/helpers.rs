// Shared helpers: callers, responses, grounding.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse};
use chrono::Utc;
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn actor_id(req: &HttpRequest) -> UnifiedResult<String> {
    Ok(unified_core::caller::require_caller(req)?.user_id)
}

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

pub(crate) fn err(error: UnifiedError) -> HttpResponse {
    json::error_response(&error, false)
}

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

pub(crate) fn load_conversation(
    state: &AiSupportState,
    id: Uuid,
    actor: &str,
) -> UnifiedResult<Conversation> {
    let conversation = state
        .conversations
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Conversation not found: {id}")))?;
    if conversation.user_id != actor {
        return Err(UnifiedError::forbidden(
            "Conversation belongs to a different user",
        ));
    }
    Ok(conversation)
}

pub(crate) fn push_message(
    state: &AiSupportState,
    conversation_id: Uuid,
    role: MessageRole,
    content: String,
    sources: Vec<ArticleSource>,
) -> ConversationMessage {
    let message = ConversationMessage {
        id: Uuid::now_v7(),
        conversation_id,
        role,
        content,
        sources,
        created_at: Utc::now(),
    };
    // One atomic entry operation: the previous get_mut-then-insert
    // lost a message when two pushes raced on a new conversation
    // (both took the None branch and the second insert overwrote
    // the first). `entry().or_default()` creates the vec once and
    // appends under the shard lock, so no push is ever lost.
    state
        .messages
        .entry(conversation_id)
        .or_default()
        .push(message.clone());
    message
}

/// Move a conversation to `target` under the lifecycle table in
/// `ConversationStatus::can_transition`. A same-status move is a
/// no-op. Anything else is rejected with 409 Conflict naming the
/// allowed targets, so no handler can assign an arbitrary status.
pub(crate) fn change_conversation_status(
    conversation: &mut Conversation,
    target: ConversationStatus,
) -> UnifiedResult<()> {
    if conversation.status == target {
        return Ok(());
    }
    if !ConversationStatus::can_transition(conversation.status, target) {
        let allowed: Vec<&str> = ConversationStatus::allowed_transitions(conversation.status)
            .iter()
            .map(|status| status.as_str())
            .collect();
        return Err(UnifiedError::conflict(format!(
            "Cannot move conversation from {} to {}; allowed: {}",
            conversation.status.as_str(),
            target.as_str(),
            allowed.join(", ")
        )));
    }
    conversation.status = target;
    Ok(())
}

// ---------------------------------------------------------------------------
// Connector authorization and config redaction
// ---------------------------------------------------------------------------
//
// Connectors are owned per user, like conversations: the creator is
// the owner and only the owner — or a platform admin (`admin` /
// `super_admin` role) — may read, update or delete one. Every
// connector operation additionally requires the
// `ai-support.connectors.manage` capability, checked through the
// `role.check` hook exactly like the other capability-checked
// modules in this codebase; it fails closed (no bus, no handler, or
// no grant means refused with 403). The config is stored verbatim
// internally but every response carries a redacted copy, so channel
// tokens and API keys never leave the server in a response body.

/// Capability every connector operation (reads and writes alike)
/// requires, checked through the `role.check` hook.
pub(crate) const PERM_CONNECTORS_MANAGE: &str = "ai-support.connectors.manage";

/// Platform admin roles, which may manage any user's connector.
/// (Same built-in roles as the network tools module: `admin` and
/// `super_admin` hold everything.)
pub(crate) const ADMIN_ROLES: &[&str] = &["admin", "super_admin"];

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for the capability check),
/// the same shape the other capability-checked modules use. No
/// verified caller is 401.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds
/// `ai-support.connectors.manage` through the `role.check` hook.
/// Fails closed: no bus, no handler, or no grant means not allowed.
pub(crate) fn check_connector_capability(state: &AiSupportState, role: &str) -> bool {
    state
        .emit(
            "role.check",
            json!({
                "role": role,
                "permission": PERM_CONNECTORS_MANAGE,
                "allowed": false,
            }),
        )
        .and_then(|ctx| ctx.get("allowed").and_then(|value| value.as_bool()))
        .unwrap_or(false)
}

pub(crate) fn require_connector_manage(state: &AiSupportState, role: &str) -> UnifiedResult<()> {
    if check_connector_capability(state, role) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{PERM_CONNECTORS_MANAGE}\""
        )))
    }
}

/// Resolve the caller and require the connector manage capability
/// in one step. Missing identity headers are 401; a caller without
/// the capability is 403.
pub(crate) fn authorize_connectors(
    state: &AiSupportState,
    req: &HttpRequest,
) -> UnifiedResult<Caller> {
    let found = caller(req)?;
    require_connector_manage(state, &found.role)?;
    Ok(found)
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 4, group E).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// Records in this module carry no company field, so the guard is a
/// fail-closed gate only — there is no per-record company check to
/// apply. Real partitioning needs a `tenant_id` on the record, a
/// C11 follow-up, not a C3 gate.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// — a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources.
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified),
    }
}

/// Writes require a resolved company: a mutating handler with no
/// resolvable company scope is refused (403). Reads proceed on the
/// tenant gate plus the mismatch check above — records here carry
/// no company field, so there is nothing to partition on read.
pub(crate) fn require_caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<String> {
    caller_company(tenant, req)?
        .ok_or_else(|| UnifiedError::forbidden("A write requires a resolved company scope"))
}

/// True when `caller` may see `connector`: its owner, or a
/// platform admin.
pub(crate) fn connector_visible_to(connector: &Connector, caller: &Caller) -> bool {
    connector.owner_id == caller.actor || ADMIN_ROLES.contains(&caller.role.as_str())
}

/// Refuse with 403 when `caller` may not see `connector`.
pub(crate) fn require_connector_access(
    connector: &Connector,
    caller: &Caller,
) -> UnifiedResult<()> {
    if connector_visible_to(connector, caller) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Connector belongs to a different user",
        ))
    }
}

/// True when a config key plausibly holds a secret. Matches whole
/// secret words and common secret compounds (`token`, `secret`,
/// `password`, `api_key`, `private_key`, `credential`, `bearer`,
/// a bare `key` / `auth`, or a `*_key` / `*_auth` suffix) without
/// flagging innocent words such as `author`.
pub(crate) fn is_secret_config_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    if lower.contains("token")
        || lower.contains("secret")
        || lower.contains("password")
        || lower.contains("passwd")
        || lower.contains("api_key")
        || lower.contains("apikey")
        || lower.contains("api-key")
        || lower.contains("private_key")
        || lower.contains("private-key")
        || lower.contains("credential")
        || lower.contains("bearer")
        || lower.contains("access_key")
        || lower.contains("access-key")
    {
        return true;
    }
    lower == "key" || lower == "auth" || lower.ends_with("_key") || lower.ends_with("-key")
        || lower.ends_with("_auth")
        || lower.ends_with("-auth")
}

/// Redact secret-bearing fields in a connector config, recursively.
/// Secret values become `"********"`; every other value is kept
/// verbatim. The stored config is never modified — callers only
/// ever receive the redacted copy.
pub(crate) fn mask_config_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (key, item) in map {
                if is_secret_config_key(key) {
                    out.insert(key.clone(), serde_json::Value::String("********".to_string()));
                } else {
                    out.insert(key.clone(), mask_config_value(item));
                }
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(mask_config_value).collect())
        }
        other => other.clone(),
    }
}

/// A copy of `connector` whose config has secret-bearing fields
/// masked. Use for every connector returned in a response.
pub(crate) fn redact_connector(connector: &Connector) -> Connector {
    let mut redacted = connector.clone();
    redacted.config = mask_config_value(&connector.config);
    redacted
}

/// Parse `knowledgebase.search` hook results into grounding sources. The
/// knowledgebase handler contributes `{ "articles": [...] }`; any other
/// shape is ignored rather than trusted.
pub(crate) fn parse_sources(ctx: &unified_core::hooks::HookContext) -> Vec<ArticleSource> {
    let mut sources: Vec<ArticleSource> = Vec::new();
    for result in &ctx.results {
        let articles = match result.get("articles").and_then(|value| value.as_array()) {
            Some(articles) => articles.clone(),
            None => match result.as_array() {
                Some(articles) => articles.clone(),
                None => continue,
            },
        };
        for article in articles {
            let article_id = article
                .get("id")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let title = article
                .get("title")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            if article_id.is_empty() || title.is_empty() {
                continue;
            }
            sources.push(ArticleSource {
                article_id,
                title,
                url: article
                    .get("url")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string(),
                score: article
                    .get("score")
                    .and_then(|value| value.as_f64())
                    .unwrap_or(0.0),
            });
        }
    }
    sources
}

/// Build the assistant answer from grounding sources only. The answer
/// cites the articles and points at them; it never invents content beyond
/// what the articles returned.
pub(crate) fn grounded_answer(sources: &[ArticleSource]) -> String {
    let mut answer = String::from(
        "Based on the knowledgebase, the following articles are relevant to your question:\n",
    );
    for (index, source) in sources.iter().enumerate() {
        answer.push_str(&format!(
            "{}. {} ({})\n",
            index + 1,
            source.title,
            source.url
        ));
    }
    answer.push_str(
        "\nOpen an article above for the full answer. If none of these cover your \
         question, ask for a handoff and a human agent will pick it up.",
    );
    answer
}

pub(crate) fn no_articles_answer() -> String {
    String::from(
        "I could not find any knowledgebase articles matching your question, so I \
         cannot give a grounded answer. You can rephrase your question, or ask for \
         a handoff and a human support agent will help you.",
    )
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;
    use std::collections::HashMap;

    fn params(page: u32, per_page: u32) -> PaginationParams {
        PaginationParams {
            page: Some(page),
            per_page: Some(per_page),
            sort: None,
            order: None,
            search: None,
            filters: HashMap::new(),
        }
    }

    #[test]
    fn parse_uuid_accepts_valid_and_rejects_invalid() {
        let id = uuid::Uuid::now_v7();
        assert_eq!(parse_uuid(&id.to_string(), "conversation").unwrap(), id);
        assert!(parse_uuid("not-a-uuid", "conversation").is_err());
        assert!(parse_uuid("", "conversation").is_err());
    }

    #[test]
    fn require_text_trims_and_validates() {
        assert_eq!(require_text("  hello  ", "message", 10).unwrap(), "hello");
        assert_eq!(require_text("abc", "message", 3).unwrap(), "abc");
        assert!(require_text("", "message", 10).is_err());
        assert!(require_text("   ", "message", 10).is_err());
        assert!(require_text("toolong", "message", 3).is_err());
    }

    #[test]
    fn grounded_answer_cites_sources_in_order() {
        let sources = vec![
            ArticleSource {
                article_id: "a1".to_string(),
                title: "First".to_string(),
                url: "https://example.test/a1".to_string(),
                score: 0.9,
            },
            ArticleSource {
                article_id: "a2".to_string(),
                title: "Second".to_string(),
                url: "https://example.test/a2".to_string(),
                score: 0.5,
            },
        ];
        let answer = grounded_answer(&sources);
        assert!(answer.contains("1. First (https://example.test/a1)"));
        assert!(answer.contains("2. Second (https://example.test/a2)"));
        assert!(answer.contains("handoff"));
        let empty = grounded_answer(&[]);
        assert!(empty.contains("Based on the knowledgebase"));
        assert!(empty.contains("handoff"));
    }

    #[test]
    fn no_articles_answer_mentions_handoff() {
        let answer = no_articles_answer();
        assert!(answer.contains("could not find any knowledgebase articles"));
        assert!(answer.contains("handoff"));
    }

    #[test]
    fn paginate_slices_pages() {
        let page = paginate(vec![1, 2, 3, 4, 5], &params(2, 2));
        assert_eq!(page.items, vec![3, 4]);
        assert_eq!(page.pagination.total, 5);
        assert_eq!(page.pagination.total_pages, 3);
        assert_eq!(page.pagination.page, 2);
        let beyond = paginate(vec![1, 2, 3], &params(5, 2));
        assert!(beyond.items.is_empty());
        assert_eq!(beyond.pagination.total, 3);
    }

    #[test]
    fn load_conversation_enforces_ownership() {
        let state = AiSupportState::default();
        let id = uuid::Uuid::now_v7();
        let now = chrono::Utc::now();
        state.conversations.insert(
            id,
            Conversation {
                id,
                user_id: "user-1".to_string(),
                title: None,
                status: ConversationStatus::Open,
                created_at: now,
                updated_at: now,
            },
        );
        let loaded = load_conversation(&state, id, "user-1").expect("owner loads");
        assert_eq!(loaded.id, id);
        assert!(load_conversation(&state, id, "user-2").is_err());
        assert!(load_conversation(&state, uuid::Uuid::now_v7(), "user-1").is_err());
    }

    #[test]
    fn push_message_appends_to_conversation() {
        let state = AiSupportState::default();
        let conversation_id = uuid::Uuid::now_v7();
        let first = push_message(
            &state,
            conversation_id,
            MessageRole::User,
            "hello".to_string(),
            vec![],
        );
        assert_eq!(first.conversation_id, conversation_id);
        assert_eq!(first.role, MessageRole::User);
        assert_eq!(first.content, "hello");
        push_message(
            &state,
            conversation_id,
            MessageRole::Assistant,
            "hi there".to_string(),
            vec![],
        );
        let stored = state.messages.get(&conversation_id).expect("messages stored");
        assert_eq!(stored.value().len(), 2);
        assert_eq!(stored.value()[1].content, "hi there");
    }

    #[test]
    fn push_message_concurrent_pushes_to_a_new_conversation_lose_nothing() {
        // Two (here eight) concurrent pushes to a brand-new
        // conversation must all survive: the old get_mut/insert
        // check-then-act dropped one of them.
        let state = AiSupportState::default();
        let conversation_id = uuid::Uuid::now_v7();
        std::thread::scope(|scope| {
            for index in 0..8 {
                let state = &state;
                scope.spawn(move || {
                    push_message(
                        state,
                        conversation_id,
                        MessageRole::User,
                        format!("message {index}"),
                        vec![],
                    );
                });
            }
        });
        let stored = state
            .messages
            .get(&conversation_id)
            .expect("messages stored");
        assert_eq!(stored.value().len(), 8);
        let mut contents: Vec<String> = stored
            .value()
            .iter()
            .map(|message| message.content.clone())
            .collect();
        contents.sort();
        assert_eq!(
            contents,
            (0..8).map(|index| format!("message {index}")).collect::<Vec<_>>()
        );
    }

    #[test]
    fn change_conversation_status_enforces_the_table() {
        let now = chrono::Utc::now();
        let mut conversation = Conversation {
            id: uuid::Uuid::now_v7(),
            user_id: "user-1".to_string(),
            title: None,
            status: ConversationStatus::Open,
            created_at: now,
            updated_at: now,
        };
        // Same status is a no-op.
        assert!(change_conversation_status(&mut conversation, ConversationStatus::Open).is_ok());
        assert_eq!(conversation.status, ConversationStatus::Open);
        // Allowed: open -> handoff -> closed.
        assert!(change_conversation_status(&mut conversation, ConversationStatus::Handoff).is_ok());
        assert_eq!(conversation.status, ConversationStatus::Handoff);
        assert!(change_conversation_status(&mut conversation, ConversationStatus::Closed).is_ok());
        assert_eq!(conversation.status, ConversationStatus::Closed);
        // Rejected: closed -> handoff (409).
        let error =
            change_conversation_status(&mut conversation, ConversationStatus::Handoff).unwrap_err();
        assert_eq!(error.code, 409);
        assert_eq!(conversation.status, ConversationStatus::Closed);
        // Allowed: closed -> open is the explicit reopen.
        assert!(change_conversation_status(&mut conversation, ConversationStatus::Open).is_ok());
        assert_eq!(conversation.status, ConversationStatus::Open);
        // Rejected from open: handoff -> open is covered via handoff state.
        change_conversation_status(&mut conversation, ConversationStatus::Handoff)
            .expect("open -> handoff");
        let error =
            change_conversation_status(&mut conversation, ConversationStatus::Open).unwrap_err();
        assert_eq!(error.code, 409);
        assert_eq!(conversation.status, ConversationStatus::Handoff);
    }

    #[test]
    fn mask_config_value_redacts_secrets_and_keeps_the_rest() {
        let config = serde_json::json!({
            "webhook_url": "https://example.test/hook",
            "bot_token": "tok-secret-123",
            "api_key": "key-secret-456",
            "client_secret": "shh",
            "password": "hunter2",
            "author": "Ada Lovelace",
            "nested": {"access_token": "tok-789", "label": "plain"},
            "items": [{"private_key": "pk-1"}, {"name": "plain"}]
        });
        let masked = mask_config_value(&config);
        assert_eq!(masked["webhook_url"], "https://example.test/hook");
        assert_eq!(masked["bot_token"], "********");
        assert_eq!(masked["api_key"], "********");
        assert_eq!(masked["client_secret"], "********");
        assert_eq!(masked["password"], "********");
        assert_eq!(masked["author"], "Ada Lovelace");
        assert_eq!(masked["nested"]["access_token"], "********");
        assert_eq!(masked["nested"]["label"], "plain");
        assert_eq!(masked["items"][0]["private_key"], "********");
        assert_eq!(masked["items"][1]["name"], "plain");
        // The stored value is untouched by masking.
        assert_eq!(config["bot_token"], "tok-secret-123");
    }

    #[test]
    fn caller_requires_a_verified_caller() {
        use actix_web::test::TestRequest;
        let req = verified_request("user-1", "manager");
        let found = caller(&req).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "manager");
        // Anonymous: no headers at all.
        assert!(caller(&TestRequest::default().to_http_request()).is_err());
        assert!(caller(
            &TestRequest::default().to_http_request()
        )
        .is_err());
        assert!(caller(
            &TestRequest::default().to_http_request()
        )
        .is_err());
    }

    #[test]
    fn connector_capability_check_fails_closed_without_a_role_check_handler() {
        // No bus attached (and no handler): the preset
        // `"allowed": false` must stand and the call is refused.
        let state = AiSupportState::default();
        assert!(!check_connector_capability(&state, "admin"));
        assert!(require_connector_manage(&state, "admin").is_err());
        let req = actix_web::test::verified_request("user-1", "admin");
        assert!(authorize_connectors(&state, &req).is_err());
    }

    #[test]
    fn connector_access_is_owner_or_platform_admin_only() {
        let now = chrono::Utc::now();
        let connector = Connector {
            id: uuid::Uuid::now_v7(),
            name: "support".to_string(),
            kind: ConnectorKind::Telegram,
            enabled: true,
            config: serde_json::json!({}),
            owner_id: "user-1".to_string(),
            created_at: now,
            updated_at: now,
        };
        let owner = Caller {
            actor: "user-1".to_string(),
            role: "manager".to_string(),
        };
        let other = Caller {
            actor: "user-2".to_string(),
            role: "manager".to_string(),
        };
        let admin = Caller {
            actor: "user-9".to_string(),
            role: "admin".to_string(),
        };
        assert!(connector_visible_to(&connector, &owner));
        assert!(require_connector_access(&connector, &owner).is_ok());
        assert!(!connector_visible_to(&connector, &other));
        assert!(require_connector_access(&connector, &other).is_err());
        assert!(connector_visible_to(&connector, &admin));
        assert!(require_connector_access(&connector, &admin).is_ok());
    }

    #[test]
    fn parse_sources_keeps_only_articles_with_id_and_title() {
        let mut ctx = unified_core::hooks::HookContext::new(
            "knowledgebase.search",
            serde_json::json!({}),
        );
        ctx.results.push(serde_json::json!({
            "articles": [
                {"id": "a1", "title": "Good", "url": "https://example.test/a1", "score": 0.9},
                {"id": "", "title": "No id"},
                {"id": "a2", "title": ""},
                {"title": "No id field"}
            ]
        }));
        // A bare array result is accepted too (fallback shape).
        ctx.results.push(serde_json::json!([
            {"id": "a3", "title": "Bare", "url": "", "score": 0.1}
        ]));
        // Anything else is ignored.
        ctx.results.push(serde_json::json!({"unrelated": true}));
        let sources = parse_sources(&ctx);
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].article_id, "a1");
        assert_eq!(sources[0].title, "Good");
        assert_eq!(sources[1].article_id, "a3");
    }

    /// Build a request carrying a resolved tenant context plus a
    /// verified caller, for the tenant-gate tests below.
    fn tenant_request(company_id: &str, caller_company: Option<&str>) -> HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        let verified = match caller_company {
            Some(company) => unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
                .with_company(company.to_string()),
            None => unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new()),
        };
        req.extensions_mut().insert(verified);
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: company_id.to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn caller_company_prefers_tenant_company() {
        let company = uuid::Uuid::now_v7().to_string();
        let req = tenant_request(&company, Some(&company));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some(company.clone())
        );
        // No tenant company: the verified caller's company is the scope.
        let req = tenant_request("", Some(&company));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(caller_company(&tenant, &req).unwrap(), Some(company));
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = tenant_request(
            &uuid::Uuid::now_v7().to_string(),
            Some(&uuid::Uuid::now_v7().to_string()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn require_caller_company_refuses_writes_without_a_resolved_company() {
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(require_caller_company(&tenant, &req).is_err());
        let company = uuid::Uuid::now_v7().to_string();
        let req = tenant_request(&company, Some(&company));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(require_caller_company(&tenant, &req).unwrap(), company);
    }
}
