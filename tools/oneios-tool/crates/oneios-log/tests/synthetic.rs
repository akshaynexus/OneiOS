//! oneios-tool log on log text written here: short invented lines in the formats of xnuhost and of the hosted
//! kernel's reports, with the damage real logs have (a record cut and the next appended, a lost first letter).
//! The backtraces are named in a tiny dyld shared cache made here (no Apple bytes).

use oneios_log::parse::{self, Src};
use oneios_log::table::{Cell, Format};
use oneios_log::{diff, reports, run_inner, stacks, table, timeline};
use oneios_sym::Locate;
use std::path::{Path, PathBuf};

const BASE: u64 = 0x1_8000_0000;
const TEXT: u64 = BASE + 0x1000;

fn put(buf: &mut [u8], at: usize, bytes: &[u8]) {
    buf[at..at + bytes.len()].copy_from_slice(bytes);
}

/// A cache of one file and one image: `_f1`..`_f5` at TEXT + 0x100 * n, each with a call at +4.
fn build_cache(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let mut main = vec![0u8; 0x8000];
    let p32 = |b: &mut Vec<u8>, at: usize, v: u32| put(b, at, &v.to_le_bytes());
    let p64 = |b: &mut Vec<u8>, at: usize, v: u64| put(b, at, &v.to_le_bytes());
    put(&mut main, 0, b"dyld_v1  arm64e\0");
    p32(&mut main, 0x10, 0x200);
    p32(&mut main, 0x14, 1);
    put(&mut main, 0x58, &[0x77; 16]);
    p64(&mut main, 0xE0, BASE);
    p64(&mut main, 0xE8, 0x4000);
    p64(&mut main, 0xF0, 0x10000);
    p64(&mut main, 0x200, BASE);
    p64(&mut main, 0x208, 0x8000);
    p32(&mut main, 0x218, 3); // the mapping may be written to: the cache's data starts at BASE
    p32(&mut main, 0x1000 + 0x4104, 0x9400_0001); // a second call 0x4000 above _f1's: one address fits two slides
    p32(&mut main, 0x1C0, 0x400);
    p32(&mut main, 0x1C4, 1);
    p64(&mut main, 0x400, TEXT);
    p32(&mut main, 0x400 + 24, 0x500);
    put(&mut main, 0x500, b"/usr/lib/libtest.dylib\0");
    // The Mach-O: __TEXT, __LINKEDIT (file offset 0x3000 of this file), LC_SYMTAB.
    p32(&mut main, 0x1000 + 16, 3);
    let mut at = 0x1020;
    for (name, vmaddr, size) in [("__TEXT", TEXT, 0x1000u64), ("__LINKEDIT", BASE + 0x3000, 0x1000)] {
        p32(&mut main, at, 0x19);
        p32(&mut main, at + 4, 72);
        put(&mut main, at + 8, name.as_bytes());
        p64(&mut main, at + 24, vmaddr);
        p64(&mut main, at + 32, size);
        at += 72;
    }
    p32(&mut main, at, 0x2);
    p32(&mut main, at + 4, 24);
    p32(&mut main, at + 8, 0x3000);
    p32(&mut main, at + 12, 5);
    p32(&mut main, at + 16, 0x3400);
    let mut strx = 1;
    for n in 1..=5usize {
        p32(&mut main, 0x1000 + 0x100 * n + 4, 0x9400_0001); // bl
        p32(&mut main, 0x3000 + 16 * (n - 1), strx as u32);
        main[0x3000 + 16 * (n - 1) + 4] = 0x0f;
        p64(&mut main, 0x3000 + 16 * (n - 1) + 8, TEXT + 0x100 * n as u64);
        let name = format!("_f{n}\0");
        put(&mut main, 0x3400 + strx, name.as_bytes());
        strx += name.len();
    }
    std::fs::write(dir.join("dyld_shared_cache_arm64e"), &main).unwrap();
    let mut sym = vec![0u8; 0x200];
    put(&mut sym, 0, b"dyld_v1  arm64e\0");
    p64(&mut sym, 0x48, 0x100);
    std::fs::write(dir.join("dyld_shared_cache_arm64e.symbols"), &sym).unwrap();
}

/// The return address in `_fN` (after its call), slid.
fn ret(n: u64, slide: u64) -> String {
    format!("{:#x}", TEXT + 0x100 * n + 8 + slide)
}

