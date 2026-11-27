// Cloud - hosting, cloud providers, containers, virtualization, and
// app deployment through those connectors.
//
// The cloud module is a CONNECTOR layer, not a VPS/VM panel: each
// provider adapter translates an external platform's API (Hetzner,
// OpenStack, Docker, Kubernetes, Proxmox, ...) into the ERP's uniform
// `CloudProvider` trait. Unified never reimplements the platforms it
// connects to (see `docs/integration/CLOUD_OVERHAUL.md`).
//
// Layout:
//   - `cloud::hosting` - hosting plans + accounts (moved from the old
//     `hosting` module; the old id stays as a deprecated compat shim).
//   - `cloud::providers` - provider connections + adapters (moved from
//     the old `providers` module; the old id stays as a compat shim).
//   - `cloud::containers` - app deployment through the connectors.
//   - `cloud::virtualization` - grouping view over the virtualization
//     connectors (Proxmox / KVM / Xen / Virtualizor).
//   - `cloud::hooks` - hook contracts for "cloud panel" external
//     modules (panel / SSL-provider connectors).
//
// The legacy routes `/api/v1/hosting/*` and `/api/v1/providers/*`
// keep working through the compat modules; the canonical routes are
// `/api/v1/cloud/*`.

pub mod ai_capabilities;
pub mod containers;
pub mod events;
pub mod handlers;
pub mod helpers;
pub mod hooks;
pub mod hosting;
pub mod module;
pub mod providers;
pub mod virtualization;

pub use self::module::{register, CloudModule};
