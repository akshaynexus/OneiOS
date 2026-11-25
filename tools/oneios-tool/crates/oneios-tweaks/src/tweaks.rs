//! The tweaks (roottweak/tweaks.py). Each takes the context, changes the trees through it and reports one line per
//! item. Two layouts (docs/userland.md, "Layouts"): `sealed` (the OS volume is Apple's image, nothing is written
//! to it) and `rw` (a root we write ourselves). States: applied, kept, not-found, off, contract.
//!
//! The texts are tweaks.py's, word for word: the report is a file of the Data seed, whose SHA-256 is pinned.

use crate::config::{Daemons, Pref};
use crate::launchd;
use crate::macho;
use crate::plist::{py_eq, P};
use crate::tree::{self, Change, Manifest, Owner, Tree, MOBILE, ROOT};
use crate::Refused;
use anyhow::{bail, Context as _, Result};
use oneios_common::json::Value;
use oneios_common::pyrepr::{repr_str, str_value};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub const ONEIOS: &str = "oneios";
pub const ONEIOS_GUEST: &str = "/private/var/oneios";
const INIT_LABEL: &str = "com.oneios.init";
const INIT_ROOT_PATH: &str = "usr/libexec/oneios-init";
/// oneios-seed runs before Data exists: it is not installed here
const HELPERS: [&str; 4] = ["oneios-init", "oneios-bootstat", "oneios-mg", "oneios-thermal"];
const LOGD: Owner = (272, 272);
const RECOVER_LABELS: [&str; 4] = ["com.apple.backboardd", "com.apple.SpringBoard", "com.apple.configd", "com.apple.runningboardd"];
pub const PREF_GROUPS: [&str; 6] = ["setup", "migration", "watchdogs", "awake", "coreanimation", "dictation"];

/// `roottweak apply`'s options (cli.py's `options`).
#[derive(Clone, Debug, Default)]
pub struct Options {
    pub build: Option<String>,
    pub any_version: bool,
    pub groups: Vec<String>,
    pub without_groups: Vec<String>,
    pub keep: Vec<String>,
    pub disable: Vec<String>,
    pub keep_recover_on_crash: bool,
    pub helpers: Option<PathBuf>,
    pub metal_plugin: Option<String>,
    pub camera_plugin: Option<PathBuf>,
    pub metal_plugin_unchecked: bool,
    pub metal_clients: Vec<String>,
    pub appos: Option<PathBuf>,
    /// the preference groups left out (--no-setup ...)
    pub no_prefs: Vec<String>,
    pub lighter_visuals: bool,
    pub plain_keybag: bool,
}

pub struct Context {
    pub root: Tree,
    pub data: Tree,
    pub rw: bool,
    pub device: Value,
    pub daemons: Daemons,
    pub prefs: Vec<Pref>,
    pub o: Options,
    pub results: Vec<Value>,
    pub build: String,
    pub trusted: Vec<Value>,
    pub contract: Value,
    pub m: Manifest,
    tweak: String,
}

fn strings(items: &[String]) -> Value {
    Value::List(items.iter().map(|s| Value::Str(s.clone())).collect())
}

