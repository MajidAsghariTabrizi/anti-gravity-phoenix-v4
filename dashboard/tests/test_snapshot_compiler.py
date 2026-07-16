from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from dashboard.snapshot_model import load_snapshot


ROOT = Path(__file__).resolve().parents[2]
FIXTURE_ROOT = ROOT / "fixtures" / "dashboard"
COMPILER = ROOT / "scripts" / "prelive_dashboard_snapshot.py"


class SnapshotCompilerTests(unittest.TestCase):
    def make_evidence_dir(self) -> Path:
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        shutil.copyfile(FIXTURE_ROOT / "latest-dashboard.json", root / "candidate.json")
        shutil.copyfile(FIXTURE_ROOT / "technical.json", root / "technical.json")
        shutil.copyfile(FIXTURE_ROOT / "business.json", root / "business.json")
        return root

    def run_compiler(
        self, source: Path, output: Path, *extra: str
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(COMPILER),
                "--input",
                str(source),
                "--output",
                str(output),
                *extra,
            ],
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
            timeout=15,
        )

    def test_promotes_valid_snapshot_atomically(self) -> None:
        root = self.make_evidence_dir()
        output = root / "latest-dashboard.json"
        result = self.run_compiler(root / "candidate.json", output)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("DASHBOARD_SNAPSHOT_OK", result.stdout)
        self.assertEqual(
            load_snapshot(output).data["schema_version"], "phoenix.prelive.dashboard.v1"
        )
        self.assertEqual(list(root.glob(".dashboard-snapshot-*.tmp")), [])

    def test_check_mode_does_not_write(self) -> None:
        root = self.make_evidence_dir()
        output = root / "latest-dashboard.json"
        result = self.run_compiler(root / "candidate.json", output, "--check")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(output.exists())
        self.assertIn("action=validated", result.stdout)

    def test_rejects_artifact_integrity_failure(self) -> None:
        root = self.make_evidence_dir()
        (root / "technical.json").write_text("changed", encoding="utf-8")
        output = root / "latest-dashboard.json"
        result = self.run_compiler(root / "candidate.json", output)
        self.assertEqual(result.returncode, 2)
        self.assertIn("artifact_size_mismatch", result.stderr)
        self.assertFalse(output.exists())

    def test_rejects_cross_directory_output(self) -> None:
        root = self.make_evidence_dir()
        other = tempfile.TemporaryDirectory()
        self.addCleanup(other.cleanup)
        output = Path(other.name) / "latest-dashboard.json"
        result = self.run_compiler(root / "candidate.json", output)
        self.assertEqual(result.returncode, 2)
        self.assertIn("snapshot_output_path_invalid", result.stderr)
        self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
