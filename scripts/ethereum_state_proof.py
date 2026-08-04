#!/usr/bin/env python3
"""Bounded Ethereum EIP-1186 Merkle-Patricia proof verification."""

from __future__ import annotations

import hashlib
import json
from typing import Any


class ProofError(RuntimeError):
    pass


KECCAK_ROUND_CONSTANTS = (
    0x0000000000000001, 0x0000000000008082, 0x800000000000808A,
    0x8000000080008000, 0x000000000000808B, 0x0000000080000001,
    0x8000000080008081, 0x8000000000008009, 0x000000000000008A,
    0x0000000000000088, 0x0000000080008009, 0x000000008000000A,
    0x000000008000808B, 0x800000000000008B, 0x8000000000008089,
    0x8000000000008003, 0x8000000000008002, 0x8000000000000080,
    0x000000000000800A, 0x800000008000000A, 0x8000000080008081,
    0x8000000000008080, 0x0000000080000001, 0x8000000080008008,
)
KECCAK_ROTATIONS = (
    1, 3, 6, 10, 15, 21, 28, 36, 45, 55, 2, 14,
    27, 41, 56, 8, 25, 43, 62, 18, 39, 61, 20, 44,
)
KECCAK_PI = (
    10, 7, 11, 17, 18, 3, 5, 16, 8, 21, 24, 4,
    15, 23, 19, 13, 12, 2, 20, 14, 22, 9, 6, 1,
)
MASK_64 = (1 << 64) - 1


def _rotate_left_64(value: int, shift: int) -> int:
    return ((value << shift) | (value >> (64 - shift))) & MASK_64


def _keccak_f1600(state: list[int]) -> None:
    for constant in KECCAK_ROUND_CONSTANTS:
        columns = [
            state[index]
            ^ state[index + 5]
            ^ state[index + 10]
            ^ state[index + 15]
            ^ state[index + 20]
            for index in range(5)
        ]
        for index in range(5):
            delta = columns[(index - 1) % 5] ^ _rotate_left_64(
                columns[(index + 1) % 5], 1
            )
            for row in range(0, 25, 5):
                state[row + index] ^= delta
        carried = state[1]
        for index, destination in enumerate(KECCAK_PI):
            current = state[destination]
            state[destination] = _rotate_left_64(
                carried, KECCAK_ROTATIONS[index]
            )
            carried = current
        for row in range(0, 25, 5):
            values = state[row : row + 5]
            for index in range(5):
                state[row + index] = values[index] ^ (
                    (~values[(index + 1) % 5]) & values[(index + 2) % 5]
                )
        state[0] ^= constant


