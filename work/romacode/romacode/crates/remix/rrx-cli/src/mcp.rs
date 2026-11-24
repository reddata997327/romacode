//! rrx MCP server — exposes every rrx tool over the Model Context Protocol.
//!
//! Transport: stdio, newline-delimited JSON-RPC 2.0 (the standard MCP stdio
//! transport used by Claude Desktop, Cursor, Cline, Zed, ...).
//!
//! Design: `tools/call` builds the equivalent `rrx ...` argv and executes the
//! same single binary (current_exe), so the MCP surface and the CLI surface can
//! never drift apart. Unsafe spend paths (send / mainnet deploy) require an
//! explicit `confirm: true` argument before `--yes` is passed.

use std::io::{BufRead, Write};
use std::process::Command;

use serde_json::{json, Map, Value};

const PROTOCOL_VERSION: &str = "2024-11-05";

// ---------------------------------------------------------------------------
// small schema helpers
// ---------------------------------------------------------------------------

fn s(desc: &str) -> Value {
    json!({"type": "string", "description": desc})
}
fn b(desc: &str) -> Value {
    json!({"type": "boolean", "description": desc})
}
fn n(desc: &str) -> Value {
    json!({"type": "number", "description": desc})
}
fn arr(desc: &str) -> Value {
    json!({"type": "array", "items": {"type": "string"}, "description": desc})
}
fn schema(props: Value, required: &[&str]) -> Value {
    let mut v = json!({"type": "object", "properties": props});
    if !required.is_empty() {
        v["required"] = json!(required);
    }
    v
}

struct Tool {
    name: &'static str,
    description: &'static str,
    input_schema: Value,
}

macro_rules! tool {
    ($name:literal, $desc:literal, $props:expr, $req:expr) => {
        Tool {
            name: $name,
            description: $desc,
            input_schema: schema($props, $req),
        }
    };
}

// ---------------------------------------------------------------------------
// tool registry (mirrors the rrx CLI 1:1; blocking servers excluded)
// ---------------------------------------------------------------------------

