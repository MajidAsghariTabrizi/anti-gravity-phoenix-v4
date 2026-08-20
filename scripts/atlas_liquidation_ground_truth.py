#!/usr/bin/env python3
"""Load reviewed Atlas liquidation ground truth into idempotent SQL inserts.

Reads the atlas-reconciler's reconciliation.ndjson on standard input and
emits strictly validated INSERT statements for the append-only
live_canary.atlas_liquidation_ground_truth evidence table. Only successful
onchain settlements (receipt_status == 1) with decoded Aave V3
LiquidationCall evidence are loaded; every other record is skipped and
counted. No network access, no secrets, integer-math only.
"""

from __future__ import annotations

import argparse
import io
import json
import os
import re
import sys
import tempfile

RECONCILIATION_SCHEMA = "phoenix.atlas-reconciliation.v1"
LOAD_SCHEMA = "phoenix.atlas-liquidation-ground-truth-load.v1"
MAX_INPUT_BYTES = 64 * 1024 * 1024
ADDRESS_PATTERN = re.compile(r"^0x[0-9a-f]{40}$")
TX_HASH_PATTERN = re.compile(r"^0x[0-9a-f]{64}$")
USER_OP_PATTERN = re.compile(r"^0x[0-9a-f]{64}$")
SHA256_PATTERN = re.compile(r"^[0-9a-f]{64}$")
UINT_PATTERN = re.compile(r"^[0-9]+$")
HEX_LOG_INDEX_PATTERN = re.compile(r"^[0-9a-f]+$")

TABLE = "live_canary.atlas_liquidation_ground_truth"
COLUMNS = (
    "transaction_hash",
    "log_index",
    "user_operation_hash",
    "borrower",
    "debt_asset",
    "collateral_asset",
    "debt_to_cover_wei",
    "liquidated_collateral_wei",
    "liquidator",
    "receive_a_token",
    "block_number",
    "reconciled_at",
    "transcript_sha256",
)


class ValidationError(Exception):
    pass


def require_address(value: object, field: str) -> str:
    if not isinstance(value, str) or not ADDRESS_PATTERN.match(value):
        raise ValidationError(f"{field}_invalid")
    return value


def require_hash(value: object, field: str, pattern: re.Pattern[str]) -> str:
    if not isinstance(value, str) or not pattern.match(value):
        raise ValidationError(f"{field}_invalid")
    return value


def require_uint_text(value: object, field: str) -> str:
    if not isinstance(value, str) or not UINT_PATTERN.match(value) or int(value) < 0:
        raise ValidationError(f"{field}_invalid")
    return value


def log_index_decimal(value: object) -> int:
    if not isinstance(value, str) or not HEX_LOG_INDEX_PATTERN.match(value) or value == "":
        raise ValidationError("log_index_invalid")
    try:
        return int(value, 16)
    except ValueError:
        raise ValidationError("log_index_invalid")


def parse_ndjson(stream: io.TextIOBase, max_bytes: int = MAX_INPUT_BYTES) -> list[dict]:
    records: list[dict] = []
    total = 0
    for line in stream:
        total += len(line.encode("utf-8"))
        if total > max_bytes:
            raise ValidationError("input_exceeds_byte_limit")
        if not line.strip():
            continue
        try:
            record = json.loads(line)
        except ValueError:
            raise ValidationError("line_not_json")
        if not isinstance(record, dict):
            raise ValidationError("line_not_object")
        records.append(record)
    return records


