#!/bin/sh
# decode.sh DIR: every stream test_standin wrote into DIR (NAME.hevc, NAME.h264) through a real decoder (ffmpeg), and
# its picture against NAME.yuv, the picture the stand-in encoder was given (at the PCM depth it wrote). A stream that
# a decoder refuses, warns about or decodes to another picture fails. Exit 0 all the same, 1 a difference, 2 usage.
[ $# -eq 1 ] && [ -d "$1" ] || { sed -n '2,4p' "$0" >&2; exit 2; }
command -v ffmpeg >/dev/null 2>&1 || { echo "decode: no ffmpeg here: the decoder comparison is skipped"; exit 0; }
fail=0; same=0
for stream in "$1"/*.hevc "$1"/*.h264; do
  [ -f "$stream" ] || continue
  name=${stream%.*}
  case $stream in *.hevc) format=hevc ;; *) format=h264 ;; esac
  # -xerror -err_detect explode: a decoder's complaint about the stream is an error, not a concealed picture.
  if ! ffmpeg -nostdin -v warning -xerror -err_detect explode -f "$format" -i "$stream" -f rawvideo -pix_fmt yuv420p -y "$name.decoded" > "$name.log" 2>&1 \
     || [ -s "$name.log" ]; then
    echo "decode: $(basename "$stream"): the decoder complained:"; sed 's/^/    /' "$name.log" | head -5; fail=1; continue
  fi
  if cmp -s "$name.decoded" "$name.yuv"; then same=$((same + 1)); else echo "decode: $(basename "$stream"): the decoded picture differs"; fail=1; fi
done
echo "decode: $same stream(s) decoded by $(ffmpeg -version | head -1 | cut -d' ' -f1-3) to the picture given"
[ "$same" -gt 0 ] || fail=1
exit $fail
