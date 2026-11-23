//! The Metal plug-in build's own steps (userland/metal/build.sh), which were inline Python until 2026-11-21:
//! `stage-edit` (the staged copy's changes against upstream, each said), `record` (NativeAdrenoMetal.build.json)
//! and `gpuaddon` (the bundle as the kernel.kc.gpuaddon sidecar). The kernel base's build scripts that build.sh
//! runs (Droid-VM-UniXNU's build-turnip-darwin.py, build-adreno-metal-bridge.py, build-adreno-metal-plugin.py)
//! are that project's Python, not ours.

use anyhow::{anyhow, bail, Context, Result};
use oneios_json::{Map, Value};
use std::io::Write;
use std::path::Path;

/// Replace the one `old` of `path` (under `root`) by `new`, for each pair, and say why.
fn edit(root: &Path, path: &str, pairs: &[(&str, String, &str)]) -> Result<()> {
    let file = root.join(path);
    let mut text = std::fs::read_to_string(&file).with_context(|| file.display().to_string())?;
    for (old, new, why) in pairs {
        if text.matches(old).count() != 1 {
            bail!("{path} no longer has exactly one {old:?}");
        }
        text = text.replacen(old, new, 1);
        println!("metal: staged {path}: {why}");
    }
    std::fs::write(&file, text)?;
    Ok(())
}

fn is_hex(c: char) -> bool {
    c.is_ascii_hexdigit()
}

/// Every `from` of `text` that no hex digit follows, replaced by `to` (Python's `re.subn(re.escape(from) +
/// r'(?![0-9a-fA-F])', to, text)`). Returns the text and the count.
pub fn replace_address(text: &str, from: &str, to: &str) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let (mut rest, mut n) = (text, 0);
    while let Some(i) = rest.find(from) {
        let after = &rest[i + from.len()..];
        out.push_str(&rest[..i]);
        if after.chars().next().is_some_and(is_hex) {
            out.push_str(from);
        } else {
            out.push_str(to);
            n += 1;
        }
        rest = after;
    }
    out.push_str(rest);
    (out, n)
}

fn s<'a>(v: &'a serde_json::Value, key: &str) -> Result<&'a str> {
    v.get(key).and_then(|x| x.as_str()).ok_or_else(|| anyhow!("the hooks file: no string {key:?} in {v}"))
}

/// The line of build-turnip-darwin.py that writes the meson cross file's compile and link arguments.
const TURNIP_CROSS_ARGS: &str = r"''.join('%s = %r\n' % (k, flags) for k in ('c_args', 'cpp_args', 'objc_args', 'c_link_args',";
/// The same line with `-D_DARWIN_C_SOURCE` in the three compile arguments (not the link ones).
const TURNIP_CROSS_ARGS_DARWIN: &str =
    r"''.join('%s = %r\n' % (k, flags + ([] if 'link' in k else ['-D_DARWIN_C_SOURCE'])) for k in ('c_args', 'cpp_args', 'objc_args', 'c_link_args',";
/// Why. flex 2.6.10 (Arch, 2026-11) writes `#define _POSIX_C_SOURCE 1` into every scanner it makes (for
/// fileno()); flex 2.6.4 (macOS's /usr/bin/flex, and the laptop's until 2026-11) does not. With _POSIX_C_SOURCE and
/// no _DARWIN_C_SOURCE, Darwin's sys/cdefs.h lowers __DARWIN_C_LEVEL, and dlfcn.h hides Dl_info and dladdr, which
/// Mesa's disk_cache.h (included by ir3_lexer.c) uses. _DARWIN_C_SOURCE keeps the full Darwin level: what every
/// file that defines no POSIX level gets anyway, so the other files compile as before.
const TURNIP_CROSS_ARGS_WHY: &str =
    "Mesa's cross c/cpp/objc args get -D_DARWIN_C_SOURCE (flex 2.6.10's scanners define _POSIX_C_SOURCE, which hides Darwin's Dl_info from ir3_lexer.c)";

