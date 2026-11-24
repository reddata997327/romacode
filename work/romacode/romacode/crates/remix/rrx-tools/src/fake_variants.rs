//! Fake-asset variant fixtures — the SPECIMENS used for study and detection.
//!
//! Each variant is a real, compilable contract that exhibits one classic scam
//! mechanic, so the audit engine can prove — with real execution, real solc,
//! real numbers — how the victim experiences it. They are STUDY FIXTURES:
//! deployments are testnet-only (the CLI hard-refuses chain id 1), and every
//! demo answers the only question that matters: "victim 1000 bech kar nikal
//! sakta hai? haan ya nahi — aur kyun".

/// variant name → (file, content) pairs. All lookalikes keep the "Tether USD"
/// listing deliberately: that is exactly what the clone detector must see.
pub fn variant_sources(name: &str) -> Result<Vec<(String, String)>, String> {
    match name {
        "plain" => Ok(vec![(
            "FakePlain.sol".into(),
            crate::wildtoken::SOURCE
                .replace("contract WildToken", "contract FakePlain"),
        )]),
        "honeypot" => Ok(vec![("FakeHoneypot.sol".into(), HONEYPOT.into())]),
        "tax" => Ok(vec![("FakeTaxToken.sol".into(), TAX.into())]),
        "freeze" => Ok(vec![("FakeFreezeToken.sol".into(), FREEZE.into())]),
        "capped" => Ok(vec![("FakeCappedToken.sol".into(), CAPPED.into())]),
        "proxy" | "renounce-fake" => Ok(vec![
            ("FakeLogicV1.sol".into(), LOGIC_V1.into()),
            ("FakeLogicV2.sol".into(), LOGIC_V2.into()),
            ("FakeProxy.sol".into(), PROXY.into()),
        ]),
        other => Err(format!(
            "unknown variant '{other}' (available: plain, honeypot, tax, freeze, capped, proxy, renounce-fake)"
        )),
    }
}

pub struct VariantInfo {
    pub name: &'static str,
    pub mechanic: &'static str,
    pub who_gets_hurt: &'static str,
    pub exit_possible: &'static str,
}

pub const VARIANTS: &[VariantInfo] = &[
    VariantInfo {
        name: "plain",
        mechanic: "open mint + permissionless burn (WildToken baseline)",
        who_gets_hurt: "holder — supply dilutes endlessly and anyone can burn anyone's balance",
        exit_possible: "YES — transfer path is clean; the danger is value, not liquidity",
    },
    VariantInfo {
        name: "honeypot",
        mechanic: "buy-in allowed for everyone, sell-out only for allow-listed wallets; owner blacklist sweep on top",
        who_gets_hurt: "the buyer — balance looks spendable in every wallet UI, but transfer() reverts for them",
        exit_possible: "NO — sell path reverts for non-listed holders; owner quietly allow-lists chosen exits",
    },
    VariantInfo {
        name: "tax",
        mechanic: "configurable buy/sell tax (default 0% buy / 98% sell), max-wallet cap, transfer cooldown",
        who_gets_hurt: "the seller — 98% of the sell is skimmed to the tax wallet; 'feature, not bug' in the docs",
        exit_possible: "TECHNICALLY YES, practically no — you can sell, you keep 2%",
    },
    VariantInfo {
        name: "freeze",
        mechanic: "freeze authority locks any address from moving (Solana-style freeze, EVM edition)",
        who_gets_hurt: "whoever the authority freezes — balance frozen mid-flight, no appeal",
        exit_possible: "NO while frozen — unfreeze is a one-sided admin decision",
    },
    VariantInfo {
        name: "capped",
        mechanic: "MAX_SUPPLY hard cap — the honest-ish cousin",
        who_gets_hurt: "no one structurally — this is the control group",
        exit_possible: "YES — cap only bounds minting, transfers are clean",
    },
    VariantInfo {
        name: "proxy",
        mechanic: "EIP-1967 upgradeable: admin can swap logic any time; token owner is NOT the admin",
        who_gets_hurt: "holders — the rules can change after you buy (v2 skims 10% of every transfer)",
        exit_possible: "YES until the upgrade lands — then silently worse",
    },
    VariantInfo {
        name: "renounce-fake",
        mechanic: "ownership renounced to 0x…dEaD for trust theater, but the PROXY admin keeps upgradeTo",
        who_gets_hurt: "holders who read 'renounced' as safety — rules still change post-renounce",
        exit_possible: "YES until the post-renounce upgrade — renounced ≠ safe when a proxy admin lives",
    },
];

// ---------------------------------------------------------------------------
// honeypot — buy fine, sell reverts
// ---------------------------------------------------------------------------

