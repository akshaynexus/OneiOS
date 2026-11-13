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
    ("hid", r"oneios-hid|ONEIOS HAPTIC|oneios-haptic|ONEIOS VIBRATOR|oneios-actuator|xnuhost . haptics: |DroidVMAudio: "),
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

/// What a look at the followed file found: its inode, change time (`stat %Z`), lines and first line.
#[derive(Clone, Debug, Default, PartialEq)]
struct Seen {
    inode: u64,
    ctime: u64,
    lines: u64,
    first: String,
}

/// One look at the followed file (one root script, the named files only): the file, or None when it is not there;
/// with `switch`, the phone's second of the last switch (switch.log's last `epoch` line).
fn look_at(adb: &Adb, path: &str, switch: bool) -> Result<(Option<Seen>, Option<u64>)> {
    let mut script = format!("if [ -f {path} ]; then echo \"file $(stat -c '%i %Z' {path}) $(wc -l < {path})\"; echo \"first $(head -n 1 {path})\"; else echo missing; fi\n");
    if switch {
        script += &format!("echo \"switch $(grep epoch {STAGE}/switch.log 2>/dev/null | tail -n 1)\"\n");
    }
    Ok(parse_look(&adb.root_ok(&script, "looking at the log")?))
}

fn parse_look(out: &str) -> (Option<Seen>, Option<u64>) {
    let mut seen = None;
    let mut epoch = None;
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("file ") {
            let n: Vec<u64> = rest.split_whitespace().filter_map(|w| w.parse().ok()).collect();
            if n.len() == 3 {
                seen = Some(Seen { inode: n[0], ctime: n[1], lines: n[2], first: String::new() });
            }
        } else if let (Some(rest), Some(s)) = (line.strip_prefix("first "), seen.as_mut()) {
            s.first = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("switch ") {
            epoch = rest.split_whitespace().skip_while(|w| *w != "epoch").nth(1).and_then(|e| e.parse().ok());
        }
    }
    (seen, epoch)
}

/// Is `now` the file that was being read (`was`, of which `read` lines came): the same inode, the same first line
/// once both have one, and not fewer lines than were read. A session's log is replaced, not truncated: another
/// inode is another file even when its first line reads the same (a new log's first line often does).
fn same_file(was: &Seen, now: &Seen, read: u64) -> bool {
    was.inode == now.inode && (was.first.is_empty() || now.first.is_empty() || was.first == now.first) && now.lines >= read
}

/// After a switch only a log changed at or after it counts: the switch replaces session/xnuhost.log only about
/// 40 s in, and until then the file there is the last session's (the cycle's watch keeps the same rule).
fn before_switch(now: &Seen, switch: Option<u64>) -> bool {
    switch.is_some_and(|epoch| now.ctime < epoch)
}

