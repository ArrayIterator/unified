// Diesel schema for the content tags + custom post types domain
// of the unified-db-platform bounded context (task 356).
//
// Byte-identical to migration
// 2026-11-23-000133_content_tags_custom_types. The file is named
// `content.rs` (not `content_tags.rs`) so the `pub mod content`
// binding never collides with a table module: the tables are
// `content_tags`, `content_post_tags`, `content_post_types`, and
// `content_custom_entries` (see the name-collision rule in
// `src/schema.rs`).

diesel::table! {
    content_tags (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        name -> Varchar,
        slug -> Varchar,
        description -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    content_post_tags (tenant_id, target_kind, target_id, tag_id) {
        tenant_id -> Uuid,
        target_kind -> Varchar,
        target_id -> Uuid,
        tag_id -> Uuid,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    content_post_types (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        slug -> Varchar,
        singular_label -> Varchar,
        plural_label -> Varchar,
        icon -> Varchar,
        description -> Nullable<Text>,
        supports -> Jsonb,
        fields -> Jsonb,
        visibility -> Varchar,
        has_archive -> Bool,
        show_in_menu -> Bool,
        menu_position -> Int4,
        rest_exposed -> Bool,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    content_custom_entries (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        type_slug -> Varchar,
        title -> Varchar,
        slug -> Varchar,
        content -> Text,
        excerpt -> Varchar,
        status -> Varchar,
        category_id -> Nullable<Uuid>,
        author_id -> Nullable<Uuid>,
        thumbnail -> Nullable<Text>,
        fields -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        published_at -> Nullable<Timestamptz>,
    }
}

diesel::allow_tables_to_appear_in_same_query!(
    content_tags,
    content_post_tags,
    content_post_types,
    content_custom_entries,
);
