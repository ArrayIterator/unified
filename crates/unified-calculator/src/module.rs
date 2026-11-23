//! calculator - module.

use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::web;
use std::sync::Arc;
use super::handlers::*;
use super::state::*;
use super::hooks::*;

pub struct CalculatorModule {
    state: Arc<CalculatorState>,
}

impl CalculatorModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(CalculatorState::new()),
        }
    }
}

impl Default for CalculatorModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for CalculatorModule {
    fn id(&self) -> &'static str {
        "calculator"
    }

    fn name(&self) -> &'static str {
        "Calculator Enterprise"
    }

    fn description(&self) -> &'static str {
        "Business and economics calculation toolkit (ROI, break-even, loan amortisation, depreciation, NPV/IRR, cash-flow projection, pricing, salary, currency conversion, tax estimation) as a submodule of Company Management."
    }

    fn requires(&self) -> &[&'static str] {
        &["company"]
    }

    fn available_as_product(&self) -> bool {
        false
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_rate_requested(state.clone(), ctx));
        bus.register("calculator.rate_requested", "calculator", handler);
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_depreciation_requested(state.clone(), ctx)
        });
        bus.register("calculator.depreciation_requested", "calculator", handler);
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_manufacturing_cost_requested(state.clone(), ctx)
        });
        bus.register(
            "manufacturing.cost.requested",
            "calculator",
            handler,
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/calculator")
                // Literal segments first: they must win over `/{id}`.
                .route("/roi", web::post().to(calculate_roi))
                .route("/break-even", web::post().to(calculate_break_even))
                .route("/loan", web::post().to(calculate_loan))
                .route("/depreciation", web::post().to(calculate_depreciation))
                .route("/npv-irr", web::post().to(calculate_npv_irr))
                .route("/cash-flow", web::post().to(calculate_cash_flow))
                .route("/pricing", web::post().to(calculate_pricing))
                .route("/salary", web::post().to(calculate_salary))
                .route("/currency-convert", web::post().to(convert_currency))
                .route("/tax-estimate", web::post().to(estimate_tax))
                .route("/calculations", web::get().to(list_calculations))
                .route("/calculations", web::post().to(create_calculation))
                .route("/calculations/{id}", web::get().to(get_calculation))
                .route("/calculations/{id}", web::put().to(update_calculation))
                .route("/calculations/{id}", web::delete().to(delete_calculation))
                .route("/reports", web::get().to(list_reports))
                .route("/reports", web::post().to(create_report))
                .route("/reports/shared/{token}", web::get().to(get_shared_report))
                .route("/reports/{id}", web::get().to(get_report))
                .route("/reports/{id}", web::put().to(update_report))
                .route("/reports/{id}", web::delete().to(delete_report))
                .route("/history", web::get().to(list_history))
                .route("/history/{id}", web::get().to(get_history_entry))
                .route("/rates", web::get().to(list_rates))
                .route("/rates", web::post().to(create_rate))
                .route("/rates/{id}", web::delete().to(delete_rate))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "POST", path: "/api/v1/calculator/roi", owner: "calculator", description: "Compute return on investment (gain, cost; cost must be greater than zero).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/calculator/break-even", owner: "calculator", description: "Compute the break-even point in units and revenue (price must exceed variable cost).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/calculator/loan", owner: "calculator", description: "Compute a loan amortisation schedule (principal, annual rate, term months).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/calculator/depreciation", owner: "calculator", description: "Compute a depreciation schedule (straight-line or declining balance).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/calculator/npv-irr", owner: "calculator", description: "Compute net present value and internal rate of return (bounded bisection; explicit error when IRR does not converge).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/calculator/cash-flow", owner: "calculator", description: "Compute a monthly cash-flow projection over N months.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/calculator/pricing", owner: "calculator", description: "Solve pricing, cost, or margin from any two of the three.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/calculator/salary", owner: "calculator", description: "Compute salary and payroll cost (gross, employer contributions, period).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/calculator/currency-convert", owner: "calculator", description: "Convert currency using a stored rate only; stale rates are flagged.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/calculator/tax-estimate", owner: "calculator", description: "Request a tax estimate from the tax module; explicit tax_estimate_unavailable error when no handler answers.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/calculator/calculations", owner: "calculator", description: "List saved calculations (paginated; filter by company_id, calculator_type, created_by).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/calculator/calculations", owner: "calculator", description: "Save a calculation for a company.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/calculator/calculations/{id}", owner: "calculator", description: "Get one saved calculation.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/calculator/calculations/{id}", owner: "calculator", description: "Update a saved calculation.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/calculator/calculations/{id}", owner: "calculator", description: "Delete a saved calculation (its reports go with it).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/calculator/reports", owner: "calculator", description: "List shareable calculation reports (paginated; filter by company_id, visibility, calculation_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/calculator/reports", owner: "calculator", description: "Create a shareable report with a random share token.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/calculator/reports/shared/{token}", owner: "calculator", description: "Fetch a report through its share token (private reports: creator only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/calculator/reports/{id}", owner: "calculator", description: "Get one report (private reports: creator only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/calculator/reports/{id}", owner: "calculator", description: "Update a report title or visibility.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/calculator/reports/{id}", owner: "calculator", description: "Delete a report.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/calculator/history", owner: "calculator", description: "List calculation history (paginated; filter by company_id, calculator_type, actor).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/calculator/history/{id}", owner: "calculator", description: "Get one history entry.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/calculator/rates", owner: "calculator", description: "List stored currency rates (paginated; filter by base_currency, target_currency, provider).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/calculator/rates", owner: "calculator", description: "Store a currency rate (replaces the stored rate for the same base/target/provider).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/calculator/rates/{id}", owner: "calculator", description: "Delete a stored currency rate.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/calculator/settings", owner: "calculator", description: "Get the currency provider settings (provider key and default max rate age).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/calculator/settings", owner: "calculator", description: "Update the currency provider settings.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(CalculatorModule::new()));
}
