//! roles - store.

use chrono::{Utc};
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;
use super::{types::*, module::*, capabilities::*, seeds::*, validation::*, handlers::*};


/// Resolve a role by id (UUID) or slug. Returns `None` when unknown.
pub fn find_role(store: &RolesStore, role: &str) -> Option<RoleRecord> {
    if let Ok(id) = role.parse::<Uuid>() {
        if let Some(record) = store.get(&id) {
            return Some(record.value().clone());
        }
    }
    store
        .iter()
        .find(|entry| entry.value().slug == role)
        .map(|entry| entry.value().clone())
}


/// Evaluate one permission against a permissions map. `permission` is
/// `resource:action` or the dot form `resource.action`; a bare
/// `resource` checks any action on it, and `*` is granted only by a
/// literal `"*": ["*"]` entry in the map. The dot form splits on the
/// last `.` (so `billing.template.view` is resource
/// `billing.template`, action `view`); a literally-keyed dot-form
/// grant such as `{"printing.view": [...]}` still matches as a
/// fallback, so existing role data keeps working.
pub fn permission_allowed(permissions: &Value, permission: &str) -> bool {
    let map = match permissions.as_object() {
        Some(map) => map,
        None => return false,
    };
    // A wildcard resource grants everything.
    if let Some(actions) = map.get("*") {
        if actions
            .as_array()
            .is_some_and(|list| list.iter().any(|a| a.as_str() == Some("*")))
        {
            return true;
        }
    }
    if permission == "*" {
        // The wildcard-resource check above already returned true for a
        // literal `"*": ["*"]` grant; any other map holds no wildcard,
        // so `"*"` must not be granted here. Returning true for any
        // non-empty map would grant every permission to any role that
        // has any permission at all.
        return false;
    }
    let (resource, action, dotted) = match permission.split_once(':') {
        Some((resource, action)) => (resource, Some(action), false),
        None => match permission.rsplit_once('.') {
            Some((resource, action)) => (resource, Some(action), true),
            None => (permission, None, false),
        },
    };
    let split_allowed = match (map.get(resource).and_then(Value::as_array), action) {
        (Some(list), None) => !list.is_empty(),
        (Some(list), Some(action)) => list
            .iter()
            .any(|a| a.as_str().is_some_and(|name| name == "*" || name == action)),
        (None, _) => false,
    };
    if split_allowed {
        return true;
    }
    // The split form did not grant it: a literally-keyed dot-form
    // grant (`{"printing.view": [...]}`) still can.
    if dotted {
        return map
            .get(permission)
            .and_then(Value::as_array)
            .is_some_and(|list| !list.is_empty());
    }
    false
}


/// Check a permission for a role looked up by id or slug. Returns
/// `Ok(None)` when the role does not exist.
pub fn check_role(store: &RolesStore, role: &str, permission: &str) -> Option<bool> {
    find_role(store, role).map(|record| permission_allowed(&record.permissions, permission))
}
#[cfg(test)]
mod tests {
    use super::*;

    fn admin_record() -> RoleRecord {
        RoleRecord {
            id: Uuid::now_v7(),
            name: "Admin".to_string(),
            slug: "admin".to_string(),
            description: None,
            is_system: true,
            permissions: json!({ "posts": ["create", "read"], "users": ["*"] }),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn store_with_admin() -> (RolesStore, Uuid) {
        let store: RolesStore = Arc::new(DashMap::new());
        let record = admin_record();
        let id = record.id;
        store.insert(id, record);
        (store, id)
    }

    #[test]
    fn permission_allowed_matches_exact_action() {
        let perms = json!({ "posts": ["create", "read"] });
        assert!(permission_allowed(&perms, "posts:create"));
        assert!(permission_allowed(&perms, "posts:read"));
        assert!(!permission_allowed(&perms, "posts:delete"));
        assert!(!permission_allowed(&perms, "users:create"));
    }

    #[test]
    fn permission_allowed_dot_form_natural_grant() {
        let perms = json!({ "printing": ["view"] });
        assert!(permission_allowed(&perms, "printing.view"));
        assert!(!permission_allowed(&perms, "printing.manage"));
        assert!(!permission_allowed(&perms, "events.view"));
    }

    #[test]
    fn permission_allowed_dot_form_multi_dot_resource() {
        let perms = json!({ "billing.template": ["view"] });
        assert!(permission_allowed(&perms, "billing.template.view"));
        assert!(!permission_allowed(&perms, "billing.template.manage"));
        assert!(!permission_allowed(&perms, "billing.view"));
    }

    #[test]
    fn permission_allowed_dot_form_star_action_scoped_to_resource() {
        let perms = json!({ "printing": ["*"] });
        assert!(permission_allowed(&perms, "printing.view"));
        assert!(permission_allowed(&perms, "printing.manage"));
        assert!(!permission_allowed(&perms, "events.view"));
    }

    #[test]
    fn permission_allowed_dot_form_literal_key_still_grants() {
        let perms = json!({ "printing.view": ["view"] });
        assert!(permission_allowed(&perms, "printing.view"));
        let empty = json!({ "printing.view": [] });
        assert!(!permission_allowed(&empty, "printing.view"));
    }

    #[test]
    fn permission_allowed_wildcard_rules_preserved_for_dot_form() {
        let admin = json!({ "*": ["*"] });
        assert!(permission_allowed(&admin, "printing.view"));
        assert!(permission_allowed(&admin, "printing.manage"));
        assert!(permission_allowed(&admin, "*"));
        let plain = json!({ "printing": ["view"] });
        assert!(!permission_allowed(&plain, "*"));
        let scoped = json!({ "events": ["*"] });
        assert!(permission_allowed(&scoped, "events.view"));
        assert!(!permission_allowed(&scoped, "printing.view"));
        assert!(!permission_allowed(&scoped, "*"));
    }

    #[test]
    fn permission_allowed_star_action_in_list() {
        let perms = json!({ "users": ["*"] });
        assert!(permission_allowed(&perms, "users:anything"));
    }

    #[test]
    fn permission_allowed_wildcard_resource_grants_everything() {
        let perms = json!({ "*": ["*"] });
        assert!(permission_allowed(&perms, "posts:delete"));
        assert!(permission_allowed(&perms, "*"));
    }

    #[test]
    fn permission_allowed_bare_resource_checks_any_action() {
        let perms = json!({ "posts": ["read"] });
        assert!(permission_allowed(&perms, "posts"));
        let empty = json!({ "posts": [] });
        assert!(!permission_allowed(&empty, "posts"));
    }

    #[test]
    fn permission_allowed_star_denied_without_wildcard_grant() {
        let perms = json!({ "posts": ["read"] });
        assert!(!permission_allowed(&perms, "*"));
        assert!(!permission_allowed(&json!("not an object"), "posts:read"));
    }

    #[test]
    fn find_role_by_slug_and_uuid() {
        let (store, id) = store_with_admin();
        assert_eq!(find_role(&store, "admin").unwrap().id, id);
        assert_eq!(find_role(&store, &id.to_string()).unwrap().slug.as_str(), "admin");
        assert!(find_role(&store, "missing").is_none());
    }

    #[test]
    fn check_role_answers_and_returns_none_for_unknown() {
        let (store, _) = store_with_admin();
        assert_eq!(check_role(&store, "admin", "posts:create"), Some(true));
        assert_eq!(check_role(&store, "admin", "posts:delete"), Some(false));
        assert_eq!(check_role(&store, "ghost", "posts:create"), None);
    }
}
