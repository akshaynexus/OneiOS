# runtime/bridges/encoder: the host video encoder

Owner: the encoder agent. The iOS/guest side: `kernel/patches/oneios-hosted/codec`. The whole path and the route:
`docs/hardware.md` section 7g ("Video encoder"). This is the host end of iOS's paravirtualized VideoToolbox
encoder, so CMPhoto writes HEIC: chronod's widget snapshots, screenshots, the photo library (then video later).

**State (2026-11-15, hevc-guest):** host and guest are built, and proven together on the PC as far as a PC boot goes
(the kernel's codec archive brings the device up and publishes the service; `codec/README.md` has the boot's lines).
The Android build (`encctl`, the helper, the device library) builds on the Linux PC with NDK r28c and is in the
runtime package. **Not yet run on the phone** ("On the phone", below).

```sh
make test          # host tests (ASan, UBSan): the stand-in encoder alone, its streams through ffmpeg, and the device +
                   #   socket + helper + stand-in over the real transport
make               # build/android/{oneios-encoder, encctl, liboneencoder.a}   (NDK r28c)
```

## The parts

| File | What |
|---|---|
| `media_proto.h` | the wire layout of the `droidvm,virtio-media` device: the commandq (OPEN/CLOSE/IOCTL/MMAP), the eventq (DQBUF/EVENT), and the V4L2 structs (`v4l2_buffer`, `v4l2_format`) as UniXNU-droidvm's `DroidVMVirtioMedia.c`/`DroidVMVTHost.cpp` read them |
| `media_device.c` | xnuhost's stateful V4L2 **encoder** device on `xh_virtio_*`: OUTPUT = tightly packed NV12, CAPTURE = HEVC/H.264 Annex-B. Each guest session gets a slice of the media pool (`media-host-pool`); a worker thread turns coded access units into CAPTURE DQBUF events. `liboneencoder.a` (static, into xnuhost) |
| `encoder.h` | the backend interface (`open`/`feed`/`drain`/`poll`/`close`) and `br_encoder_create` |
| `backend_helper.c` | the device-side backend that runs the real encoder in the **oneios-encoder helper process** (xnuhost is static; MediaCodec is a shared library, as for the camera and audio helpers). Spawns the helper, multiplexes sessions over a UNIX socket (`encoder_link.h`), buffers coded AUs for `poll()` |
| `helper.c` → `oneios-encoder` | the helper: reads the device's messages, runs the backend, streams the bitstream back. Dynamic |
| `backend_ndk.c` | the real backend: the NDK's `AMediaCodec` (`video/hevc` → Codec2 `c2.qti.hevc.encoder`, `video/avc`), NV12 ByteBuffer input, Annex-B output. Still-image settings: one keyframe, constant quality. Both the `samsung` and `treble` hardware backends use this same path |
| `backend_stub.c` | the **stand-in encoder** for a host with no MediaCodec (below). In `liboneencoder.a` too: xnuhost runs it in its own process on the PC |
| `encctl.c` → `encctl` | the phone test: the whole host path in one process (direct MediaCodec, no helper), encoding a test pattern as root |
| `tests/test_standin.c`, `tests/standin_reader.h`, `tests/decode.sh` | the stand-in's tests (below) |
| `tests/test_encoder.c` | the device's test (below) |

## The stand-in encoder (`backend_stub.c`)

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

**Tested [F, Linux PC, 2026-11-15]:** `make test` runs `test_standin` (173 checks: 16 streams, 64x64 to 1440x3120,
sizes that are not whole blocks, a black frame, a small buffer; the NAL order, the SPS's profile bytes, the first
block's arithmetic-coded bytes `86 80` worked out by hand from the standard, every sample within the PCM step of the
frame), then `tests/decode.sh`: **ffmpeg n9.0.2 decodes all 16 streams with no warning to exactly the picture given**
(`cmp` of the raw 4:2:0 output with what the test expects; skipped with a line when there is no ffmpeg).

