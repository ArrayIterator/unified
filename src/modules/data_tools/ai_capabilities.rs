//! data_tools - AI capabilities.
//!
//! Declared here, registered once in `init` (see `module.rs`),
//! and run through the AI module's gated runner
//! (`run_ai_capability`): the allowlist gate runs first, PII is
//! redacted unless the capability allows it, and outputs are
//! returned inline, never published or sent by this module.

use crate::modules::ai;

/// AI capabilities this module exposes through the AI module's
/// contract. All are non-PII: inputs are redacted before they
/// reach the model.
pub const AI_CAPABILITIES: &[ai::AiCapability] = &[
    ai::AiCapability {
        id: "explain_cleanup_report",
        module_id: "data_tools",
        name: "Explain a cleanup report",
        description: "Explain a caller-supplied cleanup scan or run report in plain language: what was counted, what was removed, and what to check next.",
        input_fields: &["cleanup_report"],
        allows_pii: false,
        use_case: "complete",
    },
    ai::AiCapability {
        id: "suggest_retention",
        module_id: "data_tools",
        name: "Suggest a retention period",
        description: "Suggest a retention period for a cleanup target from a caller-supplied description of the data and how long it is needed.",
        input_fields: &["target_description"],
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
        ai::register_ai_capabilities("data_tools", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("data_tools");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "data_tools");
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
