//! Content Management module. — State.
//!
//! Split from `src/modules/content.rs`; behaviour unchanged.

use super::ai_capabilities::*;
use super::categories::*;
use super::helpers::*;
use super::module::*;
use super::pages::*;
use super::posts::*;
use super::types::*;

use dashmap::DashMap;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone, Default)]
pub struct ContentState {
    pub posts: Arc<DashMap<Uuid, Post>>,
    pub categories: Arc<DashMap<Uuid, Category>>,
    pub pages: Arc<DashMap<Uuid, Page>>,
}
