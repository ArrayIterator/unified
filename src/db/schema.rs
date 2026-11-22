// Diesel schema for the core tables.
//
// This schema matches every migration from
// `migrations/2026-11-22-000001_core/up.sql` through
// `migrations/2026-11-22-000030_ai/up.sql` (migration 000003 is
// ALTER-only; its added columns and NUMERIC(19,4) conversions are
// folded into the table definitions below).
// Modules that need persistence beyond their in-memory store use these
// tables; the software, store, and VCS modules still keep their live
// data in memory (see TODO.md) and these tables are their persistence
// target, not yet their read path.
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
);
