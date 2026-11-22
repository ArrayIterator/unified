// Diesel schema for the core tables.
//
// This schema matches every migration from
// `migrations/2026-11-22-000001_core/up.sql` through
// `migrations/2026-11-22-000107_reference_backfill/up.sql` (migration 000003 is
// ALTER-only; its added columns and NUMERIC(19,4) conversions are
// folded into the table definitions below; migrations 000032-000107
// were added by the integration round from the batch integration
// notes, with column definitions copied from each migration's
// `up.sql` or from the notes' `table!` snippets where the notes
// carry them).
// Modules that need persistence beyond their in-memory store use these
// tables; the software, store, and VCS modules still keep their live
// data in memory (see TODO.md) and these tables are their persistence
// target, not yet their read path.
//
// KNOWN COLLISIONS (do not re-add; see the integration report):
// - `certificates` is the SSL-certificate table (migration 000005).
//   Migration 000079_elearning also issues `CREATE TABLE IF NOT
//   EXISTS certificates` with a different shape (course
//   certificates); at runtime that CREATE is a no-op and the
//   elearning certificate columns are NOT represented here.
// - `pricing_tiers` is created by both 000041_pricing_marketplace
//   (pricing-table feature tiers) and 000103_pricing (volume
//   quantity tiers) with different columns; only one `table!`
//   block can exist, so the 000041 shape is recorded here and the
//   000103 shape is NOT represented.
// - `reference_id_settings` has no migration yet (snippet only in
//   the batch-18 notes); `tenant_destroy_requests` likewise has no
//   migration yet (derived from the `DestroyRequest` struct in
//   `src/modules/tenant_destroy.rs`). `destroyed_tenants` is NOT
//   added: no struct or DDL for it exists anywhere (the module
//   states destroyed tenants have no record of their own — the
//   `tenants` row itself is the guard record, with `destroyed_at`
//   and `export_reference` already recorded there).
//
// Money is NUMERIC(19,4) everywhere (Diesel `Numeric`, Rust
// `diesel::data_types::PgNumeric`); see docs/SCHEMA.md.

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

