"""python-dbusmock template: BlueZ with a pair of fake AirPods.

Builds on dbusmock's bluez5 template and adds what AirPods do that matters
to LibrePods' connection handling, driven through org.bluez.Mock:

  AddAirPods(adapter, address, name)   paired + trusted AirPods (AACP UUID)
  ForgetHost(address)                  the AirPods drop their half of the
                                       bond: Connect fails with
                                       br-connection-key-missing
  EnterPairingMode(address)            case button held: an inquiry finds
                                       them (re-added while discovering) and
                                       Pair works once
  PageUs(address)                      they try to reconnect to us on their
                                       own (device object reappears without
                                       being in pairing mode)
  SetConnected(address, bool)          AirPods-side connect/disconnect

Run it on a private system bus (see run_sim.py); never on the real one.
"""

import dbus
from dbusmock import mockobject
from dbusmock.templates import bluez5
from dbusmock.templates.bluez5 import *  # noqa: F401,F403  (re-export the bluez5 mock API)

BUS_NAME = bluez5.BUS_NAME
MAIN_OBJ = bluez5.MAIN_OBJ
SYSTEM_BUS = True
IS_OBJECT_MANAGER = True

AACP_UUID = "74ec2172-0bad-4d01-8f77-997b2be0722a"
AUDIO_UUIDS = [
    "0000110b-0000-1000-8000-00805f9b34fb",  # A2DP sink
    "0000110e-0000-1000-8000-00805f9b34fb",  # AVRCP
    "0000111e-0000-1000-8000-00805f9b34fb",  # HFP
    AACP_UUID,
]

# Scenario state per AirPods address; survives RemoveDevice like the real
# AirPods' memory survives BlueZ forgetting them.
PODS = {}


def load(mock, parameters):
    bluez5.load(mock, parameters)


def _path(adapter, address):
    return f"/org/bluez/{adapter}/dev_" + address.replace(":", "_").upper()


def _find(address):
    for path, obj in mockobject.objects.items():
        if path.endswith("/dev_" + address.replace(":", "_").upper()):
            return path, obj
    return None, None


def _emit_props(obj, props):
    obj.UpdateProperties(bluez5.DEVICE_IFACE, props)


def _airpods_connect(device):
    address = str(device.props[bluez5.DEVICE_IFACE]["Address"])
    st = PODS.get(address, {})
    if not st.get("host_key", True) or not device.paired:
        raise dbus.exceptions.DBusException("br-connection-key-missing", name="org.bluez.Error.Failed")
    if device.connected:
        raise dbus.exceptions.DBusException("Already Connected", name="org.bluez.Error.AlreadyConnected")
    device.connected = True
    _emit_props(device, {"Connected": dbus.Boolean(True)})


def _airpods_pair(device):
    address = str(device.props[bluez5.DEVICE_IFACE]["Address"])
    st = PODS.setdefault(address, {})
    if device.paired and st.get("host_key", True):
        raise dbus.exceptions.DBusException("Already Exists", name="org.bluez.Error.AlreadyExists")
    if not st.get("pairing_mode"):
        raise dbus.exceptions.DBusException("Page Timeout", name="org.bluez.Error.ConnectionAttemptFailed")
    st["pairing_mode"] = False
    st["host_key"] = True
    st["pair_count"] = st.get("pair_count", 0) + 1
    device.paired = True
    _emit_props(
        device,
        {
            "Paired": dbus.Boolean(True),
            "Bonded": dbus.Boolean(True),
            "UUIDs": dbus.Array(AUDIO_UUIDS, signature="s"),
        },
    )


def _add_device(mock_self, adapter, address, name, paired):
    path = bluez5.AddDevice(mock_self, adapter, address, name)
    dev = mockobject.objects[path]
    dev.AddMethods(
        bluez5.DEVICE_IFACE,
        [
            ("Connect", "", "", _airpods_connect),
            ("Pair", "", "", _airpods_pair),
        ],
    )
    dev.AddProperty(bluez5.DEVICE_IFACE, "Bonded", dbus.Boolean(paired))
    props = {
        "Icon": dbus.String("audio-headphones"),
        "Class": dbus.UInt32(0x240418),
        "Modalias": dbus.String("bluetooth:v004Cp2014d0000"),
    }
    if paired:
        dev.paired = True
        props.update(
            {
                "Paired": dbus.Boolean(True),
                "Trusted": dbus.Boolean(True),
                "UUIDs": dbus.Array(AUDIO_UUIDS, signature="s"),
            }
        )
    _emit_props(dev, props)
    PODS.setdefault(address, {}).setdefault("adapter", adapter)
    return path


