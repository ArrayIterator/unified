//! ESG types: reporting frameworks, metric definitions,
//! reporting periods, metric values (entries), targets, and
//! the audit trail, plus the decimal number type and the
//! pure progress / completion math.
//!
//! Numbers are decimal strings, never floating point: a
//! numeric entry or target value is stored as its canonical
//! decimal string (`12.50` is stored as `12.5`), and progress
//! math runs on [`Decimal`], an integer mantissa plus a scale
//! - `f64` is never used for a stored or returned number
//! anywhere in this module. Progress is reported in basis
//! points (10000 = 100%) as `i64`, so it stays exact.
//!
//! Nothing about the sustainability content is hard-coded:
//! frameworks, metric definitions (including their category,
//! unit, data type, reporting frequency, and framework tags),
//! reporting periods, targets, and scopes are all data rows
//! an administrator enters. The only fixed vocabulary is the
//! ESG taxonomy itself (environmental / social / governance),
//! the three value data types (number / text / boolean), and
//! the two target directions (increase / decrease) - those
//! are the shape of the module, not its content.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_UNIT_LEN: usize = 64;

pub(crate) const MAX_CODE_LEN: usize = 64;

pub(crate) const MAX_FREQUENCY_LEN: usize = 64;

pub(crate) const MAX_SCOPE_TYPE_LEN: usize = 64;

pub(crate) const MAX_SCOPE_ID_LEN: usize = 200;

pub(crate) const MAX_NOTE_LEN: usize = 5000;

pub(crate) const MAX_REFERENCE_LEN: usize = 200;

/// Maximum document references on one entry.
pub(crate) const MAX_DOCUMENT_REFS: usize = 50;

/// Maximum framework tags on one metric.
pub(crate) const MAX_FRAMEWORK_TAGS: usize = 50;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_ESG_VIEW: &str = "esg.view";

pub(crate) const PERM_ESG_MANAGE: &str = "esg.manage";

/// Hook event names this module emits.
pub(crate) const EVENT_METRIC_DEFINED: &str = "esg.metric.defined";

pub(crate) const EVENT_ENTRY_RECORDED: &str = "esg.entry.recorded";

pub(crate) const EVENT_ENTRY_UPDATED: &str = "esg.entry.updated";

pub(crate) const EVENT_TARGET_SET: &str = "esg.target.set";

pub(crate) const EVENT_TARGET_ACHIEVED: &str = "esg.target.achieved";

pub(crate) const EVENT_PERIOD_CLOSED: &str = "esg.period.closed";

/// Hook event names this module listens to.
pub(crate) const EVENT_USER_DELETED: &str = "user.deleted";

/// Marker written into `created_by` / `entered_by` when the
/// owning user is deleted. Rows are kept (the reporting
/// history stays auditable); only the identity is anonymized.
pub(crate) const DELETED_USER_ID: &str = "deleted-user";

/// Basis points in 100%: progress is reported as an `i64`
/// where 10000 means 100%.
pub(crate) const BASIS_POINTS: i64 = 10_000;

/// Attainment at or above this value counts as achieved.
pub(crate) const ACHIEVED_BPS: i64 = 10_000;

/// Attainment at or above this value (but below achieved)
/// counts as on track.
pub(crate) const ON_TRACK_BPS: i64 = 7_500;

// Decimal: integer mantissa + scale, no floating point

/// A decimal number as an integer mantissa plus a scale: the
/// value is `mantissa / 10^scale`. `12.50` is mantissa `1250`,
/// scale `2`; parsing keeps the written scale, arithmetic
/// normalizes trailing zeroes away. All operations are
/// checked - overflow is an error, never a silent wrap or a
/// float. This is the same approach as the spreadsheets
/// module's decimal type, kept local so this module does not
/// depend on another module's internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Decimal {
    pub mantissa: i128,
    pub scale: u32,
}

/// Largest scale kept after an operation. Division always
/// produces this scale (10 fractional digits).
pub const DECIMAL_MAX_SCALE: u32 = 28;

