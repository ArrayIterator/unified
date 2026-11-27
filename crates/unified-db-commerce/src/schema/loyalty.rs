// Diesel schema for the loyalty domain of the unified-db-commerce bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Loyalty (migration 000045_loyalty). Every table here carries a
// NUMERIC column, so all five pairs skip ALL serde derives per the
// repo header rule.

diesel::table! {
    loyalty_programs (id) {
        id -> Uuid,
        company_id -> Nullable<Varchar>,
        name -> Varchar,
        description -> Nullable<Text>,
        points_name -> Varchar,
        currency -> Bpchar,
        discount_per_point -> Numeric,
        active -> Bool,
        expiry_days -> Nullable<Int4>,
        expiry_mode -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    loyalty_earn_rules (id) {
        id -> Uuid,
        program_id -> Uuid,
        kind -> Varchar,
        points_per_unit -> Numeric,
        fixed_points -> Nullable<Int8>,
        cap_per_day -> Nullable<Int8>,
        cap_total -> Nullable<Int8>,
        min_order_amount -> Nullable<Numeric>,
        active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    loyalty_tiers (id) {
        id -> Uuid,
        program_id -> Uuid,
        name -> Varchar,
        min_lifetime_points -> Int8,
        min_lifetime_spend -> Numeric,
        benefits -> Array<Text>,
        discount_percent -> Numeric,
        sort_order -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    loyalty_ledger (id) {
        id -> Uuid,
        program_id -> Uuid,
        customer_id -> Varchar,
        kind -> Varchar,
        points -> Int8,
        balance_after -> Int8,
        reason -> Text,
        actor -> Varchar,
        earn_kind -> Nullable<Varchar>,
        reference_id -> Nullable<Varchar>,
        reference_amount -> Nullable<Numeric>,
        expires_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    loyalty_redemptions (id) {
        id -> Uuid,
        program_id -> Uuid,
        customer_id -> Varchar,
        points_spent -> Int8,
        kind -> Varchar,
        discount_amount -> Numeric,
        currency -> Bpchar,
        product_id -> Nullable<Varchar>,
        order_id -> Nullable<Varchar>,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
