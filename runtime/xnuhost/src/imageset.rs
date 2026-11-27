// SPDX-License-Identifier: MIT
//! Port of imageset.c and imageset.h: the image set as xnuhost reads it (docs/image-format.md, format 1):
//! manifest.json, the chunk hash lists of the read-only images, Apple's trust caches (IM4P) and the userland overlay.
//!
//! xnuhost is the root of trust of a hosted kernel (it holds the kernel's memory), so the verified root of UniXNU
//! (signed block hashes checked by the guest's NativeVirtioBlock) is done on this side: every 1 MiB chunk of os.img,
//! systemos.img and app.img is checked against its hash list before the guest sees a byte of it, and the hash list
//! against the manifest. The kernel is told which disk is which, and that it is verified, in the disk's device tree
//! node ([`describe`]).

use std::cell::RefCell;
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::sync::Mutex;

use xh_core::bytes::le;
use xnuloader::adt::Node;

/// The files of a manifest xnuhost reads at most.
pub const MAX_FILES: usize = 64;

/// Why the image set cannot be used (the C's *error texts).
pub type Error = &'static str;

// ---- SHA-256 (FIPS 180-4), for the hash lists and the manifest's file hashes -------------------------------------

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
    0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
    0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
    0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
    0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
    0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
    0xc67178f2,
];

/// An incremental SHA-256.
#[derive(Clone)]
pub struct Sha256 {
    h: [u32; 8],
    bytes: u64,
    block: [u8; 64],
    fill: usize,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

/// The CPU's SHA-256 instructions where it has them (every phone this runs on): the first read of each 1 MiB of an
/// image is hashed here, 5.8 GiB of shared cache before the guest's first process gets anywhere.
#[cfg(target_arch = "aarch64")]
mod cpu {
    use super::K;
    use std::arch::aarch64::*;

    pub fn available() -> bool {
        std::arch::is_aarch64_feature_detected!("sha2")
    }

    /// One block with the SHA2 instructions.
    ///
    /// # Safety
    /// The CPU has the SHA2 extension ([`available`]).
    #[target_feature(enable = "sha2")]
    pub unsafe fn block(h: &mut [u32; 8], p: &[u8; 64]) {
        // SAFETY: the loads read 16 bytes inside `h` (8 u32) and `p` (64 bytes) and K (64 u32); the caller checked
        // the extension.
        unsafe {
            let mut abcd = vld1q_u32(h.as_ptr());
            let mut efgh = vld1q_u32(h.as_ptr().add(4));
            let (abcd0, efgh0) = (abcd, efgh);
            let mut w = [vdupq_n_u32(0); 4];
            for (i, wi) in w.iter_mut().enumerate() {
                *wi = vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(p.as_ptr().add(16 * i))));
            }
            for i in 0..16 {
                let t = vaddq_u32(w[i & 3], vld1q_u32(K.as_ptr().add(4 * i)));
                let a = abcd;
                abcd = vsha256hq_u32(abcd, efgh, t);
                efgh = vsha256h2q_u32(efgh, a, t);
                if i < 12 {
                    w[i & 3] =
                        vsha256su1q_u32(vsha256su0q_u32(w[i & 3], w[(i + 1) & 3]), w[(i + 2) & 3], w[(i + 3) & 3]);
                }
            }
            vst1q_u32(h.as_mut_ptr(), vaddq_u32(abcd, abcd0));
            vst1q_u32(h.as_mut_ptr().add(4), vaddq_u32(efgh, efgh0));
        }
    }
}

fn block_portable(h: &mut [u32; 8], p: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (i, word) in p.chunks_exact(4).enumerate() {
        w[i] = u32::from_be_bytes(word.try_into().expect("4 bytes"));
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
    }
    let mut v = *h;
    for i in 0..64 {
        let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
        let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
        let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
        let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
        let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
        let t2 = s0.wrapping_add(maj);
        v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
    }
    for (a, b) in h.iter_mut().zip(v) {
        *a = a.wrapping_add(b);
    }
}

