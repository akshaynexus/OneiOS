# Network: en0, the phone's Wi-Fi

`oneios_wifi_proto.h`: the station's messages between the guest's Wi-Fi driver and xnuhost's Wi-Fi bridge (scan,
join, leave; state, networks, results). They travel on the virtio-net queues as Ethernet frames of type 0x88b5 to
and from the address `02:4f:57:49:46:49`; neither end lets such a frame out (to the radio, or to iOS's network
stack). Shared by both ends, so it is here and not beside either.

The driver is not built by `drivers/build.py`: it subclasses the kernel collection's own `IOEthernetController`
and `IOEthernetInterface`, so it is an archive of the hosted kernel, `kernel/patches/oneios-hosted/net`
(`OneiOSWifi.cpp`, its README), with UniXNU-droidvm's transport `package/DroidVMVirtioNet.c` (f9121cd). The host's
side: `runtime/bridges/wifi` and `runtime/bridges/devices/virtio_net.c` at `/arm-io/net` (`droidvm,net`; the
transport reads its interrupt from the node's `droidvm,intid`). The design and what was tested: `docs/hardware.md`
section 3.
