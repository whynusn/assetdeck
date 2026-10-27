//! Shared production endpoint setup and bounded identity-based dialing.

use std::net::SocketAddr;
use std::time::Duration;

use futures_util::StreamExt;
use iroh::address_lookup::{AddrFilter, DnsAddressLookup, PkarrPublisher, PkarrResolver};
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey, TransportAddr};

use crate::engine::{EngineError, CONNECT_TIMEOUT};

const DIRECT_CONNECT_BUDGET: Duration = Duration::from_secs(3);
const RESOLUTION_BUDGET: Duration = Duration::from_secs(5);
const FINAL_CONNECT_BUDGET: Duration = Duration::from_secs(12);

/// Build the signed pkarr + dual resolver + public relay endpoint used in production.
/// Tests can disable IP transports while keeping every other production setting identical.
pub async fn production_endpoint(
    secret_key: SecretKey,
    disable_ip_transports: bool,
) -> Result<Endpoint, iroh::endpoint::BindError> {
    let relay_url: url::Url = iroh::address_lookup::N0_DNS_PKARR_RELAY_PROD
        .parse()
        .expect("n0 pkarr relay URL is valid");
    let mut builder = Endpoint::builder(presets::Minimal)
        .secret_key(secret_key)
        .alpns(vec![
            share::ALPN.as_bytes().to_vec(),
            share::ALPN_SYNC.as_bytes().to_vec(),
        ])
        .address_lookup(PkarrPublisher::builder(relay_url).addr_filter(AddrFilter::unfiltered()))
        .address_lookup(PkarrResolver::n0_dns())
        .address_lookup(DnsAddressLookup::n0_dns())
        .relay_mode(RelayMode::Default);
    if disable_ip_transports {
        builder = builder.clear_ip_transports();
    }
    builder.bind().await
}

/// Connect to the requested authenticated endpoint, combining supplied address hints
/// with all lookup results observed during the bounded resolution stage.
pub async fn connect(
    endpoint: &Endpoint,
    target: EndpointAddr,
    alpn: &[u8],
) -> Result<iroh::endpoint::Connection, EngineError> {
    let peer_id = target.id;
    let has_direct = target.ip_addrs().next().is_some();
    let started = tokio::time::Instant::now();

    if !has_direct {
        logging::info!(
            "拨号阶段=identity peer={} candidates=deferred-to-iroh",
            peer_id
        );
        match tokio::time::timeout(CONNECT_TIMEOUT, endpoint.connect(target.clone(), alpn)).await {
            Ok(Ok(connection)) => return verify_peer(connection, peer_id),
            Err(_) => return Err(EngineError::Timeout("连接")),
            Ok(Err(error)) => {
                logging::warn!("拨号阶段=initial-discovery-failed peer={peer_id} error={error}");
            }
        }
        // 冷启动的首次查询可能早于签名地址发布。只在一次中继就绪事件后
        // 重查一次，整个会话仍受 CONNECT_TIMEOUT 约束，不按时间轮询。
        let remaining = CONNECT_TIMEOUT.saturating_sub(started.elapsed());
        let _ = tokio::time::timeout(remaining.min(RESOLUTION_BUDGET), endpoint.online()).await;
    } else {
        logging::info!(
            "拨号阶段=direct peer={} candidates={}",
            peer_id,
            target.ip_addrs().count()
        );
        if let Ok(Ok(connection)) = tokio::time::timeout(
            DIRECT_CONNECT_BUDGET,
            endpoint.connect(target.clone(), alpn),
        )
        .await
        {
            return verify_peer(connection, peer_id);
        }
    }

    let discovered = resolve_candidates(endpoint, peer_id).await;
    let merged = merge_candidates(peer_id, target, discovered);
    let elapsed = started.elapsed();
    let remaining = CONNECT_TIMEOUT.saturating_sub(elapsed);
    let budget = remaining.min(FINAL_CONNECT_BUDGET);
    logging::info!(
        "拨号阶段=resolved peer={} candidates={} budget_ms={}",
        peer_id,
        merged.ip_addrs().count() + merged.relay_urls().count(),
        budget.as_millis()
    );
    if budget.is_zero() {
        return Err(EngineError::Timeout("地址发现与连接"));
    }
    let connection = tokio::time::timeout(budget, endpoint.connect(merged, alpn))
        .await
        .map_err(|_| EngineError::Timeout("连接"))?
        .map_err(|error| {
            logging::warn!("拨号阶段=failed peer={} error={}", peer_id, error);
            EngineError::Dial(error.to_string())
        })?;
    verify_peer(connection, peer_id)
}