fn block(h: &mut [u32; 8], p: &[u8; 64]) {
    #[cfg(target_arch = "aarch64")]
    {
        use std::sync::OnceLock;
        static CPU: OnceLock<bool> = OnceLock::new();
        if *CPU.get_or_init(cpu::available) {
            // SAFETY: the CPU has the SHA2 extension (checked once above).
            unsafe { cpu::block(h, p) };
            return;
        }
    }
    block_portable(h, p);
}

impl Sha256 {
    pub fn new() -> Self {
        Sha256 {
            h: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19],
            bytes: 0,
            block: [0; 64],
            fill: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.bytes += data.len() as u64;
        if self.fill != 0 {
            let take = (64 - self.fill).min(data.len());
            self.block[self.fill..self.fill + take].copy_from_slice(&data[..take]);
            self.fill += take;
            data = &data[take..];
            if self.fill < 64 {
                return;
            }
            let b = self.block;
            block(&mut self.h, &b);
            self.fill = 0;
        }
        let mut chunks = data.chunks_exact(64);
        for c in &mut chunks {
            block(&mut self.h, c.try_into().expect("64 bytes"));
        }
        let rest = chunks.remainder();
        self.block[..rest.len()].copy_from_slice(rest);
        self.fill = rest.len();
    }

    pub fn finalize(mut self) -> [u8; 32] {
        let bits = self.bytes.wrapping_mul(8);
        let pad = [0x80u8; 1];
        let zeros = [0u8; 64];
        let need = if self.fill < 56 { 56 } else { 120 } - self.fill;
        self.update(&pad);
        self.update(&zeros[..need - 1]);
        self.update(&bits.to_be_bytes());
        let mut out = [0u8; 32];
        for (o, h) in out.chunks_exact_mut(4).zip(self.h) {
            o.copy_from_slice(&h.to_be_bytes());
        }
        out
    }
}

/// The SHA-256 of `data`.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut s = Sha256::new();
    s.update(data);
    s.finalize()
}

/// 64 lower-case hex digits.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---- files -------------------------------------------------------------------------------------------------------

/// The whole file `<dir>/<name>` when it is at most `limit` bytes.
fn slurp(dir: &str, name: &str, limit: u64) -> io::Result<Vec<u8>> {
    let path = format!("{dir}/{name}");
    if path.len() >= 700 {
        return Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG));
    }
    let file = File::open(&path)?;
    if file.metadata()?.len() > limit {
        return Err(io::Error::from_raw_os_error(libc::EFBIG));
    }
    let mut data = Vec::new();
    io::Read::read_to_end(&mut &file, &mut data)?;
    Ok(data)
}

/// Exactly `out.len()` bytes as hex digits, nothing after.
fn hex_bytes(text: &str, out: &mut [u8]) -> bool {
    if text.len() != out.len() * 2 || !text.bytes().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).expect("hex digits");
    }
    true
}

// ---- JSON, as much as the manifest needs ---------------------------------------------------------------------------
// Values are addressed by their first byte's index in one text; the end of the text reads as the C's NUL.

#[derive(Clone, Copy)]
struct Json<'a>(&'a [u8]);

impl<'a> Json<'a> {
    fn at(self, i: usize) -> u8 {
        self.0.get(i).copied().unwrap_or(0)
    }

    fn ws(self, mut p: usize) -> usize {
        while matches!(self.at(p), b' ' | b'\t' | b'\n' | b'\r') {
            p += 1;
        }
        p
    }

    /// p at the opening quote; after the closing one.
    fn string_end(self, mut p: usize) -> Option<usize> {
        p += 1;
        while self.at(p) != 0 && self.at(p) != b'"' {
            if self.at(p) == b'\\' {
                p += 1;
                if self.at(p) == 0 {
                    return None;
                }
            }
            p += 1;
        }
        (self.at(p) != 0).then_some(p + 1)
    }

