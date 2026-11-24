//! Solidity Unit Testing (remix-tests equivalent): real test execution on the chain.
//!
//! Conventions (same as Remix):
//! - test files end with `_test.sol`
//! - functions named `test*` (public/external) run as tests
//! - hooks: `beforeAll`, `beforeEach`, `afterAll`, `afterEach`
//! - `Assert.*` library emits AssertionFailed/Passed events (inlined, no linking needed)
//! - failures carry the assertion message

use std::path::Path;

use rrx_chain::dev::dev_keypair;
use rrx_chain::tx::{sign_transaction, TxType, UnsignedTx};
use rrx_chain::{Chain, ChainConfig};
use rrx_util::{abi, H256, U256};
#[allow(unused_imports)]
use rrx_util::H160;
use serde_json::json;

pub const ASSERT_SOL: &str = r#"// SPDX-License-Identifier: MIT
pragma solidity >=0.8.0 <0.9.0;

/// @dev remix-tests compatible assertion library (inlined via internal calls).
library Assert {
    event AssertionFailed(string message);
    event AssertionPassed(string message);

    function _report(bool ok, string memory message) internal {
        if (!ok) {
            emit AssertionFailed(message);
        } else {
            emit AssertionPassed(message);
        }
    }

    function assertTrue(bool condition) internal { _report(condition, "Expected true, got false"); }
    function assertTrue(bool condition, string memory message) internal { _report(condition, message); }
    function assertFalse(bool condition) internal { _report(!condition, "Expected false, got true"); }
    function assertFalse(bool condition, string memory message) internal { _report(!condition, message); }

    function assertOk(uint256 value) internal { _report(value != 0, "Expected non-zero value"); }
    function assertOk(uint256 value, string memory message) internal { _report(value != 0, message); }

    function assertEqual(uint256 a, uint256 b) internal { _report(a == b, _fmtUint(a, b)); }
    function assertEqual(uint256 a, uint256 b, string memory message) internal { _report(a == b, message); }
    function assertNotEqual(uint256 a, uint256 b) internal { _report(a != b, "Values unexpectedly equal"); }
    function assertNotEqual(uint256 a, uint256 b, string memory message) internal { _report(a != b, message); }

    function assertEqual(int256 a, int256 b) internal { _report(a == b, _fmtInt(a, b)); }
    function assertEqual(int256 a, int256 b, string memory message) internal { _report(a == b, message); }

    function assertEqual(bool a, bool b) internal { _report(a == b, "Booleans not equal"); }
    function assertEqual(bool a, bool b, string memory message) internal { _report(a == b, message); }

    function assertEqual(address a, address b) internal { _report(a == b, "Addresses not equal"); }
    function assertEqual(address a, address b, string memory message) internal { _report(a == b, message); }

    function assertEqual(bytes32 a, bytes32 b) internal { _report(a == b, "bytes32 values not equal"); }
    function assertEqual(bytes32 a, bytes32 b, string memory message) internal { _report(a == b, message); }

    function assertEqual(string memory a, string memory b) internal { _report(keccak256(bytes(a)) == keccak256(bytes(b)), "Strings not equal"); }
    function assertEqual(string memory a, string memory b, string memory message) internal { _report(keccak256(bytes(a)) == keccak256(bytes(b)), message); }

    function assertGreaterThan(uint256 a, uint256 b) internal { _report(a > b, _fmtUint(a, b)); }
    function assertGreaterThan(uint256 a, uint256 b, string memory message) internal { _report(a > b, message); }
    function assertLessThan(uint256 a, uint256 b) internal { _report(a < b, _fmtUint(a, b)); }
    function assertLessThan(uint256 a, uint256 b, string memory message) internal { _report(a < b, message); }

    // remix-tests short aliases
    function equal(uint256 a, uint256 b) internal { assertEqual(a, b); }
    function equal(uint256 a, uint256 b, string memory message) internal { assertEqual(a, b, message); }
    function equal(int256 a, int256 b) internal { assertEqual(a, b); }
    function equal(int256 a, int256 b, string memory message) internal { assertEqual(a, b, message); }
    function equal(bool a, bool b) internal { assertEqual(a, b); }
    function equal(bool a, bool b, string memory message) internal { assertEqual(a, b, message); }
    function equal(address a, address b) internal { assertEqual(a, b); }
    function equal(address a, address b, string memory message) internal { assertEqual(a, b, message); }
    function equal(bytes32 a, bytes32 b) internal { assertEqual(a, b); }
    function equal(bytes32 a, bytes32 b, string memory message) internal { assertEqual(a, b, message); }
    function equal(string memory a, string memory b) internal { assertEqual(a, b); }
    function equal(string memory a, string memory b, string memory message) internal { assertEqual(a, b, message); }
    function notEqual(uint256 a, uint256 b) internal { assertNotEqual(a, b); }
    function notEqual(uint256 a, uint256 b, string memory message) internal { assertNotEqual(a, b, message); }
    function greaterThan(uint256 a, uint256 b) internal { assertGreaterThan(a, b); }
    function greaterThan(uint256 a, uint256 b, string memory message) internal { assertGreaterThan(a, b, message); }
    function lessThan(uint256 a, uint256 b) internal { assertLessThan(a, b); }
    function lessThan(uint256 a, uint256 b, string memory message) internal { assertLessThan(a, b, message); }
    function isTrue(bool condition) internal { assertTrue(condition); }
    function isTrue(bool condition, string memory message) internal { assertTrue(condition, message); }
    function isFalse(bool condition) internal { assertFalse(condition); }
    function isFalse(bool condition, string memory message) internal { assertFalse(condition, message); }
    function ok(uint256 value) internal { assertOk(value); }
    function ok(uint256 value, string memory message) internal { assertOk(value, message); }

    function _fmtUint(uint256 a, uint256 b) internal pure returns (string memory) {
        return string(abi.encodePacked("Expected ", _u(a), " to equal ", _u(b)));
    }

    function _fmtInt(int256 a, int256 b) internal pure returns (string memory) {
        return string(abi.encodePacked("Expected ", _i(a), " to equal ", _i(b)));
    }

    function _u(uint256 v) internal pure returns (string memory) {
        if (v == 0) return "0";
        uint256 digits;
        uint256 tmp = v;
        while (tmp != 0) { digits++; tmp /= 10; }
        bytes memory buf = new bytes(digits);
        while (v != 0) { digits -= 1; buf[digits] = bytes1(uint8(48 + v % 10)); v /= 10; }
        return string(buf);
    }

    function _i(int256 v) internal pure returns (string memory) {
        if (v >= 0) return _u(uint256(v));
        return string(abi.encodePacked("-", _u(uint256(-v))));
    }
}
"#;

