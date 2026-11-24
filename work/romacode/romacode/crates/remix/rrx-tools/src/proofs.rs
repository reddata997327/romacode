//! State proofs: verify REAL mainnet eth_getProof Merkle-Patricia proofs
//! locally with our own RLP decoding + MPT walk against a block stateRoot.
//!
//! This is cryptographic verification of chain data — the node cannot lie
//! about an account or storage slot if the proof verifies.

use rrx_util::rlp::Rlp;
use rrx_util::{h256_from_hex, h160_from_hex, keccak256, u256_from_str, H160, H256, U256};
use serde_json::Value;

// ---------------------------------------------------------------- RLP decode

fn be_usize(bytes: &[u8]) -> Result<usize, String> {
    if bytes.len() > 8 {
        return Err("rlp: length too large".into());
    }
    let mut n = 0usize;
    for b in bytes {
        n = (n << 8) | *b as usize;
    }
    Ok(n)
}

/// Decode one RLP item; returns (item, total bytes consumed).
pub fn rlp_decode(input: &[u8]) -> Result<(Rlp, usize), String> {
    if input.is_empty() {
        return Err("rlp: empty input".into());
    }
    let b0 = input[0];
    if b0 <= 0x7f {
        return Ok((Rlp::Bytes(vec![b0]), 1));
    }
    if b0 <= 0xb7 {
        let n = (b0 - 0x80) as usize;
        if input.len() < 1 + n {
            return Err("rlp: truncated string".into());
        }
        return Ok((Rlp::Bytes(input[1..1 + n].to_vec()), 1 + n));
    }
    if b0 <= 0xbf {
        let lb = (b0 - 0xb7) as usize;
        if input.len() < 1 + lb {
            return Err("rlp: truncated long string".into());
        }
        let n = be_usize(&input[1..1 + lb])?;
        if input.len() < 1 + lb + n {
            return Err("rlp: truncated long string payload".into());
        }
        return Ok((Rlp::Bytes(input[1 + lb..1 + lb + n].to_vec()), 1 + lb + n));
    }
    // lists
    let (lb, payload) = if b0 <= 0xf7 {
        let n = (b0 - 0xc0) as usize;
        if input.len() < 1 + n {
            return Err("rlp: truncated list".into());
        }
        (0usize, &input[1..1 + n])
    } else {
        let lb = (b0 - 0xf7) as usize;
        if input.len() < 1 + lb {
            return Err("rlp: truncated long list".into());
        }
        let n = be_usize(&input[1..1 + lb])?;
        if input.len() < 1 + lb + n {
            return Err("rlp: truncated long list payload".into());
        }
        (lb, &input[1 + lb..1 + lb + n])
    };
    let mut items = vec![];
    let mut off = 0usize;
    while off < payload.len() {
        let (item, used) = rlp_decode(&payload[off..])?;
        items.push(item);
        off += used;
    }
    if off != payload.len() {
        return Err("rlp: trailing bytes inside list".into());
    }
    let _ = lb;
    Ok((Rlp::List(items), 1 + lb + payload.len()))
}

/// get a child reference: inline node if < 32 bytes RLP, else a 32-byte hash
fn node_ref(item: &Rlp) -> Ref {
    match item {
        Rlp::Bytes(b) if b.len() == 32 => Ref::Hashed(H256::from_slice(b)),
        other => Ref::Inline(other.clone()),
    }
}

enum Ref {
    Inline(Rlp),
    Hashed(H256),
}

// ---------------------------------------------------------------- MPT walk

/// hex-prefix decode: (is_leaf, nibbles)
fn hpx_decode(hp: &[u8]) -> Result<(bool, Vec<u8>), String> {
    if hp.is_empty() {
        return Err("hpx: empty".into());
    }
    let flag = hp[0] >> 4;
    let is_leaf = flag & 2 != 0;
    let odd = flag & 1 != 0;
    let mut nibbles = vec![];
    if odd {
        nibbles.push(hp[0] & 0x0f);
    }
    for b in &hp[1..] {
        nibbles.push(b >> 4);
        nibbles.push(b & 0x0f);
    }
    Ok((is_leaf, nibbles))
}

struct ProofNode {
    rlp: Rlp,
    raw: Vec<u8>,
}

