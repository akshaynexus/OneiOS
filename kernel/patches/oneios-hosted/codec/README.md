# codec/: the video encoder and decoder (hevc-encoder, hevc-guest, 2026-11-15)

Owner: the encoder agent. The host side: `runtime/bridges/encoder`. The whole path and the route: `docs/hardware.md`,
section 7g ("Video encoder").

**State (2026-11-15, hevc-guest):** the archive is **built into `kernel.kc`** by `oneios-tool kernel gen codec`
(`kernel/build-hosted.sh` runs it after `audio`), and a PC boot with `--set encoder=auto` publishes the service.
Built and booted on the Linux PC; **not run on the phone** (the commands are in `runtime/bridges/encoder/README.md`,
"On the phone"). The results of the build and of the PC boots are at the end of this file.

## Why this route

iOS's VideoToolbox in 24A446 carries a whole **paravirtualized** encoder and decoder
(`ParavirtualizedVideoEncoder_*`, read in the 24A446 shared cache) and uses them for every eligible codec once
`VTParavirtualizationIsRunningInGuest` — i.e. once the service **`AppleVideoToolboxParavirtualizationDriver`** is
published (string in `VideoToolboxParavirtualizationSupport` 71.0.14). CMPhoto's HEIC writer takes that path: its
`_CMPhotoSupportsHEVCHWEncode` block calls `VTSelectAndCreateVideoEncoderInstance` with
`kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder` true and reports HEVC hardware support when a
hardware encoder answers (disassembled, 2026-11-15). The paravirtualized encoder answers
`UsingHardwareAcceleratedVideoEncoder` true, so CMPhoto sees hardware HEVC and `CMPhotoCompressionSessionAddImage`
with `codec: 'hvc1'` stops failing.

So the camera's plug-in route is **not** used here: no AVE2 user-client emulation (AVE2's firmware command ABI is
Apple's and not serveable by another encoder), no `.videoencoder` bundle of ours. We publish Apple's own
paravirtualization service, exactly as the Metal and camera plug-ins are iOS-side code built on Apple's own loaders.
[F] the cache reads; [I] that iOS's encode graph takes the device as it is (a session on the phone confirms).

## The sources (UniXNU-droidvm f9121cd, `external/UniXNU-droidvm/package`)

| File | What |
|---|---|
| `DroidVMVirtioMedia.c` | the virtio-media transport (V4L2 stateful codec over virtio-mmio): `/arm-io/virtio-media-0`, `media-host-pool`. The device is xnuhost's (`runtime/bridges/encoder/media_device.c`), not crosvm's. Unchanged, `-DDROIDVM_DEVICE_DYNAMIC=1`, Mach universe, as its iPadOS 27 board builds it |
| `DroidVMVTHost.cpp` | the session logic: `encs`/`+enc`/`esta`/`efrm`/`Eemt`/`ecfr`; the IOSurface lookup (IOCoreSurfaceRoot), NV12 copy into the OUTPUT buffer, hvcC/avcC assembly, the sample-buffer atoms. Runs in the guest kernel over the virtio-media device. **Staged with one edit** (below) |
| `DroidVMVTParavirt.cpp` | Apple's `AppleVideoToolboxParavirtualizationDriver` user client + the `IODataQueue` transport; publishes the service with `droidvm-vt-paravirt=1`. Unchanged; its static initializer is taken out of `__mod_init_func` and made global (`droidvm_vt_paravirt_attach` runs it in its own module load), as the audio archive's |

