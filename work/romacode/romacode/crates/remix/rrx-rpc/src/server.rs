//! A real local JSON-RPC node: serves our chain over HTTP with the standard eth_*
//! methods. Tools like wallets, Remix or ethers-js can connect directly.

use std::sync::{Arc, Mutex};

use rrx_chain::dev::dev_keypair;
use rrx_chain::tx::{sign_transaction, TxType, UnsignedTx, Transaction};
use rrx_chain::{Chain, ChainConfig};
use rrx_evm::Host;
use rrx_util::{h160_from_hex, u256_from_str, H160, U256};
use serde_json::{json, Value};
use tiny_http::{Method, Response, Server};

pub struct NodeOptions {
    pub port: u16,
    pub chain_id: u64,
    pub fund_dev_accounts: bool,
}

pub fn serve(opts: NodeOptions) -> Result<(), String> {
    let mut alloc = vec![];
    let mut keys = std::collections::HashMap::new();
    if opts.fund_dev_accounts {
        for i in 0..10 {
            let kp = dev_keypair(i);
            let addr = kp.address().unwrap();
            keys.insert(format!("{addr:?}"), kp);
            alloc.push((addr, U256::from_dec_str("10000000000000000000000").unwrap()));
        }
    }
    let config = ChainConfig {
        chain_id: opts.chain_id,
        ..Default::default()
    };
    let chain = Chain::genesis(config, &alloc);
    let shared = Arc::new(Mutex::new(chain));

    let url = format!("0.0.0.0:{}", opts.port);
    let server = Server::http(&url).map_err(|e| e.to_string())?;
    println!("rrx node listening on http://127.0.0.1:{} (chain id {})", opts.port, opts.chain_id);
    println!("dev accounts (prefunded):");
    for (a, _) in alloc.iter().take(3) {
        println!("  {a:?}");
    }

    for mut request in server.incoming_requests() {
        if request.method() != &Method::Post {
            let _ = request.respond(Response::from_string("rrx node").with_status_code(200));
            continue;
        }
        let mut body = String::new();
                request.as_reader().read_to_string(&mut body).ok();
        let resp = handle(&shared, &keys, &body);
        let response = Response::from_string(resp).with_header(
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap(),
        );
        let _ = request.respond(response);
    }
    Ok(())
}

fn err_obj(code: i64, msg: &str) -> Value {
    json!({ "code": code, "message": msg })
}

fn handle(shared: &Arc<Mutex<Chain>>, keys: &std::collections::HashMap<String, rrx_crypto::KeyPair>, body: &str) -> String {
    let req: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return json!({ "jsonrpc": "2.0", "id": null, "error": err_obj(-32700, "parse error") }).to_string(),
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!([]));
    let result = dispatch(shared, keys, method, params);
    match result {
        Ok(v) => json!({ "jsonrpc": "2.0", "id": id, "result": v }).to_string(),
        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": err_obj(-32000, &e) }).to_string(),
    }
}

fn addr_of(v: &Value) -> Result<H160, String> {
    let s = v.as_str().ok_or("expected address string")?;
    h160_from_hex(s)
}

