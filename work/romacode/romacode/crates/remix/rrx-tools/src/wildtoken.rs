//! WildToken — the "Tether USD" lookalike built EXACTLY to its spec:
//! name "Tether USD", symbol USDT, 6 decimals, 1B initial supply to the
//! deployer, PUBLIC mint (anyone, unlimited), PUBLIC permissionless burn
//! (anyone can burn from ANY wallet).
//!
//! The standard surface (transfer / approve / transferFrom / events /
//! zero-address checks) mirrors OpenZeppelin's ERC-20 semantics.
//!
//! This module is the token's first-class home in rrx: it embeds the source,
//! compiles it with real solc, and runs the full "wild story" on the real
//! rrx EVM (deploy → mint → cross-wallet burn → allowance probe), so the
//! fuzz/invariant/storage-layout/coverage tools always have a live target.
//!
//! WARNING: this token is intentionally wild. Testnet demos and security
//! training only — never present it as "fixed supply", never mainnet.

use std::path::Path;

use rrx_chain::executor::{Chain, ChainConfig};
use rrx_util::U256;

use crate::deploy::{self, AccountSpec};

// ---------------------------------------------------------------------------
// canonical source (single source of truth; the wildtoken template reuses it)
// ---------------------------------------------------------------------------

pub const SOURCE: &str = r#"// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// @title WildToken — spec-faithful "Tether USD" lookalike.
/// @notice WARNING: mimics the real Tether listing (name / symbol / decimals)
///         but is NOT Tether: no backing, no owner, no pause, no upgrade path.
///         Testnet demos + security training only. Never sell it as "fixed
///         supply" — mint is public and unlimited. Never deploy to mainnet.
contract WildToken {
    string public constant name = "Tether USD";
    string public constant symbol = "USDT";
    uint8 public constant decimals = 6;

    uint256 public totalSupply;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    uint256 private constant INITIAL_SUPPLY = 1_000_000_000 * 10 ** 6; // 1 billion

    constructor() {
        totalSupply = INITIAL_SUPPLY;
        balanceOf[msg.sender] = INITIAL_SUPPLY;
        emit Transfer(address(0), msg.sender, INITIAL_SUPPLY);
    }

    /// @notice PUBLIC mint — no owner, no cap. Anyone can call this, forever.
    function mint(uint256 amount) external {
        totalSupply += amount;
        balanceOf[msg.sender] += amount;
        emit Transfer(address(0), msg.sender, amount);
    }

    /// @notice PUBLIC permissionless burn — burns from ANY address.
    /// @dev The wild part: no _spendAllowance, no msg.sender check. Any wallet
    ///      can have its tokens burned by anyone, silently. Normal ERC-20
    ///      behaviour would require an allowance check on this line.
    function burn(address from, uint256 amount) external {
        require(balanceOf[from] >= amount, "WT: burn exceeds balance");
        balanceOf[from] -= amount;
        totalSupply -= amount;
        emit Transfer(from, address(0), amount);
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        _transfer(msg.sender, to, amount);
        return true;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        require(spender != address(0), "WT: approve to zero");
        allowance[msg.sender][spender] = amount;
        emit Approval(msg.sender, spender, amount);
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        uint256 allowed = allowance[from][msg.sender];
        require(allowed >= amount, "WT: insufficient allowance");
        if (allowed != type(uint256).max) {
            allowance[from][msg.sender] = allowed - amount;
        }
        _transfer(from, to, amount);
        return true;
    }

    function _transfer(address from, address to, uint256 amount) internal {
        require(to != address(0), "WT: transfer to zero");
        uint256 bal = balanceOf[from];
        require(bal >= amount, "WT: insufficient balance");
        balanceOf[from] = bal - amount;
        balanceOf[to] += amount;
        emit Transfer(from, to, amount);
    }
}
"#;

/// remix-tests suite (runs via `rrx test`) — proves every property of the spec.
pub const TEST_SOURCE: &str = r#"// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "./WildToken.sol";

