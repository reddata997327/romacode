//! Plugin Manager (headless): the registry of every rrx tool and its Remix origin.

pub struct ToolInfo {
    pub id: &'static str,
    pub remix_plugin: &'static str,
    pub command: &'static str,
    pub description: &'static str,
}

pub const TOOLS: &[ToolInfo] = &[
    ToolInfo { id: "file", remix_plugin: "File Explorer (filePanel)", command: "rrx file ls|cat|write|rm|mv|mkdir|tree", description: "filesystem operations on the workspace" },
    ToolInfo { id: "search", remix_plugin: "Search in files", command: "rrx search <pattern> [--regex]", description: "text/regex search across the workspace" },
    ToolInfo { id: "compile", remix_plugin: "Solidity Compiler (solidity)", command: "rrx compile [--solc v] [--no-opt]", description: "real solc standard-json compilation → artifacts" },
    ToolInfo { id: "analyze", remix_plugin: "Solidity Static Analysis (solidityStaticAnalysis)", command: "rrx analyze", description: "AST security & gas checks (tx.origin, delegatecall, loops…)" },
    ToolInfo { id: "test", remix_plugin: "Solidity Unit Testing (solidityUnitTesting)", command: "rrx test", description: "remix-tests convention runner on a real chain" },
    ToolInfo { id: "debug", remix_plugin: "Debugger (debugger)", command: "rrx debug <target>", description: "step-through EVM debugger with source maps" },
    ToolInfo { id: "deploy", remix_plugin: "Deploy & Run Transactions (udapp)", command: "rrx deploy <Contract> [args…]", description: "sign & deploy contracts (local chain or real RPC)" },
    ToolInfo { id: "call", remix_plugin: "Deploy & Run Transactions", command: "rrx call/send", description: "read calls & signed state-changing transactions" },
    ToolInfo { id: "gas", remix_plugin: "Gas analytics (hardhat-style, built-in)", command: "rrx gas estimate|report|price|trace", description: "gas estimation, per-method reports, live prices, opcode metering" },
    ToolInfo { id: "flatten", remix_plugin: "Contract Flattener (flattener)", command: "rrx flatten <file>", description: "single-file export with resolved imports" },
    ToolInfo { id: "uml", remix_plugin: "UML Generator (solidityumlgen)", command: "rrx uml", description: "mermaid class diagram from the AST" },
    ToolInfo { id: "doc", remix_plugin: "Doc Generator (contractMetadata)", command: "rrx doc", description: "markdown docs from natspec" },
    ToolInfo { id: "script", remix_plugin: "Script Runner (scriptRunner)", command: "rrx script <file.rrx>", description: "deterministic deployment & assertion scripts" },
    ToolInfo { id: "templates", remix_plugin: "Template Explorer (wizard)", command: "rrx workspace init <template>", description: "scaffold workspaces (blank, storage, erc20)" },
    ToolInfo { id: "git", remix_plugin: "Git (dgit)", command: "rrx git init|add|commit|status|log", description: "content-addressed workspace snapshots" },
    ToolInfo { id: "verify", remix_plugin: "Contract Verification (contract-verification)", command: "rrx verify …", description: "Etherscan V2 verification for 60+ chains" },
    ToolInfo { id: "keys", remix_plugin: "Account management (udapp)", command: "rrx keys new|list|dev", description: "secp256k1 keypairs & deterministic dev accounts" },
    ToolInfo { id: "sign", remix_plugin: "External crypto (built-in)", command: "rrx sign|verify-message", description: "EIP-191 personal message signing/recovery" },
    ToolInfo { id: "precompiles", remix_plugin: "EVM internals (built-in)", command: "rrx precompiles", description: "execute the 9 real precompiles (ecrecover…blake2f)" },
    ToolInfo { id: "node", remix_plugin: "Remix VM / external node", command: "rrx node --port 8545", description: "real local JSON-RPC node (eth_* over HTTP)" },
    ToolInfo { id: "net", remix_plugin: "Network status (built-in)", command: "rrx net", description: "probe every mainnet endpoint live (latency, block, client)" },
    ToolInfo { id: "chain", remix_plugin: "Network status (built-in)", command: "rrx chain [--rpc url]", description: "live chain dashboard: block, base fee, priority fees" },
    ToolInfo { id: "tx", remix_plugin: "Block explorer (built-in)", command: "rrx tx <hash>", description: "live transaction + receipt report from mainnet" },
    ToolInfo { id: "block", remix_plugin: "Block explorer (built-in)", command: "rrx block <n|latest|finalized>", description: "live block summary from mainnet" },
    ToolInfo { id: "account", remix_plugin: "Account viewer (built-in)", command: "rrx account <addr>", description: "live balance / nonce / code of any address" },
    ToolInfo { id: "trace", remix_plugin: "Debugger (debugger)", command: "rrx trace <txhash>", description: "call-tree trace of a confirmed mainnet tx" },
    ToolInfo { id: "ethcall", remix_plugin: "Deploy & Run Transactions", command: "rrx ethcall <to> <fn> --types --returns", description: "read-only eth_call with ABI decode on any network" },
    ToolInfo { id: "send", remix_plugin: "Deploy & Run Transactions (udapp)", command: "rrx send --to <addr|ens> --value 0.01 | --fn <fn> --args", description: "signed ETH transfers & state-changing calls on the MAINCHAIN" },
    ToolInfo { id: "debugtx", remix_plugin: "Debugger (debugger)", command: "rrx debugtx <txhash> [--max-steps] [--top]", description: "replay a REAL mainnet tx in the rrx EVM: status + gas match, opcode profile, steps" },
    ToolInfo { id: "logs", remix_plugin: "Block explorer / terminal logs", command: "rrx logs --address <a> --topic0 <sig> --from --to", description: "live eth_getLogs with Transfer/Approval decoding" },
    ToolInfo { id: "ens", remix_plugin: "ENS (udapp name resolution)", command: "rrx ens <name> | rrx ens --reverse <addr>", description: "real ENS resolution & reverse lookup on the mainnet registry" },
    ToolInfo { id: "token", remix_plugin: "Contract interface reader", command: "rrx token <addr> [--holder <h>]", description: "ERC-20/721/1155 inspection via real eth_call" },
    ToolInfo { id: "wildtoken", remix_plugin: "WildToken demo (built-in)", command: "rrx wildtoken source|compile|demo|deploy", description: "spec-faithful \"Tether USD\" lookalike: public mint + permissionless burn, full story on the real EVM (testnet-only deployments)" },
    ToolInfo { id: "fakeasset", remix_plugin: "Fake-asset stack (built-in)", command: "rrx fakeasset registry|variants|source|compile|deploy|audit|verify|flasher|pending|demo", description: "detection stack: canonical USDT/USDC registry + CLONE verdicts, honeypot/tax/freeze/proxy variant audits with the victim's story, flasher credit:NO deposit checklist, pending RBF risk, screenshot fact-check" },
    ToolInfo { id: "sources", remix_plugin: "Contract Verification (fetch)", command: "rrx sources <addr> [--chain 1]", description: "fetch VERIFIED sources (Sourcify / Etherscan V2) into the workspace" },
    ToolInfo { id: "proof", remix_plugin: "State/Storage proofs (built-in)", command: "rrx proof <addr> [--slot N] [--block tag]", description: "eth_getProof + LOCAL MPT verification against the block state root" },
    ToolInfo { id: "follow", remix_plugin: "Network status (live head)", command: "rrx follow [--every 12]", description: "live mainnet head tracker: new blocks as they land" },
    ToolInfo { id: "utils", remix_plugin: "Utilities (built-in)", command: "rrx utils keccak|unit|selector|namehash", description: "keccak, wei/gwei/ether conversion, selectors, ENS namehash" },
    ToolInfo { id: "solc", remix_plugin: "Compiler manager", command: "rrx solc install|list|use", description: "download & pin solc versions" },
    ToolInfo { id: "plugins", remix_plugin: "Plugin Manager (pluginManager)", command: "rrx plugins", description: "this registry" },
    // ---- advanced tiers: multi-chain, fork, fuzz, security, indexer, AA, LSP ----
    ToolInfo { id: "chains", remix_plugin: "Multi-chain networks (built-in)", command: "rrx chains [slug] [--json]", description: "live status of 13 supported chains (ETH/L2s/alt) with failover lists" },
    ToolInfo { id: "fork", remix_plugin: "Fork testing (anvil-style, built-in)", command: "rrx fork <usdt|usdc|weth|dai|0x…> [--chain] [--block]", description: "download REAL chain state into the local MPT-backed EVM" },
    ToolInfo { id: "fuzz", remix_plugin: "Property testing (foundry-style)", command: "rrx fuzz <Contract> <fn> [--runs] [--seed]", description: "ABI-aware random inputs on the real EVM + counterexample shrinking" },
    ToolInfo { id: "invariant", remix_plugin: "Invariant testing (foundry-style)", command: "rrx invariant <Contract> [--sequences] [--depth]", description: "random call sequences + invariant_<C>() view enforcement" },
    ToolInfo { id: "reentrancy", remix_plugin: "Static Analysis (dynamic mode)", command: "rrx reentrancy", description: "CEI violation detection from REAL EVM execution traces" },
    ToolInfo { id: "layout", remix_plugin: "Upgrade safety (built-in)", command: "rrx layout <Old> <New>", description: "storage layout collision check via real solc storageLayout" },
    ToolInfo { id: "bytediff", remix_plugin: "Verification (bytecode diff)", command: "rrx bytediff <addr> <Contract>", description: "diff local build vs LIVE on-chain code (eth_getCode)" },
    ToolInfo { id: "coverage", remix_plugin: "Coverage (hardhat-style)", command: "rrx coverage", description: "function-level coverage measured on real EVM executions" },
    ToolInfo { id: "index", remix_plugin: "Block explorer (indexer)", command: "rrx index --blocks N", description: "index REAL blocks + txs from a live chain into the local store" },
    ToolInfo { id: "explorer", remix_plugin: "Block explorer (UI)", command: "rrx explorer --port 8088", description: "mini block explorer web UI + JSON API over REAL indexed data" },
    ToolInfo { id: "aa", remix_plugin: "Account Abstraction (ERC-4337)", command: "rrx aa --sender <addr> --call-data 0x…", description: "run the REAL EntryPoint bytecode+state in the rrx EVM" },
    ToolInfo { id: "mcp", remix_plugin: "AI integration (MCP)", command: "rrx mcp", description: "62-tool MCP server over stdio for Claude/Cursor/Cline" },
    ToolInfo { id: "lsp", remix_plugin: "Editor integration (LSP)", command: "rrx lsp", description: "Solidity language server with REAL solc diagnostics" },
];

pub fn print_registry() -> String {
    let mut out = String::new();
    out.push_str(&format!("{:<11} {:<42} {}\n", "tool", "remix origin", "command"));
    out.push_str(&"-".repeat(110));
    out.push('\n');
    for t in TOOLS {
        out.push_str(&format!("{:<11} {:<42} {}\n", t.id, t.remix_plugin, t.command));
    }
    out.push_str(&format!("\n{} tools\n", TOOLS.len()));
    out
}
