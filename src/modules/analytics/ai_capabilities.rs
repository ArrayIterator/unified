//! analytics - AI capabilities.
//!
//! Part of the `analytics` module split; see `src/modules/analytics.rs`.
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
        id: "summarize_report",
        module_id: "analytics",
        name: "Summarize a report",
        description: "Summarize caller-supplied analytics figures into key takeaways and notable changes. Only aggregate figures are accepted.",
        input_fields: &["report_data"],
        allows_pii: false,
        use_case: "complete",
    },
    ai::AiCapability {
        id: "draft_insight",
        module_id: "analytics",
        name: "Draft an insight",
        description: "Draft a plain-language insight from caller-supplied metric values and the period they cover.",
        input_fields: &["metrics", "period"],
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
        ai::register_ai_capabilities("analytics", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("analytics");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "analytics");
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
