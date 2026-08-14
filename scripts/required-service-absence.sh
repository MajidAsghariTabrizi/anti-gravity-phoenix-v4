#!/usr/bin/env sh

# This file is a sourced library. A required-absent service is absent only when
# a fresh Docker label lookup returns no container for the Compose project and
# service. A running, stopped, or removing container therefore remains present.

phoenix_wait_required_service_absent() {
  [ "$#" -eq 2 ] || return 1
  required_compose_command=$1
  required_service=$2
  required_wait_seconds=${PHOENIX_REQUIRED_ABSENCE_WAIT_SECONDS:-30}
  required_poll_seconds=${PHOENIX_REQUIRED_ABSENCE_POLL_SECONDS:-1}
  required_docker_bin=${PHOENIX_DOCKER_BIN:-/usr/bin/docker}

  case "$required_service" in
    ''|*[!a-z0-9_.-]*|[.-]*) return 1 ;;
  esac
  [ "${#required_service}" -le 128 ] || return 1
  case "$required_wait_seconds" in
    ''|*[!0-9]*) return 1 ;;
  esac
  [ "$required_wait_seconds" -ge 1 ] &&
    [ "$required_wait_seconds" -le 300 ] || return 1
  case "$required_poll_seconds" in
    ''|*[!0-9]*) return 1 ;;
  esac
  [ "$required_poll_seconds" -ge 1 ] &&
    [ "$required_poll_seconds" -le 10 ] || return 1
  case "$required_docker_bin" in
    /*) ;;
    *) return 1 ;;
  esac
  [ -x "$required_docker_bin" ] || return 1

  required_config=$(
    "$required_compose_command" config --format json
  ) || return 1
  required_project=$(
    printf '%s\n' "$required_config" |
      python3 -I -B -c '
import json
import re
import sys

try:
    value = json.load(sys.stdin)
except (UnicodeError, json.JSONDecodeError):
    raise SystemExit(1)
name = value.get("name") if isinstance(value, dict) else None
if not isinstance(name, str) or not re.fullmatch(r"[a-z0-9][a-z0-9_.-]{0,127}", name):
    raise SystemExit(1)
print(name)
'
  ) || return 1

  required_deadline=$(( $(date +%s) + required_wait_seconds ))
  while :; do
    required_ids=$(
      "$required_docker_bin" ps --all --quiet --no-trunc \
        --filter "label=com.docker.compose.project=$required_project" \
        --filter "label=com.docker.compose.service=$required_service"
    ) || return 1
    [ -n "$required_ids" ] || return 0

    while IFS= read -r required_id; do
      [ -n "$required_id" ] || continue
      case "$required_id" in
        *[!0-9a-f]*) return 1 ;;
      esac
      [ "${#required_id}" -ge 12 ] && [ "${#required_id}" -le 64 ] ||
        return 1
    done <<EOF
$required_ids
EOF

    [ "$(date +%s)" -lt "$required_deadline" ] || return 1
    sleep "$required_poll_seconds"
  done
}
