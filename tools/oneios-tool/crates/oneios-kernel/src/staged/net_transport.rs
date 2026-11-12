// From build-oneios-net.py (stage_transport), written out by a helper of the port and kept by hand since.
/// DroidVMVirtioNet.c's line that changes.
pub const NET_TRANSPORT: &[(&str, &str)] = &[
    ("            if (broadcast || ours) {\n",
     "            if (broadcast || ours || (dst[0] & 1)) {   // OneiOS (build-oneios-net.py): multicast too\n"),
];
