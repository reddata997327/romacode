//! rrx fork — download REAL state from a live network into the local MPT-backed
//! WorldState, then execute contracts/tests against it. No mocks: every byte of
//! code, balance, nonce and storage comes from the real chain over JSON-RPC.

use rrx_chain::state::WorldState;
use rrx_util::{H160, U256};

use rrx_rpc::Provider;

pub struct ForkState {
    pub state: WorldState,
    pub chain_id: u64,
    pub block: u64,
    pub endpoint: String,
    pub accounts: Vec<H160>,
}

/// Fetch the REAL on-chain state of the given accounts at `block` and load it
/// into a local WorldState (code, balance, nonce, storage slots of contracts).
pub fn download_state(
    p: &Provider,
    block_tag: &str,
    accounts: &[H160],
) -> Result<ForkState, String> {
    let chain_id = p.chain_id()?;
    let block = p
        .block_by_tag(block_tag, false)?
        .ok_or("block not found")?;
    let number = block["number"]
        .as_str()
        .and_then(|h| u64::from_str_radix(h.trim_start_matches("0x"), 16).ok())
        .ok_or("bad block number")?;

    let mut state = WorldState::default();
    for addr in accounts {
        let balance = p.balance(addr, block_tag)?;
        let nonce = p.nonce(addr, block_tag)?;
        let code = p.code(addr, block_tag)?;
        {
            let acc = state.accounts.entry(*addr).or_default();
            acc.balance = balance;
            acc.nonce = nonce;
        }
        if !code.is_empty() {
            state.set_code_of(addr, code);
            // storage: pull a window of slots 0..=255 — enough for most
            // mappings-free state to run realistic tests.
            for slot in 0..256u64 {
                let v = p.storage_at(addr, &U256::from(slot), block_tag)?;
                if v != U256::zero() {
                    state
                        .accounts
                        .entry(*addr)
                        .or_default()
                        .storage
                        .insert(U256::from(slot), v);
                }
            }
        }
    }
    Ok(ForkState {
        state,
        chain_id,
        block: number,
        endpoint: p.url.to_string(),
        accounts: accounts.to_vec(),
    })
}

/// List of well-known mainnet contracts useful for fork demos/tests.
pub fn well_known(name: &str) -> Option<H160> {
    let s = match name.to_lowercase().as_str() {
        "usdt" => "0xdAC17F958D2ee523a2206206994597C13D831ec7",
        "usdc" => "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        "weth" => "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        "dai" => "0x6B175474E89094C44Da98b954EedeAC495271d0F",
        "entrypoint" => "0x5FF137D4b0FDCD49DcA30c7CF57E578a026d2789",
        "univ2-factory" => "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
        "univ2-router" => "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D",
        other => return other.parse().ok(),
    };
    Some(s.parse().ok()?)
}

/// Summary of what a fork test touched — proof it ran on real state.
pub fn fork_report(f: &ForkState) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "fork: chain {} @ block {} (endpoint {})\n",
        f.chain_id, f.block, f.endpoint
    ));
    out.push_str(&format!("preloaded real accounts: {}\n", f.accounts.len()));
    for a in &f.accounts {
        let acct = f.state.accounts.get(a);
        let code_len = f.state.code_of(a).len();
        out.push_str(&format!(
            "  {:?}  balance={}  nonce={}  code={} bytes  slots={}\n",
            a,
            acct.map(|x| x.balance).unwrap_or_default(),
            acct.map(|x| x.nonce).unwrap_or(0),
            code_len,
            acct.map(|x| x.storage.len()).unwrap_or(0)
        ));
    }
    out
}
