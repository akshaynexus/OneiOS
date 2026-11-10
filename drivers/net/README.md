# Network: en0

No source here. The kernel side is UniXNU-droidvm's, unchanged (f9121cd): `package/DroidVMVirtioNet.c` (transport),
`DroidVMVirtioEthernet.cpp` and `DroidVMVirtioNetIf.cpp` (the `IOEthernetController` on IONetworkingFamily),
`DroidVMWifi.inc` (a Wi-Fi interface for wifid and Settings), built by its
`ipados27/scripts/build-droidvm-devices.py`. `DroidVMVirtioNet.c` compiles with our kernel records
(`-DDROIDVM_DEVICE_DYNAMIC=1`).

The device it drives is `runtime/bridges/devices/virtio_net.c` at `/arm-io/net` (`droidvm,net`). The transport reads
its interrupt from the node's `droidvm,intid` property: booter's tree writes `interrupts`, so either the tree adds
`droidvm,intid` or the driver polls every 10 ms (it works both ways).

Not written: a bridge from One UI's Wi-Fi state (SSID, signal) into `DroidVMWifi.inc`'s station.
