//! quality - AI capabilities.
//!
//! Part of the `quality` module split; see `src/modules/quality.rs`.
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
        id: "summarize_inspection",
        module_id: "quality",
        name: "Summarize an inspection",
        description: "Summarize caller-supplied inspection results into pass rate, failures, and items needing rework.",
        input_fields: &["inspection_results"],
        allows_pii: false,
        use_case: "complete",
    },
    ai::AiCapability {
        id: "draft_corrective_action",
        module_id: "quality",
        name: "Draft a corrective action",
        description: "Draft a corrective action (CAPA) description from a failure description and its cause.",
        input_fields: &["failure", "cause"],
        allows_pii: false,
        use_case: "quality.draft_corrective_action",
    },
];

#[cfg(test)]
mod tests {
    use super::AI_CAPABILITIES;
    use crate::modules::ai;

    #[test]
    fn registers_capabilities() {
        ai::register_ai_capabilities("quality", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("quality");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "quality");
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
