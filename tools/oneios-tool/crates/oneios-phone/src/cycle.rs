//! `oneios-tool cycle`: one test cycle on the phone (docs/building.md section 6): build what changed, the host
//! tests, stage what differs from the phone, switch, watch the boot's markers, hold, return, collect the logs.
//! Every step can be left out (`--only`, `--skip`); `--dry-run` says what each would do and touches nothing.
//!
//! **Not run on the phone or with real builds yet**: tested with a stand-in checkout (its build scripts are
//! stand-ins) and the stand-in phone, `tests/cycle/run.sh`.

use crate::adb::{last_lines, Adb, STAGE};
use crate::log::{catch_stop, sleep_or_stop, stopped, Mark, Markers};
use crate::{app, phone};
use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

pub const STEPS: [&str; 8] = ["build", "tests", "stage", "switch", "watch", "hold", "return", "collect"];
/// The iOS build whose kernel collection and image set this is (kernel/prelink/build.sh's BUILD).
const BUILD: &str = "24A446";

#[derive(clap::Args)]
pub struct Args {
    /// Only these steps (comma separated): build, tests, stage, switch, watch, hold, return, collect
    #[arg(long, value_delimiter = ',')]
    only: Vec<String>,
    /// Not these steps
    #[arg(long, value_delimiter = ',')]
    skip: Vec<String>,
    /// Say what each step would do; nothing is built and the phone is not touched
    #[arg(long)]
    dry_run: bool,
    /// The session's name: out/session-logs/DATE/NAME (default cycle-HHMMSS)
    #[arg(long)]
    name: Option<String>,
    /// build: the hosted kernel's tree (HOSTED_OUT; out/kernel/current-hosted-NAME). Without it no kernel is built
    #[arg(long)]
    kernel_out: Option<String>,
    /// build: make the runtime package of this version (runtime/package.sh VERSION) and stage it
    #[arg(long)]
    runtime_version: Option<String>,
    /// stage: an unpacked runtime package that is built already
    #[arg(long)]
    runtime: Option<String>,
    /// stage: files of the image set at their paths (default: kernel.kc, its hash list, manifest.json and
    /// xnuhost.conf of out/kernel/24A446 and out/image/24A446); iOS plug-ins built elsewhere go here too
    #[arg(long)]
    image: Option<String>,
    /// build: also these parts whatever their state: kernel, prelink, runtime, plugins (macOS), app
    #[arg(long, value_delimiter = ',')]
    force: Vec<String>,
    /// tests: run runtime/host-tests.sh before staging
    #[arg(long)]
    tests: bool,
    /// hold: seconds to keep the session after it is ready (default: until Ctrl-C)
    #[arg(long)]
    hold: Option<u64>,
    /// hold: Ctrl-C leaves the session open instead of returning
    #[arg(long)]
    stay: bool,
    /// watch: seconds to wait for the first frame before returning
    #[arg(long, default_value_t = 300)]
    ready_timeout: u64,
    /// collect: also copy the Data store (phone pull-data) and run ioslog on it
    #[arg(long)]
    pull_data: bool,
    /// The phone's adb serial (or ANDROID_SERIAL)
    #[arg(long, short = 's')]
    serial: Option<String>,
    /// The OneOS checkout, for the app's version
    #[arg(long)]
    oneos: Option<String>,
}

// ---------------------------------------------------------------- where things are

/// The OneiOS checkout: ONEIOS_ROOT, or the folder at or above the current one that has scripts/bg.sh and
/// oneos/interface.md.
pub fn repo_root() -> Result<PathBuf> {
    if let Some(root) = std::env::var("ONEIOS_ROOT").ok().filter(|r| !r.is_empty()) {
        return Ok(PathBuf::from(root));
    }
    let mut dir = std::env::current_dir().context("the current folder")?;
    loop {
        if dir.join("scripts/bg.sh").is_file() && dir.join("oneos/interface.md").is_file() {
            return Ok(dir);
        }
        if !dir.pop() {
            bail!("not inside a OneiOS checkout (no scripts/bg.sh and oneos/interface.md above the current folder; or ONEIOS_ROOT=)");
        }
    }
}

/// The main checkout of a worktree (its out/ is shared: the phone lock, the sym index); `root` itself otherwise.
pub fn main_checkout(root: &Path) -> PathBuf {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| Path::new(String::from_utf8_lossy(&o.stdout).trim()).parent().map(Path::to_path_buf))
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| root.to_path_buf())
}

