// Store module - digital product, file, and preview handlers.
//
// Split from `src/modules/store.rs`; the module root re-exports the public API of this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::storage::validate_media_reference;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;

use super::digital::{digital_can_manage, digital_search_text, require_digital_manage, require_digital_public, require_digital_read, valid_format, valid_type_slug, validate_digital_metadata, validate_royalty};
use super::helpers::{
    caller_company, matches_search, paginate, require_caller_company, require_company_match_for,
    require_digital_product_in_scope, require_product_in_scope, require_record_in_company_scope,
    require_tenant, respond_created,
};
use super::pricing::{valid_product_status};
use super::state::{StoreState, new_id, now};
use super::types::{CreateDigitalFile, CreateDigitalPreview, CreateDigitalProduct, CreateDigitalProductType, DEFAULT_ROYALTY_PERCENT_BP, DigitalProduct, DigitalProductFile, DigitalProductPreview, DigitalProductType, UpdateDigitalProduct, UpdateDigitalProductType, default_product_status, default_royalty_kind};

// ---------------------------------------------------------------------------
// Digital products: types, products, files, previews, samples
// ---------------------------------------------------------------------------

/// Load a digital product for a read that may come from the public
/// storefront: drafts are invisible to anyone without the manage
/// permission (answered as "not found" so drafts do not leak).
pub(crate) fn digital_product_for_read(
    state: &StoreState,
    req: &HttpRequest,
    id: &str,
) -> UnifiedResult<DigitalProduct> {
    let digital = state
        .digital_products
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Digital product not found"))?;
    if digital.status != "published" && !digital_can_manage(state, req) {
        return Err(UnifiedError::not_found("Digital product not found"));
    }
    Ok(digital)
}

pub(crate) fn load_digital_product(state: &StoreState, id: &str) -> UnifiedResult<DigitalProduct> {
    state
        .digital_products
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Digital product not found"))
}

/// Validate a cover media reference against the shared documents
/// store contract (document id or `/files/{public,private}/{key}`
/// URL from the media picker). The error names the field so a bad
/// picker value is diagnosable from the API response alone.
fn validate_cover_reference(value: &str) -> UnifiedResult<String> {
    validate_media_reference(value)
        .map_err(|error| UnifiedError::bad_request(format!("cover_storage_id: {}", error.message)))
}

pub(crate) async fn list_digital_types(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<DigitalProductType>> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = caller_company(&tenant, &req)?;
        require_digital_read(&state, &req)?;
        let params = query.into_inner();
        let mut items: Vec<DigitalProductType> = state
            .digital_types
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|digital_type| {
                matches_search(
                    &format!(
                        "{} {} {}",
                        digital_type.id, digital_type.label, digital_type.description
                    ),
                    &params,
                ) && params.filters.get("enabled").is_none_or(|flag| {
                    digital_type.enabled == (*flag == "true" || *flag == "1")
                })
            })
            .collect();
        items.sort_by(|a, b| a.id.cmp(&b.id));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

pub(crate) async fn create_digital_type(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: web::Json<CreateDigitalProductType>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalProductType> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = require_caller_company(&tenant, &req)?;
        require_digital_manage(&state, &req)?;
        let input = body.into_inner();
        let id = input.id.trim().to_lowercase();
        if !valid_type_slug(&id) {
            return Err(UnifiedError::bad_request(
                "Type id must be a lowercase slug of letters, digits, and hyphens (at most 32 characters)",
            ));
        }
        if state.digital_types.contains_key(&id) {
            return Err(UnifiedError::conflict(
                "A digital product type with this id already exists",
            ));
        }
        let label = input
            .label
            .filter(|label| !label.trim().is_empty())
            .map(|label| label.trim().to_string())
            .unwrap_or_else(|| id.clone());
        if label.len() > 80 {
            return Err(UnifiedError::bad_request(
                "Type label must be at most 80 characters",
            ));
        }
        let description = input.description.unwrap_or_default();
        if description.len() > 500 {
            return Err(UnifiedError::bad_request(
                "Type description must be at most 500 characters",
            ));
        }
        let digital_type = DigitalProductType {
            id: id.clone(),
            label,
            description,
            enabled: input.enabled.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state
            .digital_types
            .insert(id, digital_type.clone());
        Ok(digital_type)
    })();
    respond_created(result)
}