fn tools() -> Vec<Tool> {
    vec![
        tool!("rrx_plugins",
            "List every rrx tool (38 headless Remix tools) with usage hints",
            json!({}), &[]),
        tool!("workspace_init",
            "Create a new workspace from a Template Explorer template",
            json!({
                "template": s("template name (see workspace_list)"),
                "name": s("workspace / project name")
            }), &["template", "name"]),
        tool!("workspace_list",
            "List available workspace templates",
            json!({}), &[]),
        tool!("file_ls",
            "File Explorer: list files in the workspace directory",
            json!({"path": s("relative path (default: workspace root)")}), &[]),
        tool!("file_tree",
            "File Explorer: print the workspace file tree",
            json!({}), &[]),
        tool!("file_read",
            "File Explorer: read a file from the workspace",
            json!({"file": s("file path, relative to workspace root")}), &["file"]),
        tool!("file_write",
            "File Explorer: write (create or overwrite) a workspace file",
            json!({
                "file": s("file path"),
                "content": s("full file content to write")
            }), &["file", "content"]),
        tool!("file_delete",
            "File Explorer: remove a file or directory",
            json!({"path": s("path to remove")}), &["path"]),
        tool!("file_mkdir",
            "File Explorer: create a directory",
            json!({"path": s("directory to create")}), &["path"]),
        tool!("file_move",
            "File Explorer: move / rename a file",
            json!({"from": s("source path"), "to": s("destination path")}),
            &["from", "to"]),
        tool!("search_files",
            "Search for a pattern across all workspace files",
            json!({
                "pattern": s("text to search for"),
                "regex": b("treat the pattern as a regular expression (default false)")
            }), &["pattern"]),
        tool!("compile_contracts",
            "Solidity Compiler: compile all contracts in the workspace with real solc",
            json!({
                "solc_version": s("solc version, e.g. 0.8.24 (default: bundled)"),
                "no_opt": b("disable the optimizer (default false)"),
                "evm_version": s("EVM version, e.g. shanghai / paris (default shanghai)")
            }), &[]),
        tool!("static_analysis",
            "Solidity Static Analysis: run all security/quality checks on the AST",
            json!({}), &[]),
        tool!("run_tests",
            "Solidity Unit Testing: run every *_test.sol contract (real execution + gas)",
            json!({"gas_file": b("also emit per-test gas usage (default false)")}), &[]),
        tool!("deploy_contract",
            "Deploy & Run: deploy a contract (local dev chain by default, or a real network)",
            json!({
                "contract": s("contract artifact name, e.g. Storage"),
                "args": arr("constructor args, comma-separated string, e.g. \"100,hello\""),
                "account": n("local dev account index 0-9"),
                "key": s("explicit private key hex (or RRX_PRIVATE_KEY env)"),
                "value": s("ETH value sent with deployment"),
                "rpc": s("deploy to this JSON-RPC URL instead of the local chain"),
                "mainnet": b("deploy to Ethereum MAINNET with endpoint failover (default false)"),
                "confirm": b("REQUIRED true when mainnet=true — confirms real funds will be spent")
            }), &["contract"]),
        tool!("call_contract",
            "Read-only contract call (local chain by default, or a real network)",
            json!({
                "contract": s("deployed contract artifact name"),
                "function": s("function name, e.g. retrieve"),
                "args": arr("call arguments"),
                "rpc": s("run against this JSON-RPC URL"),
                "mainnet": b("run against Ethereum mainnet with failover (default false)")
            }), &["contract", "function"]),
        tool!("send_transaction",
            "Send a signed ETH transfer or state-changing call on a REAL network (mainnet by default)",
            json!({
                "to": s("recipient address or ENS name"),
                "value": s("amount in ETH, e.g. 0.01 (omit for pure contract call)"),
                "fn_sig": s("state-changing call signature, e.g. transfer(address,uint256)"),
                "args": arr("comma-separated args string for fn_sig"),
                "key": s("private key hex (or RRX_PRIVATE_KEY env)"),
                "rpc": s("custom JSON-RPC URL (default: mainnet failover list)"),
                "confirm": b("REQUIRED true — confirms broadcast; real funds will be spent")
            }), &["to", "confirm"]),
        tool!("debug_transaction",
            "Replay a REAL confirmed mainnet transaction step-by-step in the rrx EVM",
            json!({
                "hash": s("transaction hash 0x..."),
                "max_steps": n("cap on executed steps (default 0 = unlimited)"),
                "top": n("show top N hottest opcodes (default 12)")
            }), &["hash"]),
        tool!("debug_local",
            "Debugger: step through a local contract call with full opcode trace",
            json!({
                "contract": s("contract artifact name"),
                "function": s("function to execute"),
                "args": arr("function arguments"),
                "json": b("emit JSON trace instead of text (default false)")
            }), &["contract", "function"]),
        tool!("get_logs",
            "Live event logs (eth_getLogs) with Transfer/Approval decoding",
            json!({
                "address": s("filter by contract address"),
                "topic0": s("event signature filter, e.g. Transfer(address,address,uint256)"),
                "from": s("from block (default latest)"),
                "to": s("to block (default latest)"),
                "limit": n("max logs returned (default 10)")
            }), &[]),
        tool!("ens_resolve",
            "Real ENS resolution on the Ethereum mainnet registry",
            json!({
                "name": s("ENS name, e.g. vitalik.eth — or an address when reverse=true"),
                "reverse": b("reverse-resolve an address to its primary name (default false)")
            }), &["name"]),
        tool!("wildtoken_demo",
            "Run the WildToken demo: the spec-faithful 'Tether USD' lookalike (name USDT, 6 decimals, 1B supply, public mint, permissionless burn) compiled with real solc and executed on the real rrx EVM — deploy, mint by an attacker, burn from the deployer wallet. Safe: local EVM only, no keys, no broadcast",
            json!({}), &[]),
        tool!("fakeasset_registry",
            "Canonical USDT/USDC/WETH registry per chain; compare any address and get a CLONE / REAL / UNKNOWN verdict (ticker/decimals/name + live supply vs the real one)",
            json!({
                "compare": s("token address to check"),
                "chain": s("chain slug (ethereum, base, arbitrum, optimism, polygon, bsc) — default ethereum")
            }), &[]),
        tool!("fakeasset_audit",
            "Honeypot/fake-token audit: run a variant's full victim story locally (--variant: plain|honeypot|tax|freeze|capped|proxy|renounce-fake) OR probe a live address (--address, optional --from holder seat via eth_call impersonation + --chain)",
            json!({
                "variant": s("local fixture variant to audit"),
                "address": s("live token address to probe"),
                "chain": s("chain slug for --address mode"),
                "from": s("funded holder address for the live sell-path probe")
            }), &[]),
        tool!("fakeasset_verify",
            "Screenshot fact-check: read real tx facts (status, recipient, amount, confirmations, token transfers) from the chain and verdict claimed values TRUE/FALSE",
            json!({
                "txid": s("transaction hash"),
                "claims": arr("claimed k=v pairs: recipient=, amount=, status="),
                "chain": s("chain slug (default ethereum)")
            }), &["txid"]),
        tool!("fakeasset_demo",
            "The full one-scroll fake-asset story: clone deploy → 500M mint → 1000 burn → honeypot sell revert → registry CLONE verdict → flasher credit:NO → fabricated proof caught — all real execution, detection only",
            json!({}), &[]),
        tool!("fakeasset_flasher",
            "The deposit story, exchange ki aankh se: a fake 'USDT' deposit shows 50,000 in the wallet UI, but the exchange-side checklist (registry verdict, sell-path probe, supply sanity) returns credit: NO. Pass --address to run the same checklist against a live token",
            json!({
                "address": s("optional live token address (default: local clone story)"),
                "chain": s("chain slug for --address mode (default ethereum)"),
                "from": s("funded holder for the live sell-path probe")
            }), &[]),
        tool!("fakeasset_pending",
            "Pending/RBF replacement-risk check: read the real tx fields (type, fees, nonce) and report whether the tx was replaceable-by-fee while pending, the 10% bump floor, stuck risk, and why PENDING is never a credit",
            json!({
                "txid": s("transaction hash"),
                "chain": s("chain slug (default ethereum)")
            }), &["txid"]),
        tool!("fakeasset_variants",
            "List the fake-variant matrix: plain, honeypot, tax, freeze, capped, proxy, renounce-fake — the mechanic, who each one hurts, and whether the victim can exit (1000 bech kar nikal sakta hai?)",
            json!({}), &[]),
        tool!("token_info",
            "Live token inspection (ERC-20/721/1155) via real eth_call",
            json!({
                "address": s("token contract address"),
                "holder": s("holder address / ENS to also read the balance")
            }), &["address"]),
        tool!("fetch_verified_sources",
            "Fetch VERIFIED sources of a deployed contract (Sourcify / Etherscan V2)",
            json!({
                "address": s("deployed contract address"),
                "chain_id": n("chain id (default 1 = Ethereum mainnet)")
            }), &["address"]),
        tool!("state_proof",
            "Cryptographically verify account/storage state against a block state root (MPT proof)",
            json!({
                "address": s("account to prove"),
                "slot": arr("storage slots to prove (decimal or hex)"),
                "block": s("block tag (default latest)")
            }), &["address"]),
        tool!("utils_keccak",
            "keccak256 hash of a hex string or UTF-8 text",
            json!({"input": s("hex (0x...) or plain text")}), &["input"]),
        tool!("utils_unit",
            "Convert between wei / gwei / ether",
            json!({
                "amount": s("amount to convert"),
                "from": s("wei | gwei | ether"),
                "to": s("wei | gwei | ether")
            }), &["amount", "from", "to"]),
        tool!("utils_selector",
            "4-byte function selector of name(types)",
            json!({"signature": s("e.g. transfer(address,uint256)")}), &["signature"]),
        tool!("utils_namehash",
            "ENS namehash of a domain",
            json!({"name": s("e.g. vitalik.eth")}), &["name"]),
        tool!("gas_estimate",
            "Gas Fee: real execution-based estimate (intrinsic + per-opcode breakdown)",
            json!({
                "contract": s("contract artifact name"),
                "function": s("function to estimate"),
                "args": arr("function arguments")
            }), &["contract", "function"]),
        tool!("gas_report",
            "Gas Fee: per-contract/per-method report from the last test/script run",
            json!({"style": s("table | json (default table)")}), &[]),
        tool!("gas_trace",
            "Gas Fee: real per-opcode gas metering of a call",
            json!({
                "contract": s("contract artifact name"),
                "function": s("function to trace"),
                "args": arr("function arguments"),
                "top": n("show top N hottest opcodes (default 15)")
            }), &["contract", "function"]),
        tool!("gas_price_live",
            "Live network gas prices from Ethereum mainnet (auto failover)",
            json!({}), &[]),
        tool!("flatten_contract",
            "Contract Flattener: merge imports into one self-contained .sol file",
            json!({
                "file": s("entry Solidity file"),
                "out": s("output path (default prints to stdout)")
            }), &["file"]),
        tool!("generate_uml",
            "UML Generator: mermaid class diagram of every workspace contract",
            json!({}), &[]),
        tool!("generate_docs",
            "Doc Generator: markdown documentation (NatSpec + ABI) for the workspace",
            json!({"out": s("output file (default prints to stdout)")}), &[]),
        tool!("run_script",
            "Script Runner: execute a .rrx DSL script on the real chain",
            json!({"file": s("path to the .rrx script")}), &["file"]),
        tool!("git_op",
            "mini git for the workspace: init | add | commit | status | log",
            json!({
                "action": s("one of: init, add, commit, status, log"),
                "message": s("commit message (required for commit)")
            }), &["action"]),
        tool!("verify_contract",
            "Verify a contract on Etherscan V2 (default chain: Ethereum mainnet)",
            json!({
                "address": s("deployed contract address"),
                "contract": s("workspace contract artifact/source name"),
                "compiler_version": s("e.g. v0.8.24+commit.e11b9ed9"),
                "chain_id": n("chain id (default 1)"),
                "api_key": s("Etherscan API key (or ETHERSCAN_API_KEY env)")
            }), &["address", "contract", "compiler_version"]),
        tool!("keys_new",
            "Generate a fresh secp256k1 keypair (address + private key)",
            json!({}), &[]),
        tool!("keys_list",
            "List the workspace keyfile entries",
            json!({}), &[]),
        tool!("keys_dev",
            "Show the 10 deterministic dev accounts (test-only, funded on local chain)",
            json!({}), &[]),
        tool!("sign_message",
            "Sign / recover an EIP-191 personal message",
            json!({
                "message": s("message to sign"),
                "key": s("private key hex (or RRX_PRIVATE_KEY env)"),
                "account": n("local dev account index instead of --key")
            }), &["message"]),
        tool!("run_precompile",
            "Execute a real EVM precompile (1-9) on arbitrary input",
            json!({
                "address": n("precompile number 1-9 (1=ecrecover .. 9=blake2f)"),
                "input_hex": s("hex input (default empty)")
            }), &["address"]),
        tool!("solc_install",
            "Download & install a real solc version",
            json!({"version": s("e.g. 0.8.24 (default: latest release)")}), &[]),
        tool!("solc_list",
            "List installed solc versions",
            json!({}), &[]),
        tool!("net_status",
            "Live mainnet connectivity matrix — every endpoint probed (latency, block, client)",
            json!({}), &[]),
        tool!("chain_info",
            "Live chain dashboard: client version, latest block, base fee, priority fees",
            json!({"rpc": s("custom JSON-RPC URL (default mainnet failover)")}), &[]),
        tool!("tx_info",
            "Live transaction + receipt report from the real network",
            json!({"hash": s("transaction hash 0x...")}), &["hash"]),
        tool!("block_info",
            "Live block summary from the real network",
            json!({"number": s("block number, or latest / finalized")}), &["number"]),
        tool!("account_info",
            "Live account balance / nonce / code from the real network",
            json!({"address": s("address or ENS name")}), &["address"]),
        tool!("trace_tx",
            "Live call-tree trace of a confirmed transaction (callTracer)",
            json!({"hash": s("transaction hash 0x...")}), &["hash"]),
        tool!("eth_call",
            "Read-only eth_call against a real network with ABI encode/decode",
            json!({
                "to": s("contract address"),
                "function": s("function name, e.g. balanceOf"),
                "args": arr("call arguments"),
                "types": s("ABI input types, comma separated, e.g. address,uint256"),
                "returns": s("ABI output types, e.g. uint256"),
                "rpc": s("custom JSON-RPC URL (default mainnet failover)"),
                "block": s("block tag (default latest)")
            }), &["to", "function"]),
        // ---- advanced tier 1-4 tools ----
        tool!("chains_list",
            "Live status of every supported chain (Ethereum, Base, Arbitrum, OP, Polygon, BSC, Avalanche, Gnosis + testnets) — real probes",
            json!({"filter": s("optional slug filter")}), &[]),
        tool!("fork_state",
            "Download REAL chain state (code/balance/nonce/storage) of accounts into the local MPT-backed EVM for fork testing",
            json!({
                "accounts": arr("addresses or well-known names: usdt, usdc, weth, dai, entrypoint..."),
                "chain": s("chain slug (default ethereum)"),
                "block": s("block tag (default latest)"),
                "rpc": s("custom rpc url")
            }), &["accounts"]),
        tool!("fuzz_run",
            "Property-based fuzzing: random inputs execute on the real EVM; reverts are captured and shrunk to minimal counterexamples",
            json!({
                "contract": s("contract artifact name"),
                "function": s("function to fuzz"),
                "runs": n("number of runs (default 256)"),
                "seed": n("deterministic seed (default 1)")
            }), &["contract", "function"]),
        tool!("invariant_run",
            "Invariant testing: random call sequences across all state-changing functions, then the invariant_* view must hold",
            json!({
                "contract": s("contract artifact name"),
                "invariant": s("invariant view name (default invariant_<Contract>)"),
                "sequences": n("number of sequences (default 50)"),
                "depth": n("calls per sequence (default 10)"),
                "seed": n("deterministic seed")
            }), &["contract"]),
        tool!("reentrancy_scan",
            "Dynamic reentrancy scan: executes every state-changing function on the real EVM and flags external CALL followed by SSTORE in the same frame",
            json!({}), &[]),
        tool!("layout_check",
            "Storage layout collision check between two contracts (safe upgrade verification via real solc storageLayout)",
            json!({"old_contract": s("proxy/previous implementation"), "new_contract": s("new implementation")}),
            &["old_contract", "new_contract"]),
        tool!("bytecode_diff",
            "Diff the local build against LIVE on-chain bytecode fetched over eth_getCode",
            json!({
                "address": s("deployed contract address"),
                "contract": s("local contract name"),
                "rpc": s("custom rpc url (default mainnet failover)")
            }), &["address", "contract"]),
        tool!("coverage_run",
            "Function-level coverage measured from REAL EVM executions of the test suite",
            json!({}), &[]),
        tool!("index_blocks",
            "Index REAL blocks from a live chain into the local explorer store",
            json!({
                "blocks": n("how many recent blocks (default 20)"),
                "rpc": s("custom rpc url (default mainnet failover)")
            }), &[]),
        tool!("aa_simulate",
            "ERC-4337: run the REAL EntryPoint simulateValidation for a UserOperation via read-only eth_call on mainnet",
            json!({
                "sender": s("account abstraction sender address"),
                "call_data": s("hex call data for the inner call"),
                "nonce": s("omit to read the real nonce from EntryPoint"),
                "signature": s("hex signature (default empty for simulation)")
            }), &["sender", "call_data"]),
        tool!("blob_decode",
            "EIP-4844: decode a raw blob transaction (type 0x03) and show all fields incl. versioned hashes",
            json!({"raw": s("raw signed blob tx hex (with 0x03 prefix)")}), &["raw"]),
        tool!("blob_verify",
            "EIP-4844: decode the network wrapper (tx + sidecar) and cryptographically verify every blob with REAL BLS12-381 KZG pairing checks against the Ethereum trusted setup",
            json!({"raw": s("network wrapper hex: 0x03 || rlp([payload, commitments, blobs, proofs])")}), &["raw"]),
        tool!("blob_fetch",
            "Fetch a REAL transaction from mainnet and report its blob fields when it is a type-3 (EIP-4844) tx",
            json!({"txhash": s("transaction hash")}), &["txhash"]),
        tool!("verkle_key",
            "EIP-6800: derive the verkle tree key (stem + suffix) for a storage slot of an address",
            json!({"address": s("contract address"), "slot": n("storage slot (default 0)")}), &["address"]),
        tool!("verkle_demo",
            "Build a verkle trie over REAL storage slots of a live contract (real BLS12-381 Pedersen commitments, EIP-6800 rule) and verify a witness against the root",
            json!({"address": s("contract address (default USDT)"), "slots": n("how many slots (default 8)")}), &[]),
        tool!("ws_subscribe",
            "Live WebSocket subscription (eth_subscribe) over wss:// to REAL-time chain data: newHeads / logs / newPendingTransactions",
            json!({
                "chain": s("mainnet|base|arbitrum|optimism|polygon|bsc (default mainnet)"),
                "url": s("explicit wss:// url (overrides chain)"),
                "method": s("newHeads | logs | newPendingTransactions (default newHeads)"),
                "address": s("for logs: filter by contract address"),
                "max": n("stop after N events (default 3)"),
                "timeout": n("timeout seconds (default 90)")
            }), &[]),
        tool!("build_block",
            "Block builder: assemble a REAL block from live mempool txs executed on forked state in the rrx EVM — real tx root, receipts root, EIP-1559 base fee and header hash",
            json!({
                "max_txs": n("how many txs to include (default 10)"),
                "coinbase": s("builder address (optional)"),
                "include_blobs": {"type":"boolean","description":"include blob txs (default false)"}
            }), &[]),
    ]
}

