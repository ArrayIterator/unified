// forms - submission.

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
use super::validation::*;
use super::module::*;
use super::handlers_forms::*;
use super::handlers_fields::*;
use super::handlers_submissions::*;

// Submission validation

/// Render a submitted JSON value as a plain string for equality
/// checks (conditional visibility) and CSV export.
pub(crate) fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// True when the field is visible given the submitted values:
/// no condition means visible; otherwise the referenced field's
/// value must equal the condition's `equals` string. For an array
/// value (multi-select) the field is visible when any selected
/// element equals the condition string.
pub(crate) fn field_visible(field: &FormField, values: &Map<String, Value>) -> bool {
    let Some(condition) = &field.show_when else {
        return true;
    };
    match values.get(&condition.field_key) {
        Some(Value::Array(items)) => items
            .iter()
            .any(|item| value_to_string(item) == condition.equals),
        Some(value) => value_to_string(value) == condition.equals,
        None => false,
    }
}

pub(crate) fn is_empty_value(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

/// Validate submitted values against the form's field definitions
/// and return the stored values (visible fields only, in field
/// order). Hidden fields are skipped entirely: not validated, not
/// stored. Unknown keys are rejected.
pub(crate) fn validate_submission_values(
    fields: &[FormField],
    values: &Map<String, Value>,
    honeypot_field: &str,
) -> UnifiedResult<Vec<(FormField, Value)>> {
    for key in values.keys() {
        if key == honeypot_field {
            continue;
        }
        if !fields.iter().any(|field| &field.key == key) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown form field \"{key}\""
            )));
        }
    }
    let mut stored: Vec<(FormField, Value)> = Vec::new();
    for field in fields {
        if !field_visible(field, values) {
            continue;
        }
        let value = values.get(&field.key);
        match value {
            None | Some(Value::Null) => {
                if field.required {
                    return Err(UnifiedError::bad_request(format!(
                        "Field \"{}\" is required",
                        field.label
                    )));
                }
            }
            Some(value) => {
                if field.required && is_empty_value(value) {
                    return Err(UnifiedError::bad_request(format!(
                        "Field \"{}\" is required",
                        field.label
                    )));
                }
                if !is_empty_value(value) {
                    validate_field_value(field, value)?;
                }
                stored.push((field.clone(), value.clone()));
            }
        }
    }
    Ok(stored)
}