/// `ios metal-stage-edit ROOT HOOKS JOBS KNOWS [--rt]`: what build.sh's `stage` changes in the staged copy ROOT. With `rt`, the
/// plug-in script links the bridge on metal2vulkan-rt (userland/metal/rt/build.sh ios builds it into the place the script
/// reads) instead of building the old one.
pub fn stage_edit(root: &Path, hooks: &Path, jobs: &str, knows: bool, rt: bool) -> Result<()> {
    let hooks: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(hooks).with_context(|| hooks.display().to_string())?).with_context(|| hooks.display().to_string())?;
    let pins = root.join("scripts/data/adreno-compiler-inputs.json");
    let mut source = oneios_json::read(&pins.to_string_lossy())?;
    match &mut source {
        Value::Object(m) => match m.get_mut("patches") {
            Some(Value::Array(a)) => a.push(Value::String("patches/mesa/trace-turnip-init.patch".into())),
            _ => bail!("{}: no list \"patches\"", pins.display()),
        },
        _ => bail!("{}: not an object", pins.display()),
    }
    std::fs::write(&pins, oneios_json::to_pretty(&source) + "\n")?;

    edit(
        root,
        "scripts/build-turnip-darwin.py",
        &[
            (
                "env.update(CC=find('clang'), CXX=find('clang++'), CFLAGS='', CXXFLAGS='', CPPFLAGS='', LDFLAGS='')",
                "env.update(CC='clang', CXX='clang++', CFLAGS='', CXXFLAGS='', CPPFLAGS='', LDFLAGS='')".into(),
                "meson's build-machine compiler is the host's clang (on a Mac xcrun's clang is both)",
            ),
            ("'-j6'", format!("'-j{jobs}'"), "ninja jobs"),
            (TURNIP_CROSS_ARGS, TURNIP_CROSS_ARGS_DARWIN.into(), TURNIP_CROSS_ARGS_WHY),
        ],
    )?;
    edit(
        root,
        "scripts/build-adreno-metal-bridge.py",
        &[
            ("timeout=1800)", "timeout=10800)".into(), "a longer limit for the fat-LTO Rust build"),
            (
                "\n\ndef git_tree(*steps):",
                "\n\nPATCHES.append(ROOT / 'patches/metal2vulkan/trace-metal-vulkan-init.patch')\nPATCHES.append(ROOT / 'patches/metal2vulkan/m2v-spare-command-pools.patch')\n\n\ndef git_tree(*steps):".into(),
                "record Vulkan initialization diagnostics in the translator patch stack; spare command pools follow their recent use (m2v-spare-command-pools.patch)",
            ),
        ],
    )?;
    let mut plugin_pairs: Vec<(&str, String, &str)> = vec![
        (
            "subprocess.run(['python3', ROOT / 'scripts/gen-turnip-replay.py'], check=True)\n",
            String::new(),
            "no TurnipReplay.h: it is recorded on a Mac's host simulation, and NativeAdrenoUserTest.h includes it only when it exists",
        ),
        (
            "staged_frontend.write_text(frontend)\n",
            "staged_frontend.write_text(frontend)\nsubprocess.run(['patch', '-s', '-F', '0', '-d', OUT, '-p1', '-i', ROOT / 'patches/trace-metal-surfaces.patch'], check=True)\n".into(),
            "instrument the frontend after the upstream replacements",
        ),
    ];
    if rt {
        plugin_pairs.push((
            "subprocess.run(['python3', ROOT / 'scripts/build-adreno-metal-bridge.py', '--target', BRIDGE_TARGET], check=True)\n",
            String::new(),
            "the bridge is metal2vulkan-rt's (userland/metal/rt/build.sh ios put it in build/adreno-metal-bridge/ios27); the old one is not built",
        ));
    }
    if knows {
        plugin_pairs.push((
            "'-Wall', '-Wextra', '-Werror',",
            "'-Wall', '-Wextra', '-Werror', '-Wno-error=unused-but-set-global',".into(),
            "clang 23's -Wunused-but-set-global (gZeroRest), which Xcode's clang does not have, is not an error",
        ));
    }
    edit(root, "scripts/build-adreno-metal-plugin.py", &plugin_pairs)?;

    // The frontend at b0d6a07 has the caller-by-framework answer of supportsFamily: for RenderBox and WebGPU; the
    // base's plug-in script inserts its own, newer one (ANGLE too) before installAnswers and the two collide. The
    // frontend's goes, the script's is inserted.
    let frontend = root.join("drivers/AdrenoMetal/NativeBridgeMetalFrontend.mm");
    let text = std::fs::read_to_string(&frontend).with_context(|| frontend.display().to_string())?;
    let (begin, end) = ("static BOOL (*gFamilyAnswer)(id,SEL,NSInteger);\n", "static void installAnswers(Class cls) {\n");
    if text.matches(begin).count() != 1 || text.matches(end).count() != 1 || text.find(begin) > text.find(end) {
        bail!("NativeBridgeMetalFrontend.mm: the supportsFamily: caller block is not where it was");
    }
    let (b, e) = (text.find(begin).unwrap_or(0), text.find(end).unwrap_or(0));
    std::fs::write(&frontend, format!("{}{}", &text[..b], &text[e..]))?;
    println!("metal: staged NativeBridgeMetalFrontend.mm: its supportsFamily: caller block removed; build-adreno-metal-plugin.py inserts the base's (RenderBox, WebGPU, ANGLE)");
    edit(root, "drivers/AdrenoMetal/NativeBridgeMetalFrontend.mm", &frontend_camera_edits())?;
    edit(root, "drivers/AdrenoMetal/NativeAdrenoMetalPlugin.mm", &pool_row_edits())?;

    // The plug-in's shared cache addresses: upstream's are the iPad17,3 24A446 cache's.
    let plugin = root.join("drivers/AdrenoMetal/NativeAdrenoMetalPlugin.mm");
    let mut text = std::fs::read_to_string(&plugin).with_context(|| plugin.display().to_string())?;
    let anchor = hooks.get("anchor").ok_or_else(|| anyhow!("the hooks file has no anchor"))?;
    let branch = hooks.get("branch").ok_or_else(|| anyhow!("the hooks file has no branch"))?;
    let list = |k: &str| hooks.get(k).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut items = vec![anchor.clone()];
    items.extend(list("replace"));
    for item in &items {
        let (up, value) = (s(item, "upstream")?, s(item, "value")?);
        let (new, n) = replace_address(&text, up, value);
        if n == 0 {
            bail!("NativeAdrenoMetalPlugin.mm has no {up}: the hooks file is for another revision");
        }
        text = new;
        if up != value {
            let what = item.get("what").or_else(|| item.get("symbol")).and_then(|v| v.as_str()).unwrap_or("None");
            println!("metal: staged NativeAdrenoMetalPlugin.mm: {up} -> {value} ({n} places): {what}");
        }
    }
    for fixed in [s(branch, "address")?.to_string(), format!("word != {}", s(branch, "word")?)] {
        if !text.contains(&fixed) {
            bail!("NativeAdrenoMetalPlugin.mm has no {fixed:?}");
        }
    }
    // Every address the arm64e code uses must now be one the hooks file names.
    let mut known = std::collections::BTreeSet::new();
    for item in list("replace").iter().chain(list("same").iter()).chain([anchor]) {
        known.insert(s(item, "value")?.to_string());
    }
    let cut = |from: &str, to: &str, after: usize| -> Result<(usize, usize)> {
        let a = text.find(from).ok_or_else(|| anyhow!("NativeAdrenoMetalPlugin.mm has no {from:?}"))?;
        let b = a.max(after);
        let z = text[b..].find(to).map(|i| i + b).ok_or_else(|| anyhow!("NativeAdrenoMetalPlugin.mm has no {to:?} after {from:?}"))?;
        Ok((a, z))
    };
    let (a1, z1) = cut("setenv(\"CA_DISABLE_FRAMEBUFFER_COMPRESSION\"", "#endif", 0)?;
    let a2 = text.find("static unsigned patchFramebufferBranch").ok_or_else(|| anyhow!("NativeAdrenoMetalPlugin.mm has no patchFramebufferBranch"))?;
    let z2 = text.find("static unsigned gReports").ok_or_else(|| anyhow!("NativeAdrenoMetalPlugin.mm has no gReports"))?;
    let arm64e_only = format!("{}{}", &text[a1..z1], text.get(a2..z2).unwrap_or(""));
    let re = regex::Regex::new(r"0x1[0-9a-f]{8}\b").expect("the pattern");
    let unknown: std::collections::BTreeSet<&str> = re.find_iter(&arm64e_only).map(|m| m.as_str()).filter(|a| !known.contains(*a)).collect();
    if !unknown.is_empty() {
        bail!("shared cache addresses in the arm64e code that the hooks file does not name: {}", unknown.into_iter().collect::<Vec<_>>().join(", "));
    }
    std::fs::write(&plugin, text)?;
    Ok(())
}

