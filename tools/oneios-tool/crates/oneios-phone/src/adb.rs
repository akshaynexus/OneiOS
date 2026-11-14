//! The one phone on adb, and the rules every phone command keeps (docs/building.md section 6):
//! - one phone only: `--serial`, `ANDROID_SERIAL`, or the only one attached; several attached is refused;
//! - root is checked first and its absence said plainly (KernelSU's root does not survive a restart);
//! - on the phone only named files are read: no command here walks a tree (no `find`, `ls -R`, `du`; a walk of
//!   sysfs or /proc panicked the phone's kernel on 2026-11-12);
//! - a command that changes the stage refuses while a session is open (but `display-restart --lib/--dex`, which
//!   replaces the display helper's two files, read by the helper on One UI's side only: `session.rs`);
//! - the session's own commands (`display-restart`, `touch`, `backlight`: `session.rs`) refuse without a session:
//!   what they touch is One UI's then;
//! - nothing here restarts the phone or writes under /sys/power.
//!
//! `ADB` names the adb program (the tests' stand-in phone, `tests/phone/fake-adb.sh`).

use anyhow::{anyhow, bail, Context, Result};
use std::io::Write;
use std::process::{Child, Command, Stdio};

pub const STAGE: &str = "/data/local/tmp/oneios";
/// KernelSU's busybox: `gzip`, `setsid`, `nsenter`.
pub const BUSYBOX: &str = "/data/adb/ksu/bin/busybox";

pub struct Adb {
    program: String,
    pub serial: String,
}

/// What a command on the phone gave: its exit status and its output without carriage returns.
pub struct Ran {
    pub status: i32,
    pub out: String,
    pub err: String,
}

impl Ran {
    pub fn ok(&self) -> bool {
        self.status == 0
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace('\r', "")
}

impl Adb {
    /// The phone to work on. `serial`: the `--serial` option.
    pub fn connect(serial: Option<&str>) -> Result<Adb> {
        let program = std::env::var("ADB").ok().filter(|p| !p.is_empty()).unwrap_or_else(|| "adb".into());
        let listed = Command::new(&program)
            .arg("devices")
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("cannot start {program} (install Android's platform-tools, or name it with ADB=)"))?;
        if !listed.status.success() {
            bail!("{program} devices failed: {}", text(&listed.stderr).trim());
        }
        let phones: Vec<(String, String)> = text(&listed.stdout)
            .lines()
            .skip(1)
            .filter_map(|line| {
                let mut words = line.split_whitespace();
                Some((words.next()?.to_string(), words.next()?.to_string()))
            })
            .collect();
        let wanted = serial.map(str::to_string).or_else(|| std::env::var("ANDROID_SERIAL").ok().filter(|s| !s.is_empty()));
        let (name, state) = match wanted {
            Some(want) => phones
                .iter()
                .find(|(name, _)| *name == want)
                .cloned()
                .ok_or_else(|| anyhow!("the phone {want} is not on adb (adb devices lists: {})", names(&phones)))?,
            None => match phones.len() {
                0 => bail!("no phone on adb (adb devices lists none)"),
                1 => phones[0].clone(),
                n => bail!("{n} phones are on adb ({}): name one with --serial or ANDROID_SERIAL", names(&phones)),
            },
        };
        if state != "device" {
            bail!("the phone {name} is \"{state}\" on adb, not ready (unlock it and allow USB debugging)");
        }
        Ok(Adb { program, serial: name })
    }

    fn adb(&self) -> Command {
        let mut c = Command::new(&self.program);
        c.arg("-s").arg(&self.serial);
        c
    }

    fn ran(&self, mut c: Command, stdin: Option<&str>) -> Result<Ran> {
        c.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().with_context(|| format!("cannot start {}", self.program))?;
        if let Some(script) = stdin {
            // the script is small: it fits the pipe before the phone's shell reads it
            child.stdin.take().expect("piped").write_all(script.as_bytes()).context("writing to adb")?;
        }
        let done = child.wait_with_output().context("waiting for adb")?;
        Ok(Ran { status: done.status.code().unwrap_or(-1), out: text(&done.stdout), err: text(&done.stderr) })
    }

    /// `adb shell CMD`, as the shell user.
    pub fn shell(&self, cmd: &str) -> Result<Ran> {
        let mut c = self.adb();
        c.arg("shell").arg(cmd);
        self.ran(c, None)
    }

    /// A script run by root's shell (`su -c 'sh -s'`, the script on stdin: scripts/phone/stage.sh's `root`).
    pub fn root(&self, script: &str) -> Result<Ran> {
        let mut c = self.adb();
        c.arg("shell").arg("su -c 'sh -s'");
        self.ran(c, Some(script))
    }

