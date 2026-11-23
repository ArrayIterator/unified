-- Social marketing module (`src/modules/social_marketing/`): drop the
-- persistence tables in reverse dependency order.
-- The in-memory DashMap store is authoritative; dropping
-- these tables only removes the persistence targets.

DROP TABLE IF EXISTS social_audit;
DROP TABLE IF EXISTS social_metrics;
DROP TABLE IF EXISTS social_campaigns;
DROP TABLE IF EXISTS social_posts;
DROP TABLE IF EXISTS social_accounts;
DROP TABLE IF EXISTS social_networks;
