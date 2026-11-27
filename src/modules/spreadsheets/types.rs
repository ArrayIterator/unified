//! Spreadsheets types: workbooks, sheets, cells, shares,
//! versions, and per-tenant settings, plus the decimal number
//! type the formula evaluator works on and A1 address parsing.
//!
//! Numbers are decimal strings, never floating point: a cell
//! number is stored as its decimal string (`12.50` keeps its
//! scale in the string), and the evaluator computes on
//! [`Decimal`], an integer mantissa plus a scale - `f64` is
//! never used for a stored or returned number anywhere in
//! this module.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_TITLE_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_RAW_VALUE_LEN: usize = 100_000;

pub(crate) const MAX_USER_ID_LEN: usize = 200;

pub(crate) const MAX_EMAIL_LEN: usize = 254;

pub(crate) const MAX_ROLE_LEN: usize = 64;

pub(crate) const MAX_FORMAT_NAME_LEN: usize = 64;

pub(crate) const MAX_CURRENCY_LEN: usize = 8;

/// Maximum sheets in one workbook (tenant settings may lower
/// it, never raise it past this bound).
pub(crate) const MAX_SHEETS_PER_WORKBOOK: usize = 200;

/// Maximum columns in one sheet (A..IV, zero-based 0..255).
pub(crate) const MAX_COLUMNS: i64 = 256;

/// Maximum rows in one sheet.
pub(crate) const MAX_ROWS: i64 = 10_000;

/// Maximum cells in one batch-set request.
pub(crate) const MAX_CELLS_PER_BATCH: usize = 1000;

/// Maximum cells a CSV import may write in one request.
pub(crate) const MAX_CSV_ROWS: usize = 10_000;

/// Maximum versions kept per sheet; older versions are pruned
/// when a new one is created past this bound.
pub(crate) const MAX_VERSIONS_PER_SHEET: i64 = 100;

/// Public-link token: 48 random alphanumeric characters
/// (~285 bits), following the sign module pattern. The raw
/// token is returned exactly once (when the link is enabled)
/// and is never stored - only its SHA-256 hash.
pub(crate) const PUBLIC_TOKEN_LEN: usize = 48;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_SPREADSHEETS_VIEW: &str = "spreadsheets.view";

pub(crate) const PERM_SPREADSHEETS_MANAGE: &str = "spreadsheets.manage";

/// Hook event names this module emits.
pub(crate) const EVENT_WORKBOOK_CREATED: &str = "spreadsheets.workbook.created";

pub(crate) const EVENT_WORKBOOK_UPDATED: &str = "spreadsheets.workbook.updated";

pub(crate) const EVENT_WORKBOOK_DELETED: &str = "spreadsheets.workbook.deleted";

pub(crate) const EVENT_CELL_UPDATED: &str = "spreadsheets.cell.updated";

pub(crate) const EVENT_SHARED: &str = "spreadsheets.shared";

pub(crate) const EVENT_VERSION_RESTORED: &str = "spreadsheets.version.restored";

/// Hook event names this module listens to.
pub(crate) const EVENT_USER_DELETED: &str = "user.deleted";

/// Marker written into `created_by` / `owner_id` when the
/// owning user is deleted. Rows are kept (the workbook history
/// stays auditable); only the identity is anonymized.
pub(crate) const DELETED_USER_ID: &str = "deleted-user";

/// Default share role when the caller names none.
pub(crate) const DEFAULT_SHARE_ROLE: &str = "viewer";

/// Default currency for the `currency` number format.
pub(crate) const DEFAULT_CURRENCY: &str = "USD";

/// Error tokens a formula (or an error cell) can produce.
/// These are values, not HTTP statuses: a cell whose formula
/// fails keeps the token as its computed value.
pub(crate) const ERR_VALUE: &str = "#VALUE!";

pub(crate) const ERR_DIV_ZERO: &str = "#DIV/0!";

pub(crate) const ERR_REF: &str = "#REF!";

pub(crate) const ERR_NAME: &str = "#NAME?";

pub(crate) const ERR_CIRCULAR: &str = "#CIRC!";

pub(crate) const ERR_NUM: &str = "#NUM!";

/// True when `value` is one of the six error tokens above.
pub fn is_error_token(value: &str) -> bool {
    matches!(
        value,
        ERR_VALUE | ERR_DIV_ZERO | ERR_REF | ERR_NAME | ERR_CIRCULAR | ERR_NUM
    )
}

