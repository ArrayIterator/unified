// Diesel models for the tax_subscriptions domain of the unified-db-commerce bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, NaiveDate, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Tax Management (2026-11-22-000038_tax). Money structs carry no serde
// derives (PgNumeric has none); the notes' `New*` snippets show
// `Deserialize` on money structs — dropped here per the repo money
// convention.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = tax_rules)]
pub struct TaxRule {
    pub id: Uuid,
    pub company_id: Uuid,
    pub country_code: String,
    pub region_code: Option<String>,
    pub name: String,
    pub tax_type: String,
    pub rate_percent: PgNumeric,
    pub threshold_amount: Option<PgNumeric>,
    pub currency: String,
    pub inclusive: bool,
    pub compound: bool,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub sort_order: i32,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = tax_rules)]
pub struct NewTaxRule {
    pub id: Uuid,
    pub company_id: Uuid,
    pub country_code: String,
    pub region_code: Option<String>,
    pub name: String,
    pub tax_type: String,
    pub rate_percent: PgNumeric,
    pub threshold_amount: Option<PgNumeric>,
    pub currency: String,
    pub inclusive: bool,
    pub compound: bool,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub sort_order: i32,
    pub enabled: bool,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = tax_settings)]
pub struct TaxSettings {
    pub id: Uuid,
    pub company_id: Uuid,
    pub fiscal_year_start_month: i32,
    pub fiscal_year_start_day: i32,
    pub filing_frequency: String,
    pub default_currency: String,
    pub default_country_code: Option<String>,
    pub default_region_code: Option<String>,
    pub reminder_days_before: i32,
    pub filing_due_days_after_period: i32,
    pub notify_user_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = tax_settings)]
pub struct NewTaxSettings {
    pub id: Uuid,
    pub company_id: Uuid,
    pub fiscal_year_start_month: i32,
    pub fiscal_year_start_day: i32,
    pub filing_frequency: String,
    pub default_currency: String,
    pub default_country_code: Option<String>,
    pub default_region_code: Option<String>,
    pub reminder_days_before: i32,
    pub filing_due_days_after_period: i32,
    pub notify_user_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = tax_computations)]
pub struct TaxComputation {
    pub id: Uuid,
    pub company_id: Uuid,
    pub source_type: String,
    pub source_id: String,
    pub country_code: Option<String>,
    pub region_code: Option<String>,
    pub currency: String,
    pub base_amount: PgNumeric,
    pub tax_total: PgNumeric,
    pub grand_total: PgNumeric,
    pub lines: Value,
    pub computed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = tax_computations)]
pub struct NewTaxComputation {
    pub id: Uuid,
    pub company_id: Uuid,
    pub source_type: String,
    pub source_id: String,
    pub country_code: Option<String>,
    pub region_code: Option<String>,
    pub currency: String,
    pub base_amount: PgNumeric,
    pub tax_total: PgNumeric,
    pub grand_total: PgNumeric,
    pub lines: Value,
    pub computed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = tax_filings)]
pub struct TaxFiling {
    pub id: Uuid,
    pub company_id: Uuid,
    pub country_code: String,
    pub region_code: Option<String>,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub frequency: String,
    pub due_date: NaiveDate,
    pub status: String,
    pub tax_total: PgNumeric,
    pub submitted_at: Option<DateTime<Utc>>,
    pub submitted_via: Option<String>,
    pub reference: Option<String>,
    pub last_reminder_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = tax_filings)]
pub struct NewTaxFiling {
    pub id: Uuid,
    pub company_id: Uuid,
    pub country_code: String,
    pub region_code: Option<String>,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub frequency: String,
    pub due_date: NaiveDate,
    pub status: String,
    pub tax_total: PgNumeric,
    pub submitted_at: Option<DateTime<Utc>>,
    pub submitted_via: Option<String>,
    pub reference: Option<String>,
    pub last_reminder_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = tax_connectors)]
