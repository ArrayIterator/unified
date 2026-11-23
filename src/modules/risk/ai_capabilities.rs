//! risk - AI capabilities.
//!
//! Part of the `risk` module split; see `src/modules/risk.rs`.
//!
//! Declared here, registered once in `init` (see `module.rs`
//! or `routes.rs`), and run through the AI module's gated runner
//! (`run_ai_capability`): the allowlist gate runs first, PII is
//! redacted unless the capability allows it, and outputs are
//! returned inline, never published or sent by this module.

use crate::modules::ai;

/// AI capabilities this module exposes through the AI module's
/// contract. All are non-PII: inputs are redacted before they
/// reach the model.
pub const AI_CAPABILITIES: &[ai::AiCapability] = &[
    ai::AiCapability {
        id: "draft_mitigation_plan",
        module_id: "risk",
        name: "Draft a mitigation plan",
        description: "Draft a mitigation plan for a risk from the caller-supplied risk description.",
        input_fields: &["risk_description"],
        allows_pii: false,
        use_case: "risk.draft_mitigation_plan",
    },
    ai::AiCapability {
        id: "summarize_risk",
        module_id: "risk",
        name: "Summarize risks",
        description: "Summarize caller-supplied risk entries into the highest-scored risks and their bands.",
        input_fields: &["risks"],
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
        ai::register_ai_capabilities("risk", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("risk");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "risk");
            assert!(!capability.name.is_empty());
            assert!(!capability.description.is_empty());
            assert!(!capability.input_fields.is_empty());
            assert!(!capability.use_case.is_empty());
        }
    }

    #[test]
    fn capability_ids_are_unique() {
        let mut ids: Vec<&str> = AI_CAPABILITIES.iter().map(|item| item.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), AI_CAPABILITIES.len());
    }
}
