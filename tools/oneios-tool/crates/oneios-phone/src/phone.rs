//! `oneios-tool phone`: the adb workflow of docs/building.md section 6 ("Test cycle on the phone").
//! `stage` and `sums` are scripts/phone/stage.sh, command for command.

use crate::adb::{last_lines, plain, Adb, BUSYBOX, STAGE};
use crate::sparse;
use anyhow::{anyhow, bail, Context, Result};
use oneios_common::pyjson;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::IsTerminal;
use std::time::{Duration, Instant};

/// Where `adb push` puts the files before root's script takes them (stage.sh's TMP).
const TMP: &str = "/data/local/tmp/oneios-stage-tmp";
pub const APP_ID: &str = "com.fahadali.oneos";
const ROOT_SH: &str = "/data/local/tmp/oneios/bin/oneios_root.sh";

/// The phone's clock, in seconds.
pub fn phone_epoch(adb: &Adb) -> Result<u64> {
    let r = adb.shell("date +%s")?;
    r.out.trim().parse().map_err(|_| anyhow!("the phone's `date +%s` gave \"{}\"", r.out.trim()))
}

/// `versionCode=N` and `versionName=X` of the installed app (`dumpsys package`), if it is installed.
pub fn installed_app(adb: &Adb, id: &str) -> Result<Option<(u64, String)>> {
    plain(id, "the application id")?;
    let r = adb.shell(&format!("dumpsys package {id}"))?;
    let field = |key: &str| -> Option<String> {
        r.out.lines().find_map(|line| line.split_whitespace().find_map(|word| word.strip_prefix(key).map(str::to_string)))
    };
    Ok(match (field("versionCode=").and_then(|v| v.parse().ok()), field("versionName=")) {
        (Some(code), name) => Some((code, name.unwrap_or_default())),
        _ => None,
    })
}

// ---------------------------------------------------------------- status

/// The lines of `oneios_root.sh status` (`@@RESULT ...`), read only.
pub fn root_status(adb: &Adb) -> Result<Vec<String>> {
    let out = adb.root_ok(&format!("sh {ROOT_SH} status\n"), "oneios_root.sh status")?;
    Ok(out.lines().map(str::to_string).collect())
}

pub fn status(adb: &Adb) -> Result<()> {
    println!("phone: {}", adb.serial);
    adb.need_root()?;
    println!("root: yes");
    match installed_app(adb, APP_ID)? {
        Some((code, name)) => println!("app: {APP_ID} {name} (versionCode {code})"),
        None => println!("app: {APP_ID} is not installed"),
    }
    println!("session: {}", if adb.session_open()? { "open" } else { "none" });
    for line in root_status(adb)? {
        println!("{line}");
    }
    Ok(())
}

// ---------------------------------------------------------------- switch and return

