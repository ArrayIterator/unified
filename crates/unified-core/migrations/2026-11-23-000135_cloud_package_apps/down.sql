-- Down migration for 2026-11-23-000135_cloud_package_apps:
-- drop the cloud_apps table, remove the seeded package rows, and
-- restore the pre-overhaul capability JSONs for the cloud kinds.

DROP TABLE IF EXISTS cloud_apps;

DELETE FROM saas_package_modules
WHERE package_id = '20000000-0000-4000-8000-000000000003';
DELETE FROM saas_packages
WHERE id = '20000000-0000-4000-8000-000000000003';

UPDATE provider_registry_definitions SET capabilities = '{"create": true, "scale": true, "delete": true, "containers": false, "start_stop": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'hetzner';
UPDATE provider_registry_definitions SET capabilities = '{"create": true, "scale": true, "delete": true, "containers": false, "start_stop": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'ovh';
UPDATE provider_registry_definitions SET capabilities = '{"create": false, "scale": false, "delete": false, "containers": false, "start_stop": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'tencent';
UPDATE provider_registry_definitions SET capabilities = '{"create": false, "scale": false, "delete": false, "containers": false, "start_stop": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'huawei';
UPDATE provider_registry_definitions SET capabilities = '{"create": false, "scale": false, "delete": false, "containers": false, "start_stop": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'aws';
UPDATE provider_registry_definitions SET capabilities = '{"create": false, "scale": false, "delete": false, "containers": false, "start_stop": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'google';
UPDATE provider_registry_definitions SET capabilities = '{"create": true, "scale": true, "delete": true, "containers": false, "start_stop": true}'::jsonb
WHERE family = 'cloud' AND provider_id = 'virtualizor';
UPDATE provider_registry_definitions SET capabilities = '{"create": false, "scale": false, "delete": true, "containers": true, "start_stop": true}'::jsonb
WHERE family = 'cloud' AND provider_id = 'docker';
UPDATE provider_registry_definitions SET capabilities = '{"create": false, "scale": false, "delete": false, "containers": true, "start_stop": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'kubernetes';
UPDATE provider_registry_definitions SET capabilities = '{"create": false, "scale": false, "delete": false, "containers": false, "start_stop": false}'::jsonb
WHERE family = 'cloud' AND provider_id = 'redshift';
UPDATE provider_registry_definitions SET capabilities = '{"create": true, "scale": true, "delete": true, "containers": false, "start_stop": true}'::jsonb
WHERE family = 'cloud' AND provider_id = 'digitalocean';
UPDATE provider_registry_definitions SET capabilities = '{"create": true, "scale": true, "delete": true, "containers": false, "start_stop": true}'::jsonb
WHERE family = 'cloud' AND provider_id = 'linode';
UPDATE provider_registry_definitions SET capabilities = '{"create": true, "scale": false, "delete": true, "containers": false, "start_stop": true}'::jsonb
WHERE family = 'cloud' AND provider_id = 'openstack';
UPDATE provider_registry_definitions SET capabilities = '{"create": false, "scale": false, "delete": true, "containers": false, "start_stop": true}'::jsonb
WHERE family = 'cloud' AND provider_id = 'proxmox';
UPDATE provider_registry_definitions SET capabilities = '{"create": false, "scale": false, "delete": true, "containers": false, "start_stop": true}'::jsonb
WHERE family = 'cloud' AND provider_id = 'kvm';
UPDATE provider_registry_definitions SET capabilities = '{"create": false, "scale": false, "delete": true, "containers": false, "start_stop": true}'::jsonb
WHERE family = 'cloud' AND provider_id = 'xen';
UPDATE provider_registry_definitions SET capabilities = '{"create": true, "scale": false, "delete": true, "containers": false, "start_stop": true}'::jsonb
WHERE family = 'cloud' AND provider_id = 'custom';