    /// After the value at p.
    fn skip(self, p: usize, depth: u32) -> Option<usize> {
        let mut p = self.ws(p);
        if depth > 32 {
            return None;
        }
        let c = self.at(p);
        if c == b'"' {
            return self.string_end(p);
        }
        if c == b'{' || c == b'[' {
            let (close, object) = if c == b'{' { (b'}', true) } else { (b']', false) };
            p = self.ws(p + 1);
            if self.at(p) == close {
                return Some(p + 1);
            }
            loop {
                if object {
                    if self.at(p) != b'"' {
                        return None;
                    }
                    p = self.ws(self.string_end(p)?);
                    if self.at(p) != b':' {
                        return None;
                    }
                    p += 1;
                }
                p = self.ws(self.skip(p, depth + 1)?);
                if self.at(p) == close {
                    return Some(p + 1);
                }
                if self.at(p) != b',' {
                    return None;
                }
                p = self.ws(p + 1);
            }
        }
        let start = p;
        while !matches!(self.at(p), 0 | b',' | b'}' | b']' | b' ' | b'\n' | b'\r' | b'\t') {
            p += 1;
        }
        (p > start).then_some(p)
    }

    /// The value of `key` in the object at `object`.
    fn get(self, object: Option<usize>, key: &str) -> Option<usize> {
        let mut p = self.ws(object?);
        if self.at(p) != b'{' {
            return None;
        }
        p = self.ws(p + 1);
        while self.at(p) == b'"' {
            let name = p + 1;
            let end = self.string_end(p)?;
            p = self.ws(end);
            if self.at(p) != b':' {
                return None;
            }
            p = self.ws(p + 1);
            if &self.0[name..end - 1] == key.as_bytes() {
                return Some(p);
            }
            p = self.ws(self.skip(p, 0)?);
            if self.at(p) == b',' {
                p = self.ws(p + 1);
            }
        }
        None
    }

    /// First element (None: empty, or not an array).
    fn first(self, array: Option<usize>) -> Option<usize> {
        let p = self.ws(array?);
        if self.at(p) != b'[' {
            return None;
        }
        let p = self.ws(p + 1);
        (self.at(p) != b']').then_some(p)
    }

    fn next(self, element: usize) -> Option<usize> {
        let p = self.ws(self.skip(element, 0)?);
        (self.at(p) == b',').then(|| self.ws(p + 1))
    }

    /// The elements of the array at `array`.
    fn elements(self, array: Option<usize>) -> impl Iterator<Item = usize> + 'a {
        std::iter::successors(self.first(array), move |&e| self.next(e))
    }

    /// A plain string shorter than `size` (a C buffer's room): an escape is refused.
    fn str(self, value: Option<usize>, size: usize) -> Option<String> {
        let v = value?;
        if self.at(v) != b'"' {
            return None;
        }
        let rest = &self.0[v + 1..];
        let end = rest.iter().position(|&c| c == b'"')?;
        let s = &rest[..end];
        if s.len() >= size || s.contains(&b'\\') {
            return None;
        }
        String::from_utf8(s.to_vec()).ok()
    }

    fn u64(self, value: Option<usize>) -> Option<u64> {
        let v = value?;
        if !self.at(v).is_ascii_digit() {
            return None;
        }
        let n = self.0[v..].iter().take_while(|c| c.is_ascii_digit()).count();
        let number = std::str::from_utf8(&self.0[v..v + n]).ok()?.parse().ok()?;
        matches!(self.at(v + n), b',' | b'}' | b']' | b' ' | b'\n' | b'\r' | 0).then_some(number)
    }
}

// ---- the manifest --------------------------------------------------------------------------------------------------

/// docs/image-format.md section 3: relative, plain segments.
fn path_ok(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|segment| {
            !segment.is_empty()
                && !segment.starts_with('.')
                && segment.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'+' | b'-'))
        })
}

/// One file of the manifest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageFile {
    /// relative, as in the manifest
    pub path: String,
    pub role: String,
    /// its chunk hash list ("" = none)
    pub hashes: String,
    /// first APFS volume ("" = not an image)
    pub volume_uuid: String,
    pub size: u64,
    pub sha256: [u8; 32],
}

/// The image set's manifest (xh_imageset).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageSet {
    pub dir: String,
    pub build: String,
    pub data_uuid: String,
    pub data_name: String,
    pub store_gib: u32,
    pub files: Vec<ImageFile>,
}