fn parse_nodes(proof_hexes: &[Value]) -> Result<Vec<ProofNode>, String> {
    let mut nodes = vec![];
    for (i, v) in proof_hexes.iter().enumerate() {
        let hex = v.as_str().ok_or_else(|| format!("proof[{i}] not a string"))?;
        let raw = rrx_util::from_hex(hex).map_err(|e| format!("proof[{i}]: {e}"))?;
        let (rlp, used) = rlp_decode(&raw).map_err(|e| format!("proof[{i}]: {e}"))?;
        if used != raw.len() {
            return Err(format!("proof[{i}]: trailing bytes"));
        }
        nodes.push(ProofNode { rlp, raw });
    }
    if nodes.is_empty() {
        return Err("empty proof".into());
    }
    Ok(nodes)
}

fn find_node<'a>(nodes: &'a [ProofNode], hash: &H256) -> Result<&'a ProofNode, String> {
    match nodes.iter().find(|n| H256::from(keccak256(&n.raw)) == *hash) {
        Some(n) => Ok(n),
        None => {
            if std::env::var("RRX_DEBUG_PROOF").is_ok() {
                eprintln!("DBG-walk missing {hash} among {} nodes: {}",
                    nodes.len(),
                    nodes.iter().map(|n| format!("{:#x}", H256::from(keccak256(&n.raw)))).collect::<Vec<_>>().join(","));
            }
            Err("proof: referenced node missing from proof set".into())
        }
    }
}

/// Full account proof verification.
/// Returns (nonce, balance, storage_root, code_hash) extracted from the leaf.
pub fn verify_account_proof(
    state_root: &H256,
    address: &H160,
    proof_hexes: &[Value],
) -> Result<(U256, U256, H256, H256), String> {
    let nodes = parse_nodes(proof_hexes)?;
    // verify node hashes chain to the root (first node must hash to root)
    let first_hash = H256::from(keccak256(&nodes[0].raw));
    if first_hash != *state_root {
        return Err(format!(
            "proof: first node hash {} != state root {}",
            first_hash, state_root
        ));
    }
    let nibbles = to_nibbles(&keccak256(address.as_bytes()));
    let leaf = walk_proof_owned(&nodes, state_root, &nibbles)?;
    let (item, _) = rlp_decode(&leaf)?;
    match item {
        Rlp::List(items) if items.len() == 4 => {
            let nonce = rlp_scalar(&items[0])?;
            let balance = rlp_scalar(&items[1])?;
            let storage_root = match &items[2] {
                Rlp::Bytes(b) if b.len() == 32 => H256::from_slice(b),
                Rlp::Bytes(b) if b.is_empty() => H256::from(keccak256(&rlp_empty_list())),
                _ => return Err("account leaf: bad storage root".into()),
            };
            let code_hash = match &items[3] {
                Rlp::Bytes(b) if b.len() == 32 => H256::from_slice(b),
                _ => return Err("account leaf: bad code hash".into()),
            };
            Ok((nonce, balance, storage_root, code_hash))
        }
        _ => Err("account leaf: expected [nonce, balance, storageRoot, codeHash]".into()),
    }
}

fn rlp_empty_list() -> Vec<u8> {
    Rlp::empty_list().encode()
}

fn rlp_scalar(item: &Rlp) -> Result<U256, String> {
    match item {
        Rlp::Bytes(b) if b.len() <= 32 => {
            let mut buf = [0u8; 32];
            buf[32 - b.len()..].copy_from_slice(b);
            Ok(U256::from_big_endian(&buf))
        }
        _ => Err("expected scalar".into()),
    }
}

/// 32-byte hash -> 64 nibbles (the trie path domain)
fn to_nibbles(key: &[u8]) -> Vec<u8> {
    let mut n = Vec::with_capacity(key.len() * 2);
    for b in key {
        n.push(b >> 4);
        n.push(b & 0x0f);
    }
    n
}

