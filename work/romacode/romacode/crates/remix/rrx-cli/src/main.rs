//! rrx — headless Remix: every Remix IDE tool rebuilt in pure Rust, driven by a CLI.
//! Real EVM, real state, real signatures, real gas. No simulation.

use std::io::Read as _;
use std::path::{Path, PathBuf};

mod mcp;
mod lsp;

use clap::{Parser, Subcommand};
use rrx_chain::dev::{dev_address, dev_keypair};
use rrx_chain::{Chain, ChainConfig};
use rrx_util::{u256_from_str, H160, U256};
use rrx_tools::deploy::AccountSpec;

#[derive(Parser)]
#[command(
    name = "rrx",
    version,
    about = "rrx — Remix tools rebuilt in pure Rust (headless, real EVM, real gas)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// workspace management (Template Explorer)
    Workspace {
        #[command(subcommand)]
        action: WorkspaceAction,
    },
    /// File Explorer
    File {
        #[command(subcommand)]
        action: FileAction,
    },
    /// Search in files
    Search {
        pattern: String,
        #[arg(long)]
        regex: bool,
    },
    /// Solidity Compiler (real solc)
    Compile {
        #[arg(long)]
        solc: Option<String>,
        #[arg(long)]
        no_opt: bool,
        #[arg(long, default_value = "shanghai")]
        evm_version: String,
    },
    /// Static Analysis on the AST
    Analyze,
    /// Solidity Unit Testing (remix-tests conventions, real execution)
    Test {
        #[arg(long)]
        solc: Option<String>,
        #[arg(long)]
        no_opt: bool,
        #[arg(long)]
        gas_file: bool,
    },
    /// WildToken pipeline: the spec-faithful "Tether USD" lookalike (compile / demo / deploy)
    Wildtoken {
        #[command(subcommand)]
        action: WildtokenAction,
    },
    /// Fake-asset stack: registry + clone verdicts, variant fixtures, honeypot audit, proof verify
    Fakeasset {
        #[command(subcommand)]
        action: FakeassetAction,
    },
    /// Deploy & Run: deploy a contract
    Deploy {
        contract: String,
        /// constructor args, comma separated
        #[arg(long)]
        args: Option<String>,
        /// dev account index (0-9)
        #[arg(long)]
        account: Option<usize>,
        /// explicit private key (hex) — RRX_PRIVATE_KEY env also works
        #[arg(long)]
        key: Option<String>,
        #[arg(long)]
        value: Option<String>,
        /// deploy to a REAL network instead of the local chain (RRX_RPC env also works)
        #[arg(long)]
        rpc: Option<String>,
        /// deploy straight to Ethereum MAINNET with endpoint auto-failover
        #[arg(long)]
        mainnet: bool,
        /// confirm a MAINNET (chain id 1) broadcast — real funds will be spent
        #[arg(long)]
        yes: bool,
        #[arg(long, default_value = "100")]
        max_fee_gwei: u64,
        #[arg(long, default_value = "2")]
        max_priority_gwei: u64,
    },
    /// read-only call on a deployed contract (local chain by default, or a real network)
    Call {
        contract: String,
        function: String,
        args: Vec<String>,
        /// run the call against a real network (RRX_RPC env also works)
        #[arg(long)]
        rpc: Option<String>,
        /// run the call against Ethereum mainnet with endpoint auto-failover
        #[arg(long)]
        mainnet: bool,
    },
    /// signed ETH transfer / state-changing call on a REAL network (mainchain)
    Send {
        /// recipient: address or ENS name
        #[arg(long)]
        to: String,
        /// amount in ETH (e.g. 0.01) for a plain transfer
        #[arg(long)]
        value: Option<String>,
        /// state-changing call: fn(types) name, e.g. "transfer(address,uint256)"
        #[arg(long)]
        fn_sig: Option<String>,
        /// comma-separated call args for --fn
        #[arg(long)]
        args: Option<String>,
        /// private key hex — RRX_PRIVATE_KEY env also works
        #[arg(long)]
        key: Option<String>,
        /// rpc url — RRX_RPC env also works, default = mainnet auto-failover
        #[arg(long)]
        rpc: Option<String>,
        /// force the MAINNET failover endpoint list even if RRX_RPC is set
        #[arg(long)]
        mainnet: bool,
        /// confirm a chain id 1 broadcast — real funds will be spent
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        max_fee_gwei: Option<u64>,
        #[arg(long)]
        max_priority_gwei: Option<u64>,
    },
    /// replay a REAL confirmed transaction in the rrx EVM interpreter
    Debugtx {
        hash: String,
        #[arg(long, default_value = "0")]
        max_steps: usize,
        #[arg(long, default_value = "12")]
        top: usize,
        #[arg(long)]
        rpc: Option<String>,
    },
    /// live event logs (eth_getLogs) with Transfer/Approval decoding
    Logs {
        #[arg(long)]
        address: Option<String>,
        /// event signature to filter topic0, e.g. "Transfer(address,address,uint256)"
        #[arg(long)]
        topic0: Option<String>,
        #[arg(long, default_value = "latest")]
        from: String,
        #[arg(long, default_value = "latest")]
        to: String,
        #[arg(long, default_value = "10")]
        limit: usize,
        #[arg(long)]
        rpc: Option<String>,
    },
    /// real ENS resolution on the mainnet registry
    Ens {
        name: String,
        /// reverse-resolve an address instead
        #[arg(long)]
        reverse: bool,
        #[arg(long)]
        rpc: Option<String>,
    },
    /// live token inspection (ERC-20/721/1155) via real eth_call
    Token {
        address: String,
        /// holder address (or ENS) for a balance read
        #[arg(long)]
        holder: Option<String>,
        #[arg(long)]
        rpc: Option<String>,
    },
    /// fetch VERIFIED sources of a deployed contract (Sourcify / Etherscan V2)
    Sources {
        address: String,
        #[arg(long, default_value_t = 1)]
        chain_id: u64,
        #[arg(long)]
        etherscan_key: Option<String>,
        #[arg(long, default_value = "fetched_sources")]
        out: String,
    },
    /// cryptographically verify account/storage state against a block state root
    Proof {
        address: String,
        /// storage slots to prove (decimal or hex)
        #[arg(long)]
        slot: Vec<String>,
        #[arg(long, default_value = "latest")]
        block: String,
        #[arg(long)]
        rpc: Option<String>,
    },
    /// live mainnet head tracker — prints new blocks as they land
    Follow {
        #[arg(long, default_value = "12")]
        every: u64,
        #[arg(long)]
        rpc: Option<String>,
    },
    /// small utilities: keccak / unit conversion / selector / namehash
    Utils {
        #[command(subcommand)]
        action: UtilsAction,
    },
    /// Gas Fee tool
    Gas {
        #[command(subcommand)]
        action: GasAction,
    },
    /// Debugger
    Debug {
        contract: String,
        function: String,
        args: Vec<String>,
        #[arg(long)]
        max_steps: Option<usize>,
        #[arg(long)]
        json: bool,
    },
    /// Contract Flattener
    Flatten {
        file: String,
        #[arg(long)]
        out: Option<String>,
    },
    /// UML Generator (mermaid)
    Uml,
    /// Doc Generator (markdown)
    Doc {
        #[arg(long)]
        out: Option<String>,
    },
    /// Script Runner (.rrx DSL on the real chain)
    Script { file: String },
    /// mini git for the workspace
    Git {
        #[command(subcommand)]
        action: GitAction,
    },
    /// verify a contract on Etherscan V2 (default chain: Ethereum mainnet)
    Verify {
        /// Etherscan API key — ETHERSCAN_API_KEY env also works
        #[arg(long)]
        api_key: Option<String>,
        #[arg(long, default_value_t = 1)]
        chain_id: u64,
        #[arg(long)]
        address: String,
        #[arg(long)]
        contract: String,
        #[arg(long)]
        compiler_version: String,
    },
    /// key management (secp256k1)
    Keys {
        #[command(subcommand)]
        action: KeysAction,
    },
    /// sign / recover EIP-191 messages
    Sign {
        message: String,
        #[arg(long)]
        key: Option<String>,
        #[arg(long)]
        account: Option<usize>,
    },
    /// execute a real precompile directly
    Precompiles {
        address: u8,
        #[arg(default_value = "")]
        input_hex: String,
    },
    /// solc manager
    Solc {
        #[command(subcommand)]
        action: SolcAction,
    },
    /// run a real local JSON-RPC node
    Node {
        #[arg(long, default_value = "8545")]
        port: u16,
        #[arg(long, default_value = "31337")]
        chain_id: u64,
    },
    /// live network gas prices (default: Ethereum mainnet via auto failover)
    Gasprice {
        #[arg(long)]
        rpc: Option<String>,
        /// show the local chain price instead of live network
        #[arg(long)]
        local: bool,
    },
    /// live mainnet connectivity matrix (every endpoint probed)
    Net,
    /// live chain dashboard: client, latest block, base fee, priority fees
    Chain {
        #[arg(long)]
        rpc: Option<String>,
    },
    /// live transaction + receipt report from the real network
    Tx { hash: String },
    /// live block summary ("latest" | "finalized" | decimal number)
    Block { number: String },
    /// live account balance / nonce / code from the real network
    Account { address: String },
    /// live call-tree trace of a confirmed transaction (callTracer)
    Trace { hash: String },
    /// read-only eth_call against a real network with ABI encode/decode
    Ethcall {
        to: String,
        function: String,
        args: Vec<String>,
        /// ABI input types, comma separated (e.g. "address,uint256")
        #[arg(long)]
        types: Option<String>,
        /// ABI output types, comma separated (e.g. "uint256")
        #[arg(long)]
        returns: Option<String>,
        #[arg(long)]
        rpc: Option<String>,
        #[arg(long, default_value = "latest")]
        block: String,
    },
    /// list all tools (Plugin Manager)
    Plugins,
    /// multi-chain registry: live status of every supported network
    Chains {
        /// probe one chain (slug or chain id); omit to probe all
        chain: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// download REAL chain state into the local EVM (fork testing)
    Fork {
        /// addresses or well-known names (usdt, usdc, weth, dai, entrypoint...)
        accounts: Vec<String>,
        /// chain slug (see rrx chains) — default ethereum
        #[arg(long)]
        chain: Option<String>,
        /// custom rpc url
        #[arg(long)]
        rpc: Option<String>,
        /// block tag (default latest)
        #[arg(long, default_value = "latest")]
        block: String,
    },
    /// property-based fuzzing of a function on the real EVM
    Fuzz {
        contract: String,
        function: String,
        #[arg(long, default_value = "256")]
        runs: u64,
        #[arg(long, default_value = "1")]
        seed: u64,
    },
    /// invariant testing: random call sequences + on-chain invariant check
    Invariant {
        contract: String,
        #[arg(long)]
        invariant: Option<String>,
        #[arg(long, default_value = "50")]
        sequences: u64,
        #[arg(long, default_value = "10")]
        depth: u64,
        #[arg(long, default_value = "1")]
        seed: u64,
    },
    /// dynamic reentrancy scan over real EVM executions
    Reentrancy,
    /// storage layout collision check between two contracts (upgrades)
    Layout {
        old_contract: String,
        new_contract: String,
    },
    /// diff the local build against LIVE on-chain bytecode
    Bytediff {
        address: String,
        contract: String,
        #[arg(long)]
        rpc: Option<String>,
    },
    /// function-level coverage measured from real EVM executions
    Coverage,
    /// index REAL blocks from a live chain into the local explorer store
    Index {
        #[arg(long, default_value = "20")]
        blocks: u64,
        #[arg(long, default_value = "rrx-index.json")]
        db: PathBuf,
        #[arg(long)]
        rpc: Option<String>,
    },
    /// serve the mini block explorer (real indexed chain data) over HTTP
    Explorer {
        #[arg(long, default_value = "8088")]
        port: u16,
        #[arg(long, default_value = "rrx-index.json")]
        db: PathBuf,
    },
    /// ERC-4337: simulate a UserOperation on the REAL EntryPoint (mainnet)
    Aa {
        #[arg(long)]
        sender: String,
        #[arg(long)]
        call_data: String,
        #[arg(long)]
        nonce: Option<String>,
        #[arg(long)]
        signature: Option<String>,
    },
    /// EIP-4844 blob transactions: decode / KZG-verify / build+sign / live fetch
    Blob {
        #[command(subcommand)]
        action: BlobAction,
    },
    /// Verkle trie (EIP-6800): tree keys, real Pedersen commitments, witnesses
    Verkle {
        #[command(subcommand)]
        action: VerkleAction,
    },
    /// live WebSocket subscriptions (eth_subscribe) over wss:// — real-time chain data
    Ws {
        #[command(subcommand)]
        action: WsAction,
    },
    /// assemble a REAL block from live chain data (forked state + rrx EVM)
    BuildBlock {
        #[arg(long, default_value = "10")]
        max_txs: usize,
        #[arg(long)]
        coinbase: Option<String>,
        #[arg(long, default_value_t = false)]
        include_blobs: bool,
    },
    /// run the MCP server over stdio (Claude Desktop / Cursor / any MCP client)
    Mcp,
    /// run the Solidity language server over stdio (LSP, real solc diagnostics)
    Lsp,
}