fn dict(pairs: Vec<(&str, Value)>) -> Value {
    Value::Dict(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

impl Context {
    pub fn new(root: Tree, data: Tree, rw: bool, device: Value, daemons: Daemons, prefs: Vec<Pref>, o: Options, m: Manifest) -> Context {
        let contract = dict(vec![
            ("kernel_args", Value::list()),
            ("launch_policy", dict(vec![("disable_jobs", Value::list()), ("service_manager", Value::Null), ("force_enabled", Value::list())])),
            ("addon_mounts", Value::list()),
            ("notes", Value::list()),
        ]);
        Context { root, data, rw, device, daemons, prefs, o, results: Vec::new(), build: String::new(), trusted: Vec::new(), contract, m, tweak: String::new() }
    }

    pub fn say(&mut self, state: &str, item: &str, note: &str, labels: Option<&[String]>) {
        let mut r = dict(vec![("tweak", Value::Str(self.tweak.clone())), ("state", state.into()), ("item", item.into()), ("note", note.into())]);
        if let Some(labels) = labels {
            // the whole list is in report.json; the line names the first few
            r.set("labels", strings(labels));
            let mut shown = labels.iter().take(6).cloned().collect::<Vec<_>>().join(", ");
            if labels.len() > 6 {
                shown.push_str(&format!(" and {} more (report.json)", labels.len() - 6));
            }
            r.set("note", if note.is_empty() { shown } else { format!("{note}: {shown}") });
        }
        self.results.push(r);
    }

    pub fn set_tweak(&mut self, name: &str) {
        self.tweak = name.to_string();
        self.root.tweak = name.to_string();
        self.data.tweak = name.to_string();
    }

    fn kernel_arg(&mut self, arg: &str, why: &str) {
        let Some(Value::List(args)) = self.contract.get_mut("kernel_args") else { return };
        if !args.iter().any(|a| a.get("arg").and_then(Value::as_str) == Some(arg)) {
            args.push(dict(vec![("arg", arg.into()), ("why", why.into())]));
        }
    }

    fn policy(&mut self) -> &mut Value {
        self.contract.get_mut("launch_policy").unwrap()
    }

    fn push(&mut self, key: &str, value: Value) {
        if let Some(Value::List(items)) = self.contract.get_mut(key) {
            items.push(value);
        }
    }

    fn dev(&self, table: &str, key: &str) -> Value {
        self.device.get(table).and_then(|t| t.get(key)).cloned().unwrap_or(Value::Null)
    }
}

/// Every tweak, in tweaks.py's order (ALL), with the name the manifest and the report give it.
pub type Tweak = fn(&mut Context) -> Result<()>;
pub const ALL: [(&str, Tweak); 14] = [
    ("version", version),
    ("launchd-jobs", launchd_jobs),
    ("launchd-crash-policy", launchd_crash_policy),
    ("host-services", host_services),
    ("metal-plugin", metal_plugin),
    ("camera-plugin", camera_plugin),
    ("app-cryptex", app_cryptex),
    ("staged-apps", staged_apps),
    ("preferences", preferences),
    ("data-layout", data_layout),
    ("keybag", keybag),
    ("feature-flags", feature_flags),
    ("identity", identity),
    ("kernel-features", kernel_features),
];

fn py(v: Option<&P>) -> String {
    v.map_or_else(|| "None".to_string(), P::py_str)
}

// ---- version

fn version(cx: &mut Context) -> Result<()> {
    let plist = cx.root.read_plist("System/Library/CoreServices/SystemVersion.plist", None)?;
    let plist = match plist {
        Some(p) if p.truthy() && cx.root.exists("sbin/launchd")? => p,
        _ => bail!(Refused(format!("{}: not an iOS root (no SystemVersion.plist or no sbin/launchd)", cx.root.top.display()))),
    };
    let build = match cx.o.build.clone().filter(|b| !b.is_empty()) {
        Some(b) => Some(b),
        None => match plist.get("ProductBuildVersion") {
            Some(P::Str(s)) if !s.is_empty() => Some(s.clone()),
            None | Some(P::Str(_)) => None,
            Some(other) => bail!(Refused(format!("SystemVersion.plist: ProductBuildVersion is {}, not a string", other.py_str()))),
        },
    };
    let Some(build) = build else { bail!(Refused("SystemVersion.plist names no build; give --build".into())) };
    cx.build = build;
    let have = plist.get("ProductVersion");
    let want = str_value(&cx.dev("device", "version"));
    let name = py(plist.get("ProductName"));
    // A build the device profile lists ([builds]: build -> ProductVersion, e.g. 24B156 27.2.1 beside the default
    // 24A446 27.0.1) with its own version: this profile's, kept.
    let listed = cx.device.get("builds").and_then(|b| b.get(&cx.build)).map(str_value);
    if listed.is_some() && listed.as_deref() == have.and_then(P::as_str) && have.and_then(P::as_str) != Some(want.as_str()) {
        let note = format!("{name} {} ({}), a build the device profile lists", py(have), cx.build);
        cx.say("kept", "version", &note, None);
        return Ok(());
    }
    // 20F66 (iOS 16.5): the Galaxy A56's userland (docs/a56.md, profiles/ios-16.5-a56.toml), the one other build these
    // tweaks know (apply.rs writes its xnuhost.conf): applied with this profile, as --any-version would, and said so
    let known_16 = cx.build == "20F66" && have.and_then(P::as_str) == Some("16.5");
    if have.and_then(P::as_str) != Some(want.as_str()) {
        if known_16 {
            let note = format!("root is {} ({}), the A56's iOS 16.5 (docs/a56.md): applied with the {want} profile", py(have), cx.build);
            cx.say("applied", "version", &note, None);
            return Ok(());
        }
        if !cx.o.any_version {
            bail!(Refused(format!("the root is {name} {} ({}); the profile is for {want}. --any-version applies it anyway", py(have), cx.build)));
        }
        let note = format!("root is {} ({}), profile is for {want}: applied anyway (--any-version)", py(have), cx.build);
        cx.say("applied", "version", &note, None);
    } else {
        let note = format!("{name} {} ({})", py(have), cx.build);
        cx.say("kept", "version", &note, None);
    }
    Ok(())
}

// ---- launchd jobs

/// The launchd job cache.
fn cache(cx: &Context) -> Result<P> {
    match cx.root.read_plist(launchd::CACHE, None)? {
        Some(c @ P::Dict(_)) => Ok(c),
        _ => bail!(Refused(format!("{}: no launchd job cache in this root", launchd::CACHE))),
    }
}

fn write_root_binary(cx: &mut Context, path: &str, value: &P) -> Result<bool> {
    cx.root.write_plist(&mut cx.m, path, value, ROOT, 0o644, true, None)
}

fn jetsam_files(cx: &Context) -> Result<Vec<String>> {
    Ok(cx
        .root
        .listdir(launchd::DAEMONS_DIR)?
        .into_iter()
        .filter(|n| n.starts_with("com.apple.jetsamproperties.") && n.ends_with(".plist"))
        .map(|n| format!("{}/{n}", launchd::DAEMONS_DIR))
        .collect())
}

fn launchd_jobs(cx: &mut Context) -> Result<()> {
    let mut wanted = cx.daemons.selected(&cx.o.groups, &cx.o.without_groups, &cx.o.keep)?;
    for label in cx.o.disable.clone() {
        // by hand, also against the keep list: said in the report
        if cx.daemons.keep.contains(&label) {
            let why = cx.daemons.keep_reasons.get(&label).cloned().unwrap_or_else(|| "see daemons.toml [keep]".into());
            cx.say("applied", &format!("warning: {label}"), &format!("disabled by --disable although the policy keeps it: {why}"), None);
        }
        match wanted.iter_mut().find(|(l, _)| *l == label) {
            Some(slot) => slot.1 = "by hand".into(),
            None => wanted.push((label, "by hand".into())),
        }
    }
    let mut cache = cache(cx)?;
    let have = launchd::labels(&cache);
    let mut present: Vec<String> = wanted.iter().map(|(l, _)| l.clone()).filter(|l| have.contains(l)).collect();
    present.sort();
    let mut missing: Vec<String> = wanted.iter().map(|(l, _)| l.clone()).filter(|l| !have.contains(l)).collect();
    missing.sort();
    let mut absent_keep: Vec<String> = cx.daemons.keep.iter().filter(|l| !have.contains(*l)).cloned().collect();
    absent_keep.sort();

    let path = format!("{}/disabled.plist", launchd::OVERRIDES_DIR);
    let existing = match cx.data.read_plist(&path, Some(P::dict()))? {
        Some(p) if !p.truthy() => P::dict(),
        Some(p @ P::Dict(_)) => p,
        None => P::dict(),
        Some(_) => bail!(Refused(format!("data:{path}: not a dictionary"))),
    };
    let merged = launchd::overrides(&existing, &present);
    cx.data.mkdir(&mut cx.m, launchd::OVERRIDES_DIR, ROOT, 0o755)?;
    let changed = cx.data.write_plist(&mut cx.m, &path, &merged, ROOT, 0o644, false, None)?;
    // An empty marker selects launchd's current override loader (UniXNU-droidvm DroidVMLaunchPolicy.c).
    let marker = format!("{}/disabled.migrated", launchd::OVERRIDES_DIR);
    let body = if cx.data.exists(&marker)? { cx.data.read(&marker)? } else { Vec::new() };
    cx.data.write(&mut cx.m, &marker, &body, ROOT, 0o644, None)?;
    let wanted_of: BTreeMap<&str, &str> = wanted.iter().map(|(l, w)| (l.as_str(), w.as_str())).collect();
    let mut by_group: BTreeMap<&str, usize> = BTreeMap::new();
    for label in &present {
        *by_group.entry(wanted_of[label.as_str()]).or_default() += 1;
    }
    let summary = by_group.iter().map(|(g, n)| format!("{g} {n}")).collect::<Vec<_>>().join(", ");
    let note = format!("{} jobs disabled on the Data volume ({summary}); root:wheel 0644 or launchd ignores it", present.len());
    cx.say(if changed { "applied" } else { "kept" }, "disabled.plist", &note, None);
    if !missing.is_empty() {
        cx.say("not-found", "jobs not in this root", &format!("{} of the policy's labels", missing.len()), Some(&missing));
    }
    if !absent_keep.is_empty() {
        cx.say("not-found", "kept jobs not in this root", &format!("{} of the keep list", absent_keep.len()), Some(&absent_keep));
    }
    cx.policy().set("disable_jobs", strings(&present));

    // iOS 27: DisableAllowed = false loads a job although it is disabled.
    let mut forced = BTreeSet::new();
    for file in jetsam_files(cx)? {
        if let Some(j) = cx.root.read_plist(&file, Some(P::dict()))? {
            forced.extend(launchd::forced(&j));
        }
    }
    let still: Vec<String> = present.iter().filter(|l| forced.contains(*l)).cloned().collect();
    if cx.rw {
        let gone = launchd::remove_jobs(&mut cache, &present);
        write_root_binary(cx, launchd::CACHE, &cache)?;
        cx.say(if gone.is_empty() { "kept" } else { "applied" }, "launchd cache", &format!("{} jobs taken out of {}", gone.len(), launchd::CACHE), None);
        for file in jetsam_files(cx)? {
            let Some(mut jetsam) = cx.root.read_plist(&file, None)? else { continue };
            let labels = launchd::allow_disable(&mut jetsam);
            if !labels.is_empty() {
                write_root_binary(cx, &file, &jetsam)?;
            }
            let note = if labels.is_empty() { "no DisableAllowed = false".to_string() } else { format!("DisableAllowed = false removed from {} jobs", labels.len()) };
            cx.say(if labels.is_empty() { "kept" } else { "applied" }, tree::basename(&file), &note, None);
        }
    } else if !still.is_empty() {
        cx.policy().set("force_enabled", strings(&still));
        let note = format!(
            "{} disabled jobs are marked DisableAllowed = false in the jetsam properties; on a sealed root the hosted kernel \
             shows launchd a copy with the mark renamed (0098, oneios-disable-allowed; with it off they run all the same): {}",
            still.len(),
            still.join(", ")
        );
        cx.say("contract", "DisableAllowed", &note, None);
    } else {
        cx.say("kept", "DisableAllowed", "none of the disabled jobs is marked DisableAllowed = false", None);
    }
    if !cx.rw {
        // The line's wording is kept as it was first written: the report is a file of the seed.
        cx.say("contract", "launch policy", "the kernel writes the same override before launchd starts (launch_policy.disable_jobs), for a Data volume whose file owners it cannot trust", None);
        let o = dict(vec![
            ("path", format!("/private/var/{path}").into()),
            ("marker", format!("/private/var/{}/disabled.migrated", launchd::OVERRIDES_DIR).into()),
            ("owner", "root:wheel".into()),
            ("mode", "0644".into()),
            ("carried_by", "the Data seed; the first start's unpack sets the owner".into()),
            (
                "read_by_launchd",
                "once per boot, after the boot task finish-demo-restore, only when uid 0 owns the file and neither group nor others can write it; a job in it is not spawned and its Mach services are not registered (roottweak boot-check shows it from launchd.log)".into(),
            ),
        ]);
        cx.policy().set("override", o);
    }
    Ok(())
}

fn launchd_crash_policy(cx: &mut Context) -> Result<()> {
    if !cx.rw {
        cx.push("notes", "sealed root: _PanicOnCrash and RecoverOnCrash stay as Apple set them; three crashes of backboardd, SpringBoard, configd or runningboardd within 60 s restart the guest".into());
        cx.say("contract", "crash policy", "not editable on a sealed root: launchd restarts the guest after 3 crashes in 60 s of a RecoverOnCrash job", None);
        return Ok(());
    }
    let mut cache = cache(cx)?;
    let mut renamed = 0;
    let mut jobs = launchd::jobs(&cache);
    jobs.sort();
    for (_, group, key) in jobs {
        let job = launchd::job_mut(&mut cache, &group, &key).unwrap();
        if let Some(v) = job.remove("_PanicOnCrash") {
            job.set("_PanicOnCrasX", v);
            renamed += 1;
        }
    }
    if renamed > 0 {
        write_root_binary(cx, launchd::CACHE, &cache)?;
    }
    cx.say(if renamed > 0 { "applied" } else { "kept" }, "_PanicOnCrash", &if renamed > 0 { format!("renamed in {renamed} jobs") } else { "no job has it".into() }, None);
    if cx.o.keep_recover_on_crash {
        cx.say("off", "RecoverOnCrash", "kept (--keep-recover-on-crash)", None);
        return Ok(());
    }
    let mut total = BTreeSet::new();
    for file in jetsam_files(cx)? {
        let Some(mut jetsam) = cx.root.read_plist(&file, None)? else { continue };
        let labels = launchd::drop_recover_on_crash(&mut jetsam, &RECOVER_LABELS);
        if !labels.is_empty() {
            write_root_binary(cx, &file, &jetsam)?;
            total.extend(labels);
        }
    }
    let note = if total.is_empty() { "no job has it".to_string() } else { format!("removed from {}", total.into_iter().collect::<Vec<_>>().join(", ")) };
    cx.say(if note.starts_with("removed") { "applied" } else { "kept" }, "RecoverOnCrash", &note, None);
    Ok(())
}

// ---- our own services

/// Copies one of our Mach-O files into the guest and notes it for the owner trust list.
fn install_binary(cx: &mut Context, data_tree: bool, source: &Path, path: &str, mode: u32) -> Result<()> {
    let data = std::fs::read(source).with_context(|| source.display().to_string())?;
    if !macho::is_macho(&data) {
        bail!(Refused(format!("{}: not a Mach-O (a host build? the guest needs helpers/build.sh output)", source.display())));
    }
    let slices = macho::describe(&data).map_err(|e| Refused(format!("{}: {e}; the guest runs only signed code", source.display())))?;
    let tree = if data_tree { &cx.data } else { &cx.root };
    tree.write(&mut cx.m, path, &data, ROOT, mode, None)?;
    let guest = format!("{}{path}", if data_tree { "/private/var/" } else { "/" });
    for s in slices {
        let mut warnings = Vec::new();
        if s.arch != "arm64e" {
            warnings.push(format!("{}, not arm64e: iOS 27 kills arm64 platform binaries at exec (Droid-VM/ownervisor build.py)", s.arch));
        }
        if s.page_size != 4096 {
            warnings.push(format!("signature pages are {} bytes, not 4096 (UniXNU signs with --pagesize 4096)", s.page_size));
        }
        if let Some(p) = s.platform.as_deref().filter(|p| *p != "ios") {
            warnings.push(format!("built for {p}, not ios"));
        }
        cx.trusted.push(crate::trust_entry(&guest, &s, &warnings));
        for w in &warnings {
            cx.say("applied", &format!("warning: {guest}"), w, None);
        }
    }
    Ok(())
}

/// A service file: `arg=` lines, then the keys in the order given (`env=` lines for the environment).
fn conf(args: &[String], keys: &[(&str, i64)], env: &[(&str, String)]) -> Vec<u8> {
    let mut lines: Vec<String> = args.iter().map(|a| format!("arg={a}")).collect();
    for (k, v) in keys {
        lines.push(format!("{k}={v}"));
    }
    for (k, v) in env {
        lines.push(format!("env={k}={v}"));
    }
    (lines.join("\n") + "\n").into_bytes()
}

fn host_services(cx: &mut Context) -> Result<()> {
    for dir in ["", "bin", "services.d", "enabled", "log", "state", "run"] {
        let path = format!("{ONEIOS}/{dir}");
        cx.data.mkdir(&mut cx.m, path.trim_end_matches('/'), ROOT, 0o755)?;
    }
    let expect = match cx.device.get("mobilegestalt").and_then(|m| m.get("expect")) {
        Some(Value::Dict(items)) => {
            let mut items: Vec<&(String, Value)> = items.iter().collect();
            items.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
            items.iter().map(|(k, v)| format!("{k}={}\n", str_value(v))).collect::<String>()
        }
        _ => String::new(),
    };
    cx.data.write(&mut cx.m, &format!("{ONEIOS}/identity.expect"), expect.as_bytes(), ROOT, 0o644, None)?;

    let helpers = cx.o.helpers.clone();
    let mut have: BTreeSet<String> = BTreeSet::new();
    if let Some(helpers) = &helpers {
        for name in HELPERS {
            let source = helpers.join(name);
            if tree::is_file(&source) {
                install_binary(cx, true, &source, &format!("{ONEIOS}/bin/{name}"), 0o755)?;
                have.insert(name.to_string());
            }
        }
        // The debug shell, when the helper build was given one (helpers/README.md): any other file of the folder.
        for name in tree::listdir(helpers)? {
            let source = helpers.join(&name);
            if HELPERS.contains(&name.as_str()) || name == "oneios-seed" || name.starts_with('.') || !tree::is_file(&source) {
                continue;
            }
            let head = read_head(&source, 8)?;
            if macho::is_macho(&head) {
                install_binary(cx, true, &source, &format!("{ONEIOS}/bin/{name}"), 0o755)?;
                have.insert(name);
            }
        }
    }
    let env = [("ONEIOS_ROOT", ONEIOS_GUEST.to_string()), ("PATH", format!("{ONEIOS_GUEST}/bin:/usr/bin:/bin:/usr/sbin:/sbin"))];
    let bin = |n: &str| format!("{ONEIOS_GUEST}/bin/{n}");
    let services: [(&str, Vec<u8>, &str); 4] = [
        // no bootstrap wait: it must report the stages before launchd's services exist
        ("bootstat", conf(&[bin("oneios-bootstat")], &[("keepalive", 1), ("throttle", 10), ("bootstrap", 0)], &env), "oneios-bootstat"),
        (
            "identity",
            conf(&[bin("oneios-mg"), "--expect".into(), format!("{ONEIOS_GUEST}/identity.expect")], &[("keepalive", 0), ("bootstrap", 1)], &env),
            "oneios-mg",
        ),
        (
            "dropbear",
            conf(&[bin("dropbear"), "-F".into(), "-E".into(), "-R".into(), "-p".into(), "2222".into()], &[("keepalive", 1), ("throttle", 5), ("bootstrap", 1)], &env),
            "dropbear",
        ),
        // the phone's thermal pressure (drivers/thermal) into notify's com.apple.system.thermalpressurelevel
        ("thermal", conf(&[bin("oneios-thermal")], &[("keepalive", 1), ("throttle", 10), ("bootstrap", 1)], &env), "oneios-thermal"),
    ];
    let mut enabled = Vec::new();
    for (name, body, binary) in services {
        cx.data.write(&mut cx.m, &format!("{ONEIOS}/services.d/{name}.conf"), &body, ROOT, 0o644, None)?;
        if have.contains(binary) {
            cx.data.write(&mut cx.m, &format!("{ONEIOS}/enabled/{name}"), b"", ROOT, 0o644, None)?;
            enabled.push(name);
        } else {
            cx.data.remove(&mut cx.m, &format!("{ONEIOS}/enabled/{name}"))?;
        }
    }
    if !have.contains("oneios-init") {
        cx.say("off", "oneios-init", "no helpers given (--helpers DIR, the output of userland/helpers/build.sh): the service files are placed, nothing starts them", None);
    } else {
        let list = if enabled.is_empty() { "none".to_string() } else { enabled.join(", ") };
        cx.say("applied", "oneios-init", &format!("in {ONEIOS_GUEST}/bin; services at boot: {list}"), None);
    }
    if !have.contains("dropbear") {
        cx.say("off", "ssh", "no dropbear in --helpers: no debug shell (helpers/README.md says how to add one)", None);
    }

    if cx.rw {
        let mut cache = cache(cx)?;
        let env_p = P::Dict(env.iter().map(|(k, v)| (k.to_string(), P::Str(v.clone()))).collect());
        let job = P::Dict(vec![
            ("Label".into(), P::Str(INIT_LABEL.into())),
            ("ProgramArguments".into(), P::Array(vec![P::Str(format!("/{INIT_ROOT_PATH}")), P::Str(ONEIOS_GUEST.into())])),
            ("RunAtLoad".into(), P::Bool(true)),
            ("KeepAlive".into(), P::Bool(true)),
            ("ThrottleInterval".into(), P::Int(10)),
            ("UserName".into(), P::Str("root".into())),
            ("EnvironmentVariables".into(), env_p),
            ("StandardErrorPath".into(), P::Str("/dev/console".into())),
            ("POSIXSpawnType".into(), P::Str("Interactive".into())),
            ("MinimalBootProfile".into(), P::Bool(true)),
            ("EnablePressuredExit".into(), P::Bool(false)),
            ("EnableTransactions".into(), P::Bool(false)),
        ]);
        if have.contains("oneios-init") {
            // launchd refuses a job whose program is not on the root file system (iOSPlus10 ios26-first-boot.md)
            install_binary(cx, false, &helpers.as_ref().unwrap().join("oneios-init"), INIT_ROOT_PATH, 0o755)?;
            let changed = launchd::add_daemon(&mut cache, INIT_LABEL, &job);
            write_root_binary(cx, launchd::CACHE, &cache)?;
            cx.root.write_plist(&mut cx.m, &format!("{}/{INIT_LABEL}.plist", launchd::DAEMONS_DIR), &job, ROOT, 0o644, false, None)?;
            cx.say(if changed { "applied" } else { "kept" }, "launchd job", &format!("{INIT_LABEL} runs /{INIT_ROOT_PATH}"), None);
        }
        cx.kernel_arg(
            "launchd_unsecure_cache=1",
            "an edited launchd cache loads only with it: launchd_cache_loader then skips the cache's signature check (iOSPlus10 ios26-first-boot.md; vphone patches the loader instead)",
        );
    } else {
        let manager = dict(vec![
            ("path", format!("{ONEIOS_GUEST}/bin/oneios-init").into()),
            ("args", strings(&[ONEIOS_GUEST.to_string()])),
            ("uid", 0.into()),
            ("present", have.contains("oneios-init").into()),
            (
                "how",
                "posix_spawn once from the launch policy hook while launchd is exec'd, then copy launchd's bootstrap port into its task (UniXNU-droidvm DroidVMLaunchPolicy.c spawn_service_manager, droidvm_fix_ownervisor_bootstrap)".into(),
            ),
        ]);
        cx.policy().set("service_manager", manager);
        cx.say("contract", "service manager", &format!("a sealed root has no launchd label for it: the kernel starts {ONEIOS_GUEST}/bin/oneios-init (launch_policy.service_manager)"), None);
    }
    Ok(())
}

fn read_head(path: &Path, n: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).with_context(|| path.display().to_string())?;
    let mut buf = vec![0; n];
    let mut got = 0;
    while got < n {
        let k = f.read(&mut buf[got..])?;
        if k == 0 {
            break;
        }
        got += k;
    }
    buf.truncate(got);
    Ok(buf)
}

// ---- Metal plugin

fn metal_plugin(cx: &mut Context) -> Result<()> {
    let Some(source) = cx.o.metal_plugin.clone().filter(|s| !s.is_empty()) else {
        cx.say("off", "metal plugin", "no --metal-plugin BUNDLE: backboardd composites on the CPU", None);
        return Ok(());
    };
    let source = source.trim_end_matches('/').to_string();
    let name = tree::basename(&source).to_string();
    let src = PathBuf::from(&source);
    let info_path = src.join("Info.plist");
    if !name.ends_with(".bundle") || !tree::is_file(&info_path) {
        bail!(Refused(format!("{source}: not a .bundle with an Info.plist")));
    }
    let info = crate::plist::loads(&std::fs::read(&info_path)?).map_err(|e| anyhow::anyhow!("{}: {e}", info_path.display()))?;
    let executable = match info.get("CFBundleExecutable") {
        Some(P::Str(e)) if !e.is_empty() && tree::is_file(&src.join(e)) => e.clone(),
        _ => bail!(Refused(format!("{source}: Info.plist names no CFBundleExecutable that is in the bundle"))),
    };
    // The plug-in writes QuartzCore options at fixed addresses of one dyld shared cache, so a bundle is placed only
    // when it says it was built for this guest: a file `oneios-target` in it holding "<product> <build>".
    let want = format!("{} {}", str_value(&cx.dev("device", "product")), cx.build);
    let marker = src.join("oneios-target");
    let have = if tree::is_file(&marker) { Some(py_strip(&String::from_utf8(std::fs::read(&marker)?).map_err(|e| anyhow::anyhow!("{}: {e}", marker.display()))?)) } else { None };
    if have.as_deref() != Some(want.as_str()) && !cx.o.metal_plugin_unchecked {
        let shown = have.as_deref().map_or_else(|| "None".to_string(), repr_str);
        let note = format!(
            "{name} not placed: it does not say it was built for this guest (its oneios-target is {shown}, wanted {}). The upstream plug-in holds addresses of the iPad's shared cache; placed as it is it would write into backboardd at wrong addresses. --metal-plugin-unchecked places it anyway",
            repr_str(&want)
        );
        cx.say("off", "metal plugin", &note, None);
        return Ok(());
    }
    let stem = &name[..name.len() - ".bundle".len()];
    let clients: Vec<String> = if cx.o.metal_clients.is_empty() {
        ["WidgetRenderer_Default", "*Poster*", "iconservicesagent", "SBRendererService"].iter().map(|s| s.to_string()).collect()
    } else {
        cx.o.metal_clients.clone()
    };
    let client_text = clients.join("\n") + "\n";
    let (on_data, base, guest) = if cx.rw {
        (false, "System/Library/Extensions".to_string(), "/System/Library/Extensions".to_string())
    } else {
        (true, format!("{ONEIOS}/addons/Extensions"), format!("{ONEIOS_GUEST}/addons/Extensions"))
    };
    {
        let t = if on_data { &cx.data } else { &cx.root };
        t.mkdir(&mut cx.m, &format!("{base}/{name}"), ROOT, 0o755)?;
    }
    // os.walk(source): top-down, each folder's names sorted, links to folders not gone into
    let mut stack: Vec<(PathBuf, String)> = vec![(src.clone(), ".".to_string())];
    while let Some((current, rel)) = stack.pop() {
        let (dirs, files) = tree::walk_level(&current)?;
        for d in &dirs {
            let path = tree::normpath(&format!("{base}/{name}/{rel}/{d}"));
            let t = if on_data { &cx.data } else { &cx.root };
            t.mkdir(&mut cx.m, &path, ROOT, 0o755)?;
        }
        for file in &files {
            let host = current.join(file);
            let path = tree::normpath(&format!("{base}/{name}/{rel}/{file}"));
            if std::fs::symlink_metadata(&host).is_ok_and(|m| m.file_type().is_symlink()) {
                bail!(Refused(format!("{}: a symbolic link in the plugin bundle", host.display())));
            }
            let head = read_head(&host, 8)?;
            if macho::is_macho(&head) {
                install_binary(cx, on_data, &host, &path, 0o755)?;
            } else {
                let t = if on_data { &cx.data } else { &cx.root };
                t.copy_in(&mut cx.m, &host, &path, ROOT, Some(0o644))?;
            }
        }
        for d in dirs.iter().rev() {
            let next = current.join(d);
            if tree::walk_into(&next) {
                stack.push((next, if rel == "." { d.clone() } else { format!("{rel}/{d}") }));
            }
        }
    }
    {
        let t = if on_data { &cx.data } else { &cx.root };
        // The processes that get the plugin's device, one per line; "*" is every process (the kernel's GPU driver).
        t.write(&mut cx.m, &format!("{base}/{stem}.clients"), client_text.as_bytes(), ROOT, 0o644, None)?;
        // With this file the plugin gives its device to backboardd's own discovery.
        t.write(&mut cx.m, &format!("{base}/{stem}.publish"), b"1\n", ROOT, 0o644, None)?;
    }
    if !cx.rw {
        let mount = dict(vec![
            ("source", guest.clone().into()),
            ("over", "/System/Library/Extensions".into()),
            ("type", "bindfs".into()),
            ("union", true.into()),
            ("readonly", true.into()),
            ("how", "mounted by the kernel before launchd, files shown as root's (UniXNU sheng-gpu-addon=1)".into()),
        ]);
        cx.push("addon_mounts", mount);
    }
    let principal = match info.get("NSPrincipalClass") {
        Some(P::Str(s)) => Value::Str(s.clone()),
        None => Value::Null,
        Some(other) => bail!(Refused(format!("{source}: NSPrincipalClass is {}, not a string", other.py_str()))),
    };
    let metal = dict(vec![
        ("bundle", format!("/System/Library/Extensions/{name}").into()),
        ("executable", executable.into()),
        ("principal_class", principal),
        ("clients", strings(&clients)),
        ("kernel", "the accelerator service must publish MetalPluginName (the bundle name without .bundle) and MetalPluginClassName, as an IOAccelerator does for Apple's driver".into()),
    ]);
    cx.contract.set("metal", metal);
    let note = format!(
        "{name} in {guest}; clients: {}{}",
        clients.join(", "),
        if cx.rw { "" } else { "; shown over /System/Library/Extensions by the kernel (addon_mounts)" }
    );
    cx.say("applied", "metal plugin", &note, None);
    Ok(())
}

/// Python's `str.strip()` (Unicode white space).
fn py_strip(s: &str) -> String {
    s.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)).to_string()
}

