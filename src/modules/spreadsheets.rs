// Spreadsheets module - spreadsheet workbooks with multiple
// sheets, each a grid of cells (A1 notation; text, number,
// boolean, date, empty, formula, and error values), a small
// server-side formula evaluator, per-workbook sharing,
// version history, and CSV / JSON import and export.
//
// Routes live under `/api/v1/spreadsheets`:
//
//   Public (token-gated - the token is the credential; see the
//   public-access note below):
//     GET  /public/{token} - read a workbook through its
//       public link (read-only).
//   Workbooks:
//     GET/POST /workbooks - list workbooks / create one.
//     GET/PATCH/DELETE /workbooks/{workbook_id}.
//   Sheets:
//     GET/POST /workbooks/{workbook_id}/sheets - list / add.
//     GET/PATCH/DELETE
//       /workbooks/{workbook_id}/sheets/{sheet_id}.
//   Cells:
//     GET  /workbooks/{workbook_id}/sheets/{sheet_id}/cells
//       - list cells (paginated, filter by kind, address,
//       row; search raw/computed values).
//     GET/PATCH
//       /workbooks/{workbook_id}/sheets/{sheet_id}/cells/{address}
//       - get / set one cell (a value starting with `=` is a
//       formula; empty clears the cell).
//     POST /workbooks/{workbook_id}/sheets/{sheet_id}/cells/batch
//       - set up to 1000 cells at once.
//     POST /workbooks/{workbook_id}/sheets/{sheet_id}/evaluate
//       - evaluate a formula string without saving it.
//   Versions:
//     GET  /workbooks/{workbook_id}/sheets/{sheet_id}/versions
//       - version history, newest first.
//     GET  /workbooks/{workbook_id}/sheets/{sheet_id}/versions/{version_id}.
//     POST /workbooks/{workbook_id}/sheets/{sheet_id}/versions/{version_id}/restore.
//   Shares:
//     GET/POST /workbooks/{workbook_id}/shares - list / share.
//     GET/PATCH/DELETE
//       /workbooks/{workbook_id}/shares/{share_id}.
//   Export / import:
//     GET  /workbooks/{workbook_id}/sheets/{sheet_id}/export.csv
//       - one sheet as CSV.
//     GET  /workbooks/{workbook_id}/export.csv - whole
//       workbook as CSV.
//     GET  /workbooks/{workbook_id}/export.json - workbook as
//       JSON.
//     POST /workbooks/{workbook_id}/sheets/{sheet_id}/import.csv
//       - import CSV text into a sheet.
//   Settings:
//     GET  /settings - per-tenant settings.
//     PATCH /settings (manager).
//   AI:
//     POST /ai/{capability} - one registered capability
//       through the AI module's gated runner.
//
// HONESTY - READ THIS FIRST. What is real in this module:
// in-memory workbook, sheet, cell, share, version, and
// settings management; the formula evaluator (arithmetic with
// precedence, cell references and ranges, comparisons, and a
// data-driven function registry - SUM, AVERAGE, COUNT, COUNTA,
// MIN, MAX, PRODUCT, IF, ROUND, ABS, CONCAT, TODAY, LEN,
// UPPER, LOWER, TRIM, NOT, AND, OR - with circular-reference
// detection and topological recalculation on every cell-write
// batch); numbers as decimal strings computed on an
// integer-mantissa decimal type, so `f64` is never used for a
// stored or returned number; per-workbook sharing with
// data-driven viewer / editor roles (owner deletes and
// shares, an editor sharee edits cells only); token-gated
// public-link sharing that is off by default (raw token
// returned exactly once, only its SHA-256 hash stored);
// version history as full cell snapshots with a change log,
// one version per write batch, restore reproducing the sheet
// exactly; CSV import / export plus JSON export; tenant
// scoping on every row with cross-tenant access reading as
// 404, never 403. What is NOT built here: durable storage
// until the migration runs (the DashMap stores are
// authoritative; migration `2026-11-22-000118_spreadsheets`
// is the persistence target); XLSX export - no XLSX crate is
// in the workspace `Cargo.toml` / `Cargo.lock` (checked
// before writing this module; only the transitive `zip` crate
// is present) and no heavy new dependency was added, so CSV +
// JSON only; email / push delivery (the notifications module
// owns delivery - this module only emits
// `notification.requested` when a workbook is shared).
//
// Public-access gate: the API Manager module enforces public
// access through its admin-managed `ApiAccessRule` records
// (default deny: an endpoint is public only when a rule exists
// with `public_access: true`, and denied outright when a rule
// exists with `allowed: false`). There is no reusable
// per-request "is this endpoint public?" helper to call from a
// handler - the check lives in the API Manager middleware /
// `resolve_public` path - so the public route below relies on
// that gate: an administrator must create the matching
// `ApiAccessRule` row for `GET /api/v1/spreadsheets/public/*`.
// The route is registered in `route_info()` with
// `route_version_support()` defaulting to `All`, so the API
// Manager's per-version registry picks it up. Do NOT treat the
// absence of a capability check on a public route as an open
// door: without the rule the API Manager denies the call. The
// token itself is a second gate: unknown, disabled, and
// revoked tokens all answer 404 with the same message, so
// tokens cannot be probed apart.
//
// Actor identity: the caller is the server-verified caller from
// `unified_core::caller::require_caller` (user id plus
// server-resolved role, both required for capability checks),
// exactly like the sign module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
// Handler signatures take `web::Data<Arc<AppContext>>` because
// the server registers `Arc<AppContext>` (see
// `src/http/server.rs`: `run_server(ctx: Arc<AppContext>)`);
// `web::Data<AppContext>` would fail to extract.
//
// Events emitted on the hook bus:
//   - `spreadsheets.workbook.created`, `spreadsheets.workbook.updated`,
//     `spreadsheets.workbook.deleted`
//   - `spreadsheets.cell.updated` (one event per write batch,
//     with the change log)
//   - `spreadsheets.shared`
//   - `spreadsheets.version.restored`
//   - `notification.requested` (workbook shared, in the
//     notifications module's payload shape).
// Events this module listens to:
//   - `user.deleted` - anonymizes the user's workbooks,
//     shares, and versions to `deleted-user`; workbooks are
//     never deleted.

pub mod ai_capabilities;
pub(crate) mod formula;
mod handlers_ai;
mod handlers_cells;
mod handlers_export;
mod handlers_settings;
mod handlers_shares;
mod handlers_versions;
mod handlers_workbooks;
mod helpers;
mod hooks;
mod module;
pub mod state;
pub mod types;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::formula::{function_names, FUNCTION_REGISTRY};
pub use self::module::register;
pub use self::module::SpreadsheetsModule;
pub use self::state::SpreadsheetsState;
pub use self::types::{
    BatchSetCellsRequest, CellInput, CellKind, CellSnapshot, CreateShareRequest,
    CreateSheetRequest, CreateWorkbookRequest, Decimal, EvaluateFormulaRequest, ImportCsvRequest,
    NumberFormat, SetCellRequest, Share, Sheet, SheetCell, SheetVersion, SpreadsheetSettings,
    UpdateSettingsRequest, UpdateShareRequest, UpdateSheetRequest, UpdateWorkbookRequest, Workbook,
};
