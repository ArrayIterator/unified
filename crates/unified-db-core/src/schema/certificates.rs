// Diesel schema for the certificates domain of the unified-db-core bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Certificates (2026-11-22-000005_certificates).

diesel::table! {
    certificates (id) {
        id -> Uuid,
        domain -> Varchar,
        provider -> Varchar,
        status -> Varchar,
        issuer -> Varchar,
        sans -> Jsonb,
        fingerprint_sha256 -> Varchar,
        not_before -> Nullable<Timestamptz>,
        expires_at -> Nullable<Timestamptz>,
        days_remaining -> Nullable<Int8>,
        self_signed -> Bool,
        wildcard -> Bool,
        source -> Varchar,
        revoked -> Bool,
        last_renewed_at -> Nullable<Timestamptz>,
        last_error -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    certificate_renewals (id) {
        id -> Uuid,
        domain -> Varchar,
        provider -> Varchar,
        status -> Varchar,
        started_at -> Timestamptz,
        finished_at -> Nullable<Timestamptz>,
        error -> Nullable<Text>,
        new_expires_at -> Nullable<Timestamptz>,
        forced -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    certificate_settings (key) {
        key -> Varchar,
        enabled -> Bool,
        check_interval_secs -> Int8,
        acme -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
