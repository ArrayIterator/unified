// load_balancer -- Health probing and background sweeper.

use chrono::Utc;
use serde_json::json as json_value;
use std::sync::OnceLock;
use std::sync::atomic::Ordering as AtomicOrdering;
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::constants::*;
use super::hooks::*;
use super::selection::*;
use super::store::*;
use super::types::*;

// ---------------------------------------------------------------------------
// Health probing.
// ---------------------------------------------------------------------------

pub(crate) fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

pub struct ProbeOutcome {
    pub healthy: bool,
    pub latency_ms: u64,
}

pub(crate) async fn probe_tcp(address: &str, timeout: Duration) -> ProbeOutcome {
    let started = Instant::now();
    // Try every resolved address in turn: on a dual-stack host the
    // first address may be unreachable while another one answers.
    // The backend is unhealthy only when no address connects.
    let mut healthy = false;
    if let Ok(addrs) = tokio::net::lookup_host(address).await {
        for socket_addr in addrs {
            if tokio::time::timeout(timeout, tokio::net::TcpStream::connect(socket_addr))
                .await
                .is_ok_and(|result| result.is_ok())
            {
                healthy = true;
                break;
            }
        }
    }
    ProbeOutcome {
        healthy,
        latency_ms: started.elapsed().as_millis() as u64,
    }
}

/// Probe URL for an HTTP(S) health check. The scheme follows the
/// configured kind so an HTTPS-only backend is probed over TLS
/// instead of plain HTTP.
pub(crate) fn health_check_url(address: &str, check: &HealthCheckConfig) -> String {
    let scheme = match check.kind {
        HealthCheckKind::Https => "https",
        _ => "http",
    };
    format!("{scheme}://{address}{}", check.path)
}

pub(crate) async fn probe_http(address: &str, check: &HealthCheckConfig) -> ProbeOutcome {
    let started = Instant::now();
    let url = health_check_url(address, check);
    let timeout = Duration::from_secs(check.timeout_secs);
    let result = tokio::time::timeout(timeout, http_client().get(&url).send()).await;
    let healthy = match result {
        Ok(Ok(response)) => match check.expected_status {
            Some(expected) => response.status().as_u16() == expected,
            None => response.status().is_success(),
        },
        _ => false,
    };
    ProbeOutcome {
        healthy,
        latency_ms: started.elapsed().as_millis() as u64,
    }
}

/// Probe one backend with the balancer's health-check configuration.
pub async fn probe_backend(backend: &Backend, check: &HealthCheckConfig) -> ProbeOutcome {
    let timeout = Duration::from_secs(check.timeout_secs);
    match check.kind {
        HealthCheckKind::Tcp => probe_tcp(&backend.address, timeout).await,
        HealthCheckKind::Http | HealthCheckKind::Https => probe_http(&backend.address, check).await,
    }
}

/// Apply one probe result to a backend: consecutive counters, health
/// transitions (with thresholding), stats, and the matching hook
/// events. Called by the background sweeper and by the manual
/// check-now endpoints.
pub(crate) fn apply_probe_result(backend_id: Uuid, outcome: ProbeOutcome) {
    let Some(mut backend) = backends_store().get_mut(&backend_id) else {
        return;
    };
    let balancer_id = backend.balancer_id;
    let balancer = balancers_store()
        .get(&balancer_id)
        .map(|entry| entry.clone());
    let Some(balancer) = balancer else {
        return;
    };
    let check = balancer.health_check.clone();
    let previous_health = backend.health;
    backend.last_check_at = Some(Utc::now());
    backend.last_latency_ms = Some(outcome.latency_ms);
    if outcome.healthy {
        backend.consecutive_successes = backend.consecutive_successes.saturating_add(1);
        backend.consecutive_failures = 0;
        if backend.health == HealthState::Unhealthy
            && backend.consecutive_successes >= check.healthy_threshold
        {
            backend.health = HealthState::Healthy;
        } else if backend.health == HealthState::Unknown {
            backend.health = HealthState::Healthy;
        }
    } else {
        backend.consecutive_failures = backend.consecutive_failures.saturating_add(1);
        backend.consecutive_successes = 0;
        if backend.health != HealthState::Unhealthy
            && backend.consecutive_failures >= check.unhealthy_threshold
        {
            backend.health = HealthState::Unhealthy;
        }
    }
    backend.updated_at = Utc::now();
    let (new_health, name, address) = (
        backend.health,
        backend.name.clone(),
        backend.address.clone(),
    );
    drop(backend);
    mutate_backend_stats(backend_id, |stats| {
        stats.checks_total = stats.checks_total.saturating_add(1);
        if outcome.healthy {
            stats.checks_ok = stats.checks_ok.saturating_add(1);
        }
    });
    if new_health != previous_health {
        let payload = json_value!({
            "balancer_id": balancer_id,
            "backend_id": backend_id,
            "backend_name": name,
            "address": address,
            "health": format!("{new_health:?}").to_lowercase(),
            "latency_ms": outcome.latency_ms,
            "consecutive_failures": backends_store().get(&backend_id).map(|entry| entry.consecutive_failures).unwrap_or(0),
            "consecutive_successes": backends_store().get(&backend_id).map(|entry| entry.consecutive_successes).unwrap_or(0),
        });
        if new_health == HealthState::Unhealthy {
            emit(LB_BACKEND_UNHEALTHY, payload);
        } else if new_health == HealthState::Healthy {
            emit(LB_BACKEND_HEALTHY, payload);
        }
    }
    refresh_balancer_status(balancer_id);
}

