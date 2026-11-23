//! Module registration: routes, hooks and metadata.

use super::ai_capabilities::AI_CAPABILITIES;
use super::handlers_ai::ai_capability;
use super::handlers_cells::{batch_set_cells, evaluate, get_cell, list_cells, set_cell};
use super::handlers_export::{
    export_sheet_csv, export_workbook_csv, export_workbook_json, import_sheet_csv,
};
use super::handlers_settings::{get_settings, update_settings};
use super::handlers_shares::{
    create_share, delete_share, get_public_workbook, get_share, list_shares, update_share,
};
use super::handlers_versions::{get_version, list_versions, restore_version};
use super::handlers_workbooks::{
    create_sheet, create_workbook, delete_sheet, delete_workbook, get_sheet, get_workbook,
    list_sheets, list_workbooks, update_sheet, update_workbook,
};
use super::hooks::{
    ai_readiness_event, ai_summarize_event, on_ai_readiness_requested, on_ai_summarize_requested,
    on_user_deleted,
};
use super::state::SpreadsheetsState;
use super::types::EVENT_USER_DELETED;
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct SpreadsheetsModule {
    pub(crate) state: SpreadsheetsState,
}

impl SpreadsheetsModule {
    pub fn new() -> Self {
        Self {
            state: SpreadsheetsState::default(),
        }
    }
}

