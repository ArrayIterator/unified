//! chat - handlers files.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Utc};
use serde_json::{json};
use uuid::Uuid;
use super::{limits::*, types::*, hub::*, state::*, helpers::*, module::*, handlers_channels::*, handlers_messages::*, handlers_direct::*, handlers_presence::*, handlers_websocket::*, support_config::*, support_sessions::*, support_ratings::*, support_agents::*, support_canned::*, support_offline::*};



// File sharing handlers
pub(crate) async fn share_file(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ShareFileRequest>,
) -> HttpResponse {
    let result: UnifiedResult<ChatFile> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let channel = load_channel(&state, &path.into_inner())?;
        require_record_in_company_scope(&channel.company_id, &_scope)?;
        if !can_view(&channel, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "This user cannot share files in this channel",
            ));
        }
        let name = require_text(&body.name, "name", MAX_FILE_NAME_LEN)?;
        if body.size == 0 || body.size > MAX_FILE_SIZE {
            return Err(UnifiedError::bad_request(format!(
                "size must be between 1 and {MAX_FILE_SIZE} bytes"
            )));
        }
        let mime = require_text(&body.mime, "mime", MAX_MIME_LEN)?;
        // Optional storage reference: the bytes live in the storage
        // module, chat only records where to find them.
        let storage_target =
            clean_optional_text(&body.storage_target, "storage_target", MAX_STORAGE_REF_LEN)?;
        let storage_key =
            clean_optional_text(&body.storage_key, "storage_key", MAX_STORAGE_REF_LEN)?;
        let url = clean_optional_text(&body.url, "url", MAX_URL_LEN)?;
        if storage_key.is_some() && storage_target.is_none() {
            return Err(UnifiedError::bad_request(
                "storage_key requires a storage_target",
            ));
        }
        let file = ChatFile {
            id: Uuid::now_v7(),
            channel_id: channel.id,
            uploader_id: actor.clone(),
            name: name.clone(),
            size: body.size,
            mime: mime.clone(),
            storage_target: storage_target.clone(),
            storage_key: storage_key.clone(),
            url: url.clone(),
            created_at: Utc::now(),
        };
        state.files.insert(file.id, file.clone());
        state.emit(
            "file.shared",
            json!({
                "file_id": file.id.to_string(),
                "channel_id": channel.id.to_string(),
                "uploader_id": actor,
                "name": name,
                "size": body.size,
                "mime": mime,
                "storage_target": storage_target,
                "storage_key": storage_key,
                "url": url,
                "company_id": channel.company_id,
            }),
        );
        Ok(file)
    })();
    match result {
        Ok(file) => created(file),
        Err(error) => err(error),
    }
}


pub(crate) async fn list_files(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<ChatFile>> = (|| {
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
        let mut items: Vec<ChatFile> = state
            .files
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|file| file.channel_id == channel.id)
            .filter(|file| {
                if search.is_empty() {
                    return true;
                }
                file.name.to_lowercase().contains(&search)
                    || file.mime.to_lowercase().contains(&search)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}
