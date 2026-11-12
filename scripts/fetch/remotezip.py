#!/usr/bin/env python3
"""Read single members of a ZIP (an IPSW) without downloading the archive.

    remotezip.py ls  SOURCE [REGEX]                    size, packed size, method, CRC-32, name of each member
    remotezip.py cat SOURCE MEMBER                     a small member on stdout
    remotezip.py get SOURCE OUTDIR [-j N] MEMBER[=NAME]...   members into OUTDIR (NAME: file name there)
    remotezip.py head SOURCE MEMBER BYTES              the first BYTES of a stored member on stdout

SOURCE is an https URL (HTTP range requests) or a local file. `get` fetches a member's packed bytes in
parallel chunks into NAME.part; finished chunks are listed in NAME.part.done, so an interrupted run
resumes. Deflated members are inflated locally. The CRC-32 of the ZIP directory is checked before the
file gets its final name. Standard library only; Linux and macOS.

Connections try each address of the host for at most CONNECT_TRY seconds before the next one, and an address
family that failed to connect is tried last from then on. Without that, a host whose IPv6 has no route (seen on a
Linux PC on 2026-11-12: Apple's CDN answers AAAA first) waited the whole request timeout on each IPv6 address of
every range request before it reached IPv4, about 1.6 MB/s instead of the line's speed.

Same idea as iOSPlus10's `op10tool ipsw-get` and UniXNU's scripts/unixnu/ipsw.py.
"""
import concurrent.futures
import io
import os
import re
import socket
import struct
import sys
import threading
import time
import urllib.error
import urllib.request
import zipfile
import zlib

CHUNK = 8 << 20
AGENT = "oneios-image-build"
CONNECT_TRY = 5

_failed_families = set()
_system_create_connection = socket.create_connection


def _create_connection(address, timeout=socket._GLOBAL_DEFAULT_TIMEOUT, source_address=None, **kw):
    """socket.create_connection with a short try per address and failed families last (see the header)."""
    if kw:
        return _system_create_connection(address, timeout, source_address, **kw)
    host, port = address
    infos = socket.getaddrinfo(host, port, 0, socket.SOCK_STREAM)
    infos.sort(key=lambda i: i[0] in _failed_families)
    full = None if timeout is socket._GLOBAL_DEFAULT_TIMEOUT else timeout
    last = None
    for n, (family, kind, proto, _, addr) in enumerate(infos):
        s = socket.socket(family, kind, proto)
        try:
            s.settimeout(full if n == len(infos) - 1 else min(full or CONNECT_TRY, CONNECT_TRY))
            if source_address:
                s.bind(source_address)
            s.connect(addr)
            s.settimeout(full)
            _failed_families.discard(family)
            return s
        except OSError as e:
            s.close()
            _failed_families.add(family)
            last = e
    raise last or OSError(f"getaddrinfo returned nothing for {host}")


socket.create_connection = _create_connection


class Error(Exception):
    pass


class Source:
    """Random access to a local file or an https URL."""

    def __init__(self, spec, retries=6):
        self.spec, self.retries = spec, retries
        self.remote = spec.startswith(("http://", "https://"))
        if self.remote:
            req = urllib.request.Request(spec, method="HEAD", headers={"User-Agent": AGENT})
            with urllib.request.urlopen(req, timeout=60) as r:
                self.size = int(r.headers["Content-Length"])
        else:
            self.size = os.path.getsize(spec)

    def read(self, start, end):
        """Bytes start..end inclusive."""
        want = end - start + 1
        if want <= 0:
            return b""
        if not self.remote:
            with open(self.spec, "rb") as f:
                f.seek(start)
                data = f.read(want)
            if len(data) != want:
                raise Error(f"{self.spec}: short read at {start}")
            return data
        last = None
        for attempt in range(self.retries):
            if attempt:
                time.sleep(2 * attempt)
            try:
                req = urllib.request.Request(
                    self.spec, headers={"Range": f"bytes={start}-{end}", "User-Agent": AGENT})
                with urllib.request.urlopen(req, timeout=120) as r:
                    if r.status != 206:
                        raise Error(f"{self.spec}: the server ignores range requests (status {r.status})")
                    data = r.read()
                if len(data) == want:
                    return data
                last = Error(f"range {start}-{end}: got {len(data)} bytes")
            except (OSError, urllib.error.URLError) as e:
                last = e
        raise Error(f"{self.spec}: range {start}-{end} failed: {last}")


