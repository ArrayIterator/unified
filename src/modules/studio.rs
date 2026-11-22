// Studio module — user-side customizer (metadata-driven only).
//
// A Studio-style customizer: administrators and power users add custom
// fields, validation rules, saved list views, and form layouts to any
// record type, without touching code and without changing the database
// schema at runtime.
//
// HARD BOUNDARY — READ THIS FIRST. This module is STRICTLY
// METADATA-DRIVEN. It never generates code, never alters a database
// table at runtime, never executes user-supplied code, and never adds
// a column. Customizations are metadata records that the owning
// module reads when it renders a record; custom values live in one
// generic key-value table (`studio_field_values`: record_type +
// record_id + field_key + typed JSON value). A customization can
// change what a form shows, never what the system stores.
//
// Routes live under `/api/v1/studio`:
//
//   Field types (closed vocabulary registry, read-only):
//     GET  /field-types — the seven field types and what each supports.
//   Record types (admin-editable registry; modules register theirs
//   through the `studio.record_type.register` hook):
//     GET/POST /record-types
//     GET/DELETE /record-types/{record_type}
//     GET/POST /record-types/{record_type}/system-fields
//   Custom field definitions (versioned):
//     GET/POST /fields
//     GET  /fields/record-type/{record_type}?company_id=...
//     GET/PUT/DELETE /fields/{id}
//     POST /fields/{id}/archive
//   Custom values (generic key-value store):
//     GET/POST /values
//     GET/DELETE /values/{id}
//     POST /values/validate — validate a value without saving it.
//     GET  /records/{record_type}/{record_id}/custom-fields
//        — effective definitions merged with stored values.
//   Saved list views (per user / per company, private or shared):
//     GET/POST /views
//     GET/PUT/DELETE /views/{id}
//     POST /views/{id}/set-default
//   Form layouts (field order / visibility per record type, company):
//     GET/POST /layouts
//     GET  /layouts/record-type/{record_type}?company_id=...
//     GET/PUT/DELETE /layouts/{id}
//
// Field types (closed vocabulary, same precedent as the closed
// `ProductCategory` set in `crate::core::modules`): `text`,
// `number`, `decimal`, `date`, `boolean`, `select`, `user_reference`.
// Everything else about a field — its options, defaults, help text,
// and validation rules — is data stored on the definition row.
//
// Field definitions are VERSIONED. Creating a definition stores
// version 1. Updating a definition never mutates the old row: it
// stores a new row with the same `field_key` and version + 1 and
// marks the old row `superseded`. Stored values keep pointing at the
// definition version they were validated against, so existing values
// keep working after an edit. The effective definition for a record
// type is the highest active version; a company-specific definition
// shadows a global one with the same field key.
//
// Validation rules (all data on the definition, all enforced
// server-side when a value is saved):
//   - `required` — a value must be present and non-empty.
//   - `min` / `max` — numeric bounds for `number` and `decimal`.
//   - `min_length` / `max_length` — text length bounds.
//   - `pattern` — a regular expression a `text` value must match.
//     The pattern is compiled at definition time: an invalid regex
//     is rejected when the definition is created, never at use time.
//   - `options` — the allowed values for `select` (data, not code).
//
// Safety:
//   - A record type must be registered (see the registry below)
//     before any definition, value, view, or layout may reference
//     it. Unknown record types are rejected.
//   - Custom field keys must start with the `x_` prefix (a
//     configurable default, see `FIELD_KEY_PREFIX`) so a custom field
//     can never collide with a system field.
//   - System fields are registered per record type with a
//     `hideable` flag. A layout may never hide a system field whose
//     `hideable` is false, and a layout may never hide a system
//     field marked `required`. The owning module, not Studio, owns
//     the final render — Studio only records the layout metadata.
//
// Hook contract (the core of this module). Query hooks — owning
// modules ask, Studio answers by adding a result to the hook
// context; the requester reads `context.results`:
//   - `studio.fields.requested`
//     Request payload: `{"record_type": "crm.lead", "company_id": ""}`
//     (empty `company_id` means global definitions only).
//     Result payload: `{"record_type": "...", "company_id": "...",
//     "definitions": [<FieldDefinition>, ...]}` — the effective
//     definitions (highest active version per field key, company
//     definitions shadowing global ones).
//   - `studio.layout.requested`
//     Request payload: `{"record_type": "crm.lead", "company_id": ""}`.
//     Result payload: `{"record_type": "...", "company_id": "...",
//     "layout": <FormLayout | null>}` — the default layout for that
//     company, falling back to the global default layout, or null
//     when no layout exists.
//   - `studio.views.requested`
//     Request payload: `{"record_type": "crm.lead",
//     "company_id": "...", "user_id": "..."}`.
//     Result payload: `{"record_type": "...", "views": [<ListView>,
//     ...]}` — the views visible to that user (their own views plus
//     shared company views), defaults first.
//   - `studio.record_type.register`
//     Request payload: `{"record_type": "crm.lead",
//     "module_id": "crm", "label": "Lead", "description": "...",
//     "system_fields": [{"field_key": "name", "label": "Name",
//     "field_type": "text", "required": true, "hideable": false}]}`.
//     Result payload: `{"registered": true, "record_type": "..."}`.
//     Any module may register its record types this way; the same
//     registration is also available as `POST /record-types`.
// Events emitted on the hook bus:
//   - `studio.fields.changed` — `{"field_key", "record_type",
//     "company_id", "version", "definition_id", "action":
//     "created" | "updated" | "archived"}`. Owning modules listen to
//     this to invalidate cached definitions.
//   - `studio.values.changed` — `{"record_type", "record_id",
//     "field_key", "company_id", "action": "set" | "deleted"}`.
//   - `studio.views.changed` — `{"view_id", "record_type",
//     "company_id", "action": "created" | "updated" | "deleted" |
//     "default_changed"}`.
//   - `studio.layouts.changed` — `{"layout_id", "record_type",
//     "company_id", "action": "created" | "updated" | "deleted"}`.
//   - `studio.record_type.registered` — `{"record_type",
//     "module_id", "label"}`.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs against,
// exactly like the properties module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
// Capabilities: `studio.view` (read), `studio.customize` (fields,
// values, views, layouts), `studio.admin` (record types, system
// fields). There are no public routes: every route requires a
// caller and a capability, and the API Manager's allowed-public-
// access controls apply through the versioned public API registry
// (`route_info` + `route_version_support` default `All`).
//
// Persistence: the live data lives in in-memory stores (the same
// approach the saved-views, notifications, and audit modules use).
// The migration `2026-11-22-000085_studio` creates the durable
// tables (`studio_record_types`, `studio_system_fields`,
// `studio_field_definitions`, `studio_field_values`,
// `studio_list_views`, `studio_form_layouts`) as persistence
// targets; this file never references a table that does not exist
// yet and never runs raw SQL, so it compiles against the current
// schema. The migration has never been executed against a live
// PostgreSQL database.
//
// Honesty — what is real here: metadata CRUD with server-side
// validation, versioned definitions, the generic key-value value
// store, saved views, layouts, the record-type registry, and the
// hook contract above. What is NOT built here: the owning modules
// do not yet read Studio metadata when they render (each owning
// module must adopt the query hooks), there is no migration of
// values when a definition's type changes (the old values stay
// attached to the old version), and there is no drag-and-drop
// layout builder UI — layouts are JSON metadata over HTTP.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::tenancy::TenantContext as TenancyContext;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Constants and configurable defaults.
// ---------------------------------------------------------------------------

/// Prefix every custom field key must start with, so custom fields
/// can never collide with a system field. Configurable default:
/// an administrator may change it through the field-key validation
/// settings; the value below is only the shipped default.
pub const FIELD_KEY_PREFIX: &str = "x_";

/// Global scope marker: an empty company id means "applies to every
/// company". Stored as an empty string (never NULL) so unique
/// indexes stay simple.
pub const GLOBAL_COMPANY_ID: &str = "";

/// Maximum lengths enforced on user supplied text.
const MAX_LABEL_LEN: usize = 100;
const MAX_HELP_LEN: usize = 500;
const MAX_NAME_LEN: usize = 100;
const MAX_RECORD_TYPE_LEN: usize = 128;
const MAX_RECORD_ID_LEN: usize = 200;
const MAX_USER_ID_LEN: usize = 200;
const MAX_PATTERN_LEN: usize = 500;
const MAX_OPTIONS: usize = 100;
const MAX_OPTION_LEN: usize = 100;
const MAX_COLUMNS: usize = 50;
const MAX_LAYOUT_ENTRIES: usize = 200;
const MAX_SECTIONS: usize = 20;

/// Capability names checked through the `role.check` hook.
const PERM_STUDIO_VIEW: &str = "studio.view";
const PERM_STUDIO_CUSTOMIZE: &str = "studio.customize";
const PERM_STUDIO_ADMIN: &str = "studio.admin";

/// Hook event names.
const HOOK_FIELDS_REQUESTED: &str = "studio.fields.requested";
const HOOK_LAYOUT_REQUESTED: &str = "studio.layout.requested";
const HOOK_VIEWS_REQUESTED: &str = "studio.views.requested";
const HOOK_RECORD_TYPE_REGISTER: &str = "studio.record_type.register";
const EVENT_FIELDS_CHANGED: &str = "studio.fields.changed";
const EVENT_VALUES_CHANGED: &str = "studio.values.changed";
const EVENT_VIEWS_CHANGED: &str = "studio.views.changed";
const EVENT_LAYOUTS_CHANGED: &str = "studio.layouts.changed";
const EVENT_RECORD_TYPE_REGISTERED: &str = "studio.record_type.registered";

/// The seven field types. Closed vocabulary (same precedent as the
/// closed `ProductCategory` set): a new type needs a code change
/// because each type carries server-side validation semantics.
/// Options, defaults, and rules for a field are data, not code.
pub const FIELD_TYPE_TEXT: &str = "text";
pub const FIELD_TYPE_NUMBER: &str = "number";
pub const FIELD_TYPE_DECIMAL: &str = "decimal";
pub const FIELD_TYPE_DATE: &str = "date";
pub const FIELD_TYPE_BOOLEAN: &str = "boolean";
pub const FIELD_TYPE_SELECT: &str = "select";
pub const FIELD_TYPE_USER_REFERENCE: &str = "user_reference";

pub const FIELD_TYPES: [&str; 7] = [
    FIELD_TYPE_TEXT,
    FIELD_TYPE_NUMBER,
    FIELD_TYPE_DECIMAL,
    FIELD_TYPE_DATE,
    FIELD_TYPE_BOOLEAN,
    FIELD_TYPE_SELECT,
    FIELD_TYPE_USER_REFERENCE,
];

