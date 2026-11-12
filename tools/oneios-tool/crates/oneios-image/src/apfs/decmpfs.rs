//! Files kept compressed (decmpfs): the `com.apple.decmpfs` attribute holds a 16-byte header (`fpmc`, the kind,
//! the plain size) and, for the inline kinds, the data; the other kinds keep the data in the resource fork, in
//! chunks of 64 KiB of plain bytes. Kinds: 3 and 4 zlib, 7 and 8 LZVN, 9 and 10 stored, 11 and 12 LZFSE, 13 and 14
//! LZBITMAP (the odd one inline, the even one in the resource fork). iOS 27's OS volumes use LZBITMAP for most
//! files. The layouts are the ones apfs-fuse reads, and every compressed file of the 24A446 SystemOS and AppOS
//! images comes out as apfs-fuse gives it (tools/oneios-tool/README.md "The APFS reader").
//!
//! Decoders, all libraries: `flate2` (zlib), `lzfse_rust` (LZFSE), `zbitmap` (LZBITMAP). A decmpfs LZVN stream
//! is a bare LZVN payload; lzfse_rust decodes LZVN only as a `bvxn` block of an LZFSE stream, so the payload is
//! given that block's 12-byte header and the stream's end mark: no LZVN decoder of our own.

use std::io::Read;

const CHUNK: usize = 0x10000;

fn u32_le(d: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?) as usize)
}

/// The kind and the plain size of a decmpfs attribute.
pub fn header(attr: &[u8]) -> Option<(u32, u64)> {
    if attr.get(..4)? != b"fpmc" {
        return None;
    }
    Some((u32::from_le_bytes(attr.get(4..8)?.try_into().ok()?), u64::from_le_bytes(attr.get(8..16)?.try_into().ok()?)))
}

pub fn in_resource_fork(kind: u32) -> bool {
    matches!(kind, 4 | 8 | 10 | 12 | 14)
}

fn zlib(src: &[u8], want: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(want);
    flate2::read::ZlibDecoder::new(src).take(want as u64 + 1).read_to_end(&mut out).map_err(|e| format!("zlib: {e}"))?;
    Ok(out)
}

fn lzfse(src: &[u8], want: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(want);
    lzfse_rust::decode_bytes(src, &mut out).map_err(|e| format!("LZFSE: {e}"))?;
    Ok(out)
}

/// A bare LZVN payload, as one `bvxn` block of an LZFSE stream.
fn lzvn(src: &[u8], want: usize) -> Result<Vec<u8>, String> {
    let mut stream = Vec::with_capacity(src.len() + 16);
    stream.extend_from_slice(b"bvxn");
    stream.extend_from_slice(&(want as u32).to_le_bytes());
    stream.extend_from_slice(&(src.len() as u32).to_le_bytes());
    stream.extend_from_slice(src);
    stream.extend_from_slice(b"bvx$");
    let mut out = Vec::with_capacity(want);
    lzfse_rust::decode_bytes(&stream, &mut out).map_err(|e| format!("LZVN: {e}"))?;
    Ok(out)
}

/// One piece (the inline data, or a chunk of the resource fork) of `want` plain bytes.
fn piece(kind: u32, src: &[u8], want: usize) -> Result<Vec<u8>, String> {
    let first = *src.first().ok_or("an empty piece")?;
    let stored = || src[1..].to_vec();
    let out = match kind {
        3 | 4 if first == 0x78 => zlib(src, want)?,
        3 if first == 0xff => stored(),
        4 if first & 0x0f == 0x0f => stored(),
        3 | 4 => return Err(format!("zlib data starts with {first:#04x}")),
        7 | 8 if first == 0x06 => stored(),
        7 | 8 => lzvn(src, want)?,
        9 | 10 => stored(),
        11 | 12 => lzfse(src, want)?,
        13 | 14 if first == 0xff => stored(),
        13 | 14 => {
            let mut out = Vec::with_capacity(want);
            zbitmap::decompress(src, &mut out, want).map_err(|e| format!("LZBITMAP: {e}"))?;
            out
        }
        _ => return Err(format!("decmpfs kind {kind} is not known")),
    };
    if out.len() != want {
        return Err(format!("a piece decoded to {} bytes, {want} were expected", out.len()));
    }
    Ok(out)
}