const HONEYPOT: &str = r#"// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// @title FakeHoneypot — STUDY FIXTURE: one-sided transferability.
/// Buy-in is open; sell-out reverts for anyone not on the owner's allow-list.
/// The owner can also sweep blacklisted balances. Deploy: TESTNET ONLY.
contract FakeHoneypot {
    string public constant name = "Tether USD";
    string public constant symbol = "USDT";
    uint8 public constant decimals = 6;

    uint256 public totalSupply;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    address public owner;
    mapping(address => bool) public allowedSeller;
    mapping(address => bool) public blacklisted;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    constructor() {
        owner = msg.sender;
        allowedSeller[msg.sender] = true;
        totalSupply = 1_000_000_000 * 10 ** 6;
        balanceOf[msg.sender] = totalSupply;
        emit Transfer(address(0), msg.sender, totalSupply);
    }

    modifier onlyOwner() {
        require(msg.sender == owner, "HP: not owner");
        _;
    }

    function setAllowedSeller(address a, bool ok) external onlyOwner {
        allowedSeller[a] = ok;
    }

    function setBlacklisted(address a, bool ok) external onlyOwner {
        blacklisted[a] = ok;
    }

    /// the rug bonus: confiscate a blacklisted wallet entirely
    function sweepBlacklist(address a) external onlyOwner {
        require(blacklisted[a], "HP: not blacklisted");
        uint256 bal = balanceOf[a];
        balanceOf[a] = 0;
        balanceOf[owner] += bal;
        emit Transfer(a, owner, bal);
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        _move(msg.sender, to, amount);
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        uint256 allowed = allowance[from][msg.sender];
        require(allowed >= amount, "HP: insufficient allowance");
        if (allowed != type(uint256).max) allowance[from][msg.sender] = allowed - amount;
        _move(from, to, amount);
        return true;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        emit Approval(msg.sender, spender, amount);
        return true;
    }

    function _move(address from, address to, uint256 amount) internal {
        // THE HOOK: moving funds OUT requires the allow-list. Receiving is free —
        // that asymmetry is the whole trick.
        require(allowedSeller[from] || from == owner, "HP: sell blocked");
        require(!blacklisted[from] && !blacklisted[to], "HP: blacklisted");
        require(to != address(0), "HP: zero");
        require(balanceOf[from] >= amount, "HP: balance");
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
        emit Transfer(from, to, amount);
    }
}
"#;

// ---------------------------------------------------------------------------
// tax — 0% buy / 98% sell, max wallet, cooldown
// ---------------------------------------------------------------------------

const TAX: &str = r#"// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// @title FakeTaxToken — STUDY FIXTURE: asymmetric taxation.
/// Transfers FROM the designated pair are "buys" (0%); transfers TO the pair
/// are "sells" (98% default). 98% sell tax is legally a "feature" — read the
/// docs, it says so right there. That is exactly the trap. TESTNET ONLY.
contract FakeTaxToken {
    string public constant name = "Tether USD";
    string public constant symbol = "USDT";
    uint8 public constant decimals = 6;

    uint256 public totalSupply;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    address public owner;
    address public pair;            // the "exchange" — sells happen here
    address public taxWallet;
    uint16 public buyTaxBps;        // applied when the pair sends out
    uint16 public sellTaxBps;       // applied when anyone sends to the pair
    uint256 public maxWallet;       // 0 = disabled
    uint256 public cooldownSeconds; // 0 = disabled
    mapping(address => uint256) public lastSent;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    constructor() {
        owner = msg.sender;
        taxWallet = msg.sender;
        buyTaxBps = 0;
        sellTaxBps = 9800; // 98.00%
        totalSupply = 1_000_000_000 * 10 ** 6;
        balanceOf[msg.sender] = totalSupply;
        emit Transfer(address(0), msg.sender, totalSupply);
    }

    modifier onlyOwner() {
        require(msg.sender == owner, "TAX: not owner");
        _;
    }

    function setPair(address p) external onlyOwner { pair = p; }
    function setTaxWallet(address w) external onlyOwner { taxWallet = w; }
    function setTaxes(uint16 buyBps, uint16 sellBps) external onlyOwner {
        require(buyBps <= 10_000 && sellBps <= 10_000, "TAX: bps");
        buyTaxBps = buyBps;
        sellTaxBps = sellBps;
    }
    function setMaxWallet(uint256 m) external onlyOwner { maxWallet = m; }
    function setCooldown(uint256 s) external onlyOwner { cooldownSeconds = s; }

    function transfer(address to, uint256 amount) external returns (bool) {
        _move(msg.sender, to, amount);
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        uint256 allowed = allowance[from][msg.sender];
        require(allowed >= amount, "TAX: insufficient allowance");
        if (allowed != type(uint256).max) allowance[from][msg.sender] = allowed - amount;
        _move(from, to, amount);
        return true;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        emit Approval(msg.sender, spender, amount);
        return true;
    }

    function _isExempt(address a) internal view returns (bool) {
        return a == owner || a == taxWallet || a == pair;
    }

    function _move(address from, address to, uint256 amount) internal {
        require(to != address(0), "TAX: zero");
        require(balanceOf[from] >= amount, "TAX: balance");
        require(!_isExempt(from) || block.timestamp >= lastSent[from], "TAX: cooldown");
        if (cooldownSeconds > 0 && !_isExempt(from)) {
            require(block.timestamp >= lastSent[from] + cooldownSeconds, "TAX: cooldown");
        }

        uint256 tax = 0;
        if (from == pair && !_isExempt(to)) {
            tax = amount * buyTaxBps / 10_000;            // BUY side
        } else if (to == pair && !_isExempt(from)) {
            tax = amount * sellTaxBps / 10_000;           // SELL side
        }
        uint256 net = amount - tax;

        balanceOf[from] -= amount;
        balanceOf[to] += net;
        if (tax > 0) balanceOf[taxWallet] += tax;
        lastSent[from] = block.timestamp;

        if (maxWallet > 0 && !_isExempt(to)) {
            require(balanceOf[to] <= maxWallet, "TAX: max wallet");
        }
        emit Transfer(from, to, net);
        if (tax > 0) emit Transfer(from, taxWallet, tax);
    }
}
"#;

