// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "../src/PhoenixExecutor.sol";

contract DeployPhoenixExecutorScript {
    function deploy(address owner, address flashProvider) external returns (PhoenixExecutor) {
        return new PhoenixExecutor(owner, flashProvider);
    }
}

