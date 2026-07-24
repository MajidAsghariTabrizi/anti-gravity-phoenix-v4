#!/usr/bin/env python3
import argparse
import os
import stat
import tempfile
from pathlib import Path


class ModeError(Exception):
    pass


VALUES = {
    "live": {
        "PHOENIX_MODE": "LIVE",
        "LIVE_EXECUTION": "true",
        "AUTONOMOUS_EXECUTION": "true",
    },
    "shadow": {
        "PHOENIX_MODE": "SHADOW",
        "LIVE_EXECUTION": "false",
        "AUTONOMOUS_EXECUTION": "false",
    },
}


def update(path: Path, mode: str) -> None:
    if not path.is_absolute() or not path.is_file() or path.is_symlink():
        raise ModeError("production environment file is unsafe")
    metadata = path.stat()
    if metadata.st_nlink != 1 or stat.S_IMODE(metadata.st_mode) != 0o600:
        raise ModeError("production environment metadata is unsafe")
    raw = path.read_text(encoding="utf-8")
    if "\x00" in raw:
        raise ModeError("production environment content is invalid")
    replacements = VALUES[mode]
    seen: set[str] = set()
    output: list[str] = []
    for line in raw.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#") or "=" not in stripped:
            output.append(line)
            continue
        name = stripped.split("=", 1)[0].strip()
        if name in replacements:
            if name in seen:
                raise ModeError(f"duplicate production mode key: {name}")
            output.append(f"{name}={replacements[name]}")
            seen.add(name)
        else:
            output.append(line)
    for name, value in replacements.items():
        if name not in seen:
            output.append(f"{name}={value}")
    content = "\n".join(output) + "\n"
    file_descriptor, temporary_name = tempfile.mkstemp(
        prefix=".phoenix-mode.", dir=path.parent
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(file_descriptor, "w", encoding="utf-8", newline="\n") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary, 0o600)
        os.chown(temporary, metadata.st_uid, metadata.st_gid)
        os.replace(temporary, path)
    finally:
        if temporary.exists():
            temporary.unlink()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=tuple(VALUES))
    parser.add_argument("--env-file", required=True)
    args = parser.parse_args()
    try:
        update(Path(args.env_file), args.mode)
    except (ModeError, OSError, UnicodeError) as error:
        raise SystemExit(f"PRODUCTION_MODE_FAILED: {error}") from None
    print(f"PRODUCTION_MODE_OK: {args.mode}")


if __name__ == "__main__":
    main()
