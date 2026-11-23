//! chat - handlers messages.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Utc};
use serde_json::{json, Value};
use uuid::Uuid;
use super::{limits::*, types::*, hub::*, state::*, helpers::*, module::*, handlers_channels::*, handlers_files::*, handlers_direct::*, handlers_presence::*, handlers_websocket::*, support_config::*, support_sessions::*, support_ratings::*, support_agents::*, support_canned::*, support_offline::*};



// ---------------------------------------------------------------------------
// Message handlers
// ---------------------------------------------------------------------------
pub(crate) async fn post_message(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PostMessageRequest>,
) -> HttpResponse {
    let result: UnifiedResult<ChatMessage> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let channel = load_channel(&state, &path.into_inner())?;
        require_record_in_company_scope(&channel.company_id, &_scope)?;
        if !can_view(&channel, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "This user cannot post in this channel",
            ));
        }
        let text = require_text(&body.body, "body", MAX_BODY_LEN)?;
        let now = Utc::now();
        let message = ChatMessage {
            id: Uuid::now_v7(),
            channel_id: channel.id,
            author_id: actor.clone(),
            body: text.clone(),
            edited: false,
            reactions: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        state.messages.insert(message.id, message.clone());
        // Broadcast to the WebSocket hub first so live clients see the
        // message even if a hook handler fails further down.
        state.hub.publish(ChatBroadcast {
            event: "chat.message".to_string(),
            channel_id: channel.id,
            message: message.clone(),
        });
        state.emit(
            "chat.message",
            json!({
                "channel_id": channel.id.to_string(),
                "message_id": message.id.to_string(),
                "author_id": actor.clone(),
                "body": text.clone(),
                "company_id": channel.company_id,
            }),
        );
        // One notification per @user mention (excluding self-mentions).
        for mentioned in find_mentions(&text) {
            if mentioned == actor {
                continue;
            }
            state.emit(
                "notification.requested",
                json!({
                    "user_id": mentioned,
                    "kind": "chat.mention",
                    "channel_id": channel.id.to_string(),
                    "message_id": message.id.to_string(),
                    "author_id": actor.clone(),
                    "excerpt": text.chars().take(200).collect::<String>(),
                }),
            );
        }
        Ok(message)
    })();
    match result {
        Ok(message) => created(message),
        Err(error) => err(error),
    }
}


pub(crate) async fn list_messages(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<ChatMessage>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let channel = load_channel(&state, &path.into_inner())?;
        require_record_in_company_scope(&channel.company_id, &_scope)?;
        if !can_view(&channel, &actor, &roles) {
            return Err(UnifiedError::forbidden("Channel is not visible to this user"));
        }
        let params = query.into_inner();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let author_filter = params.filters.get("author_id").cloned();
        let mut items: Vec<ChatMessage> = state
            .messages
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|message| message.channel_id == channel.id)
            .filter(|message| {
                if let Some(author) = &author_filter {
                    if &message.author_id != author {
                        return false;
                    }
                }
                if !search.is_empty() && !message.body.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        // Newest first.
        items.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}


pub(crate) async fn update_message(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateMessageRequest>,
) -> HttpResponse {
    let result: UnifiedResult<ChatMessage> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (raw_channel, raw_message) = path.into_inner();
        let channel = load_channel(&state, &raw_channel)?;
        require_record_in_company_scope(&channel.company_id, &_scope)?;
        if !can_view(&channel, &actor, &roles) {
            return Err(UnifiedError::forbidden("Channel is not visible to this user"));
        }
        let mut message = load_message(&state, channel.id, &raw_message)?;
        if message.author_id != actor {
            return Err(UnifiedError::forbidden(
                "Only the message author can edit the message",
            ));
        }
        message.body = require_text(&body.body, "body", MAX_BODY_LEN)?;
        message.edited = true;
        message.updated_at = Utc::now();
        state.messages.insert(message.id, message.clone());
        state.hub.publish(ChatBroadcast {
            event: "chat.message.updated".to_string(),
            channel_id: channel.id,
            message: message.clone(),
        });
        Ok(message)
    })();
    match result {
        Ok(message) => ok(message),
        Err(error) => err(error),
    }
}


