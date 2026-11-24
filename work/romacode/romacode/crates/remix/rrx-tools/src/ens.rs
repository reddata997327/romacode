//! ENS (Ethereum Name Service) — real resolution against the mainnet registry.
//!
//! name → address via the canonical registry 0x0000…c2e074…, fully on-chain:
//! keccak namehash → ENS.resolver(node) → Resolver.addr(node)
//! plus reverse resolution (address → primary name).

use rrx_rpc::Provider;
use rrx_util::{abi, h160_from_hex, to_hex_prefixed, H160, H256};

pub const ENS_REGISTRY: &str = "0x00000000000C2E074eC69A0dFb2997BA6C7d2e1e";

/// ENS namehash (ENSIP-1 / EIP-137): node = keccak(node(parent) ‖ keccak(label))
pub fn namehash(name: &str) -> [u8; 32] {
    let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
    if name.is_empty() {
        return [0u8; 32];
    }
    let mut hash = [0u8; 32];
    for label in name.rsplit('.') {
        let label_hash = rrx_util::keccak256(label.as_bytes());
        let mut buf = [0u8; 64];
        buf[..32].copy_from_slice(&hash);
        buf[32..].copy_from_slice(&label_hash);
        hash = rrx_util::keccak256(&buf);
    }
    hash
}

fn eth_call_u32(p: &Provider, to: &H160, selector: [u8; 4], arg: &[u8; 32]) -> Result<Vec<u8>, String> {
    let mut data = selector.to_vec();
    data.extend_from_slice(arg);
    p.call(None, to, &data, "latest")
}

/// resolve an ENS name to its address (mainnet, on-chain)
pub fn resolve(p: &Provider, name: &str) -> Result<H160, String> {
    let node = namehash(name);
    let registry = h160_from_hex(ENS_REGISTRY)?;
    let raw = eth_call_u32(p, &registry, [0x01, 0x78, 0xb8, 0xbf], &node)?; // resolver(bytes32)
    let vals = abi::decode(&["address"], &raw).map_err(|e| format!("registry decode: {e}"))?;
    let resolver = match vals.first() {
        Some(abi::AbiValue::Address(a)) => *a,
        _ => return Err("ENS: malformed resolver response".into()),
    };
    if resolver == H160::zero() {
        return Err(format!("ENS: no resolver set for '{name}'"));
    }
    let raw = eth_call_u32(p, &resolver, [0x3b, 0x3b, 0x57, 0xde], &node)?; // addr(bytes32)
    let vals = abi::decode(&["address"], &raw).map_err(|e| format!("resolver decode: {e}"))?;
    match vals.first() {
        Some(abi::AbiValue::Address(a)) if *a != H160::zero() => Ok(*a),
        _ => Err(format!("ENS: '{name}' resolves to zero (unset?)")),
    }
}

/// reverse-resolve an address to its primary ENS name
pub fn reverse(p: &Provider, addr: &H160) -> Result<String, String> {
    let label = format!("{:x}", addr); // hex without 0x, lowercase
    let node = namehash(&format!("{label}.addr.reverse"));
    let registry = h160_from_hex(ENS_REGISTRY)?;
    let raw = eth_call_u32(p, &registry, [0x01, 0x78, 0xb8, 0xbf], &node)?;
    let vals = abi::decode(&["address"], &raw).map_err(|e| format!("registry decode: {e}"))?;
    let resolver = match vals.first() {
        Some(abi::AbiValue::Address(a)) => *a,
        _ => return Err("ENS: malformed resolver response".into()),
    };
    if resolver == H160::zero() {
        return Err(format!("ENS: no primary name set for 0x{addr:x}"));
    }
    let raw = eth_call_u32(p, &resolver, [0x69, 0x1f, 0x34, 0x31], &node)?; // name(bytes32)
    let vals = abi::decode(&["string"], &raw).map_err(|e| format!("resolver decode: {e}"))?;
    match vals.first() {
        Some(abi::AbiValue::Str(s)) if !s.is_empty() => Ok(s.clone()),
        _ => Err(format!("ENS: empty primary name for 0x{addr:x}")),
    }
}

/// resolve-or-passthrough: real addresses pass, names resolve
pub fn resolve_or_address(p: &Provider, input: &str) -> Result<H160, String> {
    if input.starts_with("0x") {
        return h160_from_hex(input);
    }
    resolve(p, input)
}

pub fn report(p: &Provider, name: &str) -> Result<String, String> {
    let mut out = String::from(&format!("ens {name}\n"));
    let node = H256::from(namehash(name));
    out.push_str(&format!("  node       {:#x}\n", node));
    let addr = resolve(p, name)?;
    out.push_str(&format!("  address    0x{addr:x}\n"));
    // best-effort reverse check
    if let Ok(primary) = reverse(p, &addr) {
        out.push_str(&format!("  primary    {primary}\n"));
    }
    Ok(out)
}

pub fn reverse_report(p: &Provider, addr: &H160) -> Result<String, String> {
    let name = reverse(p, addr)?;
    Ok(format!("reverse 0x{addr:x}\n  primary    {name}\n"))
}

/// helper used by tests of the offline part
pub fn node_hex(name: &str) -> String {
    to_hex_prefixed(&namehash(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namehash_vectors() {
        // documented ENSIP-1 vectors
        assert_eq!(namehash(""), [0u8; 32]);
        assert_eq!(
            node_hex("eth"),
            "0x93cdeb708b7545dc668eb9280176169d1c33cfd8ed6f04690a0bcc88a93fc4ae"
        );
        // vitalik.eth — the most famous node on mainnet
        assert_eq!(
            node_hex("vitalik.eth"),
            "0xee6c4522aab0003e8d14cd40a6af439055fd2577951148c14b6cea9a53475835"
        );
        // case/dot normalization
        assert_eq!(namehash("VITALIK.ETH"), namehash("vitalik.eth"));
        assert_eq!(namehash("vitalik.eth."), namehash("vitalik.eth"));
    }
}
