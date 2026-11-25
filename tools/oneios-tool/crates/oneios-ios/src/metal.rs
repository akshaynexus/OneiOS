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
            Some(Value::Array(a)) => {
                a.push(Value::String("patches/mesa/trace-turnip-init.patch".into()));
                // userland/metal/turnip-idle-bo-cache.patch: the idle trim of Turnip's BO cache, applied after the trace patch.
                a.push(Value::String("patches/mesa/turnip-idle-bo-cache.patch".into()));
                // userland/metal/turnip-submit-split.patch (Droid-VM/UniXNU fb9fb47): a kernel submission ends only where a
                // command buffer begins, after the idle one.
                a.push(Value::String("patches/mesa/turnip-submit-split.patch".into()));
            }
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
                "\n\nPATCHES.append(ROOT / 'patches/metal2vulkan/trace-metal-vulkan-init.patch')\nPATCHES.append(ROOT / 'patches/metal2vulkan/m2v-spare-command-pools.patch')\nPATCHES.append(ROOT / 'patches/metal2vulkan/m2v-idle-memory.patch')\nPATCHES.append(ROOT / 'patches/metal2vulkan/90101-large-indexed-constants-to-blob.patch')\nPATCHES.append(ROOT / 'patches/metal2vulkan/90102-blob-constant-gep-pointers.patch')\nPATCHES.append(ROOT / 'patches/metal2vulkan/90110-shader-fix-switch.patch')\nPATCHES.append(ROOT / 'patches/metal2vulkan/m2v-compile-deadline.patch')\n\n\ndef git_tree(*steps):".into(),
                "record Vulkan initialization diagnostics in the translator patch stack; spare command pools follow their recent use (m2v-spare-command-pools.patch); the idle trim ages the spare image blocks, the memory census (m2v-idle-memory.patch); the Adreno shader fixes 0101 and 0102 behind their switch, on unless oneios-m2v-shader-fix=0 (90110-shader-fix-switch.patch); the compile deadline per process (m2v-compile-deadline.patch)",
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
    let text = std::fs::read_to_string(&plugin).with_context(|| plugin.display().to_string())?;
    // the bool debug options' numbers first: they follow the upstream address, which the list below then replaces
    let (mut text, said) = hook_options(&text, &hooks)?;
    for line in said {
        println!("metal: staged NativeAdrenoMetalPlugin.mm: {line}");
    }
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
    // at the top, so that the plug-in's own code and the OneiOS files appended to it (DisplayWide.inc) both see them
    let defines = hook_defines(&hooks)?;
    let text = format!("{defines}{text}");
    println!(
        "metal: staged NativeAdrenoMetalPlugin.mm: this target's QuartzCore constants at the top, for the OneiOS files appended after it ({} defines)",
        defines.matches("#define").count()
    );
    std::fs::write(&plugin, text)?;
    Ok(())
}

/// The hooks file's QuartzCore bool debug options (`"options"`: `base`, the upstream address of `_ca_debug_options`,
/// and `bool`, a list of `name`, `upstream`, `value`): init_debug stores option N at `_ca_debug_options + N`, and the
/// numbers move between builds (24A446: CA_FORCE_FP16_FRAMEBUFFER 128, CA_ALLOW_EDR_MIRRORING 156; 24B156: 129 and
/// 159; read in each build's init_debug table, docs/gpu.md section 7). Every `BASE + N` of the plug-in's text gets the
/// target's number in one pass; an N the file does not name, or a named one the text never has, is refused. The
/// address itself is the `replace` list's (`stage_edit` replaces it after this). A hooks file with no `options`
/// leaves the text as it is. The text, and a line for each number that changed.
pub fn hook_options(text: &str, hooks: &serde_json::Value) -> Result<(String, Vec<String>)> {
    let Some(options) = hooks.get("options") else { return Ok((text.to_string(), Vec::new())) };
    let base = s(options, "base")?;
    let mut map: Vec<(u64, u64, String)> = Vec::new();
    let list = options.get("bool").and_then(|v| v.as_array()).ok_or_else(|| anyhow!("the hooks file: \"options\" has no list \"bool\""))?;
    for o in list {
        let n = |k: &str| o.get(k).and_then(|v| v.as_u64()).ok_or_else(|| anyhow!("the hooks file: option {o} has no number {k:?}"));
        map.push((n("upstream")?, n("value")?, s(o, "name")?.to_string()));
    }
    let lead = format!("{base} + ");
    let (mut out, mut rest) = (String::with_capacity(text.len()), text);
    let mut seen = vec![0usize; map.len()];
    while let Some(i) = rest.find(&lead) {
        let after = &rest[i + lead.len()..];
        let digits = after.chars().take_while(char::is_ascii_digit).count();
        out.push_str(&rest[..i + lead.len()]);
        let number: u64 = after[..digits].parse().map_err(|_| anyhow!("NativeAdrenoMetalPlugin.mm: {lead}is not followed by a number"))?;
        let k = map
            .iter()
            .position(|(up, _, _)| *up == number)
            .ok_or_else(|| anyhow!("NativeAdrenoMetalPlugin.mm has {lead}{number}, an option the hooks file does not name"))?;
        seen[k] += 1;
        out.push_str(&map[k].1.to_string());
        rest = &after[digits..];
    }
    out.push_str(rest);
    let mut said = Vec::new();
    for (k, (up, value, name)) in map.iter().enumerate() {
        if seen[k] == 0 {
            bail!("NativeAdrenoMetalPlugin.mm has no {lead}{up} ({name}): the hooks file is for another revision");
        }
        if up != value {
            said.push(format!("{lead}{up} -> {lead}{value} ({} places): {name}, this build's number", seen[k]));
        }
    }
    Ok((out, said))
}

