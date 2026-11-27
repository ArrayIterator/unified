//! Content Management module. - Types.
//!
//! Split from `src/modules/content.rs`; behaviour unchanged.

use super::ai_capabilities::*;
use super::categories::*;
use super::helpers::*;
use super::module::*;
use super::pages::*;
use super::posts::*;
use super::state::*;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Publication status of a post or a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentStatus {
    Draft,
    Published,
}

impl Default for ContentStatus {
    fn default() -> Self {
        Self::Draft
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Post {
    pub id: Uuid,
    pub title: String,
    pub slug: String,
    pub content: String,
    pub excerpt: String,
    pub status: ContentStatus,
    pub category_id: Option<Uuid>,
    pub author_id: Option<Uuid>,
    /// Media attached from the shared documents store (never a
    /// re-upload): each entry references one document by id and
    /// carries the /files/ URL to render plus the author-written
    /// description for THIS attachment.
    #[serde(default)]
    pub attachments: Vec<PostAttachment>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
}

/// One media attachment on a post/page: a reference into the
/// shared documents store (never a re-upload). `document_id` is
/// the documents-store id; `url` is the /files/ URL to render
/// (public -> `/files/public/{key}`, private ->
/// `/files/private/{key}` or a signed URL); `description` is the
/// author-written caption for THIS attachment - it lives with the
/// post's attachment reference, NOT on the document row (the
/// documents metadata columns are a separate, paused task).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostAttachment {
    pub document_id: String,
    pub url: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Category {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub id: Uuid,
    pub title: String,
    pub slug: String,
    pub content: String,
    pub excerpt: String,
    pub status: ContentStatus,
    pub author_id: Option<Uuid>,
    /// Media attached from the shared documents store (see
    /// `Post::attachments`).
    #[serde(default)]
    pub attachments: Vec<PostAttachment>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePostInput {
    pub title: String,
    pub slug: Option<String>,
    pub content: Option<String>,
    pub excerpt: Option<String>,
    pub category_id: Option<Uuid>,
    pub author_id: Option<Uuid>,
    pub status: Option<ContentStatus>,
    #[serde(default)]
    pub attachments: Vec<PostAttachment>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePostInput {
    pub title: Option<String>,
    pub slug: Option<String>,
    pub content: Option<String>,
    pub excerpt: Option<String>,
    pub category_id: Option<Uuid>,
    pub author_id: Option<Uuid>,
    pub status: Option<ContentStatus>,
    /// Replace the attachments wholesale when present; absent
    /// leaves them alone.
    #[serde(default)]
    pub attachments: Option<Vec<PostAttachment>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateCategoryInput {
    pub name: String,
    pub slug: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCategoryInput {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePageInput {
    pub title: String,
    pub slug: Option<String>,
    pub content: Option<String>,
    pub excerpt: Option<String>,
    pub author_id: Option<Uuid>,
    pub status: Option<ContentStatus>,
    #[serde(default)]
    pub attachments: Vec<PostAttachment>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePageInput {
    pub title: Option<String>,
    pub slug: Option<String>,
    pub content: Option<String>,
    pub excerpt: Option<String>,
    pub author_id: Option<Uuid>,
    pub status: Option<ContentStatus>,
    /// Replace the attachments wholesale when present; absent
    /// leaves them alone.
    #[serde(default)]
    pub attachments: Option<Vec<PostAttachment>>,
}

// Tags.

/// A tag: a free-form taxonomy label usable by posts AND custom
/// entries. The slug is kebab-case and unique per tenant. Tags are
/// attached through the `post_tags` join (see `PostTagLink`), never
/// by embedding ids on the post or entry row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One row of the post/tags join: which tag is attached to which
/// target. `target_kind` is `"post"` for a built-in post or
/// `"custom"` for a custom entry; `target_id` is the post or entry
/// id. The runtime key is
/// `(tenant_id, target_kind, target_id, tag_id)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostTagLink {
    pub tenant_id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub tag_id: Uuid,
    pub created_at: DateTime<Utc>,
}

/// Target kinds for the post/tags join.
pub const TAG_TARGET_POST: &str = "post";
pub const TAG_TARGET_CUSTOM: &str = "custom";

#[derive(Debug, Deserialize)]
pub struct CreateTagInput {
    pub name: String,
    pub slug: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTagInput {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub description: Option<String>,
}

/// Assign tags to a post or a custom entry. Either one id or a list
/// of ids; both may be given and are merged (duplicates ignored).
#[derive(Debug, Deserialize)]
pub struct AssignTagsInput {
    pub tag_id: Option<Uuid>,
    #[serde(default)]
    pub tag_ids: Vec<Uuid>,
}

// Dynamic custom post types.

/// The built-in features a post type may support. Stored as plain
/// strings on the definition so a future feature can be added
/// without a code change; unknown values are rejected on write.
pub const SUPPORTED_FEATURES: [&str; 7] = [
    "title",
    "editor",
    "excerpt",
    "thumbnail",
    "categories",
    "tags",
    "custom-fields",
];

/// The field types a dynamic custom field may have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CustomFieldType {
    Text,
    Textarea,
    Richtext,
    Number,
    Date,
    Datetime,
    Select,
    Multiselect,
    Checkbox,
    Media,
    Relation,
}

/// Visibility of a custom post type: `public` types render on the
/// public surface (published entries only); `private` types are
/// visible only to callers holding `content.manage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PostTypeVisibility {
    Public,
    Private,
}

impl Default for PostTypeVisibility {
    fn default() -> Self {
        // Fail closed: a new type stays private until an admin
        // explicitly publishes it.
        Self::Private
    }
}

/// One dynamic custom field on a post type definition. `key` is the
/// storage key inside an entry's `fields` map (snake_case, unique
/// within the type). `options` is the allowlist for `select` and
/// `multiselect` fields and must be empty for every other type.
/// `default` is applied on entry create when the field is absent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomFieldDef {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub field_type: CustomFieldType,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub options: Vec<String>,
}

/// An admin-definable custom post type. Everything about the type
/// is data on this row: labels, icon (a module sprite symbol name),
/// which built-in features it supports, its dynamic field
/// definitions, visibility, archive/menu/REST flags, and whether it
/// is enabled. No per-type code exists anywhere: routes, the admin
/// menu entry, and the React list/form screens are all generated
/// from this definition.
///
/// Disabling (or deleting) a type hides its routes and UI but KEEPS
/// its entries: entries are preserved and reappear if the type is
/// re-enabled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostTypeDefinition {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub slug: String,
    pub singular_label: String,
    pub plural_label: String,
    pub icon: String,
    pub description: Option<String>,
    #[serde(default)]
    pub supports: Vec<String>,
    #[serde(default)]
    pub fields: Vec<CustomFieldDef>,
    #[serde(default)]
    pub visibility: PostTypeVisibility,
    #[serde(default)]
    pub has_archive: bool,
    #[serde(default)]
    pub show_in_menu: bool,
    #[serde(default)]
    pub menu_position: i32,
    #[serde(default)]
    pub rest_exposed: bool,
    #[serde(default = "default_post_type_enabled")]
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn default_post_type_enabled() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct CreatePostTypeInput {
    pub slug: String,
    pub singular_label: String,
    pub plural_label: String,
    #[serde(default)]
    pub icon: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub supports: Vec<String>,
    #[serde(default)]
    pub fields: Vec<CustomFieldDef>,
    #[serde(default)]
    pub visibility: Option<PostTypeVisibility>,
    #[serde(default)]
    pub has_archive: Option<bool>,
    #[serde(default)]
    pub show_in_menu: Option<bool>,
    #[serde(default)]
    pub menu_position: Option<i32>,
    #[serde(default)]
    pub rest_exposed: Option<bool>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePostTypeInput {
    pub slug: Option<String>,
    pub singular_label: Option<String>,
    pub plural_label: Option<String>,
    pub icon: Option<String>,
    pub description: Option<String>,
    pub supports: Option<Vec<String>>,
    pub fields: Option<Vec<CustomFieldDef>>,
    pub visibility: Option<PostTypeVisibility>,
    pub has_archive: Option<bool>,
    pub show_in_menu: Option<bool>,
    pub menu_position: Option<i32>,
    pub rest_exposed: Option<bool>,
    pub enabled: Option<bool>,
}

/// One entry of a custom post type. All entries - of every type -
/// live in ONE generic store (one table, `content_custom_entries`,
/// with a JSONB `fields` column); there is no table per type.
/// `fields` holds the validated values of the type's dynamic field
/// definitions, keyed by field key. `tag_ids` is the read snapshot
/// of the `post_tags` join for this entry (the join is the
/// authority; both are updated together).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomEntry {
    pub id: Uuid,
    pub tenant_id: Uuid,
    #[serde(rename = "type")]
    pub type_slug: String,
    pub title: String,
    pub slug: String,
    pub content: String,
    pub excerpt: String,
    pub status: ContentStatus,
    pub category_id: Option<Uuid>,
    pub author_id: Option<Uuid>,
    #[serde(default)]
    pub thumbnail: Option<String>,
    #[serde(default)]
    pub fields: serde_json::Map<String, Value>,
    #[serde(default)]
    pub tag_ids: Vec<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateCustomEntryInput {
    #[serde(default)]
    pub title: Option<String>,
    pub slug: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    pub excerpt: Option<String>,
    pub category_id: Option<Uuid>,
    pub author_id: Option<Uuid>,
    pub thumbnail: Option<String>,
    pub status: Option<ContentStatus>,
    #[serde(default)]
    pub fields: serde_json::Map<String, Value>,
    #[serde(default)]
    pub tag_ids: Vec<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCustomEntryInput {
    pub title: Option<String>,
    pub slug: Option<String>,
    pub content: Option<String>,
    pub excerpt: Option<String>,
    pub category_id: Option<Uuid>,
    pub author_id: Option<Uuid>,
    pub thumbnail: Option<String>,
    pub status: Option<ContentStatus>,
    /// Replace the dynamic field values wholesale when present;
    /// absent leaves them alone. Unknown keys are rejected.
    pub fields: Option<serde_json::Map<String, Value>>,
    /// Replace the tag assignment wholesale when present; absent
    /// leaves it alone.
    pub tag_ids: Option<Vec<Uuid>>,
}
