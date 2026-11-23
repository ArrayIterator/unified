//! plm - AI capabilities.
//!
//! Part of the `plm` module split; see `src/modules/plm.rs`.
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
        id: "draft_change_summary",
        module_id: "plm",
        name: "Draft a version change summary",
        description: "Draft a change summary for a new product revision from the previous revision notes and the list of changes. The output is returned inline; it is never saved by this module.",
        input_fields: &["previous_summary", "changes"],
        allows_pii: false,
        use_case: "content",
    },
    ai::AiCapability {
        id: "assess_eco_impact",
        module_id: "plm",
        name: "Assess ECO impact",
        description: "Assess the impact of an engineering change order from its description and affected products, as a draft impact assessment for a human reviewer. The output is returned inline; it never changes an ECO.",
        input_fields: &["description", "affected_products"],
        allows_pii: false,
        use_case: "complete",
    },
    ai::AiCapability {
        id: "summarize_version_history",
        module_id: "plm",
        name: "Summarize version history",
        description: "Summarize the revision history of a product from its version records into plain language. The output is returned inline; it never changes a version.",
        input_fields: &["versions"],
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
        ai::register_ai_capabilities("plm", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("plm");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "plm");
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
