"""Append-only authoritative chain evidence for a fail-closed active release."""

from __future__ import annotations

import hashlib
import json
import os
import re
import stat
import tempfile
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any, Callable


EVIDENCE_SCHEMA = "phoenix.chain-reconciliation.v1"
EVIDENCE_MODE = 0o400
DIRECTORY_MODE = 0o700
MAX_EVIDENCE_BYTES = 64 * 1024
MAX_RPC_RESPONSE_BYTES = 1024 * 1024
RPC_TIMEOUT_SECONDS = 30
RPC_USER_AGENT = "anti-gravity-phoenix-rpc-gateway/4"
RPC_HEADER_SECRET = Path(
    "/etc/phoenix/secrets/phoenix-rpc-provider-slot-1-api-key"
)
ARBITRUM_CHAIN_ID = "0xa4b1"
PRIMARY_PROVIDER_URL = "https://arbitrum.nownodes.io/"
PRIMARY_PROVIDER_IDENTITY = "rpc-bf27592026588e7d"
PAUSED_SELECTOR = "0x5c975abb"
SET_PAUSED_SELECTOR = "0x16c38b3c"
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
DIGEST_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
ADDRESS_RE = re.compile(r"^0x[0-9a-f]{40}$")
TRANSACTION_RE = re.compile(r"^0x[0-9a-f]{64}$")
BLOCK_HASH_RE = TRANSACTION_RE
PROVIDER_IDENTITY_RE = re.compile(r"^rpc-[0-9a-f]{16}$")


class ReconciliationError(ValueError):
    """A chain reconciliation operation failed closed."""

    def __init__(self, code: str):
        super().__init__(code)
        self.code = code


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):  # noqa: ANN001
        return None


def canonical_json(value: object) -> bytes:
    return (
        json.dumps(value, indent=2, sort_keys=True, separators=(",", ": "))
        + "\n"
    ).encode("utf-8")


def evidence_digest(value: object) -> str:
    return f"sha256:{hashlib.sha256(canonical_json(value)).hexdigest()}"


def evidence_path(
    state_root: Path,
    active_release: str,
    protected_main_sha: str,
) -> Path:
    if not SHA_RE.fullmatch(active_release):
        raise ReconciliationError("CHAIN_EVIDENCE_RELEASE_SHA_INVALID")
    if not SHA_RE.fullmatch(protected_main_sha):
        raise ReconciliationError("CHAIN_EVIDENCE_PROTECTED_MAIN_SHA_INVALID")
    return (
        state_root
        / "chain-reconciliation"
        / f"{active_release}.{protected_main_sha}.json"
    )


def provider_identity(url: str) -> str:
    parsed = urllib.parse.urlsplit(url)
    if (
        parsed.scheme != "https"
        or not parsed.hostname
        or parsed.username is not None
        or parsed.password is not None
        or parsed.fragment
    ):
        raise ReconciliationError("CHAIN_EVIDENCE_PROVIDER_URL_INVALID")
    authority = f"https://{parsed.hostname.lower()}:{parsed.port or 443}"
    return f"rpc-{hashlib.sha256(authority.encode()).hexdigest()[:16]}"


def _rpc_call(url: str, method: str, params: list[object]) -> object:
    opener = urllib.request.build_opener(
        urllib.request.ProxyHandler({}),
        _NoRedirect(),
    )
    body = json.dumps(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        },
        separators=(",", ":"),
    ).encode()
    try:
        metadata = RPC_HEADER_SECRET.lstat()
        if (
            stat.S_ISLNK(metadata.st_mode)
            or not stat.S_ISREG(metadata.st_mode)
            or metadata.st_size < 1
            or metadata.st_size > 4096
        ):
            raise OSError
        header_value = RPC_HEADER_SECRET.read_text(encoding="utf-8").strip()
        if not header_value or "\n" in header_value or "\r" in header_value:
            raise OSError
    except (OSError, UnicodeError) as exc:
        raise ReconciliationError("CHAIN_EVIDENCE_RPC_CREDENTIAL_INVALID") from exc
    request = urllib.request.Request(
        url,
        data=body,
        headers={
            "Content-Type": "application/json",
            "User-Agent": RPC_USER_AGENT,
            "api-key": header_value,
        },
        method="POST",
    )
    try:
        with opener.open(request, timeout=RPC_TIMEOUT_SECONDS) as response:
            raw = response.read(MAX_RPC_RESPONSE_BYTES + 1)
    except (OSError, TimeoutError, urllib.error.URLError) as exc:
        raise ReconciliationError("CHAIN_EVIDENCE_RPC_UNAVAILABLE") from exc
    if len(raw) > MAX_RPC_RESPONSE_BYTES:
        raise ReconciliationError("CHAIN_EVIDENCE_RPC_RESPONSE_OVERSIZED")
    try:
        envelope = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise ReconciliationError("CHAIN_EVIDENCE_RPC_RESPONSE_INVALID") from exc
    if (
        not isinstance(envelope, dict)
        or envelope.get("jsonrpc") != "2.0"
        or envelope.get("id") != 1
        or ("result" in envelope) == ("error" in envelope)
    ):
        raise ReconciliationError("CHAIN_EVIDENCE_RPC_RESPONSE_INVALID")
    if "error" in envelope:
        raise ReconciliationError("CHAIN_EVIDENCE_RPC_ERROR")
    return envelope["result"]


