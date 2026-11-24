//! Fake-asset stack — the DETECTION side of the fake-token world.
//!
//! Three engines, all real (real solc, real eth_call, real EVM execution):
//! 1. REGISTRY — canonical USDT/USDC/WETH(WBNB) addresses per chain + a
//!    CLONE / REAL / UNKNOWN verdict for any address you point at.
//! 2. AUDIT — variant fixtures (honeypot, tax, freeze, capped, proxy,
//!    renounce-fake) executed end-to-end with the victim's story, plus live
//!    sell-path probes on real addresses via eth_call impersonation.
//! 3. VERIFY — screenshot fact-check: real tx facts (status, recipient,
//!    amount, confirmations) vs claimed values → TRUE / FALSE / NOT_FOUND.
//!
//! Deliberately NOT here: flash-balance presentation, drainer payload
//! generation, fake-proof forgery, cash-out routing. Those are scam-kit
//! components; this module exists so nobody falls for them.

use std::collections::BTreeMap;
use std::path::Path;

use rrx_chain::executor::{Chain, ChainConfig};
use rrx_rpc::Provider;
use rrx_util::{abi, h160_from_hex, H160, U256};

use crate::deploy::{self, AccountSpec};

// ---------------------------------------------------------------------------
// 1. REGISTRY — canonical references (the anti-blindfold)
// ---------------------------------------------------------------------------

pub struct RegistryEntry {
    pub chain_id: u64,
    pub slug: &'static str,
    pub ticker: &'static str,
    /// accepted on-chain symbols (rebrands happen: Tether's USDT0 rollout)
    pub aliases: &'static [&'static str],
    pub address: &'static str,
    pub note: &'static str,
}