/// Start a session as the app does: the root script's preflight, then the switch, detached.
pub fn switch(adb: &Adb, version_code: Option<u64>) -> Result<u64> {
    adb.need_root()?;
    adb.refuse_in_session()?;
    // before One UI is stopped: a set xnuhost would refuse ends the session seconds after the switch
    let n = image_check(adb)?;
    println!("image: manifest.json and the {n} files it lists agree");
    let code = match version_code {
        Some(code) => code,
        None => installed_app(adb, APP_ID)?
            .ok_or_else(|| anyhow!("{APP_ID} is not installed, so there is no versionCode to give the switch (install it: oneios-tool app install; or --version-code N)"))?
            .0,
    };
    let before = phone_epoch(adb)?;
    let r = adb.shell(&format!("su -c '{BUSYBOX} setsid sh {ROOT_SH} switch {code} 1'"))?;
    let said = format!("{}{}", r.out, r.err);
    print!("{}", r.out);
    if let Some(why) = said.lines().find(|l| l.starts_with("@@REFUSED") || l.starts_with("@@ERROR") || l.starts_with("@@USAGE")) {
        bail!("the switch did not start: {why}");
    }
    if !said.lines().any(|l| l.trim() == "@@OK switch") {
        // setsid may have let adb go before the script answered: the switch's own log says whether it started
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let log = adb.root(&format!("grep epoch {STAGE}/switch.log | tail -n 1\n"))?;
            let epoch = log.out.split_whitespace().skip_while(|w| *w != "epoch").nth(1).and_then(|e| e.parse::<u64>().ok());
            if epoch.is_some_and(|e| e >= before) {
                break;
            }
            if Instant::now() > deadline {
                bail!("the switch gave no answer and switch.log has no new start line: {}", last_lines(&said, 3));
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    println!("switch started (versionCode {code}); One UI stops in about 40 s. Watch: oneios-tool phone log --follow --markers");
    Ok(code)
}

/// How a return ended.
pub enum Returned {
    /// No session was open: nothing was asked.
    Nothing,
    /// One UI's restore line came, after this many seconds.
    Restored(u64, String),
}

/// Ask the session to end (session/return.request, which the watcher sees within a second) and wait for One UI's
/// restore line in return.log.
pub fn return_to_one_ui(adb: &Adb, note: &str, timeout: u64) -> Result<Returned> {
    adb.need_root()?;
    if !adb.session_open()? {
        return Ok(Returned::Nothing);
    }
    let note: String = note.chars().filter(|c| c.is_ascii_alphanumeric() || " .:_-".contains(*c)).collect();
    // the log's length first: only what the return writes after it counts
    let size = adb.root_ok(&format!("stat -c %s {STAGE}/return.log 2>/dev/null || echo 0\n"), "reading return.log's size")?;
    let size: u64 = size.trim().parse().map_err(|_| anyhow!("return.log's size is \"{}\"", size.trim()))?;
    adb.root_ok(&format!("echo '{note}' > {STAGE}/session/return.request\n"), "writing session/return.request")?;
    eprintln!("return asked; waiting for One UI (about 45 s, at most {timeout} s)");
    let start = Instant::now();
    let mut dropped = 0;
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let waited = start.elapsed().as_secs();
        match adb.root(&format!("tail -c +{} {STAGE}/return.log | grep 'One UI restored' | tail -n 1\n", size + 1)) {
            Ok(r) if r.ok() && !r.out.trim().is_empty() => return Ok(Returned::Restored(waited, r.out.trim().to_string())),
            Ok(r) if r.ok() || r.status == 1 => dropped = 0,
            // adb drops for a moment when the framework starts; a phone that restarted (the failsafe) stays away longer
            _ => dropped += 1,
        }
        if waited >= timeout {
            let tail = adb.root(&format!("tail -n 5 {STAGE}/return.log; cat {STAGE}/return.outcome\n")).map(|r| r.out).unwrap_or_default();
            bail!(
                "no restore line after {waited} s{}. return.log and return.outcome: {}",
                if dropped > 3 { " and the phone does not answer on adb (a restart is the return's failsafe: wait for it, root it again)" } else { "" },
                last_lines(&tail, 6)
            );
        }
    }
}

pub fn do_return(adb: &Adb, note: &str, timeout: u64) -> Result<()> {
    match return_to_one_ui(adb, note, timeout)? {
        Returned::Nothing => println!("no session is open: nothing to return from"),
        Returned::Restored(seconds, line) => println!("{line} ({seconds} s)"),
    }
    let outcome = adb.root(&format!("cat {STAGE}/return.outcome\n"))?;
    println!("return.outcome: {}", if outcome.out.trim().is_empty() { "none" } else { outcome.out.trim() });
    Ok(())
}

// ---------------------------------------------------------------- stage and sums (scripts/phone/stage.sh)

fn sha256_of(bytes: &[u8]) -> String {
    oneios_common::hex(&Sha256::digest(bytes))
}

fn json_word(v: &serde_json::Value, key: &str) -> Result<String> {
    let word = match v.get(key) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => bail!("runtime.json has no \"{key}\""),
    };
    plain(&word, &format!("runtime.json's {key}"))?;
    Ok(word)
}