/// Definition statuses. `active` is the only effective version;
/// `superseded` rows are kept so old values stay valid; `archived`
/// rows are hidden from effective definitions but still readable.
pub const STATUS_ACTIVE: &str = "active";
pub const STATUS_SUPERSEDED: &str = "superseded";
pub const STATUS_ARCHIVED: &str = "archived";

pub const VISIBILITY_PRIVATE: &str = "private";
pub const VISIBILITY_SHARED: &str = "shared";

/// Default record types shipped with the module. CONFIGURABLE
/// DEFAULTS, clearly marked: they only seed the registry at first
/// start. Administrators add, edit, and remove record types through
/// `POST /record-types` and the `studio.record_type.register` hook,
/// and modules register their own types through the same hook.
/// (record_type, module_id, label)
pub const DEFAULT_RECORD_TYPES: [(&str, &str, &str); 8] = [
    ("crm.lead", "crm", "Lead"),
    ("crm.contact", "crm", "Contact"),
    ("crm.deal", "crm", "Deal"),
    ("projects.task", "projects", "Task"),
    ("projects.project", "projects", "Project"),
    ("store.product", "store", "Product"),
    ("tickets.ticket", "tickets", "Ticket"),
    ("hr.employee", "hr", "Employee"),
];

/// Default system fields shipped with the module. CONFIGURABLE
/// DEFAULTS: they seed the system-field registry at first start and
/// are fully admin-editable afterwards.
/// (record_type, field_key, label, field_type, required, hideable)
pub const DEFAULT_SYSTEM_FIELDS: [(&str, &str, &str, &str, bool, bool); 10] = [
    ("crm.lead", "name", "Name", "text", true, false),
    ("crm.lead", "email", "Email", "text", false, false),
    ("crm.contact", "name", "Name", "text", true, false),
    ("crm.contact", "email", "Email", "text", false, false),
    ("crm.deal", "title", "Title", "text", true, false),
    ("projects.task", "title", "Title", "text", true, false),
    ("projects.project", "name", "Name", "text", true, false),
    ("store.product", "name", "Name", "text", true, false),
    ("tickets.ticket", "subject", "Subject", "text", true, false),
    ("hr.employee", "name", "Name", "text", true, false),
];

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldValidation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
}