impl ImageSet {
    /// xh_imageset_load: Ok(Some) read; Ok(None) the folder has no manifest.json (a bench folder: nothing is
    /// verified, and the log says so); Err a manifest that cannot be used, and why.
    pub fn load(dir: &str) -> Result<Option<ImageSet>, Error> {
        if dir.len() >= 512 {
            return Err("the image folder's path is too long");
        }
        let json = match slurp(dir, "manifest.json", 4 << 20) {
            Ok(j) => j,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("manifest.json cannot be read"),
        };
        let j = Json(&json);
        if json.contains(&0) || j.skip(0, 0).is_none() {
            return Err("manifest.json is not JSON");
        }
        let root = Some(0);
        if j.u64(j.get(root, "format")) != Some(1) {
            return Err("manifest.json: format is not 1");
        }
        if j.str(j.get(root, "name"), 64).as_deref() != Some("oneios-image") {
            return Err("manifest.json: name is not oneios-image");
        }
        let mut set = ImageSet { dir: dir.to_owned(), ..ImageSet::default() };
        set.build = j.str(j.get(root, "build"), 16).ok_or("manifest.json: no build")?;
        let data = j.get(root, "data");
        if data.is_some_and(|d| j.at(d) == b'{') {
            match (j.str(j.get(data, "volume_uuid"), 40), j.str(j.get(data, "volume_name"), 32)) {
                (Some(uuid), Some(name)) => (set.data_uuid, set.data_name) = (uuid, name),
                _ => return Err("manifest.json: data.volume_uuid or volume_name"),
            }
            if let Some(gib) = j.u64(j.get(data, "store_gib")).filter(|n| (1..=4096).contains(n)) {
                set.store_gib = gib as u32;
            }
        }
        for entry in j.elements(j.get(root, "files")) {
            if set.files.len() == MAX_FILES {
                return Err("manifest.json lists more files than xnuhost reads");
            }
            let e = Some(entry);
            let mut f = ImageFile {
                path: j.str(j.get(e, "path"), 96).filter(|p| path_ok(p)).ok_or("manifest.json: a file's path")?,
                size: j.u64(j.get(e, "size")).ok_or("manifest.json: a file's size")?,
                ..ImageFile::default()
            };
            let digest = j.str(j.get(e, "sha256"), 72);
            if !digest.is_some_and(|d| hex_bytes(&d, &mut f.sha256)) {
                return Err("manifest.json: a file's sha256");
            }
            if let Some(role) = j.get(e, "role") {
                f.role = j.str(Some(role), 24).ok_or("manifest.json: a file's role")?;
            }
            if let Some(hashes) = j.get(e, "hashes") {
                f.hashes = j.str(Some(hashes), 96).filter(|p| path_ok(p)).ok_or("manifest.json: a file's hash list")?;
            }
            let volume = j.first(j.get(j.get(e, "apfs"), "volumes"));
            if volume.is_some() {
                f.volume_uuid = j.str(j.get(volume, "uuid"), 40).ok_or("manifest.json: an APFS volume's uuid")?;
            }
            set.files.push(f);
        }
        if set.files.is_empty() {
            return Err("manifest.json lists no files");
        }
        Ok(Some(set))
    }

    /// xh_imageset_find: the entry whose file is `path` (absolute, under the set's folder) or whose manifest path is
    /// `path`.
    pub fn find(&self, path: &str) -> Option<&ImageFile> {
        let path = match path.strip_prefix(self.dir.as_str()).and_then(|p| p.strip_prefix('/')) {
            Some(rel) if !self.dir.is_empty() => rel,
            _ => path,
        };
        self.files.iter().find(|f| f.path == path)
    }

    /// xh_imageset_role: the first entry of a role.
    pub fn role(&self, role: &str) -> Option<&ImageFile> {
        self.files.iter().find(|f| f.role == role)
    }

    /// A listed file, whole, checked against the manifest's size and SHA-256.
    fn listed(&self, path: &str, limit: u64) -> Result<Vec<u8>, Error> {
        let f = self.find(path).ok_or("a file xnuhost needs is not in manifest.json")?;
        let data = slurp(&self.dir, &f.path, limit).map_err(|_| "a file of the image set cannot be read")?;
        if data.len() as u64 != f.size || sha256(&data) != f.sha256 {
            return Err("a file of the image set is not the one manifest.json lists");
        }
        Ok(data)
    }

