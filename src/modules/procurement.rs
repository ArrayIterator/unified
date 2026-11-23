// Procurement / Vendor Management module: vendors, purchase orders
// with approval flow, goods receipts and budgets. Split into one
// file per concern; this file only declares the submodules and
// re-exports the public API.

pub mod ai_capabilities;
pub mod handlers_lines;
pub mod handlers_orders;
pub mod handlers_receipts_budgets;
pub mod handlers_vendors;
pub mod helpers;
pub mod module;
pub mod state;
pub mod types;

// Re-exported from types.rs.
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::ApprovalDecision;
pub use self::types::ApproveRequest;
pub use self::types::CancelRequest;
pub use self::types::CreateBudgetRequest;
pub use self::types::CreateLineRequest;
pub use self::types::CreatePurchaseOrderRequest;
pub use self::types::CreateVendorRequest;
pub use self::types::GoodsReceipt;
pub use self::types::ProcurementBudget;
pub use self::types::PurchaseOrder;
pub use self::types::PurchaseOrderApproval;
pub use self::types::PurchaseOrderLine;
pub use self::types::PurchaseOrderStatus;
pub use self::types::ReceiveRequest;
pub use self::types::RejectRequest;
pub use self::types::UpdateBudgetRequest;
pub use self::types::UpdateLineRequest;
pub use self::types::UpdatePurchaseOrderRequest;
pub use self::types::UpdateVendorRequest;
pub use self::types::Vendor;
pub use self::types::VendorStatus;
// Re-exported from state.rs.
pub use self::state::ProcurementState;
// Re-exported from module.rs.
pub use self::module::ProcurementModule;
pub use self::module::register;