def _hex_integer(value: object, code: str) -> int:
    if not isinstance(value, str) or not re.fullmatch(r"0x[0-9a-f]+", value):
        raise ReconciliationError(code)
    return int(value, 16)


def observe_contract_pause(
    executor_address: str,
    expected_code_hash: str,
    *,
    call: Callable[[str, str, list[object]], object] = _rpc_call,
) -> dict[str, Any]:
    """Return bounded, authenticated pause evidence for the reviewed executor."""

    if (
        not isinstance(executor_address, str)
        or not isinstance(expected_code_hash, str)
        or not ADDRESS_RE.fullmatch(executor_address)
        or not re.fullmatch(r"[0-9a-f]{64}", expected_code_hash)
    ):
        raise ReconciliationError("CHAIN_EVIDENCE_INPUT_INVALID")
    chain_id = call(PRIMARY_PROVIDER_URL, "eth_chainId", [])
    if chain_id != ARBITRUM_CHAIN_ID:
        raise ReconciliationError("CHAIN_EVIDENCE_CHAIN_ID_INVALID")
    code_raw = call(
        PRIMARY_PROVIDER_URL,
        "eth_getCode",
        [executor_address, "latest"],
    )
    if (
        not isinstance(code_raw, str)
        or not re.fullmatch(r"0x(?:[0-9a-f]{2})+", code_raw)
        or hashlib.sha256(bytes.fromhex(code_raw[2:])).hexdigest()
        != expected_code_hash
    ):
        raise ReconciliationError("CHAIN_EVIDENCE_EXECUTOR_IDENTITY_INVALID")
    paused_raw = call(
        PRIMARY_PROVIDER_URL,
        "eth_call",
        [{"to": executor_address, "data": PAUSED_SELECTOR}, "latest"],
    )
    if (
        not isinstance(paused_raw, str)
        or not re.fullmatch(r"0x[0-9a-f]{64}", paused_raw)
        or int(paused_raw, 16) not in {0, 1}
    ):
        raise ReconciliationError("CHAIN_EVIDENCE_PAUSE_STATE_INVALID")
    return {
        "chain_id": chain_id,
        "executor_address": executor_address,
        "paused": int(paused_raw, 16) == 1,
        "provider_identity": PRIMARY_PROVIDER_IDENTITY,
        "runtime_code_hash": expected_code_hash,
    }


def _receipt_evidence(
    url: str,
    transaction_hash: str,
    block_tag: str,
    call: Callable[[str, str, list[object]], object],
) -> tuple[dict[str, Any], str]:
    try:
        receipt = call(
            url,
            "eth_getTransactionReceipt",
            [transaction_hash],
        )
    except ReconciliationError as exc:
        if exc.code not in {
            "CHAIN_EVIDENCE_RPC_ERROR",
            "CHAIN_EVIDENCE_RPC_UNAVAILABLE",
        }:
            raise
        receipt = None
    if isinstance(receipt, dict):
        return receipt, "eth_getTransactionReceipt"

    block_receipts = call(url, "eth_getBlockReceipts", [block_tag])
    if not isinstance(block_receipts, list):
        raise ReconciliationError("CHAIN_EVIDENCE_RECEIPT_INVALID")
    matching = [
        value
        for value in block_receipts
        if (
            isinstance(value, dict)
            and isinstance(value.get("transactionHash"), str)
            and value["transactionHash"].lower() == transaction_hash
        )
    ]
    if len(matching) != 1:
        raise ReconciliationError("CHAIN_EVIDENCE_RECEIPT_INVALID")
    return matching[0], "eth_getBlockReceipts"


