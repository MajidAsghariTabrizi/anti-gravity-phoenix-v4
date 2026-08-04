#!/usr/bin/env python3
"""Create and verify the exact-SHA Phoenix root Release Platform manifest."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
import sys
from pathlib import Path, PurePosixPath
from typing import Iterable


SCHEMA = "phoenix.release-platform.v1"
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
DIGEST_RE = re.compile(r"^[0-9a-f]{64}$")
MAX_FILE_BYTES = 8 * 1024 * 1024

# Repository-relative source, absolute installed path, installed mode.
PLATFORM_FILES = (
    ("scripts/phoenix-release-gateway.sh", "/usr/local/sbin/phoenix-release-gateway", 0o755),
    ("scripts/phoenix-release-transport.sh", "/usr/local/sbin/phoenix-release-transport", 0o755),
    ("scripts/activate-economic-canary.sh", "/usr/local/libexec/phoenix-release/activate-economic-canary.sh", 0o644),
    ("scripts/economic_activation_runner.py", "/usr/local/libexec/phoenix-release/economic_activation_runner.py", 0o644),
    ("scripts/deploy-release.sh", "/usr/local/libexec/phoenix-release/deploy-release.sh", 0o644),
    ("scripts/install-production-release-context.sh", "/usr/local/libexec/phoenix-release/install-production-release-context.sh", 0o644),
    ("scripts/install-release-assets.sh", "/usr/local/libexec/phoenix-release/install-release-assets.sh", 0o644),
    ("scripts/prelive-protected-maintenance-launch.sh", "/usr/local/libexec/phoenix-release/prelive-protected-maintenance-launch.sh", 0o644),
    ("scripts/prelive-protected-maintenance-unit.sh", "/usr/local/libexec/phoenix-release/prelive-protected-maintenance-unit.sh", 0o644),
    ("scripts/prelive-protected-maintenance.sh", "/usr/local/libexec/phoenix-release/prelive-protected-maintenance.sh", 0o644),
    ("scripts/prelive_protected_maintenance.py", "/usr/local/libexec/phoenix-release/prelive_protected_maintenance.py", 0o644),
    ("scripts/production-healthcheck.sh", "/usr/local/libexec/phoenix-release/production-healthcheck.sh", 0o644),
    ("scripts/production_compose.py", "/usr/local/libexec/phoenix-release/production_compose.py", 0o644),
    ("scripts/production_context.py", "/usr/local/libexec/phoenix-release/production_context.py", 0o644),
    ("scripts/production_mode.py", "/usr/local/libexec/phoenix-release/production_mode.py", 0o644),
    ("scripts/rehearse-production-release.sh", "/usr/local/libexec/phoenix-release/rehearse-production-release.sh", 0o644),
    ("scripts/release_assets.py", "/usr/local/libexec/phoenix-release/release_assets.py", 0o644),
    ("scripts/release_components.py", "/usr/local/libexec/phoenix-release/release_components.py", 0o644),
    ("scripts/release_platform.py", "/usr/local/libexec/phoenix-release/release_platform.py", 0o644),
    ("scripts/release_provenance.py", "/usr/local/libexec/phoenix-release/release_provenance.py", 0o644),
    ("scripts/render-production-compose.sh", "/usr/local/libexec/phoenix-release/render-production-compose.sh", 0o644),
    ("scripts/rollback-release.sh", "/usr/local/libexec/phoenix-release/rollback-release.sh", 0o644),
    ("scripts/validate-production-env.sh", "/usr/local/libexec/phoenix-release/validate-production-env.sh", 0o644),
    ("scripts/validate-production-release-context.sh", "/usr/local/libexec/phoenix-release/validate-production-release-context.sh", 0o644),
    ("scripts/phoenix_release/__init__.py", "/usr/local/libexec/phoenix-release/phoenix_release/__init__.py", 0o644),
    ("scripts/phoenix_release/chain_reconciliation.py", "/usr/local/libexec/phoenix-release/phoenix_release/chain_reconciliation.py", 0o644),
    ("scripts/phoenix_release/cli.py", "/usr/local/libexec/phoenix-release/phoenix_release/cli.py", 0o644),
    ("scripts/phoenix_release/controller.py", "/usr/local/libexec/phoenix-release/phoenix_release/controller.py", 0o644),
    ("scripts/phoenix_release/gateway.py", "/usr/local/libexec/phoenix-release/phoenix_release/gateway.py", 0o644),
    ("scripts/phoenix_release/model.py", "/usr/local/libexec/phoenix-release/phoenix_release/model.py", 0o644),
    ("scripts/phoenix_release/phase_update.py", "/usr/local/libexec/phoenix-release/phoenix_release/phase_update.py", 0o644),
    ("scripts/phoenix_release/rpc_provider_secret.py", "/usr/local/libexec/phoenix-release/phoenix_release/rpc_provider_secret.py", 0o644),
    ("deploy/phoenix-economic-activation.path", "/etc/systemd/system/phoenix-economic-activation.path", 0o644),
    ("deploy/phoenix-economic-activation.service", "/etc/systemd/system/phoenix-economic-activation.service", 0o644),
    ("release-components.json", "/usr/local/libexec/phoenix-release/release-components.json", 0o644),
)
MANIFEST_PATH = "/usr/local/libexec/phoenix-release/platform-manifest.json"


class PlatformError(ValueError):
    """Release Platform identity or metadata is invalid."""


def _digest(path: Path) -> str:
    if not path.is_file() or path.is_symlink():
        raise PlatformError(f"platform file is missing or unsafe: {path}")
    metadata = path.stat()
    if metadata.st_size > MAX_FILE_BYTES:
        raise PlatformError(f"platform file is oversized: {path}")
    return hashlib.sha256(path.read_bytes()).hexdigest()


def create_manifest(source_root: Path, release_sha: str) -> dict[str, object]:
    if not SHA_RE.fullmatch(release_sha):
        raise PlatformError("release SHA is invalid")
    source_root = source_root.resolve(strict=True)
    files = []
    for source, installed, mode in PLATFORM_FILES:
        source_path = source_root / PurePosixPath(source)
        files.append(
            {
                "installed_path": installed,
                "mode": f"{mode:04o}",
                "sha256": _digest(source_path),
                "source_path": source,
            }
        )
    return {
        "files": files,
        "release_sha": release_sha,
        "schema": SCHEMA,
    }


def _canonical(value: object) -> str:
    return json.dumps(value, indent=2, sort_keys=True) + "\n"


def _installed(root: Path, absolute: str) -> Path:
    relative = PurePosixPath(absolute).relative_to("/")
    return root.joinpath(*relative.parts)


def validate_manifest(value: object) -> dict[str, object]:
    if (
        not isinstance(value, dict)
        or set(value) != {"files", "release_sha", "schema"}
        or value["schema"] != SCHEMA
        or not isinstance(value["release_sha"], str)
        or not SHA_RE.fullmatch(value["release_sha"])
        or not isinstance(value["files"], list)
        or len(value["files"]) != len(PLATFORM_FILES)
    ):
        raise PlatformError("platform manifest contract is invalid")
    expected = [(source, installed, f"{mode:04o}") for source, installed, mode in PLATFORM_FILES]
    observed = []
    for item in value["files"]:
        if (
            not isinstance(item, dict)
            or set(item) != {"installed_path", "mode", "sha256", "source_path"}
            or not isinstance(item["sha256"], str)
            or not DIGEST_RE.fullmatch(item["sha256"])
        ):
            raise PlatformError("platform manifest entry is invalid")
        observed.append((item["source_path"], item["installed_path"], item["mode"]))
    if observed != expected:
        raise PlatformError("platform manifest file set is invalid")
    return value


def verify_installed(root: Path, expected_sha: str) -> dict[str, object]:
    if not SHA_RE.fullmatch(expected_sha):
        raise PlatformError("expected release SHA is invalid")
    manifest_path = _installed(root, MANIFEST_PATH)
    if not manifest_path.is_file() or manifest_path.is_symlink():
        raise PlatformError("installed platform manifest is missing or unsafe")
    raw = manifest_path.read_text(encoding="utf-8")
    try:
        value = validate_manifest(json.loads(raw))
    except (UnicodeError, json.JSONDecodeError) as exc:
        raise PlatformError("installed platform manifest is invalid") from exc
    if raw != _canonical(value) or value["release_sha"] != expected_sha:
        raise PlatformError("installed platform SHA does not match candidate")
    for item in value["files"]:
        path = _installed(root, str(item["installed_path"]))
        metadata = path.lstat()
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_nlink != 1
            or _digest(path) != item["sha256"]
        ):
            raise PlatformError(
                f"installed platform file does not match manifest: {item['installed_path']}"
            )
        if os.name == "posix":
            if stat.S_IMODE(metadata.st_mode) != int(str(item["mode"]), 8):
                raise PlatformError(
                    f"installed platform file mode is invalid: {item['installed_path']}"
                )
            if metadata.st_uid != 0 or metadata.st_gid != 0:
                raise PlatformError(
                    f"installed platform file is not root-owned: {item['installed_path']}"
                )
    return value


def _write(path: Path, value: object) -> None:
    path.write_text(_canonical(value), encoding="utf-8")


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    create = commands.add_parser("create")
    create.add_argument("--source-root", required=True, type=Path)
    create.add_argument("--release-sha", required=True)
    create.add_argument("--output", required=True, type=Path)
    verify = commands.add_parser("verify")
    verify.add_argument("--installed-root", default=Path("/"), type=Path)
    verify.add_argument("--expected-sha", required=True)
    return parser


def main(argv: Iterable[str] | None = None) -> int:
    arguments = _parser().parse_args(argv)
    try:
        if arguments.command == "create":
            _write(
                arguments.output,
                create_manifest(arguments.source_root, arguments.release_sha),
            )
            result = {
                "release_sha": arguments.release_sha,
                "status": "created",
            }
        else:
            value = verify_installed(
                arguments.installed_root, arguments.expected_sha
            )
            result = {
                "release_sha": value["release_sha"],
                "status": "verified",
            }
        print(json.dumps(result, sort_keys=True, separators=(",", ":")))
        return 0
    except (OSError, PlatformError) as exc:
        print(
            json.dumps(
                {"error": str(exc), "status": "error"},
                sort_keys=True,
                separators=(",", ":"),
            ),
            file=sys.stderr,
        )
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
