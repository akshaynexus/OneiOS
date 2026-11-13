//! A compiled arm64e object as the generators read it: its sections, its symbols, and the signed entries of a
//! vtable. build-oneios-hid.py had these readers (`sections`, `symbols`, `vtable`, after UniXNU-droidvm's
//! build-droidvm-hid-input.py), build-oneios-net.py imported them from it, and build-oneios-display.py had its
//! own copy (`macho`, the loop of `bind`). The parsing is the `object` crate's; what is read from it is the
//! scripts'.

use anyhow::{anyhow, bail, Result};
use object::macho::{MachHeader64, ARM64_RELOC_AUTHENTICATED_POINTER, CPU_TYPE_ARM64};
use object::read::macho::{MachHeader, Nlist, Section, Segment};
use object::LittleEndian as LE;
use std::collections::BTreeMap;

pub struct Sect {
    pub va: u64,
    pub size: u64,
    pub fileoff: u32,
    /// (r_address, the second word) of each relocation
    pub relocations: Vec<(u32, u32)>,
}

/// (name, value, type byte); type 1 is undefined external, `kind & 0xe == 0xe` a symbol defined in a section.
pub struct Sym {
    pub name: String,
    pub value: u64,
    pub kind: u8,
}

/// One authenticated pointer of a vtable: the symbol it names and the stored word (the discriminator in bits
/// 32-47, address diversity in bit 48, the key in bits 49-50, "authenticated" in bit 63).
#[derive(Clone)]
pub struct Entry {
    pub symbol: String,
    pub word: u64,
}

impl Entry {
    pub fn diversity(&self) -> u64 {
        self.word >> 32 & 0xffff
    }
    pub fn address(&self) -> bool {
        self.word >> 48 & 1 != 0
    }
    pub fn key(&self) -> u64 {
        self.word >> 49 & 3
    }
}

pub struct Object<'a> {
    data: &'a [u8],
    pub sections: Vec<Sect>,
    pub symbols: Vec<Sym>,
}

impl<'a> Object<'a> {
    /// `refused` is the script's message for a file that is not an arm64 Mach-O object.
    pub fn parse(data: &'a [u8], refused: &str) -> Result<Object<'a>> {
        let header = MachHeader64::<LE>::parse(data, 0).map_err(|_| anyhow!("{refused}"))?;
        if header.cputype(LE) != CPU_TYPE_ARM64 {
            bail!("{refused}");
        }
        let broken = |e: object::Error| anyhow!("{refused}: {e}");
        let mut sections = Vec::new();
        let mut symbols = Vec::new();
        let mut commands = header.load_commands(LE, data, 0).map_err(broken)?;
        while let Some(command) = commands.next().map_err(broken)? {
            if let Some((segment, section_data)) = command.segment_64().map_err(broken)? {
                for section in segment.sections(LE, section_data).map_err(broken)? {
                    let relocations = section.relocations(LE, data).map_err(broken)?
                        .iter().map(|r| (r.r_word0.get(LE), r.r_word1.get(LE))).collect();
                    sections.push(Sect { va: section.addr(LE), size: section.size(LE), fileoff: section.offset(LE), relocations });
                }
            } else if let Some(symtab) = command.symtab().map_err(broken)? {
                let table = symtab.symbols::<MachHeader64<LE>, _>(LE, data).map_err(broken)?;
                symbols.clear();
                for nlist in table.iter() {
                    let name = nlist.name(LE, table.strings()).map_err(broken)?;
                    symbols.push(Sym { name: String::from_utf8_lossy(name).into_owned(), value: nlist.n_value(LE), kind: nlist.n_type() });
                }
            }
        }
        Ok(Object { data, sections, symbols })
    }

    /// The authenticated pointers of the sections that hold `start`, by address: the relocations that are
    /// extern, 8 bytes long and ARM64_RELOC_AUTHENTICATED_POINTER, each with its symbol and the word stored there.
    pub fn signed_pointers(&self, start: u64) -> Result<BTreeMap<u64, Entry>> {
        let mut entries = BTreeMap::new();
        for section in &self.sections {
            if !(section.va <= start && start < section.va + section.size) {
                continue;
            }
            for &(offset, bits) in &section.relocations {
                if bits >> 27 & 1 != 0 && bits >> 25 & 3 == 3 && bits >> 28 == ARM64_RELOC_AUTHENTICATED_POINTER as u32 {
                    let at = section.fileoff as usize + offset as usize;
                    let word = self.data.get(at..at + 8).ok_or_else(|| anyhow!("a relocation points outside the object"))?;
                    let symbol = self.symbols.get((bits & 0xffffff) as usize).ok_or_else(|| anyhow!("a relocation names no symbol"))?;
                    entries.insert(section.va + offset as u64,
                                   Entry { symbol: symbol.name.clone(), word: u64::from_le_bytes(word.try_into().expect("8 bytes")) });
                }
            }
        }
        Ok(entries)
    }

    /// The entries of vtable `name` from its first slot on (16 bytes after the symbol), while they are
    /// authenticated pointers. None when the object does not define `name` in a section.
    pub fn vtable(&self, name: &str) -> Result<Option<Vec<Entry>>> {
        let Some(start) = self.symbols.iter().find(|sym| sym.name == name && sym.kind & 0xe == 0xe).map(|sym| sym.value) else {
            return Ok(None);
        };
        let entries = self.signed_pointers(start)?;
        let mut table = Vec::new();
        let mut at = start + 16;
        while let Some(entry) = entries.get(&at) {
            table.push(entry.clone());
            at += 8;
        }
        Ok(Some(table))
    }

    /// The vtables an object defines for its own classes: `__ZTV...` defined in a section, the metaclasses'
    /// aside.
    pub fn own_vtables(&self) -> Vec<String> {
        self.symbols.iter().filter(|sym| sym.name.starts_with("__ZTV") && sym.kind & 0xe == 0xe && !sym.name.contains("9MetaClassE"))
            .map(|sym| sym.name.clone()).collect()
    }

    /// The undefined external names (`kind == 1`).
    pub fn undefined(&self) -> impl Iterator<Item = &str> {
        self.symbols.iter().filter(|sym| sym.kind == 1).map(|sym| sym.name.as_str())
    }
}
