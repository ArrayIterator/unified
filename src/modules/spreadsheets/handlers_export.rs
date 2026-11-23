//! HTTP handlers for import / export: CSV import into a
//! sheet, CSV export of a sheet or a whole workbook, and JSON
//! export of a workbook.
//!
//! XLSX is deliberately NOT offered: no XLSX crate is in the
//! workspace `Cargo.toml` / `Cargo.lock` (checked before
//! writing this module — only the transitive `zip` crate is
//! present, which is not an XLSX writer), and the brief
//! forbids adding heavy new dependencies. CSV + JSON only.

use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};

use super::formula::function_names;
use super::helpers::{
    apply_cell_write, caller, caller_company, err, load_sheet, load_workbook_readable,
    load_workbook_writable, ok, parse_csv, recalculate_sheet_cells, render_csv,
    require_caller_company, require_manage, require_tenant, require_view, snapshot_version,
    validate_cell_input,
};
use super::state::SpreadsheetsState;
use super::types::{ImportCsvRequest, MAX_CSV_ROWS};

/// CSV export of one sheet: the grid's bounding box, with
/// computed values (so a formula exports its result, not its
/// text). Empty trailing rows / columns are trimmed.
pub(crate) async fn export_sheet_csv(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<(String, String)> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let (workbook_id, sheet_id) = path.into_inner();
        let workbook = load_workbook_readable(&state, &tenant_id, &workbook_id, &caller.actor)?;
        let sheet = load_sheet(&state, &tenant_id, &workbook, &sheet_id)?;
        let cells = state.cells_of(sheet.id);
        let filename = format!("{}-{}.csv", workbook.title, sheet.title);
        Ok((render_csv(&grid_rows(&cells)), filename))
    })();
    match result {
        Ok((csv, filename)) => HttpResponse::Ok()
            .content_type("text/csv; charset=utf-8")
            .insert_header((
                "Content-Disposition",
                format!("attachment; filename=\"{filename}\""),
            ))
            .body(csv),
        Err(error) => err(error, debug),
    }
}

/// CSV export of a whole workbook: one CSV per sheet, joined
/// with a `# Sheet: <title>` marker line per sheet, so a
/// single download still round-trips through the import
/// endpoint sheet by sheet.
pub(crate) async fn export_workbook_csv(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<(String, String)> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let workbook =
            load_workbook_readable(&state, &tenant_id, &path.into_inner(), &caller.actor)?;
        let mut out = String::new();
        for sheet in state.sheets_of(workbook.id) {
            let cells = state.cells_of(sheet.id);
            out.push_str(&format!("# Sheet: {}\r\n", sheet.title));
            out.push_str(&render_csv(&grid_rows(&cells)));
        }
        let filename = format!("{}.csv", workbook.title);
        Ok((out, filename))
    })();
    match result {
        Ok((csv, filename)) => HttpResponse::Ok()
            .content_type("text/csv; charset=utf-8")
            .insert_header((
                "Content-Disposition",
                format!("attachment; filename=\"{filename}\""),
            ))
            .body(csv),
        Err(error) => err(error, debug),
    }
}

/// JSON export of a workbook: the workbook, every sheet with
/// its cells, and the registered formula function names, so
/// an importer knows which functions the formulas may use.
pub(crate) async fn export_workbook_json(
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
        let sheets: Vec<Value> = state
            .sheets_of(workbook.id)
            .iter()
            .map(|sheet| super::helpers::sheet_view(&state, sheet))
            .collect();
        Ok(json!({
            "workbook": super::helpers::workbook_view(&state, &workbook),
            "sheets": sheets,
            "formula_functions": function_names(),
            "export_format": "spreadsheets-json-v1",
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// CSV import into a sheet: rows map to grid rows starting at
/// `start_address` (default A1); a header row is skipped when
/// `has_header` is set. Every imported value is written as a
/// typed cell (a value starting with `=` becomes a formula);
/// the sheet is recalculated once and one version is created
/// for the whole import.
pub(crate) async fn import_sheet_csv(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
    body: web::Json<ImportCsvRequest>,
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
        let input = body.into_inner();
        let rows = parse_csv(&input.csv);
        let rows = if input.has_header {
            rows.into_iter().skip(1).collect::<Vec<_>>()
        } else {
            rows
        };
        if rows.is_empty() {
            return Err(UnifiedError::bad_request("CSV contains no data rows"));
        }
        if rows.len() > MAX_CSV_ROWS {
            return Err(UnifiedError::bad_request(format!(
                "At most {MAX_CSV_ROWS} rows per import"
            )));
        }
        let (start_row, start_col) = match &input.start_address {
            Some(address) => super::types::parse_address(address).ok_or_else(|| {
                UnifiedError::bad_request(format!("Invalid start_address: {address}"))
            })?,
            None => (0, 0),
        };
        let settings = state.settings_for(&tenant_id);
        let mut written = 0usize;
        let mut changes: Vec<String> = Vec::new();
        for (row_offset, row) in rows.iter().enumerate() {
            let row_index = start_row + row_offset as i64;
            if row_index >= super::types::MAX_ROWS {
                break;
            }
            for (col_offset, value) in row.iter().enumerate() {
                let col_index = start_col + col_offset as i64;
                if col_index >= super::types::MAX_COLUMNS {
                    break;
                }
                let address = super::types::format_address(row_index, col_index);
                validate_cell_input(&address, value, &None, &settings)?;
                let _ = apply_cell_write(
                    &state, &tenant_id, &workbook, &sheet, row_index, col_index, value, None,
                );
                written += 1;
                changes.push(format!("{address}: -> {}", value.trim()));
            }
        }
        recalculate_sheet_cells(&state, &sheet);
        let mut workbook = workbook;
        workbook.updated_at = chrono::Utc::now();
        state.workbooks.insert(workbook.id, workbook.clone());
        snapshot_version(
            &state,
            &tenant_id,
            &workbook,
            &sheet,
            &caller.actor,
            &changes,
        );
        Ok(json!({
            "imported_cells": written,
            "imported_rows": rows.len(),
            "sheet_id": sheet.id.to_string(),
            "start_address": super::types::format_address(start_row, start_col),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Bounding-box grid rows for a cell list: rows up to the
/// highest non-empty row, columns up to the highest non-empty
/// column, missing cells as empty strings, values as computed
/// values.
fn grid_rows(cells: &[super::types::SheetCell]) -> Vec<Vec<String>> {
    if cells.is_empty() {
        return Vec::new();
    }
    let max_row = cells.iter().map(|cell| cell.row_index).max().unwrap_or(0);
    let max_col = cells
        .iter()
        .map(|cell| cell.column_index)
        .max()
        .unwrap_or(0);
    let mut grid: Vec<Vec<String>> =
        vec![vec![String::new(); (max_col + 1) as usize]; (max_row + 1) as usize];
    for cell in cells {
        if cell.row_index <= max_row && cell.column_index <= max_col {
            grid[cell.row_index as usize][cell.column_index as usize] = cell.computed_value.clone();
        }
    }
    // Trim trailing empty rows.
    while let Some(last) = grid.last() {
        if last.iter().all(|field| field.is_empty()) {
            grid.pop();
        } else {
            break;
        }
    }
    grid
}
