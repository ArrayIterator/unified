//! Academic - AI capabilities.
//!
//! Part of the `academic` module split; see `src/modules/academic.rs`.

use crate::modules::ai;


/// AI capabilities this module exposes through the AI module's
/// contract. The capability is non-PII: the grounding summary is
/// anonymised (aggregates only - no user id, no student number, no
/// names), and the input is redacted before it reaches the model.
pub const AI_CAPABILITIES: &[ai::AiCapability] = &[ai::AiCapability {
    id: "summarize_progress",
    module_id: "academic",
    name: "Summarize student progress",
    description: "Summarize one student's academic progress from anonymised grade and attendance aggregates (GPA, credits, standing, per-subject average scores, attendance rate). Never receives raw personal data or student identities.",
    input_fields: &["student_id"],
    allows_pii: false,
    use_case: "academic.summarize_progress",
}];
