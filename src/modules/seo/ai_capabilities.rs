//! seo - AI capabilities.
//!
//! Part of the `seo` module split; see `src/modules/seo.rs`.
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
        id: "draft_meta_description",
        module_id: "seo",
        name: "Draft a meta description",
        description: "Draft a search meta description for a page from its title and page text.",
        input_fields: &["title", "page_text"],
        allows_pii: false,
        use_case: "seo.draft_meta_description",
    },
    ai::AiCapability {
        id: "suggest_keywords",
        module_id: "seo",
        name: "Suggest keywords",
        description: "Suggest keywords for a page from the caller-supplied page content.",
        input_fields: &["content"],
        allows_pii: false,
        use_case: "seo.suggest_keywords",
    },
];

#[cfg(test)]
mod tests {
    use super::AI_CAPABILITIES;
    use crate::modules::ai;

    #[test]
    fn registers_capabilities() {
        ai::register_ai_capabilities("seo", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("seo");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "seo");
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