// ---------------------------------------------------------------------------
// argv construction: MCP arguments -> rrx CLI argv
// ---------------------------------------------------------------------------

fn gs(args: &Map<String, Value>, key: &str) -> Option<String> {
    args.get(key).and_then(|v| match v {
        Value::String(sv) => Some(sv.clone()),
        Value::Number(nv) => Some(nv.to_string()),
        Value::Bool(bv) => Some(bv.to_string()),
        _ => None,
    })
}

fn gb(args: &Map<String, Value>, key: &str) -> bool {
    matches!(args.get(key), Some(Value::Bool(true)))
}

fn ga(args: &Map<String, Value>, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|x| match x {
                    Value::String(sv) => sv.clone(),
                    other => other.to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn need(args: &Map<String, Value>, key: &str) -> Result<String, String> {
    gs(args, key).ok_or_else(|| format!("missing required argument: {key}"))
}

fn spend_guard(args: &Map<String, Value>, what: &str) -> Result<(), String> {
    if gb(args, "confirm") {
        Ok(())
    } else {
        Err(format!(
            "{what} would broadcast a REAL transaction and spend real funds. \
             Re-run with argument \"confirm\": true to approve the broadcast."
        ))
    }
}

fn build_argv(name: &str, a: &Map<String, Value>) -> Result<Vec<String>, String> {
    let mut v: Vec<String> = Vec::new();
    match name {
        "rrx_plugins" => v.push("plugins".into()),
        "workspace_init" => {
            v.extend(["workspace".into(), "init".into(), need(a, "template")?, need(a, "name")?]);
        }
        "workspace_list" => v.extend(["workspace".into(), "list".into()]),
        "file_ls" => {
            v.push("file".into());
            v.push("ls".into());
            if let Some(p) = gs(a, "path") {
                v.push(p);
            }
        }
        "file_tree" => v.push("tree".into()),
        "file_read" => v.extend(["cat".into(), need(a, "file")?]),
        "file_write" => v.extend(["write".into(), need(a, "file")?, need(a, "content")?]),
        "file_delete" => v.extend(["rm".into(), need(a, "path")?]),
        "file_mkdir" => v.extend(["mkdir".into(), need(a, "path")?]),
        "file_move" => v.extend(["mv".into(), need(a, "from")?, need(a, "to")?]),
        "search_files" => {
            v.push("search".into());
            v.push(need(a, "pattern")?);
            if gb(a, "regex") {
                v.push("--regex".into());
            }
        }
        "compile_contracts" => {
            v.push("compile".into());
            if let Some(x) = gs(a, "solc_version") {
                v.extend(["--solc".into(), x]);
            }
            if gb(a, "no_opt") {
                v.push("--no-opt".into());
            }
            if let Some(x) = gs(a, "evm_version") {
                v.extend(["--evm-version".into(), x]);
            }
        }
        "static_analysis" => v.push("analyze".into()),
        "run_tests" => {
            v.push("test".into());
            if gb(a, "gas_file") {
                v.push("--gas-file".into());
            }
        }
        "deploy_contract" => {
            v.push("deploy".into());
            v.push(need(a, "contract")?);
            if let Some(x) = gs(a, "args") {
                v.extend(["--args".into(), x]);
            }
            if let Some(x) = gs(a, "account") {
                v.extend(["--account".into(), x]);
            }
            if let Some(x) = gs(a, "key") {
                v.extend(["--key".into(), x]);
            }
            if let Some(x) = gs(a, "value") {
                v.extend(["--value".into(), x]);
            }
            if let Some(x) = gs(a, "rpc") {
                v.extend(["--rpc".into(), x]);
            }
            if gb(a, "mainnet") {
                spend_guard(a, "mainnet deploy")?;
                v.extend(["--mainnet".into(), "--yes".into()]);
            }
        }
        "call_contract" => {
            v.push("call".into());
            v.push(need(a, "contract")?);
            v.push(need(a, "function")?);
            v.extend(ga(a, "args"));
            if let Some(x) = gs(a, "rpc") {
                v.extend(["--rpc".into(), x]);
            }
            if gb(a, "mainnet") {
                v.push("--mainnet".into());
            }
        }
        "send_transaction" => {
            spend_guard(a, "send")?;
            v.push("send".into());
            v.extend(["--to".into(), need(a, "to")?]);
            if let Some(x) = gs(a, "value") {
                v.extend(["--value".into(), x]);
            }
            if let Some(x) = gs(a, "fn_sig") {
                v.extend(["--fn-sig".into(), x]);
            }
            if let Some(x) = gs(a, "args") {
                v.extend(["--args".into(), x]);
            }
            if let Some(x) = gs(a, "key") {
                v.extend(["--key".into(), x]);
            }
            if let Some(x) = gs(a, "rpc") {
                v.extend(["--rpc".into(), x]);
            } else {
                v.push("--mainnet".into());
            }
            v.push("--yes".into());
        }
        "debug_transaction" => {
            v.push("debugtx".into());
            v.push(need(a, "hash")?);
            if let Some(x) = gs(a, "max_steps") {
                v.extend(["--max-steps".into(), x]);
            }
            if let Some(x) = gs(a, "top") {
                v.extend(["--top".into(), x]);
            }
        }
        "debug_local" => {
            v.push("debug".into());
            v.push(need(a, "contract")?);
            v.push(need(a, "function")?);
            v.extend(ga(a, "args"));
            if gb(a, "json") {
                v.push("--json".into());
            }
        }
        "get_logs" => {
            v.push("logs".into());
            if let Some(x) = gs(a, "address") {
                v.extend(["--address".into(), x]);
            }
            if let Some(x) = gs(a, "topic0") {
                v.extend(["--topic0".into(), x]);
            }
            if let Some(x) = gs(a, "from") {
                v.extend(["--from".into(), x]);
            }
            if let Some(x) = gs(a, "to") {
                v.extend(["--to".into(), x]);
            }
            if let Some(x) = gs(a, "limit") {
                v.extend(["--limit".into(), x]);
            }
        }
        "ens_resolve" => {
            v.push("ens".into());
            v.push(need(a, "name")?);
            if gb(a, "reverse") {
                v.push("--reverse".into());
            }
        }
        "wildtoken_demo" => {
            v.extend(["wildtoken".into(), "demo".into()]);
        }
        "fakeasset_registry" => {
            v.extend(["fakeasset".into(), "registry".into()]);
            if let Some(x) = gs(a, "compare") {
                v.extend(["--compare".into(), x]);
            }
            if let Some(x) = gs(a, "chain") {
                v.extend(["--chain".into(), x]);
            }
        }
        "fakeasset_audit" => {
            v.extend(["fakeasset".into(), "audit".into()]);
            if let Some(x) = gs(a, "variant") {
                v.extend(["--variant".into(), x]);
            }
            if let Some(x) = gs(a, "address") {
                v.extend(["--address".into(), x]);
            }
            if let Some(x) = gs(a, "chain") {
                v.extend(["--chain".into(), x]);
            }
            if let Some(x) = gs(a, "from") {
                v.extend(["--from".into(), x]);
            }
        }
        "fakeasset_verify" => {
            v.extend(["fakeasset".into(), "verify".into(), need(a, "txid")?]);
            if let Some(list) = a.get("claims").and_then(|c| c.as_array()) {
                for c in list.iter().filter_map(|c| c.as_str()) {
                    v.extend(["--claim".into(), c.to_string()]);
                }
            }
            if let Some(x) = gs(a, "chain") {
                v.extend(["--chain".into(), x]);
            }
        }
        "fakeasset_demo" => {
            v.extend(["fakeasset".into(), "demo".into()]);
        }
        "fakeasset_flasher" => {
            v.extend(["fakeasset".into(), "flasher".into()]);
            if let Some(x) = gs(a, "address") {
                v.extend(["--address".into(), x]);
            }
            if let Some(x) = gs(a, "chain") {
                v.extend(["--chain".into(), x]);
            }
            if let Some(x) = gs(a, "from") {
                v.extend(["--from".into(), x]);
            }
        }
        "fakeasset_pending" => {
            v.extend(["fakeasset".into(), "pending".into(), need(a, "txid")?]);
            if let Some(x) = gs(a, "chain") {
                v.extend(["--chain".into(), x]);
            }
        }
        "fakeasset_variants" => {
            v.extend(["fakeasset".into(), "variants".into()]);
        }
        "token_info" => {
            v.push("token".into());
            v.push(need(a, "address")?);
            if let Some(x) = gs(a, "holder") {
                v.extend(["--holder".into(), x]);
            }
        }
        "fetch_verified_sources" => {
            v.push("sources".into());
            v.push(need(a, "address")?);
            if let Some(x) = gs(a, "chain_id") {
                v.extend(["--chain-id".into(), x]);
            }
        }
        "state_proof" => {
            v.push("proof".into());
            v.push(need(a, "address")?);
            for slot in ga(a, "slot") {
                v.extend(["--slot".into(), slot]);
            }
            if let Some(x) = gs(a, "block") {
                v.extend(["--block".into(), x]);
            }
        }
        "utils_keccak" => v.extend(["utils".into(), "keccak".into(), need(a, "input")?]),
        "utils_unit" => {
            v.extend([
                "utils".into(),
                "unit".into(),
                need(a, "amount")?,
                need(a, "from")?,
                need(a, "to")?,
            ]);
        }
        "utils_selector" => v.extend(["utils".into(), "selector".into(), need(a, "signature")?]),
        "utils_namehash" => v.extend(["utils".into(), "namehash".into(), need(a, "name")?]),
        "gas_estimate" => {
            v.extend(["gas".into(), "estimate".into(), need(a, "contract")?, need(a, "function")?]);
            v.extend(ga(a, "args"));
        }
        "gas_report" => {
            v.extend(["gas".into(), "report".into()]);
            if let Some(x) = gs(a, "style") {
                v.extend(["--style".into(), x]);
            }
        }
        "gas_trace" => {
            v.extend(["gas".into(), "trace".into(), need(a, "contract")?, need(a, "function")?]);
            v.extend(ga(a, "args"));
            if let Some(x) = gs(a, "top") {
                v.extend(["--top".into(), x]);
            }
        }
        "gas_price_live" => v.push("gasprice".into()),
        "flatten_contract" => {
            v.push("flatten".into());
            v.push(need(a, "file")?);
            if let Some(x) = gs(a, "out") {
                v.extend(["--out".into(), x]);
            }
        }
        "generate_uml" => v.push("uml".into()),
        "generate_docs" => {
            v.push("doc".into());
            if let Some(x) = gs(a, "out") {
                v.extend(["--out".into(), x]);
            }
        }
        "run_script" => v.extend(["script".into(), need(a, "file")?]),
        "git_op" => {
            let action = need(a, "action")?;
            match action.as_str() {
                "init" | "add" | "status" | "log" => {}
                "commit" => {
                    if gs(a, "message").is_none() {
                        return Err("commit requires a message".into());
                    }
                }
                other => return Err(format!("unknown git action: {other} (init|add|commit|status|log)")),
            }
            v.push("git".into());
            v.push(action);
            if let Some(x) = gs(a, "message") {
                v.push(x);
            }
        }
        "verify_contract" => {
            v.push("verify".into());
            v.extend(["--address".into(), need(a, "address")?]);
            v.extend(["--contract".into(), need(a, "contract")?]);
            v.extend(["--compiler-version".into(), need(a, "compiler_version")?]);
            if let Some(x) = gs(a, "chain_id") {
                v.extend(["--chain-id".into(), x]);
            }
            if let Some(x) = gs(a, "api_key") {
                v.extend(["--api-key".into(), x]);
            }
        }
        "keys_new" => v.extend(["keys".into(), "new".into()]),
        "keys_list" => v.extend(["keys".into(), "list".into()]),
        "keys_dev" => v.extend(["keys".into(), "dev".into()]),
        "sign_message" => {
            v.push("sign".into());
            v.push(need(a, "message")?);
            if let Some(x) = gs(a, "key") {
                v.extend(["--key".into(), x]);
            }
            if let Some(x) = gs(a, "account") {
                v.extend(["--account".into(), x]);
            }
        }
        "run_precompile" => {
            v.push("precompiles".into());
            v.push(need(a, "address")?);
            v.push(gs(a, "input_hex").unwrap_or_default());
        }
        "solc_install" => {
            v.extend(["solc".into(), "install".into()]);
            if let Some(x) = gs(a, "version") {
                v.push(x);
            }
        }
        "solc_list" => v.extend(["solc".into(), "list".into()]),
        "net_status" => v.push("net".into()),
        "chain_info" => {
            v.push("chain".into());
            if let Some(x) = gs(a, "rpc") {
                v.extend(["--rpc".into(), x]);
            }
        }
        "tx_info" => v.extend(["tx".into(), need(a, "hash")?]),
        "block_info" => v.extend(["block".into(), need(a, "number")?]),
        "account_info" => v.extend(["account".into(), need(a, "address")?]),
        "trace_tx" => v.extend(["trace".into(), need(a, "hash")?]),
        "eth_call" => {
            v.push("ethcall".into());
            v.extend(["--to".into(), need(a, "to")?]);
            v.push(need(a, "function")?);
            v.extend(ga(a, "args"));
            if let Some(x) = gs(a, "types") {
                v.extend(["--types".into(), x]);
            }
            if let Some(x) = gs(a, "returns") {
                v.extend(["--returns".into(), x]);
            }
            if let Some(x) = gs(a, "rpc") {
                v.extend(["--rpc".into(), x]);
            }
            if let Some(x) = gs(a, "block") {
                v.extend(["--block".into(), x]);
            }
        }
        "chains_list" => {
            v.push("chains".into());
            if let Some(x) = gs(a, "filter") {
                v.push(x);
            }
        }
        "fork_state" => {
            v.push("fork".into());
            v.extend(ga(a, "accounts"));
            if let Some(x) = gs(a, "chain") {
                v.extend(["--chain".into(), x]);
            }
            if let Some(x) = gs(a, "block") {
                v.extend(["--block".into(), x]);
            }
            if let Some(x) = gs(a, "rpc") {
                v.extend(["--rpc".into(), x]);
            }
        }
        "fuzz_run" => {
            v.push("fuzz".into());
            v.push(need(a, "contract")?);
            v.push(need(a, "function")?);
            if let Some(x) = gs(a, "runs") {
                v.extend(["--runs".into(), x]);
            }
            if let Some(x) = gs(a, "seed") {
                v.extend(["--seed".into(), x]);
            }
        }
        "invariant_run" => {
            v.push("invariant".into());
            v.push(need(a, "contract")?);
            if let Some(x) = gs(a, "invariant") {
                v.extend(["--invariant".into(), x]);
            }
            if let Some(x) = gs(a, "sequences") {
                v.extend(["--sequences".into(), x]);
            }
            if let Some(x) = gs(a, "depth") {
                v.extend(["--depth".into(), x]);
            }
            if let Some(x) = gs(a, "seed") {
                v.extend(["--seed".into(), x]);
            }
        }
        "reentrancy_scan" => v.push("reentrancy".into()),
        "layout_check" => {
            v.push("layout".into());
            v.push(need(a, "old_contract")?);
            v.push(need(a, "new_contract")?);
        }
        "bytecode_diff" => {
            v.push("bytediff".into());
            v.push(need(a, "address")?);
            v.push(need(a, "contract")?);
            if let Some(x) = gs(a, "rpc") {
                v.extend(["--rpc".into(), x]);
            }
        }
        "coverage_run" => v.push("coverage".into()),
        "index_blocks" => {
            v.push("index".into());
            if let Some(x) = gs(a, "blocks") {
                v.extend(["--blocks".into(), x]);
            }
            if let Some(x) = gs(a, "rpc") {
                v.extend(["--rpc".into(), x]);
            }
        }
        "aa_simulate" => {
            v.push("aa".into());
            v.extend(["--sender".into(), need(a, "sender")?]);
            v.extend(["--call-data".into(), need(a, "call_data")?]);
            if let Some(x) = gs(a, "nonce") {
                v.extend(["--nonce".into(), x]);
            }
            if let Some(x) = gs(a, "signature") {
                v.extend(["--signature".into(), x]);
            }
        }
        "blob_decode" => {
            v.push("blob".into());
            v.push("decode".into());
            v.push(need(a, "raw")?);
        }
        "blob_verify" => {
            v.push("blob".into());
            v.push("verify".into());
            v.push(need(a, "raw")?);
        }
        "blob_fetch" => {
            v.push("blob".into());
            v.push("fetch".into());
            v.push(need(a, "txhash")?);
        }
        "verkle_key" => {
            v.push("verkle".into());
            v.push("key".into());
            v.extend(["--address".into(), need(a, "address")?]);
            if let Some(x) = gs(a, "slot") {
                v.extend(["--slot".into(), x]);
            }
        }
        "verkle_demo" => {
            v.push("verkle".into());
            v.push("demo".into());
            if let Some(x) = gs(a, "address") {
                v.extend(["--address".into(), x]);
            }
            if let Some(x) = gs(a, "slots") {
                v.extend(["--slots".into(), x]);
            }
        }
        "ws_subscribe" => {
            v.push("ws".into());
            v.push("subscribe".into());
            if let Some(x) = gs(a, "chain") {
                v.extend(["--chain".into(), x]);
            }
            if let Some(x) = gs(a, "url") {
                v.extend(["--url".into(), x]);
            }
            if let Some(x) = gs(a, "method") {
                v.extend(["--method".into(), x]);
            }
            if let Some(x) = gs(a, "address") {
                v.extend(["--address".into(), x]);
            }
            if let Some(x) = gs(a, "max") {
                v.extend(["--max".into(), x]);
            }
            if let Some(x) = gs(a, "timeout") {
                v.extend(["--timeout".into(), x]);
            }
        }
        "build_block" => {
            v.push("build-block".into());
            if let Some(x) = gs(a, "max_txs") {
                v.extend(["--max-txs".into(), x]);
            }
            if let Some(x) = gs(a, "coinbase") {
                v.extend(["--coinbase".into(), x]);
            }
            if gb(a, "include_blobs") {
                v.push("--include-blobs".into());
            }
        }
        other => return Err(format!("unknown tool: {other}")),
    }
    Ok(v)
}

// ---------------------------------------------------------------------------
// subprocess execution
// ---------------------------------------------------------------------------

fn run_rrx(argv: &[String]) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate rrx binary: {e}"))?;
    let out = Command::new(&exe)
        .args(argv)
        .output()
        .map_err(|e| format!("failed to spawn rrx: {e}"))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&err);
    }
    if !out.status.success() && text.trim().is_empty() {
        text = format!("rrx exited with status {}", out.status);
    }
    Ok(text)
}

