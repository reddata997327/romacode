//! JSON-RPC 2.0 client for REAL Ethereum networks (mainnet, sepolia, any endpoint).

use rrx_util::{h256_from_hex, u256_from_str, H160, H256, U256};
use serde_json::{json, Value};

#[derive(Clone)]
pub struct Provider {
    pub url: String,
}

pub type PResult<T> = Result<T, String>;

impl Provider {
    pub fn new(url: &str) -> Provider {
        Provider { url: url.to_string() }
    }

    pub fn raw_request(&self, method: &str, params: Value) -> PResult<Value> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let resp = ureq::post(&self.url)
            .timeout(std::time::Duration::from_secs(60))
            .send_json(body)
            .map_err(|e| format!("rpc request failed: {e}"))?;
        let v: Value = resp.into_json().map_err(|e| e.to_string())?;
        if let Some(err) = v.get("error") {
            return Err(format!(
                "rpc error: {}",
                err.get("message").and_then(|m| m.as_str()).unwrap_or("?")
            ));
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    pub fn chain_id(&self) -> PResult<u64> {
        let v = self.raw_request("eth_chainId", json!([]))?;
        Ok(u256_from_str(v.as_str().unwrap_or("0x0"))?.as_u64())
    }

    pub fn block_number(&self) -> PResult<u64> {
        let v = self.raw_request("eth_blockNumber", json!([]))?;
        Ok(u256_from_str(v.as_str().unwrap_or("0x0"))?.as_u64())
    }

    pub fn gas_price(&self) -> PResult<U256> {
        let v = self.raw_request("eth_gasPrice", json!([]))?;
        u256_from_str(v.as_str().unwrap_or("0x0"))
    }

    /// EIP-1559 fee history: returns (base fees, priority-fee rewards).
    pub fn fee_history(&self, blocks: u64) -> PResult<(Vec<U256>, Vec<Vec<U256>>)> {
        let v = self.raw_request(
            "eth_feeHistory",
            json!([format!("0x{blocks:x}"), "latest", [25, 50, 75]]),
        )?;
        let base = v["baseFeePerGas"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().and_then(|s| u256_from_str(s).ok())).collect())
            .unwrap_or_default();
        let rewards = v["reward"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|row| {
                        row.as_array()
                            .map(|r| {
                                r.iter()
                                    .filter_map(|x| x.as_str().map(u256_from_str))
                                    .filter_map(|x| x.ok())
                                    .collect()
                            })
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok((base, rewards))
    }

    pub fn balance(&self, addr: &H160, block: &str) -> PResult<U256> {
        let v = self.raw_request("eth_getBalance", json!([to_hex_addr(addr), block]))?;
        u256_from_str(v.as_str().unwrap_or("0x0"))
    }

    pub fn nonce(&self, addr: &H160, block: &str) -> PResult<u64> {
        let v = self.raw_request("eth_getTransactionCount", json!([to_hex_addr(addr), block]))?;
        Ok(u256_from_str(v.as_str().unwrap_or("0x0"))?.as_u64())
    }

    pub fn code(&self, addr: &H160, block: &str) -> PResult<Vec<u8>> {
        let v = self.raw_request("eth_getCode", json!([to_hex_addr(addr), block]))?;
        rrx_util::from_hex(v.as_str().unwrap_or("0x")).map_err(|e| e.to_string())
    }

    /// eth_call
    pub fn call(&self, from: Option<H160>, to: &H160, data: &[u8], block: &str) -> PResult<Vec<u8>> {
        let mut obj = json!({ "to": to_hex_addr(to), "data": rrx_util::to_hex_prefixed(data) });
        if let Some(f) = from {
            obj["from"] = json!(to_hex_addr(&f));
        }
        let v = self.raw_request("eth_call", json!([obj, block]))?;
        rrx_util::from_hex(v.as_str().unwrap_or("0x")).map_err(|e| e.to_string())
    }

    /// eth_call that distinguishes SUCCESS data from REVERT data (some
    /// contracts — e.g. ERC-4337 EntryPoint simulateValidation — return their
    /// result BY reverting with ABI-encoded data).
    pub fn call_any(
        &self,
        from: Option<H160>,
        to: &H160,
        data: &[u8],
        block: &str,
    ) -> Result<Result<Vec<u8>, Vec<u8>>, String> {
        let mut obj = json!({ "to": to_hex_addr(to), "data": rrx_util::to_hex_prefixed(data) });
        if let Some(f) = from {
            obj["from"] = json!(to_hex_addr(&f));
        }
        let body = json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "eth_call",
            "params": [obj, block],
        });
        let resp = ureq::post(&self.url)
            .timeout(std::time::Duration::from_secs(60))
            .send_json(body)
            .map_err(|e| format!("rpc request failed: {e}"))?;
        let v: Value = resp.into_json().map_err(|e| e.to_string())?;
        if let Some(err) = v.get("error") {
            if let Some(d) = err.get("data").and_then(|d| d.as_str()) {
                let bytes = rrx_util::from_hex(d.trim_start_matches("0x"))
                    .map_err(|e| e.to_string())?;
                return Ok(Err(bytes));
            }
            return Err(format!(
                "rpc error: {}",
                err.get("message").and_then(|m| m.as_str()).unwrap_or("?")
            ));
        }
        let out = v.get("result").cloned().unwrap_or(Value::Null);
        let bytes = rrx_util::from_hex(out.as_str().unwrap_or("0x")).map_err(|e| e.to_string())?;
        Ok(Ok(bytes))
    }

