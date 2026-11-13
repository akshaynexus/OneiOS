//! `oneios-tool kernel port-legacy-display` (was kernel/patches/oneios-hosted/display/port-legacy-display.py).
//!
//! Carry Droid-VM's reviewed layout of the original IOMobileFramebufferLegacy (iPad17,3, 24A446) over to the
//! iPhone18,2 kernelcache of the same build, and check every carried number against this kernelcache's bytes.
//!
//!   oneios-tool kernel port-legacy-display [--kernelcache out/kernel-inputs/24A446/kernelcache.macho]
//!       [--fixture external/UniXNU-droidvm/ipados27/scripts/legacy-display-24A446.json]
//!       [--classes kernel/prelink/classes-24A446-v54.json] [--out legacy-display-24A446-v54.json] [--repo CHECKOUT]
//!
//! Droid-VM's fixture (legacy-display-24A446.json) records, for the iPad kernelcache: the class's 379 vtable slots with
//! the pointer-authentication diversity of each, the names under which the owned subclass declares them (a name whose
//! discriminator is the slot's diversity), which slots the backend overrides, the fields of the display object and of
//! a swap request it reads, and the original functions it calls. kernel/prelink/classes.py already reads the same
//! class from the iPhone kernelcache (metaclass, size, vtable, slots, diversities, constructor). What is left is to
//! show that the family's code is the same code at another address, so that the reviewed meanings carry over:
//!
//!   slots      every slot whose target is inside IOMobileGraphicsFamily's text sits at one constant distance from the
//!              iPad's (the kext was laid out identically); every other slot is the kernel's and is taken from this
//!              kernelcache. Diversities must be equal slot by slot, except the pure-virtual slots: their table entry is
//!              __cxa_pure_virtual under some diversity, and what matters is the diversity original callers authenticate
//!              with (the fixture's, evidence there), which must appear as `movk xN, #d, lsl #48` in this family's text.
//!   bindings   the family's own functions move by that distance; IOSurface's by the one distance at which all eight of
//!              the fixture's byte checks hold in this kernelcache's IOSurface text (exactly one may exist).
//!   fields     unchanged (same code); the two instructions the fixture checks them by are read back here.
//!
//! The output records each byte check as its address, length and SHA-256 ("checks"), not the bytes.
//!
//! The output holds addresses, offsets and names only (no Apple code) and is the input of build_oneios_display.
//! Rerun when the kernelcache changes; it fails rather than guess. The kernelcache is read with oneios-prelink's
//! reader (the script took `--xnuprelink DIR` for XnuPrelink's kc_image.py; the option is still accepted).

use crate::gen::{self, fail};
use crate::layout::{self, Cache};
use anyhow::{anyhow, Result};
use oneios_common::pyjson::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::PathBuf;

const CLASS: &str = "IOMobileFramebufferLegacy";
const FAMILY: &str = "com.apple.iokit.IOMobileGraphicsFamily";
const SURFACE: &str = "com.apple.iokit.IOSurface";
const FAMILY_BINDINGS: [&str; 2] = ["_native_legacy_vsync_notify", "_native_legacy_set_refresh_properties"];
const SURFACE_BINDINGS: [&str; 8] = ["_native_legacy_surface_descriptor", "_native_legacy_surface_width", "_native_legacy_surface_height",
                                     "_native_legacy_surface_stride", "_native_legacy_surface_format", "_native_legacy_surface_planes",
                                     "_native_legacy_surface_create", "_native_legacy_surface_allocate"];
const CONSTRUCTOR: &str = "__ZN25IOMobileFramebufferLegacyC2EPK11OSMetaClass";
const DESTRUCTOR: &str = "__ZN25IOMobileFramebufferLegacyD2Ev";
const VTABLE: &str = "__ZTV25IOMobileFramebufferLegacy";
const BTI_C: [u8; 4] = [0x5f, 0x24, 0x03, 0xd5];

#[derive(clap::Args)]
pub struct Args {
    /// Accepted and not used (the script imported XnuPrelink's kernelcache reader from there)
    #[arg(long)]
    xnuprelink: Option<PathBuf>,
    #[arg(long)]
    kernelcache: Option<PathBuf>,
    /// Droid-VM's reviewed iPad fixture (default: external/UniXNU-droidvm/ipados27/scripts/legacy-display-24A446.json)
    #[arg(long)]
    fixture: Option<PathBuf>,
    /// classes.py's record of this kernelcache (default: kernel/prelink/classes-24A446-v54.json)
    #[arg(long)]
    classes: Option<PathBuf>,
    #[arg(long)]
    out: Option<PathBuf>,
    /// This repository's checkout (default: the current directory)
    #[arg(long)]
    repo: Option<PathBuf>,
}

