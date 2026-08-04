#!/usr/bin/env python3
"""Install and validate the rpc-gateway credential without exposing it."""

from __future__ import annotations

import argparse
import hmac
import os
import stat
import sys
import tempfile
from pathlib import Path
from typing import BinaryIO


APPROVED_DIRECTORY = Path("/etc/phoenix/secrets")
APPROVED_PATH = APPROVED_DIRECTORY / "phoenix-rpc-provider-slot-1-api-key"
EXPECTED_UID = 0
EXPECTED_GID = 65532
EXPECTED_DIRECTORY_MODE = 0o750
EXPECTED_FILE_MODE = 0o640
MAX_SECRET_BYTES = 4096


class SecretError(RuntimeError):
    """A bounded provider-secret contract violation."""


def read_secret_once(stream: BinaryIO) -> bytes | None:
    """Read at most one protected credential from a one-shot stream."""

    value = stream.read(MAX_SECRET_BYTES + 1)
    if len(value) > MAX_SECRET_BYTES:
        raise SecretError("rpc_provider_secret_too_large")
    if not value:
        return None
    if any(byte < 0x21 or byte > 0x7E for byte in value):
        raise SecretError("rpc_provider_secret_invalid")
    return value


def _validate_metadata(
    metadata: os.stat_result,
    *,
    expected_uid: int,
    expected_gid: int,
    expected_mode: int,
) -> None:
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_nlink != 1
        or metadata.st_uid != expected_uid
        or metadata.st_gid != expected_gid
        or stat.S_IMODE(metadata.st_mode) != expected_mode
    ):
        raise SecretError("rpc_provider_secret_metadata_invalid")


def read_existing_secret(
    path: Path,
    *,
    expected_uid: int = EXPECTED_UID,
    expected_gid: int = EXPECTED_GID,
    expected_mode: int = EXPECTED_FILE_MODE,
) -> bytes | None:
    """Read a verified regular file without following symlinks."""

    try:
        before = path.lstat()
    except FileNotFoundError:
        return None
    _validate_metadata(
        before,
        expected_uid=expected_uid,
        expected_gid=expected_gid,
        expected_mode=expected_mode,
    )
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as exc:
        raise SecretError("rpc_provider_secret_open_failed") from exc
    try:
        opened = os.fstat(descriptor)
        _validate_metadata(
            opened,
            expected_uid=expected_uid,
            expected_gid=expected_gid,
            expected_mode=expected_mode,
        )
        if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
            raise SecretError("rpc_provider_secret_identity_changed")
        value = os.read(descriptor, MAX_SECRET_BYTES + 1)
        if os.read(descriptor, 1):
            raise SecretError("rpc_provider_secret_too_large")
    finally:
        os.close(descriptor)
    if len(value) > MAX_SECRET_BYTES:
        raise SecretError("rpc_provider_secret_too_large")
    if not value:
        raise SecretError("rpc_provider_secret_empty")
    if any(byte < 0x21 or byte > 0x7E for byte in value):
        raise SecretError("rpc_provider_secret_invalid")
    return value