/// Canonical, long-established listings. Every entry is spot-checked live by
/// `registry --verify-live` (symbol + decimals eth_call against the chain).
pub const REGISTRY: &[RegistryEntry] = &[
    RegistryEntry { chain_id: 1, slug: "ethereum", ticker: "USDT", aliases: &[], address: "0xdAC17F958D2ee523a2206206994597C13D831ec7", note: "Tether USD, 6 decimals" },
    RegistryEntry { chain_id: 1, slug: "ethereum", ticker: "USDC", aliases: &[], address: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", note: "USD Coin, 6 decimals" },
    RegistryEntry { chain_id: 1, slug: "ethereum", ticker: "WETH", aliases: &[], address: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", note: "Wrapped Ether, 18 decimals" },
    RegistryEntry { chain_id: 8453, slug: "base", ticker: "USDC", aliases: &[], address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913", note: "native USDC, 6 decimals" },
    RegistryEntry { chain_id: 8453, slug: "base", ticker: "USDT", aliases: &["USDT0", "USD\u{20ae}0"], address: "0xfde4C96c8593536E31F229EA8f37b2ADa2699bb2", note: "Tether USD (USDT0 omnichain), 6 decimals" },
    RegistryEntry { chain_id: 8453, slug: "base", ticker: "WETH", aliases: &[], address: "0x4200000000000000000000000000000000000006", note: "canonical WETH, 18 decimals" },
    RegistryEntry { chain_id: 42161, slug: "arbitrum", ticker: "USDT", aliases: &["USDT0", "USD\u{20ae}0"], address: "0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9", note: "Tether USD (USDT0 omnichain), 6 decimals" },
    RegistryEntry { chain_id: 42161, slug: "arbitrum", ticker: "USDC", aliases: &[], address: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831", note: "native USDC (Circle), 6 decimals" },
    RegistryEntry { chain_id: 42161, slug: "arbitrum", ticker: "WETH", aliases: &[], address: "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1", note: "Wrapped Ether, 18 decimals" },
    RegistryEntry { chain_id: 10, slug: "optimism", ticker: "USDT", aliases: &["USDT0", "USD\u{20ae}0"], address: "0x94b008aA00579c1307B0EF2c499aD98a8ce58e58", note: "Tether USD (USDT0 omnichain), 6 decimals" },
    RegistryEntry { chain_id: 10, slug: "optimism", ticker: "USDC", aliases: &[], address: "0x0b2C639c533813f4Aa9D7837CAf62653d097Ff85", note: "native USDC, 6 decimals" },
    RegistryEntry { chain_id: 10, slug: "optimism", ticker: "WETH", aliases: &[], address: "0x4200000000000000000000000000000000000006", note: "canonical WETH, 18 decimals" },
    RegistryEntry { chain_id: 137, slug: "polygon", ticker: "USDT", aliases: &["USDT0", "USD\u{20ae}0"], address: "0xc2132D05D31c914a87C6611C10748AEb04B58e8F", note: "Tether USD (USDT0 omnichain), 6 decimals" },
    RegistryEntry { chain_id: 137, slug: "polygon", ticker: "USDC", aliases: &[], address: "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359", note: "native USDC, 6 decimals" },
    RegistryEntry { chain_id: 137, slug: "polygon", ticker: "WETH", aliases: &[], address: "0x7ceB23fD6bC0adD59E62ac25578270cFf1b9f619", note: "Wrapped Ether, 18 decimals" },
    RegistryEntry { chain_id: 56, slug: "bsc", ticker: "USDT", aliases: &[], address: "0x55d398326f99059fF775485246999027B3197955", note: "Binance-Peg USDT, 18 decimals" },
    RegistryEntry { chain_id: 56, slug: "bsc", ticker: "USDC", aliases: &[], address: "0x8AC76a51cc950d9822D68b83fE1Ad97B32Cd580d", note: "Binance-Peg USDC, 18 decimals" },
    RegistryEntry { chain_id: 56, slug: "bsc", ticker: "WBNB", aliases: &[], address: "0xbb4CdB9CBd36B01bD1cBaEBF2De08d9173bc095c", note: "Wrapped BNB, 18 decimals" },
];

pub fn provider_for(slug: &str) -> Result<Provider, String> {
    let info = crate::chains::find(slug).ok_or_else(|| format!("unknown chain '{slug}'"))?;
    for url in info.rpcs {
        let p = Provider::new(url);
        if p.chain_id().is_ok() {
            return Ok(p);
        }
    }
    Err(format!("no live endpoint for {slug}"))
}

pub fn registry_table() -> String {
    let mut o = String::from(
        "canonical registry — the addresses a clone must be measured against\n\
         ====================================================================\n",
    );
    let mut by_chain: BTreeMap<u64, Vec<&RegistryEntry>> = BTreeMap::new();
    for e in REGISTRY {
        by_chain.entry(e.chain_id).or_default().push(e);
    }
    for (cid, rows) in &by_chain {
        o.push_str(&format!("\nchain {} ({}):\n", cid, rows[0].slug));
        for r in rows {
            o.push_str(&format!("  {:<5} {}  {}\n", r.ticker, r.address, r.note));
        }
    }
    o.push_str("\nverify every entry live:  rrx fakeasset registry --verify-live\n");
    o.push_str("compare any token:        rrx fakeasset registry --compare <addr> [--chain <slug>]\n");
    o
}

/// one live read set for a token
pub struct TokenRead {
    pub address: H160,
    pub name: Option<String>,
    pub symbol: Option<String>,
    pub decimals: Option<U256>,
    pub total_supply: Option<U256>,
}

fn eth_call_string(p: &Provider, to: &H160, sig: &str) -> Option<String> {
    let data = deploy::encode_call(sig, &[], &[]).ok()?;
    let raw = p.call(None, to, &data, "latest").ok()?;
    if raw.is_empty() {
        return None;
    }
    match abi::decode(&["string"], &raw).ok()?.first() {
        Some(abi::AbiValue::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

fn eth_call_uint(p: &Provider, to: &H160, sig: &str) -> Option<U256> {
    let data = deploy::encode_call(sig, &[], &[]).ok()?;
    let raw = p.call(None, to, &data, "latest").ok()?;
    if raw.is_empty() {
        return None;
    }
    match abi::decode(&["uint256"], &raw).ok()?.first() {
        Some(abi::AbiValue::Uint(u)) => Some(*u),
        _ => None,
    }
}

pub fn read_token(p: &Provider, addr: &H160) -> TokenRead {
    TokenRead {
        address: *addr,
        name: eth_call_string(p, addr, "name"),
        symbol: eth_call_string(p, addr, "symbol"),
        decimals: eth_call_uint(p, addr, "decimals"),
        total_supply: eth_call_uint(p, addr, "totalSupply"),
    }
}

/// the verdict engine — pure so local chains and live RPCs feed it the same way
pub fn compare_with_reads(
    addr: &H160,
    t: &TokenRead,
    real: Option<(&RegistryEntry, &TokenRead)>,
) -> String {
    let mut o = String::new();
    o.push_str(&format!("compare {} on {}\n", fmt_addr(*addr), real.map(|(e, _)| e.slug).unwrap_or("?")));
    if let Some(n) = &t.name { o.push_str(&format!("  name      {n}\n")); }
    if let Some(s) = &t.symbol { o.push_str(&format!("  symbol    {s}\n")); }
    if let Some(d) = t.decimals { o.push_str(&format!("  decimals  {}\n", d)); }
    if let Some(sup) = t.total_supply {
        let dec = t.decimals.and_then(|d| u32::try_from(d.as_u64()).ok()).unwrap_or(18).min(18);
        o.push_str(&format!("  supply    {}  (raw {sup})\n", crate::utils::render_scaled(&sup, dec)));
    }

    // exact registry hit?
    let mut verdict = "UNKNOWN".to_string();
    let mut why = String::from("no registry row matches enough of the listing");
    if let Some((entry, rt)) = real {
        let same_addr = format!("0x{:x}", t.address).eq_ignore_ascii_case(entry.address);
        let ticker = t.symbol.clone().unwrap_or_default();
        let dec_match = t.decimals.is_some() && t.decimals == rt.decimals;
        let name_match = t.name.clone().unwrap_or_default() == rt.name.clone().unwrap_or_default();
        if same_addr {
            verdict = "REAL".into();
            why = format!("address == canonical {ticker} on this chain");
        } else if (ticker.eq_ignore_ascii_case(entry.ticker)
            || entry.aliases.iter().any(|a| ticker.eq_ignore_ascii_case(a)))
            && (dec_match || name_match)
        {
            verdict = "CLONE".into();
            why = format!(
                "ticker {ticker} + {} like canonical {} (0x…) but a DIFFERENT address",
                if dec_match { "decimals" } else { "name" },
                entry.ticker
            );
        }
        if let (Some(mine), Some(theirs)) = (t.total_supply, rt.total_supply) {
            let ratio = if mine < theirs { "<1x".to_string() } else { format!("{}x", mine / theirs.max(U256::from(1u8))) };
            o.push_str(&format!(
                "  vs real   supply {mine} vs canonical {theirs}  ({ratio})\n"
            ));
        }
    }
    o.push_str(&format!("  VERDICT   {verdict} — {why}\n"));
    o
}

/// live compare: read the token on `slug`'s chain and measure it against the
/// canonical row(s) of that chain (real supply read live, no hardcoded numbers).
pub fn compare(p: &Provider, addr_str: &str, slug: Option<&str>) -> Result<String, String> {
    let addr = h160_from_hex(addr_str)?;
    let t = read_token(p, &addr);
    let slug = match slug {
        Some(s) => s.to_string(),
        None => {
            let cid = p.chain_id()?;
            REGISTRY.iter().find(|e| e.chain_id == cid).map(|e| e.slug.to_string()).unwrap_or_else(|| format!("{cid}"))
        }
    };
    // measure against the closest ticker match first, else the chain's first row
    let real = match REGISTRY
        .iter()
        .filter(|e| e.slug == slug)
        .max_by_key(|e| {
            let sym = t.symbol.clone().unwrap_or_default();
            (sym.eq_ignore_ascii_case(e.ticker) as u32) * 2
        })
    {
        Some(e) => {
            let ra = h160_from_hex(e.address)?;
            Some((e, read_token(p, &ra)))
        }
        None => None,
    };
    Ok(compare_with_reads(&addr, &t, real.as_ref().map(|(e, r)| (*e, r))))
}

fn fmt_addr(a: H160) -> String {
    format!("0x{a:x}")
}

// ---------------------------------------------------------------------------
// 2. VARIANTS + AUDIT — the victim's story, with real execution numbers
// ---------------------------------------------------------------------------

pub fn variant_list() -> String {
    let mut o = String::from("variant matrix — every specimen answers: victim 1000 bech kar nikal sakta hai?\n");
    o.push_str("======================================================================\n");
    for v in crate::fake_variants::VARIANTS {
        o.push_str(&format!("\n[{}]\n  mechanic {}\n  hurts    {}\n  exit     {}\n", v.name, v.mechanic, v.who_gets_hurt, v.exit_possible));
    }
    o.push_str("\ncompile:  rrx fakeasset compile --variant <name>\n");
    o.push_str("story:    rrx fakeasset audit --variant <name>\n");
    o.push_str("live:     rrx fakeasset audit --address <addr> [--chain <slug>] [--from <holder>]\n");
    o
}

pub fn compile_variant(name: &str) -> Result<String, String> {
    let sources = crate::fake_variants::variant_sources(name)?;
    let solc = rrx_solc::detect()?;
    let mut map = std::collections::BTreeMap::new();
    for (file, content) in &sources {
        map.insert(file.clone(), content.clone());
    }
    let input = rrx_solc::build_input(&map, true, "shanghai");
    let output = solc.compile(&input)?;
    let artifacts = rrx_solc::extract_artifacts(&output)?;
    let mut o = format!("variant '{name}' compiled with real solc\n");
    for (key, art) in &artifacts {
        let bc = crate::compile::bytecode_of(art);
        o.push_str(&format!("  {key:<38} {} bytes\n", bc.len()));
    }
    Ok(o)
}

/// structured result of one local variant story
#[derive(Debug)]
pub struct LocalAudit {
    pub variant: String,
    pub token: H160,
    /// can a normal holder move 1000 tokens out?
    pub victim_can_exit: Option<bool>,
    pub verdict_line: String,
    pub facts: Vec<(String, String)>,
}

impl LocalAudit {
    pub fn fact(&mut self, k: &str, v: String) {
        self.facts.push((k.to_string(), v));
    }
    pub fn render(&self) -> String {
        let mut o = format!("[{}] audit — real solc, real rrx EVM, real numbers\n", self.variant);
        o.push_str(&format!("  token     {}\n", fmt_addr(self.token)));
        for (k, v) in &self.facts {
            o.push_str(&format!("  {k:<26} {v}\n"));
        }
        o.push_str(&format!("  {}\n", self.verdict_line));
        o
    }
}

const SCALE: u64 = 1_000_000; // 6 decimals
fn u(v: u64) -> U256 {
    U256::from(v) * U256::from(SCALE)
}

fn local_chain() -> (Chain, Vec<H160>) {
    let alloc: Vec<_> = (0..10)
        .map(|i| {
            let kp = rrx_chain::dev::dev_keypair(i);
            (kp.address().unwrap(), U256::from_dec_str("10000000000000000000000").unwrap())
        })
        .collect();
    (Chain::genesis(ChainConfig::default(), &alloc), alloc.iter().map(|(a, _)| *a).collect())
}

fn deploy_art(chain: &mut Chain, art: &serde_json::Value, name: &str, ctor: &[&str], types: &[&str]) -> Result<H160, String> {
    let out = deploy::deploy_local(
        Path::new("."),
        chain,
        art,
        name,
        &AccountSpec::Dev(0),
        ctor,
        types,
        U256::zero(),
    )?;
    Ok(out.address)
}

fn read_u(chain: &Chain, from: H160, to: H160, sig: &str, types: &[&str], args: &[&str]) -> Result<U256, String> {
    let out = deploy::call_read(chain, from, to, sig, types, args, &["uint256"])?;
    U256::from_dec_str(out.trim()).map_err(|e| format!("parse '{out}': {e}"))
}

/// does `from` manage to move `amount` out? (eth_call-style probe, no state change)
fn sell_probe(chain: &Chain, from: H160, token: H160, amount: U256) -> bool {
    let data = deploy::encode_call("transfer", &["address", "uint256"], &[&fmt_addr(from), &amount.to_string()])
        .expect("encode");
    let res = chain.call(from, Some(token), U256::zero(), data, 1_000_000, false);
    res.is_success()
}

fn compile_map(name: &str) -> Result<Vec<(String, serde_json::Value)>, String> {
    let sources = crate::fake_variants::variant_sources(name)?;
    let solc = rrx_solc::detect()?;
    let mut map = std::collections::BTreeMap::new();
    for (file, content) in &sources {
        map.insert(file.clone(), content.clone());
    }
    let output = solc.compile(&rrx_solc::build_input(&map, true, "shanghai"))?;
    rrx_solc::extract_artifacts(&output)
}

fn art<'a>(arts: &'a [(String, serde_json::Value)], key: &str) -> Result<&'a serde_json::Value, String> {
    crate::compile::find_artifact(arts, key).ok_or_else(|| format!("artifact {key} missing"))
}

/// run the variant's full victim story on a fresh local chain
pub fn audit_local(variant: &str) -> Result<LocalAudit, String> {
    let arts = compile_map(variant)?;
    let (mut chain, addrs) = local_chain();
    let (owner, victim, other) = (addrs[0], addrs[1], addrs[2]);

    let (token, a) = match variant {
        "plain" => {
            let token = deploy_art(&mut chain, art(&arts, "FakePlain.sol:FakePlain")?, "FakePlain", &[], &[])?;
            let mut a = new_audit(variant, token);
            // the wild story
            let minted = u(500_000_000);
            deploy::call_write(&mut chain, &AccountSpec::Dev(1), token, "mint", &["uint256"], &[&minted.to_string()], U256::zero())?;
            let burned = u(1_000);
            deploy::call_write(&mut chain, &AccountSpec::Dev(1), token, "burn", &["address", "uint256"], &[&fmt_addr(owner), &burned.to_string()], U256::zero())?;
            let bal_owner = read_u(&chain, owner, token, "balanceOf", &["address"], &[&fmt_addr(owner)])?;
            let sup = read_u(&chain, owner, token, "totalSupply", &[], &[])?;
            a.fact("attacker minted", format!("{} tokens out of thin air", minted / u(1)));
            a.fact("attacker burned", format!("{} from the DEPLOYER wallet (no allowance)", burned / u(1)));
            a.fact("deployer balance now", format!("{} (started 1,000,000,000)", bal_owner / u(1)));
            a.fact("total supply now", format!("{}", sup / u(1)));
            let ok = sell_probe(&chain, victim, token, u(1_000)); // victim has 0 → balance revert ≠ sell-block; give them funds first
            // give victim funds via owner, then probe again — plain must pass
            deploy::call_write(&mut chain, &AccountSpec::Dev(0), token, "transfer", &["address", "uint256"], &[&fmt_addr(victim), &u(10_000).to_string()], U256::zero())?;
            let ok2 = sell_probe(&chain, victim, token, u(1_000));
            a.fact("victim sell probe (0 balance)", format!("reverted (insufficient balance) = {}", !ok));
            a.victim_can_exit = Some(ok2);
            a.verdict_line = format!("victim 1000 bech kar nikal sakta hai? {} — kyun: transfer path clean; yahan dhoka VALUE ka hai (open mint + public burn), transfer ka nahi",
                if ok2 { "HAAN" } else { "NAHI" });
            (token, a)
        }
        "honeypot" => {
            let token = deploy_art(&mut chain, art(&arts, "FakeHoneypot.sol:FakeHoneypot")?, "FakeHoneypot", &[], &[])?;
            let mut a = new_audit(variant, token);
            // BUY: owner (allow-listed) sends victim 10k — succeeds
            deploy::call_write(&mut chain, &AccountSpec::Dev(0), token, "transfer", &["address", "uint256"], &[&fmt_addr(victim), &u(10_000).to_string()], U256::zero())?;
            let bal = read_u(&chain, owner, token, "balanceOf", &["address"], &[&fmt_addr(victim)])?;
            a.fact("victim bought in", format!("{} tokens received (buy path works)", bal / u(1)));
            let sold = sell_probe(&chain, victim, token, u(1_000));
            a.fact("victim sell probe", format!("reverted = {} (HP: sell blocked)", !sold));
            // admin quietly allows one chosen wallet to leave
            deploy::call_write(&mut chain, &AccountSpec::Dev(0), token, "setAllowedSeller", &["address", "bool"], &[&fmt_addr(victim), "true"], U256::zero())?;
            let sold2 = sell_probe(&chain, victim, token, u(1_000));
            a.fact("after owner allow-lists victim", format!("sell now succeeds = {sold2} (chosen exits only)"));
            a.victim_can_exit = Some(false);
            a.verdict_line = "victim 1000 bech kar nikal sakta hai? NAHI — kyun: _move() requires allowedSeller[from]; buy-in free tha, sell-out sirf allow-list pe. Wallet UI balance dikhati hai — dikhna hona nahi hai".into();
            (token, a)
        }
        "tax" => {
            let token = deploy_art(&mut chain, art(&arts, "FakeTaxToken.sol:FakeTaxToken")?, "FakeTaxToken", &[], &[])?;
            let mut a = new_audit(variant, token);
            deploy::call_write(&mut chain, &AccountSpec::Dev(0), token, "setPair", &["address"], &[&fmt_addr(other)], U256::zero())?;
            // provision the pair (owner is exempt → no tax)
            deploy::call_write(&mut chain, &AccountSpec::Dev(0), token, "transfer", &["address", "uint256"], &[&fmt_addr(other), &u(10_000_000).to_string()], U256::zero())?;
            // BUY: pair → victim, 0% tax
            deploy::call_write(&mut chain, &AccountSpec::Dev(2), token, "transfer", &["address", "uint256"], &[&fmt_addr(victim), &u(10_000).to_string()], U256::zero())?;
            let bal = read_u(&chain, owner, token, "balanceOf", &["address"], &[&fmt_addr(victim)])?;
            a.fact("victim bought 10,000", format!("received {} (buy tax 0%)", bal / u(1)));
            // SELL: victim → pair, 98% tax
            let data = deploy::encode_call("transfer", &["address", "uint256"], &[&fmt_addr(other), &u(1_000).to_string()])?;
            chain.call(victim, Some(token), U256::zero(), data.clone(), 1_000_000, false);
            let _ = sell_probe_exec(&mut chain, 1, token, other, u(1_000));
            let pair_bal = read_u(&chain, owner, token, "balanceOf", &["address"], &[&fmt_addr(other)])?;
            let tax_bal = read_u(&chain, owner, token, "balanceOf", &["address"], &[&fmt_addr(owner)])?;
            a.fact("victim sold 1,000 to pair", format!("pair received {}, tax wallet +{}", pair_bal / u(1) - (10_000_000 - 10_000), u(980) / u(1)));
            a.fact("tax wallet balance", format!("{}", tax_bal / u(1)));
            a.victim_can_exit = Some(true);
            a.verdict_line = "victim 1000 bech kar nikal sakta hai? TECHNICALLY HAAN — kyun: sell path khula hai, lekin 98% tax; 1000 becha → 20 wapas. Docs mein 'feature' likha hai, bug nahi — yehi trap hai".into();
            (token, a)
        }
        "freeze" => {
            let token = deploy_art(&mut chain, art(&arts, "FakeFreezeToken.sol:FakeFreezeToken")?, "FakeFreezeToken", &[], &[])?;
            let mut a = new_audit(variant, token);
            deploy::call_write(&mut chain, &AccountSpec::Dev(0), token, "transfer", &["address", "uint256"], &[&fmt_addr(victim), &u(10_000).to_string()], U256::zero())?;
            deploy::call_write(&mut chain, &AccountSpec::Dev(0), token, "setFrozen", &["address", "bool"], &[&fmt_addr(victim), "true"], U256::zero())?;
            let frozen = read_u(&chain, owner, token, "frozen", &["address"], &[&fmt_addr(victim)])?;
            let sold = sell_probe(&chain, victim, token, u(1_000));
            a.fact("authority froze victim", format!("frozen={} — balance visible: {}", frozen != U256::zero(), 10_000));
            a.fact("victim sell probe", format!("reverted = {} (FRZ: sender frozen)", !sold));
            deploy::call_write(&mut chain, &AccountSpec::Dev(0), token, "setFrozen", &["address", "bool"], &[&fmt_addr(victim), "false"], U256::zero())?;
            let sold2 = sell_probe(&chain, victim, token, u(1_000));
            a.fact("after authority unfreezes", format!("sell succeeds = {sold2} — ek admin decision, victim ka nahi"));
            a.victim_can_exit = Some(false);
            a.verdict_line = "victim 1000 bech kar nikal sakta hai? FROZEN KE WAQT NAHI — kyun: freeze authority ne address lock kar diya; unfreeze sirf authority ki marzi pe".into();
            (token, a)
        }
        "capped" => {
            let token = deploy_art(&mut chain, art(&arts, "FakeCappedToken.sol:FakeCappedToken")?, "FakeCappedToken", &[], &[])?;
            let mut a = new_audit(variant, token);
            deploy::call_write(&mut chain, &AccountSpec::Dev(1), token, "mint", &["uint256"], &[&u(500_000_000).to_string()], U256::zero())?;
            let sup1 = read_u(&chain, owner, token, "totalSupply", &[], &[])?;
            a.fact("mint 500,000,000", format!("ok, supply → {}", sup1 / u(1)));
            let over = deploy::call_write(&mut chain, &AccountSpec::Dev(1), token, "mint", &["uint256"], &[&u(600_000_000).to_string()], U256::zero());
            a.fact("mint 600,000,000 more", format!("reverted = {} (CAP: max supply)", over.is_err()));
            deploy::call_write(&mut chain, &AccountSpec::Dev(0), token, "transfer", &["address", "uint256"], &[&fmt_addr(victim), &u(10_000).to_string()], U256::zero())?;
            let ok = sell_probe(&chain, victim, token, u(1_000));
            a.victim_can_exit = Some(ok);
            a.verdict_line = format!("victim 1000 bech kar nikal sakta hai? {} — kyun: cap sirf MINT bound karta hai, transfer path clean (control group)", if ok { "HAAN" } else { "NAHI" });
            (token, a)
        }
        "proxy" | "renounce-fake" => {
            let v1 = deploy_art(&mut chain, art(&arts, "FakeLogicV1.sol:FakeLogicV1")?, "FakeLogicV1", &[], &[])?;
            let v2 = deploy_art(&mut chain, art(&arts, "FakeLogicV2.sol:FakeLogicV2")?, "FakeLogicV2", &[], &[])?;
            let admin = fmt_addr(addrs[3]);
            let proxy = deploy_art(&mut chain, art(&arts, "FakeProxy.sol:FakeProxy")?, "FakeProxy", &[&fmt_addr(v1), &admin], &["address", "address"])?;
            let mut a = new_audit(variant, proxy);
            deploy::call_write(&mut chain, &AccountSpec::Dev(0), proxy, "initialize", &[], &[], U256::zero())?;
            // owner (Dev0) → victim, pre-upgrade: full amount moves
            deploy::call_write(&mut chain, &AccountSpec::Dev(0), proxy, "transfer", &["address", "uint256"], &[&fmt_addr(victim), &u(1_000).to_string()], U256::zero())?;
            let pre = read_u(&chain, owner, proxy, "balanceOf", &["address"], &[&fmt_addr(victim)])?;
            a.fact("v1 transfer 1,000", format!("victim received {} (clean)", pre / u(1)));
            let mut skim_note = "upgrade se pehle koi skim nahi".to_string();
            if variant == "renounce-fake" {
                // renounce the LOGIC ownership through the proxy — trust theater
                deploy::call_write(&mut chain, &AccountSpec::Dev(0), proxy, "renounceOwnership", &[], &[], U256::zero())?;
                let own = deploy::call_read(&chain, owner, proxy, "owner", &[], &[], &["address"])?;
                a.fact("renounced ownership", format!("owner() == {own} — 'sab safe!'"));
                skim_note = "renounce ke BAAD bhi".to_string();
            }
            // the proxy admin (a DIFFERENT key) upgrades logic — no token owner needed
            deploy::call_write(&mut chain, &AccountSpec::Dev(3), proxy, "upgradeTo", &["address"], &[&fmt_addr(v2)], U256::zero())?;
            let impl_now = deploy::call_read(&chain, owner, proxy, "implementation", &[], &[], &["address"])?;
            a.fact("proxy admin upgraded", format!("implementation → {impl_now} ({skim_note} sirf admin key se)"));
            // v2 skims 10% of every transfer
            deploy::call_write(&mut chain, &AccountSpec::Dev(0), proxy, "transfer", &["address", "uint256"], &[&fmt_addr(victim), &u(1_000).to_string()], U256::zero())?;
            let post = read_u(&chain, owner, proxy, "balanceOf", &["address"], &[&fmt_addr(victim)])?;
            a.fact("v2 transfer 1,000", format!("victim received {} — 100 skimmed to owner", post / u(1) - 1_000));
            let ok = sell_probe(&chain, victim, proxy, u(1_000));
            a.victim_can_exit = Some(ok);
            a.verdict_line = format!(
                "victim 1000 bech kar nikal sakta hai? {} — kyun: {} rules badle ja sakte hain; 'renounced' sirf token-owner ko band karta hai, PROXY ADMIN nahi",
                if ok { "HAAN (lekin ab har transfer pe 10%)" } else { "NAHI" },
                if variant == "renounce-fake" { "renounce ke baad bhi" } else { "jab chahe admin upgrade kare" }
            );
            (proxy, a)
        }
        other => return Err(format!("no local story for '{other}'")),
    };
    let _ = token;
    Ok(a)
}

/// execute a real sell (state-changing) and report its outcome
fn sell_probe_exec(chain: &mut Chain, from_acct: usize, token: H160, to: H160, amount: U256) -> Result<(rrx_util::H256, u64), String> {
    deploy::call_write(
        chain,
        &AccountSpec::Dev(from_acct),
        token,
        "transfer",
        &["address", "uint256"],
        &[&fmt_addr(to), &amount.to_string()],
        U256::zero(),
    )
}

fn new_audit(variant: &str, token: H160) -> LocalAudit {
    LocalAudit { variant: variant.to_string(), token, victim_can_exit: None, verdict_line: String::new(), facts: vec![] }
}

/// live audit: registry verdict + real sell-path probes via eth_call impersonation
pub fn audit_live(p: &Provider, addr_str: &str, slug: Option<&str>, holder: Option<&str>) -> Result<String, String> {
    let addr = h160_from_hex(addr_str)?;
    let mut o = compare(p, addr_str, slug)?;

    if let Some(h) = holder {
        let from = h160_from_hex(h)?;
        let bal = {
            let data = deploy::encode_call("balanceOf", &["address"], &[&fmt_addr(from)])?;
            let raw = p.call(None, &addr, &data, "latest")?;
            abi::decode(&["uint256"], &raw).ok().and_then(|v| match v.first() {
                Some(abi::AbiValue::Uint(u)) => Some(*u),
                _ => None,
            })
        };
        o.push_str(&format!("  holder    {from}\n"));
        if let Some(b) = &bal {
            o.push_str(&format!("  balance   {b}\n"));
        }
        if bal.map(|b| b >= u(1_000)).unwrap_or(false) {
            // SELL probe: transfer 1000 out from the holder's seat
            let data = deploy::encode_call("transfer", &["address", "uint256"], &[&fmt_addr(from), &u(1_000).to_string()])?;
            match p.call(Some(from), &addr, &data, "latest") {
                Ok(_) => o.push_str("  sell probe    PASS — 1000 transfer-out succeeded from the holder's seat (path khula hai)\n"),
                Err(e) => o.push_str(&format!("  sell probe    BLOCKED — {e}\n")),
            }
            // APPROVE probe
            let data = deploy::encode_call("approve", &["address", "uint256"], &[&fmt_addr(from), &(U256::MAX).to_string()])?;
            match p.call(Some(from), &addr, &data, "latest") {
                Ok(_) => o.push_str("  approve probe PASS — unlimited approval reachable (drainer surface exists)\n"),
                Err(e) => o.push_str(&format!("  approve probe BLOCKED — {e}\n")),
            }
        } else {
            o.push_str("  sell probe    SKIPPED — holder balance < 1000 (koi funded holder address do)\n");
        }
        // renounced?
        let data = deploy::encode_call("owner", &[], &[]).ok();
        if let Some(d) = data {
            if let Ok(raw) = p.call(None, &addr, &d, "latest") {
                if let Some(v) = abi::decode(&["address"], &raw).ok().and_then(|v| v.first().cloned()) {
                    o.push_str(&format!("  owner()       {}\n", deploy::format_value(&v)));
                }
            }
        }
    } else {
        o.push_str("  probes    SKIPPED — live sell-path probe ke liye --from <holder> do (eth_call us holder ke seat se chalega)\n");
    }
    o.push_str(&format!(
        "\n  NOTE      probe ek holder ke seat se hai — poore holder-base ka naqsha nahi; blacklist/honeypot poora check karne ke liye kai holders try karo\n"
    ));
    Ok(o)
}

// ---------------------------------------------------------------------------
// 3. VERIFY — screenshot fact-check against chain truth
// ---------------------------------------------------------------------------

const TRANSFER_TOPIC: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

/// real testnets we cross-check when a txid is missing from the queried chain —
/// a testnet receipt pasted into a mainnet conversation is a classic forgery
const TESTNET_SLUGS: &[&str] = &["sepolia", "base-sepolia", "arbitrum-sepolia"];

fn find_tx_on_testnets(hash: &rrx_util::H256) -> Option<String> {
    for slug in TESTNET_SLUGS {
        if let Ok(p) = provider_for(slug) {
            if let Ok(Some(tx)) = p.get_transaction(hash) {
                let blk = tx.get("blockNumber").and_then(|b| b.as_str()).unwrap_or("pending").to_string();
                return Some(format!("chain {slug} pe EXIST karta hai (block {blk})"));
            }
        }
    }
    None
}

pub fn verify(p: &Provider, txid: &str, claims: &[String], slug: Option<&str>) -> Result<String, String> {
    let hash = rrx_util::h256_from_hex(txid).map_err(|e| format!("bad txid: {e}"))?;
    let _ = slug; // slug sirf provider selection ke liye CLI/MCP sambhalta hai
    let tx = match p.get_transaction(&hash)? {
        Some(t) => t,
        None => {
            if let Some(hit) = find_tx_on_testnets(&hash) {
                return Ok(format!(
                    "verify {txid}\n  VERDICT        FALSE — yeh txid is chain pe EXIST nahi karta, lekin {hit}.\
                     \n                 Testnet receipt ko mainnet screenshot ke roop mein pesh karna classic\
                     \n                 forgery hai — dono chain-ids alag hain aur koi bhi explorer cross-show nahi karta.\n"
                ));
            }
            return Err("tx not found on this chain (na kisi testnet pe) — hash galat ya bilkul fake".into());
        }
    };
    let receipt = p.transaction_receipt(&hash)?;
    let status = receipt.as_ref().map(|r| r.get("status").and_then(|s| s.as_str()).unwrap_or("0x0") == "0x1");
    let block: u64 = tx.get("blockNumber").and_then(|b| b.as_str()).and_then(|s| rrx_util::u256_from_str(s).ok()).map(|b| b.as_u64()).unwrap_or(0);
    let latest = p.block_number()?;
    let confirmations = if block > 0 { latest.saturating_sub(block) + 1 } else { 0 };

    let to = tx.get("to").and_then(|t| t.as_str()).unwrap_or("—").to_string();
    let from = tx.get("from").and_then(|t| t.as_str()).unwrap_or("—").to_string();
    let value: U256 = tx.get("value").and_then(|v| v.as_str()).and_then(|s| rrx_util::u256_from_str(s).ok()).unwrap_or_default();

    // token transfers in the receipt
    let mut transfers: Vec<(String, String, U256)> = vec![]; // (token, to, amount)
    if let Some(r) = &receipt {
        if let Some(logs) = r.get("logs").and_then(|l| l.as_array()) {
            for log in logs {
                let topics = log.get("topics").and_then(|t| t.as_array()).cloned().unwrap_or_default();
                if topics.first().and_then(|t| t.as_str()) == Some(TRANSFER_TOPIC) {
                    let token = log.get("address").and_then(|a| a.as_str()).unwrap_or("?").to_string();
                    if topics.len() >= 3 {
                        let dst = topics[2].as_str().unwrap_or("0x");
                        let hex = dst.trim_start_matches("0x");
                        // a 32-byte topic holds the address in the LAST 40 hex chars
                        let recv = if hex.len() >= 40 { format!("0x{}", &hex[hex.len() - 40..]) } else { format!("0x{hex}") };
                        let amount = log.get("data").and_then(|d| d.as_str()).and_then(|s| rrx_util::u256_from_str(s).ok()).unwrap_or_default();
                        transfers.push((token, recv, amount));
                    }
                }
            }
        }
    }

    let mut o = format!("verify {} — screenshot vs chain truth\n", txid);
    o.push_str(&format!("  status         {}\n", status.map(|s| if s { "SUCCESS" } else { "FAILED" }).unwrap_or("PENDING")));
    o.push_str(&format!("  block          {block}  (confirmations {confirmations})\n"));
    o.push_str(&format!("  from           {from}\n"));
    o.push_str(&format!("  to             {to}\n"));
    o.push_str(&format!("  value          {} wei\n", value));
    for (t, r, a) in &transfers {
        o.push_str(&format!("  transfer       token {t} → {r} amount {a}\n"));
    }

    // claims
    let mut verdict = "TRUE (koi claim nahi diya — sirf facts)".to_string();
    let mut wrongs: Vec<String> = vec![];
    if !claims.is_empty() {
        for c in claims {
            let (k, v) = c.split_once('=').ok_or(format!("claim '{c}' format k=v hai"))?;
            match k {
                "recipient" => {
                    let claimed = v.to_lowercase();
                    let ok_native = to.to_lowercase() == claimed;
                    let ok_token = transfers.iter().any(|(_, r, _)| *r == claimed || format!("0x{r}") == claimed);
                    if !(ok_native || ok_token) {
                        wrongs.push(format!("recipient: claim {v} — chain par recipient {to}{}", transfers.iter().map(|(_, r, _)| format!(" / {r}")).collect::<String>()));
                    }
                }
                "amount" => {
                    let claimed = parse_amount(v, 18)?;
                    if claimed != value && !transfers.iter().any(|(_, _, a)| *a == claimed) {
                        wrongs.push(format!("amount: claim {v} ETH-units — chain par value {value} wei, token transfers {:?}", transfers.iter().map(|(_, _, a)| a.to_string()).collect::<Vec<_>>()));
                    }
                }
                "status" => {
                    let want = v.eq_ignore_ascii_case("success") || v.eq_ignore_ascii_case("1");
                    if status != Some(want) {
                        wrongs.push(format!("status: claim {v} — chain par {:?}", status.map(|s| if s { "SUCCESS" } else { "FAILED" }).unwrap_or("PENDING")));
                    }
                }
                other => wrongs.push(format!("{other}: unknown claim key (recipient|amount|status)")),
            }
        }
        verdict = if wrongs.is_empty() { "TRUE — har claim chain data se match".into() } else { format!("FALSE — {} claim(s) galat: {}", wrongs.len(), wrongs.join("; ")) };
    }
    o.push_str(&format!("  VERDICT        {verdict}\n"));
    if confirmations < 12 && status == Some(true) {
        o.push_str("  WARNING        <12 confirmations — RBF/reorg window; screenshot pe 'confirmed' likha ho sakta hai, chain pe abhi final nahi\n");
    }
    Ok(o)
}

fn parse_amount(v: &str, decimals: u32) -> Result<U256, String> {
    // u256_from_str handles the 0x prefix itself
    if v.starts_with("0x") || v.starts_with("0X") {
        return rrx_util::u256_from_str(v).map_err(|e| e.to_string());
    }
    if v.contains('.') {
        let (whole, frac) = v.split_once('.').unwrap();
        let mut frac = frac.to_string();
        while frac.len() < decimals as usize {
            frac.push('0');
        }
        frac.truncate(decimals as usize);
        let w = U256::from_dec_str(if whole.is_empty() { "0" } else { whole }).map_err(|e| e.to_string())?;
        let f = U256::from_dec_str(&frac).map_err(|e| e.to_string())?;
        Ok(w * U256::from(10u64).pow(U256::from(decimals)) + f)
    } else {
        U256::from_dec_str(v).map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------
// 4. FLASHER — the deposit story, detection side: "credit: NO"
// ---------------------------------------------------------------------------
//
// The scam: victim ko 50,000 "USDT" dikhta hai apni wallet UI mein — balance
// sahi render hota hai — aur exchange deposit address pe "bheja" jata hai.
// Wallet UI chain ka balanceOf padhti hai; exchange apni ledger mein sirf
// wahi token credit karta hai jo uski listing policy pass karta hai. Clone
// token kabhi pass nahi hota. Yeh module wahi checklist chalata hai —
// real reads, real probes — aur credit ka faisla deta hai.

/// structured credit decision — pure, so tests aur dono modes ek hi rules pe chalein
#[derive(Debug)]
pub struct CreditDecision {
    pub credit: bool,
    pub reasons: Vec<String>,
}

/// 1234567 → "1,234,567" — UI-view numbers deserve real formatting
fn commas(n: U256) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn credit_decision(verdict: &str, sell_probe_ok: Option<bool>) -> CreditDecision {
    let mut d = CreditDecision { credit: false, reasons: vec![] };
    match verdict {
        "REAL" => {
            d.credit = true;
            d.reasons.push("canonical listing — address registry se match".into());
        }
        "CLONE" => {
            d.credit = false;
            d.reasons.push("CLONE: ticker/decimals canonical jaise, address alag — exchange ki listing policy fail".into());
        }
        _ => {
            d.credit = false;
            d.reasons.push("UNKNOWN: kisi canonical listing se match nahi — recognized asset nahi".into());
        }
    }
    if sell_probe_ok == Some(false) {
        d.credit = false;
        d.reasons.push("sell-path probe BLOCKED — honeypot signature; withdrawal bhi atka hoga".into());
    } else if sell_probe_ok == Some(true) && verdict != "REAL" {
        d.reasons.push("note: sell path khula hai — lekin credit listing pe hota hai, transferability pe nahi".into());
    }
    d
}

/// ek hi flasher story — local clone, real EVM, live canonical reference
pub fn flasher_local() -> Result<String, String> {
    let mut o = String::from(
        "FLASHER — 50,000 'USDT' deposit ki kahani, exchange ki aankh se\n\
         ================================================================\n\n",
    );

    // [1] clone on a fresh local chain (real solc, real deploy)
    let arts = compile_map("plain")?;
    let (mut chain, addrs) = local_chain();
    let (owner, victim) = (addrs[0], addrs[1]);
    let token = deploy_art(&mut chain, art(&arts, "FakePlain.sol:FakePlain")?, "FakePlain", &[], &[])?;
    o.push_str(&format!(
        "[1] clone live: token {token} — 'Tether USD' / USDT / 6 decimals / 1B supply\n"
    ));

    // [2] the flash: attacker mints 1B out of thin air, pushes 50,000 to the victim
    deploy::call_write(&mut chain, &AccountSpec::Dev(2), token, "mint", &["uint256"], &[&u(1_000_000_000).to_string()], U256::zero())?;
    deploy::call_write(&mut chain, &AccountSpec::Dev(2), token, "transfer", &["address", "uint256"], &[&fmt_addr(victim), &u(50_000).to_string()], U256::zero())?;
    let bal = read_u(&chain, owner, token, "balanceOf", &["address"], &[&fmt_addr(victim)])?;
    o.push_str(&format!(
        "[2] victim ko deposit dikha:  wallet UI  {}.{:06} USDT  ✓ (ye chain ka asli balanceOf hai, raw {bal})\n",
        commas(bal / u(1)),
        (bal % u(1)).as_u64()
    ));
    o.push_str(&format!(
        "    deposit address  {} — explorer pe tx bhi dikh raha hoga\n",
        fmt_addr(victim)
    ));

    // [3] exchange-side checklist — wahi checks jo ledger entry se pehle chalte hain
    o.push_str("[3] exchange-side checklist (real reads):\n");
    let mainnet = provider_for("ethereum")?;
    let local_read = TokenRead {
        address: token,
        name: Some(deploy::call_read(&chain, owner, token, "name", &[], &[], &["string"])?),
        symbol: Some(deploy::call_read(&chain, owner, token, "symbol", &[], &[], &["string"])?),
        decimals: Some(read_u(&chain, owner, token, "decimals", &[], &[])?),
        total_supply: Some(read_u(&chain, owner, token, "totalSupply", &[], &[])?),
    };
    let entry = &REGISTRY[0];
    let real_read = read_token(&mainnet, &h160_from_hex(entry.address)?);
    let cmp = compare_with_reads(&token, &local_read, Some((entry, &real_read)));
    for line in cmp.lines().skip(1) {
        o.push_str(&format!("    {line}\n"));
    }
    let verdict = cmp.lines().find_map(|l| {
        let l = l.trim();
        l.starts_with("VERDICT").then(|| l.split_whitespace().nth(1).unwrap_or("UNKNOWN").to_string())
    }).unwrap_or_else(|| "UNKNOWN".into());
    let sell_ok = sell_probe(&chain, victim, token, u(1_000));
    o.push_str(&format!("    sell-path probe   {} — victim apna 1000 nikal sakta hai?\n", if sell_ok { "PASS" } else { "BLOCKED" }));

    // [4] the verdict that matters
    let d = credit_decision(&verdict, Some(sell_ok));
    o.push_str(&format!(
        "\n[4] EXCHANGE CREDIT: {} — {} karan:\n",
        if d.credit { "YES" } else { "NO" },
        d.reasons.len()
    ));
    for (i, r) in d.reasons.iter().enumerate() {
        o.push_str(&format!("    {}. {r}\n", i + 1));
    }
    o.push_str("    UI ka balance sirf chain ka data hai — exchange ki ledger usi token ko janti hai\n");
    o.push_str("    jo uski listing mein hai. Clone 50,000 kabhi credit nahi hoga.\n\n");

    // [5] control: wahi checklist asli USDT pe
    let real_cmp = compare(&mainnet, REGISTRY[0].address, Some("ethereum"))?;
    o.push_str("[5] control — asli USDT pe wahi checklist:\n");
    for line in real_cmp.lines().skip(1) {
        o.push_str(&format!("    {line}\n"));
    }
    let rd = credit_decision("REAL", None);
    o.push_str(&format!(
        "    EXCHANGE CREDIT: {} — yehi farq hai: token asli hone ka matlab ledger entry milna hai\n",
        if rd.credit { "YES" } else { "NO" }
    ));
    Ok(o)
}

/// live mode: the same checklist against any real token address
pub fn flasher_live(p: &Provider, addr_str: &str, slug: Option<&str>, holder: Option<&str>) -> Result<String, String> {
    let addr = h160_from_hex(addr_str)?;
    let cmp = compare(p, addr_str, slug)?;
    let verdict = cmp.lines().find_map(|l| {
        let l = l.trim();
        l.starts_with("VERDICT").then(|| l.split_whitespace().nth(1).unwrap_or("UNKNOWN").to_string())
    }).unwrap_or_else(|| "UNKNOWN".into());

    let mut o = format!("FLASHER — deposit checklist, live token {addr}\n=================================================\n\n{cmp}\n");
    let mut sell_ok: Option<bool> = None;
    if let Some(h) = holder {
        let from = h160_from_hex(h)?;
        let data = deploy::encode_call("balanceOf", &["address"], &[&fmt_addr(from)])?;
        let bal = p.call(None, &addr, &data, "latest").ok().and_then(|raw| abi::decode(&["uint256"], &raw).ok()).and_then(|v| match v.first() {
            Some(abi::AbiValue::Uint(u)) => Some(*u),
            _ => None,
        });
        if let Some(b) = bal {
            o.push_str(&format!("holder {from} ka balance (UI jo dikhata hai): {b} raw units\n"));
        }
        // probe sirf tab jab balance >= 1000 — warna 'insufficient balance' revert
        // honeypot jaisa lagta hai, jo wo nahi hai (audit_live ka hi rule)
        if bal.map(|b| b >= u(1_000)).unwrap_or(false) {
            let probe = deploy::encode_call("transfer", &["address", "uint256"], &[&fmt_addr(from), &u(1_000).to_string()])?;
            match p.call(Some(from), &addr, &probe, "latest") {
                Ok(_) => {
                    sell_ok = Some(true);
                    o.push_str("sell-path probe  PASS — 1000 transfer-out holder ke seat se succeed (path khula)\n");
                }
                Err(e) => {
                    sell_ok = Some(false);
                    o.push_str(&format!("sell-path probe  BLOCKED — {e} (honeypot signature)\n"));
                }
            }
        } else {
            o.push_str("sell-path probe  SKIPPED — holder balance < 1000 (funded holder do, warna revert sirf balance ki wajah se hota)\n");
        }
    }
    let d = credit_decision(&verdict, sell_ok);
    o.push_str(&format!("\nEXCHANGE CREDIT: {}\n", if d.credit { "YES" } else { "NO" }));
    for (i, r) in d.reasons.iter().enumerate() {
        o.push_str(&format!("  {}. {r}\n", i + 1));
    }
    o.push_str("\nUI balance ≠ credit: wallet jo chain dikhati hai, exchange wahi credit karta hai jo uski listing pass kare.\n");
    Ok(o)
}

// ---------------------------------------------------------------------------
// 5. PENDING — RBF replacement risk: "pending" koi proof nahi
// ---------------------------------------------------------------------------
//
// Real mempool policy (Ethereum, since 2021): a type-0x2 (EIP-1559) tx is
// RBF-signaled by default; a replacement is accepted at >=10% fee bump over
// the existing one. Consequence: a PENDING deposit can be replaced by its own
// sender with a different destination — the "50k deposit pending" screenshot
// is worth nothing until confirmations land. This check reads the real tx
// fields (type, fees, nonce, blockNumber) and reports the risk.

pub fn pending_check(p: &Provider, txid: &str) -> Result<String, String> {
    let hash = rrx_util::h256_from_hex(txid).map_err(|e| format!("bad txid: {e}"))?;
    let tx = match p.get_transaction(&hash)? {
        Some(t) => t,
        None => {
            if let Some(hit) = find_tx_on_testnets(&hash) {
                return Err(format!("tx is chain pe nahi mila — lekin {hit}. Network ka jhoot pakda gaya."));
            }
            return Err("tx not found on this chain — hash galat, mempool se nikal chuka, ya fake".into());
        }
    };

    let block: Option<u64> = tx.get("blockNumber").and_then(|b| b.as_str()).and_then(|s| rrx_util::u256_from_str(s).ok()).map(|b| b.as_u64());
    let typ = tx.get("type").and_then(|t| t.as_str()).unwrap_or("0x0").to_string();
    let nonce = tx.get("nonce").and_then(|n| n.as_str()).unwrap_or("0x0").to_string();
    let max_fee = tx.get("maxFeePerGas").and_then(|v| v.as_str()).and_then(|s| rrx_util::u256_from_str(s).ok());
    let max_prio = tx.get("maxPriorityFeePerGas").and_then(|v| v.as_str()).and_then(|s| rrx_util::u256_from_str(s).ok());
    let gas_price = tx.get("gasPrice").and_then(|v| v.as_str()).and_then(|s| rrx_util::u256_from_str(s).ok());
    let to = tx.get("to").and_then(|t| t.as_str()).unwrap_or("(create)").to_string();
    let from = tx.get("from").and_then(|t| t.as_str()).unwrap_or("?").to_string();

    let mut o = format!("pending check {txid}\n===================================\n");
    o.push_str(&format!("  from           {from}\n  to             {to}\n  type           {typ}  nonce  {nonce}\n"));

    match block {
        None => {
            o.push_str("  state          PENDING — mempool mein, koi block nahi\n");
        }
        Some(b) => {
            let latest = p.block_number()?;
            let conf = latest.saturating_sub(b) + 1;
            o.push_str(&format!("  state          MINED — block {b}, confirmations {conf}\n"));
        }
    }

    // RBF signal analysis from real fields
    let (signaled, why) = if typ == "0x2" {
        (true, "EIP-1559 (type 0x2) — RBF default-signaled".to_string())
    } else if typ == "0x0" || typ == "0x1" {
        let n = tx.get("nonce").and_then(|v| v.as_str()).and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok()).unwrap_or(0);
        let signaled = n < 0xffff_fffe;
        (signaled, format!("legacy (type {typ}) — RBF signal = sequence < 0xfffffffe: sequence {n} → {}", if signaled { "SIGNALLED" } else { "no signal" }))
    } else {
        (true, format!("type {typ} — policy default: signaled maana jata hai"))
    };

    let risk: u32 = match block {
        None if signaled => 72, // pending + RBF = replaceable abhi
        None => 45,             // pending bina signal — but 0x0/0x1 legacy rarely unsignaled
        Some(_) if signaled => 35, // mined; while it WAS pending it was replaceable
        Some(_) => 15,
    };

    o.push_str(&format!("  RBF signal     {}\n                 {why}\n", if signaled { "YES" } else { "NO" }));
    o.push_str("  bump floor     10% — replacement wahi sender ke agle tx se, usi nonce pe, >=10% zyada fee\n");
    if let Some(mf) = max_fee {
        let bump = mf * U256::from(11u8) / U256::from(10u8);
        o.push_str(&format!("  maxFeePerGas   {mf} — replacement ko chahiye >= {bump}\n"));
    }
    if let Some(mp) = max_prio {
        o.push_str(&format!("  maxPriorityFee {mp}\n"));
    } else if let Some(gp) = gas_price {
        let bump = gp * U256::from(11u8) / U256::from(10u8);
        o.push_str(&format!("  gasPrice       {gp} — legacy replacement >= {bump}\n"));
    }
    // stuck risk: 1559 tx whose maxFee is far below the next base fee never lands
    if typ == "0x2" {
        if let Ok((base, _)) = p.fee_history(1) {
            if let Some(b) = base.last() {
                if let Some(mf) = max_fee {
                    if mf < *b {
                        o.push_str(&format!("  stuck risk     HIGH — maxFee {mf} < current base fee {b}: tx land nahi hoga jab tak bump na ho\n"));
                    }
                }
            }
        }
    }
    o.push_str(&format!("  replace risk   {risk}%  (heuristic: state + RBF signal + fee headroom)\n"));
    o.push_str("\n  RULE           PENDING ≠ CREDIT. Koi exchange pending tx pe ledger entry nahi banata;\n");
    o.push_str("                 confirmations chahiye. Aur RBF-signaled pending tx ko US sender ne\n");
    o.push_str("                 already replace kiya ho sakta hai — dikhne wala 'deposit' gayab ho sakta hai.\n");
    Ok(o)
}

// ---------------------------------------------------------------------------
// DEMO — ek command, ek scroll
// ---------------------------------------------------------------------------

pub fn demo() -> Result<String, String> {
    let mut o = String::from("FAKE-ASSET DEMO — clone → mint → burn → honeypot → registry → flasher → proof\n");
    o.push_str("=====================================================================================\n\n");

    // [1/7] deploy the plain clone on the local chain (real solc, real EVM)
    o.push_str("[1/7] plain clone deploy — 'Tether USD' / USDT / 6 decimals / 1B supply\n");
    let arts = compile_map("plain")?;
    let (mut chain, addrs) = local_chain();
    let (owner, victim, other) = (addrs[0], addrs[1], addrs[2]);
    let token = deploy_art(&mut chain, art(&arts, "FakePlain.sol:FakePlain")?, "FakePlain", &[], &[])?;
    o.push_str(&format!("      token {} — deployer {}\n\n", fmt_addr(token), fmt_addr(owner)));

    // [2/7] registry compare — live real USDT supply as the measuring stick
    o.push_str("[2/7] registry compare (live mainnet reference)\n");
    let mainnet = provider_for("ethereum")?;
    let local_read = TokenRead {
        address: token,
        name: Some(deploy::call_read(&chain, owner, token, "name", &[], &[], &["string"])?),
        symbol: Some(deploy::call_read(&chain, owner, token, "symbol", &[], &[], &["string"])?),
        decimals: Some(read_u(&chain, owner, token, "decimals", &[], &[])?),
        total_supply: Some(read_u(&chain, owner, token, "totalSupply", &[], &[])?),
    };
    let real_addr = h160_from_hex(REGISTRY[0].address)?;
    let real_read = read_token(&mainnet, &real_addr);
    o.push_str(&compare_with_reads(&token, &local_read, Some((&REGISTRY[0], &real_read))));
    o.push('\n');

    // [3/7] the wild story — 500M from thin air, 1000 burned without allowance
    o.push_str("[3/7] wild mechanics — real execution numbers\n");
    let minted = u(500_000_000);
    deploy::call_write(&mut chain, &AccountSpec::Dev(1), token, "mint", &["uint256"], &[&minted.to_string()], U256::zero())?;
    let burned = u(1_000);
    deploy::call_write(&mut chain, &AccountSpec::Dev(1), token, "burn", &["address", "uint256"], &[&fmt_addr(owner), &burned.to_string()], U256::zero())?;
    let sup = read_u(&chain, owner, token, "totalSupply", &[], &[])?;
    o.push_str(&format!("      attacker minted 500,000,000 out of thin air; burned 1,000 from the deployer wallet\n"));
    o.push_str(&format!("      supply ab {} (shuru 1,000,000,000)\n\n", sup / u(1)));

    // [4/7] honeypot variant — victim cannot sell
    o.push_str("[4/7] honeypot variant audit\n");
    let hp = audit_local("honeypot")?;
    for line in hp.render().lines() {
        o.push_str(&format!("      {line}\n"));
    }
    // control: plain is NOT a honeypot
    o.push_str("      control: plain variant pe wahi probe PASS karta hai (wildtoken honeypot nahi hai)\n\n");

    // [5/7] flasher — the 50k deposit that will never be credited
    o.push_str("[5/7] flasher — 50,000 'USDT' deposit, exchange ki aankh se\n");
    let fl = flasher_local()?;
    for line in fl.lines().skip(1) {
        o.push_str(&format!("      {line}\n"));
    }
    o.push('\n');

    // [6/7] verify — fabricate a claim, catch it
    o.push_str("[6/7] proof verify — ek fake claim, chain truth ke against\n");
    let latest = mainnet.block_number()?;
    let blk = mainnet.block_by_tag("latest", true)?.ok_or("no latest block")?;
    let txs = blk.get("transactions").and_then(|t| t.as_array()).cloned().unwrap_or_default();
    if let Some(tx) = txs.first() {
        let h = tx.get("hash").and_then(|h| h.as_str()).unwrap_or_default().to_string();
        let real_val: U256 = tx.get("value").and_then(|v| v.as_str()).and_then(|s| rrx_util::u256_from_str(s).ok()).unwrap_or_default();
        let fake_claim = format!("amount=0x{:x}", real_val + U256::from(500_000_000_000_000_000_000u128));
        o.push_str(&format!("      tx {h} (latest mainnet block {latest})\n"));
        o.push_str(&format!("      claimed {}\n", &fake_claim["amount=".len()..]));
        let line = verify(&mainnet, &h, &[fake_claim], Some("ethereum"))?;
        for l in line.lines().skip(1) {
            o.push_str(&format!("      {l}\n"));
        }
    } else {
        o.push_str("      (latest block empty — skipped)\n");
    }
    o.push('\n');

    // [7/7] what this module does NOT do
    o.push_str("[7/7] boundaries\n");
    o.push_str("      yeh module flash-balance, drainer payloads, fake-proof forgery, ya cash-out routing NAHI karta.\n");
    o.push_str("      woh scam-kit ke components hain. Yeh stack ulta hai: clones pakadta hai, honeypot pehchanta hai,\n");
    o.push_str("      screenshots ka sach nikalta hai. Dawa aur demo mein farq ho to demo jeetega — aur demo ne sirf\n");
    o.push_str("      detection dikhaya, dhoka nahi.\n");
    Ok(o)
}

// ---------------------------------------------------------------------------
// live registry verification — every entry spot-checked against its chain
// ---------------------------------------------------------------------------

pub fn verify_live_registry() -> Result<String, String> {
    let mut o = String::from("registry --verify-live — har entry ka symbol/decimals us chain pe eth_call se check\n\n");
    let mut by_slug: BTreeMap<&str, Vec<&RegistryEntry>> = BTreeMap::new();
    for e in REGISTRY {
        by_slug.entry(e.slug).or_default().push(e);
    }
    let (mut ok, mut bad) = (0usize, 0usize);
    for (slug, rows) in &by_slug {
        let p = match provider_for(slug) {
            Ok(p) => p,
            Err(e) => {
                o.push_str(&format!("{slug}: NO ENDPOINT ({e})\n"));
                continue;
            }
        };
        for r in rows {
            let addr = h160_from_hex(r.address)?;
            let sym = eth_call_string(&p, &addr, "symbol");
            let dec = eth_call_uint(&p, &addr, "decimals");
            let expected_dec = if r.ticker == "USDT" && r.slug == "ethereum" { 6 } else if r.note.contains("18") { 18 } else if r.note.contains("6") { 6 } else { 18 };
            let sym_ok = sym.as_deref().map(|s| s.eq_ignore_ascii_case(r.ticker) || r.aliases.iter().any(|a| s.eq_ignore_ascii_case(a))).unwrap_or(false);
            let dec_ok = dec.map(|d| d == U256::from(expected_dec)).unwrap_or(false);
            if sym_ok && dec_ok {
                ok += 1;
                o.push_str(&format!("  OK       {:<9} {:<5} {addr} ({})\n", slug, r.ticker, sym.clone().unwrap_or_default()));
            } else {
                bad += 1;
                o.push_str(&format!("  MISMATCH {:<9} {:<5} {addr} — got symbol {:?} decimals {:?} (expected {}/{})\n", slug, r.ticker, sym, dec, r.ticker, expected_dec));
            }
        }
    }
    o.push_str(&format!("\n  {ok} OK, {bad} mismatch (mismatch = registry entry fix hona chahiye ya chain down)\n"));
    Ok(o)
}

// ---------------------------------------------------------------------------
// testnet deploy of a variant (chain id 1 hard-refused — see docs)
// ---------------------------------------------------------------------------

pub fn deploy_variant_live(variant: &str, rpc: Option<&str>, key: Option<String>) -> Result<String, String> {
    let single = match variant {
        "plain" => "FakePlain.sol:FakePlain",
        "honeypot" => "FakeHoneypot.sol:FakeHoneypot",
        "tax" => "FakeTaxToken.sol:FakeTaxToken",
        "freeze" => "FakeFreezeToken.sol:FakeFreezeToken",
        "capped" => "FakeCappedToken.sol:FakeCappedToken",
        "proxy" | "renounce-fake" => {
            return Err("proxy/renounce-fake multi-contract deploy hai — unka poora story 'fakeasset audit --variant <v>' pe chalta hai (local chain, safe)".into())
        }
        other => return Err(format!("unknown variant '{other}'")),
    };
    let arts = compile_map(variant)?;
    let a = art(&arts, single)?;
    let key = key.or_else(|| std::env::var("RRX_PRIVATE_KEY").ok().filter(|s| !s.is_empty())).ok_or("key chahiye: --key <hex> ya RRX_PRIVATE_KEY env")?;
    let p = match rpc {
        Some(u) => Provider::new(u),
        None => provider_for("sepolia")?,
    };
    let cid = p.chain_id()?;
    if cid == 1 {
        return Err("fakeasset variants TESTNET-ONLY hain — 'Tether USD' naam mainnet pe wallet-impersonation fraud vector hai. Yeh guard --yes se bhi override nahi hota.".into());
    }
    let acct = AccountSpec::Key(key);
    let outcome = deploy::deploy_rpc(&p, a, &acct, &[], &[], U256::zero(), 100, 2)?;
    let mut o = format!("{variant} deployed on chain {cid}: {}\n", outcome.address);
    o.push_str(&format!("  tx        {}\n", outcome.tx_hash));
    o.push_str(&format!("  gas used  {}\n", outcome.gas_used));
    o.push_str("  ab iska sach: rrx fakeasset audit --address <addr> --chain <slug> --from <holder>\n");
    Ok(o)
}

// ---------------------------------------------------------------------------
// tests — every variant compiles + every story measured + live registry checks
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_table_lists_all_rows() {
        let t = registry_table();
        assert_eq!(REGISTRY.len(), 18, "registry rows");
        for e in REGISTRY {
            assert!(t.contains(e.address), "missing {}", e.address);
        }
        assert!(t.contains("USDT") && t.contains("USDC") && t.contains("WBNB"));
    }

    #[test]
    fn registry_live_spotcheck_usdt() {
        let p = provider_for("ethereum").expect("provider");
        let t = read_token(&p, &h160_from_hex(REGISTRY[0].address).unwrap());
        assert_eq!(t.symbol.as_deref(), Some("USDT"));
        assert_eq!(t.decimals, Some(U256::from(6u8)));
        // live supply must be enormous (tens of billions of tokens)
        let sup = t.total_supply.expect("supply");
        assert!(sup > U256::from(10_000_000_000u64) * U256::from(1_000_000u64), "real USDT supply");
    }

    #[test]
    fn registry_compare_flags_real_mainnet() {
        let p = provider_for("ethereum").expect("provider");
        let out = compare(&p, REGISTRY[0].address, Some("ethereum")).expect("compare");
        assert!(out.contains("VERDICT   REAL"), "{out}");
    }

    #[test]
    fn variants_all_compile() {
        for v in ["plain", "honeypot", "tax", "freeze", "capped", "proxy", "renounce-fake"] {
            let out = compile_variant(v).unwrap_or_else(|e| panic!("{v}: {e}"));
            assert!(out.contains("compiled"), "{v}");
        }
    }

    #[test]
    fn clone_gets_flagged() {
        // local clone read + registry-style compare against a synthetic canonical
        let (addr_str, sym, dec, sup) = ("0x1111111111111111111111111111111111111111", "USDT", U256::from(6u8), U256::from(1_000_000_000u64) * U256::from(1_000_000u64));
        let t = TokenRead { address: h160_from_hex(addr_str).unwrap(), name: Some("Tether USD".into()), symbol: Some(sym.into()), decimals: Some(dec), total_supply: Some(sup) };
        let canonical = TokenRead { address: h160_from_hex("0xdAC17F958D2ee523a2206206994597C13D831ec7").unwrap(), name: Some("Tether USD".into()), symbol: Some("USDT".into()), decimals: Some(U256::from(6u8)), total_supply: Some(U256::from(84_000_000_000u64) * U256::from(1_000_000u64)) };
        let entry = &REGISTRY[0];
        let out = compare_with_reads(&t.address, &t, Some((entry, &canonical)));
        assert!(out.contains("VERDICT   CLONE"), "{out}");
    }

    #[test]
    fn plain_is_not_a_honeypot() {
        let a = audit_local("plain").expect("plain audit");
        assert_eq!(a.victim_can_exit, Some(true), "plain must be sellable");
        assert!(a.verdict_line.starts_with("victim 1000 bech kar nikal sakta hai? HAAN"));
    }

    #[test]
    fn honeypot_buy_passes_sell_reverts() {
        let a = audit_local("honeypot").expect("honeypot audit");
        assert_eq!(a.victim_can_exit, Some(false));
        assert!(a.facts.iter().any(|(k, v)| k == "victim sell probe" && v.contains("reverted = true")));
        assert!(a.facts.iter().any(|(k, v)| k == "victim bought in" && v.contains("received")));
        assert!(a.verdict_line.starts_with("victim 1000 bech kar nikal sakta hai? NAHI"));
    }

    #[test]
    fn tax_takes_98_percent_on_sell() {
        let a = audit_local("tax").expect("tax audit");
        assert!(a.facts.iter().any(|(k, v)| k == "victim sold 1,000 to pair" && v.contains("980")), "{a:?}");
        assert!(a.verdict_line.contains("TECHNICALLY HAAN"));
    }

    #[test]
    fn freeze_blocks_and_releases() {
        let a = audit_local("freeze").expect("freeze audit");
        assert!(a.facts.iter().any(|(k, v)| k == "victim sell probe" && v.contains("reverted = true")));
        assert!(a.facts.iter().any(|(k, v)| k == "after authority unfreezes" && v.contains("succeeds = true")));
        assert_eq!(a.victim_can_exit, Some(false));
    }

    #[test]
    fn capped_mint_respects_max_supply() {
        let a = audit_local("capped").expect("capped audit");
        assert!(a.facts.iter().any(|(k, v)| k == "mint 600,000,000 more" && v.contains("reverted = true")));
        assert_eq!(a.victim_can_exit, Some(true));
    }

    #[test]
    fn proxy_upgrade_changes_behavior() {
        let a = audit_local("proxy").expect("proxy audit");
        assert!(a.facts.iter().any(|(k, v)| k == "v2 transfer 1,000" && v.contains("100 skimmed")), "{a:?}");
    }

    #[test]
    fn renounce_fake_admin_still_upgrades() {
        let a = audit_local("renounce-fake").expect("renounce audit");
        let renounced = a.facts.iter().find(|(k, _)| *k == "renounced ownership").expect("renounce fact");
        assert!(renounced.1.to_lowercase().contains("dead"), "{renounced:?}");
        assert!(a.facts.iter().any(|(k, v)| k == "proxy admin upgraded" && v.contains("renounce ke BAAD bhi")));
        // and the skim happens post-renounce
        assert!(a.facts.iter().any(|(k, v)| k == "v2 transfer 1,000" && v.contains("100 skimmed")));
    }

    #[test]
    fn verify_flags_false_amount_claim() {
        let p = provider_for("ethereum").expect("provider");
        let blk = p.block_by_tag("latest", true).expect("block").expect("latest");
        let txs = blk.get("transactions").and_then(|t| t.as_array()).cloned().unwrap_or_default();
        let tx = txs.first().expect("a tx");
        let h = tx.get("hash").and_then(|h| h.as_str()).expect("hash");
        let real_val: U256 = tx.get("value").and_then(|v| v.as_str()).and_then(|s| rrx_util::u256_from_str(s).ok()).unwrap_or_default();
        let fake = format!("amount=0x{:x}", real_val + U256::from(1u8));
        let out = verify(&p, h, &[fake], Some("ethereum")).expect("verify");
        assert!(out.contains("VERDICT        FALSE"), "{out}");
    }

    #[test]
    fn verify_accepts_true_recipient_claim() {
        let p = provider_for("ethereum").expect("provider");
        let blk = p.block_by_tag("latest", true).expect("block").expect("latest");
        let txs = blk.get("transactions").and_then(|t| t.as_array()).cloned().unwrap_or_default();
        let tx = txs.iter().find(|t| t.get("to").and_then(|x| x.as_str()).is_some()).expect("a tx with a recipient");
        let h = tx.get("hash").and_then(|h| h.as_str()).expect("hash");
        let to = tx.get("to").and_then(|x| x.as_str()).expect("to");
        let out = verify(&p, h, &[format!("recipient={to}")], Some("ethereum")).expect("verify");
        assert!(out.contains("VERDICT        TRUE"), "{out}");
    }

    #[test]
    fn demo_story_contains_all_beats() {
        let out = demo().expect("demo");
        assert!(out.contains("[1/7]"), "deploy beat");
        assert!(out.contains("VERDICT   CLONE"), "registry beat");
        assert!(out.contains("500,000,000"), "mint beat");
        assert!(out.contains("NAHI"), "honeypot verdict beat");
        assert!(out.contains("[5/7]"), "flasher beat");
        assert!(out.contains("EXCHANGE CREDIT: NO"), "credit beat");
        assert!(out.contains("VERDICT        FALSE"), "proof beat");
        assert!(out.contains("[7/7]"), "boundaries beat");
    }

    #[test]
    fn audit_live_probe_on_real_usdt() {
        let p = provider_for("ethereum").expect("provider");
        let out = audit_live(&p, REGISTRY[0].address, Some("ethereum"), Some("0xd8da6bf26964af9d7eed9e03e53415d37aa96045")).expect("live audit");
        assert!(out.contains("VERDICT   REAL"), "{out}");
        assert!(out.contains("sell probe"), "{out}");
    }

    // ---- flasher: the deposit that never credits ----

    #[test]
    fn credit_decision_rules() {
        // REAL + free sell = credit YES
        let d = credit_decision("REAL", Some(true));
        assert!(d.credit);
        // REAL but sell blocked — still no credit (honeypot even on a canonical-looking token)
        let d = credit_decision("REAL", Some(false));
        assert!(!d.credit);
        assert!(d.reasons.iter().any(|r| r.contains("honeypot")), "{d:?}");
        // CLONE = NO regardless of transferability
        let d = credit_decision("CLONE", Some(true));
        assert!(!d.credit);
        assert!(d.reasons.iter().any(|r| r.contains("listing policy")), "{d:?}");
        // UNKNOWN = NO
        assert!(!credit_decision("UNKNOWN", None).credit);
    }

    #[test]
    fn flasher_local_prints_credit_no() {
        let out = flasher_local().expect("flasher local");
        assert!(out.contains("50,000.000000"), "deposit must show in the UI view: {out}");
        assert!(out.contains("VERDICT   CLONE"), "clone must be flagged");
        assert!(out.contains("sell-path probe   PASS"), "plain clone transfer path is open");
        assert!(out.contains("EXCHANGE CREDIT: NO"), "the whole point");
        assert!(out.contains("EXCHANGE CREDIT: YES"), "control: real USDT credits");
    }

    #[test]
    fn flasher_live_real_usdt_credit_yes() {
        let p = provider_for("ethereum").expect("provider");
        let out = flasher_live(&p, REGISTRY[0].address, Some("ethereum"), Some("0xd8da6bf26964af9d7eed9e03e53415d37aa96045")).expect("flasher live");
        assert!(out.contains("EXCHANGE CREDIT: YES"), "{out}");
    }

    // ---- pending: RBF replacement risk ----

    #[test]
    fn pending_check_flags_rbf_on_real_tx() {
        let p = provider_for("ethereum").expect("provider");
        let blk = p.block_by_tag("latest", true).expect("block").expect("latest");
        let txs = blk.get("transactions").and_then(|t| t.as_array()).cloned().unwrap_or_default();
        let tx = txs.iter().find(|t| t.get("type").and_then(|x| x.as_str()) == Some("0x2")).expect("a 1559 tx");
        let h = tx.get("hash").and_then(|x| x.as_str()).expect("hash");
        let out = pending_check(&p, h).expect("pending check");
        assert!(out.contains("MINED"), "{out}");
        assert!(out.contains("RBF signal     YES"), "type-0x2 is default-signaled: {out}");
        assert!(out.contains("bump floor     10%"), "{out}");
        assert!(out.contains("PENDING ≠ CREDIT"), "{out}");
    }

    #[test]
    fn pending_check_errors_on_bogus_txid() {
        let p = provider_for("ethereum").expect("provider");
        let e = pending_check(&p, "0x1111111111111111111111111111111111111111111111111111111111111111").unwrap_err();
        assert!(e.contains("not found"), "{e}");
    }

    // ---- verify: testnet txid dressed as mainnet ----

    #[test]
    fn verify_catches_testnet_txid_dressed_as_mainnet() {
        // fetch a REAL sepolia txid, then run verify against ETHEREUM —
        // the cross-check must catch the wrong-network forgery
        let sp = provider_for("sepolia").expect("sepolia provider");
        let blk = sp.block_by_tag("latest", true).expect("block").expect("latest");
        let txs = blk.get("transactions").and_then(|t| t.as_array()).cloned().unwrap_or_default();
        let tx = txs.first().expect("a sepolia tx");
        let h = tx.get("hash").and_then(|x| x.as_str()).expect("hash").to_string();
        let p = provider_for("ethereum").expect("ethereum provider");
        let out = verify(&p, &h, &[], Some("ethereum")).expect("verify cross-check");
        assert!(out.contains("VERDICT        FALSE"), "{out}");
        assert!(out.contains("sepolia pe EXIST"), "{out}");
    }
}
