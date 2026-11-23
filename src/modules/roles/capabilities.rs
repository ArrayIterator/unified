//! roles - capabilities.

use super::{store::*, types::*, module::*, seeds::*, validation::*, handlers::*};


/// Capability required to read roles (list, get, permissions).
pub const PERM_ROLES_VIEW: &str = "roles:view";

/// Capability required to manage roles (create, update, delete) and
/// to call the permission check endpoint.
pub const PERM_ROLES_MANAGE: &str = "roles:manage";

/// Every capability string checked anywhere in the codebase, as the
/// single definition list. Capability checks go through the
/// `role.check` hook and fail closed: a permission that is not
/// granted to the caller's role (and is not covered by the admin
/// role's `"*": ["*"]` wildcard) is denied, so a capability only
/// becomes usable once it is listed here AND granted to a role.
/// Nothing below grants anything to the customer, editor, author,
/// or member roles — the only non-admin role seeded with new
/// capabilities is Finance (cards, batch 18). Two naming shapes
/// coexist in the tree and both are listed verbatim as checked:
/// `resource:action` (cards, pricing, entitlements, calendar,
/// holidays, backup, tax, advisor, risk, hr, and the older
/// colon-form modules) and `resource.action` (the dot-form modules
/// such as printing, manufacturing, quality, expenses, approvals,
/// studio, and the batch-4 modules).
///
/// Known gaps, recorded honestly rather than papered over:
/// - Webinars gates on the role names `admin` / `webinars.admin`
///   plus host ownership (`is_admin` in webinars.rs), not on
///   `role.check`, so `webinars.view` / `webinars.manage` /
///   `webinars.host` are defined here per the batch-10 notes but
///   are not consulted by that module yet.
/// - Appointments checks `appointments.create`, not the
///   `appointments.book` name the batch-10 notes use; `book` is
///   therefore not listed.
/// - Academic checks the colon form (`academic:read`,
///   `academic:read_all`, `academic:manage`, `academic:advise`,
///   `academic:grade`, `academic:transcript`), not the
///   `academic:view` spelling the batch-13 notes use; `view` is
///   therefore not listed. Admissions likewise checks
///   `admissions:read` / `admissions:read_all` /
///   `admissions:manage` / `admissions:apply` /
///   `admissions:score`.
pub const KNOWN_CAPABILITIES: &[&str] = &[
    // AI and usage.
    "ai:use", "ai:approve", "ai:read_usage", "ai:manage_providers",
    // Events, time tracking, todos, products (batch 2).
    "events.view", "events.create", "events.manage", "events.checkin",
    "timetracker:read", "timetracker:write", "timetracker:approve",
    "timetracker:manage", "timetracker:manage_staff",
    "timetracker:manage_rates", "timetracker:manage_settings",
    "timetracker:manage_buyer_settings", "timetracker:view_team",
    "timetracker:view_team_screenshots", "timetracker:payroll",
    "todos:read", "todos:write", "todos:share",
    "products:read",
    // Storage Manager (batch 2).
    "storage_manager.read", "storage_manager.manage_buckets",
    "storage_manager.manage_bindings", "storage_manager.manage_quotas",
    "storage_manager.manage_signed_urls", "storage_manager.manage_plans",
    "storage_manager.manage_byo", "storage_manager.use_byo",
    "storage_manager.manage_lifecycle", "storage_manager.browse",
    "storage_manager.upload", "storage_manager.health",
    // Tax, advisor, risk, HR (batch 3).
    "tax:view", "tax:manage", "tax:file", "tax:manage_connectors",
    "advisor:view", "advisor:manage",
    "risk:view", "risk:create", "risk:edit", "risk:delete",
    "hr:read", "hr:read_all", "hr:manage", "hr:payroll",
    // Subscriptions (batch 3).
    "calculator:use", "calculator:manage",
    "marketplace:read", "marketplace:manage", "marketplace:vendor",
    "pricing_tables:read", "pricing_tables:manage",
    // Batch 4 modules: `<module>.view` + `<module>.manage`.
    "workflows.view", "workflows.manage",
    "bi.view", "bi.manage",
    "okrs.view", "okrs.manage",
    "affiliates.view", "affiliates.manage",
    "loyalty.view", "loyalty.manage",
    "portals.view", "portals.manage",
    "forms.view", "forms.manage",
    "contracts.view", "contracts.manage",
    "procurement.view", "procurement.manage",
    "assets.view", "assets.manage",
    "recruitment.view", "recruitment.manage",
    "vault.view", "vault.manage",
    "incidents.view", "incidents.manage",
    "inbox.view", "inbox.manage",
    "surveys.view", "surveys.manage",
    "community.view", "community.manage",
    "compliance.view", "compliance.manage",
    "gamification.view", "gamification.manage",
    // Auditors, sharing, supervision, logistics, properties (batch 5).
    "auditors.view", "auditors.create", "auditors.manage", "auditors.revoke",
    "sharing.view", "sharing.create", "sharing.revoke",
    "supervision.view", "supervision.create", "supervision.manage",
    "supervision.approve",
    "logistics.view", "logistics.create", "logistics.manage",
    "logistics.driver", "logistics.quote",
    "properties.view", "properties.create", "properties.manage",
    // Documents, privacy, web3 (batch 6).
    "documents:read", "documents:write", "documents:admin",
    "privacy:read", "privacy:write", "privacy:admin",
    "web3:read", "web3:write", "web3:admin",
    // Licensing and network tools (batch 7).
    "licensing:view", "licensing:manage",
    "network_tools:view", "network_tools:run", "network_tools:manage",
    "network_tools:probe_private",
    // SaaS domains, domain verification, certificates (batch 8).
    "saas_domains.read", "saas_domains.manage", "saas_domains.self_assign",
    "domain_verification.read", "domain_verification.manage",
    "domain_verification.issue", "domain_verification.verify",
    "certificates.view", "certificates.manage", "certificates.manage_own",
    // Whitelabel and presets (batch 9).
    "whitelabel.view", "whitelabel.manage", "whitelabel.branding.manage",
    "presets:view", "presets:manage", "presets:apply",
    // Webinars, e-learning, POS, appointments, marketing, field
    // service, digital products (batch 10).
    "webinars.view", "webinars.manage", "webinars.host",
    "elearning.view", "elearning.manage", "elearning.instruct",
    "pos.view", "pos.sell", "pos.manage", "pos.refund",
    "appointments.view", "appointments.manage", "appointments.create",
    "marketing.view", "marketing.manage", "marketing.send",
    "field_service.view", "field_service.manage", "field_service.technician",
    "store.digital:read", "store.digital:manage",
    // Backup, clusters, import/export, cache, storage, security
    // (batch 11). `app-sync` is the module id as checked.
    "backup:view", "backup:manage",
    "clusters:view", "clusters:manage",
    "app-sync:view", "app-sync:manage",
    "import_export:read", "import_export:import", "import_export:export",
    "import_export:export_pii", "import_export:manage",
    "cache.read", "cache.write", "cache.manage",
    "storage.read", "storage.write", "storage.manage_targets",
    "security:view", "security:manage",
    // Printing, manufacturing, quality, expenses, approvals, studio
    // (batch 12).
    "billing.template.view", "billing.template.manage", "billing.print",
    "printing.view", "printing.manage", "printing.job",
    "manufacturing.view", "manufacturing.manage", "manufacturing.produce",
    "manufacturing.admin",
    "quality.view", "quality.manage", "quality.inspect",
    "expenses.view", "expenses.submit", "expenses.manage",
    "approvals.view", "approvals.manage", "approvals.decide",
    "studio.view", "studio.customize", "studio.admin",
    // Academic, curriculum, admissions, school fees (batch 13).
    "academic:read", "academic:read_all", "academic:manage",
    "academic:advise", "academic:grade", "academic:transcript",
    "curriculum:view", "curriculum:manage", "curriculum:teach",
    "admissions:read", "admissions:read_all", "admissions:manage",
    "admissions:apply", "admissions:score",
    "school_fees.view", "school_fees.manage",
    // Exams, library, SaaS packages (batch 14).
    "exams.view", "exams.manage", "exams.take", "exams.grade",
    "library.view", "library.manage", "library.circulate", "library.catalog",
    "saas_packages.view", "saas_packages.manage", "saas_packages.purchase",
    "saas_packages.provision",
    // Entitlements, pricing, product subdomains (batch 15).
    "entitlements:read", "entitlements:manage", "entitlements:grant",
    "pricing:read", "pricing:manage", "pricing:approve",
    // Calendar and holidays (batch 16).
    "calendar:read", "calendar:manage",
    "holidays:read", "holidays:manage", "holidays:sync",
    // Tenants and tenant destroy (batch 17).
    "tenants:view", "tenants:manage", "tenants:provision", "tenants:suspend",
    "tenants:destroy", "tenants:destroy_read",
    // Roles administration: role CRUD and the permission check
    // endpoint are gated behind these (see roles/handlers.rs).
    "roles:view", "roles:manage",
    // Cards and reference ids (batch 18).
    "cards:read", "cards:validate", "cards:manage",
    "store.reference:read", "store.reference:manage",
    "billing.reference:read", "billing.reference:manage",
    // Analytics, billing settings and store catalog (security fix:
    // these endpoints previously ran with no caller check at all).
    "analytics.view", "analytics.manage",
    "billing.settings.view", "billing.settings.manage",
    "store.catalog:manage",
    // Frontend builders: the site builder, email templates, and
    // theme modules check these through `role.check`, and the
    // dashboard navigation entries reference them. The site builder
    // checks the dot form `site-builder.manage`; email templates
    // and themes check the colon forms `email_templates:manage`
    // and `theme:manage` (see `CAP_SITE_BUILDER_MANAGE`,
    // `CAP_EMAIL_TEMPLATES_MANAGE`, `CAP_THEME_MANAGE` in
    // dashboards/types.rs).
    "site-builder.manage", "email_templates:manage", "theme:manage",
    // Pre-existing capability strings checked by older modules.
    "admin:access", "admin:view_as_user", "apikeys:read", "licenses:read",
    "logs:view", "modules:manage", "orders:manage", "orders:read",
    "reseller:access", "reseller:clients.view", "reseller:clients.manage",
    "reseller:payouts.view", "servers:manage",
    "services:read", "settings:manage", "tickets:read", "tickets:view_all",
    "user:access", "users:manage", "projects:view",
];


