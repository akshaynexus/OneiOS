//! `oneios-tool serve` as the OneOS app uses it (README "serve"): the process is started, requests are written to
//! its standard input and its lines are read. What is checked: the lines' form, the order of a request's events,
//! the queue, progress, every way a request and the process end (a cancel request, the end of the pipe in the
//! middle of a request, SIGTERM, the idle timeout, a reader that went away), that a stop removes the temporary
//! files, that nothing is left behind (`bye`'s threads and children, and the process is gone), the refusals with
//! their codes, the heat and battery limits on stand-in files, and each command on stand-in inputs.
//!
//! No Apple file and no phone: the stage, the phone's /sys and /proc files and the inputs are made here.
//! `ONEIOS_TOOL=PATH` runs another binary than this build's.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

fn tool() -> String {
    std::env::var("ONEIOS_TOOL").ok().filter(|t| !t.is_empty()).unwrap_or_else(|| env!("CARGO_BIN_EXE_oneios-tool").to_string())
}

/// A folder of this test, with `stage` and `sys` (the stand-in of the phone's files) in it.
struct Box {
    top: PathBuf,
}

impl Box {
    fn new(name: &str) -> Box {
        let top = std::env::temp_dir().join(format!("oneios-serve-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&top);
        std::fs::create_dir_all(top.join("stage")).unwrap();
        std::fs::create_dir_all(top.join("sys")).unwrap();
        Box { top }
    }
    fn stage(&self, rel: &str) -> String {
        self.top.join("stage").join(rel).to_string_lossy().into_owned()
    }
    fn put(&self, rel: &str, data: &[u8]) -> String {
        let path = self.top.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, data).unwrap();
        path.to_string_lossy().into_owned()
    }
    /// A file of the stand-in phone (`sys/...`).
    fn sys(&self, rel: &str, text: &str) {
        self.put(&format!("sys/{}", rel.trim_start_matches('/')), text.as_bytes());
    }
}

impl Drop for Box {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.top);
        }
    }
}

struct Serve {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<Value>,
    /// Every line read so far.
    seen: Vec<Value>,
    stderr: Option<std::thread::JoinHandle<String>>,
}

const LONG: Duration = Duration::from_secs(60);

impl Serve {
    fn start(b: &Box, options: &[&str]) -> Serve {
        Serve::start_env(b, options, &[])
    }

    /// With more variables in its environment (`ONEIOS_SERVE_TEST_FREE`).
    fn start_env(b: &Box, options: &[&str], env: &[(&str, &str)]) -> Serve {
        let mut child = Command::new(tool())
            .envs(env.iter().copied())
            .arg("serve")
            .args(["--stage", &b.stage(""), "--sys-root", &b.top.join("sys").to_string_lossy()])
            .args(options)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("oneios-tool serve starts");
        let (tx, lines) = channel();
        let out = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                assert!(line.starts_with("{\"ev\":"), "every line of the tool begins with {{\"ev\": : {line}");
                let value: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"));
                if tx.send(value).is_err() {
                    break;
                }
            }
        });
        let mut err = child.stderr.take().unwrap();
        let stderr = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = err.read_to_string(&mut text);
            text
        });
        let stdin = child.stdin.take();
        let mut serve = Serve { child, stdin, lines, seen: Vec::new(), stderr: Some(stderr) };
        let hello = serve.next();
        assert_eq!(hello["ev"], "hello", "the first line");
        serve
    }

    fn send(&mut self, request: Value) {
        let stdin = self.stdin.as_mut().expect("the pipe is open");
        writeln!(stdin, "{request}").unwrap();
        stdin.flush().unwrap();
    }

    fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    fn next_in(&mut self, time: Duration) -> Option<Value> {
        match self.lines.recv_timeout(time) {
            Ok(value) => {
                self.seen.push(value.clone());
                Some(value)
            }
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    fn next(&mut self) -> Value {
        self.next_in(LONG).unwrap_or_else(|| panic!("no line in {LONG:?}; seen: {:#?}", self.seen))
    }

    /// The lines up to and with the first one that `is` takes.
    fn until(&mut self, is: impl Fn(&Value) -> bool) -> Vec<Value> {
        let mut got = Vec::new();
        loop {
            let line = self.next();
            let hit = is(&line);
            got.push(line);
            if hit {
                return got;
            }
        }
    }

    /// The events of request `id` up to its last one (other requests' lines are skipped).
    fn request(&mut self, id: &str) -> Vec<Value> {
        self.until(|v| v["id"] == id && is_last(v)).into_iter().filter(|v| v["id"] == id).collect()
    }

    /// Send a request and return its last event.
    fn ask(&mut self, request: Value) -> Value {
        let id = request["id"].as_str().map(str::to_string).unwrap_or_else(|| request["id"].to_string());
        self.send(request);
        self.request(&id).pop().unwrap()
    }

    /// Close the pipe (or not), wait for the process: its status, the lines that still came, its stderr.
    fn end(mut self, close: bool) -> (i32, Vec<Value>, String) {
        if close {
            self.stdin = None;
        }
        let until = Instant::now() + LONG;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < until, "the process did not end; seen: {:#?}", self.seen);
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut rest = Vec::new();
        while let Some(line) = self.next_in(Duration::from_secs(5)) {
            rest.push(line);
        }
        let stderr = self.stderr.take().unwrap().join().unwrap();
        (status.code().unwrap_or(-1), rest, stderr)
    }
}

fn is_last(v: &Value) -> bool {
    matches!(v["ev"].as_str(), Some("ok" | "refused" | "failed" | "cancelled"))
}

fn names(events: &[Value]) -> Vec<&str> {
    events.iter().map(|v| v["ev"].as_str().unwrap()).collect()
}

/// `bye` says nothing is left: one thread, no child.
fn clean_bye(rest: &[Value], why: &str) -> Value {
    let bye = rest.last().unwrap_or_else(|| panic!("no bye line")).clone();
    assert_eq!((bye["ev"].as_str(), bye["why"].as_str()), (Some("bye"), Some(why)), "{bye}");
    assert_eq!(bye["children"], 0, "{bye}");
    if cfg!(any(target_os = "linux", target_os = "android")) {
        assert_eq!(bye["threads"], 1, "only the main thread is left: {bye}");
    }
    assert!(bye.get("forced").is_none(), "{bye}");
    bye
}

