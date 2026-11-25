// SPDX-License-Identifier: MIT
//! Port of displayctl.c. displayctl: the on-phone test of the display helper. It stands in for xnuhost: a frame in a
//! memfd, served on a display.sock of its own, with a moving pattern; oneios-display shows it on a top layer. With One
//! UI running (README.md):
//!   displayctl test [--seconds N] [--size WxH] [--helper PATH] [-- helper options]
//! The layer goes away when the socket closes; the helper is ended if it does not leave by itself.
use oneios_display::pixels::{self, half_from_float, srgb8_to_half};
use oneios_display::sys::{self, Errno, Map};
use oneios_display::xhd::{self, Rect, FORMAT_BGRX, FORMAT_RGBA16F};
use std::ffi::CString;
use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}

fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::Relaxed)
}

fn now() -> f64 {
    sys::monotonic_ns() as f64 / 1e9
}

/// The wide band (--format rgba16f): the lowest quarter of the frame, eight patches from the left: sRGB's red, Display
/// P3's red, sRGB's green, P3's green, sRGB's blue, P3's blue (as linear extended sRGB: components below 0 and above
/// 1), white at 1.0 and white at 2.0 (EDR). On a path that keeps wide colour each P3 patch is more saturated than the
/// sRGB one beside it; on one that clips to sRGB the two are the same. The 2.0 white is brighter than the 1.0 one only
/// where surfaceflinger gives the layer HDR headroom.
const WIDE: [[f32; 3]; 8] = [
    [1.0, 0.0, 0.0],
    [1.2249, -0.0420, -0.0197],
    [0.0, 1.0, 0.0],
    [-0.2247, 1.0419, -0.0786],
    [0.0, 0.0, 1.0],
    [0.0, 0.0, 1.0979],
    [1.0, 1.0, 1.0],
    [2.0, 2.0, 2.0],
];

/// Eight colour bars (B,G,R).
const BARS: [[u8; 3]; 8] =
    [[255, 255, 255], [0, 255, 255], [255, 255, 0], [0, 255, 0], [255, 0, 255], [0, 0, 255], [255, 0, 0], [40, 40, 40]];

/// The guest's frame: W x H in `format`, in a memfd's map.
struct Pattern {
    w: u32,
    h: u32,
    format: u32,
    bpp: usize,
    map: Map,
}

fn put_halves(p: &mut [u8], v: [u16; 4]) {
    for (i, h) in v.into_iter().enumerate() {
        p[i * 2..i * 2 + 2].copy_from_slice(&h.to_le_bytes());
    }
}

impl Pattern {
    /// One pixel from 8-bit sRGB bytes (b, g, r) in the frame's format.
    fn put8(format: u32, p: &mut [u8], b: u8, g: u8, r: u8) {
        if format == FORMAT_BGRX {
            p[..4].copy_from_slice(&[b, g, r, 255]);
        } else {
            let t = srgb8_to_half();
            put_halves(p, [t[r as usize], t[g as usize], t[b as usize], 0x3c00]);
        }
    }

    /// Eight colour bars with a white frame two pixels wide (so a cut-off edge shows).
    fn background(&mut self, x: u32, y: u32, w: u32, h: u32) {
        let (width, height, format, bpp) = (self.w, self.h, self.format, self.bpp);
        let pixels = self.map.bytes_mut();
        for row in y..(y + h).min(height) {
            for col in x..(x + w).min(width) {
                let p = &mut pixels[(row as usize * width as usize + col as usize) * bpp..][..bpp];
                let edge = col < 2 || row < 2 || col >= width - 2 || row >= height - 2;
                if !edge && format == FORMAT_RGBA16F && row >= height - height / 4 {
                    let c = WIDE[(col as u64 * 8 / width as u64) as usize];
                    put_halves(p, [half_from_float(c[0]), half_from_float(c[1]), half_from_float(c[2]), 0x3c00]);
                    continue;
                }
                let c = BARS[(col as u64 * 8 / width as u64) as usize];
                let shade = (128 + row as u64 * 127 / height as u64) as u32; // darker at the top
                if edge {
                    Pattern::put8(format, p, 255, 255, 255);
                } else {
                    let s = |v: u8| (v as u32 * shade / 255) as u8;
                    Pattern::put8(format, p, s(c[0]), s(c[1]), s(c[2]));
                }
            }
        }
    }

