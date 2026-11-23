//! Knowledgebase - Shared types: request and response bodies, views, and state shapes.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::{constants::*, state::*, helpers::*, module::*, handlers_articles::*, handlers_tickets::*, handlers_categories::*};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Publication status of an article.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArticleStatus {
    Draft,
    Published,
    Archived,
}

impl Default for ArticleStatus {
    fn default() -> Self {
        Self::Draft
    }
}

/// A knowledgebase article. `version` is the current version number; every
/// update stores a new [`ArticleVersion`] row and bumps this number.
/// `company_id` is the owning tenant, resolved at creation from the
/// caller (see `caller_company`); `None` means the article was
/// created before tenant partitioning or by a caller with no company,
/// and such articles are never returned by the tenant-scoped hook
/// searches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Article {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub category_id: Option<Uuid>,
    pub status: ArticleStatus,
    pub tags: Vec<String>,
    #[serde(default)]
    pub company_id: Option<Uuid>,
    pub version: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One immutable snapshot of an article, created on every update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArticleVersion {
    pub article_id: Uuid,
    pub version: u32,
    pub title: String,
    pub body: String,
    pub status: ArticleStatus,
    pub created_at: DateTime<Utc>,
}

/// A category grouping articles. Categories may be nested via `parent_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Category {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub parent_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A link between an article and a support ticket (owned by the tickets
/// module; only the ticket id is stored here).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketLink {
    pub article_id: Uuid,
    pub ticket_id: String,
    pub linked_at: DateTime<Utc>,
}

/// One ranked search hit.
#[derive(Debug, Clone, Serialize)]
pub struct ArticleSearchHit {
    pub id: Uuid,
    pub title: String,
    pub excerpt: String,
    pub score: f64,
    pub category_id: Option<Uuid>,
    pub status: ArticleStatus,
    pub tags: Vec<String>,
    pub url: String,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateArticleRequest {
    pub title: String,
    pub body: String,
    pub category_id: Option<Uuid>,
    pub status: Option<ArticleStatus>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateArticleRequest {
    pub title: Option<String>,
    pub body: Option<String>,
    /// `null` clears the category, a value moves the article.
    pub category_id: Option<Option<Uuid>>,
    pub status: Option<ArticleStatus>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateCategoryRequest {
    pub name: String,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub parent_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCategoryRequest {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub description: Option<Option<String>>,
    pub parent_id: Option<Option<Uuid>>,
}

#[derive(Debug, Deserialize)]
pub struct LinkTicketRequest {
    pub ticket_id: String,
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
    pub status: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn article_status_default_is_draft() {
        assert!(matches!(ArticleStatus::default(), ArticleStatus::Draft));
    }
}