/// `--follow`: `tail -n +K -f` on the file (its descriptor: when the file is replaced the stream would go on with
/// the old one, so a watcher looks at the file every 2 s and ends the stream when it was replaced). When adb drops,
/// or the stream ends, it carries on after the last whole line read when the file is the same one; a new file is
/// read from its start; a session's log older than the last switch is waited out.
fn follow(adb: &Adb, name: &str, path: &str, tail: Option<u64>, sink: &mut Sink) -> Result<()> {
    let session_log = name == "xnuhost";
    let mut current: Option<Seen> = None;
    let mut read = 0u64;
    let mut drops = 0u64;
    let mut said_wait = false;
    let mut said_missing = false;
    while !stopped() {
        let (now, switch) = match look_at(adb, path, session_log) {
            Ok(look) => look,
            Err(e) => {
                drops += 1;
                eprintln!("phone log: {e:#}; looking again");
                if sleep_or_stop(Duration::from_secs(drops.min(5))) {
                    break;
                }
                continue;
            }
        };
        let Some(now) = now else {
            if !said_missing {
                eprintln!("phone log: {path} is not there (yet); waiting for it");
                said_missing = true;
            }
            if sleep_or_stop(Duration::from_secs(2)) {
                break;
            }
            continue;
        };
        said_missing = false;
        match current.as_mut() {
            Some(was) if same_file(was, &now, read) => {
                if was.first.is_empty() {
                    was.first = now.first.clone();
                }
            }
            _ => {
                if before_switch(&now, switch) {
                    if !said_wait {
                        eprintln!(
                            "phone log: {path} was last changed before the switch at {} (phone time {}): it is the last session's log; waiting for this session's (the switch replaces it about 40 s in)",
                            switch.unwrap_or(0),
                            now.ctime
                        );
                        said_wait = true;
                    }
                    if sleep_or_stop(Duration::from_secs(2)) {
                        break;
                    }
                    continue;
                }
                read = match current {
                    Some(_) => {
                        eprintln!("phone log: {path} is a new file (another inode or first line); reading it from its start");
                        0
                    }
                    None => tail.map(|n| now.lines.saturating_sub(n)).unwrap_or(0),
                };
                current = Some(now);
            }
        }
        let following = current.clone().expect("a file is followed here");
        let before = sink.lines;
        let replaced = AtomicBool::new(false);
        let done = AtomicBool::new(false);
        let clean = std::thread::scope(|scope| {
            scope.spawn(|| watch_replaced(adb, path, &following, &done, &replaced));
            let r = stream(adb, &format!("su -c 'tail -n +{} -f {path}'", read + 1), sink);
            done.store(true, Ordering::SeqCst);
            r
        })?;
        read += sink.lines - before;
        if stopped() {
            break;
        }
        if replaced.load(Ordering::SeqCst) {
            eprintln!("phone log: {path} was replaced after {} lines; reading the new one", sink.lines);
            drops = 0;
            continue;
        }
        drops = if sink.lines > before { 1 } else { drops + 1 };
        eprintln!("phone log: adb {} after {} lines; connecting again", if clean { "ended" } else { "dropped" }, sink.lines);
        if sleep_or_stop(Duration::from_secs(drops.min(5))) {
            break;
        }
    }
    eprintln!("phone log: stopped after {} lines", sink.lines);
    Ok(())
}

/// While a stream reads `was`: every 2 s a look at the file; when it is another one (or gone), the stream is
/// ended. A look that fails (adb dropping) is not an answer.
fn watch_replaced(adb: &Adb, path: &str, was: &Seen, done: &AtomicBool, replaced: &AtomicBool) {
    loop {
        let end = Instant::now() + Duration::from_secs(2);
        while Instant::now() < end {
            if done.load(Ordering::SeqCst) || stopped() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if !replaced.load(Ordering::SeqCst) {
            match look_at(adb, path, false) {
                Ok((Some(now), _)) if same_file(was, &now, 0) => continue,
                Ok(_) => replaced.store(true, Ordering::SeqCst),
                Err(_) => continue,
            }
        }
        let child = CHILD.load(Ordering::SeqCst);
        if child > 0 {
            // Safety: the pid is our own adb child's, being read by stream()
            unsafe { libc::kill(child, libc::SIGTERM) };
        }
    }
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
        follow(adb, &o.file, &path, o.tail, &mut sink)?;
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

    #[test]
    fn follow_knows_the_file() {
        let (seen, epoch) = parse_look("file 4242 1794000050 12\nfirst [    0.003] xnuhost I xnuhost: session ABI 1\nswitch === OneOS app switch 2026-11-13 00:01:02 epoch 1794000040 versionCode 115 ===\n");
        let seen = seen.unwrap();
        assert_eq!((seen.inode, seen.ctime, seen.lines, epoch), (4242, 1794000050, 12, Some(1794000040)));
        assert_eq!(seen.first, "[    0.003] xnuhost I xnuhost: session ABI 1");
        assert_eq!(parse_look("missing\nswitch \n"), (None, None));
        // the switch's rule: changed before the switch is the last session's log; at or after it counts
        assert!(before_switch(&Seen { ctime: 1794000039, ..seen.clone() }, epoch));
        assert!(!before_switch(&Seen { ctime: 1794000040, ..seen.clone() }, epoch));
        assert!(!before_switch(&seen, None));
        // a drop: the same file grown is the same; another inode with the same first line, or fewer lines than
        // were read, or another first line, is a new file
        assert!(same_file(&seen, &Seen { lines: 30, ..seen.clone() }, 12));
        assert!(!same_file(&seen, &Seen { inode: 4243, ..seen.clone() }, 0));
        assert!(!same_file(&seen, &Seen { lines: 3, ..seen.clone() }, 12));
        assert!(!same_file(&seen, &Seen { first: "[    0.004] another".into(), ..seen.clone() }, 0));
        assert!(same_file(&Seen { first: String::new(), ..seen.clone() }, &seen, 0));
    }
}