def _provider_observation(
    url: str,
    executor_address: str,
    transaction_hash: str,
    call: Callable[[str, str, list[object]], object],
) -> dict[str, Any]:
    chain_id = call(url, "eth_chainId", [])
    paused_raw = call(
        url,
        "eth_call",
        [{"to": executor_address, "data": PAUSED_SELECTOR}, "latest"],
    )
    transaction = call(url, "eth_getTransactionByHash", [transaction_hash])
    if chain_id != ARBITRUM_CHAIN_ID:
        raise ReconciliationError("CHAIN_EVIDENCE_CHAIN_ID_INVALID")
    if (
        not isinstance(paused_raw, str)
        or not re.fullmatch(r"0x[0-9a-f]{64}", paused_raw)
        or int(paused_raw, 16) != 1
    ):
        raise ReconciliationError("CHAIN_EVIDENCE_CONTRACT_NOT_PAUSED")
    if not isinstance(transaction, dict):
        raise ReconciliationError("CHAIN_EVIDENCE_TRANSACTION_MISSING")
    transaction_value = transaction.get("hash")
    if (
        not isinstance(transaction_value, str)
        or transaction_value.lower() != transaction_hash
    ):
        raise ReconciliationError("CHAIN_EVIDENCE_TRANSACTION_INVALID")
    transaction_block_tag = transaction.get("blockNumber")
    _hex_integer(
        transaction_block_tag,
        "CHAIN_EVIDENCE_TRANSACTION_INVALID",
    )
    receipt, receipt_source = _receipt_evidence(
        url,
        transaction_hash,
        transaction_block_tag,
        call,
    )
    receipt_hash = receipt.get("transactionHash")
    if (
        not isinstance(receipt_hash, str)
        or receipt_hash.lower() != transaction_hash
    ):
        raise ReconciliationError("CHAIN_EVIDENCE_RECEIPT_INVALID")
    receipt_status = _hex_integer(
        receipt.get("status"),
        "CHAIN_EVIDENCE_RECEIPT_INVALID",
    )
    receipt_block = _hex_integer(
        receipt.get("blockNumber"),
        "CHAIN_EVIDENCE_RECEIPT_INVALID",
    )
    transaction_block = _hex_integer(
        transaction.get("blockNumber"),
        "CHAIN_EVIDENCE_TRANSACTION_INVALID",
    )
    block_hash = receipt.get("blockHash")
    target = transaction.get("to")
    calldata = transaction.get("input")
    if (
        receipt_status != 1
        or receipt_block <= 0
        or transaction_block != receipt_block
        or not isinstance(block_hash, str)
        or not BLOCK_HASH_RE.fullmatch(block_hash.lower())
        or not isinstance(target, str)
        or target.lower() != executor_address
        or not isinstance(calldata, str)
        or len(calldata) != 74
        or calldata[:10].lower() != SET_PAUSED_SELECTOR
    ):
        raise ReconciliationError("CHAIN_EVIDENCE_TRANSACTION_INVALID")
    try:
        set_paused_value = int(calldata[10:], 16)
    except ValueError as exc:
        raise ReconciliationError("CHAIN_EVIDENCE_TRANSACTION_INVALID") from exc
    if set_paused_value != 0:
        raise ReconciliationError(
            "CHAIN_EVIDENCE_HISTORICAL_TRANSACTION_NOT_UNPAUSE"
        )
    return {
        "block_hash": block_hash.lower(),
        "block_number": receipt_block,
        "chain_id": chain_id,
        "classification": "unpause",
        "executor_address": executor_address,
        "input_selector": SET_PAUSED_SELECTOR,
        "paused": True,
        "provider_identity": provider_identity(url),
        "receipt_source": receipt_source,
        "receipt_status": receipt_status,
        "set_paused_value": False,
        "transaction_hash": transaction_hash,
    }