fn camera_plugin(cx: &mut Context) -> Result<()> {
    let Some(source) = cx.o.camera_plugin.clone().filter(|s| !s.as_os_str().is_empty()) else {
        cx.say("off", "camera plugin", "no --camera-plugin DIR: iOS finds no camera (CMCapture loads no create function)", None);
        return Ok(());
    };
    let name = "H9ISP.mediacapture";
    let host = if tree::is_dir(&source) { source.join(name) } else { source.clone() };
    if host.file_name().and_then(|n| n.to_str()) != Some(name) || !tree::is_file(&host) {
        bail!(Refused(format!("{}: no {name} (the output of userland/camera/build.sh)", source.display())));
    }
    let (on_data, base, guest) = if cx.rw {
        (false, "System/Library/MediaCapture".to_string(), "/System/Library/MediaCapture".to_string())
    } else {
        (true, format!("{ONEIOS}/addons/MediaCapture"), format!("{ONEIOS_GUEST}/addons/MediaCapture"))
    };
    {
        let t = if on_data { &cx.data } else { &cx.root };
        t.mkdir(&mut cx.m, &base, ROOT, 0o755)?;
    }
    install_binary(cx, on_data, &host, &format!("{base}/{name}"), 0o755)?;
    if !cx.rw {
        let mount = dict(vec![
            ("source", guest.clone().into()),
            ("over", "/System/Library/MediaCapture".into()),
            ("type", "bindfs".into()),
            ("union", true.into()),
            ("readonly", true.into()),
            ("how", "mounted by the kernel before launchd (hosted 0056; oneios-camera-plugin=0 turns it off)".into()),
        ]);
        cx.push("addon_mounts", mount);
    }
    let camera = dict(vec![
        ("plugin", format!("/System/Library/MediaCapture/{name}").into()),
        ("symbol", "H9ISPCaptureDeviceCreate".into()),
        ("kernel", "an IOService of class AppleH9CamIn (CMCapture probes for it by IOServiceMatching) and its user client AppleH9CamInUserClient for the frames".into()),
    ]);
    cx.contract.set("camera", camera);
    let note = format!("{name} in {guest}{}", if cx.rw { "" } else { "; shown over /System/Library/MediaCapture by the kernel (addon_mounts, hosted 0056)" });
    cx.say("applied", "camera plugin", &note, None);
    Ok(())
}

