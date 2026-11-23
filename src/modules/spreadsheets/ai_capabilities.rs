//! spreadsheets - AI capabilities.
//!
//! Part of the `spreadsheets` module split; see
//! `src/modules/spreadsheets.rs`.
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
        id: "draft_formula",
        module_id: "spreadsheets",
        name: "Draft a formula",
        description: "Draft a spreadsheet formula from a plain-language description of the calculation, using only the registered function names. The output is returned inline; it is never written to a sheet by this module.",
        input_fields: &["description", "context"],
        allows_pii: false,
        use_case: "content",
    },
    ai::AiCapability {
        id: "summarize_workbook",
        module_id: "spreadsheets",
        name: "Summarize workbook",
        description: "Summarize caller-supplied workbook facts (title, sheets, cell counts) into plain language. The output is returned inline; it never changes a workbook.",
        input_fields: &["workbook_summary"],
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
        ai::register_ai_capabilities("spreadsheets", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("spreadsheets");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "spreadsheets");
            assert!(!capability.name.is_empty());
            assert!(!capability.description.is_empty());
            assert!(!capability.input_fields.is_empty());
            assert!(!capability.use_case.is_empty());
        }
    }
}
