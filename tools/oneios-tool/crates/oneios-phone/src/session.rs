//! `oneios-tool phone display-restart`, `touch`, `backlight`: what a running session needs from One UI's side and
//! had no command for (the Mac used adb and scripts three times in the session of 2026-11-14). Each runs **only
//! during a session** and is refused without one: with no session the display helper, the touch IC and the panel's
//! level are One UI's. None of them touches xnuhost or iOS, and the return works during and after each: the helper's
//! pid stays in `control/display.pid` (the return ends it by that file), and the return writes One UI's own backlight
//! value back (`control/base/backlight`, the fork's `oneios_return.sh`).
//!
//! Only named files are read and written: `control/display.pid`, `control/display.log`, `control/state`,
//! `control/base/backlight`, `/proc/PID/cmdline` of the one pid a pid file names, `/proc/PID/ns/mnt` of the script's
//! own shell and of init, `/sys/class/sec/tsp/enabled`, `/sys/class/backlight/panel0-backlight/brightness` and
//! `max_brightness`, `/sys/fs/cgroup/oneios_session/cgroup.procs`. Nothing is listed and nothing under /sys/power.

use crate::adb::{last_lines, Adb, BUSYBOX, STAGE};
use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::time::{Duration, Instant};

/// Samsung's touch IC: what One UI's input HAL writes at a panel power change, and its state when read (1: on).
pub const TSP_ENABLED: &str = "/sys/class/sec/tsp/enabled";
/// The panel's backlight node on the S24 Ultra (`max_brightness` beside it: 12750).
pub const BACKLIGHT_DIR: &str = "/sys/class/backlight/panel0-backlight";
/// Where `adb push` puts a new library or dex before root's script takes it.
const TMP: &str = "/data/local/tmp/oneios-display-tmp";

/// Every command here: root first, then a session, else a refusal that says whose the thing is.
fn need_session(adb: &Adb, what: &str) -> Result<()> {
    adb.need_root()?;
    if !adb.session_open()? {
        bail!("no session is open: {what} is One UI's while no session runs (start one: oneios-tool phone switch). Nothing was changed");
    }
    Ok(())
}

// ---------------------------------------------------------------- the display helper

pub struct Restart {
    /// A new `liboneios-display.so` for `runtime/bin/`
    pub lib: Option<String>,
    /// A new `oneios-display.dex` for `runtime/lib/`
    pub dex: Option<String>,
    /// Seconds to wait for the new helper's first frame
    pub timeout: u64,
    /// Seconds between the old helper's SIGTERM and its SIGKILL
    pub kill_after: u64,
}

/// A file of the helper that `--lib` or `--dex` replaces: its option, its path in the staged runtime, and the check
/// that it is that kind of file (an arm64 shared object, a dex).
struct Part {
    option: &'static str,
    path: &'static str,
    is: fn(&[u8]) -> bool,
    kind: &'static str,
}

const LIB: Part = Part {
    option: "--lib",
    path: "bin/liboneios-display.so",
    // "\x7fELF", 64-bit, a shared object (e_type 3), AArch64 (e_machine 183), little-endian
    is: |b| b.len() > 20 && b[..4] == *b"\x7fELF" && b[4] == 2 && b[16] == 3 && b[17] == 0 && b[18] == 0xb7 && b[19] == 0,
    kind: "an arm64 shared object",
};
const DEX: Part = Part { option: "--dex", path: "lib/oneios-display.dex", is: |b| b.starts_with(b"dex\n"), kind: "a dex" };

/// The session's state for a restart: `control/state` (the switch writes `starting`, then `running`; the return
/// `returning`, `returned`) and whether a return was asked.
pub fn state_script() -> String {
    format!("echo \"state $(cat {STAGE}/control/state 2>/dev/null)\"; [ ! -e {STAGE}/session/return.request ] || echo asked\n")
}