impl Default for FieldValidation {
    fn default() -> Self {
        Self {
            min: None,
            max: None,
            min_length: None,
            max_length: None,
            pattern: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDefinition {
    pub id: String,
    #[serde(default)]
    pub company_id: String,
    pub record_type: String,
    pub field_key: String,
    pub label: String,
    pub field_type: String,
    pub version: i64,
    pub status: String,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help_text: Option<String>,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub validation: FieldValidation,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateFieldDefinition {
    #[serde(default)]
    pub company_id: Option<String>,
    pub record_type: String,
    pub field_key: String,
    pub label: String,
    pub field_type: String,
    #[serde(default)]
    pub required: Option<bool>,
    #[serde(default)]
    pub default_value: Option<Value>,
    #[serde(default)]
    pub help_text: Option<String>,
    #[serde(default)]
    pub options: Option<Vec<String>>,
    #[serde(default)]
    pub validation: Option<FieldValidation>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateFieldDefinition {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub required: Option<bool>,
    #[serde(default)]
    pub default_value: Option<Value>,
    #[serde(default)]
    pub help_text: Option<String>,
    #[serde(default)]
    pub options: Option<Vec<String>>,
    #[serde(default)]
    pub validation: Option<FieldValidation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldValue {
    pub id: String,
    #[serde(default)]
    pub company_id: String,
    pub record_type: String,
    pub record_id: String,
    pub field_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_id: Option<String>,
    pub value_type: String,
    pub value: Value,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetFieldValue {
    #[serde(default)]
    pub company_id: Option<String>,
    pub record_type: String,
    pub record_id: String,
    pub field_key: String,
    pub value: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ValidateFieldValue {
    #[serde(default)]
    pub company_id: Option<String>,
    pub record_type: String,
    pub field_key: String,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedCustomField {
    pub definition: FieldDefinition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordType {
    pub record_type: String,
    pub module_id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateRecordType {
    pub record_type: String,
    pub module_id: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub system_fields: Option<Vec<CreateSystemField>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemField {
    pub id: String,
    pub record_type: String,
    pub field_key: String,
    pub label: String,
    pub field_type: String,
    pub required: bool,
    pub hideable: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateSystemField {
    pub field_key: String,
    pub label: String,
    pub field_type: String,
    #[serde(default)]
    pub required: Option<bool>,
    #[serde(default)]
    pub hideable: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListView {
    pub id: String,
    pub user_id: String,
    #[serde(default)]
    pub company_id: String,
    pub record_type: String,
    pub name: String,
    #[serde(default)]
    pub columns: Vec<String>,
    #[serde(default)]
    pub filters: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_by: Option<String>,
    pub visibility: String,
    pub scope: String,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateListView {
    #[serde(default)]
    pub company_id: Option<String>,
    pub record_type: String,
    pub name: String,
    #[serde(default)]
    pub columns: Option<Vec<String>>,
    #[serde(default)]
    pub filters: Option<Value>,
    #[serde(default)]
    pub sort_field: Option<String>,
    #[serde(default)]
    pub sort_order: Option<String>,
    #[serde(default)]
    pub group_by: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub is_default: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateListView {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub columns: Option<Vec<String>>,
    #[serde(default)]
    pub filters: Option<Value>,
    #[serde(default)]
    pub sort_field: Option<String>,
    #[serde(default)]
    pub sort_order: Option<String>,
    #[serde(default)]
    pub group_by: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub is_default: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutEntry {
    pub field_key: String,
    pub visible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutSection {
    pub title: String,
    #[serde(default)]
    pub entries: Vec<LayoutEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormLayout {
    pub id: String,
    #[serde(default)]
    pub company_id: String,
    pub record_type: String,
    pub name: String,
    #[serde(default)]
    pub sections: Vec<LayoutSection>,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateFormLayout {
    #[serde(default)]
    pub company_id: Option<String>,
    pub record_type: String,
    pub name: String,
    pub sections: Vec<LayoutSection>,
    #[serde(default)]
    pub is_default: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateFormLayout {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub sections: Option<Vec<LayoutSection>>,
    #[serde(default)]
    pub is_default: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FieldTypeInfo {
    pub field_type: String,
    pub label: String,
    pub supports_min_max: bool,
    pub supports_length: bool,
    pub supports_pattern: bool,
    pub supports_options: bool,
    pub supports_default: bool,
}

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct StudioState {
    pub definitions: DashMap<String, FieldDefinition>,
    pub values: DashMap<String, FieldValue>,
    pub views: DashMap<String, ListView>,
    pub layouts: DashMap<String, FormLayout>,
    pub record_types: DashMap<String, RecordType>,
    pub system_fields: DashMap<String, SystemField>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl StudioState {
    pub fn new() -> Self {
        let state = Self {
            definitions: DashMap::new(),
            values: DashMap::new(),
            views: DashMap::new(),
            layouts: DashMap::new(),
            record_types: DashMap::new(),
            system_fields: DashMap::new(),
            hooks: Arc::new(RwLock::new(None)),
        };
        // Seed the registry with the configurable defaults. An
        // administrator may replace or remove any of these through
        // the API or the registration hook; the seed only guarantees
        // the registry is never empty on a fresh install.
        let now = Utc::now();
        for (record_type, module_id, label) in DEFAULT_RECORD_TYPES {
            state.record_types.insert(
                record_type.to_string(),
                RecordType {
                    record_type: record_type.to_string(),
                    module_id: module_id.to_string(),
                    label: label.to_string(),
                    description: None,
                    created_at: now,
                    updated_at: now,
                },
            );
        }
        for (record_type, field_key, label, field_type, required, hideable) in
            DEFAULT_SYSTEM_FIELDS
        {
            state.system_fields.insert(
                Uuid::new_v4().to_string(),
                SystemField {
                    id: Uuid::new_v4().to_string(),
                    record_type: record_type.to_string(),
                    field_key: field_key.to_string(),
                    label: label.to_string(),
                    field_type: field_type.to_string(),
                    required,
                    hideable,
                    created_at: now,
                    updated_at: now,
                },
            );
        }
        state
    }

    fn emit(&self, event: &str, payload: Value) {
        if let Some(bus) = self.hooks.read().expect("hooks lock poisoned").clone() {
            bus.emit(event, payload);
        }
    }

    /// Effective definitions for a record type: the highest active
    /// version per field key. A company-specific definition shadows
    /// a global one with the same field key. Archived and
    /// superseded rows are never effective.
    pub fn effective_definitions(
        &self,
        record_type: &str,
        company_id: &str,
    ) -> Vec<FieldDefinition> {
        let mut by_key: std::collections::HashMap<String, FieldDefinition> =
            std::collections::HashMap::new();
        let mut candidates: Vec<FieldDefinition> = self
            .definitions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|definition| {
                definition.record_type == record_type && definition.status == STATUS_ACTIVE
            })
            .collect();
        // Global definitions first, company definitions last, so the
        // company row wins; within one scope the highest version wins.
        candidates.sort_by(|a, b| {
            let a_company = !a.company_id.is_empty();
            let b_company = !b.company_id.is_empty();
            a_company
                .cmp(&b_company)
                .then(a.version.cmp(&b.version))
        });
        for definition in candidates {
            if definition.company_id.is_empty() || definition.company_id == company_id {
                by_key.insert(definition.field_key.clone(), definition);
            }
        }
        let mut out: Vec<FieldDefinition> = by_key.into_values().collect();
        out.sort_by(|a, b| a.field_key.cmp(&b.field_key));
        out
    }

    fn system_fields_of(&self, record_type: &str) -> Vec<SystemField> {
        let mut items: Vec<SystemField> = self
            .system_fields
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|field| field.record_type == record_type)
            .collect();
        items.sort_by(|a, b| a.field_key.cmp(&b.field_key));
        items
    }

    /// The default layout for a record type and company: the
    /// company default first, then the global default, else none.
    pub fn default_layout(&self, record_type: &str, company_id: &str) -> Option<FormLayout> {
        let mut items: Vec<FormLayout> = self
            .layouts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|layout| layout.record_type == record_type && layout.is_default)
            .collect();
        items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        items
            .iter()
            .find(|layout| layout.company_id == company_id)
            .cloned()
            .or_else(|| {
                items
                    .iter()
                    .find(|layout| layout.company_id.is_empty())
                    .cloned()
            })
    }

    /// Views visible to a user: their own views plus shared company
    /// views for the record type. Defaults first, then newest first.
    pub fn visible_views(
        &self,
        record_type: &str,
        company_id: &str,
        user_id: &str,
    ) -> Vec<ListView> {
        let mut items: Vec<ListView> = self
            .views
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|view| {
                view.record_type == record_type
                    && (view.user_id == user_id
                        || (view.visibility == VISIBILITY_SHARED
                            && (view.company_id.is_empty() || view.company_id == company_id)))
            })
            .collect();
        items.sort_by(|a, b| {
            b.is_default
                .cmp(&a.is_default)
                .then(b.updated_at.cmp(&a.updated_at))
        });
        items
    }

    fn clear_other_view_defaults(&self, view: &ListView) {
        for mut entry in self.views.iter_mut() {
            let other = entry.value_mut();
            if other.id != view.id
                && other.record_type == view.record_type
                && other.is_default
                && view.scope == "company"
                && other.scope == "company"
                && other.company_id == view.company_id
            {
                other.is_default = false;
                other.updated_at = Utc::now();
            }
            if other.id != view.id
                && other.record_type == view.record_type
                && other.is_default
                && view.scope == "user"
                && other.scope == "user"
                && other.user_id == view.user_id
            {
                other.is_default = false;
                other.updated_at = Utc::now();
            }
        }
    }

    fn clear_other_layout_defaults(&self, layout: &FormLayout) {
        for mut entry in self.layouts.iter_mut() {
            let other = entry.value_mut();
            if other.id != layout.id
                && other.record_type == layout.record_type
                && other.company_id == layout.company_id
                && other.is_default
            {
                other.is_default = false;
                other.updated_at = Utc::now();
            }
        }
    }

    fn register_record_type(
        &self,
        input: CreateRecordType,
    ) -> UnifiedResult<RecordType> {
        let record_type = validate_record_type(&input.record_type)?;
        validate_module_id(&input.module_id)?;
        let label = require_text(&input.label, "label", MAX_LABEL_LEN)?;
        if self.record_types.contains_key(&record_type) {
            return Err(UnifiedError::conflict(format!(
                "Record type \"{record_type}\" is already registered"
            )));
        }
        let now = Utc::now();
        let registered = RecordType {
            record_type: record_type.clone(),
            module_id: input.module_id.trim().to_string(),
            label,
            description: input
                .description
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            created_at: now,
            updated_at: now,
        };
        self.record_types
            .insert(record_type.clone(), registered.clone());
        if let Some(fields) = input.system_fields {
            for field in fields {
                let created = self.add_system_field(&record_type, field)?;
                let _ = created;
            }
        }
        self.emit(
            EVENT_RECORD_TYPE_REGISTERED,
            json!({
                "record_type": record_type,
                "module_id": registered.module_id,
                "label": registered.label,
            }),
        );
        Ok(registered)
    }

    fn add_system_field(
        &self,
        record_type: &str,
        input: CreateSystemField,
    ) -> UnifiedResult<SystemField> {
        let field_key = validate_system_field_key(&input.field_key)?;
        let label = require_text(&input.label, "label", MAX_LABEL_LEN)?;
        validate_field_type(&input.field_type)?;
        if self
            .system_fields_of(record_type)
            .iter()
            .any(|field| field.field_key == field_key)
        {
            return Err(UnifiedError::conflict(format!(
                "System field \"{field_key}\" already exists for record type \"{record_type}\""
            )));
        }
        let now = Utc::now();
        let field = SystemField {
            id: Uuid::new_v4().to_string(),
            record_type: record_type.to_string(),
            field_key,
            label,
            field_type: input.field_type.trim().to_string(),
            required: input.required.unwrap_or(false),
            hideable: input.hideable.unwrap_or(true),
            created_at: now,
            updated_at: now,
        };
        self.system_fields
            .insert(field.id.clone(), field.clone());
        Ok(field)
    }
}

impl Default for StudioState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Caller identity and capability checks.
// ---------------------------------------------------------------------------

struct Caller {
    actor: String,
    role: String,
}

/// Resolve the tenant context for a request. THE single
/// convergence point for tenant-scoped data access in this file
/// (see `crate::core::tenancy`): this function returns the tenancy
/// `TenantContext` (tenant id as Uuid + schema name derived only
/// by `tenancy::schema_name()`), and the Diesel persistence wiring
/// — when it lands — takes the Uuid from here into
/// `tenancy::with_tenant_connection`. Fail closed:
/// `from_http_request` answers 401 when the request did not
/// resolve to a tenant and 403 when the resolved tenant id is not
/// a valid UUID; no resolved tenant means the request is refused
/// here, never served from `public`.
///
/// OPEN (not closed by this guard): the in-memory DashMap stores
/// in this module carry no `tenant_id` on their records yet, so
/// there is no per-record tenant filtering to apply once the
/// context is resolved — that needs a store-shape change (tenant
/// id on every record + a migration column) and is integrator
/// work. What this guard does close is the context requirement
/// itself: every tenant-data endpoint resolves the tenant first
/// through `caller()` (via `guard()`), and a request with no
/// tenant context is rejected before any business data is read.
fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<TenancyContext> {
    TenancyContext::from_http_request(req)
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    // Tenant first: no resolved tenant context means fail closed,
    // never fall back to the public schema. The resolved context
    // is bound for the Diesel persistence layer (see
    // `resolve_tenant`); the in-memory stores below carry no
    // tenant id yet, so per-record filtering is still open.
    let _tenant = resolve_tenant(req)?;
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, call: &Caller, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, &call.role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{}\" does not hold permission \"{}\"",
            call.role, permission
        )))
    }
}

fn guard(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<Caller> {
    let call = caller(req)?;
    require_permission(ctx, &call, permission)?;
    Ok(call)
}

// ---------------------------------------------------------------------------
// Validation.
// ---------------------------------------------------------------------------

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.chars().count() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            if trimmed.chars().count() > max {
                return Err(UnifiedError::bad_request(format!(
                    "{field} must be at most {max} characters"
                )));
            }
            Ok(Some(trimmed.to_string()))
        }
    }
}

fn normalize_company_id(value: &Option<String>) -> String {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(GLOBAL_COMPANY_ID)
        .to_string()
}

/// A record type looks like `module.kind`: lowercase letters,
/// digits, and underscores, at least one dot. The registry is the
/// authority; this only rejects malformed names before lookup.
fn validate_record_type(raw: &str) -> UnifiedResult<String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(UnifiedError::bad_request("record_type must not be empty"));
    }
    if value.len() > MAX_RECORD_TYPE_LEN {
        return Err(UnifiedError::bad_request(
            "record_type must be at most 128 characters",
        ));
    }
    let valid = value.contains('.')
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
        && !value.starts_with('.')
        && !value.ends_with('.')
        && !value.contains("..");
    if !valid {
        return Err(UnifiedError::bad_request(
            "record_type must look like \"module.kind\" (lowercase letters, digits, underscores, dots)",
        ));
    }
    Ok(value.to_string())
}

fn validate_module_id(raw: &str) -> UnifiedResult<String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(UnifiedError::bad_request("module_id must not be empty"));
    }
    if value.len() > 64
        || !value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(UnifiedError::bad_request(
            "module_id must be lowercase letters, digits, or underscores (at most 64 characters)",
        ));
    }
    Ok(value.to_string())
}

/// Custom field keys live in the `x_` namespace so they can never
/// collide with a system field.
fn validate_field_key(raw: &str) -> UnifiedResult<String> {
    let value = raw.trim();
    if !value.starts_with(FIELD_KEY_PREFIX) {
        return Err(UnifiedError::bad_request(format!(
            "field_key must start with the \"{FIELD_KEY_PREFIX}\" prefix"
        )));
    }
    let rest = &value[FIELD_KEY_PREFIX.len()..];
    if rest.is_empty()
        || rest.len() > 60
        || !rest
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(UnifiedError::bad_request(
            "field_key must be \"x_\" followed by lowercase letters, digits, or underscores (at most 60 characters after the prefix)",
        ));
    }
    Ok(value.to_string())
}

/// System field keys use the plain namespace (no `x_` prefix);
/// that is exactly what keeps the two namespaces apart.
fn validate_system_field_key(raw: &str) -> UnifiedResult<String> {
    let value = raw.trim();
    if value.is_empty() || value.starts_with(FIELD_KEY_PREFIX) {
        return Err(UnifiedError::bad_request(
            "system field_key must not use the custom \"x_\" prefix",
        ));
    }
    if value.len() > 64
        || !value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(UnifiedError::bad_request(
            "system field_key must be lowercase letters, digits, or underscores (at most 64 characters)",
        ));
    }
    Ok(value.to_string())
}

fn validate_field_type(raw: &str) -> UnifiedResult<String> {
    let value = raw.trim();
    if FIELD_TYPES.contains(&value) {
        Ok(value.to_string())
    } else {
        Err(UnifiedError::bad_request(format!(
            "field_type must be one of: {}",
            FIELD_TYPES.join(", ")
        )))
    }
}

fn validate_visibility(raw: &str) -> UnifiedResult<&'static str> {
    match raw.trim() {
        VISIBILITY_PRIVATE => Ok(VISIBILITY_PRIVATE),
        VISIBILITY_SHARED => Ok(VISIBILITY_SHARED),
        _ => Err(UnifiedError::bad_request(
            "visibility must be \"private\" or \"shared\"",
        )),
    }
}

fn validate_order(raw: &str) -> UnifiedResult<String> {
    match raw.trim().to_lowercase().as_str() {
        "asc" => Ok("asc".to_string()),
        "desc" => Ok("desc".to_string()),
        _ => Err(UnifiedError::bad_request("order must be \"asc\" or \"desc\"")),
    }
}

fn validate_sort_field(raw: &str) -> UnifiedResult<String> {
    let value = raw.trim();
    if value.is_empty() || value.len() > 64 {
        return Err(UnifiedError::bad_request(
            "sort_field must be a plain field name (at most 64 characters)",
        ));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return Err(UnifiedError::bad_request(
            "sort_field must contain only letters, digits, underscores, and dots",
        ));
    }
    Ok(value.to_string())
}

/// Validate the validation rules themselves. A bad rule is rejected
/// at definition time, never at use time — including the regex,
/// which is compiled here.
fn validate_rules(field_type: &str, rules: &FieldValidation) -> UnifiedResult<()> {
    let numeric = field_type == FIELD_TYPE_NUMBER || field_type == FIELD_TYPE_DECIMAL;
    let textual = field_type == FIELD_TYPE_TEXT;
    if !numeric && (rules.min.is_some() || rules.max.is_some()) {
        return Err(UnifiedError::bad_request(
            "min / max only apply to number and decimal fields",
        ));
    }
    if !textual && (rules.min_length.is_some() || rules.max_length.is_some()) {
        return Err(UnifiedError::bad_request(
            "min_length / max_length only apply to text fields",
        ));
    }
    if !textual && rules.pattern.is_some() {
        return Err(UnifiedError::bad_request(
            "pattern only applies to text fields",
        ));
    }
    if let (Some(min), Some(max)) = (rules.min, rules.max) {
        if min > max {
            return Err(UnifiedError::bad_request("min must not be greater than max"));
        }
    }
    if let (Some(min), Some(max)) = (rules.min_length, rules.max_length) {
        if min > max {
            return Err(UnifiedError::bad_request(
                "min_length must not be greater than max_length",
            ));
        }
    }
    if let Some(pattern) = &rules.pattern {
        if pattern.is_empty() || pattern.chars().count() > MAX_PATTERN_LEN {
            return Err(UnifiedError::bad_request(format!(
                "pattern must be 1..={MAX_PATTERN_LEN} characters"
            )));
        }
        // Compile now: an invalid regex is a definition error.
        Regex::new(pattern)
            .map_err(|_| UnifiedError::bad_request("pattern is not a valid regular expression"))?;
    }
    Ok(())
}

fn validate_options(field_type: &str, options: &[String]) -> UnifiedResult<Vec<String>> {
    if field_type == FIELD_TYPE_SELECT {
        if options.is_empty() {
            return Err(UnifiedError::bad_request(
                "select fields require at least one option",
            ));
        }
        if options.len() > MAX_OPTIONS {
            return Err(UnifiedError::bad_request(format!(
                "select fields allow at most {MAX_OPTIONS} options"
            )));
        }
        let mut cleaned: Vec<String> = Vec::with_capacity(options.len());
        for option in options {
            let value = require_text(option, "option", MAX_OPTION_LEN)?;
            if cleaned.contains(&value) {
                return Err(UnifiedError::bad_request(format!(
                    "select option \"{value}\" is duplicated"
                )));
            }
            cleaned.push(value);
        }
        Ok(cleaned)
    } else {
        if !options.is_empty() {
            return Err(UnifiedError::bad_request(
                "options only apply to select fields",
            ));
        }
        Ok(Vec::new())
    }
}

/// Validate one stored value against its definition. `None` means
/// "no value supplied": allowed only when the field is not required.
fn validate_value(definition: &FieldDefinition, value: Option<&Value>) -> UnifiedResult<()> {
    let value = match value {
        None => {
            if definition.required {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{}\" is required",
                    definition.field_key
                )));
            }
            return Ok(());
        }
        Some(value) => value,
    };
    let rules = &definition.validation;
    match definition.field_type.as_str() {
        FIELD_TYPE_TEXT => {
            let text = value.as_str().ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Field \"{}\" must be a text value",
                    definition.field_key
                ))
            })?;
            if definition.required && text.trim().is_empty() {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{}\" is required",
                    definition.field_key
                )));
            }
            let len = text.chars().count() as u32;
            if let Some(min) = rules.min_length {
                if len < min {
                    return Err(UnifiedError::bad_request(format!(
                        "Field \"{}\" must be at least {min} characters",
                        definition.field_key
                    )));
                }
            }
            if let Some(max) = rules.max_length {
                if len > max {
                    return Err(UnifiedError::bad_request(format!(
                        "Field \"{}\" must be at most {max} characters",
                        definition.field_key
                    )));
                }
            }
            if let Some(pattern) = &rules.pattern {
                // The pattern was compiled at definition time, so
                // this compile cannot fail on a stored definition;
                // a failure here is treated as a definition error,
                // never as a user error.
                let regex = Regex::new(pattern).map_err(|_| {
                    UnifiedError::internal(format!(
                        "Stored pattern for field \"{}\" is invalid",
                        definition.field_key
                    ))
                })?;
                if !regex.is_match(text) {
                    return Err(UnifiedError::bad_request(format!(
                        "Field \"{}\" does not match the required pattern",
                        definition.field_key
                    )));
                }
            }
            Ok(())
        }
        FIELD_TYPE_NUMBER | FIELD_TYPE_DECIMAL => {
            let number = value.as_f64().ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Field \"{}\" must be a number",
                    definition.field_key
                ))
            })?;
            if definition.field_type == FIELD_TYPE_NUMBER && number.fract() != 0.0 {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{}\" must be a whole number",
                    definition.field_key
                )));
            }
            if let Some(min) = rules.min {
                if number < min {
                    return Err(UnifiedError::bad_request(format!(
                        "Field \"{}\" must be at least {min}",
                        definition.field_key
                    )));
                }
            }
            if let Some(max) = rules.max {
                if number > max {
                    return Err(UnifiedError::bad_request(format!(
                        "Field \"{}\" must be at most {max}",
                        definition.field_key
                    )));
                }
            }
            Ok(())
        }
        FIELD_TYPE_DATE => {
            let text = value.as_str().ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Field \"{}\" must be a date string (YYYY-MM-DD)",
                    definition.field_key
                ))
            })?;
            NaiveDate::parse_from_str(text, "%Y-%m-%d").map_err(|_| {
                UnifiedError::bad_request(format!(
                    "Field \"{}\" must be a valid date (YYYY-MM-DD)",
                    definition.field_key
                ))
            })?;
            Ok(())
        }
        FIELD_TYPE_BOOLEAN => {
            if value.as_bool().is_none() {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{}\" must be true or false",
                    definition.field_key
                )));
            }
            Ok(())
        }
        FIELD_TYPE_SELECT => {
            let text = value.as_str().ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Field \"{}\" must be one of its options",
                    definition.field_key
                ))
            })?;
            if !definition.options.iter().any(|option| option == text) {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{}\" must be one of: {}",
                    definition.field_key,
                    definition.options.join(", ")
                )));
            }
            Ok(())
        }
        FIELD_TYPE_USER_REFERENCE => {
            let text = value.as_str().ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Field \"{}\" must be a user reference (user id string)",
                    definition.field_key
                ))
            })?;
            if text.trim().is_empty() || text.chars().count() > MAX_USER_ID_LEN {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{}\" must be a user id (1..={MAX_USER_ID_LEN} characters)",
                    definition.field_key
                )));
            }
            Ok(())
        }
        _ => Err(UnifiedError::internal(format!(
            "Unknown field type \"{}\" on field \"{}\"",
            definition.field_type, definition.field_key
        ))),
    }
}

