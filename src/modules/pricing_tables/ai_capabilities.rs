//! pricing_tables - AI capabilities.
//!
//! Part of the `pricing_tables` module split; see `src/modules/pricing_tables.rs`.
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
        id: "draft_tier_description",
        module_id: "pricing_tables",
        name: "Draft a tier description",
        description: "Draft a public description of a pricing tier from its name and feature summary.",
        input_fields: &["name", "features"],
        allows_pii: false,
        use_case: "content",
    },
    ai::AiCapability {
        id: "draft_table_copy",
        module_id: "pricing_tables",
        name: "Draft table copy",
        description: "Draft the headline and supporting copy for a pricing table from its name and audience.",
        input_fields: &["name", "audience"],
        allows_pii: false,
        use_case: "content",
    },
];

#[cfg(test)]
mod tests {
    use super::AI_CAPABILITIES;
    use crate::modules::ai;

    #[test]
    fn registers_capabilities() {
        ai::register_ai_capabilities("pricing_tables", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("pricing_tables");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "pricing_tables");
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
