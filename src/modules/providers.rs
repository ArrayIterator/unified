// Cloud / VPS / container provider connectors.
//
// One hookable provider registry with a common adapter interface, so new
// providers plug in through the registry instead of scattered code.
// Every adapter is an honest, typed HTTP client: it validates its
// configuration, tests the connection against the real provider API,
// and maps provider responses onto the shared `ServerInfo` shape. No
// adapter pretends to be a full vendor SDK.
//
// Scope of this round (deliberately minimal, per provider):
//
//   - Hetzner Cloud: servers list / status / create / scale (change
//     type) / delete.
//   - OVH: Public Cloud instances list / status / create / resize /
//     delete (one project at a time; the project id is the connection's
//     `project_id`).
//   - Tencent Cloud CVM: instances list / status only.
//   - Huawei Cloud ECS: instances list / status only (IAM token flow
//     or AK/SK request signing).
//   - AWS EC2: instances list / status only (SigV4 signed query API).
//     Object storage is out of scope here; there is no storage module
//     in this tree to reuse or duplicate.
//   - Google Cloud Compute Engine: instances list / status only, with
//     an OAuth2 access token supplied through settings. Token refresh
//     is out of scope; an expired token fails the connection test with
//     a clear 401.
//   - Virtualizor: VPS list / status / create / start / stop / delete
//     against a panel URL supplied on the connection.
//   - Docker: remote Engine API containers list / inspect / start /
//     stop (no image management, no create in this round).
//   - Kubernetes: namespaces, pods and deployments list / status
//     (read-only).
//   - Redshift: clusters list / status only (read-only; no query
//     execution in this round).
//   - DigitalOcean: droplets list / status / create / resize /
//     delete / power actions.
//   - Linode: instances list / status / create / resize / delete /
//     boot / shutdown / reboot.
//   - OpenStack: servers list / status / create / delete / start /
//     stop via Keystone auth + Nova (resize needs a confirm step and
//     is out of scope here).
//   - Proxmox VE: nodes + QEMU / LXC guests list / status / start /
//     stop / delete (ticket + CSRF auth; creating a bootable guest
//     needs a template / ISO choice the generic create request
//     cannot express, so create is out of scope here).
//   - KVM: the KVM (QEMU) guests of a Proxmox VE host, through the
//     Proxmox API. Plain KVM / libvirt exposes no HTTP API, so a
//     direct libvirt driver is out of scope for this round.
//   - XenServer / XCP-ng: VMs list / status / start / stop /
//     delete over the XenAPI XML-RPC endpoint (hand-rolled minimal
//     XML-RPC client; VM creation needs template clone + provision
//     and is out of scope here).
//   - Custom: a config-driven generic adapter -- base URL, auth
//     type, endpoint templates, and response field mapping supplied
//     on the connection, so a new provider plugs in without a code
//     change (template-based mapping only; exotic response shapes
//     still need a real adapter).
//
// Credentials live only on the connection record, are never logged,
// and are never returned by any API response: read views expose only
// which credential fields are configured. Provider API hosts are
// allow-listed per adapter; user-supplied panel endpoints (Virtualizor,
// Docker, Kubernetes) must be http(s) URLs with a host and no embedded
// credentials. A 429 from a provider is surfaced as a 429 with the
// provider's `Retry-After` delay in `error.metadata`.
//
// Inventory is cached in memory per connection (default TTL 300s) and
// refreshed by the sync-now endpoint. Events emitted on the hook bus:
// `provider.connected`, `provider.sync.completed`,
// `provider.sync.failed`, `provider.server.created`. The audit log can
// subscribe to these (add them to `AUDITED_EVENTS` in the audit module)
// to record connection add/delete and create/scale actions.
//
// Wiring to the service monitor happens through hooks only: this module
// subscribes to `app.reload` (emitted by the service monitor reload
// endpoint) and clears the inventory cache, since a reload may change
// provider configuration. It never calls the service monitor directly.

pub mod types;
mod validation;
pub mod store;
pub mod http;
pub mod adapters;
pub mod adapters_cloud;
pub mod adapters_custom;
pub mod adapters_vps;
pub mod adapters_infra;
mod handlers;
pub mod module;

pub use self::types::*;
pub use self::validation::*;
pub use self::store::*;
pub use self::http::*;
pub use self::adapters::*;
pub use self::adapters_cloud::*;
pub use self::adapters_custom::*;
pub use self::adapters_vps::*;
pub use self::adapters_infra::*;
pub use self::handlers::*;
pub use self::module::*;
