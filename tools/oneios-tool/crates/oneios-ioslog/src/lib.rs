//! oneios-tool ioslog: iOS's own unified log, read from a copy of the phone's Data store (tools/oneios-tool/README.md
//! "ioslog"). The image is read with our APFS reader (no mount, Linux and macOS alike); the log files are decoded
//! by Mandiant's macos-unifiedlogs. CSV and JSONL are what its `unifiedlog_iterator` writes, so the two can be
//! compared byte for byte (tests/ioslog/compare.sh).
//!
//! Hand-written here: the choice of the files in the image (`source.rs`), the filters, the text output and the
//! checks. Everything that decodes a log file is the library's.

mod checks;
#[cfg(test)]
mod lib_tests;
mod source;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
use clap::{Args, Parser, Subcommand, ValueEnum};
use macos_unifiedlogs::log_entry::LogEntry;
use macos_unifiedlogs::logarchive::{load_timesync_data, visit_provider};
use regex::Regex;
use source::Source;
use std::collections::HashMap;
use std::io::Write;
use std::ops::ControlFlow;
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "oneios-tool ioslog",
    about = "iOS's unified log from a copy of the phone's Data store (data.img), a .logarchive, or a mounted Data volume",
    after_help = "SOURCE: a raw image of the Data store (read without mounting it), a .logarchive folder, or a folder that \
                  holds db/diagnostics and db/uuidtext.\nTimes are the guest's clock, in UTC. Docs: tools/oneios-tool/README.md."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write the log entries (all of them, or the ones the filters keep)
    Show(Box<Show>),
    /// Copy the log files out of the image as a .logarchive folder (what the manual steps made)
    Save {
        #[command(flatten)]
        from: From,
        /// The folder to write (NAME.logarchive)
        archive: PathBuf,
    },
    /// List the boots in the log: UUID, start, first and last entry, number of entries
    Boots {
        #[command(flatten)]
        from: From,
    },
    /// Count the lines of the known problems (touch paths, sensor modes, backlight, crashes, launch constraints, jetsam)
    Checks {
        #[command(flatten)]
        from: From,
        /// The boot to check: last (default), all, or the start of a boot UUID
        #[arg(long, default_value = "last")]
        boot: String,
        /// How many example lines to print per check
        #[arg(long, default_value_t = 3)]
        examples: usize,
    },
}

#[derive(Args)]
struct From {
    /// The Data store's image, a .logarchive folder, or a folder with db/diagnostics
    source: String,
    /// The image's volume: its number or its name (default: the first volume that holds a log)
    #[arg(long)]
    volume: Option<String>,
    /// Read the image as its block 0 describes it, not as its newest checkpoint does
    #[arg(long)]
    block0: bool,
    /// Print the decoder's warnings on stderr
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum Format {
    /// One line per entry: time, process[pid], type, [subsystem:category], message
    Text,
    /// unifiedlog_iterator's CSV, byte for byte
    Csv,
    /// unifiedlog_iterator's JSON lines
    Jsonl,
}

#[derive(Args)]
struct Show {
    #[command(flatten)]
    from: From,
    /// The file to write (default: standard output). An existing file is replaced.
    #[arg(short, long)]
    output: Option<PathBuf>,
    #[arg(short, long, value_enum, default_value = "text")]
    format: Format,
    /// Only this boot: last, or the start of a boot UUID (see `ioslog boots`)
    #[arg(long)]
    boot: Option<String>,
    /// Only entries at or after this time (2026-11-12T14:12:00Z, or "2026-11-12 14:12:00", UTC)
    #[arg(long)]
    since: Option<String>,
    /// Only entries before this time
    #[arg(long)]
    until: Option<String>,
    /// Only this process (the last part of its path; may be given several times)
    #[arg(long = "proc")]
    process: Vec<String>,
    /// Only this process id
    #[arg(long)]
    pid: Option<u64>,
    /// Only subsystems that start with this text
    #[arg(long)]
    subsystem: Option<String>,
    /// Only this category
    #[arg(long)]
    category: Option<String>,
    /// Only these log types, separated by commas (default, info, debug, error, fault, ...: the CSV's "Log Type")
    #[arg(long = "type")]
    kind: Option<String>,
    /// Only entries whose message matches this regular expression
    #[arg(long)]
    grep: Option<String>,
    /// Write the entries in time order (keeps the selected entries in memory)
    #[arg(long)]
    sort: bool,
}

/// Run `oneios-tool ioslog ARGS...`; `args` are the arguments after the tool's name. Returns the exit status.
pub fn run(args: &[String]) -> i32 {
    let cli = match Cli::try_parse_from(std::iter::once("oneios-tool ioslog".to_string()).chain(args.iter().cloned())) {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { 2 } else { 0 };
        }
    };
    let result = match cli.command {
        Command::Show(show) => cmd_show(*show),
        Command::Save { from, archive } => cmd_save(from, archive),
        Command::Boots { from } => cmd_boots(from),
        Command::Checks { from, boot, examples } => checks::run(from, &boot, examples),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            // a reader that closed the pipe (| head) is not a failure
            if e.chain().any(|c| c.downcast_ref::<std::io::Error>().is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)) {
                return 0;
            }
            eprintln!("ioslog: {e:#}");
            1
        }
    }
}

