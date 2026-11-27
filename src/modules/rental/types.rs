//! Rental types: listings with data-driven per-period pricing,
//! orders with a status machine, deliveries, damage reports, and
//! per-tenant settings.
//!
//! A store product becomes rentable through a rental listing: the
//! listing references the product by id only (no foreign key -
//! products live in the store/products modules and this module
//! must not assume their tables or migration order). Prices are
//! data, not code: every listing carries one or more rates, each
//! rate naming its own period (`period_unit` plus
//! `period_length`, so hourly, daily, weekly, monthly, and
//! custom periods are all just rows), a deposit amount, and a
//! late-fee rule (basis, amount, grace hours, optional cap).
//! Money is always integer minor units; there is no floating
//! point money anywhere in this module.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_TITLE_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_SLUG_LEN: usize = 128;

pub(crate) const MAX_NOTES_LEN: usize = 5000;

pub(crate) const MAX_CURRENCY_LEN: usize = 3;

pub(crate) const MAX_PERIOD_UNIT_LEN: usize = 32;

pub(crate) const MAX_PERIOD_LABEL_LEN: usize = 64;

pub(crate) const MAX_PHOTO_IDS: usize = 20;

/// Default late-fee grace hours when a listing carries no rule.
pub(crate) const DEFAULT_LATE_FEE_GRACE_HOURS: i64 = 0;

/// Default maximum advance booking window in days.
pub(crate) const DEFAULT_MAX_ADVANCE_BOOKING_DAYS: i64 = 365;

/// Default cancellation cutoff: an order may be cancelled up to
/// this many hours before its start.
pub(crate) const DEFAULT_CANCEL_CUTOFF_HOURS: i64 = 24;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_RENTAL_VIEW: &str = "rental.view";

pub(crate) const PERM_RENTAL_MANAGE: &str = "rental.manage";

/// Hook event names this module listens to.
pub(crate) const EVENT_USER_DELETED: &str = "user.deleted";

pub(crate) const EVENT_PAYMENT_CAPTURED: &str = "payment.captured";

pub(crate) const EVENT_PAYMENT_REFUNDED: &str = "payment.refunded";

pub(crate) const EVENT_SHIPMENT_DELIVERED: &str = "logistics.shipment.delivered";

pub(crate) const EVENT_CONTRACT_SIGNED: &str = "contracts.signed";

/// Marker written into `customer_id` / `reported_by` /
/// `recorded_by` when the owning user is deleted. Rows are kept
/// (orders and reports stay auditable); only the identity is
/// anonymized.
pub(crate) const DELETED_USER_ID: &str = "deleted-user";

// Pure domain functions (unit-tested below)

/// Half-open range overlap: `[a_start, a_end)` overlaps
/// `[b_start, b_end)` when each starts before the other ends.
/// Touching ranges (one ends exactly when the other starts) do
/// not overlap, so back-to-back rentals of the same unit are
/// allowed.
pub fn ranges_overlap(
    a_start: DateTime<Utc>,
    a_end: DateTime<Utc>,
    b_start: DateTime<Utc>,
    b_end: DateTime<Utc>,
) -> bool {
    a_start < b_end && b_start < a_end
}

