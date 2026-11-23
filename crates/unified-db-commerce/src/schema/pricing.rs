// Diesel schema for the pricing domain of the unified-db-commerce bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Pricing engine (migration 000103_pricing); batch 15 B3 notes §6.2.
// The volume-quantity tiers live in `pricing_volume_tiers` below:
// the 000041 shape (pricing-table feature tiers) already owns the
// `pricing_tiers` name, and the 000103 table was renamed during
// the final inspection to resolve the collision.
// ---------------------------------------------------------------------------

diesel::table! {
    pricing_settings (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        rounding_mode -> Varchar,
        proration_basis -> Varchar,
        days_per_month -> Int8,
        approval_threshold_bp -> Int8,
        require_second_approver -> Bool,
        default_currency -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_currencies (code) {
        code -> Varchar,
        exponent -> Int8,
        active -> Bool,
        rate_numerator -> Nullable<Int8>,
        rate_denominator -> Nullable<Int8>,
        rate_fetched_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_rules (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        name -> Varchar,
        kind -> Varchar,
        percent_bp -> Int8,
        amount_scaled -> Numeric,
        currency -> Varchar,
        buy_quantity -> Int8,
        get_quantity -> Int8,
        min_quantity -> Int8,
        min_subtotal_scaled -> Numeric,
        product_ids -> Jsonb,
        evaluation_order -> Int8,
        stackable -> Bool,
        exclusive -> Bool,
        status -> Varchar,
        valid_from -> Nullable<Timestamptz>,
        valid_to -> Nullable<Timestamptz>,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_coupons (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        code -> Varchar,
        kind -> Varchar,
        percent_bp -> Int8,
        amount_scaled -> Numeric,
        currency -> Varchar,
        buy_quantity -> Int8,
        get_quantity -> Int8,
        product_ids -> Jsonb,
        valid_from -> Nullable<Timestamptz>,
        valid_to -> Nullable<Timestamptz>,
        usage_limit -> Nullable<Int8>,
        per_customer_limit -> Nullable<Int8>,
        used_count -> Int8,
        stackable -> Bool,
        exclusive -> Bool,
        active -> Bool,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_coupon_redemptions (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        coupon_id -> Uuid,
        customer_id -> Varchar,
        count -> Int8,
        last_redeemed_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_recommendations (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        product_id -> Varchar,
        kind -> Varchar,
        suggested_price_scaled -> Numeric,
        currency -> Varchar,
        lines -> Jsonb,
        inputs -> Jsonb,
        status -> Varchar,
        created_by -> Varchar,
        confirmed_by -> Nullable<Varchar>,
        price_version_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_cost_lines (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        recommendation_id -> Uuid,
        label -> Varchar,
        source -> Varchar,
        cost_scaled -> Numeric,
        currency -> Varchar,
    }
}

diesel::table! {
    pricing_volume_tiers (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        product_id -> Varchar,
        min_quantity -> Int8,
        max_quantity -> Nullable<Int8>,
        unit_price_scaled -> Numeric,
        currency -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_bundles (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        name -> Varchar,
        product_id -> Varchar,
        discount_kind -> Varchar,
        discount_percent_bp -> Int8,
        discount_amount_scaled -> Numeric,
        currency -> Varchar,
        active -> Bool,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_bundle_components (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        bundle_id -> Uuid,
        product_id -> Varchar,
        label -> Varchar,
        price_scaled -> Numeric,
        currency -> Varchar,
    }
}

diesel::table! {
    pricing_reference_prices (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        product_id -> Varchar,
        source -> Varchar,
        source_url -> Nullable<Varchar>,
        price_scaled -> Numeric,
        currency -> Varchar,
        observed_at -> Timestamptz,
        note -> Text,
        entered_by -> Varchar,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_price_versions (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        product_id -> Varchar,
        version -> Int8,
        price_scaled -> Numeric,
        currency -> Varchar,
        previous_price_scaled -> Numeric,
        change_bp -> Nullable<Int8>,
        above_threshold -> Bool,
        status -> Varchar,
        requested_by -> Varchar,
        approved_by -> Nullable<Varchar>,
        reason -> Nullable<Text>,
        recommendation_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_price_approvals (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        price_version_id -> Uuid,
        approver -> Varchar,
        decision -> Varchar,
        reason -> Nullable<Text>,
        decided_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_proration_records (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        subscription_id -> Nullable<Varchar>,
        customer_id -> Nullable<Varchar>,
        kind -> Varchar,
        basis -> Varchar,
        period_start -> Timestamptz,
        period_end -> Timestamptz,
        change_at -> Timestamptz,
        period_days -> Int8,
        days_used -> Int8,
        days_remaining -> Int8,
        old_daily_rate_scaled -> Numeric,
        new_daily_rate_scaled -> Numeric,
        unused_credit_scaled -> Numeric,
        new_charge_scaled -> Numeric,
        net_due_scaled -> Numeric,
        currency -> Varchar,
        lines -> Jsonb,
        created_by -> Varchar,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_credit_notes (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        number -> Varchar,
        invoice_id -> Varchar,
        amount_scaled -> Numeric,
        currency -> Varchar,
        lines -> Jsonb,
        reason -> Text,
        status -> Varchar,
        reconciled -> Bool,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_reconciliation_reports (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        invoice_id -> Nullable<Varchar>,
        matched -> Bool,
        currency -> Varchar,
        components -> Jsonb,
        total_discrepancy_scaled -> Numeric,
        checked_by -> Varchar,
        checked_at -> Timestamptz,
    }
}
