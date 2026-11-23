// Diesel schema for the vault domain of the unified-db-infra bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Document vault (migration 000052_vault). Only one `joinable!`
// per table pair is allowed, so `vault_documents ->
// vault_document_versions (current_version_id)` is omitted — the
// `vault_document_versions -> vault_documents (document_id)`
// direction is kept. The self-reference
// `vault_folders.parent_id` gets no `joinable!`.
// ---------------------------------------------------------------------------

diesel::table! {
    vault_folders (id) {
        id -> Uuid,
        name -> Varchar,
        parent_id -> Nullable<Uuid>,
        owner_id -> Varchar,
        company_id -> Nullable<Varchar>,
        retention_days -> Nullable<Int8>,
        auto_delete -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    vault_documents (id) {
        id -> Uuid,
        folder_id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        owner_id -> Varchar,
        company_id -> Nullable<Varchar>,
        current_version_id -> Nullable<Uuid>,
        storage_target -> Varchar,
        storage_key -> Varchar,
        checksum_sha256 -> Bpchar,
        size_bytes -> Int8,
        mime_type -> Varchar,
        shared_with_client_ids -> Array<Text>,
        status -> Varchar,
        flagged_for_review -> Bool,
        flagged_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    vault_document_versions (id) {
        id -> Uuid,
        document_id -> Uuid,
        version_number -> Int8,
        storage_target -> Varchar,
        storage_key -> Varchar,
        checksum_sha256 -> Bpchar,
        size_bytes -> Int8,
        mime_type -> Varchar,
        uploaded_by -> Varchar,
        note -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    vault_access_log (id) {
        id -> Uuid,
        document_id -> Uuid,
        actor_id -> Varchar,
        action -> Varchar,
        detail -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    vault_share_links (id) {
        id -> Uuid,
        document_id -> Uuid,
        token -> Varchar,
        expires_at -> Timestamptz,
        max_downloads -> Int8,
        download_count -> Int8,
        revoked -> Bool,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    vault_access_requests (id) {
        id -> Uuid,
        document_id -> Uuid,
        requester_id -> Varchar,
        reason -> Nullable<Text>,
        status -> Varchar,
        decided_by -> Nullable<Varchar>,
        decided_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