#[derive(Subcommand)]
enum BlobAction {
    /// decode a raw (0x03-prefixed) blob transaction
    Decode {
        /// raw signed tx hex (with 0x03 type byte)
        raw: String,
    },
    /// decode + REAL KZG verification of the network wrapper (tx + sidecar)
    Verify {
        /// network wrapper hex: 0x03 || rlp([payload, commitments, blobs, proofs])
        raw: String,
    },
    /// build + sign a real blob tx (KZG commitment/proof from a real blob file)
    Build {
        /// 131072-byte blob file (field elements must be < BLS modulus)
        #[arg(long)]
        blob_file: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        key: Option<String>,
        #[arg(long, default_value_t = 1)]
        chain_id: u64,
        #[arg(long)]
        nonce: Option<u64>,
        #[arg(long, default_value_t = 2_000_000_000)]
        max_fee_gwei: u64,
        #[arg(long, default_value_t = 100_000_000)]
        priority_gwei: u64,
        #[arg(long, default_value_t = 100_000_000)]
        blob_fee_gwei: u64,
        #[arg(long, default_value_t = 120_000)]
        gas: u64,
        /// print the network wrapper (tx + sidecar) for `blob verify`
        #[arg(long)]
        emit_wrapper: bool,
        /// BROADCAST to mainnet (spend guard: requires --confirm)
        #[arg(long)]
        send: bool,
        #[arg(long)]
        confirm: bool,
    },
    /// fetch a REAL tx from mainnet and report blob fields if type-3
    Fetch { txhash: String },
}

#[derive(Subcommand)]
enum VerkleAction {
    /// derive the EIP-6800 tree key for a storage slot / code chunk
    Key {
        #[arg(long)]
        address: String,
        #[arg(long, default_value_t = 0)]
        slot: u32,
    },
    /// live demo: insert storage slots of a REAL address, compute real commitments,
    /// produce + verify a witness against the root
    Demo {
        #[arg(long, default_value = "d8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
        address: String,
        #[arg(long, default_value_t = 8)]
        slots: usize,
    },
}

#[derive(Subcommand)]
enum WsAction {
    /// subscribe to live events over wss:// (real-time, no polling)
    Subscribe {
        /// chain name: mainnet|base|arbitrum|optimism|polygon|bsc
        #[arg(long, default_value = "mainnet")]
        chain: String,
        /// explicit wss:// URL (overrides --chain)
        #[arg(long)]
        url: Option<String>,
        /// eth_subscribe method: newHeads | logs | newPendingTransactions
        #[arg(long, default_value = "newHeads")]
        method: String,
        /// for logs: filter address
        #[arg(long)]
        address: Option<String>,
        /// stop after N events
        #[arg(long, default_value_t = 5)]
        max: usize,
        /// overall timeout seconds
        #[arg(long, default_value_t = 90)]
        timeout: u64,
    },
}

#[derive(Subcommand)]
enum WorkspaceAction {
    Init { template: String, name: String },
    List,
}

#[derive(Subcommand)]
enum FakeassetAction {
    /// canonical USDT/USDC/WETH registry; --compare gives the CLONE/REAL verdict
    Registry {
        #[arg(long)]
        compare: Option<String>,
        #[arg(long)]
        chain: Option<String>,
        /// live spot-check every registry entry (symbol + decimals eth_call)
        #[arg(long)]
        verify_live: bool,
    },
    /// list the variant matrix — "kaun dhoka khaata hai"
    Variants,
    /// print a variant's Solidity source
    Source {
        #[arg(long)]
        variant: String,
    },
    /// compile a variant with real solc
    Compile {
        #[arg(long)]
        variant: String,
    },
    /// deploy a variant to a TESTNET rpc (chain id 1 hard-refused)
    Deploy {
        #[arg(long)]
        variant: String,
        #[arg(long)]
        rpc: Option<String>,
        /// explicit private key (hex) — RRX_PRIVATE_KEY env also works
        #[arg(long)]
        key: Option<String>,
    },
    /// audit: local victim story (--variant) or live probes (--address + --from)
    Audit {
        #[arg(long)]
        variant: Option<String>,
        #[arg(long)]
        address: Option<String>,
        #[arg(long)]
        chain: Option<String>,
        /// holder address for the live sell-path probe (eth_call impersonation)
        #[arg(long)]
        from: Option<String>,
    },
    /// screenshot fact-check: real tx facts vs claimed values → TRUE/FALSE
    Verify {
        txid: String,
        /// claims k=v, repeatable: recipient=, amount=, status=
        #[arg(long = "claim")]
        claims: Vec<String>,
        #[arg(long)]
        chain: Option<String>,
    },
    /// flasher — the deposit story, exchange ki aankh se: UI balance vs checklist → credit: NO
    Flasher {
        /// live token address (default: runs the local clone story)
        #[arg(long)]
        address: Option<String>,
        #[arg(long)]
        chain: Option<String>,
        /// funded holder for the live sell-path probe
        #[arg(long)]
        from: Option<String>,
    },
    /// pending/RBF replacement-risk check — "pending" koi proof nahi
    Pending {
        txid: String,
        #[arg(long)]
        chain: Option<String>,
    },
    /// the full one-scroll story: clone → mint → burn → honeypot → registry → flasher → proof
    Demo,
}