impl Decimal {
    pub fn zero() -> Self {
        Self {
            mantissa: 0,
            scale: 0,
        }
    }

    pub fn from_int(value: i64) -> Self {
        Self {
            mantissa: value as i128,
            scale: 0,
        }
    }

    /// True when the value is exactly zero.
    pub fn is_zero(&self) -> bool {
        self.mantissa == 0
    }

    /// Strip trailing zeroes from the mantissa, lowering the
    /// scale; zero always normalizes to scale 0.
    pub fn normalized(&self) -> Self {
        if self.mantissa == 0 {
            return Self::zero();
        }
        let mut mantissa = self.mantissa;
        let mut scale = self.scale;
        while scale > 0 && mantissa % 10 == 0 {
            mantissa /= 10;
            scale -= 1;
        }
        Self { mantissa, scale }
    }

    /// 10 to the power `exp` as an `i128`, or `None` on
    /// overflow / out of range.
    fn pow10(exp: u32) -> Option<i128> {
        if exp > 38 {
            return None;
        }
        let mut result: i128 = 1;
        for _ in 0..exp {
            result = result.checked_mul(10)?;
        }
        Some(result)
    }

    /// Rescale to `scale` (raising it pads with zeroes,
    /// lowering it truncates toward zero). Returns `None`
    /// when the rescale would overflow.
    fn rescale(&self, scale: u32) -> Option<Self> {
        if scale == self.scale {
            return Some(*self);
        }
        if scale > self.scale {
            let factor = Self::pow10(scale - self.scale)?;
            Some(Self {
                mantissa: self.mantissa.checked_mul(factor)?,
                scale,
            })
        } else {
            let factor = Self::pow10(self.scale - scale)?;
            Some(Self {
                mantissa: self.mantissa / factor,
                scale,
            })
        }
    }

    /// Compare two decimals exactly, or `None` when the
    /// alignment would overflow.
    pub fn checked_cmp(&self, other: &Self) -> Option<Ordering> {
        let scale = self.scale.max(other.scale);
        let left = self.rescale(scale)?;
        let right = other.rescale(scale)?;
        Some(left.mantissa.cmp(&right.mantissa))
    }

    /// Division with 10 fractional digits: the result scale
    /// is always 10. Returns `None` on division by zero or
    /// overflow.
    pub fn checked_div(&self, other: &Self) -> Option<Self> {
        if other.mantissa == 0 {
            return None;
        }
        let numerator = self
            .mantissa
            .checked_mul(Self::pow10(other.scale.checked_add(10)?)?)?;
        let denominator = other.mantissa.checked_mul(Self::pow10(self.scale)?)?;
        Some(Self {
            mantissa: numerator / denominator,
            scale: 10,
        })
    }

