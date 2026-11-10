# Audio: speaker and microphone

No source here. The kernel side is UniXNU-droidvm's, unchanged (f9121cd): `package/DroidVMAudio.cpp` (two
`IOAudio2Device`s, "Speaker" and "Mic") over `DroidVMVirtioSnd.c` (virtio-snd: 48 kHz S16, stereo out, mono in,
240-frame periods). `DroidVMVirtioSnd.c` compiles with our kernel records.

The device it drives is `runtime/bridges/devices/virtio_snd.c` at `/arm-io/virtio-snd` (`droidvm,virtio-snd`),
backed by the AAudio helper `oneios-audio`.

Open on this side: an "Actuator" output for Core Haptics (a third device in `DroidVMAudio.cpp`, or ios-topaz's
RedwoodAudio HAL plug-in) writing `/dev/oneios-haptic` (`../haptics/oneios_haptic.c`).