/// A stored filter is a flat field/value map (same rule as the
/// saved-views module): scalars or arrays of scalars only, never
/// nested objects — a filter is data, never executable logic.
fn validate_filter(filter: &Value) -> UnifiedResult<()> {
    let object = match filter {
        Value::Object(map) => map,
        _ => {
            return Err(UnifiedError::bad_request(
                "filters must be a JSON object",
            ));
        }
    };
    for (key, value) in object {
        if key.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "filter keys must not be empty",
            ));
        }
        match value {
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
            Value::Array(items) => {
                for item in items {
                    match item {
                        Value::Null
                        | Value::Bool(_)
                        | Value::Number(_)
                        | Value::String(_) => {}
                        _ => {
                            return Err(UnifiedError::bad_request(
                                "filter arrays must contain scalar values only",
                            ));
                        }
                    }
                }
            }
            Value::Object(_) => {
                return Err(UnifiedError::bad_request(
                    "filter values must be scalars or arrays of scalars (nested objects are not allowed)",
                ));
            }
        }
    }
    Ok(())
}

fn validate_columns(columns: &[String]) -> UnifiedResult<Vec<String>> {
    if columns.is_empty() {
        return Err(UnifiedError::bad_request("columns must not be empty"));
    }
    if columns.len() > MAX_COLUMNS {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_COLUMNS} columns are allowed"
        )));
    }
    let mut cleaned: Vec<String> = Vec::with_capacity(columns.len());
    for column in columns {
        let value = column.trim();
        if value.is_empty() || value.len() > 64 {
            return Err(UnifiedError::bad_request(
                "column names must be 1..=64 characters",
            ));
        }
        if !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        {
            return Err(UnifiedError::bad_request(
                "column names must contain only letters, digits, underscores, and dots",
            ));
        }
        if cleaned.contains(&value.to_string()) {
            return Err(UnifiedError::bad_request(format!(
                "column \"{value}\" is duplicated"
            )));
        }
        cleaned.push(value.to_string());
    }
    Ok(cleaned)
}

/// Validate a form layout: every entry must name a known field
/// (a system field or an effective custom definition for this
/// record type), and a system field that is required or
/// non-hideable may never be hidden. Custom fields may always be
/// hidden — they are optional by construction.
fn validate_layout(
    state: &StudioState,
    record_type: &str,
    company_id: &str,
    sections: &[LayoutSection],
) -> UnifiedResult<()> {
    if sections.is_empty() {
        return Err(UnifiedError::bad_request("sections must not be empty"));
    }
    if sections.len() > MAX_SECTIONS {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_SECTIONS} sections are allowed"
        )));
    }
    let system = state.system_fields_of(record_type);
    let custom = state.effective_definitions(record_type, company_id);
    let custom_keys: Vec<&str> = custom
        .iter()
        .map(|definition| definition.field_key.as_str())
        .collect();
    let mut seen: Vec<String> = Vec::new();
    let mut total = 0usize;
    for section in sections {
        let title = section.title.trim();
        if title.is_empty() || title.chars().count() > MAX_LABEL_LEN {
            return Err(UnifiedError::bad_request(
                "section titles must be 1..=100 characters",
            ));
        }
        for entry in &section.entries {
            total += 1;
            if total > MAX_LAYOUT_ENTRIES {
                return Err(UnifiedError::bad_request(format!(
                    "at most {MAX_LAYOUT_ENTRIES} layout entries are allowed"
                )));
            }
            let key = entry.field_key.trim();
            if seen.contains(&key.to_string()) {
                return Err(UnifiedError::bad_request(format!(
                    "field \"{key}\" appears more than once in the layout"
                )));
            }
            seen.push(key.to_string());
            if let Some(field) = system.iter().find(|field| field.field_key == key) {
                if !entry.visible && (field.required || !field.hideable) {
                    return Err(UnifiedError::bad_request(format!(
                        "System field \"{key}\" is required or non-hideable and cannot be hidden"
                    )));
                }
            } else if custom_keys.contains(&key) {
                // Custom fields may be shown or hidden freely.
            } else {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown field \"{key}\" for record type \"{record_type}\""
                )));
            }
        }
    }
    // Every required / non-hideable system field must appear
    // visible somewhere in the layout; a layout that silently
    // drops one would break the owning module's form.
    for field in &system {
        if field.required || !field.hideable {
            let shown = sections.iter().any(|section| {
                section
                    .entries
                    .iter()
                    .any(|entry| entry.field_key == field.field_key && entry.visible)
            });
            if !shown {
                return Err(UnifiedError::bad_request(format!(
                    "System field \"{}\" is required or non-hideable and must appear visible in the layout",
                    field.field_key
                )));
            }
        }
    }
    Ok(())
}

