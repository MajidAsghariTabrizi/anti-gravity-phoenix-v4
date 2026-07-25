import os
import tempfile
import unittest
from pathlib import Path

from scripts import production_mode


class ProductionModeTests(unittest.TestCase):
    def setUp(self) -> None:
        if os.name == "nt":
            self.skipTest("production mode metadata is Linux-specific")
        self.temporary = tempfile.TemporaryDirectory()
        self.env_file = Path(self.temporary.name) / "phoenix.env"
        self.env_file.write_text(
            "# retained\n"
            "PHOENIX_MODE=SHADOW\n"
            "LIVE_EXECUTION=false\n"
            "AUTONOMOUS_EXECUTION=false\n"
            "SECRET_VALUE=not-printed\n",
            encoding="utf-8",
        )
        os.chmod(self.env_file, 0o600)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_live_and_shadow_transitions_preserve_unrelated_values(self) -> None:
        production_mode.update(self.env_file, "live")
        live = self.env_file.read_text(encoding="utf-8")
        self.assertIn("PHOENIX_MODE=LIVE\n", live)
        self.assertIn("LIVE_EXECUTION=true\n", live)
        self.assertIn("AUTONOMOUS_EXECUTION=true\n", live)
        self.assertIn("SECRET_VALUE=not-printed\n", live)
        self.assertEqual(self.env_file.stat().st_mode & 0o777, 0o600)

        production_mode.update(self.env_file, "shadow")
        shadow = self.env_file.read_text(encoding="utf-8")
        self.assertIn("PHOENIX_MODE=SHADOW\n", shadow)
        self.assertIn("LIVE_EXECUTION=false\n", shadow)
        self.assertIn("AUTONOMOUS_EXECUTION=false\n", shadow)

    def test_duplicate_mode_key_fails_closed_without_replacement(self) -> None:
        original = (
            "PHOENIX_MODE=SHADOW\n"
            "PHOENIX_MODE=LIVE\n"
            "LIVE_EXECUTION=false\n"
            "AUTONOMOUS_EXECUTION=false\n"
        )
        self.env_file.write_text(original, encoding="utf-8")
        with self.assertRaisesRegex(production_mode.ModeError, "duplicate"):
            production_mode.update(self.env_file, "live")
        self.assertEqual(self.env_file.read_text(encoding="utf-8"), original)

    def test_symlink_is_rejected(self) -> None:
        link = Path(self.temporary.name) / "link.env"
        try:
            link.symlink_to(self.env_file)
        except OSError:
            self.skipTest("symlink creation unavailable")
        with self.assertRaisesRegex(production_mode.ModeError, "unsafe"):
            production_mode.update(link, "live")


if __name__ == "__main__":
    unittest.main()
