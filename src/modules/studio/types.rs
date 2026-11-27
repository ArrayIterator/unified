// Studio - Custom-field, record-type, view and layout types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// Constants and configurable defaults.

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
pub(crate) const MAX_LABEL_LEN: usize = 100;

pub(crate) const MAX_HELP_LEN: usize = 500;

pub(crate) const MAX_NAME_LEN: usize = 100;

pub(crate) const MAX_RECORD_TYPE_LEN: usize = 128;

pub(crate) const MAX_RECORD_ID_LEN: usize = 200;

pub(crate) const MAX_USER_ID_LEN: usize = 200;

pub(crate) const MAX_PATTERN_LEN: usize = 500;

pub(crate) const MAX_OPTIONS: usize = 100;

pub(crate) const MAX_OPTION_LEN: usize = 100;

pub(crate) const MAX_COLUMNS: usize = 50;

pub(crate) const MAX_LAYOUT_ENTRIES: usize = 200;

pub(crate) const MAX_SECTIONS: usize = 20;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_STUDIO_VIEW: &str = "studio.view";

pub(crate) const PERM_STUDIO_CUSTOMIZE: &str = "studio.customize";

pub(crate) const PERM_STUDIO_ADMIN: &str = "studio.admin";

/// Hook event names.
pub(crate) const HOOK_FIELDS_REQUESTED: &str = "studio.fields.requested";

pub(crate) const HOOK_LAYOUT_REQUESTED: &str = "studio.layout.requested";

pub(crate) const HOOK_VIEWS_REQUESTED: &str = "studio.views.requested";

pub(crate) const HOOK_RECORD_TYPE_REGISTER: &str = "studio.record_type.register";

pub(crate) const EVENT_FIELDS_CHANGED: &str = "studio.fields.changed";

pub(crate) const EVENT_VALUES_CHANGED: &str = "studio.values.changed";

pub(crate) const EVENT_VIEWS_CHANGED: &str = "studio.views.changed";

pub(crate) const EVENT_LAYOUTS_CHANGED: &str = "studio.layouts.changed";

pub(crate) const EVENT_RECORD_TYPE_REGISTERED: &str = "studio.record_type.registered";

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

// Types.

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
