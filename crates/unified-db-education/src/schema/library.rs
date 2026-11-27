// Diesel schema for the library domain of the unified-db-education bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Library (migration 000097_library).
// No FKs by design (see migration notes): no joinable!s.

diesel::table! {
    library_categories (id) {
        id -> Uuid,
        name -> Text,
        slug -> Text,
        description -> Nullable<Text>,
        parent_id -> Nullable<Uuid>,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    library_items (id) {
        id -> Uuid,
        title -> Text,
        subtitle -> Nullable<Text>,
        authors -> Jsonb,
        isbn -> Nullable<Text>,
        isbn_normalized -> Nullable<Text>,
        edition -> Nullable<Text>,
        series -> Nullable<Text>,
        series_position -> Nullable<Int8>,
        publisher -> Nullable<Text>,
        published_year -> Nullable<Int8>,
        language -> Text,
        category_id -> Nullable<Uuid>,
        category_name -> Nullable<Text>,
        item_type -> Text,
        status -> Text,
        description -> Nullable<Text>,
        cover_ref -> Nullable<Text>,
        digital_ref -> Nullable<Text>,
        document_ref -> Nullable<Text>,
        storage_ref -> Nullable<Text>,
        tags -> Jsonb,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    library_copies (id) {
        id -> Uuid,
        item_id -> Uuid,
        barcode -> Text,
        serial -> Nullable<Text>,
        condition -> Text,
        location -> Nullable<Text>,
        shelf -> Nullable<Text>,
        status -> Text,
        notes -> Nullable<Text>,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    library_members (id) {
        id -> Uuid,
        user_id -> Text,
        member_type -> Text,
        status -> Text,
        joined_at -> Timestamptz,
        expires_at -> Nullable<Timestamptz>,
        notes -> Nullable<Text>,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    library_loans (id) {
        id -> Uuid,
        member_id -> Uuid,
        user_id -> Text,
        item_id -> Uuid,
        copy_id -> Nullable<Uuid>,
        status -> Text,
        checked_out_at -> Timestamptz,
        due_at -> Timestamptz,
        returned_at -> Nullable<Timestamptz>,
        renewal_count -> Int8,
        last_reminded_at -> Nullable<Timestamptz>,
        last_overdue_noticed_at -> Nullable<Timestamptz>,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    library_holds (id) {
        id -> Uuid,
        member_id -> Uuid,
        user_id -> Text,
        item_id -> Uuid,
        copy_id -> Nullable<Uuid>,
        status -> Text,
        position -> Int8,
        ready_at -> Nullable<Timestamptz>,
        expires_at -> Nullable<Timestamptz>,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    library_fines (id) {
        id -> Uuid,
        loan_id -> Uuid,
        member_id -> Uuid,
        user_id -> Text,
        amount_minor -> Numeric,
        currency -> Text,
        reason -> Text,
        status -> Text,
        waived_by -> Nullable<Text>,
        paid_reference -> Nullable<Text>,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    library_loan_policies (id) {
        id -> Uuid,
        name -> Text,
        member_type -> Text,
        item_type -> Text,
        loan_days -> Int8,
        max_concurrent -> Int8,
        max_renewals -> Int8,
        fine_per_day_minor -> Numeric,
        currency -> Text,
        grace_days -> Int8,
        active -> Bool,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    library_inventory_audits (id) {
        id -> Uuid,
        name -> Text,
        scope_location -> Nullable<Text>,
        scope_shelf -> Nullable<Text>,
        status -> Text,
        started_by -> Text,
        completed_at -> Nullable<Timestamptz>,
        items_checked -> Int8,
        discrepancies_found -> Int8,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    library_inventory_discrepancies (id) {
        id -> Uuid,
        audit_id -> Uuid,
        copy_id -> Uuid,
        item_id -> Uuid,
        kind -> Text,
        note -> Nullable<Text>,
        status -> Text,
        resolved_by -> Nullable<Text>,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    library_digital_access (id) {
        id -> Uuid,
        loan_id -> Uuid,
        item_id -> Uuid,
        member_id -> Uuid,
        user_id -> Text,
        expires_at -> Timestamptz,
        revoked -> Bool,
        redeemed_at -> Nullable<Timestamptz>,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