    /// xh_imageset_trustcaches: DarwinBoot's sidecar (u32 count, u32 offsets[count], version 2 modules) from the set's
    /// three IM4P trust caches and, when overlay/userland/owner-trust.json lists CDHashes, one owner module.
    pub fn trustcaches(&self) -> Result<Vec<u8>, Error> {
        self.trustcaches_trusting(&[])
    }

    /// [`trustcaches`](Self::trustcaches) with CDHashes beside the image's own: the runtime's boot addon
    /// (`kernel/gpuaddon`, addon.rs) has code the set's `owner-trust.json` need not name — an image built without the
    /// plug-in has no such list at all — and this boot's trust cache is the only place that can carry it.
    pub fn trustcaches_trusting(&self, extra: &[[u8; 20]]) -> Result<Vec<u8>, Error> {
        const NAMES: [&str; 3] = ["trust/static.trustcache", "trust/system.trustcache", "trust/app.trustcache"];
        let mut modules: Vec<Vec<u8>> = Vec::new();
        for (i, name) in NAMES.iter().enumerate() {
            if self.find(name).is_none() {
                if i == 0 {
                    return Err("the image set has no trust/static.trustcache");
                }
                continue;
            }
            let file = self.listed(name, 16 << 20)?;
            let module = [b"trst", b"ltrs", b"trcs", b"trca"]
                .iter()
                .find_map(|t| im4p_payload(&file, t))
                .ok_or("a trust cache of the image set is not an IM4P trust cache")?;
            if module.len() < 24
                || le::u32(module, 0) != 2
                || module.len() as u64 != 24 + le::u32(module, 20) as u64 * 24
            {
                return Err("a trust cache's payload is not a version 2 module (compressed or another version)");
            }
            modules.push(module.to_vec());
        }
        if let Some(owner) = self.owner_module(extra)? {
            modules.push(owner);
        }
        pack_trust_modules(modules)
    }

    /// The owner module: version 2, a UUID derived from its content, the CDHashes of overlay/userland/owner-trust.json
    /// and of `extra` (the runtime's boot addon) sorted, each as a SHA-256 hash (type 2) with no flags. None for a set
    /// without the userland overlay and with no CDHashes to add.
    fn owner_module(&self, extra: &[[u8; 20]]) -> Result<Option<Vec<u8>>, Error> {
        const OWNER: &str = "overlay/userland/owner-trust.json";
        let mut entries: Vec<[u8; 24]> = Vec::new();
        if self.find(OWNER).is_some() {
            let json = self.listed(OWNER, 4 << 20)?;
            let j = Json(&json);
            for e in j.elements(j.get(Some(0), "cdhashes")) {
                let mut entry = [0u8; 24];
                let text = j.str(Some(e), 48);
                if !text.is_some_and(|t| hex_bytes(&t, &mut entry[..20])) {
                    return Err("owner-trust.json: a CDHash is not 40 hex digits");
                }
                entry[20] = 2;
                entries.push(entry);
            }
        }
        for hash in extra {
            let mut entry = [0u8; 24];
            entry[..20].copy_from_slice(hash);
            entry[20] = 2;
            entries.push(entry);
        }
        Ok(owner_module(entries))
    }
}

/// A standalone owner module for a runtime add-on, appended to a kernel trust-cache sidecar when the image set's trust
/// caches are not available. It only trusts the CDHashes explicitly packaged beside that add-on.
pub fn trustcaches_sidecar_trusting(data: &[u8], extra: &[[u8; 20]]) -> Result<Vec<u8>, Error> {
    if extra.is_empty() {
        return Ok(data.to_vec());
    }
    if !xnuloader::boot::trustcaches_valid(data) {
        return Err("the kernel trust-cache sidecar is malformed");
    }
    let count = le::u32(data, 0) as usize;
    if count >= 128 {
        return Err("the kernel trust-cache sidecar already has the maximum 128 modules");
    }
    let mut modules = Vec::with_capacity(count + 1);
    for i in 0..count {
        let start = le::u32(data, 4 + i * 4) as usize;
        let end = if i + 1 < count { le::u32(data, 8 + i * 4) as usize } else { data.len() };
        modules.push(data[start..end].to_vec());
    }
    let entries = extra
        .iter()
        .map(|hash| {
            let mut entry = [0u8; 24];
            entry[..20].copy_from_slice(hash);
            entry[20] = 2;
            entry
        })
        .collect();
    modules.push(owner_module(entries).ok_or("the add-on has no CDHashes to trust")?);
    pack_trust_modules(modules)
}