def persist_secret(
    value: bytes,
    path: Path,
    *,
    expected_uid: int = EXPECTED_UID,
    expected_gid: int = EXPECTED_GID,
    expected_mode: int = EXPECTED_FILE_MODE,
) -> None:
    """Persist a credential atomically on the target filesystem."""

    if not value or len(value) > MAX_SECRET_BYTES:
        raise SecretError("rpc_provider_secret_invalid")
    if any(byte < 0x21 or byte > 0x7E for byte in value):
        raise SecretError("rpc_provider_secret_invalid")
    try:
        path.lstat()
    except FileNotFoundError:
        pass
    else:
        raise SecretError("rpc_provider_secret_unexpected_existing")

    descriptor, temporary_name = tempfile.mkstemp(
        prefix=".rpc-provider-slot-1.", dir=path.parent
    )
    temporary = Path(temporary_name)
    try:
        os.fchmod(descriptor, 0o600)
        offset = 0
        while offset < len(value):
            offset += os.write(descriptor, value[offset:])
        os.fchown(descriptor, expected_uid, expected_gid)
        os.fchmod(descriptor, expected_mode)
        os.fsync(descriptor)
        _validate_metadata(
            os.fstat(descriptor),
            expected_uid=expected_uid,
            expected_gid=expected_gid,
            expected_mode=expected_mode,
        )
        os.close(descriptor)
        descriptor = -1

        try:
            path.lstat()
        except FileNotFoundError:
            pass
        else:
            raise SecretError("rpc_provider_secret_unexpected_existing")
        os.replace(temporary, path)
        directory_descriptor = os.open(
            path.parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
        )
        try:
            os.fsync(directory_descriptor)
        finally:
            os.close(directory_descriptor)
        observed = read_existing_secret(
            path,
            expected_uid=expected_uid,
            expected_gid=expected_gid,
            expected_mode=expected_mode,
        )
        if observed is None or not hmac.compare_digest(observed, value):
            raise SecretError("rpc_provider_secret_persist_mismatch")
    finally:
        if descriptor >= 0:
            os.close(descriptor)
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def _validate_approved_directory() -> None:
    try:
        metadata = APPROVED_DIRECTORY.lstat()
    except FileNotFoundError as exc:
        raise SecretError("rpc_provider_secret_directory_missing") from exc
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or metadata.st_uid != EXPECTED_UID
        or metadata.st_gid != EXPECTED_GID
        or stat.S_IMODE(metadata.st_mode) != EXPECTED_DIRECTORY_MODE
    ):
        raise SecretError("rpc_provider_secret_directory_invalid")


def install_from_stream(
    stream: BinaryIO,
    path: Path,
    *,
    expected_uid: int = EXPECTED_UID,
    expected_gid: int = EXPECTED_GID,
    expected_mode: int = EXPECTED_FILE_MODE,
) -> None:
    if read_existing_secret(
        path,
        expected_uid=expected_uid,
        expected_gid=expected_gid,
        expected_mode=expected_mode,
    ) is not None:
        raise SecretError("rpc_provider_secret_unexpected_existing")
    value = read_secret_once(stream)
    if value is None:
        raise SecretError("rpc_provider_secret_missing")
    persist_secret(
        value,
        path,
        expected_uid=expected_uid,
        expected_gid=expected_gid,
        expected_mode=expected_mode,
    )


def install_from_stdin(stream: BinaryIO) -> None:
    _validate_approved_directory()
    install_from_stream(stream, APPROVED_PATH)


def reuse_existing() -> None:
    _validate_approved_directory()
    if read_existing_secret(APPROVED_PATH) is None:
        raise SecretError("rpc_provider_secret_missing")


def legacy_recovery_stream(
    stream: BinaryIO,
    path: Path,
    *,
    expected_uid: int = EXPECTED_UID,
    expected_gid: int = EXPECTED_GID,
    expected_mode: int = EXPECTED_FILE_MODE,
) -> None:
    existing = read_existing_secret(
        path,
        expected_uid=expected_uid,
        expected_gid=expected_gid,
        expected_mode=expected_mode,
    )
    supplied = read_secret_once(stream)
    if supplied is None:
        if existing is None:
            raise SecretError("rpc_provider_secret_missing")
        return
    if existing is None:
        persist_secret(
            supplied,
            path,
            expected_uid=expected_uid,
            expected_gid=expected_gid,
            expected_mode=expected_mode,
        )
        return
    if not hmac.compare_digest(existing, supplied):
        raise SecretError("rpc_provider_secret_mismatch")


def legacy_recovery(stream: BinaryIO) -> None:
    """Bridge the one active pre-fix gateway through this recovery release."""

    _validate_approved_directory()
    legacy_recovery_stream(stream, APPROVED_PATH)


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser(prog="rpc-provider-secret")
    value.add_argument(
        "operation", choices=("install-stdin", "reuse", "legacy-recovery")
    )
    return value


def main(argv: list[str] | None = None) -> int:
    arguments = parser().parse_args(argv)
    try:
        if os.geteuid() != 0:
            raise SecretError("root_required")
        if arguments.operation == "install-stdin":
            install_from_stdin(sys.stdin.buffer)
        elif arguments.operation == "reuse":
            reuse_existing()
        else:
            legacy_recovery(sys.stdin.buffer)
        return 0
    except SecretError as exc:
        print(f"PHOENIX_RPC_PROVIDER_SECRET_FAILED: {exc}", file=sys.stderr)
        return 1
    except Exception:
        print(
            "PHOENIX_RPC_PROVIDER_SECRET_FAILED: internal_error", file=sys.stderr
        )
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
