// Diesel schema for the pricing_tables_marketplace_saas_domains domain of the unified-db-commerce bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Pricing Tables + Marketplace Builder (2026-11-22-000041_pricing_marketplace).
// Note: `pricing_tiers` here is the 000041 shape (pricing-table
// feature tiers); migration 000103_pricing creates a different
// `pricing_tiers` (volume quantity tiers) — see the header note.
// ---------------------------------------------------------------------------

diesel::table! {
    pricing_tables (id) {
        id -> Uuid,
        name -> Varchar,
        slug -> Varchar,
        description -> Text,
        currency -> Varchar,
        layout -> Varchar,
        theme_accent -> Varchar,
        status -> Varchar,
        yearly_discount_percent -> Numeric,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_tiers (id) {
        id -> Uuid,
        table_id -> Uuid,
        name -> Varchar,
        description -> Text,
        monthly_price -> Numeric,
        yearly_price -> Numeric,
        cta_label -> Varchar,
        cta_url -> Varchar,
        highlighted -> Bool,
        sort_order -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_tier_features (id) {
        id -> Uuid,
        tier_id -> Uuid,
        feature_text -> Varchar,
        included -> Bool,
        note -> Text,
        sort_order -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    marketplace_vendors (id) {
        id -> Uuid,
        user_id -> Varchar,
        company_id -> Nullable<Varchar>,
        company_name -> Varchar,
        slug -> Varchar,
        email -> Varchar,
        status -> Varchar,
        application_notes -> Text,
        review_reason -> Text,
        reviewed_by -> Nullable<Varchar>,
        reviewed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    marketplace_commission_rules (id) {
        id -> Uuid,
        scope -> Varchar,
        scope_value -> Varchar,
        rate_percent -> Numeric,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    marketplace_product_approvals (id) {
        id -> Uuid,
        product_id -> Varchar,
        vendor_id -> Uuid,
        category -> Nullable<Varchar>,
        status -> Varchar,
        reason -> Text,
        reviewed_by -> Nullable<Varchar>,
        reviewed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    marketplace_commissions (id) {
        id -> Uuid,
        order_id -> Varchar,
        vendor_id -> Uuid,
        product_id -> Varchar,
        order_total -> Numeric,
        rate_percent -> Numeric,
        commission_amount -> Numeric,
        currency -> Varchar,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    marketplace_payouts (id) {
        id -> Uuid,
        vendor_id -> Uuid,
        amount -> Numeric,
        currency -> Varchar,
        status -> Varchar,
        invoice_id -> Nullable<Varchar>,
        reference -> Varchar,
        requested_by -> Varchar,
        reviewed_by -> Nullable<Varchar>,
        review_reason -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// SaaS domains (migration 000072_saas_domains), domain verification
// (migration 000074_domain_verification), domain cert modes +
// Cloudflare (migration 000075_domain_cert_modes); batch 8 notes §3.
// The notes write bare `table!` and `BigInt`/`Integer`; normalised
// to the file's `diesel::table!` / `Int8` / `Int4` style.
// ---------------------------------------------------------------------------

diesel::table! {
    saas_domain_reservations (id) {
        id -> Uuid,
        domain -> Varchar,
        company_id -> Uuid,
        reserved_by -> Text,
        reserved_until -> Timestamptz,
        status -> Varchar,
        site_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    saas_domain_aliases (id) {
        id -> Uuid,
        domain -> Varchar,
        company_id -> Uuid,
        target_type -> Varchar,
        target_value -> Varchar,
        status -> Varchar,
        site_id -> Nullable<Uuid>,
        verified_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    saas_subdomain_allocations (id) {
        id -> Uuid,
        label -> Varchar,
        hostname -> Varchar,
        company_id -> Uuid,
        site_id -> Nullable<Uuid>,
        status -> Varchar,
        approved_by -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// SaaS product subdomains (batch 15 B2 notes §2).
// ---------------------------------------------------------------------------

diesel::table! {
    saas_product_subdomains (id) {
        id -> Uuid,
        company_id -> Uuid,
        product_id -> Varchar,
        package_id -> Nullable<Varchar>,
        label -> Varchar,
        host -> Varchar,
        custom_domain -> Nullable<Varchar>,
        site_id -> Nullable<Uuid>,
        status -> Varchar,
        claimed_by -> Text,
        claimed_at -> Timestamptz,
        verified_at -> Nullable<Timestamptz>,
        released_at -> Nullable<Timestamptz>,
        grace_until -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    saas_product_subdomain_events (id) {
        id -> Uuid,
        product_subdomain_id -> Uuid,
        event -> Varchar,
        actor -> Text,
        reason -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}