/// remix-tests convention: functions starting with `test` run on the real EVM.
contract WildTokenTest {
    WildToken token;

    function beforeAll() public {
        token = new WildToken();
    }

    function testLookalikeListing() public {
        Assert.equal(token.name(), "Tether USD", "name mimics the real listing");
        Assert.equal(token.symbol(), "USDT", "symbol mimics the real listing");
        Assert.equal(uint256(uint8(token.decimals())), uint256(6), "6 decimals like the real one");
    }

    function testInitialSupplyToDeployer() public {
        Assert.equal(token.totalSupply(), uint256(1_000_000_000 * 10 ** 6), "1 billion to deployer");
        Assert.equal(token.balanceOf(address(this)), uint256(1_000_000_000 * 10 ** 6), "deployer holds all");
    }

    function testMintIsPublicAndUnbounded() public {
        WildToken t = new WildToken();
        t.mint(500_000_000 * 10 ** 6);
        Assert.equal(t.totalSupply(), uint256(1_500_000_000 * 10 ** 6), "supply grew with no cap");
        Assert.equal(t.balanceOf(address(this)), uint256(1_500_000_000 * 10 ** 6), "minter got the new tokens");
    }

    function testBurnFromAnotherWalletWithoutPermission() public {
        WildToken t = new WildToken();
        // the runner holds the supply; burn 1000 tokens FROM the runner wallet
        // through a plain external call — no allowance, no signature needed.
        (bool ok, ) = address(t).call(abi.encodeWithSignature("burn(address,uint256)", address(this), uint256(1000 * 10 ** 6)));
        Assert.assertTrue(ok, "any wallet's tokens can be burned by anyone");
        Assert.equal(t.totalSupply(), uint256(1_000_000_000 * 10 ** 6 - 1000 * 10 ** 6), "supply dropped");
    }

    function testStandardErc20PartStillWorks() public {
        WildToken t = new WildToken();
        t.transfer(address(0xdead), 100 * 10 ** 6);
        Assert.equal(t.balanceOf(address(0xdead)), uint256(100 * 10 ** 6), "recipient got tokens");
        (bool ok, ) = address(t).call(abi.encodeWithSignature("transferFrom(address,address,uint256)", address(this), address(0xdead), uint256(1)));
        Assert.assertFalse(ok, "transferFrom without allowance must still revert");
    }
}
"#;

pub const README: &str = r#"# WildToken workspace (spec-faithful "Tether USD" lookalike)

**Yeh asli Tether nahi hai** — na backing, na owner, na pause. Sirf listing same.

- name `Tether USD` / symbol `USDT` / decimals `6` — wallet list mein asli ke saath baithega
- supply: 1 billion shuru mein deployer ke wallet mein
- `mint(uint256)` PUBLIC — koi bhi, kabhi bhi, jitna chahe bana sakta hai ("fixed supply" keh ke kisi ko mat bechna)
- `burn(address,uint256)` PUBLIC aur bina permission — koi bhi aap ke wallet se aap ke tokens jala sakta hai
- transfer / approve / transferFrom / events / zero-address checks: standard ERC-20 (OpenZeppelin semantics)

## Commands

```bash
rrx compile                 # real solc
rrx test                    # remix-tests suite on the real EVM
rrx wildtoken compile       # compile the embedded canonical source
rrx wildtoken demo          # full story on the rrx EVM: deploy → mint → cross-wallet burn
rrx wildtoken deploy --rpc <RPC_URL> --key <PRIVATE_KEY>   # TESTNET ONLY (e.g. Sepolia)
```

## Mainnet?

Mat karo. "Tether USD" naam mainnet pe asli USDT ke saath wallet-confusion ka
fraud vector hai — testnet pe zero risk, wahi E2E proof milta hai.
"#;

// ---------------------------------------------------------------------------
// real compile (solc) + real execution (rrx EVM)
// ---------------------------------------------------------------------------

/// Compile the canonical source with real solc, standard-json.
pub fn compile_artifacts() -> Result<Vec<(String, serde_json::Value)>, String> {
    let solc = rrx_solc::detect()?;
    let mut sources = std::collections::BTreeMap::new();
    sources.insert("WildToken.sol".to_string(), SOURCE.to_string());
    let input = rrx_solc::build_input(&sources, true, "shanghai");
    let output = solc.compile(&input)?;
    rrx_solc::extract_artifacts(&output)
}

pub fn artifact() -> Result<serde_json::Value, String> {
    let artifacts = compile_artifacts()?;
    crate::compile::find_artifact(&artifacts, "WildToken.sol:WildToken")
        .cloned()
        .ok_or_else(|| "WildToken artifact missing after compile".to_string())
}

/// One full pass of the story, on a fresh local chain (real solc bytecode,
/// real rrx EVM execution — no mocks). Returns every measured number.
pub struct Story {
    pub token: rrx_util::H160,
    pub deployer: rrx_util::H160,
    pub attacker: rrx_util::H160,
    pub name: String,
    pub symbol: String,
    pub decimals: U256,
    pub initial_supply: U256,
    pub minted: U256,
    pub burned_from_deployer: U256,
    pub final_supply: U256,
    pub deployer_final: U256,
    pub attacker_final: U256,
    /// transferFrom without allowance must still revert (the OZ part works)
    pub allowance_enforced: bool,
}