fn require_record_type(state: &StudioState, raw: &str) -> UnifiedResult<String> {
    let record_type = validate_record_type(raw)?;
    if !state.record_types.contains_key(&record_type) {
        return Err(UnifiedError::not_found(format!(
            "Record type \"{record_type}\" is not registered"
        )));
    }
    Ok(record_type)
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

fn field_type_infos() -> Vec<FieldTypeInfo> {
    vec![
        FieldTypeInfo {
            field_type: FIELD_TYPE_TEXT.to_string(),
            label: "Text".to_string(),
            supports_min_max: false,
            supports_length: true,
            supports_pattern: true,
            supports_options: false,
            supports_default: true,
        },
        FieldTypeInfo {
            field_type: FIELD_TYPE_NUMBER.to_string(),
            label: "Number".to_string(),
            supports_min_max: true,
            supports_length: false,
            supports_pattern: false,
            supports_options: false,
            supports_default: true,
        },
        FieldTypeInfo {
            field_type: FIELD_TYPE_DECIMAL.to_string(),
            label: "Decimal".to_string(),
            supports_min_max: true,
            supports_length: false,
            supports_pattern: false,
            supports_options: false,
            supports_default: true,
        },
        FieldTypeInfo {
            field_type: FIELD_TYPE_DATE.to_string(),
            label: "Date".to_string(),
            supports_min_max: false,
            supports_length: false,
            supports_pattern: false,
            supports_options: false,
            supports_default: true,
        },
        FieldTypeInfo {
            field_type: FIELD_TYPE_BOOLEAN.to_string(),
            label: "Boolean".to_string(),
            supports_min_max: false,
            supports_length: false,
            supports_pattern: false,
            supports_options: false,
            supports_default: true,
        },
        FieldTypeInfo {
            field_type: FIELD_TYPE_SELECT.to_string(),
            label: "Select".to_string(),
            supports_min_max: false,
            supports_length: false,
            supports_pattern: false,
            supports_options: true,
            supports_default: true,
        },
        FieldTypeInfo {
            field_type: FIELD_TYPE_USER_REFERENCE.to_string(),
            label: "User reference".to_string(),
            supports_min_max: false,
            supports_length: false,
            supports_pattern: false,
            supports_options: false,
            supports_default: false,
        },
    ]
}

// ---------------------------------------------------------------------------
// Hook query handlers (the hook contract).
// ---------------------------------------------------------------------------

fn on_fields_requested(state: Arc<StudioState>, ctx: &mut HookContext) {
    let record_type = ctx
        .get("record_type")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if record_type.is_empty() || !state.record_types.contains_key(&record_type) {
        ctx.add_result(json!({
            "record_type": record_type,
            "company_id": company_id,
            "definitions": [],
            "error": "unknown record type",
        }));
        return;
    }
    let definitions = state.effective_definitions(&record_type, &company_id);
    ctx.add_result(json!({
        "record_type": record_type,
        "company_id": company_id,
        "definitions": definitions,
    }));
}

fn on_layout_requested(state: Arc<StudioState>, ctx: &mut HookContext) {
    let record_type = ctx
        .get("record_type")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let layout = if record_type.is_empty() {
        None
    } else {
        state.default_layout(&record_type, &company_id)
    };
    ctx.add_result(json!({
        "record_type": record_type,
        "company_id": company_id,
        "layout": layout,
    }));
}

fn on_views_requested(state: Arc<StudioState>, ctx: &mut HookContext) {
    let record_type = ctx
        .get("record_type")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let views = if record_type.is_empty() || user_id.is_empty() {
        Vec::new()
    } else {
        state.visible_views(&record_type, &company_id, &user_id)
    };
    ctx.add_result(json!({
        "record_type": record_type,
        "company_id": company_id,
        "user_id": user_id,
        "views": views,
    }));
}

fn on_record_type_register(state: Arc<StudioState>, ctx: &mut HookContext) {
    let input: Result<CreateRecordType, _> = serde_json::from_value(ctx.data.clone());
    match input {
        Ok(parsed) => match state.register_record_type(parsed) {
            Ok(registered) => {
                ctx.add_result(json!({
                    "registered": true,
                    "record_type": registered.record_type,
                    "module_id": registered.module_id,
                }));
            }
            Err(error) => {
                ctx.add_result(json!({
                    "registered": false,
                    "error": error.message.clone(),
                }));
            }
        },
        Err(_) => {
            ctx.add_result(json!({
                "registered": false,
                "error": "record_type.register payload must contain record_type, module_id, and label",
            }));
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers: field types and record types.
// ---------------------------------------------------------------------------

async fn list_field_types(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    match guard(&ctx, &req, PERM_STUDIO_VIEW) {
        Ok(_) => json::success_response(field_type_infos(), 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_record_types(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_VIEW) {
        return json::error_response(&error, debug);
    }
    let params = query.into_inner();
    let module_filter = params.filters.get("module_id").cloned();
    let mut items: Vec<RecordType> = state
        .record_types
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| {
            matches_search(&format!("{} {} {}", record.record_type, record.module_id, record.label), &params)
                && module_filter
                    .as_ref()
                    .is_none_or(|module_id| record.module_id == *module_id)
        })
        .collect();
    items.sort_by(|a, b| a.record_type.cmp(&b.record_type));
    json::success_response(paginate(items, &params), 200)
}

async fn create_record_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    body: web::Json<CreateRecordType>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_ADMIN) {
        return json::error_response(&error, debug);
    }
    let result = state.register_record_type(body.into_inner());
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_record_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<RecordType> = state
        .record_types
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| {
            UnifiedError::not_found(format!("Record type \"{}\" is not registered", path.as_str()))
        });
    json::respond(result, debug)
}

async fn delete_record_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_ADMIN) {
        return json::error_response(&error, debug);
    }
    let record_type = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        if state
            .definitions
            .iter()
            .any(|entry| entry.value().record_type == record_type)
            || state
                .values
                .iter()
                .any(|entry| entry.value().record_type == record_type)
            || state
                .views
                .iter()
                .any(|entry| entry.value().record_type == record_type)
            || state
                .layouts
                .iter()
                .any(|entry| entry.value().record_type == record_type)
        {
            return Err(UnifiedError::conflict(format!(
                "Record type \"{record_type}\" still has definitions, values, views, or layouts; remove those first"
            )));
        }
        state
            .record_types
            .remove(&record_type)
            .map(|_| json!({ "deleted": true, "record_type": record_type }))
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "Record type \"{record_type}\" is not registered"
                ))
            })
    })();
    json::respond(result, debug)
}

async fn list_system_fields(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_VIEW) {
        return json::error_response(&error, debug);
    }
    let record_type = path.into_inner();
    if !state.record_types.contains_key(&record_type) {
        return json::error_response(
            &UnifiedError::not_found(format!(
                "Record type \"{record_type}\" is not registered"
            )),
            debug,
        );
    }
    let params = query.into_inner();
    let items = state.system_fields_of(&record_type);
    let items: Vec<SystemField> = items
        .into_iter()
        .filter(|field| matches_search(&format!("{} {}", field.field_key, field.label), &params))
        .collect();
    json::success_response(paginate(items, &params), 200)
}

