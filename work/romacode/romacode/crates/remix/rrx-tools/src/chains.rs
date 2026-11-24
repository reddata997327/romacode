//! rrx multi-chain registry — real public endpoints for every supported chain.
//! Every entry is a REAL network (no simulation); probes hit live RPCs.

use std::time::Instant;

use serde_json::json;

use rrx_rpc::Provider;

pub struct ChainInfo {
    pub slug: &'static str,
    pub name: &'static str,
    pub chain_id: u64,
    /// public RPC endpoints in failover order
    pub rpcs: &'static [&'static str],
    pub explorer: &'static str,
    pub testnet: bool,
}

pub const CHAINS: &[ChainInfo] = &[
    ChainInfo {
        slug: "ethereum",
        name: "Ethereum Mainnet",
        chain_id: 1,
        rpcs: &[
            "https://ethereum-rpc.publicnode.com",
            "https://eth.drpc.org",
            "https://rpc.flashbots.net",
            "https://rpc.mevblocker.io",
        ],
        explorer: "https://etherscan.io",
        testnet: false,
    },
    ChainInfo {
        slug: "base",
        name: "Base",
        chain_id: 8453,
        rpcs: &["https://base-rpc.publicnode.com", "https://mainnet.base.org"],
        explorer: "https://basescan.org",
        testnet: false,
    },
    ChainInfo {
        slug: "arbitrum",
        name: "Arbitrum One",
        chain_id: 42161,
        rpcs: &["https://arbitrum-one-rpc.publicnode.com", "https://arb1.arbitrum.io/rpc"],
        explorer: "https://arbiscan.io",
        testnet: false,
    },
    ChainInfo {
        slug: "optimism",
        name: "OP Mainnet",
        chain_id: 10,
        rpcs: &["https://optimism-rpc.publicnode.com", "https://mainnet.optimism.io"],
        explorer: "https://optimistic.etherscan.io",
        testnet: false,
    },
    ChainInfo {
        slug: "polygon",
        name: "Polygon PoS",
        chain_id: 137,
        rpcs: &["https://polygon-bor-rpc.publicnode.com", "https://polygon-rpc.com"],
        explorer: "https://polygonscan.com",
        testnet: false,
    },
    ChainInfo {
        slug: "bsc",
        name: "BNB Smart Chain",
        chain_id: 56,
        rpcs: &["https://bsc-rpc.publicnode.com", "https://bsc-dataseed.binance.org"],
        explorer: "https://bscscan.com",
        testnet: false,
    },
    ChainInfo {
        slug: "avalanche",
        name: "Avalanche C-Chain",
        chain_id: 43114,
        rpcs: &["https://avalanche-c-chain-rpc.publicnode.com", "https://api.avax.network/ext/bc/C/rpc"],
        explorer: "https://snowtrace.io",
        testnet: false,
    },
    ChainInfo {
        slug: "gnosis",
        name: "Gnosis Chain",
        chain_id: 100,
        rpcs: &["https://gnosis-rpc.publicnode.com", "https://rpc.gnosischain.com"],
        explorer: "https://gnosisscan.io",
        testnet: false,
    },
    ChainInfo {
        slug: "sepolia",
        name: "Ethereum Sepolia",
        chain_id: 11155111,
        rpcs: &["https://ethereum-sepolia-rpc.publicnode.com", "https://rpc.sepolia.org"],
        explorer: "https://sepolia.etherscan.io",
        testnet: true,
    },
    ChainInfo {
        slug: "base-sepolia",
        name: "Base Sepolia",
        chain_id: 84532,
        rpcs: &["https://base-sepolia-rpc.publicnode.com"],
        explorer: "https://sepolia.basescan.org",
        testnet: true,
    },
    ChainInfo {
        slug: "arbitrum-sepolia",
        name: "Arbitrum Sepolia",
        chain_id: 421614,
        rpcs: &["https://arbitrum-sepolia-rpc.publicnode.com"],
        explorer: "https://sepolia.arbiscan.io",
        testnet: true,
    },
    ChainInfo {
        slug: "optimism-sepolia",
        name: "OP Sepolia",
        chain_id: 11155420,
        rpcs: &["https://optimism-sepolia-rpc.publicnode.com"],
        explorer: "https://sepolia-optimism.etherscan.io",
        testnet: true,
    },
    ChainInfo {
        slug: "holesky",
        name: "Ethereum Holesky",
        chain_id: 17000,
        rpcs: &["https://ethereum-holesky-rpc.publicnode.com"],
        explorer: "https://holesky.etherscan.io",
        testnet: true,
    },
];

