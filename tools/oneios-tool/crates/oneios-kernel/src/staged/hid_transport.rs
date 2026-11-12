// From build-oneios-hid.py (stage_transport), written out by a helper of the port and kept by hand since.
/// DroidVMVirtioInput.c's lines that change.
pub const HID_TRANSPORT: &[(&str, &str)] = &[
    ("struct droidvm_input_desc { uint64_t address;",
     concat!("// OneiOS (build-oneios-hid.py): a side key's event, OneiOSSideKeys.inc at the end of this file; a touch\n",
             "// frame's scan time, OneiOSTouchTime.inc after it.\n",
             "static bool oneios_side_key(unsigned code, int32_t value);\n",
             "static void oneios_touch_scan(int32_t value);\n",
             "static void oneios_touch_frame_begin(void);\n",
             "static void oneios_touch_frame_end(void);\n",
             "struct droidvm_input_desc { uint64_t address;")),
    // backboardd drops every touch whose HID Contact Identifier is 30 or more ("NN >= MAX_PATH_COUNT", iOS's own
    // log, 2026-11-12: 8,711 drops in one session). The source gives 1 + tracking id % 120, and Linux's tracking
    // ids grow with every contact for as long as the phone runs, so most touches, and at times all of them, were
    // dropped. A finger gets the next free identifier of 1..28 instead (rotating, so the next finger of a slot
    // still gets another one); 29 is the pointer's.
    ("                uint8_t id = s < DROIDVM_INPUT_SLOTS ? (uint8_t)(1 + (uint32_t)d->slots[s].id % 120) : 127;\n",
     concat!("                // OneiOS: backboardd takes identifiers below 30 only (MAX_PATH_COUNT); the next free one of 1..28.\n",
             "                uint8_t id = 29;\n",
             "                if (s < DROIDVM_INPUT_SLOTS) {\n",
             "                    static uint8_t oneios_next_id = 1;\n",
             "                    for (unsigned tries = 0; tries < 28; ++tries) {\n",
             "                        uint8_t c = oneios_next_id;\n",
             "                        oneios_next_id = c >= 28 ? 1 : (uint8_t)(c + 1);\n",
             "                        bool used = false;\n",
             "                        for (unsigned j = 0; j < n && !used; ++j) used = next[j].id == c;\n",
             "                        if (!used) { id = c; break; }\n",
             "                    }\n",
             "                }\n")),
    ("    if (e->type == EV_SYN) return e->code == SYN_REPORT;\n",
     concat!("    if (e->type == EV_SYN) return e->code == SYN_REPORT;\n",
             "    if (e->type == EV_KEY && oneios_side_key(e->code, e->value)) return false;   // OneiOS: any device's side keys\n",
             "    if (e->type == 4 && e->code == 5) { oneios_touch_scan(e->value); return false; }   // OneiOS: EV_MSC / MSC_TIMESTAMP\n")),
    ("                else touch_frame();\n",
     "                else { oneios_touch_frame_begin(); touch_frame(); oneios_touch_frame_end(); }   // OneiOS: the scan time\n"),
    // A device with the side keys and no touch screen (xnuhost found no panel) is still read.
    (concat!("        input_bit(abs, nabs, ABS_Z))\n",
             "        return KIND_MOTION;\n",
             "    return KIND_NONE;\n"),
     concat!("        input_bit(abs, nabs, ABS_Z))\n",
             "        return KIND_MOTION;\n",
             "    if (oneios_has_side_keys(keys, nkeys)) return KIND_KEYBOARD;   // OneiOS\n",
             "    return KIND_NONE;\n")),
    ("static bool input_bit(const uint8_t *bits, unsigned size, unsigned bit) {\n",
     concat!("static bool oneios_has_side_keys(const uint8_t *keys, unsigned size);\n",
             "static bool input_bit(const uint8_t *bits, unsigned size, unsigned bit) {\n")),
];
