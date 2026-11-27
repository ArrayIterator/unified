//! kyc - AI capabilities.
//!
//! Part of the `kyc` module split; see `src/modules/kyc.rs`.
//!
//! Declared here, registered once in `init` (see `module.rs`),
//! and run through the AI module's gated runner
//! (`run_ai_capability`): the allowlist gate runs first, and
//! outputs are returned inline, never published or sent by
//! this module.
//!
//! `verify_identity_documents` is the one capability that
//! allows PII: identity verification cannot work on redacted
//! identity data - the model must see the document metadata
//! and the subject's declared details to run the checks the
//! tenant declared in its default context. The input it
//! receives is metadata only (document references and content
//! hashes, never file bytes); the resolved default context
//! (`visible_fields`) and the active extra instructions reach
//! the model as grounding through the
//! `kyc.ai.verify_identity_documents_requested` hook, answered
//! in `hooks.rs`. The verdict the model returns is stored on
//! the case and feeds the human review queue; it never
//! approves a case on its own.

use crate::modules::ai;

/// AI capabilities this module exposes through the AI module's
/// contract.
pub const AI_CAPABILITIES: &[ai::AiCapability] = &[
    ai::AiCapability {
        id: "verify_identity_documents",
        module_id: "kyc",
        name: "Verify identity documents",
        description: "Verify the identity documents linked to a KYC case against the tenant's active default verification context and extra instructions: the checks declared in that context, run over the linked document metadata. The output is a verdict (verified, uncertain, or rejected) with confidence and reasons, stored on the case for human review; it never approves a case on its own.",
        input_fields: &["case_id", "subject_type", "documents", "checks", "instructions"],
        allows_pii: true,
        use_case: "complete",
    },
    ai::AiCapability {
        id: "summarize_case",
        module_id: "kyc",
        name: "Summarize a KYC case",
        description: "Summarize caller-supplied KYC case progress (status, verdict, risk level, outstanding documents) into plain language. The output is returned inline; it never changes a case.",
        input_fields: &["case_summary"],
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
        ai::register_ai_capabilities("kyc", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("kyc");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "kyc");
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

    #[test]
    fn verification_capability_declares_its_inputs() {
        let capability = AI_CAPABILITIES
            .iter()
            .find(|item| item.id == "verify_identity_documents")
            .expect("verification capability is declared");
        assert!(capability.allows_pii);
        assert!(capability.input_fields.contains(&"case_id"));
        assert!(capability.input_fields.contains(&"documents"));
        assert!(capability.input_fields.contains(&"checks"));
        assert!(capability.input_fields.contains(&"instructions"));
    }
}
