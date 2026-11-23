//! billing - school fees types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::Ordering;
use super::state::*;
use super::helpers::*;

pub(crate) const PERM_SCHOOL_FEES_VIEW: &str = "school_fees.view";

pub(crate) const PERM_SCHOOL_FEES_MANAGE: &str = "school_fees.manage";

/// School fee settings. All values are configurable defaults:
/// nothing in this section treats them as fixed rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchoolFeeSettings {
    /// Default currency for new fee types (ISO code).
    pub currency: String,
    /// Days after an academic period starts when an auto-assigned
    /// schedule first falls due.
    pub auto_assign_due_in_days: u32,
    /// Reminder schedule: days before the due date a reminder is due.
    pub reminder_before_days: Vec<i32>,
    /// Reminder schedule: days after the due date a reminder is due.
    pub reminder_after_days: Vec<i32>,
    /// Aging bucket cut-offs in days overdue, ascending.
    /// With `[30, 60, 90]` the buckets are `1-30`, `31-60`,
    /// `61-90` and `over_90`.
    pub aging_bucket_days: Vec<i32>,
    /// Allowed fee-type categories. Fee types may only use a
    /// category from this list; the list itself is editable.
    pub fee_categories: Vec<String>,
    /// Allowed billing periods for fee types (for example
    /// `monthly`, `per_term`, `one_off`).
    pub billing_periods: Vec<String>,
    /// Prefix for generated receipt numbers.
    pub receipt_prefix: String,
    /// When true, a new discount starts unapproved and a manager
    /// must approve it before it applies to charges.
    pub discount_requires_approval: bool,
}

impl Default for SchoolFeeSettings {
    fn default() -> Self {
        Self {
            currency: "USD".to_string(),
            auto_assign_due_in_days: 30,
            reminder_before_days: vec![7, 3, 1],
            reminder_after_days: vec![1, 7, 14],
            aging_bucket_days: vec![30, 60, 90],
            fee_categories: vec![
                "tuition".to_string(),
                "registration".to_string(),
                "examination".to_string(),
                "activities".to_string(),
                "transport".to_string(),
                "meals".to_string(),
                "other".to_string(),
            ],
            billing_periods: vec![
                "monthly".to_string(),
                "per_term".to_string(),
                "one_off".to_string(),
            ],
            receipt_prefix: "RCPT".to_string(),
            discount_requires_approval: true,
        }
    }
}

/// One admin-editable fee type definition. Fee types are data, not
/// code: registration, examination and activity fees are just rows
/// in this table, created by an administrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchoolFeeType {
    pub id: String,
    /// Stable code, unique across fee types (for example
    /// `monthly-tuition`).
    pub code: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Category, validated against the settings category list.
    pub category: String,
    /// Billing period, validated against the settings period list.
    pub billing_period: String,
    /// Default gross amount in minor units, used when a schedule
    /// does not set its own amount.
    pub default_amount_minor: i64,
    pub currency: String,
    /// When true, enrolment (`academic.enrollment.approved`)
    /// creates a schedule for this fee type automatically.
    #[serde(default)]
    pub auto_assign_on_enrolment: bool,
    #[serde(default = "default_true")]
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub(crate) fn default_true() -> bool {
    true
}

/// One per-student fee schedule: which fees apply to a student,
/// for which program, level and academic period, at what amount,
/// and when they fall due.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudentFeeSchedule {
    pub id: String,
    /// Academic module identity of the student.
    pub student_user_id: String,
    /// Program code owned by the academic module.
    pub program_code: String,
    /// Level code owned by the academic module.
    pub level_code: String,
    /// Academic period id owned by the academic module.
    pub academic_period_id: String,
    pub fee_type_id: String,
    /// Gross amount in minor units for one generated charge.
    pub amount_minor: i64,
    pub currency: String,
    pub due_date: DateTime<Utc>,
    /// `active`, `paused` or `cancelled`.
    pub status: String,
    #[serde(default)]
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One generated fee charge: a single amount owed for one period,
/// linked to one billing invoice (unless fully discounted).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeCharge {
    pub id: String,
    pub schedule_id: String,
    pub student_user_id: String,
    pub fee_type_id: String,
    pub academic_period_id: String,
    /// Period label supplied at generation (for example `2026-11`).
    pub period_label: String,
    /// Gross amount in minor units, before discounts.
    pub amount_minor: i64,
    /// Discount applied in minor units.
    #[serde(default)]
    pub discount_minor: i64,
    /// Net amount owed in minor units (`amount - discount`).
    pub total_minor: i64,
    /// Amount paid so far in minor units, mirrored from the
    /// linked invoice.
    #[serde(default)]
    pub amount_paid_minor: i64,
    pub currency: String,
    /// Tenant / company id the charge belongs to, resolved from
    /// the request's tenant context when the charge is generated.
    /// Carried onto the linked billing invoice so it can be routed
    /// through pricing reconciliation before it is issued.
    #[serde(default)]
    pub company_id: Option<String>,
    /// Linked billing invoice, if any. `None` only for a fully
    /// discounted charge, which has nothing to collect.
    #[serde(default)]
    pub invoice_id: Option<String>,
    /// Discount ids applied to this charge.
    #[serde(default)]
    pub discount_ids: Vec<String>,
    pub due_date: DateTime<Utc>,
    /// Base status: `open`, `partially_paid`, `paid` or `void`.
    /// Reads report `overdue` for an open charge past its due
    /// date (see `view_fee_charge`).
    pub status: String,
    /// True when the charge is past its due date with a balance
    /// outstanding. Derived on read, not stored as source of truth.
    #[serde(default)]
    pub overdue: bool,
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One per-student discount or scholarship.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeDiscount {
    pub id: String,
    pub student_user_id: String,
    /// Fee type this discount applies to, or `None` for all types.
    #[serde(default)]
    pub fee_type_id: Option<String>,
    /// `percentage` or `fixed`.
    pub kind: String,
    /// Percentage in basis points (2500 means 25.00%). Only used
    /// when `kind` is `percentage`.
    #[serde(default)]
    pub percent_basis_points: i64,
    /// Fixed discount in minor units. Only used when `kind` is
    /// `fixed`.
    #[serde(default)]
    pub amount_minor: i64,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    /// Whether the discount is approved and may apply to charges.
    /// See `discount_requires_approval` in the settings.
    #[serde(default)]
    pub approved: bool,
    #[serde(default)]
    pub approved_by: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    /// `active` or `revoked`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One issued receipt for a fee charge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeReceipt {
    pub id: String,
    /// Human-readable receipt number, for example `RCPT-000001`.
    pub number: String,
    pub charge_id: String,
    pub student_user_id: String,
    /// Receipt amount in minor units (amount paid so far).
    pub amount_minor: i64,
    pub currency: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub reference: Option<String>,
    pub issued_by: String,
    pub issued_at: DateTime<Utc>,
}

