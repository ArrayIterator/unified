// Studio - In-memory studio state.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use chrono::Utc;
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::{CreateRecordType, CreateSystemField, DEFAULT_RECORD_TYPES, DEFAULT_SYSTEM_FIELDS,
    EVENT_RECORD_TYPE_REGISTERED, FieldDefinition, FieldValue, FormLayout, ListView, MAX_LABEL_LEN,
    RecordType, STATUS_ACTIVE, SystemField, VISIBILITY_SHARED, require_text, validate_field_type,
    validate_module_id, validate_record_type, validate_system_field_key};

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

    pub(crate) fn emit(&self, event: &str, payload: Value) {
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

    pub(crate) fn system_fields_of(&self, record_type: &str) -> Vec<SystemField> {
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

    pub(crate) fn clear_other_view_defaults(&self, view: &ListView) {
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

    pub(crate) fn clear_other_layout_defaults(&self, layout: &FormLayout) {
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

    pub(crate) fn register_record_type(
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

    pub(crate) fn add_system_field(
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
