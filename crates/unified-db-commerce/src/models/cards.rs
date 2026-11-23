// Diesel models for the cards domain of the unified-db-commerce bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, NaiveDate, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Card validation + reference-id settings (batch 18 notes §3b/§3e).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = card_custom_rules)]
pub struct CardCustomRule {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub brand_name: String,
    pub iin_prefixes: Value,
    pub iin_ranges: Value,
    pub allowed_lengths: Value,
    pub cvv_length: i16,
    pub luhn_required: bool,
    pub enabled: bool,
    pub overrides_standard_brand: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = card_custom_rules)]
pub struct NewCardCustomRule {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub brand_name: String,
    pub overrides_standard_brand: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = card_validation_daily_counts)]
pub struct CardValidationDailyCount {
    pub tenant_id: String,
    pub date: NaiveDate,
    pub matched_rule: String,
    pub result: String,
    pub count: i64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = card_validation_daily_counts)]
pub struct NewCardValidationDailyCount {
    pub tenant_id: String,
    pub date: NaiveDate,
    pub matched_rule: String,
    pub result: String,
}