/// Root's script that puts one pushed file into the staged runtime: the pushed copy's SHA-256 must be `sha` before
/// anything changes, the staged file keeps its mode and is replaced by a rename, and its lines in `runtime.sha256`
/// and `runtime.ids` are written again (as `stage image` does for the image set), so that the lists the next
/// switch's preflight hashes the runtime against name the file that is there.
pub fn install_script(path: &str, sha: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let mut s = format!("cd {STAGE} || exit 2\nf=runtime/{path}; t={TMP}/{name}\n");
    // a refusal: the pushed copy and a half-made one go, the reason is the answer
    s += &format!("no() {{ rm -rf {TMP}; rm -f \"$f.part\"; echo \"@@REFUSED $1\"; exit 3; }}\n");
    s += "[ -f \"$f\" ] && [ ! -L \"$f\" ] || no \"$f is not in the staged runtime (stage a runtime that has the display helper: oneios-tool phone stage runtime DIR)\"\n";
    s += &format!("grep -q '  {path}$' runtime.sha256 || no \"runtime.sha256 does not list {path}\"\n");
    s += "p=$(sha256sum \"$t\" | cut -d' ' -f1); echo \"@@PUSHED $p\"\n";
    s += &format!("[ \"$p\" = {sha} ] || no \"the copy on the phone does not match\"\n");
    s += "m=$(stat -c %a \"$f\")\n";
    s += "cp \"$t\" \"$f.part\" && chown root:root \"$f.part\" && chmod \"$m\" \"$f.part\" || no \"the copy into the stage failed\"\n";
    s += "s=$(sha256sum \"$f.part\" | cut -d' ' -f1)\n";
    s += &format!("[ \"$s\" = {sha} ] || {{ echo \"@@STAGED $s\"; no \"the copy in the stage does not match\"; }}\n");
    s += "mv -f \"$f.part\" \"$f\" || no \"the staged file could not be replaced\"\n";
    s += &format!("awk -v p='{path}' -v s=\"$s\" '$2 == p {{ print s \"  \" p; next }} {{ print }}' runtime.sha256 > runtime.sha256.new && mv -f runtime.sha256.new runtime.sha256 && chmod 0600 runtime.sha256 || no \"runtime.sha256 could not be written\"\n");
    s += "if [ -f runtime.ids ]; then i=$(stat -c '%d:%i:%s:%Y' \"$f\")\n";
    s += &format!("awk -v p='{path}' -v i=\"$i\" '$2 == p {{ print i \" \" p; next }} {{ print }}' runtime.ids > runtime.ids.new && mv -f runtime.ids.new runtime.ids && chmod 0600 runtime.ids || no \"runtime.ids could not be written\"\nfi\n");
    s += &format!("sync; rm -rf {TMP}; echo \"@@STAGED $s $m\"\n");
    s
}

