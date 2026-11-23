// Field Service / Equipment Maintenance module — equipment
// registry, recurring maintenance plans, work orders with a
// validated status machine, technician dispatch, parts
// consumption requests, time logging, customer sign-off, and
// completion reports.
//
// Routes live under `/api/v1/field-service`. The full route list
// and module documentation live in `field_service/module.rs`.


pub mod ai_capabilities;
mod constants;
pub mod types;
pub mod requests;
pub mod state;
mod helpers;
pub mod module;
mod handlers_equipment_types;
mod handlers_equipment;
mod handlers_templates;
mod handlers_plans;
mod handlers_work_orders;
mod handlers_items;
mod handlers_parts;
mod handlers_time_signoff;
mod handlers_dispatch;
mod handlers_definitions;

// Public API (unchanged): re-exported from the split files.
// From types.rs:
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::DefinitionValue;
pub use self::types::DispatchAssignment;
pub use self::types::Equipment;
pub use self::types::EquipmentType;
pub use self::types::FieldServiceSettings;
pub use self::types::IntervalUnitDef;
pub use self::types::MaintenancePlan;
pub use self::types::MaintenanceTemplate;
pub use self::types::Signoff;
pub use self::types::WorkOrder;
pub use self::types::WorkOrderItem;
pub use self::types::WorkOrderPart;
pub use self::types::WorkOrderStatus;
// From requests.rs:
pub use self::requests::AddDefinitionRequest;
pub use self::requests::ApplyTemplateRequest;
pub use self::requests::AssignRequest;
pub use self::requests::CancelRequest;
pub use self::requests::ChangeStatusRequest;
pub use self::requests::CreateEquipmentRequest;
pub use self::requests::CreateEquipmentTypeRequest;
pub use self::requests::CreateItemRequest;
pub use self::requests::CreatePartLineRequest;
pub use self::requests::CreatePartRequest;
pub use self::requests::CreatePlanRequest;
pub use self::requests::CreateTemplateRequest;
pub use self::requests::CreateWorkOrderRequest;
pub use self::requests::RaiseIncidentRequest;
pub use self::requests::ReportFailureRequest;
pub use self::requests::SignOffRequest;
pub use self::requests::TimeLogRequest;
pub use self::requests::UpdateEquipmentRequest;
pub use self::requests::UpdateEquipmentTypeRequest;
pub use self::requests::UpdateItemRequest;
pub use self::requests::UpdatePartRequest;
pub use self::requests::UpdatePlanRequest;
pub use self::requests::UpdateSettingsRequest;
pub use self::requests::UpdateTemplateRequest;
pub use self::requests::UpdateWorkOrderRequest;
// From state.rs:
pub use self::state::FieldServiceState;
// From module.rs:
pub use self::module::FieldServiceModule;
pub use self::module::register;