/// The C defines of this target's QuartzCore constants that the OneiOS files appended to the plug-in use
/// (userland/metal/DisplayWide.inc), from the hooks file: `ONEIOS_HOOKS_TARGET`, `ONEIOS_CA_RENDER_SERVER_START` (the
/// anchor), `ONEIOS_CA_DEBUG_OPTIONS` (the `replace` value of `options.base`) and `ONEIOS_CA_OPTION_<NAME>` for each
/// bool option. Without `options` only the first two.
pub fn hook_defines(hooks: &serde_json::Value) -> Result<String> {
    let anchor = hooks.get("anchor").ok_or_else(|| anyhow!("the hooks file has no anchor"))?;
    let mut out = format!(
        "// OneiOS (oneios-tool ios metal-stage-edit): this target's QuartzCore constants (userland/metal/hooks), for the\n// OneiOS files appended to this one.\n#define ONEIOS_HOOKS_TARGET \"{}\"\n#define ONEIOS_CA_RENDER_SERVER_START {}ull\n",
        s(hooks, "target")?,
        s(anchor, "value")?
    );
    if let Some(options) = hooks.get("options") {
        let base = s(options, "base")?;
        let replace = hooks.get("replace").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let item = replace
            .iter()
            .find(|i| i.get("upstream").and_then(|u| u.as_str()) == Some(base))
            .ok_or_else(|| anyhow!("the hooks file: options.base {base} is not in \"replace\""))?;
        out.push_str(&format!("#define ONEIOS_CA_DEBUG_OPTIONS {}ull\n", s(item, "value")?));
        for o in options.get("bool").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
            let name = s(&o, "name")?;
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') {
                bail!("the hooks file: option name {name:?} is not a C name");
            }
            let value = o.get("value").and_then(|v| v.as_u64()).ok_or_else(|| anyhow!("the hooks file: option {name} has no number \"value\""))?;
            out.push_str(&format!("#define ONEIOS_CA_OPTION_{name} {value}\n"));
        }
    }
    out.push('\n');
    Ok(out)
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
        (
            concat!(
                " // Metal builds a kernel that reads a constant nobody set; the runtime refuses it: once more with those constants zero.\n",
                " if(!handle) {\n",
                "  NBFunction *rebuilt=zeroRestFunction((NBFunction *)d.computeFunction);\n"
            ),
            concat!(
                " // Metal builds a kernel that reads a constant nobody set; the runtime refuses it: once more with those constants zero.\n",
                " // OneiOS (oneios-tool ios metal-stage-edit; Droid-VM/NativeMetal 3c46b5e, MIT): the rebuilt function lives to the end of\n",
                " // the call, as the reflection below reads its handle: held in the block alone, ARC freed it with the block and the\n",
                " // reflection read a released function (cameracaptured's CCPortrait kernels: SIGSEGV at 0 in\n",
                " // nmb_compute_pipeline_reflection during its precompilation, every boot).\n",
                " NBFunction *rebuilt=nil;\n",
                " if(!handle) {\n",
                "  rebuilt=zeroRestFunction((NBFunction *)d.computeFunction);\n"
            )
            .into(),
            "a compute pipeline rebuilt with its unset constants zero keeps its function until the reflection is read (NativeMetal 3c46b5e: cameracaptured's crash)",
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

/// The Galaxy A56's Metal plug-in (userland/metal-amd): a bundle folder of this name keeps it under addons/.
const RADEON_BUNDLE: &str = "OneiOSRadeonMetal";
/// The Galaxy A56's OpenGL ES (userland/metal-amd/gl, docs/a56-gpu.md 4a): a bundle folder of this name keeps it too.
const RADEON_GL_BUNDLE: &str = "OneiOSRadeonGL";

/// The name a Metal bundle has under addons/: the A56's plug-in (a folder named `OneiOSRadeonMetal.bundle`) keeps its
/// own; every other folder is the Adreno plug-in, NativeAdrenoMetal, whatever the folder is called (as before the
/// A56's plug-in existed: out/userland and out/userland-rt both hold one).
fn addon_bundle_name(bundle: &Path) -> &'static str {
    match bundle.file_name().and_then(|n| n.to_str()) {
        Some(n) if n.strip_suffix(".bundle") == Some(RADEON_BUNDLE) => RADEON_BUNDLE,
        Some(n) if n.strip_suffix(".bundle") == Some(RADEON_GL_BUNDLE) => RADEON_GL_BUNDLE,
        _ => "NativeAdrenoMetal",
    }
}

