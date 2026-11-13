//! `oneios-tool debug ksym-compare` (was tests/ksym/compare.py): compare `oneios-tool sym kernel` with
//! llvm-symbolizer, address by address. Run by tests/ksym/compare.sh.
//!
//!   debug ksym-compare --tool BIN --kc KC --kernel KERNEL --log LOG --ref FILE --index DIR
//!                      [--expect FILE] [--dsym PATH] [--sample N] [--symbolizer llvm-symbolizer]
//!
//!   --ref FILE   what llvm-symbolizer reads: the DWARF file of a dSYM of KERNEL (file:line and inlined frames are
//!                compared), or KERNEL itself (no DWARF at hand: function names only)
//!   --dsym PATH  give the tool this dSYM (else it finds the DWARF itself: in KERNEL, beside it, or through
//!                KERNEL's debug map)
//!   --expect     ksym-make-log's list: which kernel address and function each address of the log must get
//!   --sample N   how many more addresses of the kernel's code to compare (default 20000; 0: none)
//!
//! Checks, in order:
//!   1. every address of the log's panic blocks: the tool's kernel address is the reference's question, and the
//!      tool's function, file, line and inlined frames are the reference's answer;
//!   2. the running addresses (pc, lr): collection address + slide = the address in the log;
//!   3. --expect: the kernel address and the function ksym-make-log wrote each address for;
//!   4. the collection address of each function, three ways: the tool's (--symbol), this test's own reader with
//!      the segment's move (kcmap forward), and the collection's own symbol table (kcmap own);
//!   5. every instruction of every function the panic names, and N more addresses spread over the kernel's code
//!      (Python's random.Random(27), so the sample is the one compare.py drew).
//!
//! Names are compared as the files have them (mangled: `llvm-symbolizer --no-demangle`, the tool's `raw_function`
//! and `raw_symbol`), so two demanglers' spellings do not count. llvm-symbolizer names the outermost function from
//! the symbol table; where an address has two names (`_arp_ip_handle_input` and `_inet_arp_handle_input`) the two
//! tools may print different ones: that counts as the same when both are symbols of one address, and is counted.
//! Exit status 1 when anything differs.

use std::collections::{BTreeSet, HashMap};
use std::io::Write as _;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::kcmap::{self, Seg};
use crate::pyrandom::PyRandom;

#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    tool: String,
    #[arg(long)]
    kc: String,
    #[arg(long)]
    kernel: String,
    #[arg(long)]
    log: String,
    #[arg(long = "ref")]
    reference: String,
    #[arg(long)]
    index: String,
    #[arg(long)]
    expect: Option<String>,
    #[arg(long)]
    dsym: Option<String>,
    #[arg(long, default_value_t = 20000)]
    sample: u64,
    #[arg(long, default_value = "llvm-symbolizer")]
    symbolizer: String,
}

/// A frame: (function, file, line).
type Frame = (String, String, i64);

fn base(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// A field as text; "" when it is missing, null or empty (Python's falsy `get`).
fn text<'a>(entry: &'a Value, key: &str) -> &'a str {
    entry.get(key).and_then(Value::as_str).unwrap_or("")
}

