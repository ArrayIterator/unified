//! Studio module - Studio module - user-side customizer (metadata-driven only).
//!
//! Split into submodules by concern; the public API is
//! re-exported below. The full route map and module notes
//! live in `studio/module.rs`.

pub mod ai_capabilities;
pub mod types;
pub mod state;
mod auth;
mod validation;
mod service;
mod events;
mod handlers_fields;
mod handlers_values;
mod handlers_views;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::{CreateFieldDefinition, CreateFormLayout, CreateListView, CreateRecordType,
    CreateSystemField, FIELD_TYPES, FIELD_TYPE_BOOLEAN, FIELD_TYPE_DATE, FIELD_TYPE_DECIMAL,
    FIELD_TYPE_NUMBER, FIELD_TYPE_SELECT, FIELD_TYPE_USER_REFERENCE, FieldDefinition, FieldTypeInfo,
    FieldValidation, FieldValue, FormLayout, LayoutEntry, LayoutSection, ListView, RecordType,
    ResolvedCustomField, STATUS_ARCHIVED, STATUS_SUPERSEDED, SetFieldValue, SystemField,
    UpdateFieldDefinition, UpdateFormLayout, UpdateListView, VISIBILITY_PRIVATE, VISIBILITY_SHARED,
    ValidateFieldValue};
pub(crate) use self::types::{DEFAULT_RECORD_TYPES, DEFAULT_SYSTEM_FIELDS, EVENT_FIELDS_CHANGED,
    EVENT_LAYOUTS_CHANGED, EVENT_RECORD_TYPE_REGISTERED, EVENT_VALUES_CHANGED, EVENT_VIEWS_CHANGED,
    FIELD_KEY_PREFIX, FIELD_TYPE_TEXT, GLOBAL_COMPANY_ID, HOOK_FIELDS_REQUESTED,
    HOOK_LAYOUT_REQUESTED, HOOK_RECORD_TYPE_REGISTER, HOOK_VIEWS_REQUESTED, MAX_COLUMNS,
    MAX_HELP_LEN, MAX_LABEL_LEN, MAX_LAYOUT_ENTRIES, MAX_NAME_LEN, MAX_OPTIONS, MAX_OPTION_LEN,
    MAX_PATTERN_LEN, MAX_RECORD_ID_LEN, MAX_RECORD_TYPE_LEN, MAX_SECTIONS, MAX_USER_ID_LEN,
    PERM_STUDIO_ADMIN, PERM_STUDIO_CUSTOMIZE, PERM_STUDIO_VIEW, STATUS_ACTIVE};
pub use self::state::{StudioState};
pub(crate) use self::auth::{Caller, caller, check_capability, guard, require_permission,
    resolve_tenant};
pub(crate) use self::validation::{matches_search, normalize_company_id, optional_text, paginate,
    require_record_type, require_text, validate_columns, validate_field_key, validate_field_type,
    validate_filter, validate_layout, validate_module_id, validate_options, validate_order,
    validate_record_type, validate_rules, validate_sort_field, validate_system_field_key,
    validate_value, validate_visibility};
pub(crate) use self::service::{field_type_infos, validate_new_definition};
pub(crate) use self::events::{on_fields_requested, on_layout_requested, on_record_type_register,
    on_views_requested};
pub(crate) use self::handlers_fields::{archive_definition, create_definition, create_record_type,
    create_system_field, delete_definition, delete_record_type, get_definition, get_record_type,
    list_definitions, list_effective_definitions, list_field_types, list_record_types,
    list_system_fields, update_definition};
pub(crate) use self::handlers_values::{delete_value, get_value, list_values, resolved_custom_fields,
    set_value, validate_value_handler};
pub(crate) use self::handlers_views::{create_layout, create_view, delete_layout, delete_view,
    get_default_layout, get_layout, get_view, list_layouts, list_views, set_default_view,
    update_layout, update_view};
pub use self::module::{StudioModule, register};