/// What the camera's pipeline (cameracaptured: CMCapture, CMImaging) needs of the frontend at b0d6a07, as text edits of
/// the staged copy (fix-camera-preview-stream, 2026-11-23; docs/gpu.md, "What the camera needs"):
/// - MTLResourceOptions bits that are neither Metal's three fields (bits 0-9) nor its private bit 24 are taken off
///   before the runtime sees them, once said per set of bits: metal2vulkan refuses such a word whole (UndefinedValue)
///   and FigMetalAllocator's heaps set bits 21 and 22 (0x600200, "native-metal: heap refused", then
///   `Fig assert: "_heap"` and cameracaptured's SIGSEGV two seconds later, phone 2026-11-23);
/// - NBLibrary answers newFunctionWithName:constantValues:pipelineLibrary:error: (and its completionHandler: form):
///   CMImaging's -[FigMetalContext computePipelineStateFor:constants:additionalPipelineOptions:reflection:] asks it,
///   and the unknown selector's nil was `Fig assert: "function" ... function is nil` (FigMetalContext.m:565).
fn frontend_camera_edits() -> Vec<(&'static str, String, &'static str)> {
    vec![
        (
            "static NSString *lastError() {return [NSString stringWithUTF8String:nmb_last_error()];}\n",
            concat!(
                "static NSString *lastError() {return [NSString stringWithUTF8String:nmb_last_error()];}\n",
                "// OneiOS (oneios-tool ios metal-stage-edit): MTLResourceOptions bits outside Metal's three fields (CPU cache 0-3, storage\n",
                "// 4-7, hazard tracking 8-9) and its private bit 24 are taken off: the runtime refuses such a word whole, and CMCapture's\n",
                "// FigMetalAllocator sets bits 21 and 22 on its heaps (0x600200, iOS 27 24A446). Each new set of bits is said once.\n",
                "static uint64_t nbKnownOptions(uint64_t raw,const char *what) {\n",
                " const uint64_t known=0x3ffull|(1ull<<24),extra=raw&~known;\n",
                " if(extra) {\n",
                "  static std::atomic<uint64_t> told{0};\n",
                "  if((told.fetch_or(extra)&extra)!=extra)\n",
                "   NBLOG(@\"native-metal: %s options 0x%llx: bits 0x%llx are not Metal's known ones; made without them\",what,(unsigned long long)raw,\n",
                "         (unsigned long long)extra);\n",
                " }\n",
                " return raw&known;\n",
                "}\n"
            )
            .into(),
            "unknown MTLResourceOptions bits taken off (nbKnownOptions): FigMetalAllocator's heaps (0x600200)",
        ),
        (
            "(uint64_t)d.usage,(uint64_t)d.resourceOptions,",
            "(uint64_t)d.usage,nbKnownOptions((uint64_t)d.resourceOptions,\"texture\"),".into(),
            "a texture descriptor's options through nbKnownOptions",
        ),
        (
            " NmbHeap *handle=nmb_heap_new(gDevice,d.type,d.size,(uint64_t)d.resourceOptions);\n",
            " const uint64_t heapOptions=nbKnownOptions((uint64_t)d.resourceOptions,\"heap\");\n NmbHeap *handle=nmb_heap_new(gDevice,d.type,d.size,heapOptions);\n".into(),
            "a heap's options through nbKnownOptions",
        ),
        ("heap.options=d.resourceOptions;", "heap.options=(MTLResourceOptions)heapOptions;".into(), "the heap reports the options it was made with"),
        (
            " NmbBuffer *handle=nmb_heap_new_buffer(_handle,length,options,offset);\n",
            " options=(MTLResourceOptions)nbKnownOptions(options,\"heap buffer\");\n NmbBuffer *handle=nmb_heap_new_buffer(_handle,length,options,offset);\n".into(),
            "a heap buffer's options through nbKnownOptions",
        ),
        (
            " NmbBuffer *handle=nmb_buffer_new(gDevice,length,options,nullptr);\n",
            " options=(MTLResourceOptions)nbKnownOptions(options,\"buffer\");\n NmbBuffer *handle=nmb_buffer_new(gDevice,length,options,nullptr);\n".into(),
            "a buffer's options through nbKnownOptions",
        ),
        (
            " NmbBuffer *handle=nmb_buffer_new(gDevice,length,options,bytes);\n",
            " options=(MTLResourceOptions)nbKnownOptions(options,\"buffer\");\n NmbBuffer *handle=nmb_buffer_new(gDevice,length,options,bytes);\n".into(),
            "a buffer of bytes: its options through nbKnownOptions",
        ),
        (
            "nmb_buffer_new_no_copy_releasing(gDevice,pointer,length,options,",
            "nmb_buffer_new_no_copy_releasing(gDevice,pointer,length,nbKnownOptions(options,\"no-copy buffer\"),".into(),
            "a no-copy buffer's options through nbKnownOptions",
        ),
        (
            "nmb_device_heap_buffer_size_and_align(gDevice,length,(uint64_t)options,answer)",
            "nmb_device_heap_buffer_size_and_align(gDevice,length,nbKnownOptions((uint64_t)options,\"heap buffer size\"),answer)".into(),
            "heapBufferSizeAndAlignWithLength:options: through nbKnownOptions",
        ),
        (
            concat!(
                "- (id<MTLFunction>)newFunctionWithName:(NSString *)name constantValues:(MTLFunctionConstantValues *)values error:(NSError **)error {\n",
                " return [self nb_newFunctionWithName:name constantValues:values zeroRest:NO error:error];\n",
                "}\n"
            ),
            concat!(
                "- (id<MTLFunction>)newFunctionWithName:(NSString *)name constantValues:(MTLFunctionConstantValues *)values error:(NSError **)error {\n",
                " return [self nb_newFunctionWithName:name constantValues:values zeroRest:NO error:error];\n",
                "}\n",
                "// OneiOS (oneios-tool ios metal-stage-edit): the form with a pipeline library, which CMImaging's FigMetalContext asks for\n",
                "// every camera pipeline (unanswered: \"function is nil\"). The library holds compiled pipelines; this runtime compiles its\n",
                "// own, so the function is the one without it.\n",
                "- (id<MTLFunction>)newFunctionWithName:(NSString *)name constantValues:(MTLFunctionConstantValues *)values pipelineLibrary:(id)library\n",
                "                                 error:(NSError **)error {\n",
                " (void)library;return [self newFunctionWithName:name constantValues:values error:error];\n",
                "}\n",
                "- (void)newFunctionWithName:(NSString *)name constantValues:(MTLFunctionConstantValues *)values pipelineLibrary:(id)library\n",
                "          completionHandler:(void (^)(id<MTLFunction>,NSError *))handler {\n",
                " (void)library;NSError *error=nil;id<MTLFunction> function=[self newFunctionWithName:name constantValues:values error:&error];\n",
                " if(handler)handler(function,error);\n",
                "}\n"
            )
            .into(),
            "NBLibrary answers newFunctionWithName:constantValues:pipelineLibrary:error: (CMImaging's FigMetalContext)",
        ),
    ]
}

