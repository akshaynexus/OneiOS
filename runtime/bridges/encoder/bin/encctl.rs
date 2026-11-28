// SPDX-License-Identifier: MIT
//! encctl: the video encoder bridge's phone test ([`oneencoder::encctl`]).
use std::io::Write;

fn main() {
    let code = oneencoder::encctl::main();
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    // Android 16's libmediandk.so aborts inside Scudo during __cxa_finalize
    // (std::vector<AMediaCodecInfo>::~vector -> ACodecVideoCapabilities) whenever
    // AMediaCodecStore_findNextEncoderForFormat was called. Skip atexit destructors on Android.
    #[cfg(target_os = "android")]
    // SAFETY: stdout/stderr are flushed above; _exit avoids libmediandk.so's broken static destructor.
    unsafe {
        libc::_exit(code);
    }
    #[cfg(not(target_os = "android"))]
    std::process::exit(code);
}
