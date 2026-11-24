//! rrx-crypto: real secp256k1 ECDSA for Ethereum — key generation, signing, verification,
//! public-key recovery and keccak-based address derivation.

use k256::ecdsa::signature::hazmat::PrehashVerifier;
use k256::ecdsa::{RecoveryId, Signature, SigningKey, VerifyingKey};
use rrx_util::{keccak256, H160, H256};

pub fn random_secret() -> [u8; 32] {
    use rand_core::RngCore;
    loop {
        let mut b = [0u8; 32];
        rand_core::OsRng.fill_bytes(&mut b);
        if k256::SecretKey::from_slice(&b).is_ok() {
            return b;
        }
    }
}

pub fn address_from_pubkey(pubkey65: &[u8]) -> Result<H160, String> {
    if pubkey65.len() != 65 || pubkey65[0] != 0x04 {
        return Err("expected 65-byte uncompressed secp256k1 public key".into());
    }
    let h = keccak256(&pubkey65[1..65]);
    Ok(H160::from_slice(&h[12..]))
}

#[derive(Clone, Debug)]
pub struct KeyPair {
    pub secret: [u8; 32],
}

impl KeyPair {
    pub fn from_slice(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() != 32 {
            return Err("secret key must be 32 bytes".into());
        }
        let mut s = [0u8; 32];
        s.copy_from_slice(bytes);
        k256::SecretKey::from_slice(&s).map_err(|e| format!("invalid secret key: {e}"))?;
        Ok(KeyPair { secret: s })
    }

    pub fn from_hex(s: &str) -> Result<Self, String> {
        let v = rrx_util::from_hex(s).map_err(|e| e.to_string())?;
        Self::from_slice(&v)
    }

    pub fn new_random() -> Self {
        KeyPair { secret: random_secret() }
    }

    fn signing_key(&self) -> Result<SigningKey, String> {
        let sk = k256::SecretKey::from_slice(&self.secret).map_err(|e| e.to_string())?;
        Ok(SigningKey::from(sk))
    }

    /// 65-byte uncompressed public key (0x04 || X || Y).
    pub fn public_key(&self) -> Result<[u8; 65], String> {
        let vk = VerifyingKey::from(self.signing_key()?);
        let p = vk.to_encoded_point(false);
        let mut out = [0u8; 65];
        out.copy_from_slice(p.as_bytes());
        Ok(out)
    }

    pub fn address(&self) -> Result<H160, String> {
        address_from_pubkey(&self.public_key()?)
    }

    pub fn sign_hash(&self, hash: &H256) -> Result<(Signature, RecoveryId), String> {
        self.signing_key()?.sign_prehash_recoverable(hash.as_bytes()).map_err(|e| e.to_string())
    }
}

pub fn recover_address(hash: &H256, sig: &Signature, recid: RecoveryId) -> Result<H160, String> {
    let vk = VerifyingKey::recover_from_prehash(hash.as_bytes(), sig, recid)
        .map_err(|e| format!("public key recovery failed: {e}"))?;
    let p = vk.to_encoded_point(false);
    address_from_pubkey(p.as_bytes())
}

pub fn recover_from_rsv(hash: &H256, r: &[u8; 32], s: &[u8; 32], recid: u8) -> Result<H160, String> {
    let mut sig64 = [0u8; 64];
    sig64[..32].copy_from_slice(r);
    sig64[32..].copy_from_slice(s);
    let sig = Signature::from_slice(&sig64).map_err(|e| e.to_string())?;
    let rid = RecoveryId::from_byte(recid).ok_or("bad recovery id")?;
    recover_address(hash, &sig, rid)
}

pub fn verify_hash(pubkey65: &[u8], hash: &H256, sig: &Signature) -> Result<bool, String> {
    let vk = VerifyingKey::from_sec1_bytes(pubkey65).map_err(|e| e.to_string())?;
    Ok(vk.verify_prehash(hash.as_bytes(), sig).is_ok())
}

