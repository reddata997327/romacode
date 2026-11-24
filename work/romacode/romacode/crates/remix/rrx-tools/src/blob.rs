//! EIP-4844 blob transaction tooling: decode real type-3 txs from the wire,
//! cryptographically verify KZG sidecars (blob ↔ commitment ↔ versioned hash),
//! build + sign real blob txs, and fetch live blob txs from mainnet.

use rrx_bls381::kzg;
use rrx_chain::{sign_transaction, BlobSidecar, TxType, UnsignedTx, Transaction};
use rrx_crypto::KeyPair;
use rrx_util::{keccak256, rlp::Rlp, H160, H256, U256};

/// Wrap a signed blob tx payload with its sidecar (network form):
/// 0x03 || rlp([tx_payload_body, commitments, blobs, proofs])
pub fn wrap_with_sidecar(tx: &Transaction, sidecar: &BlobSidecar) -> Result<Vec<u8>, String> {
    if !matches!(tx, Transaction::Eip4844 { .. }) {
        return Err("not a blob transaction".into());
    }
    if sidecar.blobs.len() != sidecar.commitments.len() || sidecar.blobs.len() != sidecar.proofs.len() {
        return Err("sidecar length mismatch".into());
    }
    // tx payload body without the type byte: strip the 0x03 prefix from rlp_encode.
    // The payload body is itself a valid RLP item (a list), so we can embed it raw:
    // wrapper = 0x03 || rlp([payload_list, commitments, blobs, proofs])
    let full = tx.rlp_encode();
    let body = &full[1..];
    let commits = Rlp::list(sidecar.commitments.iter().map(|c| Rlp::Bytes(c.to_vec())).collect()).encode();
    let blobs = Rlp::list(sidecar.blobs.iter().map(|b| Rlp::Bytes(b.clone())).collect()).encode();
    let proofs = Rlp::list(sidecar.proofs.iter().map(|p| Rlp::Bytes(p.to_vec())).collect()).encode();
    let out = vec![0x03u8];
    let items: Vec<Vec<u8>> = vec![body.to_vec(), commits, blobs, proofs];
    Ok([out, rlp_list_raw(&items)].concat())
}

/// Encode a list from already-encoded raw items.
fn rlp_list_raw(items: &[Vec<u8>]) -> Vec<u8> {
    let payload_len: usize = items.iter().map(|i| i.len()).sum();
    let mut out = Vec::new();
    if payload_len < 56 {
        out.push(0xc0 + payload_len as u8);
    } else {
        let lenb = payload_len.to_be_bytes();
        let start = lenb.iter().position(|&b| b != 0).unwrap();
        let sig = &lenb[start..];
        out.push(0xf7 + sig.len() as u8);
        out.extend_from_slice(sig);
    }
    for i in items {
        out.extend_from_slice(i);
    }
    out
}

/// Minimal RLP walker for the sidecar wrapper.
struct SidecarDec<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> SidecarDec<'a> {
    fn enter_list(&mut self) -> Result<usize, String> {
        let b = *self.data.get(self.pos).ok_or("rlp: eof")?;
        let (start, len) = if (0xc0..0xf8).contains(&b) {
            (self.pos + 1, (b - 0xc0) as usize)
        } else if b >= 0xf8 {
            let ll = (b - 0xf7) as usize;
            let mut l = 0usize;
            for &x in &self.data[self.pos + 1..self.pos + 1 + ll] {
                l = (l << 8) | x as usize;
            }
            (self.pos + 1 + ll, l)
        } else {
            return Err("rlp: expected list".into());
        };
        if start + len > self.data.len() {
            return Err("rlp: truncated list".into());
        }
        self.pos = start;
        Ok(start + len)
    }

    fn item(&mut self) -> Result<&'a [u8], String> {
        let b = *self.data.get(self.pos).ok_or("rlp: eof")?;
        let (start, len) = if b < 0x80 {
            (self.pos, 1)
        } else if b < 0xb8 {
            (self.pos + 1, (b - 0x80) as usize)
        } else if b < 0xc0 {
            let ll = (b - 0xb7) as usize;
            let mut l = 0usize;
            for &x in &self.data[self.pos + 1..self.pos + 1 + ll] {
                l = (l << 8) | x as usize;
            }
            (self.pos + 1 + ll, l)
        } else {
            return Err("rlp: expected bytes item".into());
        };
        if start + len > self.data.len() {
            return Err("rlp: truncated".into());
        }
        self.pos = start;
        let out = &self.data[start..start + len];
        self.pos += len;
        Ok(out)
    }

    /// Full raw encoding of the next item (bytes or list), incl. prefix.
    fn item_raw(&mut self) -> Result<&'a [u8], String> {
        let start = self.pos;
        let b = *self.data.get(self.pos).ok_or("rlp: eof")?;
        let payload_start;
        let payload_len;
        if b < 0x80 {
            payload_start = self.pos;
            payload_len = 1;
        } else if b < 0xb8 {
            payload_start = self.pos + 1;
            payload_len = (b - 0x80) as usize;
        } else if b < 0xc0 {
            let ll = (b - 0xb7) as usize;
            let mut l = 0usize;
            for &x in &self.data[self.pos + 1..self.pos + 1 + ll] {
                l = (l << 8) | x as usize;
            }
            payload_start = self.pos + 1 + ll;
            payload_len = l;
        } else if b < 0xf8 {
            payload_start = self.pos + 1;
            payload_len = (b - 0xc0) as usize;
        } else {
            let ll = (b - 0xf7) as usize;
            let mut l = 0usize;
            for &x in &self.data[self.pos + 1..self.pos + 1 + ll] {
                l = (l << 8) | x as usize;
            }
            payload_start = self.pos + 1 + ll;
            payload_len = l;
        }
        if payload_start + payload_len > self.data.len() {
            return Err("rlp: truncated item".into());
        }
        let end = payload_start + payload_len;
        self.pos = end;
        Ok(&self.data[start..end])
    }
}

