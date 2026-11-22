-- Down migration for 2026-11-22-000090_studio: drop the Studio
-- tables in reverse dependency order.
DROP TABLE IF EXISTS studio_form_layouts;
DROP TABLE IF EXISTS studio_list_views;
DROP TABLE IF EXISTS studio_field_values;
DROP TABLE IF EXISTS studio_field_definitions;
DROP TABLE IF EXISTS studio_system_fields;
DROP TABLE IF EXISTS studio_record_types;
