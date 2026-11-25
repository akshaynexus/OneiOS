//! The host phone in `oneios-tool phone status`: the same properties and files xnuhost reads at its start
//! (runtime/bridges/hw/host_device.rs), read through `adb shell`, matched against the same table, in the
//! same order (a model prefix, a codename, the SoC, soc0's machine, the platform, the device tree's compatible list).
//!
//! The table is the bridges' (runtime/bridges/hw/host_device.rs, the source of truth): [`TABLE`] mirrors it, and the
//! test `table_matches_the_bridges` reads that file and fails when the two differ, so a phone added there must be
//! added here in the same commit.

use crate::adb::Adb;
use anyhow::Result;

pub struct HostDevice {
    pub name: &'static str,
    pub label: &'static str,
    pub models: &'static [&'static str],
    pub devices: &'static [&'static str],
    pub socs: &'static [&'static str],
    pub platforms: &'static [&'static str],
    /// xnuhost settings KEY=VALUE, under the command line and the image set
    pub config: &'static [&'static str],
    /// environment defaults NAME=VALUE for the bridges (a variable already set wins)
    pub env: &'static [&'static str],
    /// session-mode=takeover (the opt-in Android takeover) is allowed on this phone (host_device.rs's `takeover: true`)
    pub takeover: bool,
}

pub const TABLE: &[HostDevice] = &[
    HostDevice {
        name: "galaxy-s24-ultra",
        label: "Galaxy S24 Ultra (SM-S928, SM8650)",
        models: &["SM-S928"],
        devices: &["e3q"],
        socs: &["SM8650"],
        platforms: &["pineapple"],
        config: &[],
        env: &[],
        takeover: true,
    },
    HostDevice {
        name: "galaxy-s25-ultra",
        label: "Galaxy S25 Ultra (SM-S938, SM8750)",
        models: &["SM-S938"],
        devices: &["pa3q"],
        socs: &["SM8750"],
        platforms: &["sun"],
        config: &[],
        env: &["ONEIOS_THERMAL_SOC_TRIPS=1", "ONEIOS_THERMAL_SOC_SAMPLES=3"],
        takeover: false,
    },
    HostDevice {
        name: "galaxy-s25",
        label: "Galaxy S25 (SM-S931, SM8750)",
        models: &["SM-S931"],
        devices: &["pa1q"],
        socs: &["SM8750"],
        platforms: &["sun"],
        config: &["panel-fallback=1080x2340@120"],
        env: &["ONEIOS_THERMAL_SOC_TRIPS=1", "ONEIOS_THERMAL_SOC_SAMPLES=3"],
        takeover: false,
    },
    HostDevice {
        name: "galaxy-s26-ultra",
        label: "Galaxy S26 Ultra (SM-S948, SM8850)",
        models: &["SM-S948"],
        devices: &["m3q"],
        socs: &["SM8850"],
        platforms: &["canoe"],
        config: &[],
        env: &["ONEIOS_BRIGHT_MAX=2300"],
        takeover: true,
    },
    HostDevice {
        name: "galaxy-a56",
        label: "Galaxy A56 (SM-A566, Exynos 1580)",
        models: &["SM-A566"],
        devices: &["a56x"],
        socs: &["s5e8855"],
        platforms: &["s5e8855"],
        config: &[
            "gpu=auto",
            "panel-fallback=1080x2340@120",
            "width=720",
            "height=1560",
            "scale=2",
            "hz=60",
            "return-graceful-ms=4000",
            "astc=0",
            "hw-backend-haptics=treble",
            "hw-backend-fallback=1",
        ],
        env: &[],
        takeover: false,
    },
];

/// What was read (an empty string: not there).
#[derive(Default, Debug, Clone)]
pub struct Facts {
    pub model: String,
    pub device: String,
    pub platform: String,
    pub soc_model: String,
    pub soc_manufacturer: String,
    pub machine: String,
    pub soc_id: String,
    pub compatible: String,
}