    pub fn estimate_gas(&self, from: Option<H160>, to: Option<&H160>, data: &[u8], value: U256) -> PResult<u64> {
        let mut obj = json!({ "data": rrx_util::to_hex_prefixed(data), "value": format!("0x{:x}", value) });
        if let Some(f) = from {
            obj["from"] = json!(to_hex_addr(&f));
        }
        if let Some(t) = to {
            obj["to"] = json!(to_hex_addr(t));
        }
        let v = self.raw_request("eth_estimateGas", json!([obj]))?;
        Ok(u256_from_str(v.as_str().unwrap_or("0x0"))?.as_u64())
    }

    pub fn send_raw_transaction(&self, raw: &[u8]) -> PResult<H256> {
        let v = self.raw_request(
            "eth_sendRawTransaction",
            json!([rrx_util::to_hex_prefixed(raw)]),
        )?;
        h256_from_hex(v.as_str().unwrap_or(""))
    }

    pub fn transaction_receipt(&self, hash: &H256) -> PResult<Option<Value>> {
        let v = self.raw_request("eth_getTransactionReceipt", json!([to_hex_h256(hash)]))?;
        Ok(if v.is_null() { None } else { Some(v) })
    }

    pub fn block_by_number(&self, n: u64, full: bool) -> PResult<Option<Value>> {
        let v = self.raw_request("eth_getBlockByNumber", json!([format!("0x{n:x}"), full]))?;
        Ok(if v.is_null() { None } else { Some(v) })
    }

    /// Block by tag: "latest" | "safe" | "finalized" | "pending" | "earliest" | decimal number.
    pub fn block_by_tag(&self, tag: &str, full: bool) -> PResult<Option<Value>> {
        let param = if tag.chars().all(|c| c.is_ascii_digit()) {
            let n: u64 = tag.parse().map_err(|_| format!("bad block number '{tag}'"))?;
            format!("0x{n:x}")
        } else {
            tag.to_string()
        };
        let v = self.raw_request("eth_getBlockByNumber", json!([param, full]))?;
        Ok(if v.is_null() { None } else { Some(v) })
    }

    pub fn client_version(&self) -> PResult<String> {
        let v = self.raw_request("web3_clientVersion", json!([]))?;
        Ok(v.as_str().unwrap_or("unknown").to_string())
    }

    /// Full transaction object by hash.
    pub fn get_transaction(&self, hash: &H256) -> PResult<Option<Value>> {
        let v = self.raw_request("eth_getTransactionByHash", json!([to_hex_h256(hash)]))?;
        Ok(if v.is_null() { None } else { Some(v) })
    }

    /// debug_traceTransaction with the callTracer (per-call gas tree).
    pub fn trace_call_tree(&self, hash: &H256) -> PResult<Value> {
        let v = self.raw_request(
            "debug_traceTransaction",
            json!([to_hex_h256(hash), { "tracer": "callTracer" }]),
        )?;
        Ok(v)
    }

    /// eth_getStorageAt — a raw storage slot of a contract at a block.
    pub fn storage_at(&self, addr: &H160, slot: &U256, block: &str) -> PResult<U256> {
        let v = self.raw_request(
            "eth_getStorageAt",
            json!([to_hex_addr(addr), format!("0x{:0>64x}", slot), block]),
        )?;
        u256_from_str(v.as_str().unwrap_or("0x0"))
    }

    /// eth_getLogs — filter by contract, topics and block range.
    pub fn get_logs(
        &self,
        address: Option<&H160>,
        topics: &[String],
        from_block: &str,
        to_block: &str,
    ) -> PResult<Vec<Value>> {
        let mut filter = serde_json::Map::new();
        if let Some(a) = address {
            filter.insert("address".into(), json!(to_hex_addr(a)));
        }
        if !topics.is_empty() {
            filter.insert("topics".into(), json!(topics));
        }
        filter.insert("fromBlock".into(), json!(from_block));
        filter.insert("toBlock".into(), json!(to_block));
        let v = self.raw_request("eth_getLogs", json!([filter]))?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    /// eth_getProof — Merkle-Patricia account + storage proofs (archive nodes only).
    pub fn get_proof(&self, addr: &H160, slots: &[U256], block: &str) -> PResult<Value> {
        let slot_hex: Vec<String> = slots.iter().map(|s| format!("0x{:0>64x}", s)).collect();
        let v = self.raw_request(
            "eth_getProof",
            json!([to_hex_addr(addr), slot_hex, block]),
        )?;
        Ok(v)
    }

    /// eth_syncing — null when in sync, object with progress when catching up.
    pub fn syncing(&self) -> PResult<Option<Value>> {
        let v = self.raw_request("eth_syncing", json!([]))?;
        Ok(if v.is_null() { None } else { Some(v) })
    }
}

fn to_hex_addr(a: &H160) -> String {
    format!("0x{}", rrx_util::to_hex(a.as_bytes()))
}

fn to_hex_h256(h: &H256) -> String {
    format!("0x{}", rrx_util::to_hex(h.as_bytes()))
}


