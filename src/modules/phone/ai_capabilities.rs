//! phone - AI capabilities.
//!
//! Declared here, registered once in `init` (see `module.rs`),
//! and run through the AI module's gated runner
//! (`run_ai_capability`): the allowlist gate runs first, and
//! outputs are returned inline, never published or sent by
//! this module.
//!
//! `answer_caller_question` is the one capability that
//! answers callers: routine questions only, grounded on the
//! tenant's stored IVR menus and auto-response scripts
//! through the `phone.ai.answer_caller_question_requested`
//! hook (answered in `hooks.rs`). The AI answers and may
//! suggest a route or escalation — it never books, charges,
//! transfers, or changes anything on its own; the caller (or
//! an admin through the API) performs any action.
//!
//! `summarize_call` turns a finished call's stored facts and
//! an optional caller-supplied transcript into a short
//! summary, stored on the call log by the summarize handler.
//! `route_caller` suggests where an incoming caller should
//! go (a queue code, a menu code, or escalation) from the
//! tenant's stored routing rules; the suggestion is returned
//! inline and never executed by the AI.

use crate::modules::ai;

/// AI capabilities this module exposes through the AI module's
/// contract.
pub const AI_CAPABILITIES: &[ai::AiCapability] = &[
    ai::AiCapability {
        id: "answer_caller_question",
        module_id: "phone",
        name: "Answer a caller question",
        description: "Answer a routine caller question from the tenant's stored IVR menus and auto-response scripts: the question, the active menus and scripts as grounding, and the caller id. The output is an answer in plain language, optionally with a suggested route (a queue code, a menu code, or escalation to a human agent). The AI never books, charges, transfers, or changes anything on its own.",
        input_fields: &["tenant_id", "caller_id", "question", "call_id", "menu_code", "script_code"],
        allows_pii: false,
        use_case: "complete",
    },
    ai::AiCapability {
        id: "summarize_call",
        module_id: "phone",
        name: "Summarize a call",
        description: "Summarize a finished call from its stored facts (kind, status, outcome, duration, subject, participants, started and ended times) and an optional caller-supplied transcript into a short paragraph. The output is parsed as JSON with a summary field and stored on the call log; it never changes the call.",
        input_fields: &["tenant_id", "call_id", "caller_id", "kind", "status", "outcome", "duration_secs", "subject", "participant_ids", "started_at", "ended_at", "transcript"],
        allows_pii: false,
        use_case: "complete",
    },
    ai::AiCapability {
        id: "route_caller",
        module_id: "phone",
        name: "Suggest a route for a caller",
        description: "Suggest where an incoming caller should go from the tenant's stored routing rules, queues, and IVR menus: the caller id, the time of day, and the matched rule if any. The output is a routing suggestion (target type and target id, or escalation to a human agent) returned inline; it is never executed by the AI.",
        input_fields: &["tenant_id", "caller_id", "now_minutes", "matched_rule", "queues", "ivr_menus"],
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
        ai::register_ai_capabilities("phone", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("phone");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "phone");
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
    fn answer_capability_declares_its_inputs() {
        let capability = AI_CAPABILITIES
            .iter()
            .find(|item| item.id == "answer_caller_question")
            .expect("answer capability is declared");
        assert!(!capability.allows_pii);
        assert!(capability.input_fields.contains(&"question"));
        assert!(capability.input_fields.contains(&"caller_id"));
        assert!(capability.input_fields.contains(&"tenant_id"));
    }

    #[test]
    fn no_capability_allows_pii() {
        // Callers are answered from stored menus and scripts,
        // and summaries from stored call facts — no capability
        // in this module needs PII.
        assert!(AI_CAPABILITIES.iter().all(|item| !item.allows_pii));
    }
}