pub(crate) async fn delete_message(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (raw_channel, raw_message) = path.into_inner();
        let channel = load_channel(&state, &raw_channel)?;
        require_record_in_company_scope(&channel.company_id, &_scope)?;
        if !can_view(&channel, &actor, &roles) {
            return Err(UnifiedError::forbidden("Channel is not visible to this user"));
        }
        let message = load_message(&state, channel.id, &raw_message)?;
        if message.author_id != actor && channel.owner_id != actor {
            return Err(UnifiedError::forbidden(
                "Only the message author or the channel owner can delete the message",
            ));
        }
        state.messages.remove(&message.id);
        state.hub.publish(ChatBroadcast {
            event: "chat.message.deleted".to_string(),
            channel_id: channel.id,
            message: message.clone(),
        });
        Ok(json!({ "deleted": true, "message_id": message.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}


pub(crate) async fn toggle_reaction(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<ReactionRequest>,
) -> HttpResponse {
    let result: UnifiedResult<ChatMessage> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (raw_channel, raw_message) = path.into_inner();
        let channel = load_channel(&state, &raw_channel)?;
        require_record_in_company_scope(&channel.company_id, &_scope)?;
        if !can_view(&channel, &actor, &roles) {
            return Err(UnifiedError::forbidden("Channel is not visible to this user"));
        }
        let mut message = load_message(&state, channel.id, &raw_message)?;
        let emoji = require_text(&body.emoji, "emoji", 16)?;
        // Toggle: reacting with the same emoji again removes the reaction.
        if let Some(position) = message
            .reactions
            .iter()
            .position(|reaction| reaction.user_id == actor && reaction.emoji == emoji)
        {
            message.reactions.remove(position);
        } else {
            message.reactions.push(Reaction {
                user_id: actor,
                emoji,
            });
        }
        message.updated_at = Utc::now();
        state.messages.insert(message.id, message.clone());
        Ok(message)
    })();
    match result {
        Ok(message) => ok(message),
        Err(error) => err(error),
    }
}


pub(crate) async fn escalate_message(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<EscalateRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (raw_channel, raw_message) = path.into_inner();
        let channel = load_channel(&state, &raw_channel)?;
        require_record_in_company_scope(&channel.company_id, &_scope)?;
        if !can_view(&channel, &actor, &roles) {
            return Err(UnifiedError::forbidden("Channel is not visible to this user"));
        }
        let message = load_message(&state, channel.id, &raw_message)?;
        let title = match &body.title {
            Some(raw) => require_text(raw, "title", MAX_NAME_LEN)?,
            None => format!("Chat escalation from #{}", channel.name),
        };
        // ERP interconnection: the tickets module answers this hook by
        // creating a real ticket; chat only requests it. The payload must
        // carry the fields the tickets handler reads (`tickets.rs`,
        // `on_ticket_create_requested`): `title`, `body`, and `user_id`.
        // `description` is kept alongside `body` for older consumers.
        state.emit(
            "ticket.create_requested",
            json!({
                "source": "chat",
                "channel_id": channel.id.to_string(),
                "message_id": message.id.to_string(),
                "user_id": message.author_id.clone(),
                "author_id": message.author_id,
                "requested_by": actor,
                "title": title.clone(),
                "body": message.body.clone(),
                "description": message.body,
                "company_id": channel.company_id,
                "priority": "normal",
                "category": "chat",
            }),
        );
        Ok(json!({
            "ticket_requested": true,
            "channel_id": channel.id.to_string(),
            "message_id": message.id.to_string(),
            "title": title,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn channel(id: Uuid, members: Vec<&str>) -> Channel {
        let now = Utc::now();
        Channel {
            id,
            name: "support".to_string(),
            kind: ChannelKind::Private,
            company_id: None,
            owner_id: "owner".to_string(),
            members: members.into_iter().map(str::to_string).collect(),
            roles: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn former_member_cannot_view_or_mutate_channel_messages() {
        // The author of a message in a channel they were removed
        // from keeps no edit/delete rights: update/delete now run
        // the same `can_view` check as post/list.
        let id = Uuid::now_v7();
        let channel = channel(id, vec!["owner"]);
        assert!(!can_view(&channel, "author", &[]));
        assert!(can_view(&channel, "owner", &[]));
        // A current member still passes the check.
        let member_channel = channel(Uuid::now_v7(), vec!["owner", "author"]);
        assert!(can_view(&member_channel, "author", &[]));
    }
}
