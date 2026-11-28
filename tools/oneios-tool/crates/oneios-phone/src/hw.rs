//! `oneios-tool phone hw [--out DIR]`: the phone's hardware as its running kernel was given it: the first facts of a new
//! port (docs/forks.md "What a new port still needs", beside tools/probe/s25-probe.sh): its device tree, memory map,
//! interrupts, CPUs, panel modes, partitions and kernel configuration. Read only, nothing is written on the phone,
//! refused during a session. From YumikoHayamato/OneiOS 9ff6bbd9 (run there on an SM-S938B on 2026-11-20).
//!
//! Named files only (the phone's rule: a walk of sysfs or /proc panicked its kernel on 2026-11-12):
//! - `/sys/firmware/fdt`, the flattened device tree the bootloader handed Linux (binary: `fdt.dtb`);
//! - `/proc/cmdline`, `version`, `cpuinfo`, `meminfo`, `iomem` (root: the real addresses), `partitions`, `interrupts`;
//! - each CPU's MIDR and REVIDR (`cpu0` to `cpu7` by name);
//! - the panel's connector, `/sys/class/drm/card0-DSI-1/{status,modes}` (absent: nothing);
//! - the partitions' names, `ls -l /dev/block/by-name` (one folder of /dev, not of /sys or /proc);
//! - a few properties (`getprop` by name), the loaded modules (`/proc/modules`);
//! - for a VM under Qualcomm's hypervisor (the fork's native path; facts only here): `ls -lZ` of `/dev/gunyah`, `/dev/kvm`,
//!   `/dev/udmabuf` and AVF's `crosvm`, `vm` and `pvmfw.img` by name, the hypervisor's properties, the package
//!   manager's virtualization features; `vm info`, `crosvm version` and `crosvm run --help` (queries, no VM is made);
//! - the kernel's configuration, `/proc/config.gz` (also unpacked to `kernel.config`).
//!
//! Each text lands in DIR as its own file (`cmdline.txt`, ...), the tree as `fdt.dtb`, and `hw.txt` says what came.
use crate::adb::Adb;
use anyhow::{bail, Context, Result};
use std::fs;
use std::io::copy;
use std::path::Path;

/// The texts: (file in DIR, what root's shell runs). Each is one named file or one named command.
pub const TEXTS: &[(&str, &str)] = &[
    ("cmdline.txt", "cat /proc/cmdline"),
    ("version.txt", "cat /proc/version"),
    ("cpuinfo.txt", "cat /proc/cpuinfo"),
    ("meminfo.txt", "cat /proc/meminfo"),
    ("iomem.txt", "cat /proc/iomem"),
    ("partitions.txt", "cat /proc/partitions"),
    ("interrupts.txt", "cat /proc/interrupts"),
    ("by-name.txt", "ls -l /dev/block/by-name"),
    ("drm.txt", "for f in status modes; do echo \"card0-DSI-1/$f: $(cat /sys/class/drm/card0-DSI-1/$f 2>/dev/null | tr '\\n' ' ')\"; done"),
    ("cpuregs.txt", "for n in 0 1 2 3 4 5 6 7; do d=/sys/devices/system/cpu/cpu$n/regs/identification; \
echo \"cpu$n midr $(cat $d/midr_el1 2>/dev/null) revidr $(cat $d/revidr_el1 2>/dev/null)\"; done"),
    ("props.txt", "for p in ro.product.model ro.product.device ro.board.platform ro.bootloader ro.build.fingerprint \
ro.boot.flash.locked ro.boot.verifiedbootstate; do echo \"$p=$(getprop $p)\"; done"),
    ("modules.txt", "cat /proc/modules"),
    // a VM under Qualcomm's hypervisor (YumikoHayamato's native path): its device nodes and AVF's programs by name, the
    // hypervisor's properties, the package manager's virtualization features
    ("hyp.txt", "for f in /dev/gunyah /dev/kvm /dev/udmabuf /apex/com.android.virt/bin/crosvm /apex/com.android.virt/bin/vm \
/apex/com.android.virt/etc/pvmfw.img; do echo \"$f: $(ls -lZ $f 2>&1)\"; done; \
getprop | grep -i -E 'hypervisor|virtualization|gunyah|avf'; pm list features 2>/dev/null | grep -i virtualization"),
    // what AVF says of the hypervisor and what its crosvm takes: queries and help texts, no VM is made. Each is
    // given 20 s: `vm info` waited on a pipe for ever on 2026-11-20 when it had the script's stdin (now /dev/null)
    ("avf.txt", "timeout 20 /apex/com.android.virt/bin/vm info; timeout 20 /apex/com.android.virt/bin/crosvm version; \
timeout 20 /apex/com.android.virt/bin/crosvm run --help"),
];

/// The binary files, copied as they are: (file in DIR, the phone's file). `config.gz` is also unpacked to
/// `kernel.config`.
pub const BINARIES: &[(&str, &str)] = &[("fdt.dtb", "/sys/firmware/fdt"), ("config.gz", "/proc/config.gz")];

/// The one script for all the texts: each one's output after a line `=== NAME`. Each runs in a subshell whose stdin
/// is /dev/null: the script itself comes on root shell's stdin, and a program that reads it would wait for ever.
pub fn hw_script() -> String {
    let mut s = String::new();
    for (name, run) in TEXTS {
        s += &format!("echo '=== {name}'\n( {run} ) </dev/null 2>&1\n");
    }
    s
}

