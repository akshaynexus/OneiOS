// SPDX-License-Identifier: MIT
// tests/test_xnuhost.c's test_config, test_host_device and test_session_mode: the settings, the image set's xnuhost.conf
// under the command line, ram=auto and what fits, the display's mode, the boot arguments' helpers, the host phone's layer,
// session-mode= and --host-info.

use super::*;
use std::path::PathBuf;

const M: u64 = 1 << 20;
const G: u64 = 1 << 30;
const PHONE: u64 = 11_746_000 << 10; // a 12 GB phone's MemTotal (about 11.2 GiB)

/// A fresh folder of its own for one test.
fn testdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("xnuhost-config-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("test folder");
    dir
}

fn write(path: impl AsRef<Path>, text: &str) {
    fs::write(path, text).expect("test file");
}

/// An image set with a kernel, two volumes and a Data seed (one byte each).
fn image_set(dir: &Path, name: &str) -> String {
    let image = dir.join(name);
    fs::create_dir_all(&image).unwrap();
    for f in ["kernel.kc", "os.img", "systemos.img", "data.seed"] {
        write(image.join(f), "x");
    }
    image.to_str().unwrap().to_owned()
}

fn argv(args: &[&str]) -> Vec<String> {
    std::iter::once("xnuhost").chain(args.iter().copied()).map(str::to_owned).collect()
}

/// xh_config_args with no environment.
fn args(c: &mut Config, a: &[&str]) -> Result<(), ConfigError> {
    c.args_with_env(&argv(a), |_| None)
}

/// The command line, finished, with the lock's answer `can_lock`.
fn run_lock(a: &[&str], can_lock: bool) -> Result<Config, ConfigError> {
    let mut c = Config::new();
    args(&mut c, a)?;
    c.finish_with(&|| can_lock)?;
    Ok(c)
}

/// As on the PC: no CAP_IPC_LOCK, RLIMIT_MEMLOCK 8 MiB.
fn run(a: &[&str]) -> Result<Config, ConfigError> {
    run_lock(a, false)
}

#[test]
fn settings_and_sizes() {
    // Explicit, disabled-by-default touchscreen exclusivity is not confused with the existing input= boolean. Live
    // EVIOCGRAB requires the phone.
    let mut c = Config::new();
    assert!(!c.touch_exclusive && c.input);
    assert!(c.set("touch-exclusive", "1").is_ok() && c.touch_exclusive);
    assert!(c.set("touch-exclusive", "wrong").is_err());
    assert!(c.set("touch-exclusive", "0").is_ok() && !c.touch_exclusive);
    assert_eq!(parse_size("4G"), Some(4 << 30));
    assert_eq!(parse_size("512m"), Some(512 << 20));
    assert_eq!(parse_size("0x1000"), Some(4096));
    assert_eq!(parse_size("12q"), None);
    assert_eq!(parse_size("010"), Some(8)); // strtoull's base 0: octal
    assert_eq!(parse_size(""), None);
    let mut c = Config::new();
    assert!(
        c.set("ram-giveback", "2").is_err() && c.set("ram-lock", "x").is_err() && c.set("ram-prefault", "").is_err()
    );
    assert!(c.set("ram-session-reserve", "lots").is_err() && c.set("ram-unlock-below", "2T").is_err());
    assert!(c.set("ram-fit", "x").is_err() && c.set("ram-pagetable-percent", "51").is_err());
    assert!(
        c.set("ram-hold-percent", "51").is_err()
            && c.set("ram-tasks", "4097").is_err()
            && c.set("ram-task-cost", "2T").is_err()
    );
    assert!(
        c.set("ram-tasks", "350").is_ok()
            && c.ram_tasks == 350
            && c.set("ram-task-cost", "3M").is_ok()
            && c.ram_task_cost == 3 << 20
    );
    assert!(c.set("ram-gpu-reserve", "?").is_err());
    assert!(
        c.set("task-trim", "x").is_err()
            && c.set("task-trim-idle", "0").is_err()
            && c.set("ram-tasks-awake", "4097").is_err()
            && c.set("ram-task-idle-cost", "2T").is_err()
    );
    assert!(
        c.set("task-trim-idle", "1").is_ok()
            && c.task_trim_idle == 1
            && c.set("task-trim", "0").is_ok()
            && !c.task_trim
    );
    assert!(c.set("ram-percent", "5").is_err() && c.set("ram-percent", "101").is_err() && c.set("ram", "0").is_err());
    assert!(
        c.set("ram", "lots").is_err() && c.set("ram-host-floor", "x").is_err() && c.set("host-mem-log", "-1").is_err()
    );
    assert!(c.set("ram-huge", "0").is_ok() && !c.ram_huge && c.set("ram-huge", "x").is_err());
    assert!(c.set("haptics", "off").is_ok() && c.haptics == Haptics::Off && c.set("haptics", "evdev").is_err());
    assert!(c.set("encoder", "sim").is_ok() && c.encoder == Encoder::Sim);
    assert!(c.set("encoder", "off").is_ok() && c.encoder == Encoder::Off && c.set("encoder", "mediacodec").is_err());
    assert!(c.set("touch-sleep", "0").is_ok() && !c.touch_sleep && c.set("touch-sleep", "maybe").is_err());
    assert!(c.set("haptics-native-impacts", "maybe").is_err());
    assert!(c.set("cpu-pin", "2").is_ok() && c.cpu_pin == CpuPin::Always);
    assert!(c.set("cpu-pin", "0").is_ok() && c.cpu_pin == CpuPin::Off && c.set("cpu-pin", "3").is_err());
    assert!(
        c.set("clusters", "EEPPPPPP").is_ok()
            && c.set("clusters", "EEPPPPPPP").is_err()
            && c.set("clusters", "EX").is_err()
    );
    let mut c = Config::new();
    assert!(c.set("pen-mirror", "maybe").is_err() && !c.given("pen-mirror")); // a bad value refused
    assert!(c.set("encoder", "mediacodec").is_err() && !c.given("encoder") && c.encoder == Encoder::Off);
    // display-format: the two names, nothing else
    let mut f = Config::new();
    assert!(f.set("display-format", "rgba16f").is_ok() && f.display_format == DisplayFormat::Rgba16f);
    assert_eq!(f.display_format.pixels(), 2);
    assert!(f.set("display-format", "bgrx").is_ok() && f.display_format == DisplayFormat::Bgrx8);
    assert!(f.set("display-format", "rgb565").is_err());
    // ram-view=
    let mut vc = Config::new();
    assert!(vc.ram_view == 0 && vc.set("ram-view", "12G").is_ok() && vc.ram_view == 12 << 30 && vc.given("ram-view"));
    assert!(vc.set("ram-view", "off").is_ok() && vc.ram_view == 0);
    assert!(vc.set("ram-view", "auto").is_ok() && vc.ram_view == 12 << 30);
    assert!(vc.set("ram-view", "lots").is_err());
    // disks
    let mut d = Config::new();
    assert!(
        d.set("disk", "a.img").is_ok() && d.set("disk", "a.img,seed").is_err() && d.set("disk-rw", ",seed").is_err()
    );
    assert!(d.set("data-store", "data.img,seed.img").is_ok());
    assert_eq!(
        d.disks[1],
        DiskConfig { path: "data.img".into(), writable: true, data_store: true, seed: "seed.img".into() }
    );
    for _ in 0..4 {
        assert!(d.set("disk-rw", "x").is_ok());
    }
    assert!(d.set("disk", "one-too-many").is_err());
    // unknown keys
    assert!(Config::new().set("no-such-key", "1").is_err());
}

