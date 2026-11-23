-- Down migration for 2026-11-22-000111_rental: drop all rental tables.
DROP TABLE IF EXISTS rental_settings;
DROP TABLE IF EXISTS rental_damage_reports;
DROP TABLE IF EXISTS rental_deliveries;
DROP TABLE IF EXISTS rental_orders;
DROP TABLE IF EXISTS rental_rates;
DROP TABLE IF EXISTS rental_listings;