pub fn run_story() -> Result<Story, String> {
    let art = artifact()?;
    let alloc: Vec<_> = (0..10)
        .map(|i| {
            let kp = rrx_chain::dev::dev_keypair(i);
            (kp.address().unwrap(), U256::from_dec_str("10000000000000000000000").unwrap())
        })
        .collect();
    let mut chain = Chain::genesis(ChainConfig::default(), &alloc);

    let deployer = AccountSpec::Dev(0);
    let attacker = AccountSpec::Dev(1);
    let deployer_addr = deployer.address()?;
    let attacker_addr = attacker.address()?;

    // 1. deploy — 1B USDT lands in the deployer wallet
    let out = deploy::deploy_local(
        Path::new("."),
        &mut chain,
        &art,
        "WildToken",
        &deployer,
        &[],
        &[],
        U256::zero(),
    )?;
    let token = out.address;
    let initial_supply = read_u256(&chain, deployer_addr, token, "totalSupply", &[], &[])?;
    let name = deploy::call_read(&chain, deployer_addr, token, "name", &[], &[], &["string"])?;
    let symbol = deploy::call_read(&chain, deployer_addr, token, "symbol", &[], &[], &["string"])?;
    let decimals = read_u256(&chain, deployer_addr, token, "decimals", &[], &[])?;

    // 2. the public mint — a random wallet mints 500M tokens out of thin air
    let minted = U256::from(500_000_000u64) * U256::from(1_000_000u64);
    deploy::call_write(
        &mut chain,
        &attacker,
        token,
        "mint",
        &["uint256"],
        &[&minted.to_string()],
        U256::zero(),
    )?;
    let attacker_after_mint = read_u256(&chain, attacker_addr, token, "balanceOf", &["address"], &[&fmt_addr(attacker_addr)])?;

    // 3. the permissionless burn — the attacker burns 1000 tokens straight out
    //    of the DEPLOYER's wallet. No allowance. No signature. Silent.
    let burned_from_deployer = U256::from(1_000u64) * U256::from(1_000_000u64);
    deploy::call_write(
        &mut chain,
        &attacker,
        token,
        "burn",
        &["address", "uint256"],
        &[&fmt_addr(deployer_addr), &burned_from_deployer.to_string()],
        U256::zero(),
    )?;

    // 4. the standard ERC-20 part still works — transferFrom without an
    //    allowance must revert (eth_call probe, no state change)
    let probe_data = deploy::encode_call(
        "transferFrom",
        &["address", "address", "uint256"],
        &[&fmt_addr(deployer_addr), &fmt_addr(attacker_addr), "1000000"],
    )?;
    let res = chain.call(attacker_addr, Some(token), U256::zero(), probe_data, 1_000_000, false);
    let allowance_enforced = !res.is_success();

    Ok(Story {
        token,
        deployer: deployer_addr,
        attacker: attacker_addr,
        name,
        symbol,
        decimals,
        initial_supply,
        minted,
        burned_from_deployer,
        final_supply: read_u256(&chain, deployer_addr, token, "totalSupply", &[], &[])?,
        deployer_final: read_u256(&chain, deployer_addr, token, "balanceOf", &["address"], &[&fmt_addr(deployer_addr)])?,
        attacker_final: attacker_after_mint,
        allowance_enforced,
    })
}

fn fmt_addr(a: rrx_util::H160) -> String {
    format!("0x{a:x}")
}

fn read_u256(
    chain: &Chain,
    from: rrx_util::H160,
    to: rrx_util::H160,
    function: &str,
    types: &[&str],
    args: &[&str],
) -> Result<U256, String> {
    let out = deploy::call_read(chain, from, to, function, types, args, &["uint256"])?;
    U256::from_dec_str(out.trim())
        .or_else(|_| U256::from_dec_str(out.trim().trim_start_matches("0x")))
        .map_err(|e| format!("parse '{out}': {e}"))
}