fn verify_peer(
    connection: iroh::endpoint::Connection,
    expected: EndpointId,
) -> Result<iroh::endpoint::Connection, EngineError> {
    let actual = connection.remote_id();
    if actual != expected {
        return Err(EngineError::Dial("对端身份校验失败".into()));
    }
    logging::info!("拨号阶段=connected peer={}", expected);
    Ok(connection)
}

async fn resolve_candidates(endpoint: &Endpoint, peer_id: EndpointId) -> Vec<TransportAddr> {
    let Ok(lookups) = endpoint.address_lookup() else {
        logging::warn!("拨号阶段=resolve-unavailable peer={peer_id}");
        return Vec::new();
    };
    let mut stream = lookups.resolve(peer_id);
    let mut candidates = Vec::new();
    let result = tokio::time::timeout(RESOLUTION_BUDGET, async {
        while let Some(item) = stream.next().await {
            let item = match item {
                Ok(Ok(item)) => item,
                Ok(Err(error)) => {
                    logging::warn!("拨号阶段=resolve-error peer={peer_id} error={error:#}");
                    continue;
                }
                Err(error) => {
                    logging::warn!("拨号阶段=resolve-task-error peer={peer_id} error={error}");
                    continue;
                }
            };
            if item.endpoint_id() != peer_id {
                continue;
            }
            let source = item.provenance();
            let addr = item.into_endpoint_addr();
            logging::info!(
                "拨号阶段=resolve-result peer={peer_id} source={source} ip_count={} relay_count={}",
                addr.ip_addrs().count(),
                addr.relay_urls().count()
            );
            candidates.extend(addr.ip_addrs().copied().map(TransportAddr::Ip));
            candidates.extend(addr.relay_urls().cloned().map(TransportAddr::Relay));
        }
    })
    .await;
    logging::info!(
        "拨号阶段=resolve peer={} candidates={} complete={}",
        peer_id,
        candidates.len(),
        result.is_ok()
    );
    candidates
}

fn merge_candidates(
    peer_id: EndpointId,
    original: EndpointAddr,
    discovered: Vec<TransportAddr>,
) -> EndpointAddr {
    let mut candidates: Vec<TransportAddr> = original
        .ip_addrs()
        .copied()
        .map(TransportAddr::Ip)
        .chain(original.relay_urls().cloned().map(TransportAddr::Relay))
        .collect();
    for candidate in discovered {
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    EndpointAddr::from_parts(peer_id, candidates)
}

pub fn socket_candidates(addrs: &[String]) -> Vec<SocketAddr> {
    addrs
        .iter()
        .filter_map(|addr| addr.parse::<SocketAddr>().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_preserves_stale_direct_and_late_relay_candidates() {
        let peer = SecretKey::generate().public();
        let stale = TransportAddr::Ip("240.0.0.1:12345".parse().unwrap());
        let relay: iroh::RelayUrl = "https://relay.example".parse().unwrap();
        let merged = merge_candidates(
            peer,
            EndpointAddr::new(peer).with_ip_addr("240.0.0.1:12345".parse().unwrap()),
            vec![TransportAddr::Relay(relay.clone())],
        );
        assert!(merged
            .ip_addrs()
            .any(|addr| *addr == "240.0.0.1:12345".parse().unwrap()));
        assert!(merged.relay_urls().any(|addr| addr == &relay));
        assert_eq!(merged.ip_addrs().count() + merged.relay_urls().count(), 2);
        assert_eq!(stale, TransportAddr::Ip("240.0.0.1:12345".parse().unwrap()));
    }

    #[test]
    fn socket_candidate_parse_discards_invalid_addresses() {
        assert_eq!(
            socket_candidates(&["bad".into(), "127.0.0.1:4321".into()]).len(),
            1
        );
    }
}