// ---- cryptexes and the removable system apps

/// Copies an extracted volume into a tree: files, directories and links, modes from the source.
fn copy_tree(cx: &mut Context, on_data: bool, source: &Path, base: &str) -> Result<usize> {
    let mut count = 0;
    let t = if on_data { &cx.data } else { &cx.root };
    t.mkdir(&mut cx.m, base, ROOT, 0o755)?;
    let mut stack: Vec<(PathBuf, String)> = vec![(source.to_path_buf(), ".".to_string())];
    while let Some((current, rel)) = stack.pop() {
        let (dirs, files) = tree::walk_level(&current)?;
        let mut kept = Vec::new();
        for name in dirs {
            let host = current.join(&name);
            let path = tree::normpath(&format!("{base}/{rel}/{name}"));
            let meta = std::fs::symlink_metadata(&host)?;
            if meta.file_type().is_symlink() {
                let target = std::fs::read_link(&host)?;
                t.symlink(&mut cx.m, &path, &target.to_string_lossy(), ROOT)?;
            } else if name == ".fseventsd" || name == ".Trashes" {
            } else {
                let mode = meta.mode() & 0o7777;
                t.mkdir(&mut cx.m, &path, ROOT, if mode == 0 { 0o755 } else { mode })?;
                kept.push(name);
            }
        }
        for name in &files {
            let host = current.join(name);
            let path = tree::normpath(&format!("{base}/{rel}/{name}"));
            let meta = std::fs::symlink_metadata(&host)?;
            if meta.file_type().is_symlink() {
                let target = std::fs::read_link(&host)?;
                t.symlink(&mut cx.m, &path, &target.to_string_lossy(), ROOT)?;
            } else if meta.is_file() {
                let data = std::fs::read(&host).with_context(|| host.display().to_string())?;
                t.write(&mut cx.m, &path, &data, ROOT, meta.mode() & 0o7777, None)?;
                count += 1;
            }
        }
        for d in kept.iter().rev() {
            stack.push((current.join(d), if rel == "." { d.clone() } else { format!("{rel}/{d}") }));
        }
    }
    Ok(count)
}