/// Quantity already committed over `[start, end)` by the given
/// booked periods. Each entry is `(starts_at, ends_at,
/// quantity)`; only periods overlapping the requested range
/// contribute.
pub fn overlapping_quantity(
    booked: &[(DateTime<Utc>, DateTime<Utc>, i64)],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> i64 {
    booked
        .iter()
        .filter(|(booked_start, booked_end, _)| {
            ranges_overlap(*booked_start, *booked_end, start, end)
        })
        .map(|(_, _, quantity)| *quantity)
        .sum()
}

/// True when `requested` more units fit alongside what is already
/// booked, given `total_quantity` units in stock.
pub fn is_available(total_quantity: i64, booked_quantity: i64, requested: i64) -> bool {
    requested > 0 && total_quantity - booked_quantity >= requested
}

/// Duration of one period in seconds. `period_unit` is data
/// supplied with the rate; the units understood here are the
/// calendar units a rental period can be expressed in, and an
/// unknown unit returns `None` so the caller rejects the rate
/// instead of guessing. A month is 30 days by definition here,
/// documented on the rate so tenants price accordingly.
pub fn period_duration_seconds(period_unit: &str, period_length: i64) -> Option<i64> {
    if period_length <= 0 {
        return None;
    }
    let unit_seconds = match period_unit {
        "hour" => 3_600,
        "day" => 86_400,
        "week" => 604_800,
        "month" => 2_592_000,
        _ => return None,
    };
    Some(unit_seconds * period_length)
}

/// Rental amount in minor units for a date range priced by one
/// rate: whole periods, rounded up (a partial period is charged
/// as a full one), times the quantity. Returns `None` when the
/// rate's period is not understood or the range is empty.
pub fn compute_rental_amount(
    rate: &RentalRate,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    quantity: i64,
) -> Option<i64> {
    if ends_at <= starts_at || quantity <= 0 {
        return None;
    }
    let period_seconds = period_duration_seconds(&rate.period_unit, rate.period_length)?;
    let total_seconds = (ends_at - starts_at).num_seconds();
    // Ceiling division, written out by hand: `i64::div_ceil` is
    // still behind the `int_roundings` feature gate on this
    // toolchain. Both operands are positive here (the early return
    // above guarantees `total_seconds > 0`), so `(a + b - 1) / b`
    // rounds up exactly like `div_ceil`.
    let periods = (total_seconds + period_seconds - 1) / period_seconds;
    Some(periods * rate.price_minor * quantity)
}

/// Late fee in minor units for a return. No fee inside the grace
/// window after `due_at`; afterwards the fee accrues per whole
/// unit of the rule's basis (`per_day`, `per_hour`, or a single
/// `flat` fee), rounded up, and is capped at `cap_minor` when
/// the rule carries one. An unknown basis accrues nothing - the
/// rule is data, and an unrecognized basis must not invent a
/// charge.
pub fn compute_late_fee(
    rule: &LateFeeRule,
    due_at: DateTime<Utc>,
    returned_at: DateTime<Utc>,
) -> i64 {
    let grace_end = due_at + Duration::hours(rule.grace_hours.max(0));
    if returned_at <= grace_end || rule.amount_minor <= 0 {
        return 0;
    }
    let overdue_seconds = (returned_at - grace_end).num_seconds();
    // Ceiling division by hand (see `compute_rental_amount`:
    // `i64::div_ceil` is still feature-gated here). `overdue_seconds`
    // is positive: the early return above guarantees it.
    let units = match rule.basis.as_str() {
        "per_day" => (overdue_seconds + 86_400 - 1) / 86_400,
        "per_hour" => (overdue_seconds + 3_600 - 1) / 3_600,
        "flat" => 1,
        _ => return 0,
    };
    let fee = units * rule.amount_minor;
    match rule.cap_minor {
        Some(cap) if fee > cap => cap.max(0),
        _ => fee,
    }
}

/// Order status machine. Allowed transitions:
/// draft -> reserved, cancelled; reserved -> out, cancelled;
/// out -> returned; returned -> completed; cancelled and
/// completed are terminal. Anything else is rejected.
pub fn can_transition(from: RentalOrderStatus, to: RentalOrderStatus) -> bool {
    use RentalOrderStatus::{Cancelled, Completed, Draft, Out, Reserved, Returned};
    matches!(
        (from, to),
        (Draft, Reserved)
            | (Draft, Cancelled)
            | (Reserved, Out)
            | (Reserved, Cancelled)
            | (Out, Returned)
            | (Returned, Completed)
    )
}

/// Statuses that still hold units against the availability
/// calendar: a draft holds nothing (it is only a quote), a
/// cancelled or completed order holds nothing.
pub fn status_holds_units(status: RentalOrderStatus) -> bool {
    matches!(
        status,
        RentalOrderStatus::Reserved | RentalOrderStatus::Out | RentalOrderStatus::Returned
    )
}

// Types

/// Order status. Serialized snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RentalOrderStatus {
    Draft,
    Reserved,
    Out,
    Returned,
    Completed,
    Cancelled,
}

impl Default for RentalOrderStatus {
    fn default() -> Self {
        Self::Draft
    }
}

