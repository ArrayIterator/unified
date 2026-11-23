// Saved Views - Saved-view validation.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use chrono::Utc;
use serde_json::Value;
use uuid::Uuid;

use super::{CreateSavedView, SavedView, VISIBILITY_PRIVATE, VISIBILITY_SHARED};

// ---------------------------------------------------------------------------
// Validation.
// ---------------------------------------------------------------------------

pub(crate) fn validate_name(name: &str) -> UnifiedResult<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request("name must not be empty"));
    }
    if trimmed.chars().count() > 100 {
        return Err(UnifiedError::bad_request(
            "name must be at most 100 characters",
        ));
    }
    Ok(())
}

/// The module id must belong to a registered module.
pub(crate) fn validate_module_id(ctx: &AppContext, module_id: &str) -> UnifiedResult<String> {
    let module_id = module_id.trim().to_string();
    if module_id.is_empty() {
        return Err(UnifiedError::bad_request("module_id must not be empty"));
    }
    if ctx.modules.get(&module_id).is_none() {
        return Err(UnifiedError::not_found(format!(
            "Module \"{module_id}\" is not registered"
        ))
        .with_metadata("module_id", Value::String(module_id.clone())));
    }
    Ok(module_id)
}

/// A filter value is a scalar, or an array of scalars only. Nested
/// objects and nested arrays are rejected.
pub(crate) fn validate_filter_value(value: &Value) -> UnifiedResult<()> {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => Ok(()),
        Value::Array(items) => {
            for item in items {
                match item {
                    Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
                    _ => {
                        return Err(UnifiedError::bad_request(
                            "filter arrays must contain scalar values only (no nested objects or arrays)",
                        ));
                    }
                }
            }
            Ok(())
        }
        Value::Object(_) => Err(UnifiedError::bad_request(
            "filter values must be scalars or arrays of scalars (nested objects are not allowed)",
        )),
    }
}

/// The filter must be an object with string keys and scalar/array
/// values only.
pub(crate) fn validate_filter(filter: &Value) -> UnifiedResult<()> {
    let object = match filter {
        Value::Object(map) => map,
        _ => {
            return Err(UnifiedError::bad_request(
                "filter must be a JSON object",
            ));
        }
    };
    if object.is_empty() {
        return Err(UnifiedError::bad_request("filter must not be empty"));
    }
    for (key, value) in object {
        if key.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "filter keys must not be empty",
            ));
        }
        validate_filter_value(value)?;
    }
    Ok(())
}

pub(crate) fn validate_visibility(visibility: &str) -> UnifiedResult<&'static str> {
    match visibility.trim() {
        VISIBILITY_PRIVATE => Ok(VISIBILITY_PRIVATE),
        VISIBILITY_SHARED => Ok(VISIBILITY_SHARED),
        _ => Err(UnifiedError::bad_request(
            "visibility must be \"private\" or \"shared\"",
        )),
    }
}

pub(crate) fn validate_order(order: &str) -> UnifiedResult<String> {
    match order.trim().to_lowercase().as_str() {
        "asc" => Ok("asc".to_string()),
        "desc" => Ok("desc".to_string()),
        _ => Err(UnifiedError::bad_request(
            "order must be \"asc\" or \"desc\"",
        )),
    }
}

pub(crate) fn validate_sort(sort: &str) -> UnifiedResult<Option<String>> {
    let sort = sort.trim();
    if sort.is_empty() {
        return Ok(None);
    }
    if sort.len() > 64 || !sort.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
        return Err(UnifiedError::bad_request(
            "sort must be a plain field name (letters, digits, underscore, dot; at most 64 characters)",
        ));
    }
    Ok(Some(sort.to_string()))
}