fn stacks_log() -> String {
    let s1 = 0x4000;
    let s2 = 0xc000;
    let r = |n| ret(n, s1);
    let q = |n| ret(n, s2);
    let mut t = String::new();
    t += "oneios inner: 10:00:00 pid 100\n";
    t += "[   100.000] xnuhost I xnuhost: session ABI 1, mode takeover, pid 100\n";
    t += "[   101.000] guest     Darwin Kernel Version 25.5.0: a test; root:xnu/DEVELOPMENT\n";
    // Sample 1: thread 0x11's line is cut in a word and thread 0x12's record is appended to it.
    t += "[   200.000] guest     oneios-ubt: 3 thread(s) of appd\n";
    t += &format!("[   200.001] guest     oneios-ubt: appd[7] thread 0x10 state 0x1: {} {} {}\n", r(1), r(2), r(3));
    t += &format!("[   200.001] guest     oneios-ubt: appd[7] thread 0x11 state 0x41: {} {} 0x18000oneios-ubt: appd[7] thread 0x12 state 0x4: {} 0x0\n", r(1), r(2), r(4));
    // Sample 2: its header is appended to a cut line; thread 0x11's record lost its first letter, so the
    // record before it lost its end.
    t += "[   260.000] guest     vm: host_statistics64() count is larger than expected. HOST_VM_Ioneios-ubt: 2 thread(s) of appd\n";
    t += &format!("[   260.001] guest     oneios-ubt: appd[7] thread 0x10 state 0x1: {} {} {}\n", r(1), r(2), r(3));
    t += &format!("[   260.001] guest     neios-ubt: appd[7] thread 0x11 state 0x4: {} {}\n", r(5), r(3));
    // Sample 3: no header line (lost); the gap in time starts it.
    t += &format!("[   320.001] guest     oneios-ubt: appd[7] thread 0x10 state 0x1: {} {} {}\n", r(1), r(2), r(3));
    t += &format!("[   320.001] guest     oneios-ubt: appd[7] thread 0x11 state 0x4: {} {}\n", r(5), r(3));
    t += "[   320.002] guest     oneios-ubt: appd[7] thread zz state 0x4: 0x1\n";
    // A restart in place: the second boot has another slide. An exception with its backtrace.
    t += "[   400.000] xnuhost I the guest asked for a restart: starting again (1)\n";
    t += "[   401.000] guest     Darwin Kernel Version 25.5.0: a test; root:xnu/DEVELOPMENT\n";
    t += "[   500.000] guest     oneios-ubt: 1 thread(s) of appd\n";
    t += &format!("[   500.001] guest     oneios-ubt: appd[9] thread 0x20 state 0x1: {} {} {} {}\n", q(1), q(2), q(3), q(4));
    t += "[   510.000] guest     sheng-exc: crashd[12] exception type=0x1 code=0x1 subcode=0x0\n";
    t += &format!("[   510.000] guest     sheng-exc: pc=0x0 lr={} sp=0x16f000000 far=0x0 esr=0x8200000f\n", q(5));
    t += "[   510.000] guest     sheng-exc: x0=0x1 0x2 0x3 0x4\n";
    t += &format!("[   510.001] guest     sheng-ubt sheng-exc: pc=0x0 lr={} sp=0x16f000000 fp=0x16f000010 x0=0x1 x1=0x2 x2=0x3\n", q(5));
    t += &format!("[   510.001] guest     sheng-ubt sheng-exc: #0 fp=0x16f000020 ret={}\n", q(2));
    t += &format!("[   510.001] guest     sheng-ubt sheng-exc: #1 fp=0x16f000030 ret={}\n", q(1));
    t
}

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("log-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    fn log(&self, name: &str, text: &str) -> String {
        let path = self.root.join(name);
        std::fs::write(&path, text).unwrap();
        path.display().to_string()
    }

    fn loc(&self) -> Locate {
        let dsc = self.root.join("dsc");
        if !dsc.exists() {
            build_cache(&dsc);
        }
        Locate { dsc: Some(dsc), index_parent: Some(self.root.join("index")) }
    }

    fn run(&self, args: &[&str]) -> String {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let first = run_inner(&args, self.loc(), &mut Vec::new()).unwrap();
        let again = run_inner(&args, self.loc(), &mut Vec::new()).unwrap();
        assert_eq!(first, again, "two runs differ: {args:?}");
        first
    }
}

#[test]
fn joined_and_cut_lines_become_records() {
    let log = parse::parse("t", stacks_log().as_bytes());
    assert_eq!(log.lines, 22);
    let ubt: Vec<&parse::Rec> = log.recs.iter().filter(|r| r.text.starts_with("oneios-ubt: ")).collect();
    assert_eq!(ubt.len(), 12);
    // Line 6: two records; the first is cut, the second appended.
    let line6: Vec<&&parse::Rec> = ubt.iter().filter(|r| r.line == 6).collect();
    assert_eq!(line6.len(), 2);
    assert!(line6[0].tail_cut && !line6[0].appended && line6[1].appended && !line6[1].tail_cut);
    // Line 7: the vm: record is cut, the header appended.
    let line7: Vec<&parse::Rec> = log.recs.iter().filter(|r| r.line == 7).collect();
    assert!(line7[0].text.starts_with("vm: ") && line7[0].tail_cut && line7[1].text == "oneios-ubt: 2 thread(s) of appd");
    // Line 9 lost its first letter: put back, and the record before it counts as cut.
    let line9 = log.recs.iter().find(|r| r.line == 9).unwrap();
    assert!(line9.head_cut && line9.text.starts_with("oneios-ubt: appd[7] thread 0x11"));
    assert!(log.recs.iter().find(|r| r.line == 8).unwrap().tail_cut);
    // The framing.
    assert_eq!(log.recs[0].src, Src::Raw);
    assert_eq!((log.recs[1].src, log.recs[1].time), (Src::Host('I'), Some(100.0)));
    assert_eq!(log.recs[2].src, Src::Guest);
    // `sheng-ubt sheng-exc:` is one record; boot arguments are not split.
    assert_eq!(parse::split_joined("sheng-ubt sheng-exc: #0 fp=0x1 ret=0x2").len(), 1);
    assert_eq!(parse::split_joined("boot-args = sheng-smp=1 sheng-exc-name=a,b oneios-ubt=a oneios-gpu-report=10").len(), 1);
    assert_eq!(parse::split_joined("x 0x1oneios-cpu: 10 s: a[1] 2% (all 2%)ONEIOS WIFI: on"), ["x 0x1", "oneios-cpu: 10 s: a[1] 2% (all 2%)", "ONEIOS WIFI: on"]);
    assert_eq!(parse::boots(&log).len(), 2);
}

