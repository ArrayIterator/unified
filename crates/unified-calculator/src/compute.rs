//! calculator - compute.

use unified_core::error::{UnifiedError, UnifiedResult};
use serde::Serialize;
use serde_json::{json, Value};
use super::types::*;
use super::validation::*;

/// Return on investment: ROI percent = (gain - cost) / cost * 100.
/// Cost must be greater than zero (division by zero is rejected).
pub(crate) fn compute_roi(input: &RoiInput) -> UnifiedResult<Value> {
    let gain = require_non_negative(input.gain, "gain")?;
    let cost = require_positive(input.cost, "cost")?;
    let net_gain = gain - cost;
    let roi_percent = net_gain / cost * 100.0;
    Ok(json!({
        "gain": round_money(gain),
        "cost": round_money(cost),
        "net_gain": round_money(net_gain),
        "roi_percent": round_ratio(roi_percent),
    }))
}

/// Break-even point in units: fixed costs / (price - variable cost).
/// Price must exceed variable cost, otherwise the break-even point
/// does not exist.
pub(crate) fn compute_break_even(input: &BreakEvenInput) -> UnifiedResult<Value> {
    let fixed = require_non_negative(input.fixed_costs, "fixed_costs")?;
    let price = require_positive(input.price, "price")?;
    let variable = require_non_negative(input.variable_cost, "variable_cost")?;
    if price <= variable {
        return Err(UnifiedError::bad_request(
            "price must be greater than variable_cost: otherwise the break-even point does not exist",
        ));
    }
    let units = fixed / (price - variable);
    Ok(json!({
        "fixed_costs": round_money(fixed),
        "price": round_money(price),
        "variable_cost": round_money(variable),
        "contribution_margin_per_unit": round_money(price - variable),
        "break_even_units": round_ratio(units),
        "break_even_revenue": round_money(units * price),
    }))
}

/// One row of a loan amortisation schedule.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AmortisationRow {
    period: u32,
    payment: f64,
    principal: f64,
    interest: f64,
    balance: f64,
}

/// Loan / mortgage amortisation: full per-period schedule with
/// principal, interest, and remaining balance.
pub(crate) fn compute_loan(input: &LoanInput) -> UnifiedResult<Value> {
    let principal = require_positive(input.principal, "principal")?;
    let annual_rate = require_non_negative(input.annual_rate, "annual_rate")?;
    if input.term_months < 1 || input.term_months > MAX_LOAN_MONTHS {
        return Err(UnifiedError::bad_request(format!(
            "term_months must be between 1 and {MAX_LOAN_MONTHS}"
        )));
    }
    let months = input.term_months;
    let monthly_rate = annual_rate / 100.0 / 12.0;
    let payment = if monthly_rate == 0.0 {
        principal / months as f64
    } else {
        principal * monthly_rate / (1.0 - (1.0 + monthly_rate).powf(-(months as f64)))
    };
    if !payment.is_finite() || payment <= 0.0 {
        return Err(UnifiedError::bad_request(
            "loan payment could not be computed from these inputs",
        ));
    }
    let mut balance = principal;
    let mut schedule: Vec<AmortisationRow> = Vec::with_capacity(months as usize);
    for period in 1..=months {
        let interest = balance * monthly_rate;
        let mut principal_part = payment - interest;
        // The final payment clears the remaining balance exactly,
        // so rounding never leaves a residual balance behind.
        if period == months {
            principal_part = balance;
        }
        if principal_part < 0.0 {
            return Err(UnifiedError::bad_request(
                "loan payment does not cover the interest: increase the payment or lower the rate",
            ));
        }
        balance -= principal_part;
        schedule.push(AmortisationRow {
            period,
            payment: round_money(if period == months {
                principal_part + interest
            } else {
                payment
            }),
            principal: round_money(principal_part),
            interest: round_money(interest),
            balance: round_money(balance.max(0.0)),
        });
    }
    // Both totals come from the same rounded schedule rows, never
    // from a mix of rounded and unrounded values: total_paid is the
    // sum of the displayed per-period payments, and total_interest
    // is that total less the principal. Summing unrounded interest
    // separately could disagree with total_paid - principal by a
    // cent; deriving interest from the rounded total makes the two
    // always agree exactly (and stay within rounding tolerance of
    // the sum of the displayed interest rows).
    let total_paid = round_money(schedule.iter().map(|row| row.payment).sum::<f64>());
    let total_interest = round_money(total_paid - principal);
    Ok(json!({
        "principal": round_money(principal),
        "annual_rate": round_ratio(annual_rate),
        "term_months": months,
        "monthly_payment": round_money(payment),
        "total_paid": total_paid,
        "total_interest": total_interest,
        "schedule": schedule,
    }))
}

