//! userland/tests/test_roottweak.py's 38 cases, on the Rust port, on a generated stand-in root (fakeroot.py's,
//! ported below). No file of Apple's is used. Each test names the Python case it ports.

use oneios_common::json::{self, Value};
use oneios_tweaks::plist::{self, P};
use oneios_tweaks::tree::{Manifest, Tree};
use oneios_tweaks::{bootcheck, cli, config, hook, launchd, macho};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

const BUILD: &str = "24A999"; // not a real build number
const TARGET: &str = "V54";
const REPORT: &str = "oneios/roottweak-report.txt";

// ---------------------------------------------------------------- fakeroot.py

fn jobs() -> Vec<(&'static str, Vec<(&'static str, P)>)> {
    let t = || P::Bool(true);
    vec![
        ("com.apple.SpringBoard", vec![("_PanicOnCrash", t())]),
        ("com.apple.backboardd", vec![("_PanicOnCrash", t()), ("EnvironmentVariables", P::Dict(vec![("CA_EXAMPLE".into(), P::Str("1".into()))]))]),
        ("com.apple.runningboardd", vec![]),
        ("com.apple.CommCenter", vec![]),
        ("com.apple.CommCenterMobileHelper", vec![]),
        ("com.apple.CommCenterRootHelper", vec![]),
        ("com.apple.sleepd", vec![]),
        ("com.apple.cloudd", vec![]),
        ("com.apple.locationd", vec![]),
        ("com.apple.mobileactivationd", vec![]),
        ("com.apple.watchdogd", vec![]),
        ("com.apple.analyticsd", vec![]),
        ("com.apple.OTATaskingAgent", vec![]),
        ("com.apple.mobile.softwareupdated", vec![]),
        ("com.apple.nfcd", vec![]),
        ("com.apple.seld", vec![]),
        ("com.apple.assistantd", vec![]),
        ("com.apple.momentsd", vec![]),
        ("com.apple.coreduetd", vec![]),
        ("com.apple.generativeexperiencesd", vec![]),
        ("com.apple.biometrickitd", vec![]),
        ("com.apple.nearbyd", vec![]),
        ("com.apple.example.unknownd", vec![]),
    ]
}
const FORCED: [&str; 5] = ["com.apple.analyticsd", "com.apple.OTATaskingAgent", "com.apple.mobile.softwareupdated", "com.apple.mobileactivationd", "com.apple.logd"];

fn put(path: &Path, data: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, data).unwrap();
}

fn d(pairs: Vec<(&str, P)>) -> P {
    P::Dict(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}
fn s(text: &str) -> P {
    P::Str(text.into())
}

fn version_plist(build: &str, version: &str) -> Vec<u8> {
    plist::dumps_xml(&d(vec![("ProductBuildVersion", s(build)), ("ProductName", s("iPhone OS")), ("ProductVersion", s(version))])).unwrap()
}

/// fakeroot.make(top): the OS volume; its Data volume is top/private/var.
fn make_root(top: &Path) {
    for dir in ["System/Library/LaunchDaemons", "System/Library/Extensions", "System/Library/xpc", "System/Library/CoreServices", "sbin", "usr/libexec", "private/var/mobile/Library/Preferences", "private/var/root", "private/var/db", "private/preboot"] {
        fs::create_dir_all(top.join(dir)).unwrap();
    }
    std::os::unix::fs::symlink("private/var", top.join("var")).unwrap();
    put(&top.join("System/Library/CoreServices/SystemVersion.plist"), &version_plist(BUILD, "27.0.1"));
    let mut daemons = P::dict();
    for (label, extra) in jobs() {
        let short = label.rsplit('.').next().unwrap();
        let mut job = d(vec![("Label", s(label)), ("ProgramArguments", P::Array(vec![s(&format!("/usr/libexec/{short}"))]))]);
        for (k, v) in extra {
            job.set(k, v);
        }
        put(&top.join(format!("System/Library/LaunchDaemons/{label}.plist")), &plist::dumps_xml(&job).unwrap());
        daemons.set(&format!("/System/Library/LaunchDaemons/{label}.plist"), job);
    }
    let cache = d(vec![("LaunchDaemons", daemons), ("Boot", d(vec![("/System/Library/xpc/boot.plist", d(vec![("Label", s("com.apple.example.boot"))]))]))]);
    put(&top.join("System/Library/xpc/launchd.plist"), &plist::dumps_binary(&cache).unwrap());
    put(&top.join("sbin/launchd"), b"not a real launchd\n");
    let mut over = P::dict();
    for label in FORCED {
        over.set(label, d(vec![("DisableAllowed", P::Bool(false)), ("ActiveSoftMemoryLimit", P::Int(10))]));
    }
    for label in ["com.apple.backboardd", "com.apple.SpringBoard", "com.apple.runningboardd", "com.apple.configd"] {
        over.set(label, d(vec![("RecoverOnCrash", P::Bool(true)), ("ActiveSoftMemoryLimit", P::Int(100))]));
    }
    let jetsam = d(vec![(
        "Version4",
        d(vec![("Daemon", d(vec![("Override", over), ("Default", P::dict())])), ("XPCService", d(vec![("Override", d(vec![("com.apple.example.xpc", d(vec![("DisableAllowed", P::Bool(false))]))]))]))]),
    )]);
    put(&top.join(format!("System/Library/LaunchDaemons/com.apple.jetsamproperties.{TARGET}.plist")), &plist::dumps_binary(&jetsam).unwrap());
}

/// fakeroot.signed_macho: a minimal arm64 Mach-O with an embedded signature and one SHA-256 code directory.
fn signed_macho(identifier: &str, arm64e: bool, page_shift: u8, payload: &[u8]) -> (Vec<u8>, String) {
    let mut ident = identifier.as_bytes().to_vec();
    ident.push(0);
    let hash_offset = 44 + ident.len() as u32;
    let code_hash = Sha256::digest(payload).to_vec();
    let mut fields = Vec::new();
    for v in [0x20400u32, 0x2, hash_offset, 44, 0, 1, payload.len() as u32] {
        fields.extend_from_slice(&v.to_be_bytes());
    }
    fields.extend_from_slice(&[32, 2, 0, page_shift]);
    fields.extend_from_slice(&0u32.to_be_bytes());
    let mut directory = Vec::new();
    directory.extend_from_slice(&0xFADE0C02u32.to_be_bytes());
    directory.extend_from_slice(&((8 + fields.len() + ident.len() + code_hash.len()) as u32).to_be_bytes());
    directory.extend_from_slice(&fields);
    directory.extend_from_slice(&ident);
    directory.extend_from_slice(&code_hash);
    let mut superblob = Vec::new();
    for v in [0xFADE0CC0u32, 20 + directory.len() as u32, 1, 0, 20] {
        superblob.extend_from_slice(&v.to_be_bytes());
    }
    superblob.extend_from_slice(&directory);
    let mut build = Vec::new();
    for v in [0x32u32, 24, 2, 27 << 16, 27 << 16, 0] {
        build.extend_from_slice(&v.to_le_bytes());
    }
    let commands_size = build.len() as u32 + 16;
    let signature_offset = 32 + commands_size + payload.len() as u32;
    let mut data = Vec::new();
    let subtype = if arm64e { 0x8000_0002u32 } else { 0 };
    for v in [0xFEEDFACFu32, 0x0100000C, subtype, 2, 2, commands_size, 0, 0] {
        data.extend_from_slice(&v.to_le_bytes());
    }
    data.extend_from_slice(&build);
    for v in [0x1Du32, 16, signature_offset, superblob.len() as u32] {
        data.extend_from_slice(&v.to_le_bytes());
    }
    data.extend_from_slice(payload);
    data.extend_from_slice(&superblob);
    let cdhash = Sha256::digest(&directory)[..20].iter().map(|b| format!("{b:02x}")).collect();
    (data, cdhash)
}

fn signed(identifier: &str) -> (Vec<u8>, String) {
    signed_macho(identifier, true, 12, b"code")
}

// ---------------------------------------------------------------- the test case (test_roottweak.py's Case)

struct Case {
    top: PathBuf,
    root: PathBuf,
    data: PathBuf,
    out: PathBuf,
    log: String,
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.top);
    }
}

