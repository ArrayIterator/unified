//! Saas Packages - Seed data installed on first start.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::middleware::TenantContext;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::tenancy as tenancy;
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai::{register_ai_capabilities, run_ai_capability, AiCapability};
use crate::modules::licensing::check_module_entitlement;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::{constants::*, types::*, state::*, auth::*, helpers::*, provisioning::*, handlers_packages::*, handlers_purchases::*, handlers_ai::*, events::*, module::*};

// ---------------------------------------------------------------------------
// SEED DATA -- the example package as initial rows.
//
// This table is read exactly once, at startup (`init`), to insert
// rows into the package store, and is never read again: every other
// line of this module reads packages from the store only. The same
// row, with the same id, is the `INSERT` statement in migration
// `000096_saas_packages`, so the database and the in-memory store
// agree. The seed is EXAMPLE DATA ONLY: an admin can edit or delete
// it like any other package, and the code works with zero packages
// defined. No package is ever hard-coded anywhere else in this file.
//
// The bundled modules are the ones the brief names for the headline
// example: exams, e-learning, academic, notifications, payments.
// At write time `exams` has a file and migration (`000090_exams`,
// parallel batch 14 builder) but is not registered in
// `src/modules/mod.rs`; `elearning` / `academic` exist as files but
// are not registered in `src/modules/mod.rs` either; `notifications` and
// `payments` are registered. `init` logs a warning per unregistered
// seed module instead of failing startup, and provisioning reports
// such modules as unknown until their modules register.
// ---------------------------------------------------------------------------

/// Fixed id for the seeded example package, shared with the
/// migration `INSERT`s so the database row and the in-memory row
/// match.
pub const SEED_ONLINE_EXAM_PACKAGE_ID: &str = "20000000-0000-4000-8000-000000000001";

/// Fixed id for the seeded example Site Builder package, shared with
/// migration `000130_site_builder_package_seed` exactly like the exam
/// package above.
pub const SEED_SITE_BUILDER_PACKAGE_ID: &str = "20000000-0000-4000-8000-000000000002";

/// The example "Site Builder SaaS" package (task 353): the site
/// builder as a PACKAGE. A tenant ordering this package gets the
/// builder plus the content module, the theme, and its own domain
/// (subdomain or custom, with SSL via the domains module). No
/// pricing is invented here (`product_id` stays `None`; pricing
/// lives on the store product, if one is ever attached). The control
/// plane's own system home page does NOT come from this package and
/// is never tenant-gated.
pub(crate) fn seed_site_builder_package() -> SaasPackage {
    let now = Utc::now();
    SaasPackage {
        id: Uuid::parse_str(SEED_SITE_BUILDER_PACKAGE_ID)
            .expect("seed package id is a valid UUID"),
        slug: "site-builder-saas".to_string(),
        name: "Site Builder SaaS".to_string(),
        description: "A website on its own domain, built with the GrapeJS site builder: pages of ordered blocks, a tenant subdomain or custom domain with SSL, and the content module for posts and pages.".to_string(),
        status: STATUS_PUBLISHED.to_string(),
        current_version: 1,
        product_id: None,
        preset_settings: json!({
            "welcome_email_enabled": true,
            "default_language": "en"
        }),
        wizard_steps: vec![
            "business_type".to_string(),
            "review_modules".to_string(),
            "apply".to_string(),
            "settings".to_string(),
            "done".to_string(),
        ],
        modules: vec![
            PackageModuleEntry { module_id: "site-builder".to_string(), required: true, sort_order: 0 },
            PackageModuleEntry { module_id: "content".to_string(), required: true, sort_order: 1 },
            PackageModuleEntry { module_id: "theme".to_string(), required: true, sort_order: 2 },
            PackageModuleEntry { module_id: "domains".to_string(), required: true, sort_order: 3 },
        ],
        created_by: "system".to_string(),
        vendor_id: None,
        created_at: now,
        updated_at: now,
    }
}

pub(crate) fn seed_online_exam_package() -> SaasPackage {
    let now = Utc::now();
    SaasPackage {
        id: Uuid::parse_str(SEED_ONLINE_EXAM_PACKAGE_ID)
            .expect("seed package id is a valid UUID"),
        slug: "online-exam-saas".to_string(),
        name: "Online Exam SaaS".to_string(),
        description: "Everything a school or training provider needs to run online exams: exams, e-learning courses, academic records, notifications, and payments, provisioned on its own tenant subdomain with a guided setup wizard.".to_string(),
        status: STATUS_PUBLISHED.to_string(),
        current_version: 1,
        product_id: None,
        preset_settings: json!({
            "welcome_email_enabled": true,
            "default_language": "en"
        }),
        wizard_steps: vec![
            "business_type".to_string(),
            "review_modules".to_string(),
            "apply".to_string(),
            "settings".to_string(),
            "done".to_string(),
        ],
        modules: vec![
            PackageModuleEntry { module_id: "exams".to_string(), required: true, sort_order: 0 },
            PackageModuleEntry { module_id: "elearning".to_string(), required: true, sort_order: 1 },
            PackageModuleEntry { module_id: "academic".to_string(), required: true, sort_order: 2 },
            PackageModuleEntry { module_id: "notifications".to_string(), required: true, sort_order: 3 },
            PackageModuleEntry { module_id: "payments".to_string(), required: true, sort_order: 4 },
        ],
        created_by: "system".to_string(),
        vendor_id: None,
        created_at: now,
        updated_at: now,
    }
}
