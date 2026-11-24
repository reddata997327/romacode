//! Token inspector: ERC-20 / ERC-721 / ERC-1155 / ERC-165 detection and
//! live reads (name, symbol, decimals, supply, balances) via real eth_call.

use rrx_rpc::Provider;
use rrx_util::{abi, h160_from_hex, H160, U256};

/// encode a call, eth_call it, return the raw output bytes
fn call_raw(p: &Provider, to: &H160, sig: &str, in_types: &[&str], args: &[&str]) -> Result<Vec<u8>, String> {
    let data = crate::deploy::encode_call(sig, in_types, args)?;
    let raw = p.call(None, to, &data, "latest")?;
    if raw.is_empty() {
        return Err("empty return".into());
    }
    Ok(raw)
}

fn call_string(p: &Provider, to: &H160, sig: &str) -> Option<String> {
    call_raw(p, to, sig, &[], &[])
        .ok()
        .and_then(|raw| abi::decode(&["string"], &raw).ok())
        .and_then(|v| match v.first() {
            Some(abi::AbiValue::Str(s)) => Some(s.clone()),
            _ => None,
        })
}

fn call_u256(p: &Provider, to: &H160, sig: &str, in_types: &[&str], args: &[&str]) -> Option<U256> {
    call_raw(p, to, sig, in_types, args)
        .ok()
        .and_then(|raw| abi::decode(&["uint256"], &raw).ok())
        .and_then(|v| match v.first() {
            Some(abi::AbiValue::Uint(u)) => Some(*u),
            _ => None,
        })
}

/// ERC-165 supportsInterface
pub fn supports_interface(p: &Provider, to: &H160, iface: [u8; 4]) -> Result<bool, String> {
    let raw = call_raw(p, to, "supportsInterface", &["bytes4"], &[&format!("0x{}", rrx_util::to_hex(&iface))])?;
    let vals = abi::decode(&["bool"], &raw).map_err(|e| e.to_string())?;
    Ok(match vals.first() {
        Some(abi::AbiValue::Bool(b)) => *b,
        _ => false,
    })
}

/// well-known interface ids
pub const IFACE_ERC165: [u8; 4] = [0x01, 0xff, 0xc9, 0xa7];
pub const IFACE_ERC20: [u8; 4] = [0x36, 0x37, 0x2b, 0x07];
pub const IFACE_ERC721: [u8; 4] = [0x80, 0xac, 0x58, 0xcd];
pub const IFACE_ERC1155: [u8; 4] = [0xd9, 0xb6, 0x7a, 0x26];
pub const IFACE_ERC721_META: [u8; 4] = [0x5b, 0x5e, 0x13, 0x9f];

pub fn report(p: &Provider, addr_str: &str, holder_str: Option<&str>) -> Result<String, String> {
    let to = h160_from_hex(addr_str)?;
    let mut out = String::from(&format!("token {to}\n"));

    // interface sniffing (real supportsInterface calls when the contract answers)
    let iface = |id: [u8; 4]| supports_interface(p, &to, id).unwrap_or(false);
    let (is721, is1155) = (iface(IFACE_ERC721), iface(IFACE_ERC1155));
    let is20ish = call_u256(p, &to, "totalSupply", &[], &[]).is_some();

    let kind = if is721 {
        if iface(IFACE_ERC721_META) { "ERC-721 (metadata)" } else { "ERC-721" }
    } else if is1155 {
        "ERC-1155"
    } else if is20ish {
        "ERC-20"
    } else {
        "unknown (no totalSupply / no ERC-165)"
    };
    out.push_str(&format!("  standard   {kind}\n"));

    if let Some(n) = call_string(p, &to, "name") {
        out.push_str(&format!("  name       {n}\n"));
    }
    if let Some(s) = call_string(p, &to, "symbol") {
        out.push_str(&format!("  symbol     {s}\n"));
    }
    if let Some(d) = call_u256(p, &to, "decimals", &[], &[]) {
        out.push_str(&format!("  decimals   {}\n", d.as_u64()));
    }
    if let Some(sup) = call_u256(p, &to, "totalSupply", &[], &[]) {
        let dec = call_u256(p, &to, "decimals", &[], &[]).unwrap_or(U256::zero()).as_u64().min(18) as u32;
        out.push_str(&format!("  supply     {}  (raw {})\n", crate::utils::render_scaled(&sup, dec), sup));
    }

    if let Some(h) = holder_str {
        if is20ish {
            let holder = resolve_holder(p, h)?;
            let bal = call_u256(p, &to, "balanceOf", &["address"], &[&format!("0x{:x}", holder)])
                .ok_or("no balanceOf result")?;
            let dec = call_u256(p, &to, "decimals", &[], &[]).unwrap_or(U256::from(18u8)).as_u64().min(18) as u32;
            out.push_str(&format!("  balance    0x{holder:x}  = {}\n", crate::utils::render_scaled(&bal, dec)));
        } else if is721 {
            // ownerOf(tokenId) — holder field carries the numeric id
            let id = holder_str
                .and_then(|s| rrx_util::u256_from_str(s.trim_start_matches('#').trim_start_matches("0x")).ok())
                .ok_or("ERC-721 owner check needs a numeric token id: --holder '#1234'")?;
            let raw = call_raw(p, &to, "ownerOf", &["uint256"], &[&format!("{id}")])?;
            let vals = abi::decode(&["address"], &raw).map_err(|e| e.to_string())?;
            out.push_str(&format!("  ownerOf #{id}  {}\n", vals.first().map(crate::deploy::format_value).unwrap_or_default()));
        } else if is1155 {
            out.push_str("  balance    ERC-1155 balanceOf(address,uint256) needs an id — use rrx ethcall\n");
        }
    }
    Ok(out)
}

/// accept an address, an ENS name, or `addr#tokenId` / `#tokenId` forms
fn resolve_holder(p: &Provider, h: &str) -> Result<H160, String> {
    let base = h.split('#').next().unwrap_or(h);
    if base.starts_with("0x") {
        h160_from_hex(base)
    } else {
        crate::ens::resolve(p, base)
    }
}