fn walk_proof_owned(nodes: &[ProofNode], root: &H256, key: &[u8]) -> Result<Vec<u8>, String> {
    let first = find_node(nodes, root)?;
    let mut current: Rlp = first.rlp.clone();
    let mut consumed = 0usize;
    loop {
        match current.clone() {
            Rlp::List(items) if items.len() == 17 => {
                if consumed == key.len() {
                    return match &items[16] {
                        Rlp::Bytes(b) => Ok(b.clone()),
                        _ => Err("branch terminator: no value".into()),
                    };
                }
                let nib = key[consumed] as usize;
                consumed += 1;
                if std::env::var("RRX_DEBUG_PROOF").is_ok() {
                    eprintln!("DBG-walk branch consumed={consumed} nib={nib} child={:?}", items[nib]);
                }
                current = match node_ref(&items[nib]) {
                    Ref::Inline(next) => next,
                    Ref::Hashed(h) => find_node(nodes, &h)?.rlp.clone(),
                };
            }
            Rlp::List(items) if items.len() == 2 => {
                let (is_leaf, path) = match &items[0] {
                    Rlp::Bytes(hp) => hpx_decode(hp)?,
                    _ => return Err("extension/leaf: path not bytes".into()),
                };
                if consumed + path.len() > key.len()
                    || key[consumed..consumed + path.len()] != path[..]
                {
                    return Err(format!(
                        "proof: path mismatch at nibble {consumed} (node path {:?})",
                        path
                    ));
                }
                consumed += path.len();
                if is_leaf {
                    if consumed != key.len() {
                        return Err("proof: leaf ended before key was consumed".into());
                    }
                    return match &items[1] {
                        Rlp::Bytes(b) => Ok(b.clone()),
                        other => Ok(other.encode()),
                    };
                }
                current = match node_ref(&items[1]) {
                    Ref::Inline(next) => next,
                    Ref::Hashed(h) => find_node(nodes, &h)?.rlp.clone(),
                };
            }
            Rlp::Bytes(b) => {
                let (decoded, _) = rlp_decode(&b)?;
                current = decoded;
            }
            _ => return Err("proof: unknown node shape".into()),
        }
    }
}

/// Storage proof verification: returns the proven slot values.
pub fn verify_storage_proofs(
    storage_root: &H256,
    storage_proof: &[Value],
) -> Result<Vec<(U256, U256)>, String> {
    let mut out = vec![];
    for sp in storage_proof {
        let key_str = sp["key"].as_str().ok_or("storage proof: no key")?;
        let claimed = u256_from_str(sp["value"].as_str().unwrap_or("0x0"))?;
        let proof = sp["proof"].as_array().ok_or("storage proof: no proof array")?;
        let nodes = parse_nodes(proof)?;
        let first_hash = H256::from(keccak256(&nodes[0].raw));
        if first_hash != *storage_root {
            return Err(format!(
                "storage proof: first node hash {} != storageRoot {}",
                first_hash, storage_root
            ));
        }
        let slot = u256_from_str(key_str)?;
        // storage trie keys are keccak256(slot as 32-byte big-endian), walked as nibbles
        let nibbles = to_nibbles(&keccak256(&rrx_util::u256_to_be32(&slot)));
        let leaf = walk_proof_owned(&nodes, storage_root, &nibbles)?;
        let (item, _) = rlp_decode(&leaf)?;
        let proven = rlp_scalar(&item)?;
        if proven != claimed {
            return Err(format!(
                "storage proof: node says {proven}, rpc said {claimed} — node is LYING"
            ));
        }
        out.push((slot, proven));
    }
    Ok(out)
}

