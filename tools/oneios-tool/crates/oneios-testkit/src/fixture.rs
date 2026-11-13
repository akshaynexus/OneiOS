//! `oneios-tool debug fixture NAME OUT`: made-up inputs for the comparisons of the kernel scripts' ports
//! (tests/kernelpy). Every byte is made here: nothing of Apple's and nothing of another project.
//!
//!   hosted-kernel   an arm64 MH_EXECUTE with an executable segment (three `mrs xN, MPIDR_EL1`, other code, two
//!                   bytes that are no instruction) and a data segment that holds the same word: what
//!                   `kernel hosted-fixup` rewrites and what it must leave
//!   kdk-pkg         a KernelDebugKit.pkg as `kernel kdk-headers` reads one: a xar whose table of contents (zlib,
//!                   XML) names KDK_SDK.pkg/Payload and KDK.pkg/Payload, each a pbzx stream (one xz chunk, one
//!                   stored chunk) of an odc cpio archive. The first has made-up headers below the four folders
//!                   that are kept, a folder entry, a link and a file outside them; the second has a Kernel.h in
//!                   KernelSupport, an entry after that folder, and then bytes that are no cpio record (a reader
//!                   that does not stop after the folder fails on them).

use std::io::Write as _;

use anyhow::{bail, Context, Result};

const PLATFORM: &str = "./System/Library/Frameworks/Kernel.framework/Versions/A/PrivateHeaders/platform";

fn hosted_kernel() -> Vec<u8> {
    const MRS_MPIDR: u32 = 0xd538_00a0;
    let mut data = vec![0u8; 32 + 72 * 2 + 64];
    let mut put32 = |at: usize, value: u32| data[at..at + 4].copy_from_slice(&value.to_le_bytes());
    for (at, value) in [(0, 0xfeed_facf), (4, 0x100_000c), (8, 2), (12, 2), (16, 2), (20, 144)] {
        put32(at, value);
    }
    // (command offset, vmaddr low word, file offset, file size, initial protection)
    for (at, va, fileoff, filesize, prot) in [(32, 0x0700_4000u32, 176u32, 30u32, 5u32), (104, 0x0800_0000, 208, 16, 3)] {
        put32(at, 0x19);
        put32(at + 4, 72);
        put32(at + 24, va);
        put32(at + 28, 0xffff_fff0);
        put32(at + 40, fileoff);
        put32(at + 48, filesize);
        put32(at + 56, 7);
        put32(at + 60, prot);
    }
    for (at, insn) in [(176, MRS_MPIDR | 3), (180, 0xd503_201f), (184, MRS_MPIDR | 17), (188, 0xd65f_03c0), (192, MRS_MPIDR), (196, 0xaa00_03e0), (200, 0xd503_245f), (208, MRS_MPIDR | 1), (212, 0x1234_5678)] {
        put32(at, insn);
    }
    data
}

/// One odc cpio record.
fn cpio(out: &mut Vec<u8>, name: &str, mode: u32, data: &[u8]) {
    write!(out, "070707{:06o}{:06o}{:06o}{:06o}{:06o}{:06o}{:06o}{:011o}{:06o}{:011o}", 1, 2, mode, 0, 0, 1, 0, 0, name.len() + 1, data.len()).unwrap();
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    out.extend_from_slice(data);
}

/// A pbzx stream of `plain`: the first part as an xz chunk, the rest stored.
fn pbzx(plain: &[u8]) -> Result<Vec<u8>> {
    let cut = plain.len() * 2 / 3 + 1;
    let mut packed = Vec::new();
    lzma_rs::xz_compress(&mut &plain[..cut], &mut packed).context("xz")?;
    let mut out = b"pbzx".to_vec();
    out.extend_from_slice(&(1u64 << 24).to_be_bytes());
    for chunk in [&packed[..], &plain[cut..]] {
        out.extend_from_slice(&(1u64 << 24).to_be_bytes());
        out.extend_from_slice(&(chunk.len() as u64).to_be_bytes());
        out.extend_from_slice(chunk);
    }
    Ok(out)
}

