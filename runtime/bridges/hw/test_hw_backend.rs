// SPDX-License-Identifier: MIT
//! Host unit tests of the hardware backend layer (C: hw/test_hw_backend.c): the settings, the merge of the command line
//! over the image set's xnuhost.conf, the environment of the ctl tools, the choice and its log lines; and the host
//! device table and its detection on stand-in property dumps and sysfs folders.

use super::testdir::{env_lock, TempDir};
use super::*;
use crate::log::{Capture, Level};
use std::env;

/// A stand-in phone's properties as `adb shell getprop` prints them.
fn phone(
    dir: &TempDir,
    name: &str,
    model: &str,
    device: &str,
    platform: &str,
    soc: &str,
    manufacturer: &str,
) -> String {
    dir.put(
        &format!("{name}.props"),
        format!(
            "[ro.build.version.release]: [16]\n[ro.product.model]: [{model}]\n[ro.product.device]: [{device}]\n\
             [ro.board.platform]: [{platform}]\n[ro.soc.model]: [{soc}]\n[ro.soc.manufacturer]: [{manufacturer}]\n\
             [ro.product.name]: [x]\n"
        ),
    )
}

/// The entry's name ("unknown", "error") and why.
fn detect(props: &str, root: &str) -> (&'static str, String) {
    match host_choose("auto", props, root) {
        Err(why) => ("error", why),
        Ok((e, _, why)) => (e.map_or("unknown", |e| e.name), why),
    }
}