fn owner_module(mut entries: Vec<[u8; 24]>) -> Option<Vec<u8>> {
    if entries.is_empty() {
        return None;
    }
    entries.sort_by(|a, b| a[..20].cmp(&b[..20]));
    entries.dedup_by(|a, b| a[..20] == b[..20]);
    let body: Vec<u8> = entries.concat();
    let digest = sha256(&body);
    let mut module = vec![2, 0, 0, 0];
    module.extend_from_slice(&digest[..16]);
    module.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    module.extend_from_slice(&body);
    Some(module)
}

fn pack_trust_modules(modules: Vec<Vec<u8>>) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    out.extend_from_slice(&(modules.len() as u32).to_le_bytes());
    let mut at = 4 + modules.len() * 4;
    for module in &modules {
        out.extend_from_slice(&(at as u32).to_le_bytes());
        at += module.len();
    }
    for module in &modules {
        out.extend_from_slice(module);
    }
    if !xnuloader::boot::trustcaches_valid(&out) {
        return Err("the trust caches do not make a sidecar the loader takes (entries unsorted?)");
    }
    Ok(out)
}

// ---- chunk verification of a read-only image ----------------------------------------------------------------------

/// A chunk that differs from its hash or cannot be read; a disk's failed read, write or flush (blk.rs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IoFailed;

/// The chunk checks of one image (xh_verify).
pub struct Verify {
    /// the hash list file
    list: Vec<u8>,
    /// of the image
    size: u64,
    count: u64,
    chunk: u32,
    digest: [u8; 32],
    /// one bit per verified chunk
    done: Mutex<Vec<u8>>,
}

impl Verify {
    /// xh_verify_open: reads and checks the hash list of `file` (its SHA-256 is the manifest's).
    pub fn open(set: &ImageSet, file: &ImageFile) -> Result<Verify, Error> {
        if file.hashes.is_empty() {
            return Err("the image has no chunk hash list");
        }
        let list = set.listed(&file.hashes, 64 << 20)?;
        let size = list.len() as u64;
        if size < 32 || &list[..8] != b"OIHL1\0\0\0" || le::u32(&list, 12) != 1 {
            return Err("the chunk hash list is not oneios-hash-list-1 with SHA-256");
        }
        let chunk = le::u32(&list, 8);
        let (image, count) = (le::u64(&list, 16), le::u64(&list, 24));
        if !(4096..=16 << 20).contains(&chunk)
            || chunk & 511 != 0
            || image != file.size
            || count != image.div_ceil(chunk as u64)
            || count > (size - 32) / 32
            || size != 32 + count * 32
        {
            return Err("the chunk hash list does not describe its image");
        }
        let digest = set.find(&file.hashes).expect("listed").sha256;
        Ok(Verify {
            list,
            size: image,
            count,
            chunk,
            digest,
            done: Mutex::new(vec![0; (count as usize).div_ceil(8) + 1]),
        })
    }

    /// xh_verify_range: checks every chunk that [offset, offset + length) touches and that was not checked before, by
    /// reading it whole from `file`. Err: a chunk differs or cannot be read.
    ///
    /// Several vCPUs read at once (the guest hashes the shared cache on all of them): the chunk is read and hashed
    /// outside the lock, in a buffer of the calling thread; the lock covers only the bits. Two threads that meet on one
    /// unchecked chunk both check it.
    pub fn range(&self, file: &File, offset: u64, length: u64) -> Result<(), IoFailed> {
        thread_local! {
            static BUFFER: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
        }
        if length == 0 {
            return Ok(());
        }
        if offset >= self.size || length > self.size - offset {
            return Err(IoFailed);
        }
        let chunk = self.chunk as u64;
        for c in offset / chunk..=(offset + length - 1) / chunk {
            if self.is_done(c) {
                continue;
            }
            let at = c * chunk;
            let want = (self.size - at).min(chunk) as usize;
            let digest = BUFFER.with(|b| {
                let mut buffer = b.borrow_mut();
                if buffer.len() < want {
                    buffer.resize(chunk as usize, 0);
                }
                file.read_exact_at(&mut buffer[..want], at).map(|_| sha256(&buffer[..want]))
            });
            let i = 32 + c as usize * 32;
            if digest.ok().as_ref().map(|d| &d[..]) != Some(&self.list[i..i + 32]) {
                return Err(IoFailed);
            }
            self.done.lock().unwrap_or_else(|e| e.into_inner())[c as usize / 8] |= 1 << (c % 8);
        }
        Ok(())
    }