pub(crate) async fn update_digital_type(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateDigitalProductType>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalProductType> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = require_caller_company(&tenant, &req)?;
        require_digital_manage(&state, &req)?;
        let input = body.into_inner();
        let mut digital_type = state
            .digital_types
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Digital product type not found"))?;
        if let Some(label) = input.label {
            if label.trim().is_empty() || label.len() > 80 {
                return Err(UnifiedError::bad_request(
                    "Type label must not be empty and must be at most 80 characters",
                ));
            }
            digital_type.label = label.trim().to_string();
        }
        if let Some(description) = input.description {
            if description.len() > 500 {
                return Err(UnifiedError::bad_request(
                    "Type description must be at most 500 characters",
                ));
            }
            digital_type.description = description;
        }
        if let Some(enabled) = input.enabled {
            digital_type.enabled = enabled;
        }
        digital_type.updated_at = now();
        Ok(digital_type.clone())
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_digital_type(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = require_caller_company(&tenant, &req)?;
        require_digital_manage(&state, &req)?;
        let id = path.as_str().to_string();
        let in_use = state
            .digital_products
            .iter()
            .any(|entry| entry.value().type_id == id);
        if in_use {
            return Err(UnifiedError::conflict(
                "Digital product type is still used by digital products; disable it instead of deleting it",
            ));
        }
        state
            .digital_types
            .remove(&id)
            .map(|_| json!({ "deleted": true, "id": id }))
            .ok_or_else(|| UnifiedError::not_found("Digital product type not found"))
    })();
    json::respond(result, false)
}