fn sha256(data: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E3779B97F4A7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

// ---------------------------------------------------------------- the lines, the queue, progress

#[test]
fn hello_ping_and_the_end_of_the_pipe() {
    let b = Box::new("hello");
    let mut s = Serve::start(&b, &["--jobs", "3", "--nice", "12"]);
    let hello = s.seen[0].clone();
    assert_eq!((hello["proto"].as_u64(), hello["jobs"].as_u64(), hello["nice"].as_i64(), hello["realtime"].as_bool()), (Some(1), Some(3), Some(12), Some(false)), "{hello}");
    assert_eq!(hello["pid"].as_i64(), Some(s.pid() as i64));
    assert_eq!((hello["idle"].as_u64(), hello["battery_min"].as_u64()), (Some(300), Some(10)));
    // the release's version (runtime/package.sh builds with ONEIOS_VERSION), else the crate's (issue 75)
    assert_eq!(hello["tool"].as_str(), Some(option_env!("ONEIOS_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))), "{hello}");
    // the pairing files' format the prelink reads (the app's kb.json kb: 2, the fixtures alone)
    assert_eq!(hello["kb"].as_u64(), Some(2));
    assert_eq!(hello["thermal"], json!([44.0, 46.0, 45.0, 50.0]));
    let cmds: Vec<&str> = hello["cmds"].as_array().unwrap().iter().map(|c| c.as_str().unwrap()).collect();
    for cmd in ["ping", "health", "cancel", "wait", "download", "zip-ls", "zip-get", "fetch", "im4p", "aea", "receive", "sums", "hashes", "verify", "apfs-ls", "apfs-copy", "tweaks", "prelink", "make-image", "stubs-check", "package-check", "ioslog-boots", "ioslog-show", "log-list", "log", "cpu"] {
        assert!(cmds.contains(&cmd), "hello's cmds have no {cmd}: {cmds:?}");
    }
    let ok = s.ask(json!({"id": "p-1", "cmd": "ping"}));
    assert_eq!((ok["ev"].as_str(), ok["cmd"].as_str(), ok["id"].as_str()), (Some("ok"), Some("ping"), Some("p-1")));
    // an id may be a whole number; it comes back as text
    assert_eq!(s.ask(json!({"id": 7, "cmd": "ping"}))["id"], "7");
    let (status, rest, stderr) = s.end(true);
    let bye = clean_bye(&rest, "eof");
    assert_eq!((status, bye["requests"].as_u64(), bye["cancelled"].as_u64()), (0, Some(2), Some(0)));
    assert_eq!(stderr, "", "the tool's standard error stays empty");
}

#[test]
fn a_request_has_steps_progress_and_its_own_threads() {
    let b = Box::new("progress");
    let mut s = Serve::start(&b, &["--jobs", "4"]);
    s.send(json!({"id": "w", "cmd": "wait", "seconds": 1.3}));
    let events = s.request("w");
    assert_eq!(names(&events)[..2], ["start", "step"], "{events:#?}");
    assert_eq!(events[0]["steps"], json!([{"step": "wait", "text": "Waiting"}]));
    assert_eq!((events[0]["jobs"].as_u64(), events[0]["cmd"].as_str()), (Some(4), Some("wait")));
    assert_eq!((events[1]["step"].as_str(), events[1]["n"].as_u64(), events[1]["of"].as_u64(), events[1]["text"].as_str()), (Some("wait"), Some(1), Some(1), Some("Waiting")));
    let progress: Vec<&Value> = events.iter().filter(|v| v["ev"] == "progress").collect();
    // the first and the last line of the step, and at most four a second between
    assert!((2..=9).contains(&progress.len()), "{} progress lines in 1.3 s", progress.len());
    assert_eq!((progress[0]["done"].as_u64(), progress[0]["total"].as_u64(), progress[0]["unit"].as_str(), progress[0]["percent"].as_f64()), (Some(0), Some(1300), Some("ms"), Some(0.0)));
    let last = progress.last().unwrap();
    assert_eq!((last["done"].as_u64(), last["percent"].as_f64()), (Some(1300), Some(100.0)), "{last}");
    let done: Vec<u64> = progress.iter().map(|p| p["done"].as_u64().unwrap()).collect();
    assert!(done.windows(2).all(|w| w[0] <= w[1]), "progress never goes back: {done:?}");
    // after a second of work a rate and an ETA are known
    let late = progress.iter().find(|p| p["done"].as_u64().unwrap() >= 1000 && p["done"].as_u64().unwrap() < 1300);
    if let Some(late) = late {
        assert!(late["rate"].as_u64().is_some_and(|r| (500..=2000).contains(&r)) && late["eta"].as_u64().is_some_and(|e| e <= 2), "{late}");
    }
    let ok = events.last().unwrap();
    assert_eq!((ok["ev"].as_str(), ok["threads"].as_u64(), ok["realtime"].as_bool()), (Some("ok"), Some(4), Some(false)), "{ok}");
    assert!(ok["nice"].as_i64().unwrap() >= 10 && ok["seconds"].as_f64().unwrap() >= 1.2, "the workers run at the process's nice: {ok}");
    // a request's own "jobs" lowers the count, and cannot raise it
    assert_eq!(s.ask(json!({"id": "w2", "cmd": "wait", "seconds": 0.05, "jobs": 2}))["threads"], 2);
    assert_eq!(s.ask(json!({"id": "w3", "cmd": "wait", "seconds": 0.05, "jobs": 8}))["threads"], 4);
    assert_eq!(s.ask(json!({"id": "w4", "cmd": "wait", "seconds": 0.05, "jobs": 9}))["code"], "usage");
    let (status, rest, _) = s.end(true);
    clean_bye(&rest, "eof");
    assert_eq!(status, 0);
}

#[test]
fn requests_wait_in_a_queue_and_ping_does_not() {
    let b = Box::new("queue");
    let mut s = Serve::start(&b, &[]);
    s.send(json!({"id": "a", "cmd": "wait", "seconds": 0.8}));
    s.send(json!({"id": "b", "cmd": "wait", "seconds": 0.1}));
    s.send(json!({"id": "c", "cmd": "wait", "seconds": 0.1}));
    s.send(json!({"id": "p", "cmd": "ping"}));
    let lines = s.until(|v| v["id"] == "c" && is_last(v));
    let at = |id: &str, ev: &str| lines.iter().position(|v| v["id"] == id && v["ev"] == ev).unwrap_or_else(|| panic!("no {ev} of {id}: {lines:#?}"));
    assert_eq!((lines[at("b", "queued")]["ahead"].as_u64(), lines[at("c", "queued")]["ahead"].as_u64()), (Some(1), Some(2)));
    assert!(lines.iter().all(|v| !(v["id"] == "a" && v["ev"] == "queued")), "the first request does not wait");
    // ping is answered while a runs; b starts only after a's last event, c after b's
    assert!(at("p", "ok") < at("a", "ok"), "{lines:#?}");
    assert!(at("a", "ok") < at("b", "start") && at("b", "ok") < at("c", "start"), "{lines:#?}");
    // a second request with a live id is not taken, and the first goes on
    s.send(json!({"id": "d", "cmd": "wait", "seconds": 0.4}));
    s.send(json!({"id": "d", "cmd": "ping"}));
    let lines = s.until(|v| v["id"] == "d" && is_last(v));
    let error = lines.iter().find(|v| v["ev"] == "error").expect("an error line");
    assert_eq!((error["code"].as_str(), error["id"].as_str()), (Some("duplicate"), Some("d")));
    assert_eq!(lines.last().unwrap()["ev"], "ok");
    // the id is free again after its last event
    assert_eq!(s.ask(json!({"id": "d", "cmd": "ping"}))["ev"], "ok");
    let (status, rest, _) = s.end(true);
    assert_eq!((status, clean_bye(&rest, "eof")["requests"].as_u64()), (0, Some(6)));
}

#[test]
fn the_queue_is_bounded() {
    let b = Box::new("busy");
    let mut s = Serve::start(&b, &[]);
    s.send(json!({"id": "run", "cmd": "wait", "seconds": 30}));
    for i in 0..33 {
        s.send(json!({"id": format!("q{i}"), "cmd": "wait", "seconds": 30}));
    }
    let lines = s.until(|v| v["id"] == "q32" && is_last(v));
    let last = lines.last().unwrap();
    assert_eq!((last["ev"].as_str(), last["code"].as_str()), (Some("refused"), Some("busy")), "the 33rd waiting request: {last}");
    assert_eq!(lines.iter().filter(|v| v["ev"] == "queued").count(), 32);
    // the end of the pipe: the running one is cancelled, the 32 waiting ones are dropped, each with its last event
    let (status, rest, _) = s.end(true);
    let cancelled: Vec<&Value> = rest.iter().filter(|v| v["ev"] == "cancelled").collect();
    assert_eq!(cancelled.len(), 33, "{rest:#?}");
    assert!(cancelled.iter().all(|v| v["by"] == "eof"));
    assert_eq!((status, clean_bye(&rest, "eof")["cancelled"].as_u64()), (5, Some(33)));
}

// ---------------------------------------------------------------- the ways it ends

/// A `wait` of 60 s with a temporary file in the stage, started and seen to run.
fn long_wait(s: &mut Serve, b: &Box, id: &str) -> String {
    let file = b.stage(&format!("work/{id}.tmp"));
    s.send(json!({"id": id, "cmd": "wait", "seconds": 60, "file": file}));
    s.until(|v| v["id"] == id && v["ev"] == "progress");
    assert!(Path::new(&file).is_file(), "the request's temporary file is there while it runs");
    file
}

#[test]
fn a_cancel_request_stops_the_running_request_and_removes_its_file() {
    let b = Box::new("cancel");
    let mut s = Serve::start(&b, &[]);
    let file = long_wait(&mut s, &b, "long");
    s.send(json!({"id": "next", "cmd": "wait", "seconds": 0.05}));
    let began = Instant::now();
    s.send(json!({"cmd": "cancel", "id": "long"}));
    let events = s.request("long");
    let last = events.last().unwrap();
    assert_eq!((last["ev"].as_str(), last["by"].as_str(), last["removed"].as_u64(), last["cmd"].as_str()), (Some("cancelled"), Some("request"), Some(1), Some("wait")), "{last}");
    assert!(began.elapsed() < Duration::from_secs(8), "a cancel is quick: {:?}", began.elapsed());
    assert!(!Path::new(&file).exists(), "the temporary file is gone");
    // the queue goes on with the next request
    assert_eq!(s.request("next").last().unwrap()["ev"], "ok");
    // a waiting request is taken out of the queue; a cancel of an id that is not there is an error line
    let file = long_wait(&mut s, &b, "long2");
    s.send(json!({"id": "waiting", "cmd": "wait", "seconds": 60}));
    s.send(json!({"cmd": "cancel", "id": "waiting"}));
    let last = s.request("waiting").pop().unwrap();
    assert_eq!((last["ev"].as_str(), last["by"].as_str(), last["removed"].as_u64()), (Some("cancelled"), Some("request"), Some(0)), "{last}");
    assert!(last["text"].as_str().unwrap().contains("was waiting"));
    s.send(json!({"cmd": "cancel", "id": "nobody"}));
    let error = s.until(|v| v["ev"] == "error").pop().unwrap();
    assert_eq!((error["code"].as_str(), error["id"].as_str()), (Some("no-such-request"), Some("nobody")));
    s.send(json!({"cmd": "cancel", "id": "long2"}));
    assert_eq!(s.request("long2").pop().unwrap()["ev"], "cancelled");
    assert!(!Path::new(&file).exists());
    // nothing after a request's last event carries its id
    assert_eq!(s.ask(json!({"id": "after", "cmd": "ping"}))["ev"], "ok");
    let (status, rest, stderr) = s.end(true);
    let bye = clean_bye(&rest, "eof");
    assert_eq!((status, bye["cancelled"].as_u64(), stderr.as_str()), (0, Some(3), ""), "the end cancelled nothing: status 0");
    for id in ["long", "long2", "waiting"] {
        let all: Vec<&Value> = s_seen(&rest, id);
        assert!(all.is_empty(), "{id} has lines after its last event: {all:?}");
    }
}

fn s_seen<'a>(lines: &'a [Value], id: &str) -> Vec<&'a Value> {
    lines.iter().filter(|v| v["id"] == id).collect()
}

#[test]
fn the_end_of_the_pipe_in_the_middle_of_a_request() {
    let b = Box::new("eof");
    let mut s = Serve::start(&b, &[]);
    let file = long_wait(&mut s, &b, "long");
    s.send(json!({"id": "waiting", "cmd": "wait", "seconds": 60}));
    s.until(|v| v["id"] == "waiting" && v["ev"] == "queued");
    let pid = s.pid();
    let began = Instant::now();
    let (status, rest, stderr) = s.end(true);
    assert!(began.elapsed() < Duration::from_secs(9), "the process ends soon after the pipe: {:?}", began.elapsed());
    let last = |id: &str| rest.iter().find(|v| v["id"] == id && is_last(v)).unwrap_or_else(|| panic!("no last event of {id}: {rest:#?}")).clone();
    assert_eq!((last("long")["ev"].as_str(), last("long")["by"].as_str(), last("long")["removed"].as_u64()), (Some("cancelled"), Some("eof"), Some(1)));
    assert_eq!((last("waiting")["ev"].as_str(), last("waiting")["by"].as_str()), (Some("cancelled"), Some("eof")));
    let bye = clean_bye(&rest, "eof");
    assert_eq!((status, bye["cancelled"].as_u64(), stderr.as_str()), (5, Some(2), ""));
    assert!(!Path::new(&file).exists(), "the temporary file is gone");
    // the process is gone, and with it every thread
    assert!(!Path::new(&format!("/proc/{pid}")).exists() || !cfg!(target_os = "linux"));
}

#[test]
fn sigterm_in_the_middle_of_a_request() {
    let b = Box::new("sigterm");
    let mut s = Serve::start(&b, &[]);
    let file = long_wait(&mut s, &b, "long");
    let pid = s.pid();
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let (status, rest, stderr) = s.end(false);
    let last = rest.iter().find(|v| v["id"] == "long" && is_last(v)).expect("the request's last event");
    assert_eq!((last["ev"].as_str(), last["by"].as_str(), last["removed"].as_u64()), (Some("cancelled"), Some("sigterm"), Some(1)), "{last}");
    clean_bye(&rest, "sigterm");
    assert_eq!((status, stderr.as_str()), (5, ""));
    assert!(!Path::new(&file).exists());
    // with nothing running: bye at once, status 0
    let s = Serve::start(&b, &[]);
    unsafe { libc::kill(s.pid(), libc::SIGTERM) };
    let (status, rest, _) = s.end(false);
    assert_eq!((status, clean_bye(&rest, "sigterm")["requests"].as_u64()), (0, Some(0)));
}

#[test]
fn the_idle_timeout() {
    let b = Box::new("idle");
    // nothing asked: the process ends after --idle seconds
    let began = Instant::now();
    let s = Serve::start(&b, &["--idle", "1"]);
    let (status, rest, _) = s.end(false);
    assert_eq!(status, 0);
    clean_bye(&rest, "idle");
    assert!((Duration::from_millis(900)..Duration::from_secs(8)).contains(&began.elapsed()), "{:?}", began.elapsed());
    // a request that runs longer than the idle time is not cut; the time counts from its end
    let mut s = Serve::start(&b, &["--idle", "1"]);
    let began = Instant::now();
    assert_eq!(s.ask(json!({"id": "w", "cmd": "wait", "seconds": 1.6}))["ev"], "ok");
    let ended = began.elapsed();
    let (status, rest, _) = s.end(false);
    assert_eq!((status, clean_bye(&rest, "idle")["requests"].as_u64()), (0, Some(1)));
    assert!(began.elapsed() >= ended + Duration::from_millis(900), "idle counts from the request's end: {:?} then {:?}", ended, began.elapsed());
    // --idle 0: never (the pipe's end ends it)
    let mut s = Serve::start(&b, &["--idle", "0"]);
    assert!(s.next_in(Duration::from_millis(1500)).is_none(), "no bye without a reason");
    let (status, rest, _) = s.end(true);
    assert_eq!(status, 0);
    clean_bye(&rest, "eof");
}

#[test]
fn a_reader_that_went_away() {
    let b = Box::new("stdout");
    let mut child = Command::new(tool()).arg("serve").args(["--stage", &b.stage("")]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut hello = String::new();
    out.read_line(&mut hello).unwrap();
    let file = b.stage("work/gone.tmp");
    writeln!(stdin, "{}", json!({"id": "long", "cmd": "wait", "seconds": 60, "file": file})).unwrap();
    let mut line = String::new();
    while !line.contains("\"progress\"") {
        line.clear();
        out.read_line(&mut line).unwrap();
    }
    assert!(Path::new(&file).is_file());
    // the app is gone: nobody reads; the next progress line cannot be written
    drop(out);
    let until = Instant::now() + LONG;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < until, "the process did not end when its reader went away");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(5));
    assert!(!Path::new(&file).exists(), "the temporary file is gone");
    drop(stdin);
}

// ---------------------------------------------------------------- refusals

#[test]
fn refusals_say_why_with_a_code() {
    let b = Box::new("refusals");
    let mut s = Serve::start(&b, &[]);
    let outside = b.top.join("elsewhere/x").to_string_lossy().into_owned();
    let image = b.put("in/a.img", b"x");
    for (request, code, words) in [
        (json!({"id": "1", "cmd": "format-the-phone"}), "unknown", "no command"),
        (json!({"id": "2", "cmd": "sums"}), "usage", "\"dir\" is missing"),
        (json!({"id": "3", "cmd": "wait", "secnods": 1}), "usage", "no key \"secnods\""),
        (json!({"id": "4", "cmd": "wait", "seconds": "one"}), "usage", "must be a number"),
        (json!({"id": "5", "cmd": "wait", "file": outside}), "outside", "outside the stage"),
        (json!({"id": "6", "cmd": "wait", "file": "relative.tmp"}), "outside", "absolute"),
        (json!({"id": "7", "cmd": "wait", "file": format!("{}/../x", b.stage("work"))}), "outside", "\"..\""),
        (json!({"id": "8", "cmd": "sums", "dir": b.stage("no-such-folder")}), "not-found", "is not there"),
        (json!({"id": "9", "cmd": "tweaks"}), "usage", "\"data\" is missing"),
        (json!({"id": "10", "cmd": "hashes", "image": image, "out": outside}), "outside", "outside the stage"),
        (json!({"id": "11", "cmd": "hashes", "image": image, "out": b.stage("a.hashes"), "chunk": 1000}), "usage", "power of two"),
        (json!({"id": "12", "cmd": "apfs-copy", "image": image, "src": "/", "dst": b.stage("")}), "outside", "outside the stage"),
        (json!({"id": "13", "cmd": "prelink", "kernel": b.stage("no.macho")}), "not-found", "is not there"),
        (json!({"id": "14", "cmd": "log", "file": "../../etc/passwd"}), "usage", "--file"),
        (json!({"id": "15", "cmd": "log", "filters": ["everything"]}), "usage", "no filter"),
        (json!({"id": "16", "cmd": "log"}), "not-found", "log-list"),
        (json!({"id": "17", "cmd": "download", "url": "ftp://x/y", "out": b.stage("y"), "sha256": "0".repeat(64)}), "usage", "https"),
        (json!({"id": "18", "cmd": "download", "url": "https://127.0.0.1:9/y", "out": b.stage("y"), "sha256": "xyz"}), "usage", "64 lower-case hex"),
        (json!({"id": "19", "cmd": "receive", "from": image, "to": b.stage("r/a"), "bytes": 1, "sha256": "0".repeat(64), "mode": "0777"}), "usage", "0644"),
        (json!({"id": "20", "cmd": "ping", "extra": 1}), "usage", "no key \"extra\""),
    ] {
        let last = s.ask(request.clone());
        assert_eq!((last["ev"].as_str(), last["code"].as_str()), (Some("refused"), Some(code)), "{request}: {last}");
        assert!(last["text"].as_str().unwrap().contains(words), "{request}: {last}");
    }
    // lines that are not requests
    for (line, words) in [("hello", "not a JSON line"), ("[]", "not a JSON object"), ("{\"cmd\":\"ping\"}", "no \"id\""), ("{\"id\":\"a b\",\"cmd\":\"ping\"}", "1 to 64 characters")] {
        writeln!(s.stdin.as_mut().unwrap(), "{line}").unwrap();
        let error = s.until(|v| v["ev"] == "error").pop().unwrap();
        assert!(error["text"].as_str().unwrap().contains(words), "{line}: {error}");
    }
    // nothing was written outside the stage, and a refused request leaves nothing in it
    assert!(!b.top.join("elsewhere").exists());
    assert!(!Path::new(&b.stage("a.hashes")).exists() && !Path::new(&b.stage("a.hashes.part")).exists() && !Path::new(&b.stage("r")).join("a").exists());
    let (status, rest, _) = s.end(true);
    assert_eq!(status, 0);
    clean_bye(&rest, "eof");
}

#[test]
fn a_session_refuses_heavy_requests_and_lets_the_readings_run() {
    let b = Box::new("session");
    b.sys("proc/sys/kernel/random/boot_id", "boot-1\n");
    b.put("stage/control/boot", b"boot-1\n");
    b.put("stage/control/state", b"running\n");
    b.put("stage/session/xnuhost.log", b"[    1.000] xnuhost I one\n[    2.000] guest     oneios-gpu-report: two\n");
    b.put("stage/image.sha256", b"");
    std::fs::create_dir_all(b.stage("image")).unwrap();
    let mut s = Serve::start(&b, &[]);
    let health = s.ask(json!({"id": "h", "cmd": "health"}));
    assert_eq!((health["ev"].as_str(), health["session"].as_bool(), health["state"].as_str()), (Some("ok"), Some(true), Some("unknown")), "{health}");
    for request in [json!({"id": "1", "cmd": "sums", "dir": b.stage("image")}), json!({"id": "2", "cmd": "wait", "seconds": 0.1, "heavy": true}), json!({"id": "3", "cmd": "tweaks"})] {
        let last = s.ask(request);
        assert_eq!((last["ev"].as_str(), last["code"].as_str()), (Some("refused"), Some("session")), "{last}");
        assert!(last["text"].as_str().unwrap().contains("return to One UI first"));
    }
    // the readings run beside a session
    assert_eq!(s.ask(json!({"id": "4", "cmd": "log", "filters": ["gpu"]}))["matched"], 1);
    assert_eq!(s.ask(json!({"id": "5", "cmd": "log-list"}))["logs"], 1);
    assert_eq!(s.ask(json!({"id": "6", "cmd": "wait", "seconds": 0.05}))["ev"], "ok");
    // the session ended: heavy requests run again
    b.put("stage/control/state", b"returned\n");
    assert_eq!(s.ask(json!({"id": "7", "cmd": "sums", "dir": b.stage("image")}))["ev"], "ok");
    s.end(true);
}

// ---------------------------------------------------------------- heat and battery

fn heat(b: &Box, case: &str, battery: &str, level: &str, status: &str) {
    b.sys("sys/class/power_supply/battery/lrp", &format!("{case}\n"));
    b.sys("sys/class/power_supply/battery/temp", &format!("{battery}\n"));
    b.sys("sys/class/power_supply/battery/capacity", &format!("{level}\n"));
    b.sys("sys/class/power_supply/battery/status", &format!("{status}\n"));
}

#[test]
fn heat_pauses_and_stops_and_the_battery_refuses() {
    let b = Box::new("heat");
    heat(&b, "380", "330", "80", "Discharging");
    let mut s = Serve::start(&b, &[]);
    let health = s.ask(json!({"id": "h1", "cmd": "health"}));
    assert_eq!((health["case"].as_f64(), health["battery"].as_f64(), health["level"].as_u64(), health["charging"].as_bool(), health["state"].as_str()), (Some(38.0), Some(33.0), Some(80), Some(false), Some("ok")), "{health}");

    // the case at its pause limit: the work waits, and goes on when it is 2 C below
    heat(&b, "445", "330", "80", "Discharging");
    assert_eq!(s.ask(json!({"id": "h2", "cmd": "health"}))["state"], "pause");
    s.send(json!({"id": "warm", "cmd": "wait", "seconds": 0.3, "heavy": true}));
    let paused = s.until(|v| v["ev"] == "heat").pop().unwrap();
    assert_eq!((paused["state"].as_str(), paused["why"].as_str(), paused["case"].as_f64(), paused["id"].as_str()), (Some("paused"), Some("case"), Some(44.5), Some("warm")), "{paused}");
    assert!(paused["text"].as_str().unwrap().contains("44.5 C") && paused["text"].as_str().unwrap().contains("below 42.0 C"), "{paused}");
    // 43 C is not cool enough: no line for 2.5 s
    heat(&b, "430", "330", "80", "Discharging");
    assert!(s.next_in(Duration::from_millis(2500)).is_none_or(|v| v["ev"] != "heat" && !is_last(&v)), "the pause holds at 43 C");
    heat(&b, "415", "330", "80", "Discharging");
    let events = s.request("warm");
    let resumed = events.iter().find(|v| v["ev"] == "heat").expect("a resumed line");
    assert_eq!((resumed["state"].as_str(), resumed["case"].as_f64()), (Some("resumed"), Some(41.5)), "{resumed}");
    assert_eq!(events.last().unwrap()["ev"], "ok");

    // a light request is not held
    heat(&b, "445", "330", "80", "Discharging");
    assert_eq!(s.ask(json!({"id": "light", "cmd": "wait", "seconds": 0.05}))["ev"], "ok");

    // the stop limit, reached while a request runs: it ends with refused, heat, and its file is gone
    heat(&b, "380", "330", "80", "Discharging");
    let file = b.stage("work/hot.tmp");
    s.send(json!({"id": "hot", "cmd": "wait", "seconds": 60, "heavy": true, "file": file}));
    s.until(|v| v["id"] == "hot" && v["ev"] == "progress");
    heat(&b, "380", "505", "80", "Discharging");
    let last = s.request("hot").pop().unwrap();
    assert_eq!((last["ev"].as_str(), last["code"].as_str()), (Some("refused"), Some("heat")), "{last}");
    assert!(last["text"].as_str().unwrap().starts_with("The battery is at 50.5 C"), "{last}");
    assert!(!Path::new(&file).exists());
    // at the stop limit a heavy request does not start
    let last = s.ask(json!({"id": "hot2", "cmd": "wait", "seconds": 0.1, "heavy": true}));
    assert_eq!((last["code"].as_str(), s.ask(json!({"id": "h3", "cmd": "health"}))["state"].as_str()), (Some("heat"), Some("stop")));

    // the battery's level counts when it is not charging
    heat(&b, "380", "330", "7", "Discharging");
    let last = s.ask(json!({"id": "low", "cmd": "wait", "seconds": 0.1, "heavy": true}));
    assert_eq!((last["ev"].as_str(), last["code"].as_str()), (Some("refused"), Some("battery")), "{last}");
    assert!(last["text"].as_str().unwrap().contains("7 %") && last["text"].as_str().unwrap().contains("charger"));
    heat(&b, "380", "330", "7", "Charging");
    assert_eq!(s.ask(json!({"id": "charging", "cmd": "wait", "seconds": 0.1, "heavy": true}))["ev"], "ok");
    let (status, rest, _) = s.end(true);
    assert_eq!(status, 0);
    clean_bye(&rest, "eof");

    // --thermal off and --battery-min 0: no check; other limits are the options'
    heat(&b, "600", "600", "1", "Discharging");
    let mut s = Serve::start(&b, &["--thermal", "off", "--battery-min", "0"]);
    assert_eq!(s.seen[0]["thermal"], Value::Null);
    assert_eq!(s.ask(json!({"id": "w", "cmd": "wait", "seconds": 0.05, "heavy": true}))["ev"], "ok");
    s.end(true);
    heat(&b, "410", "330", "80", "Discharging");
    let mut s = Serve::start(&b, &["--thermal", "38,40,45,50"]);
    assert_eq!(s.ask(json!({"id": "w", "cmd": "wait", "seconds": 0.05, "heavy": true}))["code"], "heat");
    s.end(true);
}

// ---------------------------------------------------------------- the commands

#[test]
fn sums_checks_a_folder_against_its_list() {
    let b = Box::new("sums");
    let files: Vec<(&str, Vec<u8>)> = vec![("kernel.kc", noise(3_000_000, 1)), ("os.img", noise(5_000_000, 2)), ("overlay/userland/xnuhost.conf", b"scale=3\n".to_vec()), ("empty", Vec::new())];
    let mut list = String::new();
    for (path, data) in &files {
        b.put(&format!("stage/image/{path}"), data);
        list += &format!("{}  {path}\n", sha256(data));
    }
    b.put("stage/image.sha256", list.as_bytes());
    let mut s = Serve::start(&b, &["--jobs", "4"]);
    s.send(json!({"id": "s", "cmd": "sums", "dir": b.stage("image")}));
    let events = s.request("s");
    assert_eq!(events[0]["steps"], json!([{"step": "scan", "text": "Reading the list"}, {"step": "hash", "text": "Checking the files"}]));
    let steps: Vec<&str> = events.iter().filter(|v| v["ev"] == "step").map(|v| v["step"].as_str().unwrap()).collect();
    assert_eq!(steps, ["scan", "hash"]);
    let rows: Vec<&Value> = events.iter().filter(|v| v["ev"] == "row").collect();
    assert_eq!(rows.len(), 4);
    for (path, data) in &files {
        let row = rows.iter().find(|r| r["path"] == *path).unwrap_or_else(|| panic!("no row of {path}"));
        assert_eq!((row["kind"].as_str(), row["sha256"].as_str(), row["bytes"].as_u64(), row["ok"].as_bool()), (Some("file"), Some(sha256(data).as_str()), Some(data.len() as u64), Some(true)));
    }
    let total: u64 = files.iter().map(|(_, d)| d.len() as u64).sum();
    let last_progress = events.iter().filter(|v| v["ev"] == "progress").next_back().unwrap();
    assert_eq!((last_progress["done"].as_u64(), last_progress["total"].as_u64(), last_progress["unit"].as_str(), last_progress["items_done"].as_u64(), last_progress["items_total"].as_u64()), (Some(total), Some(total), Some("bytes"), Some(4), Some(4)), "{last_progress}");
    let ok = events.last().unwrap();
    assert_eq!((ok["ev"].as_str(), ok["files"].as_u64(), ok["bytes"].as_u64()), (Some("ok"), Some(4), Some(total)), "{ok}");

    // a changed byte, a missing file, and (strict) a file the list does not name
    let mut changed = files[1].1.clone();
    changed[4_000_000] ^= 1;
    b.put("stage/image/os.img", &changed);
    std::fs::remove_file(b.stage("image/empty")).unwrap();
    b.put("stage/image/stray.bin", b"x");
    let last = s.ask(json!({"id": "s2", "cmd": "sums", "dir": b.stage("image"), "list": b.stage("image.sha256")}));
    assert_eq!((last["ev"].as_str(), last["code"].as_str()), (Some("refused"), Some("mismatch")), "{last}");
    let text = last["text"].as_str().unwrap();
    assert!(text.contains("2 of 4 files") && text.contains("os.img (another SHA-256)") && text.contains("empty (missing)") && !text.contains("stray"), "{text}");
    let text = s.ask(json!({"id": "s3", "cmd": "sums", "dir": b.stage("image"), "strict": true}))["text"].as_str().unwrap().to_string();
    assert!(text.contains("1 files of") && text.contains("stray.bin"), "{text}");
    // a list that names a path outside the folder is not followed
    b.put("stage/bad.sha256", format!("{}  ../image.sha256\n", "0".repeat(64)).as_bytes());
    let last = s.ask(json!({"id": "s4", "cmd": "sums", "dir": b.stage("image"), "list": b.stage("bad.sha256")}));
    assert_eq!(last["code"], "mismatch");
    assert!(last["text"].as_str().unwrap().contains("line 1"));
    s.end(true);
}

#[test]
fn hashes_writes_the_list_the_image_command_writes() {
    let b = Box::new("hashes");
    let data = noise(3 * (1 << 20) + 777, 9);
    let image = b.put("in/os.img", &data);
    let mut s = Serve::start(&b, &[]);
    let out = b.stage("set/os.img.hashes");
    let ok = s.ask(json!({"id": "h", "cmd": "hashes", "image": image, "out": out}));
    assert_eq!((ok["ev"].as_str(), ok["bytes"].as_u64(), ok["sha256"].as_str(), ok["chunks"].as_u64()), (Some("ok"), Some(data.len() as u64), Some(sha256(&data).as_str()), Some(4)), "{ok}");
    // the same bytes as `oneios-tool image hashes` (one thread and all)
    for jobs in ["1", "4"] {
        let other = b.top.join(format!("cli-{jobs}.hashes"));
        let done = Command::new(tool()).args(["--jobs", jobs, "image", "hashes", &image]).arg(&other).output().unwrap();
        assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
        assert_eq!(std::fs::read(&out).unwrap(), std::fs::read(&other).unwrap(), "--jobs {jobs}");
    }
    assert!(!Path::new(&format!("{out}.part")).exists());
    s.end(true);
}

/// A stand-in OS root for `tweaks` (the shape the tweaks read; nothing of Apple's).
fn stand_in_root(dir: &Path) {
    use oneios_tweaks::plist::{dumps_binary, dumps_xml, P};
    let d = |pairs: Vec<(&str, P)>| P::Dict(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect());
    let put = |rel: &str, data: &[u8]| {
        std::fs::create_dir_all(dir.join(rel).parent().unwrap()).unwrap();
        std::fs::write(dir.join(rel), data).unwrap();
    };
    put("System/Library/CoreServices/SystemVersion.plist", &dumps_xml(&d(vec![("ProductBuildVersion", P::Str("24A999".into())), ("ProductName", P::Str("iPhone OS".into())), ("ProductVersion", P::Str("27.0.1".into()))])).unwrap());
    put("sbin/launchd", b"not a real launchd\n");
    let job = |label: &str| d(vec![("Label", P::Str(label.into())), ("ProgramArguments", P::Array(vec![P::Str("/usr/libexec/x".into())]))]);
    let daemons = d(["com.apple.watchdogd", "com.apple.SpringBoard", "com.apple.OTATaskingAgent"].iter().map(|l| (*l, job(l))).collect());
    put("System/Library/xpc/launchd.plist", &dumps_binary(&d(vec![("LaunchDaemons", daemons)])).unwrap());
    let over = d(vec![("com.apple.OTATaskingAgent", d(vec![("DisableAllowed", P::Bool(false))]))]);
    put("System/Library/LaunchDaemons/com.apple.jetsamproperties.V54.plist", &dumps_binary(&d(vec![("Version4", d(vec![("Daemon", d(vec![("Override", over)]))]))])).unwrap());
}

#[test]
fn tweaks_writes_what_the_hook_command_writes() {
    let b = Box::new("tweaks");
    let root = b.top.join("in/root");
    stand_in_root(&root);
    for side in ["serve", "cli"] {
        std::fs::create_dir_all(b.top.join(format!("stage/{side}/data"))).unwrap();
        std::fs::create_dir_all(b.top.join(format!("stage/{side}/overlay"))).unwrap();
        b.put(&format!("stage/{side}/owners.tsv"), b"");
    }
    let mut s = Serve::start(&b, &[]);
    // the inputs are checked before anything is written
    for (request, code, words) in [
        (json!({"data": b.stage("serve/data"), "overlay": b.stage("serve/overlay")}), "usage", "\"os_root\""),
        (json!({"data": b.stage("serve/data"), "overlay": b.stage("serve/overlay"), "os_root": b.top.join("in/none").to_string_lossy()}), "not-found", "is not there"),
        (json!({"data": b.stage("serve/data"), "overlay": b.top.join("elsewhere").to_string_lossy(), "os_root": root.to_string_lossy()}), "outside", "outside the stage"),
        (json!({"data": b.stage("serve/data"), "overlay": b.stage("serve/overlay"), "os_root": root.to_string_lossy(), "scale": 4}), "usage", "2 or 3"),
        (json!({"data": b.stage("serve/data"), "overlay": b.stage("serve/overlay"), "os_root": root.to_string_lossy(), "args": ["--group", "nonesuch"]}), "input", "no daemon group"),
    ] {
        let mut request = request;
        request["id"] = json!("r");
        request["cmd"] = json!("tweaks");
        let last = s.ask(request.clone());
        assert_eq!((last["ev"].as_str(), last["code"].as_str()), (Some("refused"), Some(code)), "{request}: {last}");
        assert!(last["text"].as_str().unwrap().contains(words), "{request}: {last}");
    }
    let request = json!({"id": "t", "cmd": "tweaks", "os_root": root.to_string_lossy(), "data": b.stage("serve/data"), "overlay": b.stage("serve/overlay"),
        "owners": b.stage("serve/owners.tsv"), "info": b.stage("serve/info.json"), "build": "24A999", "scale": 2, "args": ["--keep", "com.apple.momentsd"]});
    s.send(request);
    let events = s.until(is_last);
    let ok = events.last().unwrap();
    assert_eq!((ok["ev"].as_str(), ok["build"].as_str(), ok["disabled_jobs"].as_u64(), ok["owner_cdhashes"].as_u64()), (Some("ok"), Some("24A999"), Some(2), Some(0)), "{ok}");
    // the report's lines are the request's lines; the two steps are said
    assert!(events.iter().any(|e| e["ev"] == "line" && e["text"].as_str().is_some_and(|t| t.contains("disabled.plist: 2 jobs disabled"))), "{events:?}");
    assert!(events.iter().any(|e| e["ev"] == "step" && e["step"] == "place"), "{events:?}");
    let (status, rest, _) = s.end(true);
    assert_eq!(status, 0);
    clean_bye(&rest, "eof");
    // the command line's hook on the same inputs: the same files, byte for byte
    let cli = b.top.join("stage/cli");
    let done = Command::new(tool())
        .args(["image", "tweaks", "hook"])
        .env("ONEIOS_HOOK_API", "1")
        .env("ONEIOS_OS_ROOT", &root)
        .env("ONEIOS_DATA", cli.join("data"))
        .env("ONEIOS_OVERLAY", cli.join("overlay"))
        .env("ONEIOS_OWNERS", cli.join("owners.tsv"))
        .env("ONEIOS_INFO", cli.join("info.json"))
        .env("ONEIOS_BUILD", "24A999")
        .env("ONEIOS_SCALE", "2")
        .env("ONEIOS_ROOTTWEAK_ARGS", "--keep com.apple.momentsd")
        .output()
        .unwrap();
    assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
    fn files(dir: &Path, rel: &str, out: &mut Vec<(String, Vec<u8>)>) {
        let mut names: Vec<_> = std::fs::read_dir(dir.join(rel)).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        for n in names {
            let r = if rel.is_empty() { n } else { format!("{rel}/{n}") };
            if dir.join(&r).is_dir() {
                out.push((r.clone(), Vec::new()));
                files(dir, &r, out);
            } else {
                out.push((r.clone(), std::fs::read(dir.join(&r)).unwrap()));
            }
        }
    }
    let (mut a, mut c) = (Vec::new(), Vec::new());
    files(&b.top.join("stage/serve"), "", &mut a);
    files(&cli, "", &mut c);
    assert!(a.len() > 40, "{}", a.len());
    assert_eq!(a.iter().map(|(n, _)| n).collect::<Vec<_>>(), c.iter().map(|(n, _)| n).collect::<Vec<_>>());
    for ((name, x), (_, y)) in a.iter().zip(&c) {
        assert!(x == y, "{name} differs");
    }
}

#[test]
fn receive_checks_size_and_sum_before_the_file_has_its_name() {
    let b = Box::new("receive");
    let data = noise(2_500_000, 5);
    let from = b.put("app/os.img", &data);
    let mut s = Serve::start(&b, &[]);
    let (to, sums, ids) = (b.stage("image.new/sub/os.img"), b.stage("image.new.sha256"), b.stage("image.new.ids"));
    let request = |id: &str, from: &str, to: &str, bytes: usize, sum: &str| json!({"id": id, "cmd": "receive", "from": from, "to": to, "bytes": bytes, "sha256": sum, "mode": "0755", "name": "sub/os.img", "sums_list": sums, "ids_list": ids});
    s.send(request("r", &from, &to, data.len(), &sha256(&data)));
    let events = s.request("r");
    let steps: Vec<&str> = events.iter().filter(|v| v["ev"] == "step").map(|v| v["step"].as_str().unwrap()).collect();
    assert_eq!(steps, ["copy", "check", "place"]);
    let space = events.iter().find(|v| v["ev"] == "space").expect("a space line");
    assert_eq!((space["need"].as_u64(), space["reserve"].as_u64()), (Some(data.len() as u64), Some(268_435_456)));
    assert!(space["free"].as_u64().unwrap() > 0 && space["path"].as_str().unwrap().contains("stage"));
    assert_eq!(events.last().unwrap()["ev"], "ok", "{events:#?}");
    assert_eq!(std::fs::read(&to).unwrap(), data);
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = std::fs::metadata(&to).unwrap();
    assert_eq!(meta.permissions().mode() & 0o7777, 0o755);
    // the lists, as oneios_root.sh receive writes them
    assert_eq!(std::fs::read_to_string(&sums).unwrap(), format!("{}  sub/os.img\n", sha256(&data)));
    assert_eq!(std::fs::read_to_string(&ids).unwrap(), format!("{}:{}:{}:{} sub/os.img\n", meta.dev(), meta.ino(), meta.size(), meta.mtime()));

    // received already; another size; another sum: refused, and nothing is left
    assert_eq!(s.ask(request("r2", &from, &to, data.len(), &sha256(&data)))["code"], "exists");
    let to2 = b.stage("image.new/second.img");
    let last = s.ask(request("r3", &from, &to2, data.len() - 1, &sha256(&data)));
    assert_eq!(last["code"], "mismatch");
    assert!(last["text"].as_str().unwrap().contains("were announced"), "{last}");
    let last = s.ask(request("r4", &from, &to2, data.len(), &sha256(b"other")));
    assert_eq!((last["ev"].as_str(), last["code"].as_str()), (Some("refused"), Some("mismatch")));
    assert!(last["text"].as_str().unwrap().contains("did not arrive intact (SHA-256 differs)"), "{last}");
    assert!(!Path::new(&to2).exists() && !Path::new(&format!("{to2}.part")).exists());
    assert_eq!(std::fs::read_to_string(&sums).unwrap().lines().count(), 1, "a refused file is not listed");

    // from a FIFO the app writes: the bytes as they come; fewer than announced is refused
    let fifo = b.top.join("app/pipe").to_string_lossy().into_owned();
    let make = |path: &str| {
        let c = std::ffi::CString::new(path).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    };
    make(&fifo);
    let (bytes, sum) = (data.len(), sha256(&data));
    s.send(json!({"id": "f", "cmd": "receive", "from": fifo, "to": to2, "bytes": bytes, "sha256": sum, "mode": "0644"}));
    let writer = {
        let (fifo, data) = (fifo.clone(), data.clone());
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let mut pipe = std::fs::OpenOptions::new().write(true).open(fifo).unwrap();
            for part in data.chunks(100_000) {
                pipe.write_all(part).unwrap();
            }
        })
    };
    assert_eq!(s.request("f").pop().unwrap()["ev"], "ok");
    writer.join().unwrap();
    assert_eq!(std::fs::read(&to2).unwrap(), data);
    assert_eq!(std::fs::metadata(&to2).unwrap().permissions().mode() & 0o7777, 0o644);
    let to3 = b.stage("image.new/third.img");
    s.send(json!({"id": "short", "cmd": "receive", "from": fifo, "to": to3, "bytes": bytes, "sha256": sum, "mode": "0644"}));
    {
        let mut pipe = std::fs::OpenOptions::new().write(true).open(&fifo).unwrap();
        pipe.write_all(&data[..1000]).unwrap();
    }
    let last = s.request("short").pop().unwrap();
    assert_eq!(last["code"], "mismatch");
    assert!(last["text"].as_str().unwrap().contains(&format!("1000 of {bytes} bytes arrived")), "{last}");
    // a FIFO nobody writes: the request waits, and a cancel ends it
    s.send(json!({"id": "nobody", "cmd": "receive", "from": fifo, "to": to3, "bytes": bytes, "sha256": sum, "mode": "0644"}));
    s.until(|v| v["id"] == "nobody" && v["ev"] == "progress");
    s.send(json!({"cmd": "cancel", "id": "nobody"}));
    let last = s.request("nobody").pop().unwrap();
    assert_eq!((last["ev"].as_str(), last["removed"].as_u64()), (Some("cancelled"), Some(1)), "{last}");
    assert!(!Path::new(&to3).exists() && !Path::new(&format!("{to3}.part")).exists());
    let (status, rest, _) = s.end(true);
    assert_eq!(status, 0);
    clean_bye(&rest, "eof");
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// A ZIP of stored members: local headers, the directory, the end record.
fn zip_of(members: &[(&str, &[u8])]) -> Vec<u8> {
    let (mut out, mut directory) = (Vec::new(), Vec::new());
    for (name, data) in members {
        let mut fields = Vec::new();
        for (value, width) in [(20u32, 2), (0, 2), (0, 2), (0, 2), (0x21, 2), (crc32(data), 4), (data.len() as u32, 4), (data.len() as u32, 4), (name.len() as u32, 2), (0, 2)] {
            fields.extend_from_slice(&value.to_le_bytes()[..width]);
        }
        let offset = out.len() as u32;
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&fields);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        directory.extend_from_slice(b"PK\x01\x02\x14\x00");
        directory.extend_from_slice(&fields);
        directory.extend_from_slice(&[0u8; 10]);
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name.as_bytes());
    }
    let (start, count) = (out.len() as u32, members.len() as u16);
    out.extend_from_slice(&directory);
    out.extend_from_slice(b"PK\x05\x06\0\0\0\0");
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&(directory.len() as u32).to_le_bytes());
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