/// One row of a depreciation schedule.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DepreciationRow {
    year: u32,
    depreciation: f64,
    accumulated_depreciation: f64,
    book_value: f64,
}

/// Depreciation: straight-line and declining balance. Cost must
/// exceed the salvage value and the useful life must be at least
/// one year.
pub(crate) fn compute_depreciation(input: &DepreciationInput) -> UnifiedResult<Value> {
    let cost = require_positive(input.cost, "cost")?;
    let salvage = require_non_negative(input.salvage_value, "salvage_value")?;
    if cost <= salvage {
        return Err(UnifiedError::bad_request(
            "cost must be greater than salvage_value",
        ));
    }
    if input.useful_life_years < 1 || input.useful_life_years > MAX_DEPRECIATION_YEARS {
        return Err(UnifiedError::bad_request(format!(
            "useful_life_years must be between 1 and {MAX_DEPRECIATION_YEARS}"
        )));
    }
    let method = input.method.as_str();
    if !matches!(method, "straight_line" | "declining_balance") {
        return Err(UnifiedError::bad_request(
            "method must be one of: straight_line, declining_balance",
        ));
    }
    let life = input.useful_life_years;
    let depreciable = cost - salvage;
    let mut schedule: Vec<DepreciationRow> = Vec::with_capacity(life as usize);
    let mut book_value = cost;
    let mut accumulated = 0.0;
    let annual_straight_line = depreciable / life as f64;
    let multiplier = match input.multiplier {
        Some(value) => {
            let value = require_finite(value, "multiplier")?;
            if !(1.0..=3.0).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "multiplier must be between 1.0 and 3.0",
                ));
            }
            value
        }
        None => 2.0,
    };
    let declining_rate = multiplier / life as f64;
    for year in 1..=life {
        let mut depreciation = match method {
            "straight_line" => annual_straight_line,
            _ => book_value * declining_rate,
        };
        // Never depreciate below the salvage value; the final year
        // clears exactly the remaining depreciable amount.
        let remaining = book_value - salvage;
        if depreciation > remaining || year == life {
            depreciation = remaining;
        }
        if depreciation < 0.0 {
            depreciation = 0.0;
        }
        book_value -= depreciation;
        accumulated += depreciation;
        schedule.push(DepreciationRow {
            year,
            depreciation: round_money(depreciation),
            accumulated_depreciation: round_money(accumulated),
            book_value: round_money(book_value),
        });
    }
    Ok(json!({
        "cost": round_money(cost),
        "salvage_value": round_money(salvage),
        "useful_life_years": life,
        "method": method,
        "multiplier": if method == "declining_balance" { Some(round_ratio(multiplier)) } else { None },
        "annual_depreciation": if method == "straight_line" { Some(round_money(annual_straight_line)) } else { None },
        "total_depreciation": round_money(accumulated),
        "schedule": schedule,
    }))
}

/// Net present value of a cash-flow series at a discount rate
/// (percent). Cash flows are end-of-period, starting in year 1.
pub(crate) fn npv_at_rate(initial_investment: f64, cash_flows: &[f64], rate: f64) -> f64 {
    let mut npv = -initial_investment;
    for (index, flow) in cash_flows.iter().enumerate() {
        npv += flow / (1.0 + rate).powf((index + 1) as f64);
    }
    npv
}

