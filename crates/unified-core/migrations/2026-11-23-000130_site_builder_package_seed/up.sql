-- Seed: the example "Site Builder SaaS" package (task 353).
--
-- The site builder is a PACKAGE, not a forced tenant feature: a
-- tenant gets the builder (and its own site on its own domain) only
-- when its package includes the `site-builder` module entry. The
-- control-plane system home page is NOT tenant-gated and does not
-- come from this package.
--
-- Example data only, like the Online Exam seed in migration
-- 000102: an admin can edit or delete it like any other package.
-- No pricing is invented here (product_id stays NULL; pricing lives
-- on the store product, if one is ever attached). The id matches the
-- Rust seed row in `src/modules/saas_packages/seed.rs` exactly
-- (`SEED_SITE_BUILDER_PACKAGE_ID`).

INSERT INTO saas_packages (id, slug, name, description, status, current_version, product_id, preset_settings, wizard_steps, created_by, vendor_id)
VALUES (
    '20000000-0000-4000-8000-000000000002',
    'site-builder-saas',
    'Site Builder SaaS',
    'A website on its own domain, built with the GrapeJS site builder: pages of ordered blocks, a tenant subdomain or custom domain with SSL, and the content module for posts and pages.',
    'published',
    1,
    NULL,
    '{"welcome_email_enabled": true, "default_language": "en"}'::jsonb,
    '["business_type", "review_modules", "apply", "settings", "done"]'::jsonb,
    'system',
    NULL
)
ON CONFLICT (id) DO NOTHING;

INSERT INTO saas_package_modules (package_id, version, module_id, required, sort_order)
VALUES
    ('20000000-0000-4000-8000-000000000002', 1, 'site-builder', TRUE, 0),
    ('20000000-0000-4000-8000-000000000002', 1, 'content', TRUE, 1),
    ('20000000-0000-4000-8000-000000000002', 1, 'theme', TRUE, 2),
    ('20000000-0000-4000-8000-000000000002', 1, 'domains', TRUE, 3)
ON CONFLICT (package_id, version, module_id) DO NOTHING;
