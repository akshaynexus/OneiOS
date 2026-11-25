//! `oneios-tool image build` and `serve`'s `make-image`: scripts/image/build.sh in one process, as `prelink build` is
//! kernel/prelink/build.sh. A fresh phone makes the whole image set with it, with no PC (the owner, 2026-11-21):
//! the phone has no bash, no apfs-fuse and no Python, and `serve` starts no program.
//!
//! The steps, in build.sh's order and with its checks (tools/oneios-tool/README.md "image build" has each one and
//! what it was compared with):
//!
//! 1. `check`: the profile, the inputs that are there, the kernel's inputs, the output folders; and what this
//!    build cannot do yet is refused here, before anything is written or fetched (`not-ported`; since 2026-11-21 nothing
//!    is: the root tweaks and the AEA decryption are in). Then **one** `space` event for everything the build writes.
//! 2. `directory`, `download`: `image fetch` of the profile's members (resumable; a member that is there and whole
//!    is not fetched again, an AEA whose decrypted image is recorded is not fetched at all).
//! 3. `decrypt`: the AEA images (OS, SystemOS) into their plain images, recorded in inputs.json (`record-plain`).
//! 4. `place`: the images and trust files into the set, hard links where the folder allows (build.sh's `put`);
//!    each volume must be a raw APFS image (`apfs-info`).
//! 5. `data`: the OS volume's `/private/var` copied out with our APFS reader, with tree-copy's meta file (`data.rs`).
//! 6. `tweaks`: the root tweaks (the userland hook of build.sh) into the copied tree, the overlay and the owners.
//! 7. `seed`: `seed-pack` of the tree into `data.seed`; the hook's overlay into `overlay/`.
//! 8. `kernelcache`: Apple's kernelcache decoded (`im4p`), unless one is given.
//! 9. `kernel-*`: `prelink build` of the runtime's `kernel/xnu.macho` with it (the audit and `stubs-check` before
//!    `kernel.kc` is taken), unless a `kernel.kc` is given.
//! 10. `manifest`: `manifest.json` and the hash lists; `verify`: `image verify` of the set, every chunk.
//! 11. `lists`: the stage's `SHA256  PATH` and `DEV:INODE:SIZE:MTIME PATH` lists of the set (what root's `receive`
//!     writes for an import), then the set into its folder.
//!
//! Between steps a `pause` holds the build (and a fetch holds its transfer at once); a stop (a cancel, the heat
//! and battery limits, the end of `serve`) ends it and removes what it wrote: its own work folder `WORK/build`, the
//! set's files and the lists' new lines. The fetched members in INDIR stay for the next run (they are the
//! resumable part). Nothing here starts a program.

pub mod a56gpu;
mod cmd;
pub mod data;

use oneios_common::job;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// The prelink's steps, as this build says them (`prelink build`'s with `kernel-` before each name).
pub const PRELINK_PREFIX: &str = "kernel-";

/// The steps, in the order they run, each with a line for a screen.
pub static STEPS: [(&str, &str); 28] = [
    ("check", "Checking the inputs and the room"),
    ("directory", "Reading the IPSW's directory"),
    ("download", "Downloading and checking the members"),
    ("decrypt", "Decrypting the OS images"),
    ("place", "Putting Apple's images into the set"),
    ("data", "Copying the OS volume's /private/var"),
    ("tweaks", "Applying the root tweaks"),
    ("seed", "Packing the Data seed"),
    ("kernelcache", "Decoding Apple's kernelcache"),
    ("kernel-check", "Kernel: checking the inputs"),
    ("kernel-native-input", "Kernel: preparing the kernel"),
    ("kernel-merge-reviewed", "Kernel: merging the reviewed records"),
    ("kernel-roots", "Kernel: choosing the kexts"),
    ("kernel-hosted", "Kernel: rewriting the kexts for the hosted kernel"),
    ("kernel-match", "Kernel: pairing the original kernel with ours"),
    ("kernel-align", "Kernel: pairing by link order"),
    ("kernel-kclasses", "Kernel: pairing the kernel's classes"),
    ("kernel-structors", "Kernel: pairing constructors and destructors"),
    ("kernel-vtables", "Kernel: pairing virtual methods"),
    ("kernel-emit", "Kernel: writing the kernel collection"),
    ("kernel-audit", "Kernel: checking every import's binding"),
    ("kernel-stubs-check", "Kernel: checking the stubs"),
    ("kernel-record", "Kernel: writing the build record"),
    ("kernel-hashes", "Kernel: hashing the kernel collection"),
    ("kernel-place", "Kernel: putting the files in place"),
    ("manifest", "Writing the manifest and the hash lists"),
    ("verify", "Checking the image set"),
    ("lists", "Putting the set in place"),
];

/// Where the set's kernel collection comes from.
#[derive(Debug, Clone)]
pub enum Kernel {
    /// A `kernel.kc` that is there (build.sh's `--kernel FILE`).
    Kc(String),
    /// No kernel collection (build.sh's `--no-kernel`): the runtime cannot start the set.
    None,
    /// `prelink build` of this kernel with Apple's kernelcache (the phone: the runtime's `kernel/xnu.macho`).
    Prelink(Prelink),
}

/// `prelink build`'s inputs (the phone keeps the pairing files in the stage's `kb/`).
#[derive(Debug, Clone)]
pub struct Prelink {
    /// The hosted kernel, `xnu.macho`.
    pub xnu: String,
    /// The decoded kernelcache; `None`: the IPSW's kernelcache member in INDIR, decoded by this build.
    pub kernelcache: Option<String>,
    pub profile: String,
    pub reviewed: String,
    pub more_reviewed: Vec<String>,
    pub pairs: String,
    pub stubs: String,
    pub xnuprelink: String,
}

impl Prelink {
    /// The pairing files laid out as the stage's `kb/` (the OneOS app writes them: `profile.toml`,
    /// `reviewed.json`, `lead.json`, `user.json`, `pairs.json`, `stubs.json`, `match/`). A `kernelcache.macho`
    /// there is used (`kb/kernelcache.sha256` beside it is the app's; the prelink checks the profile's pin).
    pub fn from_kb(xnu: &str, kb: &str) -> Prelink {
        let at = |name: &str| format!("{}/{name}", kb.trim_end_matches('/'));
        let cache = at("kernelcache.macho");
        Prelink {
            xnu: xnu.to_string(),
            kernelcache: Path::new(&cache).is_file().then_some(cache),
            profile: at("profile.toml"),
            reviewed: at("reviewed.json"),
            more_reviewed: [at("lead.json"), at("user.json")].into_iter().filter(|p| Path::new(p).is_file()).collect(),
            pairs: at("pairs.json"),
            stubs: at("stubs.json"),
            xnuprelink: at("match"),
        }
    }

