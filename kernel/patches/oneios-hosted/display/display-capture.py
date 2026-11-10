#!/usr/bin/env python3
"""Watch what xnuhost shows: a client of <session>/display.sock on the PC, in place of oneios-display.

usage: display-capture.py --session DIR [--out PREFIX] [--seconds N] [--every S]

Connects to DIR/display.sock (waiting for it to appear), takes the hello and the frame's memfd as oneios-display
does (docs/booter.md 2.8: hello {"XHD1", width, height, stride, format} with the memfd, then flush {"XHDF", x, y,
w, h} per changed rectangle), and prints one line when the picture changed since the last look: the time, the
flushes so far, the last rectangle, how many distinct colours the frame has and a hash of it. With --out it also
writes the frame as PREFIX-NNN.png each time it changed (at most one every --every seconds, default 5).

It replaces a helper that is connected (xnuhost serves one client), so do not run it beside oneios-display.
A boot's console is a few colours of text on black; the system's frames are not: the colour count tells them
apart without opening the picture.
"""
import argparse
import array
import hashlib
import mmap
import os
import select
import socket
import struct
import sys
import time
import zlib


def png(path, width, height, stride, frame):
    """B,G,R,X rows -> an RGB PNG."""
    rows = bytearray()
    for y in range(height):
        row = frame[y * stride:y * stride + width * 4]
        rgb = bytearray(width * 3)
        rgb[0::3], rgb[1::3], rgb[2::3] = row[2::4], row[1::4], row[0::4]
        rows += b'\0' + rgb

    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    with open(path, 'wb') as out:
        out.write(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 2, 0, 0, 0))
                  + chunk(b'IDAT', zlib.compress(bytes(rows), 3)) + chunk(b'IEND', b''))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--session', required=True)
    parser.add_argument('--out')
    parser.add_argument('--seconds', type=float, default=0, help='stop after this long (default: when xnuhost ends)')
    parser.add_argument('--every', type=float, default=5)
    args = parser.parse_args()
    path = os.path.join(args.session, 'display.sock')
    began = time.time()
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    while True:
        try:
            sock.connect(path)
            break
        except OSError:
            if args.seconds and time.time() - began > args.seconds:
                sys.exit(f'display-capture: no {path}')
            time.sleep(0.5)
    fds = array.array('i')
    data, ancillary, _, _ = sock.recvmsg(64, socket.CMSG_LEN(4 * 4))
    for level, kind, payload in ancillary:
        if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
            fds.frombytes(payload[:len(payload) - len(payload) % 4])
    if len(data) != 20 or data[:4] != b'XHD1' or not fds:
        sys.exit('display-capture: no hello with a memfd')
    width, height, stride, pixel_format = struct.unpack('<4I', data[4:])
    if pixel_format != 1 or not 0 < width <= 16384 or not 0 < height <= 16384 or stride < width * 4:
        sys.exit(f'display-capture: unusable hello {width}x{height} stride {stride} format {pixel_format}')
    frame = mmap.mmap(fds[0], stride * height, prot=mmap.PROT_READ)
    print(f'{time.time() - began:8.1f}s hello {width}x{height} stride {stride}', flush=True)
    flushes, last, seen, shots, looked = 0, None, None, 0, 0.0
    while not args.seconds or time.time() - began < args.seconds:
        ready, _, _ = select.select([sock], [], [], 1.0)
        if ready:
            try:
                message = sock.recv(64)
            except OSError:
                message = b''
            if not message:
                print(f'{time.time() - began:8.1f}s the socket closed after {flushes} flushes', flush=True)
                break
            if len(message) == 20 and message[:4] == b'XHDF':
                flushes += 1
                last = struct.unpack('<4I', message[4:])
        now = time.time()
        if flushes and now - looked >= args.every:
            looked = now
            picture = bytes(frame)
            digest = hashlib.sha256(picture).hexdigest()[:12]
            if digest != seen:
                seen = digest
                colours = len({picture[y * stride + x * 4:y * stride + x * 4 + 3] for y in range(0, height, 7) for x in range(0, width, 5)})
                note = ''
                if args.out:
                    shots += 1
                    note = f' -> {args.out}-{shots:03d}.png'
                    png(f'{args.out}-{shots:03d}.png', width, height, stride, picture)
                print(f'{now - began:8.1f}s {flushes} flushes, last {last}, {colours} colours sampled, frame {digest}{note}', flush=True)
    print(f'{time.time() - began:8.1f}s done: {flushes} flushes, {shots} pictures', flush=True)


if __name__ == '__main__':
    main()
