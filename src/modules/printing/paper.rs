//! printing - paper.

use unified_core::error::{UnifiedError, UnifiedResult};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use super::{events::*, thermal::*, profiles::*, jobs::*, state::*, auth::*, hooks::*, handlers_paper::*, handlers_profiles::*, handlers_jobs::*, module::*};



#[derive(Debug, Clone, Copy)]
// ---------------------------------------------------------------------------
// Paper sizes
// ---------------------------------------------------------------------------
/// One standard paper size, as a compile-time constant. See the
/// module docs for why standards are constants while custom sizes
/// are data.
pub struct StandardPaperSize {
    pub code: &'static str,
    pub name: &'static str,
    pub width_mm: f64,
    pub height_mm: Option<f64>,
    pub is_thermal: bool,
    pub thermal_width_mm: Option<u32>,
}


/// The standard paper sizes. ISO 216 A0-A6, Letter, Legal,
/// Tabloid, and the common thermal roll widths 58mm and 80mm.
/// Thermal rows have no height: a roll is continuous.
pub const STANDARD_PAPER_SIZES: &[StandardPaperSize] = &[
    StandardPaperSize { code: "a0", name: "A0", width_mm: 841.0, height_mm: Some(1189.0), is_thermal: false, thermal_width_mm: None },
    StandardPaperSize { code: "a1", name: "A1", width_mm: 594.0, height_mm: Some(841.0), is_thermal: false, thermal_width_mm: None },
    StandardPaperSize { code: "a2", name: "A2", width_mm: 420.0, height_mm: Some(594.0), is_thermal: false, thermal_width_mm: None },
    StandardPaperSize { code: "a3", name: "A3", width_mm: 297.0, height_mm: Some(420.0), is_thermal: false, thermal_width_mm: None },
    StandardPaperSize { code: "a4", name: "A4", width_mm: 210.0, height_mm: Some(297.0), is_thermal: false, thermal_width_mm: None },
    StandardPaperSize { code: "a5", name: "A5", width_mm: 148.0, height_mm: Some(210.0), is_thermal: false, thermal_width_mm: None },
    StandardPaperSize { code: "a6", name: "A6", width_mm: 105.0, height_mm: Some(148.0), is_thermal: false, thermal_width_mm: None },
    StandardPaperSize { code: "letter", name: "Letter", width_mm: 215.9, height_mm: Some(279.4), is_thermal: false, thermal_width_mm: None },
    StandardPaperSize { code: "legal", name: "Legal", width_mm: 215.9, height_mm: Some(355.6), is_thermal: false, thermal_width_mm: None },
    StandardPaperSize { code: "tabloid", name: "Tabloid", width_mm: 279.4, height_mm: Some(431.8), is_thermal: false, thermal_width_mm: None },
    StandardPaperSize { code: "thermal-58", name: "Thermal 58mm", width_mm: 58.0, height_mm: None, is_thermal: true, thermal_width_mm: Some(58) },
    StandardPaperSize { code: "thermal-80", name: "Thermal 80mm", width_mm: 80.0, height_mm: None, is_thermal: true, thermal_width_mm: Some(80) },
];


/// True when `code` names a standard paper size.
pub fn is_standard_paper_size(code: &str) -> bool {
    STANDARD_PAPER_SIZES
        .iter()
        .any(|size| size.code == code)
}


/// The standard size as a `PaperSize` record, with the seeded
/// margins (10mm for sheet sizes, 2mm for thermal rolls).
pub fn standard_paper_size(code: &str) -> Option<PaperSize> {
    let standard = STANDARD_PAPER_SIZES
        .iter()
        .find(|size| size.code == code)?;
    let margin = if standard.is_thermal { 2.0 } else { 10.0 };
    let now = Utc::now();
    Some(PaperSize {
        code: standard.code.to_string(),
        name: standard.name.to_string(),
        width_mm: standard.width_mm,
        height_mm: standard.height_mm,
        margin_top_mm: margin,
        margin_bottom_mm: margin,
        margin_left_mm: margin,
        margin_right_mm: margin,
        is_standard: true,
        is_thermal: standard.is_thermal,
        thermal_width_mm: standard.thermal_width_mm,
        company_id: None,
        // Standard rows are global: no owning tenant.
        tenant_id: None,
        created_by: None,
        created_at: now,
        updated_at: now,
    })
}


