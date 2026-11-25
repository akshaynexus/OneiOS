# runtime/bridges/encoder: the host video encoder and decoder

Owner: the encoder agent. The iOS/guest side: `kernel/patches/oneios-hosted/codec`. The whole path and the route:
`docs/hardware.md` section 7g ("Video encoder"). This is the host end of iOS's paravirtualized VideoToolbox
encoder, so CMPhoto writes HEIC: chronod's widget snapshots, screenshots, the photo library (then video later).

**State (2026-11-15, hevc-guest):** host and guest are built, and proven together on the PC as far as a PC boot goes
(the kernel's codec archive brings the device up and publishes the service; `codec/README.md` has the boot's lines).
The Android build (`encctl`, the helper, the device library) builds on the Linux PC with NDK r28c and is in the
runtime package. **2026-11-22, on main (0081, 0082) on the Mac:** `runtime/host-tests.sh` passes (`test_standin`
173/173, `test_encoder` 214/214), ffmpeg 9.0.2 on the Mac decodes all 16 stand-in streams to exactly the picture
given, `runtime/build.sh` stages `bin/oneios-encoder` (28,184 bytes) and `bin/encctl` (32,840 bytes) with NDK
28.2 and its ELF check passes, `runtime/package.sh` packs both (its Apple-file and ELF checks pass), and the image
set's `encoder=auto` now applies (it was dropped before). **Not yet
run on the phone** ("On the phone", below): that is the next step.

