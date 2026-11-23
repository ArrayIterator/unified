//! esg - AI capabilities.
//!
//! Part of the `esg` module split; see
//! `src/modules/esg.rs`.
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
pub const AI_CAPABILITIES: &[ai::AiCapability] = &[ai::AiCapability {
    id: "summarize_period",
    module_id: "esg",
    name: "Summarize reporting period",
    description: "Summarize caller-supplied ESG period facts (period code, metrics by category, completion, target attainment) into plain language. The output is returned inline; it never changes a period, an entry, or a target.",
    input_fields: &["period_summary"],
    allows_pii: false,
    use_case: "complete",
}];

#[cfg(test)]
mod tests {
    use super::AI_CAPABILITIES;
    use crate::modules::ai;

    #[test]
    fn registers_capabilities() {
        ai::register_ai_capabilities("esg", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("esg");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "esg");
            assert!(!capability.name.is_empty());
            assert!(!capability.description.is_empty());
            assert!(!capability.input_fields.is_empty());
            assert!(!capability.use_case.is_empty());
        }
    }
}