#[test]
fn host_device() {
    let _env = env_lock();
    let tmp = TempDir::new("host");
    let empty = tmp.at("empty-root");
    std::fs::create_dir_all(&empty).unwrap();

    // The table: the home device first; names unique; every entry matches by a model; its xnuhost settings are only the
    // keys the config merges as device defaults (gpu, panel-fallback, hz, width, height, scale, return-graceful-ms,
    // astc, hw-backend*), its environment ones only
    // variables a bridge reads. The S24 Ultra's entry holds nothing: its behaviour is today's.
    let t = host_table();
    assert!(t.len() == 5 && t[0].name == "galaxy-s24-ultra");
    for (i, e) in t.iter().enumerate() {
        assert!(!e.name.is_empty() && !e.label.is_empty() && !e.models.is_empty());
        assert!(std::ptr::eq(host_find(e.name).unwrap(), e));
        assert!(t[..i].iter().all(|o| !o.name.eq_ignore_ascii_case(e.name)));
        for c in e.config {
            let key = c.split('=').next().unwrap_or("");
            let device_key = matches!(
                key,
                "gpu" | "panel-fallback" | "hz" | "width" | "height" | "scale" | "return-graceful-ms" | "astc"
            );
            assert!(device_key || key.starts_with("hw-backend"), "{c}");
        }
        for v in e.env {
            assert!(
                v.starts_with("ONEIOS_BRIGHT_MAX=")
                    || v.starts_with("ONEIOS_THERMAL_SOC_TRIPS=")
                    || v.starts_with("ONEIOS_THERMAL_SOC_SAMPLES="),
                "{v}"
            );
        }
    }
    assert!(t[0].config.is_empty() && t[0].env.is_empty());
    assert!(std::ptr::eq(host_find("GALAXY-A56").unwrap(), &t[4]) && host_find("pixel").is_none());
    assert_eq!(host_names(), "galaxy-s24-ultra, galaxy-s25-ultra, galaxy-s25, galaxy-s26-ultra, galaxy-a56");

    // Each known phone by its model (the phone gates' properties).
    let p = phone(&tmp, "s24u", "SM-S928B", "e3q", "pineapple", "SM8650", "QTI");
    assert_eq!(detect(&p, &empty), ("galaxy-s24-ultra", "matched by ro.product.model=SM-S928B".to_owned()));
    // Every S24 Ultra model number gets the home entry by its own model (host_device.rs's record): the US, Canadian
    // (SM-S928W, with the US vendor partition that nothing here reads), Korean and Chinese ones.
    for model in ["SM-S928U", "SM-S928U1", "SM-S928W", "SM-S928N", "SM-S9280"] {
        let p = phone(&tmp, model, model, "e3q", "pineapple", "SM8650", "QTI");
        assert_eq!(detect(&p, &empty), ("galaxy-s24-ultra", format!("matched by ro.product.model={model}")));
    }
    // The SM-S928W as its stock firmware reports it (the OneOS fork's public issue 128): the US bootloader and vendor
    // model.
    let p = tmp.put(
        "s928w-stock.props",
        "[ro.product.model]: [SM-S928W]\n[ro.product.vendor.model]: [SM-S928U]\n[ro.bootloader]: [S928USQU6EZIF]\n\
         [ro.product.device]: [e3q]\n[ro.board.platform]: [pineapple]\n[ro.soc.model]: [SM8650]\n",
    );
    assert_eq!(detect(&p, &empty), ("galaxy-s24-ultra", "matched by ro.product.model=SM-S928W".to_owned()));
    // The S26 Ultra by its codename and by its platform alone (docs/s26.md: m3q, canoe [F]), as a ROM that renames it.
    let p = phone(&tmp, "s26u-rom", "Galaxy S26 Ultra", "m3q", "", "", "");
    assert_eq!(detect(&p, &empty), ("galaxy-s26-ultra", "matched by ro.product.device=m3q".to_owned()));
    let p = phone(&tmp, "s26u-board", "", "", "canoe", "", "");
    let (got, why) = detect(&p, &empty);
    assert!(got == "galaxy-s26-ultra" && why.contains("ro.board.platform=canoe"), "{got} {why}");
    for (name, model, device, platform, soc, maker, want) in [
        ("s25u", "SM-S938B", "pa3q", "sun", "SM8750", "QTI", "galaxy-s25-ultra"),
        ("s25", "SM-S931B", "pa1q", "sun", "SM8750", "QTI", "galaxy-s25"),
        ("s26u", "SM-S948B", "", "", "SM8850", "QTI", "galaxy-s26-ultra"),
        ("a56", "SM-A566E", "a56x", "s5e8855", "s5e8855", "Samsung", "galaxy-a56"),
    ] {
        let p = phone(&tmp, name, model, device, platform, soc, maker);
        let (got, why) = detect(&p, &empty);
        assert!(got == want && why.contains(model), "{name}: {got} {why}");
    }
    // Then the codename, then the SoC (a phone of the same SoC gets its entry: the S24 on SM8650 the home device's).
    let p = phone(&tmp, "codename", "", "pa1q", "", "", "");
    assert_eq!(detect(&p, &empty), ("galaxy-s25", "matched by ro.product.device=pa1q".to_owned()));
    let p = phone(&tmp, "s24", "SM-S921B", "e1q", "pineapple", "SM8650", "QTI");
    assert_eq!(detect(&p, &empty), ("galaxy-s24-ultra", "matched by ro.soc.model=SM8650 (the SoC's entry)".to_owned()));
    let p = phone(&tmp, "platform", "", "", "SUN", "", "");
    let (got, why) = detect(&p, &empty);
    assert!(got == "galaxy-s25-ultra" && why.contains("ro.board.platform=SUN"));
    // An unknown phone: none, and what was read is said.
    let p = phone(&tmp, "pixel", "Pixel 11 Pro", "frankel", "zuma", "Tensor G5", "Google");
    assert_eq!(detect(&p, &empty), ("unknown", "unknown phone".to_owned()));
    let f = host_read(&p, &empty).unwrap();
    assert!(f.props && f.model == "Pixel 11 Pro" && f.soc_manufacturer == "Google");
    assert_eq!(
        host_describe(&f),
        "ro.product.model=Pixel 11 Pro ro.product.device=frankel ro.board.platform=zuma ro.soc.model=Tensor G5 \
         ro.soc.manufacturer=Google"
    );
    // The fallbacks: soc0's machine, the device tree's compatible list (a board's own entry does not match).
    let root = tmp.at("soc0-root");
    tmp.put("soc0-root/sys/devices/soc0/machine", "SM8850\n");
    tmp.put("soc0-root/sys/devices/soc0/soc_id", "1234\n");
    let p = tmp.put("none.props", "# nothing\n");
    assert_eq!(detect(&p, &root), ("galaxy-s26-ultra", "matched by soc0.machine=SM8850 (the SoC's entry)".to_owned()));
    let f = host_read(&p, &root).unwrap();
    assert_eq!(f.soc_id, "1234");
    let line = host_describe(&f);
    assert!(line.contains("ro.product.model=(none)") && line.contains(" soc0.machine=SM8850 soc0.soc_id=1234"));
    let root = tmp.at("dt-root");
    tmp.put("dt-root/proc/device-tree/compatible", b"qcom,pineapple-mtp\0qcom,pineapple\0");
    let (got, why) = detect(&p, &root);
    assert!(got == "galaxy-s24-ultra" && why.contains("compatible qcom,pineapple-mtp,qcom,pineapple"), "{why}");
    tmp.put("dt-root/proc/device-tree/compatible", b"qcom,pineapple-mtp\0");
    assert_eq!(detect(&p, &root).0, "unknown");
    // build.prop's form too; a file that cannot be read is an error, not an unknown phone.
    let p = tmp.put("build.prop", "# build\nro.product.model=SM-A566B\nro.soc.model=s5e8855\n");
    assert_eq!(detect(&p, &empty).0, "galaxy-a56");
    let why = host_choose("auto", &tmp.at("missing.props"), &empty).unwrap_err();
    assert!(why.contains("cannot read"));

    // Forced and off: a name wins over what the phone says; none reads nothing; a bad name is refused.
    let p = phone(&tmp, "s24u", "SM-S928B", "e3q", "pineapple", "SM8650", "QTI");
    let (e, f, why) = host_choose("galaxy-a56", &p, &empty).unwrap();
    assert!(e == Some(&t[4]) && why == "host-device=galaxy-a56 (forced)" && f.model == "SM-S928B");
    let (e, f, why) = host_choose("none", &p, &empty).unwrap();
    assert!(e.is_none() && why == "host-device=none: detection off" && !f.props);
    assert!(host_choose("pixel", &p, &empty).unwrap_err().contains("host-device=pixel is not a known phone"));
    assert_eq!(host_choose("", &p, &empty).unwrap().0, Some(&t[0]));

    // The environment's defaults: taken when unset, a variable already set wins (the S26 Ultra's brightness cap).
    env::remove_var("ONEIOS_BRIGHT_MAX");
    let s26 = host_find("galaxy-s26-ultra").unwrap();
    assert_eq!(host_env_describe(s26), "ONEIOS_BRIGHT_MAX=2300");
    host_env_apply(s26);
    assert_eq!(env::var("ONEIOS_BRIGHT_MAX").as_deref(), Ok("2300"));
    env::set_var("ONEIOS_BRIGHT_MAX", "1000");
    assert_eq!(host_env_describe(s26), "ONEIOS_BRIGHT_MAX=2300 not taken (the environment's 1000)");
    host_env_apply(s26);
    assert_eq!(env::var("ONEIOS_BRIGHT_MAX").as_deref(), Ok("1000"));
    env::remove_var("ONEIOS_BRIGHT_MAX");
    host_env_apply(&t[0]);
    assert!(env::var("ONEIOS_BRIGHT_MAX").is_err()); // the S24 Ultra's entry sets nothing
    assert_eq!(host_env_describe(&t[0]), "");

    // The ctl tools: ONEIOS_HOST_DEVICE, ONEIOS_HOST_PROPS, ONEIOS_HOST_ROOT; the line; the environment set; hw untouched
    // (no entry has hw-backend defaults today) and what the environment gave kept.
    let mut hw = HwSettings::default();
    hw.setting("hw-backend-backlight", "treble").unwrap();
    let p = phone(&tmp, "s26u", "SM-S948B", "", "", "SM8850", "QTI");
    env::set_var("ONEIOS_HOST_PROPS", &p);
    env::set_var("ONEIOS_HOST_ROOT", &empty);
    let (e, line) = host_tool(&mut hw).unwrap();
    assert!(e == Some(s26) && env::var("ONEIOS_BRIGHT_MAX").as_deref() == Ok("2300"));
    assert_eq!(
        line,
        "host: galaxy-s26-ultra, matched by ro.product.model=SM-S948B; read: ro.product.model=SM-S948B \
         ro.product.device=(none) ro.board.platform=(none) ro.soc.model=SM8850 ro.soc.manufacturer=QTI; defaults: \
         ONEIOS_BRIGHT_MAX=2300"
    );
    assert!(hw.one[Bridge::Backlight as usize] == Backend::Treble && hw.all == Backend::Unset);
    env::remove_var("ONEIOS_BRIGHT_MAX");
    env::set_var("ONEIOS_HOST_PROPS", phone(&tmp, "s24u", "SM-S928B", "e3q", "pineapple", "SM8650", "QTI"));
    let (e, line) = host_tool(&mut hw).unwrap();
    assert!(e == Some(&t[0]) && line.contains("host: galaxy-s24-ultra, matched by ro.product.model=SM-S928B;"));
    assert!(line.contains("; defaults: none (today's)") && env::var("ONEIOS_BRIGHT_MAX").is_err());
    env::set_var("ONEIOS_HOST_PROPS", phone(&tmp, "pixel", "Pixel 11 Pro", "frankel", "zuma", "Tensor G5", "Google"));
    let (e, line) = host_tool(&mut hw).unwrap();
    assert!(
        e.is_none()
            && line.contains("host: unknown phone (ro.product.model=Pixel 11 Pro ")
            && line.contains("): today's defaults")
    );
    env::set_var("ONEIOS_HOST_DEVICE", "none");
    assert_eq!(
        host_tool(&mut hw).unwrap(),
        (None, "host: detection off (ONEIOS_HOST_DEVICE=none): today's defaults".to_owned())
    );
    env::set_var("ONEIOS_HOST_DEVICE", "galaxy-s26-ultra");
    let (e, line) = host_tool(&mut hw).unwrap();
    assert!(e == Some(s26) && line.contains("host-device=galaxy-s26-ultra (forced)"));
    assert_eq!(env::var("ONEIOS_BRIGHT_MAX").as_deref(), Ok("2300"));
    env::set_var("ONEIOS_HOST_DEVICE", "toaster");
    let line = host_tool(&mut hw).unwrap_err();
    assert!(line.contains("toaster is not a known phone") && line.contains("galaxy-a56"));
    for v in ["ONEIOS_HOST_DEVICE", "ONEIOS_HOST_PROPS", "ONEIOS_HOST_ROOT", "ONEIOS_BRIGHT_MAX"] {
        env::remove_var(v);
    }
}