fn app_cryptex(cx: &mut Context) -> Result<()> {
    let source = cx.o.appos.clone().filter(|s| !s.as_os_str().is_empty());
    if source.is_none() && !cx.rw {
        let c = dict(vec![
            ("image", "app.img".into()),
            ("mount", "/private/preboot/Cryptexes/App".into()),
            ("readonly", true.into()),
            ("also", "the SystemOS volume by its cryptex path: /System over /private/preboot/Cryptexes/OS/System and /usr over .../OS/usr, not as unions (UniXNU zzyb-native-cryptex-os-view.patch)".into()),
        ]);
        cx.contract.set("app_cryptex", c);
        cx.say("contract", "App cryptex", "not copied: the kernel mounts the image set's app.img at /private/preboot/Cryptexes/App (app_cryptex)", None);
        return Ok(());
    }
    let Some(source) = source else {
        cx.say("off", "App cryptex", "no --appos DIR (the extracted Cryptex1,AppOS volume): Safari's daemons are missing", None);
        return Ok(());
    };
    if !tree::is_dir(&source) {
        bail!(Refused(format!("{}: not a directory", source.display())));
    }
    if cx.rw {
        let count = copy_tree(cx, false, &source, "private/preboot/Cryptexes/App")?;
        cx.say("applied", "App cryptex", &format!("{count} files in /private/preboot/Cryptexes/App"), None);
        return Ok(());
    }
    let count = copy_tree(cx, true, &source, &format!("{ONEIOS}/preboot/Cryptexes/App"))?;
    for point in ["System", "usr"] {
        cx.data.mkdir(&mut cx.m, &format!("{ONEIOS}/preboot/Cryptexes/OS/{point}"), ROOT, 0o755)?;
    }
    let mount = |source: &str, over: &str, union: bool, how: &str| {
        dict(vec![("source", source.into()), ("over", over.into()), ("type", "bindfs".into()), ("union", union.into()), ("readonly", true.into()), ("how", how.into())])
    };
    cx.push("addon_mounts", mount(&format!("{ONEIOS_GUEST}/preboot"), "/private/preboot", true, "UniXNU owner-app-cryptex=1 (zzyb-native-cryptex-os-view.patch)"));
    cx.push("addon_mounts", mount("/System", "/private/preboot/Cryptexes/OS/System", false, "the SystemOS volume by its cryptex path; not a union, or directory reads loop"));
    cx.push("addon_mounts", mount("/usr", "/private/preboot/Cryptexes/OS/usr", false, "as /System"));
    cx.say("applied", "App cryptex", &format!("{count} files in {ONEIOS_GUEST}/preboot/Cryptexes/App; shown at /private/preboot by the kernel (addon_mounts)"), None);
    Ok(())
}

