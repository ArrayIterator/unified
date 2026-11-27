//! printing - profiles.

use unified_core::error::{UnifiedError, UnifiedResult};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use super::{events::*, paper::*, thermal::*, jobs::*, state::*, auth::*, hooks::*, handlers_paper::*, handlers_profiles::*, handlers_jobs::*, module::*};



#[derive(Debug, Clone, Serialize, Deserialize)]
// Printer profiles
/// One thermal printer profile: how to reach the printer and what
/// it can do. `connection_target` is an address, never a secret:
/// `192.168.1.50:9100` for a network printer, `/dev/usb/lp0` for a
/// USB printer, a device path for serial, a file path for a spool
/// file, or a connector id for a cloud printer. When the printer
/// needs credentials, `auth_token_ref` holds a reference to the
/// credential in the vault or secrets store - never the credential
/// itself.
pub struct PrinterProfile {
    pub id: String,
    #[serde(default)]
    pub company_id: Option<String>,
    /// Owning tenant id. `None` only for profiles created before
    /// tenant stamping; profiles created over HTTP always carry
    /// the caller's tenant.
    #[serde(default)]
    pub tenant_id: Option<String>,
    pub name: String,
    pub connection_type: String,
    #[serde(default)]
    pub connection_target: Option<String>,
    #[serde(default)]
    pub auth_token_ref: Option<String>,
    pub width_columns: u32,
    pub character_set: String,
    pub code_page: String,
    pub supports_drawer_kick: bool,
    pub supports_cut: bool,
    #[serde(default)]
    pub paper_size_code: Option<String>,
    pub is_default: bool,
    #[serde(default)]
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}


/// Input for creating a printer profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePrinterProfile {
    #[serde(default)]
    pub company_id: Option<String>,
    pub name: String,
    #[serde(default = "default_connection_type")]
    pub connection_type: String,
    #[serde(default)]
    pub connection_target: Option<String>,
    #[serde(default)]
    pub auth_token_ref: Option<String>,
    #[serde(default = "default_profile_width")]
    pub width_columns: u32,
    #[serde(default = "default_code_page")]
    pub character_set: String,
    #[serde(default = "default_code_page")]
    pub code_page: String,
    #[serde(default)]
    pub supports_drawer_kick: bool,
    #[serde(default = "default_true")]
    pub supports_cut: bool,
    #[serde(default)]
    pub paper_size_code: Option<String>,
}


pub(crate) fn default_connection_type() -> String {
    "network".to_string()
}


pub(crate) fn default_profile_width() -> u32 {
    42
}


pub(crate) fn default_code_page() -> String {
    "cp437".to_string()
}


pub(crate) fn default_true() -> bool {
    true
}


/// Input for updating a printer profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdatePrinterProfile {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub connection_type: Option<String>,
    #[serde(default)]
    pub connection_target: Option<Option<String>>,
    #[serde(default)]
    pub auth_token_ref: Option<Option<String>>,
    #[serde(default)]
    pub width_columns: Option<u32>,
    #[serde(default)]
    pub character_set: Option<String>,
    #[serde(default)]
    pub code_page: Option<String>,
    #[serde(default)]
    pub supports_drawer_kick: Option<bool>,
    #[serde(default)]
    pub supports_cut: Option<bool>,
    #[serde(default)]
    pub paper_size_code: Option<Option<String>>,
}


/// Validate a printer profile's connection and character settings.
/// `auth_token_ref` is checked only for shape (a short reference
/// with no whitespace): the vault owns the secret itself.
pub(crate) fn validate_printer_profile(
    connection_type: &str,
    connection_target: Option<&str>,
    auth_token_ref: Option<&str>,
    width_columns: u32,
    character_set: &str,
    code_page: &str,
    paper_size_code: Option<&str>,
) -> UnifiedResult<()> {
    if !PRINTER_CONNECTION_TYPES.contains(&connection_type) {
        return Err(UnifiedError::bad_request(
            "Printer connection type must be network, usb, serial, file, or cloud",
        ));
    }
    match connection_target {
        Some(target) if !target.trim().is_empty() && target.len() <= 512 => {}
        _ => {
            return Err(UnifiedError::bad_request(
                "A printer profile needs a connection target (address, device path, or connector id)",
            ));
        }
    }
    if let Some(reference) = auth_token_ref {
        if reference.trim().is_empty()
            || reference.len() > 128
            || reference.chars().any(char::is_whitespace)
        {
            return Err(UnifiedError::bad_request(
                "auth_token_ref must be a short vault reference without whitespace, never a raw secret",
            ));
        }
    }
    if !(16..=120).contains(&width_columns) {
        return Err(UnifiedError::bad_request(
            "Printer width must be between 16 and 120 columns",
        ));
    }
    if !SUPPORTED_CODE_PAGES.contains(&character_set) {
        return Err(UnifiedError::bad_request(
            "Printer character set is not one of the supported code pages",
        ));
    }
    if !SUPPORTED_CODE_PAGES.contains(&code_page) {
        return Err(UnifiedError::bad_request(
            "Printer code page is not one of the supported code pages",
        ));
    }
    if let Some(code) = paper_size_code {
        if code.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Printer paper size code must not be empty",
            ));
        }
    }
    Ok(())
}