/// A field as Python's `str()` prints it: `None` when it is missing.
fn shown(entry: &Value, key: &str) -> String {
    match entry.get(key) {
        None | Some(Value::Null) => "None".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

fn number(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

fn hex(text: &str) -> Result<u64> {
    u64::from_str_radix(text.trim_start_matches("0x"), 16).with_context(|| format!("not a hex number: {text}"))
}

fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value.get(key).and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// Per address: the frames, the outermost function first.
fn reference(symbolizer: &str, reference: &str, addrs: &[u64]) -> Result<Vec<Vec<Frame>>> {
    if addrs.is_empty() {
        return Ok(Vec::new());
    }
    let input: String = addrs.iter().map(|a| format!("{a:#x}\n")).collect();
    let out = piped(Command::new(symbolizer).arg(format!("--obj={reference}")).args(["--inlining", "--no-demangle", "--output-style=JSON"]), &input)?;
    if !out.status.success() {
        bail!("{symbolizer}: {}", out.status);
    }
    let mut rows = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.trim().is_empty()) {
        let entry: Value = serde_json::from_str(line).with_context(|| format!("{symbolizer}'s answer"))?;
        let mut frames = Vec::new();
        for symbol in list(&entry, "Symbol").iter().rev() {
            let name = text(symbol, "FunctionName");
            frames.push((if name == "??" { String::new() } else { name.to_string() }, text(symbol, "FileName").to_string(), number(symbol.get("Line"))));
        }
        rows.push(frames);
    }
    if rows.len() != addrs.len() {
        bail!("{symbolizer}: {} answers for {} addresses", rows.len(), addrs.len());
    }
    Ok(rows)
}

/// Run a command with `input` on its stdin; its stdout and stderr are kept.
fn piped(command: &mut Command, input: &str) -> Result<std::process::Output> {
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().with_context(|| format!("{command:?}"))?;
    let mut stdin = child.stdin.take().unwrap();
    let text = input.to_string();
    // Written from a thread: the command answers while it reads, and both pipes are small.
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(text.as_bytes());
    });
    let out = child.wait_with_output()?;
    let _ = writer.join();
    Ok(out)
}

/// The tool's frames with the names as the DWARF has them (mangled), as the reference is asked for them.
fn tool_frames(named: &Value) -> Vec<Frame> {
    list(named, "frames")
        .iter()
        .map(|f| {
            let name = f.get("raw_function").and_then(Value::as_str).unwrap_or_else(|| text(f, "function"));
            (name.to_string(), text(f, "file").to_string(), number(f.get("line")))
        })
        .collect()
}

/// The tool's symbol as the symbol table has it.
fn raw_symbol(named: &Value) -> String {
    match (text(named, "raw_symbol"), text(named, "symbol")) {
        ("", "") => String::new(),
        ("", symbol) => format!("_{symbol}"),
        (raw, _) => raw.to_string(),
    }
}

struct Compare<'a> {
    table: &'a HashMap<String, u64>,
    aliases: u64,
    alias_example: String,
}

impl Compare<'_> {
    /// Is the reference's outermost function the tool's? The reference takes that name from the symbol table.
    fn outer_same(&mut self, named: &Value, dwarf_name: Option<&str>, theirs: &str) -> bool {
        let mine = raw_symbol(named);
        if Some(theirs) == dwarf_name || theirs == mine || theirs == mine.get(1..).unwrap_or("") {
            return true;
        }
        // Another name of the same address.
        let Some(here) = self.table.get(&mine) else { return false };
        for candidate in [theirs.to_string(), format!("_{theirs}")] {
            if self.table.get(&candidate) == Some(here) {
                self.aliases += 1;
                if self.alias_example.is_empty() {
                    self.alias_example = format!("{mine} / {candidate}");
                }
                return true;
            }
        }
        false
    }

    /// Why the tool's answer is not the reference's; None when it is.
    fn differ(&mut self, named: &Value, reference: &[Frame]) -> Option<String> {
        let mine = tool_frames(named);
        if !reference.iter().any(|(_, file, _)| !file.is_empty()) {
            // The reference has the symbol table only: one frame, the function's name.
            let theirs = reference.last().map_or("", |f| f.0.as_str());
            if !mine.is_empty() {
                return Some(format!("the tool has lines, the reference none ({theirs})"));
            }
            if !theirs.is_empty() && !self.outer_same(named, None, theirs) {
                return Some(format!("function {} | reference {theirs}", raw_symbol(named)));
            }
            return None;
        }
        if mine.len() != reference.len() {
            let theirs: Vec<String> = reference.iter().map(|(n, f, l)| format!("{n} {}:{l}", base(f))).collect();
            return Some(format!("{} frame(s) | reference {}: {}", mine.len(), reference.len(), theirs.join(" > ")));
        }
        for (depth, ((name, file, line), (rname, rfile, rline))) in mine.iter().zip(reference).enumerate() {
            if name != rname && !(depth == 0 && self.outer_same(named, Some(name), rname)) {
                return Some(format!("frame {depth}: function {name} | reference {rname}"));
            }
            if base(file) != base(rfile) || line != rline {
                return Some(format!("frame {depth} ({name}): {}:{line} | reference {}:{rline}", base(file), base(rfile)));
            }
        }
        None
    }
}

