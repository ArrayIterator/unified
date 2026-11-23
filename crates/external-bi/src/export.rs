//! BI external module (split from lib.rs; moved verbatim).

use super::*;


// ---------------------------------------------------------------------------
// CSV output (proper quoting, no extra dependency)
// ---------------------------------------------------------------------------

/// Quote one CSV cell: quote when it contains a comma, quote,
/// newline, or leading/trailing space; double any inner quotes.
pub fn csv_escape(value: &str) -> String {
    let needs_quotes = value.contains(',')
        || value.contains('"')
        || value.contains('\n')
        || value.contains('\r')
        || value.starts_with(' ')
        || value.ends_with(' ');
    if needs_quotes {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn cell_to_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        _ => serde_json::to_string(value).unwrap_or_default(),
    }
}

/// Render snapshot rows as CSV: header from the dataset fields, one
/// row per record, in declared field order.
pub fn rows_to_csv(fields: &[FieldDef], rows: &[Value]) -> String {
    let mut out = String::new();
    out.push_str(
        &fields
            .iter()
            .map(|field| csv_escape(&field.name))
            .collect::<Vec<_>>()
            .join(","),
    );
    out.push('\n');
    for row in rows {
        let cells: Vec<String> = fields
            .iter()
            .map(|field| {
                let value = row.get(&field.name).unwrap_or(&Value::Null);
                csv_escape(&cell_to_string(value))
            })
            .collect();
        out.push_str(&cells.join(","));
        out.push('\n');
    }
    out
}

/// Render rows as JSONL: one compact JSON object per line.
pub fn rows_to_jsonl(rows: &[Value]) -> String {
    let mut out = String::new();
    for row in rows {
        out.push_str(&serde_json::to_string(row).unwrap_or_default());
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// SSRF-safe URL validation
// ---------------------------------------------------------------------------

/// True when the IP is public: not loopback, private, link-local,
/// multicast, unspecified, reserved, documentation, or CGNAT space.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_unspecified()
                || v4.is_broadcast()
            {
                return false;
            }
            let octets = v4.octets();
            // Benchmarking 198.18.0.0/15 and reserved 240.0.0.0/4,
            // checked by octets (no std helper is used for these).
            // CGNAT 100.64.0.0/10, documentation 192.0.2.0/24,
            // 198.51.100.0/24, 203.0.113.0/24, and 192.0.0.0/24.
            if octets[0] == 198 && (octets[1] == 18 || octets[1] == 19) {
                return false;
            }
            if octets[0] >= 240 {
                return false;
            }
            if octets[0] == 100 && (64..=127).contains(&octets[1]) {
                return false;
            }
            if octets[0] == 192 && octets[1] == 0 && (octets[2] == 0 || octets[2] == 2) {
                return false;
            }
            if octets[0] == 198 && octets[1] == 51 && octets[2] == 100 {
                return false;
            }
            if octets[0] == 203 && octets[1] == 0 && octets[2] == 113 {
                return false;
            }
            true
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
            {
                return false;
            }
            // Documentation 2001:db8::/32.
            let segments = v6.segments();
            if segments[0] == 0x2001 && segments[1] == 0x0db8 {
                return false;
            }
            true
        }
    }
}

/// Validate a webhook URL: http/https only, no embedded credentials,
/// host must resolve (or be a literal) to public IPs only. DNS is
/// resolved here and the request is sent with redirects disabled, so
/// a redirect can never smuggle the request to a private host.
pub async fn validate_webhook_url(raw: &str) -> Result<url::Url, UnifiedError> {
    let parsed = url::Url::parse(raw)
        .map_err(|_| bad_request(format!("invalid webhook URL {raw:?}")))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(bad_request("webhook URL must use http or https"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(bad_request(
            "webhook URL must not contain embedded credentials",
        ));
    }
    let Some(host) = parsed.host_str() else {
        return Err(bad_request("webhook URL must have a host"));
    };
    let port = parsed.port_or_known_default().unwrap_or(443);
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !is_public_ip(ip) {
            return Err(bad_request(
                "webhook URL must not point at a private or reserved IP address",
            ));
        }
        return Ok(parsed);
    }
    let resolved = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| bad_request(format!("webhook host {host:?} does not resolve")))?;
    let mut saw_any = false;
    for address in resolved {
        saw_any = true;
        if !is_public_ip(address.ip()) {
            return Err(bad_request(
                "webhook host resolves to a private or reserved IP address",
            ));
        }
    }
    if !saw_any {
        return Err(bad_request(format!("webhook host {host:?} does not resolve")));
    }
    Ok(parsed)
}

// ---------------------------------------------------------------------------
// File-destination path safety
// ---------------------------------------------------------------------------

/// Base directory for `file` destinations.
pub fn export_base_dir() -> PathBuf {
    match std::env::var("BI_EXPORT_BASE_DIR") {
        Ok(value) if !value.trim().is_empty() => PathBuf::from(value),
        _ => PathBuf::from(DEFAULT_EXPORT_BASE_DIR),
    }
}

/// Resolve a destination's relative `path` below the export base
/// directory. Rejects absolute paths and any `..` traversal. The
/// result is guaranteed to stay under the base directory.
pub fn resolve_export_path(relative: &str) -> Result<PathBuf, UnifiedError> {
    let candidate = Path::new(relative);
    if candidate.is_absolute() {
        return Err(bad_request("file destination path must be relative"));
    }
    for component in candidate.components() {
        match component {
            Component::ParentDir => {
                return Err(bad_request(
                    "file destination path must not contain '..'",
                ));
            }
            Component::Normal(_) | Component::CurDir => {}
            _ => {
                return Err(bad_request("invalid file destination path"));
            }
        }
    }
    if relative.trim().is_empty() {
        return Err(bad_request("file destination path must not be empty"));
    }
    let base = export_base_dir();
    let joined = base.join(candidate);
    // Normalise without touching the filesystem: `..` was already
    // rejected, so a lexical prefix check is exact here.
    let base_normalised = if base.is_absolute() {
        base.clone()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(&base)
    };
    let joined_normalised = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(&joined);
    if !joined_normalised.starts_with(&base_normalised) {
        return Err(bad_request(
            "file destination path escapes the export base directory",
        ));
    }
    Ok(joined)
}
