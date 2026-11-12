//! apfs-fuse (github.com/sgan81/apfs-fuse, GPL-2.0) as a second way to read an APFS image, on Linux: started as
//! a program, never linked. `--apfs fuse` asks for it; `--apfs auto` (the default) takes it only when our own
//! reader refuses the image or the volume. It is also the reader the comparison scripts check ours against.
//!
//! The volume is mounted read-only (`apfs-fuse -o ro,vol=N IMAGE DIR`) on a new folder under the temporary
//! folder and always unmounted again (`fusermount3 -u`, else `fusermount -u`): when the `Mount` is dropped, and
//! from the handler of Ctrl-C and SIGTERM, which then ends the process with status 130.

use crate::{err, Fail};
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicPtr, Ordering};

/// Which reader a command uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Backend {
    /// Our reader; apfs-fuse when ours refuses the image or the volume (Linux, apfs-fuse installed)
    Auto,
    /// Our reader only
    Own,
    /// apfs-fuse only (Linux)
    Fuse,
}

/// The unmount command of the mount that is up, for the signal handler: [program, "-u", "-z", folder] as C
/// strings. Lazy (`-z`): this process may be in the middle of reading a file of the mount, which a plain unmount
/// refuses as busy; the lazy one detaches the folder at once and the kernel lets go when the process has ended.
static UNMOUNT: AtomicPtr<[CString; 4]> = AtomicPtr::new(std::ptr::null_mut());

extern "C" fn on_signal(_: libc::c_int) {
    let args = UNMOUNT.load(Ordering::SeqCst);
    // Safety: only fork, execv, waitpid, rmdir and _exit are called, which may be called from a signal handler; the
    // strings were made before the handler was set and are never freed while it can run.
    unsafe {
        if !args.is_null() {
            let a = &*args;
            let pid = libc::fork();
            if pid == 0 {
                let argv = [a[0].as_ptr(), a[1].as_ptr(), a[2].as_ptr(), a[3].as_ptr(), std::ptr::null()];
                libc::execv(a[0].as_ptr(), argv.as_ptr());
                libc::_exit(127);
            } else if pid > 0 {
                let mut status = 0;
                libc::waitpid(pid, &mut status, 0);
                libc::rmdir(a[3].as_ptr());
            }
        }
        libc::_exit(130);
    }
}

fn program(names: &[&str]) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    names.iter().find_map(|name| std::env::split_paths(&path).map(|dir| dir.join(name)).find(|p| p.is_file()))
}

/// Whether this host can mount with apfs-fuse.
pub fn available() -> bool {
    cfg!(target_os = "linux") && program(&["apfs-fuse"]).is_some() && program(&["fusermount3", "fusermount"]).is_some()
}

/// A volume of an image, mounted read-only until this is dropped.
pub struct Mount {
    dir: PathBuf,
    unmount: PathBuf,
}

impl Mount {
    pub fn new(image: &str, volume: usize) -> Result<Mount, Fail> {
        if !cfg!(target_os = "linux") {
            return Err(err!("--apfs fuse: apfs-fuse is used on Linux only (macOS mounts APFS itself; our reader needs no mount)"));
        }
        let fuse = program(&["apfs-fuse"]).ok_or_else(|| err!("--apfs fuse: no apfs-fuse on PATH (github.com/sgan81/apfs-fuse; optional, docs/building.md section 10)"))?;
        let unmount = program(&["fusermount3", "fusermount"]).ok_or_else(|| err!("--apfs fuse: no fusermount3 or fusermount on PATH (the fuse3 package)"))?;
        let dir = std::env::temp_dir().join(format!("oneios-apfs-{}-{volume}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| err!("{}: {e}", dir.display()))?;
        // the handler first, so that a Ctrl-C between the mount and the next line still unmounts
        let args = Box::new([
            CString::new(unmount.display().to_string()).map_err(|_| err!("a NUL in a path"))?,
            CString::new("-u").expect("no NUL"),
            CString::new("-z").expect("no NUL"),
            CString::new(dir.display().to_string()).map_err(|_| err!("a NUL in a path"))?,
        ]);
        UNMOUNT.store(Box::into_raw(args), Ordering::SeqCst);
        // Safety: the handler only calls what a signal handler may call.
        unsafe {
            libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
            libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        }
        let out = Command::new(&fuse).arg("-o").arg(format!("ro,vol={volume}")).arg(image).arg(&dir).output().map_err(|e| err!("{}: {e}", fuse.display()));
        let mount = Mount { dir, unmount };
        match out {
            Ok(out) if out.status.success() && mount.root().is_dir() => Ok(mount),
            Ok(out) => Err(err!("{image}: apfs-fuse did not mount volume {volume}: {}", String::from_utf8_lossy(&out.stderr).trim().lines().last().unwrap_or("no message"))),
            Err(e) => Err(e),
        }
        // (a `mount` that failed is dropped here: it unmounts what may be half up and removes the folder)
    }

    /// The volume's root folder.
    pub fn root(&self) -> PathBuf {
        self.dir.join("root")
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        let plain = Command::new(&self.unmount).arg("-u").arg(&self.dir).stderr(std::process::Stdio::null()).status();
        if !plain.map(|s| s.success()).unwrap_or(false) && mounted(&self.dir) {
            // busy (a file of it is still open somewhere): detach it now, the kernel lets go when it can
            let _ = Command::new(&self.unmount).arg("-u").arg("-z").arg(&self.dir).stderr(std::process::Stdio::null()).status();
        }
        // the handler has nothing to unmount any more (the strings stay: it may be running)
        UNMOUNT.store(std::ptr::null_mut(), Ordering::SeqCst);
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// Is `path` below a mounted file system of apfs-fuse (for the tests: nothing may stay mounted).
pub fn mounted(path: &Path) -> bool {
    std::fs::read_to_string("/proc/mounts").map(|m| m.lines().any(|l| l.split_whitespace().nth(1) == Some(&path.display().to_string()))).unwrap_or(false)
}
