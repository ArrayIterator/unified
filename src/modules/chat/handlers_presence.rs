//! chat - handlers presence.

use unified_core::error::{UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Utc};
use super::{limits::*, types::*, hub::*, state::*, helpers::*, module::*, handlers_channels::*, handlers_messages::*, handlers_files::*, handlers_direct::*, handlers_websocket::*, support_config::*, support_sessions::*, support_ratings::*, support_agents::*, support_canned::*, support_offline::*};



// Presence handlers
pub(crate) async fn set_presence(
    state: web::Data<ChatState>,
    req: HttpRequest,
    body: web::Json<PresenceRequest>,
) -> HttpResponse {
    let result: UnifiedResult<PresenceEntry> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let entry = PresenceEntry {
            user_id: actor.clone(),
            status: body.status,
            last_seen: Utc::now(),
        };
        state.presence.insert(actor, entry.clone());
        Ok(entry)
    })();
    match result {
        Ok(entry) => ok(entry),
        Err(error) => err(error),
    }
}


pub(crate) async fn list_presence(
    state: web::Data<ChatState>,
    req: HttpRequest,
    query: web::Query<PresenceQuery>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<PresenceEntry>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let _actor = actor_id(&req)?;
        let query = query.into_inner();
        let now = Utc::now();
        // Drop stale entries so presence never reports ghosts.
        state.presence.retain(|_, entry| {
            now.signed_duration_since(entry.last_seen).num_seconds() <= PRESENCE_STALE_SECS
        });
        let mut items: Vec<PresenceEntry> = state
            .presence
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| match &query.user_id {
                Some(user_id) => &entry.user_id == user_id,
                None => true,
            })
            .collect();
        items.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        let params = PaginationParams {
            page: query.page,
            per_page: query.per_page,
            sort: None,
            order: None,
            search: None,
            filters: std::collections::HashMap::new(),
        };
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}