/// Root's script that ends the session's display helper and starts a new one.
///
/// The end: by `control/display.pid`, and only a pid whose `/proc/PID/cmdline` is a display helper's (the fork's
/// `end_display`): SIGTERM (the helper removes its layers and exits), and SIGKILL when it is still there after
/// `kill_after` seconds (surfaceflinger drops a dead client's layers). A helper started by hand without the pid
/// file is not looked for (that would be a walk of /proc): it leaves by itself when the new one connects, since
/// xnuhost serves one helper and closes the older one's socket.
///
/// The start is the switch script's own (`oneios_switch.sh`, "The guest's screen"; the fork has no command for it:
/// neither `oneios_root.sh` nor `oneios_switch.sh` has a mode that restarts the helper): the same file, checked the
/// same way (a regular file, executable, root's, not writable by others), the same arguments, stdin, log and pid
/// file, the same OOM score and cgroup, and init's mount namespace, which the switch itself runs in (it re-enters
/// it with `nsenter -t 1 -m` when its shell is in another, and so does this). The user is root and the
/// environment is su's, as the switch's; CLASSPATH is set by the launcher itself (`runtime/display/src/launcher.c`);
/// the switch does not change its own nice level before it starts the helper, and neither does this.
pub fn restart_script(kill_after: u64) -> String {
    let ticks = kill_after.max(1) * 5;
    let mut s = format!("S={STAGE}; C=$S/control; DH=$S/runtime/bin/oneios-display; BB={BUSYBOX}\n");
    s += "helper() { case \"$(cat /proc/$1/cmdline 2>/dev/null | tr '\\0' ' ')\" in *OneiOSDisplay*|*oneios-display*) return 0 ;; esac; return 1; }\n";
    s += "[ -f \"$DH\" ] && [ ! -L \"$DH\" ] && [ -x \"$DH\" ] || { echo \"@@REFUSED the staged runtime has no display helper (runtime/bin/oneios-display)\"; exit 3; }\n";
    s += "o=$(id -u); case \"$(stat -c '%u %A' \"$DH\" 2>/dev/null)\" in \"$o \"?????w*|\"$o \"????????w*) o= ;; \"$o \"*) ;; *) o= ;; esac\n";
    s += "[ -n \"$o\" ] || { echo \"@@REFUSED runtime/bin/oneios-display is not root's or is writable by others\"; exit 3; }\n";
    s += "for dp in $(cat \"$C/display.pid\" 2>/dev/null); do\n";
    s += "  case \"$dp\" in ''|*[!0-9]*) continue ;; esac\n";
    s += "  helper \"$dp\" || { echo \"@@OLD $dp other\"; continue; }\n";
    s += "  kill -TERM \"$dp\" 2>/dev/null\n";
    s += &format!("  n=0; while [ $n -lt {ticks} ] && helper \"$dp\"; do sleep 0.2; n=$((n + 1)); done\n");
    s += "  if helper \"$dp\"; then kill -9 \"$dp\" 2>/dev/null; echo \"@@OLD $dp killed\"; else echo \"@@OLD $dp ended\"; fi\n";
    s += "done\n";
    s += "size=$(stat -c %s \"$C/display.log\" 2>/dev/null || echo 0)\n";
    s += "self=$(readlink /proc/$$/ns/mnt 2>/dev/null); init=$(readlink /proc/1/ns/mnt 2>/dev/null)\n";
    s += "NS=; [ -n \"$self\" ] && [ \"$self\" = \"$init\" ] || NS=\"$BB nsenter -t 1 -m --\"\n";
    s += START_LINE;
    s += "DP=$!\n";
    s += "echo \"$DP\" > \"$C/display.pid\"\n";
    s += "echo 0 2>/dev/null > \"/proc/$DP/oom_score_adj\"\n";
    s += "echo \"$DP\" 2>/dev/null > /sys/fs/cgroup/oneios_session/cgroup.procs\n";
    s += "echo \"@@STARTED $DP $size ${NS:+init-ns}\"\n";
    s
}

/// The helper's start: `oneios_switch.sh`'s line (`"$BB" setsid "$DH" --session "$SESSION" --takeover --wait 60
/// </dev/null >>"$CTL/display.log" 2>&1 &`) with this script's names for the session and control folders, and
/// `$NS` in front (empty in init's mount namespace).
const START_LINE: &str = "$NS \"$BB\" setsid \"$DH\" --session \"$S/session\" --takeover --wait 60 </dev/null >>\"$C/display.log\" 2>&1 &\n";

/// What `control/display.log` gained after byte `size`, and the command line of `pid` (one named /proc entry).
pub fn progress_script(size: u64, pid: u64) -> String {
    format!("tail -c +{} {STAGE}/control/display.log 2>/dev/null; printf '@@HELPER %s\\n' \"$(cat /proc/{pid}/cmdline 2>/dev/null | tr '\\0' ' ')\"\n", size + 1)
}

fn is_helper(cmdline: &str) -> bool {
    cmdline.contains("OneiOSDisplay") || cmdline.contains("oneios-display")
}

/// The first `@@REFUSED` line of a script's output, without the mark.
fn refused(out: &str) -> Option<&str> {
    out.lines().find_map(|l| l.strip_prefix("@@REFUSED "))
}

/// The words after `mark` on the script's line that starts with it.
fn marked<'a>(out: &'a str, mark: &str) -> Option<Vec<&'a str>> {
    out.lines().find_map(|l| l.strip_prefix(mark)).map(|rest| rest.split_whitespace().collect())
}

