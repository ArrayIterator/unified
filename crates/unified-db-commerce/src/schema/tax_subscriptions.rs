// Diesel schema for the tax_subscriptions domain of the unified-db-commerce bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Tax Management (2026-11-22-000038_tax).

diesel::table! {
    tax_rules (id) {
        id -> Uuid,
        company_id -> Uuid,
        country_code -> Varchar,
        region_code -> Nullable<Varchar>,
        name -> Varchar,
        tax_type -> Varchar,
        rate_percent -> Numeric,
        threshold_amount -> Nullable<Numeric>,
        currency -> Varchar,
        inclusive -> Bool,
        compound -> Bool,
        effective_from -> Date,
        effective_to -> Nullable<Date>,
        sort_order -> Int4,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    tax_settings (id) {
        id -> Uuid,
        company_id -> Uuid,
        fiscal_year_start_month -> Int4,
        fiscal_year_start_day -> Int4,
        filing_frequency -> Varchar,
        default_currency -> Varchar,
        default_country_code -> Nullable<Varchar>,
        default_region_code -> Nullable<Varchar>,
        reminder_days_before -> Int4,
        filing_due_days_after_period -> Int4,
        notify_user_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    tax_computations (id) {
        id -> Uuid,
        company_id -> Uuid,
        source_type -> Varchar,
        source_id -> Varchar,
        country_code -> Nullable<Varchar>,
        region_code -> Nullable<Varchar>,
        currency -> Varchar,
        base_amount -> Numeric,
        tax_total -> Numeric,
        grand_total -> Numeric,
        lines -> Jsonb,
        computed_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    tax_filings (id) {
        id -> Uuid,
        company_id -> Uuid,
        country_code -> Varchar,
        region_code -> Nullable<Varchar>,
        period_start -> Date,
        period_end -> Date,
        frequency -> Varchar,
        due_date -> Date,
        status -> Varchar,
        tax_total -> Numeric,
        submitted_at -> Nullable<Timestamptz>,
        submitted_via -> Nullable<Varchar>,
        reference -> Nullable<Varchar>,
        last_reminder_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    tax_connectors (id) {
        id -> Uuid,
        company_id -> Uuid,
        name -> Varchar,
        kind -> Varchar,
        endpoint_url -> Varchar,
        allowed_hosts -> Jsonb,
        auth_scheme -> Varchar,
        credential_encrypted -> Nullable<Text>,
        credential_fingerprint -> Nullable<Varchar>,
        signing_secret_encrypted -> Nullable<Text>,
        signing_secret_fingerprint -> Nullable<Varchar>,
        timeout_ms -> Int4,
        max_retries -> Int4,
        enabled -> Bool,
        request_mapping -> Jsonb,
        response_mapping -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    tax_audit (id) {
        id -> Uuid,
        company_id -> Uuid,
        actor -> Varchar,
        action -> Varchar,
        resource_type -> Varchar,
        resource_id -> Varchar,
        details -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// Subscriptions (2026-11-22-000040_subscriptions).

diesel::table! {
    subscriptions (id) {
        id -> Uuid,
        user_id -> Varchar,
        company_id -> Nullable<Uuid>,
        product_id -> Varchar,
        tier_id -> Nullable<Varchar>,
        order_id -> Varchar,
        module_id -> Nullable<Varchar>,
        status -> Varchar,
        billing_period -> Varchar,
        price -> Numeric,
        currency -> Varchar,
        trial_days -> Int4,
        trial_end -> Nullable<Timestamptz>,
        current_period_start -> Timestamptz,
        current_period_end -> Timestamptz,
        proration_policy -> Varchar,
        grace_period_days -> Int4,
        dunning_max_retries -> Int4,
        dunning_retry_interval_days -> Int4,
        cancel_at_period_end -> Bool,
        cancelled_at -> Nullable<Timestamptz>,
        pending_product_id -> Nullable<Varchar>,
        pending_tier_id -> Nullable<Varchar>,
        pending_price -> Nullable<Numeric>,
        pending_billing_period -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    subscription_events (id) {
        id -> Uuid,
        subscription_id -> Uuid,
        event -> Varchar,
        actor -> Nullable<Varchar>,
        reason -> Nullable<Varchar>,
        payload -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    dunning_attempts (id) {
        id -> Uuid,
        subscription_id -> Uuid,
        attempt_number -> Int4,
        reason -> Varchar,
        amount_minor -> Nullable<Int8>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
