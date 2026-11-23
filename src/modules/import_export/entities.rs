//! import export - entities.

use unified_core::hooks::HookBus;
use dashmap::DashMap;
use serde::Serialize;
use std::sync::{Arc, OnceLock};
use uuid::Uuid;
use super::jobs::*;

/// One column of one importable/exportable entity.
#[derive(Debug, Clone, Serialize)]
pub struct EntityColumn {
    pub name: &'static str,
    /// `string`, `integer`, `number`, `boolean`, `date`, `email`, `uuid`.
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub required: bool,
    /// Short description of the column.
    pub description: &'static str,
}

/// One importable/exportable entity.
#[derive(Debug, Clone, Serialize)]
pub struct EntitySpec {
    pub entity: &'static str,
    pub description: &'static str,
    /// Column used for idempotency when present in the file.
    pub external_id_column: Option<&'static str>,
    /// Columns that hold personal data and are redacted on export
    /// unless the caller holds `import_export:export_pii`.
    pub pii_columns: &'static [&'static str],
    pub columns: &'static [EntityColumn],
}

pub(crate) const USERS_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "external_id", kind: "string", required: false, description: "Caller-supplied stable id for idempotency" },
    EntityColumn { name: "email", kind: "email", required: true, description: "User email address" },
    EntityColumn { name: "name", kind: "string", required: true, description: "Display name" },
    EntityColumn { name: "is_active", kind: "boolean", required: false, description: "Whether the account is active" },
    EntityColumn { name: "company_id", kind: "uuid", required: false, description: "Owning company id" },
];

pub(crate) const SOFTWARE_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "external_id", kind: "string", required: false, description: "Caller-supplied stable id for idempotency" },
    EntityColumn { name: "name", kind: "string", required: true, description: "Software name" },
    EntityColumn { name: "slug", kind: "string", required: true, description: "URL slug, unique" },
    EntityColumn { name: "version", kind: "string", required: false, description: "Current version, semver" },
    EntityColumn { name: "description", kind: "string", required: false, description: "Short description" },
    EntityColumn { name: "status", kind: "string", required: false, description: "draft, published, deprecated, or archived" },
];

pub(crate) const PRODUCTS_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "external_id", kind: "string", required: false, description: "Caller-supplied stable id for idempotency" },
    EntityColumn { name: "name", kind: "string", required: true, description: "Product name" },
    EntityColumn { name: "slug", kind: "string", required: true, description: "URL slug, unique" },
    EntityColumn { name: "price", kind: "number", required: true, description: "Price in the store currency" },
    EntityColumn { name: "currency", kind: "string", required: false, description: "ISO currency code, for example USD" },
    EntityColumn { name: "is_active", kind: "boolean", required: false, description: "Whether the product is on sale" },
];

pub(crate) const ORDERS_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "external_id", kind: "string", required: false, description: "Caller-supplied stable id for idempotency" },
    EntityColumn { name: "customer_email", kind: "email", required: true, description: "Buyer email address" },
    EntityColumn { name: "product_slug", kind: "string", required: true, description: "Ordered product slug" },
    EntityColumn { name: "quantity", kind: "integer", required: false, description: "Quantity, default 1" },
    EntityColumn { name: "total", kind: "number", required: true, description: "Order total in the store currency" },
    EntityColumn { name: "currency", kind: "string", required: false, description: "ISO currency code, for example USD" },
    EntityColumn { name: "status", kind: "string", required: false, description: "pending, paid, refunded, or cancelled" },
];

pub(crate) const INVOICES_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "external_id", kind: "string", required: false, description: "Caller-supplied stable id for idempotency" },
    EntityColumn { name: "order_external_id", kind: "string", required: true, description: "External id of the order this invoice belongs to" },
    EntityColumn { name: "number", kind: "string", required: true, description: "Invoice number, unique" },
    EntityColumn { name: "total", kind: "number", required: true, description: "Invoice total in the store currency" },
    EntityColumn { name: "currency", kind: "string", required: false, description: "ISO currency code, for example USD" },
    EntityColumn { name: "status", kind: "string", required: false, description: "draft, issued, paid, void, or overdue" },
    EntityColumn { name: "due_date", kind: "date", required: false, description: "Due date (RFC 3339 or YYYY-MM-DD)" },
];

pub(crate) const TASKS_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "external_id", kind: "string", required: false, description: "Caller-supplied stable id for idempotency" },
    EntityColumn { name: "title", kind: "string", required: true, description: "Task title" },
    EntityColumn { name: "description", kind: "string", required: false, description: "Task description" },
    EntityColumn { name: "status", kind: "string", required: false, description: "open, in_progress, done, or cancelled" },
    EntityColumn { name: "priority", kind: "string", required: false, description: "low, normal, high, or urgent" },
    EntityColumn { name: "assignee_email", kind: "email", required: false, description: "Assignee email address" },
    EntityColumn { name: "due_date", kind: "date", required: false, description: "Due date (RFC 3339 or YYYY-MM-DD)" },
];

