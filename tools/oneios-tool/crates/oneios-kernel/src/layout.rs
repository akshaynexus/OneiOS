//! `oneios-tool kernel layout hid|net` (was kernel/patches/oneios-hosted/hid/layout.py and net/layout.py), and
//! what the two share with port_legacy_display: the kernelcache as the three tools read it.
//!
//! The record of original (Apple) classes in the iPhone18,2 24A446 kernelcache, for the bindings of
//! build_oneios_hid and build_oneios_net:
//!
//!   oneios-tool kernel layout hid|net [--kernelcache KC] [--prelink-report JSON] [--out FILE] [--repo CHECKOUT]
//!
//!   default inputs   out/kernel-inputs/24A446/kernelcache.macho, out/kernel/24A446/kernel.kc.prelink.json
//!   default output   IOHIDDevice-24A446-v54.json (hid) or IONetworking-24A446-v54.json (net) in the patch folder
//!                    (addresses, sizes and discriminators only: no Apple code, so it is committed; the build reads
//!                    it and never the kernelcache)
//!
//! What is read, and from where:
//!
//!   class, size, parent, vtable, slots   kernel/prelink/classes.py, now oneios-prelink's classes.rs (the scripts
//!                                        ran it): the class's static initializer (metaclass object, name string,
//!                                        parent metaclass, `mov w3, #size`), the getMetaClass function, the vtable
//!                                        whose slot 7 points at it, each slot's authenticated fixup (target,
//!                                        pointer-authentication discriminator)
//!   family                               per slot: the target lies in the family kext's __TEXT_EXEC, so the slot is
//!                                        the class's own function (an override of IOService's, or one of its own
//!                                        virtuals); otherwise it is the kernel's (inherited, or __cxa_pure_virtual)
//!   constructor                          hid: the one function that stores vtable + 16 into [x0]; net: the first
//!                                        (classes.py gives its start). Checked to be
//!                                        `bti c; pacibsp; stp; mov; bl <parent constructor>`
//!   base_destructor                      hid: the function between the constructor and getMetaClass, checked to be
//!                                        `bti c; b <parent destructor>` (the class has no member with a destructor);
//!                                        net: the first `bti c; b <the family's code>` after the constructors
//!   abstract (hid)                       slots 0 and 1 (the complete and deleting destructors) are `bti c; brk #1`:
//!                                        the class is abstract, nothing ever destroys a plain IOHIDDevice
//!
//! The kernelcache is read with oneios-prelink's reader (the port of XnuPrelink's kc_image.py the scripts imported).

use crate::gen;
use anyhow::{anyhow, Result};
use oneios_common::pyjson::Value;
use oneios_prelink::image::Image;
use std::path::{Path, PathBuf};

pub const BTI_C: u32 = 0xd503245f;
pub const PACIBSP: u32 = 0xd503237f;
pub const BRK_1: u32 = 0xd4200020;

#[derive(clap::Args)]
pub struct Args {
    /// The decoded kernelcache (default: out/kernel-inputs/24A446/kernelcache.macho)
    #[arg(long)]
    pub kernelcache: Option<PathBuf>,
    /// kernel.kc's prelink report (default: out/kernel/24A446/kernel.kc.prelink.json)
    #[arg(long = "prelink-report")]
    pub prelink_report: Option<PathBuf>,
    /// The XnuPrelink checkout classes.py reads the kernelcache with (default: external/Droid-VM-UniXNU/kits/XnuPrelink)
    #[arg(long)]
    pub xnuprelink: Option<PathBuf>,
    /// The record to write (default: the committed one in the patch folder)
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// This repository's checkout (default: the current directory)
    #[arg(long)]
    pub repo: Option<PathBuf>,
}

/// `--repo`, or the current directory: where the scripts found the repository from their own place.
pub fn repo(given: &Option<PathBuf>) -> PathBuf {
    gen::resolve(given.as_deref().unwrap_or(Path::new(".")))
}

