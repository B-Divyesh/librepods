#!/usr/bin/env python3
"""Read an AACP control-channel trace and print it in a form you can reason about.

Record one with the daemon:

    LIBREPODS_AACP_TRACE=/tmp/aacp.log librepods --no-tray      # or the service
    # connect the AirPods, use them until the link drops, then:
    python3 tests/sim/aacp_decode.py /tmp/aacp.log

Each line of the trace is `<epoch_ms> <TX|RX> <hex>  <label>`; the daemon has
already masked keys, serials and MAC addresses (see src/bluetooth/trace.rs).
This script labels every frame, prints the opening handshake, measures the
gap between connect and the drop, and flags the things librepods#782 is about.

    --self-test   decode the documented byte sequences and check the output
                  (no trace file or hardware needed)
"""
import argparse
import sys

# opcode (LE u16 at bytes [4:6]) → short name; mirrors src/bluetooth/trace.rs
OPCODES = {
    0x0001: "handshake", 0x0004: "battery", 0x0006: "ear-detection",
    0x0009: "control-command", 0x000D: "audio-source-req", 0x000E: "audio-source-resp",
    0x000F: "notification-register", 0x0010: "smart-routing-relay", 0x0011: "smart-routing-resp",
    0x0014: "connected-device-mac", 0x0017: "multi-undocumented", 0x0019: "stem-press",
    0x001B: "timestamp", 0x001D: "device-info", 0x001E: "rename", 0x0022: "case-info-req",
    0x0029: "host-capabilities-29", 0x002B: "paired-devices", 0x002D: "connected-devices-req",
    0x002E: "connected-devices", 0x0030: "ble-keys-req", 0x0031: "ble-keys-resp",
    0x004B: "conversation-awareness", 0x004D: "host-capabilities", 0x004F: "info-req-dead",
    0x0053: "eq-data", 0x0058: "hi-res-audio",
}


def opcode(frame):
    return int.from_bytes(frame[4:6], "little") if len(frame) >= 6 else None


def name(op):
    return OPCODES.get(op, "unknown")


class Frame:
    __slots__ = ("ms", "dir", "raw", "op")

    def __init__(self, ms, direction, raw):
        self.ms, self.dir, self.raw = ms, direction, raw
        self.op = opcode(raw)


def parse(lines):
    out = []
    for ln in lines:
        parts = ln.split()
        if len(parts) < 3 or parts[1] not in ("TX", "RX"):
            continue
        try:
            out.append(Frame(int(parts[0]), parts[1], bytes.fromhex(parts[2])))
        except ValueError:
            continue
    return out


def analyse(frames):
    """Return (report_lines, warnings)."""
    report, warn = [], []
    if not frames:
        return ["(no frames)"], ["empty trace"]

    t0 = frames[0].ms
    for f in frames:
        op = f.op
        tag = f"0x{op:04x}" if op is not None else "----"
        report.append(f"  +{(f.ms - t0) / 1000:7.2f}s {f.dir} {tag} {name(op) if op is not None else 'runt'}"
                      f"  ({len(f.raw)}B)")

    tx = [f.op for f in frames if f.dir == "TX"]
    rx = [f.op for f in frames if f.dir == "RX"]

    # the documented third-party opening order
    if not tx or tx[0] != 0x0001:
        warn.append("first TX is not the handshake (0x0001)")
    if 0x004D not in tx:
        warn.append("host-capabilities (0x004D) was never sent — the startup burst may be thin")
    elif 0x0001 in tx and tx.index(0x004D) < tx.index(0x0001):
        warn.append("host-capabilities (0x004D) sent before the handshake")
    if 0x004F in tx:
        warn.append("info-req (0x004F) was sent — documented as not working even with Apple DID; drop it")
    if 0x0029 in tx:
        warn.append("extra capability packet 0x0029 sent — uncommon; try without it (LIBREPODS_AACP_STRICT=1)")
    if 0x0030 in tx:
        warn.append("BLE-key request 0x0030 sent in-session — suspected to upset 9A348; strict mode skips it")

    # device info / startup burst actually arrived?
    if 0x001D not in rx:
        warn.append("no device-info (0x001D) received — capabilities/handshake may not have taken")

    # how long did the link last?
    span = (frames[-1].ms - t0) / 1000
    report.append("")
    report.append(f"  session length: {span:.1f}s, {len(frames)} frames "
                  f"({len(tx)} TX / {len(rx)} RX)")
    if 30 <= span <= 50:
        warn.append(f"link lasted {span:.0f}s — matches the ~40s 9A348 drop (librepods#782)")
    return report, warn


def run(path):
    with open(path) as f:
        frames = parse(f)
    report, warn = analyse(frames)
    print(f"# {path}")
    print("\n".join(report))
    if warn:
        print("\nnotes:")
        for w in warn:
            print(f"  ! {w}")
    else:
        print("\nnotes: nothing unusual")
    return 0


def self_test():
    # documented frames (docs/AAP Definitions.md, docs/host-capabilities.md)
    HANDSHAKE = "00000400010002000000000000000000"
    CAPS      = "040004004d00ff00000000000000"
    NOTIFY    = "040004000f00ffffffff"
    for hexs, op, nm in [
        (HANDSHAKE, 0x0001, "handshake"),
        (CAPS, 0x004D, "host-capabilities"),
        (NOTIFY, 0x000F, "notification-register"),
    ]:
        fr = bytes.fromhex(hexs)
        assert opcode(fr) == op, (hexs, opcode(fr), op)
        assert name(op) == nm, (nm, name(op))

    t = 1_000_000
    # a healthy opening: handshake, caps, a reply burst incl. device-info, notify
    good = [
        Frame(t, "TX", bytes.fromhex(HANDSHAKE)),
        Frame(t + 300, "TX", bytes.fromhex(CAPS)),
        Frame(t + 320, "RX", bytes.fromhex("0400040004000102")),            # battery
        Frame(t + 340, "RX", bytes.fromhex("04000400" + "1d00" + "00" * 8)),  # device-info (masked)
        Frame(t + 600, "TX", bytes.fromhex(NOTIFY)),
    ]
    _, warn = analyse(good)
    assert warn == [], warn

    # a 9A348-shaped session: extra 0x29 + 0x30, no device-info, ~40s then drop
    bad = [
        Frame(t, "TX", bytes.fromhex(HANDSHAKE)),
        Frame(t + 300, "TX", bytes.fromhex(CAPS)),
        Frame(t + 600, "TX", bytes.fromhex("0400040029 00ffffffffffffffff".replace(" ", ""))),
        Frame(t + 900, "TX", bytes.fromhex("0400040030000300".replace(" ", ""))),
        Frame(t + 40_000, "RX", bytes.fromhex("0400040004000102")),
    ]
    _, warn = analyse(bad)
    joined = " ".join(warn)
    for expect in ("0x0029", "0x0030", "device-info"):
        assert expect in joined, (expect, warn)
    assert "40s" in joined, warn
    print("self-test: ok")
    return 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("trace", nargs="?")
    ap.add_argument("--self-test", action="store_true")
    a = ap.parse_args()
    if a.self_test:
        return self_test()
    if not a.trace:
        ap.error("give a trace file, or --self-test")
    return run(a.trace)


if __name__ == "__main__":
    sys.exit(main())