#[derive(Subcommand)]
enum WildtokenAction {
    /// print the canonical contract source
    Source,
    /// compile the embedded source with real solc
    Compile,
    /// full story on the real rrx EVM: deploy → public mint → cross-wallet burn
    Demo,
    /// deploy to a real network (TESTNET recommended; mainnet needs --yes)
    Deploy {
        #[arg(long)]
        rpc: Option<String>,
        #[arg(long)]
        mainnet: bool,
        /// explicit private key (hex) — RRX_PRIVATE_KEY env also works
        #[arg(long)]
        key: Option<String>,
        /// confirm a MAINNET (chain id 1) broadcast — real funds will be spent
        #[arg(long)]
        yes: bool,
        #[arg(long, default_value = "100")]
        max_fee_gwei: u64,
        #[arg(long, default_value = "2")]
        max_priority_gwei: u64,
    },
}

#[derive(Subcommand)]
enum FileAction {
    Ls { path: Option<String> },
    Tree,
    Cat { file: String },
    Write { file: String, content: String },
    Rm { path: String },
    Mkdir { path: String },
    Mv { from: String, to: String },
}

#[derive(Subcommand)]
enum GasAction {
    /// real execution-based estimate (intrinsic + execution breakdown)
    Estimate {
        contract: String,
        function: String,
        args: Vec<String>,
    },
    /// per-contract/per-method report from the last test/script run
    Report {
        #[arg(long, default_value = "table")]
        style: String,
    },
    /// local base fee & suggestions
    Price,
    /// real per-opcode gas metering of a call
    Trace {
        contract: String,
        function: String,
        args: Vec<String>,
        #[arg(long, default_value = "15")]
        top: usize,
    },
}

#[derive(Subcommand)]
enum GitAction {
    Init,
    Add,
    Commit { message: String },
    Status,
    Log,
}

#[derive(Subcommand)]
enum KeysAction {
    New,
    List,
    Dev,
}

#[derive(Subcommand)]
enum SolcAction {
    Install { version: Option<String> },
    List,
}

#[derive(Subcommand)]
enum UtilsAction {
    /// keccak256 of a hex string or UTF-8 text
    Keccak { input: String },
    /// convert between wei / gwei / ether (e.g. rrx utils unit 1 gwei ether)
    Unit { amount: String, from: String, to: String },
    /// 4-byte selector of name(types)
    Selector { signature: String },
    /// ENS namehash of a domain
    Namehash { name: String },
}

fn ws_root() -> PathBuf {
    PathBuf::from(".")
}

fn fresh_chain() -> Chain {
    let alloc: Vec<_> = (0..10)
        .map(|i| {
            let kp = dev_keypair(i);
            (kp.address().unwrap(), U256::from_dec_str("10000000000000000000000").unwrap())
        })
        .collect();
    Chain::genesis(ChainConfig::default(), &alloc)
}

/// Fresh local chain with the contract REPLAYED (real deployment executed again),
/// so storage/balances match the script/test run. CLI invocations are one-shot.
fn chain_with_deployment(name: &str) -> Result<(Chain, H160), String> {
    let out = full_compile(None, false, "shanghai")?;
    let artifact = rrx_tools::compile::find_artifact(&out.artifacts, name)
        .cloned()
        .ok_or_else(|| format!("artifact '{name}' not found"))?;
    let mut chain = fresh_chain();
    // read the constructor args recorded in the deployment entry
    let (args, types) = deployment_args(name)?;
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let type_refs: Vec<&str> = types.iter().map(|s| s.as_str()).collect();
    let acct = AccountSpec::Dev(0);
    let outcome = rrx_tools::deploy::deploy_local(
        &ws_root(),
        &mut chain,
        &artifact,
        name,
        &acct,
        &arg_refs,
        &type_refs,
        U256::zero(),
    )?;
    Ok((chain, outcome.address))
}

