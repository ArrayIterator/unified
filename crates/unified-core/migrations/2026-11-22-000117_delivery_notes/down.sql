-- Delivery notes module (`src/modules/delivery_notes/`): drop every table
-- created by `2026-11-22-000117_delivery_notes/up.sql`, in reverse
-- dependency order. Like the up migration, this has never
-- been executed against a live PostgreSQL database.

DROP TABLE IF EXISTS delivery_note_settings;
DROP TABLE IF EXISTS delivery_note_items;
DROP TABLE IF EXISTS delivery_notes;
