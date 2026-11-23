// Site Builder — Input validation and pure value checks.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::sync::OnceLock;
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::rendering::*;
use super::handlers::*;
use super::module::*;

pub(crate) fn validate_slug(slug: &str) -> UnifiedResult<()> {
    if !slug_regex().is_match(slug) {
        return Err(UnifiedError::bad_request(
            "Slug must be lowercase letters, digits, and single hyphens (for example 'about-us')",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_slug_accepts_lowercase_hyphenated_slugs() {
        assert!(validate_slug("about-us").is_ok());
        assert!(validate_slug("a").is_ok());
        assert!(validate_slug("About").is_err());
        assert!(validate_slug("a--b").is_err());
        assert!(validate_slug("").is_err());
        assert!(validate_slug("a b").is_err());
        assert!(validate_slug("-lead").is_err());
    }
}