    /// The inputs as kernel/prelink/build.sh takes them from a OneiOS checkout, for the build `build` of a profile.
    pub fn from_repo(xnu: &str, repo: &str, build: &str) -> Prelink {
        let at = |rel: &str| format!("{}/{rel}", repo.trim_end_matches('/'));
        let glob = |dir: &str| -> Vec<String> {
            let mut found: Vec<String> = std::fs::read_dir(at(dir))
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("reviewed-") && n.contains(&format!("-{build}-")) && n.ends_with(".json"))
                .map(|n| at(&format!("{dir}/{n}")))
                .collect();
            found.sort();
            found
        };
        let mut more = glob("kernel/prelink/lead");
        more.extend(glob("kernel/prelink/user"));
        // build.sh: external/XnuPrelink, else the kernel base's submodule
        let own = at("external/XnuPrelink");
        let base = at("external/Droid-VM-UniXNU/kits/XnuPrelink");
        let matcher = |dir: &str| oneios_prelink::build::is_matcher_dir(Path::new(dir));
        let xnuprelink = if matcher(&own) || !matcher(&base) { own } else { base };
        let version_dir = std::fs::read_dir(at("kernel/unixnu/profiles/os"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .find(|n| n.ends_with(&format!("-{build}")))
            .unwrap_or_else(|| format!("ios-{build}"));
        let cache = at(&format!("out/kernel-inputs/{build}/kernelcache.macho"));
        Prelink {
            xnu: xnu.to_string(),
            kernelcache: Path::new(&cache).is_file().then_some(cache),
            profile: at(&format!("kernel/unixnu/profiles/os/{version_dir}/profile.toml")),
            reviewed: at(&format!("kernel/prelink/reviewed-{build}-v54.json")),
            more_reviewed: more,
            pairs: at(&format!("kernel/prelink/pairs-{build}.json")),
            stubs: at(&format!("kernel/prelink/stubs-{build}.json")),
            xnuprelink,
        }
    }
}

/// The root tweaks' optional inputs (hook API 1's `ONEIOS_HELPERS`, `ONEIOS_METAL_PLUGIN`, `ONEIOS_CAMERA_PLUGIN`,
/// `ONEIOS_SCALE`, `ONEIOS_ROOTTWEAK_ARGS`, `ONEIOS_ALLOW_NO_METAL`; `config`: another `userland/config`, else the one built into the tool).
/// `runtime_metal` and `runtime_camera` are the staged runtime's Metal bundle and capture plug-in folder (its
/// runtime.json "metal" and "camera", runtime/package.sh's lib/guest/; the phone's `serve make-image` sets them): they
/// stand in for a `metal_plugin` (`camera_plugin`) not given, when the bundle's `oneios-target` is this image's
/// `<device> <build>` (`Tweak::for_image`). `runtime_more` are the runtime's other targets (runtime.json "metals" and
/// "cameras" after the default's: lib/guest/<build>/, iOS 27.2.1's beside 27.0.1's), each a bundle and its capture
/// plug-in folder; `for_image` takes the one whose target is the image.
#[derive(Debug, Clone, Default)]
pub struct Tweak {
    pub helpers: Option<String>,
    pub metal_plugin: Option<String>,
    pub allow_no_metal: bool,
    pub camera_plugin: Option<String>,
    pub scale: Option<String>,
    pub config: Option<String>,
    pub args: Vec<String>,
    pub runtime_metal: Option<String>,
    pub runtime_camera: Option<String>,
    pub runtime_more: Vec<(String, Option<String>)>,
    /// The staged runtime's folder when its runtime.json names the Galaxy A56's GPU plug-ins ("metal_a56"; the phone's
    /// `serve make-image` sets it): an iOS 16.5 set they are for gets them as its kernel.kc.gpuaddon (a56gpu.rs)
    pub runtime_a56: Option<String>,
}

impl Tweak {
    /// image-hook's defaults in a OneiOS checkout: `out/userland/helpers`, `.../NativeAdrenoMetal.bundle`,
    /// `.../camera` where they are there, and the checkout's `userland/config`.
    pub fn from_repo(repo: &str) -> Tweak {
        let at = |rel: &str| Some(format!("{}/{rel}", repo.trim_end_matches('/'))).filter(|p| Path::new(p).is_dir());
        Tweak { helpers: at("out/userland/helpers"), metal_plugin: at("out/userland/NativeAdrenoMetal.bundle"), camera_plugin: at("out/userland/camera"), config: at("userland/config"), ..Tweak::default() }
    }

    /// The inputs for an image of `device` `build`, and where its Metal plug-in comes from (`Built::metal`). A given
    /// `metal_plugin` is kept (the tweaks' preflight refuses one built for another image). Without one, the runtime's
    /// bundle is taken, with the runtime's capture plug-in when no `camera_plugin` is given, only when its
    /// `oneios-target` names this image: a runtime made for the S24 Ultra's iOS 27 gives an image of another iOS
    /// (another phone's fork) neither, and that image composites on the CPU as before. No bundle is never refused:
    /// the phone cannot build one (docs/image-format.md, "The Metal plug-in").
    pub fn for_image(&self, device: &str, build: &str) -> (Tweak, String) {
        let mut t = self.clone();
        let image = format!("{device} {build}");
        if device == "iPhone18,2" && t.allow_no_metal {
            return (t, "none (ONEIOS_ALLOW_NO_METAL=1)".to_string());
        }
        if let Some(given) = &t.metal_plugin {
            return (t.clone(), format!("{given} (given)"));
        }
        // the default target's first, then the others (runtime.json "metals"): the first built for this image
        let mut offered: Vec<(String, Option<String>)> = t.runtime_metal.clone().map(|b| (b, t.runtime_camera.clone())).into_iter().collect();
        offered.extend(t.runtime_more.iter().cloned());
        if offered.is_empty() {
            return (t, "none: none given, and the staged runtime carries none (CPU composition)".to_string());
        }
        let mut seen = Vec::new();
        for (bundle, camera) in offered {
            let target = std::fs::read_to_string(Path::new(&bundle).join("oneios-target")).map(|s| s.trim().to_string()).unwrap_or_default();
            if target != image {
                let target = if target.is_empty() { "no image (no oneios-target)".to_string() } else { target };
                seen.push(format!("({bundle}) is for {target}"));
                continue;
            }
            t.metal_plugin = Some(bundle.clone());
            if t.camera_plugin.is_none() {
                t.camera_plugin = camera;
            }
            return (t, format!("{bundle} (the staged runtime's, for {image})"));
        }
        (t, format!("none: the staged runtime's {}, this image is {image} (CPU composition)", seen.join("; ")))
    }
}

/// What to build.
#[derive(Debug, Clone)]
pub struct Args {
    /// The IPSW profile (profiles/*.toml; on the phone the runtime's `lib/profiles/`).
    pub profile: String,
    /// The IPSW's members (`fetch`'s INDIR).
    pub indir: String,
    /// The set folder: absent, or an empty folder (root's `begin image` makes `image.new`).
    pub out: String,
    /// The build's own files go to `WORK/build` (removed at the end, also when the build stops).
    pub work: String,
    /// Fetch the members first (false: they must be in INDIR, as build.sh wants them).
    pub fetch: bool,
    /// `fetch`'s `--source` (an IPSW URL or file instead of the profile's).
    pub source: Option<String>,
    /// Connections of the fetch.
    pub connections: usize,
    /// Keep the fetch's parts when the build stops (the next run goes on from them).
    pub resume: bool,
    /// Apply the root tweaks (false: build.sh's `--no-hook`, the seed is the OS volume's own `/private/var`).
    pub tweaks: bool,
    /// The root tweaks' inputs.
    pub tweak: Tweak,
    pub kernel: Kernel,
    /// The stage's lists to write (`image.new.sha256`, `image.new.ids`): the set's files, sorted.
    pub sums_list: Option<String>,
    pub ids_list: Option<String>,
    /// Leave `WORK/build` (for a look at the steps' files).
    pub keep_work: bool,
}

/// What the build made: the fields of `make-image`'s `ok` (root's `finish image <files> <format> <version>
/// <build>` takes the first four).
#[derive(Debug, Clone, PartialEq)]
pub struct Built {
    /// The set's files, `manifest.json` included: the lines of the sums list.
    pub files: usize,
    pub format: u64,
    pub version: String,
    pub build: String,
    pub out: String,
    /// The bytes of the set's files.
    pub bytes: u64,
    /// The SHA-256 of `manifest.json`, of `kernel.kc` (when there is one) and of `data.seed`.
    pub manifest_sha256: String,
    pub kernel_sha256: Option<String>,
    pub seed_sha256: String,
    /// Where the Metal plug-in of the Data seed came from, or why there is none (`Tweak::for_image`).
    pub metal: String,
}

/// How a build ends when it does not make the set.
#[derive(Debug)]
pub enum Stop {
    /// The build decided not to: `code` is one of `serve`'s refusal codes (`usage`, `not-found`, `exists`,
    /// `mismatch`, `space`, `not-ported`, `audit`, `outside`).
    Refused(&'static str, String),
    /// A step failed.
    Failed(String),
    /// The fetch failed (`serve` asks the transfer layer whether it was the network's doing).
    Fetch(String),
    /// A cancel or a limit stopped it.
    Stopped(job::Stop),
}

impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Stop::Refused(_, text) | Stop::Failed(text) | Stop::Fetch(text) => f.write_str(text),
            Stop::Stopped(stop) => write!(f, "{stop}"),
        }
    }
}

type R<T> = Result<T, Stop>;

fn failed(text: impl Into<String>) -> Stop {
    Stop::Failed(text.into())
}

fn refused(code: &'static str, text: impl Into<String>) -> Stop {
    Stop::Refused(code, text.into())
}

/// A step of the image tool: a stop that ended it is said as that stop.
fn image(e: oneios_image::Fail) -> Stop {
    match job::stop_reason() {
        Some(stop) => Stop::Stopped(stop),
        None => failed(e.to_string()),
    }
}

