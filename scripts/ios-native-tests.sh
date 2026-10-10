#!/usr/bin/env bash
set -euo pipefail
: "${CARGO_TARGET_DIR:?Use the existing shared Cargo target directory}"
profile=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/risunest-ios-tests.XXXXXX")
device_id=""
cleanup() {
    if [[ -n "$device_id" ]]; then xcrun simctl shutdown "$device_id" >/dev/null 2>&1 || true; fi
    rm -rf -- "$profile"
}
trap cleanup EXIT
export IPHONEOS_DEPLOYMENT_TARGET
IPHONEOS_DEPLOYMENT_TARGET=$(node -p "require('./src-tauri/tauri.ios.conf.json').bundle.iOS.minimumSystemVersion")
device_id=$(xcrun simctl list devices available -j | python3 -c 'import json,sys; data=json.load(sys.stdin); print(next(device["udid"] for runtime,devices in data["devices"].items() if "iOS" in runtime for device in devices if device.get("isAvailable") and device.get("state") in ("Shutdown", "Booted")))')
xcrun simctl boot "$device_id" 2>/dev/null || true
xcrun simctl bootstatus "$device_id" -b
cat > "$profile/entitlements.plist" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>application-identifier</key><string>io.github.rsyumi.risunest.rust-tests</string>
<key>keychain-access-groups</key><array><string>io.github.rsyumi.risunest.rust-tests</string></array>
</dict></plist>
EOF
xcrun derq query -f xml -i "$profile/entitlements.plist" -o "$profile/entitlements.der" --raw
support="$CARGO_TARGET_DIR/ios-test-support"
mkdir -p "$support"
support=$(cd "$support" && pwd -P)
identity=$(shasum -a 256 "$profile/entitlements.plist" | awk '{print $1}')
for extension in plist der; do
    if [[ ! -f "$support/entitlements-$identity.$extension" ]]; then
        mv "$profile/entitlements.$extension" "$support/entitlements-$identity.$extension"
    fi
done
link_args=(
    "-Wl,-sectcreate,__TEXT,__entitlements,$support/entitlements-$identity.plist"
    "-Wl,-sectcreate,__TEXT,__ents_der,$support/entitlements-$identity.der"
)
xcrun --sdk iphonesimulator clang -arch arm64 -mios-simulator-version-min="$IPHONEOS_DEPLOYMENT_TARGET" \
    -framework CoreFoundation -framework Security "${link_args[@]}" \
    tests/ios-rust/keychain-probe.c -o "$profile/keychain-probe"
xcrun simctl spawn "$device_id" "$profile/keychain-probe"
if [[ "${1:-}" == --preflight-only ]]; then exit 0; fi
cat > "$profile/runner.sh" <<'EOF'
#!/bin/sh
exec xcrun simctl spawn "$IOS_SIMULATOR_UDID" "$@"
EOF
chmod +x "$profile/runner.sh"
export IOS_SIMULATOR_UDID="$device_id"
export CARGO_TARGET_AARCH64_APPLE_IOS_SIM_RUNNER="$profile/runner.sh"
export CARGO_ENCODED_RUSTFLAGS="${CARGO_ENCODED_RUSTFLAGS:-}"
for argument in "${link_args[@]}"; do
    if [[ -n "$CARGO_ENCODED_RUSTFLAGS" ]]; then CARGO_ENCODED_RUSTFLAGS+=$'\x1f'; fi
    CARGO_ENCODED_RUSTFLAGS+="-Clink-arg=$argument"
done
cargo test --manifest-path src-tauri/Cargo.toml --release --locked --target aarch64-apple-ios-sim --lib "$@"
