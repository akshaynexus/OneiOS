//! `oneios-tool phone log`: the session's logs from the phone, by name (no listing of a folder on the phone), with
//! filters, a live follow that survives adb dropping, and a saved copy. Also the boot's markers, which `cycle`
//! watches. Runs beside a session: it only reads.

use crate::adb::{Adb, STAGE};
use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};

/// The stage's log files (oneos/interface.md section 3), by the name `--file` takes.
pub const FILES: [(&str, &str, &str); 6] = [
    ("xnuhost", "session/xnuhost.log", "this session's log, or the last one's (the next switch moves it to logs/)"),
    ("switch", "switch.log", "the switch"),
    ("return", "return.log", "the return, with the watcher and the key readers"),
    ("outcome", "return.outcome", "how the last return ended"),
    ("display", "control/display.log", "the display helper's output"),
    ("watchdog", "control/watchdog_feeder.log", "the watchdog feeder"),
];

/// The file on the phone for `--file NAME`: one of FILES, or `logs/xnuhost-<epoch>.log` (an earlier session).
pub fn path_of(name: &str) -> Result<String> {
    if let Some((_, path, _)) = FILES.iter().find(|(n, _, _)| *n == name) {
        return Ok(format!("{STAGE}/{path}"));
    }
    let earlier = name.strip_prefix("logs/xnuhost-").and_then(|r| r.strip_suffix(".log")).is_some_and(|e| !e.is_empty() && e.bytes().all(|b| b.is_ascii_digit()));
    if earlier {
        return Ok(format!("{STAGE}/{name}"));
    }
    bail!("--file {name}: one of {} or logs/xnuhost-<epoch>.log (phone log --list)", FILES.map(|f| f.0).join(", "))
}

// ---------------------------------------------------------------- Ctrl-C

static STOP: AtomicBool = AtomicBool::new(false);
static CHILD: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
    let child = CHILD.load(Ordering::SeqCst);
    if child > 0 {
        // Safety: kill is async-signal-safe; the pid is our own child's (0 once it is reaped).
        unsafe { libc::kill(child, libc::SIGTERM) };
    }
}

/// From now on Ctrl-C (and SIGTERM) only sets a flag and ends the adb child that is being read: the loops here
/// and in `cycle` see the flag and finish cleanly (the saved file is whole, the way back is taken).
pub fn catch_stop() {
    // Safety: the handler only stores to atomics and calls kill.
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
}

pub fn stopped() -> bool {
    STOP.load(Ordering::SeqCst)
}

/// Sleep, in short steps, until the time is over or Ctrl-C came; true when it came.
pub fn sleep_or_stop(time: Duration) -> bool {
    let end = Instant::now() + time;
    while Instant::now() < end && !stopped() {
        std::thread::sleep(Duration::from_millis(100));
    }
    stopped()
}

// ---------------------------------------------------------------- filters

/// The named filters: the prefixes of docs/booter.md, docs/hardware.md, docs/gpu.md and the hosted patches' README.
pub const NAMED: [(&str, &str); 6] = [
    ("panic", r"panic\(|xnuhost !!|xnuhost E |kernel fault|sheng-exc|sheng-exit|crashed: |killed\] exiting|stopping: |xnuhost I exit "),
    ("display", r"oneios-display|sheng-vsync|ONEIOS BACKLIGHT|oneios-swap|ONEIOS ALS|displayctl|brightctl"),
    ("gpu", r"oneios-gpu|sheng-gpu|oneios-adreno|kgsl"),
    ("wifi", r"ONEIOS WIFI|oneios-wifi|sheng-wifi|oneios-net|wifictl"),
    ("hid", r"oneios-hid|ONEIOS HAPTIC|oneios-haptic|ONEIOS VIBRATOR|oneios-actuator"),
    ("mem", r"oneios-swap|oneios-fault-around|oneios-gpu-memory|memorystatus|jetsam|vm_pageout|low swap|Too many corpses"),
];

/// The stages of a boot in the order oneos/interface.md section 8 gives them, each with what its first line holds.
const STAGES: [(&str, &str); 9] = [
    ("xnuhost", "xnuhost I xnuhost: session ABI"),
    ("kernel", "cpu0: entering the kernel at"),
    ("darwin", "Darwin Kernel Version"),
    ("bsd-root", "BSD root: "),
    ("init", "load_init_program: attempting to load"),
    ("launchd", "com.apple.xpc.launchd|"),
    ("backboardd", "backboardd["),
    ("SpringBoard", "SpringBoard["),
    ("ready", "xnuhost I guest ready: first frame"),
];