/// The script root runs for `stage runtime` (`asis`: each file's own size and SHA-256 instead of runtime.json's).
pub fn runtime_script(dir: &str, asis: bool) -> Result<String> {
    let raw = fs::read(format!("{dir}/runtime.json")).with_context(|| format!("{dir} has no runtime.json"))?;
    let j: serde_json::Value = serde_json::from_slice(&raw).with_context(|| format!("{dir}/runtime.json"))?;
    let mut files = vec![("runtime.json".to_string(), raw.len().to_string(), sha256_of(&raw), "0644".to_string())];
    for f in j.get("files").and_then(|f| f.as_array()).ok_or_else(|| anyhow!("runtime.json has no \"files\" list"))? {
        let path = json_word(f, "path")?;
        let (size, sha) = if asis {
            let bytes = fs::read(format!("{dir}/{path}")).with_context(|| format!("{dir}/{path}"))?;
            (bytes.len().to_string(), sha256_of(&bytes))
        } else {
            (json_word(f, "size")?, json_word(f, "sha256")?)
        };
        let mode = if f.get("mode").is_some() { json_word(f, "mode")? } else { "0644".to_string() };
        files.push((path, size, sha, mode));
    }
    let formats: Vec<String> = j
        .get("image_formats")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow!("runtime.json has no \"image_formats\" list"))?
        .iter()
        .map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string()))
        .collect();
    let formats = formats.join(",");
    plain(&formats, "runtime.json's image_formats")?;
    let placeholder = match j.get("placeholder") {
        None | Some(serde_json::Value::Null) | Some(serde_json::Value::Bool(false)) => 0,
        Some(serde_json::Value::Number(n)) if n.as_f64() == Some(0.0) => 0,
        Some(serde_json::Value::String(s)) if s.is_empty() => 0,
        _ => 1,
    };
    let mut script = format!("set -e; R=\"sh {STAGE}/bin/oneios_root.sh\"\n$R begin runtime\n");
    for (path, size, sha, mode) in &files {
        script += &format!("$R receive runtime {path} {size} {sha} {mode} < {TMP}/{path}\n");
    }
    script += &format!(
        "$R finish runtime {} {} {} {} {formats} {placeholder}\n",
        files.len(),
        json_word(&j, "version")?,
        json_word(&j, "session_abi")?,
        json_word(&j, "xnuhost")?
    );
    script += &format!("cat {STAGE}/runtime.conf; rm -rf {TMP}\n");
    Ok(script)
}