/// `oneios-tool rangeserve DIR PORT` on a free port of 127.0.0.1, ended when dropped.
struct Web {
    child: Child,
    port: u16,
}

impl Web {
    fn start(dir: &Path) -> Web {
        Web::with(dir, None, &[])
    }

    /// With `rangeserve`'s options that make it a bad network; on `port` when the URL must stay the same.
    fn with(dir: &Path, port: Option<u16>, options: &[&str]) -> Web {
        for _ in 0..20 {
            let port = port.unwrap_or_else(|| std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port());
            let mut child = Command::new(tool()).arg("rangeserve").arg(dir).arg(port.to_string()).args(options).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
            for _ in 0..100 {
                if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                    return Web { child, port };
                }
                if child.try_wait().unwrap().is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        panic!("rangeserve did not start");
    }
}

impl Drop for Web {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn downloads_over_http_ranges_checked_and_placed() {
    let b = Box::new("download");
    let (kernel, other) = (noise(20 << 20, 11), noise(300_000, 12));
    b.put("web/xnu.macho", &kernel);
    b.put("web/set.zip", &zip_of(&[("Firmware/a.im4p", &other), ("kernelcache.release.iphone18", &kernel[..5 << 20]), ("BuildManifest.plist", b"<plist/>")]));
    let web = Web::start(&b.top.join("web"));
    let url = |name: &str| format!("http://127.0.0.1:{}/{name}", web.port);
    let mut s = Serve::start(&b, &["--jobs", "4"]);

