// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "../src/PhoenixExecutor.sol";

interface FoundryVm {
    function startBroadcast() external;

    function stopBroadcast() external;
}

contract DeployPhoenixExecutorScript {
    error WrongChain(uint256 actualChainId);
    error DeploymentInvariant(bytes32 invariant);

    uint256 public constant ARBITRUM_ONE_CHAIN_ID = 42161;
    address public constant INITIAL_OWNER = 0x9F30c00B68F7C0eDb4b4117B9f04E0cA2EB2C17a;
    address public constant FLASH_PROVIDER = 0x794a61358D6845594F94dc1DB02A252b5b4814aD;
    FoundryVm private constant vm = FoundryVm(address(uint160(uint256(keccak256("hevm cheat code")))));

    function run() external returns (PhoenixExecutor executor) {
        _requireArbitrumOne();
        vm.startBroadcast();
        executor = new PhoenixExecutor(INITIAL_OWNER, FLASH_PROVIDER);
        vm.stopBroadcast();
        _assertDeployment(executor);
    }

    function deploy() external returns (PhoenixExecutor executor) {
        _requireArbitrumOne();
        executor = new PhoenixExecutor(INITIAL_OWNER, FLASH_PROVIDER);
        _assertDeployment(executor);
    }

    function _requireArbitrumOne() private view {
        if (block.chainid != ARBITRUM_ONE_CHAIN_ID) revert WrongChain(block.chainid);
    }

    function _assertDeployment(PhoenixExecutor executor) private view {
        _require(executor.owner() == INITIAL_OWNER, "owner");
        _require(executor.flashProvider() == FLASH_PROVIDER, "flash-provider");
        _require(executor.paused(), "paused");
        _require(executor.maximumInputAmount() == 0, "maximum-input");

        _assertNoApprovals(executor, INITIAL_OWNER);
        _assertNoApprovals(executor, FLASH_PROVIDER);
    }

    function _assertNoApprovals(PhoenixExecutor executor, address probe) private view {
        (,,,, bool poolApproved) = executor.approvedPools(probe);
        _require(!executor.authorizedSearchers(probe), "searcher-approved");
        _require(!executor.approvedAssets(probe), "asset-approved");
        _require(!executor.approvedRouters(probe), "router-approved");
        _require(!executor.approvedFactories(probe), "factory-approved");
        _require(!poolApproved, "pool-approved");
    }

    function _require(bool condition, bytes32 invariant) private pure {
        if (!condition) revert DeploymentInvariant(invariant);
    }
}