// ---------------------------------------------------------------------------
// JSON-RPC plumbing
// ---------------------------------------------------------------------------

fn result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn handle(msg: &Value) -> Option<Value> {
    let method = msg.get("method").and_then(|m| m.as_str())?;
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let is_notification = msg.get("id").is_none();

    match method {
        "initialize" => {
            let requested = msg
                .get("params")
                .and_then(|p| p.get("protocolVersion"))
                .and_then(|v| v.as_str())
                .unwrap_or(PROTOCOL_VERSION);
            let version = if requested.len() > 0 && requested.contains("20") {
                requested.to_string()
            } else {
                PROTOCOL_VERSION.to_string()
            };
            Some(result(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}, "resources": {}, "prompts": {}},
                    "serverInfo": {
                        "name": "rrx-mcp",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "instructions": "rrx — every Remix IDE tool rebuilt in pure Rust, \
                                     headless over real EVM and live Ethereum mainnet. \
                                     Start with rrx_plugins, workspace_init or net_status."
                }),
            ))
        }
        "notifications/initialized" | "notifications/cancelled" => {
            if is_notification {
                None
            } else {
                Some(result(id, json!({})))
            }
        }
        "ping" => Some(result(id, json!({}))),
        "tools/list" => {
            let list: Vec<Value> = tools()
                .into_iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "inputSchema": t.input_schema,
                    })
                })
                .collect();
            Some(result(id, json!({"tools": list})))
        }
        "tools/call" => {
            let params = msg.get("params").cloned().unwrap_or(json!({}));
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            let empty = Map::new();
            let args = params
                .get("arguments")
                .and_then(|x| x.as_object())
                .cloned()
                .unwrap_or(empty);

            let reply = match build_argv(&name, &args).and_then(|argv| run_rrx(&argv)) {
                Ok(text) => json!({
                    "content": [{"type": "text", "text": text}],
                    "isError": false
                }),
                Err(e) => json!({
                    "content": [{"type": "text", "text": e}],
                    "isError": true
                }),
            };
            Some(result(id, reply))
        }
        "resources/list" => {
            // expose workspace solidity sources + artifacts as MCP resources
            let mut items: Vec<Value> = vec![];
            let mut push_dir = |dir: &str, exts: &[&str], items: &mut Vec<Value>| {
                if let Ok(rd) = std::fs::read_dir(dir) {
                    for e in rd.flatten() {
                        let p = e.path();
                        let name = e.file_name().to_string_lossy().to_string();
                        if p.is_file() && exts.iter().any(|x| name.ends_with(x)) {
                            items.push(json!({
                                "uri": format!("file://{dir}/{name}"),
                                "name": name,
                                "mimeType": "text/plain",
                            }));
                        }
                    }
                }
            };
            push_dir(".", &[".sol", ".rrx"], &mut items);
            push_dir("contracts", &[".sol"], &mut items);
            push_dir("artifacts", &[".json"], &mut items);
            Some(result(id, json!({"resources": items})))
        }
        "resources/read" => {
            let uri = msg
                .get("params")
                .and_then(|p| p.get("uri"))
                .and_then(|u| u.as_str())
                .unwrap_or("")
                .trim_start_matches("file://")
                .to_string();
            match std::fs::read_to_string(&uri) {
                Ok(text) => Some(result(
                    id,
                    json!({
                        "contents": [{
                            "uri": uri,
                            "mimeType": "text/plain",
                            "text": text
                        }]
                    }),
                )),
                Err(e) => Some(error(id, -32602, &format!("cannot read {uri}: {e}"))),
            }
        }
        "prompts/list" => Some(result(
            id,
            json!({"prompts": [
                {
                    "name": "audit_contract",
                    "description": "Security audit workflow: static analysis + dynamic reentrancy scan + fuzzing",
                    "arguments": [{"name": "contract", "description": "contract name in the workspace", "required": true}]
                },
                {
                    "name": "explain_trace",
                    "description": "Replay and explain a real mainnet transaction step-by-step in the rrx EVM",
                    "arguments": [{"name": "hash", "description": "transaction hash", "required": true}]
                },
                {
                    "name": "write_tests",
                    "description": "Generate remix-style unit tests + an invariant test for a contract, then run them",
                    "arguments": [{"name": "contract", "description": "contract name", "required": true}]
                },
                {
                    "name": "safe_upgrade",
                    "description": "Verify an upgrade: storage layout collision check + live bytecode diff",
                    "arguments": [
                        {"name": "old_contract", "description": "current implementation", "required": true},
                        {"name": "new_contract", "description": "new implementation", "required": true}
                    ]
                }
            ]}))),
        "prompts/get" => {
            let name = msg
                .get("params")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            let text = match name {
                "audit_contract" => "Run the full rrx security audit on the given contract:\n1. static_analysis — lint the AST\n2. reentrancy_scan — execute every function on the real EVM and flag CEI violations\n3. fuzz_run — 512 random-input executions\n4. invariant_run — 50 sequences\nSummarize findings with severity and suggested fixes.".to_string(),
                "explain_trace" => "Fetch the real transaction by hash, replay it in the rrx EVM interpreter, and explain: what the transaction does, every external call it makes, gas hotspots, and any state changes.".to_string(),
                "write_tests" => "Write remix-style unit tests (*_test.sol) plus an invariant_<Contract>() view for the contract, run them with run_tests, then measure coverage with coverage_run.".to_string(),
                "safe_upgrade" => "Before deploying the upgrade: run layout_check(old, new) for storage collisions, then bytecode_diff against the live address. Only recommend the upgrade if both pass.".to_string(),
                other => return Some(error(id, -32602, &format!("unknown prompt: {other}"))),
            };
            Some(result(
                id,
                json!({"description": format!("rrx prompt: {name}"), "messages": [
                    {"role": "user", "content": {"type": "text", "text": text}}
                ]}),
            ))
        }
        other => {
            if is_notification {
                None
            } else {
                Some(error(id, -32601, &format!("method not found: {other}")))
            }
        }
    }
}

/// Serve MCP over stdio until stdin closes.
pub fn serve() -> Result<(), String> {
    eprintln!(
        "rrx-mcp v{}: serving {} tools over stdio (MCP {})",
        env!("CARGO_PKG_VERSION"),
        tools().len(),
        PROTOCOL_VERSION
    );
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut reader = stdin.lock();
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .map_err(|e| format!("stdin read failed: {e}"))?;
        if n == 0 {
            break; // EOF: client closed the pipe
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parsed: Result<Value, _> = serde_json::from_str(trimmed);
        let reply = match parsed {
            Ok(msg) => handle(&msg),
            Err(e) => Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
        };
        if let Some(out) = reply {
            let mut buf = out.to_string().into_bytes();
            buf.push(b'\n');
            stdout
                .write_all(&buf)
                .map_err(|e| format!("stdout write failed: {e}"))?;
            stdout.flush().map_err(|e| format!("stdout flush failed: {e}"))?;
        }
    }
    Ok(())
}
