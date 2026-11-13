# Audio: speaker and microphone

No source here. The kernel side is UniXNU-droidvm's (f9121cd): `package/DroidVMAudio.cpp` (two `IOAudio2Device`s,
"Speaker" and "Mic") over `DroidVMVirtioSnd.c` (virtio-snd: 48 kHz S16, stereo out, mono in, 240-frame periods).
They are built into the hosted kernel by its audio archive, `oneios-tool kernel gen audio`
(since 2026-11-12; before, they were not built at all and iOS had no audio device): the transport unchanged,
`DroidVMAudio.cpp` staged with boot arguments for its UIDs and its output latency, and `OneiOSAudio.c`, the start
that `oneios_drivers_start` calls.

The device it drives is `runtime/bridges/devices/virtio_snd.c` at `/arm-io/virtio-snd` (`droidvm,virtio-snd`),
backed by the AAudio helper `oneios-audio`. Everything else (boot arguments, log lines, measurements):
`docs/hardware.md` section 6.

Since 2026-11-12 (media) the archive also stages a third device, UID "Actuator" (Core Haptics, UI and keyboard
taps): mono, the speaker's engine, its periods to `../haptics/oneios_haptic.c` (`oneios_haptic_actuator_play`), not to
virtio-snd. `docs/hardware.md` section 5.