/// Net present value and internal rate of return. IRR is found by
/// bounded bisection (at most `IRR_MAX_ITERATIONS` iterations) over
/// the rate interval [-0.9, 10.0]. When NPV has the same sign at both
/// bounds there is no IRR to find, and when bisection does not
/// converge within the bound the error says so explicitly -- the
/// loop is bounded, never infinite.
pub(crate) fn compute_npv_irr(input: &NpvIrrInput) -> UnifiedResult<Value> {
    let initial = require_non_negative(input.initial_investment, "initial_investment")?;
    let discount_rate = require_finite(input.discount_rate, "discount_rate")?;
    if discount_rate < -99.0 {
        return Err(UnifiedError::bad_request(
            "discount_rate must be greater than -99 percent",
        ));
    }
    if input.cash_flows.is_empty() || input.cash_flows.len() > 360 {
        return Err(UnifiedError::bad_request(
            "cash_flows must contain between 1 and 360 entries",
        ));
    }
    let mut flows: Vec<f64> = Vec::with_capacity(input.cash_flows.len());
    for flow in &input.cash_flows {
        flows.push(require_finite(*flow, "cash_flows entry")?);
    }
    let rate = discount_rate / 100.0;
    let npv = npv_at_rate(initial, &flows, rate);
    // IRR by bisection between -90% and +1000%.
    let mut low: f64 = -0.9;
    let mut high: f64 = 10.0;
    let npv_low = npv_at_rate(initial, &flows, low);
    let npv_high = npv_at_rate(initial, &flows, high);
    if !npv_low.is_finite() || !npv_high.is_finite() {
        return Err(UnifiedError::bad_request(
            "IRR cannot be computed from these cash flows",
        ));
    }
    let irr_percent: Option<f64> = if npv_low == 0.0 {
        Some(low * 100.0)
    } else if npv_high == 0.0 {
        Some(high * 100.0)
    } else if npv_low.signum() == npv_high.signum() {
        None
    } else {
        let mut result: Option<f64> = None;
        let mut iterations = 0;
        while iterations < IRR_MAX_ITERATIONS {
            iterations += 1;
            let mid = (low + high) / 2.0;
            let npv_mid = npv_at_rate(initial, &flows, mid);
            if !npv_mid.is_finite() {
                break;
            }
            if npv_mid.abs() < IRR_TOLERANCE || (high - low) < IRR_TOLERANCE {
                result = Some(mid * 100.0);
                break;
            }
            if npv_mid.signum() == npv_low.signum() {
                low = mid;
            } else {
                high = mid;
            }
        }
        match result {
            Some(value) => Some(value),
            None => {
                return Err(UnifiedError::unprocessable(
                    "IRR did not converge within the iteration limit for these cash flows",
                )
                .with_metadata("error_code", json!("irr_not_converged"))
                .with_metadata("max_iterations", json!(IRR_MAX_ITERATIONS)));
            }
        }
    };
    let irr = match irr_percent {
        Some(value) => json!(round_ratio(value)),
        None => Value::Null,
    };
    let mut response = json!({
        "initial_investment": round_money(initial),
        "cash_flows": flows.iter().map(|flow| round_money(*flow)).collect::<Vec<f64>>(),
        "discount_rate": round_ratio(discount_rate),
        "npv": round_money(npv),
        "irr_percent": irr,
    });
    if irr.is_null() {
        response["irr_error"] = json!(
            "No internal rate of return exists for these cash flows: NPV has the same sign at both search bounds"
        );
    }
    Ok(response)
}

/// One month of a cash-flow projection.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CashFlowRow {
    month: u32,
    inflow: f64,
    outflow: f64,
    net: f64,
    balance: f64,
}

