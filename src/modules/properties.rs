// Properties module — land parcels and housing developments with
// 2D site plans (master plans), blocks / plots for sale, location
// search, SVG rendering, and print support.
//
// Routes live under `/api/v1/properties`:
//
//   Properties:
//     GET/POST / — list properties / create a property.
//     GET/PATCH/DELETE /{id}.
//     POST /{id}/publish, /{id}/archive, /{id}/mark-sold-out.
//   Plans:
//     GET/POST /{id}/plans — list plans / create a plan.
//     GET/PATCH/DELETE /{id}/plans/{plan_id}.
//     GET  /{id}/plans/{plan_id}/svg — the plan as SVG.
//     GET  /{id}/plans/{plan_id}/print — print-friendly HTML page.
//   Blocks:
//     GET/POST /{id}/plans/{plan_id}/blocks — list blocks / create a block.
//     GET/PATCH/DELETE /{id}/plans/{plan_id}/blocks/{block_id}.
//     POST /{id}/plans/{plan_id}/blocks/{block_id}/reserve,
//          /release, /purchase, /assign, /block, /unblock,
//          /correct-status.
//     GET  /{id}/plans/{plan_id}/blocks/{block_id}/history.
//     GET  /{id}/plans/{plan_id}/blocks/export?format=csv|json.
//   Location:
//     GET  /location/search?q=... — geocoder search.
//     GET  /location/reverse?lat=...&lng=... — reverse geocoding.
//     GET/PATCH /location/settings — geocoder provider settings.
//   Public (no auth; see the public-access note below):
//     GET  /public — published properties only.
//     GET  /public/{slug} — one published property with plans and blocks.
//     GET  /public/{slug}/plans/{plan_id}/svg.
//     GET  /public/{slug}/plans/{plan_id}/print.
//
//
// Honesty, configuration, and event notes that followed the
// route list above now live at the top of `listeners.rs`.

// Split into one file per concern under `src/modules/properties/`.
// This root only declares the submodules and re-exports the
// public API at its original paths.

pub mod ai_capabilities;
pub mod constants;
pub mod types;
pub mod requests;
pub mod geometry;
pub mod escaping;
pub mod svg;
pub mod providers;
pub mod state;
pub mod helpers;
pub mod block_status;
pub mod listeners;
pub mod property_handlers;
pub mod plan_handlers;
pub mod block_handlers;
pub mod print_handlers;
pub mod location_handlers;
pub mod public_handlers;
pub mod module;

pub(crate) use self::constants::{MAX_TITLE_LEN, MAX_DESCRIPTION_LEN, MAX_SLUG_LEN, MAX_ADDRESS_LEN, MAX_NOTES_LEN, MAX_REASON_LEN, MAX_NAME_LEN, MAX_STORAGE_KEY_LEN, MAX_SEARCH_QUERY_LEN, MAX_GEOMETRY_POSITIONS, MAX_GEOMETRY_RINGS, MAX_SEARCH_RESULTS, DEFAULT_GEOCODER_TIMEOUT_SECS, MAX_GEOCODER_TIMEOUT_SECS, GEOCODER_USER_AGENT, SVG_WIDTH, SVG_HEIGHT, SVG_PADDING, METRES_PER_DEGREE_LAT, METRES_PER_DEGREE_LNG_AT_EQUATOR, PERM_PROPERTIES_VIEW, PERM_PROPERTIES_CREATE, PERM_PROPERTIES_MANAGE, PAYMENT_CAPTURED, BANK_TRANSFER_VERIFICATION_CREATED, BANK_TRANSFER_REJECTED, PROPERTIES_ORDER_CREATED};
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::{PropertyStatus, Property, PropertyPlan, BlockStatus, ClickBehavior, ClickLinkKind, PropertyBlock, PropertyBlockHistory, BlockPurchase, PlaceCandidate, LocationSettings};
pub use self::requests::{CreatePropertyRequest, UpdatePropertyRequest, CreatePlanRequest, UpdatePlanRequest, CreateBlockRequest, UpdateBlockRequest, BlockActionRequest, CorrectStatusRequest, UpdateLocationSettingsRequest, LocationSearchQuery, LocationReverseQuery, ExportQuery};
pub(crate) use self::requests::{default_export_format};
pub use self::geometry::{ValidatedGeometry, validate_geometry};
pub(crate) use self::geometry::{read_position, read_ring, validate_polygon, validate_multi_polygon, validate_line_string, project, mean_latitude, ring_area_centroid, polygon_measurements, line_length_m, geometry_hash, geometry_positions};
pub(crate) use self::escaping::{xml_escape, csv_escape};
pub use self::svg::{render_plan_svg, render_print_html, blocks_to_csv};
pub(crate) use self::svg::{PlanProjection, ring_path, format_money};
pub use self::providers::{LocationProvider, NoneProvider, NominatimProvider};
pub(crate) use self::providers::{NominatimSearchItem, NominatimReverseItem, build_provider, valid_hostname};
pub use self::state::{PropertiesState};
pub(crate) use self::helpers::{
    Caller, caller, caller_company, check_capability, created, err, in_company_scope, load_block,
    load_plan, load_property, load_property_by_slug, ok, paginate, parse_uuid,
    require_company_change_uuid_for, require_company_for_write, require_company_match_uuid,
    require_permission, require_record_in_company_scope, require_record_writable_in_company_scope,
    require_tenant, require_text, slug_in_use, slugify, valid_currency, valid_latitude,
    valid_longitude, valid_price, valid_slug, validate_click_target, validate_storage_reference,
};
pub(crate) use self::block_status::{record_history, move_block_status, public_property_view, public_block_view, plan_view};
pub(crate) use self::listeners::{on_properties_order_created, find_purchase_for_payment, is_zero_decimal_currency, minor_to_major, on_payment_captured, on_bank_transfer_verification_created, on_bank_transfer_rejected};
pub(crate) use self::property_handlers::{list_properties, create_property, get_property, can_edit_property, update_property, delete_property, change_property_status, publish_property, archive_property, mark_property_sold_out};
pub(crate) use self::plan_handlers::{list_plans, create_plan, get_plan, update_plan, delete_plan};
pub(crate) use self::block_handlers::{list_blocks, create_block, get_block, update_block, delete_block, resolve_buyer, reserve_block, release_block, purchase_block, assign_block, block_block, unblock_block, correct_block_status, list_block_history, export_blocks};
pub(crate) use self::print_handlers::{get_plan_svg, get_plan_print};
pub(crate) use self::location_handlers::{search_location, reverse_location, get_location_settings, update_location_settings};
pub(crate) use self::public_handlers::{public_list_view, list_public_properties, get_public_property, get_public_plan_svg, get_public_plan_print};
pub use self::module::{PropertiesModule, register};