fn deployment_args(name: &str) -> Result<(Vec<String>, Vec<String>), String> {
    let path = ws_root().join(".rrx").join("deployments.json");
    let list: Vec<serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    for entry in list.iter().rev() {
        if entry["contract"] == serde_json::json!(name) {
            let args: Vec<String> = entry["ctorArgs"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            let types: Vec<String> = entry["ctorTypes"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            return Ok((args, types));
        }
    }
    Ok((vec![], vec![]))
}

fn require_artifact(name: &str) -> Result<(PathBuf, serde_json::Value), String> {
    let dir = ws_root().join("artifacts");
    let file = dir.join(format!("{name}.json"));
    if !file.exists() {
        // try file:name lookup in all artifacts
        for e in std::fs::read_dir(&dir).map_err(|_| "no artifacts — run `rrx compile` first".to_string())? {
            let e = e.map_err(|e| e.to_string())?;
            let content = std::fs::read_to_string(e.path()).map_err(|e| e.to_string())?;
            let v: serde_json::Value = serde_json::from_str(&content).map_err(|e| e.to_string())?;
            if v.get("name").and_then(|n| n.as_str()) == Some(name) {
                return Ok((ws_root(), v));
            }
        }
        return Err(format!("artifact '{name}' not found — run `rrx compile` first"));
    }
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    Ok((ws_root(), v))
}

fn account_from(account: Option<usize>, key: Option<String>) -> Result<AccountSpec, String> {
    match (key, account) {
        (Some(k), _) => Ok(AccountSpec::Key(k)),
        (None, Some(i)) => Ok(AccountSpec::Dev(i)),
        (None, None) => Ok(AccountSpec::Dev(0)),
    }
}

/// resolve `<Contract>.<fn>` targets against artifacts (used by call/send/gas/debug)
fn resolve_target(contract: &str, function: &str, args: &[String]) -> Result<(serde_json::Value, H160, Vec<String>, Vec<String>, Vec<String>), String> {
    let (_, artifact) = require_artifact(contract)?;
    let abi = artifact
        .get("abi")
        .and_then(|a| a.as_array())
        .ok_or("artifact has no abi")?;
    for entry in abi {
        if entry["type"] == "function" && entry["name"] == function {
            let types: Vec<String> = entry["inputs"]
                .as_array()
                .map(|a| a.iter().map(|i| i["type"].as_str().unwrap_or("uint256").to_string()).collect())
                .unwrap_or_default();
            let outs: Vec<String> = entry["outputs"]
                .as_array()
                .map(|a| a.iter().map(|i| i["type"].as_str().unwrap_or("uint256").to_string()).collect())
                .unwrap_or_default();
            if types.len() != args.len() {
                return Err(format!("{function} expects {} args ({}), got {}", types.len(), types.join(","), args.len()));
            }
            // address: use the most recent deployment of this contract
            let addr = latest_deployment(&artifact["name"].as_str().unwrap_or(contract).to_string())?;
            let _ = &artifact;
            return Ok((artifact.clone(), addr, types, args.to_vec(), outs));
        }
    }
    Err(format!("function '{function}' not found in ABI of {contract}"))
}

fn latest_deployment(name: &str) -> Result<H160, String> {
    let path = ws_root().join(".rrx").join("deployments.json");
    let list: Vec<serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    for entry in list.iter().rev() {
        if entry["contract"] == serde_json::json!(name) {
            return rrx_util::h160_from_hex(entry["address"].as_str().unwrap_or(""));
        }
    }
    Err(format!("no deployment of '{name}' — run `rrx deploy` or `rrx script` first"))
}

fn full_compile(solc: Option<String>, no_opt: bool, evm_version: &str) -> Result<rrx_tools::compile::CompileOutput, String> {
    rrx_tools::compile::compile_workspace(&ws_root(), solc.as_deref(), !no_opt, evm_version)
}

/// endpoint resolution for WRITE tools: --rpc > --mainnet (failover list) > RRX_RPC > auto mainnet
fn resolve_write_endpoint(rpc: Option<&String>, mainnet: bool) -> Result<rrx_rpc::Provider, String> {
    match rpc {
        Some(u) => Ok(rrx_rpc::Provider::new(u)),
        None if mainnet => rrx_rpc::auto_mainnet(),
        None => match std::env::var("RRX_RPC") {
            Ok(u) if !u.is_empty() => Ok(rrx_rpc::Provider::new(&u)),
            _ => rrx_rpc::auto_mainnet(),
        },
    }
}

/// refuse to broadcast to mainnet without --yes
fn confirm_mainnet(p: &rrx_rpc::Provider, yes: bool) -> Result<(), String> {
    let cid = p.chain_id()?;
    if cid == 1 && !yes {
        return Err(
            "SAFETY STOP: this broadcasts to ETHEREUM MAINNET (chain id 1) — real funds will be spent. Re-run with --yes to confirm.".into(),
        );
    }
    Ok(())
}

fn main() {
    let cli = Cli::parse();
    match run(cli.command) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

fn run(cmd: Command) -> Result<(), String> {
    match cmd {
        Command::Blob { action } => match action {
            BlobAction::Decode { raw } => {
                let raw = if raw == "-" {
                    let mut buf = String::new();
                    std::io::stdin().read_to_string(&mut buf).map_err(|e| e.to_string())?;
                    buf.trim().to_string()
                } else { raw };
                let bytes = rrx_util::from_hex(&raw).map_err(|e| e.to_string())?;
                let tx = rrx_chain::Transaction::decode(&bytes)?;
                print!("{}", rrx_tools::blob::tx_report(&tx));
            }
            BlobAction::Verify { raw } => {
                let raw = if raw == "-" {
                    let mut buf = String::new();
                    std::io::stdin().read_to_string(&mut buf).map_err(|e| e.to_string())?;
                    buf.trim().to_string()
                } else { raw };
                let bytes = rrx_util::from_hex(&raw).map_err(|e| e.to_string())?;
                let (tx, sidecar) = rrx_tools::blob::decode_with_sidecar(&bytes)?;
                print!("{}", rrx_tools::blob::tx_report(&tx));
                let report = rrx_tools::blob::verify_sidecar(&tx, &sidecar)?;
                print!("{report}");
            }
            BlobAction::Build {
                blob_file,
                to,
                key,
                chain_id,
                nonce,
                max_fee_gwei,
                priority_gwei,
                blob_fee_gwei,
                gas,
                emit_wrapper,
                send,
                confirm,
            } => {
                let blob = std::fs::read(&blob_file).map_err(|e| e.to_string())?;
                if blob.len() != 131072 {
                    return Err(format!("blob file must be exactly 131072 bytes (got {})", blob.len()));
                }
                let to_addr = rrx_util::h160_from_hex(&to)?;
                let kp = account_from(None, key)?.keypair()?;
                let provider = resolve_write_endpoint(None, true)?;
                let n = match nonce {
                    Some(n) => n,
                    None => provider.nonce(&kp.address()?, "pending")?,
                };
                let (tx, sidecar) = rrx_tools::blob::build_and_sign(
                    &blob,
                    to_addr,
                    &kp,
                    chain_id,
                    n,
                    U256::from(max_fee_gwei) * U256::from(1_000_000_000u64),
                    U256::from(priority_gwei) * U256::from(1_000_000_000u64),
                    U256::from(blob_fee_gwei) * U256::from(1_000_000_000u64),
                    gas,
                    U256::zero(),
                    vec![],
                )?;
                print!("{}", rrx_tools::blob::tx_report(&tx));
                let report = rrx_tools::blob::verify_sidecar(&tx, &sidecar)?;
                print!("{report}");
                let raw = tx.rlp_encode();
                println!("  raw tx:        0x{}", rrx_util::to_hex(&raw));
                if emit_wrapper {
                    let wrapped = rrx_tools::blob::wrap_with_sidecar(&tx, &sidecar)?;
                    println!("  wrapper:       0x{}", rrx_util::to_hex(&wrapped));
                    println!("  (verify anytime: rrx blob verify 0x<wrapper>)");
                }
                if send {
                    if !confirm {
                        return Err("spend guard: sending a blob tx costs real ETH — re-run with --confirm".into());
                    }
                    let hash = provider.send_raw_transaction(&raw)?;
                    println!("  broadcast:     0x{}", hex::encode(hash.0));
                }
            }
            BlobAction::Fetch { txhash } => {
                let provider = resolve_write_endpoint(None, true)?;
                print!("{}", rrx_tools::blob::fetch_live(&provider, &txhash)?);
            }
        },
        Command::Verkle { action } => match action {
            VerkleAction::Key { address, slot } => {
                let a = rrx_util::h160_from_hex(&address)?;
                let (stem, suffix) = rrx_trie::verkle::storage_key(&a.0, slot);
                println!("EIP-6800 tree key for slot {} of {}", slot, address);
                println!("  stem:   0x{}", rrx_util::to_hex(&stem));
                println!("  suffix: {}", suffix);
            }
            VerkleAction::Demo { address, slots } => {
                let a = rrx_util::h160_from_hex(&address)?;
                let provider = resolve_write_endpoint(None, true)?;
                println!("verkle trie over REAL storage of {} (slots 0..{})", address, slots);
                let mut tree = rrx_trie::verkle::VerkleTree::new();
                let mut stems = vec![];
                for slot in 0..slots as u32 {
                    let v = provider.storage_at(&a, &U256::from(slot), "latest")?;
                    let (stem, suffix) = rrx_trie::verkle::storage_key(&a.0, slot);
                    let vbe = rrx_util::u256_to_be32(&v);
                    tree.insert(stem, suffix, vbe);
                    stems.push(stem);
                    println!("  slot {} = 0x{} (stem 0x{}… suffix {})", slot, rrx_util::to_hex(&vbe), rrx_util::to_hex(&stem[..4]), suffix);
                }
                let root = tree.root()?;
                println!("  root commitment: 0x{}", rrx_util::to_hex(&root));
                println!("  (real Pedersen vector commitment over BLS12-381 G1, EIP-6800 internal-node rule)");
                for stem in stems.iter().take(1) {
                    let w = tree.witness(stem)?;
                    let ok = rrx_trie::verkle::verify_witness(&w, &root)?;
                    println!(
                        "  witness for stem 0x{}…: {}",
                        rrx_util::to_hex(&stem[..4]),
                        if ok { "VERIFIED against root ✓" } else { "FAILED ✗" }
                    );
                }
            }
        },
        Command::Ws { action } => match action {
            WsAction::Subscribe { chain, url, method, address, max, timeout } => {
                let endpoint = match &url {
                    Some(u) => u.clone(),
                    None => rrx_rpc::ws::default_wss(&chain).to_string(),
                };
                println!("connecting {endpoint} — eth_subscribe({method})…");
                let params = match (&method[..], &address) {
                    ("logs", Some(a)) => serde_json::json!([{ "address": a }]),
                    ("logs", None) => serde_json::json!([{}]),
                    _ => serde_json::Value::Null,
                };
                let (sid, events) = rrx_rpc::ws::subscribe(&endpoint, &method, params, max, timeout)?;
                println!("subscription id: {sid}");
                for (i, e) in events.iter().enumerate() {
                    println!("[{}/{}] {}", i + 1, events.len(), e);
                }
                println!("done — {} live event(s) from the real chain", events.len());
            }
        },
        Command::BuildBlock { max_txs, coinbase, include_blobs } => {
            let provider = resolve_write_endpoint(None, true)?;
            let cb = match &coinbase {
                Some(c) => Some(rrx_util::h160_from_hex(c)?),
                None => None,
            };
            print!("{}", rrx_tools::builder::build_block(&provider, max_txs, cb, include_blobs)?);
        }
        Command::Mcp => {
            mcp::serve()?;
        }
        Command::Lsp => {
            lsp::serve()?;
        }
        Command::Plugins => {
            print!("{}", rrx_tools::plugins::print_registry());
        }
        Command::Chains { chain, json } => match chain {
            None => {
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&rrx_tools::chains::registry_json())
                            .unwrap_or_default()
                    );
                } else {
                    print!("{}", rrx_tools::chains::probe_all_matrix(None));
                }
            }
            Some(c) => {
                let info = rrx_tools::chains::find(&c)
                    .ok_or_else(|| format!("unknown chain '{c}' (see rrx chains)"))?;
                let st = rrx_tools::chains::probe(info);
                println!(
                    "{} ({}) — chain id {}{}\n  endpoint {}\n  block {}  latency {} ms  client {}\n  explorer: {}",
                    info.name,
                    info.slug,
                    info.chain_id,
                    if info.testnet { " (testnet)" } else { "" },
                    st.endpoint.unwrap_or("DOWN"),
                    st.block.map(|b| b.to_string()).unwrap_or_else(|| "-".into()),
                    st.latency_ms,
                    st.client.as_deref().unwrap_or("-"),
                    info.explorer
                );
            }
        },
        Command::Fork { accounts, chain, rpc, block } => {
            let url = match &rpc {
                Some(u) => u.clone(),
                None => {
                    let info = rrx_tools::chains::find(chain.as_deref().unwrap_or("ethereum"))
                        .ok_or("unknown chain (see rrx chains)")?;
                    info.rpcs[0].to_string()
                }
            };
            let p = rrx_rpc::Provider::new(&url);
            let mut addrs = vec![];
            for a in &accounts {
                match rrx_tools::fork::well_known(a) {
                    Some(h) => addrs.push(h),
                    None => return Err(format!(
                        "cannot resolve '{a}' — pass 0x address or usdt/usdc/weth/dai/entrypoint"
                    )),
                }
            }
            if addrs.is_empty() {
                return Err("pass at least one account to preload (e.g. usdt weth)".into());
            }
            let forked = rrx_tools::fork::download_state(&p, &block, &addrs)?;
            print!("{}", rrx_tools::fork::fork_report(&forked));
            println!("\n✓ REAL state loaded into local MPT trie — full EVM ready to execute against it");
        }
        Command::Fuzz { contract, function, runs, seed } => {
            let cfg = rrx_tools::fuzz::FuzzConfig { runs, seed, account: AccountSpec::Dev(0) };
            let res = rrx_tools::fuzz::fuzz_function(ws_root().as_path(), &contract, &function, &cfg)?;
            print!("{}", res.log);
            if !res.passed {
                std::process::exit(1);
            }
        }
        Command::Invariant { contract, invariant, sequences, depth, seed } => {
            let res = rrx_tools::fuzz::fuzz_invariants(
                ws_root().as_path(),
                &contract,
                invariant.as_deref(),
                sequences,
                depth,
                seed,
            )?;
            print!("{}", res.log);
            if !res.passed {
                std::process::exit(1);
            }
        }
        Command::Reentrancy => {
            let findings = rrx_tools::security::reentrancy_scan(ws_root().as_path())?;
            if findings.is_empty() {
                println!("✓ no reentrancy surface found across real EVM executions");
            } else {
                println!("{} reentrancy finding(s) from REAL execution traces:", findings.len());
                for f in &findings {
                    println!("  ⚠ {}.{}: {}", f.contract, f.function, f.issue);
                }
                std::process::exit(1);
            }
        }
        Command::Layout { old_contract, new_contract } => {
            println!(
                "{}",
                rrx_tools::security::layout_diff(ws_root().as_path(), &old_contract, &new_contract)?
            );
        }
        Command::Bytediff { address, contract, rpc } => {
            let p = match &rpc {
                Some(u) => rrx_rpc::Provider::new(u),
                None => rrx_rpc::auto_mainnet()?,
            };
            let d = rrx_tools::security::bytecode_diff(&p, &address, ws_root().as_path(), &contract)?;
            print!("{}", rrx_tools::security::print_diff(&d));
        }
        Command::Coverage => {
            print!("{}", rrx_tools::coverage::coverage_report(ws_root().as_path(), None)?);
        }
        Command::Index { blocks, db, rpc } => {
            print!(
                "{}",
                rrx_tools::indexer::index_blocks(&rrx_tools::indexer::IndexOptions {
                    blocks,
                    db,
                    rpc,
                })?
            );
        }
        Command::Explorer { port, db } => {
            rrx_tools::indexer::explorer_serve(&rrx_tools::indexer::ExplorerOptions { port, db })?;
        }
        Command::Aa { sender, call_data, nonce, signature } => {
            let p = rrx_rpc::auto_mainnet()?;
            let mut op = rrx_tools::aa::UserOperation {
                sender: sender.clone(),
                call_data: call_data.clone(),
                ..Default::default()
            };
            op.nonce = match nonce {
                Some(n) => n,
                None => {
                    let n = rrx_tools::aa::nonce_of(&p, &sender)?;
                    println!("entryPoint nonce (read from REAL EntryPoint code+state): {n}");
                    n
                }
            };
            if let Some(s) = signature {
                op.signature = s;
            }
            print!("{}", rrx_tools::aa::simulate_validation(&p, &op)?);
        }

        Command::Workspace { action } => match action {
            WorkspaceAction::Init { template, name } => {
                let path = ws_root().join(&name);
                if path.exists() {
                    return Err(format!("'{name}' already exists"));
                }
                rrx_tools::workspace::Workspace::create_from_template(&path, &template)?;
                println!("created workspace {name} (template {template})");
                println!("next: cd {name} && rrx compile && rrx test");
            }
            WorkspaceAction::List => {
                for e in std::fs::read_dir(ws_root()).map_err(|e| e.to_string())? {
                    let e = e.map_err(|e| e.to_string())?;
                    if e.metadata().map(|m| m.is_dir()).unwrap_or(false) {
                        let has_sol = std::fs::read_dir(e.path()).map(|d| {
                            d.filter_map(|x| x.ok()).any(|f| {
                                f.path().extension().map(|x| x == "sol").unwrap_or(false)
                            })
                        }).unwrap_or(false);
                        if has_sol {
                            println!("{}", e.file_name().to_string_lossy());
                        }
                    }
                }
            }
        },

        Command::File { action } => {
            let root = ws_root();
            match action {
                FileAction::Ls { path } => {
                    let dir = path.map(|p| root.join(p)).unwrap_or_else(|| root.clone());
                    for line in rrx_tools::file_explorer::list(&dir)? {
                        println!("{line}");
                    }
                }
                FileAction::Tree => {
                    let mut out = vec![];
                    rrx_tools::file_explorer::tree(&root, "", &mut out)?;
                    for line in out {
                        println!("{line}");
                    }
                }
                FileAction::Cat { file } => {
                    print!("{}", rrx_tools::file_explorer::read(&root.join(file))?);
                }
                FileAction::Write { file, content } => {
                    let target = root.join(&file);
                    rrx_tools::file_explorer::write(&target, &content)?;
                    println!("wrote {file}");
                }
                FileAction::Rm { path } => {
                    let target = root.join(&path);
                    rrx_tools::file_explorer::remove(&target)?;
                    println!("removed {path}");
                }
                FileAction::Mkdir { path } => {
                    let target = root.join(&path);
                    rrx_tools::file_explorer::mkdir(&target)?;
                    println!("created {path}/");
                }
                FileAction::Mv { from, to } => {
                    let f = root.join(&from);
                    let t = root.join(&to);
                    rrx_tools::file_explorer::move_path(&f, &t)?;
                    println!("moved {from} -> {to}");
                }
            }
        }

        Command::Search { pattern, regex } => {
            let hits = rrx_tools::search::search(&ws_root(), &pattern, regex)?;
            for h in hits {
                println!("{}:{}: {}", h.file, h.line_no, h.line);
            }
            println!("(use --regex for regular expressions)");
        }

        Command::Compile { solc, no_opt, evm_version } => {
            let out = full_compile(solc, no_opt, &evm_version)?;
            println!("solc {} compiled {} contract(s):", out.solc_version, out.artifacts.len());
            for (key, a) in &out.artifacts {
                let code_len = a["evm"]["bytecode"]["object"].as_str().map(|s| s.len() / 2).unwrap_or(0);
                println!("  {key}  ({} bytes code)", code_len);
            }
        }

        Command::Analyze => {
            let out = full_compile(None, false, "shanghai")?;
            let ws = rrx_tools::workspace::Workspace::at(&ws_root());
            let sources = rrx_tools::unittest::prepare_sources(&ws)?;
            for (key, artifact) in &out.artifacts {
                if artifact.get("ast").is_some() {
                    continue;
                }
                let _ = artifact;
                let _ = key;
            }
            // analyze needs the raw standard-json output with ASTs; recompile with asts
            let sources_map = sources;
            let solc = rrx_solc::detect()?;
            let input = rrx_solc::build_input(&sources_map, false, "shanghai");
            let output = solc.compile(&input)?;
            let findings = rrx_tools::analyzer::analyze(&output, &sources_map);
            print!("{}", rrx_tools::analyzer::format_findings(&findings, &ws_root()));
        }

        Command::Wildtoken { action } => match action {
            WildtokenAction::Source => {
                print!("{}", rrx_tools::wildtoken::SOURCE);
            }
            WildtokenAction::Compile => {
                let art = rrx_tools::wildtoken::artifact()?;
                let bytecode = rrx_tools::compile::bytecode_of(&art);
                println!("WildToken compiled with real solc");
                println!("  name      Tether USD");
                println!("  symbol    USDT");
                println!("  decimals  6");
                println!("  supply    1,000,000,000 (6 decimals) → deployer");
                println!("  mint      public, unlimited");
                println!("  burn      public, permissionless (any wallet)");
                println!("  bytecode  {} bytes", bytecode.len());
            }
            WildtokenAction::Demo => {
                print!("{}", rrx_tools::wildtoken::demo()?);
            }
            WildtokenAction::Deploy { rpc, mainnet, key, yes, max_fee_gwei, max_priority_gwei } => {
                let art = rrx_tools::wildtoken::artifact()?;
                let key = key.or_else(|| std::env::var("RRX_PRIVATE_KEY").ok().filter(|s| !s.is_empty()));
                let acct = account_from(None, key)?;
                let provider = resolve_write_endpoint(rpc.as_ref(), mainnet)?;
                confirm_mainnet(&provider, yes)?;
                let outcome = rrx_tools::deploy::deploy_rpc(
                    &provider,
                    &art,
                    &acct,
                    &[],
                    &[],
                    rrx_util::U256::zero(),
                    max_fee_gwei,
                    max_priority_gwei,
                )?;
                println!("WildToken deployed: {}", outcome.address);
                println!("  tx        {}", outcome.tx_hash);
                println!("  gas used  {}", outcome.gas_used);
                println!("  supply    1,000,000,000 USDT → {}", outcome.deployer);
                if provider.chain_id().unwrap_or(0) == 1 {
                    println!("  WARNING: \"Tether USD\" on mainnet sits next to the real USDT in wallets — think hard about that.");
                }
            }
        },
        Command::Fakeasset { action } => match action {
            FakeassetAction::Registry { compare, chain, verify_live } => {
                if verify_live {
                    print!("{}", rrx_tools::fakeasset::verify_live_registry()?);
                } else if let Some(addr) = compare {
                    let slug = chain.as_deref();
                    let p = match slug {
                        Some(s) => rrx_tools::fakeasset::provider_for(s)?,
                        None => rrx_rpc::auto_mainnet()?,
                    };
                    print!("{}", rrx_tools::fakeasset::compare(&p, &addr, slug)?);
                } else {
                    print!("{}", rrx_tools::fakeasset::registry_table());
                }
            }
            FakeassetAction::Variants => {
                print!("{}", rrx_tools::fakeasset::variant_list());
            }
            FakeassetAction::Source { variant } => {
                for (file, content) in rrx_tools::fake_variants::variant_sources(&variant)? {
                    println!("// ==== {file} ====");
                    print!("{content}");
                }
            }
            FakeassetAction::Compile { variant } => {
                println!("{}", rrx_tools::fakeasset::compile_variant(&variant)?);
            }
            FakeassetAction::Deploy { variant, rpc, key } => {
                print!("{}", rrx_tools::fakeasset::deploy_variant_live(&variant, rpc.as_deref(), key)?);
            }
            FakeassetAction::Audit { variant, address, chain, from } => {
                if let Some(v) = variant {
                    print!("{}", rrx_tools::fakeasset::audit_local(&v)?.render());
                } else if let Some(addr) = address {
                    let slug = chain.as_deref();
                    let p = match slug {
                        Some(s) => rrx_tools::fakeasset::provider_for(s)?,
                        None => rrx_rpc::auto_mainnet()?,
                    };
                    print!("{}", rrx_tools::fakeasset::audit_live(&p, &addr, slug, from.as_deref())?);
                } else {
                    return Err("audit ko --variant <name> ya --address <addr> chahiye".into());
                }
            }
            FakeassetAction::Verify { txid, claims, chain } => {
                let p = match chain.as_deref() {
                    Some(s) => rrx_tools::fakeasset::provider_for(s)?,
                    None => rrx_rpc::auto_mainnet()?,
                };
                print!("{}", rrx_tools::fakeasset::verify(&p, &txid, &claims, chain.as_deref())?);
            }
            FakeassetAction::Flasher { address, chain, from } => match address {
                Some(addr) => {
                    let slug = chain.as_deref();
                    let p = match slug {
                        Some(s) => rrx_tools::fakeasset::provider_for(s)?,
                        None => rrx_rpc::auto_mainnet()?,
                    };
                    print!("{}", rrx_tools::fakeasset::flasher_live(&p, &addr, slug, from.as_deref())?);
                }
                None => print!("{}", rrx_tools::fakeasset::flasher_local()?),
            },
            FakeassetAction::Pending { txid, chain } => {
                let p = match chain.as_deref() {
                    Some(s) => rrx_tools::fakeasset::provider_for(s)?,
                    None => rrx_rpc::auto_mainnet()?,
                };
                print!("{}", rrx_tools::fakeasset::pending_check(&p, &txid)?);
            }
            FakeassetAction::Demo => {
                print!("{}", rrx_tools::fakeasset::demo()?);
            }
        },
        Command::Test { solc, no_opt, gas_file } => {
            let report = rrx_tools::unittest::run_tests(&ws_root(), solc.as_deref(), !no_opt, None)?;
            report.print();
            if gas_file {
                let p = ws_root().join(".rrx").join("gas.json");
                report.save_gas_records(&p)?;
                println!("gas records saved to {}", p.display());
            }
            if report.failures() > 0 {
                std::process::exit(1);
            }
        }

        Command::Deploy { contract, args, account, key, value, rpc, mainnet, yes, max_fee_gwei, max_priority_gwei } => {
            let (_, artifact) = require_artifact(&contract)?;
            let key = key.or_else(|| std::env::var("RRX_PRIVATE_KEY").ok().filter(|s| !s.is_empty()));
            let acct = account_from(account, key)?;
            let arg_list: Vec<String> = args
                .unwrap_or_default()
                .split(',')
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string())
                .collect();
            let arg_refs: Vec<&str> = arg_list.iter().map(|s| s.as_str()).collect();
            // infer ctor arg types from abi
            let abi = artifact.get("abi").and_then(|a| a.as_array()).cloned().unwrap_or_default();
            let ctor_types: Vec<String> = abi
                .iter()
                .find(|e| e["type"] == "constructor")
                .map(|e| {
                    e["inputs"]
                        .as_array()
                        .map(|a| a.iter().map(|i| i["type"].as_str().unwrap_or("uint256").to_string()).collect())
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            if ctor_types.len() != arg_refs.len() {
                return Err(format!(
                    "constructor expects {} args ({}), got {}",
                    ctor_types.len(),
                    ctor_types.join(","),
                    arg_refs.len()
                ));
            }
            let type_refs: Vec<&str> = ctor_types.iter().map(|s| s.as_str()).collect();
            let value_u = value.map(|v| u256_from_str(&v)).transpose()?.unwrap_or_default();

            let use_remote = mainnet || rpc.is_some() || std::env::var("RRX_RPC").map(|s| !s.is_empty()).unwrap_or(false);
            let outcome = if use_remote {
                let provider = resolve_write_endpoint(rpc.as_ref(), mainnet)?;
                confirm_mainnet(&provider, yes)?;
                let url = provider.url.clone();
                if let Ok((base_hist, rewards)) = provider.fee_history(10) {
                    let g = |v: &U256| (v / U256::from(1_000_000_000u64)).as_u64();
                    if let Some(base) = base_hist.last() {
                        let tip = rewards.last().and_then(|r| r.get(1).copied()).unwrap_or_default();
                        println!(
                            "live fees @ {url}: base {} gwei, tip {} gwei — your caps: max {} gwei, priority {} gwei",
                            g(base),
                            g(&tip),
                            max_fee_gwei,
                            max_priority_gwei
                        );
                        if max_fee_gwei < g(base) {
                            println!("  WARNING: max_fee_gwei is below the current base fee — the tx may sit in the mempool");
                        }
                    }
                }
                rrx_tools::deploy::deploy_rpc(&provider, &artifact, &acct, &arg_refs, &type_refs, value_u, max_fee_gwei, max_priority_gwei)?
            } else {
                // compile fresh so the local chain deploy uses real bytecode + save deployment
                let out = full_compile(None, false, "shanghai")?;
                let artifact_fresh = rrx_tools::compile::find_artifact(&out.artifacts, &contract)
                    .cloned()
                    .ok_or("artifact not found after compile")?;
                let mut chain = fresh_chain();
                let o = rrx_tools::deploy::deploy_local(&ws_root(), &mut chain, &artifact_fresh, &contract, &acct, &arg_refs, &type_refs, value_u)?;
                rrx_tools::deploy::save_deployment(&ws_root(), &o, &rrx_tools::compile::abi_of(&artifact), &arg_list, &ctor_types)?;
                println!("(local chain is ephemeral per invocation — use `rrx script` for multi-step flows, `rrx node` for a persistent RPC node)");
                o
            };
            println!(
                "deployed {} at 0x{:x}\n  tx 0x{:x}  gas {}  block {}",
                outcome.contract, outcome.address, outcome.tx_hash, outcome.gas_used, outcome.block_number
            );
        }

        Command::Call { contract, function, args, rpc, mainnet } => {
            let (_, artifact) = require_artifact(&contract)?;
            let (_, _artifact, types, arg_strs, outs) = resolve_target(&contract, &function, &args)?;
            let arg_refs: Vec<&str> = arg_strs.iter().map(|s| s.as_str()).collect();
            let type_refs: Vec<&str> = types.iter().map(|s| s.as_str()).collect();
            let out_refs: Vec<&str> = outs.iter().map(|s| s.as_str()).collect();
            let remote = mainnet || rpc.is_some();
            if remote {
                // real network: eth_call on the recorded deployment address
                let p = resolve_write_endpoint(rpc.as_ref(), mainnet)?;
                let to = latest_deployment(&contract)?;
                let data = rrx_tools::deploy::encode_call(&function, &type_refs, &arg_refs)?;
                let raw = p.call(None, &to, &data, "latest")?;
                let res = if raw.is_empty() { "(empty return)".to_string() } else { rrx_tools::deploy::format_output(&out_refs, &raw)? };
                println!("{res}");
                println!("(live eth_call @ {} on 0x{:x})", p.url, to);
            } else {
                let (chain, to) = chain_with_deployment(&contract)?;
                let from = dev_address(0);
                let res = rrx_tools::deploy::call_read(&chain, from, to, &function, &type_refs, &arg_refs, &out_refs)?;
                println!("{res}");
            }
            let _ = artifact;
        }

        Command::Gas { action } => match action {
            GasAction::Estimate { contract, function, args } => {
                let (_, _artifact, types, arg_strs, _outs) = resolve_target(&contract, &function, &args)?;
                let arg_refs: Vec<&str> = arg_strs.iter().map(|s| s.as_str()).collect();
                let type_refs: Vec<&str> = types.iter().map(|s| s.as_str()).collect();
                let data = rrx_tools::deploy::encode_call(&function, &type_refs, &arg_refs)?;
                let (chain, to) = chain_with_deployment(&contract)?;
                let from = dev_address(0);
                let est = rrx_tools::gasfee::estimate_local(&chain, from, Some(to), data, U256::zero())?;
                print!("{}", est.print(&format!("{contract}.{function}")));
            }
            GasAction::Report { style } => {
                let p = ws_root().join(".rrx").join("gas.json");
                print!("{}", rrx_tools::gasfee::report_from_file(&p, &style)?);
            }
            GasAction::Price => {
                let chain = fresh_chain();
                print!("{}", rrx_tools::gasfee::price_local(&chain).print("local chain (base fee 1 gwei default)"));
            }
            GasAction::Trace { contract, function, args, top } => {
                let (_, _a, types, arg_strs, _o) = resolve_target(&contract, &function, &args)?;
                let arg_refs: Vec<&str> = arg_strs.iter().map(|s| s.as_str()).collect();
                let type_refs: Vec<&str> = types.iter().map(|s| s.as_str()).collect();
                let data = rrx_tools::deploy::encode_call(&function, &type_refs, &arg_refs)?;
                let (chain, to) = chain_with_deployment(&contract)?;
                let from = dev_address(0);
                print!("{}", rrx_tools::gasfee::trace_execution(&chain, from, Some(to), data, U256::zero(), top)?);
            }
        },

        Command::Debug { contract, function, args, max_steps, json } => {
            let (_, _a, types, arg_strs, _o) = resolve_target(&contract, &function, &args)?;
            let arg_refs: Vec<&str> = arg_strs.iter().map(|s| s.as_str()).collect();
            let type_refs: Vec<&str> = types.iter().map(|s| s.as_str()).collect();
            let data = rrx_tools::deploy::encode_call(&function, &type_refs, &arg_refs)?;
            let (chain, to) = chain_with_deployment(&contract)?;
            let from = dev_address(0);
            // load artifacts for source maps
            let mut artifacts = vec![];
            if let Ok(entries) = std::fs::read_dir(ws_root().join("artifacts")) {
                for e in entries.flatten() {
                    if let Ok(content) = std::fs::read_to_string(e.path()) {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                            if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                                artifacts.push((format!("{name}.sol"), v));
                            }
                        }
                    }
                }
            }
            let opts = rrx_tools::debugger::DebugOptions {
                max_steps: max_steps.unwrap_or(80),
                breakpoint_line: None,
                json,
            };
            println!("{}", rrx_tools::debugger::debug_call(&chain, from, to, data, U256::zero(), &opts, &artifacts)?);
        }

        Command::Flatten { file, out } => {
            let flattened = rrx_tools::flatten::flatten(&ws_root(), &file)?;
            match out {
                Some(o) => {
                    std::fs::write(&o, &flattened).map_err(|e| e.to_string())?;
                    println!("flattened {} -> {} ({} bytes)", file, o, flattened.len());
                }
                None => print!("{}", flattened),
            }
        }

        Command::Uml => {
            let ws = rrx_tools::workspace::Workspace::at(&ws_root());
            let sources = rrx_tools::unittest::prepare_sources(&ws)?;
            let solc = rrx_solc::detect()?;
            let input = rrx_solc::build_input(&sources, false, "shanghai");
            let output = solc.compile(&input)?;
            print!("{}", rrx_tools::uml::mermaid(&output));
        }

        Command::Doc { out } => {
            let ws = rrx_tools::workspace::Workspace::at(&ws_root());
            let sources = rrx_tools::unittest::prepare_sources(&ws)?;
            let solc = rrx_solc::detect()?;
            let input = rrx_solc::build_input(&sources, false, "shanghai");
            let output = solc.compile(&input)?;
            let md = rrx_tools::docgen::generate(&output, &ws_root().file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
            match out {
                Some(o) => {
                    std::fs::write(&o, &md).map_err(|e| e.to_string())?;
                    println!("docs written to {o}");
                }
                None => print!("{md}"),
            }
        }

        Command::Script { file } => {
            let out = rrx_tools::script::run_script(&ws_root(), &ws_root().join(&file))?;
            println!("{out}");
        }

        Command::Git { action } => {
            let git = rrx_tools::minigit::MiniGit::at(&ws_root());
            match action {
                GitAction::Init => {
                    git.init()?;
                    println!("initialized .rrxgit");
                }
                GitAction::Add => {
                    let n = git.add_all(&ws_root())?;
                    println!("staged {n} file(s)");
                }
                GitAction::Commit { message } => {
                    let h = git.commit(&message)?;
                    println!("committed {h}");
                }
                GitAction::Status => {
                    let (m, a, d) = git.status(&ws_root())?;
                    for f in &a { println!("  new: {f}"); }
                    for f in &m { println!("  modified: {f}"); }
                    for f in &d { println!("  deleted: {f}"); }
                    println!("{} changed, {} new, {} deleted", m.len(), a.len(), d.len());
                }
                GitAction::Log => {
                    for line in git.log()? {
                        println!("{line}");
                    }
                }
            }
        }

        Command::Verify { api_key, chain_id, address, contract, compiler_version } => {
            let key = api_key
                .or_else(|| std::env::var("ETHERSCAN_API_KEY").ok().filter(|s| !s.is_empty()))
                .ok_or("missing API key — pass --api-key or set ETHERSCAN_API_KEY")?;
            let ws = rrx_tools::workspace::Workspace::at(&ws_root());
            let sources = ws.load_sources()?;
            let input = rrx_tools::verify::json_source_input(&sources, true, "shanghai");
            let (status, guid) = rrx_tools::verify::verify(&key, chain_id, &address, &contract, &compiler_version, input, true, 200)?;
            println!("{status} (guid {guid})");
        }

        Command::Keys { action } => match action {
            KeysAction::New => {
                let kp = rrx_crypto::KeyPair::new_random();
                println!("address: 0x{:x}", kp.address().unwrap());
                println!("secret : {}", rrx_util::to_hex_prefixed(&kp.secret));
                println!("(keep it secret — store offline)");
            }
            KeysAction::List => {
                for i in 0..10 {
                    println!("dev#{i}: 0x{:x}", dev_address(i));
                }
            }
            KeysAction::Dev => {
                for i in 0..10 {
                    println!("dev#{i}: 0x{:x}  key {}", dev_address(i), rrx_util::to_hex_prefixed(&dev_keypair(i).secret));
                }
            }
        },

        Command::Sign { message, key, account } => {
            let secret = match key {
                Some(k) => {
                    let kp = rrx_crypto::KeyPair::from_hex(&k)?;
                    kp.secret
                }
                None => dev_keypair(account.unwrap_or(0)).secret,
            };
            let sig = rrx_crypto::sign_message(message.as_bytes(), &secret)?;
            println!("signature: {}", rrx_util::to_hex_prefixed(&sig));
            let who = rrx_crypto::recover_message(&sig, message.as_bytes())?;
            println!("recovers to: 0x{who:x}");
        }

        Command::Precompiles { address, input_hex } => {
            let input = rrx_util::from_hex(&input_hex).unwrap_or_default();
            let out = rrx_precompiles::execute(address, &input, u64::MAX);
            match out {
                rrx_precompiles::PrecompileOutput::Success { gas_used, output } => {
                    println!("ok: {} (gas {gas_used})", rrx_util::to_hex_prefixed(&output));
                }
                rrx_precompiles::PrecompileOutput::Failure { .. } => {
                    println!("precompile failed (bad input)");
                }
            }
        }

        Command::Solc { action } => match action {
            SolcAction::Install { version } => {
                let v = match version {
                    Some(v) => v,
                    None => rrx_solc::latest_version()?,
                };
                println!("installing solc {v}…");
                let p = rrx_solc::install(&v)?;
                println!("installed at {}", p.display());
            }
            SolcAction::List => {
                println!("installed:");
                for v in rrx_solc::installed_versions() {
                    println!("  {v}");
                }
                println!("available (latest 10):");
                for v in rrx_solc::list_available()?.into_iter().take(10) {
                    println!("  {v}");
                }
            }
        },

        Command::Node { port, chain_id } => {
            rrx_rpc::server::serve(rrx_rpc::server::NodeOptions { port, chain_id, fund_dev_accounts: true })?;
        }

        Command::Gasprice { rpc, local } => {
            if local {
                let chain = fresh_chain();
                print!("{}", rrx_tools::gasfee::price_local(&chain).print("local chain"));
            } else {
                let p = rrx_rpc::resolve_endpoint(rpc.as_deref())?;
                let info = rrx_tools::gasfee::price_rpc(&p)?;
                print!("{}", info.print(&format!("LIVE {}", p.url)));
            }
        }

        Command::Net => {
            print!("{}", rrx_tools::netstatus::net_table());
        }

        Command::Chain { rpc } => {
            let p = rrx_rpc::resolve_endpoint(rpc.as_deref())?;
            print!("{}", rrx_tools::netstatus::chain_dashboard(&p)?);
        }

        Command::Tx { hash } => {
            let p = rrx_rpc::resolve_endpoint(None)?;
            print!("{}", rrx_tools::netstatus::tx_report(&p, &hash)?);
        }

        Command::Block { number } => {
            let p = rrx_rpc::resolve_endpoint(None)?;
            print!("{}", rrx_tools::netstatus::block_report(&p, &number)?);
        }

        Command::Account { address } => {
            let p = rrx_rpc::resolve_endpoint(None)?;
            print!("{}", rrx_tools::netstatus::account_report(&p, &address)?);
        }

        Command::Trace { hash } => {
            let p = rrx_rpc::resolve_endpoint(None)?;
            print!("{}", rrx_tools::netstatus::trace_report(&p, &hash)?);
        }

        Command::Ethcall { to, function, args, types, returns, rpc, block } => {
            let p = rrx_rpc::resolve_endpoint(rpc.as_deref())?;
            let split = |s: Option<String>| -> Vec<String> {
                s.unwrap_or_default()
                    .split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            };
            let type_list = split(types);
            let out_list = split(returns);
            let t: Vec<&str> = type_list.iter().map(|s| s.as_str()).collect();
            let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let o: Vec<&str> = out_list.iter().map(|s| s.as_str()).collect();
            let res = rrx_tools::netstatus::remote_call(&p, &to, &function, &t, &a, &o, &block)?;
            println!("{res}");
        }

        Command::Send { to, value, fn_sig, args, key, rpc, mainnet, yes, max_fee_gwei, max_priority_gwei } => {
            let khex = key
                .or_else(|| std::env::var("RRX_PRIVATE_KEY").ok().filter(|s| !s.is_empty()))
                .ok_or("no signing key — pass --key <hex> or set RRX_PRIVATE_KEY")?;
            let kp = rrx_crypto::KeyPair::from_hex(&khex)?;
            let p = resolve_write_endpoint(rpc.as_ref(), mainnet)?;
            confirm_mainnet(&p, yes)?;
            let recipient = rrx_tools::ens::resolve_or_address(&p, &to)?;
            let (res, fees_note) = if let Some(sig) = fn_sig {
                // state-changing contract call
                let types: Vec<&str> = sig[sig.find('(').ok_or("bad signature")? + 1..sig.len() - 1]
                    .split(',')
                    .map(|t| t.trim())
                    .filter(|t| !t.is_empty())
                    .collect();
                let arg_list: Vec<String> = args
                    .unwrap_or_default()
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                let arg_refs: Vec<&str> = arg_list.iter().map(|s| s.as_str()).collect();
                let name = &sig[..sig.find('(').ok_or("bad signature")?];
                let data = rrx_tools::deploy::encode_call(name, &types, &arg_refs)?;
                let val = value.map(|v| rrx_tools::send::parse_eth(&v)).transpose()?.unwrap_or_default();
                rrx_tools::send::write(&p, &kp, recipient, data, val, max_fee_gwei, max_priority_gwei)?
            } else {
                let amount = value.ok_or("plain transfers need --value <eth>")?;
                rrx_tools::send::transfer(&p, &kp, recipient, &amount, max_fee_gwei, max_priority_gwei)?
            };
            print!(
                "{}",
                rrx_tools::send::print_outcome(&res, &format!("send → 0x{recipient:x} @ {}", p.url), &fees_note)
            );
        }

        Command::Debugtx { hash, max_steps, top, rpc } => {
            let p = resolve_write_endpoint(rpc.as_ref(), false)?;
            println!("{}", rrx_tools::debugtx::debug_mainnet_tx(&p, &hash, max_steps, top)?);
        }

        Command::Logs { address, topic0, from, to, limit, rpc } => {
            let p = resolve_write_endpoint(rpc.as_ref(), false)?;
            let addr = match &address {
                Some(a) => Some(rrx_tools::ens::resolve_or_address(&p, a)?),
                None => None,
            };
            let topics: Vec<String> = match &topic0 {
                Some(sig) => vec![rrx_tools::logs::topic0_of(sig)],
                None => vec![],
            };
            println!("{}", rrx_tools::logs::query(&p, addr.as_ref(), &topics, &from, &to, limit)?);
        }

        Command::Ens { name, reverse, rpc } => {
            let p = resolve_write_endpoint(rpc.as_ref(), false)?;
            if reverse {
                let addr = rrx_util::h160_from_hex(&name)?;
                println!("{}", rrx_tools::ens::reverse_report(&p, &addr)?);
            } else {
                println!("{}", rrx_tools::ens::report(&p, &name)?);
            }
        }

        Command::Token { address, holder, rpc } => {
            let p = resolve_write_endpoint(rpc.as_ref(), false)?;
            println!("{}", rrx_tools::token::report(&p, &address, holder.as_deref())?);
        }

        Command::Sources { address, chain_id, etherscan_key, out } => {
            println!("{}", rrx_tools::sources::fetch_and_save(chain_id, &address, etherscan_key.as_deref(), Path::new(&out))?);
        }

        Command::Proof { address, slot, block, rpc } => {
            let p = resolve_write_endpoint(rpc.as_ref(), false)?;
            let addr = rrx_util::h160_from_hex(&address)?;
            let slots: Vec<U256> = slot
                .iter()
                .map(|s| u256_from_str(s))
                .collect::<Result<_, _>>()?;
            // header + proof must be from the SAME block — retry on reorg/race
            #[allow(unused_assignments)]
            let mut last_err = String::new();
            for attempt in 0..3 {
                let header = match p.block_by_tag(&block, false)? {
                    Some(b) => b,
                    None => return Err(format!("block '{block}' not found")),
                };
                let state_root = header
                    .get("stateRoot")
                    .and_then(|x| x.as_str())
                    .ok_or("header has no stateRoot")?
                    .to_string();
                let block_num = header
                    .get("number")
                    .and_then(|x| x.as_str())
                    .unwrap_or(&block)
                    .to_string();
                let proof = p.get_proof(&addr, &slots, &block_num)?;
                match rrx_tools::proofs::verify_get_proof_response(&proof, &state_root) {
                    Ok(report) => {
                        println!(
                            "state proof for 0x{addr:x} @ block {block_num} (attempt {})\n  state root  {state_root}\n  {}",
                            attempt + 1,
                            report
                        );
                        break;
                    }
                    Err(e) => {
                        last_err = e;
                        std::thread::sleep(std::time::Duration::from_millis(1500));
                    }
                }
                if attempt == 2 {
                    return Err(format!("proof verification failed after retries: {last_err}"));
                }
            }
        }

        Command::Follow { every, rpc } => {
            let p = resolve_write_endpoint(rpc.as_ref(), false)?;
            println!("following the head of chain {} — Ctrl-C to stop", p.url);
            let mut last = p.block_number()?;
            println!("  head: {}", last);
            loop {
                std::thread::sleep(std::time::Duration::from_secs(every));
                match p.block_number() {
                    Ok(n) if n > last => {
                        for b in (last + 1)..=n {
                            if let Ok(Some(header)) = p.block_by_number(b, false) {
                                let txs = header["transactions"].as_array().map(|a| a.len()).unwrap_or(0);
                                let base = header["baseFeePerGas"].as_str().and_then(|s| u256_from_str(s).ok()).unwrap_or_default();
                                let gwei = base / U256::from(1_000_000_000u64);
                                println!("  block {}  txs {}  base fee {} gwei", b, txs, gwei);
                            } else {
                                println!("  block {}", b);
                            }
                        }
                        last = n;
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("  (rpc hiccup: {e})"),
                }
            }
        }

        Command::Utils { action } => match action {
            UtilsAction::Keccak { input } => {
                println!("{}", rrx_tools::utils::keccak_of(&input));
            }
            UtilsAction::Unit { amount, from, to } => {
                println!("{}", rrx_tools::utils::unit_convert(&amount, &from, &to)?);
            }
            UtilsAction::Selector { signature } => {
                println!("{}", rrx_tools::utils::selector_of(&signature)?);
            }
            UtilsAction::Namehash { name } => {
                println!("{}", rrx_tools::ens::node_hex(&name));
            }
        },
    }
    Ok(())
}