    fn is_done(&self, c: u64) -> bool {
        self.done.lock().unwrap_or_else(|e| e.into_inner())[c as usize / 8] & (1 << (c % 8)) != 0
    }

    /// The list's SHA-256: what the kernel publishes as the identity of a verified disk.
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }

    /// The image's chunks.
    pub fn chunks(&self) -> u64 {
        self.count
    }
}

// ---- trust caches --------------------------------------------------------------------------------------------------

/// One DER header at `p[at..]`: the length and where its content starts.
fn der(p: &[u8], at: usize, tag: u8) -> Option<(usize, usize)> {
    let q = p.get(at..)?;
    if q.len() < 2 || q[0] != tag {
        return None;
    }
    let mut n = q[1] as usize;
    let mut start = at + 2;
    if n & 0x80 != 0 {
        let bytes = n & 0x7f;
        if bytes == 0 || bytes > 4 || p.len() - start < bytes {
            return None;
        }
        n = p[start..start + bytes].iter().fold(0, |n, &b| n << 8 | b as usize);
        start += bytes;
    }
    (p.len() - start >= n).then_some((start, n))
}

/// xh_im4p_payload: the payload of an IM4P ("IM4P", type, description, OCTET STRING).
pub fn im4p_payload<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    let (start, n) = der(data, 0, 0x30)?;
    let body = &data[..start + n];
    let (p, n) = der(body, start, 0x16)?;
    if n != 4 || &body[p..p + 4] != b"IM4P" {
        return None;
    }
    let (p, n) = der(body, p + n, 0x16)?;
    if n != 4 || &body[p..p + 4] != kind {
        return None;
    }
    let (p, n) = der(body, p + n, 0x16)?; // the description
    let (p, n) = der(body, p + n, 0x04)?;
    Some(&body[p..p + n])
}

// ---- the device tree -----------------------------------------------------------------------------------------------

/// What the hosted kernel's storage reads from a disk's node (kernel/patches/oneios-hosted, README):
///   oneios,role         "os" | "systemos" | "app" | "data" | "seed"
///   oneios,verified     32 bytes, the SHA-256 of the disk's chunk hash list (only on a disk whose reads xnuhost checks)
///   oneios,volume-uuid  the APFS volume to mount from it (for "data": the UUID asked of newfs_apfs)
///   oneios,volume-name  "data" only
///   oneios,bytes        u64, the file's length ("seed": the disk is padded)
///   oneios,sha256       64 hex digits + NUL ("seed": what oneios-seed records in the volume)
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiskIdentity {
    /// "" = the disk has none (nothing is described)
    pub role: String,
    pub verified: Option<[u8; 32]>,
    pub volume_uuid: String,
    pub volume_name: String,
    pub sha256_hex: String,
    pub bytes: u64,
}

/// xh_imageset_describe: the disk's identity in its device tree node.
pub fn describe(node: &mut Node, identity: &DiskIdentity) {
    if identity.role.is_empty() {
        return;
    }
    // A property the node cannot take is left out, as in C (the kernel then treats the disk as unknown).
    let _ = node.set_str("oneios,role", &identity.role);
    if let Some(v) = &identity.verified {
        let _ = node.set("oneios,verified", v);
    }
    for (name, value) in [
        ("oneios,volume-uuid", &identity.volume_uuid),
        ("oneios,volume-name", &identity.volume_name),
        ("oneios,sha256", &identity.sha256_hex),
    ] {
        if !value.is_empty() {
            let _ = node.set_str(name, value);
        }
    }
    if identity.bytes != 0 {
        let _ = node.set_u64("oneios,bytes", identity.bytes);
    }
}

