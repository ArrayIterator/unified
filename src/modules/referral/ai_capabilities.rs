//! referral - AI capabilities.
//!
//! Part of the `referral` module split; see
//! `src/modules/referral.rs`.
//!
//! Declared here, registered once in `init` (see `module.rs`),
//! and run through the AI module's gated runner
//! (`run_ai_capability`): the allowlist gate runs first, PII is
//! redacted unless the capability allows it, and outputs are
//! returned inline, never published or sent by this module.

use crate::modules::ai;

/// AI capabilities this module exposes through the AI module's
/// contract. All are non-PII: inputs are aggregate counts
/// (referrals by stage, reward totals), never candidate names
/// or emails.
pub const AI_CAPABILITIES: &[ai::AiCapability] = &[
    ai::AiCapability {
        id: "summarize_job",
        module_id: "referral",
        name: "Summarize referrals for a job",
        description: "Summarize caller-supplied referral facts for one job (referral counts by pipeline stage, reward totals by status) into plain language. The output is returned inline; it never changes a referral, a reward, or a rule.",
        input_fields: &["job_referral_summary"],
        allows_pii: false,
        use_case: "complete",
    },
    ai::AiCapability {
        id: "review_pipeline_health",
        module_id: "referral",
        name: "Review referral pipeline health",
        description: "Review caller-supplied referral pipeline facts (referral counts by stage across jobs, rewards pending versus paid) and point out where candidates stall. The output is returned inline; it never changes a referral, a stage, or a rule.",
        input_fields: &["pipeline_facts"],
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
        ai::register_ai_capabilities("referral", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("referral");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "referral");
            assert!(!capability.name.is_empty());
            assert!(!capability.description.is_empty());
            assert!(!capability.input_fields.is_empty());
            assert!(!capability.use_case.is_empty());
        }
    }
}
