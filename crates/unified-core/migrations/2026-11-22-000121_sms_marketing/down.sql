-- SMS marketing module (`src/modules/sms_marketing/`): drop the
-- persistence tables in reverse dependency order.
-- The in-memory DashMap store is authoritative; dropping
-- these tables only removes the persistence targets.

DROP TABLE IF EXISTS sms_audit;
DROP TABLE IF EXISTS sms_rate_limits;
DROP TABLE IF EXISTS sms_opt_out_keywords;
DROP TABLE IF EXISTS sms_opt_outs;
DROP TABLE IF EXISTS sms_messages;
DROP TABLE IF EXISTS sms_campaigns;
DROP TABLE IF EXISTS sms_contacts;
DROP TABLE IF EXISTS sms_contact_lists;
DROP TABLE IF EXISTS sms_templates;
DROP TABLE IF EXISTS sms_providers;
