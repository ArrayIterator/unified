//! sms_marketing - AI capabilities.
//!
//! Declared here, registered once in `init` (see
//! `module.rs`), and run through the AI module's gated
//! runner (`run_ai_capability`): the allowlist gate runs
//! first, PII is redacted unless the capability allows it,
//! and outputs are returned inline, never published or sent
//! by this module.

use crate::modules::ai;

/// AI capabilities this module exposes through the AI
/// module's contract. The draft capability takes a short
/// brief (offer, audience, tone) and returns a plain-text
/// SMS draft inline; it never sends anything and never
/// sees recipient phone numbers.
pub const AI_CAPABILITIES: &[ai::AiCapability] = &[ai::AiCapability {
    id: "draft_sms",
    module_id: "sms_marketing",
    name: "Draft SMS message",
    description: "Draft a plain-text SMS from a caller-supplied brief (offer, audience, tone, call to action) into short message text with {{variable}} placeholders where personalization belongs. The output is returned inline; it never creates a template, a campaign, or a message, and it never sends anything.",
    input_fields: &["brief"],
    allows_pii: false,
    use_case: "complete",
}];

#[cfg(test)]
mod tests {
    use super::AI_CAPABILITIES;
    use crate::modules::ai;

    #[test]
    fn registers_capabilities() {
        ai::register_ai_capabilities("sms_marketing", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("sms_marketing");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "sms_marketing");
            assert!(!capability.name.is_empty());
            assert!(!capability.description.is_empty());
            assert!(!capability.input_fields.is_empty());
            assert!(!capability.use_case.is_empty());
        }
    }
}
