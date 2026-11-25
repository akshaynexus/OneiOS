// SPDX-License-Identifier: MIT
//! Port of tests/test_display.c's displayctl part: displayctl against a stand-in helper (a shell script that takes the
//! socket path and leaves, then this program speaking the protocol). No harness: displayctl runs this binary as its
//! helper (`--socket PATH --wait N ...`), and main tells the two apart.
use oneios_display::pixels::half_to_float;
use oneios_display::sys;
use oneios_display::xhd::{self, Kind, Message, Rect, FORMAT_BGRX, FORMAT_RGBA16F};
use std::fs;
use std::os::fd::AsFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

struct Checks {
    checks: u32,
    failed: u32,
}

impl Checks {
    fn check(&mut self, ok: bool, what: &str) {
        self.checks += 1;
        if !ok {
            self.failed += 1;
            println!("FAIL {what}");
        }
    }
}

/// displayctl with TMPDIR=dir and these arguments, its output and the helper's in dir/out: the exit code and the
/// output.
fn displayctl(dir: &Path, args: &[&str]) -> (Option<i32>, String) {
    let out_path = dir.join("out");
    let out = fs::File::create(&out_path).unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_displayctl"))
        .env("TMPDIR", dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(out.try_clone().unwrap())
        .stderr(out)
        .status()
        .unwrap();
    let text = fs::read_to_string(&out_path).unwrap_or_default();
    let _ = fs::remove_file(&out_path);
    (status.code(), text)
}

/// No displayctl.* folder is left in dir.
fn no_session_left(dir: &Path) -> bool {
    fs::read_dir(dir).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with("displayctl."))
}

/// The number after `prefix` at the start of a line that ends in `suffix`.
fn number_in(text: &str, prefix: &str, suffix: &str) -> Option<u64> {
    text.lines().filter_map(|l| l.strip_prefix(prefix)).find_map(|rest| {
        let end = rest.find(|c: char| !c.is_ascii_digit())?;
        rest[end..].starts_with(suffix).then(|| rest[..end].parse().ok())?
    })
}

/// One initial full-frame hello flush + exactly one union rectangle per animation update. This catches the S26
/// flicker-causing double flush.
fn one_flush_a_step(text: &str) -> bool {
    let steps = number_in(text, "displayctl: ", " frames in ");
    let packets = text
        .lines()
        .find_map(|l| l.strip_prefix("test-helper: ")?.strip_suffix(" flush messages")?.parse::<u64>().ok());
    matches!((steps, packets), (Some(s), Some(p)) if s > 0 && p == s + 1)
}

fn test_displayctl(c: &mut Checks, dir: &Path, me: &str) {
    let helper = dir.join("helper.sh");
    fs::write(&helper, format!("#!/bin/sh\necho \"$@\" > {}/helper.args\nexit 7\n", dir.display())).unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
    let helper_s = helper.to_str().unwrap();
    let (code, out) =
        displayctl(dir, &["test", "--seconds", "1", "--size", "64x48", "--helper", helper_s, "--", "--hz", "60"]);
    c.check(code == Some(1), "a helper that never connects is a failure");
    let args = fs::read_to_string(dir.join("helper.args")).unwrap_or_default();
    let prefix = format!("--socket {}/displayctl.", dir.display());
    c.check(
        args.starts_with(&prefix) && args.trim_end().ends_with("/display.sock --wait 5 --hz 60"),
        "direct diagnostic socket; never an XNU session",
    );
    c.check(out.contains("ended before it connected") && no_session_left(dir), "and its folder is gone");
    let _ = fs::remove_file(&helper);
    let _ = fs::remove_file(dir.join("helper.args"));
    // With a helper that speaks the protocol (this program, see main): the whole exchange passes.
    let (code, out) = displayctl(dir, &["test", "--seconds", "1", "--size", "64x48", "--helper", me]);
    c.check(code == Some(0), "the whole exchange passes");
    c.check(
        out.contains("removed its layer and exited 0") && out.lines().any(|l| l.starts_with("displayctl: ok")),
        "the helper left, displayctl ok",
    );
    c.check(one_flush_a_step(&out), "one flush a step (8-bit)");
    // The half-float pattern: the stand-in checks the stride, the white edge and the wide band's P3 red.
    let (code, out) =
        displayctl(dir, &["test", "--seconds", "1", "--size", "64x48", "--format", "rgba16f", "--helper", me]);
    c.check(code == Some(0), "the half-float exchange passes");
    c.check(
        out.contains("pattern in R,G,B,A half float") && out.lines().any(|l| l.starts_with("displayctl: ok")),
        "the half-float pattern",
    );
    c.check(one_flush_a_step(&out), "one flush a step (half float)");
    // A live One UI diagnostic must reject destructive or endpoint-overriding helper flags before spawning a helper or
    // creating a test session.
    for denied in ["--takeover", "--session", "--socket", "--lib", "--wait", "--logical"] {
        let (code, out) =
            displayctl(dir, &["test", "--seconds", "1", "--size", "64x48", "--helper", me, "--", denied, "ignored"]);
        c.check(code == Some(2), &format!("{denied}: usage exit"));
        c.check(out.contains(&format!("refusing {denied}")) && no_session_left(dir), &format!("{denied} refused"));
    }
    let (code, _) = displayctl(dir, &["test", "--format", "rgb565"]);
    c.check(code == Some(2), "a format it does not make: usage");
    let (code, out) = displayctl(dir, &["drm", "/no/such/card0"]);
    c.check(code == Some(1) && out.contains("displayctl drm:"), "drm missing node reports error");
    let (code, out) = displayctl(dir, &["drm", "a", "b"]);
    c.check(code == Some(2) && out.contains("usage:"), "drm extra args rejected");
}

