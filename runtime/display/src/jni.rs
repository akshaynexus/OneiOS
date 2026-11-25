// SPDX-License-Identifier: MIT
//! Port of jni.c. liboneios-display.so: the native half of OneiOSDisplay (java/OneiOSDisplay.java). It owns the
//! socket, the mapped frame and the copy into the layer's buffers; the Java half owns the layers.
//!
//! ANativeWindow_setBuffersDataSpace and _get are API 28 and libnativewindow's (linked below: libandroid does not have
//! them; without it the library links but app_process cannot load it, the 2026-11-14 phone session: "cannot locate
//! symbol ANativeWindow_getBuffersDataSpace"); the library is built for 30, so they are there. build.rs links the
//! cdylib with -z defs, so a symbol no named library has is a link error here, not on the phone.
use crate::sys;
use crate::xhd::{self, run, Direct, Frame, DATASPACE_UNKNOWN, PANEL_DIRECT};
use std::ffi::{c_char, c_void, CStr};
use std::os::fd::{AsFd, BorrowedFd, IntoRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

// ---- JNI's types and the four functions of its table that are called --------------------------------------------

type Jint = i32;
type Jlong = i64;
type Jboolean = u8;
type Jobject = *mut c_void;
type Jstring = Jobject;
type JintArray = Jobject;
/// JNIEnv*: a pointer to the pointer to JNI's function table (struct JNINativeInterface).
type Env = *mut *const *const c_void;
const JNI_TRUE: Jboolean = 1;
const JNI_FALSE: Jboolean = 0;

// The table's slots (jni.h, struct JNINativeInterface: four reserved pointers, then the functions in order).
const GET_STRING_UTF_CHARS: usize = 169;
const RELEASE_STRING_UTF_CHARS: usize = 170;
const NEW_INT_ARRAY: usize = 179;
const SET_INT_ARRAY_REGION: usize = 211;

/// The function in slot `index` of env's table, as type F.
///
/// # Safety
/// env is the JNIEnv the VM passed to this call, and F is the slot's C signature.
unsafe fn jni_fn<F: Copy>(env: Env, index: usize) -> F {
    // SAFETY: (by the caller) env points at the VM's table, which has this slot; a function pointer and F (an extern
    // "C" fn pointer) have the same size.
    unsafe { std::mem::transmute_copy(&*(*env).add(index)) }
}

/// A Java string's (modified) UTF-8, copied out; None for null or no memory.
fn java_string(env: Env, s: Jstring) -> Option<Vec<u8>> {
    if s.is_null() {
        return None;
    }
    type Get = unsafe extern "C" fn(Env, Jstring, *mut Jboolean) -> *const c_char;
    type Release = unsafe extern "C" fn(Env, Jstring, *const c_char);
    // SAFETY: env is the VM's, s a live string; the chars are copied before they are released.
    unsafe {
        let chars = jni_fn::<Get>(env, GET_STRING_UTF_CHARS)(env, s, std::ptr::null_mut());
        if chars.is_null() {
            return None;
        }
        let out = CStr::from_ptr(chars).to_bytes().to_vec();
        jni_fn::<Release>(env, RELEASE_STRING_UTF_CHARS)(env, s, chars);
        Some(out)
    }
}

/// A new int[] holding values (null when the VM has no memory: it has thrown then).
fn int_array(env: Env, values: &[Jint]) -> JintArray {
    type New = unsafe extern "C" fn(Env, Jint) -> JintArray;
    type Set = unsafe extern "C" fn(Env, JintArray, Jint, Jint, *const Jint);
    // SAFETY: env is the VM's; the region written is the new array's whole length.
    unsafe {
        let array = jni_fn::<New>(env, NEW_INT_ARRAY)(env, values.len() as Jint);
        if !array.is_null() {
            jni_fn::<Set>(env, SET_INT_ARRAY_REGION)(env, array, 0, values.len() as Jint, values.as_ptr());
        }
        array
    }
}

// ---- the NDK's native window -------------------------------------------------------------------------------------

#[repr(C)]
struct ANativeWindow {
    _opaque: [u8; 0],
}

#[repr(C)]
struct ANativeWindowBuffer {
    width: i32,
    height: i32,
    stride: i32,
    format: i32,
    bits: *mut c_void,
    reserved: [u32; 6],
}

#[repr(C)]
struct ARect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[link(name = "android")]
extern "C" {
    fn ANativeWindow_fromSurface(env: Env, surface: Jobject) -> *mut ANativeWindow;
    fn ANativeWindow_release(window: *mut ANativeWindow);
    fn ANativeWindow_setBuffersGeometry(window: *mut ANativeWindow, width: i32, height: i32, format: i32) -> i32;
    fn ANativeWindow_lock(window: *mut ANativeWindow, buffer: *mut ANativeWindowBuffer, dirty: *mut ARect) -> i32;
    fn ANativeWindow_unlockAndPost(window: *mut ANativeWindow) -> i32;
}

#[link(name = "nativewindow")]
extern "C" {
    fn ANativeWindow_setBuffersDataSpace(window: *mut ANativeWindow, dataspace: i32) -> i32;
    fn ANativeWindow_getBuffersDataSpace(window: *mut ANativeWindow) -> i32;
}

/// The layer's window, released when dropped.
struct Window(*mut ANativeWindow);

impl Window {
    fn from_surface(env: Env, surface: Jobject) -> Option<Window> {
        // SAFETY: env is the VM's and surface the Java half's android.view.Surface.
        let w = unsafe { ANativeWindow_fromSurface(env, surface) };
        (!w.is_null()).then_some(Window(w))
    }

    fn set_geometry(&self, width: u32, height: u32, format: i32) {
        // SAFETY: self.0 is a live window (released only in drop).
        unsafe { ANativeWindow_setBuffersGeometry(self.0, width as i32, height as i32, format) };
    }

    fn set_dataspace(&self, dataspace: i32) -> i32 {
        // SAFETY: as above.
        unsafe { ANativeWindow_setBuffersDataSpace(self.0, dataspace) }
    }

    fn dataspace(&self) -> i32 {
        // SAFETY: as above.
        unsafe { ANativeWindow_getBuffersDataSpace(self.0) }
    }

    /// Locks a buffer whole (dirty: the rectangle asked for). The error is the NDK's (negative) number.
    fn lock(&self, width: u32, height: u32) -> Result<run::Locked, i32> {
        let mut dirty = ARect { left: 0, top: 0, right: width as i32, bottom: height as i32 };
        let mut b = ANativeWindowBuffer {
            width: 0,
            height: 0,
            stride: 0,
            format: 0,
            bits: std::ptr::null_mut(),
            reserved: [0; 6],
        };
        // SAFETY: a live window; b and dirty are written by the call.
        let error = unsafe { ANativeWindow_lock(self.0, &mut b, &mut dirty) };
        if error != 0 {
            return Err(error);
        }
        Ok(run::Locked {
            width: b.width,
            height: b.height,
            stride: b.stride,
            format: b.format,
            bits: b.bits as *mut u8,
            key: b.bits as usize,
        })
    }

    fn unlock_and_post(&self) {
        // SAFETY: a live window with a locked buffer.
        unsafe { ANativeWindow_unlockAndPost(self.0) };
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: the reference fromSurface gave, released once.
        unsafe { ANativeWindow_release(self.0) };
    }
}

// ---- the signals ------------------------------------------------------------------------------------------------

// SIGTERM, SIGINT and SIGHUP end the run loop through a pipe, so the Java half removes the layers.
static SIGNAL_READ: AtomicI32 = AtomicI32::new(-1);
static SIGNAL_WRITE: AtomicI32 = AtomicI32::new(-1);
static STOPPING: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(number: libc::c_int) {
    // SAFETY: errno is saved and restored around a write(2), all async-signal-safe.
    unsafe {
        let errno = libc::__errno();
        let saved = *errno;
        STOPPING.store(true, Ordering::SeqCst);
        let byte = number as u8;
        let fd = SIGNAL_WRITE.load(Ordering::SeqCst);
        if fd >= 0 {
            let _ = libc::write(fd, &byte as *const u8 as *const c_void, 1);
        }
        *errno = saved;
    }
}

/// The pipe and the handlers, once (a pipe that cannot be made: no handlers, as in C).
fn install_signals() {
    if SIGNAL_READ.load(Ordering::SeqCst) >= 0 {
        return;
    }
    let Ok((r, w)) = sys::pipe() else { return };
    // The pipe lives as long as the process: app_process loads this library once.
    SIGNAL_WRITE.store(w.into_raw_fd(), Ordering::SeqCst);
    SIGNAL_READ.store(r.into_raw_fd(), Ordering::SeqCst);
    // SAFETY: a zeroed sigaction with a handler that only does async-signal-safe things.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}

fn stopping() -> bool {
    STOPPING.load(Ordering::SeqCst)
}

// ---- the helper's state, the Java half's handle -----------------------------------------------------------------

struct Context {
    fd: OwnedFd,
    frame: Option<Frame>,
    direct: Direct,
    /// The refresh rate the Java half asked for (nHello): the write-only child's frame rate vote.
    hz: i32,
}

/// The handle's context.
///
/// # Safety
/// handle is one nOpen returned and nClose has not freed.
unsafe fn context<'a>(handle: Jlong) -> &'a mut Context {
    // SAFETY: (by the caller) a live Box<Context> leaked by nOpen; the Java half calls one native at a time.
    unsafe { &mut *(handle as usize as *mut Context) }
}

#[no_mangle]
pub extern "C" fn Java_OneiOSDisplay_nOpen(env: Env, _type: Jobject, socket_path: Jstring, wait_ms: Jint) -> Jlong {
    install_signals();
    let Some(path) = java_string(env, socket_path) else { return 0 };
    let path = PathBuf::from(std::ffi::OsStr::from_bytes(&path));
    match xhd::connect(&path, wait_ms, &stopping) {
        Ok(fd) => Box::into_raw(Box::new(Context { fd, frame: None, direct: Direct::new(), hz: 0 })) as usize as Jlong,
        Err(e) => {
            let why = if stopping() { "stopped while waiting".to_string() } else { e.to_string() };
            eprintln!("oneios-display: {}: {why}", path.display());
            0
        }
    }
}

/// Reports the panel's mode, then takes xnuhost's hello. JNI_FALSE: no usable hello (nClose is still the caller's).
#[no_mangle]
pub extern "C" fn Java_OneiOSDisplay_nHello(
    _env: Env,
    _type: Jobject,
    handle: Jlong,
    width: Jint,
    height: Jint,
    hz: Jint,
    direct: Jboolean,
) -> Jboolean {
    // SAFETY: the Java half passes the handle nOpen gave it.
    let c = unsafe { context(handle) };
    let flags = if direct != 0 { PANEL_DIRECT } else { 0 };
    c.hz = hz;
    if let Err(e) = xhd::send_panel(c.fd.as_fd(), width as u32, height as u32, hz as u32, flags) {
        eprintln!("oneios-display: the panel's mode could not be sent: {e}");
    }
    match xhd::recv_hello(c.fd.as_fd(), 10000) {
        Ok(frame) => {
            c.frame = Some(frame);
            JNI_TRUE
        }
        Err(e) => {
            eprintln!("oneios-display: no usable hello from xnuhost: {e}");
            c.frame = None;
            JNI_FALSE
        }
    }
}

#[no_mangle]
pub extern "C" fn Java_OneiOSDisplay_nInfo(env: Env, _type: Jobject, handle: Jlong) -> JintArray {
    // SAFETY: the Java half passes the handle nOpen gave it.
    let c = unsafe { context(handle) };
    let v =
        c.frame.as_ref().map_or([0; 4], |f| [f.width as Jint, f.height as Jint, f.stride as Jint, f.format as Jint]);
    int_array(env, &v)
}

#[no_mangle]
pub extern "C" fn Java_OneiOSDisplay_nFit(
    env: Env,
    _type: Jobject,
    sw: Jint,
    sh: Jint,
    dw: Jint,
    dh: Jint,
) -> JintArray {
    let f = xhd::fit(sw as u32, sh as u32, dw as u32, dh as u32);
    int_array(env, &[f.x as Jint, f.y as Jint, f.w as Jint, f.h as Jint])
}

/// The buffer layout and dataspace a frame format gets (xhd::buffer_for): {buffer, dataspace}.
#[no_mangle]
pub extern "C" fn Java_OneiOSDisplay_nBuffer(env: Env, _type: Jobject, format: Jint) -> JintArray {
    let (buffer, dataspace) = xhd::buffer_for(format as u32).unwrap_or((0, DATASPACE_UNKNOWN));
    int_array(env, &[buffer, dataspace])
}

#[no_mangle]
pub extern "C" fn Java_OneiOSDisplay_nClose(_env: Env, _type: Jobject, handle: Jlong) {
    if handle == 0 {
        return;
    }
    // SAFETY: the handle nOpen leaked, freed once: the socket, the frame and guest RAM's maps are closed with it.
    drop(unsafe { Box::from_raw(handle as usize as *mut Context) });
}

// ---- the write-only buffers: AHardwareBuffer and ASurfaceTransaction (xhd::present, display-present=ahb) ----------

#[repr(C)]
struct AHardwareBuffer {
    _opaque: [u8; 0],
}

#[repr(C)]
struct ASurfaceControl {
    _opaque: [u8; 0],
}

#[repr(C)]
struct ASurfaceTransaction {
    _opaque: [u8; 0],
}

#[repr(C)]
struct ASurfaceTransactionStats {
    _opaque: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AHardwareBufferDesc {
    width: u32,
    height: u32,
    layers: u32,
    format: u32,
    usage: u64,
    stride: u32,
    rfu0: u32,
    rfu1: u64,
}

/// CPU_WRITE_RARELY | CPU_READ_NEVER | GPU_SAMPLED_IMAGE | COMPOSER_OVERLAY (hardware_buffer.h): the CPU only writes, so
/// the buffer is not in the CPU's caches and no lock or post maintains them.
const AHB_USAGE_WRITE_ONLY: u64 = (2 << 4) | (1 << 8) | (1 << 11); // CPU_READ_NEVER is 0
const AHB_USAGE_CPU_WRITE_RARELY: u64 = 2 << 4;
const VISIBILITY_HIDE: i8 = 0;
const VISIBILITY_SHOW: i8 = 1;
const TRANSPARENCY_OPAQUE: i8 = 2;

type OnComplete = extern "C" fn(context: *mut c_void, stats: *mut ASurfaceTransactionStats);

// All of them in libandroid's API 30 stubs (AHardwareBuffer_* in libnativewindow as well); setFrameRate is API 30.
#[link(name = "android")]
extern "C" {
    fn AHardwareBuffer_isSupported(desc: *const AHardwareBufferDesc) -> i32;
    fn AHardwareBuffer_allocate(desc: *const AHardwareBufferDesc, out: *mut *mut AHardwareBuffer) -> i32;
    fn AHardwareBuffer_describe(buffer: *const AHardwareBuffer, desc: *mut AHardwareBufferDesc);
    fn AHardwareBuffer_lock(
        buffer: *mut AHardwareBuffer,
        usage: u64,
        fence: i32,
        rect: *const ARect,
        out: *mut *mut c_void,
    ) -> i32;
    fn AHardwareBuffer_unlock(buffer: *mut AHardwareBuffer, fence: *mut i32) -> i32;
    fn AHardwareBuffer_release(buffer: *mut AHardwareBuffer);
    fn ASurfaceControl_createFromWindow(parent: *mut ANativeWindow, name: *const c_char) -> *mut ASurfaceControl;
    fn ASurfaceControl_release(sc: *mut ASurfaceControl);
    fn ASurfaceTransaction_create() -> *mut ASurfaceTransaction;
    fn ASurfaceTransaction_delete(t: *mut ASurfaceTransaction);
    fn ASurfaceTransaction_apply(t: *mut ASurfaceTransaction);
    fn ASurfaceTransaction_setBuffer(
        t: *mut ASurfaceTransaction,
        sc: *mut ASurfaceControl,
        buffer: *mut AHardwareBuffer,
        acquire_fence: i32,
    );
    fn ASurfaceTransaction_setDamageRegion(
        t: *mut ASurfaceTransaction,
        sc: *mut ASurfaceControl,
        rects: *const ARect,
        count: u32,
    );
    fn ASurfaceTransaction_setVisibility(t: *mut ASurfaceTransaction, sc: *mut ASurfaceControl, visibility: i8);
    fn ASurfaceTransaction_setBufferTransparency(
        t: *mut ASurfaceTransaction,
        sc: *mut ASurfaceControl,
        transparency: i8,
    );
    fn ASurfaceTransaction_setBufferDataSpace(t: *mut ASurfaceTransaction, sc: *mut ASurfaceControl, dataspace: i32);
    fn ASurfaceTransaction_setFrameRate(
        t: *mut ASurfaceTransaction,
        sc: *mut ASurfaceControl,
        rate: f32,
        compatibility: i8,
    );
    fn ASurfaceTransaction_setOnComplete(t: *mut ASurfaceTransaction, context: *mut c_void, func: OnComplete);
    fn ASurfaceTransaction_reparent(
        t: *mut ASurfaceTransaction,
        sc: *mut ASurfaceControl,
        parent: *mut ASurfaceControl,
    );
    fn ASurfaceTransactionStats_getPreviousReleaseFenceFd(
        stats: *mut ASurfaceTransactionStats,
        sc: *mut ASurfaceControl,
    ) -> i32;
}

/// What a transaction's completion needs: the child (whose previous buffer's release fence is asked for) and the
/// present loop's callback. Leaked into the transaction's context, taken back once in `completed`.
struct Completion {
    sc: *mut ASurfaceControl,
    done: xhd::present::Done,
}

extern "C" fn completed(context: *mut c_void, stats: *mut ASurfaceTransactionStats) {
    // SAFETY: context is the Box<Completion> post() leaked for this transaction, completed once (setOnComplete's
    // contract). The child is let go only after the completions came (or a second passed: Present's end), and the
    // stats look the child up by its address among the transaction's own references (AOSP surface_control.cpp).
    let c = unsafe { Box::from_raw(context as *mut Completion) };
    // SAFETY: stats is the NDK's for this call; the fence is ours to close (surface_control.h).
    let fd = unsafe { ASurfaceTransactionStats_getPreviousReleaseFenceFd(stats, c.sc) };
    // SAFETY: a file descriptor the NDK handed over, owned here from now on.
    let fence = (fd >= 0).then(|| unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) });
    (c.done)(fence);
}

/// xhd::present's NDK half on the layer's window: a child surface of it and three write-only buffers.
struct Ahb {
    window: *mut ANativeWindow,
    sc: *mut ASurfaceControl,
    buffers: [*mut AHardwareBuffer; xhd::present::SLOTS],
    hz: i32,
}

// SAFETY: the raw pointers are the NDK's thread-safe objects (AHardwareBuffer, ASurfaceControl are reference counted);
// only the poster's thread calls the methods.
unsafe impl Send for Ahb {}

impl Ahb {
    /// window: the layer's (owned by its Window, which outlives this: both are in nRun's Present, the window last).
    fn new(window: *mut ANativeWindow, hz: i32) -> Ahb {
        Ahb { window, sc: std::ptr::null_mut(), buffers: [std::ptr::null_mut(); xhd::present::SLOTS], hz }
    }

    fn desc(width: u32, height: u32, format: i32) -> AHardwareBufferDesc {
        AHardwareBufferDesc {
            width,
            height,
            layers: 1,
            format: format as u32,
            usage: AHB_USAGE_WRITE_ONLY,
            ..Default::default()
        }
    }
}

fn rect(r: &xhd::Rect) -> ARect {
    ARect { left: r.x as i32, top: r.y as i32, right: (r.x + r.w) as i32, bottom: (r.y + r.h) as i32 }
}

impl xhd::present::Backend for Ahb {
    fn supported(&mut self, width: u32, height: u32, format: i32) -> bool {
        let d = Ahb::desc(width, height, format);
        // SAFETY: a filled descriptor, read only.
        unsafe { AHardwareBuffer_isSupported(&d) == 1 }
    }

    fn open_child(&mut self) -> Result<(), String> {
        // SAFETY: the layer's live window; a NUL-terminated name.
        self.sc = unsafe { ASurfaceControl_createFromWindow(self.window, c"OneiOS frame".as_ptr()) };
        if self.sc.is_null() {
            return Err("ASurfaceControl_createFromWindow gave none".into());
        }
        Ok(())
    }

    fn allocate(&mut self, slot: usize, width: u32, height: u32, format: i32) -> Result<u32, i32> {
        let d = Ahb::desc(width, height, format);
        let mut b = std::ptr::null_mut();
        // SAFETY: a filled descriptor; b is written by the call.
        let error = unsafe { AHardwareBuffer_allocate(&d, &mut b) };
        if error != 0 || b.is_null() {
            return Err(if error != 0 { error } else { -libc::ENOMEM });
        }
        self.buffers[slot] = b;
        let mut got = AHardwareBufferDesc::default();
        // SAFETY: a live buffer; got is written by the call.
        unsafe { AHardwareBuffer_describe(b, &mut got) };
        Ok(got.stride)
    }

    fn lock(&mut self, slot: usize) -> Result<*mut u8, i32> {
        let mut bits = std::ptr::null_mut();
        // SAFETY: a live buffer of ours (allocated, not released); whole (no rectangle), no fence to wait for (the
        // present loop waited for the compositor's release fence itself).
        let error = unsafe {
            AHardwareBuffer_lock(self.buffers[slot], AHB_USAGE_CPU_WRITE_RARELY, -1, std::ptr::null(), &mut bits)
        };
        if error != 0 {
            return Err(error);
        }
        Ok(bits as *mut u8)
    }

    fn unlock(&mut self, slot: usize) -> Result<Option<OwnedFd>, i32> {
        let mut fence = -1;
        // SAFETY: the buffer locked above; fence is written by the call.
        let error = unsafe { AHardwareBuffer_unlock(self.buffers[slot], &mut fence) };
        // SAFETY: a fence the NDK handed over (or none).
        let fence = (fence >= 0).then(|| unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fence) });
        if error != 0 {
            return Err(error);
        }
        Ok(fence)
    }

    fn post(
        &mut self,
        slot: usize,
        acquire: Option<OwnedFd>,
        damage: xhd::Rect,
        first: Option<i32>,
        done: xhd::present::Done,
    ) -> Result<(), i32> {
        // SAFETY: the NDK's constructor; checked below.
        let t = unsafe { ASurfaceTransaction_create() };
        if t.is_null() {
            return Err(-libc::ENOMEM);
        }
        let area = rect(&damage);
        let context = Box::into_raw(Box::new(Completion { sc: self.sc, done })) as *mut c_void;
        // SAFETY: a live transaction, child and buffer; the transaction takes the acquire fence (setBuffer) and the
        // completion's context (taken back once in `completed`).
        unsafe {
            ASurfaceTransaction_setBuffer(t, self.sc, self.buffers[slot], acquire.map_or(-1, IntoRawFd::into_raw_fd));
            ASurfaceTransaction_setDamageRegion(t, self.sc, &area, 1);
            if let Some(dataspace) = first {
                ASurfaceTransaction_setVisibility(t, self.sc, VISIBILITY_SHOW);
                ASurfaceTransaction_setBufferTransparency(t, self.sc, TRANSPARENCY_OPAQUE);
                ASurfaceTransaction_setBufferDataSpace(t, self.sc, dataspace);
                if self.hz > 0 {
                    ASurfaceTransaction_setFrameRate(t, self.sc, self.hz as f32, 0);
                }
            }
            ASurfaceTransaction_setOnComplete(t, context, completed);
            ASurfaceTransaction_apply(t);
            ASurfaceTransaction_delete(t);
        }
        Ok(())
    }

    fn close(&mut self) {
        if !self.sc.is_null() {
            // SAFETY: a live child; hidden and taken off the layer in one transaction, then our reference let go.
            unsafe {
                let t = ASurfaceTransaction_create();
                if !t.is_null() {
                    ASurfaceTransaction_setVisibility(t, self.sc, VISIBILITY_HIDE);
                    ASurfaceTransaction_reparent(t, self.sc, std::ptr::null_mut());
                    ASurfaceTransaction_apply(t);
                    ASurfaceTransaction_delete(t);
                }
                ASurfaceControl_release(self.sc);
            }
            self.sc = std::ptr::null_mut();
        }
        for b in self.buffers.iter_mut() {
            if !b.is_null() {
                // SAFETY: our reference, let go once (a buffer still in the compositor keeps its own).
                unsafe { AHardwareBuffer_release(*b) };
                *b = std::ptr::null_mut();
            }
        }
    }
}

