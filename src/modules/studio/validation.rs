// Studio - Field, value, filter and layout validation.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use chrono::NaiveDate;
use regex::Regex;
use serde::Serialize;
use serde_json::Value;

use super::{FIELD_KEY_PREFIX, FIELD_TYPES, FIELD_TYPE_BOOLEAN, FIELD_TYPE_DATE, FIELD_TYPE_DECIMAL,
    FIELD_TYPE_NUMBER, FIELD_TYPE_SELECT, FIELD_TYPE_TEXT, FIELD_TYPE_USER_REFERENCE,
    FieldDefinition, FieldValidation, GLOBAL_COMPANY_ID, LayoutSection, MAX_COLUMNS, MAX_LABEL_LEN,
    MAX_LAYOUT_ENTRIES, MAX_OPTIONS, MAX_OPTION_LEN, MAX_PATTERN_LEN, MAX_RECORD_TYPE_LEN,
    MAX_SECTIONS, MAX_USER_ID_LEN, StudioState, VISIBILITY_PRIVATE, VISIBILITY_SHARED};

// Validation.

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
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

pub(crate) fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
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

pub(crate) fn normalize_company_id(value: &Option<String>) -> String {
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
pub(crate) fn validate_record_type(raw: &str) -> UnifiedResult<String> {
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

pub(crate) fn validate_module_id(raw: &str) -> UnifiedResult<String> {
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
pub(crate) fn validate_field_key(raw: &str) -> UnifiedResult<String> {
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
pub(crate) fn validate_system_field_key(raw: &str) -> UnifiedResult<String> {
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

pub(crate) fn validate_field_type(raw: &str) -> UnifiedResult<String> {
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

pub(crate) fn validate_visibility(raw: &str) -> UnifiedResult<&'static str> {
    match raw.trim() {
        VISIBILITY_PRIVATE => Ok(VISIBILITY_PRIVATE),
        VISIBILITY_SHARED => Ok(VISIBILITY_SHARED),
        _ => Err(UnifiedError::bad_request(
            "visibility must be \"private\" or \"shared\"",
        )),
    }
}

pub(crate) fn validate_order(raw: &str) -> UnifiedResult<String> {
    match raw.trim().to_lowercase().as_str() {
        "asc" => Ok("asc".to_string()),
        "desc" => Ok("desc".to_string()),
        _ => Err(UnifiedError::bad_request("order must be \"asc\" or \"desc\"")),
    }
}

pub(crate) fn validate_sort_field(raw: &str) -> UnifiedResult<String> {
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
/// at definition time, never at use time - including the regex,
/// which is compiled here.
pub(crate) fn validate_rules(field_type: &str, rules: &FieldValidation) -> UnifiedResult<()> {
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

pub(crate) fn validate_options(field_type: &str, options: &[String]) -> UnifiedResult<Vec<String>> {
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
pub(crate) fn validate_value(definition: &FieldDefinition, value: Option<&Value>) -> UnifiedResult<()> {
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
/// nested objects - a filter is data, never executable logic.
pub(crate) fn validate_filter(filter: &Value) -> UnifiedResult<()> {
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

pub(crate) fn validate_columns(columns: &[String]) -> UnifiedResult<Vec<String>> {
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
/// hidden - they are optional by construction.
pub(crate) fn validate_layout(
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

pub(crate) fn require_record_type(state: &StudioState, raw: &str) -> UnifiedResult<String> {
    let record_type = validate_record_type(raw)?;
    if !state.record_types.contains_key(&record_type) {
        return Err(UnifiedError::not_found(format!(
            "Record type \"{record_type}\" is not registered"
        )));
    }
    Ok(record_type)
}

// Helpers.

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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

pub(crate) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::studio::LayoutEntry;
    use chrono::Utc;
    use serde_json::json;

    fn definition(field_type: &str) -> FieldDefinition {
        FieldDefinition {
            id: "def-1".to_string(),
            company_id: "company-1".to_string(),
            record_type: "ticket".to_string(),
            field_key: "x_priority".to_string(),
            label: "Priority".to_string(),
            field_type: field_type.to_string(),
            version: 1,
            status: "active".to_string(),
            required: false,
            default_value: None,
            help_text: None,
            options: Vec::new(),
            validation: FieldValidation::default(),
            created_by: "admin".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn require_text_trims_and_returns() {
        assert_eq!(require_text("  hello  ", "label", 10).unwrap(), "hello");
    }

    #[test]
    fn require_text_rejects_empty_and_too_long() {
        assert!(require_text("   ", "label", 10).is_err());
        assert!(require_text("abcdef", "label", 5).is_err());
    }

    #[test]
    fn optional_text_empty_becomes_none() {
        assert_eq!(optional_text(&Some("   ".to_string()), "note", 10).unwrap(), None);
        assert_eq!(optional_text(&None, "note", 10).unwrap(), None);
        assert_eq!(
            optional_text(&Some(" x ".to_string()), "note", 10).unwrap(),
            Some("x".to_string())
        );
    }

    #[test]
    fn normalize_company_id_trims_and_defaults_to_global() {
        assert_eq!(
            normalize_company_id(&Some("  company-1 ".to_string())),
            "company-1"
        );
        assert_eq!(normalize_company_id(&None), GLOBAL_COMPANY_ID);
        assert_eq!(
            normalize_company_id(&Some("   ".to_string())),
            GLOBAL_COMPANY_ID
        );
    }

    #[test]
    fn validate_record_type_accepts_module_dot_kind() {
        assert_eq!(validate_record_type("crm.lead").unwrap(), "crm.lead");
        assert_eq!(
            validate_record_type("tickets.ticket_2").unwrap(),
            "tickets.ticket_2"
        );
    }

    #[test]
    fn validate_record_type_rejects_uppercase_hyphens_and_missing_dot() {
        assert!(validate_record_type("Ticket").is_err());
        assert!(validate_record_type("ticket").is_err());
        assert!(validate_record_type("ticket-type_1").is_err());
        assert!(validate_record_type("crm..lead").is_err());
    }

    #[test]
    fn validate_module_id_accepts_lowercase_with_underscores() {
        assert_eq!(
            validate_module_id("studio_fields").unwrap(),
            "studio_fields"
        );
    }

    #[test]
    fn validate_module_id_rejects_empty_and_hyphens() {
        assert!(validate_module_id("").is_err());
        assert!(validate_module_id("studio-fields").is_err());
    }

    #[test]
    fn validate_field_key_accepts_and_rejects() {
        assert_eq!(
            validate_field_key("x_priority_level").unwrap(),
            "x_priority_level"
        );
        assert!(validate_field_key("priority_level").is_err());
        assert!(validate_field_key("x_priority-level").is_err());
    }

    #[test]
    fn validate_system_field_key_rejects_custom_prefix() {
        assert_eq!(validate_system_field_key("status").unwrap(), "status");
        assert_eq!(
            validate_system_field_key("sys_status").unwrap(),
            "sys_status"
        );
        assert!(validate_system_field_key("x_status").is_err());
    }

    #[test]
    fn validate_field_type_accepts_known_types() {
        assert_eq!(validate_field_type("text").unwrap(), "text");
        assert_eq!(validate_field_type("user_reference").unwrap(), "user_reference");
        assert!(validate_field_type("rich_text").is_err());
    }

    #[test]
    fn validate_visibility_accepts_private_and_shared() {
        assert_eq!(validate_visibility("private").unwrap(), "private");
        assert_eq!(validate_visibility("shared").unwrap(), "shared");
        assert!(validate_visibility("company").is_err());
        assert!(validate_visibility("public").is_err());
    }

    #[test]
    fn validate_order_accepts_asc_desc_case_insensitive() {
        assert_eq!(validate_order("ASC").unwrap(), "asc");
        assert_eq!(validate_order("desc").unwrap(), "desc");
        assert!(validate_order("sideways").is_err());
    }

    #[test]
    fn validate_sort_field_accepts_dotted() {
        assert_eq!(validate_sort_field("fields.name").unwrap(), "fields.name");
        assert!(validate_sort_field("fields name").is_err());
    }

    #[test]
    fn validate_rules_rejects_bad_pattern_and_bad_range() {
        let mut bad = FieldValidation::default();
        bad.pattern = Some("([bad".to_string());
        assert!(validate_rules("text", &bad).is_err());
        let mut range = FieldValidation::default();
        range.min = Some(5.0);
        range.max = Some(2.0);
        assert!(validate_rules("number", &range).is_err());
    }

    #[test]
    fn validate_rules_allows_pattern_only_for_text() {
        let mut pattern = FieldValidation::default();
        pattern.pattern = Some("^a+$".to_string());
        assert!(validate_rules("text", &pattern).is_ok());
        assert!(validate_rules("number", &pattern).is_err());
    }

    #[test]
    fn validate_options_requires_non_empty_for_select() {
        assert!(validate_options("select", &[]).is_err());
        assert!(validate_options("select", &["a".to_string(), "b".to_string()]).is_ok());
        assert!(validate_options("text", &[]).is_ok());
        assert!(validate_options("text", &["a".to_string()]).is_err());
    }

    #[test]
    fn validate_value_required_missing_is_error() {
        let mut def = definition("text");
        def.required = true;
        assert!(validate_value(&def, None).is_err());
        assert!(validate_value(&def, Some(&json!(""))).is_err());
    }

    #[test]
    fn validate_value_optional_missing_is_ok() {
        let def = definition("text");
        assert!(validate_value(&def, None).is_ok());
    }

    #[test]
    fn validate_value_checks_types() {
        assert!(validate_value(&definition("number"), Some(&json!(3))).is_ok());
        assert!(validate_value(&definition("number"), Some(&json!("three"))).is_err());
        assert!(validate_value(&definition("boolean"), Some(&json!(true))).is_ok());
        assert!(validate_value(&definition("boolean"), Some(&json!("yes"))).is_err());
        assert!(validate_value(&definition("date"), Some(&json!("2026-01-01"))).is_ok());
        assert!(validate_value(&definition("date"), Some(&json!("01/01/2026"))).is_err());
    }

    #[test]
    fn validate_value_select_rejects_unknown_option() {
        let mut def = definition("select");
        def.options = vec!["a".to_string(), "b".to_string()];
        assert!(validate_value(&def, Some(&json!("a"))).is_ok());
        assert!(validate_value(&def, Some(&json!("c"))).is_err());
    }

    #[test]
    fn validate_filter_accepts_flat_scalar_filters() {
        assert!(validate_filter(&json!({"priority": "high"})).is_ok());
        assert!(validate_filter(&json!({"priority": ["a", "b"], "count": 3})).is_ok());
        assert!(validate_filter(&json!({})).is_ok());
    }

    #[test]
    fn validate_filter_rejects_nested_objects_and_non_objects() {
        assert!(validate_filter(&json!({"priority": {"eq": "high"}})).is_err());
        assert!(validate_filter(&json!({"priority": [{"eq": "high"}]})).is_err());
        assert!(validate_filter(&json!("priority")).is_err());
    }

    #[test]
    fn validate_columns_rejects_empty_and_duplicates() {
        assert!(validate_columns(&[]).is_err());
        assert!(validate_columns(&["a".to_string(), "a".to_string()]).is_err());
        assert!(validate_columns(&["a".to_string(), "b".to_string()]).is_ok());
    }

    #[test]
    fn validate_layout_requires_sections() {
        let state = StudioState::new();
        assert!(validate_layout(&state, "crm.lead", "company-1", &[]).is_err());
    }

    #[test]
    fn validate_layout_accepts_known_fields() {
        let state = StudioState::new();
        // crm.lead seeds the system fields "name" (required,
        // non-hideable) and "email" (non-hideable): both must
        // appear visible in the layout.
        let sections = vec![LayoutSection {
            title: "Main".to_string(),
            entries: vec![
                LayoutEntry {
                    field_key: "name".to_string(),
                    visible: true,
                },
                LayoutEntry {
                    field_key: "email".to_string(),
                    visible: true,
                },
            ],
        }];
        assert!(validate_layout(&state, "crm.lead", "company-1", &sections).is_ok());
    }

    #[test]
    fn validate_layout_rejects_unknown_field() {
        let state = StudioState::new();
        let sections = vec![LayoutSection {
            title: "Main".to_string(),
            entries: vec![LayoutEntry {
                field_key: "no_such_field".to_string(),
                visible: true,
            }],
        }];
        assert!(validate_layout(&state, "crm.lead", "company-1", &sections).is_err());
    }

    #[test]
    fn validate_layout_rejects_hidden_required_system_field() {
        let state = StudioState::new();
        let sections = vec![LayoutSection {
            title: "Main".to_string(),
            entries: vec![
                LayoutEntry {
                    field_key: "name".to_string(),
                    visible: false,
                },
                LayoutEntry {
                    field_key: "email".to_string(),
                    visible: true,
                },
            ],
        }];
        assert!(validate_layout(&state, "crm.lead", "company-1", &sections).is_err());
    }

    #[test]
    fn paginate_clamps_and_slices() {
        let params = PaginationParams {
            page: Some(2),
            per_page: Some(2),
            ..Default::default()
        };
        let page = paginate(vec![1, 2, 3, 4, 5], &params);
        assert_eq!(page.items, vec![3, 4]);
        assert_eq!(page.pagination.total_pages, 3);
    }

    #[test]
    fn matches_search_is_case_insensitive() {
        let priority_params = PaginationParams {
            search: Some("priority".to_string()),
            ..Default::default()
        };
        assert!(matches_search("Priority Level", &priority_params));
        let status_params = PaginationParams {
            search: Some("status".to_string()),
            ..Default::default()
        };
        assert!(!matches_search("Priority", &status_params));
        assert!(matches_search("Priority", &PaginationParams::default()));
    }
}