    // a file: parallel range requests, the SHA-256 checked before the name is given
    let out = b.stage("downloads/xnu.macho");
    s.send(json!({"id": "d", "cmd": "download", "url": url("xnu.macho"), "out": out, "sha256": sha256(&kernel), "bytes": kernel.len()}));
    let events = s.request("d");
    let steps: Vec<&str> = events.iter().filter(|v| v["ev"] == "step").map(|v| v["step"].as_str().unwrap()).collect();
    assert_eq!(steps, ["download", "check", "place"], "{events:#?}");
    assert_eq!(events.iter().find(|v| v["ev"] == "space").expect("a space line")["need"].as_u64(), Some(kernel.len() as u64));
    let fetched: Vec<&Value> = events.iter().filter(|v| v["ev"] == "progress" && v["step"] == "download").collect();
    assert_eq!((fetched.last().unwrap()["done"].as_u64(), fetched.last().unwrap()["total"].as_u64()), (Some(kernel.len() as u64), Some(kernel.len() as u64)));
    let ok = events.last().unwrap();
    assert_eq!((ok["ev"].as_str(), ok["bytes"].as_u64(), ok["sha256"].as_str()), (Some("ok"), Some(kernel.len() as u64), Some(sha256(&kernel).as_str())), "{ok}");
    assert_eq!(std::fs::read(&out).unwrap(), kernel);
    let left: Vec<String> = std::fs::read_dir(b.stage("downloads")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(left, ["xnu.macho"], "no part is left");
    // there already with this sum: nothing is fetched
    assert!(s.ask(json!({"id": "d2", "cmd": "download", "url": url("xnu.macho"), "out": out, "sha256": sha256(&kernel)}))["text"].as_str().unwrap().contains("there already"));
    // another sum than expected, another size than expected, a file the server does not have: nothing is kept
    let out2 = b.stage("downloads/other.macho");
    let last = s.ask(json!({"id": "d3", "cmd": "download", "url": url("xnu.macho"), "out": out2, "sha256": sha256(b"x")}));
    assert_eq!((last["ev"].as_str(), last["code"].as_str()), (Some("refused"), Some("mismatch")), "{last}");
    let last = s.ask(json!({"id": "d4", "cmd": "download", "url": url("xnu.macho"), "out": out2, "sha256": sha256(&kernel), "bytes": 5}));
    assert_eq!(last["code"], "mismatch");
    assert!(last["text"].as_str().unwrap().contains("5 were expected"));
    assert_eq!(s.ask(json!({"id": "d5", "cmd": "download", "url": url("no-such-file"), "out": out2, "sha256": sha256(&kernel)}))["ev"], "failed");
    let left: Vec<String> = std::fs::read_dir(b.stage("downloads")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(left, ["xnu.macho"], "a refused or failed download leaves nothing");

    // members of a ZIP without downloading it
    s.send(json!({"id": "ls", "cmd": "zip-ls", "source": url("set.zip"), "match": "kernelcache|\\.plist$"}));
    let events = s.request("ls");
    let rows: Vec<(&str, u64, &str)> = events.iter().filter(|v| v["ev"] == "row").map(|v| (v["name"].as_str().unwrap(), v["bytes"].as_u64().unwrap(), v["method"].as_str().unwrap())).collect();
    assert_eq!(rows, [("kernelcache.release.iphone18", 5 << 20, "stored"), ("BuildManifest.plist", 8, "stored")]);
    assert_eq!(events.last().unwrap()["members"], 2);
    let outdir = b.stage("ipsw");
    s.send(json!({"id": "get", "cmd": "zip-get", "source": url("set.zip"), "outdir": outdir, "members": [{"member": "kernelcache.release.iphone18", "name": "kernelcache"}, {"member": "Firmware/a.im4p"}]}));
    let events = s.request("get");
    assert_eq!(events.last().unwrap()["ev"], "ok", "{events:#?}");
    let steps: Vec<&str> = events.iter().filter(|v| v["ev"] == "step").map(|v| v["step"].as_str().unwrap()).collect();
    assert_eq!(steps, ["directory", "download"]);
    assert_eq!(std::fs::read(format!("{outdir}/kernelcache")).unwrap(), &kernel[..5 << 20]);
    assert_eq!(std::fs::read(format!("{outdir}/a.im4p")).unwrap(), other);
    assert_eq!(std::fs::read_dir(&outdir).unwrap().count(), 2, "no part is left");
    let rows: Vec<&Value> = events.iter().filter(|v| v["ev"] == "row").collect();
    assert_eq!((rows[0]["name"].as_str(), rows[0]["crc32"].as_str().map(str::len), rows[0]["bytes"].as_u64()), (Some("kernelcache.release.iphone18"), Some(8), Some(5 << 20)));
    // a member the archive does not have; a name that is a path
    assert_eq!(s.ask(json!({"id": "g2", "cmd": "zip-get", "source": url("set.zip"), "outdir": outdir, "members": [{"member": "nothing"}]}))["code"], "not-found");
    assert_eq!(s.ask(json!({"id": "g3", "cmd": "zip-get", "source": url("set.zip"), "outdir": outdir, "members": [{"member": "Firmware/a.im4p", "name": "../a"}]}))["code"], "usage");
    // the same from a file the user gave
    let outdir2 = b.stage("ipsw2");
    assert_eq!(s.ask(json!({"id": "g4", "cmd": "zip-get", "source": b.top.join("web/set.zip").to_string_lossy(), "outdir": outdir2, "members": [{"member": "BuildManifest.plist"}]}))["ev"], "ok");
    assert_eq!(std::fs::read(format!("{outdir2}/BuildManifest.plist")).unwrap(), b"<plist/>");
    let (status, rest, stderr) = s.end(true);
    assert_eq!((status, stderr.as_str()), (0, ""));
    clean_bye(&rest, "eof");
}

#[test]
fn a_stopped_download_removes_its_part_or_keeps_it_for_resume() {
    let b = Box::new("download-stop");
    // a server that answers the size and the question whether it has ranges, and then never a byte: the download
    // stays in its first range requests
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut request = [0u8; 2048];
            let n = stream.read(&mut request).unwrap_or(0);
            let request = String::from_utf8_lossy(&request[..n]).to_ascii_lowercase();
            if request.starts_with("head") {
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 50000000\r\nConnection: close\r\n\r\n");
            } else if request.contains("range: bytes=0-0\r\n") {
                let _ = stream.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 1\r\nContent-Range: bytes 0-0/50000000\r\nConnection: close\r\n\r\nx");
            } else {
                // (kept open, silent)
                held.push(stream);
            }
        }
    });
    let url = format!("http://127.0.0.1:{port}/big.img");
    let mut s = Serve::start(&b, &[]);
    for (id, resume) in [("plain", false), ("resume", true)] {
        let out = b.stage(&format!("dl/{id}.img"));
        s.send(json!({"id": id, "cmd": "download", "url": url, "out": out, "sha256": "0".repeat(64), "resume": resume}));
        s.until(|v| v["id"] == id && v["ev"] == "progress");
        let until = Instant::now() + Duration::from_secs(10);
        while !Path::new(&format!("{out}.part.state")).exists() {
            assert!(Instant::now() < until, "the part and its state are there while the download runs");
            std::thread::sleep(Duration::from_millis(20));
        }
        if resume {
            // a pause is said at once, also while every connection is inside a read that gets no byte
            let began = Instant::now();
            s.send(json!({"cmd": "pause", "id": id}));
            let paused = s.until(|v| v["id"] == id && v["ev"] == "paused").pop().unwrap();
            assert_eq!((paused["done"].as_u64(), paused["total"].as_u64(), paused["step"].as_str()), (Some(0), Some(50_000_000), Some("download")), "{paused}");
            assert!(began.elapsed() < Duration::from_secs(2), "{:?}", began.elapsed());
        }
        let began = Instant::now();
        s.send(json!({"cmd": "cancel", "id": id}));
        let last = s.request(id).pop().unwrap();
        assert_eq!(last["ev"], "cancelled", "{last}");
        // (a range request that gets no byte is given up after 5 s: the stop is seen then at the latest)
        assert!(began.elapsed() < Duration::from_secs(9), "{:?}", began.elapsed());
        assert_eq!(Path::new(&format!("{out}.part")).exists(), resume, "the part of {id}");
        assert_eq!(Path::new(&format!("{out}.part.state")).exists(), resume);
        assert!(!Path::new(&format!("{out}.part.state.tmp")).exists());
        assert!(!Path::new(&out).exists());
    }
    let (_, rest, _) = s.end(true);
    clean_bye(&rest, "eof");
}

// ---------------------------------------------------------------- downloads on a bad network

/// The request's events of one kind.
fn of<'a>(events: &'a [Value], ev: &str) -> Vec<&'a Value> {
    events.iter().filter(|v| v["ev"] == ev).collect()
}

fn state_of(out: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(format!("{out}.part.state")).expect("the state file")).expect("one JSON object")
}

/// Send a download and wait until it has at least `least` bytes; the events so far.
fn download_until(s: &mut Serve, request: Value, least: u64) -> Vec<Value> {
    let id = request["id"].as_str().unwrap().to_string();
    s.send(request);
    s.until(|v| v["id"] == id.as_str() && (is_last(v) || (v["ev"] == "progress" && v["step"] == "download" && v["done"].as_u64().unwrap() >= least)))
}

