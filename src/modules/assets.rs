//! Asset & Inventory module - hardware / software inventory,
//!
//! Split into one file per concern under `assets/`; this file only
//! declares the submodules and re-exports the public API. The full
//! module design notes live at the top of `assets/module.rs`.
pub mod ai_capabilities;
pub mod types;
mod helpers;
mod depreciation;
mod warranty;
mod hooks;
mod handlers_assets;
mod handlers_stock;
mod handlers_stock_takes;
pub mod module;

// Re-exports from types.rs.
pub(crate) use self::types::MAX_NAME_LEN;
pub(crate) use self::types::MAX_DESCRIPTION_LEN;
pub(crate) use self::types::MAX_SERIAL_LEN;
pub(crate) use self::types::MAX_LOCATION_LEN;
pub(crate) use self::types::MAX_VENDOR_LEN;
pub(crate) use self::types::MAX_REASON_LEN;
pub(crate) use self::types::MAX_NOTES_LEN;
pub(crate) use self::types::ALERT_WINDOWS_DAYS;
pub(crate) use self::types::DEFAULT_EXPIRING_DAYS;
pub(crate) use self::types::MAX_EXPIRING_DAYS;
pub(crate) use self::types::PERM_ASSETS_VIEW;
pub(crate) use self::types::PERM_ASSETS_MANAGE;
pub(crate) use self::types::WARRANTY_CHECK_REQUESTED;
pub(crate) use self::types::MANUFACTURING_STOCK_REQUESTED;
pub(crate) use self::types::STOCK_DECREMENT_REQUESTED;
pub(crate) use self::types::STOCK_INCREMENT_REQUESTED;
pub(crate) use self::types::STOCK_ADJUST_REQUESTED;
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::AssetType;
pub use self::types::AssetStatus;
pub use self::types::DepreciationMethod;
pub use self::types::Asset;
pub use self::types::AssetAssignment;
pub use self::types::AssetStock;
pub use self::types::AssetStockAdjustment;
pub use self::types::StockTakeStatus;
pub use self::types::AssetStockTake;
pub use self::types::AssetStockTakeLine;
pub use self::types::CreateAssetRequest;
pub use self::types::UpdateAssetRequest;
pub use self::types::AssignAssetRequest;
pub use self::types::ReturnAssetRequest;
pub use self::types::CreateStockRequest;
pub use self::types::UpdateStockRequest;
pub use self::types::AdjustStockRequest;
pub use self::types::CreateStockTakeRequest;
pub use self::types::AddStockTakeLineRequest;
pub use self::types::CountStockTakeLineRequest;
pub use self::types::AssetsState;

// Re-exports from helpers.rs.
pub(crate) use self::helpers::active_assignment;
pub(crate) use self::helpers::caller;
pub(crate) use self::helpers::created;
pub(crate) use self::helpers::err;
pub(crate) use self::helpers::load_asset;
pub(crate) use self::helpers::load_stock;
pub(crate) use self::helpers::load_stock_take;
pub(crate) use self::helpers::load_stock_take_line;
pub(crate) use self::helpers::ok;
pub(crate) use self::helpers::paginate;
pub(crate) use self::helpers::require_permission;
pub(crate) use self::helpers::require_tenant;
pub(crate) use self::helpers::require_text;
pub(crate) use self::helpers::serial_is_free;
pub(crate) use self::helpers::valid_currency;
pub(crate) use self::helpers::valid_price;

// Re-exports from depreciation.rs.
pub(crate) use self::depreciation::request_depreciation;

// Re-exports from warranty.rs.
pub(crate) use self::warranty::run_expiry_check;
pub(crate) use self::warranty::queue_warranty_check;
pub(crate) use self::warranty::check_low_stock;
pub(crate) use self::warranty::on_warranty_check_requested;

// Re-exports from hooks.rs.
pub(crate) use self::hooks::on_stock_decrement_requested;
pub(crate) use self::hooks::on_stock_increment_requested;
pub(crate) use self::hooks::on_stock_adjust_requested;
pub(crate) use self::hooks::on_manufacturing_stock_requested;

// Re-exports from handlers_assets.rs.
pub(crate) use self::handlers_assets::list_assets;
pub(crate) use self::handlers_assets::create_asset;
pub(crate) use self::handlers_assets::get_asset;
pub(crate) use self::handlers_assets::update_asset;
pub(crate) use self::handlers_assets::delete_asset;
pub(crate) use self::handlers_assets::assign_asset;
pub(crate) use self::handlers_assets::return_asset;
pub(crate) use self::handlers_assets::retire_asset;
pub(crate) use self::handlers_assets::dispose_asset;
pub(crate) use self::handlers_assets::send_for_repair;
pub(crate) use self::handlers_assets::return_from_repair;
pub(crate) use self::handlers_assets::list_assignments;
pub(crate) use self::handlers_assets::get_depreciation;
pub(crate) use self::handlers_assets::compute_depreciation;
pub(crate) use self::handlers_assets::list_expiring;
pub(crate) use self::handlers_assets::run_warranty_check_handler;

// Re-exports from handlers_stock.rs.
pub(crate) use self::handlers_stock::list_stock;
pub(crate) use self::handlers_stock::create_stock;
pub(crate) use self::handlers_stock::get_stock;
pub(crate) use self::handlers_stock::update_stock;
pub(crate) use self::handlers_stock::adjust_stock;
pub(crate) use self::handlers_stock::list_stock_adjustments;

// Re-exports from handlers_stock_takes.rs.
pub(crate) use self::handlers_stock_takes::list_stock_takes;
pub(crate) use self::handlers_stock_takes::create_stock_take;
pub(crate) use self::handlers_stock_takes::get_stock_take;
pub(crate) use self::handlers_stock_takes::add_stock_take_line;
pub(crate) use self::handlers_stock_takes::count_stock_take_line;
pub(crate) use self::handlers_stock_takes::close_stock_take;

// Re-exports from module.rs.
pub use self::module::AssetsModule;
pub use self::module::register;