fn first_line(cmd: &[&str], stderr: bool) -> String {
    let Some((prog, args)) = cmd.split_first() else { return String::new() };
    match std::process::Command::new(prog).args(args).output() {
        Ok(o) => String::from_utf8_lossy(if stderr { &o.stderr } else { &o.stdout }).lines().next().unwrap_or("").trim().to_string(),
        Err(_) => String::new(),
    }
}

fn git_head(path: &Path) -> String {
    match std::process::Command::new("git").arg("-C").arg(path).args(["rev-parse", "HEAD"]).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => String::new(),
    }
}

fn env(k: &str) -> Result<String> {
    std::env::var(k).map_err(|_| anyhow!("{k} is not set (scripts/ios/env.sh sets it)"))
}

/// `ios metal-record ROOT BUNDLE RECORD HOOKS TARGET`: NativeAdrenoMetal.build.json, the SDK, the stubs, the pins,
/// the sizes and the CDHashes (the environment is scripts/ios/env.sh's).
pub fn record(root: &Path, bundle: &Path, record: &Path, hooks: &Path, target: &str) -> Result<()> {
    let mut files = Map::new();
    for name in ["NativeAdrenoMetal", "libvulkan_freedreno.dylib"] {
        let path = bundle.join(name);
        let data = std::fs::read(&path).with_context(|| path.display().to_string())?;
        let info = oneios_tweaks::macho::describe(&data).map_err(|e| anyhow!("{}: {e}", path.display()))?.into_iter().next().ok_or_else(|| anyhow!("{}: no slice", path.display()))?;
        let mut f = Map::new();
        f.insert("bytes".into(), Value::Number(data.len().into()));
        f.insert("cdhash".into(), Value::String(info.cdhash));
        f.insert("arch".into(), Value::String(info.arch));
        f.insert("page_size".into(), Value::Number(info.page_size.into()));
        f.insert("identifier".into(), Value::String(info.identifier));
        f.insert("minos".into(), info.minos.map(Value::String).unwrap_or(Value::Null));
        files.insert(name.into(), Value::Object(f));
    }
    let mut cdhashes: Vec<String> = files.values().filter_map(|f| if let Value::Object(m) = f { m.get("cdhash").map(Value::text) } else { None }).collect();
    cdhashes.sort();
    let tbd = Path::new(&env("IOS_TBD")?).join("tbd.json");
    let stubs = if tbd.exists() { oneios_json::read(&tbd.to_string_lossy())? } else { Value::Null };
    let mut sdk = Map::new();
    sdk.insert("path".into(), Value::String(env("IOS_SDK")?));
    sdk.insert("version".into(), Value::String(env("IOS_SDK_VERSION")?));
    let mut sources = Map::new();
    for name in ["NativeMetal", "metal2vulkan", "mesa-droidvm", "Droid-VM-UniXNU", "NativeAdreno", "AdrenoPM4"] {
        sources.insert(name.into(), Value::String(git_head(&root.join("external").join(name))));
    }
    let ld_version = std::env::var("IOS_LD_VERSION").unwrap_or_default();
    let mut r = Map::new();
    r.insert("bundle".into(), Value::String("NativeAdrenoMetal.bundle".into()));
    r.insert("target".into(), Value::String(target.into()));
    r.insert("files".into(), Value::Object(files.clone()));
    r.insert("cdhashes".into(), Value::Array(cdhashes.into_iter().map(Value::String).collect()));
    r.insert("sdk".into(), Value::Object(sdk));
    r.insert("stubs".into(), stubs);
    r.insert("deployment_target".into(), Value::String(env("IOS_MIN")?));
    r.insert("clang".into(), Value::String(first_line(&[&env("IOS_CLANG")?, "--version"], false)));
    r.insert("ld64".into(), if ld_version.is_empty() { Value::Null } else { Value::String(ld_version) });
    r.insert("linker".into(), Value::String(first_line(&[&env("IOS_LD")?, "-v"], true)));
    r.insert("rustc".into(), Value::String(first_line(&["rustc", "+1.98.0", "--version"], false)));
    r.insert("sources".into(), Value::Object(sources));
    let hooks_rel = hooks.strip_prefix(root).map(|p| p.display().to_string()).unwrap_or_else(|_| hooks.display().to_string());
    r.insert("hooks".into(), Value::String(hooks_rel));
    r.insert("check".into(), Value::String("ios macho-check passed against the stubs above".into()));
    std::fs::write(record, oneios_json::to_pretty(&Value::Object(r)))?;
    println!("metal: built for {target}");
    for (name, f) in &files {
        if let Value::Object(m) = f {
            let g = |k: &str| m.get(k).map(Value::text).unwrap_or_default();
            println!("metal:   {name}: {} bytes, CDHash {} ({}, {}-byte pages)", g("bytes"), g("cdhash"), g("arch"), g("page_size"));
        }
    }
    println!("metal:   SDK {} {}", env("IOS_SDK_VERSION")?, env("IOS_SDK")?);
    Ok(())
}