diesel::table! {
    store_products (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Text,
        price -> Numeric,
        currency -> Varchar,
        kind -> Varchar,
        license_product -> Bool,
        file_id -> Nullable<Uuid>,
        slug -> Nullable<Varchar>,
        software_id -> Nullable<Uuid>,
        software_slug -> Nullable<Varchar>,
        status -> Varchar,
        company_id -> Nullable<Uuid>,
        pricing_model -> Varchar,
        billing_period -> Nullable<Varchar>,
        renewal_price -> Nullable<Numeric>,
        trial_days -> Int4,
        updates_years -> Nullable<Int4>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    store_pricing_tiers (id) {
        id -> Uuid,
        product_id -> Uuid,
        slug -> Varchar,
        label -> Varchar,
        price -> Numeric,
        renewal_price -> Nullable<Numeric>,
        billing_period -> Nullable<Varchar>,
        trial_days -> Int4,
        seats -> Int4,
        download_limit -> Int4,
        features -> Jsonb,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    store_coupons (id) {
        id -> Uuid,
        code -> Varchar,
        discount_type -> Varchar,
        discount_value -> Numeric,
        expires_at -> Nullable<Timestamptz>,
        usage_limit -> Nullable<Int4>,
        used_count -> Int4,
        product_ids -> Jsonb,
        active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    store_orders (id) {
        id -> Uuid,
        user_id -> Varchar,
        subtotal -> Numeric,
        discount_total -> Numeric,
        total -> Numeric,
        currency -> Varchar,
        coupon_code -> Nullable<Varchar>,
        company_id -> Nullable<Uuid>,
        trial_until -> Nullable<Timestamptz>,
        status -> Varchar,
        provider -> Nullable<Varchar>,
        reference -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    store_order_items (id) {
        id -> Uuid,
        order_id -> Uuid,
        product_id -> Nullable<Uuid>,
        name -> Varchar,
        unit_price -> Numeric,
        quantity -> Int4,
        line_total -> Numeric,
        license_product -> Bool,
        file_id -> Nullable<Uuid>,
        tier_id -> Nullable<Uuid>,
        tier_name -> Nullable<Varchar>,
        software_id -> Nullable<Uuid>,
        software_slug -> Nullable<Varchar>,
        company_id -> Nullable<Uuid>,
        pricing_model -> Varchar,
        billing_period -> Nullable<Varchar>,
        seats -> Int4,
        expires_at -> Nullable<Timestamptz>,
        download_limit -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    store_entitlements (id) {
        id -> Uuid,
        license_id -> Nullable<Uuid>,
        license_key -> Nullable<Varchar>,
        order_id -> Nullable<Uuid>,
        user_id -> Varchar,
        product_id -> Nullable<Uuid>,
        tier_id -> Nullable<Uuid>,
        software_id -> Nullable<Uuid>,
        software_slug -> Nullable<Varchar>,
        release_id -> Nullable<Uuid>,
        download_limit -> Int4,
        downloads_used -> Int4,
        status -> Varchar,
        expires_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    store_downloads (id) {
        id -> Uuid,
        license_id -> Nullable<Uuid>,
        release_id -> Nullable<Uuid>,
        software_slug -> Nullable<Varchar>,
        order_id -> Nullable<Uuid>,
        downloaded_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    store_catalog (slug) {
        slug -> Varchar,
        name -> Varchar,
        kind -> Varchar,
        product_id -> Nullable<Uuid>,
        product_name -> Varchar,
        latest_version -> Varchar,
        release_id -> Nullable<Uuid>,
        file_id -> Nullable<Uuid>,
        changelog -> Text,
        requires_wp -> Nullable<Varchar>,
        tested_wp -> Nullable<Varchar>,
        requires_php -> Nullable<Varchar>,
        license_required -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    store_issued_licenses (license_id) {
        license_id -> Uuid,
        license_key -> Varchar,
        product -> Varchar,
        order_id -> Nullable<Uuid>,
        user_id -> Nullable<Varchar>,
        company_id -> Nullable<Uuid>,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    vcs_connections (id) {
        id -> Uuid,
        provider -> Varchar,
        owner -> Varchar,
        repo -> Varchar,
        host -> Nullable<Varchar>,
        software_id -> Nullable<Uuid>,
        enabled -> Bool,
        sync_status -> Varchar,
        last_synced_at -> Nullable<Timestamptz>,
        last_error -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    vcs_version_drafts (id) {
        id -> Uuid,
        connection_id -> Uuid,
        software_id -> Nullable<Uuid>,
        provider -> Varchar,
        version -> Varchar,
        tag -> Varchar,
        title -> Nullable<Varchar>,
        changelog -> Text,
        published_at -> Nullable<Timestamptz>,
        status -> Varchar,
        source -> Varchar,
        assets -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    vcs_sync_history (id) {
        id -> Uuid,
        connection_id -> Uuid,
        provider -> Varchar,
        status -> Varchar,
        releases_found -> Int4,
        drafts_created -> Int4,
        error -> Nullable<Text>,
        rate_limit_remaining -> Nullable<Int4>,
        started_at -> Timestamptz,
        finished_at -> Timestamptz,
        duration_ms -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    vcs_provider_settings (provider) {
        provider -> Varchar,
        token_encrypted -> Nullable<Text>,
        webhook_secret_encrypted -> Nullable<Text>,
        api_base_override -> Nullable<Varchar>,
        gitlab_hosts -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// API Management (2026-11-22-000004_api_management).
// ---------------------------------------------------------------------------

diesel::table! {
    api_versions (id) {
        id -> Varchar,
        number -> Int4,
        enabled -> Bool,
        deprecated -> Bool,
        sunset_date -> Nullable<Timestamptz>,
        rate_limit_per_minute -> Int4,
        auth_required -> Bool,
        notes -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    api_endpoints (id) {
        id -> Uuid,
        method -> Varchar,
        path -> Varchar,
        version -> Varchar,
        description -> Text,
        rate_limit -> Int4,
        enabled -> Bool,
        rules -> Jsonb,
        owner_module -> Varchar,
        auth_required -> Bool,
        source -> Varchar,
        version_support -> Jsonb,
        execution_count -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    api_prefix_settings (key) {
        key -> Varchar,
        prefix -> Varchar,
        migrate_internal -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    api_keys (id) {
        id -> Uuid,
        name -> Varchar,
        owner_id -> Nullable<Uuid>,
        prefix -> Varchar,
        last4 -> Varchar,
        key_hash -> Varchar,
        scopes -> Jsonb,
        rate_tier -> Varchar,
        requests_per_minute -> Int4,
        status -> Varchar,
        expires_at -> Nullable<Timestamptz>,
        grace_until -> Nullable<Timestamptz>,
        rotated_from -> Nullable<Uuid>,
        rotated_to -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        last_used_at -> Nullable<Timestamptz>,
        usage_count -> Int8,
    }
}

diesel::table! {
    api_access_rules (id) {
        id -> Uuid,
        method -> Varchar,
        path -> Varchar,
        version -> Nullable<Varchar>,
        allowed -> Bool,
        public_access -> Bool,
        actor -> Varchar,
        reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    api_access_audit (id) {
        id -> Uuid,
        actor -> Varchar,
        action -> Varchar,
        target -> Varchar,
        detail -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Certificates (2026-11-22-000005_certificates).
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Dashboards and reseller links (2026-11-22-000006_dashboards_reseller).
// ---------------------------------------------------------------------------

diesel::table! {
    reseller_links (reseller_id, client_id) {
        reseller_id -> Uuid,
        client_id -> Uuid,
        verified -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    view_as_sessions (admin_id) {
        admin_id -> Uuid,
        target_user_id -> Uuid,
        reason -> Text,
        started_at -> Timestamptz,
        expires_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    dashboard_settings (key) {
        key -> Varchar,
        admin_prefix -> Varchar,
        user_prefix -> Varchar,
        view_as_ttl_minutes -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Hosting (2026-11-22-000007_hosting).
// ---------------------------------------------------------------------------

diesel::table! {
    hosting_plans (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Text,
        disk_gb -> Int8,
        cpu_cores -> Float8,
        ram_mb -> Int8,
        bandwidth_gb -> Int8,
        price -> Numeric,
        currency -> Varchar,
        billing_cycle -> Varchar,
        store_product_id -> Nullable<Varchar>,
        is_active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hosting_accounts (id) {
        id -> Uuid,
        plan_id -> Uuid,
        domain -> Varchar,
        company_id -> Nullable<Uuid>,
        status -> Varchar,
        ssl_state -> Varchar,
        disk_used_mb -> Int8,
        bandwidth_used_mb -> Int8,
        store_order_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hosting_order_claims (order_id, plan_id) {
        order_id -> Varchar,
        plan_id -> Uuid,
        user_id -> Varchar,
        company_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Clusters (2026-11-22-000008_clusters).
// ---------------------------------------------------------------------------

diesel::table! {
    clusters (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Text,
        kind -> Varchar,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    cluster_nodes (id) {
        id -> Uuid,
        cluster_id -> Uuid,
        display_name -> Varchar,
        base_url -> Varchar,
        role -> Varchar,
        status -> Varchar,
        secret_hash -> Varchar,
        secret_fingerprint -> Varchar,
        health -> Varchar,
        consecutive_failures -> Int4,
        last_seen_at -> Nullable<Timestamptz>,
        last_latency_ms -> Nullable<Int8>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    cluster_sync_plans (id) {
        id -> Uuid,
        cluster_id -> Uuid,
        target_node_id -> Uuid,
        local_node_id -> Varchar,
        direction -> Varchar,
        domains -> Jsonb,
        schedule -> Varchar,
        enabled -> Bool,
        peer_secret_hash -> Varchar,
        peer_secret_fingerprint -> Nullable<Varchar>,
        last_run_at -> Nullable<Timestamptz>,
        last_success_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    cluster_sync_items (domain, key) {
        domain -> Varchar,
        key -> Varchar,
        payload -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    cluster_sync_logs (id) {
        id -> Uuid,
        plan_id -> Nullable<Uuid>,
        cluster_id -> Uuid,
        node_id -> Nullable<Uuid>,
        direction -> Varchar,
        domain -> Varchar,
        status -> Varchar,
        items_sent -> Int8,
        items_received -> Int8,
        items_applied -> Int8,
        items_skipped -> Int8,
        conflicts -> Jsonb,
        errors -> Jsonb,
        dry_run -> Bool,
        started_at -> Timestamptz,
        finished_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    cluster_health_checks (id) {
        id -> Uuid,
        node_id -> Uuid,
        cluster_id -> Uuid,
        status -> Varchar,
        latency_ms -> Nullable<Int8>,
        error -> Nullable<Text>,
        checked_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Backup and storage targets (2026-11-22-000009_backup_storage).
// ---------------------------------------------------------------------------

diesel::table! {
    backup_jobs (id) {
        id -> Uuid,
        name -> Varchar,
        kind -> Varchar,
        database_name -> Varchar,
        paths -> Jsonb,
        config_keys -> Jsonb,
        target -> Varchar,
        schedule -> Varchar,
        retention_keep_last -> Int4,
        retention_keep_days -> Int4,
        enabled -> Bool,
        scheduling -> Jsonb,
        status -> Varchar,
        checksum -> Nullable<Varchar>,
        size_bytes -> Nullable<Int8>,
        storage_key -> Nullable<Varchar>,
        last_run_id -> Nullable<Uuid>,
        started_at -> Nullable<Timestamptz>,
        finished_at -> Nullable<Timestamptz>,
        error -> Nullable<Text>,
        company_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    backup_runs (id) {
        id -> Uuid,
        job_id -> Nullable<Uuid>,
        job_name -> Varchar,
        kind -> Varchar,
        target -> Varchar,
        storage_key -> Nullable<Varchar>,
        status -> Varchar,
        checksum -> Nullable<Varchar>,
        size_bytes -> Nullable<Int8>,
        file_count -> Nullable<Int8>,
        started_at -> Timestamptz,
        finished_at -> Nullable<Timestamptz>,
        error -> Nullable<Text>,
        company_id -> Nullable<Uuid>,
        verified -> Nullable<Bool>,
        last_verified_at -> Nullable<Timestamptz>,
        manifest_verified -> Nullable<Bool>,
        verification_error -> Nullable<Text>,
        last_drill_at -> Nullable<Timestamptz>,
        drill_verified -> Nullable<Bool>,
        encryption -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_targets (name) {
        name -> Varchar,
        kind -> Varchar,
        config -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Providers (2026-11-22-000010_providers).
// ---------------------------------------------------------------------------

diesel::table! {
    provider_connections (id) {
        id -> Uuid,
        name -> Varchar,
        kind -> Varchar,
        endpoint -> Nullable<Varchar>,
        region -> Nullable<Varchar>,
        project_id -> Nullable<Varchar>,
        credentials -> Jsonb,
        custom_config -> Nullable<Jsonb>,
        enabled -> Bool,
        last_tested_at -> Nullable<Timestamptz>,
        last_status -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    provider_servers (id) {
        id -> Uuid,
        connection_id -> Uuid,
        remote_id -> Varchar,
        name -> Varchar,
        item_kind -> Varchar,
        status -> Varchar,
        raw_status -> Nullable<Varchar>,
        region -> Nullable<Varchar>,
        public_ip -> Nullable<Varchar>,
        private_ip -> Nullable<Varchar>,
        cpu_cores -> Nullable<Int4>,
        memory_mb -> Nullable<Int8>,
        disk_gb -> Nullable<Int8>,
        image -> Nullable<Varchar>,
        remote_created_at -> Nullable<Varchar>,
        fetched_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Tasks and system records (2026-11-22-000011_tasks_records).
// ---------------------------------------------------------------------------

diesel::table! {
    tasks (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Text,
        assignee_id -> Nullable<Uuid>,
        reporter_id -> Nullable<Uuid>,
        priority -> Varchar,
        status -> Varchar,
        due_date -> Nullable<Timestamptz>,
        company_id -> Nullable<Uuid>,
        project_name -> Nullable<Varchar>,
        tags -> Jsonb,
        estimate_minutes -> Nullable<Int4>,
        spent_minutes -> Int4,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    task_comments (id) {
        id -> Uuid,
        task_id -> Uuid,
        author_id -> Nullable<Uuid>,
        body -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    task_checklist_items (id) {
        id -> Uuid,
        task_id -> Uuid,
        title -> Varchar,
        done -> Bool,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    system_records (id) {
        id -> Uuid,
        seq -> Int8,
        actor -> Varchar,
        action -> Varchar,
        entity_type -> Varchar,
        entity_id -> Varchar,
        summary -> Text,
        details -> Jsonb,
        ip -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Logs and billing (2026-11-22-000012_logs_billing).
// ---------------------------------------------------------------------------

diesel::table! {
    app_logs (id) {
        id -> Uuid,
        seq -> Int8,
        timestamp -> Timestamptz,
        level -> Varchar,
        kind -> Varchar,
        source -> Varchar,
        message -> Text,
        context -> Jsonb,
        request_id -> Nullable<Varchar>,
        user_id -> Nullable<Varchar>,
        method -> Nullable<Varchar>,
        path -> Nullable<Varchar>,
        status -> Nullable<Int4>,
        duration_ms -> Nullable<Int8>,
        ip -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    logs_settings (key) {
        key -> Varchar,
        retention_days -> Int4,
        max_entries -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    invoices (id) {
        id -> Uuid,
        number -> Varchar,
        order_id -> Nullable<Varchar>,
        user_id -> Varchar,
        company_id -> Nullable<Uuid>,
        subtotal -> Numeric,
        tax_total -> Numeric,
        total -> Numeric,
        currency -> Varchar,
        status -> Varchar,
        overdue -> Bool,
        tax_inclusive -> Bool,
        amount_paid -> Numeric,
        credit_total -> Numeric,
        balance -> Numeric,
        issue_date -> Nullable<Timestamptz>,
        due_date -> Nullable<Timestamptz>,
        payment_terms_days -> Nullable<Int4>,
        paid_at -> Nullable<Timestamptz>,
        notes -> Nullable<Text>,
        reminders_sent -> Jsonb,
        reference -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    invoice_items (id) {
        id -> Uuid,
        invoice_id -> Uuid,
        name -> Varchar,
        quantity -> Int4,
        unit_price -> Numeric,
        line_total -> Numeric,
        tax_rate -> Float8,
        tax_amount -> Numeric,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    invoice_payments (id) {
        id -> Uuid,
        invoice_id -> Uuid,
        amount -> Numeric,
        method -> Varchar,
        reference -> Nullable<Varchar>,
        recorded_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    credit_notes (id) {
        id -> Uuid,
        invoice_id -> Uuid,
        number -> Varchar,
        amount -> Numeric,
        reason -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    billing_settings (key) {
        key -> Varchar,
        invoice_prefix -> Varchar,
        default_tax_rate -> Float8,
        tax_inclusive -> Bool,
        payment_terms_days -> Int4,
        reminder_before_days -> Jsonb,
        reminder_after_days -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    invoice_sequences (name) {
        name -> Varchar,
        next_value -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Panels (2026-11-22-000013_panels).
// ---------------------------------------------------------------------------

diesel::table! {
    panel_connections (id) {
        id -> Uuid,
        name -> Varchar,
        kind -> Varchar,
        endpoint -> Nullable<Varchar>,
        credentials -> Jsonb,
        generic_config -> Nullable<Jsonb>,
        enabled -> Bool,
        last_tested_at -> Nullable<Timestamptz>,
        last_status -> Nullable<Varchar>,
        last_synced_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    panel_synced_records (connection_id, collection, remote_id) {
        connection_id -> Uuid,
        collection -> Varchar,
        remote_id -> Varchar,
        name -> Varchar,
        status -> Varchar,
        hash -> Varchar,
        last_synced_at -> Timestamptz,
        direction -> Varchar,
        conflict -> Bool,
        local_changed -> Bool,
        prefer_local -> Bool,
        payload -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Payments and bank transfer (2026-11-22-000014_payments_bank_transfer).
// ---------------------------------------------------------------------------

diesel::table! {
    payment_connections (id) {
        id -> Uuid,
        name -> Varchar,
        kind -> Varchar,
        mode -> Varchar,
        endpoint -> Nullable<Varchar>,
        credentials -> Jsonb,
        custom_config -> Nullable<Jsonb>,
        delegate_connection_id -> Nullable<Uuid>,
        enabled -> Bool,
        last_tested_at -> Nullable<Timestamptz>,
        last_status -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    payment_intents (id) {
        id -> Uuid,
        connection_id -> Uuid,
        provider -> Varchar,
        customer_id -> Varchar,
        order_id -> Nullable<Varchar>,
        invoice_id -> Nullable<Varchar>,
        amount_minor -> Int8,
        currency -> Varchar,
        status -> Varchar,
        capture_method -> Varchar,
        provider_transaction_id -> Nullable<Varchar>,
        fee_minor -> Nullable<Int8>,
        idempotency_key -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    payment_transactions (id) {
        id -> Uuid,
        intent_id -> Nullable<Uuid>,
        connection_id -> Uuid,
        provider -> Varchar,
        kind -> Varchar,
        customer_id -> Varchar,
        order_id -> Nullable<Varchar>,
        invoice_id -> Nullable<Varchar>,
        subscription_id -> Nullable<Uuid>,
        amount_minor -> Int8,
        currency -> Varchar,
        fee_minor -> Nullable<Int8>,
        status -> Varchar,
        provider_transaction_id -> Varchar,
        provider_refund_id -> Nullable<Varchar>,
        idempotency_key -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    payment_methods (id) {
        id -> Uuid,
        customer_id -> Varchar,
        connection_id -> Uuid,
        provider -> Varchar,
        provider_token -> Varchar,
        brand -> Varchar,
        last4 -> Varchar,
        exp_month -> Int4,
        exp_year -> Int4,
        is_default -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    gateway_subscriptions (id) {
        id -> Uuid,
        connection_id -> Uuid,
        provider -> Varchar,
        customer_id -> Varchar,
        order_id -> Nullable<Varchar>,
        plan_id -> Nullable<Varchar>,
        amount_minor -> Int8,
        currency -> Varchar,
        interval -> Varchar,
        interval_count -> Int4,
        status -> Varchar,
        provider_subscription_id -> Varchar,
        payment_method_id -> Nullable<Uuid>,
        current_period_end -> Nullable<Timestamptz>,
        next_renewal_at -> Nullable<Timestamptz>,
        cancelled_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    split_bills (id) {
        id -> Uuid,
        order_id -> Nullable<Varchar>,
        invoice_id -> Nullable<Varchar>,
        customer_id -> Varchar,
        total_minor -> Int8,
        currency -> Varchar,
        status -> Varchar,
        paid_minor -> Int8,
        remaining_minor -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    split_parts (id) {
        id -> Uuid,
        split_id -> Uuid,
        index -> Int4,
        amount_minor -> Int8,
        currency -> Varchar,
        provider -> Varchar,
        connection_id -> Nullable<Uuid>,
        payer_id -> Varchar,
        due_date -> Nullable<Timestamptz>,
        status -> Varchar,
        paid_transaction_id -> Nullable<Uuid>,
        paid_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    bank_accounts (id) {
        id -> Uuid,
        bank_name -> Varchar,
        account_holder -> Varchar,
        account_number -> Varchar,
        swift_bic -> Nullable<Varchar>,
        currency -> Varchar,
        instructions -> Nullable<Text>,
        active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    bank_transfer_verifications (id) {
        id -> Uuid,
        order_id -> Varchar,
        invoice_id -> Nullable<Varchar>,
        customer_id -> Varchar,
        split_id -> Nullable<Uuid>,
        part_id -> Nullable<Uuid>,
        bank_account_id -> Nullable<Uuid>,
        status -> Varchar,
        expected_amount_minor -> Nullable<Int8>,
        currency -> Varchar,
        proof -> Nullable<Jsonb>,
        amount_check -> Varchar,
        amount_difference_minor -> Nullable<Int8>,
        transaction_id -> Nullable<Varchar>,
        verified_by -> Nullable<Varchar>,
        verified_at -> Nullable<Timestamptz>,
        rejection_reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    bank_transfer_verification_audit (id) {
        id -> Uuid,
        verification_id -> Uuid,
        action -> Varchar,
        actor -> Varchar,
        actor_role -> Varchar,
        reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    bank_transfer_settings (key) {
        key -> Varchar,
        amount_tolerance_minor -> Int8,
        proof_expiry_days -> Int4,
        require_reference_number -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Planner and analytics (2026-11-22-000015_planner_analytics).
// ---------------------------------------------------------------------------

diesel::table! {
    capacity_pools (id) {
        id -> Uuid,
        name -> Varchar,
        provider_connection_id -> Nullable<Varchar>,
        cluster_id -> Nullable<Varchar>,
        capacity_cpu_cores -> Float8,
        capacity_ram_gb -> Float8,
        capacity_disk_gb -> Float8,
        capacity_bandwidth_tb -> Float8,
        notes -> Nullable<Text>,
        last_sync_at -> Nullable<Timestamptz>,
        last_server_count -> Nullable<Int8>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pool_usage_samples (id) {
        id -> Uuid,
        pool_id -> Uuid,
        used_cpu_cores -> Float8,
        used_ram_gb -> Float8,
        used_disk_gb -> Float8,
        used_bandwidth_tb -> Float8,
        source -> Varchar,
        recorded_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    allocation_plans (id) {
        id -> Uuid,
        pool_id -> Uuid,
        name -> Varchar,
        company_id -> Nullable<Varchar>,
        project_name -> Nullable<Varchar>,
        reserved_cpu_cores -> Float8,
        reserved_ram_gb -> Float8,
        reserved_disk_gb -> Float8,
        reserved_bandwidth_tb -> Float8,
        starts_at -> Timestamptz,
        ends_at -> Timestamptz,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    analytics_sales (id) {
        id -> Uuid,
        order_id -> Nullable<Varchar>,
        kind -> Varchar,
        total -> Numeric,
        currency -> Varchar,
        products -> Jsonb,
        company_id -> Nullable<Varchar>,
        recorded_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    analytics_license_snapshots (license_id) {
        license_id -> Varchar,
        license_key -> Nullable<Varchar>,
        product -> Varchar,
        status -> Varchar,
        last_event_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    analytics_metric_samples (id) {
        id -> Uuid,
        kind -> Varchar,
        source -> Nullable<Varchar>,
        recorded_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    analytics_health_samples (id) {
        id -> Uuid,
        node_id -> Varchar,
        cluster_id -> Nullable<Varchar>,
        status -> Varchar,
        latency_ms -> Nullable<Int8>,
        checked_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    analytics_reports (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        metrics -> Jsonb,
        dimensions -> Jsonb,
        range_preset -> Nullable<Varchar>,
        start -> Nullable<Timestamptz>,
        end -> Nullable<Timestamptz>,
        filters -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    analytics_report_runs (id) {
        id -> Uuid,
        definition_id -> Uuid,
        status -> Varchar,
        started_at -> Timestamptz,
        finished_at -> Nullable<Timestamptz>,
        row_count -> Int8,
        error -> Nullable<Text>,
        rows -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    analytics_scheduled_reports (id) {
        id -> Uuid,
        definition_id -> Uuid,
        schedule -> Varchar,
        enabled -> Bool,
        next_run_at -> Nullable<Timestamptz>,
        last_run_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Email and newsletter (2026-11-22-000016_email_newsletter).
// ---------------------------------------------------------------------------

diesel::table! {
    email_connections (id) {
        id -> Uuid,
        name -> Varchar,
        kind -> Varchar,
        domain -> Nullable<Varchar>,
        region -> Nullable<Varchar>,
        sender_email -> Nullable<Varchar>,
        sender_name -> Nullable<Varchar>,
        webhook_url -> Nullable<Varchar>,
        credentials -> Jsonb,
        test_mode -> Bool,
        enabled -> Bool,
        last_tested_at -> Nullable<Timestamptz>,
        last_status -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    email_send_log (id) {
        id -> Uuid,
        connection_id -> Nullable<Uuid>,
        provider -> Varchar,
        to -> Varchar,
        subject -> Varchar,
        status -> Varchar,
        provider_message_id -> Nullable<Varchar>,
        test_mode -> Bool,
        error -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    mail_smtp_settings (key) {
        key -> Varchar,
        host -> Varchar,
        port -> Int4,
        tls -> Bool,
        username -> Varchar,
        password_encrypted -> Text,
        sender_name -> Varchar,
        sender_email -> Varchar,
        connector -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    mail_log (id) {
        id -> Uuid,
        to -> Varchar,
        subject -> Varchar,
        status -> Varchar,
        connector -> Varchar,
        template_id -> Nullable<Uuid>,
        error -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    email_templates (id) {
        id -> Uuid,
        name -> Varchar,
        subject -> Varchar,
        category -> Varchar,
        html -> Text,
        json -> Jsonb,
        version -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    email_template_versions (id) {
        id -> Uuid,
        template_id -> Uuid,
        version -> Int4,
        subject -> Varchar,
        html -> Text,
        json -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    newsletter_lists (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Text,
        default_connection_id -> Nullable<Uuid>,
        double_opt_in -> Bool,
        sender_name -> Nullable<Varchar>,
        sender_email -> Nullable<Varchar>,
        base_url -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    newsletter_subscribers (id) {
        id -> Uuid,
        list_id -> Uuid,
        email -> Varchar,
        name -> Nullable<Varchar>,
        status -> Varchar,
        metadata -> Jsonb,
        confirmed_at -> Nullable<Timestamptz>,
        unsubscribed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    newsletter_campaigns (id) {
        id -> Uuid,
        list_id -> Uuid,
        name -> Varchar,
        template_id -> Nullable<Uuid>,
        subject -> Varchar,
        preview_text -> Nullable<Varchar>,
        connection_id -> Nullable<Uuid>,
        status -> Varchar,
        scheduled_at -> Nullable<Timestamptz>,
        sent_at -> Nullable<Timestamptz>,
        stats_sent -> Int8,
        stats_delivered -> Int8,
        stats_opened -> Int8,
        stats_clicked -> Int8,
        stats_bounced -> Int8,
        stats_unsubscribed -> Int8,
        stats_complaints -> Int8,
        stats_unique_opened -> Int8,
        stats_unique_clicked -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    newsletter_tracking_events (id) {
        id -> Uuid,
        campaign_id -> Uuid,
        subscriber_id -> Uuid,
        kind -> Varchar,
        url -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Load balancer (2026-11-22-000017_load_balancer).
// ---------------------------------------------------------------------------

diesel::table! {
    load_balancers (id) {
        id -> Uuid,
        name -> Varchar,
        listen_address -> Varchar,
        listen_port -> Int4,
        tls_mode -> Varchar,
        domain -> Nullable<Varchar>,
        certificate_name -> Nullable<Varchar>,
        strategy -> Varchar,
        health_check -> Jsonb,
        enabled -> Bool,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    lb_backends (id) {
        id -> Uuid,
        balancer_id -> Uuid,
        name -> Varchar,
        source_kind -> Varchar,
        source_id -> Nullable<Varchar>,
        address -> Varchar,
        weight -> Int4,
        enabled -> Bool,
        draining -> Bool,
        health -> Varchar,
        last_check_at -> Nullable<Timestamptz>,
        last_latency_ms -> Nullable<Int8>,
        consecutive_failures -> Int4,
        consecutive_successes -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    lb_backend_stats (backend_id) {
        backend_id -> Uuid,
        requests_routed -> Int8,
        errors -> Int8,
        total_latency_ms -> Int8,
        latency_samples -> Int8,
        checks_total -> Int8,
        checks_ok -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Webhooks, import, and export (2026-11-22-000018_webhooks_import_export).
// ---------------------------------------------------------------------------

diesel::table! {
    webhook_endpoints (id) {
        id -> Uuid,
        name -> Varchar,
        url -> Varchar,
        events -> Jsonb,
        secret -> Varchar,
        active -> Bool,
        paused -> Bool,
        max_attempts -> Int4,
        backoff_base_secs -> Int8,
        timeout_secs -> Int8,
        delivery_count -> Int8,
        last_delivery_at -> Nullable<Timestamptz>,
        last_status -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    webhook_deliveries (id) {
        id -> Uuid,
        endpoint_id -> Uuid,
        event -> Varchar,
        payload -> Jsonb,
        payload_hash -> Varchar,
        status -> Varchar,
        attempts -> Int4,
        max_attempts -> Int4,
        next_retry_at -> Nullable<Timestamptz>,
        status_code -> Nullable<Int4>,
        duration_ms -> Nullable<Int8>,
        error -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        delivered_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    import_jobs (id) {
        id -> Uuid,
        entity -> Varchar,
        format -> Varchar,
        status -> Varchar,
        total_rows -> Int8,
        valid_rows -> Int8,
        invalid_rows -> Int8,
        row_errors -> Jsonb,
        results -> Jsonb,
        actor -> Varchar,
        company_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    export_jobs (id) {
        id -> Uuid,
        entity -> Varchar,
        format -> Varchar,
        status -> Varchar,
        fields -> Jsonb,
        row_count -> Int8,
        data_provider -> Varchar,
        download_url -> Varchar,
        error -> Nullable<Text>,
        actor -> Varchar,
        company_id -> Nullable<Uuid>,
        pii_redacted -> Bool,
        redacted_fields -> Jsonb,
        row_limit -> Int8,
        row_offset -> Int8,
        has_more -> Bool,
        rows_excluded_by_tenant_scope -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Maintenance, status page, announcements, and auth (2026-11-22-000019_maintenance_statuspage_announcements_auth).
// ---------------------------------------------------------------------------

diesel::table! {
    maintenance_windows (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Text,
        starts_at -> Timestamptz,
        ends_at -> Timestamptz,
        affected_services -> Jsonb,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        activated_at -> Nullable<Timestamptz>,
        completed_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    maintenance_history (id) {
        id -> Uuid,
        window_id -> Uuid,
        event -> Varchar,
        message -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    status_components (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Text,
        group -> Nullable<Varchar>,
        status -> Varchar,
        sort_order -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    status_daily_uptime (component_id, date) {
        component_id -> Uuid,
        date -> Varchar,
        uptime_percent -> Float8,
        minutes_down -> Int8,
        total_minutes -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    status_incidents (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Text,
        status -> Varchar,
        severity -> Varchar,
        affected_components -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        resolved_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    status_incident_updates (id) {
        id -> Uuid,
        incident_id -> Uuid,
        status -> Varchar,
        message -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    announcements (id) {
        id -> Uuid,
        title -> Varchar,
        body -> Text,
        audience -> Varchar,
        status -> Varchar,
        publish_at -> Nullable<Timestamptz>,
        expires_at -> Nullable<Timestamptz>,
        pinned -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        published_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    changelog_entries (id) {
        id -> Uuid,
        version -> Varchar,
        date -> Varchar,
        category -> Varchar,
        title -> Varchar,
        body -> Text,
        link -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    announcement_read_receipts (announcement_id, user_id) {
        announcement_id -> Uuid,
        user_id -> Varchar,
        read_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    auth_recovery_codes (user_id, code_hash) {
        user_id -> Varchar,
        code_hash -> Varchar,
        used -> Bool,
        created_at -> Timestamptz,
        used_at -> Nullable<Timestamptz>,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    google_accounts (account_id) {
        account_id -> Varchar,
        email -> Varchar,
        name -> Nullable<Varchar>,
        access_token_encrypted -> Text,
        refresh_token_encrypted -> Text,
        expires_at -> Timestamptz,
        scopes -> Jsonb,
        linked_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    google_drive_links (id) {
        id -> Uuid,
        name -> Varchar,
        account_id -> Varchar,
        email -> Varchar,
        folder_id -> Nullable<Varchar>,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Security (2026-11-22-000020_security).
// ---------------------------------------------------------------------------

diesel::table! {
    security_ip_lists (list, cidr) {
        list -> Varchar,
        cidr -> Varchar,
        network -> Varchar,
        prefix -> Int4,
        added_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    security_reputation (ip) {
        ip -> Varchar,
        score -> Int4,
        flagged -> Bool,
        reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    security_failed_auth_clusters (key) {
        key -> Varchar,
        ip -> Nullable<Varchar>,
        account -> Nullable<Varchar>,
        count -> Int8,
        first_seen -> Timestamptz,
        last_seen -> Timestamptz,
        locked -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Named routes (2026-11-22-000021_routes).
// ---------------------------------------------------------------------------

diesel::table! {
    named_routes (name) {
        name -> Varchar,
        default_path -> Varchar,
        current_path -> Varchar,
        source -> Varchar,
        description -> Text,
        config_override -> Nullable<Varchar>,
        settings_override -> Nullable<Varchar>,
        previous_path -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    route_redirects (route) {
        route -> Varchar,
        old_path -> Varchar,
        new_path -> Varchar,
        changed_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Cloudflare (2026-11-22-000022_cloudflare).
// ---------------------------------------------------------------------------

diesel::table! {
    cloudflare_settings (key) {
        key -> Varchar,
        api_token_encrypted -> Text,
        account_id -> Nullable<Varchar>,
        default_zone_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    cloudflare_purge_history (id) {
        id -> Uuid,
        zone_id -> Varchar,
        kind -> Varchar,
        targets -> Jsonb,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Search, notifications, and saved views (2026-11-22-000023_search_notifications_saved_views).
// ---------------------------------------------------------------------------

diesel::table! {
    search_documents (kind, id) {
        kind -> Varchar,
        id -> Varchar,
        title -> Varchar,
        snippet -> Text,
        url -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    notifications (id) {
        id -> Uuid,
        user_id -> Varchar,
        title -> Varchar,
        body -> Text,
        kind -> Varchar,
        read -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    notification_preferences (user_id) {
        user_id -> Varchar,
        channels -> Jsonb,
        categories -> Jsonb,
        channel_by_category -> Jsonb,
        digest -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    notification_templates (id) {
        id -> Uuid,
        event -> Varchar,
        locale -> Varchar,
        title_template -> Text,
        body_template -> Text,
        channels -> Jsonb,
        email_connection_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    saved_views (id) {
        id -> Uuid,
        user_id -> Varchar,
        module_id -> Varchar,
        name -> Varchar,
        filter -> Jsonb,
        sort -> Nullable<Varchar>,
        order -> Nullable<Varchar>,
        visibility -> Varchar,
        is_default -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timeline_entries (id) {
        id -> Uuid,
        seq -> Int8,
        actor -> Varchar,
        action -> Varchar,
        entity_type -> Varchar,
        entity_id -> Varchar,
        summary -> Text,
        source -> Varchar,
        details -> Jsonb,
        ip -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Audit (2026-11-22-000024_audit).
// ---------------------------------------------------------------------------

diesel::table! {
    audit_entries (id) {
        id -> Uuid,
        seq -> Int8,
        actor -> Varchar,
        action -> Varchar,
        resource -> Varchar,
        metadata -> Jsonb,
        ip -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Gateway (2026-11-22-000025_gateway).
// ---------------------------------------------------------------------------

diesel::table! {
    gateway_routing_rules (id) {
        id -> Uuid,
        name -> Varchar,
        priority -> Int4,
        enabled -> Bool,
        host -> Nullable<Varchar>,
        path_prefix -> Nullable<Varchar>,
        path_regex -> Nullable<Varchar>,
        headers -> Jsonb,
        upstream -> Jsonb,
        strategy -> Varchar,
        require_tls -> Bool,
        rate_limit_override -> Nullable<Varchar>,
        health_check -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    gateway_request_log (id) {
        id -> Uuid,
        request_id -> Varchar,
        method -> Varchar,
        host -> Varchar,
        path -> Varchar,
        status -> Int4,
        duration_ms -> Int8,
        upstream -> Nullable<Varchar>,
        backend_id -> Nullable<Uuid>,
        recorded_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Departments (CORE) (2026-11-22-000026_departments).
// ---------------------------------------------------------------------------

diesel::table! {
    departments (id) {
        id -> Uuid,
        name -> Varchar,
        slug -> Varchar,
        description -> Text,
        email -> Nullable<Varchar>,
        status -> Varchar,
        is_default -> Bool,
        sla_first_response_minutes -> Int4,
        sla_resolution_minutes -> Int4,
        working_hours -> Jsonb,
        auto_reply -> Jsonb,
        agents -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    department_routing_rules (id) {
        id -> Uuid,
        department_id -> Uuid,
        match_type -> Varchar,
        match_value -> Varchar,
        sort_order -> Int4,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Meetings (2026-11-22-000027_meetings).
// ---------------------------------------------------------------------------

diesel::table! {
    meeting_rooms (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Nullable<Text>,
        host_id -> Varchar,
        status -> Varchar,
        access_model -> Varchar,
        product_id -> Nullable<Varchar>,
        max_participants -> Int4,
        waiting_room_enabled -> Bool,
        join_token -> Varchar,
        starts_at -> Nullable<Timestamptz>,
        ends_at -> Nullable<Timestamptz>,
        timezone -> Varchar,
        recurrence_rule -> Nullable<Varchar>,
        scheduler_job_id -> Nullable<Varchar>,
        scheduler_status -> Varchar,
        started_at -> Nullable<Timestamptz>,
        ended_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    meeting_participants (room_id, user_id) {
        room_id -> Uuid,
        user_id -> Varchar,
        role -> Varchar,
        status -> Varchar,
        joined_at -> Nullable<Timestamptz>,
        left_at -> Nullable<Timestamptz>,
        last_seen -> Timestamptz,
        audio_muted -> Bool,
        video_muted -> Bool,
        sharing_screen -> Bool,
        hand_raised -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    meeting_messages (id) {
        id -> Uuid,
        room_id -> Uuid,
        author_id -> Varchar,
        body -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    meeting_recordings (id) {
        id -> Uuid,
        room_id -> Uuid,
        started_by -> Varchar,
        status -> Varchar,
        storage_target -> Nullable<Varchar>,
        storage_key -> Nullable<Varchar>,
        storage_url -> Nullable<Varchar>,
        size_bytes -> Nullable<Int8>,
        duration_secs -> Nullable<Int8>,
        started_at -> Timestamptz,
        stopped_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Product Management (2026-11-22-000028_products).
// ---------------------------------------------------------------------------

diesel::table! {
    product_taxonomy (id) {
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
    products (id) {
        id -> Uuid,
        slug -> Varchar,
        name -> Varchar,
        description -> Text,
        kind -> Varchar,
        status -> Varchar,
        pricing_model -> Varchar,
        billing_period -> Nullable<Varchar>,
        price -> Numeric,
        price_currency -> Varchar,
        renewal_price -> Nullable<Numeric>,
        renewal_price_currency -> Nullable<Varchar>,
        trial_days -> Int4,
        vendor_company_id -> Nullable<Uuid>,
        group_id -> Nullable<Uuid>,
        category_id -> Nullable<Uuid>,
        software_id -> Nullable<Uuid>,
        license_template_name -> Nullable<Varchar>,
        license_seats -> Int4,
        license_download_limit -> Int4,
        license_duration_days -> Nullable<Int8>,
        license_requires_license -> Bool,
        support_department_id -> Nullable<Uuid>,
        support_department_name -> Nullable<Varchar>,
        bundle_rule -> Varchar,
        bundle_discount_bps -> Nullable<Int8>,
        listed -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_options (id) {
        id -> Uuid,
        product_id -> Uuid,
        name -> Varchar,
        values -> Jsonb,
        required -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_variants (id) {
        id -> Uuid,
        product_id -> Uuid,
        sku -> Varchar,
        label -> Varchar,
        option_values -> Jsonb,
        price_delta -> Numeric,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_bundle_items (id) {
        id -> Uuid,
        bundle_product_id -> Uuid,
        child_product_id -> Uuid,
        quantity -> Int4,
        price_override -> Nullable<Numeric>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_price_tiers (id) {
        id -> Uuid,
        product_id -> Uuid,
        slug -> Varchar,
        label -> Varchar,
        price -> Numeric,
        price_currency -> Varchar,
        renewal_price -> Nullable<Numeric>,
        renewal_price_currency -> Nullable<Varchar>,
        billing_period -> Nullable<Varchar>,
        trial_days -> Int4,
        seats -> Int4,
        per_seat -> Bool,
        download_limit -> Int4,
        features -> Jsonb,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_regional_prices (id) {
        id -> Uuid,
        product_id -> Uuid,
        region -> Varchar,
        price -> Numeric,
        price_currency -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_usage_prices (id) {
        id -> Uuid,
        product_id -> Uuid,
        metric -> Varchar,
        unit_price -> Numeric,
        currency -> Varchar,
        included_units -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_tax_rules (id) {
        id -> Uuid,
        country -> Varchar,
        name -> Varchar,
        rate_bps -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_inventory (id) {
        id -> Uuid,
        product_id -> Uuid,
        variant_id -> Nullable<Uuid>,
        quantity -> Int8,
        reserved -> Int8,
        low_stock_threshold -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_files (id) {
        id -> Uuid,
        product_id -> Uuid,
        software_id -> Nullable<Uuid>,
        software_version -> Nullable<Varchar>,
        release_id -> Nullable<Uuid>,
        file_id -> Nullable<Uuid>,
        filename -> Nullable<Varchar>,
        checksum_sha256 -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_versions (id) {
        id -> Uuid,
        product_id -> Uuid,
        version -> Varchar,
        software_id -> Nullable<Uuid>,
        release_id -> Nullable<Uuid>,
        changelog -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_tags (id) {
        id -> Uuid,
        slug -> Varchar,
        name -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_tag_assignments (product_id, tag_id) {
        product_id -> Uuid,
        tag_id -> Uuid,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// MCP servers (2026-11-22-000029_mcp).
// ---------------------------------------------------------------------------

diesel::table! {
    mcp_servers (id) {
        id -> Uuid,
        slug -> Varchar,
        name -> Varchar,
        owner_id -> Uuid,
        company_id -> Nullable<Uuid>,
        site_id -> Nullable<Uuid>,
        product_id -> Nullable<Uuid>,
        status -> Varchar,
        is_public -> Bool,
        allowed_tools -> Jsonb,
        rate_limit_per_minute -> Int4,
        read_only_role -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    mcp_settings (key) {
        key -> Varchar,
        read_only_role -> Nullable<Varchar>,
        default_rate_limit_per_minute -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// AI (2026-11-22-000030_ai).
// ---------------------------------------------------------------------------

diesel::table! {
    ai_provider_connections (id) {
        id -> Uuid,
        name -> Varchar,
        kind -> Varchar,
        base_url -> Varchar,
        model -> Varchar,
        api_key_encrypted -> Nullable<Text>,
        key_fingerprint -> Nullable<Varchar>,
        enabled -> Bool,
        rate_limit_per_minute -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    ai_usage_records (id) {
        id -> Uuid,
        provider_id -> Uuid,
        provider_kind -> Varchar,
        actor -> Varchar,
        use_case -> Varchar,
        model -> Varchar,
        prompt_tokens -> Nullable<Int4>,
        completion_tokens -> Nullable<Int4>,
        total_tokens -> Nullable<Int4>,
        estimated_cost_usd -> Nullable<Float8>,
        latency_ms -> Int8,
        status -> Varchar,
        error_code -> Nullable<Int4>,
        user_id -> Nullable<Uuid>,
        company_id -> Nullable<Uuid>,
        denial_reason -> Nullable<Varchar>,
        used_personal_key -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    ai_drafts (id) {
        id -> Uuid,
        kind -> Varchar,
        title -> Nullable<Varchar>,
        subject -> Nullable<Varchar>,
        body -> Text,
        prompt -> Nullable<Text>,
        provider_id -> Nullable<Uuid>,
        provider_kind -> Varchar,
        model -> Varchar,
        actor -> Varchar,
        status -> Varchar,
        ai_generated -> Bool,
        target_type -> Nullable<Varchar>,
        target_id -> Nullable<Varchar>,
        sources -> Jsonb,
        usage_id -> Nullable<Uuid>,
        reviewed_by -> Nullable<Varchar>,
        reviewed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Project Management (2026-11-22-000031_projects).
// ---------------------------------------------------------------------------

diesel::table! {
    projects (id) {
        id -> Uuid,
        name -> Text,
        description -> Text,
        methodology -> Text,
        status -> Text,
        visibility -> Text,
        owner_id -> Text,
        company_id -> Nullable<Uuid>,
        client_id -> Nullable<Text>,
        start_date -> Nullable<Timestamptz>,
        due_date -> Nullable<Timestamptz>,
        color -> Nullable<Text>,
        icon -> Nullable<Text>,
        labels -> Jsonb,
        custom_field_definitions -> Jsonb,
        archived -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_members (id) {
        id -> Uuid,
        project_id -> Uuid,
        user_id -> Text,
        role -> Text,
        joined_at -> Timestamptz,
    }
}

diesel::table! {
    project_boards (id) {
        id -> Uuid,
        project_id -> Uuid,
        name -> Text,
        kind -> Text,
        wip_limit -> Nullable<Int4>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_board_columns (id) {
        id -> Uuid,
        board_id -> Uuid,
        name -> Text,
        position -> Int4,
        status_key -> Text,
        wip_limit -> Nullable<Int4>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_milestones (id) {
        id -> Uuid,
        project_id -> Uuid,
        name -> Text,
        description -> Text,
        due_date -> Nullable<Timestamptz>,
        reached -> Bool,
        reached_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_sprints (id) {
        id -> Uuid,
        project_id -> Uuid,
        name -> Text,
        goal -> Text,
        status -> Text,
        start_date -> Nullable<Timestamptz>,
        end_date -> Nullable<Timestamptz>,
        velocity_planned -> Nullable<Float8>,
        velocity_actual -> Nullable<Float8>,
        standup_notes -> Jsonb,
        review_notes -> Nullable<Text>,
        retrospective_notes -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_phases (id) {
        id -> Uuid,
        project_id -> Uuid,
        name -> Text,
        kind -> Text,
        position -> Int4,
        status -> Text,
        gate_status -> Text,
        gate_decision -> Nullable<Text>,
        start_date -> Nullable<Timestamptz>,
        end_date -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_cycles (id) {
        id -> Uuid,
        project_id -> Uuid,
        name -> Text,
        appetite_weeks -> Int4,
        status -> Text,
        started_at -> Nullable<Timestamptz>,
        ends_at -> Nullable<Timestamptz>,
        cooldown_weeks -> Int4,
        pitch -> Nullable<Text>,
        betting_decision -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_program_increments (id) {
        id -> Uuid,
        project_id -> Uuid,
        name -> Text,
        status -> Text,
        start_date -> Nullable<Timestamptz>,
        end_date -> Nullable<Timestamptz>,
        objectives -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_business_cases (id) {
        id -> Uuid,
        project_id -> Uuid,
        title -> Text,
        justification -> Text,
        benefits -> Jsonb,
        costs -> Jsonb,
        risks -> Jsonb,
        status -> Text,
        exception_report -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_tasks (id) {
        id -> Uuid,
        project_id -> Uuid,
        board_id -> Nullable<Uuid>,
        parent_id -> Nullable<Uuid>,
        epic_id -> Nullable<Uuid>,
        title -> Text,
        description -> Text,
        task_type -> Text,
        status -> Text,
        priority -> Text,
        assignee_id -> Nullable<Text>,
        reporter_id -> Text,
        estimate_points -> Nullable<Float8>,
        estimate_hours -> Nullable<Float8>,
        labels -> Jsonb,
        custom_values -> Jsonb,
        start_date -> Nullable<Timestamptz>,
        due_date -> Nullable<Timestamptz>,
        completed_at -> Nullable<Timestamptz>,
        sprint_id -> Nullable<Uuid>,
        phase_id -> Nullable<Uuid>,
        cycle_id -> Nullable<Uuid>,
        pi_id -> Nullable<Uuid>,
        milestone_id -> Nullable<Uuid>,
        process_group -> Nullable<Text>,
        knowledge_area -> Nullable<Text>,
        waste_flag -> Nullable<Bool>,
        pair_partner_id -> Nullable<Text>,
        test_first -> Bool,
        dmaic_phase -> Nullable<Text>,
        appetite_weeks -> Nullable<Int4>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_task_dependencies (id) {
        id -> Uuid,
        task_id -> Uuid,
        depends_on_task_id -> Uuid,
        kind -> Text,
        lag_days -> Int4,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    project_task_comments (id) {
        id -> Uuid,
        task_id -> Uuid,
        author_id -> Text,
        body -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_task_watchers (id) {
        id -> Uuid,
        task_id -> Uuid,
        user_id -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    project_task_attachments (id) {
        id -> Uuid,
        task_id -> Uuid,
        target -> Text,
        key -> Text,
        stored_key -> Text,
        filename -> Text,
        content_type -> Nullable<Text>,
        size_bytes -> Nullable<Int8>,
        uploaded_by -> Text,
        uploaded_at -> Timestamptz,
    }
}

diesel::table! {
    project_time_entries (id) {
        id -> Uuid,
        task_id -> Uuid,
        project_id -> Uuid,
        user_id -> Text,
        description -> Text,
        hours -> Float8,
        billable -> Bool,
        billed -> Bool,
        invoice_id -> Nullable<Text>,
        started_at -> Timestamptz,
        ended_at -> Nullable<Timestamptz>,
        timer_running -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_automation_rules (id) {
        id -> Uuid,
        project_id -> Uuid,
        name -> Text,
        trigger -> Text,
        conditions -> Jsonb,
        actions -> Jsonb,
        enabled -> Bool,
        run_count -> Int8,
        last_run_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_templates (id) {
        id -> Uuid,
        name -> Text,
        description -> Text,
        methodology -> Text,
        boards -> Jsonb,
        columns -> Jsonb,
        statuses -> Jsonb,
        phases -> Jsonb,
        custom_fields -> Jsonb,
        is_system -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_activity (id) {
        id -> Uuid,
        project_id -> Uuid,
        actor_id -> Text,
        action -> Text,
        target_type -> Text,
        target_id -> Nullable<Text>,
        details -> Jsonb,
        created_at -> Timestamptz,
    }
}

diesel::joinable!(users -> companies (company_id));
diesel::joinable!(user_roles -> users (user_id));
diesel::joinable!(user_roles -> roles (role_id));
diesel::joinable!(company_sites -> companies (company_id));
diesel::joinable!(domains -> companies (company_id));
diesel::joinable!(domains -> company_sites (site_id));
diesel::joinable!(licenses -> users (user_id));
diesel::joinable!(licenses -> companies (company_id));
diesel::joinable!(license_activations -> licenses (license_id));
diesel::joinable!(software -> companies (vendor_company_id));
diesel::joinable!(software -> software_taxonomy (group_id));
diesel::joinable!(software_versions -> software (software_id));
diesel::joinable!(software_releases -> software_versions (version_id));
diesel::joinable!(release_files -> software_releases (release_id));
diesel::joinable!(release_files -> software_versions (version_id));
diesel::joinable!(software_tag_assignments -> software (software_id));
diesel::joinable!(software_tag_assignments -> software_tags (tag_id));
diesel::joinable!(store_products -> software (software_id));
diesel::joinable!(store_products -> companies (company_id));
diesel::joinable!(store_pricing_tiers -> store_products (product_id));
diesel::joinable!(store_orders -> companies (company_id));
diesel::joinable!(store_order_items -> store_orders (order_id));
diesel::joinable!(store_order_items -> store_products (product_id));
diesel::joinable!(store_order_items -> store_pricing_tiers (tier_id));
diesel::joinable!(store_order_items -> software (software_id));
diesel::joinable!(store_order_items -> companies (company_id));
diesel::joinable!(store_entitlements -> licenses (license_id));
diesel::joinable!(store_entitlements -> store_orders (order_id));
diesel::joinable!(store_entitlements -> store_products (product_id));
diesel::joinable!(store_entitlements -> store_pricing_tiers (tier_id));
diesel::joinable!(store_entitlements -> software (software_id));
diesel::joinable!(store_entitlements -> software_releases (release_id));
diesel::joinable!(store_downloads -> licenses (license_id));
diesel::joinable!(store_downloads -> store_orders (order_id));
diesel::joinable!(store_downloads -> software_releases (release_id));
diesel::joinable!(store_catalog -> store_products (product_id));
diesel::joinable!(store_catalog -> software_releases (release_id));
diesel::joinable!(store_issued_licenses -> licenses (license_id));
diesel::joinable!(store_issued_licenses -> store_orders (order_id));
diesel::joinable!(store_issued_licenses -> companies (company_id));
diesel::joinable!(vcs_connections -> software (software_id));
diesel::joinable!(vcs_version_drafts -> vcs_connections (connection_id));
diesel::joinable!(vcs_version_drafts -> software (software_id));
diesel::joinable!(vcs_sync_history -> vcs_connections (connection_id));
diesel::joinable!(api_keys -> users (owner_id));
diesel::joinable!(reseller_links -> users (reseller_id));
diesel::joinable!(view_as_sessions -> users (admin_id));
diesel::joinable!(hosting_accounts -> hosting_plans (plan_id));
diesel::joinable!(hosting_accounts -> companies (company_id));
diesel::joinable!(hosting_order_claims -> hosting_plans (plan_id));
diesel::joinable!(cluster_nodes -> clusters (cluster_id));
diesel::joinable!(cluster_sync_plans -> clusters (cluster_id));
diesel::joinable!(cluster_sync_plans -> cluster_nodes (target_node_id));
diesel::joinable!(cluster_sync_logs -> cluster_sync_plans (plan_id));
diesel::joinable!(cluster_sync_logs -> clusters (cluster_id));
diesel::joinable!(cluster_sync_logs -> cluster_nodes (node_id));
diesel::joinable!(cluster_health_checks -> cluster_nodes (node_id));
diesel::joinable!(cluster_health_checks -> clusters (cluster_id));
diesel::joinable!(backup_runs -> backup_jobs (job_id));
diesel::joinable!(provider_servers -> provider_connections (connection_id));
diesel::joinable!(tasks -> users (assignee_id));
diesel::joinable!(tasks -> companies (company_id));
diesel::joinable!(task_comments -> tasks (task_id));
diesel::joinable!(task_comments -> users (author_id));
diesel::joinable!(task_checklist_items -> tasks (task_id));
diesel::joinable!(invoices -> companies (company_id));
diesel::joinable!(invoice_items -> invoices (invoice_id));
diesel::joinable!(invoice_payments -> invoices (invoice_id));
diesel::joinable!(credit_notes -> invoices (invoice_id));
diesel::joinable!(panel_synced_records -> panel_connections (connection_id));
diesel::joinable!(payment_intents -> payment_connections (connection_id));
diesel::joinable!(payment_transactions -> payment_intents (intent_id));
diesel::joinable!(payment_transactions -> payment_connections (connection_id));
diesel::joinable!(payment_methods -> payment_connections (connection_id));
diesel::joinable!(gateway_subscriptions -> payment_connections (connection_id));
diesel::joinable!(gateway_subscriptions -> payment_methods (payment_method_id));
diesel::joinable!(split_parts -> split_bills (split_id));
diesel::joinable!(split_parts -> payment_connections (connection_id));
diesel::joinable!(split_parts -> payment_transactions (paid_transaction_id));
diesel::joinable!(bank_transfer_verifications -> split_bills (split_id));
diesel::joinable!(bank_transfer_verifications -> split_parts (part_id));
diesel::joinable!(bank_transfer_verifications -> bank_accounts (bank_account_id));
diesel::joinable!(bank_transfer_verification_audit -> bank_transfer_verifications (verification_id));
diesel::joinable!(pool_usage_samples -> capacity_pools (pool_id));
diesel::joinable!(allocation_plans -> capacity_pools (pool_id));
diesel::joinable!(analytics_report_runs -> analytics_reports (definition_id));
diesel::joinable!(analytics_scheduled_reports -> analytics_reports (definition_id));
diesel::joinable!(email_send_log -> email_connections (connection_id));
diesel::joinable!(email_template_versions -> email_templates (template_id));
diesel::joinable!(newsletter_lists -> email_connections (default_connection_id));
diesel::joinable!(newsletter_subscribers -> newsletter_lists (list_id));
diesel::joinable!(newsletter_campaigns -> newsletter_lists (list_id));
diesel::joinable!(newsletter_campaigns -> email_templates (template_id));
diesel::joinable!(newsletter_campaigns -> email_connections (connection_id));
diesel::joinable!(newsletter_tracking_events -> newsletter_campaigns (campaign_id));
diesel::joinable!(newsletter_tracking_events -> newsletter_subscribers (subscriber_id));
diesel::joinable!(lb_backends -> load_balancers (balancer_id));
diesel::joinable!(lb_backend_stats -> lb_backends (backend_id));
diesel::joinable!(webhook_deliveries -> webhook_endpoints (endpoint_id));
diesel::joinable!(maintenance_history -> maintenance_windows (window_id));
diesel::joinable!(status_daily_uptime -> status_components (component_id));
diesel::joinable!(status_incident_updates -> status_incidents (incident_id));
diesel::joinable!(announcement_read_receipts -> announcements (announcement_id));
diesel::joinable!(google_drive_links -> google_accounts (account_id));
diesel::joinable!(route_redirects -> named_routes (route));
diesel::joinable!(notification_templates -> email_connections (email_connection_id));
diesel::joinable!(department_routing_rules -> departments (department_id));
diesel::joinable!(meeting_participants -> meeting_rooms (room_id));
diesel::joinable!(meeting_messages -> meeting_rooms (room_id));
diesel::joinable!(meeting_recordings -> meeting_rooms (room_id));
diesel::joinable!(products -> product_taxonomy (group_id));
diesel::joinable!(product_options -> products (product_id));
diesel::joinable!(product_variants -> products (product_id));
diesel::joinable!(product_bundle_items -> products (bundle_product_id));
diesel::joinable!(product_price_tiers -> products (product_id));
diesel::joinable!(product_regional_prices -> products (product_id));
diesel::joinable!(product_usage_prices -> products (product_id));
diesel::joinable!(product_inventory -> products (product_id));
diesel::joinable!(product_inventory -> product_variants (variant_id));
diesel::joinable!(product_files -> products (product_id));
diesel::joinable!(product_versions -> products (product_id));
diesel::joinable!(product_tag_assignments -> products (product_id));
diesel::joinable!(product_tag_assignments -> product_tags (tag_id));
diesel::joinable!(mcp_servers -> users (owner_id));
diesel::joinable!(mcp_servers -> companies (company_id));
diesel::joinable!(mcp_servers -> company_sites (site_id));
diesel::joinable!(ai_drafts -> ai_provider_connections (provider_id));
diesel::joinable!(ai_drafts -> ai_usage_records (usage_id));

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
    software_taxonomy,
    software,
    software_versions,
    software_releases,
    release_files,
    software_tags,
    software_tag_assignments,
    store_products,
    store_pricing_tiers,
    store_coupons,
    store_orders,
    store_order_items,
    store_entitlements,
    store_downloads,
    store_catalog,
    store_issued_licenses,
    vcs_connections,
    vcs_version_drafts,
    vcs_sync_history,
    vcs_provider_settings,
    api_versions,
    api_endpoints,
    api_prefix_settings,
    api_keys,
    api_access_rules,
    api_access_audit,
    certificates,
    certificate_renewals,
    certificate_settings,
    reseller_links,
    view_as_sessions,
    dashboard_settings,
    hosting_plans,
    hosting_accounts,
    hosting_order_claims,
    clusters,
    cluster_nodes,
    cluster_sync_plans,
    cluster_sync_items,
    cluster_sync_logs,
    cluster_health_checks,
    backup_jobs,
    backup_runs,
    storage_targets,
    provider_connections,
    provider_servers,
    tasks,
    task_comments,
    task_checklist_items,
    system_records,
    app_logs,
    logs_settings,
    invoices,
    invoice_items,
    invoice_payments,
    credit_notes,
    billing_settings,
    invoice_sequences,
    panel_connections,
    panel_synced_records,
    payment_connections,
    payment_intents,
    payment_transactions,
    payment_methods,
    gateway_subscriptions,
    split_bills,
    split_parts,
    bank_accounts,
    bank_transfer_verifications,
    bank_transfer_verification_audit,
    bank_transfer_settings,
    capacity_pools,
    pool_usage_samples,
    allocation_plans,
    analytics_sales,
    analytics_license_snapshots,
    analytics_metric_samples,
    analytics_health_samples,
    analytics_reports,
    analytics_report_runs,
    analytics_scheduled_reports,
    email_connections,
    email_send_log,
    mail_smtp_settings,
    mail_log,
    email_templates,
    email_template_versions,
    newsletter_lists,
    newsletter_subscribers,
    newsletter_campaigns,
    newsletter_tracking_events,
    load_balancers,
    lb_backends,
    lb_backend_stats,
    webhook_endpoints,
    webhook_deliveries,
    import_jobs,
    export_jobs,
    maintenance_windows,
    maintenance_history,
    status_components,
    status_daily_uptime,
    status_incidents,
    status_incident_updates,
    announcements,
    changelog_entries,
    announcement_read_receipts,
    auth_recovery_codes,
    google_accounts,
    google_drive_links,
    security_ip_lists,
    security_reputation,
    security_failed_auth_clusters,
    named_routes,
    route_redirects,
    cloudflare_settings,
    cloudflare_purge_history,
    search_documents,
    notifications,
    notification_preferences,
    notification_templates,
    saved_views,
    timeline_entries,
    audit_entries,
    gateway_routing_rules,
    gateway_request_log,
    departments,
    department_routing_rules,
    meeting_rooms,
    meeting_participants,
    meeting_messages,
    meeting_recordings,
    product_taxonomy,
    products,
    product_options,
    product_variants,
    product_bundle_items,
    product_price_tiers,
    product_regional_prices,
    product_usage_prices,
    product_tax_rules,
    product_inventory,
    product_files,
    product_versions,
    product_tags,
    product_tag_assignments,
    mcp_servers,
    mcp_settings,
    ai_provider_connections,
    ai_usage_records,
    ai_drafts,
    projects,
    project_members,
    project_boards,
    project_board_columns,
    project_milestones,
    project_sprints,
    project_phases,
    project_cycles,
    project_program_increments,
    project_business_cases,
    project_tasks,
    project_task_dependencies,
    project_task_comments,
    project_task_watchers,
    project_task_attachments,
    project_time_entries,
    project_automation_rules,
    project_templates,
    project_activity,
);

// ---------------------------------------------------------------------------
// TimeTracker (migration 000032_timetracker; batch 2 notes §4).
// ---------------------------------------------------------------------------

diesel::table! {
    timetracker_timers (id) {
        id -> Uuid,
        user_id -> Text,
        project_id -> Nullable<Uuid>,
        task_id -> Nullable<Uuid>,
        description -> Nullable<Text>,
        billable -> Bool,
        status -> Varchar,
        started_at -> Timestamptz,
        paused_total_secs -> Int8,
        paused_at -> Nullable<Timestamptz>,
        idle_total_secs -> Int8,
        stopped_at -> Nullable<Timestamptz>,
        entry_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_entries (id) {
        id -> Uuid,
        user_id -> Text,
        project_id -> Nullable<Uuid>,
        task_id -> Nullable<Uuid>,
        timer_id -> Nullable<Uuid>,
        description -> Text,
        started_at -> Timestamptz,
        ended_at -> Timestamptz,
        duration_secs -> Int8,
        billable -> Bool,
        approval -> Varchar,
        approved_by -> Nullable<Text>,
        approved_at -> Nullable<Timestamptz>,
        rejection_note -> Nullable<Text>,
        reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_activity (id) {
        id -> Uuid,
        user_id -> Text,
        timer_id -> Nullable<Uuid>,
        project_id -> Nullable<Uuid>,
        task_id -> Nullable<Uuid>,
        activity_score -> Int4,
        idle_secs -> Int8,
        interval_start -> Timestamptz,
        interval_end -> Timestamptz,
        triggered_auto_pause -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_app_usage (id) {
        id -> Uuid,
        user_id -> Text,
        application_name -> Text,
        domain -> Nullable<Text>,
        window_title_hash -> Nullable<Text>,
        category -> Varchar,
        duration_secs -> Int8,
        interval_start -> Timestamptz,
        interval_end -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_screenshots (id) {
        id -> Uuid,
        user_id -> Text,
        timer_id -> Nullable<Uuid>,
        project_id -> Nullable<Uuid>,
        task_id -> Nullable<Uuid>,
        storage_target -> Text,
        storage_key -> Text,
        content_sha256 -> Text,
        content_type -> Varchar,
        size_bytes -> Int8,
        blurred -> Bool,
        captured_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_rates (id) {
        id -> Uuid,
        user_id -> Text,
        currency -> Varchar,
        rate -> Numeric,
        effective_from -> Timestamptz,
        effective_to -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_timesheets (id) {
        id -> Uuid,
        user_id -> Text,
        period_start -> Timestamptz,
        period_end -> Timestamptz,
        status -> Varchar,
        entry_ids -> Array<Uuid>,
        submitted_at -> Nullable<Timestamptz>,
        reviewed_by -> Nullable<Text>,
        reviewed_at -> Nullable<Timestamptz>,
        review_note -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_settings (id) {
        id -> Uuid,
        scope -> Text,
        idle_threshold_secs -> Int8,
        retention_days -> Int8,
        overtime_hours_per_week -> Numeric,
        overtime_multiplier -> Numeric,
        low_activity_threshold -> Int8,
        storage_target -> Text,
        default_currency -> Varchar,
        auto_approve_timer_entries -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_user_settings (id) {
        id -> Uuid,
        user_id -> Text,
        screenshots_enabled -> Bool,
        blur_screenshots -> Bool,
        idle_threshold_secs -> Nullable<Int8>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_teams (id) {
        id -> Uuid,
        company_id -> Uuid,
        name -> Text,
        description -> Nullable<Text>,
        manager_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_team_members (id) {
        id -> Uuid,
        team_id -> Uuid,
        user_id -> Text,
        role -> Varchar,
        joined_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_staff_settings (id) {
        id -> Uuid,
        user_id -> Text,
        company_id -> Uuid,
        role -> Varchar,
        manager_id -> Nullable<Text>,
        screenshots_enabled -> Nullable<Bool>,
        blur_screenshots -> Nullable<Bool>,
        idle_threshold_secs -> Nullable<Int8>,
        activity_level -> Nullable<Varchar>,
        timezone -> Nullable<Text>,
        working_hours_start -> Nullable<Text>,
        working_hours_end -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_staff_invites (id) {
        id -> Uuid,
        company_id -> Uuid,
        email -> Text,
        role -> Varchar,
        team_id -> Nullable<Uuid>,
        invited_by -> Text,
        token_hash -> Text,
        status -> Varchar,
        expires_at -> Timestamptz,
        accepted_at -> Nullable<Timestamptz>,
        accepted_user_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_buyer_settings (id) {
        id -> Uuid,
        company_id -> Uuid,
        screenshots_enabled -> Nullable<Bool>,
        screenshot_interval_secs -> Nullable<Int8>,
        activity_level -> Nullable<Varchar>,
        idle_timeout_secs -> Nullable<Int8>,
        blur_screenshots -> Nullable<Bool>,
        allowed_apps -> Array<Text>,
        blocked_apps -> Array<Text>,
        allowed_websites -> Array<Text>,
        blocked_websites -> Array<Text>,
        default_hourly_rate -> Nullable<Numeric>,
        timezone -> Nullable<Text>,
        working_hours_start -> Nullable<Text>,
        working_hours_end -> Nullable<Text>,
        working_days -> Array<Int8>,
        retention_days -> Nullable<Int8>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(timetracker_timers -> timetracker_entries (entry_id));
diesel::joinable!(timetracker_teams -> companies (company_id));
diesel::joinable!(timetracker_staff_settings -> companies (company_id));
diesel::joinable!(timetracker_staff_invites -> companies (company_id));
diesel::joinable!(timetracker_buyer_settings -> companies (company_id));
diesel::joinable!(timetracker_team_members -> timetracker_teams (team_id));
diesel::joinable!(timetracker_staff_invites -> timetracker_teams (team_id));

// ---------------------------------------------------------------------------
// Storage manager (migration 000033_storage_manager; batch 2 notes §5).
// ---------------------------------------------------------------------------

diesel::table! {
    storage_buckets (id) {
        id -> Uuid,
        name -> Varchar,
        provider_kind -> Varchar,
        endpoint -> Varchar,
        region -> Varchar,
        container -> Varchar,
        path -> Varchar,
        sftp_host -> Varchar,
        sftp_port -> Int4,
        sftp_username -> Varchar,
        sftp_auth_kind -> Varchar,
        sftp_keyfile -> Varchar,
        allowed_hosts -> Array<Text>,
        credentials_encrypted -> Nullable<Text>,
        key_fingerprint -> Nullable<Varchar>,
        encryption_at_rest -> Bool,
        enabled -> Bool,
        company_id -> Nullable<Varchar>,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_bindings (id) {
        id -> Uuid,
        module_id -> Varchar,
        bucket_id -> Uuid,
        key_prefix -> Varchar,
        company_id -> Nullable<Varchar>,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_quotas (id) {
        id -> Uuid,
        scope -> Varchar,
        scope_id -> Varchar,
        bytes_limit -> Nullable<Int8>,
        objects_limit -> Nullable<Int8>,
        soft_bytes_limit -> Nullable<Int8>,
        soft_objects_limit -> Nullable<Int8>,
        used_bytes -> Int8,
        used_objects -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_usage_daily (id) {
        id -> Uuid,
        date -> Date,
        bucket_id -> Uuid,
        company_id -> Nullable<Varchar>,
        user_id -> Varchar,
        module_id -> Varchar,
        bytes_total -> Int8,
        objects_total -> Int8,
        bytes_delta -> Int8,
        objects_delta -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_lifecycle_rules (id) {
        id -> Uuid,
        bucket_id -> Uuid,
        retention_days -> Int8,
        auto_delete -> Bool,
        archive_transition -> Bool,
        archive_prefix -> Varchar,
        enabled -> Bool,
        last_run_at -> Nullable<Timestamptz>,
        last_run_summary -> Nullable<Text>,
        last_error -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_signed_url_revocations (id) {
        id -> Uuid,
        bucket_id -> Uuid,
        key -> Varchar,
        issued_by -> Varchar,
        expires_at -> Timestamptz,
        revoked_at -> Nullable<Timestamptz>,
        revoke_reason -> Nullable<Varchar>,
        provider_url_issued -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_health_checks (id) {
        id -> Uuid,
        bucket_id -> Uuid,
        ok -> Bool,
        latency_ms -> Int8,
        detail -> Varchar,
        checked_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Events (migration 000034_events; batch 2 notes §6).
// ---------------------------------------------------------------------------

diesel::table! {
    events (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Nullable<Text>,
        slug -> Varchar,
        category -> Array<Text>,
        tags -> Array<Text>,
        venue_name -> Nullable<Varchar>,
        venue_address -> Nullable<Varchar>,
        venue_latitude -> Nullable<Float8>,
        venue_longitude -> Nullable<Float8>,
        venue_capacity -> Nullable<Int8>,
        starts_at -> Timestamptz,
        ends_at -> Timestamptz,
        timezone -> Varchar,
        visibility -> Varchar,
        status -> Varchar,
        organizer_id -> Varchar,
        company_id -> Nullable<Varchar>,
        cover_storage_target -> Nullable<Varchar>,
        cover_storage_key -> Nullable<Varchar>,
        recurrence -> Nullable<Jsonb>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    event_occurrences (id) {
        id -> Uuid,
        event_id -> Uuid,
        sequence -> Int4,
        starts_at -> Timestamptz,
        ends_at -> Timestamptz,
        status -> Varchar,
        venue_name -> Nullable<Varchar>,
        venue_address -> Nullable<Varchar>,
        venue_latitude -> Nullable<Float8>,
        venue_longitude -> Nullable<Float8>,
        venue_capacity -> Nullable<Int8>,
        overridden -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    event_ticket_types (id) {
        id -> Uuid,
        event_id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        price -> Numeric,
        currency -> Varchar,
        quantity -> Int8,
        sold -> Int8,
        sales_start -> Nullable<Timestamptz>,
        sales_end -> Nullable<Timestamptz>,
        per_order_limit -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    event_registrations (id) {
        id -> Uuid,
        event_id -> Uuid,
        occurrence_id -> Nullable<Uuid>,
        ticket_type_id -> Uuid,
        user_id -> Varchar,
        quantity -> Int8,
        total_amount -> Numeric,
        currency -> Varchar,
        status -> Varchar,
        order_id -> Nullable<Varchar>,
        payment_reference -> Nullable<Varchar>,
        payment_provider -> Nullable<Varchar>,
        check_in_code -> Varchar,
        checked_in_at -> Nullable<Timestamptz>,
        cancelled_at -> Nullable<Timestamptz>,
        refund_reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    event_attendees (id) {
        id -> Uuid,
        event_id -> Uuid,
        registration_id -> Uuid,
        name -> Varchar,
        email -> Varchar,
        custom_answers -> Jsonb,
        checked_in_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    event_waitlist (id) {
        id -> Uuid,
        event_id -> Uuid,
        ticket_type_id -> Uuid,
        user_id -> Varchar,
        quantity -> Int8,
        position -> Int8,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    event_reminders (id) {
        id -> Uuid,
        event_id -> Uuid,
        offset_minutes -> Int8,
        occurrence_id -> Nullable<Uuid>,
        title_template -> Nullable<Varchar>,
        body_template -> Nullable<Text>,
        status -> Varchar,
        scheduler_job_id -> Nullable<Varchar>,
        scheduler_status -> Varchar,
        sent_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(event_occurrences -> events (event_id));
diesel::joinable!(event_ticket_types -> events (event_id));
diesel::joinable!(event_registrations -> events (event_id));
diesel::joinable!(event_registrations -> event_ticket_types (ticket_type_id));
diesel::joinable!(event_attendees -> event_registrations (registration_id));
diesel::joinable!(event_waitlist -> events (event_id));
diesel::joinable!(event_reminders -> events (event_id));

// ---------------------------------------------------------------------------
// Todos (migration 000035_todos; batch 2 notes §7). The notes write
// `Integer`/`BigInt`; normalised to the file's `Int4`/`Int8` style.
// ---------------------------------------------------------------------------

diesel::table! {
    todo_lists (id) {
        id -> Uuid,
        owner_id -> Varchar,
        name -> Varchar,
        color -> Varchar,
        icon -> Varchar,
        archived -> Bool,
        sort_order -> Int4,
        is_default -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    todos (id) {
        id -> Uuid,
        list_id -> Uuid,
        owner_id -> Varchar,
        title -> Varchar,
        notes -> Nullable<Text>,
        due_at -> Nullable<Timestamptz>,
        timezone -> Varchar,
        priority -> Varchar,
        status -> Varchar,
        tags -> Jsonb,
        subtasks -> Jsonb,
        attachments -> Jsonb,
        recurrence_frequency -> Nullable<Varchar>,
        recurrence_interval -> Int4,
        recurrence_count -> Nullable<Int4>,
        recurrence_until -> Nullable<Timestamptz>,
        recurrence_remaining -> Nullable<Int4>,
        recurrence_group_id -> Nullable<Uuid>,
        position -> Int4,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    todo_subtasks (id) {
        id -> Uuid,
        todo_id -> Uuid,
        title -> Varchar,
        done -> Bool,
        position -> Int4,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    todo_shares (id) {
        id -> Uuid,
        list_id -> Uuid,
        owner_id -> Varchar,
        shared_with_user_id -> Varchar,
        permission -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    todo_completion_log (id) {
        id -> Uuid,
        todo_id -> Uuid,
        user_id -> Varchar,
        action -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    todo_reminders (id) {
        id -> Uuid,
        todo_id -> Uuid,
        owner_id -> Varchar,
        offset_minutes -> Int8,
        remind_at -> Timestamptz,
        due_at -> Timestamptz,
        status -> Varchar,
        scheduler_job_id -> Nullable<Varchar>,
        scheduler_status -> Varchar,
        sent_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// AI access allowlists (migration 000036_ai_allowlists; batch 2 notes §8).
// ---------------------------------------------------------------------------

diesel::table! {
    ai_company_policies (id) {
        id -> Uuid,
        company_id -> Uuid,
        allowed_models -> Nullable<Jsonb>,
        allowed_features -> Nullable<Jsonb>,
        monthly_token_cap -> Nullable<Int8>,
        monthly_cost_cap_usd -> Nullable<Numeric>,
        enabled -> Bool,
        ai_enabled -> Bool,
        disabled_features -> Nullable<Jsonb>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    ai_user_policies (id) {
        id -> Uuid,
        user_id -> Uuid,
        company_id -> Uuid,
        allowed_models -> Nullable<Jsonb>,
        allowed_features -> Nullable<Jsonb>,
        monthly_token_cap -> Nullable<Int8>,
        monthly_cost_cap_usd -> Nullable<Numeric>,
        enabled -> Bool,
        ai_enabled -> Bool,
        disabled_features -> Nullable<Jsonb>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    ai_personal_keys (id) {
        id -> Uuid,
        user_id -> Uuid,
        provider_kind -> Varchar,
        base_url -> Nullable<Varchar>,
        model -> Nullable<Varchar>,
        api_key_encrypted -> Text,
        key_fingerprint -> Varchar,
        monthly_token_cap -> Nullable<Int8>,
        monthly_cost_cap_usd -> Nullable<Numeric>,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    ai_user_mcp_servers (id) {
        id -> Uuid,
        user_id -> Uuid,
        name -> Varchar,
        endpoint_url -> Varchar,
        auth_token_encrypted -> Nullable<Text>,
        token_fingerprint -> Nullable<Varchar>,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(ai_company_policies -> companies (company_id));
diesel::joinable!(ai_user_policies -> users (user_id));
diesel::joinable!(ai_user_policies -> companies (company_id));
diesel::joinable!(ai_personal_keys -> users (user_id));
diesel::joinable!(ai_user_mcp_servers -> users (user_id));
diesel::joinable!(ai_usage_records -> users (user_id));
diesel::joinable!(ai_usage_records -> companies (company_id));

// ---------------------------------------------------------------------------
// Calculator Enterprise (2026-11-22-000100_calculator).
// ---------------------------------------------------------------------------

diesel::table! {
    calculator_saved_calculations (id) {
        id -> Uuid,
        company_id -> Uuid,
        calculator_type -> Varchar,
        title -> Varchar,
        inputs -> Jsonb,
        results -> Jsonb,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    calculator_reports (id) {
        id -> Uuid,
        calculation_id -> Uuid,
        company_id -> Uuid,
        title -> Varchar,
        share_token -> Varchar,
        visibility -> Varchar,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    calculator_history (id) {
        id -> Uuid,
        company_id -> Uuid,
        calculator_type -> Varchar,
        inputs -> Jsonb,
        results -> Jsonb,
        actor -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    calculator_currency_rates (id) {
        id -> Uuid,
        base_currency -> Bpchar,
        target_currency -> Bpchar,
        rate -> Float8,
        provider -> Varchar,
        fetched_at -> Timestamptz,
        max_age_seconds -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(calculator_reports -> calculator_saved_calculations (calculation_id));

// ---------------------------------------------------------------------------
// Human Resource Management (2026-11-22-000037_hr).
// ---------------------------------------------------------------------------

diesel::table! {
    hr_employees (id) {
        id -> Uuid,
        user_id -> Varchar,
        employee_code -> Varchar,
        position_id -> Nullable<Uuid>,
        department_id -> Nullable<Varchar>,
        department_verified -> Bool,
        manager_id -> Nullable<Uuid>,
        status -> Varchar,
        hire_date -> Date,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_positions (id) {
        id -> Uuid,
        title -> Varchar,
        department_id -> Nullable<Varchar>,
        level -> Nullable<Varchar>,
        salary_band_min -> Nullable<Numeric>,
        salary_band_max -> Nullable<Numeric>,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_contracts (id) {
        id -> Uuid,
        employee_id -> Uuid,
        contract_type -> Varchar,
        start_date -> Date,
        end_date -> Nullable<Date>,
        salary_amount -> Numeric,
        currency -> Varchar,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_leave_types (id) {
        id -> Uuid,
        name -> Varchar,
        code -> Varchar,
        default_days_per_year -> Numeric,
        paid -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_leave_balances (id) {
        id -> Uuid,
        employee_id -> Uuid,
        leave_type_id -> Uuid,
        year -> Int4,
        entitled_days -> Numeric,
        used_days -> Numeric,
        carried_over_days -> Numeric,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_leave_requests (id) {
        id -> Uuid,
        employee_id -> Uuid,
        leave_type_id -> Uuid,
        start_date -> Date,
        end_date -> Date,
        days -> Numeric,
        status -> Varchar,
        reason -> Text,
        reviewed_by -> Nullable<Varchar>,
        review_note -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_attendance_records (id) {
        id -> Uuid,
        employee_id -> Uuid,
        user_id -> Varchar,
        work_date -> Date,
        minutes_worked -> Int4,
        source -> Varchar,
        entry_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_payroll_runs (id) {
        id -> Uuid,
        period -> Varchar,
        currency -> Varchar,
        status -> Varchar,
        total_gross -> Numeric,
        total_net -> Numeric,
        total_employees -> Int4,
        finalized_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_payroll_items (id) {
        id -> Uuid,
        payroll_run_id -> Uuid,
        employee_id -> Uuid,
        gross -> Numeric,
        adjustments -> Numeric,
        deductions -> Numeric,
        net -> Numeric,
        notes -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_review_cycles (id) {
        id -> Uuid,
        name -> Varchar,
        period_start -> Date,
        period_end -> Date,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_reviews (id) {
        id -> Uuid,
        cycle_id -> Uuid,
        employee_id -> Uuid,
        reviewer_id -> Varchar,
        rating -> Nullable<Int2>,
        goals -> Jsonb,
        reviewer_notes -> Text,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_checklists (id) {
        id -> Uuid,
        employee_id -> Uuid,
        kind -> Varchar,
        status -> Varchar,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_checklist_items (id) {
        id -> Uuid,
        checklist_id -> Uuid,
        title -> Varchar,
        completed -> Bool,
        completed_by -> Nullable<Varchar>,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_documents (id) {
        id -> Uuid,
        employee_id -> Uuid,
        title -> Varchar,
        document_type -> Varchar,
        storage_file_id -> Varchar,
        mime_type -> Nullable<Varchar>,
        size_bytes -> Nullable<Int8>,
        uploaded_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(hr_contracts -> hr_employees (employee_id));
diesel::joinable!(hr_leave_balances -> hr_employees (employee_id));
diesel::joinable!(hr_leave_balances -> hr_leave_types (leave_type_id));
diesel::joinable!(hr_leave_requests -> hr_employees (employee_id));
diesel::joinable!(hr_leave_requests -> hr_leave_types (leave_type_id));
diesel::joinable!(hr_attendance_records -> hr_employees (employee_id));
diesel::joinable!(hr_payroll_items -> hr_payroll_runs (payroll_run_id));
diesel::joinable!(hr_payroll_items -> hr_employees (employee_id));
diesel::joinable!(hr_reviews -> hr_review_cycles (cycle_id));
diesel::joinable!(hr_reviews -> hr_employees (employee_id));
diesel::joinable!(hr_checklists -> hr_employees (employee_id));
diesel::joinable!(hr_checklist_items -> hr_checklists (checklist_id));
diesel::joinable!(hr_documents -> hr_employees (employee_id));

// ---------------------------------------------------------------------------
// Tax Management (2026-11-22-000038_tax).
// ---------------------------------------------------------------------------

diesel::table! {
    tax_rules (id) {
        id -> Uuid,
        company_id -> Uuid,
        country_code -> Varchar,
        region_code -> Nullable<Varchar>,
        name -> Varchar,
        tax_type -> Varchar,
        rate_percent -> Numeric,
        threshold_amount -> Nullable<Numeric>,
        currency -> Varchar,
        inclusive -> Bool,
        compound -> Bool,
        effective_from -> Date,
        effective_to -> Nullable<Date>,
        sort_order -> Int4,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    tax_settings (id) {
        id -> Uuid,
        company_id -> Uuid,
        fiscal_year_start_month -> Int4,
        fiscal_year_start_day -> Int4,
        filing_frequency -> Varchar,
        default_currency -> Varchar,
        default_country_code -> Nullable<Varchar>,
        default_region_code -> Nullable<Varchar>,
        reminder_days_before -> Int4,
        filing_due_days_after_period -> Int4,
        notify_user_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    tax_computations (id) {
        id -> Uuid,
        company_id -> Uuid,
        source_type -> Varchar,
        source_id -> Varchar,
        country_code -> Nullable<Varchar>,
        region_code -> Nullable<Varchar>,
        currency -> Varchar,
        base_amount -> Numeric,
        tax_total -> Numeric,
        grand_total -> Numeric,
        lines -> Jsonb,
        computed_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    tax_filings (id) {
        id -> Uuid,
        company_id -> Uuid,
        country_code -> Varchar,
        region_code -> Nullable<Varchar>,
        period_start -> Date,
        period_end -> Date,
        frequency -> Varchar,
        due_date -> Date,
        status -> Varchar,
        tax_total -> Numeric,
        submitted_at -> Nullable<Timestamptz>,
        submitted_via -> Nullable<Varchar>,
        reference -> Nullable<Varchar>,
        last_reminder_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    tax_connectors (id) {
        id -> Uuid,
        company_id -> Uuid,
        name -> Varchar,
        kind -> Varchar,
        endpoint_url -> Varchar,
        allowed_hosts -> Jsonb,
        auth_scheme -> Varchar,
        credential_encrypted -> Nullable<Text>,
        credential_fingerprint -> Nullable<Varchar>,
        signing_secret_encrypted -> Nullable<Text>,
        signing_secret_fingerprint -> Nullable<Varchar>,
        timeout_ms -> Int4,
        max_retries -> Int4,
        enabled -> Bool,
        request_mapping -> Jsonb,
        response_mapping -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    tax_audit (id) {
        id -> Uuid,
        company_id -> Uuid,
        actor -> Varchar,
        action -> Varchar,
        resource_type -> Varchar,
        resource_id -> Varchar,
        details -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Advisor + Risk (2026-11-22-000039_advisor_risk).
// ---------------------------------------------------------------------------

diesel::table! {
    advisor_suggestions (id) {
        id -> Uuid,
        company_id -> Nullable<Uuid>,
        rule_id -> Varchar,
        title -> Varchar,
        body -> Text,
        recommended_action -> Text,
        severity -> Varchar,
        severity_reason -> Text,
        status -> Varchar,
        source_type -> Varchar,
        source_id -> Varchar,
        source_ids -> Jsonb,
        figures -> Jsonb,
        dedupe_key -> Varchar,
        expires_at -> Nullable<Timestamptz>,
        snoozed_until -> Nullable<Timestamptz>,
        acknowledged_by -> Nullable<Varchar>,
        acknowledged_at -> Nullable<Timestamptz>,
        dismissed_by -> Nullable<Varchar>,
        dismissed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    advisor_rule_runs (id) {
        id -> Uuid,
        company_id -> Nullable<Uuid>,
        rule_id -> Varchar,
        ran_at -> Timestamptz,
        suggestions_created -> Int4,
        suggestions_updated -> Int4,
        inputs_summary -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risks (id) {
        id -> Uuid,
        company_id -> Nullable<Uuid>,
        title -> Varchar,
        description -> Text,
        category -> Varchar,
        likelihood -> Int2,
        impact -> Int2,
        score -> Int2,
        band -> Varchar,
        owner_id -> Varchar,
        status -> Varchar,
        next_review_at -> Nullable<Timestamptz>,
        alert_threshold -> Int2,
        created_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risk_mitigation_plans (id) {
        id -> Uuid,
        risk_id -> Uuid,
        title -> Varchar,
        description -> Text,
        target_date -> Nullable<Timestamptz>,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risk_mitigation_tasks (id) {
        id -> Uuid,
        plan_id -> Uuid,
        risk_id -> Uuid,
        title -> Varchar,
        description -> Text,
        assignee_id -> Nullable<Varchar>,
        status -> Varchar,
        due_date -> Nullable<Timestamptz>,
        external_project_task_id -> Nullable<Varchar>,
        awaiting_projects_handler -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risk_checklist_templates (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Text,
        category -> Varchar,
        created_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risk_checklist_template_items (id) {
        id -> Uuid,
        template_id -> Uuid,
        title -> Varchar,
        description -> Text,
        required -> Bool,
        sort_order -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risk_checklist_instances (id) {
        id -> Uuid,
        template_id -> Uuid,
        company_id -> Nullable<Uuid>,
        risk_id -> Nullable<Uuid>,
        title -> Varchar,
        status -> Varchar,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risk_checklist_instance_items (id) {
        id -> Uuid,
        instance_id -> Uuid,
        template_item_id -> Uuid,
        title -> Varchar,
        required -> Bool,
        sort_order -> Int4,
        completed -> Bool,
        evidence -> Text,
        completed_by -> Nullable<Varchar>,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risk_alerts (id) {
        id -> Uuid,
        risk_id -> Uuid,
        company_id -> Nullable<Uuid>,
        kind -> Varchar,
        message -> Text,
        score -> Nullable<Int2>,
        notified_user_id -> Nullable<Varchar>,
        figures -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(risk_mitigation_plans -> risks (risk_id));
diesel::joinable!(risk_mitigation_tasks -> risk_mitigation_plans (plan_id));
diesel::joinable!(risk_mitigation_tasks -> risks (risk_id));
diesel::joinable!(risk_checklist_template_items -> risk_checklist_templates (template_id));
diesel::joinable!(risk_checklist_instances -> risk_checklist_templates (template_id));
diesel::joinable!(risk_checklist_instances -> risks (risk_id));
diesel::joinable!(risk_checklist_instance_items -> risk_checklist_instances (instance_id));
diesel::joinable!(risk_alerts -> risks (risk_id));

// ---------------------------------------------------------------------------
// Subscriptions (2026-11-22-000040_subscriptions).
// ---------------------------------------------------------------------------

diesel::table! {
    subscriptions (id) {
        id -> Uuid,
        user_id -> Varchar,
        company_id -> Nullable<Uuid>,
        product_id -> Varchar,
        tier_id -> Nullable<Varchar>,
        order_id -> Varchar,
        module_id -> Nullable<Varchar>,
        status -> Varchar,
        billing_period -> Varchar,
        price -> Numeric,
        currency -> Varchar,
        trial_days -> Int4,
        trial_end -> Nullable<Timestamptz>,
        current_period_start -> Timestamptz,
        current_period_end -> Timestamptz,
        proration_policy -> Varchar,
        grace_period_days -> Int4,
        dunning_max_retries -> Int4,
        dunning_retry_interval_days -> Int4,
        cancel_at_period_end -> Bool,
        cancelled_at -> Nullable<Timestamptz>,
        pending_product_id -> Nullable<Varchar>,
        pending_tier_id -> Nullable<Varchar>,
        pending_price -> Nullable<Numeric>,
        pending_billing_period -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    subscription_events (id) {
        id -> Uuid,
        subscription_id -> Uuid,
        event -> Varchar,
        actor -> Nullable<Varchar>,
        reason -> Nullable<Varchar>,
        payload -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    dunning_attempts (id) {
        id -> Uuid,
        subscription_id -> Uuid,
        attempt_number -> Int4,
        reason -> Varchar,
        amount_minor -> Nullable<Int8>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(dunning_attempts -> subscriptions (subscription_id));

// ---------------------------------------------------------------------------
// Pricing Tables + Marketplace Builder (2026-11-22-000041_pricing_marketplace).
// Note: `pricing_tiers` here is the 000041 shape (pricing-table
// feature tiers); migration 000103_pricing creates a different
// `pricing_tiers` (volume quantity tiers) — see the header note.
// ---------------------------------------------------------------------------

diesel::table! {
    pricing_tables (id) {
        id -> Uuid,
        name -> Varchar,
        slug -> Varchar,
        description -> Text,
        currency -> Varchar,
        layout -> Varchar,
        theme_accent -> Varchar,
        status -> Varchar,
        yearly_discount_percent -> Numeric,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_tiers (id) {
        id -> Uuid,
        table_id -> Uuid,
        name -> Varchar,
        description -> Text,
        monthly_price -> Numeric,
        yearly_price -> Numeric,
        cta_label -> Varchar,
        cta_url -> Varchar,
        highlighted -> Bool,
        sort_order -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_tier_features (id) {
        id -> Uuid,
        tier_id -> Uuid,
        feature_text -> Varchar,
        included -> Bool,
        note -> Text,
        sort_order -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    marketplace_vendors (id) {
        id -> Uuid,
        user_id -> Varchar,
        company_id -> Nullable<Varchar>,
        company_name -> Varchar,
        slug -> Varchar,
        email -> Varchar,
        status -> Varchar,
        application_notes -> Text,
        review_reason -> Text,
        reviewed_by -> Nullable<Varchar>,
        reviewed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    marketplace_commission_rules (id) {
        id -> Uuid,
        scope -> Varchar,
        scope_value -> Varchar,
        rate_percent -> Numeric,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    marketplace_product_approvals (id) {
        id -> Uuid,
        product_id -> Varchar,
        vendor_id -> Uuid,
        category -> Nullable<Varchar>,
        status -> Varchar,
        reason -> Text,
        reviewed_by -> Nullable<Varchar>,
        reviewed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    marketplace_commissions (id) {
        id -> Uuid,
        order_id -> Varchar,
        vendor_id -> Uuid,
        product_id -> Varchar,
        order_total -> Numeric,
        rate_percent -> Numeric,
        commission_amount -> Numeric,
        currency -> Varchar,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    marketplace_payouts (id) {
        id -> Uuid,
        vendor_id -> Uuid,
        amount -> Numeric,
        currency -> Varchar,
        status -> Varchar,
        invoice_id -> Nullable<Varchar>,
        reference -> Varchar,
        requested_by -> Varchar,
        reviewed_by -> Nullable<Varchar>,
        review_reason -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(pricing_tiers -> pricing_tables (table_id));
diesel::joinable!(pricing_tier_features -> pricing_tiers (tier_id));
diesel::joinable!(marketplace_product_approvals -> marketplace_vendors (vendor_id));
diesel::joinable!(marketplace_commissions -> marketplace_vendors (vendor_id));
diesel::joinable!(marketplace_payouts -> marketplace_vendors (vendor_id));

// ---------------------------------------------------------------------------
// Sharing / auditors (migration 000068_sharing_auditor; batch 5 notes §c).
// ---------------------------------------------------------------------------

diesel::table! {
    public_links (id) {
        id -> Uuid,
        token_hash -> Varchar,
        scope_kind -> Varchar,
        scope_id -> Uuid,
        scope_view -> Varchar,
        created_by -> Varchar,
        expires_at -> Timestamptz,
        revoked_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    public_link_access_log (id) {
        id -> Uuid,
        link_id -> Nullable<Uuid>,
        accessed_at -> Timestamptz,
        ip_hash -> Nullable<Varchar>,
        user_agent_hash -> Nullable<Varchar>,
        allowed -> Bool,
        reason -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    auditor_accounts (id) {
        id -> Uuid,
        user_id -> Varchar,
        label -> Varchar,
        email -> Varchar,
        is_auditor -> Bool,
        auditor_of -> Varchar,
        status -> Varchar,
        expires_at -> Timestamptz,
        password_hash -> Varchar,
        revoked_at -> Nullable<Timestamptz>,
        revoked_by -> Nullable<Varchar>,
        revoke_reason -> Nullable<Text>,
        scheduler_status -> Varchar,
        scheduler_job_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    auditor_scopes (id) {
        id -> Uuid,
        auditor_id -> Uuid,
        module -> Varchar,
        resource_kind -> Varchar,
        resource_id -> Nullable<Uuid>,
        company_id -> Nullable<Uuid>,
        allow_export -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    auditor_sessions (id) {
        id -> Uuid,
        auditor_id -> Uuid,
        user_id -> Varchar,
        started_at -> Timestamptz,
        last_seen_at -> Timestamptz,
        revoked_at -> Nullable<Timestamptz>,
        revoked_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Properties (migration 000061_properties; batch 5 notes §c).
// ---------------------------------------------------------------------------

diesel::table! {
    properties (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Nullable<Text>,
        slug -> Varchar,
        latitude -> Nullable<Float8>,
        longitude -> Nullable<Float8>,
        address -> Nullable<Varchar>,
        total_area_sqm -> Nullable<Numeric>,
        company_id -> Nullable<Uuid>,
        status -> Varchar,
        cover_storage_target -> Nullable<Varchar>,
        cover_storage_key -> Nullable<Varchar>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    property_plans (id) {
        id -> Uuid,
        property_id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        geometry -> Nullable<Jsonb>,
        geometry_hash -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    property_blocks (id) {
        id -> Uuid,
        plan_id -> Uuid,
        property_id -> Uuid,
        title -> Varchar,
        notes -> Nullable<Text>,
        geometry -> Jsonb,
        geometry_hash -> Varchar,
        area_sqm -> Nullable<Numeric>,
        length_m -> Nullable<Numeric>,
        price -> Numeric,
        currency -> Varchar,
        status -> Varchar,
        assigned_customer_id -> Nullable<Text>,
        click_behavior -> Varchar,
        click_link_kind -> Nullable<Varchar>,
        click_link_target -> Nullable<Text>,
        order_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    property_block_history (id) {
        id -> Uuid,
        block_id -> Uuid,
        property_id -> Uuid,
        plan_id -> Uuid,
        from_status -> Varchar,
        to_status -> Varchar,
        actor_id -> Text,
        reason -> Nullable<Text>,
        order_id -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}

diesel::joinable!(property_plans -> properties (property_id));
diesel::joinable!(property_blocks -> property_plans (plan_id));
diesel::joinable!(property_blocks -> properties (property_id));
diesel::joinable!(property_block_history -> property_blocks (block_id));
diesel::joinable!(property_block_history -> properties (property_id));
diesel::joinable!(property_block_history -> property_plans (plan_id));

// ---------------------------------------------------------------------------
// Logistics (migration 000062_logistics; batch 5 notes §c). The notes
// write `Double`; normalised to the file's `Float8` style (same type).
// ---------------------------------------------------------------------------

diesel::table! {
    logistics_drivers (id) {
        id -> Uuid,
        user_id -> Text,
        license_number -> Varchar,
        phone -> Nullable<Varchar>,
        status -> Varchar,
        current_vehicle_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_vehicles (id) {
        id -> Uuid,
        plate -> Varchar,
        vehicle_type -> Varchar,
        capacity_kg -> Float8,
        status -> Varchar,
        assigned_driver_id -> Nullable<Uuid>,
        company_id -> Nullable<Uuid>,
        maintenance -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_warehouses (id) {
        id -> Uuid,
        name -> Varchar,
        address -> Varchar,
        capacity -> Int8,
        manager_user_id -> Nullable<Text>,
        default_low_stock_threshold -> Int8,
        company_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_shipments (id) {
        id -> Uuid,
        tracking_number -> Varchar,
        tracking_token_hash -> Varchar,
        order_id -> Nullable<Varchar>,
        order_link_status -> Varchar,
        company_id -> Nullable<Uuid>,
        status -> Varchar,
        origin_address -> Varchar,
        origin_latitude -> Nullable<Float8>,
        origin_longitude -> Nullable<Float8>,
        destination_address -> Varchar,
        destination_latitude -> Nullable<Float8>,
        destination_longitude -> Nullable<Float8>,
        weight_kg -> Float8,
        length_cm -> Float8,
        width_cm -> Float8,
        height_cm -> Float8,
        zone_origin -> Varchar,
        zone_destination -> Varchar,
        service_level -> Varchar,
        driver_id -> Nullable<Uuid>,
        vehicle_id -> Nullable<Uuid>,
        warehouse_id -> Nullable<Uuid>,
        scheduled_at -> Nullable<Timestamptz>,
        window_start -> Nullable<Timestamptz>,
        window_end -> Nullable<Timestamptz>,
        estimated_delivery_at -> Nullable<Timestamptz>,
        shipping_cost -> Nullable<Numeric>,
        currency -> Varchar,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_shipment_events (id) {
        id -> Uuid,
        shipment_id -> Uuid,
        kind -> Varchar,
        from_status -> Nullable<Varchar>,
        to_status -> Nullable<Varchar>,
        actor -> Text,
        location -> Nullable<Varchar>,
        note -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_stock_movements (id) {
        id -> Uuid,
        warehouse_id -> Uuid,
        asset_id -> Uuid,
        direction -> Varchar,
        quantity -> Int8,
        quantity_after -> Int8,
        reason -> Varchar,
        shipment_id -> Nullable<Uuid>,
        actor -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_routes (id) {
        id -> Uuid,
        name -> Varchar,
        driver_id -> Nullable<Uuid>,
        vehicle_id -> Nullable<Uuid>,
        route_date -> Timestamptz,
        start_latitude -> Nullable<Float8>,
        start_longitude -> Nullable<Float8>,
        status -> Varchar,
        company_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_route_stops (id) {
        id -> Uuid,
        route_id -> Uuid,
        shipment_id -> Nullable<Uuid>,
        sequence -> Int8,
        address -> Varchar,
        latitude -> Nullable<Float8>,
        longitude -> Nullable<Float8>,
        status -> Varchar,
        note -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_proofs (id) {
        id -> Uuid,
        shipment_id -> Uuid,
        storage_target -> Varchar,
        storage_key -> Varchar,
        content_sha256 -> Varchar,
        signature_storage_target -> Nullable<Varchar>,
        signature_storage_key -> Nullable<Varchar>,
        recipient_name -> Varchar,
        delivered_at -> Timestamptz,
        actor -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_exceptions (id) {
        id -> Uuid,
        shipment_id -> Uuid,
        exception_type -> Varchar,
        description -> Text,
        status -> Varchar,
        previous_status -> Varchar,
        resolution -> Nullable<Text>,
        actor -> Text,
        resolved_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_rate_tables (id) {
        id -> Uuid,
        name -> Varchar,
        origin_zone -> Nullable<Varchar>,
        destination_zone -> Nullable<Varchar>,
        base_price -> Numeric,
        per_kg -> Numeric,
        per_km -> Numeric,
        currency -> Varchar,
        active -> Bool,
        company_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(logistics_shipments -> logistics_drivers (driver_id));
diesel::joinable!(logistics_shipments -> logistics_vehicles (vehicle_id));
diesel::joinable!(logistics_shipments -> logistics_warehouses (warehouse_id));
diesel::joinable!(logistics_shipment_events -> logistics_shipments (shipment_id));
diesel::joinable!(logistics_stock_movements -> logistics_warehouses (warehouse_id));
diesel::joinable!(logistics_stock_movements -> logistics_shipments (shipment_id));
diesel::joinable!(logistics_routes -> logistics_drivers (driver_id));
diesel::joinable!(logistics_routes -> logistics_vehicles (vehicle_id));
diesel::joinable!(logistics_route_stops -> logistics_routes (route_id));
diesel::joinable!(logistics_route_stops -> logistics_shipments (shipment_id));
diesel::joinable!(logistics_proofs -> logistics_shipments (shipment_id));
diesel::joinable!(logistics_exceptions -> logistics_shipments (shipment_id));

// ---------------------------------------------------------------------------
// Supervision (migration 000063_supervision; batch 5 notes §c).
// ---------------------------------------------------------------------------

diesel::table! {
    supervision_assignments (id) {
        id -> Uuid,
        project_id -> Text,
        company_id -> Nullable<Uuid>,
        user_id -> Text,
        role -> Text,
        status -> Text,
        starts_at -> Timestamptz,
        ends_at -> Nullable<Timestamptz>,
        notes -> Nullable<Text>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_inspection_templates (id) {
        id -> Uuid,
        project_id -> Nullable<Text>,
        company_id -> Nullable<Uuid>,
        title -> Text,
        description -> Nullable<Text>,
        category -> Text,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_template_items (id) {
        id -> Uuid,
        template_id -> Uuid,
        title -> Text,
        description -> Nullable<Text>,
        position -> Int4,
        required -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_inspections (id) {
        id -> Uuid,
        project_id -> Text,
        company_id -> Nullable<Uuid>,
        template_id -> Nullable<Uuid>,
        title -> Text,
        location -> Nullable<Text>,
        inspector_id -> Text,
        status -> Text,
        scheduled_for -> Timestamptz,
        conducted_at -> Nullable<Timestamptz>,
        verdict -> Nullable<Text>,
        summary -> Nullable<Text>,
        signed_by -> Nullable<Text>,
        signed_at -> Nullable<Timestamptz>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_inspection_items (id) {
        id -> Uuid,
        inspection_id -> Uuid,
        template_item_id -> Nullable<Uuid>,
        title -> Text,
        result -> Text,
        notes -> Nullable<Text>,
        follow_up_requested -> Bool,
        follow_up_task_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_progress_reports (id) {
        id -> Uuid,
        project_id -> Text,
        company_id -> Nullable<Uuid>,
        milestone_id -> Nullable<Text>,
        title -> Text,
        period_start -> Date,
        period_end -> Date,
        reported_percent_complete -> Numeric,
        planned_percent_complete -> Nullable<Numeric>,
        project_percent_complete -> Nullable<Numeric>,
        source -> Text,
        status -> Text,
        notes -> Nullable<Text>,
        reported_by -> Text,
        reviewed_by -> Nullable<Text>,
        reviewed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_defects (id) {
        id -> Uuid,
        project_id -> Text,
        company_id -> Nullable<Uuid>,
        inspection_id -> Nullable<Uuid>,
        task_id -> Nullable<Text>,
        title -> Text,
        description -> Nullable<Text>,
        location -> Nullable<Text>,
        severity -> Text,
        status -> Text,
        reported_by -> Text,
        assigned_to -> Nullable<Text>,
        follow_up_requested -> Bool,
        follow_up_task_id -> Nullable<Text>,
        resolved_at -> Nullable<Timestamptz>,
        verified_by -> Nullable<Text>,
        verified_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_approvals (id) {
        id -> Uuid,
        target_kind -> Text,
        target_id -> Uuid,
        project_id -> Text,
        company_id -> Nullable<Uuid>,
        approver_id -> Text,
        decision -> Text,
        comment -> Nullable<Text>,
        decided_at -> Timestamptz,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_daily_logs (id) {
        id -> Uuid,
        project_id -> Text,
        company_id -> Nullable<Uuid>,
        log_date -> Date,
        supervisor_id -> Text,
        weather -> Nullable<Text>,
        workforce_count -> Int8,
        work_done -> Nullable<Text>,
        materials_received -> Nullable<Text>,
        incidents_on_site -> Nullable<Text>,
        signed_by -> Nullable<Text>,
        signed_at -> Nullable<Timestamptz>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_photos (id) {
        id -> Uuid,
        project_id -> Text,
        company_id -> Nullable<Uuid>,
        subject_kind -> Text,
        subject_id -> Uuid,
        storage_target -> Text,
        storage_key -> Text,
        content_sha256 -> Text,
        content_type -> Text,
        size_bytes -> Int8,
        caption -> Nullable<Text>,
        uploaded_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(supervision_template_items -> supervision_inspection_templates (template_id));
diesel::joinable!(supervision_inspections -> supervision_inspection_templates (template_id));
diesel::joinable!(supervision_inspection_items -> supervision_inspections (inspection_id));
diesel::joinable!(supervision_defects -> supervision_inspections (inspection_id));

// ---------------------------------------------------------------------------
// AI gating (migration 000064_ai_gating; batch 5 notes §c).
// ---------------------------------------------------------------------------

diesel::table! {
    ai_global_settings (key) {
        key -> Varchar,
        enabled -> Bool,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Network Tools (migration 000070_network_tools) + Licensing SaaS
// (migration 000069_licensing_saas); batch 7 notes §2. The notes
// write `BigInt`; normalised to the file's `Int8` style.
// ---------------------------------------------------------------------------

diesel::table! {
    network_tool_saved_checks (id) {
        id -> Uuid,
        user_id -> Text,
        company_id -> Nullable<Uuid>,
        name -> Text,
        tool -> Text,
        target -> Text,
        parameters -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    network_tool_runs (id) {
        id -> Uuid,
        user_id -> Text,
        company_id -> Nullable<Uuid>,
        tool -> Text,
        target -> Text,
        duration_ms -> Int8,
        outcome -> Text,
        summary -> Jsonb,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    network_tool_uptime_monitors (id) {
        id -> Uuid,
        user_id -> Text,
        company_id -> Nullable<Uuid>,
        name -> Text,
        url -> Text,
        interval_seconds -> Int4,
        expected_status -> Int4,
        failure_threshold -> Int4,
        consecutive_failures -> Int4,
        enabled -> Bool,
        last_status -> Nullable<Text>,
        last_checked_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    network_tool_uptime_events (id) {
        id -> Uuid,
        monitor_id -> Uuid,
        status -> Text,
        http_status -> Nullable<Int4>,
        latency_ms -> Nullable<Int8>,
        detail -> Jsonb,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    network_tool_cert_watches (id) {
        id -> Uuid,
        user_id -> Text,
        company_id -> Nullable<Uuid>,
        domain -> Text,
        enabled -> Bool,
        last_days_remaining -> Nullable<Int4>,
        last_checked_at -> Nullable<Timestamptz>,
        alerted_thresholds -> Array<Int4>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    license_products (id) {
        id -> Uuid,
        product -> Varchar,
        display_name -> Nullable<Varchar>,
        owner_id -> Nullable<Uuid>,
        key_prefix -> Nullable<Varchar>,
        segments -> Int4,
        key_format -> Varchar,
        key_pattern -> Nullable<Varchar>,
        default_max_activations -> Int4,
        default_validity_days -> Nullable<Int8>,
        grace_period_days -> Int8,
        renewal_reminder_days -> Array<Int8>,
        offline_max_days -> Int8,
        allowed_distribution_channels -> Array<Text>,
        api_allowed_endpoints -> Array<Text>,
        api_rate_limit_per_minute -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    license_distributions (id) {
        id -> Uuid,
        product -> Varchar,
        provider -> Varchar,
        repo -> Varchar,
        git_ref -> Varchar,
        webhook_secret_hash -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    license_releases (id) {
        id -> Uuid,
        product_id -> Uuid,
        product -> Varchar,
        version -> Varchar,
        file_name -> Varchar,
        storage_target -> Nullable<Varchar>,
        storage_key -> Varchar,
        sha256_hex -> Varchar,
        size_bytes -> Int8,
        download_count -> Int8,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    license_events (id) {
        id -> Uuid,
        license_id -> Uuid,
        event -> Varchar,
        actor -> Nullable<Varchar>,
        details -> Jsonb,
        prev_hash -> Varchar,
        hash -> Varchar,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    license_offline_tokens (token_id) {
        token_id -> Uuid,
        license_id -> Uuid,
        product -> Varchar,
        expires_at -> Timestamptz,
        issued_at -> Timestamptz,
        revoked -> Bool,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    license_activation_history (id) {
        id -> Uuid,
        license_id -> Uuid,
        action -> Varchar,
        actor -> Nullable<Varchar>,
        instance_id -> Varchar,
        hostname -> Nullable<Varchar>,
        created_at -> Timestamptz,
    }
}

diesel::joinable!(network_tool_uptime_events -> network_tool_uptime_monitors (monitor_id));
diesel::joinable!(license_products -> users (owner_id));
diesel::joinable!(license_releases -> license_products (product_id));
diesel::joinable!(license_events -> licenses (license_id));
diesel::joinable!(license_offline_tokens -> licenses (license_id));
diesel::joinable!(license_activation_history -> licenses (license_id));

// ---------------------------------------------------------------------------
// SaaS domains (migration 000072_saas_domains), domain verification
// (migration 000074_domain_verification), domain cert modes +
// Cloudflare (migration 000075_domain_cert_modes); batch 8 notes §3.
// The notes write bare `table!` and `BigInt`/`Integer`; normalised
// to the file's `diesel::table!` / `Int8` / `Int4` style.
// ---------------------------------------------------------------------------

diesel::table! {
    saas_domain_reservations (id) {
        id -> Uuid,
        domain -> Varchar,
        company_id -> Uuid,
        reserved_by -> Text,
        reserved_until -> Timestamptz,
        status -> Varchar,
        site_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    saas_domain_aliases (id) {
        id -> Uuid,
        domain -> Varchar,
        company_id -> Uuid,
        target_type -> Varchar,
        target_value -> Varchar,
        status -> Varchar,
        site_id -> Nullable<Uuid>,
        verified_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    saas_subdomain_allocations (id) {
        id -> Uuid,
        label -> Varchar,
        hostname -> Varchar,
        company_id -> Uuid,
        site_id -> Nullable<Uuid>,
        status -> Varchar,
        approved_by -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    domain_verification_settings (id) {
        id -> Uuid,
        version -> Int8,
        record_type -> Text,
        host_template -> Text,
        value_template -> Text,
        token_length -> Int4,
        token_alphabet -> Text,
        verify_timeout_seconds -> Int4,
        max_attempts -> Int4,
        retry_delay_seconds -> Int4,
        token_ttl_seconds -> Int8,
        rate_limit_window_seconds -> Int8,
        updated_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    domain_verification_tokens (id) {
        id -> Uuid,
        domain -> Text,
        alias_id -> Nullable<Uuid>,
        tenant_id -> Nullable<Uuid>,
        company_id -> Nullable<Uuid>,
        token_hash -> Text,
        record_type -> Text,
        host -> Text,
        expected_value -> Text,
        settings_version -> Int8,
        status -> Text,
        issued_at -> Timestamptz,
        expires_at -> Timestamptz,
        verified_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    domain_verification_attempts (id) {
        id -> Uuid,
        token_id -> Uuid,
        domain -> Text,
        alias_id -> Nullable<Uuid>,
        attempt_number -> Int4,
        result -> Text,
        record_found -> Nullable<Text>,
        looked_up -> Jsonb,
        error -> Nullable<Text>,
        checked_at -> Timestamptz,
    }
}

diesel::table! {
    domain_cert_modes (id) {
        id -> Uuid,
        domain -> Varchar,
        mode -> Varchar,
        provider -> Varchar,
        fingerprint_sha256 -> Varchar,
        installed_at -> Nullable<Timestamptz>,
        previous_fingerprint -> Nullable<Varchar>,
        status -> Varchar,
        last_error -> Nullable<Text>,
        updated_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    cloudflare_detections (id) {
        id -> Uuid,
        domain -> Varchar,
        detected -> Bool,
        methods -> Jsonb,
        ssl_mode -> Nullable<Varchar>,
        end_to_end_encrypted -> Nullable<Bool>,
        flexible_warning -> Nullable<Text>,
        checked_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    cloudflare_ip_ranges (key) {
        key -> Varchar,
        v4 -> Jsonb,
        v6 -> Jsonb,
        fetched_at -> Nullable<Timestamptz>,
        source_v4 -> Varchar,
        source_v6 -> Varchar,
        fetch_error -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Whitelabel (migration 000073_whitelabel) + presets
// (migration 000071_presets); batch 9 notes §4. The notes write bare
// `table!`; normalised to the file's `diesel::table!` style.
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Entitlements (migration 000098_entitlements; tenant-schema tables);
// batch 15 B1 notes §4. No `joinable!` entries (no FKs, per notes).
// ---------------------------------------------------------------------------

diesel::table! {
    entitlements (id) {
        id -> Uuid,
        tenant_id -> Varchar,
        company_id -> Varchar,
        source_kind -> Varchar,
        source_id -> Varchar,
        source_ref -> Nullable<Varchar>,
        module_id -> Varchar,
        status -> Varchar,
        granted_at -> Timestamptz,
        expires_at -> Nullable<Timestamptz>,
        granted_by -> Nullable<Varchar>,
        reason -> Nullable<Varchar>,
        grace_started_at -> Nullable<Timestamptz>,
        grace_ends_at -> Nullable<Timestamptz>,
        lapsed_at -> Nullable<Timestamptz>,
        lapse_reason -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    entitlement_history (id) {
        id -> Uuid,
        tenant_id -> Varchar,
        entitlement_id -> Nullable<Uuid>,
        company_id -> Varchar,
        module_id -> Varchar,
        action -> Varchar,
        actor_id -> Nullable<Varchar>,
        detail -> Jsonb,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    entitlement_denied_attempts (id) {
        id -> Uuid,
        tenant_id -> Varchar,
        company_id -> Varchar,
        module_id -> Varchar,
        actor_id -> Nullable<Varchar>,
        reason -> Varchar,
        error_code -> Varchar,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    entitlement_package_changes (id) {
        id -> Uuid,
        tenant_id -> Varchar,
        company_id -> Varchar,
        source_kind -> Varchar,
        source_id -> Varchar,
        new_module_ids -> Jsonb,
        newly_entitled -> Jsonb,
        kept -> Jsonb,
        kept_via_other_source -> Jsonb,
        requires_confirmation -> Jsonb,
        disabled_module_ids -> Jsonb,
        status -> Varchar,
        requested_by -> Nullable<Varchar>,
        decided_by -> Nullable<Varchar>,
        decided_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    entitlement_settings (id) {
        id -> Varchar,
        grace_period_days -> Int8,
        read_only_grace_enabled -> Bool,
        default_deny -> Bool,
        updated_by -> Nullable<Varchar>,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    entitlement_module_activations (company_id, module_id) {
        tenant_id -> Varchar,
        company_id -> Varchar,
        module_id -> Varchar,
        enabled -> Bool,
        enabled_at -> Timestamptz,
        enabled_by -> Nullable<Varchar>,
        source_kind -> Nullable<Varchar>,
        source_id -> Nullable<Varchar>,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// SaaS product subdomains (batch 15 B2 notes §2).
// ---------------------------------------------------------------------------

diesel::table! {
    saas_product_subdomains (id) {
        id -> Uuid,
        company_id -> Uuid,
        product_id -> Varchar,
        package_id -> Nullable<Varchar>,
        label -> Varchar,
        host -> Varchar,
        custom_domain -> Nullable<Varchar>,
        site_id -> Nullable<Uuid>,
        status -> Varchar,
        claimed_by -> Text,
        claimed_at -> Timestamptz,
        verified_at -> Nullable<Timestamptz>,
        released_at -> Nullable<Timestamptz>,
        grace_until -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    saas_product_subdomain_events (id) {
        id -> Uuid,
        product_subdomain_id -> Uuid,
        event -> Varchar,
        actor -> Text,
        reason -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Pricing engine (migration 000103_pricing); batch 15 B3 notes §6.2.
// `pricing_tiers` is NOT repeated here: the 000041 shape (pricing-table
// feature tiers) is already in this file, and the 000103 shape (volume
// quantity tiers) collides with it — see the header note.
// ---------------------------------------------------------------------------

diesel::table! {
    pricing_settings (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        rounding_mode -> Varchar,
        proration_basis -> Varchar,
        days_per_month -> Int8,
        approval_threshold_bp -> Int8,
        require_second_approver -> Bool,
        default_currency -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_currencies (code) {
        code -> Varchar,
        exponent -> Int8,
        active -> Bool,
        rate_numerator -> Nullable<Int8>,
        rate_denominator -> Nullable<Int8>,
        rate_fetched_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_rules (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        name -> Varchar,
        kind -> Varchar,
        percent_bp -> Int8,
        amount_scaled -> Numeric,
        currency -> Varchar,
        buy_quantity -> Int8,
        get_quantity -> Int8,
        min_quantity -> Int8,
        min_subtotal_scaled -> Numeric,
        product_ids -> Jsonb,
        evaluation_order -> Int8,
        stackable -> Bool,
        exclusive -> Bool,
        status -> Varchar,
        valid_from -> Nullable<Timestamptz>,
        valid_to -> Nullable<Timestamptz>,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_coupons (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        code -> Varchar,
        kind -> Varchar,
        percent_bp -> Int8,
        amount_scaled -> Numeric,
        currency -> Varchar,
        buy_quantity -> Int8,
        get_quantity -> Int8,
        product_ids -> Jsonb,
        valid_from -> Nullable<Timestamptz>,
        valid_to -> Nullable<Timestamptz>,
        usage_limit -> Nullable<Int8>,
        per_customer_limit -> Nullable<Int8>,
        used_count -> Int8,
        stackable -> Bool,
        exclusive -> Bool,
        active -> Bool,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_coupon_redemptions (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        coupon_id -> Uuid,
        customer_id -> Varchar,
        count -> Int8,
        last_redeemed_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_recommendations (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        product_id -> Varchar,
        kind -> Varchar,
        suggested_price_scaled -> Numeric,
        currency -> Varchar,
        lines -> Jsonb,
        inputs -> Jsonb,
        status -> Varchar,
        created_by -> Varchar,
        confirmed_by -> Nullable<Varchar>,
        price_version_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_cost_lines (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        recommendation_id -> Uuid,
        label -> Varchar,
        source -> Varchar,
        cost_scaled -> Numeric,
        currency -> Varchar,
    }
}

diesel::table! {
    pricing_bundles (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        name -> Varchar,
        product_id -> Varchar,
        discount_kind -> Varchar,
        discount_percent_bp -> Int8,
        discount_amount_scaled -> Numeric,
        currency -> Varchar,
        active -> Bool,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_bundle_components (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        bundle_id -> Uuid,
        product_id -> Varchar,
        label -> Varchar,
        price_scaled -> Numeric,
        currency -> Varchar,
    }
}

diesel::table! {
    pricing_reference_prices (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        product_id -> Varchar,
        source -> Varchar,
        source_url -> Nullable<Varchar>,
        price_scaled -> Numeric,
        currency -> Varchar,
        observed_at -> Timestamptz,
        note -> Text,
        entered_by -> Varchar,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_price_versions (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        product_id -> Varchar,
        version -> Int8,
        price_scaled -> Numeric,
        currency -> Varchar,
        previous_price_scaled -> Numeric,
        change_bp -> Nullable<Int8>,
        above_threshold -> Bool,
        status -> Varchar,
        requested_by -> Varchar,
        approved_by -> Nullable<Varchar>,
        reason -> Nullable<Text>,
        recommendation_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_price_approvals (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        price_version_id -> Uuid,
        approver -> Varchar,
        decision -> Varchar,
        reason -> Nullable<Text>,
        decided_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_proration_records (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        subscription_id -> Nullable<Varchar>,
        customer_id -> Nullable<Varchar>,
        kind -> Varchar,
        basis -> Varchar,
        period_start -> Timestamptz,
        period_end -> Timestamptz,
        change_at -> Timestamptz,
        period_days -> Int8,
        days_used -> Int8,
        days_remaining -> Int8,
        old_daily_rate_scaled -> Numeric,
        new_daily_rate_scaled -> Numeric,
        unused_credit_scaled -> Numeric,
        new_charge_scaled -> Numeric,
        net_due_scaled -> Numeric,
        currency -> Varchar,
        lines -> Jsonb,
        created_by -> Varchar,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_credit_notes (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        number -> Varchar,
        invoice_id -> Varchar,
        amount_scaled -> Numeric,
        currency -> Varchar,
        lines -> Jsonb,
        reason -> Text,
        status -> Varchar,
        reconciled -> Bool,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_reconciliation_reports (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        invoice_id -> Nullable<Varchar>,
        matched -> Bool,
        currency -> Varchar,
        components -> Jsonb,
        total_discrepancy_scaled -> Numeric,
        checked_by -> Varchar,
        checked_at -> Timestamptz,
    }
}

diesel::joinable!(pricing_coupon_redemptions -> pricing_coupons (coupon_id));
diesel::joinable!(pricing_cost_lines -> pricing_recommendations (recommendation_id));
diesel::joinable!(pricing_bundle_components -> pricing_bundles (bundle_id));
diesel::joinable!(pricing_price_approvals -> pricing_price_versions (price_version_id));

// ---------------------------------------------------------------------------
// Storage product (migration 000057_storage_product_byo);
// storage product notes §3. No `joinable!` entries (no FKs, per notes).
// ---------------------------------------------------------------------------

diesel::table! {
    storage_plans (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Varchar,
        quota_bytes -> Int8,
        max_files -> Int8,
        included_provider_kinds -> Array<Text>,
        product_id -> Nullable<Varchar>,
        billing_cycle -> Nullable<Varchar>,
        is_public -> Bool,
        sort_order -> Int8,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_plan_assignments (id) {
        id -> Uuid,
        plan_id -> Uuid,
        company_id -> Nullable<Varchar>,
        user_id -> Varchar,
        status -> Varchar,
        assigned_by -> Varchar,
        assigned_at -> Timestamptz,
        expires_at -> Nullable<Timestamptz>,
        cancelled_at -> Nullable<Timestamptz>,
        cancel_reason -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_usage_metering (id) {
        id -> Uuid,
        company_id -> Nullable<Varchar>,
        user_id -> Varchar,
        plan_id -> Nullable<Uuid>,
        period -> Varchar,
        bytes_stored -> Int8,
        files_stored -> Int8,
        reported_by -> Varchar,
        reported_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_byo_connections (id) {
        id -> Uuid,
        name -> Varchar,
        provider_kind -> Varchar,
        s3_compatible -> Bool,
        endpoint -> Varchar,
        region -> Varchar,
        container -> Varchar,
        path -> Varchar,
        sftp_host -> Varchar,
        sftp_port -> Int4,
        sftp_username -> Varchar,
        sftp_auth_kind -> Varchar,
        sftp_keyfile -> Varchar,
        allowed_hosts -> Array<Text>,
        credentials_encrypted -> Nullable<Text>,
        key_fingerprint -> Nullable<Varchar>,
        path_prefix -> Varchar,
        owner_user -> Varchar,
        company_id -> Nullable<Varchar>,
        status -> Varchar,
        status_reason -> Nullable<Varchar>,
        allow_read -> Bool,
        allow_write -> Bool,
        allow_delete -> Bool,
        max_bytes -> Nullable<Int8>,
        max_files -> Nullable<Int8>,
        used_bytes -> Int8,
        used_files -> Int8,
        encryption_required -> Bool,
        allowed_extensions -> Array<Text>,
        allowed_mime_types -> Array<Text>,
        terms_version -> Nullable<Varchar>,
        terms_accepted_at -> Nullable<Timestamptz>,
        last_validated_at -> Nullable<Timestamptz>,
        last_validation_ok -> Nullable<Bool>,
        last_validation_error -> Nullable<Varchar>,
        last_health_at -> Nullable<Timestamptz>,
        last_health_ok -> Nullable<Bool>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_byo_terms (id) {
        id -> Uuid,
        version -> Varchar,
        title -> Varchar,
        body -> Text,
        is_current -> Bool,
        reacceptance_deadline -> Nullable<Timestamptz>,
        enabled -> Bool,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_byo_terms_acceptances (id) {
        id -> Uuid,
        terms_id -> Uuid,
        terms_version -> Varchar,
        user_id -> Varchar,
        company_id -> Nullable<Varchar>,
        connection_id -> Nullable<Uuid>,
        accepted_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_byo_objects (connection_id, key) {
        connection_id -> Uuid,
        key -> Varchar,
        size_bytes -> Int8,
        mime -> Varchar,
        owner_user -> Varchar,
        company_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_byo_health_checks (id) {
        id -> Uuid,
        connection_id -> Uuid,
        ok -> Bool,
        latency_ms -> Int8,
        detail -> Varchar,
        checked_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Card validation (migration 000105_cards) + reference-id settings
// (batch 18 notes §3b/§3e; the §3e `reference` columns for
// `store_orders` / `billing_invoices` are NOT added here — no
// migration creates them; see the header note).
// ---------------------------------------------------------------------------

diesel::table! {
    card_custom_rules (id) {
        id -> Uuid,
        tenant_id -> Text,
        name -> Text,
        brand_name -> Text,
        iin_prefixes -> Jsonb,
        iin_ranges -> Jsonb,
        allowed_lengths -> Jsonb,
        cvv_length -> Int2,
        luhn_required -> Bool,
        enabled -> Bool,
        overrides_standard_brand -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    card_validation_daily_counts (tenant_id, date, matched_rule, result) {
        tenant_id -> Text,
        date -> Date,
        matched_rule -> Text,
        result -> Text,
        count -> Int8,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    reference_id_settings (tenant_id) {
        tenant_id -> Uuid,
        default_prefix -> Varchar,
        product_prefixes -> Jsonb,
        max_total_length -> Int4,
    }
}

// ---------------------------------------------------------------------------
// Tenants (migration 000100_tenant_isolation); snippets from the
// header doc comment of `src/modules/tenants.rs`, verified against
// the migration. `tenant_module_grants` has no created_at/updated_at.
// ---------------------------------------------------------------------------

diesel::table! {
    tenants (id) {
        id -> Uuid,
        slug -> Varchar,
        display_name -> Varchar,
        status -> Varchar,
        owner_user_id -> Uuid,
        package_id -> Nullable<Uuid>,
        preset_id -> Nullable<Uuid>,
        plan_id -> Nullable<Varchar>,
        subdomain -> Nullable<Varchar>,
        subdomain_pending -> Bool,
        schema_name -> Varchar,
        suspended_at -> Nullable<Timestamptz>,
        suspended_reason -> Nullable<Text>,
        destroyed_at -> Nullable<Timestamptz>,
        export_reference -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    tenant_provisioning_steps (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        step -> Varchar,
        status -> Varchar,
        attempt -> Int4,
        error -> Nullable<Text>,
        detail -> Jsonb,
        started_at -> Nullable<Timestamptz>,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    tenant_module_grants (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        module_id -> Varchar,
        granted_via -> Varchar,
        granted_via_id -> Nullable<Uuid>,
        granted_at -> Timestamptz,
        expires_at -> Nullable<Timestamptz>,
        granted_by -> Nullable<Varchar>,
        revoked_at -> Nullable<Timestamptz>,
        revoke_reason -> Nullable<Text>,
    }
}

// ---------------------------------------------------------------------------
// Business Intelligence (migration 000042_bi).
// ---------------------------------------------------------------------------

diesel::table! {
    bi_reports (id) {
        id -> Uuid,
        company_id -> Varchar,
        owner_id -> Varchar,
        name -> Varchar,
        description -> Nullable<Text>,
        source -> Varchar,
        fields -> Jsonb,
        filters -> Jsonb,
        group_by -> Nullable<Varchar>,
        chart_type -> Varchar,
        visibility -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    bi_report_schedules (id) {
        id -> Uuid,
        report_id -> Uuid,
        company_id -> Varchar,
        schedule -> Varchar,
        recipients -> Jsonb,
        channel -> Varchar,
        scheduler_status -> Varchar,
        scheduler_job_id -> Nullable<Varchar>,
        last_delivered_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    bi_kpi_dashboards (id) {
        id -> Uuid,
        company_id -> Varchar,
        owner_id -> Varchar,
        name -> Varchar,
        description -> Nullable<Text>,
        kpis -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(bi_report_schedules -> bi_reports (report_id));

// ---------------------------------------------------------------------------
// OKRs (migration 000043_okrs). `OkrObjective`, `OkrKeyResult` and
// `OkrCheckin` carry NUMERIC columns, so per the repo header rule
// they skip ALL serde derives. The self-reference
// `okr_objectives.parent_objective_id` gets no `joinable!` (Diesel
// does not allow a table to join to itself in `allow_tables` form).
// ---------------------------------------------------------------------------

diesel::table! {
    okr_cycles (id) {
        id -> Uuid,
        company_id -> Varchar,
        name -> Varchar,
        description -> Nullable<Text>,
        starts_at -> Timestamptz,
        ends_at -> Timestamptz,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    okr_objectives (id) {
        id -> Uuid,
        cycle_id -> Uuid,
        company_id -> Varchar,
        parent_objective_id -> Nullable<Uuid>,
        title -> Varchar,
        description -> Nullable<Text>,
        level -> Varchar,
        owner_id -> Varchar,
        final_score -> Nullable<Numeric>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    okr_key_results (id) {
        id -> Uuid,
        objective_id -> Uuid,
        title -> Varchar,
        unit -> Varchar,
        target_value -> Numeric,
        start_value -> Numeric,
        current_value -> Numeric,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    okr_checkins (id) {
        id -> Uuid,
        key_result_id -> Uuid,
        author_id -> Varchar,
        note -> Text,
        confidence -> Varchar,
        kind -> Varchar,
        reason -> Nullable<Text>,
        value_snapshot -> Nullable<Numeric>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    okr_links (id) {
        id -> Uuid,
        key_result_id -> Uuid,
        link_type -> Varchar,
        target_id -> Uuid,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(okr_objectives -> okr_cycles (cycle_id));
diesel::joinable!(okr_key_results -> okr_objectives (objective_id));
diesel::joinable!(okr_checkins -> okr_key_results (key_result_id));
diesel::joinable!(okr_links -> okr_key_results (key_result_id));

// ---------------------------------------------------------------------------
// Affiliates (migration 000044_affiliates). `Affiliate`,
// `AffiliateCommissionRule`, `AffiliateConversion`,
// `AffiliateCommission` and `AffiliatePayout` carry NUMERIC columns,
// so per the repo header rule they skip ALL serde derives.
// ---------------------------------------------------------------------------

diesel::table! {
    affiliates (id) {
        id -> Uuid,
        user_id -> Varchar,
        display_name -> Varchar,
        email -> Varchar,
        status -> Varchar,
        commission_type -> Varchar,
        commission_value -> Numeric,
        currency -> Bpchar,
        attribution_window_days -> Int4,
        company_id -> Nullable<Varchar>,
        payout_method -> Nullable<Varchar>,
        payout_reference -> Nullable<Varchar>,
        reviewed_by -> Nullable<Varchar>,
        review_reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    affiliate_commission_rules (id) {
        id -> Uuid,
        affiliate_id -> Uuid,
        product_id -> Varchar,
        commission_type -> Varchar,
        commission_value -> Numeric,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    affiliate_links (id) {
        id -> Uuid,
        affiliate_id -> Uuid,
        code -> Varchar,
        campaign -> Nullable<Varchar>,
        product_id -> Nullable<Varchar>,
        active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    affiliate_clicks (id) {
        id -> Uuid,
        link_id -> Uuid,
        affiliate_id -> Uuid,
        ip_hash -> Varchar,
        user_agent -> Nullable<Varchar>,
        referrer -> Nullable<Varchar>,
        flagged -> Bool,
        flag_reasons -> Array<Text>,
        converted -> Bool,
        clicked_at -> Timestamptz,
    }
}

diesel::table! {
    affiliate_conversions (id) {
        id -> Uuid,
        affiliate_id -> Uuid,
        link_id -> Nullable<Uuid>,
        click_id -> Nullable<Uuid>,
        customer_user_id -> Varchar,
        kind -> Varchar,
        order_id -> Nullable<Varchar>,
        order_total -> Numeric,
        currency -> Bpchar,
        product_id -> Nullable<Varchar>,
        flagged -> Bool,
        flag_reasons -> Array<Text>,
        attributed_at -> Timestamptz,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    affiliate_commissions (id) {
        id -> Uuid,
        affiliate_id -> Uuid,
        conversion_id -> Uuid,
        order_id -> Nullable<Varchar>,
        product_id -> Nullable<Varchar>,
        commission_type -> Varchar,
        rate_or_amount -> Numeric,
        amount -> Numeric,
        currency -> Bpchar,
        status -> Varchar,
        payout_id -> Nullable<Uuid>,
        flagged -> Bool,
        fraud_flags -> Array<Text>,
        reviewed_by -> Nullable<Varchar>,
        review_reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    affiliate_payouts (id) {
        id -> Uuid,
        affiliate_id -> Uuid,
        commission_ids -> Array<Uuid>,
        total_amount -> Numeric,
        currency -> Bpchar,
        status -> Varchar,
        method -> Nullable<Varchar>,
        reference -> Nullable<Varchar>,
        requested_by -> Varchar,
        reviewed_by -> Nullable<Varchar>,
        review_reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(affiliate_commission_rules -> affiliates (affiliate_id));
diesel::joinable!(affiliate_links -> affiliates (affiliate_id));
diesel::joinable!(affiliate_clicks -> affiliate_links (link_id));
diesel::joinable!(affiliate_clicks -> affiliates (affiliate_id));
diesel::joinable!(affiliate_conversions -> affiliates (affiliate_id));
diesel::joinable!(affiliate_conversions -> affiliate_links (link_id));
diesel::joinable!(affiliate_conversions -> affiliate_clicks (click_id));
diesel::joinable!(affiliate_commissions -> affiliates (affiliate_id));
diesel::joinable!(affiliate_commissions -> affiliate_conversions (conversion_id));
diesel::joinable!(affiliate_payouts -> affiliates (affiliate_id));

// ---------------------------------------------------------------------------
// Loyalty (migration 000045_loyalty). Every table here carries a
// NUMERIC column, so all five pairs skip ALL serde derives per the
// repo header rule.
// ---------------------------------------------------------------------------

diesel::table! {
    loyalty_programs (id) {
        id -> Uuid,
        company_id -> Nullable<Varchar>,
        name -> Varchar,
        description -> Nullable<Text>,
        points_name -> Varchar,
        currency -> Bpchar,
        discount_per_point -> Numeric,
        active -> Bool,
        expiry_days -> Nullable<Int4>,
        expiry_mode -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    loyalty_earn_rules (id) {
        id -> Uuid,
        program_id -> Uuid,
        kind -> Varchar,
        points_per_unit -> Numeric,
        fixed_points -> Nullable<Int8>,
        cap_per_day -> Nullable<Int8>,
        cap_total -> Nullable<Int8>,
        min_order_amount -> Nullable<Numeric>,
        active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    loyalty_tiers (id) {
        id -> Uuid,
        program_id -> Uuid,
        name -> Varchar,
        min_lifetime_points -> Int8,
        min_lifetime_spend -> Numeric,
        benefits -> Array<Text>,
        discount_percent -> Numeric,
        sort_order -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    loyalty_ledger (id) {
        id -> Uuid,
        program_id -> Uuid,
        customer_id -> Varchar,
        kind -> Varchar,
        points -> Int8,
        balance_after -> Int8,
        reason -> Text,
        actor -> Varchar,
        earn_kind -> Nullable<Varchar>,
        reference_id -> Nullable<Varchar>,
        reference_amount -> Nullable<Numeric>,
        expires_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    loyalty_redemptions (id) {
        id -> Uuid,
        program_id -> Uuid,
        customer_id -> Varchar,
        points_spent -> Int8,
        kind -> Varchar,
        discount_amount -> Numeric,
        currency -> Bpchar,
        product_id -> Nullable<Varchar>,
        order_id -> Nullable<Varchar>,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(loyalty_earn_rules -> loyalty_programs (program_id));
diesel::joinable!(loyalty_tiers -> loyalty_programs (program_id));
diesel::joinable!(loyalty_ledger -> loyalty_programs (program_id));
diesel::joinable!(loyalty_redemptions -> loyalty_programs (program_id));

// ---------------------------------------------------------------------------
// Client portals (migration 000046_portals).
// ---------------------------------------------------------------------------

diesel::table! {
    portals (id) {
        id -> Uuid,
        company_id -> Varchar,
        name -> Varchar,
        slug -> Varchar,
        custom_domain -> Nullable<Varchar>,
        domain_status -> Varchar,
        logo_storage_target -> Nullable<Varchar>,
        logo_storage_key -> Nullable<Varchar>,
        primary_color -> Varchar,
        secondary_color -> Varchar,
        accent_color -> Varchar,
        background_color -> Varchar,
        text_color -> Varchar,
        sections -> Array<Text>,
        status -> Varchar,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    portal_menus (id) {
        id -> Uuid,
        portal_id -> Uuid,
        label -> Varchar,
        target_kind -> Varchar,
        target -> Varchar,
        position -> Int8,
        visible -> Bool,
        roles -> Array<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    portal_views (id) {
        id -> Uuid,
        portal_id -> Uuid,
        slug -> Varchar,
        viewer_user_id -> Nullable<Varchar>,
        ip -> Nullable<Varchar>,
        user_agent -> Nullable<Varchar>,
        viewed_at -> Timestamptz,
    }
}

diesel::joinable!(portal_menus -> portals (portal_id));
diesel::joinable!(portal_views -> portals (portal_id));

// ---------------------------------------------------------------------------
// Forms (migration 000047_forms).
// ---------------------------------------------------------------------------

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

diesel::joinable!(form_fields -> forms (form_id));
diesel::joinable!(form_submissions -> forms (form_id));
diesel::joinable!(form_submission_values -> form_submissions (submission_id));
diesel::joinable!(form_submission_values -> form_fields (field_id));

// ---------------------------------------------------------------------------
// Contracts (migration 000048_contracts). `Contract` carries a
// NUMERIC column, so it skips ALL serde derives per the repo header
// rule. The self-reference `contracts.renewed_from_id` gets no
// `joinable!` (a table cannot join to itself in that form).
// ---------------------------------------------------------------------------

diesel::table! {
    contract_templates (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        body -> Text,
        placeholders -> Array<Text>,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    contracts (id) {
        id -> Uuid,
        template_id -> Nullable<Uuid>,
        title -> Varchar,
        company_party -> Varchar,
        counterparty_name -> Varchar,
        counterparty_email -> Varchar,
        terms -> Text,
        value -> Numeric,
        currency -> Bpchar,
        signing_order -> Varchar,
        status -> Varchar,
        starts_at -> Nullable<Timestamptz>,
        ends_at -> Nullable<Timestamptz>,
        signed_at -> Nullable<Timestamptz>,
        content_hash -> Nullable<Varchar>,
        void_reason -> Nullable<Text>,
        terminate_reason -> Nullable<Text>,
        renewed_from_id -> Nullable<Uuid>,
        alert_days -> Array<Int4>,
        expiry_alert_status -> Varchar,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    contract_signers (id) {
        id -> Uuid,
        contract_id -> Uuid,
        name -> Varchar,
        email -> Varchar,
        order_index -> Int4,
        status -> Varchar,
        token_hash -> Varchar,
        token_expires_at -> Timestamptz,
        signed_at -> Nullable<Timestamptz>,
        declined_reason -> Nullable<Text>,
        viewed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    contract_audit_trail (id) {
        id -> Uuid,
        contract_id -> Uuid,
        signer_id -> Nullable<Uuid>,
        actor -> Varchar,
        action -> Varchar,
        ip_hash -> Varchar,
        user_agent_hash -> Varchar,
        details -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}

diesel::joinable!(contracts -> contract_templates (template_id));
diesel::joinable!(contract_signers -> contracts (contract_id));
diesel::joinable!(contract_audit_trail -> contracts (contract_id));
diesel::joinable!(contract_audit_trail -> contract_signers (signer_id));

// ---------------------------------------------------------------------------
// Procurement (migration 000049_procurement). `PurchaseOrder`,
// `PurchaseOrderLine`, `GoodsReceipt` and `ProcurementBudget`
// carry NUMERIC columns, so they skip ALL serde derives per the
// repo header rule.
// ---------------------------------------------------------------------------

diesel::table! {
    vendors (id) {
        id -> Uuid,
        name -> Varchar,
        contact_name -> Nullable<Varchar>,
        email -> Nullable<Varchar>,
        phone -> Nullable<Varchar>,
        address -> Nullable<Varchar>,
        categories -> Array<Text>,
        tags -> Array<Text>,
        status -> Varchar,
        payment_reference -> Nullable<Varchar>,
        notes -> Nullable<Text>,
        rating -> Nullable<Int2>,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    purchase_orders (id) {
        id -> Uuid,
        po_number -> Varchar,
        vendor_id -> Uuid,
        title -> Varchar,
        category -> Nullable<Varchar>,
        currency -> Bpchar,
        status -> Varchar,
        subtotal -> Numeric,
        tax_total -> Numeric,
        total -> Numeric,
        promised_date -> Nullable<Date>,
        ordered_at -> Nullable<Timestamptz>,
        received_at -> Nullable<Timestamptz>,
        cancel_reason -> Nullable<Text>,
        requested_by -> Varchar,
        approved_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    purchase_order_lines (id) {
        id -> Uuid,
        purchase_order_id -> Uuid,
        description -> Text,
        quantity -> Numeric,
        unit_price -> Numeric,
        tax_rate -> Numeric,
        line_total -> Numeric,
        received_quantity -> Numeric,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    purchase_order_approvals (id) {
        id -> Uuid,
        purchase_order_id -> Uuid,
        approver_id -> Varchar,
        approver_role -> Varchar,
        decision -> Varchar,
        reason -> Nullable<Text>,
        decided_at -> Timestamptz,
    }
}

diesel::table! {
    goods_receipts (id) {
        id -> Uuid,
        purchase_order_id -> Uuid,
        line_id -> Uuid,
        quantity_received -> Numeric,
        quality_note -> Nullable<Text>,
        received_by -> Varchar,
        received_at -> Timestamptz,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    procurement_budgets (id) {
        id -> Uuid,
        category -> Nullable<Varchar>,
        vendor_id -> Nullable<Uuid>,
        period_start -> Date,
        period_end -> Date,
        budget_amount -> Numeric,
        currency -> Bpchar,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(purchase_orders -> vendors (vendor_id));
diesel::joinable!(purchase_order_lines -> purchase_orders (purchase_order_id));
diesel::joinable!(purchase_order_approvals -> purchase_orders (purchase_order_id));
diesel::joinable!(goods_receipts -> purchase_orders (purchase_order_id));
diesel::joinable!(goods_receipts -> purchase_order_lines (line_id));
diesel::joinable!(procurement_budgets -> vendors (vendor_id));

// ---------------------------------------------------------------------------
// Assets (migration 000050_assets). `Asset` carries NUMERIC columns,
// so it skips ALL serde derives per the repo header rule.
// ---------------------------------------------------------------------------

diesel::table! {
    assets (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        asset_type -> Varchar,
        serial_number -> Nullable<Varchar>,
        purchase_date -> Nullable<Date>,
        purchase_price -> Numeric,
        currency -> Bpchar,
        vendor -> Nullable<Varchar>,
        location -> Nullable<Varchar>,
        status -> Varchar,
        warranty_end -> Nullable<Date>,
        license_expires_at -> Nullable<Date>,
        license_seats -> Nullable<Int8>,
        depreciation_method -> Nullable<Varchar>,
        useful_life_years -> Nullable<Int8>,
        salvage_value -> Numeric,
        depreciation_status -> Varchar,
        depreciation_schedule -> Nullable<Jsonb>,
        depreciation_error -> Nullable<Text>,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    asset_assignments (id) {
        id -> Uuid,
        asset_id -> Uuid,
        user_id -> Varchar,
        assigned_by -> Varchar,
        assigned_at -> Timestamptz,
        returned_at -> Nullable<Timestamptz>,
        return_reason -> Nullable<Text>,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    asset_stock (id) {
        id -> Uuid,
        asset_id -> Uuid,
        quantity_on_hand -> Int8,
        reorder_threshold -> Int8,
        location -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    asset_stock_adjustments (id) {
        id -> Uuid,
        stock_id -> Uuid,
        asset_id -> Uuid,
        delta -> Int8,
        quantity_after -> Int8,
        reason -> Text,
        adjusted_by -> Varchar,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    asset_stock_takes (id) {
        id -> Uuid,
        title -> Varchar,
        notes -> Nullable<Text>,
        status -> Varchar,
        started_by -> Varchar,
        started_at -> Timestamptz,
        closed_at -> Nullable<Timestamptz>,
        closed_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    asset_stock_take_lines (id) {
        id -> Uuid,
        stock_take_id -> Uuid,
        asset_id -> Uuid,
        stock_id -> Uuid,
        expected_quantity -> Int8,
        counted_quantity -> Nullable<Int8>,
        counted_by -> Nullable<Varchar>,
        note -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(asset_assignments -> assets (asset_id));
diesel::joinable!(asset_stock -> assets (asset_id));
diesel::joinable!(asset_stock_adjustments -> asset_stock (stock_id));
diesel::joinable!(asset_stock_adjustments -> assets (asset_id));
diesel::joinable!(asset_stock_take_lines -> asset_stock_takes (stock_take_id));
diesel::joinable!(asset_stock_take_lines -> assets (asset_id));
diesel::joinable!(asset_stock_take_lines -> asset_stock (stock_id));

// ---------------------------------------------------------------------------
// Recruitment (migration 000051_recruitment). `Job` and `Offer`
// carry NUMERIC columns, so they skip ALL serde derives per the
// repo header rule.
// ---------------------------------------------------------------------------

diesel::table! {
    jobs (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Nullable<Text>,
        department_id -> Nullable<Varchar>,
        location -> Nullable<Varchar>,
        job_type -> Varchar,
        salary_min -> Nullable<Numeric>,
        salary_max -> Nullable<Numeric>,
        currency -> Bpchar,
        status -> Varchar,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    applicants (id) {
        id -> Uuid,
        job_id -> Uuid,
        name -> Varchar,
        email -> Varchar,
        phone -> Nullable<Varchar>,
        source -> Nullable<Varchar>,
        resume_storage_target -> Nullable<Varchar>,
        resume_storage_key -> Nullable<Varchar>,
        consent_contact -> Bool,
        consent_at -> Nullable<Timestamptz>,
        stage -> Varchar,
        notes -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    applicant_stage_history (id) {
        id -> Uuid,
        applicant_id -> Uuid,
        job_id -> Uuid,
        from_stage -> Nullable<Varchar>,
        to_stage -> Varchar,
        moved_by -> Varchar,
        reason -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    interviews (id) {
        id -> Uuid,
        applicant_id -> Uuid,
        job_id -> Uuid,
        scheduled_at -> Timestamptz,
        type -> Varchar,
        interviewers -> Jsonb,
        notes -> Nullable<Text>,
        outcome -> Nullable<Varchar>,
        outcome_notes -> Nullable<Text>,
        status -> Varchar,
        scheduler_status -> Varchar,
        scheduler_job_id -> Nullable<Varchar>,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    evaluations (id) {
        id -> Uuid,
        applicant_id -> Uuid,
        job_id -> Uuid,
        interviewer_id -> Varchar,
        interview_id -> Nullable<Uuid>,
        scores -> Jsonb,
        notes -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    offers (id) {
        id -> Uuid,
        applicant_id -> Uuid,
        job_id -> Uuid,
        salary -> Numeric,
        currency -> Bpchar,
        start_date -> Nullable<Date>,
        terms -> Nullable<Text>,
        status -> Varchar,
        created_by -> Varchar,
        sent_at -> Nullable<Timestamptz>,
        decided_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(applicants -> jobs (job_id));
diesel::joinable!(applicant_stage_history -> applicants (applicant_id));
diesel::joinable!(applicant_stage_history -> jobs (job_id));
diesel::joinable!(interviews -> applicants (applicant_id));
diesel::joinable!(interviews -> jobs (job_id));
diesel::joinable!(evaluations -> applicants (applicant_id));
diesel::joinable!(evaluations -> jobs (job_id));
diesel::joinable!(evaluations -> interviews (interview_id));
diesel::joinable!(offers -> applicants (applicant_id));
diesel::joinable!(offers -> jobs (job_id));

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

diesel::joinable!(vault_documents -> vault_folders (folder_id));
diesel::joinable!(vault_document_versions -> vault_documents (document_id));
diesel::joinable!(vault_access_log -> vault_documents (document_id));
diesel::joinable!(vault_share_links -> vault_documents (document_id));
diesel::joinable!(vault_access_requests -> vault_documents (document_id));

// ---------------------------------------------------------------------------
// Incidents & on-call (migration 000053_incidents). The
// self-reference `on_call_schedules.overrides_entry_id` gets no
// `joinable!` (a table cannot join to itself in that form).
// ---------------------------------------------------------------------------

diesel::table! {
    incidents (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Nullable<Text>,
        severity -> Varchar,
        status -> Varchar,
        affected_service_ids -> Array<Text>,
        responders -> Array<Text>,
        current_responder -> Nullable<Varchar>,
        source -> Varchar,
        source_service_id -> Nullable<Varchar>,
        escalation_policy_id -> Nullable<Uuid>,
        rotation_id -> Nullable<Uuid>,
        created_by -> Varchar,
        acknowledged_by -> Nullable<Varchar>,
        acknowledged_at -> Nullable<Timestamptz>,
        resolved_at -> Nullable<Timestamptz>,
        closed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    incident_timeline (id) {
        id -> Uuid,
        incident_id -> Uuid,
        actor_id -> Varchar,
        kind -> Varchar,
        message -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    on_call_rotations (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        members -> Array<Text>,
        handover_interval_days -> Int8,
        handover_time -> Varchar,
        timezone -> Varchar,
        current_index -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    on_call_schedules (id) {
        id -> Uuid,
        rotation_id -> Uuid,
        member_id -> Varchar,
        starts_at -> Timestamptz,
        ends_at -> Timestamptz,
        kind -> Varchar,
        overrides_entry_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    escalation_policies (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        applies_to_severities -> Array<Text>,
        steps -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    incident_escalations (id) {
        id -> Uuid,
        incident_id -> Uuid,
        policy_id -> Uuid,
        step_index -> Int8,
        next_escalation_at -> Timestamptz,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    page_attempts (id) {
        id -> Uuid,
        incident_id -> Uuid,
        paged_user_id -> Varchar,
        channel -> Varchar,
        status -> Varchar,
        acknowledged_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    incident_postmortems (id) {
        id -> Uuid,
        incident_id -> Uuid,
        summary -> Text,
        timeline_summary -> Nullable<Text>,
        root_cause -> Nullable<Text>,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    postmortem_action_items (id) {
        id -> Uuid,
        postmortem_id -> Uuid,
        title -> Varchar,
        assignee_id -> Nullable<Varchar>,
        status -> Varchar,
        task_reference -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    auto_open_rules (service_id) {
        service_id -> Varchar,
        enabled -> Bool,
        severity -> Varchar,
        auto_resolve_on_recovery -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(incident_timeline -> incidents (incident_id));
diesel::joinable!(on_call_schedules -> on_call_rotations (rotation_id));
diesel::joinable!(incident_escalations -> incidents (incident_id));
diesel::joinable!(incident_escalations -> escalation_policies (policy_id));
diesel::joinable!(page_attempts -> incidents (incident_id));
diesel::joinable!(incident_postmortems -> incidents (incident_id));
diesel::joinable!(postmortem_action_items -> incident_postmortems (postmortem_id));
diesel::joinable!(incidents -> escalation_policies (escalation_policy_id));
diesel::joinable!(incidents -> on_call_rotations (rotation_id));

// ---------------------------------------------------------------------------
// Smart inbox (migration 000054_inbox). The self-reference
// `inbox_threads.merged_into` gets no `joinable!` (a table cannot
// join to itself in that form).
// ---------------------------------------------------------------------------

diesel::table! {
    inbox_threads (id) {
        id -> Uuid,
        source_channel -> Varchar,
        source_id -> Varchar,
        subject -> Varchar,
        customer_id -> Nullable<Varchar>,
        customer_label -> Nullable<Varchar>,
        participants -> Jsonb,
        department_id -> Nullable<Varchar>,
        assignee_id -> Nullable<Varchar>,
        status -> Varchar,
        priority -> Varchar,
        unread -> Bool,
        snoozed_until -> Nullable<Timestamptz>,
        snooze_scheduler_status -> Varchar,
        sla_due_at -> Nullable<Timestamptz>,
        sla_breached -> Bool,
        merged_into -> Nullable<Uuid>,
        last_activity_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    inbox_assignments (id) {
        id -> Uuid,
        thread_id -> Uuid,
        assignee_id -> Varchar,
        assigned_by -> Varchar,
        active -> Bool,
        assigned_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    inbox_canned_replies (id) {
        id -> Uuid,
        department_id -> Nullable<Varchar>,
        title -> Varchar,
        body -> Text,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    inbox_thread_reads (thread_id, user_id) {
        thread_id -> Uuid,
        user_id -> Varchar,
        read_at -> Timestamptz,
    }
}

diesel::joinable!(inbox_assignments -> inbox_threads (thread_id));
diesel::joinable!(inbox_thread_reads -> inbox_threads (thread_id));

// ---------------------------------------------------------------------------
// Surveys (migration 000055_surveys). The self-reference
// `surveys.supersedes` gets no `joinable!` (a table cannot join
// to itself in that form).
// ---------------------------------------------------------------------------

diesel::table! {
    surveys (id) {
        id -> Uuid,
        family_id -> Uuid,
        version -> Int4,
        supersedes -> Nullable<Uuid>,
        title -> Varchar,
        description -> Nullable<Text>,
        status -> Varchar,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    survey_questions (id) {
        id -> Uuid,
        survey_id -> Uuid,
        position -> Int4,
        question_type -> Varchar,
        text -> Varchar,
        required -> Bool,
        options -> Jsonb,
        min_value -> Nullable<Int4>,
        max_value -> Nullable<Int4>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    survey_distributions (id) {
        id -> Uuid,
        survey_id -> Uuid,
        trigger -> Varchar,
        department_id -> Nullable<Varchar>,
        product_id -> Nullable<Varchar>,
        agent_id -> Nullable<Varchar>,
        responsible_user_id -> Nullable<Varchar>,
        scheduled_at -> Nullable<Timestamptz>,
        expires_in_days -> Int4,
        status -> Varchar,
        scheduler_status -> Varchar,
        scheduler_job_id -> Nullable<Varchar>,
        sent_count -> Int8,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    survey_responses (id) {
        id -> Uuid,
        survey_id -> Uuid,
        distribution_id -> Nullable<Uuid>,
        recipient_id -> Varchar,
        recipient_email -> Nullable<Varchar>,
        agent_id -> Nullable<Varchar>,
        department_id -> Nullable<Varchar>,
        product_id -> Nullable<Varchar>,
        token -> Varchar,
        token_expires_at -> Timestamptz,
        submitted_at -> Nullable<Timestamptz>,
        follow_up_status -> Varchar,
        follow_up_task_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    survey_answers (id) {
        id -> Uuid,
        response_id -> Uuid,
        question_id -> Uuid,
        score -> Nullable<Int4>,
        text -> Nullable<Text>,
        choice -> Nullable<Varchar>,
        created_at -> Timestamptz,
    }
}

diesel::joinable!(survey_questions -> surveys (survey_id));
diesel::joinable!(survey_distributions -> surveys (survey_id));
diesel::joinable!(survey_responses -> surveys (survey_id));
diesel::joinable!(survey_responses -> survey_distributions (distribution_id));
diesel::joinable!(survey_answers -> survey_responses (response_id));
diesel::joinable!(survey_answers -> survey_questions (question_id));

// ---------------------------------------------------------------------------
// Community (migration 000056_community). The self-reference
// `community_posts.parent_post_id` gets no `joinable!` (a table
// cannot join to itself in that form).
// ---------------------------------------------------------------------------

diesel::table! {
    community_categories (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        slug -> Varchar,
        sort_order -> Int8,
        visibility -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    community_topics (id) {
        id -> Uuid,
        category_id -> Uuid,
        title -> Varchar,
        author_id -> Varchar,
        tags -> Array<Text>,
        pinned -> Bool,
        locked -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    community_posts (id) {
        id -> Uuid,
        topic_id -> Uuid,
        parent_post_id -> Nullable<Uuid>,
        author_id -> Varchar,
        body -> Text,
        hidden -> Bool,
        edited -> Bool,
        edited_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    community_post_edits (id) {
        id -> Uuid,
        post_id -> Uuid,
        editor_id -> Varchar,
        previous_body -> Text,
        edited_at -> Timestamptz,
    }
}

diesel::table! {
    community_votes (id) {
        id -> Uuid,
        post_id -> Uuid,
        user_id -> Varchar,
        value -> Int2,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    community_flags (id) {
        id -> Uuid,
        post_id -> Uuid,
        reporter_id -> Varchar,
        reason -> Varchar,
        status -> Varchar,
        reviewed_by -> Nullable<Varchar>,
        reviewed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    community_moderation_actions (id) {
        id -> Uuid,
        target_type -> Varchar,
        target_id -> Uuid,
        action -> Varchar,
        moderator_id -> Varchar,
        reason -> Nullable<Varchar>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    community_members (id) {
        id -> Uuid,
        user_id -> Varchar,
        display_name -> Varchar,
        badge_count -> Int8,
        joined_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    community_announcements (id) {
        id -> Uuid,
        title -> Varchar,
        body -> Text,
        pinned -> Bool,
        published -> Bool,
        published_at -> Nullable<Timestamptz>,
        author_id -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(community_topics -> community_categories (category_id));
diesel::joinable!(community_posts -> community_topics (topic_id));
diesel::joinable!(community_post_edits -> community_posts (post_id));
diesel::joinable!(community_votes -> community_posts (post_id));
diesel::joinable!(community_flags -> community_posts (post_id));

// ---------------------------------------------------------------------------
// Compliance (migration 000058_compliance).
// ---------------------------------------------------------------------------

diesel::table! {
    compliance_export_requests (id) {
        id -> Uuid,
        requester_id -> Varchar,
        scope -> Array<Text>,
        status -> Varchar,
        storage_target -> Nullable<Varchar>,
        storage_key -> Nullable<Varchar>,
        download_expires_at -> Nullable<Timestamptz>,
        failure_reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    compliance_deletion_requests (id) {
        id -> Uuid,
        requester_id -> Varchar,
        reason -> Text,
        status -> Varchar,
        approved_by -> Nullable<Varchar>,
        approved_at -> Nullable<Timestamptz>,
        executed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    compliance_deletion_items (id) {
        id -> Uuid,
        deletion_request_id -> Uuid,
        module_id -> Varchar,
        subject -> Text,
        status -> Varchar,
        detail -> Nullable<Text>,
        executed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    compliance_consents (id) {
        id -> Uuid,
        user_id -> Varchar,
        purpose -> Varchar,
        consent_text_version -> Varchar,
        granted -> Bool,
        recorded_at -> Timestamptz,
    }
}

diesel::table! {
    compliance_retention_policies (id) {
        id -> Uuid,
        data_category -> Varchar,
        retain_days -> Int8,
        review_action -> Varchar,
        description -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    compliance_retention_reviews (id) {
        id -> Uuid,
        policy_id -> Uuid,
        items_flagged -> Int8,
        notes -> Nullable<Text>,
        scheduler_status -> Varchar,
        scheduler_job_id -> Nullable<Varchar>,
        reviewed_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    compliance_processing_records (id) {
        id -> Uuid,
        purpose -> Varchar,
        data_categories -> Array<Text>,
        recipients -> Array<Text>,
        retention -> Varchar,
        lawful_basis -> Varchar,
        owner -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    compliance_checklists (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Nullable<Text>,
        status -> Varchar,
        evidence_storage_target -> Nullable<Varchar>,
        evidence_storage_key -> Nullable<Varchar>,
        due_date -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    compliance_breach_log (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Text,
        discovered_at -> Timestamptz,
        affected_scope -> Varchar,
        status -> Varchar,
        notification_status -> Varchar,
        reported_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    compliance_breach_timeline (id) {
        id -> Uuid,
        breach_id -> Uuid,
        author_id -> Varchar,
        entry -> Text,
        recorded_at -> Timestamptz,
    }
}

diesel::joinable!(compliance_deletion_items -> compliance_deletion_requests (deletion_request_id));
diesel::joinable!(compliance_retention_reviews -> compliance_retention_policies (policy_id));
diesel::joinable!(compliance_breach_timeline -> compliance_breach_log (breach_id));

// ---------------------------------------------------------------------------
// Gamification (migration 000059_gamification).
// ---------------------------------------------------------------------------

diesel::table! {
    gamification_rules (id) {
        id -> Uuid,
        name -> Varchar,
        action -> Varchar,
        points -> Int8,
        company_id -> Nullable<Varchar>,
        daily_cap -> Nullable<Int8>,
        active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    gamification_points_ledger (id) {
        id -> Uuid,
        user_id -> Varchar,
        company_id -> Nullable<Varchar>,
        department_id -> Nullable<Varchar>,
        rule_id -> Nullable<Uuid>,
        action -> Varchar,
        points -> Int8,
        balance_after -> Int8,
        season_id -> Nullable<Uuid>,
        reason -> Nullable<Varchar>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    gamification_badges (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Text,
        criteria_action -> Varchar,
        criteria_threshold -> Int8,
        icon -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    gamification_awards (id) {
        id -> Uuid,
        badge_id -> Uuid,
        user_id -> Varchar,
        reason -> Varchar,
        awarded_at -> Timestamptz,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    gamification_seasons (id) {
        id -> Uuid,
        name -> Varchar,
        company_id -> Nullable<Varchar>,
        starts_at -> Timestamptz,
        ends_at -> Timestamptz,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    gamification_rewards (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Text,
        points_cost -> Int8,
        active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    gamification_reward_requests (id) {
        id -> Uuid,
        reward_id -> Uuid,
        user_id -> Varchar,
        status -> Varchar,
        points_spent -> Int8,
        decided_by -> Nullable<Varchar>,
        decided_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(gamification_points_ledger -> gamification_rules (rule_id));
diesel::joinable!(gamification_awards -> gamification_badges (badge_id));
diesel::joinable!(gamification_reward_requests -> gamification_rewards (reward_id));

// ---------------------------------------------------------------------------
// Workflows (migration 000060_workflows). The self-reference
// `workflow_runs.retried_from_run_id` gets no `joinable!` (a
// table cannot join to itself in that form).
// ---------------------------------------------------------------------------

diesel::table! {
    workflows (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        trigger -> Jsonb,
        conditions -> Nullable<Jsonb>,
        actions -> Jsonb,
        is_active -> Bool,
        owner_id -> Varchar,
        company_id -> Nullable<Varchar>,
        webhook_token_hash -> Nullable<Varchar>,
        webhook_allowed_hosts -> Jsonb,
        run_count -> Int8,
        success_count -> Int8,
        failure_count -> Int8,
        dropped_count -> Int8,
        last_run_at -> Nullable<Timestamptz>,
        last_run_status -> Nullable<Varchar>,
        scheduler_status -> Varchar,
        scheduler_job_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    workflow_runs (id) {
        id -> Uuid,
        workflow_id -> Uuid,
        trigger_kind -> Varchar,
        trigger_name -> Nullable<Varchar>,
        status -> Varchar,
        payload_summary -> Jsonb,
        dry_run -> Bool,
        depth -> Int4,
        condition_matched -> Bool,
        skipped_reason -> Nullable<Text>,
        retried_from_run_id -> Nullable<Uuid>,
        duration_ms -> Nullable<Int8>,
        error -> Nullable<Text>,
        started_at -> Timestamptz,
        finished_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    workflow_run_steps (id) {
        id -> Uuid,
        run_id -> Uuid,
        workflow_id -> Uuid,
        step_index -> Int4,
        action_kind -> Varchar,
        status -> Varchar,
        attempts -> Int4,
        error -> Nullable<Text>,
        detail -> Jsonb,
        duration_ms -> Nullable<Int8>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(workflow_runs -> workflows (workflow_id));
diesel::joinable!(workflow_run_steps -> workflow_runs (run_id));
diesel::joinable!(workflow_run_steps -> workflows (workflow_id));

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
        checksum -> Text,
        storage_key -> Text,
        current_version -> Int4,
        version_count -> Int4,
        scan_status -> Text,
        scan_threat -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    document_versions (id) {
        id -> Uuid,
        document_id -> Uuid,
        version -> Int4,
        checksum -> Text,
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

diesel::joinable!(document_versions -> documents (document_id));
diesel::joinable!(document_upload_sessions -> documents (document_id));
diesel::joinable!(document_upload_parts -> document_upload_sessions (session_id));
diesel::joinable!(document_downloads -> documents (document_id));

// ---------------------------------------------------------------------------
// Privacy (migration 000066_privacy).
// ---------------------------------------------------------------------------

diesel::table! {
    privacy_field_declarations (id) {
        id -> Uuid,
        module_id -> Text,
        field_name -> Text,
        purpose -> Text,
        required -> Bool,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    privacy_anonymizations (id) {
        id -> Uuid,
        user_id -> Text,
        token -> Text,
        field_tokens -> Jsonb,
        scope_modules -> Array<Text>,
        reason -> Nullable<Text>,
        actor -> Text,
        times_applied -> Int8,
        anonymized_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    privacy_visibility_defaults (id) {
        id -> Uuid,
        module_id -> Text,
        visibility -> Text,
        explicit -> Bool,
        configured_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    privacy_consents (id) {
        id -> Uuid,
        user_id -> Text,
        consent_type -> Text,
        granted -> Bool,
        granted_at -> Nullable<Timestamptz>,
        withdrawn_at -> Nullable<Timestamptz>,
        expires_at -> Nullable<Timestamptz>,
        source -> Text,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    privacy_consent_history (id) {
        id -> Uuid,
        consent_id -> Uuid,
        user_id -> Text,
        consent_type -> Text,
        action -> Text,
        actor -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    privacy_erasure_requests (id) {
        id -> Uuid,
        user_id -> Text,
        modules -> Array<Text>,
        reason -> Text,
        status -> Text,
        requested_by -> Text,
        reviewed_by -> Nullable<Text>,
        reviewed_at -> Nullable<Timestamptz>,
        rejection_reason -> Nullable<Text>,
        fulfilled_at -> Nullable<Timestamptz>,
        anonymization_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    privacy_notices (id) {
        id -> Uuid,
        module_id -> Text,
        text -> Text,
        version -> Int8,
        effective_from -> Timestamptz,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    privacy_audit_entries (id) {
        id -> Uuid,
        actor -> Text,
        action -> Text,
        resource -> Text,
        details -> Jsonb,
        created_at -> Timestamptz,
    }
}

diesel::joinable!(privacy_consent_history -> privacy_consents (consent_id));
diesel::joinable!(privacy_erasure_requests -> privacy_anonymizations (anonymization_id));

// ---------------------------------------------------------------------------
// Web3 (migration 000067_web3). `Web3Transaction` and `Web3Balance`
// carry NUMERIC columns, so they skip ALL serde derives per the
// repo header rule. `api_key_encrypted` / `anchoring_key_encrypted`
// are write-only ciphertext and get `#[serde(skip_serializing)]`
// on the read structs.
// ---------------------------------------------------------------------------

diesel::table! {
    web3_networks (id) {
        id -> Uuid,
        name -> Text,
        kind -> Text,
        endpoint_url -> Text,
        chain_id -> Nullable<Int8>,
        native_asset_symbol -> Text,
        explorer_url_template -> Nullable<Text>,
        enabled -> Bool,
        indexer_url -> Nullable<Text>,
        verify_endpoint -> Nullable<Text>,
        api_key_encrypted -> Nullable<Text>,
        allow_private_endpoint -> Bool,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    web3_watched_addresses (id) {
        id -> Uuid,
        network_id -> Uuid,
        address -> Text,
        label -> Nullable<Text>,
        user_id -> Text,
        company_id -> Nullable<Uuid>,
        last_checked_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    web3_transactions (id) {
        id -> Uuid,
        network_id -> Uuid,
        tx_hash -> Text,
        from_address -> Text,
        to_address -> Text,
        amount_raw -> Text,
        amount -> Nullable<Numeric>,
        asset -> Text,
        status -> Text,
        confirmations -> Int8,
        block_number -> Nullable<Int8>,
        direction -> Text,
        watched_address_id -> Nullable<Uuid>,
        occurred_at -> Nullable<Timestamptz>,
        first_seen_at -> Timestamptz,
        last_checked_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    web3_anchors (id) {
        id -> Uuid,
        record_type -> Text,
        record_id -> Text,
        content_sha256 -> Text,
        network_id -> Uuid,
        status -> Text,
        tx_hash -> Nullable<Text>,
        confirmations -> Int8,
        block_number -> Nullable<Int8>,
        requested_by -> Text,
        company_id -> Nullable<Uuid>,
        submitted_at -> Nullable<Timestamptz>,
        confirmed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    web3_wallets (id) {
        id -> Uuid,
        user_id -> Text,
        company_id -> Nullable<Uuid>,
        network_id -> Uuid,
        address -> Text,
        label -> Nullable<Text>,
        verification_status -> Text,
        challenge_nonce_hash -> Nullable<Text>,
        challenge_expires_at -> Nullable<Timestamptz>,
        verified_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    web3_balances (id) {
        id -> Uuid,
        wallet_id -> Uuid,
        network_id -> Uuid,
        address -> Text,
        asset -> Text,
        amount_raw -> Text,
        amount -> Nullable<Numeric>,
        decimals -> Int4,
        checked_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    web3_settings (id) {
        id -> Uuid,
        scope -> Text,
        public_ledger_enabled -> Bool,
        allowed_private_hosts -> Array<Text>,
        allowed_extra_hosts -> Array<Text>,
        anchoring_relay_url -> Nullable<Text>,
        anchoring_key_encrypted -> Nullable<Text>,
        max_poll_transactions -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(web3_watched_addresses -> web3_networks (network_id));
diesel::joinable!(web3_transactions -> web3_networks (network_id));
diesel::joinable!(web3_transactions -> web3_watched_addresses (watched_address_id));
diesel::joinable!(web3_anchors -> web3_networks (network_id));
diesel::joinable!(web3_wallets -> web3_networks (network_id));
diesel::joinable!(web3_balances -> web3_wallets (wallet_id));
diesel::joinable!(web3_balances -> web3_networks (network_id));

// ---------------------------------------------------------------------------
// Point of sale (migration 000076_pos). Every table except
// `pos_registers` carries NUMERIC columns, so those six pairs skip
// ALL serde derives per the repo header rule.
// ---------------------------------------------------------------------------

diesel::table! {
    pos_registers (id) {
        id -> Uuid,
        name -> Varchar,
        code -> Varchar,
        location -> Nullable<Varchar>,
        status -> Varchar,
        cashiers -> Array<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pos_shifts (id) {
        id -> Uuid,
        register_id -> Uuid,
        cashier_id -> Varchar,
        status -> Varchar,
        opening_float -> Numeric,
        currency -> Bpchar,
        expected_cash -> Nullable<Numeric>,
        counted_cash -> Nullable<Numeric>,
        variance -> Nullable<Numeric>,
        notes -> Nullable<Text>,
        opened_at -> Timestamptz,
        closed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pos_sales (id) {
        id -> Uuid,
        number -> Varchar,
        register_id -> Uuid,
        shift_id -> Nullable<Uuid>,
        cashier_id -> Varchar,
        company_id -> Nullable<Varchar>,
        discount_type -> Nullable<Varchar>,
        discount_value -> Numeric,
        discount_total -> Numeric,
        subtotal -> Numeric,
        tax_total -> Numeric,
        total -> Numeric,
        currency -> Bpchar,
        tax_inclusive -> Bool,
        status -> Varchar,
        amount_paid -> Numeric,
        change_due -> Numeric,
        refunded_amount -> Numeric,
        customer_name -> Nullable<Varchar>,
        notes -> Nullable<Text>,
        completed_at -> Nullable<Timestamptz>,
        voided_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pos_sale_items (id) {
        id -> Uuid,
        sale_id -> Uuid,
        product_id -> Varchar,
        name -> Varchar,
        barcode -> Nullable<Varchar>,
        sku -> Nullable<Varchar>,
        unit_price -> Numeric,
        quantity -> Int4,
        refunded_quantity -> Int4,
        discount_type -> Nullable<Varchar>,
        discount_value -> Numeric,
        discount_amount -> Numeric,
        line_subtotal -> Numeric,
        tax_rate -> Numeric,
        tax_amount -> Numeric,
        line_total -> Numeric,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pos_payments (id) {
        id -> Uuid,
        sale_id -> Uuid,
        method_id -> Varchar,
        method_code -> Varchar,
        amount -> Numeric,
        tendered -> Numeric,
        reference -> Nullable<Varchar>,
        recorded_by -> Varchar,
        recorded_at -> Timestamptz,
    }
}

diesel::table! {
    pos_refunds (id) {
        id -> Uuid,
        sale_id -> Uuid,
        shift_id -> Nullable<Uuid>,
        amount -> Numeric,
        method_id -> Varchar,
        method_code -> Varchar,
        items -> Jsonb,
        reason -> Text,
        refunded_by -> Varchar,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    pos_receipts (id) {
        id -> Uuid,
        number -> Varchar,
        sale_id -> Uuid,
        sale_number -> Varchar,
        register_id -> Uuid,
        register_code -> Nullable<Varchar>,
        cashier_id -> Varchar,
        store_name -> Varchar,
        store_address -> Nullable<Varchar>,
        subtotal -> Numeric,
        discount_total -> Numeric,
        tax_total -> Numeric,
        total -> Numeric,
        currency -> Bpchar,
        tax_breakdown -> Jsonb,
        amount_paid -> Numeric,
        change_due -> Numeric,
        print_lines -> Jsonb,
        footer -> Nullable<Text>,
        issued_at -> Timestamptz,
        created_at -> Timestamptz,
    }
}

diesel::joinable!(pos_shifts -> pos_registers (register_id));
diesel::joinable!(pos_sales -> pos_registers (register_id));
diesel::joinable!(pos_sales -> pos_shifts (shift_id));
diesel::joinable!(pos_sale_items -> pos_sales (sale_id));
diesel::joinable!(pos_payments -> pos_sales (sale_id));
diesel::joinable!(pos_refunds -> pos_sales (sale_id));
diesel::joinable!(pos_refunds -> pos_shifts (shift_id));
diesel::joinable!(pos_receipts -> pos_sales (sale_id));
diesel::joinable!(pos_receipts -> pos_registers (register_id));

// ---------------------------------------------------------------------------
// Appointments (migration 000077_appointments). `AppointmentService`
// and `Appointment` carry NUMERIC columns, so they skip ALL serde
// derives per the repo header rule.
// ---------------------------------------------------------------------------

diesel::table! {
    appointment_services (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        duration_minutes -> Int4,
        price -> Numeric,
        currency -> Bpchar,
        capacity -> Int4,
        requires_staff -> Bool,
        location_type -> Varchar,
        location -> Nullable<Varchar>,
        buffer_before_minutes -> Nullable<Int4>,
        buffer_after_minutes -> Nullable<Int4>,
        cancellation_notice_hours -> Nullable<Int4>,
        cancellation_fee_percent -> Nullable<Numeric>,
        is_active -> Bool,
        company_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    appointment_staff_availability (id) {
        id -> Uuid,
        staff_user_id -> Varchar,
        kind -> Varchar,
        weekday -> Nullable<Int4>,
        date -> Nullable<Date>,
        start_minutes -> Nullable<Int4>,
        end_minutes -> Nullable<Int4>,
        break_start_minutes -> Nullable<Int4>,
        break_end_minutes -> Nullable<Int4>,
        valid_from -> Nullable<Date>,
        valid_until -> Nullable<Date>,
        reason -> Nullable<Varchar>,
        is_active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    appointment_staff_services (id) {
        id -> Uuid,
        staff_user_id -> Varchar,
        service_id -> Uuid,
        is_active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    appointments (id) {
        id -> Uuid,
        service_id -> Uuid,
        staff_user_id -> Nullable<Varchar>,
        customer_user_id -> Nullable<Varchar>,
        customer_name -> Varchar,
        customer_email -> Varchar,
        customer_phone -> Nullable<Varchar>,
        starts_at -> Timestamptz,
        ends_at -> Timestamptz,
        status -> Varchar,
        source -> Varchar,
        price -> Numeric,
        currency -> Bpchar,
        cancellation_fee_charged -> Nullable<Numeric>,
        cancelled_at -> Nullable<Timestamptz>,
        cancel_reason -> Nullable<Varchar>,
        notes -> Nullable<Text>,
        confirmation_code -> Varchar,
        company_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    appointment_reminders (id) {
        id -> Uuid,
        appointment_id -> Uuid,
        offset_minutes -> Int4,
        channels -> Array<Text>,
        status -> Varchar,
        scheduler_job_id -> Nullable<Varchar>,
        scheduler_status -> Varchar,
        sent_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(appointment_staff_services -> appointment_services (service_id));
diesel::joinable!(appointments -> appointment_services (service_id));
diesel::joinable!(appointment_reminders -> appointments (appointment_id));

// ---------------------------------------------------------------------------
// Digital products (migration 000078_digital_products).
// `DigitalProduct`, `DigitalPayoutRequest` and `DigitalRoyalty`
// carry NUMERIC columns, so they skip ALL serde derives per the
// repo header rule.
// ---------------------------------------------------------------------------

diesel::table! {
    digital_product_types (id) {
        id -> Text,
        label -> Text,
        description -> Text,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    digital_products (id) {
        id -> Uuid,
        product_id -> Text,
        type_id -> Text,
        author -> Nullable<Text>,
        publisher -> Nullable<Text>,
        edition -> Nullable<Text>,
        isbn -> Nullable<Text>,
        language -> Nullable<Text>,
        pages -> Nullable<Int4>,
        formats -> Array<Text>,
        cover_storage_id -> Nullable<Text>,
        sample_allowed -> Bool,
        download_limit -> Int4,
        royalty_kind -> Text,
        royalty_value -> Numeric,
        royalty_payee -> Nullable<Text>,
        royalty_payee_company_id -> Nullable<Text>,
        status -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    digital_product_files (id) {
        id -> Uuid,
        digital_product_id -> Uuid,
        storage_file_id -> Text,
        format -> Text,
        label -> Nullable<Text>,
        file_size_bytes -> Nullable<Int8>,
        checksum -> Nullable<Text>,
        is_primary -> Bool,
        sort_order -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    digital_product_previews (id) {
        id -> Uuid,
        digital_product_id -> Uuid,
        storage_file_id -> Text,
        format -> Text,
        label -> Nullable<Text>,
        allowed -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    digital_payout_requests (id) {
        id -> Uuid,
        payee -> Nullable<Text>,
        payee_company_id -> Nullable<Text>,
        currency -> Bpchar,
        amount -> Numeric,
        royalty_ids -> Array<Uuid>,
        status -> Text,
        requested_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    digital_royalties (id) {
        id -> Uuid,
        digital_product_id -> Uuid,
        product_id -> Text,
        order_id -> Text,
        buyer_user_id -> Text,
        payee -> Nullable<Text>,
        payee_company_id -> Nullable<Text>,
        amount -> Numeric,
        currency -> Bpchar,
        royalty_kind -> Text,
        royalty_value -> Numeric,
        status -> Text,
        payout_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    digital_downloads (id) {
        id -> Uuid,
        digital_product_id -> Uuid,
        file_id -> Nullable<Uuid>,
        preview_id -> Nullable<Uuid>,
        buyer_user_id -> Text,
        order_id -> Nullable<Text>,
        grant_id -> Nullable<Text>,
        kind -> Text,
        watermark_subject -> Nullable<Text>,
        watermark_applied -> Bool,
        watermark_formats -> Array<Text>,
        downloaded_at -> Timestamptz,
    }
}

diesel::table! {
    digital_watermark_formats (format) {
        format -> Text,
        metadata_supported -> Bool,
        metadata_fields -> Array<Text>,
        notes -> Text,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(digital_products -> digital_product_types (type_id));
diesel::joinable!(digital_product_files -> digital_products (digital_product_id));
diesel::joinable!(digital_product_previews -> digital_products (digital_product_id));
diesel::joinable!(digital_royalties -> digital_products (digital_product_id));
diesel::joinable!(digital_royalties -> digital_payout_requests (payout_id));
diesel::joinable!(digital_downloads -> digital_products (digital_product_id));
diesel::joinable!(digital_downloads -> digital_product_files (file_id));
diesel::joinable!(digital_downloads -> digital_product_previews (preview_id));

// ---------------------------------------------------------------------------
// E-learning (migration 000079_elearning). `certificates` is NOT
// added here: the existing `certificates` table is the SSL
// certificates table (migration 000005) and the migration's
// `CREATE TABLE IF NOT EXISTS certificates` would be a no-op at
// runtime — see the header note.
// ---------------------------------------------------------------------------

diesel::table! {
    courses (id) {
        id -> Uuid,
        title -> Text,
        description -> Text,
        category -> Text,
        level -> Text,
        language -> Text,
        thumbnail_ref -> Nullable<Text>,
        instructor_id -> Text,
        status -> Text,
        price_minor -> Int8,
        currency -> Text,
        certificate_enabled -> Bool,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    course_modules (id) {
        id -> Uuid,
        course_id -> Uuid,
        title -> Text,
        description -> Nullable<Text>,
        position -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    lessons (id) {
        id -> Uuid,
        module_id -> Uuid,
        course_id -> Uuid,
        title -> Text,
        lesson_type -> Text,
        content_ref -> Nullable<Text>,
        body -> Nullable<Text>,
        position -> Int8,
        required -> Bool,
        available_from -> Nullable<Timestamptz>,
        requires_previous -> Bool,
        duration_minutes -> Nullable<Int8>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    enrollments (id) {
        id -> Uuid,
        course_id -> Uuid,
        user_id -> Text,
        status -> Text,
        price_paid_minor -> Int8,
        currency -> Text,
        order_id -> Nullable<Text>,
        learner_name -> Nullable<Text>,
        enrolled_at -> Timestamptz,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    lesson_progress (id) {
        id -> Uuid,
        enrollment_id -> Uuid,
        lesson_id -> Uuid,
        course_id -> Uuid,
        user_id -> Text,
        completed -> Bool,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    quizzes (id) {
        id -> Uuid,
        lesson_id -> Uuid,
        course_id -> Uuid,
        title -> Text,
        description -> Nullable<Text>,
        pass_mark -> Int8,
        max_attempts -> Nullable<Int8>,
        allow_review -> Bool,
        time_limit_minutes -> Nullable<Int8>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    quiz_questions (id) {
        id -> Uuid,
        quiz_id -> Uuid,
        question -> Text,
        options -> Jsonb,
        correct_index -> Int8,
        points -> Int8,
        position -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    quiz_attempts (id) {
        id -> Uuid,
        quiz_id -> Uuid,
        lesson_id -> Uuid,
        enrollment_id -> Uuid,
        user_id -> Text,
        answers -> Jsonb,
        score -> Int8,
        max_score -> Int8,
        percent -> Int8,
        passed -> Bool,
        submitted_at -> Timestamptz,
    }
}

diesel::table! {
    reviews (id) {
        id -> Uuid,
        course_id -> Uuid,
        user_id -> Text,
        rating -> Int8,
        comment -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    learning_paths (id) {
        id -> Uuid,
        title -> Text,
        description -> Nullable<Text>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    learning_path_courses (id) {
        id -> Uuid,
        path_id -> Uuid,
        course_id -> Uuid,
        position -> Int8,
        created_at -> Timestamptz,
    }
}

diesel::joinable!(course_modules -> courses (course_id));
diesel::joinable!(lessons -> course_modules (module_id));
diesel::joinable!(lessons -> courses (course_id));
diesel::joinable!(enrollments -> courses (course_id));
diesel::joinable!(lesson_progress -> enrollments (enrollment_id));
diesel::joinable!(lesson_progress -> lessons (lesson_id));
diesel::joinable!(lesson_progress -> courses (course_id));
diesel::joinable!(quizzes -> lessons (lesson_id));
diesel::joinable!(quizzes -> courses (course_id));
diesel::joinable!(quiz_questions -> quizzes (quiz_id));
diesel::joinable!(quiz_attempts -> quizzes (quiz_id));
diesel::joinable!(quiz_attempts -> lessons (lesson_id));
diesel::joinable!(quiz_attempts -> enrollments (enrollment_id));
diesel::joinable!(reviews -> courses (course_id));
diesel::joinable!(learning_path_courses -> learning_paths (path_id));
diesel::joinable!(learning_path_courses -> courses (course_id));

// ---------------------------------------------------------------------------
// Field service (migration 000080_field_service). No `joinable!`
// entries: the migration declares no foreign keys (all references
// are plain ids by design).
// ---------------------------------------------------------------------------

diesel::table! {
    field_equipment (id) {
        id -> Uuid,
        asset_id -> Nullable<Uuid>,
        equipment_type_id -> Uuid,
        name -> Text,
        serial_number -> Nullable<Text>,
        location -> Nullable<Text>,
        install_date -> Nullable<Date>,
        warranty_end -> Nullable<Date>,
        status -> Text,
        usage_reading -> Nullable<Float8>,
        usage_unit -> Nullable<Text>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    field_maintenance_plans (id) {
        id -> Uuid,
        equipment_id -> Uuid,
        template_id -> Nullable<Uuid>,
        name -> Text,
        description -> Nullable<Text>,
        interval_value -> Int8,
        interval_unit -> Text,
        last_service_at -> Nullable<Timestamptz>,
        usage_at_last_service -> Nullable<Float8>,
        next_due_at -> Nullable<Timestamptz>,
        next_due_usage -> Nullable<Float8>,
        is_active -> Bool,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    field_work_orders (id) {
        id -> Uuid,
        equipment_id -> Uuid,
        plan_id -> Nullable<Uuid>,
        project_id -> Nullable<Text>,
        title -> Text,
        description -> Nullable<Text>,
        order_type -> Text,
        priority -> Text,
        status -> Text,
        assignee_id -> Nullable<Text>,
        scheduled_at -> Nullable<Timestamptz>,
        due_at -> Nullable<Timestamptz>,
        completed_at -> Nullable<Timestamptz>,
        verified_at -> Nullable<Timestamptz>,
        closed_at -> Nullable<Timestamptz>,
        cancelled_at -> Nullable<Timestamptz>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    field_work_order_items (id) {
        id -> Uuid,
        work_order_id -> Uuid,
        title -> Text,
        position -> Int8,
        is_done -> Bool,
        done_by -> Nullable<Text>,
        done_at -> Nullable<Timestamptz>,
        notes -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    field_work_order_parts (id) {
        id -> Uuid,
        work_order_id -> Uuid,
        stock_id -> Nullable<Uuid>,
        part_name -> Text,
        quantity -> Int8,
        unit -> Text,
        status -> Text,
        consumed_at -> Nullable<Timestamptz>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    field_dispatch_assignments (id) {
        id -> Uuid,
        work_order_id -> Uuid,
        technician_id -> Text,
        sequence -> Int8,
        status -> Text,
        assigned_by -> Text,
        assigned_at -> Timestamptz,
        started_at -> Nullable<Timestamptz>,
        completed_at -> Nullable<Timestamptz>,
        released_at -> Nullable<Timestamptz>,
        notes -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    field_signoffs (id) {
        id -> Uuid,
        work_order_id -> Uuid,
        customer_name -> Text,
        signature_ref -> Text,
        signed_at -> Timestamptz,
        notes -> Nullable<Text>,
        survey_distribution_id -> Nullable<Uuid>,
        follow_up_status -> Text,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Webinars (migration 000081_webinars).
// ---------------------------------------------------------------------------

diesel::table! {
    webinars (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Nullable<Text>,
        host_id -> Varchar,
        co_host_ids -> Array<Varchar>,
        panelist_ids -> Array<Varchar>,
        status -> Varchar,
        access_model -> Varchar,
        product_id -> Nullable<Varchar>,
        capacity -> Int4,
        requires_approval -> Bool,
        allow_waitlist -> Bool,
        chat_enabled -> Bool,
        qa_enabled -> Bool,
        polls_enabled -> Bool,
        recording_enabled -> Bool,
        meeting_room_id -> Nullable<Uuid>,
        meeting_link_status -> Varchar,
        chat_channel_id -> Nullable<Varchar>,
        survey_id -> Nullable<Varchar>,
        survey_distribution_id -> Nullable<Varchar>,
        starts_at -> Nullable<Timestamptz>,
        ends_at -> Nullable<Timestamptz>,
        timezone -> Varchar,
        recurrence_rule -> Nullable<Varchar>,
        scheduler_job_id -> Nullable<Varchar>,
        scheduler_status -> Varchar,
        reminder_scheduler_status -> Varchar,
        started_at -> Nullable<Timestamptz>,
        ended_at -> Nullable<Timestamptz>,
        follow_up_sent_at -> Nullable<Timestamptz>,
        follow_up_count -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    webinar_registrations (id) {
        id -> Uuid,
        webinar_id -> Uuid,
        user_id -> Varchar,
        status -> Varchar,
        payment_status -> Varchar,
        registered_at -> Timestamptz,
        approved_at -> Nullable<Timestamptz>,
        cancelled_at -> Nullable<Timestamptz>,
        promoted_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    webinar_polls (id) {
        id -> Uuid,
        webinar_id -> Uuid,
        question -> Varchar,
        options -> Array<Text>,
        allow_multiple -> Bool,
        status -> Varchar,
        created_by -> Varchar,
        created_at -> Timestamptz,
        closed_at -> Nullable<Timestamptz>,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    webinar_poll_votes (id) {
        id -> Uuid,
        poll_id -> Uuid,
        webinar_id -> Uuid,
        user_id -> Varchar,
        option_indexes -> Array<Int4>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    webinar_questions (id) {
        id -> Uuid,
        webinar_id -> Uuid,
        author_id -> Varchar,
        body -> Text,
        upvoted_by -> Array<Varchar>,
        answered -> Bool,
        answered_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    webinar_attendance (id) {
        id -> Uuid,
        webinar_id -> Uuid,
        user_id -> Varchar,
        joined_at -> Timestamptz,
        left_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    webinar_recordings (id) {
        id -> Uuid,
        webinar_id -> Uuid,
        meeting_recording_id -> Nullable<Uuid>,
        title -> Nullable<Varchar>,
        status -> Varchar,
        storage_target -> Nullable<Varchar>,
        storage_key -> Nullable<Varchar>,
        storage_url -> Nullable<Varchar>,
        size_bytes -> Nullable<Int8>,
        duration_secs -> Nullable<Int8>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(webinar_registrations -> webinars (webinar_id));
diesel::joinable!(webinar_polls -> webinars (webinar_id));
diesel::joinable!(webinar_poll_votes -> webinar_polls (poll_id));
diesel::joinable!(webinar_poll_votes -> webinars (webinar_id));
diesel::joinable!(webinar_questions -> webinars (webinar_id));
diesel::joinable!(webinar_attendance -> webinars (webinar_id));
diesel::joinable!(webinar_recordings -> webinars (webinar_id));

// ---------------------------------------------------------------------------
// Cluster sync reliability (migration 000082_cluster_sync_reliability).
// ---------------------------------------------------------------------------

diesel::table! {
    cluster_sync_conflicts (id) {
        id -> Uuid,
        log_id -> Nullable<Uuid>,
        plan_id -> Nullable<Uuid>,
        cluster_id -> Uuid,
        node_id -> Nullable<Uuid>,
        domain -> Varchar,
        key -> Varchar,
        local_updated_at -> Nullable<Timestamptz>,
        remote_updated_at -> Nullable<Timestamptz>,
        winner -> Varchar,
        reason -> Text,
        local_payload -> Nullable<Jsonb>,
        remote_payload -> Nullable<Jsonb>,
        status -> Varchar,
        resolution -> Nullable<Varchar>,
        resolved_by -> Nullable<Varchar>,
        resolved_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    cluster_sync_retries (id) {
        id -> Uuid,
        plan_id -> Uuid,
        cluster_id -> Uuid,
        node_id -> Nullable<Uuid>,
        domain -> Varchar,
        direction -> Varchar,
        attempt -> Int4,
        max_attempts -> Int4,
        next_retry_at -> Timestamptz,
        last_error -> Nullable<Text>,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    cluster_sync_idempotency_keys (scope, key) {
        scope -> Varchar,
        key -> Varchar,
        status -> Int4,
        body -> Jsonb,
        created_at -> Timestamptz,
        expires_at -> Timestamptz,
    }
}

diesel::table! {
    cluster_sync_settings (key) {
        key -> Varchar,
        value -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    cluster_sync_domains (domain) {
        domain -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(cluster_sync_conflicts -> cluster_sync_logs (log_id));
diesel::joinable!(cluster_sync_conflicts -> cluster_sync_plans (plan_id));
diesel::joinable!(cluster_sync_conflicts -> clusters (cluster_id));
diesel::joinable!(cluster_sync_conflicts -> cluster_nodes (node_id));
diesel::joinable!(cluster_sync_retries -> cluster_sync_plans (plan_id));
diesel::joinable!(cluster_sync_retries -> clusters (cluster_id));
diesel::joinable!(cluster_sync_retries -> cluster_nodes (node_id));

// ---------------------------------------------------------------------------
// Import / export hardening (migration 000083_import_export_hardening).
// The `import_jobs` / `export_jobs` ALTERs from the same migration
// are applied to the existing `table!` blocks above.
// ---------------------------------------------------------------------------

diesel::table! {
    mapping_templates (id) {
        id -> Uuid,
        name -> Varchar,
        entity -> Varchar,
        mapping -> Jsonb,
        actor -> Varchar,
        company_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    export_schedules (id) {
        id -> Uuid,
        name -> Varchar,
        entity -> Varchar,
        format -> Varchar,
        fields -> Jsonb,
        filters -> Jsonb,
        redact_pii -> Bool,
        redact_names -> Bool,
        pii_allowed -> Bool,
        schedule -> Varchar,
        enabled -> Bool,
        company_id -> Nullable<Uuid>,
        actor -> Varchar,
        last_run_at -> Nullable<Timestamptz>,
        last_export_id -> Nullable<Uuid>,
        last_error -> Nullable<Text>,
        run_count -> Int8,
        scheduling -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    import_sessions (id) {
        id -> Uuid,
        entity -> Varchar,
        format -> Varchar,
        status -> Varchar,
        total_chunks -> Nullable<Int4>,
        received_chunks -> Jsonb,
        total_rows -> Int8,
        valid_rows -> Int8,
        invalid_rows -> Int8,
        row_errors -> Jsonb,
        mapping -> Jsonb,
        actor -> Varchar,
        company_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Marketing automation (migration 000085_marketing).
// No FKs by design (see migration notes): no joinable!s.
// ---------------------------------------------------------------------------

diesel::table! {
    marketing_campaigns (id) {
        id -> Uuid,
        name -> Varchar,
        subject -> Varchar,
        body_text -> Nullable<Text>,
        body_html -> Nullable<Text>,
        template_id -> Nullable<Text>,
        segment_id -> Nullable<Uuid>,
        newsletter_list_id -> Nullable<Text>,
        status -> Varchar,
        scheduled_at -> Nullable<Timestamptz>,
        sent_at -> Nullable<Timestamptz>,
        ab_enabled -> Bool,
        ab_winner_metric -> Varchar,
        ab_min_sample -> Nullable<Int8>,
        winning_variant_id -> Nullable<Uuid>,
        stats_queued -> Int8,
        stats_sent -> Int8,
        stats_delivered -> Int8,
        stats_failed -> Int8,
        stats_skipped -> Int8,
        stats_opened -> Int8,
        stats_clicked -> Int8,
        stats_unsubscribed -> Int8,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    marketing_campaign_variants (id) {
        id -> Uuid,
        campaign_id -> Uuid,
        name -> Varchar,
        subject -> Varchar,
        body_text -> Nullable<Text>,
        body_html -> Nullable<Text>,
        weight_percent -> Int2,
        sent -> Int8,
        opened -> Int8,
        clicked -> Int8,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    marketing_segments (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Text,
        rule_tree -> Jsonb,
        status -> Varchar,
        last_evaluated_at -> Nullable<Timestamptz>,
        last_size -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    marketing_segment_size_history (id) {
        id -> Uuid,
        segment_id -> Uuid,
        size -> Int8,
        evaluated_contacts -> Int8,
        recorded_at -> Timestamptz,
    }
}

diesel::table! {
    marketing_sequences (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Text,
        trigger_kind -> Varchar,
        trigger_form_id -> Nullable<Text>,
        trigger_tag -> Nullable<Text>,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    marketing_sequence_steps (id) {
        id -> Uuid,
        sequence_id -> Uuid,
        step_order -> Int8,
        delay_seconds -> Int8,
        condition -> Jsonb,
        subject -> Varchar,
        body_text -> Nullable<Text>,
        body_html -> Nullable<Text>,
        template_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    marketing_enrollments (id) {
        id -> Uuid,
        sequence_id -> Uuid,
        contact_id -> Text,
        email -> Varchar,
        next_step -> Int8,
        status -> Varchar,
        next_step_at -> Nullable<Timestamptz>,
        history -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    marketing_scores (contact_id) {
        contact_id -> Text,
        email -> Varchar,
        score -> Int8,
        warm -> Bool,
        hot -> Bool,
        hot_notified_at -> Nullable<Timestamptz>,
        last_action_at -> Nullable<Timestamptz>,
        events -> Jsonb,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    marketing_score_rules (id) {
        id -> Uuid,
        action -> Varchar,
        points -> Int8,
        decay_days -> Nullable<Int8>,
        active -> Bool,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    marketing_sends (id) {
        id -> Uuid,
        campaign_id -> Nullable<Uuid>,
        sequence_id -> Nullable<Uuid>,
        step_id -> Nullable<Uuid>,
        enrollment_id -> Nullable<Uuid>,
        contact_id -> Text,
        email -> Varchar,
        variant_id -> Nullable<Uuid>,
        status -> Varchar,
        mail_job_id -> Nullable<Text>,
        skip_reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Manufacturing / MRP (migration 000086_manufacturing).
// ---------------------------------------------------------------------------

diesel::table! {
    mfg_boms (id) {
        id -> Uuid,
        product_id -> Varchar,
        name -> Varchar,
        code -> Varchar,
        description -> Nullable<Text>,
        status -> Varchar,
        active_version_id -> Nullable<Uuid>,
        latest_version -> Int4,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    mfg_bom_versions (id) {
        id -> Uuid,
        bom_id -> Uuid,
        version -> Int4,
        created_by -> Varchar,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    mfg_bom_version_lines (id) {
        id -> Uuid,
        bom_version_id -> Uuid,
        component_product_id -> Varchar,
        quantity -> Numeric,
        unit -> Varchar,
        scrap_allowance_percent -> Numeric,
        notes -> Nullable<Text>,
    }
}

diesel::table! {
    mfg_work_centers (id) {
        id -> Uuid,
        name -> Varchar,
        code -> Varchar,
        capacity_hours_per_day -> Numeric,
        cost_per_hour -> Nullable<Numeric>,
        currency -> Nullable<Bpchar>,
        status -> Varchar,
        description -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    mfg_bom_routing_steps (id) {
        id -> Uuid,
        bom_version_id -> Uuid,
        sequence -> Int4,
        name -> Varchar,
        work_center_id -> Uuid,
        duration_minutes -> Numeric,
        description -> Nullable<Text>,
    }
}

diesel::table! {
    mfg_work_orders (id) {
        id -> Uuid,
        number -> Varchar,
        bom_id -> Uuid,
        bom_version_id -> Uuid,
        bom_version -> Int4,
        product_id -> Varchar,
        quantity_planned -> Numeric,
        quantity_produced -> Numeric,
        quantity_scrapped -> Numeric,
        status -> Varchar,
        planned_start -> Nullable<Timestamptz>,
        planned_end -> Nullable<Timestamptz>,
        actual_start -> Nullable<Timestamptz>,
        actual_end -> Nullable<Timestamptz>,
        company_id -> Nullable<Varchar>,
        notes -> Nullable<Text>,
        inspection_status -> Nullable<Varchar>,
        inspection_notes -> Nullable<Text>,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    mfg_work_order_requirements (id) {
        id -> Uuid,
        work_order_id -> Uuid,
        component_product_id -> Varchar,
        quantity_required -> Numeric,
        unit -> Varchar,
        quantity_reserved -> Numeric,
        quantity_consumed -> Numeric,
        quantity_short -> Numeric,
    }
}

diesel::table! {
    mfg_scrap_reasons (id) {
        id -> Uuid,
        code -> Varchar,
        label -> Varchar,
        active -> Bool,
        description -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    mfg_units (id) {
        id -> Uuid,
        code -> Varchar,
        label -> Varchar,
        active -> Bool,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    mfg_production_runs (id) {
        id -> Uuid,
        work_order_id -> Uuid,
        quantity_produced -> Numeric,
        quantity_scrapped -> Numeric,
        scrap_reason_id -> Nullable<Uuid>,
        work_center_id -> Nullable<Uuid>,
        operator_id -> Varchar,
        status -> Varchar,
        notes -> Nullable<Text>,
        started_at -> Timestamptz,
        completed_at -> Timestamptz,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    mfg_reservations (id) {
        id -> Uuid,
        work_order_id -> Uuid,
        component_product_id -> Varchar,
        quantity_reserved -> Numeric,
        quantity_consumed -> Numeric,
        unit -> Varchar,
        status -> Varchar,
        stock_unknown -> Bool,
        confirmation_reference -> Nullable<Varchar>,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    mfg_stock_notices (id) {
        id -> Uuid,
        kind -> Varchar,
        work_order_id -> Nullable<Uuid>,
        reservation_id -> Nullable<Uuid>,
        component_product_id -> Nullable<Varchar>,
        quantity -> Nullable<Numeric>,
        message -> Nullable<Text>,
        received_at -> Timestamptz,
    }
}

diesel::joinable!(mfg_bom_versions -> mfg_boms (bom_id));
diesel::joinable!(mfg_bom_version_lines -> mfg_bom_versions (bom_version_id));
diesel::joinable!(mfg_bom_routing_steps -> mfg_bom_versions (bom_version_id));
diesel::joinable!(mfg_bom_routing_steps -> mfg_work_centers (work_center_id));
diesel::joinable!(mfg_work_orders -> mfg_boms (bom_id));
diesel::joinable!(mfg_work_orders -> mfg_bom_versions (bom_version_id));
diesel::joinable!(mfg_work_order_requirements -> mfg_work_orders (work_order_id));
diesel::joinable!(mfg_production_runs -> mfg_work_orders (work_order_id));
diesel::joinable!(mfg_production_runs -> mfg_scrap_reasons (scrap_reason_id));
diesel::joinable!(mfg_production_runs -> mfg_work_centers (work_center_id));
diesel::joinable!(mfg_reservations -> mfg_work_orders (work_order_id));
diesel::joinable!(mfg_stock_notices -> mfg_work_orders (work_order_id));
diesel::joinable!(mfg_stock_notices -> mfg_reservations (reservation_id));

// ---------------------------------------------------------------------------
// Quality management (migration 000087_quality).
// ---------------------------------------------------------------------------

diesel::table! {
    quality_points (id) {
        id -> Uuid,
        name -> Text,
        description -> Text,
        scope -> Text,
        target_ref -> Nullable<Text>,
        target_kind -> Nullable<Text>,
        check_kind -> Text,
        on_fail_policy -> Text,
        alert_severity -> Text,
        active -> Bool,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    quality_criteria (id) {
        id -> Uuid,
        point_id -> Uuid,
        name -> Text,
        kind -> Text,
        unit -> Nullable<Text>,
        min_value -> Nullable<Float8>,
        max_value -> Nullable<Float8>,
        expected_bool -> Nullable<Bool>,
        expected_text -> Nullable<Text>,
        failure_cause -> Nullable<Text>,
        weight -> Float8,
        required -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    quality_inspections (id) {
        id -> Uuid,
        point_id -> Uuid,
        target_ref -> Text,
        target_kind -> Text,
        inspector_id -> Nullable<Text>,
        status -> Text,
        result -> Nullable<Text>,
        measurements -> Jsonb,
        failure_causes -> Array<Text>,
        notes -> Nullable<Text>,
        evidence_refs -> Array<Text>,
        inspected_at -> Nullable<Timestamptz>,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    quality_alerts (id) {
        id -> Uuid,
        inspection_id -> Uuid,
        point_id -> Uuid,
        target_ref -> Text,
        target_kind -> Text,
        severity -> Text,
        status -> Text,
        message -> Text,
        blocked -> Bool,
        acknowledged_by -> Nullable<Text>,
        resolved_by -> Nullable<Text>,
        resolved_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    quality_capas (id) {
        id -> Uuid,
        inspection_id -> Uuid,
        alert_id -> Nullable<Uuid>,
        title -> Text,
        description -> Text,
        root_cause -> Nullable<Text>,
        owner_id -> Text,
        due_date -> Nullable<Timestamptz>,
        state -> Text,
        verification_notes -> Nullable<Text>,
        verified_by -> Nullable<Text>,
        verified_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(quality_criteria -> quality_points (point_id));
diesel::joinable!(quality_inspections -> quality_points (point_id));
diesel::joinable!(quality_alerts -> quality_inspections (inspection_id));
diesel::joinable!(quality_alerts -> quality_points (point_id));
diesel::joinable!(quality_capas -> quality_inspections (inspection_id));
diesel::joinable!(quality_capas -> quality_alerts (alert_id));

// ---------------------------------------------------------------------------
// Expenses (migration 000088_expenses). Money is stored in minor
// units (BIGINT), so no NUMERIC columns and serde derives apply.
// ---------------------------------------------------------------------------

diesel::table! {
    expense_categories (id) {
        id -> Uuid,
        name -> Text,
        code -> Text,
        company_id -> Nullable<Text>,
        receipt_required -> Bool,
        receipt_required_above_minor -> Nullable<Int8>,
        per_expense_limit_minor -> Nullable<Int8>,
        per_day_limit_minor -> Nullable<Int8>,
        active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    expense_policies (id) {
        id -> Uuid,
        name -> Text,
        company_id -> Nullable<Text>,
        currency -> Text,
        mileage_rate_minor -> Nullable<Int8>,
        mileage_unit -> Text,
        per_diem_rate_minor -> Nullable<Int8>,
        max_report_total_minor -> Nullable<Int8>,
        approval_chain_id -> Nullable<Text>,
        active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    expense_reports (id) {
        id -> Uuid,
        employee_id -> Text,
        company_id -> Nullable<Text>,
        policy_id -> Nullable<Uuid>,
        title -> Text,
        notes -> Nullable<Text>,
        status -> Text,
        currency -> Text,
        total_minor -> Int8,
        violations -> Array<Text>,
        approval_request_id -> Nullable<Text>,
        submitted_at -> Nullable<Timestamptz>,
        decided_at -> Nullable<Timestamptz>,
        decided_by -> Nullable<Text>,
        decision_reason -> Nullable<Text>,
        reimbursement_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    expenses (id) {
        id -> Uuid,
        report_id -> Uuid,
        kind -> Text,
        category_id -> Uuid,
        expense_date -> Date,
        amount_minor -> Int8,
        currency -> Text,
        description -> Text,
        project_id -> Nullable<Text>,
        client_id -> Nullable<Text>,
        distance -> Nullable<Float8>,
        days -> Nullable<Int8>,
        rate_minor_applied -> Nullable<Int8>,
        violations -> Array<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    expense_receipts (id) {
        id -> Uuid,
        expense_id -> Uuid,
        storage_target -> Text,
        storage_key -> Text,
        file_name -> Text,
        content_type -> Nullable<Text>,
        uploaded_by -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    expense_reimbursements (id) {
        id -> Uuid,
        report_id -> Uuid,
        employee_id -> Text,
        amount_minor -> Int8,
        currency -> Text,
        status -> Text,
        payable_reference -> Text,
        failure_reason -> Nullable<Text>,
        paid_at -> Nullable<Timestamptz>,
        attempts -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    expense_adjustments (id) {
        id -> Uuid,
        report_id -> Uuid,
        kind -> Text,
        amount_minor -> Int8,
        currency -> Text,
        reason -> Text,
        created_by -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    expense_currency_rates (id) {
        id -> Uuid,
        base_currency -> Text,
        target_currency -> Text,
        rate -> Float8,
        source -> Text,
        created_at -> Timestamptz,
    }
}

diesel::joinable!(expense_reports -> expense_policies (policy_id));
diesel::joinable!(expenses -> expense_reports (report_id));
diesel::joinable!(expenses -> expense_categories (category_id));
diesel::joinable!(expense_receipts -> expenses (expense_id));
diesel::joinable!(expense_reimbursements -> expense_reports (report_id));
diesel::joinable!(expense_adjustments -> expense_reports (report_id));

// ---------------------------------------------------------------------------
// Approvals (migration 000089_approvals). `module` is a Rust keyword,
// so the struct field is `module_` (Diesel strips the underscore).
// ---------------------------------------------------------------------------

diesel::table! {
    approval_definitions (id) {
        id -> Uuid,
        module -> Varchar,
        record_type -> Varchar,
        name -> Varchar,
        description -> Nullable<Text>,
        is_active -> Bool,
        timeout_hours -> Nullable<Int4>,
        escalation_action -> Varchar,
        escalate_to -> Nullable<Jsonb>,
        escalation_extension_hours -> Nullable<Int4>,
        company_id -> Nullable<Varchar>,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    approval_steps (id) {
        id -> Uuid,
        definition_id -> Uuid,
        position -> Int4,
        name -> Varchar,
        mode -> Varchar,
        approvers -> Jsonb,
        is_active -> Bool,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    approval_requests (id) {
        id -> Uuid,
        definition_id -> Uuid,
        module -> Varchar,
        record_type -> Varchar,
        record_id -> Varchar,
        requester_id -> Varchar,
        summary -> Varchar,
        amount_minor -> Nullable<Int8>,
        currency -> Nullable<Varchar>,
        status -> Varchar,
        current_step_index -> Int4,
        round -> Int4,
        escalation_count -> Int4,
        last_escalated_at -> Nullable<Timestamptz>,
        expires_at -> Timestamptz,
        decided_at -> Nullable<Timestamptz>,
        decided_by -> Nullable<Varchar>,
        company_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    approval_decisions (id) {
        id -> Uuid,
        request_id -> Uuid,
        step_index -> Int4,
        round -> Int4,
        approver_id -> Varchar,
        acting_for -> Nullable<Varchar>,
        slot_kind -> Varchar,
        slot_ref -> Varchar,
        decision -> Varchar,
        comment -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    approval_delegations (id) {
        id -> Uuid,
        delegator_id -> Varchar,
        delegate_id -> Varchar,
        starts_at -> Timestamptz,
        ends_at -> Timestamptz,
        is_active -> Bool,
        created_by -> Varchar,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    approval_escalations (id) {
        id -> Uuid,
        request_id -> Uuid,
        step_index -> Int4,
        reason -> Varchar,
        action -> Varchar,
        created_at -> Timestamptz,
    }
}

diesel::joinable!(approval_steps -> approval_definitions (definition_id));
diesel::joinable!(approval_requests -> approval_definitions (definition_id));
diesel::joinable!(approval_decisions -> approval_requests (request_id));
diesel::joinable!(approval_escalations -> approval_requests (request_id));

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
        columns -> Jsonb,
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

diesel::joinable!(studio_system_fields -> studio_record_types (record_type));
diesel::joinable!(studio_field_definitions -> studio_record_types (record_type));
diesel::joinable!(studio_field_values -> studio_field_definitions (definition_id));

// ---------------------------------------------------------------------------
// Printing (migration 000091_printing).
// ---------------------------------------------------------------------------

diesel::table! {
    paper_sizes (code) {
        code -> Varchar,
        name -> Varchar,
        width_mm -> Numeric,
        height_mm -> Nullable<Numeric>,
        margin_top_mm -> Numeric,
        margin_bottom_mm -> Numeric,
        margin_left_mm -> Numeric,
        margin_right_mm -> Numeric,
        is_standard -> Bool,
        is_thermal -> Bool,
        thermal_width_mm -> Nullable<Int4>,
        company_id -> Nullable<Varchar>,
        created_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    invoice_templates (id) {
        id -> Uuid,
        company_id -> Nullable<Varchar>,
        name -> Varchar,
        is_default -> Bool,
        paper_size_code -> Nullable<Varchar>,
        header_text -> Nullable<Text>,
        footer_text -> Nullable<Text>,
        logo_storage_target -> Nullable<Varchar>,
        logo_storage_key -> Nullable<Varchar>,
        logo_url -> Nullable<Varchar>,
        primary_color -> Varchar,
        accent_color -> Varchar,
        font_family -> Varchar,
        columns -> Jsonb,
        line_item_layout -> Jsonb,
        show_tax_breakdown -> Bool,
        payment_instructions -> Nullable<Text>,
        footer_notes -> Nullable<Text>,
        version -> Int4,
        created_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    invoice_template_versions (id) {
        id -> Uuid,
        template_id -> Uuid,
        version -> Int4,
        snapshot -> Jsonb,
        created_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    printer_profiles (id) {
        id -> Uuid,
        company_id -> Nullable<Varchar>,
        name -> Varchar,
        connection_type -> Varchar,
        connection_target -> Nullable<Varchar>,
        auth_token_ref -> Nullable<Varchar>,
        width_columns -> Int4,
        character_set -> Varchar,
        code_page -> Varchar,
        supports_drawer_kick -> Bool,
        supports_cut -> Bool,
        paper_size_code -> Nullable<Varchar>,
        is_default -> Bool,
        created_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    print_jobs (id) {
        id -> Uuid,
        company_id -> Nullable<Varchar>,
        invoice_id -> Varchar,
        template_id -> Nullable<Uuid>,
        template_version -> Nullable<Int4>,
        paper_size_code -> Nullable<Varchar>,
        output_kind -> Varchar,
        printer_profile_id -> Nullable<Uuid>,
        status -> Varchar,
        document_ref -> Nullable<Varchar>,
        escpos_preview -> Nullable<Text>,
        error_message -> Nullable<Text>,
        attempts -> Int4,
        requested_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        started_at -> Nullable<Timestamptz>,
        completed_at -> Nullable<Timestamptz>,
    }
}

diesel::joinable!(invoice_templates -> paper_sizes (paper_size_code));
diesel::joinable!(invoice_template_versions -> invoice_templates (template_id));
diesel::joinable!(printer_profiles -> paper_sizes (paper_size_code));
diesel::joinable!(print_jobs -> invoice_templates (template_id));
diesel::joinable!(print_jobs -> paper_sizes (paper_size_code));
diesel::joinable!(print_jobs -> printer_profiles (printer_profile_id));

// ---------------------------------------------------------------------------
// Curriculum (migration 000092_curriculum).
// ---------------------------------------------------------------------------

diesel::table! {
    curriculum_frameworks (id) {
        id -> Uuid,
        code -> Varchar,
        name -> Varchar,
        description -> Text,
        country -> Varchar,
        is_active -> Bool,
        is_built_in -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_levels (id) {
        id -> Uuid,
        framework_id -> Uuid,
        code -> Varchar,
        name -> Varchar,
        grade_count -> Int4,
        sort_order -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_subjects (id) {
        id -> Uuid,
        framework_id -> Uuid,
        level_id -> Uuid,
        code -> Varchar,
        name -> Varchar,
        sort_order -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_core_competencies (id) {
        id -> Uuid,
        framework_id -> Uuid,
        subject_id -> Uuid,
        level_id -> Uuid,
        code -> Varchar,
        title -> Varchar,
        description -> Text,
        grade_year -> Int4,
        version -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_basic_competencies (id) {
        id -> Uuid,
        framework_id -> Uuid,
        core_competency_id -> Uuid,
        code -> Varchar,
        title -> Varchar,
        description -> Text,
        grade_year -> Int4,
        version -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_structures (id) {
        id -> Uuid,
        framework_id -> Uuid,
        level_id -> Uuid,
        grade_year -> Int4,
        subject_id -> Uuid,
        hours_per_week -> Numeric,
        sort_order -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_assessment_domains (id) {
        id -> Uuid,
        code -> Varchar,
        name -> Varchar,
        parent_domain_id -> Nullable<Uuid>,
        description -> Text,
        sort_order -> Int4,
        is_active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_grade_descriptors (id) {
        id -> Uuid,
        framework_id -> Uuid,
        domain_id -> Nullable<Uuid>,
        min_score -> Numeric,
        max_score -> Numeric,
        grade_label -> Varchar,
        descriptor -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_lesson_plans (id) {
        id -> Uuid,
        framework_id -> Uuid,
        subject_id -> Uuid,
        core_competency_id -> Nullable<Uuid>,
        basic_competency_id -> Nullable<Uuid>,
        title -> Varchar,
        objectives -> Array<Text>,
        materials -> Array<Text>,
        activities -> Array<Text>,
        assessment_notes -> Text,
        time_minutes -> Int4,
        grade_year -> Int4,
        semester -> Int4,
        teacher_id -> Varchar,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_report_cards (id) {
        id -> Uuid,
        student_ref -> Varchar,
        framework_id -> Uuid,
        level_id -> Uuid,
        grade_year -> Int4,
        semester -> Int4,
        academic_year -> Varchar,
        status -> Varchar,
        issued_at -> Nullable<Timestamptz>,
        issued_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_report_card_entries (id) {
        id -> Uuid,
        report_card_id -> Uuid,
        subject_id -> Uuid,
        domain_id -> Uuid,
        score -> Nullable<Numeric>,
        grade_label -> Nullable<Varchar>,
        descriptor -> Nullable<Text>,
        notes -> Text,
        academic_grade_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_interventions (id) {
        id -> Uuid,
        student_ref -> Varchar,
        basic_competency_id -> Uuid,
        kind -> Varchar,
        status -> Varchar,
        teacher_id -> Varchar,
        scheduled_at -> Nullable<Timestamptz>,
        completed_at -> Nullable<Timestamptz>,
        notes -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_settings (key) {
        key -> Text,
        value -> Jsonb,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(curriculum_levels -> curriculum_frameworks (framework_id));
diesel::joinable!(curriculum_subjects -> curriculum_frameworks (framework_id));
diesel::joinable!(curriculum_subjects -> curriculum_levels (level_id));
diesel::joinable!(curriculum_core_competencies -> curriculum_frameworks (framework_id));
diesel::joinable!(curriculum_core_competencies -> curriculum_subjects (subject_id));
diesel::joinable!(curriculum_core_competencies -> curriculum_levels (level_id));
diesel::joinable!(curriculum_basic_competencies -> curriculum_frameworks (framework_id));
diesel::joinable!(curriculum_basic_competencies -> curriculum_core_competencies (core_competency_id));
diesel::joinable!(curriculum_structures -> curriculum_frameworks (framework_id));
diesel::joinable!(curriculum_structures -> curriculum_levels (level_id));
diesel::joinable!(curriculum_structures -> curriculum_subjects (subject_id));
diesel::joinable!(curriculum_grade_descriptors -> curriculum_frameworks (framework_id));
diesel::joinable!(curriculum_grade_descriptors -> curriculum_assessment_domains (domain_id));
diesel::joinable!(curriculum_lesson_plans -> curriculum_frameworks (framework_id));
diesel::joinable!(curriculum_lesson_plans -> curriculum_subjects (subject_id));
diesel::joinable!(curriculum_lesson_plans -> curriculum_core_competencies (core_competency_id));
diesel::joinable!(curriculum_lesson_plans -> curriculum_basic_competencies (basic_competency_id));
diesel::joinable!(curriculum_report_cards -> curriculum_frameworks (framework_id));
diesel::joinable!(curriculum_report_cards -> curriculum_levels (level_id));
diesel::joinable!(curriculum_report_card_entries -> curriculum_report_cards (report_card_id));
diesel::joinable!(curriculum_report_card_entries -> curriculum_subjects (subject_id));
diesel::joinable!(curriculum_report_card_entries -> curriculum_assessment_domains (domain_id));
diesel::joinable!(curriculum_interventions -> curriculum_basic_competencies (basic_competency_id));

// ---------------------------------------------------------------------------
// Admissions (migration 000093_admissions).
// ---------------------------------------------------------------------------

diesel::table! {
    admission_periods (id) {
        id -> Uuid,
        academic_year -> Varchar,
        term -> Varchar,
        name -> Varchar,
        open_date -> Date,
        close_date -> Date,
        status -> Varchar,
        registration_fee_amount -> Nullable<Numeric>,
        fee_currency -> Varchar,
        registration_form_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    admission_period_quotas (id) {
        id -> Uuid,
        period_id -> Uuid,
        program_id -> Varchar,
        level -> Varchar,
        seats -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    admission_stage_definitions (id) {
        id -> Uuid,
        period_id -> Uuid,
        stage_key -> Varchar,
        name -> Varchar,
        position -> Int4,
        weight -> Numeric,
        pass_score -> Nullable<Numeric>,
        required -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    admission_required_documents (id) {
        id -> Uuid,
        period_id -> Uuid,
        document_key -> Varchar,
        title -> Varchar,
        description -> Nullable<Varchar>,
        required -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    admission_applications (id) {
        id -> Uuid,
        period_id -> Uuid,
        applicant_user_id -> Varchar,
        full_name -> Varchar,
        email -> Varchar,
        phone -> Nullable<Varchar>,
        program_id -> Varchar,
        level -> Varchar,
        form_submission_id -> Nullable<Varchar>,
        status -> Varchar,
        total_score -> Nullable<Numeric>,
        fee_amount -> Nullable<Numeric>,
        fee_currency -> Varchar,
        fee_invoice_status -> Varchar,
        fee_invoice_id -> Nullable<Varchar>,
        student_id -> Nullable<Varchar>,
        source -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    admission_application_scores (id) {
        id -> Uuid,
        application_id -> Uuid,
        period_id -> Uuid,
        stage_id -> Uuid,
        stage_key -> Varchar,
        score -> Numeric,
        scored_by -> Varchar,
        note -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    admission_checklist_items (id) {
        id -> Uuid,
        application_id -> Uuid,
        period_id -> Uuid,
        required_document_id -> Uuid,
        document_key -> Varchar,
        title -> Varchar,
        required -> Bool,
        status -> Varchar,
        document_id -> Nullable<Varchar>,
        verified_by -> Nullable<Varchar>,
        verified_at -> Nullable<Timestamptz>,
        rejection_reason -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    admission_waitlist_entries (id) {
        id -> Uuid,
        period_id -> Uuid,
        application_id -> Uuid,
        position -> Int4,
        status -> Varchar,
        promoted_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    admission_results (id) {
        id -> Uuid,
        period_id -> Uuid,
        application_id -> Uuid,
        decision -> Varchar,
        published -> Bool,
        published_at -> Nullable<Timestamptz>,
        decided_by -> Varchar,
        note -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    admission_settings (key) {
        key -> Varchar,
        value -> Varchar,
        description -> Varchar,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(admission_period_quotas -> admission_periods (period_id));
diesel::joinable!(admission_stage_definitions -> admission_periods (period_id));
diesel::joinable!(admission_required_documents -> admission_periods (period_id));
diesel::joinable!(admission_applications -> admission_periods (period_id));
diesel::joinable!(admission_application_scores -> admission_applications (application_id));
diesel::joinable!(admission_application_scores -> admission_periods (period_id));
diesel::joinable!(admission_application_scores -> admission_stage_definitions (stage_id));
diesel::joinable!(admission_checklist_items -> admission_applications (application_id));
diesel::joinable!(admission_checklist_items -> admission_periods (period_id));
diesel::joinable!(admission_checklist_items -> admission_required_documents (required_document_id));
diesel::joinable!(admission_waitlist_entries -> admission_periods (period_id));
diesel::joinable!(admission_waitlist_entries -> admission_applications (application_id));
diesel::joinable!(admission_results -> admission_periods (period_id));
diesel::joinable!(admission_results -> admission_applications (application_id));

// ---------------------------------------------------------------------------
// Academic system (migration 000094_academic).
// ---------------------------------------------------------------------------

diesel::table! {
    academic_programs (id) {
        id -> Uuid,
        code -> Text,
        name -> Text,
        department_id -> Nullable<Text>,
        max_credits_per_semester -> Nullable<Numeric>,
        status -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_students (id) {
        id -> Uuid,
        user_id -> Text,
        student_number -> Text,
        program_id -> Uuid,
        advisor_id -> Nullable<Uuid>,
        enrollment_year -> Int4,
        status -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_lecturers (id) {
        id -> Uuid,
        user_id -> Text,
        lecturer_number -> Text,
        department_id -> Nullable<Text>,
        title -> Nullable<Text>,
        status -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_subjects (id) {
        id -> Uuid,
        code -> Text,
        title -> Text,
        program_id -> Uuid,
        credits -> Numeric,
        semester_number -> Nullable<Int4>,
        elearning_course_id -> Nullable<Text>,
        elearning_course_verified -> Bool,
        status -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_rooms (id) {
        id -> Uuid,
        code -> Text,
        name -> Text,
        building -> Nullable<Text>,
        capacity -> Nullable<Int4>,
        status -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_semesters (id) {
        id -> Uuid,
        code -> Text,
        name -> Text,
        start_date -> Date,
        end_date -> Date,
        status -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_calendar_events (id) {
        id -> Uuid,
        semester_id -> Nullable<Uuid>,
        event_type -> Text,
        title -> Text,
        start_date -> Date,
        end_date -> Date,
        description -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_schedules (id) {
        id -> Uuid,
        subject_id -> Uuid,
        lecturer_id -> Uuid,
        room_id -> Uuid,
        semester_id -> Uuid,
        day_of_week -> Int4,
        start_time -> Text,
        end_time -> Text,
        status -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_enrollments (id) {
        id -> Uuid,
        student_id -> Uuid,
        subject_id -> Uuid,
        semester_id -> Uuid,
        credits -> Numeric,
        status -> Text,
        advisor_id -> Nullable<Uuid>,
        rejection_reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_attendance (id) {
        id -> Uuid,
        schedule_id -> Uuid,
        student_id -> Uuid,
        date -> Date,
        status -> Text,
        source -> Text,
        entry_id -> Nullable<Text>,
        minutes -> Nullable<Int8>,
        notes -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_grade_components (id) {
        id -> Uuid,
        subject_id -> Uuid,
        name -> Text,
        kind -> Text,
        weight_percent -> Numeric,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_grades (id) {
        id -> Uuid,
        enrollment_id -> Uuid,
        component_id -> Uuid,
        score -> Numeric,
        graded_by -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_grade_appeals (id) {
        id -> Uuid,
        enrollment_id -> Uuid,
        component_id -> Nullable<Uuid>,
        student_id -> Uuid,
        reason -> Text,
        status -> Text,
        reviewed_by -> Nullable<Uuid>,
        decision -> Nullable<Text>,
        corrected_score -> Nullable<Numeric>,
        resolution_notes -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_transcripts (id) {
        id -> Uuid,
        student_id -> Uuid,
        issued_by -> Text,
        gpa -> Numeric,
        total_credits -> Numeric,
        lines -> Jsonb,
        print_job_id -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    academic_theses (id) {
        id -> Uuid,
        student_id -> Uuid,
        title -> Text,
        advisor_id -> Uuid,
        co_advisor_id -> Nullable<Uuid>,
        status -> Text,
        proposal_date -> Nullable<Date>,
        defense_date -> Nullable<Date>,
        defense_result -> Nullable<Text>,
        notes -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_standing_records (id) {
        id -> Uuid,
        student_id -> Uuid,
        semester_id -> Uuid,
        gpa -> Numeric,
        total_credits -> Numeric,
        standing -> Text,
        reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_definitions (id) {
        id -> Uuid,
        kind -> Text,
        key -> Text,
        label -> Text,
        config -> Jsonb,
        active -> Bool,
        sort_order -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_settings (key) {
        key -> Text,
        value -> Jsonb,
        description -> Nullable<Text>,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(academic_students -> academic_programs (program_id));
diesel::joinable!(academic_students -> academic_lecturers (advisor_id));
diesel::joinable!(academic_subjects -> academic_programs (program_id));
diesel::joinable!(academic_calendar_events -> academic_semesters (semester_id));
diesel::joinable!(academic_schedules -> academic_subjects (subject_id));
diesel::joinable!(academic_schedules -> academic_lecturers (lecturer_id));
diesel::joinable!(academic_schedules -> academic_rooms (room_id));
diesel::joinable!(academic_schedules -> academic_semesters (semester_id));
diesel::joinable!(academic_enrollments -> academic_students (student_id));
diesel::joinable!(academic_enrollments -> academic_subjects (subject_id));
diesel::joinable!(academic_enrollments -> academic_semesters (semester_id));
diesel::joinable!(academic_enrollments -> academic_lecturers (advisor_id));
diesel::joinable!(academic_attendance -> academic_schedules (schedule_id));
diesel::joinable!(academic_attendance -> academic_students (student_id));
diesel::joinable!(academic_grade_components -> academic_subjects (subject_id));
diesel::joinable!(academic_grades -> academic_enrollments (enrollment_id));
diesel::joinable!(academic_grades -> academic_grade_components (component_id));
diesel::joinable!(academic_grades -> academic_lecturers (graded_by));
diesel::joinable!(academic_grade_appeals -> academic_enrollments (enrollment_id));
diesel::joinable!(academic_grade_appeals -> academic_grade_components (component_id));
diesel::joinable!(academic_grade_appeals -> academic_students (student_id));
diesel::joinable!(academic_grade_appeals -> academic_lecturers (reviewed_by));
diesel::joinable!(academic_transcripts -> academic_students (student_id));
diesel::joinable!(academic_theses -> academic_students (student_id));
diesel::joinable!(academic_theses -> academic_lecturers (advisor_id));
diesel::joinable!(academic_standing_records -> academic_students (student_id));
diesel::joinable!(academic_standing_records -> academic_semesters (semester_id));

// ---------------------------------------------------------------------------
// Online exams (migration 000095_exams).
// No FKs by design (see migration notes): no joinable!s.
// ---------------------------------------------------------------------------

diesel::table! {
    exam_question_banks (id) {
        id -> Uuid,
        title -> Text,
        description -> Text,
        category -> Text,
        owner_id -> Text,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    exam_questions (id) {
        id -> Uuid,
        bank_id -> Uuid,
        version -> Int4,
        previous_version_id -> Nullable<Uuid>,
        superseded_by -> Nullable<Uuid>,
        question_type -> Text,
        category -> Text,
        difficulty -> Text,
        text -> Text,
        options -> Jsonb,
        correct_answer -> Nullable<Jsonb>,
        answer_key -> Nullable<Text>,
        marks -> Float8,
        status -> Text,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    exam_question_versions (id) {
        id -> Uuid,
        question_id -> Uuid,
        version -> Int4,
        snapshot -> Jsonb,
        reason -> Nullable<Text>,
        created_by -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    exams (id) {
        id -> Uuid,
        title -> Text,
        description -> Text,
        category -> Text,
        bank_ids -> Array<Uuid>,
        course_id -> Nullable<Text>,
        academic_enrollment_id -> Nullable<Text>,
        academic_component_id -> Nullable<Text>,
        status -> Text,
        opens_at -> Nullable<Timestamptz>,
        closes_at -> Nullable<Timestamptz>,
        duration_minutes -> Int4,
        pass_mark -> Float8,
        attempts_allowed -> Int4,
        settings -> Jsonb,
        total_marks -> Float8,
        created_by -> Text,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    exam_question_links (id) {
        id -> Uuid,
        exam_id -> Uuid,
        question_id -> Uuid,
        question_version -> Int4,
        position -> Int4,
        section -> Nullable<Text>,
        marks -> Float8,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    exam_rubrics (id) {
        id -> Uuid,
        exam_id -> Uuid,
        title -> Text,
        description -> Text,
        criteria -> Jsonb,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    exam_attempts (id) {
        id -> Uuid,
        exam_id -> Uuid,
        user_id -> Text,
        attempt_number -> Int4,
        status -> Text,
        started_at -> Timestamptz,
        expires_at -> Timestamptz,
        duration_seconds -> Int8,
        seed -> Int8,
        question_order -> Jsonb,
        option_orders -> Jsonb,
        answers -> Jsonb,
        autosaved_at -> Nullable<Timestamptz>,
        submitted_at -> Nullable<Timestamptz>,
        score -> Nullable<Float8>,
        max_score -> Float8,
        passed -> Nullable<Bool>,
        graded_at -> Nullable<Timestamptz>,
        result_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    exam_grades (id) {
        id -> Uuid,
        attempt_id -> Uuid,
        exam_id -> Uuid,
        question_id -> Uuid,
        grader_id -> Nullable<Text>,
        rubric_id -> Nullable<Uuid>,
        criterion_scores -> Jsonb,
        marks_awarded -> Nullable<Float8>,
        max_marks -> Float8,
        status -> Text,
        comment -> Nullable<Text>,
        second_grader_id -> Nullable<Text>,
        audit -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    exam_proctor_signals (id) {
        id -> Uuid,
        attempt_id -> Uuid,
        exam_id -> Uuid,
        user_id -> Text,
        signal_type -> Text,
        detail -> Nullable<Text>,
        recorded_at -> Timestamptz,
    }
}

diesel::table! {
    exam_results (id) {
        id -> Uuid,
        attempt_id -> Uuid,
        exam_id -> Uuid,
        user_id -> Text,
        score -> Float8,
        max_score -> Float8,
        percentage -> Float8,
        passed -> Bool,
        status -> Text,
        per_question -> Jsonb,
        graded_at -> Timestamptz,
        published_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// School fees (migration 000096_school_fees). Ids are TEXT (not UUID)
// and all money amounts are NUMERIC, so these structs skip serde
// derives (PgNumeric has no serde implementation).
// ---------------------------------------------------------------------------

diesel::table! {
    school_fee_types (id) {
        id -> Text,
        code -> Text,
        name -> Text,
        description -> Nullable<Text>,
        category -> Text,
        billing_period -> Text,
        default_amount_minor -> Numeric,
        currency -> Bpchar,
        auto_assign_on_enrolment -> Bool,
        active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    school_fee_schedules (id) {
        id -> Text,
        student_user_id -> Text,
        program_code -> Text,
        level_code -> Text,
        academic_period_id -> Text,
        fee_type_id -> Text,
        amount_minor -> Numeric,
        currency -> Bpchar,
        due_date -> Timestamptz,
        status -> Text,
        notes -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    school_fee_charges (id) {
        id -> Text,
        schedule_id -> Text,
        student_user_id -> Text,
        fee_type_id -> Text,
        academic_period_id -> Text,
        period_label -> Text,
        amount_minor -> Numeric,
        discount_minor -> Numeric,
        total_minor -> Numeric,
        amount_paid_minor -> Numeric,
        currency -> Bpchar,
        invoice_id -> Nullable<Text>,
        discount_ids -> Jsonb,
        due_date -> Timestamptz,
        status -> Text,
        paid_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    school_fee_discounts (id) {
        id -> Text,
        student_user_id -> Text,
        fee_type_id -> Nullable<Text>,
        kind -> Text,
        percent_basis_points -> Int8,
        amount_minor -> Numeric,
        valid_from -> Nullable<Timestamptz>,
        valid_to -> Nullable<Timestamptz>,
        approved -> Bool,
        approved_by -> Nullable<Text>,
        reason -> Nullable<Text>,
        status -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    school_fee_receipts (id) {
        id -> Text,
        number -> Text,
        charge_id -> Text,
        student_user_id -> Text,
        amount_minor -> Numeric,
        currency -> Bpchar,
        method -> Nullable<Text>,
        reference -> Nullable<Text>,
        issued_by -> Text,
        issued_at -> Timestamptz,
    }
}

diesel::table! {
    school_fee_guardians (id) {
        id -> Text,
        guardian_user_id -> Text,
        student_user_id -> Text,
        relationship -> Text,
        active -> Bool,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    school_fee_reminders (charge_id, offset_days, recipient_user_id) {
        charge_id -> Text,
        offset_days -> Int4,
        recipient_user_id -> Text,
        sent_at -> Timestamptz,
    }
}

diesel::joinable!(school_fee_schedules -> school_fee_types (fee_type_id));
diesel::joinable!(school_fee_charges -> school_fee_schedules (schedule_id));
diesel::joinable!(school_fee_charges -> school_fee_types (fee_type_id));
diesel::joinable!(school_fee_discounts -> school_fee_types (fee_type_id));
diesel::joinable!(school_fee_receipts -> school_fee_charges (charge_id));
diesel::joinable!(school_fee_reminders -> school_fee_charges (charge_id));

// ---------------------------------------------------------------------------
// Library (migration 000097_library).
// No FKs by design (see migration notes): no joinable!s.
// ---------------------------------------------------------------------------

diesel::table! {
    library_categories (id) {
        id -> Uuid,
        name -> Text,
        slug -> Text,
        description -> Nullable<Text>,
        parent_id -> Nullable<Uuid>,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    library_items (id) {
        id -> Uuid,
        title -> Text,
        subtitle -> Nullable<Text>,
        authors -> Jsonb,
        isbn -> Nullable<Text>,
        isbn_normalized -> Nullable<Text>,
        edition -> Nullable<Text>,
        series -> Nullable<Text>,
        series_position -> Nullable<Int8>,
        publisher -> Nullable<Text>,
        published_year -> Nullable<Int8>,
        language -> Text,
        category_id -> Nullable<Uuid>,
        category_name -> Nullable<Text>,
        item_type -> Text,
        status -> Text,
        description -> Nullable<Text>,
        cover_ref -> Nullable<Text>,
        digital_ref -> Nullable<Text>,
        document_ref -> Nullable<Text>,
        storage_ref -> Nullable<Text>,
        tags -> Jsonb,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    library_copies (id) {
        id -> Uuid,
        item_id -> Uuid,
        barcode -> Text,
        serial -> Nullable<Text>,
        condition -> Text,
        location -> Nullable<Text>,
        shelf -> Nullable<Text>,
        status -> Text,
        notes -> Nullable<Text>,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    library_members (id) {
        id -> Uuid,
        user_id -> Text,
        member_type -> Text,
        status -> Text,
        joined_at -> Timestamptz,
        expires_at -> Nullable<Timestamptz>,
        notes -> Nullable<Text>,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    library_loans (id) {
        id -> Uuid,
        member_id -> Uuid,
        user_id -> Text,
        item_id -> Uuid,
        copy_id -> Nullable<Uuid>,
        status -> Text,
        checked_out_at -> Timestamptz,
        due_at -> Timestamptz,
        returned_at -> Nullable<Timestamptz>,
        renewal_count -> Int8,
        last_reminded_at -> Nullable<Timestamptz>,
        last_overdue_noticed_at -> Nullable<Timestamptz>,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    library_holds (id) {
        id -> Uuid,
        member_id -> Uuid,
        user_id -> Text,
        item_id -> Uuid,
        copy_id -> Nullable<Uuid>,
        status -> Text,
        position -> Int8,
        ready_at -> Nullable<Timestamptz>,
        expires_at -> Nullable<Timestamptz>,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    library_fines (id) {
        id -> Uuid,
        loan_id -> Uuid,
        member_id -> Uuid,
        user_id -> Text,
        amount_minor -> Numeric,
        currency -> Text,
        reason -> Text,
        status -> Text,
        waived_by -> Nullable<Text>,
        paid_reference -> Nullable<Text>,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    library_loan_policies (id) {
        id -> Uuid,
        name -> Text,
        member_type -> Text,
        item_type -> Text,
        loan_days -> Int8,
        max_concurrent -> Int8,
        max_renewals -> Int8,
        fine_per_day_minor -> Numeric,
        currency -> Text,
        grace_days -> Int8,
        active -> Bool,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    library_inventory_audits (id) {
        id -> Uuid,
        name -> Text,
        scope_location -> Nullable<Text>,
        scope_shelf -> Nullable<Text>,
        status -> Text,
        started_by -> Text,
        completed_at -> Nullable<Timestamptz>,
        items_checked -> Int8,
        discrepancies_found -> Int8,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    library_inventory_discrepancies (id) {
        id -> Uuid,
        audit_id -> Uuid,
        copy_id -> Uuid,
        item_id -> Uuid,
        kind -> Text,
        note -> Nullable<Text>,
        status -> Text,
        resolved_by -> Nullable<Text>,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    library_digital_access (id) {
        id -> Uuid,
        loan_id -> Uuid,
        item_id -> Uuid,
        member_id -> Uuid,
        user_id -> Text,
        expires_at -> Timestamptz,
        revoked -> Bool,
        redeemed_at -> Nullable<Timestamptz>,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Calendar (migration 000101_calendar).
// ---------------------------------------------------------------------------

diesel::table! {
    calendar_feeds (id) {
        id -> Uuid,
        tenant_id -> Text,
        company_id -> Nullable<Text>,
        user_id -> Text,
        token_hash -> Text,
        modules -> Jsonb,
        label -> Nullable<Text>,
        expires_at -> Timestamptz,
        revoked -> Bool,
        created_at -> Timestamptz,
        last_used_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    calendar_settings (id) {
        id -> Uuid,
        tenant_id -> Text,
        scope -> Text,
        owner_id -> Text,
        weekend_days -> Jsonb,
        working_hours_start -> Text,
        working_hours_end -> Text,
        default_view -> Text,
        timezone -> Text,
        holiday_country -> Text,
        create_link_templates -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    calendar_sources (id) {
        id -> Uuid,
        tenant_id -> Text,
        user_id -> Text,
        module -> Text,
        visible -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// SaaS packages (migration 000102_saas_packages).
// Ids are TEXT and there are no FKs by design: no joinable!s.
// ---------------------------------------------------------------------------

diesel::table! {
    saas_packages (id) {
        id -> Text,
        slug -> Text,
        name -> Text,
        description -> Text,
        status -> Text,
        current_version -> Int4,
        product_id -> Nullable<Text>,
        preset_settings -> Jsonb,
        wizard_steps -> Jsonb,
        created_by -> Text,
        vendor_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    saas_package_modules (package_id, version, module_id) {
        package_id -> Text,
        version -> Int4,
        module_id -> Text,
        required -> Bool,
        sort_order -> Int4,
    }
}

diesel::table! {
    saas_package_purchases (id) {
        id -> Text,
        package_id -> Text,
        package_version -> Int4,
        buyer_id -> Text,
        company_id -> Text,
        status -> Text,
        subdomain -> Nullable<Text>,
        subdomain_pending -> Bool,
        order_id -> Nullable<Text>,
        subscription_id -> Nullable<Text>,
        product_id -> Nullable<Text>,
        enabled_modules -> Jsonb,
        grace_ends_at -> Nullable<Timestamptz>,
        cancelled_at -> Nullable<Timestamptz>,
        cancel_reason -> Nullable<Text>,
        provision_attempts -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    saas_package_provision_steps (id) {
        id -> Text,
        purchase_id -> Text,
        step -> Text,
        status -> Text,
        outcome -> Nullable<Text>,
        detail -> Nullable<Text>,
        attempt -> Int4,
        started_at -> Timestamptz,
        completed_at -> Nullable<Timestamptz>,
    }
}

// ---------------------------------------------------------------------------
// Holidays (migration 000104_holidays).
// No FKs by design (see migration notes): no joinable!s.
// ---------------------------------------------------------------------------

diesel::table! {
    holiday_providers (id) {
        id -> Text,
        name -> Text,
        kind -> Text,
        base_url -> Text,
        allowed_hosts -> Jsonb,
        api_key_required -> Bool,
        api_key_encrypted -> Nullable<Text>,
        key_fingerprint -> Nullable<Text>,
        min_interval_secs -> Int8,
        max_requests_per_minute -> Int8,
        calendar_ids -> Jsonb,
        enabled -> Bool,
        builtin -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    holiday_countries (code) {
        code -> Text,
        name -> Text,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    holiday_sync_configs (id) {
        id -> Uuid,
        tenant_id -> Text,
        company_id -> Text,
        provider_id -> Text,
        fallback_provider_ids -> Jsonb,
        countries -> Jsonb,
        years -> Jsonb,
        schedule -> Text,
        feed_url -> Nullable<Text>,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    holiday_sync_runs (id) {
        id -> Uuid,
        tenant_id -> Text,
        company_id -> Text,
        config_id -> Uuid,
        provider_id -> Text,
        country -> Text,
        year -> Int8,
        status -> Text,
        trigger -> Text,
        holidays_found -> Int8,
        holidays_stored -> Int8,
        error -> Nullable<Text>,
        started_at -> Timestamptz,
        finished_at -> Timestamptz,
        fetched_at -> Timestamptz,
    }
}

diesel::table! {
    holidays (id) {
        id -> Uuid,
        country -> Text,
        date -> Date,
        name -> Text,
        local_name -> Nullable<Text>,
        provider_id -> Text,
        source -> Text,
        global -> Bool,
        types -> Jsonb,
        counties -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    holiday_overrides (id) {
        id -> Uuid,
        tenant_id -> Text,
        company_id -> Text,
        country -> Text,
        date -> Date,
        name -> Text,
        override_kind -> Text,
        note -> Nullable<Text>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Tenant destroy requests (no migration yet; derived from the
// `DestroyRequest` struct in src/modules/tenant_destroy.rs).
// ---------------------------------------------------------------------------

diesel::table! {
    tenant_destroy_requests (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        tenant_slug -> Varchar,
        status -> Varchar,
        requested_by -> Varchar,
        requested_at -> Timestamptz,
        reason -> Nullable<Text>,
        export_job_id -> Nullable<Uuid>,
        export_requested_at -> Nullable<Timestamptz>,
        export_completed_at -> Nullable<Timestamptz>,
        export_download_url -> Nullable<Text>,
        export_verified_via -> Nullable<Varchar>,
        export_request_claimed -> Bool,
        confirmation_attempts -> Int4,
        failed_confirmation_attempts -> Int4,
        confirmation_locked_until -> Nullable<Timestamptz>,
        confirmed_at -> Nullable<Timestamptz>,
        confirmed_by -> Nullable<Varchar>,
        second_approval_required -> Bool,
        second_approval_reason -> Nullable<Text>,
        second_approver -> Nullable<Varchar>,
        second_approved_at -> Nullable<Timestamptz>,
        grace_period_days -> Int8,
        grace_expires_at -> Nullable<Timestamptz>,
        suspended_at -> Nullable<Timestamptz>,
        destroyed_at -> Nullable<Timestamptz>,
        schema_name_snapshot -> Nullable<Varchar>,
        subdomains_revoke_requested -> Bool,
        external_cleanup -> Jsonb,
        audit_trail -> Jsonb,
        cancel_reason -> Nullable<Text>,
        cancelled_by -> Nullable<Varchar>,
        cancelled_at -> Nullable<Timestamptz>,
        last_error -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Tables added by the integration round (migrations 000032-000107,
// plus `reference_id_settings` and `tenant_destroy_requests`,
// which have no migration yet).
// ---------------------------------------------------------------------------

diesel::allow_tables_to_appear_in_same_query!(
    timetracker_timers,
    timetracker_entries,
    timetracker_activity,
    timetracker_app_usage,
    timetracker_screenshots,
    timetracker_rates,
    timetracker_timesheets,
    timetracker_settings,
    timetracker_user_settings,
    timetracker_teams,
    timetracker_team_members,
    timetracker_staff_settings,
    timetracker_staff_invites,
    timetracker_buyer_settings,
    storage_buckets,
    storage_bindings,
    storage_quotas,
    storage_usage_daily,
    storage_lifecycle_rules,
    storage_signed_url_revocations,
    storage_health_checks,
    events,
    event_occurrences,
    event_ticket_types,
    event_registrations,
    event_attendees,
    event_waitlist,
    event_reminders,
    todo_lists,
    todos,
    todo_subtasks,
    todo_shares,
    todo_completion_log,
    todo_reminders,
    ai_company_policies,
    ai_user_policies,
    ai_personal_keys,
    ai_user_mcp_servers,
    calculator_saved_calculations,
    calculator_reports,
    calculator_history,
    calculator_currency_rates,
    hr_employees,
    hr_positions,
    hr_contracts,
    hr_leave_types,
    hr_leave_balances,
    hr_leave_requests,
    hr_attendance_records,
    hr_payroll_runs,
    hr_payroll_items,
    hr_review_cycles,
    hr_reviews,
    hr_checklists,
    hr_checklist_items,
    hr_documents,
    tax_rules,
    tax_settings,
    tax_computations,
    tax_filings,
    tax_connectors,
    tax_audit,
    advisor_suggestions,
    advisor_rule_runs,
    risks,
    risk_mitigation_plans,
    risk_mitigation_tasks,
    risk_checklist_templates,
    risk_checklist_template_items,
    risk_checklist_instances,
    risk_checklist_instance_items,
    risk_alerts,
    subscriptions,
    subscription_events,
    dunning_attempts,
    pricing_tables,
    pricing_tiers,
    pricing_tier_features,
    marketplace_vendors,
    marketplace_commission_rules,
    marketplace_product_approvals,
    marketplace_commissions,
    marketplace_payouts,
    public_links,
    public_link_access_log,
    auditor_accounts,
    auditor_scopes,
    auditor_sessions,
    properties,
    property_plans,
    property_blocks,
    property_block_history,
    logistics_drivers,
    logistics_vehicles,
    logistics_warehouses,
    logistics_shipments,
    logistics_shipment_events,
    logistics_stock_movements,
    logistics_routes,
    logistics_route_stops,
    logistics_proofs,
    logistics_exceptions,
    logistics_rate_tables,
    supervision_assignments,
    supervision_inspection_templates,
    supervision_template_items,
    supervision_inspections,
    supervision_inspection_items,
    supervision_progress_reports,
    supervision_defects,
    supervision_approvals,
    supervision_daily_logs,
    supervision_photos,
    ai_global_settings,
    network_tool_saved_checks,
    network_tool_runs,
    network_tool_uptime_monitors,
    network_tool_uptime_events,
    network_tool_cert_watches,
    license_products,
    license_distributions,
    license_releases,
    license_events,
    license_offline_tokens,
    license_activation_history,
    saas_domain_reservations,
    saas_domain_aliases,
    saas_subdomain_allocations,
    domain_verification_settings,
    domain_verification_tokens,
    domain_verification_attempts,
    domain_cert_modes,
    cloudflare_detections,
    cloudflare_ip_ranges,
    whitelabel_global_settings,
    whitelabel_company_settings,
    whitelabel_global_defaults,
    whitelabel_profiles,
    whitelabel_history,
    presets,
    preset_modules,
    preset_wizard_states,
    preset_applications,
    entitlements,
    entitlement_history,
    entitlement_denied_attempts,
    entitlement_package_changes,
    entitlement_settings,
    entitlement_module_activations,
    saas_product_subdomains,
    saas_product_subdomain_events,
    pricing_settings,
    pricing_currencies,
    pricing_rules,
    pricing_coupons,
    pricing_coupon_redemptions,
    pricing_recommendations,
    pricing_cost_lines,
    pricing_bundles,
    pricing_bundle_components,
    pricing_reference_prices,
    pricing_price_versions,
    pricing_price_approvals,
    pricing_proration_records,
    pricing_credit_notes,
    pricing_reconciliation_reports,
    storage_plans,
    storage_plan_assignments,
    storage_usage_metering,
    storage_byo_connections,
    storage_byo_terms,
    storage_byo_terms_acceptances,
    storage_byo_objects,
    storage_byo_health_checks,
    card_custom_rules,
    card_validation_daily_counts,
    reference_id_settings,
    tenants,
    tenant_provisioning_steps,
    tenant_module_grants,
    bi_reports,
    bi_report_schedules,
    bi_kpi_dashboards,
    okr_cycles,
    okr_objectives,
    okr_key_results,
    okr_checkins,
    okr_links,
    affiliates,
    affiliate_commission_rules,
    affiliate_links,
    affiliate_clicks,
    affiliate_conversions,
    affiliate_commissions,
    affiliate_payouts,
    loyalty_programs,
    loyalty_earn_rules,
    loyalty_tiers,
    loyalty_ledger,
    loyalty_redemptions,
    portals,
    portal_menus,
    portal_views,
    forms,
    form_fields,
    form_submissions,
    form_submission_values,
    contract_templates,
    contracts,
    contract_signers,
    contract_audit_trail,
    vendors,
    purchase_orders,
    purchase_order_lines,
    purchase_order_approvals,
    goods_receipts,
    procurement_budgets,
    assets,
    asset_assignments,
    asset_stock,
    asset_stock_adjustments,
    asset_stock_takes,
    asset_stock_take_lines,
    jobs,
    applicants,
    applicant_stage_history,
    interviews,
    evaluations,
    offers,
    vault_folders,
    vault_documents,
    vault_document_versions,
    vault_access_log,
    vault_share_links,
    vault_access_requests,
    incidents,
    incident_timeline,
    on_call_rotations,
    on_call_schedules,
    escalation_policies,
    incident_escalations,
    page_attempts,
    incident_postmortems,
    postmortem_action_items,
    auto_open_rules,
    inbox_threads,
    inbox_assignments,
    inbox_canned_replies,
    inbox_thread_reads,
    surveys,
    survey_questions,
    survey_distributions,
    survey_responses,
    survey_answers,
    community_categories,
    community_topics,
    community_posts,
    community_post_edits,
    community_votes,
    community_flags,
    community_moderation_actions,
    community_members,
    community_announcements,
    compliance_export_requests,
    compliance_deletion_requests,
    compliance_deletion_items,
    compliance_consents,
    compliance_retention_policies,
    compliance_retention_reviews,
    compliance_processing_records,
    compliance_checklists,
    compliance_breach_log,
    compliance_breach_timeline,
    gamification_rules,
    gamification_points_ledger,
    gamification_badges,
    gamification_awards,
    gamification_seasons,
    gamification_rewards,
    gamification_reward_requests,
    workflows,
    workflow_runs,
    workflow_run_steps,
    documents,
    document_versions,
    document_upload_sessions,
    document_upload_parts,
    document_allowlist_rules,
    document_downloads,
    document_quotas,
    privacy_field_declarations,
    privacy_anonymizations,
    privacy_visibility_defaults,
    privacy_consents,
    privacy_consent_history,
    privacy_erasure_requests,
    privacy_notices,
    privacy_audit_entries,
    web3_networks,
    web3_watched_addresses,
    web3_transactions,
    web3_anchors,
    web3_wallets,
    web3_balances,
    web3_settings,
    pos_registers,
    pos_shifts,
    pos_sales,
    pos_sale_items,
    pos_payments,
    pos_refunds,
    pos_receipts,
    appointment_services,
    appointment_staff_availability,
    appointment_staff_services,
    appointments,
    appointment_reminders,
    digital_product_types,
    digital_products,
    digital_product_files,
    digital_product_previews,
    digital_payout_requests,
    digital_royalties,
    digital_downloads,
    digital_watermark_formats,
    courses,
    course_modules,
    lessons,
    enrollments,
    lesson_progress,
    quizzes,
    quiz_questions,
    quiz_attempts,
    reviews,
    learning_paths,
    learning_path_courses,
    field_equipment,
    field_maintenance_plans,
    field_work_orders,
    field_work_order_items,
    field_work_order_parts,
    field_dispatch_assignments,
    field_signoffs,
    webinars,
    webinar_registrations,
    webinar_polls,
    webinar_poll_votes,
    webinar_questions,
    webinar_attendance,
    webinar_recordings,
    cluster_sync_conflicts,
    cluster_sync_retries,
    cluster_sync_idempotency_keys,
    cluster_sync_settings,
    cluster_sync_domains,
    mapping_templates,
    export_schedules,
    import_sessions,
    marketing_campaigns,
    marketing_campaign_variants,
    marketing_segments,
    marketing_segment_size_history,
    marketing_sequences,
    marketing_sequence_steps,
    marketing_enrollments,
    marketing_scores,
    marketing_score_rules,
    marketing_sends,
    mfg_boms,
    mfg_bom_versions,
    mfg_bom_version_lines,
    mfg_work_centers,
    mfg_bom_routing_steps,
    mfg_work_orders,
    mfg_work_order_requirements,
    mfg_scrap_reasons,
    mfg_units,
    mfg_production_runs,
    mfg_reservations,
    mfg_stock_notices,
    quality_points,
    quality_criteria,
    quality_inspections,
    quality_alerts,
    quality_capas,
    expense_categories,
    expense_policies,
    expense_reports,
    expenses,
    expense_receipts,
    expense_reimbursements,
    expense_adjustments,
    expense_currency_rates,
    approval_definitions,
    approval_steps,
    approval_requests,
    approval_decisions,
    approval_delegations,
    approval_escalations,
    studio_record_types,
    studio_system_fields,
    studio_field_definitions,
    studio_field_values,
    studio_list_views,
    studio_form_layouts,
    paper_sizes,
    invoice_templates,
    invoice_template_versions,
    printer_profiles,
    print_jobs,
    curriculum_frameworks,
    curriculum_levels,
    curriculum_subjects,
    curriculum_core_competencies,
    curriculum_basic_competencies,
    curriculum_structures,
    curriculum_assessment_domains,
    curriculum_grade_descriptors,
    curriculum_lesson_plans,
    curriculum_report_cards,
    curriculum_report_card_entries,
    curriculum_interventions,
    curriculum_settings,
    admission_periods,
    admission_period_quotas,
    admission_stage_definitions,
    admission_required_documents,
    admission_applications,
    admission_application_scores,
    admission_checklist_items,
    admission_waitlist_entries,
    admission_results,
    admission_settings,
    academic_programs,
    academic_students,
    academic_lecturers,
    academic_subjects,
    academic_rooms,
    academic_semesters,
    academic_calendar_events,
    academic_schedules,
    academic_enrollments,
    academic_attendance,
    academic_grade_components,
    academic_grades,
    academic_grade_appeals,
    academic_transcripts,
    academic_theses,
    academic_standing_records,
    academic_definitions,
    academic_settings,
    exam_question_banks,
    exam_questions,
    exam_question_versions,
    exams,
    exam_question_links,
    exam_rubrics,
    exam_attempts,
    exam_grades,
    exam_proctor_signals,
    exam_results,
    school_fee_types,
    school_fee_schedules,
    school_fee_charges,
    school_fee_discounts,
    school_fee_receipts,
    school_fee_guardians,
    school_fee_reminders,
    library_categories,
    library_items,
    library_copies,
    library_members,
    library_loans,
    library_holds,
    library_fines,
    library_loan_policies,
    library_inventory_audits,
    library_inventory_discrepancies,
    library_digital_access,
    calendar_feeds,
    calendar_settings,
    calendar_sources,
    saas_packages,
    saas_package_modules,
    saas_package_purchases,
    saas_package_provision_steps,
    holiday_providers,
    holiday_countries,
    holiday_sync_configs,
    holiday_sync_runs,
    holidays,
    holiday_overrides,
    tenant_destroy_requests,
);