async fn create_system_field(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateSystemField>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_ADMIN) {
        return json::error_response(&error, debug);
    }
    let record_type = path.into_inner();
    if !state.record_types.contains_key(&record_type) {
        return json::error_response(
            &UnifiedError::not_found(format!(
                "Record type \"{record_type}\" is not registered"
            )),
            debug,
        );
    }
    let result = state.add_system_field(&record_type, body.into_inner());
    match result {
        Ok(field) => json::success_response(field, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// Handlers: field definitions.
// ---------------------------------------------------------------------------

fn validate_new_definition(
    state: &StudioState,
    input: &CreateFieldDefinition,
) -> UnifiedResult<FieldDefinition> {
    let record_type = require_record_type(state, &input.record_type)?;
    let company_id = normalize_company_id(&input.company_id);
    let field_key = validate_field_key(&input.field_key)?;
    let label = require_text(&input.label, "label", MAX_LABEL_LEN)?;
    let field_type = validate_field_type(&input.field_type)?;
    let rules = input.validation.clone().unwrap_or_default();
    validate_rules(&field_type, &rules)?;
    let options = validate_options(&field_type, &input.options.clone().unwrap_or_default())?;
    let help_text = optional_text(&input.help_text, "help_text", MAX_HELP_LEN)?;
    let required = input.required.unwrap_or(false);
    if let Some(default) = &input.default_value {
        let probe = FieldDefinition {
            id: String::new(),
            company_id: company_id.clone(),
            record_type: record_type.clone(),
            field_key: field_key.clone(),
            label: label.clone(),
            field_type: field_type.clone(),
            version: 1,
            status: STATUS_ACTIVE.to_string(),
            required,
            default_value: Some(default.clone()),
            help_text: help_text.clone(),
            options: options.clone(),
            validation: rules.clone(),
            created_by: String::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        validate_value(&probe, Some(default))?;
    }
    if state
        .definitions
        .iter()
        .any(|entry| {
            let definition = entry.value();
            definition.record_type == record_type
                && definition.company_id == company_id
                && definition.field_key == field_key
                && definition.status == STATUS_ACTIVE
        })
    {
        return Err(UnifiedError::conflict(format!(
            "Field \"{field_key}\" already exists for record type \"{record_type}\" in this company scope; update it to create a new version"
        )));
    }
    Ok(FieldDefinition {
        id: Uuid::new_v4().to_string(),
        company_id,
        record_type,
        field_key,
        label,
        field_type,
        version: 1,
        status: STATUS_ACTIVE.to_string(),
        required,
        default_value: input.default_value.clone(),
        help_text,
        options,
        validation: rules,
        created_by: String::new(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    })
}

async fn list_definitions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_VIEW) {
        return json::error_response(&error, debug);
    }
    let params = query.into_inner();
    let record_filter = params.filters.get("record_type").cloned();
    let company_filter = params.filters.get("company_id").cloned();
    let status_filter = params.filters.get("status").cloned();
    let type_filter = params.filters.get("field_type").cloned();
    let mut items: Vec<FieldDefinition> = state
        .definitions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|definition| {
            matches_search(
                &format!("{} {} {}", definition.field_key, definition.label, definition.record_type),
                &params,
            ) && record_filter
                .as_ref()
                .is_none_or(|record_type| definition.record_type == *record_type)
                && company_filter
                    .as_ref()
                    .is_none_or(|company_id| definition.company_id == *company_id)
                && status_filter
                    .as_ref()
                    .is_none_or(|status| definition.status == *status)
                && type_filter
                    .as_ref()
                    .is_none_or(|field_type| definition.field_type == *field_type)
        })
        .collect();
    items.sort_by(|a, b| {
        a.record_type
            .cmp(&b.record_type)
            .then(a.field_key.cmp(&b.field_key))
            .then(b.version.cmp(&a.version))
    });
    json::success_response(paginate(items, &params), 200)
}

async fn create_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    body: web::Json<CreateFieldDefinition>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let call = match guard(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        Ok(call) => call,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<FieldDefinition> = (|| {
        let mut definition = validate_new_definition(&state, &body.into_inner())?;
        definition.created_by = call.actor.clone();
        state
            .definitions
            .insert(definition.id.clone(), definition.clone());
        state.emit(
            EVENT_FIELDS_CHANGED,
            json!({
                "field_key": definition.field_key,
                "record_type": definition.record_type,
                "company_id": definition.company_id,
                "version": definition.version,
                "definition_id": definition.id,
                "action": "created",
            }),
        );
        Ok(definition)
    })();
    match result {
        Ok(definition) => json::success_response(definition, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<FieldDefinition> = state
        .definitions
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Field definition not found"));
    json::respond(result, debug)
}

/// Update creates a NEW version: the old row becomes `superseded`
/// and is never mutated, so values validated against it keep
/// working. The field type and field key are immutable — changing
/// a type is a new field, not a new version.
async fn update_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateFieldDefinition>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let call = match guard(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        Ok(call) => call,
        Err(error) => return json::error_response(&error, debug),
    };
    let input = body.into_inner();
    let result: UnifiedResult<FieldDefinition> = (|| {
        let old = state
            .definitions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Field definition not found"))?;
        if old.status == STATUS_ARCHIVED {
            return Err(UnifiedError::conflict(
                "An archived field definition cannot be updated; create a new field instead",
            ));
        }
        let label = match &input.label {
            Some(label) => require_text(label, "label", MAX_LABEL_LEN)?,
            None => old.label.clone(),
        };
        let required = input.required.unwrap_or(old.required);
        let help_text = match &input.help_text {
            Some(help) => optional_text(&Some(help.clone()), "help_text", MAX_HELP_LEN)?,
            None => old.help_text.clone(),
        };
        let options = match &input.options {
            Some(options) => validate_options(&old.field_type, options)?,
            None => old.options.clone(),
        };
        let rules = match &input.validation {
            Some(rules) => {
                validate_rules(&old.field_type, rules)?;
                rules.clone()
            }
            None => old.validation.clone(),
        };
        let default_value = match &input.default_value {
            Some(default) => Some(default.clone()),
            None => old.default_value.clone(),
        };
        let mut next = old.clone();
        next.label = label;
        next.required = required;
        next.help_text = help_text;
        next.options = options;
        next.validation = rules;
        next.default_value = default_value.clone();
        if let Some(default) = &next.default_value {
            validate_value(&next, Some(default))?;
        }
        // Re-validate stored values only against the new rules when
        // they still reference this field key: values that no longer
        // validate stay attached to the old version and are reported
        // through the resolved endpoint instead of being deleted.
        next.id = Uuid::new_v4().to_string();
        next.version = old.version + 1;
        next.status = STATUS_ACTIVE.to_string();
        next.created_by = call.actor.clone();
        next.updated_at = Utc::now();
        if let Some(mut old_row) = state.definitions.get_mut(old.id.as_str()) {
            old_row.status = STATUS_SUPERSEDED.to_string();
            old_row.updated_at = Utc::now();
        }
        state.definitions.insert(next.id.clone(), next.clone());
        state.emit(
            EVENT_FIELDS_CHANGED,
            json!({
                "field_key": next.field_key,
                "record_type": next.record_type,
                "company_id": next.company_id,
                "version": next.version,
                "definition_id": next.id,
                "action": "updated",
            }),
        );
        Ok(next)
    })();
    match result {
        Ok(definition) => json::success_response(definition, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn delete_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    // Delete archives the row: definitions are never hard-deleted
    // while values may still reference them.
    archive_definition(ctx, state, req, path).await
}

async fn archive_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<FieldDefinition> = (|| {
        let mut definition = state
            .definitions
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Field definition not found"))?;
        definition.status = STATUS_ARCHIVED.to_string();
        definition.updated_at = Utc::now();
        let archived = (*definition).clone();
        drop(definition);
        state.emit(
            EVENT_FIELDS_CHANGED,
            json!({
                "field_key": archived.field_key,
                "record_type": archived.record_type,
                "company_id": archived.company_id,
                "version": archived.version,
                "definition_id": archived.id,
                "action": "archived",
            }),
        );
        Ok(archived)
    })();
    json::respond(result, debug)
}

async fn list_effective_definitions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_VIEW) {
        return json::error_response(&error, debug);
    }
    let record_type = path.into_inner();
    if !state.record_types.contains_key(&record_type) {
        return json::error_response(
            &UnifiedError::not_found(format!(
                "Record type \"{record_type}\" is not registered"
            )),
            debug,
        );
    }
    let params = query.into_inner();
    let company_id = params
        .filters
        .get("company_id")
        .cloned()
        .unwrap_or_default();
    let items = state.effective_definitions(&record_type, &company_id);
    let items: Vec<FieldDefinition> = items
        .into_iter()
        .filter(|definition| {
            matches_search(
                &format!("{} {}", definition.field_key, definition.label),
                &params,
            )
        })
        .collect();
    json::success_response(paginate(items, &params), 200)
}

// ---------------------------------------------------------------------------
// Handlers: values.
// ---------------------------------------------------------------------------

async fn list_values(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_VIEW) {
        return json::error_response(&error, debug);
    }
    let params = query.into_inner();
    let record_filter = params.filters.get("record_type").cloned();
    let record_id_filter = params.filters.get("record_id").cloned();
    let company_filter = params.filters.get("company_id").cloned();
    let key_filter = params.filters.get("field_key").cloned();
    let mut items: Vec<FieldValue> = state
        .values
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|value| {
            matches_search(
                &format!("{} {} {}", value.record_type, value.record_id, value.field_key),
                &params,
            ) && record_filter
                .as_ref()
                .is_none_or(|record_type| value.record_type == *record_type)
                && record_id_filter
                    .as_ref()
                    .is_none_or(|record_id| value.record_id == *record_id)
                && company_filter
                    .as_ref()
                    .is_none_or(|company_id| value.company_id == *company_id)
                && key_filter
                    .as_ref()
                    .is_none_or(|field_key| value.field_key == *field_key)
        })
        .collect();
    items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    json::success_response(paginate(items, &params), 200)
}

async fn set_value(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    body: web::Json<SetFieldValue>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let call = match guard(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        Ok(call) => call,
        Err(error) => return json::error_response(&error, debug),
    };
    let input = body.into_inner();
    let result: UnifiedResult<FieldValue> = (|| {
        let record_type = require_record_type(&state, &input.record_type)?;
        let company_id = normalize_company_id(&input.company_id);
        let record_id = require_text(&input.record_id, "record_id", MAX_RECORD_ID_LEN)?;
        let field_key = validate_field_key(&input.field_key)?;
        let effective = state.effective_definitions(&record_type, &company_id);
        let definition = effective
            .iter()
            .find(|definition| definition.field_key == field_key)
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "No active field definition \"{field_key}\" for record type \"{record_type}\""
                ))
            })?;
        validate_value(definition, Some(&input.value))?;
        let existing_id = state
            .values
            .iter()
            .find(|entry| {
                let value = entry.value();
                value.record_type == record_type
                    && value.record_id == record_id
                    && value.field_key == field_key
                    && value.company_id == company_id
            })
            .map(|entry| entry.key().clone());
        let now = Utc::now();
        let stored = match existing_id {
            Some(id) => {
                let mut row = state.values.get_mut(&id).expect("value row vanished");
                row.value = input.value.clone();
                row.value_type = definition.field_type.clone();
                row.definition_id = Some(definition.id.clone());
                row.updated_at = now;
                (*row).clone()
            }
            None => {
                let row = FieldValue {
                    id: Uuid::new_v4().to_string(),
                    company_id: company_id.clone(),
                    record_type: record_type.clone(),
                    record_id: record_id.clone(),
                    field_key: field_key.clone(),
                    definition_id: Some(definition.id.clone()),
                    value_type: definition.field_type.clone(),
                    value: input.value.clone(),
                    created_by: call.actor.clone(),
                    created_at: now,
                    updated_at: now,
                };
                state.values.insert(row.id.clone(), row.clone());
                row
            }
        };
        state.emit(
            EVENT_VALUES_CHANGED,
            json!({
                "record_type": record_type,
                "record_id": record_id,
                "field_key": field_key,
                "company_id": company_id,
                "action": "set",
            }),
        );
        Ok(stored)
    })();
    match result {
        Ok(value) => json::success_response(value, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_value(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<FieldValue> = state
        .values
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Field value not found"));
    json::respond(result, debug)
}

async fn delete_value(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let removed = state
            .values
            .remove(path.as_str())
            .map(|(_, value)| value)
            .ok_or_else(|| UnifiedError::not_found("Field value not found"))?;
        state.emit(
            EVENT_VALUES_CHANGED,
            json!({
                "record_type": removed.record_type,
                "record_id": removed.record_id,
                "field_key": removed.field_key,
                "company_id": removed.company_id,
                "action": "deleted",
            }),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, debug)
}

async fn validate_value_handler(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    body: web::Json<ValidateFieldValue>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_VIEW) {
        return json::error_response(&error, debug);
    }
    let input = body.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let record_type = require_record_type(&state, &input.record_type)?;
        let company_id = normalize_company_id(&input.company_id);
        let field_key = validate_field_key(&input.field_key)?;
        let effective = state.effective_definitions(&record_type, &company_id);
        let definition = effective
            .iter()
            .find(|definition| definition.field_key == field_key)
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "No active field definition \"{field_key}\" for record type \"{record_type}\""
                ))
            })?;
        validate_value(definition, Some(&input.value))?;
        Ok(json!({
            "valid": true,
            "field_key": field_key,
            "record_type": record_type,
            "field_type": definition.field_type,
        }))
    })();
    json::respond(result, debug)
}