/// A gzip file's text (`/proc/config.gz`), or None when it is not gzip.
pub fn unpack(data: &[u8]) -> Option<String> {
    use std::io::Read;
    if data.len() < 2 || data[..2] != [0x1f, 0x8b] {
        return None;
    }
    let mut text = String::new();
    flate2::read::GzDecoder::new(data).read_to_string(&mut text).ok()?;
    Some(text)
}

/// The script's output split back into (name, text).
pub fn split(out: &str) -> Vec<(String, String)> {
    let mut parts: Vec<(String, String)> = Vec::new();
    for line in out.lines() {
        if let Some(name) = line.strip_prefix("=== ") {
            parts.push((name.trim().to_string(), String::new()));
        } else if let Some((_, text)) = parts.last_mut() {
            text.push_str(line);
            text.push('\n');
        }
    }
    parts
}

/// `phone hw`.
pub fn hw(adb: &Adb, out: &str) -> Result<()> {
    adb.need_root()?;
    adb.refuse_in_session()?;
    fs::create_dir_all(out).with_context(|| format!("cannot make {out}"))?;
    let r = adb.root(&hw_script())?;
    if r.out.is_empty() {
        bail!("the read gave nothing (status {}): {}", r.status, r.err.trim());
    }
    let mut report = String::new();
    for (name, text) in split(&r.out) {
        let path = Path::new(out).join(&name);
        fs::write(&path, &text).with_context(|| format!("cannot write {}", path.display()))?;
        report += &format!("{name}: {} lines\n", text.lines().count());
    }
    for (name, remote) in BINARIES {
        let path = Path::new(out).join(name);
        let mut child = adb.exec_out(&format!("su -c 'cat {remote}'"))?;
        let mut file = fs::File::create(&path).with_context(|| format!("cannot write {}", path.display()))?;
        let n = copy(child.stdout.as_mut().context("no output from adb")?, &mut file)?;
        child.wait()?;
        let data = fs::read(&path)?;
        report += &match *name {
            "fdt.dtb" => {
                if n < 40 || data[..4] != [0xd0, 0x0d, 0xfe, 0xed] {
                    bail!("{}: {n} bytes, not a flattened device tree (no d00dfeed): the phone's kernel has no {remote}?", path.display());
                }
                let total = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
                format!("{name}: {n} bytes, the header says {total}\n")
            }
            _ => match unpack(&data) {
                Some(text) => {
                    fs::write(Path::new(out).join("kernel.config"), &text)?;
                    format!("{name}: {n} bytes, kernel.config {} lines\n", text.lines().count())
                }
                None => format!("{name}: {n} bytes, not gzip (the phone's kernel has no {remote}?)\n"),
            },
        };
    }
    fs::write(Path::new(out).join("hw.txt"), &report)?;
    print!("{report}");
    println!("hw: {out} (decompile the tree: dtc -I dtb -O dts {}/fdt.dtb)", out);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_script_reads_named_files_only() {
        let s = hw_script();
        assert!(!s.contains('*'), "no glob");
        assert!(!s.contains("find ") && !s.contains("ls -R") && !s.contains("du "), "no walk");
        // the one folder listed is /dev's, never a folder of /sys or /proc; else `ls -lZ` of files named in the loop
        for line in s.lines().filter(|l| l.contains("ls ")) {
            assert!(line.contains("ls -l /dev/block/by-name") || line.contains("ls -lZ $f 2>&1"), "{line}");
        }
        let named = s.lines().find(|l| l.contains("for f in /dev/gunyah")).expect("the named files' loop");
        let list = named.split("for f in ").nth(1).and_then(|rest| rest.split("; do").next()).expect("for f in ...; do");
        let files: Vec<&str> = list.split_whitespace().collect();
        assert!(files.len() >= 5 && files.contains(&"/dev/gunyah"), "{files:?}");
        for f in files {
            assert!(f.starts_with("/dev/") || f.starts_with("/apex/"), "{f}");
        }
        assert!(s.contains("cat /proc/iomem") && s.contains("for n in 0 1 2 3 4 5 6 7"));
        // no text reads the script's own stdin
        assert_eq!(s.matches(") </dev/null 2>&1\n").count(), TEXTS.len());
        for (_, remote) in BINARIES {
            assert!(!remote.contains('*') && remote.starts_with('/'), "{remote}");
        }
    }

    #[test]
    fn config_gz_unpacks_and_anything_else_is_not_gzip() {
        use std::io::Write;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(b"CONFIG_GUNYAH=y\nCONFIG_UH=y\n").unwrap();
        let data = gz.finish().unwrap();
        assert_eq!(unpack(&data).as_deref(), Some("CONFIG_GUNYAH=y\nCONFIG_UH=y\n"));
        assert_eq!(unpack(b"cat: /proc/config.gz: No such file"), None);
        assert_eq!(unpack(b""), None);
    }

    #[test]
    fn the_output_splits_back_into_its_files() {
        let out = "=== cmdline.txt\nconsole=ttyMSM0\n=== iomem.txt\n80000000-8fffffff : System RAM\n00000000-0 : x\n=== drm.txt\n";
        let parts = split(out);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], ("cmdline.txt".to_string(), "console=ttyMSM0\n".to_string()));
        assert_eq!(parts[1].1.lines().count(), 2);
        assert_eq!(parts[2].1, "");
    }
}