pub(crate) async fn list_digital_products(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<DigitalProduct>> = (|| {
        require_digital_public(&state, &req)?;
        let params = query.into_inner();
        let show_all = digital_can_manage(&state, &req);
        let mut items: Vec<DigitalProduct> = state
            .digital_products
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|digital| {
                (show_all || digital.status == "published")
                    && params
                        .filters
                        .get("type_id")
                        .is_none_or(|type_id| digital.type_id == *type_id)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| digital.status == *status)
                    && params
                        .filters
                        .get("product_id")
                        .is_none_or(|product_id| digital.product_id == *product_id)
                    && params.filters.get("author").is_none_or(|author| {
                        digital.author.as_deref() == Some(author.as_str())
                    })
                    && matches_search(&digital_search_text(&state, digital), &params)
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

pub(crate) async fn create_digital_product(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: web::Json<CreateDigitalProduct>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalProduct> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        require_digital_manage(&state, &req)?;
        let input = body.into_inner();
        let _product = require_product_in_scope(&state, &scope, &input.product_id)?;
        let type_id = input.type_id.trim().to_lowercase();
        match state.digital_types.get(&type_id) {
            Some(digital_type) if digital_type.enabled => {}
            Some(_) => {
                return Err(UnifiedError::unprocessable(format!(
                    "Digital product type \"{type_id}\" is disabled"
                )));
            }
            None => {
                return Err(UnifiedError::not_found(format!(
                    "Digital product type \"{type_id}\" not found"
                )));
            }
        }
        if state
            .digital_products
            .iter()
            .any(|entry| entry.value().product_id == input.product_id)
        {
            return Err(UnifiedError::conflict(
                "This product already has a digital profile",
            ));
        }
        validate_digital_metadata(
            input.isbn.as_deref(),
            input.language.as_deref(),
            input.pages,
            input.formats.as_deref(),
        )?;
        let royalty_kind = input.royalty_kind.unwrap_or_else(default_royalty_kind);
        let royalty_percent_bp = input
            .royalty_percent_bp
            .unwrap_or(DEFAULT_ROYALTY_PERCENT_BP);
        let royalty_fixed_minor = input.royalty_fixed_minor.unwrap_or(0);
        validate_royalty(&royalty_kind, royalty_percent_bp, royalty_fixed_minor)?;
        if let Some(status) = &input.status {
            if !valid_product_status(status) {
                return Err(UnifiedError::bad_request(
                    "Digital product status must be one of: draft, published",
                ));
            }
        }
        if let Some(download_limit) = input.download_limit {
            if download_limit > 10_000 {
                return Err(UnifiedError::bad_request(
                    "Download limit must be at most 10000 (0 means the configured default)",
                ));
            }
        }
        let formats: Vec<String> = input
            .formats
            .unwrap_or_default()
            .into_iter()
            .map(|format| format.trim().to_lowercase())
            .collect();
        let payee_company = input
            .royalty_payee_company_id
            .clone()
            .filter(|value| !value.trim().is_empty());
        let royalty_payee_company_id = require_company_match_for(&scope, &payee_company)?;
        // The cover is a reference into the shared documents store
        // (document id or `/files/{public,private}/{key}` URL from
        // the media picker) — never file bytes, never a foreign URL.
        let cover_storage_id = match input
            .cover_storage_id
            .filter(|value| !value.trim().is_empty())
        {
            Some(reference) => Some(validate_cover_reference(&reference)?),
            None => None,
        };
        let digital = DigitalProduct {
            id: new_id(),
            product_id: input.product_id,
            type_id,
            author: input.author.filter(|value| !value.trim().is_empty()),
            publisher: input.publisher.filter(|value| !value.trim().is_empty()),
            edition: input.edition.filter(|value| !value.trim().is_empty()),
            isbn: input.isbn.filter(|value| !value.trim().is_empty()),
            language: input.language.filter(|value| !value.trim().is_empty()),
            pages: input.pages,
            formats,
            cover_storage_id,
            sample_allowed: input.sample_allowed.unwrap_or(false),
            download_limit: input.download_limit.unwrap_or(0),
            royalty_kind,
            royalty_percent_bp,
            royalty_fixed_minor,
            royalty_payee: input.royalty_payee.filter(|value| !value.trim().is_empty()),
            royalty_payee_company_id,
            status: input.status.unwrap_or_else(default_product_status),
            created_at: now(),
            updated_at: now(),
        };
        state
            .digital_products
            .insert(digital.id.clone(), digital.clone());
        state.emit(
            "store.digital.created",
            serde_json::to_value(&digital).unwrap_or(Value::Null),
        );
        Ok(digital)
    })();
    respond_created(result)
}

pub(crate) async fn get_digital_product(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalProduct> = (|| {
        require_digital_public(&state, &req)?;
        digital_product_for_read(&state, &req, path.as_str())
    })();
    json::respond(result, false)
}

pub(crate) async fn update_digital_product(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateDigitalProduct>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalProduct> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        require_digital_manage(&state, &req)?;
        let input = body.into_inner();
        // Validate the new type and royalty pair before touching the
        // stored record, so a rejected update never leaves a
        // half-mutated record behind.
        if let Some(type_id) = &input.type_id {
            let type_id = type_id.trim().to_lowercase();
            match state.digital_types.get(&type_id) {
                Some(digital_type) if digital_type.enabled => {}
                Some(_) => {
                    return Err(UnifiedError::unprocessable(format!(
                        "Digital product type \"{type_id}\" is disabled"
                    )));
                }
                None => {
                    return Err(UnifiedError::not_found(format!(
                        "Digital product type \"{type_id}\" not found"
                    )));
                }
            }
        }
        validate_digital_metadata(
            input.isbn.as_deref(),
            input.language.as_deref(),
            input.pages,
            input.formats.as_deref(),
        )?;
        let existing = load_digital_product(&state, path.as_str())?;
        require_record_in_company_scope(&existing.royalty_payee_company_id, &scope)?;
        let new_payee_company = input
            .royalty_payee_company_id
            .clone()
            .filter(|value| !value.trim().is_empty());
        require_company_match_for(&scope, &new_payee_company)?;
        let royalty_kind = input
            .royalty_kind
            .clone()
            .unwrap_or_else(|| existing.royalty_kind.clone());
        let royalty_percent_bp = input
            .royalty_percent_bp
            .unwrap_or(existing.royalty_percent_bp);
        let royalty_fixed_minor = input
            .royalty_fixed_minor
            .unwrap_or(existing.royalty_fixed_minor);
        validate_royalty(&royalty_kind, royalty_percent_bp, royalty_fixed_minor)?;
        if let Some(status) = &input.status {
            if !valid_product_status(status) {
                return Err(UnifiedError::bad_request(
                    "Digital product status must be one of: draft, published",
                ));
            }
        }
        let mut digital = state
            .digital_products
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Digital product not found"))?;
        if let Some(type_id) = input.type_id {
            digital.type_id = type_id.trim().to_lowercase();
        }
        if let Some(author) = input.author {
            digital.author = if author.trim().is_empty() {
                None
            } else {
                Some(author.trim().to_string())
            };
        }
        if let Some(publisher) = input.publisher {
            digital.publisher = if publisher.trim().is_empty() {
                None
            } else {
                Some(publisher.trim().to_string())
            };
        }
        if let Some(edition) = input.edition {
            digital.edition = if edition.trim().is_empty() {
                None
            } else {
                Some(edition.trim().to_string())
            };
        }
        if let Some(isbn) = input.isbn {
            digital.isbn = if isbn.trim().is_empty() {
                None
            } else {
                Some(isbn.trim().to_string())
            };
        }
        if let Some(language) = input.language {
            digital.language = if language.trim().is_empty() {
                None
            } else {
                Some(language.trim().to_string())
            };
        }
        if let Some(pages) = input.pages {
            digital.pages = Some(pages);
        }
        if let Some(formats) = input.formats {
            digital.formats = formats
                .into_iter()
                .map(|format| format.trim().to_lowercase())
                .collect();
        }
        if let Some(cover_storage_id) = input.cover_storage_id {
            digital.cover_storage_id = if cover_storage_id.trim().is_empty() {
                None
            } else {
                Some(validate_cover_reference(&cover_storage_id)?)
            };
        }
        if let Some(sample_allowed) = input.sample_allowed {
            digital.sample_allowed = sample_allowed;
        }
        if let Some(download_limit) = input.download_limit {
            if download_limit > 10_000 {
                return Err(UnifiedError::bad_request(
                    "Download limit must be at most 10000 (0 means the configured default)",
                ));
            }
            digital.download_limit = download_limit;
        }
        digital.royalty_kind = royalty_kind;
        digital.royalty_percent_bp = royalty_percent_bp;
        digital.royalty_fixed_minor = royalty_fixed_minor;
        if let Some(royalty_payee) = input.royalty_payee {
            digital.royalty_payee = if royalty_payee.trim().is_empty() {
                None
            } else {
                Some(royalty_payee.trim().to_string())
            };
        }
        if let Some(royalty_payee_company_id) = input.royalty_payee_company_id {
            digital.royalty_payee_company_id = if royalty_payee_company_id.trim().is_empty() {
                None
            } else {
                Some(royalty_payee_company_id.trim().to_string())
            };
        }
        if let Some(status) = input.status {
            digital.status = status;
        }
        digital.updated_at = now();
        Ok(digital.clone())
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_digital_product(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        require_digital_manage(&state, &req)?;
        let id = path.as_str().to_string();
        let _digital = require_digital_product_in_scope(&state, &scope, &id)?;
        if state
            .digital_files
            .iter()
            .any(|entry| entry.value().digital_product_id == id)
        {
            return Err(UnifiedError::conflict(
                "Digital product still has files; delete the files first",
            ));
        }
        if state
            .digital_previews
            .iter()
            .any(|entry| entry.value().digital_product_id == id)
        {
            return Err(UnifiedError::conflict(
                "Digital product still has previews; delete the previews first",
            ));
        }
        if state
            .digital_royalties
            .iter()
            .any(|entry| entry.value().digital_product_id == id)
        {
            return Err(UnifiedError::conflict(
                "Digital product has royalty history; it cannot be deleted",
            ));
        }
        if state
            .digital_downloads
            .iter()
            .any(|entry| entry.value().digital_product_id == id)
        {
            return Err(UnifiedError::conflict(
                "Digital product has download history; it cannot be deleted",
            ));
        }
        state
            .digital_products
            .remove(&id)
            .map(|_| json!({ "deleted": true, "id": id }))
            .ok_or_else(|| UnifiedError::not_found("Digital product not found"))
    })();
    json::respond(result, false)
}

