#!/usr/bin/env python3
"""Assemble and validate immutable Phoenix release evidence."""

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
INHERITED_PROVENANCE_SCHEMA = "phoenix.release-provenance.v2"
RUN_EVIDENCE_SCHEMA = "phoenix.build-run-evidence.v1"
FRAGMENT_SCHEMA = "phoenix.release-fragment.v1"
INHERITED_FRAGMENT_SCHEMA = "phoenix.release-inherited-fragment.v1"
RELEASE_SCHEMA = "phoenix.release.v1"
INHERITED_RELEASE_SCHEMA = "phoenix.release.v2"
REPOSITORY = "MajidAsghariTabrizi/anti-gravity-phoenix-v4"
WORKFLOW = "Build Phoenix Images"
WORKFLOW_PATH = ".github/workflows/build-images.yml"
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
PROTECTED_IMAGES = ("feed-ingestor", "recorder")
BUILT_IMAGES = tuple(name for name in EXPECTED_IMAGES if name not in PROTECTED_IMAGES)
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


def _validate_sha(value: object, label: str) -> str:
    if not isinstance(value, str) or not SHA_PATTERN.fullmatch(value):
        raise ReleaseProvenanceError(f"{label} is invalid")
    return value


def _validate_run_id(run_id: object, label: str = "build run ID") -> str:
    if not isinstance(run_id, str) or not RUN_ID_PATTERN.fullmatch(run_id):
        raise ReleaseProvenanceError(f"{label} is invalid")
    if run_id in QUARANTINED_RUNS:
        raise ReleaseProvenanceError(
            f"build run {run_id} is quarantined as {QUARANTINED_RUNS[run_id]}"
        )
    return run_id


def _validate_digest(value: object, label: str) -> str:
    if (
        not isinstance(value, str)
        or not DIGEST_PATTERN.fullmatch(value)
        or value == f"sha256:{'0' * 64}"
    ):
        raise ReleaseProvenanceError(f"{label} is invalid")
    return value


def _validate_repository(value: object, name: str, label: str) -> str:
    expected = f"ghcr.io/majidasgharitabrizi/{name}"
    if value != expected:
        raise ReleaseProvenanceError(f"{label} is invalid for {name}")
    return expected


def validate_dispatch(
    release_sha: str,
    release_intent: str,
    confirm_publish: str,
    checked_out_sha: str,
    protected_base_sha: str = "",
    protected_base_build_run_id: str = "",
) -> None:
    _validate_sha(release_sha, "release_sha")
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
    if bool(protected_base_sha) != bool(protected_base_build_run_id):
        raise ReleaseProvenanceError(
            "protected base SHA and build run ID must be supplied together"
        )
    if protected_base_sha:
        _validate_sha(protected_base_sha, "protected_base_sha")
        _validate_run_id(
            protected_base_build_run_id, "protected_base_build_run_id"
        )
        if protected_base_sha == release_sha:
            raise ReleaseProvenanceError("protected base SHA must differ from release_sha")


def _validate_created_at(value: object) -> None:
    if not isinstance(value, str) or not value:
        raise ReleaseProvenanceError("release manifest created_at is invalid")


def _validate_legacy_manifest(
    value: object, expected_sha: str
) -> dict[str, Any]:
    manifest = _require_keys(
        value,
        {"schema", "release_sha", "created_at", "images"},
        "release manifest",
    )
    if manifest["schema"] != RELEASE_SCHEMA or manifest["release_sha"] != expected_sha:
        raise ReleaseProvenanceError("release manifest identity is invalid")
    _validate_created_at(manifest["created_at"])
    images = manifest["images"]
    if not isinstance(images, dict) or tuple(sorted(images)) != EXPECTED_IMAGES:
        raise ReleaseProvenanceError("release manifest image set is invalid")
    for name in EXPECTED_IMAGES:
        image = _require_keys(
            images[name],
            {"repository", "tag", "digest"},
            f"release manifest image {name}",
        )
        _validate_repository(image["repository"], name, "release manifest repository")
        if image["tag"] != f"sha-{expected_sha}":
            raise ReleaseProvenanceError(f"release manifest tag is invalid for {name}")
        _validate_digest(image["digest"], f"release manifest digest for {name}")
    return manifest