fn io(path: &Path, e: std::io::Error) -> Stop {
    failed(format!("{}: {e}", path.display()))
}

/// Between steps: a stop ends the build, a pause holds it here.
fn between() -> R<()> {
    job::check().map_err(Stop::Stopped)?;
    job::hold().map_err(Stop::Stopped)?;
    Ok(())
}

/// `oneios-tool image ARGS...` run in this process (build.sh starts it): what it printed, or the step's failure
/// with the tool's own reason.
fn image_cmd(args: &[&str]) -> R<String> {
    let said = cmd::image(args).map_err(|e| failed(format!("image {}: {e}", args[0])))?;
    if said.status == 0 {
        return Ok(said.out);
    }
    if let Some(stop) = job::stop_reason() {
        return Err(Stop::Stopped(stop));
    }
    let why = said.err.trim().trim_start_matches("imagetool: ").to_string();
    Err(failed(format!("image {}: {}", args[0], if why.is_empty() { format!("exit status {}", said.status) } else { why })))
}

/// The room check of `serve` (one `space` event and the refusal), or of the command line.
pub type Space<'a> = &'a (dyn Fn(&Path, u64) -> Result<(), String> + Sync);

/// One member of the profile, as `image profile --members` lists it.
#[derive(Debug, Clone)]
struct Member {
    role: String,
    path: String,
    /// The input's file name in INDIR (an AEA member's: the decrypted image's; a UDIF member's: the raw image's).
    name: String,
    aea: bool,
    /// A UDIF disk image (iOS 16's OS image), converted to raw APFS in the decrypt step.
    udif: bool,
    size: u64,
    /// Its path in the set (`None`: not part of the set).
    out: Option<String>,
}

/// The profile as build.sh reads it: `image profile --shell` and `--members`.
struct Profile {
    path: String,
    vars: std::collections::HashMap<String, String>,
    members: Vec<Member>,
}

impl Profile {
    fn load(path: &str) -> R<Profile> {
        let text = |form: &str| -> R<String> {
            let said = cmd::image(&["profile", path, &format!("--{form}")]).map_err(|e| failed(format!("image profile: {e}")))?;
            match said.status {
                0 => Ok(said.out),
                _ => Err(refused("usage", format!("{path}: not a profile this tool reads: {}", said.err.trim().trim_start_matches("imagetool: ")))),
            }
        };
        let mut vars = std::collections::HashMap::new();
        for line in text("shell")?.lines() {
            if let Some((k, v)) = line.split_once('=') {
                vars.insert(k.to_string(), unquote(v));
            }
        }
        let mut members = Vec::new();
        for line in text("members")?.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() != 6 {
                return Err(failed(format!("{path}: a member line that is not six fields: {line}")));
            }
            members.push(Member {
                role: f[0].into(),
                path: f[1].into(),
                name: f[2].into(),
                aea: f[3] == "1",
                udif: f[3] == "2",
                size: f[4].parse().unwrap_or(0),
                out: (f[5] != "-").then(|| f[5].to_string()),
            });
        }
        Ok(Profile { path: path.to_string(), vars, members })
    }
    fn var(&self, key: &str) -> &str {
        self.vars.get(key).map(String::as_str).unwrap_or("")
    }
    fn member(&self, role: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.role == role)
    }
}

/// A value of `--shell`'s output: plain, or in single quotes ('\'' for a quote inside).
fn unquote(v: &str) -> String {
    match v.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        Some(inner) => inner.replace("'\\''", "'").replace("'\"'\"'", "'"),
        None => v.to_string(),
    }
}

fn size_of(path: &str) -> Option<u64> {
    std::fs::metadata(path).ok().filter(|m| m.is_file()).map(|m| m.len())
}

/// The record `fetch` keeps in INDIR (`inputs.json`), when it is there.
fn inputs_record(indir: &str) -> Option<oneios_common::json::Value> {
    oneios_common::json::load(&format!("{indir}/inputs.json")).ok()
}

/// An AEA member's decrypted image is in INDIR and is the one `record-plain` recorded (fetch's
/// `decrypted_is_there`, without its look at the .aea).
fn plain_recorded(m: &Member, indir: &str, record: Option<&oneios_common::json::Value>) -> bool {
    let Some(size) = size_of(&format!("{indir}/{}", m.name)) else { return false };
    let rec = record.and_then(|r| r.get("members")).and_then(|ms| ms.get(&m.role));
    rec.is_some_and(|r| r.get("member").and_then(|v| v.as_str()) == Some(m.path.as_str()) && r.get("plain_size").and_then(oneios_common::json::Value::as_int) == Some(size as i128))
}

/// The AEA decryption (since 2026-11-21: `oneios_image::aea_decrypt`, README "aea-decrypt"): PROFILE, the encrypted
/// member, the plain image. `None` would make the build refuse (`not-ported`) when it needs it.
fn aea_decryptor() -> Option<fn(&str, &str, &str) -> Result<(), String>> {
    Some(decrypt_aea)
}

/// One AEA member to its plain image beside it: the key from Apple's key service, kept in INDIR/.aea-keys/ for the
/// second image (both of 24A446 use one key), every byte authenticated, the plain SHA-256 the profile's
/// `plain_sha256` when it pins one, resumable. A step of this build: it says no steps or room of its own (the
/// `check` step asked for all the room), only the `decrypt` step's progress.
fn decrypt_aea(profile: &str, enc: &str, plain: &str) -> Result<(), String> {
    use oneios_image::aea_decrypt::{aea_decrypt, profile_pin, AeaKey, AeaOptions};
    let indir = Path::new(plain).parent().unwrap_or(Path::new("."));
    let opts = AeaOptions {
        key: AeaKey::Service { cache: Some(indir.join(".aea-keys")) },
        jobs: 0,
        resume: true,
        plain_sha256: profile_pin(profile, enc).map_err(|e| e.to_string())?,
        log: &|line| println!("{line}"),
        embedded: true,
    };
    let done = aea_decrypt(enc, plain, &opts).map_err(|e| e.to_string())?;
    println!("{}: {} bytes, SHA-256 {} (key from {})", plain, done.bytes, done.sha256, done.key_from);
    Ok(())
}

/// The device a path is on (its nearest folder that is there).
fn device_of(path: &str) -> Option<u64> {
    let mut at = Path::new(path);
    loop {
        if let Ok(m) = std::fs::metadata(at) {
            return Some(m.dev());
        }
        at = at.parent()?;
    }
}

const GIB: u64 = 1 << 30;

/// Removes what the build wrote when it does not end with the set in place (an error, a cancel, a panic).
struct Undo {
    /// The set folder was made by the build (else: it was there and empty, and only its entries go).
    out_made: bool,
    out: PathBuf,
    /// Entries put into `out`.
    placed: Vec<PathBuf>,
    /// Each list and its length before the build.
    lists: Vec<(PathBuf, Option<u64>)>,
    armed: bool,
}

impl Drop for Undo {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if self.out_made {
            let _ = std::fs::remove_dir_all(&self.out);
        } else {
            for p in &self.placed {
                let _ = if p.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
            }
        }
        for (list, len) in &self.lists {
            match len {
                None => {
                    let _ = std::fs::remove_file(list);
                }
                Some(len) => {
                    let _ = std::fs::OpenOptions::new().write(true).open(list).and_then(|f| f.set_len(*len));
                }
            }
        }
    }
}

