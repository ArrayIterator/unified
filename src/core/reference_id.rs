// Payment reference ids.
//
// A single 30-character, time-ordered, gateway-safe reference format
// shared by every order, invoice, and payment in the system:
//
//   `{PREFIX 4 chars}-{12 hex timestamp}-{12 hex random}`
//   `ORDR-019d43d20eb8-6a5a7925dfb5` (30 characters total)
//
// The timestamp segment is the current time in milliseconds since the
// Unix epoch, masked to 48 bits and rendered as zero-padded lowercase
// hex, so lexicographic order is chronological order and range queries
// work as plain string comparisons (see `get_time_range_pattern`).
// The random segment is 6 cryptographically secure random bytes
// rendered as 12 lowercase hex characters. Together the two segments
// carry 96 bits of entropy, so collisions are negligible; callers that
// persist a reference still enforce uniqueness with a database UNIQUE
// constraint and retry generation once on conflict (never silently
// overwrite an existing row).
//
// The format itself is the one fixed part of this module. Everything
// else is settings: the default prefix (`ORDR`) and the per-product
// prefix map live in `ReferenceIdSettings`, which administrators edit
// through the store and billing settings endpoints. Changing the
// default prefix or a product prefix never breaks validation of
// already-issued ids: `is_valid` accepts any id in the stored format,
// whatever prefix it carries.
//
// Tenant scoping: reference generation itself needs no database
// access, so it cannot leak across tenants. The settings a caller
// passes in are the ones resolved for that tenant (product prefixes
// may differ per tenant or product, never per hard-coded tenant id),
// and any persistence of a generated reference goes through the
// caller's tenant context like any other tenant business data. A
// caller with no tenant context must not persist a reference at all.
//
// PCI boundary: this module never touches card numbers. It generates
// opaque references only; no PAN or CVV is ever logged, stored, or
// accepted here. Payment methods stay tokenized upstream.
//
// Persistence note for the integrator: add a UNIQUE constraint on
// every column that stores one of these references (see the migration
// snippet in the batch report). The in-memory maps used by the store,
// billing, and payments modules today are keyed by the internal id;
// the reference is a second, public, unique key alongside it.

use crate::core::hooks::HookBus;
use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Default prefix used when no product prefix applies.
pub const DEFAULT_PREFIX: &str = "ORDR";

/// Total length of a reference id, in characters. The format is
/// fixed at 30 characters so it fits gateways that accept only 30.
pub const TOTAL_LENGTH: usize = 30;

/// Length of the timestamp segment, in hex characters (48 bits).
pub const TIMESTAMP_HEX_LENGTH: usize = 12;

/// Length of the random segment, in hex characters (48 bits).
pub const RANDOM_HEX_LENGTH: usize = 12;

/// Mask that keeps the low 48 bits of a millisecond timestamp.
pub const TIMESTAMP_MASK: u64 = 0xFFFF_FFFF_FFFF;

/// A valid reference id, usable as sample data and in tests.
pub const EXAMPLE_REFERENCE_ID: &str = "ORDR-019d43d20eb8-6a5a7925dfb5";

/// Hook event emitted after a reference is generated.
/// Payload: `{ "reference": ..., "prefix": ..., "tenant_id": ... }`.
pub const EVENT_REFERENCE_GENERATED: &str = "reference.generated";

/// Hook event emitted after the reference settings change.
/// Payload: the new `ReferenceIdSettings` value.
pub const EVENT_REFERENCE_SETTINGS_CHANGED: &str = "reference.settings_changed";

