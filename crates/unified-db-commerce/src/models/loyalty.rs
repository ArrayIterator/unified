// Diesel models for the loyalty domain of the unified-db-commerce bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Loyalty (migration 000045_loyalty).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = loyalty_programs)]
pub struct LoyaltyProgram {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub points_name: String,
    pub currency: String,
    pub discount_per_point: PgNumeric,
    pub active: bool,
    pub expiry_days: Option<i32>,
    pub expiry_mode: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = loyalty_programs)]
pub struct NewLoyaltyProgram {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub expiry_days: Option<i32>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = loyalty_earn_rules)]
pub struct LoyaltyEarnRule {
    pub id: Uuid,
    pub program_id: Uuid,
    pub kind: String,
    pub points_per_unit: PgNumeric,
    pub fixed_points: Option<i64>,
    pub cap_per_day: Option<i64>,
    pub cap_total: Option<i64>,
    pub min_order_amount: Option<PgNumeric>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = loyalty_earn_rules)]
pub struct NewLoyaltyEarnRule {
    pub id: Uuid,
    pub program_id: Uuid,
    pub kind: String,
    pub fixed_points: Option<i64>,
    pub cap_per_day: Option<i64>,
    pub cap_total: Option<i64>,
    pub min_order_amount: Option<PgNumeric>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = loyalty_tiers)]
pub struct LoyaltyTier {
    pub id: Uuid,
    pub program_id: Uuid,
    pub name: String,
    pub min_lifetime_points: i64,
    pub min_lifetime_spend: PgNumeric,
    pub benefits: Vec<String>,
    pub discount_percent: PgNumeric,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = loyalty_tiers)]
pub struct NewLoyaltyTier {
    pub id: Uuid,
    pub program_id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = loyalty_ledger)]
pub struct LoyaltyLedger {
    pub id: Uuid,
    pub program_id: Uuid,
    pub customer_id: String,
    pub kind: String,
    pub points: i64,
    pub balance_after: i64,
    pub reason: String,
    pub actor: String,
    pub earn_kind: Option<String>,
    pub reference_id: Option<String>,
    pub reference_amount: Option<PgNumeric>,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = loyalty_ledger)]
pub struct NewLoyaltyLedger {
    pub id: Uuid,
    pub program_id: Uuid,
    pub customer_id: String,
    pub kind: String,
    pub points: i64,
    pub balance_after: i64,
    pub reason: String,
    pub actor: String,
    pub earn_kind: Option<String>,
    pub reference_id: Option<String>,
    pub reference_amount: Option<PgNumeric>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = loyalty_redemptions)]
pub struct LoyaltyRedemption {
    pub id: Uuid,
    pub program_id: Uuid,
    pub customer_id: String,
    pub points_spent: i64,
    pub kind: String,
    pub discount_amount: PgNumeric,
    pub currency: String,
    pub product_id: Option<String>,
    pub order_id: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = loyalty_redemptions)]
pub struct NewLoyaltyRedemption {
    pub id: Uuid,
    pub program_id: Uuid,
    pub customer_id: String,
    pub points_spent: i64,
    pub kind: String,
    pub product_id: Option<String>,
    pub order_id: Option<String>,
}
