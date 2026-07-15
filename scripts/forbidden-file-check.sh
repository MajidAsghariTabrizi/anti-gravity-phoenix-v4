#!/usr/bin/env sh
set -eu

found_file="$(mktemp)"
trap 'rm -f "$found_file"' EXIT

check_path() {
  path="$1"
  category="$2"
  printf 'FILE: %s\n' "$path"
  printf 'FORBIDDEN CATEGORY: %s\n' "$category"
  printf 'ACTION REQUIRED: remove from tracked candidates or update ignore policy\n\n'
  echo 1 > "$found_file"
}

git ls-files --cached --others --exclude-standard | while IFS= read -r path; do
  case "$path" in
    .env.example|fixtures/*) continue ;;
    FETCH_HEAD) check_path "$path" "accidental Git runtime state" ;;
    .env|.env.local|.env.*.local) check_path "$path" "environment file" ;;
    *.pem|*.key|*.pfx|*.p12|*.jks|*.keystore) check_path "$path" "private key or certificate" ;;
    keystore/*|keystores/*|wallets/*|*UTC--*|*wallet*.json|*.wallet) check_path "$path" "keystore or wallet export" ;;
    *.db|*.sqlite|*.sqlite3|*.db-wal|*.db-shm) check_path "$path" "local database" ;;
    recordings/*|feed-recordings/*|*.ndjson.zst|*.jsonl.zst) check_path "$path" "feed recording output" ;;
    replay-output/*|benchmark-output/*|bench-output/*|*.prof|*.pprof|*.bench) check_path "$path" "replay or benchmark output" ;;
    postgres-data/*|postgres_data/*|pgdata/*|prometheus-data/*|prometheus_data/*|.tmp/*|tmp/*|temp/*) check_path "$path" "runtime data directory" ;;
    target/*|*/target/*|out/*|cache/*|broadcast/*|dist/*|build/*|node_modules/*|*__pycache__*|*.pyc|*.exe|*.test|*.out) check_path "$path" "build output" ;;
  esac
done

if [ -s "$found_file" ]; then
  exit 1
fi
