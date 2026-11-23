// Diesel schema for the studio domain of the unified-db-platform bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Studio customizer (migration 000090_studio).
// ---------------------------------------------------------------------------

diesel::table! {
    studio_record_types (record_type) {
        record_type -> Text,
        module_id -> Text,
        label -> Text,
        description -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    studio_system_fields (id) {
        id -> Uuid,
        record_type -> Text,
        field_key -> Text,
        label -> Text,
        field_type -> Text,
        required -> Bool,
        hideable -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    studio_field_definitions (id) {
        id -> Uuid,
        company_id -> Text,
        record_type -> Text,
        field_key -> Text,
        label -> Text,
        field_type -> Text,
        version -> Int8,
        status -> Text,
        required -> Bool,
        default_value -> Nullable<Jsonb>,
        help_text -> Nullable<Text>,
        options -> Jsonb,
        validation -> Jsonb,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    studio_field_values (id) {
        id -> Uuid,
        company_id -> Text,
        record_type -> Text,
        record_id -> Text,
        field_key -> Text,
        definition_id -> Nullable<Uuid>,
        value_type -> Text,
        value -> Jsonb,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    studio_list_views (id) {
        id -> Uuid,
        user_id -> Text,
        company_id -> Text,
        record_type -> Text,
        name -> Text,
        #[sql_name = "columns"]
        columns_ -> Jsonb,
        filters -> Jsonb,
        sort_field -> Nullable<Text>,
        sort_order -> Nullable<Text>,
        group_by -> Nullable<Text>,
        visibility -> Text,
        scope -> Text,
        is_default -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    studio_form_layouts (id) {
        id -> Uuid,
        company_id -> Text,
        record_type -> Text,
        name -> Text,
        sections -> Jsonb,
        is_default -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
