// Network Tools module.
//
// Developer network diagnostics: DNS lookup, ping, traceroute, port
// check, SSL/TLS checker, WHOIS, HTTP header/status check, IP info and
// Geo, subnet/CIDR calculator, webhook tester, uptime checks (one-off
// and scheduled monitors), certificate expiry watch, and DNS blacklist
// checks. Saved checks, run history, and shareable results round out
// the module.
//
// Safety model (read this before extending the module):
//   - Every tool runs server-side with strict limits: per-tool
//     timeouts (default 5s connect / 10s total, configurable per call
//     within hard caps), per-actor rate limits held in memory, and
//     SSRF guards that resolve hostnames and check EVERY resolved IP.
//     Private, loopback, link-local, reserved, and multicast targets
//     are rejected unless the caller holds
//     `network_tools:probe_private`. URLs with credentials, non-http(s)
//     schemes, and redirect targets that fail the same check are
//     rejected as well.
//   - Every tool run is audit-logged: the run is stored in the run
//     history and a `network_tools.run` hook event is emitted carrying
//     the tool, target, duration, and outcome - never the response
//     body.
//   - Ping and traceroute shell out to the system `ping` /
//     `traceroute` binaries with strictly validated arguments and a
//     hard timeout, because no raw-socket crate is available. When the
//     binary is absent, ping falls back to TCP-connect timing and the
//     result records which path was used; traceroute has no faithful
//     fallback and reports the binary as unavailable instead of
//     inventing hops.
//
// Reuse seams (do not rebuild these):
//   - DNS: `crate::modules::network::DnsVerifier` for A/AAAA/MX/TXT/
//     CNAME/NS/PTR. SOA is resolved through hickory directly because
//     `DnsVerifier` does not expose an SOA lookup.
//   - Geo/IP info: `crate::modules::geo::lookup_geo`.
//   - Certificates: the live TLS storage on `AppContext.tls`
//     (`unified_core::tls::SslStorage::list_certificates`) is consulted
//     first for domains the server already manages; the SSL/TLS
//     checker below only probes the network for other domains. This
//     module does NOT duplicate the certificates module alert store:
//     its own watch list only records which domains to watch and which
//     thresholds were already alerted, and it reuses the same
//     30/14/7/1-day thresholds.
//   - Public-link sharing: `unified_core::sharing` plus the auditors
//     module (`POST /api/v1/sharing/links`, scope kind
//     `network_tools_run`). A run result is shared by minting a link
//     through that API; this module registers the shareable kind and
//     emits `network_tools.result.shared`, it does not mint tokens
//     itself.
//
// Capabilities (caller identity comes from the request headers,
// `X-User-Id` / `X-Role`, via `viewer_of` / `tool_actor` -
// never from query parameters; see `require_capability`):
//   - `network_tools:view` - read history, saved checks, monitors,
//     watches, tool metadata.
//   - `network_tools:run` - execute any tool.
//   - `network_tools:manage` - create/update/delete saved checks,
//     uptime monitors, certificate watches; share results; trigger
//     checks.
//   - `network_tools:probe_private` - probe private / loopback /
//     link-local / reserved / multicast targets. The capability
//     alone does not enable it: the operator must also set
//     `UNIFIED_NETWORK_TOOLS_ALLOW_PRIVATE=1` server-side (see
//     `helpers::allow_private_probes`), because the role is
//     self-asserted until verified-caller auth lands (C1).
//
// Routes live under `/api/v1/network-tools`. All responses use the
// standard JSON envelope (`unified_core::json::respond`).
//
// TODO (known gap, blocked on scheduler work): nothing in this
// tree emits `network_tools.uptime.check_requested` or
// `network_tools.cert.check_requested`, so scheduled uptime and
// certificate checks never run on their own. The listeners exist
// (module.rs registers both events) but there is no trigger point
// inside this module to wire them to: this module spawns no
// background task and no timer, and its hook handlers are
// synchronous, so they can only acknowledge a request, never run a
// probe. The scheduler job must be added in the scheduler module
// (out of scope for this module): a new job kind in the scheduler's
// known-kinds list plus a dispatch arm that, on each tick, emits
// `network_tools.uptime.check_requested` once per enabled uptime
// monitor (payload `{"monitor_id", "actor": "system"}`) and
// `network_tools.cert.check_requested` for the certificate watches.
// Until that lands, scheduled checks only happen when an operator
// calls the trigger endpoints (`POST /uptime-monitors/{id}/check`,
// `POST /cert-watches/check`).
// The scheduler has no network-tools job kind yet. Scheduled uptime
// checks are exposed as the `network_tools.uptime.check_requested`
// hook event plus the `POST /uptime-monitors/{id}/check` trigger
// endpoint; the integrator must arm a scheduler job that emits that
// event (or calls the trigger endpoint) at each monitor interval.
// Failures emit `network_tools.uptime.down` and request a
// notification through `notification.requested`, using the same direct
// pattern the certificates module uses for `ssl.expiring`.

pub mod ai_capabilities;
pub mod types;
mod store;
mod helpers;
mod validation;
mod tools;
mod tools_tls;
mod tools_net;
mod tools_monitor;
mod handlers;
mod handlers_saved;
mod handlers_monitors;
mod hooks;
mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::DEFAULT_CONNECT_TIMEOUT_SECS;
pub use self::types::DEFAULT_TOTAL_TIMEOUT_SECS;
pub use self::types::MAX_TOTAL_TIMEOUT_SECS;
pub use self::types::MAX_CONNECT_TIMEOUT_SECS;
pub use self::types::MAX_PING_COUNT;
pub use self::types::MAX_TRACEROUTE_HOPS;
pub use self::types::MAX_BANNER_BYTES;
pub use self::types::MAX_WEBHOOK_BODY_BYTES;
pub use self::types::MAX_WHOIS_TEXT_CHARS;
pub use self::types::MAX_HTTP_REDIRECTS;
pub use self::types::RATE_LIMIT_RUNS_PER_MINUTE;
pub use self::types::RATE_LIMIT_READS_PER_MINUTE;
pub use self::types::CERT_ALERT_THRESHOLDS_DAYS;
pub use self::types::DEFAULT_DNSBL_ZONES;
pub use self::types::network_tool_events;
pub use self::types::NetworkToolSavedCheck;
pub use self::types::NetworkToolRun;
pub use self::types::NetworkToolUptimeMonitor;
pub use self::types::NetworkToolUptimeEvent;
pub use self::types::NetworkToolCertWatch;
pub use self::types::ToolTargetRequest;
pub use self::types::DnsLookupRequest;
pub use self::types::PingRequest;
pub use self::types::TracerouteRequest;
pub use self::types::PortCheckRequest;
pub use self::types::WhoisRequest;
pub use self::types::HttpCheckRequest;
pub use self::types::WebhookTestRequest;
pub use self::types::SubnetRequest;
pub use self::types::BlacklistRequest;
pub use self::types::CreateSavedCheckRequest;
pub use self::types::UpdateSavedCheckRequest;
pub use self::types::CreateUptimeMonitorRequest;
pub use self::types::UpdateUptimeMonitorRequest;
pub use self::types::CreateCertWatchRequest;
pub use self::types::ShareRunRequest;
pub use self::types::ToolResult;
pub use self::module::NetworkToolsModule;
pub use self::module::register;
