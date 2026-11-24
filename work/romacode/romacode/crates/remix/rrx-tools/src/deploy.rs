//! Deploy & Run Transactions tool: real deployments and calls via local chain or RPC.

use std::path::Path;

use rrx_chain::dev::{dev_address, dev_keypair};
use rrx_chain::tx::{sign_transaction, TxType, UnsignedTx};
use rrx_chain::Chain;
use rrx_evm::Host;
use rrx_util::{abi, u256_from_str, H160, U256};
use serde_json::json;

pub enum AccountSpec {
    Dev(usize),
    Key(String),
}

impl AccountSpec {
    pub fn address(&self) -> Result<H160, String> {
        match self {
            AccountSpec::Dev(i) => Ok(dev_address(*i)),
            AccountSpec::Key(k) => rrx_crypto::KeyPair::from_hex(k)?.address(),
        }
    }

    pub fn keypair(&self) -> Result<rrx_crypto::KeyPair, String> {
        match self {
            AccountSpec::Dev(i) => Ok(dev_keypair(*i)),
            AccountSpec::Key(k) => rrx_crypto::KeyPair::from_hex(k),
        }
    }
}

pub struct DeployOutcome {
    pub contract: String,
    pub address: H160,
    pub tx_hash: rrx_util::H256,
    pub gas_used: u64,
    pub block_number: u64,
    pub deployer: H160,
}

/// Deploy a compiled contract on the LOCAL chain with constructor args.
#[allow(clippy::too_many_arguments)]
pub fn deploy_local(
    workspace: &Path,
    chain: &mut Chain,
    artifact: &serde_json::Value,
    contract_name: &str,
    account: &AccountSpec,
    ctor_args: &[&str],
    ctor_types: &[&str],
    value: U256,
) -> Result<DeployOutcome, String> {
    let _ = workspace;
    let kp = account.keypair()?;
    let sender = kp.address()?;
    let mut data = crate::compile::bytecode_of(artifact);
    if !ctor_args.is_empty() {
        let values = parse_args(ctor_args, ctor_types)?;
        let encoded = abi::encode(ctor_types, &values)?;
        data.extend_from_slice(&encoded);
    }
    let nonce = rrx_evm::Host::nonce(&chain.state, &sender);
    let tx = sign_transaction(
        &UnsignedTx {
                max_fee_per_blob_gas: Default::default(),
                blob_versioned_hashes: vec![],
            tx_type: TxType::Eip1559,
            chain_id: chain.config.chain_id,
            nonce,
            gas_price: U256::zero(),
            max_fee: chain.latest_header().base_fee_per_gas * U256::from(4u8),
            max_priority_fee: U256::from(1_000_000_000u64),
            gas_limit: 4_000_000,
            to: None,
            value,
            data,
            access_list: vec![],
        },
        &kp,
    )?;
    let hash = tx.hash();
    chain.mine_block(vec![tx])?;
    let receipt = chain.receipt_for(&hash).ok_or("no receipt")?;
    if !receipt.status {
        return Err(format!("deployment of {contract_name} reverted"));
    }
    Ok(DeployOutcome {
        contract: contract_name.to_string(),
        address: receipt.contract_address.unwrap_or_default(),
        tx_hash: hash,
        gas_used: receipt.gas_used,
        block_number: receipt.block_number,
        deployer: sender,
    })
}

