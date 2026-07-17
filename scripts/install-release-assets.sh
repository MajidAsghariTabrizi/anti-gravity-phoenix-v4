#!/usr/bin/env sh
set -eu
umask 027

release_sha=${1:-}
archive=${2:-}
manifest=${3:-}
checksums=${4:-}
release_root=${PHOENIX_RELEASE_ROOT:-/opt/phoenix/releases}
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
context_installer=${PHOENIX_CONTEXT_INSTALLER:-$script_dir/install-production-release-context.sh}

fail() {
  echo "RELEASE_ASSET_INSTALL_FAILED: $1" >&2
  exit 1
}

[ "$(id -u)" -eq 0 ] || fail 'root privileges are required'
[ "$(uname -s)" = Linux ] || fail 'Linux is required'
case "$release_sha" in
  *[!0-9a-f]*|'') fail 'release SHA must be 40 lowercase hex characters' ;;
esac
[ "${#release_sha}" -eq 40 ] || fail 'release SHA must be 40 lowercase hex characters'
[ -f "$archive" ] || fail 'release archive is missing'
[ -f "$manifest" ] || fail 'release-assets manifest is missing'
[ -f "$checksums" ] || fail 'release-assets checksums are missing'
command -v python3 >/dev/null 2>&1 || fail 'python3 is unavailable'
command -v tar >/dev/null 2>&1 || fail 'tar is unavailable'
command -v sha256sum >/dev/null 2>&1 || fail 'sha256sum is unavailable'
command -v cmp >/dev/null 2>&1 || fail 'cmp is unavailable'
command -v readlink >/dev/null 2>&1 || fail 'readlink is unavailable'
[ -f "$context_installer" ] && [ ! -L "$context_installer" ] ||
  fail 'release-context installer is missing or unsafe'

archive=$(readlink -f "$archive") || fail 'release archive path is invalid'
manifest=$(readlink -f "$manifest") || fail 'release-assets manifest path is invalid'
checksums=$(readlink -f "$checksums") || fail 'release-assets checksum path is invalid'
artifact_dir=$(dirname "$archive")
[ "$(dirname "$manifest")" = "$artifact_dir" ] || fail 'release artifacts must share one directory'
[ "$(dirname "$checksums")" = "$artifact_dir" ] || fail 'release artifacts must share one directory'
[ "$(basename "$archive")" = "phoenix-release-assets-$release_sha.tar.gz" ] || fail 'release archive name is invalid'
[ "$(basename "$manifest")" = release-assets-manifest.json ] || fail 'release-assets manifest name is invalid'
[ "$(basename "$checksums")" = release-assets-checksums.txt ] || fail 'release-assets checksum name is invalid'

(
  cd "$artifact_dir"
  sha256sum -c "$(basename "$checksums")" >/dev/null
) || fail 'release-assets checksum validation failed'

bundle_root="phoenix-release-$release_sha"
python3 - "$archive" "$bundle_root" <<'PY' || fail 'release archive member validation failed'
import sys
import tarfile
from pathlib import PurePosixPath

archive_path, expected_root = sys.argv[1:]
count = 0
total = 0
with tarfile.open(archive_path, mode="r:gz") as archive:
    for member in archive.getmembers():
        count += 1
        if count > 513 or not member.isfile() or member.size > 8 * 1024 * 1024:
            raise SystemExit(1)
        path = PurePosixPath(member.name)
        if path.is_absolute() or len(path.parts) < 2 or path.parts[0] != expected_root:
            raise SystemExit(1)
        if any(part in ("", ".", "..") for part in path.parts):
            raise SystemExit(1)
        total += member.size
        if total > 72 * 1024 * 1024:
            raise SystemExit(1)
PY

install -d -m 0750 -o root -g root "$release_root"
candidate=$(mktemp -d "$release_root/.candidate-$release_sha.XXXXXX") || fail 'release staging directory could not be created'
cleanup() {
  rm -rf -- "$candidate"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
tar --extract --gzip --file "$archive" --directory "$candidate" --no-same-owner || fail 'release archive extraction failed'
candidate_root=$candidate/$bundle_root
[ -d "$candidate_root" ] || fail 'release archive root is missing'

python3 "$candidate_root/scripts/release_assets.py" verify \
  --archive "$archive" \
  --manifest "$manifest" \
  --checksums "$checksums" \
  --expected-sha "$release_sha" >/dev/null || fail 'release archive integrity verification failed'

final_root=$release_root/$release_sha
if [ -e "$final_root" ]; then
  [ -d "$final_root" ] || fail 'immutable release path is not a directory'
  cmp "$candidate_root/release-assets-manifest.json" "$final_root/release-assets-manifest.json" >/dev/null 2>&1 ||
    fail 'immutable release path already contains different assets'
else
  mv "$candidate_root" "$final_root" || fail 'immutable release promotion failed'
  chown -R root:root "$final_root"
  chmod -R go-w "$final_root"
fi

python3 "$final_root/scripts/release_assets.py" verify-tree \
  --root "$final_root" \
  --manifest "$manifest" \
  --expected-sha "$release_sha" >/dev/null || fail 'immutable release tree verification failed'

/bin/sh "$context_installer" "$release_sha" "$final_root" ||
  fail 'canonical release-context installation failed'
trap - EXIT HUP INT TERM
rm -rf -- "$candidate"
echo "RELEASE_ASSET_INSTALL_OK: $release_sha"
