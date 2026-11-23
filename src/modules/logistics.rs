// Logistics module — shipments, fleet, warehouses, delivery
// routes, delivery scheduling, proof of delivery, delivery
// exceptions, and shipping cost quotes.
//
// Full module notes (routes, honesty notes) live at the
// top of `src/modules/logistics/module.rs`.

pub mod ai_capabilities;
mod constants;
mod delivery_routes;
mod drivers;
mod helpers;
mod hooks;
pub mod module;
mod rates;
pub mod requests;
mod shipments;
pub mod state;
mod tracking;
pub mod types;
mod vehicles;
mod warehouses;

// Re-exported from `types`.
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::DeliveryException;
pub use self::types::DeliveryRoute;
pub use self::types::Driver;
pub use self::types::MaintenanceReminder;
pub use self::types::ProofOfDelivery;
pub use self::types::RateTable;
pub use self::types::RouteStop;
pub use self::types::Shipment;
pub use self::types::ShipmentEndpoint;
pub use self::types::ShipmentStatus;
pub use self::types::ShipmentTimelineEntry;
pub use self::types::StockMovement;
pub use self::types::Vehicle;
pub use self::types::Warehouse;
// Re-exported from `requests`.
pub use self::requests::AssignRouteRequest;
pub use self::requests::AssignVehicleRequest;
pub use self::requests::CreateDriverRequest;
pub use self::requests::CreateExceptionRequest;
pub use self::requests::CreateMaintenanceRequest;
pub use self::requests::CreateProofRequest;
pub use self::requests::CreateRateTableRequest;
pub use self::requests::CreateRouteRequest;
pub use self::requests::CreateRouteStopRequest;
pub use self::requests::CreateShipmentRequest;
pub use self::requests::CreateStockMovementRequest;
pub use self::requests::CreateVehicleRequest;
pub use self::requests::CreateWarehouseRequest;
pub use self::requests::LinkOrderRequest;
pub use self::requests::QuoteRequest;
pub use self::requests::RescheduleRequest;
pub use self::requests::ResolveExceptionRequest;
pub use self::requests::ReturnExceptionRequest;
pub use self::requests::ScheduleRequest;
pub use self::requests::TrackQuery;
pub use self::requests::TransitionRequest;
pub use self::requests::UpdateDriverRequest;
pub use self::requests::UpdateRateTableRequest;
pub use self::requests::UpdateRouteRequest;
pub use self::requests::UpdateRouteStopRequest;
pub use self::requests::UpdateShipmentRequest;
pub use self::requests::UpdateVehicleRequest;
pub use self::requests::UpdateWarehouseRequest;
// Re-exported from `state`.
pub use self::state::LogisticsState;
// Re-exported from `module`.
pub use self::module::register;
pub use self::module::LogisticsModule;