`OneiOSCodec.c` (this folder, OneiOS's own) is the start: `oneios_codec_start` → `droidvm_media_start()` +
`droidvm_vt_paravirt_attach()`, the start line, and the report thread.

**The one edit** (`tools/oneios-tool/crates/oneios-kernel/src/staged/codec.rs`): the hvcC's twelve bytes of profile,
tier and level. Upstream copies them from the SPS as the stream has them (`memcpy(out + at, s->sps + 3, 12)`). Every
HEVC SPS has emulation prevention bytes in exactly those bytes (Main's compatibility flags `60 00 00 00` are
`60 00 00 03 00` in the stream, and the constraint flags' zeros get two more), so the copy put `03` bytes into the
flags and never reached `general_level_idc`: the hvcC said level 0 for any real encoder's stream. The staged copy
takes the twelve bytes without them. [F] read from the code and the standard; the stand-in's own SPS has the same
three bytes (`runtime/bridges/encoder/tests/test_standin.c` checks its first two).

## The generator and the build

`oneios-tool kernel gen codec` (`tools/oneios-tool/crates/oneios-kernel/src/build_oneios_codec.rs`; written in Rust,
no Python original) is one entry of the hosted build's board archives, after `audio`:

- compiles `DroidVMVirtioMedia.c` and `OneiOSCodec.c` with the first kernel pass's Mach record, the staged
  `DroidVMVTHost.cpp` and `DroidVMVTParavirt.cpp` with its IOKit record (`-I` the package folder);
- makes `DroidVMVTParavirt.late.o` (no `__DATA,__mod_init_func`, `__GLOBAL__sub_I_DroidVMVTParavirt.cpp` global);
- archives `build/oneios-codec/liboneioscodec.a` (4 objects) and requires `_oneios_codec_start`,
  `_droidvm_media_start`, `_droidvm_vt_paravirt_attach`, `__GLOBAL__sub_I_DroidVMVTParavirt.cpp`,
  `_droidvm_vthost_available`, `_droidvm_vthost_message`, `_droidvm_media_ioctl`, `_droidvm_media_mmap`, and that
  neither C++ object still has a `__mod_init_func` section;
- writes `objects.json` (each source with its SHA-256).

`kernel/build-hosted.sh` adds `-Wl,-force_load,{tree}/build/oneios-codec/liboneioscodec.a` to
`CONFIG_XNU_EXTRA_LD_ARCHIVES` (after `audio`'s). `ONEIOS_CODEC_RECORDS` and `ONEIOS_CODEC_OUT` name another record
folder and output folder: the generator against a finished build's tree, without a kernel build.

Tests: `cargo test -p oneios-kernel --release --test golden` (`the_codec_edit_writes_its_frozen_text_and_fits_the_
pinned_source`: the edit list on a made-up source against `tests/kernelgen/golden/synthetic/codec.out`, and on the
real file); `tools/oneios-tool/tests/kernelgen/compare-golden.sh WORK` after a build (the staged file's hash, the
generator's line).

## Boot arguments and log lines

| Boot argument | What |
|---|---|
| `oneios-codec=0` | nothing of this archive starts (`ONEIOS CODEC: off (oneios-codec=0)`) |
| `droidvm-media=0` | no transport (no device is looked for) |
| `droidvm-vt-paravirt=1` | publish the service. xnuhost adds it when `encoder=auto` or `encoder=sim` gave the guest a device. Without it the classes are registered (stage 1) and root can publish later: `sysctl kern.droidvm_vt_paravirt=1` |
| `oneios-codec-report=0` | no report thread |

- At start, always: `ONEIOS CODEC: N virtio-media device(s), M up; VideoToolbox paravirtualization registered (stage
  2)` with a device and `droidvm-vt-paravirt=1`; `... not published (stage 1)` without the boot argument or without a
  device (a kernel booted with `encoder=off`: `0 virtio-media device(s), 0 up`).
- With a device, 3 s later: `ONEIOS CODEC: device 0: oneios-encoder caps 0x4004000 out: NV12 cap: HEVC H264` (the
  transport's own start-up check: a session opened, each queue's formats listed, the session closed).
- Every 30 s while a figure moves (nothing while nobody uses the codec): `ONEIOS CODEC: counters: service opened N
  closed N, messages N (refused N, unknown N), replies N; sessions N, frames in N out N, errors N (last: step S value
  V); virtio-media commands N (timeouts N), events N errors N buffers N other`, then `ONEIOS CODEC: session: ...` for
  each line `DroidVMVTHost.cpp` wrote into its trace ring since (a session's size and codec, the IOSurface lookups).
  `step S` is the stage `DroidVMVTHost.cpp` gives `failed()`: 90-96 the session's start (90 OPEN, 91/92 S_FMT, 93/94
  REQBUFS, 95/96 STREAMON), 10-12 a buffer's QUERYBUF, MMAP and mapping, 40-42 and 97 the IOSurface lookup, 84 the
  frame's copy, 85 its QBUF, 80/81 the format description, 86 a coded frame nobody waits for.

## What the transport costs when idle

`DroidVMVirtioMedia.c`'s thread looks at the event queue's used ring every 50 ms (every 1 ms while a session
streams); that is a read of guest memory, no trap. xnuhost's side sleeps until a command or a kick with no session
open (`media_device.c`).

## Limits [F from the code]

- A session's buffers come from its half of the 48 MiB media pool (`MEDIA_SLOTS` 2): 4 NV12 frames and 6 coded
  buffers of width x height / 2 + 64 KiB each, about 9 bytes a pixel. A frame above about 2.7 megapixels is refused
  at REQBUFS (`errors`, step 93 or 94) and that session gets no encoder. CMPhoto encodes large pictures as tiles; the
  size it asks for a full-screen picture has not been seen yet.
- Two sessions at a time; a third `OPEN` fails (step 90) until one closes.
- No decoder: the device's card is `oneios-encoder`, so `decs` is empty and iOS keeps its own decoders.

## What a session on the phone must show

Kernel (`xnuhost.log`): `encoder: on: mediacodec-helper backend, media pool 48 MiB at ...`; `ONEIOS CODEC: 1
virtio-media device(s), 1 up; VideoToolbox paravirtualization registered (stage 2)`; `ONEIOS CODEC: device 0:
oneios-encoder ...`; then `ONEIOS CODEC: counters:` with `sessions` and `frames out` above 0 and `errors 0`, and
xnuhost's `encoder: session N: HEVC WxH on mediacodec-helper`.

iOS's log (`oneios-tool ioslog`):
- **no** `CMPhotoCompressionSessionAddImage ... (codec: 'hvc1')` / `kCMBaseObjectError_UnsupportedOperation` errors;
- WidgetRenderer_Default **without** `Content load failed: unable to find or unarchive file for key: ...systemSmall...`;
- `oneios-gpu-pixels: WidgetRenderer_ ... drawn`;
- a screenshot saved as HEIC (its first bytes `....ftypheic`) [I: that iOS 27 saves a screenshot as HEIC].

## Built and booted on the Linux PC (2026-11-15)

- **Built [F]:** `origin/main` `fb4ec88` plus this work, `HOSTED_OUT=hevc XNU_FILESET_REFERENCE=1
  kernel/build-hosted.sh`, 716 s from an empty tree: `xnu.macho` `972b9132...` (24,961,392 bytes), the step
  `archive-13-oneios-tool` (`build-oneios-codec: .../liboneioscodec.a (4 objects: ...)`), `_oneios_codec_start` the
  archive's and called by `_oneios_drivers_start`. `kernel/prelink/build.sh`: `kernel.kc`
  `e4ffcabd1d95f25eb542b7226be3235ac2317743d667f22dcc094a5b59198d8b` (96,141,312 bytes), `audit: 2711 paired, 93
  functions on the stub, 9 data unpaired; 0 failures`; `oneios-tool prelink stubs-check`: 666 good, 0 bad.
- **Booted [F], the first 40 s only:** a PC boot (`qemu-aarch64`, `--mode bench --ram 4G --set wifi=sim --set
  encoder=auto`, a script that swipes Home every 2 minutes) printed

  ```
  xnuhost I encoder: on: stand-in backend, media pool 48 MiB at 0x20fbe04000 (2 session slots)
  xnuhost I encoder: the stand-in encoder (encoder=auto with the bridges off: no phone): no MediaCodec; every frame is coded as it is (I_PCM), no compression
  guest     ONEIOS CODEC: 1 virtio-media device(s), 1 up; VideoToolbox paravirtualization registered (stage 2)
  guest     ONEIOS CODEC: device 0: oneios-encoder caps 0x4004000 out: NV12 cap: HEVC H264
  ```

  (the last line is the guest transport's own check against xnuhost's device: a session opened, ENUM_FMT on both
  queues, the session closed). Then all work was stopped by the owner and the boot was ended.
- **Not done:** the rest of that boot (whether a process opens the service and encodes: the `ONEIOS CODEC: counters:`
  and `message` lines), the count of chronod's `CMPhotoCompressionSessionAddImage ... 'hvc1'` errors and of
  WidgetRenderer's `Content load failed` in iOS's log, and the same boot without `encoder=auto` to count against.
  Three finished PC boots of other work (main's kernel, no home script) have **none** of either line: without a GPU
  the PC's widgets may never reach the snapshot, so the PC may not be able to show that count at all [I]; the phone
  can. The next step is that boot to its end (`out/pcboot.sh` of this work is not in git: the command is the PC boot
  of `docs/building.md` section 5 with the three `--set` above), then the phone.