fn tmpdir(name: &str) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let base = std::env::temp_dir().canonicalize().unwrap();
    let dir = base.join(format!("oneios-tweaks-test-{}-{}-{name}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn as_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

impl Case {
    fn new(name: &str) -> Case {
        let top = tmpdir(name);
        let root = top.join("root");
        make_root(&root);
        let data = top.join("data");
        fs::create_dir(&data).unwrap();
        Case { out: top.join("out"), top, root, data, log: String::new() }
    }

    fn helpers_named(&self, names: &[&str], arm64e: bool, page_shift: u8) -> (PathBuf, BTreeMap<String, String>) {
        let dir = self.top.join("helpers");
        fs::create_dir_all(&dir).unwrap();
        let mut hashes = BTreeMap::new();
        for name in names {
            let (data, cdhash) = signed_macho(&format!("com.oneios.{name}"), arm64e, page_shift, b"code");
            fs::write(dir.join(name), data).unwrap();
            fs::set_permissions(dir.join(name), fs::Permissions::from_mode(0o755)).unwrap();
            hashes.insert(name.to_string(), cdhash);
        }
        (dir, hashes)
    }
    fn helpers(&self) -> (PathBuf, BTreeMap<String, String>) {
        self.helpers_named(&["oneios-init", "oneios-bootstat", "oneios-mg", "dropbear"], true, 12)
    }

    /// `apply`, as cli.main(argv); the status (0, or 1 with the error in `log`).
    fn apply_in(&mut self, layout: &str, args: &[&str]) -> i32 {
        let mut argv: Vec<String> = ["--root", &self.root.to_string_lossy(), "--out", &self.out.to_string_lossy(), "--layout", layout].iter().map(|s| s.to_string()).collect();
        if layout == "sealed" {
            argv.extend(["--data".to_string(), self.data.to_string_lossy().into_owned()]);
        }
        argv.extend(args.iter().map(|s| s.to_string()));
        let result = cli::parse_apply(&argv).and_then(|a| oneios_tweaks::apply(&a).map(|_| ()));
        match result {
            Ok(()) => {
                self.log = String::new();
                0
            }
            Err(e) => {
                self.log = format!("{e:#}");
                1
            }
        }
    }
    fn apply(&mut self, args: &[&str]) {
        let status = self.apply_in("sealed", args);
        assert_eq!(status, 0, "{}", self.log);
    }
    fn apply_fails(&mut self, args: &[&str]) {
        assert_eq!(self.apply_in("sealed", args), 1, "apply {args:?} did not fail");
    }
    fn read(&self, name: &str) -> Value {
        json::load(self.out.join(name)).unwrap()
    }
    fn states(&self) -> HashMap<String, String> {
        let r = self.read("report.json");
        r.get("results").unwrap().as_list().unwrap().iter().map(|r| (str_of(r, "item"), str_of(r, "state"))).collect()
    }
    fn entry(&self, volume: &str, path: &str) -> Value {
        let m = self.read("tweaks.manifest.json");
        let found: Vec<Value> = m.get("entries").unwrap().as_list().unwrap().iter().filter(|e| str_of(e, "volume") == volume && str_of(e, "path") == path).cloned().collect();
        assert_eq!(found.len(), 1, "{volume}:{path}");
        found.into_iter().next().unwrap()
    }
    fn contract(&self) -> Value {
        self.read("boot-contract.json")
    }
}

fn str_of(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}
fn int_of(v: &Value, k: &str) -> i128 {
    v.get(k).and_then(Value::as_int).unwrap()
}
fn strs(v: &Value) -> Vec<String> {
    v.as_list().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect()
}

fn read_plist(base: &Path, path: &str) -> P {
    plist::loads(&fs::read(base.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))).unwrap()
}

fn keys(p: &P) -> Vec<String> {
    let mut k: Vec<String> = p.items().iter().map(|(k, _)| k.clone()).collect();
    k.sort();
    k
}

/// Every path of a tree with its type, link target or content hash; the run's own report left out.
fn digest(top: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    fn walk(top: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(top).unwrap().to_string_lossy().into_owned();
            if rel.ends_with(REPORT) {
                continue;
            }
            let meta = fs::symlink_metadata(&p).unwrap();
            if meta.file_type().is_symlink() {
                out.insert(rel, format!("link {}", fs::read_link(&p).unwrap().display()));
            } else if meta.is_dir() {
                out.insert(rel, "dir".into());
                walk(top, &p, out);
            } else {
                out.insert(rel, format!("{:x}", Sha256::digest(fs::read(&p).unwrap())));
            }
        }
    }
    walk(top, top, &mut out);
    out
}

fn mode_of(p: &Path) -> u32 {
    fs::metadata(p).unwrap().permissions().mode() & 0o7777
}

// ---------------------------------------------------------------- Policy

#[test]
fn policy_the_policy_does_not_contradict_itself() {
    let d = config::daemons(None).unwrap();
    let chosen: Vec<String> = d.selected(&[], &[], &[]).unwrap().into_iter().map(|(l, _)| l).collect();
    assert!(!chosen.iter().any(|l| d.keep.contains(l)));
    assert!(!chosen.iter().any(|l| d.idle.iter().any(|(i, _)| i == l)));
    // vphone's base list of 134, less generativeexperiencesd (kept) and the four OTA jobs (replace)
    // analyticsd (2026-11-12), navd (2026-11-22) and duetexpertd (2026-11-23) went to keep; 16 to telemetry (2026-11-23), with
    // osanalyticshelper and its second job, osanalyticshelper.agent (0094)
    assert_eq!(d.group("base").unwrap().labels.len(), 110);
    assert_eq!(d.group("telemetry").unwrap().labels.len(), 18);
    for label in ["com.apple.SpringBoard", "com.apple.backboardd", "com.apple.CommCenter", "com.apple.sleepd", "com.apple.generativeexperiencesd", "com.apple.bluetoothd", "com.apple.locationd", "com.apple.navd", "com.apple.duetexpertd"] {
        assert!(d.keep.contains(label), "{label}");
    }
    // on by default: replace, hardware (aned) and the crash loops; base, extra and signin run since 2026-11-23
    for label in ["com.apple.watchdogd", "com.apple.mobile.softwareupdated", "com.apple.aned", "com.apple.aonsensed", "com.apple.prngseedd", "com.apple.osanalytics.osanalyticshelper"] {
        assert!(chosen.iter().any(|l| l == label), "{label}");
    }
    for label in ["com.apple.nfcd", "com.apple.seld", "com.apple.momentsd", "com.apple.coreduetd", "com.apple.fairplayd.H2", "com.apple.identityservicesd", "com.apple.weatherd", "com.apple.followupd"] {
        assert!(!chosen.iter().any(|l| l == label), "{label}");
    }
    // what costs a feature is off unless asked for
    for label in ["com.apple.apsd", "com.apple.chronod", "com.apple.appstored"] {
        assert!(!chosen.iter().any(|l| l == label), "{label}");
    }
    let has = |sel: Vec<(String, String)>, l: &str| sel.iter().any(|(x, _)| x == l);
    assert!(has(d.selected(&["fewest".into()], &[], &[]).unwrap(), "com.apple.apsd"));
    assert!(has(d.selected(&["base".into()], &[], &[]).unwrap(), "com.apple.nfcd"));
    assert!(!has(d.selected(&["base".into()], &[], &["com.apple.weatherd".into()]).unwrap(), "com.apple.weatherd"));
    assert!(d.selected(&["nonesuch".into()], &[], &[]).is_err());
}

