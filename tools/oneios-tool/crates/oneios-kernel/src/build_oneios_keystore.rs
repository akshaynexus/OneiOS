//! `oneios-tool kernel gen keystore` (was kernel/patches/oneios-hosted/keystore/build-oneios-keystore.py).
//!
//! The hosted kernel's key store archive, built between the two kernel passes from the compile records of the first:
//!
//!   build/oneios-keystore/liboneioskeystore.a   OneiOSKeyStore.cpp (the patch folder: oneios_keystore_start, the
//!                                               class keys, wrap and unwrap) and UniXNU's AppleKeyStoreState
//!                                               (NativeKeyStoreEvents.cpp: the AppleKeyStore service and its user client)
//!
//! Why: the phone has no Secure Enclave and AppleSEPKeyStore is left out of kernel.kc, so launchd finds no AppleKeyStore
//! service and MobileKeyBag cannot load a system keybag (README.md of the patch folder). Not a security boundary.
//!
//! NativeKeyStoreEvents.cpp is compiled from a staged copy with two lines added, as Droid-VM's
//! build-droidvm-legacy-display.py does for its KeyStoreWrap.inc: the declaration of oneios_keystore_wrap, and
//! selectors 10 and 11 (aks_wrap_key, aks_unwrap_key) sent to it. The component's classes subclass IOService and
//! IOUserClient of the kernel itself, so unlike the software AES no binding to the kernel collection is generated.
//! Its static initializer is taken out of __mod_init_func and called by oneios_keystore_start inside a module load.
//!
//! Force-loaded by CONFIG_XNU_EXTRA_LD_ARCHIVES and started from oneios_board_late_start (0022-keystore.patch).
//! kernel/build-hosted.sh adds it to the board's archives of the build's device repository.
//! Run by UniXNU's driver in the build tree, with UNIXNU_DEVICE = the device repository. No Apple file is read.

use crate::gen::{self, Compile, Ctx};
use anyhow::Result;
use std::path::{Path, PathBuf};

const REQUIRED: [&str; 6] = ["_oneios_keystore_start", "_oneios_keystore_wrap", "__GLOBAL__sub_I_NativeKeyStoreEvents.cpp",
                             "_native_keyevents_allocate", "_native_aks_encode_state", "__ZN13AppleKeyStore10gMetaClassE"];
pub const EDITS: [(&str, &str); 2] = [
    ("namespace {\n",
     "extern \"C\" IOReturn oneios_keystore_wrap(uint32_t selector, IOExternalMethodArguments *args);\nnamespace {\n"),
    ("        default: break;\n",
     "        case 10: case 11: rc = oneios_keystore_wrap(selector, args); break;\n        default: break;\n"),
];

pub fn main() -> Result<()> {
    let ctx = Ctx::new("build-oneios-keystore", "run through the kernel build")?;
    let here = ctx.repo.join("kernel/patches/oneios-hosted/keystore");
    let component = ctx.repo.join("external/Droid-VM-UniXNU/drivers/AppleKeyStoreState/NativeKeyStoreEvents.cpp");
    gen::need_files(&[ctx.repo.join("drivers/build.py"), component.clone(), ctx.records.join("iokit/DEVELOPMENT/IOPlatformExpert.cpo.json"),
                      here.join("OneiOSKeyStore.cpp")],
                    |path| format!("build-oneios-keystore: {} missing (external/Droid-VM-UniXNU: git submodule update --init drivers/AppleKeyStoreState)",
                                   path.display()))?;
    let out = ctx.tree.join("build/oneios-keystore");
    gen::mkdirs(&out)?;
    let record = ctx.record("iokit")?;
    let compile = Compile { ctx: &ctx, shown: 6000 };
    let mut objects = Vec::new();
    for source in [here.join("OneiOSKeyStore.cpp"), staged_component(&component, &out)?] {
        objects.push(compile.object(&record, &source, &out.join(gen::stem(&source) + ".o"), &[])?);
    }
    // The component's static initializer registers its two classes: it must run inside the board's module load, not
    // with the kernel's own initializers. Out of __mod_init_func, and global so OneiOSKeyStore.cpp can call it.
    let late = out.join("NativeKeyStoreEvents.late.o");
    gen::late_object(&gen::which("llvm-objcopy", "objcopy"), &out.join("NativeKeyStoreEvents.o"), &late, &["__DATA,__mod_init_func"],
                     "__GLOBAL__sub_I_NativeKeyStoreEvents.cpp")?;
    objects[1] = late;
    let archive = out.join("liboneioskeystore.a");
    gen::archive(&archive, &objects)?;
    gen::require_definitions(&ctx, &archive, &REQUIRED)?;
    gen::write_objects(&ctx, &out, Some("iokit"), &[here.join("OneiOSKeyStore.cpp"), component])?;
    println!("build-oneios-keystore: {} ({} objects)", archive.display(), objects.len());
    Ok(())
}

/// NativeKeyStoreEvents.cpp with the two lines added.
pub fn staged_text(text: String) -> Result<String> {
    gen::replace_once(text, &EDITS, |anchor| format!("build-oneios-keystore: NativeKeyStoreEvents.cpp no longer has exactly one {}", anchor.trim()))
}

fn staged_component(component: &Path, out: &Path) -> Result<PathBuf> {
    let staged = out.join("NativeKeyStoreEvents.cpp"); // the same file name: the static initializer is named after it
    gen::write_text(&staged, &staged_text(gen::read_text(component)?)?)?;
    Ok(staged)
}
