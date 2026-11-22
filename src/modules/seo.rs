// SEO Manager module.
//
// Site-wide SEO settings, a real on-page analyzer (title length, keyword
// presence and density, heading structure, meta description length, and a
// Flesch reading-ease estimate) that produces a 0-100 score, and a meta
// tag generator. The analyzer also runs automatically whenever the
// `content.published` hook fires.

use crate::core::context::AppContext;
use crate::core::error::UnifiedError;
use crate::core::hooks::HookBus;
use crate::core::json as api_json;
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
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

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone)]
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

// ---------------------------------------------------------------------------
// Analysis engine
// ---------------------------------------------------------------------------

fn count_word_chars(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}

fn count_sentences(text: &str) -> usize {
    let count = text
        .chars()
        .filter(|ch| matches!(ch, '.' | '!' | '?'))
        .count();
    count.max(1)
}

/// Rough syllable estimate: count groups of vowels in each word, with a
/// minimum of one syllable per word.
fn count_syllables(words: &[String]) -> usize {
    let mut total = 0usize;
    for word in words {
        let lower = word.to_lowercase();
        let mut syllables = 0usize;
        let mut previous_vowel = false;
        for ch in lower.chars() {
            let is_vowel = matches!(ch, 'a' | 'e' | 'i' | 'o' | 'u' | 'y');
            if is_vowel && !previous_vowel {
                syllables += 1;
            }
            previous_vowel = is_vowel;
        }
        // A trailing silent "e" usually does not add a spoken syllable.
        if lower.ends_with('e') && syllables > 1 {
            syllables -= 1;
        }
        total += syllables.max(1);
    }
    total
}

/// Flesch reading ease, clamped to 0..=100. Higher is easier to read.
fn reading_ease(words: &[String], sentences: usize, syllables: usize) -> f64 {
    if words.is_empty() {
        return 0.0;
    }
    let words_per_sentence = words.len() as f64 / sentences as f64;
    let syllables_per_word = syllables as f64 / words.len() as f64;
    (206.835 - 1.015 * words_per_sentence - 84.6 * syllables_per_word).clamp(0.0, 100.0)
}

fn count_tag(content_lower: &str, tag: &str) -> usize {
    content_lower.matches(tag).count()
}

fn grade_for(score: u8) -> String {
    match score {
        90..=100 => "A",
        75..=89 => "B",
        60..=74 => "C",
        40..=59 => "D",
        _ => "F",
    }
    .to_string()
}

