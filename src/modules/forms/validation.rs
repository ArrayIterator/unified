// forms — validation.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::store::*;
use super::common::*;
use super::submission::*;
use super::module::*;
use super::handlers_forms::*;
use super::handlers_fields::*;
use super::handlers_submissions::*;

/// A slug is lowercase letters, digits, and single hyphens only,
/// never leading or trailing with a hyphen, at most 128 chars.
pub(crate) fn valid_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > MAX_SLUG_LEN {
        return false;
    }
    if slug.starts_with('-') || slug.ends_with('-') || slug.contains("--") {
        return false;
    }
    slug.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub(crate) fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for ch in title.to_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("form");
    }
    out.truncate(MAX_SLUG_LEN);
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// A field key is lowercase letters, digits, and underscores only,
/// never leading with a digit, at most 64 chars.
pub(crate) fn valid_key(key: &str) -> bool {
    if key.is_empty() || key.len() > MAX_KEY_LEN {
        return false;
    }
    if key.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return false;
    }
    key.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

pub(crate) fn valid_email(email: &str) -> bool {
    if email.is_empty() || email.len() > 254 || email.contains(' ') {
        return false;
    }
    let mut parts = email.split('@');
    let (Some(local), Some(domain)) = (parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() || local.is_empty() || domain.is_empty() {
        return false;
    }
    domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// Best-effort client IP: the first `X-Forwarded-For` entry, else
/// the peer address. Used only for rate limiting and submission
/// records, never for authentication.
pub(crate) fn client_ip(req: &HttpRequest) -> Option<String> {
    req.headers()
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            req.connection_info()
                .realip_remote_addr()
                .map(str::to_string)
        })
}

pub(crate) fn slug_in_use(state: &FormsState, slug: &str, except: Option<Uuid>) -> bool {
    state
        .forms
        .iter()
        .any(|entry| entry.value().slug == slug && Some(entry.value().id) != except)
}

pub(crate) fn load_form(state: &FormsState, raw_id: &str) -> UnifiedResult<Form> {
    let id = parse_uuid(raw_id, "form")?;
    state
        .forms
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Form not found: {id}")))
}

pub(crate) fn load_form_by_slug(state: &FormsState, slug: &str) -> UnifiedResult<Form> {
    state
        .forms
        .iter()
        .map(|entry| entry.value().clone())
        .find(|form| form.slug == slug)
        .ok_or_else(|| UnifiedError::not_found(format!("Form not found: {slug}")))
}

pub(crate) fn fields_for(state: &FormsState, form_id: Uuid) -> Vec<FormField> {
    let mut items: Vec<FormField> = state
        .fields
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|field| field.form_id == form_id)
        .collect();
    items.sort_by_key(|field| (field.position, field.created_at));
    items
}

pub(crate) fn load_field(state: &FormsState, form_id: Uuid, raw_id: &str) -> UnifiedResult<FormField> {
    let id = parse_uuid(raw_id, "field")?;
    state
        .fields
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|field| field.form_id == form_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Field not found: {id}")))
}

pub(crate) fn key_in_use(state: &FormsState, form_id: Uuid, key: &str, except: Option<Uuid>) -> bool {
    state.fields.iter().any(|entry| {
        entry.value().form_id == form_id
            && entry.value().key == key
            && Some(entry.value().id) != except
    })
}

pub(crate) fn validate_mapping(mapping: &MappingRequest) -> UnifiedResult<FormMapping> {
    if !MAPPING_TARGETS.contains(&mapping.target.as_str()) {
        return Err(UnifiedError::bad_request(format!(
            "Mapping target must be one of: {}",
            MAPPING_TARGETS.join(", ")
        )));
    }
    let mut field_map: HashMap<String, String> = HashMap::new();
    for (from, to) in &mapping.field_map {
        let from = require_text(from, "mapping.field_map key", MAX_KEY_LEN)?;
        let to = require_text(to, "mapping.field_map value", MAX_KEY_LEN)?;
        if !valid_key(&from) || !valid_key(&to) {
            return Err(UnifiedError::bad_request(
                "mapping.field_map keys and values must be field keys ([a-z0-9_]+, max 64 characters)",
            ));
        }
        field_map.insert(from, to);
    }
    let title_template = match &mapping.title_template {
        Some(raw) => Some(require_text(raw, "mapping.title_template", MAX_TITLE_TEMPLATE_LEN)?),
        None => None,
    };
    Ok(FormMapping {
        target: mapping.target.clone(),
        field_map,
        title_template,
    })
}