/// Validate one submitted value against its field's rules.
pub(crate) fn validate_field_value(field: &FormField, value: &Value) -> UnifiedResult<()> {
    let label = &field.label;
    match field.field_type.as_str() {
        "text" | "textarea" => {
            let text = value.as_str().ok_or_else(|| {
                UnifiedError::bad_request(format!("Field \"{label}\" must be text"))
            })?;
            check_text_rules(field, text)?;
        }
        "email" => {
            let text = value.as_str().ok_or_else(|| {
                UnifiedError::bad_request(format!("Field \"{label}\" must be text"))
            })?;
            if !valid_email(text) {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be a valid email address"
                )));
            }
            check_text_rules(field, text)?;
        }
        "number" => {
            let number = value.as_f64().or_else(|| {
                value
                    .as_str()
                    .and_then(|text| text.trim().parse::<f64>().ok())
            });
            let Some(number) = number else {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be a number"
                )));
            };
            if !number.is_finite() {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be a finite number"
                )));
            }
            if let Some(min) = field.min_value {
                if number < min {
                    return Err(UnifiedError::bad_request(format!(
                        "Field \"{label}\" must be >= {min}"
                    )));
                }
            }
            if let Some(max) = field.max_value {
                if number > max {
                    return Err(UnifiedError::bad_request(format!(
                        "Field \"{label}\" must be <= {max}"
                    )));
                }
            }
        }
        "select" | "radio" => {
            let text = value.as_str().ok_or_else(|| {
                UnifiedError::bad_request(format!("Field \"{label}\" must be text"))
            })?;
            if !field.options.iter().any(|option| option == text) {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be one of: {}",
                    field.options.join(", ")
                )));
            }
        }
        "multi-select" => {
            let items = value.as_array().ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be a list of options"
                ))
            })?;
            for item in items {
                let text = item.as_str().ok_or_else(|| {
                    UnifiedError::bad_request(format!(
                        "Field \"{label}\" options must be text"
                    ))
                })?;
                if !field.options.iter().any(|option| option == text) {
                    return Err(UnifiedError::bad_request(format!(
                        "Field \"{label}\" must only contain: {}",
                        field.options.join(", ")
                    )));
                }
            }
        }
        "checkbox" => {
            if !value.is_boolean() {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be true or false"
                )));
            }
        }
        "date" => {
            let text = value.as_str().ok_or_else(|| {
                UnifiedError::bad_request(format!("Field \"{label}\" must be text"))
            })?;
            let parsed = DateTime::parse_from_rfc3339(text).is_ok()
                || NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok();
            if !parsed {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be a date (YYYY-MM-DD or RFC 3339)"
                )));
            }
        }
        "file" => {
            let key = value.as_str().ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be a storage key reference"
                ))
            })?;
            if key.trim().is_empty()
                || key.len() > MAX_STORAGE_KEY_LEN
                || key.chars().any(char::is_whitespace)
            {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be a storage key reference (at most 512 characters, no whitespace)"
                )));
            }
        }
        "rating" => {
            // Whole-number JSON floats (4.0) are valid 1-5 ratings:
            // accept any finite number with no fractional part.
            let number = value.as_f64().or_else(|| {
                value
                    .as_str()
                    .and_then(|text| text.trim().parse::<f64>().ok())
            });
            let Some(number) =
                number.filter(|number| number.is_finite() && number.fract() == 0.0)
            else {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be a whole number from 1 to 5"
                )));
            };
            if !(1.0..=5.0).contains(&number) {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be a whole number from 1 to 5"
                )));
            }
        }
        "consent" => {
            let accepted = value.as_bool().unwrap_or(false);
            if field.required && !accepted {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be accepted"
                )));
            }
            if !value.is_boolean() {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be true or false"
                )));
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn check_text_rules(field: &FormField, text: &str) -> UnifiedResult<()> {
    let label = &field.label;
    if let Some(min) = field.min_length {
        if (text.chars().count() as i64) < min {
            return Err(UnifiedError::bad_request(format!(
                "Field \"{label}\" must be at least {min} characters"
            )));
        }
    }
    if let Some(max) = field.max_length {
        if (text.chars().count() as i64) > max {
            return Err(UnifiedError::bad_request(format!(
                "Field \"{label}\" must be at most {max} characters"
            )));
        }
    }
    if let Some(pattern) = &field.pattern {
        // A stored pattern that no longer compiles is an internal
        // error, never a silently skipped rule: fail closed so a
        // bad pattern can never disable the check unnoticed.
        let regex = Regex::new(pattern).map_err(|error| {
            UnifiedError::internal(format!(
                "Field \"{label}\" has an invalid stored regex pattern: {error}"
            ))
        })?;
        if !regex.is_match(text) {
            return Err(UnifiedError::bad_request(format!(
                "Field \"{label}\" does not match the required format"
            )));
        }
    }
    Ok(())
}

// Rate limiting + mapping

/// Enforce the submit rate limits and record the attempt: at most
/// 10 submissions per IP per form per rolling hour, and at most
/// 200 submissions per form per rolling hour. Stale timestamps
/// (older than one hour) are pruned on every call.
pub(crate) fn check_rate_limit(state: &FormsState, form_id: Uuid, ip: &str) -> UnifiedResult<()> {
    let now = Utc::now();
    let hour_ago = now - chrono::Duration::hours(1);
    let ip_key = format!("ip:{form_id}:{ip}");
    let form_key = format!("form:{form_id}");
    let mut ip_attempts = state
        .attempts
        .get(&ip_key)
        .map(|entry| entry.value().clone())
        .unwrap_or_default();
    ip_attempts.retain(|instant| *instant > hour_ago);
    if ip_attempts.len() >= MAX_SUBMISSIONS_PER_IP_PER_HOUR {
        return Err(UnifiedError::new(
            "Too many submissions from this address for this form; try again later",
            429,
        ));
    }
    let mut form_attempts = state
        .attempts
        .get(&form_key)
        .map(|entry| entry.value().clone())
        .unwrap_or_default();
    form_attempts.retain(|instant| *instant > hour_ago);
    if form_attempts.len() >= MAX_SUBMISSIONS_PER_FORM_PER_HOUR {
        return Err(UnifiedError::new(
            "This form has reached its hourly submission limit; try again later",
            429,
        ));
    }
    ip_attempts.push(now);
    form_attempts.push(now);
    state.attempts.insert(ip_key, ip_attempts);
    state.attempts.insert(form_key, form_attempts);
    Ok(())
}