def collect_provider_evidence(
    providers: list[str],
    executor_address: str,
    transaction_hash: str,
    *,
    call: Callable[[str, str, list[object]], object] = _rpc_call,
) -> list[dict[str, Any]]:
    executor_address = executor_address.lower()
    transaction_hash = transaction_hash.lower()
    if (
        len(providers) != 1
        or not ADDRESS_RE.fullmatch(executor_address)
        or not TRANSACTION_RE.fullmatch(transaction_hash)
    ):
        raise ReconciliationError("CHAIN_EVIDENCE_INPUT_INVALID")
    observations = [
        _provider_observation(
            provider,
            executor_address,
            transaction_hash,
            call,
        )
        for provider in providers
    ]
    return observations


def build_evidence(
    *,
    active_release_sha: str,
    release_assets_sha: str,
    protected_main_sha: str,
    release_platform_manifest_sha256: str,
    executor_address: str,
    owner_transaction_hash: str,
    historical_contract_paused: bool,
    runtime: dict[str, object],
    providers: list[dict[str, Any]],
) -> dict[str, Any]:
    value = {
        "active_release_sha": active_release_sha,
        "executor_address": executor_address.lower(),
        "historical_release_evidence": {
            "contract_paused": historical_contract_paused,
            "owner_transaction_hash": owner_transaction_hash.lower(),
        },
        "owner_transaction_hash": owner_transaction_hash.lower(),
        "protected_main_sha": protected_main_sha,
        "provider_agreement": False,
        "providers": providers,
        "release_assets_sha": release_assets_sha,
        "release_platform_manifest_sha256": (
            release_platform_manifest_sha256
        ),
        "runtime": runtime,
        "schema": EVIDENCE_SCHEMA,
    }
    return validate_evidence(value)


def validate_evidence(
    value: object,
    expected: dict[str, object] | None = None,
) -> dict[str, Any]:
    required = {
        "active_release_sha",
        "executor_address",
        "historical_release_evidence",
        "owner_transaction_hash",
        "protected_main_sha",
        "provider_agreement",
        "providers",
        "release_assets_sha",
        "release_platform_manifest_sha256",
        "runtime",
        "schema",
    }
    if not isinstance(value, dict) or set(value) != required:
        raise ReconciliationError("CHAIN_EVIDENCE_SCHEMA_INVALID")
    if (
        value["schema"] != EVIDENCE_SCHEMA
        or not SHA_RE.fullmatch(str(value["active_release_sha"]))
        or not SHA_RE.fullmatch(str(value["release_assets_sha"]))
        or not SHA_RE.fullmatch(str(value["protected_main_sha"]))
        or not DIGEST_RE.fullmatch(
            str(value["release_platform_manifest_sha256"])
        )
        or not ADDRESS_RE.fullmatch(str(value["executor_address"]))
        or not TRANSACTION_RE.fullmatch(str(value["owner_transaction_hash"]))
        or value["provider_agreement"] is not False
    ):
        raise ReconciliationError("CHAIN_EVIDENCE_SCHEMA_INVALID")
    historical = value["historical_release_evidence"]
    if (
        not isinstance(historical, dict)
        or set(historical) != {
            "contract_paused",
            "owner_transaction_hash",
        }
        or type(historical["contract_paused"]) is not bool
        or historical["owner_transaction_hash"]
        != value["owner_transaction_hash"]
    ):
        raise ReconciliationError("CHAIN_EVIDENCE_SCHEMA_INVALID")
    runtime = value["runtime"]
    expected_runtime = {
        "active_attempts": 0,
        "armed": False,
        "execution_mode": "disarmed",
        "kill_switch": True,
        "live_executor_stopped": True,
        "open_routes": 0,
        "unresolved_submissions": 0,
    }
    if runtime != expected_runtime:
        raise ReconciliationError("CHAIN_EVIDENCE_RUNTIME_NOT_FAIL_CLOSED")
    providers = value["providers"]
    if not isinstance(providers, list) or len(providers) != 1:
        raise ReconciliationError("CHAIN_EVIDENCE_SCHEMA_INVALID")
    provider_keys = {
        "block_hash",
        "block_number",
        "chain_id",
        "classification",
        "executor_address",
        "input_selector",
        "paused",
        "provider_identity",
        "receipt_source",
        "receipt_status",
        "set_paused_value",
        "transaction_hash",
    }
    for provider in providers:
        if (
            not isinstance(provider, dict)
            or set(provider) != provider_keys
            or not PROVIDER_IDENTITY_RE.fullmatch(
                str(provider["provider_identity"])
            )
            or provider["chain_id"] != ARBITRUM_CHAIN_ID
            or provider["executor_address"] != value["executor_address"]
            or provider["transaction_hash"]
            != value["owner_transaction_hash"]
            or provider["paused"] is not True
            or provider["receipt_source"]
            not in {
                "eth_getBlockReceipts",
                "eth_getTransactionReceipt",
            }
            or provider["receipt_status"] != 1
            or type(provider["block_number"]) is not int
            or provider["block_number"] <= 0
            or not BLOCK_HASH_RE.fullmatch(str(provider["block_hash"]))
            or provider["input_selector"] != SET_PAUSED_SELECTOR
            or provider["set_paused_value"] is not False
            or provider["classification"] != "unpause"
        ):
            raise ReconciliationError("CHAIN_EVIDENCE_SCHEMA_INVALID")
    if expected is not None:
        for key, expected_value in expected.items():
            if value.get(key) != expected_value:
                raise ReconciliationError("CHAIN_EVIDENCE_BINDING_MISMATCH")
    return value


