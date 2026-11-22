-- Roll back 2026-11-22-000039_advisor_risk, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS risk_alerts;
DROP TABLE IF EXISTS risk_checklist_instance_items;
DROP TABLE IF EXISTS risk_checklist_instances;
DROP TABLE IF EXISTS risk_checklist_template_items;
DROP TABLE IF EXISTS risk_checklist_templates;
DROP TABLE IF EXISTS risk_mitigation_tasks;
DROP TABLE IF EXISTS risk_mitigation_plans;
DROP TABLE IF EXISTS risks;
DROP TABLE IF EXISTS advisor_rule_runs;
DROP TABLE IF EXISTS advisor_suggestions;
