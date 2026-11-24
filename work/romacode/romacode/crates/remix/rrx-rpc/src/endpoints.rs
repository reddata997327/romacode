//! Mainnet endpoint registry, health probing and automatic failover.
//!
//! "har link proper connect ho main chain sy" — every tool resolves to a real
//! Ethereum mainnet endpoint; if one is down the next one is used automatically.

use crate::client::{PResult, Provider};
use rrx_util::u256_from_str;
use serde_json::{json, Value};

/// Public Ethereum mainnet JSON-RPC endpoints (no API key required).
/// Order = preference. All of them serve plain `eth_*` over HTTPS.
pub const MAINNET_ENDPOINTS: &[&str] = &[
    "https://ethereum-rpc.publicnode.com",
    "https://eth.drpc.org",
    "https://rpc.flashbots.net",
    "https://eth.meowrpc.com",
    "https://rpc.mevblocker.io",
];

pub const MAINNET_CHAIN_ID: u64 = 1;

/// Result of probing one endpoint.
#[derive(Clone, Debug)]
pub struct EndpointStatus {
    pub url: String,
    pub ok: bool,
    pub chain_id: Option<u64>,
    pub block: Option<u64>,
    pub client: Option<String>,
    pub latency_ms: u128,
    pub error: Option<String>,
}

/// Probe a single endpoint: chain id + latest block + client version + latency.
pub fn probe(url: &str) -> EndpointStatus {
    let started = std::time::Instant::now();
    let p = Provider::new(url);
    let mut st = EndpointStatus {
        url: url.to_string(),
        ok: false,
        chain_id: None,
        block: None,
        client: None,
        latency_ms: 0,
        error: None,
    };
    match p.chain_id() {
        Ok(id) => st.chain_id = Some(id),
        Err(e) => {
            st.error = Some(e);
            st.latency_ms = started.elapsed().as_millis();
            return st;
        }
    }
    st.block = p.block_number().ok();
    st.client = p.client_version().ok();
    st.ok = st.chain_id == Some(MAINNET_CHAIN_ID) && st.block.is_some();
    if !st.ok && st.error.is_none() {
        st.error = Some(format!("chain id {:?} != 1", st.chain_id));
    }
    st.latency_ms = started.elapsed().as_millis();
    st
}

/// Probe every built-in mainnet endpoint.
pub fn probe_all() -> Vec<EndpointStatus> {
    MAINNET_ENDPOINTS.iter().map(|u| probe(u)).collect()
}

/// Pick the first healthy mainnet endpoint (chain id == 1, latest block known).
pub fn auto_mainnet() -> Result<Provider, String> {
    let mut tried = Vec::new();
    for url in MAINNET_ENDPOINTS {
        let st = probe(url);
        if st.ok {
            return Ok(Provider::new(url));
        }
        tried.push(format!("  {} → {}", url, st.error.unwrap_or_else(|| "unreachable".into())));
    }
    Err(format!(
        "no mainnet endpoint reachable — check your internet connection. tried:\n{}",
        tried.join("\n")
    ))
}

/// Resolve the RPC url to use: explicit --rpc > RRX_RPC env > auto mainnet.
pub fn resolve_endpoint(rpc: Option<&str>) -> Result<Provider, String> {
    match rpc {
        Some(u) => Ok(Provider::new(u)),
        None => match std::env::var("RRX_RPC") {
            Ok(u) if !u.is_empty() => Ok(Provider::new(&u)),
            _ => auto_mainnet(),
        },
    }
}

/// A provider with automatic endpoint failover: on transport errors (or
/// `method not found`) the request is retried on the next endpoint.
pub struct MultiProvider {
    pub endpoints: Vec<String>,
    pub current: usize,
}

impl MultiProvider {
    pub fn mainnet() -> MultiProvider {
        MultiProvider {
            endpoints: MAINNET_ENDPOINTS.iter().map(|s| s.to_string()).collect(),
            current: 0,
        }
    }

    pub fn provider(&self) -> Provider {
        Provider::new(&self.endpoints[self.current])
    }

    pub fn url(&self) -> &str {
        &self.endpoints[self.current]
    }

    /// Raw request with cross-endpoint retry.
    pub fn raw_request(&mut self, method: &str, params: Value) -> PResult<Value> {
        let n = self.endpoints.len();
        let mut last_err = String::new();
        for attempt in 0..n {
            let idx = (self.current + attempt) % n;
            let p = Provider::new(&self.endpoints[idx]);
            match p.raw_request(method, params.clone()) {
                Ok(v) => {
                    self.current = idx;
                    return Ok(v);
                }
                Err(e) => {
                    let rotate = e.contains("rpc request failed") || e.contains("method not found");
                    last_err = e;
                    if !rotate {
                        return Err(last_err);
                    }
                }
            }
        }
        Err(format!("all {} endpoints failed; last error: {last_err}", n))
    }

    pub fn block_number(&mut self) -> PResult<u64> {
        let v = self.raw_request("eth_blockNumber", json!([]))?;
        Ok(u256_from_str(v.as_str().unwrap_or("0x0"))?.as_u64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_list_is_https() {
        for u in MAINNET_ENDPOINTS {
            assert!(u.starts_with("https://"), "{u} must be https");
        }
    }

    #[test]
    fn resolve_prefers_explicit() {
        let p = resolve_endpoint(Some("http://127.0.0.1:8545")).unwrap();
        assert_eq!(p.url, "http://127.0.0.1:8545");
    }
}