/// The plain contents of a compressed file: `attr` is its `com.apple.decmpfs` attribute, `fork` reads its
/// resource fork when the kind keeps the data there.
pub fn decode(attr: &[u8], fork: impl FnOnce() -> Result<Vec<u8>, String>) -> Result<Vec<u8>, String> {
    let (kind, size) = header(attr).ok_or("the decmpfs attribute has no fpmc header")?;
    let size = usize::try_from(size).map_err(|_| "the plain size does not fit")?;
    if !in_resource_fork(kind) {
        return if size == 0 { Ok(Vec::new()) } else { piece(kind, &attr[16..], size) };
    }
    let fork = fork()?;
    let chunks = size.div_ceil(CHUNK);
    // where chunk k is in the fork: (start, length)
    let place = |k: usize| -> Option<(usize, usize)> {
        if kind == 4 {
            // a resource fork: big-endian header, the data's place first; there a length, then the table
            let base = u32::from_be_bytes(fork.get(..4)?.try_into().ok()?) as usize + 4;
            if k >= u32_le(&fork, base)? {
                return None;
            }
            Some((base + u32_le(&fork, base + 4 + 8 * k)?, u32_le(&fork, base + 8 + 8 * k)?))
        } else {
            // a list of chunks + 1 offsets from the fork's start
            let start = u32_le(&fork, 4 * k)?;
            Some((start, u32_le(&fork, 4 * k + 4)?.checked_sub(start)?))
        }
    };
    let mut out = Vec::with_capacity(size);
    for k in 0..chunks {
        let want = (size - k * CHUNK).min(CHUNK);
        let (start, len) = place(k).ok_or_else(|| format!("the resource fork has no chunk {k} of {chunks}"))?;
        let src = fork.get(start..start + len).ok_or_else(|| format!("chunk {k} is outside the resource fork"))?;
        out.extend_from_slice(&piece(kind, src, want).map_err(|e| format!("chunk {k}: {e}"))?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn attr(kind: u32, size: usize, data: &[u8]) -> Vec<u8> {
        let mut a = b"fpmc".to_vec();
        a.extend_from_slice(&kind.to_le_bytes());
        a.extend_from_slice(&(size as u64).to_le_bytes());
        a.extend_from_slice(data);
        a
    }

    fn deflate(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    fn none() -> Result<Vec<u8>, String> {
        Err("no fork".into())
    }

    #[test]
    fn inline_kinds() {
        let plain = b"hello hello hello hello hello hello".to_vec();
        assert_eq!(decode(&attr(3, plain.len(), &deflate(&plain)), none).unwrap(), plain);
        let mut stored = vec![0xff];
        stored.extend_from_slice(&plain);
        assert_eq!(decode(&attr(3, plain.len(), &stored), none).unwrap(), plain);
        stored[0] = 0x06;
        assert_eq!(decode(&attr(7, plain.len(), &stored), none).unwrap(), plain);
        stored[0] = 0xcc;
        assert_eq!(decode(&attr(9, plain.len(), &stored), none).unwrap(), plain);
        let mut packed = Vec::new();
        lzfse_rust::encode_bytes(&plain, &mut packed).unwrap();
        assert_eq!(decode(&attr(11, plain.len(), &packed), none).unwrap(), plain);
        assert_eq!(decode(&attr(3, 0, &[]), none).unwrap(), b"");
        // a bare LZVN payload: a literal of 5 bytes (opcode 0xe5), then the end of the stream
        let mut vn = vec![0xe5];
        vn.extend_from_slice(b"abcde");
        vn.extend_from_slice(&[0x06, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(decode(&attr(7, 5, &vn), none).unwrap(), b"abcde");
        // LZBITMAP: "hello" as one stored chunk and the end chunk (the zbitmap crate's own example), and stored whole
        assert_eq!(decode(&attr(13, 5, b"ZBM\x09\x0b\x00\x00\x05\x00\x00hello\x06\x00\x00\x00\x00\x00"), none).unwrap(), b"hello");
        assert_eq!(decode(&attr(13, 5, b"\xffhello"), none).unwrap(), b"hello");
        assert!(decode(&attr(13, 5, &[1, 2]), none).unwrap_err().contains("LZBITMAP"));
        assert!(decode(&attr(99, 5, &[1, 2]), none).unwrap_err().contains("not known"));
        assert!(decode(&attr(3, plain.len() + 1, &deflate(&plain)), none).unwrap_err().contains("were expected"));
        assert!(decode(b"nope", none).is_err());
    }

    #[test]
    fn resource_fork_kinds() {
        // 64 KiB + 100 bytes: two chunks
        let plain: Vec<u8> = (0..CHUNK + 100).map(|i| (i % 251) as u8).collect();
        let (a, b) = plain.split_at(CHUNK);
        // kind 4: a resource fork header, then a length, the count, (offset, length) pairs from the count's place
        let (za, zb) = (deflate(a), { let mut s = vec![0x0f]; s.extend_from_slice(b); s });
        let mut fork = Vec::new();
        fork.extend_from_slice(&0x100u32.to_be_bytes());
        fork.resize(0x100, 0);
        fork.extend_from_slice(&[0; 4]);
        fork.extend_from_slice(&2u32.to_le_bytes());
        let first = 4 + 16;
        for (off, len) in [(first, za.len()), (first + za.len(), zb.len())] {
            fork.extend_from_slice(&(off as u32).to_le_bytes());
            fork.extend_from_slice(&(len as u32).to_le_bytes());
        }
        fork.extend_from_slice(&za);
        fork.extend_from_slice(&zb);
        assert!(decode(&attr(4, plain.len(), &[]), || Ok(fork.clone())).unwrap() == plain);
        // kind 12: offsets from the fork's start
        let (mut la, mut lb) = (Vec::new(), Vec::new());
        lzfse_rust::encode_bytes(a, &mut la).unwrap();
        lzfse_rust::encode_bytes(b, &mut lb).unwrap();
        let mut fork = Vec::new();
        for off in [12, 12 + la.len(), 12 + la.len() + lb.len()] {
            fork.extend_from_slice(&(off as u32).to_le_bytes());
        }
        fork.extend_from_slice(&la);
        fork.extend_from_slice(&lb);
        assert!(decode(&attr(12, plain.len(), &[]), || Ok(fork.clone())).unwrap() == plain);
        // kind 8 with stored chunks
        let mut fork = Vec::new();
        for off in [12, 13 + CHUNK, 14 + CHUNK + 100] {
            fork.extend_from_slice(&(off as u32).to_le_bytes());
        }
        fork.push(0x06);
        fork.extend_from_slice(a);
        fork.push(0x06);
        fork.extend_from_slice(b);
        assert!(decode(&attr(8, plain.len(), &[]), || Ok(fork.clone())).unwrap() == plain);
        fork.truncate(40);
        assert!(decode(&attr(8, plain.len(), &[]), || Ok(fork.clone())).unwrap_err().contains("outside the resource fork"));
    }
}