/// Run every check and return a scored analysis. `post_id` links the
/// analysis to a published post when the analyzer runs from a hook.
pub fn analyze_content(
    title: &str,
    content: &str,
    keyword: Option<&str>,
    meta_description: Option<&str>,
    url: Option<String>,
    post_id: Option<Uuid>,
) -> SeoAnalysis {
    let words = count_word_chars(content);
    let word_count = words.len();
    let sentences = count_sentences(content);
    let syllables = count_syllables(&words);
    let ease = reading_ease(&words, sentences, syllables);
    let content_lower = content.to_lowercase();
    let title_lower = title.to_lowercase();
    let title_len = title.chars().count();

    let keyword_lower = keyword.map(str::to_lowercase);
    let keyword_occurrences = keyword_lower
        .as_ref()
        .map(|keyword| content_lower.matches(keyword.as_str()).count())
        .unwrap_or(0);
    let keyword_density = if word_count == 0 {
        0.0
    } else {
        keyword_occurrences as f64 / word_count as f64 * 100.0
    };
    let keyword_in_title = keyword_lower
        .as_ref()
        .is_some_and(|keyword| title_lower.contains(keyword.as_str()));

    let h1_count = count_tag(&content_lower, "<h1");
    let h2_count = count_tag(&content_lower, "<h2");
    let h3_count = count_tag(&content_lower, "<h3");
    let meta_len = meta_description.map(|text| text.chars().count());

    let mut checks: Vec<SeoCheck> = Vec::new();
    let mut issues: Vec<String> = Vec::new();
    let mut suggestions: Vec<String> = Vec::new();
    let mut score: u8 = 0;

    // Title length: 30-60 characters is the ideal range. (20 points)
    let title_points = if (30..=60).contains(&title_len) {
        20
    } else if (20..=70).contains(&title_len) {
        12
    } else {
        5
    };
    score += title_points;
    let title_passed = (30..=60).contains(&title_len);
    checks.push(SeoCheck {
        name: "title_length".to_string(),
        passed: title_passed,
        weight: 20,
        detail: format!("Title is {title_len} characters long (ideal: 30-60)."),
    });
    if !title_passed {
        issues.push(format!("Title length is {title_len} characters; aim for 30-60."));
        suggestions.push("Rewrite the title to fit 30-60 characters.".to_string());
    }

    // Keyword in title. (15 points)
    match &keyword_lower {
        Some(_) => {
            if keyword_in_title {
                score += 15;
            } else {
                issues.push("Keyword does not appear in the title.".to_string());
                suggestions.push("Include the focus keyword in the title.".to_string());
            }
            checks.push(SeoCheck {
                name: "keyword_in_title".to_string(),
                passed: keyword_in_title,
                weight: 15,
                detail: if keyword_in_title {
                    "Keyword found in the title.".to_string()
                } else {
                    "Keyword missing from the title.".to_string()
                },
            });
        }
        None => {
            checks.push(SeoCheck {
                name: "keyword_in_title".to_string(),
                passed: false,
                weight: 15,
                detail: "No focus keyword was provided.".to_string(),
            });
            suggestions.push("Provide a focus keyword to check title relevance.".to_string());
        }
    }

    // Keyword density: 0.5%-2.5% is the healthy range. (15 points)
    match &keyword_lower {
        Some(_) => {
            let density_points = if (0.5..=2.5).contains(&keyword_density) {
                15
            } else if keyword_occurrences > 0 {
                7
            } else {
                0
            };
            score += density_points;
            checks.push(SeoCheck {
                name: "keyword_density".to_string(),
                passed: (0.5..=2.5).contains(&keyword_density),
                weight: 15,
                detail: format!(
                    "Keyword appears {keyword_occurrences} times ({keyword_density:.2}% density)."
                ),
            });
            if keyword_occurrences == 0 {
                issues.push("Keyword does not appear in the content at all.".to_string());
            } else if !(0.5..=2.5).contains(&keyword_density) {
                issues.push(format!(
                    "Keyword density is {keyword_density:.2}%; aim for 0.5%-2.5%."
                ));
            }
        }
        None => {
            checks.push(SeoCheck {
                name: "keyword_density".to_string(),
                passed: false,
                weight: 15,
                detail: "No focus keyword was provided.".to_string(),
            });
        }
    }

    // Meta description length: 120-160 characters is ideal. (15 points)
    match meta_len {
        Some(len) => {
            let points = if (120..=160).contains(&len) {
                15
            } else if (50..=160).contains(&len) {
                8
            } else {
                3
            };
            score += points;
            checks.push(SeoCheck {
                name: "meta_description_length".to_string(),
                passed: (120..=160).contains(&len),
                weight: 15,
                detail: format!("Meta description is {len} characters long (ideal: 120-160)."),
            });
            if !(120..=160).contains(&len) {
                issues.push(format!(
                    "Meta description is {len} characters; aim for 120-160."
                ));
            }
        }
        None => {
            checks.push(SeoCheck {
                name: "meta_description_length".to_string(),
                passed: false,
                weight: 15,
                detail: "No meta description was provided.".to_string(),
            });
            issues.push("No meta description was provided.".to_string());
            suggestions.push("Write a meta description of 120-160 characters.".to_string());
        }
    }

    // Heading structure: exactly one H1, at least one H2. (15 points)
    let headings_ok = h1_count == 1 && h2_count >= 1;
    score += if headings_ok {
        15
    } else if h1_count == 1 || h2_count >= 1 {
        8
    } else {
        0
    };
    checks.push(SeoCheck {
        name: "heading_structure".to_string(),
        passed: headings_ok,
        weight: 15,
        detail: format!("Found {h1_count} H1, {h2_count} H2, and {h3_count} H3 headings."),
    });
    if h1_count != 1 {
        issues.push(format!(
            "Expected exactly one H1 heading, found {h1_count}."
        ));
        suggestions.push("Use exactly one H1 heading per page.".to_string());
    }
    if h2_count == 0 {
        issues.push("No H2 headings found.".to_string());
        suggestions.push("Break the content into sections with H2 headings.".to_string());
    }

    // Content length: at least 300 words. (10 points)
    let length_points = if word_count >= 300 {
        10
    } else if word_count >= 150 {
        5
    } else {
        0
    };
    score += length_points;
    checks.push(SeoCheck {
        name: "content_length".to_string(),
        passed: word_count >= 300,
        weight: 10,
        detail: format!("Content has {word_count} words (recommended: at least 300)."),
    });
    if word_count < 300 {
        issues.push(format!("Content is only {word_count} words long."));
        suggestions.push("Expand the content to at least 300 words.".to_string());
    }

    // Readability: Flesch reading ease of 60 or higher. (10 points)
    let readability_points = if ease >= 60.0 {
        10
    } else if ease >= 30.0 {
        5
    } else {
        0
    };
    score += readability_points;
    checks.push(SeoCheck {
        name: "readability".to_string(),
        passed: ease >= 60.0,
        weight: 10,
        detail: format!("Flesch reading ease is {ease:.1} (60+ is easy to read)."),
    });
    if ease < 60.0 {
        issues.push(format!("Readability score is {ease:.1}; aim for 60 or higher."));
        suggestions.push("Use shorter sentences and simpler words.".to_string());
    }

    SeoAnalysis {
        id: Uuid::now_v7(),
        post_id,
        url,
        title: title.to_string(),
        keyword: keyword.map(str::to_string),
        score: score.min(100),
        grade: grade_for(score.min(100)),
        word_count,
        reading_ease: (ease * 10.0).round() / 10.0,
        keyword_density: (keyword_density * 100.0).round() / 100.0,
        h1_count,
        h2_count,
        h3_count,
        checks,
        issues,
        suggestions,
        created_at: Utc::now(),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn get_settings(state: web::Data<SeoState>) -> HttpResponse {
    let settings = state
        .settings
        .read()
        .expect("seo settings lock poisoned")
        .clone();
    api_json::success_response(settings, 200)
}

async fn update_settings(
    state: web::Data<SeoState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<UpdateSeoSettingsInput>,
) -> HttpResponse {
    let input = input.into_inner();
    let outcome = (|| -> Result<SeoSettings, UnifiedError> {
        let mut settings = state
            .settings
            .write()
            .expect("seo settings lock poisoned");
        if let Some(title) = input.site_title {
            let title = title.trim();
            if title.is_empty() {
                return Err(UnifiedError::bad_request("site_title must not be empty"));
            }
            if title.chars().count() > 200 {
                return Err(UnifiedError::bad_request(
                    "site_title must be at most 200 characters",
                ));
            }
            settings.site_title = title.to_string();
        }
        if let Some(description) = input.site_description {
            if description.chars().count() > 500 {
                return Err(UnifiedError::bad_request(
                    "site_description must be at most 500 characters",
                ));
            }
            settings.site_description = description;
        }
        if let Some(robots) = input.robots {
            validate_robots(&robots)?;
            settings.robots = robots;
        }
        if let Some(sitemap_enabled) = input.sitemap_enabled {
            settings.sitemap_enabled = sitemap_enabled;
        }
        if input.default_og_image.is_some() {
            settings.default_og_image = input.default_og_image;
        }
        Ok(settings.clone())
    })();
    match outcome {
        Ok(settings) => {
            ctx.hooks.emit(
                "seo.settings.updated",
                json!({
                    "site_title": settings.site_title,
                    "robots": settings.robots,
                    "sitemap_enabled": settings.sitemap_enabled,
                }),
            );
            api_json::success_response(settings, 200)
        }
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

const ROBOT_TOKENS: &[&str] = &[
    "index",
    "noindex",
    "follow",
    "nofollow",
    "noarchive",
    "nosnippet",
    "noimageindex",
    "notranslate",
    "none",
    "all",
];

fn validate_robots(robots: &str) -> Result<(), UnifiedError> {
    let robots = robots.trim();
    if robots.is_empty() {
        return Err(UnifiedError::bad_request("robots must not be empty"));
    }
    for token in robots.split(',') {
        let token = token.trim().to_lowercase();
        if !ROBOT_TOKENS.contains(&token.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "unknown robots token \"{token}\""
            )));
        }
    }
    Ok(())
}

async fn analyze(
    state: web::Data<SeoState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<AnalyzeInput>,
) -> HttpResponse {
    let input = input.into_inner();
    if input.title.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("title must not be empty"),
            ctx.is_debug(),
        );
    }
    if input.content.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("content must not be empty"),
            ctx.is_debug(),
        );
    }
    if let Some(keyword) = &input.keyword {
        if keyword.trim().is_empty() {
            return api_json::error_response(
                &UnifiedError::bad_request("keyword must not be empty when provided"),
                ctx.is_debug(),
            );
        }
    }
    let analysis = analyze_content(
        input.title.trim(),
        &input.content,
        input.keyword.as_deref().map(str::trim),
        input.meta_description.as_deref(),
        input.url,
        None,
    );
    state.analyses.insert(analysis.id, analysis.clone());
    ctx.hooks.emit(
        "seo.analyzed",
        json!({
            "id": analysis.id.to_string(),
            "title": analysis.title,
            "score": analysis.score,
            "grade": analysis.grade,
        }),
    );
    api_json::success_response(analysis, 200)
}