/// The Metal plug-in's files as the kernel's 0031 union wants them: `addons/<name>.bundle/...` in os.walk's order, then
/// the `addons/<name>.publish` marker (the name: [`addon_bundle_name`]).
fn metal_files(bundle: &Path) -> Result<Vec<AddonFile>> {
    if addon_bundle_name(bundle) == RADEON_GL_BUNDLE {
        bail!("{}: the A56's OpenGL ES bundle is not a Metal plug-in: give it with --gl", bundle.display());
    }
    bundle_files(bundle, true)
}

/// The A56's OpenGL ES bundle (a folder named `OneiOSRadeonGL.bundle`) under addons/ beside the Metal plug-in:
/// `addons/OneiOSRadeonGL.bundle/...`, which 0031's union puts in /System/Library/Extensions, where libGFXShared loads
/// the accelerator's IOGLESBundleName. No .publish marker (that is the Metal plug-in's).
fn gl_files(bundle: &Path) -> Result<Vec<AddonFile>> {
    if addon_bundle_name(bundle) != RADEON_GL_BUNDLE {
        bail!("{}: not an OneiOSRadeonGL.bundle folder (userland/metal-amd/gl/build.sh plugin)", bundle.display());
    }
    bundle_files(bundle, false)
}

fn bundle_files(bundle: &Path, publish: bool) -> Result<Vec<AddonFile>> {
    let mut files: Vec<AddonFile> = Vec::new();
    let name = addon_bundle_name(bundle);
    fn walk(dir: &Path, rel: &str, bundle: &str, out: &mut Vec<AddonFile>) -> Result<()> {
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
            out.push((format!("addons/{bundle}.bundle/{r}"), file_mode(&p)?, std::fs::read(&p)?));
        }
        for e in dirs {
            let name = e.file_name().to_string_lossy().into_owned();
            walk(&e.path(), &if rel.is_empty() { name } else { format!("{rel}/{name}") }, bundle, out)?;
        }
        Ok(())
    }
    walk(bundle, "", name, &mut files)?;
    if publish {
        files.push((format!("addons/{name}.publish"), 0o644, b"1\n".to_vec()));
    }
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
pub fn gpuaddon(bundle: &Path, output: &Path, gl: Option<&Path>) -> Result<()> {
    let mut files = metal_files(bundle)?;
    if let Some(gl) = gl {
        files.extend(gl_files(gl)?);
    }
    let blob = pack(&files)?;
    std::fs::write(output, &blob).with_context(|| output.display().to_string())?;
    println!("metal: {}: {} files, {} bytes", output.display(), files.len(), blob.len());
    Ok(())
}