/// One paper size: a standard row or an admin-created custom row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperSize {
    pub code: String,
    pub name: String,
    pub width_mm: f64,
    pub height_mm: Option<f64>,
    pub margin_top_mm: f64,
    pub margin_bottom_mm: f64,
    pub margin_left_mm: f64,
    pub margin_right_mm: f64,
    pub is_standard: bool,
    pub is_thermal: bool,
    pub thermal_width_mm: Option<u32>,
    #[serde(default)]
    pub company_id: Option<String>,
    /// Owning tenant id. `None` for the global standard rows
    /// (visible to every tenant) and for records created before
    /// tenant stamping; custom sizes always carry a tenant.
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}


/// Input for creating a custom paper size. Standards cannot be
/// created through the API: their codes are rejected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePaperSize {
    pub code: String,
    pub name: String,
    pub width_mm: f64,
    #[serde(default)]
    pub height_mm: Option<f64>,
    #[serde(default)]
    pub margin_top_mm: Option<f64>,
    #[serde(default)]
    pub margin_bottom_mm: Option<f64>,
    #[serde(default)]
    pub margin_left_mm: Option<f64>,
    #[serde(default)]
    pub margin_right_mm: Option<f64>,
    #[serde(default)]
    pub is_thermal: bool,
    #[serde(default)]
    pub thermal_width_mm: Option<u32>,
    #[serde(default)]
    pub company_id: Option<String>,
}


/// Input for updating a custom paper size. `height_mm` uses the
/// nested option so JSON `null` clears the height (thermal roll)
/// while an absent key leaves it alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdatePaperSize {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub width_mm: Option<f64>,
    #[serde(default)]
    pub height_mm: Option<Option<f64>>,
    #[serde(default)]
    pub margin_top_mm: Option<f64>,
    #[serde(default)]
    pub margin_bottom_mm: Option<f64>,
    #[serde(default)]
    pub margin_left_mm: Option<f64>,
    #[serde(default)]
    pub margin_right_mm: Option<f64>,
    #[serde(default)]
    pub is_thermal: Option<bool>,
    #[serde(default)]
    pub thermal_width_mm: Option<Option<u32>>,
    #[serde(default)]
    pub company_id: Option<Option<String>>,
}


/// A paper-size code is a lowercase slug.
pub(crate) fn valid_paper_size_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 64
        && code
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}