/// Decode a network-wrapped blob tx (with sidecar). Returns tx + sidecar.
pub fn decode_with_sidecar(raw: &[u8]) -> Result<(Transaction, BlobSidecar), String> {
    if raw.is_empty() || raw[0] != 0x03 {
        return Err("not a blob transaction wrapper (expected 0x03 prefix)".into());
    }
    // outer structure: rlp([payload_list, commitments, blobs, proofs])
    let mut d = SidecarDec { data: &raw[1..], pos: 0 };
    let _outer_end = d.enter_list()?;
    let payload = d.item_raw()?.to_vec();
    // the payload list (without type byte) is the signed tx payload → re-add 0x03 prefix
    let tx = Transaction::decode(&[&[0x03u8], &payload[..]].concat())?;
    let c_end = d.enter_list()?;
    let mut commitments = vec![];
    while d.pos < c_end {
        let b = d.item()?;
        if b.len() != 48 {
            return Err("commitment must be 48 bytes".into());
        }
        let mut c = [0u8; 48];
        c.copy_from_slice(b);
        commitments.push(c);
    }
    d.pos = c_end;
    let b_end = d.enter_list()?;
    let mut blobs = vec![];
    while d.pos < b_end {
        blobs.push(d.item()?.to_vec());
    }
    d.pos = b_end;
    let p_end = d.enter_list()?;
    let mut proofs = vec![];
    while d.pos < p_end {
        let b = d.item()?;
        if b.len() != 48 {
            return Err("proof must be 48 bytes".into());
        }
        let mut pr = [0u8; 48];
        pr.copy_from_slice(b);
        proofs.push(pr);
    }
    Ok((tx, BlobSidecar { blobs, commitments, proofs }))
}

/// REAL KZG verification of a full sidecar against the tx's versioned hashes.
pub fn verify_sidecar(tx: &Transaction, sidecar: &BlobSidecar) -> Result<String, String> {
    let vhs = match tx {
        Transaction::Eip4844 { blob_versioned_hashes, .. } => blob_versioned_hashes,
        _ => return Err("not a blob transaction".into()),
    };
    if vhs.len() != sidecar.blobs.len() {
        return Err(format!(
            "versioned hash count {} != blob count {}",
            vhs.len(),
            sidecar.blobs.len()
        ));
    }
    let mut report = String::from("KZG sidecar verification (real BLS12-381 pairing checks):\n");
    for (i, blob) in sidecar.blobs.iter().enumerate() {
        let c = &sidecar.commitments[i];
        let p = &sidecar.proofs[i];
        // 1. versioned hash must equal 0x01 || sha256(commitment)[1..]
        let vh = kzg::versioned_hash(c)?;
        if vh != vhs[i].0 {
            return Err(format!(
                "blob #{}: versioned hash mismatch (tx hash {} != computed {})",
                i,
                hex::encode(vhs[i].0),
                hex::encode(vh)
            ));
        }
        // 2. full pairing check: verify_blob_kzg_proof(blob, commitment, proof)
        let ok = kzg::verify_blob_kzg_proof(blob, c, p)?;
        if !ok {
            return Err(format!("blob #{}: KZG proof verification FAILED", i));
        }
        report.push_str(&format!(
            "  blob #{}: ✓ versioned hash + KZG pairing verified ({} bytes)\n",
            i,
            blob.len()
        ));
    }
    Ok(report)
}