def record_rows(record: dict) -> list[dict]:
    if record.get("schema") != RECONCILIATION_SCHEMA:
        raise ValidationError("reconciliation_schema_invalid")
    if not isinstance(record.get("public_settlement_found"), bool):
        raise ValidationError("public_settlement_found_invalid")
    if not record["public_settlement_found"]:
        return []
    if record.get("receipt_status") != 1:
        return []
    tx_hash = require_hash(record.get("onchain_transaction"), "onchain_transaction", TX_HASH_PATTERN)
    user_op = require_hash(record.get("user_operation_hash"), "user_operation_hash", USER_OP_PATTERN)
    transcript = require_hash(record.get("transcript_sha256"), "transcript_sha256", SHA256_PATTERN)
    block = record.get("transaction_block")
    if not isinstance(block, int) or block <= 0:
        raise ValidationError("transaction_block_invalid")
    reconciled_at = record.get("reconciled_at")
    if not isinstance(reconciled_at, str) or not reconciled_at.endswith("Z"):
        raise ValidationError("reconciled_at_invalid")
    liquidations = record.get("public_liquidations")
    if not isinstance(liquidations, list):
        raise ValidationError("public_liquidations_invalid")
    rows: list[dict] = []
    for liquidation in liquidations:
        if not isinstance(liquidation, dict):
            raise ValidationError("liquidation_entry_invalid")
        if not isinstance(liquidation.get("receive_a_token"), bool):
            raise ValidationError("receive_a_token_invalid")
        rows.append(
            {
                "transaction_hash": tx_hash,
                "log_index": log_index_decimal(liquidation.get("log_index")),
                "user_operation_hash": user_op,
                "borrower": require_address(liquidation.get("borrower"), "borrower"),
                "debt_asset": require_address(liquidation.get("debt_asset"), "debt_asset"),
                "collateral_asset": require_address(liquidation.get("collateral_asset"), "collateral_asset"),
                "debt_to_cover_wei": require_uint_text(liquidation.get("debt_to_cover"), "debt_to_cover"),
                "liquidated_collateral_wei": require_uint_text(
                    liquidation.get("liquidated_collateral_amount"), "liquidated_collateral_amount"
                ),
                "liquidator": require_address(liquidation.get("liquidator"), "liquidator"),
                "receive_a_token": liquidation["receive_a_token"],
                "block_number": block,
                "reconciled_at": reconciled_at,
                "transcript_sha256": transcript,
            }
        )
    return rows


def render_inserts(rows: list[dict]) -> str:
    statements: list[str] = []
    for row in rows:
        values = ", ".join(
            [
                "'" + row["transaction_hash"] + "'",
                str(row["log_index"]),
                "'" + row["user_operation_hash"] + "'",
                "'" + row["borrower"] + "'",
                "'" + row["debt_asset"] + "'",
                "'" + row["collateral_asset"] + "'",
                "'" + row["debt_to_cover_wei"] + "'",
                "'" + row["liquidated_collateral_wei"] + "'",
                "'" + row["liquidator"] + "'",
                "true" if row["receive_a_token"] else "false",
                str(row["block_number"]),
                "'" + row["reconciled_at"] + "'",
                "'" + row["transcript_sha256"] + "'",
            ]
        )
        statements.append(
            "INSERT INTO {table} ({columns}) VALUES ({values}) ON CONFLICT DO NOTHING;".format(
                table=TABLE,
                columns=", ".join(COLUMNS),
                values=values,
            )
        )
    return "\n".join(statements) + ("\n" if statements else "")


def load_summary(records: list[dict], rows: list[dict]) -> dict:
    settlements = sum(
        1 for record in records if record.get("public_settlement_found") is True
    )
    unsuccessful = sum(
        1
        for record in records
        if record.get("public_settlement_found") is True and record.get("receipt_status") != 1
    )
    without_settlement = sum(
        1 for record in records if record.get("public_settlement_found") is not True
    )
    return {
        "schema": LOAD_SCHEMA,
        "records": len(records),
        "settlements_found": settlements,
        "settlements_unsuccessful_skipped": unsuccessful,
        "records_without_settlement": without_settlement,
        "liquidation_rows_loaded": len(rows),
    }


def atomic_write(path: str, content: str) -> None:
    directory = os.path.dirname(os.path.abspath(path)) or "."
    if not os.path.isdir(directory):
        raise ValidationError("output_dir_invalid")
    fd, temporary = tempfile.mkstemp(prefix=".ground-truth-", dir=directory)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary, 0o644)
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except OSError:
            pass
        raise


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--format", choices=("text", "json"), default="text")
    parser.add_argument("--output-dir", help="write the load summary JSON atomically here")
    args = parser.parse_args()
    try:
        records = parse_ndjson(sys.stdin)
        rows: list[dict] = []
        for record in records:
            rows.extend(record_rows(record))
        sql = render_inserts(rows)
        summary = load_summary(records, rows)
        sys.stdout.write(sql)
        if args.output_dir:
            atomic_write(
                os.path.join(args.output_dir, "phoenix-atlas-liquidation-ground-truth-load.json"),
                json.dumps(summary, sort_keys=True, separators=(",", ":")) + "\n",
            )
        if args.format == "json":
            print(json.dumps(summary, sort_keys=True, separators=(",", ":")), file=sys.stderr)
        else:
            print(
                "records={records} settlements={settlements_found} "
                "unsuccessful_skipped={settlements_unsuccessful_skipped} "
                "without_settlement={records_without_settlement} "
                "rows_loaded={liquidation_rows_loaded}".format(**summary),
                file=sys.stderr,
            )
        return 0
    except ValidationError as error:
        print(f"ground-truth load failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