pub(crate) fn set_digital_product_status(
    state: &StoreState,
    req: &HttpRequest,
    scope: &Option<String>,
    id: &str,
    status: &str,
    event: &str,
) -> UnifiedResult<DigitalProduct> {
    require_digital_manage(state, req)?;
    let mut digital = state
        .digital_products
        .get_mut(id)
        .ok_or_else(|| UnifiedError::not_found("Digital product not found"))?;
    require_record_in_company_scope(&digital.royalty_payee_company_id, scope)?;
    if digital.status == status {
        return Err(UnifiedError::conflict(format!(
            "Digital product is already {status}"
        )));
    }
    digital.status = status.to_string();
    digital.updated_at = now();
    let updated = digital.clone();
    drop(digital);
    state.emit(event, serde_json::to_value(&updated).unwrap_or(Value::Null));
    Ok(updated)
}

pub(crate) async fn publish_digital_product(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let scope = match require_caller_company(&tenant, &req) {
        Ok(company) => Some(company),
        Err(error) => return json::error_response(&error, false),
    };
    let result = set_digital_product_status(
        &state,
        &req,
        &scope,
        path.as_str(),
        "{status}",
        "{event}",
    );
    json::respond(result, false)
}

pub(crate) async fn unpublish_digital_product(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let scope = match require_caller_company(&tenant, &req) {
        Ok(company) => Some(company),
        Err(error) => return json::error_response(&error, false),
    };
    let result = set_digital_product_status(
        &state,
        &req,
        &scope,
        path.as_str(),
        "{status}",
        "{event}",
    );
    json::respond(result, false)
}