fn open(from: &From) -> Result<Source> {
    if from.verbose {
        let _ = tracing_subscriber::fmt().with_writer(std::io::stderr).with_max_level(tracing::Level::WARN).without_time().try_init();
    }
    let source = Source::open(&from.source, from.volume.as_deref(), from.block0)?;
    if source.tracev3.is_empty() {
        bail!("{}: the log folders hold no tracev3 file", source.what);
    }
    Ok(source)
}

fn cmd_save(from: From, archive: PathBuf) -> Result<()> {
    let source = open(&from)?;
    if archive.exists() && std::fs::read_dir(&archive).map(|mut d| d.next().is_some()).unwrap_or(true) {
        bail!("{}: is there already and is not an empty folder", archive.display());
    }
    let (files, bytes) = source.save(&archive)?;
    println!("{}: {files} files, {bytes} bytes from {}", archive.display(), source.what);
    Ok(())
}

/// A UUID as the library's output has it: upper-case hex, no hyphens.
pub(crate) fn uuid_text(uuid: Uuid) -> String {
    let mut buffer = Uuid::encode_buffer();
    uuid.simple().encode_upper(&mut buffer).to_string()
}

/// One boot of the log.
pub(crate) struct Boot {
    pub uuid: Uuid,
    /// The boot's start by logd's time records (nanoseconds since 1970), when the source has one for it.
    pub start: Option<i64>,
    pub first: f64,
    pub last: f64,
    pub entries: u64,
}

/// The boots that have entries, in the order of their start (boots without a time record first, by their
/// first entry).
pub(crate) fn boots(source: &Source) -> Result<Vec<Boot>> {
    let starts: HashMap<Uuid, i64> = load_timesync_data(source)?.into_iter().map(|(uuid, boot)| (uuid, boot.boot_time)).collect();
    let mut found: HashMap<Uuid, Boot> = HashMap::new();
    visit_provider(source, |entry| {
        let boot = found.entry(entry.boot_uuid).or_insert_with(|| Boot {
            uuid: entry.boot_uuid,
            start: starts.get(&entry.boot_uuid).copied(),
            first: f64::MAX,
            last: f64::MIN,
            entries: 0,
        });
        boot.first = boot.first.min(entry.time);
        boot.last = boot.last.max(entry.time);
        boot.entries += 1;
    })?;
    let mut out: Vec<Boot> = found.into_values().collect();
    out.sort_by(|a, b| {
        let key = |x: &Boot| (x.start.unwrap_or(i64::MIN), x.first);
        key(a).partial_cmp(&key(b)).unwrap_or(std::cmp::Ordering::Equal).then(a.uuid.cmp(&b.uuid))
    });
    Ok(out)
}

/// The boot `--boot WHAT` names: `last` (the newest start among logd's time records, else the last by entries),
/// or the start of a UUID. `None` for `all`.
pub(crate) fn pick_boot(source: &Source, what: &str) -> Result<Option<Uuid>> {
    if what == "all" {
        return Ok(None);
    }
    if what == "last" {
        let starts = load_timesync_data(source)?;
        if let Some((uuid, _)) = starts.iter().max_by_key(|(uuid, boot)| (boot.boot_time, **uuid)) {
            return Ok(Some(*uuid));
        }
        return boots(source)?.last().map(|b| Some(b.uuid)).ok_or_else(|| anyhow!("the log has no entries"));
    }
    let want: String = what.chars().filter(|c| *c != '-').collect::<String>().to_uppercase();
    let all = boots(source)?;
    let hits: Vec<&Boot> = all.iter().filter(|b| uuid_text(b.uuid).starts_with(&want)).collect();
    match hits.as_slice() {
        [one] => Ok(Some(one.uuid)),
        [] => bail!("no boot {what} in the log (see `ioslog boots`)"),
        _ => bail!("{what} is the start of {} boots (see `ioslog boots`)", hits.len()),
    }
}

