// SPDX-License-Identifier: MIT
//! Starting a helper process with its end of a socket as descriptor 3 (the camera's and the encoder's helpers), with
//! posix_spawn as the C did: xnuhost holds gigabytes of locked guest RAM, and a fork would copy its page tables.
use std::ffi::{c_char, c_int, CString};
use std::os::fd::{AsRawFd, BorrowedFd};

extern "C" {
    static environ: *const *mut c_char;
    // bionic has them from API 28, glibc always; the libc crate declares them for glibc only.
    fn posix_spawn(
        pid: *mut libc::pid_t,
        path: *const c_char,
        actions: *const libc::c_void,
        attr: *const libc::c_void,
        argv: *const *mut c_char,
        envp: *const *mut c_char,
    ) -> c_int;
    fn posix_spawn_file_actions_init(actions: *mut libc::c_void) -> c_int;
    fn posix_spawn_file_actions_adddup2(actions: *mut libc::c_void, fd: c_int, to: c_int) -> c_int;
    fn posix_spawn_file_actions_destroy(actions: *mut libc::c_void) -> c_int;
    fn posix_spawnattr_init(attr: *mut libc::c_void) -> c_int;
    fn posix_spawnattr_setflags(attr: *mut libc::c_void, flags: libc::c_short) -> c_int;
    fn posix_spawnattr_setsigmask(attr: *mut libc::c_void, sigmask: *const libc::sigset_t) -> c_int;
    fn posix_spawnattr_destroy(attr: *mut libc::c_void) -> c_int;
}

/// POSIX_SPAWN_SETSIGMASK on both glibc and Android bionic (<spawn.h>).
const POSIX_SPAWN_SETSIGMASK: libc::c_short = 0x08;

/// Starts `path` with `args` after argv[0] (= `path`), this process's environment, an empty signal mask (so helpers
/// like oneios-encoder / oneios-camera never inherit xnuhost's blocked vCPU real-time signals), and `fd` as its
/// descriptor 3 (dup2 clears close-on-exec on the copy; every other descriptor of ours is close-on-exec). Its pid, or
/// the errno posix_spawn gave (ENOENT for a program that is not there).
pub fn spawn_with_fd3(path: &str, args: &[&str], fd: BorrowedFd<'_>) -> Result<libc::pid_t, i32> {
    let einval = |_| libc::EINVAL;
    let path_c = CString::new(path).map_err(einval)?;
    let args_c = args.iter().map(|a| CString::new(*a)).collect::<Result<Vec<_>, _>>().map_err(einval)?;
    let mut argv: Vec<*mut c_char> = std::iter::once(&path_c).chain(&args_c).map(|a| a.as_ptr().cast_mut()).collect();
    argv.push(std::ptr::null_mut());
    // posix_spawn_file_actions_t: 80 bytes in glibc, a pointer in bionic; this is room for either, aligned.
    let mut actions = [0u64; 16];
    let at = actions.as_mut_ptr().cast::<libc::c_void>();
    // posix_spawnattr_t: 336 bytes in glibc, a pointer in bionic; 48 u64s (384 bytes) holds either, aligned.
    let mut attr = [0u64; 48];
    let attr_p = attr.as_mut_ptr().cast::<libc::c_void>();
    let mut pid: libc::pid_t = 0;
    // SAFETY: the actions/attr objects live on the stack for the calls; argv is NULL-terminated over live CStrings;
    // the environment is the process's own; empty sigset unblocks all signals in the child.
    let rc = unsafe {
        let mut empty: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut empty);
        posix_spawn_file_actions_init(at);
        posix_spawn_file_actions_adddup2(at, fd.as_raw_fd(), 3);
        posix_spawnattr_init(attr_p);
        posix_spawnattr_setflags(attr_p, POSIX_SPAWN_SETSIGMASK);
        posix_spawnattr_setsigmask(attr_p, &empty);
        let rc = posix_spawn(&mut pid, path_c.as_ptr(), at, attr_p, argv.as_ptr(), environ);
        posix_spawnattr_destroy(attr_p);
        posix_spawn_file_actions_destroy(at);
        rc
    };
    if rc != 0 {
        return Err(rc);
    }
    Ok(pid)
}

/// Reaps `pid` if it has ended, without waiting (`waitpid(pid, NULL, WNOHANG)`).
pub fn reap_nowait(pid: libc::pid_t) {
    // SAFETY: a plain call on our own child.
    unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
}
