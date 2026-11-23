// Conversation handlers.

use unified_core::error::UnifiedResult;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};

use super::*;

// ---------------------------------------------------------------------------
// Conversation handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_conversations(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<Conversation>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Conversation> = state
            .conversations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|conversation| conversation.user_id == actor)
            .filter(|conversation| {
                if let Some(status) = &status_filter {
                    let expected = match status.as_str() {
                        "open" => ConversationStatus::Open,
                        "handoff" => ConversationStatus::Handoff,
                        "closed" => ConversationStatus::Closed,
                        _ => return false,
                    };
                    if conversation.status != expected {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let title = conversation.title.clone().unwrap_or_default().to_lowercase();
                    if !title.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["created_at", "updated_at"], "updated_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "created_at" => a.created_at.cmp(&b.created_at),
                _ => a.updated_at.cmp(&b.updated_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

pub(crate) async fn get_conversation(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ConversationDetail> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let id = parse_uuid(&path.into_inner(), "conversation")?;
        let conversation = load_conversation(&state, id, &actor)?;
        let mut messages: Vec<ConversationMessage> = state
            .messages
            .get(&id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        messages.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(ConversationDetail {
            conversation,
            messages,
        })
    })();
    match result {
        Ok(detail) => ok(detail),
        Err(error) => err(error),
    }
}

pub(crate) async fn update_conversation(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateConversationRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Conversation> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let id = parse_uuid(&path.into_inner(), "conversation")?;
        let mut conversation = load_conversation(&state, id, &actor)?;
        if let Some(title) = &body.title {
            conversation.title = match title {
                Some(raw) => Some(require_text(raw, "title", MAX_TITLE_LEN)?),
                None => None,
            };
        }
        if let Some(status) = body.status {
            // The lifecycle table is enforced: an illegal move is a
            // 409. The only reopen is `closed` -> `open`, and it
            // must be explicit — asking never reopens (see ask()).
            change_conversation_status(&mut conversation, status)?;
        }
        conversation.updated_at = Utc::now();
        state
            .conversations
            .insert(conversation.id, conversation.clone());
        Ok(conversation)
    })();
    match result {
        Ok(conversation) => ok(conversation),
        Err(error) => err(error),
    }
}

pub(crate) async fn delete_conversation(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let id = parse_uuid(&path.into_inner(), "conversation")?;
        let _conversation = load_conversation(&state, id, &actor)?;
        state.conversations.remove(&id);
        state.messages.remove(&id);
        Ok(json!({ "deleted": true, "conversation_id": id.to_string() }))
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

    fn stored_conversation(state: &AiSupportState, status: ConversationStatus) -> Conversation {
        let now = Utc::now();
        let conversation = Conversation {
            id: Uuid::now_v7(),
            user_id: "user-1".to_string(),
            title: None,
            status,
            created_at: now,
            updated_at: now,
        };
        state
            .conversations
            .insert(conversation.id, conversation.clone());
        conversation
    }

    async fn update_async(
        state: AiSupportState,
        id: uuid::Uuid,
        status: ConversationStatus,
    ) -> actix_web::HttpResponse {
        let req = verified_request_without_roles("user-1");
        update_conversation(
            web::Data::new(state),
            req,
            web::Path::from(id.to_string()),
            web::Json(UpdateConversationRequest {
                title: None,
                status: Some(status),
            }),
        )
        .await
    }

    #[tokio::test]
    async fn update_rejects_illegal_transitions_with_409() {
        let state = AiSupportState::default();
        let closed = stored_conversation(&state, ConversationStatus::Closed);
        let resp = update_async(state.clone(), closed.id, ConversationStatus::Handoff).await;
        assert_eq!(resp.status().as_u16(), 409);
        let stored = state.conversations.get(&closed.id).expect("stored");
        assert_eq!(stored.value().status, ConversationStatus::Closed);

        let open = stored_conversation(&state, ConversationStatus::Open);
        let resp = update_async(state.clone(), open.id, ConversationStatus::Open).await;
        assert_eq!(resp.status().as_u16(), 200);
    }

    #[tokio::test]
    async fn update_allows_the_documented_moves() {
        let state = AiSupportState::default();
        // open -> handoff -> closed.
        let conversation = stored_conversation(&state, ConversationStatus::Open);
        let resp = update_async(
            state.clone(),
            conversation.id,
            ConversationStatus::Handoff,
        )
        .await;
        assert_eq!(resp.status().as_u16(), 200);
        let resp = update_async(
            state.clone(),
            conversation.id,
            ConversationStatus::Closed,
        )
        .await;
        assert_eq!(resp.status().as_u16(), 200);
        // handoff -> open is rejected.
        let handed_off = stored_conversation(&state, ConversationStatus::Handoff);
        let resp = update_async(state.clone(), handed_off.id, ConversationStatus::Open).await;
        assert_eq!(resp.status().as_u16(), 409);
        // closed -> open is the explicit reopen.
        let resp = update_async(state.clone(), conversation.id, ConversationStatus::Open).await;
        assert_eq!(resp.status().as_u16(), 200);
        let stored = state.conversations.get(&conversation.id).expect("stored");
        assert_eq!(stored.value().status, ConversationStatus::Open);
    }
}
