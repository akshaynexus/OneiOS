// Written with `oneios-tool kernel gen codec` (2026-11-15): no Python original.
/// DroidVMVTHost.cpp's lines that change.
///
/// The hvcC's profile, tier and level: upstream copies the twelve bytes after the SPS's first payload byte as they
/// are in the stream. Every HEVC SPS has emulation prevention bytes in those twelve (Main's compatibility flags are
/// 60 00 00 00, which the stream carries as 60 00 00 03 00, and the constraint flags' zeros get two more), so the
/// plain copy wrote them into the hvcC as flags and never reached general_level_idc (hvcC byte 12 was a flags byte,
/// 0 for a stream of MediaCodec's or x265's). The twelve bytes are taken without them.
pub const CODEC_HOST: &[(&str, &str)] = &[
    ("    memcpy(out + at, s->sps + 3, 12); at += 12;   // profile space/tier/idc, compatibility flags, constraint flags, level\n",
     concat!("    // OneiOS (kernel gen codec): the twelve bytes without the stream's emulation prevention bytes (00 00 03 -> 00 00),\n",
             "    // counted from the SPS's first payload byte; upstream copied them as they are in the stream.\n",
             "    {\n",
             "        uint32_t zeros = 0, payload = 0, taken = 0;\n",
             "        for (uint32_t i = 2; i < s->spsSize && taken < 12; ++i) {\n",
             "            const uint8_t byte = s->sps[i];\n",
             "            if (zeros >= 2 && byte == 3) { zeros = 0; continue; }\n",
             "            zeros = byte ? 0 : zeros + 1;\n",
             "            if (payload++ >= 1) out[at + taken++] = byte;   // profile space/tier/idc, compatibility flags, constraint flags, level\n",
             "        }\n",
             "        if (taken < 12) return 0;\n",
             "        at += 12;\n",
             "    }\n")),
];