pub(crate) fn validate_new(ctx: &AppContext, input: CreateSavedView) -> UnifiedResult<SavedView> {
    if input.user_id.trim().is_empty() {
        return Err(UnifiedError::bad_request("user_id must not be empty"));
    }
    let module_id = validate_module_id(ctx, &input.module_id)?;
    validate_name(&input.name)?;
    validate_filter(&input.filter)?;
    let sort = match input.sort {
        Some(sort) => validate_sort(&sort)?,
        None => None,
    };
    let order = match input.order {
        Some(order) => Some(validate_order(&order)?),
        None => None,
    };
    let visibility = match input.visibility {
        Some(visibility) => validate_visibility(&visibility)?.to_string(),
        None => VISIBILITY_PRIVATE.to_string(),
    };
    let now = Utc::now();
    Ok(SavedView {
        id: Uuid::new_v4().to_string(),
        user_id: input.user_id.trim().to_string(),
        module_id,
        name: input.name.trim().to_string(),
        filter: input.filter,
        sort,
        order,
        visibility,
        is_default: input.is_default.unwrap_or(false),
        created_at: now,
        updated_at: now,
    })
}


#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::context::AppContext;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use crate::modules::saved_views::SavedViewsModule;
    use serde_json::json;
    use std::sync::Arc;

    fn registered_context() -> Arc<AppContext> {
        let ctx = Arc::new(AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        ));
        ctx.modules
            .register(Arc::new(SavedViewsModule::new()))
            .expect("register saved_views module");
        ctx
    }

    fn new_input() -> CreateSavedView {
        CreateSavedView {
            user_id: "user-1".to_string(),
            module_id: "saved_views".to_string(),
            name: "Open tickets".to_string(),
            filter: json!({"status": "open"}),
            sort: Some("created_at".to_string()),
            order: Some("asc".to_string()),
            visibility: Some("private".to_string()),
            is_default: Some(false),
        }
    }

    #[test]
    fn validate_name_accepts_normal_name() {
        assert!(validate_name("My view").is_ok());
    }

    #[test]
    fn validate_name_rejects_empty_and_too_long() {
        assert!(validate_name("   ").is_err());
        assert!(validate_name(&"x".repeat(101)).is_err());
    }

    #[test]
    fn validate_filter_value_accepts_scalars_and_arrays() {
        assert!(validate_filter_value(&json!("open")).is_ok());
        assert!(validate_filter_value(&json!(42)).is_ok());
        assert!(validate_filter_value(&json!(["a", "b"])).is_ok());
        assert!(validate_filter_value(&json!({"nested": true})).is_err());
    }

    #[test]
    fn validate_filter_requires_non_empty_object() {
        assert!(validate_filter(&json!({"status": "open"})).is_ok());
        assert!(validate_filter(&json!({})).is_err());
        assert!(validate_filter(&json!([1])).is_err());
    }

    #[test]
    fn validate_visibility_accepts_private_and_shared() {
        assert_eq!(validate_visibility("private").unwrap(), "private");
        assert_eq!(validate_visibility("shared").unwrap(), "shared");
        assert!(validate_visibility("public").is_err());
    }

    #[test]
    fn validate_order_accepts_asc_desc_case_insensitive() {
        assert_eq!(validate_order("ASC").unwrap(), "asc");
        assert_eq!(validate_order("desc").unwrap(), "desc");
        assert!(validate_order("random").is_err());
    }

    #[test]
    fn validate_sort_accepts_dotted_field() {
        assert_eq!(
            validate_sort("fields.created_at").unwrap(),
            Some("fields.created_at".to_string())
        );
        assert!(validate_sort("created at").is_err());
    }

    #[test]
    fn validate_new_accepts_valid_input() {
        let ctx = registered_context();
        let validated = validate_new(&ctx, new_input()).unwrap();
        assert_eq!(validated.name, "Open tickets");
        assert_eq!(validated.user_id, "user-1");
        assert_eq!(validated.module_id, "saved_views");
        assert_eq!(validated.sort.as_deref(), Some("created_at"));
        assert_eq!(validated.order.as_deref(), Some("asc"));
    }

    #[test]
    fn validate_new_rejects_unknown_module() {
        let ctx = registered_context();
        let mut input = new_input();
        input.module_id = "no-such-module".to_string();
        assert!(validate_new(&ctx, input).is_err());
    }

    #[test]
    fn validate_new_rejects_bad_filter() {
        let ctx = registered_context();
        let mut input = new_input();
        input.filter = json!({});
        assert!(validate_new(&ctx, input).is_err());
    }

    #[test]
    fn validate_new_rejects_bad_order() {
        let ctx = registered_context();
        let mut input = new_input();
        input.order = Some("sideways".to_string());
        assert!(validate_new(&ctx, input).is_err());
    }
}
