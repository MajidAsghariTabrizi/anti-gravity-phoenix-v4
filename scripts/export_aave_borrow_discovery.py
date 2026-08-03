#!/usr/bin/env python3
"""Export a sanitized, hash-bound Aave borrower-discovery transcript.

Borrow is the canonical discovery event because Aave V3 debt tokens are
non-transferable. Provider URLs and environment values are never emitted.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any


CHAIN_ID = 42161
POOL = "0x794a61358d6845594f94dc1db02a252b5b4814ad"
BORROW_TOPIC = "0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0"
MAX_PROVIDERS = 8
MAX_TOTAL_SPAN = 500_000_000
INITIAL_CHUNK = 2_000_000
MIN_CHUNK = 512
MAX_RPC_ATTEMPTS = 9
CHUNK_PACING_SECONDS = 2.0
DEFAULT_PROVIDER_ENV = "PHOENIX_ATLAS_ARCHIVE_PRIMARY_RPC_URL"
STATE_SCHEMA = "phoenix.atlas.aave-borrow-archive-state.v1"


class ExportError(RuntimeError):
    pass


def canonical_hash(value: Any) -> str:
    return hashlib.sha256(
        json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


def _validated_urls(urls: object, minimum: int) -> list[str]:
    if not isinstance(urls, list) or not (minimum <= len(urls) <= MAX_PROVIDERS):
        raise ExportError("reviewed provider set is unavailable")
    if len(set(urls)) != len(urls):
        raise ExportError("reviewed provider set contains duplicates")
    if not all(
        isinstance(url, str) and url.startswith(("http://", "https://"))
        for url in urls
    ):
        raise ExportError("reviewed provider configuration is invalid")
    return urls


def provider_urls(container: str | None, provider_envs: list[str]) -> list[str]:
    if provider_envs:
        if container is not None:
            raise ExportError("select either provider environment references or container")
        urls = []
        for name in provider_envs:
            if not name or not name.replace("_", "").isalnum() or name.upper() != name:
                raise ExportError("provider environment reference is invalid")
            value = os.environ.get(name)
            if not value:
                raise ExportError(f"provider environment reference is unset:{name}")
            urls.append(value)
        return _validated_urls(urls, 1)
    if container is None:
        raise ExportError("provider configuration is required")
    result = subprocess.run(
        ["sudo", "-n", "docker", "inspect", "--format", "{{json .Config.Env}}", container],
        check=True,
        capture_output=True,
        text=True,
    )
    values = {}
    for item in json.loads(result.stdout):
        if "=" in item:
            key, value = item.split("=", 1)
            values[key] = value
    raw = values.get("RPC_PROVIDER_URLS", "")
    urls = json.loads(raw) if raw.startswith("[") else [
        part.strip() for part in raw.split(",") if part.strip()
    ]
    return _validated_urls(urls, 2)


class Provider:
    def __init__(self, label: str, url: str) -> None:
        self.label = label
        self._url = url
        self._request_id = 0
        self.retry_count = 0

    def call(
        self, method: str, params: list[object], attempts: int = MAX_RPC_ATTEMPTS
    ) -> object:
        if method not in {
            "eth_chainId",
            "eth_blockNumber",
            "eth_getBlockByNumber",
            "eth_getCode",
            "eth_getStorageAt",
            "eth_call",
            "eth_getLogs",
        }:
            raise ExportError("RPC method outside read-only allowlist")
        failure = "unavailable"
        for attempt in range(attempts):
            self._request_id += 1
            body = json.dumps(
                {"jsonrpc": "2.0", "id": self._request_id, "method": method, "params": params},
                separators=(",", ":"),
            ).encode()
            request = urllib.request.Request(
                self._url,
                data=body,
                headers={"Content-Type": "application/json", "User-Agent": "phoenix-atlas-borrow-discovery/1"},
                method="POST",
            )
            try:
                timeout_seconds = 45 if method == "eth_getLogs" else 90
                with urllib.request.urlopen(request, timeout=timeout_seconds) as response:
                    payload = json.load(response)
                if payload.get("error") is not None:
                    error = payload["error"]
                    code = error.get("code") if isinstance(error, dict) else None
                    failure = f"rpc_error:{code}" if isinstance(code, int) else "rpc_error"
                    if method == "eth_getLogs" and code in {-32000, -32005}:
                        raise ExportError(f"{self.label}:{method}:{failure}")
                elif "result" in payload:
                    return payload["result"]
                else:
                    failure = "result_missing"
            except ExportError:
                raise
            except urllib.error.HTTPError as error:
                failure = f"http_error:{error.code}"
            except Exception:
                failure = "transport_error"
            if attempt + 1 < attempts:
                self.retry_count += 1
                time.sleep(min(2**attempt, 30))
        raise ExportError(f"{self.label}:{method}:{failure}")


class SSHContainerProvider:
    """Persistent credential-redacting bridge to a reviewed remote provider.

    The bridge reads the selected URL inside the remote gateway host and emits
    JSON-RPC results only. Provider URLs and container environment values never
    cross the SSH channel.
    """

    def __init__(
        self,
        label: str,
        ssh_executable: str,
        host: str,
        port: int,
        identity: Path,
        known_hosts: Path | None,
        container: str,
        provider_index: int,
    ) -> None:
        windows_client = ssh_executable.lower().endswith(".exe")
        if (
            not host
            or not ssh_executable
            or not 1 <= port <= 65535
            or (not windows_client and not identity.is_file())
            or (
                not windows_client
                and known_hosts is not None
                and not known_hosts.is_file()
            )
            or not container
            or not 0 <= provider_index < MAX_PROVIDERS
        ):
            raise ExportError("SSH provider bridge configuration is invalid")
        self.label = label
        self._request_id = 0
        self.retry_count = 0
        remote_source = f"""