/// Every regular file below `dir`, relative, sorted (stage.sh's `find . -type f | sort`). A walk of a local
/// folder, never of the phone.
fn local_files(dir: &str) -> Result<Vec<String>> {
    fn walk(top: &str, rel: &str, out: &mut Vec<String>) -> Result<()> {
        let here = if rel.is_empty() { top.to_string() } else { format!("{top}/{rel}") };
        for entry in fs::read_dir(&here).with_context(|| here.clone())? {
            let entry = entry?;
            let name = entry.file_name().into_string().map_err(|n| anyhow!("{here}: the name {n:?} is not UTF-8"))?;
            let path = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            let kind = entry.file_type()?;
            if kind.is_dir() {
                walk(top, &path, out)?;
            } else if kind.is_file() {
                out.push(path);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out)?;
    out.sort();
    Ok(out)
}

/// The script root runs for `stage image`: each file replaces the staged one, and its lines in image.sha256 and
/// image.ids are written again.
pub fn image_script(dir: &str) -> Result<String> {
    if !fs::metadata(dir).map(|m| m.is_dir()).unwrap_or(false) {
        bail!("no folder {dir}");
    }
    let mut script = format!("set -e; cd {STAGE}\n");
    for p in local_files(dir)? {
        plain(&p, "the file name")?;
        script += &format!("f=image/{p}; [ -f \"$f\" ] || {{ echo \"not staged: $f\"; exit 1; }}\n");
        script += &format!("cp {TMP}/{p} \"$f.part\" && chown root:root \"$f.part\" && chmod 0644 \"$f.part\" && mv -f \"$f.part\" \"$f\"\n");
        script += "s=$(sha256sum \"$f\" | cut -d' ' -f1); i=$(stat -c '%d:%i:%s:%Y' \"$f\")\n";
        script += &format!("awk -v p='{p}' -v s=\"$s\" '$2 == p {{ print s \"  \" p; next }} {{ print }}' image.sha256 > image.sha256.new && mv -f image.sha256.new image.sha256\n");
        script += &format!("awk -v p='{p}' -v i=\"$i\" '$2 == p {{ print i \" \" p; next }} {{ print }}' image.ids > image.ids.new && mv -f image.ids.new image.ids\n");
        script += "echo \"staged $s  $f\"\n";
    }
    script += &format!("chmod 0600 image.sha256 image.ids; sync; rm -rf {TMP}\n");
    Ok(script)
}

pub const SUMS_SCRIPT: &str = "cd /data/local/tmp/oneios && sha256sum image/kernel.kc image/kernel.kc.hashes image/manifest.json image/overlay/userland/xnuhost.conf && grep ' data.seed$' image.sha256 && cat runtime.conf && cat runtime.sha256\n";

pub fn stage(adb: &Adb, kind: &str, dir: &str) -> Result<()> {
    let dir = dir.trim_end_matches('/');
    let script = match kind {
        "runtime" => runtime_script(dir, false)?,
        "runtime-asis" => runtime_script(dir, true)?,
        "image" => image_script(dir)?,
        _ => bail!("stage runtime|runtime-asis|image DIR"),
    };
    adb.need_root()?;
    adb.refuse_in_session()?;
    let r = adb.shell(&format!("rm -rf {TMP}"))?;
    if !r.ok() {
        bail!("cannot clear {TMP} on the phone: {}", last_lines(&r.err, 2));
    }
    adb.push(dir, TMP)?;
    let r = adb.root(&script)?;
    print!("{}", r.out);
    eprint!("{}", r.err);
    if !r.ok() {
        bail!("staging stopped on the phone with status {} (the files pushed are still in {TMP})", r.status);
    }
    if kind == "image" {
        // said now, not at the switch: the files staged may need a manifest of their own
        match image_check(adb) {
            Ok(n) => println!("image: manifest.json and the {n} files it lists agree"),
            Err(e) => eprintln!("warning: {e:#}"),
        }
    }
    Ok(())
}

/// The staged files' sha256 (stage.sh sums), as text. Unlike stage.sh's, it refuses while a session is open:
/// it reads the whole kernel collection, and only status, return and log run beside a session.
pub fn sums_text(adb: &Adb) -> Result<String> {
    adb.need_root()?;
    adb.refuse_in_session()?;
    adb.root_ok(SUMS_SCRIPT, "reading the staged sums")
}

// ---------------------------------------------------------------- the image set's manifest

/// `image.sha256`'s lines (`sha256  path`, paths under image/) as path -> sha256.
pub fn parse_image_sums(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once("  "))
        .map(|(sha, path)| (path.trim().trim_start_matches("./").to_string(), sha.trim().to_string()))
        .collect()
}

/// The staged set's own record: its manifest.json and image.sha256 (the sums `stage image` and the app write when
/// they stage a file). Read only, and cheap: nothing is hashed on the phone.
pub fn phone_image_record(adb: &Adb) -> Result<(pyjson::Value, BTreeMap<String, String>)> {
    let text = adb.root_ok(&format!("cat {STAGE}/image/manifest.json\n"), "reading image/manifest.json")?;
    let manifest = pyjson::parse(&text).map_err(|e| anyhow!("the phone's image/manifest.json: {e}"))?;
    let sums = adb.root_ok(&format!("cat {STAGE}/image.sha256\n"), "reading image.sha256")?;
    Ok((manifest, parse_image_sums(&sums)))
}

/// manifest.json's `files`: (path, sha256, size) of each entry.
pub fn manifest_entries(manifest: &pyjson::Value) -> Result<Vec<(String, String, u64)>> {
    let files = manifest.get("files").and_then(|f| f.as_list()).ok_or_else(|| anyhow!("manifest.json has no \"files\" list"))?;
    files
        .iter()
        .map(|f| {
            let path = f.get("path").and_then(|p| p.as_str()).ok_or_else(|| anyhow!("a manifest.json entry has no path"))?;
            let sha = f.get("sha256").and_then(|s| s.as_str()).ok_or_else(|| anyhow!("manifest.json's {path} has no sha256"))?;
            Ok((path.to_string(), sha.to_string(), f.get("size").and_then(|s| s.as_u64()).unwrap_or(0)))
        })
        .collect()
}

/// Where manifest.json and the staged files (image.sha256) disagree, one line each; `skip`: paths not to look at.
/// xnuhost refuses a set whose files are not the ones its manifest lists ("trust caches of /data/image: a file of
/// the image set is not the one manifest.json lists", phone 2026-11-12 23:04: a manifest of another set was staged,
/// and the session ended 2 s after One UI was stopped).
pub fn manifest_problems(manifest: &pyjson::Value, sums: &BTreeMap<String, String>, skip: &[String]) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    for (path, sha, _) in manifest_entries(manifest)? {
        if skip.contains(&path) {
            continue;
        }
        match sums.get(&path) {
            None => problems.push(format!("{path}: manifest.json lists it, image.sha256 has no staged file of that name")),
            Some(staged) if *staged != sha => {
                problems.push(format!("{path}: manifest.json lists {}, the staged file is {}", &sha[..16.min(sha.len())], &staged[..16.min(staged.len())]))
            }
            Some(_) => {}
        }
    }
    Ok(problems)
}