#[test]
fn settings_choice_and_lines() {
    // Nothing set: every bridge samsung, "the default", no fallback.
    let mut s = HwSettings::default();
    for b in Bridge::ALL {
        assert_eq!(s.choose(b, Backend::Unset), (Backend::Samsung, "the default".to_owned()));
    }
    assert!(!s.fallback_allowed());

    // The keys: hw-backend, hw-backend-<bridge>, hw-backend-fallback; other keys are not ours; bad values refused.
    use HwSettingError::{BadValue, NotOurs};
    assert!(s.setting("hw-backend", "treble").is_ok() && s.all == Backend::Treble);
    assert!(s.setting("hw-backend-touch", "samsung").is_ok() && s.one[Bridge::Touch as usize] == Backend::Samsung);
    assert!(
        s.setting("hw-backend-backlight", "treble").is_ok() && s.one[Bridge::Backlight as usize] == Backend::Treble
    );
    assert!(s.setting("hw-backend-fallback", "1").is_ok() && s.fallback == Some(true) && s.fallback_allowed());
    assert!(s.setting("hw-backend-fallback", "no").is_ok() && s.fallback == Some(false) && !s.fallback_allowed());
    assert!(s.setting("hw-backend", "pixel") == Err(BadValue) && s.all == Backend::Treble);
    assert!(s.setting("hw-backend-fallback", "maybe") == Err(BadValue) && s.fallback == Some(false));
    assert_eq!(s.setting("hw-backend-toaster", "treble"), Err(BadValue));
    assert!(s.setting("hw-backend-battery", "") == Err(BadValue) && s.one[Bridge::Battery as usize] == Backend::Unset);
    assert!(s.setting("haptics", "sim") == Err(NotOurs) && s.setting("hw-backendx", "treble") == Err(NotOurs));
    let names = [
        "backlight",
        "battery",
        "thermal",
        "haptics",
        "touch",
        "keys",
        "audio",
        "camera",
        "wifi",
        "bluetooth",
        "display",
        "encoder",
    ];
    for (i, name) in names.iter().enumerate() {
        let mut t = HwSettings::default();
        assert_eq!(Bridge::ALL[i].name(), *name);
        assert!(t.setting(&format!("hw-backend-{name}"), "treble").is_ok() && t.one[i] == Backend::Treble);
    }

    // The choice: per bridge over hw-backend over the default; a forced one (a test, a ctl tool) over all.
    assert_eq!(s.choose(Bridge::Touch, Backend::Unset), (Backend::Samsung, "hw-backend-touch=samsung".to_owned()));
    assert_eq!(
        s.choose(Bridge::Backlight, Backend::Unset),
        (Backend::Treble, "hw-backend-backlight=treble".to_owned())
    );
    assert_eq!(s.choose(Bridge::Thermal, Backend::Unset), (Backend::Treble, "hw-backend=treble".to_owned()));
    assert_eq!(s.choose(Bridge::Thermal, Backend::Samsung), (Backend::Samsung, "the caller's choice".to_owned()));

    // The command line over the image set's xnuhost.conf: what the first leaves unset comes from the second.
    let mut s = HwSettings::default();
    let mut under = HwSettings::default();
    s.setting("hw-backend-battery", "samsung").unwrap();
    under.setting("hw-backend", "treble").unwrap();
    under.setting("hw-backend-battery", "treble").unwrap();
    under.setting("hw-backend-haptics", "treble").unwrap();
    under.setting("hw-backend-fallback", "1").unwrap();
    s.merge(&under);
    assert!(s.all == Backend::Treble && s.one[Bridge::Battery as usize] == Backend::Samsung);
    assert!(s.one[Bridge::Haptics as usize] == Backend::Treble && s.fallback == Some(true));
    s.setting("hw-backend-fallback", "0").unwrap();
    s.merge(&HwSettings::default());
    assert_eq!(s.fallback, Some(false));

    // The environment (the ctl tools).
    {
        let _env = env_lock();
        let mut s = HwSettings::default();
        env::set_var("ONEIOS_HW_BACKEND", "treble");
        env::set_var("ONEIOS_HW_BACKEND_KEYS", "samsung");
        env::set_var("ONEIOS_HW_FALLBACK", "1");
        assert!(s.env().is_ok() && s.all == Backend::Treble);
        assert!(s.one[Bridge::Keys as usize] == Backend::Samsung && s.fallback == Some(true));
        env::set_var("ONEIOS_HW_BACKEND_BATTERY", "lg");
        assert_eq!(s.env(), Err("ONEIOS_HW_BACKEND_BATTERY".to_owned()));
        for v in ["ONEIOS_HW_BACKEND", "ONEIOS_HW_BACKEND_KEYS", "ONEIOS_HW_FALLBACK", "ONEIOS_HW_BACKEND_BATTERY"] {
            env::remove_var(v);
        }
    }

    // The process's settings and the log lines. Not configured: samsung, the default.
    let settings = super::settings_lock();
    let log = Capture::start();
    let last = || log.lines().pop().unwrap();
    assert_eq!(select(Bridge::Battery, Backend::Unset), Backend::Samsung);
    assert_eq!(last(), (Level::Info, "hw: battery: backend samsung (the default)".to_owned()));
    let mut s = HwSettings::default();
    s.setting("hw-backend", "treble").unwrap();
    settings.configure(&s);
    assert_eq!(select(Bridge::Thermal, Backend::Unset), Backend::Treble);
    assert_eq!(last().1, "hw: thermal: backend treble (hw-backend=treble)");
    assert!(!missing(Bridge::Thermal, "android.hardware.thermal.IThermal/default is not registered"));
    assert_eq!(
        last(),
        (
            Level::Error,
            "hw: thermal: treble: missing: android.hardware.thermal.IThermal/default is not registered; the bridge stays \
             off (hw-backend-fallback=0)"
                .to_owned()
        )
    );
    s.setting("hw-backend-fallback", "1").unwrap();
    settings.configure(&s);
    assert!(missing(Bridge::Haptics, "x"));
    assert_eq!(
        last(),
        (Level::Info, "hw: haptics: treble: missing: x; falling back to samsung (hw-backend-fallback=1)".to_owned())
    );
    settings.configure(&HwSettings::default());
    assert!(select(Bridge::Haptics, Backend::Unset) == Backend::Samsung && !hw_current().fallback_allowed());
}