/// Fill a title template's `{field_key}` placeholders from the
/// stored values, then trim. Unknown keys stay untouched.
pub(crate) fn fill_title_template(template: &str, values: &[(FormField, Value)]) -> String {
    let mut out = template.to_string();
    for (field, value) in values {
        let placeholder = format!("{{{}}}", field.key);
        out = out.replace(&placeholder, &value_to_string(value));
    }
    out.trim().to_string()
}

/// Run the form's mapping for one submission: emit
/// `forms.mapping.requested` and read the handler's answer. Returns
/// (mapping_status, mapped_target_id, mapping_error). A missing
/// bus, an empty answer, or an explicit failure are all recorded
/// as `failed` with an error message - never silent.
pub(crate) fn try_mapping(
    state: &FormsState,
    form: &Form,
    submission: &FormSubmission,
    stored: &[(FormField, Value)],
) -> (String, Option<String>, Option<String>) {
    let Some(mapping) = &form.mapping else {
        return ("none".to_string(), None, None);
    };
    let Some(bus) = state.bus() else {
        return (
            "failed".to_string(),
            None,
            Some("mapping bus is unavailable".to_string()),
        );
    };
    let mut fields: Map<String, Value> = Map::new();
    for (field, value) in stored {
        if let Some(target_field) = mapping.field_map.get(&field.key) {
            fields.insert(target_field.clone(), value.clone());
        }
    }
    let title = mapping
        .title_template
        .as_deref()
        .map(|template| fill_title_template(template, stored))
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| format!("Form submission: {}", form.title));
    let answer = bus.emit(
        "forms.mapping.requested",
        json!({
            "target": mapping.target,
            "form_id": form.id.to_string(),
            "submission_id": submission.id.to_string(),
            "title": title,
            "fields": fields,
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("created").and_then(Value::as_bool) == Some(true) => {
            let target_id = result
                .get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
                .or_else(|| {
                    result
                        .get("target_id")
                        .and_then(|value| value.as_str())
                        .map(str::to_string)
                });
            ("mapped".to_string(), target_id, None)
        }
        Some(result) => {
            let error = result
                .get("error")
                .and_then(|value| value.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| "mapping handler declined the submission".to_string());
            ("failed".to_string(), None, Some(error))
        }
        None => (
            "failed".to_string(),
            None,
            Some(format!(
                "no mapping handler registered for target \"{}\"",
                mapping.target
            )),
        ),
    }
}

/// Apply a mapping outcome to the stored submission and emit
/// `forms.submission.mapped` on success.
pub(crate) fn apply_mapping_outcome(
    state: &FormsState,
    form: &Form,
    submission: &mut FormSubmission,
    outcome: (String, Option<String>, Option<String>),
) {
    let (status, target_id, error) = outcome;
    submission.mapping_status = status.clone();
    submission.mapped_target_id = target_id.clone();
    submission.mapping_error = error;
    submission.updated_at = Utc::now();
    state.submissions.insert(submission.id, submission.clone());
    if status == "mapped" {
        state.emit(
            "forms.submission.mapped",
            json!({
                "form_id": form.id.to_string(),
                "submission_id": submission.id.to_string(),
                "target": form.mapping.as_ref().map(|mapping| mapping.target.clone()),
                "target_id": target_id,
            }),
        );
    }
}

pub(crate) fn submission_view(state: &FormsState, submission: &FormSubmission) -> Value {
    let mut values: Vec<FormSubmissionValue> = state
        .values
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|value| value.submission_id == submission.id)
        .collect();
    values.sort_by_key(|value| value.created_at);
    let mut view = serde_json::to_value(submission).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut view {
        map.insert(
            "values".to_string(),
            serde_json::to_value(values).unwrap_or(Value::Null),
        );
    }
    view
}