    fn square(&mut self, x: u32, y: u32, side: u32) {
        let (width, height, format, bpp) = (self.w, self.h, self.format, self.bpp);
        let pixels = self.map.bytes_mut();
        for row in y..(y + side).min(height) {
            for col in x..(x + side).min(width) {
                Pattern::put8(
                    format,
                    &mut pixels[(row as usize * width as usize + col as usize) * bpp..],
                    255,
                    255,
                    255,
                );
            }
        }
    }
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: displayctl test [--seconds N] [--size WxH] [--format bgrx|rgba16f] [--helper PATH] [-- helper options]\n       displayctl drm [PATH]"
    );
    ExitCode::from(2)
}

/// atof: the longest leading number (0 when there is none).
fn atof(s: &str) -> f64 {
    let s = s.trim_start();
    (1..=s.len()).rev().filter(|&n| s.is_char_boundary(n)).find_map(|n| s[..n].parse::<f64>().ok()).unwrap_or(0.0)
}

/// The leading decimal digits of s as a number, and the rest (sscanf's %u, for "WxH").
fn leading_u32(s: &str) -> Option<(u32, &str)> {
    let s = s.trim_start();
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    Some((s[..end].parse::<u64>().ok()? as u32, &s[end..]))
}

fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, rest) = leading_u32(s)?;
    let (h, _) = leading_u32(rest.strip_prefix('x')?)?;
    Some((w, h))
}

struct Options {
    seconds: f64,
    w: u32,
    h: u32,
    format: u32,
    helper: Option<String>,
    extra: Vec<String>,
}

fn parse(args: &[String]) -> Result<Options, ExitCode> {
    if args.len() < 2 || args[1] != "test" {
        return Err(usage());
    }
    let mut o = Options { seconds: 5.0, w: 1440, h: 3120, format: FORMAT_BGRX, helper: None, extra: Vec::new() };
    let mut i = 2;
    while i < args.len() {
        if args[i] == "--" {
            o.extra = args[i + 1..].to_vec();
            break;
        }
        let Some(value) = args.get(i + 1) else { return Err(usage()) };
        match args[i].as_str() {
            "--seconds" => o.seconds = atof(value),
            "--size" => (o.w, o.h) = parse_size(value).ok_or_else(usage)?,
            "--helper" => o.helper = Some(value.clone()),
            "--format" => {
                o.format = match value.as_str() {
                    "bgrx" => FORMAT_BGRX,
                    "rgba16f" => FORMAT_RGBA16F,
                    _ => return Err(usage()),
                }
            }
            _ => return Err(usage()),
        }
        i += 2;
    }
    if o.w < 16 || o.h < 16 || o.w > 8192 || o.h > 8192 || o.seconds <= 0.0 || o.seconds > 3600.0 {
        return Err(usage());
    }
    // Only non-disruptive display options may reach the helper. In particular --takeover changes panel ownership and
    // --session/--socket override the diagnostic endpoint (and can re-enable the production ready-file hold).
    let mut k = 0;
    while k < o.extra.len() {
        let opt = o.extra[k].as_str();
        match opt {
            "--no-projection" => {}
            "--hz" | "--format" | "--dataspace" => {
                k += 1;
                if k >= o.extra.len() {
                    return Err(usage());
                }
            }
            _ => {
                eprintln!(
                    "displayctl: refusing {opt}: test mode only accepts --hz, --format, --dataspace or --no-projection"
                );
                return Err(ExitCode::from(2));
            }
        }
        k += 1;
    }
    Ok(o)
}

