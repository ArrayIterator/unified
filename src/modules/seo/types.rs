//! seo - types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value};
use uuid::Uuid;
use super::{state::*, analysis::*, handlers::*, module::*};



#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
// Types
pub struct SeoSettings {
    pub site_title: String,
    pub site_description: String,
    pub robots: String,
    pub sitemap_enabled: bool,
    pub default_og_image: Option<String>,
}


impl Default for SeoSettings {
    fn default() -> Self {
        Self {
            site_title: "Unified".to_string(),
            site_description: String::new(),
            robots: "index, follow".to_string(),
            sitemap_enabled: true,
            default_og_image: None,
        }
    }
}


#[derive(Debug, Deserialize)]
pub struct UpdateSeoSettingsInput {
    pub site_title: Option<String>,
    pub site_description: Option<String>,
    pub robots: Option<String>,
    pub sitemap_enabled: Option<bool>,
    pub default_og_image: Option<String>,
}


#[derive(Debug, Deserialize)]
pub struct AnalyzeInput {
    pub title: String,
    pub content: String,
    pub keyword: Option<String>,
    pub meta_description: Option<String>,
    pub url: Option<String>,
}


#[derive(Debug, Clone, Serialize)]
pub struct SeoCheck {
    pub name: String,
    pub passed: bool,
    pub weight: u8,
    pub detail: String,
}


#[derive(Debug, Clone, Serialize)]
pub struct SeoAnalysis {
    pub id: Uuid,
    pub post_id: Option<Uuid>,
    pub url: Option<String>,
    pub title: String,
    pub keyword: Option<String>,
    pub score: u8,
    pub grade: String,
    pub word_count: usize,
    pub reading_ease: f64,
    pub keyword_density: f64,
    pub h1_count: usize,
    pub h2_count: usize,
    pub h3_count: usize,
    pub checks: Vec<SeoCheck>,
    pub issues: Vec<String>,
    pub suggestions: Vec<String>,
    pub created_at: DateTime<Utc>,
}


#[derive(Debug, Deserialize)]
pub struct MetaTagsInput {
    pub title: String,
    pub description: Option<String>,
    pub keywords: Option<Vec<String>>,
    pub canonical: Option<String>,
    pub og_image: Option<String>,
    pub og_type: Option<String>,
    pub robots: Option<String>,
    pub twitter_card: Option<String>,
}


#[derive(Debug, Clone, Serialize)]
pub struct MetaTag {
    pub tag: String,
    pub attributes: Value,
}
