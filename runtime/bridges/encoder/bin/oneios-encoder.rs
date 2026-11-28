// SPDX-License-Identifier: MIT
//! bin/oneios-encoder: the video encoder helper process ([`oneencoder::helper`]).
use std::io::Write;

fn main() {
    let code = oneencoder::helper::main();
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
