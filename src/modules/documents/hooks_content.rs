// documents - hook listeners for content publish state.
//
// When a post/page is published, its attached media flips public
// (so anonymous readers can load it); when it returns to draft,
// the media flips back private. The flips run as detached tasks:
// the hook bus is synchronous and the flip is async. Best-effort
// by design - the explicit POST /{id}/flip endpoint is the
// reliable, user-visible path, and the media picker shows the
// live visibility. A flip is skipped when the attachment's scan
// is not clean (the scan gate) or when the payload carries no
// usable tenant scope.

use super::*;
use unified_core::hooks::{HookBus, HookContext};
use std::sync::Arc;
use uuid::Uuid;

/// Register the content publish-state listeners.
pub(crate) fn register_content_hooks(state: Arc<DocumentsState>, bus: &HookBus) {
    let publish_state = state.clone();
    bus.register(
        "content.published",
        "documents",
        Arc::new(move |ctx: &mut HookContext| {
            on_content_published(publish_state.clone(), ctx);
        }),
    );
    let unpublish_state = state.clone();
    bus.register(
        "content.unpublished",
        "documents",
        Arc::new(move |ctx: &mut HookContext| {
            on_content_unpublished(unpublish_state.clone(), ctx);
        }),
    );
}

/// `(tenant_id, [document_id, ...])` from a content publish-state
/// payload: `{"tenant_id", "attachments": [{"document_id",
/// "url", "description"}, ...], ...}`.
fn publish_payload(ctx: &HookContext) -> (Option<Uuid>, Vec<String>) {
    let tenant_id = ctx
        .data
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .and_then(|value| Uuid::parse_str(value).ok());
    let attachments = ctx
        .data
        .get("attachments")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.get("document_id")
                        .and_then(|value| value.as_str())
                        .map(|value| value.to_string())
                })
                .collect()
        })
        .unwrap_or_default();
    (tenant_id, attachments)
}

/// Flip one attachment as a detached task (the hook bus is
/// synchronous; the flip needs the async provider). Best-effort:
/// a failure leaves the document at its current visibility.
fn spawn_flip(
    state: Arc<DocumentsState>,
    scope: StorageScope,
    document_id: String,
    visibility: &str,
) {
    let visibility = visibility.to_string();
    let task = async move {
        let _ = state
            .flip_visibility(scope, &document_id, visibility, "content-publish")
            .await;
    };
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(task);
    } else {
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(task);
        });
    }
}

fn on_content_published(state: Arc<DocumentsState>, ctx: &mut HookContext) {
    let (tenant_id, attachments) = publish_payload(ctx);
    let Some(tenant_id) = tenant_id else {
        return;
    };
    let scope = StorageScope::Tenant(tenant_id);
    for document_id in attachments {
        spawn_flip(state.clone(), scope, document_id, "public");
    }
}

fn on_content_unpublished(state: Arc<DocumentsState>, ctx: &mut HookContext) {
    let (tenant_id, attachments) = publish_payload(ctx);
    let Some(tenant_id) = tenant_id else {
        return;
    };
    let scope = StorageScope::Tenant(tenant_id);
    for document_id in attachments {
        spawn_flip(state.clone(), scope, document_id, "private");
    }
}
