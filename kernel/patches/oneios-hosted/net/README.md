# kernel/patches/oneios-hosted/net: the Wi-Fi, en0 on the phone's radio

Owner: radio, then radio2 (2026-11-12). One archive of the hosted kernel, `build/oneios-net/liboneiosnet.a`, hooked
into `kernel/build-hosted.sh` like `hid/`. There is no numbered patch: the start, `oneios_net_start`, is called from
`oneios_drivers_start` (`drivers/bridge/OneiOSBridge.c`, a weak default there). The design, the host's side and what
was tested: `docs/hardware.md` section 3.

```
 iOS: wifid (Apple80211 requests on en0, IEEE 802.11 kernel events), configd (DHCP, IPv6, DNS)
   -> IO80211Interface / IO80211Controller / OneiOSWifiController        OneiOSWifi.cpp, this archive
      under the collection's IOEthernetInterface / IOEthernetController  Apple's IONetworkingFamily, unchanged, in kernel.kc
   -> DroidVMVirtioNet.c (staged)                                         the transport, in the kernel
   -> virtio-net (/arm-io/net)                                            runtime/bridges/devices/virtio_net.c
   -> runtime/bridges/wifi: frames to and from wlan0, the station's messages to nl80211 and the vendor's supplicant
```

## Files

| File | What |
|---|---|
| `OneiOSWifi.cpp` | The classes, the Apple80211 requests and events, the station (scan, join, leave: messages to the host), frames in and out, `oneios_net_start` |
| `OneiOSWifiLayout.cpp` | `IONetworkInterface::setInterfaceSubType`'s store (the Wi-Fi subfamily). That function is not virtual, so it has no slot to bind it by; this file does what it does with the family's own member layout (the header with its private members visible; the classes' sizes are asserted) |
| `oneios-tool kernel layout net` (was `layout.py` here) | Reads the two original classes from the kernelcache into `IONetworking-24A446-v54.json` (run by hand; needs `out/kernel-inputs/24A446/kernelcache.macho` and a `kernel.kc.prelink.json`) |
| `IONetworking-24A446-v54.json` | What the kernelcache says of `IOEthernetController` (0x118 bytes, 250 slots) and `IOEthernetInterface` (0x148 bytes, 221 slots): metaclass, vtable, each slot's target, pointer-authentication discriminator and whether the target is the family's code, the constructor, the base destructor. Addresses and numbers only: no Apple code, so it is committed |
| `oneios-tool kernel gen net` | Fetches IONetworkingFamily-188's headers, checks them against the record, compiles the sources, generates the bindings, makes the archive |
| `drivers/net/oneios_wifi_proto.h` | The station's messages (shared with `runtime/bridges/wifi`) |

## Why the original classes, and how they are checked

Apple's `com.apple.iokit.IONetworkingFamily` is a root of the collection, and 25 of the 130 roots link it (the
Skywalk family, the USB network classes, IOUserEthernet and others). Droid-VM, on the same 24A446 kernelcache,
compiles IONetworkingFamily-188 into its kernel and leaves Apple's copy out of its collection; here that would take
those 25 roots out too. So the Wi-Fi classes subclass the collection's own `IOEthernetController` and
`IOEthernetInterface`, as `hid/` does with `IOHIDDevice`, and the collection is not changed.

They are compiled against the headers of
[IONetworkingFamily-188](https://github.com/apple-oss-distributions/IONetworkingFamily) (APSL 2.0, commit `bb910e2`,
cloned once into `out/sources/IONetworkingFamily`) with `__PRIVATE_SPI__`, which is how Apple builds the family
(it changes `IONetworkController`'s and `IONetworkInterface`'s layout). That these headers describe the classes of
the iPhone's kernelcache is checked at every build, before anything else is compiled: a probe subclass of each
parent is compiled and its vtable is compared with the record, slot by slot: the number of slots (250 and 221), each
slot's discriminator (it is the hash of the mangled name and so of the method's name and argument types), and
whether the slot's function is the family's or the kernel's. One difference stops the build with the slot's name.
The instance sizes are asserted in `OneiOSWifiLayout.cpp` and compared with the live metaclasses at the start.

Calls into the family go through generated stubs (`bti c; b <name>.original`, the target an absolute symbol at the
function's address in the collection, which XnuPrelink's emitter retargets; `kernel/prelink/stubs-check.py` checks
every one in `kernel.kc`): 159 functions on 2026-11-12, all of them virtual functions bound by their slot, the two
constructors and the two base destructors. **Nothing calls a non-virtual function of the family**: such a function
has no slot, so its address would have to be found by hand. The build fails with the name when a source starts to
use one. What that rules out is replaced as follows: `IONetworkMedium::medium()` and `addMedium()` by
`OSMetaClass::allocClassWithName("IONetworkMedium")`, the virtual `init` and `publishMediumDictionary`;
`setInterfaceSubType` by `OneiOSWifiLayout.cpp`; `OSDynamicCast` to a family class is not used.

The start (`register_classes`): loads `com.apple.iokit.IONetworkingFamily`, compares the live metaclasses with the
bound ones (address and instance size; another collection stops here with one line), then runs the archive's static
initializer inside the kernel's own module load and sets the two superclass links, as `hid/` does.

## The medium: what makes en0 a Wi-Fi interface for IO80211.framework

24A446's IO80211.framework (`__getIfListCopy`) lists the Wi-Fi interfaces by `SIOCGIFMEDIA`: the current medium's
type must be IEEE 802.11. The family answers that request from the controller's selected and active medium, so the
controller calls `setSelectedMedium` with its one medium (`kIOMediumIEEE80211Auto`) at its start and gives the same
medium to every `setLinkStatus`, link up or down. Without it the framework's list is empty, wifid does not take the
interface, and nothing ever asks for a scan (the first phone session). `docs/hardware.md` section 3 has the evidence.

## Boot argument and log lines

`oneios-wifi=0` leaves the Wi-Fi out. The lines (`ONEIOS WIFI: ...`), in the order of a good boot: `classes
registered below the collection's IOEthernetController and IOEthernetInterface; starting /arm-io/net`, `on: the
phone's Wi-Fi, address ..; asking the host for its radio`, `the host has the phone's radio`, `the interface is enabled (en0 is up)`, `event N for en0, M bytes -> error` (the
first 24 events posted to the daemon, and any that fails: 10 is SCAN_DONE, 9 ASSOC_DONE, 4 LINK_CHANGED), `NAME[PID] gets|sets N: len .. val .. ->
error` (each Apple80211 request with the process that made it, for the first 150, then a type's first, and every
scan, join and leave), `NAME[PID]: interface request 0x..` (the first 24 other requests on en0), `NAME[PID] asks the
interface for a user client of type 0x.. -> 0x..` (the first 8), `scan
done: N networks`, `join: layout 656, authentication lower 0x.. upper 0x.., key of N bytes of kind K` (the request's
length, 656 on 24A446, picks its layout), `join: a network name of N bytes, a passphrase|a PSK|open`, `join done:
joined` or `join failed (status S, 802.11 code C)`, `link down (reason R)`. An `off: ...` line names what was missing. For a debugger:
`oneios_wifi_stage` (1 the family loaded, 2 the classes are the bound ones, 3 registered, 4 the controller
started), `oneios_wifi_requests`, `oneios_wifi_refused`, `oneios_wifi_rx`, `oneios_wifi_tx`, `oneios_wifi_dropped`.

## State

Built on the Mac (`HOSTED_OUT=radio`) and prelinked; the collection passes the import audit and the stub check.
What a boot on the phone showed: `docs/hardware.md` section 3, "Tested".