fn dispatch(
    shared: &Arc<Mutex<Chain>>,
    keys: &std::collections::HashMap<String, rrx_crypto::KeyPair>,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let arr = params.as_array().cloned().unwrap_or_default();
    let mut chain = shared.lock().unwrap();
    match method {
        "web3_clientVersion" => Ok(json!("rrx/v0.1.0")),
        "net_version" => Ok(json!(chain.config.chain_id.to_string())),
        "eth_chainId" => Ok(json!(format!("0x{:x}", chain.config.chain_id))),
        "eth_blockNumber" => Ok(json!(format!("0x{:x}", chain.latest_header().number))),
        "eth_gasPrice" => Ok(json!(format!("0x{:x}", chain.latest_header().base_fee_per_gas))),
        "eth_accounts" => {
            let list: Vec<String> = keys
                .keys()
                .map(|k| format!("0x{}", k.trim_start_matches("0x").to_ascii_lowercase()))
                .collect();
            Ok(json!(list))
        }
        "eth_getBalance" => {
            let a = addr_of(arr.first().ok_or("missing addr")?)?;
            let tag = arr.get(1).and_then(|v| v.as_str()).unwrap_or("latest");
            let bal = if tag == "latest" || tag == "pending" {
                chain.state.balance(&a)
            } else {
                chain.state.balance(&a)
            };
            Ok(json!(format!("0x{:x}", bal)))
        }
        "eth_getTransactionCount" => {
            let a = addr_of(arr.first().ok_or("missing addr")?)?;
            Ok(json!(format!("0x{:x}", chain.state.nonce(&a))))
        }
        "eth_getCode" => {
            let a = addr_of(arr.first().ok_or("missing addr")?)?;
            Ok(json!(rrx_util::to_hex_prefixed(&chain.state.code(&a))))
        }
        "eth_call" => {
            let obj = arr.first().ok_or("missing tx object")?;
            let to = obj.get("to").map(|v| addr_of(v)).transpose()?.unwrap_or_default();
            let from = match obj.get("from") {
                Some(v) => Some(addr_of(v)?),
                None => None,
            };
            let data = rrx_util::from_hex(obj.get("data").or_else(|| obj.get("input")).and_then(|d| d.as_str()).unwrap_or("0x"))
                .map_err(|e| e.to_string())?;
            let gas = obj
                .get("gas")
                .and_then(|g| g.as_str())
                .and_then(|g| u256_from_str(g).ok())
                .map(|g| g.as_u64())
                .unwrap_or(chain.config.gas_limit);
            let res = chain.call(from.unwrap_or_default(), Some(to), U256::zero(), data, gas, false);
            if res.is_success() {
                Ok(json!(rrx_util::to_hex_prefixed(&res.output)))
            } else {
                Err(match res.revert_reason() {
                    Some(r) => format!("execution reverted: {r}"),
                    None => "execution reverted".into(),
                })
            }
        }
        "eth_estimateGas" => {
            let obj = arr.first().ok_or("missing tx object")?;
            let to = match obj.get("to") {
                Some(v) => Some(addr_of(v)?),
                None => None,
            };
            let from = match obj.get("from") {
                Some(v) => Some(addr_of(v)?),
                None => Some(H160::from_low_u64_be(1)),
            };
            let data = rrx_util::from_hex(obj.get("data").or_else(|| obj.get("input")).and_then(|d| d.as_str()).unwrap_or("0x"))
                .map_err(|e| e.to_string())?;
            let est = chain.estimate_gas(from.unwrap_or_else(|| H160::from_low_u64_be(1)), to, U256::zero(), data)?;
            Ok(json!(format!("0x{:x}", est)))
        }
        "eth_feeHistory" => {
            let n = arr.first().and_then(|v| v.as_str()).and_then(|s| u256_from_str(s).ok()).map(|v| v.as_u64()).unwrap_or(1);
            if n == 0 {
                // avoid the `n - 1` underflow; geth returns empty result arrays
                return Ok(json!({ "baseFeePerGas": [], "gasUsedRatio": [], "reward": Value::Null }));
            }
            let mut base = vec![];
            let latest = chain.latest_header().number;
            for i in (latest.saturating_sub(n - 1))..=latest {
                if let Some(b) = chain.block_by_number(i) {
                    base.push(json!(format!("0x{:x}", b.header.base_fee_per_gas)));
                }
            }
            let next = chain.next_base_fee();
            base.push(json!(format!("0x{:x}", next)));
            // allocation bounded by the number of blocks actually collected
            let ratios = base.len().saturating_sub(1);
            Ok(json!({ "baseFeePerGas": base, "gasUsedRatio": vec![0.5f64; ratios], "reward": Value::Null }))
        }
        "eth_sendRawTransaction" => {
            let raw = rrx_util::from_hex(arr.first().and_then(|v| v.as_str()).ok_or("missing raw")?)
                .map_err(|e| e.to_string())?;
            let hash = chain.send_raw_transaction(&raw)?;
            Ok(json!(format!("0x{:x}", hash)))
        }
        "eth_sendTransaction" => {
            // dev-node convenience: sign with the unlocked dev account
            let obj = arr.first().ok_or("missing tx object")?;
            let from = addr_of(obj.get("from").ok_or("missing from")?)?;
            let kp = keys
                .get(&format!("{from:?}"))
                .ok_or("account not unlocked")?;
            let to = match obj.get("to") {
                Some(v) => Some(addr_of(v)?),
                None => None,
            };
            let value = obj
                .get("value")
                .and_then(|v| v.as_str())
                .and_then(|s| u256_from_str(s).ok())
                .unwrap_or_default();
            let data = rrx_util::from_hex(obj.get("data").or_else(|| obj.get("input")).and_then(|d| d.as_str()).unwrap_or("0x"))
                .map_err(|e| e.to_string())?;
            let gas_limit = obj
                .get("gas")
                .and_then(|g| g.as_str())
                .and_then(|g| u256_from_str(g).ok())
                .map(|g| g.as_u64())
                .unwrap_or(600_000);
            let unsigned = UnsignedTx {
                max_fee_per_blob_gas: Default::default(),
                blob_versioned_hashes: vec![],
                tx_type: TxType::Eip1559,
                chain_id: chain.config.chain_id,
                nonce: chain.state.nonce(&from),
                gas_price: U256::zero(),
                max_fee: chain.latest_header().base_fee_per_gas * U256::from(2u8),
                max_priority_fee: U256::from(1_000_000_000u64),
                gas_limit,
                to,
                value,
                data,
                access_list: vec![],
            };
            let signed = sign_transaction(&unsigned, kp)?;
            let hash = signed.hash();
            chain.mine_block(vec![signed])?;
            Ok(json!(format!("0x{:x}", hash)))
        }
        "eth_getTransactionReceipt" => {
            let h = rrx_util::h256_from_hex(arr.first().and_then(|v| v.as_str()).ok_or("missing hash")?)?;
            match chain.receipt_for(&h) {
                Some(r) => Ok(json!({
                    "transactionHash": format!("0x{:x}", r.tx_hash),
                    "blockNumber": format!("0x{:x}", r.block_number),
                    "status": format!("0x{:x}", r.status as u64),
                    "gasUsed": format!("0x{:x}", r.gas_used),
                    "effectiveGasPrice": format!("0x{:x}", r.effective_gas_price),
                    "contractAddress": r.contract_address.map(|a| json!(format!("0x{:x}", a))),
                    "logs": r.logs.iter().map(|l| json!({
                        "address": format!("0x{:x}", l.address),
                        "topics": l.topics.iter().map(|t| json!(format!("0x{:x}", t))).collect::<Vec<_>>(),
                        "data": rrx_util::to_hex_prefixed(&l.data),
                    })).collect::<Vec<_>>(),
                })),
                None => Ok(Value::Null),
            }
        }
        "eth_getBlockByNumber" => {
            let tag = arr.first().and_then(|v| v.as_str()).unwrap_or("latest");
            let n = match tag {
                "latest" => chain.latest_header().number,
                "earliest" => 0,
                s => u256_from_str(s)?.as_u64(),
            };
            match chain.block_by_number(n) {
                Some(b) => Ok(json!({
                    "number": format!("0x{:x}", b.header.number),
                    "hash": format!("0x{:x}", b.header.hash()),
                    "parentHash": format!("0x{:x}", b.header.parent_hash),
                    "stateRoot": format!("0x{:x}", b.header.state_root),
                    "transactionsRoot": format!("0x{:x}", b.header.transactions_root),
                    "gasLimit": format!("0x{:x}", b.header.gas_limit),
                    "gasUsed": format!("0x{:x}", b.header.gas_used),
                    "baseFeePerGas": format!("0x{:x}", b.header.base_fee_per_gas),
                    "timestamp": format!("0x{:x}", b.header.timestamp),
                    "transactions": b.transactions.iter().map(|t: &Transaction| json!(format!("0x{:x}", t.hash()))).collect::<Vec<_>>(),
                })),
                None => Ok(Value::Null),
            }
        }
        "evm_mine" => {
            let pending = chain.pending.drain(..).collect();
            let hash = chain.mine_block(pending)?;
            Ok(json!(format!("0x{:x}", hash)))
        }
        "rrx_stateRoot" => Ok(json!(format!("0x{:x}", chain.state.state_root()))),
        _ => Err(format!("method {method} not supported")),
    }
}