impl RentalOrderStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Reserved => "reserved",
            Self::Out => "out",
            Self::Returned => "returned",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "draft" => Some(Self::Draft),
            "reserved" => Some(Self::Reserved),
            "out" => Some(Self::Out),
            "returned" => Some(Self::Returned),
            "completed" => Some(Self::Completed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// Deposit lifecycle. `held` means the deposit was captured and
/// is still held; `refunded` means it went back in full;
/// `deducted` means it was kept in full against damage or late
/// fees; `partially_refunded` means part was kept and the rest
/// returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DepositStatus {
    Pending,
    Held,
    Refunded,
    Deducted,
    PartiallyRefunded,
}

impl Default for DepositStatus {
    fn default() -> Self {
        Self::Pending
    }
}

impl DepositStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Held => "held",
            Self::Refunded => "refunded",
            Self::Deducted => "deducted",
            Self::PartiallyRefunded => "partially_refunded",
        }
    }
}

/// Late-fee rule, stored as data on the listing. `basis` is
/// `per_day`, `per_hour`, or `flat`; `amount_minor` is the fee
/// per unit (or the single flat fee) in minor units;
/// `grace_hours` is the fee-free window after the due time;
/// `cap_minor` caps the total late fee when set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LateFeeRule {
    pub basis: String,
    pub amount_minor: i64,
    pub grace_hours: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap_minor: Option<i64>,
}

impl Default for LateFeeRule {
    fn default() -> Self {
        Self {
            basis: "per_day".to_string(),
            amount_minor: 0,
            grace_hours: DEFAULT_LATE_FEE_GRACE_HOURS,
            cap_minor: None,
        }
    }
}

/// One rentable product. `product_id` references a store
/// product by id only (no foreign key). `slug` is unique per
/// tenant. `quantity` is how many identical units of this
/// listing can be rented at once; the availability calendar
/// counts overlapping orders against it. `deposit_amount_minor`
/// is the refundable deposit per unit. `currency` is a
/// three-letter code shared by the rates and the orders priced
/// from this listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RentalListing {
    pub id: Uuid,
    pub tenant_id: String,
    pub product_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub slug: String,
    pub currency: String,
    pub deposit_amount_minor: i64,
    pub quantity: i64,
    pub late_fee_rule: LateFeeRule,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One per-period price of a listing. `period_unit` plus
/// `period_length` define the period (for example unit `day`,
/// length 1 is a daily rate; unit `week`, length 2 is a
/// two-week rate); `period_label` is the display name shown to
/// renters. One rate per (`listing_id`, `period_label`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RentalRate {
    pub id: Uuid,
    pub tenant_id: String,
    pub listing_id: Uuid,
    pub period_label: String,
    pub period_unit: String,
    pub period_length: i64,
    pub price_minor: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One rental order (contract). `rental_amount_minor` is the
/// priced amount for the range at booking time;
/// `late_fee_minor` and `damage_total_minor` are computed at
/// return; `total_minor` is rental + late fee + damage minus
/// nothing - the deposit is handled separately through
/// `deposit_status` and `deposit_deducted_minor`.
/// `contract_id` references a contracts-module agreement by id
/// only (no foreign key); `agreement_signed` is set by the
/// `contracts.signed` listener. `shipment_id` is set by the
/// logistics delivery listener when a shipment for this order
/// is delivered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RentalOrder {
    pub id: Uuid,
    pub tenant_id: String,
    pub listing_id: Uuid,
    pub rate_id: Uuid,
    pub customer_id: String,
    pub status: RentalOrderStatus,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub quantity: i64,
    pub currency: String,
    pub rental_amount_minor: i64,
    pub deposit_amount_minor: i64,
    pub deposit_status: DepositStatus,
    pub deposit_deducted_minor: i64,
    pub late_fee_minor: i64,
    pub damage_total_minor: i64,
    pub total_minor: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_id: Option<String>,
    pub agreement_signed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipment_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One hand-out or return event for an order. `kind` is
