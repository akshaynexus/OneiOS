//! `oneios-tool debug NAME`: the helpers of the comparison scripts under tools/oneios-tool/tests. Each was a
//! Python script beside its test until 2026-11-13 (CLAUDE.md: no new Python); each module has that script's name
//! and description, takes what it took and prints what it printed.
//!
//!   kcmap.rs          tests/ksym/kcmap.py          the test's own reader of a kernel collection
//!   ksym_make_log.rs  tests/ksym/make-log.py       a stand-in panic log for a collection and its kernel
//!   ksym_compare.rs   tests/ksym/compare.py        `sym kernel` against llvm-symbolizer
//!   kernel_diff.rs    tests/kernelgen/kernel-diff.py   where two builds of the hosted kernel differ
//!   mkstream.rs       tests/phone/mkstream.py      the streams of tests/phone/sparse-compare.sh
//!   pyrandom.rs       Python's random.Random, for the sample and the streams
//!   stub_kc.rs        new: a made-up kernel collection for the tests of `prelink stubs-check`
//!   fixture.rs        new: a made-up kernel and a made-up KernelDebugKit.pkg for tests/kernelpy

pub mod fixture;
pub mod kcmap;
pub mod kernel_diff;
pub mod ksym_compare;
pub mod ksym_make_log;
pub mod mkstream;
pub mod pyrandom;
pub mod stub_kc;

#[derive(clap::Subcommand)]
pub enum Command {
    /// The test's own reader of a kernel collection: members KC | forward KC KERNEL NAME... | own KC NAME...
    Kcmap {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// A stand-in xnuhost session log with the panic of 2026-11-12, for a collection and its kernel
    KsymMakeLog {
        kc: String,
        kernel: String,
        /// One line per address written: KIND ADDRESS-IN-LOG KERNEL-ADDRESS FUNCTION
        #[arg(long)]
        expect: Option<String>,
    },
    /// `sym kernel` against llvm-symbolizer, address by address (tests/ksym/compare.sh runs it)
    KsymCompare(ksym_compare::Args),
    /// Where two builds of the hosted kernel differ: only the build's time, the UUID, the N_OSO times, the seal?
    KernelDiff { a: String, b: String },
    /// A made-up kernel collection with GOOD good stubs and BAD bad ones (the tests of `prelink stubs-check`)
    StubKc {
        out: String,
        good: u64,
        bad: u64,
        /// Appended after the file's last byte
        note: Option<String>,
    },
    /// A made-up input for tests/kernelpy: hosted-kernel | kdk-pkg
    Fixture { name: String, out: String },
    /// The streams of tests/phone/sparse-compare.sh: runs|tail|zero|empty|lastbyte|dense | phone MIB DATA
    Mkstream {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

/// A helper's message ends on stderr with status 1, as the scripts' `SystemExit(message)` did.
pub fn main(command: Command) -> i32 {
    let done = match command {
        Command::Kcmap { args } => kcmap::run(&args),
        Command::KsymMakeLog { kc, kernel, expect } => ksym_make_log::run(&kc, &kernel, expect.as_deref()),
        Command::KsymCompare(args) => ksym_compare::run(&args),
        Command::KernelDiff { a, b } => kernel_diff::run(&a, &b),
        Command::Mkstream { args } => Ok(mkstream::run(&args)),
        Command::Fixture { name, out } => fixture::run(&name, &out),
        Command::StubKc { out, good, bad, note } => stub_kc::run(&out, good, bad, note.as_deref().unwrap_or("")),
    };
    done.unwrap_or_else(|e| {
        eprintln!("{e:#}");
        1
    })
}