pub fn find(slug_or_id: &str) -> Option<&'static ChainInfo> {
    let needle = slug_or_id.trim().to_lowercase();
    CHAINS
        .iter()
        .find(|c| c.slug == needle || c.chain_id.to_string() == needle || c.name.to_lowercase() == needle)
}

pub struct ChainStatus {
    pub chain: &'static ChainInfo,
    pub endpoint: Option<&'static str>,
    pub live_chain_id: Option<u64>,
    pub block: Option<u64>,
    pub latency_ms: u128,
    pub client: Option<String>,
    pub ok: bool,
    pub error: Option<String>,
}

/// Probe one chain: try endpoints in order until one answers, verifying chain id.
pub fn probe(info: &'static ChainInfo) -> ChainStatus {
    let mut best = ChainStatus {
        chain: info,
        endpoint: None,
        live_chain_id: None,
        block: None,
        latency_ms: 0,
        client: None,
        ok: false,
        error: None,
    };
    let mut last_err = String::new();
    for url in info.rpcs {
        let t0 = Instant::now();
        let p = Provider::new(url);
        match (|| -> Result<(u64, u64, String), String> {
            let cid = p.chain_id()?;
            if cid != info.chain_id {
                return Err(format!("endpoint serves chain {cid}, expected {}", info.chain_id));
            }
            let bn = p.block_number()?;
            let client = p.client_version().unwrap_or_else(|_| "unknown".into());
            Ok((cid, bn, client))
        })() {
            Ok((cid, bn, client)) => {
                best.endpoint = Some(url);
                best.live_chain_id = Some(cid);
                best.block = Some(bn);
                best.client = Some(client);
                best.latency_ms = t0.elapsed().as_millis();
                best.ok = true;
                return best;
            }
            Err(e) => {
                last_err = format!("{url}: {e}");
            }
        }
    }
    best.error = Some(last_err);
    best
}

/// Probe every chain in the registry; returns a printable report.
pub fn probe_all_matrix(filter: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{:<18} {:>7}  {:<4} {:>12}  {:>7}  {}\n",
        "chain", "chainId", "live", "block", "ms", "client"
    ));
    out.push_str(&"-".repeat(95));
    out.push('\n');
    for info in CHAINS {
        if let Some(f) = filter {
            if !info.slug.contains(&f.to_lowercase()) {
                continue;
            }
        }
        let st = probe(info);
        if st.ok {
            out.push_str(&format!(
                "{:<18} {:>7}  {:<4} {:>12}  {:>7}  {}\n",
                info.slug,
                info.chain_id,
                "UP",
                st.block.unwrap_or(0),
                st.latency_ms,
                st.client.as_deref().unwrap_or("-")
            ));
        } else {
            out.push_str(&format!(
                "{:<18} {:>7}  {:<4} {:>12}  {:>7}  {}\n",
                info.slug,
                info.chain_id,
                "DOWN",
                "-",
                "-",
                st.error.as_deref().unwrap_or("unreachable")
            ));
        }
    }
    out.push_str(&format!("\nexplorers & failover lists: rrx chains --json\n"));
    out
}

/// JSON description of the whole registry (for MCP / scripts).
pub fn registry_json() -> serde_json::Value {
    json!(CHAINS
        .iter()
        .map(|c| json!({
            "slug": c.slug,
            "name": c.name,
            "chain_id": c.chain_id,
            "testnet": c.testnet,
            "explorer": c.explorer,
            "rpcs": c.rpcs,
        }))
        .collect::<Vec<_>>())
}