import hashlib,json,subprocess,sys,urllib.request
container={container!r}
provider_index={provider_index}
allowed={{"eth_chainId","eth_blockNumber","eth_getBlockByNumber","eth_getCode","eth_getStorageAt","eth_call","eth_getLogs"}}
try:
    result=subprocess.run(
        ["sudo","-n","docker","inspect","--format","{{{{json .Config.Env}}}}",container],
        check=True,capture_output=True,text=True,
    )
    values={{}}
    for item in json.loads(result.stdout):
        if "=" in item:
            key,value=item.split("=",1); values[key]=value
    raw=values.get("RPC_PROVIDER_URLS","")
    providers=json.loads(raw) if raw.startswith("[") else [part.strip() for part in raw.split(",") if part.strip()]
    if not isinstance(providers,list) or provider_index >= len(providers):
        raise RuntimeError("provider unavailable")
    provider=providers[provider_index]
    if not isinstance(provider,str) or not provider.startswith(("http://","https://")):
        raise RuntimeError("provider invalid")
except Exception:
    sys.stdout.write(json.dumps({{"bridge_status":"startup_failed"}})+"\\n"); sys.stdout.flush()
    raise SystemExit(1)
sys.stdout.write(json.dumps({{
    "bridge_status":"ready",
    "provider_reference_sha256":hashlib.sha256(provider.encode()).hexdigest(),
}})+"\\n"); sys.stdout.flush()
for line in sys.stdin:
    try:
        body=json.loads(line)
        items=body if isinstance(body,list) else [body]
        if not items or any(not isinstance(item,dict) or item.get("method") not in allowed for item in items):
            raise RuntimeError("method denied")
        request=urllib.request.Request(
            provider,
            data=json.dumps(body,separators=(",",":")).encode(),
            headers={{"Content-Type":"application/json","User-Agent":"phoenix-atlas-archive-bridge/1"}},
            method="POST",
        )
        timeout=120 if any(item.get("method")=="eth_getLogs" for item in items) else 90
        with urllib.request.urlopen(request,timeout=timeout) as response:
            payload=json.load(response)
        def sanitize(item):
            safe={{"jsonrpc":"2.0","id":item.get("id") if isinstance(item,dict) else None}}
            if isinstance(item,dict) and item.get("error") is not None:
                error=item["error"]
                code=error.get("code") if isinstance(error,dict) else None
                safe["error"]={{"code":code if isinstance(code,int) else -32097,"message":"upstream rpc error"}}
            elif isinstance(item,dict) and "result" in item:
                safe["result"]=item["result"]
            else:
                safe["error"]={{"code":-32096,"message":"upstream result missing"}}
            return safe
        safe=[sanitize(item) for item in payload] if isinstance(payload,list) else sanitize(payload)
    except Exception:
        safe={{"jsonrpc":"2.0","id":None,"error":{{"code":-32098,"message":"redacted transport failure"}}}}
    sys.stdout.write(json.dumps(safe,separators=(",",":"))+"\\n"); sys.stdout.flush()