/// The sidecar's limit in xnuhost.
pub const ADDON_MAX: usize = 64 << 20;

/// A file of a sidecar: its name below /private/var/ngpu, its mode and its bytes.
type AddonFile = (String, u32, Vec<u8>);

fn file_mode(p: &Path) -> Result<u32> {
    use std::os::unix::fs::PermissionsExt;
    Ok(if std::fs::metadata(p).with_context(|| p.display().to_string())?.permissions().mode() & 0o111 != 0 { 0o755 } else { 0o644 })
}

/// The Metal plug-in's files as the kernel's 0031 union wants them: `addons/NativeAdrenoMetal.bundle/...` in
/// os.walk's order, then the `addons/NativeAdrenoMetal.publish` marker.
fn metal_files(bundle: &Path) -> Result<Vec<AddonFile>> {
    let mut files: Vec<AddonFile> = Vec::new();
    fn walk(dir: &Path, rel: &str, out: &mut Vec<AddonFile>) -> Result<()> {
        let mut entries: Vec<_> = std::fs::read_dir(dir).with_context(|| dir.display().to_string())?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        // os.walk's order: this folder's files (by name), then each folder (by name)
        let (mut dirs, mut names) = (Vec::new(), Vec::new());
        for e in entries {
            let p = e.path();
            if p.is_dir() {
                dirs.push(e);
            } else {
                names.push(e);
            }
        }
        for e in names {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let r = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            out.push((format!("addons/NativeAdrenoMetal.bundle/{r}"), file_mode(&p)?, std::fs::read(&p)?));
        }
        for e in dirs {
            let name = e.file_name().to_string_lossy().into_owned();
            walk(&e.path(), &if rel.is_empty() { name } else { format!("{rel}/{name}") }, out)?;
        }
        Ok(())
    }
    walk(bundle, "", &mut files)?;
    files.push(("addons/NativeAdrenoMetal.publish".into(), 0o644, b"1\n".to_vec()));
    Ok(files)
}