// Decimal: integer mantissa + scale, no floating point

/// A decimal number as an integer mantissa plus a scale: the
/// value is `mantissa / 10^scale`. `12.50` is mantissa `1250`,
/// scale `2`; parsing keeps the written scale, arithmetic
/// normalizes trailing zeroes away. All operations are checked
/// - overflow is an error, never a silent wrap or a float.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Decimal {
    pub mantissa: i128,
    pub scale: u32,
}

/// Largest scale kept after an operation. Division always
/// produces this scale (10 fractional digits); multiplication
/// past it is rejected as overflow rather than silently
/// truncated.
pub const DECIMAL_MAX_SCALE: u32 = 28;

impl Decimal {
    pub fn zero() -> Self {
        Self {
            mantissa: 0,
            scale: 0,
        }
    }

    pub fn one() -> Self {
        Self {
            mantissa: 1,
            scale: 0,
        }
    }

    pub fn from_int(value: i64) -> Self {
        Self {
            mantissa: value as i128,
            scale: 0,
        }
    }

    /// True when the value is a whole number (no fractional
    /// part after normalization).
    pub fn is_integer(&self) -> bool {
        let normalized = self.normalized();
        normalized.scale == 0
    }

    /// The value as an `i64` when it is whole and fits, else
    /// `None`.
    pub fn as_int(&self) -> Option<i64> {
        if !self.is_integer() {
            return None;
        }
        i64::try_from(self.normalized().mantissa).ok()
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

    pub fn checked_add(&self, other: &Self) -> Option<Self> {
        let scale = self.scale.max(other.scale);
        let left = self.rescale(scale)?;
        let right = other.rescale(scale)?;
        Some(Self {
            mantissa: left.mantissa.checked_add(right.mantissa)?,
            scale,
        })
    }

    pub fn checked_sub(&self, other: &Self) -> Option<Self> {
        let scale = self.scale.max(other.scale);
        let left = self.rescale(scale)?;
        let right = other.rescale(scale)?;
        Some(Self {
            mantissa: left.mantissa.checked_sub(right.mantissa)?,
            scale,
        })
    }

    pub fn checked_mul(&self, other: &Self) -> Option<Self> {
        let scale = self.scale.checked_add(other.scale)?;
        if scale > DECIMAL_MAX_SCALE {
            return None;
        }
        Some(Self {
            mantissa: self.mantissa.checked_mul(other.mantissa)?,
            scale,
        })
    }

    /// Division with 10 fractional digits: the result scale is
    /// always 10 (plus the inputs' scales cancel out exactly).
    /// Returns `None` on division by zero or overflow.
    pub fn checked_div(&self, other: &Self) -> Option<Self> {
        if other.mantissa == 0 {
            return None;
        }
        // value = (ma / 10^sa) / (mb / 10^sb)
        //       = ma * 10^sb / (mb * 10^sa); with result scale 10,
        //       mr = ma * 10^(sb + 10) / (mb * 10^sa).
        let numerator = self
            .mantissa
            .checked_mul(Self::pow10(other.scale.checked_add(10)?)?)?;
        let denominator = other.mantissa.checked_mul(Self::pow10(self.scale)?)?;
        Some(Self {
            mantissa: numerator / denominator,
            scale: 10,
        })
    }

    /// Remainder: the sign follows the dividend, like `%` on
    /// integers. Only defined for whole-number operands.
    pub fn checked_rem(&self, other: &Self) -> Option<Self> {
        let left = self.as_int()?;
        let right = other.as_int()?;
        if right == 0 {
            return None;
        }
        Some(Self::from_int(left % right))
    }

    /// Integer power: the exponent must be whole and within
    /// [-100, 100]. A negative exponent inverts the result.
    pub fn checked_pow(&self, exponent: i64) -> Option<Self> {
        if !(-100..=100).contains(&exponent) {
            return None;
        }
        if exponent == 0 {
            return Some(Self::one());
        }
        let mut result = Self::one();
        let base = *self;
        let count = exponent.unsigned_abs();
        for _ in 0..count {
            result = result.checked_mul(&base)?;
        }
        if exponent < 0 {
            result = Self::one().checked_div(&result)?;
        }
        Some(result)
    }

    /// Round to `places` fractional digits, half away from
    /// zero (so 2.5 -> 3 and -2.5 -> -3). `places` must be
    /// within [0, 10].
    pub fn round(&self, places: u32) -> Option<Self> {
        if places > 10 {
            return None;
        }
        if self.scale <= places {
            return self.rescale(places);
        }
        let factor = Self::pow10(self.scale - places)?;
        let sign: i128 = if self.mantissa < 0 { -1 } else { 1 };
        let magnitude = self.mantissa.abs();
        let kept = magnitude / factor;
        let remainder = magnitude % factor;
        let rounded = if remainder * 2 >= factor {
            kept + 1
        } else {
            kept
        };
        Some(Self {
            mantissa: sign * rounded,
            scale: places,
        })
    }

    pub fn abs(&self) -> Self {
        Self {
            mantissa: self.mantissa.abs(),
            scale: self.scale,
        }
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
            // Exponent shifts the point right: pad with zeroes.
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

// A1 addresses

/// Parse an A1 address (`B12`, `$B$12`, `AA100`) into a
/// zero-based `(row, column)`. Column letters are case
/// insensitive; `$` markers are stripped. Returns `None` for
/// anything that is not a plain address.
pub fn parse_address(raw: &str) -> Option<(i64, i64)> {
    let text = raw.trim().replace('$', "");
    if text.is_empty() || text.len() > 16 {
        return None;
    }
    let split = text
        .find(|c: char| c.is_ascii_digit())
        .unwrap_or(text.len());
    let (letters, digits) = text.split_at(split);
    if letters.is_empty() || digits.is_empty() {
        return None;
    }
    if !letters.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut column: i64 = 0;
    for byte in letters.bytes() {
        let value = (byte.to_ascii_uppercase() - b'A') as i64 + 1;
        column = column.checked_mul(26)?.checked_add(value)?;
    }
    column -= 1;
    let row: i64 = digits.parse().ok()?;
    if row < 1 {
        return None;
    }
    Some((row - 1, column))
}

/// Format a zero-based `(row, column)` as an A1 address.
pub fn format_address(row: i64, column: i64) -> String {
    let mut letters = String::new();
    let mut value = column + 1;
    while value > 0 {
        let remainder = (value - 1) % 26;
        letters.insert(0, (b'A' + remainder as u8) as char);
        value = (value - 1) / 26;
    }
    format!("{letters}{}", row + 1)
}

/// Parse a range (`A1:B3`) into normalized zero-based corners
/// `((top_row, left_col), (bottom_row, right_col))`. A single
/// address parses as a one-cell range.
pub fn parse_range(raw: &str) -> Option<((i64, i64), (i64, i64))> {
    match raw.split_once(':') {
        Some((first, second)) => {
            let (row_a, col_a) = parse_address(first)?;
            let (row_b, col_b) = parse_address(second)?;
            Some((
                (row_a.min(row_b), col_a.min(col_b)),
                (row_a.max(row_b), col_a.max(col_b)),
            ))
        }
        None => {
            let cell = parse_address(raw)?;
            Some((cell, cell))
        }
    }
}

/// True when the address is inside the sheet bounds
/// (`MAX_ROWS` rows, `MAX_COLUMNS` columns).
pub fn address_in_bounds(row: i64, column: i64) -> bool {
    (0..MAX_ROWS).contains(&row) && (0..MAX_COLUMNS).contains(&column)
}

/// True when `text` looks like a date in `YYYY-MM-DD` form.
/// The evaluator and the cell writer use this to classify a
/// typed value; full calendar validity is checked by chrono.
pub fn looks_like_date(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    if parts.len() != 3 {
        return false;
    }
    parts[0].len() == 4
        && parts[1].len() == 2
        && parts[2].len() == 2
        && parts
            .iter()
            .all(|part| part.bytes().all(|b| b.is_ascii_digit()))
}

/// True when `text` is a valid `YYYY-MM-DD` calendar date.
pub fn valid_date(text: &str) -> bool {
    if !looks_like_date(text) {
        return false;
    }
    chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok()
}

/// Classify a typed (non-formula) cell value: empty, boolean
/// (`TRUE` / `FALSE`, case insensitive), number, date, or text.
pub fn classify_value(raw: &str) -> CellKind {
    let text = raw.trim();
    if text.is_empty() {
        return CellKind::Empty;
    }
    if text.eq_ignore_ascii_case("true") || text.eq_ignore_ascii_case("false") {
        return CellKind::Boolean;
    }
    if Decimal::parse(text).is_some() {
        return CellKind::Number;
    }
    if valid_date(text) {
        return CellKind::Date;
    }
    CellKind::Text
}

/// Normalize a typed value's computed form: numbers become
/// their canonical decimal string, booleans become `TRUE` /
/// `FALSE`, everything else is the trimmed text.
pub fn computed_for_typed(raw: &str, kind: CellKind) -> String {
    let text = raw.trim();
    match kind {
        CellKind::Number => Decimal::parse(text)
            .map(|value| value.to_decimal_string())
            .unwrap_or_else(|| text.to_string()),
        CellKind::Boolean => {
            if text.eq_ignore_ascii_case("true") {
                "TRUE".to_string()
            } else {
                "FALSE".to_string()
            }
        }
        _ => text.to_string(),
    }
}

// Types

/// Cell value kind. Serialized snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellKind {
    Text,
    Number,
    Boolean,
    Date,
    Empty,
    Formula,
    Error,
}

impl Default for CellKind {
    fn default() -> Self {
        Self::Empty
    }
}

impl CellKind {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Date => "date",
            Self::Empty => "empty",
            Self::Formula => "formula",
            Self::Error => "error",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "text" => Some(Self::Text),
            "number" => Some(Self::Number),
            "boolean" => Some(Self::Boolean),
            "date" => Some(Self::Date),
            "empty" => Some(Self::Empty),
            "formula" => Some(Self::Formula),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// One number-format definition from tenant settings: a name
/// the cell references plus the pattern that renders it. The
/// pattern is data - the module never hard-codes a format list
/// in code; the defaults live in the settings row and in the
/// migration seed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NumberFormat {
    pub name: String,
    pub pattern: String,
}

/// One workbook (spreadsheet document). `public_token_hash` is
/// never serialized: only the SHA-256 of the public-link token
/// is stored, and the raw token is returned exactly once when
/// the link is enabled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workbook {
    pub id: Uuid,
    pub tenant_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub owner_id: String,
    #[serde(default)]
    pub public_link_enabled: bool,
    #[serde(skip_serializing)]
    pub public_token_hash: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One sheet (tab) of a workbook. `position` orders the tabs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sheet {
    pub id: Uuid,
    pub tenant_id: String,
    pub workbook_id: Uuid,
    pub title: String,
    pub position: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One cell. `raw_value` is what the user typed - a formula
/// starts with `=` and is kept verbatim. `computed_value` is
/// the evaluated result: a decimal string for numbers, `TRUE`
/// / `FALSE` for booleans, an ISO date for dates, the text
/// itself for text, or an error token. `computed_kind` names
/// the result kind; for a formula cell the stored `kind` is
/// `formula` and the result kind lives in `computed_kind`.
/// `number_format` names a format from tenant settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SheetCell {
    pub id: Uuid,
    pub tenant_id: String,
    pub workbook_id: Uuid,
    pub sheet_id: Uuid,
    pub address: String,
    pub row_index: i64,
    pub column_index: i64,
    pub kind: CellKind,
    pub raw_value: String,
    pub computed_value: String,
    pub computed_kind: CellKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_format: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One cell as stored in a version snapshot: address plus the
/// typed value, enough to restore the sheet exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CellSnapshot {
    pub address: String,
    pub kind: CellKind,
    pub raw_value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_format: Option<String>,
}

/// One share row: a workbook shared with one user. `role` is
/// data (the tenant settings list the allowed roles; the
/// defaults are `viewer` and `editor`). `shared_with` is a
/// user id; `shared_with_email` is kept for a person with no
/// account yet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Share {
    pub id: Uuid,
    pub tenant_id: String,
    pub workbook_id: Uuid,
    pub shared_with: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_with_email: Option<String>,
    pub role: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One version: a full snapshot of one sheet plus the change
/// log of the batch that created it. `version_number` counts
/// up per sheet; `cells_snapshot` holds every non-empty cell
/// so a restore reproduces the sheet exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SheetVersion {
    pub id: Uuid,
    pub tenant_id: String,
    pub workbook_id: Uuid,
    pub sheet_id: Uuid,
    pub version_number: i64,
    pub created_by: String,
    pub change_summary: String,
    pub cells_snapshot: Vec<CellSnapshot>,
    pub created_at: DateTime<Utc>,
}

/// Per-tenant spreadsheets settings. Number formats and share
/// roles are data: `number_formats` is the list of named
/// formats a cell may reference, `share_roles` is the list of
/// roles a share may carry, and `default_currency` names the
/// currency the `currency` format renders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpreadsheetSettings {
    pub default_currency: String,
    pub number_formats: Vec<NumberFormat>,
    pub share_roles: Vec<String>,
    pub default_share_role: String,
    pub public_links_allowed: bool,
    pub max_sheets_per_workbook: i64,
}

