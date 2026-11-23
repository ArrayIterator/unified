//! Error helpers for the scripting runtime.
//!
//! JavaScript errors (thrown values, syntax errors, interrupts,
//! out-of-memory) are converted to `UnifiedError` with the JS
//! message attached, so a broken module reports like any other
//! server error instead of leaking engine internals.

use rquickjs::{Ctx, Error as QjsError};
use unified_core::error::UnifiedError;

/// Format a QuickJS error for a human. When the engine holds a
/// thrown JS value, include its message (via `ctx.catch()`); a bare
/// engine error (allocation failure, interrupt) formats on its own.
pub fn format_qjs_error(ctx: &Ctx<'_>, error: &QjsError) -> String {
    match error {
        QjsError::Exception => {
            let thrown = ctx.catch();
            match ctx.json_stringify(thrown) {
                Ok(Some(text)) => match text.to_string() {
                    Ok(rendered) => rendered,
                    Err(_) => "JavaScript exception (unprintable)".to_string(),
                },
                _ => "JavaScript exception".to_string(),
            }
        }
        other => other.to_string(),
    }
}

/// Convert a QuickJS error raised while running module `module_id`
/// into a server error. A JS throw is a 400 (the module is at
/// fault); an engine failure (OOM, interrupt) is a 500.
pub fn qjs_error(ctx: &Ctx<'_>, module_id: &str, error: QjsError) -> UnifiedError {
    let message = format_qjs_error(ctx, &error);
    match error {
        QjsError::Exception => {
            UnifiedError::bad_request(format!("Script module \"{module_id}\" threw: {message}"))
        }
        _ => UnifiedError::internal(format!("Script module \"{module_id}\" failed: {message}")),
    }
}

/// Convert a `unified-ts` transpile failure into a server error.
pub fn transpile_error(error: unified_ts::TranspileError) -> UnifiedError {
    UnifiedError::bad_request(error.to_string())
}
