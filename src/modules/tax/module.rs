// Tax module — module definition and registration.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{HttpRequest, HttpResponse, web};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct TaxModule {
    pub(crate) state: Arc<TaxState>,
}

impl TaxModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(TaxState::new()),
        }
    }
}

impl Default for TaxModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for TaxModule {
    fn id(&self) -> &'static str {
        "tax"
    }

    fn name(&self) -> &'static str {
        "Tax Management"
    }

    fn description(&self) -> &'static str {
        "Tax rules per jurisdiction, per-company tax settings, tax computation for invoices and orders, tax reports, filing reminders, and a generic HTTP connector framework for tax reporting and filing providers."
    }

    fn requires(&self) -> &[&'static str] {
        &["billing", "settings", "notifications", "scheduler", "audit"]
    }

    fn available_as_product(&self) -> bool {
        false
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("tax", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Real billing event names, plus the `billing.*` alias
        // spellings named in the module brief (the billing module
        // emits `invoice.created` and consumes `order.created`).
        for event in ["invoice.created", "billing.invoice_created"] {
            let state = self.state.clone();
            let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
                on_source_created(state.clone(), ctx, "invoice")
            });
            bus.register(event, "tax", handler);
        }
        for event in ["order.created", "billing.order_created"] {
            let state = self.state.clone();
            let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
                on_source_created(state.clone(), ctx, "order")
            });
            bus.register(event, "tax", handler);
        }
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_filing_check_requested(state.clone(), ctx));
        bus.register("tax.filing.check_requested", "tax", handler);
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/tax")
                // Literal segments first: they must win over `/{id}`.
                .route("/rules", web::get().to(list_rules))
                .route("/rules", web::post().to(create_rule))
                .route("/rules/{id}", web::get().to(get_rule))
                .route("/rules/{id}", web::put().to(update_rule))
                .route("/rules/{id}", web::delete().to(delete_rule))
                .route("/compute", web::post().to(compute_tax))
                .route("/computations", web::get().to(list_computations))
                .route("/computations/{id}", web::get().to(get_computation))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/reports/summary", web::get().to(report_summary))
                .route(
                    "/reports/jurisdictions",
                    web::get().to(report_jurisdictions),
                )
                .route("/reports/periods", web::get().to(report_periods))
                .route("/reports/export", web::get().to(report_export))
                .route("/filings/check-due", web::post().to(check_due_filings))
                .route("/filings", web::get().to(list_filings))
                .route("/filings", web::post().to(create_filing))
                .route("/filings/{id}", web::get().to(get_filing))
                .route("/filings/{id}/submit", web::post().to(submit_filing))
                .route("/connectors", web::get().to(list_connectors))
                .route("/connectors", web::post().to(create_connector))
                .route("/connectors/{id}", web::get().to(get_connector))
                .route("/connectors/{id}", web::put().to(update_connector))
                .route("/connectors/{id}", web::delete().to(delete_connector))
                .route("/connectors/{id}/test", web::post().to(test_connector))
                .route("/audit", web::get().to(list_audit)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/tax/rules",
                owner: "tax",
                description: "List tax rules for the acting company (paginated; filter by country_code, region_code, tax_type, enabled).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/tax/rules",
                owner: "tax",
                description: "Create a tax rule for the acting company.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/tax/rules/{id}",
                owner: "tax",
                description: "Get one tax rule.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/tax/rules/{id}",
                owner: "tax",
                description: "Update a tax rule.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/tax/rules/{id}",
                owner: "tax",
                description: "Delete a tax rule.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/tax/compute",
                owner: "tax",
                description: "Compute tax for an amount and jurisdiction; returns the line-by-line breakdown and stores the computation.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/tax/computations",
                owner: "tax",
                description: "List stored tax computations (paginated; filter by source_type, source_id, country_code, currency).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/tax/computations/{id}",
                owner: "tax",
                description: "Get one stored tax computation.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/tax/settings",
                owner: "tax",
                description: "Get the acting company's tax settings.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/tax/settings",
                owner: "tax",
                description: "Create or update the acting company's tax settings.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/tax/reports/summary",
                owner: "tax",
                description: "Tax summary for a period: totals plus per-jurisdiction and per-period breakdowns.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/tax/reports/jurisdictions",
                owner: "tax",
                description: "Tax totals grouped by jurisdiction for a period.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/tax/reports/periods",
                owner: "tax",
                description: "Tax totals grouped by month for a period.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/tax/reports/export",
                owner: "tax",
                description: "Export the tax report as JSON (default) or CSV (?format=csv).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/tax/filings/check-due",
                owner: "tax",
                description: "Run the filing-reminder check for the acting company: refresh filings and emit reminders for due and overdue filings.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/tax/filings",
                owner: "tax",
                description: "List tax filings for the acting company (paginated; filter by status, country_code, frequency).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/tax/filings",
                owner: "tax",
                description: "Create a tax filing period for the acting company.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/tax/filings/{id}",
                owner: "tax",
                description: "Get one tax filing.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/tax/filings/{id}/submit",
                owner: "tax",
                description: "Submit a tax filing, manually or through a connector.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/tax/connectors",
                owner: "tax",
                description: "List tax provider connectors for the acting company (credentials are never returned).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/tax/connectors",
                owner: "tax",
                description: "Create a tax provider connector (credential is write-only, stored encrypted).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/tax/connectors/{id}",
                owner: "tax",
                description: "Get one tax provider connector (credential fingerprint only).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/tax/connectors/{id}",
                owner: "tax",
                description: "Update a tax provider connector (a supplied credential replaces the stored one).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/tax/connectors/{id}",
                owner: "tax",
                description: "Delete a tax provider connector.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/tax/connectors/{id}/test",
                owner: "tax",
                description: "Test a tax provider connector (example kind returns a simulated result and performs no network call).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/tax/audit",
                owner: "tax",
                description: "List this module's own tax audit trail (paginated; filter by action, resource_type, resource_id).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(TaxModule::new()));
}