/// mkdtemp of `template` (ending in XXXXXX).
fn mkdtemp(template: &str) -> Result<PathBuf, Errno> {
    let mut bytes = CString::new(template).map_err(|_| Errno(libc::EINVAL))?.into_bytes_with_nul();
    // SAFETY: bytes is a writable NUL-terminated template; mkdtemp fills in its X's.
    if unsafe { libc::mkdtemp(bytes.as_mut_ptr() as *mut libc::c_char) }.is_null() {
        return Err(Errno::last());
    }
    bytes.pop();
    Ok(PathBuf::from(String::from_utf8_lossy(&bytes).into_owned()))
}

fn run_drm(args: &[String]) -> ExitCode {
    if args.len() > 3 {
        return usage();
    }
    let path = args.get(2).map(String::as_str).unwrap_or("/dev/dri/card0");
    match oneios_display::drm_probe::probe(path) {
        Ok(report) => {
            print!("{report}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("displayctl drm: {e}");
            ExitCode::from(1)
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("drm") {
        return run_drm(&args);
    }
    let o = match parse(&args) {
        Ok(o) => o,
        Err(code) => return code,
    };
    let helper = match &o.helper {
        Some(h) => h.clone(),
        None => match sys::self_exe() {
            Ok(me) => format!("{}/oneios-display", me.parent().unwrap_or(Path::new(".")).display()),
            Err(e) => {
                eprintln!("displayctl: /proc/self/exe: {}", Errno::from(e));
                return ExitCode::from(2);
            }
        },
    };
    if let Err(e) = sys::access(helper.as_ref(), libc::X_OK) {
        eprintln!("displayctl: {helper}: {e}");
        return ExitCode::from(2);
    }
    let tmp = std::env::var("TMPDIR").ok().filter(|t| !t.is_empty()).unwrap_or_else(|| "/data/local/tmp".into());
    let template = format!("{tmp}/displayctl.XXXXXX");
    let session = match mkdtemp(&template) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("displayctl: {template}: {e}");
            return ExitCode::from(2);
        }
    };
    let path = session.join("display.sock");
    let failed = run(&o, &helper, &path);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&session);
    println!("displayctl: {}", if failed { "FAILED" } else { "ok" });
    ExitCode::from(failed as u8)
}

/// The session: the frame, the socket, the helper, the bouncing square. true when it failed.
fn run(o: &Options, helper: &str, path: &Path) -> bool {
    let bpp = pixels::bpp(o.format) as usize;
    let bytes = o.w as usize * o.h as usize * bpp;
    let made = xhd::memfd("displayctl-frame", bytes).and_then(|memfd| {
        let server = xhd::listen(path)?;
        let map = Map::new(memfd.as_fd(), bytes, true)?;
        Ok((memfd, server, map))
    });
    let (memfd, server, map) = match made {
        Ok(m) => m,
        Err(e) => {
            eprintln!("displayctl: socket or frame: {e}");
            return true;
        }
    };
    let mut pattern = Pattern { w: o.w, h: o.h, format: o.format, bpp, map };
    pattern.background(0, 0, o.w, o.h);
    // SAFETY: on_signal only stores to an atomic, which is async-signal-safe.
    unsafe {
        libc::signal(libc::SIGINT, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
    }
    // A test pattern is already ready; only a real guest session needs session/ready. --socket tells OneiOSDisplay to
    // post the first frame immediately instead of waiting for an XNU-created marker forever.
    let spawned = Command::new(helper).arg("--socket").arg(path).args(["--wait", "5"]).args(&o.extra).spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            eprintln!("displayctl: {helper}: {}", Errno::from(e));
            return true;
        }
    };
    let failed = serve(o, &mut pattern, &memfd, &server, &mut child);
    drop(server); // the session ends: the helper removes its layer and exits
    match failed {
        Served::HelperGone => true,
        Served::Done(failed) => reap(&mut child) || failed,
    }
}

enum Served {
    /// The helper ended before it connected (already reaped).
    HelperGone,
    /// The socket was served and closed; true when that failed.
    Done(bool),
}