The crate `oneencoder` (Rust, in the runtime's workspace; `runtime/RUST.md`): since 2026-11-23 a port of the C
encoder bridge, one module per C file, log lines byte for byte. The test counts and sizes quoted above and below
(173, 214 checks; 28,184 and 32,840 bytes) are of the C build; the Rust tests have the same cases.

```sh
cd runtime
cargo test -p oneencoder    # the stand-in's 16 streams (tests/standin.rs, and tests/decode.sh: ffmpeg decodes all 16 to
                            # the picture given; skipped with a line without ffmpeg), the device through the helper
                            # process and in-process (tests/encoder.rs), MediaCodec's session logic against a fake codec,
                            # the NDK's numbers (tests/headers.rs)
./build.sh STAGE            # STAGE/bin/oneios-encoder and STAGE/bin/encctl (dynamic), with the rest of the runtime
cargo build --release --target aarch64-linux-android -p oneencoder --bins   # the two alone
```

## The parts

| Module | What |
|---|---|
| `proto.rs` | the wire layout of the `droidvm,virtio-media` device: the commandq (OPEN/CLOSE/IOCTL/MMAP), the eventq (DQBUF/EVENT), and the V4L2 structs (`v4l2_buffer`, `v4l2_format`) as UniXNU-droidvm's `DroidVMVirtioMedia.c`/`DroidVMVTHost.cpp` read them (the C's `media_proto.h`; no C side any more: checked by `tests/encoder.rs`'s round trips) |
| `media_device.rs` | xnuhost's stateful V4L2 **encoder** device (and decoder, 2026-11-24: "The decoder") on `xh_core::virtio` (`create`, `stats`): OUTPUT = tightly packed NV12 or P010 (2026-11-23), CAPTURE = HEVC/H.264 Annex-B. Each guest session gets a slice of the media pool (`media-host-pool`); a worker thread turns coded access units into CAPTURE DQBUF events. Linked statically into xnuhost |
| `backend.rs` | the backend interface (`open`/`feed`/`drain`/`poll`/`close`) as the `Backend` and `Session` traits (the C's `encoder.h`); since 2026-11-23 `open` takes the input format (NV12 or P010), a session says its `Coding` (8-bit, Main10, 10-bit as 8-bit), and `prepare` (a guest's virtio-media OPEN) lets the helper backend start its helper early |
| `backend_helper.rs` | the device-side backend that runs the real encoder in the **oneios-encoder helper process** (`HelperBackend::new`; xnuhost is static; MediaCodec is a shared library, as for the camera and audio helpers). Spawns the helper, multiplexes sessions over a UNIX socket (`link.rs`, the C's `encoder_link.h`), buffers coded AUs for `poll()`. **Never waits for the helper without a limit** (a vCPU calls it): the HELLO and each write 1 s, an open's reply `OPEN_MS` (2.5 s; `HelperBackend::with_open_ms` for another); a helper that misses one is killed (`encoder: the helper (pid N) did not answer an open in 2500 ms (state S, waiting in W): killed (1 of 2); the next open starts a new one`), the next open starts a new one, twice a session at most (then `encoder: the helper hung 2 times: no more helpers this session ...`, once); `backend_helper::stop()` (xnuhost's stop; `HelperBackend::stop` for one backend) kills it without waiting. Session ids are never reused. `ONEIOS_ENCODER_STUB_HANG_OPEN=1` in the helper's environment makes the stand-in's open hang (`tests/hung_helper.rs`) |
| `helper.rs` → `bin/oneios-encoder.rs` → `oneios-encoder` | the helper: reads the device's messages, runs the backend, streams the bitstream back. Dynamic. Since 2026-11-23 it starts its binder threads, warms MediaCodec up, and names a slow open's MediaCodec call ("10-bit pictures and the helper's open", below) |
| `backend_ndk.rs` | the real backend: the NDK's `AMediaCodec` (`video/hevc` → Codec2 `c2.qti.hevc.encoder`, `video/avc`) behind a `Codec` trait, NV12 ByteBuffer input (P010 with HEVC Main10, 2026-11-23), placed by the codec's own stride and slice height, Annex-B output. Still-image settings: one keyframe, constant quality. With Android 16's codec store (API 36, dlsym): the vendor encoder by name, its smallest size and alignment (`caps`). Both the `samsung` and `treble` hardware backends use this same path |
| `backend_stub.rs` | the **stand-in encoder** (`StubBackend`) for a host with no MediaCodec (below). In the library too: xnuhost runs it in its own process on the PC |
| `encctl.rs` → `bin/encctl.rs` → `encctl` | the phone test: the whole host path in one process (direct MediaCodec, no helper), encoding a test pattern as root (`hevc10`: a P010 one), and `encctl caps` (the encoders' sizes and Main10); on a host the same path with the stand-in; and the in-process guest `Guest` the device test drives |
| `tests/standin.rs`, `tests/standin_reader/`, `tests/decode.sh` | the stand-in's tests (below; the C's `test_standin.c`, `standin_reader.h`) |
| `tests/encoder.rs` | the device's test (below; the C's `test_encoder.c`) |
| `tests/hung_helper.rs` | a helper that hangs in its open: bounded, killed, a new one, none after two hangs, a stop not a hang (the C's `test_hung_helper` in `test_encoder.c`; a test binary of its own, since it sets the hang switch in its process's environment) |
| `tests/headers.rs` | MediaCodec's numbers and `AMediaCodecBufferInfo`'s layout against the NDK's headers (a probe built with the NDK's clang, run under qemu-aarch64; skipped with a line without them) |
| `decoder.rs` | the **decoder** (2026-11-24, "The decoder" below): the `Decoder` and `DecodeBackend` traits, `DecEvent` (a picture's layout, a picture placed, a drain's end) to a `DecodeSink`, `PoolMem` (the pool as this process maps it), `OutLayout` (MediaCodec's output format, the picture's copy into a CAPTURE buffer: two copies for NV12, row by row when cropped or P010), `stream_headers` (the parameter sets or AV1 sequence header kept for after a drain), `Core` (the logic every backend shares), and the stand-in decoder (`StubDecodeBackend`) |
| `decode_ndk.rs` | the real decoder (Android): `AMediaCodec` in asynchronous mode over `Core`, in the helper (and in `encctl decode`) |
| `encctl_decode.rs` | `encctl decode FILE`: the decoder's phone test (an IVF or Annex-B file through the device and MediaCodec) |
| `tests/decoder.rs` | the decoder's test (below) |

## The stand-in encoder (`backend_stub.rs`)

The PC has no MediaCodec, and the Android helper cannot run there. The stand-in compresses nothing, and what it
writes is a real stream that a decoder decodes to the frame it was given:

- **HEVC**: VPS, SPS, PPS and one IDR slice (Main profile, 16x16 coding tree blocks) whose every coding unit is
  **I_PCM**: the frame's own samples, raw. The only arithmetic-coded bins are `part_mode` and the terminating bins
  (`pcm_flag`, `end_of_slice_segment_flag`), so the file carries the standard's CABAC encoder whole (the two tables,
  40 lines).
- **H.264**: SPS, PPS and one IDR slice (Constrained Baseline, CAVLC): I_PCM macroblocks when they fit the buffer,
  else Intra 16x16 DC macroblocks with no residual (one byte each): a grey picture of the frame's size.
- The guest's coded buffer is width x height / 2 + 64 KiB (`DroidVMVTHost.cpp`'s S_FMT), a third of the raw frame.
  So the HEVC samples are written at the largest PCM bit depths that fit (`pcm_sample_bit_depth_luma/chroma`: 8+8,
  4+4, 3+2, 2+2, 2+1 or 1+1 bits a sample), with an ordered dither, and the decoder shifts them back up: the picture
  is the frame with fewer levels. The frame is padded to whole blocks and cropped by the conformance window.
- Until 2026-11-15 the stand-in wrote NAL units of the right types around zeros and a marker (no decoder took it).