#[test]
fn a_download_is_paused_and_resumed() {
    let b = Box::new("pause");
    let data = noise(20 << 20, 21);
    b.put("web/xnu.macho", &data);
    b.put("web/set.zip", &zip_of(&[("kernelcache", &data[..3 << 20])]));
    // a slow line: 2.5 MB a second on each connection (the file is three pieces of 8 MiB: three connections)
    let web = Web::with(&b.top.join("web"), None, &["--rate", "2500000"]);
    let url = |name: &str| format!("http://127.0.0.1:{}/{name}", web.port);
    let mut s = Serve::start(&b, &["--jobs", "4"]);
    let out = b.stage("dl/xnu.macho");
    // what cannot be paused: a request that is not there, one that waits, one without a transfer
    s.send(json!({"cmd": "pause", "id": "nothing"}));
    assert_eq!(s.until(|v| v["ev"] == "error").pop().unwrap()["code"], "no-such-request");
    s.send(json!({"id": "w", "cmd": "wait", "seconds": 0.6}));
    s.until(|v| v["id"] == "w" && v["ev"] == "start");
    s.send(json!({"cmd": "pause", "id": "w"}));
    assert_eq!(s.until(|v| v["ev"] == "error").pop().unwrap()["code"], "not-pausable");
    let before = download_until(&mut s, json!({"id": "d", "cmd": "download", "url": url("xnu.macho"), "out": out, "sha256": sha256(&data), "connections": 4}), 0);
    assert_eq!(before.iter().find(|v| v["ev"] == "queued").map(|v| v["id"].as_str()), Some(Some("d")), "{before:#?}");
    s.send(json!({"id": "q", "cmd": "wait", "seconds": 0}));
    s.send(json!({"cmd": "pause", "id": "q"}));
    assert_eq!(s.until(|v| v["ev"] == "error").pop().unwrap()["code"], "not-running");
    s.send(json!({"cmd": "resume", "id": "d"}));
    assert_eq!(s.until(|v| v["ev"] == "error").pop().unwrap()["code"], "not-paused");

    // the pause: said at once with the bytes that are there; nothing is counted, nothing is removed, the queue waits
    s.until(|v| v["id"] == "d" && v["ev"] == "progress" && v["done"].as_u64().unwrap() >= 1_500_000);
    s.send(json!({"cmd": "pause", "id": "d"}));
    let paused = s.until(|v| v["id"] == "d" && v["ev"] == "paused").pop().unwrap();
    let at = paused["done"].as_u64().unwrap();
    assert!((1_500_000..20 << 20).contains(&at), "{paused}");
    assert_eq!((paused["total"].as_u64(), paused["step"].as_str(), paused["unit"].as_str()), (Some(20 << 20), Some("download"), Some("bytes")), "{paused}");
    assert!(paused["text"].as_str().unwrap().starts_with("paused at "), "{paused}");
    let mut quiet = Vec::new();
    while let Some(line) = s.next_in(Duration::from_millis(1500)) {
        quiet.push(line);
    }
    assert!(quiet.iter().all(|v| v["ev"] != "progress" && !is_last(v) && v["id"] != "q"), "nothing moves while it is paused: {quiet:#?}");
    // the state file lists what `paused` said, and the part holds those bytes
    let state = state_of(&out);
    let listed: u64 = state["done"].as_array().unwrap().iter().map(|r| r[1].as_u64().unwrap() - r[0].as_u64().unwrap()).sum();
    assert_eq!(listed, at, "{state}");
    assert_eq!((state["v"].as_u64(), state["source"].as_str(), state["what"].as_str(), state["size"].as_u64(), state["ranges"].as_bool()), (Some(1), Some(url("xnu.macho").as_str()), Some("file"), Some(20 << 20), Some(true)), "{state}");
    let part = std::fs::read(format!("{out}.part")).unwrap();
    for range in state["done"].as_array().unwrap() {
        let (first, end) = (range[0].as_u64().unwrap() as usize, range[1].as_u64().unwrap() as usize);
        assert_eq!(part[first..end], data[first..end], "the bytes {first}..{end} of the part");
    }
    // a second pause says the same again; ping is answered while it is paused
    s.send(json!({"cmd": "pause", "id": "d"}));
    assert_eq!(s.until(|v| v["id"] == "d" && v["ev"] == "paused").pop().unwrap()["done"].as_u64(), Some(at));
    assert_eq!(s.ask(json!({"id": "p", "cmd": "ping"}))["ev"], "ok");

    // the resume: it goes on from those bytes, to the end, and the SHA-256 is the file's
    s.send(json!({"cmd": "resume", "id": "d"}));
    let resumed = s.until(|v| v["id"] == "d" && v["ev"] == "resumed").pop().unwrap();
    assert_eq!((resumed["done"].as_u64(), resumed["total"].as_u64()), (Some(at), Some(20 << 20)), "{resumed}");
    let events = s.request("d");
    let ok = events.last().unwrap();
    assert_eq!((ok["ev"].as_str(), ok["sha256"].as_str(), ok["connections"].as_u64(), ok["ranges"].as_bool()), (Some("ok"), Some(sha256(&data).as_str()), Some(3), Some(true)), "{events:#?}");
    assert!(of(&events, "progress").iter().all(|v| v["step"] != "download" || v["done"].as_u64().unwrap() >= at), "the count does not go back");
    assert_eq!(std::fs::read(&out).unwrap(), data);
    let left: Vec<String> = std::fs::read_dir(b.stage("dl")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(left, ["xnu.macho"], "no part and no state file are left");
    assert_eq!(s.request("q").pop().unwrap()["ev"], "ok", "the queue went on");

    // a member of an archive: the same pause, and a cancel of the paused request ends it at once
    let outdir = b.stage("ipsw");
    let get = |id: &str, resume: bool| json!({"id": id, "cmd": "zip-get", "source": url("set.zip"), "outdir": outdir, "members": [{"member": "kernelcache"}], "resume": resume, "connections": 2});
    for (id, resume) in [("z1", false), ("z2", true)] {
        download_until(&mut s, get(id, resume), 300_000);
        s.send(json!({"cmd": "pause", "id": id}));
        let paused = s.until(|v| v["id"] == id && v["ev"] == "paused").pop().unwrap();
        assert!(paused["done"].as_u64().unwrap() >= 300_000, "{paused}");
        let began = Instant::now();
        s.send(json!({"cmd": "cancel", "id": id}));
        let last = s.request(id).pop().unwrap();
        assert_eq!((last["ev"].as_str(), last["by"].as_str()), (Some("cancelled"), Some("request")), "{last}");
        assert!(began.elapsed() < Duration::from_secs(2), "{:?}", began.elapsed());
        // (a cancel request removes the part unless the request said "resume")
        assert_eq!(Path::new(&format!("{outdir}/kernelcache.part")).exists(), resume, "{id}");
        assert_eq!(Path::new(&format!("{outdir}/kernelcache.part.state")).exists(), resume, "{id}");
    }
    // the member goes on from its part
    let events = { s.send(get("z3", true)); s.request("z3") };
    assert_eq!(events.last().unwrap()["ev"], "ok", "{events:#?}");
    assert!(of(&events, "line").iter().any(|v| v["text"].as_str().unwrap().starts_with("kernelcache: going on from ")), "{events:#?}");
    assert_eq!(std::fs::read(format!("{outdir}/kernelcache")).unwrap(), &data[..3 << 20]);
    assert_eq!(std::fs::read_dir(&outdir).unwrap().count(), 1, "no part and no state file are left");
    let (status, rest, stderr) = s.end(true);
    assert_eq!((status, stderr.as_str()), (0, ""));
    clean_bye(&rest, "eof");
}

#[test]
fn a_download_goes_on_in_a_new_process_unless_the_servers_file_changed() {
    let b = Box::new("resume");
    let data = noise(6 << 20, 22);
    b.put("web/xnu.macho", &data);
    let web = Web::with(&b.top.join("web"), None, &["--rate", "1000000", "--etag", "one"]);
    let (port, url) = (web.port, format!("http://127.0.0.1:{}/xnu.macho", web.port));
    let request = |id: &str, out: &str, resume: bool| json!({"id": id, "cmd": "download", "url": url, "out": out, "sha256": sha256(&data), "connections": 2, "resume": resume});
    // Three downloads that end in the middle: (a) paused, then the app goes away (the pipe closes), without
    // "resume"; (b) paused, then the process is killed; (c) "resume", and the pipe closes while it runs.
    let outs = [b.stage("dl/a.macho"), b.stage("dl/b.macho"), b.stage("dl/c.macho")];
    let mut had = Vec::new();
    for (at, out) in outs.iter().enumerate() {
        let mut s = Serve::start(&b, &["--jobs", "4"]);
        download_until(&mut s, request("d", out, at == 2), 500_000);
        let listed = |out: &str| state_of(out)["done"].as_array().unwrap().iter().map(|r| r[1].as_u64().unwrap() - r[0].as_u64().unwrap()).sum::<u64>();
        if at < 2 {
            s.send(json!({"cmd": "pause", "id": "d"}));
            let paused = s.until(|v| v["id"] == "d" && v["ev"] == "paused").pop().unwrap();
            // the state file is written at a pause (after the part was synced): a moment after `paused` is said
            let until = Instant::now() + Duration::from_secs(10);
            while listed(out) != paused["done"].as_u64().unwrap() {
                assert!(Instant::now() < until, "the state file lists the bytes `paused` said: {} and {paused}", state_of(out));
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        if at == 1 {
            // Safety: a signal to the process this test started.
            unsafe { libc::kill(s.pid(), libc::SIGKILL) };
            let _ = s.child.wait();
        } else {
            let (status, rest, _) = s.end(true);
            let last = rest.iter().find(|v| v["id"] == "d" && is_last(v)).expect("the request's last event");
            assert_eq!((last["ev"].as_str(), last["by"].as_str(), last["removed"].as_u64(), status), (Some("cancelled"), Some("eof"), Some(0), 5), "{rest:#?}");
            clean_bye(&rest, "eof");
        }
        let state = state_of(out);
        assert_eq!(state["etag"], "\"one\"", "{state}");
        had.push(listed(out));
        assert!(had[at] >= 500_000 && had[at] < 6 << 20 && !Path::new(out).exists(), "{state}");
    }
    // A new process goes on from each part: `resumed_from` is what the state file listed.
    let mut s = Serve::start(&b, &["--jobs", "4"]);
    for (out, had) in outs.iter().zip(&had).take(2) {
        s.send(request("again", out, true));
        let events = s.request("again");
        let ok = events.last().unwrap();
        assert_eq!((ok["ev"].as_str(), ok["resumed_from"].as_u64(), ok["sha256"].as_str()), (Some("ok"), Some(*had), Some(sha256(&data).as_str())), "{events:#?}");
        assert!(of(&events, "line").iter().any(|v| v["text"] == format!("xnu.macho: going on from {had} of {} bytes", 6 << 20).as_str()), "{events:#?}");
        // (the first progress line of the step has the earlier bytes already)
        let first = of(&events, "progress").into_iter().find(|v| v["step"] == "download" && v["done"].as_u64().unwrap() > 0).unwrap();
        assert!(first["done"].as_u64().unwrap() >= *had, "{first}");
        assert_eq!(std::fs::read(out).unwrap(), data);
        assert!(!Path::new(&format!("{out}.part")).exists() && !Path::new(&format!("{out}.part.state")).exists());
    }
    // The server's file changed (the same URL, another ETag): the third part is not gone on from, and a line says why.
    drop(web);
    let _web = Web::with(&b.top.join("web"), Some(port), &["--etag", "two"]);
    s.send(request("changed", &outs[2], true));
    let events = s.request("changed");
    let ok = events.last().unwrap();
    assert_eq!((ok["ev"].as_str(), ok["resumed_from"].as_u64()), (Some("ok"), Some(0)), "{events:#?}");
    let lines: Vec<&str> = of(&events, "line").iter().map(|v| v["text"].as_str().unwrap()).collect();
    assert!(lines.contains(&"xnu.macho: the server's file changed (ETag \"one\" then, \"two\" now): starting again from the first byte"), "{lines:?}");
    assert_eq!(std::fs::read(&outs[2]).unwrap(), data);
    // without "resume" a part that is there is not gone on from either, and it is removed when the request is stopped
    let (status, rest, stderr) = s.end(true);
    assert_eq!((status, stderr.as_str()), (0, ""));
    clean_bye(&rest, "eof");
}

#[test]
fn drops_and_503s_are_tried_again_and_the_app_is_told() {
    let b = Box::new("drops");
    let data = noise(3 << 20, 23);
    b.put("web/xnu.macho", &data);
    b.put("web/set.zip", &zip_of(&[("kernelcache", &data[..2 << 20])]));
    // every body breaks off after 0.3 to 0.9 MB, and every 5th request is answered 503
    let web = Web::with(&b.top.join("web"), None, &["--drop-every", "600000", "--fail-every", "5"]);
    let url = |name: &str| format!("http://127.0.0.1:{}/{name}", web.port);
    let mut s = Serve::start(&b, &["--jobs", "4"]);
    // one connection: every failure is the whole request's, and none of its tries brought a mebibyte, so each
    // waits the time of its count and is said
    let out = b.stage("dl/one.macho");
    s.send(json!({"id": "one", "cmd": "download", "url": url("xnu.macho"), "out": out, "sha256": sha256(&data), "connections": 1}));
    let events = s.request("one");
    let ok = events.last().unwrap();
    assert_eq!((ok["ev"].as_str(), ok["sha256"].as_str(), ok["connections"].as_u64()), (Some("ok"), Some(sha256(&data).as_str()), Some(1)), "{events:#?}");
    assert_eq!(std::fs::read(&out).unwrap(), data);
    let net = of(&events, "net");
    let (waiting, resumed): (Vec<&Value>, Vec<&Value>) = net.iter().partition(|v| v["state"] == "waiting");
    assert!(waiting.len() >= 3 && !resumed.is_empty(), "3 MiB in bodies of 0.9 MB at most: {net:#?}");
    assert_eq!(ok["retries"].as_u64(), Some(waiting.len() as u64), "each repeated try was said: {net:#?}");
    // a `waiting` counts from 1 after a byte came and one up after another failure; a `resumed` follows a
    // `waiting` and says its count; the last word is `resumed`
    let mut count = 0;
    for v in &net {
        if v["state"] == "waiting" {
            count += 1;
            assert_eq!(v["try"].as_u64(), Some(count), "{net:#?}");
        } else {
            assert!(count > 0 && v["state"] == "resumed" && v["tries"].as_u64() == Some(count), "{net:#?}");
            count = 0;
        }
    }
    assert_eq!(count, 0, "{net:#?}");
    for v in &waiting {
        let (count, next) = (v["try"].as_u64().unwrap(), v["next"].as_f64().unwrap());
        // 1 s doubling, less a random part of up to half
        let whole = (1u64 << (count - 1)) as f64;
        assert!((whole / 2.0 - 0.06..=whole + 0.06).contains(&next), "{v}");
        assert!(matches!(v["why"].as_str(), Some("dropped" | "server")) && v["left"].as_u64().unwrap() >= 1790, "{v}");
        assert_eq!(v["why"] == "server", v["status"] == 503, "{v}");
        let seconds = if next < 1.0 { format!("{next:.1}") } else { format!("{next:.0}") };
        assert_eq!(v["text"].as_str().unwrap(), format!("waiting for the network (try {count}, next in {seconds} s): {}", v["error"].as_str().unwrap()), "{v}");
    }
    assert!(resumed.iter().all(|v| v["tries"].as_u64().unwrap() >= 1 && v["waited"].as_f64().unwrap() >= 0.4 && v["text"].as_str().unwrap().starts_with("the network is back after ")), "{resumed:#?}");
    // no progress line goes back: a piece goes on from the byte it reached
    let counts: Vec<u64> = of(&events, "progress").iter().filter(|v| v["step"] == "download").map(|v| v["done"].as_u64().unwrap()).collect();
    assert!(counts.windows(2).all(|w| w[0] <= w[1]) && counts.last() == Some(&(3 << 20)), "{counts:?}");
    // the members of an archive (its directory is read through the same tries)
    let outdir = b.stage("ipsw");
    assert_eq!(s.ask(json!({"id": "ls", "cmd": "zip-ls", "source": url("set.zip")}))["members"], 1);
    let ok = s.ask(json!({"id": "zip", "cmd": "zip-get", "source": url("set.zip"), "outdir": outdir, "members": [{"member": "kernelcache"}]}));
    assert_eq!(ok["ev"], "ok", "{ok}");
    assert_eq!(std::fs::read(format!("{outdir}/kernelcache")).unwrap(), &data[..2 << 20]);
    // the keys' limits
    for (key, value) in [("connections", json!(0)), ("connections", json!(9)), ("give_up_s", json!(0)), ("give_up_s", json!(86_401)), ("connections", json!("4"))] {
        let mut request = json!({"id": "bad", "cmd": "download", "url": url("xnu.macho"), "out": b.stage("dl/bad"), "sha256": sha256(&data)});
        request[key] = value;
        assert_eq!(s.ask(request)["code"], "usage", "{key}");
    }
    assert_eq!(s.ask(json!({"id": "bad", "cmd": "zip-ls", "source": url("set.zip"), "connections": 2}))["code"], "usage", "zip-ls fetches no file");

    // Several connections (a file of three pieces of 8 MiB) to a server whose bodies break off after 1.5 to 4.5 MB:
    // a connection that brought a mebibyte and then broke is opened again after a short sleep, and nothing is said.
    drop(web);
    let web = Web::with(&b.top.join("web"), None, &["--drop-every", "3000000"]);
    let big = noise(20 << 20, 25);
    b.put("web/big.img", &big);
    let out = b.stage("dl/big.img");
    let began = Instant::now();
    s.send(json!({"id": "four", "cmd": "download", "url": format!("http://127.0.0.1:{}/big.img", web.port), "out": out, "sha256": sha256(&big), "connections": 4}));
    let events = s.request("four");
    let ok = events.last().unwrap();
    assert_eq!((ok["ev"].as_str(), ok["sha256"].as_str(), ok["connections"].as_u64()), (Some("ok"), Some(sha256(&big).as_str()), Some(3)), "{ok}");
    assert!(ok["retries"].as_u64().unwrap() >= 4, "20 MiB in bodies of 4.5 MB at most: {ok}");
    assert!(of(&events, "net").is_empty(), "no connection waited for the network: {events:#?}");
    // (0.1 to 0.2 s after each drop; the waits of the tries' row would be 0.5 s and more each)
    assert!(began.elapsed() < Duration::from_millis(400) * ok["retries"].as_u64().unwrap() as u32 + Duration::from_secs(3), "{:?} with {ok}", began.elapsed());
    assert_eq!(std::fs::read(&out).unwrap(), big);
    let (status, rest, stderr) = s.end(true);
    assert_eq!((status, stderr.as_str()), (0, ""));
    clean_bye(&rest, "eof");
}

#[test]
fn a_cancel_or_a_pause_during_the_wait_acts_at_once_and_the_wait_has_an_end() {
    let b = Box::new("backoff");
    b.put("web/xnu.macho", b"x");
    // a server that answers every request 503: the request waits, longer each time
    let web = Web::with(&b.top.join("web"), None, &["--fail-first", "1000000000"]);
    let url = format!("http://127.0.0.1:{}/xnu.macho", web.port);
    let mut s = Serve::start(&b, &[]);
    let request = |id: &str| json!({"id": id, "cmd": "download", "url": url, "out": b.stage(&format!("dl/{id}")), "sha256": "0".repeat(64)});
    // (the third wait is 2 to 4 s long: a stop that waited for its end would be seen)
    let third = |s: &mut Serve, id: &str| {
        let events = s.until(|v| v["id"] == id && v["ev"] == "net" && v["try"] == 3);
        let tries: Vec<u64> = events.iter().filter(|v| v["ev"] == "net").map(|v| v["try"].as_u64().unwrap()).collect();
        assert_eq!(tries, [1, 2, 3], "one `waiting` per try: {events:#?}");
        assert!(events.last().unwrap()["next"].as_f64().unwrap() >= 1.9, "{events:#?}");
        Instant::now()
    };
    s.send(request("c"));
    let began = third(&mut s, "c");
    s.send(json!({"cmd": "cancel", "id": "c"}));
    let last = s.request("c").pop().unwrap();
    assert_eq!((last["ev"].as_str(), last["by"].as_str()), (Some("cancelled"), Some("request")), "{last}");
    assert!(began.elapsed() < Duration::from_millis(1000), "the cancel ended the wait: {:?}", began.elapsed());
    // a pause during the wait: said at once; no try is made while it is paused; the resume starts the count anew
    s.send(request("p"));
    let began = third(&mut s, "p");
    s.send(json!({"cmd": "pause", "id": "p"}));
    s.until(|v| v["id"] == "p" && v["ev"] == "paused");
    assert!(began.elapsed() < Duration::from_millis(1000), "{:?}", began.elapsed());
    assert!(s.next_in(Duration::from_secs(5)).is_none(), "no try and no line while it is paused");
    s.send(json!({"cmd": "resume", "id": "p"}));
    let events = s.until(|v| v["id"] == "p" && v["ev"] == "net");
    assert_eq!((events[0]["ev"].as_str(), events.last().unwrap()["try"].as_u64()), (Some("resumed"), Some(1)), "{events:#?}");
    // the end of the pipe during a wait ends the process at once, with nothing left
    let began = Instant::now();
    let (status, rest, _) = s.end(true);
    assert!(began.elapsed() < Duration::from_secs(8), "{:?}", began.elapsed());
    assert_eq!((status, rest.iter().find(|v| v["id"] == "p" && is_last(v)).map(|v| v["ev"].as_str())), (5, Some(Some("cancelled"))), "{rest:#?}");
    clean_bye(&rest, "eof");
    assert_eq!(std::fs::read_dir(b.stage("dl")).map(|d| d.count()).unwrap_or(0), 0, "nothing was written");

    // the time a request gives: after 2 s without a byte it fails with `network`, and says how long it waited
    let mut s = Serve::start(&b, &[]);
    let began = Instant::now();
    let mut request = request("g");
    request["give_up_s"] = json!(2);
    s.send(request);
    let events = s.request("g");
    let last = events.last().unwrap();
    assert_eq!((last["ev"].as_str(), last["code"].as_str()), (Some("failed"), Some("network")), "{events:#?}");
    assert!(last["text"].as_str().unwrap().contains("no byte arrived for 2 s: gave up"), "{last}");
    assert!((2..7).contains(&began.elapsed().as_secs()), "{:?}", began.elapsed());
    assert!(of(&events, "net").iter().all(|v| v["left"].as_u64().unwrap() <= 2), "{events:#?}");
    let (_, rest, _) = s.end(true);
    clean_bye(&rest, "eof");
}

#[test]
fn a_server_without_ranges_is_fetched_in_one_stream() {
    let b = Box::new("norange");
    let data = noise(4 << 20, 24);
    b.put("web/xnu.macho", &data);
    let web = Web::with(&b.top.join("web"), None, &["--no-range", "--rate", "2000000"]);
    let url = format!("http://127.0.0.1:{}/xnu.macho", web.port);
    let mut s = Serve::start(&b, &["--jobs", "4"]);
    let out = b.stage("dl/xnu.macho");
    let before = download_until(&mut s, json!({"id": "d", "cmd": "download", "url": url, "out": out, "sha256": sha256(&data), "resume": true}), 2_000_000);
    assert!(of(&before, "line").iter().any(|v| v["text"].as_str().unwrap().contains("the server has no range requests: one stream")), "{before:#?}");
    // a pause keeps the bytes, but such a stream can only begin again: the count goes back to 0 after the resume
    s.send(json!({"cmd": "pause", "id": "d"}));
    let at = s.until(|v| v["id"] == "d" && v["ev"] == "paused").pop().unwrap()["done"].as_u64().unwrap();
    assert_eq!(state_of(&out)["ranges"], false);
    // (long enough for the connection to see the pause: a resume that comes before it does lets the stream go on)
    assert!(s.next_in(Duration::from_millis(600)).is_none(), "nothing moves while it is paused");
    s.send(json!({"cmd": "resume", "id": "d"}));
    let events = s.request("d");
    let ok = events.last().unwrap();
    assert_eq!((ok["ev"].as_str(), ok["ranges"].as_bool(), ok["connections"].as_u64(), ok["sha256"].as_str()), (Some("ok"), Some(false), Some(1), Some(sha256(&data).as_str())), "{events:#?}");
    let counts: Vec<u64> = of(&events, "progress").iter().filter(|v| v["step"] == "download").map(|v| v["done"].as_u64().unwrap()).collect();
    assert!(counts.iter().any(|done| *done < at), "the stream began again: {counts:?} after {at}");
    assert_eq!(std::fs::read(&out).unwrap(), data);
    let (status, rest, stderr) = s.end(true);
    assert_eq!((status, stderr.as_str()), (0, ""));
    clean_bye(&rest, "eof");
}

#[test]
fn the_heat_limits_hold_and_stop_a_download() {
    let b = Box::new("download-heat");
    let data = noise(4 << 20, 26);
    b.put("web/xnu.macho", &data);
    let web = Web::with(&b.top.join("web"), None, &["--rate", "1000000"]);
    let url = format!("http://127.0.0.1:{}/xnu.macho", web.port);
    heat(&b, "380", "330", "80", "Discharging");
    let mut s = Serve::start(&b, &["--jobs", "4"]);
    let out = b.stage("dl/xnu.macho");
    let request = |id: &str| json!({"id": id, "cmd": "download", "url": url, "out": out, "sha256": sha256(&data), "resume": true});
    // the case at its pause limit while the download runs: it waits (no byte is counted), and goes on when it is cool
    download_until(&mut s, request("d"), 300_000);
    heat(&b, "445", "330", "80", "Discharging");
    let paused = s.until(|v| v["ev"] == "heat").pop().unwrap();
    assert_eq!((paused["state"].as_str(), paused["why"].as_str(), paused["id"].as_str()), (Some("paused"), Some("case"), Some("d")), "{paused}");
    let mut held = Vec::new();
    while let Some(line) = s.next_in(Duration::from_millis(1500)) {
        held.push(line);
    }
    let counts: Vec<u64> = of(&held, "progress").iter().map(|v| v["done"].as_u64().unwrap()).collect();
    assert!(counts.windows(2).all(|w| w[0] == w[1]) && of(&held, "net").is_empty(), "nothing arrives, and the network is not blamed: {held:#?}");
    heat(&b, "400", "330", "80", "Discharging");
    let events = s.until(|v| v["id"] == "d" && v["ev"] == "progress" && v["done"].as_u64().unwrap() >= 1_500_000);
    assert_eq!(of(&events, "heat").first().map(|v| v["state"].as_str()), Some(Some("resumed")), "{events:#?}");
    assert!(of(&events, "net").is_empty(), "the connection that went stale in the pause is opened again without a word: {events:#?}");
    // the battery at its stop limit: the request ends with refused, heat; its part stays (the request said "resume")
    heat(&b, "380", "505", "80", "Discharging");
    let last = s.request("d").pop().unwrap();
    assert_eq!((last["ev"].as_str(), last["code"].as_str()), (Some("refused"), Some("heat")), "{last}");
    let had: u64 = state_of(&out)["done"].as_array().unwrap().iter().map(|r| r[1].as_u64().unwrap() - r[0].as_u64().unwrap()).sum();
    assert!(had >= 1_500_000 && !Path::new(&out).exists(), "{had}");
    // cool again: the same request goes on from the part
    heat(&b, "380", "330", "80", "Discharging");
    let ok = s.ask(request("again"));
    assert_eq!((ok["ev"].as_str(), ok["resumed_from"].as_u64(), ok["sha256"].as_str()), (Some("ok"), Some(had), Some(sha256(&data).as_str())), "{ok}");
    assert_eq!(std::fs::read(&out).unwrap(), data);
    let (status, rest, stderr) = s.end(true);
    assert_eq!((status, stderr.as_str()), (0, ""));
    clean_bye(&rest, "eof");
}

#[test]
fn the_logs_and_the_cpu_check_from_named_files() {
    let b = Box::new("readings");
    let log: String = (0..40).map(|i| format!("[{:>9}.000] guest     {}\n", i, if i % 4 == 0 { "oneios-gpu-report: frame" } else { "launchd: a job" })).collect();
    b.put("stage/session/xnuhost.log", log.as_bytes());
    b.put("stage/switch.log", b"=== OneOS app switch ===\n");
    b.put("stage/logs/xnuhost-1794000000.log", b"an earlier session\n");
    for (n, midr, max, smax) in [(0, "0x00000000410fd801", "2265600", "2265600"), (7, "0x00000000411fd821", "3052800", "2400000")] {
        let d = format!("sys/devices/system/cpu/cpu{n}");
        b.sys(&format!("{d}/regs/identification/midr_el1"), midr);
        b.sys(&format!("{d}/cpu_capacity"), "1024");
        b.sys(&format!("{d}/cpufreq/cpuinfo_max_freq"), max);
        b.sys(&format!("{d}/cpufreq/scaling_max_freq"), smax);
        b.sys(&format!("{d}/cpufreq/scaling_governor"), "schedutil");
        b.sys(&format!("{d}/cpufreq/related_cpus"), &n.to_string());
    }
    b.sys("dev/cpuset/top-app/cpus", "0-7");
    let mut s = Serve::start(&b, &[]);

    s.send(json!({"id": "list", "cmd": "log-list"}));
    let events = s.request("list");
    let rows: Vec<&Value> = events.iter().filter(|v| v["ev"] == "row").collect();
    let row = |name: &str| rows.iter().find(|r| r["name"] == name).unwrap_or_else(|| panic!("no row {name}"));
    assert_eq!((row("xnuhost")["bytes"].as_u64(), row("xnuhost")["path"].as_str()), (Some(log.len() as u64), Some("session/xnuhost.log")));
    assert_eq!((row("return")["missing"].as_bool(), row("logs/xnuhost-1794000000.log")["bytes"].as_u64()), (Some(true), Some(19)));
    assert_eq!(events.last().unwrap()["logs"], 3);

    // the lines as `line` events; the filters; the last lines; into a file
    s.send(json!({"id": "log", "cmd": "log", "filters": ["gpu"], "since": 20.0}));
    let events = s.request("log");
    let lines: Vec<&str> = events.iter().filter(|v| v["ev"] == "line").map(|v| v["text"].as_str().unwrap()).collect();
    assert_eq!(lines.len(), 5, "{lines:?}");
    assert!(lines.iter().all(|l| l.contains("oneios-gpu-report")) && events.iter().filter(|v| v["ev"] == "line").all(|v| v["stream"] == "out"));
    assert!(lines[0].starts_with("[       20.000]"));
    let ok = events.last().unwrap();
    assert_eq!((ok["lines"].as_u64(), ok["matched"].as_u64(), ok["bytes"].as_u64()), (Some(40), Some(5), Some(log.len() as u64)), "{ok}");
    let ok = s.ask(json!({"id": "tail", "cmd": "log", "tail": 3, "grep": ["launchd"]}));
    assert_eq!((ok["lines"].as_u64(), ok["matched"].as_u64()), (Some(3), Some(3)));
    let out = b.stage("work/log.txt");
    assert_eq!(s.ask(json!({"id": "file", "cmd": "log", "file": "logs/xnuhost-1794000000.log", "out": out}))["out"], out.as_str());
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "an earlier session\n");
    // "lines": at most that many line events; the rest are counted
    s.send(json!({"id": "few", "cmd": "log", "lines": 4}));
    let events = s.request("few");
    assert_eq!(events.iter().filter(|v| v["ev"] == "line").count(), 4);
    assert_eq!(events.last().unwrap()["lines_dropped"], 36);

    s.send(json!({"id": "cpu", "cmd": "cpu"}));
    let events = s.request("cpu");
    let rows: Vec<&Value> = events.iter().filter(|v| v["ev"] == "row").collect();
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[1]["n"].as_u64(), rows[1]["core"].as_str(), rows[1]["max_khz"].as_u64(), rows[1]["scaling_max_khz"].as_u64(), rows[1]["capped"].as_bool()), (Some(7), Some("Cortex-X4"), Some(3052800), Some(2400000), Some(true)), "{}", rows[1]);
    assert_eq!((rows[0]["core"].as_str(), rows[0]["capped"].as_bool()), (Some("Cortex-A520"), Some(false)));
    let text: Vec<&str> = events.iter().filter(|v| v["ev"] == "line").map(|v| v["text"].as_str().unwrap()).collect();
    assert!(text.iter().any(|l| l.contains("cpu7 Cortex-X4") && l.contains("CAPPED")) && text.iter().any(|l| l.contains("xnuhost: no session")), "{text:?}");
    assert_eq!(events.last().unwrap()["cpus"], 2);
    let (status, rest, stderr) = s.end(true);
    assert_eq!((status, stderr.as_str()), (0, ""));
    clean_bye(&rest, "eof");
}

