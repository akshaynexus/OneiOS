//! `oneios-tool prelink classes` (was kernel/prelink/classes.py, retired 2026-11-13): the layout of original
//! (Apple) classes in the kernelcache, for board drivers that subclass them.
//!
//! usage: prelink classes --xnuprelink DIR --kernelcache KC [--prelink-report kernel.kc.prelink.json] --out classes.json CLASS...
//!
//! A board driver that subclasses a class of a kept kext (Droid-VM's NativeAES under IOAESAccelerator, the display
//! under IOMobileFramebufferLegacy, the HID device under IOHIDDevice, NativeAdreno under the AGX accelerator) is
//! linked into the kernel and needs the class's addresses in the collection: its metaclass object, instance size,
//! parent, vtable and slots, constructor and destructors. Kext entries carry no symbols, so each is read from code:
//!
//!   metaclass, size, parent   the class's static initializer: x0 = the metaclass object, x1 = the class name
//!                             string, x2 = the parent's metaclass, w3 = the instance size, then the OSMetaClass
//!                             constructor
//!   getMetaClass              the function that returns the metaclass object (adrp/add x0; ret)
//!   vtable                    the table whose slot 7 points at getMetaClass (slot 7 of every OSObject vtable)
//!   slots                     its entries while they are pointers; slot 0 and 1 are the complete and deleting
//!                             destructors
//!   constructors              functions that store vtable + 16 into [x0]
//!
//! Addresses are original kernelcache VAs; with --prelink-report each also gets its address as linked in kernel.kc
//! (original + collection slide). These are addresses and names, no Apple code: the output may be committed.
//! A class is looked for in every kext entry; give CLASS as Name or Name@bundle.id.
//!
//! How a board archive reaches them: a native stub `bti c; b <target>` with an N_ABS symbol `<stub>.original` at
//! the linked address (XnuPrelink's emitter recomputes the branch after it places the kernel, README there). An
//! absolute address in a literal, as Droid-VM's SoftwareAES-24A446.S has, is wrong under xnuhost: the collection
//! is slid.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use indexmap::IndexMap;
use oneios_common::pyjson::{self, Value};

use crate::image::{hex, scan, u32_at, Fixups, Image, Ref, RefKind, Segment};

const RET: u32 = 0xd65f_03c0;
const BTI_C: u32 = 0xd503_245f;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Not read: the kernelcache reader is this crate's (image.rs, the port of XnuPrelink's kc_image.py)
    #[arg(long)]
    xnuprelink: String,
    #[arg(long)]
    kernelcache: String,
    #[arg(long = "prelink-report")]
    prelink_report: Option<String>,
    #[arg(long)]
    out: String,
    #[arg(required = true, value_name = "CLASS")]
    classes: Vec<String>,
}

/// The kernelcache as the reader needs it.
struct Cache<'a> {
    raw: &'a [u8],
    cache: Image<'a>,
    fixups: Fixups,
    entries: IndexMap<String, Image<'a>>,
    /// The collection's slide in kernel.kc, with --prelink-report.
    slide: Option<u64>,
}

impl Cache<'_> {
    fn word(&self, address: u64) -> Result<u32> {
        Ok(u32_at(self.raw, self.cache.offset_of(address, 4)?))
    }

    /// An address as linked in kernel.kc; null without the report.
    fn linked(&self, address: u64) -> Value {
        self.slide.map(|slide| hex(address.wrapping_add(slide))).into()
    }
}

/// A class's static initializer, found.
struct Found<'a> {
    bundle: &'a str,
    image: &'a Image<'a>,
    code: &'a Segment,
    refs: Vec<Ref>,
    meta: u64,
    parent: Option<u64>,
    size: Option<u32>,
    site: u64,
}

