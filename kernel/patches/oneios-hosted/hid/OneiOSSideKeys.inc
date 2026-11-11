// SPDX-License-Identifier: MIT
// OneiOS: the phone's buttons, appended to the staged DroidVMVirtioInput.c by build-oneios-hid.py.
//
// xnuhost's virtio-input device carries the touch screen's events and the side keys' (runtime/xnuhost/src/input.c:
// evdev codes unchanged, read without a grab, so the switch's Volume Up + Volume Down watcher sees them too).
// input_event() hands every EV_KEY here first. The pressed set goes to NativeHidInput's button sink as the bits of
// its report 2:
//   bit 0  Consumer / Menu (Home)             KEY_HOMEPAGE (172)   no such key on the S24 Ultra: for a host that has one
//   bit 1  Consumer / Power                   KEY_POWER (116)
//   bit 2  Consumer / Volume Increment        KEY_VOLUMEUP (115)
//   bit 3  Consumer / Volume Decrement        KEY_VOLUMEDOWN (114)
//   bit 4  Telephony / Ring Enable            KEY_MUTE (113): each press flips the switch; oneios_hid_ringer_set()
//                                             sets it. The S24 Ultra has no ringer switch either.
// Whether iOS 27 reads Ring Enable = 1 as "ring" is not checked on this build (a guess from older releases).
enum { ONEIOS_KEY_MUTE = 113, ONEIOS_KEY_VOLUMEDOWN = 114, ONEIOS_KEY_VOLUMEUP = 115, ONEIOS_KEY_POWER = 116, ONEIOS_KEY_HOMEPAGE = 172 };
enum { ONEIOS_BUTTON_HOME = 1, ONEIOS_BUTTON_POWER = 2, ONEIOS_BUTTON_VOLUME_UP = 4, ONEIOS_BUTTON_VOLUME_DOWN = 8, ONEIOS_BUTTON_RINGER = 16 };

static void (*oneios_buttons_sink)(void *, uint32_t);
static void *oneios_buttons_sink_context;
static uint32_t oneios_buttons_mask;
// For the log (OneiOSHidInput.cpp): key events taken, and button states handed to the sink.
volatile uint64_t oneios_hid_key_events, oneios_hid_button_reports;

static void oneios_buttons_publish(void) {
    void (*sink)(void *, uint32_t) = __atomic_load_n(&oneios_buttons_sink, __ATOMIC_ACQUIRE);
    if (!sink) return;
    sink(oneios_buttons_sink_context, oneios_buttons_mask);
    oneios_hid_button_reports = oneios_hid_button_reports + 1;
}

// NativeHidInput's buttons device installs its sink here when it starts (and removes it when it stops).
void native_buttons_set_sink(void (*sink)(void *, uint32_t), void *context) {
    oneios_buttons_sink_context = context;
    __atomic_store_n(&oneios_buttons_sink, sink, __ATOMIC_RELEASE);
}

// The ringer switch from somewhere else than a key (a bridge message, when a host has such a control).
void oneios_hid_ringer_set(bool ring) {
    oneios_buttons_mask = ring ? oneios_buttons_mask | ONEIOS_BUTTON_RINGER : oneios_buttons_mask & ~(uint32_t)ONEIOS_BUTTON_RINGER;
    oneios_buttons_publish();
}

static bool oneios_has_side_keys(const uint8_t *keys, unsigned size) {
    return input_bit(keys, size, ONEIOS_KEY_POWER) || input_bit(keys, size, ONEIOS_KEY_VOLUMEUP) ||
           input_bit(keys, size, ONEIOS_KEY_VOLUMEDOWN);
}

// True when the event was one of the buttons' (value 2, the autorepeat of a held key, changes nothing).
static bool oneios_side_key(unsigned code, int32_t value) {
    uint32_t bit;
    switch (code) {
    case ONEIOS_KEY_HOMEPAGE: bit = ONEIOS_BUTTON_HOME; break;
    case ONEIOS_KEY_POWER: bit = ONEIOS_BUTTON_POWER; break;
    case ONEIOS_KEY_VOLUMEUP: bit = ONEIOS_BUTTON_VOLUME_UP; break;
    case ONEIOS_KEY_VOLUMEDOWN: bit = ONEIOS_BUTTON_VOLUME_DOWN; break;
    case ONEIOS_KEY_MUTE:
        if (value == 1) {
            oneios_hid_key_events = oneios_hid_key_events + 1;
            oneios_hid_ringer_set(!(oneios_buttons_mask & ONEIOS_BUTTON_RINGER));
        }
        return true;
    default: return false;
    }
    if (value == 2) return true;
    oneios_hid_key_events = oneios_hid_key_events + 1;
    oneios_buttons_mask = value ? oneios_buttons_mask | bit : oneios_buttons_mask & ~bit;
    oneios_buttons_publish();
    return true;
}
