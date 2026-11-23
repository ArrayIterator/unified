// Diesel schema for the documents domain of the unified-db-platform bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Documents (migration 000065_documents).
// ---------------------------------------------------------------------------

diesel::table! {
    documents (id) {
        id -> Uuid,
        name -> Text,
        module_id -> Text,
        company_id -> Nullable<Uuid>,
        owner_id -> Text,
        scope -> Text,
        mime -> Text,
        size -> Int8,
        checksum -> Nullable<Text>,
        storage_key -> Text,
        current_version -> Int4,
        version_count -> Int4,
        scan_status -> Text,
        scan_threat -> Nullable<Text>,
        visibility -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        title -> Text,
        alt_text -> Nullable<Text>,
        caption -> Nullable<Text>,
        description -> Nullable<Text>,
        custom_metadata -> Jsonb,
    }
}

diesel::table! {
    document_versions (id) {
        id -> Uuid,
        document_id -> Uuid,
        version -> Int4,
        checksum -> Nullable<Text>,
        storage_key -> Text,
        size -> Int8,
        mime -> Text,
        uploader_id -> Text,
        scan_status -> Text,
        scan_threat -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    document_upload_sessions (id) {
        id -> Uuid,
        document_id -> Nullable<Uuid>,
        name -> Text,
        module_id -> Text,
        company_id -> Nullable<Uuid>,
        owner_id -> Text,
        total_size -> Int8,
        expected_sha256 -> Text,
        chunk_size -> Int8,
        part_count -> Int4,
        status -> Text,
        expires_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    document_upload_parts (id) {
        id -> Uuid,
        session_id -> Uuid,
        part_index -> Int4,
        size -> Int8,
        part_sha256 -> Text,
        received_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    document_allowlist_rules (id) {
        id -> Uuid,
        module_id -> Text,
        extensions -> Array<Text>,
        mime_types -> Array<Text>,
        max_bytes -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    document_downloads (id) {
        id -> Uuid,
        document_id -> Nullable<Uuid>,
        version -> Int4,
        user_id -> Text,
        via -> Text,
        scan_status -> Text,
        bytes -> Int8,
        downloaded_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    document_quotas (id) {
        id -> Uuid,
        scope -> Text,
        scope_id -> Text,
        bytes_limit -> Nullable<Int8>,
        used_bytes -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    finalize_jobs (id) {
        id -> Uuid,
        session_id -> Uuid,
        document_id -> Nullable<Uuid>,
        // C11 (migration 000126): tenant scope of the job, copied
        // from the upload session at creation. NULL for jobs
        // created before the column existed.
        tenant_id -> Nullable<Uuid>,
        status -> Text,
        attempts -> Int4,
        last_error -> Nullable<Text>,
        claimed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
