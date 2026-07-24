#!/usr/bin/env python3
"""Loopback-only JSON-RPC proxy for deterministic autonomous integration quotes."""

import argparse
import json
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def encoded_gas_components(gas: int = 100_000, l1_gas: int = 0) -> str:
    return "0x" + "".join(f"{value:064x}" for value in (gas, l1_gas, 0, 0))


class Handler(BaseHTTPRequestHandler):
    upstream = ""

    def do_POST(self) -> None:  # noqa: N802
        if self.client_address[0] not in {"127.0.0.1", "::1"}:
            self.send_error(403)
            return
        length = int(self.headers.get("Content-Length", "0"))
        if length <= 0 or length > 2 * 1024 * 1024:
            self.send_error(400)
            return
        request = json.loads(self.rfile.read(length))
        method = request.get("method")
        if method == "eth_estimateGas":
            response = {"jsonrpc": "2.0", "id": request.get("id"), "result": "0x186a0"}
        elif method == "eth_call" and request.get("params", [{}])[0].get("to", "").lower() == (
            "0x00000000000000000000000000000000000000c8"
        ):
            response = {
                "jsonrpc": "2.0",
                "id": request.get("id"),
                "result": encoded_gas_components(),
            }
        else:
            forwarded = urllib.request.Request(
                self.upstream,
                data=json.dumps(request, separators=(",", ":")).encode(),
                headers={"Content-Type": "application/json"},
                method="POST",
            )
            with urllib.request.urlopen(forwarded, timeout=5) as upstream_response:
                response = json.load(upstream_response)
        body = json.dumps(response, separators=(",", ":")).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, _format: str, *_args: object) -> None:
        return


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--listen-port", type=int, required=True)
    parser.add_argument("--upstream", required=True)
    args = parser.parse_args()
    if not (1024 <= args.listen_port <= 65535):
        raise SystemExit("listen port is invalid")
    if not args.upstream.startswith("http://127.0.0.1:"):
        raise SystemExit("upstream must be loopback Anvil")
    Handler.upstream = args.upstream
    ThreadingHTTPServer(("127.0.0.1", args.listen_port), Handler).serve_forever()


if __name__ == "__main__":
    main()