/// True when `permission` is a capability this codebase defines
/// (see `KNOWN_CAPABILITIES`). This only answers whether the string
/// exists; whether a role holds it is still decided by
/// `permission_allowed` on that role's permission map.
pub fn capability_defined(permission: &str) -> bool {
    KNOWN_CAPABILITIES.contains(&permission)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_defined_finds_known_capabilities() {
        assert!(capability_defined("ai:use"));
        assert!(capability_defined("events.view"));
        assert!(capability_defined("timetracker:read"));
        assert!(capability_defined(PERM_ROLES_VIEW));
        assert!(capability_defined(PERM_ROLES_MANAGE));
        // Security fix: the analytics, billing-settings and
        // store-catalog capabilities must be known strings.
        assert!(capability_defined("analytics.view"));
        assert!(capability_defined("analytics.manage"));
        assert!(capability_defined("billing.settings.view"));
        assert!(capability_defined("billing.settings.manage"));
        assert!(capability_defined("store.catalog:manage"));
        // Frontend builders: the site builder, email templates,
        // and theme manage capabilities must be known strings.
        assert!(capability_defined("site-builder.manage"));
        assert!(capability_defined("email_templates:manage"));
        assert!(capability_defined("theme:manage"));
    }

    #[test]
    fn capability_defined_rejects_unknown_strings() {
        assert!(!capability_defined("nope:nah"));
        assert!(!capability_defined(""));
        assert!(!capability_defined("ai:uses"));
    }
}