/// One-shot: verify an eth_getProof response against a block's stateRoot.
pub fn verify_get_proof_response(proof: &Value, state_root_hex: &str) -> Result<String, String> {
    let state_root = h256_from_hex(state_root_hex)?;
    let addr = h160_from_hex(proof["address"].as_str().ok_or("missing address")?)?;
    let account_proof = proof["accountProof"]
        .as_array()
        .ok_or("missing accountProof")?;
    let (nonce, balance, storage_root, code_hash) =
        verify_account_proof(&state_root, &addr, account_proof)?;

    // the claimed fields must equal the proven ones
    let claimed_balance = u256_from_str(proof["balance"].as_str().unwrap_or("0x0"))?;
    let claimed_nonce = u256_from_str(proof["nonce"].as_str().unwrap_or("0x0"))?;
    if claimed_balance != balance || claimed_nonce != nonce {
        return Err("account header fields do not match the proven leaf".into());
    }
    let mut out = format!(
        "PROOF VALID ✓\n  account 0x{addr:x} verified against state root {state_root_hex}\n");
    out.push_str(&format!("  nonce       {nonce}\n  balance     {balance} wei\n"));
    out.push_str(&format!("  storageRoot {:#x}\n", storage_root));
    out.push_str(&format!("  codeHash    {:#x}\n", code_hash));

    let slots = proof["storageProof"].as_array().cloned().unwrap_or_default();
    if !slots.is_empty() {
        let proven = verify_storage_proofs(&storage_root, &slots)?;
        out.push_str(&format!("  storage: {} slot(s) verified\n", proven.len()));
        for (k, v) in proven {
            out.push_str(&format!("    slot 0x{k:x} = {v}\n"));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rlp_roundtrip_scalars() {
        for v in [0u64, 1, 15, 16, 127, 128, 255, 1024, 1 << 40] {
            let enc = Rlp::u64(v).encode();
            let (dec, used) = rlp_decode(&enc).unwrap();
            assert_eq!(used, enc.len());
            match dec {
                Rlp::Bytes(b) => {
                    let mut buf = [0u8; 8];
                    buf[8 - b.len()..].copy_from_slice(&b);
                    assert_eq!(u64::from_be_bytes(buf), v, "value {v}");
                }
                _ => panic!("not bytes"),
            }
        }
    }

    #[test]
    fn rlp_roundtrip_lists() {
        let l = Rlp::list(vec![
            Rlp::u64(1),
            Rlp::bytes("cat".as_bytes().to_vec()),
            Rlp::list(vec![Rlp::u64(1024)]),
        ]);
        let enc = l.encode();
        let (dec, used) = rlp_decode(&enc).unwrap();
        assert_eq!(used, enc.len());
        match dec {
            Rlp::List(items) => {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0], Rlp::u64(1));
                assert_eq!(items[1], Rlp::bytes("cat".as_bytes().to_vec()));
                match &items[2] {
                    Rlp::List(inner) => assert_eq!(inner[0], Rlp::u64(1024)),
                    _ => panic!(),
                }
            }
            _ => panic!("not list"),
        }
    }

    #[test]
    fn rlp_empty_and_long() {
        let (d, _) = rlp_decode(&[0x80]).unwrap();
        assert_eq!(d, Rlp::Bytes(vec![]));
        let big = Rlp::bytes(vec![7u8; 60]);
        let enc = big.encode();
        assert_eq!(enc[0], 0xb8);
        let (d, used) = rlp_decode(&enc).unwrap();
        assert_eq!(used, enc.len());
        assert_eq!(d, big);
        // long list (>55 byte payload) -> 0xf8/0xf9 header
        let items: Vec<Rlp> = (0..10).map(|_| Rlp::bytes(vec![0u8; 30])).collect();
        let enc = Rlp::list(items).encode();
        assert_eq!(enc[0], 0xf9); // 10*32 = 320 byte payload
        let (d, used) = rlp_decode(&enc).unwrap();
        assert_eq!(used, enc.len());
        match d {
            Rlp::List(v) => assert_eq!(v.len(), 10),
            _ => panic!(),
        }
    }

    #[test]
    fn hpx_decode_cases() {
        let (leaf, nib) = hpx_decode(&[0x20, 0x01, 0x02]).unwrap();
        assert!(leaf);
        assert_eq!(nib, vec![0, 1, 0, 2]);
        // odd-length leaf path [1,2,3]: flags 3 + first nibble packed
        let (leaf, nib) = hpx_decode(&[0x31, 0x23]).unwrap();
        assert!(leaf);
        assert_eq!(nib, vec![1, 2, 3]);
        // odd-length extension path [1,2,3]: flags 1
        let (leaf, nib) = hpx_decode(&[0x11, 0x23]).unwrap();
        assert!(!leaf);
        assert_eq!(nib, vec![1, 2, 3]);
        // even-length leaf [1,2,3,4]: flags 2 + byte pairs (1,2)(3,4)
        let (leaf, nib) = hpx_decode(&[0x20, 0x12, 0x34]).unwrap();
        assert!(leaf);
        assert_eq!(nib, vec![1, 2, 3, 4]);
        let (leaf, nib) = hpx_decode(&[0x00]).unwrap();
        assert!(!leaf);
        assert!(nib.is_empty());
    }
}