/// `f'{value:#x}'` of a number that may be negative.
fn signed_hex(value: i128) -> String {
    if value >= 0 { format!("{value:#x}") } else { format!("-{:#x}", -value) }
}

fn unhex(text: &str) -> Result<Vec<u8>> {
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2).unwrap_or("?"), 16).map_err(|_| anyhow!("not hexadecimal bytes: {text}")))
        .collect()
}

/// `[(target, diversity)]` of the fixture's or the output's slot pairs.
fn pairs(list: &[Value]) -> Result<Vec<(u64, u64)>> {
    list.iter().map(|pair| {
        let pair = pair.as_list().filter(|p| p.len() == 2).ok_or_else(|| anyhow!("a slot is not [target, diversity]"))?;
        Ok((gen::hex(pair[0].as_str().unwrap_or(""))?, gen::hex(pair[1].as_str().unwrap_or(""))?))
    }).collect()
}

fn find(code: &[u8], pattern: &[u8], from: usize) -> Option<usize> {
    if from > code.len() || pattern.is_empty() {
        return None;
    }
    code[from..].windows(pattern.len()).position(|window| window == pattern).map(|at| at + from)
}

pub fn main(args: Args) -> Result<()> {
    let root = layout::repo(&args.repo);
    let kernelcache = args.kernelcache.unwrap_or_else(|| root.join("out/kernel-inputs/24A446/kernelcache.macho"));
    let fixture = args.fixture.unwrap_or_else(|| root.join("external/UniXNU-droidvm/ipados27/scripts/legacy-display-24A446.json"));
    let classes = args.classes.unwrap_or_else(|| root.join("kernel/prelink/classes-24A446-v54.json"));
    let out_path = args.out.unwrap_or_else(|| root.join("kernel/patches/oneios-hosted/display/legacy-display-24A446-v54.json"));
    let raw = gen::read_bytes(&kernelcache)?;
    let digest = oneios_common::hex(&Sha256::digest(&raw));
    let record = gen::read_json(&classes)?;
    if gen::key_str(&record, "kernelcache_sha256")? != digest {
        fail!("port-legacy-display: {} is for another kernelcache", classes.display());
    }
    let ipad = gen::read_json(&fixture)?;
    let cls = match gen::key(&record, "classes")?.get(CLASS) {
        Some(cls) if cls.as_dict().is_some_and(|d| !d.is_empty()) => cls,
        _ => fail!("port-legacy-display: {} has no {CLASS}", classes.display()),
    };
    let ipad_slots = gen::key_list(&ipad, "slots")?;
    if gen::key_str(cls, "bundle")? != FAMILY || gen::key(cls, "size")? != gen::key(&ipad, "instance_size")?
        || gen::key_int(cls, "slots")? != ipad_slots.len() as i128 {
        fail!("port-legacy-display: {CLASS} here is {} bytes with {} slots; the fixture describes {} and {}", gen::key_str(cls, "size")?,
              gen::key_int(cls, "slots")?, gen::key_str(&ipad, "instance_size")?, ipad_slots.len());
    }
    let cache = Cache::new(&raw)?;
    let text = |bundle: &str| -> Result<(u64, u64)> {
        match cache.text(bundle, false)? {
            Some(bounds) => Ok(bounds),
            None => fail!("port-legacy-display: {bundle} is not in this kernelcache"),
        }
    };
    let (family, surface) = (text(FAMILY)?, text(SURFACE)?);
    let inside = |address: i128, bounds: (u64, u64)| bounds.0 as i128 <= address && address < bounds.1 as i128;

    // ---- slots
    let mut table: Vec<&Value> = gen::key_list(cls, "slot_table")?.iter().collect();
    table.sort_by_key(|entry| entry.get("slot").and_then(Value::as_int).unwrap_or(0));
    let here: Vec<(u64, u64)> = table.iter().map(|e| Ok((gen::key_hex(e, "original")?, gen::key_hex(e, "diversity")?))).collect::<Result<_>>()?;
    let there = pairs(ipad_slots)?;
    let moves: BTreeSet<i128> = here.iter().zip(&there).filter(|(h, _)| inside(h.0 as i128, family)).map(|(h, t)| h.0 as i128 - t.0 as i128).collect();
    if moves.len() != 1 {
        let mut shown: Vec<String> = moves.iter().map(|m| format!("'{}'", signed_hex(*m))).collect();
        shown.sort();
        fail!("port-legacy-display: the family's slots are not at one distance from the iPad's: [{}]", shown.join(", "));
    }
    let moved = *moves.iter().next().expect("one move");
    let pure = |slots: &[(u64, u64)]| -> BTreeSet<usize> { slots.iter().enumerate().filter(|(_, s)| s.0 == slots[214].0).map(|(i, _)| i).collect() };
    let (pure_here, pure_there) = (pure(&here), pure(&there));
    if pure_here != pure_there {
        fail!("port-legacy-display: the pure-virtual slots differ: {}", crate::class_record::pylist(pure_here.symmetric_difference(&pure_there)));
    }
    let family_code = cache.read(family.0, family.1 - family.0)?;
    let words: std::collections::HashSet<u32> = family_code.chunks_exact(4).map(|w| u32::from_le_bytes(w.try_into().expect("4 bytes"))).collect();
    // movk xN, #diversity, lsl #48
    let authenticated = |diversity: u64| (0..31u32).any(|reg| words.contains(&(0xf2e00000 | (diversity as u32) << 5 | reg)));

    let mut slots = Value::list();
    let mut callers = Value::dict();
    for (i, (h, t)) in here.iter().zip(&there).enumerate() {
        let carried = t.0 as i128 + moved;
        if inside(carried, family) != inside(h.0 as i128, family) || (inside(h.0 as i128, family) && h.0 as i128 != carried) {
            fail!("port-legacy-display: slot {i}: {:#x} here, {:#x} on the iPad: not the same function of the family", h.0, t.0);
        }
        let mut diversity = h.1;
        if pure_here.contains(&i) {
            diversity = t.1; // the callers', as the fixture has it
            if h.1 != t.1 {
                if !authenticated(t.1) {
                    fail!("port-legacy-display: slot {i} is pure and no call in the family authenticates with the fixture's {:#x}", t.1);
                }
                let mut caller = Value::dict();
                caller.set("diversity", format!("{:#x}", t.1));
                caller.set("table_entry", format!("{:#x}", h.1));
                caller.set("evidence", format!("pure slot: the table entry is __cxa_pure_virtual; the family's text has a movk xN, #{:#x}, lsl #48 \
                                                (the call sites' discriminator, as the iPad fixture records it){}", t.1,
                                               if authenticated(h.1) { String::new() } else { format!(" and none with the table entry's {:#x}", h.1) }));
                callers.set(i.to_string(), caller);
            }
        } else if h.1 != t.1 {
            fail!("port-legacy-display: slot {i}: diversity {:#x} here, {:#x} on the iPad", h.1, t.1);
        }
        let mut pair = Value::list();
        pair.push(format!("{:#x}", h.0));
        pair.push(format!("{diversity:#x}"));
        slots.push(pair);
    }

    // ---- bindings
    let ipad_bindings = gen::key(&ipad, "bindings")?;
    let ipad_at = |name: &str| -> Result<i128> { Ok(gen::key_hex(ipad_bindings, name)? as i128) };
    let constructor: Vec<u64> = gen::key_list(cls, "stores_vtable")?.iter().map(|e| gen::key_hex(e, "original")).collect::<Result<_>>()?;
    if constructor.len() != 1 || constructor[0] as i128 != ipad_at(CONSTRUCTOR)? + moved {
        fail!("port-legacy-display: the constructor that stores the vtable is not the iPad's moved");
    }
    let mut bindings: Vec<(String, u64)> = vec![
        (CONSTRUCTOR.to_string(), constructor[0]),
        (DESTRUCTOR.to_string(), (ipad_at(DESTRUCTOR)? + moved) as u64),
        ("__ZN25IOMobileFramebufferLegacy10gMetaClassE".to_string(), gen::key_hex(cls, "meta_class")?),
        (VTABLE.to_string(), gen::key_hex(cls, "vtable")? - 16)];
    if gen::key_hex(&ipad, "vtable")? as i128 - ipad_at(VTABLE)? != 16 {
        fail!("port-legacy-display: the fixture's vtable symbol is not 16 bytes before slot 0");
    }
    for name in FAMILY_BINDINGS {
        let at = ipad_at(name)? + moved;
        if !inside(at, family) || cache.read(at as u64, 4)? != BTI_C {
            fail!("port-legacy-display: {name}: no function entry (bti c) at {:#x}", at as u64);
        }
        bindings.push((name.to_string(), at as u64));
    }
    // the fixture's byte checks, in its order (a dict: a later one of the same address takes the first's place)
    let mut checks: Vec<(u64, Vec<u8>)> = Vec::new();
    for check in gen::key_list(&ipad, "checks")? {
        let check = check.as_list().filter(|c| c.len() == 2).ok_or_else(|| anyhow!("a check is not [address, bytes]"))?;
        let (address, bytes) = (gen::hex(check[0].as_str().unwrap_or(""))?, unhex(check[1].as_str().unwrap_or(""))?);
        match checks.iter_mut().find(|(at, _)| *at == address) {
            Some(known) => known.1 = bytes,
            None => checks.push((address, bytes)),
        }
    }
    let surface_addresses: BTreeSet<u64> = SURFACE_BINDINGS.iter().map(|name| Ok(ipad_at(name)? as u64)).collect::<Result<_>>()?;
    let surface_checks: Vec<&(u64, Vec<u8>)> = checks.iter().filter(|(address, _)| surface_addresses.contains(address)).collect();
    if surface_checks.len() != SURFACE_BINDINGS.len() {
        fail!("port-legacy-display: the fixture does not check every IOSurface binding");
    }
    let (anchor, pattern) = (surface_checks[0].0, &surface_checks[0].1);
    let surface_code = cache.read(surface.0, surface.1 - surface.0)?;
    let mut surface_moves = Vec::new();
    let mut at = find(surface_code, pattern, 0);
    while let Some(found) = at {
        let candidate = surface.0 as i128 + found as i128 - anchor as i128;
        let fits = |check: &&(u64, Vec<u8>)| {
            let address = check.0 as i128 + candidate;
            inside(address, surface) && cache.read(address as u64, check.1.len() as u64).is_ok_and(|bytes| bytes == check.1)
        };
        if found % 4 == 0 && surface_checks.iter().all(fits) {
            surface_moves.push(candidate);
        }
        at = find(surface_code, pattern, found + 4);
    }
    if surface_moves.len() != 1 {
        fail!("port-legacy-display: {} placements of IOSurface's eight functions fit the fixture's checks (one must)", surface_moves.len());
    }
    for name in SURFACE_BINDINGS {
        bindings.push((name.to_string(), (ipad_at(name)? + surface_moves[0]) as u64));
    }
    let mut moved_checks = Value::list();
    for (address, expected) in &checks {
        let address = (*address as i128 + if surface_addresses.contains(address) { surface_moves[0] } else { moved }) as u64;
        let found = cache.read(address, expected.len() as u64)?;
        if found != expected {
            fail!("port-legacy-display: check at {address:#x}: {}, expected {}", oneios_common::hex(found), oneios_common::hex(expected));
        }
        // the place, the length and a hash: the output carries no instruction bytes
        let mut check = Value::list();
        check.push(format!("{address:#x}"));
        check.push(expected.len());
        check.push(oneios_common::hex(&Sha256::digest(expected)));
        moved_checks.push(check);
    }
    let destructor = bindings.iter().find(|(name, _)| name == DESTRUCTOR).expect("set above").1;
    if cache.read(destructor, 4)? != BTI_C {
        fail!("port-legacy-display: no function entry at the base destructor");
    }
    let slide = gen::key_hex(cls, "meta_class_linked")? as i128 - gen::key_hex(cls, "meta_class")? as i128;
    let family_slots = here.iter().filter(|h| inside(h.0 as i128, family)).count();

    let mut ported = Value::dict();
    ported.set("fixture", gen::name(&fixture));
    ported.set("kernelcache_sha256", gen::key(&ipad, "kernelcache_sha256")?.clone());
    ported.set("family_move", signed_hex(moved));
    ported.set("surface_move", signed_hex(surface_moves[0]));
    ported.set("family_slots_at_that_distance", family_slots);
    let mut addresses = Value::dict();
    for (name, address) in &bindings {
        addresses.set(name.clone(), format!("{address:#x}"));
    }
    let (slot_count, check_count, caller_count) = (here.len().min(there.len()), checks.len(), callers.as_dict().map_or(0, <[_]>::len));
    let mut out = Value::dict();
    out.set("about", "The original IOMobileFramebufferLegacy of the iPhone18,2 24A446 kernelcache for the owned display backend \
                      (NativeMobileDisplay): Droid-VM's reviewed iPad fixture carried over and checked by port-legacy-display.py. \
                      Addresses are original kernelcache VAs; build-oneios-display.py adds the collection slide.");
    out.set("kernelcache_sha256", digest);
    out.set("ported_from", ported);
    out.set("collection_slide", signed_hex(slide));
    out.set("family_text", layout::hex_list(family.0, family.1));
    layout::copy(&mut out, cls, &["vtable"])?;
    out.set("instance_size", gen::key(cls, "size")?.clone());
    out.set("slots", slots);
    layout::copy(&mut out, &ipad, &["methods", "leaves"])?;
    out.set("bindings", addresses);
    out.set("checks", moved_checks);
    layout::copy(&mut out, &ipad, &["display", "request", "names"])?;
    out.set("caller_diversity", callers);
    gen::write_json(&out_path, &out, 1)?;
    println!("port-legacy-display: {}: {slot_count} slots ({family_slots} of the family, moved {}), IOSurface moved {}, {check_count} byte checks hold, \
              {caller_count} pure slots on the callers' diversity", out_path.display(), signed_hex(moved), signed_hex(surface_moves[0]));
    Ok(())
}
