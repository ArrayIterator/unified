//! exams - settings.

use serde::{Deserialize, Serialize};
use super::{constants::*, types::*, module::*, helpers::*, inputs::*, handlers_banks::*, handlers_questions::*, handlers_exams::*, handlers_attempts::*, handlers_grading::*, handlers_results::*, handlers_ai::*, handlers_settings::*, hooks::*};



#[derive(Debug, Clone, Serialize, Deserialize)]
// Configurable settings (admin-editable; defaults are defaults only)
/// All behaviour-shaping lists and defaults of this module. An
/// administrator edits these through `PUT /settings`; every
/// validation and default in this file reads the live settings,
/// never a constant list.
pub struct ExamsSettings {
    /// Allowed question types (for example multiple_choice_single,
    /// multiple_choice_multiple, true_false, short_answer, essay).
    pub question_types: Vec<String>,
    /// Allowed difficulty levels (for example easy, medium, hard).
    pub difficulty_levels: Vec<String>,
    /// Allowed question categories.
    pub categories: Vec<String>,
    /// Allowed proctoring signal types.
    pub proctor_signal_types: Vec<String>,
    /// Default pass mark as a percentage (0-100).
    pub default_pass_mark: f64,
    /// Default exam duration in minutes.
    pub default_duration_minutes: i64,
    /// Default number of attempts allowed per student.
    pub default_attempts_allowed: u32,
    /// Default result visibility after grading.
    pub default_show_results: String,
    /// Number of buckets in the score distribution.
    pub distribution_buckets: u32,
    /// Whether short answers match case-insensitively by default.
    pub short_answer_case_insensitive: bool,
    /// Whether certificates are requested on pass by default.
    pub certificate_on_pass: bool,
    /// Maximum questions an exam may hold.
    pub max_questions_per_exam: usize,
    /// Maximum options a question may hold.
    pub max_options_per_question: usize,
}


impl Default for ExamsSettings {
    fn default() -> Self {
        Self {
            question_types: vec![
                "multiple_choice_single".to_string(),
                "multiple_choice_multiple".to_string(),
                "true_false".to_string(),
                "short_answer".to_string(),
                "essay".to_string(),
            ],
            difficulty_levels: vec![
                "easy".to_string(),
                "medium".to_string(),
                "hard".to_string(),
            ],
            categories: vec![
                "general".to_string(),
                "mathematics".to_string(),
                "language".to_string(),
                "science".to_string(),
                "history".to_string(),
                "technology".to_string(),
            ],
            proctor_signal_types: vec![
                "tab_switch".to_string(),
                "window_blur".to_string(),
                "navigation_away".to_string(),
                "copy_attempt".to_string(),
                "paste_attempt".to_string(),
                "fullscreen_exit".to_string(),
                "idle_timeout".to_string(),
                "other".to_string(),
            ],
            default_pass_mark: 60.0,
            default_duration_minutes: 60,
            default_attempts_allowed: 1,
            default_show_results: "after_grading".to_string(),
            distribution_buckets: 10,
            short_answer_case_insensitive: true,
            certificate_on_pass: true,
            max_questions_per_exam: MAX_QUESTIONS_PER_EXAM,
            max_options_per_question: MAX_OPTIONS_PER_QUESTION,
        }
    }
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateExamsSettings {
    pub question_types: Option<Vec<String>>,
    pub difficulty_levels: Option<Vec<String>>,
    pub categories: Option<Vec<String>>,
    pub proctor_signal_types: Option<Vec<String>>,
    pub default_pass_mark: Option<f64>,
    pub default_duration_minutes: Option<i64>,
    pub default_attempts_allowed: Option<u32>,
    pub default_show_results: Option<String>,
    pub distribution_buckets: Option<u32>,
    pub short_answer_case_insensitive: Option<bool>,
    pub certificate_on_pass: Option<bool>,
    pub max_questions_per_exam: Option<usize>,
    pub max_options_per_question: Option<usize>,
}