/// Build the set (the module's header has the steps). `space` is asked once, before the build writes anything,
/// with the bytes it will write.
pub fn build(args: &Args, space: Space) -> R<Built> {
    let started = std::time::Instant::now();
    // ---- 1. check
    job::step("check");
    let p = Profile::load(&args.profile)?;
    let (build_id, version) = (p.var("P_BUILD").to_string(), p.var("P_VERSION").to_string());
    // the Metal plug-in: the one given, else the staged runtime's when it is for this image (Tweak::for_image)
    let (tweak, metal) = if args.tweaks { args.tweak.for_image(p.var("P_DEVICE"), &build_id) } else { (args.tweak.clone(), "none (no root tweaks)".to_string()) };
    println!("Metal plug-in: {metal}");
    if args.tweaks {
        let device = p.var("P_DEVICE");
        oneios_tweaks::preflight_metal(
            device,
            &build_id,
            tweak.metal_plugin.as_deref().map(Path::new),
            tweak.allow_no_metal,
        )
        .map_err(|e| refused("metal", format!("step check: {e:#}")))?;
    }
    let indir = args.indir.trim_end_matches('/').to_string();
    let record = inputs_record(&indir);
    if !args.fetch {
        // build.sh: one line that names every missing input (an AEA member: its plain image, or the encrypted member,
        // which the decrypt step makes the plain image of)
        let encrypted_there = |m: &Member| (m.aea || m.udif) && size_of(&format!("{indir}/{}", m.path.rsplit('/').next().unwrap_or(&m.path))).is_some();
        let absent: Vec<&str> = p.members.iter().filter(|m| m.out.is_some() && size_of(&format!("{indir}/{}", m.name)).is_none() && !encrypted_there(m)).map(|m| m.name.as_str()).collect();
        if !absent.is_empty() {
            return Err(refused("not-found", format!("missing inputs in {indir}: {} (fetch them first)", absent.join(" "))));
        }
    }
    let to_decrypt: Vec<&Member> = p.members.iter().filter(|m| (m.aea || m.udif) && m.out.is_some() && !plain_recorded(m, &indir, record.as_ref())).collect();
    if to_decrypt.iter().any(|m| m.aea) && aea_decryptor().is_none() {
        return Err(refused(
            "not-ported",
            format!(
                "step decrypt: the AEA decryption is not in this tool yet ({} must be decrypted; its Rust port is being written: docs/agents.md, Interfaces). Nothing was fetched or written",
                to_decrypt.iter().map(|m| m.path.as_str()).collect::<Vec<_>>().join(", ")
            ),
        ));
    }
    let kernel_cache_member = p.member("kernelcache").cloned();
    match &args.kernel {
        Kernel::Kc(kc) if size_of(kc).is_none() => return Err(refused("not-found", format!("missing kernel collection {kc}"))),
        Kernel::Prelink(pl) => {
            let mut wanted = vec![pl.xnu.clone(), pl.profile.clone(), pl.reviewed.clone(), pl.pairs.clone(), pl.stubs.clone()];
            wanted.extend(pl.more_reviewed.iter().cloned());
            match &pl.kernelcache {
                Some(cache) => wanted.push(cache.clone()),
                None if !args.fetch => wanted.push(format!("{indir}/{}", kernel_cache_member.as_ref().map(|m| m.name.as_str()).unwrap_or("kernelcache"))),
                None => {}
            }
            let mut absent: Vec<String> = wanted.into_iter().filter(|f| !Path::new(f).is_file()).collect();
            if !oneios_prelink::build::is_matcher_dir(Path::new(&pl.xnuprelink)) {
                absent.push(format!("{}/fixtures/fileset-*-symbols.json", pl.xnuprelink));
            }
            if !absent.is_empty() {
                return Err(refused("not-found", format!("the kernel's inputs are missing: {}", absent.join(", "))));
            }
            if pl.kernelcache.is_none() && kernel_cache_member.is_none() {
                return Err(refused("usage", format!("{}: no kernelcache member to decode, and no kernelcache given", p.path)));
            }
        }
        _ => {}
    }
    let out = PathBuf::from(&args.out);
    let out_made = match std::fs::symlink_metadata(&out) {
        Ok(m) if m.is_dir() => {
            if std::fs::read_dir(&out).map_err(|e| io(&out, e))?.next().is_some() {
                return Err(refused("exists", format!("{} is not empty", out.display())));
            }
            false
        }
        Ok(_) => return Err(refused("exists", format!("{} is there and is not a folder", out.display()))),
        Err(_) => true,
    };
    let work = PathBuf::from(&args.work).join("build");
    // (a build that was killed left its work folder: it is not resumable, it goes)
    let _ = std::fs::remove_dir_all(&work);

    // The room: what the fetch and the decryption still add to INDIR, the set's own files (the images are hard
    // links when INDIR and the set's folder are one volume), the seed's tree and tar, the kernel's work, the
    // hash lists. Asked once, here.
    let need = {
        let member_there = |m: &Member| if m.aea || m.udif { plain_recorded(m, &indir, record.as_ref()) } else { size_of(&format!("{indir}/{}", m.name)) == Some(m.size) };
        let fetch: u64 = if args.fetch { p.members.iter().filter(|m| !member_there(m)).map(|m| m.size).sum() } else { 0 };
        // the profile's need_gib is the inputs' whole room: the members and the decrypted images beside them
        let members: u64 = p.members.iter().map(|m| m.size).sum();
        let plains = if to_decrypt.is_empty() { 0 } else { (p.var("P_NEED_GIB").parse::<u64>().unwrap_or(30) * GIB).saturating_sub(members) };
        let os_image = p.member("os").map(|m| format!("{indir}/{}", m.name)).unwrap_or_default();
        let data_source = p.var("P_DATA_SOURCE").to_string();
        // (an image our reader cannot read yet is said by the place step; here the guess for 24A446-like sets)
        let (data, entries) = Some(&os_image).filter(|p| size_of(p).is_some()).and_then(|p| data::tree_size(p, &data_source).ok()).unwrap_or((2 * GIB, 30_000));
        // the copied tree, the tar beside it (a 1.5 KiB header per entry), the meta file
        // and what the tweaks place in the tree (the plug-ins, the helpers: about 36 MB for the Metal bundle), twice too
        let placed: u64 = if args.tweaks { [&tweak.helpers, &tweak.metal_plugin, &tweak.camera_plugin].iter().filter_map(|p| p.as_deref()).map(|p| tree_bytes(Path::new(p))).sum() } else { 0 };
        let seed = 2 * (data + placed) + entries * 2048;
        let images: u64 = p.members.iter().filter(|m| m.out.is_some()).map(|m| size_of(&format!("{indir}/{}", m.name)).unwrap_or(m.size)).sum();
        let linked = device_of(&indir).is_some() && device_of(&indir) == device_of(&args.out) && device_of(&args.out) == device_of(&args.work);
        let copies = if linked { 0 } else { images + if to_decrypt.is_empty() { 0 } else { plains } };
        let kernel = match &args.kernel {
            Kernel::None => 0,
            Kernel::Kc(kc) => size_of(kc).unwrap_or(0),
            Kernel::Prelink(pl) => {
                let im4p = kernel_cache_member.as_ref().map_or(0, |m| m.size);
                let cache = pl.kernelcache.as_deref().and_then(size_of).unwrap_or(4 * im4p);
                let xnu = size_of(&pl.xnu).unwrap_or(0);
                // the decode, prelink build's work (serve's `prelink`: the kernel twice, four kernelcaches), the
                // collection in the set
                (if pl.kernelcache.is_none() { cache } else { 0 }) + 2 * xnu + 4 * cache + (64 << 20) + cache * 3 / 2
            }
        };
        let hash_lists = (images + kernel) / (1 << 20) * 32 + (1 << 20);
        fetch + plains + seed + copies + kernel + hash_lists
    };
    space(Path::new(&args.out), need).map_err(|text| refused("space", text))?;

    let mut undo = Undo {
        out_made,
        out: out.clone(),
        placed: Vec::new(),
        lists: [&args.sums_list, &args.ids_list].into_iter().flatten().map(|l| (PathBuf::from(l), std::fs::metadata(l).ok().map(|m| m.len()))).collect(),
        armed: true,
    };
    let work_guard = (!args.keep_work).then(|| job::temp(&work));
    std::fs::create_dir_all(&work).map_err(|e| io(&work, e))?;
    let set = work.join("set");
    std::fs::create_dir_all(&set).map_err(|e| io(&set, e))?;

    // ---- 2. fetch
    if args.fetch {
        between()?;
        job::step("directory");
        oneios_image::fetch(&args.profile, &indir, args.connections.max(1), None, args.source.as_deref(), args.resume).map_err(|e| {
            let text = e.to_string();
            match job::stop_reason() {
                Some(stop) => Stop::Stopped(stop),
                None if text.contains("the profile pins") || text.contains("the profile does not match") => refused("mismatch", text),
                None => Stop::Fetch(text),
            }
        })?;
        job::end();
    }
    let record = inputs_record(&indir);

    // ---- 3. decrypt
    between()?;
    job::step("decrypt");
    for m in p.members.iter().filter(|m| (m.aea || m.udif) && m.out.is_some()) {
        if plain_recorded(m, &indir, record.as_ref()) {
            println!("{}: {} image present", m.path, if m.udif { "raw" } else { "decrypted" });
            continue;
        }
        if m.udif {
            // iOS 16's OS image: its Apple_APFS partition as raw APFS, the table's CRC-32 and the profile's plain_sha256
            // checked (oneios_image::udif), then recorded as an AEA's plain image is; the .dmg goes
            let (dmg, raw) = (format!("{indir}/{}", m.path.rsplit('/').next().unwrap_or(&m.path)), format!("{indir}/{}", m.name));
            let pin = oneios_image::aea_decrypt::profile_pin(&p.path, &dmg).map_err(|e| failed(e.to_string()))?;
            let done = oneios_image::udif::to_raw(&dmg, &raw, pin.as_deref()).map_err(|e| match job::stop_reason() {
                Some(stop) => Stop::Stopped(stop),
                None if e.to_string().contains("the profile pins") => refused("mismatch", e.to_string()),
                None => failed(e.to_string()),
            })?;
            println!("{raw}: {} bytes, SHA-256 {} (the {} partition of {})", done.bytes, done.sha256, done.partition, m.path);
            image_cmd(&["record-plain", &indir, &m.role, &raw])?;
            let _ = std::fs::remove_file(&dmg);
            continue;
        }
        let decrypt = aea_decryptor().ok_or_else(|| refused("not-ported", format!("step decrypt: {} must be decrypted; the AEA decryption is not in this tool yet", m.path)))?;
        let enc = format!("{indir}/{}", m.path.rsplit('/').next().unwrap_or(&m.path));
        let plain = format!("{indir}/{}", m.name);
        decrypt(&p.path, &enc, &plain).map_err(|e| match job::stop_reason() {
            Some(stop) => Stop::Stopped(stop),
            None if e.contains("does not authenticate") || e.contains("does not open") => refused("auth", e),
            None if e.contains("was expected: nothing was kept") => refused("mismatch", e),
            None => failed(e),
        })?;
        image_cmd(&["record-plain", &indir, &m.role, &plain])?;
        let _ = std::fs::remove_file(&enc);
    }

    // ---- 4. place: build.sh's `put` (a hard link when the folder allows it, else a copy)
    between()?;
    job::step("place");
    let present: Vec<&Member> = p.members.iter().filter(|m| m.out.is_some()).collect();
    let absent: Vec<&str> = present.iter().filter(|m| size_of(&format!("{indir}/{}", m.name)).is_none()).map(|m| m.name.as_str()).collect();
    if !absent.is_empty() {
        return Err(refused("not-found", format!("missing inputs in {indir}: {}", absent.join(" "))));
    }
    for m in &present {
        let (from, rel) = (format!("{indir}/{}", m.name), m.out.as_deref().expect("filtered"));
        let to = set.join(rel);
        put(Path::new(&from), &to)?;
        if matches!(m.role.as_str(), "os" | "systemos" | "app") {
            oneios_image::apfs::apfs_info(&to.to_string_lossy()).map_err(|e| failed(format!("{} is not a raw APFS image ({e})", m.name)))?;
        }
    }

    // ---- 5. data: the OS volume's /private/var
    between()?;
    job::step("data");
    let source = p.var("P_DATA_SOURCE").to_string();
    let os = set.join(p.member("os").and_then(|m| m.out.clone()).ok_or_else(|| failed("the profile has no os member in the set"))?);
    let tree = work.join("data");
    let meta = work.join("data.meta.json");
    println!("reading {source} of the OS volume");
    let copied = data::copy(&os.to_string_lossy(), &source, &tree.to_string_lossy(), &meta.to_string_lossy()).map_err(|e| match job::stop_reason() {
        Some(stop) => Stop::Stopped(stop),
        None => failed(e),
    })?;
    println!("copied {} entries{}", copied.entries, if copied.skipped == 0 { String::new() } else { format!(", skipped {} special files", copied.skipped) });
    let overlay = work.join("overlay");
    std::fs::create_dir_all(&overlay).map_err(|e| io(&overlay, e))?;
    let owners = work.join("owners.tsv");
    std::fs::write(&owners, b"").map_err(|e| io(&owners, e))?;
    let tweaks_json = work.join("tweaks.json");
    std::fs::write(&tweaks_json, b"null\n").map_err(|e| io(&tweaks_json, e))?;

    // ---- 6. tweaks
    between()?;
    job::step("tweaks");
    if args.tweaks {
        // hook API 1 as a call (oneios-tweaks): the copied tree changed in place, the overlay, the owner rules, the
        // summary; the OS volume read from its image, as nothing is mounted
        println!("running the root tweaks (oneios-tweaks)");
        let t = &tweak;
        let hook = oneios_tweaks::Hook {
            os_root: PathBuf::new(),
            os_image: Some(os.clone()),
            data: tree.clone(),
            overlay: overlay.clone(),
            build: Some(build_id.clone()),
            device: Some(p.var("P_DEVICE").to_string()),
            helpers: t.helpers.clone().map(PathBuf::from),
            metal_plugin: t.metal_plugin.clone().map(PathBuf::from),
            allow_no_metal: t.allow_no_metal,
            camera_plugin: t.camera_plugin.clone().map(PathBuf::from),
            scale: t.scale.clone(),
            config: t.config.clone().map(PathBuf::from),
            args: t.args.clone(),
            owners: Some(owners.clone()),
            info: Some(work.join("hook-info.json")),
        };
        let done = oneios_tweaks::run_hook(&hook).map_err(|e| match (job::stop_reason(), e.downcast_ref::<oneios_tweaks::Refused>()) {
            (Some(stop), _) => Stop::Stopped(stop),
            (None, Some(why)) => refused("input", format!("step tweaks: {why}")),
            (None, None) => failed(format!("step tweaks: {e:#}")),
        })?;
        // build.sh's record of the hook: what ran, its SHA-256 (the tool's binary), its summary (no git here)
        let exe = std::env::current_exe().map_err(|e| failed(format!("this tool's own file: {e}")))?;
        let mut record = oneios_common::json::Value::dict();
        record.set("hook", "oneios-tool image tweaks");
        record.set("hook_sha256", oneios_image::sha256_of(&exe.to_string_lossy()).map_err(image)?);
        record.set("info", done.info.clone());
        std::fs::write(&tweaks_json, oneios_common::json::dumps(&record, None, false) + "\n").map_err(|e| io(&tweaks_json, e))?;
        println!("root tweaks: {} launchd jobs off, {} owner cdhashes; off: {}", done.disabled_jobs, done.owner_cdhashes, done.off.len());
    } else {
        println!("no root tweaks: the seed is the OS volume's own /{source}");
    }

    // ---- 7. seed
    between()?;
    job::step("seed");
    let epoch: i128 = p.var("P_EPOCH").parse().map_err(|_| failed(format!("{}: [data] epoch is not a whole number", p.path)))?;
    let seed_name = p.var("P_SEED").to_string();
    let (tree_s, meta_s, seed_s, owners_s, epoch_s) = (tree.to_string_lossy().into_owned(), meta.to_string_lossy().into_owned(), set.join(&seed_name).to_string_lossy().into_owned(), owners.to_string_lossy().into_owned(), epoch.to_string());
    let seed_info = image_cmd(&["seed-pack", &tree_s, &meta_s, &seed_s, "--epoch", &epoch_s, "--owners", &owners_s])?.trim_end().to_string();
    let seed_json = work.join("seed.json");
    std::fs::write(&seed_json, format!("{seed_info}\n")).map_err(|e| io(&seed_json, e))?;
    println!("{seed_info}");
    if std::fs::read_dir(&overlay).map_err(|e| io(&overlay, e))?.next().is_some() {
        copy_tree(&overlay, &set.join("overlay"))?;
    }
    // the copied tree is not needed any more: its room goes back before the kernel's work
    let _ = std::fs::remove_dir_all(&tree);

    // ---- 8, 9. the kernel collection
    let kernel_name = p.var("P_KERNEL").to_string();
    let kc = match &args.kernel {
        Kernel::None => None,
        Kernel::Kc(kc) => {
            put(Path::new(kc), &set.join(&kernel_name))?;
            Some(kc.clone())
        }
        Kernel::Prelink(pl) => {
            between()?;
            job::step("kernelcache");
            let cache = match &pl.kernelcache {
                Some(cache) => cache.clone(),
                None => {
                    let m = kernel_cache_member.as_ref().expect("checked");
                    let decoded = work.join("kernelcache.macho");
                    let said = oneios_image::im4p::cmd(&format!("{indir}/{}", m.name), &decoded.to_string_lossy()).map_err(image)?;
                    println!("{said}");
                    decoded.to_string_lossy().into_owned()
                }
            };
            between()?;
            let kout = work.join("kernel");
            let prelink = oneios_prelink::build::Args {
                kernel: pl.xnu.clone(),
                kernelcache: cache,
                profile: pl.profile.clone(),
                reviewed: pl.reviewed.clone(),
                more_reviewed: pl.more_reviewed.clone(),
                pairs: pl.pairs.clone(),
                stubs: pl.stubs.clone(),
                xnuprelink: pl.xnuprelink.clone(),
                reference: None,
                out: kout.to_string_lossy().into_owned(),
                repo: None,
                // (nothing here starts a program: the record's commits are null)
                no_git: true,
                oneios_commit: None,
                xnuprelink_commit: None,
                remove_work: true,
            };
            let built = {
                let _prefix = job::step_prefix(PRELINK_PREFIX);
                oneios_prelink::build::build(&prelink)
            };
            let built = built.map_err(|e| {
                use oneios_prelink::build::Refused;
                match (job::stop_reason(), e.downcast_ref::<Refused>()) {
                    (Some(stop), _) => Stop::Stopped(stop),
                    (None, Some(Refused::NotFound(t))) => refused("not-found", t.clone()),
                    (None, Some(Refused::Mismatch(t))) => refused("mismatch", t.clone()),
                    (None, Some(Refused::Audit(t))) => refused("audit", t.clone()),
                    (None, None) => failed(format!("{e:#}")),
                }
            })?;
            std::fs::rename(&built.kc, set.join(&kernel_name)).map_err(|e| io(Path::new(&built.kc), e))?;
            println!("kernel collection: {} bytes, sha256 {}", built.bytes, built.sha256);
            Some(built.kc)
        }
    };

    // ---- 10. manifest, verify
    between()?;
    job::step("manifest");
    let set_s = set.to_string_lossy().into_owned();
    let inputs_json = format!("{indir}/inputs.json");
    let inputs_json = Path::new(&inputs_json).is_file().then_some(inputs_json);
    let (seed_j, tweaks_j) = (seed_json.to_string_lossy().into_owned(), tweaks_json.to_string_lossy().into_owned());
    let mut margs: Vec<&str> = vec!["manifest", &args.profile, &set_s, "--seed-info", &seed_j, "--tweaks", &tweaks_j];
    if let Some(inputs) = &inputs_json {
        margs.extend(["--inputs", inputs.as_str()]);
    }
    let said = image_cmd(&margs)?.trim_end().to_string();
    println!("{said}");
    // The Galaxy A56's GPU plug-ins of the staged runtime (a56gpu.rs): an iOS 16.5 set they are for gets its
    // kernel.kc.gpuaddon of them and their CDHashes in owner-trust.json (and that entry of manifest.json), before the
    // verify reads the manifest; the sidecar is not in the manifest, so it is added to the lists below by name.
    let mut a56_addon: Option<String> = None;
    if let Some(runtime) = &args.tweak.runtime_a56 {
        let plugins = a56gpu::runtime_plugins(Path::new(runtime)).map_err(failed)?;
        match a56gpu::for_set(&set, plugins.as_ref()).map_err(failed)? {
            Ok(placed) => {
                a56gpu::write(&placed, &set, false).map_err(failed)?;
                println!("{}", a56gpu::counts_line(&placed));
                println!("{}", a56gpu::placed_line(&placed));
                a56_addon = Some(oneios_image::sha256_of(&format!("{set_s}/{}", a56gpu::ADDON)).map_err(image)?);
            }
            Err(why) => println!("@@A56GPU none: {why}"),
        }
    }
    between()?;
    let total: u64 = set_files(&set)?.iter().map(|(_, m)| m.len()).sum();
    job::begin("verify", "bytes", total);
    let said = oneios_image::verify_set(&set_s, false).map_err(|e| match job::stop_reason() {
        Some(stop) => Stop::Stopped(stop),
        None => refused("mismatch", e.to_string()),
    })?;
    job::end();
    println!("{said}");

    // ---- 11. the lists, the set in place
    between()?;
    job::step("lists");
    let manifest = oneios_common::json::load(&format!("{set_s}/manifest.json")).map_err(|e| failed(format!("{set_s}/manifest.json: {e:#}")))?;
    let mut sums: Vec<(String, String)> = Vec::new();
    for f in manifest.get("files").and_then(|f| f.as_list()).unwrap_or(&[]) {
        let (Some(path), Some(sha)) = (f.get("path").and_then(|v| v.as_str()), f.get("sha256").and_then(|v| v.as_str())) else {
            return Err(failed("manifest.json: a file without path or sha256"));
        };
        sums.push((path.to_string(), sha.to_string()));
    }
    let manifest_sha256 = oneios_image::sha256_of(&format!("{set_s}/manifest.json")).map_err(image)?;
    sums.push(("manifest.json".into(), manifest_sha256.clone()));
    if let Some(sha) = a56_addon {
        sums.push((a56gpu::ADDON.into(), sha));
    }
    sums.sort();
    let on_disk = set_files(&set)?;
    if on_disk.len() != sums.len() {
        return Err(failed(format!("the set holds {} files, its manifest lists {} (and itself)", on_disk.len(), sums.len() - 1)));
    }
    let seed_sha256 = sums.iter().find(|(p, _)| *p == seed_name).map(|(_, s)| s.clone()).unwrap_or_default();
    let kernel_sha256 = kc.as_ref().and_then(|_| sums.iter().find(|(p, _)| *p == kernel_name).map(|(_, s)| s.clone()));
    let bytes: u64 = on_disk.iter().map(|(_, m)| m.len()).sum();
    // into OUT: the folder whole when it was not there, else entry by entry
    if out_made {
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
        }
        std::fs::rename(&set, &out).map_err(|e| io(&out, e))?;
    } else {
        for e in std::fs::read_dir(&set).map_err(|e| io(&set, e))? {
            let e = e.map_err(|e| io(&set, e))?;
            let to = out.join(e.file_name());
            std::fs::rename(e.path(), &to).map_err(|err| io(&to, err))?;
            undo.placed.push(to);
        }
    }
    // the lists, as root's `receive` writes them for an import: "SHA256  PATH", "DEV:INODE:SIZE:MTIME PATH"
    if let Some(list) = &args.sums_list {
        let text: String = sums.iter().map(|(path, sha)| format!("{sha}  {path}\n")).collect();
        write_list(list, &text)?;
    }
    if let Some(list) = &args.ids_list {
        let mut text = String::new();
        for (path, _) in &sums {
            let m = std::fs::metadata(out.join(path)).map_err(|e| io(&out.join(path), e))?;
            text.push_str(&format!("{}:{}:{}:{} {path}\n", m.dev(), m.ino(), m.size(), m.mtime()));
        }
        write_list(list, &text)?;
    }
    undo.armed = false;
    match work_guard {
        Some(guard) => drop(guard),
        None => println!("work folder kept: {}", work.display()),
    }
    println!("image set: {} ({} files, {bytes} bytes, {:.1} s, peak memory {} MiB)", out.display(), sums.len(), started.elapsed().as_secs_f64(), peak_rss() >> 20);
    Ok(Built { files: sums.len(), format: 1, version, build: build_id, out: args.out.clone(), bytes, manifest_sha256, kernel_sha256, seed_sha256, metal })
}