def keccak256(payload: bytes) -> bytes:
    state = [0] * 25
    rate = 136
    offset = 0
    while len(payload) - offset >= rate:
        block = payload[offset : offset + rate]
        for lane in range(rate // 8):
            state[lane] ^= int.from_bytes(
                block[lane * 8 : (lane + 1) * 8], "little"
            )
        _keccak_f1600(state)
        offset += rate
    final = bytearray(rate)
    remaining = payload[offset:]
    final[: len(remaining)] = remaining
    final[len(remaining)] ^= 0x01
    final[-1] ^= 0x80
    for lane in range(rate // 8):
        state[lane] ^= int.from_bytes(
            final[lane * 8 : (lane + 1) * 8], "little"
        )
    _keccak_f1600(state)
    return b"".join(value.to_bytes(8, "little") for value in state)[:32]


def _rlp_decode_at(data: bytes, cursor: int) -> tuple[bytes | list[Any], int]:
    if cursor >= len(data):
        raise ProofError("RLP input is truncated")
    prefix = data[cursor]
    if prefix <= 0x7F:
        return bytes([prefix]), cursor + 1
    if prefix <= 0xB7:
        length = prefix - 0x80
        start = cursor + 1
        end = start + length
        if end > len(data) or (length == 1 and data[start] <= 0x7F):
            raise ProofError("RLP string is non-canonical")
        return data[start:end], end
    if prefix <= 0xBF:
        size_length = prefix - 0xB7
        size_start = cursor + 1
        size_end = size_start + size_length
        if size_end > len(data) or data[size_start] == 0:
            raise ProofError("RLP string length is invalid")
        length = int.from_bytes(data[size_start:size_end], "big")
        start = size_end
        end = start + length
        if length < 56 or end > len(data):
            raise ProofError("RLP long string is invalid")
        return data[start:end], end
    if prefix <= 0xF7:
        length = prefix - 0xC0
        start = cursor + 1
        end = start + length
    else:
        size_length = prefix - 0xF7
        size_start = cursor + 1
        size_end = size_start + size_length
        if size_end > len(data) or data[size_start] == 0:
            raise ProofError("RLP list length is invalid")
        length = int.from_bytes(data[size_start:size_end], "big")
        start = size_end
        end = start + length
        if length < 56:
            raise ProofError("RLP long list is non-canonical")
    if end > len(data):
        raise ProofError("RLP list is truncated")
    output: list[Any] = []
    item_cursor = start
    while item_cursor < end:
        item, item_cursor = _rlp_decode_at(data, item_cursor)
        output.append(item)
    if item_cursor != end:
        raise ProofError("RLP list length mismatch")
    return output, end


def rlp_decode(data: bytes) -> bytes | list[Any]:
    value, cursor = _rlp_decode_at(data, 0)
    if cursor != len(data):
        raise ProofError("RLP input contains trailing data")
    return value


def _hex_bytes(value: Any, label: str, length: int | None = None) -> bytes:
    if not isinstance(value, str) or not value.startswith("0x"):
        raise ProofError(f"{label} is not hexadecimal")
    try:
        output = bytes.fromhex(value[2:])
    except ValueError as error:
        raise ProofError(f"{label} is not hexadecimal") from error
    if length is not None and len(output) != length:
        raise ProofError(f"{label} has an invalid length")
    return output


def _nibbles(value: bytes) -> list[int]:
    output: list[int] = []
    for byte in value:
        output.extend((byte >> 4, byte & 0x0F))
    return output


def _compact_path(value: bytes) -> tuple[bool, list[int]]:
    nibbles = _nibbles(value)
    if not nibbles or nibbles[0] > 3:
        raise ProofError("trie compact path is invalid")
    leaf = bool(nibbles[0] & 2)
    odd = bool(nibbles[0] & 1)
    if not odd and (len(nibbles) < 2 or nibbles[1] != 0):
        raise ProofError("trie compact path padding is invalid")
    return leaf, nibbles[1 if odd else 2 :]


def _trie_value(root: bytes, key: bytes, proof: list[Any]) -> bytes | None:
    if len(root) != 32 or not isinstance(proof, list) or not proof:
        raise ProofError("trie proof root or node set is invalid")
    path = _nibbles(key)
    expected = root
    cursor = 0
    for proof_index, encoded_hex in enumerate(proof):
        encoded = _hex_bytes(encoded_hex, "trie proof node")
        if (len(expected) == 32 and keccak256(encoded) != expected) or (
            len(expected) < 32 and encoded != expected
        ):
            raise ProofError("trie proof node reference mismatch")
        node = rlp_decode(encoded)
        if not isinstance(node, list):
            raise ProofError("trie proof node is not a list")
        if len(node) == 17:
            if cursor == len(path):
                if proof_index != len(proof) - 1 or not isinstance(node[16], bytes):
                    raise ProofError("trie branch proof has trailing nodes")
                return node[16] or None
            child = node[path[cursor]]
            cursor += 1
            if not isinstance(child, bytes):
                raise ProofError("trie branch child is invalid")
            if not child:
                if proof_index != len(proof) - 1:
                    raise ProofError("trie exclusion proof has trailing nodes")
                return None
            expected = child
            continue
        if len(node) == 2 and isinstance(node[0], bytes) and isinstance(node[1], bytes):
            leaf, segment = _compact_path(node[0])
            if path[cursor : cursor + len(segment)] != segment:
                if proof_index != len(proof) - 1:
                    raise ProofError("trie exclusion proof has trailing nodes")
                return None
            cursor += len(segment)
            if leaf:
                if cursor != len(path) or proof_index != len(proof) - 1:
                    raise ProofError("trie leaf path is incomplete")
                return node[1]
            if not node[1]:
                raise ProofError("trie extension child is empty")
            expected = node[1]
            continue
        raise ProofError("trie proof node shape is invalid")
    raise ProofError("trie proof ended before a value was established")


def _quantity(value: Any, label: str) -> int:
    if not isinstance(value, str) or not value.startswith("0x"):
        raise ProofError(f"{label} is not a hexadecimal quantity")
    try:
        return int(value, 16)
    except ValueError as error:
        raise ProofError(f"{label} is not a hexadecimal quantity") from error


def _rlp_integer(value: bytes | list[Any], label: str) -> int:
    if not isinstance(value, bytes) or (len(value) > 1 and value[0] == 0):
        raise ProofError(f"{label} is not a canonical RLP integer")
    return int.from_bytes(value, "big")


def verify_eip1186_proof(
    proof: Any,
    *,
    address: str,
    block_state_root: str,
    expected_storage: dict[str, str],
) -> dict[str, Any]:
    if not isinstance(proof, dict) or str(proof.get("address", "")).lower() != address.lower():
        raise ProofError("account proof identity mismatch")
    address_bytes = _hex_bytes(address.lower(), "account address", 20)
    state_root = _hex_bytes(block_state_root.lower(), "block state root", 32)
    account_value = _trie_value(
        state_root, keccak256(address_bytes), proof.get("accountProof")
    )
    if account_value is None:
        raise ProofError("account proof establishes non-existence")
    account = rlp_decode(account_value)
    if (
        not isinstance(account, list)
        or len(account) != 4
        or not all(isinstance(item, bytes) for item in account)
    ):
        raise ProofError("account proof value is invalid")
    nonce = _rlp_integer(account[0], "account nonce")
    balance = _rlp_integer(account[1], "account balance")
    storage_root = account[2]
    code_hash = account[3]
    if (
        nonce != _quantity(proof.get("nonce"), "proof nonce")
        or balance != _quantity(proof.get("balance"), "proof balance")
        or storage_root != _hex_bytes(proof.get("storageHash"), "storage root", 32)
        or code_hash != _hex_bytes(proof.get("codeHash"), "code hash", 32)
    ):
        raise ProofError("account proof fields disagree with the trie value")
    storage_proofs = proof.get("storageProof")
    if not isinstance(storage_proofs, list) or len(storage_proofs) != len(expected_storage):
        raise ProofError("storage proof set is incomplete")
    by_key: dict[int, dict[str, Any]] = {}
    for item in storage_proofs:
        if not isinstance(item, dict):
            raise ProofError("storage proof item is invalid")
        key = _quantity(item.get("key"), "storage proof key")
        if key in by_key:
            raise ProofError("storage proof key is duplicated")
        by_key[key] = item
    verified_storage: dict[str, str] = {}
    for slot, expected_word in expected_storage.items():
        slot_number = _quantity(slot, "expected storage slot")
        item = by_key.get(slot_number)
        if item is None:
            raise ProofError("expected storage proof is missing")
        trie_value = _trie_value(
            storage_root,
            keccak256(slot_number.to_bytes(32, "big")),
            item.get("proof"),
        )
        observed = 0 if trie_value is None else _rlp_integer(
            rlp_decode(trie_value), "storage trie value"
        )
        if observed != _quantity(item.get("value"), "storage proof value"):
            raise ProofError("storage proof value disagrees with the trie")
        if observed != _quantity(expected_word, "expected storage value"):
            raise ProofError("storage proof disagrees with direct storage state")
        verified_storage[slot.lower()] = "0x" + observed.to_bytes(32, "big").hex()
    canonical = json.dumps(proof, sort_keys=True, separators=(",", ":")).encode()
    return {
        "cryptographic_proof_valid": True,
        "proof_response_sha256": hashlib.sha256(canonical).hexdigest(),
        "account_proof_node_count": len(proof["accountProof"]),
        "storage_proof_node_count": sum(
            len(item["proof"]) for item in storage_proofs
        ),
        "storage_root": "0x" + storage_root.hex(),
        "code_hash": "0x" + code_hash.hex(),
        "verified_storage": verified_storage,
    }
