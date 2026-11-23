-- Roll back 2026-11-22-000089_approvals, in reverse dependency
-- order. No other migration depends on these tables.

DROP TRIGGER IF EXISTS trg_approval_decisions_no_update ON approval_decisions;
DROP TRIGGER IF EXISTS trg_approval_decisions_no_delete ON approval_decisions;
DROP FUNCTION IF EXISTS forbid_approval_decision_mutation();
DROP TABLE IF EXISTS approval_escalations;
DROP TABLE IF EXISTS approval_delegations;
DROP TABLE IF EXISTS approval_decisions;
DROP TABLE IF EXISTS approval_requests;
DROP TABLE IF EXISTS approval_steps;
DROP TABLE IF EXISTS approval_definitions;