**Tested [F, Linux PC, 2026-11-15, the C build]:** the C's `test_standin` (173 checks; now `cargo test -p oneencoder --test standin`, the same cases: 16 streams, 64x64 to 1440x3120,
sizes that are not whole blocks, a black frame, a small buffer; the NAL order, the SPS's profile bytes, the first
block's arithmetic-coded bytes `86 80` worked out by hand from the standard, every sample within the PCM step of the
frame), then `tests/decode.sh`: **ffmpeg n9.0.2 decodes all 16 streams with no warning to exactly the picture given**
(`cmp` of the raw 4:2:0 output with what the test expects; skipped with a line when there is no ffmpeg).

## The device against the guest (`media_device.rs`; fixed in the C's `media_device.c`) — what the 2026-11-15 reading of the guest's code changed

The first host test's guest was written beside the device. Read against the real guest
(`DroidVMVirtioMedia.c`), four things were wrong, each of which would have stopped the first encode on the phone:

1. **MMAP** answered the buffer's guest address. The guest takes the answer as the buffer's place in the pool
   (`at > pool_size` is refused, then `gpa = pool_base + at`), so every buffer was refused (`DroidVMVTHost` step 11).
   The device answers the place in the pool.
2. **CLOSE** comes with no buffer for an answer (`droidvm_media_close` sends the command alone); the device dropped
   every command without one. No session was ever closed: the guest's start-up check took one of the two slots for
   ever, the first encode the other. The device closes first and answers only when there is a buffer.
3. **ENUM_FMT** answered 0 with nothing: the guest's start-up check asked 16 times a queue and listed 16 empty
   formats. The device lists NV12 (OUTPUT), HEVC and H.264 (CAPTURE), then EINVAL.
4. **A coded unit larger than the guest's buffer** stayed at the head of the helper backend's ring and came back as
   an error at every poll (1000 a second) for as long as the session lived. It is dropped and counted once.

And the worker's polling: it asked the backend every 20 ms for ever (50 wake-ups a second with nobody using the
device) and every millisecond while a session was open. Now: every millisecond for 2 s after the guest's last command
or the backend's last frame, every 50 ms while a session is open and quiet, and **not at all with no session**: it
sleeps until a command or a kick. Since 2026-11-25 the 1 ms looks are gone too for the helper and the stand-in: "The
worker waits for the encoder" below.

**Tested [F]:** `test_encoder` (C, 214 checks; now `cargo test -p oneencoder --test encoder`, the same cases): the device driven over xnuhost's real transport (`xh_core::virtio`; the C's `virtio.c`) by a guest of
plain memory that now does MMAP, CLOSE and ENUM_FMT **as `DroidVMVirtioMedia.c` does** (the place in the pool with
the guest's bound, CLOSE with no answer buffer, the format lists), replaying `DroidVMVTHost`'s encoder sequence
(OPEN, S_FMT HEVC+NV12, REQBUFS/QUERYBUF/MMAP, STREAMON, QBUF a NV12 test pattern, the coded DQBUF, ENCODER_CMD STOP →
EOS), then five more sessions opened and closed one after another (more than the device has slots). Twice: through
the **real helper process** built for the host with the stand-in, and with the stand-in in the device's own process
(xnuhost's PC arrangement). The coded picture is read back and compared with the pattern.
**The hung helper [F]:** the C's `test_hung_helper` (main, 2026-11-23, after the two v0.1.16 phone boots that froze on
a helper that never answered an OPEN) and now `cargo test -p oneencoder --test hung_helper` (2026-11-23, 1.1 s): with
the stand-in's open made to hang and a 300 ms limit, the open comes back in 300-1500 ms with no session and the line
`... did not answer an open in 300 ms (state ...): killed (1 of 2) ...`; the next open starts a new helper that
works; a stop is not counted as a hang; after the second hang every open fails at once (under 100 ms) and the
"no more helpers" line is said once.
**Not tested here:** the real guest transport against the device outside a boot (its MMIO accessors are arm64
assembly); the PC boot runs its start-up check (OPEN, ENUM_FMT, CLOSE), and a session only when iOS encodes.

## Into xnuhost

The crate links into xnuhost (static). `runtime/xnuhost/src/session/bridges.rs` carves the media pool (after the GPU pool, inside the 64 MiB
the hosted kernel lets a task map, `0024-gpu-pool.patch`), creates the device, and publishes `/arm-io/virtio-media-0`
with `media-host-pool`; it adds `droidvm-vt-paravirt=1` to the guest's boot arguments when the device exists.

| `encoder=` | What |
|---|---|
| `off` | the default (until a phone session proves it): no pool, no device |
| `auto` | in a session (the bridges on): the phone's MediaCodec through `bin/oneios-encoder` (`encoder: on: mediacodec-helper backend, ...`; `encoder: off: no bin/oneios-encoder ...` when the helper is not in the runtime). With the bridges off (a PC boot, `--mode bench`): the stand-in inside xnuhost (`encoder: on: stand-in backend, ...` and `encoder: the stand-in encoder (encoder=auto with the bridges off: no phone): ...`) |
| `sim` | the stand-in always, also in a session (`encoder: the stand-in encoder (encoder=sim): ...`): iOS gets a hardware encoder whose pictures have few levels |

The stand-in is never taken in a session on the phone unless `encoder=sim` asks for it. Ship
`oneios-encoder` as `bin/oneios-encoder` (0755) in the runtime package (`runtime/build.sh` does).

## CLAUDE.md's bridge rules

- **One replaceable lookup, read-only to the system:** the device touches only its own media pool and the socket to
  the helper; the helper opens Android's MediaCodec and nothing of One UI's `/data`.
- **No realtime threads:** the device's worker is an ordinary `SCHED_OTHER` thread; the helper is a plain process.
- **Closed at the return:** the device's `reset` closes every encoder session; closing the socket ends the helper (it
  exits when its `read` returns 0), and MediaCodec is released per session. xnuhost's stop also kills the helper (one that hangs in
  MediaCodec would never see its socket's end). Nothing is grabbed, nothing persists.

