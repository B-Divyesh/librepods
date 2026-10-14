#!/usr/bin/env python3
"""A stand-in LibrePods daemon that plays AirPods scenarios to the real UI.

It speaks the daemon's socket protocol (status / watch / connect / mode /
ca / menu / popup / repair …), so the real librepods-popup (card + bar menu)
and `librepods ctl waybar` render exactly what they would with AirPods —
for checking flows by eye and for screenshots, with no Bluetooth at all.

  python3 tests/sim/ui_sim.py --scenario tour            # then, in other shells:
  librepods-popup --socket $XDG_RUNTIME_DIR/librepods-sim.sock
  LIBREPODS_SOCKET=$XDG_RUNTIME_DIR/librepods-sim.sock librepods ctl waybar

Scenarios: idle (just answers commands), tour, repair, handoff. Clicking
Connect / modes / Try Again in the popup drives the simulated AirPods.
"""

import argparse
import copy
import json
import os
import socket
import sys
import threading
import time

LOCK = threading.Lock()
WATCHERS = []  # sockets streaming events

STATE = {
    "connected": False,
    "nearby": True,
    "connected_elsewhere": False,
    "name": "AirPods Pro",
    "model": "AirPods Pro 2 (USB-C)",
    "model_id": 0x2024,
    "mac": "AC:DE:48:00:11:22",
    "left": {"level": 92, "charging": False},
    "right": {"level": 88, "charging": False},
    "case": {"level": 64, "charging": True},
    "headphone": None,
    "in_ear": {"left": False, "right": False},
    "in_case": {"left": True, "right": True},
    "lid_open": False,
    "listening_mode": None,
    "allow_off": True,
    "conversational_awareness": None,
    "auto_connect": True,
    "bluetooth_on": True,
    "repair": None,
}
MODES = ["anc", "transparency", "adaptive"]


def emit(event, anchor_x=None, **changes):
    with LOCK:
        STATE.update(changes)
        msg = dict(copy.deepcopy(STATE), event=event)
        if anchor_x is not None:
            msg["anchor_x"] = anchor_x
        line = (json.dumps(msg) + "\n").encode()
        for w in list(WATCHERS):
            try:
                w.sendall(line)
            except OSError:
                WATCHERS.remove(w)


def log(msg):
    print(f"[sim {time.strftime('%H:%M:%S')}] {msg}", flush=True)


def connect_flow(delay=1.6):
    emit("connecting")
    time.sleep(delay)
    emit(
        "connected",
        connected=True,
        connected_elsewhere=False,
        listening_mode=STATE["listening_mode"] or "anc",
        conversational_awareness=True if STATE["conversational_awareness"] is None else STATE["conversational_awareness"],
    )


def repair_flow(wait=5.0):
    emit("repair", repair="waiting", connected=False)
    log("repair: waiting for pairing mode (simulated)")
    time.sleep(wait)
    emit("repair", repair="pairing")
    time.sleep(2.0)
    emit("repaired", repair=None)
    connect_flow(1.0)


def handle(cmd, arg, conn):
    if cmd == "status":
        with LOCK:
            return json.dumps(STATE)
    if cmd == "connect" or (cmd == "toggle" and not STATE["connected"]):
        if STATE["repair"] == "failed":
            threading.Thread(target=repair_flow, daemon=True).start()
            return json.dumps({"ok": True})
        connect_flow()
        return json.dumps({"ok": True})
    if cmd in ("disconnect", "toggle"):
        emit("disconnected", connected=False, listening_mode=None, conversational_awareness=None)
        return json.dumps({"ok": True})
    if cmd == "mode":
        cur = STATE["listening_mode"] or "anc"
        nxt = MODES[(MODES.index(cur) + 1) % len(MODES)] if arg in ("", "next") else arg
        emit("mode", listening_mode=nxt)
        return json.dumps({"ok": True, "mode": nxt})
    if cmd in ("ca", "conversation"):
        on = (not STATE["conversational_awareness"]) if arg in ("", "toggle") else arg == "on"
        emit("ca", conversational_awareness=on)
        return json.dumps({"ok": True})
    if cmd == "menu":
        emit("menu", anchor_x=int(arg) if arg.isdigit() else None)
        return json.dumps({"ok": True})
    if cmd == "popup":
        emit("popup")
        return json.dumps({"ok": True})
    if cmd == "repair":
        threading.Thread(target=repair_flow, daemon=True).start()
        return json.dumps({"ok": True, "repair": "waiting"})
    if cmd in ("open", "bluetooth", "bt"):
        log(f"(would {cmd} {arg})")
        return json.dumps({"ok": True})
    return json.dumps({"error": f"unknown command '{cmd}'"})