pub struct TaxConnector {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub kind: String,
    pub endpoint_url: String,
    pub allowed_hosts: Value,
    pub auth_scheme: String,
    #[serde(skip_serializing)]
    pub credential_encrypted: Option<String>,
    pub credential_fingerprint: Option<String>,
    #[serde(skip_serializing)]
    pub signing_secret_encrypted: Option<String>,
    pub signing_secret_fingerprint: Option<String>,
    pub timeout_ms: i32,
    pub max_retries: i32,
    pub enabled: bool,
    pub request_mapping: Value,
    pub response_mapping: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = tax_connectors)]
pub struct NewTaxConnector {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub kind: String,
    pub endpoint_url: String,
    pub allowed_hosts: Value,
    pub auth_scheme: String,
    pub credential_encrypted: Option<String>,
    pub credential_fingerprint: Option<String>,
    pub signing_secret_encrypted: Option<String>,
    pub signing_secret_fingerprint: Option<String>,
    pub timeout_ms: i32,
    pub max_retries: i32,
    pub enabled: bool,
    pub request_mapping: Value,
    pub response_mapping: Value,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = tax_audit)]
pub struct TaxAuditEntry {
    pub id: Uuid,
    pub company_id: Uuid,
    pub actor: String,
    pub action: String,
    pub resource_type: String,
    pub resource_id: String,
    pub details: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = tax_audit)]
pub struct NewTaxAuditEntry {
    pub id: Uuid,
    pub company_id: Uuid,
    pub actor: String,
    pub action: String,
    pub resource_type: String,
    pub resource_id: String,
    pub details: Value,
}

// ---------------------------------------------------------------------------
// Subscriptions (2026-11-22-000040_subscriptions). `NewSubscription`
// deliberately includes the defaulted business fields (see the notes:
// the writer must persist the values the store computed at
// activation; the SQL defaults would be wrong data). Money structs
// carry no serde derives (PgNumeric has none); the notes' `New*`
// snippet shows `Deserialize` on the money struct — dropped here.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = subscriptions)]
pub struct Subscription {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub product_id: String,
    pub tier_id: Option<String>,
    pub order_id: String,
    pub module_id: Option<String>,
    pub status: String,
    pub billing_period: String,
    pub price: PgNumeric,
    pub currency: String,
    pub trial_days: i32,
    pub trial_end: Option<DateTime<Utc>>,
    pub current_period_start: DateTime<Utc>,
    pub current_period_end: DateTime<Utc>,
    pub proration_policy: String,
    pub grace_period_days: i32,
    pub dunning_max_retries: i32,
    pub dunning_retry_interval_days: i32,
    pub cancel_at_period_end: bool,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub pending_product_id: Option<String>,
    pub pending_tier_id: Option<String>,
    pub pending_price: Option<PgNumeric>,
    pub pending_billing_period: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = subscriptions)]
pub struct NewSubscription {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub product_id: String,
    pub tier_id: Option<String>,
    pub order_id: String,
    pub module_id: Option<String>,
    pub status: String,
    pub billing_period: String,
    pub price: PgNumeric,
    pub currency: String,
    pub trial_days: i32,
    pub trial_end: Option<DateTime<Utc>>,
    pub current_period_start: DateTime<Utc>,
    pub current_period_end: DateTime<Utc>,
    pub proration_policy: String,
    pub grace_period_days: i32,
    pub dunning_max_retries: i32,
    pub dunning_retry_interval_days: i32,
    pub cancel_at_period_end: bool,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub pending_product_id: Option<String>,
    pub pending_tier_id: Option<String>,
    pub pending_price: Option<PgNumeric>,
    pub pending_billing_period: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, Deserialize)]
#[diesel(table_name = subscription_events)]
pub struct SubscriptionEvent {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub event: String,
    pub actor: Option<String>,
    pub reason: Option<String>,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = subscription_events)]
pub struct NewSubscriptionEvent {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub event: String,
    pub actor: Option<String>,
    pub reason: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, Deserialize)]
#[diesel(table_name = dunning_attempts)]
pub struct DunningAttempt {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub attempt_number: i32,
    pub reason: String,
    pub amount_minor: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = dunning_attempts)]
pub struct NewDunningAttempt {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub attempt_number: i32,
    pub reason: String,
    pub amount_minor: Option<i64>,
}