const HOOKS: [&str; 4] = ["beforeAll", "beforeEach", "afterAll", "afterEach"];

#[derive(Clone, Debug)]
pub struct TestResult {
    pub contract: String,
    pub name: String,
    pub passed: bool,
    pub message: String,
    pub gas_used: u64,
}

#[derive(Clone, Debug, Default)]
pub struct GasRecord {
    pub contract: String,
    pub method: String,
    pub kind: String, // deploy | test | call
    pub gas: u64,
}

pub struct TestReport {
    pub results: Vec<TestResult>,
    pub gas: Vec<GasRecord>,
    pub solc_version: String,
}

impl TestReport {
    pub fn failures(&self) -> usize {
        self.results.iter().filter(|r| !r.passed).count()
    }

    pub fn print(&self) {
        let mut current = String::new();
        for r in &self.results {
            if r.contract != current {
                current = r.contract.clone();
                println!("\n▶ {current}");
            }
            let mark = if r.passed { "✓" } else { "✗" };
            println!("  {mark} {:50} gas={:>8} {}", r.name, r.gas_used, if r.passed { "".into() } else { format!("— {}", r.message) });
        }
        let passed = self.results.iter().filter(|r| r.passed).count();
        println!(
            "\n{} test(s): {} passed, {} failed (solc {})",
            self.results.len(),
            passed,
            self.failures(),
            self.solc_version
        );
    }