pub(crate) async fn list_digital_files(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<DigitalProductFile>> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = caller_company(&tenant, &req)?;
        require_digital_read(&state, &req)?;
        let digital = digital_product_for_read(&state, &req, path.as_str())?;
        if digital_can_manage(&state, &req) {
            require_record_in_company_scope(&digital.royalty_payee_company_id, &scope)?;
        }
        let params = query.into_inner();
        let mut items: Vec<DigitalProductFile> = state
            .digital_files
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|file| {
                file.digital_product_id == digital.id
                    && matches_search(
                        &format!("{} {}", file.format, file.label.clone().unwrap_or_default()),
                        &params,
                    )
                    && params
                        .filters
                        .get("format")
                        .is_none_or(|format| file.format == *format)
                    && params.filters.get("is_primary").is_none_or(|flag| {
                        file.is_primary == (*flag == "true" || *flag == "1")
                    })
            })
            .collect();
        items.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then(a.created_at.cmp(&b.created_at))
        });
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

pub(crate) async fn create_digital_file(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateDigitalFile>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalProductFile> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        require_digital_manage(&state, &req)?;
        let digital = require_digital_product_in_scope(&state, &scope, path.as_str())?;
        let input = body.into_inner();
        let storage_file_id = input.storage_file_id.trim().to_string();
        if storage_file_id.is_empty() || storage_file_id.len() > 512 {
            return Err(UnifiedError::bad_request(
                "Storage file id must not be empty and must be at most 512 characters",
            ));
        }
        let format = input.format.trim().to_lowercase();
        if !valid_format(&format) {
            return Err(UnifiedError::bad_request(
                "Format must be a lowercase alphanumeric token such as pdf or epub",
            ));
        }
        if let Some(checksum) = &input.checksum {
            let checksum = checksum.trim();
            if !checksum.is_empty()
                && (checksum.len() < 32
                    || checksum.len() > 128
                    || !checksum.chars().all(|c| c.is_ascii_hexdigit()))
            {
                return Err(UnifiedError::bad_request(
                    "Checksum must be a hex digest of 32 to 128 characters",
                ));
            }
        }
        if let Some(file_size_bytes) = input.file_size_bytes {
            if file_size_bytes == 0 {
                return Err(UnifiedError::bad_request(
                    "File size must be greater than zero when set",
                ));
            }
        }
        let is_primary = input.is_primary.unwrap_or(false);
        if is_primary {
            for mut entry in state.digital_files.iter_mut() {
                if entry.value().digital_product_id == digital.id {
                    entry.value_mut().is_primary = false;
                    entry.value_mut().updated_at = now();
                }
            }
        }
        let file = DigitalProductFile {
            id: new_id(),
            digital_product_id: digital.id.clone(),
            storage_file_id,
            format,
            label: input.label.filter(|value| !value.trim().is_empty()),
            file_size_bytes: input.file_size_bytes,
            checksum: input.checksum.filter(|value| !value.trim().is_empty()),
            is_primary,
            sort_order: input.sort_order.unwrap_or(0),
            created_at: now(),
            updated_at: now(),
        };
        state.digital_files.insert(file.id.clone(), file.clone());
        Ok(file)
    })();
    respond_created(result)
}