    /// The same, and a status that is not 0 is an error that carries what the phone said.
    pub fn root_ok(&self, script: &str, what: &str) -> Result<String> {
        let r = self.root(script)?;
        if !r.ok() {
            bail!("{what} failed on the phone (status {}): {}", r.status, last_lines(&format!("{}{}", r.out, r.err), 5));
        }
        Ok(r.out)
    }

    /// `adb exec-out CMD`: the command's raw output as a stream.
    pub fn exec_out(&self, cmd: &str) -> Result<Child> {
        let mut c = self.adb();
        c.arg("exec-out").arg(cmd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        c.spawn().with_context(|| format!("cannot start {}", self.program))
    }

    /// `adb push LOCAL REMOTE`.
    pub fn push(&self, local: &str, remote: &str) -> Result<()> {
        let mut c = self.adb();
        c.arg("push").arg(local).arg(remote);
        let r = self.ran(c, None)?;
        if !r.ok() {
            bail!("adb push {local} failed: {}", last_lines(&format!("{}{}", r.out, r.err), 3));
        }
        Ok(())
    }

    /// `adb install -r APK`: over the installed app, keeping its data. Never an uninstall.
    pub fn install(&self, apk: &str) -> Result<Ran> {
        let mut c = self.adb();
        c.arg("install").arg("-r").arg(apk);
        self.ran(c, None)
    }

    /// Root first: every command that needs it says plainly when it is missing.
    pub fn need_root(&self) -> Result<()> {
        let r = self.shell("su -c 'id -u'")?;
        if r.ok() && r.out.trim() == "0" {
            return Ok(());
        }
        let said = last_lines(&format!("{}{}", r.out, r.err), 2);
        bail!(
            "no root on the phone {}: `su -c id -u` gave {} (status {}). KernelSU's root does not survive a restart of \
             the phone: root it again and allow the shell in the manager, then repeat. Nothing was changed",
            self.serial,
            if said.is_empty() { "nothing".to_string() } else { format!("\"{said}\"") },
            r.status
        )
    }

    /// Is a session open: session/xnuhost.pid names a live process that is still an xnuhost (a pid is used again
    /// after the session ends). One named file and one /proc entry are read.
    pub fn session_open(&self) -> Result<bool> {
        match self.root_ok(&session_check_script(), "the session check")?.trim() {
            "open" => Ok(true),
            "closed" => Ok(false),
            other => bail!("the session check gave \"{other}\""),
        }
    }

    /// For every command but status, return and log.
    pub fn refuse_in_session(&self) -> Result<()> {
        if self.session_open()? {
            bail!("a session is open: return to One UI first (oneios-tool phone return)");
        }
        Ok(())
    }
}

/// The session check's script for root's shell (`Adb::session_open`). The stand-in phone answers it with its own
/// /proc on a host without one (tests/phone/fake-rewrite.sh): the script is the phone's on every host.
pub fn session_check_script() -> String {
    format!(
        "p=$(cat {STAGE}/session/xnuhost.pid 2>/dev/null); [ -n \"$p\" ] && cat /proc/$p/cmdline 2>/dev/null | tr '\\0' ' ' | grep -q xnuhost && echo open || echo closed\n"
    )
}

fn names(phones: &[(String, String)]) -> String {
    if phones.is_empty() {
        return "none".into();
    }
    phones.iter().map(|(name, state)| format!("{name} {state}")).collect::<Vec<_>>().join(", ")
}

/// The last `n` lines that are not empty, joined with " | ".
pub fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join(" | ")
}

/// A word that goes into a script for root's shell: only what cannot be read as shell syntax.
pub fn plain(word: &str, what: &str) -> Result<()> {
    let ok = !word.is_empty() && word.bytes().all(|b| b.is_ascii_alphanumeric() || b"._+-/,".contains(&b)) && !word.starts_with('-') && !word.split('/').any(|part| part == ".." || part.is_empty());
    if !ok {
        bail!("{what} \"{word}\" has characters this tool does not put into a root shell (letters, digits and ._+-/ only)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the phone runs for the session check, byte for byte: the stand-in phone maps /proc on a host without
    /// one (macOS); the tool must not send anything else to make the stand-in pass.
    #[test]
    fn session_check_is_the_phones_command() {
        assert_eq!(
            session_check_script(),
            r#"p=$(cat /data/local/tmp/oneios/session/xnuhost.pid 2>/dev/null); [ -n "$p" ] && cat /proc/$p/cmdline 2>/dev/null | tr '\0' ' ' | grep -q xnuhost && echo open || echo closed"#.to_string() + "\n"
        );
    }
}