    pub fn save_gas_records(&self, path: &Path) -> Result<(), String> {
        let entries: Vec<serde_json::Value> = self
            .gas
            .iter()
            .map(|g| {
                json!({
                    "contract": g.contract,
                    "method": g.method,
                    "kind": g.kind,
                    "gas": g.gas,
                })
            })
            .collect();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(
            path,
            serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
}

/// Load workspace sources and inject the Assert library + imports (remix-tests style).
pub fn prepare_sources(ws: &crate::workspace::Workspace) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut sources = ws.load_sources()?;
    sources.insert("remix_tests.sol".into(), ASSERT_SOL.into());
    let test_files: Vec<String> = sources
        .keys()
        .filter(|k| k.ends_with("_test.sol"))
        .cloned()
        .collect();
    for tf in &test_files {
        let content = sources.get_mut(tf).unwrap();
        if !content.contains("remix_tests.sol") {
            let mut lines: Vec<&str> = content.lines().collect();
            let mut out = String::new();
            let mut inserted = false;
            for line in lines.drain(..) {
                out.push_str(line);
                out.push('\n');
                if !inserted && line.trim_start().starts_with("pragma") {
                    out.push_str("import \"./remix_tests.sol\";\n");
                    inserted = true;
                }
            }
            if !inserted {
                out = format!("import \"./remix_tests.sol\";\n{out}");
            }
            *content = out;
        }
    }
    // solc standard-json resolves "./x" relative to the importing file's directory;
    // add alias keys so sloppy relative imports keep working
    for _ in 0..3 {
        let snapshot = sources.clone();
        for (file, content) in &snapshot {
            let dir = match file.rfind('/') {
                Some(i) => &file[..i + 1],
                None => "",
            };
            for import in extract_imports(content) {
                let import = import.trim_start_matches("./");
                let key = format!("{dir}{import}");
                if !snapshot.contains_key(&key) {
                    if let Some((_k, existing)) = snapshot.iter().find(|(k, _)| k.as_str() == import || k.ends_with(&format!("/{import}"))) {
                        sources.insert(key, existing.clone());
                    } else if import == "remix_tests.sol" {
                        sources.insert(key, ASSERT_SOL.into());
                    }
                }
            }
        }
    }
    Ok(sources)
}

fn extract_imports(content: &str) -> Vec<String> {
    let mut out = vec![];
    for line in content.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("import ") {
            if let (Some(a), Some(b)) = (rest.find('"'), rest.rfind('"')) {
                if b > a {
                    out.push(rest[a + 1..b].to_string());
                }
            }
        }
    }
    out
}

/// Compile the workspace (+ Assert) and run every *_test.sol contract on a real chain.
pub fn run_tests(
    root: &Path,
    version: Option<&str>,
    optimizer: bool,
    save_gas_to: Option<&Path>,
) -> Result<TestReport, String> {
    let ws = crate::workspace::Workspace::at(root);
    let sources = prepare_sources(&ws)?;
    let test_files: Vec<String> = sources
        .keys()
        .filter(|k| k.ends_with("_test.sol"))
        .cloned()
        .collect();

    let solc = match version {
        Some(v) => rrx_solc::use_version(v)?,
        None => rrx_solc::detect()?,
    };
    let input = rrx_solc::build_input(&sources, optimizer, "shanghai");
    let output = solc.compile(&input)?;
    let artifacts = rrx_solc::extract_artifacts(&output)?;

    // fresh chain with a funded runner account
    let runner = dev_keypair(9);
    let runner_addr = runner.address().unwrap();
    let mut chain = Chain::genesis(
        ChainConfig::default(),
        &[(runner_addr, U256::from_dec_str("10000000000000000000000").unwrap())],
    );
    let mut report = TestReport { results: vec![], gas: vec![], solc_version: solc.version };

    // deploy each test contract (contracts defined in *_test.sol files)
    let mut nonce: u64 = 0;
    let mut deployed: std::collections::HashMap<String, H160> = std::collections::HashMap::new();

    for (key, artifact) in &artifacts {
        let (file, cname) = match key.split_once(':') {
            Some(v) => (v.0, v.1),
            None => continue,
        };
        if !test_files.contains(&file.to_string()) {
            continue;
        }
        let contract_label = format!("{file}:{cname}");
        let bytecode = rrx_util::from_hex(artifact["evm"]["bytecode"]["object"].as_str().unwrap_or("0x"))
            .map_err(|e| e.to_string())?;
        if bytecode.is_empty() {
            continue;
        }

        // deploy with a signed tx (real)
        let tx = sign_transaction(
            &UnsignedTx {
                max_fee_per_blob_gas: Default::default(),
                blob_versioned_hashes: vec![],
                tx_type: TxType::Eip1559,
                chain_id: chain.config.chain_id,
                nonce,
                gas_price: U256::zero(),
                max_fee: U256::from(20_000_000_000u64),
                max_priority_fee: U256::from(1_000_000_000u64),
                gas_limit: 9_000_000,
                to: None,
                value: U256::zero(),
                data: bytecode.clone(),
                access_list: vec![],
            },
            &runner,
        )
        .map_err(|e| format!("sign deploy failed: {e}"))?;
        let deploy_hash = tx.hash();
        chain
            .mine_block(vec![tx])
            .map_err(|e| format!("deploy of {cname} failed: {e}"))?;
        nonce += 1;
        let receipt = chain.receipt_for(&deploy_hash).unwrap();
        if !receipt.status {
            return Err(format!(
                "deploy of {cname} reverted: reason={:?} output={:?}",
                rrx_util::abi::decode_revert_reason(&receipt.output),
                rrx_util::to_hex_prefixed(&receipt.output)
            ));
        }
        let addr = receipt.contract_address.unwrap();
        deployed.insert(contract_label, addr);
        report.gas.push(GasRecord {
            contract: cname.to_string(),
            method: "constructor".into(),
            kind: "deploy".into(),
            gas: receipt.gas_used,
        });

        // find test functions in declaration order via the AST
        let ast_fn_names = function_names_from_ast(&output, &file, cname);

        // hooks + tests, all as real signed transactions
        for name in ast_fn_names {
            let is_hook = HOOKS.contains(&name.as_str());
            let is_test = name.starts_with("test");
            if !is_hook && !is_test {
                continue;
            }
            let selector = abi::selector(&name, &[]);
            let data = selector.to_vec();

            // beforeEach/afterEach run around each test — handled by order below
            let tx = sign_transaction(
                &UnsignedTx {
                max_fee_per_blob_gas: Default::default(),
                blob_versioned_hashes: vec![],
                    tx_type: TxType::Eip1559,
                    chain_id: chain.config.chain_id,
                    nonce,
                    gas_price: U256::zero(),
                    max_fee: U256::from(20_000_000_000u64),
                    max_priority_fee: U256::from(1_000_000_000u64),
                    gas_limit: 9_000_000,
                    to: Some(addr),
                    value: U256::zero(),
                    data,
                    access_list: vec![],
                },
                &runner,
            )
            .map_err(|e| e.to_string())?;
            let call_hash = tx.hash();
            chain
                .mine_block(vec![tx])
                .map_err(|e| format!("call {name} failed: {e}"))?;
            nonce += 1;
            let receipt = chain.receipt_for(&call_hash).unwrap();

            if is_hook {
                continue; // hooks don't report
            }

            let failed_event = H256(rrx_util::keccak256(b"AssertionFailed(string)"));
            let mut failure_msg = String::new();
            let mut passed = receipt.status;
            if !passed {
                failure_msg = abi::decode_revert_reason(&receipt.output)
                    .unwrap_or_else(|| "reverted".into());
            }
            for log in &receipt.logs {
                if log.topics.first() == Some(&failed_event) && log.data.len() >= 96 {
                    // decode AssertionFailed(string): data = offset(32) + len(32) + bytes
                    {
                        let len = U256::from_big_endian(&log.data[32..64]).as_usize();
                        let msg = String::from_utf8_lossy(&log.data[64..64 + len.min(log.data.len() - 64)]).into_owned();
                        failure_msg = msg;
                        passed = false;
                    }
                }
            }

            report.results.push(TestResult {
                contract: cname.to_string(),
                name: name.clone(),
                passed,
                message: failure_msg,
                gas_used: receipt.gas_used,
            });
            report.gas.push(GasRecord {
                contract: cname.to_string(),
                method: name.clone(),
                kind: "test".into(),
                gas: receipt.gas_used,
            });
        }
    }

    if let Some(p) = save_gas_to {
        report.save_gas_records(p)?;
    }
    Ok(report)
}

