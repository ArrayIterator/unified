// Routing rules for the Pingora gateway.
//
// A rule matches an incoming request by host, path, and headers, and
// names the upstream that serves it. Rules are evaluated in priority
// order (highest first); the first enabled rule whose matchers all
// match wins. When no rule matches, the gateway falls back to the
// configured default upstream, or rejects the request when no default
// is configured.
//
// Upstream references:
//   - `balancer`: a load balancer id. Upstreams are resolved at
//     request time through the load-balancer module
//     (`eligible_backends` + `select_backend`), so a balancer's
//     backends — including cluster-node backends (`source_kind:
//     "cluster"`, `source_id: "<cluster_id>/<node_id>"`) and provider
//     backends — are reached without the gateway importing the
//     cluster or provider modules. The gateway never caches a
//     resolved backend address past the health-check TTL: selection
//     runs against the live eligible set on every request.
//   - `address`: one plain `host:port` upstream.
//   - `address_pool`: a weighted list of plain `host:port` upstreams,
//     selected with the same `select_backend` function over
//     synthesized backend records.
//
// There is deliberately no separate `cluster` upstream kind in this
// version: the cluster module exposes no public node-list accessor
// (its `ClustersState` fields are private), so a cluster node cannot
// be enumerated directly. Cluster nodes are proxied by pointing a
// rule at a load balancer whose backends reference those nodes. The
// integrator snippet for a direct cluster resolver (a public node
// snapshot on the cluster module) is in the coordinator report.
//
// Rate limiting is NOT reimplemented here: `rate_limit_override`
// only names a rule owned by the hardening module, which enforces
// it. A name that does not exist there is a configuration error at
// enforcement time, not at rule-creation time, because the hardening
// module exposes no public rule lookup to validate against.

use crate::core::error::{UnifiedError, UnifiedResult};
use crate::modules::load_balancer::{
    parse_host_port, HealthCheckConfig, Strategy,
};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Limits.
// ---------------------------------------------------------------------------

const MAX_NAME_LEN: usize = 200;
const MAX_HOST_LEN: usize = 253;
const MAX_PATH_LEN: usize = 2048;
const MAX_HEADER_MATCHES: usize = 16;
const MAX_HEADER_VALUE_LEN: usize = 1024;
const MAX_POOL_ADDRESSES: usize = 128;
const MIN_PRIORITY: i32 = -10_000;
const MAX_PRIORITY: i32 = 10_000;
const MAX_WEIGHT: u32 = 100;
const DEFAULT_PRIORITY: i32 = 100;

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

/// One header matcher. `exact: true` requires the whole header value
/// to equal `value` (case-insensitive); `exact: false` requires the
/// header value to contain `value` (case-insensitive substring).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderMatch {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub exact: bool,
}

/// What a rule forwards to.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpstreamRef {
    /// A load balancer id; backends come from the load-balancer
    /// module at request time.
    Balancer { balancer_id: Uuid },
    /// One plain `host:port` upstream.
    Address { address: String },
    /// A weighted list of plain `host:port` upstreams.
    AddressPool { addresses: Vec<PoolAddress> },
}

/// One member of a direct address pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolAddress {
    pub address: String,
    #[serde(default = "default_pool_weight")]
    pub weight: u32,
}

fn default_pool_weight() -> u32 {
    1
}

/// One routing rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingRule {
    pub id: Uuid,
    pub name: String,
    /// Higher values win. Ties break on `created_at` (oldest first).
    pub priority: i32,
    pub enabled: bool,
    /// Host to match, lowercased, without a port. `None` matches any
    /// host. A leading `*.` matches exactly one label
    /// (`*.example.com` matches `api.example.com`, not
    /// `example.com` and not `a.b.example.com`).
    pub host: Option<String>,
    /// Path must start with this prefix. `None` matches any path.
    pub path_prefix: Option<String>,
    /// Path must match this regular expression. `None` matches any
    /// path. Combined with `path_prefix`, both must match.
    pub path_regex: Option<String>,
    /// Every header matcher must match. Empty matches any headers.
    #[serde(default)]
    pub headers: Vec<HeaderMatch>,
    pub upstream: UpstreamRef,
    /// Selection strategy used for this rule's pool. This is the
    /// gateway's own strategy: the load balancer's stored strategy is
    /// not readable here (its `get_balancer` accessor is private),
    /// so the rule carries the strategy that governs gateway routing.
    pub strategy: Strategy,
    /// When true, the request must have arrived over the TLS listener;
    /// plain-HTTP requests matching the rule are rejected.
    pub require_tls: bool,
    /// Name of a rate-limit rule owned by the hardening module. The
    /// hardening module enforces it; the gateway only stores the
    /// reference and forwards the request with the rule id attached.
    pub rate_limit_override: Option<String>,
    /// Health-check configuration used by the gateway sweeper for
    /// this rule's direct upstreams (address pools). Balancer pools
    /// are health-checked by the load-balancer module itself.
    pub health_check: HealthCheckConfig,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Compiled form of `path_regex`. Never serialized; rebuilt by
    /// `compile_regex` after deserialization.
    #[serde(skip, default)]
    pub compiled_path_regex: Option<Regex>,
}