/// The capture plug-in as the kernel's 0090 wants it: `camera/MediaCapture/H9ISP.mediacapture`, from DIR (the
/// output folder of userland/camera/build.sh) or from the file itself.
fn camera_files(dir: &Path) -> Result<Vec<AddonFile>> {
    let file = if dir.is_dir() { dir.join("H9ISP.mediacapture") } else { dir.to_path_buf() };
    if file.file_name().and_then(|n| n.to_str()) != Some("H9ISP.mediacapture") {
        bail!("{}: not H9ISP.mediacapture (or a folder that holds it)", file.display());
    }
    let data = std::fs::read(&file).with_context(|| format!("{}: run userland/camera/build.sh first", file.display()))?;
    Ok(vec![("camera/MediaCapture/H9ISP.mediacapture".into(), file_mode(&file)?, data)])
}

/// The files as an NGPUADD2 sidecar (Droid-VM-UniXNU's make-gpu-addon.py's format): "NGPUADD2", u32 count, u32 0,
/// u64 size; per file u32 name length (with its NUL), u32 mode, u64 size, u64 packed size, the name and the zlib
/// stream each padded to 8. Refused over xnuhost's limit or with a name the kernel would refuse.
fn pack(files: &[AddonFile]) -> Result<Vec<u8>> {
    let pad = |v: &mut Vec<u8>| {
        let n = (8 - v.len() % 8) % 8;
        v.extend(std::iter::repeat_n(0u8, n));
    };
    let mut body = Vec::new();
    for (name, mode, data) in files {
        let mut encoded = name.as_bytes().to_vec();
        encoded.push(0);
        if encoded.len() > 160 {
            bail!("{name}: a name of over 159 bytes");
        }
        if name.starts_with('/') || name.contains("..") {
            bail!("{name}: the kernel takes only names below /private/var/ngpu without \"..\"");
        }
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(9));
        z.write_all(data)?;
        let mut packed = z.finish()?;
        body.extend((encoded.len() as u32).to_le_bytes());
        body.extend(mode.to_le_bytes());
        body.extend((data.len() as u64).to_le_bytes());
        body.extend((packed.len() as u64).to_le_bytes());
        pad(&mut encoded);
        pad(&mut packed);
        body.extend(encoded);
        body.extend(packed);
    }
    let mut blob = b"NGPUADD2".to_vec();
    blob.extend((files.len() as u32).to_le_bytes());
    blob.extend(0u32.to_le_bytes());
    blob.extend(((24 + body.len()) as u64).to_le_bytes());
    blob.extend(body);
    if blob.len() > ADDON_MAX {
        bail!("{} bytes: over xnuhost's 64 MiB limit for the sidecar", blob.len());
    }
    Ok(blob)
}

/// `ios gpuaddon BUNDLE OUTPUT`: the Metal bundle alone as the boot addon (byte for byte what it was before
/// `ios addon`).
pub fn gpuaddon(bundle: &Path, output: &Path) -> Result<()> {
    let files = metal_files(bundle)?;
    let blob = pack(&files)?;
    std::fs::write(output, &blob).with_context(|| output.display().to_string())?;
    println!("metal: {}: {} files, {} bytes", output.display(), files.len(), blob.len());
    Ok(())
}

/// `ios addon OUTPUT [--metal BUNDLE] [--camera DIR]`: the boot addon (the kernel.kc.gpuaddon sidecar) with the Metal
/// plug-in under addons/ (hosted 0031's union), the capture plug-in under camera/ (hosted 0090's), or both; and
/// OUTPUT.cdhashes, one `CDHASH NAME` line for each Mach-O file in it: the CDHashes the image's owner-trust.json must
/// list (`oneios-tool phone addon OUTPUT` adds them).
pub fn addon(output: &Path, metal: Option<&Path>, camera: Option<&Path>) -> Result<()> {
    if metal.is_none() && camera.is_none() {
        bail!("ios addon OUTPUT --metal BUNDLE and/or --camera DIR: nothing to put in it");
    }
    let mut files = Vec::new();
    if let Some(bundle) = metal {
        files.extend(metal_files(bundle)?);
    }
    let metal_count = files.len();
    if let Some(dir) = camera {
        files.extend(camera_files(dir)?);
    }
    let mut cdhashes = String::new();
    for (name, _, data) in &files {
        if let Ok(slices) = oneios_tweaks::macho::describe(data) {
            for s in slices {
                cdhashes += &format!("{} {name}\n", s.cdhash);
            }
        }
    }
    let blob = pack(&files)?;
    std::fs::write(output, &blob).with_context(|| output.display().to_string())?;
    let list = format!("{}.cdhashes", output.display());
    std::fs::write(&list, &cdhashes).with_context(|| list.clone())?;
    println!(
        "addon: {}: {} files ({} of the Metal plug-in under addons/, {} of the capture plug-in under camera/), {} bytes; {} CDHashes in {list}",
        output.display(),
        files.len(),
        metal_count,
        files.len() - metal_count,
        blob.len(),
        cdhashes.lines().count()
    );
    Ok(())
}