/// `--lib` or `--dex`: the file into the staged runtime, with the SHA-256 printed for both sides.
fn install(adb: &Adb, part: &Part, local: &str, bytes: &[u8]) -> Result<()> {
    let sha = oneios_common::hex(&Sha256::digest(bytes));
    let name = part.path.rsplit('/').next().unwrap_or(part.path);
    println!("{}: local  {sha}  {local} ({} bytes)", part.option, bytes.len());
    let r = adb.shell(&format!("rm -rf {TMP}"))?;
    if !r.ok() {
        bail!("cannot clear {TMP} on the phone: {}", last_lines(&r.err, 2));
    }
    adb.push(local, &format!("{TMP}/{name}"))?;
    let r = adb.root(&install_script(part.path, &sha))?;
    let pushed = marked(&r.out, "@@PUSHED ").and_then(|w| w.first().map(|s| s.to_string()));
    let staged = marked(&r.out, "@@STAGED ");
    if let Some(why) = refused(&r.out) {
        if let Some(p) = pushed.as_deref().filter(|p| *p != sha) {
            println!("{}: phone  {p}  {TMP}/{name} (the copy adb pushed)", part.option);
        }
        if let Some(s) = staged.as_ref().and_then(|w| w.first()).filter(|s| **s != sha) {
            println!("{}: phone  {s}  runtime/{}.part (the copy in the stage, removed)", part.option, part.path);
        }
        bail!("{} {local}: {why}. The staged runtime/{} is unchanged and the display helper was not touched", part.option, part.path);
    }
    match staged.as_deref() {
        Some([s, mode]) if r.ok() && *s == sha => {
            println!("{}: phone  {s}  runtime/{} (mode {mode}; its lines in runtime.sha256 and runtime.ids are new)", part.option, part.path);
            Ok(())
        }
        _ => bail!("{} {local}: staging stopped on the phone (status {}): {}", part.option, r.status, last_lines(&format!("{}{}", r.out, r.err), 4)),
    }
}

