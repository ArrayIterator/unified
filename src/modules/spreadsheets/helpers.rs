//! Caller, permission, loading, view, CSV, and recalculation
//! helpers.

use super::formula::{recalculate_sheet, RecalcInput};
use super::state::SpreadsheetsState;
use super::types::{
    address_in_bounds, classify_value, computed_for_typed, format_address, parse_address, CellKind,
    Share, Sheet, SheetCell, SpreadsheetSettings, Workbook, MAX_CELLS_PER_BATCH, MAX_COLUMNS,
    MAX_DESCRIPTION_LEN, MAX_FORMAT_NAME_LEN, MAX_RAW_VALUE_LEN, MAX_ROWS, MAX_TITLE_LEN,
    MAX_USER_ID_LEN, PERM_SPREADSHEETS_MANAGE, PERM_SPREADSHEETS_VIEW,
};
use actix_web::{HttpRequest, HttpResponse};
use chrono::Utc;
use rand::{distr::Alphanumeric, Rng};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::Digest;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use uuid::Uuid;

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the sign module.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(
    ctx: &AppContext,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

pub(crate) fn require_view(ctx: &AppContext, role: &str) -> UnifiedResult<()> {
    require_permission(ctx, role, PERM_SPREADSHEETS_VIEW)
}

pub(crate) fn require_manage(ctx: &AppContext, role: &str) -> UnifiedResult<()> {
    require_permission(ctx, role, PERM_SPREADSHEETS_MANAGE)
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 5, group E).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
/// It replaces the old `resolve_tenant` helper, which returned only
/// the tenant id string: handlers now keep the full context so the
/// company scope can be resolved from it.
///
/// Records in this module carry `tenant_id`, not `company_id`, and
/// every state accessor / loader takes that tenant id, so the
/// tenant gate is also the record partition. The company scope
/// below is enforced as a mismatch check on reads and as a
/// presence check on writes; there is no per-record company field
/// to filter on (a C11 follow-up if company partitioning is
/// wanted).
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// -- a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources.
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified),
    }
}

/// Writes require a resolved company: a mutating handler with no
/// resolvable company scope is refused (403). Reads proceed on the
/// tenant gate plus the mismatch check above.
pub(crate) fn require_caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<String> {
    caller_company(tenant, req)?
        .ok_or_else(|| UnifiedError::forbidden("A write requires a resolved company scope"))
}

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be empty"
        )));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn optional_text(
    raw: &Option<String>,
    field: &str,
    max: usize,
) -> UnifiedResult<Option<String>> {
    match raw {
        Some(text) => Ok(Some(require_text(text, field, max)?)),
        None => Ok(None),
    }
}

pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

pub(crate) fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

pub(crate) fn paginate<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

/// SHA-256 hex digest, for public-link token storage: the raw
/// token is never stored.
pub fn hash_value(value: &str) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap_or('0'));
    }
    out
}

/// One unguessable public-link token (48 random alphanumeric
/// characters). Only its SHA-256 hash is ever stored.
pub fn generate_public_token() -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(super::types::PUBLIC_TOKEN_LEN)
        .map(char::from)
        .collect()
}

/// Today's date as `YYYY-MM-DD` (UTC), for `TODAY()`.
pub fn today_string() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate a workbook title and optional description.
pub(crate) fn validate_workbook_input(
    title: &str,
    description: &Option<String>,
) -> UnifiedResult<(String, Option<String>)> {
    let title = require_text(title, "title", MAX_TITLE_LEN)?;
    let description = optional_text(description, "description", MAX_DESCRIPTION_LEN)?;
    Ok((title, description))
}

/// Validate sheet titles: non-empty, within bounds, no
/// duplicates (case-insensitive) within the supplied list.
pub(crate) fn validate_sheet_titles(titles: &[String]) -> UnifiedResult<Vec<String>> {
    let mut seen = std::collections::HashSet::new();
    let mut clean = Vec::new();
    for title in titles {
        let title = require_text(title, "sheet title", MAX_TITLE_LEN)?;
        if !seen.insert(title.to_lowercase()) {
            return Err(UnifiedError::bad_request(format!(
                "Sheet titles must not repeat \"{title}\""
            )));
        }
        clean.push(title);
    }
    Ok(clean)
}