def _validate_inherited_manifest(
    value: object,
    expected_sha: str,
    expected_run_id: str | None,
) -> dict[str, Any]:
    manifest = _require_keys(
        value,
        {
            "schema",
            "release_sha",
            "build_run_id",
            "created_at",
            "protected_base_sha",
            "protected_base_build_run_id",
            "images",
        },
        "release manifest",
    )
    if (
        manifest["schema"] != INHERITED_RELEASE_SCHEMA
        or manifest["release_sha"] != expected_sha
    ):
        raise ReleaseProvenanceError("release manifest identity is invalid")
    run_id = _validate_run_id(manifest["build_run_id"])
    if expected_run_id is not None and run_id != expected_run_id:
        raise ReleaseProvenanceError("release manifest build run is invalid")
    base_sha = _validate_sha(manifest["protected_base_sha"], "protected base SHA")
    base_run_id = _validate_run_id(
        manifest["protected_base_build_run_id"], "protected base build run ID"
    )
    if base_sha == expected_sha:
        raise ReleaseProvenanceError("protected base SHA matches release SHA")
    _validate_created_at(manifest["created_at"])
    images = manifest["images"]
    if not isinstance(images, dict) or tuple(sorted(images)) != EXPECTED_IMAGES:
        raise ReleaseProvenanceError("release manifest image set is invalid")
    for name in EXPECTED_IMAGES:
        image = _require_keys(
            images[name],
            {
                "repository",
                "tag",
                "digest",
                "origin",
                "source_sha",
                "source_build_run_id",
                "oci_revision",
            },
            f"release manifest image {name}",
        )
        _validate_repository(image["repository"], name, "release manifest repository")
        _validate_digest(image["digest"], f"release manifest digest for {name}")
        source_sha = _validate_sha(image["source_sha"], f"source SHA for {name}")
        source_run_id = _validate_run_id(
            image["source_build_run_id"], f"source build run ID for {name}"
        )
        if image["tag"] != f"sha-{source_sha}":
            raise ReleaseProvenanceError(f"release manifest tag is invalid for {name}")
        if image["oci_revision"] != source_sha:
            raise ReleaseProvenanceError(f"OCI revision is invalid for {name}")
        if name in PROTECTED_IMAGES:
            if image["origin"] != "inherited":
                raise ReleaseProvenanceError(f"protected image origin is invalid for {name}")
        elif (
            image["origin"] != "built"
            or source_sha != expected_sha
            or source_run_id != run_id
        ):
            raise ReleaseProvenanceError(
                f"non-protected image is not bound to release SHA for {name}"
            )
    # Keep these local variables validated and visible to type checkers.
    _ = base_run_id
    return manifest


def validate_release_manifest(
    value: object,
    expected_sha: str,
    expected_run_id: str | None = None,
) -> dict[str, Any]:
    _validate_sha(expected_sha, "expected release SHA")
    if not isinstance(value, dict):
        raise ReleaseProvenanceError("release manifest contract is invalid")
    schema = value.get("schema")
    if schema == RELEASE_SCHEMA:
        return _validate_legacy_manifest(value, expected_sha)
    if schema == INHERITED_RELEASE_SCHEMA:
        return _validate_inherited_manifest(value, expected_sha, expected_run_id)
    raise ReleaseProvenanceError("release manifest schema is invalid")


# Retain the private name for existing callers and fixtures.
_validate_release_manifest = validate_release_manifest