/// `ios addon OUTPUT [--metal BUNDLE] [--camera DIR]`: the boot addon (the kernel.kc.gpuaddon sidecar) with the Metal
/// plug-in under addons/ (hosted 0031's union), the capture plug-in under camera/ (hosted 0090's), or both; and
/// OUTPUT.cdhashes, one `CDHASH NAME` line for each Mach-O file in it: the CDHashes the image's owner-trust.json must
/// list (`oneios-tool phone addon OUTPUT` adds them).
pub fn addon(output: &Path, metal: Option<&Path>, camera: Option<&Path>, gl: Option<&Path>) -> Result<()> {
    if metal.is_none() && camera.is_none() {
        bail!("ios addon OUTPUT --metal BUNDLE and/or --camera DIR: nothing to put in it");
    }
    let mut files = Vec::new();
    if let Some(bundle) = metal {
        files.extend(metal_files(bundle)?);
    }
    if let Some(bundle) = gl {
        files.extend(gl_files(bundle)?);
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
        "addon: {}: {} files ({} of the Metal plug-in (and the A56's OpenGL ES) under addons/, {} of the capture plug-in under camera/), {} bytes; {} CDHashes in {list}",
        output.display(),
        files.len(),
        metal_count,
        files.len() - metal_count,
        blob.len(),
        cdhashes.lines().count()
    );
    Ok(())
}


/// The Galaxy A56's boot addon in memory: the Metal plug-in (`OneiOSRadeonMetal.bundle`) and, when given, its OpenGL ES
/// (`OneiOSRadeonGL.bundle`) under addons/, as `ios gpuaddon BUNDLE OUT --gl GL` writes it, and the CDHash of every Mach-O
/// in it (sorted, each once): what the image's owner-trust.json must list. The phone's Update the kernel and Make iOS image
/// build it from the staged runtime's lib/guest/ (oneios-imagebuild a56gpu; docs/a56-gpu.md 6).
pub fn a56_addon(metal: &Path, gl: Option<&Path>) -> Result<(Vec<u8>, Vec<String>)> {
    if addon_bundle_name(metal) != RADEON_BUNDLE {
        bail!("{}: not an OneiOSRadeonMetal.bundle folder (userland/metal-amd/air/build.sh plugin)", metal.display());
    }
    let mut files = metal_files(metal)?;
    if let Some(gl) = gl {
        files.extend(gl_files(gl)?);
    }
    let mut cdhashes: Vec<String> = Vec::new();
    for (_, _, data) in &files {
        if data.get(..4) == Some(&[0xcf, 0xfa, 0xed, 0xfe]) {
            let slices = oneios_tweaks::macho::describe(data).map_err(|e| anyhow!("{e}"))?;
            cdhashes.extend(slices.into_iter().map(|s| s.cdhash));
        }
    }
    cdhashes.sort();
    cdhashes.dedup();
    Ok((pack(&files)?, cdhashes))
}