// ---- the run loop -----------------------------------------------------------------------------------------------

impl run::Layer for Window {
    fn set_geometry(&self, width: u32, height: u32, format: i32) {
        Window::set_geometry(self, width, height, format)
    }
    fn set_dataspace(&self, dataspace: i32) -> i32 {
        Window::set_dataspace(self, dataspace)
    }
    fn dataspace(&self) -> i32 {
        Window::dataspace(self)
    }
    fn lock(&self, width: u32, height: u32) -> Result<run::Locked, i32> {
        Window::lock(self, width, height)
    }
    fn unlock_and_post(&self, _changed: &xhd::Rect) {
        Window::unlock_and_post(self)
    }
}

/// Posts frames until the socket closes (0) or a signal asks to stop (1); negative on an error (xhd::run: a reader
/// thread that reads each frame into the snapshot and answers it, and this thread, which posts).
///
/// ready_path: SESSION/ready (--session), or null (--socket): the first post waits for it (xhd::hold_first). Until the
/// first post the layer has no buffer and the OneOS boot screen under it shows.
#[no_mangle]
pub extern "C" fn Java_OneiOSDisplay_nRun(
    env: Env,
    _type: Jobject,
    handle: Jlong,
    surface: Jobject,
    format: Jint,
    dataspace: Jint,
    ready_path: Jstring,
) -> Jint {
    // SAFETY: the Java half passes the handle nOpen gave it.
    let c = unsafe { context(handle) };
    let Some(f) = c.frame.as_ref() else { return -1 };
    let Some(window) = Window::from_surface(env, surface) else {
        eprintln!("oneios-display: no native window for the layer");
        return -1;
    };
    let ready = java_string(env, ready_path).map(|p| PathBuf::from(std::ffi::OsStr::from_bytes(&p)));
    let signal = SIGNAL_READ.load(Ordering::SeqCst);
    // SAFETY: the signal pipe's read end is open for the whole process (install_signals); with no pipe (-1) only the
    // socket is polled.
    let signal_fd = (signal >= 0).then(|| unsafe { BorrowedFd::borrow_raw(signal) });
    // display-present (xnuhost's options, or ONEIOS_DISPLAY_PRESENT): write-only buffers on a child of the layer, else
    // the window; xhd::present chooses at the first post and falls back to the window by itself.
    let raw = window.0;
    let env = std::env::var(xhd::present::ENV).ok();
    let present = xhd::present::Present::new(window, Ahb::new(raw, c.hz), f.format, env);
    let end = run::run(c.fd.as_fd(), f, &mut c.direct, &present, format, dataspace, ready, signal_fd, &stopping);
    drop(present); // the child hidden and let go, then the window
    end as Jint
}