fn serve(o: &Options, pattern: &mut Pattern, memfd: &OwnedFd, server: &OwnedFd, child: &mut Child) -> Served {
    // The helper is a Java runtime starting up: a second or two.
    let deadline = now() + 20.0;
    let mut client = None;
    while client.is_none() && now() < deadline && !interrupted() {
        if let Ok(Some(status)) = child.try_wait() {
            eprintln!("displayctl: the helper ended before it connected (status {})", status.into_raw());
            return Served::HelperGone;
        }
        if matches!(sys::poll(&[(server.as_fd(), libc::POLLIN)], 200, false), Ok((n, _)) if n > 0) {
            client = sys::accept(server.as_fd()).ok();
        }
    }
    let Some(client) = client else {
        eprintln!("displayctl: the helper did not connect");
        return Served::Done(true);
    };
    let fd = client.as_fd();
    let (w, h) = (o.w, o.h);
    let sent = xhd::send_hello(fd, w, h, w * pattern.bpp as u32, o.format, memfd.as_fd())
        .and_then(|_| xhd::send_flush(fd, 0, 0, w, h));
    if let Err(e) = sent {
        eprintln!("displayctl: hello: {e}");
        return Served::Done(true);
    }
    println!(
        "displayctl: helper {} connected; {w}x{h} pattern in {} for {:.1} s",
        child.id(),
        pixels::name(o.format),
        o.seconds
    );
    let _ = std::io::stdout().flush();

    // A white square that bounces; each step flushes where it was and where it is.
    let side = w / 5;
    let (mut x, mut y) = (2u32, 2u32);
    let (mut old_x, mut old_y) = (x, y);
    let mut dx = (w / 90) as i32 + 1;
    let mut dy = (h / 150) as i32 + 1;
    let mut frames = 0u64;
    let start = now();
    let mut gone = false;
    while now() - start < o.seconds && !interrupted() {
        pattern.background(old_x, old_y, side, side);
        if (x as i32 + dx) < 2 || x + side + dx.max(0) as u32 > w - 2 {
            dx = -dx;
        }
        if (y as i32 + dy) < 2 || y + side + dy.max(0) as u32 > h - 2 {
            dy = -dy;
        }
        x = (x as i32 + dx) as u32;
        y = (y as i32 + dy) as u32;
        pattern.square(x, y, side);
        // Present one *complete* animation step. Two separate flush packets allow the helper to post after only the
        // old square's damage is copied, leaving the new square's moving edge stale for an intermediate visible
        // buffer. The union covers both the erased old position and the newly drawn one.
        let mut changed = Rect::new(old_x, old_y, side, side);
        changed.union(&Rect::new(x, y, side, side));
        if let Err(e) = xhd::send_flush(fd, changed.x, changed.y, changed.w, changed.h) {
            eprintln!("displayctl: frame flush failed: {e}");
            gone = true;
            break;
        }
        old_x = x;
        old_y = y;
        frames += 1;
        std::thread::sleep(std::time::Duration::from_nanos(1_000_000_000 / 120));
    }
    println!(
        "displayctl: {frames} frames in {:.2} s{}",
        now() - start,
        if gone { " (the helper closed the socket early)" } else { "" }
    );
    Served::Done(gone)
}

/// Waits up to 3 s for the helper to leave after the socket closed, then ends it. true when it did not exit 0.
fn reap(child: &mut Child) -> bool {
    let mut code = None;
    for _ in 0..30 {
        if let Ok(Some(status)) = child.try_wait() {
            code = Some(status);
            break;
        }
        sys::nap_ms(100);
    }
    let Some(status) = code else {
        eprintln!("displayctl: the helper did not leave within 3 s of the socket closing; ending it");
        // SAFETY: kill takes no pointers; the child is ours and not reaped yet.
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
        sys::nap_ms(1000);
        if !matches!(child.try_wait(), Ok(Some(_))) {
            let _ = child.kill();
            let _ = child.wait();
        }
        return true;
    };
    if status.code() != Some(0) {
        eprintln!("displayctl: the helper ended with status {:#x}", status.into_raw());
        return true;
    }
    println!("displayctl: the helper removed its layer and exited 0");
    false
}
