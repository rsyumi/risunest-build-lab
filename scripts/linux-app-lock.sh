set -euo pipefail
test "$GITHUB_ACTIONS" = true
test "$CHECK_SCOPE" = app-lock
test -x "$HARNESS_BINARY"
export DISPLAY=:91
test ! -e /tmp/.X11-unix/X91
export GTK_THEME=Adwaita
printf '<openbox_config xmlns="http://openbox.org/3.4/rc"><theme><name>Clearlooks</name></theme></openbox_config>\n' > "$RUNNER_TEMP/app-lock-openbox.xml"
Xvfb "$DISPLAY" -screen 0 1280x900x24 -nolisten tcp > artifacts/app-lock-xvfb.log 2>&1 &
xvfb_pid=$!
wm_pid=
trap 'if [[ -n "$wm_pid" ]]; then kill "$wm_pid" 2>/dev/null || true; fi; kill "$xvfb_pid" 2>/dev/null || true' EXIT
for attempt in {1..50}; do
  if xdpyinfo >/dev/null 2>&1; then break; fi
  kill -0 "$xvfb_pid"
  sleep 0.1
done
xdpyinfo > artifacts/app-lock-display.txt
openbox --config-file "$RUNNER_TEMP/app-lock-openbox.xml" > artifacts/app-lock-openbox.log 2>&1 &
wm_pid=$!
for attempt in {1..50}; do
  if xprop -root _NET_SUPPORTING_WM_CHECK | grep -Eq '0x[1-9a-fA-F][0-9a-fA-F]*'; then break; fi
  kill -0 "$wm_pid"
  sleep 0.1
done
xprop -root _NET_SUPPORTING_WM_CHECK | grep -Eq '0x[1-9a-fA-F][0-9a-fA-F]*'
for phase in app-lock-immediate app-lock-timed app-lock-immediate-mutant; do
  python3 benchmarks/linux/run.py --binary "$HARNESS_BINARY" \
    --output artifacts/app-lock-frames --phase "$phase" --capture-size 1280x900 \
    --display-owner-pid "$xvfb_pid"
done