/// build.sh's `put`: a hard link when the folder allows it, else a copy.
fn put(from: &Path, to: &Path) -> R<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
    }
    if std::fs::hard_link(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to).map(|_| ()).map_err(|e| failed(format!("{} -> {}: {e}", from.display(), to.display())))
}

/// `cp -R` of the hook's overlay (files and folders only: build.sh refuses anything else).
fn copy_tree(from: &Path, to: &Path) -> R<()> {
    std::fs::create_dir_all(to).map_err(|e| io(to, e))?;
    for e in std::fs::read_dir(from).map_err(|e| io(from, e))? {
        let e = e.map_err(|e| io(from, e))?;
        let kind = e.file_type().map_err(|err| io(&e.path(), err))?;
        if kind.is_dir() {
            copy_tree(&e.path(), &to.join(e.file_name()))?;
        } else if kind.is_file() {
            std::fs::copy(e.path(), to.join(e.file_name())).map_err(|err| io(&e.path(), err))?;
        } else {
            return Err(failed("the root tweaks put something other than files and folders in the overlay"));
        }
    }
    Ok(())
}

/// The files of a folder (paths below it, and their metadata), not following links.
fn set_files(dir: &Path) -> R<Vec<(String, std::fs::Metadata)>> {
    fn walk(dir: &Path, below: &str, out: &mut Vec<(String, std::fs::Metadata)>) -> std::io::Result<()> {
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            let rel = if below.is_empty() { name } else { format!("{below}/{name}") };
            let m = std::fs::symlink_metadata(e.path())?;
            if m.is_dir() {
                walk(&e.path(), &rel, out)?;
            } else {
                out.push((rel, m));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out).map_err(|e| io(dir, e))?;
    Ok(out)
}

/// A list written whole (it was empty, as root's `begin` leaves it): beside it first, then renamed, mode 0600.
fn write_list(list: &str, text: &str) -> R<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let part = format!("{list}.part");
    let guard = job::temp(&part);
    let _ = std::fs::remove_file(&part);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&part).map_err(|e| io(Path::new(&part), e))?;
    std::io::Write::write_all(&mut f, text.as_bytes()).map_err(|e| io(Path::new(&part), e))?;
    drop(f);
    std::fs::rename(&part, list).map_err(|e| io(Path::new(list), e))?;
    guard.keep();
    Ok(())
}