/// The check before a switch: the staged manifest.json lists the staged files. Err with every difference.
pub fn image_check(adb: &Adb) -> Result<usize> {
    let (manifest, sums) = phone_image_record(adb)?;
    let problems = manifest_problems(&manifest, &sums, &[])?;
    if !problems.is_empty() {
        bail!(
            "the staged image set's manifest.json does not list the staged files (xnuhost would refuse the set):\n  {}\nmake a manifest for the files you staged: oneios-tool phone manifest DIR --stage",
            problems.join("\n  ")
        );
    }
    Ok(manifest_entries(&manifest)?.len())
}

/// The phone's manifest.json with the entries of DIR's files (at their paths in the set) set to those files'
/// SHA-256 and size. Every other entry must already list the staged file. DIR's own manifest.json is ignored.
pub fn manifest_for(adb: &Adb, dir: &str) -> Result<(String, Vec<String>)> {
    let (mut manifest, sums) = phone_image_record(adb)?;
    let local: Vec<String> = local_files(dir)?.into_iter().filter(|p| p != "manifest.json").collect();
    if local.is_empty() {
        bail!("{dir} has no file of the image set (kernel.kc, kernel.kc.hashes, ... at their paths)");
    }
    let listed: Vec<String> = manifest_entries(&manifest)?.into_iter().map(|e| e.0).collect();
    if let Some(p) = local.iter().find(|p| !listed.contains(p)) {
        bail!("{p} is not a file of the phone's image set (its manifest.json does not list it)");
    }
    let problems = manifest_problems(&manifest, &sums, &local)?;
    if !problems.is_empty() {
        bail!("the phone's image set is not consistent apart from DIR's files; a new manifest would not fix it:\n  {}", problems.join("\n  "));
    }
    let mut changed = Vec::new();
    if let Some(pyjson::Value::List(files)) = manifest.get_mut("files") {
        for f in files.iter_mut() {
            let Some(path) = f.get("path").and_then(|p| p.as_str()).map(str::to_string) else { continue };
            if !local.contains(&path) {
                continue;
            }
            let bytes = fs::read(format!("{dir}/{path}")).with_context(|| format!("{dir}/{path}"))?;
            let sha = sha256_of(&bytes);
            if f.get("sha256").and_then(|s| s.as_str()) != Some(sha.as_str()) {
                changed.push(format!("{path} {} ({} bytes)", &sha[..16], bytes.len()));
            }
            f.set("sha256", pyjson::Value::str(sha));
            f.set("size", pyjson::Value::Int(bytes.len() as i128));
        }
    }
    Ok((pyjson::dumps(&manifest, Some(2), false) + "\n", changed))
}