pub(crate) const LICENSES_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "external_id", kind: "string", required: false, description: "Caller-supplied stable id for idempotency" },
    EntityColumn { name: "software_slug", kind: "string", required: true, description: "Licensed software slug" },
    EntityColumn { name: "owner_email", kind: "email", required: true, description: "License owner email address" },
    EntityColumn { name: "seats", kind: "integer", required: false, description: "Seat count, default 1" },
    EntityColumn { name: "expires_at", kind: "date", required: false, description: "Expiry (RFC 3339 or YYYY-MM-DD)" },
    EntityColumn { name: "status", kind: "string", required: false, description: "active, suspended, or revoked" },
];

pub(crate) const TENANT_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "id", kind: "uuid", required: true, description: "Tenant uuidv7" },
    EntityColumn { name: "slug", kind: "string", required: true, description: "Tenant slug" },
    EntityColumn { name: "display_name", kind: "string", required: false, description: "Tenant display name" },
    EntityColumn { name: "status", kind: "string", required: false, description: "provisioning, provision_failed, active, suspended, deprovisioning, or deleted" },
    EntityColumn { name: "owner_user_id", kind: "uuid", required: false, description: "Owning user id" },
    EntityColumn { name: "package_id", kind: "uuid", required: false, description: "Package the tenant was provisioned under" },
    EntityColumn { name: "preset_id", kind: "uuid", required: false, description: "Preset the tenant was provisioned under" },
    EntityColumn { name: "plan_id", kind: "uuid", required: false, description: "Billing plan id" },
    EntityColumn { name: "subdomain", kind: "string", required: false, description: "Tenant subdomain label" },
    EntityColumn { name: "subdomain_pending", kind: "boolean", required: false, description: "Whether the subdomain claim is pending approval" },
    EntityColumn { name: "schema_name", kind: "string", required: false, description: "Tenant database schema name" },
    EntityColumn { name: "suspended_at", kind: "date", required: false, description: "Suspension time (RFC 3339 or YYYY-MM-DD)" },
    EntityColumn { name: "suspended_reason", kind: "string", required: false, description: "Why the tenant was suspended" },
    EntityColumn { name: "destroyed_at", kind: "date", required: false, description: "Destroy time (RFC 3339 or YYYY-MM-DD)" },
    EntityColumn { name: "export_reference", kind: "string", required: false, description: "Export job id recorded before destroy" },
    EntityColumn { name: "created_at", kind: "date", required: false, description: "Creation time (RFC 3339 or YYYY-MM-DD)" },
    EntityColumn { name: "updated_at", kind: "date", required: false, description: "Last update time (RFC 3339 or YYYY-MM-DD)" },
];

pub(crate) const ENTITY_SPECS: &[EntitySpec] = &[
    EntitySpec { entity: "users", description: "User accounts", external_id_column: Some("external_id"), pii_columns: &["email", "name"], columns: USERS_COLUMNS },
    EntitySpec { entity: "software", description: "Software products and releases", external_id_column: Some("external_id"), pii_columns: &[], columns: SOFTWARE_COLUMNS },
    EntitySpec { entity: "products", description: "Store products", external_id_column: Some("external_id"), pii_columns: &[], columns: PRODUCTS_COLUMNS },
    EntitySpec { entity: "orders", description: "Store orders", external_id_column: Some("external_id"), pii_columns: &["customer_email"], columns: ORDERS_COLUMNS },
    EntitySpec { entity: "invoices", description: "Billing invoices", external_id_column: Some("external_id"), pii_columns: &[], columns: INVOICES_COLUMNS },
    EntitySpec { entity: "tasks", description: "Tasks", external_id_column: Some("external_id"), pii_columns: &["assignee_email"], columns: TASKS_COLUMNS },
    EntitySpec { entity: "licenses", description: "Software licenses", external_id_column: Some("external_id"), pii_columns: &["owner_email"], columns: LICENSES_COLUMNS },
    EntitySpec { entity: "tenant", description: "Tenant control-plane record (destroy export)", external_id_column: None, pii_columns: &[], columns: TENANT_COLUMNS },
];

pub(crate) fn entity_spec(name: &str) -> Option<&'static EntitySpec> {
    ENTITY_SPECS.iter().find(|spec| spec.entity == name)
}

pub(crate) static HOOK_BUS: OnceLock<Arc<HookBus>> = OnceLock::new();

pub(crate) static EXPORT_JOBS: OnceLock<DashMap<Uuid, ExportJob>> = OnceLock::new();

pub(crate) static IMPORT_JOBS: OnceLock<DashMap<Uuid, ImportJob>> = OnceLock::new();

pub(crate) static MAPPINGS: OnceLock<DashMap<Uuid, MappingTemplate>> = OnceLock::new();

pub(crate) static EXPORT_SCHEDULES: OnceLock<DashMap<Uuid, ExportSchedule>> = OnceLock::new();

pub(crate) static IMPORT_SESSIONS: OnceLock<DashMap<Uuid, ImportSession>> = OnceLock::new();
