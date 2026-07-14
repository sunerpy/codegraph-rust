// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import "./IERC20.sol";

error Unauthorized(address caller);

uint256 constant MAX_SUPPLY = 1000000;

contract Token is IERC20 {
    uint256 public total;

    event Transfer(address indexed from, uint256 value);

    enum Status {
        Active,
        Closed
    }

    struct Holder {
        address account;
        uint256 balance;
    }

    modifier onlyOwner() {
        _;
    }

    constructor() {
        total = MAX_SUPPLY;
    }

    fallback() external {
    }

    receive() external payable {
    }

    function transfer(address to, uint256 value) public onlyOwner returns (bool) {
        emit Transfer(to, value);
        return true;
    }
}

library Math {
    function add(uint256 a, uint256 b) internal pure returns (uint256) {
        return a + b;
    }
}