/// The addresses of the class name's string in a kext's __TEXT and __DATA_CONST: a whole string, NUL before it.
fn name_strings(raw: &[u8], image: &Image, name: &str) -> HashSet<u64> {
    let needle = [name.as_bytes(), b"\0"].concat();
    let mut strings = HashSet::new();
    for segment in image.segments.iter().filter(|s| (s.name == "__TEXT" || s.name == "__DATA_CONST") && s.filesize != 0) {
        let blob = &raw[segment.fileoff as usize..(segment.fileoff + segment.filesize) as usize];
        for at in memchr::memmem::find_iter(blob, &needle) {
            if at == 0 || blob[at - 1] == 0 {
                strings.insert(segment.va + at as u64);
            }
        }
    }
    strings
}

/// The sites of a piece of code that give a register an address: site -> (target, how).
type Sites = HashMap<u64, (u64, RefKind)>;

impl Cache<'_> {
    /// The address a register was given (adrp/add) at most `reach` bytes before.
    fn held(&self, sites: &Sites, register: u32, before: u64, reach: u64) -> Result<Option<u64>> {
        for back in (0..reach / 4).map(|i| before - 4 * i) {
            if let Some(&(target, RefKind::Add)) = sites.get(&back) {
                if self.word(back)? & 31 == register {
                    return Ok(Some(target));
                }
            }
        }
        Ok(None)
    }

    /// The metaclass object of the initializer that sets x1 to the class name at `here`.
    fn meta_class(&self, sites: &Sites, here: u64) -> Result<Option<u64>> {
        // the static initializer sets x0 just before; the MetaClass constructors take x0 from the caller
        if let Some(meta) = self.held(sites, 0, here - 8, 20)? {
            return Ok(Some(meta));
        }
        // the metaclass kept in a callee-saved register and moved to x0 after the name (an initializer that goes
        // on to use it as the next class's parent: IOAcceleratorES before IOGPU)
        let mut meta = None;
        for forward in (here + 4..here + 32).step_by(4) {
            let instruction = self.word(forward)?;
            if instruction & 0xffe0_ffff == 0xaa00_03e0 {
                // mov x0, xN
                meta = self.held(sites, instruction >> 16 & 31, here - 8, 20)?;
            }
            if instruction >> 26 == 0x25 {
                break;
            }
        }
        Ok(meta)
    }

    /// The parent's metaclass and the instance size, set between the name and the OSMetaClass constructor's call.
    fn parent_and_size(&self, sites: &Sites, here: u64) -> Result<(Option<u64>, Option<u32>)> {
        let (mut parent, mut size) = (None, None);
        for forward in (here + 4..here + 32).step_by(4) {
            let instruction = self.word(forward)?;
            if instruction & 0xffe0_ffff == 0xaa00_03e2 {
                // mov x2, xN: the parent's metaclass made earlier in this function
                parent = self.held(sites, instruction >> 16 & 31, forward - 4, 256)?;
            }
            if let (Some(&(target, how)), 2) = (sites.get(&forward), instruction & 31) {
                // through the kext's GOT when it is a load
                parent = if how == RefKind::Ldst { self.fixups.get(&target).map(|fix| fix.target) } else { Some(target) };
            }
            if instruction & 0xffe0_001f == 0x5280_0003 {
                // mov w3, #size
                size = Some(instruction >> 5 & 0xffff);
            }
            if instruction >> 26 == 0x25 {
                // bl: the OSMetaClass constructor
                break;
            }
        }
        Ok((parent, size))
    }

    /// The static initializer that registers `name`, in `bundle` or (when that is empty) in the first kext that has one.
    fn find(&self, name: &str, bundle: &str) -> Result<Option<Found<'_>>> {
        for (bundle_id, image) in &self.entries {
            if !bundle.is_empty() && bundle_id != bundle {
                continue;
            }
            let Some(code) = image.segments.iter().find(|s| s.name == "__TEXT_EXEC" && s.filesize != 0) else { continue };
            let strings = name_strings(self.raw, image, name);
            if strings.is_empty() {
                continue;
            }
            let refs = scan(&self.raw[code.fileoff as usize..(code.fileoff + code.filesize) as usize], code.va).refs;
            let sites: Sites = refs.iter().map(|r| (r.site, (r.target, r.kind))).collect();
            for r in &refs {
                if !strings.contains(&r.target) || r.kind != RefKind::Add || self.word(r.site)? & 31 != 1 {
                    continue;
                }
                let Some(meta) = self.meta_class(&sites, r.site)? else { continue };
                let (parent, size) = self.parent_and_size(&sites, r.site)?;
                let site = r.site;
                return Ok(Some(Found { bundle: bundle_id, image, code, refs, meta, parent, size, site }));
            }
        }
        Ok(None)
    }

    /// getMetaClass: the function that returns the metaclass object (adrp/add x0; ret), `bti c` first where it has one.
    fn getter(&self, found: &Found) -> Result<Option<u64>> {
        for r in &found.refs {
            if r.target == found.meta && r.kind == RefKind::Add && self.word(r.site)? & 31 == 0 && self.word(r.site + 4)? == RET {
                let getter = r.site - 4;
                return Ok(Some(if self.word(getter - 4)? == BTI_C { getter - 4 } else { getter }));
            }
        }
        Ok(None)
    }

    /// The tables whose slot 7 points at the getter, and that start where a vtable starts (two words that are no
    /// pointers, then the first slot).
    fn vtables(&self, getter: u64) -> Vec<u64> {
        let fixed = |at: u64| self.fixups.contains_key(&at);
        self.fixups
            .iter()
            .filter(|(_, fix)| fix.target == getter)
            .map(|(slot, _)| slot.wrapping_sub(16 + 7 * 8))
            .filter(|&base| !fixed(base) && !fixed(base + 8) && fixed(base + 16))
            .collect()
    }

    /// The vtable's entries while they are pointers.
    fn slots(&self, vtable: u64) -> Vec<Value> {
        let mut slots = Vec::new();
        while let Some(fix) = self.fixups.get(&(vtable + 16 + 8 * slots.len() as u64)) {
            let mut slot = Value::dict();
            slot.set("slot", slots.len());
            slot.set("original", hex(fix.target));
            slot.set("linked", self.linked(fix.target));
            slot.set("diversity", fix.auth.then(|| hex(fix.diversity as u64)));
            slots.push(slot);
        }
        slots
    }

    /// The functions that store vtable + 16 (`add xN, xN, #16` after the adrp/add of the vtable).
    fn stores_vtable(&self, found: &Found, vtable: u64) -> Result<Vec<u64>> {
        let (low, high) = (found.code.va, found.code.va + found.code.size);
        let mut starts: Vec<u64> = found.image.function_starts()?.into_iter().filter(|s| low <= *s && *s < high).collect();
        starts.sort_unstable();
        let mut constructors: Vec<u64> = Vec::new();
        for r in &found.refs {
            let register = self.word(r.site)? & 31;
            if r.target == vtable && r.kind == RefKind::Add && self.word(r.site + 4)? == 0x9100_4000 | register << 5 | register {
                let start = starts[..starts.partition_point(|s| *s <= r.site)].last().copied();
                if let Some(start) = start.filter(|s| *s != 0 && !constructors.contains(s)) {
                    constructors.push(start);
                }
            }
        }
        constructors.sort_unstable();
        Ok(constructors)
    }

    /// One class's record.
    fn record(&self, name: &str, bundle: &str) -> Result<Value> {
        let mut record = Value::dict();
        let Some(found) = self.find(name, bundle)? else {
            record.set("error", "no static initializer with this class name found");
            return Ok(record);
        };
        record.set("bundle", found.bundle);
        record.set("size", found.size.map(|s| hex(s as u64)));
        record.set("meta_class", hex(found.meta));
        record.set("meta_class_linked", self.linked(found.meta));
        record.set("parent_meta_class", found.parent.filter(|p| *p != 0).map(hex));
        record.set("static_initializer_site", hex(found.site));
        let getter = self.getter(&found)?.filter(|g| *g != 0);
        record.set("get_meta_class", getter.map(hex));
        let vtables = getter.map_or(Vec::new(), |g| self.vtables(g));
        if let [vtable] = vtables[..] {
            let slots = self.slots(vtable);
            let original = |i: usize| slots.get(i).and_then(|s| s.get("original")).cloned();
            record.set("vtable", hex(vtable));
            record.set("vtable_linked", self.linked(vtable));
            record.set("slots", slots.len());
            record.set("destructor_complete", original(0));
            record.set("destructor_deleting", original(1));
            record.set("slot_table", slots);
            let stores: Vec<Value> = self
                .stores_vtable(&found, vtable)?
                .into_iter()
                .map(|c| {
                    let mut entry = Value::dict();
                    entry.set("original", hex(c));
                    entry.set("linked", self.linked(c));
                    entry
                })
                .collect();
            record.set("stores_vtable", stores);
        } else {
            record.set("vtable", Value::Null);
            record.set("vtable_candidates", vtables.into_iter().map(hex).collect::<Vec<String>>());
        }
        Ok(record)
    }
}

