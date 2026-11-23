//! seo - state.

use dashmap::DashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::{types::*, analysis::*, handlers::*, module::*};



#[derive(Clone)]
// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------
pub struct SeoState {
    pub settings: Arc<RwLock<SeoSettings>>,
    pub analyses: Arc<DashMap<Uuid, SeoAnalysis>>,
}


impl Default for SeoState {
    fn default() -> Self {
        Self {
            settings: Arc::new(RwLock::new(SeoSettings::default())),
            analyses: Arc::new(DashMap::new()),
        }
    }
}