impl Default for SpreadsheetSettings {
    fn default() -> Self {
        Self {
            default_currency: DEFAULT_CURRENCY.to_string(),
            number_formats: vec![
                NumberFormat {
                    name: "general".to_string(),
                    pattern: "general".to_string(),
                },
                NumberFormat {
                    name: "0.00".to_string(),
                    pattern: "0.00".to_string(),
                },
                NumberFormat {
                    name: "#,##0.00".to_string(),
                    pattern: "#,##0.00".to_string(),
                },
                NumberFormat {
                    name: "percent".to_string(),
                    pattern: "percent".to_string(),
                },
                NumberFormat {
                    name: "currency".to_string(),
                    pattern: "currency".to_string(),
                },
            ],
            share_roles: vec!["viewer".to_string(), "editor".to_string()],
            default_share_role: DEFAULT_SHARE_ROLE.to_string(),
            public_links_allowed: false,
            max_sheets_per_workbook: 50,
        }
    }
}

// Request bodies

#[derive(Debug, Deserialize)]
pub struct CreateWorkbookRequest {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Titles for the initial sheets; defaults to one sheet
    /// named `Sheet1` when empty.
    #[serde(default)]
    pub sheets: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWorkbookRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    /// Enable or disable the public link. Enabling mints a
    /// fresh token (returned once); disabling revokes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_link_enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSheetRequest {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSheetRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
}

