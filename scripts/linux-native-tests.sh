#!/usr/bin/env bash
set -euo pipefail
: "${CARGO_TARGET_DIR:?Use the existing shared Cargo target directory}"
: "${DBUS_SESSION_BUS_ADDRESS:?Run this script inside dbus-run-session}"
profile=$(mktemp -d /tmp/risunest-linux-tests.XXXXXX)
keyring=
sampler=
finish() {
    local status=$? code=running
    if [[ -n "$keyring" ]]; then
        if ! kill -0 "$keyring" 2>/dev/null; then
            wait "$keyring" && code=0 || code=$?
        fi
        # A lost keyring fails every credential test, so report how it ended and what it held.
        if [[ "$status" != 0 || "$code" != running ]]; then
            echo "gnome-keyring-daemon: $code; nofile $(ulimit -n), memlock $(ulimit -l), cpus $(nproc)" >&2
            tail -n 40 "$profile/keyring-samples" >&2 || true
            sudo -n dmesg 2>/dev/null | grep -iE 'keyring|segfault|oom|killed process' | tail -n 20 >&2 || true
        fi
    fi
    if [[ -n "$sampler" ]]; then kill "$sampler" 2>/dev/null || true; fi
    if [[ "$code" == running ]]; then kill "$keyring" 2>/dev/null && wait "$keyring" 2>/dev/null || true; fi
    rm -rf -- "$profile"
    exit "$status"
}
trap finish EXIT
export XDG_DATA_HOME="$profile/data"
export XDG_CONFIG_HOME="$profile/config"
export XDG_CACHE_HOME="$profile/cache"
mkdir -m 700 -p "$XDG_DATA_HOME" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME" "$profile/control"
printf '%s' 'risunest-synthetic-tests-only' | gnome-keyring-daemon --foreground --unlock --components=secrets --control-directory="$profile/control" &
keyring=$!
for _ in $(seq 100); do
    [[ -S "$profile/control/control" ]] || ! kill -0 "$keyring" 2>/dev/null && break
    sleep 0.1
done
[[ -S "$profile/control/control" ]] || { echo "gnome-keyring-daemon did not start" >&2; exit 1; }
gnome-keyring-daemon --start --components=secrets --control-directory="$profile/control"
for _ in $(seq 100); do
    owner=$(dbus-send --session --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus \
        org.freedesktop.DBus.NameHasOwner string:org.freedesktop.secrets 2>/dev/null || true)
    [[ "$owner" == *'boolean true'* ]] && break
    sleep 0.1
done
[[ "$owner" == *'boolean true'* ]] || { echo "gnome-keyring-daemon did not own org.freedesktop.secrets" >&2; exit 1; }
while kill -0 "$keyring" 2>/dev/null; do
    echo "$(date -u +%T) fds $(ls "/proc/$keyring/fd" 2>/dev/null | wc -l) $(grep VmRSS "/proc/$keyring/status" 2>/dev/null || true)" >> "$profile/keyring-samples"
    sleep 15
done >/dev/null 2>&1 &
sampler=$!
if [[ "${1:-}" == --boundary ]]; then
    shift
    pnpm desktop:test-boundary:agent "$@"
else
    cargo test --manifest-path src-tauri/Cargo.toml --locked "$@"
fi
