// Library - Catalog, copy and category handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::{COPY_AVAILABLE, COPY_ON_LOAN, COPY_WITHDRAWN, CreateCategoryRequest, CreateCopyRequest,
    CreateItemRequest, ITEM_ACTIVE, ITEM_WITHDRAWN, LibraryCategory, LibraryCopy, LibraryItem,
    LibraryState, MAX_AUTHORS, MAX_CODE_LEN, MAX_NOTE_LEN, MAX_REF_LEN, MAX_SLUG_LEN, MAX_TAGS,
    MAX_TEXT_LEN, MAX_TITLE_LEN, PERM_LIBRARY_VIEW, UpdateCategoryRequest, UpdateCopyRequest,
    UpdateItemRequest, caller, check_search_rate, created, err, item_matches_query, load_category,
    load_copy, load_item, normalize_isbn, ok, optional_text, paginate, parse_optional_uuid,
    parse_uuid, require_catalog, require_in_list, require_non_negative, require_permission,
    require_tenant, require_text, slugify};

// Item handlers

pub(crate) async fn list_items(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let params = query.into_inner();
        let pagination = params.normalized();
        let filters = pagination.filters.clone();
        let mut items: Vec<LibraryItem> = state
            .items
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items.retain(|item| super::auth::in_company_scope(&item.company_id, &company));
        if let Some(status) = filters.get("status") {
            items.retain(|item| &item.status == status);
        }
        if let Some(item_type) = filters.get("item_type") {
            items.retain(|item| &item.item_type == item_type);
        }
        if let Some(language) = filters.get("language") {
            items.retain(|item| &item.language == language);
        }
        if let Some(category) = filters.get("category_id") {
            items.retain(|item| {
                item.category_id
                    .map(|id| id.to_string() == *category)
                    .unwrap_or(false)
            });
        }
        if let Some(category_name) = filters.get("category_name") {
            items.retain(|item| {
                item.category_name
                    .as_deref()
                    .map(|name| name == category_name)
                    .unwrap_or(false)
            });
        }
        if let Some(author) = filters.get("author") {
            let needle = author.to_lowercase();
            items.retain(|item| {
                item.authors
                    .iter()
                    .any(|name| name.to_lowercase().contains(&needle))
            });
        }
        if let Some(isbn) = filters.get("isbn") {
            let needle = isbn.replace('-', "").replace(' ', "").to_uppercase();
            items.retain(|item| {
                item.isbn_normalized
                    .as_deref()
                    .map(|value| value.contains(&needle))
                    .unwrap_or(false)
            });
        }
        if let Some(search) = &pagination.search {
            items.retain(|item| item_matches_query(item, search));
        }
        items.sort_by(|a, b| a.title.cmp(&b.title).then(a.id.cmp(&b.id)));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    body: web::Json<CreateItemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryItem> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_catalog(&ctx, &caller.role)?;
        let input = body.into_inner();
        let company_id = super::auth::require_company_match_for(&company, &input.company_id)?;
        let settings = state.settings();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let language = require_in_list(&input.language, "language", &settings.languages)?;
        let item_type = require_in_list(&input.item_type, "item_type", &settings.item_types)?;
        let authors = match &input.authors {
            Some(list) => {
                if list.len() > MAX_AUTHORS {
                    return Err(UnifiedError::bad_request(format!(
                        "authors must have at most {MAX_AUTHORS} entries"
                    )));
                }
                let mut checked = Vec::with_capacity(list.len());
                for author in list {
                    checked.push(require_text(author, "authors", MAX_TITLE_LEN)?);
                }
                checked
            }
            None => Vec::new(),
        };
        let tags = match &input.tags {
            Some(list) => {
                if list.len() > MAX_TAGS {
                    return Err(UnifiedError::bad_request(format!(
                        "tags must have at most {MAX_TAGS} entries"
                    )));
                }
                let mut checked = Vec::with_capacity(list.len());
                for tag in list {
                    checked.push(require_text(tag, "tags", MAX_SLUG_LEN)?);
                }
                checked
            }
            None => Vec::new(),
        };
        let isbn_normalized = match &input.isbn {
            Some(raw) if !raw.trim().is_empty() => Some(normalize_isbn(raw)?),
            _ => None,
        };
        if let Some(normalized) = &isbn_normalized {
            let edition = input.edition.as_deref().unwrap_or("").trim();
            let duplicate = state.items.iter().any(|entry| {
                entry.value().isbn_normalized.as_deref() == Some(normalized.as_str())
                    && entry.value().edition.as_deref().unwrap_or("") == edition
                    && entry.value().status != ITEM_WITHDRAWN
            });
            if duplicate {
                return Err(UnifiedError::conflict(format!(
                    "An active item already uses ISBN {normalized} for this edition"
                )));
            }
        }
        let published_year = match input.published_year {
            Some(year) => {
                let current = Utc::now().format("%Y").to_string().parse::<i64>().unwrap_or(0);
                if year < 0 || year > current + 1 {
                    return Err(UnifiedError::bad_request(
                        "published_year must be a plausible year",
                    ));
                }
                Some(year)
            }
            None => None,
        };
        let series_position = match input.series_position {
            Some(value) => Some(require_non_negative(value, "series_position")?),
            None => None,
        };
        let category_id = parse_optional_uuid(&input.category_id, "category")?;
        if let Some(category_uuid) = category_id {
            if !state.categories.contains_key(&category_uuid) {
                return Err(UnifiedError::not_found(format!(
                    "Category \"{category_uuid}\" not found"
                )));
            }
        }
        let category_name = optional_text(&input.category_name, "category_name", MAX_SLUG_LEN)?;
        if let Some(name) = &category_name {
            if !settings.categories.iter().any(|entry| entry == name) {
                return Err(UnifiedError::bad_request(format!(
                    "category_name \"{name}\" is not in the configured categories"
                )));
            }
        }
        let now = Utc::now();
        let item = LibraryItem {
            id: Uuid::new_v4(),
            title,
            subtitle: optional_text(&input.subtitle, "subtitle", MAX_TITLE_LEN)?,
            authors,
            isbn: input.isbn.map(|raw| raw.trim().to_string()).filter(|value| !value.is_empty()),
            isbn_normalized,
            edition: optional_text(&input.edition, "edition", MAX_SLUG_LEN)?,
            series: optional_text(&input.series, "series", MAX_TITLE_LEN)?,
            series_position,
            publisher: optional_text(&input.publisher, "publisher", MAX_TITLE_LEN)?,
            published_year,
            language,
            category_id,
            category_name,
            item_type,
            status: ITEM_ACTIVE.to_string(),
            description: optional_text(&input.description, "description", MAX_TEXT_LEN)?,
            cover_ref: optional_text(&input.cover_ref, "cover_ref", MAX_REF_LEN)?,
            digital_ref: optional_text(&input.digital_ref, "digital_ref", MAX_REF_LEN)?,
            document_ref: optional_text(&input.document_ref, "document_ref", MAX_REF_LEN)?,
            storage_ref: optional_text(&input.storage_ref, "storage_ref", MAX_REF_LEN)?,
            tags,
            company_id,
            created_at: now,
            updated_at: now,
        };
        state.items.insert(item.id, item.clone());
        state.emit(
            "library.item.created",
            json!({
                "item_id": item.id.to_string(),
                "title": item.title,
                "item_type": item.item_type,
                "isbn": item.isbn_normalized,
                "actor": caller.actor,
            }),
        );
        Ok(item)
    })();
    match result {
        Ok(item) => created(item),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryItem> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let item = load_item(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&item.company_id, &company)?;
        Ok(item)
    })();
    match result {
        Ok(item) => ok(item),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateItemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryItem> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_catalog(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut item = load_item(&state, &path.into_inner())?;
        super::auth::require_record_writable_in_company_scope(&item.company_id, &company)?;
        if item.status == ITEM_WITHDRAWN {
            return Err(UnifiedError::conflict(
                "A withdrawn item cannot be updated; create a new edition instead",
            ));
        }
        let settings = state.settings();
        if let Some(title) = &input.title {
            item.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(subtitle) = &input.subtitle {
            item.subtitle = match subtitle {
                Some(value) => Some(require_text(value, "subtitle", MAX_TITLE_LEN)?),
                None => None,
            };
        }
        if let Some(authors) = &input.authors {
            if authors.len() > MAX_AUTHORS {
                return Err(UnifiedError::bad_request(format!(
                    "authors must have at most {MAX_AUTHORS} entries"
                )));
            }
            let mut checked = Vec::with_capacity(authors.len());
            for author in authors {
                checked.push(require_text(author, "authors", MAX_TITLE_LEN)?);
            }
            item.authors = checked;
        }
        if let Some(isbn) = &input.isbn {
            let normalized = match isbn {
                Some(raw) if !raw.trim().is_empty() => Some(normalize_isbn(raw)?),
                _ => None,
            };
            if let Some(value) = &normalized {
                let edition = item.edition.as_deref().unwrap_or("");
                let duplicate = state.items.iter().any(|entry| {
                    entry.value().id != item.id
                        && entry.value().isbn_normalized.as_deref() == Some(value.as_str())
                        && entry.value().edition.as_deref().unwrap_or("") == edition
                        && entry.value().status != ITEM_WITHDRAWN
                });
                if duplicate {
                    return Err(UnifiedError::conflict(format!(
                        "An active item already uses ISBN {value} for this edition"
                    )));
                }
            }
            item.isbn = isbn.clone().map(|raw| raw.trim().to_string()).filter(|value| !value.is_empty());
            item.isbn_normalized = normalized;
        }
        if let Some(edition) = &input.edition {
            item.edition = match edition {
                Some(value) => Some(require_text(value, "edition", MAX_SLUG_LEN)?),
                None => None,
            };
        }
        if let Some(series) = &input.series {
            item.series = match series {
                Some(value) => Some(require_text(value, "series", MAX_TITLE_LEN)?),
                None => None,
            };
        }
        if let Some(position) = input.series_position {
            item.series_position = match position {
                Some(value) => Some(require_non_negative(value, "series_position")?),
                None => None,
            };
        }
        if let Some(publisher) = &input.publisher {
            item.publisher = match publisher {
                Some(value) => Some(require_text(value, "publisher", MAX_TITLE_LEN)?),
                None => None,
            };
        }
        if let Some(year) = input.published_year {
            item.published_year = match year {
                Some(value) => {
                    let current = Utc::now().format("%Y").to_string().parse::<i64>().unwrap_or(0);
                    if value < 0 || value > current + 1 {
                        return Err(UnifiedError::bad_request(
                            "published_year must be a plausible year",
                        ));
                    }
                    Some(value)
                }
                None => None,
            };
        }
        if let Some(language) = &input.language {
            item.language = require_in_list(language, "language", &settings.languages)?;
        }
        if let Some(category_id) = &input.category_id {
            let parsed = match category_id {
                Some(raw) => Some(parse_uuid(raw, "category")?),
                None => None,
            };
            if let Some(uuid) = parsed {
                if !state.categories.contains_key(&uuid) {
                    return Err(UnifiedError::not_found(format!(
                        "Category \"{uuid}\" not found"
                    )));
                }
            }
            item.category_id = parsed;
        }
        if let Some(category_name) = &input.category_name {
            let checked = match category_name {
                Some(name) => Some(require_in_list(name, "category_name", &settings.categories)?),
                None => None,
            };
            item.category_name = checked;
        }
        if let Some(item_type) = &input.item_type {
            item.item_type = require_in_list(item_type, "item_type", &settings.item_types)?;
        }
        if let Some(description) = &input.description {
            item.description = match description {
                Some(value) => Some(require_text(value, "description", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        if let Some(cover_ref) = &input.cover_ref {
            item.cover_ref = match cover_ref {
                Some(value) => Some(require_text(value, "cover_ref", MAX_REF_LEN)?),
                None => None,
            };
        }
        if let Some(digital_ref) = &input.digital_ref {
            item.digital_ref = match digital_ref {
                Some(value) => Some(require_text(value, "digital_ref", MAX_REF_LEN)?),
                None => None,
            };
        }
        if let Some(document_ref) = &input.document_ref {
            item.document_ref = match document_ref {
                Some(value) => Some(require_text(value, "document_ref", MAX_REF_LEN)?),
                None => None,
            };
        }
        if let Some(storage_ref) = &input.storage_ref {
            item.storage_ref = match storage_ref {
                Some(value) => Some(require_text(value, "storage_ref", MAX_REF_LEN)?),
                None => None,
            };
        }
        if let Some(tags) = &input.tags {
            if tags.len() > MAX_TAGS {
                return Err(UnifiedError::bad_request(format!(
                    "tags must have at most {MAX_TAGS} entries"
                )));
            }
            let mut checked = Vec::with_capacity(tags.len());
            for tag in tags {
                checked.push(require_text(tag, "tags", MAX_SLUG_LEN)?);
            }
            item.tags = checked;
        }
        item.updated_at = Utc::now();
        state.items.insert(item.id, item.clone());
        state.emit(
            "library.item.updated",
            json!({
                "item_id": item.id.to_string(),
                "title": item.title,
                "actor": caller.actor,
            }),
        );
        Ok(item)
    })();
    match result {
        Ok(item) => ok(item),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_catalog(&ctx, &caller.role)?;
        let item = load_item(&state, &path.into_inner())?;
        super::auth::require_record_writable_in_company_scope(&item.company_id, &company)?;
        let has_copies = state
            .copies
            .iter()
            .any(|entry| entry.value().item_id == item.id);
        if has_copies {
            return Err(UnifiedError::conflict(
                "An item with copies cannot be deleted; withdraw it instead",
            ));
        }
        let has_loans = state
            .loans
            .iter()
            .any(|entry| entry.value().item_id == item.id);
        if has_loans {
            return Err(UnifiedError::conflict(
                "An item with loan history cannot be deleted; withdraw it instead",
            ));
        }
        state.items.remove(&item.id);
        state.emit(
            "library.item.updated",
            json!({
                "item_id": item.id.to_string(),
                "title": item.title,
                "deleted": true,
                "actor": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "item_id": item.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn withdraw_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryItem> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_catalog(&ctx, &caller.role)?;
        let mut item = load_item(&state, &path.into_inner())?;
        super::auth::require_record_writable_in_company_scope(&item.company_id, &company)?;
        if item.status == ITEM_WITHDRAWN {
            return Err(UnifiedError::conflict("Item is already withdrawn"));
        }
        let on_loan = state.copies.iter().any(|entry| {
            entry.value().item_id == item.id && entry.value().status == COPY_ON_LOAN
        });
        if on_loan {
            return Err(UnifiedError::conflict(
                "An item with copies on loan cannot be withdrawn until the copies are returned",
            ));
        }
        item.status = ITEM_WITHDRAWN.to_string();
        item.updated_at = Utc::now();
        state.items.insert(item.id, item.clone());
        for mut entry in state.copies.iter_mut() {
            if entry.value().item_id == item.id && entry.value().status == COPY_AVAILABLE {
                entry.value_mut().status = COPY_WITHDRAWN.to_string();
                entry.value_mut().updated_at = Utc::now();
            }
        }
        state.emit(
            "library.item.withdrawn",
            json!({
                "item_id": item.id.to_string(),
                "title": item.title,
                "actor": caller.actor,
            }),
        );
        Ok(item)
    })();
    match result {
        Ok(item) => ok(item),
        Err(error) => err(error, debug),
    }
}

// Copy handlers

pub(crate) async fn list_item_copies(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let item = load_item(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&item.company_id, &company)?;
        let params = query.into_inner();
        let pagination = params.normalized();
        let filters = pagination.filters.clone();
        let mut copies: Vec<LibraryCopy> = state
            .copies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|copy| copy.item_id == item.id)
            .collect();
        copies.retain(|copy| super::auth::in_company_scope(&copy.company_id, &company));
        if let Some(status) = filters.get("status") {
            copies.retain(|copy| &copy.status == status);
        }
        if let Some(shelf) = filters.get("shelf") {
            copies.retain(|copy| copy.shelf.as_deref() == Some(shelf.as_str()));
        }
        if let Some(location) = filters.get("location") {
            copies.retain(|copy| copy.location.as_deref() == Some(location.as_str()));
        }
        copies.sort_by(|a, b| a.barcode.cmp(&b.barcode));
        Ok(paginate(copies, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_copy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateCopyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryCopy> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_catalog(&ctx, &caller.role)?;
        let item = load_item(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&item.company_id, &company)?;
        if item.status == ITEM_WITHDRAWN {
            return Err(UnifiedError::conflict(
                "Copies cannot be added to a withdrawn item",
            ));
        }
        let input = body.into_inner();
        let company_id = super::auth::require_company_match_for(&company, &input.company_id)?;
        let settings = state.settings();
        let barcode = require_text(&input.barcode, "barcode", MAX_CODE_LEN)?;
        let duplicate = state
            .copies
            .iter()
            .any(|entry| entry.value().barcode == barcode);
        if duplicate {
            return Err(UnifiedError::conflict(format!(
                "A copy already uses barcode \"{barcode}\""
            )));
        }
        let condition = match &input.condition {
            Some(value) => require_in_list(value, "condition", &settings.copy_conditions)?,
            None => "good".to_string(),
        };
        let now = Utc::now();
        let copy = LibraryCopy {
            id: Uuid::new_v4(),
            item_id: item.id,
            barcode,
            serial: optional_text(&input.serial, "serial", MAX_CODE_LEN)?,
            condition,
            location: optional_text(&input.location, "location", MAX_TITLE_LEN)?,
            shelf: optional_text(&input.shelf, "shelf", MAX_SLUG_LEN)?,
            status: COPY_AVAILABLE.to_string(),
            notes: optional_text(&input.notes, "notes", MAX_NOTE_LEN)?,
            company_id,
            created_at: now,
            updated_at: now,
        };
        state.copies.insert(copy.id, copy.clone());
        state.emit(
            "library.copy.created",
            json!({
                "copy_id": copy.id.to_string(),
                "item_id": item.id.to_string(),
                "barcode": copy.barcode,
                "actor": caller.actor,
            }),
        );
        Ok(copy)
    })();
    match result {
        Ok(copy) => created(copy),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_copy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryCopy> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let copy = load_copy(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&copy.company_id, &company)?;
        Ok(copy)
    })();
    match result {
        Ok(copy) => ok(copy),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_copy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCopyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryCopy> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_catalog(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut copy = load_copy(&state, &path.into_inner())?;
        super::auth::require_record_writable_in_company_scope(&copy.company_id, &company)?;
        let settings = state.settings();
        if let Some(barcode) = &input.barcode {
            let checked = require_text(barcode, "barcode", MAX_CODE_LEN)?;
            let duplicate = state.copies.iter().any(|entry| {
                entry.value().id != copy.id && entry.value().barcode == checked
            });
            if duplicate {
                return Err(UnifiedError::conflict(format!(
                    "A copy already uses barcode \"{checked}\""
                )));
            }
            copy.barcode = checked;
        }
        if let Some(serial) = &input.serial {
            copy.serial = match serial {
                Some(value) => Some(require_text(value, "serial", MAX_CODE_LEN)?),
                None => None,
            };
        }
        if let Some(condition) = &input.condition {
            copy.condition = require_in_list(condition, "condition", &settings.copy_conditions)?;
        }
        if let Some(location) = &input.location {
            copy.location = match location {
                Some(value) => Some(require_text(value, "location", MAX_TITLE_LEN)?),
                None => None,
            };
        }
        if let Some(shelf) = &input.shelf {
            copy.shelf = match shelf {
                Some(value) => Some(require_text(value, "shelf", MAX_SLUG_LEN)?),
                None => None,
            };
        }
        if let Some(status) = &input.status {
            let checked = require_in_list(status, "status", &settings.copy_statuses)?;
            if copy.status == COPY_ON_LOAN && checked != COPY_ON_LOAN {
                return Err(UnifiedError::conflict(
                    "A copy on loan cannot change status directly; return the loan first",
                ));
            }
            if checked == COPY_ON_LOAN {
                return Err(UnifiedError::conflict(
                    "A copy cannot be set to on-loan directly; check it out through a loan",
                ));
            }
            copy.status = checked;
        }
        if let Some(notes) = &input.notes {
            copy.notes = match notes {
                Some(value) => Some(require_text(value, "notes", MAX_NOTE_LEN)?),
                None => None,
            };
        }
        copy.updated_at = Utc::now();
        state.copies.insert(copy.id, copy.clone());
        state.emit(
            "library.copy.updated",
            json!({
                "copy_id": copy.id.to_string(),
                "item_id": copy.item_id.to_string(),
                "status": copy.status,
                "actor": caller.actor,
            }),
        );
        Ok(copy)
    })();
    match result {
        Ok(copy) => ok(copy),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_copy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_catalog(&ctx, &caller.role)?;
        let copy = load_copy(&state, &path.into_inner())?;
        super::auth::require_record_writable_in_company_scope(&copy.company_id, &company)?;
        if copy.status == COPY_ON_LOAN {
            return Err(UnifiedError::conflict(
                "A copy on loan cannot be deleted; return the loan first",
            ));
        }
        let has_loans = state
            .loans
            .iter()
            .any(|entry| entry.value().copy_id == Some(copy.id));
        if has_loans {
            return Err(UnifiedError::conflict(
                "A copy with loan history cannot be deleted; withdraw it instead",
            ));
        }
        state.copies.remove(&copy.id);
        state.emit(
            "library.copy.deleted",
            json!({
                "copy_id": copy.id.to_string(),
                "item_id": copy.item_id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "copy_id": copy.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Catalog search handlers

pub(crate) async fn search_catalog(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        check_search_rate(&state, &caller.actor)?;
        let params = query.into_inner();
        let pagination = params.normalized();
        let filters = pagination.filters.clone();
        let mut items: Vec<LibraryItem> = state
            .items
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| {
                item.status == ITEM_ACTIVE
                    && super::auth::in_company_scope(&item.company_id, &company)
            })
            .collect();
        if let Some(search) = &pagination.search {
            if !search.trim().is_empty() {
                items.retain(|item| item_matches_query(item, search));
            }
        }
        if let Some(item_type) = filters.get("item_type") {
            items.retain(|item| &item.item_type == item_type);
        }
        if let Some(language) = filters.get("language") {
            items.retain(|item| &item.language == language);
        }
        if let Some(category_name) = filters.get("category_name") {
            items.retain(|item| {
                item.category_name
                    .as_deref()
                    .map(|name| name == category_name)
                    .unwrap_or(false)
            });
        }
        if let Some(author) = filters.get("author") {
            let needle = author.to_lowercase();
            items.retain(|item| {
                item.authors
                    .iter()
                    .any(|name| name.to_lowercase().contains(&needle))
            });
        }
        if let Some(isbn) = filters.get("isbn") {
            let needle = isbn.replace('-', "").replace(' ', "").to_uppercase();
            items.retain(|item| {
                item.isbn_normalized
                    .as_deref()
                    .map(|value| value.contains(&needle))
                    .unwrap_or(false)
            });
        }
        items.sort_by(|a, b| a.title.cmp(&b.title).then(a.id.cmp(&b.id)));
        // Availability summary per item, computed from live copies.
        let enriched: Vec<Value> = items
            .iter()
            .map(|item| {
                let copies: Vec<LibraryCopy> = state
                    .copies
                    .iter()
                    .filter(|entry| entry.value().item_id == item.id)
                    .map(|entry| entry.value().clone())
                    .collect();
                let available = copies
                    .iter()
                    .filter(|copy| copy.status == COPY_AVAILABLE)
                    .count();
                json!({
                    "item": item,
                    "total_copies": copies.len(),
                    "available_copies": available,
                    "on_loan_copies": copies.iter().filter(|copy| copy.status == COPY_ON_LOAN).count(),
                })
            })
            .collect();
        Ok(paginate(enriched, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_catalog_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let item = load_item(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&item.company_id, &company)?;
        if item.status != ITEM_ACTIVE {
            return Err(UnifiedError::not_found(format!(
                "Catalog item \"{}\" not found",
                item.id
            )));
        }
        let mut copies: Vec<LibraryCopy> = state
            .copies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|copy| copy.item_id == item.id)
            .collect();
        copies.retain(|copy| super::auth::in_company_scope(&copy.company_id, &company));
        copies.sort_by(|a, b| a.barcode.cmp(&b.barcode));
        Ok(json!({ "item": item, "copies": copies }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Category handlers

pub(crate) async fn list_categories(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let params = query.into_inner();
        let pagination = params.normalized();
        let filters = pagination.filters.clone();
        let mut items: Vec<LibraryCategory> = state
            .categories
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items.retain(|category| super::auth::in_company_scope(&category.company_id, &company));
        if let Some(parent_id) = filters.get("parent_id") {
            items.retain(|category| {
                category
                    .parent_id
                    .map(|id| id.to_string() == *parent_id)
                    .unwrap_or(false)
            });
        }
        if let Some(search) = &pagination.search {
            let needle = search.to_lowercase();
            items.retain(|category| {
                category.name.to_lowercase().contains(&needle)
                    || category.slug.to_lowercase().contains(&needle)
            });
        }
        items.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    body: web::Json<CreateCategoryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryCategory> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_catalog(&ctx, &caller.role)?;
        let input = body.into_inner();
        let company_id = super::auth::require_company_match_for(&company, &input.company_id)?;
        let name = require_text(&input.name, "name", MAX_TITLE_LEN)?;
        let slug = match &input.slug {
            Some(raw) => {
                let value = slugify(raw);
                if value.is_empty() {
                    return Err(UnifiedError::bad_request("slug must not be empty"));
                }
                if value.len() > MAX_SLUG_LEN {
                    return Err(UnifiedError::bad_request(format!(
                        "slug must be at most {MAX_SLUG_LEN} characters"
                    )));
                }
                value
            }
            None => slugify(&name),
        };
        if state
            .categories
            .iter()
            .any(|entry| entry.value().slug == slug)
        {
            return Err(UnifiedError::conflict(format!(
                "A category already uses slug \"{slug}\""
            )));
        }
        let parent_id = parse_optional_uuid(&input.parent_id, "category")?;
        if let Some(parent) = parent_id {
            if !state.categories.contains_key(&parent) {
                return Err(UnifiedError::not_found(format!(
                    "Parent category \"{parent}\" not found"
                )));
            }
        }
        let now = Utc::now();
        let category = LibraryCategory {
            id: Uuid::new_v4(),
            name,
            slug,
            description: optional_text(&input.description, "description", MAX_TEXT_LEN)?,
            parent_id,
            company_id,
            created_at: now,
            updated_at: now,
        };
        state.categories.insert(category.id, category.clone());
        Ok(category)
    })();
    match result {
        Ok(category) => created(category),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryCategory> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let category = load_category(&state, &path.into_inner())?;
        super::auth::require_record_in_company_scope(&category.company_id, &company)?;
        Ok(category)
    })();
    match result {
        Ok(category) => ok(category),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCategoryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryCategory> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_catalog(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut category = load_category(&state, &path.into_inner())?;
        super::auth::require_record_writable_in_company_scope(&category.company_id, &company)?;
        if let Some(name) = &input.name {
            category.name = require_text(name, "name", MAX_TITLE_LEN)?;
        }
        if let Some(slug) = &input.slug {
            let value = slugify(slug);
            if value.is_empty() {
                return Err(UnifiedError::bad_request("slug must not be empty"));
            }
            let duplicate = state.categories.iter().any(|entry| {
                entry.value().id != category.id && entry.value().slug == value
            });
            if duplicate {
                return Err(UnifiedError::conflict(format!(
                    "A category already uses slug \"{value}\""
                )));
            }
            category.slug = value;
        }
        if let Some(description) = &input.description {
            category.description = match description {
                Some(value) => Some(require_text(value, "description", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        if let Some(parent_id) = &input.parent_id {
            let parsed = match parent_id {
                Some(raw) => Some(parse_uuid(raw, "category")?),
                None => None,
            };
            if let Some(parent) = parsed {
                if parent == category.id {
                    return Err(UnifiedError::bad_request(
                        "A category cannot be its own parent",
                    ));
                }
                if !state.categories.contains_key(&parent) {
                    return Err(UnifiedError::not_found(format!(
                        "Parent category \"{parent}\" not found"
                    )));
                }
            }
            category.parent_id = parsed;
        }
        category.updated_at = Utc::now();
        state.categories.insert(category.id, category.clone());
        Ok(category)
    })();
    match result {
        Ok(category) => ok(category),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let company = super::auth::caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_catalog(&ctx, &caller.role)?;
        let category = load_category(&state, &path.into_inner())?;
        super::auth::require_record_writable_in_company_scope(&category.company_id, &company)?;
        let has_children = state
            .categories
            .iter()
            .any(|entry| entry.value().parent_id == Some(category.id));
        if has_children {
            return Err(UnifiedError::conflict(
                "A category with child categories cannot be deleted",
            ));
        }
        let in_use = state
            .items
            .iter()
            .any(|entry| entry.value().category_id == Some(category.id));
        if in_use {
            return Err(UnifiedError::conflict(
                "A category used by catalog items cannot be deleted",
            ));
        }
        state.categories.remove(&category.id);
        Ok(json!({ "deleted": true, "category_id": category.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
