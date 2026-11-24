//! Small utilities panel: keccak hashing, ether unit conversion,
//! ENS namehash and function selectors — the daily-driver tools.

use rrx_util::{keccak256, to_hex_prefixed, U256};

/// keccak256 of a hex string (0x-prefixed → raw bytes) or UTF-8 text
pub fn keccak_of(input: &str) -> String {
    let data = if let Some(hex) = input.strip_prefix("0x") {
        rrx_util::from_hex(hex).unwrap_or_else(|_| input.as_bytes().to_vec())
    } else {
        input.as_bytes().to_vec()
    };
    to_hex_prefixed(&keccak256(&data))
}

/// supported unit table: (name, wei per unit)
fn unit_pow(name: &str) -> Option<u32> {
    match name {
        "wei" => Some(0),
        "kwei" | "babbage" => Some(3),
        "mwei" | "lovelace" => Some(6),
        "gwei" | "shannon" => Some(9),
        "microether" | "szabo" => Some(12),
        "milliether" | "finney" => Some(15),
        "ether" | "eth" => Some(18),
        _ => None,
    }
}

/// decimal string → U256 scaled by 10^pow (fraction beyond precision truncated)
pub fn parse_scaled(s: &str, pow: u32) -> Result<U256, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty number".into());
    }
    if s.starts_with('-') {
        return Err("negative amounts are not supported".into());
    }
    let (whole, frac) = match s.split_once('.') {
        Some((w, f)) => (w, f),
        None => (s, ""),
    };
    if whole.is_empty() && frac.is_empty() {
        return Err(format!("bad number '{s}'"));
    }
    let mut acc = U256::zero();
    if !whole.is_empty() {
        acc = U256::from_dec_str(whole).map_err(|_| format!("bad number '{s}'"))?;
    }
    acc = acc * U256::from(10u64).pow(U256::from(pow));
    for (i, c) in frac.chars().enumerate().take(pow as usize) {
        if !c.is_ascii_digit() {
            return Err(format!("bad number '{s}'"));
        }
        let exp = pow - 1 - i as u32;
        let d = U256::from((c as u8 - b'0') as u64);
        if exp > 0 {
            acc = acc + d * U256::from(10u64).pow(U256::from(exp));
        } else {
            acc = acc + d;
        }
    }
    Ok(acc)
}

/// convert an amount between wei / gwei / ether / …
pub fn unit_convert(amount: &str, from: &str, to: &str) -> Result<String, String> {
    let from_pow = unit_pow(from).ok_or_else(|| format!("unknown unit '{from}'"))?;
    let to_pow = unit_pow(to).ok_or_else(|| format!("unknown unit '{to}'"))?;
    let wei = parse_scaled(amount, from_pow)?;
    Ok(render_scaled(&wei, to_pow))
}

/// U256 scaled by 10^pow → decimal string (fraction trimmed of trailing zeros)
pub fn render_scaled(v: &U256, pow: u32) -> String {
    let scale = U256::from(10u64).pow(U256::from(pow));
    let whole = *v / scale;
    let rem = *v % scale;
    if rem == U256::zero() {
        return whole.to_string();
    }
    let mut frac = format!("{:0width$}", rem.as_u128(), width = pow as usize);
    while frac.ends_with('0') {
        frac.pop();
    }
    format!("{whole}.{frac}")
}

/// 4-byte selector of "name(type1,type2)"
pub fn selector_of(sig: &str) -> Result<String, String> {
    let sig = sig.trim();
    let open = sig.find('(').ok_or("selector needs name(types) form")?;
    if !sig.ends_with(')') {
        return Err("selector needs name(types) form".into());
    }
    let name = &sig[..open];
    let types: Vec<&str> = sig[open + 1..sig.len() - 1]
        .split(',')
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect();
    let sel = rrx_util::abi::selector(name, &types);
    Ok(to_hex_prefixed(&sel))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keccak_known_vectors() {
        assert_eq!(
            keccak_of(""),
            "0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        );
        assert_eq!(
            keccak_of("abc"),
            "0x4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45"
        );
        // hex input path
        assert_eq!(keccak_of("0x"), keccak_of(""));
    }

    #[test]
    fn unit_conversions() {
        assert_eq!(unit_convert("1", "ether", "wei").unwrap(), "1000000000000000000");
        assert_eq!(unit_convert("1", "eth", "gwei").unwrap(), "1000000000");
        assert_eq!(unit_convert("1", "gwei", "ether").unwrap(), "0.000000001");
        assert_eq!(unit_convert("123", "wei", "ether").unwrap(), "0.000000000000000123");
        assert_eq!(unit_convert("2.5", "ether", "gwei").unwrap(), "2500000000");
        assert_eq!(unit_convert("42", "wei", "wei").unwrap(), "42");
        assert!(unit_convert("1", "btc", "wei").is_err());
    }

    #[test]
    fn selector_vectors() {
        assert_eq!(selector_of("transfer(address,uint256)").unwrap(), "0xa9059cbb");
        assert_eq!(selector_of("balanceOf(address)").unwrap(), "0x70a08231");
        assert_eq!(selector_of("totalSupply()").unwrap(), "0x18160ddd");
        assert!(selector_of("transfer").is_err());
    }

    #[test]
    fn parse_scaled_fraction() {
        assert_eq!(parse_scaled("1.5", 18).unwrap().as_u128(), 1_500_000_000_000_000_000);
        assert_eq!(parse_scaled("0.000000001", 9).unwrap().as_u128(), 1);
        assert!(parse_scaled("-2", 0).is_err());
    }
}
