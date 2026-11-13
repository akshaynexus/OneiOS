//! The layout of a kernel collection (`kernel.kc`, an MH_FILESET) and of a plain kernel (`xnu.macho`): the
//! members, each member's segments, each member's own symbols. All Mach-O reading is the `object` crate's
//! (header, load commands, `LC_FILESET_ENTRY`, `LC_SEGMENT_64`, `LC_SYMTAB`, nlist entries).

use anyhow::{anyhow, bail, Context, Result};
use object::macho::{self, MachHeader64, Nlist64};
use object::read::macho::{LoadCommandVariant, MachHeader, Nlist, Segment as _};
use object::LittleEndian as LE;

/// The fileset member that is the kernel.
pub const KERNEL_MEMBER: &str = "com.apple.kernel";

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Segment {
    pub name: String,
    pub vmaddr: u64,
    pub vmsize: u64,
}

impl Segment {
    pub fn holds(&self, addr: u64) -> bool {
        addr >= self.vmaddr && addr - self.vmaddr < self.vmsize
    }
}

/// A defined symbol of a symbol table (N_SECT, no stab).
#[derive(Clone, Debug, PartialEq)]
pub struct Symbol {
    pub addr: u64,
    pub name: String,
    pub external: bool,
    /// The segment of the symbol's section (index into the member's segments). An address alone does not say it:
    /// a symbol that marks an end sits on the next segment's first address, and a segment may have no size.
    pub segment: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct Member {
    /// The entry's id (`com.apple.kernel`, `com.apple.driver.X`), or the file's kind for a plain Mach-O.
    pub name: String,
    /// The address of the member's Mach-O header.
    pub vmaddr: u64,
    pub segments: Vec<Segment>,
    pub symbols: Vec<Symbol>,
}

/// A parsed Mach-O: a collection with its members, or a plain image as one member.
#[derive(Clone, Debug)]
pub struct Image {
    pub fileset: bool,
    pub uuid: Option<[u8; 16]>,
    /// The file's own (top-level) segments. Their lowest address is the link base, as xnuhost's loader takes it
    /// (`runtime/loader/src/macho.c`: `k->base`).
    pub segments: Vec<Segment>,
    pub members: Vec<Member>,
}

impl Image {
    /// The lowest address of the file's segments: xnuhost's "link base".
    pub fn link_base(&self) -> u64 {
        self.segments.iter().map(|s| s.vmaddr).min().unwrap_or(0)
    }

    /// The end of the file's last segment.
    pub fn end(&self) -> u64 {
        self.segments.iter().map(|s| s.vmaddr.saturating_add(s.vmsize)).max().unwrap_or(0)
    }

    pub fn holds(&self, addr: u64) -> bool {
        self.segments.iter().any(|s| s.holds(addr))
    }

    pub fn member(&self, name: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.name == name)
    }

    /// The kernel: the member `com.apple.kernel` of a collection, or the only member of a plain image.
    pub fn kernel(&self) -> Option<&Member> {
        if self.fileset {
            self.member(KERNEL_MEMBER)
        } else {
            self.members.first()
        }
    }
}

struct Commands {
    segments: Vec<Segment>,
    symbols: Vec<Symbol>,
    entries: Vec<(String, u64, u64)>,
    uuid: Option<[u8; 16]>,
    filetype: u32,
}