/// (YYYY-MM-DD, HHMMSS) of this machine's clock.
pub fn local_date_time() -> (String, String) {
    // Safety: time and localtime_r only write the two values handed to them.
    let tm = unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        tm
    };
    (format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday), format!("{:02}{:02}{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec))
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path).with_context(|| path.display().to_string())?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h).with_context(|| path.display().to_string())?;
    Ok(oneios_common::hex(&h.finalize()))
}

fn newer(a: &Path, b: &Path) -> bool {
    match (fs::metadata(a).and_then(|m| m.modified()), fs::metadata(b).and_then(|m| m.modified())) {
        (Ok(a), Ok(b)) => a > b,
        (Ok(_), Err(_)) => true,
        _ => false,
    }
}

/// One phone, one cycle: an exclusive lock on out/phone.lock of the main checkout, held until the cycle ends.
fn phone_lock(root: &Path, name: &str) -> Result<fs::File> {
    let dir = main_checkout(root).join("out");
    fs::create_dir_all(&dir).with_context(|| dir.display().to_string())?;
    let path = dir.join("phone.lock");
    let file = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path).with_context(|| path.display().to_string())?;
    // Safety: the descriptor is open; flock only takes the lock.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        bail!("the phone is taken: {} is locked by another cycle ({})", path.display(), fs::read_to_string(&path).unwrap_or_default().trim());
    }
    file.set_len(0).ok();
    fs::write(&path, format!("pid {} cycle {name}\n", std::process::id())).ok();
    Ok(file)
}

// ---------------------------------------------------------------- the run

struct Cycle<'a> {
    a: &'a Args,
    root: PathBuf,
    name: String,
    out: PathBuf,
    dry: bool,
    /// (step, seconds, how it ended)
    times: Vec<(String, f64, String)>,
    notes: Vec<String>,
    markers: Markers,
    marker_times: Vec<(f64, String)>,
    lines_seen: u64,
    switched: Option<Instant>,
    runtime_dir: Option<PathBuf>,
    image_dir: Option<PathBuf>,
    way_back: String,
}