/// This program as the helper displayctl starts (--socket PATH --wait N): connects, takes the hello, reads flushes
/// until the socket closes, and checks it saw the moving square's damage. The exit code says what was wrong.
fn as_helper(socket: &str) -> u8 {
    // Diagnostics must be free of the real guest's session/ready requirement.
    if xhd::hold_first(None) {
        return 9;
    }
    let Ok(fd) = xhd::connect(socket.as_ref(), 5000, &xhd::no_stop) else { return 9 };
    let Ok(frame) = xhd::recv_hello(fd.as_fd(), 5000) else { return 9 };
    let (mut full, mut partial, mut flushes) = (0, 0, 0);
    for _ in 0..2000 {
        // Count wire packets, not helper presentations: a double-flush per animation step would pass the image tests
        // yet flicker on the S26.
        let mut p = [0u8; Message::BYTES];
        let n = match sys::recv_packet(fd.as_fd(), &mut p, libc::MSG_DONTWAIT) {
            Err(sys::Errno(libc::EINTR)) => continue,
            Err(sys::Errno(e)) if e == libc::EAGAIN || e == libc::EWOULDBLOCK => {
                sys::nap_ms(5);
                continue;
            }
            Err(_) => return 10,
            Ok(got) => got.length,
        };
        if n > 0 {
            let Some(m) = Message::parse(&p[..n]).filter(|m| m.is(Kind::Flush)) else { return 11 };
            let mut r = Rect::new(m.a, m.b, m.c, m.d);
            if !r.clip(frame.width, frame.height) {
                return 12;
            }
            flushes += 1;
            if r.w == frame.width && r.h == frame.height {
                full += 1;
            } else {
                partial += 1;
            }
            continue;
        }
        // Peer closed: one initial full flush plus ONE union flush per completed animation step. These counts must
        // agree with displayctl.
        println!("test-helper: {flushes} flush messages");
        let px = frame.pixels();
        let moved = full > 0 && partial > 0;
        if frame.format == FORMAT_BGRX {
            return if moved && px[3] == 255 { 0 } else { 8 };
        }
        // half floats: the white edge at 1.0, and the wide band's P3 red (second eighth, lowest quarter) above sRGB's
        // 1.0 (the bouncing square may cover part of the band: any pixel of the row will do)
        let half = |at: usize| u16::from_le_bytes([px[at], px[at + 1]]);
        let row = (frame.height as usize - 4) * frame.stride as usize;
        let wide = (frame.width / 8..frame.width / 4).any(|x| {
            let at = row + x as usize * 8;
            half_to_float(half(at)) > 1.2 && half_to_float(half(at + 2)) < 0.0
        });
        let ok = frame.format == FORMAT_RGBA16F
            && frame.stride == frame.width * 8
            && half(0) == 0x3c00
            && half(6) == 0x3c00
            && wide;
        return if moved && ok { 0 } else { 6 };
    }
    7
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 2 && args[1] == "--socket" {
        return ExitCode::from(as_helper(&args[2]));
    }
    // libtest's own options (cargo test passes some, e.g. --list for discovery): nothing to list.
    if args.iter().any(|a| a == "--list") {
        return ExitCode::SUCCESS;
    }
    let me = std::env::current_exe().unwrap();
    let dir: PathBuf = std::env::temp_dir().join(format!("oneios-display-test.{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let mut c = Checks { checks: 0, failed: 0 };
    test_displayctl(&mut c, &dir, me.to_str().unwrap());
    let _ = fs::remove_dir_all(&dir);
    println!("{}: {} checks, {} failed", if c.failed > 0 { "FAILED" } else { "ok" }, c.checks, c.failed);
    ExitCode::from((c.failed > 0) as u8)
}