/// What a log line means for the boot.
#[derive(Clone, Debug, PartialEq)]
pub enum Mark {
    /// A stage reached for the first time (its name), or reached again after a restart in place.
    Stage(&'static str),
    Panic,
    /// `stopping:`, `exit N`, a restart, a host fault: the session is ending or started over.
    End,
}

/// Finds the markers in a log as it comes: a stage counts once per boot.
#[derive(Default)]
pub struct Markers {
    seen: [bool; 9],
    pub found: Vec<(Mark, String)>,
}

impl Markers {
    pub fn line(&mut self, line: &str) -> Option<Mark> {
        let mark = if line.contains("panic(cpu") {
            Some(Mark::Panic)
        } else if line.contains("xnuhost I stopping: ") || line.contains("xnuhost I exit ") || line.contains("the guest asked for a restart") || line.contains("xnuhost !! ") || line.contains("without getting ready") {
            Some(Mark::End)
        } else {
            let at = STAGES.iter().position(|(_, text)| line.contains(text) || (*text == "SpringBoard[" && line.contains("crashed: SpringBoard.")))?;
            if at == 0 {
                // a new boot (also after a restart in place): every stage counts again
                self.seen = [false; 9];
            }
            if self.seen[at] {
                return None;
            }
            self.seen[at] = true;
            Some(Mark::Stage(STAGES[at].0))
        };
        if let Some(mark) = &mark {
            self.found.push((mark.clone(), line.to_string()));
        }
        mark
    }
    pub fn ready(&self) -> bool {
        self.seen[8]
    }
}

/// The seconds a line's `[seconds.millis]` stamp names.
fn stamp(line: &str) -> Option<f64> {
    line.strip_prefix('[')?.split_once(']')?.0.trim().parse().ok()
}

pub struct Filter {
    any: Vec<Regex>,
    markers: Option<Markers>,
    since: Option<f64>,
    /// whether the last stamped line was at or after `since` (lines without a stamp follow it)
    after: bool,
}

impl Filter {
    pub fn new(grep: &[String], named: &[&str], markers: bool, since: Option<f64>) -> Result<Filter> {
        let mut any = Vec::new();
        for pattern in grep {
            any.push(Regex::new(pattern).map_err(|e| anyhow!("--grep {pattern}: {e}"))?);
        }
        for name in named {
            let (_, pattern) = NAMED.iter().find(|(n, _)| n == name).expect("a named filter");
            any.push(Regex::new(pattern).expect("a fixed pattern"));
        }
        Ok(Filter { any, markers: markers.then(Markers::default), since, after: since.is_none() })
    }
    /// Is this line printed: after `--since`, and matching any filter given (every line when none is).
    pub fn takes(&mut self, line: &str) -> bool {
        if let (Some(since), Some(t)) = (self.since, stamp(line)) {
            self.after = t >= since;
        }
        // the markers see every line, so that a stage counts once even when its first line is before --since
        let marked = self.markers.as_mut().is_some_and(|m| m.line(line).is_some());
        if !self.after {
            return false;
        }
        if self.any.is_empty() && self.markers.is_none() {
            return true;
        }
        marked || self.any.iter().any(|r| r.is_match(line))
    }
}

// ---------------------------------------------------------------- reading

pub struct Options {
    pub file: String,
    pub tail: Option<u64>,
    pub follow: bool,
    pub save: Option<String>,
    pub analyze: bool,
}

/// `--list`: each named file with its size and the time of its last change, and the earlier sessions' logs as the
/// fork's own `oneios_root.sh logs` names them. Named files only: no folder of the phone is listed here.
pub fn list(adb: &Adb) -> Result<()> {
    adb.need_root()?;
    let script: String = FILES.iter().map(|(name, path, _)| format!("echo \"{name} $(stat -c '%s %Y' {STAGE}/{path} 2>/dev/null || echo missing)\"\n")).collect();
    let out = adb.root_ok(&script, "reading the logs' sizes")?;
    for (line, (_, path, what)) in out.lines().zip(FILES) {
        let mut words = line.split_whitespace();
        let name = words.next().unwrap_or("?");
        match (words.next(), words.next()) {
            (Some(size), Some(time)) if size != "missing" => println!("{name:9} {size:>10} bytes  changed {time}  {path}: {what}"),
            _ => println!("{name:9} {:>10}        {path}: {what}", "missing"),
        }
    }
    let r = adb.root(&format!("sh {STAGE}/bin/oneios_root.sh logs\n"))?;
    for line in r.out.lines().filter(|l| l.starts_with("@@RESULT log ")) {
        println!("{line}");
    }
    Ok(())
}

/// Where `--save-auto` writes: out/session-logs/DATE/live-HHMMSS.log under `root`.
pub fn auto_save_path(root: &std::path::Path) -> String {
    let (date, time) = crate::cycle::local_date_time();
    root.join("out/session-logs").join(date).join(format!("live-{time}.log")).display().to_string()
}

struct Sink {
    filter: Filter,
    save: Option<std::fs::File>,
    lines: u64,
}

impl Sink {
    fn line(&mut self, line: &str) -> Result<()> {
        self.lines += 1;
        if let Some(f) = &mut self.save {
            writeln!(f, "{line}").context("writing the saved log")?;
        }
        if self.filter.takes(line) {
            let mut out = std::io::stdout().lock();
            // a closed pipe (| head) ends the follow like Ctrl-C
            if writeln!(out, "{line}").and_then(|_| out.flush()).is_err() {
                STOP.store(true, Ordering::SeqCst);
            }
        }
        Ok(())
    }
}

/// Read what one adb command prints, line by line; returns how it ended.
fn stream(adb: &Adb, cmd: &str, sink: &mut Sink) -> Result<bool> {
    let mut child = adb.exec_out(cmd)?;
    CHILD.store(child.id() as i32, Ordering::SeqCst);
    let mut reader = BufReader::new(child.stdout.take().expect("piped"));
    let mut raw = Vec::new();
    let mut result = Ok(());
    loop {
        raw.clear();
        match reader.read_until(b'\n', &mut raw) {
            Ok(0) => break,
            Ok(_) => {
                // a line without its end is cut off (adb dropped): it comes again whole after the reconnect
                if raw.last() != Some(&b'\n') {
                    break;
                }
                let text = String::from_utf8_lossy(&raw);
                result = sink.line(text.trim_end_matches(['\n', '\r']));
                if result.is_err() || stopped() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    if stopped() || result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait();
    CHILD.store(0, Ordering::SeqCst);
    result?;
    Ok(status.map(|s| s.success()).unwrap_or(false))
}

pub fn log(adb: &Adb, o: &Options, filter: Filter) -> Result<()> {
    adb.need_root()?;
    // one of the fixed names, or logs/xnuhost-<digits>.log: nothing else reaches the phone's shell
    let path = path_of(&o.file)?;
    let save = match &o.save {
        Some(p) => {
            if let Some(dir) = std::path::Path::new(p).parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir).with_context(|| dir.display().to_string())?;
            }
            Some(std::fs::File::create(p).with_context(|| p.clone())?)
        }
        None => None,
    };
    let mut sink = Sink { filter, save, lines: 0 };
    catch_stop();
    if !o.follow {
        let cmd = match o.tail {
            Some(n) => format!("su -c 'tail -n {n} {path}'"),
            None => format!("su -c 'cat {path}'"),
        };
        if !stream(adb, &cmd, &mut sink)? && !stopped() {
            bail!("reading {path} failed (phone log --list shows which logs are there)");
        }
    } else {
        // `tail -n +K -F` on the one named file. When adb drops, start again after the lines that came whole.
        let mut first = match o.tail {
            Some(n) => {
                let total: u64 = adb.root_ok(&format!("wc -l < {path} 2>/dev/null || echo 0\n"), "counting the log's lines")?.trim().parse().unwrap_or(0);
                total.saturating_sub(n)
            }
            None => 0,
        };
        let mut drops = 0;
        while !stopped() {
            let before = sink.lines;
            let clean = stream(adb, &format!("su -c 'tail -n +{} -F {path}'", first + 1), &mut sink)?;
            first += sink.lines - before;
            if stopped() {
                break;
            }
            drops = if sink.lines > before { 1 } else { drops + 1 };
            eprintln!("phone log: adb {} after {} lines; connecting again", if clean { "ended" } else { "dropped" }, sink.lines);
            if sleep_or_stop(Duration::from_secs(drops.min(5))) {
                break;
            }
            // a new session's log starts over: fewer lines than were read means a new file
            if let Ok(r) = adb.root(&format!("wc -l < {path} 2>/dev/null || echo 0\n")) {
                if r.out.trim().parse::<u64>().is_ok_and(|total| total < first) {
                    eprintln!("phone log: {path} is a new file; reading it from its start");
                    first = 0;
                }
            }
        }
        eprintln!("phone log: stopped after {} lines", sink.lines);
    }
    drop(sink.save.take());
    if let Some(p) = &o.save {
        eprintln!("phone log: saved {} lines to {p}", sink.lines);
    }
    if o.analyze {
        analyze(o.save.as_deref().expect("--analyze saves the log"))?;
    }
    Ok(())
}

/// `--analyze`: the saved log through `oneios-tool log`: the timeline, and the threads' stacks named through the
/// `sym` index when one was built (out/dsc-index/latest, or ONEIOS_DSC_INDEX).
pub fn analyze(saved: &str) -> Result<()> {
    println!("--- oneios-tool log timeline {saved}");
    let status = oneios_log::run(&["timeline".to_string(), saved.to_string()]);
    let index = std::env::var("ONEIOS_DSC_INDEX").ok().filter(|d| !d.is_empty()).map(std::path::PathBuf::from).or_else(|| {
        let root = crate::cycle::repo_root().ok()?;
        [root.join("out/dsc-index/latest"), crate::cycle::main_checkout(&root).join("out/dsc-index/latest")].into_iter().find(|p| p.exists())
    });
    match index.filter(|p| p.exists()) {
        Some(dir) => {
            println!("--- oneios-tool log stacks {saved} --summary (the sym index: {})", dir.display());
            oneios_log::run(&["stacks".to_string(), saved.to_string(), "--summary".to_string(), "--index".to_string(), dir.display().to_string()]);
        }
        None => println!("--- no sym index (oneios-tool sym index builds one): the stacks are not named"),
    }
    if status != 0 {
        bail!("oneios-tool log timeline ended with status {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_count_a_stage_once_per_boot() {
        let mut m = Markers::default();
        let lines = [
            "[    0.001] xnuhost I xnuhost: session ABI 1",
            "[    0.500] xnuhost I cpu0: entering the kernel at 0x1",
            "[    9.000] guest     backboardd[66]: a",
            "[    9.100] guest     backboardd[66]: b",
            "[   20.000] xnuhost I guest ready: first frame",
            "[   21.000] guest     panic(cpu 0 caller 0x1): x",
            "[   21.100] xnuhost I the guest asked for a restart: starting again",
            "[   21.200] xnuhost I xnuhost: session ABI 1",
            "[   30.000] guest     backboardd[70]: c",
        ];
        let marks: Vec<Option<Mark>> = lines.iter().map(|l| m.line(l)).collect();
        assert_eq!(marks[0], Some(Mark::Stage("xnuhost")));
        assert_eq!(marks[2], Some(Mark::Stage("backboardd")));
        assert_eq!(marks[3], None);
        assert_eq!(marks[4], Some(Mark::Stage("ready")));
        assert_eq!(marks[5], Some(Mark::Panic));
        assert_eq!(marks[6], Some(Mark::End));
        assert_eq!(marks[8], Some(Mark::Stage("backboardd")));
        assert!(!m.ready());
    }

    #[test]
    fn filters() {
        let mut f = Filter::new(&["touch (down|up)".to_string()], &["gpu"], false, Some(5.0)).unwrap();
        assert!(!f.takes("[    1.000] guest     oneios-gpu-report: early"));
        assert!(!f.takes("a line with no stamp, before"));
        assert!(f.takes("[    5.000] guest     oneios-gpu-report: 600 frames"));
        assert!(f.takes("[    6.000] guest     oneios-hid: touch down 1 2"));
        assert!(!f.takes("[    7.000] guest     oneios-hid: move"));
        let mut all = Filter::new(&[], &[], false, None).unwrap();
        assert!(all.takes("anything"));
        assert!(path_of("xnuhost").unwrap().ends_with("/session/xnuhost.log"));
        assert!(path_of("logs/xnuhost-1794000000.log").is_ok());
        assert!(path_of("logs/../../etc/passwd").is_err() && path_of("session").is_err());
    }
}
