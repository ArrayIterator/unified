// Diesel schema for the affiliates domain of the unified-db-commerce bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Affiliates (migration 000044_affiliates). `Affiliate`,
// `AffiliateCommissionRule`, `AffiliateConversion`,
// `AffiliateCommission` and `AffiliatePayout` carry NUMERIC columns,
// so per the repo header rule they skip ALL serde derives.

diesel::table! {
    affiliates (id) {
        id -> Uuid,
        user_id -> Varchar,
        display_name -> Varchar,
        email -> Varchar,
        status -> Varchar,
        commission_type -> Varchar,
        commission_value -> Numeric,
        currency -> Bpchar,
        attribution_window_days -> Int4,
        company_id -> Nullable<Varchar>,
        payout_method -> Nullable<Varchar>,
        payout_reference -> Nullable<Varchar>,
        reviewed_by -> Nullable<Varchar>,
        review_reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    affiliate_commission_rules (id) {
        id -> Uuid,
        affiliate_id -> Uuid,
        product_id -> Varchar,
        commission_type -> Varchar,
        commission_value -> Numeric,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    affiliate_links (id) {
        id -> Uuid,
        affiliate_id -> Uuid,
        code -> Varchar,
        campaign -> Nullable<Varchar>,
        product_id -> Nullable<Varchar>,
        active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    affiliate_clicks (id) {
        id -> Uuid,
        link_id -> Uuid,
        affiliate_id -> Uuid,
        ip_hash -> Varchar,
        user_agent -> Nullable<Varchar>,
        referrer -> Nullable<Varchar>,
        flagged -> Bool,
        flag_reasons -> Array<Text>,
        converted -> Bool,
        clicked_at -> Timestamptz,
    }
}

diesel::table! {
    affiliate_conversions (id) {
        id -> Uuid,
        affiliate_id -> Uuid,
        link_id -> Nullable<Uuid>,
        click_id -> Nullable<Uuid>,
        customer_user_id -> Varchar,
        kind -> Varchar,
        order_id -> Nullable<Varchar>,
        order_total -> Numeric,
        currency -> Bpchar,
        product_id -> Nullable<Varchar>,
        flagged -> Bool,
        flag_reasons -> Array<Text>,
        attributed_at -> Timestamptz,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    affiliate_commissions (id) {
        id -> Uuid,
        affiliate_id -> Uuid,
        conversion_id -> Uuid,
        order_id -> Nullable<Varchar>,
        product_id -> Nullable<Varchar>,
        commission_type -> Varchar,
        rate_or_amount -> Numeric,
        amount -> Numeric,
        currency -> Bpchar,
        status -> Varchar,
        payout_id -> Nullable<Uuid>,
        flagged -> Bool,
        fraud_flags -> Array<Text>,
        reviewed_by -> Nullable<Varchar>,
        review_reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    affiliate_payouts (id) {
        id -> Uuid,
        affiliate_id -> Uuid,
        commission_ids -> Array<Uuid>,
        total_amount -> Numeric,
        currency -> Bpchar,
        status -> Varchar,
        method -> Nullable<Varchar>,
        reference -> Nullable<Varchar>,
        requested_by -> Varchar,
        reviewed_by -> Nullable<Varchar>,
        review_reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
