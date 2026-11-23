// SEO Manager module.

// Split into one file per concern under `src/modules/seo/`;
// this root only declares the submodules and re-exports the
// public API so every item stays reachable as `crate::modules::seo::Item`.

pub mod ai_capabilities;
pub mod types;
pub mod state;
pub mod analysis;
mod handlers;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::{AnalyzeInput, MetaTag, MetaTagsInput, SeoAnalysis, SeoCheck, SeoSettings, UpdateSeoSettingsInput};
pub use self::state::{SeoState};
pub use self::analysis::{analyze_content};
pub(crate) use self::analysis::{count_sentences, count_syllables, count_tag, count_word_chars, grade_for, reading_ease};
pub(crate) use self::handlers::{ROBOT_TOKENS, analyze, escape_attr, generate_meta_tags, get_analysis, get_settings, list_analyses, update_settings, validate_robots};
pub use self::module::{SeoModule, register};