/// The files of a sidecar: (name, mode, the unpacked bytes). The reader the tests check `gpuaddon` with.
pub fn read_gpuaddon(blob: &[u8]) -> Result<Vec<(String, u32, Vec<u8>)>> {
    use std::io::Read;
    let u32_at = |at: usize| blob.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap_or_default())).ok_or_else(|| anyhow!("cut at {at}"));
    let u64_at = |at: usize| blob.get(at..at + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap_or_default())).ok_or_else(|| anyhow!("cut at {at}"));
    if blob.get(..8) != Some(b"NGPUADD2".as_slice()) {
        bail!("not NGPUADD2");
    }
    let count = u32_at(8)? as usize;
    if u64_at(16)? as usize != blob.len() {
        bail!("the size field is not the length");
    }
    let mut at = 24;
    let mut out = Vec::new();
    let up8 = |n: usize| n.div_ceil(8) * 8;
    for _ in 0..count {
        let (nlen, mode, size, plen) = (u32_at(at)? as usize, u32_at(at + 4)?, u64_at(at + 8)? as usize, u64_at(at + 16)? as usize);
        at += 24;
        let name = blob.get(at..at + nlen - 1).ok_or_else(|| anyhow!("cut"))?;
        at += up8(nlen);
        let packed = blob.get(at..at + plen).ok_or_else(|| anyhow!("cut"))?;
        at += up8(plen);
        let mut data = Vec::new();
        flate2::read::ZlibDecoder::new(packed).read_to_end(&mut data)?;
        if data.len() != size {
            bail!("a file's size field is wrong");
        }
        out.push((String::from_utf8_lossy(name).into_owned(), mode, data));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_not_followed_by_a_hex_digit() {
        let (t, n) = replace_address("a 0x1e7021014; b 0x1e70210145 c 0x1e7021014)", "0x1e7021014", "0x1e7025014");
        assert_eq!(t, "a 0x1e7025014; b 0x1e70210145 c 0x1e7025014)");
        assert_eq!(n, 2);
        assert_eq!(replace_address("none", "0x1", "0x2"), ("none".into(), 0));
        assert_eq!(replace_address("0x1", "0x1", "0x2"), ("0x2".into(), 1));
    }

    #[test]
    fn a_sidecar_reads_back() {
        let dir = std::env::temp_dir().join(format!("oneios-ios-addon-{}", std::process::id()));
        let b = dir.join("B.bundle");
        std::fs::create_dir_all(b.join("sub")).unwrap();
        std::fs::write(b.join("Info.plist"), b"<plist/>").unwrap();
        std::fs::write(b.join("bin"), vec![7u8; 100_000]).unwrap();
        std::fs::write(b.join("sub/x"), b"x").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(b.join("bin"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = dir.join("a.gpuaddon");
        gpuaddon(&b, &out).unwrap();
        let blob = std::fs::read(&out).unwrap();
        let files = read_gpuaddon(&blob).unwrap();
        let names: Vec<(&str, u32, usize)> = files.iter().map(|(n, m, d)| (n.as_str(), *m, d.len())).collect();
        assert_eq!(
            names,
            vec![
                ("addons/NativeAdrenoMetal.bundle/Info.plist", 0o644, 8),
                ("addons/NativeAdrenoMetal.bundle/bin", 0o755, 100_000),
                ("addons/NativeAdrenoMetal.bundle/sub/x", 0o644, 1),
                ("addons/NativeAdrenoMetal.publish", 0o644, 2),
            ]
        );
        assert_eq!(files[1].2, vec![7u8; 100_000]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mesa_cross_compile_args_get_darwin_c_source() {
        // the upstream line as Droid-VM-UniXNU's build-turnip-darwin.py has it
        let dir = std::env::temp_dir().join(format!("oneios-ios-turnip-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("s")).unwrap();
        let line = "        ''.join('%s = %r\\n' % (k, flags) for k in ('c_args', 'cpp_args', 'objc_args', 'c_link_args', 'cpp_link_args',\n";
        std::fs::write(dir.join("s/t.py"), line).unwrap();
        edit(&dir, "s/t.py", &[(TURNIP_CROSS_ARGS, TURNIP_CROSS_ARGS_DARWIN.into(), TURNIP_CROSS_ARGS_WHY)]).unwrap();
        let text = std::fs::read_to_string(dir.join("s/t.py")).unwrap();
        assert!(text.contains("(k, flags + ([] if 'link' in k else ['-D_DARWIN_C_SOURCE'])) for k in ('c_args',"));
        assert!(text.ends_with("'cpp_link_args',\n"));
        // a second stage of the same copy finds no upstream line: refused, as every other edit
        assert!(edit(&dir, "s/t.py", &[(TURNIP_CROSS_ARGS, TURNIP_CROSS_ARGS_DARWIN.into(), TURNIP_CROSS_ARGS_WHY)]).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_camera_edits_apply_once_each() {
        let dir = std::env::temp_dir().join(format!("oneios-ios-frontend-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("d")).unwrap();
        let edits = frontend_camera_edits();
        let stand_in: String = edits.iter().map(|(old, _, _)| format!("{old}\n/* between */\n")).collect();
        std::fs::write(dir.join("d/F.mm"), &stand_in).unwrap();
        edit(&dir, "d/F.mm", &edits).unwrap();
        let text = std::fs::read_to_string(dir.join("d/F.mm")).unwrap();
        assert_eq!(text.matches("static uint64_t nbKnownOptions(uint64_t raw,const char *what) {").count(), 1);
        assert_eq!(text.matches("nbKnownOptions(").count(), 8);
        assert!(text.contains("pipelineLibrary:(id)library\n                                 error:(NSError **)error {"));
        assert!(text.contains("heap.options=(MTLResourceOptions)heapOptions;"));
        // a frontend that no longer has an anchor is refused, not built without the fix
        std::fs::write(dir.join("d/F.mm"), "nothing").unwrap();
        assert!(edit(&dir, "d/F.mm", &edits).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_addon_holds_the_camera_plugin_and_the_metal_bundle() {
        let dir = std::env::temp_dir().join(format!("oneios-ios-addon2-{}", std::process::id()));
        let (b, cam) = (dir.join("B.bundle"), dir.join("camera"));
        std::fs::create_dir_all(&b).unwrap();
        std::fs::create_dir_all(&cam).unwrap();
        std::fs::write(b.join("Info.plist"), b"<plist/>").unwrap();
        std::fs::write(cam.join("H9ISP.mediacapture"), b"not a Mach-O").unwrap();
        // the camera plug-in alone: one file under camera/, no Metal file (hosted 0090 keeps the seed's Metal plug-in)
        let out = dir.join("c.gpuaddon");
        addon(&out, None, Some(&cam)).unwrap();
        let files = read_gpuaddon(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(files.iter().map(|f| (f.0.as_str(), f.1)).collect::<Vec<_>>(), vec![("camera/MediaCapture/H9ISP.mediacapture", 0o644)]);
        assert_eq!(files[0].2, b"not a Mach-O");
        // no Mach-O in it: an empty CDHash list
        assert_eq!(std::fs::read_to_string(dir.join("c.gpuaddon.cdhashes")).unwrap(), "");
        // both: the Metal files first (as `ios gpuaddon` writes them), then the camera's; the file itself is taken too
        let both = dir.join("b.gpuaddon");
        addon(&both, Some(&b), Some(&cam.join("H9ISP.mediacapture"))).unwrap();
        let names: Vec<String> = read_gpuaddon(&std::fs::read(&both).unwrap()).unwrap().into_iter().map(|f| f.0).collect();
        assert_eq!(
            names,
            ["addons/NativeAdrenoMetal.bundle/Info.plist", "addons/NativeAdrenoMetal.publish", "camera/MediaCapture/H9ISP.mediacapture"]
        );
        // the Metal part is byte for byte `ios gpuaddon`'s
        let metal = dir.join("m.gpuaddon");
        gpuaddon(&b, &metal).unwrap();
        addon(&dir.join("m2.gpuaddon"), Some(&b), None).unwrap();
        assert_eq!(std::fs::read(&metal).unwrap(), std::fs::read(dir.join("m2.gpuaddon")).unwrap());
        // refusals: nothing asked for, a file of another name
        assert!(addon(&dir.join("x"), None, None).is_err());
        std::fs::write(dir.join("other.dylib"), b"x").unwrap();
        assert!(addon(&dir.join("x"), None, Some(&dir.join("other.dylib"))).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// The command pool row (userland/metal/CommandPoolRow.inc, appended by build.sh after this edit): its start declared
/// near the top of the plug-in and called where a process gets its device, the render server's path and a client's
/// (docs/gpu.md, "Who keeps a blob").
fn pool_row_edits() -> Vec<(&'static str, String, &'static str)> {
    vec![
        (
            "static unsigned gClientPort;\n",
            "static unsigned gClientPort;\nextern \"C\" void oneios_gpu_pool_row_start(unsigned port);   // CommandPoolRow.inc (OneiOS)\n".into(),
            "declare the command pool row's start (CommandPoolRow.inc)",
        ),
        (
            "        gpu->mark(0x6802);\n        startMemoryWriter();\n",
            "        gpu->mark(0x6802);\n        startMemoryWriter();\n        oneios_gpu_pool_row_start(gpu->port);\n".into(),
            "the render server's command pool row",
        ),
        (
            "        startPassReporter(port);\n        startMemoryWriter();\n",
            "        startPassReporter(port);\n        startMemoryWriter();\n        oneios_gpu_pool_row_start(port);\n".into(),
            "a client's command pool row",
        ),
    ]
}
