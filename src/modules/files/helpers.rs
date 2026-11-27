//! files module - Small pure helpers: ids, paging, search, names, MIME types.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::path::Path;
use uuid::Uuid;

use super::types::MAX_FOLDER_NAME_LEN;

// Helpers

/// Fail-closed tenant gate: no resolved tenant context -> error, never
/// fall back to `public`. Records in this module carry no `tenant_id`
/// of their own, so the guard is a gate only; real partitioning is a
/// C11 follow-up.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}

pub(crate) fn paginate<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

pub(crate) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

pub(crate) fn respond_created<T: Serialize>(result: UnifiedResult<T>, debug: bool) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

/// Validate a folder name: non-empty, no path separators, at most
/// `MAX_FOLDER_NAME_LEN` characters (matching the apikeys name limit).
pub(crate) fn validate_folder_name(raw: &str) -> UnifiedResult<String> {
    let name = raw.trim().to_string();
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return Err(UnifiedError::bad_request(
            "Folder name must not be empty and must not contain path separators",
        ));
    }
    if name.chars().count() > MAX_FOLDER_NAME_LEN {
        return Err(UnifiedError::bad_request(format!(
            "Folder name must be at most {MAX_FOLDER_NAME_LEN} characters"
        )));
    }
    Ok(name)
}

/// Build a safe `Content-Disposition` value for a download. The
/// stored name is never interpolated raw: the plain `filename` is an
/// ASCII-only sanitised fallback (quotes, backslashes and control
/// characters become `_`), and the real name travels percent-encoded
/// as RFC 5987 `filename*`, so a quote or CRLF in a record name can
/// never inject a header.
pub(crate) fn content_disposition(name: &str) -> String {
    let fallback = ascii_fallback_name(name);
    let encoded = rfc5987_encode(name);
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

/// ASCII-only fallback for `filename`: printable ASCII except `"`
/// and `\` is kept, everything else becomes `_`. Never empty.
fn ascii_fallback_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        out.push_str("download");
    }
    out
}

/// RFC 5987 `attr-char` percent-encoding of a UTF-8 name.
fn rfc5987_encode(name: &str) -> String {
    const ATTR_CHARS: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!#$&+-.^_`|~";
    let mut out = String::new();
    for byte in name.bytes() {
        if ATTR_CHARS.contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Strip any directory components from an uploaded name. Returns an error
/// when nothing usable remains (for example `..` or `/`).
pub(crate) fn sanitize_file_name(raw: &str) -> UnifiedResult<String> {
    let name = Path::new(raw)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if name.is_empty() || name == "." || name == ".." {
        return Err(UnifiedError::bad_request(
            "Uploaded file name is not usable",
        ));
    }
    Ok(name)
}

pub(crate) fn mime_for(name: &str, bytes: &[u8]) -> String {
    if let Some(kind) = infer::get(bytes) {
        return kind.mime_type().to_string();
    }
    match Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "txt" => "text/plain".to_string(),
        "html" | "htm" => "text/html".to_string(),
        "css" => "text/css".to_string(),
        "js" | "mjs" => "text/javascript".to_string(),
        "json" => "application/json".to_string(),
        "pdf" => "application/pdf".to_string(),
        "zip" => "application/zip".to_string(),
        "md" => "text/markdown".to_string(),
        "csv" => "text/csv".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::content_disposition;
    use super::matches_search;
    use super::mime_for;
    use super::require_tenant;
    use super::sanitize_file_name;
    use super::validate_folder_name;
    use actix_web::test::TestRequest;
    use unified_core::pagination::PaginationParams;
    use std::collections::HashMap;

    fn params_with_search(search: Option<&str>) -> PaginationParams {
        PaginationParams {
            page: None,
            per_page: None,
            sort: None,
            order: None,
            search: search.map(str::to_string),
            filters: HashMap::new(),
        }
    }

    #[test]
    fn sanitize_strips_directories() {
        assert_eq!(
            sanitize_file_name("some/dir/report.pdf").unwrap(),
            "report.pdf"
        );
        assert_eq!(sanitize_file_name("plain.txt").unwrap(), "plain.txt");
    }

    #[test]
    fn sanitize_rejects_empty_and_dot_names() {
        assert!(sanitize_file_name("..").is_err());
        assert!(sanitize_file_name("/").is_err());
        assert!(sanitize_file_name("").is_err());
    }

    #[test]
    fn sanitize_trims_surrounding_whitespace() {
        assert_eq!(sanitize_file_name("  spaced.txt  ").unwrap(), "spaced.txt");
    }

    #[test]
    fn mime_falls_back_to_extension() {
        assert_eq!(mime_for("notes.txt", &[]), "text/plain");
        assert_eq!(mime_for("page.html", &[]), "text/html");
        assert_eq!(mime_for("data.json", &[]), "application/json");
        assert_eq!(
            mime_for("archive.unknown-ext", &[]),
            "application/octet-stream"
        );
    }

    #[test]
    fn mime_prefers_detected_content() {
        let png: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];
        assert_eq!(mime_for("photo.bin", &png), "image/png");
    }

    #[test]
    fn search_matches_case_insensitively() {
        let params = params_with_search(Some("HELLO"));
        assert!(matches_search("say hello there", &params));
        assert!(!matches_search("nothing here", &params));
    }

    #[test]
    fn search_without_needle_matches_everything() {
        assert!(matches_search("anything", &params_with_search(None)));
        assert!(matches_search("anything", &params_with_search(Some("   "))));
    }

    #[test]
    fn folder_name_rejects_empty_and_separators() {
        assert!(validate_folder_name("").is_err());
        assert!(validate_folder_name("   ").is_err());
        assert!(validate_folder_name("a/b").is_err());
        assert!(validate_folder_name("a\\b").is_err());
    }

    #[test]
    fn folder_name_caps_length_at_120_chars() {
        assert_eq!(validate_folder_name("  Docs  ").unwrap(), "Docs");
        assert!(validate_folder_name(&"x".repeat(120)).is_ok());
        assert!(validate_folder_name(&"x".repeat(121)).is_err());
    }

    #[test]
    fn content_disposition_keeps_plain_ascii_names() {
        assert_eq!(
            content_disposition("report.pdf"),
            "attachment; filename=\"report.pdf\"; filename*=UTF-8''report.pdf"
        );
    }

    #[test]
    fn content_disposition_neutralises_quotes_and_crlf() {
        let header = content_disposition("we\"ird\r\nX: y.txt");
        assert!(!header.contains('\r'));
        assert!(!header.contains('\n'));
        assert_eq!(
            header,
            "attachment; filename=\"we_ird__X:_y.txt\"; \
             filename*=UTF-8''we%22ird%0D%0AX%3A%20y.txt"
        );
    }

    #[test]
    fn content_disposition_encodes_non_ascii_names() {
        let header = content_disposition("laporan keuangan.pdf");
        assert_eq!(
            header,
            "attachment; filename=\"laporan_keuangan.pdf\"; \
             filename*=UTF-8''laporan%20keuangan.pdf"
        );
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }
}