async fn list_analyses(
    state: web::Data<SeoState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let search = params.search.clone();
    let mut items: Vec<SeoAnalysis> = state
        .analyses
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|analysis| {
            if let Some(needle) = &search {
                let needle = needle.to_lowercase();
                return analysis.title.to_lowercase().contains(&needle)
                    || analysis
                        .keyword
                        .as_deref()
                        .is_some_and(|keyword| keyword.to_lowercase().contains(&needle));
            }
            true
        })
        .collect();
    let descending = params.normalized().order == "desc";
    items.sort_by(|a, b| {
        if descending {
            b.created_at.cmp(&a.created_at)
        } else {
            a.created_at.cmp(&b.created_at)
        }
    });
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<SeoAnalysis> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

async fn get_analysis(state: web::Data<SeoState>, id: web::Path<Uuid>) -> HttpResponse {
    match state.analyses.get(&id.into_inner()) {
        Some(analysis) => api_json::success_response(analysis.value().clone(), 200),
        None => api_json::error_response(&UnifiedError::not_found("analysis not found"), false),
    }
}

fn escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

async fn generate_meta_tags(input: web::Json<MetaTagsInput>) -> HttpResponse {
    let input = input.into_inner();
    let title = input.title.trim();
    if title.is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("title must not be empty"),
            false,
        );
    }
    let mut warnings: Vec<String> = Vec::new();
    let title_len = title.chars().count();
    if !(30..=60).contains(&title_len) {
        warnings.push(format!(
            "Title is {title_len} characters; 30-60 is recommended for search results."
        ));
    }
    let description = input.description.as_deref().map(str::trim);
    if let Some(description) = description {
        let len = description.chars().count();
        if len > 160 {
            warnings.push(format!(
                "Description is {len} characters; search engines usually truncate past 160."
            ));
        }
        if len < 50 {
            warnings.push(format!(
                "Description is only {len} characters; 120-160 is recommended."
            ));
        }
    } else {
        warnings.push("No description provided; add one of 120-160 characters.".to_string());
    }
    let canonical = match &input.canonical {
        Some(raw) => match url::Url::parse(raw.trim()) {
            Ok(parsed) => Some(parsed.to_string()),
            Err(_) => {
                return api_json::error_response(
                    &UnifiedError::bad_request("canonical must be a valid URL"),
                    false,
                );
            }
        },
        None => None,
    };
    let og_image = input.og_image.as_deref().map(str::trim).filter(|value| {
        if value.is_empty() {
            return false;
        }
        match url::Url::parse(value) {
            Ok(_) => true,
            Err(_) => false,
        }
    });
    if input.og_image.as_deref().is_some_and(|value| !value.trim().is_empty()) && og_image.is_none()
    {
        warnings.push("og_image is not a valid URL and was omitted.".to_string());
    }
    let robots = input.robots.as_deref().unwrap_or("index, follow");
    if let Err(error) = validate_robots(robots) {
        return api_json::error_response(&error, false);
    }

    let mut tags: Vec<MetaTag> = Vec::new();
    let mut html_lines: Vec<String> = Vec::new();
    let mut push_tag = |tag: &str, attributes: Value, html: String| {
        tags.push(MetaTag {
            tag: tag.to_string(),
            attributes,
        });
        html_lines.push(html);
    };

    push_tag(
        "title",
        json!({ "text": title }),
        format!("<title>{}</title>", escape_attr(title)),
    );
    if let Some(description) = description {
        push_tag(
            "meta",
            json!({ "name": "description", "content": description }),
            format!(
                "<meta name=\"description\" content=\"{}\">",
                escape_attr(description)
            ),
        );
    }
    push_tag(
        "meta",
        json!({ "name": "robots", "content": robots }),
        format!("<meta name=\"robots\" content=\"{}\">", escape_attr(robots)),
    );
    if let Some(keywords) = &input.keywords {
        let joined = keywords
            .iter()
            .map(|keyword| keyword.trim())
            .filter(|keyword| !keyword.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        if !joined.is_empty() {
            push_tag(
                "meta",
                json!({ "name": "keywords", "content": joined }),
                format!("<meta name=\"keywords\" content=\"{}\">", escape_attr(&joined)),
            );
        }
    }
    if let Some(canonical) = &canonical {
        push_tag(
            "link",
            json!({ "rel": "canonical", "href": canonical }),
            format!("<link rel=\"canonical\" href=\"{}\">", escape_attr(canonical)),
        );
    }
    let og_type = input.og_type.as_deref().unwrap_or("website");
    push_tag(
        "meta",
        json!({ "property": "og:title", "content": title }),
        format!(
            "<meta property=\"og:title\" content=\"{}\">",
            escape_attr(title)
        ),
    );
    push_tag(
        "meta",
        json!({ "property": "og:type", "content": og_type }),
        format!("<meta property=\"og:type\" content=\"{}\">", escape_attr(og_type)),
    );
    if let Some(description) = description {
        push_tag(
            "meta",
            json!({ "property": "og:description", "content": description }),
            format!(
                "<meta property=\"og:description\" content=\"{}\">",
                escape_attr(description)
            ),
        );
    }
    if let Some(image) = og_image {
        push_tag(
            "meta",
            json!({ "property": "og:image", "content": image }),
            format!("<meta property=\"og:image\" content=\"{}\">", escape_attr(image)),
        );
    }
    let twitter_card = input.twitter_card.as_deref().unwrap_or("summary_large_image");
    push_tag(
        "meta",
        json!({ "name": "twitter:card", "content": twitter_card }),
        format!(
            "<meta name=\"twitter:card\" content=\"{}\">",
            escape_attr(twitter_card)
        ),
    );
    push_tag(
        "meta",
        json!({ "name": "twitter:title", "content": title }),
        format!(
            "<meta name=\"twitter:title\" content=\"{}\">",
            escape_attr(title)
        ),
    );
    if let Some(description) = description {
        push_tag(
            "meta",
            json!({ "name": "twitter:description", "content": description }),
            format!(
                "<meta name=\"twitter:description\" content=\"{}\">",
                escape_attr(description)
            ),
        );
    }

    api_json::success_response(
        json!({
            "tags": tags,
            "html": html_lines.join("\n"),
            "warnings": warnings,
        }),
        200,
    )
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct SeoModule {
    state: SeoState,
}

impl SeoModule {
    pub fn new() -> Self {
        Self {
            state: SeoState::default(),
        }
    }
}

impl Default for SeoModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for SeoModule {
    fn id(&self) -> &'static str {
        "seo"
    }

    fn name(&self) -> &'static str {
        "SEO Manager"
    }

    fn description(&self) -> &'static str {
        "SEO settings, on-page analysis with scoring, and meta tag generation."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["content"]
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Auto-analyze every published post or page so the SEO overview
        // stays in sync with the content module.
        let state = self.state.clone();
        bus.register("content.published", "seo", Arc::new(move |ctx| {
            let title = ctx
                .get("title")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let content = ctx
                .get("content")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            if title.is_empty() || content.is_empty() {
                return;
            }
            let post_id = ctx
                .get("id")
                .and_then(|value| value.as_str())
                .and_then(|raw| raw.parse::<Uuid>().ok());
            let analysis = analyze_content(&title, &content, None, None, None, post_id);
            state.analyses.insert(analysis.id, analysis);
        }));
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/seo")
                .app_data(state)
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/analyze", web::post().to(analyze))
                .route("/meta", web::post().to(generate_meta_tags))
                .route("/analyses", web::get().to(list_analyses))
                .route("/analyses/{id}", web::get().to(get_analysis)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SeoModule::new()));
}