// ---------------------------------------------------------------------------
// freeze — address-level lock, Solana-style authority, EVM edition
// ---------------------------------------------------------------------------

const FREEZE: &str = r#"// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// @title FakeFreezeToken — STUDY FIXTURE: freeze authority.
/// One address can lock any wallet in place. Balance stays visible; movement
/// stops. TESTNET ONLY.
contract FakeFreezeToken {
    string public constant name = "Tether USD";
    string public constant symbol = "USDT";
    uint8 public constant decimals = 6;

    uint256 public totalSupply;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    address public owner;
    address public freezeAuthority;
    mapping(address => bool) public frozen;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);
    event Freeze(address indexed account, bool status);

    constructor() {
        owner = msg.sender;
        freezeAuthority = msg.sender;
        totalSupply = 1_000_000_000 * 10 ** 6;
        balanceOf[msg.sender] = totalSupply;
        emit Transfer(address(0), msg.sender, totalSupply);
    }

    modifier onlyOwner() {
        require(msg.sender == owner, "FRZ: not owner");
        _;
    }

    function setFreezeAuthority(address a) external onlyOwner { freezeAuthority = a; }
    function setFrozen(address a, bool status) external {
        require(msg.sender == freezeAuthority, "FRZ: not authority");
        frozen[a] = status;
        emit Freeze(a, status);
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        _move(msg.sender, to, amount);
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        uint256 allowed = allowance[from][msg.sender];
        require(allowed >= amount, "FRZ: insufficient allowance");
        if (allowed != type(uint256).max) allowance[from][msg.sender] = allowed - amount;
        _move(from, to, amount);
        return true;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        emit Approval(msg.sender, spender, amount);
        return true;
    }

    function _move(address from, address to, uint256 amount) internal {
        require(!frozen[from], "FRZ: sender frozen");
        require(!frozen[to], "FRZ: recipient frozen");
        require(to != address(0), "FRZ: zero");
        require(balanceOf[from] >= amount, "FRZ: balance");
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
        emit Transfer(from, to, amount);
    }
}
"#;

// ---------------------------------------------------------------------------
// capped — the control group
// ---------------------------------------------------------------------------

const CAPPED: &str = r#"// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// @title FakeCappedToken — STUDY FIXTURE: the control group.
/// MAX_SUPPLY bounds minting; transfers are clean. Compare against `plain`
/// to show what an open mint actually costs the holder. TESTNET ONLY.
contract FakeCappedToken {
    string public constant name = "Tether USD";
    string public constant symbol = "USDT";
    uint8 public constant decimals = 6;

    uint256 public constant MAX_SUPPLY = 2_000_000_000 * 10 ** 6; // 2B ceiling

    uint256 public totalSupply;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    constructor() {
        totalSupply = 1_000_000_000 * 10 ** 6;
        balanceOf[msg.sender] = totalSupply;
        emit Transfer(address(0), msg.sender, totalSupply);
    }

    function mint(uint256 amount) external {
        require(totalSupply + amount <= MAX_SUPPLY, "CAP: max supply");
        totalSupply += amount;
        balanceOf[msg.sender] += amount;
        emit Transfer(address(0), msg.sender, amount);
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        _move(msg.sender, to, amount);
        return true;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        emit Approval(msg.sender, spender, amount);
        return true;
    }

    function _move(address from, address to, uint256 amount) internal {
        require(to != address(0), "CAP: zero");
        require(balanceOf[from] >= amount, "CAP: balance");
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
        emit Transfer(from, to, amount);
    }
}
"#;

