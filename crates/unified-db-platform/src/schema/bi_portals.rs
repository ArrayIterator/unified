// Diesel schema for the bi_portals domain of the unified-db-platform bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Business Intelligence (migration 000042_bi).

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

// Client portals (migration 000046_portals).

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