// ---------------------------------------------------------------- the command line

const USAGE: &str = "\
oneios-tool image build: the whole image set in one process (scripts/image/build.sh's steps; tools/oneios-tool/README.md \"image build\")

    oneios-tool image build PROFILE --indir DIR --out SET [--work DIR] [options]

  --indir DIR          the IPSW's members (`image fetch`'s INDIR); fetched into it first unless --no-fetch
  --out SET            the set folder: absent or empty
  --work DIR           the build's own files go to DIR/build (default: SET's folder); removed at the end
  --no-fetch           the members must be in DIR already (build.sh's way)
  --source URL|FILE    fetch from this IPSW instead of the profile's URL
  --resume             keep the fetch's parts when the build stops
  --no-tweaks          no root tweaks: the seed is the OS volume's own /private/var (build.sh --no-hook)
  kernel (one of):
  --kernel-kc FILE     put this kernel.kc into the set (build.sh --kernel FILE)
  --no-kernel          no kernel collection (build.sh --no-kernel)
  --xnu FILE           prelink this hosted kernel (the runtime's kernel/xnu.macho) with Apple's kernelcache, with
                       the pairing files of --kb DIR (laid out as the stage's kb/) or of --repo DIR (a OneiOS
                       checkout, as kernel/prelink/build.sh takes them), each one overridable:
                       --kernelcache FILE (decoded; default: decode INDIR's kernelcache member), --prelink-profile,
                       --reviewed, --more-reviewed (repeatable), --pairs, --stubs, --xnuprelink DIR
  --sums-list FILE     write the set's `SHA256  PATH` list (root's image.sha256)
  --ids-list FILE      write the set's `DEV:INODE:SIZE:MTIME PATH` list (root's image.ids)
  --keep-work          leave DIR/build
  the root tweaks (oneios-tweaks; image-hook's inputs, all optional; --repo DIR gives image-hook's defaults there):
  --helpers DIR, --metal-plugin BUNDLE, --camera-plugin DIR, --scale 2|3, --tweaks-config DIR (default: the
                       userland/config built into the tool), --tweak-arg ARG (repeatable: more `apply` options)
  ONEIOS_ALLOW_NO_METAL=1 deliberately skips Metal for a CPU-only S24 Ultra image
  -j N                 connections of the fetch (default 8)
";

/// `oneios-tool image build ARGS...`: the exit status (0 done, 1 failed, 2 refused, 3 usage).
pub fn main(args: &[String]) -> i32 {
    let parsed = match parse(args) {
        Ok(Some(a)) => a,
        Ok(None) => {
            print!("{USAGE}");
            return 0;
        }
        Err(e) => {
            eprintln!("image build: {e}\n\n{USAGE}");
            return 3;
        }
    };
    let space = |path: &Path, need: u64| -> Result<(), String> {
        let Some((at, free)) = free_bytes(path) else { return Ok(()) };
        eprintln!("space: {} needs {need} bytes and {RESERVE} must stay free; {free} are free", at.display());
        if free < need.saturating_add(RESERVE) {
            return Err(format!("not enough free space in {}: {need} bytes are needed and {RESERVE} must stay free, {free} are free", at.display()));
        }
        Ok(())
    };
    match build(&parsed, &space) {
        Ok(built) => {
            println!(
                "ok: {} files, format {}, iOS {} ({}), {} bytes; manifest.json {}, kernel.kc {}, data.seed {}",
                built.files,
                built.format,
                built.version,
                built.build,
                built.bytes,
                built.manifest_sha256,
                built.kernel_sha256.as_deref().unwrap_or("none"),
                built.seed_sha256
            );
            0
        }
        Err(Stop::Refused(code, text)) => {
            eprintln!("image build: refused ({code}): {text}");
            2
        }
        Err(e) => {
            eprintln!("image build: {e}");
            1
        }
    }
}

/// The room that must stay free (as `serve`'s 256 MiB).
const RESERVE: u64 = 256 << 20;

fn free_bytes(path: &Path) -> Option<(PathBuf, u64)> {
    let mut at = path;
    while !at.exists() {
        at = at.parent()?;
    }
    let c = std::ffi::CString::new(at.as_os_str().as_encoded_bytes()).ok()?;
    // Safety: statvfs fills the zeroed block it is given; the path is a C string.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some((at.to_path_buf(), st.f_bavail as u64 * st.f_frsize as u64))
}

fn parse(args: &[String]) -> Result<Option<Args>, String> {
    let mut profile = None;
    let (mut indir, mut out, mut work, mut source, mut kc, mut xnu, mut kb, mut repo, mut cache) = (None, None, None, None, None, None, None, None, None);
    let (mut pprofile, mut reviewed, mut pairs, mut stubs, mut xnuprelink, mut sums, mut ids) = (None, None, None, None, None, None, None);
    let mut more: Vec<String> = Vec::new();
    let (mut t_helpers, mut t_metal, mut t_camera, mut t_scale, mut t_config, mut t_args): (Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Vec<String>) = (None, None, None, None, None, Vec::new());
    let (mut fetch, mut resume, mut tweaks, mut no_kernel, mut keep_work, mut connections) = (true, false, true, false, false, 8usize);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "-h" | "--help" => return Ok(None),
            "--indir" => indir = Some(value(a)?),
            "--out" => out = Some(value(a)?),
            "--work" => work = Some(value(a)?),
            "--source" => source = Some(value(a)?),
            "--no-fetch" => fetch = false,
            "--resume" => resume = true,
            "--no-tweaks" => tweaks = false,
            "--kernel-kc" => kc = Some(value(a)?),
            "--no-kernel" => no_kernel = true,
            "--xnu" => xnu = Some(value(a)?),
            "--kb" => kb = Some(value(a)?),
            "--repo" => repo = Some(value(a)?),
            "--kernelcache" => cache = Some(value(a)?),
            "--prelink-profile" => pprofile = Some(value(a)?),
            "--reviewed" => reviewed = Some(value(a)?),
            "--more-reviewed" => more.push(value(a)?),
            "--pairs" => pairs = Some(value(a)?),
            "--stubs" => stubs = Some(value(a)?),
            "--xnuprelink" => xnuprelink = Some(value(a)?),
            "--sums-list" => sums = Some(value(a)?),
            "--ids-list" => ids = Some(value(a)?),
            "--keep-work" => keep_work = true,
            "--helpers" => t_helpers = Some(value(a)?),
            "--metal-plugin" => t_metal = Some(value(a)?),
            "--camera-plugin" => t_camera = Some(value(a)?),
            "--scale" => t_scale = Some(value(a)?),
            "--tweaks-config" => t_config = Some(value(a)?),
            "--tweak-arg" => t_args.push(value(a)?),
            "-j" => connections = value(a)?.parse().map_err(|_| "-j needs a number".to_string())?,
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            s if profile.is_none() => profile = Some(s.to_string()),
            _ => return Err("one profile at a time".into()),
        }
    }
    let profile = profile.ok_or("no PROFILE")?;
    let (indir, out) = (indir.ok_or("--indir DIR is needed")?, out.ok_or("--out SET is needed")?);
    let work = work.unwrap_or_else(|| Path::new(&out).parent().map(|p| p.to_string_lossy().into_owned()).filter(|p| !p.is_empty()).unwrap_or_else(|| ".".into()));
    let repo_for_tweaks = repo.clone();
    let kernel = match (kc, no_kernel, xnu) {
        (Some(kc), false, None) => Kernel::Kc(kc),
        (None, true, None) => Kernel::None,
        (None, false, Some(xnu)) => {
            let build = Profile::load(&profile).map(|p| p.var("P_BUILD").to_string()).unwrap_or_default();
            let mut pl = match (kb, repo) {
                (Some(kb), None) => Prelink::from_kb(&xnu, &kb),
                (None, Some(repo)) => Prelink::from_repo(&xnu, &repo, &build),
                (None, None) => Prelink { xnu: xnu.clone(), kernelcache: None, profile: String::new(), reviewed: String::new(), more_reviewed: Vec::new(), pairs: String::new(), stubs: String::new(), xnuprelink: String::new() },
                _ => return Err("--kb or --repo, not both".into()),
            };
            match cache.as_deref() {
                // (`-`: decode the IPSW's kernelcache member, as the phone does without kb/kernelcache.macho)
                Some("-") => pl.kernelcache = None,
                Some(_) => pl.kernelcache = cache,
                None => {}
            }
            if !more.is_empty() {
                pl.more_reviewed = more;
            }
            for (slot, given) in [(&mut pl.profile, pprofile), (&mut pl.reviewed, reviewed), (&mut pl.pairs, pairs), (&mut pl.stubs, stubs), (&mut pl.xnuprelink, xnuprelink)] {
                if let Some(given) = given {
                    *slot = given;
                }
            }
            if [&pl.profile, &pl.reviewed, &pl.pairs, &pl.stubs, &pl.xnuprelink].iter().any(|s| s.is_empty()) {
                return Err("--xnu needs the pairing files: --kb DIR, --repo DIR, or each of --prelink-profile, --reviewed, --pairs, --stubs, --xnuprelink".into());
            }
            Kernel::Prelink(pl)
        }
        _ => return Err("one of --kernel-kc FILE, --no-kernel, --xnu FILE".into()),
    };
    // the root tweaks: image-hook's defaults from --repo, each option over them
    let mut tweak = repo_for_tweaks.as_deref().map(Tweak::from_repo).unwrap_or_default();
    tweak.allow_no_metal = std::env::var("ONEIOS_ALLOW_NO_METAL").map_or(false, |value| value == "1");
    for (slot, given) in [(&mut tweak.helpers, t_helpers), (&mut tweak.metal_plugin, t_metal), (&mut tweak.camera_plugin, t_camera), (&mut tweak.scale, t_scale), (&mut tweak.config, t_config)] {
        if given.is_some() {
            *slot = given;
        }
    }
    tweak.args.extend(t_args);
    Ok(Some(Args { profile, indir, out, work, fetch, source, connections, resume, tweaks, tweak, kernel, sums_list: sums, ids_list: ids, keep_work }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prelink_steps_are_prelink_builds() {
        let ours: Vec<&str> = STEPS.iter().filter_map(|(name, _)| name.strip_prefix(PRELINK_PREFIX)).collect();
        let theirs: Vec<&str> = oneios_prelink::build::STEPS.iter().map(|(name, _)| *name).collect();
        assert_eq!(ours, theirs);
    }

    #[test]
    fn unquote_as_shell() {
        assert_eq!(unquote("24A446"), "24A446");
        assert_eq!(unquote("'ios-27.0.1-24A446-iPhone18,2'"), "ios-27.0.1-24A446-iPhone18,2");
        assert_eq!(unquote("'it'\\''s'"), "it's");
        assert_eq!(unquote(r#"'it'"'"'s x'"#), "it's x");
    }

    #[test]
    fn the_runtimes_plugins_go_with_their_own_image_only() {
        let dir = std::env::temp_dir().join(format!("oneios-imagebuild-for-image.{}", std::process::id()));
        let bundle = dir.join("NativeAdrenoMetal.bundle");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(bundle.join("oneios-target"), "iPhone18,2 24A446\n").unwrap();
        let runtime = Tweak { runtime_metal: Some(bundle.display().to_string()), runtime_camera: Some("CAMERA".into()), ..Tweak::default() };
        // its own image: the bundle, and the runtime's capture plug-in with it
        let (t, said) = runtime.for_image("iPhone18,2", "24A446");
        assert_eq!((t.metal_plugin.as_deref(), t.camera_plugin.as_deref()), (Some(bundle.to_str().unwrap()), Some("CAMERA")));
        assert!(said.ends_with("(the staged runtime's, for iPhone18,2 24A446)"), "{said}");
        // a camera plug-in given is kept
        let given = Tweak { camera_plugin: Some("MINE".into()), ..runtime.clone() };
        assert_eq!(given.for_image("iPhone18,2", "24A446").0.camera_plugin.as_deref(), Some("MINE"));
        // another image (the A56's iOS 16.5): neither
        let (t, said) = runtime.for_image("iPhone15,4", "20F66");
        assert_eq!((t.metal_plugin, t.camera_plugin), (None, None));
        assert!(said.contains(") is for iPhone18,2 24A446, this image is iPhone15,4 20F66"), "{said}");
        // two targets (runtime.json "metals"): each image takes its own bundle and the capture plug-in beside it
        let other = dir.join("24B156/NativeAdrenoMetal.bundle");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("oneios-target"), "iPhone18,2 24B156\n").unwrap();
        let both = Tweak { runtime_more: vec![(other.display().to_string(), Some("CAMERA-B".into()))], ..runtime.clone() };
        let (t, said) = both.for_image("iPhone18,2", "24B156");
        assert_eq!((t.metal_plugin.as_deref(), t.camera_plugin.as_deref()), (Some(other.to_str().unwrap()), Some("CAMERA-B")));
        assert!(said.ends_with("(the staged runtime's, for iPhone18,2 24B156)"), "{said}");
        let (t, _) = both.for_image("iPhone18,2", "24A446");
        assert_eq!((t.metal_plugin.as_deref(), t.camera_plugin.as_deref()), (Some(bundle.to_str().unwrap()), Some("CAMERA")));
        let (t, said) = both.for_image("iPhone18,2", "24C1");
        assert_eq!(t.metal_plugin, None);
        assert!(said.contains(") is for iPhone18,2 24A446; (") && said.contains(") is for iPhone18,2 24B156, this image is iPhone18,2 24C1"), "{said}");
        // ONEIOS_ALLOW_NO_METAL=1 on the S24 Ultra's image: none, as the tweaks' preflight takes it
        let (t, said) = Tweak { allow_no_metal: true, ..runtime.clone() }.for_image("iPhone18,2", "24A446");
        assert_eq!((t.metal_plugin, said.as_str()), (None, "none (ONEIOS_ALLOW_NO_METAL=1)"));
        // a bundle without oneios-target is for no image
        std::fs::remove_file(bundle.join("oneios-target")).unwrap();
        assert!(runtime.for_image("iPhone18,2", "24A446").1.contains("is for no image (no oneios-target)"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The most memory this process has held (getrusage's ru_maxrss: KiB on Linux and Android, bytes on macOS).
pub fn peak_rss() -> u64 {
    // Safety: getrusage fills the zeroed block it is given.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) } != 0 {
        return 0;
    }
    let max = ru.ru_maxrss.max(0) as u64;
    if cfg!(target_os = "macos") {
        max
    } else {
        max << 10
    }
}

/// The bytes of the files below `path` (a file: its own; links not followed; what cannot be read: 0).
fn tree_bytes(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
    if !meta.is_dir() {
        return if meta.is_file() { meta.len() } else { 0 };
    }
    std::fs::read_dir(path).map(|entries| entries.flatten().map(|e| tree_bytes(&e.path())).sum()).unwrap_or(0)
}