async fn resolved_custom_fields(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_VIEW) {
        return json::error_response(&error, debug);
    }
    let (record_type, record_id) = path.into_inner();
    if !state.record_types.contains_key(&record_type) {
        return json::error_response(
            &UnifiedError::not_found(format!(
                "Record type \"{record_type}\" is not registered"
            )),
            debug,
        );
    }
    let params = query.into_inner();
    let company_id = params
        .filters
        .get("company_id")
        .cloned()
        .unwrap_or_default();
    let definitions = state.effective_definitions(&record_type, &company_id);
    let mut items: Vec<ResolvedCustomField> = definitions
        .into_iter()
        .map(|definition| {
            let stored = state.values.iter().find(|entry| {
                let value = entry.value();
                value.record_type == record_type
                    && value.record_id == record_id
                    && value.field_key == definition.field_key
                    && value.company_id == company_id
            });
            match stored {
                Some(entry) => ResolvedCustomField {
                    definition,
                    value: Some(entry.value().value.clone()),
                    value_id: Some(entry.value().id.clone()),
                },
                None => ResolvedCustomField {
                    definition,
                    value: None,
                    value_id: None,
                },
            }
        })
        .filter(|resolved| {
            matches_search(
                &format!("{} {}", resolved.definition.field_key, resolved.definition.label),
                &params,
            )
        })
        .collect();
    items.sort_by(|a, b| a.definition.field_key.cmp(&b.definition.field_key));
    json::success_response(paginate(items, &params), 200)
}

// ---------------------------------------------------------------------------
// Handlers: list views.
// ---------------------------------------------------------------------------

async fn list_views(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let call = match guard(&ctx, &req, PERM_STUDIO_VIEW) {
        Ok(call) => call,
        Err(error) => return json::error_response(&error, debug),
    };
    let params = query.into_inner();
    let record_filter = params.filters.get("record_type").cloned();
    let company_filter = params.filters.get("company_id").cloned();
    let visibility_filter = params.filters.get("visibility").cloned();
    let mut items: Vec<ListView> = state
        .views
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|view| {
            matches_search(&format!("{} {}", view.name, view.record_type), &params)
                && record_filter
                    .as_ref()
                    .is_none_or(|record_type| view.record_type == *record_type)
                && company_filter
                    .as_ref()
                    .is_none_or(|company_id| view.company_id == *company_id)
                && visibility_filter
                    .as_ref()
                    .is_none_or(|visibility| view.visibility == *visibility)
                && (view.user_id == call.actor || view.visibility == VISIBILITY_SHARED)
        })
        .collect();
    items.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then(b.updated_at.cmp(&a.updated_at))
    });
    json::success_response(paginate(items, &params), 200)
}