#[test]
fn stacks_per_thread_with_the_slide_of_each_boot() {
    let f = Fixture::new("stacks");
    let path = f.log("x.log", &stacks_log());
    let out = f.run(&["stacks", &path]);
    // Boot 1: slide 0x4000 from its own addresses; boot 2: 0xc000.
    assert!(out.contains("boot 1 (lines 1-12): 3 sample(s), 0 exception(s); slide 0x4000 (5 of 5 distinct addresses follow a call)"), "{out}");
    assert!(out.contains("boot 2 (lines 13-22): 1 sample(s), 1 exception(s); slide 0xc000 (5 of 5 distinct addresses follow a call)"), "{out}");
    assert!(out.contains("  sample 2 (260.000 s, line 7): 2 thread(s) read of the 2 of appd\n"), "{out}");
    assert!(out.contains("  sample 3 (320.001 s, line 10): 2 thread(s) read (its header line is not in the log)\n"), "{out}");
    // Thread 0x10: one stack in all three samples (the cut line of sample 2 is its start).
    let stuck = "== appd[7] thread 0x10: in 3 of 3 sample(s), 1 stack(s)\n  \
                 sample 1 (200.000 s, line 5); samples 2-3: same; state 0x1 wait; 1 of 3 line(s) cut:\n    \
                 0x180005108  libtest.dylib  _f1 + 0x8\n    \
                 0x180005208  libtest.dylib  _f2 + 0x8\n    \
                 0x180005308  libtest.dylib  _f3 + 0x8\n";
    assert!(out.contains(stuck), "{out}");
    // Thread 0x11: the cut stack of sample 1 (its cut last address dropped), then another one twice.
    let changed = "== appd[7] thread 0x11: in 3 of 3 sample(s), 2 stack(s)\n  \
                   sample 1 (200.000 s, line 6); state 0x41 wait+wait-report; 1 of 1 line(s) cut (this stack may be longer):\n    \
                   0x180005108  libtest.dylib  _f1 + 0x8\n    \
                   0x180005208  libtest.dylib  _f2 + 0x8\n  \
                   sample 2 (260.000 s, line 9); sample 3: same; state 0x4 run:\n    \
                   0x180005508  libtest.dylib  _f5 + 0x8\n    \
                   0x180005308  libtest.dylib  _f3 + 0x8\n";
    assert!(out.contains(changed), "{out}");
    assert!(out.contains("    0x0  (not in a cache image; unslid -0x4000)\n"), "{out}");
    let summary = "summary of boot 1: 3 sample(s), 3 thread(s), 0 exception(s)\n  \
                   never changed (the same stack in every sample that has the thread, 2 or more): 1\n    \
                   appd[7] thread 0x10  (3 samples)  _f1 <- _f2 <- _f3\n  \
                   changed: 1\n    \
                   appd[7] thread 0x11  (3 samples, 2 stacks)  _f5 <- _f3\n  \
                   in one sample only: 1\n    \
                   appd[7] thread 0x12  (1 samples)  _f4 <- 0x0\n  \
                   unparsed: 1 backtrace line(s), the first at line 12\n";
    assert!(out.contains(summary), "{out}");
    // The exception of boot 2, named with boot 2's slide.
    let exception = "== exceptions (sheng-exc)\n  \
                     crashd[12] (510.000 s, line 17): type=0x1 code=0x1 subcode=0x0\n    \
                     pc 0x0 lr 0x18000d508 far 0x0 esr 0x8200000f\n    \
                     0x0  (not in a cache image; unslid -0xc000)\n    \
                     0x18000d508  libtest.dylib  _f5 + 0x8\n    \
                     0x18000d208  libtest.dylib  _f2 + 0x8\n    \
                     0x18000d108  libtest.dylib  _f1 + 0x8\n";
    assert!(out.contains(exception), "{out}");

    // Filters, a given slide, no names.
    let one = f.run(&["stacks", &path, "--proc", "appd", "--thread", "0x20"]);
    assert!(one.contains("== appd[9] thread 0x20: in 1 of 1 sample(s), 1 stack(s)") && !one.contains("thread 0x10:") && !one.contains("crashd"), "{one}");
    let raw = f.run(&["stacks", &path, "--no-sym", "--proc", "appd"]);
    assert!(raw.contains("; addresses not named\n") && raw.contains("    0x180005108\n") && !raw.contains("libtest"), "{raw}");
    let given = f.run(&["stacks", &path, "--slide", "0x4000", "--summary"]);
    assert!(given.contains("slide 0x4000 (given)") && !given.contains("== appd"), "{given}");
    // The library's parser on one line.
    let t = stacks::parse_thread("My App[41] thread 0x2b5 state 0x41: 0x10 0x20 x", false, 3).unwrap();
    assert_eq!((t.proc_name.as_str(), t.pid, t.tid, t.state, t.addrs.as_slice()), ("My App", 41, 0x2b5, 0x41, [0x10, 0x20].as_slice()));
    assert!(stacks::parse_thread("appd[x] thread 0x1 state 0x1: 0x1", false, 1).is_none());
}