/// `handover` (the unit goes out) or `return` (the unit comes
/// back). `condition_notes` records the unit's condition at
/// that moment; `photo_ids` holds document/storage references
/// only - metadata, never file bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RentalDelivery {
    pub id: Uuid,
    pub tenant_id: String,
    pub order_id: Uuid,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition_notes: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub photo_ids: Vec<String>,
    pub recorded_by: String,
    pub created_at: DateTime<Utc>,
}

/// One damage report against an order. `status` is `open`,
/// `resolved`, or `dismissed`; resolving a report adds its
/// `estimated_cost_minor` to the order's damage total (once -
/// the resolved report is marked so a second resolve is a
/// no-op for the total).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RentalDamageReport {
    pub id: Uuid,
    pub tenant_id: String,
    pub order_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_id: Option<Uuid>,
    pub description: String,
    pub estimated_cost_minor: i64,
    pub status: String,
    pub reported_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Per-tenant rental settings. The in-memory copy is
/// authoritative until the migration lands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RentalSettings {
    pub late_fee_grace_hours: i64,
    pub max_advance_booking_days: i64,
    pub cancel_cutoff_hours: i64,
}

impl Default for RentalSettings {
    fn default() -> Self {
        Self {
            late_fee_grace_hours: DEFAULT_LATE_FEE_GRACE_HOURS,
            max_advance_booking_days: DEFAULT_MAX_ADVANCE_BOOKING_DAYS,
            cancel_cutoff_hours: DEFAULT_CANCEL_CUTOFF_HOURS,
        }
    }
}

// Request bodies

#[derive(Debug, Deserialize)]
pub struct CreateListingRequest {
    pub product_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    pub currency: String,
    #[serde(default)]
    pub deposit_amount_minor: i64,
    #[serde(default = "default_quantity_one")]
    pub quantity: i64,
    #[serde(default)]
    pub late_fee_rule: LateFeeRule,
    #[serde(default = "default_true")]
    pub active: bool,
}