def _validate_directory(path: Path, expected_uid: int, expected_gid: int) -> None:
    metadata = path.lstat()
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or path.is_symlink()
        or stat.S_IMODE(metadata.st_mode) != DIRECTORY_MODE
        or metadata.st_uid != expected_uid
        or metadata.st_gid != expected_gid
    ):
        raise ReconciliationError("CHAIN_EVIDENCE_DIRECTORY_UNSAFE")


def read_evidence(
    path: Path,
    *,
    expected: dict[str, object] | None = None,
    expected_uid: int = 0,
    expected_gid: int = 0,
) -> dict[str, Any]:
    try:
        _validate_directory(path.parent, expected_uid, expected_gid)
        metadata = path.lstat()
    except OSError as exc:
        raise ReconciliationError("CHAIN_EVIDENCE_MISSING") from exc
    if (
        not stat.S_ISREG(metadata.st_mode)
        or path.is_symlink()
        or metadata.st_nlink != 1
        or stat.S_IMODE(metadata.st_mode) != EVIDENCE_MODE
        or metadata.st_uid != expected_uid
        or metadata.st_gid != expected_gid
        or metadata.st_size > MAX_EVIDENCE_BYTES
    ):
        raise ReconciliationError("CHAIN_EVIDENCE_FILE_UNSAFE")
    try:
        raw = path.read_bytes()
        value = json.loads(raw)
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise ReconciliationError("CHAIN_EVIDENCE_FILE_INVALID") from exc
    validated = validate_evidence(value, expected)
    if raw != canonical_json(validated):
        raise ReconciliationError("CHAIN_EVIDENCE_FILE_NONCANONICAL")
    return validated


def write_evidence(
    path: Path,
    value: dict[str, Any],
    *,
    expected_uid: int = 0,
    expected_gid: int = 0,
) -> bool:
    validated = validate_evidence(value)
    content = canonical_json(validated)
    try:
        if path.parent.exists() or path.parent.is_symlink():
            _validate_directory(path.parent, expected_uid, expected_gid)
        else:
            path.parent.mkdir(mode=DIRECTORY_MODE)
            _validate_directory(path.parent, expected_uid, expected_gid)
    except OSError as exc:
        raise ReconciliationError("CHAIN_EVIDENCE_DIRECTORY_UNSAFE") from exc
    if path.exists() or path.is_symlink():
        existing = read_evidence(
            path,
            expected_uid=expected_uid,
            expected_gid=expected_gid,
        )
        if canonical_json(existing) != content:
            raise ReconciliationError("CHAIN_EVIDENCE_ALREADY_DIFFERS")
        return False
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.",
        dir=path.parent,
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary, EVIDENCE_MODE)
        try:
            os.link(temporary, path, follow_symlinks=False)
        except FileExistsError:
            existing = read_evidence(
                path,
                expected_uid=expected_uid,
                expected_gid=expected_gid,
            )
            if canonical_json(existing) != content:
                raise ReconciliationError("CHAIN_EVIDENCE_ALREADY_DIFFERS")
            return False
        temporary.unlink()
        if os.name == "posix":
            directory_descriptor = os.open(
                path.parent,
                os.O_RDONLY | getattr(os, "O_DIRECTORY", 0),
            )
            try:
                os.fsync(directory_descriptor)
            finally:
                os.close(directory_descriptor)
        read_evidence(
            path,
            expected=validated,
            expected_uid=expected_uid,
            expected_gid=expected_gid,
        )
        return True
    finally:
        temporary.unlink(missing_ok=True)