/// A patch of the hosted series, as text.
fn hosted_patch(name: &str) -> String {
    std::fs::read_to_string(format!("{}/../../../../kernel/patches/oneios-hosted/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// The labels of a C string list `NAME[] =` ... `;` as a patch adds it ("+" lines).
fn patch_list(patch: &str, name: &str) -> Vec<String> {
    let start = patch.find(&format!("{name}[] =")).unwrap_or_else(|| panic!("the list {name}"));
    let body = &patch[start..start + patch[start..].find("\n+;").expect("its end")];
    let joined: String = body.lines().skip(1).filter_map(|l| l.split('"').nth(1)).collect();
    joined.split(',').map(str::to_string).collect()
}

#[test]
fn policy_the_kernels_enable_list_is_what_the_policy_runs_again() {
    // hosted 0092: a Data store from a seed made before 2026-11-23 has the groups base, extra and signin and
    // fairplayd.H2 in its disabled.plist; the kernel's built-in list enables them again. It must be the policy's.
    // hosted 0093 takes the telemetry group's labels out of it again (its "-" lines).
    let removed: Vec<String> = hosted_patch("0093-telemetry-jobs-off.patch")
        .lines()
        .filter(|l| l.starts_with("-\t\"com.apple."))
        .filter_map(|l| l.split('"').nth(1).map(|s| s.trim_end_matches(',').to_string()))
        .collect();
    let mut kernel: Vec<String> =
        patch_list(&hosted_patch("0092-enable-jobs-policy.patch"), "oneios_jobs_enable_default").into_iter().filter(|l| !removed.contains(l)).collect();
    kernel.sort();
    let d = config::daemons(None).unwrap();
    let telemetry = d.group("telemetry").unwrap();
    assert!(telemetry.default, "telemetry is on by default");
    let mut gone = removed.clone();
    gone.sort();
    let mut want_gone: Vec<String> =
        telemetry.labels.iter().filter(|l| !l.starts_with("com.apple.osanalytics.osanalyticshelper")).cloned().collect();
    want_gone.sort();
    assert_eq!(gone, want_gone, "0093 takes out of 0092's list the telemetry jobs it had (osanalyticshelper and its agent job were never in it)");
    let mut want: Vec<String> = ["base", "extra", "signin"].iter().flat_map(|g| d.group(g).unwrap().labels.clone()).collect();
    for g in ["base", "extra", "signin"] {
        assert!(!d.group(g).unwrap().default, "{g} is off by default");
    }
    // kept since the seeds that disabled them: analyticsd (0044), navd (0083), duetexpertd (0092); fairplayd.H2 left hardware
    for label in ["com.apple.analyticsd", "com.apple.navd", "com.apple.duetexpertd", "com.apple.fairplayd.H2"] {
        want.push(label.to_string());
    }
    want.sort();
    assert_eq!(kernel, want);
    let chosen: Vec<String> = d.selected(&[], &[], &[]).unwrap().into_iter().map(|(l, _)| l).collect();
    assert!(!kernel.iter().any(|l| chosen.contains(l)), "the kernel enables a job the policy disables");
    assert!(kernel.join(",").len() < 4096, "0092's buffer is 4096 bytes");
}

#[test]
fn policy_the_kernels_disable_list_is_the_loops_and_the_telemetry() {
    // hosted 0053 and 0093: a Data store from an older seed gets the policy's crash loops and telemetry disabled. 0094 adds
    // a label to the list (its "+" lines inside it).
    let mut kernel = patch_list(&hosted_patch("0093-telemetry-jobs-off.patch"), "oneios_jobs_disable_default");
    kernel.extend(
        hosted_patch("0094-osanalytics-agent-off.patch")
            .lines()
            .filter(|l| l.starts_with("+\t\"com.apple."))
            .filter_map(|l| l.split('"').nth(1).map(|s| s.trim_end_matches(',').to_string())),
    );
    assert!(kernel.join(",").len() < 1024, "0093's buffer is 1024 bytes");
    kernel.sort();
    let d = config::daemons(None).unwrap();
    let mut want: Vec<String> = ["loops", "telemetry"].iter().flat_map(|g| d.group(g).unwrap().labels.clone()).collect();
    want.sort();
    assert_eq!(kernel, want);
    let chosen: Vec<String> = d.selected(&[], &[], &[]).unwrap().into_iter().map(|(l, _)| l).collect();
    assert!(kernel.iter().all(|l| chosen.contains(l)), "the kernel disables a job the policy runs");
    assert!(!kernel.iter().any(|l| d.keep.contains(l)), "the kernel disables a kept job");
}

#[test]
fn policy_a_policy_that_disables_what_it_keeps_is_refused() {
    let raw: toml::Table = "[keep]\nlabels = ['a']\n[replace.labels]\n[group.g]\nlabels = ['a']\nsource = 's'\n".parse().unwrap();
    assert!(config::Daemons::from_toml(&raw).is_err());
}

#[test]
fn policy_the_device_is_an_iphone_at_3x_in_portrait() {
    let d = config::device(None, None).unwrap();
    let g = |t: &str, k: &str| d.get(t).unwrap().get(k).cloned().unwrap();
    assert_eq!((str_of(d.get("device").unwrap(), "product"), str_of(d.get("device").unwrap(), "device_class")), ("iPhone18,2".into(), "iPhone".into()));
    assert_eq!((g("display", "scale"), str_of(d.get("display").unwrap(), "orientation")), (Value::Int(3), "portrait".into()));
    let disp = d.get("display").unwrap();
    assert_eq!((int_of(disp, "width"), int_of(disp, "height"), int_of(disp, "refresh_hz")), (1440, 3120, 120));
    assert_eq!(g("display", "points"), Value::List(vec![Value::Int(480), Value::Int(1040)]));
    let adt = d.get("adt").unwrap();
    assert_eq!((int_of(adt.get("chosen").unwrap(), "display-scale"), int_of(adt.get("product").unwrap(), "artwork-scale-factor")), (3, 3));
    assert_eq!(int_of(adt.get("product").unwrap(), "artwork-device-subtype"), 2868);
}

#[test]
fn policy_the_ui_scale_is_one_value_everywhere() {
    let d = config::device(None, Some(2)).unwrap();
    let disp = d.get("display").unwrap();
    assert_eq!((int_of(disp, "width"), int_of(disp, "height"), int_of(disp, "scale")), (1440, 3120, 2));
    assert_eq!(disp.get("points").cloned().unwrap(), Value::List(vec![Value::Int(720), Value::Int(1560)]));
    let adt = d.get("adt").unwrap();
    assert_eq!((int_of(adt.get("chosen").unwrap(), "display-scale"), int_of(adt.get("product").unwrap(), "artwork-scale-factor")), (2, 2));
    let expect = d.get("mobilegestalt").unwrap().get("expect").unwrap();
    assert_eq!((str_of(expect, "main-screen-width"), str_of(expect, "main-screen-height"), str_of(expect, "main-screen-scale")), ("1440".into(), "3120".into(), "2".into()));
    assert_eq!(str_of(config::device(None, Some(3)).unwrap().get("mobilegestalt").unwrap().get("expect").unwrap(), "main-screen-scale"), "3");
    assert!(config::device(None, Some(4)).is_err());
    assert_eq!(str_of(adt.get("product").unwrap(), "artwork-device-idiom"), "phone");
    assert!(adt.get("absent").unwrap().get("baseband").is_some());
    assert_eq!(str_of(expect, "DeviceClass"), "iPhone");
}

#[test]
fn policy_every_preference_names_its_source() {
    let prefs = config::prefs(None).unwrap();
    assert!(prefs.iter().all(|p| !p.source.is_empty()));
    let groups: std::collections::BTreeSet<&str> = prefs.iter().map(|p| p.group.as_str()).collect();
    assert_eq!(groups.into_iter().collect::<Vec<_>>(), ["awake", "coreanimation", "dictation", "migration", "setup", "visuals", "watchdogs"]);
}

// ---------------------------------------------------------------- Sealed

#[test]
fn sealed_nothing_is_written_to_the_os_volume() {
    let mut c = Case::new("nothing-written");
    let before = digest(&c.root);
    let (helpers, _) = c.helpers();
    c.apply(&["--helpers", &helpers.to_string_lossy()]);
    assert_eq!(digest(&c.root), before);
    let m = c.read("tweaks.manifest.json");
    assert!(!m.get("entries").unwrap().as_list().unwrap().iter().any(|e| str_of(e, "volume") == "root"));
}

#[test]
fn sealed_disabled_jobs_go_to_the_data_volume_as_roots_file() {
    let mut c = Case::new("disabled");
    c.apply(&[]);
    let disabled = read_plist(&c.data, "db/com.apple.xpc.launchd/disabled.plist");
    // the fake root's jobs that the default disables: the replace jobs (base and extra run since 2026-11-23)
    let mut want = vec!["com.apple.watchdogd", "com.apple.mobile.softwareupdated", "com.apple.OTATaskingAgent"];
    want.sort();
    assert_eq!(keys(&disabled), want);
    assert!(disabled.items().iter().all(|(_, v)| matches!(v, P::Bool(true))));
    // launchd ignores the file unless it is root's, 0644, with the marker beside it
    let e = c.entry("data", "db/com.apple.xpc.launchd/disabled.plist");
    assert_eq!((int_of(&e, "uid"), int_of(&e, "gid"), str_of(&e, "mode")), (0, 0, "0644".into()));
    assert!(c.data.join("db/com.apple.xpc.launchd/disabled.migrated").is_file());
    let contract = c.contract();
    let lp = contract.get("launch_policy").unwrap();
    let mut jobs = strs(lp.get("disable_jobs").unwrap());
    jobs.sort();
    assert_eq!(jobs, keys(&disabled));
    // iOS 27's DisableAllowed = false: said, since a sealed root cannot change it (the kernel answers it: hosted 0098)
    assert_eq!(strs(lp.get("force_enabled").unwrap()), ["com.apple.OTATaskingAgent", "com.apple.mobile.softwareupdated"]);
    // labels the fake root does not have are reported, not written
    let r = c.read("report.json");
    let missing = r.get("results").unwrap().as_list().unwrap().iter().find(|r| str_of(r, "item") == "jobs not in this root").unwrap().clone();
    assert!(strs(missing.get("labels").unwrap()).contains(&"com.apple.aonsensed".to_string()));
    assert!(disabled.get("com.apple.aonsensed").is_none());
    // nothing kept is disabled, and a job the policy does not know stays
    for label in ["com.apple.SpringBoard", "com.apple.CommCenter", "com.apple.sleepd", "com.apple.example.unknownd", "com.apple.generativeexperiencesd", "com.apple.biometrickitd", "com.apple.nfcd", "com.apple.coreduetd"] {
        assert!(disabled.get(label).is_none(), "{label}");
    }
}

#[test]
fn sealed_an_override_already_there_stays() {
    let mut c = Case::new("override-stays");
    put(&c.data.join("db/com.apple.xpc.launchd/disabled.plist"), &plist::dumps_xml(&d(vec![("com.example.theirs", P::Bool(true)), ("com.example.on", P::Bool(false))])).unwrap());
    c.apply(&[]);
    let disabled = read_plist(&c.data, "db/com.apple.xpc.launchd/disabled.plist");
    assert!(matches!(disabled.get("com.example.theirs"), Some(P::Bool(true))));
    assert!(matches!(disabled.get("com.example.on"), Some(P::Bool(false))));
    assert!(matches!(disabled.get("com.apple.watchdogd"), Some(P::Bool(true))));
}

#[test]
fn sealed_groups_keep_and_disable_by_hand() {
    let mut c = Case::new("groups");
    c.apply(&["--group", "extra", "--no-group", "replace", "--keep", "com.apple.momentsd", "--disable", "com.apple.CommCenter", "--disable", "com.apple.example.unknownd"]);
    let disabled = read_plist(&c.data, "db/com.apple.xpc.launchd/disabled.plist");
    assert!(disabled.get("com.apple.watchdogd").is_none()); // --no-group replace
    assert!(disabled.get("com.apple.nfcd").is_none()); // base is off by default
    assert!(disabled.get("com.apple.coreduetd").is_some()); // --group extra
    assert!(disabled.get("com.apple.momentsd").is_none()); // --keep wins over the group
    assert!(disabled.get("com.apple.CommCenter").is_some());
    assert!(disabled.get("com.apple.example.unknownd").is_some());
    assert!(c.states().contains_key("warning: com.apple.CommCenter"));
    c.apply_fails(&["--group", "nonesuch"]);
}

#[test]
fn sealed_preferences_are_seeded_with_owners_and_the_build() {
    let mut c = Case::new("prefs");
    c.apply(&[]);
    let prefs = "mobile/Library/Preferences/";
    let pb = read_plist(&c.data, &format!("{prefs}com.apple.purplebuddy.plist"));
    assert!(plist::py_eq(&pb, &d(vec![("SetupDone", P::Bool(true)), ("SetupFinishedAllSteps", P::Bool(true)), ("SetupVersion", P::Int(11)), ("daemonizeNeedsToRun", P::Bool(false))])));
    let migration = read_plist(&c.data, &format!("{prefs}com.apple.migration.plist"));
    assert_eq!(migration.get("LastSystemVersion").and_then(P::as_str), Some(BUILD));
    assert!(plist::py_eq(migration.get("DMLastMigrationResults").unwrap(), &d(vec![("buildVersion", s(BUILD)), ("success", P::Bool(true))])));
    let sb = read_plist(&c.data, &format!("{prefs}com.apple.springboard.plist"));
    assert!(matches!(sb.get("FBLaunchWatchdogScale"), Some(P::Real(f)) if *f == 4.0));
    assert!(matches!(sb.get("BKNoWatchdogs"), Some(P::Bool(true))));
    assert!(matches!(sb.get("SBMinimumLockscreenIdleTime"), Some(P::Int(2147483647))));
    assert!(plist::py_eq(sb.get("FBLaunchWatchdogExceptions").unwrap(), &d(vec![("com.apple.purplebuddy", P::Real(100.0))])));
    for path in [format!("{prefs}com.apple.backboardd.plist"), format!("{prefs}.GlobalPreferences.plist"), "root/Library/Preferences/com.apple.backboardd.plist".into(), "preferences/.GlobalPreferences.plist".into()] {
        assert!(matches!(read_plist(&c.data, &path).get("CAEnableDeepFramebuffer"), Some(P::Bool(false))), "{path}");
    }
    assert!(matches!(read_plist(&c.data, &format!("{prefs}com.apple.coreanimation.plist")).get("CAEnableMedina"), Some(P::Bool(false))));
    // the offline dictation status: an empty dictionary, so AssistantServices caches it (hosted 0083 for older stores)
    let assistant = read_plist(&c.data, &format!("{prefs}com.apple.assistant.support.plist"));
    assert!(plist::py_eq(assistant.get("Offline Dictation Status").unwrap(), &d(vec![])), "{assistant:?}");
    let e = c.entry("data", &format!("{prefs}com.apple.assistant.support.plist"));
    assert_eq!((int_of(&e, "uid"), int_of(&e, "gid"), str_of(&e, "mode")), (501, 501, "0600".into()));
    let e = c.entry("data", &format!("{prefs}com.apple.springboard.plist"));
    assert_eq!((int_of(&e, "uid"), int_of(&e, "gid"), str_of(&e, "mode")), (501, 501, "0600".into()));
    assert_eq!(int_of(&c.entry("data", "mobile/Library/Preferences"), "uid"), 501);
    assert_eq!(int_of(&c.entry("data", "preferences/.GlobalPreferences.plist"), "uid"), 0);
    assert!(fs::read(c.data.join(format!("{prefs}com.apple.springboard.plist"))).unwrap().starts_with(b"bplist00"));
    // Reduce Motion only when asked
    assert!(!c.data.join(format!("{prefs}com.apple.Accessibility.plist")).exists());
    c.apply(&["--lighter-visuals", "--no-awake"]);
    assert!(matches!(read_plist(&c.data, &format!("{prefs}com.apple.Accessibility.plist")).get("ReduceMotionEnabled"), Some(P::Bool(true))));
    assert_eq!(c.states()["preferences: awake"], "off");
}

#[test]
fn sealed_keys_of_somebody_else_stay_and_a_broken_file_is_not_written_over() {
    let mut c = Case::new("theirs");
    let path = c.data.join("mobile/Library/Preferences");
    put(&path.join("com.apple.springboard.plist"), &plist::dumps_xml(&d(vec![("SBIconVisibility", P::Bool(true)), ("FBLaunchWatchdogScale", P::Int(20))])).unwrap());
    c.apply(&[]);
    let sb = read_plist(&c.data, "mobile/Library/Preferences/com.apple.springboard.plist");
    assert!(matches!(sb.get("SBIconVisibility"), Some(P::Bool(true))));
    assert!(matches!(sb.get("FBLaunchWatchdogScale"), Some(P::Real(f)) if *f == 4.0));
    fs::write(path.join("com.apple.purplebuddy.plist"), b"not a plist").unwrap();
    c.apply_fails(&[]);
    assert!(c.log.contains("not a property list"), "{}", c.log);
    assert_eq!(fs::read(path.join("com.apple.purplebuddy.plist")).unwrap(), b"not a plist");
}

#[test]
fn sealed_a_second_run_changes_nothing() {
    let mut c = Case::new("second-run");
    let (helpers, _) = c.helpers();
    let h = helpers.to_string_lossy().into_owned();
    c.apply(&["--helpers", &h]);
    let entries = |c: &Case| -> Vec<Value> { c.read("tweaks.manifest.json").get("entries").unwrap().as_list().unwrap().iter().filter(|e| str_of(e, "path") != REPORT).cloned().collect() };
    let (first, manifest) = (digest(&c.data), entries(&c));
    c.apply(&["--helpers", &h]);
    assert_eq!(digest(&c.data), first);
    assert_eq!(entries(&c), manifest);
    let states = c.states();
    for item in ["disabled.plist", "preferences: setup", "preferences: coreanimation", "data layout", "CoreLocation flags"] {
        assert_eq!(states[item], "kept", "{item}");
    }
}

#[test]
fn sealed_corelocation_cohorts_are_off_on_the_data_volume() {
    let mut c = Case::new("corelocation");
    let flags = "preferences/FeatureFlags/Domain/CoreLocation.plist";
    put(&c.data.join(flags), &plist::dumps_xml(&d(vec![("Somebody", d(vec![("Enabled", P::Bool(true))]))])).unwrap());
    c.apply(&[]);
    let have = read_plist(&c.data, flags);
    assert!(plist::py_eq(&have, &d(vec![("Somebody", d(vec![("Enabled", P::Bool(true))])), ("CLAutoCohort", d(vec![("Enabled", P::Bool(false))]))])));
    assert!(fs::read(c.data.join(flags)).unwrap().starts_with(b"<?xml"), "XML, as the kernel (hosted 0048) reads it");
    assert_eq!(c.states()["CoreLocation flags"], "applied");
    // the sealed root keeps Apple's defaults and gets no ExtensionKit flag
    assert!(!c.data.join("preferences/FeatureFlags/Domain/ExtensionKit.plist").exists());
}

#[test]
fn sealed_the_data_layout() {
    let mut c = Case::new("layout");
    let cache = "containers/Shared/SystemGroup/systemgroup.com.apple.mobilegestaltcache/Library/Caches";
    put(&c.data.join(cache).join("com.apple.MobileGestalt.plist"), &plist::dumps_xml(&d(vec![("CacheExtra", d(vec![("h9jDsbgj7xIVeIQ8S3/X3Q", s("iPad17,3"))]))])).unwrap());
    c.apply(&[]);
    assert!(!c.data.join(cache).join("com.apple.MobileGestalt.plist").exists());
    // the swap files' folder (XNU's /private/var/vm/swapfileN), root 0755 as the kernel (hosted 0057) makes it
    assert!(c.data.join("vm").is_dir());
    assert_eq!(mode_of(&c.data.join("vm")), 0o755);
    assert_eq!(c.states()["MobileGestalt cache"], "applied");
    assert_eq!(int_of(&c.entry("data", "containers/Bundle"), "uid"), 33);
    assert_eq!(str_of(&c.entry("data", "tmp"), "mode"), "1777");
    assert_eq!(str_of(&c.entry("data", "keybags"), "mode"), "0700");
    let logd = c.entry("data", "db/diagnostics/.migrated");
    assert_eq!((int_of(&logd, "uid"), str_of(&logd, "mode")), (272, "0644".into()));
    assert_eq!(str_of(&c.entry("data", "db/diagnostics"), "mode"), "0751");
    // no keybag unless asked: UniXNU's key store wants the folder empty
    assert_eq!(fs::read_dir(c.data.join("keybags")).unwrap().count(), 0);
    c.apply(&["--plain-keybag"]);
    assert!(plist::py_eq(&read_plist(&c.data, "keybags/systembag.kb"), &d(vec![("KeyBagKeys", P::Data(b"SWEET".to_vec()))])));
    assert_eq!(str_of(&c.entry("data", "keybags/systembag.kb"), "mode"), "0600");
}

#[test]
fn sealed_our_services_and_the_trust_list() {
    let mut c = Case::new("services");
    let (helpers, hashes) = c.helpers();
    c.apply(&["--helpers", &helpers.to_string_lossy()]);
    let base = c.data.join("oneios");
    let mut enabled: Vec<String> = fs::read_dir(base.join("enabled")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    enabled.sort();
    assert_eq!(enabled, ["bootstat", "dropbear", "identity"]);
    let conf = fs::read_to_string(base.join("services.d/dropbear.conf")).unwrap();
    let conf: Vec<&str> = conf.lines().collect();
    assert_eq!(conf[..6], ["arg=/private/var/oneios/bin/dropbear", "arg=-F", "arg=-E", "arg=-R", "arg=-p", "arg=2222"]);
    assert!(conf.contains(&"keepalive=1"));
    let expect = fs::read_to_string(base.join("identity.expect")).unwrap();
    assert!(expect.contains("DeviceClass=iPhone\n"));
    assert!(expect.contains("main-screen-scale=3\n"));
    assert_eq!(str_of(&c.entry("data", "oneios/bin/oneios-init"), "mode"), "0755");
    assert_eq!(str_of(&c.entry("data", "oneios/services.d/dropbear.conf"), "mode"), "0644");
    let trust = c.read("owner-trust.json");
    let mut want: Vec<String> = hashes.values().cloned().collect();
    want.sort();
    assert_eq!(strs(trust.get("cdhashes").unwrap()), want);
    let files = trust.get("files").unwrap().as_list().unwrap();
    let mut paths: Vec<String> = files.iter().map(|f| str_of(f, "path")).collect();
    paths.sort();
    let mut want_paths: Vec<String> = hashes.keys().map(|n| format!("/private/var/oneios/bin/{n}")).collect();
    want_paths.sort();
    assert_eq!(paths, want_paths);
    assert!(files.iter().all(|f| f.get("warnings").unwrap().as_list().unwrap().is_empty()));
    let manager = c.contract().get("launch_policy").unwrap().get("service_manager").cloned().unwrap();
    assert_eq!((str_of(&manager, "path"), manager.get("present").cloned()), ("/private/var/oneios/bin/oneios-init".into(), Some(Value::Bool(true))));
    assert!(fs::read_to_string(base.join("roottweak-report.txt")).unwrap().contains("layout sealed"));
}

#[test]
fn sealed_without_helpers_the_services_are_placed_and_nothing_is_enabled() {
    let mut c = Case::new("no-helpers");
    c.apply(&[]);
    let base = c.data.join("oneios");
    assert_eq!(fs::read_dir(base.join("enabled")).unwrap().count(), 0);
    assert!(base.join("services.d/bootstat.conf").is_file());
    assert_eq!(c.states()["oneios-init"], "off");
    assert_eq!(c.contract().get("launch_policy").unwrap().get("service_manager").unwrap().get("present").cloned(), Some(Value::Bool(false)));
    assert!(c.read("owner-trust.json").get("cdhashes").unwrap().as_list().unwrap().is_empty());
}

#[test]
fn sealed_a_helper_that_is_not_signed_or_not_arm64e() {
    let mut c = Case::new("bad-helper");
    let bad = c.top.join("bad");
    fs::create_dir(&bad).unwrap();
    fs::write(bad.join("oneios-init"), b"\x7fELF a host build").unwrap();
    c.apply_fails(&["--helpers", &bad.to_string_lossy()]);
    assert!(c.log.contains("not a Mach-O"), "{}", c.log);
    let (helpers, _) = c.helpers_named(&["oneios-init", "oneios-bootstat", "oneios-mg", "dropbear"], false, 14);
    c.apply(&["--helpers", &helpers.to_string_lossy()]);
    let warnings = strs(c.read("owner-trust.json").get("files").unwrap().as_list().unwrap()[0].get("warnings").unwrap());
    assert!(warnings.iter().any(|w| w.contains("not arm64e")) && warnings.iter().any(|w| w.contains("16384")), "{warnings:?}");
}

fn bundle(top: &Path, info: P) -> (PathBuf, Vec<String>) {
    let b = top.join("NativeAdrenoMetal.bundle");
    fs::create_dir_all(b.join("_CodeSignature")).unwrap();
    fs::write(b.join("Info.plist"), plist::dumps_xml(&info).unwrap()).unwrap();
    let mut hashes = Vec::new();
    for name in ["NativeAdrenoMetal", "libvulkan_freedreno.dylib"] {
        let (data, cdhash) = signed(&format!("org.example.{name}"));
        hashes.push(cdhash);
        fs::write(b.join(name), data).unwrap();
    }
    fs::write(b.join("_CodeSignature/CodeResources"), "resources").unwrap();
    (b, hashes)
}

fn info() -> P {
    d(vec![("CFBundleExecutable", s("NativeAdrenoMetal")), ("NSPrincipalClass", s("NativeAdrenoMetalDevice"))])
}

#[test]
fn sealed_the_metal_plugin_goes_to_the_addon_view() {
    let mut c = Case::new("metal");
    let (b, mut hashes) = bundle(&c.top, info());
    let bs = b.to_string_lossy().into_owned();
    // a bundle that does not say it was built for this guest's shared cache is not placed
    for target in [None, Some("iPad17,3 24A446")] {
        if let Some(t) = target {
            fs::write(b.join("oneios-target"), format!("{t}\n")).unwrap();
        }
        c.apply(&["--metal-plugin", &bs]);
        assert_eq!(c.states()["metal plugin"], "off");
        assert!(!c.data.join("oneios/addons").exists());
        assert!(c.read("owner-trust.json").get("cdhashes").unwrap().as_list().unwrap().is_empty());
    }
    fs::write(b.join("oneios-target"), format!("iPhone18,2 {BUILD}\n")).unwrap();
    c.apply(&["--metal-plugin", &bs]);
    let base = c.data.join("oneios/addons/Extensions");
    let mut names: Vec<String> = fs::read_dir(&base).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, ["NativeAdrenoMetal.bundle", "NativeAdrenoMetal.clients", "NativeAdrenoMetal.publish"]);
    assert_eq!(str_of(&c.entry("data", "oneios/addons/Extensions/NativeAdrenoMetal.bundle/oneios-target"), "mode"), "0644");
    assert_eq!(fs::read_to_string(base.join("NativeAdrenoMetal.clients")).unwrap(), "WidgetRenderer_Default\n*Poster*\niconservicesagent\nSBRendererService\n");
    assert_eq!(str_of(&c.entry("data", "oneios/addons/Extensions/NativeAdrenoMetal.bundle/NativeAdrenoMetal"), "mode"), "0755");
    assert_eq!(str_of(&c.entry("data", "oneios/addons/Extensions/NativeAdrenoMetal.bundle/Info.plist"), "mode"), "0644");
    let contract = c.contract();
    assert_eq!(str_of(&contract.get("addon_mounts").unwrap().as_list().unwrap()[0], "over"), "/System/Library/Extensions");
    assert_eq!(str_of(contract.get("metal").unwrap(), "principal_class"), "NativeAdrenoMetalDevice");
    hashes.sort();
    assert_eq!(strs(c.read("owner-trust.json").get("cdhashes").unwrap()), hashes);
    c.apply(&["--metal-plugin", &bs, "--metal-client", "*"]);
    assert_eq!(fs::read_to_string(base.join("NativeAdrenoMetal.clients")).unwrap(), "*\n");
    let top = c.top.to_string_lossy().into_owned();
    c.apply_fails(&["--metal-plugin", &top]);
}

#[test]
fn sealed_the_camera_plugin_goes_to_the_mediacapture_view() {
    let mut c = Case::new("camera");
    c.apply(&[]);
    assert_eq!(c.states()["camera plugin"], "off");
    let folder = c.top.join("camera");
    fs::create_dir(&folder).unwrap();
    let f = folder.to_string_lossy().into_owned();
    c.apply_fails(&["--camera-plugin", &f]); // no H9ISP.mediacapture in it
    let (data, cdhash) = signed("com.oneios.H9ISP");
    fs::write(folder.join("H9ISP.mediacapture"), data).unwrap();
    c.apply(&["--camera-plugin", &f]);
    assert_eq!(c.states()["camera plugin"], "applied");
    assert_eq!(str_of(&c.entry("data", "oneios/addons/MediaCapture/H9ISP.mediacapture"), "mode"), "0755");
    let contract = c.contract();
    let mount = contract.get("addon_mounts").unwrap().as_list().unwrap().iter().find(|m| str_of(m, "over") == "/System/Library/MediaCapture").cloned().unwrap();
    assert_eq!(str_of(&mount, "source"), "/private/var/oneios/addons/MediaCapture");
    assert_eq!(str_of(contract.get("camera").unwrap(), "symbol"), "H9ISPCaptureDeviceCreate");
    assert!(strs(c.read("owner-trust.json").get("cdhashes").unwrap()).contains(&cdhash));
}

#[test]
fn sealed_the_app_cryptex_is_carried_on_the_data_volume() {
    let mut c = Case::new("appos");
    let appos = c.top.join("appos");
    fs::create_dir_all(appos.join("usr/libexec")).unwrap();
    fs::create_dir_all(appos.join(".fseventsd")).unwrap();
    fs::write(appos.join("usr/libexec/exampled"), b"stand-in").unwrap();
    fs::set_permissions(appos.join("usr/libexec/exampled"), fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("libexec/exampled", appos.join("usr/link")).unwrap();
    c.apply(&["--appos", &appos.to_string_lossy()]);
    let base = c.data.join("oneios/preboot/Cryptexes");
    assert!(base.join("App/usr/libexec/exampled").is_file());
    assert_eq!(fs::read_link(base.join("App/usr/link")).unwrap(), Path::new("libexec/exampled"));
    assert!(!base.join("App/.fseventsd").exists());
    assert!(base.join("OS/System").is_dir() && base.join("OS/usr").is_dir());
    assert_eq!(str_of(&c.entry("data", "oneios/preboot/Cryptexes/App/usr/libexec/exampled"), "mode"), "0755");
    let mounts: Vec<String> = c.contract().get("addon_mounts").unwrap().as_list().unwrap().iter().map(|m| str_of(m, "over")).collect();
    assert_eq!(mounts, ["/private/preboot", "/private/preboot/Cryptexes/OS/System", "/private/preboot/Cryptexes/OS/usr"]);
}

#[test]
fn sealed_the_contract_names_the_iphone() {
    let mut c = Case::new("contract");
    c.apply(&[]);
    let k = c.contract();
    let root = k.get("adt").unwrap().get("root").unwrap();
    assert_eq!((str_of(root, "model"), str_of(root, "target-type"), strs(root.get("compatible").unwrap())), ("iPhone18,2".into(), "V54".into(), vec!["V54AP".to_string(), "iPhone18,2".into(), "AppleARM".into()]));
    assert_eq!(str_of(k.get("app_cryptex").unwrap(), "image"), "app.img");
    assert_eq!(int_of(k.get("adt").unwrap().get("chosen").unwrap(), "display-scale"), 3);
    assert_eq!(str_of(k.get("adt").unwrap().get("product").unwrap(), "artwork-device-idiom"), "phone");
    let disp = k.get("display").unwrap();
    assert_eq!((int_of(disp, "width"), int_of(disp, "height"), int_of(disp, "scale")), (1440, 3120, 3));
    assert_eq!(disp.get("points").cloned().unwrap(), Value::List(vec![Value::Int(480), Value::Int(1040)]));
    assert!(fs::read_to_string(c.out.join("xnuhost.conf")).unwrap().contains("scale=3\n"));
}

#[test]
fn sealed_scale_2_reaches_the_contract_the_seed_and_xnuhost() {
    let mut c = Case::new("scale2");
    c.apply(&["--scale", "2"]);
    let k = c.contract();
    let disp = k.get("display").unwrap();
    assert_eq!((int_of(disp, "width"), int_of(disp, "height"), int_of(disp, "scale")), (1440, 3120, 2));
    assert_eq!(disp.get("points").cloned().unwrap(), Value::List(vec![Value::Int(720), Value::Int(1560)]));
    let adt = k.get("adt").unwrap();
    assert_eq!((int_of(adt.get("chosen").unwrap(), "display-scale"), int_of(adt.get("product").unwrap(), "artwork-scale-factor")), (2, 2));
    let expect = fs::read_to_string(c.data.join("oneios/identity.expect")).unwrap();
    for line in ["main-screen-scale=2\n", "main-screen-width=1440\n", "main-screen-height=3120\n"] {
        assert!(expect.contains(line), "{line}");
    }
    let conf = fs::read_to_string(c.out.join("xnuhost.conf")).unwrap();
    assert_eq!(conf.lines().filter(|l| !l.starts_with('#')).collect::<Vec<_>>(), ["scale=2", "pen=auto"]);
    assert!(disp.get("scale_note").is_some());
    assert!(adt.get("required").unwrap().get("chosen/unique-chip-id").is_some());
    // the two nodes of Apple's tree that must stay out: the modem, and the fstab that names volumes this layout has not
    let mut absent: Vec<String> = adt.get("absent").unwrap().as_dict().unwrap().iter().map(|(k, _)| k.clone()).collect();
    absent.sort();
    assert_eq!(absent, ["baseband", "filesystems/fstab"]);
    assert!(str_of(adt.get("absent").unwrap(), "filesystems/fstab").contains("exits 66"));
    let o = k.get("launch_policy").unwrap().get("override").unwrap();
    assert_eq!((str_of(o, "path"), str_of(o, "owner"), str_of(o, "mode")), ("/private/var/db/com.apple.xpc.launchd/disabled.plist".into(), "root:wheel".into(), "0644".into()));
    assert_eq!((str_of(&k, "layout"), str_of(&k, "build")), ("sealed".into(), BUILD.into()));
    assert!(k.get("kernel_args").unwrap().as_list().unwrap().is_empty());
    assert!(k.get("kernel_features").unwrap().as_list().unwrap().iter().any(|f| str_of(f, "name") == "key-store"));
    assert_eq!(c.states()["target-type"], "kept");
}

#[test]
fn sealed_refusals() {
    let mut c = Case::new("refusals");
    fs::write(c.root.join("System/Library/CoreServices/SystemVersion.plist"), version_plist("23F84", "26.5.2")).unwrap();
    c.apply_fails(&[]);
    assert!(c.log.contains("--any-version"), "{}", c.log);
    assert_eq!(fs::read_dir(&c.data).unwrap().count(), 0);
    c.apply(&["--any-version"]);
    assert_eq!(read_plist(&c.data, "mobile/Library/Preferences/com.apple.migration.plist").get("LastSystemVersion").and_then(P::as_str), Some("23F84"));
    // not a root at all
    let empty = c.top.join("empty");
    fs::create_dir(&empty).unwrap();
    c.root = empty;
    c.apply_fails(&[]);
    assert!(c.log.contains("not an iOS root"), "{}", c.log);
}

#[test]
fn sealed_20f66_gets_the_16_userland_boot_args() {
    // port-a56: a 20F66 root writes the 16.5 userland's xnuhost.conf; the keybagd fixup is scoped by hash to the
    // real 20F66 binary, so the stand-in (whose keybagd differs) must not get it
    let mut c = Case::new("20f66");
    fs::write(c.root.join("System/Library/CoreServices/SystemVersion.plist"), version_plist("20F66", "16.5")).unwrap();
    fs::write(c.root.join("usr/libexec/keybagd"), b"not the real one").unwrap();
    // 20F66 with 16.5 needs no --any-version (the A56's userland, 2026-11-23); another build of 16.5 still does
    c.apply(&[]);
    let report = fs::read_to_string(c.out.join("report.txt")).unwrap();
    assert!(report.contains("applied   version: root is 16.5 (20F66), the A56's iOS 16.5"), "{report}");
    let conf = fs::read_to_string(c.out.join("xnuhost.conf")).unwrap();
    for part in [
        "scale=",
        "oneios-gpu-metal=0",
        "oneios-legacy-bootstrap-replies=1",
        "oneios-legacy-registered-ports=1",
        "oneios-mac-exec-skip=mediaserverd,TVRemoteConnectionService oneios-hid16=1 ",
        " oneios-grant=configd:com.apple.networking.ionetworkstack.user-client oneios-ls-keep=3 oneios-disable-jobs=",
        ",com.apple.tailspind\n",
        "text-fixup=dyld_shared_cache_arm64.03,15e4684,e8279f1a,28008052",
    ] {
        assert!(conf.contains(part), "{part}");
    }
    assert!(!conf.contains("text-fixup=keybagd"));
    // The A56's 16.5 image never claimed the pen: no pen=auto there.
    assert!(!conf.contains("pen=auto"), "{conf}");
}

// ---------------------------------------------------------------- Hook

fn env_of<'a>(map: &'a HashMap<&str, String>) -> impl Fn(&str) -> Option<String> + 'a {
    move |k: &str| map.get(k).cloned()
}

#[test]
fn hook_the_image_hook_speaks_api_1() {
    let c = Case::new("hook");
    let (overlay, owners, info) = (c.top.join("overlay"), c.top.join("owners.tsv"), c.top.join("info.json"));
    fs::create_dir(&overlay).unwrap();
    let none = c.top.join("none").to_string_lossy().into_owned();
    let mut env: HashMap<&str, String> = HashMap::new();
    for (k, v) in [
        ("ONEIOS_HOOK_API", "1".to_string()),
        ("ONEIOS_OS_ROOT", c.root.to_string_lossy().into_owned()),
        ("ONEIOS_DATA", c.data.to_string_lossy().into_owned()),
        ("ONEIOS_OVERLAY", overlay.to_string_lossy().into_owned()),
        ("ONEIOS_OWNERS", owners.to_string_lossy().into_owned()),
        ("ONEIOS_INFO", info.to_string_lossy().into_owned()),
        ("ONEIOS_BUILD", BUILD.to_string()),
        ("ONEIOS_HELPERS", none.clone()),
        ("ONEIOS_METAL_PLUGIN", none.clone()),
        ("ONEIOS_CAMERA_PLUGIN", none.clone()),
        ("ONEIOS_ROOTTWEAK_ARGS", "--keep com.apple.momentsd".to_string()),
        ("ONEIOS_SCALE", "2".to_string()),
    ] {
        env.insert(k, v);
    }
    let before = digest(&c.root);
    hook::run_hook(&hook::Hook::from_env(&env_of(&env), None).unwrap()).unwrap();
    assert_eq!(digest(&c.root), before);
    let mut names: Vec<String> = fs::read_dir(overlay.join("userland")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, ["boot-contract.json", "owner-trust.json", "report.json", "report.txt", "tweaks.manifest.json", "xnuhost.conf"]);
    let conf = fs::read_to_string(overlay.join("userland/xnuhost.conf")).unwrap();
    assert!(conf.contains("scale=2\n"));
    // The S Pen stays on across a rebuilt image (docs/hardware.md 7f): xnuhost's pen=auto, safe on a phone without one.
    assert!(conf.contains("pen=auto\n"), "{conf}");
    let summary = json::load(&info).unwrap();
    assert!(summary.get("first_start_unpacker").unwrap().is_null());
    let rules: HashMap<String, String> = fs::read_to_string(&owners).unwrap().lines().map(|l| l.split_once('\t').map(|(a, b)| (a.to_string(), b.to_string())).unwrap()).collect();
    assert_eq!(rules["db/com.apple.xpc.launchd/disabled.plist"], "0\t0\t0644");
    assert_eq!(rules["mobile/Library/Preferences/com.apple.springboard.plist"], "501\t501\t0600");
    assert_eq!(rules["tmp"], "0\t0\t1777");
    assert_eq!((str_of(&summary, "layout"), int_of(&summary, "disabled_jobs"), str_of(&summary, "build")), ("sealed".into(), 3, BUILD.into())); // the replace jobs of the fake root (base and extra run since 2026-11-23)
    assert!(strs(summary.get("off").unwrap()).iter().any(|l| l.starts_with("oneios-init")));
    assert!(read_plist(&c.data, "db/com.apple.xpc.launchd/disabled.plist").get("com.apple.momentsd").is_none());
    // with built helpers: the first-start unpacker goes to the overlay, not into the seed, and is trusted
    let (helpers, hashes) = c.helpers_named(&["oneios-init", "oneios-seed"], true, 12);
    env.insert("ONEIOS_HELPERS", helpers.to_string_lossy().into_owned());
    hook::run_hook(&hook::Hook::from_env(&env_of(&env), None).unwrap()).unwrap();
    let seed = overlay.join("userland/bin/oneios-seed");
    assert!(mode_of(&seed) & 0o111 != 0);
    assert!(!c.data.join("oneios/bin/oneios-seed").exists());
    let mut want: Vec<String> = hashes.values().cloned().collect();
    want.sort();
    assert_eq!(strs(json::load(overlay.join("userland/owner-trust.json")).unwrap().get("cdhashes").unwrap()), want);
    assert_eq!(str_of(&json::load(&info).unwrap(), "first_start_unpacker"), "overlay/userland/bin/oneios-seed");
    env.insert("ONEIOS_HOOK_API", "2".into());
    assert!(hook::Hook::from_env(&env_of(&env), None).is_err());
}

#[test]
fn hook_the_hook_checks_the_plug_ins_build_record() {
    let c = Case::new("hook-record");
    let (overlay, info_file) = (c.top.join("overlay"), c.top.join("info.json"));
    fs::create_dir(&overlay).unwrap();
    fs::create_dir(c.top.join("out")).unwrap();
    let (b, hashes) = bundle(&c.top.join("out"), info());
    fs::remove_dir_all(b.join("_CodeSignature")).unwrap();
    fs::write(b.join("oneios-target"), format!("iPhone18,2 {BUILD}\n")).unwrap();
    let record = c.top.join("out/NativeAdrenoMetal.build.json");
    let write = |cdhashes: &[String]| {
        let mut v = Value::dict();
        v.set("target", format!("iPhone18,2 {BUILD}"));
        v.set("cdhashes", Value::List(cdhashes.iter().map(Value::str).collect()));
        let mut sdk = Value::dict();
        sdk.set("version", "26.5");
        v.set("sdk", sdk);
        fs::write(&record, json::dumps(&v, None, false)).unwrap();
    };
    let none = c.top.join("none").to_string_lossy().into_owned();
    let env: HashMap<&str, String> = [
        ("ONEIOS_HOOK_API", "1".to_string()),
        ("ONEIOS_OS_ROOT", c.root.to_string_lossy().into_owned()),
        ("ONEIOS_DATA", c.data.to_string_lossy().into_owned()),
        ("ONEIOS_OVERLAY", overlay.to_string_lossy().into_owned()),
        ("ONEIOS_INFO", info_file.to_string_lossy().into_owned()),
        ("ONEIOS_BUILD", BUILD.to_string()),
        ("ONEIOS_HELPERS", none.clone()),
        ("ONEIOS_METAL_PLUGIN", b.to_string_lossy().into_owned()),
        ("ONEIOS_CAMERA_PLUGIN", none),
    ]
    .into_iter()
    .collect();
    let mut sorted = hashes.clone();
    sorted.sort();
    write(&sorted);
    hook::run_hook(&hook::Hook::from_env(&env_of(&env), None).unwrap()).unwrap();
    assert_eq!(strs(json::load(overlay.join("userland/owner-trust.json")).unwrap().get("cdhashes").unwrap()), sorted);
    let metal = json::load(&info_file).unwrap().get("metal_plugin").cloned().unwrap();
    assert_eq!(metal.get("placed").cloned(), Some(Value::Bool(true)));
    assert_eq!((str_of(&metal, "target"), str_of(&metal, "sdk"), str_of(&metal, "record")), (format!("iPhone18,2 {BUILD}"), "26.5".into(), "NativeAdrenoMetal.build.json".into()));
    assert_eq!(strs(metal.get("cdhashes").unwrap()), sorted);
    // a bundle that is not the one its record describes stops the build
    let mut other = vec![hashes[0].clone(), "00".repeat(20)];
    other.sort();
    write(&other);
    let e = hook::run_hook(&hook::Hook::from_env(&env_of(&env), None).unwrap()).unwrap_err();
    assert!(e.to_string().contains("changed after it was built"), "{e}");
}

// ---------------------------------------------------------------- ReadWrite

fn cache_of(c: &Case) -> P {
    read_plist(&c.root, "System/Library/xpc/launchd.plist")
}

fn job<'a>(cache: &'a P, label: &str) -> Option<(String, &'a P)> {
    launchd::jobs(cache).into_iter().find(|(l, _, _)| l == label).map(|(_, g, k)| (k.clone(), cache.get(&g).unwrap().get(&k).unwrap()))
}

#[test]
fn rw_the_launchd_cache_and_the_jetsam_properties_are_edited() {
    let mut c = Case::new("rw");
    let (helpers, _) = c.helpers();
    assert_eq!(c.apply_in("rw", &["--helpers", &helpers.to_string_lossy()]), 0, "{}", c.log);
    let cache = cache_of(&c);
    assert!(job(&cache, "com.apple.watchdogd").is_none());
    for label in ["com.apple.nfcd", "com.apple.momentsd"] {
        assert!(job(&cache, label).is_some(), "{label}"); // base and extra run since 2026-11-23
    }
    assert!(job(&cache, "com.apple.analyticsd").is_some()); // kept: its clients spin without it
    for label in ["com.apple.SpringBoard", "com.apple.CommCenter", "com.apple.example.unknownd", "com.apple.example.boot"] {
        assert!(job(&cache, label).is_some(), "{label}");
    }
    // our manager is a launchd job whose program is on the root file system
    let (key, init) = job(&cache, "com.oneios.init").unwrap();
    assert_eq!(key, "/System/Library/LaunchDaemons/com.oneios.init.plist");
    assert!(plist::py_eq(init.get("ProgramArguments").unwrap(), &P::Array(vec![s("/usr/libexec/oneios-init"), s("/private/var/oneios")])));
    assert!(init.get("RunAtLoad").unwrap().truthy() && init.get("KeepAlive").unwrap().truthy() && init.get("UserName").and_then(P::as_str) == Some("root"));
    assert!(plist::py_eq(&read_plist(&c.root, "System/Library/LaunchDaemons/com.oneios.init.plist"), init));
    assert_eq!(str_of(&c.entry("root", "usr/libexec/oneios-init"), "mode"), "0755");
    assert!(c.read("owner-trust.json").get("files").unwrap().as_list().unwrap().iter().any(|f| str_of(f, "path") == "/usr/libexec/oneios-init"));
    // a crash loop does not restart the guest
    let (_, sb) = job(&cache, "com.apple.SpringBoard").unwrap();
    assert!(sb.get("_PanicOnCrash").is_none());
    assert!(matches!(sb.get("_PanicOnCrasX"), Some(P::Bool(true))));
    let (_, bb) = job(&cache, "com.apple.backboardd").unwrap();
    assert!(plist::py_eq(bb.get("EnvironmentVariables").unwrap(), &d(vec![("CA_EXAMPLE", s("1"))])));
    let jetsam = read_plist(&c.root, "System/Library/LaunchDaemons/com.apple.jetsamproperties.V54.plist");
    let over = jetsam.get("Version4").unwrap().get("Daemon").unwrap().get("Override").unwrap();
    assert!(plist::py_eq(over.get("com.apple.backboardd").unwrap(), &d(vec![("ActiveSoftMemoryLimit", P::Int(100))])));
    assert!(plist::py_eq(over.get("com.apple.analyticsd").unwrap(), &d(vec![("ActiveSoftMemoryLimit", P::Int(10))])));
    assert!(launchd::forced(&jetsam).is_empty());
    // the Data tree is the root's /private/var; the edited cache needs the boot argument
    assert!(read_plist(&c.root, "private/var/db/com.apple.xpc.launchd/disabled.plist").get("com.apple.watchdogd").is_some());
    let ek = read_plist(&c.root, "private/var/preferences/FeatureFlags/Domain/ExtensionKit.plist");
    assert!(matches!(ek.get("enforce_xpc_cache_code_signing").and_then(|f| f.get("Enabled")), Some(P::Bool(false))));
    let k = c.contract();
    let args: Vec<String> = k.get("kernel_args").unwrap().as_list().unwrap().iter().map(|a| str_of(a, "arg")).collect();
    assert_eq!(args, ["launchd_unsecure_cache=1"]);
    assert!(k.get("launch_policy").unwrap().get("service_manager").unwrap().is_null());
    assert!(k.get("kernel_features").is_none());
}

#[test]
fn rw_a_second_run_and_the_kept_key() {
    let mut c = Case::new("rw-again");
    assert_eq!(c.apply_in("rw", &[]), 0, "{}", c.log);
    let first = digest(&c.root);
    assert_eq!(c.apply_in("rw", &[]), 0, "{}", c.log);
    assert_eq!(digest(&c.root), first);
    assert_eq!(c.states()["launchd cache"], "kept");
}

#[test]
fn rw_recover_on_crash_can_stay() {
    let mut c = Case::new("rw-recover");
    assert_eq!(c.apply_in("rw", &["--keep-recover-on-crash"]), 0, "{}", c.log);
    let jetsam = read_plist(&c.root, "System/Library/LaunchDaemons/com.apple.jetsamproperties.V54.plist");
    assert!(matches!(jetsam.get("Version4").unwrap().get("Daemon").unwrap().get("Override").unwrap().get("com.apple.backboardd").unwrap().get("RecoverOnCrash"), Some(P::Bool(true))));
}

#[test]
fn rw_staged_apps_and_the_plugin_go_into_the_root() {
    let mut c = Case::new("rw-staged");
    put(&c.root.join("private/var/staged_system_apps/Example.app/Info.plist"), &plist::dumps_xml(&d(vec![("CFBundleIdentifier", s("com.example.app"))])).unwrap());
    fs::create_dir_all(c.root.join("Applications")).unwrap();
    let b = c.top.join("NativeAdrenoMetal.bundle");
    fs::create_dir_all(&b).unwrap();
    fs::write(b.join("Info.plist"), plist::dumps_xml(&d(vec![("CFBundleExecutable", s("NativeAdrenoMetal"))])).unwrap()).unwrap();
    fs::write(b.join("NativeAdrenoMetal"), signed("com.oneios.test").0).unwrap();
    assert_eq!(c.apply_in("rw", &["--metal-plugin", &b.to_string_lossy(), "--metal-plugin-unchecked"]), 0, "{}", c.log);
    assert!(c.root.join("Applications/Example.app/Info.plist").is_file());
    assert!(c.root.join("System/Library/Extensions/NativeAdrenoMetal.bundle/NativeAdrenoMetal").is_file());
    assert!(c.contract().get("addon_mounts").unwrap().as_list().unwrap().is_empty());
}

// ---------------------------------------------------------------- Trees

#[test]
fn trees_links_are_followed_inside_the_tree_only() {
    let top = tmpdir("links");
    let outside = top.join("outside");
    let tree_top = top.join("tree");
    fs::create_dir_all(tree_top.join("private/var")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink("/private/var", tree_top.join("var")).unwrap(); // absolute: from the tree's top
    std::os::unix::fs::symlink("../../outside", tree_top.join("escape")).unwrap(); // relative, pointing out
    std::os::unix::fs::symlink(&outside, tree_top.join("abs")).unwrap(); // absolute host path
    let tree = Tree::new(&tree_top, "root", false).unwrap();
    let mut m = Manifest::default();
    tree.write(&mut m, "var/a.txt", b"a", (0, 0), 0o644, None).unwrap();
    assert!(tree_top.join("private/var/a.txt").is_file());
    tree.write(&mut m, "escape/b.txt", b"b", (0, 0), 0o644, None).unwrap();
    tree.write(&mut m, "abs/c.txt", b"c", (0, 0), 0o644, None).unwrap();
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    for path in [tree.host("escape/b.txt").unwrap(), tree.host("abs/c.txt").unwrap(), tree.host("../../etc/passwd").unwrap()] {
        assert!(path.starts_with(&tree_top) && path != tree_top, "{}", path.display());
    }
    assert!(Tree::new(&tree_top, "root", true).unwrap().write(&mut m, "x", b"x", (0, 0), 0o644, None).is_err());
    let _ = fs::remove_dir_all(&top);
}

#[test]
fn trees_merge_keeps_types_apart() {
    let top = tmpdir("merge");
    let tree = Tree::new(&top, "data", false).unwrap();
    let mut m = Manifest::default();
    tree.write_plist(&mut m, "p.plist", &d(vec![("a", P::Int(1)), ("b", P::Bool(true))]), (0, 0), 0o644, true, None).unwrap();
    let values = vec![("a".to_string(), P::Int(1)), ("b".to_string(), P::Int(1)), ("c".to_string(), P::Int(2))];
    assert_eq!(tree.merge_plist(&mut m, "p.plist", &values, (0, 0), 0o644, true, None).unwrap(), ["b", "c"]);
    assert!(tree.merge_plist(&mut m, "p.plist", &values, (0, 0), 0o644, true, None).unwrap().is_empty());
    let _ = fs::remove_dir_all(&top);
}

// ---------------------------------------------------------------- BootCheck

const LOG: &str = "\
2026-11-10 14:11:18.969742 (user/501) <Notice>: pending spawn, domain in on-demand-only mode: com.apple.sleepd
2026-11-10 14:11:19.407385 (user/501/com.apple.sleepd) <Notice>: service state: spawning
2026-11-10 14:11:25.728310 (system/com.apple.backboardd) <Notice>: service state: spawning
2026-11-10 14:11:27.945354 (user/501/com.apple.SpringBoard [32]) <Notice>: service state: running
2026-11-10 14:11:29.000000 (user/501) <Warning>: failed lookup: name = com.apple.watchdogd, requestor = backboardd[55], error = 3: No such process
";

fn booted(c: &mut Case, more: &str) {
    c.apply(&[]);
    put(&c.data.join("log/com.apple.xpc.launchd/launchd.log"), format!("{LOG}{more}").as_bytes());
}

#[test]
fn bootcheck_the_policy_was_applied() {
    let mut c = Case::new("boot-ok");
    booted(&mut c, "");
    let (lines, problems) = bootcheck::check(&c.data, Some(&c.root), !as_root());
    assert!(problems.is_empty(), "{problems:?}");
    let text = lines.join("\n");
    assert!(text.contains("2 jobs spawned, 1 pending at load"), "{text}");
    assert!(text.contains("com.apple.watchdogd (asked by backboardd[55])"), "{text}");
    assert!(text.contains("no disabled job was started"), "{text}");
}

#[test]
fn bootcheck_a_disabled_job_that_ran_is_a_problem_unless_launchd_may_not_disable_it() {
    let mut c = Case::new("boot-ran");
    booted(&mut c, "2026-11-10 14:12:00.000000 (system/com.apple.watchdogd) <Notice>: service state: spawning\n2026-11-10 14:12:01.000000 (system/com.apple.OTATaskingAgent [70]) <Notice>: service state: spawning\n");
    let (lines, problems) = bootcheck::check(&c.data, Some(&c.root), true);
    assert_eq!(problems, ["1 disabled jobs were started: com.apple.watchdogd"]);
    assert!(lines.contains(&"run although disabled (DisableAllowed = false): com.apple.OTATaskingAgent".to_string()), "{lines:?}");
    // without the OS volume's files only launchd's own line says a job may not be disabled
    let (_, problems) = bootcheck::check(&c.data, None, true);
    assert_eq!(problems, ["2 disabled jobs were started: com.apple.OTATaskingAgent, com.apple.watchdogd"]);
}

#[test]
fn bootcheck_what_launchd_asks_of_the_override_file() {
    let mut c = Case::new("boot-owner");
    booted(&mut c, "");
    let mode = fs::metadata(c.data.join(bootcheck::OVERRIDE)).unwrap().permissions().mode();
    assert!(bootcheck::accepted(bootcheck::Stat { mode, uid: 0 }).is_none());
    assert!(bootcheck::accepted(bootcheck::Stat { mode, uid: 501 }).unwrap().contains("uid 501"));
    assert!(bootcheck::accepted(bootcheck::Stat { mode: mode | 0o020, uid: 0 }).unwrap().contains("group and others"));
    if !as_root() {
        let (_, problems) = bootcheck::check(&c.data, Some(&c.root), false);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("root owns it"), "{problems:?}");
    }
}

#[test]
fn bootcheck_no_log_and_the_command() {
    let mut c = Case::new("boot-nolog");
    c.apply(&[]);
    let (_, problems) = bootcheck::check(&c.data, Some(&c.root), true);
    assert!(problems[0].contains("launchd.log"), "{problems:?}");
    booted(&mut c, "");
    let (lines, _) = bootcheck::check(&c.data, Some(&c.root), true);
    assert!(lines.iter().any(|l| l.contains("the policy is in force")));
    let status = oneios_tweaks::run(&["boot-check".into(), "--data".into(), c.data.to_string_lossy().into_owned(), "--root".into(), c.root.to_string_lossy().into_owned(), "--ignore-owner".into()]);
    assert_eq!(status, 0);
}

// ---------------------------------------------------------------- MachO

#[test]
fn macho_the_cdhash_is_the_hash_of_the_code_directory() {
    let (data, cdhash) = signed("com.example.tool");
    let slices = macho::describe(&data).unwrap();
    assert_eq!(slices.len(), 1);
    let s = &slices[0];
    assert_eq!((s.cdhash.as_str(), s.identifier.as_str(), s.arch.as_str(), s.page_size, s.adhoc, s.platform.as_deref(), s.minos.as_deref()), (cdhash.as_str(), "com.example.tool", "arm64e", 4096, true, Some("ios"), Some("27.0")));
    assert_eq!(cdhash.len(), 40);
    let (_, other) = signed_macho("com.example.tool", true, 12, b"other code");
    assert_ne!(cdhash, other);
}

#[test]
fn macho_fat_files_and_files_that_are_not_signed() {
    let (a, hash_a) = signed("a");
    let (b, hash_b) = signed_macho("b", false, 12, b"code");
    let mut fat = Vec::new();
    for v in [0xCAFEBABEu32, 2, 0x0100000C, 2, 48, a.len() as u32, 0, 0x0100000C, 0, 48 + a.len() as u32, b.len() as u32, 0] {
        fat.extend_from_slice(&v.to_be_bytes());
    }
    fat.extend_from_slice(&a);
    fat.extend_from_slice(&b);
    let got: Vec<(String, String)> = macho::describe(&fat).unwrap().into_iter().map(|s| (s.cdhash, s.arch)).collect();
    assert_eq!(got, [(hash_a, "arm64e".to_string()), (hash_b, "arm64".to_string())]);
    let mut unsigned = Vec::new();
    for v in [0xFEEDFACFu32, 0x0100000C, 0, 2, 0, 0, 0, 0] {
        unsigned.extend_from_slice(&v.to_le_bytes());
    }
    assert!(macho::describe(&unsigned).is_err());
    assert!(macho::describe(&a[..a.len() - 10]).is_err());
    assert!(!macho::is_macho(b"\x7fELF...."));
}

/// The S24 Ultra's Metal preflight: no bundle passes (the phone's Make iOS image never has one: the add-on reaches the
/// phone on its own), a bundle built for this image passes, and one built for another image or with no record is
/// refused (merged from orumanayurpanjayat-ux/OneiOS 3311e38, 2026-11-24, with the missing-bundle refusal taken out).
#[test]
fn the_metal_preflight_refuses_only_a_mismatched_bundle() {
    let dir = std::env::temp_dir().join(format!("oneios-metal-preflight-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let bundle = dir.join("NativeAdrenoMetal.bundle");
    fs::create_dir_all(&bundle).unwrap();
    assert!(oneios_tweaks::preflight_metal("iPhone18,2", "24A446", None, false).is_ok());
    assert!(oneios_tweaks::preflight_metal("iPhone18,2", "24A446", Some(&dir.join("absent.bundle")), false).is_ok());
    // a bundle with no target or record is refused for the S24 Ultra
    assert!(oneios_tweaks::preflight_metal("iPhone18,2", "24A446", Some(&bundle), false).is_err());
    fs::write(bundle.join("oneios-target"), "iPhone18,2 24A446\n").unwrap();
    assert!(oneios_tweaks::preflight_metal("iPhone18,2", "24A446", Some(&bundle), false).is_err()); // no record
    fs::write(dir.join("NativeAdrenoMetal.build.json"), r#"{"target": "iPhone18,2 24A446"}"#).unwrap();
    assert!(oneios_tweaks::preflight_metal("iPhone18,2", "24A446", Some(&bundle), false).is_ok());
    // built for another image
    assert!(oneios_tweaks::preflight_metal("iPhone18,2", "24B5070a", Some(&bundle), false).is_err());
    // the explicit CPU-only image places no bundle and checks nothing
    assert!(oneios_tweaks::preflight_metal("iPhone18,2", "24B5070a", Some(&bundle), true).is_ok());
    let _ = fs::remove_dir_all(&dir);
}
