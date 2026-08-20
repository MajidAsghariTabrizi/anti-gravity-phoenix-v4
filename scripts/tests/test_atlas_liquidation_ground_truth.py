"""Tests for the read-only Atlas liquidation ground-truth loader."""

import io
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS = REPO_ROOT / "scripts"
sys.path.insert(0, str(SCRIPTS))

import atlas_liquidation_ground_truth as loader  # noqa: E402


def canonical_line(
    settled: bool = True,
    receipt_status: int = 1,
    extra_liquidation: bool = True,
) -> dict:
    liquidations = [
        {
            "collateral_asset": "0x912ce59144191c1204e64559fe8253a0e49e6548",
            "debt_asset": "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
            "borrower": "0x1111111111111111111111111111111111111111",
            "debt_to_cover": "50000000",
            "liquidated_collateral_amount": "55000000",
            "liquidator": "0x2222222222222222222222222222222222222222",
            "receive_a_token": True,
            "log_index": "a",
        }
    ]
    if extra_liquidation:
        liquidations.append(
            {
                "collateral_asset": "0x912ce59144191c1204e64559fe8253a0e49e6548",
                "debt_asset": "0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9",
                "borrower": "0x3333333333333333333333333333333333333333",
                "debt_to_cover": "7000000",
                "liquidated_collateral_amount": "8000000",
                "liquidator": "0x4444444444444444444444444444444444444444",
                "receive_a_token": False,
                "log_index": "0",
            }
        )
    return {
        "schema": "phoenix.atlas-reconciliation.v1",
        "reconciled_at": "2026-08-20T22:00:00Z",
        "auction_id": "auction-1",
        "user_operation_hash": "0x" + "a" * 64,
        "transcript_sha256": "b" * 64,
        "public_settlement_found": settled,
        "onchain_transaction": "0x" + "c" * 64,
        "transaction_block": 491300000 if settled else None,
        "receipt_status": receipt_status if settled else None,
        "public_liquidations": liquidations if settled else None,
    }


def ndjson(*lines: dict) -> str:
    return "\n".join(json.dumps(line, sort_keys=True) for line in lines) + "\n"


def parse(payload: str) -> list[dict]:
    return loader.parse_ndjson(io.StringIO(payload))


def rows_of(payload: str) -> list[dict]:
    rows: list[dict] = []
    for record in parse(payload):
        rows.extend(loader.record_rows(record))
    return rows


def run_loader(payload: str, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(SCRIPTS / "atlas_liquidation_ground_truth.py"), *args],
        input=payload.encode("utf-8"),
        capture_output=True,
        timeout=60,
    )


class AtlasLiquidationGroundTruthTests(unittest.TestCase):
    def test_canonical_records_produce_rows_and_inserts(self) -> None:
        payload = ndjson(canonical_line(), canonical_line(settled=False))
        records = parse(payload)
        rows = rows_of(payload)
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0]["log_index"], 10)
        self.assertEqual(rows[1]["log_index"], 0)
        self.assertEqual(rows[1]["debt_asset"], "0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9")
        sql = loader.render_inserts(rows)
        self.assertEqual(sql.count("INSERT INTO"), 2)
        self.assertIn("ON CONFLICT DO NOTHING", sql)
        self.assertNotIn("--", sql)
        summary = loader.load_summary(records, rows)
        self.assertEqual(summary["settlements_found"], 1)
        self.assertEqual(summary["records_without_settlement"], 1)
        self.assertEqual(summary["liquidation_rows_loaded"], 2)

    def test_unsuccessful_settlement_is_skipped(self) -> None:
        payload = ndjson(canonical_line(receipt_status=0))
        records = parse(payload)
        rows = rows_of(payload)
        self.assertEqual(rows, [])
        self.assertEqual(loader.load_summary(records, rows)["settlements_unsuccessful_skipped"], 1)

    def test_stdin_end_to_end_exit_zero(self) -> None:
        completed = run_loader(ndjson(canonical_line()))
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn(b"INSERT INTO live_canary.atlas_liquidation_ground_truth", completed.stdout)
        self.assertIn(b"rows_loaded=2", completed.stderr)

    def test_malformed_address_is_rejected(self) -> None:
        line = canonical_line(extra_liquidation=False)
        line["public_liquidations"][0]["borrower"] = "0xBAD"
        completed = run_loader(ndjson(line))
        self.assertEqual(completed.returncode, 1)
        self.assertIn(b"borrower_invalid", completed.stderr)

    def test_wrong_reconciliation_schema_is_rejected(self) -> None:
        line = canonical_line(extra_liquidation=False)
        line["schema"] = "phoenix.other.v1"
        completed = run_loader(ndjson(line))
        self.assertEqual(completed.returncode, 1)
        self.assertIn(b"reconciliation_schema_invalid", completed.stderr)

    def test_invalid_log_index_is_rejected(self) -> None:
        line = canonical_line(extra_liquidation=False)
        line["public_liquidations"][0]["log_index"] = "0xZZ"
        completed = run_loader(ndjson(line))
        self.assertEqual(completed.returncode, 1)
        self.assertIn(b"log_index_invalid", completed.stderr)

    def test_invalid_transcript_hash_is_rejected(self) -> None:
        line = canonical_line(extra_liquidation=False)
        line["transcript_sha256"] = "not-a-hash"
        completed = run_loader(ndjson(line))
        self.assertEqual(completed.returncode, 1)
        self.assertIn(b"transcript_sha256_invalid", completed.stderr)

    def test_output_dir_writes_atomic_summary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            completed = run_loader(
                ndjson(canonical_line()), "--format", "json", "--output-dir", directory
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            artifact = Path(directory) / "phoenix-atlas-liquidation-ground-truth-load.json"
            self.assertTrue(artifact.is_file())
            summary = json.loads(artifact.read_bytes())
            self.assertEqual(summary["schema"], loader.LOAD_SCHEMA)
            self.assertEqual(summary["liquidation_rows_loaded"], 2)
            self.assertEqual(
                len([p for p in Path(directory).iterdir() if p.name.endswith(".tmp")]), 0
            )

    def test_missing_output_dir_fails(self) -> None:
        completed = run_loader(
            ndjson(canonical_line()), "--output-dir", "/nonexistent-phoenix-dir"
        )
        self.assertEqual(completed.returncode, 1)

    def test_render_is_deterministic(self) -> None:
        payload = ndjson(canonical_line())
        self.assertEqual(loader.render_inserts(rows_of(payload)), loader.render_inserts(rows_of(payload)))


if __name__ == "__main__":
    unittest.main()
