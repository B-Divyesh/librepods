#!/usr/bin/env bash
# tests/sim/screenshots.sh [OUT_DIR] — README screenshots from fake AirPods.
#
# Hyprland only. For about forty seconds it switches to an empty workspace,
# mutes mako, hides your Waybar and shows a copy of it whose AirPods module is
# fed by ui_sim.py, drives the real librepods-popup (card, bar menu) and the
# real settings window (LIBREPODS_UI_DEMO, on a mocked BlueZ), captures, then
# puts everything back — also when interrupted. No Bluetooth is touched.
#
#   needs: grim, jq, magick, python3 with python-dbusmock (PYTHON=…), and
#          `cargo build --release` in linux-rust/ and linux-rust/popup/
#   env:   WAYBAR_CONFIG, WAYBAR_STYLE (default ~/.config/waybar/…), WS=9
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RS="$(cd "$HERE/../.." && pwd)"
OUT="$(mkdir -p "${1:-$PWD/screenshots}" && cd "${1:-$PWD/screenshots}" && pwd)"
PY="${PYTHON:-python3}"
BIN="$RS/target/release/librepods"
POPUP="$RS/popup/target/release/librepods-popup"
WB_CONF="${WAYBAR_CONFIG:-$HOME/.config/waybar/config.jsonc}"
WB_CSS="${WAYBAR_STYLE:-$HOME/.config/waybar/style.css}"
RUN="$(mktemp -d)"
SIM="$RUN/sim.sock"
for x in "$BIN" "$POPUP"; do [[ -x $x ]] || { echo "build first: $x" >&2; exit 1; }; done

pids=() hidden= prev_ws="$(hyprctl activeworkspace -j | jq .id)" real_bar="$(pidof -s waybar || true)"
cleanup() {
    for p in "${pids[@]}"; do kill "$p" 2>/dev/null; done
    [[ -n ${BUS_PID:-} ]] && kill "$BUS_PID" 2>/dev/null
    [[ -n $hidden ]] && kill -SIGUSR1 "$real_bar" 2>/dev/null   # show the real bar again
    hyprctl dispatch "hl.dsp.focus({ workspace = $prev_ws })" >/dev/null
    makoctl mode -r do-not-disturb >/dev/null 2>&1
    rm -rf "$RUN"
}
trap cleanup EXIT INT TERM

