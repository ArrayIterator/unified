-- Down migration for 2026-11-22-000070_network_tools: drop the
-- Network Tools tables in reverse dependency order.
DROP TABLE IF EXISTS network_tool_cert_watches;
DROP TABLE IF EXISTS network_tool_uptime_events;
DROP TABLE IF EXISTS network_tool_uptime_monitors;
DROP TABLE IF EXISTS network_tool_runs;
DROP TABLE IF EXISTS network_tool_saved_checks;