impl RoutingRule {
    /// Compile `path_regex` into `compiled_path_regex`. Called by
    /// validation and after loading a rule from configuration.
    pub fn compile_regex(&mut self) -> UnifiedResult<()> {
        self.compiled_path_regex = match &self.path_regex {
            Some(pattern) => Some(Regex::new(pattern).map_err(|error| {
                UnifiedError::bad_request(format!("Invalid path regex \"{pattern}\": {error}"))
            })?),
            None => None,
        };
        Ok(())
    }
}

/// Request body for creating a routing rule.
#[derive(Debug, Deserialize)]
pub struct CreateRuleRequest {
    pub name: String,
    pub priority: Option<i32>,
    pub host: Option<String>,
    pub path_prefix: Option<String>,
    pub path_regex: Option<String>,
    pub headers: Option<Vec<HeaderMatch>>,
    pub upstream: UpstreamRef,
    pub strategy: Option<Strategy>,
    pub require_tls: Option<bool>,
    pub rate_limit_override: Option<String>,
    pub health_check: Option<HealthCheckConfig>,
}

/// Request body for updating a routing rule. Every field is optional;
/// absent fields keep their current value. `rate_limit_override`
/// uses a double option: `None` keeps the value, `Some(None)`
/// clears it, `Some(Some(value))` sets it.
#[derive(Debug, Deserialize)]
pub struct UpdateRuleRequest {
    pub name: Option<String>,
    pub priority: Option<i32>,
    pub host: Option<String>,
    pub path_prefix: Option<String>,
    pub path_regex: Option<String>,
    pub headers: Option<Vec<HeaderMatch>>,
    pub upstream: Option<UpstreamRef>,
    pub strategy: Option<Strategy>,
    pub require_tls: Option<bool>,
    pub enabled: Option<bool>,
    pub rate_limit_override: Option<Option<String>>,
    pub health_check: Option<HealthCheckConfig>,
}

// ---------------------------------------------------------------------------
// Matching (pure functions).
// ---------------------------------------------------------------------------

/// True when `host` (already stripped of any port, lowercased)
/// matches the rule host pattern: exact match, or a `*.` wildcard
/// matching exactly one label.
pub fn host_matches(pattern: &str, host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let pattern = pattern.trim_end_matches('.').to_ascii_lowercase();
    if pattern == host {
        return true;
    }
    if let Some(base) = pattern.strip_prefix("*.") {
        if host != base
            && host.ends_with(&format!(".{base}"))
            && host.matches('.').count() == base.matches('.').count() + 1
        {
            return true;
        }
    }
    false
}

