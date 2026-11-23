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
