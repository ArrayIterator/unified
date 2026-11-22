-- Down migration for 2026-11-22-000078_digital_products: drop the
-- Digital Products tables in reverse dependency order.
DROP TABLE IF EXISTS digital_downloads;
DROP TABLE IF EXISTS digital_royalties;
DROP TABLE IF EXISTS digital_payout_requests;
DROP TABLE IF EXISTS digital_product_previews;
DROP TABLE IF EXISTS digital_product_files;
DROP TABLE IF EXISTS digital_products;
DROP TABLE IF EXISTS digital_watermark_formats;
DROP TABLE IF EXISTS digital_product_types;
