// load_balancer -- Unit tests (pure logic only).

use chrono::Utc;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use super::selection::*;
use super::store::*;
use super::types::*;
use super::validation::*;

// ---------------------------------------------------------------------------
// Unit tests (pure logic only; no network, no database).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_backend(id: u128, weight: u32) -> Backend {
        let now = Utc::now();
        Backend {
            id: Uuid::from_u128(id),
            balancer_id: Uuid::from_u128(99),
            name: format!("backend-{id}"),
            source_kind: BackendSourceKind::Address,
            source_id: None,
            address: "127.0.0.1:8080".to_string(),
            weight,
            enabled: true,
            draining: false,
            health: HealthState::Healthy,
            last_check_at: None,
            last_latency_ms: None,
            consecutive_failures: 0,
            consecutive_successes: 0,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn round_robin_cycles_in_order() {
        let backends = vec![test_backend(1, 1), test_backend(2, 1), test_backend(3, 1)];
        let connections = HashMap::new();
        let picked: Vec<Uuid> = (0..6)
            .map(|counter| {
                select_backend(&backends, Strategy::RoundRobin, None, counter, &connections)
                    .expect("a backend is picked")
            })
            .collect();
        assert_eq!(
            picked,
            vec![
                backends[0].id,
                backends[1].id,
                backends[2].id,
                backends[0].id,
                backends[1].id,
                backends[2].id,
            ]
        );
    }

    #[test]
    fn weighted_selection_follows_weights() {
        let backends = vec![test_backend(1, 3), test_backend(2, 1)];
        let connections = HashMap::new();
        let picked: Vec<Uuid> = (0..8)
            .map(|counter| {
                select_backend(&backends, Strategy::Weighted, None, counter, &connections)
                    .expect("a backend is picked")
            })
            .collect();
        assert_eq!(
            picked,
            vec![
                backends[0].id,
                backends[0].id,
                backends[0].id,
                backends[1].id,
                backends[0].id,
                backends[0].id,
                backends[0].id,
                backends[1].id,
            ]
        );
    }

    #[test]
    fn least_connections_picks_lightest_backend() {
        let backends = vec![test_backend(1, 1), test_backend(2, 1), test_backend(3, 1)];
        let mut connections = HashMap::new();
        connections.insert(backends[0].id, 5);
        connections.insert(backends[1].id, 1);
        connections.insert(backends[2].id, 3);
        let picked = select_backend(&backends, Strategy::LeastConnections, None, 0, &connections);
        assert_eq!(picked, Some(backends[1].id));
    }

    #[test]
    fn least_connections_accounts_for_weight() {
        let backends = vec![test_backend(1, 1), test_backend(2, 4)];
        let mut connections = HashMap::new();
        connections.insert(backends[0].id, 1);
        connections.insert(backends[1].id, 2);
        // Load ratios: 1/1 = 1.0 vs 2/4 = 0.5, so the heavier backend wins.
        let picked = select_backend(&backends, Strategy::LeastConnections, None, 0, &connections);
        assert_eq!(picked, Some(backends[1].id));
    }

    #[test]
    fn ip_hash_is_stable_and_spreads() {
        let backends = vec![test_backend(1, 1), test_backend(2, 1), test_backend(3, 1)];
        let connections = HashMap::new();
        let first = select_backend(
            &backends,
            Strategy::IpHash,
            Some("203.0.113.7"),
            0,
            &connections,
        );
        let second = select_backend(
            &backends,
            Strategy::IpHash,
            Some("203.0.113.7"),
            42,
            &connections,
        );
        assert_eq!(first, second);
        let mut seen = HashSet::new();
        for octet in 1..=50 {
            let picked = select_backend(
                &backends,
                Strategy::IpHash,
                Some(&format!("203.0.113.{octet}")),
                0,
                &connections,
            );
            seen.insert(picked);
        }
        assert!(
            seen.len() > 1,
            "IP hash should spread clients across backends"
        );
    }

    #[test]
    fn ip_hash_virtual_nodes_spread_traffic_evenly() {
        let backends = vec![test_backend(1, 1), test_backend(2, 1), test_backend(3, 1)];
        let connections = HashMap::new();
        let mut counts = [0u32; 3];
        for octet_a in 1..=20 {
            for octet_b in 1..=50 {
                let picked = select_backend(
                    &backends,
                    Strategy::IpHash,
                    Some(&format!("203.0.{octet_a}.{octet_b}")),
                    0,
                    &connections,
                )
                .expect("a backend is picked");
                let index = backends.iter().position(|b| b.id == picked).unwrap();
                counts[index] += 1;
            }
        }
        // 1000 clients over 3 backends: with 128 virtual nodes each,
        // every backend should hold a roughly fair share (well
        // inside 15%..55%). A single ring point per backend can
        // leave one backend with almost no traffic.
        for count in counts {
            assert!(
                (150..=550).contains(&count),
                "IP hash share out of bounds: {counts:?}"
            );
        }
    }

    #[test]
    fn ip_hash_removal_moves_only_neighbours() {
        let backends = vec![test_backend(1, 1), test_backend(2, 1), test_backend(3, 1)];
        let connections = HashMap::new();
        let mut moved = 0u32;
        let total = 300u32;
        for n in 0..total {
            let ip = format!("198.51.100.{}", n % 250 + 1);
            let before = select_backend(&backends, Strategy::IpHash, Some(&ip), 0, &connections);
            let after = select_backend(
                &backends[..2],
                Strategy::IpHash,
                Some(&ip),
                0,
                &connections,
            );
            if before != after {
                moved += 1;
            }
        }
        // Removing one of three backends should move roughly the
        // removed backend's share (~1/3), not most of the traffic.
        assert!(
            moved < total / 2,
            "removing a backend moved {moved}/{total} clients"
        );
    }

    #[test]
    fn active_connections_open_and_close_in_balance() {
        let backend_id = Uuid::new_v4();
        assert_eq!(active_connections(backend_id), 0);
        note_backend_connection_open(backend_id);
        note_backend_connection_open(backend_id);
        assert_eq!(active_connections(backend_id), 2);
        note_backend_connection_closed(backend_id);
        assert_eq!(active_connections(backend_id), 1);
        note_backend_connection_closed(backend_id);
        assert_eq!(active_connections(backend_id), 0);
        // Closing with nothing open saturates at zero, never underflows.
        note_backend_connection_closed(backend_id);
        assert_eq!(active_connections(backend_id), 0);
        connections_store().remove(&backend_id);
    }

    #[test]
    fn routed_totals_are_tracked_separately_from_active_connections() {
        let backend_id = Uuid::new_v4();
        assert_eq!(active_connections(backend_id), 0);
        assert_eq!(routed_requests(backend_id), 0);
        mutate_backend_stats(backend_id, |stats| {
            stats.requests_routed = stats.requests_routed.saturating_add(5);
        });
        // Routing requests grows the cumulative total only; the live
        // connection count is untouched (only the real proxy path
        // moves it, with a matching close).
        assert_eq!(routed_requests(backend_id), 5);
        assert_eq!(active_connections(backend_id), 0);
        stats_store().remove(&backend_id);
        connections_store().remove(&backend_id);
    }

    #[test]
    fn refresh_balancer_status_recovers_from_degraded_to_active() {
        let balancer_id = Uuid::new_v4();
        let now = Utc::now();
        let mut backend = test_backend(1, 1);
        backend.balancer_id = balancer_id;
        backend.health = HealthState::Healthy;
        let backend_id = backend.id;
        backends_store().insert(backend_id, backend);
        balancers_store().insert(
            balancer_id,
            LoadBalancer {
                id: balancer_id,
                name: "recovering".to_string(),
                listen_address: "127.0.0.1".to_string(),
                listen_port: 8080,
                tls_mode: TlsMode::Off,
                domain: None,
                certificate_name: None,
                strategy: Strategy::RoundRobin,
                health_check: HealthCheckConfig::default(),
                enabled: true,
                status: BalancerStatus::Degraded,
                created_at: now,
                updated_at: now,
            },
        );
        // A healthy, eligible backend exists, so the refresh must
        // move the balancer Degraded -> Active (the transition on
        // which `lb.recovered` is emitted).
        refresh_balancer_status(balancer_id);
        assert_eq!(
            balancers_store().get(&balancer_id).unwrap().status,
            BalancerStatus::Active
        );
        backends_store().remove(&backend_id);
        balancers_store().remove(&balancer_id);
    }

    #[test]
    fn empty_backend_list_selects_nothing() {
        let connections = HashMap::new();
        assert_eq!(
            select_backend(&[], Strategy::RoundRobin, None, 0, &connections),
            None
        );
    }

    #[test]
    fn host_port_parsing() {
        assert_eq!(
            parse_host_port("example.com:8080"),
            Some(("example.com".to_string(), 8080))
        );
        assert_eq!(
            parse_host_port("[::1]:8443"),
            Some(("::1".to_string(), 8443))
        );
        assert_eq!(parse_host_port("example.com:0"), None);
        assert_eq!(parse_host_port("example.com"), None);
        assert_eq!(parse_host_port(":8080"), None);
        assert_eq!(parse_host_port("example.com:notaport"), None);
    }

    #[test]
    fn health_check_validation_rejects_bad_timeout() {
        let mut config = HealthCheckConfig::default();
        config.timeout_secs = 30;
        config.interval_secs = 10;
        assert!(validate_health_check(&config).is_err());
        let mut config = HealthCheckConfig::default();
        config.path = "health".to_string();
        assert!(validate_health_check(&config).is_err());
    }

    #[test]
    fn stats_average_and_uptime() {
        let stats = BackendStats {
            requests_routed: 4,
            errors: 0,
            total_latency_ms: 400,
            latency_samples: 4,
            checks_total: 10,
            checks_ok: 9,
            ..BackendStats::default()
        };
        assert_eq!(stats.average_latency_ms(), Some(100.0));
        assert_eq!(stats.uptime_ratio(), Some(0.9));
        assert_eq!(BackendStats::default().uptime_ratio(), None);
    }
}