/// Cash-flow projection: monthly inflows and outflows over N months,
/// with optional monthly growth rates for each side.
pub(crate) fn compute_cash_flow(input: &CashFlowInput) -> UnifiedResult<Value> {
    let starting = require_finite(input.starting_balance, "starting_balance")?;
    let inflow = require_non_negative(input.monthly_inflow, "monthly_inflow")?;
    let outflow = require_non_negative(input.monthly_outflow, "monthly_outflow")?;
    let inflow_growth = require_finite(
        input.inflow_growth_percent.unwrap_or(0.0),
        "inflow_growth_percent",
    )?;
    let outflow_growth = require_finite(
        input.outflow_growth_percent.unwrap_or(0.0),
        "outflow_growth_percent",
    )?;
    if inflow_growth < -100.0 || outflow_growth < -100.0 {
        return Err(UnifiedError::bad_request(
            "growth rates must not be below -100 percent",
        ));
    }
    // Monthly growth above 1000 percent is not a plausible
    // projection and overflows the compounding power almost
    // immediately (for example, 1e9 percent over a few months
    // overflows to infinity, which JSON cannot represent and
    // serde_json would silently serialize as null).
    if inflow_growth > 1000.0 || outflow_growth > 1000.0 {
        return Err(UnifiedError::bad_request(
            "growth rates must not exceed 1000 percent",
        ));
    }
    if input.months < 1 || input.months > MAX_PROJECTION_MONTHS {
        return Err(UnifiedError::bad_request(format!(
            "months must be between 1 and {MAX_PROJECTION_MONTHS}"
        )));
    }
    let mut balance = starting;
    let mut rows: Vec<CashFlowRow> = Vec::with_capacity(input.months as usize);
    let mut total_inflow = 0.0;
    let mut total_outflow = 0.0;
    let mut lowest_balance = starting;
    for month in 1..=input.months {
        let month_inflow = inflow * (1.0 + inflow_growth / 100.0).powf((month - 1) as f64);
        let month_outflow = outflow * (1.0 + outflow_growth / 100.0).powf((month - 1) as f64);
        let net = month_inflow - month_outflow;
        balance += net;
        // Inputs are checked finite, but the computed values are
        // not: even a moderate growth rate compounds to infinity
        // over enough months (or with large enough amounts), and
        // an infinite value would serialize as null in the JSON
        // response. Reject the projection instead of returning
        // silently corrupted numbers.
        if !month_inflow.is_finite()
            || !month_outflow.is_finite()
            || !net.is_finite()
            || !balance.is_finite()
        {
            return Err(UnifiedError::bad_request(
                "cash-flow projection overflowed: reduce the growth rates, amounts, or months",
            ));
        }
        total_inflow += month_inflow;
        total_outflow += month_outflow;
        if !total_inflow.is_finite() || !total_outflow.is_finite() {
            return Err(UnifiedError::bad_request(
                "cash-flow projection overflowed: reduce the growth rates, amounts, or months",
            ));
        }
        if balance < lowest_balance {
            lowest_balance = balance;
        }
        rows.push(CashFlowRow {
            month,
            inflow: round_money(month_inflow),
            outflow: round_money(month_outflow),
            net: round_money(net),
            balance: round_money(balance),
        });
    }
    Ok(json!({
        "starting_balance": round_money(starting),
        "monthly_inflow": round_money(inflow),
        "monthly_outflow": round_money(outflow),
        "months": input.months,
        "inflow_growth_percent": round_ratio(inflow_growth),
        "outflow_growth_percent": round_ratio(outflow_growth),
        "total_inflow": round_money(total_inflow),
        "total_outflow": round_money(total_outflow),
        "total_net": round_money(total_inflow - total_outflow),
        "ending_balance": round_money(balance),
        "lowest_balance": round_money(lowest_balance),
        "projection": rows,
    }))
}

/// Pricing, margin, and markup: any two of price, cost, and margin
/// solve the third. Margin is a percent of price and must stay
/// below 100.
pub(crate) fn compute_pricing(input: &PricingInput) -> UnifiedResult<Value> {
    let price = match input.price {
        Some(value) => Some(require_positive(value, "price")?),
        None => None,
    };
    let cost = match input.cost {
        Some(value) => Some(require_non_negative(value, "cost")?),
        None => None,
    };
    let margin = match input.margin_percent {
        Some(value) => {
            let value = require_finite(value, "margin_percent")?;
            if !(0.0..100.0).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "margin_percent must be between 0 (inclusive) and 100 (exclusive)",
                ));
            }
            Some(value)
        }
        None => None,
    };
    let provided = [price.is_some(), cost.is_some(), margin.is_some()]
        .iter()
        .filter(|present| **present)
        .count();
    if provided != 2 {
        return Err(UnifiedError::bad_request(
            "Provide exactly two of price, cost, and margin_percent; the third is solved",
        ));
    }
    let (price, cost, margin) = match (price, cost, margin) {
        (Some(price), Some(cost), None) => {
            if price < cost {
                return Err(UnifiedError::bad_request(
                    "price must not be below cost when solving for margin",
                ));
            }
            let margin = (price - cost) / price * 100.0;
            (price, cost, margin)
        }
        (Some(price), None, Some(margin)) => {
            let cost = price * (1.0 - margin / 100.0);
            (price, cost, margin)
        }
        (None, Some(cost), Some(margin)) => {
            let price = cost / (1.0 - margin / 100.0);
            (price, cost, margin)
        }
        _ => {
            return Err(UnifiedError::bad_request(
                "Provide exactly two of price, cost, and margin_percent; the third is solved",
            ));
        }
    };
    let markup = if cost == 0.0 {
        0.0
    } else {
        (price - cost) / cost * 100.0
    };
    Ok(json!({
        "price": round_money(price),
        "cost": round_money(cost),
        "margin_percent": round_ratio(margin),
        "markup_percent": round_ratio(markup),
        "gross_profit": round_money(price - cost),
    }))
}