#[test]
fn command_line_and_image_set() {
    let dir = testdir("cmdline");
    let dirs = dir.to_str().unwrap();
    let image = image_set(&dir, "image");
    let path = format!("{image}/xnuhost.conf");
    // the OneOS fork's command line (oneos/interface.md, section 4)
    let fork = [
        "--session-abi",
        "1",
        "--mode",
        "takeover",
        "--runtime",
        "/data/runtime",
        "--image",
        &image,
        "--data",
        dirs,
        "--session",
        dirs,
    ];
    let c = run(&fork).unwrap();
    assert!(c.kernel.ends_with("/image/kernel.kc") && c.disks.len() == 4);
    assert!(c.disks[3].path.contains("data.seed") && !c.disks[3].writable && !c.disks[0].writable);
    assert!(c.disks[1].path.contains("systemos.img"));
    assert!(c.disks[2].data_store && c.disks[2].writable && c.disks[2].path.ends_with("/data.img"));
    assert!(c.disks[2].seed.contains("data.seed"));
    assert!(c.ncpu >= 1 && c.ncpu as usize <= MAX_CPUS && c.display == DisplayBackend::Shm && c.input && c.tasks);
    assert!(c.bridges && c.bluetooth == Bluetooth::Auto && c.wifi == Wifi::Auto); // on in a session
    assert!(c.touch_sleep);
    // The exclusive physical touchscreen may only be grabbed in a non-takeover bench using the SHM display and no
    // hardware bridges. All other modes fail.
    let c = run(&[
        "--image",
        &image,
        "--mode",
        "bench",
        "--display",
        "shm",
        "--set",
        "bridges=0",
        "--set",
        "touch-exclusive=1",
    ])
    .unwrap();
    assert!(c.touch_exclusive && c.input && !c.bridges && c.display == DisplayBackend::Shm);
    assert!(run(&["--image", &image, "--set", "touch-exclusive=1"]).is_err());
    let no_input =
        ["--image", &image, "--mode", "bench", "--set", "bridges=0", "--set", "touch-exclusive=1", "--no-input"];
    assert!(run(&no_input).is_err());
    let bad_display =
        ["--image", &image, "--mode", "bench", "--display", "none", "--set", "bridges=0", "--set", "touch-exclusive=1"];
    assert!(run(&bad_display).is_err());
    let bridges =
        ["--image", &image, "--mode", "bench", "--display", "shm", "--set", "bridges=1", "--set", "touch-exclusive=1"];
    assert!(run(&bridges).is_err());
    // the image set's own settings, and what the command line overrides
    write(&path, "# settings of this image\nboot-args=-v debug=0x8\n");
    let c = run(&["--image", &image, "--cpus", "6", "--ram", "6G", "--set", "display=none", "--no-input"]).unwrap();
    assert!(c.boot_args == "-v debug=0x8" && c.ncpu == 6 && c.ram_bytes == 6 << 30);
    assert!(c.display == DisplayBackend::None && !c.input && c.disks.len() == 3);
    // Bluetooth, haptics, the encoder, Wi-Fi from the command line
    let bench_hidl = ["--image", &image, "--mode", "bench", "--bluetooth", "hidl"];
    let c = run(&bench_hidl).unwrap();
    assert!(!c.bridges && c.bluetooth == Bluetooth::Hidl);
    assert_eq!(run(&["--image", &image, "--set", "bluetooth=virtual"]).unwrap().bluetooth, Bluetooth::Virtual);
    let c = run(&["--image", &image, "--mode", "bench", "--set", "bluetooth=sim"]).unwrap(); // the PC's stand-in
    assert!(!c.bridges && c.bluetooth == Bluetooth::Sim);
    assert_eq!(Config::new().haptics, Haptics::Auto);
    let c = run(&["--image", &image, "--mode", "bench", "--set", "haptics=sim"]).unwrap(); // the PC's stand-in actuator
    assert!(!c.bridges && c.haptics == Haptics::Sim);
    // encoder: off by default; auto (the phone's MediaCodec in a session, the stand-in with the bridges off); sim (the
    // stand-in always); anything else refused.
    assert_eq!(Config::new().encoder, Encoder::Off);
    let c = run(&["--image", &image, "--mode", "bench", "--set", "encoder=auto"]).unwrap(); // a PC boot
    assert!(!c.bridges && c.encoder == Encoder::Auto);
    assert_eq!(run(&["--image", &image, "--set", "wifi=off"]).unwrap().wifi, Wifi::Off);
    assert_eq!(run(&["--image", &image, "--set", "wifi=sim"]).unwrap().wifi, Wifi::Sim);
    // refused
    assert!(args(&mut Config::new(), &["--image", &image, "--set", "wifi=nat"]).is_err());
    assert!(args(&mut Config::new(), &["--image", &image, "--bluetooth", "usb"]).is_err());
    let e = run(&["--image", &image, "--session-abi", "2"]).unwrap_err();
    assert_eq!(e.lines, ["session ABI 2 is not supported (this build: 1)"]);
    assert!(args(&mut Config::new(), &["--image", &image, "--cpus", "9"]).is_err());
    assert!(args(&mut Config::new(), &["--image", &image, "--scale", "4"]).is_err());
    assert!(args(&mut Config::new(), &["--image", &image, "--hz", "500"]).is_err());
    assert!(args(&mut Config::new(), &["--image", &image, "--text-fixup", "file,10,aa"]).is_err());
    let e = run(&["--image", dirs]).unwrap_err(); // no kernel there
    assert_eq!(e.lines, [format!("no kernel.kc in {dirs} (and no --kernel)")]);
    // what the command line says when it is wrong, as C said it
    let e = args(&mut Config::new(), &["--image"]).unwrap_err();
    assert!(e.usage && e.lines == ["unknown or incomplete argument --image"]);
    let e = args(&mut Config::new(), &["--set", "novalue"]).unwrap_err();
    assert_eq!(e.lines, ["--set takes KEY=VALUE"]);
    let e = args(&mut Config::new(), &["--cpus", "x"]).unwrap_err();
    assert_eq!(e.lines, ["bad value for --cpus"]);
    let e = args(&mut Config::new(), &["--set", "cpus=x"]).unwrap_err();
    assert_eq!(e.lines, ["bad setting cpus"]);
    let e = args(&mut Config::new(), &["-h"]).unwrap_err();
    assert!(e.usage && e.lines.is_empty());
    let e = run(&["--image", &image, "--mode", "shell"]).unwrap_err();
    assert_eq!(e.lines, ["mode shell is not supported"]);
    let e = run(&["--image", &image, "--display", "vnc"]).unwrap_err();
    assert_eq!(e.lines, ["display vnc is not supported"]);
    let e = run(&[]).unwrap_err();
    assert_eq!(e.lines, ["--image DIR is required"]);
    // a bad line of a --config file: both lines, as C printed them
    let bad = format!("{dirs}/bad.conf");
    write(&bad, "cpus=6\nwidth=1\n\n# c\ncpus=99\n");
    let e = args(&mut Config::new(), &["--config", &bad]).unwrap_err();
    assert_eq!(e.lines, [format!("{bad}:5: bad setting cpus"), format!("cannot use {bad}")]);
    let mut c = Config::new();
    write(&bad, "  cpus=3  \n");
    assert!(args(&mut c, &["--config", &bad]).is_ok() && c.ncpu == 3);
    // the environment (ONEIOS_*) first, then argv
    let mut c = Config::new();
    let env = |name: &str| match name {
        "ONEIOS_MODE" => Some("bench".to_owned()),
        "ONEIOS_IMAGE" => Some(image.clone()),
        "ONEIOS_DATA" => Some(String::new()), // empty: not used
        _ => None,
    };
    assert!(c.args_with_env(&argv(&["--mode", "takeover"]), env).is_ok());
    assert!(c.mode == Mode::Takeover && c.image == image && c.data.is_empty());
    let e = Config::new().args_with_env(&argv(&[]), |n| (n == "ONEIOS_SESSION_ABI").then(|| "1".repeat(600))).err();
    assert!(e.is_none()); // session-abi takes any text (atoi), refused at the finish
    let e = Config::new().args_with_env(&argv(&[]), |n| (n == "ONEIOS_IMAGE").then(|| "x".repeat(600))).unwrap_err();
    assert_eq!(e.lines, ["bad ONEIOS_IMAGE"]);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn image_set_settings() {
    let dir = testdir("imageset");
    let image = image_set(&dir, "image");
    let path = format!("{image}/xnuhost.conf");
    let meminfo = dir.join("meminfo");
    write(&meminfo, "MemTotal:       11746000 kB\nMemFree:          401234 kB\nMemAvailable:    7340032 kB\nBuffers:            1234 kB\n");
    let set_meminfo = format!("ram-meminfo={}", meminfo.display());
    let img1 = ["--image", image.as_str(), "--mode", "takeover"];
    // The image set's bridges=, wifi=, bluetooth=, haptics= apply when the command line has none (they were read, then
    // ignored)
    write(&path, &format!("boot-args=-v debug=0x8\nbridges=0\nwifi=sim\nbluetooth=sim\nhaptics=sim\n{set_meminfo}\n"));
    let c = run(&img1).unwrap();
    assert!(!c.bridges && c.wifi == Wifi::Sim && c.bluetooth == Bluetooth::Sim && c.haptics == Haptics::Sim);
    let c = run(&[&img1[..], &["--set", "bridges=1", "--set", "wifi=auto", "--set", "haptics=off"]].concat()).unwrap();
    assert!(c.bridges && c.wifi == Wifi::Auto && c.bluetooth == Bluetooth::Sim && c.haptics == Haptics::Off); // the command line wins, auto too
                                                                                                              // The image set's pen=, pen-rotate=, pen-mirror= apply too (until 2026-11-22 they were read, then dropped: the
                                                                                                              // phone's pen=auto never turned the pen on); the command line wins.
    write(&path, &format!("boot-args=-v debug=0x8\npen=auto\npen-rotate=90\npen-mirror=1\n{set_meminfo}\n"));
    let c = run(&img1).unwrap();
    assert!(c.pen == Pen::Auto && c.pen_rotate == PenRotate::R90 && c.pen_rotate.degrees() == Some(90) && c.pen_mirror);
    let c =
        run(&[&img1[..], &["--set", "pen=off", "--set", "pen-rotate=auto", "--set", "pen-mirror=0"]].concat()).unwrap();
    assert!(c.pen == Pen::Off && c.pen_rotate == PenRotate::Auto && !c.pen_mirror);
    // The image set's encoder= applies (until 2026-11-22 it was read, then dropped: encoder=auto in the phone's
    // xnuhost.conf never gave the guest its encoder); the command line wins.
    write(&path, &format!("boot-args=-v debug=0x8\nencoder=auto\n{set_meminfo}\n"));
    assert_eq!(run(&img1).unwrap().encoder, Encoder::Auto);
    assert_eq!(run(&[&img1[..], &["--set", "encoder=off"]].concat()).unwrap().encoder, Encoder::Off);
    // the image set's camera-model= applies (the phone's overlay/userland/xnuhost.conf); the command line wins
    write(&path, &format!("boot-args=-v debug=0x8\ncamera-model=V54AP\n{set_meminfo}\n"));
    assert_eq!(run(&img1).unwrap().camera_model, "V54AP");
    assert_eq!(run(&[&img1[..], &["--set", "camera-model=OneiOSAP"]].concat()).unwrap().camera_model, "OneiOSAP");
    write(&path, &format!("boot-args=-v debug=0x8\ncamera-vm=show\n{set_meminfo}\n"));
    assert_eq!(run(&img1).unwrap().camera_vm, CameraVm::Show);
    assert_eq!(run(&[&img1[..], &["--set", "camera-vm=hide"]].concat()).unwrap().camera_vm, CameraVm::Hide);
    // hw-backend: samsung by default for every bridge; xnuhost.conf, --set and --hw-backend*; the command line wins per
    // setting over the image set's file; a bad value or an unknown bridge refused.
    let c = Config::new();
    for b in Bridge::ALL {
        assert_eq!(c.hw.choose(b, Backend::Unset).0, Backend::Samsung);
    }
    assert!(!c.hw.fallback_allowed());
    write(&path, "boot-args=-v debug=0x8\nhw-backend=treble\nhw-backend-keys=samsung\nhw-backend-fallback=1\n");
    let c = run(&["--image", &image, "--hw-backend-battery", "samsung", "--set", "hw-backend-fallback=0"]).unwrap();
    assert_eq!(c.hw.choose(Bridge::Backlight, Backend::Unset).0, Backend::Treble);
    assert_eq!(c.hw.choose(Bridge::Keys, Backend::Unset).0, Backend::Samsung);
    assert!(c.hw.choose(Bridge::Battery, Backend::Unset).0 == Backend::Samsung && !c.hw.fallback_allowed());
    let c = run(&["--image", &image, "--hw-backend", "samsung"]).unwrap();
    assert_eq!(c.hw.choose(Bridge::Thermal, Backend::Unset).0, Backend::Samsung);
    assert!(c.hw.choose(Bridge::Keys, Backend::Unset).0 == Backend::Samsung && c.hw.fallback_allowed());
    assert!(args(&mut Config::new(), &["--image", &image, "--hw-backend", "pixel"]).is_err());
    assert!(args(&mut Config::new(), &["--image", &image, "--set", "hw-backend-toaster=treble"]).is_err());
    // the panel's fallback from the image set's xnuhost.conf; the command line's wins
    write(&path, "# settings of this image\nboot-args=-v debug=0x8\npanel-fallback=1080x2340@120\n");
    let c = run(&["--image", &image]).unwrap();
    assert_eq!(c.panel_fallback, Some(PanelMode { width: 1080, height: 2340, hz: 120 }));
    let c = run(&["--image", &image, "--set", "panel-fallback=1260x2800@60"]).unwrap();
    assert_eq!(c.panel_fallback, Some(PanelMode { width: 1260, height: 2800, hz: 60 }));
    // boot-args-add (YumikoHayamato/OneiOS 440abb2): added to the boot arguments, whichever gave them, each unless they
    // name it; the command line's wins over the image set's.
    write(&path, "# settings of this image\nboot-args=-v debug=0x8\nboot-args-add=oneios-disable-jobs=a,b -x\n");
    assert_eq!(run(&["--image", &image]).unwrap().boot_args, "-v debug=0x8 oneios-disable-jobs=a,b -x");
    let c = run(&["--image", &image, "--set", "boot-args-add=oneios-disable-jobs=c"]).unwrap();
    assert_eq!(c.boot_args, "-v debug=0x8 oneios-disable-jobs=c");
    let c = run(&["--image", &image, "--set", "boot-args=-x oneios-disable-jobs=z"]).unwrap();
    assert_eq!(c.boot_args, "-x oneios-disable-jobs=z"); // both named already
    write(&path, "boot-args-add=oneios-disable-jobs=a,b\n");
    let c = run(&["--image", &image]).unwrap();
    assert!(c.boot_args.contains("vgpu-3d=1 ") && c.boot_args.ends_with(" oneios-disable-jobs=a,b")); // after the default
    assert!(c.boot_args.starts_with(DEFAULT_BOOT_ARGS));
    let long = format!("boot-args=-{}", "a".repeat(999));
    let e = run(&["--image", &image, "--set", &long]).unwrap_err(); // 1000 + 24 do not fit
    assert_eq!(e.lines, ["boot-args-add does not fit beside the guest boot args (1023 bytes at most)"]);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn native_impacts() {
    // haptics-native-impacts (docs/hardware.md 5): unset by default (the environment decides, no boot argument added);
    // =1 in the image set's xnuhost.conf, the OneOS fork's command line (which passes no environment) turns on both
    // halves: the HAL's presets and oneios_native_impacts=1 after the default boot arguments, kept whole.
    let dir = testdir("impacts");
    let dirs = dir.to_str().unwrap();
    let image = image_set(&dir, "image");
    let path = format!("{image}/xnuhost.conf");
    let fork = [
        "--session-abi",
        "1",
        "--mode",
        "takeover",
        "--runtime",
        "/data/runtime",
        "--image",
        &image,
        "--data",
        dirs,
        "--session",
        dirs,
        "--ram",
        "4G",
    ];
    write(&path, "hw-backend-haptics=treble\n");
    let c = run(&fork).unwrap();
    assert!(c.haptics_native_impacts.is_none() && !c.boot_args.contains("oneios_native_impacts"));
    write(&path, "haptics-native-impacts=1\nhw-backend-haptics=treble\nhw-backend-fallback=0\n");
    let c = run(&fork).unwrap();
    assert_eq!(c.haptics_native_impacts, Some(true));
    assert_eq!(c.hw.choose(Bridge::Haptics, Backend::Unset).0, Backend::Treble);
    assert!(c.boot_args.contains("native-source-storage=1 ") && c.boot_args.ends_with(" oneios_native_impacts=1"));
    assert_eq!(c.boot_args.matches("oneios_native_impacts").count(), 1);
    // the image set's own boot arguments: kept, and an oneios_native_impacts=1 already there not doubled
    write(
        &path,
        "haptics-native-impacts=1\nhw-backend-haptics=treble\nboot-args=-v oneios_native_impacts=1 debug=0x8\n",
    );
    assert_eq!(run(&fork).unwrap().boot_args, "-v oneios_native_impacts=1 debug=0x8");
    write(&path, "haptics-native-impacts=1\nhw-backend-haptics=treble\nboot-args=-v debug=0x8\n");
    assert_eq!(run(&fork).unwrap().boot_args, "-v debug=0x8 oneios_native_impacts=1");
    // refused: the guest told otherwise (=0, a longer value, twice), the evdev backend, a fallback to it, no motor bridge
    for refused in [
        "haptics-native-impacts=1\nhw-backend-haptics=treble\nboot-args=-v oneios_native_impacts=0\n",
        "haptics-native-impacts=1\nhw-backend-haptics=treble\nboot-args=-v oneios_native_impacts=10\n",
        "haptics-native-impacts=1\nhw-backend-haptics=treble\nboot-args=oneios_native_impacts=1 oneios_native_impacts=1\n",
        "haptics-native-impacts=1\nhw-backend-haptics=treble\nhw-backend-fallback=1\n",
        "haptics-native-impacts=1\nhw-backend-haptics=samsung\n",
        "haptics-native-impacts=1\n",
    ] {
        write(&path, refused);
        assert!(run(&fork).is_err(), "{refused}");
    }
    // no motor bridge: haptics=sim or off (command line), or bench mode (the bridges off unless asked for)
    write(&path, "haptics-native-impacts=1\nhw-backend-haptics=treble\n");
    assert!(run(&["--image", &image, "--ram", "4G", "--set", "haptics=sim"]).is_err());
    assert!(run(&["--image", &image, "--ram", "4G", "--set", "haptics=off"]).is_err());
    assert!(run(&["--image", &image, "--ram", "4G", "--mode", "bench"]).is_err());
    // the command line's =0 wins over the image set's =1: off, no boot argument
    let c = run(&["--image", &image, "--ram", "4G", "--set", "haptics-native-impacts=0"]).unwrap();
    assert!(c.haptics_native_impacts == Some(false) && !c.boot_args.contains("oneios_native_impacts"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ram_arithmetic() {
    // ram=auto (docs/booter.md 1): min(90% of MemTotal, MemTotal - 1.5 GiB) in 256 MiB steps, at least 1 GiB, at most
    // 64 GiB. 11,746,000 kB stands for a 12 GB phone's MemTotal (about 11.2 GiB; the phone's own number is read at the
    // start).
    assert_eq!(ram_auto(PHONE, 90, RAM_AUTO_FLOOR), 9728 * M); // the floor decides: 9.5 GiB
    assert_eq!(ram_auto(PHONE, 50, RAM_AUTO_FLOOR), 5632 * M); // the share decides
    assert_eq!(ram_auto(PHONE, 90, 4 * G), 7168 * M);
    assert!(ram_auto(64 * G, 90, RAM_AUTO_FLOOR) == 58880 * M && ram_auto(128 * G, 90, RAM_AUTO_FLOOR) == RAM_MAX);
    assert!(ram_auto(2560 * M, 90, RAM_AUTO_FLOOR) == G && ram_auto(2 * G, 90, RAM_AUTO_FLOOR) == 0);
    assert_eq!(ram_auto(G, 100, 0), G);
    assert!(ram_auto(RAM_AUTO_FLOOR, 90, RAM_AUTO_FLOOR) == 0 && ram_auto(0, 90, 0) == 0);
    // What fits as of 2026-11-14 (the arithmetic, with the terms of then: no tasks, 8% of the guest for page tables):
    // G + 8% of G + 256 MiB (GPU) + 1 GiB (headroom) <= MemAvailable at the start.
    let (r, gr, pt) = (G, RAM_GPU_RESERVE, 8);
    let old = |r: u64| FitTerms {
        reserve: r,
        gpu: gr,
        tasks: 0,
        task_cost: 0,
        awake: 0,
        idle_cost: 0,
        hold_percent: 0,
        pagetable_percent: pt,
    };
    assert_eq!(ram_fit(7 * G, &old(r)) / M, 5451); // (7168 - 1280) / 1.08
    assert!(ram_fit(r + gr + 108 * M, &old(r)) == 100 * M && ram_fit(r + gr, &old(r)) == 0 && ram_fit(0, &old(r)) == 0);
    assert_eq!(
        ram_fit(
            6 * G,
            &FitTerms {
                reserve: 0,
                gpu: 0,
                tasks: 0,
                task_cost: 0,
                awake: 0,
                idle_cost: 0,
                hold_percent: 0,
                pagetable_percent: 0
            }
        ),
        6 * G
    );
    let floor = RAM_AUTO_FLOOR;
    assert_eq!(ram_fit_auto(PHONE, 90, floor, Some(ram_fit(7 * G, &old(r)))), (5376 * M, Some(RamBound::Available)));
    assert_eq!(ram_fit_auto(PHONE, 90, floor, None), (9728 * M, Some(RamBound::Floor))); // ram-fit=0
    assert_eq!(ram_fit_auto(PHONE, 90, floor, Some(10000 * M)), (9728 * M, Some(RamBound::Floor))); // fits: the floor
    assert_eq!(ram_fit_auto(PHONE, 50, floor, Some(10000 * M)), (5632 * M, Some(RamBound::Share)));
    assert_eq!(ram_fit_auto(PHONE, 90, floor, Some(1279 * M)).0, G);
    assert_eq!(ram_fit_auto(PHONE, 90, floor, Some(1023 * M)), (0, None));
    // The phone of 2026-11-14 (MemTotal 11,084 MiB): the old ram=auto gave 9472 MiB and Linux swapped 2126 MiB of it.
    // What fits for a start MemAvailable of 8.5 GiB [assumed: that session's start line is not in the notes] is 6656.
    assert_eq!(ram_auto(11084 * M, 90, floor), 9472 * M);
    assert_eq!(ram_fit_auto(11084 * M, 90, floor, Some(ram_fit(8704 * M, &old(r)))).0, 6656 * M);
    // The host swaps the guest's RAM: a line once past 64 MiB, then at each further 256 MiB (320, 576, ...).
    let mut next = 0;
    assert!(
        !swap_note(10 * M, &mut next)
            && !swap_note(64 * M, &mut next)
            && swap_note(65 * M, &mut next)
            && next == 320 * M
    );
    assert!(!swap_note(300 * M, &mut next) && !swap_note(320 * M, &mut next) && swap_note(321 * M, &mut next));
    assert_eq!(next, 576 * M);
    assert!(
        swap_note(2126 * M, &mut next)
            && next == 2368 * M
            && !swap_note(2200 * M, &mut next)
            && !swap_note(0, &mut next)
    );
    // The fit with the lock (fix-ram-locked, 2026-11-15), the phone's session of main 3233fed: MemAvailable 8561 MiB at
    // the start. With the lock the headroom is 768 MiB: (8561 - 768 - 256) / 1.08 = 6978 MiB -> 6912 MiB; without it
    // 6741 -> 6656 MiB, as that session.
    assert_eq!(ram_fit(8561 * M, &old(768 * M)) / M, 6978);
    let locked = ram_fit(8561 * M, &old(768 * M));
    assert_eq!(ram_fit_auto(11084 * M, 90, floor, Some(locked)), (6912 * M, Some(RamBound::Available)));
    assert_eq!(ram_fit_auto(11084 * M, 90, floor, Some(ram_fit(8561 * M, &old(r)))).0, 6656 * M);
    // What that leaves the host when the session is up (page tables at 8 % of the guest, the GPU's 256 MiB): 8561 -
    // 6912 - 552 - 256 = 841 MiB of MemAvailable, the valve's 256 MiB and 585 MiB more for what else grows.
    assert!(8561 * M - 6912 * M - 6912 * M * pt as u64 / 100 - gr > RAM_UNLOCK_BELOW + 512 * M);
}

/// The terms of ram=auto with the lock, as on the phone (2026-11-23).
fn locked_terms(reserve: u64) -> FitTerms {
    FitTerms {
        reserve,
        gpu: RAM_GPU_RESERVE,
        tasks: RAM_TASKS,
        task_cost: RAM_TASK_COST,
        awake: u32::MAX, // the terms of 2026-11-23, before the trim: every task at ram-task-cost
        idle_cost: RAM_TASK_IDLE_COST,
        hold_percent: RAM_HOLD_PERCENT,
        pagetable_percent: RAM_PAGETABLE_PERCENT,
    }
}

#[test]
fn ram_fit_sessions_2026_11_23() {
    // The phone's two sessions of 2026-11-23 (MemTotal 11084 MiB, the lock, the image's ram-session-reserve=2G, the
    // terms of 2026-11-14): out/session-logs/2026-11-23-mac-s1824 and -s1904. Their ram=auto lines: "what fits ...
    // 5686 MiB" and "5793 MiB", the guest 5632 MiB both times.
    let floor = RAM_AUTO_FLOOR;
    let then = FitTerms {
        reserve: 2 * G,
        gpu: RAM_GPU_RESERVE,
        tasks: 0,
        task_cost: 0,
        awake: 0,
        idle_cost: 0,
        hold_percent: 0,
        pagetable_percent: 8,
    };
    assert_eq!(ram_fit(8445 * M, &then) / M, 5686);
    assert_eq!(ram_fit(8561 * M, &then) / M, 5793);
    assert_eq!(ram_fit_auto(11084 * M, 90, floor, Some(ram_fit(8561 * M, &then))).0, 5632 * M);
    // What the sessions needed outside the guest (MiB), measured: holding the RAM (MemAvailable at the start less the
    // guest less MemAvailable once it was held), and the growth after it to the end (MemAvailable's fall plus the RAM
    // the host freed by swapping, 2/3 of the swap's growth at zram's 3:1 [guess]).
    // (start, held, the end's MemAvailable, swap at the lock, swap at the end, live tasks, page tables at the lock and the end)
    let sessions = [
        (8445u64, 2480u64, 1159u64, 533u64, 902u64, 342u64, 60u64, 743u64),
        (8561, 2540, 1077, 628, 905, 353, 60, 788),
    ];
    let t = locked_terms(RAM_SESSION_RESERVE_LOCKED);
    for (start, held, end, swap0, swap1, tasks, pt0, pt1) in sessions {
        let hold = start - 5632 - held;
        let growth = held - end + (swap1 - swap0) * 2 / 3;
        assert!((333..=389).contains(&hold) && (1500..=1700).contains(&growth), "{start}: hold {hold} growth {growth}");
        // The terms cover what was measured: 7 % of the guest for holding it, 400 tasks at 4 MiB and the GPU's 256 MiB
        // for the growth; page tables alone were 2.0-2.1 MiB a task.
        assert!(5632 * t.percent() / 100 >= hold && (t.tasks_bytes() + t.gpu) >> 20 >= growth);
        assert!((pt1 - pt0) * 1024 / tasks <= RAM_TASK_COST >> 10 && (pt1 - pt0) * 1024 / tasks > 1900);
        // The old size did not leave the headroom: MemAvailable at the end would have been under it without the swap.
        assert!(start - 5632 - hold - growth < RAM_SESSION_RESERVE_LOCKED >> 20);
        // The new size: what fits, in 256 MiB steps, is 4608 MiB for both starts.
        let fit = ram_fit(start * M, &t);
        let (ram, bound) = ram_fit_auto(11084 * M, 90, floor, Some(fit));
        assert_eq!((ram, bound), (4608 * M, Some(RamBound::Available)), "{start}");
        // With it, the measured needs (holding scaled to the size) leave more than the headroom at the peak, and the
        // model's own estimate of the peak too.
        let left = start - 4608 - hold * 4608 / 5632 - growth;
        assert!(left >= RAM_SESSION_RESERVE_LOCKED >> 20, "{start}: {left}");
        assert!(t.left_at_peak(start * M, ram) >= RAM_SESSION_RESERVE_LOCKED as i64);
    }
    assert_eq!(ram_fit(8561 * M, &t) / M, 4830); // (8561 - 1536 - 256 - 1600) / 1.07
    assert_eq!(ram_fit(8445 * M, &t) / M, 4722);
    assert_eq!(t.left_at_peak(8561 * M, 4608 * M) / M as i64, 1774); // 8561 - 4608 - 322 - 256 - 1600
                                                                     // With the image's ram-session-reserve=2G kept: (8561 - 2048 - 1856) / 1.07 = 4352 MiB.
    assert_eq!(ram_fit_auto(11084 * M, 90, floor, Some(ram_fit(8561 * M, &locked_terms(2 * G)))).0, 4352 * M);
    // Nothing fits under the fixed terms; a host with less than they need starts no guest.
    assert_eq!(ram_fit(t.fixed(), &t), 0);
    assert_eq!(ram_fit_auto(4 * G, 90, floor, Some(ram_fit(4 * G, &t))), (0, None));
    // ram-tasks=0 and ram-hold-percent=0: only the headroom and the GPU's (the test kernel's check sets them small).
    let bare = FitTerms { tasks: 0, hold_percent: 0, ..t };
    assert_eq!(ram_fit(4 * G, &bare), 4 * G - RAM_SESSION_RESERVE_LOCKED - RAM_GPU_RESERVE);
}

#[test]
fn ram_fit_with_the_trim() {
    // The terms with task-trim (task.rs, "The trim", the default since 2026-11-23): of 400 tasks 150 awake at 4 MiB,
    // 250 trimmed at 1 MiB: 850 MiB instead of 1600.
    let t = FitTerms { awake: RAM_TASKS_AWAKE, ..locked_terms(RAM_SESSION_RESERVE_LOCKED) };
    assert_eq!(t.tasks_bytes(), 850 * M);
    // The S24 Ultra's starts of 2026-11-23 (MemAvailable 8561 and 8445 MiB): 5376 MiB both times, 768 MiB more than
    // the 4608 MiB of the terms without the trim.
    for (start, fit) in [(8561u64, 5531u64), (8445, 5423)] {
        assert_eq!(ram_fit(start * M, &t) / M, fit); // (start - 1536 - 256 - 850) / 1.07
        let (ram, bound) = ram_fit_auto(11084 * M, 90, RAM_AUTO_FLOOR, Some(ram_fit(start * M, &t)));
        assert_eq!((ram, bound), (5376 * M, Some(RamBound::Available)), "{start}");
        assert!(t.left_at_peak(start * M, ram) >= RAM_SESSION_RESERVE_LOCKED as i64);
        assert_eq!(
            ram_fit_auto(
                11084 * M,
                90,
                RAM_AUTO_FLOOR,
                Some(ram_fit(start * M, &locked_terms(RAM_SESSION_RESERVE_LOCKED)))
            )
            .0,
            4608 * M
        );
    }
    // 8561 - 5376 - 376.3 (7 %) - 256 - 850 = 1702 MiB left at the peak, 166 above the headroom.
    assert_eq!(t.left_at_peak(8561 * M, 5376 * M) / M as i64, 1702);
    // More awake than tasks: all of them at 4 MiB; none awake: all at 1 MiB.
    assert_eq!(FitTerms { awake: 4096, ..t }.tasks_bytes(), 1600 * M);
    assert_eq!(FitTerms { awake: 0, ..t }.tasks_bytes(), 400 * M);
}

#[test]
fn host_change_by_user() {
    // The hold line (memory.rs held_tick): the change by user between two looks, largest first, 1 MiB or more.
    let before = [("page cache", 3000 * M), ("anonymous", 400 * M), ("page tables", 60 * M), ("the rest", 900 * M)];
    let after = [
        ("the guest's RAM", 5632 * M),
        ("page cache", 2700 * M),
        ("anonymous", 400 * M),
        ("page tables", 71 * M),
        ("the rest", 1180 * M),
    ];
    assert_eq!(
        host_change_text(&before, 6000 * M, &after, 30 * M),
        "free -5970, the guest's RAM +5632, page cache -300, the rest +280, page tables +11"
    );
    assert_eq!(host_change_text(&before, 0, &before, 0), "");
}

#[test]
fn host_pressure() {
    let r = RAM_SESSION_RESERVE_LOCKED;
    let mut p = Pressure::default();
    // The phone at 19:04-19:06 (one look a 30 s line here): 2540, 1966: nothing; 1503 (under 1536): the line, once;
    // 1187, 1208 and 1105 with swap-ins of 36 MB/s and 1077 with 42: no more lines.
    assert_eq!(p.step(0, 2540 * M, 0, 5, r), None);
    assert_eq!(p.step(30, 1966 * M, 22 << 10, 5, r), None);
    assert_eq!(p.step(60, 1503 * M, 108 << 10, 5, r), Some(PressureNote::Start { short: true, swapping: false }));
    assert!(p.active());
    for (t, a, s) in
        [(90, 1187, 597u64 << 10), (120, 1208, 2540 << 10), (150, 1105, 36324 << 10), (180, 1077, 41717 << 10)]
    {
        assert_eq!(p.step(t, a * M, s, 5, r), None);
    }
    // Calm (MemAvailable at least the headroom and 128 MiB, swap-ins under 1 MiB/s) for 30 s: over, once.
    assert_eq!(p.step(185, 1700 * M, 0, 5, r), None);
    assert_eq!(p.step(200, 1700 * M, 2 << 20, 5, r), None); // swap-ins of 2 MiB/s: not calm, the 30 s start again
    assert_eq!(p.step(205, 1700 * M, 0, 5, r), None);
    assert_eq!(p.step(230, 1600 * M, 0, 5, r), None); // under 1536 + 128: not calm
    assert_eq!(p.step(235, 1700 * M, 0, 5, r), None);
    assert_eq!(p.step(260, 1700 * M, 0, 5, r), None);
    let over = p.step(265, 1800 * M, 0, 5, r);
    let swapped = ((108 + 597 + 2540 + 36324 + 41717) << 10) * 5 + (2 << 20) * 5;
    assert_eq!(over, Some(PressureNote::Over { seconds: 205, lowest: 1077 * M, swapped_in: swapped }));
    assert!(!p.active() && p.step(270, 1800 * M, 0, 5, r).is_none());
    // Swap-ins alone start one (MemAvailable above the headroom): 4 MiB/s or more over the look.
    assert_eq!(p.step(275, 2000 * M, (4 << 20) - 1, 5, r), None);
    assert_eq!(p.step(280, 2000 * M, 4 << 20, 5, r), Some(PressureNote::Start { short: false, swapping: true }));
    assert_eq!(p.step(285, 1000 * M, 50 << 20, 5, r), None);

    // The host's RAM by user, largest first (MiB), as at the end of 19:06 [the slab and stack figures made up].
    let m = HostMemory {
        total: 11084 * M,
        free: 210 * M,
        cached: 6420 * M,
        shmem: 5700 * M,
        anon: 905 * M,
        page_tables: 788 * M,
        slab_unreclaimable: 690 * M,
        slab_reclaimable: 150 * M,
        kernel_stack: 61 * M,
    };
    let users = host_users(&m, 5632 * M, 2 * M, 300 * M);
    assert_eq!(
        host_users_text(&users),
        "the guest's RAM 5632, the rest 1558, anonymous 905, page tables 788, page cache 720, unreclaimable slab 690, \
         zram's pool 300, reclaimable slab 150, other shmem 68, kernel stacks 61, KGSL's own 2"
    );
    // From one proc_values read; a missing value counts 0 and drops out; nothing named past the total: no rest.
    let v = [Some(4 * G), Some(G), None, None, Some(G), None, None, None, None, Some(2 * G), None, None];
    let h = HostMemory::from_values(&v);
    assert!(h.total == 4 * G && h.free == G && h.anon == G && h.cached == 0);
    assert_eq!(host_users_text(&host_users(&h, 0, 0, 0)), "the rest 2048, anonymous 1024");
    assert_eq!(host_users_text(&host_users(&HostMemory { total: G, anon: 2 * G, ..h }, 0, 0, 0)), "anonymous 2048");
}

#[test]
fn proc_reads() {
    let dir = testdir("proc");
    let meminfo = dir.join("meminfo");
    let partial = dir.join("meminfo-partial");
    write(&meminfo, "MemTotal:       11746000 kB\nMemFree:          401234 kB\nMemAvailable:    7340032 kB\nBuffers:            1234 kB\n");
    write(&partial, "MemTotal:        2097152 kB\nMemFree:          1048576 kB\n");
    assert_eq!(meminfo_fn(&meminfo), Some((PHONE, 7 * G)));
    assert!(meminfo_fn(&partial).is_none() && meminfo_fn("/nonexistent/meminfo").is_none());
    assert_eq!(meminfo_value(&meminfo, "MemFree"), Some(401234 << 10));
    assert_eq!(meminfo_value(&meminfo, "Buffers"), Some(1234 << 10));
    assert!(meminfo_value(&meminfo, "Mem").is_none() && meminfo_value(&meminfo, "SwapTotal").is_none());
    assert!(meminfo_value("/nonexistent/meminfo", "MemFree").is_none());
    // Several values in one read (host_memory_tick's one /proc/meminfo; vmstat's "key N" lines too).
    let mv = proc_values(&meminfo, &["MemAvailable", "Buffers", "SwapFree", "MemTotal"]);
    assert_eq!(mv, [Some(7 * G), Some(1234 << 10), None, Some(PHONE)]);
    assert_eq!(proc_values("/nonexistent/meminfo", &["MemAvailable"]), [None]);
    let vmstat = dir.join("vmstat");
    write(&vmstat, "nr_free_pages 12345\npswpin 777\npswpout 888888\npswpout_zero 1\n");
    assert_eq!(proc_values(&vmstat, &["pswpin", "pswpout"]), [Some(777), Some(888888)]);
    let _ = fs::remove_dir_all(&dir);
}

/// config::meminfo (the module's function, beside the tests' own `meminfo` paths).
fn meminfo_fn(path: impl AsRef<Path>) -> Option<(u64, u64)> {
    super::meminfo(path)
}

#[test]
fn ram_config() {
    let dir = testdir("ram");
    let image = image_set(&dir, "image");
    let path = format!("{image}/xnuhost.conf");
    let meminfo = dir.join("meminfo");
    let small = dir.join("meminfo-small");
    write(&meminfo, "MemTotal:       11746000 kB\nMemFree:          401234 kB\nMemAvailable:    7340032 kB\nBuffers:            1234 kB\n");
    write(&small, "MemTotal:        2097152 kB\nMemAvailable:    1048576 kB\n");
    let set_meminfo = format!("ram-meminfo={}", meminfo.display());
    let set_small = format!("ram-meminfo={}", small.display());
    let set_none = format!("ram-meminfo={}/none", dir.display());
    let with = |extra: &[&str]| -> Vec<String> {
        let mut a: Vec<String> = vec!["--image".into(), image.clone(), "--set".into(), set_meminfo.clone()];
        for s in extra {
            a.push("--set".into());
            a.push((*s).into());
        }
        a
    };
    let go = |a: &[String], lock: bool| run_lock(&a.iter().map(String::as_str).collect::<Vec<_>>(), lock);
    // The default since fix-ram-locked (2026-11-15): ram-giveback=0, prefaulted and locked, with what fits: MemAvailable
    // 7 GiB here. Where the lock cannot work (here) the headroom is 1792 MiB, and of the 400 tasks 150 awake at 4 MiB and 250
    // trimmed at 1 MiB (task-trim, 850 MiB): (7168 - 1792 - 256 - 850) / 1.07 = 3990 -> 3840 MiB.
    let c = go(&with(&[]), false).unwrap();
    assert!(c.ram_bytes == 3840 * M && c.ram_auto_total == PHONE && c.ram_auto_available == 7 * G);
    assert!(c.ram_percent == 90 && c.ram_host_floor == RAM_AUTO_FLOOR && c.host_mem_log == 30 && c.usb_log == 3);
    assert!(
        c.ready_content_wait == READY_CONTENT_WAIT && c.ram_auto_bound == Some(RamBound::Available) && c.ram_fit_on
    );
    assert!(c.ram_fit / M == 3990 && c.ram_fit_available == 7 * G && !c.ram_giveback && c.ram_prefault && c.ram_lock);
    assert!(!c.ram_lock_expected && c.ram_lock_fit == 0 && c.ram_session_reserve == RAM_SESSION_RESERVE);
    assert!(
        c.ram_unlock_below == RAM_UNLOCK_BELOW
            && c.ram_gpu_reserve == RAM_GPU_RESERVE
            && c.ram_pagetable_percent == RAM_PAGETABLE_PERCENT
    );
    assert!(c.ram_hold_percent == RAM_HOLD_PERCENT && c.ram_tasks == RAM_TASKS && c.ram_task_cost == RAM_TASK_COST);
    assert!(c.return_graceful_ms == 0);
    // Where the lock can work (the phone: root with CAP_IPC_LOCK): the headroom is 1536 MiB, (7168 - 2642) / 1.07 = 4229
    // -> 4096 MiB.
    let c = go(&with(&[]), true).unwrap();
    assert!(c.ram_bytes == 4096 * M && c.ram_lock_expected && c.ram_session_reserve == RAM_SESSION_RESERVE_LOCKED);
    assert!(c.ram_fit / M == 4229 && !c.ram_giveback && c.ram_lock);
    assert!(c.ram_lock_fit == 4096 * M && c.ram_huge); // the locked size that fits; 2 MiB pages asked
    assert!(
        c.task_trim
            && c.task_trim_idle == TASK_TRIM_IDLE
            && c.ram_tasks_awake == RAM_TASKS_AWAKE
            && c.ram_task_idle_cost == RAM_TASK_IDLE_COST
    );
    // Without the trim every task counts at 4 MiB, as on 2026-11-23: (7168 - 1536 - 256 - 1600) / 1.07 = 3528 -> 3328.
    let c = go(&with(&["task-trim=0"]), true).unwrap();
    assert!(c.ram_bytes == 3328 * M && c.ram_fit / M == 3528 && !c.task_trim);
    // ram-fit-terms=old (docs/switches.md): the terms before 3b8742a: headroom 768 MiB locked, page tables 8 %, no tasks,
    // no hold share: (7168 - 768 - 256) / 1.08 = 5688. A key given by hand still wins; unlocked the headroom is 1 GiB.
    let c = go(&with(&["ram-fit-terms=old"]), true).unwrap();
    assert!(c.ram_fit_terms_old && c.ram_fit / M == 5688 && c.ram_session_reserve == RAM_SESSION_RESERVE_LOCKED_OLD);
    assert!(c.ram_pagetable_percent == 8 && c.ram_hold_percent == 0 && c.ram_tasks == 0 && c.ram_bytes <= c.ram_fit);
    let c = go(&with(&["ram-fit-terms=old", "ram-hold-percent=7"]), true).unwrap();
    assert!(c.ram_hold_percent == 7 && c.ram_pagetable_percent == 8 && c.ram_fit / M == (7168 - 768 - 256) * 100 / 115);
    let c = go(&with(&["ram-fit-terms=old"]), false).unwrap();
    assert!(c.ram_session_reserve == RAM_SESSION_RESERVE_OLD && c.ram_fit / M == (7168 - 1024 - 256) * 100 / 108);
    assert!(go(&with(&["ram-fit-terms=new"]), true).is_err());
    // The tasks' terms by hand: 400 tasks, 100 awake at 4 MiB, 300 at 512 KiB: (7168 - 1536 - 256 - 550) / 1.07 = 4510.
    let c = go(&with(&["ram-tasks-awake=100", "ram-task-idle-cost=512K"]), true).unwrap();
    assert!(c.ram_fit / M == 4510 && c.ram_bytes == 4352 * M);
    // The owner's boot of 2026-11-15 says the default explicitly: the same.
    let c = go(&with(&["ram-giveback=0", "ram-prefault=1", "ram-lock=1"]), true).unwrap();
    assert!(c.ram_bytes == 4096 * M && c.ram_lock_expected && c.ram_prefault);
    // One setting flips it back: ram-giveback=1 is 0058's way, nothing prefaulted or locked, the 1 GiB headroom.
    let c = go(&with(&["ram-giveback=1"]), true).unwrap();
    assert!(c.ram_bytes == 3840 * M && c.ram_giveback && !c.ram_prefault && !c.ram_lock && !c.ram_lock_expected);
    assert_eq!(c.ram_session_reserve, RAM_SESSION_RESERVE);
    // The valve stays on with the lock: ram-unlock-below=0 or 64M becomes 128 MiB; a headroom under the valve's level
    // and 256 MiB is raised to that (else the valve would open as soon as the session is up).
    let c = go(&with(&["ram-unlock-below=0", "ram-session-reserve=200M"]), true).unwrap();
    assert!(c.ram_unlock_below == RAM_UNLOCK_MIN && c.ram_session_reserve == RAM_UNLOCK_MIN + RAM_VALVE_MARGIN);
    let c = go(&with(&["ram-unlock-below=512M", "ram-session-reserve=1G"]), true).unwrap();
    assert!(c.ram_unlock_below == 512 * M && c.ram_session_reserve == G);
    // The RAM held fixed (ram-giveback=0 said): the same size; prefaulted and locked.
    let c = go(&with(&["ram-giveback=0"]), false).unwrap();
    assert!(
        c.ram_bytes == 3840 * M
            && c.ram_auto_bound == Some(RamBound::Available)
            && !c.ram_giveback
            && c.ram_prefault
            && c.ram_lock
    );
    let c = go(&with(&["ram-giveback=0", "ram-fit=0"]), false).unwrap(); // what fits off: the old formula
    assert!(c.ram_bytes == 9728 * M && c.ram_auto_bound == Some(RamBound::Floor) && c.ram_prefault && c.ram_lock);
    let c = go(&with(&["ram-giveback=1", "ram-fit=0"]), false).unwrap(); // 0058's way as before 2026-11-14
    assert!(c.ram_bytes == 9728 * M && c.ram_giveback && !c.ram_prefault && !c.ram_lock);
    let no_prefault =
        ["ram-prefault=0", "ram-lock=1", "ram-session-reserve=512M", "ram-unlock-below=0", "ram-giveback=0"];
    // (7168 - 512 - 256 - 850) / 1.07 = 5186 -> 5120 MiB
    let c = go(&with(&no_prefault), false).unwrap(); // no prefault: no lock either
    assert!(c.ram_bytes == 5120 * M && !c.ram_prefault && !c.ram_lock && c.ram_unlock_below == 0 && !c.ram_giveback);
    let c = go(&with(&["ram-percent=50", "host-mem-log=0"]), false).unwrap();
    assert!(c.ram_bytes == 3840 * M && c.host_mem_log == 0 && c.ram_auto_bound == Some(RamBound::Available));
    let c = go(&with(&["ram-host-floor=4G", "ram-fit=0"]), false).unwrap();
    assert_eq!(c.ram_bytes, 7168 * M);
    // a host too small for auto: say so, start nothing
    assert!(run(&["--image", &image, "--set", &set_small]).is_err());
    // no meminfo: the old 4 GiB
    let c = run(&["--image", &image, "--set", &set_none]).unwrap();
    assert!(c.ram_bytes == 4 * G && c.ram_auto_total == 0);
    // a size given: no auto
    let c = run(&["--image", &image, "--set", &set_small, "--ram", "1536M"]).unwrap();
    assert!(c.ram_bytes == 1536 * M && c.ram_auto_total == 0);
    let e = run(&["--image", &image, "--ram", "65G"]).unwrap_err();
    assert_eq!(e.lines, ["--ram must be a multiple of 2 MiB, at least 256 MiB, at most 64 GiB"]);
    // The image set's xnuhost.conf: its ram= and ram-percent= apply when the command line has none; --ram auto wins.
    write(&path, &format!("boot-args=-v debug=0x8\nram=3G\nram-percent=80\nram-fit=0\n{set_meminfo}\n"));
    let c = run(&["--image", &image]).unwrap();
    assert!(c.ram_bytes == 3 * G && c.ram_percent == 80);
    let c = run(&["--image", &image, "--ram", "auto"]).unwrap();
    assert!(c.ram_bytes == 8960 * M && c.ram_auto_total == PHONE); // 80% of 11.2 GiB
                                                                   // The image set's own ram-giveback=0 holds the RAM fixed, at what fits (3840 MiB of MemAvailable 7 GiB)
    write(&path, &format!("boot-args=-v debug=0x8\nram-percent=80\nram-giveback=0\n{set_meminfo}\n"));
    let c = run(&["--image", &image, "--ram", "auto"]).unwrap();
    assert!(c.ram_bytes == 3840 * M && !c.ram_giveback && c.ram_lock);
    let c = run(&["--image", &image, "--set", "ram-giveback=1"]).unwrap(); // the command line wins
    assert!(c.ram_bytes == 3840 * M && c.ram_giveback && !c.ram_lock);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn boot_argument_helpers() {
    // owner-hide-vmm naming bluetoothd (a crash loop in 24A446): found in any position of the list, not as a prefix
    assert!(boot_args_hides_vmm_from("-v owner-hide-vmm=bluetoothd,BlueTool wdt=-1", "bluetoothd"));
    assert!(boot_args_hides_vmm_from("owner-hide-vmm=audiomxd,bluetoothd", "bluetoothd"));
    assert!(!boot_args_hides_vmm_from("owner-hide-vmm=audiomxd,com.apple.WebKit.GPU bluetoothd=1", "bluetoothd"));
    assert!(!boot_args_hides_vmm_from("owner-hide-vmm=bluetoothd2 xowner-hide-vmm=bluetoothd", "bluetoothd"));
    assert!(!boot_args_hides_vmm_from("", "bluetoothd"));
    // cameracaptured added to the first owner-hide-vmm= word (the one the kernel reads), or a new word; never twice
    let add = |args: &str, size: usize, name: &str| {
        let mut a = args.to_owned();
        let r = boot_args_hide_vmm_add(&mut a, size, name);
        (r, a)
    };
    let (r, a) = add("-v wdt=-1", 96, "cameracaptured");
    assert_eq!((r, a.as_str()), (Ok(true), "-v wdt=-1 owner-hide-vmm=cameracaptured"));
    assert_eq!(add(&a, 96, "cameracaptured"), (Ok(false), "-v wdt=-1 owner-hide-vmm=cameracaptured".to_owned()));
    let (r, b) = add("-v owner-hide-vmm=audiomxd,com.apple.WebKit.GPU wdt=-1", 96, "cameracaptured");
    assert_eq!((r, b.as_str()), (Ok(true), "-v owner-hide-vmm=audiomxd,com.apple.WebKit.GPU,cameracaptured wdt=-1"));
    assert!(boot_args_hides_vmm_from(&b, "cameracaptured") && boot_args_hides_vmm_from(&b, "audiomxd"));
    assert_eq!(add("owner-hide-vmm=", 96, "cameracaptured"), (Ok(true), "owner-hide-vmm=cameracaptured".to_owned()));
    let (r, d) = add("owner-hide-vmm=audiomxd x owner-hide-vmm=cameracaptured", 96, "cameracaptured"); // the first word
    assert_eq!((r, d.as_str()), (Ok(true), "owner-hide-vmm=audiomxd,cameracaptured x owner-hide-vmm=cameracaptured"));
    assert_eq!(add("-v wdt=-1", 24, "cameracaptured"), (Err(NoRoom), "-v wdt=-1".to_owned())); // no room: unchanged
    let mut g = String::from("owner-hide-vmm=procname1"); // the kernel's 127 characters for the list
    for _ in 1..12 {
        g.push_str(",procname");
    }
    assert_eq!(g.len() - 15, 108);
    assert_eq!(boot_args_hide_vmm_add(&mut g, 200, "cameracaptured"), Ok(true));
    assert_eq!(boot_args_hide_vmm_add(&mut g, 200, "anotherprocess"), Err(NoRoom));
    // camera-vm=: hide by default, show; nothing else
    let mut c = Config::new();
    assert_eq!(c.camera_vm, CameraVm::Hide);
    assert!(
        c.set("camera-vm", "show").is_ok() && c.camera_vm == CameraVm::Show && c.set("camera-vm", "maybe").is_err()
    );
    // camera-model= (/product unique-model with the cameras): OneiOSAP by default (in no AVGestalt table: VirtualAudio
    // starts); V54AP the iPhone's; letters and digits ending in AP only, at most 15
    let mut c = Config::new();
    assert!(c.camera_model == "OneiOSAP" && !c.given("camera-model"));
    assert!(c.set("camera-model", "V54AP").is_ok() && c.given("camera-model") && c.camera_model == "V54AP");
    assert!(c.set("camera-model", "OneiOSAP").is_ok() && c.camera_model == "OneiOSAP");
    assert!(c.set("camera-model", "V54").is_err() && c.set("camera-model", "AP").is_err());
    assert!(c.set("camera-model", "../V54AP").is_err() && c.set("camera-model", "One iOSAP").is_err());
    assert!(c.set("camera-model", "ABCDEFGHIJKLMAP").is_ok() && c.set("camera-model", "ABCDEFGHIJKLMNAP").is_err());
    assert_eq!(c.camera_model, "ABCDEFGHIJKLMAP");
    // bt-transport-override read as bluetoothd reads it: any word that contains the name, the value after its '=',
    // under 13, the last word wins
    assert_eq!(boot_args_bt_transport("-v bt-transport-override=10 wdt=-1"), Some(10));
    assert_eq!(boot_args_bt_transport("bt-transport-override=10 bt-transport-override=1"), Some(1));
    assert_eq!(boot_args_bt_transport("xbt-transport-override=10"), Some(10));
    assert_eq!(boot_args_bt_transport("bt-transport-override=13"), None);
    assert_eq!(boot_args_bt_transport("bt-transport-override=10 bt-transport-override=99"), Some(10));
    assert_eq!(boot_args_bt_transport("bt-transport-override"), None);
    assert_eq!(boot_args_bt_transport("-v wdt=-1 transport=10"), None);
    assert_eq!(boot_args_bt_transport(""), None);
    // ram-view=: the compressor's room for the view (the view over the RAM, rounded up), added once; a given value stays
    let mut v = "-v".to_owned();
    assert!(view_boot_args(&mut v, 96, 0, 7 << 30) == Ok(0) && v == "-v"); // off
    assert!(view_boot_args(&mut v, 96, 6 << 30, 7 << 30) == Ok(0) && v == "-v"); // not over the RAM
    assert!(view_boot_args(&mut v, 96, 12 << 30, 7 << 30) == Ok(2) && v == "-v kern.compressor_pool_multiplier=2");
    assert!(view_boot_args(&mut v, 96, 12 << 30, 7 << 30) == Ok(2) && v == "-v kern.compressor_pool_multiplier=2");
    let mut v = "kern.compressor_pool_multiplier=4".to_owned();
    assert!(view_boot_args(&mut v, 96, 12 << 30, 4 << 30) == Ok(3) && v == "kern.compressor_pool_multiplier=4");
    assert_eq!(view_boot_args(&mut String::new(), 20, 12 << 30, 4 << 30), Err(NoRoom)); // no room
                                                                                        // the display driver's switches on the kernel's command line: added once, a given value stays
    let mut a = "-v debug=0x8".to_owned();
    assert!(display_boot_args(&mut a, 128, 3, 120, DisplayFormat::Bgrx8).is_ok());
    assert_eq!(a, "-v debug=0x8 offline-legacy-unit-scale=3 sheng-vsync=120");
    assert!(display_boot_args(&mut a, 128, 2, 60, DisplayFormat::Bgrx8).is_ok());
    assert_eq!(a, "-v debug=0x8 offline-legacy-unit-scale=3 sheng-vsync=120");
    let mut a = "sheng-vsync=90 not-sheng-vsync=1".to_owned();
    assert!(display_boot_args(&mut a, 128, 2, 120, DisplayFormat::Bgrx8).is_ok());
    assert_eq!(a, "sheng-vsync=90 not-sheng-vsync=1 offline-legacy-unit-scale=2");
    let mut a = String::new();
    assert!(display_boot_args(&mut a, 128, 2, 120, DisplayFormat::Bgrx8).is_ok());
    assert_eq!(a, "offline-legacy-unit-scale=2 sheng-vsync=120");
    assert!(display_boot_args(&mut a, 44, 2, 120, DisplayFormat::Bgrx8).is_ok());
    assert_eq!(display_boot_args(&mut String::new(), 40, 2, 120, DisplayFormat::Bgrx8), Err(NoRoom)); // no room
                                                                                                      // a half-float frame: the guest is told to hand its half-float surfaces over as they are; a given value stays
    let mut a = String::new();
    assert!(display_boot_args(&mut a, 128, 3, 120, DisplayFormat::Rgba16f).is_ok());
    assert_eq!(a, "oneios-display-wide=1 offline-legacy-unit-scale=3 sheng-vsync=120");
    let mut a = "oneios-display-wide=0".to_owned();
    assert!(display_boot_args(&mut a, 128, 3, 120, DisplayFormat::Rgba16f).is_ok());
    assert_eq!(a, "oneios-display-wide=0 offline-legacy-unit-scale=3 sheng-vsync=120");
}

#[test]
fn display_mode() {
    let dir = testdir("display");
    let image = image_set(&dir, "image");
    let path = format!("{image}/xnuhost.conf");
    write(&path, "# settings of this image\nboot-args=-v debug=0x8\n");
    let pm = |width, height, hz| PanelMode { width, height, hz };
    // The display's mode: the panel's native (largest) mode, not the one One UI runs; overrides; the defaults.
    let modes = [
        pm(1080, 2340, 120),
        pm(1440, 3120, 60),
        pm(1440, 3120, 120),
        pm(720, 1560, 120),
        pm(1440, 3120, 24),
        pm(0, 0, 120),
    ];
    assert_eq!(panel_native(&modes), Some(pm(1440, 3120, 120)));
    assert_eq!(panel_native(&modes[..1]), Some(pm(1080, 2340, 120))); // an FHD+ panel is its own native
    assert!(panel_native(&modes[5..]).is_none() && panel_native(&[]).is_none());
    let bench_hidl = ["--image", image.as_str(), "--mode", "bench", "--bluetooth", "hidl"];
    let mut c = run(&bench_hidl).unwrap();
    assert!(c.scale == 3 && c.hz == 0 && c.scale_fixup);
    assert_eq!(c.display_mode(None), pm(1440, 3120, 120)); // nothing readable: the S24 Ultra's
    assert_eq!(c.display_mode(Some(&pm(1260, 2800, 144))), pm(1260, 2800, 144)); // another panel: its own
    assert_eq!(c.display_mode(Some(&pm(1260, 2800, 0))).hz, 120); // a mode without a rate
                                                                  // panel-fallback= (the Galaxy S25's and A56's 1080x2340): only where the panel is not read; a read panel still wins
    assert_eq!(c.panel_fallback(), pm(1440, 3120, 120));
    assert!(c.set("panel-fallback", "1080x2340@120").is_ok() && c.panel_fallback == Some(pm(1080, 2340, 120)));
    assert_eq!(c.display_mode(None), pm(1080, 2340, 120));
    assert_eq!(c.display_mode(Some(&pm(1260, 2800, 144))), pm(1260, 2800, 144));
    assert!(c.set("panel-fallback", "1080x2340").is_ok() && c.panel_fallback == Some(pm(1080, 2340, 0)));
    assert_eq!(c.display_mode(None).hz, 120);
    assert!(c.set("panel-fallback", "1080x2340@60").is_ok() && c.display_mode(None).hz == 60);
    let rateless = pm(1260, 2800, 0);
    assert!(c.display_mode(Some(&rateless)).width == 1260 && c.display_mode(Some(&rateless)).hz == 60); // a rate-less report: the fallback's
    for bad in [
        "",
        "1080",
        "1080x",
        "x2340",
        "0x2340",
        "1080x0",
        "1080x2340@",
        "1080x2340@20",
        "1080x2340@241",
        "1080x2340 ",
        "20000x2340",
        "1080x2340@120x",
        "-1080x2340",
        "123456x10",
    ] {
        assert!(c.set("panel-fallback", bad).is_err(), "{bad:?}");
    }
    assert_eq!(c.panel_fallback, Some(pm(1080, 2340, 60))); // a refused value changes nothing
    let mode1 = [
        "--image",
        &image,
        "--width",
        "1320",
        "--height",
        "2868",
        "--hz",
        "60",
        "--scale",
        "2",
        "--set",
        "scale-fixup=0",
    ];
    let c = run(&mode1).unwrap();
    assert!(c.scale == 2 && c.hz == 60 && !c.scale_fixup);
    assert_eq!(c.display_mode(Some(&pm(1440, 3120, 120))), pm(1320, 2868, 60));
    // the connector of the panel: not the Virtual (writeback, 5120x2560) ones of the S24 Ultra, not DisplayPort
    assert!(
        panel_connector(16)
            && panel_connector(14)
            && !panel_connector(15)
            && !panel_connector(10)
            && !panel_connector(11)
    );
    // the helper's report of the panel, and the mode it gives a guest with nothing configured
    let report = |magic: &[u8; 4], a: u32, b: u32, c: u32| -> Vec<u8> {
        let mut r = magic.to_vec();
        for w in [a, b, c, 0] {
            r.extend_from_slice(&w.to_ne_bytes());
        }
        r
    };
    assert_eq!(panel_report(&report(b"XHDP", 1440, 3120, 120)), Some(pm(1440, 3120, 120)));
    assert!(panel_report(&report(b"XHDP", 1440, 3120, 120)[..19]).is_none());
    assert!(panel_report(&report(b"XHDP", 0, 3120, 120)).is_none());
    assert!(panel_report(&report(b"XHDP", 16385, 3120, 120)).is_none());
    assert!(panel_report(&report(b"XHDQ", 1440, 3120, 120)).is_none());
    // the scale in the image set (the userland hook's file), under the command line's
    let overlay = format!("{image}/overlay/userland");
    fs::create_dir_all(&overlay).unwrap();
    let overlay = format!("{overlay}/xnuhost.conf");
    write(&overlay, "scale=2\nwidth=1440\nheight=3120\nhz=120\n");
    let c = run(&bench_hidl).unwrap();
    assert!(c.scale == 2 && c.width == 1440 && c.height == 3120 && c.hz == 120); // the image's mode, over any panel
    assert_eq!(c.display_mode(Some(&pm(5120, 2560, 60))), pm(1440, 3120, 120));
    let c = run(&mode1).unwrap();
    assert!(c.width == 1320 && c.hz == 60); // the command line's wins
    let mut c = run(&["--image", &image, "--set", "scale=3", "--text-fixup", "file.01,10,aa,bb"]).unwrap();
    assert!(c.scale == 3 && c.text_fixups.len_bytes() == 17);
    // the fixups of QuartzCore's preferred scale: for @3x on the build they were read in, after the configured ones
    scale_fixups(Some("24A446"), 3, &mut c.text_fixups);
    let f = c.text_fixups.entries();
    assert!(c.text_fixups.len_bytes() == 17 + 54 + 70 && f[0] == "file.01,10,aa,bb");
    assert_eq!(f[1], "dyld_shared_cache_arm64e.01,40b17a8,49008052,69008052");
    assert!(f[2].ends_with(",40b17bc,280080520895889a,680080521f2003d5"));
    let bytes = c.text_fixups.to_bytes();
    assert!(bytes.len() == 141 && bytes[140] == 0 && bytes[16] == 0);
    let one = |fixups: &mut TextFixups| {
        *fixups = TextFixups::default();
        fixups.push_within("file.01,10,aa,bb", TEXT_FIXUPS_SIZE);
    };
    one(&mut c.text_fixups);
    scale_fixups(Some("24A446"), 2, &mut c.text_fixups); // @2x is QuartzCore's own answer
    assert_eq!(c.text_fixups.len_bytes(), 17);
    scale_fixups(Some("24A447"), 3, &mut c.text_fixups);
    scale_fixups(None, 3, &mut c.text_fixups);
    assert_eq!(c.text_fixups.len_bytes(), 17);
    let _ = fs::remove_dir_all(&dir);
}

// ---- the host phone (onebridges::hw, host_device_layer; docs/hardware.md "The host device") -------------------------
// Each known phone's defaults under the command line and the image set, an unknown phone, a forced one, detection off;
// and the proof that the S24 Ultra's entry and an unknown phone finish to exactly the config of detection off (today's).

fn host_props(dir: &Path, name: &str, model: &str, soc: &str) -> String {
    let path = dir.join(format!("{name}.props"));
    write(&path, &format!("[ro.product.model]: [{model}]\n[ro.soc.model]: [{soc}]\n[ro.soc.manufacturer]: [QTI]\n"));
    path.to_str().unwrap().to_owned()
}

struct HostRig {
    image: String,
    meminfo: String,
    root: String,
}

impl HostRig {
    /// The command line of the OneOS fork's session plus the host settings, finished.
    fn finish(&self, props: Option<&str>, device: Option<&str>, extra: Option<&str>) -> Result<Config, ConfigError> {
        let ram_meminfo = format!("ram-meminfo={}", self.meminfo);
        let host_root = format!("host-root={}", self.root);
        let mut a: Vec<String> = ["--image", &self.image, "--ram", "4G", "--set", &ram_meminfo, "--set", &host_root]
            .map(String::from)
            .to_vec();
        if let Some(p) = props {
            a.extend(["--set".into(), format!("host-props={p}")]);
        }
        if let Some(d) = device {
            a.extend(["--set".into(), format!("host-device={d}")]);
        }
        if let Some(e) = extra {
            a.extend(["--set".into(), e.to_owned()]);
        }
        run(&a.iter().map(String::as_str).collect::<Vec<_>>())
    }
}

/// The config's host fields cleared (so two configs compare whole).
fn host_clear(mut c: Config) -> Config {
    c.host_device.clear();
    c.host_props.clear();
    c.host_root.clear();
    c.host_line.clear();
    c.host = None;
    c.given.remove("host-device");
    c
}

#[test]
fn host_device() {
    let dir = testdir("host");
    let image = image_set(&dir, "host-image");
    let root = dir.join("host-root");
    fs::create_dir_all(&root).unwrap();
    let meminfo = dir.join("host-meminfo"); // a fixed MemAvailable: two finishes agree
    write(&meminfo, "MemTotal:       11746000 kB\nMemAvailable:    9000000 kB\n");
    let rig = HostRig {
        image: image.clone(),
        meminfo: meminfo.to_str().unwrap().into(),
        root: root.to_str().unwrap().into(),
    };
    let find = |name| bhw::host_find(name).map(Host);

    // Today's defaults: detection off.
    let off = rig.finish(None, Some("none"), None).unwrap();
    assert!(off.host.is_none() && off.host_line == "host: detection off (host-device=none): today's defaults");
    assert!(off.gpu == Gpu::Auto && off.hw == HwSettings::default());
    let off = host_clear(off);
    // The S24 Ultra (the home device) and an unknown phone: byte for byte today's config.
    let props = host_props(&dir, "s24u", "SM-S928B", "SM8650");
    let c = rig.finish(Some(&props), None, None).unwrap();
    assert!(c.host == find("galaxy-s24-ultra"));
    assert!(c.host_line.contains(
        "host: galaxy-s24-ultra (Galaxy S24 Ultra (SM-S928, SM8650)), matched by ro.product.model=SM-S928B; read: "
    ));
    assert!(c.host_line.contains("; defaults: none (today's)"));
    assert_eq!(host_clear(c), off);
    let c = rig.finish(Some(&props), Some("galaxy-s24-ultra"), None).unwrap();
    assert!(c.host_line.contains("host-device=galaxy-s24-ultra (forced)"));
    assert_eq!(host_clear(c), off);
    let props = host_props(&dir, "pixel", "Pixel 11 Pro", "Tensor G5");
    let c = rig.finish(Some(&props), None, None).unwrap();
    assert!(c.host.is_none());
    assert!(c.host_line.contains("host: unknown phone (read: ro.product.model=Pixel 11 Pro ro.product.device=(none) "));
    assert!(c.host_line.contains("ro.soc.model=Tensor G5 ro.soc.manufacturer=QTI): today's defaults (host-device=NAME forces one of galaxy-s24-ultra, "));
    assert_eq!(host_clear(c), off);
    // No property area at all (this PC: no bionic, no file) is an unknown phone too. Not on a bionic build: that reads
    // the property area (qemu-user's has none of a phone's).
    if cfg!(not(target_os = "android")) {
        let c = rig.finish(None, None, None).unwrap();
        assert!(c.host.is_none() && c.host_line.contains("unknown phone (read: no property area)"));
        assert_eq!(host_clear(c), off);
    }
    // The S25 Ultra: the SoC rule on its own trips and on 3 readings (environment defaults, set by the session, not
    // here); no xnuhost setting of its own (docs/s25.md: everything else is [U]).
    let props = host_props(&dir, "s25u", "SM-S938B", "SM8750");
    let c = rig.finish(Some(&props), None, None).unwrap();
    assert!(c.host == find("galaxy-s25-ultra"));
    assert!(c.host_line.contains("; defaults: ONEIOS_THERMAL_SOC_TRIPS=1, ONEIOS_THERMAL_SOC_SAMPLES=3"));
    assert!(std::env::var_os("ONEIOS_THERMAL_SOC_TRIPS").is_none());
    assert_eq!(host_clear(c), off);
    // The S25: its 1080x2340 panel as the fallback mode only (a panel that is read still wins); nothing else changes.
    let props = host_props(&dir, "s25", "SM-S931B", "SM8750");
    let c = rig.finish(Some(&props), None, None).unwrap();
    assert!(c.host == find("galaxy-s25") && c.panel_fallback == Some(PanelMode { width: 1080, height: 2340, hz: 120 }));
    assert!(c.host_line.contains(
        "; defaults: panel-fallback=1080x2340@120, ONEIOS_THERMAL_SOC_TRIPS=1, ONEIOS_THERMAL_SOC_SAMPLES=3"
    ));
    let read_panel = PanelMode { width: 1440, height: 3120, hz: 120 };
    assert!(c.display_mode(Some(&read_panel)).width == 1440 && c.display_mode(None).width == 1080);
    let c2 = rig.finish(Some(&props), None, Some("panel-fallback=1260x2800")).unwrap();
    assert!(
        c2.panel_fallback.unwrap().width == 1260 && c2.host_line.contains("panel-fallback=1080x2340@120 not taken")
    );
    let mut c = host_clear(c);
    c.panel_fallback = None;
    assert_eq!(c, off);
    // The S26 Ultra: its brightness cap is an environment default (the session sets it); the config is today's.
    let props = host_props(&dir, "s26u", "SM-S948B", "SM8850");
    let c = rig.finish(Some(&props), None, None).unwrap();
    assert!(c.host == find("galaxy-s26-ultra") && c.host_line.contains("matched by ro.product.model=SM-S948B"));
    assert!(c.host_line.contains("; defaults: ONEIOS_BRIGHT_MAX=2300"));
    assert_eq!(host_clear(c), off);
    // The A56: gpu=2d, under the command line and the image set.
    let props = host_props(&dir, "a56", "SM-A566E", "s5e8855");
    let c = rig.finish(Some(&props), None, None).unwrap();
    assert!(c.host == find("galaxy-a56") && c.gpu == Gpu::TwoD);
    assert!(c.host_line.contains("; defaults: gpu=2d") && !c.host_line.contains("not taken"));
    let c = rig.finish(Some(&props), None, Some("gpu=auto")).unwrap();
    assert!(
        c.gpu == Gpu::Auto && c.host_line.contains("gpu=2d not taken (the command line or the image set gives it)")
    );
    assert_eq!(rig.finish(Some(&props), Some("none"), None).unwrap().gpu, Gpu::Auto);
    let c = rig.finish(Some(&props), Some("NONE"), None).unwrap();
    assert!(c.host.is_none() && c.gpu == Gpu::Auto);
    let conf = format!("{image}/xnuhost.conf");
    write(&conf, "gpu=3d\n");
    let c = rig.finish(Some(&props), None, None).unwrap();
    assert!(c.gpu == Gpu::ThreeD && c.host_line.contains("gpu=2d not taken"));
    assert_eq!(rig.finish(Some(&props), None, Some("gpu=2d")).unwrap().gpu, Gpu::TwoD); // the command line over the image set
    write(&conf, "host-device=none\n"); // the image set may turn it off
    let c = rig.finish(Some(&props), None, None).unwrap();
    assert!(c.host.is_none() && c.gpu == Gpu::Auto);
    assert!(rig.finish(Some(&props), Some("auto"), None).unwrap().host == find("galaxy-a56")); // and the command line on
    fs::remove_file(&conf).unwrap();
    // Forced: a name wins over the phone's properties; a name that is not in the table is refused, and so is a
    // property file that cannot be read.
    let props = host_props(&dir, "s24u", "SM-S928B", "SM8650");
    let c = rig.finish(Some(&props), Some("galaxy-a56"), None).unwrap();
    assert!(c.host == find("galaxy-a56") && c.gpu == Gpu::TwoD);
    assert!(c.host_line.contains("host-device=galaxy-a56 (forced); read: ro.product.model=SM-S928B"));
    assert!(rig.finish(Some(&props), Some("pixel-11"), None).is_err());
    let mut c = Config::new();
    assert!(c.set("host-device", "pixel-11").is_err() && c.set("host-device", "galaxy-s26-ultra").is_ok());
    assert!(c.set("host-device", "auto").is_ok() && c.set("host-device", "none").is_ok());
    let missing = dir.join("no-such.props");
    assert!(rig.finish(Some(missing.to_str().unwrap()), None, None).is_err());
    // Every entry of the table: forced, it finishes, and each of its xnuhost defaults is what the finished config holds.
    for entry in bhw::host_table() {
        let c = rig.finish(None, Some(entry.name), None).unwrap();
        assert!(c.host == Some(Host(entry)));
        for pair in entry.config {
            let (key, value) = pair.split_once('=').expect("KEY=VALUE");
            let mut want = Config::new();
            assert!(want.set(key, value).is_ok(), "{pair}");
            match key {
                "gpu" => assert_eq!(c.gpu, want.gpu),
                "panel-fallback" => assert_eq!(c.panel_fallback, want.panel_fallback),
                _ => {
                    want.hw.merge(&off.hw);
                    assert_eq!(c.hw, want.hw);
                }
            }
        }
    }
    // The selftest leaves no gpu unset.
    let c = run(&["--selftest"]).unwrap();
    assert!(c.gpu == Gpu::Auto && c.host_line.is_empty());
    let _ = fs::remove_dir_all(&dir);
}

// ---- session-mode= and --host-info (docs/booter.md 2.9d) ---------------------------------------------------------------
// hosted is the default and changes nothing; takeover only in the OneOS session, only on a phone whose host-table row
// allows it (the S24 Ultra, the S26 Ultra), and it turns the native touchscreen's grab on unless touch-exclusive= was
// given; the image set's session-mode= under the command line's. And --host-info, the one line the OneOS switch reads
// before it stops anything.

/// The fork's command line on the phone whose properties are `props`, with `extra` after it, finished.
fn mode_finish(rig: &HostRig, props: &str, extra: &[&str]) -> Result<Config, ConfigError> {
    let ram_meminfo = format!("ram-meminfo={}", rig.meminfo);
    let host_props = format!("host-props={props}");
    let host_root = format!("host-root={}", rig.root);
    let mut a = vec!["--image", &rig.image, "--ram", "4G", "--set", &ram_meminfo, "--set", &host_props];
    a.extend(["--set", &host_root]);
    a.extend_from_slice(extra);
    run(&a)
}

/// --host-info's line for a phone (and a host-device=), or "refused".
fn host_info_line(rig: &HostRig, props: &str, device: Option<&str>) -> String {
    let mut a = vec!["--host-info", "--host-props", props, "--host-root", &rig.root];
    if let Some(d) = device {
        a.extend(["--host-device", d]);
    }
    let mut c = Config::new();
    match args(&mut c, &a).map(|()| c.host_info).map(|on| on.then(|| c.host_info_line())) {
        Ok(Some(Ok(line))) => line,
        _ => "refused".to_owned(),
    }
}

#[test]
fn session_mode() {
    let dir = testdir("mode");
    let image = image_set(&dir, "mode-image");
    let root = dir.join("mode-root");
    fs::create_dir_all(&root).unwrap();
    let meminfo = dir.join("mode-meminfo");
    write(&meminfo, "MemTotal:       11746000 kB\nMemAvailable:    9000000 kB\n");
    let rig = HostRig {
        image: image.clone(),
        meminfo: meminfo.to_str().unwrap().into(),
        root: root.to_str().unwrap().into(),
    };
    let s24 = host_props(&dir, "mode-s24u", "SM-S928B", "SM8650");
    let s26 = host_props(&dir, "mode-s26u", "SM-S948B", "SM8850");
    let s25u = host_props(&dir, "mode-s25u", "SM-S938B", "SM8750");
    let pixel = host_props(&dir, "mode-pixel", "Pixel 11 Pro", "Tensor G5");
    let find = |name| bhw::host_find(name).map(Host);

    // The table: the S24 Ultra and the S26 Ultra allow it, no other row.
    for e in bhw::host_table() {
        assert_eq!(e.takeover, e.name == "galaxy-s24-ultra" || e.name == "galaxy-s26-ultra", "{}", e.name);
    }

    // hosted: the default, and the same config as no setting at all, on the S24 Ultra too.
    let plain = mode_finish(&rig, &s24, &[]).unwrap();
    assert!(plain.session_mode == SessionMode::Hosted && !plain.touch_exclusive);
    assert!(plain.takeover_line().is_none() && plain.reads_side_keys() && plain.exclusive_touch_text() == "off");
    let mut c = mode_finish(&rig, &s24, &["--set", "session-mode=hosted"]).unwrap();
    assert!(c.session_mode == SessionMode::Hosted && !c.touch_exclusive);
    c.given.remove("session-mode");
    assert_eq!(c, plain);
    let mut c = Config::new();
    assert!(c.set("session-mode", "android").is_err() && c.set("session-mode", "").is_err());
    assert!(c.session_mode == SessionMode::Hosted && !c.given("session-mode"));

    // takeover on the two phones that allow it: the touchscreen grab on, the session's mode unchanged.
    let takeover = ["--set", "session-mode=takeover"];
    let c = mode_finish(&rig, &s24, &takeover).unwrap();
    assert!(c.session_mode == SessionMode::Takeover && c.touch_exclusive && c.input && c.mode == Mode::Takeover);
    assert!(c.host == find("galaxy-s24-ultra"));
    // the side keys read, the lines of the mode
    assert!(c.reads_side_keys());
    assert_eq!(
        c.exclusive_touch_text(),
        "requested (session-mode=takeover: sec_touchscreen grab, the side keys read and never grabbed)"
    );
    assert_eq!(
        c.takeover_line().unwrap(),
        "session: Android takeover (session-mode=takeover; galaxy-s24-ultra allows it): the native touchscreen is grabbed for iOS; the side keys are read, never grabbed; Android's services stopped by the switch: takeover-stops=default; the switch's CPU cap, its stops and the way back are the OneOS fork's"
    );
    let c = mode_finish(&rig, &s26, &takeover).unwrap();
    assert!(c.touch_exclusive && c.host == find("galaxy-s26-ultra"));
    // touch-exclusive=0 given: kept off, before or after session-mode=.
    let c = mode_finish(&rig, &s24, &["--set", "touch-exclusive=0", "--set", "session-mode=takeover"]).unwrap();
    assert!(c.session_mode == SessionMode::Takeover && !c.touch_exclusive && c.exclusive_touch_text() == "off");
    assert!(c.takeover_line().unwrap().contains("grabbed for iOS (not: touch-exclusive=0 was given); the side keys"));
    let c = mode_finish(&rig, &s24, &["--set", "session-mode=takeover", "--set", "touch-exclusive=0"]).unwrap();
    assert!(c.session_mode == SessionMode::Takeover && !c.touch_exclusive);
    // Refused: a phone whose row does not allow it, an unknown phone, detection off, the bench, no input.
    let e = mode_finish(&rig, &s25u, &takeover).unwrap_err();
    assert_eq!(
        e.to_string(),
        "session-mode=takeover is not supported on this phone (galaxy-s25-ultra): the host table (runtime/bridges/hw/host_device.rs) does not allow it; session-mode=hosted is the session as before"
    );
    let e = mode_finish(&rig, &pixel, &takeover).unwrap_err();
    assert!(e.to_string().contains("not supported on this phone (no row of the host table)"));
    assert!(mode_finish(&rig, &s24, &["--set", "session-mode=takeover", "--set", "host-device=none"]).is_err());
    let forced = ["--set", "session-mode=takeover", "--set", "host-device=galaxy-s24-ultra"];
    assert!(mode_finish(&rig, &pixel, &forced).unwrap().touch_exclusive); // a forced row is that row
    let e = mode_finish(&rig, &s24, &["--set", "session-mode=takeover", "--mode", "bench"]).unwrap_err();
    assert_eq!(e.to_string(), "session-mode=takeover needs mode=takeover (the OneOS session)");
    let e = mode_finish(&rig, &s24, &["--set", "session-mode=takeover", "--no-input"]).unwrap_err();
    assert_eq!(e.to_string(), "session-mode=takeover needs input=1 (its touchscreen grab)");
    // hosted still refuses the grab in a session, as before session-mode existed.
    let e = mode_finish(&rig, &s24, &["--set", "touch-exclusive=1"]).unwrap_err();
    assert_eq!(
        e.to_string(),
        "touch-exclusive=1 requires session-mode=takeover, or mode=bench, display=shm, input=1, bridges=0"
    );
    // The bench preview: the grab, and the side keys left to Android.
    let bench = ["--mode", "bench", "--display", "shm", "--set", "bridges=0", "--set", "touch-exclusive=1"];
    let c = mode_finish(&rig, &s24, &bench).unwrap();
    assert!(c.touch_exclusive && !c.reads_side_keys() && c.takeover_line().is_none());
    assert_eq!(c.exclusive_touch_text(), "requested (bench+SHM sec_touchscreen grab)");

    // The image set's overlay/userland/xnuhost.conf may say it; the command line's session-mode= wins.
    let conf = Path::new(&image).join("overlay/userland");
    fs::create_dir_all(&conf).unwrap();
    let conf = conf.join("xnuhost.conf");
    write(&conf, "session-mode=takeover\n");
    let c = mode_finish(&rig, &s24, &[]).unwrap();
    assert!(c.session_mode == SessionMode::Takeover && c.touch_exclusive);
    let c = mode_finish(&rig, &s24, &["--set", "session-mode=hosted"]).unwrap();
    assert!(c.session_mode == SessionMode::Hosted && !c.touch_exclusive);
    assert!(mode_finish(&rig, &s25u, &[]).is_err());
    fs::remove_file(&conf).unwrap();

    // takeover-stops=: the switch's stop list, checked here (default, none, or names of TAKEOVER_STOPPABLE, each once),
    // named in the takeover line, from the image set or the command line; hosted accepts it and does nothing with it.
    let mut c = Config::new();
    for ok in
        ["default", "none", "netd", "cameraserver,mediaextractor,mediametrics,media.tuner,perfmond,wificond,netd,media"]
    {
        assert!(c.set("takeover-stops", ok).is_ok() && c.takeover_stops == ok, "{ok}");
    }
    for bad in ["", "zygote", "netd,netd", "netd,", "vold", "surfaceflinger", "media,keystore2", "NETD"] {
        assert!(c.set("takeover-stops", bad).is_err(), "{bad}");
    }
    assert_eq!(c.takeover_stops, TAKEOVER_STOPPABLE.join(","));
    write(&conf, "session-mode=takeover\ntakeover-stops=cameraserver,netd\n");
    let c = mode_finish(&rig, &s24, &[]).unwrap();
    assert!(c
        .takeover_line()
        .unwrap()
        .contains("; Android's services stopped by the switch: takeover-stops=cameraserver,netd; "));
    let c = mode_finish(&rig, &s24, &["--set", "takeover-stops=none"]).unwrap();
    assert!(c.takeover_line().unwrap().contains("takeover-stops=none; "));
    write(&conf, "takeover-stops=netd\n");
    let c = mode_finish(&rig, &s24, &[]).unwrap();
    assert!(c.session_mode == SessionMode::Hosted && c.takeover_line().is_none());
    write(&conf, "session-mode=takeover\ntakeover-stops=vold\n");
    assert!(mode_finish(&rig, &s24, &[]).is_err());
    fs::remove_file(&conf).unwrap();

    // --host-info: one line, no image needed.
    assert_eq!(
        host_info_line(&rig, &s24, None),
        "host-info: device=galaxy-s24-ultra takeover=yes (matched by ro.product.model=SM-S928B)"
    );
    assert_eq!(
        host_info_line(&rig, &s26, None),
        "host-info: device=galaxy-s26-ultra takeover=yes (matched by ro.product.model=SM-S948B)"
    );
    assert_eq!(
        host_info_line(&rig, &s25u, None),
        "host-info: device=galaxy-s25-ultra takeover=no (matched by ro.product.model=SM-S938B)"
    );
    assert!(host_info_line(&rig, &pixel, None)
        .starts_with("host-info: device=none takeover=no (unknown phone; read: ro.product.model=Pixel 11 Pro "));
    assert!(host_info_line(&rig, &s24, Some("none"))
        .starts_with("host-info: device=none takeover=no (host-device=none: detection off; read: "));
    assert_eq!(
        host_info_line(&rig, &pixel, Some("galaxy-s26-ultra")),
        "host-info: device=galaxy-s26-ultra takeover=yes (host-device=galaxy-s26-ultra (forced))"
    );
    assert_eq!(host_info_line(&rig, &s24, Some("pixel-11")), "refused");
    let missing = dir.join("no-such.props");
    assert_eq!(host_info_line(&rig, missing.to_str().unwrap(), None), "refused");
    let _ = fs::remove_dir_all(&dir);
}

/// The switches of docs/switches.md: each key's values, the image's xnuhost.conf (the command line wins), the line.
#[test]
fn the_switches() {
    let mut c = Config::default();
    assert!(c.switches_line().ends_with("all at their defaults"), "{}", c.switches_line());
    assert!(c.wifi_ring_wait && c.wifi_send_wait && c.wifi_buffers && c.wifi_joined_scan == WifiJoinedScan::Cache);
    assert!(c.encoder_10bit == Encoder10::Main10 && c.encoder_warmup && c.gpu_down == GpuDown::Governor);
    assert!(c.gpu_blob_entries == 65532 && c.stop_loop == StopLoop::Wait);
    for (k, v) in [
        ("wifi-ring-wait", "0"),
        ("wifi-send-wait", "0"),
        ("wifi-buffers", "0"),
        ("wifi-joined-scan", "plain"),
        ("encoder-10bit", "nv12"),
        ("encoder-warmup", "0"),
        ("gpu-blob-entries", "16384"),
        ("gpu-down", "bottom"),
        ("stop-loop", "poll"),
        ("uclamp-rule", "all"),
    ] {
        assert!(c.set(k, v).is_ok(), "{k}={v}");
    }
    for (k, v) in [
        ("gpu-blob-entries", "65533"),
        ("gpu-blob-entries", "1024"),
        ("wifi-joined-scan", "on"),
        ("encoder-10bit", "10"),
        ("gpu-down", "top"),
        ("stop-loop", "sleep"),
        ("uclamp-rule", "some"),
        ("wifi-buffers", "off"),
    ] {
        assert!(c.set(k, v).is_err(), "{k}={v} refused");
    }
    c.boot_args = "-v oneios-wifi-rssi=0 oneios-telemetry-jobs=1 oneios-pool-cut=0 oneios-pressure-big=0 oneios-idle-spares=0 oneios-bo-cache-idle=0".into();
    let line = c.switches_line();
    assert!(line.contains(" wifi-joined-scan=plain ") && line.contains(" gpu-blob-entries=16384 "), "{line}");
    assert!(
        line.contains(
            " oneios-wifi-rssi=0 oneios-telemetry-jobs=1 oneios-pool-cut=0 oneios-pressure-big=0 oneios-idle-spares=0 oneios-bo-cache-idle=0 (docs/switches.md)"
        ),
        "{line}"
    );
    assert!(
        line.ends_with(
            "not at the default: stop-loop uclamp-rule gpu-down gpu-blob-entries wifi-ring-wait wifi-send-wait wifi-buffers \
             wifi-joined-scan encoder-10bit encoder-warmup oneios-wifi-rssi oneios-telemetry-jobs oneios-pool-cut \
             oneios-pressure-big oneios-idle-spares oneios-bo-cache-idle"
        ),
        "{line}"
    );
    // From the image's xnuhost.conf, and the command line over it.
    let dir = testdir("switches");
    let image = image_set(&dir, "image");
    let meminfo = dir.join("meminfo");
    write(&meminfo, "MemTotal:       11746000 kB\nMemAvailable:    7340032 kB\n");
    write(
        format!("{image}/xnuhost.conf"),
        &format!(
            "boot-args=-v\nwifi-joined-scan=low\ngpu-down=bottom\nencoder-10bit=8bit\nwifi-send-wait=0\nram-meminfo={}\n",
            meminfo.display()
        ),
    );
    let c = run(&["--image", &image, "--mode", "takeover", "--set", "wifi-joined-scan=cache"]).unwrap();
    assert!(c.wifi_joined_scan == WifiJoinedScan::Cache && c.gpu_down == GpuDown::Bottom);
    assert!(c.encoder_10bit == Encoder10::EightBit && !c.wifi_send_wait && c.wifi_ring_wait);
    let _ = fs::remove_dir_all(&dir);
}
