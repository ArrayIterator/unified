// logs - tracing.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{http::header, web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, OnceLock, RwLock,
};
use uuid::Uuid;
use super::*;
use super::redaction::*;
use super::types::*;
use super::store::*;
use super::service::*;
use super::common::*;
use super::handlers::*;
use super::hooks::*;
use super::module::*;

// Tracing subscriber bridge

/// Collects the fields of one tracing event as debug strings.
pub(crate) struct FieldVisitor {
    pub(crate) fields: Vec<(String, String)>,
}

impl ::tracing::field::Visit for FieldVisitor {
    fn record_debug(&mut self, field: &::tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.fields
            .push((field.name().to_string(), format!("{value:?}")));
    }
}

/// A `tracing_subscriber::Layer` that forwards every tracing event into
/// the shared log buffer, so application logs written with `tracing`
/// or `log` appear in the log viewer. It never logs anything itself,
/// so it cannot recurse.
///
/// Integrator wiring (in the logging setup, next to the `fmt` layer):
///
/// ```rust
/// use tracing_subscriber::{prelude::*, Registry};
/// let subscriber = Registry::default()
///     .with(tracing_subscriber::fmt::layer().with_env_filter(filter))
///     .with(crate::modules::logs::LogsLayer);
/// let _ = tracing::subscriber::set_global_default(subscriber);
/// ```
pub struct LogsLayer;

impl<S> tracing_subscriber::Layer<S> for LogsLayer
where
    S: ::tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &::tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let metadata = event.metadata();
        let level = metadata.level().as_str().to_lowercase();
        let source = metadata.target().to_string();
        let mut visitor = FieldVisitor { fields: Vec::new() };
        event.record(&mut visitor);
        let mut message = String::new();
        let mut context = serde_json::Map::new();
        for (name, value) in visitor.fields {
            if name == "message" {
                message = value.trim_matches('"').to_string();
            } else {
                context.insert(name, Value::String(value));
            }
        }
        if message.is_empty() {
            message = format!("Event at {}", metadata.target());
        }
        push_log_full(
            &level,
            &source,
            &message,
            Value::Object(context),
            None,
            None,
        );
    }
}