send() { "$PY" -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.settimeout(10); s.connect(sys.argv[1]); s.sendall((" ".join(sys.argv[2:])+"\n").encode()); s.makefile().readline()' "$SIM" "$@"; }
layer() {  # namespace [pid]
    hyprctl layers -j | jq -r --arg n "$1" --arg p "${2:-}" \
        '[.[].levels[][] | select(.namespace==$n and ($p == "" or (.pid|tostring) == $p))][0] | "\(.x),\(.y) \(.w)x\(.h)"'
}
grab() { grim -g "$1" "$OUT/$2.png"; }

# an empty workspace
ws="${WS:-9}"
while (( $(hyprctl workspaces -j | jq --argjson w "$ws" '[.[] | select(.id==$w)][0].windows // 0') > 0 )); do ws=$((ws - 1)); done
makoctl mode -a do-not-disturb >/dev/null 2>&1
hyprctl dispatch "hl.dsp.focus({ workspace = $ws })" >/dev/null

"$PY" -u "$HERE/ui_sim.py" --socket "$SIM" > "$RUN/sim.log" 2>&1 & pids+=($!)
for _ in $(seq 50); do [[ -S $SIM ]] && break; sleep 0.1; done
"$POPUP" --socket "$SIM" > "$RUN/popup.log" 2>&1 & pids+=($!)
# the bar, with its AirPods module (and clicks) pointed at the simulator
sed "s|~/.local/bin/librepods ctl|env LIBREPODS_SOCKET=$SIM $BIN ctl|g" "$WB_CONF" > "$RUN/bar.jsonc"
[[ -n $real_bar ]] && kill -SIGUSR1 "$real_bar" && hidden=1
waybar -c "$RUN/bar.jsonc" -s "$WB_CSS" > "$RUN/bar.log" 2>&1 & bar_pid=$!; pids+=($bar_pid)
for _ in $(seq 60); do                 # wait for the copy (not the hidden real bar) to map
    BAR="$(layer waybar "$bar_pid")"
    [[ $BAR != null* ]] && break; sleep 0.25
done
[[ $BAR != null* ]] || { echo "the bar copy did not come up (see $RUN/bar.log)" >&2; exit 1; }
sleep 1.5                               # its modules fill in after the first frame

# ── bar module + finding it ──
grab "$BAR" bar-disconnected
send connect; sleep 2.2
grab "$BAR" bar-anc
# the connected pill is the only thing in the bar with an amber underline
# (Noise Cancellation) along its bottom edge
pill="$("$PY" - "$OUT/bar-anc.png" "$BAR" <<'EOF'
import sys
from PIL import Image
img = Image.open(sys.argv[1]).convert("RGB")
pos, size = sys.argv[2].split(" ")
x0, lw = int(pos.split(",")[0]), int(size.split("x")[0])
w, h = img.size
scale = w / lw                                   # grim captures at the output scale
amber = (232, 155, 94)
xs = [x for x in range(w // 2, w) for y in range(int(h * 0.70), int(h * 0.95))
      if sum(abs(c - a) for c, a in zip(img.getpixel((x, y)), amber)) < 60]
if not xs:
    print(0, 0, 0)
else:
    l, r = min(xs) / scale + x0, max(xs) / scale + x0
    print(int((l + r) / 2), int(l), int(r))
EOF
)"
read -r pill_x pill_l pill_r <<< "$pill"
send mode transparency; sleep 0.9; grab "$BAR" bar-transparency
send mode adaptive;     sleep 0.9; grab "$BAR" bar-adaptive
send mode anc;          sleep 0.6

# ── hero: the whole desktop with the dropdown open ──
send menu "$pill_x"; sleep 1.1
grim "$OUT/hero-menu.png"
send menu "$pill_x"; sleep 0.6

# ── the case card ──
send disconnect; sleep 0.5
send popup; sleep 1.4
CARD="$(layer librepods-popup)"
grab "$CARD" card-connect
grim "$OUT/hero-card.png"
send connect & sleep 0.6; grab "$CARD" card-connecting
sleep 1.3; grab "$CARD" card-connected
sleep 3.5

# ── pairing recovery ──
send disconnect; sleep 0.4
send repair & sleep 1.3
grab "$CARD" card-repair-waiting
grab "$BAR" bar-repair
sleep 4.2; grab "$CARD" card-repair-pairing
sleep 4

# ── settings window: the real UI with its demo device, on a mocked BlueZ ──
ADDR="$(dbus-daemon --session --print-address=1 --fork --print-pid=3 3>"$RUN/bus.pid" | head -1)"
BUS_PID="$(cat "$RUN/bus.pid")"
DBUS_SYSTEM_BUS_ADDRESS="$ADDR" "$PY" -m dbusmock --system --template "$HERE/fake_airpods.py" > "$RUN/mock.log" 2>&1 & pids+=($!)
sleep 1.5
gdbus call --address "$ADDR" -d org.bluez -o / -m org.bluez.Mock.AddAdapter hci0 sim >/dev/null
mkdir -p "$RUN/cfg" "$RUN/data"
for tab in device settings; do
    demo=1; [[ $tab == settings ]] && demo=settings
    env DBUS_SYSTEM_BUS_ADDRESS="$ADDR" LIBREPODS_UI_DEMO="$demo" LIBREPODS_SOCKET="$RUN/demo.sock" \
        XDG_CONFIG_HOME="$RUN/cfg" XDG_DATA_HOME="$RUN/data" "$BIN" --hide-tray-icon > "$RUN/demo-$tab.log" 2>&1 &
    dp=$!; pids+=($dp)
    for _ in $(seq 60); do
        geo="$(hyprctl clients -j | jq -r '.[] | select(.class=="me.kavishdevar.librepods") | "\(.at[0]),\(.at[1]) \(.size[0])x\(.size[1])"' | head -1)"
        [[ -n $geo ]] && break; sleep 0.25
    done
    sleep 2
    [[ -n $geo ]] && grab "$geo" "window-$tab"
    kill $dp; sleep 1
done

echo "pill at x=$pill_x ($pill_l–$pill_r); screenshots in $OUT:"
ls "$OUT"