/// The kernelcache and the code of one of its kexts.
pub struct Cache<'a> {
    pub raw: &'a [u8],
    pub image: Image<'a>,
}

impl<'a> Cache<'a> {
    pub fn new(raw: &'a [u8]) -> Result<Cache<'a>> {
        Ok(Cache { raw, image: Image::new(raw, 0)? })
    }

    /// The bounds of a kext's __TEXT_EXEC: the first such segment with file contents (`on_disk`), or the first.
    pub fn text(&self, bundle: &str, on_disk: bool) -> Result<Option<(u64, u64)>> {
        if !self.image.entries.contains_key(bundle) {
            return Ok(None);
        }
        let kext = self.image.entry(bundle)?;
        let segment = kext.segments.iter().find(|s| s.name == "__TEXT_EXEC" && (!on_disk || s.filesize != 0))
            .ok_or_else(|| anyhow!("{bundle} has no __TEXT_EXEC"))?;
        Ok(Some((segment.va, segment.va + if on_disk { segment.filesize } else { segment.size })))
    }

    pub fn read(&self, address: u64, size: u64) -> Result<&'a [u8]> {
        let at = self.image.offset(address, size).ok_or_else(|| anyhow!("{address:#x} is not mapped in the kernelcache"))?;
        Ok(&self.raw[at..at + size as usize])
    }

    pub fn word(&self, address: u64) -> Result<u32> {
        Ok(u32::from_le_bytes(self.read(address, 4)?.try_into().expect("4 bytes")))
    }

    /// The target of the b (or bl) at address, or None.
    pub fn branch(&self, address: u64, link: bool) -> Result<Option<u64>> {
        let instruction = self.word(address)?;
        if instruction >> 26 != if link { 0x25 } else { 0x05 } {
            return Ok(None);
        }
        let offset = (instruction & 0x3ffffff) as i64;
        let offset = if offset & 1 << 25 != 0 { offset - (1 << 26) } else { offset };
        Ok(Some((address as i64 + 4 * offset) as u64))
    }
}

/// The record of `classes` (Name@bundle) as kernel/prelink/classes.py wrote it (oneios-prelink's classes.rs; the
/// scripts ran classes.py as a program and read the temporary file it wrote).
pub fn class_record(root: &Path, args: &Args, kernelcache: &Path, _out: &Path, classes: &[String]) -> Result<Value> {
    let report = args.prelink_report.clone().unwrap_or_else(|| root.join("out/kernel/24A446/kernel.kc.prelink.json"));
    let raw = oneios_common::Map::open(kernelcache)?;
    oneios_prelink::classes::record(&raw, Some(&gen::s(&report)), classes)
}

/// One slot of classes.py's slot table as the layout records write it, with whether its target is the family's.
pub fn slot_entry(entry: &Value, low: u64, high: u64) -> Result<Value> {
    let target = gen::key_hex(entry, "original")?;
    let mut slot = Value::dict();
    for name in ["slot", "original", "linked", "diversity"] {
        slot.set(name, gen::key(entry, name)?.clone());
    }
    slot.set("family", low <= target && target < high);
    Ok(slot)
}

/// `{'original': hex(at), 'linked': hex(at + slide), <what>: hex(target)}`
pub fn placed(at: u64, slide: i128, what: &str, target: u64) -> Value {
    let mut value = Value::dict();
    value.set("original", format!("{at:#x}"));
    value.set("linked", format!("{:#x}", (at as i128 + slide) as u64));
    value.set(what, format!("{target:#x}"));
    value
}

pub fn copy(to: &mut Value, from: &Value, names: &[&str]) -> Result<()> {
    for name in names {
        to.set(*name, gen::key(from, name)?.clone());
    }
    Ok(())
}

pub fn hex_list(low: u64, high: u64) -> Value {
    let mut list = Value::list();
    list.push(format!("{low:#x}"));
    list.push(format!("{high:#x}"));
    list
}