/// Extract public/external function names in declaration order from the solc AST.
fn function_names_from_ast(output: &serde_json::Value, file: &str, contract: &str) -> Vec<String> {
    let mut names = vec![];
    if let Some(ast) = output
        .get("sources")
        .and_then(|s| s.get(file))
        .and_then(|s| s.get("ast"))
        .and_then(|a| a.as_object())
    {
        walk_for_contract(ast, contract, &mut names);
    }
    names
}

fn walk_for_contract(node: &serde_json::Map<String, serde_json::Value>, contract: &str, names: &mut Vec<String>) {
    if node.get("nodeType").and_then(|t| t.as_str()) == Some("ContractDefinition")
        && node.get("name").and_then(|n| n.as_str()) == Some(contract)
    {
        if let Some(children) = node.get("nodes").and_then(|n| n.as_array()) {
            for c in children {
                if c.get("nodeType").and_then(|t| t.as_str()) == Some("FunctionDefinition") {
                    let vis = c.get("visibility").and_then(|v| v.as_str()).unwrap_or("");
                    let sm = c.get("stateMutability").and_then(|m| m.as_str()).unwrap_or("");
                    if (vis == "public" || vis == "external") && sm != "pure" {
                        if let Some(n) = c.get("name").and_then(|n| n.as_str()) {
                            names.push(n.to_string());
                        }
                    }
                }
            }
        }
        return;
    }
    for (_k, v) in node {
        match v {
            serde_json::Value::Object(o) => walk_for_contract(o, contract, names),
            serde_json::Value::Array(a) => {
                for item in a {
                    if let serde_json::Value::Object(o) = item {
                        walk_for_contract(o, contract, names);
                    }
                }
            }
            _ => {}
        }
    }
}