const PROPS: [&str; 5] = ["ro.product.model", "ro.product.device", "ro.board.platform", "ro.soc.model", "ro.soc.manufacturer"];

/// One `adb shell` call, as the shell user (getprop and these files need no root): KEY=VALUE lines.
pub const READ_SCRIPT: &str = "for k in ro.product.model ro.product.device ro.board.platform ro.soc.model ro.soc.manufacturer; do \
echo \"$k=$(getprop $k)\"; done; echo \"soc0.machine=$(cat /sys/devices/soc0/machine 2>/dev/null)\"; \
echo \"soc0.soc_id=$(cat /sys/devices/soc0/soc_id 2>/dev/null)\"; \
echo \"dt=$(cat /proc/device-tree/compatible 2>/dev/null | tr '\\0' ',')\"";

/// The facts from [`READ_SCRIPT`]'s output.
pub fn parse(out: &str) -> Facts {
    let mut f = Facts::default();
    for line in out.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim().trim_end_matches(',').to_string();
        match key.trim() {
            "ro.product.model" => f.model = value,
            "ro.product.device" => f.device = value,
            "ro.board.platform" => f.platform = value,
            "ro.soc.model" => f.soc_model = value,
            "ro.soc.manufacturer" => f.soc_manufacturer = value,
            "soc0.machine" => f.machine = value,
            "soc0.soc_id" => f.soc_id = value,
            "dt" => f.compatible = value,
            _ => {}
        }
    }
    f
}

pub fn read(adb: &Adb) -> Result<Facts> {
    Ok(parse(&adb.shell(READ_SCRIPT)?.out))
}

/// A compatible entry "vendor,NAME" whose NAME is `name`, any case (host_device.rs's compatible_has).
fn compatible_has(list: &str, name: &str) -> bool {
    let parts: Vec<&str> = list.split(',').collect();
    parts.chunks(2).any(|pair| pair.len() == 2 && pair[1].eq_ignore_ascii_case(name))
}

/// The entry the facts match, in host_device.rs's order, and the fact that matched it.
pub fn matched(f: &Facts) -> Option<(&'static HostDevice, String)> {
    if !f.model.is_empty() {
        if let Some(e) = TABLE.iter().find(|e| e.models.iter().any(|m| f.model.starts_with(m))) {
            return Some((e, format!("ro.product.model={}", f.model)));
        }
    }
    if !f.device.is_empty() {
        if let Some(e) = TABLE.iter().find(|e| e.devices.iter().any(|d| d.eq_ignore_ascii_case(&f.device))) {
            return Some((e, format!("ro.product.device={}", f.device)));
        }
    }
    for e in TABLE {
        for s in e.socs {
            if !f.soc_model.is_empty() && s.eq_ignore_ascii_case(&f.soc_model) {
                return Some((e, format!("ro.soc.model={} (the SoC's entry)", f.soc_model)));
            }
            if !f.machine.is_empty() && s.eq_ignore_ascii_case(&f.machine) {
                return Some((e, format!("soc0.machine={} (the SoC's entry)", f.machine)));
            }
        }
    }
    if !f.platform.is_empty() {
        if let Some(e) = TABLE.iter().find(|e| e.platforms.iter().any(|p| p.eq_ignore_ascii_case(&f.platform))) {
            return Some((e, format!("ro.board.platform={} (the SoC's entry)", f.platform)));
        }
    }
    if !f.compatible.is_empty() {
        if let Some(e) = TABLE.iter().find(|e| e.socs.iter().chain(e.platforms).any(|n| compatible_has(&f.compatible, n))) {
            return Some((e, format!("the device tree's compatible {} (the SoC's entry)", f.compatible)));
        }
    }
    None
}

