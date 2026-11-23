// Diesel schema for the digital_products domain of the unified-db-commerce bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Digital products (migration 000078_digital_products).
// `DigitalProduct`, `DigitalPayoutRequest` and `DigitalRoyalty`
// carry NUMERIC columns, so they skip ALL serde derives per the
// repo header rule.
// ---------------------------------------------------------------------------

diesel::table! {
    digital_product_types (id) {
        id -> Text,
        label -> Text,
        description -> Text,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    digital_products (id) {
        id -> Uuid,
        product_id -> Text,
        type_id -> Text,
        author -> Nullable<Text>,
        publisher -> Nullable<Text>,
        edition -> Nullable<Text>,
        isbn -> Nullable<Text>,
        language -> Nullable<Text>,
        pages -> Nullable<Int4>,
        formats -> Array<Text>,
        cover_storage_id -> Nullable<Text>,
        sample_allowed -> Bool,
        download_limit -> Int4,
        royalty_kind -> Text,
        royalty_value -> Numeric,
        royalty_payee -> Nullable<Text>,
        royalty_payee_company_id -> Nullable<Text>,
        status -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    digital_product_files (id) {
        id -> Uuid,
        digital_product_id -> Uuid,
        storage_file_id -> Text,
        format -> Text,
        label -> Nullable<Text>,
        file_size_bytes -> Nullable<Int8>,
        checksum -> Nullable<Text>,
        is_primary -> Bool,
        sort_order -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    digital_product_previews (id) {
        id -> Uuid,
        digital_product_id -> Uuid,
        storage_file_id -> Text,
        format -> Text,
        label -> Nullable<Text>,
        allowed -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    digital_payout_requests (id) {
        id -> Uuid,
        payee -> Nullable<Text>,
        payee_company_id -> Nullable<Text>,
        currency -> Bpchar,
        amount -> Numeric,
        royalty_ids -> Array<Uuid>,
        status -> Text,
        requested_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    digital_royalties (id) {
        id -> Uuid,
        digital_product_id -> Uuid,
        product_id -> Text,
        order_id -> Text,
        buyer_user_id -> Text,
        payee -> Nullable<Text>,
        payee_company_id -> Nullable<Text>,
        amount -> Numeric,
        currency -> Bpchar,
        royalty_kind -> Text,
        royalty_value -> Numeric,
        status -> Text,
        payout_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    digital_downloads (id) {
        id -> Uuid,
        digital_product_id -> Uuid,
        file_id -> Nullable<Uuid>,
        preview_id -> Nullable<Uuid>,
        buyer_user_id -> Text,
        order_id -> Nullable<Text>,
        grant_id -> Nullable<Text>,
        kind -> Text,
        watermark_subject -> Nullable<Text>,
        watermark_applied -> Bool,
        watermark_formats -> Array<Text>,
        downloaded_at -> Timestamptz,
    }
}

diesel::table! {
    digital_watermark_formats (format) {
        format -> Text,
        metadata_supported -> Bool,
        metadata_fields -> Array<Text>,
        notes -> Text,
        updated_at -> Timestamptz,
    }
}
