//! vault module - HTTP handlers for folders.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::access::can_see_document;
use super::access::load_folder;
use super::constants::MAX_NAME_LEN;
use super::constants::PERM_VAULT_MANAGE;
use super::constants::PERM_VAULT_VIEW;
use super::helpers::caller;
use super::helpers::check_capability;
use super::helpers::created;
use super::helpers::err;
use super::helpers::ok;
use super::helpers::paginate;
use super::helpers::require_permission;
use super::helpers::require_text;
use super::state::VaultState;
use super::types::CreateFolderRequest;
use super::types::SetRetentionRequest;
use super::types::UpdateFolderRequest;
use super::types::VaultDocument;
use super::types::VaultFolder;

// Folder handlers

pub(crate) async fn list_folders(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_VIEW)?;
        let params = query.into_inner();
        let parent_filter = params.filters.get("parent_id").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<VaultFolder> = state
            .folders
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|folder| {
                // Tenant first: a caller only ever sees their own
                // tenant's folders.
                if folder.tenant_id.as_deref() != Some(caller.tenant_id.as_str()) {
                    return false;
                }
                if let Some(parent) = &parent_filter {
                    if parent == "null" {
                        if folder.parent_id.is_some() {
                            return false;
                        }
                    } else if folder.parent_id.map(|id| id.to_string()) != Some(parent.clone()) {
                        return false;
                    }
                }
                if let Some(company) = &company_filter {
                    // Folders carry only the resolved company of
                    // the tenant that created them, so a company
                    // filter for any other company matches nothing
                    // by construction.
                    if folder.company_id.as_deref() != Some(company.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() && !folder.name.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by_key(|folder| folder.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_folder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    body: web::Json<CreateFolderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultFolder> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        if let Some(parent_id) = input.parent_id {
            let same_tenant = state
                .folders
                .get(&parent_id)
                .map(|entry| entry.value().tenant_id.as_deref() == Some(caller.tenant_id.as_str()))
                .unwrap_or(false);
            if !same_tenant {
                return Err(UnifiedError::not_found(format!(
                    "Parent folder not found: {parent_id}"
                )));
            }
        }
        if let Some(days) = input.retention_days {
            if days < 0 {
                return Err(UnifiedError::bad_request("retention_days must be >= 0"));
            }
        }
        let now = Utc::now();
        let folder = VaultFolder {
            id: Uuid::now_v7(),
            name,
            parent_id: input.parent_id,
            owner_id: caller.actor.clone(),
            // Tenant and company are bound server-side from the
            // resolved tenant context. The request body's
            // company_id is ignored (see CreateFolderRequest):
            // a client must not be able to create a folder under
            // another tenant or company.
            tenant_id: Some(caller.tenant_id.clone()),
            company_id: caller.company_id.clone(),
            retention_days: input.retention_days,
            auto_delete: false,
            created_at: now,
            updated_at: now,
        };
        state.folders.insert(folder.id, folder.clone());
        state.emit(
            "vault.folder.created",
            json!({
                "folder_id": folder.id.to_string(),
                "owner_id": folder.owner_id,
                "company_id": folder.company_id,
            }),
        );
        Ok(folder)
    })();
    match result {
        Ok(folder) => created(folder),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_folder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultFolder> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_VIEW)?;
        load_folder(&state, &path.into_inner(), &caller.tenant_id)
    })();
    match result {
        Ok(folder) => ok(folder),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_folder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateFolderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultFolder> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let folder = load_folder(&state, &path.into_inner(), &caller.tenant_id)?;
        let input = body.into_inner();
        let mut updated = folder.clone();
        if let Some(name) = input.name {
            updated.name = require_text(&name, "name", MAX_NAME_LEN)?;
        }
        if let Some(parent) = input.parent_id {
            match parent {
                Some(parent_id) => {
                    if parent_id == folder.id {
                        return Err(UnifiedError::bad_request(
                            "A folder cannot be its own parent",
                        ));
                    }
                    let same_tenant = state
                        .folders
                        .get(&parent_id)
                        .map(|entry| {
                            entry.value().tenant_id.as_deref() == Some(caller.tenant_id.as_str())
                        })
                        .unwrap_or(false);
                    if !same_tenant {
                        return Err(UnifiedError::not_found(format!(
                            "Parent folder not found: {parent_id}"
                        )));
                    }
                    updated.parent_id = Some(parent_id);
                }
                None => updated.parent_id = None,
            }
        }
        if let Some(retention) = input.retention_days {
            match retention {
                Some(days) => {
                    if days < 0 {
                        return Err(UnifiedError::bad_request("retention_days must be >= 0"));
                    }
                    updated.retention_days = Some(days);
                }
                None => updated.retention_days = None,
            }
        }
        updated.updated_at = Utc::now();
        state.folders.insert(updated.id, updated.clone());
        Ok(updated)
    })();
    match result {
        Ok(folder) => ok(folder),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_folder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let folder = load_folder(&state, &path.into_inner(), &caller.tenant_id)?;
        if state
            .folders
            .iter()
            .any(|entry| entry.value().parent_id == Some(folder.id))
        {
            return Err(UnifiedError::conflict(
                "Folder still has sub-folders; move or delete them first",
            ));
        }
        if state
            .documents
            .iter()
            .any(|entry| entry.value().folder_id == folder.id)
        {
            return Err(UnifiedError::conflict(
                "Folder still has documents; move or delete them first",
            ));
        }
        state.folders.remove(&folder.id);
        state.emit(
            "vault.folder.deleted",
            json!({
                "folder_id": folder.id.to_string(),
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "folder_id": folder.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn set_folder_retention(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SetRetentionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultFolder> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let folder = load_folder(&state, &path.into_inner(), &caller.tenant_id)?;
        let input = body.into_inner();
        if input.retention_days < 0 {
            return Err(UnifiedError::bad_request("retention_days must be >= 0"));
        }
        let mut updated = folder.clone();
        updated.retention_days = Some(input.retention_days);
        updated.updated_at = Utc::now();
        state.folders.insert(updated.id, updated.clone());
        Ok(updated)
    })();
    match result {
        Ok(folder) => ok(folder),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_folder_documents(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_VIEW)?;
        let folder = load_folder(&state, &path.into_inner(), &caller.tenant_id)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let is_manager = check_capability(&ctx, &caller.role, PERM_VAULT_MANAGE);
        let mut items: Vec<VaultDocument> = state
            .documents
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|document| {
                if document.folder_id != folder.id {
                    return false;
                }
                if !can_see_document(
                    &state,
                    document,
                    &caller.actor,
                    is_manager,
                    &caller.tenant_id,
                ) {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &document.status != status {
                        return false;
                    }
                }
                if !search.is_empty() && !document.name.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by_key(|document| document.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