#[test]
fn a_tie_is_settled_by_the_unnest_range() {
    let f = Fixture::new("tie");
    // One return address: it follows a call at slide 0x8000 and at slide 0x4000.
    let thread = format!("[   200.001] guest     oneios-ubt: appd[7] thread 0x10 state 0x1: {}\n", ret(1, 0x8000));
    let banner = "[   101.000] guest     Darwin Kernel Version 25.5.0: a test; root:xnu/DEVELOPMENT\n";
    let alone = f.log("alone.log", &format!("{banner}{thread}"));
    let out = f.run(&["stacks", &alone]);
    assert!(out.contains("slide 0x4000 (1 of 1 distinct addresses follow a call; 2 slides fit as well: NOT reliable)"), "{out}");
    // The kernel's unnest line: the cache's data (BASE) slid is at 0x180008000, so the slide is 0x8000.
    let unnest = "[   150.000] guest     appd[7] triggered unnest of range 0x180008000->0x182008000 of DYLD shared region in VM map 0x1. While not abnormal for debuggers, this increases system memory footprint\n";
    let with = f.log("with.log", &format!("{banner}{unnest}{thread}"));
    let out = f.run(&["stacks", &with]);
    assert!(
        out.contains("slide 0x8000 (1 of 1 distinct addresses follow a call; 2 slides fit as well: this one puts the cache's data in the unnest range 0x180008000 of line 2)"),
        "{out}"
    );
    assert!(out.contains("    0x180009108  libtest.dylib  _f1 + 0x8\n"), "{out}");
}

fn timeline_log() -> String {
    let mut t = String::new();
    t += "[   100.000] xnuhost I xnuhost: session ABI 1, mode takeover, pid 100\n";
    t += "[   100.500] xnuhost I placed: entry 0x1; boot-args \"debug=0x14e -v oneios-ubt=appd oneios-gpu-report=10\"\n";
    t += "[   101.000] xnuhost I cpu0: entering the kernel at 0x2003dbb488\n";
    t += "[   101.100] guest     Darwin Kernel Version 25.5.0: a test; root:xnu/DEVELOPMENT\n";
    t += "[   101.500] guest     BSD root: disk7s1, major 1, minor 10\n";
    t += "[   102.000] guest     load_init_program: attempting to load /sbin/launchd\n";
    t += "[   103.000] guest     com.apple.xpc.launchd|2026-01-01 00:00:00.000000 <Notice>: hello\n";
    t += "[   103.100] guest     com.apple.xpc.launchd|2026-01-01 00:00:00.100000 <Notice>: again\n";
    t += "[   110.000] guest     oneios-display: framebuffer opened by backboardd[53], type 0: status 0, client IOMobileFramebufferUserClient\n";
    t += "[   111.000] guest     oneios-display: swap 1 accepted (reason 0)\n";
    t += "[   111.100] xnuhost I guest ready: first frame\n";
    t += "[   112.000] guest     SpringBoard[30] triggered unnest of range 0x1->0x2 of DYLD shared region\n";
    // A gesture with one swap in its time (W2: guest clock 400.2 = the log's 150.2).
    t += "[   150.000] xnuhost I input: touch: 10 frames in 100 ms, 1 contact, a frame every 8.0..8.4 ms; read 1/2/3 us after the scan (min/avg/max); the guest's next frame 12.5 ms after the touch-down\n";
    t += "[   160.000] guest     oneios-iobusy: 30 s: busy 1: defaults (class IOPlatformDevice, id 0x10000013d), inactive 0\n";
    t += "[   160.000] guest     oneios-iobusy: 30 s: 1 services busy, the root's count 2\n";
    t += "[   190.000] xnuhost I input: touch: 20 frames in 200 ms, 1 contact, a frame every 8.0..8.4 ms; read 1/2/3 us after the scan (min/avg/max); no guest frame after the touch-down\n";
    t += "[   200.000] guest     oneios-display-report: prepare calls=3 failures=0 retried=0 max_attempts=0 waited_ms=0 vsync hz=120 sent=0 arms=0 idle=0 late=0 start=1 refresh=1\n";
    t += "[   200.000] guest     oneios-display-report: clients 2; swaps validated 3 (accepted 3, refused 0, last reason 0); transfers 3 (status 0); retirements 3 (generation 64, presented 0); guard slot 0 caller 0x0\n";
    t += "[   200.000] guest     oneios-display-report: swapstat validated 3 kicks 6 in-flight 1 not-ready 0 no-head 2 latch-busy 0 fence 0 copy 3 blank 0 flips 0 latches 0 retires 3\n";
    t += "[   200.000] guest     oneios-display-report: W1 t=361.000s +0ms offer=0ms done=200ms\n";
    t += "[   200.000] guest     oneios-display-report: W2 t=400.200s +39200ms offer=1ms done=7ms\n";
    t += "[   200.000] guest     oneios-display-report: W3 t=420.000s +19800ms offer=0ms done=9ms\n";
    t += "[   205.000] xnuhost E drm2kgsl: context 5: constrained\n";
    t += "[   210.000] guest     sheng-exc: crashd[12] exception type=0x1 code=0x1 subcode=0x0\n";
    t += "[   210.100] guest     Process[12] crashed: crashd. Too many corpses being created.\n";
    t += "[   230.000] xnuhost I input: touch: 5 frames in 40 ms, 1 contact, a frame every 8.0..8.4 ms; read 1/2/3 us after the scan (min/avg/max)\n";
    t += "[   240.000] xnuhost I stopping: signal 15 (reason 0, code 0)\n";
    t += "[   240.100] xnuhost I exit 0\n";
    t
}

