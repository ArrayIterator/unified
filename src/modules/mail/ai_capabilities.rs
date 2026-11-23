//! Mail - AI capabilities.
//!
//! Part of the `mail` module split; see `src/modules/mail.rs`.

use crate::modules::ai;

// ---------------------------------------------------------------------------

/// AI capabilities this module exposes through the AI module's
/// contract. The capability is non-PII: inputs are redacted before
/// they reach the model.
pub const AI_CAPABILITIES: &[ai::AiCapability] = &[ai::AiCapability {
    id: "draft",
    module_id: "mail",
    name: "Draft email",
    description: "Draft an email from a subject and bullet points (a string, or a JSON array of points). The output is returned inline; it is never sent by this module.",
    input_fields: &["subject", "bullets"],
    allows_pii: false,
    use_case: "email",
}];
