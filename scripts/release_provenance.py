#!/usr/bin/env python3
"""Assemble and validate one-run immutable Phoenix release evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

try:
    from scripts import release_assets
except (ImportError, ModuleNotFoundError):  # Direct execution from scripts/.
    import release_assets  # type: ignore[no-redef]


PROVENANCE_SCHEMA = "phoenix.release-provenance.v1"
RUN_EVIDENCE_SCHEMA = "phoenix.build-run-evidence.v1"
FRAGMENT_SCHEMA = "phoenix.release-fragment.v1"
RELEASE_SCHEMA = "phoenix.release.v1"
REPOSITORY = "MajidAsghariTabrizi/anti-gravity-phoenix-v4"
WORKFLOW = "Build Phoenix Images"
RELEASE_INTENT = "PHOENIX_PRELIVE_SHADOW_V5"
PUBLISH_CONFIRMATION = "PUBLISH_IMMUTABLE_PHOENIX_IMAGES"
QUARANTINED_RUNS = {
    "29683234024": "NON_CANONICAL_INCOMPLETE_BUILD",
}
EXPECTED_IMAGES = (
    "dashboard",
    "feed-ingestor",
    "fork-sandbox",
    "live-executor",
    "phoenix-engine",
    "recorder",
    "rpc-gateway",
)
EXPECTED_JOBS = (
    "publication-preflight",
    *(f"build-{name}" for name in EXPECTED_IMAGES),
    "release-assets",
    "release-manifest",
)
SHA_PATTERN = re.compile(r"^[0-9a-f]{40}$")
DIGEST_PATTERN = re.compile(r"^sha256:[0-9a-f]{64}$")
RUN_ID_PATTERN = re.compile(r"^[1-9][0-9]{0,19}$")
MAX_JSON_BYTES = 2 * 1024 * 1024


class ReleaseProvenanceError(ValueError):
    pass


def _sha256_bytes(value: bytes) -> str:
    return f"sha256:{hashlib.sha256(value).hexdigest()}"


def _sha256_file(path: Path) -> str:
    return _sha256_bytes(path.read_bytes())


def _canonical_json(value: object) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode("utf-8")


def _require_keys(value: object, expected: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != expected:
        raise ReleaseProvenanceError(f"{label} contract is invalid")
    return value


def _read_json(path: Path, label: str) -> dict[str, Any]:
    if path.is_symlink() or not path.is_file():
        raise ReleaseProvenanceError(f"{label} must be a regular file")
    if path.stat().st_size > MAX_JSON_BYTES:
        raise ReleaseProvenanceError(f"{label} exceeds the size limit")
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise ReleaseProvenanceError(f"{label} is not valid UTF-8 JSON") from exc
    if not isinstance(value, dict):
        raise ReleaseProvenanceError(f"{label} must be a JSON object")
    return value


def _release_artifact_names(release_sha: str) -> tuple[str, ...]:
    return (
        *(f"release-fragment-{name}" for name in EXPECTED_IMAGES),
        f"phoenix-release-assets-{release_sha}",
        f"phoenix-release-manifest-{release_sha}",
    )


def validate_dispatch(
    release_sha: str,
    release_intent: str,
    confirm_publish: str,
    checked_out_sha: str,
) -> None:
    if not SHA_PATTERN.fullmatch(release_sha):
        raise ReleaseProvenanceError(
            "release_sha must be exactly 40 lowercase hexadecimal characters"
        )
    if release_intent != RELEASE_INTENT:
        raise ReleaseProvenanceError(
            f"release_intent must be exactly {RELEASE_INTENT}"
        )
    if confirm_publish != PUBLISH_CONFIRMATION:
        raise ReleaseProvenanceError(
            f"confirm_publish must be exactly {PUBLISH_CONFIRMATION}"
        )
    if checked_out_sha != release_sha:
        raise ReleaseProvenanceError("checked-out SHA does not match release_sha")


def _validate_run_id(run_id: str) -> None:
    if not RUN_ID_PATTERN.fullmatch(run_id):
        raise ReleaseProvenanceError("build run ID is invalid")
    if run_id in QUARANTINED_RUNS:
        raise ReleaseProvenanceError(
            f"build run {run_id} is quarantined as {QUARANTINED_RUNS[run_id]}"
        )


def _validate_release_manifest(
    value: object, expected_sha: str
) -> dict[str, Any]:
    manifest = _require_keys(
        value,
        {"schema", "release_sha", "created_at", "images"},
        "release manifest",
    )
    if manifest["schema"] != RELEASE_SCHEMA or manifest["release_sha"] != expected_sha:
        raise ReleaseProvenanceError("release manifest identity is invalid")
    if not isinstance(manifest["created_at"], str) or not manifest["created_at"]:
        raise ReleaseProvenanceError("release manifest created_at is invalid")
    images = manifest["images"]
    if not isinstance(images, dict) or tuple(sorted(images)) != EXPECTED_IMAGES:
        raise ReleaseProvenanceError("release manifest image set is invalid")
    for name in EXPECTED_IMAGES:
        image = _require_keys(
            images[name],
            {"repository", "tag", "digest"},
            f"release manifest image {name}",
        )
        if image["repository"] != f"ghcr.io/majidasgharitabrizi/{name}":
            raise ReleaseProvenanceError(f"release manifest repository is invalid for {name}")
        if image["tag"] != f"sha-{expected_sha}":
            raise ReleaseProvenanceError(f"release manifest tag is invalid for {name}")
        if (
            not isinstance(image["digest"], str)
            or not DIGEST_PATTERN.fullmatch(image["digest"])
            or image["digest"] == f"sha256:{'0' * 64}"
        ):
            raise ReleaseProvenanceError(f"release manifest digest is invalid for {name}")
    return manifest


def _load_fragments(
    directory: Path,
    release_sha: str,
    run_id: str,
    release_intent: str,
) -> tuple[dict[str, dict[str, str]], dict[str, dict[str, str]]]:
    if directory.is_symlink() or not directory.is_dir():
        raise ReleaseProvenanceError("fragment directory is invalid")
    paths = sorted(directory.glob("*.json"))
    expected_names = [f"{name}.json" for name in EXPECTED_IMAGES]
    if [path.name for path in paths] != expected_names:
        raise ReleaseProvenanceError("release fragment set is incomplete or unexpected")

    images: dict[str, dict[str, str]] = {}
    evidence: dict[str, dict[str, str]] = {}
    for path in paths:
        fragment = _require_keys(
            _read_json(path, f"release fragment {path.name}"),
            {
                "schema",
                "release_sha",
                "build_run_id",
                "release_intent",
                "name",
                "repository",
                "tag",
                "digest",
            },
            f"release fragment {path.name}",
        )
        name = fragment["name"]
        if name not in EXPECTED_IMAGES or path.name != f"{name}.json":
            raise ReleaseProvenanceError(f"release fragment name is invalid in {path.name}")
        if fragment["schema"] != FRAGMENT_SCHEMA:
            raise ReleaseProvenanceError(f"release fragment schema is invalid in {path.name}")
        if fragment["release_sha"] != release_sha:
            raise ReleaseProvenanceError(f"mixed release SHA in {path.name}")
        if (
            not isinstance(fragment["build_run_id"], str)
            or fragment["build_run_id"] != run_id
        ):
            raise ReleaseProvenanceError(f"mixed build run in {path.name}")
        if fragment["release_intent"] != release_intent:
            raise ReleaseProvenanceError(f"mixed release intent in {path.name}")
        if fragment["repository"] != f"ghcr.io/majidasgharitabrizi/{name}":
            raise ReleaseProvenanceError(f"repository is invalid in {path.name}")
        if fragment["tag"] != f"sha-{release_sha}":
            raise ReleaseProvenanceError(f"tag is invalid in {path.name}")
        digest = fragment["digest"]
        if (
            not isinstance(digest, str)
            or not DIGEST_PATTERN.fullmatch(digest)
            or digest == f"sha256:{'0' * 64}"
        ):
            raise ReleaseProvenanceError(f"digest is invalid in {path.name}")
        images[name] = {
            "repository": fragment["repository"],
            "tag": fragment["tag"],
            "digest": digest,
        }
        evidence[name] = {
            "artifact_name": f"release-fragment-{name}",
            "sha256": _sha256_file(path),
        }
    return images, evidence


def assemble_release(
    fragments_dir: Path,
    release_assets_dir: Path,
    release_sha: str,
    run_id: str,
    release_intent: str,
    output_manifest: Path,
    output_provenance: Path,
    created_at: str | None = None,
) -> tuple[dict[str, Any], dict[str, Any]]:
    validate_dispatch(
        release_sha,
        release_intent,
        PUBLISH_CONFIRMATION,
        release_sha,
    )
    _validate_run_id(run_id)
    images, fragment_evidence = _load_fragments(
        fragments_dir, release_sha, run_id, release_intent
    )

    archive = release_assets_dir / f"phoenix-release-assets-{release_sha}.tar.gz"
    assets_manifest = release_assets_dir / release_assets.MANIFEST_NAME
    checksums = release_assets_dir / "release-assets-checksums.txt"
    release_assets.verify_release_assets(
        archive, assets_manifest, checksums, release_sha
    )

    timestamp = created_at or datetime.now(timezone.utc).replace(
        microsecond=0
    ).isoformat().replace("+00:00", "Z")
    manifest: dict[str, Any] = {
        "schema": RELEASE_SCHEMA,
        "release_sha": release_sha,
        "created_at": timestamp,
        "images": images,
    }
    manifest_bytes = _canonical_json(manifest)
    provenance: dict[str, Any] = {
        "schema": PROVENANCE_SCHEMA,
        "repository": REPOSITORY,
        "workflow": WORKFLOW,
        "release_sha": release_sha,
        "release_intent": release_intent,
        "build_run_id": run_id,
        "quarantine": {
            "classification": "NON_CANONICAL_INCOMPLETE_BUILD",
            "run_ids": sorted(QUARANTINED_RUNS),
        },
        "required_jobs": list(EXPECTED_JOBS),
        "required_release_artifacts": list(_release_artifact_names(release_sha)),
        "image_fragments": fragment_evidence,
        "release_assets": {
            "artifact_name": f"phoenix-release-assets-{release_sha}",
            "archive_name": archive.name,
            "archive_sha256": _sha256_file(archive),
            "manifest_sha256": _sha256_file(assets_manifest),
            "checksums_sha256": _sha256_file(checksums),
        },
        "release_manifest_sha256": _sha256_bytes(manifest_bytes),
    }
    output_manifest.parent.mkdir(parents=True, exist_ok=True)
    output_provenance.parent.mkdir(parents=True, exist_ok=True)
    output_manifest.write_bytes(manifest_bytes)
    output_provenance.write_bytes(_canonical_json(provenance))
    validate_provenance(provenance, manifest)
    return manifest, provenance


def validate_provenance(
    value: object,
    manifest_value: object,
    *,
    manifest_bytes: bytes | None = None,
) -> dict[str, Any]:
    provenance = _require_keys(
        value,
        {
            "schema",
            "repository",
            "workflow",
            "release_sha",
            "release_intent",
            "build_run_id",
            "quarantine",
            "required_jobs",
            "required_release_artifacts",
            "image_fragments",
            "release_assets",
            "release_manifest_sha256",
        },
        "release provenance",
    )
    if provenance["schema"] != PROVENANCE_SCHEMA:
        raise ReleaseProvenanceError("release provenance schema is invalid")
    if provenance["repository"] != REPOSITORY or provenance["workflow"] != WORKFLOW:
        raise ReleaseProvenanceError("release provenance workflow identity is invalid")
    release_sha = provenance["release_sha"]
    if not isinstance(release_sha, str) or not SHA_PATTERN.fullmatch(release_sha):
        raise ReleaseProvenanceError("release provenance SHA is invalid")
    if provenance["release_intent"] != RELEASE_INTENT:
        raise ReleaseProvenanceError("release provenance intent is invalid")
    if not isinstance(provenance["build_run_id"], str):
        raise ReleaseProvenanceError("release provenance build run ID is invalid")
    run_id = provenance["build_run_id"]
    _validate_run_id(run_id)
    manifest = _validate_release_manifest(manifest_value, release_sha)

    quarantine = _require_keys(
        provenance["quarantine"], {"classification", "run_ids"}, "quarantine"
    )
    if quarantine != {
        "classification": "NON_CANONICAL_INCOMPLETE_BUILD",
        "run_ids": sorted(QUARANTINED_RUNS),
    }:
        raise ReleaseProvenanceError("quarantine contract is invalid")
    if provenance["required_jobs"] != list(EXPECTED_JOBS):
        raise ReleaseProvenanceError("required build job contract is invalid")
    if provenance["required_release_artifacts"] != list(
        _release_artifact_names(release_sha)
    ):
        raise ReleaseProvenanceError("required release artifact contract is invalid")

    fragments = provenance["image_fragments"]
    if not isinstance(fragments, dict) or tuple(sorted(fragments)) != EXPECTED_IMAGES:
        raise ReleaseProvenanceError("image fragment evidence is invalid")
    for name in EXPECTED_IMAGES:
        fragment = _require_keys(
            fragments[name], {"artifact_name", "sha256"}, f"fragment evidence {name}"
        )
        if fragment["artifact_name"] != f"release-fragment-{name}":
            raise ReleaseProvenanceError(f"fragment artifact name is invalid for {name}")
        if not isinstance(fragment["sha256"], str) or not DIGEST_PATTERN.fullmatch(
            fragment["sha256"]
        ):
            raise ReleaseProvenanceError(f"fragment hash is invalid for {name}")

    assets = _require_keys(
        provenance["release_assets"],
        {
            "artifact_name",
            "archive_name",
            "archive_sha256",
            "manifest_sha256",
            "checksums_sha256",
        },
        "release assets evidence",
    )
    if assets["artifact_name"] != f"phoenix-release-assets-{release_sha}":
        raise ReleaseProvenanceError("release assets artifact identity is invalid")
    if assets["archive_name"] != f"phoenix-release-assets-{release_sha}.tar.gz":
        raise ReleaseProvenanceError("release assets archive identity is invalid")
    for name in ("archive_sha256", "manifest_sha256", "checksums_sha256"):
        if not isinstance(assets[name], str) or not DIGEST_PATTERN.fullmatch(
            assets[name]
        ):
            raise ReleaseProvenanceError(f"release assets {name} is invalid")
    canonical_manifest = _canonical_json(manifest)
    if manifest_bytes is not None and manifest_bytes != canonical_manifest:
        raise ReleaseProvenanceError("release manifest JSON is not canonical")
    expected_manifest_hash = _sha256_bytes(manifest_bytes or canonical_manifest)
    if provenance["release_manifest_sha256"] != expected_manifest_hash:
        raise ReleaseProvenanceError("release manifest hash is invalid")
    return provenance


def validate_canonical_run(
    provenance_value: object,
    manifest_value: object,
    run_evidence_value: object,
) -> None:
    provenance = validate_provenance(provenance_value, manifest_value)
    evidence = _require_keys(
        run_evidence_value,
        {
            "schema",
            "repository",
            "workflow",
            "event",
            "run_id",
            "head_sha",
            "release_intent",
            "status",
            "conclusion",
            "jobs",
            "artifacts",
        },
        "build run evidence",
    )
    if evidence["schema"] != RUN_EVIDENCE_SCHEMA:
        raise ReleaseProvenanceError("build run evidence schema is invalid")
    if evidence["repository"] != REPOSITORY or evidence["workflow"] != WORKFLOW:
        raise ReleaseProvenanceError("build run evidence workflow identity is invalid")
    if not isinstance(evidence["run_id"], str):
        raise ReleaseProvenanceError("build run evidence run ID is invalid")
    run_id = evidence["run_id"]
    _validate_run_id(run_id)
    if run_id != str(provenance["build_run_id"]):
        raise ReleaseProvenanceError("build run evidence uses a mixed run")
    if evidence["head_sha"] != provenance["release_sha"]:
        raise ReleaseProvenanceError("build run evidence uses a mixed SHA")
    if evidence["release_intent"] != provenance["release_intent"]:
        raise ReleaseProvenanceError("build run evidence uses a mixed release intent")
    if evidence["event"] != "workflow_dispatch":
        raise ReleaseProvenanceError("canonical builds require workflow_dispatch")
    if evidence["status"] != "completed" or evidence["conclusion"] != "success":
        raise ReleaseProvenanceError("build run did not complete successfully")

    jobs = evidence["jobs"]
    if not isinstance(jobs, list):
        raise ReleaseProvenanceError("build run jobs must be an array")
    by_name: dict[str, dict[str, Any]] = {}
    for raw_job in jobs:
        job = _require_keys(raw_job, {"name", "status", "conclusion"}, "build job")
        name = job["name"]
        if not isinstance(name, str) or name in by_name:
            raise ReleaseProvenanceError("build run contains a duplicate or invalid job")
        by_name[name] = job
    for name in EXPECTED_JOBS:
        job = by_name.get(name)
        if job is None:
            raise ReleaseProvenanceError(f"required build job is missing: {name}")
        if job["status"] != "completed" or job["conclusion"] != "success":
            raise ReleaseProvenanceError(
                f"required build job did not succeed: {name}"
            )

    artifacts = evidence["artifacts"]
    if not isinstance(artifacts, list) or not all(
        isinstance(name, str) and name for name in artifacts
    ):
        raise ReleaseProvenanceError("build run artifacts must be an array of names")
    if len(artifacts) != len(set(artifacts)):
        raise ReleaseProvenanceError("build run contains duplicate artifact names")
    for name in _release_artifact_names(provenance["release_sha"]):
        if name not in artifacts:
            raise ReleaseProvenanceError(f"required release artifact is missing: {name}")


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    dispatch = subparsers.add_parser("validate-dispatch")
    dispatch.add_argument("--release-sha", required=True)
    dispatch.add_argument("--release-intent", required=True)
    dispatch.add_argument("--confirm-publish", required=True)
    dispatch.add_argument("--checked-out-sha", required=True)

    assemble = subparsers.add_parser("assemble")
    assemble.add_argument("--fragments-dir", type=Path, required=True)
    assemble.add_argument("--release-assets-dir", type=Path, required=True)
    assemble.add_argument("--release-sha", required=True)
    assemble.add_argument("--build-run-id", required=True)
    assemble.add_argument("--release-intent", required=True)
    assemble.add_argument("--output-manifest", type=Path, required=True)
    assemble.add_argument("--output-provenance", type=Path, required=True)

    canonical = subparsers.add_parser("validate-canonical")
    canonical.add_argument("--release-manifest", type=Path, required=True)
    canonical.add_argument("--release-provenance", type=Path, required=True)
    canonical.add_argument("--run-evidence", type=Path, required=True)
    return parser


def main() -> None:
    args = _parser().parse_args()
    try:
        if args.command == "validate-dispatch":
            validate_dispatch(
                args.release_sha,
                args.release_intent,
                args.confirm_publish,
                args.checked_out_sha,
            )
        elif args.command == "assemble":
            assemble_release(
                args.fragments_dir,
                args.release_assets_dir,
                args.release_sha,
                args.build_run_id,
                args.release_intent,
                args.output_manifest,
                args.output_provenance,
            )
        else:
            manifest = _read_json(args.release_manifest, "release manifest")
            provenance = _read_json(args.release_provenance, "release provenance")
            evidence = _read_json(args.run_evidence, "build run evidence")
            validate_provenance(
                provenance,
                manifest,
                manifest_bytes=args.release_manifest.read_bytes(),
            )
            validate_canonical_run(provenance, manifest, evidence)
    except (ReleaseProvenanceError, release_assets.ReleaseAssetError) as exc:
        raise SystemExit(f"RELEASE_PROVENANCE_ERROR: {exc}") from exc
    print("RELEASE_PROVENANCE_OK")


if __name__ == "__main__":
    main()
