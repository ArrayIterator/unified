// Panel integrations module - hosting / billing panel connectors.
//
// One hookable panel registry with a common adapter interface, so new
// panels plug in through the registry instead of scattered code. Every
// adapter is an honest, typed HTTP client: it validates its
// configuration, tests the connection against the real panel API, and
// maps panel responses onto the shared record shapes. No adapter
// pretends to be a full vendor SDK, and no adapter claims a collection
// it cannot actually fetch: the capability flags are the contract.
//
// Scope of this round (deliberately minimal, per panel):
//
//   - WHMCS: clients, products, services (client products), domains,
//     and invoices through the action-based API (`includes/api.php`
//     with identifier + secret). No write operations in this round:
//     creating or updating billing records through the panel API is
//     out of scope; this module imports and reports only.
//   - cPanel / WHM: server accounts (`listaccts`), packages
//     (`listpkgs`, exposed as products), and the domains reported by
//     `listaccts`. WHM has no billing clients / invoices API, so
//     those collections are unsupported here.
//   - DirectAdmin: users (`CMD_API_SHOW_ALL_USERS`, exposed as
//     clients and as server accounts), domains, and packages. The
//     JSON shapes returned by DirectAdmin vary by version; the parser
//     accepts an array, a `list` array, and a map of objects, and
//     reports an error when none of those shapes is present.
//   - Plesk: clients, subscriptions (exposed as services), domains,
//     and service plans (exposed as products) through the JSON API
//     (`/api/v2/...`) with an API key. Plesk has no billing invoices
//     API, so invoices are unsupported here.
//   - CyberPanel: websites (exposed as server accounts and as
//     domains) and packages (exposed as products) through the JSON
//     API. CyberPanel has no billing clients / services / invoices
//     API, so those collections are unsupported here.
//   - Generic: a configuration-driven adapter (base URL + auth type +
//     per-collection endpoint templates + response field mapping) for
//     any other panel that answers JSON over HTTP. It validates its
//     configuration, allow-lists only the configured base host, and
//     reports exactly which collections it was configured for.
//
// Credentials live only on the connection record, are never logged,
// and are never returned by any API response: read views expose only
// which credential fields are configured. Panel API hosts are
// allow-listed per connection: every request must target the host of
// the connection's own endpoint URL, and user-supplied endpoints must
// be http(s) URLs with a host and no embedded credentials. A 429 from
// a panel is surfaced as a 429 with the panel's `Retry-After` delay
// in `error.metadata`.
//
// Imported records are stored in memory per connection (same pattern
// as the provider inventory store), each with its remote id, a hash
// of the remote payload, a last-synced timestamp, and two-way sync
// state. Two-way sync never silently overwrites: when a record
// changed both locally (flagged through the API) and remotely, the
// sync reports a conflict and keeps the remote payload unless the
// record is explicitly flagged `prefer_local`, in which case the
// local version wins and the conflict is logged. Events emitted on
// the hook bus: `panel.connected`, `panel.client.synced`,
// `panel.service.synced`, `panel.sync.completed`,
// `panel.sync.failed`, plus `panel.connection.created` and
// `panel.connection.deleted` for the audit trail (add them to
// `AUDITED_EVENTS` in the audit module to record connection
// add/delete and sync runs).

pub mod adapter;
pub mod adapters_cpanel;
pub mod adapters_cyberpanel;
pub mod adapters_directadmin;
pub mod adapters_generic;
pub mod adapters_plesk;
pub mod adapters_whmcs;
pub mod ai_capabilities;
pub mod constants;
pub mod handlers;
mod handlers_connections;
mod handlers_records;
pub mod module;
pub mod registry;
pub mod store;
pub mod sync;
pub mod types;

pub use self::adapter::{PanelAdapter, payload_hash};
pub use self::adapters_cpanel::CpanelAdapter;
pub use self::adapters_cyberpanel::CyberPanelAdapter;
pub use self::adapters_directadmin::DirectAdminAdapter;
pub use self::adapters_generic::GenericAdapter;
pub use self::adapters_plesk::PleskAdapter;
pub use self::adapters_whmcs::WhmcsAdapter;
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::constants::{COLLECTIONS, PANEL_CONNECTION_DELETED, PANEL_SYNC_CONFLICT};
pub use self::constants::{PANEL_CLIENT_SYNCED, PANEL_CONNECTED, PANEL_SERVICE_SYNCED};
pub use self::constants::{PANEL_CONNECTION_CREATED, PANEL_SYNC_COMPLETED, PANEL_SYNC_FAILED};
pub use self::handlers::{CreateConnectionRequest, UpdateConnectionRequest};
pub use self::handlers::{MarkLocalChangedRequest, ResolveConflictRequest};
pub use self::module::{PanelsModule, register};
pub use self::registry::{PanelFactory, PanelRegistry, panel_registry};
pub use self::store::{PanelSyncState, SyncDirection, SyncedRecord};
pub use self::sync::{CollectionSyncOutcome, PanelSyncResult};
pub use self::types::{GenericFieldMapping, GenericPanelConfig, PanelCredentials, PanelSyncCounts};
pub use self::types::{PanelCapabilities, PanelKind, collection_from_segment, collection_segment};
pub use self::types::{PanelClient, PanelInfo, PanelProduct, PanelService, capabilities_for};
pub use self::types::{PanelConnection, PanelConnectionView, generic_capabilities};
pub use self::types::{PanelDomain, PanelInvoice, PanelServerAccount, RemoteRecord};
