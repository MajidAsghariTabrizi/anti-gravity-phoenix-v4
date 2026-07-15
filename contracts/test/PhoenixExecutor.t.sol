// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "../src/PhoenixExecutor.sol";

contract MockERC20 is IERC20 {
    string public name;
    mapping(address => uint256) public override balanceOf;
    mapping(address => mapping(address => uint256)) public override allowance;

    constructor(string memory n) {
        name = n;
    }

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function transfer(address to, uint256 amount) external override returns (bool) {
        require(balanceOf[msg.sender] >= amount, "balance");
        balanceOf[msg.sender] -= amount;
        balanceOf[to] += amount;
        return true;
    }

    function approve(address spender, uint256 amount) external override returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external override returns (bool) {
        require(balanceOf[from] >= amount, "balance");
        require(allowance[from][msg.sender] >= amount, "allowance");
        allowance[from][msg.sender] -= amount;
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
        return true;
    }
}

    contract MockFactory is IV3Factory {
        mapping(bytes32 => address) public pools;

        function setPool(address tokenA, address tokenB, uint24 fee, address pool) external {
            pools[key(tokenA, tokenB, fee)] = pool;
            pools[key(tokenB, tokenA, fee)] = pool;
        }

        function getPool(address tokenA, address tokenB, uint24 fee) external view override returns (address) {
            return pools[key(tokenA, tokenB, fee)];
        }

        function key(address tokenA, address tokenB, uint24 fee) internal pure returns (bytes32) {
            return keccak256(abi.encode(tokenA, tokenB, fee));
        }
    }

    contract MockPool is IV3Pool {
        address public override token0;
        address public override token1;
        uint24 public override fee;
        address public override factory;
        uint256 public outputAmount;
        uint256 public lastAmountIn;

        constructor(address f, address a, address b, uint24 poolFee, uint256 out) {
            factory = f;
            token0 = a;
            token1 = b;
            fee = poolFee;
            outputAmount = out;
        }

        function setOutput(uint256 out) external {
            outputAmount = out;
        }

        function swap(address recipient, bool zeroForOne, int256 amountSpecified, uint160, bytes calldata data)
            external
            override
            returns (int256 amount0, int256 amount1)
        {
            uint256 amountIn = uint256(amountSpecified);
            lastAmountIn = amountIn;
            if (zeroForOne) {
                MockERC20(token1).mint(recipient, outputAmount);
                PhoenixExecutor(msg.sender).uniswapV3SwapCallback(int256(amountIn), 0, data);
                return (int256(amountIn), -int256(outputAmount));
            }
            MockERC20(token0).mint(recipient, outputAmount);
            PhoenixExecutor(msg.sender).uniswapV3SwapCallback(0, int256(amountIn), data);
            return (-int256(outputAmount), int256(amountIn));
        }
    }

        contract MockAavePool is IAaveV3Pool {
            uint256 public premium;

            constructor(uint256 p) {
                premium = p;
            }

            function flashLoanSimple(
                address receiverAddress,
                address asset,
                uint256 amount,
                bytes calldata params,
                uint16
            ) external override {
                MockERC20(asset).mint(receiverAddress, amount);
                bool ok = IAaveFlashBorrower(receiverAddress)
                    .executeOperation(asset, amount, premium, receiverAddress, params);
                require(ok, "callback");
                require(IERC20(asset).transferFrom(receiverAddress, address(this), amount + premium), "repay");
            }
        }

        contract PhoenixExecutorTest {
            MockERC20 usdc;
            MockERC20 weth;
            MockAavePool aave;
            MockFactory factory1;
            MockFactory factory2;
            MockPool pool1;
            MockPool pool2;
            PhoenixExecutor executor;
            address originRouter = address(0xBEEF);

            function setUp() public {
                usdc = new MockERC20("USDC");
                weth = new MockERC20("WETH");
                aave = new MockAavePool(1);
                factory1 = new MockFactory();
                factory2 = new MockFactory();
                pool1 = new MockPool(address(factory1), address(usdc), address(weth), 500, 105);
                pool2 = new MockPool(address(factory2), address(weth), address(usdc), 500, 117);
                factory1.setPool(address(usdc), address(weth), 500, address(pool1));
                factory2.setPool(address(weth), address(usdc), 500, address(pool2));
                executor = new PhoenixExecutor(address(this), address(aave));
                executor.setAsset(address(usdc), true);
                executor.setAsset(address(weth), true);
                executor.setRouter(originRouter, true);
                executor.setMaximumInputAmount(1_000);
                executor.setFactory(address(factory1), true);
                executor.setFactory(address(factory2), true);
                executor.approvePool(address(pool1), address(factory1), address(usdc), address(weth), 500, true);
                executor.approvePool(address(pool2), address(factory2), address(weth), address(usdc), 500, true);
            }

            function opportunity(uint256 minProfit, uint256 deadline)
                internal
                view
                returns (PhoenixExecutor.Opportunity memory op)
            {
                PhoenixExecutor.Leg[] memory legs = new PhoenixExecutor.Leg[](2);
                legs[0] = PhoenixExecutor.Leg({
                    pool: address(pool1),
                    tokenIn: address(usdc),
                    tokenOut: address(weth),
                    fee: 500,
                    zeroForOne: true,
                    minAmountOut: 100
                });
                legs[1] = PhoenixExecutor.Leg({
                    pool: address(pool2),
                    tokenIn: address(weth),
                    tokenOut: address(usdc),
                    fee: 500,
                    zeroForOne: true,
                    minAmountOut: 100
                });
                op = PhoenixExecutor.Opportunity({
                    routeId: bytes32("route-1"),
                    originRouter: originRouter,
                    recipient: address(executor),
                    flashAsset: address(usdc),
                    flashAmount: 100,
                    maxInputAmount: 1_000,
                    minProfit: minProfit,
                    deadline: deadline,
                    legs: legs
                });
            }

            function testHappyPath() public {
                setUp();
                executor.executeOpportunity(opportunity(5, block.timestamp + 1));
                require(usdc.balanceOf(address(executor)) == 16, "profit retained");
                require(pool2.lastAmountIn() == 105, "actual prior output not chained");
            }

            function testUnauthorizedCaller() public {
                setUp();
                Attacker attacker = new Attacker();
                require(!attacker.tryExecute(executor, opportunity(5, block.timestamp + 1)), "unauthorized accepted");
            }

            function testFakeFlashCallback() public {
                setUp();
                bytes memory params = abi.encode(opportunity(5, block.timestamp + 1));
                try executor.executeOperation(address(usdc), 100, 1, address(executor), params) returns (bool) {
                    revert("fake callback accepted");
                } catch {}
            }

            function testFakeV3Callback() public {
                setUp();
                try executor.uniswapV3SwapCallback(
                    1, 0, abi.encode(PhoenixExecutor.SwapCallbackData(address(usdc), address(weth), address(this)))
                ) {
                    revert("fake v3 callback accepted");
                } catch {}
            }

            function testInvalidFactoryRejected() public {
                setUp();
                MockFactory other = new MockFactory();
                try executor.approvePool(address(pool1), address(other), address(usdc), address(weth), 500, true) {
                    revert("invalid factory accepted");
                } catch {}
            }

            function testExpiredOpportunity() public {
                setUp();
                try executor.executeOpportunity(opportunity(5, block.timestamp - 1)) {
                    revert("expired accepted");
                } catch {}
            }

            function testMinProfitFailure() public {
                setUp();
                pool2.setOutput(101);
                try executor.executeOpportunity(opportunity(5, block.timestamp + 1)) {
                    revert("min profit failure accepted");
                } catch {}
            }

            function testSlippageFailure() public {
                setUp();
                pool1.setOutput(99);
                try executor.executeOpportunity(opportunity(5, block.timestamp + 1)) {
                    revert("slippage failure accepted");
                } catch {}
            }

            function testPausedContract() public {
                setUp();
                executor.setPaused(true);
                try executor.executeOpportunity(opportunity(5, block.timestamp + 1)) {
                    revert("paused accepted");
                } catch {}
            }

            function testUnsupportedAsset() public {
                setUp();
                PhoenixExecutor.Opportunity memory op = opportunity(5, block.timestamp + 1);
                op.flashAsset = address(weth);
                try executor.executeOpportunity(op) {
                    revert("unsupported asset accepted");
                } catch {}
            }

            function testUnsupportedIntermediateToken() public {
                setUp();
                executor.setAsset(address(weth), false);
                try executor.executeOpportunity(opportunity(5, block.timestamp + 1)) {
                    revert("unsupported intermediate token accepted");
                } catch {}
            }

            function testUnsupportedRouter() public {
                setUp();
                PhoenixExecutor.Opportunity memory op = opportunity(5, block.timestamp + 1);
                op.originRouter = address(0xBAD);
                try executor.executeOpportunity(op) {
                    revert("unsupported router accepted");
                } catch {}
            }

            function testUnsupportedPool() public {
                setUp();
                PhoenixExecutor.Opportunity memory op = opportunity(5, block.timestamp + 1);
                op.legs[0].pool = address(this);
                try executor.executeOpportunity(op) {
                    revert("unsupported pool accepted");
                } catch {}
            }

            function testInvalidRecipient() public {
                setUp();
                PhoenixExecutor.Opportunity memory op = opportunity(5, block.timestamp + 1);
                op.recipient = address(this);
                try executor.executeOpportunity(op) {
                    revert("invalid recipient accepted");
                } catch {}
            }

            function testMaximumInputGuard() public {
                setUp();
                PhoenixExecutor.Opportunity memory op = opportunity(5, block.timestamp + 1);
                op.flashAmount = 1_001;
                op.maxInputAmount = 1_001;
                try executor.executeOpportunity(op) {
                    revert("oversized input accepted");
                } catch {}
            }

            function testMultipleSequentialOpportunities() public {
                setUp();
                executor.executeOpportunity(opportunity(5, block.timestamp + 1));
                executor.executeOpportunity(opportunity(5, block.timestamp + 1));
                require(usdc.balanceOf(address(executor)) == 32, "sequential profit mismatch");
            }
        }

        contract Attacker {
            function tryExecute(PhoenixExecutor executor, PhoenixExecutor.Opportunity memory op)
                external
                returns (bool)
            {
                try executor.executeOpportunity(op) {
                    return true;
                } catch {
                    return false;
                }
            }
        }
