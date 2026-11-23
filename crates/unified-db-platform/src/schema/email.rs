// Diesel schema for the email domain of the unified-db-platform bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

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
