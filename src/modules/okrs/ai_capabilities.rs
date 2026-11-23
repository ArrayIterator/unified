//! okrs - AI capabilities.
//!
//! Part of the `okrs` module split; see `src/modules/okrs.rs`.
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
        id: "draft_objective",
        module_id: "okrs",
        name: "Draft an objective",
        description: "Draft an objective with key results from a goal statement and the cycle it belongs to. The output is returned inline; nothing is created by this module.",
        input_fields: &["goal", "cycle"],
        allows_pii: false,
        use_case: "okrs.draft_objective",
    },
    ai::AiCapability {
        id: "analyze_progress",
        module_id: "okrs",
        name: "Analyze progress",
        description: "Analyze caller-supplied check-in notes for one objective and return progress signals and risks.",
        input_fields: &["check_ins"],
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
        ai::register_ai_capabilities("okrs", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("okrs");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "okrs");
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
