//! Elearning - AI capabilities.
//!
//! Part of the `elearning` module split; see `src/modules/elearning.rs`.

use crate::modules::ai;

// ---------------------------------------------------------------------------

/// AI capabilities this module exposes through the AI module's
/// contract. Both are non-PII: lesson content is course material,
/// redacted before it reaches the model; no learner data is ever
/// grounded (no enrollments, attempts, or scores).
pub const AI_CAPABILITIES: &[ai::AiCapability] = &[
    ai::AiCapability {
        id: "draft_quiz",
        module_id: "elearning",
        name: "Draft quiz questions",
        description: "Draft quiz questions for a lesson, grounded in the lesson content and the lesson's existing questions (so drafts do not repeat them). The output is returned inline; it is never stored by this module.",
        input_fields: &["lesson_id", "count", "difficulty"],
        allows_pii: false,
        use_case: "elearning.draft_quiz",
    },
    ai::AiCapability {
        id: "summarize_lesson",
        module_id: "elearning",
        name: "Summarize lesson",
        description: "Summarize one lesson, grounded in the lesson content. The output is returned inline; it is never stored by this module.",
        input_fields: &["lesson_id"],
        allows_pii: false,
        use_case: "elearning.summarize_lesson",
    },
];
