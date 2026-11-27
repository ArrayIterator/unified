// Global Search - Public record, request, and view types.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, Pagination, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use super::constants::*;
use super::store::*;
use super::service::*;
use super::handlers::*;
use super::hooks::*;
use super::module::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchDocument {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub snippet: String,
    #[serde(default)]
    pub url: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IndexDocumentRequest {
    pub id: String,
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub snippet: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub document: SearchDocument,
    pub score: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct KindGroup {
    pub kind: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResponse {
    pub items: Vec<SearchHit>,
    pub groups: Vec<KindGroup>,
    pub pagination: unified_core::pagination::PageInfo,
}

#[derive(Debug, Clone, Serialize)]
pub struct SuggestResponse {
    pub query: String,
    pub suggestions: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchStats {
    pub total_documents: u64,
    pub by_kind: Vec<KindGroup>,
    pub kinds: Vec<String>,
    pub max_documents: usize,
}
