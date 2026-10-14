#!/usr/bin/env python3
"""End-to-end scenarios for LibrePods' connection handling, without AirPods.

Runs the real `librepods` daemon (headless) against a mocked BlueZ on a
private D-Bus (fake_airpods.py), with its own socket, config and data dirs,
and no access to the real Bluetooth stack, sound server or notifications.

  pip install python-dbusmock          (plus dbus-python, PyGObject)
  cargo build --release
  python3 tests/sim/run_sim.py [-v] [-k pattern]

Set LIBREPODS_BIN to test another binary; SIM_LOG=1 prints the daemon log.
"""

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import unittest

import dbus
import dbusmock

HERE = os.path.dirname(os.path.abspath(__file__))
BIN = os.environ.get("LIBREPODS_BIN", os.path.join(HERE, "..", "..", "target", "release", "librepods"))
PODS = "AA:BB:CC:00:11:22"
REPAIR_TIMEOUT = 6  # seconds the daemon waits for pairing mode in these tests


class Sim(dbusmock.DBusTestCase):
    @classmethod
    def setUpClass(cls):
        cls.start_system_bus()
        cls.start_session_bus()  # notifications go nowhere
        cls.bluez, cls.bluez_obj = cls.spawn_server_template(
            os.path.join(HERE, "fake_airpods.py"), {}, stdout=subprocess.DEVNULL, system_bus=True
        )
        cls.mock = dbus.Interface(cls.bluez_obj, "org.bluez.Mock")
        cls.mock.AddAdapter("hci0", "sim")

    @classmethod
    def tearDownClass(cls):
        cls.bluez.terminate()
        cls.bluez.wait()
        super().tearDownClass()

    def setUp(self):
        self.mock.AddAirPods("hci0", PODS, "AirPods Pro")
        self.tmp = tempfile.mkdtemp(prefix="librepods-sim-")
        for d in ("config", "data/librepods", "run"):
            os.makedirs(os.path.join(self.tmp, d), exist_ok=True)
        with open(os.path.join(self.tmp, "data/librepods/devices.json"), "w") as f:
            json.dump({PODS: {"name": "AirPods Pro", "type_": "AirPods", "information": None}}, f)
        self.sock = os.path.join(self.tmp, "run", "librepods.sock")
        env = {
            k: v
            for k, v in os.environ.items()
            if k not in ("WAYLAND_DISPLAY", "DISPLAY", "PULSE_SERVER", "XDG_RUNTIME_DIR")
        }
        env.update(
            {
                "XDG_CONFIG_HOME": os.path.join(self.tmp, "config"),
                "XDG_DATA_HOME": os.path.join(self.tmp, "data"),
                "XDG_RUNTIME_DIR": os.path.join(self.tmp, "run"),
                "LIBREPODS_SOCKET": self.sock,
                "LIBREPODS_REPAIR_TIMEOUT": str(REPAIR_TIMEOUT),
                "RUST_LOG": "info,zbus=warn",
            }
        )
        self.env = env
        self.log = open(os.path.join(self.tmp, "daemon.log"), "w")
        self.daemon = subprocess.Popen([BIN, "--no-tray"], env=env, stdout=self.log, stderr=subprocess.STDOUT)
        self.wait_for(lambda: os.path.exists(self.sock), 10, "daemon socket")

    def tearDown(self):
        self.daemon.terminate()
        try:
            self.daemon.wait(5)
        except subprocess.TimeoutExpired:
            self.daemon.kill()
        self.log.close()
        if os.environ.get("SIM_LOG"):
            with open(os.path.join(self.tmp, "daemon.log")) as f:
                sys.stderr.write(f"\n--- daemon log ({self._testMethodName}) ---\n{f.read()}\n")
        try:
            self.mock.RemoveAdapterWithDevices("hci0")
        except dbus.exceptions.DBusException:
            pass
        self.mock.AddAdapter("hci0", "sim")
        shutil.rmtree(self.tmp, ignore_errors=True)

    # ── helpers ──

    def ipc(self, line, timeout=25):
        s = socket.socket(socket.AF_UNIX)
        s.settimeout(timeout)
        s.connect(self.sock)
        s.sendall((line + "\n").encode())
        buf = b""
        while not buf.endswith(b"\n"):
            chunk = s.recv(4096)
            if not chunk:
                break
            buf += chunk
        s.close()
        return json.loads(buf.decode())

    def status(self):
        return self.ipc("status")

    def pods(self):
        return {str(k): bool(v) if isinstance(v, dbus.Boolean) else int(v) for k, v in self.mock.PodsState(PODS).items()}

    def wait_for(self, pred, timeout, what):
        end = time.time() + timeout
        while time.time() < end:
            try:
                if pred():
                    return
            except (OSError, ValueError, dbus.exceptions.DBusException):
                pass
            time.sleep(0.1)
        self.fail(f"timed out waiting for {what}; status={self._safe_status()} pods={self.pods()}")

    def _safe_status(self):
        try:
            return self.status()
        except Exception as e:  # noqa: BLE001
            return f"<{e}>"

    def lose_pairing(self):
        """The AirPods forget us, then a connect is attempted."""
        self.mock.ForgetHost(PODS)
        reply = self.ipc("connect")
        self.assertIn("error", reply, reply)
        self.assertIn("forgot", reply["error"])
        self.wait_for(lambda: self.status()["repair"] == "waiting", 3, "repair=waiting")

    # ── scenarios ──

    def test_connect_works_normally(self):
        reply = self.ipc("connect")
        self.assertTrue(reply.get("ok"), reply)
        self.wait_for(lambda: self.pods()["connected"], 3, "connected")
        self.assertIsNone(self.status()["repair"])

    def test_lost_pairing_is_repaired_by_itself(self):
        self.lose_pairing()
        # the stale bond is dropped so an inquiry can find them again
        self.wait_for(lambda: not self.pods()["present"], 3, "stale device removed")
        self.mock.EnterPairingMode(PODS)
        self.wait_for(lambda: self.status()["repair"] is None, 8, "repair finished")
        p = self.pods()
        self.assertEqual(p["pair_count"], 1)
        self.assertTrue(p["paired"] and p["trusted"] and p["host_key"], p)
        self.wait_for(lambda: self.pods()["connected"], 8, "reconnected after pairing")

    def test_pairing_mode_before_the_scan_starts(self):
        # Holding the button early: the next inquiry still finds them.
        self.mock.ForgetHost(PODS)
        self.mock.EnterPairingMode(PODS)
        self.ipc("connect")
        self.wait_for(lambda: self.status()["repair"] is None and self.pods()["pair_count"] == 1, 10, "repaired")

    def test_page_outside_pairing_mode_keeps_waiting(self):
        self.lose_pairing()
        self.wait_for(lambda: not self.pods()["present"], 3, "stale device removed")
        self.mock.PageUs(PODS)  # they reappear, but pairing is refused
        time.sleep(1.5)
        self.assertEqual(self.status()["repair"], "waiting", "a refused pair must not give up")
        self.mock.EnterPairingMode(PODS)
        self.wait_for(lambda: self.status()["repair"] is None, 10, "repaired after pairing mode")
        self.assertEqual(self.pods()["pair_count"], 1)

    def test_timeout_then_connect_retries(self):
        self.lose_pairing()
        self.wait_for(lambda: self.status()["repair"] == "failed", REPAIR_TIMEOUT + 4, "repair=failed")
        # Connect after a failed recovery starts it again (the bond is gone,
        # so a plain connect could never work).
        reply = self.ipc("connect")
        self.assertTrue(reply.get("ok"), reply)
        self.wait_for(lambda: self.status()["repair"] == "waiting", 3, "repair restarted")
        self.mock.EnterPairingMode(PODS)
        self.wait_for(lambda: self.status()["repair"] is None, 10, "repaired")

    def test_cancel(self):
        self.lose_pairing()
        self.assertTrue(self.ipc("repair cancel").get("ok"))
        self.wait_for(lambda: self.status()["repair"] is None, 3, "repair cleared")

    def test_connect_is_refused_while_repairing(self):
        self.lose_pairing()
        reply = self.ipc("connect")
        self.assertIn("pairing again", reply.get("error", ""), reply)

    def test_manual_repair_command(self):
        reply = self.ipc("repair")
        self.assertEqual(reply.get("repair"), "waiting", reply)
        self.wait_for(lambda: not self.pods()["present"], 3, "old bond removed")
        self.mock.EnterPairingMode(PODS)
        self.wait_for(lambda: self.status()["repair"] is None and self.pods()["pair_count"] == 1, 10, "re-paired")

    def test_watch_stream_reports_each_step(self):
        s = socket.socket(socket.AF_UNIX)
        s.settimeout(15)
        s.connect(self.sock)
        s.sendall(b"watch popup\n")
        f = s.makefile()
        self.assertEqual(json.loads(f.readline())["event"], "status")
        self.lose_pairing()
        self.mock.EnterPairingMode(PODS)
        seen = []
        end = time.time() + 12
        while time.time() < end and "repaired" not in [e for e, _ in seen]:
            ev = json.loads(f.readline())
            seen.append((ev["event"], ev.get("repair")))
        s.close()
        repair_states = [r for e, r in seen if e == "repair"]
        self.assertIn("waiting", repair_states, seen)
        self.assertIn("pairing", repair_states, seen)
        self.assertIn(("repaired", None), seen)

    def test_waybar_output_while_repairing(self):
        self.lose_pairing()
        out = subprocess.run(
            [BIN, "ctl", "status"], env=self.env, capture_output=True, text=True, timeout=10
        ).stdout
        self.assertEqual(json.loads(out)["repair"], "waiting")
        wb = subprocess.Popen([BIN, "ctl", "waybar"], env=self.env, stdout=subprocess.PIPE, text=True)
        try:
            line = json.loads(wb.stdout.readline())
        finally:
            wb.terminate()
        self.assertIn("repair", line["class"])
        self.assertIn("waiting", line["class"])
        self.assertIn("pairing again", line["tooltip"])


if __name__ == "__main__":
    if not os.path.exists(BIN):
        sys.exit(f"build first: {BIN} not found")
    unittest.main(verbosity=2 if "-v" in sys.argv else 1)
