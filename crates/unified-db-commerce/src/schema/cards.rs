// Diesel schema for the cards domain of the unified-db-commerce bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Card validation (migration 000105_cards) + reference-id settings
// (batch 18 notes sec. 3b/sec. 3e; the sec. 3e `reference` columns for
// `store_orders` / `billing_invoices` are NOT added here - no
// migration creates them; see the header note).

diesel::table! {
    card_custom_rules (id) {
        id -> Uuid,
        tenant_id -> Text,
        name -> Text,
        brand_name -> Text,
        iin_prefixes -> Jsonb,
        iin_ranges -> Jsonb,
        allowed_lengths -> Jsonb,
        cvv_length -> Int2,
        luhn_required -> Bool,
        enabled -> Bool,
        overrides_standard_brand -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    card_validation_daily_counts (tenant_id, date, matched_rule, result) {
        tenant_id -> Text,
        date -> Date,
        matched_rule -> Text,
        result -> Text,
        count -> Int8,
        updated_at -> Timestamptz,
    }
}
