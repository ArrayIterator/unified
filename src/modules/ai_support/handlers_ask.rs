// Ask handler: grounded answering.

use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use super::*;

// Ask handler (grounded answering)

pub(crate) async fn ask(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    body: web::Json<AskRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let question = require_text(&body.message, "message", MAX_MESSAGE_LEN)?;
        // Resolve or create the conversation; it must belong to the caller.
        let mut conversation = match body.conversation_id {
            Some(id) => load_conversation(&state, id, &actor)?,
            None => {
                let now = Utc::now();
                let title: String = question.chars().take(80).collect();
                let conversation = Conversation {
                    id: Uuid::now_v7(),
                    user_id: actor.clone(),
                    title: Some(title),
                    status: ConversationStatus::Open,
                    created_at: now,
                    updated_at: now,
                };
                state
                    .conversations
                    .insert(conversation.id, conversation.clone());
                conversation
            }
        };
        // A closed conversation stays closed: asking a question
        // never reopens it. The caller must explicitly reopen it
        // first (PATCH the conversation `closed` -> `open`, the one
        // reopen move the lifecycle table allows); otherwise this
        // is a 409 Conflict.
        if conversation.status == ConversationStatus::Closed {
            return Err(UnifiedError::conflict(
                "Conversation is closed; reopen it before asking another question",
            ));
        }
        push_message(
            &state,
            conversation.id,
            MessageRole::User,
            question.clone(),
            Vec::new(),
        );
        // Grounding: ask the knowledgebase through the hook bus. When no
        // handler answers, `sources` stays empty and the reply says so.
        let mut sources: Vec<ArticleSource> = Vec::new();
        if let Some(ctx) = state.emit(
            "knowledgebase.search",
            json!({ "query": question, "limit": GROUNDING_LIMIT }),
        ) {
            sources = parse_sources(&ctx);
        }
        let grounded = !sources.is_empty();
        let answer = if grounded {
            grounded_answer(&sources)
        } else {
            no_articles_answer()
        };
        push_message(
            &state,
            conversation.id,
            MessageRole::Assistant,
            answer.clone(),
            sources.clone(),
        );
        conversation.updated_at = Utc::now();
        state
            .conversations
            .insert(conversation.id, conversation.clone());
        Ok(json!({
            "conversation_id": conversation.id.to_string(),
            "answer": answer,
            "sources": sources,
            "grounded": grounded,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
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
                .with_roles(vec![role.to_string()])
                .with_company("company-1".to_string()),
        );
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-1".to_string(),
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

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_company("company-1".to_string()),
        );
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-1".to_string(),
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
    use actix_web::test::TestRequest;

    fn conversation(state: &AiSupportState, status: ConversationStatus) -> Conversation {
        let now = Utc::now();
        let conversation = Conversation {
            id: Uuid::now_v7(),
            user_id: "user-1".to_string(),
            title: Some("Help".to_string()),
            status,
            created_at: now,
            updated_at: now,
        };
        state
            .conversations
            .insert(conversation.id, conversation.clone());
        conversation
    }

    #[tokio::test]
    async fn ask_on_a_closed_conversation_is_a_conflict_not_a_reopen() {
        let state = AiSupportState::default();
        let closed = conversation(&state, ConversationStatus::Closed);
        let req = verified_request_without_roles("user-1");
        let resp = ask(
            web::Data::new(state.clone()),
            req,
            web::Json(AskRequest {
                message: "hello again".to_string(),
                conversation_id: Some(closed.id),
            }),
        )
        .await;
        assert_eq!(resp.status().as_u16(), 409);
        // The conversation stays closed and gains no messages.
        let stored = state
            .conversations
            .get(&closed.id)
            .expect("conversation stored");
        assert_eq!(stored.value().status, ConversationStatus::Closed);
        assert!(state.messages.get(&closed.id).is_none());
    }

    #[tokio::test]
    async fn ask_on_an_open_conversation_answers() {
        let state = AiSupportState::default();
        let open = conversation(&state, ConversationStatus::Open);
        let req = verified_request_without_roles("user-1");
        let resp = ask(
            web::Data::new(state.clone()),
            req,
            web::Json(AskRequest {
                message: "how do I reset my password?".to_string(),
                conversation_id: Some(open.id),
            }),
        )
        .await;
        assert_eq!(resp.status().as_u16(), 200);
        let stored = state
            .conversations
            .get(&open.id)
            .expect("conversation stored");
        assert_eq!(stored.value().status, ConversationStatus::Open);
        let messages = state.messages.get(&open.id).expect("messages stored");
        assert_eq!(messages.value().len(), 2);
    }
}