fn show(named: &Value) -> String {
    let name = match text(named, "symbol") {
        "" => "?",
        symbol => symbol,
    };
    let mut out = match text(named, "offset") {
        "" => name.to_string(),
        offset => format!("{name}+{offset}"),
    };
    let frames: Vec<String> = list(named, "frames")
        .iter()
        .enumerate()
        .map(|(i, f)| format!("{}{}:{}", if i > 0 { format!("{} ", text(f, "function")) } else { String::new() }, base(text(f, "file")), number(f.get("line"))))
        .collect();
    if !frames.is_empty() {
        out += &format!("  ({})", frames.join(" > "));
    }
    out
}

/// The tool, the files it is given and the index it uses.
struct Tool<'a> {
    bin: &'a str,
    index: &'a str,
    files: Vec<String>,
}

impl Tool<'_> {
    /// `oneios-tool sym kernel -f json ARGS`, with the files in front unless `alone` names its own.
    fn run(&self, args: &[&str], stdin: Option<&str>) -> Result<Value> {
        let mut command = Command::new(self.bin);
        command.args(["sym", "kernel", "-f", "json"]).args(args).env("ONEIOS_KSYM_INDEX", self.index);
        let done = match stdin {
            Some(input) => piped(&mut command, input)?,
            None => command.stdin(Stdio::null()).output().with_context(|| self.bin.to_string())?,
        };
        if !done.status.success() {
            bail!("oneios-tool sym kernel {}: status {}\n{}", args.join(" "), done.status.code().unwrap_or(-1), String::from_utf8_lossy(&done.stderr));
        }
        serde_json::from_slice(&done.stdout).context("the tool's JSON")
    }

    fn with_files(&self, rest: &[&str], stdin: Option<&str>) -> Result<Value> {
        let mut args: Vec<&str> = self.files.iter().map(String::as_str).collect();
        args.extend_from_slice(rest);
        self.run(&args, stdin)
    }
}

/// The kernel and the collection as kcmap reads them.
struct Maps {
    segs: Vec<Seg>,
    in_kc: HashMap<String, Seg>,
    table: HashMap<String, u64>,
    own: HashMap<String, u64>,
}

impl Maps {
    /// A kernel address in the collection: the segment's own move added.
    fn forward(&self, addr: u64) -> Option<u64> {
        let seg = self.segs.iter().find(|s| s.holds(addr) && self.in_kc.contains_key(&s.name))?;
        Some(addr.wrapping_add(self.in_kc[&seg.name].vmaddr).wrapping_sub(seg.vmaddr))
    }
}

pub fn run(args: &Args) -> Result<i32> {
    let mut files = vec!["--kc".to_string(), args.kc.clone(), "--kernel".to_string(), args.kernel.clone()];
    if let Some(dsym) = &args.dsym {
        files.extend(["--dsym".to_string(), dsym.clone()]);
    }
    let tool = Tool { bin: &args.tool, index: &args.index, files };
    let (kc, kernel) = (kcmap::open_map(&args.kc)?, kcmap::open_map(&args.kernel)?);
    let member = kcmap::kernel_member(&kc)?;
    let (_, cmds) = kcmap::commands(&kernel, 0)?;
    let maps = Maps {
        segs: kcmap::segments(&kernel, &cmds),
        in_kc: kcmap::by_name(kcmap::segments(&kc, &member)),
        table: kcmap::symbols(&kernel, &cmds),
        own: kcmap::symbols(&kc, &member),
    };
    let mut compare = Compare { table: &maps.table, aliases: 0, alias_example: String::new() };
    let mut failures = 0u64;

    let report = tool.with_files(&[&args.log], None)?;
    for warning in list(&report, "warnings") {
        println!("  tool warning: {}", warning.as_str().unwrap_or(""));
    }
    let panics = list(&report, "panics");
    if panics.is_empty() {
        println!("  FAIL: the tool found no panic block in the log");
        return Ok(1);
    }
    let lines_compared = args.reference != args.kernel;
    println!(
        "1. the log's addresses against {} ({})",
        args.symbolizer,
        if lines_compared { "function, file:line and inlined frames" } else { "NO DWARF at hand: function names only" }
    );
    let named_functions = log_addresses(args, panics, &mut compare, &mut failures)?;
    running_addresses(panics, &mut failures)?;
    expected(args.expect.as_deref(), panics, &mut failures)?;
    collection_addresses(&tool, &maps, &named_functions, &mut failures)?;
    sweep(args, &tool, &maps, &named_functions, &mut compare, &mut failures)?;
    alone(args, &tool, &maps, panics, &mut failures)?;

    println!("{}: {failures} difference(s)", if failures > 0 { "FAIL" } else { "PASS" });
    Ok(if failures > 0 { 1 } else { 0 })
}