@dbus.service.method(bluez5.BLUEZ_MOCK_IFACE, in_signature="sss", out_signature="s")
def AddAirPods(self, adapter, address, name):
    PODS[address] = {"adapter": adapter, "host_key": True, "pairing_mode": False, "pair_count": 0}
    return _add_device(self, adapter, address, name, paired=True)


@dbus.service.method(bluez5.BLUEZ_MOCK_IFACE, in_signature="s", out_signature="")
def ForgetHost(self, address):
    PODS.setdefault(address, {})["host_key"] = False
    _, dev = _find(address)
    if dev is not None and dev.connected:
        dev.connected = False
        _emit_props(dev, {"Connected": dbus.Boolean(False)})


def _adapter_discovering(adapter):
    obj = mockobject.objects.get(f"/org/bluez/{adapter}")
    return bool(obj and obj.props[bluez5.ADAPTER_IFACE]["Discovering"])


@dbus.service.method(bluez5.BLUEZ_MOCK_IFACE, in_signature="s", out_signature="b")
def EnterPairingMode(self, address):
    """Returns True if an ongoing inquiry found them right away."""
    st = PODS.setdefault(address, {})
    st["pairing_mode"] = True
    adapter = st.get("adapter", "hci0")
    path, _ = _find(address)
    if path is None and _adapter_discovering(adapter):
        _add_device(self, adapter, address, "AirPods Pro", paired=False)
        return True
    return False


@dbus.service.method(bluez5.BLUEZ_MOCK_IFACE, in_signature="s", out_signature="")
def PageUs(self, address):
    st = PODS.setdefault(address, {})
    path, _ = _find(address)
    if path is None:
        _add_device(self, st.get("adapter", "hci0"), address, "AirPods Pro", paired=False)


@dbus.service.method(bluez5.BLUEZ_MOCK_IFACE, in_signature="sb", out_signature="")
def SetConnected(self, address, connected):
    _, dev = _find(address)
    if dev is None:
        raise dbus.exceptions.DBusException("no such device", name="org.bluez.Mock.NoSuchDevice")
    dev.connected = bool(connected)
    _emit_props(dev, {"Connected": dbus.Boolean(bool(connected))})


@dbus.service.method(bluez5.BLUEZ_MOCK_IFACE, in_signature="s", out_signature="a{sv}")
def PodsState(self, address):
    st = PODS.get(address, {})
    path, dev = _find(address)
    out = {
        "present": dbus.Boolean(path is not None),
        "host_key": dbus.Boolean(st.get("host_key", True)),
        "pairing_mode": dbus.Boolean(st.get("pairing_mode", False)),
        "pair_count": dbus.Int32(st.get("pair_count", 0)),
    }
    if dev is not None:
        p = dev.props[bluez5.DEVICE_IFACE]
        out.update(
            {
                "paired": dbus.Boolean(dev.paired),
                "trusted": dbus.Boolean(bool(p.get("Trusted", False))),
                "connected": dbus.Boolean(dev.connected),
            }
        )
    return out


# A real inquiry keeps finding a case in pairing mode: re-add it whenever a
# discovery starts while it is in pairing mode and BlueZ has no record.
def _start_discovery(adapter):
    bluez5.StartDiscovery(adapter)
    name = adapter.__dbus_object_path__.rsplit("/", 1)[-1]
    for address, st in PODS.items():
        if st.get("pairing_mode") and st.get("adapter", "hci0") == name and _find(address)[0] is None:
            _add_device(mockobject.objects["/"], name, address, "AirPods Pro", paired=False)


@dbus.service.method(bluez5.BLUEZ_MOCK_IFACE, in_signature="ss", out_signature="s")
def AddAdapter(self, device_name, system_name):
    path = bluez5.AddAdapter(self, device_name, system_name)
    mockobject.objects[path].AddMethods(bluez5.ADAPTER_IFACE, [("StartDiscovery", "", "", _start_discovery)])
    return path
