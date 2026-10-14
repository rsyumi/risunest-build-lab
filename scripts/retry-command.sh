#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -eq 0 ]; then
  echo "Usage: retry-command.sh command [arguments...]" >&2
  exit 2
fi

for attempt in 1 2 3; do
  if "$@"; then
    exit 0
  else
    status=$?
  fi
  if [ "$attempt" -eq 3 ] || [ "$status" -ge 128 ]; then
    exit "$status"
  fi
  delay=$((attempt * 10))
  echo "Command failed (exit $status); retrying in ${delay}s ($((attempt + 1))/3)." >&2
  sleep "$delay"
done
