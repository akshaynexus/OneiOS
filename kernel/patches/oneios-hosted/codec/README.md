# codec/: the video encoder and decoder (hevc-encoder, hevc-guest, 2026-11-15)

Owner: the encoder agent. The host side: `runtime/bridges/encoder`. The whole path and the route: `docs/hardware.md`,
section 7g ("Video encoder").

**State (2026-11-15, hevc-guest):** the archive is **built into `kernel.kc`** by `oneios-tool kernel gen codec`
(`kernel/build-hosted.sh` runs it after `audio`), and a PC boot with `--set encoder=auto` publishes the service.
Built and booted on the Linux PC; on main (0081, 0082) built and prelinked on the Mac on 2026-11-22 (`kernel.kc`
`5add18fa...`). **On the phone (2026-11-22)** that kernel encoded nothing: iOS 27 writes HEIC through VideoToolbox's
**tile session**, which upstream's host does not have ("The tile session", below). The staged host now has it;
built and prelinked on the Mac, host-tested, **not yet booted on the phone**. The results of the builds and of the
boots are at the end of this file.

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
| `DroidVMVirtioMedia.c` | the virtio-media transport (V4L2 stateful codec over virtio-mmio): `/arm-io/virtio-media-0`, `media-host-pool`. The device is xnuhost's (`runtime/bridges/encoder/media_device.rs`), not crosvm's. `-DDROIDVM_DEVICE_DYNAMIC=1`, Mach universe, as its iPadOS 27 board builds it. **Staged with one edit** since 2026-11-25 (7, `staged/codec_transport.rs`: the event queue on the device's interrupt, "The transport on the interrupt" below) |
| `DroidVMVTHost.cpp` | the session logic: `encs`/`+enc`/`esta`/`efrm`/`Eemt`/`ecfr` (and, staged, iOS 27's tile session `ests`/`epti`/`etil`/`ecti` with `TEsa`/`TEcd`/`TEem`); the IOSurface lookup (IOCoreSurfaceRoot), NV12 copy into the OUTPUT buffer (staged: NV12 or P010), hvcC/avcC assembly, the sample-buffer atoms. Runs in the guest kernel over the virtio-media device. **Staged with six edits** (below; the sixth, the decoder's, is `staged/codec_decode.rs`) |
| `DroidVMVTParavirt.cpp` | Apple's `AppleVideoToolboxParavirtualizationDriver` user client + the `IODataQueue` transport; publishes the service with `droidvm-vt-paravirt=1`. Unchanged; its static initializer is taken out of `__mod_init_func` and made global (`droidvm_vt_paravirt_attach` runs it in its own module load), as the audio archive's |

`OneiOSCodec.c` (this folder, OneiOS's own) is the start: `oneios_codec_start` → `droidvm_media_start()` +
`droidvm_vt_paravirt_attach()`, the start line, and the report thread.

**The edits** (`tools/oneios-tool/crates/oneios-kernel/src/staged/codec.rs`; the file in `external/` stays as it is):

1. The hvcC's twelve bytes of profile, tier and level (below).
2. iOS 27's **tile session** (2026-11-22, "The tile session" below): `ests`, `epti`, `etil`, `ecti` from the guest,
   `TEsa`, `TEcd`, `TEem` to it.
3. A **small HEVC session** is coded at 96x96 at least (c2.qti's smallest) and cropped back by its SPS's conformance
   window.
4. `jcap` (the paravirtualized JPEG session's capability query) is answered -12908: there is no JPEG codec here.
5. **10-bit pictures** (2026-11-23, "10-bit pictures" below): `etil` and `efrm` take `x420` and `xf20` surfaces and
   send them to the device as P010 (or NV12, their high bytes, to a device that takes no P010); a tile session's
   device opens at its first tile.
6. **The decoder** (2026-11-24, `staged/codec_decode.rs`, "The decoder" below): VP9 and AV1, `dcap` and `dcan`, the
. **The transport on the interrupt** (2026-11-25, fix/codec-irq, `staged/codec_transport.rs`, the edit of
   `DroidVMVirtioMedia.c`; "The transport on the interrupt" below): the event queue on the media device's SPI with a
   guard timeout instead of the 1 ms poll, the command queue `NO_INTERRUPT`, events handed on without the transport's
   lock, the `transport:` counters; `oneios-codec-irq=0` keeps the poll.

The hvcC's twelve bytes of profile, tier and level. Upstream copies them from the SPS as the stream has them
(`memcpy(out + at, s->sps + 3, 12)`). Every
HEVC SPS has emulation prevention bytes in exactly those bytes (Main's compatibility flags `60 00 00 00` are
`60 00 00 03 00` in the stream, and the constraint flags' zeros get two more), so the copy put `03` bytes into the
flags and never reached `general_level_idc`: the hvcC said level 0 for any real encoder's stream. The staged copy
takes the twelve bytes without them. [F] read from the code and the standard; the stand-in's own SPS has the same
three bytes (`runtime/bridges/encoder/tests/standin.rs` checks its first two).

## The tile session (2026-11-22)

**The fault [F, the phone, main 203a9e7, `out/kernel/codec-24A446/kernel.kc`, `encoder=auto`]:** the host encoder
worked (`encctl hevc` and `encctl h264 640 480` passed; `encoder: on: mediacodec-helper backend`; `ONEIOS CODEC: device
0: oneios-encoder caps ... out: NV12 cap: HEVC H264`), and no frame was ever encoded. `xnuhost.log`
(`out/session-logs/2026-11-22/kbd-debug/live11.log`): `counters: ... messages 109 (refused 0, unknown 27) ...;
sessions 26, frames in 0 out 0, errors 0` at 12442 s, `messages 469 (unknown 117) ...; sessions 116` at 12472 s:
**3 sessions a second**, each `videocodecd '+enc' 281 bytes, 'ests' 175 bytes, 'einv' 48 bytes, '-enc' 48 bytes`.
iOS's log of that boot (`out/session-logs/2026-11-22/s7-ioslog`, `oneios-tool phone pull-log`): videocodecd's
`ParavirtualizedVideoEncoder_CreateInstanceWithSpecification: ... codecType hvc1 ... guestProtocolVersion 6
hostProtocolVersion 6`, then `ParavirtualizedVideoEncoder_StartTileSession: (encoder ...) StartTileSession failed
with error -12900 for 84 x 84`, again and again; chronod's 116 `CMPhotoCompressionSessionAddImage: err = unknown error
[-12900] (codec: 'hvc1')` (white widgets). SpringBoard's one `'jcap' 48 bytes` was the first unknown.

**The cause [F, read in 24A446's VideoToolbox, extracted from `out/dsc-24A446-full` with `ipsw dyld extract`]:**
upstream's exchange was recorded against macOS's host and its dispatch has no tile messages, so `ests` got the
unknown type's -12900; VideoToolbox gives such a session up (`einv`, `-enc`) and CMPhoto tries again. 24A446's own
host (`___VTParavirtualizationHostEncoderSessionDeliverMessageFromGuest_block_invoke`) dispatches `esta eset emul esup
ecop epfr efrm emif ecfr ebps eeps esav` and the tile session's `ests epti etil ecti`. The tile messages, as its host
(`vtParavirtualizationHostEncoderSession_HandleEncoder{StartTileSession,PrepareToEncodeTiles,EncodeTile,
CompleteTiles}`, `VTParavirtualizationHostEncoderSession{SetTileAttributes,CreateTileVideoFormatDescription,
EmitEncodedTile}`) and its guest (`ParavirtualizedVideoEncoder_*Tile*`, `paravirtualizedVideoEncoder_
HandleMessageFromHost`) write and read them:

| Message | Parameters | Answer |
|---|---|---|
| `ests` (StartTileSession) | `,twh` the tile size (VTInt32Size, type `=32s`: width, height), `,esp` the session's properties | `,err` |
| `TEsa` (host to guest) | `,twh`, `,pba` the tiles' pixel buffer attributes | the guest's `,err` (not waited for) |
| `epti` (PrepareToEncodeTiles) | `,ept` | `,err` |
| `etil` (EncodeTile) | `,til` the tile's ID (s64), `,tio` its origin in the surface (`=32p`: x, y), `,tia` its size, `,tip` options, `,uui` the pixel buffer's UUID, the IOSurface | `,err`, `,efl` (1: asynchronous) |
| `TEcd` (host to guest) | `,cod ,wdt ,hgt ,ext`, as `Efds` | `,err`, `=fds` |
| `TEem` (host to guest) | `,til ,err ,efl ,sbf ,uui`, as `Eemt` with `,til` for `,frm` (pieces flagged `0x10`) | `,err` |
| `ecti` (CompleteTiles) | none | `,err` once every tile is handed over |

`TEsr` (the host's tile encode requirements, `,cba ,ter`) is optional and not sent. The frame path's keys are the
same in 24A446 as upstream's (`efrm` answered `,err ,inf`).

**The fix (staged `DroidVMVTHost.cpp`, edits 2 to 4):**
- `ests` sends `TEsa` (420v, as `Epba`) and (since 2026-11-23, edit 5) leaves the device's open to the first tile,
  which opens it with that tile's samples; `epti` is answered; `etil` looks the
  surface up as `efrm` does (one shared lookup now), takes its part (`,tio`, `,tia`; a 420v, 420f, x420 or xf20
  surface) and
  queues it as a frame with no times; the worker copies that part and the phone's encoder codes it (every frame of
  it is a key frame: `i-frame-interval` 0), then `TEcd` once per new parameter sets and `TEem` per tile; `ecti` is
  answered as `ecfr` (the drain, the 2 s deadline, a tile not coded by then handed over dropped).
- An HEVC session below 96x96 (chronod's tiles are 84x84; c2.qti's encoders code nothing smaller) is coded at 96 in
  that direction: the copy repeats the last column and row, and the codec's SPS gets the conformance window that
  crops the picture back to the session's size before it goes into the hvcC (`oneios_hevc_sps_crop`). The SPS is
  compared as the codec wrote it, so `TEcd` is sent only when it changes.
- `jcap` (`VTParavirtualizedJPEGSessionCreate`'s capability query, `,jca` the answer) gets -12908: no JPEG codec here,
  and the guest keeps its own JPEG code. 24A446's own host without CMPhoto's JPEG hardware answers nothing either.
- The counters line names the last unknown type (`the last 'TYPE'`), so a type of a later iOS shows at once.

The copy and the crop are `OneiOSCodecPicture.h` (this folder, plain C), which the staged file includes (`-I` this
folder) and `kernel/tests/host.sh` tests on the host: `tests/test_picture.c` (17 checks: tiles inside, at the edges,
coded larger, parts outside; SPSs built field by field with and without a window of the codec's own, with
sub-layers, with emulation prevention bytes, read back; since 2026-11-23 30, with the 10-bit copies of "10-bit
pictures" below) and `tests/crop.sh` (ffmpeg 9.0.2's libx265 and
hevc_videotoolbox code 96x96 and 128x96, the SPS is cropped to 84x84 and 120x90, ffprobe reads the new size and
ffmpeg decodes, with `-err_detect explode`, exactly the top-left part of the picture: 4 streams on the Mac).

## The generator and the build

`oneios-tool kernel gen codec` (`tools/oneios-tool/crates/oneios-kernel/src/build_oneios_codec.rs`; written in Rust,
no Python original) is one entry of the hosted build's board archives, after `audio`:

- compiles the staged `DroidVMVirtioMedia.c` (edit 7, written into the archive's folder as `DroidVMVTHost.cpp` is) and
  `OneiOSCodec.c` with the first kernel pass's Mach record, the staged `DroidVMVTHost.cpp` and `DroidVMVTParavirt.cpp`
  with its IOKit record (`-I` the package folder; the staged files and `OneiOSCodec.c` also `-I` this folder, for
  `OneiOSCodecPicture.h` and `OneiOSCodecWait.h`);
- makes `DroidVMVTParavirt.late.o` (no `__DATA,__mod_init_func`, `__GLOBAL__sub_I_DroidVMVTParavirt.cpp` global);
- archives `build/oneios-codec/liboneioscodec.a` (4 objects) and requires `_oneios_codec_start`,
  `_droidvm_media_start`, `_droidvm_vt_paravirt_attach`, `__GLOBAL__sub_I_DroidVMVTParavirt.cpp`,
  `_droidvm_vthost_available`, `_droidvm_vthost_message`, `_droidvm_media_ioctl`, `_droidvm_media_mmap`, and that
  neither C++ object still has a `__mod_init_func` section;
- writes `objects.json` (each source with its SHA-256, `OneiOSCodecPicture.h`, `OneiOSCodecWait.h` and both staged
  files among them).

`kernel/build-hosted.sh` adds `-Wl,-force_load,{tree}/build/oneios-codec/liboneioscodec.a` to
`CONFIG_XNU_EXTRA_LD_ARCHIVES` (after `audio`'s). `ONEIOS_CODEC_RECORDS` and `ONEIOS_CODEC_OUT` name another record
folder and output folder: the generator against a finished build's tree, without a kernel build.

Tests: `cargo test -p oneios-kernel --release --test golden` (`the_codec_edit_writes_its_frozen_text_and_fits_the_
pinned_source`: the edit list on a made-up source against `tests/kernelgen/golden/synthetic/codec.out`, and on the
real file: each tile message has its case, each helper is defined before its caller);
`the_transport_edit_fits_the_pinned_source` (edit 7 on the pinned `DroidVMVirtioMedia.c`: each anchor once, the poll
gone, applied twice it fails);
`kernel/tests/host.sh` (`OneiOSCodecPicture.h`: `tests/test_picture.c` and `tests/crop.sh`, above; `OneiOSCodecWait.h`:
`tests/test_wait.c`, both paths of the transport's wait);
`tools/oneios-tool/tests/kernelgen/compare-golden.sh WORK` after a build (the staged file's hash, the generator's
line).

## Boot arguments and log lines

| Boot argument | What |
|---|---|
| `oneios-codec=0` | nothing of this archive starts (`ONEIOS CODEC: off (oneios-codec=0)`) |
| `droidvm-media=0` | no transport (no device is looked for) |
| `droidvm-vt-paravirt=1` | publish the service. xnuhost adds it when `encoder=auto` or `encoder=sim` gave the guest a device. Without it the classes are registered (stage 1) and root can publish later: `sysctl kern.droidvm_vt_paravirt=1` |
| `oneios-codec-report=0` | no report thread |
| `oneios-codec-irq=0` | (2026-11-25, edit 7) the transport polls its event queue as the pin does (1 ms while a session streams, 50 ms otherwise) instead of waiting on the device's interrupt; default 1. `docs/switches.md` |

- At start, always: `ONEIOS CODEC: N virtio-media device(s), M up; VideoToolbox paravirtualization registered (stage
  2)` with a device and `droidvm-vt-paravirt=1`; `... not published (stage 1)` without the boot argument or without a
  device (a kernel booted with `encoder=off`: `0 virtio-media device(s), 0 up`).
- With a device, 3 s later: `ONEIOS CODEC: device 0: oneios-encoder caps 0x4004000 out: NV12 cap: HEVC H264` (the
  transport's own start-up check: a session opened, each queue's formats listed, the session closed).
- Every 30 s while a figure moves (nothing while nobody uses the codec): `ONEIOS CODEC: counters: service opened N
  closed N, messages N (refused N, unknown N, the last 'TYPE'), to the guest N (dropped N), replies N; sessions N,
  frames in N out N, errors N (last: step S value V); virtio-media commands N (timeouts N), events N errors N buffers
  N other`, then `ONEIOS CODEC: session: ...` for each line `DroidVMVTHost.cpp` wrote into its trace ring since (a
  session's size, `tiles` for a tile session, the size the device codes, its samples (`NV12`, `P010`, or `NV12 (10-bit
  pictures, the device takes no P010)`), the codec; `tile session WxH: the device opens at the first tile`; the
  IOSurface lookups). Then, once a picture was copied (2026-11-23), `ONEIOS CODEC: pictures: 8-bit N, 10-bit as P010
  N, 10-bit as 8-bit N (the device takes no P010)`: the pictures copied for the device by their samples.
  `the last 'TYPE'` (2026-11-22) is the last message type the host has no case for (`droidvm_vthost_last_unknown` as
  four characters, `'none'` before the first): a type a new iOS sends shows on this line at once (iOS 27's `'ests'`
  was found from the message lines). `step S` is the stage `DroidVMVTHost.cpp` gives `failed()`: 90-96 the session's
  start (90 OPEN, 91/92 S_FMT, 93/94 REQBUFS, 95/96 STREAMON), 10-12 a buffer's QUERYBUF, MMAP and mapping, 40-42 and
  97 the IOSurface lookup, 84 the frame's copy, 85 its QBUF, 80/81 the format description, 86 a coded frame nobody
  waits for, 20 a message that does not fit; staged: 100 a tile's parameters (`etil` without `,til`/`,tio`/`,tia`/
  `,uui`, or larger than the tile size), 101 a tile's surface none of 420v, 420f, x420, xf20 (until 2026-11-23: not
  420v/420f), 102 an SPS whose conformance window
  could not be written (a small HEVC session: no description, so no tile is handed over).
- With each counters line (2026-11-25, edit 7): `ONEIOS CODEC: transport: interrupt (SPI N), S s: interrupts N; event
  worker woken by the interrupt N, by the timer N (with events N); commands N (waited N)`, or `transport: polled (WHY),
  ...` with WHY `oneios-codec-irq=0`, `no interrupt in the device tree` or `ArmCpu did not attach the SPI`. The counts
  are those since the last such line, S its seconds (30 a report the counters line was due). "By the timer" is the
  guard on the interrupt (a few per 30 s; "with events" above 0 is an interrupt that came late or not at all) and every
  poll when polled (about 30,000 per 30 s while a session streams). "Waited" is a command xnuhost did not answer inside
  its notify (0 expected: it answers every command there).

## What the transport costs when idle

Since 2026-11-25 (edit 7), on the interrupt: `DroidVMVirtioMedia.c`'s event worker sleeps until the device's SPI, at
most its guard (1 s with no session streaming, 25 ms while one streams): one wake a second when idle. With
`oneios-codec-irq=0` (or no SPI) it looks at the event queue's used ring every 50 ms (every 1 ms while a session
streams), as the pin did: each look is a read of guest memory, but each sleep a host timer and a wake of a vCPU.
xnuhost's side (`media_device.rs`) sleeps until a command, a kick, a decoder's result or (since 2026-11-25) an
encoder's coded unit, which the helper backend's receive thread wakes it with; with an encoder session open, or an
event waiting for a guest buffer, 50 ms at most (its guard). Until 2026-11-25 it asked an encoder session's backend
every 1 ms for 2 s after each command (`runtime/bridges/encoder/README.md`, "The worker waits for the encoder").

## The transport on the interrupt (fix/codec-irq, 2026-11-25)

**Why [F, read in the code].** The pinned transport (UniXNU-droidvm f9121cd) set `VIRTQ_AVAIL_F_NO_INTERRUPT` on both
queues and polled: its thread slept `IOSleep(fast ? 1 : 50)`, `fast` while a session streams. While a video plays that
is 1000 timer wakes of a vCPU a second (each a host timer and a wake: `docs/booter.md` 2.4), 20 a second for the
whole boot, and each event (a decoded picture, an input buffer consumed) handed on up to 1 ms late.
Droid-VM moved the same file to completion interrupts (UniXNU-droidvm `96e8c1f`, `2cd4c8e`: H.264 decode 268 fps and
HEVC 367 fps against about 150 before, their PROGRESS.md of 2026-11-13; on crosvm, whose commands complete
asynchronously).

**Whether the interrupt existed [F].** On the host side, yes: xnuhost's virtio transport raises the device's SPI on
every used-ring write (`runtime/core/src/virtio.rs` `put`, for every device), the media device is made with its SPI
(`media_device.rs` `Virtio::create(base, spi, ...)`), and the loader names it in `/arm-io/virtio-media-0` as
`droidvm,intid` (`runtime/loader/src/machine.rs`, `BOARD_VIRTIO_SPI + index`). Only the guest never attached it. Two
facts of xnuhost change the port against Droid-VM's: xnuhost answers every command inside the guest's notify (the
used ring is written before `post` returns, so a command never waits), and it raised the line for every command too,
ignoring `NO_INTERRUPT`: an interrupt per command would have been an ArmCpu dispatch (about 140-290 us of trapped GIC
registers, `ArmCpu's trapped registers` in `oneios-perf`) for nothing.

**The fix.**
- Guest (edit 7, `staged/codec_transport.rs`, after 96e8c1f and 2cd4c8e): the window's INTID from `droidvm,intid`
  (or `interrupts`, `gic-spi`), attached with `armcpu_spi_attach` as a level SPI (as the bridge, camera and input
  drivers take theirs). The handler acknowledges the device's interrupt status (0x60, 0x64), counts, and wakes the
  event worker; no mutex, no callback. Only the event queue clears `NO_INTERRUPT`; the command queue keeps it. The
  event worker arms its wait, looks at `media_pending` (an interrupt before the sleep is not lost), sleeps until the
  interrupt or its guard (`OneiOSCodecWait.h`: 25 ms while a session streams, 1000 ms otherwise), then reaps every
  device. Events are copied and their buffers returned under the device's lock and handed to `DroidVMVTHost.cpp`
  without it (96e8c1f's order; the sink takes the session table's and the session's locks). A command no longer reaps
  events while it waits (it holds the device's lock; the event worker does).
- xnuhost (`runtime/core/src/virtio.rs`, `Device::quiet_queues`, default none): a device may name queues whose
  driver's `NO_INTERRUPT` the transport honours; the media device names both (`media_device.rs`), so the command
  queue raises nothing and the event queue raises the SPI. Every other device is unchanged (the flag is ignored there,
  as before). An older kernel (both queues `NO_INTERRUPT`, nothing attached) gets no line at all now; a new kernel on an
  older xnuhost gets an interrupt per command as well (it works, at that cost).
- `oneios-codec-irq=0` keeps the pin's poll; a window with no SPI, or one ArmCpu does not attach (16 slots), is
  polled the same way. Not exact: events go through the event worker without the lock on both values.

**Tests [F, the build laptop].**
- `kernel/tests/host.sh`: `tests/test_wait.c` (26 checks: the mode and why on each path, the worker's sleep polled and
  on the interrupt, the wake counters of a stream of each, the line of both modes).
- `cargo test -p oneios-kernel --test golden`: `the_transport_edit_fits_the_pinned_source`.
- `cargo test -p xh-core`: `virtio::tests::no_interrupt_is_honoured_only_on_the_queues_the_device_names` (a quiet
  queue with the flag raises nothing, without it the line rises and the ack lowers it, another queue and another
  device raise whatever the flag).
- `cargo test -p oneencoder --test interrupt` (`runtime/bridges/encoder/tests/interrupt.rs`): the media device driven as
  edit 7 drives it: OPEN and the session's commands raise nothing, the decoder's SOURCE_CHANGE raises the SPI once, the
  ack takes it down; with both queues `NO_INTERRUPT` the line stays down and the event still comes.
- Not tested on the host: the kernel's wait primitives and ArmCpu's dispatch (the phone is the test).

**What a phone session must show (YouTube in Safari, `decoder=auto`).**
- `ONEIOS CODEC: transport: interrupt (SPI 4x), 30 s: interrupts N; event worker woken by the interrupt N, by the timer
  N (with events N); commands N (waited 0)`: interrupts and wakes by the interrupt in the hundreds to thousands while
  the video plays, by the timer a few per 30 s (polled it was about 30,000), with events near 0.
- `ONEIOS CODEC: decoded: ...` growing as before, dropped near 0, `dfrm to Demt avg` no higher than before (an event
  is handed on at once now, not after up to 1 ms).
- `oneios-perf`: `timer_set` per 30 s and the vCPU wakes lower during the video than with `oneios-codec-irq=0`;
  `ArmCpu's trapped registers` higher by about the interrupts' count (the trade).
- `oneios-cpu`: kernel_task, videocodecd, mediaplaybackd, com.apple.WebKit.Web at or below their figures with
  `oneios-codec-irq=0`; `thermal: 30 s` cooler or the same for the same video.
- Failure signs: `transport: polled (ArmCpu did not attach the SPI)` (no slot left: polled as before); `by the timer`
  with events climbing (interrupts lost); a video that stalls (then boot with `oneios-codec-irq=0`).
- xnuhost's worker (the second fix, `runtime/bridges/encoder/README.md` "The worker waits for the encoder"; no
  switch, the same on both values of `oneios-codec-irq`): `media: 30 s: worker woken N: by data (commands, kicks,
  results, coded units) N, by the timeout N; 1 ms looks 0` in xnuhost.log while the video plays (and while the camera
  records or a photo is saved: the encoder), "by the timeout" in the tens at most. `1 ms looks` above 0 only with a
  backend that cannot wake the worker (`encoder: session N: ...; asked every 1 ms`), never with `mediacodec-helper`.

**Built on the build laptop (2026-11-25, origin/main a1200e5 plus this work). Not booted** (the phone was not
connected; CLAUDE.md 2026-11-24: no qemu boots).
- **Kernel [F]:** `HOSTED_OUT=codecirq ONEIOS_CRYPTO=original kernel/build-hosted.sh`: `xnu.macho`
  `ad9dfc32cc0d98bc85fa437163815d4fe0816bc6c9f7079ecdad401a06a0a37a` (24,736,968 bytes), releasable, 193 patches, in
  547 s. The archive's four compiler logs are empty (no warning). The staged `DroidVMVirtioMedia.c` is `0996ec0d...`
  (27,259 bytes), the staged `DroidVMVTHost.cpp` `72bfaad0...` as before (`tests/kernelgen/golden/staged.sha256`);
  `_media_interrupt`, `_oneios_media_counts`, `_oneios_media_polled` and ArmCpu's `_armcpu_spi_attach` are in the kernel.
- **Prelinked [F]:** `KERNEL=out/kernel/current-hosted-codecirq/xnu.macho OUT=out/kernel/codecirq-24A446
  kernel/prelink/build.sh`: `kernel.kc` `782b3c24749782d9f09ad8a4baa96badbda075f087b17b7efa88889b55180383` (96,337,920
  bytes), `audit: 2711 paired, 93 functions on the stub, 9 data unpaired; 0 failures`; `oneios-tool prelink
  stubs-check`: 666 good, 0 bad. `kernel.kc.hashes` `bf795f6c...`.
- `compare-golden.sh`: the codec's two staged files are the frozen ones; `oneios-net/report.json`, `DroidVMVirtioInput.c`,
  `DroidVMGpu.cpp` and net's line differ as they did before this work.
- **Host tests [F]:** `kernel/tests/host.sh` 14 of 14 (`test_wait` 26 checks); `cargo test -p oneios-kernel --test
  golden` 16 of 16; `runtime/host-tests.sh`: workspace, lint-android, port, links and package pass, the boot part
  skipped; its lint part failed on the formatting of this work's two test files, fixed (`cargo fmt`), then `cargo fmt
  --check` and `cargo clippy --workspace --all-targets -D warnings` pass, `tests/interrupt.rs` and xh-core's virtio
  tests (15) pass again.

## Limits [F from the code]

- A session's buffers come from the 48 MiB media pool, first fit (`media_device.rs` `Alloc`; until 2026-11-24 each of
  two sessions had a fixed half, `MEDIA_SLOTS` 2): 4 NV12 frames and 6 coded
  buffers of width x height / 2 + 64 KiB each, about 9 bytes a pixel (P010 frames: about 15). A frame above about 2.7 megapixels is refused
  at REQBUFS (`errors`, step 93 or 94) and that session gets no encoder. CMPhoto encodes HEIC as tiles: a tile
  session is opened at the tile size (84x84 for chronod's widget snapshots on 2026-11-22, coded at 96x96), not the
  picture's.
- Four sessions at a time (`MEDIA_SESSIONS`, encoders and decoders); a fifth `OPEN` fails (step 90) until one closes.
- The decoder (2026-11-24): the device's card is `oneios-encoder+decoder` with `decoder=auto` (`oneios-encoder` with
  `decoder=off`, and then `decs` is empty and iOS keeps its own decoders). It takes 8-bit 4:2:0 only, 96 to 4096 a
  side and at most 4096x2304; anything else is refused at `dsta` and decoded in software. At most one 4K session fits
  the pool next to the encoder's tiles.

## What a session on the phone must show

Kernel (`xnuhost.log`): `encoder: on: mediacodec-helper backend, media pool 48 MiB at ...`; `ONEIOS CODEC: 1
virtio-media device(s), 1 up; VideoToolbox paravirtualization registered (stage 2)`; `ONEIOS CODEC: device 0:
oneios-encoder ...`; then `ONEIOS CODEC: counters:` with `sessions` and `frames out` above 0 and `errors 0`, and
xnuhost's `encoder: session N: HEVC WxH on mediacodec-helper`.

With the tile session (2026-11-22): the message lines `'+enc'`, `'ests'`, `'epti'` (when VideoToolbox prepares the
tiles), `'etil'` (one per tile, with a surface: `1 surface(s)`), `'ecti'`, then `'einv'`, `'-enc'`; `sessions`
growing slowly (once per snapshot, not 3 a second); `frames in N out N` with the same N above 0; `unknown` not
growing and `the last` not `'ests'`;
`ONEIOS CODEC: session: encoder session N 84x84 tiles coded 96x96 NV12 hvc1` (`P010` for the wallpaper posters'
10-bit tiles, 2026-11-23); xnuhost's `encoder: session N: HEVC 96x96 on mediacodec-helper: 8-bit` (the device's
size; for 10-bit tiles `10-bit pictures as Main10 (P010)` or `10-bit pictures as 8-bit (the encoder offers no
Main10 at this size)`).

With 10-bit pictures (2026-11-23, "10-bit pictures" below): **no** `DroidVMVTHost: stage 101 failed`; `ONEIOS
CODEC: pictures: ...` with `10-bit as P010` above 0; xnuhost's `encoder: 60 s: coded 8-bit N, 10-bit as Main10 N,
10-bit as 8-bit N; opens refused 0; sessions N` with a coded count above 0; **no** `encoder: backend mediacodec-helper
refused`, **no** `did not answer an open`; the helper's `encoder: helper: binder threads: ...` and `encoder: helper:
warm-up: ...` lines near the boot's start (the transport's check starts the helper).

iOS's log (`oneios-tool phone pull-log DIR`, then `oneios-tool ioslog health` and `checks`):
- **no** `StartTileSession failed with error` from videocodecd;
- **no** `CMPhotoCompressionSessionAddImage ... (codec: 'hvc1')` / `kCMBaseObjectError_UnsupportedOperation` /
  `unknown error [-12900]` errors: `encode-fail 0` in `ioslog-health.txt`, the `heic` row of `ioslog checks` 0;
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
  can. On 2026-11-20 (b5cf905's message) a 10-minute PC boot of the hevc-guest merge (`HOSTED_OUT=hevc`) with
  `encoder=auto` ran clean: the codec registered, cameracaptured's `encs` was answered, no panic; the `hvc1` count was
  not recorded. The next step is the phone, with the Mac build below (no PC boot was run for it).

## On main, built on the Mac (2026-11-22)

PR #59 (`hevc-guest`) merged onto main with 0081 and 0082 (`fix-dyld-cache-unnesting`). The archive is built after
`audio` and force-loaded last of the hosted archives (`oneios-tool kernel hosted-archives`, `FORCE_LOADED` in
`tools/oneios-tool/crates/oneios-kernel/src/hosted_device.rs`: gpu aes keystore net hid audio codec); it has no
numbered patch, so the series is unchanged and 0081 and 0082 apply as on main. b5cf905's 0080 change is not taken:
main's 65b7bd4 already anchors the same `vm_shared_region.c` hunk.

- **Built [F]:** `HOSTED_OUT=codec kernel/build-hosted.sh` (no reference kernel), 492 s on the Mac: `xnu.macho`
  `baad846266253a2f...` (25,630,144 bytes); the step `archive-13-oneios-tool` printed `build-oneios-codec:
  .../liboneioscodec.a (4 objects: ...)`; `_oneios_codec_start`, `_droidvm_media_start`,
  `_droidvm_vt_paravirt_attach` and 0082's `_hosted_dyld_flags_init` are in it.
- **Prelinked [F]:** `KERNEL=out/kernel/current-hosted-codec/xnu.macho OUT=out/kernel/codec-24A446
  kernel/prelink/build.sh`: `kernel.kc` `5add18fab1cb64ce5b6cea3f8349dde12aed125b88eaa4fe041a7366ac1a3a8a`
  (96,845,824 bytes), `audit: 2712 paired, 92 functions on the stub, 9 data unpaired; 0 failures`; `oneios-tool
  prelink stubs-check`: 666 good, 0 bad. The same symbols are in the collection.
- **Host tests [F]:** `runtime/host-tests.sh` all parts pass (xnuhost 14229 checks), `kernel/tests/host.sh` 7 of 7,
  `cargo test -p oneios-kernel` (golden 14 of 14).
- **Fixed with it:** xnuhost dropped the image set's `encoder=` (read, then not copied back, as `pen=` was): the OneOS
  app passes no `--set`, so a phone session could not turn the encoder on. `runtime/bridges/encoder/README.md` "On
  the phone" has how to set it now.
- **On the phone (2026-11-22 23:50, kernel `out/kernel/combo-24A446` `5baab993...` = main 23561aa with 0082, 0083 and
  the tile messages; `encoder=auto`):** `encctl hevc 96 96` ok before the session; `ONEIOS CODEC: 1 virtio-media device(s), 1
  up`; `counters: ... unknown 0, the last 'none'` (the earlier boot's ests/einv/-enc loop is gone: `sessions 1`, not 3 a
  second); iOS's log (`oneios-tool phone pull-log`): `encode-fail 0` (116 `CMPhotoCompressionSessionAddImage ... err =`
  the boot before), no `StartTileSession failed`; the owner: "widgets now render". Not yet seen: `frames in`/`out` above 0
  in the counters (no tile was sent in that boot), so the HEVC encode itself is still unproven on the phone.

## The tile session, built on the Mac (2026-11-22)

- **Built [F]:** `HOSTED_OUT=codecfix kernel/build-hosted.sh` on main 63f05a6 plus this work, 266 s: `xnu.macho`
  `1325991ff5d6a698...` (25,653,624 bytes); `archive-13-oneios-tool.log` has `build-oneios-codec: ... (4 objects:
  ...)`; the staged `DroidVMVTHost.cpp` `6a07f479...` (94,344 bytes; `tests/kernelgen/golden/staged.sha256`), no
  compiler warning; `droidvm_vthost_last_unknown` is in the archive.
- **Prelinked [F]:** `KERNEL=out/kernel/current-hosted-codecfix/xnu.macho OUT=out/kernel/codecfix-24A446
  kernel/prelink/build.sh`: `kernel.kc` `852adb8be21b525d7ba61dc857b120304f9f9d7f1081ff3d3a711e6eb3585dfb`
  (96,845,824 bytes), `audit: 2712 paired, 92 functions on the stub, 9 data unpaired; 0 failures`; `oneios-tool
  prelink stubs-check`: 666 good, 0 bad. No pairing record changes.
- **Host tests [F]:** `cargo test -p oneios-kernel -p oneios-ioslog` (golden 14 of 14, ioslog 15 of 15 with the
  `encode-fail` name test); `kernel/tests/host.sh` 8 of 8 (`test_picture` 17 checks, `crop.sh` 4 streams);
  `compare-golden.sh` on the build: the 11 staged hashes and the codec line the same (the Wi-Fi archive's
  `report.json` and line differ on the `codec` tree of before this work too: not this change).
- **Not done:** the phone boot: stage `out/kernel/codecfix-24A446/kernel.kc` and `kernel.kc.hashes` with
  `encoder=auto` in `xnuhost.conf` ("What a session on the phone must show", above). Not known before it [I]: that
  CMPhoto gives the tiles in 420v or 420f surfaces after `TEsa` (step 101 says the format if not), and that c2.qti
  takes 96x96 (`encctl hevc 96 96` tells beforehand).

## The tile session on the phone (2026-11-23 18:24, runtime 0.1.18)

The phone's session (`out/session-logs/2026-11-23-mac-s1824/xnuhost-final.log`) answered both questions above, and
both are no:
- **Step 101 [F]:** six `DroidVMVTHost: stage 101 failed: 2019963440` lines (and the counters' `errors N (last: step
  101 value 2019963440)`). The value is the surface's format as four characters: 2019963440 = 0x78663230 = `'xf20'`,
  `kCVPixelFormatType_420YpCbCr10BiPlanarFullRange` (10-bit 4:2:0, full range, two planes of 16-bit samples). So
  CMPhoto hands the wallpaper posters' tiles in a 10-bit surface although `TEsa` asks for 420v, and the staged
  `etil` (`staged/codec.rs`, the `failed(101, ...)` check) refuses it with -12902.
- **96x96 [F], read again on 2026-11-23:** six `xnuhost E encoder: backend mediacodec-helper refused 96x96`. Each
  comes 1 ms after `encoder: the helper (pid N) did not answer an open in 2500 ms (state S, waiting in
  futex_wait_queue): killed (1 of 2)` (lines 2416-2417 of the log), and after the second kill every open fails at once
  (`the helper hung 2 times: no more helpers this session`). So c2.qti did not refuse the size: MediaCodec's open
  never came back in the helper, as in the two v0.1.16 boots (docs/booter.md). `encctl hevc 96 96`, run from a root
  shell, passed on 2026-11-22.

So no tile is coded; the guest's tile encode fails, and nothing else of the session is harmed (the line is the
codec's, not the GPU's: the abort at the end of that session was drm2kgsl's, docs/gpu.md 2.4b). Both are fixed by
"10-bit pictures" below (built and host-tested; not yet booted on the phone).

## 10-bit pictures and the helper's open (2026-11-23)

**The 10-bit tiles.** Edit 5 of the staged `DroidVMVTHost.cpp` (`staged/codec.rs`):
- `etil` takes `420v`, `420f`, `x420` and `xf20` surfaces (`surfaceSampleBytes`: 1 or 2 bytes a sample; any other
  format is still step 101). `efrm` takes them too.
- A tile session's device opens at its first tile, not at `ests` (`openForPicture`): a 10-bit tile opens it with
  OUTPUT `V4L2_PIX_FMT_P010` (16-bit samples, the 10 bits high: the surface's own layout, copied as it is), an 8-bit
  tile with NV12. A device that refuses P010 (an xnuhost before this change) gets NV12, each sample's high byte. A
  frame session opens at `esta` as before and again at its first frame when that frame is 10-bit. A later picture
  of the other depth is converted by the copy (16 bits to 8: the high byte; 8 to 16: v x 257, the low 6 bits
  cleared). `TEsa` still asks for 420v, and is sent at `ests` as before.
- A tile session's SPS is always cropped to the tile size (the host may code a tile larger than asked: below).
- `oneios_codec_pictures[3]` counts the pictures copied (8-bit, 10-bit as P010, 10-bit as 8-bit): the line
  `ONEIOS CODEC: pictures: ...` after the counters line.

The copy is `oneios_yuv_copy_area` (`OneiOSCodecPicture.h`; `oneios_nv12_copy_area` is its 8-bit case). xnuhost's
side (`runtime/bridges/encoder/README.md`, "10-bit pictures"): the device takes P010 on its OUTPUT queue (ENUM_FMT
lists it), and the MediaCodec backend codes it as HEVC Main10 with P010 input (`COLOR_FormatYUVP010`,
`HEVCProfileMain10`) when the encoder offers it at that size, else as 8-bit (the high bytes, NV12). The session's
line says which path ran (`encoder: session N: HEVC 96x96 on mediacodec-helper: 10-bit pictures as Main10 (P010)`),
and xnuhost's `encoder: 60 s: coded 8-bit N, 10-bit as Main10 N, 10-bit as 8-bit N; opens refused N; sessions N`
counts the pictures by path, every 60 s while any are coded.

The camera's "10-bit formats" (docs/HANDOFF-2026-11-23.md, c.) are not the same need: they are the formats the
capture plug-in's streams list and fill (`x420`, `x422`; done in `a5c244d`), not what the encoder takes. A 10-bit
video frame given to the encoder (`efrm` with an `x420` surface) now goes the same way as a tile; `x422` (4:2:2)
is not taken (step 84: no copy).

**The helper's open.** The refused opens were the helper's MediaCodec open that never came back. The likely cause [I,
from the camera helper's fault of 2026-11-17, `runtime/bridges/camera/README.md`]: a plain executable has no binder
threads, and Codec2 calls its client back over binder. The helper (and `encctl`) now start their binder threads
first (libbinder_ndk's pool; libhidlbase's when it is loaded), start at the guest's first virtio-media session (the
transport's check at boot) instead of the first encode, open one HEVC encoder at once (a warm-up), and name the
MediaCodec call an open waits in at 1 s and 2 s. Lines: `encoder: helper: binder threads: ...`, `encoder: helper:
warm-up: c2.qti.hevc.encoder: widths A-B, heights C-D, alignment WxH; smallest WxH, Main10 (P010) there yes|no (in N
ms); an HEVC WxH encoder opened and closed in N ms`, and only when slow `encoder: helper: an open of HEVC 96x96 (NV12)
has waited 1000 ms in AMediaCodec_configure`.

**The smallest size.** With Android 16's codec store (API 36, by dlsym) the backend opens the first vendor HEVC
encoder by its name and codes a picture below its smallest size, or off its alignment, at the next size it takes (the
frame's last column and row repeated; the guest's SPS crop above takes it back to the tile). The guest still pads to
96x96 first. The phone's numbers: `encctl caps` (one line an encoder, then 64 to 512 square: the size, and Main10
with P010) and `encctl hevc10 96 96` (a P010 picture through the whole host path; its last line says `coded Main10`
or `coded 10-bit as 8-bit`).

**Built and tested on the build laptop (2026-11-23, main 78c4b86 plus this work):**
- **Kernel [F]:** `HOSTED_OUT=codec10 ONEIOS_CRYPTO=original kernel/build-hosted.sh` (as a release is built), 416 s:
  `xnu.macho` `73f3e2241e68240c...` (24,649,528 bytes), `releasable`, 0 of the source corecrypto's 1191 symbols;
  `build-oneios-codec: ... (4 objects ...)`, no compiler warning; the staged `DroidVMVTHost.cpp` `15745ee1...`
  (98,771 bytes; `tests/kernelgen/golden/staged.sha256`); `_oneios_codec_pictures` in the kernel.
- **Prelinked [F]:** `KERNEL=out/kernel/current-hosted-codec10/xnu.macho OUT=out/kernel/codec10-24A446
  kernel/prelink/build.sh`: `kernel.kc` `b5f5378a3acac4428848400d79618609bcca727e70004383e54ebc2050e79950`
  (96,272,384 bytes), `audit: 2711 paired, 93 functions on the stub, 9 data unpaired; 0 failures`; `oneios-tool
  prelink stubs-check`: 666 good, 0 bad.
- **Host tests [F]:** `kernel/tests/host.sh` 5 of 5 (`test_picture` 30 checks; `crop.sh` 2 libx265 streams, no
  VideoToolbox on Linux); `cargo test -p oneios-kernel --test golden` 14 of 14; `runtime/tests/host.sh` passed (the
  workspace's tests, fmt, clippy for the host and Android, the three arm64 boots); `compare-golden.sh`: the codec
  line is the new hash; `oneios-net/report.json`, `DroidVMVirtioInput.c` and the tree paths of `stdout.txt` differ as
  they did before this work.
- **Runtime [F]:** `runtime/build.sh` (elf-check 0 failures): `bin/xnuhost` `f5e52e28...`, `bin/oneios-encoder`
  `cc04ff76...`, `bin/encctl` `22ccf55e...`.
- **Not tested:** the phone (MediaCodec's Main10 with P010, the codec store's numbers, whether the binder threads
  end the helper's hang, CMPhoto taking the Main10 tiles). The proof lines: "What a session on the phone must
  show", "With 10-bit pictures".

## The decoder (fix/hw-decode, 2026-11-24)

**Why [F, `out/session-logs/2026-11-24-mac-gpumem/live.log`].** YouTube in Safari lagged and the phone got hot. The
device was an encoder only, so videocodecd's `decs` got an empty list, and iOS decoded the video on the CPU.
`oneios-cpu` during the video: com.apple.WebKit.Web 91-142 %, videocodecd 15-26 %, mediaplaybackd 58 %.

**What 24A446 asks [F, read in its VideoToolbox and WebCore, `ipsw dyld extract` of `out/ios27-cache`, 2026-11-24].**
- The guest's paravirtualized decoder registers every `decs` entry, whatever its codec type
  (`vtLoadParavirtualizedVideoDecoders`). `VTIsHardwareDecodeSupported` creates one with a hardware decoder required;
  VP9 is behind `VTIsVP9DecodeEnabledForCurrentProcess`. So `vp09` and `av01` entries make iOS see hardware VP9 and
  AV1.
- `ParavirtualizedVideoDecoder_CanAcceptFormatDescription` sends `dcan` (`,fds`) and reads `,can` (s32).
- In a guest, `vtCreateDecoderCapabilitiesDictionary` (WebCore's `VTCopyAV1DecoderCapabilitiesDictionary` and
  `VTCopyHEVCDecoderCapabilitiesDictionary`) sends `dcap` (`,ver` 6, `,cod`) and takes the host's `,cda`. The keys
  (the CFStrings of `kVTDecoderCodecCapability_*` and `kVTDecoderProfileCapability_*`): `VTSupportedProfiles` (an
  array of profile numbers), `VTPerProfileSupport` (keyed by the profile's number as a string, "0" and "2" in VT's
  own VP9 dictionary), each with `VTIsHardwareAccelerated`, `VTMaxDecodeLevel`, `VTMaxPlaybackLevel`. VT's own VP9
  entry has playback level 51. Upstream answered both with the unknown type's -12900.
- WebCore 625.1.29 has `isSupportingAV1`, `isSupportingVP9Profile`, `SWVP9` and AV1 level tables (strings);
  YouTube's codec choice follows what it offers [I].

**Edit 6 (`staged/codec_decode.rs`, after the encoder's five):**
- `decs` lists `avc1`, `hvc1`, `vp09` and `av01` (IDs `com.oneios.videodecoder.virtio-media.vp9` and `.av1`). It lists
  each one the device's OUTPUT queue offers: `decoderCodecs()` runs VIDIOC_ENUM_FMT once in a session of its own
  (`H264`, `HEVC`, `VP90`, `AV01`), and falls back to upstream's two when the device cannot be asked.
- `dsta` takes VP9 profile 0 (`vpcC`: 8-bit, 4:2:0) and AV1 Main 8-bit 4:2:0 (`av1C`). A 10-bit or other stream is
  refused (-12910), and VideoToolbox decodes it in software.
- A VP9 or AV1 sample is copied as it is (no NAL length prefixes). AV1's configOBUs (the sequence header) go before
  the first frame, as an avcC's parameter sets do.
- `dcap`: H.264 profiles 66, 77, 100 at level 51; HEVC 1 and 2 at 153; VP9 0 at 51; AV1 0 at 13 (seq_level_idx 5.1).
  All are hardware accelerated, and the levels are bounded by the media pool (4K). A codec the device does not offer
  gets -12906.
- `dcan`: 1 for the same codec and size in a format the device decodes. The new parameter sets go before the next
  frame. Else 0, and VideoToolbox makes a new session.
- `copyFrame` keeps each surface's kernel mapping for the session (at most 16 surfaces, released at teardown).
  Upstream mapped and unmapped the whole surface for every frame. The pages are wired only while the mapping is made
  (`complete()` at once), so a process that frees its surface never waits for the session.
- A SOURCE_CHANGE while CAPTURE streams takes the new geometry (G_FMT) when it fits the mapped buffers (steps 57 and
  58 when not). Upstream ignored it.
- Counters, for `OneiOSCodec.c`:
  - `oneios_decode_frames[4]` and `oneios_decode_dropped[4]` (avc1, hvc1, vp09, av01), counted at Demt.
  - `oneios_decode_us_sum`, `_count` and `_max`: the time from a frame's dfrm to its Demt. A codec's reordering is in
    it.

**The lines.**
- `ONEIOS CODEC: device 0: oneios-encoder+decoder caps 0x4004000 out: NV12 P010 H264 HEVC VP90 AV01 cap: HEVC H264
  NV12`. That is the transport's check.
- `ONEIOS CODEC: session: decoder codecs 0xf (avc1 hvc1 vp09 av01)` and `ONEIOS CODEC: session: decoder session N
  1920x1080 vp09`.
- Each report while frames are decoded or dropped: `ONEIOS CODEC: decoded: avc1 N, hvc1 N, vp09 N, av01 N; dropped
  N, N, N, N; dfrm to Demt avg N us, max N us (N frames)`.
- The message lines name `'decs'`, `'dcap'`, `'+dec'`, `'dsta'`, `'dfrm'`, `'dfin'` and `'dcan'`. `the last` must not
  be `'dcap'` or `'dcan'`.

The host's side and its 60 s line: `runtime/bridges/encoder/README.md`, "The decoder".

**What a phone session must show (YouTube in Safari).**
- xnuhost: `decoder: on: mediacodec-helper backend, ...`, `decoder: session N: VP9 ... on mediacodec-helper` (or AV1,
  or H.264), the helper's `decoder: VP9 WxH: c2.qti.vp9.decoder, opened in N ms`, then `decoder: 60 s: decoded ...`
  with decoded close to fed, dropped near 0, and the average decode time well under 33 ms.
- The guest: `ONEIOS CODEC: decoded: ... vp09 N` (or av01) growing, dropped 0, and dfrm-to-Demt averages in the tens
  of ms.
- `oneios-cpu`: com.apple.WebKit.Web, videocodecd and mediaplaybackd far below the 91-142 %, 15-26 % and 58 % of
  2026-11-24. The `thermal` lines cooler at the same brightness.
- iOS's log (`phone pull-log`): no `ParavirtualizedVideoDecoder ... failed`.

**Built and tested on the build laptop (2026-11-24, main b917889 plus this work). Not booted** (CLAUDE.md 2026-11-24:
no qemu boots; the phone is the test).
- **Kernel [F]:** `HOSTED_OUT=hwdec ONEIOS_CRYPTO=original kernel/build-hosted.sh`: `xnu.macho` `9e4bae00b86529e5...`
  (24,688,976 bytes), releasable. The series runs to 0100. `build-oneios-codec: ... (4 objects ...)`, no compiler
  warning. The staged `DroidVMVTHost.cpp` is `72bfaad0...` (112,882 bytes; `tests/kernelgen/golden/staged.sha256`).
  `decoderCapabilities`, `acceptFormat`, `decoderCodecs` and `oneios_decode_*` are in the archive and the kernel.
- **Prelinked [F]:** `KERNEL=out/kernel/current-hosted-hwdec/xnu.macho OUT=out/kernel/hwdec-24A446
  kernel/prelink/build.sh`: `kernel.kc` `dfd8835e44f5c13ab2ac276a5c1c6334203d8998a9fbfcecd9261c72df51d930`
  (96,288,768 bytes), `audit: 2711 paired, 93 functions on the stub, 9 data unpaired; 0 failures`; `oneios-tool
  prelink stubs-check`: 666 good, 0 bad. `kernel.kc.hashes` is `5d21b2a3...`.
- **Host tests [F]:**
  - `kernel/tests/host.sh` 11 of 11.
  - `cargo test -p oneios-kernel --test golden` 14 of 14 (the decoder's edit on the pinned source and the synthetic
    one).
  - `runtime/host-tests.sh`: workspace, lint, lint-android, port, links and package pass; the boot part is skipped.
  - `compare-golden.sh`: the codec's hash is the new one. `oneios-net/report.json`, `DroidVMVirtioInput.c` and
    the tree paths of `stdout.txt` differ as they did before this work.
- **Runtime [F]:** `runtime/build.sh` (elf-check 0 failures): `bin/xnuhost` `ffb3ef6d...`, `bin/oneios-encoder`
  `01e26ba6...`, `bin/encctl` `d678acfc...`. Both dynamic ones need `AMediaCodec_setAsyncNotifyCallback`,
  `createDecoderByType`, `flush`, `getName` and `AMediaFormat_getRect`, all found in the NDK's API 30 stubs.
- **Not tested:** anything on the phone. That covers MediaCodec's decoders in asynchronous ByteBuffer mode, whether
  WebCore offers VP9 or AV1 after `dcap`, and the CPU and thermal effect.