fn staged_apps(cx: &mut Context) -> Result<()> {
    let staged = "private/var/staged_system_apps";
    let names: Vec<String> = cx.root.listdir(staged)?.into_iter().filter(|n| n.ends_with(".app")).collect();
    if !cx.rw {
        let c = dict(vec![
            ("source", "/private/var/staged_system_apps (the OS volume's, before Data covers /private/var)".into()),
            ("over", "/Applications".into()),
            ("how", "UniXNU owner-staged-apps=1; also drop LaunchServices' database at each boot, or the home screen shows Settings alone (ipados27-bringup.patch)".into()),
        ]);
        cx.contract.set("staged_apps", c);
        cx.say("contract", "staged apps", &format!("{} removable system apps stay in the OS volume; the kernel shows them in /Applications (staged_apps)", names.len()), None);
        return Ok(());
    }
    let mut moved = 0;
    for name in names {
        if cx.root.lexists(&format!("Applications/{name}"))? {
            continue;
        }
        let from = cx.root.host(&format!("{staged}/{name}"))?;
        let to = cx.root.host("Applications")?.join(&name);
        std::fs::rename(&from, &to).with_context(|| format!("{} -> {}", from.display(), to.display()))?;
        let target = format!("{staged}/{name}");
        let tweak = cx.tweak.clone();
        cx.m.record("root", &format!("Applications/{name}"), "moved", Change { target: Some(&target), ..Default::default() }, Some(&tweak));
        let placeholder = format!("System/Library/AppPlaceholders/{name}");
        if cx.root.isfile(&placeholder)? {
            cx.root.remove(&mut cx.m, &placeholder)?;
        }
        moved += 1;
    }
    cx.say(if moved > 0 { "applied" } else { "kept" }, "staged apps", &if moved > 0 { format!("{moved} moved to /Applications") } else { "none staged".into() }, None);
    Ok(())
}

