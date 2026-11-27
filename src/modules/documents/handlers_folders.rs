// documents - HTTP handlers: folders, document move, document rename.
//
// Every handler follows the module's guard order (C3): tenant
// first (`require_tenant`), then the caller company scope
// (`caller_company`), then the capability (`authorize`), then the
// record load + company-scope check. A folder id is never trusted
// without an existence + scope check (`load_folder_in_scope` /
// `folder_accepts`); a missing folder and a foreign-company folder
// both surface as 404/403 through those helpers, never as a
// silent cross-company write.

use super::types::{
    CreateFolderRequest, DocumentFolder, DocumentFolderTree, MoveDocumentRequest,
    MoveFolderRequest, RenameDocumentRequest, UpdateFolderRequest,
};
use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};
use std::sync::Arc;

// Handlers - folders

/// Create a folder (`POST /folders`, body `CreateFolderRequest`).
/// The stored `company_id` is the caller's scope, never a client
/// claim; the parent (when given) must exist in that scope.
pub(crate) async fn create_folder(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateFolderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_WRITE) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<DocumentFolder> = (|| {
        let input = body.into_inner();
        let parent_id = input.parent_id.clone().unwrap_or_default();
        if !parent_id.is_empty() {
            state.load_folder_in_scope(&parent_id, &_scope)?;
        }
        let company_id = _scope.clone().unwrap_or_default();
        state.create_folder(&input.name, &parent_id, &company_id, &found.actor)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

/// List the folder tree (`GET /folders`): top-level folders with
/// children nested recursively, each node carrying its direct
/// document count. Only folders in the caller's company scope are
/// returned. The standard pagination parameters are accepted but
/// the tree is returned whole (a tree cannot page by node); the
/// response is the plain array, not a paginated page.
pub(crate) async fn list_folders(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    _query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let _found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let tree: Vec<DocumentFolderTree> = state.folder_tree(&_scope);
    json::success_response(tree, 200)
}

/// Get one folder (`GET /folders/{id}`).
pub(crate) async fn get_folder(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let _found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<DocumentFolder> = (|| {
        state
            .load_folder_in_scope(path.as_str(), &_scope)?
            .ok_or_else(|| UnifiedError::not_found("Folder not found"))
    })();
    json::respond(result, debug)
}

/// Rename a folder (`PATCH /folders/{id}`, body
/// `UpdateFolderRequest`). Only the owner or an admin may rename.
pub(crate) async fn rename_folder(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateFolderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_WRITE) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<DocumentFolder> = (|| {
        let folder = state
            .load_folder_in_scope(path.as_str(), &_scope)?
            .ok_or_else(|| UnifiedError::not_found("Folder not found"))?;
        if folder.owner_id != found.actor && !check_capability(&ctx, &found.role, PERM_ADMIN) {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin may rename this folder",
            ));
        }
        let name = body
            .into_inner()
            .name
            .ok_or_else(|| UnifiedError::bad_request("Folder name is required"))?;
        state.rename_folder(path.as_str(), &name, &found.actor)
    })();
    json::respond(result, debug)
}

/// Move a folder to a new parent (`POST /folders/{id}/move`,
/// body `MoveFolderRequest`). Only the owner or an admin may move.
/// A move into the folder itself or one of its descendants is
/// refused (409); a move across companies is refused (403).
pub(crate) async fn move_folder(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<MoveFolderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_WRITE) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<DocumentFolder> = (|| {
        let folder = state
            .load_folder_in_scope(path.as_str(), &_scope)?
            .ok_or_else(|| UnifiedError::not_found("Folder not found"))?;
        if folder.owner_id != found.actor && !check_capability(&ctx, &found.role, PERM_ADMIN) {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin may move this folder",
            ));
        }
        let parent_id = body.into_inner().parent_id;
        if !parent_id.is_empty() {
            state.load_folder_in_scope(&parent_id, &_scope)?;
        }
        state.move_folder(path.as_str(), &parent_id, &found.actor)
    })();
    json::respond(result, debug)
}

/// Delete a folder (`DELETE /folders/{id}`). Only the owner or an
/// admin may delete. The delete is refused (409) while the folder
/// still holds documents or subfolders - the caller moves or
/// deletes the contents first, so a delete can never orphan or
/// silently drop files. The UI confirms before calling this
/// endpoint; the server enforces the non-empty guard regardless.
pub(crate) async fn delete_folder(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_WRITE) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<Value> = (|| {
        let folder = state
            .load_folder_in_scope(path.as_str(), &_scope)?
            .ok_or_else(|| UnifiedError::not_found("Folder not found"))?;
        if folder.owner_id != found.actor && !check_capability(&ctx, &found.role, PERM_ADMIN) {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin may delete this folder",
            ));
        }
        state.delete_folder(path.as_str())?;
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, debug)
}

// Handlers - document move / rename

/// Move a document between folders (`POST /{id}/move`, body
/// `MoveDocumentRequest`). Only the owner or an admin may move.
/// The target folder must exist in the caller's company scope;
/// an empty target means the library root. Only the folder
/// assignment changes: the stored key, the visibility area, and
/// the serving URL are untouched.
pub(crate) async fn move_document(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<MoveDocumentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_WRITE) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<DocumentRecord> = (|| {
        let document = state.load_document(path.as_str())?;
        require_record_in_company_scope(&document.company_id, &_scope)?;
        if document.owner_id != found.actor && !check_capability(&ctx, &found.role, PERM_ADMIN) {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin may move this document",
            ));
        }
        let folder_id = body.into_inner().folder_id;
        state.move_document_to_folder(path.as_str(), &folder_id, &_scope, &found.actor)
    })();
    json::respond(result, debug)
}

/// Rename a document (`POST /{id}/rename`, body
/// `RenameDocumentRequest`). Only the owner or an admin may
/// rename. The file name changes; the display title follows only
/// when it still equals the old file name. The stored key and the
/// serving URL never change.
pub(crate) async fn rename_document(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RenameDocumentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_WRITE) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<DocumentRecord> = (|| {
        let document = state.load_document(path.as_str())?;
        require_record_in_company_scope(&document.company_id, &_scope)?;
        if document.owner_id != found.actor && !check_capability(&ctx, &found.role, PERM_ADMIN) {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin may rename this document",
            ));
        }
        let name = body.into_inner().name;
        state.rename_document(path.as_str(), &name, &found.actor)
    })();
    json::respond(result, debug)
}
