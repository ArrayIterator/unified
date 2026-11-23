-- 000135: Cloud package + apps (task 377).
--
-- Three things, one migration:
--
--   1. `cloud_apps` — the deployed-application records (in-memory
--      DashMap is authoritative in this round; the table exists for
--      future persistence). `plan_id` / `connection_id` carry NO
--      foreign keys on purpose: plans and connections are
--      in-memory-authoritative, so an FK would reject valid
--      references to rows that only exist in memory.
--   2. The seeded "Cloud SaaS" package (`cloud-saas`,
--      `20000000-0000-4000-8000-000000000003`): the cloud module as a
--      sellable product (site-builder pattern — see migration
--      000130). Modules: `cloud` (required) + `domains` (required).
--      No pricing is invented (product_id stays NULL).
--   3. Capability updates for the stored registry rows: Docker
--      gains `create`, Kubernetes gains `create` / `scale` /
--      `delete`, and the new WHMCS-alignment flags (`suspend`,
--      `renew`, `change_password`, `usage`) are set per kind from
--      the updated seed JSON. The 000127 INSERT block is generated —
--      never hand-edit it; capability changes on stored rows go here.
--      Unknown capability keys read as false at runtime, so rows
--      this UPDATE misses stay valid (just without the new flags).

CREATE TABLE IF NOT EXISTS cloud_apps (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    plan_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('docker', 'kubernetes')),
    resource_id TEXT NOT NULL CHECK (char_length(resource_id) BETWEEN 1 AND 500),
    image TEXT NOT NULL CHECK (char_length(image) BETWEEN 1 AND 500),
    status TEXT NOT NULL CHECK (status IN ('creating', 'running', 'stopped', 'failed')),
    url TEXT CHECK (url IS NULL OR char_length(url) <= 1000),
    env JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_cloud_apps_tenant_plan
    ON cloud_apps (tenant_id, plan_id);
CREATE INDEX IF NOT EXISTS idx_cloud_apps_tenant_connection
    ON cloud_apps (tenant_id, connection_id);

INSERT INTO saas_packages (id, slug, name, description, status, current_version, product_id, preset_settings, wizard_steps, created_by, vendor_id)
VALUES (
    '20000000-0000-4000-8000-000000000003',
    'cloud-saas',
    'Cloud SaaS',
    'Cloud hosting, provider connectors, containers, and app deployment: hosting plans and accounts, cloud provider adapters (OpenStack, Docker, Kubernetes, Proxmox, and more), and apps deployed through those connectors.',
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
    ('20000000-0000-4000-8000-000000000003', 1, 'cloud', TRUE, 0),
    ('20000000-0000-4000-8000-000000000003', 1, 'domains', TRUE, 1)
ON CONFLICT (package_id, version, module_id) DO NOTHING;

UPDATE provider_registry_definitions SET capabilities = '{"create": true, "scale": true, "delete": true, "containers": false, "start_stop": false, "suspend": false, "renew": false, "change_password": false, "usage": true}'::jsonb
WHERE family = 'cloud' AND provider_id = 'hetzner';
UPDATE provider_registry_definitions SET capabilities = '{"create": true, "scale": true, "delete": true, "containers": false, "start_stop": false, "suspend": false, "renew": false, "change_password": false, "usage": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'ovh';
UPDATE provider_registry_definitions SET capabilities = '{"create": false, "scale": false, "delete": false, "containers": false, "start_stop": false, "suspend": false, "renew": false, "change_password": false, "usage": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'tencent';
UPDATE provider_registry_definitions SET capabilities = '{"create": false, "scale": false, "delete": false, "containers": false, "start_stop": false, "suspend": false, "renew": false, "change_password": false, "usage": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'huawei';
UPDATE provider_registry_definitions SET capabilities = '{"create": false, "scale": false, "delete": false, "containers": false, "start_stop": false, "suspend": false, "renew": false, "change_password": false, "usage": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'aws';
UPDATE provider_registry_definitions SET capabilities = '{"create": false, "scale": false, "delete": false, "containers": false, "start_stop": false, "suspend": false, "renew": false, "change_password": false, "usage": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'google';
UPDATE provider_registry_definitions SET capabilities = '{"create": true, "scale": true, "delete": true, "containers": false, "start_stop": true, "suspend": true, "renew": false, "change_password": true, "usage": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'virtualizor';
UPDATE provider_registry_definitions SET capabilities = '{"create": true, "scale": false, "delete": true, "containers": true, "start_stop": true, "suspend": true, "renew": false, "change_password": false, "usage": true}'::jsonb
WHERE family = 'cloud' AND provider_id = 'docker';
UPDATE provider_registry_definitions SET capabilities = '{"create": true, "scale": true, "delete": true, "containers": true, "start_stop": false, "suspend": true, "renew": false, "change_password": false, "usage": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'kubernetes';
UPDATE provider_registry_definitions SET capabilities = '{"create": false, "scale": false, "delete": false, "containers": false, "start_stop": false, "suspend": false, "renew": false, "change_password": false, "usage": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'redshift';
UPDATE provider_registry_definitions SET capabilities = '{"create": true, "scale": true, "delete": true, "containers": false, "start_stop": true, "suspend": false, "renew": false, "change_password": false, "usage": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'digitalocean';
UPDATE provider_registry_definitions SET capabilities = '{"create": true, "scale": true, "delete": true, "containers": false, "start_stop": true, "suspend": false, "renew": false, "change_password": true, "usage": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'linode';
UPDATE provider_registry_definitions SET capabilities = '{"create": true, "scale": false, "delete": true, "containers": false, "start_stop": true, "suspend": true, "renew": false, "change_password": true, "usage": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'openstack';
UPDATE provider_registry_definitions SET capabilities = '{"create": false, "scale": false, "delete": true, "containers": false, "start_stop": true, "suspend": true, "renew": false, "change_password": false, "usage": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'proxmox';
UPDATE provider_registry_definitions SET capabilities = '{"create": false, "scale": false, "delete": true, "containers": false, "start_stop": true, "suspend": true, "renew": false, "change_password": false, "usage": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'kvm';
UPDATE provider_registry_definitions SET capabilities = '{"create": false, "scale": false, "delete": true, "containers": false, "start_stop": true, "suspend": true, "renew": false, "change_password": false, "usage": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'xen';
UPDATE provider_registry_definitions SET capabilities = '{"create": true, "scale": false, "delete": true, "containers": false, "start_stop": true, "suspend": false, "renew": false, "change_password": false, "usage": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'custom';
