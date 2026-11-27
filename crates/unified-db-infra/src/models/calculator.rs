// Diesel models for the calculator domain of the unified-db-infra bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// Calculator Enterprise (2026-11-22-000100_calculator).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = calculator_saved_calculations)]
pub struct CalculatorSavedCalculation {
    pub id: Uuid,
    pub company_id: Uuid,
    pub calculator_type: String,
    pub title: String,
    pub inputs: Value,
    pub results: Value,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = calculator_saved_calculations)]
pub struct NewCalculatorSavedCalculation {
    pub id: Uuid,
    pub company_id: Uuid,
    pub calculator_type: String,
    pub title: String,
    pub inputs: Value,
    pub results: Value,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = calculator_reports)]
pub struct CalculatorReport {
    pub id: Uuid,
    pub calculation_id: Uuid,
    pub company_id: Uuid,
    pub title: String,
    pub share_token: String,
    pub visibility: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = calculator_reports)]
pub struct NewCalculatorReport {
    pub id: Uuid,
    pub calculation_id: Uuid,
    pub company_id: Uuid,
    pub title: String,
    pub share_token: String,
    pub visibility: String,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = calculator_history)]
pub struct CalculatorHistory {
    pub id: Uuid,
    pub company_id: Uuid,
    pub calculator_type: String,
    pub inputs: Value,
    pub results: Value,
    pub actor: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = calculator_history)]
pub struct NewCalculatorHistory {
    pub id: Uuid,
    pub company_id: Uuid,
    pub calculator_type: String,
    pub inputs: Value,
    pub results: Value,
    pub actor: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = calculator_currency_rates)]
pub struct CalculatorCurrencyRate {
    pub id: Uuid,
    pub base_currency: String,
    pub target_currency: String,
    pub rate: f64,
    pub provider: String,
    pub fetched_at: DateTime<Utc>,
    pub max_age_seconds: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = calculator_currency_rates)]
pub struct NewCalculatorCurrencyRate {
    pub id: Uuid,
    pub base_currency: String,
    pub target_currency: String,
    pub rate: f64,
    pub provider: String,
    pub fetched_at: DateTime<Utc>,
    pub max_age_seconds: i32,
}