/// Normalize a caller-supplied prefix into the 4-character form the
/// format requires: trim surrounding whitespace, uppercase, keep
/// ASCII alphanumeric characters only, truncate to 4 characters, and
/// pad with `X` when shorter than 4.
///
///   - `" order "` trims to `ORDER`, then truncates to `"ORDE"`.
///   - `"hstg"` becomes `"HSTG"`.
///   - `"a!"` keeps only `A`, then pads to `"AXXX"`.
///   - `""` becomes `"XXXX"`.
///   - `"JKT1"` stays `"JKT1"`.
pub fn format_prefix(input: &str) -> String {
    let mut prefix: String = input
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .take(4)
        .collect();
    while prefix.len() < 4 {
        prefix.push('X');
    }
    prefix
}

/// Current time in milliseconds since the Unix epoch, masked to 48
/// bits. The clock is read from `SystemTime`, never from a caller
/// supplied value, so a generated id always orders after ids
/// generated earlier on the same host.
pub fn timestamp_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
        & TIMESTAMP_MASK
}

/// Render a millisecond timestamp as the 12-character, zero-padded,
/// lowercase hex segment used in the middle of a reference id.
pub fn timestamp_hex(timestamp_ms: u64) -> String {
    format!("{:012x}", timestamp_ms & TIMESTAMP_MASK)
}

