//! social_marketing - AI capabilities.
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
/// brief (topic, audience, tone) and returns a post draft
/// inline; it never creates a post or a campaign, and it
/// never publishes anything. The summarize capability
/// summarizes a campaign from its stored facts (post counts
/// and summed metrics); it never sees account handles or
/// credentials.
pub const AI_CAPABILITIES: &[ai::AiCapability] = &[
    ai::AiCapability {
        id: "draft_social_post",
        module_id: "social_marketing",
        name: "Draft social post",
        description: "Draft a social post from a caller-supplied brief (topic, audience, tone, call to action) into short post text with hashtags where they belong. The output is returned inline; it never creates a post, a campaign, or an account, and it never publishes anything.",
        input_fields: &["brief"],
        allows_pii: false,
        use_case: "complete",
    },
    ai::AiCapability {
        id: "summarize_campaign",
        module_id: "social_marketing",
        name: "Summarize social campaign",
        description: "Summarize a social campaign from its stored facts (post counts by status, summed metrics, engagement rate, goal progress). The output is returned inline; it never edits the campaign and never sees account handles or credentials.",
        input_fields: &["campaign_id"],
        allows_pii: false,
        use_case: "complete",
    },
];

#[cfg(test)]
mod tests {
    use super::AI_CAPABILITIES;
    use crate::modules::ai;

    #[test]
    fn registers_capabilities() {
        ai::register_ai_capabilities("social_marketing", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("social_marketing");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "social_marketing");
            assert!(!capability.name.is_empty());
            assert!(!capability.description.is_empty());
            assert!(!capability.input_fields.is_empty());
            assert!(!capability.use_case.is_empty());
        }
    }
}