/// One guardian-to-student link. Guardians see only the students
/// linked to them here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeGuardianLink {
    pub id: String,
    pub guardian_user_id: String,
    pub student_user_id: String,
    /// Relationship label, for example `parent` or `guardian`.
    #[serde(default)]
    pub relationship: String,
    #[serde(default = "default_true")]
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

/// One fee reminder already sent, so the scan never sends the same
/// reminder twice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeReminder {
    pub charge_id: String,
    /// Days relative to the due date: positive means days before
    /// due, negative means days after due, `0` means a manual
    /// reminder.
    pub offset_days: i32,
    pub recipient_user_id: String,
    pub sent_at: DateTime<Utc>,
}

/// One aging bucket in an arrears summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArrearsBucket {
    pub bucket: String,
    pub amount_minor: i64,
    pub charge_count: u32,
}

/// Per-student arrears and aging summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudentArrears {
    pub student_user_id: String,
    pub total_overdue_minor: i64,
    pub currency: String,
    pub charge_count: u32,
    pub oldest_due_date: Option<DateTime<Utc>>,
    pub buckets: Vec<ArrearsBucket>,
}

impl BillingState {
    pub(crate) fn fee_settings(&self) -> SchoolFeeSettings {
        self.fee_settings
            .read()
            .expect("school fee settings lock poisoned")
            .clone()
    }

    pub(crate) fn next_fee_receipt_number(&self) -> String {
        let prefix = self.fee_settings().receipt_prefix;
        let seq = self.fee_receipt_seq.fetch_add(1, Ordering::SeqCst);
        format!("{prefix}-{seq:06}")
    }
}

/// Convert major units to integer minor units.
///
/// Returns `None` for non-finite values and for magnitudes that
/// would overflow `i64` — the old unchecked `as i64` cast silently
/// saturated `1e17` to `i64::MAX`. Callers must handle `None`
/// (reject the amount, skip the record) instead of assuming a
/// conversion always succeeds.
pub(crate) fn major_to_minor(amount: f64, currency: &str) -> Option<i64> {
    if !amount.is_finite() {
        return None;
    }
    if is_zero_decimal_currency(currency) {
        let rounded = amount.round();
        if rounded > i64::MAX as f64 || rounded < i64::MIN as f64 {
            return None;
        }
        Some(rounded as i64)
    } else {
        unified_core::money::major_to_minor_checked(amount)
    }
}

/// Read a money amount out of a hook/order JSON payload.
///
/// Accepts an integer JSON number (minor units, the canonical wire
/// form) or a legacy major-unit float, which is converted with the
/// same range check as `major_to_minor`. Returns `None` when the
/// value is missing, not a number, non-finite, or out of range, so
/// callers fail closed instead of silently saturating.
pub(crate) fn value_to_minor(value: &Value, currency: &str) -> Option<i64> {
    if let Some(minor) = value.as_i64() {
        return Some(minor);
    }
    value.as_f64().and_then(|major| major_to_minor(major, currency))
}

/// Format integer minor units as a decimal string (no floats in the
/// receipt payload).
pub(crate) fn minor_to_decimal_string(amount_minor: i64, currency: &str) -> String {
    if is_zero_decimal_currency(currency) {
        format!("{amount_minor}")
    } else {
        let sign = if amount_minor < 0 { "-" } else { "" };
        let absolute = amount_minor.abs();
        format!("{sign}{}.{:02}", absolute / 100, absolute % 100)
    }
}