/// True when every matcher of the rule matches the request. All
/// matchers are ANDed: host, path prefix, path regex, and every
/// header matcher must match.
pub fn rule_matches(rule: &RoutingRule, host: &str, path: &str, headers: &[(String, String)]) -> bool {
    if !rule.enabled {
        return false;
    }
    // The host header may carry a port (`example.com:8081`); strip it
    // before matching. IPv6 literals keep their brackets.
    let host_only = strip_host_port(host);
    if let Some(pattern) = &rule.host {
        if !host_matches(pattern, &host_only) {
            return false;
        }
    }
    if let Some(prefix) = &rule.path_prefix {
        if !path.starts_with(prefix.as_str()) {
            return false;
        }
    }
    if let Some(regex) = &rule.compiled_path_regex {
        if !regex.is_match(path) {
            return false;
        }
    } else if let Some(pattern) = &rule.path_regex {
        match Regex::new(pattern) {
            Ok(regex) => {
                if !regex.is_match(path) {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    for matcher in &rule.headers {
        let wanted = matcher.value.to_ascii_lowercase();
        let found = headers.iter().any(|(name, value)| {
            if !name.eq_ignore_ascii_case(&matcher.name) {
                return false;
            }
            let value = value.to_ascii_lowercase();
            if matcher.exact {
                value == wanted
            } else {
                value.contains(&wanted)
            }
        });
        if !found {
            return false;
        }
    }
    true
}

/// Strip a `:port` suffix from a host header value. A bare IPv6
/// literal in brackets keeps its brackets; an unbracketed value with
/// more than one colon is returned unchanged.
pub fn strip_host_port(host: &str) -> String {
    let host = host.trim();
    if let Some(rest) = host.strip_prefix('[') {
        if let Some((inside, _)) = rest.split_once(']') {
            return inside.to_string();
        }
        return host.to_string();
    }
    if host.matches(':').count() == 1 {
        if let Some((name, _)) = host.rsplit_once(':') {
            return name.to_string();
        }
    }
    host.to_string()
}

/// The winning rule for a request: enabled rules sorted by priority
/// (highest first), then `created_at` (oldest first); the first rule
/// whose matchers all match wins.
pub fn select_rule<'a>(
    rules: &'a [RoutingRule],
    host: &str,
    path: &str,
    headers: &[(String, String)],
) -> Option<&'a RoutingRule> {
    let mut ordered: Vec<&RoutingRule> = rules.iter().filter(|rule| rule.enabled).collect();
    ordered.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.created_at.cmp(&b.created_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    ordered.into_iter().find(|rule| rule_matches(rule, host, path, headers))
}

/// Rules in evaluation order (priority desc, oldest first).
pub fn sorted_rules(rules: Vec<RoutingRule>) -> Vec<RoutingRule> {
    let mut rules = rules;
    rules.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.created_at.cmp(&b.created_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    rules
}

// ---------------------------------------------------------------------------
// Validation.
// ---------------------------------------------------------------------------

fn validate_name(name: &str) -> UnifiedResult<String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(UnifiedError::bad_request("Rule name must not be empty"));
    }
    if name.len() > MAX_NAME_LEN {
        return Err(UnifiedError::bad_request(format!(
            "Rule name must be at most {MAX_NAME_LEN} characters"
        )));
    }
    Ok(name)
}

fn validate_host(host: &str) -> UnifiedResult<String> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || host.len() > MAX_HOST_LEN {
        return Err(UnifiedError::bad_request(
            "Rule host must not be empty and must be at most 253 characters",
        ));
    }
    let bare = host.strip_prefix("*.").unwrap_or(&host);
    if bare.is_empty()
        || bare.starts_with('-')
        || bare.starts_with('.')
        || bare.contains("..")
        || !bare
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == ':')
    {
        return Err(UnifiedError::bad_request(format!("Invalid rule host \"{host}\"")));
    }
    Ok(host)
}

fn validate_priority(priority: i32) -> UnifiedResult<i32> {
    if !(MIN_PRIORITY..=MAX_PRIORITY).contains(&priority) {
        return Err(UnifiedError::bad_request(format!(
            "Rule priority must be between {MIN_PRIORITY} and {MAX_PRIORITY}"
        )));
    }
    Ok(priority)
}

fn validate_health_check(config: &HealthCheckConfig) -> UnifiedResult<()> {
    // Mirrors the load-balancer module's health-check validation
    // (that function is private to the module); keep the two in sync.
    if config.kind == crate::modules::load_balancer::HealthCheckKind::Http {
        if !config.path.starts_with('/') {
            return Err(UnifiedError::bad_request(
                "Health check path must start with \"/\"",
            ));
        }
        if config.path.len() > 512 {
            return Err(UnifiedError::bad_request(
                "Health check path must be at most 512 characters",
            ));
        }
    }
    if !(5..=3600).contains(&config.interval_secs) {
        return Err(UnifiedError::bad_request(
            "Health check interval must be between 5 and 3600 seconds",
        ));
    }
    if config.timeout_secs < 1 || config.timeout_secs > 60 {
        return Err(UnifiedError::bad_request(
            "Health check timeout must be between 1 and 60 seconds",
        ));
    }
    if config.timeout_secs >= config.interval_secs {
        return Err(UnifiedError::bad_request(
            "Health check timeout must be smaller than the check interval",
        ));
    }
    if config.healthy_threshold < 1 || config.healthy_threshold > 10 {
        return Err(UnifiedError::bad_request(
            "Healthy threshold must be between 1 and 10",
        ));
    }
    if config.unhealthy_threshold < 1 || config.unhealthy_threshold > 10 {
        return Err(UnifiedError::bad_request(
            "Unhealthy threshold must be between 1 and 10",
        ));
    }
    if let Some(status) = config.expected_status {
        if !(100..=599).contains(&status) {
            return Err(UnifiedError::bad_request(
                "Expected HTTP status must be between 100 and 599",
            ));
        }
    }
    Ok(())
}