/// A contribution or deduction rate in percent of gross pay.
/// Rates are capped at 100 percent: an employer cannot contribute
/// more than the gross itself, and an employee deduction above
/// 100 percent would make net pay negative.
fn require_percent_rate(value: f64, field: &str) -> UnifiedResult<f64> {
    let value = require_non_negative(value, field)?;
    if value > 100.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be between 0 and 100 percent"
        )));
    }
    Ok(value)
}

/// Salary and payroll cost: gross pay plus employer contributions
/// and employee deductions, for a monthly or annual period.
pub(crate) fn compute_salary(input: &SalaryInput) -> UnifiedResult<Value> {
    let gross = require_positive(input.gross, "gross")?;
    let employer_rate = require_percent_rate(
        input.employer_contribution_percent,
        "employer_contribution_percent",
    )?;
    let employee_rate = require_percent_rate(
        input.employee_deduction_percent.unwrap_or(0.0),
        "employee_deduction_percent",
    )?;
    if !matches!(input.period.as_str(), "monthly" | "annual") {
        return Err(UnifiedError::bad_request(
            "period must be one of: monthly, annual",
        ));
    }
    let employer_contribution = gross * employer_rate / 100.0;
    let employee_deduction = gross * employee_rate / 100.0;
    let total_employer_cost = gross + employer_contribution;
    let net_pay = gross - employee_deduction;
    // Belt and braces: the 100 percent cap above already keeps net
    // pay non-negative, but never report a negative net pay.
    if net_pay < 0.0 {
        return Err(UnifiedError::bad_request(
            "employee_deduction_percent must not make net_pay negative",
        ));
    }
    let (monthly_gross, annual_gross) = if input.period == "monthly" {
        (gross, gross * 12.0)
    } else {
        (gross / 12.0, gross)
    };
    Ok(json!({
        "gross": round_money(gross),
        "period": input.period,
        "employer_contribution_percent": round_ratio(employer_rate),
        "employer_contribution": round_money(employer_contribution),
        "total_employer_cost": round_money(total_employer_cost),
        "employee_deduction_percent": round_ratio(employee_rate),
        "employee_deduction": round_money(employee_deduction),
        "net_pay": round_money(net_pay),
        "monthly_gross": round_money(monthly_gross),
        "annual_gross": round_money(annual_gross),
        "monthly_total_employer_cost": round_money(if input.period == "monthly" { total_employer_cost } else { total_employer_cost / 12.0 }),
        "annual_total_employer_cost": round_money(if input.period == "annual" { total_employer_cost } else { total_employer_cost * 12.0 }),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roi_computes_percent_and_net_gain() {
        let out = compute_roi(&RoiInput { gain: 150.0, cost: 100.0 }).expect("roi computes");
        assert_eq!(out["roi_percent"].as_f64().unwrap(), 50.0);
        assert_eq!(out["net_gain"].as_f64().unwrap(), 50.0);
    }

    #[test]
    fn roi_rejects_zero_cost() {
        assert!(compute_roi(&RoiInput { gain: 10.0, cost: 0.0 }).is_err());
    }

    #[test]
    fn roi_rejects_negative_gain() {
        assert!(compute_roi(&RoiInput { gain: -1.0, cost: 10.0 }).is_err());
    }

    #[test]
    fn break_even_computes_units_and_revenue() {
        let out = compute_break_even(&BreakEvenInput {
            fixed_costs: 1000.0,
            price: 10.0,
            variable_cost: 6.0,
        })
        .expect("break-even computes");
        assert_eq!(out["break_even_units"].as_f64().unwrap(), 250.0);
        assert_eq!(out["break_even_revenue"].as_f64().unwrap(), 2500.0);
        assert_eq!(out["contribution_margin_per_unit"].as_f64().unwrap(), 4.0);
    }

    #[test]
    fn break_even_rejects_price_at_or_below_variable_cost() {
        assert!(compute_break_even(&BreakEvenInput {
            fixed_costs: 100.0,
            price: 5.0,
            variable_cost: 5.0,
        })
        .is_err());
        assert!(compute_break_even(&BreakEvenInput {
            fixed_costs: 100.0,
            price: 4.0,
            variable_cost: 5.0,
        })
        .is_err());
    }

    #[test]
    fn loan_zero_rate_splits_principal_evenly() {
        let out = compute_loan(&LoanInput {
            principal: 12000.0,
            annual_rate: 0.0,
            term_months: 12,
        })
        .expect("loan computes");
        assert_eq!(out["monthly_payment"].as_f64().unwrap(), 1000.0);
        assert_eq!(out["total_interest"].as_f64().unwrap(), 0.0);
        assert_eq!(out["total_paid"].as_f64().unwrap(), 12000.0);
        assert_eq!(out["schedule"].as_array().unwrap().len(), 12);
        assert_eq!(out["schedule"][11]["balance"].as_f64().unwrap(), 0.0);
    }

    #[test]
    fn loan_with_rate_charges_interest_and_clears_balance() {
        let out = compute_loan(&LoanInput {
            principal: 12000.0,
            annual_rate: 12.0,
            term_months: 12,
        })
        .expect("loan computes");
        let payment = out["monthly_payment"].as_f64().unwrap();
        assert!(payment > 1000.0 && payment < 1100.0);
        assert!(out["total_interest"].as_f64().unwrap() > 0.0);
        assert_eq!(out["schedule"][11]["balance"].as_f64().unwrap(), 0.0);
    }

    #[test]
    fn loan_rejects_zero_term() {
        assert!(compute_loan(&LoanInput {
            principal: 1000.0,
            annual_rate: 5.0,
            term_months: 0,
        })
        .is_err());
    }

    #[test]
    fn depreciation_straight_line_is_even() {
        let out = compute_depreciation(&DepreciationInput {
            cost: 10000.0,
            salvage_value: 1000.0,
            useful_life_years: 5,
            method: "straight_line".to_string(),
            multiplier: None,
        })
        .expect("depreciation computes");
        let schedule = out["schedule"].as_array().unwrap();
        assert_eq!(schedule.len(), 5);
        assert_eq!(schedule[0]["depreciation"].as_f64().unwrap(), 1800.0);
        assert_eq!(schedule[4]["book_value"].as_f64().unwrap(), 1000.0);
    }

    #[test]
    fn depreciation_declining_balance_front_loads() {
        let out = compute_depreciation(&DepreciationInput {
            cost: 10000.0,
            salvage_value: 0.0,
            useful_life_years: 5,
            method: "declining_balance".to_string(),
            multiplier: None,
        })
        .expect("depreciation computes");
        let schedule = out["schedule"].as_array().unwrap();
        assert_eq!(schedule[0]["depreciation"].as_f64().unwrap(), 4000.0);
        assert!(schedule[1]["depreciation"].as_f64().unwrap() < 4000.0);
    }

    #[test]
    fn depreciation_rejects_unknown_method_and_bad_life() {
        assert!(compute_depreciation(&DepreciationInput {
            cost: 10000.0,
            salvage_value: 0.0,
            useful_life_years: 5,
            method: "sideways".to_string(),
            multiplier: None,
        })
        .is_err());
        assert!(compute_depreciation(&DepreciationInput {
            cost: 1000.0,
            salvage_value: 1000.0,
            useful_life_years: 5,
            method: "straight_line".to_string(),
            multiplier: None,
        })
        .is_err());
    }

    #[test]
    fn npv_at_rate_zero_discount_sums_flows() {
        assert_eq!(npv_at_rate(1000.0, &[600.0, 600.0], 0.0), 200.0);
    }

    #[test]
    fn npv_irr_reports_npv_and_irr() {
        let out = compute_npv_irr(&NpvIrrInput {
            initial_investment: 1000.0,
            cash_flows: vec![600.0, 600.0],
            discount_rate: 10.0,
        })
        .expect("npv/irr computes");
        let npv = out["npv"].as_f64().unwrap();
        assert!((npv - 41.3223).abs() < 0.001);
        let irr = out["irr_percent"].as_f64().unwrap();
        assert!(irr > 10.0 && irr < 20.0);
    }

    #[test]
    fn cash_flow_projects_balance() {
        let out = compute_cash_flow(&CashFlowInput {
            starting_balance: 100.0,
            monthly_inflow: 50.0,
            monthly_outflow: 20.0,
            months: 2,
            inflow_growth_percent: None,
            outflow_growth_percent: None,
        })
        .expect("cash flow computes");
        assert_eq!(out["ending_balance"].as_f64().unwrap(), 160.0);
        assert_eq!(out["total_inflow"].as_f64().unwrap(), 100.0);
        assert_eq!(out["total_net"].as_f64().unwrap(), 60.0);
        assert_eq!(out["projection"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn pricing_solves_margin_and_cost() {
        let out = compute_pricing(&PricingInput {
            price: Some(100.0),
            cost: Some(60.0),
            margin_percent: None,
        })
        .expect("pricing computes");
        assert_eq!(out["margin_percent"].as_f64().unwrap(), 40.0);
        assert_eq!(out["gross_profit"].as_f64().unwrap(), 40.0);
        let solved = compute_pricing(&PricingInput {
            price: Some(100.0),
            cost: None,
            margin_percent: Some(25.0),
        })
        .expect("pricing solves cost");
        assert_eq!(solved["cost"].as_f64().unwrap(), 75.0);
    }

    #[test]
    fn pricing_rejects_wrong_number_of_inputs() {
        assert!(compute_pricing(&PricingInput {
            price: Some(100.0),
            cost: None,
            margin_percent: None,
        })
        .is_err());
        assert!(compute_pricing(&PricingInput {
            price: Some(50.0),
            cost: Some(60.0),
            margin_percent: None,
        })
        .is_err());
    }

    #[test]
    fn salary_splits_monthly_and_annual() {
        let out = compute_salary(&SalaryInput {
            gross: 1000.0,
            employer_contribution_percent: 10.0,
            employee_deduction_percent: Some(5.0),
            period: "monthly".to_string(),
        })
        .expect("salary computes");
        assert_eq!(out["employer_contribution"].as_f64().unwrap(), 100.0);
        assert_eq!(out["total_employer_cost"].as_f64().unwrap(), 1100.0);
        assert_eq!(out["net_pay"].as_f64().unwrap(), 950.0);
        assert_eq!(out["annual_gross"].as_f64().unwrap(), 12000.0);
    }

    #[test]
    fn salary_rejects_unknown_period() {
        assert!(compute_salary(&SalaryInput {
            gross: 1000.0,
            employer_contribution_percent: 0.0,
            employee_deduction_percent: None,
            period: "weekly".to_string(),
        })
        .is_err());
    }

    #[test]
    fn salary_rejects_rates_above_100_percent() {
        // A 500 percent employer rate was previously accepted.
        assert!(compute_salary(&SalaryInput {
            gross: 1000.0,
            employer_contribution_percent: 500.0,
            employee_deduction_percent: None,
            period: "monthly".to_string(),
        })
        .is_err());
        // A deduction above 100 percent made net pay negative.
        assert!(compute_salary(&SalaryInput {
            gross: 1000.0,
            employer_contribution_percent: 0.0,
            employee_deduction_percent: Some(500.0),
            period: "monthly".to_string(),
        })
        .is_err());
        assert!(compute_salary(&SalaryInput {
            gross: 1000.0,
            employer_contribution_percent: 101.0,
            employee_deduction_percent: None,
            period: "monthly".to_string(),
        })
        .is_err());
        assert!(compute_salary(&SalaryInput {
            gross: 1000.0,
            employer_contribution_percent: 0.0,
            employee_deduction_percent: Some(100.01),
            period: "monthly".to_string(),
        })
        .is_err());
    }

    #[test]
    fn salary_accepts_100_percent_rates_with_zero_net_pay() {
        let out = compute_salary(&SalaryInput {
            gross: 1000.0,
            employer_contribution_percent: 100.0,
            employee_deduction_percent: Some(100.0),
            period: "monthly".to_string(),
        })
        .expect("100 percent rates are the cap, not an error");
        assert_eq!(out["employer_contribution"].as_f64().unwrap(), 1000.0);
        assert_eq!(out["total_employer_cost"].as_f64().unwrap(), 2000.0);
        assert_eq!(out["net_pay"].as_f64().unwrap(), 0.0);
    }

    #[test]
    fn cash_flow_rejects_extreme_growth_rates() {
        // 1e9 percent growth overflowed powf to infinity, which
        // serde_json would silently serialize as null.
        assert!(compute_cash_flow(&CashFlowInput {
            starting_balance: 0.0,
            monthly_inflow: 100.0,
            monthly_outflow: 0.0,
            months: 12,
            inflow_growth_percent: Some(1e9),
            outflow_growth_percent: None,
        })
        .is_err());
        assert!(compute_cash_flow(&CashFlowInput {
            starting_balance: 0.0,
            monthly_inflow: 0.0,
            monthly_outflow: 100.0,
            months: 12,
            inflow_growth_percent: None,
            outflow_growth_percent: Some(1e9),
        })
        .is_err());
    }

    #[test]
    fn cash_flow_rejects_projection_that_overflows() {
        // Even a permitted growth rate overflows with a large
        // enough amount: the computed values must be rejected,
        // not returned as nulls.
        assert!(compute_cash_flow(&CashFlowInput {
            starting_balance: 0.0,
            monthly_inflow: 1e308,
            monthly_outflow: 0.0,
            months: 120,
            inflow_growth_percent: Some(100.0),
            outflow_growth_percent: None,
        })
        .is_err());
    }

    #[test]
    fn cash_flow_with_ordinary_growth_returns_finite_numbers() {
        let out = compute_cash_flow(&CashFlowInput {
            starting_balance: 100.0,
            monthly_inflow: 50.0,
            monthly_outflow: 20.0,
            months: 12,
            inflow_growth_percent: Some(5.0),
            outflow_growth_percent: Some(2.0),
        })
        .expect("ordinary growth computes");
        for key in ["total_inflow", "total_outflow", "total_net", "ending_balance"] {
            assert!(out[key].as_f64().is_some(), "{key} must be a number, not null");
            assert!(out[key].as_f64().unwrap().is_finite());
        }
        assert!(out["ending_balance"].as_f64().unwrap() > 100.0);
    }

    #[test]
    fn loan_totals_agree_with_rounded_schedule() {
        let out = compute_loan(&LoanInput {
            principal: 10000.0,
            annual_rate: 7.5,
            term_months: 36,
        })
        .expect("loan computes");
        let principal = out["principal"].as_f64().unwrap();
        let total_paid = out["total_paid"].as_f64().unwrap();
        let total_interest = out["total_interest"].as_f64().unwrap();
        let schedule = out["schedule"].as_array().unwrap();
        let schedule_paid: f64 = schedule
            .iter()
            .map(|row| row["payment"].as_f64().unwrap())
            .sum();
        let schedule_interest: f64 = schedule
            .iter()
            .map(|row| row["interest"].as_f64().unwrap())
            .sum();
        // total_paid is exactly the sum of the displayed payments.
        assert!((total_paid - schedule_paid).abs() < 0.0001);
        // total_paid - principal agrees with total_interest exactly.
        assert!((total_paid - principal - total_interest).abs() < 0.0001);
        assert!(total_interest >= 0.0);
        // And total_interest stays within rounding tolerance of the
        // sum of the displayed interest rows.
        assert!((total_interest - schedule_interest).abs() < 0.01);
    }
}