/// The record of `classes` (each Name or Name@bundle.id) of the kernelcache `raw`: what classes.json holds.
pub fn record(raw: &[u8], prelink_report: Option<&str>, classes: &[String]) -> Result<Value> {
    let cache = Image::new(raw, 0)?;
    let slide = match prelink_report {
        Some(path) => {
            let report = pyjson::load(path)?;
            Some(crate::image::unhex(report.get("collection_slide").and_then(Value::as_str).with_context(|| format!("{path}: no collection_slide"))?)?)
        }
        None => None,
    };
    let entries = cache.entries.iter().map(|(name, (_, at))| Ok((name.clone(), Image::new(raw, *at as usize)?))).collect::<Result<_>>()?;
    let reader = Cache { raw, fixups: cache.fixups()?, cache, entries, slide };
    let mut result = Value::dict();
    for item in classes {
        let (name, bundle) = item.split_once('@').unwrap_or((item, ""));
        result.set(name, reader.record(name, bundle)?);
    }
    let mut output = Value::dict();
    output.set("kernelcache_sha256", oneios_common::sha256_hex(raw));
    let linked = slide.map_or(String::new(), |s| format!("; \"linked\" = as placed in kernel.kc (original + collection slide {})", hex(s)));
    output.set("addresses", format!("original kernelcache VAs{linked}"));
    output.set("classes", result);
    Ok(output)
}

