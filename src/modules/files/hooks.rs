//! files module - Hook handlers for file events from other modules.

use unified_core::hooks::HookContext;
use serde_json::{Value, json};
use std::sync::Arc;

use super::helpers::new_id;
use super::helpers::now;
use super::helpers::sanitize_file_name;
use super::types::FileRecord;
use super::types::FilesState;

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

pub(crate) fn on_file_shared(state: Arc<FilesState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    if let Some(file_id) = data.get("file_id").and_then(|v| v.as_str()) {
        if state.records.contains_key(file_id) {
            ctx.add_result(json!({ "file_id": file_id, "already_registered": true }));
            return;
        }
    }
    let raw_name = data
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("shared-file");
    // Sanitize exactly as upload does: a quote or CRLF in a
    // hook-supplied name must never reach Content-Disposition.
    let name = match sanitize_file_name(raw_name) {
        Ok(name) => name,
        Err(_) => {
            ctx.add_result(json!({ "error": "Shared file name is not usable" }));
            return;
        }
    };
    let folder_id = data
        .get("folder_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if !state.folder_exists(&folder_id) {
        ctx.add_result(json!({ "error": "Target folder not found" }));
        return;
    }
    let company_id = data
        .get("company_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let id = data
        .get("file_id")
        .and_then(|v| v.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(new_id);
    let record = FileRecord {
        id,
        name,
        kind: "file".to_string(),
        size: data.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
        mime: data
            .get("mime")
            .and_then(|v| v.as_str())
            .unwrap_or("application/octet-stream")
            .to_string(),
        folder_id,
        path: data
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        company_id,
        created_at: now(),
        updated_at: now(),
    };
    state.records.insert(record.id.clone(), record.clone());
    ctx.add_result(serde_json::to_value(&record).unwrap_or(Value::Null));
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::hooks::HookContext;
    use serde_json::json;

    fn context(data: Value) -> HookContext {
        HookContext::new("file.shared", data)
    }

    #[test]
    fn shared_name_is_sanitized_like_upload() {
        let state = Arc::new(FilesState::new());
        let mut ctx = context(json!({
            "file_id": "shared-1",
            "name": "some/dir/we\"ird.txt",
            "url": "https://example.invalid/weird.txt",
        }));
        on_file_shared(state.clone(), &mut ctx);
        let record = state.records.get("shared-1").unwrap();
        assert_eq!(record.name, "we\"ird.txt");
        assert_eq!(record.path, "https://example.invalid/weird.txt");
    }

    #[test]
    fn shared_name_with_crlf_passes_upload_sanitizer() {
        let state = Arc::new(FilesState::new());
        let mut ctx = context(json!({
            "file_id": "shared-2",
            "name": "evil\r\nX-Injected: yes.txt",
            "url": "https://example.invalid/evil.txt",
        }));
        on_file_shared(state.clone(), &mut ctx);
        // sanitize_file_name keeps this name, exactly as upload does;
        // the header stays safe because download emits RFC 5987 with a
        // sanitised ASCII fallback (see the download header test).
        assert!(state.records.contains_key("shared-2"));
        let record = state.records.get("shared-2").unwrap();
        assert_eq!(record.name, "evil\r\nX-Injected: yes.txt");
    }

    #[test]
    fn shared_unusable_name_is_rejected() {
        let state = Arc::new(FilesState::new());
        let mut ctx = context(json!({
            "file_id": "shared-3",
            "name": "..",
            "url": "https://example.invalid/dot.txt",
        }));
        on_file_shared(state.clone(), &mut ctx);
        assert!(!state.records.contains_key("shared-3"));
        assert!(
            ctx.results
                .iter()
                .any(|result| result.get("error").is_some())
        );
    }

    #[test]
    fn shared_record_carries_the_hook_company_id() {
        let state = Arc::new(FilesState::new());
        let mut ctx = context(json!({
            "file_id": "shared-4",
            "name": "report.pdf",
            "company_id": "company-a",
            "url": "https://example.invalid/report.pdf",
        }));
        on_file_shared(state.clone(), &mut ctx);
        let record = state.records.get("shared-4").unwrap();
        assert_eq!(record.company_id, "company-a");
    }
}
