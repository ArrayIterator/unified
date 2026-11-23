// Calculator Enterprise module, as its own crate
// (`crates/unified-calculator`).
//
// Calculator Enterprise module.
//
// A business and economics calculation toolkit, a SUBMODULE of
// Company Management (`requires = ["company"]`). This is NOT a
// standard calculator: every endpoint runs a business calculation
// server-side (return on investment, break-even point, loan
// amortisation, depreciation, net present value / internal rate of
// return, cash-flow projection, pricing and margin, salary and
// payroll cost, currency conversion, and tax estimation) and returns
// the result together with the exact inputs that produced it.
//
// Persistence model: the in-memory DashMap store is authoritative
// (see the module header of `the server modules::departments` for the
// pattern); the migration `2026-11-22-000100_calculator` is the
// persistence target for saved calculations, shareable reports,
// calculation history, and configured currency rates.
//
// Events emitted:
//   - `calculator.calculation_saved` -- a calculation was saved
//     (`{"calculation_id", "company_id", "calculator_type",
//     "actor"}`).
//   - `calculator.report_shared` -- a shareable report was created
//     (`{"report_id", "calculation_id", "company_id",
//     "share_token", "visibility", "actor"}`).
//   - `calculator.calculation_deleted` -- a saved calculation was
//     deleted (`{"calculation_id", "company_id", "actor"}`).
//
// Events consumed (outgoing request only):
//   - `tax.estimate_requested` -- tax estimation is delegated to the
//     tax module through this event. This module never computes a tax
//     figure itself: when no handler answers, the endpoint returns an
//     explicit `tax_estimate_unavailable` error instead of a guessed
//     number.
//   - `calculator.rate_requested` -- answered by this module: other
//     modules may ask for a stored currency rate
//     (`{"base_currency", "target_currency"}`) and receive
//     `{"base_currency", "target_currency", "rate", "provider",
//     "stale"}` when a rate is stored.
//   - `calculator.depreciation_requested` -- answered by this
//     module: the assets module asks for a depreciation schedule
//     (`{"asset_id", "method", "purchase_price", "salvage_value",
//     "useful_life_years", "purchase_date", "currency"}`) and
//     receives the computed result object, which carries
//     `schedule` (one row per year) plus `cost`, `salvage_value`,
//     `useful_life_years`, `method`, `multiplier`,
//     `annual_depreciation`, and `total_depreciation`. A request
//     that is missing a field or fails validation is answered
//     with `{"error": ...}` instead of silence.
//   - `manufacturing.cost.requested` -- answered by this module:
//     manufacturing asks for per-product unit costs
//     (`{"items": [{product_id, quantity, unit}], "currency"}`)
//     and receives `{"unit_costs": {product_id: cost}, "gaps":
//     [...], "partial": true/false}`. A unit cost comes only from
//     the latest saved `pricing` calculation whose inputs name
//     that product (its solved `cost`); products with no such
//     calculation are listed in `gaps` with `partial: true` -- a
//     cost is never fabricated, and an unpriced product is never
//     answered with zero.
//
// Currency conversion: rates come only from the rates stored through
// this module (configured under a provider settings key, see
// `CalculatorSettings`). No exchange rate is hardcoded anywhere. A
// rate whose `fetched_at` timestamp is older than its configured
// `max_age_seconds` is reported with `stale: true`.
//
// Money: monetary results are rounded to four decimals by the module
// (matching NUMERIC(19,4) in SQL); exchange rates and percentages
// are ratios, not money, and are stored as DOUBLE PRECISION with
// that justification in the migration.


// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

pub mod types;
pub mod state;
mod validation;
mod compute;
mod handlers;
mod hooks;
pub mod module;

pub use self::types::*;
pub use self::state::*;
pub use self::validation::*;
pub use self::compute::*;
pub use self::handlers::*;
pub use self::hooks::*;
pub use self::module::*;