/// 1. every address of the log's panic blocks against the reference. Returns the functions they name.
fn log_addresses(args: &Args, panics: &[Value], compare: &mut Compare, failures: &mut u64) -> Result<Vec<String>> {
    let mut named_functions: Vec<String> = Vec::new();
    for block in panics {
        for warning in list(block, "warnings") {
            println!("  tool warning (panic at line {}): {}", shown(block, "line"), warning.as_str().unwrap_or(""));
        }
        let entries = list(block, "frames");
        let known: Vec<usize> = (0..entries.len()).filter(|&i| !text(&entries[i], "kernel_address").is_empty()).collect();
        let addrs = known.iter().map(|&i| hex(text(&entries[i], "kernel_address"))).collect::<Result<Vec<u64>>>()?;
        let answers = reference(&args.symbolizer, &args.reference, &addrs)?;
        let by_entry: HashMap<usize, &Vec<Frame>> = known.iter().copied().zip(&answers).collect();
        for (i, e) in entries.iter().enumerate() {
            let Some(answer) = by_entry.get(&i) else {
                let place = if text(e, "member").is_empty() {
                    e.get("note").map_or("not named".to_string(), |_| shown(e, "note"))
                } else {
                    format!("{} {}+{}", shown(e, "member"), shown(e, "segment"), shown(e, "segment_offset"))
                };
                println!("  --    {:<6} {}  not compared (not in the kernel): {place}", text(e, "kind"), text(e, "address"));
                continue;
            };
            let why = compare.differ(e, answer);
            *failures += why.is_some() as u64;
            let tail = why.as_ref().map_or(String::new(), |w| format!("   <- {w}"));
            println!("  {}  {:<6} {}  {}{tail}", if why.is_some() { "DIFF" } else { "same" }, text(e, "kind"), text(e, "address"), show(e));
            let raw = raw_symbol(e);
            if !raw.is_empty() && !named_functions.contains(&raw) {
                named_functions.push(raw);
            }
        }
    }
    Ok(named_functions)
}

/// 2. the running addresses: collection address + slide = the log's address.
fn running_addresses(panics: &[Value], failures: &mut u64) -> Result<()> {
    println!("2. running addresses: collection address + slide = the log's address");
    for block in panics {
        let frames = list(block, "frames");
        let twin_of = |kc_address: &str| frames.iter().rfind(|e| text(e, "kind") == "frame" && !text(e, "kc_address").is_empty() && text(e, "kc_address") == kc_address);
        for e in frames {
            let kind = text(e, "kind");
            if text(e, "space") != "running" {
                if (kind == "pc" || kind == "lr") && text(e, "space") == "outside" {
                    *failures += 1;
                    println!("  FAIL  {kind} {}: {}", text(e, "address"), shown(e, "note"));
                }
                continue;
            }
            let slide = hex(text(block, "slide")).context("a running address in a block with no slide")?;
            let back = hex(text(e, "kc_address"))?.wrapping_add(slide);
            let ok = back == hex(text(e, "address"))?;
            *failures += !ok as u64;
            let also = twin_of(text(e, "kc_address")).map_or(String::new(), |t| format!("; the same place as frame {} ({})", text(t, "address"), shown(t, "symbol")));
            println!(
                "  {}  {kind:<6} {} - {slide:#x} = {}  {}+{}{also}",
                if ok { "same" } else { "FAIL" },
                text(e, "address"),
                text(e, "kc_address"),
                shown(e, "symbol"),
                shown(e, "offset")
            );
        }
    }
    Ok(())
}

