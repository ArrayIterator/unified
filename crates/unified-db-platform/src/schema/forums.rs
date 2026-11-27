// Diesel schema for the forums domain of the unified-db-platform bounded context.
//
// Tables are defined by migration 2026-11-22-000110_forums. They had
// no Diesel bindings before C11: the forums module kept
// its live data in memory and the tables were persistence targets
// only. The columns below are byte-identical to that migration.
// No foreign keys: forum rows reference users and documents by
// plain ids only (see the migration header), so no `joinable!`
// declarations belong here; the hydrate path in
// `src/modules/forums/state.rs` loads each table on its own.

// Forums (migration 000110_forums).

diesel::table! {
    forums (id) {
        id -> Uuid,
        tenant_id -> Text,
        name -> Text,
        description -> Nullable<Text>,
        slug -> Text,
        icon -> Nullable<Text>,
        visibility -> Text,
        sort_order -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    forum_categories (id) {
        id -> Uuid,
        tenant_id -> Text,
        forum_id -> Uuid,
        name -> Text,
        description -> Nullable<Text>,
        slug -> Text,
        sort_order -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    forum_threads (id) {
        id -> Uuid,
        tenant_id -> Text,
        forum_id -> Uuid,
        category_id -> Nullable<Uuid>,
        title -> Text,
        author_id -> Text,
        tags -> Array<Text>,
        pinned -> Bool,
        locked -> Bool,
        solved -> Bool,
        best_answer_post_id -> Nullable<Uuid>,
        view_count -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    forum_posts (id) {
        id -> Uuid,
        tenant_id -> Text,
        thread_id -> Uuid,
        parent_post_id -> Nullable<Uuid>,
        depth -> Int2,
        author_id -> Text,
        body -> Text,
        hidden -> Bool,
        edited -> Bool,
        edited_at -> Nullable<Timestamptz>,
        quoted_post_id -> Nullable<Uuid>,
        attachment_ids -> Array<Text>,
        mentions -> Array<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    forum_post_edits (id) {
        id -> Uuid,
        tenant_id -> Text,
        post_id -> Uuid,
        editor_id -> Text,
        previous_body -> Text,
        edited_at -> Timestamptz,
    }
}

diesel::table! {
    forum_votes (id) {
        id -> Uuid,
        tenant_id -> Text,
        post_id -> Uuid,
        user_id -> Text,
        value -> Int2,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    forum_reports (id) {
        id -> Uuid,
        tenant_id -> Text,
        target_type -> Text,
        target_id -> Uuid,
        reporter_id -> Text,
        reason -> Text,
        status -> Text,
        reviewed_by -> Nullable<Text>,
        reviewed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    forum_moderation_actions (id) {
        id -> Uuid,
        tenant_id -> Text,
        target_type -> Text,
        target_id -> Uuid,
        action -> Text,
        moderator_id -> Text,
        reason -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    forum_subscriptions (id) {
        id -> Uuid,
        tenant_id -> Text,
        user_id -> Text,
        target_type -> Text,
        target_id -> Uuid,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    forum_polls (id) {
        id -> Uuid,
        tenant_id -> Text,
        thread_id -> Uuid,
        question -> Text,
        closed -> Bool,
        closed_at -> Nullable<Timestamptz>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    forum_poll_options (id) {
        id -> Uuid,
        tenant_id -> Text,
        poll_id -> Uuid,
        text -> Text,
        sort_order -> Int8,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    forum_poll_votes (id) {
        id -> Uuid,
        tenant_id -> Text,
        poll_id -> Uuid,
        option_id -> Uuid,
        user_id -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    forum_reputation (id) {
        id -> Uuid,
        tenant_id -> Text,
        user_id -> Text,
        score -> Int8,
        upvotes_received -> Int8,
        downvotes_received -> Int8,
        best_answers -> Int8,
        badge_count -> Int8,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    forum_settings (tenant_id) {
        tenant_id -> Text,
        max_threads_per_hour -> Int8,
        max_posts_per_hour -> Int8,
        spam_report_threshold -> Int8,
        new_user_link_limit -> Int8,
        new_user_age_hours -> Int8,
        max_attachments_per_post -> Int8,
        updated_at -> Timestamptz,
    }
}
