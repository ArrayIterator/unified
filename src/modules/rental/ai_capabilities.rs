//! rental - AI capabilities.
//!
//! Part of the `rental` module split; see `src/modules/rental.rs`.
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
        id: "draft_listing",
        module_id: "rental",
        name: "Draft a rental listing",
        description: "Draft a rental listing title and description from a product name and key features. The output is returned inline; it is never published by this module.",
        input_fields: &["product_name", "key_features"],
        allows_pii: false,
        use_case: "content",
    },
    ai::AiCapability {
        id: "summarize_rental_terms",
        module_id: "rental",
        name: "Summarize rental terms",
        description: "Summarize caller-supplied rental terms (periods, deposit, late fees) into plain language. The output is returned inline; it never changes a listing.",
        input_fields: &["terms"],
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
        ai::register_ai_capabilities("rental", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("rental");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "rental");
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