// ---- the versions line (docs/booter.md 2.9b) ------------------------------------------------------------------------
// One line that says which runtime, kernel, image and app made a log, at the start and again in the stop summary (a
// user's log is often its last lines). It reads two small JSON files and never fails: a missing or bad value is
// "unknown". The kernel collection is not hashed (100 MB): its SHA-256 is the image manifest's entry.

/// The environment variable the OneOS fork's switch sets to the app's versionCode (its docs/oneios.md).
pub const APP_VERSION_ENV: &str = "ONEIOS_APP_VERSION_CODE";

/// A value fit for the line: 1 to 80 of `[A-Za-z0-9._+,-]`.
fn token(value: Option<String>) -> Option<String> {
    value.filter(|v| {
        !v.is_empty() && v.len() <= 80 && v.bytes().all(|c| c.is_ascii_alphanumeric() || b"._+,-".contains(&c))
    })
}

/// A JSON file of `dir` (at most 4 MiB) when it parses.
fn json_file(dir: &str, name: &str) -> Option<Vec<u8>> {
    if dir.is_empty() {
        return None;
    }
    let text = slurp(dir, name, 4 << 20).ok()?;
    (!text.contains(&0) && Json(&text).skip(0, 0).is_some()).then_some(text)
}

/// `versions: runtime V (git C[ dirty]), kernel ID, kernel.kc S, image OS V B format F, app versionCode N`: the
/// runtime's `runtime.json` (beside `bin/`), the image's `manifest.json`, and `app` (the fork's versionCode, from
/// APP_VERSION_ENV). Each value it cannot read is "unknown"; "none" is a runtime or image that says it has no kernel.
pub fn versions_line(runtime: &str, image: &str, app: Option<&str>) -> String {
    const UNKNOWN: &str = "unknown";
    let or = |v: Option<String>| v.unwrap_or_else(|| UNKNOWN.to_string());
    let is_null = |j: Json, p: Option<usize>| p.is_some_and(|p| j.0[p..].starts_with(b"null"));
    let (mut version, mut commit, mut kernel) = (None, None, None);
    let mut dirty = false;
    if let Some(text) = json_file(runtime, "runtime.json") {
        let j = Json(&text);
        let root = Some(j.ws(0));
        version = token(j.str(j.get(root, "version"), 64));
        let git = j.get(root, "git");
        commit = token(j.str(j.get(git, "commit"), 41).filter(|c| c.len() >= 12).map(|c| c[..12].to_string()));
        dirty = j.get(git, "dirty").is_some_and(|p| j.0[p..].starts_with(b"true"));
        let k = j.get(root, "kernel");
        kernel = if is_null(j, k) { Some("none".to_string()) } else { token(j.str(j.get(k, "build_id"), 96)) };
    }
    let (mut kc, mut os, mut ios, mut build, mut format) = (None, None, None, None, None);
    if let Some(text) = json_file(image, "manifest.json") {
        let j = Json(&text);
        let root = Some(j.ws(0));
        os = token(j.str(j.get(root, "os"), 16));
        ios = token(j.str(j.get(root, "version"), 32));
        build = token(j.str(j.get(root, "build"), 16));
        format = j.u64(j.get(root, "format")).map(|f| f.to_string());
        let k = j.get(root, "kernel");
        kc = if is_null(j, k) {
            Some("none".to_string())
        } else {
            j.str(k, 96).and_then(|name| {
                j.elements(j.get(root, "files"))
                    .find(|&e| j.str(j.get(Some(e), "path"), 96).as_deref() == Some(name.as_str()))
                    .and_then(|e| j.str(j.get(Some(e), "sha256"), 65))
                    .filter(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()))
                    .map(|s| s[..12].to_string())
            })
        };
    }
    let app = app.filter(|a| !a.is_empty() && a.len() <= 9 && a.bytes().all(|c| c.is_ascii_digit()));
    format!(
        "versions: runtime {} (git {}{}), kernel {}, kernel.kc {}, image {} {} {} format {}, app {}",
        or(version),
        or(commit),
        if dirty { " dirty" } else { "" },
        or(kernel),
        or(kc),
        or(os),
        or(ios),
        or(build),
        or(format),
        app.map_or_else(|| UNKNOWN.to_string(), |a| format!("versionCode {a}"))
    )
}

#[cfg(test)]
mod tests;