## On the phone

The phone is not on this box. `runtime/build.sh STAGE` (or `cargo build ... -p oneencoder --bins`, above) on the Linux PC or the
Mac builds `bin/oneios-encoder` and `bin/encctl`; the runtime package carries both. Root through KernelSU, One UI
running:

```sh
adb push STAGE/bin/encctl STAGE/bin/oneios-encoder /data/local/tmp/
adb shell su -c 'cd /data/local/tmp && ./encctl hevc'                          # HEVC 256x256 through c2.qti.hevc.encoder
adb shell su -c 'cd /data/local/tmp && ./encctl h264 640 480'
adb shell su -c 'cd /data/local/tmp && ./encctl hevc 512 512 /data/local/tmp/enc.hevc'   # the stream too
adb pull /data/local/tmp/enc.hevc && ffmpeg -i enc.hevc -frames:v 1 enc.png   # a grey gradient with diagonal bands
```

`encctl` encodes one NV12 test pattern and prints, on a pass: each access unit's size, its buffer's size and the types
of its NAL units (`encctl: AU 1234 bytes of a 98304-byte buffer, NAL types 32, 33, 34, 19 (keyframe)`; MediaCodec may
give the parameter sets as a unit of their own first), and `encctl: hevc 256x256: N access unit(s), M bytes, keyframe
present; ok`. `error:` lines say which step failed (`S_FMT`, `no output buffer`, `MMAP: ... is not in the pool`, or
`N access units, ... key 0`: MediaCodec refused NV12 ByteBuffer input — then try `encctl` with a larger size, since
`c2.qti` encoders have a 96x96 minimum). It is harmless beside One UI (it opens its own MediaCodec instance; it
touches nothing of One UI's). It has not been run on the phone yet, nor have the device library and the helper.

From a staged runtime (`oneios-tool phone stage runtime DIR` of a runtime package that has them; the CLI has no
`encctl` command, so this is adb):

```sh
adb shell su -c 'cd /data/local/tmp/oneios/runtime/bin && ./encctl hevc'
adb shell su -c 'cd /data/local/tmp/oneios/runtime/bin && ./encctl h264 640 480'
```

Then a session with `encoder=auto`. The OneOS app passes no `--set encoder=`, so the setting goes into the staged
image set: add the line `encoder=auto` to the phone's `image/overlay/userland/xnuhost.conf` (keep its other lines,
`scale=` and `pen=` among them) and stage that file with the new `kernel.kc` and `kernel.kc.hashes`, at their paths in
one folder, by `oneios-tool phone manifest DIR --stage` (the manifest lists the file, so it must be staged with its
new entry). Until 2026-11-22 xnuhost read `encoder=` of the image set and then dropped it (the C's `runtime/xnuhost/src/config.c`,
`encoder_given`): only `--set encoder=auto` turned the encoder on. The lines that prove it are in
`kernel/patches/oneios-hosted/codec/README.md` ("What a session on the phone must show").

**Guess [I]:** NV12 as `COLOR_FormatYUV420SemiPlanar` ByteBuffer input to `c2.qti.hevc.encoder`; the still-image CQ
settings; that MediaCodec's unit for a frame at quality 90 fits width x height / 2 + 64 KiB (a larger one is dropped
and counted, point 4 above).

## 10-bit pictures and the helper's open (2026-11-23)

