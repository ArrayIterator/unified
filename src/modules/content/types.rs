//! Content Management module. — Types.
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
/// author-written caption for THIS attachment — it lives with the
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
