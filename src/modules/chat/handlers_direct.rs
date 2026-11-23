//! chat - handlers direct.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Utc};
use serde_json::{json};
use uuid::Uuid;
use super::{limits::*, types::*, hub::*, state::*, helpers::*, module::*, handlers_channels::*, handlers_messages::*, handlers_files::*, handlers_presence::*, handlers_websocket::*, support_config::*, support_sessions::*, support_ratings::*, support_agents::*, support_canned::*, support_offline::*};



// ---------------------------------------------------------------------------
// Direct message handlers
// ---------------------------------------------------------------------------
pub(crate) async fn create_dm(
    state: web::Data<ChatState>,
    req: HttpRequest,
    body: web::Json<CreateDmRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Channel> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let other = require_text(&body.user_id, "user_id", 128)?;
        if other == actor {
            return Err(UnifiedError::bad_request(
                "A direct message requires a different user",
            ));
        }
        let mut members = vec![actor.clone(), other.clone()];
        members.sort();
        // Find-or-create: a 1:1 channel is unique for its member pair.
        if let Some(existing) = state.channels.iter().find(|entry| {
            let channel = entry.value();
            channel.kind == ChannelKind::Dm && {
                let mut pair = channel.members.clone();
                pair.sort();
                pair == members
            }
        }) {
            return Ok(existing.value().clone());
        }
        let now = Utc::now();
        let channel = Channel {
            id: Uuid::now_v7(),
            name: format!("dm:{}:{}", members[0], members[1]),
            kind: ChannelKind::Dm,
            company_id: None,
            owner_id: actor.clone(),
            members,
            roles: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        state.channels.insert(channel.id, channel.clone());
        state.emit(
            "chat.channel.created",
            json!({
                "channel_id": channel.id.to_string(),
                "kind": "dm",
                "owner_id": actor,
                "members": channel.members.clone(),
            }),
        );
        Ok(channel)
    })();
    match result {
        Ok(channel) => created(channel),
        Err(error) => err(error),
    }
}


pub(crate) async fn list_dms(
    state: web::Data<ChatState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<Channel>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let params = query.into_inner();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Channel> = state
            .channels
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|channel| {
                channel.kind == ChannelKind::Dm
                    && channel.members.iter().any(|member| member == &actor)
            })
            .filter(|channel| {
                if search.is_empty() {
                    return true;
                }
                channel
                    .members
                    .iter()
                    .any(|member| member.to_lowercase().contains(&search))
            })
            .collect();
        items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}