fn name_of(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// The load commands of the Mach-O header at `offset` of `data`. A fileset member's offsets (its segments' file
/// offsets, its symbol table) count from the start of the collection, as `data` does.
fn commands(data: &[u8], offset: u64, with_symbols: bool) -> Result<Commands> {
    let header = MachHeader64::<LE>::parse(data, offset).map_err(|e| anyhow!("Mach-O header at {offset:#x}: {e}"))?;
    if !header.is_little_endian() {
        bail!("not a little-endian 64-bit Mach-O header at {offset:#x}");
    }
    let mut out = Commands { segments: Vec::new(), symbols: Vec::new(), entries: Vec::new(), uuid: None, filetype: header.filetype(LE) };
    // The segment of every section, in the order n_sect counts them (from 1).
    let mut section_segment: Vec<u32> = Vec::new();
    let mut sections: Vec<u8> = Vec::new();
    let mut iter = header.load_commands(LE, data, offset).map_err(|e| anyhow!("load commands at {offset:#x}: {e}"))?;
    while let Some(command) = iter.next().map_err(|e| anyhow!("load command at {offset:#x}: {e}"))? {
        match command.variant().map_err(|e| anyhow!("load command at {offset:#x}: {e}"))? {
            LoadCommandVariant::Segment64(segment, _) => {
                section_segment.extend(std::iter::repeat_n(out.segments.len() as u32, segment.nsects(LE) as usize));
                out.segments.push(Segment { name: name_of(segment.name()), vmaddr: segment.vmaddr(LE), vmsize: segment.vmsize(LE) });
            }
            LoadCommandVariant::FilesetEntry(entry) => {
                let id = command.string(LE, entry.entry_id).map_err(|e| anyhow!("fileset entry: {e}"))?;
                out.entries.push((name_of(id), entry.vmaddr.get(LE), entry.fileoff.get(LE)));
            }
            LoadCommandVariant::Uuid(uuid) => out.uuid = Some(uuid.uuid),
            LoadCommandVariant::Symtab(symtab) if with_symbols => {
                let table = symtab.symbols::<MachHeader64<LE>, _>(LE, data).map_err(|e| anyhow!("symbol table at {offset:#x}: {e}"))?;
                let strings = table.strings();
                for nlist in table.iter() {
                    let nlist: &Nlist64<LE> = nlist;
                    if !nlist.is_definition() {
                        continue;
                    }
                    let Ok(name) = nlist.name(LE, strings) else { continue };
                    if name.is_empty() {
                        continue;
                    }
                    out.symbols.push(Symbol {
                        addr: nlist.n_value(LE),
                        name: String::from_utf8_lossy(name).into_owned(),
                        external: nlist.n_type() & macho::N_EXT != 0,
                        segment: None,
                    });
                    sections.push(nlist.n_sect());
                }
            }
            _ => {}
        }
    }
    for (symbol, &section) in out.symbols.iter_mut().zip(&sections) {
        symbol.segment = (section as usize).checked_sub(1).and_then(|i| section_segment.get(i)).copied();
    }
    Ok(out)
}

/// Parse a collection or a plain Mach-O. `with_symbols`: read every member's symbol table too.
pub fn parse(data: &[u8], with_symbols: bool) -> Result<Image> {
    let top = commands(data, 0, with_symbols).context("the file's Mach-O header")?;
    if top.filetype != macho::MH_FILESET {
        let name = match top.filetype {
            macho::MH_EXECUTE => "kernel",
            macho::MH_KEXT_BUNDLE => "kext",
            macho::MH_DSYM => "dSYM",
            _ => "image",
        };
        let vmaddr = top.segments.iter().find(|s| s.name == "__TEXT").map(|s| s.vmaddr).unwrap_or(0);
        let member = Member { name: name.to_string(), vmaddr, segments: top.segments.clone(), symbols: top.symbols };
        return Ok(Image { fileset: false, uuid: top.uuid, segments: top.segments, members: vec![member] });
    }
    let mut members = Vec::with_capacity(top.entries.len());
    for (name, vmaddr, fileoff) in top.entries {
        let inner = commands(data, fileoff, with_symbols).with_context(|| format!("fileset member {name}"))?;
        members.push(Member { name, vmaddr, segments: inner.segments, symbols: inner.symbols });
    }
    Ok(Image { fileset: true, uuid: top.uuid, segments: top.segments, members })
}

/// One segment of one member, for the search by address.
#[derive(Clone, Copy, Debug)]
struct Range {
    start: u64,
    end: u64,
    member: u32,
    segment: u32,
}

/// Which member and segment hold an address. Segments of no size are left out; where two members list the same
/// range (every kext of a collection lists the shared `__LINKEDIT`), the first member has it.
pub struct Locator {
    ranges: Vec<Range>,
}

impl Locator {
    pub fn new(members: &[(String, u64, Vec<Segment>)]) -> Locator {
        let mut ranges = Vec::new();
        for (m, (_, _, segments)) in members.iter().enumerate() {
            for (s, segment) in segments.iter().enumerate() {
                if segment.vmsize != 0 {
                    ranges.push(Range { start: segment.vmaddr, end: segment.vmaddr.saturating_add(segment.vmsize), member: m as u32, segment: s as u32 });
                }
            }
        }
        // By start; of ranges with one start the smallest first, then the earliest member.
        ranges.sort_by_key(|r| (r.start, r.end, r.member, r.segment));
        Locator { ranges }
    }

    /// (member, segment) of the smallest segment that holds `addr`.
    pub fn find(&self, addr: u64) -> Option<(usize, usize)> {
        let upto = self.ranges.partition_point(|r| r.start <= addr);
        let mut best: Option<&Range> = None;
        for r in &self.ranges[..upto] {
            if r.end > addr && best.is_none_or(|b| (r.end - r.start, r.member) < (b.end - b.start, b.member)) {
                best = Some(r);
            }
        }
        best.map(|r| (r.member as usize, r.segment as usize))
    }
}

/// A segment's move by the prelink: an address `a` of the unprelinked kernel's segment is `a + delta` in the
/// collection. Every segment has its own delta (the prelink places each on its own).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Moved {
    pub name: String,
    /// The segment in the collection's kernel member.
    pub kc_vmaddr: u64,
    /// The segment of the same name in the unprelinked kernel.
    pub kernel_vmaddr: u64,
    pub vmsize: u64,
    /// The sizes in the two files, when they differ (then the files do not belong together).
    pub kernel_vmsize: u64,
}

