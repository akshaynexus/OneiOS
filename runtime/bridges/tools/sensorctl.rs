// SPDX-License-Identifier: MIT
//! sensorctl: the sensor hub (SSC) on the phone without xnuhost or a guest (runtime/bridges/sensors/README.md). Run as
//! root (su) beside One UI:
//!   sensorctl qrtr [--sensors]       read only: whether this process can reach Linux's QRTR (socket AF_QIPCRTR), the
//!                                    local node, every service the name server lists (service, version, instance, node,
//!                                    port), and whether the sensor hub's SNS client service (400) is among them and
//!                                    answers a lookup. With --sensors also each data type's SUIDs and attributes (name,
//!                                    vendor, stream type, rates, range): lookups only, no stream is started. Nothing on
//!                                    the phone changes; the sockets close at the exit.
//!   sensorctl stream [--seconds N] [--hz N]
//!                                    the bridge itself for N s (default 10, at most 600): the accelerometer, gyroscope,
//!                                    magnetometer and front light streamed from the hub as a session does, one line a
//!                                    second (samples, the last values in the guest's units, lux), then every stream
//!                                    stopped. Our requests end with our sockets: One UI's own sensor clients are not
//!                                    touched.
//! Each ends with "ok" or "failed".

use onebridges::log::{set_sink, Level, Sink};
use onebridges::sensors::{self, qrtr, sns, Hub, Kind, Motion, Options};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

fn qrtr_cmd(args: &[String]) -> i32 {
    let with_sensors = args.iter().any(|a| a == "--sensors");
    let timeout = Duration::from_millis(1500);
    let socket = match qrtr::Socket::open() {
        Ok(s) => s,
        Err(e) => {
            println!(
                "qrtr: socket(AF_QIPCRTR): {} ({}): this process cannot reach QRTR{}",
                xh_core::sys::error_text(&e),
                e.raw_os_error().unwrap_or(0),
                match e.raw_os_error() {
                    Some(libc::EAFNOSUPPORT) => " (the kernel has no QRTR)",
                    Some(libc::EACCES) | Some(libc::EPERM) => " (refused: SELinux or a capability)",
                    _ => "",
                }
            );
            println!(
                "qrtr: SSC: unreachable: sensors=auto takes the old path (no motion device, the rear light sensor)"
            );
            return 1;
        }
    };
    println!("qrtr: socket(AF_QIPCRTR) open: local node {}, our port {:#x}", socket.node, socket.port);
    let list = match socket.lookup(0, Duration::from_secs(3)) {
        Ok(l) => l,
        Err(e) => {
            println!("qrtr: the name server did not answer a lookup: {}", xh_core::sys::error_text(&e));
            return 1;
        }
    };
    drop(socket);
    let mut nodes: Vec<u32> = list.iter().map(|s| s.node).collect();
    nodes.sort_unstable();
    nodes.dedup();
    println!("qrtr: {} services on {} nodes ({:?})", list.len(), nodes.len(), nodes);
    for s in &list {
        let name = qrtr::service_name(s.service);
        println!(
            "qrtr: service {} version {} instance {} node {} port {:#x}{}{}",
            s.service,
            s.version(),
            s.instance_id(),
            s.node,
            s.port,
            if name.is_empty() { "" } else { ": " },
            name
        );
    }
    let Some(ssc) = list.iter().find(|s| s.service == sns::SERVICE) else {
        println!(
            "qrtr: SSC: no service 400 (the sensor hub's SNS client service is not offered to this process): \
                  sensors=auto takes the old path"
        );
        return 1;
    };
    // one lookup of the accelerometer: does the hub answer a client of ours
    match sensors::list(&["accel"], timeout) {
        Ok((_, l)) => match &l[0].1 {
            Ok(suids) => println!(
                "qrtr: SSC: service 400 version {} at node {} port {:#x} answers: accel lookup gave {} SUID(s){}",
                ssc.version(),
                ssc.node,
                ssc.port,
                suids.len(),
                if suids.is_empty() { "" } else { ": sensors=auto takes the hub" }
            ),
            Err(why) => {
                println!("qrtr: SSC: service 400 at node {} port {:#x} is listed but {why}", ssc.node, ssc.port);
                return 1;
            }
        },
        Err(why) => {
            println!("qrtr: SSC: {why}");
            return 1;
        }
    }
    if with_sensors {
        match sensors::list(&sensors::LIST_TYPES, timeout) {
            Ok((_, l)) => {
                for (dt, r) in l {
                    match r {
                        Ok(v) if v.is_empty() => println!("sensors: {dt}: none"),
                        Ok(v) => {
                            for (i, (suid, a)) in v.iter().enumerate() {
                                let more = a.as_ref().map_or(String::new(), |a| {
                                    format!(
                                        "{}{}{}",
                                        a.range.map_or(String::new(), |(lo, hi)| format!(", range {lo}..{hi}")),
                                        a.hw_id.map_or(String::new(), |h| format!(", hw id {h}")),
                                        if a.placement.is_empty() {
                                            String::new()
                                        } else {
                                            format!(", placement {:?}", a.placement)
                                        }
                                    )
                                });
                                println!(
                                    "sensors: {dt} {i}: {suid} {}{more}",
                                    a.as_ref().map_or_else(|| "(no attributes)".to_string(), sensors::describe)
                                );
                            }
                        }
                        Err(why) => println!("sensors: {dt}: {why}"),
                    }
                }
            }
            Err(why) => {
                println!("sensors: {why}");
                return 1;
            }
        }
    }
    0
}