    /// Parse a decimal literal: optional sign, digits with an
    /// optional fractional part, and an optional exponent
    /// (`1.5e3`). Rejects empty input, lone signs, and anything
    /// that is not a number.
    pub fn parse(raw: &str) -> Option<Self> {
        let text = raw.trim();
        if text.is_empty() {
            return None;
        }
        let (negative, rest) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text.strip_prefix('+').unwrap_or(text)),
        };
        if rest.is_empty() {
            return None;
        }
        let (mantissa_part, exponent) = match rest.split_once(['e', 'E']) {
            Some((base, exp)) => {
                let exp: i32 = exp.parse().ok()?;
                if !(-38..=38).contains(&exp) {
                    return None;
                }
                (base, exp)
            }
            None => (rest, 0),
        };
        let (int_part, frac_part) = match mantissa_part.split_once('.') {
            Some((int, frac)) => (int, frac),
            None => (mantissa_part, ""),
        };
        if int_part.is_empty() && frac_part.is_empty() {
            return None;
        }
        if !int_part.bytes().all(|b| b.is_ascii_digit())
            || !frac_part.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        if int_part.len() + frac_part.len() > 38 {
            return None;
        }
        let digits = format!("{int_part}{frac_part}");
        let digits = digits.trim_start_matches('0');
        if digits.is_empty() {
            return Some(Self::zero());
        }
        let mut mantissa: i128 = digits.parse().ok()?;
        let mut scale: i32 = frac_part.len() as i32 - exponent;
        if scale < 0 {
            let pad = (-scale) as u32;
            mantissa = mantissa.checked_mul(Self::pow10(pad)?)?;
            scale = 0;
        }
        if scale as u32 > DECIMAL_MAX_SCALE {
            return None;
        }
        if negative {
            mantissa = -mantissa;
        }
        Some(Self {
            mantissa,
            scale: scale as u32,
        })
    }

    /// Canonical decimal string: no exponent, no trailing
    /// zeroes in the fraction, `-0` written as `0`.
    pub fn to_decimal_string(&self) -> String {
        let value = self.normalized();
        if value.mantissa == 0 {
            return "0".to_string();
        }
        let negative = value.mantissa < 0;
        let digits = value.mantissa.unsigned_abs().to_string();
        let mut out = String::new();
        if negative {
            out.push('-');
        }
        if value.scale == 0 {
            out.push_str(&digits);
            return out;
        }
        let scale = value.scale as usize;
        if digits.len() > scale {
            let split = digits.len() - scale;
            out.push_str(&digits[..split]);
            out.push('.');
            out.push_str(&digits[split..]);
        } else {
            out.push_str("0.");
            for _ in 0..(scale - digits.len()) {
                out.push('0');
            }
            out.push_str(&digits);
        }
        out
    }
}

impl std::fmt::Display for Decimal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_decimal_string())
    }
}

// Pure progress math (unit-tested below)

/// Target attainment in basis points (10000 = 100%), or
/// `None` when it is undefined: an `increase` target of zero
/// with a non-zero actual has no finite ratio. Attainment is
/// deliberately not capped at 10000 - beating a target is
/// real, and the summary caps per-metric attainment when it
/// averages. `direction` is `increase` (higher is better) or
/// `decrease` (lower is better, e.g. emissions).
///
/// Edge cases, all deliberate:
/// - actual and target both zero: 10000 (nothing to do, done).
/// - increase, target zero, actual non-zero: `None`.
/// - decrease, actual zero: 10000 (zero meets any target).
/// - decrease, target zero, actual non-zero: 0.
pub fn attainment_bps(
    actual: &Decimal,
    target: &Decimal,
    direction: TargetDirection,
) -> Option<i64> {
    match direction {
        TargetDirection::Increase => {
            if target.is_zero() {
                return if actual.is_zero() {
                    Some(BASIS_POINTS)
                } else {
                    None
                };
            }
            ratio_bps(actual, target)
        }
        TargetDirection::Decrease => {
            if actual.is_zero() {
                return Some(BASIS_POINTS);
            }
            if target.is_zero() {
                return Some(0);
            }
            ratio_bps(target, actual)
        }
    }
}

/// `(numerator / denominator) * 10000` in basis points,
/// rounded half away from zero, or `None` on overflow /
/// division by zero. Both inputs must be non-negative here;
/// callers validate that.
fn ratio_bps(numerator: &Decimal, denominator: &Decimal) -> Option<i64> {
    let ratio = numerator.checked_div(denominator)?;
    // Ratio has scale 10, so basis points are
    // mantissa / 10^6, rounded half away from zero.
    let divisor: i128 = 1_000_000;
    let sign: i128 = if ratio.mantissa < 0 { -1 } else { 1 };
    let magnitude = ratio.mantissa.abs();
    let bps = (magnitude + divisor / 2) / divisor;
    let bps = sign * bps;
    i64::try_from(bps).ok()
}

/// Plain-language progress label for an attainment value in
/// basis points: achieved at 100% or more, on track at 75%
/// or more, behind above zero, not started at zero.
pub fn attainment_label(bps: i64) -> &'static str {
    if bps >= ACHIEVED_BPS {
        "achieved"
    } else if bps >= ON_TRACK_BPS {
        "on_track"
    } else if bps > 0 {
        "behind"
    } else {
        "not_started"
    }
}