impl Moved {
    pub fn to_kernel(&self, kc_addr: u64) -> u64 {
        kc_addr.wrapping_sub(self.kc_vmaddr).wrapping_add(self.kernel_vmaddr)
    }

    pub fn to_kc(&self, kernel_addr: u64) -> u64 {
        kernel_addr.wrapping_sub(self.kernel_vmaddr).wrapping_add(self.kc_vmaddr)
    }

    pub fn holds_kc(&self, kc_addr: u64) -> bool {
        kc_addr >= self.kc_vmaddr && kc_addr - self.kc_vmaddr < self.vmsize
    }

    pub fn holds_kernel(&self, kernel_addr: u64) -> bool {
        kernel_addr >= self.kernel_vmaddr && kernel_addr - self.kernel_vmaddr < self.vmsize
    }
}

/// The kernel member's segments paired by name with the unprelinked kernel's (a segment of no size too: a
/// symbol can sit on it). Segments the other file does not have are left out.
pub fn moves(kc_kernel: &Member, kernel: &Member) -> Vec<Moved> {
    let mut out = Vec::new();
    for segment in &kc_kernel.segments {
        if let Some(other) = kernel.segments.iter().find(|o| o.name == segment.name) {
            out.push(Moved {
                name: segment.name.clone(),
                kc_vmaddr: segment.vmaddr,
                kernel_vmaddr: other.vmaddr,
                vmsize: segment.vmsize.min(other.vmsize),
                kernel_vmsize: other.vmsize,
            });
        }
    }
    out
}

/// The physical-aperture rule (docs/booter.md 2.2, `runtime/xnuhost/src/mem.c`, `runtime/loader/src/boot.c`):
/// xnuhost copies every segment of the collection to `RAM + (vmaddr - link base)` and the kernel runs there with
/// physical address = virtual address. So a running kernel's address is the collection's address plus
/// `slide = RAM - link base` (modulo 2^64; the log's `memory:` line prints both), and the collection's address
/// is the running address minus the slide.
pub fn unslide(runtime: u64, slide: u64) -> u64 {
    runtime.wrapping_sub(slide)
}

/// The slide of a boot: where the RAM window is, minus the collection's link base (modulo 2^64).
pub fn slide_of(ram_base: u64, link_base: u64) -> u64 {
    ram_base.wrapping_sub(link_base)
}
