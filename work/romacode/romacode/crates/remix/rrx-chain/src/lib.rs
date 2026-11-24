//! rrx-chain: real Ethereum-style execution layer — trie-backed world state,
//! typed transactions (EIP-155 / 2930 / 1559), receipts with logs bloom,
//! block building with EIP-1559 base fee, and a `Host` implementation for rrx-evm.

pub mod dev;
pub mod executor;
pub mod receipt;
pub mod state;
pub mod tx;

pub use executor::{Chain, ChainConfig};
pub use receipt::{LogBloom, Receipt};
pub use state::{storage_trie_root, WorldState};
pub use tx::{sign_transaction, AccessList, BlobSidecar, TxType, Transaction, UnsignedTx};
