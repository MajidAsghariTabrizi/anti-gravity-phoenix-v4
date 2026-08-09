// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "../src/PhoenixExecutor.sol";
import "../script/DeployPhoenixExecutor.s.sol";

interface Vm {
    function chainId(uint256 newChainId) external;

    function deal(address account, uint256 newBalance) external;
}

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
                PhoenixExecutor(payable(msg.sender)).uniswapV3SwapCallback(int256(amountIn), 0, data);
                return (int256(amountIn), -int256(outputAmount));
            }
            MockERC20(token0).mint(recipient, outputAmount);
            PhoenixExecutor(payable(msg.sender)).uniswapV3SwapCallback(0, int256(amountIn), data);
            return (-int256(outputAmount), int256(amountIn));
        }
    }

        contract MockAavePool is IAaveV3Pool {
            uint256 public premium;
            PhoenixExecutor public withdrawalTarget;
            bytes4 public withdrawalError;
            address public liquidationCollateral;
            uint256 public liquidationCollateralAmount;
            uint256 public liquidationDebtAmount;

            constructor(uint256 p) {
                premium = p;
            }

            function acceptExecutorOwnership(PhoenixExecutor target) external {
                withdrawalTarget = target;
                target.acceptOwnership();
            }

            function setLiquidationResult(address collateral, uint256 amount) external {
                liquidationCollateral = collateral;
                liquidationCollateralAmount = amount;
            }

            function setLiquidationDebtAmount(uint256 amount) external {
                liquidationDebtAmount = amount;
            }

            function liquidationCall(
                address collateralAsset,
                address debtAsset,
                address,
                uint256 debtToCover,
                bool receiveAToken
            ) external override {
                require(!receiveAToken, "aToken");
                require(collateralAsset == liquidationCollateral, "collateral");
                uint256 consumed = liquidationDebtAmount == 0 ? debtToCover : liquidationDebtAmount;
                require(IERC20(debtAsset).transferFrom(msg.sender, address(this), consumed), "cover");
                MockERC20(collateralAsset).mint(msg.sender, liquidationCollateralAmount);
            }

            function flashLoanSimple(
                address receiverAddress,
                address asset,
                uint256 amount,
                bytes calldata params,
                uint16
            ) external override {
                if (address(withdrawalTarget) != address(0)) {
                    withdrawalTarget.setPaused(true);
                    try withdrawalTarget.withdrawToken(asset, 1) {
                        revert("active withdrawal accepted");
                    } catch (bytes memory reason) {
                        if (reason.length >= 4) {
                            bytes4 selector;
                            assembly {
                                selector := mload(add(reason, 32))
                            }
                            withdrawalError = selector;
                        }
                    }
                }
                MockERC20(asset).mint(receiverAddress, amount);
                bool ok = IAaveFlashBorrower(receiverAddress)
                    .executeOperation(asset, amount, premium, receiverAddress, params);
                require(ok, "callback");
                require(IERC20(asset).transferFrom(receiverAddress, address(this), amount + premium), "repay");
            }
        }

        contract MockAtlas {
            function shortfall() external pure returns (uint256 gasLiability, uint256 borrowLiability) {
                return (0, 0);
            }

            function reconcile(uint256) external payable returns (uint256 owed) {
                return 0;
            }

            function run(
                PhoenixExecutor target,
                address solverOpFrom,
                address executionEnvironment,
                address bidToken,
                uint256 bidAmount,
                bytes calldata solverOpData
            ) external {
                target.atlasSolverCall(solverOpFrom, executionEnvironment, bidToken, bidAmount, solverOpData, bytes(""));
            }
        }

        contract PhoenixExecutorTest {
            Vm private constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

            MockERC20 usdc;
            MockERC20 weth;
            MockAavePool aave;
            MockAtlas atlas;
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
                atlas = new MockAtlas();
                factory1 = new MockFactory();
                factory2 = new MockFactory();
                pool1 = new MockPool(address(factory1), address(usdc), address(weth), 500, 105);
                pool2 = new MockPool(address(factory2), address(weth), address(usdc), 500, 117);
                factory1.setPool(address(usdc), address(weth), 500, address(pool1));
                factory2.setPool(address(weth), address(usdc), 500, address(pool2));
                executor = new PhoenixExecutor(address(this), address(aave), address(atlas), address(weth));
                executor.setAsset(address(usdc), true);
                executor.setAsset(address(weth), true);
                executor.setRouter(originRouter, true);
                executor.setMaximumInputAmount(1_000);
                executor.setFactory(address(factory1), true);
                executor.setFactory(address(factory2), true);
                executor.approvePool(address(pool1), address(factory1), address(usdc), address(weth), 500, true);
                executor.approvePool(address(pool2), address(factory2), address(weth), address(usdc), 500, true);
                executor.setPaused(false);
                aave.setLiquidationResult(address(weth), 105);
            }

            function liquidationRequest(uint256 minProfit, uint256 maxBid)
                internal
                view
                returns (PhoenixExecutor.AaveLiquidationRequest memory request)
            {
                PhoenixExecutor.Leg[] memory legs = new PhoenixExecutor.Leg[](1);
                legs[0] = PhoenixExecutor.Leg({
                    pool: address(pool2),
                    tokenIn: address(weth),
                    tokenOut: address(usdc),
                    fee: 500,
                    zeroForOne: true,
                    minAmountOut: 110
                });
                request = PhoenixExecutor.AaveLiquidationRequest({
                    routeId: bytes32("aave-route-1"),
                    borrower: address(0xB0B),
                    debtAsset: address(usdc),
                    collateralAsset: address(weth),
                    repayAmount: 100,
                    receiveAToken: false,
                    maxInputAmount: 1_000,
                    minCollateralReceived: 100,
                    minUnwindOutput: 110,
                    minProfit: minProfit,
                    maxAtlasBid: maxBid,
                    deadline: block.timestamp + 1,
                    unwindLegs: legs
                });
            }

            function wethIdentityLiquidationRequest()
                internal
                view
                returns (PhoenixExecutor.AaveLiquidationRequest memory request)
            {
                PhoenixExecutor.Leg[] memory legs = new PhoenixExecutor.Leg[](0);
                request = PhoenixExecutor.AaveLiquidationRequest({
                    routeId: bytes32("aave-weth-identity"),
                    borrower: address(0xB0B),
                    debtAsset: address(weth),
                    collateralAsset: address(weth),
                    repayAmount: 100,
                    receiveAToken: false,
                    maxInputAmount: 1_000,
                    minCollateralReceived: 110,
                    minUnwindOutput: 110,
                    minProfit: 5,
                    maxAtlasBid: 0,
                    deadline: block.timestamp + 1,
                    unwindLegs: legs
                });
            }

            function testDirectAaveLiquidationHappyPath() public {
                setUp();
                executor.executeAaveLiquidation(liquidationRequest(5, 0));
                require(usdc.balanceOf(address(executor)) == 16, "liquidation profit retained");
            }

            function testDirectAaveLiquidationRejectsCappedActualRepay() public {
                setUp();
                aave.setLiquidationDebtAmount(99);
                try executor.executeAaveLiquidation(liquidationRequest(5, 0)) {
                    revert("capped actual repay accepted");
                } catch (bytes memory reason) {
                    require(bytes4(reason) == PhoenixExecutor.RepayAmountMismatch.selector, "wrong repay error");
                }
            }

            function testDirectAaveWethCollateralUsesDeterministicZeroLegRoute() public {
                setUp();
                aave.setLiquidationResult(address(weth), 110);
                executor.executeAaveLiquidation(wethIdentityLiquidationRequest());

                require(weth.balanceOf(address(executor)) == 9, "identity-route profit retained");
            }

            function testDirectAaveWethCollateralRejectsRepayRoundingCollision() public {
                setUp();
                PhoenixExecutor.AaveLiquidationRequest memory request = wethIdentityLiquidationRequest();
                request.repayAmount = 101;
                request.minCollateralReceived = 105;
                request.minUnwindOutput = 105;
                // A 100 WETH actual repayment returning 104 WETH collateral has
                // the same net balance delta as the predicted 101 -> 105 path.
                // The remaining Pool allowance must still expose the short repay.
                aave.setLiquidationResult(address(weth), 104);
                aave.setLiquidationDebtAmount(100);
                try executor.executeAaveLiquidation(request) {
                    revert("capped identity repay accepted");
                } catch (bytes memory reason) {
                    require(bytes4(reason) == PhoenixExecutor.RepayAmountMismatch.selector, "wrong identity error");
                }
            }

            function testAtlasAaveLiquidationPaysBoundedBidAndRetainsMinimumProfit() public {
                setUp();
                uint256 beforeBid = usdc.balanceOf(address(this));
                PhoenixExecutor.AaveLiquidationRequest memory request = liquidationRequest(5, 5);
                atlas.run(executor, address(this), address(this), address(usdc), 5, abi.encode(request));
                require(usdc.balanceOf(address(this)) == beforeBid + 5, "atlas bid not paid");
                require(usdc.balanceOf(address(executor)) == 11, "post-bid profit retained");
            }

            function testAtlasBidAboveRequestMaximumReverts() public {
                setUp();
                PhoenixExecutor.AaveLiquidationRequest memory request = liquidationRequest(5, 4);
                try atlas.run(executor, address(this), address(this), address(usdc), 5, abi.encode(request)) {
                    revert("excessive bid accepted");
                } catch {}
                require(usdc.balanceOf(address(executor)) == 0, "state changed after rejected bid");
            }

            receive() external payable {}

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

            function testStartsPausedWithNoInputOrApprovals() public {
                MockAavePool freshAave = new MockAavePool(1);
                PhoenixExecutor fresh = new PhoenixExecutor(
                    address(this), address(freshAave), address(0xA71A5), address(weth)
                );
                require(fresh.paused(), "executor did not start paused");
                require(fresh.maximumInputAmount() == 0, "maximum input was initialized");
                require(!fresh.authorizedSearchers(address(this)), "searcher was approved");
                require(!fresh.approvedAssets(address(usdc)), "asset was approved");
                require(!fresh.approvedRouters(originRouter), "router was approved");
                require(!fresh.approvedFactories(address(factory1)), "factory was approved");
                (,,,, bool approved) = fresh.approvedPools(address(pool1));
                require(!approved, "pool was approved");
            }

            function testExecutionFailsFromInitialPausedState() public {
                setUp();
                executor.setPaused(true);
                try executor.executeOpportunity(opportunity(5, block.timestamp + 1)) {
                    revert("paused execution accepted");
                } catch {}
            }

            function testWithdrawalFailsWhileUnpaused() public {
                setUp();
                usdc.mint(address(executor), 10);
                vm.deal(address(executor), 10);
                try executor.withdrawToken(address(usdc), 1) {
                    revert("unpaused token withdrawal accepted");
                } catch {}
                try executor.withdrawNative(1) {
                    revert("unpaused native withdrawal accepted");
                } catch {}
                require(usdc.balanceOf(address(executor)) == 10, "token balance changed");
                require(address(executor).balance == 10, "native balance changed");
            }

            function testOnlyOwnerCanWithdraw() public {
                setUp();
                executor.setPaused(true);
                usdc.mint(address(executor), 10);
                vm.deal(address(executor), 10);
                Attacker attacker = new Attacker();
                require(!attacker.tryWithdrawToken(executor, address(usdc), 1), "non-owner token withdrawal accepted");
                require(!attacker.tryWithdrawNative(executor, 1), "non-owner native withdrawal accepted");
            }

            function testTokenWithdrawalWorksWhilePausedAndOnlyPaysOwner() public {
                setUp();
                executor.setPaused(true);
                address other = address(0xCAFE);
                usdc.mint(address(executor), 10);
                uint256 ownerBefore = usdc.balanceOf(address(this));
                executor.withdrawToken(address(usdc), 7);
                require(usdc.balanceOf(address(this)) == ownerBefore + 7, "owner did not receive tokens");
                require(usdc.balanceOf(other) == 0, "another recipient received tokens");
                require(usdc.balanceOf(address(executor)) == 3, "executor token balance mismatch");
            }

            function testNativeWithdrawalWorksWhilePausedAndOnlyPaysOwner() public {
                setUp();
                executor.setPaused(true);
                address other = address(0xCAFE);
                vm.deal(address(executor), 10);
                uint256 ownerBefore = address(this).balance;
                executor.withdrawNative(7);
                require(address(this).balance == ownerBefore + 7, "owner did not receive native value");
                require(other.balance == 0, "another recipient received native value");
                require(address(executor).balance == 3, "executor native balance mismatch");
            }

            function testWithdrawalRejectsZeroTokenAndAmounts() public {
                setUp();
                executor.setPaused(true);
                try executor.withdrawToken(address(0), 1) {
                    revert("zero token accepted");
                } catch {}
                try executor.withdrawToken(address(usdc), 0) {
                    revert("zero token amount accepted");
                } catch {}
                try executor.withdrawNative(0) {
                    revert("zero native amount accepted");
                } catch {}
            }

            function testWithdrawalRejectsActiveExecution() public {
                setUp();
                executor.setSearcher(address(this), true);
                executor.transferOwnership(address(aave));
                aave.acceptExecutorOwnership(executor);
                executor.executeOpportunity(opportunity(5, block.timestamp + 1));
                require(aave.withdrawalError() == PhoenixExecutor.ExecutionActive.selector, "active guard not enforced");
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

            function tryWithdrawToken(PhoenixExecutor executor, address token, uint256 amount) external returns (bool) {
                try executor.withdrawToken(token, amount) {
                    return true;
                } catch {
                    return false;
                }
            }

            function tryWithdrawNative(PhoenixExecutor executor, uint256 amount) external returns (bool) {
                try executor.withdrawNative(amount) {
                    return true;
                } catch {
                    return false;
                }
            }
        }

        contract DeployPhoenixExecutorScriptTest {
            Vm private constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

            function testDeploysExactArbitrumCanaryConfiguration() public {
                vm.chainId(42161);
                DeployPhoenixExecutorScript script = new DeployPhoenixExecutorScript();
                PhoenixExecutor deployed = script.run();

                require(deployed.owner() == script.INITIAL_OWNER(), "owner mismatch");
                require(deployed.flashProvider() == script.FLASH_PROVIDER(), "flash provider mismatch");
                require(deployed.paused(), "deployment not paused");
                require(deployed.maximumInputAmount() == 0, "deployment input enabled");
                require(!deployed.authorizedSearchers(script.INITIAL_OWNER()), "searcher approved");
                require(!deployed.approvedAssets(script.FLASH_PROVIDER()), "asset approved");
                require(!deployed.approvedRouters(address(script)), "router approved");
                require(!deployed.approvedFactories(script.INITIAL_OWNER()), "factory approved");
                (,,,, bool poolApproved) = deployed.approvedPools(script.FLASH_PROVIDER());
                require(!poolApproved, "pool approved");
            }

            function testDeploymentRejectsAnotherChain() public {
                vm.chainId(1);
                DeployPhoenixExecutorScript script = new DeployPhoenixExecutorScript();
                try script.deploy() returns (PhoenixExecutor) {
                    revert("wrong chain accepted");
                } catch {}
            }
        }
