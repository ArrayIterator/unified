// Site Builder - Public record, request, and view types.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::sync::OnceLock;
use uuid::Uuid;

use super::constants::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::rendering::*;
use super::handlers::*;
use super::module::*;

#[derive(Debug, Clone, Serialize)]
pub struct Block {
    pub id: String,
    #[serde(rename = "type")]
    pub block_type: String,
    pub content: Value,
    pub order: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Section {
    pub id: String,
    pub name: String,
    pub layout: String,
    pub content: Value,
    pub order: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Page {
    pub id: String,
    pub title: String,
    pub slug: String,
    pub theme_id: String,
    /// `draft` or `published`.
    pub status: String,
    pub blocks: Vec<Block>,
    pub sections: Vec<Section>,
    pub created_at: String,
    pub updated_at: String,
}