/// 3. ksym-make-log's list: the kernel address and the function of each address.
fn expected(expect_path: Option<&str>, panics: &[Value], failures: &mut u64) -> Result<()> {
    let Some(path) = expect_path else {
        println!("3. no --expect list (a real log): skipped");
        return Ok(());
    };
    println!("3. ksym-make-log's list: the kernel address and the function of each address");
    let mut by_addr: HashMap<(String, u64), &Value> = HashMap::new();
    for block in panics {
        for e in list(block, "frames") {
            by_addr.insert((text(e, "kind").to_string(), hex(text(e, "address"))?), e);
        }
    }
    for line in std::fs::read_to_string(path).with_context(|| path.to_string())?.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        let [kind, shown_addr, kernel_addr, name] = words[..] else { bail!("{path}: not four words: {line}") };
        let e = by_addr.get(&(kind.to_string(), hex(shown_addr)?));
        let ok = e.is_some_and(|e| text(e, "kernel_address") == kernel_addr && text(e, "symbol") == name);
        *failures += !ok as u64;
        let tail = if ok {
            String::new()
        } else {
            format!("   <- tool: {}", e.map_or("not in the tool's output".to_string(), |e| format!("{} {}", shown(e, "kernel_address"), shown(e, "symbol"))))
        };
        println!("  {}  {kind:<6} {shown_addr}  {kernel_addr} {name}{tail}", if ok { "same" } else { "FAIL" });
    }
    Ok(())
}

/// 4. each function's address in the collection, three ways.
fn collection_addresses(tool: &Tool, maps: &Maps, named_functions: &[String], failures: &mut u64) -> Result<()> {
    println!("4. each function's address in the collection: the tool | kcmap with the segment's move | the collection's own symbol table");
    for raw in named_functions {
        let Some(&addr) = maps.table.get(raw) else {
            println!("  --    {raw}: not a symbol of the kernel");
            continue;
        };
        let answer = tool.with_files(&["--symbol", &raw[1..]], None)?;
        let mine: BTreeSet<&str> = list(&answer, "addresses")
            .iter()
            .filter(|a| !text(a, "kc_address").is_empty() && raw_symbol(a) == *raw && text(a, "offset") == "0x0")
            .map(|a| text(a, "kc_address"))
            .collect();
        let mine: Vec<&str> = mine.into_iter().collect();
        let ours = maps.forward(addr).with_context(|| format!("{raw}: its segment is not in the collection"))?;
        let kept = maps.own.get(raw).copied();
        let ok = mine == [format!("{ours:#x}")] && kept.is_none_or(|k| k == ours);
        *failures += !ok as u64;
        let kept_text = kept.map_or("not kept".to_string(), |k| format!("{k:#x}"));
        let mine_text = if mine.is_empty() { "none".to_string() } else { mine.join(", ") };
        println!("  {}  {:<40} {mine_text} | {ours:#x} | {kept_text}", if ok { "same" } else { "FAIL" }, &raw[1..]);
    }
    Ok(())
}