/// Validate one cell write: the address must parse and be in
/// bounds, the value within the length bound, and the named
/// number format (when given) must exist in tenant settings.
pub(crate) fn validate_cell_input(
    address: &str,
    value: &str,
    number_format: &Option<String>,
    settings: &SpreadsheetSettings,
) -> UnifiedResult<(i64, i64, String)> {
    let (row, col) = parse_address(address)
        .ok_or_else(|| UnifiedError::bad_request(format!("Invalid cell address: {address}")))?;
    if !address_in_bounds(row, col) {
        return Err(UnifiedError::bad_request(format!(
            "Cell address out of bounds (at most {MAX_ROWS} rows, {MAX_COLUMNS} columns): {address}"
        )));
    }
    if value.len() > MAX_RAW_VALUE_LEN {
        return Err(UnifiedError::bad_request(format!(
            "Cell value must be at most {MAX_RAW_VALUE_LEN} characters"
        )));
    }
    if let Some(format) = number_format {
        let format = require_text(format, "number_format", MAX_FORMAT_NAME_LEN)?;
        if !settings
            .number_formats
            .iter()
            .any(|known| known.name == format)
        {
            return Err(UnifiedError::bad_request(format!(
                "Unknown number format: \"{format}\""
            )));
        }
    }
    Ok((row, col, format_address(row, col)))
}

/// Validate a batch of cell writes: at most the module bound,
/// no repeated address.
pub(crate) fn validate_cell_batch(cells: &[super::types::CellInput]) -> UnifiedResult<()> {
    if cells.is_empty() {
        return Err(UnifiedError::bad_request(
            "cells must contain at least one cell",
        ));
    }
    if cells.len() > MAX_CELLS_PER_BATCH {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_CELLS_PER_BATCH} cells per batch"
        )));
    }
    let mut seen = std::collections::HashSet::new();
    for cell in cells {
        let address = cell.address.trim().to_uppercase();
        if !seen.insert(address) {
            return Err(UnifiedError::bad_request(format!(
                "Batch must not repeat cell \"{}\"",
                cell.address
            )));
        }
    }
    Ok(())
}

/// Validate a share role against the tenant settings' role
/// list (roles are data, never hard-coded here).
pub(crate) fn validate_share_role(
    role: &str,
    settings: &SpreadsheetSettings,
) -> UnifiedResult<String> {
    let role = require_text(role, "role", MAX_USER_ID_LEN.min(64))?;
    if !settings.share_roles.iter().any(|known| *known == role) {
        return Err(UnifiedError::bad_request(format!(
            "Unknown share role: \"{role}\""
        )));
    }
    Ok(role)
}

