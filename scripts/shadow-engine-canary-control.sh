#!/usr/bin/env sh

canary_input_limit_is_valid() {
  case "$1" in
    ''|*[!0-9]*) return 1 ;;
  esac
  [ "${#1}" -le 7 ] && [ "$1" -le 1000000 ]
}

canary_positive_timeout_is_valid() {
  case "$1" in
    ''|*[!0-9]*) return 1 ;;
  esac
  [ "${#1}" -le 4 ] && [ "$1" -gt 0 ] && [ "$1" -le 3600 ]
}

canary_is_enabled() {
  [ "$1" -gt 0 ]
}

canary_processed_delta() {
  [ "$2" -ge "$1" ] || return 1
  printf '%s\n' "$(( $2 - $1 ))"
}

canary_target_reached() {
  canary_delta=$(canary_processed_delta "$1" "$2") || return 1
  [ "$canary_delta" -ge "$3" ]
}

canary_within_overshoot_bound() {
  canary_delta=$(canary_processed_delta "$1" "$2") || return 1
  [ "$canary_delta" -le "$(( $3 + $4 ))" ]
}