def _normalized_image_identity(
    manifest: dict[str, Any], name: str, build_run_id: str
) -> dict[str, str]:
    image = manifest["images"][name]
    if manifest["schema"] == RELEASE_SCHEMA:
        return {
            "repository": image["repository"],
            "tag": image["tag"],
            "digest": image["digest"],
            "source_sha": manifest["release_sha"],
            "source_build_run_id": build_run_id,
            "oci_revision": manifest["release_sha"],
        }
    return {
        key: image[key]
        for key in (
            "repository",
            "tag",
            "digest",
            "source_sha",
            "source_build_run_id",
            "oci_revision",
        )
    }


def _validate_fragment_common(
    fragment: dict[str, Any],
    path: Path,
    name: str,
    release_sha: str,
    run_id: str,
    release_intent: str,
) -> None:
    if fragment["release_sha"] != release_sha:
        raise ReleaseProvenanceError(f"mixed release SHA in {path.name}")
    if fragment["build_run_id"] != run_id:
        raise ReleaseProvenanceError(f"mixed build run in {path.name}")
    if fragment["release_intent"] != release_intent:
        raise ReleaseProvenanceError(f"mixed release intent in {path.name}")
    if fragment["name"] != name or path.name != f"{name}.json":
        raise ReleaseProvenanceError(f"release fragment name is invalid in {path.name}")
    _validate_repository(fragment["repository"], name, "repository")
    _validate_digest(fragment["digest"], f"digest in {path.name}")


def _load_fragments(
    directory: Path,
    release_sha: str,
    run_id: str,
    release_intent: str,
    protected_base_sha: str = "",
    protected_base_build_run_id: str = "",
) -> tuple[dict[str, dict[str, str]], dict[str, dict[str, str]]]:
    if directory.is_symlink() or not directory.is_dir():
        raise ReleaseProvenanceError("fragment directory is invalid")
    paths = sorted(directory.glob("*.json"))
    expected_names = [f"{name}.json" for name in EXPECTED_IMAGES]
    if [path.name for path in paths] != expected_names:
        raise ReleaseProvenanceError("release fragment set is incomplete or unexpected")

    inherited = bool(protected_base_sha)
    images: dict[str, dict[str, str]] = {}
    evidence: dict[str, dict[str, str]] = {}
    for path in paths:
        name = path.stem
        if inherited and name in PROTECTED_IMAGES:
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
                    "origin",
                    "protected_base_sha",
                    "protected_base_build_run_id",
                    "source_sha",
                    "source_build_run_id",
                    "oci_revision",
                },
                f"release fragment {path.name}",
            )
            _validate_fragment_common(
                fragment, path, name, release_sha, run_id, release_intent
            )
            if fragment["schema"] != INHERITED_FRAGMENT_SCHEMA:
                raise ReleaseProvenanceError(
                    f"release fragment schema is invalid in {path.name}"
                )
            if (
                fragment["origin"] != "inherited"
                or fragment["protected_base_sha"] != protected_base_sha
                or fragment["protected_base_build_run_id"]
                != protected_base_build_run_id
            ):
                raise ReleaseProvenanceError(
                    f"protected base identity is invalid in {path.name}"
                )
            source_sha = _validate_sha(
                fragment["source_sha"], f"source SHA in {path.name}"
            )
            _validate_run_id(
                fragment["source_build_run_id"],
                f"source build run ID in {path.name}",
            )
            if fragment["tag"] != f"sha-{source_sha}":
                raise ReleaseProvenanceError(f"tag is invalid in {path.name}")
            if fragment["oci_revision"] != source_sha:
                raise ReleaseProvenanceError(
                    f"OCI revision is invalid in {path.name}"
                )
            images[name] = {
                "repository": fragment["repository"],
                "tag": fragment["tag"],
                "digest": fragment["digest"],
                "origin": "inherited",
                "source_sha": source_sha,
                "source_build_run_id": fragment["source_build_run_id"],
                "oci_revision": fragment["oci_revision"],
            }
        else:
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
            _validate_fragment_common(
                fragment, path, name, release_sha, run_id, release_intent
            )
            if fragment["schema"] != FRAGMENT_SCHEMA:
                raise ReleaseProvenanceError(
                    f"release fragment schema is invalid in {path.name}"
                )
            if fragment["tag"] != f"sha-{release_sha}":
                raise ReleaseProvenanceError(f"tag is invalid in {path.name}")
            if inherited:
                images[name] = {
                    "repository": fragment["repository"],
                    "tag": fragment["tag"],
                    "digest": fragment["digest"],
                    "origin": "built",
                    "source_sha": release_sha,
                    "source_build_run_id": run_id,
                    "oci_revision": release_sha,
                }
            else:
                images[name] = {
                    "repository": fragment["repository"],
                    "tag": fragment["tag"],
                    "digest": fragment["digest"],
                }
        evidence[name] = {
            "artifact_name": f"release-fragment-{name}",
            "sha256": _sha256_file(path),
        }
    return images, evidence


