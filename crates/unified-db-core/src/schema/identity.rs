// Diesel schema for the identity domain of the unified-db-core bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

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
        updated_at -> Timestamptz,
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
        updated_at -> Timestamptz,
    }
}