fn kernel_features(cx: &mut Context) -> Result<()> {
    if cx.rw {
        return Ok(());
    }
    let features: [(&str, &str, &str); 11] = [
        ("launch-policy", "droidvm-disable-jobs / native_board_launch_policy", "write launch_policy.disable_jobs to disabled.plist as root before each launchd start"),
        ("service-manager", "owner-service-manager=1", "start launch_policy.service_manager and hand it launchd's bootstrap port"),
        ("no-launch-constraints", "sheng-no-lc=1 (zzyc-owner-skip-launch-constraints.patch)", "our binaries are in the owner trust cache but are not platform binaries"),
        ("owner-trust", "/chosen/memory-map/TrustCache with an owner module", "the CDHashes of owner-trust.json beside Apple's three trust caches"),
        ("addon-mounts", "sheng-gpu-addon=1, owner-app-cryptex=1 (bindfs, BINDM_FORCE_ROOT)", "the read-only views in addon_mounts, after Data is mounted"),
        ("staged-apps", "owner-staged-apps=1", "staged_apps"),
        ("data-protection", "owner-dataprotect=1", "a Data volume without content protection: class kept in an xattr; PosterBoard needs it for its wallpaper store"),
        ("key-store", "sheng-aks=1 sheng-aks-sls=3 offline-key-events=1 (AppleKeyStoreState, KeyStoreWrap.inc)", "no passcode; wrap and unwrap for the keychain; /private/var/keybags stays empty"),
        ("software-aes", "SoftwareAES (IOAESAccelerator)", "AppleImage4 waits for one; without it IOKit never goes quiet and configd waits 5 minutes"),
        ("gestalt-cache-drop", "native_gestalt_cache_drop", "remove the MobileGestalt cache at each boot so the identity always comes from the device tree"),
        ("text-fixups", "/chosen/text-fixups (native-text-fixup patch)", "for -[CADisplayMode preferredScale]: see display.scale_note"),
    ];
    let list = Value::List(features.iter().map(|(n, u, w)| dict(vec![("name", (*n).into()), ("unixnu", (*u).into()), ("what", (*w).into())])).collect());
    cx.contract.set("kernel_features", list);
    // Where the function is in this build's cache (runtime/xnuhost/src/config.rs SCALE_FIXUPS_3X): 24A446's text is
    // kept as it was; 24B156 has the same function at another offset.
    let at = match cx.build.as_str() {
        "24B156" => "-[CADisplayMode preferredScale] (QuartzCore, read in 24B156's dyld_shared_cache_arm64e.01 at 0x3f9d6f4: the same code as 24A446's at 0x40b1758)",
        _ => "-[CADisplayMode preferredScale] (QuartzCore, read in 24A446's dyld_shared_cache_arm64e.01 at 0x40b1758; the cache is the one the iPad has)",
    };
    let note = format!("{at} returns the mode's own preferred scale when it has one; else, in a virtual device, 2 or the mode's native scale field if that is more; else 2 for a mode taller than 1080 pixels and 1 otherwise. /chosen display-scale is not read there. So @2x is what it answers unchanged, and for @3x xnuhost adds two text fixups (runtime/xnuhost/src/config.rs scale_fixups) that make both answers 3. Not verified in a running guest: no frame of SpringBoard's has been rendered yet.");
    match cx.contract.get_mut("display") {
        Some(d @ Value::Dict(_)) => d.set("scale_note", note),
        _ => bail!("kernel-features: no display in the contract (identity runs first)"),
    }
    cx.say("contract", "kernel features", &format!("{} things the kernel does for a sealed root (kernel_features)", features.len()), None);
    Ok(())
}

// ---- preferences

/// `_fill`: @BUILD@ in strings, also in dictionaries; a list is left as it is.
fn fill(v: &toml::Value, build: &str) -> Result<P> {
    Ok(match v {
        toml::Value::String(s) => P::Str(s.replace("@BUILD@", build)),
        toml::Value::Table(t) => P::Dict(t.iter().map(|(k, v)| Ok((k.clone(), fill(v, build)?))).collect::<Result<_>>()?),
        other => crate::config::to_plist(other)?,
    })
}

fn preferences(cx: &mut Context) -> Result<()> {
    let mut off: BTreeSet<String> = cx.o.no_prefs.iter().cloned().collect();
    if !cx.o.lighter_visuals {
        off.insert("visuals".into());
    }
    struct File {
        owner: Owner,
        mode: u32,
        parent: Owner,
        values: Vec<(String, P)>,
        groups: BTreeSet<String>,
    }
    let mut by_file: BTreeMap<String, File> = BTreeMap::new();
    for p in &cx.prefs {
        if off.contains(&p.group) {
            continue;
        }
        let (path, owner, mode, parent) = match p.user.as_str() {
            "any" => (format!("preferences/{}.plist", p.domain), ROOT, 0o644, ROOT),
            "root" => (format!("root/Library/Preferences/{}.plist", p.domain), ROOT, 0o600, ROOT),
            _ => (format!("mobile/Library/Preferences/{}.plist", p.domain), MOBILE, 0o600, MOBILE),
        };
        let entry = by_file.entry(path).or_insert(File { owner, mode, parent, values: Vec::new(), groups: BTreeSet::new() });
        if let P::Dict(items) = fill(&p.values, &cx.build)? {
            for (k, v) in items {
                match entry.values.iter_mut().find(|(have, _)| *have == k) {
                    Some(slot) => slot.1 = v,
                    None => entry.values.push((k, v)),
                }
            }
        }
        entry.groups.insert(p.group.clone());
    }
    let mut groups: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for (path, entry) in &by_file {
        let changed = cx.data.merge_plist(&mut cx.m, path, &entry.values, entry.owner, entry.mode, true, Some(entry.parent))?;
        for group in &entry.groups {
            let g = groups.entry(group.clone()).or_default();
            g.0 += 1;
            g.1 += !changed.is_empty() as usize;
        }
    }
    let all: BTreeSet<String> = cx.prefs.iter().map(|p| p.group.clone()).collect();
    for group in all {
        if off.contains(&group) {
            let note = if group == "visuals" { "not asked for".to_string() } else { format!("--no-{group}") };
            cx.say("off", &format!("preferences: {group}"), &note, None);
        } else {
            let (files, changed) = groups.get(&group).copied().unwrap_or((0, 0));
            cx.say(if changed > 0 { "applied" } else { "kept" }, &format!("preferences: {group}"), &format!("{files} files"), None);
        }
    }
    Ok(())
}

// ---- Data volume layout

fn data_layout(cx: &mut Context) -> Result<()> {
    let mut made: Vec<String> = Vec::new();
    let mut need = |cx: &mut Context, path: &str, owner: Owner, mode: u32| -> Result<()> {
        if !cx.data.isdir(path)? {
            made.push(path.to_string());
        }
        cx.data.mkdir(&mut cx.m, path, owner, mode)
    };
    // containers: ios-sweet sweet-root-tweaks.py (owners), UniXNU make-data-seed.py (the same four)
    need(cx, "containers/Bundle", (33, 33), 0o755)?;
    need(cx, "containers/Data/System", ROOT, 0o755)?;
    need(cx, "containers/Shared/SystemGroup", ROOT, 0o755)?;
    need(cx, "containers/Temp", ROOT, 0o755)?;
    // UniXNU make-data-seed.py
    need(cx, "keybags", ROOT, 0o700)?;
    need(cx, "mobile/tmp", MOBILE, 0o755)?;
    need(cx, "tmp", ROOT, 0o1777)?;
    need(cx, "run", ROOT, 0o755)?;
    // the compressor's swap files (/private/var/vm/swapfileN): XNU never makes the folder (hosted 0057)
    need(cx, "vm", ROOT, 0o755)?;
    // logd stores with their markers, or logd fails with "Failed to migrate directory" (ios-sweet mkiosroot.sh)
    for (path, mode) in [("db/diagnostics", 0o751), ("db/uuidtext", 0o755), ("db/diagnostics/timesync", 0o755)] {
        need(cx, path, LOGD, mode)?;
        cx.data.write(&mut cx.m, &format!("{path}/.migrated"), b"", LOGD, 0o644, None)?;
    }
    let note = if made.is_empty() { "containers, keybags, tmp, vm, logd stores".to_string() } else { format!("made {}", made.join(", ")) };
    cx.say(if made.is_empty() { "kept" } else { "applied" }, "data layout", &note, None);

    // A MobileGestalt cache from another identity keeps its answers: none is carried.
    let cache = "containers/Shared/SystemGroup/systemgroup.com.apple.mobilegestaltcache/Library/Caches/com.apple.MobileGestalt.plist";
    let removed = if cx.data.lexists(cache)? { cx.data.remove(&mut cx.m, cache)? } else { false };
    let note = if removed { "a stale cache removed; it is rebuilt from the device tree" } else { "none in the seed; built from the device tree at the first boot" };
    cx.say(if removed { "applied" } else { "kept" }, "MobileGestalt cache", note, None);
    Ok(())
}