/// EIP-191 personal message hash.
pub fn eip191_hash(message: &[u8]) -> H256 {
    let prefix = format!("\x19Ethereum Signed Message:\n{}", message.len());
    let mut buf = prefix.into_bytes();
    buf.extend_from_slice(message);
    H256(keccak256(&buf))
}

/// 65-byte signature (r || s || v27/28) over an EIP-191 personal message.
pub fn sign_message(message: &[u8], secret: &[u8; 32]) -> Result<[u8; 65], String> {
    let kp = KeyPair::from_slice(secret)?;
    let hash = eip191_hash(message);
    let (sig, recid) = kp.sign_hash(&hash)?;
    let raw = sig.to_bytes();
    let mut out = [0u8; 65];
    out[..32].copy_from_slice(&raw[..32]);
    out[32..64].copy_from_slice(&raw[32..64]);
    out[64] = 27 + recid.to_byte();
    Ok(out)
}

pub fn recover_message(sig65: &[u8; 65], message: &[u8]) -> Result<H160, String> {
    let mut r = [0u8; 32];
    r.copy_from_slice(&sig65[..32]);
    let mut s = [0u8; 32];
    s.copy_from_slice(&sig65[32..64]);
    let v = sig65[64];
    let recid = if v >= 27 { v - 27 } else { v };
    recover_from_rsv(&eip191_hash(message), &r, &s, recid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rrx_util::rlp::Rlp;
    use rrx_util::{to_hex, U256};

    /// EIP-155 example transaction: signing hash, deterministic signature, sender recovery.
    #[test]
    fn eip155_known_vector() {
        let key = [0x46u8; 32];
        let to = H160::from_slice(&rrx_util::from_hex("3535353535353535353535353535353535353535").unwrap());
        let signing = Rlp::list(vec![
            Rlp::u64(9),
            Rlp::u256(&U256::from(20_000_000_000u64)),
            Rlp::u64(21000),
            Rlp::h160(&to),
            Rlp::u256(&U256::from_dec_str("1000000000000000000").unwrap()),
            Rlp::Bytes(vec![]),
            Rlp::u64(1),
            Rlp::u64(0),
            Rlp::u64(0),
        ]);
        let hash = H256(keccak256(&signing.encode()));
        assert_eq!(
            to_hex(hash.as_bytes()),
            "daf5a779ae972f972197303d7b574746c7ef83eadac0f2791ad23db92e4c8e53"
        );

        let kp = KeyPair::from_slice(&key).unwrap();
        let (sig, recid) = kp.sign_hash(&hash).unwrap();
        let raw = sig.to_bytes();
        // deterministic RFC6979 signature from the EIP-155 specification
        assert_eq!(
            to_hex(&raw[..32]),
            "28ef61340bd939bc2195fe537567866003e1a15d3c71ff63e1590620aa636276"
        );
        assert_eq!(
            to_hex(&raw[32..]),
            "67cbe9d8997f761aecb703304b3800ccf555c9f3dc64214b297fb1966a3b6d83"
        );
        assert_eq!(recid.to_byte(), 0);

        // v = 37 for chainId 1
        let sender = recover_from_rsv(
            &hash,
            &raw[..32].try_into().unwrap(),
            &raw[32..].try_into().unwrap(),
            0,
        )
        .unwrap();
        assert_eq!(sender, kp.address().unwrap());
    }

    #[test]
    fn message_sign_roundtrip() {
        let kp = KeyPair::new_random();
        let msg = b"rrx says hello";
        let sig = sign_message(msg, &kp.secret).unwrap();
        let who = recover_message(&sig, msg).unwrap();
        assert_eq!(who, kp.address().unwrap());
        assert!(sig[64] == 27 || sig[64] == 28);
    }

    #[test]
    fn keygen_and_address() {
        for _ in 0..8 {
            let kp = KeyPair::new_random();
            let addr = kp.address().unwrap();
            assert_ne!(addr, H160::zero());
        }
    }
}