/// Validate one field definition before it is stored. `known_keys`
/// holds the keys of the form's other fields (for `show_when`
/// references); a condition may only reference a key in that set.
/// Rules are type-scoped: options only for choice fields, length
/// bounds and patterns only for text-like fields, value bounds
/// only for number fields.
pub(crate) fn validate_field_definition(
    input: &CreateFieldRequest,
    known_keys: &[String],
) -> UnifiedResult<()> {
    if !valid_key(&input.key) {
        return Err(UnifiedError::bad_request(
            "field key must be lowercase letters, digits, and underscores only, never leading with a digit, max 64 characters",
        ));
    }
    if input.label.trim().is_empty() || input.label.len() > MAX_LABEL_LEN {
        return Err(UnifiedError::bad_request(
            "field label must not be empty and must be at most 200 characters",
        ));
    }
    if !FIELD_TYPES.contains(&input.field_type.as_str()) {
        return Err(UnifiedError::bad_request(format!(
            "Unknown field type \"{}\" (expected one of: {})",
            input.field_type,
            FIELD_TYPES.join(", ")
        )));
    }
    if let Some(position) = input.position {
        if position < 0 {
            return Err(UnifiedError::bad_request("field position must be >= 0"));
        }
    }
    if let Some(placeholder) = &input.placeholder {
        if placeholder.len() > MAX_PLACEHOLDER_LEN {
            return Err(UnifiedError::bad_request(
                "field placeholder must be at most 200 characters",
            ));
        }
    }
    if let Some(help) = &input.help_text {
        if help.len() > MAX_HELP_LEN {
            return Err(UnifiedError::bad_request(
                "field help_text must be at most 500 characters",
            ));
        }
    }
    let is_choice = matches!(input.field_type.as_str(), "select" | "multi-select" | "radio");
    let is_text_like = matches!(
        input.field_type.as_str(),
        "text" | "textarea" | "email"
    );
    if is_choice {
        if input.options.is_empty() {
            return Err(UnifiedError::bad_request(format!(
                "field type \"{}\" requires at least one option",
                input.field_type
            )));
        }
        if input.options.len() > MAX_OPTIONS {
            return Err(UnifiedError::bad_request(format!(
                "at most {MAX_OPTIONS} options are allowed per field"
            )));
        }
        let mut seen: Vec<&str> = Vec::new();
        for option in &input.options {
            if option.trim().is_empty() || option.len() > MAX_OPTION_LEN {
                return Err(UnifiedError::bad_request(
                    "field options must not be empty and must be at most 128 characters",
                ));
            }
            if seen.contains(&option.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "Duplicate field option \"{option}\""
                )));
            }
            seen.push(option.as_str());
        }
    } else if !input.options.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "field type \"{}\" does not take options",
            input.field_type
        )));
    }
    if !is_text_like {
        if input.min_length.is_some() || input.max_length.is_some() {
            return Err(UnifiedError::bad_request(format!(
                "field type \"{}\" does not take min_length / max_length",
                input.field_type
            )));
        }
        if input.pattern.is_some() {
            return Err(UnifiedError::bad_request(format!(
                "field type \"{}\" does not take a regex pattern",
                input.field_type
            )));
        }
    }
    // Each bound is validated on its own: a lone bound must already
    // be usable, since a single-sided bound is stored and enforced
    // at submit time (a lone NaN / negative bound used to pass
    // silently and then never fire).
    if let Some(min) = input.min_length {
        if min < 0 {
            return Err(UnifiedError::bad_request(
                "min_length must be >= 0",
            ));
        }
    }
    if let Some(max) = input.max_length {
        if max < 0 {
            return Err(UnifiedError::bad_request(
                "max_length must be >= 0",
            ));
        }
    }
    if let (Some(min), Some(max)) = (input.min_length, input.max_length) {
        if max < min {
            return Err(UnifiedError::bad_request(
                "max_length must be >= min_length",
            ));
        }
    }
    if input.field_type != "number" {
        if input.min_value.is_some() || input.max_value.is_some() {
            return Err(UnifiedError::bad_request(
                "only number fields take min_value / max_value",
            ));
        }
    }
    if let Some(min) = input.min_value {
        if !min.is_finite() {
            return Err(UnifiedError::bad_request(
                "min_value must be finite",
            ));
        }
    }
    if let Some(max) = input.max_value {
        if !max.is_finite() {
            return Err(UnifiedError::bad_request(
                "max_value must be finite",
            ));
        }
    }
    if let (Some(min), Some(max)) = (input.min_value, input.max_value) {
        if max < min {
            return Err(UnifiedError::bad_request(
                "max_value must be >= min_value",
            ));
        }
    }
    if let Some(pattern) = &input.pattern {
        if pattern.len() > MAX_PATTERN_LEN {
            return Err(UnifiedError::bad_request(
                "field pattern must be at most 500 characters",
            ));
        }
        if let Err(error) = Regex::new(pattern) {
            return Err(UnifiedError::bad_request(format!(
                "field pattern is not a valid regex: {error}"
            )));
        }
    }
    if let Some(condition) = &input.show_when {
        if condition.field_key == input.key {
            return Err(UnifiedError::bad_request(
                "show_when must reference a different field",
            ));
        }
        if !known_keys.iter().any(|key| key == &condition.field_key) {
            return Err(UnifiedError::bad_request(format!(
                "show_when references unknown field \"{}\"",
                condition.field_key
            )));
        }
        if condition.equals.len() > MAX_OPTION_LEN {
            return Err(UnifiedError::bad_request(
                "show_when.equals must be at most 128 characters",
            ));
        }
    }
    Ok(())
}

