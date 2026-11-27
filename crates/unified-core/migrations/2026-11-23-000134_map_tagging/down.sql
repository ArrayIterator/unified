-- Roll back migration 000134: drop the Map & Tagging
-- tables. Locations, links, layers, geofences, positions, kind /
-- role / provider definitions and per-tenant map settings are all
-- removed; nothing else references them (other modules keep their
-- own latitude/longitude columns and are untouched by this
-- migration in either direction).
DROP TABLE IF EXISTS map_settings;
DROP TABLE IF EXISTS map_providers;
DROP TABLE IF EXISTS map_link_roles;
DROP TABLE IF EXISTS map_location_kinds;
DROP TABLE IF EXISTS map_positions;
DROP TABLE IF EXISTS map_geofences;
DROP TABLE IF EXISTS map_location_links;
DROP TABLE IF EXISTS map_locations;
DROP TABLE IF EXISTS map_layers;