// ---------------------------------------------------------------------------
// Background health sweeper.
// ---------------------------------------------------------------------------

pub(crate) async fn sweep_once() {
    let now = Utc::now();
    let due: Vec<(Backend, HealthCheckConfig)> = backends_store()
        .iter()
        .filter_map(|entry| {
            let backend = entry.clone();
            let balancer = balancers_store().get(&backend.balancer_id)?;
            if !balancer.enabled || !backend.enabled {
                return None;
            }
            let check = balancer.health_check.clone();
            let due = match backend.last_check_at {
                None => true,
                Some(last) => {
                    now.signed_duration_since(last).num_seconds() >= check.interval_secs as i64
                }
            };
            if due { Some((backend, check)) } else { None }
        })
        .collect();
    for (backend, check) in due {
        let outcome = probe_backend(&backend, &check).await;
        apply_probe_result(backend.id, outcome);
    }
}

pub(crate) fn start_health_sweeper() {
    if SWEEPER_STARTED.swap(true, AtomicOrdering::SeqCst) {
        return;
    }
    let started = std::thread::Builder::new()
        .name("lb-health-sweeper".to_string())
        .spawn(|| {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    log::error!("Load balancer health sweeper cannot start a runtime: {error}");
                    // Release the flag so a later start attempt can
                    // retry instead of being locked out forever.
                    reset_sweeper_started();
                    return;
                }
            };
            loop {
                std::thread::sleep(Duration::from_secs(SWEEP_TICK_SECS));
                runtime.block_on(sweep_once());
            }
        });
    if let Err(error) = started {
        log::error!("Load balancer health sweeper thread failed to start: {error}");
        reset_sweeper_started();
    }
}

/// Release the sweeper-started flag so a later
/// `start_health_sweeper` call can retry after a start failure.
fn reset_sweeper_started() {
    SWEEPER_STARTED.store(false, AtomicOrdering::SeqCst);
}

// Tests for the health probes and the sweeper flag in this file.
#[cfg(test)]
mod tests {
    use super::*;

    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime builds")
    }

    #[test]
    fn health_check_url_uses_https_for_https_kind() {
        let mut check = HealthCheckConfig::default();
        assert_eq!(
            health_check_url("10.0.0.1:8443", &check),
            "http://10.0.0.1:8443/health"
        );
        check.kind = HealthCheckKind::Https;
        assert_eq!(
            health_check_url("10.0.0.1:8443", &check),
            "https://10.0.0.1:8443/health"
        );
        check.kind = HealthCheckKind::Tcp;
        assert_eq!(
            health_check_url("10.0.0.1:8443", &check),
            "http://10.0.0.1:8443/health"
        );
    }

    #[test]
    fn https_kind_serializes_as_https() {
        assert_eq!(
            serde_json::to_string(&HealthCheckKind::Https).unwrap(),
            "\"https\""
        );
        assert_eq!(
            serde_json::from_str::<HealthCheckKind>("\"https\"").unwrap(),
            HealthCheckKind::Https
        );
    }

    #[test]
    fn probe_tcp_connects_to_a_local_listener() {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let port = listener.local_addr().expect("listener address").port();
        let runtime = test_runtime();
        let outcome = runtime
            .block_on(probe_tcp(&format!("127.0.0.1:{port}"), Duration::from_secs(2)));
        assert!(outcome.healthy);
        drop(listener);
        let outcome = runtime
            .block_on(probe_tcp(&format!("127.0.0.1:{port}"), Duration::from_secs(2)));
        assert!(!outcome.healthy);
    }

    #[test]
    fn probe_tcp_tries_every_resolved_address() {
        // `localhost` resolves to both ::1 and 127.0.0.1 on dual-stack
        // hosts; the listener only accepts on IPv4, so the probe must
        // keep trying past an unreachable first address.
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let port = listener.local_addr().expect("listener address").port();
        let runtime = test_runtime();
        let outcome = runtime
            .block_on(probe_tcp(&format!("localhost:{port}"), Duration::from_secs(2)));
        assert!(outcome.healthy);
    }

    #[test]
    fn sweeper_started_flag_can_be_released_for_retry() {
        SWEEPER_STARTED.store(true, AtomicOrdering::SeqCst);
        reset_sweeper_started();
        assert!(!SWEEPER_STARTED.load(AtomicOrdering::SeqCst));
    }
}
