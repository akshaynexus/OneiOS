//! `oneios-tool kernel layout hid` (was kernel/patches/oneios-hosted/hid/layout.py): the record of the original
//! IOHIDDevice in the iPhone18,2 24A446 kernelcache, for OriginalHidDeviceABI.h and the bindings of build_oneios_hid.
//! What is read and from where: layout.rs.
// SPDX-License-Identifier: MIT

use crate::gen::{self, fail};
use crate::layout::{self, Args, Cache, BRK_1, BTI_C, PACIBSP};
use anyhow::Result;
use oneios_common::pyjson::Value;

const CLASS: &str = "IOHIDDevice";
const BUNDLE: &str = "com.apple.iokit.IOHIDFamily";

pub fn main(args: Args) -> Result<()> {
    let root = layout::repo(&args.repo);
    let kernelcache = args.kernelcache.clone().unwrap_or_else(|| root.join("out/kernel-inputs/24A446/kernelcache.macho"));
    let out = args.out.clone().unwrap_or_else(|| root.join("kernel/patches/oneios-hosted/hid/IOHIDDevice-24A446-v54.json"));
    let record = layout::class_record(&root, &args, &kernelcache, &out, &[format!("{CLASS}@{BUNDLE}")])?;
    let cls = gen::key(gen::key(&record, "classes")?, CLASS)?;
    let stores = gen::key_list(cls, "stores_vtable")?;
    if cls.get("vtable").is_none_or(Value::is_null) || stores.len() != 1 {
        fail!("hid layout: {CLASS}: one vtable and one constructor expected");
    }
    let raw = gen::read_bytes(&kernelcache)?;
    let cache = Cache::new(&raw)?;
    let (low, high) = cache.text(BUNDLE, true)?.ok_or_else(|| anyhow::anyhow!("hid layout: {BUNDLE} is not in this kernelcache"))?;

    let slide = gen::key_hex(cls, "meta_class_linked")? as i128 - gen::key_hex(cls, "meta_class")? as i128;
    let constructor = gen::key_hex(&stores[0], "original")?;
    let getter = gen::key_hex(cls, "get_meta_class")?;
    let destructor = getter - 8;
    let parent_constructor = cache.branch(constructor + 16, true)?;
    let parent_destructor = cache.branch(destructor + 4, false)?;
    let Some(parent_constructor) = parent_constructor.filter(|_| cache.word(constructor).is_ok_and(|w| w == BTI_C)
                                                             && cache.word(constructor + 4).is_ok_and(|w| w == PACIBSP)) else {
        fail!("hid layout: the function at {constructor:#x} is not `bti c; pacibsp; stp; mov; bl <parent constructor>`");
    };
    let Some(parent_destructor) = parent_destructor.filter(|_| cache.word(destructor).is_ok_and(|w| w == BTI_C)
                                                           && constructor < destructor && destructor < getter) else {
        fail!("hid layout: no `bti c; b <parent destructor>` between the constructor and getMetaClass ({destructor:#x})");
    };
    let mut slots = Vec::new();
    for entry in gen::key_list(cls, "slot_table")? {
        slots.push(layout::slot_entry(entry, low, high)?);
    }
    let mut is_abstract = true;
    for slot in &slots[..2] {
        let at = gen::key_hex(slot, "original")?;
        is_abstract &= cache.word(at)? == BTI_C && cache.word(at + 4)? == BRK_1;
    }
    let family = |slot: &Value| slot.get("family") == Some(&Value::Bool(true));
    let kernel_targets: Vec<&str> = slots.iter().filter(|slot| !family(slot)).filter_map(|slot| slot.get("original").and_then(Value::as_str)).collect();
    // a kernel target that more than one slot has, and that is not an IOService method: __cxa_pure_virtual
    let mut repeated: Vec<&str> = kernel_targets.iter().copied().filter(|t| kernel_targets.iter().filter(|other| other == &t).count() > 1).collect();
    repeated.sort();
    repeated.dedup();

    let mut output = Value::dict();
    layout::copy(&mut output, &record, &["kernelcache_sha256", "addresses"])?;
    output.set("class", CLASS);
    layout::copy(&mut output, cls, &["bundle", "size", "meta_class", "meta_class_linked", "parent_meta_class", "static_initializer_site",
                                     "get_meta_class", "vtable", "vtable_linked"])?;
    output.set("family_text", layout::hex_list(low, high));
    output.set("constructor", layout::placed(constructor, slide, "calls_parent_constructor", parent_constructor));
    output.set("base_destructor", layout::placed(destructor, slide, "branches_to_parent_destructor", parent_destructor));
    output.set("abstract", is_abstract);
    let mut list = Value::list();
    repeated.into_iter().for_each(|target| list.push(target));
    output.set("kernel_targets_repeated", list);
    let own: Vec<i128> = slots.iter().filter(|slot| family(slot)).filter_map(|slot| slot.get("slot").and_then(Value::as_int)).collect();
    let count = slots.len();
    output.set("slots", Value::List(slots));
    gen::write_json(&out, &output, 1)?;
    println!("hid layout: {}: {CLASS} {} bytes, {count} slots, {} of them the family's ({} below 168), constructor {constructor:#x}, \
              base destructor {destructor:#x}, abstract {}", gen::name(&out), gen::key_str(cls, "size")?, own.len(),
             crate::class_record::pylist(own.iter().filter(|slot| **slot < 168)), if is_abstract { "True" } else { "False" });
    Ok(())
}
