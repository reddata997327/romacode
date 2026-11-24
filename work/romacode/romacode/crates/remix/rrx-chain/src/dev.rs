//! Deterministic dev accounts (like anvil's prefunded keys) for local real execution.

use rrx_crypto::KeyPair;
use rrx_util::{keccak256, H160};

/// Derive the i-th deterministic dev key: keccak256("rrx dev account <i>").
pub fn dev_key(i: usize) -> [u8; 32] {
    keccak256(format!("rrx dev account {i}").as_bytes())
}

pub fn dev_keypair(i: usize) -> KeyPair {
    KeyPair::from_slice(&dev_key(i)).expect("valid dev key")
}

pub fn dev_address(i: usize) -> H160 {
    dev_keypair(i).address().expect("valid dev address")
}

pub fn dev_accounts(count: usize) -> Vec<(KeyPair, H160)> {
    (0..count)
        .map(|i| {
            let kp = dev_keypair(i);
            let addr = kp.address().expect("valid");
            (kp, addr)
        })
        .collect()
}
