// Diesel models for the affiliates domain of the unified-db-commerce bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Affiliates (migration 000044_affiliates).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = affiliates)]
pub struct Affiliate {
    pub id: Uuid,
    pub user_id: String,
    pub display_name: String,
    pub email: String,
    pub status: String,
    pub commission_type: String,
    pub commission_value: PgNumeric,
    pub currency: String,
    pub attribution_window_days: i32,
    pub company_id: Option<String>,
    pub payout_method: Option<String>,
    pub payout_reference: Option<String>,
    pub reviewed_by: Option<String>,
    pub review_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = affiliates)]
pub struct NewAffiliate {
    pub id: Uuid,
    pub user_id: String,
    pub display_name: String,
    pub email: String,
    pub company_id: Option<String>,
    pub payout_method: Option<String>,
    pub payout_reference: Option<String>,
    pub reviewed_by: Option<String>,
    pub review_reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = affiliate_commission_rules)]
pub struct AffiliateCommissionRule {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub product_id: String,
    pub commission_type: String,
    pub commission_value: PgNumeric,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = affiliate_commission_rules)]
pub struct NewAffiliateCommissionRule {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub product_id: String,
    pub commission_type: String,
    pub commission_value: PgNumeric,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = affiliate_links)]
pub struct AffiliateLink {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub code: String,
    pub campaign: Option<String>,
    pub product_id: Option<String>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = affiliate_links)]
pub struct NewAffiliateLink {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub code: String,
    pub campaign: Option<String>,
    pub product_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = affiliate_clicks)]
pub struct AffiliateClick {
    pub id: Uuid,
    pub link_id: Uuid,
    pub affiliate_id: Uuid,
    pub ip_hash: String,
    pub user_agent: Option<String>,
    pub referrer: Option<String>,
    pub flagged: bool,
    pub flag_reasons: Vec<String>,
    pub converted: bool,
    pub clicked_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = affiliate_clicks)]
pub struct NewAffiliateClick {
    pub id: Uuid,
    pub link_id: Uuid,
    pub affiliate_id: Uuid,
    pub ip_hash: String,
    pub user_agent: Option<String>,
    pub referrer: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = affiliate_conversions)]
pub struct AffiliateConversion {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub link_id: Option<Uuid>,
    pub click_id: Option<Uuid>,
    pub customer_user_id: String,
    pub kind: String,
    pub order_id: Option<String>,
    pub order_total: PgNumeric,
    pub currency: String,
    pub product_id: Option<String>,
    pub flagged: bool,
    pub flag_reasons: Vec<String>,
    pub attributed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = affiliate_conversions)]
pub struct NewAffiliateConversion {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub link_id: Option<Uuid>,
    pub click_id: Option<Uuid>,
    pub customer_user_id: String,
    pub kind: String,
    pub order_id: Option<String>,
    pub product_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = affiliate_commissions)]
pub struct AffiliateCommission {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub conversion_id: Uuid,
    pub order_id: Option<String>,
    pub product_id: Option<String>,
    pub commission_type: String,
    pub rate_or_amount: PgNumeric,
    pub amount: PgNumeric,
    pub currency: String,
    pub status: String,
    pub payout_id: Option<Uuid>,
    pub flagged: bool,
    pub fraud_flags: Vec<String>,
    pub reviewed_by: Option<String>,
    pub review_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = affiliate_commissions)]
pub struct NewAffiliateCommission {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub conversion_id: Uuid,
    pub order_id: Option<String>,
    pub product_id: Option<String>,
    pub commission_type: String,
    pub rate_or_amount: PgNumeric,
    pub amount: PgNumeric,
    pub payout_id: Option<Uuid>,
    pub reviewed_by: Option<String>,
    pub review_reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = affiliate_payouts)]
pub struct AffiliatePayout {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub commission_ids: Vec<Uuid>,
    pub total_amount: PgNumeric,
    pub currency: String,
    pub status: String,
    pub method: Option<String>,
    pub reference: Option<String>,
    pub requested_by: String,
    pub reviewed_by: Option<String>,
    pub review_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = affiliate_payouts)]
pub struct NewAffiliatePayout {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub total_amount: PgNumeric,
    pub method: Option<String>,
    pub reference: Option<String>,
    pub requested_by: String,
    pub reviewed_by: Option<String>,
    pub review_reason: Option<String>,
}
