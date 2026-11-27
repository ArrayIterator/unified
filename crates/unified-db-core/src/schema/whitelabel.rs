// Diesel schema for the whitelabel domain of the unified-db-core bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Whitelabel (migration 000073_whitelabel) + presets
// (migration 000071_presets); batch 9 notes sec. 4. The notes write bare
// `table!`; normalised to the file's `diesel::table!` style.

diesel::table! {
    whitelabel_global_settings (id) {
        id -> Varchar,
        allow_whitelabel -> Bool,
        updated_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    whitelabel_company_settings (company_id) {
        company_id -> Varchar,
        mode -> Varchar,
        updated_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    whitelabel_global_defaults (id) {
        id -> Varchar,
        display_name -> Varchar,
        logo_storage_target -> Nullable<Varchar>,
        logo_storage_key -> Nullable<Varchar>,
        favicon_storage_target -> Nullable<Varchar>,
        favicon_storage_key -> Nullable<Varchar>,
        primary_color -> Varchar,
        accent_color -> Varchar,
        background_color -> Varchar,
        text_color -> Varchar,
        sender_name -> Varchar,
        sender_email -> Varchar,
        login_tagline -> Varchar,
        login_background_storage_target -> Nullable<Varchar>,
        login_background_storage_key -> Nullable<Varchar>,
        powered_by_text -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    whitelabel_profiles (id) {
        id -> Uuid,
        company_id -> Varchar,
        tenant_id -> Nullable<Varchar>,
        display_name -> Nullable<Varchar>,
        logo_storage_target -> Nullable<Varchar>,
        logo_storage_key -> Nullable<Varchar>,
        favicon_storage_target -> Nullable<Varchar>,
        favicon_storage_key -> Nullable<Varchar>,
        primary_color -> Nullable<Varchar>,
        accent_color -> Nullable<Varchar>,
        background_color -> Nullable<Varchar>,
        text_color -> Nullable<Varchar>,
        custom_domain -> Nullable<Varchar>,
        domain_status -> Varchar,
        hide_powered_by -> Bool,
        sender_name -> Nullable<Varchar>,
        sender_email -> Nullable<Varchar>,
        login_logo_storage_target -> Nullable<Varchar>,
        login_logo_storage_key -> Nullable<Varchar>,
        login_background_storage_target -> Nullable<Varchar>,
        login_background_storage_key -> Nullable<Varchar>,
        login_tagline -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    whitelabel_history (id) {
        id -> Uuid,
        company_id -> Varchar,
        tenant_id -> Nullable<Varchar>,
        actor -> Varchar,
        action -> Varchar,
        field -> Varchar,
        old_value -> Nullable<Text>,
        new_value -> Nullable<Text>,
        allowed -> Bool,
        created_at -> Timestamptz,
    }
}