/// Deploy via a REAL network through JSON-RPC.
#[allow(clippy::too_many_arguments)]
pub fn deploy_rpc(
    provider: &rrx_rpc::Provider,
    artifact: &serde_json::Value,
    account: &AccountSpec,
    ctor_args: &[&str],
    ctor_types: &[&str],
    value: U256,
    max_fee_gwei: u64,
    max_priority_gwei: u64,
) -> Result<DeployOutcome, String> {
    let kp = account.keypair()?;
    let sender = kp.address()?;
    let chain_id = provider.chain_id()?;
    let mut data = crate::compile::bytecode_of(artifact);
    if !ctor_args.is_empty() {
        let values = parse_args(ctor_args, ctor_types)?;
        let encoded = abi::encode(ctor_types, &values)?;
        data.extend_from_slice(&encoded);
    }
    let nonce = provider.nonce(&sender, "latest")?;
    let gas = provider.estimate_gas(Some(sender), None, &data, value)?;
    let tx = sign_transaction(
        &UnsignedTx {
                max_fee_per_blob_gas: Default::default(),
                blob_versioned_hashes: vec![],
            tx_type: TxType::Eip1559,
            chain_id,
            nonce,
            gas_price: U256::zero(),
            max_fee: U256::from(max_fee_gwei) * U256::from(1_000_000_000u64),
            max_priority_fee: U256::from(max_priority_gwei) * U256::from(1_000_000_000u64),
            gas_limit: gas + gas / 10,
            to: None,
            value,
            data,
            access_list: vec![],
        },
        &kp,
    )?;
    let hash = tx.hash();
    provider.send_raw_transaction(&tx.rlp_encode())?;
    // wait for the receipt
    for _ in 0..120 {
        if let Some(receipt) = provider.transaction_receipt(&hash)? {
            let status = receipt.get("status").and_then(|s| s.as_str()).unwrap_or("0x0");
            if status == "0x1" {
                let addr = receipt
                    .get("contractAddress")
                    .and_then(|a| a.as_str())
                    .and_then(|a| rrx_util::h160_from_hex(a).ok())
                    .unwrap_or_default();
                let gas_used = receipt
                    .get("gasUsed")
                    .and_then(|g| g.as_str())
                    .and_then(|g| u256_from_str(g).ok())
                    .map(|g| g.as_u64())
                    .unwrap_or(0);
                let block_number = receipt
                    .get("blockNumber")
                    .and_then(|g| g.as_str())
                    .and_then(|g| u256_from_str(g).ok())
                    .map(|g| g.as_u64())
                    .unwrap_or(0);
                return Ok(DeployOutcome {
                    contract: artifact["name"].as_str().unwrap_or("?").to_string(),
                    address: addr,
                    tx_hash: hash,
                    gas_used,
                    block_number,
                    deployer: sender,
                });
            }
            return Err("deployment reverted on remote network".into());
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    Err("timed out waiting for receipt".into())
}

/// Encode a function call and run it as a read (eth_call / chain.call).
pub fn call_read(
    chain: &Chain,
    from: H160,
    to: H160,
    function: &str,
    types: &[&str],
    args: &[&str],
    output_types: &[&str],
) -> Result<String, String> {
    let data = encode_call(function, types, args)?;
    let res = chain.call(from, Some(to), U256::zero(), data, 1_000_000, false);
    if !res.is_success() {
        return Err(res.revert_reason().unwrap_or_else(|| "reverted".into()));
    }
    format_output(output_types, &res.output)
}

/// Encode a function call and send it as a real transaction (state-changing).
pub fn call_write(
    chain: &mut Chain,
    account: &AccountSpec,
    to: H160,
    function: &str,
    types: &[&str],
    args: &[&str],
    value: U256,
) -> Result<(rrx_util::H256, u64), String> {
    let kp = account.keypair()?;
    let sender = kp.address()?;
    let data = encode_call(function, types, args)?;
    let nonce = chain.state.nonce(&sender);
    let tx = sign_transaction(
        &UnsignedTx {
                max_fee_per_blob_gas: Default::default(),
                blob_versioned_hashes: vec![],
            tx_type: TxType::Eip1559,
            chain_id: chain.config.chain_id,
            nonce,
            gas_price: U256::zero(),
            max_fee: chain.latest_header().base_fee_per_gas * U256::from(4u8),
            max_priority_fee: U256::from(1_000_000_000u64),
            gas_limit: 2_000_000,
            to: Some(to),
            value,
            data,
            access_list: vec![],
        },
        &kp,
    )?;
    let hash = tx.hash();
    chain.mine_block(vec![tx])?;
    let receipt = chain.receipt_for(&hash).ok_or("no receipt")?;
    if !receipt.status {
        return Err(abi::decode_revert_reason(&receipt.output)
            .unwrap_or_else(|| "transaction reverted".into()));
    }
    Ok((hash, receipt.gas_used))
}

pub fn encode_call(function: &str, types: &[&str], args: &[&str]) -> Result<Vec<u8>, String> {
    let values = parse_args(args, types)?;
    let sel = abi::selector(function, types);
    let mut data = sel.to_vec();
    data.extend(abi::encode(types, &values)?);
    Ok(data)
}

/// Parse CLI args into ABI values using expected types.
pub fn parse_args(args: &[&str], types: &[&str]) -> Result<Vec<abi::AbiValue>, String> {
    if args.len() != types.len() {
        return Err(format!("{} args given, expected {}", args.len(), types.len()));
    }
    let mut out = vec![];
    for (arg, ty) in args.iter().zip(types) {
        out.push(parse_single(arg, ty)?);
    }
    Ok(out)
}

pub fn parse_single(arg: &str, ty: &str) -> Result<abi::AbiValue, String> {
    let base = ty.split('[').next().unwrap_or(ty);
    if ty.contains('[') {
        // array literal like "1,2,3"
        let items: Vec<&str> = arg.trim_matches(|c| c == '[' || c == ']').split(',').filter(|s| !s.is_empty()).collect();
        let mut vals = vec![];
        for it in items {
            vals.push(parse_single(it, base)?);
        }
        return Ok(abi::AbiValue::Array(vals));
    }
    match base {
        "address" => Ok(abi::AbiValue::Address(rrx_util::h160_from_hex(arg)?)),
        "bool" => Ok(abi::AbiValue::Bool(matches!(
            arg.to_ascii_lowercase().as_str(),
            "true" | "1" | "yes"
        ))),
        "string" => Ok(abi::AbiValue::Str(arg.to_string())),
        "bytes" => Ok(abi::AbiValue::Bytes(rrx_util::from_hex(arg).map_err(|e| e.to_string())?)),
        "int" | "int256" => {
            let v = if let Some(stripped) = arg.strip_prefix('-') {
                U256::zero().overflowing_sub(rrx_util::u256_from_str(stripped)?).0
            } else {
                rrx_util::u256_from_str(arg)?
            };
            Ok(abi::AbiValue::Int(v))
        }
        _ if base.starts_with("bytes") => {
            let v = rrx_util::from_hex(arg).map_err(|e| e.to_string())?;
            Ok(abi::AbiValue::FixedBytes(v))
        }
        _ => Ok(abi::AbiValue::Uint(rrx_util::u256_from_str(arg)?)),
    }
}

/// Format raw return data using the ABI output types.
pub fn format_output(types: &[&str], data: &[u8]) -> Result<String, String> {
    if data.is_empty() {
        return Ok("".into());
    }
    if types.is_empty() {
        return Ok(format!("0x{}", rrx_util::to_hex(data)));
    }
    let vals = abi::decode(types, data)?;
    let strs: Vec<String> = vals.iter().map(format_value).collect();
    Ok(strs.join(", "))
}

pub fn format_value(v: &abi::AbiValue) -> String {
    match v {
        abi::AbiValue::Uint(u) | abi::AbiValue::Int(u) => u.to_string(),
        abi::AbiValue::Bool(b) => b.to_string(),
        abi::AbiValue::Address(a) => format!("{a:?}"),
        abi::AbiValue::FixedBytes(b) => rrx_util::to_hex_prefixed(b),
        abi::AbiValue::Bytes(b) => rrx_util::to_hex_prefixed(b),
        abi::AbiValue::Str(s) => s.clone(),
        abi::AbiValue::Array(items) => {
            let inner: Vec<String> = items.iter().map(format_value).collect();
            format!("[{}]", inner.join(", "))
        }
    }
}

/// Persist deployments (like Remix's deployed contracts panel).
pub fn save_deployment(
    root: &Path,
    outcome: &DeployOutcome,
    abi: &serde_json::Value,
    ctor_args: &[String],
    ctor_types: &[String],
) -> Result<(), String> {
    let path = root.join(".rrx").join("deployments.json");
    let mut list: Vec<serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    list.push(json!({
        "contract": outcome.contract,
        "address": format!("0x{:x}", outcome.address),
        "txHash": format!("0x{:x}", outcome.tx_hash),
        "gasUsed": outcome.gas_used,
        "block": outcome.block_number,
        "deployer": format!("0x{:x}", outcome.deployer),
        "ctorArgs": ctor_args,
        "ctorTypes": ctor_types,
        "abi": abi,
    }));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&list).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
