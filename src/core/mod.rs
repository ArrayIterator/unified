// Core engine of the Unified Pentagonal Project.
//
// The core provides the building blocks every module is built on:
// configuration, errors, the JSON API envelope, pagination and filtering,
// the hook/event bus, the module registry, logging, and the shared
// application context. Nothing here depends on any single module.

pub mod cache;
pub mod config;
pub mod context;
pub mod error;
pub mod hardening;
pub mod hooks;
pub mod json;
pub mod logging;
pub mod middleware;
pub mod modules;
pub mod pagination;
pub mod reference_id;
pub mod routes;
pub mod storage;
pub mod tenancy;