/// Nanoseconds since 1970 as text, to the microsecond, UTC.
pub(crate) fn time_text(ns: f64) -> String {
    DateTime::from_timestamp_nanos(ns as i64).format("%Y-%m-%d %H:%M:%S%.6fZ").to_string()
}

fn cmd_boots(from: From) -> Result<()> {
    let source = open(&from)?;
    let (uuidtext, dsc) = source.string_files();
    println!(
        "{}: {} tracev3 files, {} timesync files, {uuidtext} uuidtext files, {dsc} shared-cache string files",
        source.what,
        source.tracev3.len(),
        source.timesync.len()
    );
    println!("{:<32}  {:<27}  {:<27}  {:<27}  {:>9}", "boot", "start (logd's record)", "first entry", "last entry", "entries");
    for boot in boots(&source)? {
        println!(
            "{:<32}  {:<27}  {:<27}  {:<27}  {:>9}",
            uuid_text(boot.uuid),
            boot.start.map(|t| time_text(t as f64)).unwrap_or_else(|| "-".into()),
            time_text(boot.first),
            time_text(boot.last),
            boot.entries
        );
    }
    Ok(())
}

/// `--since` / `--until`: RFC 3339, or date and time with a space, taken as UTC. Nanoseconds since 1970.
fn parse_time(text: &str) -> Result<f64> {
    if let Ok(t) = DateTime::parse_from_rfc3339(text) {
        return Ok(t.with_timezone(&Utc).timestamp_nanos_opt().unwrap_or(0) as f64);
    }
    for format in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M"] {
        if let Ok(t) = NaiveDateTime::parse_from_str(text, format) {
            return Ok(t.and_utc().timestamp_nanos_opt().unwrap_or(0) as f64);
        }
    }
    bail!("{text}: not a time (2026-11-12T14:12:00Z or \"2026-11-12 14:12:00\")")
}

/// What `show` keeps. The cheap fields are tested first; the message is formatted only for `--grep` and for
/// the entries that are written.
struct Filter {
    boot: Option<Uuid>,
    since: Option<f64>,
    until: Option<f64>,
    process: Vec<String>,
    pid: Option<u64>,
    subsystem: Option<String>,
    category: Option<String>,
    kinds: Option<Vec<String>>,
    grep: Option<Regex>,
}

/// The last part of a path.
pub(crate) fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

impl Filter {
    fn keeps(&self, entry: &LogEntry<'_, '_>) -> bool {
        if self.boot.is_some_and(|b| b != entry.boot_uuid)
            || self.since.is_some_and(|t| entry.time < t)
            || self.until.is_some_and(|t| entry.time >= t)
            || self.pid.is_some_and(|p| p != entry.pid)
        {
            return false;
        }
        if !self.process.is_empty() {
            let name = base_name(entry.process.unwrap_or_default());
            if !self.process.iter().any(|p| p == name) {
                return false;
            }
        }
        if let Some(want) = &self.subsystem {
            if !entry.subsystem.is_some_and(|s| s.starts_with(want.as_str())) {
                return false;
            }
        }
        if let Some(want) = &self.category {
            if entry.category != Some(want.as_str()) {
                return false;
            }
        }
        if let Some(kinds) = &self.kinds {
            let kind = format!("{:?}", entry.log_type).to_lowercase();
            if !kinds.contains(&kind) {
                return false;
            }
        }
        if let Some(re) = &self.grep {
            if !re.is_match(&entry.message()) {
                return false;
            }
        }
        true
    }
}

/// unifiedlog_iterator's CSV columns.
const CSV_HEADER: [&str; 19] = [
    "Timestamp",
    "Event Type",
    "Log Type",
    "Subsystem",
    "Thread ID",
    "PID",
    "EUID",
    "Persona ID",
    "Library",
    "Library UUID",
    "Activity ID",
    "Parent Activity ID",
    "Category",
    "Process",
    "Process UUID",
    "Message",
    "Raw Message",
    "Boot UUID",
    "System Timezone Name",
];