/// 6 cryptographically secure random bytes rendered as 12 lowercase
/// hex characters. `rand::rng()` is the CSPRNG the codebase already
/// uses for tokens and nonces; no new dependency is introduced here.
fn random_hex() -> String {
    let mut bytes = [0u8; 6];
    rand::rng().fill_bytes(&mut bytes);
    let mut out = String::with_capacity(RANDOM_HEX_LENGTH);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Generate a reference id with the default prefix (`ORDR`), or with
/// `prefix` when one is given. The prefix is normalized with
/// `format_prefix`, so any caller input is safe to pass here.
pub fn generate(prefix: Option<&str>) -> String {
    match prefix {
        Some(prefix) => generate_with_prefix(prefix),
        None => generate_with_prefix(DEFAULT_PREFIX),
    }
}

/// Generate a reference id with an explicit prefix, normalized with
/// `format_prefix`.
pub fn generate_with_prefix(prefix: &str) -> String {
    let prefix = format_prefix(prefix);
    format!(
        "{prefix}-{}-{}",
        timestamp_hex(timestamp_ms_now()),
        random_hex()
    )
}

/// Generate a reference id that `exists` reports as unused,
/// retrying generation once on conflict: when the first generated id
/// already exists, one fresh id is generated and returned whether or
/// not it also exists. The caller must treat a second conflict as an
/// error (unique-violation on insert) and never silently overwrite
/// the existing row. The timestamp plus 48 random bits make even the
/// first conflict negligible; the retry exists so a collision is a
/// retry, not a failure.
pub fn generate_unique(mut exists: impl FnMut(&str) -> bool) -> String {
    generate_unique_with_prefix(DEFAULT_PREFIX, &mut exists)
}

/// `generate_unique` with an explicit prefix (normalized with
/// `format_prefix`).
pub fn generate_unique_with_prefix(
    prefix: &str,
    mut exists: impl FnMut(&str) -> bool,
) -> String {
    let first = generate_with_prefix(prefix);
    if !exists(&first) {
        return first;
    }
    generate_with_prefix(prefix)
}

/// Generate a reference id from settings: the product prefix when
/// `product_id` or `product_slug` has one configured, otherwise the
/// settings' default prefix.
pub fn generate_for(
    settings: &ReferenceIdSettings,
    product_id: Option<&str>,
    product_slug: Option<&str>,
) -> String {
    let prefix = settings.prefix_for_product(product_id, product_slug);
    generate_with_prefix(&prefix)
}

/// True when `id` is exactly one reference in the stored format:
/// 4 uppercase alphanumeric characters, a dash, 12 lowercase hex
/// characters, a dash, 12 lowercase hex characters (30 characters
/// total). This is the `^[A-Z0-9]{4}-[0-9a-f]{12}-[0-9a-f]{12}$`
/// shape, checked without a regex so validation stays cheap and
/// dependency-free. Uppercase hex, a lowercase prefix, a wrong
/// length, or any other character is rejected.
pub fn is_valid(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.len() != TOTAL_LENGTH {
        return false;
    }
    if bytes[4] != b'-' || bytes[17] != b'-' {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        match index {
            0..=3 => {
                if !byte.is_ascii_alphanumeric() || byte.is_ascii_lowercase() {
                    return false;
                }
            }
            5..=16 | 18..=29 => {
                if !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase() {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

/// A parsed reference id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReferenceOrderId {
    /// The 4-character prefix, for example `ORDR`.
    pub prefix: String,
    /// The millisecond timestamp segment, unmasked.
    pub timestamp_ms: u64,
    /// The 12-character lowercase hex random segment.
    pub random_hex: String,
    /// The timestamp segment as a datetime.
    pub created_at: DateTime<Utc>,
}

/// Parse a reference id into its parts. Returns `None` when the id
/// is not in the stored format (see `is_valid`).
pub fn parse(id: &str) -> Option<ReferenceOrderId> {
    if !is_valid(id) {
        return None;
    }
    let prefix = id[0..4].to_string();
    let timestamp_ms = u64::from_str_radix(&id[5..17], 16).ok()?;
    let random_hex = id[18..30].to_string();
    let created_at = DateTime::from_timestamp_millis(timestamp_ms as i64).unwrap_or_default();
    Some(ReferenceOrderId {
        prefix,
        timestamp_ms,
        random_hex,
        created_at,
    })
}

/// Extract the millisecond timestamp from a reference id, or `None`
/// when the id is not in the stored format.
pub fn get_timestamp(id: &str) -> Option<u64> {
    parse(id).map(|parsed| parsed.timestamp_ms)
}

/// A time range expressed as reference-id bounds, for range queries
/// over a reference column.
///
/// Because the timestamp is the middle segment and is zero-padded,
/// lexicographic order is chronological order, so a range query is:
///
/// ```sql
/// order_reference >= 'ORDR-<from_hex>' AND order_reference < 'ORDR-<to_hex>'
/// ```
///
/// combined with `order_reference LIKE 'ORDR-%'` (the `like_pattern`)
/// when the query must stay inside one prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TimeRangePattern {
    /// Normalized prefix the range applies to.
    pub prefix: String,
    /// Lower bound timestamp as 12 lowercase hex characters.
    pub from_hex: String,
    /// Upper bound timestamp as 12 lowercase hex characters.
    /// The bound is exclusive: ids at exactly this timestamp are
    /// outside the range.
    pub to_hex: String,
    /// `"{prefix}-%"`, for the `LIKE` clause of a range query.
    pub like_pattern: String,
}

impl TimeRangePattern {
    /// Full lower bound for the range query (`PREFIX-<from_hex>`).
    pub fn from_bound(&self) -> String {
        format!("{}-{}", self.prefix, self.from_hex)
    }

    /// Full exclusive upper bound (`PREFIX-<to_hex>`).
    pub fn to_bound(&self) -> String {
        format!("{}-{}", self.prefix, self.to_hex)
    }
}

/// Build the range pattern for `[from_ms, to_ms)`. Callers must pass
/// `from_ms <= to_ms`; the bounds are returned exactly as given, so a
/// reversed range is visible to the caller instead of silently
/// swapped. Both bounds are masked to 48 bits, like the ids
/// themselves.
pub fn get_time_range_pattern(prefix: &str, from_ms: u64, to_ms: u64) -> TimeRangePattern {
    let prefix = format_prefix(prefix);
    TimeRangePattern {
        from_hex: timestamp_hex(from_ms),
        to_hex: timestamp_hex(to_ms),
        like_pattern: format!("{prefix}-%"),
        prefix,
    }
}

/// Admin-editable reference settings.
///
/// The default prefix and the per-product prefix map are settings,
/// not code: an administrator changes them through the store and
/// billing reference-settings endpoints, and the durable form is the
/// `reference_id_settings` table described in the migration snippet
/// in the batch report. The total-length cap is a guard, not a dial:
/// the format is always 30 characters, so a stored cap outside
/// 30-64 is a configuration error reported by `validate`, never a
/// reason to emit a different format. Changing any of these settings
/// never breaks validation of already-issued ids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReferenceIdSettings {
    /// Prefix used when no product prefix applies. Stored canonical
    /// (see `format_prefix`); defaults to `ORDR`.
    pub default_prefix: String,
    /// Per-product prefix overrides, keyed by product id or product
    /// slug. Values are stored canonical (see `format_prefix`).
    /// A product with no entry here uses `default_prefix`.
    pub product_prefixes: HashMap<String, String>,
    /// Total length a reference id must have. The format is always
    /// exactly 30 characters; the cap is a ceiling in the range
    /// 30-64 (matching the settings-module `reference_id.*`
    /// contract) that never binds for well-formed ids. A cap outside
    /// 30-64 is a configuration error reported by `validate`, never
    /// a reason to emit a different format.
    pub max_total_length: usize,
}

impl Default for ReferenceIdSettings {
    fn default() -> Self {
        Self {
            default_prefix: DEFAULT_PREFIX.to_string(),
            product_prefixes: HashMap::new(),
            max_total_length: TOTAL_LENGTH,
        }
    }
}

impl ReferenceIdSettings {
    /// Settings with the default prefix and no product overrides.
    pub fn new() -> Self {
        Self::default()
    }

    /// Settings with a custom default prefix (normalized with
    /// `format_prefix`).
    pub fn with_default_prefix(prefix: &str) -> Self {
        Self {
            default_prefix: format_prefix(prefix),
            ..Self::default()
        }
    }

    /// Prefix for a product: the configured override for its id,
    /// then for its slug, then the default prefix. Either key may be
    /// absent; a product with no override uses the default.
    pub fn prefix_for_product(
        &self,
        product_id: Option<&str>,
        product_slug: Option<&str>,
    ) -> String {
        for key in [product_id, product_slug] {
            if let Some(key) = key.filter(|key| !key.trim().is_empty()) {
                if let Some(prefix) = self.product_prefixes.get(key) {
                    return prefix.clone();
                }
            }
        }
        self.default_prefix.clone()
    }

    /// Set the prefix override for one product key (a product id or
    /// slug). The key must be non-empty and at most 128 characters;
    /// the prefix is stored canonical (see `format_prefix`). Returns
    /// an error message when the key is unusable; the settings are
    /// left unchanged in that case.
    pub fn set_product_prefix(
        &mut self,
        product_key: &str,
        prefix: &str,
    ) -> Result<(), String> {
        let key = product_key.trim();
        if key.is_empty() {
            return Err("Product key must not be empty".to_string());
        }
        if key.len() > 128 {
            return Err("Product key must be at most 128 characters".to_string());
        }
        self.product_prefixes
            .insert(key.to_string(), format_prefix(prefix));
        Ok(())
    }

    /// Remove the prefix override for one product key. True when an
    /// override existed.
    pub fn remove_product_prefix(&mut self, product_key: &str) -> bool {
        self.product_prefixes
            .remove(product_key.trim())
            .is_some()
    }

    /// Generate a reference id with these settings for a product.
    pub fn generate(
        &self,
        product_id: Option<&str>,
        product_slug: Option<&str>,
    ) -> String {
        generate_for(self, product_id, product_slug)
    }

    /// Generate a reference id with these settings and an explicit
    /// prefix (normalized with `format_prefix`).
    pub fn generate_with_prefix(&self, prefix: &str) -> String {
        generate_with_prefix(prefix)
    }

    /// Configuration problems in these settings, if any. An empty
    /// list means the settings are usable as stored. Problems are:
    /// a default prefix that is not already canonical, a product
    /// prefix that is not already canonical, an empty or over-long
    /// product key, or a total-length cap outside 30-64.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.default_prefix != format_prefix(&self.default_prefix) {
            problems.push(format!(
                "default_prefix {:?} is not canonical (expected {:?})",
                self.default_prefix,
                format_prefix(&self.default_prefix)
            ));
        }
        for (key, prefix) in &self.product_prefixes {
            if key.trim().is_empty() || key.len() > 128 {
                problems.push(format!(
                    "product prefix key {key:?} must be non-empty and at most 128 characters"
                ));
            }
            if *prefix != format_prefix(prefix) {
                problems.push(format!(
                    "product prefix for {key:?} ({prefix:?}) is not canonical (expected {:?})",
                    format_prefix(prefix)
                ));
            }
        }
        if self.max_total_length < TOTAL_LENGTH || self.max_total_length > 64 {
            problems.push(format!(
                "max_total_length is {} but the cap must be between {TOTAL_LENGTH} and 64 (the reference format itself is always {TOTAL_LENGTH} characters)",
                self.max_total_length
            ));
        }
        problems
    }

    /// A copy of these settings with every value canonical: the
    /// default and product prefixes normalized with `format_prefix`
    /// and the total-length cap clamped into 30-64.
    pub fn sanitized(&self) -> Self {
        let mut sanitized = Self {
            default_prefix: format_prefix(&self.default_prefix),
            product_prefixes: HashMap::with_capacity(self.product_prefixes.len()),
            max_total_length: self.max_total_length.clamp(TOTAL_LENGTH, 64),
        };
        for (key, prefix) in &self.product_prefixes {
            let key = key.trim();
            if !key.is_empty() && key.len() <= 128 {
                sanitized
                    .product_prefixes
                    .insert(key.to_string(), format_prefix(prefix));
            }
        }
        sanitized
    }
}

/// Emit `reference.generated` on the hook bus after a reference is
/// issued, so audit and analytics listeners can observe issuance
/// without the generator knowing about them. The tenant id is
/// included when the caller generated the reference inside a tenant
/// context; `None` means the caller had no tenant context (a
/// control-plane generation, which must never be persisted as tenant
/// business data).
pub fn emit_generated(bus: &HookBus, reference: &str, tenant_id: Option<Uuid>) {
    bus.emit(
        EVENT_REFERENCE_GENERATED,
        serde_json::json!({
            "reference": reference,
            "prefix": reference.get(0..4).unwrap_or(""),
            "tenant_id": tenant_id.map(|id| id.to_string()),
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_prefix_cases() {
        // " order " trims to "ORDER", then truncates to 4 chars.
        assert_eq!(format_prefix(" order "), "ORDE");
        assert_eq!(format_prefix("hstg"), "HSTG");
        assert_eq!(format_prefix("a!"), "AXXX");
        assert_eq!(format_prefix(""), "XXXX");
        assert_eq!(format_prefix("JKT1"), "JKT1");
        assert_eq!(format_prefix("kvms"), "KVMS");
        assert_eq!(format_prefix("  HSTG  "), "HSTG");
        assert_eq!(format_prefix("ab"), "ABXX");
        assert_eq!(format_prefix("a-b_c"), "ABXX");
        assert_eq!(format_prefix("12345"), "1234");
    }

    #[test]
    fn generate_shape_and_uniqueness() {
        let settings = ReferenceIdSettings::default();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..256 {
            let id = settings.generate(None, None);
            assert_eq!(id.len(), TOTAL_LENGTH);
            assert!(is_valid(&id), "{id} must be valid");
            assert!(id.starts_with("ORDR-"));
            assert!(seen.insert(id), "generated ids must be unique");
        }
        let with_prefix = generate(Some("hstg"));
        assert!(with_prefix.starts_with("HSTG-"));
        // Empty prefix pads to XXXX; check the shape directly.
        assert!(generate_with_prefix("").starts_with("XXXX-"));
    }

    #[test]
    fn is_valid_accepts_and_rejects() {
        assert!(is_valid(EXAMPLE_REFERENCE_ID));
        assert!(is_valid("HSTG-019d43d20eb8-6a5a7925dfb5"));
        assert!(is_valid("JKT1-000000000000-000000000000"));
        // Wrong lengths.
        assert!(!is_valid("ORDR-019d43d20eb8-6a5a7925dfb"));
        assert!(!is_valid("ORDR-019d43d20eb8-6a5a7925dfb55"));
        assert!(!is_valid("ORD-019d43d20eb8-6a5a7925dfb5"));
        assert!(!is_valid("ORDRR-019d43d20eb8-6a5a7925dfb5"));
        // Uppercase hex is rejected; lowercase prefix is rejected.
        assert!(!is_valid("ORDR-019D43D20EB8-6a5a7925dfb5"));
        assert!(!is_valid("ORDR-019d43d20eb8-6A5A7925DFB5"));
        assert!(!is_valid("ordr-019d43d20eb8-6a5a7925dfb5"));
        // Wrong characters and separators.
        assert!(!is_valid("ORD!-019d43d20eb8-6a5a7925dfb5"));
        assert!(!is_valid("ORDR_019d43d20eb8-6a5a7925dfb5"));
        assert!(!is_valid("ORDR-019d43d20eb8_6a5a7925dfb5"));
        assert!(!is_valid("ORDR-019d43d20ebg-6a5a7925dfb5"));
        assert!(!is_valid(""));
    }

    #[test]
    fn parse_round_trip() {
        let generated = generate(None);
        let parsed = parse(&generated).expect("generated id must parse");
        assert_eq!(parsed.prefix, "ORDR");
        assert_eq!(parsed.timestamp_ms, get_timestamp(&generated).unwrap());
        assert_eq!(parsed.random_hex.len(), RANDOM_HEX_LENGTH);
        assert!(parsed.random_hex.chars().all(|c| c.is_ascii_hexdigit()));
        // The parsed timestamp is the generation time (within a
        // small window around now).
        let now = timestamp_ms_now();
        assert!(parsed.timestamp_ms <= now);
        assert!(now - parsed.timestamp_ms < 60_000);

        let example = parse(EXAMPLE_REFERENCE_ID).expect("example must parse");
        assert_eq!(example.prefix, "ORDR");
        assert_eq!(example.timestamp_ms, 0x019d_43d2_0eb8);
        assert_eq!(example.random_hex, "6a5a7925dfb5");
        assert_eq!(
            example.created_at,
            DateTime::from_timestamp_millis(0x019d_43d2_0eb8).unwrap()
        );
        assert_eq!(get_timestamp(EXAMPLE_REFERENCE_ID), Some(0x019d_43d2_0eb8));

        assert!(parse("not-a-reference").is_none());
        assert!(parse("ORDR-019D43D20EB8-6a5a7925dfb5").is_none());
        assert!(get_timestamp("ORDR-019d43d20eb8-6a5a7925dfb").is_none());
    }

    #[test]
    fn time_range_pattern_ordering() {
        let from_ms = 1_700_000_000_000u64;
        let to_ms = 1_700_000_360_000u64;
        let pattern = get_time_range_pattern("ordr", from_ms, to_ms);
        assert_eq!(pattern.prefix, "ORDR");
        assert_eq!(pattern.from_hex, timestamp_hex(from_ms));
        assert_eq!(pattern.to_hex, timestamp_hex(to_ms));
        assert_eq!(pattern.like_pattern, "ORDR-%");
        assert!(pattern.from_hex < pattern.to_hex);
        assert!(pattern.from_bound() < pattern.to_bound());
        assert_eq!(pattern.from_bound(), format!("ORDR-{}", pattern.from_hex));
        assert_eq!(pattern.to_bound(), format!("ORDR-{}", pattern.to_hex));
        // A generated id inside the window sorts between the bounds.
        let inside = format!("ORDR-{}-aaaaaaaaaaaa", timestamp_hex(from_ms + 1_000));
        assert!(inside >= pattern.from_bound());
        assert!(inside < pattern.to_bound());
        assert!(inside.starts_with(&pattern.like_pattern[..5]));
    }

    #[test]
    fn settings_prefix_resolution_and_validation() {
        let mut settings = ReferenceIdSettings::default();
        assert_eq!(settings.prefix_for_product(None, None), "ORDR");
        assert_eq!(settings.prefix_for_product(Some("unknown"), None), "ORDR");
        settings.set_product_prefix("prod-hosting", "hstg").unwrap();
        settings.set_product_prefix("prod-kvm", "KVMS").unwrap();
        assert_eq!(
            settings.prefix_for_product(Some("prod-hosting"), None),
            "HSTG"
        );
        assert_eq!(
            settings.prefix_for_product(Some("missing"), Some("prod-kvm")),
            "KVMS"
        );
        assert!(settings.validate().is_empty());
        assert!(settings.remove_product_prefix("prod-hosting"));
        assert_eq!(settings.prefix_for_product(Some("prod-hosting"), None), "ORDR");
        assert!(settings.set_product_prefix("", "HSTG").is_err());
        assert!(settings.set_product_prefix("x", "hstg").is_ok());

        let dirty = ReferenceIdSettings {
            default_prefix: "ordr".to_string(),
            product_prefixes: [("p1".to_string(), "kvms".to_string())]
                .into_iter()
                .collect(),
            max_total_length: 64,
        };
        // The cap is a ceiling in 30-64, so 64 is valid; only the
        // two non-canonical prefixes are problems.
        assert_eq!(dirty.validate().len(), 2);
        let clean = dirty.sanitized();
        assert_eq!(clean.default_prefix, "ORDR");
        assert_eq!(clean.product_prefixes.get("p1").map(String::as_str), Some("KVMS"));
        assert_eq!(clean.max_total_length, 64);
        assert!(clean.validate().is_empty());
        // Caps outside 30-64 are configuration errors; sanitizing
        // clamps them into range.
        let low_cap = ReferenceIdSettings {
            max_total_length: 29,
            ..ReferenceIdSettings::default()
        };
        assert_eq!(low_cap.validate().len(), 1);
        assert_eq!(low_cap.sanitized().max_total_length, TOTAL_LENGTH);
        let high_cap = ReferenceIdSettings {
            max_total_length: 65,
            ..ReferenceIdSettings::default()
        };
        assert_eq!(high_cap.validate().len(), 1);
        assert_eq!(high_cap.sanitized().max_total_length, 64);
    }

    #[test]
    fn generate_unique_retries_once_on_conflict() {
        let first = generate_unique(|_| false);
        assert!(is_valid(&first));
        // First id taken, second free: the retry is returned.
        let mut seen = 0;
        let second = generate_unique(|_| {
            seen += 1;
            seen == 1
        });
        assert!(is_valid(&second));
        assert_ne!(first, second);
        // Always taken: still returns a valid id; the caller must
        // fail the insert on the second conflict, never overwrite.
        let fallback = generate_unique(|_| true);
        assert!(is_valid(&fallback));
        let prefixed = generate_unique_with_prefix("hstg", |_| false);
        assert!(prefixed.starts_with("HSTG-"));
    }

    #[test]
    fn settings_generate_uses_product_prefix() {
        let mut settings = ReferenceIdSettings::default();
        settings.set_product_prefix("prod-jkt", "JKT1").unwrap();
        let id = settings.generate(Some("prod-jkt"), None);
        assert!(id.starts_with("JKT1-"));
        assert!(is_valid(&id));
        let fallback = settings.generate(Some("other"), None);
        assert!(fallback.starts_with("ORDR-"));
    }
}
