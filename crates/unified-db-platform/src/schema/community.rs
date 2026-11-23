// Diesel schema for the community domain of the unified-db-platform bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

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
