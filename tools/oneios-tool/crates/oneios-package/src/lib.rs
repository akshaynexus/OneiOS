//! `oneios-tool package`: the checks, the manifest and the archive of the runtime package (runtime/package.sh).
//! One module, `pack.rs`: it was runtime/package/pack.py, and takes that script's three commands and arguments.
//! Exit status: 0; 3 for a refusal (`package: refused: ...` on stderr: the package may not be made); 1 for anything
//! else that stopped it; 2 for a wrong command line.

pub mod pack;

use std::path::PathBuf;

#[derive(clap::Subcommand)]
pub enum Command {
    /// Refuse Apple files and anything the OneOS fork's unpacker rejects (exit 3)
    Check { stage: PathBuf },
    /// Write STAGE/runtime.json (after the same check)
    Manifest {
        stage: PathBuf,
        version: String,
        git_commit: String,
        /// 1 when the checkout has changes
        git_dirty: String,
        min_fork: String,
        /// 1 when bin/xnuhost is the placeholder
        placeholder: String,
    },
    /// A ustar archive of the files MEMBERS lists, in that order, owner 0:0, time MTIME
    Archive { stage: PathBuf, members: PathBuf, mtime: String, out: PathBuf },
}

pub fn main(command: Command) -> i32 {
    let done = match command {
        Command::Check { stage } => pack::check(&stage).map(|_| ()),
        Command::Manifest { stage, version, git_commit, git_dirty, min_fork, placeholder } => pack::manifest(&stage, &version, &git_commit, &git_dirty, &min_fork, &placeholder),
        Command::Archive { stage, members, mtime, out } => pack::archive(&stage, &members, &mtime, &out),
    };
    match done {
        Ok(()) => 0,
        Err(pack::Stop::Refused(refusal)) => {
            eprintln!("package: refused: {refusal}");
            3
        }
        Err(pack::Stop::Failed(message)) => {
            eprintln!("package: {message}");
            1
        }
    }
}