pub(crate) async fn delete_digital_file(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        require_digital_manage(&state, &req)?;
        let id = path.as_str().to_string();
        let file = state
            .digital_files
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Digital file not found"))?;
        let _digital = require_digital_product_in_scope(&state, &scope, &file.digital_product_id)?;
        if state
            .entitlements
            .iter()
            .any(|entry| entry.value().release_id.as_deref() == Some(id.as_str()))
        {
            return Err(UnifiedError::conflict(
                "Digital file has download entitlements; it cannot be deleted",
            ));
        }
        if state
            .digital_downloads
            .iter()
            .any(|entry| entry.value().file_id.as_deref() == Some(id.as_str()))
        {
            return Err(UnifiedError::conflict(
                "Digital file has download history; it cannot be deleted",
            ));
        }
        state
            .digital_files
            .remove(&id)
            .map(|_| json!({ "deleted": true, "id": file.id }))
            .ok_or_else(|| UnifiedError::not_found("Digital file not found"))
    })();
    json::respond(result, false)
}

pub(crate) async fn list_digital_previews(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<DigitalProductPreview>> = (|| {
        require_digital_public(&state, &req)?;
        let digital = digital_product_for_read(&state, &req, path.as_str())?;
        let params = query.into_inner();
        let show_all = digital_can_manage(&state, &req);
        let mut items: Vec<DigitalProductPreview> = state
            .digital_previews
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|preview| {
                preview.digital_product_id == digital.id
                    && (show_all || preview.allowed)
                    && matches_search(
                        &format!(
                            "{} {}",
                            preview.format,
                            preview.label.clone().unwrap_or_default()
                        ),
                        &params,
                    )
                    && params.filters.get("format").is_none_or(|format| {
                        preview.format == *format
                    })
                    && params.filters.get("allowed").is_none_or(|flag| {
                        preview.allowed == (*flag == "true" || *flag == "1")
                    })
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

pub(crate) async fn create_digital_preview(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateDigitalPreview>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalProductPreview> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        require_digital_manage(&state, &req)?;
        let digital = require_digital_product_in_scope(&state, &scope, path.as_str())?;
        let input = body.into_inner();
        let storage_file_id = input.storage_file_id.trim().to_string();
        if storage_file_id.is_empty() || storage_file_id.len() > 512 {
            return Err(UnifiedError::bad_request(
                "Storage file id must not be empty and must be at most 512 characters",
            ));
        }
        let format = input.format.trim().to_lowercase();
        if !valid_format(&format) {
            return Err(UnifiedError::bad_request(
                "Format must be a lowercase alphanumeric token such as pdf or epub",
            ));
        }
        let preview = DigitalProductPreview {
            id: new_id(),
            digital_product_id: digital.id.clone(),
            storage_file_id,
            format,
            label: input.label.filter(|value| !value.trim().is_empty()),
            allowed: input.allowed.unwrap_or(false),
            created_at: now(),
            updated_at: now(),
        };
        state
            .digital_previews
            .insert(preview.id.clone(), preview.clone());
        Ok(preview)
    })();
    respond_created(result)
}

pub(crate) async fn delete_digital_preview(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        require_digital_manage(&state, &req)?;
        let id = path.as_str().to_string();
        let preview = state
            .digital_previews
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Digital preview not found"))?;
        let _digital =
            require_digital_product_in_scope(&state, &scope, &preview.digital_product_id)?;
        if state
            .digital_downloads
            .iter()
            .any(|entry| entry.value().preview_id.as_deref() == Some(id.as_str()))
        {
            return Err(UnifiedError::conflict(
                "Preview has download history; it cannot be deleted",
            ));
        }
        state
            .digital_previews
            .remove(&id)
            .map(|_| json!({ "deleted": true, "id": id }))
            .ok_or_else(|| UnifiedError::not_found("Digital preview not found"))
    })();
    json::respond(result, false)
}