fn sdk_payload() -> Result<Vec<u8>> {
    let mut archive = Vec::new();
    cpio(&mut archive, ".", 0o040755, b"");
    cpio(&mut archive, &format!("{PLATFORM}/CoreEntitlements"), 0o040755, b"");
    let api = "/* made-up stand-in for the comparison of kdk-headers */\ntypedef int CEError_t;\nCEError_t\nCEManagedContextFromCFData(int runtime, int data);\nCEError_t\nCEQueryContextAllowsEntitlement(int context);\nstatic int notCEAtTheStart(void);\n";
    let acceleration = "/* made-up stand-in */\nCEError_t\nCEAcquireManagedContext(int runtime);\nCEError_t\nCEManagedContextFromCFData(int runtime, int data);\n";
    cpio(&mut archive, &format!("{PLATFORM}/CoreEntitlements/V2/API.h"), 0o100644, api.as_bytes());
    cpio(&mut archive, &format!("{PLATFORM}/CoreEntitlements/V2/Acceleration.h"), 0o100644, acceleration.as_bytes());
    cpio(&mut archive, &format!("{PLATFORM}/CoreEntitlements/Current"), 0o120755, b"V2");
    cpio(&mut archive, &format!("{PLATFORM}/TrustCache/API.h"), 0o100644, b"/* made-up stand-in: TrustCache */\n");
    cpio(&mut archive, &format!("{PLATFORM}/CodeSignature/Entitlements.h"), 0o100644, b"/* made-up stand-in: CodeSignature */\n");
    cpio(&mut archive, &format!("{PLATFORM}/TrustedExecutionMonitor/API.h"), 0o100644, b"/* made-up stand-in: TXM */\n");
    cpio(&mut archive, &format!("{PLATFORM}/Other/NotKept.h"), 0o100644, b"/* outside the four folders */\n");
    cpio(&mut archive, "./System/Library/Frameworks/Kernel.framework/Versions/A/Headers/other.h", 0o100644, b"/* outside */\n");
    cpio(&mut archive, "TRAILER!!!", 0, b"");
    pbzx(&archive)
}

fn kdk_payload() -> Result<Vec<u8>> {
    let mut archive = Vec::new();
    cpio(&mut archive, "./System/Library/Kernels/kernel", 0o100755, b"made-up: no kernel");
    cpio(&mut archive, "./System/Library/KernelSupport/CoreEntitlements/V2/Kernel.h", 0o100644, b"/* made-up stand-in for the KDK's Kernel.h */\n");
    cpio(&mut archive, "./System/Library/KernelSupport/Later/not-read.a", 0o100644, b"the reader stops here");
    archive.extend_from_slice(b"these bytes are no cpio record: a reader that goes on fails here, and then some more of them");
    pbzx(&archive)
}

fn kdk_pkg() -> Result<Vec<u8>> {
    let (sdk, kdk) = (sdk_payload()?, kdk_payload()?);
    let payload = |offset: usize, length: usize| format!("<file><name>Payload</name><type>file</type><data><offset>{offset}</offset><length>{length}</length><encoding style=\"application/octet-stream\"/></data></file>");
    let toc = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<xar><toc><file><name>KDK_SDK.pkg</name><type>directory</type>{}</file><file><name>KDK.pkg</name><type>directory</type>{}</file><file><name>Distribution</name><type>file</type><data><offset>{}</offset><length>4</length><encoding style=\"application/x-gzip\"/></data></file></toc></xar>\n",
        payload(0, sdk.len()),
        payload(sdk.len(), kdk.len()),
        sdk.len() + kdk.len()
    );
    let mut packed = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    packed.write_all(toc.as_bytes())?;
    let packed = packed.finish()?;
    let mut out = b"xar!".to_vec();
    out.extend_from_slice(&28u16.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&(packed.len() as u64).to_be_bytes());
    out.extend_from_slice(&(toc.len() as u64).to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&packed);
    out.extend_from_slice(&sdk);
    out.extend_from_slice(&kdk);
    out.extend_from_slice(b"dist");
    Ok(out)
}

pub fn run(name: &str, out: &str) -> Result<i32> {
    let bytes = match name {
        "hosted-kernel" => hosted_kernel(),
        "kdk-pkg" => kdk_pkg()?,
        other => bail!("debug fixture: no fixture {other} (hosted-kernel, kdk-pkg)"),
    };
    std::fs::write(out, bytes).with_context(|| out.to_string())?;
    Ok(0)
}