/// `phone display-restart`.
pub fn display_restart(adb: &Adb, o: &Restart) -> Result<()> {
    // the local files first: a wrong one is refused before the phone is asked anything
    let mut parts = Vec::new();
    for (part, local) in [(&LIB, &o.lib), (&DEX, &o.dex)] {
        let Some(local) = local else { continue };
        let bytes = fs::read(local).with_context(|| format!("{} {local}", part.option))?;
        if !(part.is)(&bytes) {
            bail!("{} {local} is not {} (runtime/{} is one): nothing was sent to the phone", part.option, part.kind, part.path);
        }
        parts.push((part, local, bytes));
    }
    need_session(adb, "the display helper")?;
    let said = adb.root_ok(&state_script(), "reading the session's state")?;
    let state = marked(&said, "state ").and_then(|w| w.first().map(|s| s.to_string())).unwrap_or_default();
    if said.lines().any(|l| l.trim() == "asked") || matches!(state.as_str(), "returning" | "returned" | "failed") {
        bail!(
            "the session is returning to One UI (control/state: {}{}): the return ends the display helper. Nothing was changed",
            if state.is_empty() { "none" } else { &state },
            if said.lines().any(|l| l.trim() == "asked") { ", session/return.request is there" } else { "" }
        );
    }
    if state == "starting" {
        bail!("the switch is still starting the session (control/state: starting) and starts the display helper itself: try again when the session runs. Nothing was changed");
    }
    for (part, local, bytes) in &parts {
        install(adb, part, local, bytes)?;
    }
    if !parts.is_empty() {
        println!(
            "the staged runtime now differs from the package runtime.conf names: `phone sums` (after the return) and the next switch's \
             preflight go by the new lines; `oneios-tool phone stage runtime DIR` puts a whole package back"
        );
    }

    let r = adb.root(&restart_script(o.kill_after))?;
    if let Some(why) = refused(&r.out) {
        bail!("{why}. Nothing was stopped or started");
    }
    for line in r.out.lines().filter_map(|l| l.strip_prefix("@@OLD ")) {
        let mut w = line.split_whitespace();
        let (pid, how) = (w.next().unwrap_or("?"), w.next().unwrap_or(""));
        match how {
            "ended" => println!("old helper: pid {pid} ended on SIGTERM"),
            "killed" => println!("old helper: pid {pid} was still there {} s after SIGTERM: SIGKILL", o.kill_after.max(1)),
            _ => println!("old helper: control/display.pid named pid {pid}, which is not a display helper (gone, or a pid used again): left alone"),
        }
    }
    if !r.out.contains("@@OLD ") {
        println!("old helper: control/display.pid names none");
    }
    let started = marked(&r.out, "@@STARTED ").ok_or_else(|| anyhow!("the restart gave no answer (status {}): {}", r.status, last_lines(&format!("{}{}", r.out, r.err), 4)))?;
    let pid: u64 = started.first().and_then(|p| p.parse().ok()).ok_or_else(|| anyhow!("the restart's answer names no pid: {}", r.out.trim()))?;
    let size: u64 = started.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    println!(
        "new helper: pid {pid} (control/display.pid), started as the switch does{}: runtime/bin/oneios-display --session {STAGE}/session --takeover --wait 60 >> control/display.log",
        if started.get(2).is_some() { ", through nsenter into init's mount namespace" } else { ", in init's mount namespace" }
    );

    // what control/display.log gains, until the first frame is on the layer
    let start = Instant::now();
    let (mut seen, mut gone, mut up) = (0usize, 0u32, false);
    let mut tail: Vec<String> = Vec::new();
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let r = adb.root(&progress_script(size, pid))?;
        let at = r.out.rfind("@@HELPER ").ok_or_else(|| anyhow!("reading control/display.log failed (status {}): {}", r.status, last_lines(&format!("{}{}", r.out, r.err), 3)))?;
        let (log, cmdline) = (&r.out[..at], r.out[at + 9..].trim());
        // a line that is still being written (no newline yet) waits for the next look
        let whole = &log[..log.rfind('\n').map_or(0, |i| i + 1)];
        for line in whole.lines().skip(seen) {
            println!("  {line}");
            seen += 1;
            up |= line.contains("oneios-display: layer up");
            if tail.len() == 4 {
                tail.remove(0);
            }
            tail.push(line.to_string());
            if line.contains("oneios-display: first frame posted") {
                println!("display helper: pid {pid} runs, first frame posted {:.1} s after the start", start.elapsed().as_secs_f32());
                println!("session: {}", if adb.session_open()? { "open (xnuhost was not touched; the return ends this helper by control/display.pid)" } else { "ended meanwhile" });
                return Ok(());
            }
        }
        // twice in a row: the helper's last lines are in the log by then
        gone = if is_helper(cmdline) { 0 } else { gone + 1 };
        let said = if tail.is_empty() { "control/display.log gained nothing".to_string() } else { format!("its last lines: {}", tail.join(" | ")) };
        if gone >= 2 {
            bail!("the new display helper (pid {pid}) ended before its first frame; {said}. The session goes on with the picture it had or a dark screen; the return is not affected");
        }
        if gone == 0 && start.elapsed().as_secs() >= o.timeout {
            bail!(
                "no `oneios-display: first frame posted` in control/display.log {} s after the start{}; the helper (pid {pid}) still runs and is left running (it waits 60 s for xnuhost's socket); {said}. \
                 Look again: oneios-tool phone log --file display --tail 20",
                o.timeout,
                if up { " (its layer is up)" } else { "" }
            );
        }
    }
}

// ---------------------------------------------------------------- the touch IC

pub fn touch_read_script() -> String {
    format!("cat {TSP_ENABLED}\n")
}

/// What One UI's input HAL writes when the panel comes on: display state 2 (on), event 0 (early) then 1 (late).
/// The brightness bridge writes the same when iOS's level leaves 0 (`runtime/bridges/brightness`).
pub fn touch_wake_script() -> String {
    format!("echo 2,0 > {TSP_ENABLED} && echo 2,1 > {TSP_ENABLED}\n")
}

fn touch_read(adb: &Adb) -> Result<String> {
    Ok(adb.root_ok(&touch_read_script(), &format!("reading {TSP_ENABLED}"))?.trim().to_string())
}

fn touch_line(value: &str) -> String {
    let means = match value {
        "1" => "on",
        "0" => "off: the touch IC sleeps (oneios-tool phone touch wake)",
        _ => "neither 0 nor 1",
    };
    format!("touch: {TSP_ENABLED} reads {value} ({means})")
}