#[test]
fn timeline_milestones_and_gestures() {
    let f = Fixture::new("timeline");
    let path = f.log("t.log", &timeline_log());
    let out = f.run(&["timeline", &path]);
    let lines: Vec<&str> = out.lines().collect();
    let row = |what: &str| lines.iter().find(|l| l.contains(what)).unwrap_or_else(|| panic!("no {what:?} in\n{out}")).to_string();
    assert!(out.contains("swaps: 3 W record(s); the guest's clock is the log's + 250.000 s (exact)"), "{out}");
    assert_eq!(row("xnuhost starts"), "     100.000     +0.000       1  xnuhost starts: session ABI 1, mode takeover, pid 100");
    assert_eq!(row("cpu0 enters"), "     101.000     +1.000       3  cpu0 enters the kernel");
    assert!(row("kernel starts").contains("Darwin Kernel Version 25.5.0"));
    assert!(row("BSD root").ends_with("  5  BSD root: disk7s1, major 1, minor 10"));
    assert!(row("launchd's first line").contains("     7  "));
    assert_eq!(lines.iter().filter(|l| l.contains("launchd's first line")).count(), 1);
    assert!(row("backboardd first seen").contains("     9  "));
    assert!(row("SpringBoard first seen").contains("    12  "));
    assert!(row("GUEST READY").starts_with("     111.100    +11.100      11  "));
    assert!(row("10 touch frames").ends_with("gesture: 10 touch frames in 100 ms; xnuhost: the guest's next frame 12.5 ms after the touch-down; W records: 1 swap(s)"), "{out}");
    assert!(row("20 touch frames").ends_with("xnuhost: NO guest frame from the touch-down to the lift; W records: 0 swap(s)"), "{out}");
    // After the last report nothing says how many swaps came.
    assert!(row("5 touch frames").ends_with("xnuhost: not measured (the screen was not still at the touch-down); W records: 0 swap(s) or more (they do not hold every swap of that time)"), "{out}");
    assert!(row("display report").ends_with("display report: 3 swap(s) since the report before (3 since the boot)"));
    assert!(row("services busy").contains("oneios-iobusy: 30 s: 1 services busy"));
    assert!(!out.contains("busy 1: defaults"));
    assert!(row("CRASH").ends_with("CRASH: crashd[12] exception type=0x1 code=0x1 subcode=0x0"));
    assert!(row("died:").contains("Process[12] crashed: crashd."));
    assert!(row("END: stopping").ends_with("END: stopping: signal 15 (reason 0, code 0)"));
    assert!(row("END: exit").ends_with("END: exit 0"));
    assert!(out.contains("(1 `xnuhost E` line(s) in this boot)"));
    // The rows are in the log's order.
    let numbers: Vec<usize> = lines.iter().filter_map(|l| l.get(25..31)?.trim().parse().ok()).collect();
    assert!(numbers.windows(2).all(|w| w[0] <= w[1]) && numbers.len() > 15, "{numbers:?}");
}