/// `ios bundle-record ROOT BUNDLE RECORD TARGET [SOURCE...]`: the build record of one of our guest bundles (the Galaxy
/// A56's OneiOSRadeonMetal and OneiOSRadeonGL), in the shape of NativeAdrenoMetal.build.json that runtime/package.sh
/// and the package's guest check read: "bundle", "target", "files" (each Mach-O of the bundle: bytes, cdhash, arch,
/// page_size, identifier, minos), "cdhashes" and "sources" (each SOURCE `NAME` is external/NAME's HEAD in ROOT, or
/// `NAME=COMMIT` as given: a source outside external/, as iOSPlus10's NegroniGL).
pub fn bundle_record(root: &Path, bundle: &Path, record: &Path, target: &str, sources: &[String]) -> Result<()> {
    let mut files = Map::new();
    let mut stack = vec![(bundle.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&dir).with_context(|| dir.display().to_string())?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let r = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            if p.is_dir() {
                stack.push((p, r));
                continue;
            }
            let data = std::fs::read(&p).with_context(|| p.display().to_string())?;
            if data.get(..4) != Some(&[0xcf, 0xfa, 0xed, 0xfe]) {
                continue;
            }
            let info = oneios_tweaks::macho::describe(&data).map_err(|e| anyhow!("{}: {e}", p.display()))?.into_iter().next().ok_or_else(|| anyhow!("{}: no slice", p.display()))?;
            let mut f = Map::new();
            f.insert("bytes".into(), Value::Number(data.len().into()));
            f.insert("cdhash".into(), Value::String(info.cdhash));
            f.insert("arch".into(), Value::String(info.arch));
            f.insert("page_size".into(), Value::Number(info.page_size.into()));
            f.insert("identifier".into(), Value::String(info.identifier));
            f.insert("minos".into(), info.minos.map(Value::String).unwrap_or(Value::Null));
            files.insert(r, Value::Object(f));
        }
    }
    if files.is_empty() {
        bail!("{}: no Mach-O in the bundle", bundle.display());
    }
    let mut cdhashes: Vec<String> = files.values().filter_map(|f| if let Value::Object(m) = f { m.get("cdhash").map(Value::text) } else { None }).collect();
    cdhashes.sort();
    let mut pins = Map::new();
    for s in sources {
        match s.split_once('=') {
            Some((name, commit)) => pins.insert(name.into(), Value::String(commit.into())),
            None => pins.insert(s.clone(), Value::String(git_head(&root.join("external").join(s)))),
        };
    }
    let mut r = Map::new();
    r.insert("bundle".into(), Value::String(addon_bundle_name(bundle).to_string() + ".bundle"));
    r.insert("target".into(), Value::String(target.into()));
    r.insert("files".into(), Value::Object(files));
    r.insert("cdhashes".into(), Value::Array(cdhashes.into_iter().map(Value::String).collect()));
    r.insert("sources".into(), Value::Object(pins));
    let text = oneios_json::to_pretty(&Value::Object(r)) + "\n";
    std::fs::write(record, text).with_context(|| record.display().to_string())?;
    println!("metal: {}: the build record of {}", record.display(), bundle.display());
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

    fn hooks_file(target: &str) -> serde_json::Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../userland/metal/hooks").join(format!("{}.json", target.replace(' ', "-")));
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
    }

    #[test]
    fn each_build_gets_its_own_option_numbers() {
        let text = "a(0x1e702116c + 156 + slide); b(0x1e702116c + 128 + gSlide); c(0x1e702116c + 156 + gSlide); 0x1e702116c + 128)\n";
        // 24A446: the numbers are upstream's, nothing said
        let (same, said) = hook_options(text, &hooks_file("iPhone18,2 24A446")).unwrap();
        assert!(same == text && said.is_empty());
        // 24B156: 128 -> 129, 156 -> 159, in one pass (no number is changed twice)
        let (b, said) = hook_options(text, &hooks_file("iPhone18,2 24B156")).unwrap();
        assert_eq!(b, "a(0x1e702116c + 159 + slide); b(0x1e702116c + 129 + gSlide); c(0x1e702116c + 159 + gSlide); 0x1e702116c + 129)\n");
        assert_eq!(said.len(), 2);
        assert!(said[0].contains("+ 128 -> 0x1e702116c + 129 (2 places): CA_FORCE_FP16_FRAMEBUFFER"), "{said:?}");
        // an option the file does not name, or a named one not in the text: refused
        assert!(hook_options("x(0x1e702116c + 140)", &hooks_file("iPhone18,2 24B156")).unwrap_err().to_string().contains("+ 140, an option the hooks file does not name"));
        assert!(hook_options("x(0x1e702116c + 128)", &hooks_file("iPhone18,2 24B156")).unwrap_err().to_string().contains("no 0x1e702116c + 156 (CA_ALLOW_EDR_MIRRORING)"));
        // no "options": the text as it was
        assert_eq!(hook_options("0x1e702116c + 140", &serde_json::json!({})).unwrap().0, "0x1e702116c + 140");
    }

    #[test]
    fn the_defines_are_the_targets() {
        let a = hook_defines(&hooks_file("iPhone18,2 24A446")).unwrap();
        for line in ["#define ONEIOS_HOOKS_TARGET \"iPhone18,2 24A446\"", "#define ONEIOS_CA_RENDER_SERVER_START 0x1846dce88ull", "#define ONEIOS_CA_DEBUG_OPTIONS 0x1e702516cull", "#define ONEIOS_CA_OPTION_CA_FORCE_FP16_FRAMEBUFFER 128"] {
            assert!(a.contains(&format!("{line}\n")), "{a}");
        }
        let b = hook_defines(&hooks_file("iPhone18,2 24B156")).unwrap();
        for line in ["#define ONEIOS_HOOKS_TARGET \"iPhone18,2 24B156\"", "#define ONEIOS_CA_RENDER_SERVER_START 0x1845ce544ull", "#define ONEIOS_CA_DEBUG_OPTIONS 0x1edf3e5ccull", "#define ONEIOS_CA_OPTION_CA_FORCE_FP16_FRAMEBUFFER 129", "#define ONEIOS_CA_OPTION_CA_ALLOW_EDR_MIRRORING 159"] {
            assert!(b.contains(&format!("{line}\n")), "{b}");
        }
        // DisplayWide.inc uses the defines, and no build's numbers of its own
        let inc = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../userland/metal/DisplayWide.inc")).unwrap();
        let code: String = inc.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
        assert!(code.contains("ONEIOS_CA_DEBUG_OPTIONS + ONEIOS_CA_OPTION_CA_FORCE_FP16_FRAMEBUFFER + slide") && code.contains("- ONEIOS_CA_RENDER_SERVER_START;"));
        assert!(regex::Regex::new(r"0x1[0-9a-f]{8}").unwrap().find(&code).is_none(), "an address in DisplayWide.inc's code");
    }

    /// Both hooks files against the plug-in of the pinned NativeMetal (when external/ has it): every address and option
    /// the arm64e code uses is named, and what stage_edit makes of it holds only the target's.
    #[test]
    fn both_hooks_files_fit_the_pinned_plugin() {
        let upstream = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../external/NativeMetal/NativeAdrenoMetalPlugin.mm");
        let Ok(text) = std::fs::read_to_string(&upstream) else {
            eprintln!("skipped: no {} (scripts/setup/get-references.sh NativeMetal)", upstream.display());
            return;
        };
        for target in ["iPhone18,2 24A446", "iPhone18,2 24B156"] {
            let hooks = hooks_file(target);
            let (mut t, _) = hook_options(&text, &hooks).unwrap();
            let mut items = vec![hooks["anchor"].clone()];
            items.extend(hooks["replace"].as_array().unwrap().iter().cloned());
            for item in &items {
                let (new, n) = replace_address(&t, item["upstream"].as_str().unwrap(), item["value"].as_str().unwrap());
                assert!(n > 0, "{target}: no {}", item["upstream"]);
                t = new;
            }
            assert!(t.contains(&format!("{} + slide", hooks["branch"]["address"].as_str().unwrap())), "{target}");
            if target.ends_with("24B156") {
                for gone in ["0x1846dce88", "0x184461928", "0x184461838", "0x1e7021014", "0x1e7021104", "0x1e702116c", "0x1e701bdd9", "0x1e7020780", "0x1e9021680"] {
                    assert!(!t.contains(gone), "{target}: {gone} is still there");
                }
                assert!(t.contains("0x1edf3e5cc + 159 + slide") && t.contains("0x1edf3e5cc + 129 + gSlide"));
            }
        }
    }

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
        gpuaddon(&b, &out, None).unwrap();
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
    fn the_a56_gl_bundle_goes_beside_the_metal_plugin() {
        // userland/metal-amd/gl's OneiOSRadeonGL.bundle (docs/a56-gpu.md 4a): its own name under addons/, after the Metal
        // plug-in and its .publish marker, with no marker of its own (xnuhost's gl_in_addon looks for its executable)
        let dir = std::env::temp_dir().join(format!("oneios-ios-addon-a56gl-{}", std::process::id()));
        let (m, g) = (dir.join("OneiOSRadeonMetal.bundle"), dir.join("OneiOSRadeonGL.bundle"));
        for (b, exe) in [(&m, "OneiOSRadeonMetal"), (&g, "OneiOSRadeonGL")] {
            std::fs::create_dir_all(b).unwrap();
            std::fs::write(b.join("Info.plist"), b"<plist/>").unwrap();
            std::fs::write(b.join(exe), b"mach-o").unwrap();
            std::fs::write(b.join("oneios-target"), b"iPhone10,3 20F66\n").unwrap();
        }
        let out = dir.join("a.gpuaddon");
        gpuaddon(&m, &out, Some(&g)).unwrap();
        let names: Vec<String> = read_gpuaddon(&std::fs::read(&out).unwrap()).unwrap().into_iter().map(|f| f.0).collect();
        assert_eq!(
            names,
            vec![
                "addons/OneiOSRadeonMetal.bundle/Info.plist",
                "addons/OneiOSRadeonMetal.bundle/OneiOSRadeonMetal",
                "addons/OneiOSRadeonMetal.bundle/oneios-target",
                "addons/OneiOSRadeonMetal.publish",
                "addons/OneiOSRadeonGL.bundle/Info.plist",
                "addons/OneiOSRadeonGL.bundle/OneiOSRadeonGL",
                "addons/OneiOSRadeonGL.bundle/oneios-target",
            ]
        );
        // the same through `ios addon --gl`; a folder of another name is refused as --gl, the GL bundle as the Metal one
        addon(&dir.join("b.gpuaddon"), Some(&m), None, Some(&g)).unwrap();
        assert_eq!(read_gpuaddon(&std::fs::read(dir.join("b.gpuaddon")).unwrap()).unwrap().len(), 7);
        assert!(gpuaddon(&m, &out, Some(&m)).is_err());
        assert!(gpuaddon(&g, &out, None).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_a56_bundle_keeps_its_name_in_the_sidecar() {
        // userland/metal-amd's OneiOSRadeonMetal.bundle: its own name under addons/ and its own .publish marker
        let dir = std::env::temp_dir().join(format!("oneios-ios-addon-a56-{}", std::process::id()));
        let b = dir.join("OneiOSRadeonMetal.bundle");
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join("Info.plist"), b"<plist/>").unwrap();
        std::fs::write(b.join("OneiOSRadeonMetal"), b"mach-o").unwrap();
        let out = dir.join("a.gpuaddon");
        gpuaddon(&b, &out, None).unwrap();
        let files = read_gpuaddon(&std::fs::read(&out).unwrap()).unwrap();
        let names: Vec<&str> = files.iter().map(|(n, _, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "addons/OneiOSRadeonMetal.bundle/Info.plist",
                "addons/OneiOSRadeonMetal.bundle/OneiOSRadeonMetal",
                "addons/OneiOSRadeonMetal.publish",
            ]
        );
        // any other folder name is the Adreno plug-in, as before
        assert_eq!(addon_bundle_name(Path::new("out/userland-rt/NativeAdrenoMetal.bundle")), "NativeAdrenoMetal");
        assert_eq!(addon_bundle_name(Path::new("x/OneiOSRadeonMetalX.bundle")), "NativeAdrenoMetal");
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
        // NativeMetal 3c46b5e: the rebuilt function is declared before the retry's block, not inside it
        assert!(text.contains(" NBFunction *rebuilt=nil;\n if(!handle) {\n  rebuilt=zeroRestFunction((NBFunction *)d.computeFunction);\n"));
        assert!(!text.contains("  NBFunction *rebuilt=zeroRestFunction((NBFunction *)d.computeFunction);"));
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
        addon(&out, None, Some(&cam), None).unwrap();
        let files = read_gpuaddon(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(files.iter().map(|f| (f.0.as_str(), f.1)).collect::<Vec<_>>(), vec![("camera/MediaCapture/H9ISP.mediacapture", 0o644)]);
        assert_eq!(files[0].2, b"not a Mach-O");
        // no Mach-O in it: an empty CDHash list
        assert_eq!(std::fs::read_to_string(dir.join("c.gpuaddon.cdhashes")).unwrap(), "");
        // both: the Metal files first (as `ios gpuaddon` writes them), then the camera's; the file itself is taken too
        let both = dir.join("b.gpuaddon");
        addon(&both, Some(&b), Some(&cam.join("H9ISP.mediacapture")), None).unwrap();
        let names: Vec<String> = read_gpuaddon(&std::fs::read(&both).unwrap()).unwrap().into_iter().map(|f| f.0).collect();
        assert_eq!(
            names,
            ["addons/NativeAdrenoMetal.bundle/Info.plist", "addons/NativeAdrenoMetal.publish", "camera/MediaCapture/H9ISP.mediacapture"]
        );
        // the Metal part is byte for byte `ios gpuaddon`'s
        let metal = dir.join("m.gpuaddon");
        gpuaddon(&b, &metal, None).unwrap();
        addon(&dir.join("m2.gpuaddon"), Some(&b), None, None).unwrap();
        assert_eq!(std::fs::read(&metal).unwrap(), std::fs::read(dir.join("m2.gpuaddon")).unwrap());
        // refusals: nothing asked for, a file of another name
        assert!(addon(&dir.join("x"), None, None, None).is_err());
        std::fs::write(dir.join("other.dylib"), b"x").unwrap();
        assert!(addon(&dir.join("x"), None, Some(&dir.join("other.dylib")), None).is_err());
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
