// Diesel schema for the presets domain of the unified-db-platform bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

diesel::table! {
    presets (id) {
        id -> Uuid,
        slug -> Text,
        name -> Text,
        description -> Text,
        business_type -> Text,
        built_in -> Bool,
        archived -> Bool,
        settings -> Jsonb,
        steps -> Jsonb,
        created_by -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    preset_modules (id) {
        id -> Uuid,
        preset_id -> Uuid,
        module_id -> Text,
        required -> Bool,
        sort_order -> Int4,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    preset_wizard_states (id) {
        id -> Uuid,
        company_id -> Uuid,
        actor_id -> Text,
        preset_id -> Nullable<Uuid>,
        steps -> Jsonb,
        current_step -> Text,
        status -> Text,
        skipped_steps -> Jsonb,
        disabled_optional_modules -> Jsonb,
        settings -> Jsonb,
        application_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    preset_applications (id) {
        id -> Uuid,
        preset_id -> Nullable<Uuid>,
        preset_slug -> Text,
        company_id -> Uuid,
        actor_id -> Text,
        status -> Text,
        outcomes -> Jsonb,
        settings_applied -> Jsonb,
        created_at -> Timestamptz,
        completed_at -> Nullable<Timestamptz>,
    }
}