"""
        encoded = base64.b64encode(remote_source.encode()).decode()
        remote_command = (
            "python3 -u -c "
            + repr(f"import base64;exec(base64.b64decode('{encoded}'))")
        )
        ssh_command = [
                ssh_executable,
                "-T",
                "-p",
                str(port),
                "-i",
                str(identity),
                "-o",
                "BatchMode=yes",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "StrictHostKeyChecking=yes",
        ]
        if known_hosts is not None:
            ssh_command.extend(["-o", f"UserKnownHostsFile={known_hosts}"])
        ssh_command.extend([host, remote_command])
        self._process = subprocess.Popen(
            ssh_command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        if self._process.stdout is None:
            raise ExportError("SSH provider bridge stdout unavailable")
        try:
            ready = json.loads(self._process.stdout.readline())
        except Exception as error:
            diagnostic = "unavailable"
            if self._process.stderr is not None:
                diagnostic = self._process.stderr.read(512).strip().replace("\n", " ")
                if "http://" in diagnostic or "https://" in diagnostic:
                    diagnostic = "redacted"
            self.close()
            raise ExportError(
                f"SSH provider bridge startup failed:{diagnostic[:200]}"
            ) from error
        if ready.get("bridge_status") != "ready":
            diagnostic = "remote_startup_failed"
            if self._process.stderr is not None and self._process.poll() is not None:
                observed = self._process.stderr.read(512).strip().replace("\n", " ")
                if observed and "http://" not in observed and "https://" not in observed:
                    diagnostic = observed[:200]
            self.close()
            raise ExportError(f"SSH provider bridge startup failed:{diagnostic}")
        provider_reference = ready.get("provider_reference_sha256")
        if (
            not isinstance(provider_reference, str)
            or len(provider_reference) != 64
            or any(character not in "0123456789abcdef" for character in provider_reference)
        ):
            self.close()
            raise ExportError("SSH provider bridge identity is invalid")
        self.provider_reference_sha256 = provider_reference

    def call(
        self, method: str, params: list[object], attempts: int = MAX_RPC_ATTEMPTS
    ) -> object:
        if method not in {
            "eth_chainId",
            "eth_blockNumber",
            "eth_getBlockByNumber",
            "eth_getCode",
            "eth_getStorageAt",
            "eth_getLogs",
        }:
            raise ExportError("RPC method outside read-only allowlist")
        failure = "unavailable"
        for attempt in range(attempts):
            if self._process.poll() is not None:
                raise ExportError(f"{self.label}:{method}:bridge_stopped")
            self._request_id += 1
            request: object = {
                "jsonrpc": "2.0",
                "id": self._request_id,
                "method": method,
                "params": params,
            }
            try:
                payload = self._request(request)
                if payload.get("error") is not None:
                    error = payload["error"]
                    code = error.get("code") if isinstance(error, dict) else None
                    failure = f"rpc_error:{code}" if isinstance(code, int) else "rpc_error"
                    if method == "eth_getLogs" and code in {-32000, -32005}:
                        raise ExportError(f"{self.label}:{method}:{failure}")
                elif "result" in payload:
                    return payload["result"]
                else:
                    failure = "result_missing"
            except ExportError:
                raise
            except Exception:
                failure = "bridge_transport_error"
            if attempt + 1 < attempts:
                self.retry_count += 1
                time.sleep(min(2**attempt, 30))
        raise ExportError(f"{self.label}:{method}:{failure}")

    def _request(self, payload: object) -> object:
        if self._process.poll() is not None:
            raise ExportError(f"{self.label}:bridge_stopped")
        if self._process.stdin is None or self._process.stdout is None:
            raise ExportError(f"{self.label}:bridge_pipe_unavailable")
        self._process.stdin.write(json.dumps(payload, separators=(",", ":")) + "\n")
        self._process.stdin.flush()
        return json.loads(self._process.stdout.readline())

    def eth_calls(
        self, calls: list[tuple[str, str]], block: int, batch_size: int = 80
    ) -> list[str]:
        if not 1 <= batch_size <= 200:
            raise ExportError("RPC batch size is invalid")
        results: list[str] = []
        for cursor in range(0, len(calls), batch_size):
            batch = calls[cursor : cursor + batch_size]
            payload = []
            ids = []
            for target, data in batch:
                self._request_id += 1
                ids.append(self._request_id)
                payload.append(
                    {
                        "jsonrpc": "2.0",
                        "id": self._request_id,
                        "method": "eth_call",
                        "params": [{"to": target, "data": data}, hex(block)],
                    }
                )
            response = self._request(payload)
            if not isinstance(response, list):
                raise ExportError(f"{self.label}:eth_call:batch_invalid")
            mapped = {
                item.get("id"): item for item in response if isinstance(item, dict)
            }
            for request_id in ids:
                item = mapped.get(request_id)
                if (
                    item is None
                    or item.get("error") is not None
                    or not isinstance(item.get("result"), str)
                ):
                    raise ExportError(f"{self.label}:eth_call:batch_item_failed")
                results.append(str(item["result"]).lower())
            time.sleep(0.20)
        return results

    def close(self) -> None:
        process = getattr(self, "_process", None)
        if process is None:
            return
        if process.stdin is not None:
            process.stdin.close()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()


def header(provider: Provider, block_number: int) -> dict[str, object]:
    value = provider.call("eth_getBlockByNumber", [hex(block_number), False])
    if not isinstance(value, dict):
        raise ExportError(f"{provider.label}:block_header_incomplete")
    if int(str(value.get("number")), 16) != block_number:
        raise ExportError(f"{provider.label}:block_number_disagreement")
    block_hash = str(value.get("hash", "")).lower()
    parent_hash = str(value.get("parentHash", "")).lower()
    if len(block_hash) != 66 or len(parent_hash) != 66:
        raise ExportError(f"{provider.label}:block_hash_invalid")
    return {"number": block_number, "hash": block_hash, "parent_hash": parent_hash}


def get_logs(provider: Provider, start: int, end: int) -> list[dict[str, object]]:
    try:
        value = provider.call(
            "eth_getLogs",
            [{"address": POOL, "fromBlock": hex(start), "toBlock": hex(end), "topics": [BORROW_TOPIC]}],
            attempts=MAX_RPC_ATTEMPTS,
        )
    except ExportError as error:
        reason = str(error)
        range_limited = (
            "rpc_error:-32005" in reason
            or "rpc_error:-32000" in reason
            or "http_error:413" in reason
        )
        if not range_limited:
            raise
        if end - start + 1 <= MIN_CHUNK:
            raise
        print(
            f"archive_range_split={start}-{end}",
            file=sys.stderr,
            flush=True,
        )
        midpoint = (start + end) // 2
        return get_logs(provider, start, midpoint) + get_logs(provider, midpoint + 1, end)
    if not isinstance(value, list):
        raise ExportError(f"{provider.label}:log_result_invalid")
    return value


def sanitize_log(log: dict[str, object]) -> dict[str, object]:
    topics = log.get("topics")
    if not isinstance(topics, list) or len(topics) != 4:
        raise ExportError("Borrow log topic shape invalid")
    if str(topics[0]).lower() != BORROW_TOPIC:
        raise ExportError("Borrow log signature mismatch")
    if log.get("removed") is True:
        raise ExportError("removed Borrow log is not canonical")
    borrower = "0x" + str(topics[2]).lower()[-40:]
    return {
        "block_number": int(str(log["blockNumber"]), 16),
        "block_hash": str(log["blockHash"]).lower(),
        "transaction_hash": str(log["transactionHash"]).lower(),
        "transaction_index": int(str(log["transactionIndex"]), 16),
        "log_index": int(str(log["logIndex"]), 16),
        "reserve": "0x" + str(topics[1]).lower()[-40:],
        "borrower": borrower,
        "referral_code": int(str(topics[3]), 16),
        "data_sha256": hashlib.sha256(str(log.get("data", "")).lower().encode()).hexdigest(),
    }


def load_cached_chunk(path: Path, start: int, end: int) -> list[dict[str, object]]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ExportError("cached Borrow chunk is unreadable") from error
    if not isinstance(value, dict) or value.get("schema") != "phoenix.atlas.aave-borrow-chunk.v1":
        raise ExportError("cached Borrow chunk schema mismatch")
    if (
        value.get("chain_id") != CHAIN_ID
        or value.get("pool") != POOL
        or value.get("borrow_topic") != BORROW_TOPIC
    ):
        raise ExportError("cached Borrow chunk identity mismatch")
    observed = value.get("content_sha256")
    body = {key: item for key, item in value.items() if key != "content_sha256"}
    if observed != canonical_hash(body):
        raise ExportError("cached Borrow chunk content hash mismatch")
    if value.get("start_block") != start or value.get("end_block") != end:
        raise ExportError("cached Borrow chunk range mismatch")
    logs = value.get("logs")
    if not isinstance(logs, list):
        raise ExportError("cached Borrow chunk logs are invalid")
    if any(
        not isinstance(log, dict)
        or not start <= int(log.get("block_number", -1)) <= end
        for log in logs
    ):
        raise ExportError("cached Borrow chunk contains an out-of-range log")
    return logs


def write_cached_chunk(
    directory: Path, start: int, end: int, logs: list[dict[str, object]]
) -> None:
    value = {
        "schema": "phoenix.atlas.aave-borrow-chunk.v1",
        "chain_id": CHAIN_ID,
        "pool": POOL,
        "borrow_topic": BORROW_TOPIC,
        "start_block": start,
        "end_block": end,
        "logs": logs,
    }
    value["content_sha256"] = canonical_hash(value)
    destination = directory / f"{start}-{end}.json"
    temporary = directory / f".{start}-{end}.{os.getpid()}.tmp"
    temporary.write_text(
        json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )
    temporary.chmod(0o600)
    os.replace(temporary, destination)


def write_json_atomic(path: Path, value: dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.parent.chmod(0o700)
    temporary = path.parent / f".{path.name}.{os.getpid()}.tmp"
    temporary.write_text(
        json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )
    temporary.chmod(0o600)
    os.replace(temporary, path)


def chunk_summary(path: Path, start: int, end: int) -> dict[str, object]:
    value = json.loads(path.read_text(encoding="utf-8"))
    return {
        "start_block": start,
        "end_block": end,
        "content_sha256": value["content_sha256"],
        "log_count": len(value["logs"]),
    }


def archive_state(
    start_block: int,
    checkpoint_block: int,
    chunk_size: int,
    provider_bindings: list[dict[str, object]],
    chunks: list[dict[str, object]],
    archive_complete: bool,
    artifact_sha256: str | None = None,
) -> dict[str, object]:
    next_start = (
        checkpoint_block + 1 if archive_complete else start_block + len(chunks) * chunk_size
    )
    value: dict[str, object] = {
        "schema": STATE_SCHEMA,
        "chain_id": CHAIN_ID,
        "pool": POOL,
        "borrow_topic": BORROW_TOPIC,
        "start_block": start_block,
        "checkpoint_block": checkpoint_block,
        "chunk_size": chunk_size,
        "expected_chunk_count": (checkpoint_block - start_block) // chunk_size + 1,
        "completed_chunk_count": len(chunks),
        "next_start_block": next_start,
        "provider_bindings": provider_bindings,
        "chunks": chunks,
        "archive_complete": archive_complete,
        "artifact_content_sha256": artifact_sha256,
    }
    value["content_sha256"] = canonical_hash(value)
    return value


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--container")
    parser.add_argument(
        "--provider-env",
        action="append",
        default=[],
        help=f"environment variable containing a reviewed provider URL (default: {DEFAULT_PROVIDER_ENV})",
    )
    parser.add_argument("--provider-id", action="append", default=[])
    parser.add_argument("--ssh-provider-host")
    parser.add_argument("--ssh-executable", default="ssh")
    parser.add_argument("--ssh-provider-port", type=int, default=22)
    parser.add_argument("--ssh-provider-identity", type=Path)
    parser.add_argument("--ssh-provider-known-hosts", type=Path)
    parser.add_argument("--ssh-provider-container")
    parser.add_argument("--ssh-provider-index", type=int, default=0)
    parser.add_argument("--start-block", required=True, type=int)
    parser.add_argument("--checkpoint-block", required=True, type=int)
    parser.add_argument("--chunk-cache-dir", required=True, type=Path)
    parser.add_argument("--state-file", required=True, type=Path)
    parser.add_argument("--output-file", required=True, type=Path)
    parser.add_argument("--chunk-size", type=int, default=INITIAL_CHUNK)
    parser.add_argument("--pace-seconds", type=float, default=CHUNK_PACING_SECONDS)
    args = parser.parse_args()
    span = args.checkpoint_block - args.start_block + 1
    if (
        args.start_block < 0
        or span < 1
        or span > MAX_TOTAL_SPAN
        or not MIN_CHUNK <= args.chunk_size <= INITIAL_CHUNK
        or not 0 <= args.pace_seconds <= 60
    ):
        print("bounded Aave discovery export failed: invalid block bounds", file=sys.stderr)
        return 1
    try:
        ssh_selected = any(
            value is not None
            for value in (
                args.ssh_provider_host,
                args.ssh_provider_identity,
                args.ssh_provider_container,
            )
        )
        if ssh_selected:
            if (
                not args.ssh_provider_host
                or args.ssh_provider_identity is None
                or not args.ssh_provider_container
                or args.container is not None
                or args.provider_env
            ):
                raise ExportError("SSH provider bridge arguments are incomplete")
            provider_ids = args.provider_id or ["reviewed-primary"]
            if len(provider_ids) != 1:
                raise ExportError("SSH provider bridge requires exactly one identity")
            providers = [
                SSHContainerProvider(
                    provider_ids[0],
                    args.ssh_executable,
                    args.ssh_provider_host,
                    args.ssh_provider_port,
                    args.ssh_provider_identity,
                    args.ssh_provider_known_hosts,
                    args.ssh_provider_container,
                    args.ssh_provider_index,
                )
            ]
        else:
            provider_envs = args.provider_env or (
                [DEFAULT_PROVIDER_ENV] if args.container is None else []
            )
            urls = provider_urls(args.container, provider_envs)
            if args.provider_id and len(args.provider_id) != len(urls):
                raise ExportError("provider identity count mismatch")
            provider_ids = args.provider_id or [
                f"reviewed-provider-{i}" for i in range(1, len(urls) + 1)
            ]
            if len(set(provider_ids)) != len(provider_ids):
                raise ExportError("provider identities contain duplicates")
            providers = [Provider(label, url) for label, url in zip(provider_ids, urls)]
        bindings = []
        for provider in providers:
            chain = provider.call("eth_chainId", [])
            if int(str(chain), 16) != CHAIN_ID:
                raise ExportError(f"{provider.label}:chain_disagreement")
            start_header = header(provider, args.start_block)
            checkpoint_header = header(provider, args.checkpoint_block)
            bindings.append(
                {
                    "provider_id": provider.label,
                    "chain_id": CHAIN_ID,
                    "start_block": start_header,
                    "checkpoint_block": checkpoint_header,
                }
            )
        if len({item["checkpoint_block"]["hash"] for item in bindings}) != 1:
            raise ExportError("independent checkpoint hash disagreement")
        if len({item["start_block"]["hash"] for item in bindings}) != 1:
            raise ExportError("independent start hash disagreement")

        cache_dir = args.chunk_cache_dir
        cache_dir.mkdir(parents=True, exist_ok=True)
        cache_dir.chmod(0o700)
        logs = []
        cursor = args.start_block
        chunks = 0
        reused_chunks = 0
        chunk_manifest: list[dict[str, object]] = []
        write_json_atomic(
            args.state_file,
            archive_state(
                args.start_block,
                args.checkpoint_block,
                args.chunk_size,
                bindings,
                chunk_manifest,
                False,
            ),
        )
        while cursor <= args.checkpoint_block:
            chunk_end = min(cursor + args.chunk_size - 1, args.checkpoint_block)
            cache_path = cache_dir / f"{cursor}-{chunk_end}.json"
            if cache_path.is_file():
                chunk_logs = load_cached_chunk(cache_path, cursor, chunk_end)
                reused_chunks += 1
            else:
                chunk_logs = [
                    sanitize_log(log)
                    for log in get_logs(providers[0], cursor, chunk_end)
                ]
                write_cached_chunk(cache_dir, cursor, chunk_end, chunk_logs)
            logs.extend(chunk_logs)
            chunk_manifest.append(chunk_summary(cache_path, cursor, chunk_end))
            cursor = chunk_end + 1
            chunks += 1
            write_json_atomic(
                args.state_file,
                archive_state(
                    args.start_block,
                    args.checkpoint_block,
                    args.chunk_size,
                    bindings,
                    chunk_manifest,
                    False,
                ),
            )
            time.sleep(args.pace_seconds)
            if chunks % 10 == 0:
                print(
                    f"archive_chunks_completed={chunks} cache_reused={reused_chunks}",
                    file=sys.stderr,
                    flush=True,
                )
        logs.sort(key=lambda item: (item["block_number"], item["transaction_index"], item["log_index"]))
        identities = {(item["block_hash"], item["transaction_hash"], item["log_index"]) for item in logs}
        if len(identities) != len(logs):
            raise ExportError("duplicate canonical Borrow log identity")
        borrowers = sorted({str(item["borrower"]) for item in logs})
        output = {
            "schema": "phoenix.atlas.aave-borrow-discovery.v1",
            "chain_id": CHAIN_ID,
            "pool": POOL,
            "borrow_topic": BORROW_TOPIC,
            "start_block": args.start_block,
            "checkpoint_block": args.checkpoint_block,
            "provider_bindings": bindings,
            "source_methods": ["eth_chainId", "eth_getBlockByNumber", "eth_getLogs"],
            "archive_complete": True,
            "independent_archive_validation": len(bindings) >= 2,
            "chunk_size": args.chunk_size,
            "chunk_count": chunks,
            "log_count": len(logs),
            "borrower_count": len(borrowers),
            "borrowers": borrowers,
            "logs": logs,
        }
        output["content_sha256"] = canonical_hash(output)
        write_json_atomic(args.output_file, output)
        write_json_atomic(
            args.state_file,
            archive_state(
                args.start_block,
                args.checkpoint_block,
                args.chunk_size,
                bindings,
                chunk_manifest,
                True,
                str(output["content_sha256"]),
            ),
        )
        print(
            f"archive_complete=true chunks={chunks} logs={len(logs)} borrowers={len(borrowers)} content_sha256={output['content_sha256']}",
            file=sys.stderr,
            flush=True,
        )
        for provider in providers:
            close = getattr(provider, "close", None)
            if close is not None:
                close()
        return 0
    except Exception as error:
        print(f"bounded Aave discovery export failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
