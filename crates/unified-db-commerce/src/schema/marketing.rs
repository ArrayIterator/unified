// Diesel schema for the marketing domain of the unified-db-commerce bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

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
