//! HTTP handlers for workbooks and sheets: CRUD for both,
//! with tenant scoping (cross-tenant reads as 404), owner /
//! sharee access, and workbook events.

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    apply_cell_write, caller, caller_company, created, err, generate_public_token, hash_value,
    load_sheet, load_workbook, load_workbook_readable, load_workbook_writable, ok, optional_text,
    paginate, require_caller_company, require_manage, require_tenant, require_text, require_view,
    validate_sheet_titles, validate_workbook_input, workbook_view,
};
use super::state::SpreadsheetsState;
use super::types::{
    CreateSheetRequest, CreateWorkbookRequest, Sheet, UpdateSheetRequest, UpdateWorkbookRequest,
    Workbook, EVENT_WORKBOOK_CREATED, EVENT_WORKBOOK_DELETED, EVENT_WORKBOOK_UPDATED,
};

// ---------------------------------------------------------------------------
// Workbooks
// ---------------------------------------------------------------------------

pub(crate) async fn list_workbooks(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let params = query.into_inner();
        let owner_filter = params.filters.get("owner_id").cloned();
        let updated_after = params.filters.get("updated_after").cloned();
        let updated_before = params.filters.get("updated_before").cloned();
        let search = params.search.clone();
        let mut items: Vec<Workbook> = state
            .workbooks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|workbook| workbook.tenant_id == tenant_id)
            .filter(|workbook| state.can_read(workbook, &caller.actor))
            .filter(|workbook| {
                if let Some(owner) = &owner_filter {
                    if workbook.owner_id != *owner {
                        return false;
                    }
                }
                if let Some(after) = &updated_after {
                    if workbook.updated_at.to_rfc3339() < *after {
                        return false;
                    }
                }
                if let Some(before) = &updated_before {
                    if workbook.updated_at.to_rfc3339() > *before {
                        return false;
                    }
                }
                if let Some(needle) = &search {
                    let needle = needle.to_lowercase();
                    if !workbook.title.to_lowercase().contains(&needle)
                        && !workbook
                            .description
                            .as_deref()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&needle)
                    {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        let views: Vec<Value> = items
            .iter()
            .map(|item| workbook_view(&state, item))
            .collect();
        Ok(serde_json::to_value(paginate(views, &params)).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_workbook(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateWorkbookRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let (title, description) = validate_workbook_input(&input.title, &input.description)?;
        let sheet_titles = if input.sheets.is_empty() {
            vec!["Sheet1".to_string()]
        } else {
            validate_sheet_titles(&input.sheets)?
        };
        let settings = state.settings_for(&tenant_id);
        if sheet_titles.len() as i64 > settings.max_sheets_per_workbook {
            return Err(UnifiedError::bad_request(format!(
                "At most {} sheets per workbook",
                settings.max_sheets_per_workbook
            )));
        }
        let now = Utc::now();
        let workbook = Workbook {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            title,
            description,
            owner_id: caller.actor.clone(),
            public_link_enabled: false,
            public_token_hash: String::new(),
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.workbooks.insert(workbook.id, workbook.clone());
        for (position, sheet_title) in sheet_titles.iter().enumerate() {
            let sheet = Sheet {
                id: Uuid::new_v4(),
                tenant_id: tenant_id.clone(),
                workbook_id: workbook.id,
                title: sheet_title.clone(),
                position: position as i64,
                created_at: now,
                updated_at: now,
            };
            state.sheets.insert(sheet.id, sheet);
        }
        state.emit(
            EVENT_WORKBOOK_CREATED,
            json!({
                "tenant_id": tenant_id,
                "workbook_id": workbook.id.to_string(),
                "title": workbook.title,
                "owner_id": workbook.owner_id,
            }),
        );
        Ok(workbook_view(&state, &workbook))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_workbook(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let workbook =
            load_workbook_readable(&state, &tenant_id, &path.into_inner(), &caller.actor)?;
        Ok(workbook_view(&state, &workbook))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_workbook(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<UpdateWorkbookRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let mut workbook =
            load_workbook_writable(&state, &tenant_id, &path.into_inner(), &caller.actor)?;
        let input = body.into_inner();
        if let Some(title) = input.title {
            workbook.title = require_text(&title, "title", super::types::MAX_TITLE_LEN)?;
        }
        if let Some(description) = input.description {
            workbook.description = optional_text(
                &description,
                "description",
                super::types::MAX_DESCRIPTION_LEN,
            )?;
        }
        // Public-link toggle: enabling mints a fresh token and
        // returns it exactly once; disabling revokes it. The
        // tenant switch must allow public links at all.
        let mut raw_token: Option<String> = None;
        if let Some(enabled) = input.public_link_enabled {
            if enabled && !workbook.public_link_enabled {
                let settings = state.settings_for(&tenant_id);
                if !settings.public_links_allowed {
                    return Err(UnifiedError::forbidden(
                        "Public links are not allowed for this tenant",
                    ));
                }
                let token = generate_public_token();
                workbook.public_token_hash = hash_value(&token);
                workbook.public_link_enabled = true;
                raw_token = Some(token);
            } else if !enabled && workbook.public_link_enabled {
                workbook.public_link_enabled = false;
                workbook.public_token_hash = String::new();
            }
        }
        workbook.updated_at = Utc::now();
        state.workbooks.insert(workbook.id, workbook.clone());
        state.emit(
            EVENT_WORKBOOK_UPDATED,
            json!({
                "tenant_id": tenant_id,
                "workbook_id": workbook.id.to_string(),
                "title": workbook.title,
                "public_link_enabled": workbook.public_link_enabled,
            }),
        );
        let mut view = workbook_view(&state, &workbook);
        if let (Some(token), Value::Object(map)) = (raw_token, &mut view) {
            // Returned exactly once: the caller must store it,
            // the hash is what is kept.
            map.insert("public_token".to_string(), Value::String(token));
        }
        Ok(view)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_workbook(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let workbook = load_workbook(&state, &tenant_id, &path.into_inner())?;
        // Only the owner deletes a workbook: a sharee with an
        // editor share may edit cells, never delete the document.
        if workbook.owner_id != caller.actor {
            return Err(UnifiedError::not_found(format!(
                "Workbook not found: {}",
                workbook.id
            )));
        }
        let sheets = state.sheets_of(workbook.id);
        for sheet in &sheets {
            let cell_ids: Vec<Uuid> = state
                .cells
                .iter()
                .filter(|entry| entry.value().sheet_id == sheet.id)
                .map(|entry| *entry.key())
                .collect();
            for id in cell_ids {
                state.cells.remove(&id);
            }
            let version_ids: Vec<Uuid> = state
                .versions
                .iter()
                .filter(|entry| entry.value().sheet_id == sheet.id)
                .map(|entry| *entry.key())
                .collect();
            for id in version_ids {
                state.versions.remove(&id);
            }
            state.sheets.remove(&sheet.id);
        }
        let share_ids: Vec<Uuid> = state
            .shares
            .iter()
            .filter(|entry| entry.value().workbook_id == workbook.id)
            .map(|entry| *entry.key())
            .collect();
        for id in share_ids {
            state.shares.remove(&id);
        }
        state.workbooks.remove(&workbook.id);
        state.emit(
            EVENT_WORKBOOK_DELETED,
            json!({
                "tenant_id": tenant_id,
                "workbook_id": workbook.id.to_string(),
                "title": workbook.title,
            }),
        );
        Ok(json!({ "deleted": workbook.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Sheets
// ---------------------------------------------------------------------------

pub(crate) async fn list_sheets(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let workbook =
            load_workbook_readable(&state, &tenant_id, &path.into_inner(), &caller.actor)?;
        let params = query.into_inner();
        let search = params.search.clone();
        let mut items = state.sheets_of(workbook.id);
        if let Some(needle) = &search {
            let needle = needle.to_lowercase();
            items.retain(|sheet| sheet.title.to_lowercase().contains(&needle));
        }
        let views: Vec<Value> = items
            .iter()
            .map(|item| serde_json::to_value(item).unwrap_or(Value::Null))
            .collect();
        Ok(serde_json::to_value(paginate(views, &params)).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_sheet(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<CreateSheetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let workbook =
            load_workbook_writable(&state, &tenant_id, &path.into_inner(), &caller.actor)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", super::types::MAX_TITLE_LEN)?;
        let settings = state.settings_for(&tenant_id);
        let existing = state.sheets_of(workbook.id);
        if existing.len() as i64 >= settings.max_sheets_per_workbook {
            return Err(UnifiedError::bad_request(format!(
                "At most {} sheets per workbook",
                settings.max_sheets_per_workbook
            )));
        }
        if existing
            .iter()
            .any(|sheet| sheet.title.eq_ignore_ascii_case(&title))
        {
            return Err(UnifiedError::conflict(format!(
                "A sheet named \"{title}\" already exists in this workbook"
            )));
        }
        let position = input.position.unwrap_or_else(|| {
            existing
                .iter()
                .map(|sheet| sheet.position)
                .max()
                .unwrap_or(-1)
                + 1
        });
        if position < 0 {
            return Err(UnifiedError::bad_request("position must not be negative"));
        }
        let now = Utc::now();
        let sheet = Sheet {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            workbook_id: workbook.id,
            title,
            position,
            created_at: now,
            updated_at: now,
        };
        state.sheets.insert(sheet.id, sheet.clone());
        let mut workbook = workbook;
        workbook.updated_at = now;
        state.workbooks.insert(workbook.id, workbook.clone());
        state.emit(
            EVENT_WORKBOOK_UPDATED,
            json!({
                "tenant_id": tenant_id,
                "workbook_id": workbook.id.to_string(),
                "sheet_id": sheet.id.to_string(),
                "title": sheet.title,
            }),
        );
        Ok(serde_json::to_value(sheet).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_sheet(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let (workbook_id, sheet_id) = path.into_inner();
        let workbook = load_workbook_readable(&state, &tenant_id, &workbook_id, &caller.actor)?;
        let sheet = load_sheet(&state, &tenant_id, &workbook, &sheet_id)?;
        Ok(super::helpers::sheet_view(&state, &sheet))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_sheet(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateSheetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let (workbook_id, sheet_id) = path.into_inner();
        let workbook = load_workbook_writable(&state, &tenant_id, &workbook_id, &caller.actor)?;
        let mut sheet = load_sheet(&state, &tenant_id, &workbook, &sheet_id)?;
        let input = body.into_inner();
        if let Some(title) = input.title {
            let title = require_text(&title, "title", super::types::MAX_TITLE_LEN)?;
            if state
                .sheets_of(workbook.id)
                .iter()
                .any(|other| other.id != sheet.id && other.title.eq_ignore_ascii_case(&title))
            {
                return Err(UnifiedError::conflict(format!(
                    "A sheet named \"{title}\" already exists in this workbook"
                )));
            }
            sheet.title = title;
        }
        if let Some(position) = input.position {
            if position < 0 {
                return Err(UnifiedError::bad_request("position must not be negative"));
            }
            sheet.position = position;
        }
        sheet.updated_at = Utc::now();
        state.sheets.insert(sheet.id, sheet.clone());
        Ok(serde_json::to_value(sheet).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_sheet(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let (workbook_id, sheet_id) = path.into_inner();
        let workbook = load_workbook_writable(&state, &tenant_id, &workbook_id, &caller.actor)?;
        let sheet = load_sheet(&state, &tenant_id, &workbook, &sheet_id)?;
        if state.sheets_of(workbook.id).len() <= 1 {
            return Err(UnifiedError::bad_request(
                "A workbook must keep at least one sheet",
            ));
        }
        let cell_ids: Vec<Uuid> = state
            .cells
            .iter()
            .filter(|entry| entry.value().sheet_id == sheet.id)
            .map(|entry| *entry.key())
            .collect();
        for id in cell_ids {
            state.cells.remove(&id);
        }
        let version_ids: Vec<Uuid> = state
            .versions
            .iter()
            .filter(|entry| entry.value().sheet_id == sheet.id)
            .map(|entry| *entry.key())
            .collect();
        for id in version_ids {
            state.versions.remove(&id);
        }
        state.sheets.remove(&sheet.id);
        Ok(json!({ "deleted": sheet.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Re-exported for the version restore handler: replace a
/// sheet's cells with a snapshot, then recalculate.
pub(crate) fn restore_cells_from_snapshot(
    state: &SpreadsheetsState,
    tenant_id: &str,
    workbook: &Workbook,
    sheet: &Sheet,
    snapshot: &[super::types::CellSnapshot],
) {
    let cell_ids: Vec<Uuid> = state
        .cells
        .iter()
        .filter(|entry| entry.value().sheet_id == sheet.id)
        .map(|entry| *entry.key())
        .collect();
    for id in cell_ids {
        state.cells.remove(&id);
    }
    let settings = state.settings_for(tenant_id);
    for snap in snapshot {
        if let Some((row, col)) = super::types::parse_address(&snap.address) {
            if !super::types::address_in_bounds(row, col) {
                continue;
            }
            if snap.raw_value.len() > super::types::MAX_RAW_VALUE_LEN {
                continue;
            }
            let format = snap.number_format.as_ref().filter(|name| {
                settings
                    .number_formats
                    .iter()
                    .any(|known| &known.name == *name)
            });
            let _ = apply_cell_write(
                state,
                tenant_id,
                workbook,
                sheet,
                row,
                col,
                &snap.raw_value,
                format.cloned(),
            );
        }
    }
    super::helpers::recalculate_sheet_cells(state, sheet);
}