fn validate_upstream(upstream: &UpstreamRef) -> UnifiedResult<()> {
    match upstream {
        UpstreamRef::Balancer { .. } => Ok(()),
        UpstreamRef::Address { address } => {
            parse_host_port(address).ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Upstream address must be a valid \"host:port\" address, got \"{address}\""
                ))
            })?;
            Ok(())
        }
        UpstreamRef::AddressPool { addresses } => {
            if addresses.is_empty() {
                return Err(UnifiedError::bad_request(
                    "Upstream address pool must contain at least one address",
                ));
            }
            if addresses.len() > MAX_POOL_ADDRESSES {
                return Err(UnifiedError::bad_request(format!(
                    "Upstream address pool must contain at most {MAX_POOL_ADDRESSES} addresses"
                )));
            }
            let mut seen: Vec<String> = Vec::new();
            for entry in addresses {
                parse_host_port(&entry.address).ok_or_else(|| {
                    UnifiedError::bad_request(format!(
                        "Upstream address must be a valid \"host:port\" address, got \"{}\"",
                        entry.address
                    ))
                })?;
                if !(1..=MAX_WEIGHT).contains(&entry.weight) {
                    return Err(UnifiedError::bad_request(format!(
                        "Upstream weight for \"{}\" must be between 1 and {MAX_WEIGHT}",
                        entry.address
                    )));
                }
                if seen.contains(&entry.address) {
                    return Err(UnifiedError::bad_request(format!(
                        "Upstream address pool contains a duplicate address \"{}\"",
                        entry.address
                    )));
                }
                seen.push(entry.address.clone());
            }
            Ok(())
        }
    }
}

fn validate_header_matches(headers: &[HeaderMatch]) -> UnifiedResult<Vec<HeaderMatch>> {
    if headers.len() > MAX_HEADER_MATCHES {
        return Err(UnifiedError::bad_request(format!(
            "A rule may have at most {MAX_HEADER_MATCHES} header matchers"
        )));
    }
    let mut out = Vec::with_capacity(headers.len());
    for matcher in headers {
        let name = matcher.name.trim().to_string();
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(UnifiedError::bad_request(format!(
                "Invalid header name \"{}\"",
                matcher.name
            )));
        }
        if matcher.value.len() > MAX_HEADER_VALUE_LEN {
            return Err(UnifiedError::bad_request(format!(
                "Header match value for \"{name}\" must be at most {MAX_HEADER_VALUE_LEN} characters"
            )));
        }
        out.push(HeaderMatch {
            name,
            value: matcher.value.clone(),
            exact: matcher.exact,
        });
    }
    Ok(out)
}

fn validate_rate_limit_override(value: &Option<String>) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(name) => {
            let name = name.trim().to_string();
            if name.is_empty() || name.len() > MAX_NAME_LEN {
                return Err(UnifiedError::bad_request(
                    "Rate limit override must be a non-empty rule name of at most 200 characters",
                ));
            }
            Ok(Some(name))
        }
    }
}

// ---------------------------------------------------------------------------
// Store (process-wide, in memory, like the load balancer's stores).
// Rules are configuration, not traffic state: they reset on restart
// until the integrator lands the persistence migration; the
// management API is the source of truth at runtime.
// ---------------------------------------------------------------------------

static RULES: OnceLock<DashMap<Uuid, RoutingRule>> = OnceLock::new();

fn rules_store() -> &'static DashMap<Uuid, RoutingRule> {
    RULES.get_or_init(DashMap::new)
}

