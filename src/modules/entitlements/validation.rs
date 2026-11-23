// Input validation.

use unified_core::error::{UnifiedError, UnifiedResult};
use std::collections::HashSet;

use super::*;

/// Validate a module id: lowercase letters, digits, underscore,
/// 1..=64 characters. Module ids come from the registry, so an
/// unknown id is a 404, but the shape is validated first so a
/// hostile id can never reach a query or an event payload.
pub(crate) fn validate_module_id(value: &str) -> UnifiedResult<String> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(UnifiedError::bad_request("module_id must not be empty"));
    }
    if value.len() > MAX_MODULE_ID_LEN
        || !value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(UnifiedError::bad_request(
            "module_id must be 1-64 lowercase letters, digits, or underscores",
        ));
    }
    Ok(value)
}

pub(crate) fn validate_company_id(value: &str) -> UnifiedResult<String> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(UnifiedError::bad_request("company_id must not be empty"));
    }
    if value.len() > MAX_ID_LEN {
        return Err(UnifiedError::bad_request(
            "company_id must be at most 128 characters",
        ));
    }
    Ok(value)
}

pub(crate) fn validate_source_kind(value: &str) -> UnifiedResult<String> {
    let value = value.trim().to_string();
    if SOURCE_KINDS.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(UnifiedError::bad_request(format!(
            "source_kind must be one of: {}",
            SOURCE_KINDS.join(", ")
        )))
    }
}

pub(crate) fn validate_source_id(value: &str) -> UnifiedResult<String> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(UnifiedError::bad_request("source_id must not be empty"));
    }
    if value.len() > MAX_SOURCE_ID_LEN {
        return Err(UnifiedError::bad_request(
            "source_id must be at most 256 characters",
        ));
    }
    Ok(value)
}

pub(crate) fn validate_reason(value: &str, required: bool) -> UnifiedResult<Option<String>> {
    let value = value.trim();
    if value.is_empty() {
        if required {
            return Err(UnifiedError::bad_request("reason is required"));
        }
        return Ok(None);
    }
    if value.chars().count() > MAX_REASON_LEN {
        return Err(UnifiedError::bad_request(
            "reason must be at most 1000 characters",
        ));
    }
    Ok(Some(value.to_string()))
}

pub(crate) fn validate_module_id_list(values: &[String]) -> UnifiedResult<Vec<String>> {
    if values.len() > MAX_MODULES_PER_CHANGE {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_MODULES_PER_CHANGE} module ids are allowed in one change"
        )));
    }
    let mut cleaned: Vec<String> = Vec::with_capacity(values.len());
    let mut seen: HashSet<String> = HashSet::new();
    for value in values {
        let module_id = validate_module_id(value)?;
        if seen.insert(module_id.clone()) {
            cleaned.push(module_id);
        }
    }
    Ok(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_module_id_accepts_valid_and_trims() {
        assert_eq!(validate_module_id("billing").unwrap(), "billing");
        assert_eq!(validate_module_id("  billing_2 ").unwrap(), "billing_2");
        assert_eq!(validate_module_id(&"m".repeat(64)).unwrap(), "m".repeat(64));
    }

    #[test]
    fn validate_module_id_rejects_empty_upper_dash_and_too_long() {
        assert!(validate_module_id("").is_err());
        assert!(validate_module_id("   ").is_err());
        assert!(validate_module_id("Billing").is_err());
        assert!(validate_module_id("has-dash").is_err());
        assert!(validate_module_id("has.dot").is_err());
        assert!(validate_module_id(&"m".repeat(65)).is_err());
    }

    #[test]
    fn validate_company_id_enforces_presence_and_128_limit() {
        assert_eq!(validate_company_id("acme").unwrap(), "acme");
        assert!(validate_company_id("").is_err());
        assert_eq!(validate_company_id(&"c".repeat(128)).unwrap(), "c".repeat(128));
        assert!(validate_company_id(&"c".repeat(129)).is_err());
    }

    #[test]
    fn validate_source_kind_accepts_closed_vocabulary_only() {
        for kind in ["license", "subscription", "saas_package", "manual_grant", "owner_entitlement"] {
            assert_eq!(validate_source_kind(kind).unwrap(), kind);
        }
        assert!(validate_source_kind("bogus").is_err());
        assert!(validate_source_kind("").is_err());
        assert!(validate_source_kind("LICENSE").is_err());
    }

    #[test]
    fn validate_source_id_enforces_presence_and_256_limit() {
        assert_eq!(validate_source_id("src-1").unwrap(), "src-1");
        assert!(validate_source_id("").is_err());
        assert!(validate_source_id(&"s".repeat(257)).is_err());
    }

    #[test]
    fn validate_reason_optional_and_required() {
        assert_eq!(validate_reason("", false).unwrap(), None);
        assert!(validate_reason("", true).is_err());
        assert_eq!(validate_reason("  done  ", true).unwrap(), Some("done".to_string()));
        assert_eq!(validate_reason(&"r".repeat(1000), true).unwrap().unwrap().len(), 1000);
        assert!(validate_reason(&"r".repeat(1001), true).is_err());
    }

    #[test]
    fn validate_module_id_list_dedupes_and_enforces_limits() {
        let ids = vec!["a".to_string(), "b".to_string(), "a".to_string()];
        assert_eq!(validate_module_id_list(&ids).unwrap(), vec!["a".to_string(), "b".to_string()]);
        assert!(validate_module_id_list(&["Bad".to_string()]).is_err());
        let many: Vec<String> = (0..501).map(|i| format!("m{i}")).collect();
        assert!(validate_module_id_list(&many).is_err());
    }
}