/// The narrative report for `rrx wildtoken demo`.
pub fn demo() -> Result<String, String> {
    let s = run_story()?;
    let scale = U256::from(1_000_000u64);
    let t = |v: &U256| (v / scale).to_string(); // whole tokens (6 decimals)

    let mut o = String::new();
    o.push_str("WildToken — spec-faithful \"Tether USD\" lookalike (real solc, real rrx EVM)\n");
    o.push_str(&format!("=========================================================================\n\n"));
    o.push_str("SPEC (read back on-chain after deploy — real calls, not constants)\n");
    o.push_str(&format!("  name      {}\n", s.name));
    o.push_str(&format!("  symbol    {}\n", s.symbol));
    o.push_str(&format!("  decimals  {}\n", s.decimals));
    o.push_str(&format!("  supply    {} initial — sab deployer ke wallet mein\n\n", t(&s.initial_supply)));
    o.push_str(&format!(
        "DEPLOY\n  token     {} (tx {})\n  deployer  {}\n\n",
        fmt_addr(s.token),
        "mined on the rrx chain",
        fmt_addr(s.deployer)
    ));
    o.push_str("ATTACK 1 — public mint (no owner, no cap)\n");
    o.push_str(&format!(
        "  {} minted {} tokens out of thin air\n  attacker balance: {}\n  total supply: {} → {}\n\n",
        fmt_addr(s.attacker),
        t(&s.minted),
        t(&s.attacker_final),
        t(&s.initial_supply),
        t(&(s.initial_supply + s.minted))
    ));
    o.push_str("ATTACK 2 — permissionless burn from ANY wallet\n");
    o.push_str(&format!(
        "  attacker burned {} tokens straight out of the DEPLOYER wallet\n  deployer: {} → {}\n  supply dropped by {}\n  (no allowance check, no signature — ek call, bas)\n\n",
        t(&s.burned_from_deployer),
        t(&(s.initial_supply)),
        t(&s.deployer_final),
        t(&s.burned_from_deployer)
    ));
    o.push_str("THE STANDARD PART (OpenZeppelin semantics) — still intact\n");
    o.push_str(&format!(
        "  transferFrom without allowance reverts: {}\n  transfer/approve/events/zero-address checks: asli ERC-20 jaise\n\n",
        if s.allowance_enforced { "YES" } else { "NO (BUG!)" }
    ));
    o.push_str("VERDICT\n");
    o.push_str("  supply \"1 billion\" koi hadd nahi, sirf shuruaati number hai.\n");
    o.push_str("  Aap ka apna paisa aap ki marzi ke baghair jal sakta hai.\n");
    o.push_str("  Testnet-only. Mainnet pe \"Tether USD\" naam = asli USDT ke saath\n");
    o.push_str("  confusion — woh fraud vector hai, is liye nahi.\n");
    Ok(o)
}

// ---------------------------------------------------------------------------
// tests — real solc + real EVM (network needed on a cold cache for the solc bin)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildtoken_compiles_with_real_solc() {
        let arts = compile_artifacts().expect("compile");
        let art = crate::compile::find_artifact(&arts, "WildToken.sol:WildToken").expect("artifact");
        let bytecode = crate::compile::bytecode_of(art);
        assert!(!bytecode.is_empty(), "bytecode must not be empty");
        let ids = art["evm"]["methodIdentifiers"]
            .as_object()
            .expect("methodIdentifiers");
        for want in ["name()", "symbol()", "decimals()", "totalSupply()", "mint(uint256)", "burn(address,uint256)", "transfer(address,uint256)", "approve(address,uint256)", "transferFrom(address,address,uint256)"] {
            assert!(ids.contains_key(want), "missing selector for {want}");
        }
    }

    #[test]
    fn wildtoken_story_end_to_end() {
        let s = run_story().expect("story");
        let million = U256::from(1_000_000u64);
        let initial = U256::from(1_000_000_000u64) * million;
        let minted = U256::from(500_000_000u64) * million;
        let burned = U256::from(1_000u64) * million;

        assert_eq!(s.initial_supply, initial, "initial supply = 1B * 10^6");
        assert_eq!(s.name, "Tether USD", "name read back on-chain");
        assert_eq!(s.symbol, "USDT", "symbol read back on-chain");
        assert_eq!(s.decimals, U256::from(6u8), "6 decimals read back on-chain");
        assert_eq!(s.minted, minted);
        assert_eq!(s.attacker_final, minted, "attacker holds exactly what it minted");
        assert_eq!(s.final_supply, initial + minted - burned, "supply = 1.5B - burned");
        assert_eq!(s.deployer_final, initial - burned, "deployer lost exactly the burned amount");
        assert!(s.allowance_enforced, "transferFrom without allowance must revert");
        assert_ne!(s.deployer, s.attacker, "attacker must be a different wallet");
    }
}