The phone's session of 2026-11-23 18:24 coded no tile: the guest refused CMPhoto's 10-bit (`xf20`) tiles, and every
open of the helper hung until the device killed it (`did not answer an open in 2500 ms ... futex_wait_queue`, then
`refused 96x96`; the guest side: `kernel/patches/oneios-hosted/codec/README.md`, "10-bit pictures and the helper's
open").

- **P010 [F, host-tested]:** the device takes `V4L2_PIX_FMT_P010` on OUTPUT (ENUM_FMT index 1; S_FMT answers
  bytesperline 2 W and sizeimage 3 W H; REQBUFS sizes its buffers so; any other format EINVAL). `Backend::open` gets
  the input format; the helper link carries it (link version 2: the OPEN's payload, the OPEN_REPLY's `b` = the
  session's `Coding`).
- **MediaCodec [I: not run on the phone]:** a P010 HEVC session is configured with `COLOR_FormatYUVP010` (54) and
  `HEVCProfileMain10` (2), at constant quality, then at a bit rate; if both are refused (or Android 16's codec store
  says the encoder has no Main10 with P010 at that size), as 8-bit NV12 with each sample's high byte, and that size
  is not tried for Main10 again by the process. The frame goes into the input buffer at the codec's own `stride` and
  `slice-height` (its input format, read after start), the last column and row repeated to the size the codec codes
  (`Layout::place`).
- **The codec's size:** with the codec store (API 36: `AMediaCodecStore_*`, `ACodecVideoCapabilities_*`, found with
  dlsym since the programs are built for API 30) the backend opens the first vendor encoder of the type by its name,
  and a picture below its smallest size or off its alignment is coded at the next size it takes. Without the store:
  the type's first encoder at the picture's size, as before.
- **The helper's open:** the helper (and `encctl`) start their binder threads first (libbinder_ndk's pool through
  `onecamera::android::binder_threads`, the camera's fix of 2026-11-17; libhidlbase's `configureRpcThreadpool` and a
  thread in `joinRpcThreadpool` when that library is loaded), then say HELLO; the device starts the helper at the
  guest's first virtio-media OPEN (`Backend::prepare`: the transport's check at boot), and the helper opens one HEVC
  encoder at once on a thread of its own (the warm-up). A watchdog thread names the MediaCodec call an open waits in
  (`backend_ndk::open_step`) at 1 s and 2 s; the device logs the helper's lines (`ENC_MSG_LOG`). [I] that the missing
  binder threads were the hang: the warm-up line of the next boot says whether an open comes back now.
- **The 60 s line:** `encoder: 60 s: coded 8-bit N, 10-bit as Main10 N, 10-bit as 8-bit N; opens refused N; sessions
  N`, the access units coded by their session's path since the last line; said 60 s after the first unit or refused
  open, and not at all while nothing is coded (the worker sleeps until it is due, one wake-up). Each session's line
  says its path once: `encoder: session N: HEVC 96x96 on mediacodec-helper: 10-bit pictures as Main10 (P010)`.

Tested on the build laptop: `cargo test -p oneencoder` (unit: the P010 placing with a codec's stride, slice height
and padding, P010 into an 8-bit codec, 8-bit into P010, the rows taken or not, the coded sizes, the 60 s window and
text, the high bytes; the device test through the helper process and in process, NV12 and P010 each: ENUM_FMT, the
S_FMT sizes, a refused format, the picture read back, the coded counts by path; the hung helper; the stand-in's 16
streams), `cargo clippy` for the host and `aarch64-linux-android`. Not tested: anything with MediaCodec (the phone).

On the phone (the CLI has no `encctl` command, so this is adb; one line an encoder, then one a square size):

```sh
adb push STAGE/bin/encctl /data/local/tmp/ && adb shell su -c '/data/local/tmp/encctl caps'
adb shell su -c 'cd /data/local/tmp && ./encctl hevc10 96 96'    # the last line: "coded Main10" or "coded 10-bit as 8-bit"
adb shell su -c 'cd /data/local/tmp && ./encctl hevc 96 96'
```

## The Rust port (`oneencoder`, 2026-11-23; runtime/RUST.md)

One Rust module per C file (the table above; the C is gone from the tree): the C's `br_encoder_create` is
`media_device::create`, `br_encoder_backend_helper` is `HelperBackend::new`, `br_encoder_helper_open_ms` is `HelperBackend::with_open_ms`, `br_encoder_helper_stop` is `backend_helper::stop()`. The helper spawn and the byte views are
onecamera's. The commands are at the top of this file.

- On a host `encctl` runs the whole path with the stand-in (`encctl hevc 512 512 FILE` gives a stream ffmpeg
  decodes); on Android with MediaCodec, as before. Both programs are dynamic on Android (`build.rs` links libmediandk
  into the bins only) and pass `runtime/package/elf-check.sh` (2026-11-23); not run on the phone yet.
