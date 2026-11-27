//! `apply`: the tweaks over an extracted root and its Data tree, and the files they leave in OUT
//! (roottweak/cli.py `_apply`): tweaks.manifest.json, boot-contract.json, owner-trust.json, report.json,
//! xnuhost.conf, report.txt; and `oneios/roottweak-report.txt` in the Data tree.

use crate::config;
use crate::tree::{Manifest, Tree, ROOT};
use crate::tweaks::{self, Context, Options, ONEIOS};
use crate::{write_file, Refused, SCHEMA};
use anyhow::{bail, Result};
use oneios_common::job;
use oneios_common::json::{self, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// What `apply` is given (`roottweak apply`'s command line).
#[derive(Clone, Debug, Default)]
pub struct Apply {
    /// the extracted OS volume (/)
    pub root: PathBuf,
    /// or the OS volume's image (raw APFS, `os.img`), read in place with our reader: sealed only
    pub root_image: Option<PathBuf>,
    /// the volume of `root_image`: a number or a name (default: the first)
    pub root_volume: Option<String>,
    /// the tree that becomes the Data volume (/private/var); with `rw` default ROOT/private/var
    pub data: Option<PathBuf>,
    /// where the manifest, the boot contract and the report go
    pub out: PathBuf,
    /// `rw`: a root we write; else `sealed` (nothing is written to the OS volume)
    pub rw: bool,
    /// another userland/config folder; None: the one built into the tool
    pub config: Option<PathBuf>,
    /// the UI scale, 2 or 3 (None: the profile's)
    pub scale: Option<i64>,
    pub options: Options,
}

/// What `apply` did.
pub struct Applied {
    pub build: String,
    /// report.txt's text (also printed)
    pub report: String,
    /// items that are off, not found or warnings
    pub off: usize,
}

pub fn apply(a: &Apply) -> Result<Applied> {
    job::step("tweaks");
    let layout = if a.rw { "rw" } else { "sealed" };
    let mut m = Manifest::default();
    let root = match &a.root_image {
        Some(_) if a.rw => bail!(Refused("--root-image is read only: --layout rw needs --root DIR".into())),
        Some(image) => Tree::from_image(image, "root", a.root_volume.as_deref())?,
        None => Tree::new(&a.root, "root", !a.rw)?,
    };
    let data = match &a.data {
        Some(d) => Tree::new(d, "data", false)?,
        // one volume: the Data tree is the root's /private/var
        None if a.rw => Tree::new(&root.host("private/var")?, "data", false)?,
        None => bail!(Refused("--layout sealed needs --data DIR, the tree that becomes the Data volume (/private/var)".into())),
    };
    if !root.is_image() && root.top.starts_with(&data.top) {
        bail!(Refused("--data must not contain --root".into()));
    }
    std::fs::create_dir_all(&a.out).map_err(|e| anyhow::anyhow!("{}: {e}", a.out.display()))?;
    // A run over trees an earlier run changed makes less itself (a directory is there already), but the image
    // build needs every owner: what the earlier manifest says of paths that still exist is carried over.
    let earlier = a.out.join("tweaks.manifest.json");
    if crate::tree::is_file(&earlier) {
        let old = json::load(&earlier)?;
        let num_is = |v: Option<&Value>, n: i128| matches!(v, Some(Value::Int(i)) if *i == n) || matches!(v, Some(Value::Bool(b)) if *b as i128 == n) || matches!(v, Some(Value::Float(f)) if *f == n as f64);
        if num_is(old.get("schema"), SCHEMA as i128) && old.get("layout").and_then(Value::as_str) == Some(layout) {
            for e in old.get("entries").and_then(Value::as_list).unwrap_or(&[]) {
                let (Some(volume), Some(kind), Some(path)) = (e.get("volume").and_then(Value::as_str), e.get("type").and_then(Value::as_str), e.get("path").and_then(Value::as_str)) else { continue };
                let tree = match volume {
                    "root" => &root,
                    "data" => &data,
                    _ => continue,
                };
                if ["file", "dir", "link"].contains(&kind) && tree.lexists(path)? {
                    m.entries.insert((volume.to_string(), path.to_string()), e.clone());
                }
            }
        }
    }
    let device = config::device(a.config.as_deref(), a.scale)?;
    let daemons = config::daemons(a.config.as_deref())?;
    let prefs = config::prefs(a.config.as_deref())?;
    let mut cx = Context::new(root, data, a.rw, device, daemons, prefs, a.options.clone(), m);
    for (name, tweak) in tweaks::ALL {
        job::check()?;
        cx.set_tweak(name);
        tweak(&mut cx)?;
    }

    let lines: Vec<String> = cx
        .results
        .iter()
        .map(|r| {
            let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            format!("{:<9} {}: {}", s("state"), s("item"), s("note"))
        })
        .collect();
    let report = std::iter::once(format!("roottweak schema {SCHEMA}, layout {layout}, build {}", cx.build)).chain(lines).collect::<Vec<_>>().join("\n") + "\n";
    cx.data.tweak = "report".into();
    cx.data.write(&mut cx.m, &format!("{ONEIOS}/roottweak-report.txt"), report.as_bytes(), ROOT, 0o644, None)?;

    job::step("place");
    let mut contract = Value::dict();
    contract.set("schema", SCHEMA);
    contract.set("layout", layout);
    contract.set("build", cx.build.as_str());
    contract.set("data_mount", "/private/var");
    if let Value::Dict(items) = &cx.contract {
        for (k, v) in items {
            contract.set(k.as_str(), v.clone());
        }
    }
    let trust = trust_value(&cx.trusted);
    let mut volumes = Value::dict();
    volumes.set("root", "the OS volume (/)");
    volumes.set("data", "the Data volume (/private/var)");
    let mut manifest = Value::dict();
    manifest.set("schema", SCHEMA);
    manifest.set("layout", layout);
    manifest.set("volumes", volumes);
    manifest.set("entries", Value::List(cx.m.sorted()));
    let mut report_json = Value::dict();
    report_json.set("schema", SCHEMA);
    report_json.set("layout", layout);
    report_json.set("build", cx.build.as_str());
    report_json.set("results", Value::List(cx.results.clone()));
    for (name, value) in [("tweaks.manifest.json", &manifest), ("boot-contract.json", &contract), ("owner-trust.json", &trust), ("report.json", &report_json)] {
        write_file(&a.out.join(name), (json::dumps(value, Some(2), true) + "\n").as_bytes())?;
    }
    // xnuhost's part of the display: it reads this file from the image set's overlay/userland (docs/booter.md).
    let scale = oneios_common::pyrepr::str_value(cx.device.get("display").and_then(|d| d.get("scale")).unwrap_or(&Value::Null));
    let mut conf = format!("# written by roottweak: the UI scale this image was made for (2 or 3)\nscale={scale}\n");
    // The S Pen as an Apple Pencil (docs/hardware.md 7f): xnuhost reads the digitizer and never grabs it, and finds
    // nothing on a phone without one. The S24 Ultra's image carried this by hand since 2026-11-16 and a rebuilt image
    // lost it (the owner's S Pen stopped working, 2026-11-25); the generator writes it, so a rebuild keeps it. Not for
    // the A56's 16.5 image, whose port never claimed the pen.
    if cx.build != "20F66" {
        conf.push_str("pen=auto\n");
    }
    if cx.build == "20F66" {
        // iOS 16.5 (port-a56, docs/a56.md): the 16.5 userland on the 24A446 collection (cli.py has the reasons).
        // oneios-mac-exec-skip (kernel 0073): 24A446's Sandbox refuses 20F66's mediaserverd and TVRemoteConnectionService at
        // exec, and without mediaserverd SpringBoard waits on the boot logo for com.apple.coremedia.systemcontroller.xpc (the
        // other A56 fork's lock-screen run of 2026-11-18 passed it on its switch's command line; here the image carries it,
        // 2026-11-23, so the OneOS switch passes nothing of the A56's own).
        // 2026-11-24 (port/a56-full): kernel 0103-0105's switches from the other A56 fork (touch, configd's IONetworkStack
        // entitlement, LaunchServices' keep) and its idle jobs off (0053); xnuhost adds the same words to an older 20F66
        // image's boot-args (runtime/xnuhost/src/config.rs IOS16_BOOT_ARGS: keep the two lists the same).
        conf.push_str(
            "boot-args=debug=0x14e serial=3 -v kt_fixed_zones=120 -no_slto_panic ml-timeout-lock-panic-scale=40 tlto_us=0 sheng-smp=1 armcpu-self-ipi=1 armcpu-spi-ipi=1 use_contiguous_hint=0 interrupt-masked-debug-mode=0 sched_preemption_disable_debug_mode=0 wdt=-1 native-source-storage=1 vgpu-3d=1 oneios-gpu-metal=0 ool_port_array_enforced=0 oneios-legacy-bootstrap-replies=1 oneios-legacy-registered-ports=1 oneios-mac-exec-skip=mediaserverd,TVRemoteConnectionService oneios-hid16=1 oneios-grant=configd:com.apple.networking.ionetworkstack.user-client oneios-ls-keep=3 oneios-disable-jobs=com.apple.accessibility.axassetsd,com.apple.aonsensed,com.apple.prngseedd,com.apple.suggestd,com.apple.spotlightknowledged,com.apple.corespotlightservice,com.apple.siriknowledged,com.apple.tipsd,com.apple.gamed,com.apple.followupd,com.apple.tailspind\n",
        );
        let keybagd = "usr/libexec/keybagd";
        // Scope the software keystore path to keybagd: the 20F66 binary only.
        if cx.root.isfile(keybagd)? && format!("{:x}", Sha256::digest(cx.root.read(keybagd)?)) == "3fab018f83b3c4a89d1e0e726caf16a4dcf27adeb662dbe7b09a500abf841b64" {
            conf.push_str("text-fixup=keybagd,cf5c,600000b4,1f2003d5\n");
        }
        // QuartzCore (subcache .03): IOMobileFramebuffer's vsync notifications as with CA_FORCE_TIMER.
        conf.push_str("text-fixup=dyld_shared_cache_arm64.03,15e4684,e8279f1a,28008052\n");
    }
    write_file(&a.out.join("xnuhost.conf"), conf.as_bytes())?;
    write_file(&a.out.join("report.txt"), report.as_bytes())?;
    print!("{report}");
    let off = cx
        .results
        .iter()
        .filter(|r| {
            let state = r.get("state").and_then(Value::as_str).unwrap_or("");
            state == "off" || state == "not-found" || r.get("item").and_then(Value::as_str).unwrap_or("").starts_with("warning")
        })
        .count();
    if off > 0 {
        println!("\n{off} items are off, not found or warnings: read them above.");
    }
    Ok(Applied { build: cx.build, report, off })
}

/// owner-trust.json's value of the trusted files.
pub fn trust_value(files: &[Value]) -> Value {
    let mut cdhashes: Vec<String> = files.iter().filter_map(|f| f.get("cdhash").and_then(Value::as_str).map(str::to_string)).collect();
    cdhashes.sort();
    cdhashes.dedup();
    let mut trust = Value::dict();
    trust.set("schema", SCHEMA);
    trust.set(
        "what",
        "CDHashes of OneiOS's own Mach-O files in the guest; add them as one owner module of the trust cache bundle (UniXNU-droidvm ipados27/scripts/build-droidvm-trust.py --owner reads the \"cdhashes\" list)",
    );
    trust.set("cdhashes", Value::List(cdhashes.into_iter().map(Value::Str).collect()));
    trust.set("files", Value::List(files.to_vec()));
    trust
}
