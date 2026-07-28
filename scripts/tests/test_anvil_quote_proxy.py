import unittest

from scripts.anvil_quote_proxy import deterministic_response, encoded_gas_components


class AnvilQuoteProxyTests(unittest.TestCase):
    def test_quote_inputs_are_pinned_to_one_wei_fee_economics(self) -> None:
        self.assertEqual(
            deterministic_response(
                {"jsonrpc": "2.0", "id": 1, "method": "eth_estimateGas", "params": []}
            )["result"],
            "0x186a0",
        )
        self.assertEqual(
            deterministic_response(
                {
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "eth_maxPriorityFeePerGas",
                    "params": [],
                }
            )["result"],
            "0x1",
        )

    def test_node_interface_gas_components_are_deterministic(self) -> None:
        response = deterministic_response(
            {
                "jsonrpc": "2.0",
                "id": 3,
                "method": "eth_call",
                "params": [
                    {"to": "0x00000000000000000000000000000000000000C8"},
                    "latest",
                ],
            }
        )
        self.assertEqual(response["result"], encoded_gas_components())

    def test_unrelated_requests_are_forwarded(self) -> None:
        self.assertIsNone(
            deterministic_response(
                {"jsonrpc": "2.0", "id": 4, "method": "eth_chainId", "params": []}
            )
        )


if __name__ == "__main__":
    unittest.main()