def _validate_base_contract(
    manifest_path: Path,
    provenance_path: Path,
    expected_sha: str,
    expected_run_id: str,
) -> tuple[dict[str, Any], dict[str, Any]]:
    manifest = _read_json(manifest_path, "protected base release manifest")
    provenance = _read_json(provenance_path, "protected base release provenance")
    validated = validate_provenance(
        provenance,
        manifest,
        manifest_bytes=manifest_path.read_bytes(),
    )
    if manifest.get("release_sha") != expected_sha:
        raise ReleaseProvenanceError("protected base manifest SHA is invalid")
    if validated["build_run_id"] != expected_run_id:
        raise ReleaseProvenanceError("protected base build run is invalid")
    return manifest, provenance


def write_inherited_fragments(
    output_dir: Path,
    release_sha: str,
    run_id: str,
    release_intent: str,
    protected_base_sha: str,
    protected_base_build_run_id: str,
    protected_base_manifest: Path,
    protected_base_provenance: Path,
) -> None:
    validate_dispatch(
        release_sha,
        release_intent,
        PUBLISH_CONFIRMATION,
        release_sha,
        protected_base_sha,
        protected_base_build_run_id,
    )
    _validate_run_id(run_id)
    base_manifest, _ = _validate_base_contract(
        protected_base_manifest,
        protected_base_provenance,
        protected_base_sha,
        protected_base_build_run_id,
    )
    output_dir.mkdir(parents=True, exist_ok=True)
    for name in PROTECTED_IMAGES:
        identity = _normalized_image_identity(
            base_manifest, name, protected_base_build_run_id
        )
        fragment = {
            "schema": INHERITED_FRAGMENT_SCHEMA,
            "release_sha": release_sha,
            "build_run_id": run_id,
            "release_intent": release_intent,
            "name": name,
            **identity,
            "origin": "inherited",
            "protected_base_sha": protected_base_sha,
            "protected_base_build_run_id": protected_base_build_run_id,
        }
        (output_dir / f"{name}.json").write_bytes(_canonical_json(fragment))