impl Default for SpreadsheetsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for SpreadsheetsModule {
    fn id(&self) -> &'static str {
        "spreadsheets"
    }

    fn name(&self) -> &'static str {
        "Spreadsheets"
    }

    fn description(&self) -> &'static str {
        "Spreadsheet workbooks with multiple sheets, each a grid of cells (A1 notation; text, number, boolean, date, empty, formula, and error values), a small server-side formula evaluator (arithmetic with precedence, cell references and ranges, comparisons, and a data-driven function registry — SUM, AVERAGE, COUNT, COUNTA, MIN, MAX, PRODUCT, IF, ROUND, ABS, CONCAT, TODAY, LEN, UPPER, LOWER, TRIM, NOT, AND, OR) with circular-reference detection and topological recalculation on every write, per-workbook sharing with data-driven viewer / editor roles, token-gated public-link sharing that is off by default, version history as full cell snapshots with a change log and restore, and CSV import / export plus JSON export (no XLSX: no XLSX crate is in the workspace)."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("spreadsheets", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EVENT_USER_DELETED,
            "spreadsheets",
            Arc::new(move |ctx: &mut HookContext| on_user_deleted(state.clone(), ctx)),
        );
        // The AI module's runner emits these grounding events for
        // the summary and readiness capabilities; this module
        // answers with the workbook's stored facts.
        let state = self.state.clone();
        bus.register(
            ai_summarize_event(),
            "spreadsheets",
            Arc::new(move |ctx: &mut HookContext| on_ai_summarize_requested(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            ai_readiness_event(),
            "spreadsheets",
            Arc::new(move |ctx: &mut HookContext| on_ai_readiness_requested(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/spreadsheets")
                .app_data(state)
                .route("/public/{token}", web::get().to(get_public_workbook))
                .route("/workbooks", web::get().to(list_workbooks))
                .route("/workbooks", web::post().to(create_workbook))
                .route("/workbooks/{workbook_id}", web::get().to(get_workbook))
                .route("/workbooks/{workbook_id}", web::patch().to(update_workbook))
                .route(
                    "/workbooks/{workbook_id}",
                    web::delete().to(delete_workbook),
                )
                .route(
                    "/workbooks/{workbook_id}/sheets",
                    web::get().to(list_sheets),
                )
                .route(
                    "/workbooks/{workbook_id}/sheets",
                    web::post().to(create_sheet),
                )
                .route(
                    "/workbooks/{workbook_id}/sheets/{sheet_id}",
                    web::get().to(get_sheet),
                )
                .route(
                    "/workbooks/{workbook_id}/sheets/{sheet_id}",
                    web::patch().to(update_sheet),
                )
                .route(
                    "/workbooks/{workbook_id}/sheets/{sheet_id}",
                    web::delete().to(delete_sheet),
                )
                .route(
                    "/workbooks/{workbook_id}/sheets/{sheet_id}/cells",
                    web::get().to(list_cells),
                )
                .route(
                    "/workbooks/{workbook_id}/sheets/{sheet_id}/cells/batch",
                    web::post().to(batch_set_cells),
                )
                .route(
                    "/workbooks/{workbook_id}/sheets/{sheet_id}/cells/{address}",
                    web::get().to(get_cell),
                )
                .route(
                    "/workbooks/{workbook_id}/sheets/{sheet_id}/cells/{address}",
                    web::patch().to(set_cell),
                )
                .route(
                    "/workbooks/{workbook_id}/sheets/{sheet_id}/evaluate",
                    web::post().to(evaluate),
                )
                .route(
                    "/workbooks/{workbook_id}/sheets/{sheet_id}/versions",
                    web::get().to(list_versions),
                )
                .route(
                    "/workbooks/{workbook_id}/sheets/{sheet_id}/versions/{version_id}",
                    web::get().to(get_version),
                )
                .route(
                    "/workbooks/{workbook_id}/sheets/{sheet_id}/versions/{version_id}/restore",
                    web::post().to(restore_version),
                )
                .route(
                    "/workbooks/{workbook_id}/shares",
                    web::get().to(list_shares),
                )
                .route(
                    "/workbooks/{workbook_id}/shares",
                    web::post().to(create_share),
                )
                .route(
                    "/workbooks/{workbook_id}/shares/{share_id}",
                    web::get().to(get_share),
                )
                .route(
                    "/workbooks/{workbook_id}/shares/{share_id}",
                    web::patch().to(update_share),
                )
                .route(
                    "/workbooks/{workbook_id}/shares/{share_id}",
                    web::delete().to(delete_share),
                )
                .route(
                    "/workbooks/{workbook_id}/sheets/{sheet_id}/export.csv",
                    web::get().to(export_sheet_csv),
                )
                .route(
                    "/workbooks/{workbook_id}/export.csv",
                    web::get().to(export_workbook_csv),
                )
                .route(
                    "/workbooks/{workbook_id}/export.json",
                    web::get().to(export_workbook_json),
                )
                .route(
                    "/workbooks/{workbook_id}/sheets/{sheet_id}/import.csv",
                    web::post().to(import_sheet_csv),
                )
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings))
                .route("/ai/{capability}", web::post().to(ai_capability)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        let auth = unified_core::auth::RouteAuth::Authenticated;
        vec![
            RouteInfo { method: "GET", path: "/api/v1/spreadsheets/public/{token}", owner: "spreadsheets", description: "PUBLIC (token-gated): read one workbook through its public link — sheets with cells, read-only; unknown, disabled, and revoked tokens all answer 404.", auth: unified_core::auth::RouteAuth::Public },
            RouteInfo { method: "GET", path: "/api/v1/spreadsheets/workbooks", owner: "spreadsheets", description: "List workbooks the caller owns or is shared on (paginated, filter by owner_id, updated range; search by title/description).", auth },
            RouteInfo { method: "POST", path: "/api/v1/spreadsheets/workbooks", owner: "spreadsheets", description: "Create a workbook with its initial sheets (manager).", auth },
            RouteInfo { method: "GET", path: "/api/v1/spreadsheets/workbooks/{workbook_id}", owner: "spreadsheets", description: "Get one workbook with its sheets and share count.", auth },
            RouteInfo { method: "PATCH", path: "/api/v1/spreadsheets/workbooks/{workbook_id}", owner: "spreadsheets", description: "Update a workbook (owner or editor): title, description, or the public-link toggle — enabling mints a fresh token returned once.", auth },
            RouteInfo { method: "DELETE", path: "/api/v1/spreadsheets/workbooks/{workbook_id}", owner: "spreadsheets", description: "Delete a workbook with its sheets, cells, shares, and versions (owner only).", auth },
            RouteInfo { method: "GET", path: "/api/v1/spreadsheets/workbooks/{workbook_id}/sheets", owner: "spreadsheets", description: "List the sheets of a workbook (paginated, search by title).", auth },
            RouteInfo { method: "POST", path: "/api/v1/spreadsheets/workbooks/{workbook_id}/sheets", owner: "spreadsheets", description: "Add a sheet to a workbook (manager).", auth },
            RouteInfo { method: "GET", path: "/api/v1/spreadsheets/workbooks/{workbook_id}/sheets/{sheet_id}", owner: "spreadsheets", description: "Get one sheet with its non-empty cells.", auth },
            RouteInfo { method: "PATCH", path: "/api/v1/spreadsheets/workbooks/{workbook_id}/sheets/{sheet_id}", owner: "spreadsheets", description: "Rename or reorder a sheet (manager).", auth },
            RouteInfo { method: "DELETE", path: "/api/v1/spreadsheets/workbooks/{workbook_id}/sheets/{sheet_id}", owner: "spreadsheets", description: "Delete a sheet with its cells and versions; the last sheet cannot be deleted (manager).", auth },
            RouteInfo { method: "GET", path: "/api/v1/spreadsheets/workbooks/{workbook_id}/sheets/{sheet_id}/cells", owner: "spreadsheets", description: "List the cells of a sheet (paginated, filter by kind, address, or row; search raw/computed values).", auth },
            RouteInfo { method: "POST", path: "/api/v1/spreadsheets/workbooks/{workbook_id}/sheets/{sheet_id}/cells/batch", owner: "spreadsheets", description: "Set up to 1000 cells in one batch: the sheet is recalculated once and one version is created for the batch (manager).", auth },
            RouteInfo { method: "GET", path: "/api/v1/spreadsheets/workbooks/{workbook_id}/sheets/{sheet_id}/cells/{address}", owner: "spreadsheets", description: "Get one cell by A1 address.", auth },
            RouteInfo { method: "PATCH", path: "/api/v1/spreadsheets/workbooks/{workbook_id}/sheets/{sheet_id}/cells/{address}", owner: "spreadsheets", description: "Set one cell (a value starting with = is a formula; empty clears it): recalculates the sheet and creates a version (manager).", auth },
            RouteInfo { method: "POST", path: "/api/v1/spreadsheets/workbooks/{workbook_id}/sheets/{sheet_id}/evaluate", owner: "spreadsheets", description: "Evaluate a formula string in a sheet context without saving anything; returns the value, its kind, and whether it is an error.", auth },
            RouteInfo { method: "GET", path: "/api/v1/spreadsheets/workbooks/{workbook_id}/sheets/{sheet_id}/versions", owner: "spreadsheets", description: "List the version history of a sheet, newest first (paginated).", auth },
            RouteInfo { method: "GET", path: "/api/v1/spreadsheets/workbooks/{workbook_id}/sheets/{sheet_id}/versions/{version_id}", owner: "spreadsheets", description: "Get one version with its full cell snapshot and change log.", auth },
            RouteInfo { method: "POST", path: "/api/v1/spreadsheets/workbooks/{workbook_id}/sheets/{sheet_id}/versions/{version_id}/restore", owner: "spreadsheets", description: "Restore a version: the sheet's cells are replaced with the snapshot and recalculated; the restore itself becomes a new version (manager).", auth },
            RouteInfo { method: "GET", path: "/api/v1/spreadsheets/workbooks/{workbook_id}/shares", owner: "spreadsheets", description: "List the shares of a workbook (paginated, filter by role or user).", auth },
            RouteInfo { method: "POST", path: "/api/v1/spreadsheets/workbooks/{workbook_id}/shares", owner: "spreadsheets", description: "Share a workbook with a user as viewer or editor (owner only).", auth },
            RouteInfo { method: "GET", path: "/api/v1/spreadsheets/workbooks/{workbook_id}/shares/{share_id}", owner: "spreadsheets", description: "Get one share row.", auth },
            RouteInfo { method: "PATCH", path: "/api/v1/spreadsheets/workbooks/{workbook_id}/shares/{share_id}", owner: "spreadsheets", description: "Change a share's role (owner only).", auth },
            RouteInfo { method: "DELETE", path: "/api/v1/spreadsheets/workbooks/{workbook_id}/shares/{share_id}", owner: "spreadsheets", description: "Remove a share (owner only).", auth },
            RouteInfo { method: "GET", path: "/api/v1/spreadsheets/workbooks/{workbook_id}/sheets/{sheet_id}/export.csv", owner: "spreadsheets", description: "Export one sheet as CSV (computed values, bounding box).", auth },
            RouteInfo { method: "GET", path: "/api/v1/spreadsheets/workbooks/{workbook_id}/export.csv", owner: "spreadsheets", description: "Export a whole workbook as CSV (one sheet per # Sheet: section).", auth },
            RouteInfo { method: "GET", path: "/api/v1/spreadsheets/workbooks/{workbook_id}/export.json", owner: "spreadsheets", description: "Export a workbook as JSON (workbook, sheets with cells, registered formula functions).", auth },
            RouteInfo { method: "POST", path: "/api/v1/spreadsheets/workbooks/{workbook_id}/sheets/{sheet_id}/import.csv", owner: "spreadsheets", description: "Import CSV text into a sheet from a start address (manager); recalculates once and creates one version.", auth },
            RouteInfo { method: "GET", path: "/api/v1/spreadsheets/settings", owner: "spreadsheets", description: "Get per-tenant spreadsheets settings (number formats, share roles, default currency, public-link switch, sheet bound).", auth },
            RouteInfo { method: "PATCH", path: "/api/v1/spreadsheets/settings", owner: "spreadsheets", description: "Update per-tenant spreadsheets settings (manager).", auth },
            RouteInfo { method: "POST", path: "/api/v1/spreadsheets/ai/{capability}", owner: "spreadsheets", description: "Run one registered spreadsheets AI capability through the AI module's gated runner.", auth },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SpreadsheetsModule::new()));
}
