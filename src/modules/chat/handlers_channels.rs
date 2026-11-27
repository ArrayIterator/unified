//! chat - handlers channels.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Utc};
use serde_json::{json, Value};
use uuid::Uuid;
use super::{limits::*, types::*, hub::*, state::*, helpers::*, module::*, handlers_messages::*, handlers_files::*, handlers_direct::*, handlers_presence::*, handlers_websocket::*, support_config::*, support_sessions::*, support_ratings::*, support_agents::*, support_canned::*, support_offline::*};



// Channel handlers
pub(crate) async fn create_channel(
    state: web::Data<ChatState>,
    req: HttpRequest,
    body: web::Json<CreateChannelRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Channel> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let kind = body.kind.unwrap_or_default();
        if kind == ChannelKind::Dm {
            return Err(UnifiedError::bad_request(
                "Direct message channels must be created through POST /api/v1/chat/dm",
            ));
        }
        let name = require_text(&body.name, "name", MAX_NAME_LEN)?;
        let company_id = require_company_match(
            &_tenant,
            &req,
            &match &body.company_id {
                Some(raw) => Some(require_text(raw, "company_id", 128)?),
                None => None,
            },
        )?;
        let members = clean_members(body.members.clone(), &actor)?;
        let roles = clean_roles(body.roles.clone())?;
        let now = Utc::now();
        let channel = Channel {
            id: Uuid::now_v7(),
            name,
            kind,
            company_id,
            owner_id: actor.clone(),
            members,
            roles,
            created_at: now,
            updated_at: now,
        };
        state.channels.insert(channel.id, channel.clone());
        state.emit(
            "chat.channel.created",
            json!({
                "channel_id": channel.id.to_string(),
                "name": channel.name.clone(),
                "kind": format!("{:?}", channel.kind).to_lowercase(),
                "owner_id": actor,
                "company_id": channel.company_id.clone(),
            }),
        );
        Ok(channel)
    })();
    match result {
        Ok(channel) => created(channel),
        Err(error) => err(error),
    }
}


pub(crate) async fn list_channels(
    state: web::Data<ChatState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<Channel>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Channel> = state
            .channels
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|channel| can_view(channel, &actor, &roles))
            .filter(|channel| in_company_scope(&channel.company_id, &_scope))
            .filter(|channel| {
                if let Some(kind) = &kind_filter {
                    let expected = match kind.as_str() {
                        "public" => ChannelKind::Public,
                        "private" => ChannelKind::Private,
                        "dm" => ChannelKind::Dm,
                        _ => return false,
                    };
                    if channel.kind != expected {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if channel.company_id.as_deref() != Some(company_id.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() && !channel.name.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["name", "created_at", "updated_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.cmp(&b.name),
                "updated_at" => a.updated_at.cmp(&b.updated_at),
                _ => a.created_at.cmp(&b.created_at),
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


pub(crate) async fn get_channel(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Channel> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let channel = load_channel(&state, &path.into_inner())?;
        require_record_in_company_scope(&channel.company_id, &_scope)?;
        if !can_view(&channel, &actor, &roles) {
            return Err(UnifiedError::forbidden("Channel is not visible to this user"));
        }
        Ok(channel)
    })();
    match result {
        Ok(channel) => ok(channel),
        Err(error) => err(error),
    }
}


pub(crate) async fn update_channel(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateChannelRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Channel> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let mut channel = load_channel(&state, &path.into_inner())?;
        require_record_writable_in_company_scope(&channel.company_id, &_scope)?;
        if channel.owner_id != actor {
            return Err(UnifiedError::forbidden(
                "Only the channel owner can update the channel",
            ));
        }
        if let Some(name) = &body.name {
            channel.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(company_id) = &body.company_id {
            // Outer Some = the field was sent; inner None clears the
            // company (kept as an explicit clear, never silently
            // re-stamped with the caller scope); inner Some must
            // match the caller scope.
            channel.company_id = match company_id {
                Some(raw) => require_company_match(
                    &_tenant,
                    &req,
                    &Some(require_text(raw, "company_id", 128)?),
                )?,
                None => None,
            };
        }
        if let Some(roles) = &body.roles {
            channel.roles = clean_roles(Some(roles.clone()))?;
        }
        channel.updated_at = Utc::now();
        state.channels.insert(channel.id, channel.clone());
        state.emit(
            "chat.channel.updated",
            json!({ "channel_id": channel.id.to_string(), "updated_by": actor }),
        );
        Ok(channel)
    })();
    match result {
        Ok(channel) => ok(channel),
        Err(error) => err(error),
    }
}


pub(crate) async fn delete_channel(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let channel = load_channel(&state, &path.into_inner())?;
        require_record_writable_in_company_scope(&channel.company_id, &_scope)?;
        if channel.owner_id != actor {
            return Err(UnifiedError::forbidden(
                "Only the channel owner can delete the channel",
            ));
        }
        state.channels.remove(&channel.id);
        state.messages.retain(|_, message| message.channel_id != channel.id);
        state.files.retain(|_, file| file.channel_id != channel.id);
        state.emit(
            "chat.channel.deleted",
            json!({ "channel_id": channel.id.to_string(), "deleted_by": actor }),
        );
        Ok(json!({ "deleted": true, "channel_id": channel.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}


pub(crate) async fn add_member(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddMemberRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Channel> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let mut channel = load_channel(&state, &path.into_inner())?;
        require_record_in_company_scope(&channel.company_id, &_scope)?;
        if channel.owner_id != actor {
            return Err(UnifiedError::forbidden(
                "Only the channel owner can add members",
            ));
        }
        let user_id = require_text(&body.user_id, "user_id", 128)?;
        if channel.members.contains(&user_id) {
            return Err(UnifiedError::conflict(format!(
                "User {user_id} is already a member of this channel"
            )));
        }
        if channel.members.len() >= MAX_MEMBERS {
            return Err(UnifiedError::bad_request(format!(
                "At most {MAX_MEMBERS} members are allowed"
            )));
        }
        channel.members.push(user_id);
        channel.updated_at = Utc::now();
        state.channels.insert(channel.id, channel.clone());
        Ok(channel)
    })();
    match result {
        Ok(channel) => ok(channel),
        Err(error) => err(error),
    }
}


pub(crate) async fn remove_member(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Channel> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let (raw_id, user_id) = path.into_inner();
        let mut channel = load_channel(&state, &raw_id)?;
        require_record_in_company_scope(&channel.company_id, &_scope)?;
        if channel.owner_id != actor && actor != user_id {
            return Err(UnifiedError::forbidden(
                "Only the channel owner or the member themselves can remove a member",
            ));
        }
        if channel.owner_id == user_id {
            return Err(UnifiedError::bad_request(
                "The channel owner cannot be removed; delete the channel instead",
            ));
        }
        let before = channel.members.len();
        channel.members.retain(|member| member != &user_id);
        if channel.members.len() == before {
            return Err(UnifiedError::not_found(format!(
                "User {user_id} is not a member of this channel"
            )));
        }
        channel.updated_at = Utc::now();
        state.channels.insert(channel.id, channel.clone());
        Ok(channel)
    })();
    match result {
        Ok(channel) => ok(channel),
        Err(error) => err(error),
    }
}