/// `phone manifest DIR`: write the manifest for DIR's files (to --out, or print it); with `stage`, stage DIR's
/// files and that manifest together.
pub fn manifest_cmd(adb: &Adb, dir: &str, out: Option<&str>, stage_too: bool) -> Result<()> {
    adb.need_root()?;
    adb.refuse_in_session()?;
    let dir = dir.trim_end_matches('/');
    let (text, changed) = manifest_for(adb, dir)?;
    eprintln!("manifest.json: the phone's, {}", if changed.is_empty() { "no entry changed".to_string() } else { format!("new entries: {}", changed.join(", ")) });
    if let Some(path) = out {
        fs::write(path, &text).with_context(|| path.to_string())?;
        eprintln!("written: {path}");
    } else if !stage_too {
        print!("{text}");
    }
    if stage_too {
        let part = std::env::temp_dir().join(format!("oneios-manifest-stage-{}", std::process::id()));
        let _ = fs::remove_dir_all(&part);
        for p in local_files(dir)?.into_iter().filter(|p| p != "manifest.json") {
            let dst = part.join(&p);
            fs::create_dir_all(dst.parent().expect("a file's folder"))?;
            if fs::hard_link(format!("{dir}/{p}"), &dst).is_err() {
                fs::copy(format!("{dir}/{p}"), &dst).with_context(|| format!("{dir}/{p}"))?;
            }
        }
        fs::write(part.join("manifest.json"), &text)?;
        let r = stage(adb, "image", &part.display().to_string());
        let _ = fs::remove_dir_all(&part);
        r?;
        let n = image_check(adb)?;
        println!("image: manifest.json and the {n} files it lists agree");
    }
    Ok(())
}

// ---------------------------------------------------------------- sparse-write (pull-data: pull.rs)

fn progress_line(start: Instant, last: &mut Instant, size: u64, data: u64, total: Option<u64>) {
    if last.elapsed() < Duration::from_secs(2) {
        return;
    }
    *last = Instant::now();
    let gib = |n: u64| n as f64 / (1u64 << 30) as f64;
    let of = total.map(|t| format!(" of {:.1}", gib(t))).unwrap_or_default();
    let end = if std::io::stderr().is_terminal() { "\r" } else { "\n" };
    eprint!("{:.1}{of} GiB read, {:.2} GiB of data, {} s{end}", gib(size), gib(data), start.elapsed().as_secs());
}

/// `phone sparse-write OUT`: stdin to OUT, zero runs as holes.
pub fn sparse_write(out: &str, gunzip: bool, expect: Option<u64>) -> Result<()> {
    let file = fs::File::create(out).with_context(|| out.to_string())?;
    let stdin = std::io::stdin().lock();
    let (start, mut last) = (Instant::now(), Instant::now());
    let show = |size, data| progress_line(start, &mut last, size, data, expect);
    let w = if gunzip { sparse::copy(flate2::read::MultiGzDecoder::new(stdin), &file, show)? } else { sparse::copy(stdin, &file, show)? };
    println!("{out}: {} bytes, {} of them written ({} s)", w.size, w.data, start.elapsed().as_secs());
    if let Some(expect) = expect.filter(|e| *e != w.size) {
        bail!("{out}: {} bytes came, {expect} were expected", w.size);
    }
    Ok(())
}