fn csv_record(fields: &[&str]) -> Result<Vec<u8>> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record(fields)?;
    writer.into_inner().map_err(|e| anyhow!("csv: {e}"))
}

/// One entry as bytes of the chosen format (with its line end).
fn render(format: Format, entry: &LogEntry<'_, '_>) -> Result<Vec<u8>> {
    Ok(match format {
        // the fields and their order: unifiedlog_iterator's OutputWriter::write_record
        Format::Csv => csv_record(&[
            entry.timestamp().to_rfc3339_opts(SecondsFormat::Millis, true).as_str(),
            &format!("{:?}", entry.event_type),
            &format!("{:?}", entry.log_type),
            entry.subsystem.unwrap_or_default(),
            &entry.thread_id.to_string(),
            &entry.pid.to_string(),
            &entry.euid.to_string(),
            &entry.persona_id.map(|id| id.to_string()).unwrap_or_default(),
            entry.library.unwrap_or_default(),
            &uuid_text(entry.library_uuid),
            &entry.activity_id.to_string(),
            &entry.parent_activity_id.unwrap_or(0).to_string(),
            entry.category.unwrap_or_default(),
            entry.process.unwrap_or_default(),
            &uuid_text(entry.process_uuid),
            &entry.message(),
            entry.raw_message(),
            &uuid_text(entry.boot_uuid),
            entry.timezone_name,
        ])?,
        Format::Jsonl => {
            let mut line = serde_json::to_vec(entry)?;
            line.push(b'\n');
            line
        }
        Format::Text => text_line(entry).into_bytes(),
    })
}

/// `TIME PROCESS[PID] TYPE [SUBSYSTEM:CATEGORY] MESSAGE`.
pub(crate) fn text_line(entry: &LogEntry<'_, '_>) -> String {
    let scope = match (entry.subsystem, entry.category) {
        (None, None) => String::new(),
        (s, c) => format!(" [{}:{}]", s.unwrap_or_default(), c.unwrap_or_default()),
    };
    format!(
        "{} {}[{}] {:?}{scope} {}\n",
        time_text(entry.time),
        base_name(entry.process.unwrap_or("?")),
        entry.pid,
        entry.log_type,
        entry.message()
    )
}

fn cmd_show(show: Show) -> Result<()> {
    let source = open(&show.from)?;
    let filter = Filter {
        boot: match &show.boot {
            Some(what) => pick_boot(&source, what)?,
            None => None,
        },
        since: show.since.as_deref().map(parse_time).transpose()?,
        until: show.until.as_deref().map(parse_time).transpose()?,
        process: show.process.clone(),
        pid: show.pid,
        subsystem: show.subsystem.clone(),
        category: show.category.clone(),
        kinds: show.kind.as_ref().map(|k| k.split(',').map(|s| s.trim().to_lowercase()).collect()),
        grep: show.grep.as_deref().map(Regex::new).transpose().context("--grep")?,
    };
    let mut out: Box<dyn Write> = match &show.output {
        Some(path) => Box::new(std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(path).with_context(|| path.display().to_string())?)),
        None => Box::new(std::io::BufWriter::with_capacity(1 << 16, std::io::stdout().lock())),
    };
    if show.format == Format::Csv {
        out.write_all(&csv_record(&CSV_HEADER)?)?;
    }
    let mut kept: Vec<(f64, Vec<u8>)> = Vec::new();
    let mut failure: Option<anyhow::Error> = None;
    let (mut seen, mut written) = (0u64, 0u64);
    visit_provider(&source, |entry| {
        seen += 1;
        if !filter.keeps(&entry) {
            return ControlFlow::Continue(());
        }
        written += 1;
        let done = render(show.format, &entry).and_then(|bytes| {
            if show.sort {
                kept.push((entry.time, bytes));
                Ok(())
            } else {
                out.write_all(&bytes).map_err(Into::into)
            }
        });
        match done {
            Ok(()) => ControlFlow::Continue(()),
            Err(e) => {
                failure = Some(e);
                ControlFlow::Break(())
            }
        }
    })?;
    if let Some(e) = failure {
        return Err(e);
    }
    if show.sort {
        // stable: entries of one time stay in the decoder's order
        kept.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        for (_, bytes) in &kept {
            out.write_all(bytes)?;
        }
    }
    out.flush()?;
    if show.output.is_some() {
        eprintln!("ioslog: {written} of {seen} entries from {}", source.what);
    }
    Ok(())
}