/// Every rule, in evaluation order.
pub fn list_rules() -> Vec<RoutingRule> {
    sorted_rules(
        rules_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect(),
    )
}

pub fn get_rule(id: Uuid) -> UnifiedResult<RoutingRule> {
    rules_store()
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Routing rule \"{id}\" not found")))
}

/// Build and store a rule from a create request.
pub fn create_rule(body: CreateRuleRequest) -> UnifiedResult<RoutingRule> {
    let name = validate_name(&body.name)?;
    let priority = validate_priority(body.priority.unwrap_or(DEFAULT_PRIORITY))?;
    let host = match &body.host {
        Some(host) => Some(validate_host(host)?),
        None => None,
    };
    if let Some(prefix) = &body.path_prefix {
        if !prefix.starts_with('/') || prefix.len() > MAX_PATH_LEN {
            return Err(UnifiedError::bad_request(
                "Path prefix must start with \"/\" and be at most 2048 characters",
            ));
        }
    }
    if let Some(pattern) = &body.path_regex {
        if pattern.len() > MAX_PATH_LEN {
            return Err(UnifiedError::bad_request(
                "Path regex must be at most 2048 characters",
            ));
        }
        Regex::new(pattern).map_err(|error| {
            UnifiedError::bad_request(format!("Invalid path regex \"{pattern}\": {error}"))
        })?;
    }
    let headers = validate_header_matches(&body.headers.unwrap_or_default())?;
    validate_upstream(&body.upstream)?;
    let health_check = body.health_check.unwrap_or_default();
    validate_health_check(&health_check)?;
    let rate_limit_override = validate_rate_limit_override(&body.rate_limit_override)?;
    let now = Utc::now();
    let mut rule = RoutingRule {
        id: Uuid::now_v7(),
        name,
        priority,
        enabled: true,
        host,
        path_prefix: body.path_prefix,
        path_regex: body.path_regex,
        headers,
        upstream: body.upstream,
        strategy: body.strategy.unwrap_or_default(),
        require_tls: body.require_tls.unwrap_or(false),
        rate_limit_override,
        health_check,
        created_at: now,
        updated_at: now,
        compiled_path_regex: None,
    };
    rule.compile_regex()?;
    rules_store().insert(rule.id, rule.clone());
    Ok(rule)
}

/// Apply an update request to a stored rule.
pub fn update_rule(id: Uuid, body: UpdateRuleRequest) -> UnifiedResult<RoutingRule> {
    let Some(mut entry) = rules_store().get_mut(&id) else {
        return Err(UnifiedError::not_found(format!("Routing rule \"{id}\" not found")));
    };
    if let Some(name) = &body.name {
        entry.name = validate_name(name)?;
    }
    if let Some(priority) = body.priority {
        entry.priority = validate_priority(priority)?;
    }
    if let Some(host) = &body.host {
        entry.host = Some(validate_host(host)?);
    }
    if let Some(prefix) = &body.path_prefix {
        if !prefix.starts_with('/') || prefix.len() > MAX_PATH_LEN {
            return Err(UnifiedError::bad_request(
                "Path prefix must start with \"/\" and be at most 2048 characters",
            ));
        }
        entry.path_prefix = Some(prefix.clone());
    }
    if let Some(pattern) = &body.path_regex {
        if pattern.len() > MAX_PATH_LEN {
            return Err(UnifiedError::bad_request(
                "Path regex must be at most 2048 characters",
            ));
        }
        Regex::new(pattern).map_err(|error| {
            UnifiedError::bad_request(format!("Invalid path regex \"{pattern}\": {error}"))
        })?;
        entry.path_regex = Some(pattern.clone());
    }
    if let Some(headers) = &body.headers {
        entry.headers = validate_header_matches(headers)?;
    }
    if let Some(upstream) = body.upstream {
        validate_upstream(&upstream)?;
        entry.upstream = upstream;
    }
    if let Some(strategy) = body.strategy {
        entry.strategy = strategy;
    }
    if let Some(require_tls) = body.require_tls {
        entry.require_tls = require_tls;
    }
    if let Some(enabled) = body.enabled {
        entry.enabled = enabled;
    }
    if let Some(rate_limit_override) = &body.rate_limit_override {
        entry.rate_limit_override = validate_rate_limit_override(rate_limit_override)?;
    }
    if let Some(health_check) = body.health_check {
        validate_health_check(&health_check)?;
        entry.health_check = health_check;
    }
    entry.updated_at = Utc::now();
    entry.compile_regex()?;
    Ok(entry.clone())
}