#[test]
fn diff_of_two_logs() {
    let f = Fixture::new("diff");
    let a = f.log("a.log", &timeline_log());
    let other = timeline_log()
        .replace("oneios-ubt=appd oneios-gpu-report=10", "oneios-ubt=appd,other sheng-vsync=120")
        .replace("[   205.000] xnuhost E drm2kgsl: context 5: constrained\n", "")
        .replace("crashd[12] exception", "crashd[99] exception")
        + "[   250.000] guest     ONEIOS WIFI: wifid[41] gets 12: len 23 val 0 -> 0 (len 12 val 0)\n"
        + "[   251.000] xnuhost I input: touch: 5 frames in 40 ms, 1 contact\n"
        + "[   252.000] xnuhost I input: touch: 5 frames in 40 ms, 1 contact\n"
        + "[   253.000] xnuhost I input: touch: 5 frames in 40 ms, 1 contact\n";
    let b = f.log("b.log", &other);
    let out = f.run(&["diff", &a, &b]);
    assert!(out.contains("  differs:   oneios-ubt: A appd  B appd,other\n"), "{out}");
    assert!(out.contains("  only in A: oneios-gpu-report=10\n") && out.contains("  only in B: sheng-vsync=120\n"), "{out}");
    assert!(out.contains("== only in A (1 kinds)\n   count    line  kind\n       1      23  xnuhost E drmNkgsl: context N: constrained\n"), "{out}");
    assert!(out.contains("== only in B (1 kinds)\n   count    line  kind\n       1      28  guest ONEIOS WIFI: wifid[N] gets N: len ...\n"), "{out}");
    // The pid does not make a kind; the gesture lines are in both: 3 and 6.
    assert!(!out.contains("crashd"), "{out}");
    assert!(out.contains("       3       6      13      13  xnuhost I input: touch: N frames in N ...\n"), "{out}");
    assert_eq!(diff::kind(Src::Guest, "oneios-ubt: appd[7] thread 0x10 state 0x1: 0x180005108 0x180005208 0x1", 6), "guest oneios-ubt: appd[N] thread 0xN state 0xN: ...");
    assert_eq!(diff::kind(Src::Guest, "oneios-cpu: 10 s: a[1] 2%, b[2] 1% (all 3%)", 6), "guest oneios-cpu: N s: ...");
    assert_eq!(diff::kind(Src::Guest, "handle_mount:893: disk8s1 vol-uuid: BBA627F7-F5DC-47E9-BD09-F4186021465B block", 6), "guest handle_mount:N: diskNsN vol-uuid: HEX block");
}

fn reports_log() -> String {
    let mut t = String::new();
    t += "[   300.000] guest     oneios-cpu: 10 s: kernel_task[0] 259%, launchd[1] 18% (all 277%)\n";
    t += "[   310.000] guest     oneios-cpu: 10 s: My App[41] 50%, launchd[1] 2% (all 52%)\n";
    t += "[   310.000] guest     oneios-gpu-report: 10 s: submits +11 (fenced +1); frames +1 mean 7328 us max ever 7328 us; <1/<2/<4/<8/<16/<32 ms +0/+1/+2/+3/+4/+5 longer +6; fence timeouts +7; refusals +8 (op 0 0)\n";
    t += "[   310.000] guest     oneios-gpu-memory: blobs +23 closed +1 (deferred +2, waiting 3); live 21: own 19 (12 MiB), wired 2 (17 MiB); contexts +2 clients 1\n";
    t += "[   310.000] guest     oneios-gpu-clients: backboardd 12+17 MiB, SpringBoard 191+4 MiB\n";
    // A report cut after `frames +2 me`: another record was written into it.
    t += "[   320.000] guest     oneios-gpu-report: 10 s: submits +5 (fenced +5); frames +2 meoneios-gpu-memory: blobs +1 closed +0 (deferred +0, waiting 0); live 22: own 20 (13 MiB), wired 2 (17 MiB); contexts +0 clients 1\n";
    t += "[   320.000] guest     oneios-gpu-report: garbage\n";
    t += "[   321.000] xnuhost I gpu: 10 s host: backboardd fences 74, submit->retired mean 3859 us max 20242 us; submit calls mean 26 us max 783 us; other fences 7, mean 7800 us; blobs made 183, mean 883 us max 40295 us (udmabufs 427 us of it); clock held +1 times, keep-alives +28\n";
    t += "[   322.000] xnuhost I gpu: 75 s host: backboardd fences 0, submit->retired mean 0 us max 0 us; submit calls mean 0 us max 0 us; other fences 0, mean 0 us; blobs made 2, mean 1145 us max 2236 us\n";
    t += "[   323.000] xnuhost I gpu: drm2kgsl on /dev/kgsl-3d0: chip 0x43051400\n";
    t += "[   330.000] xnuhost I display: 10 s: flushes 66, copy mean 2315 us max 5034 us (1131 MiB)\n";
    t += "[   330.000] xnuhost I display: shm, guest frame 1440x3120 at 120 Hz\n";
    t += "[   340.000] guest     oneios-perf: 30 s: 68183 user stays; 20558 syscalls (bsd 16069, 4156677 us; mach 4450, 168124 us; platform 39, 577 us); 47054 faults, 27605 filled from the tables (77014 us), 19455 to vm_fault (2868024 us), 19456 refilled; 571 interrupts; twins +29 -23\n";
    t += "[   340.000] guest     oneios-perf:   pmap: user maps 47060, unmaps 15896, kernel maps 1699986, unmaps 13408; TLB lock taken 1880200, 188276 waited (4257163 us waiting, 14.1% of a vCPU), held 11354514 us (37.8%); copyio 20251 (261 faults)\n";
    t += "[   340.000] guest     oneios-perf:   bsd (number calls x mean): 75 4186 x63 5 3598 x142 us\n";
    t += "[   340.000] guest     oneios-perf:   mach traps: -47 18873 x88 -18 6393 x11 us\n";
    t += "[   341.000] xnuhost I perf: 30.0 s: 46936 task traps (1562/s): syscall 11002, fault 35575, interrupt 359, other 0; futex sleeps: vCPU 58193, iOS thread 16287 (of 46933 hand-offs each way); futex wakes 0 + 16288; task_map 35574 carried by a release, 3 ordered; timer_set 171011 (171012 to the host timer)\n";
    t += "[   341.000] xnuhost I perf:   up (trap->vCPU)    n    46933  mean   12.52  p50   14.34  p90   16.38  p99    49.15  max     949.1 us\n";
    t += "[   341.000] xnuhost I perf:   kernel: syscall    n    11001  mean  384.52  p50   40.96  p90  196.61  p99 10485.76  max  242913.6 us\n";
    t += "[   341.000] xnuhost I perf: cpu3: 19% of a core; of its wall time 28% kernel, 6% an iOS thread's turn, 66% idle; 754 traps/s\n";
    t
}

