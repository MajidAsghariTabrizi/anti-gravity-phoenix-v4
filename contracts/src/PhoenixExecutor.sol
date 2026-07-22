// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "./interfaces/IERC20.sol";
import "./interfaces/IAaveV3Pool.sol";
import "./interfaces/IV3Pool.sol";

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

    struct PoolConfig {
        address factory;
        address token0;
        address token1;
        uint24 fee;
        bool approved;
    }

    struct ActiveExecution {
        bool active;
        bytes32 routeId;
        address asset;
        uint256 amount;
        uint256 baselineBalance;
    }

    struct SwapCallbackData {
        address tokenIn;
        address tokenOut;
        address pool;
    }

    address public owner;
    address public pendingOwner;
    address public flashProvider;
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

    constructor(address initialOwner, address initialFlashProvider) {
        if (initialOwner == address(0) || initialFlashProvider == address(0)) revert ZeroAddress();
        owner = initialOwner;
        flashProvider = initialFlashProvider;
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
            routeId: op.routeId,
            asset: op.flashAsset,
            amount: op.flashAmount,
            baselineBalance: IERC20(op.flashAsset).balanceOf(address(this))
        });

        emit OpportunityStarted(op.routeId, op.flashAsset, op.flashAmount);
        IAaveV3Pool(flashProvider).flashLoanSimple(address(this), op.flashAsset, op.flashAmount, abi.encode(op), 0);
        delete activeExecution;
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

        Opportunity memory op = abi.decode(params, (Opportunity));
        if (op.flashAsset != asset || op.flashAmount != amount || op.routeId != ctx.routeId) revert CallbackSpoof();

        uint256 amountIn = amount;
        for (uint256 i = 0; i < op.legs.length; i++) {
            Leg memory leg = op.legs[i];
            uint256 beforeOut = IERC20(leg.tokenOut).balanceOf(address(this));
            IV3Pool(leg.pool)
                .swap(
                    address(this),
                    leg.zeroForOne,
                    int256(amountIn),
                    leg.zeroForOne ? uint160(4_295_128_739) + 1 : type(uint160).max - 1,
                    abi.encode(SwapCallbackData({tokenIn: leg.tokenIn, tokenOut: leg.tokenOut, pool: leg.pool}))
                );
            uint256 received = IERC20(leg.tokenOut).balanceOf(address(this)) - beforeOut;
            if (received < leg.minAmountOut) revert InvalidLeg();
            amountIn = received;
        }

        uint256 repay = amount + premium;
        uint256 finalBalance = IERC20(asset).balanceOf(address(this));
        if (finalBalance < ctx.baselineBalance + repay) revert MinProfit(0, op.minProfit);
        uint256 realizedProfit = finalBalance - ctx.baselineBalance - repay;
        if (realizedProfit < op.minProfit) revert MinProfit(realizedProfit, op.minProfit);

        _safeApprove(asset, flashProvider, 0);
        _safeApprove(asset, flashProvider, repay);

        emit OpportunitySettled(op.routeId, asset, amount, premium, realizedProfit);
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

    function _safeTransfer(address token, address to, uint256 amount) internal {
        (bool ok, bytes memory ret) = token.call(abi.encodeCall(IERC20.transfer, (to, amount)));
        if (!ok || (ret.length != 0 && !abi.decode(ret, (bool)))) revert TransferFailed();
    }

    function _safeApprove(address token, address spender, uint256 amount) internal {
        (bool ok, bytes memory ret) = token.call(abi.encodeCall(IERC20.approve, (spender, amount)));
        if (!ok || (ret.length != 0 && !abi.decode(ret, (bool)))) revert TransferFailed();
    }
}
