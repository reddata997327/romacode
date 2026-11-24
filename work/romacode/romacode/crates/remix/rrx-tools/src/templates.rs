//! Built-in workspace templates (Template Explorer equivalent).

pub const TPL_STORAGE_SOL: &str = r#"// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// @title A tiny on-chain storage contract
contract Storage {
    uint256 private number;

    event NumberChanged(uint256 newValue);

    /// @notice Store a number
    function store(uint256 num) public {
        number = num;
        emit NumberChanged(num);
    }

    /// @notice Read the stored number
    function retrieve() public view returns (uint256) {
        return number;
    }
}
"#;

pub const TPL_ERC20_SOL: &str = r#"// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// @title Minimal, original ERC-20 implementation for rrx demos
contract RrxToken {
    string public name;
    string public symbol;
    uint8 public constant decimals = 18;
    uint256 public totalSupply;

    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    constructor(string memory _name, string memory _symbol, uint256 _initialSupply) {
        name = _name;
        symbol = _symbol;
        totalSupply = _initialSupply;
        balanceOf[msg.sender] = _initialSupply;
        emit Transfer(address(0), msg.sender, _initialSupply);
    }

    function transfer(address to, uint256 value) public returns (bool) {
        _transfer(msg.sender, to, value);
        return true;
    }

    function approve(address spender, uint256 value) public returns (bool) {
        allowance[msg.sender][spender] = value;
        emit Approval(msg.sender, spender, value);
        return true;
    }

    function transferFrom(address from, address to, uint256 value) public returns (bool) {
        uint256 allowed = allowance[from][msg.sender];
        require(allowed >= value, "ERC20: insufficient allowance");
        allowance[from][msg.sender] = allowed - value;
        _transfer(from, to, value);
        return true;
    }

    function _transfer(address from, address to, uint256 value) internal {
        require(to != address(0), "ERC20: transfer to zero");
        uint256 bal = balanceOf[from];
        require(bal >= value, "ERC20: insufficient balance");
        balanceOf[from] = bal - value;
        balanceOf[to] += value;
        emit Transfer(from, to, value);
    }
}
"#;

pub const TPL_STORAGE_TEST_SOL: &str = r#"// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "./Storage.sol";

/// @dev remix-tests convention: file name ends with _test.sol,
/// functions starting with `test` run, hooks: beforeAll/beforeEach/afterAll/afterEach.
contract StorageTest {
    Storage storageContract;

    function beforeAll() public {
        storageContract = new Storage();
    }

    function testInitialValue() public {
        Assert.equal(storageContract.retrieve(), uint256(0), "initial number should be 0");
    }

    function testStoreAndRetrieve() public {
        storageContract.store(42);
        Assert.equal(storageContract.retrieve(), uint256(42), "stored value should be 42");
    }

    function checkValueIsZeroAfterFreshDeployment() public {
        Storage fresh = new Storage();
        Assert.equal(fresh.retrieve(), uint256(0), "fresh storage starts at zero");
    }
}
"#;

pub const TPL_ERC20_TEST_SOL: &str = r#"// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "./RrxToken.sol";

contract RrxTokenTest {
    RrxToken token;

    function beforeAll() public {
        token = new RrxToken("Rrx Token", "RRX", 1000000 ether);
    }

    function testInitialSupplyToDeployer() public {
        Assert.equal(token.balanceOf(address(this)), uint256(1000000 ether), "deployer holds initial supply");
    }

    function testTransferMovesBalance() public {
        RrxToken t = new RrxToken("T", "T", 1000 ether);
        t.transfer(address(0xdead), 100 ether);
        Assert.equal(t.balanceOf(address(0xdead)), uint256(100 ether), "recipient balance");
        Assert.equal(t.balanceOf(address(this)), uint256(900 ether), "sender balance reduced");
    }

    function testTransferRevertsOnInsufficientBalance() public {
        RrxToken t = new RrxToken("T2", "T2", 1 ether);
        (bool ok, ) = address(t).call(abi.encodeWithSignature("transfer(address,uint256)", address(0xdead), 2 ether));
        Assert.assertFalse(ok, "transfer above balance must fail");
    }
}
"#;

pub const TPL_ERC20_SCRIPT: &str = r#"# rrx script — real deployment + assertions on the chain (run: rrx script demo.rrx)
compile .
account 0
deploy RrxToken --args "Rrx Token,RRX,1000000000000000000000000" -> token
send token.transfer(0xdead000000000000000000000000000000000001, 1000000000000000000000)
assert-eq call token.balanceOf(0xdead000000000000000000000000000000000001) 1000000000000000000000
expect-revert send token.transfer(0xdead000000000000000000000000000000000001, 1000000000000000000000000)
gas report
"#;

/// (relative path, content) pairs for each template.
pub fn template_files(name: &str) -> Result<Vec<(String, String)>, String> {
    match name {
        "storage" => Ok(vec![
            ("contracts/Storage.sol".into(), TPL_STORAGE_SOL.into()),
            ("tests/Storage_test.sol".into(), TPL_STORAGE_TEST_SOL.into()),
            (
                "README.md".into(),
                "# rrx storage workspace\n\n- `rrx compile` then `rrx test` on real EVM execution.\n".into(),
            ),
        ]),
        "erc20" => Ok(vec![
            ("contracts/RrxToken.sol".into(), TPL_ERC20_SOL.into()),
            ("tests/RrxToken_test.sol".into(), TPL_ERC20_TEST_SOL.into()),
            ("scripts/demo.rrx".into(), TPL_ERC20_SCRIPT.into()),
            (
                "README.md".into(),
                "# rrx erc20 workspace\n\n- `rrx compile` + `rrx test` — real solc, real EVM.\n- `rrx script demo.rrx` — deploy + transfer + assertions with gas report.\n- `rrx deploy RrxToken --args \"Rrx Token,RRX,1000000000000000000000000\"`\n- live mainnet: `rrx chain` / `rrx ens vitalik.eth` / `rrx token 0xdAC17F958D2ee523a2206206994597C13D831ec7`\n".into(),
            ),
        ]),
        "wildtoken" => Ok(vec![
            ("contracts/WildToken.sol".into(), crate::wildtoken::SOURCE.into()),
            ("tests/WildToken_test.sol".into(), crate::wildtoken::TEST_SOURCE.into()),
            ("README.md".into(), crate::wildtoken::README.into()),
        ]),
        "blank" => Ok(vec![(
            "contracts/.keep".into(),
            "put your Solidity sources here\n".into(),
        )]),
        other => Err(format!(
            "unknown template '{other}' (available: blank, storage, erc20, wildtoken)"
        )),
    }
}
