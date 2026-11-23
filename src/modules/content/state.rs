//!
//! Content Management module. — State.
//!
//! Split from `src/modules/content.rs`; behaviour unchanged.
//! Task 356 adds the tag, post-type, and custom-entry stores.
//!
//! Tenant isolation: every new record carries the resolved tenant
//! id and every handler filters on it. The older posts / categories
//! / pages stores predate tenant partitioning (see the C3 note in
//! `helpers.rs`); the new stores do not repeat that gap. The
//! durable tables (migration 000133) are provisioned per tenant
//! schema, so the tenant column there is the same partition key.

use super::ai_capabilities::*;
use super::categories::*;
use super::custom_types::*;
use super::helpers::*;
use super::module::*;
use super::pages::*;
use super::posts::*;
use super::state::*;
use super::tags::*;
use super::types::*;

use dashmap::DashMap;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone, Default)]
pub struct ContentState {
    pub posts: Arc<DashMap<Uuid, Post>>,
    pub categories: Arc<DashMap<Uuid, Category>>,
    pub pages: Arc<DashMap<Uuid, Page>>,
    /// Tags, keyed by id. Slug uniqueness is per tenant (see
    /// `tags.rs`), so lookups by slug always filter on tenant.
    pub tags: Arc<DashMap<Uuid, Tag>>,
    /// The post/tags join, keyed by
    /// `(tenant_id, target_kind, target_id, tag_id)`. `target_kind`
    /// is `"post"` or `"custom"`. Entries keep a `tag_ids` read
    /// snapshot; this map is the authority and both are updated
    /// together by the tag helpers in `tags.rs`.
    pub post_tags: Arc<DashMap<(Uuid, String, Uuid, Uuid), PostTagLink>>,
    /// Custom post type definitions, keyed by id. Slug uniqueness
    /// is per tenant; lookups by slug filter on tenant.
    pub post_types: Arc<DashMap<Uuid, PostTypeDefinition>>,
    /// Custom entries of ALL types in one store, keyed by id.
    /// `type_slug` selects the type; there is no store per type.
    pub custom_entries: Arc<DashMap<Uuid, CustomEntry>>,
}