/// `phone touch status`: read only.
pub fn touch_status(adb: &Adb) -> Result<()> {
    need_session(adb, "the touch IC")?;
    println!("{}", touch_line(&touch_read(adb)?));
    Ok(())
}

/// `phone touch wake`: `2,0` then `2,1`, then the state read back.
pub fn touch_wake(adb: &Adb) -> Result<()> {
    need_session(adb, "the touch IC")?;
    let before = touch_read(adb)?;
    adb.root_ok(&touch_wake_script(), &format!("writing 2,0 and 2,1 to {TSP_ENABLED}"))?;
    let after = touch_read(adb)?;
    println!("touch: wrote 2,0 then 2,1 to {TSP_ENABLED} (it read {before} before)");
    println!("{}", touch_line(&after));
    if after != "1" {
        bail!("the touch IC did not wake: {TSP_ENABLED} reads {after}, not 1");
    }
    Ok(())
}

// ---------------------------------------------------------------- the panel's level

/// The level, the node's maximum and One UI's own value as the switch recorded it (`control/base/backlight`, which
/// the return writes back).
pub fn backlight_read_script() -> String {
    format!(
        "echo \"now $(cat {BACKLIGHT_DIR}/brightness)\"; echo \"max $(cat {BACKLIGHT_DIR}/max_brightness)\"; \
         echo \"base $(awk 'index($1, \"panel0-backlight/brightness\") {{ print $2 }}' {STAGE}/control/base/backlight 2>/dev/null)\"\n"
    )
}

pub fn backlight_write_script(level: u32) -> String {
    format!("echo {level} > {BACKLIGHT_DIR}/brightness && cat {BACKLIGHT_DIR}/brightness\n")
}

struct Backlight {
    now: u32,
    max: u32,
    /// One UI's value at the switch, when the switch recorded one for this node
    base: Option<u32>,
}

fn backlight_read(adb: &Adb) -> Result<Backlight> {
    let r = adb.root(&backlight_read_script())?;
    let field = |key: &str| marked(&r.out, key).and_then(|w| w.first().and_then(|v| v.parse::<u32>().ok()));
    match (field("now "), field("max ")) {
        (Some(now), Some(max)) => Ok(Backlight { now, max, base: field("base ") }),
        _ => bail!("{BACKLIGHT_DIR}/brightness or max_brightness gave no number: {}", last_lines(&format!("{}{}", r.out, r.err), 4)),
    }
}

fn base_line(b: &Backlight) -> String {
    match b.base {
        Some(base) => format!("One UI's own value: {base} (control/base/backlight, read at the switch): the return writes it back"),
        None => "One UI's own value: the switch recorded none for this node (control/base/backlight): after the return One UI's system_server sets its level when it starts".to_string(),
    }
}

/// `phone backlight status`: read only.
pub fn backlight_status(adb: &Adb) -> Result<()> {
    need_session(adb, "the panel's level")?;
    let b = backlight_read(adb)?;
    println!("backlight: {} of {} ({BACKLIGHT_DIR}/brightness)", b.now, b.max);
    println!("{}", base_line(&b));
    Ok(())
}