/// "ro.product.model=SM-S928B ro.product.device=e3q ..." (host_device.rs's host_describe).
pub fn describe(f: &Facts) -> String {
    let values = [&f.model, &f.device, &f.platform, &f.soc_model, &f.soc_manufacturer];
    let mut out: Vec<String> =
        PROPS.iter().zip(values).map(|(k, v)| format!("{k}={}", if v.is_empty() { "(none)" } else { v.as_str() })).collect();
    for (k, v) in [("soc0.machine", &f.machine), ("soc0.soc_id", &f.soc_id), ("dt", &f.compatible)] {
        if !v.is_empty() {
            out.push(format!("{k}={v}"));
        }
    }
    out.join(" ")
}

/// The status line: "host: NAME (LABEL), matched by ...; defaults: ..." or "host: unknown phone (...)".
pub fn line(f: &Facts) -> String {
    match matched(f) {
        Some((e, why)) => {
            let defaults: Vec<&str> = e.config.iter().chain(e.env).copied().collect();
            let defaults = if defaults.is_empty() { "none (today's)".to_string() } else { defaults.join(", ") };
            format!("host: {} ({}), matched by {why}; defaults: {defaults}", e.name, e.label)
        }
        None => format!("host: unknown phone ({}): xnuhost keeps today's defaults (host-device=NAME forces one)", describe(f)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(model: &str, device: &str, platform: &str, soc: &str) -> Facts {
        Facts { model: model.into(), device: device.into(), platform: platform.into(), soc_model: soc.into(), ..Default::default() }
    }

    #[test]
    fn each_phone_and_unknown() {
        let name = |f: &Facts| matched(f).map(|(e, _)| e.name).unwrap_or("unknown");
        assert_eq!(name(&facts("SM-S928B", "e3q", "pineapple", "SM8650")), "galaxy-s24-ultra");
        // Every S24 Ultra model number, by its own model (host_device.c's record): US, Canada, Korea, China.
        for model in ["SM-S928U", "SM-S928U1", "SM-S928W", "SM-S928N", "SM-S9280"] {
            let (e, why) = matched(&facts(model, "e3q", "pineapple", "SM8650")).expect(model);
            assert_eq!((e.name, why), ("galaxy-s24-ultra", format!("ro.product.model={model}")));
        }
        assert_eq!(name(&facts("SM-S938B", "pa3q", "sun", "SM8750")), "galaxy-s25-ultra");
        assert_eq!(name(&facts("SM-S931B", "pa1q", "sun", "SM8750")), "galaxy-s25");
        assert_eq!(name(&facts("SM-S948B", "", "", "SM8850")), "galaxy-s26-ultra");
        // The S26 Ultra by its codename and platform alone (port-sm8850-s26: m3q, canoe [F]), as a ROM that renames it.
        assert_eq!(name(&facts("Galaxy S26 Ultra", "m3q", "", "")), "galaxy-s26-ultra");
        assert_eq!(name(&facts("", "", "canoe", "")), "galaxy-s26-ultra");
        assert_eq!(name(&facts("SM-A566E", "a56x", "s5e8855", "s5e8855")), "galaxy-a56");
        assert_eq!(name(&facts("", "pa1q", "", "")), "galaxy-s25");
        assert_eq!(name(&facts("SM-S921B", "e1q", "pineapple", "SM8650")), "galaxy-s24-ultra");
        assert_eq!(name(&facts("", "", "SUN", "")), "galaxy-s25-ultra");
        assert_eq!(name(&facts("Pixel 11 Pro", "frankel", "zuma", "Tensor G5")), "unknown");
        let dt = Facts { compatible: "qcom,pineapple-mtp,qcom,pineapple".into(), ..Default::default() };
        assert_eq!(name(&dt), "galaxy-s24-ultra");
        let dt = Facts { compatible: "qcom,pineapple-mtp".into(), ..Default::default() };
        assert_eq!(name(&dt), "unknown");
        let m = Facts { machine: "SM8850".into(), ..Default::default() };
        assert_eq!(name(&m), "galaxy-s26-ultra");
    }

    #[test]
    fn lines() {
        let f = parse("ro.product.model=SM-S928B\nro.product.device=e3q\nro.board.platform=pineapple\nro.soc.model=SM8650\nro.soc.manufacturer=QTI\nsoc0.machine=\nsoc0.soc_id=\ndt=\n");
        assert_eq!(line(&f), "host: galaxy-s24-ultra (Galaxy S24 Ultra (SM-S928, SM8650)), matched by ro.product.model=SM-S928B; defaults: none (today's)");
        let f = parse("ro.product.model=SM-A566E\nro.soc.model=s5e8855\n");
        assert_eq!(line(&f), "host: galaxy-a56 (Galaxy A56 (SM-A566, Exynos 1580)), matched by ro.product.model=SM-A566E; defaults: gpu=auto, panel-fallback=1080x2340@120, width=720, height=1560, scale=2, hz=60, return-graceful-ms=4000, astc=0, hw-backend-haptics=treble, hw-backend-fallback=1");
        let f = parse("ro.product.model=SM-S948B\nro.soc.model=SM8850\n");
        assert!(line(&f).ends_with("defaults: ONEIOS_BRIGHT_MAX=2300"));
        let f = parse("ro.product.model=Pixel 11 Pro\nro.soc.model=Tensor G5\nsoc0.soc_id=519\ndt=google,zuma,\n");
        assert_eq!(
            line(&f),
            "host: unknown phone (ro.product.model=Pixel 11 Pro ro.product.device=(none) ro.board.platform=(none) ro.soc.model=Tensor G5 \
             ro.soc.manufacturer=(none) soc0.soc_id=519 dt=google,zuma): xnuhost keeps today's defaults (host-device=NAME forces one)"
        );
    }

    /// The strings of `field: &["a", "b"],` (or `field: NONE,`) in one entry of the bridges' Rust table.
    fn rs_list(entry: &str, field: &str) -> Vec<String> {
        let key = format!("{field}: &[");
        let Some(at) = entry.find(&key) else { return Vec::new() };
        let rest = &entry[at + key.len()..];
        let body = &rest[..rest.find(']').expect("a closing bracket")];
        body.split('"').skip(1).step_by(2).map(str::to_string).collect()
    }

    fn rs_string(entry: &str, field: &str) -> String {
        let key = format!("{field}: \"");
        let at = entry.find(&key).unwrap_or_else(|| panic!("no {field} in {entry}")) + key.len();
        entry[at..at + entry[at..].find('"').unwrap()].to_string()
    }

    #[test]
    fn table_matches_the_bridges() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../../runtime/bridges/hw/host_device.rs");
        let source = std::fs::read_to_string(path).expect("runtime/bridges/hw/host_device.rs");
        let start = source.find("static TABLE: [HostDevice; ").expect("the table");
        let body = &source[start..start + source[start..].find("\n];").expect("the table's end")];
        let code: String = body.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
        let entries: Vec<&str> = code.split("HostDevice {").skip(1).collect();
        assert_eq!(entries.len(), TABLE.len(), "host_device.rs has {} entries, host.rs {}", entries.len(), TABLE.len());
        for (b, r) in entries.iter().zip(TABLE) {
            assert_eq!(rs_string(b, "name"), r.name);
            assert_eq!(rs_string(b, "label"), r.label);
            for (field, ours) in [
                ("models", r.models),
                ("devices", r.devices),
                ("socs", r.socs),
                ("platforms", r.platforms),
                ("config", r.config),
                ("env", r.env),
            ] {
                assert_eq!(rs_list(b, field), ours.iter().map(|s| s.to_string()).collect::<Vec<_>>(), "{}: {field}", r.name);
            }
            assert_eq!(b.contains("takeover: true"), r.takeover, "{}: takeover", r.name);
        }
    }
}
