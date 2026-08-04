// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "./interfaces/IERC20.sol";
import "./interfaces/IAaveV3Pool.sol";
import "./interfaces/IV3Pool.sol";

interface IAtlasAccounting {
    function shortfall() external view returns (uint256 gasLiability, uint256 borrowLiability);
    function reconcile(uint256 maxApprovedGasSpend) external payable returns (uint256 owed);
}

interface IWETH {
    function withdraw(uint256 amount) external;
}

contract PhoenixExecutor is IAaveFlashBorrower {
    error Unauthorized();
    error Paused();
    error NotPaused();
    error ExecutionActive();
    error Reentrant();
    error ZeroAddress();
    error ZeroAmount();
    error UnsupportedAsset(address asset);
    error InvalidRouter(address router);
    error InvalidFactory(address factory);
    error InvalidPool(address pool);
    error InvalidLeg();
    error InvalidRecipient(address recipient);
    error InputLimit(uint256 amount, uint256 maximum);
    error Expired();
    error MinProfit(uint256 realizedProfit, uint256 minProfit);
    error CallbackSpoof();
    error NoActiveExecution();
    error MalformedLegs();
    error TransferFailed();
    error InvalidAtlas(address atlas);
    error InvalidSolver(address solver);
    error InvalidBid(address token, uint256 amount, uint256 maximum);
    error InvalidLiquidation();
    error InsufficientCollateral(uint256 received, uint256 minimum);

    event OwnershipTransferStarted(address indexed previousOwner, address indexed newOwner);
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);
    event SearcherUpdated(address indexed searcher, bool authorized);
    event PausedSet(bool paused);
    event FlashProviderUpdated(address indexed provider);
    event AssetUpdated(address indexed asset, bool approved);
    event RouterUpdated(address indexed router, bool approved);
    event FactoryUpdated(address indexed factory, bool approved);
    event PoolUpdated(address indexed pool, address indexed factory, bool approved);
    event MaximumInputUpdated(uint256 maximumInputAmount);
    event TokenWithdrawn(address indexed token, address indexed owner, uint256 amount);
    event NativeWithdrawn(address indexed owner, uint256 amount);
    event OpportunityStarted(bytes32 indexed routeId, address indexed asset, uint256 flashAmount);
    event OpportunitySettled(
        bytes32 indexed routeId, address indexed asset, uint256 flashAmount, uint256 premium, uint256 realizedProfit
    );
    event AaveLiquidationStarted(
        bytes32 indexed routeId,
        address indexed borrower,
        address indexed debtAsset,
        address collateralAsset,
        uint256 repayAmount,
        bool atlas
    );
    event AaveLiquidationSettled(
        bytes32 indexed routeId,
        address indexed borrower,
        address indexed debtAsset,
        uint256 repayAmount,
        uint256 premium,
        uint256 atlasBid,
        uint256 realizedProfit
    );

    struct Leg {
        address pool;
        address tokenIn;
        address tokenOut;
        uint24 fee;
        bool zeroForOne;
        uint256 minAmountOut;
    }

    struct Opportunity {
        bytes32 routeId;
        address originRouter;
        address recipient;
        address flashAsset;
        uint256 flashAmount;
        uint256 maxInputAmount;
        uint256 minProfit;
        uint256 deadline;
        Leg[] legs;
    }

    struct AaveLiquidationRequest {
        bytes32 routeId;
        address borrower;
        address debtAsset;
        address collateralAsset;
        uint256 repayAmount;
        bool receiveAToken;
        uint256 maxInputAmount;
        uint256 minCollateralReceived;
        uint256 minUnwindOutput;
        uint256 minProfit;
        uint256 maxAtlasBid;
        uint256 deadline;
        Leg[] unwindLegs;
    }

    struct PoolConfig {
        address factory;
        address token0;
        address token1;
        uint24 fee;
        bool approved;
    }

    struct ActiveExecution {
        bool active;
        uint8 kind;
        bool atlas;
        bytes32 routeId;
        address asset;
        uint256 amount;
        uint256 baselineBalance;
        uint256 requiredProfit;
        uint256 premium;
    }

    struct SwapCallbackData {
        address tokenIn;
        address tokenOut;
        address pool;
    }

    address public owner;
    address public pendingOwner;
    address public flashProvider;
    address public immutable atlas;
    address public immutable weth;
    bool public paused;
    bool private entered;

    mapping(address => bool) public authorizedSearchers;
    mapping(address => bool) public approvedAssets;
    mapping(address => bool) public approvedRouters;
    mapping(address => bool) public approvedFactories;
    mapping(address => PoolConfig) public approvedPools;
    uint256 public maximumInputAmount;

    ActiveExecution private activeExecution;

    modifier onlyOwner() {
        if (msg.sender != owner) revert Unauthorized();
        _;
    }

    modifier onlySearcher() {
        if (msg.sender != owner && !authorizedSearchers[msg.sender]) revert Unauthorized();
        _;
    }

    modifier whenNotPaused() {
        if (paused) revert Paused();
        _;
    }

    modifier whenPaused() {
        if (!paused) revert NotPaused();
        _;
    }

    modifier whenNoActiveExecution() {
        if (activeExecution.active) revert ExecutionActive();
        _;
    }

    modifier nonReentrant() {
        if (entered) revert Reentrant();
        entered = true;
        _;
        entered = false;
    }

    constructor(address initialOwner, address initialFlashProvider, address initialAtlas, address initialWeth) {
        if (
            initialOwner == address(0) || initialFlashProvider == address(0) || initialAtlas == address(0)
                || initialWeth == address(0)
        ) revert ZeroAddress();
        owner = initialOwner;
        flashProvider = initialFlashProvider;
        atlas = initialAtlas;
        weth = initialWeth;
        paused = true;
        emit OwnershipTransferred(address(0), initialOwner);
        emit FlashProviderUpdated(initialFlashProvider);
        emit PausedSet(true);
    }

    receive() external payable {}

    function transferOwnership(address newOwner) external onlyOwner {
        if (newOwner == address(0)) revert ZeroAddress();
        pendingOwner = newOwner;
        emit OwnershipTransferStarted(owner, newOwner);
    }

    function acceptOwnership() external {
        if (msg.sender != pendingOwner) revert Unauthorized();
        address oldOwner = owner;
        owner = pendingOwner;
        pendingOwner = address(0);
        emit OwnershipTransferred(oldOwner, owner);
    }

    function setSearcher(address searcher, bool authorized) external onlyOwner {
        if (searcher == address(0)) revert ZeroAddress();
        authorizedSearchers[searcher] = authorized;
        emit SearcherUpdated(searcher, authorized);
    }

    function setPaused(bool value) external onlyOwner {
        paused = value;
        emit PausedSet(value);
    }

    function withdrawToken(address token, uint256 amount)
        external
        onlyOwner
        whenPaused
        whenNoActiveExecution
        nonReentrant
    {
        if (token == address(0)) revert ZeroAddress();
        if (amount == 0) revert ZeroAmount();
        address recipient = owner;
        _safeTransfer(token, recipient, amount);
        emit TokenWithdrawn(token, recipient, amount);
    }

    function withdrawNative(uint256 amount) external onlyOwner whenPaused whenNoActiveExecution nonReentrant {
        if (amount == 0) revert ZeroAmount();
        address recipient = owner;
        (bool ok,) = payable(recipient).call{value: amount}("");
        if (!ok) revert TransferFailed();
        emit NativeWithdrawn(recipient, amount);
    }

    function setFlashProvider(address provider) external onlyOwner {
        if (provider == address(0)) revert ZeroAddress();
        flashProvider = provider;
        emit FlashProviderUpdated(provider);
    }

    function setAsset(address asset, bool approved) external onlyOwner {
        if (asset == address(0)) revert ZeroAddress();
        approvedAssets[asset] = approved;
        emit AssetUpdated(asset, approved);
    }

    function setRouter(address router, bool approved) external onlyOwner {
        if (router == address(0)) revert ZeroAddress();
        approvedRouters[router] = approved;
        emit RouterUpdated(router, approved);
    }

    function setMaximumInputAmount(uint256 maximum) external onlyOwner {
        if (maximum == 0) revert ZeroAmount();
        if (maximum > uint256(type(int256).max)) {
            revert InputLimit(maximum, uint256(type(int256).max));
        }
        maximumInputAmount = maximum;
        emit MaximumInputUpdated(maximum);
    }

    function setFactory(address factory, bool approved) external onlyOwner {
        if (factory == address(0)) revert ZeroAddress();
        approvedFactories[factory] = approved;
        emit FactoryUpdated(factory, approved);
    }

    function approvePool(address pool, address factory, address token0, address token1, uint24 fee, bool approved)
        external
        onlyOwner
    {
        if (pool == address(0) || factory == address(0) || token0 == address(0) || token1 == address(0)) {
            revert ZeroAddress();
        }
        if (!approvedFactories[factory]) revert InvalidFactory(factory);
        if (IV3Factory(factory).getPool(token0, token1, fee) != pool) revert InvalidPool(pool);
        if (IV3Pool(pool).token0() != token0 || IV3Pool(pool).token1() != token1 || IV3Pool(pool).fee() != fee) {
            revert InvalidPool(pool);
        }
        approvedPools[pool] =
            PoolConfig({factory: factory, token0: token0, token1: token1, fee: fee, approved: approved});
        emit PoolUpdated(pool, factory, approved);
    }

    function executeOpportunity(Opportunity calldata op) external onlySearcher whenNotPaused nonReentrant {
        if (op.flashAmount == 0) revert ZeroAmount();
        if (!approvedAssets[op.flashAsset]) revert UnsupportedAsset(op.flashAsset);
        if (!approvedRouters[op.originRouter]) revert InvalidRouter(op.originRouter);
        if (op.recipient != address(this)) revert InvalidRecipient(op.recipient);
        if (
            maximumInputAmount == 0 || op.maxInputAmount == 0 || op.maxInputAmount > maximumInputAmount
                || op.flashAmount > op.maxInputAmount
        ) {
            revert InputLimit(op.flashAmount, maximumInputAmount);
        }
        if (block.timestamp > op.deadline) revert Expired();
        _validateLegs(op);

        activeExecution = ActiveExecution({
            active: true,
            kind: 1,
            atlas: false,
            routeId: op.routeId,
            asset: op.flashAsset,
            amount: op.flashAmount,
            baselineBalance: IERC20(op.flashAsset).balanceOf(address(this)),
            requiredProfit: op.minProfit,
            premium: 0
        });

        emit OpportunityStarted(op.routeId, op.flashAsset, op.flashAmount);
        IAaveV3Pool(flashProvider).flashLoanSimple(address(this), op.flashAsset, op.flashAmount, abi.encode(op), 0);
        delete activeExecution;
    }

    function executeAaveLiquidation(AaveLiquidationRequest calldata request)
        external
        onlySearcher
        whenNotPaused
        nonReentrant
        returns (uint256 realizedProfit)
    {
        _startAaveLiquidation(request, false, 0);
        realizedProfit = _finishAaveLiquidation(request, 0);
        delete activeExecution;
    }

    /// @notice Atlas v1.6.4 solver callback. `solverOpData` is abi.encode(AaveLiquidationRequest).
    function atlasSolverCall(
        address solverOpFrom,
        address executionEnvironment,
        address bidToken,
        uint256 bidAmount,
        bytes calldata solverOpData,
        bytes calldata
    ) external payable whenNotPaused nonReentrant {
        if (msg.sender != atlas) revert InvalidAtlas(msg.sender);
        if (solverOpFrom != owner) revert InvalidSolver(solverOpFrom);
        AaveLiquidationRequest memory request = abi.decode(solverOpData, (AaveLiquidationRequest));
        if (
            bidAmount == 0 || bidAmount > request.maxAtlasBid
                || (bidToken != address(0) && bidToken != request.debtAsset)
                || (bidToken == address(0) && request.debtAsset != weth)
        ) revert InvalidBid(bidToken, bidAmount, request.maxAtlasBid);

        _startAaveLiquidation(request, true, bidAmount);
        _payAtlasBid(executionEnvironment, bidToken, bidAmount);
        _reconcileAtlas(msg.value);
        _finishAaveLiquidation(request, bidAmount);
        delete activeExecution;
    }

    function _payAtlasBid(address executionEnvironment, address bidToken, uint256 bidAmount) internal {
        if (bidToken == address(0)) {
            IWETH(weth).withdraw(bidAmount);
            (bool sent,) = payable(executionEnvironment).call{value: bidAmount}("");
            if (!sent) revert TransferFailed();
        } else {
            _safeTransfer(bidToken, executionEnvironment, bidAmount);
        }
    }

    function _reconcileAtlas(uint256 suppliedValue) internal {
        (uint256 gasLiability, uint256 borrowLiability) = IAtlasAccounting(atlas).shortfall();
        uint256 nativeRepayment = borrowLiability < suppliedValue ? borrowLiability : suppliedValue;
        IAtlasAccounting(atlas).reconcile{value: nativeRepayment}(gasLiability);
    }

    function _finishAaveLiquidation(AaveLiquidationRequest memory request, uint256 bidAmount)
        internal
        returns (uint256 realizedProfit)
    {
        uint256 finalBalance = IERC20(request.debtAsset).balanceOf(address(this));
        realizedProfit = finalBalance - activeExecution.baselineBalance;
        if (realizedProfit < request.minProfit) revert MinProfit(realizedProfit, request.minProfit);
        emit AaveLiquidationSettled(
            request.routeId,
            request.borrower,
            request.debtAsset,
            request.repayAmount,
            activeExecution.premium,
            bidAmount,
            realizedProfit
        );
    }

    function _startAaveLiquidation(AaveLiquidationRequest memory request, bool isAtlas, uint256 bidAmount) internal {
        if (
            request.borrower == address(0) || request.debtAsset == address(0) || request.collateralAsset == address(0)
                || request.repayAmount == 0 || request.receiveAToken || request.minCollateralReceived == 0
                || request.minUnwindOutput == 0 || request.minProfit == 0 || request.maxInputAmount == 0
                || maximumInputAmount == 0 || request.maxInputAmount > maximumInputAmount
                || request.repayAmount > request.maxInputAmount || block.timestamp > request.deadline
                || !approvedAssets[request.debtAsset] || !approvedAssets[request.collateralAsset]
        ) revert InvalidLiquidation();
        _validateUnwindLegs(request);
        uint256 requiredProfit = request.minProfit + bidAmount;
        activeExecution = ActiveExecution({
            active: true,
            kind: 2,
            atlas: isAtlas,
            routeId: request.routeId,
            asset: request.debtAsset,
            amount: request.repayAmount,
            baselineBalance: IERC20(request.debtAsset).balanceOf(address(this)),
            requiredProfit: requiredProfit,
            premium: 0
        });
        emit AaveLiquidationStarted(
            request.routeId, request.borrower, request.debtAsset, request.collateralAsset, request.repayAmount, isAtlas
        );
        IAaveV3Pool(flashProvider)
            .flashLoanSimple(address(this), request.debtAsset, request.repayAmount, abi.encode(request), 0);
    }

    function executeOperation(address asset, uint256 amount, uint256 premium, address initiator, bytes calldata params)
        external
        override
        returns (bool)
    {
        if (msg.sender != flashProvider || initiator != address(this)) revert CallbackSpoof();
        ActiveExecution memory ctx = activeExecution;
        if (!ctx.active) revert NoActiveExecution();
        if (asset != ctx.asset || amount != ctx.amount) revert CallbackSpoof();

        uint256 minimumProfit;
        if (ctx.kind == 1) {
            Opportunity memory op = abi.decode(params, (Opportunity));
            if (op.flashAsset != asset || op.flashAmount != amount || op.routeId != ctx.routeId) {
                revert CallbackSpoof();
            }
            _executeSwapLegs(op.legs, amount);
            minimumProfit = op.minProfit;
        } else if (ctx.kind == 2) {
            AaveLiquidationRequest memory request = abi.decode(params, (AaveLiquidationRequest));
            if (
                request.debtAsset != asset || request.repayAmount != amount || request.routeId != ctx.routeId
                    || request.receiveAToken
            ) revert CallbackSpoof();
            uint256 beforeCollateral = IERC20(request.collateralAsset).balanceOf(address(this));
            _safeApprove(asset, flashProvider, 0);
            _safeApprove(asset, flashProvider, amount);
            IAaveV3Pool(flashProvider).liquidationCall(request.collateralAsset, asset, request.borrower, amount, false);
            _safeApprove(asset, flashProvider, 0);
            uint256 collateralReceived = IERC20(request.collateralAsset).balanceOf(address(this)) - beforeCollateral;
            if (collateralReceived < request.minCollateralReceived) {
                revert InsufficientCollateral(collateralReceived, request.minCollateralReceived);
            }
            uint256 unwindOutput = _executeSwapLegs(request.unwindLegs, collateralReceived);
            if (unwindOutput < request.minUnwindOutput) revert InvalidLeg();
            minimumProfit = ctx.requiredProfit;
        } else {
            revert CallbackSpoof();
        }

        uint256 repay = amount + premium;
        uint256 finalBalance = IERC20(asset).balanceOf(address(this));
        if (finalBalance < ctx.baselineBalance + repay) revert MinProfit(0, minimumProfit);
        uint256 realizedProfit = finalBalance - ctx.baselineBalance - repay;
        if (realizedProfit < minimumProfit) revert MinProfit(realizedProfit, minimumProfit);

        _safeApprove(asset, flashProvider, 0);
        _safeApprove(asset, flashProvider, repay);
        activeExecution.premium = premium;

        if (ctx.kind == 1) {
            emit OpportunitySettled(ctx.routeId, asset, amount, premium, realizedProfit);
        }
        return true;
    }

    function uniswapV3SwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata data) external {
        if (!activeExecution.active) revert NoActiveExecution();
        PoolConfig memory cfg = approvedPools[msg.sender];
        if (!cfg.approved || !approvedFactories[cfg.factory]) revert CallbackSpoof();
        if (IV3Factory(cfg.factory).getPool(cfg.token0, cfg.token1, cfg.fee) != msg.sender) revert CallbackSpoof();

        SwapCallbackData memory cb = abi.decode(data, (SwapCallbackData));
        if (cb.pool != msg.sender) revert CallbackSpoof();

        if ((amount0Delta > 0) == (amount1Delta > 0)) revert CallbackSpoof();
        if (amount0Delta > 0) {
            if (cb.tokenIn != cfg.token0) revert CallbackSpoof();
            _safeTransfer(cb.tokenIn, msg.sender, uint256(amount0Delta));
        }
        if (amount1Delta > 0) {
            if (cb.tokenIn != cfg.token1) revert CallbackSpoof();
            _safeTransfer(cb.tokenIn, msg.sender, uint256(amount1Delta));
        }
    }

    function _validateLegs(Opportunity calldata op) internal view {
        if (op.legs.length == 0 || op.legs.length > 4) revert MalformedLegs();
        address expectedInput = op.flashAsset;
        for (uint256 i = 0; i < op.legs.length; i++) {
            Leg calldata leg = op.legs[i];
            PoolConfig memory cfg = approvedPools[leg.pool];
            if (!cfg.approved || !approvedFactories[cfg.factory]) revert InvalidPool(leg.pool);
            if (
                !approvedAssets[leg.tokenIn] || !approvedAssets[leg.tokenOut] || leg.tokenIn != expectedInput
                    || leg.fee != cfg.fee || leg.minAmountOut == 0
            ) revert InvalidLeg();
            if (leg.zeroForOne) {
                if (leg.tokenIn != cfg.token0 || leg.tokenOut != cfg.token1) revert InvalidLeg();
            } else {
                if (leg.tokenIn != cfg.token1 || leg.tokenOut != cfg.token0) revert InvalidLeg();
            }
            expectedInput = leg.tokenOut;
        }
        if (expectedInput != op.flashAsset) revert InvalidLeg();
    }

    function _validateUnwindLegs(AaveLiquidationRequest memory request) internal view {
        if (request.collateralAsset == request.debtAsset) {
            if (request.unwindLegs.length != 0) revert MalformedLegs();
            return;
        }
        if (request.unwindLegs.length == 0 || request.unwindLegs.length > 4) revert MalformedLegs();
        address expectedInput = request.collateralAsset;
        for (uint256 i = 0; i < request.unwindLegs.length; i++) {
            Leg memory leg = request.unwindLegs[i];
            PoolConfig memory cfg = approvedPools[leg.pool];
            if (!cfg.approved || !approvedFactories[cfg.factory]) revert InvalidPool(leg.pool);
            if (
                !approvedAssets[leg.tokenIn] || !approvedAssets[leg.tokenOut] || leg.tokenIn != expectedInput
                    || leg.fee != cfg.fee || leg.minAmountOut == 0
            ) revert InvalidLeg();
            if (leg.zeroForOne) {
                if (leg.tokenIn != cfg.token0 || leg.tokenOut != cfg.token1) revert InvalidLeg();
            } else if (leg.tokenIn != cfg.token1 || leg.tokenOut != cfg.token0) {
                revert InvalidLeg();
            }
            expectedInput = leg.tokenOut;
        }
        if (expectedInput != request.debtAsset) revert InvalidLeg();
    }

    function _executeSwapLegs(Leg[] memory legs, uint256 amountIn) internal returns (uint256 amountOut) {
        amountOut = amountIn;
        for (uint256 i = 0; i < legs.length; i++) {
            Leg memory leg = legs[i];
            uint256 beforeOut = IERC20(leg.tokenOut).balanceOf(address(this));
            IV3Pool(leg.pool)
                .swap(
                    address(this),
                    leg.zeroForOne,
                    int256(amountOut),
                    leg.zeroForOne ? uint160(4_295_128_739) + 1 : type(uint160).max - 1,
                    abi.encode(SwapCallbackData({tokenIn: leg.tokenIn, tokenOut: leg.tokenOut, pool: leg.pool}))
                );
            uint256 received = IERC20(leg.tokenOut).balanceOf(address(this)) - beforeOut;
            if (received < leg.minAmountOut) revert InvalidLeg();
            amountOut = received;
        }
    }

    function _safeTransfer(address token, address to, uint256 amount) internal {
        (bool ok, bytes memory ret) = token.call(abi.encodeCall(IERC20.transfer, (to, amount)));
        if (!ok || (ret.length != 0 && !abi.decode(ret, (bool)))) revert TransferFailed();
    }

    function _safeApprove(address token, address spender, uint256 amount) internal {
        (bool ok, bytes memory ret) = token.call(abi.encodeCall(IERC20.approve, (spender, amount)));
        if (!ok || (ret.length != 0 && !abi.decode(ret, (bool)))) revert TransferFailed();
    }
}
