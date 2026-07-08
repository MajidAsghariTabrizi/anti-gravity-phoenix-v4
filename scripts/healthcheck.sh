#!/usr/bin/env sh
set -eu

docker compose ps
docker compose exec -T postgres pg_isready -U phoenix -d phoenix