/// Validate the dimensions of a (custom) paper size. Width is
/// 20mm to 1500mm; sheet sizes need a height in the same range;
/// thermal sizes must have no height and must name their roll
/// width; margins must fit inside the sheet.
pub(crate) fn validate_paper_size_dimensions(
    width_mm: f64,
    height_mm: Option<f64>,
    margin_top_mm: f64,
    margin_bottom_mm: f64,
    margin_left_mm: f64,
    margin_right_mm: f64,
    is_thermal: bool,
    thermal_width_mm: Option<u32>,
) -> UnifiedResult<()> {
    if !width_mm.is_finite() || !(20.0..=1500.0).contains(&width_mm) {
        return Err(UnifiedError::bad_request(
            "Paper width must be between 20mm and 1500mm",
        ));
    }
    if is_thermal {
        if height_mm.is_some() {
            return Err(UnifiedError::bad_request(
                "A thermal (roll) paper size has no height",
            ));
        }
        if thermal_width_mm.is_none() {
            return Err(UnifiedError::bad_request(
                "A thermal paper size needs its roll width in millimetres",
            ));
        }
        if thermal_width_mm != Some(width_mm as u32) {
            return Err(UnifiedError::bad_request(
                "A thermal paper size's roll width must equal its width",
            ));
        }
    } else {
        if thermal_width_mm.is_some() {
            return Err(UnifiedError::bad_request(
                "Only a thermal paper size carries a roll width",
            ));
        }
        match height_mm {
            Some(height) if height.is_finite() && (20.0..=1500.0).contains(&height) => {}
            _ => {
                return Err(UnifiedError::bad_request(
                    "A sheet paper size needs a height between 20mm and 1500mm",
                ));
            }
        }
    }
    for (label, margin) in [
        ("top margin", margin_top_mm),
        ("bottom margin", margin_bottom_mm),
        ("left margin", margin_left_mm),
        ("right margin", margin_right_mm),
    ] {
        if !margin.is_finite() || !(0.0..=100.0).contains(&margin) {
            return Err(UnifiedError::bad_request(format!(
                "Paper {label} must be between 0mm and 100mm"
            )));
        }
    }
    if margin_left_mm + margin_right_mm >= width_mm {
        return Err(UnifiedError::bad_request(
            "Left and right margins must fit inside the paper width",
        ));
    }
    if let Some(height) = height_mm {
        if margin_top_mm + margin_bottom_mm >= height {
            return Err(UnifiedError::bad_request(
                "Top and bottom margins must fit inside the paper height",
            ));
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_standard_paper_size_knows_seeded_codes() {
        assert!(is_standard_paper_size("a4"));
        assert!(is_standard_paper_size("letter"));
        assert!(is_standard_paper_size("thermal-80"));
        assert!(!is_standard_paper_size("a7"));
        assert!(!is_standard_paper_size("A4"));
        assert!(!is_standard_paper_size(""));
    }

    #[test]
    fn standard_paper_size_returns_sheet_margins() {
        let size = standard_paper_size("a4").unwrap();
        assert_eq!(size.width_mm, 210.0);
        assert_eq!(size.height_mm, Some(297.0));
        assert_eq!(size.margin_top_mm, 10.0);
        assert_eq!(size.margin_left_mm, 10.0);
        assert!(size.is_standard);
        assert!(!size.is_thermal);
    }

    #[test]
    fn standard_paper_size_returns_thermal_margins() {
        let size = standard_paper_size("thermal-58").unwrap();
        assert!(size.is_thermal);
        assert_eq!(size.height_mm, None);
        assert_eq!(size.margin_top_mm, 2.0);
        assert_eq!(size.thermal_width_mm, Some(58));
        assert!(standard_paper_size("nope").is_none());
    }

    #[test]
    fn valid_paper_size_code_is_a_lowercase_slug() {
        assert!(valid_paper_size_code("a4"));
        assert!(valid_paper_size_code("custom-size_2"));
        assert!(!valid_paper_size_code(""));
        assert!(!valid_paper_size_code("A4"));
        assert!(!valid_paper_size_code("has space"));
        assert!(!valid_paper_size_code(&"x".repeat(65)));
    }

    #[test]
    fn validate_dimensions_accepts_sheet_and_thermal() {
        assert!(validate_paper_size_dimensions(210.0, Some(297.0), 10.0, 10.0, 10.0, 10.0, false, None).is_ok());
        assert!(validate_paper_size_dimensions(80.0, None, 0.0, 0.0, 0.0, 0.0, true, Some(80)).is_ok());
    }

    #[test]
    fn validate_dimensions_rejects_bad_width_and_height() {
        assert!(validate_paper_size_dimensions(10.0, Some(297.0), 10.0, 10.0, 10.0, 10.0, false, None).is_err());
        assert!(validate_paper_size_dimensions(210.0, None, 10.0, 10.0, 10.0, 10.0, false, None).is_err());
        assert!(validate_paper_size_dimensions(210.0, Some(2000.0), 10.0, 10.0, 10.0, 10.0, false, None).is_err());
    }

    #[test]
    fn validate_dimensions_enforces_thermal_rules_and_margins() {
        assert!(validate_paper_size_dimensions(80.0, Some(100.0), 0.0, 0.0, 0.0, 0.0, true, Some(80)).is_err());
        assert!(validate_paper_size_dimensions(80.0, None, 0.0, 0.0, 0.0, 0.0, true, None).is_err());
        assert!(validate_paper_size_dimensions(210.0, Some(297.0), 10.0, 10.0, 10.0, 10.0, false, Some(80)).is_err());
        assert!(validate_paper_size_dimensions(210.0, Some(297.0), 101.0, 10.0, 10.0, 10.0, false, None).is_err());
        assert!(validate_paper_size_dimensions(20.0, Some(20.0), 10.0, 10.0, 15.0, 15.0, false, None).is_err());
    }

    #[test]
    fn validate_dimensions_rejects_thermal_width_mismatch() {
        // A 58mm roll claiming an 80mm thermal width used to pass.
        assert!(validate_paper_size_dimensions(58.0, None, 0.0, 0.0, 0.0, 0.0, true, Some(80)).is_err());
        assert!(validate_paper_size_dimensions(80.0, None, 0.0, 0.0, 0.0, 0.0, true, Some(58)).is_err());
        assert!(validate_paper_size_dimensions(58.0, None, 0.0, 0.0, 0.0, 0.0, true, Some(58)).is_ok());
    }
}