pub(crate) fn build_field(state: &FormsState, form_id: Uuid, input: &CreateFieldRequest, now: DateTime<Utc>) -> FormField {
    let position = match input.position {
        Some(position) => position,
        None => fields_for(state, form_id)
            .iter()
            .map(|field| field.position)
            .max()
            .unwrap_or(-1)
            + 1,
    };
    FormField {
        id: Uuid::now_v7(),
        form_id,
        key: input.key.trim().to_string(),
        label: input.label.trim().to_string(),
        field_type: input.field_type.clone(),
        position,
        required: input.required,
        placeholder: input.placeholder.clone(),
        help_text: input.help_text.clone(),
        options: input.options.clone(),
        min_length: input.min_length,
        max_length: input.max_length,
        min_value: input.min_value,
        max_value: input.max_value,
        pattern: input.pattern.clone(),
        show_when: input.show_when.clone(),
        created_at: now,
        updated_at: now,
    }
}

/// Shift every field at or after `position` one step down, so an
/// insert at an explicit position keeps positions unique.
pub(crate) fn shift_fields_for_insert(state: &FormsState, form_id: Uuid, position: i64) {
    for mut entry in state.fields.iter_mut() {
        if entry.value().form_id == form_id && entry.value().position >= position {
            entry.value_mut().position += 1;
            entry.value_mut().updated_at = Utc::now();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_lowercases_and_collapses_separators() {
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("  --Hi--  "), "hi");
        assert_eq!(slugify("a  b"), "a-b");
        assert_eq!(slugify(""), "form");
        assert_eq!(slugify("!!!"), "form");
        assert_eq!(slugify("Contact Us 2026"), "contact-us-2026");
    }

    #[test]
    fn slugs_accept_only_lowercase_dash_words() {
        assert!(valid_slug("contact-us"));
        assert!(valid_slug("form-1"));
        assert!(!valid_slug("Contact"));
        assert!(!valid_slug("contact--us"));
        assert!(!valid_slug("-contact"));
        assert!(!valid_slug("contact-"));
        assert!(!valid_slug(""));
        assert!(!valid_slug("contact us"));
    }

    #[test]
    fn field_keys_must_start_with_a_letter() {
        assert!(valid_key("email"));
        assert!(valid_key("full_name_2"));
        assert!(!valid_key("2email"));
        assert!(!valid_key("Email"));
        assert!(!valid_key("full-name"));
        assert!(!valid_key(""));
    }

    #[test]
    fn emails_need_a_dotted_domain() {
        assert!(valid_email("ada@example.com"));
        assert!(!valid_email("ada@example"));
        assert!(!valid_email("ada@.example.com"));
        assert!(!valid_email("ada@example.com."));
        assert!(!valid_email("ada@@example.com"));
        assert!(!valid_email("ada example@example.com"));
        assert!(!valid_email(""));
    }

    fn field_request(field_type: &str) -> CreateFieldRequest {
        CreateFieldRequest {
            key: "field_one".to_string(),
            label: "Field One".to_string(),
            field_type: field_type.to_string(),
            position: None,
            required: false,
            placeholder: None,
            help_text: None,
            options: Vec::new(),
            min_length: None,
            max_length: None,
            min_value: None,
            max_value: None,
            pattern: None,
            show_when: None,
        }
    }

    #[test]
    fn single_sided_bounds_are_validated_on_their_own() {
        // A lone negative length bound must be rejected, not stored.
        let mut field = field_request("text");
        field.min_length = Some(-5);
        assert!(validate_field_definition(&field, &[]).is_err());
        let mut field = field_request("text");
        field.max_length = Some(-1);
        assert!(validate_field_definition(&field, &[]).is_err());
        // A lone non-finite value bound must be rejected: a stored
        // NaN bound never fires at submit time (number < NaN is
        // false), so the field would silently have no bound.
        let mut field = field_request("number");
        field.min_value = Some(f64::NAN);
        assert!(validate_field_definition(&field, &[]).is_err());
        let mut field = field_request("number");
        field.max_value = Some(f64::INFINITY);
        assert!(validate_field_definition(&field, &[]).is_err());
        // Lone valid bounds still pass, as does a valid pair.
        let mut field = field_request("text");
        field.min_length = Some(2);
        assert!(validate_field_definition(&field, &[]).is_ok());
        let mut field = field_request("number");
        field.max_value = Some(10.0);
        assert!(validate_field_definition(&field, &[]).is_ok());
        let mut field = field_request("text");
        field.min_length = Some(1);
        field.max_length = Some(5);
        assert!(validate_field_definition(&field, &[]).is_ok());
        let mut field = field_request("text");
        field.min_length = Some(5);
        field.max_length = Some(1);
        assert!(validate_field_definition(&field, &[]).is_err());
    }
}
