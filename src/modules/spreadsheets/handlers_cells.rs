//! HTTP handlers for cells: read a sheet's grid, read / set
//! one cell, batch-set cells, and evaluate a formula in a
//! sheet context without saving it. Every write recalculates
//! the sheet in topological order and creates one version
//! snapshot for the batch.

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;

use super::formula::{evaluate_formula, EvalValue};
use super::helpers::{
    apply_cell_write, caller, caller_company, err, load_sheet, load_workbook_readable,
    load_workbook_writable, ok, paginate, recalculate_sheet_cells, require_caller_company,
    require_manage, require_tenant, require_view, snapshot_version, today_string,
    validate_cell_batch, validate_cell_input,
};
use super::state::SpreadsheetsState;
use super::types::{
    parse_address, BatchSetCellsRequest, CellKind, EvaluateFormulaRequest, SetCellRequest,
    EVENT_CELL_UPDATED,
};
use super::types::{Decimal, MAX_CELLS_PER_BATCH};

/// One cell as a grid row, with its computed value inline.
fn cell_json(cell: &super::types::SheetCell) -> Value {
    serde_json::to_value(cell).unwrap_or(Value::Null)
}

pub(crate) async fn list_cells(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
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
        let params = query.into_inner();
        let kind_filter = params
            .filters
            .get("kind")
            .and_then(|raw| CellKind::from_str(raw));
        let address_filter = params.filters.get("address").cloned();
        let row_filter = params
            .filters
            .get("row")
            .and_then(|raw| raw.parse::<i64>().ok());
        let search = params.search.clone();
        let mut items = state.cells_of(sheet.id);
        items.retain(|cell| {
            if let Some(kind) = kind_filter {
                if cell.kind != kind {
                    return false;
                }
            }
            if let Some(address) = &address_filter {
                if !cell.address.eq_ignore_ascii_case(address) {
                    return false;
                }
            }
            if let Some(row) = row_filter {
                if cell.row_index != row {
                    return false;
                }
            }
            if let Some(needle) = &search {
                let needle = needle.to_lowercase();
                if !cell.raw_value.to_lowercase().contains(&needle)
                    && !cell.computed_value.to_lowercase().contains(&needle)
                {
                    return false;
                }
            }
            true
        });
        let views: Vec<Value> = items.iter().map(cell_json).collect();
        Ok(serde_json::to_value(paginate(views, &params)).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_cell(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let (workbook_id, sheet_id, address) = path.into_inner();
        let workbook = load_workbook_readable(&state, &tenant_id, &workbook_id, &caller.actor)?;
        let sheet = load_sheet(&state, &tenant_id, &workbook, &sheet_id)?;
        let (row, col) = parse_address(&address)
            .ok_or_else(|| UnifiedError::bad_request(format!("Invalid cell address: {address}")))?;
        let cell = state
            .cells
            .iter()
            .map(|entry| entry.value().clone())
            .find(|cell| {
                cell.sheet_id == sheet.id && cell.row_index == row && cell.column_index == col
            })
            .ok_or_else(|| UnifiedError::not_found(format!("Cell not found: {address}")))?;
        Ok(cell_json(&cell))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Shared write path for one cell and for batches: apply the
/// writes, recalculate the sheet, snapshot one version for
/// the batch, and emit one cell event per changed address.
fn write_cells(
    state: &SpreadsheetsState,
    tenant_id: &str,
    workbook: &super::types::Workbook,
    sheet: &super::types::Sheet,
    actor: &str,
    writes: &[(i64, i64, String, Option<String>)],
) -> Vec<super::types::SheetCell> {
    let mut changes: Vec<String> = Vec::new();
    for (row, col, value, format) in writes {
        let previous = apply_cell_write(
            state,
            tenant_id,
            workbook,
            sheet,
            *row,
            *col,
            value,
            format.clone(),
        );
        let address = super::types::format_address(*row, *col);
        let before = previous.unwrap_or_default();
        changes.push(format!("{address}: {before} -> {}", value.trim()));
    }
    let cells = recalculate_sheet_cells(state, sheet);
    let mut workbook = workbook.clone();
    workbook.updated_at = Utc::now();
    state.workbooks.insert(workbook.id, workbook.clone());
    snapshot_version(state, tenant_id, &workbook, sheet, actor, &changes);
    state.emit(
        EVENT_CELL_UPDATED,
        json!({
            "tenant_id": tenant_id,
            "workbook_id": workbook.id.to_string(),
            "sheet_id": sheet.id.to_string(),
            "changes": changes,
            "actor": actor,
        }),
    );
    cells
}

pub(crate) async fn set_cell(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String, String)>,
    body: web::Json<SetCellRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let (workbook_id, sheet_id, address) = path.into_inner();
        let workbook = load_workbook_writable(&state, &tenant_id, &workbook_id, &caller.actor)?;
        let sheet = load_sheet(&state, &tenant_id, &workbook, &sheet_id)?;
        let input = body.into_inner();
        let settings = state.settings_for(&tenant_id);
        let (row, col, _) =
            validate_cell_input(&address, &input.value, &input.number_format, &settings)?;
        write_cells(
            &state,
            &tenant_id,
            &workbook,
            &sheet,
            &caller.actor,
            &[(row, col, input.value.clone(), input.number_format.clone())],
        );
        let cell = state
            .cells
            .iter()
            .map(|entry| entry.value().clone())
            .find(|cell| {
                cell.sheet_id == sheet.id && cell.row_index == row && cell.column_index == col
            });
        match cell {
            Some(cell) => Ok(cell_json(&cell)),
            None => Ok(json!({
                "address": super::types::format_address(row, col),
                "kind": "empty",
                "raw_value": "",
                "computed_value": "",
            })),
        }
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn batch_set_cells(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
    body: web::Json<BatchSetCellsRequest>,
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
        validate_cell_batch(&input.cells)?;
        let settings = state.settings_for(&tenant_id);
        let mut writes: Vec<(i64, i64, String, Option<String>)> = Vec::new();
        for cell in &input.cells {
            let (row, col, _) =
                validate_cell_input(&cell.address, &cell.value, &cell.number_format, &settings)?;
            writes.push((row, col, cell.value.clone(), cell.number_format.clone()));
        }
        let cells = write_cells(
            &state,
            &tenant_id,
            &workbook,
            &sheet,
            &caller.actor,
            &writes,
        );
        let views: Vec<Value> = cells.iter().map(cell_json).collect();
        Ok(json!({
            "updated_cells": views.len(),
            "max_batch": MAX_CELLS_PER_BATCH,
            "cells": views,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn evaluate(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
    body: web::Json<EvaluateFormulaRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let (workbook_id, sheet_id) = path.into_inner();
        let workbook = load_workbook_readable(&state, &tenant_id, &workbook_id, &caller.actor)?;
        let sheet = load_sheet(&state, &tenant_id, &workbook, &sheet_id)?;
        let input = body.into_inner();
        let formula = input.formula.trim();
        if formula.is_empty() {
            return Err(UnifiedError::bad_request("formula must not be empty"));
        }
        if formula.len() > super::types::MAX_RAW_VALUE_LEN {
            return Err(UnifiedError::bad_request("formula is too long"));
        }
        // Current computed values, so the evaluation reads the
        // sheet as it stands without re-entering any formula.
        let mut current: HashMap<(i64, i64), EvalValue> = HashMap::new();
        for cell in state.cells_of(sheet.id) {
            let value = match cell.computed_kind {
                CellKind::Number => Decimal::parse(&cell.computed_value)
                    .map(EvalValue::Number)
                    .unwrap_or_else(|| EvalValue::Error(super::types::ERR_VALUE.to_string())),
                CellKind::Boolean => {
                    EvalValue::Bool(cell.computed_value.eq_ignore_ascii_case("true"))
                }
                CellKind::Error => EvalValue::Error(cell.computed_value.clone()),
                CellKind::Empty => EvalValue::Empty,
                _ => EvalValue::Text(cell.computed_value.clone()),
            };
            current.insert((cell.row_index, cell.column_index), value);
        }
        let value = evaluate_formula(formula, &current, &today_string());
        Ok(json!({
            "formula": formula,
            "value": value.display(),
            "kind": value.kind().as_str(),
            "is_error": matches!(value, EvalValue::Error(_)),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
