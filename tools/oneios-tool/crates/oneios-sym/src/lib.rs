//! oneios-tool sym: name user addresses of a boot log in the image set's dyld shared cache. The port of
//! `scripts/debug/dsc.py`: the same output lines for the same input, answered from a persistent index
//! (`index.rs`) so that a call takes milliseconds and does not read the 6 GiB cache again.
//!
//! Library use (the log tool): `open_index(&Locate::from_env(), &mut stderr)`, then `Index::find_slide` and
//! `Index::name`.

pub mod cache;
pub mod index;

pub use cache::Cache;
pub use index::{Index, Name, Slide};

use anyhow::{anyhow, bail, Context, Result};
use rayon::prelude::*;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const USAGE: &str = "\
usage: oneios-tool sym [--slide HEX] [--stdin | -] [--dsc DIR] [--index DIR] [--no-index] ADDR...
       oneios-tool sym index [--force] [--dsc DIR] [--index DIR]

Name user addresses of a boot log (the kernel's sheng-exc / sheng-ubt / oneios-ubt lines) in the image set's dyld
shared cache: image + nearest symbol (local symbols from the .symbols file, else the image's own symbol table) for
each address. The output is that of scripts/debug/dsc.py.

Without --slide the slide is found from the addresses themselves: the one multiple of 0x4000 below maxSlide for
which the most addresses sit right after a call instruction (give return addresses of one backtrace).

The cache is the folder ONEIOS_DSC_DIR or --dsc DIR (the one that holds dyld_shared_cache_arm64e and its .NN and
.symbols files). It is read once, to build an index in out/dsc-index/<cache UUID>/ of this repository (or under
ONEIOS_DSC_INDEX or --index DIR); after that the answers come from the index and the cache is not needed: without
ONEIOS_DSC_DIR the index built last is used.
  --slide HEX   the slide, not searched for
  --stdin, -    read the addresses from standard input (any number per line), after those of the command line
  --no-index    read the cache for every answer, as dsc.py does (slow)
  index         build the index now (--force: again, when it is there)

Kernel addresses (a panic's caller, pc, lr and frame lines) are `oneios-tool sym kernel`'s: its own --help.";

/// Where the cache and the indexes are.
#[derive(Clone, Debug, Default)]
pub struct Locate {
    /// The cache's folder: `--dsc`, else ONEIOS_DSC_DIR.
    pub dsc: Option<PathBuf>,
    /// The folder that holds one index folder per cache UUID: `--index`, else ONEIOS_DSC_INDEX, else
    /// `out/dsc-index` of the repository that holds the tool.
    pub index_parent: Option<PathBuf>,
}

impl Locate {
    pub fn from_env() -> Locate {
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        Locate { dsc: var("ONEIOS_DSC_DIR"), index_parent: var("ONEIOS_DSC_INDEX") }
    }

    fn parent(&self) -> Result<PathBuf> {
        if let Some(dir) = &self.index_parent {
            return Ok(dir.clone());
        }
        let is_root = |dir: &Path| dir.join("tools/oneios-tool/Cargo.toml").is_file();
        let exe = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok());
        let cwd = std::env::current_dir().ok();
        for start in [exe, cwd].into_iter().flatten() {
            if let Some(root) = start.ancestors().find(|d| is_root(d)) {
                return Ok(root.join("out/dsc-index"));
            }
        }
        bail!("no OneiOS repository above the tool or the current folder: set ONEIOS_DSC_INDEX to the folder for the index")
    }
}

/// Open the index of the cache (`loc.dsc`), building it first when it is not there (one line on `err`). Without
/// a cache folder, or when the cache cannot be read (not mounted now): the index built last.
pub fn open_index(loc: &Locate, err: &mut dyn Write) -> Result<Index> {
    let parent = loc.parent()?;
    if let Some(dsc) = &loc.dsc {
        let uuid = match cache::read_uuid(dsc) {
            Ok(uuid) => index::uuid_hex(&uuid),
            Err(e) => {
                let index = open_latest(&parent).map_err(|_| e)?;
                let _ = writeln!(err, "oneios-tool sym: {} cannot be read: using the index built last, of cache {}", dsc.display(), index::uuid_hex(&index.uuid));
                return Ok(index);
            }
        };
        let dir = parent.join(&uuid);
        if !Index::is_current(&dir) {
            let _ = writeln!(err, "oneios-tool sym: no index of cache {uuid} in {}: building it (once)", parent.display());
            let report = build_index(dsc, &parent)?;
            let _ = writeln!(err, "oneios-tool sym: index built in {:.1} s", report.seconds);
        }
        return Index::open(&dir);
    }
    open_latest(&parent)
}

/// The index built last (`latest` names it), or the only one there is.
fn open_latest(parent: &Path) -> Result<Index> {
    let latest = std::fs::read_to_string(parent.join("latest")).ok().map(|s| s.trim().to_string());
    let mut found: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.flatten() {
            if Index::is_current(&entry.path()) {
                found.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    found.sort();
    let pick = match latest {
        Some(name) if found.contains(&name) => name,
        _ if found.len() == 1 => found[0].clone(),
        _ if found.is_empty() => bail!(
            "no index in {} and no cache: set ONEIOS_DSC_DIR to the folder that holds {} (see --help)",
            parent.display(),
            cache::MAIN_NAME
        ),
        _ => bail!("{} holds {} indexes and no `latest`: set ONEIOS_DSC_DIR", parent.display(), found.len()),
    };
    Index::open(&parent.join(pick))
}

fn build_index(dsc: &Path, parent: &Path) -> Result<index::BuildReport> {
    std::fs::create_dir_all(parent).with_context(|| format!("{}", parent.display()))?;
    let cache = Cache::open(dsc)?;
    index::build(&cache, parent)
}

/// Python's `int(text, 16)`: an optional sign, an optional 0x, hex digits (single underscores between them).
pub fn parse_hex(text: &str) -> Option<i128> {
    let t = text.trim();
    let (negative, t) = match t.as_bytes().first()? {
        b'-' => (true, &t[1..]),
        b'+' => (false, &t[1..]),
        _ => (false, t),
    };
    let (digits, prefixed) = match t.get(..2) {
        Some("0x") | Some("0X") => (&t[2..], true),
        _ => (t, false),
    };
    let mut value: i128 = 0;
    let mut last_digit = false;
    let mut any = false;
    for (i, c) in digits.chars().enumerate() {
        if c == '_' {
            if !(last_digit || (i == 0 && prefixed)) {
                return None;
            }
            last_digit = false;
            continue;
        }
        value = value.checked_mul(16)?.checked_add(c.to_digit(16)? as i128)?;
        last_digit = true;
        any = true;
    }
    (any && last_digit).then_some(if negative { -value } else { value })
}

/// Python's `f'{v:#x}'`.
fn hex(v: i128) -> String {
    if v < 0 {
        format!("-{:#x}", v.unsigned_abs())
    } else {
        format!("{v:#x}")
    }
}

struct Options {
    slide: Option<i128>,
    stdin: bool,
    no_index: bool,
    force: bool,
    build: bool,
    addrs: Vec<i128>,
}

/// `oneios-tool sym`'s command line (clap). `index` as the first word builds the index; `-` reads the addresses
/// from standard input, as `--stdin` does.
#[derive(clap::Args, Debug)]
pub struct Cli {
    /// The slide of the addresses (hex). Without it the slide is found from the addresses themselves.
    #[arg(long, value_name = "HEX")]
    slide: Option<String>,
    /// The folder that holds dyld_shared_cache_arm64e (default: ONEIOS_DSC_DIR)
    #[arg(long, value_name = "DIR")]
    dsc: Option<PathBuf>,
    /// The folder the index is kept in (default: ONEIOS_DSC_INDEX, else out/dsc-index)
    #[arg(long, value_name = "DIR")]
    index: Option<PathBuf>,
    /// Read addresses from standard input too
    #[arg(long)]
    stdin: bool,
    /// Read the cache itself instead of the index
    #[arg(long = "no-index")]
    no_index: bool,
    /// `index`: build it again even when it is current
    #[arg(long)]
    force: bool,
    /// Addresses (hex), `-` for standard input, or `index` first to build the index
    #[arg(value_name = "ADDR", allow_hyphen_values = true)]
    words: Vec<String>,
}

#[derive(clap::Parser)]
#[command(name = "oneios-tool sym", args_override_self = true, disable_help_flag = true)]
struct Standalone {
    #[command(flatten)]
    cli: Cli,
}

fn options(cli: Cli, loc: &mut Locate) -> Result<Options> {
    let mut o = Options { slide: None, stdin: cli.stdin, no_index: cli.no_index, force: cli.force, build: false, addrs: Vec::new() };
    let address = |text: &str| parse_hex(text).ok_or_else(|| anyhow!("invalid hex address: {text:?}"));
    if let Some(slide) = &cli.slide {
        o.slide = Some(address(slide)?);
    }
    if cli.dsc.is_some() {
        loc.dsc = cli.dsc;
    }
    if cli.index.is_some() {
        loc.index_parent = cli.index;
    }
    // An address may be negative (dsc.py's int() takes "-0x20"), so the words may start with a hyphen; clap then
    // hands over an option that follows a word as a word too. Such an option is read here, as it always was.
    let mut words = cli.words.iter().enumerate();
    while let Some((i, word)) = words.next() {
        let mut value = || words.next().map(|(_, v)| v.as_str()).ok_or_else(|| anyhow!("{word} needs a value"));
        match word.as_str() {
            "index" if i == 0 => o.build = true,
            "-" | "--stdin" => o.stdin = true,
            "--no-index" => o.no_index = true,
            "--force" => o.force = true,
            "--slide" => o.slide = Some(address(value()?)?),
            "--dsc" => loc.dsc = Some(PathBuf::from(value()?)),
            "--index" => loc.index_parent = Some(PathBuf::from(value()?)),
            other if other.starts_with("--") => bail!("unknown option {other}"),
            other => o.addrs.push(address(other)?),
        }
    }
    Ok(o)
}

/// Run `oneios-tool sym ARGS...`; `args` are the arguments after the tool's name. Returns the exit status.
pub fn run(args: &[String]) -> i32 {
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let status = run_with(args, Locate::from_env(), &mut std::io::stdin().lock(), &mut out, &mut std::io::stderr());
    let _ = out.flush();
    status
}

/// `run` with its surroundings passed in (the tests).
pub fn run_with(args: &[String], loc: Locate, input: &mut dyn Read, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        let _ = writeln!(out, "{USAGE}");
        return 0;
    }
    let cli = match <Standalone as clap::Parser>::try_parse_from(std::iter::once("sym".to_string()).chain(args.iter().cloned())) {
        Ok(parsed) => parsed.cli,
        Err(e) => {
            let _ = writeln!(err, "oneios-tool sym: {}", e.to_string().trim_end());
            return 1;
        }
    };
    run_cli(cli, loc, input, out, err)
}

/// Run a parsed command line (oneios-tool's main parses it); the exit status.
pub fn main(cli: Cli) -> i32 {
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let status = run_cli(cli, Locate::from_env(), &mut std::io::stdin().lock(), &mut out, &mut std::io::stderr());
    let _ = out.flush();
    status
}

fn run_cli(cli: Cli, loc: Locate, input: &mut dyn Read, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    match run_inner(cli, loc, input, out, err) {
        Ok(status) => status,
        // The reader of the output went away (`| head`): not an error.
        Err(e) if e.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe) => 0,
        Err(e) => {
            let _ = writeln!(err, "oneios-tool sym: {e:#}");
            1
        }
    }
}

fn run_inner(cli: Cli, mut loc: Locate, input: &mut dyn Read, out: &mut dyn Write, err: &mut dyn Write) -> Result<i32> {
    let mut o = options(cli, &mut loc)?;
    if o.build {
        return build_command(&o, &loc, out);
    }
    if o.stdin {
        let mut text = String::new();
        input.read_to_string(&mut text).context("standard input")?;
        for word in text.split_whitespace() {
            o.addrs.push(parse_hex(word).ok_or_else(|| anyhow!("invalid hex address: {word:?}"))?);
        }
    }
    if o.no_index {
        let Some(dsc) = &loc.dsc else {
            let _ = writeln!(err, "{USAGE}");
            return Ok(1);
        };
        return direct(&Cache::open(dsc)?, &o, out).map(|()| 0);
    }
    let index = match open_index(&loc, err) {
        Ok(index) => index,
        Err(e) if loc.dsc.is_none() => {
            // dsc.py without ONEIOS_DSC_DIR: its usage, status 1.
            let _ = writeln!(err, "{USAGE}\n\noneios-tool sym: {e:#}");
            return Ok(1);
        }
        Err(e) => return Err(e),
    };
    let base = index.base as i128;
    let end = base + index.size as i128 + index.max_slide as i128;
    let inside: Vec<u64> = o.addrs.iter().filter(|&&a| base <= a && a < end).map(|&a| a as u64).collect();
    let slide = match o.slide {
        Some(slide) => slide,
        None => {
            let (mut scores, unaligned) = index.slide_scores(&inside);
            if !unaligned.is_empty() {
                // The bitmap holds aligned words only; dsc.py reads the unaligned word of the cache.
                match loc.dsc.as_ref().and_then(|dsc| Cache::open(dsc).ok()) {
                    Some(cache) => {
                        if cache.uuid != index.uuid {
                            bail!("{}: not the cache of the index {}", cache.dir.display(), index.dir.display());
                        }
                        let extra = direct_scores(&cache, &unaligned);
                        for (score, more) in scores.iter_mut().zip(extra) {
                            *score += more;
                        }
                    }
                    None => {
                        let _ = writeln!(
                            err,
                            "oneios-tool sym: {} address(es) are not multiples of 4: not counted in the slide search (set ONEIOS_DSC_DIR to the cache to count them as dsc.py does)",
                            unaligned.len()
                        );
                    }
                }
            }
            let found = index::pick_slide(&scores, inside.len());
            slide_line(out, &found)?;
            found.slide as i128
        }
    };
    for &a in &o.addrs {
        let u = a - slide;
        let name = u64::try_from(u).ok().and_then(|u| index.name(u));
        match name {
            None => writeln!(out, "{}  (not in a cache image; unslid {})", hex(a), hex(u))?,
            Some(Name { image, symbol: Some((symbol, offset)) }) => writeln!(out, "{}  {image}  {symbol} + {offset:#x}", hex(a))?,
            Some(Name { image, symbol: None }) => writeln!(out, "{}  {image}  ?", hex(a))?,
        }
    }
    Ok(0)
}

fn slide_line(out: &mut dyn Write, found: &Slide) -> Result<()> {
    write!(out, "slide {:#x} ({} of {} addresses follow a call", found.slide, found.score, found.inside)?;
    if found.ties > 1 {
        writeln!(
            out,
            "; {} slides fit as well: NOT reliable, give a whole backtrace (8 or more return addresses) or --slide)",
            found.ties
        )?;
    } else {
        writeln!(out, ")")?;
    }
    Ok(())
}

/// The slide candidates' scores read from the cache itself, as dsc.py does.
fn direct_scores(cache: &Cache, inside: &[u64]) -> Vec<u32> {
    let candidates = cache.max_slide.div_ceil(index::SLIDE_STEP);
    (0..candidates)
        .into_par_iter()
        .map(|k| {
            let slide = k * index::SLIDE_STEP;
            inside
                .iter()
                .filter(|&&a| {
                    a.checked_sub(slide + 4).and_then(|va| cache.word(va)).is_some_and(cache::is_call)
                })
                .count() as u32
        })
        .collect()
}

/// dsc.py's `main`, reading the cache for everything.
fn direct(cache: &Cache, o: &Options, out: &mut dyn Write) -> Result<()> {
    let base = cache.base as i128;
    let end = base + cache.size as i128 + cache.max_slide as i128;
    let inside: Vec<u64> = o.addrs.iter().filter(|&&a| base <= a && a < end).map(|&a| a as u64).collect();
    let slide = match o.slide {
        Some(slide) => slide,
        None => {
            let found = index::pick_slide(&direct_scores(cache, &inside), inside.len());
            slide_line(out, &found)?;
            found.slide as i128
        }
    };
    let ranges = cache.text_ranges()?;
    let mut table: HashMap<u32, Vec<(u64, std::borrow::Cow<str>)>> = HashMap::new();
    for &a in &o.addrs {
        let u = a - slide;
        let range = u64::try_from(u).ok().and_then(|u| {
            let i = ranges.partition_point(|r| r.start <= u);
            let r = ranges.get(i.checked_sub(1)?)?;
            (r.start <= u && u < r.end).then_some((u, r.image))
        });
        let Some((u, image)) = range else {
            writeln!(out, "{}  (not in a cache image; unslid {})", hex(a), hex(u))?;
            continue;
        };
        if let std::collections::hash_map::Entry::Vacant(slot) = table.entry(image) {
            slot.insert(cache.symbols(image as usize)?);
        }
        let syms = &table[&image];
        let path = &cache.images[image as usize].1;
        let file = path.rsplit('/').next().unwrap_or(path);
        match syms.partition_point(|s| s.0 <= u).checked_sub(1) {
            Some(k) => writeln!(out, "{}  {file}  {} + {:#x}", hex(a), syms[k].1, u - syms[k].0)?,
            None => writeln!(out, "{}  {file}  ?", hex(a))?,
        }
    }
    Ok(())
}

fn build_command(o: &Options, loc: &Locate, out: &mut dyn Write) -> Result<i32> {
    let Some(dsc) = &loc.dsc else {
        bail!("index: set ONEIOS_DSC_DIR (or --dsc DIR) to the folder that holds {}", cache::MAIN_NAME);
    };
    let parent = loc.parent()?;
    let uuid = index::uuid_hex(&cache::read_uuid(dsc)?);
    let dir = parent.join(&uuid);
    if Index::is_current(&dir) && !o.force {
        let index = Index::open(&dir)?;
        writeln!(out, "index: {} is there (--force builds it again)", dir.display())?;
        describe(out, &index)?;
        return Ok(0);
    }
    let report = build_index(dsc, &parent)?;
    writeln!(out, "index: {} built in {:.1} s", report.dir.display(), report.seconds)?;
    let index = Index::open(&report.dir)?;
    describe(out, &index)?;
    writeln!(out, "  symbols: {} read from the cache, {} kept (one per address an image's __TEXT can get)", report.symbols_read, report.symbols_kept)?;
    let [ranges, scan, write] = report.stages;
    writeln!(out, "  time: load commands {ranges:.1} s; call bitmap and symbols {scan:.1} s; writing {write:.1} s")?;
    Ok(0)
}

fn describe(out: &mut dyn Write, index: &Index) -> Result<()> {
    writeln!(
        out,
        "  cache {}: base {:#x}, size {:#x}, max slide {:#x} ({} slide candidates); {} mappings, {} images, {} symbols",
        index::uuid_hex(&index.uuid),
        index.base,
        index.size,
        index.max_slide,
        index.candidates(),
        index.mapping_count(),
        index.image_count(),
        index.symbol_count()
    )?;
    let mut total = 0;
    for name in ["meta.bin", "syms.bin", "names.bin", "calls.bin"] {
        let len = std::fs::metadata(index.dir.join(name)).map(|m| m.len()).unwrap_or(0);
        total += len;
        writeln!(out, "  {name:<10} {len:>12} bytes  ({:.1} MiB)", len as f64 / 1048576.0)?;
    }
    writeln!(out, "  {:<10} {total:>12} bytes  ({:.1} MiB)", "total", total as f64 / 1048576.0)?;
    Ok(())
}