// ---------------------------------------------------------------------------
// proxy / renounce-fake — EIP-1967, admin lives separately from token owner
// ---------------------------------------------------------------------------

const LOGIC_V1: &str = r#"// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// @title FakeLogicV1 — STUDY FIXTURE (behind FakeProxy). Slot layout is
/// shared with V2: totalSupply(0) balanceOf(1) allowance(2) owner(3) initialized(4).
contract FakeLogicV1 {
    string public constant name = "Tether USD";
    string public constant symbol = "USDT";
    uint8 public constant decimals = 6;

    uint256 public totalSupply;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    address public owner;
    bool public initialized;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    modifier onlyOwner() {
        require(msg.sender == owner, "L1: not owner");
        _;
    }

    function initialize() external {
        require(!initialized, "L1: already");
        initialized = true;
        owner = msg.sender;
        totalSupply = 1_000_000_000 * 10 ** 6;
        balanceOf[msg.sender] = totalSupply;
        emit Transfer(address(0), msg.sender, totalSupply);
    }

    function renounceOwnership() external onlyOwner {
        owner = 0x000000000000000000000000000000000000dEaD;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        _move(msg.sender, to, amount);
        return true;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        emit Approval(msg.sender, spender, amount);
        return true;
    }

    function _move(address from, address to, uint256 amount) internal {
        require(to != address(0), "L1: zero");
        require(balanceOf[from] >= amount, "L1: balance");
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
        emit Transfer(from, to, amount);
    }
}
"#;

const LOGIC_V2: &str = r#"// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// @title FakeLogicV2 — STUDY FIXTURE. Identical slot layout to V1; the new
/// logic skims 10% of every transfer() to `owner`. THIS is what an upgrade
/// can silently do to holders.
contract FakeLogicV2 {
    string public constant name = "Tether USD";
    string public constant symbol = "USDT";
    uint8 public constant decimals = 6;

    uint256 public totalSupply;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    address public owner;
    bool public initialized;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    function skimRateBps() external pure returns (uint256) {
        return 1000; // 10% — the "new fee model"
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        require(to != address(0), "L2: zero");
        require(balanceOf[msg.sender] >= amount, "L2: balance");
        uint256 skim = amount / 10;
        balanceOf[msg.sender] -= amount;
        balanceOf[to] += amount - skim;
        if (skim > 0 && owner != address(0)) {
            balanceOf[owner] += skim;
            emit Transfer(msg.sender, owner, skim);
        }
        emit Transfer(msg.sender, to, amount - skim);
        return true;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        emit Approval(msg.sender, spender, amount);
        return true;
    }
}
"#;

const PROXY: &str = r#"// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// @title FakeProxy — STUDY FIXTURE: minimal EIP-1967 admin proxy.
/// The admin is a SEPARATE key from the token owner. upgradeTo() answers only
/// to the admin — which is why "renounced ownership" on the logic contract
/// protects nobody when a proxy admin lives. TESTNET ONLY.
contract FakeProxy {
    bytes32 private constant IMPL_SLOT =
        0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc;
    bytes32 private constant ADMIN_SLOT =
        0xb53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d6103;

    constructor(address impl, address admin_) {
        assembly {
            sstore(IMPL_SLOT, impl)
            sstore(ADMIN_SLOT, admin_)
        }
    }

    function admin() external view returns (address a) {
        assembly { a := sload(ADMIN_SLOT) }
    }

    function implementation() external view returns (address i) {
        assembly { i := sload(IMPL_SLOT) }
    }

    function upgradeTo(address newImpl) external {
        assembly {
            if iszero(eq(caller(), sload(ADMIN_SLOT))) { revert(0, 0) }
            sstore(IMPL_SLOT, newImpl)
        }
    }

    fallback() external payable {
        assembly {
            let impl := sload(IMPL_SLOT)
            calldatacopy(0, 0, calldatasize())
            let ok := delegatecall(gas(), impl, 0, calldatasize(), 0, 0)
            returndatacopy(0, 0, returndatasize())
            switch ok
            case 0 { revert(0, returndatasize()) }
            default { return(0, returndatasize()) }
        }
    }
}
"#;