- Changes from the C, on purpose: the helper backend writes to the helper under a lock of its own (one lock for
  writes and the receive thread's state could stop both with the socket full both ways); its socket is close-on-exec
  (the C's leaked into later children, the helper too); the stand-in reuses one RBSP buffer per session; the
  device's worker holds the device weakly.
- Changes from main's C of the hung helper (cce3151), on purpose: a session of a helper that was killed never writes
  to the next helper (the C's feed and drain went to the new one's socket under an id it never opened) and its poll
  gives nothing; an open or a write that fails on a helper the stop already killed does not count a hang, and a
  reaped pid is never killed again; the stop that finds the state locked kills the helper by its pid alone (the C
  read the state without the lock).

## Switches (2026-11-23, docs/switches.md)

The decoder's (2026-11-24): `decoder=auto|off` and `decoder-codecs=h264,hevc,vp9,av1`, set once by xnuhost
(`switches::set_decoder`); the device reads them (its card, ENUM_FMT, S_FMT), the helper never sees them.

`switches.rs`: `encoder-10bit=main10|8bit|nv12` and `encoder-warmup=0|1` of xnuhost's config, set once before the
device is made. `nv12`: the device lists no P010 at ENUM_FMT and refuses it at S_FMT (the guest sends NV12, its high
bytes); `8bit`: P010 is taken and coded 8-bit, Main10 never tried (`backend_ndk`). The helper gets them on its command
line: `oneios-encoder 3 [no-main10] [no-warmup]`; `no-warmup` skips the warm-up thread and `prepare()` (the helper starts
at the first encode). The binder threads start either way.

## The interrupt (fix/codec-irq, 2026-11-25)

The device raises its SPI through xh-core's transport on every used-ring write, as every device does. Since 2026-11-25
it honours the guest's `VIRTQ_AVAIL_F_NO_INTERRUPT` on both its queues (`quiet_queues` in `media_device.rs`, the
transport's `Device::quiet_queues`): the staged `DroidVMVirtioMedia.c` (edit 7) sets it on the command queue, whose
answers it reads as soon as its notify returns (`command` answers inside the notify), and clears it on the event
queue, whose interrupt wakes its event worker (a DQBUF or a V4L2 event pushed by the worker). With
`oneios-codec-irq=0`, or an older kernel, the guest sets it on both and polls: the line stays down. Test:
`tests/interrupt.rs` (commands raise nothing, the decoder's SOURCE_CHANGE raises the SPI once, the ack lowers it, both
flags set: no line and the event still comes). The guest side: `kernel/patches/oneios-hosted/codec/README.md`, "The
transport on the interrupt".

## The worker waits for the encoder (fix/codec-irq, 2026-11-25)

**Why [F, read in the code].** The device's worker asked an encoder session's backend for coded units every
millisecond for 2 s (`MEDIA_BUSY_MS`) after each of the guest's commands and each unit: the backend had no callback.
While anything encodes, and also during a decode while an encoder session was open or an event waited for a guest
buffer (each command, a decoder's too, restarted the 2 s), that was up to 1000 wake-ups a second of a host thread,
each a timer and a look under the device's lock, and each unit handed on up to 1 ms late. The helper's units already arrive on a thread of their own (its socket's receive
thread), so the worker could be told.

**The fix.**
- `Session::wakes(Wakeup) -> bool` (`backend.rs`): the worker hands its wake-up to each encoder session when it opens.
  The helper backend (`backend_helper.rs`) keeps it in the session's slot and its receive thread calls it, with the
  state's lock let go, after each `ENC_MSG_CODED` is in the ring, at `ENC_MSG_EOS`, and for every session when the
  helper ends; it says true. The stand-in says true without keeping it: its units are coded in the poll from what a
  `feed` or `drain` left, and those come in a guest's command, which wakes the worker anyway. The NDK's MediaCodec
  in-process (`backend_ndk.rs`: encctl on the phone) says false, the default.
- The worker (`media_device.rs` `worker`) sleeps until woken: a command, a kick of the event queue, a decoder's
  result, an encoder's unit. A wake-up that comes while it works is kept for its next sleep (a flag under the
  sleep's lock): none is lost. With an encoder session open, or an event waiting for a guest buffer, it looks again
  `MEDIA_IDLE_MS` (50 ms) after its last look at most (the guard, unchanged: a lost wake-up would cost that much,
  never a stalled stream). The 1 ms looks for `MEDIA_BUSY_MS` remain only for a session whose backend said false.
- No switch: each command does what it did, only the worker's looks between them changed. The old path stays
  reachable through a backend that cannot wake (the test below drives it). The session's line says which:
  `encoder: session N: HEVC WxH on mediacodec-helper: 8-bit`, or `...; asked every 1 ms (the backend cannot wake the
  worker)`.
- **The line:** `media: 30 s: worker woken N: by data (commands, kicks, results, coded units) N, by the timeout N; 1 ms
  looks N`: the worker's wake-ups since the last such line, said 30 s after the first wake-up that follows it and not
  at all while the worker sleeps (one more wake-up, counted "by the timeout", for the line itself).

**Tested [F, the build laptop].** `cargo test -p oneencoder`: unit `the_wakes_line_comes_once_a_window_and_only_while_
the_worker_wakes`, `a_wake_up_while_the_worker_works_is_kept_for_its_next_sleep`; `tests/wakes.rs`: 2 s of 30 frames a
second through one encoder session as `DroidVMVTHost.cpp` drives it, counting the worker's wake-ups: a backend like
MediaCodec (each unit 4 ms after its frame, on a thread of its own) that wakes the worker gave about 280-320 wake-ups
by data, 0 by the timeout and 0 1 ms looks; the same backend unable to wake it, over 1000 1 ms looks (every unit out
either way); the helper process (the stand-in in MediaCodec's place), 0 1 ms looks. Not booted.

**What a phone session must show.** While a video plays or the camera records: `media: 30 s: ...; 1 ms looks 0`,
"by the timeout" in the tens at most (the 50 ms guard runs only while an encoder session is open), "by data" about
the commands' and units' count. `1 ms looks` above 0 means a backend that cannot wake the worker (the session's line
says `asked every 1 ms`). `oneios-perf`'s and `oneios-cpu`'s xnuhost numbers no higher than before for the same
video.

## The decoder (fix/hw-decode, 2026-11-24)

**Why:** YouTube in Safari decoded on the CPU, and the phone got hot. The device was an encoder only, so iOS's
paravirtualized decoder had nothing to use. The guest's side and 24A446's messages are in
`kernel/patches/oneios-hosted/codec/README.md`, "The decoder".

**The device (`media_device.rs`).**
- A session is a decoder when its OUTPUT S_FMT is `H264`, `HEVC`, `VP90` or `AV01` (one of `decoder-codecs=`; 96 to
  4096 a side, at most 4096x2304; OUTPUT buffers 1 to 4 MiB, the guest asks 2).
- The decoder opens at OUTPUT's STREAMON, on the vCPU, as the encoder does. A refusal is that ioctl's EIO, and the
  guest's `dsta` fails, so VideoToolbox decodes in software.
- The decoder's first result is its pictures' layout: the device's SOURCE_CHANGE. Then come G_FMT (NV12 at the
  decoder's stride), G_CTRL (MIN_BUFFERS_FOR_CAPTURE 2) and REQBUFS of CAPTURE (as many as fit the pool, two at
  least).
- Each CAPTURE QBUF gives the buffer to the decoder. Each picture is a CAPTURE DQBUF with its frame id as timestamp,
  and an error is `V4L2_BUF_FLAG_ERROR`.
- `DECODER_CMD` STOP drains, ending with EOS; START takes frames again.
- The buffers of every session come from one first-fit allocator (`Alloc`) over the pool. Four sessions at a time.
- The card is `oneios-encoder+decoder`; ENUM_FMT lists the codecs (OUTPUT) and NV12 (CAPTURE); ENUM_FRAMESIZES gives
  the sizes. With `decoder=off` all of this is as before.

**No polling.**
- A decoder gives its results to a `DecodeSink`: the device queues them and wakes its worker. The worker turns them
  into events.
- With only decoder sessions open the worker sleeps until woken. The 50 ms guard is for encoder sessions alone (and an
  event that waits for a guest buffer); the 1 ms busy look, since 2026-11-25, only for an encoder backend that cannot
  wake the worker ("The worker waits for the encoder" below).
- In the helper, MediaCodec runs in asynchronous mode. Its callbacks feed the waiting frames and place the waiting
  pictures. The helper's main thread only reads the socket.

**The pictures' copies.**
- The helper maps the media pool: xnuhost sends its RAM memfd by SCM_RIGHTS in the first message after the HELLO
  (`ENC_MSG_POOL`, link version 3: the file offset of the pool and its size; `HelperBackend::set_pool`).
- Each picture is copied once on the host, from MediaCodec's output buffer into the guest's CAPTURE buffer. That is
  two `copy_from_slice`s for NV12 at MediaCodec's stride; row by row when cropped at the left or top, or for P010,
  whose high bytes are taken.
- It is copied once in the guest, into the IOSurface, through a kernel mapping kept per surface.
- Only the compressed frames cross the socket.
- Not zero-copy: MediaCodec's ByteBuffer mode copies its own graphic buffer once more inside Codec2 [I]. A
  Surface/AHardwareBuffer output would need the guest's surfaces imported as dma-bufs (udmabuf, as the GPU's); not
  done.

**After a drain.**
- MediaCodec ends a drain with an EOS buffer. In asynchronous mode it then wants a flush and a start.
- After a flush it no longer has the stream's parameter sets, and the guest sends them only before its first frame.
- So `Core` keeps the last parameter sets a frame carried (H.264 SPS/PPS, HEVC VPS/SPS/PPS, AV1's sequence header;
  `stream_headers`) and puts them before the next frame.
- A drain mid-stream still loses the references: the next frame must be a key frame. VideoToolbox drains at the
  end and at a seek [I].

**Counters.** `Stats` has `dec_sessions`, `dec_refused`, `decoded[4]`, `dec_fed`, `dec_dropped` and `dec_us`/`dec_timed`.
`dec_dropped` counts pictures with an error, and frames fed but not out by a drain's end or the session's. Every 60 s
while frames are decoded:
`decoder: 60 s: decoded H.264 N, HEVC N, VP9 N, AV1 N of N fed; decode time avg A ms, max M ms; dropped N; sessions
N, opens refused N (mediacodec-helper)`. The decode time is a frame's time from its feed to its picture, the codec's
reordering included.

**Tests [F, the build laptop, 2026-11-24].**
- `cargo test -p oneencoder`: `decoder.rs`'s unit tests (the layouts, the headers of each codec, the stand-in one
  frame late, a drain, the headers after a restart, a picture that waits for a CAPTURE buffer) and the device's
  (the 60 s line, the allocator).
- `tests/decoder.rs` runs the staged DroidVMVTHost.cpp's decoder sequence over the real transport. VP9 and H.264
  go through the helper process (the pool a memfd the helper maps); AV1 and HEVC run with the stand-in in-process.
  The pictures are read back from the guest's buffers; there is a drain and a start; every pool buffer comes back.
  Then `decoder=off` and `decoder-codecs=vp9`.
- `encctl decode` runs on an IVF and an Annex-B file with the stand-in. Until 2026-11-24 it failed 1 run in 3
  ("nothing from the decoder for 5 s: 4 of 5 frames out", v0.1.21's first release run, and 1 of 3 runs alone on
  the build laptop). The stand-in dropped a picture when all 4 of its output buffers were taken. That happened when
  the drain came before the guest had queued its CAPTURE buffers, and then the last picture, which carries the end,
  was lost. Now pictures wait for a buffer in order, as MediaCodec keeps them
  (`the_stand_in_keeps_a_picture_while_every_output_buffer_is_taken`). Only the stand-in had this fault. The phone's
  NDK backend did not.
- `runtime/build.sh`: the helper and encctl link with `decode_ndk.rs`, and elf-check finds every NDK symbol in the
  API 30 stubs.

**On the phone (not yet run):**

```sh
# on the Mac: test files (any size up to 4096x2304)
ffmpeg -f lavfi -i testsrc2=s=1920x1080:r=30 -t 4 -c:v libvpx-vp9 -b:v 4M t-vp9.ivf
ffmpeg -f lavfi -i testsrc2=s=1920x1080:r=30 -t 4 -c:v libaom-av1 -cpu-used 8 -b:v 3M t-av1.ivf
ffmpeg -f lavfi -i testsrc2=s=1920x1080:r=30 -t 4 -c:v libx264 -bsf:v h264_mp4toannexb t.h264
adb push t-vp9.ivf t-av1.ivf t.h264 /data/local/tmp/
adb shell su -c 'cd /data/local/tmp/oneios/runtime/bin && ./encctl decode /data/local/tmp/t-vp9.ivf'
adb shell su -c 'cd /data/local/tmp/oneios/runtime/bin && ./encctl decode /data/local/tmp/t-av1.ivf'
adb shell su -c 'cd /data/local/tmp/oneios/runtime/bin && ./encctl decode /data/local/tmp/t.h264 1920 1080 /data/local/tmp/t.nv12'
adb pull /data/local/tmp/t.nv12 && ffmpeg -f rawvideo -pix_fmt nv12 -s 1920x1080 -i t.nv12 -frames:v 1 t.png
```

A pass prints the decoder's name and its open time (`encctl: decoder: VP9 1920x1080: c2.qti.vp9.decoder, opened in
N ms`), MediaCodec's output format, and `encctl: decode ...: 120 of 120 frames decoded in S s (N a second), pictures
1920x1080 at a stride of S, decode time avg A ms, dropped 0; ok`. The CLI has no `encctl` command: this is adb.
