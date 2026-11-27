// Diesel schema for the forms domain of the unified-db-platform bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Forms (migration 000047_forms).

diesel::table! {
    forms (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Nullable<Text>,
        slug -> Varchar,
        status -> Varchar,
        company_id -> Nullable<Varchar>,
        owner_id -> Varchar,
        submit_button_label -> Varchar,
        success_message -> Varchar,
        honeypot_field -> Varchar,
        mapping -> Nullable<Jsonb>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    form_fields (id) {
        id -> Uuid,
        form_id -> Uuid,
        key -> Varchar,
        label -> Varchar,
        field_type -> Varchar,
        position -> Int8,
        required -> Bool,
        placeholder -> Nullable<Varchar>,
        help_text -> Nullable<Varchar>,
        options -> Array<Text>,
        min_length -> Nullable<Int8>,
        max_length -> Nullable<Int8>,
        min_value -> Nullable<Float8>,
        max_value -> Nullable<Float8>,
        pattern -> Nullable<Varchar>,
        show_when -> Nullable<Jsonb>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    form_submissions (id) {
        id -> Uuid,
        form_id -> Uuid,
        status -> Varchar,
        mapping_status -> Varchar,
        mapping_error -> Nullable<Text>,
        mapped_target_id -> Nullable<Varchar>,
        ip_address -> Nullable<Varchar>,
        user_agent -> Nullable<Varchar>,
        submitted_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    form_submission_values (id) {
        id -> Uuid,
        submission_id -> Uuid,
        field_id -> Uuid,
        field_key -> Varchar,
        value -> Jsonb,
        file_storage_key -> Nullable<Varchar>,
        created_at -> Timestamptz,
    }
}