/// Build + sign a real blob transaction from a blob file.
/// Computes the REAL KZG commitment + proof with the Ethereum trusted setup.
pub fn build_and_sign(
    blob: &[u8],
    to: H160,
    key: &KeyPair,
    chain_id: u64,
    nonce: u64,
    max_fee: U256,
    max_priority_fee: U256,
    max_fee_per_blob_gas: U256,
    gas_limit: u64,
    value: U256,
    data: Vec<u8>,
) -> Result<(Transaction, BlobSidecar), String> {
    let commitment = kzg::blob_to_kzg_commitment(blob)?;
    let proof = kzg::compute_blob_kzg_proof(blob, &commitment)?;
    let vh = kzg::versioned_hash(&commitment)?;
    let unsigned = UnsignedTx {
        tx_type: TxType::Eip4844,
        chain_id,
        nonce,
        gas_price: U256::zero(),
        max_fee,
        max_priority_fee,
        gas_limit,
        to: Some(to),
        value,
        data,
        access_list: vec![],
        max_fee_per_blob_gas,
        blob_versioned_hashes: vec![H256(vh)],
    };
    let tx = sign_transaction(&unsigned, key)?;
    let sidecar = BlobSidecar { blobs: vec![blob.to_vec()], commitments: vec![commitment], proofs: vec![proof] };
    Ok((tx, sidecar))
}

/// Human-readable report of a decoded blob tx.
pub fn tx_report(tx: &Transaction) -> String {
    match tx {
        Transaction::Eip4844 { chain_id, nonce, max_priority_fee, max_fee, gas_limit, to, value, data, max_fee_per_blob_gas, blob_versioned_hashes, y_parity, .. } => {
            let mut out = String::new();
            out.push_str("EIP-4844 blob transaction (type 0x03)\n");
            out.push_str(&format!("  tx hash:        0x{}\n", hex::encode(tx.hash().0)));
            out.push_str(&format!("  sender:         0x{}\n", hex::encode(tx.sender().map(|a| a.0).unwrap_or_default())));
            out.push_str(&format!("  chain id:       {}\n", chain_id));
            out.push_str(&format!("  nonce:          {}\n", nonce));
            out.push_str(&format!("  to:             0x{}\n", hex::encode(to.0)));
            out.push_str(&format!("  value:          {} wei\n", value));
            out.push_str(&format!("  max fee:        {} wei\n", max_fee));
            out.push_str(&format!("  max priority:   {} wei\n", max_priority_fee));
            out.push_str(&format!("  gas limit:      {}\n", gas_limit));
            out.push_str(&format!("  data:           {} bytes\n", data.len()));
            out.push_str(&format!("  max blob fee:   {} wei\n", max_fee_per_blob_gas));
            out.push_str(&format!("  y_parity:       {}\n", y_parity));
            out.push_str(&format!("  blobs:          {}\n", blob_versioned_hashes.len()));
            for (i, vh) in blob_versioned_hashes.iter().enumerate() {
                out.push_str(&format!("  vhash[{}]:       0x{}\n", i, hex::encode(vh.0)));
            }
            out
        }
        _ => format!("transaction (not type 3): hash 0x{}", hex::encode(tx.hash().0)),
    }
}

