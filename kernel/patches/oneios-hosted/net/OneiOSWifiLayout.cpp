// SPDX-License-Identifier: BSD-2-Clause-Patent
//
// OneiOS: IONetworkInterface::setInterfaceSubType for the Wi-Fi interface (OneiOSWifi.cpp). The family's function is
// not virtual, so it has no slot to bind; this does what it does (IONetworkingFamily-188, IONetworkInterface.cpp:
// refused once the interface has its ifnet, else the subtype is kept in the expansion data, which BSD attach passes
// to ifnet_allocate_extended as the subfamily) with the family's own member layout: its header with the private
// members visible. The layout is the collection's: the class's instance size is checked here and its vtable slot by
// slot by build-oneios-net.py.
#define private public
#include <IOKit/network/IONetworkInterface.h>
#include <IOKit/network/IOEthernetInterface.h>
#include <IOKit/network/IOEthernetController.h>
#undef private

static_assert(sizeof(IONetworkInterface) == 0x110 && sizeof(IOEthernetInterface) == 0x148 && sizeof(IOEthernetController) == 0x118,
              "IONetworkingFamily-188's layout is the 24A446 collection's (IONetworking-24A446-v54.json)");

extern "C" bool oneios_net_set_subtype(IONetworkInterface *interface, uint32_t subtype) {
    if (!interface || interface->_backingIfnet || !interface->_reserved) return false;
    interface->_reserved->subType = subtype;
    return true;
}