/// Reporting completion in basis points (10000 = 100%):
/// `reported` of `total` metrics have a value. A period with
/// no metrics counts as complete (nothing to report).
pub fn completion_bps(reported: i64, total: i64) -> i64 {
    if total <= 0 {
        return BASIS_POINTS;
    }
    let reported = reported.clamp(0, total);
    (reported * BASIS_POINTS) / total
}

/// True when `text` is a valid `YYYY-MM-DD` calendar date.
pub fn valid_date(text: &str) -> bool {
    if text.len() != 10 {
        return false;
    }
    let parts: Vec<&str> = text.split('-').collect();
    if parts.len() != 3 {
        return false;
    }
    if parts[0].len() != 4 || parts[1].len() != 2 || parts[2].len() != 2 {
        return false;
    }
    if !parts
        .iter()
        .all(|part| part.bytes().all(|b| b.is_ascii_digit()))
    {
        return false;
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok()
}

/// Parse and validate a period date range: both dates must be
/// valid `YYYY-MM-DD` dates and start must not be after end.
pub fn validate_period_range(
    starts_on: &str,
    ends_on: &str,
) -> Result<(NaiveDate, NaiveDate), String> {
    if !valid_date(starts_on) {
        return Err(format!("starts_on must be a YYYY-MM-DD date: {starts_on}"));
    }
    if !valid_date(ends_on) {
        return Err(format!("ends_on must be a YYYY-MM-DD date: {ends_on}"));
    }
    let start = NaiveDate::parse_from_str(starts_on, "%Y-%m-%d")
        .map_err(|_| format!("starts_on must be a YYYY-MM-DD date: {starts_on}"))?;
    let end = NaiveDate::parse_from_str(ends_on, "%Y-%m-%d")
        .map_err(|_| format!("ends_on must be a YYYY-MM-DD date: {ends_on}"))?;
    if start > end {
        return Err("starts_on must not be after ends_on".to_string());
    }
    Ok((start, end))
}

// Enumerations (fixed vocabulary - the shape, not the content)

/// ESG category: the taxonomy itself. Metric names, units,
/// and framework tags are data; the three categories are not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Environmental,
    Social,
    Governance,
}

impl Category {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Environmental => "environmental",
            Self::Social => "social",
            Self::Governance => "governance",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "environmental" => Some(Self::Environmental),
            "social" => Some(Self::Social),
            "governance" => Some(Self::Governance),
            _ => None,
        }
    }
}

/// Value data type of a metric: numbers are stored as
/// canonical decimal strings, booleans as `true` / `false`,
/// text as written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricDataType {
    Number,
    Text,
    Boolean,
}

impl MetricDataType {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Number => "number",
            Self::Text => "text",
            Self::Boolean => "boolean",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "number" => Some(Self::Number),
            "text" => Some(Self::Text),
            "boolean" => Some(Self::Boolean),
            _ => None,
        }
    }
}

/// Which direction counts as progress toward a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetDirection {
    Increase,
    Decrease,
}

impl TargetDirection {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Increase => "increase",
            Self::Decrease => "decrease",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "increase" => Some(Self::Increase),
            "decrease" => Some(Self::Decrease),
            _ => None,
        }
    }
}

/// Reporting period status. Entries and targets may only be
/// written while a period is open; closing is explicit and
/// reversible through reopen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeriodStatus {
    Open,
    Closed,
}

impl PeriodStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "open" => Some(Self::Open),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }
}

// Stored types

/// One reporting framework definition, entered by an
/// administrator as data - for example a standard the tenant
/// reports against, with its own version. Frameworks are
/// never a hard-coded list: a tenant with no framework rows
/// simply has no framework tags to attach.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Framework {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One admin-defined sustainability metric: its name,
/// category, unit, data type, reporting frequency, and the
/// framework ids it reports under are all data. `unit` is
/// free text (a unit name the tenant uses); `frequency` is
/// free text as well (e.g. a cadence name) so no cadence list
/// is hard-coded. `active` false hides the metric from new
/// entries without deleting its history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metric {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub category: Category,
    pub unit: String,
    pub data_type: MetricDataType,
    pub frequency: String,
    #[serde(default)]
    pub framework_ids: Vec<String>,
    #[serde(default = "default_active")]
    pub active: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn default_active() -> bool {
    true
}

