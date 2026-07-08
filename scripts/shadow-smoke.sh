#!/usr/bin/env sh
set -eu

PHOENIX_MODE=SHADOW LIVE_EXECUTION=false docker compose up --build feed-ingestor phoenix-engine recorder dashboard prometheus