/// `phone backlight set N`: 1 to the node's `max_brightness`. 0 is refused: it is the panel off, and iOS's own level
/// 0 is what puts the touch IC to sleep.
pub fn backlight_set(adb: &Adb, level: u32) -> Result<()> {
    need_session(adb, "the panel's level")?;
    let b = backlight_read(adb)?;
    if level < 1 || level > b.max {
        bail!("the level {level} is outside 1..{} ({BACKLIGHT_DIR}/max_brightness; 0 would turn the panel off). Nothing was written", b.max);
    }
    let read = adb.root_ok(&backlight_write_script(level), &format!("writing {BACKLIGHT_DIR}/brightness"))?;
    let read = read.trim();
    println!("backlight: {level} written (it was {}), the node reads {read} of {}", b.now, b.max);
    println!("{}", base_line(&b));
    println!("the level holds until iOS next changes its brightness: xnuhost's brightness bridge then writes the node");
    if read != level.to_string() {
        bail!("{BACKLIGHT_DIR}/brightness reads {read} after {level} was written");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The helper's start is the switch script's line (OneOS `switcher/assets/oneios_switch.sh`, "The guest's
    /// screen"), with `$NS` in front and this script's names for the two folders.
    #[test]
    fn the_helper_starts_as_the_switch_starts_it() {
        let switch = r#""$BB" setsid "$DH" --session "$SESSION" --takeover --wait 60 </dev/null >>"$CTL/display.log" 2>&1 &"#;
        let ours = START_LINE.trim_end().strip_prefix("$NS ").expect("$NS first").replace("\"$S/session\"", "\"$SESSION\"").replace("\"$C/display.log\"", "\"$CTL/display.log\"");
        assert_eq!(ours, switch);
        let script = restart_script(3);
        for line in [
            "S=/data/local/tmp/oneios; C=$S/control; DH=$S/runtime/bin/oneios-display; BB=/data/adb/ksu/bin/busybox",
            "DP=$!",
            r#"echo "$DP" > "$C/display.pid""#,
            r#"echo 0 2>/dev/null > "/proc/$DP/oom_score_adj""#,
            r#"echo "$DP" 2>/dev/null > /sys/fs/cgroup/oneios_session/cgroup.procs"#,
            r#"NS=; [ -n "$self" ] && [ "$self" = "$init" ] || NS="$BB nsenter -t 1 -m --""#,
        ] {
            assert!(script.lines().any(|l| l == line), "no line: {line}");
        }
        // 3 s between SIGTERM and SIGKILL, looked at five times a second
        assert!(script.contains("while [ $n -lt 15 ] && helper \"$dp\"; do sleep 0.2;"));
        assert!(restart_script(0).contains("[ $n -lt 5 ]"));
    }

    /// Only named files, nothing under /sys/power, no walk and no restart, in every script of this file.
    #[test]
    fn the_scripts_name_their_files() {
        let scripts = [
            state_script(),
            install_script(LIB.path, &"0".repeat(64)),
            install_script(DEX.path, &"0".repeat(64)),
            restart_script(3),
            progress_script(0, 4242),
            touch_read_script(),
            touch_wake_script(),
            backlight_read_script(),
            backlight_write_script(5350),
        ];
        let allowed = [
            "/sys/class/sec/tsp/enabled",
            "/sys/class/backlight/panel0-backlight/brightness",
            "/sys/class/backlight/panel0-backlight/max_brightness",
            "/sys/fs/cgroup/oneios_session/cgroup.procs",
        ];
        for s in &scripts {
            for bad in ["find ", "ls ", "du ", "/sys/power", "reboot", "*/", "pkill", "pidof", "killall"] {
                assert!(!s.contains(bad), "\"{bad}\" in: {s}");
            }
            for (at, _) in s.match_indices("/sys/") {
                let path: String = s[at..].chars().take_while(|c| !c.is_whitespace() && !";\")".contains(*c)).collect();
                assert!(allowed.contains(&path.as_str()), "{path} is not one of the named /sys files");
            }
        }
    }

    #[test]
    fn the_touch_and_backlight_commands_are_the_phones() {
        assert_eq!(touch_wake_script(), "echo 2,0 > /sys/class/sec/tsp/enabled && echo 2,1 > /sys/class/sec/tsp/enabled\n");
        assert_eq!(touch_read_script(), "cat /sys/class/sec/tsp/enabled\n");
        assert_eq!(
            backlight_write_script(5350),
            "echo 5350 > /sys/class/backlight/panel0-backlight/brightness && cat /sys/class/backlight/panel0-backlight/brightness\n"
        );
    }

    #[test]
    fn a_library_and_a_dex_are_told_apart() {
        let mut elf = vec![0u8; 64];
        elf[..4].copy_from_slice(b"\x7fELF");
        (elf[4], elf[16], elf[18]) = (2, 3, 0xb7);
        assert!((LIB.is)(&elf) && !(DEX.is)(&elf));
        assert!((DEX.is)(b"dex\n035\0rest") && !(LIB.is)(b"dex\n035\0rest and more bytes here"));
        elf[18] = 0x3e; // x86_64: a host build
        assert!(!(LIB.is)(&elf));
    }
}