/// 5. whole functions, and a sample of the kernel's code.
fn sweep(args: &Args, tool: &Tool, maps: &Maps, named_functions: &[String], compare: &mut Compare, failures: &mut u64) -> Result<()> {
    let mut starts: Vec<u64> = maps.table.values().copied().collect();
    starts.sort_unstable();
    starts.dedup();
    let mut sweep: Vec<u64> = Vec::new();
    for raw in named_functions {
        let Some(&start) = maps.table.get(raw) else { continue };
        let seg = maps.segs.iter().find(|s| s.holds(start)).with_context(|| format!("{raw} is outside every segment"))?;
        let top = seg.vmaddr + seg.vmsize;
        let end = starts.get(starts.partition_point(|&s| s <= start)).map_or(top, |&s| s.min(top));
        sweep.extend((start..end).step_by(4));
    }
    let in_functions = sweep.len();
    if let (true, Some(code)) = (args.sample > 0, maps.segs.iter().find(|s| s.name == "__TEXT_EXEC")) {
        let mut rng = PyRandom::new(27);
        sweep.extend((0..args.sample).map(|_| code.vmaddr + 4 * rng.below(code.vmsize / 4)));
    }
    sweep.retain(|&a| maps.forward(a).is_some());
    println!(
        "5. {in_functions} instructions of the {} functions the panic names, and {} more addresses of __TEXT_EXEC",
        named_functions.len(),
        sweep.len() as i64 - in_functions as i64
    );
    if sweep.is_empty() {
        return Ok(());
    }
    compare.aliases = 0;
    let input: String = sweep.iter().map(|&a| format!("{:#x}\n", maps.forward(a).unwrap())).collect();
    let answer = tool.with_files(&["--stdin"], Some(&input))?;
    let mine = list(&answer, "addresses");
    let theirs = reference(&args.symbolizer, &args.reference, &sweep)?;
    let wrong_place = sweep.iter().zip(mine).filter(|(a, m)| text(m, "kernel_address") != format!("{a:#x}")).count();
    let mut diffs = Vec::new();
    for ((a, m), t) in sweep.iter().zip(mine).zip(&theirs) {
        if let Some(why) = compare.differ(m, t) {
            diffs.push((*a, m, why));
        }
    }
    let inlined = mine.iter().filter(|m| list(m, "frames").len() > 1).count();
    let with_lines = mine.iter().filter(|m| !list(m, "frames").is_empty()).count();
    println!(
        "  {} addresses: {} the same, {} differ; {with_lines} with file:line, {inlined} of them in inlined code; {wrong_place} mapped to another kernel address than asked",
        sweep.len(),
        sweep.len() - diffs.len(),
        diffs.len()
    );
    if compare.aliases > 0 {
        println!(
            "  note: at {} of the same ones the two tools print different names of one address (both are symbols there), e.g. {}",
            compare.aliases, compare.alias_example
        );
    }
    *failures += (diffs.len() + wrong_place) as u64;
    for (a, m, why) in diffs.iter().take(12) {
        println!("  DIFF  {a:#x} (collection {})  {}   <- {why}", shown(m, "kc_address"), show(m));
    }
    if diffs.len() > 12 {
        println!("  ... and {} more", diffs.len() - 12);
    }
    Ok(())
}

/// 6. the collection alone (--no-kernel): function + offset from the symbols it keeps for the kernel.
fn alone(args: &Args, tool: &Tool, maps: &Maps, panics: &[Value], failures: &mut u64) -> Result<()> {
    println!("6. the collection alone (--no-kernel): function + offset from the symbols it keeps for the kernel");
    let answer = tool.run(&["--kc", &args.kc, "--no-kernel", &args.log], None)?;
    if maps.own.is_empty() {
        println!("  --    the collection keeps no symbols of the kernel: not compared");
        return Ok(());
    }
    for (full_block, alone_block) in panics.iter().zip(list(&answer, "panics")) {
        let mut same = 0;
        for (full, lone) in list(full_block, "frames").iter().zip(list(alone_block, "frames")) {
            if text(full, "kernel_address").is_empty() {
                continue;
            }
            let (a, b) = (raw_symbol(full), raw_symbol(lone));
            let one_address = maps.own.get(&a).is_some_and(|at| maps.own.get(&b) == Some(at));
            let ok = full.get("offset") == lone.get("offset") && (a == b || one_address);
            same += ok as u64;
            *failures += !ok as u64;
            if !ok {
                println!("  FAIL  {:<6} {}  {a}+{} | alone: {b}+{}", text(full, "kind"), text(full, "address"), shown(full, "offset"), shown(lone, "offset"));
            }
        }
        println!("  {same} address(es) of the panic at line {}: the same function + offset", shown(full_block, "line"));
    }
    Ok(())
}