pub(crate) fn escape_csv_field(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn field(key: &str) -> FormField {
        FormField {
            id: Uuid::new_v4(),
            form_id: Uuid::new_v4(),
            key: key.to_string(),
            label: key.to_string(),
            field_type: "text".to_string(),
            position: 0,
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
            created_at: Utc.with_ymd_and_hms(2026, 11, 22, 12, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 11, 22, 12, 0, 0).unwrap(),
        }
    }

    #[test]
    fn csv_fields_quote_only_when_needed() {
        assert_eq!(escape_csv_field("plain"), "plain");
        assert_eq!(escape_csv_field("a,b"), "\"a,b\"");
        assert_eq!(escape_csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(escape_csv_field("line1\nline2"), "\"line1\nline2\"");
    }

    #[test]
    fn empty_values_cover_null_blank_text_and_empty_lists() {
        assert!(is_empty_value(&Value::Null));
        assert!(is_empty_value(&json!("   ")));
        assert!(is_empty_value(&json!([])));
        assert!(!is_empty_value(&json!("x")));
        assert!(!is_empty_value(&json!(0)));
        assert!(!is_empty_value(&json!(false)));
    }

    #[test]
    fn values_convert_to_plain_text() {
        assert_eq!(value_to_string(&json!("hello")), "hello");
        assert_eq!(value_to_string(&json!(42)), "42");
        assert_eq!(value_to_string(&json!(true)), "true");
        assert_eq!(value_to_string(&Value::Null), "");
        assert_eq!(value_to_string(&json!([1, 2])), "[1,2]");
    }

    #[test]
    fn conditional_fields_follow_their_show_when_rule() {
        let mut shown = field("details");
        shown.show_when = Some(FieldCondition {
            field_key: "kind".to_string(),
            equals: "other".to_string(),
        });
        let mut values = Map::new();
        assert!(!field_visible(&shown, &values));
        values.insert("kind".to_string(), json!("other"));
        assert!(field_visible(&shown, &values));
        values.insert("kind".to_string(), json!("standard"));
        assert!(!field_visible(&shown, &values));
        assert!(field_visible(&field("plain"), &values));
    }

    #[test]
    fn title_templates_replace_placeholders_and_trim() {
        let name = field("name");
        let out = fill_title_template("  Hello {name}, welcome  ", &[(name, json!("Ada"))]);
        assert_eq!(out, "Hello Ada, welcome");
        let missing = fill_title_template("Ticket {missing}", &[]);
        assert_eq!(missing, "Ticket {missing}");
    }

    #[test]
    fn whole_number_float_ratings_are_accepted() {
        let mut rating = field("rating");
        rating.field_type = "rating".to_string();
        rating.label = "Rating".to_string();
        assert!(validate_field_value(&rating, &json!(4.0)).is_ok());
        assert!(validate_field_value(&rating, &json!(4)).is_ok());
        assert!(validate_field_value(&rating, &json!("4.0")).is_ok());
        assert!(validate_field_value(&rating, &json!(5.0)).is_ok());
        assert!(validate_field_value(&rating, &json!(4.5)).is_err());
        assert!(validate_field_value(&rating, &json!(0.0)).is_err());
        assert!(validate_field_value(&rating, &json!(6.0)).is_err());
        assert!(validate_field_value(&rating, &json!("high")).is_err());
    }

    #[test]
    fn show_when_matches_any_selected_multi_select_option() {
        let mut shown = field("details");
        shown.show_when = Some(FieldCondition {
            field_key: "topics".to_string(),
            equals: "other".to_string(),
        });
        let mut values = Map::new();
        values.insert("topics".to_string(), json!(["news", "other"]));
        assert!(field_visible(&shown, &values));
        values.insert("topics".to_string(), json!(["news", "events"]));
        assert!(!field_visible(&shown, &values));
        values.insert("topics".to_string(), json!([]));
        assert!(!field_visible(&shown, &values));
    }

    #[test]
    fn invalid_stored_pattern_fails_closed() {
        let mut broken = field("code");
        broken.pattern = Some("[".to_string());
        let error = check_text_rules(&broken, "abc").unwrap_err();
        assert_eq!(error.code, 500);
        let mut good = field("code");
        good.pattern = Some("^a+$".to_string());
        assert!(check_text_rules(&good, "aaa").is_ok());
        assert!(check_text_rules(&good, "bbb").is_err());
    }
}
