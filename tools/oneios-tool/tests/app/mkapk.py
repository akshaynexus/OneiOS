#!/usr/bin/env python3
"""A stand-in APK for tests/app/run.sh: mkapk.py OUT.apk CERT.der [TEXT]

A zip with one file and an APK Signing Block (scheme v2) whose one signer carries CERT.der. The signature and
the digests are filler: oneios-tool reads the certificate, it does not verify the APK (Android does, at install).
"""
import io, struct, sys, zipfile

out, cert = sys.argv[1], open(sys.argv[2], "rb").read()
text = sys.argv[3] if len(sys.argv) > 3 else "stand-in"

def lp(b):  # length-prefixed
    return struct.pack("<I", len(b)) + b

buf = io.BytesIO()
with zipfile.ZipFile(buf, "w", zipfile.ZIP_STORED) as z:
    z.writestr("stand-in.txt", text)
raw = buf.getvalue()
eocd = raw.rfind(b"PK\x05\x06")
cd_size, cd_offset = struct.unpack("<II", raw[eocd + 12:eocd + 20])

digests = lp(lp(struct.pack("<I", 0x0103) + lp(b"\0" * 32)))
signed = lp(digests) + lp(lp(cert)) + lp(b"")
signatures = lp(struct.pack("<I", 0x0103) + lp(b"\0" * 256))
signer = lp(signed) + lp(signatures) + lp(b"\0" * 32)
value = lp(lp(signer))
pair = struct.pack("<QI", len(value) + 4, 0x7109871A) + value
size = len(pair) + 8 + 16
block = struct.pack("<Q", size) + pair + struct.pack("<Q", size) + b"APK Sig Block 42"

apk = raw[:cd_offset] + block + raw[cd_offset:eocd]
apk += raw[eocd:eocd + 16] + struct.pack("<I", cd_offset + len(block)) + raw[eocd + 20:]
open(out, "wb").write(apk)