## The device against the guest (`media_device.c`) — what the 2026-11-15 reading of the guest's code changed

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
sleeps until a command or a kick.

**Tested [F]:** `test_encoder`, 214 checks: the device driven over xnuhost's real transport (`virtio.c`) by a guest of
plain memory that now does MMAP, CLOSE and ENUM_FMT **as `DroidVMVirtioMedia.c` does** (the place in the pool with
the guest's bound, CLOSE with no answer buffer, the format lists), replaying `DroidVMVTHost`'s encoder sequence
(OPEN, S_FMT HEVC+NV12, REQBUFS/QUERYBUF/MMAP, STREAMON, QBUF a NV12 test pattern, the coded DQBUF, ENCODER_CMD STOP →
EOS), then five more sessions opened and closed one after another (more than the device has slots). Twice: through
the **real helper process** built for the host with the stand-in, and with the stand-in in the device's own process
(xnuhost's PC arrangement). The coded picture is read back and compared with the pattern.
**Not tested here:** the real guest transport against the device outside a boot (its MMIO accessors are arm64
assembly); the PC boot runs its start-up check (OPEN, ENUM_FMT, CLOSE), and a session only when iOS encodes.

## Into xnuhost

`liboneencoder.a` links into xnuhost (static). `main.c` carves the media pool (after the GPU pool, inside the 64 MiB
the hosted kernel lets a task map, `0024-gpu-pool.patch`), creates the device, and publishes `/arm-io/virtio-media-0`
with `media-host-pool`; it adds `droidvm-vt-paravirt=1` to the guest's boot arguments when the device exists.

| `encoder=` | What |
|---|---|
| `off` | the default (until a phone session proves it): no pool, no device |
| `auto` | in a session (the bridges on): the phone's MediaCodec through `bin/oneios-encoder` (`encoder: on: mediacodec-helper backend, ...`; `encoder: off: no bin/oneios-encoder ...` when the helper is not in the runtime). With the bridges off (a PC boot, `--mode bench`): the stand-in inside xnuhost (`encoder: on: stand-in backend, ...` and `encoder: the stand-in encoder (encoder=auto with the bridges off: no phone): ...`) |
| `sim` | the stand-in always, also in a session (`encoder: the stand-in encoder (encoder=sim): ...`): iOS gets a hardware encoder whose pictures have few levels |

The stand-in is never taken in a session on the phone unless `encoder=sim` asks for it. Ship
`build/android/oneios-encoder` as `bin/oneios-encoder` (0755) in the runtime package (`runtime/build.sh` does).

## CLAUDE.md's bridge rules

- **One replaceable lookup, read-only to the system:** the device touches only its own media pool and the socket to
  the helper; the helper opens Android's MediaCodec and nothing of One UI's `/data`.
- **No realtime threads:** the device's worker is an ordinary `SCHED_OTHER` thread; the helper is a plain process.
- **Closed at the return:** the device's `reset` closes every encoder session; closing the socket ends the helper (it
  exits when its `read` returns 0), and MediaCodec is released per session. Nothing is grabbed, nothing persists.

## On the phone

The phone is not on this box. `runtime/build.sh STAGE` (or `make -C runtime/bridges/encoder`) on the Linux PC or the
Mac builds `bin/oneios-encoder` and `bin/encctl`; the runtime package carries both. Root through KernelSU, One UI
running:

```sh
adb push runtime/bridges/encoder/build/android/encctl runtime/bridges/encoder/build/android/oneios-encoder /data/local/tmp/
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

Then a session with `encoder=auto` in the image's `xnuhost.conf` (or `--set encoder=auto`); the lines that prove it
are in `kernel/patches/oneios-hosted/codec/README.md` ("What a session on the phone must show").

**Guess [I]:** NV12 as `COLOR_FormatYUV420SemiPlanar` ByteBuffer input to `c2.qti.hevc.encoder`; the still-image CQ
settings; that MediaCodec's unit for a frame at quality 90 fits width x height / 2 + 64 KiB (a larger one is dropped
and counted, point 4 above).