fn value(args: &[String], name: &str, default: u32, max: u32) -> Option<u32> {
    match args.iter().position(|a| a == name) {
        None => Some(default),
        Some(i) => args.get(i + 1).and_then(|v| v.parse::<u32>().ok()).filter(|v| (1..=max).contains(v)),
    }
}

fn stream(args: &[String]) -> i32 {
    let (Some(seconds), Some(hz)) = (value(args, "--seconds", 10, 600), value(args, "--hz", 50, 200)) else {
        println!("stream: --seconds 1..600, --hz 1..200");
        return 2;
    };
    let hub = match Hub::open(&Options { hz, ..Options::default() }) {
        Ok(h) => Arc::new(h),
        Err(why) => {
            println!("stream: {why}");
            return 1;
        }
    };
    for (kind, suid, what) in &hub.found().sensors {
        println!("stream: {}: {suid} {what}", kind.name());
    }
    let last: Arc<Mutex<(Motion, u64, u32, u64)>> = Arc::new(Mutex::new((Motion::default(), 0, 0, 0)));
    let (l1, l2) = (Arc::clone(&last), Arc::clone(&last));
    let motion: sensors::MotionFn = Arc::new(move |m: &Motion| {
        let mut g = l1.lock().unwrap();
        g.0 = *m;
        g.1 += 1;
        true
    });
    let light: sensors::LightFn = Arc::new(move |mlx| {
        let mut g = l2.lock().unwrap();
        g.2 = mlx;
        g.3 += 1;
    });
    if !hub.start(Some(motion), hub.has(Kind::Light).then_some(light)) {
        return 1;
    }
    let (gate, cv) = (Mutex::new(()), Condvar::new());
    let end = Instant::now() + Duration::from_secs(u64::from(seconds));
    let mut seen = (0u64, 0u64);
    while Instant::now() < end {
        // one line a second: a timed wait on a condition nobody signals (no busy loop)
        let g = gate.lock().unwrap();
        let _ = cv.wait_timeout(g, Duration::from_secs(1).min(end.saturating_duration_since(Instant::now())));
        let (m, frames, mlx, lights) = *last.lock().unwrap();
        let a = sensors::axes(&m);
        println!(
            "stream: {} frames/s, accel {:?} (256/g), gyro {:?} (16 per deg/s), mag {:?} (16/uT); light {} readings, {}.{:03} lux",
            frames - seen.0,
            &a[..3],
            &a[3..6],
            &a[6..],
            lights - seen.1,
            mlx / 1000,
            mlx % 1000
        );
        seen = (frames, lights);
    }
    hub.close();
    println!("stream: every stream stopped on the hub");
    0
}

fn main() {
    set_sink(Sink::Custom(Arc::new(|level: Level, line: &str| {
        println!("  {}{line}", if level == Level::Error { "error: " } else { "" })
    })));
    let args: Vec<String> = std::env::args().collect();
    let rc = match args.get(1).map(String::as_str) {
        Some("qrtr") => qrtr_cmd(&args[2..]),
        Some("stream") => stream(&args[2..]),
        _ => {
            eprintln!("usage: sensorctl qrtr [--sensors] | stream [--seconds N] [--hz N]");
            std::process::exit(2);
        }
    };
    println!("{}", if rc == 0 { "ok" } else { "failed" });
    std::process::exit(rc);
}