fn ints(row: &[Cell]) -> Vec<i64> {
    row.iter()
        .map(|c| match c {
            Cell::Int(v) => *v,
            Cell::Float(v, _) => (*v * 1000.0).round() as i64,
            Cell::None => -1,
            Cell::Text(_) => -2,
        })
        .collect()
}

#[test]
fn report_tables() {
    let log = parse::parse("r", (timeline_log() + &reports_log()).as_bytes());

    let (tables, notes) = reports::gpu(&log);
    assert_eq!(tables.len(), 4);
    let report = &tables[0];
    assert_eq!(report.columns[..6], ["time", "line", "s", "submits", "fenced", "frames"]);
    assert_eq!(ints(&report.rows[0]), [310_000, 31, 10, 11, 1, 1, 7328, 7328, 0, 1, 2, 3, 4, 5, 6, 7, 8]);
    // The cut report: what it still has; the appended memory record is read too.
    assert_eq!(ints(&report.rows[1]), [320_000, 34, 10, 5, 5, 2, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1]);
    assert_eq!(report.rows.len(), 2);
    assert_eq!(ints(&tables[1].rows[0]), [310_000, 32, 23, 1, 2, 3, 21, 19, 12, 2, 17, 2, 1]);
    assert_eq!(ints(&tables[1].rows[1]), [320_000, 34, 1, 0, 0, 0, 22, 20, 13, 2, 17, 0, 1]);
    assert_eq!(tables[2].rows.len(), 2);
    assert_eq!(tables[2].rows[1][2..], [Cell::text("SpringBoard"), Cell::Int(191), Cell::Int(4)]);
    assert_eq!(ints(&tables[3].rows[0]), [321_000, 36, 10, 74, 3859, 20242, 26, 783, 7, 7800, 183, 883, 40295, 427, 1, 28]);
    // The older host line has no udmabufs, clock hold or keep-alives.
    assert_eq!(ints(&tables[3].rows[1]), [322_000, 37, 75, 0, 0, 0, 0, 0, 0, 0, 2, 1145, 2236, -1, -1, -1]);
    assert_eq!(tables[3].rows.len(), 2);
    assert_eq!(
        notes,
        [
            "unparsed: 1 `oneios-gpu-report` line(s), the first at line 35",
            "cut: 1 `oneios-gpu-report` line(s) lost their end (another record was written into them), the first at line 34; their missing fields are `-`"
        ]
    );

    let (tables, notes) = reports::cpu(&log);
    assert!(notes.is_empty());
    assert_eq!(tables[0].rows[1][2..], [Cell::Int(10), Cell::Int(52), Cell::text("My App[41] 50%, launchd[1] 2%")]);
    // By process, the largest sum first: kernel_task 259, My App 50, launchd 18 + 2.
    assert_eq!(tables[1].rows[0][..4], [Cell::text("kernel_task"), Cell::Int(0), Cell::Int(1), Cell::Int(259)]);
    assert_eq!(tables[1].rows[1][..4], [Cell::text("My App"), Cell::Int(41), Cell::Int(1), Cell::Int(50)]);
    assert_eq!(tables[1].rows[2], [Cell::text("launchd"), Cell::Int(1), Cell::Int(2), Cell::Int(20), Cell::Float(10.0, 1), Cell::Float(10.0, 1), Cell::Int(18)]);

    let (tables, _) = reports::display(&log);
    assert_eq!(ints(&tables[0].rows[0]), [200_000, 17, 3, 2, 3, 3, 3, 0, 3, 3, 6, 1, 0, 2, 0, 0, 3, 0, 0, 0, 3]);
    assert_eq!(ints(&tables[1].rows[0]), [330_000, 39, 10, 66, 2315, 5034, 1131]);
    assert_eq!(tables[1].rows.len(), 1);
    assert_eq!(ints(&tables[2].rows[1]), [2, 21, 400_200, 39200, 1, 7]);
    assert_eq!(tables[2].rows.len(), 3);

    let (tables, notes) = reports::perf(&log);
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(ints(&tables[0].rows[0]), [340_000, 41, 30, 68183, 20558, 16069, 4156677, 4450, 168124, 39, 577, 47054, 27605, 77014, 19455, 2868024, 19456, 571, 29, 23]);
    assert_eq!(ints(&tables[1].rows[0]), [340_000, 42, 47060, 15896, 1699986, 13408, 1880200, 188276, 4257163, 14_100, 11354514, 37_800, 20251, 261]);
    assert_eq!(tables[2].rows.len(), 4);
    assert_eq!(tables[2].rows[3][2..], [Cell::text("mach"), Cell::Int(-18), Cell::Int(6393), Cell::Int(11)]);
    assert_eq!(ints(&tables[3].rows[0]), [341_000, 45, 30_000, 46936, 1562, 11002, 35575, 359, 0, 58193, 16287, 46933, 0, 16288, 35574, 3, 171011, 171012]);
    assert_eq!(tables[4].rows[1], [Cell::Float(341.0, 3), Cell::Int(47), Cell::text("kernel: syscall"), Cell::Int(11001), Cell::Float(384.52, 2), Cell::Float(40.96, 2), Cell::Float(196.61, 2), Cell::Float(10485.76, 2), Cell::Float(242913.6, 1)]);
    assert_eq!(ints(&tables[5].rows[0]), [341_000, 48, 3, 19, 28, 6, 66, 754]);

    // The three formats of one table.
    let (tables, notes) = reports::cpu(&log);
    let text = table::render(&tables[..1], &notes, Format::Text);
    assert_eq!(
        text,
        "== guest CPU (oneios-cpu: percent of one CPU per period) (2 rows)\n   \
         time  line   s  all%  top\n\
         300.000    29  10   277  kernel_task[0] 259%, launchd[1] 18%\n\
         310.000    30  10    52  My App[41] 50%, launchd[1] 2%\n"
    );
    let csv = table::render(&tables[..1], &notes, Format::Csv);
    assert!(csv.ends_with("time,line,s,all%,top\n300.000,29,10,277,\"kernel_task[0] 259%, launchd[1] 18%\"\n310.000,30,10,52,\"My App[41] 50%, launchd[1] 2%\"\n"), "{csv}");
    let json = table::render(&tables[..1], &notes, Format::Json);
    assert!(json.contains("{\"time\": 310.000, \"line\": 30, \"s\": 10, \"all%\": 52, \"top\": \"My App[41] 50%, launchd[1] 2%\"}"), "{json}");
    assert!(json.starts_with("{\"tables\": [\n{\"table\": \"guest CPU") && json.ends_with("], \"notes\": []}\n"), "{json}");
}