#[test]
fn the_runtime_check_refuses_a_file_of_apples() {
    let b = Box::new("package");
    b.put("stage/runtime/bin/xnuhost", b"\x7fELF not really");
    b.put("stage/runtime/runtime.json", b"{}");
    let mut s = Serve::start(&b, &[]);
    let first = s.ask(json!({"id": "ok", "cmd": "package-check", "dir": b.stage("runtime")}));
    // a Mach-O (Apple's format) in the runtime: refused, with the tool's own reason
    b.put("stage/runtime/lib/kernel.kc", &[0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0, 0, 1, 0, 0, 0, 0, 0xc, 0, 0, 0]);
    let last = s.ask(json!({"id": "apple", "cmd": "package-check", "dir": b.stage("runtime")}));
    assert_eq!((last["ev"].as_str(), last["code"].as_str()), (Some("refused"), Some("apple-file")), "first: {first}; then: {last}");
    assert!(last["text"].as_str().unwrap().contains("kernel.kc"), "{last}");
    s.end(true);
}

// ---------------------------------------------------------------- the one-shot form

#[test]
fn the_one_shot_form_and_its_exit_status() {
    let b = Box::new("oneshot");
    let run = |requests: &[Value], options: &[&str]| {
        let mut command = Command::new(tool());
        command.arg("serve").args(["--stage", &b.stage("")]).args(options);
        for request in requests {
            command.args(["--request", &request.to_string()]);
        }
        let done = command.stdin(Stdio::null()).output().unwrap();
        let lines: Vec<Value> = String::from_utf8_lossy(&done.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        (done.status.code().unwrap(), lines, String::from_utf8_lossy(&done.stderr).into_owned())
    };
    let (status, lines, stderr) = run(&[json!({"id": "1", "cmd": "wait", "seconds": 0.2}), json!({"id": "2", "cmd": "ping"})], &[]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert_eq!(lines.first().unwrap()["ev"], "hello");
    assert_eq!(clean_bye(&lines, "requests")["requests"], 2);
    // the status is the last request's: refused 2, a wrong request 3, failed 4
    assert_eq!(run(&[json!({"id": "1", "cmd": "sums", "dir": b.stage("no-such-folder")})], &[]).0, 2);
    assert_eq!(run(&[json!({"id": "1", "cmd": "wait", "seconds": -1})], &[]).0, 3);
    assert_eq!(run(&[json!({"id": "1", "cmd": "ioslog-boots", "source": b.put("stage/not-a-store.img", &[0u8; 8192])})], &[]).0, 4);
    assert_eq!(run(&[json!({"id": "1", "cmd": "sums", "dir": b.stage("no-such-folder")}), json!({"id": "2", "cmd": "ping"})], &[]).0, 0);
    // a wrong command line of serve itself: 3, said on stderr before any line
    for options in [&["--request", "nonsense"][..], &["--request", "{\"id\":\"1\",\"cmd\":\"nothing\"}"], &["--nice", "5"], &["--nice", "20"], &["--jobs", "9"], &["--jobs", "0"], &["--thermal", "50,40,45,50"], &["--battery-min", "101"], &["--no-such-option"]] {
        let (status, lines, stderr) = run(&[], options);
        assert_eq!((status, lines.len()), (3, 0), "{options:?}: {stderr}");
        assert!(!stderr.is_empty());
    }
}

#[test]
fn the_one_shot_commands_take_jobs_and_nice() {
    let b = Box::new("options");
    let image = b.put("a.img", &noise(100_000, 3));
    let out = b.top.join("a.hashes");
    let run = |words: &[&str]| Command::new(tool()).args(words).args(["image", "hashes", &image]).arg(&out).output().unwrap();
    assert!(run(&["--jobs", "2", "--nice", "15"]).status.success());
    assert!(run(&["--nice", "19", "--jobs", "8"]).status.success());
    for (words, said) in [(&["--jobs", "0"][..], "--jobs 0: 1 to 8"), (&["--jobs", "9"], "1 to 8"), (&["--nice", "5"], "10 to 19"), (&["--nice", "x"], "10 to 19"), (&["--jobs"], "--jobs image: 1 to 8")] {
        let done = run(words);
        assert_eq!(done.status.code(), Some(2), "{words:?}");
        assert!(String::from_utf8_lossy(&done.stderr).contains(said), "{words:?}: {}", String::from_utf8_lossy(&done.stderr));
    }
}

// ---------------------------------------------------------------- make-image (the whole image set)

/// A stand-in IPSW for `make-image` (nothing of Apple's): generated members, a BuildManifest with the build identity
/// the profile asks for, and the profile that pins every member's size and CRC-32. With `aea` the OS and SystemOS
/// members are AEA files (`.aea`), as in a real profile. The OS member is noise of `os_len` bytes, so a build gets
/// past the fetch and stops at the place step's APFS check. Returns (the IPSW's folder, its file name, the profile).
fn stand_in_ipsw(b: &Box, os_len: usize, aea: bool) -> (PathBuf, String) {
    stand_in_ipsw_sealed(b, os_len, aea, &|data| data)
}

/// The same, with `seal` making the OS and SystemOS members' bytes (an AEA archive of them, for a build that decrypts).
fn stand_in_ipsw_sealed(b: &Box, os_len: usize, aea: bool, seal: &dyn Fn(Vec<u8>) -> Vec<u8>) -> (PathBuf, String) {
    let x = if aea { ".aea" } else { "" };
    let mut members: Vec<(&str, String, Vec<u8>)> = vec![
        ("os", format!("001-00001-001.dmg{x}"), seal(noise(os_len, 31))),
        ("systemos", format!("001-00002-001.dmg{x}"), seal(noise(70_000, 32))),
        ("app", "001-00003-001.dmg".into(), noise(32_768, 33)),
        ("trust-static", format!("Firmware/001-00001-001.dmg{x}.trustcache"), noise(100, 34)),
        ("trust-system", format!("Firmware/001-00002-001.dmg{x}.trustcache"), noise(100, 35)),
        ("trust-app", "Firmware/001-00003-001.dmg.trustcache".into(), noise(100, 36)),
        ("roothash-os", format!("Firmware/001-00001-001.dmg{x}.root_hash"), noise(100, 37)),
        ("roothash-system", format!("Firmware/001-00002-001.dmg{x}.root_hash"), noise(100, 38)),
        ("roothash-app", "Firmware/001-00003-001.dmg.root_hash".into(), noise(100, 39)),
        ("kernelcache", "kernelcache.release.t1".into(), noise(4000, 40)),
        ("devicetree", "Firmware/all_flash/DeviceTree.t1ap.im4p".into(), noise(200, 41)),
    ];
    let components = [("os", "OS"), ("systemos", "Cryptex1,SystemOS"), ("app", "Cryptex1,AppOS"), ("trust-static", "StaticTrustCache"), ("trust-system", "Cryptex1,SystemTrustCache"), ("trust-app", "Cryptex1,AppTrustCache"), ("roothash-os", "SystemVolume"), ("roothash-system", "Cryptex1,SystemVolume"), ("roothash-app", "Cryptex1,AppVolume"), ("kernelcache", "KernelCache"), ("devicetree", "DeviceTree")];
    let manifest: String = components
        .iter()
        .map(|(role, name)| format!("<key>{name}</key><dict><key>Info</key><dict><key>Path</key><string>{}</string></dict></dict>", members.iter().find(|m| m.0 == *role).unwrap().1))
        .collect();
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>ProductVersion</key><string>0.0.1</string><key>ProductBuildVersion</key><string>0T1</string><key>SupportedProductTypes</key><array><string>iTest1,1</string></array><key>BuildIdentities</key><array><dict><key>ApChipID</key><string>0x1234</string><key>ApBoardID</key><string>0x02</string><key>Info</key><dict><key>DeviceClass</key><string>t1ap</string><key>RestoreBehavior</key><string>Erase</string></dict><key>Manifest</key><dict>{manifest}</dict></dict></array></dict></plist>"
    );
    members.push(("buildmanifest", "BuildManifest.plist".into(), plist.into_bytes()));
    members.push(("restore", "Restore.plist".into(), b"<plist/>".to_vec()));
    let zip: Vec<(&str, &[u8])> = members.iter().map(|(_, path, data)| (path.as_str(), data.as_slice())).collect();
    b.put("web/stand-in.ipsw", &zip_of(&zip));
    let pins: String = members
        .iter()
        .map(|(role, path, data)| format!("[[member]]\nrole = \"{role}\"\npath = \"{path}\"\nsize = {}\ncrc32 = \"{:08x}\"\n{}", data.len(), crc32(data), if aea && path.ends_with(".aea") { "aea = true\n" } else { "" }))
        .collect();
    let profile = format!(
        "schema = 1\n[os]\nname = \"ios-0.0.1-0T1-iTest1,1\"\nversion = \"0.0.1\"\nbuild = \"0T1\"\ndevice = \"iTest1,1\"\nboard = \"t1ap\"\nchip_id = 0x1234\nboard_id = 0x02\narch = \"arm64e\"\n[ipsw]\nurl = \"https://example.invalid/iTest.ipsw\"\nsize = 12345\nsha256 = \"{}\"\nneed_gib = 0\n{pins}[image]\nformat = 1\nid = \"ios-0.0.1-0T1-iTest1-1\"\nstage = \"/data/local/tmp/oneios/image\"\narchive = \"OneiOS_test.tar.zst\"\nchunk = 4096\nseed = \"data.seed\"\nkernel = \"kernel.kc\"\n[image.files]\nos = \"os.img\"\nsystemos = \"systemos.img\"\napp = \"app.img\"\ntrust-static = \"trust/static.trustcache\"\ntrust-system = \"trust/system.trustcache\"\ntrust-app = \"trust/app.trustcache\"\nroothash-os = \"trust/os.root_hash\"\nroothash-system = \"trust/system.root_hash\"\nroothash-app = \"trust/app.root_hash\"\n[data]\nvolume_uuid = \"11111111-2222-3333-4444-555555555555\"\nepoch = 1700000000\n",
        "00".repeat(32)
    );
    let path = b.put(&format!("stage/runtime/lib/profiles/test{}.toml", if aea { "-aea" } else { "" }), profile.as_bytes());
    (b.top.join("web"), path)
}

/// What root's `begin image` leaves: an empty `image.new` and two empty lists.
fn begin_image(b: &Box) {
    let _ = std::fs::remove_dir_all(b.stage("image.new"));
    std::fs::create_dir_all(b.stage("image.new")).unwrap();
    std::fs::write(b.stage("image.new.sha256"), b"").unwrap();
    std::fs::write(b.stage("image.new.ids"), b"").unwrap();
}

/// The `make-image` request the OneOS app sends (oneos/interface.md section 9), with more keys.
fn make_image(b: &Box, id: &str, more: Value) -> Value {
    let mut request = json!({"id": id, "cmd": "make-image", "indir": b.stage("make/inputs"), "out": b.stage("image.new"), "sums_list": b.stage("image.new.sha256"), "ids_list": b.stage("image.new.ids"), "work": b.stage("make")});
    for (k, v) in more.as_object().unwrap() {
        request[k] = v.clone();
    }
    request
}

/// Nothing of a build that did not end well is left: the set folder empty, the lists empty, no work folder.
fn nothing_left(b: &Box, what: &str) {
    assert_eq!(std::fs::read_dir(b.stage("image.new")).unwrap().count(), 0, "{what}: image.new is empty");
    assert_eq!(std::fs::read(b.stage("image.new.sha256")).unwrap(), b"", "{what}");
    assert_eq!(std::fs::read(b.stage("image.new.ids")).unwrap(), b"", "{what}");
    assert!(!Path::new(&b.stage("make/build")).exists(), "{what}: no work folder");
}

#[test]
fn an_id_is_free_as_soon_as_its_last_event_is_sent() {
    // The runner clears a request up after its last event; ONEIOS_SERVE_TEST_LINGER_MS makes that gap long, which a
    // loaded PC made long by itself (make_image_refuses_... then waited 60 s for a request refused `duplicate`).
    let b = Box::new("id-reuse");
    let mut s = Serve::start_env(&b, &[], &[("ONEIOS_SERVE_TEST_LINGER_MS", "800")]);
    for _ in 0..3 {
        let last = s.ask(json!({"id": "w", "cmd": "wait", "seconds": 0.0}));
        assert_eq!(last["ev"], "ok", "the same id again, in the gap, is a new request: {last}");
    }
    assert!(s.seen.iter().all(|v| v["code"] != "duplicate"), "no duplicate: {:#?}", s.seen);
    // A cancel in the gap reaches the new request that waits, not the one that already ended.
    assert_eq!(s.ask(json!({"id": "c", "cmd": "wait", "seconds": 0.0}))["ev"], "ok");
    s.send(json!({"id": "c", "cmd": "wait", "seconds": 30.0}));
    s.send(json!({"id": "c", "cmd": "cancel"}));
    let last = s.request("c").pop().unwrap();
    assert_eq!((last["ev"].as_str(), last["text"].as_str()), (Some("cancelled"), Some("was waiting; it did not start")), "{last}");
    assert_eq!(s.ask(json!({"id": "z", "cmd": "wait", "seconds": 0.0}))["ev"], "ok");
    let (status, rest, _) = s.end(true);
    assert_eq!(status, 0);
    clean_bye(&rest, "eof");
}

#[test]
fn make_image_refuses_what_it_cannot_do_yet_and_writes_nothing() {
    let b = Box::new("make-refusals");
    let (_, profile) = stand_in_ipsw(&b, 9000, false);
    let (_, aea_profile) = stand_in_ipsw(&b, 9000, true);
    let kc = b.put("stage/kb/kernel.kc", &noise(5000, 42));
    // a staged runtime with a kernel, as v0.1.12's (no pairing files in kb/: the prelink's inputs are missing)
    b.put("stage/runtime/runtime.json", br#"{"format": 1, "kernel": {"path": "kernel/xnu.macho"}}"#);
    b.put("stage/runtime/kernel/xnu.macho", &noise(4096, 43));
    begin_image(&b);
    let mut s = Serve::start(&b, &[]);
    for (more, code, words) in [
        // the app's own request: the AEA images are decrypted (since 2026-11-21) and the root tweaks run, so an AEA
        // profile also goes past them to the kernel's inputs
        (json!({"profile": aea_profile}), "not-found", "the kernel's inputs are missing"),
        // the root tweaks are in the tool: the default request goes past them, to the kernel's inputs (kb/ is empty)
        (json!({"profile": profile}), "not-found", "the kernel's inputs are missing"),
        // with the runtime's profiles/ holding two, the request must name one
        (json!({"tweaks": false}), "usage", "give \"profile\""),
        (json!({"profile": profile, "tweaks": false}), "not-found", "the kernel's inputs are missing"),
        (json!({"profile": profile, "tweaks": false, "kc": kc, "fetch": false}), "not-found", "missing inputs"),
        (json!({"profile": profile, "tweaks": false, "kc": kc, "no_kernel": true}), "usage", "not both"),
        (json!({"profile": profile, "tweaks": false, "out": b.top.join("elsewhere").to_string_lossy()}), "outside", "outside the stage"),
        (json!({"profile": profile, "tweaks": "no"}), "usage", "true or false"),
    ] {
        let last = s.ask(make_image(&b, "m", more.clone()));
        assert_eq!((last["ev"].as_str(), last["code"].as_str()), (Some("refused"), Some(code)), "{more}: {last}");
        assert!(last["text"].as_str().unwrap().contains(words), "{more}: {last}");
        nothing_left(&b, &more.to_string());
    }
    // a set folder that is not empty
    b.put("stage/image.new/old", b"x");
    let last = s.ask(make_image(&b, "m", json!({"profile": profile, "tweaks": false, "kc": kc, "fetch": false})));
    assert_eq!(last["code"], "not-found", "the missing inputs are said first: {last}");
    for m in ["001-00001-001.dmg", "001-00002-001.dmg", "001-00003-001.dmg", "001-00001-001.dmg.trustcache", "001-00002-001.dmg.trustcache", "001-00003-001.dmg.trustcache", "001-00001-001.dmg.root_hash", "001-00002-001.dmg.root_hash", "001-00003-001.dmg.root_hash"] {
        b.put(&format!("stage/make/inputs/{m}"), b"stand-in");
    }
    let last = s.ask(make_image(&b, "m", json!({"profile": profile, "tweaks": false, "kc": kc, "fetch": false})));
    assert_eq!((last["code"].as_str(), last["text"].as_str().map(|t| t.contains("not empty"))), (Some("exists"), Some(true)), "{last}");
    assert_eq!(std::fs::read(b.stage("image.new/old")).unwrap(), b"x", "what was there stays");
    let (status, rest, _) = s.end(true);
    assert_eq!(status, 0);
    clean_bye(&rest, "eof");
}

/// The runtime's `lib/profiles/` as runtime/package.sh packs it: every file of the repository's `profiles/`, the
/// comma of a name as a dash (v0.1.16: two host profiles and three IPSW profiles, two of them iOS 27.0.1 24A446).
fn packed_profiles(b: &Box) -> Vec<String> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../profiles");
    let mut names = Vec::new();
    for e in std::fs::read_dir(&repo).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().replace(',', "-");
        if name.ends_with(".toml") {
            b.put(&format!("stage/runtime/lib/profiles/{name}"), &std::fs::read(e.path()).unwrap());
            names.push(name);
        }
    }
    names
}