async fn create_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    body: web::Json<CreateListView>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let call = match guard(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        Ok(call) => call,
        Err(error) => return json::error_response(&error, debug),
    };
    let input = body.into_inner();
    let result: UnifiedResult<ListView> = (|| {
        let record_type = require_record_type(&state, &input.record_type)?;
        let company_id = normalize_company_id(&input.company_id);
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let columns = validate_columns(&input.columns.unwrap_or_default())?;
        let filters = input.filters.unwrap_or_else(|| json!({}));
        validate_filter(&filters)?;
        let sort_field = match &input.sort_field {
            Some(field) => Some(validate_sort_field(field)?),
            None => None,
        };
        let sort_order = match &input.sort_order {
            Some(order) => Some(validate_order(order)?),
            None => None,
        };
        let group_by = match &input.group_by {
            Some(field) => Some(validate_sort_field(field)?),
            None => None,
        };
        let visibility = match &input.visibility {
            Some(visibility) => validate_visibility(visibility)?.to_string(),
            None => VISIBILITY_PRIVATE.to_string(),
        };
        // A shared view belongs to the company; a private view
        // belongs to the user. The scope drives default clearing.
        let scope = if visibility == VISIBILITY_SHARED {
            "company".to_string()
        } else {
            "user".to_string()
        };
        let now = Utc::now();
        let view = ListView {
            id: Uuid::new_v4().to_string(),
            user_id: call.actor.clone(),
            company_id: company_id.clone(),
            record_type: record_type.clone(),
            name,
            columns,
            filters,
            sort_field,
            sort_order,
            group_by,
            visibility,
            scope,
            is_default: input.is_default.unwrap_or(false),
            created_at: now,
            updated_at: now,
        };
        state.views.insert(view.id.clone(), view.clone());
        if view.is_default {
            state.clear_other_view_defaults(&view);
        }
        state.emit(
            EVENT_VIEWS_CHANGED,
            json!({
                "view_id": view.id,
                "record_type": record_type,
                "company_id": company_id,
                "action": "created",
            }),
        );
        Ok(view)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let call = match guard(&ctx, &req, PERM_STUDIO_VIEW) {
        Ok(call) => call,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<ListView> = (|| {
        let view = state
            .views
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("List view not found"))?;
        if view.user_id != call.actor && view.visibility != VISIBILITY_SHARED {
            return Err(UnifiedError::forbidden(
                "This list view is private to another user",
            ));
        }
        Ok(view)
    })();
    json::respond(result, debug)
}

async fn update_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateListView>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let call = match guard(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        Ok(call) => call,
        Err(error) => return json::error_response(&error, debug),
    };
    let input = body.into_inner();
    let result: UnifiedResult<ListView> = (|| {
        let mut view = state
            .views
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("List view not found"))?;
        if view.user_id != call.actor {
            return Err(UnifiedError::forbidden(
                "Only the owner of a list view may change it",
            ));
        }
        if let Some(name) = &input.name {
            view.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(columns) = &input.columns {
            view.columns = validate_columns(columns)?;
        }
        if let Some(filters) = &input.filters {
            validate_filter(filters)?;
            view.filters = filters.clone();
        }
        if let Some(sort_field) = &input.sort_field {
            view.sort_field = Some(validate_sort_field(sort_field)?);
        }
        if let Some(sort_order) = &input.sort_order {
            view.sort_order = Some(validate_order(sort_order)?);
        }
        if let Some(group_by) = &input.group_by {
            view.group_by = Some(validate_sort_field(group_by)?);
        }
        if let Some(visibility) = &input.visibility {
            view.visibility = validate_visibility(visibility)?.to_string();
            view.scope = if view.visibility == VISIBILITY_SHARED {
                "company".to_string()
            } else {
                "user".to_string()
            };
        }
        let make_default = input.is_default.unwrap_or(false);
        if let Some(is_default) = input.is_default {
            view.is_default = is_default;
        }
        view.updated_at = Utc::now();
        state.views.insert(view.id.clone(), view.clone());
        if make_default {
            state.clear_other_view_defaults(&view);
        }
        state.emit(
            EVENT_VIEWS_CHANGED,
            json!({
                "view_id": view.id,
                "record_type": view.record_type,
                "company_id": view.company_id,
                "action": "updated",
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

async fn delete_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let call = match guard(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        Ok(call) => call,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<Value> = (|| {
        let view = state
            .views
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("List view not found"))?;
        if view.user_id != call.actor {
            return Err(UnifiedError::forbidden(
                "Only the owner of a list view may delete it",
            ));
        }
        state.views.remove(path.as_str());
        state.emit(
            EVENT_VIEWS_CHANGED,
            json!({
                "view_id": view.id,
                "record_type": view.record_type,
                "company_id": view.company_id,
                "action": "deleted",
            }),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, debug)
}

async fn set_default_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let call = match guard(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        Ok(call) => call,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<ListView> = (|| {
        let mut view = state
            .views
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("List view not found"))?;
        if view.user_id != call.actor {
            return Err(UnifiedError::forbidden(
                "Only the owner of a list view may make it the default",
            ));
        }
        view.is_default = true;
        view.updated_at = Utc::now();
        state.views.insert(view.id.clone(), view.clone());
        state.clear_other_view_defaults(&view);
        state.emit(
            EVENT_VIEWS_CHANGED,
            json!({
                "view_id": view.id,
                "record_type": view.record_type,
                "company_id": view.company_id,
                "action": "default_changed",
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: form layouts.
// ---------------------------------------------------------------------------

async fn list_layouts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_VIEW) {
        return json::error_response(&error, debug);
    }
    let params = query.into_inner();
    let record_filter = params.filters.get("record_type").cloned();
    let company_filter = params.filters.get("company_id").cloned();
    let mut items: Vec<FormLayout> = state
        .layouts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|layout| {
            matches_search(&format!("{} {}", layout.name, layout.record_type), &params)
                && record_filter
                    .as_ref()
                    .is_none_or(|record_type| layout.record_type == *record_type)
                && company_filter
                    .as_ref()
                    .is_none_or(|company_id| layout.company_id == *company_id)
        })
        .collect();
    items.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then(b.updated_at.cmp(&a.updated_at))
    });
    json::success_response(paginate(items, &params), 200)
}

async fn create_layout(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    body: web::Json<CreateFormLayout>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        return json::error_response(&error, debug);
    }
    let input = body.into_inner();
    let result: UnifiedResult<FormLayout> = (|| {
        let record_type = require_record_type(&state, &input.record_type)?;
        let company_id = normalize_company_id(&input.company_id);
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        validate_layout(&state, &record_type, &company_id, &input.sections)?;
        let now = Utc::now();
        let layout = FormLayout {
            id: Uuid::new_v4().to_string(),
            company_id: company_id.clone(),
            record_type: record_type.clone(),
            name,
            sections: input.sections,
            is_default: input.is_default.unwrap_or(false),
            created_at: now,
            updated_at: now,
        };
        state.layouts.insert(layout.id.clone(), layout.clone());
        if layout.is_default {
            state.clear_other_layout_defaults(&layout);
        }
        state.emit(
            EVENT_LAYOUTS_CHANGED,
            json!({
                "layout_id": layout.id,
                "record_type": record_type,
                "company_id": company_id,
                "action": "created",
            }),
        );
        Ok(layout)
    })();
    match result {
        Ok(layout) => json::success_response(layout, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_layout(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<FormLayout> = state
        .layouts
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Form layout not found"));
    json::respond(result, debug)
}

async fn update_layout(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateFormLayout>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        return json::error_response(&error, debug);
    }
    let input = body.into_inner();
    let result: UnifiedResult<FormLayout> = (|| {
        let mut layout = state
            .layouts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Form layout not found"))?;
        if let Some(name) = &input.name {
            layout.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(sections) = &input.sections {
            validate_layout(&state, &layout.record_type, &layout.company_id, sections)?;
            layout.sections = sections.clone();
        }
        let make_default = input.is_default.unwrap_or(false);
        if let Some(is_default) = input.is_default {
            layout.is_default = is_default;
        }
        layout.updated_at = Utc::now();
        state.layouts.insert(layout.id.clone(), layout.clone());
        if make_default {
            state.clear_other_layout_defaults(&layout);
        }
        state.emit(
            EVENT_LAYOUTS_CHANGED,
            json!({
                "layout_id": layout.id,
                "record_type": layout.record_type,
                "company_id": layout.company_id,
                "action": "updated",
            }),
        );
        Ok(layout)
    })();
    json::respond(result, debug)
}

async fn delete_layout(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let layout = state
            .layouts
            .remove(path.as_str())
            .map(|(_, layout)| layout)
            .ok_or_else(|| UnifiedError::not_found("Form layout not found"))?;
        state.emit(
            EVENT_LAYOUTS_CHANGED,
            json!({
                "layout_id": layout.id,
                "record_type": layout.record_type,
                "company_id": layout.company_id,
                "action": "deleted",
            }),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, debug)
}

async fn get_default_layout(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = guard(&ctx, &req, PERM_STUDIO_VIEW) {
        return json::error_response(&error, debug);
    }
    let record_type = path.into_inner();
    if !state.record_types.contains_key(&record_type) {
        return json::error_response(
            &UnifiedError::not_found(format!(
                "Record type \"{record_type}\" is not registered"
            )),
            debug,
        );
    }
    let params = query.into_inner();
    let company_id = params
        .filters
        .get("company_id")
        .cloned()
        .unwrap_or_default();
    let result: UnifiedResult<FormLayout> = state
        .default_layout(&record_type, &company_id)
        .ok_or_else(|| {
            UnifiedError::not_found("No default layout for this record type and company")
        });
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Module.
// ---------------------------------------------------------------------------

pub struct StudioModule {
    state: StudioState,
}

impl StudioModule {
    pub fn new() -> Self {
        Self {
            state: StudioState::new(),
        }
    }
}

impl Default for StudioModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for StudioModule {
    fn id(&self) -> &'static str {
        "studio"
    }

    fn name(&self) -> &'static str {
        "Studio"
    }

    fn description(&self) -> &'static str {
        "User-side customizer: metadata-driven custom fields, validation rules, saved list views, and form layouts for any registered record type. No code generation and no runtime schema changes."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "company"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = Arc::new(self.state.clone());
        bus.register(
            HOOK_FIELDS_REQUESTED,
            "studio",
            Arc::new(move |ctx: &mut HookContext| {
                on_fields_requested(Arc::clone(&state), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            HOOK_LAYOUT_REQUESTED,
            "studio",
            Arc::new(move |ctx: &mut HookContext| {
                on_layout_requested(Arc::clone(&state), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            HOOK_VIEWS_REQUESTED,
            "studio",
            Arc::new(move |ctx: &mut HookContext| {
                on_views_requested(Arc::clone(&state), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            HOOK_RECORD_TYPE_REGISTER,
            "studio",
            Arc::new(move |ctx: &mut HookContext| {
                on_record_type_register(Arc::clone(&state), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/studio")
                .app_data(state)
                .route("/field-types", web::get().to(list_field_types))
                .route("/record-types", web::get().to(list_record_types))
                .route("/record-types", web::post().to(create_record_type))
                .route("/record-types/{record_type}", web::get().to(get_record_type))
                .route(
                    "/record-types/{record_type}",
                    web::delete().to(delete_record_type),
                )
                .route(
                    "/record-types/{record_type}/system-fields",
                    web::get().to(list_system_fields),
                )
                .route(
                    "/record-types/{record_type}/system-fields",
                    web::post().to(create_system_field),
                )
                .route("/fields", web::get().to(list_definitions))
                .route("/fields", web::post().to(create_definition))
                .route(
                    "/fields/record-type/{record_type}",
                    web::get().to(list_effective_definitions),
                )
                .route("/fields/{id}", web::get().to(get_definition))
                .route("/fields/{id}", web::put().to(update_definition))
                .route("/fields/{id}", web::delete().to(delete_definition))
                .route(
                    "/fields/{id}/archive",
                    web::post().to(archive_definition),
                )
                .route("/values", web::get().to(list_values))
                .route("/values", web::post().to(set_value))
                .route("/values/validate", web::post().to(validate_value_handler))
                .route("/values/{id}", web::get().to(get_value))
                .route("/values/{id}", web::delete().to(delete_value))
                .route(
                    "/records/{record_type}/{record_id}/custom-fields",
                    web::get().to(resolved_custom_fields),
                )
                .route("/views", web::get().to(list_views))
                .route("/views", web::post().to(create_view))
                .route("/views/{id}", web::get().to(get_view))
                .route("/views/{id}", web::put().to(update_view))
                .route("/views/{id}", web::delete().to(delete_view))
                .route("/views/{id}/set-default", web::post().to(set_default_view))
                .route("/layouts", web::get().to(list_layouts))
                .route("/layouts", web::post().to(create_layout))
                .route(
                    "/layouts/record-type/{record_type}",
                    web::get().to(get_default_layout),
                )
                .route("/layouts/{id}", web::get().to(get_layout))
                .route("/layouts/{id}", web::put().to(update_layout))
                .route("/layouts/{id}", web::delete().to(delete_layout)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/studio/field-types", owner: "studio", description: "List the seven field types and what each supports." },
            RouteInfo { method: "GET", path: "/api/v1/studio/record-types", owner: "studio", description: "List registered record types (paginated, filter by module_id)." },
            RouteInfo { method: "POST", path: "/api/v1/studio/record-types", owner: "studio", description: "Register a record type (admin only), optionally with system fields." },
            RouteInfo { method: "GET", path: "/api/v1/studio/record-types/{record_type}", owner: "studio", description: "Get one registered record type." },
            RouteInfo { method: "DELETE", path: "/api/v1/studio/record-types/{record_type}", owner: "studio", description: "Remove a record type with no definitions, values, views, or layouts (admin only)." },
            RouteInfo { method: "GET", path: "/api/v1/studio/record-types/{record_type}/system-fields", owner: "studio", description: "List the system fields of a record type (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/studio/record-types/{record_type}/system-fields", owner: "studio", description: "Register a system field for a record type (admin only)." },
            RouteInfo { method: "GET", path: "/api/v1/studio/fields", owner: "studio", description: "List field definitions (paginated, filter by record_type, company_id, status, field_type)." },
            RouteInfo { method: "POST", path: "/api/v1/studio/fields", owner: "studio", description: "Create a field definition (version 1)." },
            RouteInfo { method: "GET", path: "/api/v1/studio/fields/record-type/{record_type}", owner: "studio", description: "Effective definitions for a record type and company (highest active version per field key)." },
            RouteInfo { method: "GET", path: "/api/v1/studio/fields/{id}", owner: "studio", description: "Get one field definition version." },
            RouteInfo { method: "PUT", path: "/api/v1/studio/fields/{id}", owner: "studio", description: "Update a definition by creating a new version; the old version is superseded, never mutated." },
            RouteInfo { method: "DELETE", path: "/api/v1/studio/fields/{id}", owner: "studio", description: "Archive a field definition (soft delete; values keep working)." },
            RouteInfo { method: "POST", path: "/api/v1/studio/fields/{id}/archive", owner: "studio", description: "Archive a field definition." },
            RouteInfo { method: "GET", path: "/api/v1/studio/values", owner: "studio", description: "List stored custom values (paginated, filter by record_type, record_id, company_id, field_key)." },
            RouteInfo { method: "POST", path: "/api/v1/studio/values", owner: "studio", description: "Set (upsert) one custom value, validated against its definition." },
            RouteInfo { method: "POST", path: "/api/v1/studio/values/validate", owner: "studio", description: "Validate a value against a definition without saving it." },
            RouteInfo { method: "GET", path: "/api/v1/studio/values/{id}", owner: "studio", description: "Get one stored custom value." },
            RouteInfo { method: "DELETE", path: "/api/v1/studio/values/{id}", owner: "studio", description: "Delete one stored custom value." },
            RouteInfo { method: "GET", path: "/api/v1/studio/records/{record_type}/{record_id}/custom-fields", owner: "studio", description: "Effective definitions for one record merged with its stored values." },
            RouteInfo { method: "GET", path: "/api/v1/studio/views", owner: "studio", description: "List list views visible to the caller (paginated, filter by record_type, company_id, visibility)." },
            RouteInfo { method: "POST", path: "/api/v1/studio/views", owner: "studio", description: "Create a saved list view (private or shared, optionally default)." },
            RouteInfo { method: "GET", path: "/api/v1/studio/views/{id}", owner: "studio", description: "Get one list view (owner or shared only)." },
            RouteInfo { method: "PUT", path: "/api/v1/studio/views/{id}", owner: "studio", description: "Update a list view (owner only)." },
            RouteInfo { method: "DELETE", path: "/api/v1/studio/views/{id}", owner: "studio", description: "Delete a list view (owner only)." },
            RouteInfo { method: "POST", path: "/api/v1/studio/views/{id}/set-default", owner: "studio", description: "Make a list view the default for its scope (owner only)." },
            RouteInfo { method: "GET", path: "/api/v1/studio/layouts", owner: "studio", description: "List form layouts (paginated, filter by record_type, company_id)." },
            RouteInfo { method: "POST", path: "/api/v1/studio/layouts", owner: "studio", description: "Create a form layout (system-required / non-hideable fields can never be hidden)." },
            RouteInfo { method: "GET", path: "/api/v1/studio/layouts/record-type/{record_type}", owner: "studio", description: "The default form layout for a record type and company (company default, else global default)." },
            RouteInfo { method: "GET", path: "/api/v1/studio/layouts/{id}", owner: "studio", description: "Get one form layout." },
            RouteInfo { method: "PUT", path: "/api/v1/studio/layouts/{id}", owner: "studio", description: "Update a form layout." },
            RouteInfo { method: "DELETE", path: "/api/v1/studio/layouts/{id}", owner: "studio", description: "Delete a form layout." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(StudioModule::new()));
}