#[test]
fn malformed_lines_never_panic() {
    let f = Fixture::new("garbage");
    let mut text: Vec<u8> = Vec::new();
    let pieces: [&[u8]; 22] = [
        b"",
        b"[",
        b"[  12.3",
        b"[abc] guest     x",
        b"[  1.000]",
        b"[  1.000] guest",
        b"[  1.000] guest     ",
        b"[  1.000] xnuhost",
        b"[  1.000] xnuhost I",
        b"[  1.000] guest     oneios-ubt: ",
        b"[  1.000] guest     oneios-ubt: 3 thread(s) of ",
        b"[  1.000] guest     oneios-ubt: a[1] thread 0x state 0x1: 0x1",
        b"[  1.000] guest     oneios-ubt: a[1] thread 0x1 state 0x1: 0xfffffffffffffffffffff 0x1",
        b"[  1.000] guest     oneios-ubt: \xff\xfe[1] thread 0x1 state 0x1: 0x1 0x2oneios-ubt: ",
        b"[  1.000] guest     oneios-cpu: 10 s: [] %, a[b] 1% (all",
        b"[  1.000] guest     oneios-perf:   bsd (number calls x mean): 75 x 9 x",
        b"[  1.000] guest     oneios-display-report: W",
        b"[  1.000] guest     oneios-display-report: clients ",
        b"[  1.000] guest     sheng-ubt sheng-exc: #0 fp=0x1 ret=",
        b"[  1.000] guest     sheng-exc: [] exception",
        b"[  1.000] xnuhost I input: touch: frames in ms",
        b"[  1.000] xnuhost I perf:   up   n   mean  p50",
    ];
    for piece in pieces {
        text.extend_from_slice(piece);
        text.push(b'\n');
    }
    text.extend_from_slice(&vec![b'x'; 100_000]);
    text.extend_from_slice(b"\n[  2.000] guest     oneios-gpu-report: 10 s: submits +");
    let path = f.root.join("g.log");
    std::fs::write(&path, &text).unwrap();
    let path = path.display().to_string();
    for command in ["stacks", "timeline", "gpu", "cpu", "display", "perf"] {
        let out = f.run(&[command, &path]);
        assert!(!out.is_empty(), "{command}");
        for format in ["--json", "--csv"] {
            if !matches!(command, "stacks" | "timeline") {
                f.run(&[command, &path, format]);
            }
        }
    }
    f.run(&["diff", &path, &path]);
    let empty = f.log("empty.log", "");
    for command in ["stacks", "timeline", "gpu", "cpu", "display", "perf"] {
        f.run(&[command, &empty]);
    }
    assert!(timeline::report(&parse::parse("e", b"")).contains("0 lines"));
    // Errors, not panics.
    let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(run_inner(&args(&["stacks", "/nonexistent/x.log"]), f.loc(), &mut Vec::new()).is_err());
    assert!(run_inner(&args(&["diff", &path]), f.loc(), &mut Vec::new()).is_err());
    assert!(run_inner(&args(&["what", &path]), f.loc(), &mut Vec::new()).is_err());
    assert!(run_inner(&args(&["gpu", &path, "--nope"]), f.loc(), &mut Vec::new()).is_err());
}
