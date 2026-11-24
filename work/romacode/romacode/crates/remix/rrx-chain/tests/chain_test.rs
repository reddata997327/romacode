//! End-to-end chain tests: real signed txs, real state, real blocks.

use rrx_chain::dev::dev_keypair;
use rrx_chain::tx::{sign_transaction, TxType, UnsignedTx};
use rrx_chain::{Chain, ChainConfig};
use rrx_evm::Host;
use rrx_util::{abi, h160_from_hex, u256_from_str, H160, U256};

fn new_chain() -> Chain {
    let mut alloc = vec![];
    for i in 0..10 {
        let kp = dev_keypair(i);
        alloc.push((kp.address().unwrap(), U256::from_dec_str("1000000000000000000000").unwrap()));
    }
    Chain::genesis(ChainConfig::default(), &alloc)
}

fn alice() -> H160 {
    dev_keypair(0).address().unwrap()
}
fn bob() -> H160 {
    dev_keypair(1).address().unwrap()
}

#[test]
fn signed_transfer_changes_balances_and_nonce() {
    let mut chain = new_chain();
    let bal_before = chain.state.balance(&bob());
    let kp = dev_keypair(0);
    let tx = sign_transaction(
        &UnsignedTx {
            max_fee_per_blob_gas: Default::default(),
            blob_versioned_hashes: vec![],
            tx_type: TxType::Legacy,
            chain_id: 31337,
            nonce: 0,
            gas_price: U256::from(2_000_000_000u64),
            max_fee: U256::zero(),
            max_priority_fee: U256::zero(),
            gas_limit: 21_000,
            to: Some(bob()),
            value: U256::from(1_000_000_000_000_000_000u64), // 1 ETH
            data: vec![],
            access_list: vec![],
        },
        &kp,
    )
    .unwrap();
    assert_eq!(tx.sender().unwrap(), alice());
    let raw = tx.rlp_encode();
    let decoded = rrx_chain::Transaction::decode(&raw).unwrap();
    assert_eq!(decoded.sender().unwrap(), alice());
    assert_eq!(decoded.hash(), tx.hash());

    let tx_hash = tx.hash();
    chain.mine_block(vec![tx]).unwrap();
    let bal_after = chain.state.balance(&bob());
    assert_eq!(bal_after - bal_before, U256::from(1_000_000_000_000_000_000u64));
    assert_eq!(chain.state.nonce(&alice()), 1);
    let receipt = chain.receipt_for(&tx_hash).unwrap();
    assert!(receipt.status);
    assert_eq!(receipt.gas_used, 21_000);
    assert_eq!(chain.latest_header().number, 1);
    assert_eq!(chain.latest_header().parent_hash, chain.block_hash(0).unwrap());
}

#[test]
fn eip1559_transfer() {
    let mut chain = new_chain();
    let kp = dev_keypair(0);
    let tx = sign_transaction(
        &UnsignedTx {
            max_fee_per_blob_gas: Default::default(),
            blob_versioned_hashes: vec![],
            tx_type: TxType::Eip1559,
            chain_id: 31337,
            nonce: 0,
            gas_price: U256::zero(),
            max_fee: U256::from(5_000_000_000u64),
            max_priority_fee: U256::from(1_000_000_000u64),
            gas_limit: 21_000,
            to: Some(bob()),
            value: U256::from(500_000_000_000_000_000u64),
            data: vec![],
            access_list: vec![],
        },
        &kp,
    )
    .unwrap();
    chain.mine_block(vec![tx]).unwrap();
    let receipt = chain.latest_block_receipts(0);
    assert!(receipt.status);
    assert_eq!(chain.state.nonce(&alice()), 1);
}

#[test]
fn deploy_and_call_contract_on_chain() {
    let mut chain = new_chain();
    let kp = dev_keypair(0);

    let runtime = rrx_util::from_hex("602a60005260206000f3").unwrap();
    let mut init = rrx_util::from_hex("600a600c600039600a6000f3").unwrap();
    init.extend_from_slice(&runtime);


    let signed = sign_transaction(
        &UnsignedTx {
            max_fee_per_blob_gas: Default::default(),
            blob_versioned_hashes: vec![],
            tx_type: TxType::Legacy,
            chain_id: 31337,
            nonce: 0,
            gas_price: U256::from(2_000_000_000u64),
            max_fee: U256::zero(),
            max_priority_fee: U256::zero(),
            gas_limit: 100_000,
            to: None,
            value: U256::zero(),
            data: init,
            access_list: vec![],
        },
        &kp,
    )
    .unwrap();
    let tx_hash = signed.hash();
    chain.mine_block(vec![signed]).unwrap();
    let receipt = chain.receipt_for(&tx_hash).unwrap();
    assert!(receipt.status, "deploy failed: {:?}", receipt.output);
    let created = receipt.contract_address.unwrap();
    assert_eq!(chain.state.code(&created), runtime);

    let res = chain.call(alice(), Some(created), U256::zero(), vec![], 100_000, false);
    assert!(res.is_success());
    let mut expected = [0u8; 32];
    expected[31] = 42;
    assert_eq!(res.output, expected);
}

#[test]
fn base_fee_updates_per_eip1559() {
    let mut chain = new_chain();
    let kp = dev_keypair(0);
    let base0 = chain.latest_header().base_fee_per_gas;
    let n_txs = (chain.config.gas_limit / 2 / 21_000) as usize + 1; // just over target
    let mut txs = vec![];
    for i in 0..n_txs {
        txs.push(
            sign_transaction(
                &UnsignedTx {
            max_fee_per_blob_gas: Default::default(),
            blob_versioned_hashes: vec![],
                    tx_type: TxType::Legacy,
                    chain_id: 31337,
                    nonce: i as u64,
                    gas_price: base0 * U256::from(4u8),
                    max_fee: U256::zero(),
                    max_priority_fee: U256::zero(),
                    gas_limit: 21_000,
                    to: Some(bob()),
                    value: U256::zero(),
                    data: vec![],
                    access_list: vec![],
                },
                &kp,
            )
            .unwrap(),
        );
    }
    chain.mine_block(txs).unwrap();
    // header base fee is the one USED in this block; the EIP-1559 update applies to the next block
    let header_base = chain.latest_header().base_fee_per_gas;
    let used = 21_000u64 * n_txs as u64;
    let target = chain.config.gas_limit / 2;
    let expected = header_base
        + ((U256::from(used - target) * header_base / U256::from(target)) / U256::from(8u8)).max(U256::one());
    assert!(used > target);
    assert_eq!(chain.next_base_fee(), expected);
}

#[test]
fn abi_encode_call_args() {
    let data = abi::encode(
        &["address", "uint256"],
        &[
            abi::AbiValue::Address(bob()),
            abi::AbiValue::Uint(U256::from(123u8)),
        ],
    )
    .unwrap();
    let mut call = vec![0xa9, 0x05, 0x9c, 0xbb];
    call.extend(data);
    assert_eq!(call.len(), 4 + 64);
    let selector = abi::selector("transfer", &["address", "uint256"]);
    assert_eq!(call[..4], selector);
    let _ = u256_from_str("1000000");
    let _ = h160_from_hex("0000000000000000000000000000000000000001").unwrap();
}