/// One cell write: an A1 address plus the typed value. A value
/// starting with `=` is a formula; an empty value clears the
/// cell.
#[derive(Debug, Deserialize, Clone)]
pub struct CellInput {
    pub address: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_format: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SetCellRequest {
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_format: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct BatchSetCellsRequest {
    pub cells: Vec<CellInput>,
}

#[derive(Debug, Deserialize)]
pub struct EvaluateFormulaRequest {
    /// The formula to evaluate, with or without a leading `=`.
    pub formula: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateShareRequest {
    /// User id to share with.
    pub shared_with: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_with_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateShareRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ImportCsvRequest {
    /// The CSV text to import (quoted fields supported).
    pub csv: String,
    /// First row is a header row; its values are imported as
    /// text in row 1 when false, skipped when true.
    #[serde(default)]
    pub has_header: bool,
    /// Top-left address the import starts at; defaults to A1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_address: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_currency: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_formats: Option<Vec<NumberFormat>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_roles: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_share_role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_links_allowed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_sheets_per_workbook: Option<i64>,
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
    fn decimal_arithmetic_is_exact() {
        let a = Decimal::parse("0.1").unwrap();
        let b = Decimal::parse("0.2").unwrap();
        assert_eq!(a.checked_add(&b).unwrap().to_decimal_string(), "0.3");
        assert_eq!(
            Decimal::parse("1.5")
                .unwrap()
                .checked_mul(&Decimal::parse("2").unwrap())
                .unwrap()
                .to_decimal_string(),
            "3"
        );
        assert_eq!(
            Decimal::parse("10")
                .unwrap()
                .checked_div(&Decimal::parse("4").unwrap())
                .unwrap()
                .to_decimal_string(),
            "2.5"
        );
        assert_eq!(
            Decimal::parse("7")
                .unwrap()
                .checked_sub(&Decimal::parse("10").unwrap())
                .unwrap()
                .to_decimal_string(),
            "-3"
        );
        assert_eq!(
            Decimal::parse("7")
                .unwrap()
                .checked_rem(&Decimal::parse("3").unwrap())
                .unwrap()
                .to_decimal_string(),
            "1"
        );
        assert_eq!(
            Decimal::parse("2")
                .unwrap()
                .checked_pow(10)
                .unwrap()
                .to_decimal_string(),
            "1024"
        );
        assert_eq!(
            Decimal::parse("2")
                .unwrap()
                .checked_pow(-1)
                .unwrap()
                .to_decimal_string(),
            "0.5"
        );
        assert!(Decimal::parse("1")
            .unwrap()
            .checked_div(&Decimal::zero())
            .is_none());
        assert_eq!(
            Decimal::parse("2.5")
                .unwrap()
                .round(0)
                .unwrap()
                .to_decimal_string(),
            "3"
        );
        assert_eq!(
            Decimal::parse("-2.5")
                .unwrap()
                .round(0)
                .unwrap()
                .to_decimal_string(),
            "-3"
        );
        assert_eq!(
            Decimal::parse("3.14159")
                .unwrap()
                .round(2)
                .unwrap()
                .to_decimal_string(),
            "3.14"
        );
        assert_eq!(
            Decimal::parse("3.145")
                .unwrap()
                .round(2)
                .unwrap()
                .to_decimal_string(),
            "3.15"
        );
    }

    #[test]
    fn decimal_ordering() {
        assert!(Decimal::parse("1.5").unwrap() > Decimal::parse("1.25").unwrap());
        assert!(Decimal::parse("-1").unwrap() < Decimal::zero());
        assert_eq!(
            Decimal::parse("1.0").unwrap(),
            Decimal::parse("1.00").unwrap()
        );
        assert_eq!(Decimal::from_int(42).as_int(), Some(42));
        assert_eq!(Decimal::parse("42.5").unwrap().as_int(), None);
    }

    #[test]
    fn a1_addresses_parse_and_format() {
        assert_eq!(parse_address("A1"), Some((0, 0)));
        assert_eq!(parse_address("B12"), Some((11, 1)));
        assert_eq!(parse_address("$B$12"), Some((11, 1)));
        assert_eq!(parse_address("AA1"), Some((0, 26)));
        assert_eq!(parse_address("IV1"), Some((0, 255)));
        assert_eq!(parse_address("a1"), Some((0, 0)));
        assert_eq!(parse_address("A0"), None);
        assert_eq!(parse_address("A"), None);
        assert_eq!(parse_address("1"), None);
        assert_eq!(parse_address(""), None);
        assert_eq!(parse_address("A1B"), None);
        assert_eq!(format_address(0, 0), "A1");
        assert_eq!(format_address(11, 1), "B12");
        assert_eq!(format_address(0, 26), "AA1");
        assert_eq!(format_address(0, 255), "IV1");
        assert_eq!(parse_range("B2:D4"), Some(((1, 1), (3, 3))));
        assert_eq!(parse_range("D4:B2"), Some(((1, 1), (3, 3))));
        assert_eq!(parse_range("C3"), Some(((2, 2), (2, 2))));
        assert!(address_in_bounds(0, 0));
        assert!(address_in_bounds(9999, 255));
        assert!(!address_in_bounds(10000, 0));
        assert!(!address_in_bounds(0, 256));
    }

    #[test]
    fn value_classification() {
        assert_eq!(classify_value(""), CellKind::Empty);
        assert_eq!(classify_value("  "), CellKind::Empty);
        assert_eq!(classify_value("TRUE"), CellKind::Boolean);
        assert_eq!(classify_value("false"), CellKind::Boolean);
        assert_eq!(classify_value("12.50"), CellKind::Number);
        assert_eq!(classify_value("-3"), CellKind::Number);
        assert_eq!(classify_value("2026-11-22"), CellKind::Date);
        assert_eq!(classify_value("2026-13-40"), CellKind::Text);
        assert_eq!(classify_value("hello"), CellKind::Text);
        assert_eq!(computed_for_typed("12.50", CellKind::Number), "12.5");
        assert_eq!(computed_for_typed("true", CellKind::Boolean), "TRUE");
        assert_eq!(computed_for_typed("FALSE", CellKind::Boolean), "FALSE");
    }

    #[test]
    fn settings_defaults_are_data_driven() {
        let settings = SpreadsheetSettings::default();
        assert_eq!(settings.default_currency, "USD");
        assert_eq!(settings.share_roles, vec!["viewer", "editor"]);
        assert_eq!(settings.default_share_role, "viewer");
        assert!(!settings.public_links_allowed);
        assert_eq!(settings.max_sheets_per_workbook, 50);
        assert!(settings
            .number_formats
            .iter()
            .any(|format| format.name == "currency"));
    }

    #[test]
    fn error_tokens_are_recognized() {
        for token in ["#VALUE!", "#DIV/0!", "#REF!", "#NAME?", "#CIRC!", "#NUM!"] {
            assert!(is_error_token(token));
        }
        assert!(!is_error_token("42"));
        assert!(!is_error_token(""));
    }
}
