//! `oneios-tool kernel hosted-fixup KERNEL` (was kernel/hosted-fixup.py, retired 2026-11-13): post-link fixes of
//! the hosted kernel's text. KERNEL is rewritten in place.
//!
//! Run by kernel/build-hosted.sh on the linked kernel, before it is sealed again (the kernel's text is covered by
//! corecrypto's integrity HMAC, so its instructions cannot be changed at boot).
//!
//!   mrs xN, MPIDR_EL1  ->  ldr wN, [x18]
//!       Under xnuhost a vCPU learns who it is from its struct xh_cpu (x18 in kernel code; offset 0 is the vCPU
//!       index, which is the "reg" of its /cpus node). MPIDR_EL1 cannot say it: at EL0 Linux answers that
//!       register itself with the same value on every core. XNU's own reads and ArmCpu's (it parks a secondary
//!       core whose MPIDR is not the affinity it was started with) all become the vCPU index.
//!
//! Only the executable segments of an MH_EXECUTE arm64/arm64e kernel are touched; anything else is refused.

use std::path::Path;

use anyhow::{bail, Context, Result};

const MRS_MPIDR: u32 = 0xd538_00a0; // mrs xN, MPIDR_EL1 (S3_0_C0_C0_5)
const LDR_INDEX: u32 = 0xb940_0240; // ldr wN, [x18]

fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

fn u64_at(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}

/// Rewrites every `mrs xN, MPIDR_EL1` of the executable segments; returns the addresses of the sites.
pub fn fix(data: &mut [u8]) -> Result<Vec<u64>> {
    if data.len() < 32 || (u32_at(data, 0), u32_at(data, 4), u32_at(data, 12)) != (0xfeed_facf, 0x100_000c, 2) {
        bail!("hosted-fixup: not an arm64 MH_EXECUTE kernel");
    }
    let (mut offset, mut sites) = (32usize, Vec::new());
    for _ in 0..u32_at(data, 16) {
        let (cmd, length) = (u32_at(data, offset), u32_at(data, offset + 4) as usize);
        // LC_SEGMENT_64 with VM_PROT_EXECUTE in its initial protection
        if cmd == 0x19 && u32_at(data, offset + 60) & 4 != 0 {
            let (va, fileoff, filesize) = (u64_at(data, offset + 24), u64_at(data, offset + 40) as usize, u64_at(data, offset + 48) as usize);
            for at in (fileoff..fileoff + (filesize & !3)).step_by(4) {
                let insn = u32_at(data, at);
                if insn & 0xffff_ffe0 == MRS_MPIDR {
                    data[at..at + 4].copy_from_slice(&(LDR_INDEX | (insn & 31)).to_le_bytes());
                    sites.push(va + (at - fileoff) as u64);
                }
            }
        }
        offset += length;
    }
    Ok(sites)
}

pub fn main(path: &Path) -> Result<()> {
    let mut data = std::fs::read(path).with_context(|| path.display().to_string())?;
    let sites = fix(&mut data)?;
    std::fs::write(path, &data).with_context(|| path.display().to_string())?;
    let shown: Vec<String> = sites.iter().take(16).map(|s| format!("{s:#x}")).collect();
    println!(
        "hosted-fixup: {} MPIDR_EL1 reads now read the vCPU index{}",
        sites.len(),
        if sites.is_empty() { String::new() } else { format!(": {}", shown.join(" ")) }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A made-up kernel: one executable segment with two reads and one that is data, one segment without execute.
    fn kernel() -> Vec<u8> {
        let mut data = vec![0u8; 32 + 72 * 2 + 32];
        for (at, value) in [(0, 0xfeed_facfu32), (4, 0x100_000c), (12, 2), (16, 2)] {
            data[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        let mut segment = |at: usize, va: u64, fileoff: u64, filesize: u64, prot: u32| {
            data[at..at + 4].copy_from_slice(&0x19u32.to_le_bytes());
            data[at + 4..at + 8].copy_from_slice(&72u32.to_le_bytes());
            data[at + 24..at + 32].copy_from_slice(&va.to_le_bytes());
            data[at + 40..at + 48].copy_from_slice(&fileoff.to_le_bytes());
            data[at + 48..at + 56].copy_from_slice(&filesize.to_le_bytes());
            data[at + 60..at + 64].copy_from_slice(&prot.to_le_bytes());
        };
        segment(32, 0x1000, 176, 18, 5); // 18 bytes: the last two are no instruction
        segment(104, 0x2000, 196, 8, 3);
        for (at, insn) in [(176, MRS_MPIDR | 3), (180, 0xd503_201f), (184, MRS_MPIDR | 17), (196, MRS_MPIDR | 1)] {
            data[at..at + 4].copy_from_slice(&insn.to_le_bytes());
        }
        data
    }

    #[test]
    fn only_executable_segments_change() {
        let mut data = kernel();
        assert_eq!(fix(&mut data).unwrap(), [0x1000, 0x1008]);
        assert_eq!(u32_at(&data, 176), LDR_INDEX | 3);
        assert_eq!(u32_at(&data, 180), 0xd503_201f);
        assert_eq!(u32_at(&data, 184), LDR_INDEX | 17);
        assert_eq!(u32_at(&data, 196), MRS_MPIDR | 1);
    }

    #[test]
    fn another_file_is_refused() {
        let mut data = kernel();
        data[12] = 0xc; // MH_FILESET
        assert_eq!(fix(&mut data).unwrap_err().to_string(), "hosted-fixup: not an arm64 MH_EXECUTE kernel");
    }
}
