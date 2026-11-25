//! `oneios-tool phone qrtr [--sensors] [--binary FILE] [--save FILE]`: whether a root process beside One UI reaches the
//! sensor hub (SSC) over Linux's QRTR, read only (runtime/bridges/sensors/README.md, docs/hardware.md "Sensors"). It
//! runs the runtime's `sensorctl qrtr` as root: the socket AF_QIPCRTR, the local node, every service the name server
//! lists, and whether the SNS client service (400) is there and answers a lookup; with `--sensors` each data type's
//! SUIDs and attributes (lookups only: no stream starts). That one answer settles whether `sensors=auto` takes the hub
//! or the old path on this phone.
//!
//! `sensorctl` comes from the staged runtime (`STAGE/runtime/bin/sensorctl`), or with `--binary FILE` from this
//! machine: pushed to /data/local/tmp/oneios-sensorctl, run, and removed again whatever happened. Nothing else is
//! written on the phone; the tool's sockets close when it exits.
use crate::adb::{Adb, STAGE};
use anyhow::{bail, Context, Result};

/// The staged runtime's tool (runtime/build.sh puts it there).
pub fn sensorctl() -> String {
    format!("{STAGE}/runtime/bin/sensorctl")
}

/// Where `--binary` puts its copy for the run (removed after it).
pub const PUSHED: &str = "/data/local/tmp/oneios-sensorctl";

/// The root script: `sensorctl qrtr` from `path`; a pushed copy is made executable first.
pub fn script(path: &str, sensors: bool, pushed: bool) -> String {
    format!(
        "{}{path} qrtr{}\n",
        if pushed { format!("chmod 0755 {path} && ") } else { String::new() },
        if sensors { " --sensors" } else { "" }
    )
}

/// The verdict lines of sensorctl's report: the socket's and the SSC's.
pub fn verdict(out: &str) -> Vec<&str> {
    out.lines().filter(|l| l.starts_with("qrtr: SSC:") || l.starts_with("qrtr: socket(")).collect()
}

/// sensorctl's last line: "ok" or "failed".
pub fn outcome(out: &str) -> &str {
    out.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")
}

/// `phone qrtr`.
pub fn qrtr(adb: &Adb, sensors: bool, binary: Option<&str>, save: &Option<String>) -> Result<()> {
    adb.need_root()?;
    let (path, pushed) = match binary {
        Some(file) => {
            if !std::path::Path::new(file).is_file() {
                bail!("--binary {file}: no such file here (runtime/build.sh STAGE makes STAGE/bin/sensorctl). Nothing was changed");
            }
            adb.push(file, PUSHED)?;
            (PUSHED.to_string(), true)
        }
        None => {
            let s = sensorctl();
            let r = adb.root(&format!("[ -x {s} ] && echo yes || echo no\n"))?;
            if r.out.trim() != "yes" {
                bail!(
                    "the staged runtime has no {s}: give this machine's copy with --binary STAGE/bin/sensorctl, or stage a \
                     runtime that has it (oneios-tool phone stage runtime-asis DIR). Nothing was changed"
                );
            }
            (s, false)
        }
    };
    let ran = adb.root(&script(&path, sensors, pushed));
    if pushed {
        // whatever the run did: the copy goes
        let _ = adb.root(&format!("rm -f {PUSHED}\n"));
    }
    let r = ran?;
    print!("{}", r.out);
    if !r.err.trim().is_empty() {
        eprint!("{}", r.err);
    }
    if let Some(p) = save {
        std::fs::write(p, &r.out).with_context(|| format!("cannot write {p}"))?;
        println!("saved: {p}");
    }
    let v = verdict(&r.out);
    if !v.is_empty() {
        println!("---- verdict");
        for line in v {
            println!("{line}");
        }
    }
    if outcome(&r.out) != "ok" {
        bail!(
            "sensorctl qrtr: the sensor hub is not reachable this way (status {}): sensors=auto takes the old path on this \
             phone; the fallback is the Android sensors HAL (runtime/bridges/sensors/README.md)",
            r.status
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_script_and_the_report() {
        assert_eq!(script("/x/sensorctl", false, false), "/x/sensorctl qrtr\n");
        assert_eq!(script(PUSHED, true, true), format!("chmod 0755 {PUSHED} && {PUSHED} qrtr --sensors\n"));
        let out = "qrtr: socket(AF_QIPCRTR) open: local node 1, our port 0x4001\nqrtr: 2 services on 2 nodes ([0, 5])\n\
                   qrtr: service 400 version 1 instance 0 node 5 port 0x9: SNS client (sensors: SEE)\n\
                   qrtr: SSC: service 400 version 1 at node 5 port 0x9 answers: accel lookup gave 1 SUID(s): sensors=auto takes the hub\nok\n";
        assert_eq!(verdict(out), [
            "qrtr: socket(AF_QIPCRTR) open: local node 1, our port 0x4001",
            "qrtr: SSC: service 400 version 1 at node 5 port 0x9 answers: accel lookup gave 1 SUID(s): sensors=auto takes the hub"
        ]);
        assert_eq!(outcome(out), "ok");
        assert_eq!(outcome("qrtr: socket(AF_QIPCRTR): Permission denied (13)\nfailed\n\n"), "failed");
    }
}
