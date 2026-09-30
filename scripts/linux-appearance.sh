set -euo pipefail
theme="${CHECK_SCOPE#appearance-}"
case "$theme" in light|dark) ;; codec) theme=light;; *) exit 1;; esac
test "$GITHUB_ACTIONS" = true
test -x "$HARNESS_BINARY"
export DISPLAY=:91
test ! -e /tmp/.X11-unix/X91
export GTK_THEME=Adwaita
wm_theme=Clearlooks
color_scheme=default
if [[ "$theme" == dark ]]; then
  export GTK_THEME=Adwaita:dark
  wm_theme=Onyx
  color_scheme=prefer-dark
fi
gsettings set org.gnome.desktop.interface gtk-theme "${GTK_THEME/:dark/}"
gsettings set org.gnome.desktop.interface color-scheme "$color_scheme"
test -d "/usr/share/themes/$wm_theme/openbox-3"
printf '<openbox_config xmlns="http://openbox.org/3.4/rc"><theme><name>%s</name></theme></openbox_config>\n' "$wm_theme" > "$RUNNER_TEMP/appearance-openbox.xml"
Xvfb "$DISPLAY" -screen 0 1280x900x24 -nolisten tcp > artifacts/appearance-xvfb.log 2>&1 &
xvfb_pid=$!
wm_pid=
trap 'if [[ -n "$wm_pid" ]]; then kill "$wm_pid" 2>/dev/null || true; fi; kill "$xvfb_pid" 2>/dev/null || true' EXIT
for attempt in {1..50}; do
  if xdpyinfo >/dev/null 2>&1; then break; fi
  kill -0 "$xvfb_pid"
  sleep 0.1
done
xdpyinfo > artifacts/appearance-display.txt
openbox --config-file "$RUNNER_TEMP/appearance-openbox.xml" > artifacts/appearance-openbox.log 2>&1 &
wm_pid=$!
for attempt in {1..50}; do
  if xprop -root _NET_SUPPORTING_WM_CHECK | grep -Eq '0x[1-9a-fA-F][0-9a-fA-F]*'; then break; fi
  kill -0 "$wm_pid"
  sleep 0.1
done
if [[ "$CHECK_SCOPE" == appearance-codec ]]; then
  python3 benchmarks/linux/run.py --binary "$HARNESS_BINARY" \
    --output artifacts/appearance-codec --phase appearance-app \
    --app-theme light --system-theme light --capture-size 1280x900 \
    --display-owner-pid "$xvfb_pid" --capture-codec-experiment
  exit 0
fi
for app_theme in light dark; do
  output="artifacts/appearance-$theme-$app_theme"
  python3 benchmarks/linux/run.py --binary "$HARNESS_BINARY" --output "$output" \
    --phase appearance-seed --app-theme "$app_theme" --system-theme "$theme"
  python3 benchmarks/linux/run.py --binary "$HARNESS_BINARY" --output "$output" \
    --phase appearance-app --app-theme "$app_theme" --system-theme "$theme" \
    --capture-size 1280x900 --display-owner-pid "$xvfb_pid"
done