fn default_quantity_one() -> i64 {
    1
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct UpdateListingRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deposit_amount_minor: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub late_fee_rule: Option<LateFeeRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateRateRequest {
    pub period_label: String,
    pub period_unit: String,
    pub period_length: i64,
    pub price_minor: i64,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_length: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_minor: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateOrderRequest {
    pub listing_id: Uuid,
    pub rate_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_id: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    #[serde(default = "default_quantity_one")]
    pub quantity: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateOrderRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_id: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct HandoverRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition_notes: Option<String>,
    #[serde(default)]
    pub photo_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReturnRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition_notes: Option<String>,
    #[serde(default)]
    pub photo_ids: Vec<String>,
    /// When the unit actually came back; defaults to now.
    /// Supplied mainly for back-office recording.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returned_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct DepositRequest {
    /// `refund` returns the deposit in full; `deduct` keeps
    /// `amount_minor` of it against damage or late fees.
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount_minor: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateDamageReportRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_id: Option<Uuid>,
    pub description: String,
    #[serde(default)]
    pub estimated_cost_minor: i64,
}

#[derive(Debug, Deserialize)]
pub struct ResolveDamageReportRequest {
    /// `resolved` adds the estimated cost to the order damage
    /// total once; `dismissed` closes the report without a
    /// charge.
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub late_fee_grace_hours: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_advance_booking_days: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_cutoff_hours: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(year, month, day, hour, 0, 0).unwrap()
    }

    fn rate(unit: &str, length: i64, price: i64) -> RentalRate {
        RentalRate {
            id: Uuid::new_v4(),
            tenant_id: "tenant-1".to_string(),
            listing_id: Uuid::new_v4(),
            period_label: "daily".to_string(),
            period_unit: unit.to_string(),
            period_length: length,
            price_minor: price,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn rule(basis: &str, amount: i64, grace: i64, cap: Option<i64>) -> LateFeeRule {
        LateFeeRule {
            basis: basis.to_string(),
            amount_minor: amount,
            grace_hours: grace,
            cap_minor: cap,
        }
    }

    #[test]
    fn ranges_overlap_half_open() {
        let a_start = utc(2026, 11, 22, 10);
        let a_end = utc(2026, 11, 24, 10);
        // Touching at the boundary: no overlap.
        assert!(!ranges_overlap(
            a_start,
            a_end,
            a_end,
            utc(2026, 11, 26, 10)
        ));
        assert!(!ranges_overlap(
            a_end,
            utc(2026, 11, 26, 10),
            a_start,
            a_end
        ));
        // Partial overlap both ways.
        assert!(ranges_overlap(
            a_start,
            a_end,
            utc(2026, 11, 23, 0),
            utc(2026, 11, 25, 0)
        ));
        assert!(ranges_overlap(
            utc(2026, 11, 23, 0),
            utc(2026, 11, 25, 0),
            a_start,
            a_end
        ));
        // Containment.
        assert!(ranges_overlap(
            a_start,
            a_end,
            utc(2026, 11, 22, 12),
            utc(2026, 11, 23, 12)
        ));
        // Identical ranges overlap.
        assert!(ranges_overlap(a_start, a_end, a_start, a_end));
    }

    #[test]
    fn overlapping_quantity_counts_only_overlapping() {
        let booked = vec![
            (utc(2026, 11, 22, 10), utc(2026, 11, 24, 10), 2),
            (utc(2026, 11, 25, 10), utc(2026, 11, 26, 10), 1),
        ];
        assert_eq!(
            overlapping_quantity(&booked, utc(2026, 11, 23, 0), utc(2026, 11, 25, 0)),
            2
        );
        assert_eq!(
            overlapping_quantity(&booked, utc(2026, 11, 24, 10), utc(2026, 11, 25, 10)),
            0
        );
        assert_eq!(
            overlapping_quantity(&booked, utc(2026, 11, 22, 0), utc(2026, 11, 27, 0)),
            3
        );
    }

    #[test]
    fn availability_respects_total_quantity() {
        assert!(is_available(2, 1, 1));
        assert!(!is_available(2, 2, 1));
        assert!(!is_available(2, 0, 0));
        assert!(!is_available(1, 0, 2));
    }

    #[test]
    fn period_duration_seconds_units() {
        assert_eq!(period_duration_seconds("hour", 1), Some(3_600));
        assert_eq!(period_duration_seconds("day", 2), Some(172_800));
        assert_eq!(period_duration_seconds("week", 1), Some(604_800));
        assert_eq!(period_duration_seconds("month", 1), Some(2_592_000));
        assert_eq!(period_duration_seconds("fortnight", 1), None);
        assert_eq!(period_duration_seconds("day", 0), None);
        assert_eq!(period_duration_seconds("day", -1), None);
    }

    #[test]
    fn rental_amount_rounds_partial_period_up() {
        let daily = rate("day", 1, 10_000);
        // 2 days 1 hour is 3 daily periods.
        assert_eq!(
            compute_rental_amount(&daily, utc(2026, 11, 22, 10), utc(2026, 11, 24, 11), 1),
            Some(30_000)
        );
        // Exactly 2 days is 2 periods; quantity multiplies.
        assert_eq!(
            compute_rental_amount(&daily, utc(2026, 11, 22, 10), utc(2026, 11, 24, 10), 2),
            Some(40_000)
        );
        // Empty range and unknown unit price nothing.
        assert_eq!(
            compute_rental_amount(&daily, utc(2026, 11, 22, 10), utc(2026, 11, 22, 10), 1),
            None
        );
        assert_eq!(
            compute_rental_amount(
                &rate("fortnight", 1, 10_000),
                utc(2026, 11, 22, 10),
                utc(2026, 11, 24, 10),
                1
            ),
            None
        );
    }

    #[test]
    fn late_fee_grace_and_units() {
        let due = utc(2026, 11, 24, 10);
        let per_day = rule("per_day", 5_000, 2, None);
        // Inside the 2-hour grace window: no fee.
        assert_eq!(compute_late_fee(&per_day, due, utc(2026, 11, 24, 11)), 0);
        // 1 day 1 hour past grace: 2 days charged (rounded up).
        assert_eq!(
            compute_late_fee(&per_day, due, utc(2026, 11, 25, 13)),
            10_000
        );
        let per_hour = rule("per_hour", 1_000, 0, None);
        assert_eq!(
            compute_late_fee(&per_hour, due, utc(2026, 11, 24, 12)),
            2_000
        );
        // 30 minutes late, hourly basis: one hour charged.
        assert_eq!(
            compute_late_fee(
                &per_hour,
                due,
                utc(2026, 11, 24, 10)
                    .checked_add_signed(Duration::minutes(30))
                    .unwrap()
            ),
            1_000
        );
        let flat = rule("flat", 7_500, 0, None);
        assert_eq!(compute_late_fee(&flat, due, utc(2026, 11, 30, 10)), 7_500);
        // On time: no fee even for a flat rule.
        assert_eq!(compute_late_fee(&flat, due, due), 0);
    }

    #[test]
    fn late_fee_cap_and_unknown_basis() {
        let due = utc(2026, 11, 24, 10);
        let capped = rule("per_day", 5_000, 0, Some(8_000));
        assert_eq!(compute_late_fee(&capped, due, utc(2026, 11, 30, 10)), 8_000);
        let unknown = rule("per_moon", 5_000, 0, None);
        assert_eq!(compute_late_fee(&unknown, due, utc(2026, 11, 30, 10)), 0);
        let zero_amount = rule("per_day", 0, 0, None);
        assert_eq!(
            compute_late_fee(&zero_amount, due, utc(2026, 11, 30, 10)),
            0
        );
    }

    #[test]
    fn status_machine_transitions() {
        use RentalOrderStatus::{Cancelled, Completed, Draft, Out, Reserved, Returned};
        assert!(can_transition(Draft, Reserved));
        assert!(can_transition(Draft, Cancelled));
        assert!(!can_transition(Draft, Out));
        assert!(!can_transition(Draft, Returned));
        assert!(!can_transition(Draft, Completed));
        assert!(can_transition(Reserved, Out));
        assert!(can_transition(Reserved, Cancelled));
        assert!(!can_transition(Reserved, Returned));
        assert!(can_transition(Out, Returned));
        assert!(!can_transition(Out, Completed));
        assert!(!can_transition(Out, Cancelled));
        assert!(can_transition(Returned, Completed));
        assert!(!can_transition(Returned, Out));
        assert!(!can_transition(Completed, Cancelled));
        assert!(!can_transition(Cancelled, Reserved));
        assert!(!can_transition(Draft, Draft));
    }

    #[test]
    fn status_holds_units_only_while_active() {
        use RentalOrderStatus::{Cancelled, Completed, Draft, Out, Reserved, Returned};
        assert!(!status_holds_units(Draft));
        assert!(status_holds_units(Reserved));
        assert!(status_holds_units(Out));
        assert!(status_holds_units(Returned));
        assert!(!status_holds_units(Completed));
        assert!(!status_holds_units(Cancelled));
    }

    #[test]
    fn order_status_round_trips() {
        for (status, text) in [
            (RentalOrderStatus::Draft, "draft"),
            (RentalOrderStatus::Reserved, "reserved"),
            (RentalOrderStatus::Out, "out"),
            (RentalOrderStatus::Returned, "returned"),
            (RentalOrderStatus::Completed, "completed"),
            (RentalOrderStatus::Cancelled, "cancelled"),
        ] {
            assert_eq!(status.as_str(), text);
            assert_eq!(RentalOrderStatus::from_str(text), Some(status));
        }
        assert_eq!(RentalOrderStatus::from_str("shipped"), None);
    }

    #[test]
    fn late_fee_rule_default_is_zero_per_day() {
        let rule = LateFeeRule::default();
        assert_eq!(rule.basis, "per_day");
        assert_eq!(rule.amount_minor, 0);
        assert_eq!(rule.grace_hours, DEFAULT_LATE_FEE_GRACE_HOURS);
        assert_eq!(rule.cap_minor, None);
    }

    #[test]
    fn rental_settings_defaults_use_constants() {
        let settings = RentalSettings::default();
        assert_eq!(settings.late_fee_grace_hours, DEFAULT_LATE_FEE_GRACE_HOURS);
        assert_eq!(
            settings.max_advance_booking_days,
            DEFAULT_MAX_ADVANCE_BOOKING_DAYS
        );
        assert_eq!(settings.cancel_cutoff_hours, DEFAULT_CANCEL_CUTOFF_HOURS);
    }
}