/// Validate per-tenant settings supplied as data.
pub(crate) fn validate_settings(settings: &SpreadsheetSettings) -> UnifiedResult<()> {
    if settings.default_currency.trim().is_empty()
        || settings.default_currency.len() > super::types::MAX_CURRENCY_LEN
    {
        return Err(UnifiedError::bad_request(
            "default_currency must be 1 to 8 characters",
        ));
    }
    if settings.number_formats.is_empty() {
        return Err(UnifiedError::bad_request(
            "number_formats must contain at least one format",
        ));
    }
    let mut seen_formats = std::collections::HashSet::new();
    for format in &settings.number_formats {
        let name = require_text(&format.name, "number format name", MAX_FORMAT_NAME_LEN)?;
        if format.pattern.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "number format pattern must not be empty",
            ));
        }
        if !seen_formats.insert(name) {
            return Err(UnifiedError::bad_request(
                "number_formats must not repeat a format name",
            ));
        }
    }
    if settings.share_roles.is_empty() {
        return Err(UnifiedError::bad_request(
            "share_roles must contain at least one role",
        ));
    }
    if !settings
        .share_roles
        .iter()
        .any(|role| *role == settings.default_share_role)
    {
        return Err(UnifiedError::bad_request(
            "default_share_role must be one of share_roles",
        ));
    }
    if settings.max_sheets_per_workbook < 1
        || settings.max_sheets_per_workbook > super::types::MAX_SHEETS_PER_WORKBOOK as i64
    {
        return Err(UnifiedError::bad_request(format!(
            "max_sheets_per_workbook must be between 1 and {}",
            super::types::MAX_SHEETS_PER_WORKBOOK
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tenant-scoped loaders (cross-tenant reads as 404, never 403)
// ---------------------------------------------------------------------------

fn tenant_match(row_tenant_id: &str, tenant_id: &str, id: Uuid, what: &str) -> UnifiedResult<()> {
    if row_tenant_id != tenant_id {
        return Err(UnifiedError::not_found(format!("{what} not found: {id}")));
    }
    Ok(())
}

pub(crate) fn load_workbook(
    state: &SpreadsheetsState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<Workbook> {
    let id = parse_uuid(raw_id, "workbook")?;
    let workbook = state
        .workbooks
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Workbook not found: {id}")))?;
    tenant_match(&workbook.tenant_id, tenant_id, id, "Workbook")?;
    Ok(workbook)
}

/// Load a workbook the caller may read (owner or sharee).
/// A workbook the caller cannot read at all reads as 404.
pub(crate) fn load_workbook_readable(
    state: &SpreadsheetsState,
    tenant_id: &str,
    raw_id: &str,
    user_id: &str,
) -> UnifiedResult<Workbook> {
    let workbook = load_workbook(state, tenant_id, raw_id)?;
    if !state.can_read(&workbook, user_id) {
        return Err(UnifiedError::not_found(format!(
            "Workbook not found: {}",
            workbook.id
        )));
    }
    Ok(workbook)
}

/// Load a workbook the caller may write (owner or editor share).
pub(crate) fn load_workbook_writable(
    state: &SpreadsheetsState,
    tenant_id: &str,
    raw_id: &str,
    user_id: &str,
) -> UnifiedResult<Workbook> {
    let workbook = load_workbook(state, tenant_id, raw_id)?;
    if !state.can_write(&workbook, user_id) {
        return Err(UnifiedError::not_found(format!(
            "Workbook not found: {}",
            workbook.id
        )));
    }
    Ok(workbook)
}

pub(crate) fn load_sheet(
    state: &SpreadsheetsState,
    tenant_id: &str,
    workbook: &Workbook,
    raw_id: &str,
) -> UnifiedResult<Sheet> {
    let id = parse_uuid(raw_id, "sheet")?;
    let sheet = state
        .sheets
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Sheet not found: {id}")))?;
    tenant_match(&sheet.tenant_id, tenant_id, id, "Sheet")?;
    if sheet.workbook_id != workbook.id {
        return Err(UnifiedError::not_found(format!("Sheet not found: {id}")));
    }
    Ok(sheet)
}

pub(crate) fn load_share(
    state: &SpreadsheetsState,
    tenant_id: &str,
    workbook: &Workbook,
    raw_id: &str,
) -> UnifiedResult<Share> {
    let id = parse_uuid(raw_id, "share")?;
    let share = state
        .shares
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Share not found: {id}")))?;
    tenant_match(&share.tenant_id, tenant_id, id, "Share")?;
    if share.workbook_id != workbook.id {
        return Err(UnifiedError::not_found(format!("Share not found: {id}")));
    }
    Ok(share)
}

// ---------------------------------------------------------------------------
// Cells: write + recalculate + version
// ---------------------------------------------------------------------------

/// Build a fresh cell row from a typed value: classify the
/// value, compute its typed result, and fill in the ids.
pub(crate) fn build_cell(
    tenant_id: &str,
    workbook_id: Uuid,
    sheet_id: Uuid,
    row: i64,
    col: i64,
    value: &str,
    number_format: Option<String>,
) -> SheetCell {
    let now = Utc::now();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return SheetCell {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.to_string(),
            workbook_id,
            sheet_id,
            address: format_address(row, col),
            row_index: row,
            column_index: col,
            kind: CellKind::Empty,
            raw_value: String::new(),
            computed_value: String::new(),
            computed_kind: CellKind::Empty,
            number_format,
            created_at: now,
            updated_at: now,
        };
    }
    if trimmed.starts_with('=') {
        return SheetCell {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.to_string(),
            workbook_id,
            sheet_id,
            address: format_address(row, col),
            row_index: row,
            column_index: col,
            kind: CellKind::Formula,
            raw_value: trimmed.to_string(),
            computed_value: String::new(),
            computed_kind: CellKind::Empty,
            number_format,
            created_at: now,
            updated_at: now,
        };
    }
    let kind = classify_value(trimmed);
    let computed = computed_for_typed(trimmed, kind);
    SheetCell {
        id: Uuid::new_v4(),
        tenant_id: tenant_id.to_string(),
        workbook_id,
        sheet_id,
        address: format_address(row, col),
        row_index: row,
        column_index: col,
        kind,
        raw_value: trimmed.to_string(),
        computed_value: computed,
        computed_kind: kind,
        number_format,
        created_at: now,
        updated_at: now,
    }
}

/// Recompute every formula cell of a sheet in topological
/// order and store the results back. Returns the recomputed
/// sheet cells.
pub(crate) fn recalculate_sheet_cells(state: &SpreadsheetsState, sheet: &Sheet) -> Vec<SheetCell> {
    let stored = state.cells_of(sheet.id);
    let mut inputs: RecalcInput = HashMap::new();
    for cell in &stored {
        inputs.insert(
            (cell.row_index, cell.column_index),
            (cell.raw_value.clone(), cell.kind),
        );
    }
    let computed = recalculate_sheet(&inputs, &today_string());
    let now = Utc::now();
    let mut out = Vec::new();
    for mut cell in stored {
        if let Some((value, kind)) = computed.get(&(cell.row_index, cell.column_index)) {
            cell.computed_value = value.clone();
            cell.computed_kind = *kind;
            if cell.kind == CellKind::Formula && *kind == CellKind::Error {
                cell.computed_kind = CellKind::Error;
            }
            cell.updated_at = now;
            state.cells.insert(cell.id, cell.clone());
        }
        out.push(cell);
    }
    out
}

/// Create a version snapshot of a sheet after a write batch.
/// `changes` is the human-readable change log (one line per
/// changed address, `A1: before -> after`); the snapshot holds
/// every non-empty cell so a restore reproduces the sheet.
pub(crate) fn snapshot_version(
    state: &SpreadsheetsState,
    tenant_id: &str,
    workbook: &Workbook,
    sheet: &Sheet,
    actor: &str,
    changes: &[String],
) {
    let cells = state.cells_of(sheet.id);
    let snapshot: Vec<super::types::CellSnapshot> = cells
        .iter()
        .filter(|cell| cell.kind != CellKind::Empty)
        .map(|cell| super::types::CellSnapshot {
            address: cell.address.clone(),
            kind: cell.kind,
            raw_value: cell.raw_value.clone(),
            number_format: cell.number_format.clone(),
        })
        .collect();
    let version = super::types::SheetVersion {
        id: Uuid::new_v4(),
        tenant_id: tenant_id.to_string(),
        workbook_id: workbook.id,
        sheet_id: sheet.id,
        version_number: state.next_version_number(sheet.id),
        created_by: actor.to_string(),
        change_summary: changes.join("\n"),
        cells_snapshot: snapshot,
        created_at: Utc::now(),
    };
    state.versions.insert(version.id, version);
    // Prune versions past the module bound, oldest first.
    let mut versions = state.versions_of(sheet.id);
    if versions.len() as i64 > super::types::MAX_VERSIONS_PER_SHEET {
        versions.sort_by_key(|version| version.version_number);
        let excess = versions.len() - super::types::MAX_VERSIONS_PER_SHEET as usize;
        for old in versions.iter().take(excess) {
            state.versions.remove(&old.id);
        }
    }
}

/// Apply one cell write to the store (insert or replace the
/// row for the address), returning the previous raw value
/// (`None` when the cell was empty) for the change log.
pub(crate) fn apply_cell_write(
    state: &SpreadsheetsState,
    tenant_id: &str,
    workbook: &Workbook,
    sheet: &Sheet,
    row: i64,
    col: i64,
    value: &str,
    number_format: Option<String>,
) -> Option<String> {
    let previous = state
        .cells
        .iter()
        .find(|entry| {
            entry.value().sheet_id == sheet.id
                && entry.value().row_index == row
                && entry.value().column_index == col
        })
        .map(|entry| entry.value().raw_value.clone());
    let previous_kind = state
        .cells
        .iter()
        .find(|entry| {
            entry.value().sheet_id == sheet.id
                && entry.value().row_index == row
                && entry.value().column_index == col
        })
        .map(|entry| entry.value().kind);
    if value.trim().is_empty() {
        // Clearing a cell removes its row.
        let ids: Vec<Uuid> = state
            .cells
            .iter()
            .filter(|entry| {
                entry.value().sheet_id == sheet.id
                    && entry.value().row_index == row
                    && entry.value().column_index == col
            })
            .map(|entry| *entry.key())
            .collect();
        for id in ids {
            state.cells.remove(&id);
        }
        return previous.filter(|_| previous_kind != Some(CellKind::Empty));
    }
    let mut cell = build_cell(
        tenant_id,
        workbook.id,
        sheet.id,
        row,
        col,
        value,
        number_format,
    );
    // Keep the original row id and creation time on replace.
    if let Some(existing) = state.cells.iter().find(|entry| {
        entry.value().sheet_id == sheet.id
            && entry.value().row_index == row
            && entry.value().column_index == col
    }) {
        cell.id = *existing.key();
        cell.created_at = existing.value().created_at;
    }
    state.cells.insert(cell.id, cell);
    previous
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

/// A workbook view with its sheets and share count attached.
pub(crate) fn workbook_view(state: &SpreadsheetsState, workbook: &Workbook) -> Value {
    let mut value = serde_json::to_value(workbook).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        let sheets: Vec<Value> = state
            .sheets_of(workbook.id)
            .iter()
            .map(|sheet| serde_json::to_value(sheet).unwrap_or(Value::Null))
            .collect();
        map.insert("sheets".to_string(), Value::Array(sheets));
        map.insert(
            "share_count".to_string(),
            Value::from(state.shares_of(workbook.id).len() as u64),
        );
    }
    value
}

/// A sheet view with its non-empty cells attached.
pub(crate) fn sheet_view(state: &SpreadsheetsState, sheet: &Sheet) -> Value {
    let mut value = serde_json::to_value(sheet).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        let cells: Vec<Value> = state
            .cells_of(sheet.id)
            .iter()
            .map(|cell| serde_json::to_value(cell).unwrap_or(Value::Null))
            .collect();
        map.insert("cells".to_string(), Value::Array(cells));
        map.insert("cell_count".to_string(), Value::from(cells.len() as u64));
    }
    value
}

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// CSV (pure functions, unit-tested below)
// ---------------------------------------------------------------------------

/// Parse CSV text into rows of fields. Handles quoted fields
/// (with `""` escapes), commas and newlines inside quotes, and
/// `\r\n` line endings. A trailing empty line is ignored.
pub fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let chars: Vec<char> = text.chars().collect();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut index = 0usize;
    let mut field_started = false;
    while index < chars.len() {
        let current = chars[index];
        if in_quotes {
            if current == '"' {
                if chars.get(index + 1) == Some(&'"') {
                    field.push('"');
                    index += 2;
                } else {
                    in_quotes = false;
                    index += 1;
                }
            } else {
                field.push(current);
                index += 1;
            }
        } else if current == '"' && !field_started {
            in_quotes = true;
            field_started = true;
            index += 1;
        } else if current == ',' {
            row.push(std::mem::take(&mut field));
            field_started = false;
            index += 1;
        } else if current == '\r' {
            // Skip; the \n that follows ends the row.
            index += 1;
        } else if current == '\n' {
            row.push(std::mem::take(&mut field));
            field_started = false;
            rows.push(std::mem::take(&mut row));
            index += 1;
        } else {
            field.push(current);
            field_started = true;
            index += 1;
        }
    }
    if in_quotes {
        // Unterminated quote: keep what was read.
        row.push(field);
        rows.push(row);
        return rows;
    }
    if field_started || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    // Drop a single trailing empty row from a trailing newline.
    while let Some(last) = rows.last() {
        if last.len() == 1 && last[0].is_empty() {
            rows.pop();
        } else {
            break;
        }
    }
    rows
}

/// Quote one CSV field: quote when it contains a comma, a
/// quote, or a newline; double any quotes inside.
pub fn csv_field(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

/// Render rows of fields as CSV text (CRLF line endings).
pub fn render_csv(rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    for row in rows {
        let fields: Vec<String> = row.iter().map(|field| csv_field(field)).collect();
        out.push_str(&fields.join(","));
        out.push_str("\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn pagination_params(page: Option<u32>, per_page: Option<u32>) -> PaginationParams {
        PaginationParams {
            page,
            per_page,
            sort: None,
            order: None,
            search: None,
            filters: HashMap::new(),
        }
    }

    #[test]
    fn paginate_bounds() {
        let items: Vec<i64> = (1..=5).collect();
        let first = paginate(items.clone(), &pagination_params(Some(1), Some(2)));
        assert_eq!(first.items, vec![1, 2]);
        assert_eq!(first.pagination.total, 5);
        let past_end = paginate(items.clone(), &pagination_params(Some(4), Some(2)));
        assert!(past_end.items.is_empty());
        assert_eq!(past_end.pagination.total, 5);
    }

    #[test]
    fn token_hash_is_stable_sha256() {
        let token = "abc123";
        assert_eq!(hash_value(token), hash_value(token));
        assert_eq!(hash_value(token).len(), 64);
        assert_eq!(
            hash_value(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_ne!(hash_value(token), hash_value("other"));
        assert_eq!(
            generate_public_token().len(),
            super::types::PUBLIC_TOKEN_LEN
        );
    }

    #[test]
    fn csv_parses_quotes_commas_and_newlines() {
        let rows = parse_csv("a,b,c\r\n\"x,y\",\"say \"\"hi\"\"\",3\r\n\"line1\nline2\",b,");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], vec!["a", "b", "c"]);
        assert_eq!(rows[1], vec!["x,y", "say \"hi\"", "3"]);
        assert_eq!(rows[2], vec!["line1\nline2", "b", ""]);
        assert!(parse_csv("").is_empty());
        assert_eq!(parse_csv("only"), vec![vec!["only"]]);
    }

    #[test]
    fn csv_renders_with_quoting() {
        let rows = vec![
            vec!["a".to_string(), "x,y".to_string()],
            vec!["say \"hi\"".to_string(), "plain".to_string()],
        ];
        let text = render_csv(&rows);
        assert_eq!(text, "a,\"x,y\"\r\n\"say \"\"hi\"\"\",plain\r\n");
        // Round trip.
        assert_eq!(parse_csv(&text), rows);
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
    }

    #[test]
    fn settings_validation() {
        assert!(validate_settings(&SpreadsheetSettings::default()).is_ok());
        let mut bad = SpreadsheetSettings::default();
        bad.share_roles = vec!["owner".to_string()];
        assert!(validate_settings(&bad).is_err());
        let mut bad = SpreadsheetSettings::default();
        bad.max_sheets_per_workbook = 0;
        assert!(validate_settings(&bad).is_err());
        let mut bad = SpreadsheetSettings::default();
        bad.number_formats = vec![];
        assert!(validate_settings(&bad).is_err());
        assert!(validate_share_role("viewer", &SpreadsheetSettings::default()).is_ok());
        assert!(validate_share_role("admin", &SpreadsheetSettings::default()).is_err());
    }

    /// Request carrying the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach, plus a verified
    /// caller (optionally with a server-resolved company).
    fn tenant_request(company_id: &str, caller_company: Option<&str>) -> HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: company_id.to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let mut caller = unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new());
        if let Some(company) = caller_company {
            caller = caller.with_company(company.to_string());
        }
        req.extensions_mut().insert(caller);
        req
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn caller_company_prefers_tenant_company() {
        let req = tenant_request("company-1", Some("company-1"));
        let tenant = require_tenant(&req).expect("tenant resolves");
        assert_eq!(
            caller_company(&tenant, &req).unwrap().as_deref(),
            Some("company-1")
        );
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = tenant_request("company-1", Some("company-2"));
        let tenant = require_tenant(&req).expect("tenant resolves");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn require_caller_company_refuses_writes_without_a_resolved_company() {
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant resolves");
        assert!(require_caller_company(&tenant, &req).is_err());
        let req = tenant_request("company-1", None);
        let tenant = require_tenant(&req).expect("tenant resolves");
        assert_eq!(require_caller_company(&tenant, &req).unwrap(), "company-1");
    }
}