/// One reporting period definition, entered as data: a code
/// (e.g. a quarter or fiscal-year label), a date range, and
/// an open / closed status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Period {
    pub id: Uuid,
    pub tenant_id: String,
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub starts_on: NaiveDate,
    pub ends_on: NaiveDate,
    pub status: PeriodStatus,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One recorded metric value for a metric in a period at a
/// scope. `scope_type` / `scope_id` are plain reference ids
/// (a company or a site from the company module, or any other
/// scope the tenant uses) - this module never assumes the
/// company module's tables. Numeric values are canonical
/// decimal strings; booleans are `true` / `false`; text is
/// stored as written. `document_ids` are references into the
/// documents module by id only (no document data is copied
/// here). One entry per (metric, period, scope) - a second
/// value for the same combination updates the entry and
/// appends a history row instead of creating a duplicate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: Uuid,
    pub tenant_id: String,
    pub metric_id: Uuid,
    pub period_id: Uuid,
    pub scope_type: String,
    pub scope_id: String,
    pub value_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_note: Option<String>,
    #[serde(default)]
    pub document_ids: Vec<String>,
    pub entered_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One value-history row: every entry creation and every
/// update appends a row with who entered the value and when,
/// so the full history of a value is preserved even though
/// the entry row itself only holds the latest value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryHistory {
    pub id: Uuid,
    pub tenant_id: String,
    pub entry_id: Uuid,
    pub metric_id: Uuid,
    pub period_id: Uuid,
    pub value_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_note: Option<String>,
    pub entered_by: String,
    pub created_at: DateTime<Utc>,
}

/// One per-metric, per-period, per-scope target: the target
/// value (a decimal string for numeric metrics, `true` /
/// `false` for boolean metrics, free text otherwise), the
/// direction that counts as progress, and the deadline.
/// One target per (metric, period, scope); setting a target
/// again replaces it and writes an audit row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Target {
    pub id: Uuid,
    pub tenant_id: String,
    pub metric_id: Uuid,
    pub period_id: Uuid,
    pub scope_type: String,
    pub scope_id: String,
    pub target_value: String,
    pub direction: TargetDirection,
    pub deadline: NaiveDate,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One audit row: who changed what, when, with the before
/// and after values as JSON. Written for entry creates /
/// updates / deletes, target creates / updates / deletes,
/// and period close / reopen. Audit rows are never updated
/// or deleted through the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    pub id: Uuid,
    pub tenant_id: String,
    pub entity_type: String,
    pub entity_id: String,
    pub action: String,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

// Report views (computed, not stored)

/// Progress of one metric in a period summary: the latest
/// recorded value (when any), the target (when any), the
/// attainment in basis points (when computable), and its
/// label. One row per (metric, scope) combination that has an
/// entry or a target - values are never summed across scopes,
/// because scopes (sites, companies) report independently.
/// `attainment_bps` is absent for text metrics, for metrics
/// with no target, and for the undefined increase-from-zero
/// case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricProgress {
    pub metric_id: String,
    pub metric_name: String,
    pub category: Category,
    pub unit: String,
    pub data_type: MetricDataType,
    pub scope_type: String,
    pub scope_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attainment_bps: Option<i64>,
    pub attainment_label: String,
}

/// One category slice of a period summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategorySummary {
    pub category: Category,
    pub metric_count: i64,
    pub reported_count: i64,
    pub completion_bps: i64,
    pub targets_set: i64,
    pub targets_achieved: i64,
}

/// Summary of one reporting period: metrics by category,
/// overall completion, and target attainment. Averages cap
/// each metric's attainment at 10000 so one over-achieved
/// metric cannot hide others that are behind.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeriodSummary {
    pub period_id: String,
    pub period_code: String,
    pub status: PeriodStatus,
    pub metric_count: i64,
    pub reported_count: i64,
    pub completion_bps: i64,
    pub targets_set: i64,
    pub targets_achieved: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub average_attainment_bps: Option<i64>,
    pub by_category: Vec<CategorySummary>,
    pub metrics: Vec<MetricProgress>,
}