class _Stream(io.RawIOBase):
    """Seekable view of a Source with a small block cache, for zipfile's directory reads."""

    def __init__(self, source, block=1 << 20):
        self.source, self.block, self.pos, self.cache = source, block, 0, {}

    def readable(self):
        return True

    def seekable(self):
        return True

    def tell(self):
        return self.pos

    def seek(self, offset, whence=0):
        self.pos = {0: offset, 1: self.pos + offset, 2: self.source.size + offset}[whence]
        return self.pos

    def readinto(self, buf):
        want = min(len(buf), self.source.size - self.pos)
        done = 0
        while done < want:
            index, inside = divmod(self.pos, self.block)
            if index not in self.cache:
                if len(self.cache) >= 8:
                    self.cache.pop(next(iter(self.cache)))
                start = index * self.block
                self.cache[index] = self.source.read(start, min(start + self.block, self.source.size) - 1)
            data = self.cache[index][inside:inside + want - done]
            buf[done:done + len(data)] = data
            done += len(data)
            self.pos += len(data)
        return done


class RemoteZip:
    def __init__(self, spec):
        self.source = Source(spec)
        self.zip = zipfile.ZipFile(io.BufferedReader(_Stream(self.source), 1 << 20))

    def infos(self):
        return self.zip.infolist()

    def find(self, name):
        """The entry called name, or the only one whose path ends in /name."""
        names = [i.filename for i in self.zip.infolist()]
        if name in names:
            return self.zip.getinfo(name)
        hits = [n for n in names if n.endswith("/" + name)]
        if len(hits) != 1:
            raise Error(f"{name}: not in the archive" if not hits else f"{name}: {len(hits)} members match")
        return self.zip.getinfo(hits[0])

    def data_start(self, info):
        head = self.source.read(info.header_offset, info.header_offset + 29)
        if head[:4] != b"PK\x03\x04":
            raise Error(f"{info.filename}: no local header")
        nlen, elen = struct.unpack_from("<HH", head, 26)
        return info.header_offset + 30 + nlen + elen

    def read(self, name):
        return self.zip.read(self.find(name).filename)

    def head(self, name, count):
        info = self.find(name)
        if info.compress_type != zipfile.ZIP_STORED:
            raise Error(f"{info.filename}: not stored, cannot read its first bytes alone")
        start = self.data_start(info)
        return self.source.read(start, start + min(count, info.file_size) - 1)

    def get(self, name, out, jobs=8, chunk=CHUNK, log=print):
        """Fetch one member to the path out. Returns (size, crc32)."""
        info = self.find(name)
        if info.compress_type not in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED):
            raise Error(f"{info.filename}: compression method {info.compress_type} is not supported")
        if info.flag_bits & 1:
            raise Error(f"{info.filename}: encrypted ZIP member")
        if os.path.isfile(out) and os.path.getsize(out) == info.file_size and _crc_file(out) == info.CRC:
            log(f"{info.filename}: already complete")
            return info.file_size, info.CRC
        stored = info.compress_type == zipfile.ZIP_STORED
        part = out + (".part" if stored else ".deflate.part")
        done_path = part + ".done"
        size, start = info.compress_size, self.data_start(info)
        done = set()
        if os.path.isfile(done_path) and os.path.isfile(part) and os.path.getsize(part) == size:
            with open(done_path) as f:
                # the first line names what the chunk list belongs to
                if f.readline().strip() == f"{info.CRC:08x} {size} {chunk}":
                    done = {int(line) for line in f if line.strip().isdigit()}
        fd = os.open(part, os.O_RDWR | os.O_CREAT, 0o644)
        try:
            os.ftruncate(fd, size)
            todo = [k for k in range(-(-size // chunk)) if k not in done]
            lock = threading.Lock()
            got = [len(done) * chunk]
            with open(done_path, "a" if done else "w") as marks:
                if not done:
                    marks.write(f"{info.CRC:08x} {size} {chunk}\n")
                    marks.flush()

                def one(k):
                    st = k * chunk
                    en = min(st + chunk, size) - 1
                    os.pwrite(fd, self.source.read(start + st, start + en), st)
                    with lock:
                        marks.write(f"{k}\n")
                        marks.flush()
                        got[0] = min(got[0] + en - st + 1, size)
                        log(f"{info.filename}: {got[0] * 100 // max(size, 1)}% ({got[0] >> 20}/{size >> 20} MiB)")

                with concurrent.futures.ThreadPoolExecutor(max(1, jobs)) as pool:
                    for f in [pool.submit(one, k) for k in todo]:
                        f.result()
        except (Error, OSError) as e:
            raise Error(f"{info.filename}: download failed, run again to resume ({e})") from None
        finally:
            os.close(fd)
        tmp = part
        if stored:
            crc = _crc_file(part)
        else:
            log(f"{info.filename}: inflating")
            tmp, crc = out + ".part", 0
            z = zlib.decompressobj(-15)
            with open(part, "rb") as src, open(tmp, "wb") as dst:
                while block := src.read(1 << 24):
                    data = z.decompress(block)
                    crc = zlib.crc32(data, crc)
                    dst.write(data)
                data = z.flush()
                crc = zlib.crc32(data, crc)
                dst.write(data)
        if crc != info.CRC or os.path.getsize(tmp) != info.file_size:
            os.unlink(done_path)
            raise Error(f"{info.filename}: CRC-32 {crc:08x}, the archive says {info.CRC:08x}; chunk list reset, run again")
        os.replace(tmp, out)
        os.unlink(done_path)
        if not stored:
            os.unlink(part)
        log(f"{info.filename}: {info.file_size} bytes, CRC-32 ok -> {out}")
        return info.file_size, info.CRC


def _crc_file(path):
    crc = 0
    with open(path, "rb") as f:
        while block := f.read(1 << 24):
            crc = zlib.crc32(block, crc)
    return crc


def main(argv):
    if len(argv) < 3:
        sys.exit(__doc__)
    cmd, spec, rest = argv[1], argv[2], argv[3:]
    z = RemoteZip(spec)
    if cmd == "ls":
        pat = re.compile(rest[0]) if rest else None
        for i in z.infos():
            if not pat or pat.search(i.filename):
                method = {0: "stored", 8: "deflate"}.get(i.compress_type, str(i.compress_type))
                print(f"{i.file_size:>13} {i.compress_size:>13} {method:<7} {i.CRC:08x} {i.filename}")
    elif cmd == "cat":
        sys.stdout.buffer.write(z.read(rest[0]))
    elif cmd == "head":
        sys.stdout.buffer.write(z.head(rest[0], int(rest[1])))
    elif cmd == "get":
        outdir, jobs, members = rest[0], 8, []
        args = rest[1:]
        while args:
            a = args.pop(0)
            if a == "-j":
                jobs = int(args.pop(0))
            else:
                members.append(a)
        os.makedirs(outdir, exist_ok=True)
        for m in members:
            name, _, as_name = m.partition("=")
            z.get(name, os.path.join(outdir, as_name or os.path.basename(name)), jobs)
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    try:
        main(sys.argv)
    except Error as e:
        sys.exit(f"remotezip: {e}")
