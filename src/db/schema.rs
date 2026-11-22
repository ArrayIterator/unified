// Diesel schema for the core tables.
//
// This schema matches `migrations/2026-11-22-000001_core/up.sql`.
// Modules that need persistence beyond their in-memory store use these
// tables; all other module data lives in the module's own store until a
// dedicated migration is added for it.

diesel::table! {
    users (id) {
        id -> Uuid,
        email -> Varchar,
        name -> Varchar,
        password_hash -> Varchar,
        totp_secret -> Nullable<Varchar>,
        totp_enabled -> Bool,
        is_active -> Bool,
        company_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    roles (id) {
        id -> Uuid,
        name -> Varchar,
        slug -> Varchar,
        description -> Nullable<Text>,
        is_system -> Bool,
        permissions -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    user_roles (user_id, role_id) {
        user_id -> Uuid,
        role_id -> Uuid,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    companies (id) {
        id -> Uuid,
        name -> Varchar,
        slug -> Varchar,
        is_active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    company_sites (id) {
        id -> Uuid,
        company_id -> Uuid,
        name -> Varchar,
        domain -> Varchar,
        is_primary -> Bool,
        is_active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    domains (id) {
        id -> Uuid,
        name -> Varchar,
        company_id -> Nullable<Uuid>,
        site_id -> Nullable<Uuid>,
        is_active -> Bool,
        ssl_status -> Varchar,
        ssl_expires_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    geo_overrides (id) {
        id -> Uuid,
        cidr -> Varchar,
        country_code -> Varchar,
        country_name -> Nullable<Varchar>,
        city -> Nullable<Varchar>,
        asn -> Nullable<Int4>,
        asn_organization -> Nullable<Varchar>,
        is_active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    licenses (id) {
        id -> Uuid,
        license_key -> Varchar,
        product -> Varchar,
        user_id -> Nullable<Uuid>,
        company_id -> Nullable<Uuid>,
        max_activations -> Int4,
        status -> Varchar,
        expires_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    license_activations (id) {
        id -> Uuid,
        license_id -> Uuid,
        instance_id -> Varchar,
        hostname -> Nullable<Varchar>,
        created_at -> Timestamptz,
    }
}

diesel::joinable!(users -> companies (company_id));
diesel::joinable!(user_roles -> users (user_id));
diesel::joinable!(user_roles -> roles (role_id));
diesel::joinable!(company_sites -> companies (company_id));
diesel::joinable!(domains -> companies (company_id));
diesel::joinable!(licenses -> users (user_id));
diesel::joinable!(licenses -> companies (company_id));
diesel::joinable!(license_activations -> licenses (license_id));

diesel::allow_tables_to_appear_in_same_query!(
    users,
    roles,
    user_roles,
    companies,
    company_sites,
    domains,
    geo_overrides,
    licenses,
    license_activations,
);
