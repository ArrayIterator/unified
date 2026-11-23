//! roles - seeds.

use serde_json::{json, Value};
use super::{store::*, types::*, module::*, capabilities::*, validation::*, handlers::*};


/// The seven static default roles and their permission sets.
pub(crate) fn default_roles() -> Vec<(&'static str, &'static str, &'static str, Value)> {
    vec![
        (
            "Administrator",
            "admin",
            "Full access to every resource.",
            json!({ "*": ["*"] }),
        ),
        (
            "Editor",
            "editor",
            "Can manage content, pages, comments and media.",
            json!({
                "posts": ["create", "read", "update", "publish", "delete"],
                "pages": ["create", "read", "update", "publish", "delete"],
                "comments": ["read", "update", "delete"],
                "media": ["create", "read", "update", "delete"],
            }),
        ),
        (
            "Author",
            "author",
            "Can write and manage their own content.",
            json!({
                "posts": ["create", "read", "update"],
                "media": ["create", "read", "update"],
                "comments": ["read"],
            }),
        ),
        (
            "Member",
            "member",
            "Can read content and take part in discussions.",
            json!({
                "posts": ["read"],
                "pages": ["read"],
                "comments": ["create", "read"],
            }),
        ),
        (
            "Customer",
            "customer",
            "Can browse the store, place orders and view licenses.",
            json!({
                "store": ["read", "purchase"],
                "orders": ["create", "read"],
                "licenses": ["read"],
            }),
        ),
        (
            "Reseller",
            "reseller",
            "Can manage linked clients and view commissions and payouts.",
            json!({
                "reseller": ["access", "clients.view", "clients.manage", "payouts.view"],
                "user": ["access"],
                "orders": ["read"],
                "licenses": ["read"],
            }),
        ),
        (
            // Finance role (batch 18): the only non-admin role seeded
            // with card capabilities. Never grant `cards:validate`
            // to customer, editor, author, or member roles — an open
            // validation oracle lets anyone probe card numbers.
            "Finance",
            "finance",
            "Manages card validation settings and custom rules.",
            json!({ "cards": ["read", "validate", "manage"], "payments": ["read"] }),
        ),
    ]
}