/// A field as Python's `str()` prints it.
fn shown(record: &Value, key: &str) -> String {
    match record.get(key) {
        None | Some(Value::Null) => "None".to_string(),
        Some(Value::Str(s)) => s.clone(),
        Some(Value::Int(n)) => n.to_string(),
        Some(other) => pyjson::dumps(other, None, false),
    }
}

pub fn run(args: Args) -> Result<()> {
    let raw = oneios_common::Map::open(&args.kernelcache)?;
    let output = record(&raw, args.prelink_report.as_deref(), &args.classes)?;
    std::fs::write(&args.out, pyjson::dumps(&output, Some(1), false) + "\n").with_context(|| args.out.clone())?;
    for (name, record) in output.get("classes").and_then(Value::as_dict).unwrap_or(&[]) {
        if record.get("error").is_some() {
            println!("classes: {name}: {}", shown(record, "error"));
        } else {
            let slots = record.get("slots").map_or("0".to_string(), |_| shown(record, "slots"));
            let stores = record.get("stores_vtable").and_then(Value::as_list).map_or(0, <[Value]>::len);
            println!(
                "classes: {name} in {}: size {}, metaclass {}, vtable {}, {slots} slots, {stores} function(s) store the vtable",
                shown(record, "bundle"),
                shown(record, "size"),
                shown(record, "meta_class"),
                shown(record, "vtable")
            );
        }
    }
    Ok(())
}
