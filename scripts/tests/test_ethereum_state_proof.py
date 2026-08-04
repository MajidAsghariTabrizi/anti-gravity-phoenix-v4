import unittest

from scripts.ethereum_state_proof import (
    ProofError,
    keccak256,
    verify_eip1186_proof,
)


def rlp_bytes(value: bytes) -> bytes:
    if len(value) == 1 and value[0] <= 0x7F:
        return value
    if len(value) < 56:
        return bytes([0x80 + len(value)]) + value
    length = len(value).to_bytes((len(value).bit_length() + 7) // 8, "big")
    return bytes([0xB7 + len(length)]) + length + value


def rlp_list(items: list[bytes]) -> bytes:
    payload = b"".join(rlp_bytes(item) for item in items)
    if len(payload) < 56:
        return bytes([0xC0 + len(payload)]) + payload
    length = len(payload).to_bytes((len(payload).bit_length() + 7) // 8, "big")
    return bytes([0xF7 + len(length)]) + length + payload


def quantity(value: int) -> str:
    return hex(value)


class EthereumStateProofTests(unittest.TestCase):
    def fixture(self):
        address = "0x" + "12" * 20
        slot = "0x" + "34" * 32
        storage_value = 0xF05F
        storage_key = keccak256(int(slot, 16).to_bytes(32, "big"))
        encoded_storage_value = rlp_bytes(storage_value.to_bytes(2, "big"))
        storage_leaf = rlp_list([b"\x20" + storage_key, encoded_storage_value])
        storage_root = keccak256(storage_leaf)
        code_hash = keccak256(b"reviewed-code")
        account_value = rlp_list([b"", b"", storage_root, code_hash])
        account_key = keccak256(bytes.fromhex(address[2:]))
        account_leaf = rlp_list([b"\x20" + account_key, account_value])
        state_root = keccak256(account_leaf)
        proof = {
            "address": address,
            "accountProof": ["0x" + account_leaf.hex()],
            "balance": "0x0",
            "codeHash": "0x" + code_hash.hex(),
            "nonce": "0x0",
            "storageHash": "0x" + storage_root.hex(),
            "storageProof": [
                {
                    "key": slot,
                    "value": quantity(storage_value),
                    "proof": ["0x" + storage_leaf.hex()],
                }
            ],
        }
        return address, slot, storage_value, state_root, proof

    def test_valid_account_and_storage_proof_is_cryptographically_bound(self):
        address, slot, value, state_root, proof = self.fixture()
        evidence = verify_eip1186_proof(
            proof,
            address=address,
            block_state_root="0x" + state_root.hex(),
            expected_storage={slot: "0x" + value.to_bytes(32, "big").hex()},
        )
        self.assertTrue(evidence["cryptographic_proof_valid"])
        self.assertEqual(evidence["account_proof_node_count"], 1)
        self.assertEqual(evidence["storage_proof_node_count"], 1)

    def test_wrong_block_state_root_is_rejected(self):
        address, slot, value, _, proof = self.fixture()
        with self.assertRaisesRegex(ProofError, "reference mismatch"):
            verify_eip1186_proof(
                proof,
                address=address,
                block_state_root="0x" + "00" * 32,
                expected_storage={slot: "0x" + value.to_bytes(32, "big").hex()},
            )

    def test_direct_storage_disagreement_is_rejected(self):
        address, slot, _, state_root, proof = self.fixture()
        with self.assertRaisesRegex(ProofError, "direct storage state"):
            verify_eip1186_proof(
                proof,
                address=address,
                block_state_root="0x" + state_root.hex(),
                expected_storage={slot: "0x" + "00" * 32},
            )


if __name__ == "__main__":
    unittest.main()