fn keybag(cx: &mut Context) -> Result<()> {
    if !cx.o.plain_keybag {
        cx.say("off", "systembag.kb", "not written: the key-store stand-in of the kernel owns the keybag (--plain-keybag writes ios-sweet's plain one)", None);
        return Ok(());
    }
    let body = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>KeyBagKeys</key><data>U1dFRVQ=</data></dict></plist>\n";
    cx.data.mkdir(&mut cx.m, "keybags", ROOT, 0o700)?;
    let changed = cx.data.write(&mut cx.m, "keybags/systembag.kb", body, ROOT, 0o600, None)?;
    cx.push("notes", "plain keybag written: the device tree needs /defaults/no-effaceable-storage = 1 (ios-sweet tools/mkadt.py)".into());
    cx.say(if changed { "applied" } else { "kept" }, "systembag.kb", "plain property-list keybag, root 0600", None);
    Ok(())
}

/// Feature flags turned off on the Data volume in both layouts (the kernel writes the same into an older store:
/// hosted 0048, oneios-feature-off). CoreLocation's automatic cohorts crash-looped locationd.
const DATA_FLAGS_OFF: [(&str, &[&str]); 1] = [("CoreLocation", &["CLAutoCohort"])];

fn feature_flags(cx: &mut Context) -> Result<()> {
    let off = || P::Dict(vec![("Enabled".to_string(), P::Bool(false))]);
    for (domain, flags) in DATA_FLAGS_OFF {
        let values: Vec<(String, P)> = flags.iter().map(|f| (f.to_string(), off())).collect();
        let changed = cx.data.merge_plist(&mut cx.m, &format!("preferences/FeatureFlags/Domain/{domain}.plist"), &values, ROOT, 0o644, false, None)?;
        let note = flags.iter().map(|f| format!("{f} off")).collect::<Vec<_>>().join(", ");
        cx.say(if changed.is_empty() { "kept" } else { "applied" }, &format!("{domain} flags"), &note, None);
    }
    if !cx.rw {
        cx.say("kept", "ExtensionKit flag", "sealed root: the extension cache is Apple's and its signature holds", None);
        return Ok(());
    }
    let flag = vec![("enforce_xpc_cache_code_signing".to_string(), off())];
    let mut changed = !cx.data.merge_plist(&mut cx.m, "preferences/FeatureFlags/Domain/ExtensionKit.plist", &flag, ROOT, 0o644, false, None)?.is_empty();
    let system = "System/Library/FeatureFlags/Domain/ExtensionKit.plist";
    if cx.root.exists(system)? {
        changed = !cx.root.merge_plist(&mut cx.m, system, &flag, ROOT, 0o644, true, None)?.is_empty() || changed;
    }
    cx.say(if changed { "applied" } else { "kept" }, "ExtensionKit flag", "enforce_xpc_cache_code_signing off", None);
    Ok(())
}

// ---- identity and display: the booter's part

fn identity(cx: &mut Context) -> Result<()> {
    let d = cx.device.clone();
    let get = |t: &str| d.get(t).cloned().unwrap_or(Value::Null);
    let (device, adt, display) = (get("device"), get("adt"), get("display"));
    cx.contract.set("device", device.clone());
    let a = |k: &str| adt.get(k).cloned();
    let identity_tree = format!("the IPSW's DeviceTree for {}: /product is copied from it (see the device profile)", str_value(device.get("board").unwrap_or(&Value::Null)));
    let adt_c = dict(vec![
        ("root", a("root").ok_or_else(|| Refused("the device profile has no [adt.root]".into()))?),
        ("chosen", a("chosen").unwrap_or(Value::Null)),
        ("product", a("product").unwrap_or_else(Value::dict)),
        ("buttons", a("buttons").unwrap_or_else(Value::dict)),
        ("identity_tree", identity_tree.into()),
        ("required", a("required").unwrap_or_else(Value::dict)),
        ("absent", a("absent").unwrap_or_else(Value::dict)),
    ]);
    cx.contract.set("adt", adt_c);
    cx.contract.set("display", display.clone());
    let s = |v: &Value, k: &str| str_value(v.get(k).unwrap_or(&Value::Null));
    let note = format!(
        "{} ({}), device class {}: from the device tree the booter builds (adt); nothing in the root holds it",
        s(&device, "product"),
        s(&device, "board"),
        s(&device, "device_class")
    );
    cx.say("contract", "identity", &note, None);
    let points = display.get("points").and_then(Value::as_list).unwrap_or(&[]).to_vec();
    let p = |i: usize| points.get(i).map(str_value).unwrap_or_default();
    let note = format!(
        "{}x{} @{}x ({}x{} points) {}, {} Hz: the framebuffer the booter presents (display)",
        s(&display, "width"),
        s(&display, "height"),
        s(&display, "scale"),
        p(0),
        p(1),
        s(&display, "orientation"),
        s(&display, "refresh_hz")
    );
    cx.say("contract", "display", &note, None);
    let target = s(&device, "target_type");
    let jetsam = format!("{}/com.apple.jetsamproperties.{target}.plist", launchd::DAEMONS_DIR);
    if cx.root.exists(&jetsam)? {
        cx.say("kept", "target-type", &format!("{target}: the root has its jetsam properties"), None);
    } else {
        let prefix = "com.apple.jetsamproperties.";
        let names: Vec<String> = cx
            .root
            .listdir(launchd::DAEMONS_DIR)?
            .into_iter()
            .filter(|n| n.starts_with(prefix))
            .map(|n| {
                // n[len(prefix):-len('.plist')]
                let chars: Vec<char> = n.chars().collect();
                let (a, b) = (prefix.len(), chars.len().saturating_sub(6));
                if a < b { chars[a..b].iter().collect() } else { String::new() }
            })
            .collect();
        let list = if names.is_empty() { "none".to_string() } else { names.join(", ") };
        cx.say("not-found", "target-type", &format!("no jetsam properties for {target} in this root (it has: {list}): check target_type in the device profile against the IPSW"), None);
    }
    Ok(())
}

/// Two plists of Python's `==`, for the tests.
pub fn same(a: &P, b: &P) -> bool {
    py_eq(a, b)
}