// Request bodies

#[derive(Debug, Deserialize)]
pub struct CreateFrameworkRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateFrameworkRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateMetricRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub category: String,
    pub unit: String,
    pub data_type: String,
    pub frequency: String,
    #[serde(default)]
    pub framework_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMetricRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framework_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePeriodRequest {
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub starts_on: String,
    pub ends_on: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePeriodRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_on: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_on: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateEntryRequest {
    pub metric_id: String,
    pub period_id: String,
    pub scope_type: String,
    pub scope_id: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_note: Option<String>,
    #[serde(default)]
    pub document_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateEntryRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_note: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_ids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTargetRequest {
    pub metric_id: String,
    pub period_id: String,
    pub scope_type: String,
    pub scope_id: String,
    pub target_value: String,
    pub direction: String,
    pub deadline: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTargetRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_parse_and_display_round_trip() {
        for (raw, canonical) in [
            ("0", "0"),
            ("12.50", "12.5"),
            ("-0.5", "-0.5"),
            ("100", "100"),
            ("0.000", "0"),
            ("1.5e3", "1500"),
            ("2E-2", "0.02"),
            ("+7.25", "7.25"),
            ("-0", "0"),
            ("0.10", "0.1"),
        ] {
            let value = Decimal::parse(raw).unwrap_or_else(|| panic!("parse {raw}"));
            assert_eq!(value.to_decimal_string(), canonical, "for {raw}");
        }
        assert!(Decimal::parse("").is_none());
        assert!(Decimal::parse("abc").is_none());
        assert!(Decimal::parse("12.34.56").is_none());
        assert!(Decimal::parse("-").is_none());
        assert!(Decimal::parse("1e100").is_none());
    }

    #[test]
    fn decimal_comparison_is_exact() {
        let a = Decimal::parse("1.5").unwrap();
        let b = Decimal::parse("1.25").unwrap();
        assert_eq!(a.checked_cmp(&b), Some(Ordering::Greater));
        assert_eq!(
            Decimal::parse("1.0")
                .unwrap()
                .checked_cmp(&Decimal::parse("1.00").unwrap()),
            Some(Ordering::Equal)
        );
        assert_eq!(
            Decimal::parse("-1").unwrap().checked_cmp(&Decimal::zero()),
            Some(Ordering::Less)
        );
        assert_eq!(
            Decimal::parse("10")
                .unwrap()
                .checked_div(&Decimal::parse("4").unwrap())
                .unwrap()
                .to_decimal_string(),
            "2.5"
        );
        assert!(
            Decimal::parse("1")
                .unwrap()
                .checked_div(&Decimal::zero())
                .is_none()
        );
    }

    #[test]
    fn attainment_increase() {
        let actual = Decimal::parse("80").unwrap();
        let target = Decimal::parse("100").unwrap();
        assert_eq!(
            attainment_bps(&actual, &target, TargetDirection::Increase),
            Some(8000)
        );
        assert_eq!(
            attainment_bps(&target, &target, TargetDirection::Increase),
            Some(10_000)
        );
        assert_eq!(
            attainment_bps(
                &Decimal::parse("150").unwrap(),
                &target,
                TargetDirection::Increase
            ),
            Some(15_000)
        );
        // Zero target with a non-zero actual is undefined.
        assert_eq!(
            attainment_bps(&actual, &Decimal::zero(), TargetDirection::Increase),
            None
        );
        // Zero against zero counts as achieved.
        assert_eq!(
            attainment_bps(
                &Decimal::zero(),
                &Decimal::zero(),
                TargetDirection::Increase
            ),
            Some(10_000)
        );
        // Fractional ratio rounds half away from zero.
        assert_eq!(
            attainment_bps(
                &Decimal::parse("1").unwrap(),
                &Decimal::parse("3").unwrap(),
                TargetDirection::Increase
            ),
            Some(3333)
        );
        assert_eq!(
            attainment_bps(
                &Decimal::parse("2").unwrap(),
                &Decimal::parse("3").unwrap(),
                TargetDirection::Increase
            ),
            Some(6667)
        );
    }

    #[test]
    fn attainment_decrease() {
        let actual = Decimal::parse("80").unwrap();
        let target = Decimal::parse("100").unwrap();
        // Lower is better: 80 against a 100 target is 125%.
        assert_eq!(
            attainment_bps(&actual, &target, TargetDirection::Decrease),
            Some(12_500)
        );
        assert_eq!(
            attainment_bps(&target, &target, TargetDirection::Decrease),
            Some(10_000)
        );
        // Zero actual fully meets any positive target.
        assert_eq!(
            attainment_bps(&Decimal::zero(), &target, TargetDirection::Decrease),
            Some(10_000)
        );
        // Zero target with a non-zero actual is a miss.
        assert_eq!(
            attainment_bps(&actual, &Decimal::zero(), TargetDirection::Decrease),
            Some(0)
        );
        assert_eq!(
            attainment_bps(
                &Decimal::zero(),
                &Decimal::zero(),
                TargetDirection::Decrease
            ),
            Some(10_000)
        );
    }

    #[test]
    fn attainment_labels() {
        assert_eq!(attainment_label(10_000), "achieved");
        assert_eq!(attainment_label(12_500), "achieved");
        assert_eq!(attainment_label(7_500), "on_track");
        assert_eq!(attainment_label(9_999), "on_track");
        assert_eq!(attainment_label(1), "behind");
        assert_eq!(attainment_label(0), "not_started");
    }

    #[test]
    fn completion_math() {
        assert_eq!(completion_bps(0, 4), 0);
        assert_eq!(completion_bps(1, 4), 2500);
        assert_eq!(completion_bps(3, 4), 7500);
        assert_eq!(completion_bps(4, 4), 10_000);
        assert_eq!(completion_bps(0, 0), 10_000);
        assert_eq!(completion_bps(9, 4), 10_000);
    }

    #[test]
    fn period_range_validation() {
        assert!(validate_period_range("2026-01-01", "2026-03-31").is_ok());
        assert!(validate_period_range("2026-01-01", "2026-01-01").is_ok());
        assert!(validate_period_range("2026-04-01", "2026-03-31").is_err());
        assert!(validate_period_range("2026-13-01", "2026-03-31").is_err());
        assert!(validate_period_range("2026-01-01", "31-03-2026").is_err());
        assert!(validate_period_range("2026-02-30", "2026-03-31").is_err());
        assert!(valid_date("2026-11-22"));
        assert!(!valid_date("2026-11-32"));
        assert!(!valid_date("22-11-2026"));
    }

    #[test]
    fn vocabulary_parsing() {
        assert_eq!(
            Category::from_str("environmental"),
            Some(Category::Environmental)
        );
        assert_eq!(Category::from_str("social"), Some(Category::Social));
        assert_eq!(Category::from_str("governance"), Some(Category::Governance));
        assert_eq!(Category::from_str("economic"), None);
        assert_eq!(
            MetricDataType::from_str("number"),
            Some(MetricDataType::Number)
        );
        assert_eq!(MetricDataType::from_str("text"), Some(MetricDataType::Text));
        assert_eq!(
            MetricDataType::from_str("boolean"),
            Some(MetricDataType::Boolean)
        );
        assert_eq!(MetricDataType::from_str("date"), None);
        assert_eq!(
            TargetDirection::from_str("increase"),
            Some(TargetDirection::Increase)
        );
        assert_eq!(
            TargetDirection::from_str("decrease"),
            Some(TargetDirection::Decrease)
        );
        assert_eq!(TargetDirection::from_str("maintain"), None);
        assert_eq!(PeriodStatus::from_str("open"), Some(PeriodStatus::Open));
        assert_eq!(PeriodStatus::from_str("closed"), Some(PeriodStatus::Closed));
        assert_eq!(PeriodStatus::from_str("draft"), None);
    }
}