def client(conn):
    f = conn.makefile("r")
    for line in f:
        parts = line.split()
        if not parts:
            continue
        cmd, arg = parts[0], (parts[1] if len(parts) > 1 else "")
        if cmd == "watch":
            with LOCK:
                conn.sendall((json.dumps(dict(STATE, event="status")) + "\n").encode())
                WATCHERS.append(conn)
            return  # the connection now belongs to the event stream
        log(f"<- {line.strip()}")
        reply = handle(cmd, arg, conn)
        try:
            conn.sendall((reply + "\n").encode())
        except OSError:
            return


SCENARIOS = {
    "idle": [],
    # The case-open → connect → modes → battery tour (good for screenshots).
    "tour": [
        (1.0, lambda: emit("lid_open", lid_open=True)),
        (2.5, connect_flow),
        (2.0, lambda: emit("status", in_ear={"left": True, "right": True}, in_case={"left": False, "right": False},
                           case={"level": 64, "charging": False}, lid_open=False)),
        (2.0, lambda: emit("mode", listening_mode="transparency")),
        (2.0, lambda: emit("mode", listening_mode="adaptive")),
        (2.0, lambda: emit("low_battery", right={"level": 18, "charging": False})),
        (3.0, lambda: emit("disconnected", connected=False, listening_mode=None, in_ear={"left": False, "right": False})),
    ],
    # AirPods forgot this computer; it re-pairs as soon as the case is in pairing mode.
    "repair": [
        (1.0, lambda: emit("lid_open", lid_open=True)),
        (1.5, lambda: repair_flow(6.0)),
    ],
    # On the iPhone → press play here → taken over → idle → handed back.
    "handoff": [
        (0.5, lambda: emit("status", connected_elsewhere=True, in_ear={"left": True, "right": True},
                           in_case={"left": False, "right": False})),
        (3.0, lambda: (log("you press play here"), connect_flow(1.4))),
        (6.0, lambda: (log("two quiet minutes later…"),
                       emit("disconnected", connected=False, listening_mode=None, connected_elsewhere=True))),
    ],
}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--socket", default=os.path.join(os.environ.get("XDG_RUNTIME_DIR", "/tmp"), "librepods-sim.sock"))
    ap.add_argument("--scenario", default="idle", choices=SCENARIOS)
    ap.add_argument("--loop", action="store_true")
    ap.add_argument("--speed", type=float, default=1.0, help="2 = twice as fast")
    a = ap.parse_args()

    if os.path.exists(a.socket):
        os.unlink(a.socket)
    srv = socket.socket(socket.AF_UNIX)
    srv.bind(a.socket)
    srv.listen(16)
    log(f"listening on {a.socket} — scenario {a.scenario}")
    threading.Thread(target=lambda: [threading.Thread(target=client, args=(c,), daemon=True).start()
                                     for c, _ in iter(srv.accept, None)], daemon=True).start()
    try:
        while True:
            for delay, step in SCENARIOS[a.scenario]:
                time.sleep(delay / a.speed)
                step()
            if not a.loop:
                break
            time.sleep(2)
        if a.scenario != "idle" and not a.loop:
            log("scenario finished; still answering commands (Ctrl-C to quit)")
        while True:
            time.sleep(3600)
    except KeyboardInterrupt:
        pass
    finally:
        os.unlink(a.socket)


if __name__ == "__main__":
    sys.exit(main())
