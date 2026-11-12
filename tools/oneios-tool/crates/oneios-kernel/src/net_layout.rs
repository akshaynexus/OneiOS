//! `oneios-tool kernel layout net` (was kernel/patches/oneios-hosted/net/layout.py): the record of the original
//! IOEthernetController and IOEthernetInterface in the iPhone18,2 24A446 kernelcache, for the bindings of
//! build_oneios_net (the Wi-Fi controller and interface subclass them). What is read and from where: layout.rs.
// SPDX-License-Identifier: MIT

use crate::gen::{self, fail};
use crate::layout::{self, Args, Cache, BTI_C, PACIBSP};
use anyhow::Result;
use oneios_common::pyjson::Value;

const CLASSES: [&str; 2] = ["IOEthernetController", "IOEthernetInterface"];
const BUNDLE: &str = "com.apple.iokit.IONetworkingFamily";

pub fn main(args: Args) -> Result<()> {
    let root = layout::repo(&args.repo);
    let kernelcache = args.kernelcache.clone().unwrap_or_else(|| root.join("out/kernel-inputs/24A446/kernelcache.macho"));
    let out = args.out.clone().unwrap_or_else(|| root.join("kernel/patches/oneios-hosted/net/IONetworking-24A446-v54.json"));
    let record = layout::class_record(&root, &args, &kernelcache, &out, &CLASSES.map(|class| format!("{class}@{BUNDLE}")))?;
    let raw = gen::read_bytes(&kernelcache)?;
    let cache = Cache::new(&raw)?;
    let (low, high) = cache.text(BUNDLE, true)?.ok_or_else(|| anyhow::anyhow!("net layout: {BUNDLE} is not in this kernelcache"))?;

    let mut output = Value::dict();
    layout::copy(&mut output, &record, &["kernelcache_sha256", "addresses"])?;
    output.set("bundle", BUNDLE);
    output.set("family_text", layout::hex_list(low, high));
    let mut classes = Value::dict();
    for name in CLASSES {
        let cls = gen::key(gen::key(&record, "classes")?, name)?;
        let stores: Vec<u64> = gen::key_list(cls, "stores_vtable")?.iter().map(|s| gen::key_hex(s, "original")).collect::<Result<_>>()?;
        if cls.get("vtable").is_none_or(Value::is_null) || stores.is_empty() {
            fail!("net layout: {name}: no vtable or constructor found");
        }
        let slide = gen::key_hex(cls, "meta_class_linked")? as i128 - gen::key_hex(cls, "meta_class")? as i128;
        let constructor = stores[0];
        let getter = gen::key_hex(cls, "get_meta_class")?;
        let Some(parent_constructor) = cache.branch(constructor + 16, true)?.filter(|_| cache.word(constructor).is_ok_and(|w| w == BTI_C)
                                                                                   && cache.word(constructor + 4).is_ok_and(|w| w == PACIBSP)) else {
            fail!("net layout: {name}: the function at {constructor:#x} is not `bti c; pacibsp; stp; mov; bl <parent constructor>`");
        };
        // the first `bti c; b <the family's code>` after the last constructor that is before getMetaClass
        let last_store = stores.iter().copied().filter(|at| *at < getter).max()
            .ok_or_else(|| anyhow::anyhow!("net layout: {name}: no constructor before getMetaClass"))?;
        let mut found = None;
        let mut at = last_store;
        while at < getter {
            if cache.word(at)? == BTI_C {
                if let Some(target) = cache.branch(at + 4, false)?.filter(|target| low <= *target && *target < high) {
                    found = Some((at, target));
                    break;
                }
            }
            at += 4;
        }
        let Some((destructor, parent_destructor)) = found else {
            fail!("net layout: {name}: no `bti c; b <parent destructor>` between its constructors and getMetaClass");
        };
        let mut slots = Vec::new();
        for entry in gen::key_list(cls, "slot_table")? {
            slots.push(layout::slot_entry(entry, low, high)?);
        }
        let of_family = slots.iter().filter(|slot| slot.get("family") == Some(&Value::Bool(true))).count();
        let mut class = Value::dict();
        layout::copy(&mut class, cls, &["size", "meta_class", "meta_class_linked", "parent_meta_class", "get_meta_class", "vtable", "vtable_linked"])?;
        class.set("constructor", layout::placed(constructor, slide, "calls_parent_constructor", parent_constructor));
        class.set("base_destructor", layout::placed(destructor, slide, "branches_to_parent_destructor", parent_destructor));
        println!("net layout: {name} {} bytes, {} slots ({of_family} the family's), constructor {constructor:#x}, base destructor {destructor:#x}",
                 gen::key_str(cls, "size")?, slots.len());
        class.set("slots", Value::List(slots));
        classes.set(name, class);
    }
    output.set("classes", classes);
    gen::write_json(&out, &output, 1)
}
