// Unified core library.
//!
//! The core engine every module is built on: configuration, errors, the
//! JSON API envelope, pagination, the hook/event bus, the module registry,
//! the shared application context, the Diesel database layer, and TLS
//! certificate storage. External modules depend on this crate only; see
//! `docs/WORKSPACE_AND_EXTERNAL_MODULES.md` for the API contract.
//!
//! Canonical import paths are `unified_core::<module>::<Item>`; the five
//! root re-exports below are an ergonomic convenience only.

pub mod auth;
pub mod cache;
pub mod caller;
pub mod config;
pub mod context;
pub mod db;
pub mod error;
pub mod hardening;
pub mod hooks;
pub mod json;
pub mod middleware;
pub mod modules;
pub mod money;
pub mod net;
pub mod pagination;
pub mod provider_registry;
pub mod reference_data;
pub mod reference_id;
pub mod routes;
pub mod sharing;
pub mod storage;
pub mod tenancy;
pub mod tls;

pub use auth::{Caller, RouteAuth, caller_from};
pub use config::AppConfig;
pub use context::AppContext;
pub use error::{UnifiedError, UnifiedResult};
pub use hooks::HookBus;
pub use modules::{Module, ModuleRegistry};