impl Cycle<'_> {
    fn say(&self, text: &str) {
        println!("{}{text}", if self.dry { "would: " } else { "" });
    }

    /// A long job in tmux (scripts/bg.sh), waited for; its exit code must be 0.
    fn job(&mut self, job: &str, cmd: &[String]) -> Result<()> {
        let shown = format!("scripts/bg.sh {job} {}", cmd.join(" "));
        self.say(&shown);
        if self.dry {
            return Ok(());
        }
        let exit = self.root.join(format!("logs/{job}.exit"));
        let status = Command::new(self.root.join("scripts/bg.sh")).arg(job).args(cmd).current_dir(&self.root).status().context("scripts/bg.sh")?;
        if !status.success() {
            bail!("scripts/bg.sh {job} did not start ({status})");
        }
        let start = Instant::now();
        let mut said = 0;
        while !exit.is_file() {
            if sleep_or_stop(Duration::from_secs(1)) {
                let _ = Command::new("tmux").args(["kill-session", "-t", &format!("oneios-{job}")]).status();
                bail!("stopped by Ctrl-C; the job {job} was ended");
            }
            let minutes = start.elapsed().as_secs() / 60;
            if minutes > said {
                said = minutes;
                println!("  {job}: running for {minutes} min (logs/{job}.log)");
            }
        }
        let code = fs::read_to_string(&exit).unwrap_or_default().trim().to_string();
        let log = fs::read_to_string(self.root.join(format!("logs/{job}.log"))).unwrap_or_default();
        if code != "0" {
            bail!("{job} ended with exit code {code}: {}", last_lines(&log, 4));
        }
        println!("  {job}: exit 0 after {} s: {}", start.elapsed().as_secs(), last_lines(&log, 1));
        Ok(())
    }

    // ------------------------------------------------------------ build

    fn build(&mut self) -> Result<()> {
        let forced = |part: &str| self.a.force.iter().any(|f| f == part);
        let root = self.root.clone();
        let kc = root.join(format!("out/kernel/{BUILD}/kernel.kc"));
        // the hosted kernel: when its tree is named and older than the newest patch (or forced)
        let tree = self.a.kernel_out.as_ref().map(|n| root.join(format!("out/kernel/current-hosted-{n}")));
        if let (Some(name), Some(tree)) = (&self.a.kernel_out, &tree) {
            let macho = tree.join("xnu.macho");
            let newest = fs::read_dir(root.join("kernel/patches/oneios-hosted")).ok().and_then(|d| d.flatten().filter_map(|e| e.metadata().ok()?.modified().ok()).max());
            let stale = match (fs::metadata(&macho).and_then(|m| m.modified()), newest) {
                (Ok(built), Some(patch)) => patch > built,
                (Err(_), _) => true,
                _ => false,
            };
            if stale || forced("kernel") {
                self.job("cycle-kernel", &["env".into(), format!("HOSTED_OUT={name}"), "XNU_FILESET_REFERENCE=1".into(), "kernel/build-hosted.sh".into()])?;
            } else {
                self.say(&format!("kernel: up to date ({} is newer than every patch)", macho.display()));
            }
        } else {
            self.say("kernel: not built (no --kernel-out NAME)");
        }
        // the collection: when the kernel is newer than it; the audit and the stubs must be clean
        if let Some(tree) = &tree {
            let macho = tree.join("xnu.macho");
            if newer(&macho, &kc) || forced("prelink") || forced("kernel") || (self.dry && !kc.is_file()) {
                let mut cmd = vec!["env".to_string(), format!("KERNEL={}", macho.display())];
                let reference = tree.join("reference.macho");
                if reference.is_file() || self.dry {
                    cmd.push(format!("REFERENCE={}", reference.display()));
                }
                cmd.push("kernel/prelink/build.sh".into());
                self.job("cycle-prelink", &cmd)?;
                self.say("the audit: kernel/prelink/build.sh stops on a failure; then python3 kernel/prelink/stubs-check.py (every stub good, or the cycle stops)");
                if !self.dry {
                    let out = Command::new("python3").arg(root.join("kernel/prelink/stubs-check.py")).arg(&kc).current_dir(&root).output().context("stubs-check.py")?;
                    let text = String::from_utf8_lossy(&out.stdout).to_string();
                    let line = text.lines().find(|l| l.starts_with("stubs: ")).unwrap_or("").to_string();
                    println!("  {line}");
                    if !out.status.success() || !line.ends_with(", 0 bad") {
                        bail!("the collection's stubs are not all good: {}", last_lines(&text, 4));
                    }
                    self.notes.push(format!("prelink: audit 0 failures (build.sh exit 0), {line}"));
                }
            } else {
                self.say(&format!("prelink: up to date ({} is newer than the kernel)", kc.display()));
            }
        }
        // the runtime package
        if let Some(version) = &self.a.runtime_version {
            self.job("cycle-runtime", &["runtime/package.sh".to_string(), version.clone()])?;
            let dir = root.join(format!("out/cycle/runtime-{version}"));
            let pack = root.join(format!("out/release/oneios-runtime-{version}-arm64.tar.zst"));
            self.say(&format!("unpack {} into {}", pack.display(), dir.display()));
            if !self.dry {
                let _ = fs::remove_dir_all(&dir);
                fs::create_dir_all(&dir)?;
                let status = Command::new("sh").arg("-c").arg("zstd -dc \"$1\" | tar -xf - -C \"$2\"").arg("sh").arg(&pack).arg(&dir).status()?;
                if !status.success() || !dir.join("runtime.json").is_file() {
                    bail!("unpacking {} gave no runtime.json", pack.display());
                }
            }
            self.runtime_dir = Some(dir);
        } else if let Some(dir) = &self.a.runtime {
            self.say(&format!("runtime: the given package {dir}"));
            self.runtime_dir = Some(PathBuf::from(dir));
        } else {
            self.say("runtime: not built (no --runtime-version V) and none given (--runtime DIR)");
        }
        // the image set's parts that a cycle stages
        if let Some(dir) = &self.a.image {
            self.say(&format!("image parts: the given folder {dir}"));
            self.image_dir = Some(PathBuf::from(dir));
        } else {
            let set = root.join(format!("out/image/{BUILD}"));
            let parts = [(kc.clone(), "kernel.kc"), (kc.with_file_name("kernel.kc.hashes"), "kernel.kc.hashes"), (set.join("manifest.json"), "manifest.json"), (set.join("overlay/userland/xnuhost.conf"), "overlay/userland/xnuhost.conf")];
            let dir = root.join("out/cycle/image");
            let have: Vec<&(PathBuf, &str)> = parts.iter().filter(|(src, _)| src.is_file()).collect();
            self.say(&format!("image parts: {} into {}", if have.is_empty() { "none built here".to_string() } else { have.iter().map(|p| p.1).collect::<Vec<_>>().join(", ") }, dir.display()));
            if !self.dry && !have.is_empty() {
                let _ = fs::remove_dir_all(&dir);
                for (src, rel) in have {
                    let dst = dir.join(rel);
                    fs::create_dir_all(dst.parent().expect("a file's folder"))?;
                    // a hard link where it can be: the collection is 90 MB
                    if fs::hard_link(src, &dst).is_err() {
                        fs::copy(src, &dst).with_context(|| dst.display().to_string())?;
                    }
                }
                self.image_dir = Some(dir);
            }
        }
        // the iOS plug-ins: Xcode builds them
        if forced("plugins") {
            if cfg!(target_os = "macos") {
                self.job("cycle-metal", &["userland/metal/build.sh".to_string()])?;
                self.job("cycle-camera", &["userland/camera/build.sh".to_string()])?;
            } else {
                bail!("the iOS plug-ins build on macOS only: build them there and give their files in --image DIR at their paths in the set");
            }
        } else {
            self.say("plug-ins: not built (macOS only, with --force plugins; from elsewhere: their files in --image DIR)");
        }
        // the app: said, and built only when asked
        match app::find_fork(self.a.oneos.as_deref()).and_then(|f| app::source_version(&f).map(|v| (f, v))) {
            Err(e) => self.say(&format!("app: no source to compare ({e:#})")),
            Ok((fork, v)) => {
                if forced("app") {
                    self.say(&format!("app: oneios-tool app build && app install ({} {})", v.name, v.code));
                    if !self.dry {
                        app::build(&fork, false)?;
                        app::install(Some(&fork), self.a.serial.as_deref(), None, None, "akshaynexus/OneOS", Path::new("out/app"))?;
                    }
                } else {
                    self.say(&format!("app: the source is {} (versionCode {}); the stage step compares it with the installed app (--force app builds and installs it)", v.name, v.code));
                    self.notes.push(format!("app source: {} (versionCode {})", v.name, v.code));
                }
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ stage

    /// `phone sums` as a map: "image/kernel.kc" -> sha256, "data.seed" -> sha256, "runtime/<path>" -> sha256,
    /// and runtime.conf's "key=value" lines as "conf:key" -> value.
    fn sums(text: &str) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        for line in text.lines() {
            if let Some((key, value)) = line.split_once('=').filter(|(k, _)| !k.contains(' ')) {
                map.insert(format!("conf:{key}"), value.to_string());
            } else if let Some((sha, path)) = line.split_once("  ") {
                let key = if path.starts_with("image/") || path == "data.seed" { path.to_string() } else { format!("runtime/{path}") };
                map.insert(key, sha.to_string());
            }
        }
        map
    }

    /// What of the local files differs from the phone's: (image files to stage, whether the runtime differs).
    fn differences(&self, phone: &BTreeMap<String, String>) -> Result<(Vec<String>, bool)> {
        let mut image = Vec::new();
        if let Some(dir) = &self.image_dir {
            for rel in ["kernel.kc", "kernel.kc.hashes", "manifest.json", "overlay/userland/xnuhost.conf"] {
                let path = dir.join(rel);
                if path.is_file() && phone.get(&format!("image/{rel}")) != Some(&sha256_file(&path)?) {
                    image.push(rel.to_string());
                }
            }
            if dir.join("data.seed").exists() {
                println!("  data.seed in {} is never staged by a cycle: a new seed starts the phone's Data store over", dir.display());
            }
        }
        let mut runtime = false;
        if let Some(dir) = &self.runtime_dir {
            let raw = fs::read(dir.join("runtime.json")).with_context(|| format!("{} has no runtime.json", dir.display()))?;
            let j: serde_json::Value = serde_json::from_slice(&raw).context("runtime.json")?;
            runtime = phone.get("runtime/runtime.json") != Some(&oneios_common::hex(&Sha256::digest(&raw)));
            for f in j.get("files").and_then(|f| f.as_array()).into_iter().flatten() {
                if let (Some(path), Some(sha)) = (f.get("path").and_then(|p| p.as_str()), f.get("sha256").and_then(|s| s.as_str())) {
                    runtime |= phone.get(&format!("runtime/{path}")).map(String::as_str) != Some(sha);
                }
            }
        }
        Ok((image, runtime))
    }

    fn stage(&mut self, adb: &Adb) -> Result<()> {
        if self.image_dir.is_none() && self.runtime_dir.is_none() {
            // the build step did not run: the same choices, without building
            self.image_dir = self.a.image.as_ref().map(PathBuf::from).or_else(|| Some(self.root.join("out/cycle/image")).filter(|d| d.is_dir()));
            self.runtime_dir = self.a.runtime.as_ref().map(PathBuf::from);
        }
        let before = Self::sums(&phone::sums_text(adb)?);
        let (image, runtime) = self.differences(&before)?;
        println!("  against the phone's sums: {}; the runtime {}", if image.is_empty() { "no image file differs".to_string() } else { format!("{} differ", image.join(", ")) }, match (&self.runtime_dir, runtime) {
            (None, _) => "is not given",
            (_, true) => "differs",
            _ => "is the same",
        });
        if !image.is_empty() {
            // only the files that differ, at their paths
            let part = self.root.join(format!("out/cycle/stage-{}", self.name));
            let _ = fs::remove_dir_all(&part);
            for rel in &image {
                let dst = part.join(rel);
                fs::create_dir_all(dst.parent().expect("a file's folder"))?;
                let src = self.image_dir.as_ref().expect("image files").join(rel);
                if fs::hard_link(&src, &dst).is_err() {
                    fs::copy(&src, &dst)?;
                }
            }
            phone::stage(adb, "image", &part.display().to_string())?;
            let _ = fs::remove_dir_all(&part);
        }
        if runtime {
            phone::stage(adb, "runtime", &self.runtime_dir.as_ref().expect("a runtime").display().to_string())?;
        }
        // then the phone's sums against the local hashes
        let text = phone::sums_text(adb)?;
        fs::write(self.out.join("sums.txt"), &text)?;
        let after = Self::sums(&text);
        let (image, runtime) = self.differences(&after)?;
        if !image.is_empty() || runtime {
            bail!("after staging the phone's sums still differ from the local files: {}{}", image.join(", "), if runtime { " the runtime" } else { "" });
        }
        self.notes.push(format!("staged: kernel.kc {}, runtime {}", after.get("image/kernel.kc").map(|s| &s[..16]).unwrap_or("?"), after.get("conf:version").map(String::as_str).unwrap_or("?")));
        println!("  the phone's sums are the local files' (sums.txt)");
        if let Ok(Some((code, name))) = phone::installed_app(adb, phone::APP_ID) {
            self.notes.push(format!("app installed: {name} (versionCode {code})"));
        }
        Ok(())
    }

    // ------------------------------------------------------------ switch

    /// Before One UI is stopped: the way back must be there.
    fn way_back_ready(&self, adb: &Adb) -> Result<()> {
        adb.need_root()?;
        adb.refuse_in_session()?;
        let script = format!(
            "cd {STAGE} || {{ echo 'no stage'; exit 0; }}\n[ -f oneios_return.sh ] || echo 'oneios_return.sh is not staged'\n[ -f oneios_switch.sh ] || echo 'oneios_switch.sh is not staged'\n[ -f bin/oneios_root.sh ] || echo 'bin/oneios_root.sh is not staged'\n[ ! -e runtime.new ] || echo 'an import of a runtime is in progress (runtime.new)'\n[ ! -e image.new ] || echo 'an import of an image is in progress (image.new)'\n"
        );
        let wrong = adb.root_ok(&script, "the check of the way back")?;
        if !wrong.trim().is_empty() {
            bail!("no switch: {} (oneios-tool app stage-scripts stages the scripts)", wrong.trim().replace('\n', "; "));
        }
        let status = phone::root_status(adb)?;
        match status.iter().find(|l| l.starts_with("@@RESULT ready ")) {
            Some(line) if line.starts_with("@@RESULT ready 1") => Ok(()),
            Some(line) => bail!("no switch: the preflight refuses: {}", line.trim_start_matches("@@RESULT ready 0").trim()),
            None => bail!("no switch: oneios_root.sh status gave no ready line: {}", last_lines(&status.join("\n"), 2)),
        }
    }

    // ------------------------------------------------------------ watch and hold

    /// The log's new lines since the last look; the markers among them are printed. A failed look is not an error
    /// (adb drops while the framework stops).
    fn look(&mut self, adb: &Adb) {
        let Ok(r) = adb.root(&format!("tail -n +{} {STAGE}/session/xnuhost.log 2>/dev/null\n", self.lines_seen + 1)) else { return };
        if !r.ok() {
            return;
        }
        let since = self.switched.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
        // only whole lines count
        let whole = r.out.rfind('\n').map(|end| &r.out[..end]).unwrap_or("");
        for line in whole.lines() {
            self.lines_seen += 1;
            if let Some(mark) = self.markers.line(line) {
                let what = match &mark {
                    Mark::Stage(name) => (*name).to_string(),
                    Mark::Panic => "PANIC".to_string(),
                    Mark::End => "end".to_string(),
                };
                println!("  +{since:5.0} s  {what:11} {line}");
                self.marker_times.push((since, format!("{what}: {line}")));
            }
        }
    }

    fn ended(&self) -> Option<&'static str> {
        if self.markers.found.iter().any(|(m, _)| *m == Mark::Panic) {
            return Some("a panic");
        }
        self.markers.found.iter().any(|(m, _)| *m == Mark::End).then_some("the session ended by itself")
    }

    /// Until the first frame, a panic, the session's own end, Ctrl-C or the time limit. Ok(true): ready.
    fn watch(&mut self, adb: &Adb) -> Result<bool> {
        let start = Instant::now();
        loop {
            self.look(adb);
            if self.markers.ready() {
                println!("  ready after {:.0} s", start.elapsed().as_secs_f64());
                return Ok(true);
            }
            if let Some(why) = self.ended() {
                self.notes.push(format!("watch: {why} before the first frame"));
                println!("  {why}: returning");
                return Ok(false);
            }
            if start.elapsed().as_secs() >= self.a.ready_timeout {
                self.notes.push(format!("watch: no first frame within {} s", self.a.ready_timeout));
                println!("  no first frame within {} s: returning", self.a.ready_timeout);
                return Ok(false);
            }
            if sleep_or_stop(Duration::from_secs(2)) {
                self.notes.push("watch: stopped by Ctrl-C".into());
                return Ok(false);
            }
        }
    }

    /// Keep the session: `--hold S` seconds, or until Ctrl-C. Ok(false): leave it open (Ctrl-C with --stay).
    fn hold(&mut self, adb: &Adb) -> Result<bool> {
        let start = Instant::now();
        match self.a.hold {
            Some(s) => println!("  holding the session for {s} s (Ctrl-C returns{})", if self.a.stay { "; with --stay it leaves the session open" } else { "" }),
            None => println!("  holding the session until Ctrl-C{}", if self.a.stay { " (--stay: the session is left open)" } else { " (which returns to One UI)" }),
        }
        loop {
            if self.a.hold.is_some_and(|s| start.elapsed().as_secs() >= s) {
                return Ok(true);
            }
            if sleep_or_stop(Duration::from_secs(2)) {
                self.notes.push(format!("hold: Ctrl-C after {} s", start.elapsed().as_secs()));
                return Ok(!self.a.stay);
            }
            self.look(adb);
            if let Some(why) = self.ended() {
                self.notes.push(format!("hold: {why} after {} s", start.elapsed().as_secs()));
                println!("  {why}: returning");
                return Ok(true);
            }
        }
    }

    // ------------------------------------------------------------ collect

    fn collect(&mut self, adb: &Adb) -> Result<()> {
        for (name, file) in [("xnuhost.log", "session/xnuhost.log"), ("switch.log", "switch.log"), ("return.log", "return.log"), ("return.outcome", "return.outcome")] {
            // the last 2000 lines of the fork's own logs, the whole session log
            let cmd = if name == "xnuhost.log" || name == "return.outcome" { format!("su -c 'cat {STAGE}/{file}'") } else { format!("su -c 'tail -n 2000 {STAGE}/{file}'") };
            let mut child = adb.exec_out(&cmd)?;
            let mut out = fs::File::create(self.out.join(name))?;
            std::io::copy(child.stdout.as_mut().expect("piped"), &mut out)?;
            let ok = child.wait()?.success();
            println!("  {name}: {} bytes{}", out.metadata()?.len(), if ok { "" } else { " (not there)" });
        }
        if !self.out.join("sums.txt").exists() && !adb.session_open().unwrap_or(true) {
            if let Ok(text) = phone::sums_text(adb) {
                fs::write(self.out.join("sums.txt"), text)?;
            }
        }
        if self.a.pull_data {
            phone::pull_data(adb, &self.out.join("data.img").display().to_string(), true)?;
        }
        Ok(())
    }

    fn summary(&self, result: &Result<()>) -> Result<()> {
        let mut s = format!("# Cycle {}\n\n", self.name);
        let (date, time) = local_date_time();
        s += &format!("- written {date} {time} by oneios-tool cycle; the result: {}\n", match result {
            Ok(()) => "every step that ran ended well".to_string(),
            Err(e) => format!("**stopped**: {e:#}"),
        });
        let commit = Command::new("git").arg("-C").arg(&self.root).args(["rev-parse", "--short", "HEAD"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        s += &format!("- OneiOS checkout: {} at {commit}\n", self.root.display());
        s += "\n## Versions and hashes\n\n";
        for note in &self.notes {
            s += &format!("- {note}\n");
        }
        if let Ok(sums) = fs::read_to_string(self.out.join("sums.txt")) {
            s += "\nThe phone's sums (`phone sums`, after staging):\n\n```\n";
            s += &sums;
            s += "```\n";
        }
        s += "\n## Steps\n\n| Step | Seconds | Result |\n|---|---|---|\n";
        for (step, seconds, how) in &self.times {
            s += &format!("| {step} | {seconds:.1} | {how} |\n");
        }
        s += "\n## Markers (seconds after the switch was asked)\n\n";
        if self.marker_times.is_empty() {
            s += "none seen\n";
        }
        for (t, line) in &self.marker_times {
            s += &format!("- +{t:.0} s {line}\n");
        }
        let panics: Vec<&String> = self.markers.found.iter().filter(|(m, _)| *m == Mark::Panic).map(|(_, l)| l).collect();
        s += &format!("\n## Panics\n\n{}\n", if panics.is_empty() { "none".to_string() } else { panics.iter().map(|l| format!("- `{l}`")).collect::<Vec<_>>().join("\n") });
        s += &format!("\n## The way back\n\n{}\n", if self.way_back.is_empty() { "no return was made by this cycle" } else { &self.way_back });
        fs::write(self.out.join("summary.md"), s).context("summary.md")
    }

    // ------------------------------------------------------------ the steps in order

    fn timed(&mut self, step: &str, start: Instant, result: &Result<()>) {
        let how = match result {
            Ok(()) => "ok".to_string(),
            Err(e) => format!("failed: {e:#}"),
        };
        self.times.push((step.to_string(), start.elapsed().as_secs_f64(), how));
    }

    fn run(&mut self, steps: &[&str]) -> Result<()> {
        let on = |step: &str| steps.contains(&step);
        let phone_steps = ["stage", "switch", "watch", "hold", "return", "collect"].iter().any(|s| on(s));
        if on("build") {
            println!("== build");
            let start = Instant::now();
            let r = self.build();
            self.timed("build", start, &r);
            r?;
        }
        if on("tests") {
            println!("== tests");
            let start = Instant::now();
            let r = if self.a.tests { self.job("cycle-tests", &["runtime/host-tests.sh".to_string()]) } else {
                self.say("tests: not asked (--tests runs runtime/host-tests.sh)");
                Ok(())
            };
            self.timed("tests", start, &r);
            r?;
        }
        if !phone_steps {
            return Ok(());
        }
        if self.dry {
            for (step, what) in [
                ("stage", "phone sums; stage image (only the files whose sha256 differs) and stage runtime (when a file of it differs); phone sums again, which must equal the local hashes"),
                ("switch", "check the way back (root, no session, the return script staged, no import in progress, the preflight says ready), then phone switch with the installed app's versionCode"),
                ("watch", "read session/xnuhost.log's new lines every 2 s and print the markers (kernel, launchd, backboardd, SpringBoard, the first frame, a panic); return on a panic or after --ready-timeout without the first frame"),
                ("hold", "keep the session for --hold seconds, or until Ctrl-C (which returns unless --stay)"),
                ("return", "write session/return.request and wait for One UI's restore line"),
                ("collect", "xnuhost.log, switch.log, return.log, return.outcome, sums.txt and summary.md into the session's folder (--pull-data: also the Data store and ioslog)"),
            ] {
                if on(step) {
                    println!("== {step}\nwould: {what}");
                }
            }
            println!("would write: {}", self.out.display());
            return Ok(());
        }
        fs::create_dir_all(&self.out).with_context(|| self.out.display().to_string())?;
        let _lock = phone_lock(&self.root, &self.name)?;
        let adb = Adb::connect(self.a.serial.as_deref())?;
        println!("phone: {} (the lock is taken); the logs go to {}", adb.serial, self.out.display());
        let result = self.phone_steps(&adb, steps);
        if on("collect") {
            println!("== collect");
            let start = Instant::now();
            let r = self.collect(&adb);
            self.timed("collect", start, &r);
            if let Err(e) = &r {
                eprintln!("collect: {e:#}");
            }
            self.summary(&result)?;
            println!("  summary.md in {}", self.out.display());
        }
        result
    }

    fn phone_steps(&mut self, adb: &Adb, steps: &[&str]) -> Result<()> {
        let on = |step: &str| steps.contains(&step);
        if on("stage") {
            println!("== stage");
            let start = Instant::now();
            let r = self.stage(adb);
            self.timed("stage", start, &r);
            r?;
        }
        if on("switch") {
            println!("== switch");
            let start = Instant::now();
            let r = self.way_back_ready(adb).and_then(|()| phone::switch(adb, None).map(|_| ()));
            self.timed("switch", start, &r);
            r?;
            self.switched = Some(start);
        }
        // from here on a session may be open: whatever happens, the way back is taken (unless --stay left it open)
        let mut back = true;
        let mut failure = None;
        if on("watch") {
            println!("== watch");
            let start = Instant::now();
            let r = self.watch(adb);
            self.timed("watch", start, &r.as_ref().map(|_| ()).map_err(|e| anyhow!("{e:#}")));
            match r {
                Ok(true) => {
                    if on("hold") {
                        println!("== hold");
                        let start = Instant::now();
                        back = self.hold(adb).unwrap_or(true);
                        self.timed("hold", start, &Ok(()));
                    }
                }
                Ok(false) => failure = Some(anyhow!("the boot did not reach its first frame ({})", self.notes.last().cloned().unwrap_or_default())),
                Err(e) => failure = Some(e),
            }
        } else if on("hold") {
            println!("== hold");
            let start = Instant::now();
            back = self.hold(adb).unwrap_or(true);
            self.timed("hold", start, &Ok(()));
        }
        if on("return") && back {
            println!("== return");
            let start = Instant::now();
            let r = phone::return_to_one_ui(adb, &format!("oneios-tool cycle {}", self.name), 180);
            let outcome = adb.root(&format!("cat {STAGE}/return.outcome\n")).map(|r| r.out.trim().to_string()).unwrap_or_default();
            match &r {
                Ok(phone::Returned::Restored(seconds, line)) => {
                    self.way_back = format!("{line} ({seconds} s after the request); return.outcome: {outcome}");
                    println!("  {}", self.way_back);
                }
                Ok(phone::Returned::Nothing) => {
                    self.way_back = format!("no session was open when the return was due; return.outcome: {outcome}");
                    println!("  {}", self.way_back);
                }
                Err(e) => self.way_back = format!("**the return did not finish**: {e:#}"),
            }
            let r = r.map(|_| ());
            self.timed("return", start, &r);
            r?;
        } else if !back {
            self.way_back = "the session was left open (--stay): end it with oneios-tool phone return".into();
            println!("  {}", self.way_back);
        }
        match failure {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

pub fn main(a: Args) -> i32 {
    let run = || -> Result<()> {
        for s in a.only.iter().chain(&a.skip) {
            if !STEPS.contains(&s.as_str()) {
                bail!("{s} is not a step ({})", STEPS.join(", "));
            }
        }
        let steps: Vec<&str> = STEPS.iter().copied().filter(|s| (a.only.is_empty() || a.only.iter().any(|o| o == s)) && !a.skip.iter().any(|k| k == s)).collect();
        let root = repo_root()?;
        let (date, time) = local_date_time();
        let name = a.name.clone().unwrap_or_else(|| format!("cycle-{time}"));
        crate::adb::plain(&name, "the session's name")?;
        let out = root.join("out/session-logs").join(date).join(&name);
        catch_stop();
        println!("cycle {name}: {}{}", steps.join(", "), if a.dry_run { " (dry run: nothing is built, the phone is not touched)" } else { "" });
        let mut c = Cycle { a: &a, root, name, out, dry: a.dry_run, times: Vec::new(), notes: Vec::new(), markers: Markers::default(), marker_times: Vec::new(), lines_seen: 0, switched: None, runtime_dir: None, image_dir: None, way_back: String::new() };
        let result = c.run(&steps);
        if stopped() && result.is_ok() {
            println!("cycle: ended by Ctrl-C");
        }
        result
    };
    match run() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("oneios-tool cycle: {e:#}");
            1
        }
    }
}