pub fn delete_rule(id: Uuid) -> UnifiedResult<RoutingRule> {
    rules_store()
        .remove(&id)
        .map(|(_, rule)| rule)
        .ok_or_else(|| UnifiedError::not_found(format!("Routing rule \"{id}\" not found")))
}

pub fn rule_count() -> usize {
    rules_store().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_rule(priority: i32, host: Option<&str>, prefix: Option<&str>) -> RoutingRule {
        let now = Utc::now();
        RoutingRule {
            id: Uuid::now_v7(),
            name: "test".to_string(),
            priority,
            enabled: true,
            host: host.map(str::to_string),
            path_prefix: prefix.map(str::to_string),
            path_regex: None,
            headers: Vec::new(),
            upstream: UpstreamRef::Address {
                address: "127.0.0.1:8080".to_string(),
            },
            strategy: Strategy::RoundRobin,
            require_tls: false,
            rate_limit_override: None,
            health_check: HealthCheckConfig::default(),
            created_at: now,
            updated_at: now,
            compiled_path_regex: None,
        }
    }

    #[test]
    fn host_wildcard_matches_one_label_only() {
        assert!(host_matches("*.example.com", "api.example.com"));
        assert!(!host_matches("*.example.com", "example.com"));
        assert!(!host_matches("*.example.com", "a.b.example.com"));
        assert!(host_matches("example.com", "example.com"));
        assert!(host_matches("example.com", "EXAMPLE.COM"));
    }

    #[test]
    fn host_port_is_stripped_before_matching() {
        assert_eq!(strip_host_port("example.com:8081"), "example.com");
        assert_eq!(strip_host_port("example.com"), "example.com");
        assert_eq!(strip_host_port("[::1]:8081"), "::1");
    }

    #[test]
    fn higher_priority_rule_wins() {
        let low = test_rule(10, Some("example.com"), None);
        let high = test_rule(200, Some("example.com"), Some("/api"));
        let rules = vec![low, high];
        let picked = select_rule(&rules, "example.com", "/api/users", &[]).expect("a rule matches");
        assert_eq!(picked.priority, 200);
        let picked = select_rule(&rules, "example.com", "/other", &[]).expect("a rule matches");
        assert_eq!(picked.priority, 10);
    }

    #[test]
    fn path_regex_and_header_matchers_are_anded() {
        let mut rule = test_rule(100, None, None);
        rule.path_regex = Some("^/v[0-9]+/".to_string());
        rule.compile_regex().expect("regex compiles");
        rule.headers = vec![HeaderMatch {
            name: "x-tenant".to_string(),
            value: "acme".to_string(),
            exact: true,
        }];
        assert!(rule_matches(
            &rule,
            "example.com",
            "/v2/items",
            &[("x-tenant".to_string(), "acme".to_string())]
        ));
        assert!(!rule_matches(
            &rule,
            "example.com",
            "/v2/items",
            &[("x-tenant".to_string(), "other".to_string())]
        ));
        assert!(!rule_matches(
            &rule,
            "example.com",
            "/items",
            &[("x-tenant".to_string(), "acme".to_string())]
        ));
    }

    #[test]
    fn validation_rejects_bad_upstream_and_regex() {
        let bad_address = CreateRuleRequest {
            name: "bad".to_string(),
            priority: None,
            host: None,
            path_prefix: None,
            path_regex: None,
            headers: None,
            upstream: UpstreamRef::Address {
                address: "not-an-address".to_string(),
            },
            strategy: None,
            require_tls: None,
            rate_limit_override: None,
            health_check: None,
        };
        assert!(create_rule(bad_address).is_err());
        let bad_regex = CreateRuleRequest {
            name: "bad".to_string(),
            priority: None,
            host: None,
            path_prefix: None,
            path_regex: Some("([invalid".to_string()),
            headers: None,
            upstream: UpstreamRef::Address {
                address: "127.0.0.1:8080".to_string(),
            },
            strategy: None,
            require_tls: None,
            rate_limit_override: None,
            health_check: None,
        };
        assert!(create_rule(bad_regex).is_err());
    }
}