/// Fetch a REAL transaction from mainnet and report; blob txs show versioned hashes.
pub fn fetch_live(p: &rrx_rpc::Provider, txhash: &str) -> Result<String, String> {
    let h = txhash.trim_start_matches("0x");
    if h.len() != 64 {
        return Err("tx hash must be 32 bytes".into());
    }
    let raw_hash = hex::decode(h).map_err(|e| e.to_string())?;
    let mut bh = [0u8; 32];
    bh.copy_from_slice(&raw_hash);
    let v = p.get_transaction(&H256(bh))?
        .ok_or_else(|| "transaction not found on chain".to_string())?;
    let ttype = v.get("type").and_then(|t| t.as_str()).unwrap_or("0x0").to_string();
    let mut out = String::new();
    out.push_str(&format!("live tx {} on mainnet\n", txhash));
    out.push_str(&format!("  type:       {}{}\n", ttype, if ttype == "0x3" { " (EIP-4844 blob)" } else { "" }));
    if let Some(bl) = v.get("blockNumber").and_then(|b| b.as_str()) {
        out.push_str(&format!("  block:      {}\n", bl));
    }
    if let Some(fr) = v.get("from").and_then(|b| b.as_str()) {
        out.push_str(&format!("  from:       {}\n", fr));
    }
    if let Some(to) = v.get("to").and_then(|b| b.as_str()) {
        out.push_str(&format!("  to:         {}\n", to));
    }
    if ttype == "0x3" {
        if let Some(vhs) = v.get("blobVersionedHashes").and_then(|b| b.as_array()) {
            out.push_str(&format!("  blobs:      {}\n", vhs.len()));
            for (i, vh) in vhs.iter().enumerate() {
                out.push_str(&format!("  vhash[{}]:   {}\n", i, vh.as_str().unwrap_or("?")));
            }
        }
        if let Some(mfbg) = v.get("maxFeePerBlobGas").and_then(|b| b.as_str()) {
            out.push_str(&format!("  maxBlobGas: {} wei\n", mfbg));
        }
        out.push_str("  note:       blob sidecar data lives on the beacon chain, not the execution layer;\n");
        out.push_str("              use `rrx blob verify --raw <network-wrapper>` to cryptographically verify a sidecar\n");
        out.push_str("              or fetch one from blobscan.com by versioned hash\n");
    } else {
        out.push_str("  (use `rrx tx <hash>` for the full report)\n");
    }
    let _ = keccak256(&[0u8; 1]); // keep import used
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> KeyPair {
        KeyPair::from_slice(&[7u8; 32]).unwrap()
    }

    #[test]
    fn blob_tx_roundtrip_and_verify() {
        // synthetic-but-canonical blob
        let mut blob = vec![0u8; kzg::BLOB_SIZE];
        for (i, b) in blob.iter_mut().enumerate() {
            *b = ((i * 13 + 5) % 256) as u8;
        }
        for chunk in blob.chunks_mut(32) {
            chunk[0] &= 0x0f;
        }
        let key = test_key();
        let to = H160::from_low_u64_be(0xdead);
        let (tx, sidecar) = build_and_sign(
            &blob,
            to,
            &key,
            1,                          // mainnet
            42,
            U256::from(2_000_000_000u64),
            U256::from(100_000_000u64),
            U256::from(1_000_000_000u64),
            120_000,
            U256::zero(),
            vec![0x11, 0x22],
        )
        .unwrap();
        // sender recovered correctly
        assert_eq!(tx.sender().unwrap(), key.address().unwrap());
        // KZG sidecar verifies cryptographically
        let rep = verify_sidecar(&tx, &sidecar).unwrap();
        assert!(rep.contains("✓"));
        // network wrapper roundtrip
        let wrapped = wrap_with_sidecar(&tx, &sidecar).unwrap();
        let (tx2, sc2) = decode_with_sidecar(&wrapped).unwrap();
        assert_eq!(tx2, tx);
        assert_eq!(sc2, sidecar);
        assert!(verify_sidecar(&tx2, &sc2).unwrap().contains("✓"));
        // tamper with a blob byte → verification must fail
        let mut bad = sidecar.clone();
        bad.blobs[0][100] ^= 0x01;
        assert!(verify_sidecar(&tx, &bad).is_err());
    }

    #[test]
    fn blob_tx_decode_el_payload() {
        let mut blob = vec![0u8; kzg::BLOB_SIZE];
        blob[0] = 0x01;
        let (tx, _sc) = build_and_sign(
            &blob,
            H160::from_low_u64_be(1),
            &test_key(),
            11155111,
            1,
            U256::from(1_000_000_000u64),
            U256::from(1),
            U256::from(1),
            50_000,
            U256::zero(),
            vec![],
        )
        .unwrap();
        let raw = tx.rlp_encode();
        let decoded = Transaction::decode(&raw).unwrap();
        assert_eq!(decoded, tx);
        match &decoded {
            Transaction::Eip4844 { blob_versioned_hashes, chain_id, .. } => {
                assert_eq!(chain_id, &11155111);
                assert_eq!(blob_versioned_hashes.len(), 1);
                assert_eq!(blob_versioned_hashes[0].0[0], 0x01);
            }
            _ => panic!("wrong type"),
        }
    }
}
