// Widgets - shared types: widget definitions, layouts, contexts,
// and the hook + capability names other modules use to talk to this
// module.
//
// A widget definition is DATA, never a hard-coded render branch:
// modules register new widget types by answering the
// `widgets.widget_types` hook with definition objects in this shape,
// and the catalogue is the built-in seed below plus those answers.
//
// A layout is keyed by (zone, scope, owner). A zone is the
// kebab-case id of a widgetized page ("context" in the API and the
// table, for the same reason a zone needs no registration: any page
// can declare one). The owner is a user id; the empty owner id is
// the SHARED (team) default for the zone. Load order: the user's
// own row, then the shared row, then the built-in default.

use serde::{Deserialize, Serialize};

/// Hook other modules answer to register widget types. Each result
/// is one definition object, or an array of them (see
/// `registry::collect_widget_types`).
pub const EVENT_WIDGET_TYPES: &str = "widgets.widget_types";

/// Event emitted (fire-and-forget) after a layout is saved or reset.
pub const EVENT_LAYOUT_CHANGED: &str = "widgets.layout_changed";

/// Capability to read widget types and layouts.
pub const CAP_WIDGETS_VIEW: &str = "widgets.view";

/// Capability to save or reset a layout.
pub const CAP_WIDGETS_MANAGE: &str = "widgets.manage";

/// The control-plane admin dashboard.
pub const CONTEXT_ADMIN_DASHBOARD: &str = "admin-dashboard";

/// A tenant's own dashboard (tenant-scoped).
pub const CONTEXT_TENANT_DASHBOARD: &str = "tenant-dashboard";

/// The signed-in user's (client) dashboard.
pub const CONTEXT_USER_DASHBOARD: &str = "user-dashboard";

/// A reseller's dashboard (their own clients, payouts, and orders).
pub const CONTEXT_RESELLER_DASHBOARD: &str = "reseller-dashboard";

/// The seeded zones: the first widgetized dashboard pages. A zone is
/// just a kebab-case page id - see `validation::is_valid_zone` - so
/// any other dashboard/admin page can declare its own zone without
/// touching this module (the front page is NOT a zone: it is
/// site-builder output).

/// Control-plane scope: the layout belongs to the platform itself.
pub const SCOPE_PLATFORM: &str = "platform";

/// Tenant scope: the layout belongs to one tenant (company).
pub const SCOPE_TENANT: &str = "tenant";

/// The scope id for platform scope.
pub const SCOPE_ID_PLATFORM: &str = "platform";

/// Owner id of a context DEFAULT layout (as opposed to a per-user
/// override, whose owner id is the user's id).
pub const OWNER_DEFAULT: &str = "";

/// Hard cap on widgets in one layout: a layout is a dashboard, not
/// an unbounded list.
pub const MAX_LAYOUT_ITEMS: usize = 32;

/// One widget type. `contexts` lists the zones the widget may
/// appear in; an EMPTY list means every zone. `source_module` names
/// the module that owns the live data, and `data_source` names the
/// hook (or endpoint) that serves it, so a reader can trace every
/// number back to its source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetDefinition {
    pub id: String,
    pub title: String,
    pub description: String,
    pub contexts: Vec<String>,
    pub source_module: String,
    pub data_source: String,
}

impl WidgetDefinition {
    /// True when this widget may appear in the given zone: always
    /// when `contexts` is empty, otherwise only when the zone is
    /// listed.
    pub fn for_context(&self, context: &str) -> bool {
        self.contexts.is_empty() || self.contexts.iter().any(|item| item == context)
    }
}

/// One entry in a layout: the widget, in display order (the array
/// order IS the layout order), plus its pinned flag. Pinned widgets
/// sort before unpinned ones when a layout is rendered.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WidgetItem {
    pub widget_id: String,
    #[serde(default)]
    pub pinned: bool,
}

/// A resolved layout: the context, its scope, its owner, and the
/// ordered items. `is_default` is true when no row is stored and the
/// built-in default for the context is returned instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetLayout {
    pub context: String,
    pub scope_kind: String,
    pub scope_id: String,
    pub owner_id: String,
    pub items: Vec<WidgetItem>,
    pub is_default: bool,
}

/// Request body for `PUT /api/v1/widgets/layouts/{context}`.
/// `shared` writes the SHARED (team) default for the zone instead
/// of the caller's own layout; the shared row is the fallback a
/// user starts from, never the live layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveLayoutInput {
    #[serde(default)]
    pub items: Vec<WidgetItem>,
    #[serde(default)]
    pub shared: bool,
}
