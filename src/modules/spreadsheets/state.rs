//! In-memory spreadsheets store.

use super::types::{Share, Sheet, SheetCell, SheetVersion, SpreadsheetSettings, Workbook};
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use unified_core::hooks::HookBus;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for the spreadsheets module. The hook bus
/// is attached in `init` (the bus lives in `AppContext`);
/// handlers tolerate a missing bus by skipping event emission.
/// Durable rows for these types are defined by migration
/// `2026-11-22-000118_spreadsheets`; until that migration is
/// executed, spreadsheets data does not survive a restart.
#[derive(Clone)]
pub struct SpreadsheetsState {
    pub workbooks: Arc<DashMap<Uuid, Workbook>>,
    pub sheets: Arc<DashMap<Uuid, Sheet>>,
    pub cells: Arc<DashMap<Uuid, SheetCell>>,
    pub shares: Arc<DashMap<Uuid, Share>>,
    pub versions: Arc<DashMap<Uuid, SheetVersion>>,
    pub settings: Arc<DashMap<String, SpreadsheetSettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for SpreadsheetsState {
    fn default() -> Self {
        Self {
            workbooks: Arc::new(DashMap::new()),
            sheets: Arc::new(DashMap::new()),
            cells: Arc::new(DashMap::new()),
            shares: Arc::new(DashMap::new()),
            versions: Arc::new(DashMap::new()),
            settings: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl SpreadsheetsState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Per-tenant settings, falling back to the module default
    /// when the tenant has never saved settings.
    pub(crate) fn settings_for(&self, tenant_id: &str) -> SpreadsheetSettings {
        self.settings
            .get(tenant_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default()
    }

    /// Sheets of one workbook, in tab order.
    pub(crate) fn sheets_of(&self, workbook_id: Uuid) -> Vec<Sheet> {
        let mut sheets: Vec<Sheet> = self
            .sheets
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|sheet| sheet.workbook_id == workbook_id)
            .collect();
        sheets.sort_by_key(|sheet| (sheet.position, sheet.created_at));
        sheets
    }

    /// Cells of one sheet, ordered by row then column.
    pub(crate) fn cells_of(&self, sheet_id: Uuid) -> Vec<SheetCell> {
        let mut cells: Vec<SheetCell> = self
            .cells
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|cell| cell.sheet_id == sheet_id)
            .collect();
        cells.sort_by_key(|cell| (cell.row_index, cell.column_index));
        cells
    }

    /// Shares of one workbook, in creation order.
    pub(crate) fn shares_of(&self, workbook_id: Uuid) -> Vec<Share> {
        let mut shares: Vec<Share> = self
            .shares
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|share| share.workbook_id == workbook_id)
            .collect();
        shares.sort_by_key(|share| share.created_at);
        shares
    }

    /// Versions of one sheet, newest first.
    pub(crate) fn versions_of(&self, sheet_id: Uuid) -> Vec<SheetVersion> {
        let mut versions: Vec<SheetVersion> = self
            .versions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|version| version.sheet_id == sheet_id)
            .collect();
        versions.sort_by_key(|version| std::cmp::Reverse(version.version_number));
        versions
    }

    /// Next version number for a sheet (one past the highest
    /// stored, or 1 for the first version).
    pub(crate) fn next_version_number(&self, sheet_id: Uuid) -> i64 {
        self.versions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|version| version.sheet_id == sheet_id)
            .map(|version| version.version_number)
            .max()
            .unwrap_or(0)
            + 1
    }

    /// True when `user_id` may read the workbook: the owner,
    /// or any user with a share row for it.
    pub(crate) fn can_read(&self, workbook: &Workbook, user_id: &str) -> bool {
        workbook.owner_id == user_id
            || self.shares.iter().any(|entry| {
                entry.value().workbook_id == workbook.id && entry.value().shared_with == user_id
            })
    }

    /// True when `user_id` may write the workbook: the owner,
    /// or a user with an `editor` share row for it.
    pub(crate) fn can_write(&self, workbook: &Workbook, user_id: &str) -> bool {
        workbook.owner_id == user_id
            || self.shares.iter().any(|entry| {
                let share = entry.value();
                share.workbook_id == workbook.id
                    && share.shared_with == user_id
                    && share.role == "editor"
            })
    }

    /// Emit a `notification.requested` event for a user, in the
    /// notifications module's payload shape.
    pub(crate) fn notify(&self, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": "spreadsheets",
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "spreadsheets",
                "category": "spreadsheets",
                "priority": "normal",
                "link": link,
            }),
        );
    }
}