def assemble_release(
    fragments_dir: Path,
    release_assets_dir: Path,
    release_sha: str,
    run_id: str,
    release_intent: str,
    output_manifest: Path,
    output_provenance: Path,
    created_at: str | None = None,
    protected_base_sha: str = "",
    protected_base_build_run_id: str = "",
    protected_base_manifest: Path | None = None,
    protected_base_provenance: Path | None = None,
) -> tuple[dict[str, Any], dict[str, Any]]:
    validate_dispatch(
        release_sha,
        release_intent,
        PUBLISH_CONFIRMATION,
        release_sha,
        protected_base_sha,
        protected_base_build_run_id,
    )
    _validate_run_id(run_id)
    inherited = bool(protected_base_sha)
    if inherited != bool(protected_base_manifest and protected_base_provenance):
        raise ReleaseProvenanceError(
            "protected base manifest and provenance are required together"
        )

    base_manifest: dict[str, Any] | None = None
    if inherited:
        assert protected_base_manifest is not None
        assert protected_base_provenance is not None
        base_manifest, _ = _validate_base_contract(
            protected_base_manifest,
            protected_base_provenance,
            protected_base_sha,
            protected_base_build_run_id,
        )

    images, fragment_evidence = _load_fragments(
        fragments_dir,
        release_sha,
        run_id,
        release_intent,
        protected_base_sha,
        protected_base_build_run_id,
    )

    if base_manifest is not None:
        for name in PROTECTED_IMAGES:
            inherited_identity = {
                key: images[name][key]
                for key in (
                    "repository",
                    "tag",
                    "digest",
                    "source_sha",
                    "source_build_run_id",
                    "oci_revision",
                )
            }
            base_identity = _normalized_image_identity(
                base_manifest, name, protected_base_build_run_id
            )
            if inherited_identity != base_identity:
                raise ReleaseProvenanceError(
                    f"inherited protected image differs from base for {name}"
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
        "schema": INHERITED_RELEASE_SCHEMA if inherited else RELEASE_SCHEMA,
        "release_sha": release_sha,
        "created_at": timestamp,
        "images": images,
    }
    if inherited:
        manifest.update(
            {
                "build_run_id": run_id,
                "protected_base_sha": protected_base_sha,
                "protected_base_build_run_id": protected_base_build_run_id,
            }
        )
    manifest_bytes = _canonical_json(manifest)

    provenance: dict[str, Any] = {
        "schema": INHERITED_PROVENANCE_SCHEMA if inherited else PROVENANCE_SCHEMA,
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
    if inherited:
        assert protected_base_manifest is not None
        assert protected_base_provenance is not None
        provenance.update(
            {
                "protected_base": {
                    "release_sha": protected_base_sha,
                    "build_run_id": protected_base_build_run_id,
                    "release_manifest_sha256": _sha256_file(
                        protected_base_manifest
                    ),
                    "release_provenance_sha256": _sha256_file(
                        protected_base_provenance
                    ),
                },
                "built_images": list(BUILT_IMAGES),
                "inherited_images": list(PROTECTED_IMAGES),
            }
        )
    output_manifest.parent.mkdir(parents=True, exist_ok=True)
    output_provenance.parent.mkdir(parents=True, exist_ok=True)
    output_manifest.write_bytes(manifest_bytes)
    output_provenance.write_bytes(_canonical_json(provenance))
    validate_provenance(provenance, manifest)
    return manifest, provenance


def _validate_common_provenance(
    provenance: dict[str, Any], manifest_value: object, manifest_bytes: bytes | None
) -> tuple[dict[str, Any], str, str]:
    if provenance["repository"] != REPOSITORY or provenance["workflow"] != WORKFLOW:
        raise ReleaseProvenanceError("release provenance workflow identity is invalid")
    release_sha = _validate_sha(provenance["release_sha"], "release provenance SHA")
    if provenance["release_intent"] != RELEASE_INTENT:
        raise ReleaseProvenanceError("release provenance intent is invalid")
    run_id = _validate_run_id(
        provenance["build_run_id"], "release provenance build run ID"
    )
    manifest = validate_release_manifest(manifest_value, release_sha, run_id)

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
        _validate_digest(fragment["sha256"], f"fragment hash for {name}")

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
        _validate_digest(assets[name], f"release assets {name}")
    canonical_manifest = _canonical_json(manifest)
    if manifest_bytes is not None and manifest_bytes != canonical_manifest:
        raise ReleaseProvenanceError("release manifest JSON is not canonical")
    expected_manifest_hash = _sha256_bytes(manifest_bytes or canonical_manifest)
    if provenance["release_manifest_sha256"] != expected_manifest_hash:
        raise ReleaseProvenanceError("release manifest hash is invalid")
    return manifest, release_sha, run_id


def validate_provenance(
    value: object,
    manifest_value: object,
    *,
    manifest_bytes: bytes | None = None,
) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ReleaseProvenanceError("release provenance contract is invalid")
    schema = value.get("schema")
    common_keys = {
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
    }
    if schema == PROVENANCE_SCHEMA:
        provenance = _require_keys(value, common_keys, "release provenance")
    elif schema == INHERITED_PROVENANCE_SCHEMA:
        provenance = _require_keys(
            value,
            common_keys | {"protected_base", "built_images", "inherited_images"},
            "release provenance",
        )
    else:
        raise ReleaseProvenanceError("release provenance schema is invalid")

    manifest, _, _ = _validate_common_provenance(
        provenance, manifest_value, manifest_bytes
    )
    if schema == PROVENANCE_SCHEMA:
        if manifest["schema"] != RELEASE_SCHEMA:
            raise ReleaseProvenanceError("release provenance/manifest schema mismatch")
        return provenance

    if manifest["schema"] != INHERITED_RELEASE_SCHEMA:
        raise ReleaseProvenanceError("release provenance/manifest schema mismatch")
    base = _require_keys(
        provenance["protected_base"],
        {
            "release_sha",
            "build_run_id",
            "release_manifest_sha256",
            "release_provenance_sha256",
        },
        "protected base evidence",
    )
    _validate_sha(base["release_sha"], "protected base evidence SHA")
    _validate_run_id(base["build_run_id"], "protected base evidence build run ID")
    _validate_digest(
        base["release_manifest_sha256"], "protected base manifest hash"
    )
    _validate_digest(
        base["release_provenance_sha256"], "protected base provenance hash"
    )
    if (
        base["release_sha"] != manifest["protected_base_sha"]
        or base["build_run_id"] != manifest["protected_base_build_run_id"]
    ):
        raise ReleaseProvenanceError("protected base evidence identity is invalid")
    if provenance["built_images"] != list(BUILT_IMAGES):
        raise ReleaseProvenanceError("built image contract is invalid")
    if provenance["inherited_images"] != list(PROTECTED_IMAGES):
        raise ReleaseProvenanceError("inherited image contract is invalid")
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
    run_id = _validate_run_id(evidence["run_id"], "build run evidence run ID")
    if run_id != provenance["build_run_id"]:
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
    for name in provenance["required_jobs"]:
        job = by_name.get(name)
        if job is None:
            raise ReleaseProvenanceError(f"required build job is missing: {name}")
        if job["status"] != "completed" or job["conclusion"] != "success":
            raise ReleaseProvenanceError(f"required build job did not succeed: {name}")

    artifacts = evidence["artifacts"]
    if not isinstance(artifacts, list) or not all(
        isinstance(name, str) and name for name in artifacts
    ):
        raise ReleaseProvenanceError("build run artifacts must be an array of names")
    if len(artifacts) != len(set(artifacts)):
        raise ReleaseProvenanceError("build run contains duplicate artifact names")
    for name in provenance["required_release_artifacts"]:
        if name not in artifacts:
            raise ReleaseProvenanceError(f"required release artifact is missing: {name}")


def validate_github_run(
    value: object, expected_sha: str, expected_run_id: str
) -> None:
    _validate_sha(expected_sha, "expected build run SHA")
    _validate_run_id(expected_run_id, "expected build run ID")
    if not isinstance(value, dict):
        raise ReleaseProvenanceError("GitHub build run evidence is invalid")
    repository = value.get("repository")
    if (
        str(value.get("id")) != expected_run_id
        or value.get("name") != WORKFLOW
        or value.get("path") != WORKFLOW_PATH
        or value.get("event") != "workflow_dispatch"
        or value.get("head_sha") != expected_sha
        or value.get("status") != "completed"
        or value.get("conclusion") != "success"
        or not isinstance(repository, dict)
        or repository.get("full_name") != REPOSITORY
    ):
        raise ReleaseProvenanceError("GitHub build run identity or result is invalid")


def validate_deploy_pair(
    candidate_manifest_path: Path,
    candidate_provenance_path: Path,
    candidate_sha: str,
    candidate_run_id: str,
    rollback_manifest_path: Path,
    rollback_provenance_path: Path,
    rollback_sha: str,
    rollback_run_id: str,
) -> None:
    candidate_manifest = _read_json(candidate_manifest_path, "candidate manifest")
    candidate_provenance = _read_json(
        candidate_provenance_path, "candidate provenance"
    )
    rollback_manifest = _read_json(rollback_manifest_path, "rollback manifest")
    rollback_provenance = _read_json(
        rollback_provenance_path, "rollback provenance"
    )
    candidate_validated = validate_provenance(
        candidate_provenance,
        candidate_manifest,
        manifest_bytes=candidate_manifest_path.read_bytes(),
    )
    rollback_validated = validate_provenance(
        rollback_provenance,
        rollback_manifest,
        manifest_bytes=rollback_manifest_path.read_bytes(),
    )
    if (
        candidate_manifest["release_sha"] != candidate_sha
        or candidate_validated["build_run_id"] != candidate_run_id
    ):
        raise ReleaseProvenanceError("candidate release identity is invalid")
    if (
        rollback_manifest["release_sha"] != rollback_sha
        or rollback_validated["build_run_id"] != rollback_run_id
    ):
        raise ReleaseProvenanceError("rollback release identity is invalid")

    if candidate_manifest["schema"] == INHERITED_RELEASE_SCHEMA:
        base = candidate_provenance["protected_base"]
        if (
            candidate_manifest["protected_base_sha"] != rollback_sha
            or candidate_manifest["protected_base_build_run_id"] != rollback_run_id
            or base["release_sha"] != rollback_sha
            or base["build_run_id"] != rollback_run_id
        ):
            raise ReleaseProvenanceError(
                "candidate protected base does not match rollback release"
            )
        if base["release_manifest_sha256"] != _sha256_file(rollback_manifest_path):
            raise ReleaseProvenanceError("rollback manifest hash differs from protected base")
        if base["release_provenance_sha256"] != _sha256_file(
            rollback_provenance_path
        ):
            raise ReleaseProvenanceError(
                "rollback provenance hash differs from protected base"
            )
        for name in PROTECTED_IMAGES:
            candidate_identity = _normalized_image_identity(
                candidate_manifest, name, candidate_run_id
            )
            rollback_identity = _normalized_image_identity(
                rollback_manifest, name, rollback_run_id
            )
            if candidate_identity != rollback_identity:
                raise ReleaseProvenanceError(
                    f"inherited protected image differs from rollback for {name}"
                )
        return

    # Legacy full-build releases used release-specific tags. Preserve their existing
    # deploy contract while still requiring canonical repositories and exact digests.
    for name in PROTECTED_IMAGES:
        candidate_image = candidate_manifest["images"][name]
        rollback_image = rollback_manifest["images"][name]
        if (
            candidate_image["repository"] != rollback_image["repository"]
            or candidate_image["digest"] != rollback_image["digest"]
        ):
            raise ReleaseProvenanceError(
                f"protected image changed for {name}; maintenance is required"
            )


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    dispatch = subparsers.add_parser("validate-dispatch")
    dispatch.add_argument("--release-sha", required=True)
    dispatch.add_argument("--release-intent", required=True)
    dispatch.add_argument("--confirm-publish", required=True)
    dispatch.add_argument("--checked-out-sha", required=True)
    dispatch.add_argument("--protected-base-sha", default="")
    dispatch.add_argument("--protected-base-build-run-id", default="")

    github_run = subparsers.add_parser("validate-github-run")
    github_run.add_argument("--run-evidence", type=Path, required=True)
    github_run.add_argument("--expected-sha", required=True)
    github_run.add_argument("--expected-run-id", required=True)

    inherit = subparsers.add_parser("inherit-protected")
    inherit.add_argument("--output-dir", type=Path, required=True)
    inherit.add_argument("--release-sha", required=True)
    inherit.add_argument("--build-run-id", required=True)
    inherit.add_argument("--release-intent", required=True)
    inherit.add_argument("--protected-base-sha", required=True)
    inherit.add_argument("--protected-base-build-run-id", required=True)
    inherit.add_argument("--protected-base-manifest", type=Path, required=True)
    inherit.add_argument("--protected-base-provenance", type=Path, required=True)

    assemble = subparsers.add_parser("assemble")
    assemble.add_argument("--fragments-dir", type=Path, required=True)
    assemble.add_argument("--release-assets-dir", type=Path, required=True)
    assemble.add_argument("--release-sha", required=True)
    assemble.add_argument("--build-run-id", required=True)
    assemble.add_argument("--release-intent", required=True)
    assemble.add_argument("--output-manifest", type=Path, required=True)
    assemble.add_argument("--output-provenance", type=Path, required=True)
    assemble.add_argument("--protected-base-sha", default="")
    assemble.add_argument("--protected-base-build-run-id", default="")
    assemble.add_argument("--protected-base-manifest", type=Path)
    assemble.add_argument("--protected-base-provenance", type=Path)

    canonical = subparsers.add_parser("validate-canonical")
    canonical.add_argument("--release-manifest", type=Path, required=True)
    canonical.add_argument("--release-provenance", type=Path, required=True)
    canonical.add_argument("--run-evidence", type=Path, required=True)

    deploy = subparsers.add_parser("validate-deploy-pair")
    deploy.add_argument("--release-manifest", type=Path, required=True)
    deploy.add_argument("--release-provenance", type=Path, required=True)
    deploy.add_argument("--release-sha", required=True)
    deploy.add_argument("--build-run-id", required=True)
    deploy.add_argument("--rollback-manifest", type=Path, required=True)
    deploy.add_argument("--rollback-provenance", type=Path, required=True)
    deploy.add_argument("--rollback-sha", required=True)
    deploy.add_argument("--rollback-build-run-id", required=True)
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
                args.protected_base_sha,
                args.protected_base_build_run_id,
            )
        elif args.command == "validate-github-run":
            validate_github_run(
                _read_json(args.run_evidence, "GitHub build run evidence"),
                args.expected_sha,
                args.expected_run_id,
            )
        elif args.command == "inherit-protected":
            write_inherited_fragments(
                args.output_dir,
                args.release_sha,
                args.build_run_id,
                args.release_intent,
                args.protected_base_sha,
                args.protected_base_build_run_id,
                args.protected_base_manifest,
                args.protected_base_provenance,
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
                protected_base_sha=args.protected_base_sha,
                protected_base_build_run_id=args.protected_base_build_run_id,
                protected_base_manifest=args.protected_base_manifest,
                protected_base_provenance=args.protected_base_provenance,
            )
        elif args.command == "validate-canonical":
            manifest = _read_json(args.release_manifest, "release manifest")
            provenance = _read_json(args.release_provenance, "release provenance")
            evidence = _read_json(args.run_evidence, "build run evidence")
            validate_provenance(
                provenance,
                manifest,
                manifest_bytes=args.release_manifest.read_bytes(),
            )
            validate_canonical_run(provenance, manifest, evidence)
        else:
            validate_deploy_pair(
                args.release_manifest,
                args.release_provenance,
                args.release_sha,
                args.build_run_id,
                args.rollback_manifest,
                args.rollback_provenance,
                args.rollback_sha,
                args.rollback_build_run_id,
            )
    except (ReleaseProvenanceError, release_assets.ReleaseAssetError) as exc:
        raise SystemExit(f"RELEASE_PROVENANCE_ERROR: {exc}") from exc
    print("RELEASE_PROVENANCE_OK")


if __name__ == "__main__":
    main()