#[test]
fn make_image_takes_the_apps_request_with_the_released_runtimes_profiles() {
    // Issue 75: the app's request names no profile, and a released runtime packs every profile of profiles/; the
    // tool refused `usage` ("holds 5 profiles: give \"profile\"") before it fetched a byte, on every phone, and the
    // app said "OneOS and the runtime don't agree". Now it takes the staged kernel's iOS for iPhone18,2.
    let b = Box::new("make-packed");
    let names = packed_profiles(&b);
    assert!(names.len() >= 5 && names.iter().any(|n| n.starts_with("host-")), "the packed profiles: {names:?}");
    // a staged runtime as v0.1.16's: runtime.json's kernel with its record's profile (no pairing files in kb/)
    b.put("stage/runtime/runtime.json", br#"{"format": 1, "version": "0.1.16", "kernel": {"path": "kernel/xnu.macho", "profile": "ios-27.0.1-24A446"}}"#);
    b.put("stage/runtime/kernel/xnu.macho", &noise(4096, 43));
    begin_image(&b);
    let mut s = Serve::start(&b, &[]);
    // the app's request (fork OneiosTool.makeImageRequest up to 0.5.74: no "profile"), as it is
    let app = json!({"id": "m", "cmd": "make-image", "indir": b.stage("make/inputs"), "out": b.stage("image.new"), "sums_list": b.stage("image.new.sha256"), "ids_list": b.stage("image.new.ids"), "work": b.stage("make"), "resume": true});
    let last = s.ask(app.clone());
    assert_eq!((last["ev"].as_str(), last["code"].as_str()), (Some("refused"), Some("not-found")), "past the profile, to the kernel's inputs: {last}");
    assert!(last["text"].as_str().unwrap().contains("the kernel's inputs are missing"), "{last}");
    nothing_left(&b, "the app's request");
    // the app's request from 0.5.75 on names the profile (OneiosTool.IPSW_PROFILE): the same
    let mut named = app.clone();
    named["profile"] = json!(b.stage("runtime/lib/profiles/ios-27.0.1-iPhone18-2.toml"));
    let last = s.ask(named);
    assert_eq!((last["ev"].as_str(), last["code"].as_str()), (Some("refused"), Some("not-found")), "{last}");
    assert!(last["text"].as_str().unwrap().contains("the kernel's inputs are missing"), "{last}");
    // iOS 27.2.1 (2026-11-25): a runtime with its kernel in "kernels"; a request naming its profile and no xnu or kb
    // takes that kernel and kb-24B156/ (the app names both itself)
    b.put("stage/runtime/runtime.json", br#"{"format": 1, "kernel": {"path": "kernel/xnu.macho", "profile": "ios-27.0.1-24A446"}, "kernels": [{"path": "kernel/xnu.macho", "profile": "ios-27.0.1-24A446", "build": "24A446"}, {"path": "kernel/24B156/xnu.macho", "profile": "ios-27.2.1-24B156", "build": "24B156"}]}"#);
    b.put("stage/runtime/kernel/24B156/xnu.macho", &noise(4096, 44));
    let mut b272 = app.clone();
    b272["profile"] = json!(b.stage("runtime/lib/profiles/ios-27.2.1-iPhone18-2.toml"));
    let last = s.ask(b272);
    assert_eq!(last["code"].as_str(), Some("not-found"), "{last}");
    assert!(last["text"].as_str().unwrap().contains(&b.stage("kb-24B156/profile.toml")), "27.2.1's pairing files: {last}");
    nothing_left(&b, "the 27.2.1 request");
    // and without "profile", 27.0.1 (the staged kernel's)
    let last = s.ask(app.clone());
    assert!(last["text"].as_str().unwrap().contains(&b.stage("kb/profile.toml")), "{last}");
    // a runtime whose kernel is of no profile there: two 27.0.1 24A446 profiles are left, and iPhone18,2's is taken
    b.put("stage/runtime/runtime.json", br#"{"format": 1, "kernel": {"path": "kernel/xnu.macho", "profile": "ios-99.0-99Z999"}}"#);
    assert_eq!(s.ask(app.clone())["code"], "not-found");
    // (27.0.1 and 27.2.1 for iPhone18,2 since 2026-11-25: of those, 27.0.1's)
    // with no iPhone18,2 profile among several iOS profiles, the request must name one, and the refusal names them.
    // Two of another device are made from iPhone18,2's; the repository's ios-16.5-a56.toml is an IPSW profile since
    // 2026-11-23 (Apple's 20F66 pins, the A56's image), so it is named too; ios-27.0.1-a56.toml is still reference only
    // (no [[member]]) and is not.
    let iphone = std::fs::read_to_string(b.stage("runtime/lib/profiles/ios-27.0.1-iPhone18-2.toml")).unwrap();
    assert!(iphone.contains("device = \"iPhone18,2\""), "the iPhone profile's device line");
    for other in ["iPhone99-1", "iPhone99-2"] {
        b.put(&format!("stage/runtime/lib/profiles/ios-27.0.1-{other}.toml"), iphone.replace("device = \"iPhone18,2\"", &format!("device = \"{}\"", other.replace('-', ","))).as_bytes());
    }
    for name in ["ios-27.0.1-iPhone18-2.toml", "ios-27.2.1-iPhone18-2.toml"] {
        std::fs::remove_file(b.stage(&format!("runtime/lib/profiles/{name}"))).unwrap();
    }
    let last = s.ask(app.clone());
    assert_eq!((last["ev"].as_str(), last["code"].as_str()), (Some("refused"), Some("usage")), "{last}");
    let text = last["text"].as_str().unwrap();
    assert!(text.contains("give \"profile\"") && text.contains("ios-27.0.1-iPhone99-1.toml") && text.contains("ios-27.0.1-iPhone99-2.toml"), "{last}");
    assert!(text.contains("holds 3 iOS profiles") && text.contains("ios-16.5-a56.toml"), "the A56's 20F66 profile is one: {last}");
    assert!(!text.contains("host-") && !text.contains("ios-27.0.1-a56"), "only IPSW profiles are named: {last}");
    // the A56's request names its profile (the fork's A56 path): past the profile, to the kernel's inputs, as above
    let mut a56 = app.clone();
    a56["profile"] = json!(b.stage("runtime/lib/profiles/ios-16.5-a56.toml"));
    let last = s.ask(a56);
    assert_eq!(last["ev"].as_str(), Some("refused"), "{last}");
    assert!(!last["text"].as_str().unwrap().contains("give \"profile\""), "the profile was taken: {last}");
    assert_eq!(last["code"].as_str(), Some("not-found"), "{last}");
    assert!(last["text"].as_str().unwrap().contains("the kernel's inputs are missing"), "as for iPhone18,2's: {last}");
    nothing_left(&b, "the A56's request");
    // host profiles and reference-only ones alone are no iOS profile
    for name in ["ios-27.0.1-iPhone99-1.toml", "ios-27.0.1-iPhone99-2.toml", "ios-16.5-a56.toml"] {
        std::fs::remove_file(b.stage(&format!("runtime/lib/profiles/{name}"))).unwrap();
    }
    assert!(names.iter().any(|n| n.contains("a56")), "the A56 profiles stay: {names:?}");
    let last = s.ask(app);
    assert_eq!((last["code"].as_str(), last["text"].as_str().map(|t| t.contains("holds no iOS profile"))), (Some("not-found"), Some(true)), "{last}");
    nothing_left(&b, "the refusals");
    let (status, rest, _) = s.end(true);
    assert_eq!(status, 0);
    clean_bye(&rest, "eof");
}

#[test]
fn make_image_takes_the_staged_runtimes_metal_plugin_for_its_own_image_only() {
    // The phone's Make iOS image gives no Metal plug-in (the app sends none): the staged runtime's lib/guest/ bundle
    // (runtime/package.sh's METAL_DIR, runtime.json "metal") stands in, when its oneios-target is the image's
    // <device> <build>; a runtime for another iOS gives none and the build goes on (another phone's fork).
    let b = Box::new("make-metal");
    let (_, profile) = stand_in_ipsw(&b, 9000, false);
    b.put("stage/runtime/kernel/xnu.macho", &noise(4096, 43));
    let bundle = b.stage("runtime/lib/guest/NativeAdrenoMetal.bundle");
    let with_guest = br#"{"format": 1, "kernel": {"path": "kernel/xnu.macho"}, "metal": {"bundle": "lib/guest/NativeAdrenoMetal.bundle", "record": "lib/guest/NativeAdrenoMetal.build.json", "target": "x", "cdhashes": []}, "camera": {"path": "lib/guest/camera", "cdhash": "00"}}"#;
    b.put("stage/runtime/runtime.json", with_guest);
    b.put("stage/runtime/lib/guest/NativeAdrenoMetal.bundle/Info.plist", b"<plist/>");
    b.put("stage/runtime/lib/guest/camera/H9ISP.mediacapture", b"stand-in");
    begin_image(&b);
    let mut s = Serve::start(&b, &[]);
    let metal_line = |s: &mut Serve, more: Value| -> String {
        s.send(make_image(&b, "m", more.clone()));
        let events = s.request("m");
        let last = events.last().unwrap().clone();
        // past the check step, to the kernel's inputs (kb/ is empty): the same refusal as without a plug-in
        assert_eq!((last["ev"].as_str(), last["code"].as_str()), (Some("refused"), Some("not-found")), "{more}: {last}");
        assert!(last["text"].as_str().unwrap().contains("the kernel's inputs are missing"), "{more}: {last}");
        nothing_left(&b, &more.to_string());
        let lines: Vec<String> = events.iter().filter(|v| v["ev"] == "line").filter_map(|v| v["text"].as_str()).filter(|t| t.starts_with("Metal plug-in: ")).map(str::to_string).collect();
        assert_eq!(lines.len(), 1, "{more}: one Metal line: {events:#?}");
        lines[0].clone()
    };
    // the runtime's bundle is for this image (the stand-in profile's iTest1,1 0T1): taken
    b.put("stage/runtime/lib/guest/NativeAdrenoMetal.bundle/oneios-target", b"iTest1,1 0T1\n");
    assert_eq!(metal_line(&mut s, json!({"profile": profile})), format!("Metal plug-in: {bundle} (the staged runtime's, for iTest1,1 0T1)"));
    // for another image (a runtime for the S24 Ultra's iOS 27, an image of another iOS): left out, the build goes on
    b.put("stage/runtime/lib/guest/NativeAdrenoMetal.bundle/oneios-target", b"iPhone18,2 24A446\n");
    assert_eq!(
        metal_line(&mut s, json!({"profile": profile})),
        format!("Metal plug-in: none: the staged runtime's ({bundle}) is for iPhone18,2 24A446, this image is iTest1,1 0T1 (CPU composition)")
    );
    // "runtime_plugins": false, and a runtime without them (before this package layout): none
    b.put("stage/runtime/lib/guest/NativeAdrenoMetal.bundle/oneios-target", b"iTest1,1 0T1\n");
    let none = "Metal plug-in: none: none given, and the staged runtime carries none (CPU composition)";
    assert_eq!(metal_line(&mut s, json!({"profile": profile, "runtime_plugins": false})), none);
    b.put("stage/runtime/runtime.json", br#"{"format": 1, "kernel": {"path": "kernel/xnu.macho"}}"#);
    assert_eq!(metal_line(&mut s, json!({"profile": profile})), none);
    // a bundle the request gives is the request's (the preflight checks its target)
    b.put("stage/runtime/runtime.json", with_guest);
    assert_eq!(metal_line(&mut s, json!({"profile": profile, "metal_plugin": bundle})), format!("Metal plug-in: {bundle} (given)"));
    // no root tweaks: no plug-in
    assert_eq!(metal_line(&mut s, json!({"profile": profile, "tweaks": false})), "Metal plug-in: none (no root tweaks)");
    let (status, rest, _) = s.end(true);
    assert_eq!(status, 0);
    clean_bye(&rest, "eof");
}

#[test]
fn make_image_asks_for_its_room_once_and_is_refused_when_it_is_short() {
    let b = Box::new("make-space");
    let (web, profile) = stand_in_ipsw(&b, 9000, false);
    let kc = b.put("stage/kb/kernel.kc", &noise(5000, 42));
    begin_image(&b);
    let web = Web::start(&web);
    let mut s = Serve::start_env(&b, &[], &[("ONEIOS_SERVE_TEST_FREE", "1000")]);
    s.send(make_image(&b, "m", json!({"profile": profile, "tweaks": false, "kc": kc, "source": format!("http://127.0.0.1:{}/stand-in.ipsw", web.port)})));
    let events = s.request("m");
    let space = of(&events, "space");
    assert_eq!(space.len(), 1, "one space event: {events:#?}");
    assert!(space[0]["need"].as_u64().unwrap() > 1000 && space[0]["free"] == 1000 && space[0]["reserve"] == 268_435_456, "{}", space[0]);
    let last = events.last().unwrap();
    assert_eq!((last["ev"].as_str(), last["code"].as_str()), (Some("refused"), Some("space")), "{last}");
    assert!(last["text"].as_str().unwrap().contains("Not enough free space"), "{last}");
    let steps: Vec<&str> = of(&events, "step").iter().map(|v| v["step"].as_str().unwrap()).collect();
    assert_eq!(steps, ["check"], "nothing after the check: {events:#?}");
    nothing_left(&b, "space");
    assert!(!Path::new(&b.stage("make/inputs")).exists(), "nothing was fetched");
    let (status, rest, _) = s.end(true);
    assert_eq!(status, 0);
    clean_bye(&rest, "eof");
}

#[test]
fn make_image_fetches_pauses_resumes_is_cancelled_and_goes_on() {
    let b = Box::new("make-fetch");
    // an OS member of 12 MiB on a slow line: the fetch is long enough to be paused and cancelled
    let (web, profile) = stand_in_ipsw(&b, 12 << 20, false);
    let kc = b.put("stage/kb/kernel.kc", &noise(5000, 42));
    begin_image(&b);
    let slow = Web::with(&web, None, &["--rate", "2500000"]);
    let source = format!("http://127.0.0.1:{}/stand-in.ipsw", slow.port);
    let mut s = Serve::start(&b, &["--jobs", "4"]);
    let request = make_image(&b, "m1", json!({"profile": profile, "tweaks": false, "kc": kc, "source": source, "connections": 2}));
    s.send(request);
    let start = s.until(|v| v["id"] == "m1" && v["ev"] == "start").pop().unwrap();
    let listed: Vec<&str> = start["steps"].as_array().unwrap().iter().map(|v| v["step"].as_str().unwrap()).collect();
    assert_eq!(&listed[..4], ["check", "directory", "download", "decrypt"], "{start}");
    assert!(listed.contains(&"kernel-audit") && listed.contains(&"kernel-stubs-check") && listed.ends_with(&["manifest", "verify", "lists"]), "{start}");
    // the pause holds the fetch; the resume lets it go on
    s.until(|v| v["id"] == "m1" && v["ev"] == "progress" && v["step"] == "download" && v["done"].as_u64().unwrap() >= 1_000_000);
    s.send(json!({"cmd": "pause", "id": "m1"}));
    let paused = s.until(|v| v["id"] == "m1" && v["ev"] == "paused").pop().unwrap();
    assert_eq!(paused["step"], "download", "{paused}");
    let mut quiet = Vec::new();
    while let Some(line) = s.next_in(Duration::from_millis(1200)) {
        quiet.push(line);
    }
    assert!(quiet.iter().all(|v| v["ev"] != "progress" && !is_last(v)), "nothing moves while it is paused: {quiet:#?}");
    s.send(json!({"cmd": "resume", "id": "m1"}));
    s.until(|v| v["id"] == "m1" && v["ev"] == "resumed");
    // the cancel: what the build wrote goes, the fetched part stays for the next run
    s.until(|v| v["id"] == "m1" && v["ev"] == "progress" && v["step"] == "download" && v["done"].as_u64().unwrap() >= paused["done"].as_u64().unwrap() + 500_000);
    s.send(json!({"cmd": "cancel", "id": "m1"}));
    let events = s.request("m1");
    let last = events.last().unwrap();
    assert_eq!((last["ev"].as_str(), last["by"].as_str()), (Some("cancelled"), Some("request")), "{last}");
    assert_eq!(s.seen.iter().filter(|v| v["id"] == "m1" && v["ev"] == "space").count(), 1, "one space event");
    nothing_left(&b, "cancel");
    assert!(Path::new(&b.stage("make/inputs/001-00001-001.dmg.part")).exists(), "the part is kept for the next run");

    // the next run goes on from the part, fetches the rest and stops at the place step: the OS member is no APFS
    // image. Everything it wrote goes; the fetched members stay.
    let port = slow.port;
    drop(slow);
    let fast = Web::with(&web, Some(port), &[]);
    let request = make_image(&b, "m2", json!({"profile": profile, "tweaks": false, "kc": kc, "source": format!("http://127.0.0.1:{}/stand-in.ipsw", fast.port)}));
    s.send(request);
    let events = s.request("m2");
    let last = events.last().unwrap();
    assert_eq!((last["ev"].as_str(), last["code"].as_str()), (Some("failed"), Some("failed")), "{events:#?}");
    assert!(last["text"].as_str().unwrap().contains("is not a raw APFS image"), "{last}");
    assert!(of(&events, "line").iter().any(|v| v["text"].as_str().unwrap().starts_with("001-00001-001.dmg: going on from ")), "{events:#?}");
    let steps: Vec<&str> = of(&events, "step").iter().map(|v| v["step"].as_str().unwrap()).collect();
    assert_eq!(steps, ["check", "directory", "download", "decrypt", "place"], "{events:#?}");
    assert_eq!(of(&events, "space").len(), 1, "one space event, not one more of the fetch: {events:#?}");
    nothing_left(&b, "failure");
    assert_eq!(std::fs::metadata(b.stage("make/inputs/001-00001-001.dmg")).unwrap().len(), 12 << 20, "the fetched members stay");
    assert!(Path::new(&b.stage("make/inputs/inputs.json")).is_file());
    let (status, rest, stderr) = s.end(true);
    assert_eq!((status, stderr.as_str()), (0, ""));
    clean_bye(&rest, "eof");
}

#[test]
fn aea_decrypts_with_the_key_services_key_and_refuses_a_changed_archive() {
    use oneios_image::aea_decrypt::testkit::{encrypt, service_key, Spec};
    let b = Box::new("aea");
    let key = [0x42u8; 32];
    let (pem, response) = service_key(3, &key);
    // the key service: a local server with the key's file
    b.put("keys/Kid=", pem.as_bytes());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}/Kid=", listener.local_addr().unwrap().port());
    let root = b.top.join("keys");
    std::thread::spawn(move || oneios_image::rangeserve::serve_on(listener, root, Default::default()));
    let plain = noise(5 * 4096 * 4 + 333, 21);
    let fields = vec![("com.apple.wkms.fcs-response".to_string(), response), ("com.apple.wkms.fcs-key-url".to_string(), url.into_bytes())];
    let archive = encrypt(&plain, &key, &Spec { compression: b'e', fields, ..Spec::default() });
    let input = b.put("in/os.dmg.aea", &archive);
    let mut s = Serve::start(&b, &["--jobs", "3"]);
    let (out, cache) = (b.stage("work/os.img"), b.stage("work/keys"));
    s.send(json!({"id": "a", "cmd": "aea", "in": input, "out": out, "sha256": sha256(&plain), "key_cache": cache}));
    let events = s.request("a");
    let ok = events.last().unwrap();
    assert_eq!((ok["ev"].as_str(), ok["bytes"].as_u64(), ok["sha256"].as_str(), ok["key"].as_str()), (Some("ok"), Some(plain.len() as u64), Some(sha256(&plain).as_str()), Some("service")), "{events:#?}");
    assert_eq!(std::fs::read(&out).unwrap(), plain);
    let steps: Vec<&str> = events.iter().filter(|v| v["ev"] == "step").map(|v| v["step"].as_str().unwrap()).collect();
    assert_eq!(steps, ["key", "decrypt", "place"]);
    let last = events.iter().filter(|v| v["ev"] == "progress" && v["step"] == "decrypt").last().unwrap();
    assert_eq!((last["done"].as_u64(), last["unit"].as_str()), (Some(plain.len() as u64), Some("bytes")), "{last}");
    assert!(events.iter().any(|v| v["ev"] == "space"), "a space event");
    assert!(Path::new(&cache).join("Kid=.pem").is_file());
    // the second archive: the key from the cache
    let out2 = b.stage("work/os2.img");
    let ok = s.ask(json!({"id": "b", "cmd": "aea", "in": input, "out": out2, "key_cache": cache}));
    assert_eq!((ok["ev"].as_str(), ok["key"].as_str()), (Some("ok"), Some("cache")), "{ok}");
    // refusals: the output is there, a wrong pin, a changed byte; nothing is left of them
    let mut bad = archive.clone();
    let n = bad.len();
    bad[n / 2] ^= 4;
    let changed = b.put("in/bad.dmg.aea", &bad);
    let out3 = b.stage("work/os3.img");
    for (request, code, words) in [
        (json!({"id": "c", "cmd": "aea", "in": input, "out": out, "key_cache": cache}), "exists", "is there already"),
        (json!({"id": "d", "cmd": "aea", "in": input, "out": out3, "key_cache": cache, "sha256": "0".repeat(64)}), "mismatch", "nothing was kept"),
        (json!({"id": "e", "cmd": "aea", "in": changed, "out": out3, "key_cache": cache}), "auth", "does not authenticate"),
        (json!({"id": "f", "cmd": "aea", "in": input, "out": out3, "key_cache": cache, "key_file": input}), "usage", "not both"),
    ] {
        let last = s.ask(request.clone());
        assert_eq!((last["ev"].as_str(), last["code"].as_str()), (Some("refused"), Some(code)), "{request}: {last}");
        assert!(last["text"].as_str().unwrap().contains(words), "{request}: {last}");
        assert!(!Path::new(&out3).exists() && !Path::new(&format!("{out3}.part")).exists());
    }
    let (status, rest, _) = s.end(true);
    assert_eq!(status, 0);
    clean_bye(&rest, "eof");
}

#[test]
fn make_image_decrypts_the_aea_members_with_the_key_services_key() {
    use oneios_image::aea_decrypt::testkit::{encrypt, service_key, Spec};
    let b = Box::new("make-aea");
    let key = [0x24u8; 32];
    let (pem, response) = service_key(5, &key);
    b.put("keys/K5=", pem.as_bytes());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}/K5=", listener.local_addr().unwrap().port());
    let root = b.top.join("keys");
    std::thread::spawn(move || oneios_image::rangeserve::serve_on(listener, root, Default::default()));
    let fields = vec![("com.apple.wkms.fcs-response".to_string(), response), ("com.apple.wkms.fcs-key-url".to_string(), url.into_bytes())];
    let seal = |data: Vec<u8>| encrypt(&data, &key, &Spec { compression: b'e', fields: fields.clone(), ..Spec::default() });
    let (web, profile) = stand_in_ipsw_sealed(&b, 9000, true, &seal);
    let kc = b.put("stage/kb/kernel.kc", &noise(5000, 42));
    begin_image(&b);
    let web = Web::start(&web);
    let mut s = Serve::start(&b, &[]);
    s.send(make_image(&b, "m", json!({"profile": profile, "tweaks": false, "kc": kc, "source": format!("http://127.0.0.1:{}/stand-in.ipsw", web.port)})));
    let events = s.request("m");
    // decrypted (the stand-in OS image is noise, so the build stops at the place step's APFS check)
    let last = events.last().unwrap();
    assert!(last["text"].as_str().unwrap_or("").contains("not a raw APFS image"), "{last}");
    let steps: Vec<&str> = events.iter().filter(|v| v["ev"] == "step").map(|v| v["step"].as_str().unwrap()).collect();
    assert!(steps.windows(2).any(|w| w == ["decrypt", "place"]), "the decryption says no steps of its own: {steps:?}");
    assert_eq!(of(&events, "space").len(), 1, "one space event");
    assert!(events.iter().any(|v| v["ev"] == "progress" && v["step"] == "decrypt"), "the decrypt step's progress");
    assert_eq!(std::fs::read(b.stage("make/inputs/001-00001-001.dmg")).unwrap(), noise(9000, 31));
    assert_eq!(std::fs::read(b.stage("make/inputs/001-00002-001.dmg")).unwrap(), noise(70_000, 32));
    assert!(Path::new(&b.stage("make/inputs/.aea-keys/K5=.pem")).is_file(), "the key, kept in the inputs' folder");
    assert!(!Path::new(&b.stage("make/inputs/001-00001-001.dmg.aea")).exists(), "the encrypted member is removed");
    nothing_left(&b, "make-aea");
    // without the fetch: the encrypted members are inputs (the decrypt step makes the plain images), not missing ones
    for (name, plain) in [("001-00001-001.dmg", noise(9000, 31)), ("001-00002-001.dmg", noise(70_000, 32))] {
        std::fs::write(b.stage(&format!("make/inputs/{name}.aea")), seal(plain)).unwrap();
        std::fs::remove_file(b.stage(&format!("make/inputs/{name}"))).unwrap();
    }
    begin_image(&b);
    let last = s.ask(make_image(&b, "n", json!({"profile": profile, "tweaks": false, "kc": kc, "fetch": false})));
    assert!(last["text"].as_str().unwrap_or("").contains("not a raw APFS image"), "{last}");
    assert_eq!(std::fs::read(b.stage("make/inputs/001-00001-001.dmg")).unwrap(), noise(9000, 31));
    nothing_left(&b, "make-aea, no fetch");
    let (status, rest, _) = s.end(true);
    assert_eq!(status, 0);
    clean_bye(&rest, "eof");
}
