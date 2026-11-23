// Diesel schema for the software domain of the unified-db-core bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

diesel::table! {
    software_taxonomy (id) {
        id -> Uuid,
        kind -> Varchar,
        slug -> Varchar,
        name -> Varchar,
        description -> Text,
        parent_id -> Nullable<Uuid>,
        sort_order -> Int8,
        depth -> Int8,
        path -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    software (id) {
        id -> Uuid,
        slug -> Varchar,
        name -> Varchar,
        kind -> Varchar,
        description -> Text,
        vendor_company_id -> Nullable<Uuid>,
        group_id -> Nullable<Uuid>,
        category_id -> Nullable<Uuid>,
        status -> Varchar,
        homepage_url -> Nullable<Varchar>,
        repository_url -> Nullable<Varchar>,
        documentation_url -> Nullable<Varchar>,
        screenshots -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    software_versions (id) {
        id -> Uuid,
        product -> Varchar,
        version -> Varchar,
        changelog -> Text,
        file_id -> Nullable<Uuid>,
        license_required -> Bool,
        software_id -> Nullable<Uuid>,
        requirements -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    software_releases (id) {
        id -> Uuid,
        version_id -> Uuid,
        channel -> Varchar,
        notes -> Text,
        status -> Varchar,
        download_count -> Int8,
        created_at -> Timestamptz,
        published_at -> Nullable<Timestamptz>,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    release_files (id) {
        id -> Uuid,
        release_id -> Uuid,
        version_id -> Uuid,
        filename -> Varchar,
        size_bytes -> Int8,
        checksum_sha256 -> Nullable<Varchar>,
        checksum_sha512 -> Nullable<Varchar>,
        download_url -> Nullable<Varchar>,
        storage_path -> Nullable<Varchar>,
        integrity_status -> Varchar,
        verified_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    software_tags (id) {
        id -> Uuid,
        slug -> Varchar,
        name -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    software_tag_assignments (software_id, tag_id) {
        software_id -> Uuid,
        tag_id -> Uuid,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
