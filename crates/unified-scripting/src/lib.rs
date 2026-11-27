//! TypeScript external-module runtime for Unified.
//!
//! External modules are authored in TypeScript. This crate is the
//! server side of that contract: it transpiles `.ts` sources to
//! JavaScript in memory (OXC, via `unified-ts`), executes them in an
//! embedded QuickJS runtime, and injects one `Unified` global object
//! holding the whole SDK - the same shape as Bun's `Bun` global.
//!
//! There is deliberately no V8 here: QuickJS is small (~1 MB),
//! embeddable, and musl-friendly, while V8 would break the static
//! musl binary. No JavaScript engine other than QuickJS is used.

pub mod engine;
pub mod error;
pub mod host;
pub mod loader;
pub mod module;
pub mod sdk;
pub mod sdk_db;
pub mod sdk_http;
pub mod sdk_storage;

/// The one function the server calls (see `src/modules/mod.rs`):
/// re-exported at the crate root so `unified_scripting::register`
/// resolves. The definition lives in `module.rs`.
pub use module::register;
