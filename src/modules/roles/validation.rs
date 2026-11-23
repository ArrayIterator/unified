//! roles - validation.

use unified_core::error::{UnifiedError, UnifiedResult};
use serde_json::{json, Map, Value};
use super::{store::*, types::*, module::*, capabilities::*, seeds::*, handlers::*};


pub(crate) fn empty_permissions() -> Value {
    Value::Object(Map::new())
}


pub(crate) fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}


/// True when (`resource`, `action`) names a capability this codebase
/// defines, in either naming shape, or uses a wildcard. A pair is
/// looked up as both `resource:action` and `resource.action`, so
/// `{"printing": ["view"]}` and `{"reseller": ["clients.view"]}`
/// both resolve.
fn permission_known(resource: &str, action: &str) -> bool {
    if resource == "*" || action == "*" {
        return true;
    }
    capability_defined(&format!("{resource}:{action}"))
        || capability_defined(&format!("{resource}.{action}"))
}


/// Validate a permissions value: it must be an object mapping resource
/// names to arrays of action strings. A resource/action pair that is
/// not in `KNOWN_CAPABILITIES` is logged as a warning: it can never
/// match a capability check, so granting it is silently useless.
/// Unknown pairs are warned about, not rejected: the seeded roles
/// grant legacy content pairs (`posts`, `pages`, `media`,
/// `comments`, `store`, `payments`) that no module checks through
/// `role.check` yet, and rejecting them would make those roles
/// uneditable.
pub(crate) fn validate_permissions(permissions: &Value) -> UnifiedResult<()> {
    let map = match permissions.as_object() {
        Some(map) => map,
        None => {
            return Err(UnifiedError::bad_request(
                "Permissions must be a JSON object mapping resource names to action lists",
            ))
        }
    };
    for (resource, actions) in map {
        if resource.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Permission resource names must not be empty",
            ));
        }
        let actions = match actions.as_array() {
            Some(actions) => actions,
            None => {
                return Err(UnifiedError::bad_request(format!(
                    "Permissions for resource \"{resource}\" must be an array of actions"
                )))
            }
        };
        for action in actions {
            let action = match action.as_str() {
                Some(action) if !action.trim().is_empty() => action,
                _ => {
                    return Err(UnifiedError::bad_request(format!(
                        "Actions for resource \"{resource}\" must be non-empty strings"
                    )))
                }
            };
            if !permission_known(resource, action) {
                log::warn!(
                    "Role grants unknown permission \"{resource}\" action \"{action}\": not in KNOWN_CAPABILITIES, so no capability check will ever match it"
                );
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_slug_accepts_lowercase_digits_dashes() {
        assert!(valid_slug("admin"));
        assert!(valid_slug("finance-2"));
        assert!(valid_slug("a_b"));
    }

    #[test]
    fn valid_slug_rejects_empty_uppercase_and_spaces() {
        assert!(!valid_slug(""));
        assert!(!valid_slug("Admin"));
        assert!(!valid_slug("has space"));
        assert!(!valid_slug("dot.name"));
    }

    #[test]
    fn validate_permissions_accepts_well_formed_map() {
        let perms = json!({ "posts": ["create", "read"], "*": ["*"] });
        assert!(validate_permissions(&perms).is_ok());
        assert!(validate_permissions(&empty_permissions()).is_ok());
    }

    #[test]
    fn validate_permissions_accepts_known_capabilities() {
        assert!(validate_permissions(&json!({ "printing": ["view"] })).is_ok());
        assert!(validate_permissions(&json!({ "tenants": ["view", "manage"] })).is_ok());
        assert!(validate_permissions(&json!({ "roles": ["manage"] })).is_ok());
        assert!(validate_permissions(&json!({ "reseller": ["clients.view"] })).is_ok());
        assert!(validate_permissions(&json!({ "store.digital": ["read"] })).is_ok());
        assert!(validate_permissions(&json!({ "printing": ["*"] })).is_ok());
        assert!(validate_permissions(&json!({ "*": ["read"] })).is_ok());
    }

    #[test]
    fn validate_permissions_warns_but_accepts_unknown_pairs() {
        // Warn, not reject: the seeded roles grant legacy content
        // pairs (posts/pages/media/...) that are not in
        // KNOWN_CAPABILITIES, and rejecting them would make those
        // roles uneditable.
        assert!(validate_permissions(&json!({ "nope": ["nah"] })).is_ok());
        assert!(validate_permissions(&json!({ "posts": ["create"] })).is_ok());
    }

    #[test]
    fn validate_permissions_rejects_non_object() {
        assert!(validate_permissions(&json!(["posts"])).is_err());
        assert!(validate_permissions(&json!("posts")).is_err());
    }

    #[test]
    fn validate_permissions_rejects_bad_resources_and_actions() {
        assert!(validate_permissions(&json!({ "": ["read"] })).is_err());
        assert!(validate_permissions(&json!({ "posts": "read" })).is_err());
        assert!(validate_permissions(&json!({ "posts": [""] })).is_err());
        assert!(validate_permissions(&json!({ "posts": [42] })).is_err());
    }
}
