#!/usr/bin/env python3
"""Build and verify the immutable Phoenix release-assets bundle."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import os
import re
import stat
import sys
import tarfile
import tempfile
from pathlib import Path, PurePosixPath


SCHEMA = "phoenix.release-assets.v1"
SHA_PATTERN = re.compile(r"^[0-9a-f]{40}$")
DIGEST_PATTERN = re.compile(r"^sha256:[0-9a-f]{64}$")
MODE_PATTERN = re.compile(r"^(0644|0755)$")
MAX_FILES = 512
MAX_FILE_BYTES = 8 * 1024 * 1024
MAX_TOTAL_BYTES = 64 * 1024 * 1024
MAX_ARCHIVE_BYTES = 72 * 1024 * 1024

STATIC_PATHS = (
    "compose.live-canary.yml",
    "compose.prod.yml",
    "release-components.json",
    "dashboard/snapshot_model.py",
    "deploy/nats-server.conf",
    "deploy/prelive-v5-release.example.json",
    "fixtures/routes/arbitrum_uniswap_v3_pool_proofs.json",
    "live-executor/schema/001_live_canary.sql",
    "live-executor/schema/002_approval_evidence.sql",
    "prometheus/prometheus.yml",
    "scripts/bootstrap-production.sh",
    "scripts/deploy-release.sh",
    "scripts/install-production-release-context.sh",
    "scripts/install-release-assets.sh",
    "scripts/install-shadow-deploy-gateway.sh",
    "scripts/phoenix-shadow-deploy-gateway.sh",
    "scripts/phoenix_shadow_deploy.py",
    "scripts/prelive-money-path-report.sh",
    "scripts/prelive-v5-fresh-database-gate.sh",
    "scripts/prelive-protected-maintenance-launch.sh",
    "scripts/prelive-protected-maintenance-unit.sh",
    "scripts/prelive-protected-maintenance.sh",
    "scripts/prelive-shadow-control.sh",
    "scripts/prelive_dashboard_live.py",
    "scripts/prelive_dashboard_snapshot.py",
    "scripts/prelive_money_path_report.py",
    "scripts/prelive_protected_maintenance.py",
    "scripts/prelive_shadow_control.py",
    "scripts/prelive_v5_release.py",
    "scripts/production-healthcheck.sh",
    "scripts/production_context.py",
    "scripts/provision-production-host.sh",
    "scripts/release_assets.py",
    "scripts/release_components.py",
    "scripts/release_provenance.py",
    "scripts/render-production-compose.sh",
    "scripts/rollback-release.sh",
    "scripts/shadow-engine-isolated-canary.sh",
    "scripts/shadow-positive-route-evidence.sh",
    "scripts/shadow-profitability-report.sh",
    "scripts/shadow-route-discovery.sh",
    "scripts/shadow_profitability_report.py",
    "scripts/shadow_route_discovery.py",
    "scripts/validate-production-env.sh",
    "scripts/validate-production-release-context.sh",
    "scripts/verify-compose-route-registry.py",
    "scripts/verify_dashboard_compose.py",
)
GLOB_PATHS = (
    "migrations/*.sql",
    "schemas/*.json",
    "scripts/sql/*.sql",
)
CONTRACT_TARGET = "contracts/PhoenixExecutor.compiled.json"
MANIFEST_NAME = "release-assets-manifest.json"


class ReleaseAssetError(ValueError):
    pass


def _sha256(data: bytes) -> str:
    return f"sha256:{hashlib.sha256(data).hexdigest()}"


def _canonical_json(value: object) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode("utf-8")


def _validate_release_sha(value: str) -> str:
    if not SHA_PATTERN.fullmatch(value):
        raise ReleaseAssetError("release SHA must be 40 lowercase hex characters")
    return value


def _validate_relative_path(value: str) -> str:
    if (
        not value
        or len(value) > 255
        or "\\" in value
        or "\x00" in value
        or "//" in value
        or value.endswith("/")
    ):
        raise ReleaseAssetError("release asset path is invalid")
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in ("", ".", "..") for part in path.parts):
        raise ReleaseAssetError("release asset path is invalid")
    lowered = value.lower()
    if "__pycache__" in (part.lower() for part in path.parts) or lowered.endswith(
        (".pyc", ".pyo")
    ):
        raise ReleaseAssetError("release asset path is generated Python bytecode")
    sensitive_names = (
        ".env",
        ".pem",
        ".key",
        ".p12",
        ".pfx",
        "id_rsa",
        "id_ed25519",
        "credentials",
        "secrets",
    )
    if any(token in lowered for token in sensitive_names):
        raise ReleaseAssetError("release asset path is sensitive")
    return value


def _mode_for(path: str) -> str:
    if path.startswith("scripts/") and (path.endswith(".sh") or path.endswith(".py")):
        return "0755"
    return "0644"


def _atomic_write(path: Path, data: bytes, mode: int = 0o644) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary, mode)
        os.replace(temporary, path)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def _read_bounded(path: Path, maximum: int = MAX_FILE_BYTES) -> bytes:
    if not path.is_file() or path.is_symlink():
        raise ReleaseAssetError(f"release asset is missing or not a regular file: {path}")
    size = path.stat().st_size
    if size > maximum:
        raise ReleaseAssetError(f"release asset exceeds {maximum} bytes: {path}")
    try:
        return path.read_bytes()
    except OSError as exc:
        raise ReleaseAssetError(f"release asset could not be read: {path}") from exc


def _collect_sources(repo_root: Path, contract_artifact: Path) -> dict[str, bytes]:
    sources: dict[str, Path] = {}
    for relative in STATIC_PATHS:
        sources[_validate_relative_path(relative)] = repo_root / relative
    for pattern in GLOB_PATHS:
        matches = sorted(repo_root.glob(pattern))
        if not matches:
            raise ReleaseAssetError(f"release asset pattern has no matches: {pattern}")
        for source in matches:
            relative = source.relative_to(repo_root).as_posix()
            sources[_validate_relative_path(relative)] = source
    sources[CONTRACT_TARGET] = contract_artifact

    if len(sources) > MAX_FILES:
        raise ReleaseAssetError("release asset count exceeds the configured bound")
    payloads: dict[str, bytes] = {}
    total = 0
    for relative, source in sorted(sources.items()):
        payload = _read_bounded(source)
        total += len(payload)
        if total > MAX_TOTAL_BYTES:
            raise ReleaseAssetError("release asset bytes exceed the configured bound")
        payloads[relative] = payload
    return payloads


def _manifest(release_sha: str, payloads: dict[str, bytes]) -> dict[str, object]:
    return {
        "schema": SCHEMA,
        "release_sha": release_sha,
        "files": [
            {
                "mode": _mode_for(path),
                "path": path,
                "sha256": _sha256(payload),
                "size_bytes": len(payload),
            }
            for path, payload in sorted(payloads.items())
        ],
    }


def _tar_bytes(root_name: str, manifest_bytes: bytes, payloads: dict[str, bytes]) -> bytes:
    raw = io.BytesIO()
    with gzip.GzipFile(filename="", mode="wb", fileobj=raw, compresslevel=9, mtime=0) as gz:
        with tarfile.open(fileobj=gz, mode="w", format=tarfile.USTAR_FORMAT) as archive:
            entries = {MANIFEST_NAME: (manifest_bytes, "0644")}
            entries.update((path, (payload, _mode_for(path))) for path, payload in payloads.items())
            for relative, (payload, mode) in sorted(entries.items()):
                info = tarfile.TarInfo(f"{root_name}/{relative}")
                info.size = len(payload)
                info.mode = int(mode, 8)
                info.mtime = 0
                info.uid = 0
                info.gid = 0
                info.uname = ""
                info.gname = ""
                archive.addfile(info, io.BytesIO(payload))
    return raw.getvalue()


def build_release_assets(
    repo_root: Path,
    release_sha: str,
    output_dir: Path,
    contract_artifact: Path,
) -> tuple[Path, Path, Path]:
    release_sha = _validate_release_sha(release_sha)
    repo_root = repo_root.resolve(strict=True)
    contract_artifact = contract_artifact.resolve(strict=True)
    payloads = _collect_sources(repo_root, contract_artifact)
    manifest_bytes = _canonical_json(_manifest(release_sha, payloads))
    root_name = f"phoenix-release-{release_sha}"
    archive_name = f"phoenix-release-assets-{release_sha}.tar.gz"
    archive_bytes = _tar_bytes(root_name, manifest_bytes, payloads)

    output_dir.mkdir(parents=True, exist_ok=True)
    archive_path = output_dir / archive_name
    manifest_path = output_dir / MANIFEST_NAME
    checksum_path = output_dir / "release-assets-checksums.txt"
    checksum_bytes = (
        f"{hashlib.sha256(archive_bytes).hexdigest()}  {archive_name}\n"
        f"{hashlib.sha256(manifest_bytes).hexdigest()}  {MANIFEST_NAME}\n"
    ).encode("ascii")
    _atomic_write(archive_path, archive_bytes)
    _atomic_write(manifest_path, manifest_bytes)
    _atomic_write(checksum_path, checksum_bytes)
    return archive_path, manifest_path, checksum_path


def _load_manifest(path: Path, expected_sha: str) -> tuple[bytes, dict[str, dict[str, object]]]:
    raw = _read_bounded(path)
    try:
        value = json.loads(raw)
    except (UnicodeError, json.JSONDecodeError) as exc:
        raise ReleaseAssetError("release-assets manifest is invalid JSON") from exc
    if not isinstance(value, dict) or set(value) != {"schema", "release_sha", "files"}:
        raise ReleaseAssetError("release-assets manifest contract is invalid")
    if value["schema"] != SCHEMA or value["release_sha"] != expected_sha:
        raise ReleaseAssetError("release-assets manifest identity is invalid")
    files = value["files"]
    if not isinstance(files, list) or not files or len(files) > MAX_FILES:
        raise ReleaseAssetError("release-assets manifest file list is invalid")

    indexed: dict[str, dict[str, object]] = {}
    previous = ""
    total = 0
    for item in files:
        if not isinstance(item, dict) or set(item) != {"mode", "path", "sha256", "size_bytes"}:
            raise ReleaseAssetError("release-assets manifest entry is invalid")
        path = item["path"]
        mode = item["mode"]
        digest = item["sha256"]
        size = item["size_bytes"]
        if not isinstance(path, str):
            raise ReleaseAssetError("release-assets manifest path is invalid")
        _validate_relative_path(path)
        if path <= previous or path in indexed:
            raise ReleaseAssetError("release-assets manifest paths are not unique and sorted")
        if not isinstance(mode, str) or not MODE_PATTERN.fullmatch(mode):
            raise ReleaseAssetError("release-assets manifest mode is invalid")
        if mode != _mode_for(path):
            raise ReleaseAssetError("release-assets manifest mode does not match policy")
        if not isinstance(digest, str) or not DIGEST_PATTERN.fullmatch(digest):
            raise ReleaseAssetError("release-assets manifest digest is invalid")
        if not isinstance(size, int) or isinstance(size, bool) or not 0 <= size <= MAX_FILE_BYTES:
            raise ReleaseAssetError("release-assets manifest size is invalid")
        total += size
        if total > MAX_TOTAL_BYTES:
            raise ReleaseAssetError("release-assets manifest exceeds the configured byte bound")
        indexed[path] = item
        previous = path
    canonical = _canonical_json(value)
    if raw != canonical:
        raise ReleaseAssetError("release-assets manifest is not canonical")
    return canonical, indexed


def _verify_checksums(checksum_path: Path, archive_path: Path, manifest_path: Path) -> None:
    raw = _read_bounded(checksum_path)
    try:
        lines = raw.decode("ascii").splitlines()
    except UnicodeError as exc:
        raise ReleaseAssetError("release-assets checksum file is invalid") from exc
    expected_names = (archive_path.name, manifest_path.name)
    if len(lines) != len(expected_names):
        raise ReleaseAssetError("release-assets checksum file is invalid")
    expected_paths = (archive_path, manifest_path)
    for line, expected_name, expected_path in zip(
        lines, expected_names, expected_paths, strict=True
    ):
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9._-]+)", line)
        if match is None or match.group(2) != expected_name:
            raise ReleaseAssetError("release-assets checksum file is invalid")
        maximum = MAX_ARCHIVE_BYTES if expected_path == archive_path else MAX_FILE_BYTES
        if match.group(1) != hashlib.sha256(_read_bounded(expected_path, maximum)).hexdigest():
            raise ReleaseAssetError("release-assets checksum mismatch")


def verify_release_assets(
    archive_path: Path,
    manifest_path: Path,
    checksum_path: Path,
    expected_sha: str,
) -> None:
    expected_sha = _validate_release_sha(expected_sha)
    _verify_checksums(checksum_path, archive_path, manifest_path)
    manifest_bytes, files = _load_manifest(manifest_path, expected_sha)
    root_name = f"phoenix-release-{expected_sha}"
    expected_members = {f"{root_name}/{MANIFEST_NAME}"}
    expected_members.update(f"{root_name}/{path}" for path in files)

    seen: set[str] = set()
    total = 0
    try:
        with tarfile.open(archive_path, mode="r:gz") as archive:
            members = archive.getmembers()
            if len(members) != len(expected_members):
                raise ReleaseAssetError("release-assets archive member set is invalid")
            for member in members:
                name = member.name
                if name not in expected_members or name in seen or not member.isfile():
                    raise ReleaseAssetError("release-assets archive member is invalid")
                seen.add(name)
                relative = name.removeprefix(f"{root_name}/")
                _validate_relative_path(relative)
                if member.uid != 0 or member.gid != 0 or member.mtime != 0:
                    raise ReleaseAssetError("release-assets archive metadata is not normalized")
                extracted = archive.extractfile(member)
                if extracted is None:
                    raise ReleaseAssetError("release-assets archive member is unreadable")
                payload = extracted.read(MAX_FILE_BYTES + 1)
                if len(payload) > MAX_FILE_BYTES:
                    raise ReleaseAssetError("release-assets archive member exceeds the byte bound")
                total += len(payload)
                if total > MAX_TOTAL_BYTES:
                    raise ReleaseAssetError("release-assets archive exceeds the byte bound")
                if relative == MANIFEST_NAME:
                    if payload != manifest_bytes or member.mode != 0o644:
                        raise ReleaseAssetError("release-assets internal manifest mismatch")
                    continue
                item = files[relative]
                if member.mode != int(str(item["mode"]), 8):
                    raise ReleaseAssetError("release-assets archive mode mismatch")
                if len(payload) != item["size_bytes"] or _sha256(payload) != item["sha256"]:
                    raise ReleaseAssetError("release-assets archive payload mismatch")
    except (OSError, tarfile.TarError) as exc:
        raise ReleaseAssetError("release-assets archive is invalid") from exc
    if seen != expected_members:
        raise ReleaseAssetError("release-assets archive is incomplete")


def verify_release_tree(root: Path, manifest_path: Path, expected_sha: str) -> None:
    expected_sha = _validate_release_sha(expected_sha)
    manifest_bytes, files = _load_manifest(manifest_path, expected_sha)
    if root.is_symlink():
        raise ReleaseAssetError("release-assets tree root is invalid")
    root = root.resolve(strict=True)
    if not root.is_dir():
        raise ReleaseAssetError("release-assets tree root is invalid")

    observed: dict[str, Path] = {}
    for candidate in root.rglob("*"):
        if candidate.is_symlink():
            raise ReleaseAssetError("release-assets tree contains a symbolic link")
        if candidate.is_dir():
            continue
        if not candidate.is_file():
            raise ReleaseAssetError("release-assets tree contains a non-file entry")
        relative = _validate_relative_path(candidate.relative_to(root).as_posix())
        observed[relative] = candidate

    expected = set(files) | {MANIFEST_NAME}
    if set(observed) != expected:
        raise ReleaseAssetError("release-assets tree member set is invalid")
    if _read_bounded(observed[MANIFEST_NAME]) != manifest_bytes:
        raise ReleaseAssetError("release-assets tree manifest mismatch")
    if os.name == "posix" and stat.S_IMODE(observed[MANIFEST_NAME].stat().st_mode) != 0o644:
        raise ReleaseAssetError("release-assets tree manifest mode mismatch")
    for relative, item in files.items():
        payload = _read_bounded(observed[relative])
        if len(payload) != item["size_bytes"] or _sha256(payload) != item["sha256"]:
            raise ReleaseAssetError("release-assets tree payload mismatch")
        if os.name == "posix" and stat.S_IMODE(observed[relative].stat().st_mode) != int(
            str(item["mode"]), 8
        ):
            raise ReleaseAssetError("release-assets tree mode mismatch")


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="command", required=True)

    build = subcommands.add_parser("build")
    build.add_argument("--repo-root", required=True, type=Path)
    build.add_argument("--release-sha", required=True)
    build.add_argument("--output-dir", required=True, type=Path)
    build.add_argument("--contract-artifact", required=True, type=Path)

    verify = subcommands.add_parser("verify")
    verify.add_argument("--archive", required=True, type=Path)
    verify.add_argument("--manifest", required=True, type=Path)
    verify.add_argument("--checksums", required=True, type=Path)
    verify.add_argument("--expected-sha", required=True)

    verify_tree = subcommands.add_parser("verify-tree")
    verify_tree.add_argument("--root", required=True, type=Path)
    verify_tree.add_argument("--manifest", required=True, type=Path)
    verify_tree.add_argument("--expected-sha", required=True)
    return parser


def main() -> None:
    args = _parser().parse_args()
    try:
        if args.command == "build":
            archive, manifest, checksums = build_release_assets(
                args.repo_root, args.release_sha, args.output_dir, args.contract_artifact
            )
            print(
                json.dumps(
                    {
                        "archive": archive.name,
                        "checksums": checksums.name,
                        "manifest": manifest.name,
                        "status": "ok",
                    },
                    sort_keys=True,
                    separators=(",", ":"),
                )
            )
        elif args.command == "verify":
            verify_release_assets(args.archive, args.manifest, args.checksums, args.expected_sha)
            print('{"status":"ok"}')
        else:
            verify_release_tree(args.root, args.manifest, args.expected_sha)
            print('{"status":"ok"}')
    except (OSError, ReleaseAssetError) as exc:
        print(json.dumps({"error": str(exc), "status": "error"}, sort_keys=True), file=sys.stderr)
        raise SystemExit(1) from None


if __name__ == "__main__":
    main()
